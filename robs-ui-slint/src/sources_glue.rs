//! Batch A glue: everything behind the `SourcesApi` global — scene
//! add/remove/select extras, the add-source pickers (display / window /
//! webcam), per-source actions, text overlays, and the source-properties
//! dialog. Ports `robs-ui/src/app/panels/scenes.rs` + `sources.rs` onto the
//! Phase 1 tick loop.
//!
//! Device lists: monitors and cameras are enumerated once at startup (camera
//! enumeration spawns an FFmpeg probe per call); the window list re-enumerates
//! every [`WINDOW_REFRESH`] (a cheap Win32 walk) so the picker stays current.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use robs_controller::devices::{get_monitors, get_video_devices};
use robs_controller::state::{EventLogKind, MonitorInfo};
use robs_controller::RobsController;
use robs_core::types::SourceId;
use robs_core::{CaptureSource, Crop, ObjectId, Position, Scale, SceneItemId, TextOverlay};
use robs_sources::native_capture::{get_open_windows, WebcamCapture, WindowInfo};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{MainWindow, OverlayView, SourceRowView, SourcesApi};

/// Window-picker re-enumeration cadence.
const WINDOW_REFRESH: Duration = Duration::from_secs(5);

/// Picker list caps (window titles truncate like the old egui menu).
const WINDOW_MAX: usize = 50;
const WINDOW_TITLE_CHARS: usize = 40;
const OVERLAY_TEXT_CHARS: usize = 28;

/// Glue-side state for the sources panels: cached device lists, mirrors of
/// the last pushed models, and the item loaded into the properties dialog.
pub struct SourcesUi {
    monitors: Vec<MonitorInfo>,
    windows: Vec<WindowInfo>,
    cameras: Vec<String>,
    editing_id: Option<SceneItemId>,
    rows_mirror: Vec<(SceneItemId, String, bool)>,
    overlays_mirror: Vec<(ObjectId, String, bool)>,
    monitors_mirror: Vec<String>,
    cameras_mirror: Vec<String>,
    windows_mirror: Vec<String>,
    last_window_refresh: Instant,
}

impl SourcesUi {
    pub fn new() -> Self {
        Self {
            monitors: get_monitors(),
            windows: get_open_windows(),
            cameras: get_video_devices(),
            editing_id: None,
            rows_mirror: Vec::new(),
            overlays_mirror: Vec::new(),
            monitors_mirror: Vec::new(),
            cameras_mirror: Vec::new(),
            windows_mirror: Vec::new(),
            last_window_refresh: Instant::now(),
        }
    }

    fn monitor_label(monitor: &MonitorInfo) -> String {
        if monitor.is_primary {
            format!(
                "{} ({}x{} - PRIMARY)",
                monitor.name, monitor.width, monitor.height
            )
        } else {
            format!(
                "{} ({}x{} @ {},{})",
                monitor.name, monitor.width, monitor.height, monitor.position_x, monitor.position_y
            )
        }
    }
}

fn item_id(id: i32) -> SceneItemId {
    SceneItemId(ObjectId(id.max(0) as u64))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

fn parse_f32_or(s: SharedString, fallback: f32) -> f32 {
    s.trim().parse().unwrap_or(fallback)
}

fn parse_u32_or(s: SharedString, fallback: u32) -> u32 {
    s.trim().parse().unwrap_or(fallback)
}

/// Wire every `SourcesApi` callback. Device pickers' static models are
/// installed here; the (refreshable) window list and the scene-derived models
/// are maintained by [`push`].
pub fn install(
    component: &slint::Weak<MainWindow>,
    controller: &Rc<RefCell<RobsController>>,
    state: &Rc<RefCell<SourcesUi>>,
) {
    // ---- Static picker models (monitors + cameras enumerated once) ----
    if let Some(component) = component.upgrade() {
        let api = component.global::<SourcesApi>();
        let monitors: Vec<SharedString> = state
            .borrow()
            .monitors
            .iter()
            .map(|m| SourcesUi::monitor_label(m).into())
            .collect();
        api.set_monitor_names(ModelRc::new(VecModel::from(monitors)));
        let cameras: Vec<SharedString> = state
            .borrow()
            .cameras
            .iter()
            .map(|c| c.as_str().into())
            .collect();
        api.set_camera_names(ModelRc::new(VecModel::from(cameras)));
    }

    // Owned handle for the rest of the install; handlers that need
    // `SourcesApi` re-acquire it from this weak handle.
    let Some(root) = component.upgrade() else {
        return;
    };
    let api = root.global::<SourcesApi>();

    // ---- Scenes ----
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            let ctl = Rc::clone(&controller);
            api.on_add_scene(move || {
                let mut c = ctl.borrow_mut();
                let name = format!("Scene {}", c.scenes.count() + 1);
                c.scenes.create_scene(name.clone());
                c.scenes.set_current_scene(&name);
            });
            let ctl = Rc::clone(&controller);
            api.on_remove_scene(move || {
                let mut c = ctl.borrow_mut();
                if c.scenes.count() > 1 {
                    if let Some(name) = c.scenes.current_scene_name().map(str::to_string) {
                        c.scenes.remove(&name);
                        let mut names: Vec<String> =
                            c.scenes.list().iter().map(|s| s.to_string()).collect();
                        names.sort();
                        if let Some(first) = names.first() {
                            c.scenes.set_current_scene(first);
                        }
                    }
                }
            });
        }
    }

    // ---- Add sources ----
    {
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_add_display_source(move |index: i32| {
                let monitor = state
                    .borrow()
                    .monitors
                    .get(index.max(0) as usize)
                    .cloned();
                let Some(monitor) = monitor else { return };
                let mut c = controller.borrow_mut();
                if let Some(scene) = c.scenes.current_scene_mut() {
                    let label = if monitor.is_primary {
                        "Primary".to_string()
                    } else {
                        monitor.name.clone()
                    };
                    let capture = CaptureSource::Display {
                        x: monitor.position_x,
                        y: monitor.position_y,
                        width: monitor.width,
                        height: monitor.height,
                        label,
                    };
                    let name = capture.display_name();
                    let item_id = scene.add_source(SourceId(ObjectId::new()), name);
                    if let Some(item) = scene.item_mut(item_id) {
                        item.set_capture(Some(capture));
                    }
                }
            });
        }
    }
    {
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_add_window_source(move |index: i32| {
                let window = state
                    .borrow()
                    .windows
                    .get(index.max(0) as usize)
                    .cloned();
                let Some(window) = window else { return };
                let mut c = controller.borrow_mut();
                if let Some(scene) = c.scenes.current_scene_mut() {
                    let capture = CaptureSource::Window {
                        title: window.title.clone(),
                    };
                    let name = capture.display_name();
                    let item_id = scene.add_source(SourceId(ObjectId::new()), name);
                    if let Some(item) = scene.item_mut(item_id) {
                        item.set_capture(Some(capture));
                    }
                    c.window_hwnds.insert(item_id, window.hwnd);
                }
            });
        }
    }
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_add_webcam_source(move |device: slint::SharedString| {
                let cam = device.to_string();
                let mut c = controller.borrow_mut();
                if let Some(scene) = c.scenes.current_scene_mut() {
                    let capture = CaptureSource::Webcam {
                        device: cam.clone(),
                        width: 1280,
                        height: 720,
                    };
                    let name = capture.display_name();
                    let item_id = scene.add_source(SourceId(ObjectId::new()), name);
                    if let Some(item) = scene.item_mut(item_id) {
                        item.set_capture(Some(capture));
                    }
                    c.log_event(format!("Video source added: {cam}"), EventLogKind::Info);
                    match WebcamCapture::new(&cam, 1280, 720, 30.0) {
                        Some(wc) => {
                            c.webcam_captures.insert(item_id, wc);
                        }
                        None => {
                            eprintln!("[Webcam] Failed to start capture for: {cam}");
                        }
                    }
                }
            });
        }
    }

    // ---- Per-source actions ----
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_move_source_up(move |id: i32| {
                let mut c = controller.borrow_mut();
                if let Some(scene) = c.scenes.current_scene_mut() {
                    scene.move_item_up(item_id(id));
                }
            });
        }
    }
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_move_source_down(move |id: i32| {
                let mut c = controller.borrow_mut();
                if let Some(scene) = c.scenes.current_scene_mut() {
                    scene.move_item_down(item_id(id));
                }
            });
        }
    }
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_remove_source(move |id: i32| {
                let id = item_id(id);
                let mut c = controller.borrow_mut();
                if let Some(scene) = c.scenes.current_scene_mut() {
                    scene.remove_item(id);
                }
                // Tear down capture-side state for the item: dropping the
                // WebcamCapture stops its FFmpeg child and joins the reader
                // thread (the old egui panel leaked both).
                c.window_hwnds.remove(&id);
                c.webcam_captures.remove(&id);
            });
        }
    }
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_set_source_visible(move |id: i32, visible: bool| {
                let mut c = controller.borrow_mut();
                if let Some(scene) = c.scenes.current_scene_mut() {
                    scene.set_item_visible(item_id(id), visible);
                }
            });
        }
    }

    // ---- Properties dialog ----
    // These callbacks read/write `SourcesApi` properties, so they re-acquire
    // the global from the window inside the handler (a `'static` requirement —
    // the `api` reference itself can't be captured).
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        api.on_source_properties(move |id: i32| {
            let id = item_id(id);
            let Some(root) = weak.upgrade() else { return };
            let api = root.global::<SourcesApi>();
            let c = controller.borrow_mut();
            let props = c
                .scenes
                .current_scene()
                .and_then(|scene| scene.item(id))
                .map(|item| {
                    (
                        item.name().to_string(),
                        item.position(),
                        item.scale(),
                        item.rotation(),
                        item.crop(),
                    )
                });
            if let Some((name, pos, scale, rotation, crop)) = props {
                api.set_prop_name(name.into());
                api.set_prop_pos_x(pos.x.to_string().into());
                api.set_prop_pos_y(pos.y.to_string().into());
                api.set_prop_scale_x(scale.x.to_string().into());
                api.set_prop_scale_y(scale.y.to_string().into());
                api.set_prop_rotation(rotation.to_string().into());
                api.set_prop_crop_left(crop.left.to_string().into());
                api.set_prop_crop_top(crop.top.to_string().into());
                api.set_prop_crop_right(crop.right.to_string().into());
                api.set_prop_crop_bottom(crop.bottom.to_string().into());
                api.set_properties_open(true);
                state.borrow_mut().editing_id = Some(id);
            }
        });
    }
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        let cancel_state = Rc::clone(&state);
        api.on_apply_properties(move || {
            let Some(root) = weak.upgrade() else { return };
            let api = root.global::<SourcesApi>();
            let id = state.borrow().editing_id;
            let Some(id) = id else { return };
            let pos_x = parse_f32_or(api.get_prop_pos_x(), f32::NAN);
            let pos_y = parse_f32_or(api.get_prop_pos_y(), f32::NAN);
            let scale_x = parse_f32_or(api.get_prop_scale_x(), f32::NAN);
            let scale_y = parse_f32_or(api.get_prop_scale_y(), f32::NAN);
            let rotation = parse_f32_or(api.get_prop_rotation(), f32::NAN);
            let crop_left = parse_u32_or(api.get_prop_crop_left(), u32::MAX);
            let crop_top = parse_u32_or(api.get_prop_crop_top(), u32::MAX);
            let crop_right = parse_u32_or(api.get_prop_crop_right(), u32::MAX);
            let crop_bottom = parse_u32_or(api.get_prop_crop_bottom(), u32::MAX);

            let mut c = controller.borrow_mut();
            if let Some(item) = c
                .scenes
                .current_scene_mut()
                .and_then(|scene| scene.item_mut(id))
            {
                // Unparseable fields fall back to the item's current value
                // (the egui DragValues could never produce invalid text).
                item.set_position(Position::new(
                    if pos_x.is_nan() { item.position().x } else { pos_x },
                    if pos_y.is_nan() { item.position().y } else { pos_y },
                ));
                item.set_scale(Scale::new(
                    if scale_x.is_nan() { item.scale().x } else { scale_x },
                    if scale_y.is_nan() { item.scale().y } else { scale_y },
                ));
                if !rotation.is_nan() {
                    item.set_rotation(rotation);
                }
                item.set_crop(Crop::new(
                    if crop_left == u32::MAX { item.crop().left } else { crop_left },
                    if crop_top == u32::MAX { item.crop().top } else { crop_top },
                    if crop_right == u32::MAX { item.crop().right } else { crop_right },
                    if crop_bottom == u32::MAX { item.crop().bottom } else { crop_bottom },
                ));
            }
            api.set_properties_open(false);
            state.borrow_mut().editing_id = None;
        });
        let weak = component.clone();
        api.on_cancel_properties(move || {
            if let Some(root) = weak.upgrade() {
                root.global::<SourcesApi>().set_properties_open(false);
            }
            cancel_state.borrow_mut().editing_id = None;
        });
    }

    // ---- Text overlays ----
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_add_overlay(move |text: slint::SharedString| {
                let text = text.trim().to_string();
                if text.is_empty() {
                    return;
                }
                let mut c = controller.borrow_mut();
                c.log_event(
                    format!("Text overlay added: \"{text}\""),
                    EventLogKind::Overlay,
                );
                c.text_overlays.push(TextOverlay::new(text));
            });
        }
    }
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_remove_overlay(move |id: i32| {
                let id = ObjectId(id.max(0) as u64);
                controller.borrow_mut().text_overlays.retain(|o| o.id() != id);
            });
        }
    }
    {
        let controller = Rc::clone(controller);
        if let Some(root) = component.upgrade() {
            let api = root.global::<SourcesApi>();
            api.on_set_overlay_visible(move |id: i32, visible: bool| {
                let id = ObjectId(id.max(0) as u64);
                let mut c = controller.borrow_mut();
                if let Some(overlay) = c.text_overlays.iter_mut().find(|o| o.id() == id) {
                    overlay.set_visible(visible);
                }
            });
        }
    }
}

/// Per-tick push: refresh the window picker on its cadence and update the
/// scene-derived models only when a row actually changed.
pub fn push(component: &MainWindow, controller: &mut RobsController, state: &mut SourcesUi) {
    let api = component.global::<SourcesApi>();

    if state.last_window_refresh.elapsed() >= WINDOW_REFRESH {
        state.last_window_refresh = Instant::now();
        state.windows = get_open_windows();
    }

    // ---- Static-ish picker lists ----
    let monitors: Vec<String> = state.monitors.iter().map(SourcesUi::monitor_label).collect();
    if monitors != state.monitors_mirror {
        state.monitors_mirror = monitors.clone();
        api.set_monitor_names(ModelRc::new(VecModel::from(
            monitors.into_iter().map(SharedString::from).collect::<Vec<_>>(),
        )));
    }
    let cameras = state.cameras.clone();
    if cameras != state.cameras_mirror {
        state.cameras_mirror = cameras.clone();
        api.set_camera_names(ModelRc::new(VecModel::from(
            cameras.into_iter().map(SharedString::from).collect::<Vec<_>>(),
        )));
    }
    let windows: Vec<String> = state
        .windows
        .iter()
        .take(WINDOW_MAX)
        .map(|w| truncate(&w.title, WINDOW_TITLE_CHARS))
        .collect();
    if windows != state.windows_mirror {
        state.windows_mirror = windows.clone();
        api.set_window_names(ModelRc::new(VecModel::from(
            windows.into_iter().map(SharedString::from).collect::<Vec<_>>(),
        )));
    }

    // ---- Sources of the current scene ----
    let rows: Vec<(SceneItemId, String, bool)> = controller
        .scenes
        .current_scene()
        .map(|scene| {
            scene
                .items()
                .iter()
                .map(|item| (item.id(), item.name().to_string(), item.is_visible()))
                .collect()
        })
        .unwrap_or_default();
    if rows != state.rows_mirror {
        state.rows_mirror = rows.clone();
        let view: Vec<SourceRowView> = rows
            .iter()
            .map(|(id, name, visible)| SourceRowView {
                id: id.0 .0 as i32,
                name: name.as_str().into(),
                visible: *visible,
            })
            .collect();
        api.set_source_rows(ModelRc::new(VecModel::from(view)));
    }

    // ---- Text overlays ----
    let overlays: Vec<(ObjectId, String, bool)> = controller
        .text_overlays
        .iter()
        .map(|o| (o.id(), o.text().to_string(), o.is_visible()))
        .collect();
    if overlays != state.overlays_mirror {
        state.overlays_mirror = overlays.clone();
        let view: Vec<OverlayView> = overlays
            .iter()
            .map(|(id, text, visible)| OverlayView {
                id: id.0 as i32,
                text: truncate(text, OVERLAY_TEXT_CHARS).into(),
                visible: *visible,
            })
            .collect();
        api.set_overlays(ModelRc::new(VecModel::from(view)));
    }
}

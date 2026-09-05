//! Per-tick state snapshot: controller -> `Api` properties/models.
//!
//! Every tick pushes the status flags, the letterboxed canvas geometry (the
//! fit math is ported verbatim from the old
//! `robs-ui/src/app/panels/preview.rs`), fresh preview frames as
//! `SharedPixelBuffer` images (only when a frame's version changed), and the
//! tail of the event log. Mirrors of the last pushed data keep unchanged
//! rows untouched so a 30 fps tick stays cheap.

use std::collections::HashMap;
use std::time::Duration;

use crate::canvas_glue::{ann_bbox, path_commands, tool_index, TEXT_FONT_SIZE};
use crate::{
    AnnotationView, Api, CanvasApi, CanvasTextView, LogLineView, MainWindow, PanelsApi,
    SceneItemView,
};
use robs_controller::state::{EventLogEntry, EventLogKind, PreviewFrame};
use robs_controller::RobsController;
use robs_core::{AnnotationShape, SceneItemId};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString,
    VecModel,
};

/// Event-log rows pushed to the UI (the tail of the controller's 200-entry
/// ring).
const LOG_ROWS: usize = 100;

/// Snapshot-toast display window (matches the old egui panel).
const SNAPSHOT_FLASH: Duration = Duration::from_millis(1500);

/// Shell layout constants — MUST mirror `ui/theme.slint`
/// (`topbar-h`, `rail-w`, `right-w`, `actions-h`, `log-h`): the glue
/// letterboxes the canvas from these and the markup places the regions with
/// the same values.
pub mod layout {
    pub const TOPBAR_H: f32 = 40.0;
    pub const RAIL_W: f32 = 220.0;
    pub const RIGHT_W: f32 = 300.0;
    pub const ACTIONS_H: f32 = 52.0;
    pub const LOG_H: f32 = 110.0;
    /// Annotation toolbar strip height — mirrors `ui/theme.slint`
    /// (`annotations-h`).
    pub const TOOLBAR_H: f32 = 36.0;
}

/// Handles to the models installed in `Api`, plus mirrors of the last pushed
/// data for change detection. The two item models are owned as `ModelRc`
/// handles (the clonable model type); when the item count changes they are
/// rebuilt wholesale, otherwise rows are updated in place.
pub struct PushedState {
    pub scene_items: ModelRc<SceneItemView>,
    pub item_frames: ModelRc<Image>,
    items_mirror: Vec<ItemMirror>,
    /// Last `PreviewFrame::version` pushed per item.
    frame_versions: HashMap<SceneItemId, u64>,
    names_mirror: Vec<String>,
    /// `event_log.len()` at the last push.
    log_len: usize,
    /// Canvas geometry from the last push; canvas-relative pointer events
    /// convert to scene units through `scale` (see `canvas_glue`).
    pub canvas: CanvasGeom,
    /// Signature of the last pushed annotation model.
    ann_mirror: Vec<AnnSig>,
    /// Signature of the last pushed overlay model.
    overlay_mirror: Vec<OverlaySig>,
    /// Editing-active at the previous push (seeds `text-input` on rising edge).
    editing_prev: bool,
}

/// Canvas fit scale from the last push (scene units per canvas pixel;
/// computed by [`push_state`] each tick). Pointer events arrive
/// canvas-relative, so only the scale is needed to convert them.
#[derive(Clone, Copy)]
pub struct CanvasGeom {
    pub scale: f32,
}

/// Change-detection signature of one pushed annotation row.
#[derive(Clone, PartialEq)]
struct AnnSig {
    id: i32,
    commands: String,
    stroke: [u8; 4],
    stroke_width: f32,
    filled: bool,
    is_text: bool,
    text: String,
    x: f32,
    y: f32,
    font_size: f32,
}

/// Change-detection signature of one pushed overlay row.
#[derive(Clone, PartialEq)]
struct OverlaySig {
    id: i32,
    text: String,
    color: [u8; 4],
    x: f32,
    y: f32,
}

struct ItemMirror {
    id: SceneItemId,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    visible: bool,
    has_frame: bool,
}

impl ItemMirror {
    /// True when any pushed field changed and the row must be re-set.
    fn differs_from(&self, id: SceneItemId, row: &SceneItemView) -> bool {
        self.id != id
            || self.x != row.x
            || self.y != row.y
            || self.w != row.width
            || self.h != row.height
            || self.visible != row.visible
            || self.has_frame != row.has_frame
    }
}

impl PushedState {
    pub fn new() -> Self {
        Self {
            scene_items: ModelRc::new(VecModel::default()),
            item_frames: ModelRc::new(VecModel::default()),
            items_mirror: Vec::new(),
            frame_versions: HashMap::new(),
            names_mirror: Vec::new(),
            log_len: 0,
            canvas: CanvasGeom { scale: 1.0 },
            ann_mirror: Vec::new(),
            overlay_mirror: Vec::new(),
            editing_prev: false,
        }
    }
}

/// One scene item as observed from the controller, copied out before any
/// model mutation (borrow idiom: collect first, mutate after).
struct ItemData {
    id: SceneItemId,
    name: String,
    px: f32,
    py: f32,
    sx: f32,
    sy: f32,
    visible: bool,
    /// Native capture size, when known (window capture resolves at runtime).
    native: Option<(u32, u32)>,
}

pub fn push_state(
    component: &MainWindow,
    controller: &mut RobsController,
    pushed: &mut PushedState,
) {
    let api = component.global::<Api>();

    // Window size in logical pixels (all pushed lengths are logical px).
    let window = component.window();
    let scale = window.scale_factor();
    let size = window.size();
    let win_w = size.width as f32 / scale;
    let win_h = size.height as f32 / scale;

    // Copy the scene data out of the controller first.
    let (scene_w, scene_h) = controller
        .scenes
        .current_scene()
        .map(|s| s.output_size())
        .unwrap_or((1920, 1080));
    let scene_present = controller.scenes.current_scene().is_some();
    let items: Vec<ItemData> = controller
        .scenes
        .current_scene()
        .map(|s| s.items().iter().map(|i| ItemData {
            id: i.id(),
            name: i.name().to_string(),
            px: i.position().x,
            py: i.position().y,
            sx: i.scale().x,
            sy: i.scale().y,
            visible: i.is_visible(),
            native: i.capture().and_then(|c| c.native_size()),
        }).collect())
        .unwrap_or_default();

    // ---- Canvas fit math (ported verbatim from the old preview panel) ----
    // Effective insets mirror the View-menu visibility bindings in
    // `ui/mainwindow.slint` (rail/right/log/actions collapse to zero when
    // the corresponding PanelsApi show-* flag is off).
    let panels = component.global::<PanelsApi>();
    let rail_w = if panels.get_show_scenes() { layout::RAIL_W } else { 0.0 };
    let right_shown =
        panels.get_show_audio() || panels.get_show_chat() || panels.get_show_stats();
    let right_w = if right_shown { layout::RIGHT_W } else { 0.0 };
    let actions_h = if panels.get_show_controls() { layout::ACTIONS_H } else { 0.0 };
    let log_h = if panels.get_show_event_log() { layout::LOG_H } else { 0.0 };
    // The annotation toolbar inserts a strip below the top bar while shown
    // (mirrors `annot-shift` in `ui/mainwindow.slint`).
    let toolbar_h = if panels.get_show_annotations() { layout::TOOLBAR_H } else { 0.0 };
    let area_x = rail_w;
    let area_y = layout::TOPBAR_H + toolbar_h;
    let area_w = (win_w - rail_w - right_w).max(1.0);
    let area_h = (win_h - layout::TOPBAR_H - toolbar_h - actions_h - log_h).max(1.0);

    let scale_x = area_w / scene_w as f32;
    let scale_y = area_h / scene_h as f32;
    let canvas_scale = scale_x.min(scale_y) * 0.95;
    let canvas_w = scene_w as f32 * canvas_scale;
    let canvas_h = scene_h as f32 * canvas_scale;
    let canvas_x = area_x + (area_w - canvas_w) / 2.0;
    let canvas_y = area_y + (area_h - canvas_h) / 2.0;

    api.set_canvas_x(canvas_x);
    api.set_canvas_y(canvas_y);
    api.set_canvas_width(canvas_w);
    api.set_canvas_height(canvas_h);
    pushed.canvas = CanvasGeom { scale: canvas_scale };

    // ---- Scene items: geometry rows + frames ----
    // Build the desired row/mirror state for every item (scene data was
    // copied out above; no controller borrows are held here).
    let mut rows: Vec<SceneItemView> = Vec::with_capacity(items.len());
    let mut mirrors: Vec<ItemMirror> = Vec::with_capacity(items.len());
    for item in &items {
        // Source dimensions: typed capture metadata, falling back to the
        // scene output size (window capture and non-capture sources).
        let (src_w, src_h) = item
            .native
            .map(|(w, h)| (w as f32, h as f32))
            .unwrap_or((scene_w as f32, scene_h as f32));

        // Rendered rect after scaling, scene coords -> canvas coords.
        let x = item.px * canvas_scale;
        let y = item.py * canvas_scale;
        let w = src_w * item.sx * canvas_scale;
        let h = src_h * item.sy * canvas_scale;

        let has_frame = controller.preview.preview_frames.contains_key(&item.id);
        rows.push(SceneItemView {
            id: item.id.0 .0 as i32,
            name: item.name.as_str().into(),
            x,
            y,
            width: w,
            height: h,
            visible: item.visible,
            has_frame,
        });
        mirrors.push(ItemMirror {
            id: item.id,
            x,
            y,
            w,
            h,
            visible: item.visible,
            has_frame,
        });
    }

    let count_changed = pushed.scene_items.row_count() != rows.len();
    if count_changed {
        // Item sets change rarely: rebuild both models wholesale.
        pushed.frame_versions.clear();
        let mut frames = Vec::with_capacity(rows.len());
        for item in &items {
            match controller.preview.preview_frames.get(&item.id) {
                Some(f) => {
                    pushed.frame_versions.insert(item.id, f.version);
                    frames.push(frame_to_image(f));
                }
                None => frames.push(Image::default()),
            }
        }
        pushed.items_mirror = mirrors;
        pushed.scene_items = ModelRc::new(VecModel::from(rows));
        pushed.item_frames = ModelRc::new(VecModel::from(frames));
        api.set_scene_items(pushed.scene_items.clone());
        api.set_item_frames(pushed.item_frames.clone());
    } else {
        for (i, item) in items.iter().enumerate() {
            let row_changed = pushed.items_mirror[i]
                .differs_from(item.id, &rows[i]);
            if row_changed {
                pushed.scene_items.set_row_data(i, rows[i].clone());
            }

            // Upload a frame only when its version changed.
            let frame = controller.preview.preview_frames.get(&item.id);
            let changed =
                match (frame.map(|f| f.version), pushed.frame_versions.get(&item.id)) {
                    (Some(v), Some(last)) => v != *last,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
            if changed {
                match frame {
                    Some(f) => {
                        pushed.item_frames.set_row_data(i, frame_to_image(f));
                        pushed.frame_versions.insert(item.id, f.version);
                    }
                    None => {
                        pushed.item_frames.set_row_data(i, Image::default());
                        pushed.frame_versions.remove(&item.id);
                    }
                }
            }
        }
        pushed.items_mirror = mirrors;
    }

    // ---- Scenes rail ----
    let mut names: Vec<String> = controller
        .scenes
        .list()
        .iter()
        .map(|s| s.to_string())
        .collect();
    names.sort();
    if names != pushed.names_mirror {
        pushed.names_mirror = names.clone();
        api.set_scene_names(ModelRc::new(VecModel::from(
            names.into_iter().map(SharedString::from).collect::<Vec<_>>(),
        )));
    }
    api.set_selected_scene(controller.scenes.current_scene_name().unwrap_or("").into());

    // ---- Event log (only when it grew) ----
    if controller.event_log.len() != pushed.log_len {
        pushed.log_len = controller.event_log.len();
        let rows: Vec<LogLineView> = controller
            .event_log
            .iter()
            .rev()
            .take(LOG_ROWS)
            .rev()
            .map(log_line_view)
            .collect();
        api.set_log_lines(ModelRc::new(VecModel::from(rows)));
    }

    // ---- Status snapshot ----
    api.set_scene_present(scene_present);
    api.set_recording(controller.record.recording);
    api.set_recording_paused(controller.record.recording_paused);
    api.set_recording_time(
        RobsController::format_time(controller.record.recording_time / 1000).into(),
    );
    api.set_streaming(controller.streaming);
    api.set_streaming_paused(controller.streaming_paused);
    api.set_streaming_time(
        RobsController::format_time(controller.streaming_time / 1000).into(),
    );
    // Quick-action enablement, exact parity with the old bar.
    api.set_can_start(
        controller.record.recording || controller.scene_has_sources(),
    );
    api.set_can_snapshot(controller.record.recording);
    api.set_can_mark(
        controller.record.recording
            && controller.record.clip_marking_supported
            && controller.record.clip_export_pending == 0,
    );
    api.set_mark_open(controller.record.clip_mark_start.is_some());
    api.set_snapshot_flash(
        controller
            .snapshot_flash
            .map_or(false, |t| t.elapsed() < SNAPSHOT_FLASH),
    );

    // ---- Phase 3: CanvasApi snapshot ----
    push_canvas(
        &component.global::<CanvasApi>(),
        controller,
        pushed,
        scene_w,
        scene_h,
        canvas_scale,
    );
}

// ---------------------------------------------------------------------------
// Phase 3: CanvasApi snapshot
// ---------------------------------------------------------------------------

/// Push the `CanvasApi` snapshot: toolbar mirrors, committed annotations as
/// Path commands (scene coordinates; the markup's viewbox letterboxes them),
/// the in-progress drawing, the selection outline, on-canvas text overlays,
/// and the inline-editor state. Mirror-compared like the other per-tick
/// models. Shared geometry helpers live in `canvas_glue`.
fn push_canvas(
    api: &CanvasApi,
    controller: &mut RobsController,
    pushed: &mut PushedState,
    scene_w: u32,
    scene_h: u32,
    scale: f32,
) {
    let ann = &controller.annotation;
    let font_px = (TEXT_FONT_SIZE * scale).max(8.0);

    // ---- Toolbar mirrors ----
    api.set_scene_width(scene_w as f32);
    api.set_scene_height(scene_h as f32);
    api.set_tool(tool_index(ann.annotation_tool));
    let style = ann.annotation_style;
    api.set_stroke_color(Color::from_argb_u8(
        style.color[3],
        style.color[0],
        style.color[1],
        style.color[2],
    ));
    api.set_fill_enabled(
        ann.annotation_tool.shape().map(|s| s.is_closed()).unwrap_or(false),
    );
    api.set_filled(style.filled);
    // Items stop being draggable while a drawing tool is active (egui
    // `draw_active` parity).
    api.set_draw_active(ann.show_annotations && ann.annotation_tool.shape().is_some());
    // The slider is two-way; only write back on drift so user edits survive.
    if (api.get_stroke_width() - style.stroke_width).abs() > 0.01 {
        api.set_stroke_width(style.stroke_width);
    }

    // ---- Committed annotations ----
    let mut sigs: Vec<AnnSig> = Vec::with_capacity(ann.annotations.len());
    let mut rows: Vec<AnnotationView> = Vec::with_capacity(ann.annotations.len());
    for annotation in &ann.annotations {
        if !annotation.is_visible() {
            continue;
        }
        // Each annotation snapshots the toolbar style at creation time.
        let ann_style = annotation.style();
        let start = annotation.start();
        let sig = AnnSig {
            id: annotation.id().0 .0 as i32,
            commands: path_commands(annotation, scale),
            stroke: ann_style.color,
            stroke_width: ann_style.stroke_width,
            filled: ann_style.filled,
            is_text: annotation.shape() == AnnotationShape::Text,
            text: annotation.text().to_string(),
            x: start.x * scale,
            y: start.y * scale,
            font_size: font_px,
        };
        rows.push(AnnotationView {
            id: sig.id,
            commands: sig.commands.as_str().into(),
            stroke: Color::from_argb_u8(
                sig.stroke[3],
                sig.stroke[0],
                sig.stroke[1],
                sig.stroke[2],
            ),
            stroke_width: sig.stroke_width,
            filled: sig.filled,
            is_text: sig.is_text,
            text: sig.text.as_str().into(),
            x: sig.x,
            y: sig.y,
            font_size: sig.font_size,
            selected: ann.selected_annotation == Some(annotation.id()),
        });
        sigs.push(sig);
    }
    if sigs != pushed.ann_mirror {
        pushed.ann_mirror = sigs;
        api.set_annotations(ModelRc::new(VecModel::from(rows)));
    }

    // ---- In-progress drawing ----
    match &ann.annotation_drawing {
        Some(drawing) => {
            let drawing_style = drawing.style();
            api.set_drawing(AnnotationView {
                id: drawing.id().0 .0 as i32,
                commands: path_commands(drawing, scale).into(),
                stroke: Color::from_argb_u8(
                    drawing_style.color[3],
                    drawing_style.color[0],
                    drawing_style.color[1],
                    drawing_style.color[2],
                ),
                stroke_width: drawing_style.stroke_width,
                filled: drawing_style.filled,
                is_text: false,
                text: "".into(),
                x: 0.0,
                y: 0.0,
                font_size: 0.0,
                selected: false,
            });
            api.set_drawing_active(true);
        }
        None => api.set_drawing_active(false),
    }

    // ---- Selection outline (canvas px; the markup expands it by 4px) ----
    let sel_rect = ann
        .selected_annotation
        .and_then(|sel| ann.annotations.iter().find(|a| a.id() == sel))
        .and_then(|sel_ann| ann_bbox(sel_ann, scale));
    match sel_rect {
        Some((x, y, w, h)) => {
            api.set_has_selection(true);
            api.set_sel_x(x);
            api.set_sel_y(y);
            api.set_sel_width(w);
            api.set_sel_height(h);
        }
        None => api.set_has_selection(false),
    }

    // ---- Text overlays ----
    let mut overlay_sigs: Vec<OverlaySig> = Vec::new();
    let mut overlay_rows: Vec<CanvasTextView> = Vec::new();
    for overlay in controller.text_overlays.iter().filter(|o| o.is_visible()) {
        let pos = overlay.position();
        let sig = OverlaySig {
            id: overlay.id().0 as i32,
            text: overlay.text().to_string(),
            color: overlay.color(),
            x: pos.x * scale,
            y: pos.y * scale,
        };
        overlay_rows.push(CanvasTextView {
            id: sig.id,
            text: sig.text.as_str().into(),
            x: sig.x,
            y: sig.y,
            font_size: (overlay.font_size() * scale).max(8.0),
        });
        overlay_sigs.push(sig);
    }
    if overlay_sigs != pushed.overlay_mirror {
        pushed.overlay_mirror = overlay_sigs;
        api.set_text_overlays(ModelRc::new(VecModel::from(overlay_rows)));
    }

    // ---- Inline text editor ----
    let editing = ann
        .editing_text_id
        .and_then(|id| ann.annotations.iter().find(|a| a.id() == id))
        .map(|edited| {
            let start = edited.start();
            (start.x * scale, start.y * scale)
        });
    match editing {
        Some((x, y)) => {
            api.set_editing(true);
            api.set_edit_x(x);
            api.set_edit_y(y);
            if !pushed.editing_prev {
                // Rising edge: seed the two-way LineEdit buffer.
                api.set_text_input(ann.text_input.as_str().into());
            }
        }
        None => api.set_editing(false),
    }
    pushed.editing_prev = editing.is_some();
}

/// RGBA preview frame -> Slint image (straight alpha, unmultiplied).
fn frame_to_image(frame: &PreviewFrame) -> Image {
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &frame.data,
        frame.width,
        frame.height,
    );
    Image::from_rgba8(buffer)
}

fn log_line_view(entry: &EventLogEntry) -> LogLineView {
    LogLineView {
        timestamp: entry.timestamp.format("%H:%M:%S").to_string().into(),
        kind: kind_str(entry.kind).into(),
        message: entry.message.as_str().into(),
    }
}

fn kind_str(kind: EventLogKind) -> &'static str {
    match kind {
        EventLogKind::Stream => "stream",
        EventLogKind::Record => "record",
        EventLogKind::Info => "info",
        EventLogKind::Annotation => "annotation",
        EventLogKind::Overlay => "overlay",
    }
}

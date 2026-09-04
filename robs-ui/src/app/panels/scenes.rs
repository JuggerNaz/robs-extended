//! Left-hand Scenes + Sources panel (add/remove capture sources, text overlays,
//! per-item context menu). Extracted verbatim from `app.rs`.

use robs_controller::devices::{get_monitors, get_video_devices};
use robs_controller::state::EventLogKind;
use super::super::RobsApp;
use eframe::egui;
use robs_core::scene::CaptureSource;
use robs_core::types::SourceId;
use robs_sources::native_capture::get_open_windows;

impl RobsApp {
    pub(crate) fn scenes_panel(&mut self, ctx: &egui::Context) {
        if self.show_scenes {
            egui::SidePanel::left("scenes_panel")
                .default_width(250.0)
                .min_width(200.0)
                .resizable(true)
                .show_animated(ctx, true, |ui| {
                    ui.heading("Scenes");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("+ Add").clicked() {
                            let name = format!("Scene {}", self.scenes.count() + 1);
                            self.scenes.create_scene(name.clone());
                            self.scenes.set_current_scene(&name);
                            self.current_scene = name;
                        }
                        if ui.button("\u{2212}").clicked() {
                            if self.scenes.count() > 1 {
                                if let Some(name) = self.scenes.current_scene_name() {
                                    let name = name.to_string(); // Copy for later use
                                    self.scenes.remove(&name);
                                    // Switch to first available scene - collect names first
                                    let scene_names: Vec<String> =
                                        self.scenes.list().iter().map(|s| s.to_string()).collect();
                                    if let Some(first) = scene_names.first() {
                                        self.scenes.set_current_scene(first);
                                        self.current_scene = first.clone();
                                    }
                                }
                            }
                        }
                    });
                    ui.separator();

                    // Collect scene list to avoid borrow issues
                    let scene_names: Vec<String> =
                        self.scenes.list().iter().map(|s| s.to_string()).collect();

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for name in &scene_names {
                            let selected = self.current_scene == *name;
                            if ui.selectable_label(selected, name).clicked() {
                                self.scenes.set_current_scene(name);
                                self.current_scene = name.clone();
                            }
                        }
                    });

                    // Show sources for current scene - get data first to avoid borrow issues
                    let scene_name = self.scenes.current_scene_name().map(|s| s.to_string());
                    if let Some(name) = scene_name {
                        ui.separator();
                        ui.heading("Sources");
                        ui.separator();
                        ui.menu_button("+ Add Source", |ui| {
                            ui.menu_button("Display Capture", |ui| {
                                let monitors = get_monitors();
                                if monitors.is_empty() {
                                    ui.label("No monitors detected");
                                } else {
                                    ui.label("Select Monitor:");
                                    for monitor in &monitors {
                                        let label = if monitor.is_primary {
                                            format!(
                                                "{} ({}x{} - PRIMARY)",
                                                monitor.name, monitor.width, monitor.height
                                            )
                                        } else {
                                            format!(
                                                "{} ({}x{} @ {},{})",
                                                monitor.name,
                                                monitor.width,
                                                monitor.height,
                                                monitor.position_x,
                                                monitor.position_y
                                            )
                                        };
                                        if ui.button(label).clicked() {
                                            if let Some(scene) = self.scenes.current_scene_mut() {
                                                let source_id =
                                                    SourceId(robs_core::types::ObjectId::new());
                                                let mon_name = if monitor.is_primary {
                                                    "Primary".to_string()
                                                } else {
                                                    monitor.name.clone()
                                                };
                                                let capture = CaptureSource::Display {
                                                    x: monitor.position_x,
                                                    y: monitor.position_y,
                                                    width: monitor.width,
                                                    height: monitor.height,
                                                    label: mon_name,
                                                };
                                                let name = capture.display_name();
                                                let item_id = scene.add_source(source_id, name);
                                                if let Some(item) = scene.item_mut(item_id) {
                                                    item.set_capture(Some(capture));
                                                }
                                            }
                                            ui.close_menu();
                                        }
                                    }
                                }
                            });
                            ui.menu_button("Window Capture", |ui| {
                                let windows = get_open_windows();
                                if windows.is_empty() {
                                    ui.label("No windows found");
                                } else {
                                    ui.label("Select Window:");
                                    for window in windows.iter().take(50) {
                                        let label: String = window.title.chars().take(40).collect();
                                        let label = if window.title.chars().count() > 40 {
                                            format!("{}...", label)
                                        } else {
                                            label
                                        };

                                        if ui.button(&label).clicked() {
                                            if let Some(scene) = self.scenes.current_scene_mut() {
                                                let source_id =
                                                    SourceId(robs_core::types::ObjectId::new());
                                                let capture = CaptureSource::Window {
                                                    title: window.title.clone(),
                                                };
                                                let name = capture.display_name();
                                                let item_id = scene.add_source(source_id, name);
                                                if let Some(item) = scene.item_mut(item_id) {
                                                    item.set_capture(Some(capture));
                                                }
                                                self.window_hwnds.insert(item_id, window.hwnd);
                                            }
                                            ui.close_menu();
                                        }
                                    }
                                }
                            });
                            ui.menu_button("Video Capture Device", |ui| {
                                let cameras = get_video_devices();
                                if cameras.is_empty() {
                                    ui.label("No webcam devices found");
                                } else {
                                    ui.label("Select Device:");
                                    for cam in &cameras {
                                        if ui.button(cam).clicked() {
                                            if let Some(scene) = self.scenes.current_scene_mut() {
                                                let source_id =
                                                    SourceId(robs_core::types::ObjectId::new());
                                                let capture = CaptureSource::Webcam {
                                                    device: cam.clone(),
                                                    width: 1280,
                                                    height: 720,
                                                };
                                                let name = capture.display_name();
                                                let item_id = scene.add_source(source_id, name);
                                                if let Some(item) = scene.item_mut(item_id) {
                                                    item.set_capture(Some(capture));
                                                }
                                                self.log_event(
                                                    format!("Video source added: {}", cam),
                                                    EventLogKind::Info,
                                                );
                                                let wc = robs_sources::native_capture::WebcamCapture::new(
                                                    cam, 1280, 720, 30.0,
                                                );
                                                if wc.is_none() {
                                                    eprintln!("[Webcam] Failed to start capture for: {}", cam);
                                                }
                                                if let Some(wc) = wc {
                                                    self.webcam_captures.insert(item_id, wc);
                                                }
                                            }
                                            ui.close_menu();
                                        }
                                    }
                                }
                            });
                        });

                        // Display sources in current scene - get items first
                        let items_data: Vec<_> = if let Some(scene) = self.scenes.get(&name) {
                            scene
                                .items()
                                .iter()
                                .map(|i| {
                                    let pos = i.position();
                                    let scale = i.scale();
                                    let crop = i.crop();
                                    (
                                        i.id(),
                                        i.name().to_string(),
                                        i.is_visible(),
                                        pos.x,
                                        pos.y,
                                        scale.x,
                                        scale.y,
                                        i.rotation(),
                                        crop.left,
                                        crop.top,
                                        crop.right,
                                        crop.bottom,
                                    )
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };

                        for (
                            id,
                            item_name,
                            is_visible,
                            _px,
                            _py,
                            _sx,
                            _sy,
                            _rot,
                            _cl,
                            _ct,
                            _cr,
                            _cb,
                        ) in items_data
                        {
                            let mut visible = is_visible;
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut visible, "");
                                // Source names are already clean display labels now
                                // (typed metadata carries the parameters separately).
                                let display_name = item_name.clone();
                                let response = ui.add(
                                    egui::Label::new(&display_name).truncate(),
                                );

                                // Context menu: Remove, Properties
                                response.context_menu(|ui| {
                                    if ui.button("Properties").clicked() {
                                        // Load source properties into editing fields.
                                        // Snapshot the item through the scene borrow
                                        // first, then write: `scenes` and `editing`
                                        // are both reached through the controller
                                        // deref, so the two borrows cannot overlap.
                                        let props = self
                                            .scenes
                                            .get_mut(&name)
                                            .and_then(|scene| scene.item_mut(id))
                                            .map(|item| {
                                                (
                                                    item.name().to_string(),
                                                    item.position(),
                                                    item.scale(),
                                                    item.rotation(),
                                                    item.crop(),
                                                )
                                            });
                                        if let Some((item_name, pos, scale, rotation, crop)) = props {
                                            self.editing.editing_source_id = Some(id);
                                            self.editing.editing_source_name = item_name;
                                            self.editing.editing_source_pos_x = pos.x;
                                            self.editing.editing_source_pos_y = pos.y;
                                            self.editing.editing_source_scale_x = scale.x;
                                            self.editing.editing_source_scale_y = scale.y;
                                            self.editing.editing_source_rotation = rotation;
                                            self.editing.editing_source_crop_left = crop.left;
                                            self.editing.editing_source_crop_top = crop.top;
                                            self.editing.editing_source_crop_right = crop.right;
                                            self.editing.editing_source_crop_bottom = crop.bottom;
                                            self.editing.show_source_properties = true;
                                        }
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    // Move order controls
                                    ui.horizontal(|ui| {
                                        ui.label("Order:");
                                        if ui.small_button("\u{2191}").clicked() {
                                            if let Some(scene) = self.scenes.get_mut(&name) {
                                                scene.move_item_up(id);
                                            }
                                            ui.close_menu();
                                        }
                                        if ui.small_button("\u{2193}").clicked() {
                                            if let Some(scene) = self.scenes.get_mut(&name) {
                                                scene.move_item_down(id);
                                            }
                                            ui.close_menu();
                                        }
                                    });
                                    ui.separator();
                                    if ui.button("Remove").clicked() {
                                        if let Some(scene) = self.scenes.get_mut(&name) {
                                            scene.remove_item(id);
                                        }
                                        ui.close_menu();
                                    }
                                });
                            });
                            // Update visibility if changed
                            if visible != is_visible {
                                if let Some(scene) = self.scenes.current_scene_mut() {
                                    scene.set_item_visible(id, visible);
                                }
                            }
                        }

                        // ---- Text Overlays ----
                        ui.separator();
                        ui.heading("Text Overlays");
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.overlay_text_input)
                                    .hint_text("Enter overlay text...")
                                    .desired_width(140.0)
                                    .desired_rows(3),
                            );
                            if ui.button("Add").clicked() && !self.overlay_text_input.trim().is_empty() {
                                let text = self.overlay_text_input.trim().to_string();
                                self.log_event(format!("Text overlay added: \"{}\"", text), EventLogKind::Overlay);
                                self.text_overlays.push(robs_core::TextOverlay::new(text));
                                self.overlay_text_input.clear();
                            }
                        });

                        let mut overlay_remove: Option<robs_core::ObjectId> = None;
                        for ov in &mut self.text_overlays {
                            ui.horizontal(|ui| {
                                ui.checkbox(ov.visible_mut(), "");
                                let text = ov.text().to_string();
                                let label: String = text.chars().take(28).collect();
                                let label = if text.chars().count() > 28 {
                                    format!("{}...", label)
                                } else {
                                    label
                                };
                                ui.add(egui::Label::new(label).truncate());
                                if ui.small_button("X").clicked() {
                                    overlay_remove = Some(ov.id());
                                }
                            });
                        }
                        if let Some(id) = overlay_remove {
                            self.text_overlays.retain(|o| o.id() != id);
                        }
                    }
                });
        }
    }
}

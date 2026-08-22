//! Central preview panel: scene-item rendering, drag/resize, annotation overlay,
//! and the quick-actions bar. Extracted verbatim from `app.rs`.

use super::super::state::EventLogKind;
use super::super::RobsApp;
use eframe::egui;
use robs_core::scene::{CaptureSource, Position, Scale};

impl RobsApp {
    pub(crate) fn preview_panel(&mut self, ctx: &egui::Context) {
        if self.show_preview {
            egui::CentralPanel::default().show(ctx, |ui| {
                let full_rect = ui.available_rect_before_wrap();

                // Reserve space at bottom for the quick actions bar
                let actions_height = 52.0;
                let actions_rect = egui::Rect::from_min_size(
                    egui::pos2(full_rect.min.x, full_rect.max.y - actions_height),
                    egui::vec2(full_rect.width(), actions_height),
                );
                let rect = egui::Rect::from_min_max(
                    full_rect.min,
                    egui::pos2(full_rect.max.x, full_rect.max.y - actions_height),
                );

                // Draw background
                ui.painter()
                    .rect_filled(rect, 2.0, egui::Color32::from_rgb(30, 30, 30));

                // Get scene and calculate canvas area
                let scene = self.scenes.current_scene();
                let (scene_output_w, scene_output_h) =
                    scene.map(|s| s.output_size()).unwrap_or((1920, 1080));

                // Calculate preview canvas size (fit scene output into available rect)
                let available_size = rect.size();
                let scale_x = available_size.x / scene_output_w as f32;
                let scale_y = available_size.y / scene_output_h as f32;
                let canvas_scale = scale_x.min(scale_y) * 0.95;

                let canvas_w = scene_output_w as f32 * canvas_scale;
                let canvas_h = scene_output_h as f32 * canvas_scale;
                let canvas_off_x = (available_size.x - canvas_w) / 2.0;
                let canvas_off_y = (available_size.y - canvas_h) / 2.0;

                let canvas_rect = egui::Rect::from_min_size(
                    rect.min + egui::vec2(canvas_off_x, canvas_off_y),
                    egui::vec2(canvas_w, canvas_h),
                );

                // Draw canvas border
                ui.painter().rect_stroke(
                    canvas_rect,
                    2.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 60)),
                );

                // A draw tool overrides source-item dragging so the canvas is
                // free for drawing annotations.
                let draw_active =
                    self.annotation.show_annotations && self.annotation.annotation_tool.shape().is_some();

                // Render scene items
                if let Some(scene) = scene {
                    // Collect items to avoid borrow issues
                    let items: Vec<_> = scene
                        .items()
                        .iter()
                        .map(|i| {
                            let pos = i.position();
                            let scale = i.scale();
                            let crop = i.crop();
                            (
                                i.id(),
                                i.name().to_string(),
                                i.capture().cloned(),
                                i.is_visible(),
                                pos.x,
                                pos.y,
                                scale.x,
                                scale.y,
                                crop.left,
                                crop.top,
                                crop.right,
                                crop.bottom,
                            )
                        })
                        .collect();

                    for (id, name, capture, visible, px, py, sx, sy, _cl, _ct, _cr, _cb) in items {
                        if !visible {
                            continue;
                        }

                        // Determine source dimensions from typed metadata. Window
                        // capture resolves its size dynamically, so fall back to the
                        // scene output size for it (and for non-capture sources).
                        let (src_w, src_h) = match &capture {
                            Some(c) => match c.native_size() {
                                Some((w, h)) => (w as f32, h as f32),
                                None => (scene_output_w as f32, scene_output_h as f32),
                            },
                            None => (scene_output_w as f32, scene_output_h as f32),
                        };

                        if src_w == 0.0 || src_h == 0.0 {
                            continue;
                        }

                        // Calculate rendered size after scaling
                        let render_w = src_w * sx;
                        let render_h = src_h * sy;

                        // Calculate position on canvas (scene coords -> canvas coords)
                        let item_x = canvas_rect.min.x + px * canvas_scale;
                        let item_y = canvas_rect.min.y + py * canvas_scale;
                        let item_w = render_w * canvas_scale;
                        let item_h = render_h * canvas_scale;

                        let item_rect = egui::Rect::from_min_size(
                            egui::pos2(item_x, item_y),
                            egui::vec2(item_w, item_h),
                        );

                        // Draw source content - look up texture by SceneItemId
                        if let Some(texture) = self.preview.preview_textures.get(&id) {
                            ui.painter().image(
                                texture.id(),
                                item_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        } else {
                            // Placeholder: draw colored rect with source name
                            let color = match &capture {
                                Some(CaptureSource::Display { .. }) => {
                                    egui::Color32::from_rgb(40, 60, 80)
                                }
                                Some(CaptureSource::Webcam { .. }) => {
                                    egui::Color32::from_rgb(50, 70, 50)
                                }
                                Some(CaptureSource::Window { .. }) | None => {
                                    egui::Color32::from_rgb(60, 40, 80)
                                }
                            };
                            ui.painter().rect_filled(item_rect, 2.0, color);
                            ui.painter().text(
                                item_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                &name,
                                egui::FontId::proportional(12.0),
                                egui::Color32::LIGHT_GRAY,
                            );
                        }

                        // Draw selection border
                        ui.painter().rect_stroke(
                            item_rect,
                            2.0,
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(0, 120, 255)),
                        );

                        // Make item draggable (disabled while drawing annotations)
                        if !draw_active {
                            let response = ui.interact(
                                item_rect,
                                ui.make_persistent_id(format!("source_item_{}", id.0 .0)),
                                egui::Sense::drag(),
                            );

                            if response.dragged() {
                                let drag_delta = response.drag_delta();
                                let delta_scene_x = drag_delta.x / canvas_scale;
                                let delta_scene_y = drag_delta.y / canvas_scale;

                                if let Some(scene) = self.scenes.current_scene_mut() {
                                    if let Some(item) = scene.item_mut(id) {
                                        let pos = item.position();
                                        item.set_position(Position::new(
                                            pos.x + delta_scene_x,
                                            pos.y + delta_scene_y,
                                        ));
                                    }
                                }
                            }

                            // Draw resize handles at corners
                            let handle_size = 8.0;
                            let corners = [
                                (item_rect.min, "tl"),
                                (egui::pos2(item_rect.max.x, item_rect.min.y), "tr"),
                                (egui::pos2(item_rect.min.x, item_rect.max.y), "bl"),
                                (item_rect.max, "br"),
                            ];

                            for (corner, corner_name) in corners {
                                let handle_rect = egui::Rect::from_center_size(
                                    corner,
                                    egui::vec2(handle_size, handle_size),
                                );
                                ui.painter()
                                    .rect_filled(handle_rect, 1.0, egui::Color32::WHITE);
                                ui.painter().rect_stroke(
                                    handle_rect,
                                    1.0,
                                    egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 120, 255)),
                                );

                                // Make corner handle draggable for resize
                                let drag_response = ui.interact(
                                    handle_rect,
                                    ui.make_persistent_id(format!(
                                        "resize_{}_{}",
                                        id.0 .0, corner_name
                                    )),
                                    egui::Sense::drag(),
                                );

                                if drag_response.dragged() {
                                    let drag_delta = drag_response.drag_delta();
                                    let delta_w = drag_delta.x / canvas_scale;
                                    let delta_h = drag_delta.y / canvas_scale;

                                    if let Some(scene) = self.scenes.current_scene_mut() {
                                        if let Some(item) = scene.item_mut(id) {
                                            let scale = item.scale();
                                            let new_sx = (scale.x + delta_w / src_w).max(0.01);
                                            let new_sy = (scale.y + delta_h / src_h).max(0.01);
                                            item.set_scale(Scale::new(new_sx, new_sy));
                                        }
                                    }
                                }
                            }
                        }

                        // Draw source label
                        ui.painter().text(
                            egui::pos2(item_rect.min.x, item_rect.min.y - 16.0),
                            egui::Align2::LEFT_BOTTOM,
                            &name,
                            egui::FontId::proportional(11.0),
                            egui::Color32::from_rgb(180, 180, 180),
                        );
                    }
                } else {
                    let center = rect.center();
                    ui.painter().text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "No Active Scene",
                        egui::FontId::proportional(24.0),
                        egui::Color32::GRAY,
                    );
                }

                // Annotation / mark-up overlay and tool interaction
                self.render_annotations(ui, canvas_rect, canvas_scale);

                // Text overlays (rendered on top of everything)
                self.render_text_overlays(ui, canvas_rect, canvas_scale);

                // Show recording indicator
                if self.record.recording {
                    let live_rect = egui::Rect::from_min_size(
                        rect.min + egui::vec2(10.0, 10.0),
                        egui::vec2(60.0, 25.0),
                    );
                    ui.painter().rect_filled(live_rect, 4.0, egui::Color32::RED);
                    ui.painter().text(
                        live_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "REC",
                        egui::FontId::proportional(14.0),
                        egui::Color32::WHITE,
                    );
                }

                // ---- Quick Actions bar (below the preview) ----
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(actions_rect), |ui| {
                    ui.painter().rect_filled(
                        actions_rect,
                        0.0,
                        egui::Color32::from_rgb(35, 35, 35),
                    );
                    ui.painter().hline(
                        actions_rect.min.x..=actions_rect.max.x,
                        actions_rect.min.y,
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 60)),
                    );
                    ui.add_space(7.0);
                    ui.horizontal_centered(|ui| {

                        ui.add_space(10.0);

                        let (rec_icon, rec_label, rec_color) = if !self.record.recording {
                            ("\u{25B6}", "START", egui::Color32::from_rgb(0, 140, 60))
                        } else if self.record.recording_paused {
                            ("\u{25B6}", "RESUME", egui::Color32::from_rgb(0, 140, 60))
                        } else {
                            ("\u{23F8}", "PAUSE", egui::Color32::from_rgb(200, 150, 0))
                        };
                        if Self::quick_action_button(ui, rec_icon, rec_label, rec_color, true) {
                            if !self.record.recording {
                                self.start_recording();
                            } else if self.record.recording_paused {
                                self.record.recording_paused = false;
                                self.log_event("Recording resumed", EventLogKind::Record);
                            } else {
                                self.record.recording_paused = true;
                                self.log_event("Recording paused", EventLogKind::Record);
                            }
                        }

                        // STOP halts whichever encoder is live (recording
                        // and/or the stream; they run as separate FFmpeg
                        // processes and are stopped independently).
                        let stop_color = egui::Color32::from_rgb(180, 0, 0);
                        if Self::quick_action_button(ui, "\u{25A0}", "STOP", stop_color, true)
                            && (self.record.recording || self.streaming)
                        {
                            if self.record.recording {
                                self.stop_recording();
                            }
                            if self.streaming {
                                self.stop_streaming();
                            }
                        }

                        ui.separator();

                        let (str_icon, str_label, str_color) = if !self.streaming {
                            ("\u{1F4E1}", "STREAM", egui::Color32::from_rgb(0, 110, 180))
                        } else if self.streaming_paused {
                            ("\u{25B6}", "RESUME", egui::Color32::from_rgb(0, 110, 180))
                        } else {
                            ("\u{23F8}", "PAUSE", egui::Color32::from_rgb(200, 150, 0))
                        };
                        if Self::quick_action_button(ui, str_icon, str_label, str_color, true) {
                            if !self.streaming {
                                // Spawns the RTMP FFmpeg; on a missing
                                // server/key it logs guidance and stays off.
                                self.start_streaming();
                            } else if self.streaming_paused {
                                self.streaming_paused = false;
                                self.log_event("Streaming resumed", EventLogKind::Stream);
                            } else {
                                self.streaming_paused = true;
                                self.log_event("Streaming paused", EventLogKind::Stream);
                            }
                        }

                        ui.separator();

                        // Snapshot is only meaningful during an active
                        // recording; the shared button greys out otherwise.
                        if Self::quick_action_button(
                            ui,
                            "\u{1F4F7}",
                            "SNAPSHOT",
                            egui::Color32::from_rgb(40, 80, 160),
                            self.record.recording,
                        ) {
                            self.take_snapshot = true;
                        }

                        if Self::quick_action_button(
                            ui,
                            "\u{2691}",
                            "MARK",
                            egui::Color32::from_rgb(180, 110, 0),
                            true,
                        ) {
                            self.log_event(
                                format!("Marker #{} added", self.event_log.len()),
                                EventLogKind::Info,
                            );
                        }

                        if Self::quick_action_button(
                            ui,
                            "\u{1F516}",
                            "BOOKMARK",
                            egui::Color32::from_rgb(120, 80, 160),
                            true,
                        ) {
                            self.log_event("Bookmark added", EventLogKind::Info);
                        }
                    });
                });

                // Snapshot confirmation toast (fades after ~1.5s).
                if let Some(t) = self.snapshot_flash {
                    if t.elapsed() < std::time::Duration::from_millis(1500) {
                        let toast_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.center().x, rect.min.y + 30.0),
                            egui::vec2(190.0, 30.0),
                        );
                        ui.painter()
                            .rect_filled(toast_rect, 4.0, egui::Color32::from_rgb(20, 90, 45));
                        ui.painter().text(
                            toast_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Snapshot saved",
                            egui::FontId::proportional(13.0),
                            egui::Color32::WHITE,
                        );
                    }
                }
            });
        }
    }

    /// Paint one Quick Actions bar button: a fixed 56x46 rounded rect with a
    /// 22pt glyph above a 9pt label — the exact construction the original
    /// START/STOP buttons used, shared by every control in the bar so they
    /// all have identical dimensions. Returns `true` when clicked. With
    /// `enabled == false` the button is greyed out and inert (SNAPSHOT
    /// outside a recording) but keeps the same footprint.
    fn quick_action_button(
        ui: &mut egui::Ui,
        icon: &str,
        label: &str,
        color: egui::Color32,
        enabled: bool,
    ) -> bool {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(56.0, 46.0), egui::Sense::click());
        let (fill, text) = if enabled {
            (
                if resp.hovered() {
                    color.linear_multiply(1.2)
                } else {
                    color
                },
                egui::Color32::WHITE,
            )
        } else {
            (
                egui::Color32::from_rgb(60, 60, 60),
                egui::Color32::from_rgb(150, 150, 150),
            )
        };
        ui.painter().rect_filled(rect, 4.0, fill);
        ui.painter().text(
            egui::pos2(rect.center().x, rect.min.y + 16.0),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(22.0),
            text,
        );
        ui.painter().text(
            egui::pos2(rect.center().x, rect.max.y - 9.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(9.0),
            text,
        );
        if enabled && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        enabled && resp.clicked()
    }
}

//! Source Properties modal (position / scale / rotation / crop editor).
//! Extracted verbatim from `app.rs`.

use super::super::RobsApp;
use eframe::egui;
use robs_core::scene::{Crop, Position, Scale};

impl RobsApp {
    pub(crate) fn source_properties_modal(&mut self, ctx: &egui::Context) {
        if self.editing.show_source_properties {
            egui::Window::new("Source Properties")
                .collapsible(false)
                .resizable(false)
                .default_width(320.0)
                .show(ctx, |ui| {
                    ui.heading(&self.editing.editing_source_name);
                    ui.separator();

                    // Position
                    ui.label("Position");
                    ui.horizontal(|ui| {
                        ui.label("X:");
                        ui.add(egui::DragValue::new(&mut self.editing.editing_source_pos_x).speed(1.0));
                        ui.label("Y:");
                        ui.add(egui::DragValue::new(&mut self.editing.editing_source_pos_y).speed(1.0));
                    });

                    ui.separator();

                    // Scale
                    ui.label("Scale");
                    ui.horizontal(|ui| {
                        ui.label("X:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing.editing_source_scale_x)
                                .speed(0.01)
                                .range(0.01..=10.0),
                        );
                        ui.label("Y:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing.editing_source_scale_y)
                                .speed(0.01)
                                .range(0.01..=10.0),
                        );
                    });

                    ui.separator();

                    // Rotation
                    ui.label("Rotation");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.editing.editing_source_rotation)
                                .speed(1.0)
                                .range(0.0..=360.0),
                        );
                        ui.label("degrees");
                    });

                    ui.separator();

                    // Crop
                    ui.label("Crop (pixels)");
                    ui.horizontal(|ui| {
                        ui.label("L:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing.editing_source_crop_left)
                                .speed(1.0)
                                .range(0..=9999),
                        );
                        ui.label("T:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing.editing_source_crop_top)
                                .speed(1.0)
                                .range(0..=9999),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("R:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing.editing_source_crop_right)
                                .speed(1.0)
                                .range(0..=9999),
                        );
                        ui.label("B:");
                        ui.add(
                            egui::DragValue::new(&mut self.editing.editing_source_crop_bottom)
                                .speed(1.0)
                                .range(0..=9999),
                        );
                    });

                    ui.separator();

                    // Apply / Cancel
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            if let Some(id) = self.editing.editing_source_id {
                                // Copy the edit fields out first: `scenes` and
                                // `editing` are both reached through the
                                // controller deref, so the `&mut` scene borrow
                                // would otherwise block reading the fields.
                                let pos_x = self.editing.editing_source_pos_x;
                                let pos_y = self.editing.editing_source_pos_y;
                                let scale_x = self.editing.editing_source_scale_x;
                                let scale_y = self.editing.editing_source_scale_y;
                                let rotation = self.editing.editing_source_rotation;
                                let crop_left = self.editing.editing_source_crop_left;
                                let crop_top = self.editing.editing_source_crop_top;
                                let crop_right = self.editing.editing_source_crop_right;
                                let crop_bottom = self.editing.editing_source_crop_bottom;
                                if let Some(scene) = self.scenes.current_scene_mut() {
                                    if let Some(item) = scene.item_mut(id) {
                                        item.set_position(Position::new(pos_x, pos_y));
                                        item.set_scale(Scale::new(scale_x, scale_y));
                                        item.set_rotation(rotation);
                                        item.set_crop(Crop::new(
                                            crop_left, crop_top, crop_right, crop_bottom,
                                        ));
                                    }
                                }
                            }
                            self.editing.show_source_properties = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.editing.show_source_properties = false;
                        }
                    });
                });
        }
    }
}

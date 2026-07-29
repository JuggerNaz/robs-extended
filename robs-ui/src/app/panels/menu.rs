//! Top menu bar (File / Edit / View / Profile / Help). Extracted verbatim
//! from `app.rs`.

use super::super::RobsApp;
use eframe::egui;

impl RobsApp {
    pub(crate) fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New Profile").clicked() {
                        let id = self.profile_manager.write().create("New Profile".into());
                        self.profile_manager.write().set_current(id).ok();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Settings").clicked() {
                        self.show_settings = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Undo").clicked() {
                        ui.close_menu();
                    }
                    if ui.button("Redo").clicked() {
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.show_preview, "Preview");
                    ui.checkbox(&mut self.show_scenes, "Scenes");
                    ui.checkbox(&mut self.show_controls, "Controls");
                    ui.checkbox(&mut self.show_audio, "Audio Mixer");
                    ui.checkbox(&mut self.show_chat, "Chat");
                    ui.checkbox(&mut self.show_stats, "Stats");
                    ui.checkbox(&mut self.show_event_log, "Event Log");
                    ui.separator();
                    ui.checkbox(&mut self.annotation.show_annotations, "Annotations Toolbar");
                });
                ui.menu_button("Profile", |ui| {
                    let profiles = self.profile_manager.read().list();
                    for (id, name) in profiles {
                        if ui.button(&name).clicked() {
                            self.profile_manager.write().set_current(id).ok();
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About ROBS").clicked() {
                        ui.close_menu();
                    }
                });
            });
            ui.add_space(4.0);
        });
    }
}

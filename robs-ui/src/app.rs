//! Root of the `app` module tree.
//!
//! [`RobsApp`] is the single `eframe` state object — since the engine-core
//! extraction it is a thin *view* over [`RobsController`] (in the
//! `robs-controller` crate), which owns all engine state (scenes, capture,
//! recording, streaming, blackbox, anomaly, annotations) and is driven once
//! per frame from `update()` via [`RobsController::tick`]. [`RobsApp`] derefs
//! to the controller, so panel code keeps resolving engine methods and fields
//! unchanged; only UI-toolkit-only state (the GPU texture cache, window and
//! layout toggles, text inputs, and the settings UI) remains on [`RobsApp`].
//!
//! Panel renderers live in the sibling submodules under `app/` and are marked
//! `pub(crate)` when invoked across module boundaries (from `update()` here
//! or from sibling submodules), since Rust only lets a module call another
//! module's *private* items when the caller is a descendant of the defining
//! module. Private helpers that are only used within their own submodule
//! stay private.

mod annotations;
mod anomaly;
mod blackbox;
mod panels;
mod settings;

use eframe::egui;
use parking_lot::RwLock;
use robs_chat::aggregator::ChatAggregator;
use robs_chat::message::ChatEvent;
use robs_controller::state::{AudioChannel, AudioDeviceInfo};
use robs_controller::{devices, RobsController};
use robs_core::SceneItemId;
use robs_profiles::profile::ProfileManager;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct RobsApp {
    // Engine core: scenes, capture, record/stream pipelines, blackbox,
    // anomaly, annotations — driven by `controller.tick()` once per frame.
    controller: RobsController,

    show_event_log: bool,
    profile_manager: Arc<RwLock<ProfileManager>>,
    chat_input: String,
    current_scene: String,
    show_settings: bool,
    #[allow(dead_code)]
    settings_rect: Option<egui::Rect>,
    active_settings_tab: usize,
    audio_channels: Vec<AudioChannel>,
    show_preview: bool,
    show_scenes: bool,
    show_controls: bool,
    show_audio: bool,
    show_chat: bool,
    show_stats: bool,
    confirm_on_exit: bool,
    minimize_to_tray: bool,
    always_on_top: bool,
    check_for_updates: bool,
    filename_formatting: String,
    base_width: u32,
    base_height: u32,
    audio_sample_rate: String,  // "44100" or "48000"
    audio_channel_mode: String, // "mono" or "stereo"
    // Audio devices
    audio_devices: Vec<AudioDeviceInfo>,
    #[allow(dead_code)]
    selected_audio_device: String,
    overlay_text_input: String,
    // View-side GPU texture cache mirroring the controller's latest preview
    // frames (`controller.preview.preview_frames`), keyed by scene-item ID.
    // `preview_texture_versions` remembers which frame version each texture
    // was built from so unchanged frames are never re-uploaded.
    preview_textures: HashMap<SceneItemId, egui::TextureHandle>,
    preview_texture_versions: HashMap<SceneItemId, u64>,
}

impl RobsApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
        });

        Self {
            // Engine core (encoder detection, scenes, stream/record defaults,
            // blackbox/anomaly event buses) initializes in the controller.
            controller: RobsController::new(),
            show_event_log: true,
            profile_manager: Arc::new(RwLock::new(ProfileManager::default())),
            chat_input: String::new(),
            current_scene: "Main Scene".to_string(),
            show_settings: false,
            settings_rect: None,
            active_settings_tab: 0,
            audio_channels: vec![
                AudioChannel {
                    name: "Mic/Aux".into(),
                    volume: 0.8,
                    muted: false,
                    device_id: "default".to_string(),
                    is_desktop: false,
                },
                AudioChannel {
                    name: "Desktop Audio".into(),
                    volume: 0.6,
                    muted: false,
                    device_id: "default".to_string(),
                    is_desktop: true,
                },
            ],
            show_preview: true,
            show_scenes: true,
            show_controls: true,
            show_audio: true,
            show_chat: true,
            show_stats: true,
            confirm_on_exit: true,
            minimize_to_tray: false,
            always_on_top: false,
            check_for_updates: true,
            filename_formatting: "%CCYY-%MM-%DD %hh-%mm-%ss".into(),
            base_width: 1920,
            base_height: 1080,
            audio_sample_rate: "48000".to_string(),
            audio_channel_mode: "stereo".to_string(),
            // Audio devices
            audio_devices: devices::get_audio_devices(),
            selected_audio_device: "0".to_string(),
            overlay_text_input: String::new(),
            preview_textures: HashMap::new(),
            preview_texture_versions: HashMap::new(),
        }
    }

    /// Engine variant wired to a live chat aggregator (see `robs` main).
    pub fn with_chat(
        mut self,
        _aggregator: Arc<ChatAggregator>,
        rx: mpsc::Receiver<ChatEvent>,
    ) -> Self {
        self.controller = self.controller.with_chat(_aggregator, rx);
        self
    }

    /// Mirror the controller's latest preview frames into GPU textures. The
    /// controller versions every frame it captures, so each frame is uploaded
    /// exactly once regardless of how many repaints happen between captures;
    /// textures for scene items that no longer produce frames are dropped.
    fn sync_preview_textures(&mut self, ctx: &egui::Context) {
        for (id, frame) in &self.controller.preview.preview_frames {
            if self.preview_texture_versions.get(id) == Some(&frame.version) {
                continue;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.data,
            );
            let texture = ctx.load_texture(
                format!("preview_{:?}", id),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.preview_textures.insert(*id, texture);
            self.preview_texture_versions.insert(*id, frame.version);
        }
        // Drop textures for scene items that vanished from the controller.
        let active_ids = &self.controller.preview.preview_frames;
        self.preview_textures.retain(|id, _| active_ids.contains_key(id));
        self.preview_texture_versions
            .retain(|id, _| active_ids.contains_key(id));
    }
}

impl std::ops::Deref for RobsApp {
    type Target = RobsController;

    fn deref(&self) -> &Self::Target {
        &self.controller
    }
}

impl std::ops::DerefMut for RobsApp {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.controller
    }
}

impl eframe::App for RobsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drive the engine first: event drains, elapsed timers, capture
        // lifecycle, and the returned wake hint (the engine's former direct
        // repaint requests, now handed back to the view).
        if let Some(wake) = self.controller.tick() {
            ctx.request_repaint_after(wake);
        }

        // Upload freshly captured preview frames as GPU textures before the
        // panels render them.
        self.sync_preview_textures(ctx);

        self.menu_bar(ctx);
        self.annotation_toolbar(ctx);
        self.streaming_controls(ctx);
        self.scenes_panel(ctx);
        self.source_properties_modal(ctx);
        self.right_panel(ctx);
        self.preview_panel(ctx);

        if self.show_settings {
            self.show_settings_window(ctx);
        }
    }
}

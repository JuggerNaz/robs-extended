//! Batch B glue: everything behind the `PanelsApi` global — the audio
//! mixer, chat, stats, the event-log strip actions, the menu bar (profiles,
//! view toggles), the settings window, and the blackbox / anomaly controls.
//! Ports `robs-ui/src/app/panels/right.rs`, `menu.rs`, and `settings.rs`.
//!
//! Settings plumbing: every editable setting lives in a two-way-bound
//! `PanelsApi` property. The glue seeds the properties from the engine
//! (once at startup and on every open), and each tick detects *drift* —
//! a property that differs from its backing value means the user edited
//! it, so the new value is applied to the engine. Paths that only change
//! through Browse dialogs are read-only properties pushed when the engine
//! value moves.
//!
//! Borrow discipline: the native save/folder dialogs run a modal message
//! loop, during which the tick timer can fire on this same thread — no
//! `RefMut` of the controller may be held across them. Data is copied out,
//! the borrow is dropped, the dialog runs, and a fresh borrow applies the
//! result.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::RwLock;
use robs_chat::message::UnifiedChatMessage;
use robs_controller::devices::get_audio_devices;
use robs_controller::state::{AudioChannel, AudioDeviceInfo, EventLogKind};
use robs_controller::RobsController;
use robs_core::ProfileId;
use robs_profiles::profile::ProfileManager;
use slint::{Color, ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{AudioChannelView, ChatMessageView, MainWindow, PanelsApi};

/// Chat rows kept in the UI (the egui panel scrolls the whole backlog; the
/// Slint list caps at the same 100-row budget as the event log).
const CHAT_ROWS: usize = 100;

const ENCODER_SW: &str = "libx264";
const ENCODER_HW: &str = "h264_nvenc";

/// UI-side state for the panels: the audio mixer channels (UI-owned, like
/// the egui `RobsApp`), the enumerated device list, the profile manager,
/// push mirrors, and the preferences with no engine backing (they live only
/// here, mirroring the egui inert fields; the seed writes them into their
/// properties so edits survive closing and reopening the window).
pub struct PanelsUi {
    audio_channels: Vec<AudioChannel>,
    audio_devices: Vec<AudioDeviceInfo>,
    profile_manager: Arc<RwLock<ProfileManager>>,
    /// Snapshot behind the `profile-names` model; `select-profile` indexes
    /// into this (names were sorted at push time).
    profiles_snapshot: Vec<(ProfileId, String)>,
    profiles_mirror: Vec<String>,
    /// `(volume bits, muted)` per channel at the last audio-model push.
    audio_mirror: Vec<(u32, bool)>,
    /// `chat_messages.len()` at the last chat-model push.
    chat_len: usize,
    seeded: bool,
    pref_confirm_on_exit: bool,
    pref_minimize_to_tray: bool,
    pref_always_on_top: bool,
    pref_check_for_updates: bool,
    pref_filename_formatting: String,
    pref_base_width: i32,
    pref_base_height: i32,
    pref_audio_sample_rate: String,  // "44100" | "48000"
    pref_audio_channel_mode: String, // "mono" | "stereo"
}

impl PanelsUi {
    pub fn new() -> Self {
        Self {
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
            audio_devices: get_audio_devices(),
            profile_manager: Arc::new(RwLock::new(ProfileManager::default())),
            profiles_snapshot: Vec::new(),
            profiles_mirror: Vec::new(),
            audio_mirror: Vec::new(),
            chat_len: 0,
            seeded: false,
            pref_confirm_on_exit: true,
            pref_minimize_to_tray: false,
            pref_always_on_top: false,
            pref_check_for_updates: true,
            pref_filename_formatting: "%CCYY-%MM-%DD %hh-%mm-%ss".into(),
            pref_base_width: 1920,
            pref_base_height: 1080,
            pref_audio_sample_rate: "48000".to_string(),
            pref_audio_channel_mode: "stereo".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

/// Position of `device_id` in the device list (Default's slot as fallback).
fn device_index(devices: &[AudioDeviceInfo], device_id: &str) -> i32 {
    devices
        .iter()
        .position(|d| d.id == device_id)
        .map_or(1, |i| i as i32)
}

/// Device id at `index`, Default when out of bounds.
fn device_id_at(devices: &[AudioDeviceInfo], index: i32) -> String {
    devices
        .get(index.max(0) as usize)
        .map_or_else(|| "default".to_string(), |d| d.id.clone())
}

fn encoder_index(encoder: &str) -> i32 {
    if encoder == ENCODER_HW { 1 } else { 0 }
}

fn encoder_at(index: i32) -> &'static str {
    if index == 1 { ENCODER_HW } else { ENCODER_SW }
}

/// `#RRGGBB` -> Slint color; `None` on malformed input.
fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().map(|v| {
        Color::from_rgb_u8(((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8)
    })
}

/// Chat author color: the user's Twitch-style hex, else the platform's
/// brand color, else white (the egui fallback chain).
fn chat_color(msg: &UnifiedChatMessage) -> Color {
    msg.user
        .color
        .as_deref()
        .and_then(parse_hex_color)
        .or_else(|| parse_hex_color(msg.platform.color_hex()))
        .unwrap_or_else(|| Color::from_rgb_u8(255, 255, 255))
}

/// The mic/aux or desktop channel position, by its `is_desktop` flag.
fn channel_position(channels: &[AudioChannel], is_desktop: bool) -> usize {
    channels
        .iter()
        .position(|c| c.is_desktop == is_desktop)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Install: commands + static models
// ---------------------------------------------------------------------------

/// Wire every `PanelsApi` callback and the models that never change. The
/// per-tick models (audio, chat, profiles, stats, statuses) are maintained
/// by [`push`].
pub fn install(
    component: &slint::Weak<MainWindow>,
    controller: &Rc<RefCell<RobsController>>,
    state: &Rc<RefCell<PanelsUi>>,
) {
    // ---- Static models + encoder availability ----
    if let Some(root) = component.upgrade() {
        let api = root.global::<PanelsApi>();
        let devices: Vec<SharedString> = state
            .borrow()
            .audio_devices
            .iter()
            .map(|d| d.name.as_str().into())
            .collect();
        api.set_audio_device_labels(ModelRc::new(VecModel::from(devices)));
        let c = controller.borrow();
        let video: Vec<SharedString> = c
            .available_video_encoders
            .iter()
            .map(|s| s.as_str().into())
            .collect();
        api.set_video_encoders(ModelRc::new(VecModel::from(video)));
        let audio: Vec<SharedString> = c
            .available_audio_encoders
            .iter()
            .map(|s| s.as_str().into())
            .collect();
        api.set_audio_encoders(ModelRc::new(VecModel::from(audio)));
        api.set_ffmpeg_ok(c.ffmpeg_available);
        api.set_nvenc_ok(c.nvenc_available);
        api.set_aac_ok(c.aac_available);
        // View toggles start from the egui defaults (all on).
        api.set_show_preview(true);
        api.set_show_scenes(true);
        api.set_show_controls(true);
        api.set_show_audio(true);
        api.set_show_chat(true);
        api.set_show_stats(true);
        api.set_show_event_log(true);
        api.set_show_annotations(c.annotation.show_annotations);
    }

    // Handlers that read/write `PanelsApi` properties re-acquire the global
    // from the window inside the handler (a `'static` requirement — the
    // `api` reference itself can't be captured).
    let Some(root) = component.upgrade() else {
        return;
    };
    let api = root.global::<PanelsApi>();

    // ---- Audio mixer ----
    {
        let state = Rc::clone(state);
        api.on_set_audio_volume(move |index: i32, volume: f32| {
            if let Some(ch) = state.borrow_mut().audio_channels.get_mut(index.max(0) as usize) {
                ch.volume = volume.clamp(0.0, 1.0);
            }
        });
    }
    {
        let state = Rc::clone(state);
        api.on_set_audio_muted(move |index: i32, muted: bool| {
            if let Some(ch) = state.borrow_mut().audio_channels.get_mut(index.max(0) as usize) {
                ch.muted = muted;
            }
        });
    }

    // ---- Event log strip ----
    {
        let controller = Rc::clone(controller);
        api.on_clear_log(move || {
            controller.borrow_mut().event_log.clear();
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_export_log_pdf(move || {
            export_event_log_pdf(&controller);
        });
    }

    // ---- Settings window ----
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        api.on_open_settings(move || {
            let Some(root) = weak.upgrade() else { return };
            let api = root.global::<PanelsApi>();
            // Re-seed on every open so the widgets show the live engine
            // values (the egui window was rebuilt from state each frame).
            let st = state.borrow();
            seed_settings(&api, &controller.borrow(), &st);
            api.set_settings_open(true);
        });
    }
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        api.on_settings_closed(move || {
            // Closing is the settings-commit gesture (persists the anomaly
            // section of the settings file, like the egui close handler).
            controller.borrow_mut().save_anomaly_settings();
            if let Some(root) = weak.upgrade() {
                root.global::<PanelsApi>().set_settings_open(false);
            }
        });
    }

    // ---- Folder pickers (no controller borrow across the native dialog) ----
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        api.on_browse_recording_path(move || {
            let Some(dir) = rfd::FileDialog::new().pick_folder() else { return };
            let dir = dir.to_string_lossy().into_owned();
            controller.borrow_mut().recording_path = dir.clone();
            if let Some(root) = weak.upgrade() {
                root.global::<PanelsApi>().set_cfg_recording_path(dir.into());
            }
        });
    }
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        api.on_browse_bb_dir(move || {
            let Some(dir) = rfd::FileDialog::new().pick_folder() else { return };
            let dir = dir.to_string_lossy().into_owned();
            controller.borrow_mut().blackbox.settings.output_dir = dir.clone();
            if let Some(root) = weak.upgrade() {
                root.global::<PanelsApi>().set_cfg_bb_output_dir(dir.into());
            }
        });
    }
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        api.on_browse_an_dir(move || {
            let Some(dir) = rfd::FileDialog::new().pick_folder() else { return };
            let dir = dir.to_string_lossy().into_owned();
            controller.borrow_mut().anomaly.settings.output_dir = dir.clone();
            if let Some(root) = weak.upgrade() {
                root.global::<PanelsApi>().set_cfg_an_output_dir(dir.into());
            }
        });
    }

    // ---- Blackbox / Anomaly ----
    {
        let controller = Rc::clone(controller);
        api.on_restart_blackbox(move || {
            controller.borrow_mut().stop_blackbox();
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_toggle_anomaly_engine(move || {
            let mut c = controller.borrow_mut();
            let running = c.anomaly.engine.as_ref().is_some_and(|e| e.is_running());
            if running {
                c.stop_anomaly();
            } else {
                c.start_anomaly();
            }
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_capture_anomaly_clip(move || {
            controller.borrow_mut().request_anomaly_clip();
        });
    }

    // ---- Profiles / menu ----
    {
        let state = Rc::clone(state);
        api.on_new_profile(move || {
            let st = state.borrow();
            let mut pm = st.profile_manager.write();
            let id = pm.create("New Profile".into());
            pm.set_current(id).ok();
        });
    }
    {
        let state = Rc::clone(state);
        api.on_select_profile(move |index: i32| {
            let st = state.borrow();
            let Some((id, _)) = st.profiles_snapshot.get(index.max(0) as usize) else {
                return;
            };
            let id = id.clone();
            st.profile_manager.write().set_current(id).ok();
        });
    }
    api.on_exit_app(move || {
        let _ = slint::quit_event_loop();
    });
    // `undo`, `redo`, `about`, and `send-chat` are no-ops, exactly like
    // their egui counterparts; no registration is needed.
}

/// Port of `RobsApp::export_event_log_pdf`. Entries are copied out and the
/// controller borrow dropped before the modal save dialog opens (the tick
/// timer runs on this thread and borrows the controller).
fn export_event_log_pdf(controller: &Rc<RefCell<RobsController>>) {
    let entries: Vec<(String, &'static str, String)> = controller
        .borrow()
        .event_log
        .iter()
        .map(|e| {
            let kind = match e.kind {
                EventLogKind::Stream => "Stream",
                EventLogKind::Record => "Record",
                EventLogKind::Annotation => "Annotation",
                EventLogKind::Overlay => "Overlay",
                EventLogKind::Info => "Info",
            };
            (
                e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                kind,
                e.message.clone(),
            )
        })
        .collect();
    if entries.is_empty() {
        controller
            .borrow_mut()
            .log_event("Event log is empty - nothing to export", EventLogKind::Info);
        return;
    }

    let default_name = format!(
        "robs_event_log_{}.pdf",
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
    );
    let start_dir = robs_controller::user_home().unwrap_or_default();
    let Some(path) = rfd::FileDialog::new()
        .set_directory(start_dir)
        .set_file_name(default_name)
        .add_filter("PDF report", &["pdf"])
        .save_file()
    else {
        return; // user cancelled the dialog
    };

    // The log is stored newest-last in the VecDeque but displayed
    // newest-first; a report reads naturally oldest-first, so the collected
    // order is already chronological.
    let mut report =
        robs_outputs::Report::new("ROBS Event Log Report", "Session event log export");
    report.push_meta(
        "Generated",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    );
    report.push_meta("Entries", entries.len().to_string());
    report.push_meta("Application", format!("ROBS {}", robs_core::ROBS_VERSION));
    for (timestamp, kind, message) in &entries {
        report.push_line(format!("{timestamp}  [{kind:<10}]  {message}"));
    }

    let outcome = robs_outputs::report::write_pdf(&report, &path);
    let mut c = controller.borrow_mut();
    match outcome {
        Ok(()) => c.log_event(
            format!("Event log exported to {}", path.display()),
            EventLogKind::Info,
        ),
        Err(e) => c.log_event(format!("PDF export failed: {e}"), EventLogKind::Info),
    }
}

// ---------------------------------------------------------------------------
// Seed: engine values -> settings properties
// ---------------------------------------------------------------------------

/// Write every settings property from the engine and the UI-side preference
/// store. Called on the first tick and every time the settings window opens.
fn seed_settings(api: &PanelsApi, controller: &RobsController, state: &PanelsUi) {
    // General: UI-side preferences (re-applied so edits survive close/open).
    api.set_cfg_confirm_on_exit(state.pref_confirm_on_exit);
    api.set_cfg_minimize_to_tray(state.pref_minimize_to_tray);
    api.set_cfg_always_on_top(state.pref_always_on_top);
    api.set_cfg_check_for_updates(state.pref_check_for_updates);
    api.set_cfg_filename_formatting(state.pref_filename_formatting.as_str().into());
    api.set_cfg_base_width(state.pref_base_width);
    api.set_cfg_base_height(state.pref_base_height);
    api.set_cfg_sample_rate_index(if state.pref_audio_sample_rate == "44100" { 0 } else { 1 });
    api.set_cfg_channel_mode_index(if state.pref_audio_channel_mode == "mono" { 0 } else { 1 });
    let desktop_pos = channel_position(&state.audio_channels, true);
    let mic_pos = channel_position(&state.audio_channels, false);
    api.set_cfg_desktop_device_index(device_index(
        &state.audio_devices,
        &state.audio_channels[desktop_pos].device_id,
    ));
    api.set_cfg_mic_device_index(device_index(
        &state.audio_devices,
        &state.audio_channels[mic_pos].device_id,
    ));

    // Video.
    api.set_cfg_output_width(controller.output_width as i32);
    api.set_cfg_output_height(controller.output_height as i32);
    api.set_cfg_fps(controller.fps_setting);

    // Streaming (service first: the ComboBox's selected() handler rewrites
    // the server property from the service preset, so the real server value
    // must land after it).
    api.set_cfg_stream_service(controller.stream_service.as_str().into());
    api.set_cfg_stream_server(controller.stream_server.as_str().into());
    api.set_cfg_stream_key(controller.stream_key.as_str().into());
    api.set_cfg_video_encoder(controller.video_encoder.as_str().into());
    api.set_cfg_audio_encoder(controller.audio_encoder.as_str().into());
    api.set_cfg_stream_bitrate(controller.stream_bitrate as f32);
    api.set_cfg_keyframe_interval(controller.keyframe_interval as f32);

    // Outputs.
    api.set_cfg_recording_format(controller.recording_format.as_str().into());
    api.set_cfg_recording_bitrate(controller.recording_bitrate as f32);
    api.set_cfg_recording_path(controller.recording_path.as_str().into());

    // Blackbox.
    let bb = &controller.blackbox;
    api.set_cfg_bb_enabled(bb.enabled);
    api.set_cfg_bb_encoder_index(encoder_index(&bb.settings.encoder));
    api.set_cfg_bb_output_dir(bb.settings.output_dir.as_str().into());
    api.set_cfg_bb_segment_secs(bb.settings.segment_duration_secs as f32);
    api.set_cfg_bb_segment_mb(bb.settings.segment_size_mb as f32);
    api.set_cfg_bb_crf(bb.settings.crf as f32);
    api.set_cfg_bb_bitrate(bb.settings.video_bitrate_kbps as f32);
    api.set_cfg_bb_disk_warn(bb.settings.disk_low_warn_percent as f32);
    api.set_cfg_bb_disk_critical(bb.settings.disk_low_critical_percent as f32);
    api.set_cfg_bb_retention(bb.settings.max_retention_gb as f32);
    api.set_cfg_bb_stall(bb.settings.stall_threshold_secs as f32);

    // Anomaly.
    let an = &controller.anomaly;
    api.set_cfg_an_encoder_index(encoder_index(&an.settings.encoder));
    api.set_cfg_an_output_dir(an.settings.output_dir.as_str().into());
    api.set_cfg_an_pre_roll(an.settings.pre_roll_secs as f32);
    api.set_cfg_an_post_roll(an.settings.post_roll_secs as f32);
    api.set_cfg_an_segment_secs(an.settings.segment_duration_secs as f32);
    api.set_cfg_an_max_buffer(an.settings.max_buffer_mb as f32);
    api.set_cfg_an_crf(an.settings.crf as f32);
    api.set_cfg_an_bitrate(an.settings.video_bitrate_kbps as f32);
    api.set_cfg_an_clip_prefix(an.settings.clip_prefix.as_str().into());
    api.set_cfg_an_clip_suffix(an.settings.clip_suffix.as_str().into());
    api.set_cfg_an_hotkey(an.settings.capture_clip_hotkey.as_str().into());
}

// ---------------------------------------------------------------------------
// Drift: settings properties -> engine values
// ---------------------------------------------------------------------------

/// Per-tick drift sync. A property that differs from its backing value was
/// edited by the user, so it is applied. When the settings window is closed
/// every property equals its backing value and this is a no-op sweep.
fn apply_settings_drift(
    api: &PanelsApi,
    controller: &mut RobsController,
    state: &mut PanelsUi,
) {
    // ---- General (UI-side; the mirror is the storage) ----
    state.pref_confirm_on_exit = api.get_cfg_confirm_on_exit();
    state.pref_minimize_to_tray = api.get_cfg_minimize_to_tray();
    state.pref_always_on_top = api.get_cfg_always_on_top();
    state.pref_check_for_updates = api.get_cfg_check_for_updates();
    state.pref_filename_formatting = api.get_cfg_filename_formatting().to_string();
    state.pref_base_width = api.get_cfg_base_width();
    state.pref_base_height = api.get_cfg_base_height();

    // ---- Video ----
    controller.output_width = api.get_cfg_output_width().max(0) as u32;
    controller.output_height = api.get_cfg_output_height().max(0) as u32;
    controller.fps_setting = api.get_cfg_fps();

    // ---- Audio (indexes -> strings / device ids) ----
    state.pref_audio_sample_rate = if api.get_cfg_sample_rate_index() == 0 {
        "44100".into()
    } else {
        "48000".into()
    };
    state.pref_audio_channel_mode = if api.get_cfg_channel_mode_index() == 0 {
        "mono".into()
    } else {
        "stereo".into()
    };
    let desktop_pos = channel_position(&state.audio_channels, true);
    let mic_pos = channel_position(&state.audio_channels, false);
    let desktop_id =
        device_id_at(&state.audio_devices, api.get_cfg_desktop_device_index());
    let mic_id = device_id_at(&state.audio_devices, api.get_cfg_mic_device_index());
    if let Some(ch) = state.audio_channels.get_mut(desktop_pos) {
        ch.device_id = desktop_id;
    }
    if let Some(ch) = state.audio_channels.get_mut(mic_pos) {
        ch.device_id = mic_id;
    }

    // ---- Streaming ----
    controller.stream_service = api.get_cfg_stream_service().to_string();
    controller.stream_server = api.get_cfg_stream_server().to_string();
    controller.stream_key = api.get_cfg_stream_key().to_string();
    controller.video_encoder = api.get_cfg_video_encoder().to_string();
    controller.audio_encoder = api.get_cfg_audio_encoder().to_string();
    controller.stream_bitrate = api.get_cfg_stream_bitrate().max(0.0) as u32;
    controller.keyframe_interval = api.get_cfg_keyframe_interval().max(0.0) as u32;

    // ---- Outputs ----
    controller.recording_format = api.get_cfg_recording_format().to_string();
    controller.recording_bitrate = api.get_cfg_recording_bitrate().max(0.0) as u32;

    // ---- Blackbox (the checkbox mirrors into settings, egui parity) ----
    let bb = &mut controller.blackbox;
    bb.enabled = api.get_cfg_bb_enabled();
    bb.settings.enabled = bb.enabled;
    bb.settings.encoder = encoder_at(api.get_cfg_bb_encoder_index()).to_string();
    bb.settings.segment_duration_secs = api.get_cfg_bb_segment_secs().max(0.0) as u64;
    bb.settings.segment_size_mb = api.get_cfg_bb_segment_mb().max(0.0) as u64;
    bb.settings.crf = api.get_cfg_bb_crf().clamp(0.0, 51.0) as u8;
    bb.settings.video_bitrate_kbps = api.get_cfg_bb_bitrate().max(0.0) as u32;
    bb.settings.disk_low_warn_percent = api.get_cfg_bb_disk_warn().clamp(0.0, 100.0) as u8;
    bb.settings.disk_low_critical_percent =
        api.get_cfg_bb_disk_critical().clamp(0.0, 100.0) as u8;
    bb.settings.max_retention_gb = api.get_cfg_bb_retention().max(0.0) as u32;
    bb.settings.stall_threshold_secs = api.get_cfg_bb_stall().max(0.0) as u64;

    // ---- Anomaly ----
    let an = &mut controller.anomaly;
    an.settings.encoder = encoder_at(api.get_cfg_an_encoder_index()).to_string();
    an.settings.pre_roll_secs = api.get_cfg_an_pre_roll().max(0.0) as u32;
    an.settings.post_roll_secs = api.get_cfg_an_post_roll().max(0.0) as u32;
    an.settings.segment_duration_secs = api.get_cfg_an_segment_secs().max(1.0) as u32;
    an.settings.max_buffer_mb = api.get_cfg_an_max_buffer().max(0.0) as u32;
    an.settings.crf = api.get_cfg_an_crf().clamp(0.0, 51.0) as u8;
    an.settings.video_bitrate_kbps = api.get_cfg_an_bitrate().max(0.0) as u32;
    an.settings.clip_prefix = api.get_cfg_an_clip_prefix().to_string();
    an.settings.clip_suffix = api.get_cfg_an_clip_suffix().to_string();
    an.settings.capture_clip_hotkey = api.get_cfg_an_hotkey().to_string();
}

// ---------------------------------------------------------------------------
// Push: per-tick models and statuses
// ---------------------------------------------------------------------------

/// Per-tick maintenance: seeds the settings once, applies drift, refreshes
/// the audio / chat / profile models when their source data changed, and
/// updates the stats + blackbox / anomaly status snapshots.
pub fn push(component: &MainWindow, controller: &mut RobsController, state: &mut PanelsUi) {
    let api = component.global::<PanelsApi>();

    if !state.seeded {
        state.seeded = true;
        seed_settings(&api, controller, state);
    }

    apply_settings_drift(&api, controller, state);
    push_audio(&api, state);
    push_chat(&api, controller, state);
    push_profiles(&api, state);
    push_status(&api, controller);

    // Read-only path properties follow the engine (the Browse callbacks
    // already wrote the property, so this only fires on external moves).
    let path = controller.recording_path.clone();
    if api.get_cfg_recording_path().as_str() != path.as_str() {
        api.set_cfg_recording_path(path.as_str().into());
    }
    let dir = controller.blackbox.settings.output_dir.clone();
    if api.get_cfg_bb_output_dir().as_str() != dir.as_str() {
        api.set_cfg_bb_output_dir(dir.as_str().into());
    }
    let dir = controller.anomaly.settings.output_dir.clone();
    if api.get_cfg_an_output_dir().as_str() != dir.as_str() {
        api.set_cfg_an_output_dir(dir.as_str().into());
    }
}

/// Rebuild the mixer model when a channel's (volume, muted) changed.
fn push_audio(api: &PanelsApi, state: &mut PanelsUi) {
    let sig: Vec<(u32, bool)> = state
        .audio_channels
        .iter()
        .map(|c| (c.volume.to_bits(), c.muted))
        .collect();
    if sig == state.audio_mirror {
        return;
    }
    state.audio_mirror = sig;
    let rows: Vec<AudioChannelView> = state
        .audio_channels
        .iter()
        .enumerate()
        .map(|(index, ch)| AudioChannelView {
            index: index as i32,
            name: ch.name.as_str().into(),
            volume: ch.volume,
            muted: ch.muted,
            pct: format!("{:.0}%", ch.volume * 100.0).into(),
        })
        .collect();
    api.set_audio_channels(ModelRc::new(VecModel::from(rows)));
}

/// Append the chat tail when new messages arrived (the mock generator and
/// the platform aggregators both feed this queue).
fn push_chat(api: &PanelsApi, controller: &RobsController, state: &mut PanelsUi) {
    let messages = controller.chat_messages.read();
    if messages.len() == state.chat_len {
        return;
    }
    let rows: Vec<ChatMessageView> = messages
        .iter()
        .rev()
        .take(CHAT_ROWS)
        .rev()
        .map(|msg| ChatMessageView {
            author: msg.user.display_name.as_str().into(),
            color: chat_color(msg),
            text: msg.content.as_str().into(),
        })
        .collect();
    api.set_chat_messages(ModelRc::new(VecModel::from(rows)));
    state.chat_len = messages.len();
}

/// Refresh the profile menu when the manager's profile set changed; names
/// are sorted so the snapshot indexes stay stable across pushes.
fn push_profiles(api: &PanelsApi, state: &mut PanelsUi) {
    let mut profiles = state.profile_manager.read().list();
    profiles.sort_by(|a, b| a.1.cmp(&b.1));
    let names: Vec<String> = profiles.iter().map(|(_, name)| name.clone()).collect();
    if names == state.profiles_mirror {
        return;
    }
    state.profiles_snapshot = profiles;
    state.profiles_mirror = names.clone();
    api.set_profile_names(ModelRc::new(VecModel::from(
        names.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )));
}

/// Stats strings and the blackbox / anomaly status chips (egui parity for
/// text and the good/warn/neutral coloring).
fn push_status(api: &PanelsApi, controller: &RobsController) {
    api.set_stats_streaming(if controller.streaming { "Active" } else { "Inactive" }.into());
    api.set_stats_recording(
        if controller.record.recording { "Active" } else { "Inactive" }.into(),
    );
    api.set_stats_duration(if controller.streaming {
        RobsController::format_time(controller.streaming_time / 1000).into()
    } else {
        "".into()
    });
    api.set_stats_fps(format!("{:.1} fps", controller.fps).into());
    api.set_stats_bitrate(format!("{} kbps", controller.bitrate).into());
    api.set_stats_dropped(format!("{}", controller.dropped_frames).into());
    api.set_log_label(format!("EVENT LOG ({})", controller.event_log.len()).into());

    // Blackbox chip: disabled / idle / disk-paused (warn) / recording (good).
    let bb = &controller.blackbox;
    let bb_running = bb.engine.as_ref().is_some_and(|e| e.is_running());
    let (bb_text, bb_state) = if !bb.enabled {
        ("disabled", 0)
    } else if !bb_running {
        ("idle (no capture source)", 0)
    } else if bb.status.disk_paused {
        ("disk full — ingestion paused", 2)
    } else {
        ("recording", 1)
    };
    api.set_bb_status(bb_text.into());
    api.set_bb_status_state(bb_state);
    api.set_bb_last_error(bb.status.last_error.as_deref().unwrap_or("").into());
    api.set_bb_engine_running(bb_running);

    // Anomaly chip: stopped / running (waiting for frames) / buffering.
    let an = &controller.anomaly;
    let an_running = an.engine.as_ref().is_some_and(|e| e.is_running());
    let (an_text, an_state): (String, i32) = if !an_running {
        ("stopped".to_string(), 0)
    } else if !an.status.buffering {
        ("running (waiting for frames)".to_string(), 0)
    } else {
        (
            format!("buffering — {}s held", an.status.buffer_secs_filled),
            1,
        )
    };
    api.set_an_status(an_text.into());
    api.set_an_status_state(an_state);
    api.set_an_last_error(an.status.last_error.as_deref().unwrap_or("").into());
    api.set_an_engine_running(an_running);
    api.set_an_can_capture(an_running && an.status.buffering && an.status.clips_busy == 0);
}

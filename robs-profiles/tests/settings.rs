//! Integration tests for `AnomalySettings` persistence.
//!
//! Pure std: each test owns a RAII `TestDir` (mirrors the pattern in
//! `robs-outputs/tests/`, no `tempfile` dependency) and exercises the
//! path-parameterized `load_from`/`save_to` core; the canonical-path
//! wrappers are thin enough not to need their own tests.

use robs_profiles::settings::AnomalySettings;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static DIR_SEQ: AtomicU32 = AtomicU32::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let seq = DIR_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "robs-profiles-settings-{}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create test dir");
        Self(dir)
    }

    /// A path inside the test dir.
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A settings set that differs from `Default` in (almost) every field, so a
/// round-trip actually proves values survived.
fn customized() -> AnomalySettings {
    AnomalySettings {
        enabled: true,
        output_dir: "D:\\Clips\\Anomaly".into(),
        pre_roll_secs: 30,
        post_roll_secs: 5,
        max_buffer_mb: 128,
        segment_duration_secs: 5,
        encoder: "h264_nvenc".into(),
        crf: 20,
        video_bitrate_kbps: 8000,
        output_width: 1280,
        output_height: 720,
        clip_prefix: "Clip".into(),
        clip_suffix: "_marked".into(),
        capture_clip_hotkey: "Ctrl+Shift+A".into(),
    }
}

#[test]
fn save_then_load_round_trips_every_field() {
    let dir = TestDir::new();
    let path = dir.path("settings.json");
    let custom = customized();
    assert_ne!(custom, AnomalySettings::default());

    custom.save_to(&path).expect("save_to");
    let loaded = AnomalySettings::load_from(&path).expect("load_from after save");

    assert_eq!(loaded, custom);
}

#[test]
fn missing_file_yields_none() {
    let dir = TestDir::new();
    assert!(AnomalySettings::load_from(&dir.path("absent.json")).is_none());
}

#[test]
fn corrupt_json_yields_none() {
    let dir = TestDir::new();
    let path = dir.path("settings.json");
    fs::write(&path, "{ not valid json").unwrap();
    assert!(AnomalySettings::load_from(&path).is_none());
}

#[test]
fn file_without_anomaly_key_yields_none() {
    let dir = TestDir::new();
    let path = dir.path("settings.json");
    fs::write(&path, r#"{"blackbox":{"enabled":true}}"#).unwrap();
    assert!(AnomalySettings::load_from(&path).is_none());
}

#[test]
fn unknown_fields_inside_anomaly_are_ignored() {
    let dir = TestDir::new();
    let path = dir.path("settings.json");
    // Forward-added fields (a newer build wrote this file) must not brick an
    // older one: the known subset still loads.
    fs::write(
        &path,
        r#"{"anomaly":{"pre_roll_secs":30,"future_field":123}}"#,
    )
    .unwrap();
    let loaded = AnomalySettings::load_from(&path).expect("loads despite unknown field");
    assert_eq!(loaded.pre_roll_secs, 30);
    // Fields absent from the partial section fall back to struct defaults.
    assert_eq!(loaded.post_roll_secs, AnomalySettings::default().post_roll_secs);
}

#[test]
fn save_preserves_unrelated_sections() {
    let dir = TestDir::new();
    let path = dir.path("settings.json");
    fs::write(&path, r#"{"blackbox":{"enabled":true,"crf":20}}"#).unwrap();

    customized().save_to(&path).expect("save_to");

    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(root["blackbox"]["enabled"], serde_json::json!(true));
    assert_eq!(root["blackbox"]["crf"], serde_json::json!(20));
    assert!(root["anomaly"].is_object());
}

#[test]
fn save_replaces_a_stale_anomaly_section() {
    let dir = TestDir::new();
    let path = dir.path("settings.json");
    fs::write(
        &path,
        r#"{"anomaly":{"pre_roll_secs":1,"clip_prefix":"Old"},"blackbox":{"enabled":true}}"#,
    )
    .unwrap();

    let custom = customized();
    custom.save_to(&path).expect("save_to");

    let loaded = AnomalySettings::load_from(&path).expect("load_from after replace");
    assert_eq!(loaded, custom);
    // The unrelated section survived the overwrite.
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(root["blackbox"]["enabled"], serde_json::json!(true));
}

#[test]
fn save_creates_missing_parent_directories() {
    let dir = TestDir::new();
    let path = dir.path("nested/deep/settings.json");

    customized().save_to(&path).expect("save_to creates dirs");

    assert!(path.is_file());
    assert!(AnomalySettings::load_from(&path).is_some());
}

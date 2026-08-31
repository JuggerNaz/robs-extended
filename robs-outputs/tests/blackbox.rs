//! Integration tests for the Blackbox Dual Recording Engine.
//!
//! Two tiers:
//! - **Pure** tests cover the deterministic logic that has no external
//!   dependencies: config math, sink path naming + ring-buffer reclaim, `.part`
//!   marker handling, and the engine's guard clauses. These run everywhere.
//! - **ffmpeg** tests exercise the real encode / remux pipeline (segment
//!   finalization, crash recovery). They are skipped automatically when
//!   `ffmpeg` is not on `PATH`, because CI sandboxes may not ship it even
//!   though ROBS itself is ffmpeg-dependent at runtime.
//!
//! No `tempfile` dependency: each test owns its directory through the [`TestDir`]
//! guard, which removes it on drop.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use robs_core::{BlackboxEvent, EventBus, RobsEvent};
use robs_outputs::blackbox::recovery::recover;
use robs_outputs::blackbox::segment::{part_marker, ActiveSegment};
use robs_outputs::{BlackboxConfig, BlackboxEngine, BlackboxSink, LocalFileSink};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// RAII temp directory. Created on construction, removed (best-effort) on drop
/// so tests never leak into the system temp dir even on panic.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        // Nanos alone are not unique on macOS (~microsecond clock granularity):
        // parallel tests starting in the same tick collide, and the first twin's
        // Drop deletes the dir out from under the second. A per-process counter
        // makes the name collision-proof.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "robs-blackbox-test-{}-{seq}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&p).expect("create test dir");
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A small, software-only config so tests never depend on NVENC hardware.
/// Output dir is the only field that varies per test.
fn make_config(output_dir: PathBuf) -> BlackboxConfig {
    BlackboxConfig {
        output_dir,
        output_width: 64,
        output_height: 64,
        fps: 15.0,
        // Keep segments large/long so a single segment is produced per test;
        // rotation is not under test here.
        segment_duration_secs: 3600,
        segment_size_mb: 4,
        encoder: "libx264".into(),
        crf: 28,
        video_bitrate_kbps: 500,
        disk_low_warn_percent: 10,
        disk_low_critical_percent: 3,
        max_retention_gb: 1,
        stall_threshold_secs: 60,
    }
}

/// A raw BGRA frame (all-black) of `width x height`.
fn frame(width: u32, height: u32) -> Vec<u8> {
    vec![0u8; width as usize * height as usize * 4]
}

/// True if `ffmpeg` is runnable on PATH.
fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Call at the top of an ffmpeg-dependent test: returns `false` (after logging)
/// when ffmpeg is absent so the test can bail out as a skip rather than fail.
fn require_ffmpeg() -> bool {
    if ffmpeg_available() {
        true
    } else {
        eprintln!("skipping blackbox ffmpeg test: ffmpeg not on PATH");
        false
    }
}

/// Stamp a file's mtime to `days_ago` days before now. Used to make reclaim's
/// oldest-first ordering deterministic (mtime-second resolution is enough once
/// the gaps are a day apart).
fn set_mtime_days_ago(path: &Path, days_ago: u64) {
    let mtime = SystemTime::now() - Duration::from_secs(days_ago * 86_400);
    let times = fs::FileTimes::new().set_modified(mtime);
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for set_times")
        .set_times(times)
        .expect("set_times");
}

/// Collect every `BlackboxEvent` currently queued on `rx`, dropping the
/// non-blackbox events (e.g. periodic `StatusUpdated` arrives as blackbox too).
fn drain_blackbox(rx: &robs_core::EventRx) -> Vec<BlackboxEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let RobsEvent::Blackbox(b) = ev {
            out.push(b);
        }
    }
    out
}

// ===========================================================================
// Pure tests — no ffmpeg
// ===========================================================================

#[test]
fn config_segment_size_bytes_converts_mib() {
    let cfg = make_config(PathBuf::from("/tmp/x"));
    // segment_size_mb defaults to 4 in make_config.
    assert_eq!(cfg.segment_size_bytes(), 4 * 1024 * 1024);

    let mut cfg = cfg;
    cfg.segment_size_mb = 0;
    assert_eq!(cfg.segment_size_bytes(), 0);
    cfg.segment_size_mb = 1;
    assert_eq!(cfg.segment_size_bytes(), 1024 * 1024);
}

#[test]
fn part_marker_appends_dot_part_suffix() {
    let seg = Path::new("/tmp/blackbox_0000_a.mkv");
    let marker = part_marker(seg);
    // ".part" is appended, not used as a replacement extension, so the `.mkv`
    // identity of the segment stays readable.
    assert_eq!(marker.file_name().unwrap(), "blackbox_0000_a.mkv.part");
}

#[test]
fn sink_segment_target_is_named_and_located_correctly() {
    let dir = TestDir::new();
    let sink = LocalFileSink::new(&make_config(dir.path().to_path_buf()));

    let path = sink.segment_target(0, chrono::Utc::now());
    assert_eq!(path.parent(), Some(dir.path()), "segment under output dir");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("mkv"));
    let name = path.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("blackbox_0000_"),
        "expected zero-padded index prefix, got {name}"
    );

    // Index rolls into the next zero-padded slot.
    let name42 = sink
        .segment_target(42, chrono::Utc::now())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name42.starts_with("blackbox_0042_"));
}

#[test]
fn sink_storage_status_reports_real_volume() {
    let dir = TestDir::new();
    let sink = LocalFileSink::new(&make_config(dir.path().to_path_buf()));
    let s = sink.storage_status();
    // The temp volume is real and non-trivial; fs4 should resolve it.
    assert!(s.total_bytes > 0, "expected a non-zero total volume size");
    assert!(s.free_bytes <= s.total_bytes);
}

#[test]
fn sink_reclaim_is_noop_under_retention_and_partial_to_demand() {
    let dir = TestDir::new();
    let sink = LocalFileSink::new(&make_config(dir.path().to_path_buf()));

    let chunk = 100 * 1024; // 100 KiB each
    let payload = vec![0u8; chunk];
    let oldest = dir.path().join("blackbox_0000_oldest.mkv");
    let mid = dir.path().join("blackbox_0001_mid.mkv");
    let newest = dir.path().join("blackbox_0002_newest.mkv");
    fs::write(&oldest, &payload).unwrap();
    fs::write(&mid, &payload).unwrap();
    fs::write(&newest, &payload).unwrap();
    // Distinct mtimes so oldest-first ordering is unambiguous.
    set_mtime_days_ago(&oldest, 30);
    set_mtime_days_ago(&mid, 20);
    set_mtime_days_ago(&newest, 10);

    // (a) No demand + total (300 KiB) far under the 1 GiB retention cap → no-op.
    let freed = sink.reclaim(0).expect("reclaim noop");
    assert_eq!(freed, 0, "nothing should be reclaimed when under retention");
    assert!(oldest.exists() && mid.exists() && newest.exists());

    // (b) Demand 150 KiB: oldest two (200 KiB total) deleted, newest survives.
    let freed = sink.reclaim(150 * 1024).expect("reclaim demand");
    assert!(
        freed >= 150 * 1024,
        "expected >=150 KiB freed, got {freed}"
    );
    assert!(!oldest.exists(), "oldest segment reclaimed first");
    assert!(!mid.exists(), "second segment reclaimed to meet demand");
    assert!(newest.exists(), "newest segment must survive partial reclaim");
}

#[test]
fn recovery_clears_stale_marker_with_no_segment() {
    // A `.part` marker whose segment was never written (or already gone) must
    // be cleared silently — recover returns 0 and emits no Recovered event.
    let dir = TestDir::new();
    let marker = dir.path().join("blackbox_0000_ghost.mkv.part");
    fs::write(&marker, b"").unwrap();

    let (bus, rx) = EventBus::new();
    let recovered = recover(dir.path(), &bus.tx());

    assert_eq!(recovered, 0, "nothing to remux, so 0 recovered");
    assert!(!marker.exists(), "stale marker must be removed");
    assert!(rx.try_recv().is_err(), "no events should be emitted");
}

#[test]
fn engine_stop_is_safe_when_never_started() {
    // stop() before start() must be a no-op (no panic, no thread joins).
    let dir = TestDir::new();
    let (bus, _rx) = EventBus::new();
    let sink = Arc::new(LocalFileSink::new(&make_config(dir.path().to_path_buf())));
    let mut engine = BlackboxEngine::new(make_config(dir.path().to_path_buf()), sink, bus.tx());

    engine.stop();
    engine.stop(); // double-stop is also safe
    assert!(!engine.is_running());
}

#[test]
fn engine_submit_frame_is_noop_when_not_running() {
    // submit_frame must never block or panic when the engine is stopped.
    let dir = TestDir::new();
    let (bus, _rx) = EventBus::new();
    let sink = Arc::new(LocalFileSink::new(&make_config(dir.path().to_path_buf())));
    let engine = BlackboxEngine::new(make_config(dir.path().to_path_buf()), sink, bus.tx());

    let before = Instant::now();
    engine.submit_frame(&frame(64, 64), 64, 64);
    let elapsed = before.elapsed();

    assert!(elapsed < Duration::from_millis(50), "submit_frame must be non-blocking");
    assert!(!engine.is_running());
}

// ===========================================================================
// ffmpeg tests — skipped when ffmpeg is absent
// ===========================================================================

#[test]
fn engine_lifecycle_writes_a_finalized_segment() {
    if !require_ffmpeg() {
        return;
    }
    let dir = TestDir::new();
    let (bus, rx) = EventBus::new();
    let sink = Arc::new(LocalFileSink::new(&make_config(dir.path().to_path_buf())));
    let mut engine = BlackboxEngine::new(
        make_config(dir.path().to_path_buf()),
        sink,
        bus.tx(),
    );

    engine.start();
    assert!(engine.is_running());

    // Feed ~1.5s of frames so the worker has time to spawn ffmpeg, open a
    // segment, and write. submit_frame is non-blocking regardless.
    let f = frame(64, 64);
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        engine.submit_frame(&f, 64, 64);
        std::thread::sleep(Duration::from_millis(33));
    }

    engine.stop();
    assert!(!engine.is_running());

    let events = drain_blackbox(&rx);
    assert!(
        events.iter().any(|e| matches!(e, BlackboxEvent::Started)),
        "expected a Started event, got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, BlackboxEvent::Stopped)),
        "expected a Stopped event, got {events:?}"
    );
    let closed = events
        .iter()
        .filter(|e| matches!(e, BlackboxEvent::SegmentClosed { .. }))
        .count();
    assert!(closed >= 1, "expected >=1 finalized segment, got {closed}");

    // On disk: at least one finalized .mkv, and no leftover .part markers.
    let (mut mkv, mut part) = (0, 0);
    for entry in fs::read_dir(dir.path()).expect("read output dir").flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.ends_with(".mkv") {
            mkv += 1;
        } else if s.ends_with(".part") {
            part += 1;
        }
    }
    assert!(mkv >= 1, "expected >=1 .mkv segment on disk, got {mkv}");
    assert_eq!(part, 0, "no .part markers should remain after a clean stop");
}

#[test]
fn recovery_remuxes_a_part_marked_segment() {
    if !require_ffmpeg() {
        return;
    }
    let dir = TestDir::new();
    let cfg = make_config(dir.path().to_path_buf());

    // Produce a valid finalized .mkv via a real ffmpeg segment.
    let seg_path = dir.path().join("blackbox_0000_real.mkv");
    let mut seg = ActiveSegment::open(&cfg, 0, 64, 64, seg_path.clone()).expect("open segment");
    for _ in 0..15 {
        seg.write_frame(&frame(64, 64)).expect("write frame");
        std::thread::sleep(Duration::from_millis(20));
    }
    seg.close().expect("close segment"); // finalizes + removes the marker
    assert!(seg_path.exists(), "segment file should exist after close");

    // Simulate a crash before finalization by re-adding the .part marker.
    fs::write(part_marker(&seg_path), b"").unwrap();

    // Recovery should remux the segment in place and clear the marker.
    let (bus, rx) = EventBus::new();
    let recovered = recover(dir.path(), &bus.tx());
    assert_eq!(recovered, 1, "exactly one segment should be recovered");
    assert!(
        !part_marker(&seg_path).exists(),
        "marker must be cleared after successful recovery"
    );

    let events = drain_blackbox(&rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BlackboxEvent::Recovered { .. })),
        "expected a Recovered event, got {events:?}"
    );
}

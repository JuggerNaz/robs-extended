//! Integration tests for the Short Clip Anomaly Capture engine.
//!
//! Two tiers, mirroring `tests/blackbox.rs`:
//! - **Pure** tests cover the deterministic, dependency-free logic: config
//!   dimension resolution, the rolling [`Ring`]'s eviction (by time, by byte
//!   cap, never-empty) and pre-roll selection, the export empty-input guard,
//!   and the engine's guard clauses (stop-before-start, submit/save while
//!   stopped, concurrent `save` → `ClipBusy`). These run everywhere.
//! - **ffmpeg** tests exercise the real segment-writer + concat pipeline. They
//!   auto-skip when `ffmpeg` is not on `PATH`, since CI sandboxes may omit it
//!   even though ROBS depends on ffmpeg at runtime.
//!
//! No `tempfile` dependency: each test owns its directory through the [`TestDir`]
//! guard, which removes it on drop.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use robs_core::{AnomalyEvent, EventBus, RobsEvent};
use robs_outputs::anomaly::export::export_clip;
use robs_outputs::anomaly::{Ring, RingEntry};
use robs_outputs::{AnomalyCaptureEngine, AnomalyConfig};

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
            "robs-anomaly-test-{}-{seq}-{nanos}",
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
/// `output_dir` is the only field that varies per test.
fn make_config(output_dir: PathBuf) -> AnomalyConfig {
    AnomalyConfig {
        output_dir,
        output_width: 64,
        output_height: 64,
        fps: 15.0,
        pre_roll_secs: 15,
        post_roll_secs: 15,
        max_buffer_bytes: 64 * 1024 * 1024,
        // Short segments so ffmpeg tests rotate quickly and produce several
        // finalized scratch files within a couple of seconds.
        segment_duration_secs: 1,
        encoder: "libx264".into(),
        crf: 28,
        video_bitrate_kbps: 500,
        clip_prefix: "Anomaly".into(),
        clip_suffix: String::new(),
        disk_low_warn_percent: 10,
    }
}

/// A raw BGRA frame (all-black) of `width x height`.
fn frame(width: u32, height: u32) -> Vec<u8> {
    vec![0u8; width as usize * height as usize * 4]
}

/// Build a [`RingEntry`] with a throwaway timestamp; the ring never inspects
/// `started_utc` for eviction, only `duration_ms` and `bytes`.
fn entry(path: PathBuf, index: u64, duration_ms: u64, bytes: u64) -> RingEntry {
    RingEntry {
        path,
        index,
        started_utc: chrono::Utc::now(),
        duration_ms,
        bytes,
    }
}

/// Create a dummy segment file of `bytes` zeros under `dir` and return its path.
fn segment_file(dir: &Path, name: &str, bytes: usize) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, vec![0u8; bytes]).expect("write segment file");
    p
}

/// Every path currently held by the ring, oldest-first. Uses a huge pre-roll
/// request so [`Ring::pin_pre_roll`] returns the entire ring.
fn all_paths(ring: &Ring) -> Vec<PathBuf> {
    ring.pin_pre_roll(u64::MAX)
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
        eprintln!("skipping anomaly ffmpeg test: ffmpeg not on PATH");
        false
    }
}

/// Collect every `AnomalyEvent` currently queued on `rx`, dropping non-anomaly
/// events.
fn drain_anomaly(rx: &robs_core::EventRx) -> Vec<AnomalyEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let RobsEvent::Anomaly(a) = ev {
            out.push(a);
        }
    }
    out
}

/// Feed frames continuously for `millis`, then stop. Used to fill the rolling
/// buffer before a trigger.
fn feed(engine: &AnomalyCaptureEngine, f: &[u8], w: u32, h: u32, millis: u64) {
    let end = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < end {
        engine.submit_frame(f, w, h);
        std::thread::sleep(Duration::from_millis(33));
    }
}

/// Feed frames continuously while polling for a `ClipReady` event, returning
/// its `(clip_id, path)` if one arrives within `timeout_secs`. Feeding through
/// the post-roll window is what lets the worker tee new segments into the clip.
fn feed_until_clip_ready(
    engine: &AnomalyCaptureEngine,
    rx: &robs_core::EventRx,
    f: &[u8],
    w: u32,
    h: u32,
    timeout_secs: u64,
) -> Option<(String, String)> {
    let end = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < end {
        engine.submit_frame(f, w, h);
        while let Ok(ev) = rx.try_recv() {
            if let RobsEvent::Anomaly(AnomalyEvent::ClipReady { clip_id, path }) = ev {
                return Some((clip_id, path));
            }
        }
        std::thread::sleep(Duration::from_millis(33));
    }
    None
}

// ===========================================================================
// Pure tests — no ffmpeg
// ===========================================================================

#[test]
fn config_scaled_output_inherits_capture_dims_when_zero() {
    let mut cfg = make_config(PathBuf::from("/tmp/x"));
    cfg.output_width = 0;
    cfg.output_height = 0;
    // A 0 dimension means "no scaling": the capture resolution passes through.
    assert_eq!(cfg.scaled_output(1920, 1080), (1920, 1080));
    assert_eq!(cfg.scaled_output(320, 240), (320, 240));
}

#[test]
fn config_scaled_output_uses_explicit_dims_when_set() {
    let mut cfg = make_config(PathBuf::from("/tmp/x"));
    cfg.output_width = 1280;
    cfg.output_height = 720;
    // Explicit dimensions override the capture resolution regardless of input.
    assert_eq!(cfg.scaled_output(1920, 1080), (1280, 720));
    assert_eq!(cfg.scaled_output(640, 360), (1280, 720));
}

#[test]
fn ring_new_is_empty_with_zero_secs() {
    let ring = Ring::new(1024);
    assert!(ring.is_empty());
    assert_eq!(ring.secs_filled(), 0);
    assert!(all_paths(&ring).is_empty());
}

#[test]
fn ring_secs_filled_floors_whole_seconds() {
    let mut ring = Ring::new(1024);
    ring.push(entry(PathBuf::from("/a"), 0, 1500, 10));
    ring.push(entry(PathBuf::from("/b"), 1, 1500, 10));
    // 3000 ms -> 3 whole seconds.
    assert_eq!(ring.secs_filled(), 3);

    let mut short = Ring::new(1024);
    short.push(entry(PathBuf::from("/c"), 0, 900, 10));
    // 900 ms floors to 0.
    assert_eq!(short.secs_filled(), 0);
}

#[test]
fn ring_sweep_evicts_oldest_past_keep_window() {
    let dir = TestDir::new();
    let mut ring = Ring::new(u64::MAX); // byte cap never triggers here
    let oldest = segment_file(dir.path(), "seg_0.mkv", 100);
    let mid = segment_file(dir.path(), "seg_1.mkv", 100);
    let newest = segment_file(dir.path(), "seg_2.mkv", 100);
    ring.push(entry(oldest.clone(), 0, 1000, 100));
    ring.push(entry(mid.clone(), 1, 1000, 100));
    ring.push(entry(newest.clone(), 2, 1000, 100));

    // keep_secs = 2 -> keep_ms = 2000. Total 3000 ms exceeds it, so the oldest
    // entry is evicted; the remainder (2000 ms) is within window and survives.
    ring.sweep(2);

    assert!(!oldest.exists(), "oldest segment evicted past keep window");
    assert!(mid.exists() && newest.exists());
    assert_eq!(
        all_paths(&ring),
        vec![mid, newest],
        "remaining entries stay oldest-first"
    );
}

#[test]
fn ring_sweep_evicts_oldest_past_byte_cap() {
    let dir = TestDir::new();
    let mut ring = Ring::new(150); // 150-byte cap; keep_secs huge so time is irrelevant
    let oldest = segment_file(dir.path(), "seg_0.mkv", 100);
    let mid = segment_file(dir.path(), "seg_1.mkv", 100);
    let newest = segment_file(dir.path(), "seg_2.mkv", 100);
    ring.push(entry(oldest.clone(), 0, 1000, 100));
    ring.push(entry(mid.clone(), 1, 1000, 100));
    ring.push(entry(newest.clone(), 2, 1000, 100));

    ring.sweep(3600);

    // 300 > 150 -> evict oldest (200 > 150) -> evict mid (100, now only 1 left).
    assert!(!oldest.exists() && !mid.exists());
    assert!(newest.exists(), "newest segment must survive byte-cap reclaim");
    assert_eq!(all_paths(&ring), vec![newest]);
}

#[test]
fn ring_sweep_keeps_at_least_one_entry() {
    let dir = TestDir::new();
    let mut ring = Ring::new(1); // impossible byte cap; keep_secs = 0 forces time eviction too
    let mut paths = Vec::new();
    for i in 0..5 {
        let p = segment_file(dir.path(), &format!("seg_{i}.mkv"), 100);
        ring.push(entry(p.clone(), i, 1000, 100));
        paths.push(p);
    }

    ring.sweep(0);

    // Even with both caps permanently exceeded, exactly one (the newest) survives.
    assert_eq!(all_paths(&ring).len(), 1, "ring must never empty below one");
    let survivor = all_paths(&ring)[0].clone();
    assert_eq!(survivor, paths[4], "only the newest entry survives");
    for p in &paths[..4] {
        assert!(!p.exists(), "older segment {:?} should be deleted", p);
    }
}

#[test]
fn ring_pin_pre_roll_picks_newest_oldest_first() {
    let mut ring = Ring::new(u64::MAX);
    let a = PathBuf::from("/a.mkv");
    let b = PathBuf::from("/b.mkv");
    let c = PathBuf::from("/c.mkv");
    ring.push(entry(a.clone(), 0, 1000, 10));
    ring.push(entry(b.clone(), 1, 1000, 10));
    ring.push(entry(c.clone(), 2, 1000, 10));

    // Request 2s: walking newest-first accumulates c (1s) then b (2s) -> the
    // newest two, returned oldest-first as [b, c].
    assert_eq!(ring.pin_pre_roll(2), vec![b, c]);
}

#[test]
fn ring_pin_pre_roll_returns_all_when_buffer_short() {
    let mut ring = Ring::new(u64::MAX);
    let a = PathBuf::from("/a.mkv");
    ring.push(entry(a.clone(), 0, 1000, 10));

    // Requesting more footage than the ring holds returns everything available,
    // never an empty list when the ring is non-empty.
    assert_eq!(ring.pin_pre_roll(5), vec![a]);
}

#[test]
fn export_clip_rejects_empty_segment_list() {
    let dir = TestDir::new();
    let out = dir.path().join("clip.mp4");

    // An empty segment list bails before ffmpeg is ever spawned, so no
    // concat.txt is written and no output file appears.
    let err = export_clip(dir.path(), &[], &out).unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("no segments"),
        "expected the empty-list guard to fire, got: {msg}"
    );
    assert!(!out.exists(), "no output should be written on empty input");
    assert!(
        !dir.path().join("concat.txt").exists(),
        "concat list must not be written for an empty export"
    );
}

#[test]
fn engine_stop_is_safe_when_never_started() {
    // stop() before start() must be a no-op (no panic, no thread joins).
    let dir = TestDir::new();
    let (bus, _rx) = EventBus::new();
    let mut engine = AnomalyCaptureEngine::new(make_config(dir.path().to_path_buf()), bus.tx());

    engine.stop();
    engine.stop(); // double-stop is also safe
    assert!(!engine.is_running());
}

#[test]
fn engine_submit_frame_is_noop_and_nonblocking_when_not_running() {
    // submit_frame must never block or panic when the engine is stopped.
    let dir = TestDir::new();
    let (bus, _rx) = EventBus::new();
    let engine = AnomalyCaptureEngine::new(make_config(dir.path().to_path_buf()), bus.tx());

    let before = Instant::now();
    engine.submit_frame(&frame(64, 64), 64, 64);
    let elapsed = before.elapsed();

    assert!(elapsed < Duration::from_millis(50), "submit_frame must be non-blocking");
    assert!(!engine.is_running());
}

#[test]
fn engine_save_returns_none_when_not_running() {
    let dir = TestDir::new();
    let (bus, _rx) = EventBus::new();
    let engine = AnomalyCaptureEngine::new(make_config(dir.path().to_path_buf()), bus.tx());

    assert!(engine.save(5, 5).is_none(), "save must be a no-op when stopped");
    assert!(!engine.is_running());
}

#[test]
fn engine_save_emits_clipbusy_for_concurrent_request() {
    // Two rapid triggers while the buffer is running: the worker accepts the
    // first (setting up an in-flight export) and rejects the second with
    // ClipBusy. No ffmpeg is exercised here — the busy path is pure control
    // flow — so this test does not depend on ffmpeg being installed.
    let dir = TestDir::new();
    let (bus, rx) = EventBus::new();
    let mut engine = AnomalyCaptureEngine::new(make_config(dir.path().to_path_buf()), bus.tx());

    engine.start();
    assert!(engine.is_running());

    // First trigger: long post-roll keeps the export in flight. Second trigger:
    // short, but it must be rejected because the first is still active.
    let _first = engine.save(0, 30).expect("first save accepted");
    let second = engine.save(0, 1).expect("second save queued");

    // The worker drains both commands within a loop iteration; the second must
    // produce ClipBusy(second) promptly.
    let mut busy = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        while let Ok(ev) = rx.try_recv() {
            if let RobsEvent::Anomaly(AnomalyEvent::ClipBusy { clip_id }) = ev {
                busy = Some(clip_id);
            }
        }
        if busy.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    engine.stop();

    let busy = busy.expect("expected a ClipBusy event for the second save");
    assert_eq!(busy, second, "ClipBusy must reference the rejected clip id");
}

// ===========================================================================
// ffmpeg tests — skipped when ffmpeg is absent
// ===========================================================================

#[test]
fn scratch_segment_writes_finalized_mkv_with_no_part_marker() {
    // The defining difference from the Blackbox segment writer: anomaly scratch
    // segments are disposable and never carry a `.part` crash marker. A clean
    // open/write/close must still yield a finalized, non-empty `.mkv`.
    if !require_ffmpeg() {
        return;
    }
    use robs_outputs::anomaly::ScratchSegment;

    let dir = TestDir::new();
    let cfg = make_config(dir.path().to_path_buf());
    let path = dir.path().join("scratch_0000.mkv");

    let mut seg =
        ScratchSegment::open(&cfg, 0, 64, 64, path.clone()).expect("open scratch segment");
    for _ in 0..15 {
        seg.write_frame(&frame(64, 64)).expect("write frame");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (bytes, duration_ms) = seg.close().expect("close scratch segment");

    assert!(path.exists(), "finalized scratch segment must exist on disk");
    assert!(bytes > 0, "finalized segment must be non-empty");
    assert!(duration_ms > 0);
    assert!(
        !dir.path().join("scratch_0000.mkv.part").exists(),
        "anomaly scratch segments must never create a .part marker"
    );
}

#[test]
fn engine_lifecycle_emits_started_stopped_and_writes_segments() {
    if !require_ffmpeg() {
        return;
    }
    let dir = TestDir::new();
    let (bus, rx) = EventBus::new();
    let mut engine = AnomalyCaptureEngine::new(make_config(dir.path().to_path_buf()), bus.tx());

    engine.start();
    assert!(engine.is_running());

    // Feed ~3s so the 1s segment duration rotates at least twice, finalizing
    // segments into the rolling buffer.
    let f = frame(64, 64);
    feed(&engine, &f, 64, 64, 3000);

    engine.stop();
    assert!(!engine.is_running());

    let events = drain_anomaly(&rx);
    assert!(
        events.iter().any(|e| matches!(e, AnomalyEvent::Started)),
        "expected a Started event, got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, AnomalyEvent::Stopped)),
        "expected a Stopped event, got {events:?}"
    );

    // On disk: at least one finalized .mkv under <output_dir>/buffer, and never
    // a leftover .part marker (scratch segments are disposable).
    let buffer_dir = dir.path().join("buffer");
    let (mut mkv, mut part) = (0, 0);
    for entry in fs::read_dir(&buffer_dir).expect("read buffer dir").flatten() {
        let s = entry.file_name();
        let s = s.to_string_lossy();
        if s.ends_with(".mkv") {
            mkv += 1;
        } else if s.ends_with(".part") {
            part += 1;
        }
    }
    assert!(mkv >= 1, "expected >=1 finalized scratch .mkv, got {mkv}");
    assert_eq!(part, 0, "no .part markers should ever appear in the anomaly buffer");
}

#[test]
fn engine_save_exports_nonempty_mp4_with_prefix() {
    if !require_ffmpeg() {
        return;
    }
    let dir = TestDir::new();
    let (bus, rx) = EventBus::new();
    let mut engine = AnomalyCaptureEngine::new(make_config(dir.path().to_path_buf()), bus.tx());

    engine.start();
    let f = frame(64, 64);

    // Fill the rolling buffer with several seconds (>1 segment) so the export
    // concats multiple pre-roll segments plus the post-roll tail.
    feed(&engine, &f, 64, 64, 3500);

    let clip_id = engine
        .save(2, 2)
        .expect("save returns a clip id while running");

    // Keep feeding through the post-roll window while waiting for the export.
    let ready = feed_until_clip_ready(&engine, &rx, &f, 64, 64, 20);
    engine.stop();

    let (ready_id, path_str) = ready.expect("expected a ClipReady event within 20s");
    assert_eq!(ready_id, clip_id, "ClipReady must reference the requested clip id");

    let clip_path = PathBuf::from(&path_str);
    let meta = fs::metadata(&clip_path).expect("exported clip must exist on disk");
    assert!(meta.len() > 0, "exported clip must be non-empty");

    let name = clip_path
        .file_name()
        .expect("clip has a file name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("Anomaly") && name.ends_with(".mp4"),
        "clip must be named <prefix>...<clip_id>.mp4, got {name}"
    );

    // A clean single-export run must never surface a ClipFailed. (The ClipReady
    // itself was already observed by feed_until_clip_ready above.)
    let tail = drain_anomaly(&rx);
    assert!(
        !tail
            .iter()
            .any(|e| matches!(e, AnomalyEvent::ClipFailed { .. })),
        "no ClipFailed should occur on a clean export: {tail:?}"
    );
}

#[test]
fn engine_save_exports_clip_when_buffer_underfilled() {
    // Graceful degradation: a trigger that asks for more pre-roll than the ring
    // currently holds must still export a valid, non-empty clip containing
    // whatever footage was buffered, rather than failing.
    if !require_ffmpeg() {
        return;
    }
    let dir = TestDir::new();
    let (bus, rx) = EventBus::new();
    let mut engine = AnomalyCaptureEngine::new(make_config(dir.path().to_path_buf()), bus.tx());

    engine.start();
    let f = frame(64, 64);

    // Only ~1.3s buffered (one finalized segment) — far short of the 4s requested.
    feed(&engine, &f, 64, 64, 1300);

    engine.save(4, 1).expect("save queued while running");

    let ready = feed_until_clip_ready(&engine, &rx, &f, 64, 64, 20);
    engine.stop();

    let (_id, path_str) = ready.expect("expected a ClipReady even when underfilled");
    let clip_path = PathBuf::from(&path_str);
    let meta = fs::metadata(&clip_path).expect("exported clip must exist on disk");
    assert!(meta.len() > 0, "underfilled clip must still be non-empty");
}

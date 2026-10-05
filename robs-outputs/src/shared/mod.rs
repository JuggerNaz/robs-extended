//! Machinery shared by the Blackbox and Anomaly engines.
//!
//! Both engines are built from the same skeleton — a frame-pump worker thread
//! that turns raw BGRA frames into rotated ffmpeg `.mkv` segments, plus a
//! monitor thread that probes disk space and publishes status — while
//! deliberately diverging in policy (crash-safe `.part` markers + recovery +
//! sink reclaim + disk-critical pause for Blackbox; disposable ring-buffered
//! scratch segments + MP4-concat clip export for Anomaly). This module holds
//! the deduplicated machinery the two share:
//!
//! - [`FrameInput`] — one raw captured frame on the engine channel;
//! - [`now_ms`] / [`sleep_with_stop`] — clock + stop-responsive sleep helpers;
//! - [`build_storage_status`] / [`free_percent`] — storage snapshot math;
//! - [`probe_free_space`] — volume probe with parent-dir fallback;
//! - [`LatchedFlag`] — boolean latch with hysteresis-controlled release, used
//!   by both monitors so low/critical conditions fire once per episode
//!   instead of flapping near the threshold.
//! - [`segment`] — the common ffmpeg rawvideo segment writer behind
//!   `ActiveSegment` and `ScratchSegment` (see [`SegmentOps`]);
//! - [`pump`] — the single parameterized worker frame-pump loop
//!   ([`frame_pump`](pump::frame_pump)) both engines run, parameterized by
//!   [`FramePumpPolicy`](pump::FramePumpPolicy).

pub mod pump;
pub mod segment;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use robs_core::event::BlackboxStorageStatus;

/// One raw captured frame handed to an engine's worker.
pub(crate) struct FrameInput {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Milliseconds since the Unix epoch (0 if the clock is before the epoch).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sleep for `d`, but wake early if `stop_flag` becomes set.
pub(crate) fn sleep_with_stop(d: Duration, stop_flag: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut remaining = d;
    while remaining > Duration::ZERO {
        if stop_flag.load(Ordering::SeqCst) {
            return;
        }
        let t = remaining.min(step);
        std::thread::sleep(t);
        remaining = remaining.saturating_sub(t);
    }
}

/// Percentage of the volume that is free (0.0–100.0); 0.0 when the total is
/// unknown (avoids div-by-zero).
pub(crate) fn free_percent(free_bytes: u64, total_bytes: u64) -> f32 {
    if total_bytes == 0 {
        0.0
    } else {
        (free_bytes as f64 / total_bytes as f64 * 100.0) as f32
    }
}

/// Build the blackbox storage snapshot published with each monitor tick.
pub(crate) fn build_storage_status(
    free_bytes: u64,
    total_bytes: u64,
    warn: bool,
    critical: bool,
) -> BlackboxStorageStatus {
    BlackboxStorageStatus {
        free_bytes,
        total_bytes,
        free_percent: free_percent(free_bytes, total_bytes),
        low_warning: warn,
        critical,
    }
}

/// Probe free/total bytes on the volume holding `dir` (falling back to its
/// parent when the dir does not yet exist). Returns zeros on failure.
pub(crate) fn probe_free_space(dir: &Path) -> (u64, u64) {
    let probe = if fs::metadata(dir).is_ok() {
        dir.to_path_buf()
    } else if let Some(parent) = dir.parent() {
        parent.to_path_buf()
    } else {
        return (0, 0);
    };
    let total = fs4::total_space(&probe).unwrap_or(0);
    let free = fs4::free_space(&probe).unwrap_or(0);
    (free, total)
}

/// Boolean latch with hysteresis-controlled release, shared by both engines'
/// monitors so a low/critical storage condition emits once per episode rather
/// than re-firing every tick near the threshold.
pub(crate) struct LatchedFlag {
    active: bool,
}

impl LatchedFlag {
    pub(crate) const fn new() -> Self {
        Self { active: false }
    }

    /// Advance the latch one tick. `condition` raises it immediately; it only
    /// lowers again once `condition` is false *and* `recovered` holds (callers
    /// pass `value > threshold + margin`). Returns
    /// `(is_active, rose_this_tick)`; an infallible `recovered` reproduces a
    /// plain pass-through of `condition`.
    pub(crate) fn update(&mut self, condition: bool, recovered: bool) -> (bool, bool) {
        let rose = condition && !self.active;
        if condition {
            self.active = true;
        } else if self.active && recovered {
            self.active = false;
        }
        (self.active, rose)
    }
}

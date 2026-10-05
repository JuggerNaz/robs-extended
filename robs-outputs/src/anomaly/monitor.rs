//! Health monitor for the Anomaly Capture engine.
//!
//! A single background thread (spawned by [`spawn`]) that:
//!
//! - probes free/total disk space on the output dir roughly every 2s;
//! - derives `buffering` (recent frame activity) and `clips_busy` (an export is
//!   in flight) and folds them, with the storage snapshot, into the shared
//!   status;
//! - emits `BufferReady` once the rolling ring covers the configured pre-roll
//!   window (and resets when it drops below);
//! - publishes a `StatusUpdated` snapshot each tick so the UI stays live.
//!
//! ffmpeg-death self-heal lives in the worker (broken-pipe detection), not here
//! — keeping the two threads free of cross-synchronization, like the Blackbox
//! monitor.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use robs_core::event::{AnomalyEvent, AnomalyStorageStatus, EventTx, RobsEvent};

use super::config::AnomalyConfig;
use crate::shared::{free_percent, now_ms, probe_free_space, sleep_with_stop, LatchedFlag};

/// How often to probe and publish.
const TICK: Duration = Duration::from_secs(2);
/// A frame is considered "recent" (buffering) if seen within this window.
const BUFFERING_AGE_MS: u64 = 3000;

/// Spawn the monitor thread. Returns immediately with the handle.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    config: Arc<AnomalyConfig>,
    status: Arc<RwLock<robs_core::event::AnomalyStatus>>,
    stop_flag: Arc<AtomicBool>,
    last_frame_ms: Arc<AtomicU64>,
    export_active: Arc<AtomicBool>,
    events: EventTx,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("anomaly-monitor".into())
        .spawn(move || {
            monitor_loop(
                &config,
                &status,
                &stop_flag,
                &last_frame_ms,
                &export_active,
                &events,
            );
        })
        .expect("spawn anomaly-monitor")
}

#[allow(clippy::too_many_arguments)]
fn monitor_loop(
    config: &AnomalyConfig,
    status: &Arc<RwLock<robs_core::event::AnomalyStatus>>,
    stop_flag: &AtomicBool,
    last_frame_ms: &AtomicU64,
    export_active: &AtomicBool,
    events: &EventTx,
) {
    let send = |ev: AnomalyEvent| {
        let _ = events.send(RobsEvent::Anomaly(ev));
    };
    let warn = config.disk_low_warn_percent as f32;
    let mut was_ready = false;
    // Pass-through latch: the anomaly monitor reports the raw condition each
    // tick (no hysteresis margin) rather than latching an episode like the
    // blackbox monitor's StorageLow/StorageCritical events.
    let mut low_flag = LatchedFlag::new();

    while !stop_flag.load(Ordering::SeqCst) {
        let tick_start = Instant::now();

        // --- Disk-space probe ---
        let (free, total) = probe_free_space(&config.output_dir);
        let free_pct = free_percent(free, total);
        let now_low = free_pct <= warn && total > 0;
        let (low, _) = low_flag.update(now_low, true);

        // --- Fold live signals + storage into status, capture buffer fill ---
        let secs_filled;
        {
            let mut s = status.write();
            let last = last_frame_ms.load(Ordering::Acquire);
            s.buffering = last != 0 && now_ms().saturating_sub(last) < BUFFERING_AGE_MS;
            s.clips_busy = if export_active.load(Ordering::Acquire) {
                1
            } else {
                0
            };
            s.storage = AnomalyStorageStatus {
                free_bytes: free,
                total_bytes: total,
                free_percent: free_pct,
                low_warning: low,
            };
            secs_filled = s.buffer_secs_filled;
        }

        // --- BufferReady edge ---
        if secs_filled >= config.pre_roll_secs {
            if !was_ready {
                send(AnomalyEvent::BufferReady { secs_filled });
                was_ready = true;
            }
        } else {
            was_ready = false;
        }

        // --- Publish status snapshot ---
        let snapshot = status.read().clone();
        send(AnomalyEvent::StatusUpdated { status: snapshot });

        sleep_with_stop(TICK.saturating_sub(tick_start.elapsed()), stop_flag);
    }
}

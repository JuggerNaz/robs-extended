//! Storage + health monitor for the Blackbox engine.
//!
//! A single background thread (spawned by [`spawn`]) that:
//!
//! - probes free/total disk space on the sink roughly every second;
//! - flips the shared `disk_critical` flag when free space drops below the
//!   critical threshold (causing [`super::BlackboxEngine::submit_frame`] to
//!   shed frames and the worker to pause segment writes), and clears it once
//!   space recovers;
//! - asks the sink to [`BlackboxSink::reclaim`] old segments when space is low;
//! - emits `StorageLow` / `StorageCritical` / `Stalled` events with hysteresis
//!   so they fire once per episode rather than every tick;
//! - publishes a `StatusUpdated` snapshot each tick so the UI can render a
//!   live storage bar and dropped-frame counter.
//!
//! It deliberately never touches the active ffmpeg segment (that's the worker's
//! job), which keeps the two threads free of cross-synchronization.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use robs_core::event::{BlackboxEvent, BlackboxStatus, EventTx, RobsEvent};

use super::config::BlackboxConfig;
use super::sink::BlackboxSink;
use super::{build_storage_status, now_ms};

/// How often to probe and publish. Short enough to be responsive, long enough
/// that a free-space syscall + dir scan is negligible.
const TICK: Duration = Duration::from_secs(1);
/// When low, try to free at least this many bytes in one reclaim pass.
const RECLAIM_CHUNK: u64 = 512 * 1024 * 1024;
/// Hysteresis: a low/critical condition must clear by this many points before
/// the flag resets, to avoid flapping near the threshold.
const HYSTERESIS: f32 = 2.0;

/// Spawn the monitor thread. Returns immediately with the handle.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    config: Arc<BlackboxConfig>,
    sink: Arc<dyn BlackboxSink>,
    status: Arc<RwLock<BlackboxStatus>>,
    stop_flag: Arc<AtomicBool>,
    disk_critical: Arc<AtomicBool>,
    last_frame_ms: Arc<AtomicU64>,
    events: EventTx,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("blackbox-monitor".into())
        .spawn(move || {
            monitor_loop(
                &config,
                &sink,
                &status,
                &stop_flag,
                &disk_critical,
                &last_frame_ms,
                &events,
            );
        })
        .expect("spawn blackbox-monitor")
}

#[allow(clippy::too_many_arguments)]
fn monitor_loop(
    config: &BlackboxConfig,
    sink: &Arc<dyn BlackboxSink>,
    status: &Arc<RwLock<BlackboxStatus>>,
    stop_flag: &AtomicBool,
    disk_critical: &AtomicBool,
    last_frame_ms: &AtomicU64,
    events: &EventTx,
) {
    let send = |ev: BlackboxEvent| {
        let _ = events.send(RobsEvent::Blackbox(ev));
    };

    let warn = config.disk_low_warn_percent as f32;
    let critical = config.disk_low_critical_percent as f32;
    let stall_secs = config.stall_threshold_secs;

    let mut was_low = false;
    let mut was_critical = false;
    let mut was_stalled = false;
    let mut last_status_emit = Instant::now() - TICK; // force an immediate first publish

    while !stop_flag.load(Ordering::SeqCst) {
        let tick_start = Instant::now();

        // --- Disk-space probe ---
        let s = sink.storage_status();
        let free_pct = s.free_percent();

        // LOW warning (informational; does not pause ingestion).
        let now_low = free_pct <= warn && s.total_bytes > 0;
        if now_low && !was_low {
            send(BlackboxEvent::StorageLow {
                free_bytes: s.free_bytes,
                total_bytes: s.total_bytes,
                free_percent: free_pct,
            });
        }
        if !now_low && was_low && free_pct > warn + HYSTERESIS {
            was_low = false;
        } else if now_low {
            was_low = true;
        }

        // CRITICAL: flip the flag that pauses ingestion + writes.
        let now_critical = free_pct <= critical && s.total_bytes > 0;
        disk_critical.store(now_critical, Ordering::Release);
        if now_critical && !was_critical {
            send(BlackboxEvent::StorageCritical {
                free_bytes: s.free_bytes,
                total_bytes: s.total_bytes,
            });
            // Best-effort: clear space so we can resume ASAP.
            if let Err(e) = sink.reclaim(RECLAIM_CHUNK) {
                send(BlackboxEvent::Error {
                    message: format!("reclaim failed: {e}"),
                });
            }
        }
        if !now_critical && was_critical && free_pct > critical + HYSTERESIS {
            was_critical = false;
        } else if now_critical {
            was_critical = true;
        }

        // --- Capture stall detection ---
        let last = last_frame_ms.load(Ordering::Acquire);
        let capturing = if last == 0 {
            false
        } else {
            let age_secs = now_ms().saturating_sub(last) / 1000;
            if age_secs <= stall_secs {
                true
            } else {
                if !was_stalled {
                    send(BlackboxEvent::Stalled {
                        seconds_idle: age_secs,
                    });
                }
                was_stalled = true;
                false
            }
        };
        if capturing {
            was_stalled = false;
        }

        // --- Publish status snapshot ---
        {
            let mut st = status.write();
            st.capturing = capturing;
            st.storage = build_storage_status(
                s.free_bytes,
                s.total_bytes,
                was_low,
                was_critical,
            );
        }
        if last_status_emit.elapsed() >= TICK {
            let snapshot = status.read().clone();
            send(BlackboxEvent::StatusUpdated {
                status: snapshot,
            });
            last_status_emit = Instant::now();
        }

        // Sleep out the remainder of the tick, but stay responsive to stop.
        let elapsed = tick_start.elapsed();
        let remaining = TICK.saturating_sub(elapsed);
        let mut left = remaining;
        while left > Duration::ZERO {
            if stop_flag.load(Ordering::SeqCst) {
                return;
            }
            let step = left.min(Duration::from_millis(150));
            std::thread::sleep(step);
            left = left.saturating_sub(step);
        }
    }
}

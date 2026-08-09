//! Blackbox Dual Recording Engine.
//!
//! An always-on, background, parallel recorder that captures a separate encode
//! of the active scene independently of the user's Record button. Footage is
//! never lost to an unpressed Record, an accidental pause, a full disk, or a
//! crashed encode:
//!
//! - **independent of main recording** — runs whenever a capture source is
//!   active, with its own ffmpeg subprocess / container / path;
//! - **crash-safe container** — writes Matroska (`.mkv`), playable without
//!   finalization, plus a `.part` marker repaired on next start (see
//!   [`recovery`]);
//! - **disk-safe** — a monitor thread probes free space, reclaims old
//!   segments, and pauses ingestion when the disk is critically full;
//! - **self-healing** — if ffmpeg dies mid-segment, the worker closes the
//!   segment, reports the error, and opens a fresh one.
//!
//! The engine runs on plain std threads (no runtime) so it can live alongside
//! the egui UI without pulling tokio into the hot path. Frames are submitted
//! from the UI thread via [`BlackboxEngine::submit_frame`], which is strictly
//! non-blocking: on a full channel or a disk pause it drops the frame and
//! bumps a counter rather than ever stalling the UI.

pub mod config;
pub mod monitor;
pub mod recovery;
pub mod segment;
pub mod sink;

pub use config::BlackboxConfig;
pub use segment::ActiveSegment;
pub use sink::{BlackboxSink, LocalFileSink, SegmentInfo, StorageStatus};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use flume::{Receiver, Sender, TrySendError};
use parking_lot::RwLock;

use robs_core::event::{BlackboxEvent, BlackboxStatus, BlackboxStorageStatus, EventTx, RobsEvent};

use recovery::recover;
use segment::ActiveSegment as Seg;

/// One raw captured frame handed to the engine.
struct FrameInput {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

/// The Blackbox engine. Owns the worker + monitor threads and the frame
/// channel. Construct with [`BlackboxEngine::new`], then [`start`](Self::start)
/// / [`stop`](Self::stop). `submit_frame` is the only method called per-frame.
pub struct BlackboxEngine {
    config: Arc<BlackboxConfig>,
    sink: Arc<dyn BlackboxSink>,
    status: Arc<RwLock<BlackboxStatus>>,
    events: EventTx,
    /// Frame channel sender, present only while running.
    frame_tx: Option<Sender<FrameInput>>,
    worker: Option<JoinHandle<()>>,
    monitor: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    /// Set by the monitor when the target disk is critically full; checked by
    /// `submit_frame` (drops frames) and the worker (pauses segment writes).
    disk_critical: Arc<AtomicBool>,
    /// Wall-clock ms of the most recent submitted frame (0 = none yet).
    last_frame_ms: Arc<AtomicU64>,
    running: AtomicBool,
}

impl std::fmt::Debug for BlackboxEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlackboxEngine")
            .field("config", &self.config)
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish()
    }
}

impl BlackboxEngine {
    /// Build a stopped engine. Does no I/O until [`start`](Self::start).
    pub fn new(config: BlackboxConfig, sink: Arc<dyn BlackboxSink>, events: EventTx) -> Self {
        Self {
            config: Arc::new(config),
            sink,
            status: Arc::new(RwLock::new(BlackboxStatus::default())),
            events,
            frame_tx: None,
            worker: None,
            monitor: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            disk_critical: Arc::new(AtomicBool::new(false)),
            last_frame_ms: Arc::new(AtomicU64::new(0)),
            running: AtomicBool::new(false),
        }
    }

    /// Whether the engine currently has live worker threads.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Snapshot of engine health for UI display.
    pub fn status(&self) -> BlackboxStatus {
        self.status.read().clone()
    }

    /// Spawn worker + monitor threads and begin capturing. Idempotent: a second
    /// call while running is a no-op. Ensures the output dir exists and runs
    /// crash recovery first.
    pub fn start(&mut self) {
        if self.running.load(Ordering::Relaxed) {
            return;
        }

        // Ensure output dir exists; if we can't create it, report and abort.
        if let Err(e) = sink::ensure_dir(&self.config.output_dir) {
            self.emit(BlackboxEvent::Error {
                message: format!("cannot create blackbox output dir: {e}"),
            });
            return;
        }

        // Repair any segments left unfinished by a previous run before writing
        // new ones into the same dir.
        let _ = recover(&self.config.output_dir, &self.events);

        self.stop_flag.store(false, Ordering::SeqCst);
        self.disk_critical.store(false, Ordering::SeqCst);
        self.last_frame_ms.store(0, Ordering::SeqCst);

        // Bounded channel sized to ~2s of frames so a slow ffmpeg can't make us
        // queue unbounded memory; overflow drops frames (counted, not fatal).
        let capacity = ((self.config.fps * 2.0).ceil() as usize).max(8);
        let (tx, rx) = flume::bounded(capacity);
        self.frame_tx = Some(tx);

        {
            let mut s = self.status.write();
            *s = BlackboxStatus {
                running: true,
                ..Default::default()
            };
        }

        let worker = spawn_worker(
            Arc::clone(&self.config),
            Arc::clone(&self.sink),
            Arc::clone(&self.status),
            rx,
            Arc::clone(&self.stop_flag),
            Arc::clone(&self.disk_critical),
            self.events.clone(),
        );
        let monitor = monitor::spawn(
            Arc::clone(&self.config),
            Arc::clone(&self.sink),
            Arc::clone(&self.status),
            Arc::clone(&self.stop_flag),
            Arc::clone(&self.disk_critical),
            Arc::clone(&self.last_frame_ms),
            self.events.clone(),
        );

        self.worker = Some(worker);
        self.monitor = Some(monitor);
        self.running.store(true, Ordering::SeqCst);
        self.emit(BlackboxEvent::Started);
    }

    /// Signal the worker/monitor to stop, finalize the active segment, and join.
    /// Safe to call repeatedly.
    pub fn stop(&mut self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        self.stop_flag.store(true, Ordering::SeqCst);
        // Drop the sender so the worker's recv unblocks immediately.
        self.frame_tx = None;

        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.monitor.take() {
            let _ = handle.join();
        }

        {
            let mut s = self.status.write();
            s.running = false;
            s.capturing = false;
            s.disk_paused = false;
            s.current_segment_path = None;
        }
        self.emit(BlackboxEvent::Stopped);
    }

    /// Submit a raw BGRA frame captured at `width x height`. Never blocks:
    /// drops the frame and increments `dropped_frames` when the channel is full
    /// or the disk is critically full. No-op when not running.
    pub fn submit_frame(&self, data: &[u8], width: u32, height: u32) {
        if !self.running.load(Ordering::Relaxed) {
            return;
        }

        // Disk full: shed load rather than queue work we can't write.
        if self.disk_critical.load(Ordering::Acquire) {
            let mut s = self.status.write();
            s.dropped_frames = s.dropped_frames.wrapping_add(1);
            return;
        }

        let now = now_ms();
        self.last_frame_ms.store(now, Ordering::Release);

        let Some(tx) = &self.frame_tx else {
            return;
        };
        match tx.try_send(FrameInput {
            data: data.to_vec(),
            width,
            height,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let mut s = self.status.write();
                s.dropped_frames = s.dropped_frames.wrapping_add(1);
            }
            // Disconnected means we're stopping; treat as dropped silently.
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn emit(&self, event: BlackboxEvent) {
        let _ = self.events.send(RobsEvent::Blackbox(event));
    }
}

impl Drop for BlackboxEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Spawn the writer/rotation worker thread.
#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    config: Arc<BlackboxConfig>,
    sink: Arc<dyn BlackboxSink>,
    status: Arc<RwLock<BlackboxStatus>>,
    rx: Receiver<FrameInput>,
    stop_flag: Arc<AtomicBool>,
    disk_critical: Arc<AtomicBool>,
    events: EventTx,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("blackbox-worker".into())
        .spawn(move || {
            worker_loop(&config, &sink, &status, &rx, &stop_flag, &disk_critical, &events);
        })
        .expect("spawn blackbox-worker")
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    config: &BlackboxConfig,
    sink: &Arc<dyn BlackboxSink>,
    status: &Arc<RwLock<BlackboxStatus>>,
    rx: &Receiver<FrameInput>,
    stop_flag: &AtomicBool,
    disk_critical: &AtomicBool,
    events: &EventTx,
) {
    let mut segment_index: u64 = 0;
    let mut active: Option<Seg> = None;
    let mut finalized_bytes: u64 = 0;
    let mut capture_started_at: Option<Instant> = None;

    let send_ev = |ev: BlackboxEvent| {
        let _ = events.send(RobsEvent::Blackbox(ev));
    };
    let set_last_error = |msg: String| {
        status.write().last_error = Some(msg);
    };

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        // Disk-critical pause: finalize whatever we have and idle until the
        // monitor clears the flag. We do NOT open new segments while paused.
        if disk_critical.load(Ordering::Acquire) {
            if let Some(seg) = active.take() {
                finalized_bytes =
                    close_segment(seg, sink, status, finalized_bytes, &send_ev, capture_started_at);
            }
            status.write().disk_paused = true;
            sleep_with_stop(Duration::from_millis(500), stop_flag);
            continue;
        }
        {
            let mut s = status.write();
            if s.disk_paused {
                s.disk_paused = false;
            }
        }

        if active.is_none() {
            // Need a frame to learn the input format before spawning ffmpeg.
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(frame) => {
                    if capture_started_at.is_none() {
                        capture_started_at = Some(Instant::now());
                    }
                    match open_segment(config, sink, segment_index, frame.width, frame.height) {
                        Ok(mut seg) => {
                            mark_segment_open(status, &seg);
                            send_ev(BlackboxEvent::SegmentStarted {
                                path: seg.path().to_string_lossy().into_owned(),
                                index: seg.index(),
                            });
                            if let Err(e) = seg.write_frame(&frame.data) {
                                send_ev(BlackboxEvent::Error {
                                    message: format!("ffmpeg write failed on open: {e}"),
                                });
                                set_last_error(format!("{e}"));
                                // Close the just-opened segment and back off.
                                finalized_bytes = close_segment(
                                    seg, sink, status, finalized_bytes, &send_ev,
                                    capture_started_at,
                                );
                            } else {
                                active = Some(seg);
                            }
                        }
                        Err(e) => {
                            send_ev(BlackboxEvent::Error {
                                message: format!("failed to open segment: {e}"),
                            });
                            set_last_error(format!("{e}"));
                            sleep_with_stop(Duration::from_secs(1), stop_flag);
                        }
                    }
                }
                Err(flume::RecvTimeoutError::Timeout) => continue,
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            // Active segment open: pull the next frame and write or rotate.
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(frame) => {
                    let rotate = {
                        let seg = active.as_ref().expect("active segment");
                        !seg.dims_match(frame.width, frame.height)
                            || seg.elapsed().as_secs() >= config.segment_duration_secs
                            || seg.bytes_in() >= config.segment_size_bytes()
                    };

                    if rotate {
                        let old = active
                            .take()
                            .expect("active segment present in else branch");
                        finalized_bytes =
                            close_segment(old, sink, status, finalized_bytes, &send_ev, capture_started_at);
                        segment_index += 1;
                        match open_segment(config, sink, segment_index, frame.width, frame.height) {
                            Ok(mut seg) => {
                                mark_segment_open(status, &seg);
                                send_ev(BlackboxEvent::SegmentStarted {
                                    path: seg.path().to_string_lossy().into_owned(),
                                    index: seg.index(),
                                });
                                if let Err(e) = seg.write_frame(&frame.data) {
                                    send_ev(BlackboxEvent::Error {
                                        message: format!("ffmpeg write failed after rotate: {e}"),
                                    });
                                    set_last_error(format!("{e}"));
                                } else {
                                    active = Some(seg);
                                }
                            }
                            Err(e) => {
                                send_ev(BlackboxEvent::Error {
                                    message: format!("failed to open segment after rotate: {e}"),
                                });
                                set_last_error(format!("{e}"));
                                sleep_with_stop(Duration::from_secs(1), stop_flag);
                            }
                        }
                    } else if let Err(e) = active
                        .as_mut()
                        .expect("active segment")
                        .write_frame(&frame.data)
                    {
                        // ffmpeg died (broken pipe). Drop the segment, report,
                        // and let the next iteration reopen a fresh one.
                        send_ev(BlackboxEvent::Error {
                            message: format!("ffmpeg died mid-segment: {e}"),
                        });
                        set_last_error(format!("{e}"));
                        let dead = active
                            .take()
                            .expect("active segment present");
                        finalized_bytes =
                            close_segment(dead, sink, status, finalized_bytes, &send_ev, capture_started_at);
                        sleep_with_stop(Duration::from_millis(500), stop_flag);
                    }
                }
                Err(flume::RecvTimeoutError::Timeout) => continue,
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    // Shutdown: finalize the in-flight segment.
    if let Some(seg) = active.take() {
        finalized_bytes =
            close_segment(seg, sink, status, finalized_bytes, &send_ev, capture_started_at);
    }
    {
        let mut s = status.write();
        s.running = false;
        s.capturing = false;
        s.current_segment_path = None;
        s.bytes_written = finalized_bytes;
        if let Some(start) = capture_started_at {
            s.total_duration_ms = s.total_duration_ms.saturating_add(start.elapsed().as_millis() as u64);
        }
    }
}

/// Open a new segment, delegating path selection to the sink.
fn open_segment(
    config: &BlackboxConfig,
    sink: &Arc<dyn BlackboxSink>,
    index: u64,
    width: u32,
    height: u32,
) -> anyhow::Result<Seg> {
    let path = sink.segment_target(index, chrono::Utc::now());
    Seg::open(config, index, width, height, path)
}

/// Record status for a freshly opened segment.
fn mark_segment_open(status: &Arc<RwLock<BlackboxStatus>>, seg: &Seg) {
    let mut s = status.write();
    s.current_segment_index = seg.index();
    s.current_segment_path = Some(seg.path().to_string_lossy().into_owned());
}

/// Finalize a segment: close ffmpeg, clear the crash marker, notify the sink,
/// and update cumulative counters. Returns the updated `finalized_bytes`.
fn close_segment(
    seg: Seg,
    sink: &Arc<dyn BlackboxSink>,
    status: &Arc<RwLock<BlackboxStatus>>,
    mut finalized_bytes: u64,
    send_ev: &impl Fn(BlackboxEvent),
    capture_started_at: Option<Instant>,
) -> u64 {
    let index = seg.index();
    let path = seg.path().to_path_buf();
    match seg.close() {
        Ok((bytes, duration_ms)) => {
            finalized_bytes = finalized_bytes.saturating_add(bytes);
            let info = SegmentInfo {
                path: path.clone(),
                index,
                bytes,
                duration_ms,
            };
            let _ = sink.on_segment_closed(&info);
            send_ev(BlackboxEvent::SegmentClosed {
                path: path.to_string_lossy().into_owned(),
                index,
                bytes,
                duration_ms,
            });
            let mut s = status.write();
            s.segments_written = s.segments_written.saturating_add(1);
            s.bytes_written = finalized_bytes;
            s.current_segment_path = None;
            if let Some(start) = capture_started_at {
                s.total_duration_ms = s.total_duration_ms.saturating_add(duration_ms);
                // Keep the running wall-clock in sync too.
                s.total_duration_ms = s.total_duration_ms.max(start.elapsed().as_millis() as u64);
            }
        }
        Err(e) => {
            send_ev(BlackboxEvent::Error {
                message: format!("failed to finalize segment: {e}"),
            });
            status.write().current_segment_path = None;
        }
    }
    finalized_bytes
}

/// Sleep for `d`, but wake early if `stop_flag` becomes set.
fn sleep_with_stop(d: Duration, stop_flag: &AtomicBool) {
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

/// Re-export so `monitor` can build storage snapshots for status.
pub(crate) fn build_storage_status(
    free_bytes: u64,
    total_bytes: u64,
    warn: bool,
    critical: bool,
) -> BlackboxStorageStatus {
    let free_percent = if total_bytes == 0 {
        0.0
    } else {
        (free_bytes as f64 / total_bytes as f64 * 100.0) as f32
    };
    BlackboxStorageStatus {
        free_bytes,
        total_bytes,
        free_percent,
        low_warning: warn,
        critical,
    }
}

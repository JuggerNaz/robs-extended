//! Short Clip Anomaly Capture engine.
//!
//! An explicitly user-toggled rolling buffer that, on a manual or programmatic
//! trigger ([`AnomalyCaptureEngine::save`]), exports a short **MP4** clip
//! spanning a configurable pre-roll + post-roll window around the trigger —
//! without disrupting the main recording or the Blackbox engine.
//!
//! # How it works
//! - **Rolling scratch segments** — while running, the worker writes short
//!   `.mkv` segments (~`segment_duration_secs`) into `<output_dir>/buffer`. A
//!   [`Ring`] sweeper keeps only roughly the pre-roll window (plus a small
//!   buffer), capped by `max_buffer_bytes`, by deleting the oldest segments.
//! - **Trigger** ([`EngineCmd::Save`]) — the worker closes the in-flight
//!   segment, pins the last `pre_roll_secs` of the ring, copies them into a
//!   per-clip working dir, then keeps capturing for `post_roll_secs`, teeing
//!   each newly finalized segment into the same working dir.
//! - **Export** — when the post-roll window elapses, a dedicated thread runs
//!   `ffmpeg -f concat … -c copy` over the staged copies into the final
//!   `.mp4`, emits [`AnomalyEvent::ClipReady`], and removes the working dir.
//! - **Non-disruption** — [`AnomalyCaptureEngine::submit_frame`] is strictly
//!   non-blocking (bounded channel, drop+count on full), exactly like the
//!   Blackbox tap.
//!
//! The engine runs on plain std threads (no runtime). v1 supports one clip
//! export at a time; a `save()` while one is in flight yields
//! [`AnomalyEvent::ClipBusy`].

pub mod config;
pub mod export;
pub mod monitor;
pub mod ring;
pub mod segment;

pub use config::AnomalyConfig;
pub use ring::{Ring, RingEntry};
pub use segment::ScratchSegment;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use flume::{Receiver, Sender, TrySendError};
use parking_lot::RwLock;

use robs_core::event::{AnomalyEvent, AnomalyStatus, EventTx, RobsEvent};

use segment::ScratchSegment as Seg;

/// One raw captured frame handed to the engine.
struct FrameInput {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

/// Command channel message: request a clip export.
enum EngineCmd {
    Save {
        clip_id: String,
        pre_roll_secs: u64,
        post_roll_secs: u64,
    },
}

/// A clip export in progress: its working dir, the staged segment filenames
/// (oldest-first), and the wall-clock instant post-roll capture ends.
struct ActiveExport {
    clip_id: String,
    work_dir: PathBuf,
    out_path: PathBuf,
    files: Vec<String>,
    next_file_index: u32,
    post_roll_deadline: Instant,
}

/// The Anomaly Capture engine. Owns the worker + monitor threads and the frame
/// / command channels. Construct with [`AnomalyCaptureEngine::new`], then
/// [`start`](Self::start) / [`stop`](Self::stop). `submit_frame` is the only
/// method called per-frame; `save` triggers a clip export.
pub struct AnomalyCaptureEngine {
    config: Arc<AnomalyConfig>,
    status: Arc<RwLock<AnomalyStatus>>,
    events: EventTx,
    frame_tx: Option<Sender<FrameInput>>,
    cmd_tx: Option<Sender<EngineCmd>>,
    worker: Option<JoinHandle<()>>,
    monitor: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    last_frame_ms: Arc<AtomicU64>,
    export_active: Arc<AtomicBool>,
    running: AtomicBool,
    clip_counter: AtomicU64,
}

impl std::fmt::Debug for AnomalyCaptureEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnomalyCaptureEngine")
            .field("config", &self.config)
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish()
    }
}

impl AnomalyCaptureEngine {
    /// Build a stopped engine. Does no I/O until [`start`](Self::start).
    pub fn new(config: AnomalyConfig, events: EventTx) -> Self {
        Self {
            config: Arc::new(config),
            status: Arc::new(RwLock::new(AnomalyStatus::default())),
            events,
            frame_tx: None,
            cmd_tx: None,
            worker: None,
            monitor: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            last_frame_ms: Arc::new(AtomicU64::new(0)),
            export_active: Arc::new(AtomicBool::new(false)),
            running: AtomicBool::new(false),
            clip_counter: AtomicU64::new(0),
        }
    }

    /// Whether the engine currently has live worker threads.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Snapshot of engine health for UI display.
    pub fn status(&self) -> AnomalyStatus {
        self.status.read().clone()
    }

    /// Spawn worker + monitor threads and begin buffering. Idempotent. Ensures
    /// the output + buffer dirs exist first.
    pub fn start(&mut self) {
        if self.running.load(Ordering::Relaxed) {
            return;
        }
        if let Err(e) = fs::create_dir_all(&self.config.output_dir) {
            self.emit(AnomalyEvent::Error {
                message: format!("cannot create anomaly output dir: {e}"),
            });
            return;
        }
        let _ = fs::create_dir_all(self.config.output_dir.join("buffer"));

        self.stop_flag.store(false, Ordering::SeqCst);
        self.export_active.store(false, Ordering::SeqCst);
        self.last_frame_ms.store(0, Ordering::SeqCst);

        // Bounded to ~2s of frames so a slow ffmpeg can't queue unbounded
        // memory; overflow drops frames (counted, not fatal).
        let capacity = ((self.config.fps * 2.0).ceil() as usize).max(8);
        let (ftx, frx) = flume::bounded(capacity);
        let (ctx, crx) = flume::unbounded::<EngineCmd>();
        self.frame_tx = Some(ftx);
        self.cmd_tx = Some(ctx);

        {
            let mut s = self.status.write();
            *s = AnomalyStatus {
                running: true,
                ..Default::default()
            };
        }

        let worker = spawn_worker(
            Arc::clone(&self.config),
            Arc::clone(&self.status),
            frx,
            crx,
            Arc::clone(&self.stop_flag),
            Arc::clone(&self.export_active),
            self.events.clone(),
        );
        let monitor = monitor::spawn(
            Arc::clone(&self.config),
            Arc::clone(&self.status),
            Arc::clone(&self.stop_flag),
            Arc::clone(&self.last_frame_ms),
            Arc::clone(&self.export_active),
            self.events.clone(),
        );

        self.worker = Some(worker);
        self.monitor = Some(monitor);
        self.running.store(true, Ordering::SeqCst);
        self.emit(AnomalyEvent::Started);
    }

    /// Signal the worker/monitor to stop and join. Abandons any in-flight
    /// export (its work dir is removed). Safe to call repeatedly.
    pub fn stop(&mut self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        self.stop_flag.store(true, Ordering::SeqCst);
        self.frame_tx = None;
        self.cmd_tx = None;

        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.monitor.take() {
            let _ = handle.join();
        }

        {
            let mut s = self.status.write();
            s.running = false;
            s.buffering = false;
            s.clips_busy = 0;
        }
        self.emit(AnomalyEvent::Stopped);
    }

    /// Submit a raw BGRA frame captured at `width x height`. Never blocks:
    /// drops the frame and increments `dropped_frames` when the channel is
    /// full. No-op when not running.
    pub fn submit_frame(&self, data: &[u8], width: u32, height: u32) {
        if !self.running.load(Ordering::Relaxed) {
            return;
        }
        self.last_frame_ms.store(now_ms(), Ordering::Release);

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
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Request an anomaly clip spanning `pre_roll_secs` before and
    /// `post_roll_secs` after now. Non-blocking: returns the assigned clip id
    /// (and emits [`AnomalyEvent::ClipRequested`]). The worker decides whether
    /// to accept it; a request while an export is in flight yields
    /// [`AnomalyEvent::ClipBusy`]. Returns `None` when the engine isn't
    /// running.
    pub fn save(&self, pre_roll_secs: u64, post_roll_secs: u64) -> Option<String> {
        if !self.running.load(Ordering::Relaxed) {
            return None;
        }
        let seq = self.clip_counter.fetch_add(1, Ordering::Relaxed);
        let clip_id = format!("{:08}", seq);
        self.emit(AnomalyEvent::ClipRequested {
            clip_id: clip_id.clone(),
        });
        if let Some(tx) = &self.cmd_tx {
            match tx.send(EngineCmd::Save {
                clip_id: clip_id.clone(),
                pre_roll_secs,
                post_roll_secs,
            }) {
                Ok(()) => Some(clip_id),
                Err(_) => {
                    self.emit(AnomalyEvent::ClipFailed {
                        clip_id,
                        message: "buffer not running".into(),
                    });
                    None
                }
            }
        } else {
            None
        }
    }

    fn emit(&self, event: AnomalyEvent) {
        let _ = self.events.send(RobsEvent::Anomaly(event));
    }
}

impl Drop for AnomalyCaptureEngine {
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

/// Spawn the buffer/rotation/export worker thread.
#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    config: Arc<AnomalyConfig>,
    status: Arc<RwLock<AnomalyStatus>>,
    rx: Receiver<FrameInput>,
    cmd_rx: Receiver<EngineCmd>,
    stop_flag: Arc<AtomicBool>,
    export_active: Arc<AtomicBool>,
    events: EventTx,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("anomaly-worker".into())
        .spawn(move || {
            worker_loop(&config, &status, &rx, &cmd_rx, &stop_flag, &export_active, &events);
        })
        .expect("spawn anomaly-worker")
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    config: &AnomalyConfig,
    status: &Arc<RwLock<AnomalyStatus>>,
    rx: &Receiver<FrameInput>,
    cmd_rx: &Receiver<EngineCmd>,
    stop_flag: &AtomicBool,
    export_active: &AtomicBool,
    events: &EventTx,
) {
    let buffer_dir = config.output_dir.join("buffer");
    // Keep a little more than the pre-roll window so pin_pre_roll can always
    // find enough (the byte cap is enforced independently by sweep).
    let keep_secs = config
        .pre_roll_secs
        .saturating_add(config.segment_duration_secs.saturating_mul(2));

    let mut segment_index: u64 = 0;
    let mut active: Option<Seg> = None;
    let mut ring = Ring::new(config.max_buffer_bytes);
    let mut active_export: Option<ActiveExport> = None;

    let send_ev = |ev: AnomalyEvent| {
        let _ = events.send(RobsEvent::Anomaly(ev));
    };
    let set_last_error = |msg: String| {
        status.write().last_error = Some(msg);
    };

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        // --- Drain trigger commands ---
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                EngineCmd::Save {
                    clip_id,
                    pre_roll_secs,
                    post_roll_secs,
                } => {
                    if active_export.is_some() {
                        send_ev(AnomalyEvent::ClipBusy { clip_id });
                        continue;
                    }
                    // Close the in-flight segment so its footage is available as
                    // pre-roll (active_export is None here, so no copy occurs).
                    if let Some(seg) = active.take() {
                        register_segment(
                            seg,
                            &mut ring,
                            &mut active_export,
                            status,
                            keep_secs,
                            &send_ev,
                        );
                    }
                    // Stage pre-roll copies in a per-clip working dir.
                    let pre_paths = ring.pin_pre_roll(pre_roll_secs);
                    let work_dir = config.output_dir.join(format!("_clip_{}", clip_id));
                    let _ = fs::create_dir_all(&work_dir);
                    let mut files: Vec<String> = Vec::with_capacity(pre_paths.len());
                    for p in &pre_paths {
                        let name = format!("seg_{:04}.mkv", files.len());
                        if fs::copy(p, work_dir.join(&name)).is_ok() {
                            files.push(name);
                        }
                    }
                    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
                    let clip_name = format!(
                        "{}_{}_{}{}.mp4",
                        config.clip_prefix, ts, clip_id, config.clip_suffix
                    );
                    let next_file_index = files.len() as u32;
                    let out_path = config.output_dir.join(clip_name);
                    active_export = Some(ActiveExport {
                        clip_id: clip_id.clone(),
                        work_dir,
                        out_path,
                        files,
                        next_file_index,
                        post_roll_deadline: Instant::now()
                            + Duration::from_secs(post_roll_secs),
                    });
                    export_active.store(true, Ordering::Release);
                }
            }
        }

        // --- Pull the next frame (short timeout keeps commands/deadlines live) ---
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(frame) => {
                let rotate = match active.as_ref() {
                    None => true,
                    Some(seg) => {
                        !seg.dims_match(frame.width, frame.height)
                            || seg.elapsed().as_secs() >= config.segment_duration_secs
                    }
                };
                if rotate {
                    if let Some(seg) = active.take() {
                        register_segment(
                            seg,
                            &mut ring,
                            &mut active_export,
                            status,
                            keep_secs,
                            &send_ev,
                        );
                    }
                    let path = buffer_dir.join(format!("seg_{:06}.mkv", segment_index));
                    match Seg::open(config, segment_index, frame.width, frame.height, path) {
                        Ok(mut seg) => {
                            if let Err(e) = seg.write_frame(&frame.data) {
                                send_ev(AnomalyEvent::Error {
                                    message: format!("ffmpeg write failed on open: {e}"),
                                });
                                set_last_error(format!("{e}"));
                            } else {
                                active = Some(seg);
                            }
                        }
                        Err(e) => {
                            send_ev(AnomalyEvent::Error {
                                message: format!("failed to open anomaly segment: {e}"),
                            });
                            set_last_error(format!("{e}"));
                            sleep_with_stop(Duration::from_secs(1), stop_flag);
                        }
                    }
                    segment_index = segment_index.wrapping_add(1);
                } else {
                    // Write into the active segment; heal a dead ffmpeg by
                    // finalizing the segment and letting the next frame reopen.
                    let write_err = active
                        .as_mut()
                        .and_then(|seg| seg.write_frame(&frame.data).err());
                    if let Some(e) = write_err {
                        send_ev(AnomalyEvent::Error {
                            message: format!("ffmpeg died mid-segment: {e}"),
                        });
                        set_last_error(format!("{e}"));
                        let dead = active
                            .take()
                            .expect("active segment present in write path");
                        register_segment(
                            dead,
                            &mut ring,
                            &mut active_export,
                            status,
                            keep_secs,
                            &send_ev,
                        );
                    }
                }
            }
            Err(flume::RecvTimeoutError::Timeout) => {}
            Err(flume::RecvTimeoutError::Disconnected) => break,
        }

        // --- Finalize an in-flight export when its post-roll window elapses ---
        let due = match active_export.as_ref() {
            Some(exp) => Instant::now() >= exp.post_roll_deadline,
            None => false,
        };
        if due {
            let mut exp_taken = active_export
                .take()
                .expect("active_export present in deadline check");
            // Fold the current post-roll segment into this export explicitly
            // (register_segment would route it to active_export, which is None).
            if let Some(seg) = active.take() {
                let index = seg.index();
                let started_utc = seg.started_utc();
                let spath = seg.path().to_path_buf();
                match seg.close() {
                    Ok((bytes, duration_ms)) => {
                        ring.push(RingEntry {
                            path: spath.clone(),
                            index,
                            started_utc,
                            duration_ms,
                            bytes,
                        });
                        ring.sweep(keep_secs);
                        if let Err(e) = copy_into_export(&mut exp_taken, &spath) {
                            send_ev(AnomalyEvent::Error {
                                message: format!("copy final post-roll segment: {e}"),
                            });
                        }
                    }
                    Err(e) => {
                        send_ev(AnomalyEvent::Error {
                            message: format!("finalize post-roll segment: {e}"),
                        });
                    }
                }
                status.write().buffer_secs_filled = ring.secs_filled();
            }
            export_active.store(false, Ordering::Release);
            spawn_export(exp_taken, Arc::clone(status), events.clone());
        }
    }

    // Shutdown: the in-flight segment is disposable (no finalize). Abandon any
    // in-progress export and clean its work dir.
    active.take();
    if let Some(exp) = active_export.take() {
        let _ = fs::remove_dir_all(&exp.work_dir);
    }
    {
        let mut s = status.write();
        s.buffering = false;
        s.clips_busy = 0;
        s.buffer_secs_filled = ring.secs_filled();
    }
}

/// Finalize a segment, register it in the ring, sweep, and — if an export is
/// in flight — copy it into that export's working dir. Updates
/// `buffer_secs_filled`.
fn register_segment(
    seg: Seg,
    ring: &mut Ring,
    active_export: &mut Option<ActiveExport>,
    status: &Arc<RwLock<AnomalyStatus>>,
    keep_secs: u64,
    send_ev: &impl Fn(AnomalyEvent),
) {
    let index = seg.index();
    let started_utc = seg.started_utc();
    let spath = seg.path().to_path_buf();
    match seg.close() {
        Ok((bytes, duration_ms)) => {
            ring.push(RingEntry {
                path: spath.clone(),
                index,
                started_utc,
                duration_ms,
                bytes,
            });
            ring.sweep(keep_secs);
            if let Some(exp) = active_export.as_mut() {
                if let Err(e) = copy_into_export(exp, &spath) {
                    send_ev(AnomalyEvent::Error {
                        message: format!("copy post-roll segment: {e}"),
                    });
                }
            }
        }
        Err(e) => {
            send_ev(AnomalyEvent::Error {
                message: format!("finalize segment: {e}"),
            });
        }
    }
    status.write().buffer_secs_filled = ring.secs_filled();
}

/// Copy a finalized segment into an export's working dir under the next
/// sequential name and record that name.
fn copy_into_export(exp: &mut ActiveExport, src: &Path) -> anyhow::Result<()> {
    let name = format!("seg_{:04}.mkv", exp.next_file_index);
    exp.next_file_index = exp.next_file_index.wrapping_add(1);
    let dst = exp.work_dir.join(&name);
    fs::copy(src, &dst).map_err(|e| anyhow::anyhow!("copy {src:?} -> {dst:?}: {e}"))?;
    exp.files.push(name);
    Ok(())
}

/// Run the concat export on its own thread, emit ClipReady/ClipFailed, bump the
/// clip counter, and clean the working dir.
fn spawn_export(exp: ActiveExport, status: Arc<RwLock<AnomalyStatus>>, events: EventTx) {
    let _ = std::thread::Builder::new()
        .name("anomaly-export".into())
        .spawn(move || {
            let res = export::export_clip(&exp.work_dir, &exp.files, &exp.out_path);
            match res {
                Ok(()) => {
                    let path_str = exp.out_path.to_string_lossy().into_owned();
                    {
                        let mut s = status.write();
                        s.clips_exported = s.clips_exported.wrapping_add(1);
                        s.last_clip_path = Some(path_str.clone());
                    }
                    let _ = events.send(RobsEvent::Anomaly(AnomalyEvent::ClipReady {
                        clip_id: exp.clip_id.clone(),
                        path: path_str,
                    }));
                }
                Err(e) => {
                    let _ = events.send(RobsEvent::Anomaly(AnomalyEvent::ClipFailed {
                        clip_id: exp.clip_id.clone(),
                        message: format!("{e}"),
                    }));
                }
            }
            let _ = fs::remove_dir_all(&exp.work_dir);
        });
}

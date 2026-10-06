//! The single parameterized worker frame-pump loop shared by the Blackbox and
//! Anomaly engines.
//!
//! Both engines' workers used to be hand-rolled loops with the same skeleton:
//! check the stop flag, honor a pause gate, drain engine commands, pull a
//! frame, decide rotate-vs-write, open/write/finalize segments, run a
//! post-frame step, and finalize (or deliberately abandon) state on shutdown.
//! [`frame_pump`] is that skeleton, exactly once; every point where the two
//! engines deliberately diverge is a [`FramePumpPolicy`] hook:
//!
//! - pause gate (`should_pause` / `on_pause` / `on_active_tick`) — the
//!   blackbox disk-critical pause;
//! - command drain (`pre_recv`) — the anomaly `save` trigger channel;
//! - rotation predicate (`should_rotate`) — blackbox also rotates on a raw
//!   byte budget;
//! - segment lifecycle (`open_segment` / `on_segment_opened` /
//!   `finalize_segment`) — sink-driven paths + `SegmentStarted` events for
//!   blackbox, fixed-name buffer paths for anomaly;
//! - error handling (`on_open_error` / `on_open_write_error` /
//!   `on_segment_died`) — identical self-heal flow, engine-specific event
//!   wording and accounting;
//! - post-frame step (`post_frame`) — the anomaly export-deadline check;
//! - shutdown (`shutdown`) — blackbox finalizes the in-flight segment,
//!   anomaly deliberately abandons it.
//!
//! Timing, event ordering, and status transitions are preserved 1:1 from the
//! original per-engine loops.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use flume::Receiver;

use super::segment::SegmentOps;
use super::{sleep_with_stop, FrameInput};

/// How long the pump idles while paused between re-checks of the pause gate.
const PAUSE_POLL: Duration = Duration::from_millis(500);
/// Backoff after a failed segment open, so a persistently broken sink/ffmpeg
/// can't spin the worker.
const OPEN_BACKOFF: Duration = Duration::from_secs(1);

/// Engine-specific behavior for [`frame_pump`]. `Seg` is the engine's segment
/// type ([`SegmentOps`] implementor).
pub(crate) trait FramePumpPolicy {
    type Seg: SegmentOps;

    /// Frame-channel receive timeout. `has_active` distinguishes the
    /// idle-open wait from the active-segment wait.
    fn recv_timeout(&self, has_active: bool) -> Duration;

    /// Whether this frame must start a new segment: `None` means no segment
    /// is open (idle-open), `Some` lets the engine apply its rotation
    /// predicate (dimension change, duration, byte budget, ...).
    fn should_rotate(&self, active: Option<&Self::Seg>, frame: &FrameInput) -> bool;

    /// Open a segment for this frame. `replacing` is true when an open
    /// segment was just finalized for the rotation (vs. an idle-open).
    fn open_segment(&mut self, frame: &FrameInput, replacing: bool) -> anyhow::Result<Self::Seg>;

    /// A segment opened successfully, before its first frame is written
    /// (blackbox marks status + emits `SegmentStarted` here).
    fn on_segment_opened(&mut self, seg: &Self::Seg);

    /// The first-frame write on a freshly opened segment failed. Engines own
    /// the wording and whether the just-opened segment is finalized
    /// (blackbox idle-open) or dropped (blackbox rotate, anomaly).
    fn on_open_write_error(&mut self, seg: Self::Seg, err: anyhow::Error, replacing: bool);

    /// Opening a segment failed (after the pump's backoff sleep).
    fn on_open_error(&mut self, err: anyhow::Error, replacing: bool);

    /// Finalize a segment with full engine accounting (sink hook / ring
    /// registration, events, status counters).
    fn finalize_segment(&mut self, seg: Self::Seg);

    /// The active segment's write failed mid-stream (ffmpeg died): report,
    /// finalize, and let the next frame reopen a fresh segment.
    fn on_segment_died(&mut self, seg: Self::Seg, err: anyhow::Error);

    /// Pause gate: when true, the pump finalizes the in-flight segment,
    /// calls [`on_pause`](Self::on_pause), and idles. (Blackbox disk-critical.)
    fn should_pause(&self) -> bool {
        false
    }

    /// Entered the pause path (after the in-flight segment was finalized).
    fn on_pause(&mut self) {}

    /// Each non-paused tick (blackbox clears its `disk_paused` status flag).
    fn on_active_tick(&mut self) {}

    /// Before each frame receive: drain engine commands. May finalize the
    /// in-flight segment (anomaly `save` closes it as extra pre-roll).
    fn pre_recv(&mut self, _active: &mut Option<Self::Seg>) {}

    /// After each frame or timeout tick (anomaly export-deadline check).
    fn post_frame(&mut self, _active: &mut Option<Self::Seg>) {}

    /// Worker shutdown, with whatever segment was still open. Blackbox
    /// finalizes it; anomaly abandons it (scratch segments are disposable).
    fn shutdown(&mut self, active: Option<Self::Seg>);
}

/// The unified worker loop. Runs until the channel disconnects (engine
/// stopped) or `stop_flag` is set, then hands the still-open segment, if any,
/// to [`FramePumpPolicy::shutdown`].
pub(crate) fn frame_pump<P: FramePumpPolicy>(
    mut policy: P,
    rx: &Receiver<FrameInput>,
    stop_flag: &AtomicBool,
) {
    let mut active: Option<P::Seg> = None;
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        // Pause gate: finalize whatever we have and idle until the gate
        // clears. No new segments are opened while paused.
        if policy.should_pause() {
            if let Some(seg) = active.take() {
                policy.finalize_segment(seg);
            }
            policy.on_pause();
            sleep_with_stop(PAUSE_POLL, stop_flag);
            continue;
        }
        policy.on_active_tick();
        policy.pre_recv(&mut active);

        match rx.recv_timeout(policy.recv_timeout(active.is_some())) {
            Ok(frame) => {
                if policy.should_rotate(active.as_ref(), &frame) {
                    let replacing = active.is_some();
                    if let Some(old) = active.take() {
                        policy.finalize_segment(old);
                    }
                    match policy.open_segment(&frame, replacing) {
                        Ok(mut seg) => {
                            policy.on_segment_opened(&seg);
                            if let Err(e) = seg.write_frame(&frame.data) {
                                policy.on_open_write_error(seg, e, replacing);
                            } else {
                                active = Some(seg);
                            }
                        }
                        Err(e) => {
                            policy.on_open_error(e, replacing);
                            sleep_with_stop(OPEN_BACKOFF, stop_flag);
                        }
                    }
                } else if let Err(e) = active
                    .as_mut()
                    .expect("active segment present in write path")
                    .write_frame(&frame.data)
                {
                    // ffmpeg died (broken pipe). Drop the segment, report, and
                    // let the next iteration reopen a fresh one.
                    let dead = active.take().expect("active segment present in write path");
                    policy.on_segment_died(dead, e);
                }
            }
            // Timeout: fall through to the post-frame step, then re-check the
            // stop/pause gates on the next iteration.
            Err(flume::RecvTimeoutError::Timeout) => {}
            Err(flume::RecvTimeoutError::Disconnected) => break,
        }
        policy.post_frame(&mut active);
    }
    policy.shutdown(active);
}

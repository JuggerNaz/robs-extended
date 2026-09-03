//! Clip marking: zero-overhead "Mark In / Mark Out" during a recording.
//!
//! A mark is just two frame-position integers captured at button-press time.
//! Frames are piped to FFmpeg at a fixed `-framerate` and the wall-clock
//! pacer guarantees one frame per tick interval, so file position =
//! `frame_count / fps`: marks are exact content positions, immune to timer
//! drift and pause (pause gates all frame sends).
//!
//! The clip files are produced AFTER `stop_recording` finalizes the
//! recording: a background thread stream-copies each marked span
//! (`-ss … -i … -t … -c copy`) — lossless, near-instant, no re-encode.
//! Results flow back over an mpsc channel drained in `handle_events`
//! (same pattern as the Anomaly engine).
//!
//! Unlike the Anomaly rolling buffer, marks cannot retro-capture the past
//! and require a live recording; the buffer remains the tool for the
//! no-recording retro case.

use super::state::EventLogKind;
use super::RobsController;

/// One closed mark: frame span `[start, end)` in file-position frames of the
/// current recording session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipMark {
    pub(crate) start_frame: u64,
    pub(crate) end_frame: u64,
}

impl ClipMark {
    pub(crate) fn new(start_frame: u64, end_frame: u64) -> Self {
        Self {
            start_frame,
            end_frame,
        }
    }

    /// Content-time position of the mark start, in seconds.
    pub(crate) fn start_secs(&self, fps: f32) -> f32 {
        self.start_frame as f32 / fps.max(0.001)
    }

    /// Length of the marked span, in seconds. `end > start` whenever a mark
    /// is pushed, so this is always at least one frame's duration.
    pub(crate) fn duration_secs(&self, fps: f32) -> f32 {
        (self.end_frame - self.start_frame) as f32 / fps.max(0.001)
    }
}

/// Outcome of one Mark In/Out toggle; pure so it can be unit-tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkAction {
    /// Mark In: a mark is now open at this frame.
    Opened(u64),
    /// Mark Out: closed at `end`; `pushed` is false when the span was too
    /// short (`end <= start`) and must be discarded.
    Closed { start: u64, end: u64, pushed: bool },
}

/// Pure transition for the `NoMark ⇄ MarkOpen` sub-state inside Recording.
pub(crate) fn apply_mark_toggle(open: Option<u64>, frame_count: u64) -> MarkAction {
    match open {
        None => MarkAction::Opened(frame_count),
        Some(start) => MarkAction::Closed {
            start,
            end: frame_count,
            pushed: frame_count > start,
        },
    }
}

/// FFmpeg args that stream-copy one mark out of a finalized recording.
///
/// `-ss` before `-i` + `-c copy` = fast keyframe seek; the clip start snaps
/// back to the previous keyframe (≤ ~one keyframe interval early — 2s at the
/// default settings). `-t` (not `-to`) because input-side `-ss` resets
/// timestamps.
pub(crate) fn build_clip_args(input: &str, output: &str, mark: &ClipMark, fps: f32) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{:.3}", mark.start_secs(fps)),
        "-i".into(),
        input.to_string(),
        "-t".into(),
        format!("{:.3}", mark.duration_secs(fps)),
    ];
    args.extend(["-c", "copy", "-avoid_negative_ts", "make_zero", "-y"].map(String::from));
    args.push(output.to_string());
    args
}

/// One result per exported clip, sent from the export thread.
pub struct ClipExportResult {
    pub(crate) index: usize,
    pub(crate) path: String,
    pub(crate) ok: bool,
    pub(crate) message: Option<String>,
}

impl RobsController {
    /// Mark In / Mark Out button handler. Requires a live recording; allowed
    /// while paused (the paused position is the resume point, which is still
    /// the correct content time).
    pub fn toggle_clip_mark(&mut self) {
        if !self.record.recording {
            return;
        }
        let fps = self.fps_setting;
        match apply_mark_toggle(self.record.clip_mark_start, self.record.frame_count) {
            MarkAction::Opened(start) => {
                self.record.clip_mark_start = Some(start);
                self.log_event(
                    format!(
                        "Clip mark in at {}",
                        Self::format_time((start as f32 / fps) as u64)
                    ),
                    EventLogKind::Record,
                );
            }
            MarkAction::Closed { start, end, pushed } => {
                self.record.clip_mark_start = None;
                if pushed {
                    self.record.clip_marks.push(ClipMark::new(start, end));
                    let queued = self.record.clip_marks.len();
                    self.log_event(
                        format!(
                            "Clip marked {}\u{2013}{} ({queued} queued)",
                            Self::format_time((start as f32 / fps) as u64),
                            Self::format_time((end as f32 / fps) as u64),
                        ),
                        EventLogKind::Record,
                    );
                } else {
                    self.log_event("Clip mark too short, discarded", EventLogKind::Record);
                }
            }
        }
    }

    /// Hand closed marks to a background thread that stream-copies each span
    /// out of the finalized recording. Called from `stop_recording` (which
    /// already blocks the UI thread — cutting must not happen inline).
    pub(crate) fn start_clip_exports(&mut self, recording_path: String, marks: Vec<ClipMark>, fps: f32) {
        let (tx, rx) = std::sync::mpsc::channel::<ClipExportResult>();
        self.record.clip_export_rx = Some(rx);
        self.record.clip_export_pending = marks.len() as u32;

        std::thread::spawn(move || {
            let input = std::path::Path::new(&recording_path);
            let stem = input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "ROBS_Recording".to_string());
            let ext = input
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "mp4".to_string());
            let dir = input
                .parent()
                .map(|p| p.join("Clips"))
                .unwrap_or_else(|| std::path::PathBuf::from("Clips"));

            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("[Clips] Cannot create {}: {e}", dir.display());
                for (index, _) in marks.iter().enumerate() {
                    let _ = tx.send(ClipExportResult {
                        index,
                        path: String::new(),
                        ok: false,
                        message: Some("failed to create Clips directory".into()),
                    });
                }
                return;
            }

            for (index, mark) in marks.iter().enumerate() {
                let output = dir
                    .join(format!("{stem}_clip_{:02}.{ext}", index + 1))
                    .to_string_lossy()
                    .into_owned();
                let args = build_clip_args(&recording_path, &output, mark, fps);
                eprintln!("[Clips] FFmpeg args: {args:?}");
                let result = match std::process::Command::new("ffmpeg").args(&args).output() {
                    Ok(out) if out.status.success() => ClipExportResult {
                        index,
                        path: output.clone(),
                        ok: true,
                        message: None,
                    },
                    Ok(out) => {
                        let detail: String = String::from_utf8_lossy(&out.stderr)
                            .chars()
                            .take(200)
                            .collect();
                        ClipExportResult {
                            index,
                            path: output.clone(),
                            ok: false,
                            message: Some(format!("ffmpeg exited {}: {detail}", out.status)),
                        }
                    }
                    Err(e) => ClipExportResult {
                        index,
                        path: output.clone(),
                        ok: false,
                        message: Some(format!("failed to spawn ffmpeg: {e}")),
                    },
                };
                let ok = result.ok;
                let _ = tx.send(result);
                eprintln!(
                    "[Clips] clip {} {}",
                    index + 1,
                    if ok { "saved" } else { "FAILED" }
                );
            }
        });
    }

    /// Drain pending export results (mirrors the Anomaly event drain).
    pub(crate) fn drain_clip_export_events(&mut self) -> Vec<ClipExportResult> {
        let Some(rx) = self.record.clip_export_rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(res) = rx.try_recv() {
            out.push(res);
        }
        out
    }

    /// Log each result and release its busy slot.
    pub(crate) fn apply_clip_export_events(&mut self, events: Vec<ClipExportResult>) {
        for ev in events {
            self.record.clip_export_pending = self.record.clip_export_pending.saturating_sub(1);
            if ev.ok {
                self.log_event(format!("Clip saved: {}", ev.path), EventLogKind::Record);
            } else {
                let detail = ev.message.unwrap_or_else(|| "unknown error".to_string());
                self.log_event(
                    format!("Clip export failed ({}): {detail}", ev.index + 1),
                    EventLogKind::Record,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_opens_then_closes_and_pushes() {
        assert_eq!(apply_mark_toggle(None, 120), MarkAction::Opened(120));
        assert_eq!(
            apply_mark_toggle(Some(120), 300),
            MarkAction::Closed {
                start: 120,
                end: 300,
                pushed: true,
            }
        );
    }

    #[test]
    fn toggle_discards_non_positive_span() {
        assert_eq!(
            apply_mark_toggle(Some(300), 300),
            MarkAction::Closed {
                start: 300,
                end: 300,
                pushed: false,
            }
        );
        // frame_count can never move backwards, but the helper must still
        // refuse to push an inverted span.
        assert_eq!(
            apply_mark_toggle(Some(300), 100),
            MarkAction::Closed {
                start: 300,
                end: 100,
                pushed: false,
            }
        );
    }

    #[test]
    fn clip_mark_seconds_math() {
        let m = ClipMark::new(60, 660); // 2s → 22s at 30fps
        assert!((m.start_secs(30.0) - 2.0).abs() < 1e-6);
        assert!((m.duration_secs(30.0) - 20.0).abs() < 1e-6);
        // Degenerate fps must not divide by zero.
        assert!(m.duration_secs(0.0).is_finite());
    }

    #[test]
    fn clip_args_seek_before_input_copy_before_output() {
        let m = ClipMark::new(30, 120);
        let args = build_clip_args("in.mp4", "out.mp4", &m, 30.0);
        let idx = |needle: &str| args.iter().position(|a| a == needle).unwrap();
        assert_eq!(args[idx("-ss") + 1], "1.000"); // 30 frames @ 30fps
        assert_eq!(args[idx("-i") + 1], "in.mp4");
        assert_eq!(args[idx("-t") + 1], "3.000"); // 90 frames @ 30fps
        assert!(idx("-ss") < idx("-i"), "input-side -ss must precede -i");
        assert_eq!(args[idx("-c") + 1], "copy");
        assert_eq!(args.last().unwrap(), "out.mp4");
    }
}

//! Tape discovery + segment merging for the Blackbox recorder.
//!
//! A "tape" is one manual tape number attached to a recording session: when a
//! recording starts with a non-empty tape number, every blackbox channel
//! folder under `<session>/Blackbox/<Camera>/` receives a static `tape.json`
//! manifest (written by the blackbox sync path as channel engines start).
//!
//! Merging turns one camera's tape segments into a single Matroska file via
//! the ffmpeg concat demuxer — a lossless stream copy, near-instant even for
//! hours of footage. Each merge runs on its own background std thread and
//! reports over an mpsc channel drained in `handle_events`: the exact pattern
//! the clip exporter (`clips.rs`) uses.
//!
//! One entry per (tape, camera) pair satisfies both client bullets: "merge
//! all 15-minute segments into one video based on Tape No." and "Merge Video
//! by Camera for each Tape No." — the "Merge All Cameras" action just starts
//! one merge job per channel of the tape.

use std::path::{Path, PathBuf};

use super::record::RecordService;
use super::state::EventLogKind;
use super::RobsController;

/// One mergeable (tape, camera) pair discovered on disk.
#[derive(Debug, Clone)]
pub struct TapeEntry {
    pub tape_no: String,
    pub camera: String,
    /// `<base>/<date>/recording_N/Blackbox/<Camera>` folder holding the segments.
    pub channel_dir: PathBuf,
    pub segment_count: usize,
    pub total_bytes: u64,
}

impl TapeEntry {
    /// Compact UI label for the merge ComboBox:
    /// `Tape 1042 — Main — 23_09_2026/recording_1 — 3 segs, 1234.5 MB`.
    pub fn label(&self) -> String {
        let session = self
            .channel_dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let date = self
            .channel_dir
            .parent()
            .and_then(Path::parent)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mib = self.total_bytes as f64 / 1_048_576.0;
        format!(
            "Tape {} \u{2014} {} \u{2014} {}/{} \u{2014} {} segs, {:.1} MB",
            self.tape_no, self.camera, date, session, self.segment_count, mib
        )
    }
}

/// Outcome of one merge job, sent from the merge thread.
pub struct MergeResult {
    pub tape_no: String,
    pub camera: String,
    pub segments: usize,
    pub path: String,
    pub ok: bool,
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Tape manifests
// ---------------------------------------------------------------------------

/// The static per-channel manifest written when a channel's engine starts
/// under a recording session with a tape number. Pure so it can be tested.
pub fn build_tape_manifest(
    tape_no: &str,
    camera: &str,
    session: &str,
    started_at: chrono::DateTime<chrono::Local>,
) -> String {
    serde_json::json!({
        "tape_no": tape_no,
        "camera": camera,
        "session": session,
        "started_at": started_at.to_rfc3339(),
    })
    .to_string()
}

/// Parse a `tape.json` manifest into (tape_no, camera). `None` on malformed
/// content — a folder without a manifest is simply not a tape.
pub(crate) fn parse_tape_manifest(text: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let tape_no = value.get("tape_no")?.as_str()?.to_string();
    let camera = value.get("camera")?.as_str()?.to_string();
    Some((tape_no, camera))
}

// ---------------------------------------------------------------------------
// Segment enumeration
// ---------------------------------------------------------------------------

/// Order pre-collected `(path, bytes, mtime)` entries oldest-first. Pure so
/// the mtime (not filename) ordering can be tested without touching the
/// filesystem clock.
fn order_segments_oldest_first(
    mut entries: Vec<(PathBuf, u64, std::time::SystemTime)>,
) -> Vec<(PathBuf, u64)> {
    entries.sort_by_key(|(_, _, mtime)| *mtime);
    entries.into_iter().map(|(path, bytes, _)| (path, bytes)).collect()
}

/// Finalized blackbox segments in `channel_dir`, oldest-first, paired with
/// their sizes. In-flight segments (those with a `.part` crash-marker
/// sibling, see `robs-outputs::blackbox::segment::part_marker`) are skipped.
///
/// Ordering is by modified time, NOT filename: the segment index resets to 0
/// whenever the engine restarts mid-session (capture drop, toggle), so
/// `blackbox_0000_…` from a later run would sort before `blackbox_0001_…`
/// from an earlier one. mtime = finalization time = chronological.
pub fn segment_files(channel_dir: &Path) -> Vec<(PathBuf, u64)> {
    let read = match std::fs::read_dir(channel_dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mkv") {
            continue;
        }
        // A `.part` sibling means the segment may not be finalized yet.
        let mut marker = path.file_name().unwrap_or_default().to_os_string();
        marker.push(".part");
        if channel_dir.join(marker).exists() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        entries.push((path, meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH)));
    }
    order_segments_oldest_first(entries)
}

/// Scan the recording base for mergeable tapes: `<base>/<date>/recording_N/
/// Blackbox/<Camera>/tape.json`. Folders without a manifest (standalone
/// blackbox output, untaped sessions) are not tapes and are skipped. The
/// result is sorted by (tape_no, camera) so UI indexes stay stable.
pub fn discover_tapes(base_dir: &Path) -> Vec<TapeEntry> {
    let mut tapes: Vec<TapeEntry> = Vec::new();
    let Ok(dates) = std::fs::read_dir(base_dir) else {
        return tapes;
    };
    for date in dates.flatten() {
        let date_path = date.path();
        if !date_path.is_dir() {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(&date_path) else {
            continue;
        };
        for session in sessions.flatten() {
            let session_path = session.path();
            if !session_path.is_dir() {
                continue;
            }
            let blackbox_root = session_path.join("Blackbox");
            let Ok(channels) = std::fs::read_dir(&blackbox_root) else {
                continue;
            };
            for channel in channels.flatten() {
                let channel_dir = channel.path();
                if !channel_dir.is_dir() {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(channel_dir.join("tape.json")) else {
                    continue;
                };
                let Some((tape_no, camera)) = parse_tape_manifest(&text) else {
                    continue;
                };
                let segments = segment_files(&channel_dir);
                if segments.is_empty() {
                    continue;
                }
                tapes.push(TapeEntry {
                    tape_no,
                    camera,
                    channel_dir,
                    segment_count: segments.len(),
                    total_bytes: segments.iter().map(|(_, bytes)| *bytes).sum(),
                });
            }
        }
    }
    tapes.sort_by(|a, b| a.tape_no.cmp(&b.tape_no).then(a.camera.cmp(&b.camera)));
    tapes
}

// ---------------------------------------------------------------------------
// FFmpeg concat
// ---------------------------------------------------------------------------

/// Concat-demuxer list file content for `segments`, in order. Paths are
/// single-quoted; embedded single quotes are doubled (ffmpeg's escaping).
pub fn build_concat_list(segments: &[PathBuf]) -> String {
    let mut list = String::new();
    for segment in segments {
        let path = segment.to_string_lossy();
        list.push_str(&format!("file '{}'\n", path.replace('\'', "''")));
    }
    list
}

/// FFmpeg args that stream-copy all segments of the concat list into one
/// file — lossless, near-instant, no re-encode.
pub fn build_merge_args(list_path: &str, output: &str) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_path.into(),
        "-c".into(),
        "copy".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-y".into(),
        output.into(),
    ]
}

// ---------------------------------------------------------------------------
// RecordService: merge job plumbing (mirrors the clip exporter)
// ---------------------------------------------------------------------------

impl RecordService {
    /// Spawn one background ffmpeg merge for `entry`. Results arrive on the
    /// shared merge channel drained in `handle_events`; several jobs may run
    /// concurrently (one thread + one ffmpeg each — "Merge All Cameras"
    /// starts one job per channel of the tape).
    pub(crate) fn start_merge(&mut self, entry: TapeEntry) {
        // One shared (tx, rx) pair for the service's lifetime: every job
        // clones the tx, `handle_events` drains the rx.
        if self.merge_result_tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<MergeResult>();
            self.merge_result_tx = Some(tx);
            self.merge_result_rx = Some(rx);
        }
        self.merge_pending += 1;
        let tx = self.merge_result_tx.clone().expect("just created");

        let tape_no = entry.tape_no.clone();
        let camera = entry.camera.clone();
        std::thread::spawn(move || {
            // Merged output goes next to the channel folders:
            // `<session>/Blackbox/merged/Tape_<no>_<Camera>_<stamp>.mkv`.
            let result = run_merge(&entry);
            eprintln!(
                "[Tape] merge tape {tape_no} ({camera}): {}",
                if result.ok {
                    format!("saved to {}", result.path)
                } else {
                    format!("FAILED: {}", result.message.as_deref().unwrap_or("unknown"))
                }
            );
            let _ = tx.send(result);
        });
    }

    /// Drain pending merge results (mirrors the clip-export drain).
    pub(crate) fn drain_merge_events(&mut self) -> Vec<MergeResult> {
        let Some(rx) = self.merge_result_rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(res) = rx.try_recv() {
            out.push(res);
        }
        out
    }

    /// Release each result's busy slot and turn it into an event-log line,
    /// in arrival order for the facade to log.
    pub(crate) fn apply_merge_events(
        &mut self,
        events: Vec<MergeResult>,
    ) -> Vec<(String, EventLogKind)> {
        let mut logs = Vec::new();
        for ev in events {
            self.merge_pending = self.merge_pending.saturating_sub(1);
            if ev.ok {
                logs.push((
                    format!(
                        "Merged tape {} ({}): {} segments -> {}",
                        ev.tape_no, ev.camera, ev.segments, ev.path
                    ),
                    EventLogKind::Record,
                ));
            } else {
                let detail = ev.message.unwrap_or_else(|| "unknown error".to_string());
                logs.push((
                    format!("Tape merge failed {} ({}): {detail}", ev.tape_no, ev.camera),
                    EventLogKind::Record,
                ));
            }
        }
        logs
    }
}

/// Body of one merge job: enumerate the channel's segments, write the concat
/// list next to the output, and stream-copy them into one file. Returns the
/// result for the mpsc channel (kept separate from the thread spawn so it
/// stays testable).
fn run_merge(entry: &TapeEntry) -> MergeResult {
    let fail = |message: String| MergeResult {
        tape_no: entry.tape_no.clone(),
        camera: entry.camera.clone(),
        segments: 0,
        path: String::new(),
        ok: false,
        message: Some(message),
    };

    let Some(blackbox_root) = entry.channel_dir.parent().map(Path::to_path_buf) else {
        return fail("channel folder has no parent".into());
    };
    let out_dir = blackbox_root.join("merged");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        return fail(format!("cannot create {}: {e}", out_dir.display()));
    }

    let segments: Vec<PathBuf> = segment_files(&entry.channel_dir)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    if segments.is_empty() {
        return fail("no finalized segments found".into());
    }

    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let output = out_dir.join(format!(
        "Tape_{}_{}_{}.mkv",
        entry.tape_no, entry.camera, stamp
    ));
    let list_path = out_dir.join(format!(
        "Tape_{}_{}_{}_concat.txt",
        entry.tape_no, entry.camera, stamp
    ));
    if let Err(e) = std::fs::write(&list_path, build_concat_list(&segments)) {
        return fail(format!("cannot write concat list: {e}"));
    }

    let args = build_merge_args(
        &list_path.to_string_lossy(),
        &output.to_string_lossy(),
    );
    eprintln!("[Tape] ffmpeg args: {args:?}");
    let result = match std::process::Command::new("ffmpeg").args(&args).output() {
        Ok(out) if out.status.success() => MergeResult {
            tape_no: entry.tape_no.clone(),
            camera: entry.camera.clone(),
            segments: segments.len(),
            path: output.to_string_lossy().into_owned(),
            ok: true,
            message: None,
        },
        Ok(out) => {
            let detail: String = String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(200)
                .collect();
            fail(format!("ffmpeg exited {}: {detail}", out.status))
        }
        Err(e) => fail(format!("failed to spawn ffmpeg: {e}")),
    };

    // The temp concat list has served its purpose (best-effort removal).
    let _ = std::fs::remove_file(&list_path);
    result
}

// ---------------------------------------------------------------------------
// Controller delegates
// ---------------------------------------------------------------------------

impl RobsController {
    /// Recording base directory (settings path or the default videos folder)
    /// — the same resolution `start_recording` uses for session folders.
    pub(crate) fn recording_base(&self) -> PathBuf {
        if self.recording_path.is_empty() {
            PathBuf::from(super::default_videos_dir())
        } else {
            PathBuf::from(&self.recording_path)
        }
    }

    /// Discover mergeable tapes under the recording base directory.
    pub fn list_tapes(&self) -> Vec<TapeEntry> {
        discover_tapes(&self.recording_base())
    }

    /// Merge one (tape, camera) entry in the background.
    pub fn merge_tape(&mut self, entry: TapeEntry) {
        self.record.start_merge(entry);
    }

    /// Merge every camera channel of `tape_no` (one background job each).
    pub fn merge_tape_all(&mut self, tape_no: &str) {
        for entry in self.list_tapes() {
            if entry.tape_no == tape_no {
                self.record.start_merge(entry);
            }
        }
    }

    /// Drain pending merge results (delegates to
    /// [`RecordService::drain_merge_events`]).
    pub(crate) fn drain_merge_events(&mut self) -> Vec<MergeResult> {
        self.record.drain_merge_events()
    }

    /// Log each result and release its busy slot (delegates to
    /// [`RecordService::apply_merge_events`]).
    pub(crate) fn apply_merge_events(&mut self, events: Vec<MergeResult>) {
        for (message, kind) in self.record.apply_merge_events(events) {
            self.log_event(message, kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "robs-merge-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn manifest(channel_dir: &Path, tape_no: &str, camera: &str) {
        write(
            &channel_dir.join("tape.json"),
            build_tape_manifest(
                tape_no,
                camera,
                "recording_1",
                chrono::Local::now(),
            )
            .as_bytes(),
        );
    }

    #[test]
    fn manifest_roundtrips_tape_and_camera() {
        let text = build_tape_manifest("1042", "Main", "recording_1", chrono::Local::now());
        assert_eq!(
            parse_tape_manifest(&text),
            Some(("1042".to_string(), "Main".to_string()))
        );
        // Malformed content and wrong shapes yield None, not a panic.
        assert_eq!(parse_tape_manifest("not json"), None);
        assert_eq!(parse_tape_manifest("{\"tape_no\": 7}"), None);
    }

    #[test]
    fn concat_list_quotes_paths_and_doubles_inner_quotes() {
        let list = build_concat_list(&[
            PathBuf::from(r"C:\vids\main\blackbox_0000_a.mkv"),
            PathBuf::from("/tmp/it's/seg.mkv"),
        ]);
        let lines: Vec<&str> = list.lines().collect();
        assert_eq!(lines[0], r"file 'C:\vids\main\blackbox_0000_a.mkv'");
        assert_eq!(lines[1], "file '/tmp/it''s/seg.mkv'");
    }

    #[test]
    fn merge_args_concat_copy_output_last() {
        let args = build_merge_args("list.txt", "out.mkv");
        let idx = |needle: &str| args.iter().position(|a| a == needle).unwrap();
        assert_eq!(args[idx("-f") + 1], "concat");
        assert_eq!(args[idx("-safe") + 1], "0");
        assert_eq!(args[idx("-i") + 1], "list.txt");
        assert_eq!(args[idx("-c") + 1], "copy");
        assert_eq!(args.last().unwrap(), "out.mkv");
    }

    #[test]
    fn segments_order_by_mtime_not_filename() {
        // A mid-session engine restart resets the index to 0000, so a
        // filename sort would put the LATER run's `0000` before the earlier
        // run's `0001`. mtime ordering must not.
        let now = std::time::SystemTime::now();
        let earlier = now - std::time::Duration::from_secs(60);
        let ordered = order_segments_oldest_first(vec![
            (PathBuf::from("blackbox_0000_late.mkv"), 1, now),
            (PathBuf::from("blackbox_0001_early.mkv"), 2, earlier),
        ]);
        assert_eq!(
            ordered.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("blackbox_0001_early.mkv"),
                PathBuf::from("blackbox_0000_late.mkv"),
            ]
        );
    }

    #[test]
    fn segment_files_skips_part_markers_and_non_segments() {
        let dir = temp_dir("segments");
        write(&dir.join("blackbox_0000_a.mkv"), b"a");
        write(&dir.join("blackbox_0001_b.mkv"), b"bb");
        write(&dir.join("blackbox_0001_b.mkv.part"), b""); // in flight
        write(&dir.join("blackbox_0002_c.mkv.part"), b""); // marker only
        write(&dir.join("notes.txt"), b"skip"); // not a segment

        let found: Vec<PathBuf> = segment_files(&dir)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(found, vec![dir.join("blackbox_0000_a.mkv")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_finds_manifest_channels_across_dates_and_sessions() {
        let base = temp_dir("discover");
        let cam_a = base.join("23_09_2026").join("recording_1").join("Blackbox").join("Main");
        let cam_b = base.join("23_09_2026").join("recording_2").join("Blackbox").join("Port");
        let cam_c = base.join("24_09_2026").join("recording_1").join("Blackbox").join("Starboard");
        manifest(&cam_a, "1042", "Main");
        write(&cam_a.join("blackbox_0000_a.mkv"), b"aaa");
        write(&cam_a.join("blackbox_0001_a.mkv"), b"aaaa");
        manifest(&cam_b, "1042", "Port");
        write(&cam_b.join("blackbox_0000_b.mkv"), b"b");
        // No manifest -> not a tape.
        write(&cam_c.join("blackbox_0000_c.mkv"), b"c");
        // Untaped stray recording -> ignored entirely.
        let _ = std::fs::create_dir_all(base.join("25_09_2026").join("recording_1").join("Blackbox"));

        let tapes = discover_tapes(&base);
        assert_eq!(tapes.len(), 2, "manifest-less folders are not tapes");
        assert_eq!(tapes[0].tape_no, "1042");
        assert_eq!(tapes[0].camera, "Main");
        assert_eq!(tapes[0].segment_count, 2);
        assert_eq!(tapes[0].total_bytes, 7);
        assert_eq!(tapes[1].camera, "Port");
        assert!(tapes[0].label().contains("1042"));
        assert!(tapes[0].label().contains("Main"));

        let _ = std::fs::remove_dir_all(&base);
    }
}

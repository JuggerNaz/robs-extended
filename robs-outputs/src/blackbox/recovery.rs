//! Crash recovery for the Blackbox engine.
//!
//! On every engine start we scan *only* the blackbox output dir for leftover
//! `.part` markers (see [`super::segment::part_marker`]). A leftover marker
//! means the previous run crashed or was killed before the segment was
//! finalized. MKV is playable even when truncated, but we remux it once to
//! rebuild the index / drop partial trailing packets, then clear the marker so
//! the segment is treated as finalized going forward.
//!
//! This is deliberately scoped to the blackbox directory so it never touches
//! the user's normal recordings.

use std::fs;
use std::path::Path;
use std::process::Command;

use robs_core::event::{BlackboxEvent, RobsEvent};

/// Scan `dir` for `.part` markers and repair the segments they guard. Returns
/// the number of segments successfully remuxed. Each repair is best-effort:
/// a failure is logged via `events` and skipped rather than aborting recovery.
pub fn recover(dir: &Path, events: &robs_core::EventTx) -> usize {
    let entries = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };

    let mut recovered = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("part") {
            continue;
        }

        // The marker is "<segment>.mkv.part"; the segment it guards is the
        // marker path with the trailing ".part" stripped.
        let segment_path = match path.to_str().and_then(|s| s.strip_suffix(".part")) {
            Some(s) => Path::new(s).to_path_buf(),
            None => continue,
        };
        if segment_path.extension().and_then(|e| e.to_str()) != Some("mkv") {
            continue;
        }
        if !segment_path.exists() {
            // Nothing to repair — just clear the stale marker.
            let _ = fs::remove_file(&path);
            continue;
        }

        match remux(&segment_path) {
            Ok(()) => {
                let _ = fs::remove_file(&path);
                let _ = events.send(RobsEvent::Blackbox(BlackboxEvent::Recovered {
                    path: segment_path.to_string_lossy().into_owned(),
                }));
                recovered += 1;
            }
            Err(e) => {
                eprintln!(
                    "[Blackbox] recovery: failed to remux {:?}: {}",
                    segment_path, e
                );
                // Leave the marker so we try again next start; clear it only if
                // the source file is gone to avoid retrying forever on a
                // permanently corrupt file.
            }
        }
    }

    recovered
}

/// Remux `src` into a temp file then atomically replace it. Stream-copy only
/// (no re-encode) so it's fast. `-err_detect ignore_err` lets ffmpeg salvage
/// what it can from a truncated file.
fn remux(src: &Path) -> anyhow::Result<()> {
    let tmp = src.with_extension("mkv.fixtmp");
    let status = Command::new("ffmpeg")
        .args([
            "-err_detect",
            "ignore_err",
            "-i",
            &src.to_string_lossy(),
            "-c",
            "copy",
            "-f",
            "matroska",
            "-y",
            &tmp.to_string_lossy(),
        ])
        // ffmpeg prints progress to stderr; discard to avoid noise.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if !status.success() || !tmp.exists() {
        let _ = fs::remove_file(&tmp);
        anyhow::bail!("ffmpeg remux exited {status}");
    }

    fs::rename(&tmp, src)?;
    Ok(())
}

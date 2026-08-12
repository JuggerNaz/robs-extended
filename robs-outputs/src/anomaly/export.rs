//! Clip export: concatenate the per-clip working-dir segment copies into a
//! single `.mp4` via ffmpeg's concat demuxer with stream-copy (`-c copy`, no
//! re-encode). The working dir holds the segment copies plus a generated
//! `concat.txt` list; absolute forward-slash paths keep the demuxer happy on
//! Windows.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// Concatenate `ordered_files` (filenames living in `work_dir`) into `out_mp4`.
/// Returns `Ok` only if ffmpeg succeeds *and* the output is non-empty.
pub fn export_clip(work_dir: &Path, ordered_files: &[String], out_mp4: &Path) -> Result<()> {
    if ordered_files.is_empty() {
        bail!("no segments to export");
    }
    if let Some(parent) = out_mp4.parent() {
        fs::create_dir_all(parent).ok();
    }

    // Build the concat list with absolute, forward-slash paths.
    let list_path = work_dir.join("concat.txt");
    let mut content = String::new();
    for name in ordered_files {
        let abs = work_dir.join(name);
        let escaped = abs.to_string_lossy().replace('\\', "/");
        content.push_str(&format!("file '{}'\n", escaped));
    }
    fs::write(&list_path, &content).context("writing anomaly concat list")?;

    let status = Command::new("ffmpeg")
        .args([
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            &list_path.to_string_lossy(),
            "-c",
            "copy",
            "-y",
            &out_mp4.to_string_lossy(),
        ])
        .current_dir(work_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("spawning ffmpeg concat for anomaly clip")?;

    if !status.success() {
        bail!("ffmpeg concat exited {status}");
    }
    let bytes = fs::metadata(out_mp4).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        bail!("ffmpeg concat produced an empty file");
    }
    Ok(())
}

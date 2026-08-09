//! A single ffmpeg invocation that writes one `.mkv` segment.
//!
//! Each [`ActiveSegment`] owns a spawned ffmpeg child whose stdin is fed raw
//! BGRA frames. When dropped via [`ActiveSegment::close`], stdin is closed so
//! ffmpeg finalizes the matroska file, the child is reaped, and the in-progress
//! `.part` crash marker is removed (signalling the segment is finalized and
//! safe). See [`super::recovery`] for how an uncleared marker is repaired on
//! the next engine start.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result};

use super::config::BlackboxConfig;

/// An open segment being written to by ffmpeg.
pub struct ActiveSegment {
    child: Child,
    stdin: Option<ChildStdin>,
    path: PathBuf,
    index: u64,
    input_width: u32,
    input_height: u32,
    started_at: Instant,
    started_utc: chrono::DateTime<chrono::Utc>,
    bytes_in: u64,
}

/// Path of the in-progress marker file sitting next to a segment. Its presence
/// means the segment may not be fully finalized.
pub fn part_marker(segment_path: &Path) -> PathBuf {
    let mut p = segment_path.to_path_buf();
    // Append ".part" rather than replacing the extension, so "x.mkv" -> "x.mkv.part".
    let mut name = p.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    p.set_file_name(name);
    p
}

impl ActiveSegment {
    /// Spawn ffmpeg for a new segment. Creates the `.part` marker.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        config: &BlackboxConfig,
        index: u64,
        input_width: u32,
        input_height: u32,
        path: PathBuf,
    ) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }

        let mut cmd = Command::new("ffmpeg");
        // Raw BGRA frames from stdin at the *capture* resolution.
        cmd.args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "-s",
            &format!("{}x{}", input_width, input_height),
            "-r",
            &format!("{}", config.fps),
            "-i",
            "pipe:0",
        ]);

        // Scale to the blackbox output resolution.
        if input_width != config.output_width || input_height != config.output_height {
            cmd.args([
                "-vf",
                &format!(
                    "scale={}:{}",
                    config.output_width, config.output_height
                ),
            ]);
        }

        // Video encoder. Mirror the main recorder's arg shapes so the same
        // ffmpeg builds behave identically.
        match config.encoder.as_str() {
            "h264_nvenc" => {
                cmd.args([
                    "-c:v",
                    "h264_nvenc",
                    "-preset",
                    "p4",
                    "-tune",
                    "hq",
                    "-rc",
                    "cbr",
                    "-b:v",
                    &format!("{}k", config.video_bitrate_kbps),
                    "-maxrate",
                    &format!("{}k", config.video_bitrate_kbps),
                    "-bufsize",
                    &format!("{}k", config.video_bitrate_kbps * 2),
                    "-pix_fmt",
                    "yuv420p",
                ]);
            }
            _ => {
                cmd.args([
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "zerolatency",
                    "-crf",
                    &config.crf.to_string(),
                    "-pix_fmt",
                    "yuv420p",
                ]);
            }
        }

        // MKV container: playable without finalization, so a crash mid-segment
        // still yields a recoverable file.
        cmd.args(["-f", "matroska", "-y", &path.to_string_lossy()]);

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning ffmpeg for blackbox segment {:?}", path))?;
        let stdin = child
            .stdin
            .take()
            .context("ffmpeg stdin was not piped")?;

        // Write the .part marker AFTER a successful spawn so recovery can find
        // it if we crash before close().
        let _ = fs::write(part_marker(&path), b"");

        Ok(Self {
            child,
            stdin: Some(stdin),
            path,
            index,
            input_width,
            input_height,
            started_at: Instant::now(),
            started_utc: chrono::Utc::now(),
            bytes_in: 0,
        })
    }

    pub fn index(&self) -> u64 {
        self.index
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn started_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.started_utc
    }

    pub fn input_width(&self) -> u32 {
        self.input_width
    }

    pub fn input_height(&self) -> u32 {
        self.input_height
    }

    pub fn bytes_in(&self) -> u64 {
        self.bytes_in
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    /// Write one raw BGRA frame. Returns Err on a broken pipe (ffmpeg died) or
    /// if the segment is already closing (stdin taken).
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("blackbox segment stdin already closed")?;
        stdin
            .write_all(frame)
            .with_context(|| "writing frame to blackbox ffmpeg stdin")?;
        stdin
            .flush()
            .with_context(|| "flushing blackbox ffmpeg stdin")?;
        self.bytes_in += frame.len() as u64;
        Ok(())
    }

    /// Returns true if the captured frame's dimensions differ from this
    /// segment's input format (signals the worker to rotate).
    pub fn dims_match(&self, width: u32, height: u32) -> bool {
        self.input_width == width && self.input_height == height
    }

    /// Finalize: close stdin (ffmpeg writes the trailer), wait for exit, and
    /// clear the `.part` marker. Returns the finalized segment's size + duration.
    pub fn close(mut self) -> Result<(u64, u64)> {
        // Dropping the ChildStdin closes the pipe -> ffmpeg sees EOF and
        // finalizes the matroska file.
        let _ = self.stdin.take();
        // Wait with a bounded timeout so a wedged ffmpeg can't hang shutdown.
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Err(_) => break,
            }
        }

        let duration_ms = self.started_at.elapsed().as_millis() as u64;
        let bytes = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);

        // Segment is finalized: remove the in-progress marker.
        let _ = fs::remove_file(part_marker(&self.path));
        Ok((bytes, duration_ms))
    }
}

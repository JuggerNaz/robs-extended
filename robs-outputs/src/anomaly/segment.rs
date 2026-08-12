//! A single ffmpeg invocation that writes one disposable `.mkv` scratch
//! segment for the Anomaly rolling buffer.
//!
//! Mirrors the Blackbox [`ActiveSegment`](../blackbox/segment/struct.ActiveSegment.html)
//! but deliberately omits the `.part` crash marker + recovery machinery:
//! scratch segments are disposable, so one left unfinished by a crash is simply
//! deleted on the next start rather than repaired.
//!
//! Each [`ScratchSegment`] owns a spawned ffmpeg child whose stdin is fed raw
//! BGRA frames. [`ScratchSegment::close`] closes stdin so ffmpeg finalizes the
//! matroska file and reaps the child.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::config::AnomalyConfig;

/// An open scratch segment being written to by ffmpeg.
pub struct ScratchSegment {
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

impl ScratchSegment {
    /// Spawn ffmpeg for a new scratch segment.
    pub fn open(
        config: &AnomalyConfig,
        index: u64,
        input_width: u32,
        input_height: u32,
        path: PathBuf,
    ) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }

        let (out_w, out_h) = config.scaled_output(input_width, input_height);

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

        // Scale to the configured output resolution (if any).
        if input_width != out_w || input_height != out_h {
            cmd.args(["-vf", &format!("scale={}:{}", out_w, out_h)]);
        }

        // Video encoder. Mirror the main recorder's arg shapes.
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

        // MKV container: disposable scratch, but still playable when finalized.
        cmd.args(["-f", "matroska", "-y", &path.to_string_lossy()]);

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning ffmpeg for anomaly segment {:?}", path))?;
        let stdin = child
            .stdin
            .take()
            .context("ffmpeg stdin was not piped")?;

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

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn started_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.started_utc
    }

    pub fn bytes_in(&self) -> u64 {
        self.bytes_in
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Write one raw BGRA frame. Returns Err on a broken pipe (ffmpeg died).
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("anomaly segment stdin already closed")?;
        stdin
            .write_all(frame)
            .with_context(|| "writing frame to anomaly ffmpeg stdin")?;
        stdin
            .flush()
            .with_context(|| "flushing anomaly ffmpeg stdin")?;
        self.bytes_in += frame.len() as u64;
        Ok(())
    }

    /// Returns true if the captured frame's dimensions differ from this
    /// segment's input format (signals the worker to rotate).
    pub fn dims_match(&self, width: u32, height: u32) -> bool {
        self.input_width == width && self.input_height == height
    }

    /// Finalize: close stdin (ffmpeg writes the trailer) and wait for exit.
    /// Returns the finalized segment's on-disk size + capture duration.
    pub fn close(mut self) -> Result<(u64, u64)> {
        let _ = self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
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
        Ok((bytes, duration_ms))
    }
}

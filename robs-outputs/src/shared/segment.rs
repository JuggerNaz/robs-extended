//! The common ffmpeg rawvideo segment writer behind the Blackbox
//! [`ActiveSegment`](../../blackbox/segment/struct.ActiveSegment.html) and the
//! Anomaly [`ScratchSegment`](../../anomaly/segment/struct.ScratchSegment.html).
//!
//! Both engines spawn ffmpeg with the same rawvideo → (optional scale) →
//! encoder → matroska pipeline and drive it over stdin; they diverge only in
//! policy: Blackbox guards each segment with a `.part` crash marker (cleared
//! on finalize, repaired by `recovery`), while Anomaly scratch segments are
//! disposable and carry no marker. [`RawFfmpegSegment`] holds the shared
//! mechanics, [`SegmentOps`] the shared contract, and the two engine segment
//! types are thin newtypes adding their own finalize semantics.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// ffmpeg parameters for one rawvideo segment invocation, resolved from an
/// engine config. `output_width`/`output_height` are the *effective* output
/// dimensions (anomaly resolves its `0`-means-inherit semantics upstream via
/// `AnomalyConfig::scaled_output`).
pub(crate) struct SegmentFfmpegArgs<'a> {
    pub input_width: u32,
    pub input_height: u32,
    pub fps: f32,
    pub output_width: u32,
    pub output_height: u32,
    pub encoder: &'a str,
    pub crf: u8,
    pub video_bitrate_kbps: u32,
}

/// Build the ffmpeg invocation both engines construct for a segment: raw BGRA
/// frames on stdin at the capture resolution, optional scale to the output
/// resolution, the configured encoder, matroska output.
fn build_segment_command(args: &SegmentFfmpegArgs<'_>, out_path: &Path) -> Command {
    let mut cmd = Command::new("ffmpeg");
    // Raw BGRA frames from stdin at the *capture* resolution.
    cmd.args([
        "-f",
        "rawvideo",
        "-pix_fmt",
        "bgra",
        "-s",
        &format!("{}x{}", args.input_width, args.input_height),
        "-r",
        &format!("{}", args.fps),
        "-i",
        "pipe:0",
    ]);

    // Scale to the engine's output resolution (when it differs).
    if args.input_width != args.output_width || args.input_height != args.output_height {
        cmd.args([
            "-vf",
            &format!("scale={}:{}", args.output_width, args.output_height),
        ]);
    }

    // Video encoder. Mirror the main recorder's arg shapes so the same
    // ffmpeg builds behave identically.
    match args.encoder {
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
                &format!("{}k", args.video_bitrate_kbps),
                "-maxrate",
                &format!("{}k", args.video_bitrate_kbps),
                "-bufsize",
                &format!("{}k", args.video_bitrate_kbps * 2),
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
                &args.crf.to_string(),
                "-pix_fmt",
                "yuv420p",
            ]);
        }
    }

    // MKV container: playable without finalization, so a crash mid-segment
    // still yields a recoverable file.
    cmd.args(["-f", "matroska", "-y", &out_path.to_string_lossy()]);

    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    cmd
}

/// An open ffmpeg child writing one `.mkv` segment: the engine-agnostic core
/// shared by `ActiveSegment` and `ScratchSegment`. `label` ("blackbox" /
/// "anomaly") only feeds error-message wording so messages stay identical to
/// the pre-dedup strings.
pub(crate) struct RawFfmpegSegment {
    child: Child,
    stdin: Option<ChildStdin>,
    path: PathBuf,
    index: u64,
    input_width: u32,
    input_height: u32,
    started_at: Instant,
    started_utc: chrono::DateTime<chrono::Utc>,
    bytes_in: u64,
    label: &'static str,
}

impl RawFfmpegSegment {
    /// Ensure the parent dir exists, spawn ffmpeg, and take its stdin.
    pub(crate) fn spawn(
        args: &SegmentFfmpegArgs<'_>,
        index: u64,
        out_path: PathBuf,
        label: &'static str,
    ) -> Result<Self> {
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).ok();
        }

        let mut cmd = build_segment_command(args, &out_path);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning ffmpeg for {label} segment {:?}", out_path))?;
        let stdin = child
            .stdin
            .take()
            .context("ffmpeg stdin was not piped")?;

        Ok(Self {
            child,
            stdin: Some(stdin),
            path: out_path,
            index,
            input_width: args.input_width,
            input_height: args.input_height,
            started_at: Instant::now(),
            started_utc: chrono::Utc::now(),
            bytes_in: 0,
            label,
        })
    }

    pub(crate) fn index(&self) -> u64 {
        self.index
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn started_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.started_utc
    }

    pub(crate) fn bytes_in(&self) -> u64 {
        self.bytes_in
    }

    pub(crate) fn input_width(&self) -> u32 {
        self.input_width
    }

    pub(crate) fn input_height(&self) -> u32 {
        self.input_height
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Returns true if the captured frame's dimensions differ from this
    /// segment's input format (signals the worker to rotate).
    pub(crate) fn dims_match(&self, width: u32, height: u32) -> bool {
        self.input_width == width && self.input_height == height
    }

    /// Write one raw BGRA frame. Returns Err on a broken pipe (ffmpeg died) or
    /// if the segment is already closing (stdin taken).
    pub(crate) fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .with_context(|| format!("{} segment stdin already closed", self.label))?;
        stdin
            .write_all(frame)
            .with_context(|| format!("writing frame to {} ffmpeg stdin", self.label))?;
        stdin
            .flush()
            .with_context(|| format!("flushing {} ffmpeg stdin", self.label))?;
        self.bytes_in += frame.len() as u64;
        Ok(())
    }

    /// Finalize: close stdin (ffmpeg writes the trailer), wait for exit (with
    /// a bounded timeout so a wedged ffmpeg can't hang shutdown), and report
    /// the finalized segment's on-disk size + capture duration. Engine-specific
    /// teardown (e.g. clearing the blackbox `.part` marker) happens in the
    /// caller's [`SegmentOps::close`].
    pub(crate) fn close_inner(mut self) -> (u64, u64) {
        // Dropping the ChildStdin closes the pipe -> ffmpeg sees EOF and
        // finalizes the matroska file.
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
        (bytes, duration_ms)
    }
}

/// The segment contract shared by the Blackbox `ActiveSegment` and the Anomaly
/// `ScratchSegment`, as consumed by the unified worker frame-pump's policies.
pub(crate) trait SegmentOps: Sized {
    /// Spawn the underlying ffmpeg writer via [`RawFfmpegSegment::spawn`]
    /// (input dims come from `args`). Engine-specific `open` wrappers
    /// (`.part` markers, output-dim resolution) call this after resolving
    /// their own specifics.
    fn open(
        index: u64,
        path: PathBuf,
        args: &SegmentFfmpegArgs<'_>,
        label: &'static str,
    ) -> Result<Self>;

    /// Write one raw BGRA frame. Returns Err on a broken pipe (ffmpeg died).
    fn write_frame(&mut self, frame: &[u8]) -> Result<()>;

    /// Finalize the segment (engine-specific teardown included). Returns the
    /// finalized segment's on-disk size + capture duration.
    fn close(self) -> Result<(u64, u64)>;

    /// Returns true if the captured frame's dimensions differ from this
    /// segment's input format (signals the worker to rotate).
    fn dims_match(&self, width: u32, height: u32) -> bool;

    fn elapsed(&self) -> Duration;

    fn index(&self) -> u64;

    fn path(&self) -> &Path;

    fn started_utc(&self) -> chrono::DateTime<chrono::Utc>;

    /// Raw input bytes fed to ffmpeg so far.
    fn bytes_in(&self) -> u64;
}

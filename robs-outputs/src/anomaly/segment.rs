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
//! matroska file and reaps the child. The ffmpeg plumbing itself lives in the
//! shared [`RawFfmpegSegment`](crate::shared::segment::RawFfmpegSegment) core;
//! this type resolves the `0`-means-inherit output-dimension semantics and
//! adds no crash-recovery state.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use super::config::AnomalyConfig;
use crate::shared::segment::{RawFfmpegSegment, SegmentFfmpegArgs, SegmentOps};

/// An open scratch segment being written to by ffmpeg.
pub struct ScratchSegment(RawFfmpegSegment);

impl SegmentOps for ScratchSegment {
    fn open(
        index: u64,
        path: PathBuf,
        args: &SegmentFfmpegArgs<'_>,
        label: &'static str,
    ) -> Result<Self> {
        Ok(Self(RawFfmpegSegment::spawn(args, index, path, label)?))
    }

    fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        self.0.write_frame(frame)
    }

    /// Finalize: close stdin (ffmpeg writes the trailer) and wait for exit.
    /// Returns the finalized segment's on-disk size + capture duration.
    fn close(self) -> Result<(u64, u64)> {
        Ok(self.0.close_inner())
    }

    /// Returns true if the captured frame's dimensions differ from this
    /// segment's input format (signals the worker to rotate).
    fn dims_match(&self, width: u32, height: u32) -> bool {
        self.0.dims_match(width, height)
    }

    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    fn index(&self) -> u64 {
        self.0.index()
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn started_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.0.started_utc()
    }

    fn bytes_in(&self) -> u64 {
        self.0.bytes_in()
    }
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
        let (out_w, out_h) = config.scaled_output(input_width, input_height);
        let args = SegmentFfmpegArgs {
            input_width,
            input_height,
            fps: config.fps,
            output_width: out_w,
            output_height: out_h,
            encoder: &config.encoder,
            crf: config.crf,
            video_bitrate_kbps: config.video_bitrate_kbps,
        };
        SegmentOps::open(index, path, &args, "anomaly")
    }

    pub fn index(&self) -> u64 {
        SegmentOps::index(self)
    }

    pub fn path(&self) -> &Path {
        SegmentOps::path(self)
    }

    pub fn started_utc(&self) -> chrono::DateTime<chrono::Utc> {
        SegmentOps::started_utc(self)
    }

    pub fn bytes_in(&self) -> u64 {
        SegmentOps::bytes_in(self)
    }

    pub fn elapsed(&self) -> Duration {
        SegmentOps::elapsed(self)
    }

    /// Write one raw BGRA frame. Returns Err on a broken pipe (ffmpeg died).
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        SegmentOps::write_frame(self, frame)
    }

    /// Returns true if the captured frame's dimensions differ from this
    /// segment's input format (signals the worker to rotate).
    pub fn dims_match(&self, width: u32, height: u32) -> bool {
        SegmentOps::dims_match(self, width, height)
    }

    /// Finalize: close stdin (ffmpeg writes the trailer) and wait for exit.
    /// Returns the finalized segment's on-disk size + capture duration.
    pub fn close(self) -> Result<(u64, u64)> {
        SegmentOps::close(self)
    }
}

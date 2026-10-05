//! A single ffmpeg invocation that writes one `.mkv` segment.
//!
//! Each [`ActiveSegment`] owns a spawned ffmpeg child whose stdin is fed raw
//! BGRA frames. When dropped via [`ActiveSegment::close`], stdin is closed so
//! ffmpeg finalizes the matroska file, the child is reaped, and the in-progress
//! `.part` crash marker is removed (signalling the segment is finalized and
//! safe). See [`super::recovery`] for how an uncleared marker is repaired on
//! the next engine start.
//!
//! The ffmpeg plumbing itself (spawn, frame writes, finalize wait) lives in
//! the shared [`RawFfmpegSegment`](crate::shared::segment::RawFfmpegSegment)
//! core; this type is the blackbox policy on top of it: the `.part` marker is
//! written after a successful spawn and cleared on finalize.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use super::config::BlackboxConfig;
use crate::shared::segment::{RawFfmpegSegment, SegmentFfmpegArgs, SegmentOps};

/// An open segment being written to by ffmpeg.
pub struct ActiveSegment(RawFfmpegSegment);

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

impl SegmentOps for ActiveSegment {
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

    /// Finalize: close stdin (ffmpeg writes the trailer), wait for exit, and
    /// clear the `.part` marker. Returns the finalized segment's size + duration.
    fn close(self) -> Result<(u64, u64)> {
        let path = self.0.path().to_path_buf();
        let result = self.0.close_inner();
        // Segment is finalized: remove the in-progress marker.
        let _ = fs::remove_file(part_marker(&path));
        Ok(result)
    }

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

impl ActiveSegment {
    /// Spawn ffmpeg for a new segment. Creates the `.part` marker.
    pub fn open(
        config: &BlackboxConfig,
        index: u64,
        input_width: u32,
        input_height: u32,
        path: PathBuf,
    ) -> Result<Self> {
        let args = SegmentFfmpegArgs {
            input_width,
            input_height,
            fps: config.fps,
            output_width: config.output_width,
            output_height: config.output_height,
            encoder: &config.encoder,
            crf: config.crf,
            video_bitrate_kbps: config.video_bitrate_kbps,
        };
        let seg = SegmentOps::open(index, path.clone(), &args, "blackbox")?;
        // Write the .part marker AFTER a successful spawn so recovery can find
        // it if we crash before close().
        let _ = fs::write(part_marker(&path), b"");
        Ok(seg)
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

    pub fn input_width(&self) -> u32 {
        self.0.input_width()
    }

    pub fn input_height(&self) -> u32 {
        self.0.input_height()
    }

    pub fn bytes_in(&self) -> u64 {
        SegmentOps::bytes_in(self)
    }

    pub fn elapsed(&self) -> Duration {
        SegmentOps::elapsed(self)
    }

    /// Write one raw BGRA frame. Returns Err on a broken pipe (ffmpeg died) or
    /// if the segment is already closing (stdin taken).
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        SegmentOps::write_frame(self, frame)
    }

    /// Returns true if the captured frame's dimensions differ from this
    /// segment's input format (signals the worker to rotate).
    pub fn dims_match(&self, width: u32, height: u32) -> bool {
        SegmentOps::dims_match(self, width, height)
    }

    /// Finalize: close stdin (ffmpeg writes the trailer), wait for exit, and
    /// clear the `.part` marker. Returns the finalized segment's size + duration.
    pub fn close(self) -> Result<(u64, u64)> {
        SegmentOps::close(self)
    }
}

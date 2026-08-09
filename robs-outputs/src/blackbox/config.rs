//! Configuration for the Blackbox engine. Built by the UI from
//! [`BlackboxSettings`](../../robs_profiles/settings/struct.BlackboxSettings.html)
//! plus the active video settings.

use std::path::PathBuf;

/// Runtime config consumed by the Blackbox engine. `Clone` so it can be handed
/// to both the worker and monitor threads.
#[derive(Debug, Clone)]
pub struct BlackboxConfig {
    /// Directory finalized segments are written into (created on start).
    pub output_dir: PathBuf,
    /// Output (scaled) resolution. Frames are scaled by ffmpeg from the native
    /// capture dimensions to this size.
    pub output_width: u32,
    pub output_height: u32,
    /// Target frame rate passed to ffmpeg (`-r`).
    pub fps: f32,
    /// Rotate a segment after this many seconds of capture.
    pub segment_duration_secs: u64,
    /// Rotate a segment after roughly this many MiB of *raw input* have been
    /// fed in (a proxy for output size; exact output sizing would require
    /// stat-ing the file, which the monitor does for status).
    pub segment_size_mb: u64,
    /// ffmpeg video encoder id: `"libx264"` or `"h264_nvenc"`.
    pub encoder: String,
    /// CRF for the software (x264) path. Ignored for nvenc.
    pub crf: u8,
    /// Bitrate in kbps for the nvenc CBR path. Ignored for x264.
    pub video_bitrate_kbps: u32,
    /// Free up disk when free space drops below this percentage.
    pub disk_low_warn_percent: u8,
    /// Pause ingestion (drop frames) when free space drops below this percentage.
    pub disk_low_critical_percent: u8,
    /// Hard cap on total on-disk blackbox storage in GiB (0 = unlimited).
    pub max_retention_gb: u32,
    /// Emit a `Stalled` event after this many seconds with no incoming frames.
    pub stall_threshold_secs: u64,
}

impl BlackboxConfig {
    /// Raw-bytes threshold at which the worker rotates to a new segment.
    pub fn segment_size_bytes(&self) -> u64 {
        self.segment_size_mb * 1024 * 1024
    }
}

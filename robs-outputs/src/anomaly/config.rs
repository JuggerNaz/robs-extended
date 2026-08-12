//! Runtime configuration for the Anomaly Capture engine.
//!
//! Built by the UI from
//! [`AnomalySettings`](../../../robs_profiles/settings/struct.AnomalySettings.html)
//! plus the active video settings.

use std::path::PathBuf;

/// Runtime config consumed by the Anomaly engine. `Clone` so it can be handed
/// to both the worker and monitor threads.
#[derive(Debug, Clone)]
pub struct AnomalyConfig {
    /// Root output directory. Rolling scratch segments live in `<output_dir>/buffer`;
    /// exported `.mp4` clips are written directly into `output_dir`.
    pub output_dir: PathBuf,
    /// Scaled output width. `0` means "use the native capture resolution".
    pub output_width: u32,
    /// Scaled output height. `0` means "use the native capture resolution".
    pub output_height: u32,
    /// Target frame rate passed to ffmpeg (`-r`).
    pub fps: f32,
    /// Footage captured *before* the trigger to include in an exported clip.
    pub pre_roll_secs: u64,
    /// Footage captured *after* the trigger to include in an exported clip.
    pub post_roll_secs: u64,
    /// Hard cap on the on-disk scratch ring size, in bytes.
    pub max_buffer_bytes: u64,
    /// Duration of each rolling scratch segment.
    pub segment_duration_secs: u64,
    /// ffmpeg video encoder id: `"libx264"` or `"h264_nvenc"`.
    pub encoder: String,
    /// CRF for the software (x264) path. Ignored for nvenc.
    pub crf: u8,
    /// Bitrate in kbps for the nvenc CBR path. Ignored for x264.
    pub video_bitrate_kbps: u32,
    /// Prefix prepended to exported clip filenames.
    pub clip_prefix: String,
    /// Suffix appended to exported clip filenames (before the extension).
    pub clip_suffix: String,
    /// Emit a low-disk warning when free space drops below this percent.
    pub disk_low_warn_percent: u8,
}

impl AnomalyConfig {
    /// Resolve the effective output dimensions for a frame captured at
    /// `capture_w x capture_h`. A `0` configured dimension inherits the capture
    /// resolution (no scaling).
    pub fn scaled_output(&self, capture_w: u32, capture_h: u32) -> (u32, u32) {
        let w = if self.output_width == 0 {
            capture_w
        } else {
            self.output_width
        };
        let h = if self.output_height == 0 {
            capture_h
        } else {
            self.output_height
        };
        (w, h)
    }
}

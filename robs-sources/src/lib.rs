pub mod capture;
pub mod native_capture;

pub use capture::*;
// `native_capture` also defines a `WindowCaptureSource` (native Win32/GDI),
// which collides with `capture::WindowCaptureSource` (FFmpeg gdigrab). Re-export
// its other public items here; the native window source stays reachable via
// `robs_sources::native_capture::WindowCaptureSource`.
pub use native_capture::{WindowInfo, WebcamCapture, capture_window, get_open_windows};

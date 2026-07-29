//! Typed capture-source metadata.
//!
//! Historically the *kind* of a capture source (display / window / webcam) and
//! its parameters were encoded into the [`crate::scene::scene_item::SceneItem`]
//! *name string* (e.g. `"Display Capture - Primary|idx:0|x:0|y:0|w:1920|h:1080"`)
//! and re-parsed with `starts_with` + manual splitting wherever the source was
//! consumed. [`CaptureSource`] replaces that fragile scheme: the descriptive
//! metadata lives in a typed enum stored on the scene item, and the item's name
//! is just a display label.
//!
//! The actual capture machinery (HWND handles, `WebcamCapture` sessions) stays
//! UI-side, keyed by [`crate::types::SceneItemId`] — only the descriptive
//! metadata moves here.

use serde::{Deserialize, Serialize};

/// What a scene item captures, plus the parameters needed to drive it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CaptureSource {
    /// A whole display monitor, identified by its position in virtual-screen
    /// coordinates (the authoritative identifier — monitor *index* is not used
    /// for capture, only for display).
    Display {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        label: String,
    },
    /// A specific application window, captured by title.
    Window { title: String },
    /// A webcam / video capture device.
    Webcam { device: String, width: u32, height: u32 },
}

impl CaptureSource {
    /// Human-readable label used in the UI (scenes list, properties, etc.).
    pub fn display_name(&self) -> String {
        match self {
            CaptureSource::Display { label, .. } => {
                format!("Display Capture - {}", label)
            }
            CaptureSource::Window { title } => format!("Window: {}", title),
            CaptureSource::Webcam { device, .. } => format!("Video Capture: {}", device),
        }
    }

    /// Native capture dimensions, when known up front. Window capture resolves
    /// its size dynamically from the target window, so it reports `None`.
    pub fn native_size(&self) -> Option<(u32, u32)> {
        match self {
            CaptureSource::Display { width, height, .. } => Some((*width, *height)),
            CaptureSource::Window { .. } => None,
            CaptureSource::Webcam { width, height, .. } => Some((*width, *height)),
        }
    }
}

//! Desktop duplication capture.
//!
//! On Windows this is DXGI Desktop Duplication via the `rusty-duplication`
//! crate — each monitor gets its own dedicated `Capturer` instance, no index
//! switching. On other platforms a stub backend with the exact same public
//! API compiles in its place: initialization succeeds but enumeration is
//! empty and frame capture reports "not supported", so callers degrade
//! gracefully (no frames) instead of failing to build. A macOS port would
//! swap the stub for a ScreenCaptureKit backend; Linux would use PipeWire.

/// Information about a monitor
#[derive(Clone, Debug)]
pub struct DxgiOutputInfo {
    pub position_x: i32,
    pub position_y: i32,
    pub width: u32,
    pub height: u32,
    pub device_name: String,
    pub is_primary: bool,
}

/// A wrapper for captured frame data
#[derive(Clone)]
pub struct DxgiFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub position_x: i32,
    pub position_y: i32,
}

// ---------------------------------------------------------------------------
// Windows backend: DXGI Desktop Duplication.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use super::{DxgiFrame, DxgiOutputInfo};
    use rusty_duplication::{MonitorInfoExt, OutputDescExt, Scanner, VecCapturer};
    use std::collections::HashMap;

    /// Per-monitor capture state
    struct MonitorCapture {
        capturer: VecCapturer,
        info: DxgiOutputInfo,
    }

    /// Capture manager - one capturer per monitor
    pub struct DxgiCaptureManager {
        /// Per-monitor capturers indexed by position (x, y)
        monitors: HashMap<(i32, i32), MonitorCapture>,
    }

    impl DxgiCaptureManager {
        /// Create a new DXGI capture manager and enumerate all monitors
        pub fn new() -> Result<Self, String> {
            let mut manager = Self {
                monitors: HashMap::new(),
            };

            // Enumerate monitors on creation
            manager.enumerate_outputs()?;

            Ok(manager)
        }

        /// Enumerate all monitors and create capturers for each
        pub fn enumerate_outputs(&mut self) -> Result<Vec<DxgiOutputInfo>, String> {
            let mut outputs = Vec::new();

            // Clear existing monitors
            self.monitors.clear();

            // Create scanner and iterate over all monitors
            let scanner = Scanner::new().map_err(|e| format!("Failed to create scanner: {:?}", e))?;

            for monitor in scanner {
                // Get output description for position info
                let desc = match monitor.dxgi_output_desc() {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[DXGI] Failed to get output desc: {:?}", e);
                        continue;
                    }
                };

                let width = desc.width();
                let height = desc.height();
                let position_x = desc.DesktopCoordinates.left;
                let position_y = desc.DesktopCoordinates.top;
                let device_name = String::from_utf16_lossy(&desc.DeviceName);
                let is_primary = monitor
                    .monitor_info()
                    .map(|mi| mi.is_primary())
                    .unwrap_or(false);

                eprintln!(
                    "[DXGI] Found monitor: {}x{} at ({}, {}) - {} - primary: {}",
                    width, height, position_x, position_y, device_name, is_primary
                );

                // Create a capturer for this monitor
                match VecCapturer::try_from(monitor) {
                    Ok(capturer) => {
                        let info = DxgiOutputInfo {
                            position_x,
                            position_y,
                            width,
                            height,
                            device_name: device_name.clone(),
                            is_primary,
                        };

                        eprintln!(
                            "[DXGI] Created capturer for {} at {:?}",
                            device_name,
                            (position_x, position_y)
                        );

                        self.monitors.insert(
                            (position_x, position_y),
                            MonitorCapture {
                                capturer,
                                info: info.clone(),
                            },
                        );

                        outputs.push(info);
                    }
                    Err(e) => {
                        eprintln!(
                            "[DXGI] Failed to create capturer for {}: {:?}",
                            device_name, e
                        );
                    }
                }
            }

            eprintln!(
                "[DXGI] Enumerated {} monitors with capturers",
                outputs.len()
            );
            Ok(outputs)
        }

        /// Check if we have a capturer at the given position
        pub fn has_output_at_position(&self, x: i32, y: i32) -> bool {
            self.monitors.contains_key(&(x, y))
        }

        /// Get output info at position
        pub fn get_output_at_position(&self, x: i32, y: i32) -> Option<&DxgiOutputInfo> {
            self.monitors.get(&(x, y)).map(|m| &m.info)
        }

        /// Capture a frame from the specified monitor position
        pub fn capture_frame(&mut self, position: (i32, i32)) -> Result<DxgiFrame, String> {
            // Ensure monitors are enumerated
            if self.monitors.is_empty() {
                self.enumerate_outputs()?;
            }

            // Get the capturer for this position
            let mc = self
                .monitors
                .get_mut(&position)
                .ok_or_else(|| format!("No capturer for position {:?}", position))?;

            // Capture frame
            match mc.capturer.capture() {
                Ok(_frame_info) => {
                    // Get dimensions from the stored info (not from frame_info)
                    let width = mc.info.width;
                    let height = mc.info.height;
                    let data = mc.capturer.buffer.clone();

                    Ok(DxgiFrame {
                        data,
                        width,
                        height,
                        position_x: position.0,
                        position_y: position.1,
                    })
                }
                Err(e) => {
                    let err_str = format!("{:?}", e);
                    if err_str.contains("WaitTimeout") {
                        Err("Timeout".to_string())
                    } else if err_str.contains("AccessLost") {
                        eprintln!(
                            "[DXGI] Access lost for monitor at {:?}, recreating capturer...",
                            position
                        );
                        // Remove the dead capturer
                        if let Some(_info) = self.monitors.remove(&position) {
                            eprintln!("[DXGI] Removed dead capturer for {:?}", position);
                            // Try to re-enumerate all monitors to recreate capturers
                            match self.enumerate_outputs() {
                                Ok(outputs) => {
                                    eprintln!("[DXGI] Re-enumerated {} monitors", outputs.len());
                                    // Check if we got the capturer back
                                    if self.monitors.contains_key(&position) {
                                        eprintln!(
                                            "[DXGI] Successfully recreated capturer for {:?}",
                                            position
                                        );
                                    } else {
                                        eprintln!(
                                            "[DXGI] Failed to recreate capturer for {:?}",
                                            position
                                        );
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[DXGI] Re-enumeration failed: {:?}", e);
                                }
                            }
                        }
                        Err("Access lost".to_string())
                    } else {
                        Err(format!("Capture error: {:?}", e))
                    }
                }
            }
        }

        /// Release all resources
        pub fn release_all(&mut self) {
            self.monitors.clear();
        }
    }
}

// ---------------------------------------------------------------------------
// Non-Windows stub backend: identical public API, no capture capability.
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
mod imp {
    use super::{DxgiFrame, DxgiOutputInfo};

    /// Stub capture manager. Keeps the same public API as the Windows DXGI
    /// backend so callers compile unchanged; desktop capture simply reports
    /// "not supported" until a native backend (ScreenCaptureKit / PipeWire)
    /// is implemented.
    pub struct DxgiCaptureManager;

    impl DxgiCaptureManager {
        /// Mirrors the Windows constructor: succeeds so lazy initialization
        /// at the call site does not spam errors on every frame.
        pub fn new() -> Result<Self, String> {
            Ok(Self)
        }

        /// No monitors are enumerable without a native backend.
        pub fn enumerate_outputs(&mut self) -> Result<Vec<DxgiOutputInfo>, String> {
            Ok(Vec::new())
        }

        pub fn has_output_at_position(&self, _x: i32, _y: i32) -> bool {
            false
        }

        pub fn get_output_at_position(&self, _x: i32, _y: i32) -> Option<&DxgiOutputInfo> {
            None
        }

        /// Always fails with a clear reason; the caller maps `Err` to "no
        /// frame this tick".
        pub fn capture_frame(&mut self, _position: (i32, i32)) -> Result<DxgiFrame, String> {
            Err("Desktop capture is not supported on this platform yet \
                 (needs a native ScreenCaptureKit/PipeWire backend)"
                .to_string())
        }

        pub fn release_all(&mut self) {}
    }
}

pub use imp::*;

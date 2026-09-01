//! OS device enumeration: monitors (GDI), and DirectShow audio/video devices
//! (via FFmpeg). Extracted verbatim from `app.rs`.

use super::state::{AudioDeviceInfo, MonitorInfo};

pub(crate) fn get_monitors() -> Vec<MonitorInfo> {
    let mut monitors = Vec::new();

    // Use Windows GDI to enumerate display monitors
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::{LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
        };

        unsafe extern "system" fn enum_monitors(
            hmonitor: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            lparam: LPARAM,
        ) -> windows::Win32::Foundation::BOOL {
            let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);

            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

            if GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
                let is_primary = (info.monitorInfo.dwFlags & 1) != 0;
                let width = info.monitorInfo.rcMonitor.right - info.monitorInfo.rcMonitor.left;
                let height = info.monitorInfo.rcMonitor.bottom - info.monitorInfo.rcMonitor.top;
                let position_x = info.monitorInfo.rcMonitor.left;
                let position_y = info.monitorInfo.rcMonitor.top;

                let name = String::from_utf16_lossy(
                    &info.szDevice[..info
                        .szDevice
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(info.szDevice.len())],
                );

                monitors.push(MonitorInfo {
                    name,
                    width: width as u32,
                    height: height as u32,
                    is_primary,
                    position_x,
                    position_y,
                });
            }

            windows::Win32::Foundation::BOOL(1)
        }

        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(enum_monitors),
                LPARAM(&mut monitors as *mut _ as isize),
            );
        }
    }

    // If no monitors found, provide a default
    if monitors.is_empty() {
        monitors.push(MonitorInfo {
            name: "Display 1".to_string(),
            width: 1920,
            height: 1080,
            is_primary: true,
            position_x: 0,
            position_y: 0,
        });
    }

    monitors
}

pub(crate) fn get_audio_devices() -> Vec<AudioDeviceInfo> {
    let mut devices = Vec::new();

    // Add special options first
    devices.push(AudioDeviceInfo {
        name: "Disabled".to_string(),
        id: "disabled".to_string(),
        is_input: true,
    });
    devices.push(AudioDeviceInfo {
        name: "Default".to_string(),
        id: "default".to_string(),
        is_input: true,
    });

    // Use FFmpeg to list DirectShow audio devices
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;

        // Get audio input devices (microphones)
        if let Ok(output) = Command::new("ffmpeg")
            .args(["-list_devices", "true", "-f", "dshow", "-i", "dummy"])
            .stderr(std::process::Stdio::piped())
            .output()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut in_audio_section = false;

            for line in stderr.lines() {
                if line.contains("DirectShow audio devices") {
                    in_audio_section = true;
                    continue;
                }

                // Stop at video devices section
                if line.contains("DirectShow video devices") {
                    in_audio_section = false;
                }

                if in_audio_section && line.contains("(audio)") {
                    // Parse device name from format: "Device Name" (audio)
                    if let Some(start) = line.find("\"") {
                        if let Some(end) = line[start + 1..].find("\"") {
                            let name = &line[start + 1..start + 1 + end];
                            // Skip device enumeration lines, keep actual device names
                            if !name.contains("Device") && !name.is_empty() && name.len() > 2 {
                                let id = format!("audio={}", name);
                                devices.push(AudioDeviceInfo {
                                    name: name.to_string(),
                                    id: id.clone(),
                                    is_input: true,
                                });
                            }
                        }
                    }
                }
            }
        }

        // If no devices found via FFmpeg, add known devices as fallback
        if devices.len() <= 2 {
            // Add GoXLR broadcast mix (system audio)
            devices.push(AudioDeviceInfo {
                name: "Broadcast Stream Mix (TC-HELICON GoXLR)".to_string(),
                id: "audio=Broadcast Stream Mix (TC-HELICON GoXLR)".to_string(),
                is_input: false, // This is system/desktop audio
            });
            // Add GoXLR chat mic
            devices.push(AudioDeviceInfo {
                name: "Chat Mic (TC-HELICON GoXLR)".to_string(),
                id: "audio=Chat Mic (TC-HELICON GoXLR)".to_string(),
                is_input: true,
            });
            // Add VB-Audio Virtual Cable
            devices.push(AudioDeviceInfo {
                name: "CABLE Output (VB-Audio Virtual Cable)".to_string(),
                id: "audio=CABLE Output (VB-Audio Virtual Cable)".to_string(),
                is_input: false,
            });
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        devices.push(AudioDeviceInfo {
            name: "Default".to_string(),
            id: "default".to_string(),
            is_input: true,
        });
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;

        // List AVFoundation microphones alongside the Default entry.
        if let Ok(output) = Command::new("ffmpeg")
            .args(["-list_devices", "true", "-f", "avfoundation", "-i", "dummy"])
            .stderr(std::process::Stdio::piped())
            .output()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut in_audio_section = false;

            for line in stderr.lines() {
                if line.contains("audio devices") {
                    in_audio_section = true;
                    continue;
                }

                if line.contains("video devices") {
                    in_audio_section = false;
                }

                if !in_audio_section {
                    continue;
                }

                // FFmpeg >= 8 format: `[AVFoundation indev @ ...] [0] Device Name`
                // split_once drops only the indev prefix, keeping `[0] Device Name`.
                let payload = line.split_once("] ").map(|(_, rest)| rest).unwrap_or(line);
                if let Some(rest) = payload.strip_prefix('[') {
                    if let Some((_, name)) = rest.split_once(']') {
                        let name = name.trim();
                        if !name.is_empty() {
                            devices.push(AudioDeviceInfo {
                                name: name.to_string(),
                                id: name.to_string(),
                                is_input: true,
                            });
                        }
                    }
                }
            }
        }
    }

    devices
}

/// Enumerate DirectShow video devices (webcams) via FFmpeg.
pub(crate) fn get_video_devices() -> Vec<String> {
    let mut devices = Vec::new();

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;

        if let Ok(output) = Command::new("ffmpeg")
            .args(["-list_devices", "true", "-f", "dshow", "-i", "dummy"])
            .stderr(std::process::Stdio::piped())
            .output()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);

            for line in stderr.lines() {
                // FFmpeg 8.x format:  [in#0 @ ...] "Device Name" (video)
                // FFmpeg <8 format:   "Device Name" (video)  (inside "DirectShow video devices" section)
                if line.contains("(video)") && !line.contains("Alternative name") {
                    if let Some(start) = line.find('"') {
                        if let Some(end) = line[start + 1..].find('"') {
                            let name = &line[start + 1..start + 1 + end];
                            if !name.is_empty() && name.len() > 2 {
                                devices.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;

        // List AVFoundation cameras; ffmpeg writes the device list to stderr.
        if let Ok(output) = Command::new("ffmpeg")
            .args(["-list_devices", "true", "-f", "avfoundation", "-i", "dummy"])
            .stderr(std::process::Stdio::piped())
            .output()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut in_video_section = false;

            for line in stderr.lines() {
                if line.contains("video devices") {
                    in_video_section = true;
                    continue;
                }

                if line.contains("audio devices") {
                    in_video_section = false;
                }

                if !in_video_section {
                    continue;
                }

                // Drop the `[AVFoundation indev @ ...] ` prefix if present.
                // split_once keeps the FFmpeg >= 8 `[0] Device Name` index intact.
                let payload = line.split_once("] ").map(|(_, rest)| rest).unwrap_or(line);

                // FFmpeg >= 8: `[0] Device Name`; older: `"Device Name" (video)`
                let name = if let Some(rest) = payload.strip_prefix('[') {
                    rest.split_once(']').map(|(_, name)| name.trim())
                } else if payload.starts_with('"') {
                    payload[1..].find('"').map(|end| &payload[1..1 + end])
                } else {
                    None
                };

                if let Some(name) = name {
                    if !name.is_empty() {
                        devices.push(name.to_string());
                    }
                }
            }
        }
    }

    devices
}

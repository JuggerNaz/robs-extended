//! Native capture helpers.
//!
//! Window enumeration/capture uses Win32 (`EnumWindows`, `PrintWindow`, GDI
//! `BitBlt`) and is only compiled on Windows; other platforms get honest
//! stubs (empty enumeration / `None` captures) until native backends are
//! added (ScreenCaptureKit on macOS, X11 on Linux). Webcam capture goes
//! through an FFmpeg subprocess and works on every platform via the OS
//! input format (dshow / avfoundation / v4l2).

use anyhow::Result;
use async_trait::async_trait;
use robs_core::traits::*;
use robs_core::*;
use std::any::Any;

/// Window info for enumeration
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub process_id: u32,
}

// ---------------------------------------------------------------------------
// Window enumeration + single-frame window capture (Windows: Win32/GDI,
// other platforms: stubs).
// ---------------------------------------------------------------------------

// Global storage for window enumeration
#[cfg(windows)]
static ENUM_WINDOWS: std::sync::Mutex<Vec<WindowInfo>> = std::sync::Mutex::new(Vec::new());

/// Get list of open windows using Win32 EnumWindows.
#[cfg(windows)]
pub fn get_open_windows() -> Vec<WindowInfo> {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // Clear the global storage
    {
        let mut windows = ENUM_WINDOWS.lock().unwrap();
        windows.clear();
    }

    unsafe {
        // Use EnumWindows with a static callback
        let _ = EnumWindows(Some(enum_windows_callback), LPARAM(0));
    }

    // Get the collected windows
    let mut windows = ENUM_WINDOWS.lock().unwrap();
    let result = windows.clone();
    windows.clear();
    result
}

/// Non-Windows stub: native window enumeration is not implemented yet.
/// The UI renders an empty list ("No windows found") on these platforms.
#[cfg(not(windows))]
pub fn get_open_windows() -> Vec<WindowInfo> {
    Vec::new()
}

// Callback function for EnumWindows (must be a function, not a closure)
#[cfg(windows)]
unsafe extern "system" fn enum_windows_callback(
    hwnd: windows::Win32::Foundation::HWND,
    _: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::BOOL {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // Check if window is visible
    let is_visible: BOOL = IsWindowVisible(hwnd);
    if is_visible.0 == 0 {
        return BOOL(1); // Continue enumeration
    }

    // Get window title
    let mut title_buf = [0u16; 512];
    let len = GetWindowTextW(hwnd, &mut title_buf);
    if len == 0 {
        return BOOL(1); // Continue
    }

    let title = String::from_utf16_lossy(&title_buf[..len as usize]);
    if title.is_empty() {
        return BOOL(1); // Continue
    }

    // Get process ID
    let mut process_id: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));

    // Skip some system windows
    if title.starts_with("Windows ")
        || title.starts_with("Program Manager")
        || process_id == 0
    {
        return BOOL(1); // Continue
    }

    // Add to global storage
    if let Ok(mut windows) = ENUM_WINDOWS.lock() {
        windows.push(WindowInfo {
            hwnd: hwnd.0 as isize,
            title,
            process_id,
        });
    }

    BOOL(1) // Continue enumeration
}

/// Capture a single frame from a window by HWND.
///
/// Windows: uses `PrintWindow` with `PW_RENDERFULLCONTENT` first (captures
/// hardware-accelerated / DirectX content), then falls back to `BitBlt`
/// from the window DC for older renderers.
///
/// Returns `(bgra_data, width, height)` or `None` on failure.
#[cfg(windows)]
pub fn capture_window(hwnd: isize) -> Option<(Vec<u8>, u32, u32)> {
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};

    unsafe {
        let hwnd = HWND(hwnd as *mut std::ffi::c_void);

        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }

        let width = (rect.right - rect.left) as u32;
        let height = (rect.bottom - rect.top) as u32;
        if width == 0 || height == 0 {
            return None;
        }

        // Use a screen DC to create compatible GDI objects.
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return None;
        }

        let mem_dc = CreateCompatibleDC(screen_dc);
        let bitmap = CreateCompatibleBitmap(screen_dc, width as i32, height as i32);
        let old_bitmap = SelectObject(mem_dc, bitmap);

        // Try PrintWindow with PW_RENDERFULLCONTENT (= 2) for
        // hardware-accelerated content, then fall back to BitBlt.
        let pw_ok = PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(2)).as_bool();
        let blt_ok = if !pw_ok {
            let hdc = GetWindowDC(hwnd);
            if hdc.is_invalid() {
                false
            } else {
                let r = BitBlt(
                    mem_dc,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    hdc,
                    0,
                    0,
                    SRCCOPY,
                )
                .is_ok();
                let _ = ReleaseDC(hwnd, hdc);
                r
            }
        } else {
            true
        };

        let result = if blt_ok {
            let mut bmi = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };

            let mut buffer = vec![0u8; (width * height * 4) as usize];
            let got_bits = GetDIBits(
                mem_dc,
                bitmap,
                0,
                height,
                Some(buffer.as_mut_ptr() as *mut _),
                &mut bmi as *mut _ as *mut BITMAPINFO,
                DIB_RGB_COLORS,
            );

            if got_bits == 0 {
                None
            } else {
                Some((buffer, width, height))
            }
        } else {
            None
        };

        let _ = SelectObject(mem_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);

        result
    }
}

/// Non-Windows stub: native window capture is not implemented yet.
/// Window-capture sources on these platforms simply produce no frames.
#[cfg(not(windows))]
pub fn capture_window(_hwnd: isize) -> Option<(Vec<u8>, u32, u32)> {
    None
}

/// Window capture source using window handle
pub struct WindowCaptureSource {
    id: SourceId,
    name: String,
    hwnd: isize,
    active: bool,
    video_info: VideoInfo,
    frame_count: u64,
}

impl WindowCaptureSource {
    pub fn new(name: String, hwnd: isize) -> Self {
        Self {
            id: SourceId(ObjectId::new()),
            name,
            hwnd,
            active: false,
            video_info: VideoInfo {
                width: 1920,
                height: 1080,
                fps_num: 30,
                fps_den: 1,
                format: PixelFormat::BGRA,
                range: VideoRange::Full,
                color_space: ColorSpace::SRGB,
            },
            frame_count: 0,
        }
    }

    /// Capture a frame from the window
    fn capture_frame(&mut self) -> Result<VideoFrame> {
        match capture_window(self.hwnd) {
            Some((buffer, width, height)) => {
                self.frame_count += 1;
                let pts = (self.frame_count * 1000 / 30) as i64;
                self.video_info.width = width;
                self.video_info.height = height;
                Ok(VideoFrame {
                    width,
                    height,
                    format: PixelFormat::BGRA,
                    data: buffer,
                    pts,
                    duration: 33333,
                    linesize: vec![(width * 4) as usize],
                })
            }
            None => {
                anyhow::bail!("Window capture failed");
            }
        }
    }
}

#[async_trait]
impl Source for WindowCaptureSource {
    fn id(&self) -> SourceId { self.id }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn get_video_info(&self) -> Option<VideoInfo> { Some(self.video_info.clone()) }
    fn get_audio_info(&self) -> Option<AudioInfo> { None }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }

    async fn activate(&mut self) -> Result<()> {
        self.active = true;
        self.frame_count = 0;
        println!("[WindowCapture] Activated window: {}", self.name);
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<()> {
        self.active = false;
        println!("[WindowCapture] Deactivated window: {}", self.name);
        Ok(())
    }

    fn is_active(&self) -> bool { self.active }

    fn properties_definition(&self) -> Vec<PropertyDef> {
        vec![]
    }

    fn get_property(&self, _name: &str) -> Option<PropertyValue> {
        None
    }

    fn set_property(&mut self, _name: &str, _value: PropertyValue) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl VideoSource for WindowCaptureSource {
    async fn get_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.active {
            return Ok(None);
        }

        match self.capture_frame() {
            Ok(frame) => Ok(Some(frame)),
            Err(e) => {
                println!("[WindowCapture] Error capturing frame: {}", e);
                Ok(None)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Webcam capture via FFmpeg (all platforms).
// ---------------------------------------------------------------------------

/// FFmpeg args selecting the platform's camera as FFmpeg input: everything
/// from the input format up to and including `-i <spec>`. Returns `None`
/// when the platform has no supported camera backend.
///
/// Windows uses DirectShow (`video=NAME`), macOS uses AVFoundation (device
/// name or index), Linux uses V4L2 (device path). The raw BGRA output side
/// is appended by the caller and is platform-independent.
fn webcam_input_args(device_name: &str, width: u32, height: u32, fps: f32) -> Option<Vec<String>> {
    let mut args: Vec<String> = Vec::new();

    #[cfg(windows)]
    args.push("-f".into());
    #[cfg(windows)]
    args.push("dshow".into());

    #[cfg(target_os = "macos")]
    args.push("-f".into());
    #[cfg(target_os = "macos")]
    args.push("avfoundation".into());

    #[cfg(all(unix, not(target_os = "macos")))]
    args.push("-f".into());
    #[cfg(all(unix, not(target_os = "macos")))]
    args.push("v4l2".into());

    #[cfg(not(any(windows, unix)))]
    {
        let _ = (device_name, width, height, fps);
        return None; // no camera backend for this platform
    }

    args.push("-video_size".into());
    args.push(format!("{}x{}", width, height));
    args.push("-framerate".into());
    args.push(fps.to_string());
    args.push("-i".into());
    // dshow wants `video=NAME`; avfoundation/v4l2 take the device
    // name/index/path exactly as reported by the device listing.
    #[cfg(windows)]
    args.push(format!("video={}", device_name));
    #[cfg(not(windows))]
    args.push(device_name.to_string());

    Some(args)
}

/// Manages an FFmpeg process that captures from a camera device
/// and continuously reads raw BGRA frames on a background thread.
pub struct WebcamCapture {
    child: Option<std::process::Child>,
    frame_thread: Option<std::thread::JoinHandle<()>>,
    latest_frame: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    width: u32,
    height: u32,
}

impl WebcamCapture {
    /// Spawn FFmpeg to capture from `device_name` at the given resolution/fps.
    /// Returns `None` if FFmpeg fails to start or the platform has no backend.
    pub fn new(device_name: &str, width: u32, height: u32, fps: f32) -> Option<Self> {
        use std::io::Read;
        use std::process::{Command, Stdio};

        let Some(input_args) = webcam_input_args(device_name, width, height, fps) else {
            return None;
        };

        let mut child = Command::new("ffmpeg")
            .args(&input_args)
            .args([
                "-f", "rawvideo",
                "-pix_fmt", "bgra",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let stdout = child.stdout.take()?;
        let latest_frame =
            std::sync::Arc::new(std::sync::Mutex::new(None::<Vec<u8>>));
        let stop_flag =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let lf = latest_frame.clone();
        let sf = stop_flag.clone();
        let frame_size = (width as usize) * (height as usize) * 4;

        let handle = std::thread::spawn(move || {
            let mut stdout = stdout;
            loop {
                if sf.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let mut buf = vec![0u8; frame_size];
                match stdout.read_exact(&mut buf) {
                    Ok(()) => {
                        if let Ok(mut frame) = lf.lock() {
                            *frame = Some(buf);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Some(Self {
            child: Some(child),
            frame_thread: Some(handle),
            latest_frame,
            stop_flag,
            width,
            height,
        })
    }

    /// Returns a clone of the latest captured frame (BGRA), or `None`.
    pub fn capture_frame(&self) -> Option<(Vec<u8>, u32, u32)> {
        let frame = self.latest_frame.lock().ok()?;
        frame
            .as_ref()
            .map(|data| (data.clone(), self.width, self.height))
    }

    /// Shut down FFmpeg and join the reader thread.
    pub fn stop(&mut self) {
        self.stop_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.frame_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WebcamCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

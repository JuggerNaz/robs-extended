//! Desktop/window/webcam preview capture and frame delivery to the recording
//! pipeline.

use super::RobsController;
use crate::dxgi_capture::DxgiCaptureManager;
use crate::state::PreviewFrame;
use robs_core::scene::CaptureSource;
use robs_core::types::SceneItemId;

impl RobsController {
    pub(crate) fn start_preview_capture(&mut self) {
        if self.preview.preview_capture_active {
            return;
        }

        // For now, just mark as active - actual capture will use recording pipeline
        // This is a placeholder until we can integrate with the video pipeline properly
        self.preview.preview_capture_active = true;
        eprintln!("[Preview] Preview capture requested (using recording pipeline)");
    }

    pub(crate) fn stop_preview_capture(&mut self) {
        if !self.preview.preview_capture_active {
            return;
        }

        self.preview.preview_capture_active = false;
        eprintln!("[Preview] Preview capture stopped");
    }

    /// Capture a specific monitor using our direct DXGI implementation
    /// position: (x, y) coordinates in virtual screen space - the authoritative monitor identifier
    fn capture_desktop_frame(&mut self, position: (i32, i32)) -> Option<(Vec<u8>, u32, u32)> {
        // Initialize DXGI manager if needed
        if self.dxgi_manager.is_none() {
            eprintln!("[DXGI] Initializing direct DXGI capture manager...");
            match DxgiCaptureManager::new() {
                Ok(manager) => {
                    self.dxgi_manager = Some(manager);
                    eprintln!("[DXGI] Manager initialized successfully");
                }
                Err(e) => {
                    eprintln!("[DXGI] Failed to create DXGI manager: {:?}", e);
                    return None;
                }
            }
        }

        let manager = self.dxgi_manager.as_mut().unwrap();

        // Enumerate outputs on first capture
        if !manager.has_output_at_position(position.0, position.1) {
            eprintln!("[DXGI] Enumerating all outputs...");
            match manager.enumerate_outputs() {
                Ok(outputs) => {
                    eprintln!("[DXGI] Found {} outputs total", outputs.len());
                }
                Err(e) => {
                    eprintln!("[DXGI] Enumeration failed: {:?}", e);
                    return None;
                }
            }
        }

        // Capture with DXGI - returns None on timeout (normal)
        manager
            .capture_frame(position)
            .ok()
            .map(|frame| (frame.data, frame.width, frame.height))
    }

    /// True when at least one frame interval has passed since the last frame
    /// handed to an encoder; updates the pacer timestamp when due. Shared by
    /// the fresh-compose path and the duplicate-resend path so exactly one
    /// frame per interval reaches FFmpeg regardless of capture activity.
    fn encoder_frame_due(&mut self) -> bool {
        frame_pacer_due(&mut self.record.last_frame_time, self.fps_setting)
    }

    /// Compose the shared output frame for whichever encoders are active
    /// (recording and/or streaming). This reuses the preview capture frame -
    /// no double capture needed - and exists so both consumers get the SAME
    /// composed frame instead of scaling/annotating twice per tick.
    ///
    /// Rate-limits to the target FPS, scales to the output resolution, bakes
    /// annotations/overlays, handles the snapshot hook, and converts RGBA→BGRA
    /// for FFmpeg. Returns `None` when the rate limiter skipped this frame.
    fn compose_output_frame(
        &mut self,
        rgba_data: &[u8],
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>> {
        // Rate limit to target FPS
        if !self.encoder_frame_due() {
            return None;
        }

        let out_w = self.output_width;
        let out_h = self.output_height;

        // Scale on main thread BEFORE sending to reduce memory pressure
        // 4K (33MB) → 1080p (8.3MB) = 75% reduction per frame
        let scaled_data = if width != out_w || height != out_h {
            self.scale_frame_to_output(rgba_data, width, height, out_w, out_h)
        } else {
            rgba_data.to_vec()
        };

        // Bake annotations into the frame. Annotations are stored in scene
        // output coordinates; map them onto the output-resolution frame.
        let mut scaled_data = scaled_data;
        if !self.annotation.annotations.is_empty() || !self.text_overlays.is_empty() {
            let (scene_w, scene_h) = self
                .scenes
                .current_scene()
                .map(|s| s.output_size())
                .unwrap_or((out_w, out_h));
            if scene_w > 0 && scene_h > 0 {
                let scale_x = out_w as f32 / scene_w as f32;
                let scale_y = out_h as f32 / scene_h as f32;
                if self.annotation.record_font.is_none() {
                    self.annotation.record_font = crate::annotation_raster::load_system_font();
                }
                crate::annotation_raster::composite_annotations(
                    &mut scaled_data,
                    out_w,
                    out_h,
                    &self.annotation.annotations,
                    scale_x,
                    scale_y,
                    self.annotation.record_font.as_ref(),
                );
                crate::annotation_raster::composite_text_overlays(
                    &mut scaled_data,
                    out_w,
                    out_h,
                    &self.text_overlays,
                    scale_x,
                    scale_y,
                    self.annotation.record_font.as_ref(),
                );
            }
        }

        // Convert RGBA to BGRA for FFmpeg
        let mut bgra_data = scaled_data;
        for chunk in bgra_data.chunks_exact_mut(4) {
            chunk.swap(0, 2); // RGBA -> BGRA
        }

        // Snapshot: capture the exact frame that is being encoded (scaled to
        // output resolution, annotations baked in). `bgra_data` is BGRA; the
        // PNG path wants RGBA, so swap channels on a clone.
        if self.take_snapshot {
            let mut snap = bgra_data.clone();
            for chunk in snap.chunks_exact_mut(4) {
                chunk.swap(0, 2); // BGRA -> RGBA
            }
            self.save_snapshot(&snap, out_w, out_h);
            self.take_snapshot = false;
        }

        // Remember so a later tick with no fresh frame can duplicate it.
        self.preview.last_output_frame = Some(bgra_data.clone());

        Some(bgra_data)
    }

    /// Re-send the last composed output frame to any live encoder when the
    /// pacer is due. FFmpeg timestamps piped raw frames at a fixed
    /// `-framerate`, so gaps in frame production compress wall-clock time on
    /// playback (the fast-forward-recording bug): when a UI tick produced no
    /// fresh frame (static screen, DXGI timeout, webcam lag), the previous
    /// frame is repeated instead.
    fn resend_last_output_frame(&mut self) {
        let recording_active = self.record.recording
            && !self.record.recording_paused
            && self.record.recording_frame_sender.is_some();
        let streaming_active =
            self.streaming && !self.streaming_paused && self.stream.frame_sender.is_some();
        if (!recording_active && !streaming_active) || !self.encoder_frame_due() {
            return;
        }
        let Some(frame) = self.preview.last_output_frame.clone() else {
            return; // no frame composed yet this session
        };
        if recording_active {
            if let Some(tx) = &self.record.recording_frame_sender {
                match tx.send(frame.clone()) {
                    Ok(_) => self.record.frame_count += 1,
                    Err(e) => {
                        eprintln!("[DXGI-Record] Duplicate send failed: {e}, stopping recording");
                        self.stop_recording();
                        return;
                    }
                }
            }
        }
        if streaming_active {
            if let Some(tx) = &self.stream.frame_sender {
                if tx.send(frame).is_err() {
                    eprintln!("[Stream] Duplicate send failed, stopping stream");
                    self.stop_streaming();
                }
            }
        }
    }

    pub(crate) fn process_preview_frames(&mut self) {
        // Limit preview capture to target FPS to avoid excessive CPU usage
        // from BGRA->RGBA conversion and frame storage. Encoder pacing is
        // NOT gated here — see `resend_last_output_frame` below.
        let target_frame_interval = std::time::Duration::from_secs_f32(1.0 / self.fps_setting);
        if self.preview.last_preview_capture.elapsed() < target_frame_interval {
            self.resend_last_output_frame();
            return;
        }
        self.preview.last_preview_capture = std::time::Instant::now();

        // Collect all visible capture source items first to avoid borrow issues.
        // Each item carries its typed `CaptureSource` metadata; the name string is
        // now just a display label and carries no machine-parsed parameters.
        let scene = self.scenes.current_scene();
        let capture_items: Vec<_> = scene
            .map(|s| {
                s.items()
                    .iter()
                    .filter(|i| i.is_visible() && i.capture().is_some())
                    .map(|i| (i.id(), i.capture().cloned()))
                    .collect()
            })
            .unwrap_or_default();

        if capture_items.is_empty() {
            // No capture sources - stop preview. A live encoder still gets
            // duplicated frames so its output keeps real-time pace.
            if self.preview.preview_capture_active {
                self.preview.preview_capture_active = false;
                eprintln!("[Preview] No capture sources, stopping preview");
            }
            self.resend_last_output_frame();
            return;
        }

        self.preview.preview_capture_active = true;
        self.preview.preview_frame_count += 1;

        // Capture each source independently. The capture kind + parameters are
        // now typed, so we resolve a single frame per item from its `CaptureSource`
        // and share the texture / recording / snapshot handling below.
        for (item_id, capture) in &capture_items {
            let texture_key = *item_id;
            let Some(capture) = capture else {
                continue;
            };

            // Resolve a fresh frame for this source based on its typed metadata.
            // DXGI / GDI / webcam all return BGRA, so the shared body converts once.
            let frame: Option<(Vec<u8>, u32, u32)> = match capture {
                CaptureSource::Window { .. } => {
                    // Window capture via GDI using the stored HWND.
                    self.window_hwnds
                        .get(item_id)
                        .and_then(|&hwnd| robs_sources::native_capture::capture_window(hwnd))
                }
                CaptureSource::Display {
                    x,
                    y,
                    width,
                    height,
                    label,
                } => {
                    let position = (*x, *y);
                    eprintln!(
                        "[Preview] Capturing Display Capture '{}' at position ({}, {}) - {}x{}",
                        label, x, y, width, height
                    );
                    // Pure DX11 capture - no GDI fallback
                    self.capture_desktop_frame(position)
                }
                CaptureSource::Webcam { .. } => {
                    // Webcam capture via FFmpeg dshow background process
                    self.webcam_captures
                        .get_mut(item_id)
                        .and_then(|wc| wc.capture_frame())
                }
            };

            if let Some((data, width, height)) = frame {
                // Blackbox tap: feed the raw BGRA frame to the always-on safety
                // recorder BEFORE the preview path swaps it to RGBA (ffmpeg
                // consumes BGRA natively). Non-blocking; no-op without an engine.
                self.submit_blackbox_frame(&data, width, height);
                // Anomaly tap: same raw BGRA frame, fed to the (user-toggled)
                // rolling buffer for on-demand clip capture. Non-blocking; no-op
                // without a running engine.
                self.submit_anomaly_frame(&data, width, height);

                // Convert BGRA -> RGBA (DXGI / GDI / webcam all return BGRA).
                let mut rgba_data = data;
                for chunk in rgba_data.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                }

                // Store the raw RGBA frame for the view layer to upload; the
                // version bump lets it skip re-uploading a frame it already
                // consumed (the former in-place texture update).
                let version = self.preview.next_frame_version;
                self.preview.next_frame_version = self.preview.next_frame_version.wrapping_add(1);
                self.preview.preview_frames.insert(
                    texture_key,
                    PreviewFrame {
                        data: rgba_data.clone(),
                        width,
                        height,
                        version,
                    },
                );

                // RECORDING / STREAMING: reuse the captured frame (no double
                // capture - both encoders tap into the same frame preview
                // uses). Compose once, then hand a copy to each active
                // encoder so running both at once does not double the
                // scale/annotation work per tick.
                let recording_active = self.record.recording
                    && !self.record.recording_paused
                    && self.record.recording_frame_sender.is_some();
                let streaming_active =
                    self.streaming && !self.streaming_paused && self.stream.frame_sender.is_some();
                if recording_active || streaming_active {
                    if let Some(bgra) = self.compose_output_frame(&rgba_data, width, height) {
                        if recording_active {
                            if let Some(tx) = &self.record.recording_frame_sender {
                                match tx.send(bgra.clone()) {
                                    Ok(_) => {
                                        self.record.frame_count += 1;
                                        if self.record.frame_count.is_multiple_of(30) {
                                            eprintln!(
                                                "[DXGI-Record] Sent {} frames to FFmpeg",
                                                self.record.frame_count
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "[DXGI-Record] Channel send failed: {e}, stopping recording"
                                        );
                                        self.stop_recording();
                                    }
                                }
                            }
                        }
                        if streaming_active {
                            if let Some(tx) = &self.stream.frame_sender {
                                match tx.send(bgra) {
                                    Ok(_) => self.stream.frame_count += 1,
                                    Err(e) => {
                                        eprintln!(
                                            "[Stream] Channel send failed: {e}, stopping stream"
                                        );
                                        self.stop_streaming();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Clean up frames, hwnd mappings, and webcam captures for sources that no longer exist
        let active_ids: std::collections::HashSet<SceneItemId> =
            capture_items.iter().map(|(id, _)| *id).collect();
        self.preview
            .preview_frames
            .retain(|id, _| active_ids.contains(id));
        self.window_hwnds.retain(|id, _| active_ids.contains(id));
        self.webcam_captures.retain(|id, _| active_ids.contains(id));

        // Frame duplication: when this tick composed no fresh frame (e.g.
        // DXGI timed out on an unchanged screen), repeat the last one so the
        // encoders' constant-framerate stream stays wall-clock paced. A
        // no-op when fresh frames were sent (the pacer is not due).
        self.resend_last_output_frame();
    }

    /// Scale captured frame to output resolution (OBS-style: preview matches output)
    fn scale_frame_to_output(
        &self,
        data: &[u8],
        src_width: u32,
        src_height: u32,
        dst_width: u32,
        dst_height: u32,
    ) -> Vec<u8> {
        use image::{ImageBuffer, Rgba};

        // If dimensions match, return original
        if src_width == dst_width && src_height == dst_height {
            return data.to_vec();
        }

        // Convert BGRA to RGBA for image crate
        let mut rgba_data = data.to_vec();
        for chunk in rgba_data.chunks_exact_mut(4) {
            chunk.swap(0, 2); // BGRA -> RGBA
        }

        // Create source image
        let src_img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            match ImageBuffer::from_raw(src_width, src_height, rgba_data) {
                Some(img) => img,
                None => {
                    eprintln!(
                        "[Scale] Failed to create {}x{} image ({} bytes)",
                        src_width,
                        src_height,
                        data.len()
                    );
                    return data.to_vec();
                }
            };

        // Resize to output resolution using bilinear filtering
        let dst_img = image::imageops::resize(
            &src_img,
            dst_width,
            dst_height,
            image::imageops::FilterType::Triangle,
        );

        // Convert back to BGRA
        let mut output = dst_img.into_raw();
        for chunk in output.chunks_exact_mut(4) {
            chunk.swap(0, 2); // RGBA -> BGRA
        }

        output
    }
}

/// Pacing decision for encoder frame delivery: `true` when at least one
/// frame interval (1 / `fps`) has elapsed since `last_frame_time`, updating
/// the timestamp only when due. This is the wall-clock heart of the
/// recording/streaming pipelines — both the fresh-frame path and the
/// duplicate-frame path consult it, so FFmpeg's constant-framerate stdin
/// receives exactly one frame per interval whether or not the screen changed.
pub(crate) fn frame_pacer_due(last_frame_time: &mut Option<std::time::Instant>, fps: f32) -> bool {
    let interval = std::time::Duration::from_secs_f32(1.0 / fps.max(1.0));
    let now = std::time::Instant::now();
    if let Some(last_time) = last_frame_time {
        if now.duration_since(*last_time) < interval {
            return false; // Skip this frame - not enough time elapsed
        }
    }
    *last_frame_time = Some(now);
    true
}

#[cfg(test)]
mod tests {
    use super::frame_pacer_due;
    use std::time::{Duration, Instant};

    #[test]
    fn first_frame_is_always_due() {
        let mut last = None;
        assert!(frame_pacer_due(&mut last, 30.0));
        assert!(last.is_some(), "due updates the timestamp");
    }

    #[test]
    fn not_due_within_the_frame_interval() {
        let mut last = Some(Instant::now());
        assert!(!frame_pacer_due(&mut last, 30.0));
        // Timestamp must be untouched when not due, so the remaining time to
        // the interval boundary is not repeatedly reset.
        assert!(last.unwrap().elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn due_again_after_one_interval() {
        let stale = Instant::now() - Duration::from_millis(100);
        let mut last = Some(stale);
        assert!(frame_pacer_due(&mut last, 30.0));
        // And immediately after a due frame, the next one is not due.
        assert!(!frame_pacer_due(&mut last, 30.0));
    }

    #[test]
    fn degenerate_fps_is_clamped_not_panicking() {
        // fps = 0 would divide by zero; the clamp treats it as 1 fps.
        let mut last = None;
        assert!(frame_pacer_due(&mut last, 0.0));
    }
}

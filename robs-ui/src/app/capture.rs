//! Desktop/window/webcam preview capture and frame delivery to the recording
//! pipeline.

use super::RobsApp;
use crate::dxgi_capture::DxgiCaptureManager;
use eframe::egui;
use robs_core::scene::CaptureSource;
use robs_core::types::SceneItemId;

impl RobsApp {
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

    /// Send an already-captured frame to the recording pipeline
    /// This reuses the preview capture frame - no double capture needed
    fn send_frame_to_recording(&mut self, rgba_data: &[u8], width: u32, height: u32) {
        // Rate limit to target FPS
        let target_frame_interval = std::time::Duration::from_secs_f32(1.0 / self.fps_setting);

        let now = std::time::Instant::now();
        if let Some(last_time) = self.record.last_frame_time {
            if now.duration_since(last_time) < target_frame_interval {
                return; // Skip this frame - not enough time elapsed
            }
        }
        self.record.last_frame_time = Some(now);

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

        // Snapshot: capture the exact frame that is being recorded (scaled to
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

        // Send to FFmpeg writer thread
        if let Some(ref tx) = self.record.recording_frame_sender {
            match tx.send(bgra_data) {
                Ok(_) => {
                    self.record.frame_count += 1;
                    if self.record.frame_count % 30 == 0 {
                        eprintln!(
                            "[DXGI-Record] Sent {} frames to FFmpeg ({}x{})",
                            self.record.frame_count,
                            out_w,
                            out_h
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[DXGI-Record] Channel send failed: {}, stopping recording",
                        e
                    );
                    self.stop_recording();
                }
            }
        }
    }

    pub(crate) fn process_preview_frames(&mut self, ctx: &egui::Context) {
        // Limit preview capture to target FPS to avoid excessive CPU usage
        // from BGRA->RGBA conversion and texture upload
        let target_frame_interval = std::time::Duration::from_secs_f32(1.0 / self.fps_setting);
        if self.preview.last_preview_capture.elapsed() < target_frame_interval {
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
            // No capture sources - stop preview
            if self.preview.preview_capture_active {
                self.preview.preview_capture_active = false;
                eprintln!("[Preview] No capture sources, stopping preview");
            }
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
                CaptureSource::Display { x, y, width, height, label } => {
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
                // Convert BGRA -> RGBA (DXGI / GDI / webcam all return BGRA).
                let mut rgba_data = data;
                for chunk in rgba_data.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                }

                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    &rgba_data,
                );

                // Reuse existing texture or create a new one
                if let Some(texture) = self.preview.preview_textures.get_mut(&texture_key) {
                    texture.set(color_image, egui::TextureOptions::LINEAR);
                } else {
                    let texture = ctx.load_texture(
                        format!("preview_{:?}", texture_key),
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.preview.preview_textures.insert(texture_key, texture);
                }

                // RECORDING: reuse the captured frame (no double capture -
                // recording taps into the same frame preview uses).
                if self.record.recording
                    && !self.record.recording_paused
                    && self.record.recording_frame_sender.is_some()
                {
                    self.send_frame_to_recording(&rgba_data, width, height);
                }
            }
        }

        // Clean up textures, hwnd mappings, and webcam captures for sources that no longer exist
        let active_ids: std::collections::HashSet<SceneItemId> =
            capture_items.iter().map(|(id, _)| *id).collect();
        self.preview.preview_textures
            .retain(|id, _| active_ids.contains(id));
        self.window_hwnds
            .retain(|id, _| active_ids.contains(id));
        self.webcam_captures
            .retain(|id, _| active_ids.contains(id));
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

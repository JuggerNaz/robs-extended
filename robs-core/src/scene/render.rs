use super::scene_item::{Crop, Scale};
use crate::traits::VideoFrame;
use crate::types::PixelFormat;

/// Blend two pixels with alpha compositing (simple over operator)
#[inline]
fn blend_pixels(bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    let alpha = top[3] as f32 / 255.0;
    let inv_alpha = 1.0 - alpha;

    [
        (top[0] as f32 * alpha + bottom[0] as f32 * inv_alpha) as u8,
        (top[1] as f32 * alpha + bottom[1] as f32 * inv_alpha) as u8,
        (top[2] as f32 * alpha + bottom[2] as f32 * inv_alpha) as u8,
        255, // Output alpha always fully opaque for now
    ]
}

/// Crop a video frame to the specified crop values
/// Returns a new frame with the cropped dimensions
pub fn crop_frame(frame: &VideoFrame, crop: &Crop) -> VideoFrame {
    let cropped_width = crop.cropped_width(frame.width);
    let cropped_height = crop.cropped_height(frame.height);

    if cropped_width == 0 || cropped_height == 0 {
        return VideoFrame::new(1, 1, frame.format);
    }

    let mut result = VideoFrame::new(cropped_width, cropped_height, frame.format);
    result.pts = frame.pts;
    result.duration = frame.duration;

    match frame.format {
        PixelFormat::RGBA | PixelFormat::BGRA => {
            let src_stride = frame.width as usize * 4;
            let dst_stride = cropped_width as usize * 4;

            for y in 0..cropped_height {
                let src_y = (y + crop.top) as usize;
                let src_row = &frame.data[src_y * src_stride..];
                let dst_row = &mut result.data[y as usize * dst_stride..];

                let src_start = (crop.left as usize) * 4;
                let src_end = src_start + dst_stride;
                dst_row.copy_from_slice(&src_row[src_start..src_end]);
            }
        }

        PixelFormat::Rgb24 | PixelFormat::Bgr24 => {
            let src_stride = frame.width as usize * 3;
            let dst_stride = cropped_width as usize * 3;

            for y in 0..cropped_height {
                let src_y = (y + crop.top) as usize;
                let src_row = &frame.data[src_y * src_stride..];
                let dst_row = &mut result.data[y as usize * dst_stride..];

                let src_start = (crop.left as usize) * 3;
                let src_end = src_start + dst_stride;
                dst_row.copy_from_slice(&src_row[src_start..src_end]);
            }
        }

        PixelFormat::YUY2 | PixelFormat::UYVY => {
            // YUY2/UYVY: 2 bytes per pixel, but chroma is shared
            let src_stride = frame.width as usize * 2;
            let dst_stride = cropped_width as usize * 2;

            for y in 0..cropped_height {
                let src_y = (y + crop.top) as usize;
                let src_row = &frame.data[src_y * src_stride..];
                let dst_row = &mut result.data[y as usize * dst_stride..];

                let src_start = (crop.left as usize) * 2;
                let src_end = src_start + dst_stride;
                dst_row.copy_from_slice(&src_row[src_start..src_end]);
            }
        }

        PixelFormat::NV12 => {
            // NV12: Y plane + interleaved UV plane
            let y_stride = frame.width as usize;
            let uv_stride = frame.width as usize;

            let y_plane_size = y_stride * frame.height as usize;

            // Copy Y plane
            for y in 0..cropped_height {
                let src_y = (y + crop.top) as usize;
                let src_offset = src_y * y_stride + crop.left as usize;
                let dst_offset = y as usize * cropped_width as usize;
                result.data[dst_offset..dst_offset + cropped_width as usize]
                    .copy_from_slice(&frame.data[src_offset..src_offset + cropped_width as usize]);
            }

            // Copy UV plane (chroma is half size)
            let uv_crop_left = crop.left / 2;
            let uv_crop_top = crop.top / 2;
            let cropped_uv_width = cropped_width / 2;
            let cropped_uv_height = cropped_height / 2;

            for y in 0..cropped_uv_height {
                let src_y = (y + uv_crop_top) as usize;
                let src_offset = y_plane_size + src_y * uv_stride + uv_crop_left as usize;
                let dst_offset = y_plane_size + y as usize * cropped_uv_width as usize;
                result.data[dst_offset..dst_offset + cropped_uv_width as usize].copy_from_slice(
                    &frame.data[src_offset..src_offset + cropped_uv_width as usize],
                );
            }
        }

        PixelFormat::I420 => {
            // I420: Y plane + U plane + V plane (all separate)
            let y_stride = frame.width as usize;
            let uv_stride = frame.width as usize / 2;

            let y_plane_size = y_stride * frame.height as usize;
            let uv_plane_size = uv_stride * (frame.height as usize / 2);

            // Copy Y plane
            for y in 0..cropped_height {
                let src_y = (y + crop.top) as usize;
                let src_offset = src_y * y_stride + crop.left as usize;
                let dst_offset = y as usize * cropped_width as usize;
                result.data[dst_offset..dst_offset + cropped_width as usize]
                    .copy_from_slice(&frame.data[src_offset..src_offset + cropped_width as usize]);
            }

            // Copy U plane
            let uv_crop_left = crop.left / 2;
            let uv_crop_top = crop.top / 2;
            let cropped_uv_width = cropped_width / 2;
            let cropped_uv_height = cropped_height / 2;

            for y in 0..cropped_uv_height {
                let src_y = (y + uv_crop_top) as usize;
                let src_offset = y_plane_size + src_y * uv_stride + uv_crop_left as usize;
                let dst_offset = y_plane_size + y as usize * cropped_uv_width as usize;
                result.data[dst_offset..dst_offset + cropped_uv_width as usize].copy_from_slice(
                    &frame.data[src_offset..src_offset + cropped_uv_width as usize],
                );
            }

            // Copy V plane
            for y in 0..cropped_uv_height {
                let src_y = (y + uv_crop_top) as usize;
                let src_offset =
                    y_plane_size + uv_plane_size + src_y * uv_stride + uv_crop_left as usize;
                let dst_offset =
                    y_plane_size + uv_plane_size + y as usize * cropped_uv_width as usize;
                result.data[dst_offset..dst_offset + cropped_uv_width as usize].copy_from_slice(
                    &frame.data[src_offset..src_offset + cropped_uv_width as usize],
                );
            }
        }

        _ => {
            // For unsupported formats, just return copy as-is (no actual crop)
            result.data.copy_from_slice(&frame.data);
        }
    }

    result
}

/// Scale a video frame to the target dimensions using bilinear interpolation
pub fn scale_frame(
    frame: &VideoFrame,
    _scale: Scale,
    target_width: u32,
    target_height: u32,
) -> VideoFrame {
    let src_width = frame.width;
    let src_height = frame.height;

    if target_width == 0 || target_height == 0 {
        return VideoFrame::new(1, 1, frame.format);
    }

    let mut result = VideoFrame::new(target_width, target_height, frame.format);
    result.pts = frame.pts;
    result.duration = frame.duration;

    let x_ratio = (src_width as f32) / (target_width as f32);
    let y_ratio = (src_height as f32) / (target_height as f32);

    match frame.format {
        PixelFormat::RGBA | PixelFormat::BGRA => {
            let src_stride = src_width as usize * 4;
            let dst_stride = target_width as usize * 4;

            for dy in 0..target_height {
                let src_y = (dy as f32 * y_ratio) as u32;
                let src_y = src_y.min(src_height - 1);
                let y_offset = (src_y as usize) * src_stride;

                for dx in 0..target_width {
                    let src_x = (dx as f32 * x_ratio) as u32;
                    let src_x = src_x.min(src_width - 1);
                    let x_offset = (src_x as usize) * 4;

                    let src_idx = y_offset + x_offset;
                    let dst_idx = (dy as usize) * dst_stride + (dx as usize) * 4;

                    // Simple nearest neighbor for now (faster, good enough for preview)
                    result.data[dst_idx..dst_idx + 4]
                        .copy_from_slice(&frame.data[src_idx..src_idx + 4]);
                }
            }
        }

        PixelFormat::Rgb24 | PixelFormat::Bgr24 => {
            let src_stride = src_width as usize * 3;
            let dst_stride = target_width as usize * 3;

            for dy in 0..target_height {
                let src_y = ((dy as f32 * y_ratio) as u32).min(src_height - 1);

                for dx in 0..target_width {
                    let src_x = ((dx as f32 * x_ratio) as u32).min(src_width - 1);

                    let src_idx = src_y as usize * src_stride + src_x as usize * 3;
                    let dst_idx = dy as usize * dst_stride + dx as usize * 3;

                    result.data[dst_idx..dst_idx + 3]
                        .copy_from_slice(&frame.data[src_idx..src_idx + 3]);
                }
            }
        }

        PixelFormat::YUY2 | PixelFormat::UYVY => {
            let src_stride = src_width as usize * 2;
            let dst_stride = target_width as usize * 2;

            for dy in 0..target_height {
                let src_y = ((dy as f32 * y_ratio) as u32).min(src_height - 1);

                for dx in 0..target_width {
                    let src_x = ((dx as f32 * x_ratio) as u32).min(src_width - 1);

                    let src_idx = src_y as usize * src_stride + src_x as usize * 2;
                    let dst_idx = dy as usize * dst_stride + dx as usize * 2;

                    result.data[dst_idx..dst_idx + 2]
                        .copy_from_slice(&frame.data[src_idx..src_idx + 2]);
                }
            }
        }

        PixelFormat::NV12 => {
            // NV12: Y plane + UV plane
            let y_plane_size = (src_width * src_height) as usize;
            let y_stride = src_width as usize;
            let uv_stride = src_width as usize;

            let dst_y_stride = target_width as usize;

            // Scale Y plane
            for dy in 0..target_height {
                let src_y = ((dy as f32 * y_ratio) as u32).min(src_height - 1);

                for dx in 0..target_width {
                    let src_x = ((dx as f32 * x_ratio) as u32).min(src_width - 1);

                    let src_idx = src_y as usize * y_stride + src_x as usize;
                    let dst_idx = dy as usize * dst_y_stride + dx as usize;

                    result.data[dst_idx] = frame.data[src_idx];
                }
            }

            // Scale UV plane (half resolution)
            let src_uv_height = src_height / 2;
            let src_uv_width = src_width / 2;
            let target_uv_width = target_width / 2;
            let target_uv_height = target_height / 2;

            let uv_x_ratio = (src_uv_width as f32) / (target_uv_width.max(1) as f32);
            let uv_y_ratio = (src_uv_height as f32) / (target_uv_height.max(1) as f32);

            let dst_uv_stride = target_uv_width as usize;

            for dy in 0..target_uv_height {
                let src_y = ((dy as f32 * uv_y_ratio) as u32).min(src_uv_height - 1);

                for dx in 0..target_uv_width {
                    let src_x = ((dx as f32 * uv_x_ratio) as u32).min(src_uv_width - 1);

                    let src_idx = y_plane_size + src_y as usize * uv_stride + src_x as usize * 2;
                    let dst_idx = y_plane_size + dy as usize * dst_uv_stride + dx as usize * 2;

                    result.data[dst_idx..dst_idx + 2]
                        .copy_from_slice(&frame.data[src_idx..src_idx + 2]);
                }
            }
        }

        _ => {
            // Fallback: just copy what fits
            let copy_width = target_width.min(src_width);
            let copy_height = target_height.min(src_height);

            for y in 0..copy_height {
                for x in 0..copy_width {
                    result.data[(y as usize) * target_width as usize * 4 + (x as usize) * 4..]
                        .copy_from_slice(
                            &frame.data[(y as usize) * src_width as usize * 4 + (x as usize) * 4..],
                        );
                }
            }
        }
    }

    result
}

/// Render a scene to an output frame buffer
/// Composites all visible items with their transforms applied
pub fn render_scene<F>(scene: &super::Scene, get_frame: F, output: &mut VideoFrame)
where
    F: Fn(crate::types::SourceId) -> Option<VideoFrame>,
{
    let bg = scene.background_color();

    // Clear output to background color
    match output.format {
        PixelFormat::RGBA | PixelFormat::BGRA => {
            for chunk in output.data.chunks_mut(4) {
                chunk.copy_from_slice(&bg);
            }
        }
        _ => {
            output.data.fill(0);
        }
    }

    // Render items from bottom to top (index 0 = bottom)
    for item in scene.items().iter().filter(|i| i.is_visible()) {
        // Get the source frame
        let source_frame = match get_frame(item.source_id()) {
            Some(f) => f,
            None => continue,
        };

        // 1. Crop the source frame
        let cropped = crop_frame(&source_frame, &item.crop());

        // 2. Calculate target dimensions after scaling
        let scale = item.scale();
        let target_width = (cropped.width as f32 * scale.x) as u32;
        let target_height = (cropped.height as f32 * scale.y) as u32;

        // Skip if zero dimensions
        if target_width == 0 || target_height == 0 {
            continue;
        }

        // 3. Scale the cropped frame
        let scaled = scale_frame(&cropped, scale, target_width, target_height);

        // 4. Blit to output at position
        let pos = item.position();
        let dst_x = pos.x as i32;
        let dst_y = pos.y as i32;

        // Simple alpha blending
        match (output.format, scaled.format) {
            (PixelFormat::RGBA, PixelFormat::RGBA) | (PixelFormat::BGRA, PixelFormat::BGRA) => {
                let src_stride = scaled.width as usize * 4;
                let dst_stride = output.width as usize * 4;

                for sy in 0..scaled.height {
                    let dy = dst_y + sy as i32;
                    if dy < 0 || dy >= output.height as i32 {
                        continue;
                    }

                    for sx in 0..scaled.width {
                        let dx = dst_x + sx as i32;
                        if dx < 0 || dx >= output.width as i32 {
                            continue;
                        }

                        let src_idx = sy as usize * src_stride + sx as usize * 4;
                        let dst_idx = dy as usize * dst_stride + dx as usize * 4;

                        let src_pixel: [u8; 4] = [
                            scaled.data[src_idx],
                            scaled.data[src_idx + 1],
                            scaled.data[src_idx + 2],
                            scaled.data[src_idx + 3],
                        ];

                        // Skip fully transparent pixels
                        if src_pixel[3] == 0 {
                            continue;
                        }

                        let dst_pixel: [u8; 4] = [
                            output.data[dst_idx],
                            output.data[dst_idx + 1],
                            output.data[dst_idx + 2],
                            output.data[dst_idx + 3],
                        ];

                        let blended = blend_pixels(dst_pixel, src_pixel);
                        output.data[dst_idx..dst_idx + 4].copy_from_slice(&blended);
                    }
                }
            }

            _ => {
                // For other formats, do a simple copy (no alpha blending)
                // This is a simplification - full implementation would handle all format combinations
            }
        }
    }
}

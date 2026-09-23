//! Software rasterization of annotations into an RGBA8 pixel buffer.
//!
//! Used to bake mark-up into recorded frames before they are handed to
//! FFmpeg. Implements alpha-blended line / arrow / rectangle / ellipse /
//! freehand-pen / text drawing without any extra dependencies beyond
//! `ab_glyph`, following the manual pixel manipulation already used
//! elsewhere in the engine (e.g. the BGRA/RGBA swaps in the capture
//! pipeline).

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use robs_core::{Annotation, AnnotationShape};

/// Font size (in scene units) used for `Text` annotations before scaling to
/// the frame.
pub const TEXT_FONT_SIZE: f32 = 32.0;

/// Composite a set of annotations onto an RGBA8 (`[r, g, b, a]`) frame.
///
/// `scale_x` / `scale_y` map annotation **scene coordinates** to frame
/// pixels (e.g. `frame_width / scene_width`). Annotations are drawn in the
/// order supplied (later ones on top). Alpha is blended over the existing
/// frame contents, so partially transparent colors composite correctly.
///
/// `font` is required for `Text` annotations to render; if `None`, text is
/// skipped (e.g. when no system font could be loaded).
pub fn composite_annotations(
    frame: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    annotations: &[Annotation],
    scale_x: f32,
    scale_y: f32,
    font: Option<&FontVec>,
) {
    for ann in annotations.iter().filter(|a| a.is_visible()) {
        let style = ann.style();
        let color = style.color;
        // Stroke width is interpreted in scene units; scale to frame pixels.
        let radius = (style.stroke_width * scale_x.min(scale_y) / 2.0).max(0.5);

        let x0 = ann.start().x * scale_x;
        let y0 = ann.start().y * scale_y;
        let x1 = ann.end().x * scale_x;
        let y1 = ann.end().y * scale_y;

        match ann.shape() {
            AnnotationShape::Line => {
                stamp_line(frame, frame_w, frame_h, x0, y0, x1, y1, radius, color);
            }
            AnnotationShape::Arrow => {
                stamp_line(frame, frame_w, frame_h, x0, y0, x1, y1, radius, color);
                let dx = x1 - x0;
                let dy = y1 - y0;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > 1.0 {
                    let dirx = dx / dist;
                    let diry = dy / dist;
                    let head_len = (style.stroke_width * scale_x.min(scale_y) * 3.0).max(12.0);
                    let head_ang = 0.5_f32;
                    let perpx = -diry;
                    let perpy = dirx;
                    let bx = x1 - dirx * head_len;
                    let by = y1 - diry * head_len;
                    let spread = head_len * head_ang.tan();
                    let lx = bx + perpx * spread;
                    let ly = by + perpy * spread;
                    let rx = bx - perpx * spread;
                    let ry = by - perpy * spread;
                    stamp_line(frame, frame_w, frame_h, x1, y1, lx, ly, radius, color);
                    stamp_line(frame, frame_w, frame_h, x1, y1, rx, ry, radius, color);
                }
            }
            AnnotationShape::Rectangle => {
                if style.filled {
                    fill_rect(frame, frame_w, frame_h, x0, y0, x1, y1, color);
                }
                stamp_line(frame, frame_w, frame_h, x0, y0, x1, y0, radius, color);
                stamp_line(frame, frame_w, frame_h, x1, y0, x1, y1, radius, color);
                stamp_line(frame, frame_w, frame_h, x1, y1, x0, y1, radius, color);
                stamp_line(frame, frame_w, frame_h, x0, y1, x0, y0, radius, color);
            }
            AnnotationShape::Ellipse => {
                let cx = (x0 + x1) / 2.0;
                let cy = (y0 + y1) / 2.0;
                let rx = ((x1 - x0) / 2.0).abs();
                let ry = ((y1 - y0) / 2.0).abs();
                if style.filled {
                    fill_ellipse(frame, frame_w, frame_h, cx, cy, rx, ry, color);
                }
                // Outline as a closed parametric polyline.
                let n = 48;
                let mut prev_x = cx + rx;
                let mut prev_y = cy;
                for i in 1..=n {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    let px = cx + rx * t.cos();
                    let py = cy + ry * t.sin();
                    stamp_line(frame, frame_w, frame_h, prev_x, prev_y, px, py, radius, color);
                    prev_x = px;
                    prev_y = py;
                }
            }
            AnnotationShape::Pen => {
                // Freehand polyline (scaled to frame pixels).
                let pts: Vec<(f32, f32)> = ann
                    .points()
                    .iter()
                    .map(|p| (p.x * scale_x, p.y * scale_y))
                    .collect();
                for w in pts.windows(2) {
                    stamp_line(frame, frame_w, frame_h, w[0].0, w[0].1, w[1].0, w[1].1, radius, color);
                }
            }
            AnnotationShape::Text => {
                if let Some(font) = font {
                    let text = ann.text();
                    if !text.is_empty() {
                        let size = TEXT_FONT_SIZE * scale_x;
                        render_text(frame, frame_w, frame_h, font, x0, y0, text, color, size);
                    }
                }
            }
        }
    }
}

/// Composite text overlays (with semi-transparent backgrounds) into an
/// RGBA8 frame. `scale_x` / `scale_y` map overlay scene coordinates to
/// frame pixels.
pub fn composite_text_overlays(
    frame: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    overlays: &[robs_core::TextOverlay],
    scale_x: f32,
    scale_y: f32,
    font: Option<&FontVec>,
) {
    let font = match font {
        Some(f) => f,
        None => return,
    };
    for ov in overlays.iter().filter(|o| o.is_visible()) {
        let text = ov.text();
        if text.is_empty() {
            continue;
        }
        let x = ov.position().x * scale_x;
        let y = ov.position().y * scale_y;
        let size = ov.font_size() * scale_x;
        let c = ov.color();

        // Semi-transparent background box for readability.
        let est_w = text.len() as f32 * size * 0.6 + 8.0 * scale_x;
        let est_h = size * 1.2 + 4.0 * scale_y;
        fill_rect(
            frame,
            frame_w,
            frame_h,
            x - 4.0 * scale_x,
            y - 2.0 * scale_y,
            x - 4.0 * scale_x + est_w,
            y - 2.0 * scale_y + est_h,
            [0, 0, 0, 160],
        );

        render_text(frame, frame_w, frame_h, font, x, y, text, c, size);
    }
}

/// Try to load a usable system TrueType font for rasterizing text into
/// recordings. Returns `None` if no candidate could be read.
pub fn load_system_font() -> Option<FontVec> {
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\arial.ttf",
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "C:\\Windows\\Fonts\\consola.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/Library/Fonts/Arial.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(data) = std::fs::read(path) {
            if let Ok(font) = FontVec::try_from_vec(data) {
                return Some(font);
            }
        }
    }
    None
}

/// Render a single line of text into the frame using `ab_glyph`.
fn render_text(
    frame: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    font: &FontVec,
    x: f32,
    y: f32,
    text: &str,
    color: [u8; 4],
    size: f32,
) {
    let pxscale = PxScale::from(size);
    let scaled = font.as_scaled(pxscale);
    let ascent = scaled.ascent();
    let mut pen = ab_glyph::Point { x, y: y + ascent };

    for ch in text.chars() {
        let glyph_id = scaled.glyph_id(ch);
        let glyph = glyph_id.with_scale_and_position(pxscale, pen);
        if let Some(outlined) = font.outline_glyph(glyph) {
            let min_x = outlined.px_bounds().min.x;
            let min_y = outlined.px_bounds().min.y;
            outlined.draw(|gx, gy, coverage| {
                // coverage is 0..1 anti-aliasing alpha for this pixel.
                let alpha = (coverage * color[3] as f32) as u8;
                if alpha > 0 {
                    let px = min_x as i32 + gx as i32;
                    let py = min_y as i32 + gy as i32;
                    put_pixel(
                        frame,
                        frame_w,
                        frame_h,
                        px,
                        py,
                        [color[0], color[1], color[2], alpha],
                    );
                }
            });
        }
        pen.x += scaled.h_advance(glyph_id);
    }
}

/// Alpha-blend `color` into the pixel at `(x, y)` (bounds-checked, no-op if
/// out of frame).
#[inline]
fn put_pixel(frame: &mut [u8], w: u32, h: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 {
        return;
    }
    let (xu, yu) = (x as u32, y as u32);
    if xu >= w || yu >= h {
        return;
    }
    let idx = ((yu * w + xu) * 4) as usize;
    let a = color[3] as f32 / 255.0;
    let ia = 1.0 - a;
    frame[idx] = (color[0] as f32 * a + frame[idx] as f32 * ia) as u8;
    frame[idx + 1] = (color[1] as f32 * a + frame[idx + 1] as f32 * ia) as u8;
    frame[idx + 2] = (color[2] as f32 * a + frame[idx + 2] as f32 * ia) as u8;
    frame[idx + 3] = 255;
}

/// Stamp a filled disk (used as the brush for thick strokes).
fn stamp_disk(frame: &mut [u8], w: u32, h: u32, cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
    if radius <= 0.0 {
        return;
    }
    let x0 = (cx - radius).floor() as i32;
    let x1 = (cx + radius).ceil() as i32;
    let y0 = (cy - radius).floor() as i32;
    let y1 = (cy + radius).ceil() as i32;
    let r2 = radius * radius;
    let mut py = y0;
    while py <= y1 {
        let mut px = x0;
        while px <= x1 {
            let ddx = px as f32 - cx;
            let ddy = py as f32 - cy;
            if ddx * ddx + ddy * ddy <= r2 {
                put_pixel(frame, w, h, px, py, color);
            }
            px += 1;
        }
        py += 1;
    }
}

/// Draw a thick line by stamping the disk brush along a DDA interpolation.
fn stamp_line(
    frame: &mut [u8],
    w: u32,
    h: u32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: f32,
    color: [u8; 4],
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let dist = (dx * dx + dy * dy).sqrt();
    let n = dist.ceil().max(1.0) as i32;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        stamp_disk(frame, w, h, x0 + dx * t, y0 + dy * t, radius, color);
    }
}

/// Fill the axis-aligned rectangle spanned by the two points.
fn fill_rect(frame: &mut [u8], w: u32, h: u32, x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 4]) {
    let (xa, xb) = (x0.min(x1), x0.max(x1));
    let (ya, yb) = (y0.min(y1), y0.max(y1));
    let mut y = ya.floor() as i32;
    while y <= yb.ceil() as i32 {
        let mut x = xa.floor() as i32;
        while x <= xb.ceil() as i32 {
            put_pixel(frame, w, h, x, y, color);
            x += 1;
        }
        y += 1;
    }
}

// ---------------------------------------------------------------------------
// Scene overlays: data-string boxes + company logo
// ---------------------------------------------------------------------------

/// One label/value row of the data-string overlay.
pub struct DataStringRow {
    pub label: String,
    pub value: String,
}

/// Data-string layout constants (scene units, scaled like coordinates).
const DS_FONT_SIZE: f32 = 22.0;
const DS_ROW_HEIGHT: f32 = 30.0;
const DS_PAD: f32 = 12.0;
const DS_INSET: f32 = 16.0;
const DS_LABEL_GAP: f32 = 14.0;
const DS_BACKDROP: [u8; 4] = [0, 0, 0, 160];
const DS_LABEL_COLOR: [u8; 4] = [160, 160, 160, 255];
const DS_VALUE_COLOR: [u8; 4] = [255, 255, 255, 255];

/// Rendered width of `text` at `size` px (sum of glyph advances).
fn measure_text(font: &FontVec, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    text.chars()
        .map(|ch| {
            let gid = scaled.glyph_id(ch);
            scaled.h_advance(gid)
        })
        .sum()
}

/// Composite the data-string overlay: one semi-transparent box per group —
/// the first anchored bottom-left, the second bottom-right. Rows render as
/// `LABEL` + `VALUE` in two columns (the label column sized by the widest
/// label), matching the ROV mockup. No-op without a font or with empty
/// groups.
pub fn composite_data_string(
    frame: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    groups: &[Vec<DataStringRow>],
    scale_x: f32,
    scale_y: f32,
    font: Option<&FontVec>,
) {
    let Some(font) = font else { return };
    let font_size = DS_FONT_SIZE * scale_x;
    let row_h = DS_ROW_HEIGHT * scale_y;
    let pad = DS_PAD * scale_x;
    let inset = DS_INSET * scale_x;
    let gap = DS_LABEL_GAP * scale_x;
    for (group_idx, rows) in groups.iter().enumerate().take(2) {
        if rows.is_empty() {
            continue;
        }
        let label_w = rows
            .iter()
            .map(|r| measure_text(font, &r.label, font_size))
            .fold(0.0_f32, f32::max);
        let value_w = rows
            .iter()
            .map(|r| measure_text(font, &r.value, font_size))
            .fold(0.0_f32, f32::max);
        let box_w = pad * 2.0 + label_w + gap + value_w;
        let box_h = pad * 2.0 + row_h * rows.len() as f32;
        let box_x = if group_idx == 0 {
            inset
        } else {
            (frame_w as f32 - inset - box_w).max(0.0)
        };
        let box_y = (frame_h as f32 - inset - box_h).max(0.0);
        fill_rect(
            frame,
            frame_w,
            frame_h,
            box_x,
            box_y,
            box_x + box_w,
            box_y + box_h,
            DS_BACKDROP,
        );
        let value_x = box_x + pad + label_w + gap;
        for (i, row) in rows.iter().enumerate() {
            let text_y = box_y + pad + row_h * i as f32;
            render_text(
                frame,
                frame_w,
                frame_h,
                font,
                box_x + pad,
                text_y,
                &row.label,
                DS_LABEL_COLOR,
                font_size,
            );
            render_text(
                frame,
                frame_w,
                frame_h,
                font,
                value_x,
                text_y,
                &row.value,
                DS_VALUE_COLOR,
                font_size,
            );
        }
    }
}

/// Alpha-blend an RGBA8 logo onto the RGBA8 frame at frame-pixel `(x, y)`.
/// Fully transparent logo pixels leave the frame untouched.
pub fn composite_logo(
    frame: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    logo_rgba: &[u8],
    logo_w: u32,
    logo_h: u32,
    x: f32,
    y: f32,
) {
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    for ly in 0..logo_h as i64 {
        let fy = y0 + ly;
        if fy < 0 || fy >= frame_h as i64 {
            continue;
        }
        for lx in 0..logo_w as i64 {
            let fx = x0 + lx;
            if fx < 0 || fx >= frame_w as i64 {
                continue;
            }
            let src = ((ly * logo_w as i64 + lx) * 4) as usize;
            if src + 3 >= logo_rgba.len() {
                continue;
            }
            let alpha = logo_rgba[src + 3];
            if alpha == 0 {
                continue;
            }
            put_pixel(
                frame,
                frame_w,
                frame_h,
                fx as i32,
                fy as i32,
                [logo_rgba[src], logo_rgba[src + 1], logo_rgba[src + 2], alpha],
            );
        }
    }
}

/// Fill an ellipse via per-row scanline extents.
fn fill_ellipse(
    frame: &mut [u8],
    w: u32,
    h: u32,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    color: [u8; 4],
) {
    if rx <= 0.0 || ry <= 0.0 {
        return;
    }
    let mut y = (cy - ry).floor() as i32;
    let y_end = (cy + ry).ceil() as i32;
    while y <= y_end {
        let dy = y as f32 - cy;
        let k = 1.0 - (dy / ry) * (dy / ry);
        if k >= 0.0 {
            let dxmax = (rx * rx * k).sqrt();
            let mut x = (cx - dxmax).floor() as i32;
            let x_end = (cx + dxmax).ceil() as i32;
            while x <= x_end {
                put_pixel(frame, w, h, x, y, color);
                x += 1;
            }
        }
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_string_darkens_bottom_left_and_leaves_rest_clean() {
        let Some(font) = load_system_font() else {
            return; // font discovery is environment-dependent
        };
        let (w, h) = (400u32, 300u32);
        let mut frame = vec![255u8; (w * h * 4) as usize];
        let groups = vec![vec![DataStringRow {
            label: "EASTING".to_string(),
            value: "123456 m".to_string(),
        }]];
        composite_data_string(&mut frame, w, h, &groups, 1.0, 1.0, Some(&font));
        let px = |x: u32, y: u32| frame[((y * w + x) * 4) as usize];
        // Inside the bottom-left box but left of the label pen (in the
        // backdrop padding): darkened by the translucent backdrop.
        assert!(
            px(24, 260) < 200,
            "expected a darkened pixel inside the data-string box"
        );
        // Well above the box: untouched.
        assert_eq!(px(24, 100), 255);
    }

    #[test]
    fn data_string_without_font_or_rows_is_a_noop() {
        let (w, h) = (64u32, 64u32);
        let mut frame = vec![7u8; (w * h * 4) as usize];
        let before = frame.clone();
        composite_data_string(&mut frame, w, h, &[], 1.0, 1.0, None);
        assert_eq!(frame, before);
    }

    #[test]
    fn composite_logo_blends_opaque_pixels_only() {
        let (w, h) = (16u32, 16u32);
        let mut frame = vec![0u8; (w * h * 4) as usize];
        // 2x2 fully opaque red logo at (10, 10).
        let logo = vec![
            255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        composite_logo(&mut frame, w, h, &logo, 2, 2, 10.0, 10.0);
        let px = |x: u32, y: u32| {
            let o = ((y * w + x) * 4) as usize;
            (frame[o], frame[o + 1], frame[o + 2], frame[o + 3])
        };
        assert_eq!(px(10, 10), (255, 0, 0, 255));
        assert_eq!(px(11, 11), (255, 0, 0, 255));
        // Nothing outside the logo footprint was touched.
        assert_eq!(px(12, 12), (0, 0, 0, 0));
    }
}

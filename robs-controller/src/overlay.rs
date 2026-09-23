//! Scene overlays: company-logo loading/persistence and the data-string
//! rows baked into the composed output frame (see
//! `capture.rs::compose_output_frame` and the canvas preview in
//! `robs-ui-slint/src/push.rs`).

use crate::annotation_raster::DataStringRow;
use crate::state::{EventLogKind, LogoImage};
use crate::RobsController;

impl RobsController {
    /// Decode an image file and install it as the logo overlay. A failure is
    /// logged and leaves any previously loaded logo untouched.
    pub fn set_logo_from_path(&mut self, path: String) {
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                self.overlay.logo = Some(LogoImage {
                    rgba: rgba.into_raw(),
                    width,
                    height,
                    path: path.clone(),
                });
                self.overlay.logo_resized = None;
                self.log_event(
                    format!("Logo overlay loaded: {path} ({width}x{height})"),
                    EventLogKind::Overlay,
                );
            }
            Err(e) => {
                self.log_event(
                    format!("Failed to load logo '{path}': {e}"),
                    EventLogKind::Overlay,
                );
            }
        }
    }

    /// Current overlay state projected into the persisted settings section.
    pub fn overlay_settings(&self) -> robs_profiles::settings::OverlaySettings {
        robs_profiles::settings::OverlaySettings {
            data_string_enabled: self.overlay.data_string_enabled,
            logo_enabled: self.overlay.logo_enabled,
            logo_path: self
                .overlay
                .logo
                .as_ref()
                .map(|logo| logo.path.clone())
                .unwrap_or_default(),
            logo_x: self.overlay.logo_position.x,
            logo_y: self.overlay.logo_position.y,
            logo_height_fraction: self.overlay.logo_height_fraction,
        }
    }

    /// Persist the `overlay` section (best-effort, like the other savers).
    pub fn save_overlay_settings(&self) {
        let _ = self.overlay_settings().save();
    }

    /// The two data-string groups (bottom-left, bottom-right) built from the
    /// latest telemetry snapshot — same field mapping and unit suffixes as
    /// the bottom telemetry bar (`robs-ui-slint/src/telemetry_glue.rs`).
    pub fn data_string_rows(&self) -> Vec<Vec<DataStringRow>> {
        const EMPTY: &str = "—";
        let snap = self.telemetry.snapshot.read();
        let field = |key: &str| -> Option<String> {
            snap.fields
                .get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let with_unit = |raw: Option<String>, unit: &str| -> String {
            raw.map(|v| format!("{v}{unit}"))
                .unwrap_or_else(|| EMPTY.to_string())
        };
        let row = |label: &str, value: String| DataStringRow {
            label: label.to_string(),
            value,
        };
        vec![
            vec![
                row("EASTING", with_unit(field("E"), " m")),
                row("NORTHING", with_unit(field("N"), " m")),
                row("DATE", field("D").unwrap_or_else(|| EMPTY.to_string())),
                row("TIME", field("T").unwrap_or_else(|| EMPTY.to_string())),
            ],
            vec![
                row("ROV HEADING", with_unit(field("H"), "°")),
                row("DEPTH", with_unit(field("Z"), " m")),
                row("CP VALUE", with_unit(field("CP"), " mV")),
                row("FG VALUE", field("FG").unwrap_or_else(|| EMPTY.to_string())),
            ],
        ]
    }

    /// The logo resized for an output frame `out_h` pixels tall, cached so
    /// the filter runs once per size instead of once per frame. Returns
    /// `(rgba, width, height)`.
    pub fn logo_for_output(&mut self, out_h: u32) -> Option<(Vec<u8>, u32, u32)> {
        let target = {
            let logo = self.overlay.logo.as_ref()?;
            let dst_h = (self.overlay.logo_height_fraction.clamp(0.02, 0.5)
                * out_h.max(1) as f32)
                .round()
                .max(1.0) as u32;
            let dst_w = ((dst_h as f32 * logo.width.max(1) as f32
                / logo.height.max(1) as f32)
                .round() as u32)
                .max(1);
            match &self.overlay.logo_resized {
                Some((w, h, _)) if *w == dst_w && *h == dst_h => None,
                _ => Some((dst_w, dst_h)),
            }
        };
        if let Some((dst_w, dst_h)) = target {
            let logo = self.overlay.logo.as_ref()?;
            let src = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(
                logo.width.max(1),
                logo.height.max(1),
                logo.rgba.clone(),
            )?;
            let resized =
                image::imageops::resize(&src, dst_w, dst_h, image::imageops::FilterType::Triangle);
            self.overlay.logo_resized = Some((dst_w, dst_h, resized.into_raw()));
        }
        self.overlay
            .logo_resized
            .as_ref()
            .map(|(w, h, rgba)| (rgba.clone(), *w, *h))
    }
}

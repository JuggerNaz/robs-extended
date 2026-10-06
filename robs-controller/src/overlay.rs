//! Scene overlays: company-logo loading/persistence and the data-string
//! rows baked into the composed output frame (see
//! `capture.rs::compose_output_frame` and the canvas preview in
//! `robs-ui-slint/src/push.rs`).

use crate::annotation_raster::DataStringRow;
use crate::state::{EventLogKind, LogoImage, OverlayState};
use crate::telemetry::TelemetrySnapshot;
use crate::RobsController;

/// Service owning the scene-overlay state cluster ([`OverlayState`]): the
/// company-logo lifecycle and the data-string projection baked into the
/// composed output frame. Cross-service reads (the telemetry snapshot) are
/// explicit parameters; everything else lives behind the `Deref` impls so
/// view code (`controller.overlay.logo …`) is unchanged.
#[derive(Default)]
pub struct OverlayService {
    state: OverlayState,
}

impl OverlayService {
    /// Decode an image file and install it as the logo overlay, replacing
    /// any previously loaded one. Returns the event-log line for the outcome
    /// (`Ok` on success, `Err` on failure) — logging stays with the facade
    /// because the event log is facade-owned state.
    pub fn install_logo(&mut self, path: &str) -> Result<String, String> {
        match image::open(path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                self.logo = Some(LogoImage {
                    rgba: rgba.into_raw(),
                    width,
                    height,
                    path: path.to_string(),
                });
                self.logo_resized = None;
                Ok(format!("Logo overlay loaded: {path} ({width}x{height})"))
            }
            Err(e) => Err(format!("Failed to load logo '{path}': {e}")),
        }
    }

    /// Current overlay state projected into the persisted settings section.
    pub fn overlay_settings(&self) -> robs_profiles::settings::OverlaySettings {
        robs_profiles::settings::OverlaySettings {
            data_string_enabled: self.data_string_enabled,
            logo_enabled: self.logo_enabled,
            logo_path: self
                .logo
                .as_ref()
                .map(|logo| logo.path.clone())
                .unwrap_or_default(),
            logo_x: self.logo_position.x,
            logo_y: self.logo_position.y,
            logo_height_fraction: self.logo_height_fraction,
        }
    }

    /// Persist the `overlay` section (best-effort, like the other savers).
    pub fn save_overlay_settings(&self) {
        let _ = self.overlay_settings().save();
    }

    /// The two data-string groups (bottom-left, bottom-right) built from a
    /// telemetry snapshot — same field mapping and unit suffixes as the
    /// bottom telemetry bar (`robs-ui-slint/src/telemetry_glue.rs`). The
    /// snapshot is passed in explicitly: it belongs to TelemetryService.
    pub fn data_string_rows(&self, snapshot: &TelemetrySnapshot) -> Vec<Vec<DataStringRow>> {
        const EMPTY: &str = "—";
        let field = |key: &str| -> Option<String> {
            snapshot
                .fields
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
            let logo = self.logo.as_ref()?;
            let dst_h = (self.logo_height_fraction.clamp(0.02, 0.5) * out_h.max(1) as f32)
                .round()
                .max(1.0) as u32;
            let dst_w = ((dst_h as f32 * logo.width.max(1) as f32
                / logo.height.max(1) as f32)
                .round() as u32)
                .max(1);
            match &self.logo_resized {
                Some((w, h, _)) if *w == dst_w && *h == dst_h => None,
                _ => Some((dst_w, dst_h)),
            }
        };
        if let Some((dst_w, dst_h)) = target {
            let logo = self.logo.as_ref()?;
            let src = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(
                logo.width.max(1),
                logo.height.max(1),
                logo.rgba.clone(),
            )?;
            let resized =
                image::imageops::resize(&src, dst_w, dst_h, image::imageops::FilterType::Triangle);
            self.logo_resized = Some((dst_w, dst_h, resized.into_raw()));
        }
        self.logo_resized
            .as_ref()
            .map(|(w, h, rgba)| (rgba.clone(), *w, *h))
    }
}

impl std::ops::Deref for OverlayService {
    type Target = OverlayState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl std::ops::DerefMut for OverlayService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl RobsController {
    /// Decode an image file and install it as the logo overlay. A failure is
    /// logged and leaves any previously loaded logo untouched.
    pub fn set_logo_from_path(&mut self, path: String) {
        let message = self.overlay.install_logo(&path);
        match message {
            Ok(line) => self.log_event(line, EventLogKind::Overlay),
            Err(line) => self.log_event(line, EventLogKind::Overlay),
        }
    }

    /// Current overlay state projected into the persisted settings section.
    pub fn overlay_settings(&self) -> robs_profiles::settings::OverlaySettings {
        self.overlay.overlay_settings()
    }

    /// Persist the `overlay` section (best-effort, like the other savers).
    pub fn save_overlay_settings(&self) {
        self.overlay.save_overlay_settings();
    }

    /// The two data-string groups (bottom-left, bottom-right) built from the
    /// latest telemetry snapshot — same field mapping and unit suffixes as
    /// the bottom telemetry bar (`robs-ui-slint/src/telemetry_glue.rs`).
    pub fn data_string_rows(&self) -> Vec<Vec<DataStringRow>> {
        let snap = self.telemetry.snapshot.read();
        self.overlay.data_string_rows(&snap)
    }

    /// The logo resized for an output frame `out_h` pixels tall (cached per
    /// size; see `OverlayService::logo_for_output`).
    pub fn logo_for_output(&mut self, out_h: u32) -> Option<(Vec<u8>, u32, u32)> {
        self.overlay.logo_for_output(out_h)
    }
}

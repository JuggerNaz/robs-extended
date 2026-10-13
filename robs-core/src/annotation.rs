//! Annotation / mark-up shapes drawn over a scene preview.
//!
//! Annotations are lightweight vector overlays (arrow, line, rectangle,
//! ellipse) defined in scene output coordinates. They follow the same data
//! conventions as [`crate::scene::SceneItem`]: private fields with
//! getter/setter accessors and TOML-friendly serde derives.

use serde::{Deserialize, Serialize};

use crate::scene::Position;
use crate::types::AnnotationId;

/// RGBA color packed as `[r, g, b, a]`, matching the convention used by
/// [`crate::scene::Scene::background_color`].
pub type AnnotationColor = [u8; 4];

/// Default text font size in scene units (the historical hardcoded value,
/// now only the seed for new text annotations).
pub const DEFAULT_TEXT_FONT_SIZE: f32 = 32.0;

/// One selectable text font preset: the UI/persisted name, the Slint
/// `font-family` for the live preview, and per-platform TrueType candidates
/// for the raster (bake) path — regular and bold resolved independently,
/// first existing file wins. The Linux DejaVu entries double as the
/// fallback for presets the OS does not ship (e.g. Segoe UI off Windows).
pub struct TextFontPreset {
    /// Name shown in the UI and stored on [`Annotation::set_font_family`].
    pub name: &'static str,
    /// Family name passed to Slint's `font-family` in the preview.
    pub slint_family: &'static str,
    /// Candidate .ttf paths for the regular face.
    pub regular_ttfs: &'static [&'static str],
    /// Candidate .ttf paths for the bold face (`Impact` ships none —
    /// consumers fall back to the regular face).
    pub bold_ttfs: &'static [&'static str],
}

/// The toolbar's font list, in display order. `TEXT_FONT_PRESETS[0]`
/// (Arial) is the default for new text annotations.
pub const TEXT_FONT_PRESETS: &[TextFontPreset] = &[
    TextFontPreset {
        name: "Arial",
        slint_family: "Arial",
        regular_ttfs: &["C:\\Windows\\Fonts\\arial.ttf", "/System/Library/Fonts/Supplemental/Arial.ttf", "/Library/Fonts/Arial.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
        bold_ttfs: &["C:\\Windows\\Fonts\\arialbd.ttf", "/System/Library/Fonts/Supplemental/Arial Bold.ttf", "/Library/Fonts/Arial Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"],
    },
    TextFontPreset {
        name: "Segoe UI",
        slint_family: "Segoe UI",
        regular_ttfs: &["C:\\Windows\\Fonts\\segoeui.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
        bold_ttfs: &["C:\\Windows\\Fonts\\segoeuib.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"],
    },
    TextFontPreset {
        name: "Times New Roman",
        slint_family: "Times New Roman",
        regular_ttfs: &["C:\\Windows\\Fonts\\times.ttf", "/System/Library/Fonts/Supplemental/Times New Roman.ttf", "/Library/Fonts/Times New Roman.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf"],
        bold_ttfs: &["C:\\Windows\\Fonts\\timesbd.ttf", "/System/Library/Fonts/Supplemental/Times New Roman Bold.ttf", "/Library/Fonts/Times New Roman Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Bold.ttf"],
    },
    TextFontPreset {
        name: "Courier New",
        slint_family: "Courier New",
        regular_ttfs: &["C:\\Windows\\Fonts\\cour.ttf", "/System/Library/Fonts/Supplemental/Courier New.ttf", "/Library/Fonts/Courier New.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"],
        bold_ttfs: &["C:\\Windows\\Fonts\\courbd.ttf", "/System/Library/Fonts/Supplemental/Courier New Bold.ttf", "/Library/Fonts/Courier New Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"],
    },
    TextFontPreset {
        name: "Verdana",
        slint_family: "Verdana",
        regular_ttfs: &["C:\\Windows\\Fonts\\verdana.ttf", "/System/Library/Fonts/Supplemental/Verdana.ttf", "/Library/Fonts/Verdana.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
        bold_ttfs: &["C:\\Windows\\Fonts\\verdanab.ttf", "/System/Library/Fonts/Supplemental/Verdana Bold.ttf", "/Library/Fonts/Verdana Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"],
    },
    TextFontPreset {
        name: "Georgia",
        slint_family: "Georgia",
        regular_ttfs: &["C:\\Windows\\Fonts\\georgia.ttf", "/System/Library/Fonts/Supplemental/Georgia.ttf", "/Library/Fonts/Georgia.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf"],
        bold_ttfs: &["C:\\Windows\\Fonts\\georgiab.ttf", "/System/Library/Fonts/Supplemental/Georgia Bold.ttf", "/Library/Fonts/Georgia Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Bold.ttf"],
    },
    TextFontPreset {
        name: "Impact",
        slint_family: "Impact",
        regular_ttfs: &["C:\\Windows\\Fonts\\impact.ttf", "/System/Library/Fonts/Supplemental/Impact.ttf", "/Library/Fonts/Impact.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"],
        bold_ttfs: &[],
    },
    TextFontPreset {
        name: "Consolas",
        slint_family: "Consolas",
        regular_ttfs: &["C:\\Windows\\Fonts\\consola.ttf", "/System/Library/Fonts/Menlo.ttc", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"],
        bold_ttfs: &["C:\\Windows\\Fonts\\consolab.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"],
    },
];

/// Resolve a preset by (case-insensitive) name, falling back to the first
/// preset when the name is unknown — the same resolution both render paths
/// apply per annotation.
pub fn font_preset_by_name(name: &str) -> &'static TextFontPreset {
    TEXT_FONT_PRESETS
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .unwrap_or(&TEXT_FONT_PRESETS[0])
}

fn default_text_font_size() -> f32 {
    DEFAULT_TEXT_FONT_SIZE
}

fn default_text_font_family() -> String {
    TEXT_FONT_PRESETS[0].name.to_string()
}

/// Visual appearance shared by all annotation shapes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationStyle {
    pub color: AnnotationColor,
    pub stroke_width: f32,
    /// Whether closed shapes (rectangle, ellipse) are filled.
    pub filled: bool,
}

impl Default for AnnotationStyle {
    fn default() -> Self {
        Self {
            color: [255, 235, 59, 255], // amber/yellow, OBS-like default
            stroke_width: 4.0,
            filled: false,
        }
    }
}

impl AnnotationStyle {
    pub fn new(color: AnnotationColor, stroke_width: f32) -> Self {
        Self {
            color,
            stroke_width,
            filled: false,
        }
    }

    pub fn with_filled(mut self, filled: bool) -> Self {
        self.filled = filled;
        self
    }
}

/// The geometric shape of an annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationShape {
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    /// Freehand stroke; the control points live in [`Annotation::points`].
    Pen,
    /// Text label; the anchor is [`Annotation::start`] and the content in
    /// [`Annotation::text`].
    Text,
}

impl AnnotationShape {
    /// All selectable shapes in toolbar order.
    pub const ALL: [AnnotationShape; 6] = [
        AnnotationShape::Arrow,
        AnnotationShape::Line,
        AnnotationShape::Rectangle,
        AnnotationShape::Ellipse,
        AnnotationShape::Pen,
        AnnotationShape::Text,
    ];

    /// Human-readable label shown in the UI.
    pub fn label(&self) -> &'static str {
        match self {
            AnnotationShape::Arrow => "Arrow",
            AnnotationShape::Line => "Line",
            AnnotationShape::Rectangle => "Rectangle",
            AnnotationShape::Ellipse => "Ellipse",
            AnnotationShape::Pen => "Pen",
            AnnotationShape::Text => "Text",
        }
    }

    /// Glyph used as the toolbar icon.
    pub fn icon(&self) -> &'static str {
        match self {
            AnnotationShape::Arrow => "->",
            AnnotationShape::Line => "/",
            AnnotationShape::Rectangle => "\u{25A0}",  // ■
            AnnotationShape::Ellipse => "\u{25CB}",    // ○
            AnnotationShape::Pen => "~",
            AnnotationShape::Text => "T",
        }
    }

    /// Closed shapes can be filled.
    pub fn is_closed(&self) -> bool {
        matches!(
            self,
            AnnotationShape::Rectangle | AnnotationShape::Ellipse
        )
    }
}

/// The active annotation tool in the UI. `Select` does not create shapes;
/// it selects, moves, and removes existing annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationTool {
    Select,
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    Pen,
    Text,
}

impl Default for AnnotationTool {
    fn default() -> Self {
        AnnotationTool::Select
    }
}

impl AnnotationTool {
    /// The shape this tool draws, or `None` for the `Select` tool.
    pub fn shape(&self) -> Option<AnnotationShape> {
        match self {
            AnnotationTool::Select => None,
            AnnotationTool::Arrow => Some(AnnotationShape::Arrow),
            AnnotationTool::Line => Some(AnnotationShape::Line),
            AnnotationTool::Rectangle => Some(AnnotationShape::Rectangle),
            AnnotationTool::Ellipse => Some(AnnotationShape::Ellipse),
            AnnotationTool::Pen => Some(AnnotationShape::Pen),
            AnnotationTool::Text => Some(AnnotationShape::Text),
        }
    }

    /// Human-readable label shown in the UI.
    pub fn label(&self) -> &'static str {
        match self {
            AnnotationTool::Select => "Select",
            AnnotationTool::Arrow => "Arrow",
            AnnotationTool::Line => "Line",
            AnnotationTool::Rectangle => "Rectangle",
            AnnotationTool::Ellipse => "Ellipse",
            AnnotationTool::Pen => "Pen",
            AnnotationTool::Text => "Text",
        }
    }
}

/// A single mark-up annotation drawn in scene (output) coordinates.
///
/// `start` and `end` are the two control points of the shape:
/// - Arrow / Line: tail (`start`) to head (`end`).
/// - Rectangle / Ellipse: opposite corners of the bounding box.
/// - Text: `start` is the anchor (top-left) of the label.
///
/// For the `Pen` shape the freehand polyline lives in `points`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Annotation {
    id: AnnotationId,
    shape: AnnotationShape,
    start: Position,
    end: Position,
    /// Freehand polyline (scene coordinates); used by `Pen`.
    #[serde(default)]
    points: Vec<Position>,
    /// Text content; used by `Text`.
    #[serde(default)]
    text: String,
    /// Text font size in scene units; used by `Text` (resizable via the
    /// corner drag).
    #[serde(default = "default_text_font_size")]
    font_size: f32,
    /// Whether text renders in a bold face; used by `Text`.
    #[serde(default)]
    bold: bool,
    /// Font preset name resolved via [`font_preset_by_name`]; used by
    /// `Text`.
    #[serde(default = "default_text_font_family")]
    font_family: String,
    style: AnnotationStyle,
    visible: bool,
}

impl Default for Annotation {
    fn default() -> Self {
        Self::new(AnnotationShape::Arrow, Position::zero(), Position::zero())
    }
}

impl Annotation {
    /// Create a new annotation with the given shape and control points,
    /// using the default style.
    pub fn new(shape: AnnotationShape, start: Position, end: Position) -> Self {
        Self {
            id: AnnotationId(crate::types::ObjectId::new()),
            shape,
            start,
            end,
            points: Vec::new(),
            text: String::new(),
            font_size: DEFAULT_TEXT_FONT_SIZE,
            bold: false,
            font_family: default_text_font_family(),
            style: AnnotationStyle::default(),
            visible: true,
        }
    }

    pub fn id(&self) -> AnnotationId {
        self.id
    }

    pub fn shape(&self) -> AnnotationShape {
        self.shape
    }

    pub fn start(&self) -> Position {
        self.start
    }

    pub fn end(&self) -> Position {
        self.end
    }

    /// The freehand polyline points (scene coordinates). Empty for non-Pen
    /// shapes.
    pub fn points(&self) -> &[Position] {
        &self.points
    }

    /// The text content. Empty for non-Text shapes.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text font size in scene units. Only meaningful for `Text`.
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Whether the text renders in a bold face. Only meaningful for `Text`.
    pub fn bold(&self) -> bool {
        self.bold
    }

    /// The font preset name. Only meaningful for `Text`.
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    pub fn style(&self) -> AnnotationStyle {
        self.style
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_shape(&mut self, shape: AnnotationShape) {
        self.shape = shape;
    }

    pub fn set_start(&mut self, start: Position) {
        self.start = start;
    }

    pub fn set_end(&mut self, end: Position) {
        self.end = end;
    }

    pub fn set_points(&mut self, points: Vec<Position>) {
        self.points = points;
    }

    /// Append a point to the freehand polyline.
    pub fn push_point(&mut self, point: Position) {
        self.points.push(point);
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.font_size = font_size;
    }

    pub fn set_bold(&mut self, bold: bool) {
        self.bold = bold;
    }

    pub fn set_font_family(&mut self, font_family: String) {
        self.font_family = font_family;
    }

    pub fn set_style(&mut self, style: AnnotationStyle) {
        self.style = style;
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Translate every control point (and the freehand polyline) by
    /// `(dx, dy)` in scene coordinates.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.start = Position::new(self.start.x + dx, self.start.y + dy);
        self.end = Position::new(self.end.x + dx, self.end.y + dy);
        for p in self.points.iter_mut() {
            p.x += dx;
            p.y += dy;
        }
    }

    /// Scale every control point from the bounding box `old` (min corner +
    /// size) to the box `new`, in scene coordinates — the model side of the
    /// UI's corner-resize drag: the anchor corner stays fixed while the
    /// grabbed corner follows the pointer. `Text` instead keeps its anchor
    /// `start` and scales [`Annotation::set_font_size`] by the height ratio
    /// (clamped to 8..=400 scene units). Callers pass `old_size` dims
    /// clamped to >= 1.0 so degenerate boxes (flat lines) scale 1:1 on that
    /// axis.
    pub fn rescale(
        &mut self,
        old_min: Position,
        old_size: (f32, f32),
        new_min: Position,
        new_size: (f32, f32),
    ) {
        let (ow, oh) = old_size;
        if ow <= 0.0 || oh <= 0.0 {
            return;
        }
        if self.shape == AnnotationShape::Text {
            let ratio = (new_size.1 / oh).clamp(0.02, 50.0);
            self.font_size = (self.font_size * ratio).clamp(8.0, 400.0);
            return;
        }
        let (nw, nh) = new_size;
        let map = |p: Position| -> Position {
            Position::new(
                new_min.x + (p.x - old_min.x) / ow * nw,
                new_min.y + (p.y - old_min.y) / oh * nh,
            )
        };
        self.start = map(self.start);
        self.end = map(self.end);
        for p in self.points.iter_mut() {
            *p = map(*p);
        }
    }

    /// The absolute size of the annotation in scene coordinates. For `Pen`
    /// this is the bounding box of the polyline.
    pub fn size(&self) -> (f32, f32) {
        if self.shape == AnnotationShape::Pen {
            if self.points.is_empty() {
                return (0.0, 0.0);
            }
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for p in &self.points {
                min_x = min_x.min(p.x);
                min_y = min_y.min(p.y);
                max_x = max_x.max(p.x);
                max_y = max_y.max(p.y);
            }
            (max_x - min_x, max_y - min_y)
        } else {
            (
                (self.end.x - self.start.x).abs(),
                (self.end.y - self.start.y).abs(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field-tolerant position compare (f32 math through the rescale map).
    fn pos_eq(a: Position, b: Position) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn annotations_serialized_before_text_fields_load_with_defaults() {
        // Pre-bold/font payloads carry no font_size/bold/font_family keys;
        // the serde defaults must restore the historical values.
        let json = r#"{"id":7,"shape":"Text","start":{"x":1.0,"y":2.0},"end":{"x":1.0,"y":2.0},"text":"hi","style":{"color":[255,0,0,255],"stroke_width":4.0,"filled":false},"visible":true}"#;
        let ann: Annotation = serde_json::from_str(json).unwrap();
        assert_eq!(ann.text(), "hi");
        assert_eq!(ann.font_size(), DEFAULT_TEXT_FONT_SIZE);
        assert!(!ann.bold());
        assert_eq!(ann.font_family(), "Arial");
    }

    #[test]
    fn rescale_maps_shape_points_and_keeps_anchor_corner_fixed() {
        let mut ann = Annotation::new(
            AnnotationShape::Rectangle,
            Position::new(10.0, 10.0),
            Position::new(30.0, 50.0),
        );
        // Old box min (10,10) size (20,40) -> new box min (0,0) size (40,40):
        // the min corner maps to the new min, the max corner to the new max.
        ann.rescale(
            Position::new(10.0, 10.0),
            (20.0, 40.0),
            Position::new(0.0, 0.0),
            (40.0, 40.0),
        );
        assert!(pos_eq(ann.start(), Position::new(0.0, 0.0)));
        assert!(pos_eq(ann.end(), Position::new(40.0, 40.0)));
    }

    #[test]
    fn rescale_grows_pen_points() {
        let mut ann = Annotation::new(AnnotationShape::Pen, Position::zero(), Position::zero());
        ann.push_point(Position::new(0.0, 0.0));
        ann.push_point(Position::new(10.0, 20.0));
        ann.rescale(
            Position::new(0.0, 0.0),
            (10.0, 20.0),
            Position::new(0.0, 0.0),
            (20.0, 40.0),
        );
        assert!(pos_eq(ann.points()[1], Position::new(20.0, 40.0)));
    }

    #[test]
    fn rescale_text_scales_font_size_and_keeps_anchor() {
        let mut ann =
            Annotation::new(AnnotationShape::Text, Position::new(5.0, 5.0), Position::new(5.0, 5.0));
        ann.set_text("hello".to_string());
        // Height doubles (30 -> 60): font 32 -> 64; the anchor start stays.
        ann.rescale(
            Position::new(5.0, 5.0),
            (20.0, 30.0),
            Position::new(5.0, 5.0),
            (40.0, 60.0),
        );
        assert!(pos_eq(ann.start(), Position::new(5.0, 5.0)));
        assert!((ann.font_size() - 64.0).abs() < 1e-3);
    }

    #[test]
    fn rescale_text_clamps_font_size() {
        let mut ann = Annotation::new(AnnotationShape::Text, Position::zero(), Position::zero());
        ann.rescale(
            Position::new(0.0, 0.0),
            (10.0, 10.0),
            Position::new(0.0, 0.0),
            (10.0, 10000.0),
        );
        assert_eq!(ann.font_size(), 400.0);
    }

    #[test]
    fn font_preset_lookup_is_case_insensitive_with_fallback() {
        assert_eq!(font_preset_by_name("arial").name, "Arial");
        assert_eq!(font_preset_by_name("COURIER NEW").name, "Courier New");
        assert_eq!(font_preset_by_name("nope").name, "Arial");
    }
}

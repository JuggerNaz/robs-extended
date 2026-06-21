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
            AnnotationShape::Arrow => "➤",
            AnnotationShape::Line => "／",
            AnnotationShape::Rectangle => "▭",
            AnnotationShape::Ellipse => "◯",
            AnnotationShape::Pen => "✎",
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

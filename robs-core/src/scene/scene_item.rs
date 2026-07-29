use super::capture_source::CaptureSource;
use crate::types::{ObjectId, SceneItemId, SourceId};
use serde::{Deserialize, Serialize};

/// Alignment anchor point for positioning and scaling
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Alignment {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// Position in output coordinates (not source native resolution)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

impl Position {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn zero() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

/// Scale factors (1.0 = original size, 2.0 = 2x, 0.5 = half)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Scale {
    pub x: f32,
    pub y: f32,
}

impl Scale {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn uniform(s: f32) -> Self {
        Self { x: s, y: s }
    }

    pub fn one() -> Self {
        Self { x: 1.0, y: 1.0 }
    }
}

/// Crop values in source native pixels (applied before scaling)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Crop {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Crop {
    pub fn new(left: u32, top: u32, right: u32, bottom: u32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn none() -> Self {
        Self {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        }
    }

    /// Calculate cropped dimensions
    pub fn cropped_width(&self, source_width: u32) -> u32 {
        source_width
            .saturating_sub(self.left)
            .saturating_sub(self.right)
    }

    pub fn cropped_height(&self, source_height: u32) -> u32 {
        source_height
            .saturating_sub(self.top)
            .saturating_sub(self.bottom)
    }
}

/// Bounds type for constraining item size
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BoundsType {
    #[default]
    None,
    Max,
    Scale,
    NoneKeepRatio,
    MaxKeepRatio,
}

/// A single layer in a scene with transform and crop state
/// This is the OBS sceneitem_t equivalent
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneItem {
    id: SceneItemId,
    source_id: SourceId,
    source_name: String,

    // Transform (OBS obs_transform_info equivalent)
    position: Position,
    scale: Scale,
    rotation: f32,
    alignment: Alignment,

    // Bounds (for resize constraints)
    bounds_type: BoundsType,
    bounds_alignment: Alignment,
    bounds_width: f32,
    bounds_height: f32,

    // Crop (in source native pixels)
    crop: Crop,

    // State
    visible: bool,
    locked: bool,

    // Typed capture-source metadata (`None` for non-capture sources).
    capture: Option<CaptureSource>,
}

impl SceneItem {
    /// Create a new scene item from a source
    pub fn new(source_id: SourceId, source_name: String) -> Self {
        Self {
            id: SceneItemId(ObjectId::new()),
            source_id,
            source_name,
            position: Position::zero(),
            scale: Scale::one(),
            rotation: 0.0,
            alignment: Alignment::TopLeft,
            bounds_type: BoundsType::None,
            bounds_alignment: Alignment::Center,
            bounds_width: 0.0,
            bounds_height: 0.0,
            crop: Crop::none(),
            visible: true,
            locked: false,
            capture: None,
        }
    }

    // Getters
    pub fn id(&self) -> SceneItemId {
        self.id
    }

    pub fn source_id(&self) -> SourceId {
        self.source_id
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    pub fn rotation(&self) -> f32 {
        self.rotation
    }

    pub fn alignment(&self) -> Alignment {
        self.alignment
    }

    pub fn crop(&self) -> Crop {
        self.crop
    }

    pub fn bounds_type(&self) -> BoundsType {
        self.bounds_type
    }

    pub fn bounds_alignment(&self) -> Alignment {
        self.bounds_alignment
    }

    pub fn bounds(&self) -> (f32, f32) {
        (self.bounds_width, self.bounds_height)
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Typed capture-source metadata for this item, if any.
    pub fn capture(&self) -> Option<&CaptureSource> {
        self.capture.as_ref()
    }

    // Setters
    pub fn set_position(&mut self, position: Position) {
        if !self.locked {
            self.position = position;
        }
    }

    pub fn set_scale(&mut self, scale: Scale) {
        if !self.locked {
            self.scale = scale;
        }
    }

    pub fn set_rotation(&mut self, rotation: f32) {
        if !self.locked {
            self.rotation = rotation;
        }
    }

    pub fn set_alignment(&mut self, alignment: Alignment) {
        if !self.locked {
            self.alignment = alignment;
        }
    }

    pub fn set_crop(&mut self, crop: Crop) {
        if !self.locked {
            self.crop = crop;
        }
    }

    pub fn set_bounds(
        &mut self,
        bounds_type: BoundsType,
        width: f32,
        height: f32,
        alignment: Alignment,
    ) {
        if !self.locked {
            self.bounds_type = bounds_type;
            self.bounds_width = width;
            self.bounds_height = height;
            self.bounds_alignment = alignment;
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
    }

    /// Set the typed capture-source metadata for this item.
    pub fn set_capture(&mut self, capture: Option<CaptureSource>) {
        self.capture = capture;
    }

    /// Get the source name (for UI display)
    pub fn name(&self) -> &str {
        &self.source_name
    }
}

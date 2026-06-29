use serde::{Deserialize, Serialize};

use crate::types::ObjectId;
use crate::Position;

/// A persistent text overlay rendered on the preview and baked into
/// recordings. Unlike a text annotation (drawn ad-hoc), overlays are
/// managed through a dedicated UI panel and are always rendered while
/// visible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextOverlay {
    id: ObjectId,
    text: String,
    position: Position,
    font_size: f32,
    color: [u8; 4],
    visible: bool,
}

impl Default for TextOverlay {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl TextOverlay {
    pub fn new(text: String) -> Self {
        Self {
            id: ObjectId::new(),
            text,
            position: Position::new(20.0, 20.0),
            font_size: 28.0,
            color: [255, 255, 255, 255],
            visible: true,
        }
    }

    pub fn id(&self) -> ObjectId {
        self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    pub fn color(&self) -> [u8; 4] {
        self.color
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Mutable reference to the visibility flag (for egui checkboxes).
    pub fn visible_mut(&mut self) -> &mut bool {
        &mut self.visible
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub fn set_position(&mut self, position: Position) {
        self.position = position;
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.font_size = font_size;
    }

    pub fn set_color(&mut self, color: [u8; 4]) {
        self.color = color;
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }
}

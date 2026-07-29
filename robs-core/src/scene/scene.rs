use serde::{Deserialize, Serialize};

use super::scene_item::{Alignment, BoundsType, Crop, Position, Scale, SceneItem};
use crate::types::{ObjectId, SceneId, SceneItemId, SourceId};

/// A scene contains multiple sources (scene items) arranged with transforms
/// This is the OBS obs_scene_t equivalent
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scene {
    id: SceneId,
    name: String,
    items: Vec<SceneItem>, // Z-ordered: index 0 = bottom/first rendered

    // Output resolution (default 1920x1080)
    output_width: u32,
    output_height: u32,

    // Background color (RGBA)
    background_color: [u8; 4],
}

impl Default for Scene {
    fn default() -> Self {
        Self::new("Main Scene".to_string())
    }
}

impl Scene {
    /// Create a new scene with default 1920x1080 resolution
    pub fn new(name: String) -> Self {
        Self {
            id: SceneId(ObjectId::new()),
            name,
            items: Vec::new(),
            output_width: 1920,
            output_height: 1080,
            background_color: [0, 0, 0, 255], // Black
        }
    }

    /// Create a new scene with custom resolution
    pub fn with_resolution(name: String, width: u32, height: u32) -> Self {
        Self {
            id: SceneId(ObjectId::new()),
            name,
            items: Vec::new(),
            output_width: width,
            output_height: height,
            background_color: [0, 0, 0, 255],
        }
    }

    // Getters
    pub fn id(&self) -> SceneId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn items(&self) -> &[SceneItem] {
        &self.items
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.output_width, self.output_height)
    }

    pub fn background_color(&self) -> [u8; 4] {
        self.background_color
    }

    /// Get item by ID
    pub fn item(&self, item_id: SceneItemId) -> Option<&SceneItem> {
        self.items.iter().find(|item| item.id() == item_id)
    }

    /// Get item by ID (mutable)
    pub fn item_mut(&mut self, item_id: SceneItemId) -> Option<&mut SceneItem> {
        self.items.iter_mut().find(|item| item.id() == item_id)
    }

    /// Get item index by ID
    pub fn item_index(&self, item_id: SceneItemId) -> Option<usize> {
        self.items.iter().position(|item| item.id() == item_id)
    }

    /// Get visible items in render order (top to bottom for rendering)
    pub fn visible_items(&self) -> Vec<&SceneItem> {
        self.items
            .iter()
            .filter(|item| item.is_visible())
            .rev()
            .collect()
    }

    // Setters
    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    pub fn set_output_resolution(&mut self, width: u32, height: u32) {
        self.output_width = width;
        self.output_height = height;
    }

    pub fn set_background_color(&mut self, color: [u8; 4]) {
        self.background_color = color;
    }

    // Item management

    /// Add a source to the scene (creates new scene item)
    pub fn add_source(&mut self, source_id: SourceId, source_name: String) -> SceneItemId {
        let item = SceneItem::new(source_id, source_name);
        let id = item.id();
        self.items.push(item);
        id
    }

    /// Remove an item from the scene
    pub fn remove_item(&mut self, item_id: SceneItemId) -> bool {
        if let Some(pos) = self.item_index(item_id) {
            self.items.remove(pos);
            true
        } else {
            false
        }
    }

    /// Get item count
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    // Transform setters (convenience methods)

    /// Set item position
    pub fn set_item_position(&mut self, item_id: SceneItemId, position: Position) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_position(position);
            true
        } else {
            false
        }
    }

    /// Set item scale
    pub fn set_item_scale(&mut self, item_id: SceneItemId, scale: Scale) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_scale(scale);
            true
        } else {
            false
        }
    }

    /// Set item rotation (degrees)
    pub fn set_item_rotation(&mut self, item_id: SceneItemId, rotation: f32) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_rotation(rotation);
            true
        } else {
            false
        }
    }

    /// Set item crop
    pub fn set_item_crop(&mut self, item_id: SceneItemId, crop: Crop) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_crop(crop);
            true
        } else {
            false
        }
    }

    /// Set item visibility
    pub fn set_item_visible(&mut self, item_id: SceneItemId, visible: bool) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_visible(visible);
            true
        } else {
            false
        }
    }

    /// Set item lock state
    pub fn set_item_locked(&mut self, item_id: SceneItemId, locked: bool) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_locked(locked);
            true
        } else {
            false
        }
    }

    /// Set item alignment
    pub fn set_item_alignment(&mut self, item_id: SceneItemId, alignment: Alignment) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_alignment(alignment);
            true
        } else {
            false
        }
    }

    /// Set item bounds (for resize constraints)
    pub fn set_item_bounds(
        &mut self,
        item_id: SceneItemId,
        bounds_type: BoundsType,
        width: f32,
        height: f32,
        alignment: Alignment,
    ) -> bool {
        if let Some(item) = self.item_mut(item_id) {
            item.set_bounds(bounds_type, width, height, alignment);
            true
        } else {
            false
        }
    }

    // Z-order management

    /// Move item to a specific z-order position
    pub fn reorder_item(&mut self, item_id: SceneItemId, new_index: usize) -> bool {
        if let Some(current_index) = self.item_index(item_id) {
            if current_index == new_index {
                return true;
            }

            let item = self.items.remove(current_index);
            let target_index = new_index.min(self.items.len());
            self.items.insert(target_index, item);
            true
        } else {
            false
        }
    }

    /// Move item up in z-order (render on top)
    pub fn move_item_up(&mut self, item_id: SceneItemId) -> bool {
        if let Some(idx) = self.item_index(item_id) {
            if idx < self.items.len() - 1 {
                self.items.swap(idx, idx + 1);
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Move item down in z-order (render below)
    pub fn move_item_down(&mut self, item_id: SceneItemId) -> bool {
        if let Some(idx) = self.item_index(item_id) {
            if idx > 0 {
                self.items.swap(idx, idx - 1);
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Move item to top (render last/on top)
    pub fn move_item_to_top(&mut self, item_id: SceneItemId) -> bool {
        if let Some(idx) = self.item_index(item_id) {
            if idx < self.items.len() - 1 {
                let item = self.items.remove(idx);
                self.items.push(item);
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Move item to bottom (render first/bottom)
    pub fn move_item_to_bottom(&mut self, item_id: SceneItemId) -> bool {
        if let Some(idx) = self.item_index(item_id) {
            if idx > 0 {
                let item = self.items.remove(idx);
                self.items.insert(0, item);
                true
            } else {
                false
            }
        } else {
            false
        }
    }
}

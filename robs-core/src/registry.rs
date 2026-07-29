use std::collections::HashMap;
use std::sync::Arc;

pub struct Registry<T> {
    items: HashMap<String, Arc<T>>,
}

impl<T> Registry<T> {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: &str, item: T) {
        self.items.insert(name.to_string(), Arc::new(item));
    }

    pub fn unregister(&mut self, name: &str) {
        self.items.remove(name);
    }

    pub fn get(&self, name: &str) -> Option<Arc<T>> {
        self.items.get(name).cloned()
    }

    pub fn list(&self) -> Vec<&str> {
        self.items.keys().map(|s| s.as_str()).collect()
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }
}

pub type SourceRegistry = Registry<Box<dyn crate::traits::SourceFactory>>;
pub type EncoderRegistry = Registry<Box<dyn crate::traits::EncoderFactory>>;
pub type OutputRegistry = Registry<Box<dyn crate::traits::OutputFactory>>;

/// Registry for managing multiple scenes
pub struct SceneCollection {
    scenes: HashMap<String, crate::scene::Scene>,
    current_scene_name: Option<String>,
}

impl SceneCollection {
    pub fn new() -> Self {
        Self {
            scenes: HashMap::new(),
            current_scene_name: None,
        }
    }

    /// Create a new scene and add it to the collection
    pub fn create_scene(&mut self, name: String) -> Option<&crate::scene::Scene> {
        let scene = crate::scene::Scene::new(name.clone());
        self.scenes.insert(name.clone(), scene);
        self.scenes.get(&name)
    }

    /// Create a new scene with custom resolution
    pub fn create_scene_with_resolution(
        &mut self,
        name: String,
        width: u32,
        height: u32,
    ) -> Option<&crate::scene::Scene> {
        let scene = crate::scene::Scene::with_resolution(name.clone(), width, height);
        self.scenes.insert(name.clone(), scene);
        self.scenes.get(&name)
    }

    /// Get a scene by name
    pub fn get(&self, name: &str) -> Option<&crate::scene::Scene> {
        self.scenes.get(name)
    }

    /// Get a scene by name (mutable)
    pub fn get_mut(&mut self, name: &str) -> Option<&mut crate::scene::Scene> {
        self.scenes.get_mut(name)
    }

    /// Get the current scene
    pub fn current_scene(&self) -> Option<&crate::scene::Scene> {
        self.current_scene_name
            .as_ref()
            .and_then(|n| self.scenes.get(n))
    }

    /// Get the current scene (mutable)
    pub fn current_scene_mut(&mut self) -> Option<&mut crate::scene::Scene> {
        self.current_scene_name
            .as_ref()
            .and_then(|n| self.scenes.get_mut(n))
    }

    /// Set the current scene by name
    pub fn set_current_scene(&mut self, name: &str) -> bool {
        if self.scenes.contains_key(name) {
            self.current_scene_name = Some(name.to_string());
            true
        } else {
            false
        }
    }

    /// Get the current scene name
    pub fn current_scene_name(&self) -> Option<&str> {
        self.current_scene_name.as_deref()
    }

    /// List all scene names
    pub fn list(&self) -> Vec<&str> {
        self.scenes.keys().map(|s| s.as_str()).collect()
    }

    /// Remove a scene by name
    pub fn remove(&mut self, name: &str) -> bool {
        if name == self.current_scene_name.as_deref().unwrap_or("") {
            self.current_scene_name = None;
        }
        self.scenes.remove(name).is_some()
    }

    /// Get the number of scenes
    pub fn count(&self) -> usize {
        self.scenes.len()
    }

    /// Check if a scene exists
    pub fn exists(&self, name: &str) -> bool {
        self.scenes.contains_key(name)
    }

    /// Get all scenes (for iteration)
    pub fn scenes(&self) -> &HashMap<String, crate::scene::Scene> {
        &self.scenes
    }

    /// Get all scenes (mutable)
    pub fn scenes_mut(&mut self) -> &mut HashMap<String, crate::scene::Scene> {
        &mut self.scenes
    }
}

impl Default for SceneCollection {
    fn default() -> Self {
        Self::new()
    }
}

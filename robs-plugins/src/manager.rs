use crate::{Plugin, PluginInfo, UnknownPlugin};
use anyhow::Result;
use parking_lot::RwLock;
use robs_core::registry::*;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

pub struct PluginManager {
    plugins: HashMap<String, Arc<dyn Plugin>>,
    plugin_dirs: Vec<PathBuf>,
    source_registry: Arc<RwLock<SourceRegistry>>,
    encoder_registry: Arc<RwLock<EncoderRegistry>>,
    output_registry: Arc<RwLock<OutputRegistry>>,
}

impl PluginManager {
    pub fn new(
        source_registry: Arc<RwLock<SourceRegistry>>,
        encoder_registry: Arc<RwLock<EncoderRegistry>>,
        output_registry: Arc<RwLock<OutputRegistry>>,
    ) -> Self {
        let plugin_dirs = vec![PathBuf::from("./plugins")];

        Self {
            plugins: HashMap::new(),
            plugin_dirs,
            source_registry,
            encoder_registry,
            output_registry,
        }
    }

    pub fn add_plugin_dir(&mut self, dir: PathBuf) {
        self.plugin_dirs.push(dir);
    }

    pub fn discover_plugins(&self) -> Vec<String> {
        let mut discovered = Vec::new();

        for dir in &self.plugin_dirs {
            if !dir.exists() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

                    if ext == "dll" || ext == "so" || ext == "dylib" {
                        discovered.push(path.to_string_lossy().into_owned());
                    }
                }
            }
        }

        discovered
    }

    pub fn load_plugin(&mut self, path: &str) -> Result<Arc<dyn Plugin>> {
        let info = PluginInfo {
            name: path.to_string(),
            version: "1.0".into(),
            author: "Unknown".into(),
            description: "Loaded plugin".into(),
            path: path.into(),
        };

        let plugin: Arc<dyn Plugin> = Arc::new(UnknownPlugin { info });

        self.register_plugin_components(&plugin);

        self.plugins.insert(path.to_string(), plugin.clone());

        println!("[PluginManager] Loaded: {}", path);

        Ok(plugin)
    }

    pub fn unload_plugin(&mut self, name: &str) -> Result<()> {
        if let Some(plugin) = self.plugins.remove(name) {
            self.unregister_plugin_components(&plugin);
            println!("[PluginManager] Unloaded: {}", name);
        }
        Ok(())
    }

    fn register_plugin_components(&self, plugin: &Arc<dyn Plugin>) {
        let caps = plugin.capabilities();

        if caps.sources {
            let mut registry = self.source_registry.write();
            let factories = plugin.get_sources();
            for factory in factories {
                let type_name = factory.source_type().to_string();
                registry.register(&type_name, factory);
            }
        }

        if caps.encoders {
            let mut registry = self.encoder_registry.write();
            let factories = plugin.get_encoders();
            for factory in factories {
                let type_name = factory.encoder_type().to_string();
                registry.register(&type_name, factory);
            }
        }

        if caps.outputs {
            let mut registry = self.output_registry.write();
            let factories = plugin.get_outputs();
            for factory in factories {
                let type_name = factory.output_type().to_string();
                registry.register(&type_name, factory);
            }
        }
    }

    fn unregister_plugin_components(&self, plugin: &Arc<dyn Plugin>) {
        let caps = plugin.capabilities();

        if caps.sources {
            let mut registry = self.source_registry.write();
            let factories = plugin.get_sources();
            for factory in factories {
                let type_name = factory.source_type().to_string();
                registry.unregister(&type_name);
            }
        }

        if caps.encoders {
            let mut registry = self.encoder_registry.write();
            let factories = plugin.get_encoders();
            for factory in factories {
                let type_name = factory.encoder_type().to_string();
                registry.unregister(&type_name);
            }
        }

        if caps.outputs {
            let mut registry = self.output_registry.write();
            let factories = plugin.get_outputs();
            for factory in factories {
                let type_name = factory.output_type().to_string();
                registry.unregister(&type_name);
            }
        }
    }

    pub fn list_plugins(&self) -> Vec<&str> {
        self.plugins.keys().map(|s| s.as_str()).collect()
    }

    pub fn get_plugin(&self, name: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins.get(name).cloned()
    }
}

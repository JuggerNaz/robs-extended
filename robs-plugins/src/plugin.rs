use anyhow::Result;
use robs_core::traits::*;

pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub path: String,
}

#[derive(Clone)]
pub struct PluginCapabilities {
    pub sources: bool,
    pub encoders: bool,
    pub outputs: bool,
    pub audio: bool,
    pub ui: bool,
}

impl Default for PluginCapabilities {
    fn default() -> Self {
        Self {
            sources: false,
            encoders: false,
            outputs: false,
            audio: false,
            ui: false,
        }
    }
}

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn author(&self) -> &str;
    fn description(&self) -> &str;

    fn capabilities(&self) -> PluginCapabilities;

    fn get_sources(&self) -> Vec<Box<dyn SourceFactory>>;
    fn get_encoders(&self) -> Vec<Box<dyn EncoderFactory>>;
    fn get_outputs(&self) -> Vec<Box<dyn OutputFactory>>;

    fn initialize(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
}

pub struct UnknownPlugin {
    pub info: PluginInfo,
}

impl UnknownPlugin {
    pub fn new(info: PluginInfo) -> Self {
        Self { info }
    }
}

impl Plugin for UnknownPlugin {
    fn name(&self) -> &str {
        &self.info.name
    }
    fn version(&self) -> &str {
        &self.info.version
    }
    fn author(&self) -> &str {
        &self.info.author
    }
    fn description(&self) -> &str {
        &self.info.description
    }
    fn capabilities(&self) -> PluginCapabilities {
        PluginCapabilities::default()
    }
    fn get_sources(&self) -> Vec<Box<dyn SourceFactory>> {
        vec![]
    }
    fn get_encoders(&self) -> Vec<Box<dyn EncoderFactory>> {
        vec![]
    }
    fn get_outputs(&self) -> Vec<Box<dyn OutputFactory>> {
        vec![]
    }
    fn initialize(&mut self) -> Result<()> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}

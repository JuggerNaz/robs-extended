use crate::types::*;
use crate::traits::*;
use anyhow::Result;
use std::sync::Arc;
use parking_lot::RwLock;
use std::collections::HashMap;

pub struct Pipeline {
    video_pipeline: VideoPipeline,
    audio_pipelines: HashMap<TrackId, AudioPipeline>,
    output_manager: OutputManager,
    running: bool,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            video_pipeline: VideoPipeline::new(),
            audio_pipelines: HashMap::new(),
            output_manager: OutputManager::new(),
            running: false,
        }
    }
    
    pub fn add_video_source(&mut self, source: Box<dyn VideoSource>) -> SourceId {
        self.video_pipeline.add_source(source)
    }
    
    pub fn remove_video_source(&mut self, id: SourceId) -> Result<()> {
        self.video_pipeline.remove_source(id)
    }
    
    pub fn set_video_encoder(&mut self, encoder: Box<dyn Encoder>) -> EncoderId {
        self.video_pipeline.set_encoder(encoder)
    }
    
    pub fn add_audio_track(&mut self) -> TrackId {
        let track = TrackId(self.audio_pipelines.len() as u32);
        self.audio_pipelines.insert(track, AudioPipeline::new(track));
        track
    }
    
    pub fn remove_audio_track(&mut self, track: TrackId) -> Result<()> {
        self.audio_pipelines.remove(&track);
        Ok(())
    }
    
    pub fn add_audio_source_to_track(&mut self, track: TrackId, source: Box<dyn AudioSource>) -> Result<SourceId> {
        let pipeline = self.audio_pipelines.get_mut(&track)
            .ok_or_else(|| anyhow::anyhow!("Track not found"))?;
        Ok(pipeline.add_source(source))
    }
    
    pub fn set_audio_encoder(&mut self, track: TrackId, encoder: Box<dyn Encoder>) -> Result<()> {
        let pipeline = self.audio_pipelines.get_mut(&track)
            .ok_or_else(|| anyhow::anyhow!("Track not found"))?;
        pipeline.set_encoder(encoder);
        Ok(())
    }
    
    pub fn add_output(&mut self, output: Box<dyn Output>) -> OutputId {
        self.output_manager.add_output(output)
    }
    
    pub fn remove_output(&mut self, id: OutputId) -> Result<()> {
        self.output_manager.remove_output(id)
    }
    
    pub async fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        
        self.running = true;
        self.video_pipeline.start().await?;
        for pipeline in self.audio_pipelines.values_mut() {
            pipeline.start().await?;
        }
        self.output_manager.connect_all().await?;
        
        Ok(())
    }
    
    pub async fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }
        
        self.running = false;
        self.output_manager.disconnect_all().await?;
        self.video_pipeline.stop().await?;
        for pipeline in self.audio_pipelines.values_mut() {
            pipeline.stop().await?;
        }
        
        Ok(())
    }
    
    pub fn is_running(&self) -> bool {
        self.running
    }
}

pub struct VideoPipeline {
    sources: HashMap<SourceId, Arc<RwLock<Box<dyn VideoSource>>>>,
    encoder: Option<Arc<RwLock<Box<dyn Encoder>>>>,
    active: bool,
}

impl VideoPipeline {
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
            encoder: None,
            active: false,
        }
    }
    
    pub fn add_source(&mut self, source: Box<dyn VideoSource>) -> SourceId {
        let id = source.id();
        self.sources.insert(id, Arc::new(RwLock::new(source)));
        id
    }
    
    pub fn remove_source(&mut self, id: SourceId) -> Result<()> {
        self.sources.remove(&id);
        Ok(())
    }
    
    pub fn set_encoder(&mut self, encoder: Box<dyn Encoder>) -> EncoderId {
        let id = encoder.id();
        self.encoder = Some(Arc::new(RwLock::new(encoder)));
        id
    }
    
    pub async fn start(&mut self) -> Result<()> {
        self.active = true;
        for source in self.sources.values() {
            let mut s = source.write();
            s.activate().await?;
        }
        Ok(())
    }
    
    pub async fn stop(&mut self) -> Result<()> {
        self.active = false;
        for source in self.sources.values() {
            let mut s = source.write();
            s.deactivate().await?;
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub struct AudioPipeline {
    track: TrackId,
    sources: HashMap<SourceId, Arc<RwLock<Box<dyn AudioSource>>>>,
    encoder: Option<Arc<RwLock<Box<dyn Encoder>>>>,
    mixer: AudioMixer,
    active: bool,
}

impl AudioPipeline {
    pub fn new(track: TrackId) -> Self {
        Self {
            track,
            sources: HashMap::new(),
            encoder: None,
            mixer: AudioMixer::new(),
            active: false,
        }
    }
    
    pub fn add_source(&mut self, source: Box<dyn AudioSource>) -> SourceId {
        let id = source.id();
        self.sources.insert(id, Arc::new(RwLock::new(source)));
        id
    }
    
    pub fn set_encoder(&mut self, encoder: Box<dyn Encoder>) {
        self.encoder = Some(Arc::new(RwLock::new(encoder)));
    }
    
    pub async fn start(&mut self) -> Result<()> {
        self.active = true;
        for source in self.sources.values() {
            let mut s = source.write();
            s.activate().await?;
        }
        Ok(())
    }
    
    pub async fn stop(&mut self) -> Result<()> {
        self.active = false;
        for source in self.sources.values() {
            let mut s = source.write();
            s.deactivate().await?;
        }
        Ok(())
    }
}

pub struct AudioMixer {
    sample_rate: u32,
    #[allow(dead_code)]
    channels: usize,
}

impl AudioMixer {
    pub fn new() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
        }
    }
    
    pub fn mix(&self, inputs: &[AudioFrame]) -> AudioFrame {
        if inputs.is_empty() {
            return AudioFrame::new(1024, &AudioInfo::default());
        }
        
        let max_frames = inputs.iter().map(|f| f.frames).max().unwrap_or(1024);
        let mixed = AudioFrame::new(max_frames, &AudioInfo {
            sample_rate: self.sample_rate,
            format: AudioFormat::F32,
            speakers: vec![AudioSpeaker::FL, AudioSpeaker::FR],
        });
        
        mixed
    }
}

pub struct OutputManager {
    outputs: HashMap<OutputId, Arc<RwLock<Box<dyn Output>>>>,
}

impl OutputManager {
    pub fn new() -> Self {
        Self {
            outputs: HashMap::new(),
        }
    }
    
    pub fn add_output(&mut self, output: Box<dyn Output>) -> OutputId {
        let id = output.id();
        self.outputs.insert(id, Arc::new(RwLock::new(output)));
        id
    }
    
    pub fn remove_output(&mut self, id: OutputId) -> Result<()> {
        self.outputs.remove(&id);
        Ok(())
    }
    
    pub async fn connect_all(&self) -> Result<()> {
        for output in self.outputs.values() {
            let mut o = output.write();
            o.connect().await?;
        }
        Ok(())
    }
    
    pub async fn disconnect_all(&self) -> Result<()> {
        for output in self.outputs.values() {
            let mut o = output.write();
            o.disconnect().await?;
        }
        Ok(())
    }
    
    pub async fn send_to_all(&self, packet: EncodedPacket) -> Result<()> {
        for output in self.outputs.values() {
            let mut o = output.write();
            o.send_packet(packet.clone()).await?;
        }
        Ok(())
    }
}
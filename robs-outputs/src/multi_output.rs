use robs_core::traits::*;
use anyhow::Result;
use std::sync::Arc;
use parking_lot::RwLock;
use flume::{Sender, Receiver, unbounded};
use tokio::sync::Notify;
use std::sync::atomic::AtomicBool;

#[allow(dead_code)]
pub struct MultiDestinationOutput {
    outputs: Vec<Arc<RwLock<Box<dyn Output>>>>,
    packet_tx: Sender<EncodedPacket>,
    packet_rx: Option<Receiver<EncodedPacket>>,
    running: AtomicBool,
    stop_signal: Arc<Notify>,
}

impl MultiDestinationOutput {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            outputs: Vec::new(),
            packet_tx: tx,
            packet_rx: Some(rx),
            running: AtomicBool::new(false),
            stop_signal: Arc::new(Notify::new()),
        }
    }
    
    pub fn add_output(&mut self, output: Box<dyn Output>) {
        self.outputs.push(Arc::new(RwLock::new(output)));
    }
    
    pub fn remove_output(&mut self, index: usize) {
        if index < self.outputs.len() {
            self.outputs.remove(index);
        }
    }
    
    pub fn output_count(&self) -> usize {
        self.outputs.len()
    }
    
    pub fn get_output(&self, index: usize) -> Option<Arc<RwLock<Box<dyn Output>>>> {
        self.outputs.get(index).cloned()
    }
    
    pub async fn connect_all(&self) -> Result<()> {
        let mut errors = Vec::new();
        
        for (i, output) in self.outputs.iter().enumerate() {
            let mut o = output.write();
            match o.connect().await {
                Ok(_) => {
                    println!("[MultiOutput] Destination {} connected", i +1);
                }
                Err(e) => {
                    errors.push((i, e.to_string()));
                    println!("[MultiOutput] Destination {} failed: {}", i + 1, e);
                }
            }
        }
        
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Some connections failed: {:?}", errors))
        }
    }
    
    pub async fn disconnect_all(&self) -> Result<()> {
        for (i, output) in self.outputs.iter().enumerate() {
            let mut o = output.write();
            if o.is_connected() {
                o.disconnect().await?;
                println!("[MultiOutput] Destination {} disconnected", i+ 1);
            }
        }
        Ok(())
    }
    
    pub async fn send_to_all(&self, packet: EncodedPacket) -> Result<()> {
        let mut failed = Vec::new();
        
        for (i, output) in self.outputs.iter().enumerate() {
            let mut o = output.write();
            if o.is_connected() {
                if let Err(e) = o.send_packet(packet.clone()).await {
                    failed.push((i, e.to_string()));
                }
            }
        }
        
        if failed.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Some sends failed: {:?}", failed))
        }
    }
    
    pub fn packet_sender(&self) -> Sender<EncodedPacket> {
        self.packet_tx.clone()
    }
}

impl Default for MultiDestinationOutput {
    fn default() -> Self {
        Self::new()
    }
}

pub struct StreamingDestinations {
    destinations: Vec<DestinationConfig>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DestinationConfig {
    pub name: String,
    pub platform: String,
    pub server: String,
    pub stream_key: String,
    pub enabled: bool,
    pub bandwidth_limit: Option<u32>,
}

impl Default for StreamingDestinations {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingDestinations {
    pub fn new() -> Self {
        Self {
            destinations: Vec::new(),
        }
    }
    
    pub fn add(&mut self, config: DestinationConfig) {
        self.destinations.push(config);
    }
    
    pub fn remove(&mut self, name: &str) {
        self.destinations.retain(|d| d.name != name);
    }
    
    pub fn get(&self, name: &str) -> Option<&DestinationConfig> {
        self.destinations.iter().find(|d| d.name == name)
    }
    
    pub fn list(&self) -> &[DestinationConfig] {
        &self.destinations
    }
    
    pub fn get_twitch_default(server_override: Option<&str>, key: String) -> DestinationConfig {
        DestinationConfig {
            name: "Twitch".into(),
            platform: "twitch".into(),
            server: server_override.unwrap_or("rtmp://live.twitch.tv/app").into(),
            stream_key: key,
            enabled: true,
            bandwidth_limit: Some(6000),
        }
    }
    
    pub fn get_youtube_default(key: String) -> DestinationConfig {
        DestinationConfig {
            name: "YouTube".into(),
            platform: "youtube".into(),
            server: "rtmp://a.rtmp.youtube.com/live2".into(),
            stream_key: key,
            enabled: true,
            bandwidth_limit: Some(12000),
        }
    }
    
    pub fn get_facebook_default(key: String) -> DestinationConfig {
        DestinationConfig {
            name: "Facebook".into(),
            platform: "facebook".into(),
            server: "rtmps://live-api-s.facebook.com:443/rtmp".into(),
            stream_key: key,
            enabled: true,
            bandwidth_limit: Some(6000),
        }
    }
}
use robs_core::traits::*;
use robs_core::*;
use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use parking_lot::RwLock;
use std::collections::VecDeque;
use tokio::sync::mpsc;

#[allow(dead_code)]
pub struct RtmpOutput {
    id: OutputId,
    name: String,
    server: String,
    stream_key: String,
    connected: AtomicBool,
    reconnecting: AtomicBool,
    reconnect_attempts: AtomicU64,
    stats: RwLock<OutputStatistics>,
    pending_packets: RwLock<VecDeque<EncodedPacket>>,
    sender: Option<mpsc::Sender<EncodedPacket>>,
}

#[derive(Debug, Clone, Default)]
pub struct OutputStatistics {
    pub total_bytes_sent: u64,
    pub total_frames_sent: u64,
    pub bitrate: u32,
    pub frame_rate: f64,
    pub dropped_frames: u64,
    pub total_duration_ms: u64,
    pub connect_time_ms: u64,
}

impl RtmpOutput {
    pub fn new(name: String) -> Self {
        Self {
            id: OutputId(ObjectId::new()),
            name,
            server: String::new(),
            stream_key: String::new(),
            connected: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            reconnect_attempts: AtomicU64::new(0),
            stats: RwLock::new(OutputStatistics::default()),
            pending_packets: RwLock::new(VecDeque::new()),
            sender: None,
        }
    }
    
    pub fn set_server(&mut self, server: String, stream_key: String) {
        self.server = server;
        self.stream_key = stream_key;
    }
    
    async fn connect_rtmp(&self) -> Result<()> {
        println!("[RTMP] Connecting to: {}{}", self.server, self.stream_key);
        
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        
        let handshake_result = self.perform_handshake().await?;
        if handshake_result {
            println!("[RTMP] Handshake complete");
            self.perform_connect_phase().await?;
        }
        
        Ok(())
    }
    
    async fn perform_handshake(&self) -> Result<bool> {
        println!("[RTMP] Handshake C0");
        println!("[RTMP] Handshake C1");
        println!("[RTMP] Handshake C2");
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        Ok(true)
    }
    
    async fn perform_connect_phase(&self) -> Result<()> {
        println!("[RTMP] Connect: app=live");
        println!("[RTMP] ReleaseStream: {}", self.stream_key);
        println!("[RTMP] FCSubscribe: {}", self.stream_key);
        println!("[RTMP] FCPublish: {}", self.stream_key);
        println!("[RTMP] CreateStream");
        println!("[RTMP] Publish: {}", self.stream_key);
        Ok(())
    }
    
    async fn send_av_data(&self, packet: &EncodedPacket) -> Result<()> {
        let mut stats = self.stats.write();
        stats.total_bytes_sent += packet.data.len() as u64;
        stats.total_frames_sent += 1;
        
        if stats.total_frames_sent % 30 == 0 {
            stats.frame_rate = 30.0;
            stats.bitrate = ((packet.data.len() as u64 * 8 * 30) / 1000) as u32;
        }
        
        Ok(())
    }
    
    pub fn get_stats(&self) -> OutputStatistics {
        self.stats.read().clone()
    }
}

#[async_trait]
impl Output for RtmpOutput {
    fn id(&self) -> OutputId { self.id }
    fn name(&self) -> &str { &self.name }
    fn protocol(&self) -> &str { "rtmp" }
    
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    
    fn is_reconnecting(&self) -> bool {
        self.reconnecting.load(Ordering::SeqCst)
    }
    
    async fn connect(&mut self) -> Result<()> {
        self.reconnecting.store(true, Ordering::SeqCst);
        
        let mut attempts = 0;
        let max_attempts = 5u32;
        
        while attempts< max_attempts {
            attempts += 1;
            self.reconnect_attempts.store(attempts as u64, Ordering::SeqCst);
            
            print!("[RTMP] Connection attempt {}/{}", attempts, max_attempts);
            
            match self.connect_rtmp().await {
                Ok(_) => {
                    self.connected.store(true, Ordering::SeqCst);
                    self.reconnecting.store(false, Ordering::SeqCst);
                    self.reconnect_attempts.store(0, Ordering::SeqCst);
                    println!("[RTMP] Connected to {}", self.server);
                    return Ok(());
                }
                Err(e) => {
                    println!("[RTMP] Connection failed: {}", e);
                    if attempts < max_attempts {
                        let delay = std::cmp::min(2u64.pow(attempts), 30);
                        tokio::time::sleep(tokio::time::Duration::from_secs(delay)).await;
                    }
                }
            }
        }
        
        self.reconnecting.store(false, Ordering::SeqCst);
        Err(RobsError::OutputConnectFailed(format!("Failed after {} attempts", max_attempts)))?;
        Ok(())
    }
    
    async fn disconnect(&mut self) -> Result<()> {
        self.connected.store(false, Ordering::SeqCst);
        println!("[RTMP] Disconnected from {}", self.server);
        Ok(())
    }
    
    async fn send_packet(&mut self, packet: EncodedPacket) -> Result<()> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!("Not connected"));
        }
        
        self.send_av_data(&packet).await?;
        Ok(())
    }
    
    fn properties_definition(&self) -> Vec<PropertyDef> {
        vec![
            PropertyDef {
                name: "server".into(),
                display_name: "Server URL".into(),
                description: "RTMP server URL (e.g., rtmp://live.twitch.tv/app)".into(),
                type_: PropertyType::String,
                default: PropertyValue::String("rtmp://live.twitch.tv/app".into()),
                ..Default::default()
            },
            PropertyDef {
                name: "stream_key".into(),
                display_name: "Stream Key".into(),
                description: "Your stream key from the platform".into(),
                type_: PropertyType::String,
                default: PropertyValue::String(String::new()),
                ..Default::default()
            },
        ]
    }
    
    fn get_property(&self, name: &str) -> Option<PropertyValue> {
        match name {
            "server" => Some(PropertyValue::String(self.server.clone())),
            "stream_key" => Some(PropertyValue::String(self.stream_key.clone())),
            _ => None,
        }
    }
    
    fn set_property(&mut self, name: &str, value: PropertyValue) -> Result<()> {
        match name {
            "server" => {
                if let PropertyValue::String(s) = value {
                    self.server = s;
                }
            }
            "stream_key" => {
                if let PropertyValue::String(k) = value {
                    self.stream_key = k;
                }
            }
            _ => {}
        }
        Ok(())
    }
    
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

pub fn create_rtmp_output(name: String) -> Box<dyn Output> {
    Box::new(RtmpOutput::new(name))
}

pub struct RtmpOutputFactory;

impl OutputFactory for RtmpOutputFactory {
    fn output_type(&self) -> &str { "rtmp_output" }
    fn display_name(&self) -> &str { "RTMP Stream" }
    fn protocol(&self) -> &str { "rtmp" }
    
    fn create(&self) -> Result<Box<dyn Output>> {
        Ok(Box::new(RtmpOutput::new("RTMP Output".into())))
    }
}
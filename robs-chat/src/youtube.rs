use crate::message::*;
use crate::aggregator::ChatConnection;
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;
use parking_lot::RwLock;
use std::sync::Arc;

#[allow(dead_code)]
pub struct YouTubeChatConnection {
    channel_id: String,
    live_chat_id: Option<String>,
    api_key: Option<String>,
    connected: RwLock<bool>,
    event_tx: Sender<ChatEvent>,
}

impl YouTubeChatConnection {
    pub fn new(channel_id: String, live_chat_id: Option<String>, api_key: Option<String>, event_tx: Sender<ChatEvent>) -> Self {
        Self {
            channel_id,
            live_chat_id,
            api_key,
            connected: RwLock::new(false),
            event_tx,
        }
    }
}

#[async_trait]
impl ChatConnection for YouTubeChatConnection {
    fn platform(&self) -> ChatPlatform { ChatPlatform::YouTube }
    fn channel(&self) -> &str { &self.channel_id }
    fn is_connected(&self) -> bool { *self.connected.read() }
    
    async fn connect(&self) -> Result<()> {
        println!("[YouTube] Connecting to channel: {}", self.channel_id);
        *self.connected.write() = true;
        
        let _ = self.event_tx.send(ChatEvent::Connected(ChatPlatform::YouTube, self.channel_id.clone()));
        
        Ok(())
    }
    
    async fn disconnect(&self) -> Result<()> {
        println!("[YouTube] Disconnecting from channel: {}", self.channel_id);
        *self.connected.write() = false;
        
        let _ = self.event_tx.send(ChatEvent::Disconnected(ChatPlatform::YouTube, self.channel_id.clone()));
        
        Ok(())
    }
    
    async fn send_message(&self, message: &str) -> Result<()> {
        if !self.is_connected() {
            return Err(anyhow::anyhow!("Not connected"));
        }
        
        println!("[YouTube] Sending message to {}: {}", self.channel_id, message);
        Ok(())
    }
}

pub struct YouTubeChatConnectionBuilder {
    channel_id: String,
    live_chat_id: Option<String>,
    api_key: Option<String>,
    event_tx: Sender<ChatEvent>,
}

impl YouTubeChatConnectionBuilder {
    pub fn new(channel_id: impl Into<String>, event_tx: Sender<ChatEvent>) -> Self {
        Self {
            channel_id: channel_id.into(),
            live_chat_id: None,
            api_key: None,
            event_tx,
        }
    }
    
    pub fn live_chat_id(mut self, id: impl Into<String>) -> Self {
        self.live_chat_id = Some(id.into());
        self
    }
    
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
    
    pub fn build(self) -> Arc<dyn ChatConnection> {
        Arc::new(YouTubeChatConnection::new(
            self.channel_id,
            self.live_chat_id,
            self.api_key,
            self.event_tx,
        ))
    }
}
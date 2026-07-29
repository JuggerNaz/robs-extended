use crate::message::*;
use anyhow::Result;
use tokio::sync::mpsc::Sender;
use parking_lot::RwLock;
use std::sync::Arc;
use std::collections::VecDeque;
use chrono::{DateTime, Utc};

pub struct ChatAggregator {
    connections: RwLock<Vec<Arc<dyn ChatConnection>>>,
    message_buffer: RwLock<VecDeque<UnifiedChatMessage>>,
    event_tx: Sender<ChatEvent>,
    max_messages: usize,
}

impl ChatAggregator {
    pub fn new(event_tx: Sender<ChatEvent>, max_messages: usize) -> Self {
        Self {
            connections: RwLock::new(Vec::new()),
            message_buffer: RwLock::new(VecDeque::with_capacity(max_messages)),
            event_tx,
            max_messages,
        }
    }
    
    pub fn add_connection(&self, connection: Arc<dyn ChatConnection>) -> Result<()> {
        self.connections.write().push(connection);
        Ok(())
    }
    
    pub fn remove_connection(&self, platform: ChatPlatform, channel: &str) -> Result<()> {
        let mut conns = self.connections.write();
        conns.retain(|c| !(c.platform() == platform && c.channel() == channel));
        Ok(())
    }
    
    pub async fn connect_all(&self) -> Result<()> {
        let conns = self.connections.read();
        for conn in conns.iter() {
            conn.connect().await?;
        }
        Ok(())
    }
    
    pub async fn disconnect_all(&self) -> Result<()> {
        let conns = self.connections.read();
        for conn in conns.iter() {
            conn.disconnect().await?;
        }
        Ok(())
    }
    
    pub fn push_message(&self, message: UnifiedChatMessage) {
        let mut buffer = self.message_buffer.write();
        if buffer.len() >= self.max_messages {
            buffer.pop_front();
        }
        buffer.push_back(message);
        
        let _ = self.event_tx.send(ChatEvent::Message(Box::new(buffer.back().unwrap().clone())));
    }
    
    pub fn get_messages(&self) -> Vec<UnifiedChatMessage> {
        self.message_buffer.read().iter().cloned().collect()
    }
    
    pub fn get_messages_since(&self, since: DateTime<Utc>) -> Vec<UnifiedChatMessage> {
        self.message_buffer.read()
            .iter()
            .filter(|m| m.timestamp > since)
            .cloned()
            .collect()
    }
    
    pub fn clear(&self) {
        self.message_buffer.write().clear();
    }
    
    pub fn get_connection(&self, platform: ChatPlatform, channel: &str) -> Option<Arc<dyn ChatConnection>> {
        self.connections.read()
            .iter()
            .find(|c| c.platform() == platform && c.channel() == channel)
            .cloned()
    }
    
    pub async fn send_message(&self, platform: ChatPlatform, channel: &str, message: &str) -> Result<()> {
        if let Some(conn) = self.get_connection(platform, channel) {
            conn.send_message(message).await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait ChatConnection: Send + Sync {
    fn platform(&self) -> ChatPlatform;
    fn channel(&self) -> &str;
    fn is_connected(&self) -> bool;
    
    async fn connect(&self) -> Result<()>;
    async fn disconnect(&self) -> Result<()>;
    async fn send_message(&self, message: &str) -> Result<()>;
}

pub struct ChatConfig {
    pub twitch: TwitchConfig,
    pub youtube: YouTubeConfig,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            twitch: TwitchConfig::default(),
            youtube: YouTubeConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TwitchConfig {
    pub channel: String,
    pub oauth_token: Option<String>,
    pub username: Option<String>,
    pub connect: bool,
}

impl Default for TwitchConfig {
    fn default() -> Self {
        Self {
            channel: String::new(),
            oauth_token: None,
            username: None,
            connect: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct YouTubeConfig {
    pub channel_id: String,
    pub api_key: Option<String>,
    pub live_chat_id: Option<String>,
    pub connect: bool,
}

pub fn create_mock_chat_message(platform: ChatPlatform, channel: &str, user: &str, content: &str) -> UnifiedChatMessage {
    UnifiedChatMessage {
        id:uuid::Uuid::new_v4().to_string(),
        platform,
        channel: channel.to_string(),
        user: ChatUser {
            id: user.to_lowercase(),
            username: user.to_lowercase(),
            display_name: user.to_string(),
            color: Some(match platform {
                ChatPlatform::Twitch => "#9146FF".into(),
                ChatPlatform::YouTube => "#FF0000".into(),
                _ => "#333333".into(),
            }),
            badges: vec![],
            profile_image_url: None,
        },
        content: content.to_string(),
        timestamp: Utc::now(),
        metadata: MessageMetadata::default(),
    }
}
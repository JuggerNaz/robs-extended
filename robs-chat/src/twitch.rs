use crate::message::*;
use crate::aggregator::ChatConnection;
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;
use parking_lot::RwLock;
use std::sync::Arc;

#[allow(dead_code)]
pub struct TwitchChatConnection {
    channel: String,
    oauth_token: Option<String>,
    username: Option<String>,
    connected: RwLock<bool>,
    event_tx: Sender<ChatEvent>,
}

#[allow(dead_code)]
impl TwitchChatConnection {
    pub fn new(channel: String, oauth_token: Option<String>, username: Option<String>, event_tx: Sender<ChatEvent>) -> Self {
        Self {
            channel,
            oauth_token,
            username,
            connected: RwLock::new(false),
            event_tx,
        }
    }
    
    async fn run_irc_loop(&self) -> Result<()> {
        println!("[Twitch] Connecting to IRC...");
        
        Ok(())
    }
    
    fn parse_irc_message(&self, line: &str) -> Option<UnifiedChatMessage> {
        if line.contains("PRIVMSG") {
            let parts: Vec<&str> = line.splitn(3,' ').collect();
            if parts.len() >= 3 {
                let user = parts[1].split('!').next()?;
                let content = parts[2].trim_start_matches(':');
                
                return Some(UnifiedChatMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    platform: ChatPlatform::Twitch,
                    channel: self.channel.clone(),
                    user: ChatUser {
                        id: user.to_lowercase(),
                        username: user.to_string(),
                        display_name: user.to_string(),
                        color: None,
                        badges: vec![],
                        profile_image_url: None,
                    },
                    content: content.to_string(),
                    timestamp: chrono::Utc::now(),
                    metadata: MessageMetadata::default(),
                });
            }
        }
        None
    }
}

#[async_trait]
impl ChatConnection for TwitchChatConnection {
    fn platform(&self) -> ChatPlatform { ChatPlatform::Twitch }
    fn channel(&self) -> &str { &self.channel }
    fn is_connected(&self) -> bool { *self.connected.read() }
    
    async fn connect(&self) -> Result<()> {
        println!("[Twitch] Connecting to channel: {}", self.channel);
        *self.connected.write() = true;
        
        let _ = self.event_tx.send(ChatEvent::Connected(ChatPlatform::Twitch, self.channel.clone()));
        
        Ok(())
    }
    
    async fn disconnect(&self) -> Result<()> {
        println!("[Twitch] Disconnecting from channel: {}", self.channel);
        *self.connected.write() = false;
        
        let _ = self.event_tx.send(ChatEvent::Disconnected(ChatPlatform::Twitch, self.channel.clone()));
        
        Ok(())
    }
    
    async fn send_message(&self, message: &str) -> Result<()> {
        if !self.is_connected() {
            return Err(anyhow::anyhow!("Not connected"));
        }
        
        println!("[Twitch] Sending message to {}: {}", self.channel, message);
        Ok(())
    }
}

pub struct TwitchChatConnectionBuilder {
    channel: String,
    oauth_token: Option<String>,
    username: Option<String>,
    event_tx: Sender<ChatEvent>,
}

impl TwitchChatConnectionBuilder {
    pub fn new(channel: impl Into<String>, event_tx: Sender<ChatEvent>) -> Self {
        Self {
            channel: channel.into(),
            oauth_token: None,
            username: None,
            event_tx,
        }
    }
    
    pub fn oauth_token(mut self, token: impl Into<String>) -> Self {
        self.oauth_token = Some(token.into());
        self
    }
    
    pub fn username(mut self, name: impl Into<String>) -> Self {
        self.username = Some(name.into());
        self
    }
    
    pub fn build(self) -> Arc<dyn ChatConnection> {
        Arc::new(TwitchChatConnection::new(
            self.channel,
            self.oauth_token,
            self.username,
            self.event_tx,
        ))
    }
}
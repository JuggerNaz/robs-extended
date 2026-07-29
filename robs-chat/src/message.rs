use robs_core::event::ChatMessage;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct UnifiedChatMessage {
    pub id: String,
    pub platform: ChatPlatform,
    pub channel: String,
    pub user: ChatUser,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub metadata: MessageMetadata,
}

impl From<UnifiedChatMessage> for ChatMessage {
    fn from(msg: UnifiedChatMessage) -> Self {
        ChatMessage {
            id: msg.id,
            platform: format!("{:?}", msg.platform),
            channel: msg.channel,
            user: msg.user.display_name,
            user_id: msg.user.id,
            content: msg.content,
            timestamp: msg.timestamp.timestamp(),
            color: msg.user.color,
            badges: msg.user.badges,
            is_mod: msg.metadata.is_mod,
            is_subscriber: msg.metadata.is_subscriber,
            is_vip: msg.metadata.is_vip,
            is_broadcaster: msg.metadata.is_broadcaster,
            is_first_message: msg.metadata.is_first_message,
            is_highlighted: msg.metadata.is_highlighted,
            reply_count: msg.metadata.reply_count.unwrap_or(0),
            bits: msg.metadata.bits.unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatPlatform {
    Twitch,
    YouTube,
    Facebook,
    Trovo,
    Kick,
}

impl ChatPlatform {
    pub fn display_name(&self) -> &str {
        match self {
            ChatPlatform::Twitch => "Twitch",
            ChatPlatform::YouTube => "YouTube",
            ChatPlatform::Facebook => "Facebook",
            ChatPlatform::Trovo => "Trovo",
            ChatPlatform::Kick => "Kick",
        }
    }
    
    pub fn color_hex(&self) -> &str {
        match self {
            ChatPlatform::Twitch => "#9146FF",
            ChatPlatform::YouTube => "#FF0000",
            ChatPlatform::Facebook => "#1877F2",
            ChatPlatform::Trovo => "#34EB8B",
            ChatPlatform::Kick => "#53FC18",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatUser {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub color: Option<String>,
    pub badges: Vec<String>,
    pub profile_image_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MessageMetadata {
    pub is_mod: bool,
    pub is_subscriber: bool,
    pub is_vip: bool,
    pub is_broadcaster: bool,
    pub is_first_message: bool,
    pub is_highlighted: bool,
    pub bits: Option<u32>,
    pub reply_count: Option<u32>,
    pub custom_reward_id: Option<String>,
}

impl Default for MessageMetadata {
    fn default() -> Self {
        Self {
            is_mod: false,
            is_subscriber: false,
            is_vip: false,
            is_broadcaster: false,
            is_first_message: false,
            is_highlighted: false,
            bits: None,
            reply_count: None,
            custom_reward_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ChatEvent {
    Connected(ChatPlatform, String),
    Disconnected(ChatPlatform, String),
    Message(Box<UnifiedChatMessage>),
    UserJoined(ChatPlatform, String, String),
    UserLeft(ChatPlatform, String, String),
    Cleared(ChatPlatform, String),
    UserBanned { platform: ChatPlatform, channel: String, user: String, reason: String },
    UserTimedOut { platform: ChatPlatform, channel: String, user: String, duration: u32 },
    Raided { platform: ChatPlatform, channel: String, raider: String, viewers: u32 },
    Subscribed { platform: ChatPlatform, channel: String, user: String, tier: u16, months: u32 },
    GiftedSub { platform: ChatPlatform, channel: String, gifter: String, recipient: String },
    Donation { platform: ChatPlatform, channel: String, user: String, amount: f64, message: String },
    Error { platform: ChatPlatform, message: String },
}
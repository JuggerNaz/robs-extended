use robs_chat::aggregator::ChatAggregator;
use robs_chat::message::ChatPlatform;
use robs_controller::RobsController;
use std::sync::Arc;
use tokio::sync::mpsc;

/// The egui front-end (`robs-ui`) was removed; Slint is the only UI. `--ui`
/// is still accepted (and ignored, along with its value) so existing
/// launchers that pass `--ui slint` keep working unchanged.
fn warn_if_ui_flag() {
    for arg in std::env::args().skip(1) {
        if arg == "--ui" {
            eprintln!(
                "[ROBS] The egui UI was removed; Slint is the only front-end. Ignoring --ui and its value."
            );
            break;
        }
    }
}

fn main() {
    // Only enable ERROR level to eliminate TRACE noise
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .init();

    println!("[DEBUG] ROBS starting...");
    eprintln!("[DEBUG] ROBS starting (stderr)...");

    // Set up panic hook to capture any crashes
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic".to_string()
        };

        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());

        eprintln!("[PANIC] {} at {}", msg, location);
    }));

    println!(
        r#"
    ╔═════════════════════════════════════════╗
    ║         ROBS - Rust OBS Studio          ║
    ║         Version: 0.1.0                  ║
    ╚═════════════════════════════════════════╝
    "#
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (chat_tx, chat_rx) = mpsc::channel(1000);
    let chat_aggregator = Arc::new(ChatAggregator::new(chat_tx.clone(), 500));

    let chat_tx_clone = chat_tx.clone();
    rt.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let platforms = [ChatPlatform::Twitch, ChatPlatform::YouTube];
        let users = ["xQc", "shroud", "Ninja", "Pokimane", "summit1g", "TimTheTatman"];
        let messages = [
            "Let's go!",
            "GG",
            "This stream is amazing",
            "How did you do that?",
            "First time here, love the content",
            "Can you play some music?",
            "W stream",
            "PogChamp",
            "Hello from Brazil!",
            "Just subscribed!",
        ];
        let mut rng = fastrand::Rng::new();
        loop {
            let platform = platforms[rng.usize(..platforms.len())];
            let user = users[rng.usize(..users.len())];
            let msg = messages[rng.usize(..messages.len())];
            let mock = create_mock_chat_message(platform, "robs_channel", user, msg);
            let _ = chat_tx_clone.send(robs_chat::message::ChatEvent::Message(Box::new(mock))).await;
            tokio::time::sleep(std::time::Duration::from_millis(800 + rng.u64(0..1200))).await;
        }
    });

    warn_if_ui_flag();

    println!("[ROBS] Starting Slint UI...");

    let controller = RobsController::new().with_chat(chat_aggregator.clone(), chat_rx);
    if let Err(err) = robs_ui_slint::run(controller) {
        eprintln!("[ROBS] Slint UI exited with error: {err}");
    }
}

fn create_mock_chat_message(platform: ChatPlatform, channel: &str, user: &str, content: &str) -> robs_chat::message::UnifiedChatMessage {
    use robs_chat::message::{ChatUser, MessageMetadata};
    robs_chat::message::UnifiedChatMessage {
        id: uuid::Uuid::new_v4().to_string(),
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
        timestamp: chrono::Utc::now(),
        metadata: MessageMetadata::default(),
    }
}

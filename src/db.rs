use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use slacko::types::{Channel, Message, User};
use surrealdb::engine::local::SurrealKv;
use surrealdb::Surreal;
use tracing::{error, info};

type Db = Surreal<surrealdb::engine::local::Db>;

/// Stored login credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCredentials {
    pub auth_mode: String, // "stealth" or "bot"
    pub xoxc_token: Option<String>,
    pub xoxd_cookie: Option<String>,
    pub bot_token: Option<String>,
    pub app_token: Option<String>,
    pub workspace_url: Option<String>,
}

/// A recently used status (emoji + text).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentStatus {
    pub emoji: String,
    pub text: String,
}

/// Wrapper to store a list as a JSON blob, avoiding surrealdb `id` field conflicts.
#[derive(Debug, Serialize, Deserialize)]
struct JsonCache {
    data: String,
}

#[derive(Clone)]
pub struct Database {
    db: Db,
}

impl Database {
    pub async fn open(rt: &tokio::runtime::Handle) -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("slack-frontend");

        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create data dir: {e}"))?;

        let db_path = data_dir.join("db");
        info!("Opening database at {}", db_path.display());

        // surrealdb must be initialized on the tokio runtime
        let path = db_path.to_string_lossy().to_string();
        let db = rt
            .spawn(async move {
                let db = Surreal::new::<SurrealKv>(&path)
                    .await
                    .map_err(|e| format!("DB open error: {e}"))?;
                db.use_ns("slack")
                    .use_db("main")
                    .await
                    .map_err(|e| format!("DB namespace error: {e}"))?;
                Ok::<Db, String>(db)
            })
            .await
            .map_err(|e| format!("DB task error: {e}"))??;

        Ok(Self { db })
    }

    // ── Credentials ──

    pub async fn save_credentials(&self, creds: &SavedCredentials) -> Result<(), String> {
        // Delete then create for a clean upsert
        let _: Result<Option<SavedCredentials>, _> =
            self.db.delete(("credentials", "main")).await;
        let _: Option<SavedCredentials> = self
            .db
            .create(("credentials", "main"))
            .content(creds.clone())
            .await
            .map_err(|e| format!("DB save credentials error: {e}"))?;

        info!("Credentials saved to database");
        Ok(())
    }

    pub async fn load_credentials(&self) -> Option<SavedCredentials> {
        match self
            .db
            .select::<Option<SavedCredentials>>(("credentials", "main"))
            .await
        {
            Ok(Some(creds)) => {
                info!("Loaded saved credentials (mode: {})", creds.auth_mode);
                Some(creds)
            }
            Ok(None) => {
                info!("No saved credentials found");
                None
            }
            Err(e) => {
                error!("DB load credentials error: {e}");
                None
            }
        }
    }

    pub async fn clear_credentials(&self) {
        let _: Result<Option<SavedCredentials>, _> =
            self.db.delete(("credentials", "main")).await;
        info!("Credentials cleared from database");
    }

    // ── Channels ──

    pub async fn save_channels(&self, channels: &[Channel]) -> Result<(), String> {
        let json =
            serde_json::to_string(channels).map_err(|e| format!("JSON serialize error: {e}"))?;
        let cache = JsonCache { data: json };
        // Delete then create for a clean upsert
        let _: Result<Option<JsonCache>, _> = self.db.delete(("cache", "channels")).await;
        let _: Option<JsonCache> = self
            .db
            .create(("cache", "channels"))
            .content(cache)
            .await
            .map_err(|e| format!("DB save channels error: {e}"))?;
        info!("Saved {} channels to database", channels.len());
        Ok(())
    }

    // ── Last channel ──

    pub async fn save_last_channel(&self, channel_id: &str) {
        let _: Result<Option<serde_json::Value>, _> =
            self.db.delete(("cache", "last_channel")).await;
        let val = serde_json::json!({ "channel_id": channel_id });
        let _: Result<Option<serde_json::Value>, _> = self
            .db
            .create(("cache", "last_channel"))
            .content(val)
            .await;
    }

    pub async fn load_last_channel(&self) -> Option<String> {
        let result: Result<Option<serde_json::Value>, _> =
            self.db.select(("cache", "last_channel")).await;
        result
            .ok()
            .flatten()
            .and_then(|v| v.get("channel_id")?.as_str().map(String::from))
    }

    // ── Recent statuses ──

    /// Push a status to the front of the recent list (deduplicating, max 8).
    pub async fn push_recent_status(&self, status: &RecentStatus) {
        if status.emoji.is_empty() && status.text.is_empty() {
            return;
        }
        let mut list = self.load_recent_statuses().await;
        list.retain(|s| s != status);
        list.insert(0, status.clone());
        list.truncate(8);
        let json = serde_json::to_string(&list).unwrap_or_default();
        let cache = JsonCache { data: json };
        let _: Result<Option<JsonCache>, _> = self.db.delete(("cache", "recent_statuses")).await;
        let _: Result<Option<JsonCache>, _> = self
            .db
            .create(("cache", "recent_statuses"))
            .content(cache)
            .await;
    }

    pub async fn load_recent_statuses(&self) -> Vec<RecentStatus> {
        match self
            .db
            .select::<Option<JsonCache>>(("cache", "recent_statuses"))
            .await
        {
            Ok(Some(cache)) => {
                serde_json::from_str(&cache.data).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    // ── Users ──

    pub async fn save_users(&self, users: &[User]) -> Result<(), String> {
        let json =
            serde_json::to_string(users).map_err(|e| format!("JSON serialize error: {e}"))?;
        let cache = JsonCache { data: json };
        let _: Result<Option<JsonCache>, _> = self.db.delete(("cache", "users")).await;
        let _: Option<JsonCache> = self
            .db
            .create(("cache", "users"))
            .content(cache)
            .await
            .map_err(|e| format!("DB save users error: {e}"))?;
        info!("Saved {} users to database", users.len());
        Ok(())
    }

    pub async fn load_users(&self) -> Option<Vec<User>> {
        match self
            .db
            .select::<Option<JsonCache>>(("cache", "users"))
            .await
        {
            Ok(Some(cache)) => match serde_json::from_str(&cache.data) {
                Ok(users) => {
                    let users: Vec<User> = users;
                    info!("Loaded {} cached users from database", users.len());
                    Some(users)
                }
                Err(e) => {
                    error!("Failed to deserialize cached users: {e}");
                    None
                }
            },
            Ok(None) => {
                info!("No cached users found");
                None
            }
            Err(e) => {
                error!("DB load users error: {e}");
                None
            }
        }
    }

    // ── Channel activity ──

    /// Update the last activity timestamp for a channel.
    pub async fn update_channel_activity(&self, channel_id: &str, ts: &str) {
        let key = format!("activity_{channel_id}");
        let val = serde_json::json!({ "ts": ts });
        let _: Result<Option<serde_json::Value>, _> = self.db.delete(("cache", &*key)).await;
        let _: Result<Option<serde_json::Value>, _> = self
            .db
            .create(("cache", &*key))
            .content(val)
            .await;
    }

    /// Load all channel activity timestamps. Returns map of channel_id -> ts string.
    pub async fn load_all_channel_activity(&self) -> HashMap<String, String> {
        // Query all cache records with activity_ prefix
        let result: Result<Vec<serde_json::Value>, _> =
            self.db.select("cache").await;
        let mut map = HashMap::new();
        if let Ok(records) = result {
            for record in records {
                // SurrealDB records have an `id` field like `cache:activity_C12345`
                let id_str = record
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if let Some(channel_id) = id_str
                    .strip_prefix("cache:")
                    .and_then(|s| s.strip_prefix("activity_"))
                {
                    if let Some(ts) = record.get("ts").and_then(|v| v.as_str()) {
                        map.insert(channel_id.to_string(), ts.to_string());
                    }
                }
            }
        }
        map
    }

    // ── Messages ──

    /// Save messages for a channel (replaces existing cache).
    pub async fn save_messages(&self, channel_id: &str, messages: &[Message]) -> Result<(), String> {
        let json =
            serde_json::to_string(messages).map_err(|e| format!("JSON serialize error: {e}"))?;
        let cache = JsonCache { data: json };
        let key = format!("messages_{channel_id}");
        let _: Result<Option<JsonCache>, _> = self.db.delete(("cache", &*key)).await;
        let _: Option<JsonCache> = self
            .db
            .create(("cache", &*key))
            .content(cache)
            .await
            .map_err(|e| format!("DB save messages error: {e}"))?;
        tracing::debug!("Saved {} messages for channel {channel_id}", messages.len());
        Ok(())
    }

    /// Load cached messages for a channel.
    pub async fn load_messages(&self, channel_id: &str) -> Option<Vec<Message>> {
        let key = format!("messages_{channel_id}");
        match self
            .db
            .select::<Option<JsonCache>>(("cache", &*key))
            .await
        {
            Ok(Some(cache)) => match serde_json::from_str(&cache.data) {
                Ok(messages) => {
                    let messages: Vec<Message> = messages;
                    tracing::debug!(
                        "Loaded {} cached messages for channel {channel_id}",
                        messages.len()
                    );
                    Some(messages)
                }
                Err(e) => {
                    error!("Failed to deserialize cached messages: {e}");
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                error!("DB load messages error: {e}");
                None
            }
        }
    }

    /// Append a single message to the cached messages for a channel.
    pub async fn append_message(&self, channel_id: &str, message: &Message) {
        let mut messages = self.load_messages(channel_id).await.unwrap_or_default();
        messages.push(message.clone());
        // Keep only the last 100 messages
        if messages.len() > 100 {
            messages.drain(..messages.len() - 100);
        }
        let _ = self.save_messages(channel_id, &messages).await;
    }

    /// Update a reaction on a cached message. Returns the updated reactions list if found.
    pub async fn update_reaction(
        &self,
        channel_id: &str,
        message_ts: &str,
        reaction_name: &str,
        user_id: &str,
        added: bool,
    ) -> Option<Vec<slacko::types::Reaction>> {
        let mut messages = self.load_messages(channel_id).await?;
        let msg = messages.iter_mut().find(|m| m.ts == message_ts)?;

        let reactions = msg.reactions.get_or_insert_with(Vec::new);

        if added {
            if let Some(r) = reactions.iter_mut().find(|r| r.name == reaction_name) {
                if !r.users.contains(&user_id.to_string()) {
                    r.users.push(user_id.to_string());
                    r.count += 1;
                }
            } else {
                reactions.push(slacko::types::Reaction {
                    name: reaction_name.to_string(),
                    count: 1,
                    users: vec![user_id.to_string()],
                });
            }
        } else if let Some(r) = reactions.iter_mut().find(|r| r.name == reaction_name) {
            r.users.retain(|u| u != user_id);
            r.count = r.count.saturating_sub(1);
        }

        // Remove reactions with zero count
        reactions.retain(|r| r.count > 0);

        let updated = msg.reactions.clone();
        let _ = self.save_messages(channel_id, &messages).await;
        updated
    }

    pub async fn load_channels(&self) -> Option<Vec<Channel>> {
        match self
            .db
            .select::<Option<JsonCache>>(("cache", "channels"))
            .await
        {
            Ok(Some(cache)) => match serde_json::from_str(&cache.data) {
                Ok(channels) => {
                    let channels: Vec<Channel> = channels;
                    info!("Loaded {} cached channels from database", channels.len());
                    Some(channels)
                }
                Err(e) => {
                    error!("Failed to deserialize cached channels: {e}");
                    None
                }
            },
            Ok(None) => {
                info!("No cached channels found");
                None
            }
            Err(e) => {
                error!("DB load channels error: {e}");
                None
            }
        }
    }
}

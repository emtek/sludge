use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use slacko::types::{Channel, Message, User};
use surrealdb::engine::local::SurrealKv;
use surrealdb::types::SurrealValue;
use surrealdb::Surreal;
use tracing::{error, info};

type Db = Surreal<surrealdb::engine::local::Db>;

/// Stored login credentials.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct SavedCredentials {
    pub xoxc_token: Option<String>,
    pub xoxd_cookie: Option<String>,
    pub workspace_url: Option<String>,
}

/// A recently used status (emoji + text).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, SurrealValue)]
pub struct RecentStatus {
    pub emoji: String,
    pub text: String,
}

/// Wrapper to store a list as a JSON blob, avoiding surrealdb `id` field conflicts.
#[derive(Debug, Serialize, Deserialize, SurrealValue)]
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
            .join("sludge");

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

        // Define tables and full-text search indexes
        let mut schema_response = db.query(
            "DEFINE TABLE IF NOT EXISTS message SCHEMAFULL;
             DEFINE FIELD IF NOT EXISTS channel ON message TYPE string;
             DEFINE FIELD IF NOT EXISTS ts ON message TYPE string;
             DEFINE FIELD IF NOT EXISTS thread_ts ON message TYPE option<string>;
             DEFINE FIELD IF NOT EXISTS user ON message TYPE option<string>;
             DEFINE FIELD IF NOT EXISTS text ON message TYPE string;
             DEFINE FIELD IF NOT EXISTS data ON message TYPE string;
             DEFINE INDEX IF NOT EXISTS message_channel_ts ON message FIELDS channel, ts UNIQUE;
             DEFINE ANALYZER IF NOT EXISTS msg_analyzer TOKENIZERS blank, class FILTERS lowercase, snowball(english);
             DEFINE INDEX IF NOT EXISTS message_text_ft ON message FIELDS text
                 FULLTEXT ANALYZER msg_analyzer BM25 HIGHLIGHTS;

             DEFINE TABLE IF NOT EXISTS credentials SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS cache SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS settings SCHEMALESS;

             DEFINE TABLE IF NOT EXISTS channel_meta SCHEMAFULL;
             DEFINE FIELD IF NOT EXISTS channel ON channel_meta TYPE string;
             DEFINE FIELD IF NOT EXISTS oldest_ts ON channel_meta TYPE string;
             DEFINE FIELD IF NOT EXISTS newest_ts ON channel_meta TYPE string;
             DEFINE FIELD IF NOT EXISTS backfill_checked_at ON channel_meta TYPE option<string>;
             DEFINE INDEX IF NOT EXISTS channel_meta_channel ON channel_meta FIELDS channel UNIQUE;
            "
        )
            .await
            .map_err(|e| format!("DB schema error: {e}"))?;

        // Check each schema statement for errors
        let statement_names = [
            "DEFINE TABLE message", "DEFINE FIELD channel", "DEFINE FIELD ts",
            "DEFINE FIELD thread_ts", "DEFINE FIELD user", "DEFINE FIELD text",
            "DEFINE FIELD data", "DEFINE INDEX message_channel_ts",
            "DEFINE ANALYZER msg_analyzer", "DEFINE INDEX message_text_ft",
            "DEFINE TABLE credentials", "DEFINE TABLE cache", "DEFINE TABLE settings",
            "DEFINE TABLE channel_meta", "DEFINE FIELD channel_meta.channel",
            "DEFINE FIELD channel_meta.oldest_ts", "DEFINE FIELD channel_meta.newest_ts",
            "DEFINE FIELD channel_meta.backfill_checked_at",
            "DEFINE INDEX channel_meta_channel",
        ];
        for (i, name) in statement_names.iter().enumerate() {
            let result: Result<Vec<serde_json::Value>, _> = schema_response.take(i);
            if let Err(e) = result {
                tracing::warn!("Schema statement {i} ({name}) error: {e}");
            }
        }

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
                info!("Loaded saved credentials");
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

    // ── Custom Emoji ──

    pub async fn save_custom_emoji(&self, emoji: &HashMap<String, String>) -> Result<(), String> {
        let json =
            serde_json::to_string(emoji).map_err(|e| format!("JSON serialize error: {e}"))?;
        let cache = JsonCache { data: json };
        let _: Result<Option<JsonCache>, _> = self.db.delete(("cache", "custom_emoji")).await;
        let _: Option<JsonCache> = self
            .db
            .create(("cache", "custom_emoji"))
            .content(cache)
            .await
            .map_err(|e| format!("DB save custom emoji error: {e}"))?;
        info!("Saved {} custom emoji to database", emoji.len());
        Ok(())
    }

    pub async fn load_custom_emoji(&self) -> Option<HashMap<String, String>> {
        match self
            .db
            .select::<Option<JsonCache>>(("cache", "custom_emoji"))
            .await
        {
            Ok(Some(cache)) => match serde_json::from_str(&cache.data) {
                Ok(emoji) => {
                    let emoji: HashMap<String, String> = emoji;
                    info!("Loaded {} cached custom emoji from database", emoji.len());
                    Some(emoji)
                }
                Err(e) => {
                    error!("Failed to deserialize cached custom emoji: {e}");
                    None
                }
            },
            Ok(None) => {
                info!("No cached custom emoji found");
                None
            }
            Err(e) => {
                error!("DB load custom emoji error: {e}");
                None
            }
        }
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

    /// Save messages for a channel (replaces existing cache and updates FTS index).
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

        // Index each message for full-text search and update channel metadata
        self.index_messages(channel_id, messages).await;
        self.update_channel_meta(channel_id, messages).await;

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

    // ── Channel metadata ──

    /// Update channel metadata (oldest/newest message timestamps).
    /// Only expands the range — oldest gets smaller, newest gets larger.
    pub async fn update_channel_meta(&self, channel_id: &str, messages: &[Message]) {
        if messages.is_empty() {
            return;
        }
        let batch_oldest = messages.iter().map(|m| m.ts.as_str()).min().unwrap();
        let batch_newest = messages.iter().map(|m| m.ts.as_str()).max().unwrap();

        if let Err(e) = self
            .db
            .query(
                "INSERT INTO channel_meta (channel, oldest_ts, newest_ts) VALUES ($channel, $oldest, $newest)
                 ON DUPLICATE KEY UPDATE
                     oldest_ts = IF $oldest < oldest_ts THEN $oldest ELSE oldest_ts END,
                     newest_ts = IF $newest > newest_ts THEN $newest ELSE newest_ts END",
            )
            .bind(("channel", channel_id.to_string()))
            .bind(("oldest", batch_oldest.to_string()))
            .bind(("newest", batch_newest.to_string()))
            .await
        {
            tracing::warn!("Failed to update channel meta for {channel_id}: {e}");
        }
    }

    /// Remove all stored data for a channel (messages, cache, activity, metadata).
    pub async fn delete_channel_data(&self, channel_id: &str) {
        // Delete indexed messages
        if let Err(e) = self.db
            .query("DELETE FROM message WHERE channel = $channel")
            .bind(("channel", channel_id.to_string()))
            .await
        {
            tracing::warn!("Failed to delete messages for {channel_id}: {e}");
        }

        // Delete cached message list and activity
        let cache_key = format!("messages_{channel_id}");
        let activity_key = format!("activity_{channel_id}");
        let _: Result<Option<serde_json::Value>, _> = self.db.delete(("cache", &*cache_key)).await;
        let _: Result<Option<serde_json::Value>, _> = self.db.delete(("cache", &*activity_key)).await;

        // Delete channel metadata
        if let Err(e) = self.db
            .query("DELETE FROM channel_meta WHERE channel = $channel")
            .bind(("channel", channel_id.to_string()))
            .await
        {
            tracing::warn!("Failed to delete channel meta for {channel_id}: {e}");
        }

        tracing::info!("Deleted all data for channel {channel_id}");
    }

    /// Mark a channel's backfill as fully checked (reached end of history).
    pub async fn mark_backfill_checked(&self, channel_id: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = self
            .db
            .query(
                "UPDATE channel_meta SET backfill_checked_at = $checked WHERE channel = $channel",
            )
            .bind(("channel", channel_id.to_string()))
            .bind(("checked", now))
            .await
        {
            tracing::warn!("Failed to mark backfill checked for {channel_id}: {e}");
        }
    }

    /// Load all channel metadata. Returns map of channel_id -> (oldest_ts, newest_ts, backfill_checked_at).
    pub async fn load_all_channel_meta(&self) -> HashMap<String, (String, String, Option<String>)> {
        let result: Result<Vec<serde_json::Value>, _> = self.db.select("channel_meta").await;
        let mut map = HashMap::new();
        if let Ok(records) = result {
            for record in records {
                let channel = record.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                let oldest = record.get("oldest_ts").and_then(|v| v.as_str()).unwrap_or("");
                let newest = record.get("newest_ts").and_then(|v| v.as_str()).unwrap_or("");
                let checked = record.get("backfill_checked_at").and_then(|v| v.as_str()).map(String::from);
                if !channel.is_empty() {
                    map.insert(channel.to_string(), (oldest.to_string(), newest.to_string(), checked));
                }
            }
        }
        map
    }

    /// Index messages into the full-text search table.
    pub async fn index_messages(&self, channel_id: &str, messages: &[Message]) {
        let mut indexed = 0u32;
        let mut errors = 0u32;
        for msg in messages {
            let data = serde_json::to_string(msg).unwrap_or_default();
            let res = self
                .db
                .query(
                    "INSERT INTO message (channel, ts, thread_ts, user, text, data) VALUES ($channel, $ts, $thread_ts, $user, $text, $data)
                     ON DUPLICATE KEY UPDATE text = $text, data = $data",
                )
                .bind(("channel", channel_id.to_string()))
                .bind(("ts", msg.ts.clone()))
                .bind(("thread_ts", msg.thread_ts.clone()))
                .bind(("user", msg.user.clone()))
                .bind(("text", msg.text.clone()))
                .bind(("data", data))
                .await;
            match res {
                Ok(mut response) => {
                    // Check the actual statement result — the outer Ok just means the query was sent
                    let inner: Result<Vec<serde_json::Value>, _> = response.take(0);
                    match inner {
                        Ok(rows) if !rows.is_empty() => indexed += 1,
                        Ok(_) => {
                            if errors == 0 {
                                tracing::warn!(
                                    "FTS index returned empty for message {} in {channel_id}",
                                    msg.ts
                                );
                            }
                            errors += 1;
                        }
                        Err(e) => {
                            if errors == 0 {
                                tracing::warn!(
                                    "FTS index statement error for message {} in {channel_id}: {e}",
                                    msg.ts
                                );
                            }
                            errors += 1;
                        }
                    }
                }
                Err(e) => {
                    if errors == 0 {
                        tracing::warn!("FTS index query error for message {} in {channel_id}: {e}", msg.ts);
                    }
                    errors += 1;
                }
            }
        }
        if indexed > 0 || errors > 0 {
            tracing::info!("FTS indexing {channel_id}: {indexed} ok, {errors} errors (of {} total)", messages.len());
        }
    }

    /// Look up a single indexed message by channel and timestamp.
    pub async fn get_indexed_message(&self, channel_id: &str, ts: &str) -> Option<Message> {
        let result = self
            .db
            .query("SELECT data FROM message WHERE channel = $channel AND ts = $ts LIMIT 1")
            .bind(("channel", channel_id.to_string()))
            .bind(("ts", ts.to_string()))
            .await;

        match result {
            Ok(mut response) => {
                let rows: Result<Vec<serde_json::Value>, _> = response.take(0);
                rows.ok()?
                    .into_iter()
                    .next()
                    .and_then(|row| {
                        let data = row.get("data")?.as_str()?;
                        serde_json::from_str(data).ok()
                    })
            }
            Err(_) => None,
        }
    }

    /// Get the oldest indexed message timestamp for a channel.
    pub async fn oldest_indexed_ts(&self, channel_id: &str) -> Option<String> {
        let result: Result<Vec<serde_json::Value>, _> = self
            .db
            .query("SELECT ts FROM message WHERE channel = $channel ORDER BY ts ASC LIMIT 1")
            .bind(("channel", channel_id.to_string()))
            .await
            .map(|mut r| r.take(0).unwrap_or_default());

        match result {
            Ok(rows) => rows
                .first()
                .and_then(|row| row.get("ts")?.as_str().map(|s| s.to_string())),
            Err(e) => {
                tracing::warn!("Failed to get oldest indexed ts for {channel_id}: {e}");
                None
            }
        }
    }

    /// Full-text search across all indexed messages.
    /// Returns matching messages scored by relevance and recency.
    pub async fn search_messages(&self, query: &str) -> Vec<(String, Message)> {
        let now = chrono::Utc::now().timestamp() as f64;

        let result = self
            .db
            .query(
                "SELECT channel, ts, data, search::score(0) AS score
                 FROM message
                 WHERE text @0@ $query
                 ORDER BY score DESC
                 LIMIT 200",
            )
            .bind(("query", query.to_string()))
            .await;

        match result {
            Ok(mut response) => {
                let rows: Result<Vec<serde_json::Value>, _> = response.take(0);
                match rows {
                    Ok(rows) => {
                        tracing::debug!("FTS returned {} rows for {query:?}", rows.len());
                        let mut scored: Vec<(f64, String, Message)> = rows
                            .into_iter()
                            .filter_map(|row| {
                                let channel = row.get("channel")?.as_str()?.to_string();
                                let ts_str = row.get("ts")?.as_str()?;
                                let data = row.get("data")?.as_str()?;
                                let msg: Message = serde_json::from_str(data).ok()?;
                                let relevance = row.get("score")?.as_f64().unwrap_or(0.0);

                                // Recency: days since message, decayed exponentially
                                // Half-life of ~7 days: a week-old message gets ~0.5 boost
                                let msg_ts: f64 = ts_str.split('.').next()?.parse().ok()?;
                                let age_days = (now - msg_ts) / 86400.0;
                                let recency = (-age_days / 10.0).exp(); // 0..1

                                // Combined: relevance dominant, recency as tiebreaker
                                let combined = relevance + 0.3 * recency;

                                Some((combined, channel, msg))
                            })
                            .collect();

                        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                        scored.truncate(50);
                        scored.into_iter().map(|(_, ch, msg)| (ch, msg)).collect()
                    }
                    Err(e) => {
                        error!("FTS search statement error for {query:?}: {e}");
                        Vec::new()
                    }
                }
            }
            Err(e) => {
                error!("FTS search query error for {query:?}: {e}");
                Vec::new()
            }
        }
    }

    // ── Presence watches ──

    /// Save the set of user IDs to watch for presence changes.
    pub async fn save_presence_watches(&self, user_ids: &[String]) {
        let json = serde_json::to_string(user_ids).unwrap_or_default();
        let cache = JsonCache { data: json };
        let _: Result<Option<JsonCache>, _> = self.db.delete(("settings", "presence_watches")).await;
        let _: Result<Option<JsonCache>, _> = self
            .db
            .create(("settings", "presence_watches"))
            .content(cache)
            .await;
    }

    /// Load the set of user IDs being watched for presence changes.
    pub async fn load_presence_watches(&self) -> Vec<String> {
        match self
            .db
            .select::<Option<JsonCache>>(("settings", "presence_watches"))
            .await
        {
            Ok(Some(cache)) => serde_json::from_str(&cache.data).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Add a user ID to the presence watch list.
    pub async fn add_presence_watch(&self, user_id: &str) {
        let mut watches = self.load_presence_watches().await;
        if !watches.iter().any(|u| u == user_id) {
            watches.push(user_id.to_string());
            self.save_presence_watches(&watches).await;
        }
    }

    /// Remove a user ID from the presence watch list.
    pub async fn remove_presence_watch(&self, user_id: &str) {
        let mut watches = self.load_presence_watches().await;
        let before = watches.len();
        watches.retain(|u| u != user_id);
        if watches.len() != before {
            self.save_presence_watches(&watches).await;
        }
    }

    // ── Recent emoji ──

    /// Push an emoji shortcode to the front of the recent list (deduplicating, max 50).
    pub async fn push_recent_emoji(&self, shortcode: &str) {
        if shortcode.is_empty() {
            return;
        }
        let mut list = self.load_recent_emoji().await;
        list.retain(|s| s != shortcode);
        list.insert(0, shortcode.to_string());
        list.truncate(50);
        let json = serde_json::to_string(&list).unwrap_or_default();
        let cache = JsonCache { data: json };
        let _: Result<Option<JsonCache>, _> = self.db.delete(("cache", "recent_emoji")).await;
        let _: Result<Option<JsonCache>, _> = self
            .db
            .create(("cache", "recent_emoji"))
            .content(cache)
            .await;
    }

    pub async fn load_recent_emoji(&self) -> Vec<String> {
        match self
            .db
            .select::<Option<JsonCache>>(("cache", "recent_emoji"))
            .await
        {
            Ok(Some(cache)) => serde_json::from_str(&cache.data).unwrap_or_default(),
            _ => Vec::new(),
        }
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

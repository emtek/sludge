use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use slacko::types::{Channel, Message, User};
use tracing::{error, info};

/// Stored login credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCredentials {
    pub xoxc_token: Option<String>,
    pub xoxd_cookie: Option<String>,
    pub workspace_url: Option<String>,
}

/// A recently used status (emoji + text).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentStatus {
    pub emoji: String,
    pub text: String,
}

/// User-configurable preferences, persisted to the `kv` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Preferences {
    /// How many months of history to backfill per channel.
    pub history_months: u32,
    /// How many weeks counts as "recent" activity for sidebar filtering.
    pub activity_weeks: u32,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            history_months: 1,
            activity_weeks: 2,
        }
    }
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub async fn open(_rt: &tokio::runtime::Handle) -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("sludge");

        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create data dir: {e}"))?;

        let db_path = data_dir.join("sludge.db");
        info!("Opening database at {}", db_path.display());

        let conn = Connection::open(&db_path)
            .map_err(|e| format!("DB open error: {e}"))?;

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("WAL mode error: {e}"))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| format!("synchronous pragma error: {e}"))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| format!("foreign_keys pragma error: {e}"))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS message (
                channel TEXT NOT NULL,
                ts      TEXT NOT NULL,
                thread_ts TEXT,
                user    TEXT,
                text    TEXT NOT NULL DEFAULT '',
                data    TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (channel, ts)
            );

            CREATE INDEX IF NOT EXISTS idx_message_channel ON message(channel);

            CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
                text,
                content=message,
                content_rowid=rowid,
                tokenize='porter unicode61'
            );

            -- Triggers to keep FTS index in sync with message table
            CREATE TRIGGER IF NOT EXISTS message_fts_insert AFTER INSERT ON message BEGIN
                INSERT INTO message_fts(rowid, text) VALUES (new.rowid, new.text);
            END;
            CREATE TRIGGER IF NOT EXISTS message_fts_delete AFTER DELETE ON message BEGIN
                INSERT INTO message_fts(message_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
            END;
            CREATE TRIGGER IF NOT EXISTS message_fts_update AFTER UPDATE ON message BEGIN
                INSERT INTO message_fts(message_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
                INSERT INTO message_fts(rowid, text) VALUES (new.rowid, new.text);
            END;

            CREATE TABLE IF NOT EXISTS kv (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS channel_meta (
                channel             TEXT PRIMARY KEY,
                oldest_ts           TEXT NOT NULL,
                newest_ts           TEXT NOT NULL,
                backfill_checked_at TEXT
            );

            CREATE TABLE IF NOT EXISTS channel_activity (
                channel TEXT PRIMARY KEY,
                ts      TEXT NOT NULL
            );"
        ).map_err(|e| format!("DB schema error: {e}"))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── helpers ──

    fn with_conn<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&Connection) -> T,
    {
        let conn = self.conn.lock().unwrap();
        f(&conn)
    }

    fn kv_set(&self, key: &str, value: &str) {
        self.with_conn(|c| {
            let _ = c.execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                params![key, value],
            );
        });
    }

    fn kv_get(&self, key: &str) -> Option<String> {
        self.with_conn(|c| {
            c.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .ok()
        })
    }

    fn kv_delete(&self, key: &str) {
        self.with_conn(|c| {
            let _ = c.execute("DELETE FROM kv WHERE key = ?1", params![key]);
        });
    }

    // ── Credentials ──

    pub async fn save_credentials(&self, creds: &SavedCredentials) -> Result<(), String> {
        let json =
            serde_json::to_string(creds).map_err(|e| format!("JSON serialize error: {e}"))?;
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.kv_set("credentials", &json))
            .await
            .map_err(|e| format!("spawn_blocking error: {e}"))?;
        info!("Credentials saved to database");
        Ok(())
    }

    pub async fn load_credentials(&self) -> Option<SavedCredentials> {
        let db = self.clone();
        let json = tokio::task::spawn_blocking(move || db.kv_get("credentials"))
            .await
            .ok()??;
        match serde_json::from_str(&json) {
            Ok(creds) => {
                info!("Loaded saved credentials");
                Some(creds)
            }
            Err(e) => {
                error!("Failed to deserialize credentials: {e}");
                None
            }
        }
    }

    pub async fn clear_credentials(&self) {
        let db = self.clone();
        let _ = tokio::task::spawn_blocking(move || db.kv_delete("credentials")).await;
        info!("Credentials cleared from database");
    }

    /// Clear all cached content — messages, channel/user cache, channel metadata.
    /// Leaves credentials and app settings intact.
    pub async fn clear_cache(&self) {
        let db = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let _ = c.execute_batch(
                    "DELETE FROM message;
                     DELETE FROM channel_meta;
                     DELETE FROM channel_activity;
                     DELETE FROM kv WHERE key LIKE 'cache:%';",
                );
            });
        })
        .await;
        info!("Cleared cached messages, channels, users, and channel metadata");
    }

    // ── Custom Emoji ──

    pub async fn save_custom_emoji(
        &self,
        emoji: &HashMap<String, String>,
    ) -> Result<(), String> {
        let json =
            serde_json::to_string(emoji).map_err(|e| format!("JSON serialize error: {e}"))?;
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.kv_set("cache:custom_emoji", &json))
            .await
            .map_err(|e| format!("spawn_blocking error: {e}"))?;
        info!("Saved {} custom emoji to database", emoji.len());
        Ok(())
    }

    pub async fn load_custom_emoji(&self) -> Option<HashMap<String, String>> {
        let db = self.clone();
        let json =
            tokio::task::spawn_blocking(move || db.kv_get("cache:custom_emoji"))
                .await
                .ok()??;
        match serde_json::from_str(&json) {
            Ok(emoji) => {
                let emoji: HashMap<String, String> = emoji;
                info!("Loaded {} cached custom emoji from database", emoji.len());
                Some(emoji)
            }
            Err(e) => {
                error!("Failed to deserialize cached custom emoji: {e}");
                None
            }
        }
    }

    // ── Channels ──

    pub async fn save_channels(&self, channels: &[Channel]) -> Result<(), String> {
        let json = serde_json::to_string(channels)
            .map_err(|e| format!("JSON serialize error: {e}"))?;
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.kv_set("cache:channels", &json))
            .await
            .map_err(|e| format!("spawn_blocking error: {e}"))?;
        info!("Saved {} channels to database", channels.len());
        Ok(())
    }

    pub async fn load_channels(&self) -> Option<Vec<Channel>> {
        let db = self.clone();
        let json =
            tokio::task::spawn_blocking(move || db.kv_get("cache:channels"))
                .await
                .ok()??;
        match serde_json::from_str(&json) {
            Ok(channels) => {
                let channels: Vec<Channel> = channels;
                info!("Loaded {} cached channels from database", channels.len());
                Some(channels)
            }
            Err(e) => {
                error!("Failed to deserialize cached channels: {e}");
                None
            }
        }
    }

    // ── Last channel ──

    pub async fn save_last_channel(&self, channel_id: &str) {
        let db = self.clone();
        let cid = channel_id.to_string();
        let _ = tokio::task::spawn_blocking(move || db.kv_set("cache:last_channel", &cid)).await;
    }

    pub async fn load_last_channel(&self) -> Option<String> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.kv_get("cache:last_channel"))
            .await
            .ok()?
    }

    // ── Recent statuses ──

    pub async fn push_recent_status(&self, status: &RecentStatus) {
        if status.emoji.is_empty() && status.text.is_empty() {
            return;
        }
        let mut list = self.load_recent_statuses().await;
        list.retain(|s| s != status);
        list.insert(0, status.clone());
        list.truncate(8);
        let json = serde_json::to_string(&list).unwrap_or_default();
        let db = self.clone();
        let _ =
            tokio::task::spawn_blocking(move || db.kv_set("cache:recent_statuses", &json)).await;
    }

    pub async fn load_recent_statuses(&self) -> Vec<RecentStatus> {
        let db = self.clone();
        let json = tokio::task::spawn_blocking(move || db.kv_get("cache:recent_statuses"))
            .await
            .ok()
            .flatten();
        json.and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    // ── Users ──

    pub async fn save_users(&self, users: &[User]) -> Result<(), String> {
        let json =
            serde_json::to_string(users).map_err(|e| format!("JSON serialize error: {e}"))?;
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.kv_set("cache:users", &json))
            .await
            .map_err(|e| format!("spawn_blocking error: {e}"))?;
        info!("Saved {} users to database", users.len());
        Ok(())
    }

    pub async fn load_users(&self) -> Option<Vec<User>> {
        let db = self.clone();
        let json = tokio::task::spawn_blocking(move || db.kv_get("cache:users"))
            .await
            .ok()??;
        match serde_json::from_str(&json) {
            Ok(users) => {
                let users: Vec<User> = users;
                info!("Loaded {} cached users from database", users.len());
                Some(users)
            }
            Err(e) => {
                error!("Failed to deserialize cached users: {e}");
                None
            }
        }
    }

    // ── Channel activity ──

    pub async fn update_channel_activity(&self, channel_id: &str, ts: &str) {
        let db = self.clone();
        let cid = channel_id.to_string();
        let ts = ts.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let _ = c.execute(
                    "INSERT OR REPLACE INTO channel_activity (channel, ts) VALUES (?1, ?2)",
                    params![cid, ts],
                );
            });
        })
        .await;
    }

    pub async fn load_all_channel_activity(&self) -> HashMap<String, String> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut stmt = match c.prepare("SELECT channel, ts FROM channel_activity") {
                    Ok(s) => s,
                    Err(_) => return HashMap::new(),
                };
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .unwrap_or_else(|_| unreachable!());
                rows.filter_map(|r| r.ok()).collect()
            })
        })
        .await
        .unwrap_or_default()
    }

    // ── Messages ──

    pub async fn save_messages(
        &self,
        channel_id: &str,
        messages: &[Message],
    ) -> Result<(), String> {
        self.index_messages(channel_id, messages).await;
        self.update_channel_meta(channel_id, messages).await;
        tracing::debug!(
            "Indexed {} messages for channel {channel_id}",
            messages.len()
        );
        Ok(())
    }

    pub async fn load_messages(&self, channel_id: &str) -> Option<Vec<Message>> {
        let db = self.clone();
        let cid = channel_id.to_string();
        let result = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT data FROM message
                     WHERE channel = ?1 AND (thread_ts IS NULL OR thread_ts = ts)
                     ORDER BY ts DESC LIMIT 10",
                ).ok()?;
                let rows: Vec<Message> = stmt
                    .query_map(params![cid], |row| row.get::<_, String>(0))
                    .ok()?
                    .filter_map(|r| r.ok())
                    .filter_map(|data| serde_json::from_str(&data).ok())
                    .collect();
                if rows.is_empty() { None } else { Some(rows) }
            })
        })
        .await
        .ok()?;
        result
    }

    pub async fn append_message(&self, channel_id: &str, message: &Message) {
        self.index_messages(channel_id, &[message.clone()]).await;
    }

    pub async fn reply_counts_for_channel(
        &self,
        channel_id: &str,
    ) -> HashMap<String, usize> {
        let db = self.clone();
        let cid = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut counts = HashMap::new();
                let mut stmt = match c.prepare(
                    "SELECT thread_ts FROM message
                     WHERE channel = ?1 AND thread_ts IS NOT NULL AND thread_ts != ts",
                ) {
                    Ok(s) => s,
                    Err(_) => return counts,
                };
                let rows = stmt
                    .query_map(params![cid], |row| row.get::<_, String>(0))
                    .unwrap_or_else(|_| unreachable!());
                for r in rows.flatten() {
                    *counts.entry(r).or_insert(0) += 1;
                }
                counts
            })
        })
        .await
        .unwrap_or_default()
    }

    pub async fn load_messages_after(
        &self,
        channel_id: &str,
        after_ts: &str,
        limit: u32,
    ) -> Vec<Message> {
        let db = self.clone();
        let cid = channel_id.to_string();
        let ats = after_ts.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut stmt = match c.prepare(
                    "SELECT data FROM message
                     WHERE channel = ?1 AND ts > ?2 AND (thread_ts IS NULL OR thread_ts = ts)
                     ORDER BY ts ASC LIMIT ?3",
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("DB load_messages_after error: {e}");
                        return Vec::new();
                    }
                };
                stmt.query_map(params![cid, ats, limit], |row| row.get::<_, String>(0))
                    .unwrap_or_else(|_| unreachable!())
                    .filter_map(|r| r.ok())
                    .filter_map(|data| serde_json::from_str(&data).ok())
                    .collect()
            })
        })
        .await
        .unwrap_or_default()
    }

    pub async fn delete_indexed_message(&self, channel_id: &str, ts: &str) {
        let db = self.clone();
        let cid = channel_id.to_string();
        let ts = ts.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                if let Err(e) = c.execute(
                    "DELETE FROM message WHERE channel = ?1 AND ts = ?2",
                    params![cid, ts],
                ) {
                    tracing::warn!("Failed to delete indexed message {ts} in {cid}: {e}");
                }
            });
        })
        .await;
    }

    pub async fn update_indexed_message_text(
        &self,
        channel_id: &str,
        ts: &str,
        new_text: &str,
    ) {
        if let Some(mut msg) = self.get_indexed_message(channel_id, ts).await {
            msg.text = new_text.to_string();
            let data = serde_json::to_string(&msg).unwrap_or_default();
            let db = self.clone();
            let cid = channel_id.to_string();
            let ts = ts.to_string();
            let text = new_text.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                db.with_conn(|c| {
                    if let Err(e) = c.execute(
                        "UPDATE message SET text = ?1, data = ?2 WHERE channel = ?3 AND ts = ?4",
                        params![text, data, cid, ts],
                    ) {
                        tracing::warn!("Failed to update indexed message {ts} in {cid}: {e}");
                    }
                });
            })
            .await;
        }
    }

    pub async fn update_reaction(
        &self,
        channel_id: &str,
        message_ts: &str,
        reaction_name: &str,
        user_id: &str,
        added: bool,
    ) -> Option<Vec<slacko::types::Reaction>> {
        let mut msg = self.get_indexed_message(channel_id, message_ts).await?;

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

        reactions.retain(|r| r.count > 0);

        let updated = msg.reactions.clone();
        self.index_messages(channel_id, &[msg]).await;
        updated
    }

    // ── Channel metadata ──

    pub async fn update_channel_meta(&self, channel_id: &str, messages: &[Message]) {
        if messages.is_empty() {
            return;
        }
        let batch_oldest = messages.iter().map(|m| m.ts.as_str()).min().unwrap();
        let batch_newest = messages.iter().map(|m| m.ts.as_str()).max().unwrap();

        let db = self.clone();
        let cid = channel_id.to_string();
        let oldest = batch_oldest.to_string();
        let newest = batch_newest.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                if let Err(e) = c.execute(
                    "INSERT INTO channel_meta (channel, oldest_ts, newest_ts)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(channel) DO UPDATE SET
                         oldest_ts = CASE WHEN excluded.oldest_ts < oldest_ts THEN excluded.oldest_ts ELSE oldest_ts END,
                         newest_ts = CASE WHEN excluded.newest_ts > newest_ts THEN excluded.newest_ts ELSE newest_ts END",
                    params![cid, oldest, newest],
                ) {
                    tracing::warn!("Failed to update channel meta for {cid}: {e}");
                }
            });
        })
        .await;
    }

    pub async fn delete_channel_data(&self, channel_id: &str) {
        let db = self.clone();
        let cid = channel_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                if let Err(e) = c.execute(
                    "DELETE FROM message WHERE channel = ?1",
                    params![cid],
                ) {
                    tracing::warn!("Failed to delete messages for {cid}: {e}");
                }
                let _ = c.execute(
                    "DELETE FROM channel_activity WHERE channel = ?1",
                    params![cid],
                );
                let _ = c.execute(
                    "DELETE FROM channel_meta WHERE channel = ?1",
                    params![cid],
                );
            });
            tracing::info!("Deleted all data for channel {cid}");
        })
        .await;
    }

    pub async fn mark_backfill_checked(&self, channel_id: &str) {
        let db = self.clone();
        let cid = channel_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                if let Err(e) = c.execute(
                    "UPDATE channel_meta SET backfill_checked_at = ?1 WHERE channel = ?2",
                    params![now, cid],
                ) {
                    tracing::warn!("Failed to mark backfill checked for {cid}: {e}");
                }
            });
        })
        .await;
    }

    pub async fn get_newest_ts(&self, channel_id: &str) -> Option<String> {
        let db = self.clone();
        let cid = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                c.query_row(
                    "SELECT newest_ts FROM channel_meta WHERE channel = ?1",
                    params![cid],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .filter(|s| !s.is_empty())
            })
        })
        .await
        .ok()?
    }

    pub async fn load_messages_around(
        &self,
        channel_id: &str,
        ts: &str,
        count: u32,
    ) -> Vec<Message> {
        let db = self.clone();
        let cid = channel_id.to_string();
        let ts = ts.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut messages = Vec::new();
                let mut seen = std::collections::HashSet::new();

                // Before (inclusive)
                if let Ok(mut stmt) = c.prepare(
                    "SELECT data FROM message
                     WHERE channel = ?1 AND ts <= ?2 AND (thread_ts IS NULL OR thread_ts = ts)
                     ORDER BY ts DESC LIMIT ?3",
                ) {
                    if let Ok(rows) = stmt.query_map(params![cid, ts, count], |row| {
                        row.get::<_, String>(0)
                    }) {
                        for data in rows.flatten() {
                            if let Ok(msg) = serde_json::from_str::<Message>(&data) {
                                if seen.insert(msg.ts.clone()) {
                                    messages.push(msg);
                                }
                            }
                        }
                    }
                }

                // After
                if let Ok(mut stmt) = c.prepare(
                    "SELECT data FROM message
                     WHERE channel = ?1 AND ts > ?2 AND (thread_ts IS NULL OR thread_ts = ts)
                     ORDER BY ts ASC LIMIT ?3",
                ) {
                    if let Ok(rows) = stmt.query_map(params![cid, ts, count], |row| {
                        row.get::<_, String>(0)
                    }) {
                        for data in rows.flatten() {
                            if let Ok(msg) = serde_json::from_str::<Message>(&data) {
                                if seen.insert(msg.ts.clone()) {
                                    messages.push(msg);
                                }
                            }
                        }
                    }
                }

                messages.sort_by(|a, b| b.ts.cmp(&a.ts));
                messages
            })
        })
        .await
        .unwrap_or_default()
    }

    pub async fn load_all_channel_meta(
        &self,
    ) -> HashMap<String, (String, String, Option<String>)> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut map = HashMap::new();
                let mut stmt = match c
                    .prepare("SELECT channel, oldest_ts, newest_ts, backfill_checked_at FROM channel_meta")
                {
                    Ok(s) => s,
                    Err(_) => return map,
                };
                let rows = stmt
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    })
                    .unwrap_or_else(|_| unreachable!());
                for r in rows.flatten() {
                    map.insert(r.0, (r.1, r.2, r.3));
                }
                map
            })
        })
        .await
        .unwrap_or_default()
    }

    pub async fn index_messages(&self, channel_id: &str, messages: &[Message]) {
        if messages.is_empty() {
            return;
        }
        let db = self.clone();
        let cid = channel_id.to_string();
        let msgs: Vec<Message> = messages.to_vec();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut indexed = 0u32;
                let mut errors = 0u32;
                for msg in &msgs {
                    let data = serde_json::to_string(msg).unwrap_or_default();
                    match c.execute(
                        "INSERT OR REPLACE INTO message (channel, ts, thread_ts, user, text, data)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![cid, msg.ts, msg.thread_ts, msg.user, msg.text, data],
                    ) {
                        Ok(_) => indexed += 1,
                        Err(e) => {
                            if errors == 0 {
                                tracing::warn!(
                                    "FTS index error for message {} in {cid}: {e}",
                                    msg.ts
                                );
                            }
                            errors += 1;
                        }
                    }
                }
                if indexed > 0 || errors > 0 {
                    tracing::info!(
                        "FTS indexing {cid}: {indexed} ok, {errors} errors (of {} total)",
                        msgs.len()
                    );
                }
            });
        })
        .await;
    }

    pub async fn get_indexed_message(&self, channel_id: &str, ts: &str) -> Option<Message> {
        let db = self.clone();
        let cid = channel_id.to_string();
        let ts = ts.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                c.query_row(
                    "SELECT data FROM message WHERE channel = ?1 AND ts = ?2 LIMIT 1",
                    params![cid, ts],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .and_then(|data| serde_json::from_str(&data).ok())
            })
        })
        .await
        .ok()?
    }

    pub async fn oldest_indexed_ts(&self, channel_id: &str) -> Option<String> {
        let db = self.clone();
        let cid = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                c.query_row(
                    "SELECT ts FROM message WHERE channel = ?1 ORDER BY ts ASC LIMIT 1",
                    params![cid],
                    |row| row.get::<_, String>(0),
                )
                .ok()
            })
        })
        .await
        .ok()?
    }

    /// Full-text search across all indexed messages.
    pub async fn search_messages(&self, query: &str) -> Vec<(String, Message)> {
        let now = chrono::Utc::now().timestamp() as f64;
        let db = self.clone();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                // FTS5 MATCH query; bm25() gives relevance (lower = better match)
                let mut stmt = match c.prepare(
                    "SELECT m.channel, m.ts, m.data, bm25(message_fts) AS score
                     FROM message_fts
                     JOIN message m ON m.rowid = message_fts.rowid
                     WHERE message_fts MATCH ?1
                     ORDER BY score
                     LIMIT 200",
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("FTS search query error for {query:?}: {e}");
                        return Vec::new();
                    }
                };
                let rows = match stmt.query_map(params![query], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                }) {
                    Ok(r) => r,
                    Err(e) => {
                        error!("FTS search error for {query:?}: {e}");
                        return Vec::new();
                    }
                };

                let mut scored: Vec<(f64, String, Message)> = rows
                    .filter_map(|r| r.ok())
                    .filter_map(|(channel, ts_str, data, bm25)| {
                        let msg: Message = serde_json::from_str(&data).ok()?;
                        // bm25() returns negative values; negate for positive relevance
                        let relevance = -bm25;
                        let msg_ts: f64 = ts_str.split('.').next()?.parse().ok()?;
                        let age_days = (now - msg_ts) / 86400.0;
                        let recency = (-age_days / 10.0).exp();
                        let combined = relevance + 0.3 * recency;
                        Some((combined, channel, msg))
                    })
                    .collect();

                scored.sort_by(|a, b| {
                    b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
                });
                scored.truncate(50);
                scored.into_iter().map(|(_, ch, msg)| (ch, msg)).collect()
            })
        })
        .await
        .unwrap_or_default()
    }

    /// Search messages in a specific channel, returning highlighted text.
    pub async fn search_channel_messages(
        &self,
        channel_id: &str,
        query: &str,
    ) -> Vec<(Message, String)> {
        let db = self.clone();
        let cid = channel_id.to_string();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            db.with_conn(|c| {
                let mut stmt = match c.prepare(
                    "SELECT m.ts, m.data, highlight(message_fts, 0, '<b>', '</b>') AS highlighted
                     FROM message_fts
                     JOIN message m ON m.rowid = message_fts.rowid
                     WHERE message_fts MATCH ?1 AND m.channel = ?2
                     ORDER BY m.ts ASC
                     LIMIT 50",
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("Channel FTS query error for {query:?}: {e}");
                        return Vec::new();
                    }
                };
                let rows = match stmt.query_map(params![query, cid], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
                }) {
                    Ok(r) => r,
                    Err(e) => {
                        error!("Channel FTS error for {query:?}: {e}");
                        return Vec::new();
                    }
                };
                rows.filter_map(|r| r.ok())
                    .filter_map(|(data, highlighted)| {
                        let msg: Message = serde_json::from_str(&data).ok()?;
                        Some((msg, highlighted))
                    })
                    .collect()
            })
        })
        .await
        .unwrap_or_default()
    }

    // ── Presence watches ──

    pub async fn save_presence_watches(&self, user_ids: &[String]) {
        let json = serde_json::to_string(user_ids).unwrap_or_default();
        let db = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            db.kv_set("settings:presence_watches", &json);
        })
        .await;
    }

    pub async fn load_presence_watches(&self) -> Vec<String> {
        let db = self.clone();
        let json = tokio::task::spawn_blocking(move || {
            db.kv_get("settings:presence_watches")
        })
        .await
        .ok()
        .flatten();
        json.and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    pub async fn add_presence_watch(&self, user_id: &str) {
        let mut watches = self.load_presence_watches().await;
        if !watches.iter().any(|u| u == user_id) {
            watches.push(user_id.to_string());
            self.save_presence_watches(&watches).await;
        }
    }

    pub async fn remove_presence_watch(&self, user_id: &str) {
        let mut watches = self.load_presence_watches().await;
        let before = watches.len();
        watches.retain(|u| u != user_id);
        if watches.len() != before {
            self.save_presence_watches(&watches).await;
        }
    }

    // ── Preferences ──

    pub async fn save_preferences(&self, prefs: &Preferences) {
        let json = serde_json::to_string(prefs).unwrap_or_default();
        let db = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            db.kv_set("settings:preferences", &json);
        })
        .await;
    }

    pub async fn load_preferences(&self) -> Preferences {
        let db = self.clone();
        let json = tokio::task::spawn_blocking(move || db.kv_get("settings:preferences"))
            .await
            .ok()
            .flatten();
        json.and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    // ── Recent emoji ──

    pub async fn push_recent_emoji(&self, shortcode: &str) {
        if shortcode.is_empty() {
            return;
        }
        let mut list = self.load_recent_emoji().await;
        list.retain(|s| s != shortcode);
        list.insert(0, shortcode.to_string());
        list.truncate(50);
        let json = serde_json::to_string(&list).unwrap_or_default();
        let db = self.clone();
        let _ =
            tokio::task::spawn_blocking(move || db.kv_set("cache:recent_emoji", &json)).await;
    }

    pub async fn load_recent_emoji(&self) -> Vec<String> {
        let db = self.clone();
        let json = tokio::task::spawn_blocking(move || db.kv_get("cache:recent_emoji"))
            .await
            .ok()
            .flatten();
        json.and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }
}

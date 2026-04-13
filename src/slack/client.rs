use std::collections::HashMap;
use reqwest::header::{HeaderMap, HeaderValue, COOKIE};
use slacko::types::{Channel, Message, User};
use tracing::{debug, error, info};

/// Generic Slack API response envelope for stealth-mode raw HTTP calls.
#[derive(Debug, serde::Deserialize)]
struct RawResponse<T> {
    ok: bool,
    error: Option<String>,
    #[serde(flatten)]
    data: Option<T>,
}

#[derive(Debug, serde::Deserialize)]
struct RawAuthTest {
    url: Option<String>,
    team: Option<String>,
    user: Option<String>,
    team_id: Option<String>,
    user_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawConversationsList {
    channels: Vec<Channel>,
    response_metadata: Option<slacko::types::ResponseMetadata>,
}

#[derive(Debug, serde::Deserialize)]
struct RawConversationsInfo {
    channel: Channel,
}

#[derive(Debug, serde::Deserialize)]
struct RawConversationHistory {
    messages: Vec<Message>,
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, serde::Deserialize)]
struct RawUsersList {
    members: Vec<User>,
    response_metadata: Option<slacko::types::ResponseMetadata>,
}

#[derive(Debug, serde::Deserialize)]
struct RawPostMessage {
    ts: String,
}

#[derive(Debug, serde::Deserialize)]
struct RawProfileResponse {
    profile: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct RawPresenceResponse {
    presence: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawCallsRequest {
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct RawUsergroupsList {
    usergroups: Vec<RawUsergroup>,
}

#[derive(Debug, serde::Deserialize)]
struct RawUsergroup {
    id: String,
    handle: String,
}

#[derive(Debug, serde::Deserialize)]
struct RawEmojiList {
    emoji: std::collections::HashMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawConversationMembers {
    members: Vec<String>,
    response_metadata: Option<slacko::types::ResponseMetadata>,
}

#[derive(Debug, serde::Deserialize)]
struct RawGetUploadUrl {
    upload_url: String,
    file_id: String,
}

/// Credentials for stealth (cookie) auth, kept around for raw HTTP calls.
#[derive(Clone)]
struct StealthCreds {
    xoxc_token: String,
    xoxd_cookie: String,
    /// Workspace base URL discovered via auth.test (e.g. "https://myteam.slack.com").
    workspace_url: Option<String>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    creds: StealthCreds,
}

/// Public auth test result that both backends can produce.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AuthInfo {
    pub url: String,
    pub team: String,
    pub user: String,
    pub team_id: String,
    pub user_id: String,
}

impl Client {
    pub fn new(xoxc_token: String, xoxd_cookie: String, workspace_url: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            creds: StealthCreds {
                xoxc_token,
                xoxd_cookie,
                workspace_url,
            },
        }
    }

    /// Returns RTM connection parameters.
    pub fn rtm_params(&self) -> (reqwest::Client, String, String, Option<String>) {
        (
            self.http.clone(),
            self.creds.xoxc_token.clone(),
            self.creds.xoxd_cookie.clone(),
            self.creds.workspace_url.clone(),
        )
    }

    /// Fetch image bytes with local disk caching.
    /// Cached files are stored under `~/.local/share/sludge/image_cache/`.
    pub async fn fetch_image_bytes(&self, url: &str) -> Result<bytes::Bytes, String> {
        use std::hash::{Hash, Hasher};

        // Build a deterministic cache path from the URL
        let cache_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("sludge")
            .join("image_cache");

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut hasher);
        let hash = hasher.finish();
        let cache_path = cache_dir.join(format!("{hash:016x}"));

        // Return cached bytes if available
        if let Ok(bytes) = tokio::fs::read(&cache_path).await {
            tracing::debug!("Image cache HIT: {}", cache_path.display());
            return Ok(bytes::Bytes::from(bytes));
        }

        // Fetch from network, then cache
        tracing::debug!("Image cache MISS: {url}");
        let bytes = self.fetch_image_bytes_uncached(url).await?;

        // Best-effort write to cache
        let _ = tokio::fs::create_dir_all(&cache_dir).await;
        let _ = tokio::fs::write(&cache_path, &bytes).await;

        Ok(bytes)
    }

    /// Fetch raw bytes from a URL, adding auth headers for Slack-hosted private URLs.
    async fn fetch_image_bytes_uncached(&self, url: &str) -> Result<bytes::Bytes, String> {
        let is_slack_url = url.contains("slack.com") || url.contains("slack-files.com");

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|e| format!("HTTP client error: {e}"))?;

        let req = if is_slack_url {
            http.get(url)
                .bearer_auth(&self.creds.xoxc_token)
                .headers(Self::stealth_headers(&self.creds))
        } else {
            http.get(url)
        };

        let resp = req.send().await.map_err(|e| format!("Image fetch error: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Image fetch {}: {}", resp.status(), url));
        }
        resp.bytes()
            .await
            .map_err(|e| format!("Image read error: {e}"))
    }

    // ── Stealth HTTP helpers ──

    fn stealth_api_url(creds: &StealthCreds, method: &str) -> String {
        match &creds.workspace_url {
            Some(base) => format!("{base}/api/{method}"),
            None => format!("https://slack.com/api/{method}"),
        }
    }

    fn stealth_headers(creds: &StealthCreds) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(&format!("d={}", creds.xoxd_cookie)) {
            headers.insert(COOKIE, val);
        }
        headers
    }

    async fn stealth_post<T: serde::de::DeserializeOwned>(
        http: &reqwest::Client,
        creds: &StealthCreds,
        method: &str,
        fields: &[(&str, &str)],
    ) -> Result<T, String> {
        let url = Self::stealth_api_url(creds, method);
        debug!("Stealth POST {url}");

        let mut all_fields: Vec<(&str, &str)> = fields.to_vec();
        all_fields.push(("token", &creds.xoxc_token));

        let resp = http
            .post(&url)
            .headers(Self::stealth_headers(creds))
            .form(&all_fields)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            error!("Stealth {method} HTTP {status}: {text}");
            return Err(format!("{method} HTTP {status}: {text}"));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| format!("Read error: {e}"))?;
        debug!("Stealth {method} response: {}", &text[..text.len().min(500)]);

        let body: RawResponse<T> =
            serde_json::from_str(&text).map_err(|e| format!("Parse error (status {status}): {e}"))?;

        if !body.ok {
            let err = body.error.unwrap_or_else(|| "unknown".into());
            error!("Slack API {method} error: {err}");
            return Err(format!("{method} - {err}"));
        }

        body.data.ok_or_else(|| format!("{method}: empty response"))
    }

    // ── Auth ──

    pub async fn auth_test(&mut self) -> Result<AuthInfo, String> {
        info!("Calling auth.test...");
        let r: RawAuthTest =
            Self::stealth_post(&self.http, &self.creds, "auth.test", &[]).await?;
        let url = r.url.unwrap_or_default();
        let team = r.team.unwrap_or_default();
        let user = r.user.unwrap_or_default();
        let team_id = r.team_id.unwrap_or_default();
        let user_id = r.user_id.unwrap_or_default();

        info!("auth.test OK — user: {user}, team: {team}, url: {url}");

        if !url.is_empty() {
            let ws_url = url.trim_end_matches('/').to_string();
            info!("Setting workspace base URL to: {ws_url}");
            self.creds.workspace_url = Some(ws_url);
        }

        Ok(AuthInfo { url, team, user, team_id, user_id })
    }

    // ── Conversations ──

    pub async fn conversations_list_all(&self) -> Result<Vec<Channel>, String> {
        info!("Calling conversations.list (paginated)...");
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        let mut page = 0u32;
        loop {
            page += 1;
            debug!("conversations.list page {page}");
            let mut fields = vec![
                ("types", "public_channel,private_channel,im,mpim"),
                ("exclude_archived", "true"),
                ("limit", "200"),
            ];
            let cursor_val;
            if let Some(c) = &cursor {
                cursor_val = c.clone();
                fields.push(("cursor", &cursor_val));
            }
            let data: RawConversationsList =
                Self::stealth_post(&self.http, &self.creds, "conversations.list", &fields).await?;
            let count = data.channels.len();
            debug!("conversations.list page {page} returned {count} channels");
            all.extend(data.channels);
            let next = data
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty());
            if next.is_none() {
                break;
            }
            cursor = next;
        }
        info!("conversations.list complete: {} channels total", all.len());
        Ok(all)
    }

    pub async fn conversations_info(&self, channel: &str) -> Result<Channel, String> {
        info!("Calling conversations.info for channel={channel}");
        let fields = vec![("channel", channel)];
        let data: RawConversationsInfo =
            Self::stealth_post(&self.http, &self.creds, "conversations.info", &fields).await?;
        Ok(data.channel)
    }

    pub async fn conversation_history(
        &self,
        channel: &str,
        limit: u32,
    ) -> Result<Vec<Message>, String> {
        info!("Calling conversations.history for channel={channel}, limit={limit}");
        let limit_str = limit.to_string();
        let fields = vec![("channel", channel), ("limit", &limit_str)];
        let data: RawConversationHistory =
            Self::stealth_post(&self.http, &self.creds, "conversations.history", &fields).await?;
        info!("conversations.history returned {} messages", data.messages.len());
        Ok(data.messages)
    }

    /// Fetch messages older than `latest` (exclusive) from a channel.
    pub async fn conversation_history_before(
        &self,
        channel: &str,
        latest: &str,
        limit: u32,
    ) -> Result<Vec<Message>, String> {
        info!("Calling conversations.history for channel={channel}, latest={latest}, limit={limit}");
        let limit_str = limit.to_string();
        let fields = vec![
            ("channel", channel),
            ("latest", latest),
            ("limit", &limit_str),
            ("inclusive", "false"),
        ];
        let data: RawConversationHistory =
            Self::stealth_post(&self.http, &self.creds, "conversations.history", &fields).await?;
        Ok(data.messages)
    }

    /// Fetch messages around a specific timestamp: up to `count` before and `count` after.
    /// Returns messages sorted newest-first (Slack's default).
    pub async fn conversation_history_around(
        &self,
        channel: &str,
        ts: &str,
        count: u32,
    ) -> Result<Vec<Message>, String> {
        // Fetch messages at-and-before ts (inclusive) and after ts.
        let (before, _) = self
            .conversation_history_page_inclusive(channel, "0", Some(ts), count + 1)
            .await?;
        let (after, _) = self
            .conversation_history_page(channel, ts, None, count + 1)
            .await?;

        // Merge and deduplicate by ts
        let mut seen = std::collections::HashSet::new();
        let mut combined = Vec::new();
        for msg in before.into_iter().chain(after) {
            if seen.insert(msg.ts.clone()) {
                combined.push(msg);
            }
        }
        // Sort newest first (Slack ts are lexicographically comparable)
        combined.sort_by(|a, b| b.ts.cmp(&a.ts));
        Ok(combined)
    }

    /// Same as `conversation_history_page` but with `inclusive=true` for the `latest` bound.
    async fn conversation_history_page_inclusive(
        &self,
        channel: &str,
        oldest: &str,
        latest: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<Message>, bool), String> {
        let limit_str = limit.to_string();
        let mut fields = vec![
            ("channel", channel),
            ("oldest", oldest),
            ("limit", &limit_str),
            ("inclusive", "true"),
        ];
        if let Some(l) = latest {
            fields.push(("latest", l));
        }
        let data: RawConversationHistory =
            Self::stealth_post(&self.http, &self.creds, "conversations.history", &fields).await?;
        Ok((data.messages, data.has_more))
    }

    /// Fetch a page of messages older than `latest` with `oldest` lower bound.
    /// Returns (messages, has_more).
    pub async fn conversation_history_page(
        &self,
        channel: &str,
        oldest: &str,
        latest: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<Message>, bool), String> {
        let limit_str = limit.to_string();
        let mut fields = vec![
            ("channel", channel),
            ("oldest", oldest),
            ("limit", &limit_str),
            ("inclusive", "false"),
        ];
        if let Some(l) = latest {
            fields.push(("latest", l));
        }
        let data: RawConversationHistory =
            Self::stealth_post(&self.http, &self.creds, "conversations.history", &fields).await?;
        Ok((data.messages, data.has_more))
    }

    // ── Presence ──

    /// Get presence for a user. Returns "active" or "away".
    pub async fn get_presence(&self, user_id: &str) -> Result<String, String> {
        info!("Calling users.getPresence for user={user_id}");
        let data: RawPresenceResponse =
            Self::stealth_post(&self.http, &self.creds, "users.getPresence", &[("user", user_id)])
                .await?;
        Ok(data.presence.unwrap_or_else(|| "active".into()))
    }

    /// Set presence: "auto" (active) or "away".
    pub async fn set_presence(&self, presence: &str) -> Result<(), String> {
        info!("Calling users.setPresence to {presence}");
        let _: serde_json::Value =
            Self::stealth_post(&self.http, &self.creds, "users.setPresence", &[("presence", presence)])
                .await?;
        Ok(())
    }

    // ── Reactions ──

    pub async fn add_reaction(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), String> {
        info!("Calling reactions.add: {name} on {channel}/{timestamp}");
        let _: serde_json::Value = Self::stealth_post(
            &self.http,
            &self.creds,
            "reactions.add",
            &[("channel", channel), ("timestamp", timestamp), ("name", name)],
        )
        .await?;
        Ok(())
    }

    pub async fn remove_reaction(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), String> {
        info!("Calling reactions.remove: {name} on {channel}/{timestamp}");
        let _: serde_json::Value = Self::stealth_post(
            &self.http,
            &self.creds,
            "reactions.remove",
            &[("channel", channel), ("timestamp", timestamp), ("name", name)],
        )
        .await?;
        Ok(())
    }

    // ── Channel actions ──

    pub async fn leave_channel(&self, channel: &str) -> Result<(), String> {
        info!("Calling conversations.leave for channel={channel}");
        let _: serde_json::Value =
            Self::stealth_post(&self.http, &self.creds, "conversations.leave", &[("channel", channel)])
                .await?;
        Ok(())
    }

    pub async fn archive_channel(&self, channel: &str) -> Result<(), String> {
        info!("Calling conversations.archive for channel={channel}");
        let _: serde_json::Value =
            Self::stealth_post(&self.http, &self.creds, "conversations.archive", &[("channel", channel)])
                .await?;
        Ok(())
    }

    /// Open (or create) a multi-party DM with the given user IDs.
    pub async fn conversations_open(&self, user_ids: &[String]) -> Result<Channel, String> {
        let users_str = user_ids.join(",");
        info!("Calling conversations.open for users={users_str}");
        let data: RawConversationsInfo = Self::stealth_post(
            &self.http,
            &self.creds,
            "conversations.open",
            &[("users", &users_str), ("return_im", "false")],
        )
        .await?;
        Ok(data.channel)
    }

    /// Create a new channel (public or private).
    pub async fn conversations_create(
        &self,
        name: &str,
        is_private: bool,
    ) -> Result<Channel, String> {
        let private_str = if is_private { "true" } else { "false" };
        info!("Calling conversations.create name={name} is_private={private_str}");
        let data: RawConversationsInfo = Self::stealth_post(
            &self.http,
            &self.creds,
            "conversations.create",
            &[("name", name), ("is_private", private_str)],
        )
        .await?;
        Ok(data.channel)
    }

    /// List member user IDs of a conversation (paginated).
    pub async fn conversations_members(&self, channel: &str) -> Result<Vec<String>, String> {
        info!("Calling conversations.members for channel={channel}");
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut fields: Vec<(&str, &str)> = vec![("channel", channel), ("limit", "200")];
            let cursor_val;
            if let Some(c) = &cursor {
                cursor_val = c.clone();
                fields.push(("cursor", &cursor_val));
            }
            let data: RawConversationMembers =
                Self::stealth_post(&self.http, &self.creds, "conversations.members", &fields)
                    .await?;
            all.extend(data.members);
            let next = data
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty());
            if next.is_none() {
                break;
            }
            cursor = next;
        }
        Ok(all)
    }

    /// Invite users to a channel.
    pub async fn conversations_invite(
        &self,
        channel: &str,
        user_ids: &[String],
    ) -> Result<(), String> {
        let users_str = user_ids.join(",");
        info!("Calling conversations.invite channel={channel} users={users_str}");
        let _: serde_json::Value = Self::stealth_post(
            &self.http,
            &self.creds,
            "conversations.invite",
            &[("channel", channel), ("users", &users_str)],
        )
        .await?;
        Ok(())
    }

    pub async fn close_conversation(&self, channel: &str) -> Result<(), String> {
        info!("Calling conversations.close for channel={channel}");
        let _: serde_json::Value =
            Self::stealth_post(&self.http, &self.creds, "conversations.close", &[("channel", channel)])
                .await?;
        Ok(())
    }

    // ── Chat ──

    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<String, String> {
        info!("Calling chat.postMessage to channel={channel}");
        let mut fields = vec![("channel", channel), ("text", text)];
        if let Some(ts) = thread_ts {
            fields.push(("thread_ts", ts));
        }
        let raw: RawPostMessage =
            Self::stealth_post(&self.http, &self.creds, "chat.postMessage", &fields).await?;
        Ok(raw.ts)
    }

    /// Upload a file to a channel using the v2 upload flow:
    /// 1. files.getUploadURLExternal → get upload URL + file_id
    /// 2. PUT file content to the upload URL
    /// 3. files.completeUploadExternal → share to channel
    pub async fn upload_file(
        &self,
        channel: &str,
        content: Vec<u8>,
        filename: &str,
        initial_comment: Option<&str>,
        thread_ts: Option<&str>,
    ) -> Result<(), String> {
        let len = content.len() as u64;
        info!("Uploading file '{filename}' ({len} bytes) to channel={channel} (v2 flow)");

        // Step 1: Get upload URL
        let len_str = len.to_string();
        let get_url_resp: RawGetUploadUrl = Self::stealth_post(
            &self.http, &self.creds,
            "files.getUploadURLExternal",
            &[("filename", filename), ("length", &len_str)],
        ).await?;

        // Step 2: PUT content to the upload URL
        let put_resp = self.http
            .post(&get_url_resp.upload_url)
            .header("Content-Type", "application/octet-stream")
            .body(content)
            .send()
            .await
            .map_err(|e| format!("File PUT error: {e}"))?;
        if !put_resp.status().is_success() {
            return Err(format!("File PUT failed: {}", put_resp.status()));
        }

        // Step 3: Complete upload
        let files_json = serde_json::json!([{"id": get_url_resp.file_id}]).to_string();
        let mut fields = vec![
            ("files", files_json.as_str()),
            ("channel_id", channel),
        ];
        if let Some(comment) = initial_comment {
            fields.push(("initial_comment", comment));
        }
        if let Some(ts) = thread_ts {
            fields.push(("thread_ts", ts));
        }
        let _: serde_json::Value = Self::stealth_post(
            &self.http, &self.creds,
            "files.completeUploadExternal",
            &fields,
        ).await?;
        Ok(())
    }

    pub async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), String> {
        info!("Calling chat.delete on channel={channel} ts={ts}");
        let _: serde_json::Value =
            Self::stealth_post(&self.http, &self.creds, "chat.delete", &[("channel", channel), ("ts", ts)])
                .await?;
        Ok(())
    }

    pub async fn mark_channel(&self, channel: &str, ts: &str) -> Result<(), String> {
        let _: serde_json::Value =
            Self::stealth_post(&self.http, &self.creds, "conversations.mark", &[("channel", channel), ("ts", ts)])
                .await?;
        Ok(())
    }

    pub async fn update_message(&self, channel: &str, ts: &str, text: &str) -> Result<(), String> {
        info!("Calling chat.update on channel={channel} ts={ts}");
        let _: serde_json::Value =
            Self::stealth_post(&self.http, &self.creds, "chat.update", &[("channel", channel), ("ts", ts), ("text", text)])
                .await?;
        Ok(())
    }

    /// Request a Google Meet call link for a channel (stealth-only internal API).
    pub async fn calls_request(&self, channel: &str) -> Result<String, String> {
        info!("Calling calls.request for channel={channel}");
        let raw: RawCallsRequest = Self::stealth_post(
            &self.http,
            &self.creds,
            "calls.request",
            &[
                ("channel", channel),
                ("app", "A0F7YS351"),
                ("type", "video"),
            ],
        )
        .await?;
        Ok(raw.url)
    }

    // ── Thread replies ──

    pub async fn conversation_replies(
        &self,
        channel: &str,
        thread_ts: &str,
    ) -> Result<Vec<Message>, String> {
        info!("Calling conversations.replies for channel={channel}, ts={thread_ts}");
        let fields = vec![
            ("channel", channel),
            ("ts", thread_ts),
            ("limit", "100"),
        ];
        match Self::stealth_post::<RawConversationHistory>(
            &self.http, &self.creds, "conversations.replies", &fields,
        )
        .await
        {
            Ok(data) => {
                info!("conversations.replies returned {} messages", data.messages.len());
                Ok(data.messages)
            }
            // If the thread doesn't exist yet (e.g. user just posted the first reply
            // and Slack hasn't indexed it, or the thread was deleted), treat as empty.
            Err(e) if e.contains("thread_not_found") => {
                info!("conversations.replies thread_not_found — treating as empty");
                Ok(Vec::new())
            }
            Err(e) => Err(e),
        }
    }

    // ── User profile ──

    /// Get a user's profile. Returns the profile as a JSON value.
    pub async fn get_user_profile(&self, user_id: &str) -> Result<serde_json::Value, String> {
        info!("Calling users.profile.get for user={user_id}");
        let data: RawProfileResponse =
            Self::stealth_post(&self.http, &self.creds, "users.profile.get", &[("user", user_id)])
                .await?;
        Ok(data.profile)
    }

    /// Set the authenticated user's status text and emoji.
    pub async fn set_user_status(
        &self,
        status_text: &str,
        status_emoji: &str,
    ) -> Result<(), String> {
        info!("Calling users.profile.set (status_text={status_text:?}, status_emoji={status_emoji:?})");
        let profile = serde_json::json!({
            "status_text": status_text,
            "status_emoji": status_emoji,
        });
        let profile_str = serde_json::to_string(&profile).unwrap();
        let _: RawProfileResponse = Self::stealth_post(
            &self.http,
            &self.creds,
            "users.profile.set",
            &[("profile", &profile_str)],
        )
        .await?;
        Ok(())
    }

    // ── Users ──

    /// Fetch all custom emoji for the workspace.
    /// Returns a map of emoji name -> URL (or "alias:other_name" for aliases).
    pub async fn emoji_list(&self) -> Result<std::collections::HashMap<String, String>, String> {
        info!("Calling emoji.list...");
        let data: RawEmojiList =
            Self::stealth_post(&self.http, &self.creds, "emoji.list", &[]).await?;
        info!("emoji.list returned {} emoji", data.emoji.len());
        Ok(data.emoji)
    }

    /// Fetch all usergroups and return a map of subteam ID -> @handle.
    pub async fn usergroups_list(&self) -> Result<HashMap<String, String>, String> {
        info!("Calling usergroups.list...");
        let data: RawUsergroupsList =
            Self::stealth_post(&self.http, &self.creds, "usergroups.list", &[]).await?;
        let map: HashMap<String, String> = data.usergroups.into_iter()
            .map(|g| (g.id, format!("@{}", g.handle)))
            .collect();
        info!("usergroups.list returned {} groups", map.len());
        Ok(map)
    }

    pub async fn users_list_all(&self) -> Result<Vec<User>, String> {
        info!("Calling users.list (paginated)...");
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        let mut page = 0u32;
        loop {
            page += 1;
            debug!("users.list page {page}");
            let mut fields: Vec<(&str, &str)> = vec![("limit", "200")];
            let cursor_val;
            if let Some(c) = &cursor {
                cursor_val = c.clone();
                fields.push(("cursor", &cursor_val));
            }
            let data: RawUsersList =
                Self::stealth_post(&self.http, &self.creds, "users.list", &fields).await?;
            let count = data.members.len();
            debug!("users.list page {page} returned {count} users");
            all.extend(data.members);
            let next = data
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty());
            if next.is_none() {
                break;
            }
            cursor = next;
        }
        info!("users.list complete: {} users total", all.len());
        Ok(all)
    }
}

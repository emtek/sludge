use reqwest::header::{HeaderMap, HeaderValue, COOKIE};
use slacko::api::chat::PostMessageRequest;
use slacko::api::conversations::{ConversationHistoryRequest, ListConversationsRequest};
use slacko::types::{Channel, Message, User};
use slacko::{AuthConfig, SlackClient};
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
struct RawConversationHistory {
    messages: Vec<Message>,
}

#[derive(Debug, serde::Deserialize)]
struct RawUsersList {
    members: Vec<User>,
    response_metadata: Option<slacko::types::ResponseMetadata>,
}

#[derive(Debug, serde::Deserialize)]
struct RawPostMessage {}

#[derive(Debug, serde::Deserialize)]
struct RawProfileResponse {
    profile: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct RawPresenceResponse {
    presence: Option<String>,
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
enum Backend {
    /// Uses slacko for all API calls (bot/oauth tokens).
    Slacko(SlackClient),
    /// Uses raw reqwest with workspace-specific URL (stealth/xoxc tokens).
    Stealth {
        http: reqwest::Client,
        creds: StealthCreds,
    },
}

#[derive(Clone)]
pub struct Client {
    backend: Backend,
    /// Separate slacko client using the app-level token for Socket Mode.
    app_token_client: Option<SlackClient>,
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
    /// Create a client using slacko (for bot/oauth tokens).
    pub fn new_bot(auth: AuthConfig, app_token: Option<String>) -> Self {
        let inner = SlackClient::new(auth).expect("Failed to create Slack client");
        let app_token_client = app_token.map(|t| {
            SlackClient::new(AuthConfig::bot(&t)).expect("Failed to create app token client")
        });
        Self {
            backend: Backend::Slacko(inner),
            app_token_client,
        }
    }

    /// Create a client using raw HTTP for stealth (xoxc/xoxd) auth.
    pub fn new_stealth(xoxc_token: String, xoxd_cookie: String) -> Self {
        let http = reqwest::Client::new();
        Self {
            backend: Backend::Stealth {
                http,
                creds: StealthCreds {
                    xoxc_token,
                    xoxd_cookie,
                    workspace_url: None,
                },
            },
            app_token_client: None,
        }
    }

    pub fn socket_mode_client(&self) -> Option<&SlackClient> {
        self.app_token_client.as_ref()
    }

    /// Returns stealth credentials for RTM if using stealth backend.
    /// Returns (http_client, xoxc_token, xoxd_cookie, workspace_url).
    pub fn stealth_rtm_params(
        &self,
    ) -> Option<(reqwest::Client, String, String, Option<String>)> {
        match &self.backend {
            Backend::Stealth { http, creds } => Some((
                http.clone(),
                creds.xoxc_token.clone(),
                creds.xoxd_cookie.clone(),
                creds.workspace_url.clone(),
            )),
            _ => None,
        }
    }

    /// Fetch image bytes with local disk caching.
    /// Cached files are stored under `~/.local/share/slack-frontend/image_cache/`.
    pub async fn fetch_image_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        use std::hash::{Hash, Hasher};

        // Build a deterministic cache path from the URL
        let cache_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("slack-frontend")
            .join("image_cache");

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut hasher);
        let hash = hasher.finish();
        let cache_path = cache_dir.join(format!("{hash:016x}"));

        // Return cached bytes if available
        if let Ok(bytes) = tokio::fs::read(&cache_path).await {
            tracing::debug!("Image cache HIT: {}", cache_path.display());
            return Ok(bytes);
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
    async fn fetch_image_bytes_uncached(&self, url: &str) -> Result<Vec<u8>, String> {
        let is_slack_url = url.contains("slack.com") || url.contains("slack-files.com");

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|e| format!("HTTP client error: {e}"))?;

        let req = if is_slack_url {
            match &self.backend {
                Backend::Stealth { creds, .. } => {
                    // File hosts (files.slack.com, files-origin.slack.com) need
                    // Bearer auth rather than cookie + form token.
                    http.get(url)
                        .bearer_auth(&creds.xoxc_token)
                        .headers(Self::stealth_headers(creds))
                }
                Backend::Slacko(_) => http.get(url),
            }
        } else {
            http.get(url)
        };

        let resp = req.send().await.map_err(|e| format!("Image fetch error: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Image fetch {}: {}", resp.status(), url));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
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
        let body: RawResponse<T> = resp
            .json()
            .await
            .map_err(|e| format!("Parse error (status {status}): {e}"))?;

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
        match &mut self.backend {
            Backend::Slacko(inner) => {
                let r = inner.auth().test().await.map_err(|e| format!("Auth error: {e}"))?;
                info!("auth.test OK — user: {}, team: {}, url: {}", r.user, r.team, r.url);
                Ok(AuthInfo {
                    url: r.url,
                    team: r.team,
                    user: r.user,
                    team_id: r.team_id,
                    user_id: r.user_id,
                })
            }
            Backend::Stealth { http, creds } => {
                let r: RawAuthTest =
                    Self::stealth_post(http, creds, "auth.test", &[]).await?;
                let url = r.url.unwrap_or_default();
                let team = r.team.unwrap_or_default();
                let user = r.user.unwrap_or_default();
                let team_id = r.team_id.unwrap_or_default();
                let user_id = r.user_id.unwrap_or_default();

                info!("auth.test OK — user: {user}, team: {team}, url: {url}");

                // Discover workspace URL for subsequent calls
                if !url.is_empty() {
                    let ws_url = url.trim_end_matches('/').to_string();
                    info!("Setting workspace base URL to: {ws_url}");
                    creds.workspace_url = Some(ws_url);
                }

                Ok(AuthInfo { url, team, user, team_id, user_id })
            }
        }
    }

    // ── Conversations ──

    pub async fn conversations_list_all(&self) -> Result<Vec<Channel>, String> {
        info!("Calling conversations.list (paginated)...");
        match &self.backend {
            Backend::Slacko(inner) => {
                let mut all = Vec::new();
                let mut cursor: Option<String> = None;
                let mut page = 0u32;
                loop {
                    page += 1;
                    debug!("conversations.list page {page}");
                    let req = ListConversationsRequest {
                        types: Some("public_channel,private_channel,im,mpim".to_string()),
                        exclude_archived: Some(true),
                        limit: Some(200),
                        cursor: cursor.clone(),
                    };
                    let data = inner
                        .conversations()
                        .list_with_options(req)
                        .await
                        .map_err(|e| {
                            error!("conversations.list failed on page {page}: {e}");
                            format!("API error: {e}")
                        })?;
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
            Backend::Stealth { http, creds } => {
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
                        Self::stealth_post(http, creds, "conversations.list", &fields).await?;
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
        }
    }

    pub async fn conversation_history(
        &self,
        channel: &str,
        limit: u32,
    ) -> Result<Vec<Message>, String> {
        info!("Calling conversations.history for channel={channel}, limit={limit}");
        match &self.backend {
            Backend::Slacko(inner) => {
                let req = ConversationHistoryRequest {
                    channel: channel.to_string(),
                    limit: Some(limit),
                    cursor: None,
                    oldest: None,
                    latest: None,
                    inclusive: None,
                };
                let data = inner
                    .conversations()
                    .history_with_options(req)
                    .await
                    .map_err(|e| {
                        error!("conversations.history failed for {channel}: {e}");
                        format!("API error: {e}")
                    })?;
                info!("conversations.history returned {} messages", data.messages.len());
                Ok(data.messages)
            }
            Backend::Stealth { http, creds } => {
                let limit_str = limit.to_string();
                let fields = vec![("channel", channel), ("limit", &limit_str)];
                let data: RawConversationHistory =
                    Self::stealth_post(http, creds, "conversations.history", &fields).await?;
                info!("conversations.history returned {} messages", data.messages.len());
                Ok(data.messages)
            }
        }
    }

    // ── Presence ──

    /// Get presence for a user. Returns "active" or "away".
    pub async fn get_presence(&self, user_id: &str) -> Result<String, String> {
        info!("Calling users.getPresence for user={user_id}");
        match &self.backend {
            Backend::Slacko(inner) => {
                let resp = inner
                    .users()
                    .get_presence(user_id)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(resp.presence)
            }
            Backend::Stealth { http, creds } => {
                let data: RawPresenceResponse =
                    Self::stealth_post(http, creds, "users.getPresence", &[("user", user_id)])
                        .await?;
                Ok(data.presence.unwrap_or_else(|| "active".into()))
            }
        }
    }

    /// Set presence: "auto" (active) or "away".
    pub async fn set_presence(&self, presence: &str) -> Result<(), String> {
        info!("Calling users.setPresence to {presence}");
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .users()
                    .set_presence(presence)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let _: serde_json::Value =
                    Self::stealth_post(http, creds, "users.setPresence", &[("presence", presence)])
                        .await?;
                Ok(())
            }
        }
    }

    // ── Reactions ──

    pub async fn add_reaction(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), String> {
        info!("Calling reactions.add: {name} on {channel}/{timestamp}");
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .reactions()
                    .add(channel, timestamp, name)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let _: serde_json::Value = Self::stealth_post(
                    http,
                    creds,
                    "reactions.add",
                    &[("channel", channel), ("timestamp", timestamp), ("name", name)],
                )
                .await?;
                Ok(())
            }
        }
    }

    pub async fn remove_reaction(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), String> {
        info!("Calling reactions.remove: {name} on {channel}/{timestamp}");
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .reactions()
                    .remove(channel, timestamp, name)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let _: serde_json::Value = Self::stealth_post(
                    http,
                    creds,
                    "reactions.remove",
                    &[("channel", channel), ("timestamp", timestamp), ("name", name)],
                )
                .await?;
                Ok(())
            }
        }
    }

    // ── Channel actions ──

    pub async fn leave_channel(&self, channel: &str) -> Result<(), String> {
        info!("Calling conversations.leave for channel={channel}");
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .conversations()
                    .leave(channel)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let _: serde_json::Value =
                    Self::stealth_post(http, creds, "conversations.leave", &[("channel", channel)])
                        .await?;
                Ok(())
            }
        }
    }

    pub async fn archive_channel(&self, channel: &str) -> Result<(), String> {
        info!("Calling conversations.archive for channel={channel}");
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .conversations()
                    .archive(channel)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let _: serde_json::Value =
                    Self::stealth_post(http, creds, "conversations.archive", &[("channel", channel)])
                        .await?;
                Ok(())
            }
        }
    }

    pub async fn close_conversation(&self, channel: &str) -> Result<(), String> {
        info!("Calling conversations.close for channel={channel}");
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .conversations()
                    .close(channel)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let _: serde_json::Value =
                    Self::stealth_post(http, creds, "conversations.close", &[("channel", channel)])
                        .await?;
                Ok(())
            }
        }
    }

    // ── Chat ──

    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<(), String> {
        info!("Calling chat.postMessage to channel={channel}");
        match &self.backend {
            Backend::Slacko(inner) => {
                let mut req = PostMessageRequest::new(channel).text(text);
                if let Some(ts) = thread_ts {
                    req = req.thread_ts(ts);
                }
                inner
                    .chat()
                    .post_message_with_options(req)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let mut fields = vec![("channel", channel), ("text", text)];
                if let Some(ts) = thread_ts {
                    fields.push(("thread_ts", ts));
                }
                let _: RawPostMessage =
                    Self::stealth_post(http, creds, "chat.postMessage", &fields).await?;
                Ok(())
            }
        }
    }

    pub async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), String> {
        info!("Calling chat.delete on channel={channel} ts={ts}");
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .chat()
                    .delete_message(channel, ts)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let _: serde_json::Value =
                    Self::stealth_post(http, creds, "chat.delete", &[("channel", channel), ("ts", ts)])
                        .await?;
                Ok(())
            }
        }
    }

    /// Fetch messages newer than `oldest` timestamp.
    pub async fn conversation_history_since(
        &self,
        channel: &str,
        oldest: &str,
    ) -> Result<Vec<Message>, String> {
        debug!("Calling conversations.history for channel={channel}, oldest={oldest}");
        match &self.backend {
            Backend::Slacko(inner) => {
                let req = ConversationHistoryRequest {
                    channel: channel.to_string(),
                    limit: Some(100),
                    cursor: None,
                    oldest: Some(oldest.to_string()),
                    latest: None,
                    inclusive: None,
                };
                let data = inner
                    .conversations()
                    .history_with_options(req)
                    .await
                    .map_err(|e| {
                        error!("conversations.history failed for {channel}: {e}");
                        format!("API error: {e}")
                    })?;
                Ok(data.messages)
            }
            Backend::Stealth { http, creds } => {
                let fields = vec![
                    ("channel", channel),
                    ("oldest", oldest),
                    ("limit", "100"),
                ];
                let data: RawConversationHistory =
                    Self::stealth_post(http, creds, "conversations.history", &fields).await?;
                Ok(data.messages)
            }
        }
    }

    // ── Thread replies ──

    pub async fn conversation_replies(
        &self,
        channel: &str,
        thread_ts: &str,
    ) -> Result<Vec<Message>, String> {
        info!("Calling conversations.replies for channel={channel}, ts={thread_ts}");
        match &self.backend {
            Backend::Slacko(inner) => {
                let data = inner
                    .conversations()
                    .replies(channel, thread_ts)
                    .await
                    .map_err(|e| {
                        error!("conversations.replies failed: {e}");
                        format!("API error: {e}")
                    })?;
                info!("conversations.replies returned {} messages", data.messages.len());
                Ok(data.messages)
            }
            Backend::Stealth { http, creds } => {
                let fields = vec![
                    ("channel", channel),
                    ("ts", thread_ts),
                    ("limit", "100"),
                ];
                let data: RawConversationHistory =
                    Self::stealth_post(http, creds, "conversations.replies", &fields).await?;
                info!("conversations.replies returned {} messages", data.messages.len());
                Ok(data.messages)
            }
        }
    }

    // ── User profile ──

    /// Get a user's profile. Returns the profile as a JSON value.
    pub async fn get_user_profile(&self, user_id: &str) -> Result<serde_json::Value, String> {
        info!("Calling users.profile.get for user={user_id}");
        match &self.backend {
            Backend::Slacko(inner) => {
                let resp = inner
                    .users()
                    .get_profile(user_id)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(resp.profile)
            }
            Backend::Stealth { http, creds } => {
                let data: RawProfileResponse =
                    Self::stealth_post(http, creds, "users.profile.get", &[("user", user_id)])
                        .await?;
                Ok(data.profile)
            }
        }
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
        match &self.backend {
            Backend::Slacko(inner) => {
                inner
                    .users()
                    .set_profile(profile)
                    .await
                    .map_err(|e| format!("API error: {e}"))?;
                Ok(())
            }
            Backend::Stealth { http, creds } => {
                let profile_str = serde_json::to_string(&profile).unwrap();
                let _: RawProfileResponse = Self::stealth_post(
                    http,
                    creds,
                    "users.profile.set",
                    &[("profile", &profile_str)],
                )
                .await?;
                Ok(())
            }
        }
    }

    // ── Users ──

    pub async fn users_list_all(&self) -> Result<Vec<User>, String> {
        info!("Calling users.list (paginated)...");
        match &self.backend {
            Backend::Slacko(inner) => {
                let mut all = Vec::new();
                let mut cursor: Option<String> = None;
                let mut page = 0u32;
                loop {
                    page += 1;
                    debug!("users.list page {page}");
                    let req = slacko::api::users::UsersListRequest {
                        limit: Some(200),
                        cursor: cursor.clone(),
                    };
                    let data = inner
                        .users()
                        .list_with_options(req)
                        .await
                        .map_err(|e| {
                            error!("users.list failed on page {page}: {e}");
                            format!("API error: {e}")
                        })?;
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
            Backend::Stealth { http, creds } => {
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
                        Self::stealth_post(http, creds, "users.list", &fields).await?;
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
    }
}

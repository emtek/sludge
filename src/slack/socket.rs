use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

/// Events emitted by the socket listener for the UI to consume.
#[derive(Debug, Clone)]
pub enum SlackEvent {
    /// A new message was posted in a channel.
    MessageReceived {
        channel: String,
        user: Option<String>,
        text: String,
        ts: String,
        thread_ts: Option<String>,
        files: Option<Vec<slacko::types::File>>,
    },
    /// A user's presence changed (active/away).
    PresenceChange {
        user: String,
        presence: String,
    },
    /// A user's profile was updated (status, avatar, etc.).
    UserProfileChanged {
        user: String,
        profile: serde_json::Value,
    },
    /// A reaction was added or removed from a message.
    ReactionChanged {
        channel: String,
        message_ts: String,
        reaction: String,
        user: String,
        added: bool,
    },
    /// A user is typing in a channel/thread.
    UserTyping {
        channel: String,
        user: String,
    },
    /// A channel was marked as read (from another client).
    ChannelMarked {
        channel: String,
        unread_count_display: u32,
    },
    /// A message received new replies (authoritative reply count from server).
    MessageReplied {
        channel: String,
        thread_ts: String,
        reply_count: usize,
    },
    /// Indicates the WebSocket connection is alive.
    Connected,
    /// Connection was lost; will attempt to reconnect.
    Disconnected,
}

// ── RTM ──

/// Run RTM WebSocket for stealth mode with auto-reconnect.
/// `presence_rx` receives batches of user IDs to subscribe to for presence updates.
pub async fn run_rtm_stealth(
    http: reqwest::Client,
    xoxc_token: String,
    xoxd_cookie: String,
    workspace_url: Option<String>,
    tx: mpsc::UnboundedSender<SlackEvent>,
    mut presence_rx: mpsc::UnboundedReceiver<Vec<String>>,
) {
    loop {
        match rtm_connect(&http, &xoxc_token, &xoxd_cookie, workspace_url.as_deref()).await {
            Ok(ws_url) => {
                info!("RTM connecting to WebSocket...");
                let _ = tx.send(SlackEvent::Connected);

                if let Err(e) = rtm_listen(&ws_url, &xoxd_cookie, &tx, &mut presence_rx).await {
                    error!("RTM WebSocket error: {e}");
                }

                let _ = tx.send(SlackEvent::Disconnected);
                warn!("RTM disconnected, reconnecting in 5s...");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => {
                error!("rtm.connect failed: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        }
    }
}

async fn rtm_connect(
    http: &reqwest::Client,
    xoxc_token: &str,
    xoxd_cookie: &str,
    workspace_url: Option<&str>,
) -> Result<String, String> {
    let base = workspace_url.unwrap_or("https://slack.com");
    let url = format!("{base}/api/rtm.connect");

    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("d={xoxd_cookie}")) {
        headers.insert(reqwest::header::COOKIE, val);
    }

    let resp = http
        .post(&url)
        .headers(headers)
        .form(&[("token", xoxc_token)])
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    #[derive(serde::Deserialize)]
    struct RawResp {
        ok: bool,
        error: Option<String>,
        url: Option<String>,
    }

    let body: RawResp = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if !body.ok {
        return Err(format!(
            "rtm.connect failed: {}",
            body.error.unwrap_or_else(|| "unknown".into())
        ));
    }

    let url = body.url.ok_or_else(|| "rtm.connect: no URL returned".to_string())?;
    info!("rtm.connect returned URL: {url}");
    Ok(url)
}

async fn rtm_listen(
    ws_url: &str,
    xoxd_cookie: &str,
    tx: &mpsc::UnboundedSender<SlackEvent>,
    presence_rx: &mut mpsc::UnboundedReceiver<Vec<String>>,
) -> Result<(), String> {
    use tokio_tungstenite::tungstenite::http::Request;

    let request = Request::builder()
        .uri(ws_url)
        .header("Cookie", format!("d={xoxd_cookie}"))
        .header("Host", "wss-primary.slack.com")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .map_err(|e| format!("Request build error: {e}"))?;

    let (ws_stream, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("WebSocket connect error: {e}"))?;

    info!("RTM WebSocket connected");

    let (mut write, mut read) = ws_stream.split();
    let mut msg_id: u64 = 1;

    loop {
        tokio::select! {
            ws_msg = read.next() => {
                let Some(msg) = ws_msg else { break };
                match msg {
                    Ok(WsMessage::Text(text)) => {
                        debug!("RTM received: {text}");

                        if let Ok(evt) = serde_json::from_str::<serde_json::Value>(&text.to_string()) {
                            let evt_type = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");

                            match evt_type {
                                "hello" => {
                                    info!("RTM hello received");
                                }
                                _ => {
                                    dispatch_event(&evt, tx);
                                }
                            }
                        }
                    }
                    Ok(WsMessage::Ping(data)) => {
                        debug!("RTM ping");
                        if let Err(e) = write.send(WsMessage::Pong(data)).await {
                            error!("Failed to send pong: {e}");
                        }
                    }
                    Ok(WsMessage::Close(_)) => {
                        warn!("RTM WebSocket closed by server");
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return Err(format!("WebSocket error: {e}"));
                    }
                }
            }
            Some(user_ids) = presence_rx.recv() => {
                if !user_ids.is_empty() {
                    let sub_msg = serde_json::json!({
                        "type": "presence_sub",
                        "ids": user_ids,
                        "id": msg_id,
                    });
                    msg_id += 1;
                    info!("Sending presence_sub for {} users", user_ids.len());
                    if let Err(e) = write.send(WsMessage::Text(sub_msg.to_string().into())).await {
                        error!("Failed to send presence_sub: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Shared event dispatcher ──

fn dispatch_event(evt: &serde_json::Value, tx: &mpsc::UnboundedSender<SlackEvent>) {
    let evt_type = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match evt_type {
        "message" => {
            let subtype = evt.get("subtype").and_then(|v| v.as_str());

            // Handle message_replied: update thread reply counts
            if subtype == Some("message_replied") {
                if let Some(msg) = evt.get("message") {
                    let channel = evt.get("channel").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let thread_ts = msg.get("thread_ts").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let reply_count = msg.get("reply_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    if !channel.is_empty() && !thread_ts.is_empty() {
                        let _ = tx.send(SlackEvent::MessageReplied {
                            channel,
                            thread_ts,
                            reply_count,
                        });
                    }
                }
                return;
            }

            // Skip subtypes we can't handle (allow thread_broadcast, bot_message, file_share)
            if let Some(st) = subtype {
                if st != "thread_broadcast" && st != "bot_message" && st != "file_share" {
                    debug!("Skipping message subtype: {st}");
                    return;
                }
            }

            let channel = evt
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let user = evt.get("user").and_then(|v| v.as_str()).map(String::from);
            let mut text = evt
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // For bot messages with empty text, extract content from blocks/attachments
            if text.is_empty() {
                text = extract_text_from_blocks_and_attachments(evt);
            }

            let ts = evt
                .get("ts")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let thread_ts = evt
                .get("thread_ts")
                .and_then(|v| v.as_str())
                .map(String::from);

            let files: Option<Vec<slacko::types::File>> = evt
                .get("files")
                .and_then(|v| serde_json::from_value(v.clone()).ok());

            if !channel.is_empty() && !ts.is_empty() {
                let _ = tx.send(SlackEvent::MessageReceived {
                    channel,
                    user,
                    text,
                    ts,
                    thread_ts,
                    files,
                });
            }
        }
        "presence_change" | "manual_presence_change" => {
            let presence = evt
                .get("presence")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if presence.is_empty() {
                return;
            }

            // Batch format: { "type": "presence_change", "users": ["U1","U2"], "presence": "active" }
            if let Some(users) = evt.get("users").and_then(|v| v.as_array()) {
                for u in users {
                    if let Some(uid) = u.as_str() {
                        let _ = tx.send(SlackEvent::PresenceChange {
                            user: uid.to_string(),
                            presence: presence.clone(),
                        });
                    }
                }
            } else {
                // Single user format
                let user = evt
                    .get("user")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let _ = tx.send(SlackEvent::PresenceChange { user, presence });
            }
        }
        "user_profile_changed" | "user_change" => {
            if let Some(user_obj) = evt.get("user") {
                let user_id = user_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(profile) = user_obj.get("profile") {
                    if !user_id.is_empty() {
                        let _ = tx.send(SlackEvent::UserProfileChanged {
                            user: user_id,
                            profile: profile.clone(),
                        });
                    }
                }
            }
        }
        "user_typing" => {
            let channel = evt.get("channel").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let user = evt.get("user").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !channel.is_empty() && !user.is_empty() {
                let _ = tx.send(SlackEvent::UserTyping { channel, user });
            }
        }
        "im_marked" | "channel_marked" | "group_marked" | "mpim_marked" => {
            let channel = evt.get("channel").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let unread_count_display = evt.get("unread_count_display")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            if !channel.is_empty() {
                let _ = tx.send(SlackEvent::ChannelMarked { channel, unread_count_display });
            }
        }
        "reaction_added" | "reaction_removed" => {
            let user = evt.get("user").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let reaction = evt.get("reaction").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let item = evt.get("item");
            let channel = item.and_then(|i| i.get("channel")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let message_ts = item.and_then(|i| i.get("ts")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let added = evt_type == "reaction_added";
            if !channel.is_empty() && !message_ts.is_empty() && !reaction.is_empty() {
                let _ = tx.send(SlackEvent::ReactionChanged {
                    channel, message_ts, reaction, user, added,
                });
            }
        }
        // Informational events we can safely ignore
        "thread_subscribed" | "update_global_thread_state" | "activity" => {}
        _ => {
            debug!("Unhandled RTM event type: {evt_type}");
        }
    }
}

/// Extract readable text from Slack blocks and attachments when the top-level text is empty.
/// This handles bot/integration messages (e.g. GitHub) that put all content in blocks.
fn extract_text_from_blocks_and_attachments(evt: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Extract text from blocks
    if let Some(blocks) = evt.get("blocks").and_then(|v| v.as_array()) {
        for block in blocks {
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match block_type {
                "section" => {
                    if let Some(text) = block.get("text").and_then(|v| v.get("text")).and_then(|v| v.as_str()) {
                        parts.push(text.to_string());
                    }
                    if let Some(fields) = block.get("fields").and_then(|v| v.as_array()) {
                        for field in fields {
                            if let Some(t) = field.get("text").and_then(|v| v.as_str()) {
                                parts.push(t.to_string());
                            }
                        }
                    }
                }
                "header" => {
                    if let Some(text) = block.get("text").and_then(|v| v.get("text")).and_then(|v| v.as_str()) {
                        parts.push(format!("*{text}*"));
                    }
                }
                "rich_text" => {
                    extract_rich_text_block(block, &mut parts);
                }
                "context" => {
                    if let Some(elements) = block.get("elements").and_then(|v| v.as_array()) {
                        let ctx: Vec<&str> = elements.iter()
                            .filter_map(|e| e.get("text").and_then(|v| v.as_str()))
                            .collect();
                        if !ctx.is_empty() {
                            parts.push(ctx.join(" "));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Extract text from attachments
    if let Some(attachments) = evt.get("attachments").and_then(|v| v.as_array()) {
        for att in attachments {
            if let Some(pretext) = att.get("pretext").and_then(|v| v.as_str()) {
                if !pretext.is_empty() {
                    parts.push(pretext.to_string());
                }
            }
            if let Some(title) = att.get("title").and_then(|v| v.as_str()) {
                if !title.is_empty() {
                    parts.push(format!("*{title}*"));
                }
            }
            if let Some(text) = att.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
            if parts.is_empty() {
                if let Some(fallback) = att.get("fallback").and_then(|v| v.as_str()) {
                    if !fallback.is_empty() {
                        parts.push(fallback.to_string());
                    }
                }
            }
        }
    }

    parts.join("\n")
}

/// Extract text from a rich_text block (used by many integrations).
fn extract_rich_text_block(block: &serde_json::Value, parts: &mut Vec<String>) {
    if let Some(elements) = block.get("elements").and_then(|v| v.as_array()) {
        for element in elements {
            let el_type = element.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match el_type {
                "rich_text_section" | "rich_text_preformatted" | "rich_text_quote" => {
                    if let Some(inner) = element.get("elements").and_then(|v| v.as_array()) {
                        let line: String = inner.iter()
                            .filter_map(|e| {
                                match e.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                                    "text" => e.get("text").and_then(|v| v.as_str()).map(String::from),
                                    "link" => {
                                        let url = e.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                        let text = e.get("text").and_then(|v| v.as_str()).unwrap_or(url);
                                        Some(text.to_string())
                                    }
                                    "user" => e.get("user_id").and_then(|v| v.as_str()).map(|id| format!("<@{id}>")),
                                    "emoji" => e.get("name").and_then(|v| v.as_str()).map(|n| format!(":{n}:")),
                                    _ => None,
                                }
                            })
                            .collect();
                        if !line.is_empty() {
                            parts.push(line);
                        }
                    }
                }
                "rich_text_list" => {
                    if let Some(items) = element.get("elements").and_then(|v| v.as_array()) {
                        for (i, item) in items.iter().enumerate() {
                            if let Some(inner) = item.get("elements").and_then(|v| v.as_array()) {
                                let line: String = inner.iter()
                                    .filter_map(|e| e.get("text").and_then(|v| v.as_str()))
                                    .collect();
                                if !line.is_empty() {
                                    let style = element.get("style").and_then(|v| v.as_str()).unwrap_or("bullet");
                                    let prefix = if style == "ordered" {
                                        format!("{}. ", i + 1)
                                    } else {
                                        "• ".to_string()
                                    };
                                    parts.push(format!("{prefix}{line}"));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

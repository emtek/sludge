use futures_util::{SinkExt, StreamExt};
use slacko::api::socket_mode::{SocketModeEvent, SocketModePayload};
use slacko::SlackClient;
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
        thread_ts: Option<String>,
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

// ── Socket Mode (bot tokens with app-level token) ──

/// Run Socket Mode using slacko's built-in WebSocket management with auto-reconnect.
pub async fn run_socket_mode(client: SlackClient, tx: mpsc::UnboundedSender<SlackEvent>) {
    let result = client
        .socket_mode()
        .start_with_reconnect(move |event: SocketModeEvent| {
            handle_socket_mode_event(&event, &tx);
            None
        })
        .await;

    if let Err(e) = result {
        error!("Socket Mode exited with error: {e}");
    }
}

fn handle_socket_mode_event(
    event: &SocketModeEvent,
    tx: &mpsc::UnboundedSender<SlackEvent>,
) {
    match &event.payload {
        SocketModePayload::EventsApi(payload) => {
            if let Some(evt) = &payload.event {
                dispatch_event(evt, tx);
            }
        }
        SocketModePayload::Hello => {
            info!("Socket Mode connected");
            let _ = tx.send(SlackEvent::Connected);
        }
        SocketModePayload::Disconnect { .. } => {
            info!("Socket Mode disconnect requested");
            let _ = tx.send(SlackEvent::Disconnected);
        }
        _ => {}
    }
}

// ── RTM (stealth/xoxc tokens) ──

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

            // Skip other subtypes (except thread_broadcast)
            if let Some(st) = subtype {
                if st != "thread_broadcast" {
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
            let text = evt
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ts = evt
                .get("ts")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let thread_ts = evt
                .get("thread_ts")
                .and_then(|v| v.as_str())
                .map(String::from);

            if !channel.is_empty() && !ts.is_empty() {
                let _ = tx.send(SlackEvent::MessageReceived {
                    channel,
                    user,
                    text,
                    ts,
                    thread_ts,
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
            let thread_ts = evt.get("thread_ts").and_then(|v| v.as_str()).map(String::from);
            if !channel.is_empty() && !user.is_empty() {
                let _ = tx.send(SlackEvent::UserTyping { channel, user, thread_ts });
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

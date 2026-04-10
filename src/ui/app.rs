use gtk4::prelude::*;
use gtk4::{self as gtk, Application, ApplicationWindow, Label};
use gtk4::gio;
use slacko::types::{Channel, Message};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::db::{Database, RecentStatus};
use crate::slack::helpers::replace_emoji_shortcodes;
use crate::slack::client::Client;
use crate::slack::helpers::{channel_display_name, format_message_plain, user_display_name};
use crate::slack::socket::SlackEvent;
use crate::ui::channel_sidebar::ChannelSidebar;
use crate::ui::message_input::MessageInput;
use crate::ui::message_view::MessageView;
use crate::ui::thread_panel::ThreadPanel;

/// Returns true if the error string indicates an authentication/authorization
/// failure that means the current token is unusable.
fn is_fatal_auth_error(err: &str) -> bool {
    const FATAL_ERRORS: &[&str] = &[
        "missing_scope",
        "invalid_auth",
        "not_authed",
        "token_revoked",
        "token_expired",
        "account_inactive",
    ];
    FATAL_ERRORS.iter().any(|e| err.contains(e))
}

/// Action to perform on startup, from CLI flags.
#[derive(Clone, Debug)]
pub enum StartupAction {
    /// Open a channel: `--open ch:CHANNEL_ID`
    OpenChannel(String),
    /// Open a message: `--open msg:CHANNEL_ID:TS`
    OpenMessage { channel_id: String, message_ts: String },
    /// Run a search: `--search QUERY`
    Search(String),
}

/// Shared application state accessible from GTK callbacks.
struct AppState {
    channels: Vec<Channel>,
    /// Map from user ID to display name (Rc-wrapped for cheap cloning).
    user_names: Rc<HashMap<String, String>>,
    /// Map from subteam/usergroup ID to @handle.
    subteam_names: Rc<HashMap<String, String>>,
    /// The authenticated user's ID.
    self_user_id: String,
    /// Currently selected channel ID.
    current_channel: Option<String>,
    /// Currently open thread (thread_ts, channel_id).
    current_thread: Option<(String, String)>,
    /// Unread message counts per channel.
    unread_counts: HashMap<String, u32>,
    /// Message ts to scroll to after channel load (set by notification click).
    pending_scroll: Option<String>,
    /// (thread_ts, reply_ts) to open thread panel and scroll to after channel load.
    pending_thread: Option<(String, String)>,
    /// Timestamps of messages we sent optimistically (to suppress socket duplicates).
    sent_ts: std::collections::HashSet<String>,
}

pub fn build_app(
    app: &Application,
    client: Client,
    rt: tokio::runtime::Handle,
    mut event_rx: mpsc::UnboundedReceiver<SlackEvent>,
    db: Arc<Database>,
    user_id: String,
    presence_tx: mpsc::UnboundedSender<Vec<String>>,
    startup_action: Option<StartupAction>,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Sludge")
        .default_width(1200)
        .default_height(800)
        .build();

    // ── Layout ──
    // [ Sidebar | Message area ]
    //                [ Messages ]
    //                [ Input    ]

    // ── Header bar with profile button ──
    let header_bar = gtk::HeaderBar::new();

    // Empty title — channel name is shown in the message view header
    header_bar.set_title_widget(Some(&gtk::Label::new(None)));

    // Search button to toggle channel filter in the sidebar
    let channel_search_btn = gtk::ToggleButton::new();
    channel_search_btn.set_icon_name("system-search-symbolic");
    channel_search_btn.add_css_class("flat");
    header_bar.pack_start(&channel_search_btn);

    let avatar_size = 32;
    let avatar_texture: Rc<RefCell<Option<gtk4::gdk::Texture>>> = Rc::new(RefCell::new(None));
    // true = active (filled green dot), false = away (hollow ring)
    let presence_active: Rc<RefCell<bool>> = Rc::new(RefCell::new(true));
    let profile_avatar = gtk::DrawingArea::new();
    profile_avatar.set_size_request(avatar_size, avatar_size);
    profile_avatar.set_content_width(avatar_size);
    profile_avatar.set_content_height(avatar_size);
    {
        let texture = avatar_texture.clone();
        let presence = presence_active.clone();
        profile_avatar.set_draw_func(move |_da, cr, width, height| {
            let w = width as f64;
            let h = height as f64;
            let radius = 8.0;

            // Rounded rectangle path
            cr.new_sub_path();
            cr.arc(w - radius, radius, radius, -std::f64::consts::FRAC_PI_2, 0.0);
            cr.arc(w - radius, h - radius, radius, 0.0, std::f64::consts::FRAC_PI_2);
            cr.arc(radius, h - radius, radius, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
            cr.arc(radius, radius, radius, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
            cr.close_path();

            if let Some(tex) = texture.borrow().as_ref() {
                cr.clip();
                let snapshot = gtk::Snapshot::new();
                tex.snapshot(snapshot.upcast_ref::<gtk4::gdk::Snapshot>(), w, h);
                if let Some(node) = snapshot.to_node() {
                    node.draw(cr);
                }
                cr.reset_clip();
            } else {
                cr.set_source_rgba(0.3, 0.3, 0.3, 1.0);
                let _ = cr.fill();
            }

            // Status indicator (bottom-right)
            let dot_r = 5.0;
            let dot_cx = w - dot_r - 1.0;
            let dot_cy = h - dot_r - 1.0;

            // Dark border ring
            cr.arc(dot_cx, dot_cy, dot_r + 2.0, 0.0, 2.0 * std::f64::consts::PI);
            cr.set_source_rgba(0.15, 0.15, 0.15, 1.0);
            let _ = cr.fill();

            if *presence.borrow() {
                // Active: filled green dot
                cr.arc(dot_cx, dot_cy, dot_r, 0.0, 2.0 * std::f64::consts::PI);
                cr.set_source_rgba(0.18, 0.8, 0.35, 1.0);
                let _ = cr.fill();
            } else {
                // Away: hollow white ring
                cr.arc(dot_cx, dot_cy, dot_r, 0.0, 2.0 * std::f64::consts::PI);
                cr.set_source_rgba(0.85, 0.85, 0.85, 1.0);
                let _ = cr.fill();
                // Punch out inner circle for ring effect
                cr.arc(dot_cx, dot_cy, dot_r - 2.0, 0.0, 2.0 * std::f64::consts::PI);
                cr.set_source_rgba(0.15, 0.15, 0.15, 1.0);
                let _ = cr.fill();
            }
        });
    }

    // Status popover
    let popover = gtk::Popover::new();
    let popover_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    popover_box.set_margin_top(12);
    popover_box.set_margin_bottom(12);
    popover_box.set_margin_start(12);
    popover_box.set_margin_end(12);

    // ── Presence toggle ──
    let presence_header = Label::new(Some("Presence"));
    presence_header.add_css_class("heading");
    presence_header.set_halign(gtk::Align::Start);
    popover_box.append(&presence_header);

    let presence_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let active_btn = gtk::ToggleButton::with_label("Active");
    let away_btn = gtk::ToggleButton::with_label("Away");
    active_btn.set_active(true);
    away_btn.set_group(Some(&active_btn));
    active_btn.add_css_class("flat");
    away_btn.add_css_class("flat");
    presence_box.append(&active_btn);
    presence_box.append(&away_btn);
    popover_box.append(&presence_box);

    let sep1 = gtk::Separator::new(gtk::Orientation::Horizontal);
    popover_box.append(&sep1);

    // ── Status text + emoji ──
    let status_header = Label::new(Some("Update your status"));
    status_header.add_css_class("heading");
    status_header.set_halign(gtk::Align::Start);
    popover_box.append(&status_header);

    let emoji_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let emoji_entry = gtk::TextView::new();
    emoji_entry.set_width_request(100);
    emoji_entry.set_top_margin(6);
    emoji_entry.set_bottom_margin(6);
    emoji_entry.set_left_margin(6);
    emoji_entry.set_right_margin(6);
    emoji_entry.add_css_class("card");
    emoji_entry.set_accepts_tab(false);
    // Single-line: suppress Enter from inserting newlines
    {
        let key_ctl = gtk::EventControllerKey::new();
        key_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
        key_ctl.connect_key_pressed(|_, key, _, _| {
            if key == gtk4::gdk::Key::Return || key == gtk4::gdk::Key::KP_Enter {
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
        emoji_entry.add_controller(key_ctl);
    }
    let emoji_frame = gtk::Frame::new(None);
    emoji_frame.set_child(Some(&emoji_entry));
    emoji_row.append(&emoji_frame);

    // Attach emoji autocomplete (inline to avoid nested Wayland popups)
    let (_emoji_autocomplete, emoji_ac_widget) =
        crate::ui::autocomplete::Autocomplete::attach_inline(&emoji_entry);

    // Pre-fill ":" when the emoji field gains focus (if empty)
    {
        let focus_ctl = gtk::EventControllerFocus::new();
        let ee = emoji_entry.clone();
        focus_ctl.connect_enter(move |_| {
            let buf = ee.buffer();
            let (s, e) = buf.bounds();
            if buf.text(&s, &e, false).is_empty() {
                let t = ee.clone();
                gtk4::glib::idle_add_local_once(move || {
                    t.buffer().set_text(":");
                    let iter = t.buffer().end_iter();
                    t.buffer().place_cursor(&iter);
                });
            }
        });
        emoji_entry.add_controller(focus_ctl);
    }

    let status_entry = gtk::Entry::new();
    status_entry.set_placeholder_text(Some("What's your status?"));
    status_entry.set_hexpand(true);
    emoji_row.append(&status_entry);
    popover_box.append(&emoji_ac_widget);
    popover_box.append(&emoji_row);

    // ── Recent statuses ──
    let sep2 = gtk::Separator::new(gtk::Orientation::Horizontal);
    popover_box.append(&sep2);

    let recent_label = Label::new(Some("Recent"));
    recent_label.add_css_class("heading");
    recent_label.set_halign(gtk::Align::Start);
    recent_label.set_visible(false);
    popover_box.append(&recent_label);

    let recent_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    popover_box.append(&recent_box);

    let sep3 = gtk::Separator::new(gtk::Orientation::Horizontal);
    popover_box.append(&sep3);

    let status_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    status_buttons.set_halign(gtk::Align::End);

    let clear_btn = gtk::Button::with_label("Clear");
    let save_btn = gtk::Button::with_label("Save");
    save_btn.add_css_class("suggested-action");
    status_buttons.append(&clear_btn);
    status_buttons.append(&save_btn);
    popover_box.append(&status_buttons);

    popover.set_child(Some(&popover_box));

    // Focus the status text entry (not the emoji field) when the popover opens
    {
        let se = status_entry.clone();
        popover.connect_show(move |_| {
            let se = se.clone();
            gtk4::glib::idle_add_local_once(move || {
                se.grab_focus();
            });
        });
    }

    // Shared function to rebuild the recent statuses list
    let rebuild_recent: Rc<dyn Fn(Vec<RecentStatus>)> = {
        let recent_box = recent_box.clone();
        let recent_label = recent_label.clone();
        let emoji_entry = emoji_entry.clone();
        let status_entry = status_entry.clone();
        Rc::new(move |statuses: Vec<RecentStatus>| {
            // Clear
            while let Some(child) = recent_box.first_child() {
                recent_box.remove(&child);
            }
            recent_label.set_visible(!statuses.is_empty());

            for status in statuses {
                let btn = gtk::Button::new();
                btn.add_css_class("flat");
                btn.set_halign(gtk::Align::Fill);

                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.set_margin_start(4);
                row.set_margin_end(4);

                // Render emoji shortcode to unicode for display
                let emoji_display = replace_emoji_shortcodes(&status.emoji);
                let emoji_lbl = Label::new(Some(&emoji_display));
                row.append(&emoji_lbl);

                let text_lbl = Label::new(Some(&status.text));
                text_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                text_lbl.set_halign(gtk::Align::Start);
                row.append(&text_lbl);

                btn.set_child(Some(&row));

                let ee = emoji_entry.clone();
                let se = status_entry.clone();
                let s = status.clone();
                btn.connect_clicked(move |_| {
                    ee.buffer().set_text(&s.emoji);
                    se.set_text(&s.text);
                });

                recent_box.append(&btn);
            }
        })
    };

    // Profile button: avatar + status emoji
    let profile_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    profile_box.append(&profile_avatar);
    let self_status_icon = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    profile_box.append(&self_status_icon);

    let profile_btn = gtk::MenuButton::new();
    profile_btn.set_child(Some(&profile_box));
    profile_btn.set_popover(Some(&popover));
    profile_btn.add_css_class("flat");

    header_bar.pack_end(&profile_btn);

    // ── Layout ──
    // [ Sidebar | Messages+Input | ThreadPanel ]
    let main_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    let sidebar = Rc::new(ChannelSidebar::new());

    // Search entries packed into the start of the header bar
    window.set_titlebar(Some(&header_bar));
    let message_view = Rc::new(MessageView::new());
    let message_input = Rc::new(MessageInput::new());
    let thread_panel = Rc::new(ThreadPanel::new());

    let right_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    right_pane.set_hexpand(true);

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);

    right_pane.append(&message_view.widget);
    right_pane.append(&separator);
    right_pane.append(&message_input.widget);

    let vert_sep = gtk::Separator::new(gtk::Orientation::Vertical);

    main_box.append(&sidebar.widget);
    main_box.append(&vert_sep);
    main_box.append(&right_pane);
    main_box.append(&thread_panel.separator);
    main_box.append(&thread_panel.widget);

    // ── Toggle channel search from header button ──
    {
        let search_entry = sidebar.search_entry.clone();
        search_entry.set_visible(false);
        let btn = channel_search_btn.clone();
        btn.connect_toggled(move |btn| {
            let active = btn.is_active();
            search_entry.set_visible(active);
            if active {
                search_entry.grab_focus();
            } else {
                search_entry.set_text("");
            }
        });
    }

    window.set_child(Some(&main_box));

    let state = Rc::new(RefCell::new(AppState {
        channels: Vec::new(),
        user_names: Rc::new(HashMap::new()),
        subteam_names: Rc::new(HashMap::new()),
        self_user_id: user_id.clone(),
        current_channel: None,
        current_thread: None,
        unread_counts: HashMap::new(),
        pending_scroll: None,
        pending_thread: None,
        sent_ts: std::collections::HashSet::new(),
    }));

    // Flag to suppress presence API calls during initial load
    let presence_user_changed: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    // ── Load profile image, current status, presence, and recent statuses ──
    {
        let client = client.clone();
        let rt = rt.clone();
        let db = db.clone();
        let user_id = user_id.clone();
        let profile_avatar = profile_avatar.clone();
        let avatar_texture = avatar_texture.clone();
        let presence_active = presence_active.clone();
        let emoji_entry = emoji_entry.clone();
        let status_entry = status_entry.clone();
        let active_btn = active_btn.clone();
        let away_btn = away_btn.clone();
        let rebuild_recent = rebuild_recent.clone();
        let presence_user_changed = presence_user_changed.clone();
        let self_status_icon_init = self_status_icon.clone();
        gtk4::glib::spawn_future_local(async move {
            // Load recent statuses from DB
            {
                let db2 = db.clone();
                let rt2 = rt.clone();
                let recents = rt2
                    .spawn(async move { db2.load_recent_statuses().await })
                    .await
                    .unwrap();
                rebuild_recent(recents);
            }

            let uid = user_id.clone();
            let c = client.clone();
            let result = rt
                .spawn(async move {
                    let profile = c.get_user_profile(&uid).await;
                    let presence = c.get_presence(&uid).await;
                    (profile, presence)
                })
                .await
                .unwrap();

            let (profile_result, presence_result) = result;

            // Set presence state (without triggering API call)
            if let Ok(presence) = presence_result {
                let is_active = presence == "active";
                *presence_active.borrow_mut() = is_active;
                if is_active {
                    active_btn.set_active(true);
                } else {
                    away_btn.set_active(true);
                }
                profile_avatar.queue_draw();
            }
            // Now allow toggle handlers to call the API
            *presence_user_changed.borrow_mut() = true;

            if let Ok(profile) = profile_result {
                // Pre-fill current status in the popover
                if let Some(text) = profile.get("status_text").and_then(|v| v.as_str()) {
                    status_entry.set_text(text);
                }
                if let Some(emoji) = profile.get("status_emoji").and_then(|v| v.as_str()) {
                    emoji_entry.buffer().set_text(emoji);
                }
                // Show status emoji beside profile avatar
                update_self_status_icon(
                    &self_status_icon_init,
                    profile.get("status_emoji").and_then(|v| v.as_str()),
                );

                // Load profile image (prefer image_72)
                let image_url = profile
                    .get("image_72")
                    .or_else(|| profile.get("image_48"))
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if let Some(url) = image_url {
                    let c = client.clone();
                    let res = rt
                        .spawn(async move { c.fetch_image_bytes(&url).await })
                        .await
                        .unwrap();

                    if let Ok(bytes) = res {
                        let gbytes = gtk4::glib::Bytes::from_owned(bytes);
                        let stream = gtk4::gio::MemoryInputStream::from_bytes(&gbytes);
                        if let Ok(pixbuf) = gtk4::gdk_pixbuf::Pixbuf::from_stream(
                            &stream,
                            gtk4::gio::Cancellable::NONE,
                        ) {
                            let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                            *avatar_texture.borrow_mut() = Some(texture);
                            profile_avatar.queue_draw();
                        }
                    }
                }
            }
        });
    }

    // ── Presence toggle ──
    {
        let client_p = client.clone();
        let rt_p = rt.clone();
        let presence_active_ref = presence_active.clone();
        let avatar_ref = profile_avatar.clone();
        let user_changed = presence_user_changed.clone();
        active_btn.connect_toggled(move |btn| {
            if !btn.is_active() {
                return;
            }
            *presence_active_ref.borrow_mut() = true;
            avatar_ref.queue_draw();
            if !*user_changed.borrow() {
                return;
            }
            let c = client_p.clone();
            let rt = rt_p.clone();
            rt.spawn(async move {
                if let Err(e) = c.set_presence("auto").await {
                    tracing::error!("Failed to set presence: {e}");
                }
            });
        });

        let client_p = client.clone();
        let rt_p = rt.clone();
        let presence_active_ref = presence_active.clone();
        let avatar_ref = profile_avatar.clone();
        let user_changed = presence_user_changed.clone();
        away_btn.connect_toggled(move |btn| {
            if !btn.is_active() {
                return;
            }
            *presence_active_ref.borrow_mut() = false;
            avatar_ref.queue_draw();
            if !*user_changed.borrow() {
                return;
            }
            let c = client_p.clone();
            let rt = rt_p.clone();
            rt.spawn(async move {
                if let Err(e) = c.set_presence("away").await {
                    tracing::error!("Failed to set presence: {e}");
                }
            });
        });
    }

    // ── Periodic presence heartbeat: keep us "active" on Slack's server ──
    {
        let client_hb = client.clone();
        let rt_hb = rt.clone();
        let presence_active_hb = presence_active.clone();
        gtk4::glib::timeout_add_seconds_local(300, move || {
            if *presence_active_hb.borrow() {
                let c = client_hb.clone();
                rt_hb.spawn(async move {
                    if let Err(e) = c.set_presence("auto").await {
                        tracing::error!("Presence heartbeat failed: {e}");
                    }
                });
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    // ── Status save/clear buttons ──
    {
        let client_save = client.clone();
        let rt_save = rt.clone();
        let db_save = db.clone();
        let popover_save = popover.clone();
        let emoji_save = emoji_entry.clone();
        let status_save = status_entry.clone();
        let rebuild_save = rebuild_recent.clone();
        let self_status_save = self_status_icon.clone();
        save_btn.connect_clicked(move |_| {
            let text = status_save.text().to_string();
            let buf = emoji_save.buffer();
            let (s, e) = buf.bounds();
            let emoji = buf.text(&s, &e, false).to_string();
            let client = client_save.clone();
            let rt = rt_save.clone();
            let db = db_save.clone();
            let popover = popover_save.clone();
            let rebuild = rebuild_save.clone();
            let status_icon = self_status_save.clone();
            gtk4::glib::spawn_future_local(async move {
                let text2 = text.clone();
                let emoji2 = emoji.clone();
                let result = rt
                    .spawn(async move { client.set_user_status(&text2, &emoji2).await })
                    .await
                    .unwrap();
                if let Err(e) = result {
                    tracing::error!("Failed to set status: {e}");
                } else {
                    // Save to recent and rebuild the list
                    let status = RecentStatus {
                        emoji: emoji.clone(),
                        text: text.clone(),
                    };
                    let db2 = db.clone();
                    let s = status.clone();
                    let recents = rt
                        .spawn(async move {
                            db2.push_recent_status(&s).await;
                            db2.load_recent_statuses().await
                        })
                        .await
                        .unwrap();
                    rebuild(recents);
                    update_self_status_icon(&status_icon, Some(&emoji));
                }
                popover.popdown();
            });
        });

        let client_clear = client.clone();
        let rt_clear = rt.clone();
        let popover_clear = popover.clone();
        let emoji_clear = emoji_entry.clone();
        let status_clear = status_entry.clone();
        let self_status_clear = self_status_icon.clone();
        clear_btn.connect_clicked(move |_| {
            let client = client_clear.clone();
            let rt = rt_clear.clone();
            let popover = popover_clear.clone();
            let emoji_entry = emoji_clear.clone();
            let status_entry = status_clear.clone();
            let status_icon = self_status_clear.clone();
            gtk4::glib::spawn_future_local(async move {
                let result = rt
                    .spawn(async move { client.set_user_status("", "").await })
                    .await
                    .unwrap();
                if let Err(e) = result {
                    tracing::error!("Failed to clear status: {e}");
                }
                status_entry.set_text("");
                emoji_entry.buffer().set_text("");
                update_self_status_icon(&status_icon, None);
                popover.popdown();
            });
        });
    }

    // ── Thread panel: open callback ──
    {
        let thread_panel = thread_panel.clone();
        let state = state.clone();
        let client = client.clone();
        let rt = rt.clone();
        let db = db.clone();
        let message_view_tc = message_view.clone();
        let cb: crate::ui::message_view::ThreadOpenCallback =
            Rc::new(move |thread_ts: &str, channel_id: &str| {
                let tp = thread_panel.clone();
                let state = state.clone();
                let client = client.clone();
                let rt = rt.clone();
                let db = db.clone();
                let message_view = message_view_tc.clone();
                let thread_ts = thread_ts.to_string();
                let channel_id = channel_id.to_string();

                state.borrow_mut().current_thread =
                    Some((thread_ts.clone(), channel_id.clone()));

                tp.set_channel_id(&channel_id);
                tp.clear();
                tp.show();
                tp.text_view.grab_focus();

                let mv = message_view.clone();
                let db = db.clone();
                gtk4::glib::spawn_future_local(async move {
                    let c = client.clone();
                    let cid = channel_id.clone();
                    let tts = thread_ts.clone();
                    let tts2 = tts.clone();
                    let db2 = db.clone();
                    let result = rt
                        .spawn(async move {
                            let replies = c.conversation_replies(&cid, &tts).await;
                            if let Ok(ref messages) = replies {
                                db2.index_messages(&cid, messages).await;
                            }
                            replies
                        })
                        .await
                        .unwrap();

                    if let Ok(messages) = result {
                        // Update thread reply count on the button label
                        let count = if messages.is_empty() {
                            0
                        } else {
                            messages.len() - 1 // first message is the parent
                        };
                        mv.update_thread_count(&tts2, count);

                        let users = state.borrow().user_names.clone();
                        tp.set_messages(&messages, &users, &client, &rt);

                        // Scroll to pending reply if set by notification click
                        let pending = tp.pending_scroll.borrow_mut().take();
                        if let Some(ts) = pending {
                            tp.scroll_to_message(&ts);
                        }
                    }
                });
            });
        message_view.set_thread_callback(cb);
    }

    // ── Mention callback: open DM with user ──
    {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let mention_cb: crate::ui::message_view::MentionCallback =
            Rc::new(move |user_id: &str| {
                // Find the DM channel for this user (is_im with matching user field)
                let channel_id = {
                    let st = state.borrow();
                    st.channels
                        .iter()
                        .find(|ch| {
                            ch.is_im == Some(true)
                                && ch.user.as_deref() == Some(user_id)
                        })
                        .map(|ch| ch.id.clone())
                };

                if let Some(cid) = channel_id {
                    sidebar.select_channel_by_id(&cid);
                } else {
                    tracing::warn!("No DM channel found for user {user_id}");
                }
            });
        message_view.set_mention_callback(mention_cb.clone());
        thread_panel.set_mention_callback(mention_cb);
    }

    // ── Reaction callback: add/remove reaction via API ──
    {
        let client = client.clone();
        let rt = rt.clone();
        let window_ref = window.clone();
        let reaction_cb: crate::ui::message_view::ReactionCallback =
            Rc::new(move |channel_id: &str, ts: &str, name: &str, btn: &gtk::Button| {
                let c = client.clone();
                let c2 = client.clone();
                let cid = channel_id.to_string();
                let ts = ts.to_string();
                let name = name.to_string();
                let rt2 = rt.clone();
                let rt3 = rt.clone();
                let btn = btn.clone();
                let win = window_ref.clone();

                // Resolve emoji for display
                let emoji_unicode = emojis::get_by_shortcode(&name)
                    .map(|e| e.as_str().to_string())
                    .unwrap_or_else(|| format!(":{name}:"));

                gtk4::glib::spawn_future_local(async move {
                    let cid_add = cid.clone();
                    let ts_add = ts.clone();
                    let name_add = name.clone();
                    let result = rt2
                        .spawn(async move {
                            c.add_reaction(&cid_add, &ts_add, &name_add).await
                        })
                        .await
                        .unwrap();

                    match result {
                        Ok(()) => {
                            // Success — increment count on the button if it's a reaction btn
                            let current_label = btn.label().map(|l| l.to_string());
                            if let Some(label) = current_label {
                                // Existing reaction button: "👍 3" → "👍 4"
                                if let Some(count_str) =
                                    label.split_whitespace().last()
                                {
                                    if let Ok(count) = count_str.parse::<u32>() {
                                        btn.set_label(&format!(
                                            "{} {}",
                                            emoji_unicode,
                                            count + 1
                                        ));
                                    }
                                }
                            }
                        }
                        Err(e) if e.contains("already_reacted") => {
                            // Show dialog asking to remove
                            let dialog = gtk::AlertDialog::builder()
                                .modal(true)
                                .message("Remove reaction?")
                                .detail(&format!(
                                    "You already reacted with {emoji_unicode}. Remove it?"
                                ))
                                .buttons(["Cancel", "Remove"])
                                .cancel_button(0)
                                .default_button(1)
                                .build();

                            let response = dialog.choose_future(Some(&win)).await;
                            if response == Ok(1) {
                                let cid_rm = cid.clone();
                                let ts_rm = ts.clone();
                                let name_rm = name.clone();
                                let rm_result = rt3
                                    .spawn(async move {
                                        c2.remove_reaction(&cid_rm, &ts_rm, &name_rm)
                                            .await
                                    })
                                    .await
                                    .unwrap();

                                match rm_result {
                                    Ok(()) => {
                                        // Decrement count or hide button
                                        let current_label =
                                            btn.label().map(|l| l.to_string());
                                        if let Some(label) = current_label {
                                            if let Some(count_str) =
                                                label.split_whitespace().last()
                                            {
                                                if let Ok(count) =
                                                    count_str.parse::<u32>()
                                                {
                                                    if count <= 1 {
                                                        btn.set_visible(false);
                                                    } else {
                                                        btn.set_label(&format!(
                                                            "{} {}",
                                                            emoji_unicode,
                                                            count - 1
                                                        ));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to remove reaction: {e}"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to add reaction: {e}");
                        }
                    }
                });
            });
        message_view.set_reaction_callback(reaction_cb.clone());
        thread_panel.set_reaction_callback(reaction_cb);
    }

    // ── Channel action callback (leave, archive, close) ──
    {
        use crate::ui::channel_sidebar::ChannelAction;
        let client = client.clone();
        let rt = rt.clone();
        let db = db.clone();
        let state = state.clone();
        let sidebar_action = sidebar.clone();
        let message_view = message_view.clone();
        let window = window.clone();
        let action_cb: crate::ui::channel_sidebar::ChannelActionCallback =
            Rc::new(move |action, channel_id| {
                // Presence watch/unwatch — no confirmation needed
                match &action {
                    ChannelAction::WatchPresence(uid) => {
                        let db2 = db.clone();
                        let uid = uid.clone();
                        rt.spawn(async move { db2.add_presence_watch(&uid).await });
                        return;
                    }
                    ChannelAction::UnwatchPresence(uid) => {
                        let db2 = db.clone();
                        let uid = uid.clone();
                        rt.spawn(async move { db2.remove_presence_watch(&uid).await });
                        return;
                    }
                    _ => {}
                }

                let (title, detail) = match action {
                    ChannelAction::Leave => (
                        "Leave channel?",
                        "You can rejoin later if needed.",
                    ),
                    ChannelAction::Archive => (
                        "Archive channel?",
                        "This will archive the channel for all members. This can be undone by a workspace admin.",
                    ),
                    ChannelAction::Close => (
                        "Close conversation?",
                        "You can reopen it later.",
                    ),
                    ChannelAction::WatchPresence(_) | ChannelAction::UnwatchPresence(_) => unreachable!(),
                };

                let dialog = gtk::AlertDialog::builder()
                    .message(title)
                    .detail(detail)
                    .buttons(["Cancel", "Confirm"])
                    .cancel_button(0)
                    .default_button(1)
                    .build();

                let c = client.clone();
                let rt2 = rt.clone();
                let db2 = db.clone();
                let state2 = state.clone();
                let sidebar2 = sidebar_action.clone();
                let mv = message_view.clone();
                let cid = channel_id.to_string();
                let action = action;
                let window = window.clone();

                dialog.choose(
                    Some(&window),
                    gtk4::gio::Cancellable::NONE,
                    move |result| {
                        if result != Ok(1) { return; }

                        // Clear current channel if it's the one being acted on
                        {
                            let mut st = state2.borrow_mut();
                            if st.current_channel.as_deref() == Some(&cid) {
                                st.current_channel = None;
                                st.current_thread = None;
                            }
                            // Remove from channel list
                            st.channels.retain(|c| c.id != cid);
                        }

                        // Remove from sidebar
                        for list in [&sidebar2.channels_list, &sidebar2.dm_list, &sidebar2.group_list] {
                            let mut idx = 0;
                            while let Some(row) = list.row_at_index(idx) {
                                if row.widget_name() == cid {
                                    list.remove(&row);
                                    break;
                                }
                                idx += 1;
                            }
                        }

                        // Clear message view if showing this channel
                        mv.clear();
                        mv.set_channel_name("Select a channel");

                        // Call API
                        let c = c.clone();
                        let cid2 = cid.clone();
                        rt2.spawn(async move {
                            let result = match action {
                                ChannelAction::Leave => c.leave_channel(&cid2).await,
                                ChannelAction::Archive => c.archive_channel(&cid2).await,
                                ChannelAction::Close => c.close_conversation(&cid2).await,
                                ChannelAction::WatchPresence(_) | ChannelAction::UnwatchPresence(_) => return,
                            };
                            if let Err(e) = result {
                                tracing::error!("Channel action failed: {e}");
                            }
                        });

                        // Delete channel data and update cached channels
                        let db2 = db2.clone();
                        let cid3 = cid.clone();
                        let channels = state2.borrow().channels.clone();
                        rt2.spawn(async move {
                            db2.delete_channel_data(&cid3).await;
                            let _ = db2.save_channels(&channels).await;
                        });
                    },
                );
            });
        sidebar.set_action_callback(action_cb);
    }

    // ── Delete message callback ──
    {
        message_view.set_self_user_id(&user_id);
        let client = client.clone();
        let rt = rt.clone();
        let db = db.clone();
        let window = window.clone();
        let delete_cb: crate::ui::message_view::DeleteCallback =
            Rc::new(move |channel_id, message_ts, row| {
                let dialog = gtk::AlertDialog::builder()
                    .message("Delete message?")
                    .detail("This can't be undone.")
                    .buttons(["Cancel", "Delete"])
                    .cancel_button(0)
                    .default_button(1)
                    .build();

                let c = client.clone();
                let rt2 = rt.clone();
                let db2 = db.clone();
                let cid = channel_id.to_string();
                let ts = message_ts.to_string();
                let row = row.clone();
                let list_box = row.parent()
                    .and_then(|p| p.downcast::<gtk::ListBox>().ok());
                let window = window.clone();
                dialog.choose(
                    Some(&window),
                    gtk4::gio::Cancellable::NONE,
                    move |result| {
                        if result == Ok(1) {
                            // Remove row from UI immediately
                            if let Some(list) = &list_box {
                                list.remove(&row);
                            }
                            // Delete via API and remove from index
                            let c = c.clone();
                            let db2 = db2.clone();
                            let cid = cid.clone();
                            let ts = ts.clone();
                            rt2.spawn(async move {
                                if let Err(e) = c.delete_message(&cid, &ts).await {
                                    tracing::error!("Failed to delete message: {e}");
                                }
                                db2.delete_indexed_message(&cid, &ts).await;
                            });
                        }
                    },
                );
            });
        message_view.set_delete_callback(delete_cb.clone());
        thread_panel.set_delete_callback(delete_cb);
        thread_panel.set_self_user_id(&user_id);
    }

    // ── Edit message callback ──
    {
        let client = client.clone();
        let rt = rt.clone();
        let db = db.clone();
        let window = window.clone();
        let state = state.clone();
        let edit_cb: crate::ui::message_view::EditCallback =
            Rc::new(move |channel_id, message_ts, current_text, outer_box| {
                let dialog = gtk::Window::builder()
                    .title("Edit message")
                    .modal(true)
                    .transient_for(&window)
                    .default_width(500)
                    .default_height(200)
                    .build();

                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
                vbox.set_margin_top(12);
                vbox.set_margin_bottom(12);
                vbox.set_margin_start(12);
                vbox.set_margin_end(12);

                let text_view = gtk::TextView::new();
                text_view.set_hexpand(true);
                text_view.set_vexpand(true);
                text_view.set_wrap_mode(gtk::WrapMode::WordChar);
                text_view.set_top_margin(6);
                text_view.set_bottom_margin(6);
                text_view.set_left_margin(6);
                text_view.set_right_margin(6);
                text_view.add_css_class("card");
                text_view.buffer().set_text(current_text);

                let scrolled = gtk::ScrolledWindow::new();
                scrolled.set_vexpand(true);
                scrolled.set_child(Some(&text_view));
                vbox.append(&scrolled);

                let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                btn_box.set_halign(gtk::Align::End);

                let cancel_btn = gtk::Button::with_label("Cancel");
                cancel_btn.add_css_class("flat");
                let save_btn = gtk::Button::with_label("Save");
                save_btn.add_css_class("suggested-action");

                btn_box.append(&cancel_btn);
                btn_box.append(&save_btn);
                vbox.append(&btn_box);

                dialog.set_child(Some(&vbox));

                let dialog_weak = dialog.downgrade();
                cancel_btn.connect_clicked(move |_| {
                    if let Some(d) = dialog_weak.upgrade() {
                        d.close();
                    }
                });

                let c = client.clone();
                let rt2 = rt.clone();
                let db2 = db.clone();
                let cid = channel_id.to_string();
                let ts = message_ts.to_string();
                let outer = outer_box.clone();
                let state2 = state.clone();
                let dialog_weak = dialog.downgrade();
                save_btn.connect_clicked(move |btn| {
                    let buf = text_view.buffer();
                    let new_text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
                    if new_text.is_empty() {
                        return;
                    }
                    btn.set_sensitive(false);
                    let c = c.clone();
                    let db2 = db2.clone();
                    let cid = cid.clone();
                    let ts = ts.clone();
                    let new_text2 = new_text.clone();
                    let outer = outer.clone();
                    let state2 = state2.clone();
                    let dialog_weak = dialog_weak.clone();
                    let rt3 = rt2.clone();
                    gtk4::glib::spawn_future_local(async move {
                        let c2 = c.clone();
                        let db3 = db2.clone();
                        let cid2 = cid.clone();
                        let ts2 = ts.clone();
                        let text = new_text2.clone();
                        let result = rt3.spawn(async move {
                            let r = c2.update_message(&cid2, &ts2, &text).await;
                            if r.is_ok() {
                                db3.update_indexed_message_text(&cid2, &ts2, &text).await;
                            }
                            r
                        }).await;

                        match result {
                            Ok(Ok(())) => {
                                if let Some(d) = dialog_weak.upgrade() {
                                    d.close();
                                }
                                // Replace the body widget in the outer box
                                let st = state2.borrow();
                                let users = &st.user_names;
                                let subteam_names = &st.subteam_names;
                                if let Some(header) = outer.first_child() {
                                    if let Some(old_body) = header.next_sibling() {
                                        if old_body.downcast_ref::<gtk::Label>().is_some()
                                            || old_body.downcast_ref::<gtk::TextView>().is_some()
                                        {
                                            let new_body = crate::ui::message_view::make_message_body(
                                                &new_text2, users, subteam_names, &None,
                                            );
                                            outer.insert_child_after(&new_body, Some(&header));
                                            outer.remove(&old_body);
                                        }
                                    }
                                }
                            }
                            Ok(Err(e)) => {
                                tracing::error!("Failed to edit message: {e}");
                                if let Some(d) = dialog_weak.upgrade() {
                                    d.close();
                                }
                            }
                            Err(e) => {
                                tracing::error!("Edit task panicked: {e}");
                                if let Some(d) = dialog_weak.upgrade() {
                                    d.close();
                                }
                            }
                        }
                    });
                });

                dialog.present();
            });
        message_view.set_edit_callback(edit_cb.clone());
        thread_panel.set_edit_callback(edit_cb);
    }

    // ── Google Meet call button ──
    {
        let state = state.clone();
        let client = client.clone();
        let rt = rt.clone();
        message_view.call_button.connect_clicked(move |_| {
            let channel = match state.borrow().current_channel.clone() {
                Some(c) => c,
                None => return,
            };
            let client = client.clone();
            let rt2 = rt.clone();
            gtk4::glib::spawn_future_local(async move {
                let c = client.clone();
                let ch = channel.clone();
                let result = rt2
                    .spawn(async move { c.calls_request(&ch).await })
                    .await
                    .unwrap();
                match result {
                    Ok(url) => {
                        if let Err(e) = gtk4::gio::AppInfo::launch_default_for_uri(
                            &url,
                            gtk4::gio::AppLaunchContext::NONE,
                        ) {
                            tracing::error!("Failed to open Meet URL: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to request call: {e}");
                    }
                }
            });
        });
    }

    // ── Infinite scroll: load older messages (top) and newer messages (bottom) ──
    {
        let mv = message_view.clone();
        let state = state.clone();
        let client = client.clone();
        let rt = rt.clone();
        message_view.set_load_more_callback(Rc::new(move |channel_id: &str, oldest_ts: &str| {
            let mv = mv.clone();
            let state = state.clone();
            let client = client.clone();
            let rt = rt.clone();
            let cid = channel_id.to_string();
            let ts = oldest_ts.to_string();
            gtk4::glib::spawn_future_local(async move {
                let c = client.clone();
                let ch = cid.clone();
                let t = ts.clone();
                let result = rt
                    .spawn(async move { c.conversation_history_before(&ch, &t, 25).await })
                    .await;
                match result {
                    Ok(Ok(messages)) => {
                        let users = state.borrow().user_names.clone();
                        mv.prepend_messages(&messages, &users, &client, &rt, 25);
                    }
                    Ok(Err(e)) => {
                        tracing::error!("Failed to load older messages: {e}");
                        mv.reset_loading_more();
                    }
                    Err(e) => {
                        tracing::error!("Load more task error: {e}");
                        mv.reset_loading_more();
                    }
                }
            });
        }));
    }
    {
        let mv = message_view.clone();
        let state = state.clone();
        let db = db.clone();
        let rt = rt.clone();
        let client = client.clone();
        message_view.set_load_newer_callback(Rc::new(move |channel_id: &str, newest_ts: &str| {
            let mv = mv.clone();
            let state = state.clone();
            let db = db.clone();
            let rt = rt.clone();
            let client = client.clone();
            let cid = channel_id.to_string();
            let ts = newest_ts.to_string();
            gtk4::glib::spawn_future_local(async move {
                let db2 = db.clone();
                let cid2 = cid.clone();
                let ts2 = ts.clone();
                let result = rt
                    .spawn(async move { db2.load_messages_after(&cid2, &ts2, 25).await })
                    .await;
                match result {
                    Ok(messages) => {
                        let users = state.borrow().user_names.clone();
                        mv.append_newer_messages(&messages, &users, &client, &rt, 25);
                    }
                    Err(e) => {
                        tracing::error!("Load newer task error: {e}");
                        mv.reset_loading_more();
                    }
                }
            });
        }));
        message_view.connect_edge_loading();
    }

    // ── Emoji pick persistence: record to DB when emoji is selected via autocomplete ──
    {
        let db_ep = db.clone();
        let rt_ep = rt.clone();
        crate::ui::autocomplete::set_emoji_persist_callback(move |shortcode: &str| {
            let db = db_ep.clone();
            let sc = shortcode.to_string();
            rt_ep.spawn(async move { db.push_recent_emoji(&sc).await });
        });

        let on_pick: Rc<dyn Fn(&str)> = Rc::new(|shortcode: &str| {
            crate::ui::autocomplete::record_emoji_used(shortcode);
        });
        message_input.set_on_emoji_picked(on_pick.clone());
        thread_panel.set_on_emoji_picked(on_pick);
    }

    // ── Thread panel: close button ──
    {
        let tp_close = thread_panel.clone();
        let state = state.clone();
        thread_panel.close_button.connect_clicked(move |_| {
            tp_close.hide();
            state.borrow_mut().current_thread = None;
        });
    }

    // ── Thread panel: send reply ──
    {
        let tp_reply = thread_panel.clone();
        let state = state.clone();
        let client = client.clone();
        let rt = rt.clone();

        let do_reply = move || {
            let text = tp_reply.get_reply_text();
            let files = tp_reply.take_files();
            if text.trim().is_empty() && files.is_empty() {
                return;
            }

            let (thread_ts, channel_id) = match state.borrow().current_thread.clone() {
                Some(t) => t,
                None => return,
            };

            tp_reply.clear_reply();

            let client = client.clone();
            let rt2 = rt.clone();
            let tp = tp_reply.clone();
            let state2 = state.clone();
            let client2 = client.clone();
            let rt3 = rt.clone();
            gtk4::glib::spawn_future_local(async move {
                let c = client.clone();
                let cid = channel_id.clone();
                let tts = thread_ts.clone();
                let txt = text.clone();
                let sent_ts = rt2
                    .spawn(async move {
                        // Upload files first
                        for path in &files {
                            let filename = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "file".into());
                            match tokio::fs::read(path).await {
                                Ok(bytes) => {
                                    let comment = if path == files.first().unwrap() && !txt.trim().is_empty() {
                                        Some(txt.as_str())
                                    } else {
                                        None
                                    };
                                    if let Err(e) = c.upload_file(&cid, bytes, &filename, comment, Some(&tts)).await {
                                        tracing::error!("Failed to upload file {filename}: {e}");
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to read file {}: {e}", path.display());
                                }
                            }
                        }
                        // If no files, just send a text message and return the ts
                        if files.is_empty() && !txt.trim().is_empty() {
                            match c.post_message(&cid, &txt, Some(&tts)).await {
                                Ok(ts) => Some(ts),
                                Err(e) => {
                                    tracing::error!("Failed to send thread reply: {e}");
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    })
                    .await
                    .unwrap_or(None);

                // Optimistically append the sent message so it's visible immediately
                if let Some(ts) = sent_ts {
                    state2.borrow_mut().sent_ts.insert(ts.clone());
                    if !list_has_ts(&tp.list_box, &ts) {
                        let st = state2.borrow();
                        let self_uid = st.self_user_id.clone();
                        let users = st.user_names.clone();
                        drop(st);
                        let msg = Message {
                            msg_type: "message".into(),
                            user: Some(self_uid),
                            bot_id: None,
                            text,
                            ts,
                            thread_ts: Some(thread_ts),
                            channel: Some(channel_id),
                            attachments: None,
                            reactions: None,
                            files: None,
                        };
                        tp.append_message(&msg, &users, &client2, &rt3);
                    }
                }
            });
        };

        let do_reply_btn = do_reply.clone();
        let tp_btn = thread_panel.clone();
        tp_btn.send_button.send.connect_clicked(move |_| {
            do_reply_btn();
        });

        let key_controller = gtk::EventControllerKey::new();
        let do_reply_key = do_reply.clone();
        let tv = thread_panel.text_view.clone();
        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            if key == gtk4::gdk::Key::Return || key == gtk4::gdk::Key::KP_Enter {
                if modifier.contains(gtk4::gdk::ModifierType::SHIFT_MASK)
                    || modifier.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
                {
                    tv.buffer().insert_at_cursor("\n");
                    return gtk4::glib::Propagation::Stop;
                }
                do_reply_key();
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        thread_panel.text_view.add_controller(key_controller);
    }

    // ── Load initial data from cache, then select last channel ──
    {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let rt = rt.clone();
        let client = client.clone();
        let db = db.clone();
        let message_view = message_view.clone();
        let message_input = message_input.clone();
        let thread_panel = thread_panel.clone();
        let presence_tx = presence_tx.clone();
        let app_logout = app.clone();
        let window_logout = window.clone();
        gtk4::glib::spawn_future_local(async move {
            // Load cached data in parallel: channels, users, last channel, activity, watched, emoji
            let (cached_channels, cached_users, last_channel, activity, watched_users, cached_emoji, recent_emoji) = {
                let db_ch = db.clone();
                let db_us = db.clone();
                let db_lc = db.clone();
                let db_act = db.clone();
                let db_pw = db.clone();
                let db_em = db.clone();
                let db_re = db.clone();
                let db_meta = db.clone();
                let rt2 = rt.clone();
                rt2.spawn(async move {
                    let ch = db_ch.load_channels().await;
                    let us = db_us.load_users().await;
                    let lc = db_lc.load_last_channel().await;
                    let mut act = db_act.load_all_channel_activity().await;
                    let pw = db_pw.load_presence_watches().await;
                    let em = db_em.load_custom_emoji().await;
                    let re = db_re.load_recent_emoji().await;
                    // Merge channel meta newest_ts into activity (use whichever is newer)
                    let meta = db_meta.load_all_channel_meta().await;
                    for (cid, (_oldest, newest, _checked)) in &meta {
                        let entry = act.entry(cid.clone()).or_default();
                        if newest.as_str() > entry.as_str() {
                            *entry = newest.clone();
                        }
                    }
                    (ch, us, lc, act, pw, em, re)
                })
                .await
                .unwrap()
            };

            // Override last_channel / set pending state from CLI startup action
            let (last_channel, startup_search) = match startup_action {
                Some(StartupAction::OpenChannel(cid)) => (Some(cid), None),
                Some(StartupAction::OpenMessage { channel_id, message_ts }) => {
                    state.borrow_mut().pending_scroll = Some(message_ts);
                    (Some(channel_id), None)
                }
                Some(StartupAction::Search(query)) => (last_channel, Some(query)),
                None => (last_channel, None),
            };

            // Apply cached custom emoji BEFORE rendering any messages
            if let Some(ref emoji) = cached_emoji {
                let paths = build_emoji_path_map(emoji);
                crate::slack::helpers::set_custom_emoji(paths);
            }

            // Apply recent emoji for autocomplete sorting
            crate::slack::helpers::set_recent_emoji(recent_emoji);

            // Apply activity data
            sidebar.set_activity(activity);

            // Apply watched presence users
            sidebar.set_watched_users(watched_users.into_iter().collect());

            // Apply cached users
            if let Some(users) = cached_users {
                let mut names = HashMap::new();
                let mut status_emoji_map = HashMap::new();
                let mut status_text_map = HashMap::new();
                for u in &users {
                    names.insert(u.id.clone(), user_display_name(u));
                    if let Some(emoji) = u.profile.as_ref()
                        .and_then(|p| p.status_emoji.as_deref())
                        .filter(|e| !e.is_empty())
                    {
                        status_emoji_map.insert(u.id.clone(), emoji.to_string());
                    }
                    if let Some(text) = u.profile.as_ref()
                        .and_then(|p| p.status_text.as_deref())
                        .filter(|t| !t.is_empty())
                    {
                        status_text_map.insert(u.id.clone(), text.to_string());
                    }
                }
                sidebar.set_user_names(&names);
                sidebar.set_all_status(status_emoji_map, status_text_map);
                message_input.set_mention_users(&names);
                thread_panel.set_mention_users(&names);
                state.borrow_mut().user_names = Rc::new(names);
            }

            // Subscribe to presence for DM user IDs
            let subscribe_presence = |channels: &[Channel], tx: &mpsc::UnboundedSender<Vec<String>>| {
                let dm_user_ids: Vec<String> = channels
                    .iter()
                    .filter(|c| c.is_im == Some(true))
                    .filter_map(|c| c.user.clone())
                    .collect();
                if !dm_user_ids.is_empty() {
                    let _ = tx.send(dm_user_ids);
                }
            };

            // Apply cached channels and restore last selection
            let have_cache = if let Some(channels) = cached_channels {
                let sorted = sort_and_filter_channels(channels);
                if !sorted.is_empty() {
                    sidebar.set_channels(&sorted);
                    subscribe_presence(&sorted, &presence_tx);
                    state.borrow_mut().channels = sorted;
                    if let Some(ref cid) = last_channel {
                        sidebar.select_channel_by_id(cid);
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };

            // If no cache, fetch fresh data now (first run)
            if !have_cache || state.borrow().user_names.is_empty() {
                if !have_cache {
                    message_view.spinner.set_visible(true);
                    message_view.spinner.start();
                }

                let fetch_channels = !have_cache;
                let fetch_users = state.borrow().user_names.is_empty();

                let client2 = client.clone();
                let rt2 = rt.clone();
                let fetch_subteams = state.borrow().subteam_names.is_empty();
                let (ch_result, usr_result, subteam_result) = rt2
                    .spawn(async move {
                        let ch = if fetch_channels {
                            Some(client2.conversations_list_all().await)
                        } else {
                            None
                        };
                        let us = if fetch_users {
                            Some(client2.users_list_all().await)
                        } else {
                            None
                        };
                        let st: Option<Result<HashMap<String, String>, String>> = if fetch_subteams {
                            Some(client2.usergroups_list().await)
                        } else {
                            None
                        };
                        (ch, us, st)
                    })
                    .await
                    .unwrap();

                if let Some(Ok(channels)) = ch_result {
                    let sorted = sort_and_filter_channels(channels);

                    let db2 = db.clone();
                    let to_save = sorted.clone();
                    let rt2 = rt.clone();
                    rt2.spawn(async move {
                        if let Err(e) = db2.save_channels(&to_save).await {
                            tracing::error!("Failed to cache channels: {e}");
                        }
                    });

                    sidebar.set_channels(&sorted);
                    subscribe_presence(&sorted, &presence_tx);
                    state.borrow_mut().channels = sorted;
                    message_view.spinner.stop();
                    message_view.spinner.set_visible(false);

                    if let Some(ref cid) = last_channel {
                        sidebar.select_channel_by_id(cid);
                    }
                } else if let Some(Err(e)) = ch_result {
                    tracing::error!("Failed to load channels: {e}");
                    message_view.spinner.stop();
                    message_view.spinner.set_visible(false);
                    if is_fatal_auth_error(&e) {
                        tracing::warn!("Fatal auth error, logging out");
                        let db2 = db.clone();
                        let rt2 = rt.clone();
                        rt2.spawn(async move { db2.clear_credentials().await });
                        window_logout.close();
                        crate::ui::login::show_login(&app_logout, rt, db);
                        return;
                    }
                }

                if let Some(Ok(users)) = usr_result {
                    let to_cache = users.clone();
                    let db2 = db.clone();
                    let rt2 = rt.clone();
                    rt2.spawn(async move {
                        if let Err(e) = db2.save_users(&to_cache).await {
                            tracing::error!("Failed to cache users: {e}");
                        }
                    });

                    let mut names = HashMap::new();
                    let mut status_emoji_map = HashMap::new();
                    let mut status_text_map = HashMap::new();
                    for u in &users {
                        names.insert(u.id.clone(), user_display_name(u));
                        if let Some(emoji) = u.profile.as_ref()
                            .and_then(|p| p.status_emoji.as_deref())
                            .filter(|e| !e.is_empty())
                        {
                            status_emoji_map.insert(u.id.clone(), emoji.to_string());
                        }
                        if let Some(text) = u.profile.as_ref()
                            .and_then(|p| p.status_text.as_deref())
                            .filter(|t| !t.is_empty())
                        {
                            status_text_map.insert(u.id.clone(), text.to_string());
                        }
                    }
                    sidebar.set_user_names(&names);
                    sidebar.set_all_status(status_emoji_map, status_text_map);
                    message_input.set_mention_users(&names);
                    thread_panel.set_mention_users(&names);
                    state.borrow_mut().user_names = Rc::new(names);

                    // Rebuild sidebar now that user names are available for DMs
                    if have_cache {
                        let chs = state.borrow().channels.clone();
                        sidebar.set_channels(&chs);
                        if let Some(ref cid) = last_channel {
                            sidebar.select_channel_by_id(cid);
                        }
                    }
                } else if let Some(Err(e)) = usr_result {
                    tracing::error!("Failed to load users: {e}");
                    if is_fatal_auth_error(&e) {
                        tracing::warn!("Fatal auth error, logging out");
                        let db2 = db.clone();
                        let rt2 = rt.clone();
                        rt2.spawn(async move { db2.clear_credentials().await });
                        window_logout.close();
                        crate::ui::login::show_login(&app_logout, rt, db);
                        return;
                    }
                }

                if let Some(Ok(subteams)) = subteam_result {
                    tracing::info!("Loaded {} subteam names", subteams.len());
                    let names = Rc::new(subteams);
                    message_view.set_subteam_names(names.clone());
                    thread_panel.set_subteam_names(names.clone());
                    state.borrow_mut().subteam_names = names;
                } else if let Some(Err(e)) = subteam_result {
                    tracing::error!("Failed to load usergroups: {e}");
                }
            }

            // ── Custom emoji: refresh from API in background ──
            {
                let db2 = db.clone();
                let rt2 = rt.clone();
                let client2 = client.clone();
                rt2.spawn(async move {
                    match client2.emoji_list().await {
                        Ok(emoji) => {
                            let paths = download_custom_emoji(&client2, &emoji).await;
                            crate::slack::helpers::set_custom_emoji(paths);
                            if let Err(e) = db2.save_custom_emoji(&emoji).await {
                                tracing::error!("Failed to cache custom emoji: {e}");
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to fetch custom emoji: {e}");
                        }
                    }
                });
            }

            // ── Execute startup search if requested via --search CLI flag ──
            if let Some(query) = startup_search {
                let db = db.clone();
                let rt = rt.clone();
                let state = state.clone();
                let mv = message_view.clone();
                let client = client.clone();
                let q = query.clone();
                let results = rt
                    .spawn(async move { db.search_messages(&q).await })
                    .await
                    .unwrap();
                let st = state.borrow();
                let users = st.user_names.clone();
                let channels = st.channels.clone();
                drop(st);
                mv.set_search_results(&results, &users, &channels, &client, &rt);
            }

            // ── Background backfill: fetch up to 1 year of history for all channels ──
            {
                let channel_ids: Vec<String> = state.borrow().channels.iter().map(|c| c.id.clone()).collect();
                let client = client.clone();
                let db = db.clone();
                let rt = rt.clone();
                rt.spawn(async move {
                    backfill_history(client, db, channel_ids).await;
                });
            }

            // ── Periodic user list refresh (every 30 minutes) ──
            {
                let state = state.clone();
                let client = client.clone();
                let db = db.clone();
                let rt = rt.clone();
                let sidebar = sidebar.clone();
                let message_input = message_input.clone();
                let thread_panel = thread_panel.clone();
                gtk4::glib::spawn_future_local(async move {
                    loop {
                        gtk4::glib::timeout_future(std::time::Duration::from_secs(30 * 60)).await;
                        tracing::info!("Periodic user list refresh...");
                        let client2 = client.clone();
                        let rt2 = rt.clone();
                        let result = rt2
                            .spawn(async move { client2.users_list_all().await })
                            .await;
                        let Ok(Ok(users)) = result else {
                            tracing::error!("Periodic user refresh failed");
                            continue;
                        };
                        let db2 = db.clone();
                        let rt2 = rt.clone();
                        let to_cache = users.clone();
                        rt2.spawn(async move {
                            if let Err(e) = db2.save_users(&to_cache).await {
                                tracing::error!("Failed to cache users: {e}");
                            }
                        });
                        let mut names = HashMap::new();
                        let mut status_emoji_map = HashMap::new();
                        let mut status_text_map = HashMap::new();
                        for u in &users {
                            names.insert(u.id.clone(), user_display_name(&u));
                            if let Some(emoji) = u.profile.as_ref()
                                .and_then(|p| p.status_emoji.as_deref())
                                .filter(|e| !e.is_empty())
                            {
                                status_emoji_map.insert(u.id.clone(), emoji.to_string());
                            }
                            if let Some(text) = u.profile.as_ref()
                                .and_then(|p| p.status_text.as_deref())
                                .filter(|t| !t.is_empty())
                            {
                                status_text_map.insert(u.id.clone(), text.to_string());
                            }
                        }
                        sidebar.set_user_names(&names);
                        sidebar.set_all_status(status_emoji_map, status_text_map);
                        message_input.set_mention_users(&names);
                        thread_panel.set_mention_users(&names);
                        state.borrow_mut().user_names = Rc::new(names);
                        tracing::info!("Periodic user refresh complete: {} users", users.len());
                    }
                });
            }
        });
    }

    // ── Channel / DM selection (shared handler for both lists) ──
    {
        let on_select = {
            let state = state.clone();
            let message_view = message_view.clone();
            let thread_panel = thread_panel.clone();
            let sidebar = sidebar.clone();
            let rt = rt.clone();
            let client = client.clone();
            let db = db.clone();
            let message_input = message_input.clone();
            let search_entry = sidebar.search_entry.clone();
            let window = window.clone();
            Rc::new(move |row: &gtk::ListBoxRow| {
                let channel_id = row.widget_name().to_string();
                if channel_id.is_empty() {
                    return;
                }

                // Clear search and focus the message input box
                search_entry.set_text("");
                let input_focus = message_input.text_view.clone();
                gtk4::glib::idle_add_local_once(move || {
                    input_focus.grab_focus();
                });

                // Persist last selected channel
                let db2 = db.clone();
                let cid = channel_id.clone();
                let rt2 = rt.clone();
                rt2.spawn(async move { db2.save_last_channel(&cid).await });

                // Close thread panel when switching channels
                thread_panel.hide();
                {
                    let mut st = state.borrow_mut();
                    st.current_channel = Some(channel_id.clone());
                    st.current_thread = None;
                    st.unread_counts.remove(&channel_id);
                }
                sidebar.set_unread(&channel_id, 0);
                message_view.set_channel_id(&channel_id);

                // Set header name
                {
                    let st = state.borrow();
                    if let Some(ch) = st.channels.iter().find(|c| c.id == channel_id) {
                        let name = if ch.is_im == Some(true) {
                            st.user_names
                                .get(ch.user.as_deref().unwrap_or(&channel_id))
                                .cloned()
                                .unwrap_or_else(|| channel_display_name(ch))
                        } else {
                            channel_display_name(ch)
                        };
                        message_view.set_channel_name(&name);
                        window.set_title(Some(&format!("{name} — Sludge")));
                    }
                }

                // Hide search entry when switching channels
                message_view.search_entry.set_text("");
                message_view.search_entry.set_visible(false);
                message_view.header_label.set_visible(true);

                // Backfill from API since last cached message, then load from DB
                let mv = message_view.clone();
                let tp = thread_panel.clone();
                let state2 = state.clone();
                let client_fetch = client.clone();
                let client_img = client.clone();
                let rt2 = rt.clone();
                let rt3 = rt.clone();
                let db2 = db.clone();
                let sidebar2 = sidebar.clone();

                mv.spinner.set_visible(true);
                mv.spinner.start();

                gtk4::glib::spawn_future_local(async move {
                    let cid = channel_id.clone();

                    // Check if we need to navigate to a specific message
                    let pending_ts = state2.borrow().pending_scroll.clone()
                        .or_else(|| state2.borrow().pending_thread.as_ref().map(|(tts, _)| tts.clone()));

                    // Backfill: fetch messages from API since the last cached message
                    let db_meta = db2.clone();
                    let cid_meta = cid.clone();
                    let newest_ts = rt2
                        .spawn(async move { db_meta.get_newest_ts(&cid_meta).await })
                        .await
                        .unwrap();

                    let c = client_fetch.clone();
                    let cid_fetch = cid.clone();
                    if let Some(ref newest) = newest_ts {
                        // Fetch only messages newer than what we have
                        let oldest = newest.clone();
                        let backfill = rt2
                            .spawn(async move {
                                c.conversation_history_page(&cid_fetch, &oldest, None, 200).await
                            })
                            .await
                            .unwrap();
                        if let Ok((messages, _)) = backfill {
                            if !messages.is_empty() {
                                let db_save = db2.clone();
                                let cid_save = cid.clone();
                                let _ = rt2
                                    .spawn(async move { db_save.save_messages(&cid_save, &messages).await })
                                    .await;
                            }
                        }
                    } else {
                        // No cached messages at all — seed from API
                        let seed = rt2
                            .spawn(async move { c.conversation_history(&cid_fetch, 50).await })
                            .await
                            .unwrap();
                        if let Ok(messages) = seed {
                            if !messages.is_empty() {
                                let db_save = db2.clone();
                                let cid_save = cid.clone();
                                let _ = rt2
                                    .spawn(async move { db_save.save_messages(&cid_save, &messages).await })
                                    .await;
                            }
                        }
                    }

                    // Load messages from DB
                    let db_load = db2.clone();
                    let cid_load = cid.clone();
                    let pending_ts_load = pending_ts.clone();
                    let messages = rt2
                        .spawn(async move {
                            if let Some(ref ts) = pending_ts_load {
                                let around = db_load.load_messages_around(&cid_load, ts, 25).await;
                                if around.is_empty() { None } else { Some(around) }
                            } else {
                                db_load.load_messages(&cid_load).await
                            }
                        })
                        .await
                        .unwrap();

                    mv.spinner.stop();
                    mv.spinner.set_visible(false);

                    if let Some(messages) = messages {
                        let users = state2.borrow().user_names.clone();
                        mv.set_messages(&messages, &users, &client_img, &rt3);

                        if pending_ts.is_some() {
                            mv.has_more_newer.set(true);
                        }

                        // Scroll to and select pending message
                        let pending = state2.borrow_mut().pending_scroll.take();
                        if let Some(ts) = pending {
                            mv.scroll_to_message(&ts);
                        }

                        // Open pending thread if set by notification click
                        let pending_thread = state2.borrow_mut().pending_thread.take();
                        if let Some((thread_ts, reply_ts)) = pending_thread {
                            mv.scroll_to_message(&thread_ts);
                            tp.pending_scroll.replace(Some(reply_ts));
                            mv.open_thread(&thread_ts, &cid);
                        }

                        // Update activity and mark as read
                        let last_ts = messages.first().map(|m| m.ts.clone());
                        if let Some(ref ts) = last_ts {
                            sidebar2.update_activity(&cid, ts);
                        }
                        let db_mark = db2.clone();
                        let client_mark = client_fetch.clone();
                        let cid_mark = cid.clone();
                        rt2.spawn(async move {
                            if let Some(ref ts) = last_ts {
                                db_mark.update_channel_activity(&cid_mark, ts).await;
                                let _ = client_mark.mark_channel(&cid_mark, ts).await;
                            }
                        });
                    }
                });
            })
        };

        let on_select_ch = on_select.clone();
        sidebar.channels_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                on_select_ch(row);
            }
        });

        let on_select_dm = on_select.clone();
        sidebar.dm_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                on_select_dm(row);
            }
        });

        let on_select_gr = on_select.clone();
        sidebar.group_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                on_select_gr(row);
            }
        });
    }

    // ── Notification click → navigate to channel + message ──
    let nav_action = gio::SimpleAction::new("navigate", Some(&String::static_variant_type()));
    {
        let action = &nav_action;
        let sidebar = sidebar.clone();
        let state = state.clone();
        let message_view = message_view.clone();
        let thread_panel = thread_panel.clone();
        let window = window.clone();
        let client = client.clone();
        let rt = rt.clone();
        let db = db.clone();
        action.connect_activate(move |_, param| {
            let Some(param) = param.and_then(|p| p.get::<String>()) else { return };

            // Parse the navigation parameter:
            //   "user:U123"           → open DM with user U123
            //   "ch:C123"             → open channel C123  (from search provider)
            //   "msg:C123:1234.5678"  → open channel C123 and scroll to message
            //   "C123"                → open channel (legacy / notification format)
            //   "C123:1234.5678"      → open channel + scroll (legacy)
            //   "C123:1234.5678:5678" → open channel + thread (legacy)
            let (channel_id, message_ts, thread_ts) = if let Some(user_id) = param.strip_prefix("user:") {
                // Find the DM channel for this user
                let st = state.borrow();
                let dm_channel = st.channels.iter().find(|c| {
                    c.user.as_deref() == Some(user_id)
                });
                if let Some(ch) = dm_channel {
                    (ch.id.clone(), None, None)
                } else {
                    tracing::warn!("No DM channel found for user {user_id}");
                    return;
                }
            } else if let Some(channel_id) = param.strip_prefix("ch:") {
                (channel_id.to_string(), None, None)
            } else if let Some(rest) = param.strip_prefix("msg:") {
                let parts: Vec<&str> = rest.splitn(3, ':').collect();
                (
                    parts[0].to_string(),
                    parts.get(1).map(|s| s.to_string()),
                    parts.get(2).map(|s| s.to_string()),
                )
            } else {
                // Legacy format: channel_id[:message_ts[:thread_ts]]
                let parts: Vec<&str> = param.splitn(3, ':').collect();
                (
                    parts[0].to_string(),
                    parts.get(1).map(|s| s.to_string()),
                    parts.get(2).map(|s| s.to_string()),
                )
            };

            // Raise the window
            window.present();

            // If no explicit thread_ts, check if the message is a thread reply
            let (message_ts, thread_ts) = if thread_ts.is_none() {
                if let Some(ref mts) = message_ts {
                    let db2 = db.clone();
                    let cid = channel_id.clone();
                    let ts = mts.clone();
                    // Quick synchronous-ish lookup from DB
                    let msg = rt.block_on(db2.get_indexed_message(&cid, &ts));
                    if let Some(ref m) = msg {
                        if let Some(ref tts) = m.thread_ts {
                            if *tts != m.ts {
                                // It's a thread reply — treat thread_ts as the parent
                                (message_ts, Some(tts.clone()))
                            } else {
                                (message_ts, None)
                            }
                        } else {
                            (message_ts, None)
                        }
                    } else {
                        (message_ts, None)
                    }
                } else {
                    (message_ts, thread_ts)
                }
            } else {
                (message_ts, thread_ts)
            };

            let current = state.borrow().current_channel.clone();
            let needs_channel_switch = current.as_deref() != Some(&channel_id);

            if needs_channel_switch {
                // Set pending state, then switch channel
                if let Some(ref tts) = thread_ts {
                    if let Some(ref mts) = message_ts {
                        state.borrow_mut().pending_thread = Some((tts.clone(), mts.clone()));
                    }
                } else if let Some(ref mts) = message_ts {
                    state.borrow_mut().pending_scroll = Some(mts.clone());
                }
                sidebar.select_channel_by_id(&channel_id);
            } else if let Some(ref tts) = thread_ts {
                // Already on the channel — scroll to parent, open thread, scroll to reply
                if let Some(ref mts) = message_ts {
                    message_view.scroll_to_message(tts);
                    thread_panel.pending_scroll.replace(Some(mts.clone()));
                    message_view.open_thread(tts, &channel_id);
                }
            } else if let Some(ref mts) = message_ts {
                // Check if the message is already loaded; if not, fetch around it
                let mut found = false;
                let mut idx = 0;
                while let Some(row) = message_view.list_box.row_at_index(idx) {
                    if row.widget_name() == *mts {
                        found = true;
                        break;
                    }
                    idx += 1;
                }
                if found {
                    message_view.scroll_to_message(mts);
                } else {
                    let mv = message_view.clone();
                    let client2 = client.clone();
                    let rt2 = rt.clone();
                    let db2 = db.clone();
                    let cid = channel_id.clone();
                    let ts = mts.clone();
                    let state2 = state.clone();
                    gtk4::glib::spawn_future_local(async move {
                        let c = client2.clone();
                        let cid2 = cid.clone();
                        let ts2 = ts.clone();
                        let result = rt2.spawn(async move {
                            c.conversation_history_around(&cid2, &ts2, 25).await
                        }).await;
                        if let Ok(Ok(messages)) = result {
                            let users = state2.borrow().user_names.clone();
                            mv.set_messages(&messages, &users, &client2, &rt2);
                            mv.has_more_newer.set(true);
                            mv.scroll_to_message(&ts);
                            let db3 = db2.clone();
                            let to_save = messages.clone();
                            let cid3 = cid.clone();
                            rt2.spawn(async move {
                                let _ = db3.save_messages(&cid3, &to_save).await;
                            });
                        }
                    });
                }
            }
        });
        app.add_action(action);
    }

    // ── Tab from sidebar moves focus to message input ──
    {
        let input = message_input.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        key_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gtk4::gdk::Key::Tab {
                input.text_view.grab_focus();
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        sidebar.widget.add_controller(key_controller);
    }

    // ── Ctrl+P toggles channel search, Ctrl+F toggles message search ──
    {
        let ch_search_btn = channel_search_btn.clone();
        let msg_entry = message_view.search_entry.clone();
        let header_label = message_view.header_label.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            if modifier.contains(gtk4::gdk::ModifierType::CONTROL_MASK) {
                if key == gtk4::gdk::Key::p {
                    ch_search_btn.set_active(!ch_search_btn.is_active());
                    return gtk4::glib::Propagation::Stop;
                } else if key == gtk4::gdk::Key::f {
                    msg_entry.set_visible(true);
                    header_label.set_visible(false);
                    msg_entry.grab_focus();
                    return gtk4::glib::Propagation::Stop;
                }
            }
            gtk4::glib::Propagation::Proceed
        });
        window.add_controller(key_controller);
    }

    // ── Press : on selected message to open reaction picker ──
    {
        let open_reaction_picker = |list_box: &gtk::ListBox| {
            let list_box = list_box.clone();
            let key_controller = gtk::EventControllerKey::new();
            key_controller.connect_key_pressed(move |_, key, _, _| {
                if key == gtk4::gdk::Key::colon {
                    if let Some(row) = list_box.selected_row() {
                        // Walk the row's widget tree to find the reaction-add button
                        fn find_reaction_btn(widget: &gtk::Widget) -> Option<gtk::Button> {
                            if let Some(btn) = widget.downcast_ref::<gtk::Button>() {
                                if btn.has_css_class("reaction-add-btn") {
                                    return Some(btn.clone());
                                }
                            }
                            let mut child = widget.first_child();
                            while let Some(c) = child {
                                if let Some(btn) = find_reaction_btn(&c) {
                                    return Some(btn);
                                }
                                child = c.next_sibling();
                            }
                            None
                        }
                        if let Some(btn) = find_reaction_btn(row.upcast_ref()) {
                            btn.emit_clicked();
                            return gtk4::glib::Propagation::Stop;
                        }
                    }
                }
                gtk4::glib::Propagation::Proceed
            });
            key_controller
        };

        message_view.list_box.add_controller(open_reaction_picker(&message_view.list_box));
        thread_panel.list_box.add_controller(open_reaction_picker(&thread_panel.list_box));
    }

    // ── Message search: navigate to channel + scroll to message on click ──
    {
        let state_nav = state.clone();
        let sidebar_nav = sidebar.clone();
        let message_view_nav = message_view.clone();
        let client_nav = client.clone();
        let rt_nav = rt.clone();
        message_view.set_search_result_callback(Rc::new(move |channel_id: &str, ts: &str| {
            let current = state_nav.borrow().current_channel.clone();
            let needs_switch = current.as_deref() != Some(channel_id);

            let mv = message_view_nav.clone();
            let c = client_nav.clone();
            let c_img = client_nav.clone();
            let rt2 = rt_nav.clone();
            let rt3 = rt_nav.clone();
            let cid = channel_id.to_string();
            let target_ts = ts.to_string();
            let state2 = state_nav.clone();
            let sidebar2 = sidebar_nav.clone();

            // Set current channel immediately so the sidebar switch doesn't
            // trigger a second load
            state_nav.borrow_mut().current_channel = Some(cid.clone());

            if needs_switch {
                sidebar2.select_channel_by_id(&cid);
            }

            // Load 25 messages either side of the target message
            mv.spinner.set_visible(true);
            mv.spinner.start();

            gtk4::glib::spawn_future_local(async move {
                let ch = cid.clone();
                let t = target_ts.clone();
                let result = rt2
                    .spawn(async move { c.conversation_history_around(&ch, &t, 25).await })
                    .await
                    .unwrap();

                mv.spinner.stop();
                mv.spinner.set_visible(false);

                if let Ok(messages) = result {
                    let users = state2.borrow().user_names.clone();
                    mv.set_messages(&messages, &users, &c_img, &rt3);
                    mv.has_more_newer.set(true);
                    mv.scroll_to_message(&target_ts);
                }
            });
        }));
    }

    // ── Message search: query on Enter (searches current channel) ──
    {
        let db_search = db.clone();
        let rt_search = rt.clone();
        let state_search = state.clone();
        let mv_search = message_view.clone();
        let client_search = client.clone();
        message_view.search_entry.connect_activate(move |entry| {
            let query = entry.text().to_string();
            if query.trim().is_empty() {
                // Empty query — restore latest messages and hide search
                let current_channel = state_search.borrow().current_channel.clone();
                let Some(channel_id) = current_channel else { return };
                entry.set_visible(false);
                mv_search.header_label.set_visible(true);
                if mv_search.search_query.borrow().is_none() {
                    return; // already showing normal messages
                }
                let rt = rt_search.clone();
                let state = state_search.clone();
                let mv = mv_search.clone();
                let client = client_search.clone();
                gtk4::glib::spawn_future_local(async move {
                    let c = client.clone();
                    let cid = channel_id.clone();
                    let result = rt
                        .spawn(async move { c.conversation_history(&cid, 25).await })
                        .await;
                    if let Ok(Ok(messages)) = result {
                        let users = state.borrow().user_names.clone();
                        mv.set_messages(&messages, &users, &client, &rt);
                        mv.scroll_to_bottom();
                    }
                });
                return;
            }

            let current_channel = state_search.borrow().current_channel.clone();
            let Some(channel_id) = current_channel else {
                return;
            };

            let db = db_search.clone();
            let rt = rt_search.clone();
            let state = state_search.clone();
            let mv = mv_search.clone();
            let client = client_search.clone();
            gtk4::glib::spawn_future_local(async move {
                let q = query.clone();
                let cid = channel_id.clone();
                let results = rt
                    .spawn(async move { db.search_channel_messages(&cid, &q).await })
                    .await
                    .unwrap();

                let users = state.borrow().user_names.clone();
                mv.set_channel_search_results(&query, &results, &users, &client, &rt);
            });
        });
    }

    // ── Clear search when text is emptied (e.g. pressing X or Escape) ──
    {
        let state_clear = state.clone();
        let mv_clear = message_view.clone();
        let client_clear = client.clone();
        let rt_clear = rt.clone();
        message_view.search_entry.connect_search_changed(move |entry| {
            if !entry.text().is_empty() {
                return;
            }
            // Text was cleared — restore latest messages and hide search
            let current_channel = state_clear.borrow().current_channel.clone();
            let Some(channel_id) = current_channel else { return };
            entry.set_visible(false);
            mv_clear.header_label.set_visible(true);
            if mv_clear.search_query.borrow().is_none() {
                return;
            }
            let c = client_clear.clone();
            let rt = rt_clear.clone();
            let state = state_clear.clone();
            let mv = mv_clear.clone();
            let client = client_clear.clone();
            gtk4::glib::spawn_future_local(async move {
                let cid = channel_id.clone();
                let result = rt
                    .spawn(async move { c.conversation_history(&cid, 25).await })
                    .await;
                if let Ok(Ok(messages)) = result {
                    let users = state.borrow().user_names.clone();
                    mv.set_messages(&messages, &users, &client, &rt);
                    mv.scroll_to_bottom();
                }
            });
        });
    }

    // ── Send message ──
    {
        let state_send = state.clone();
        let input = message_input.clone();
        let rt_send = rt.clone();
        let client_send = client.clone();
        let message_view_send = message_view.clone();

        let do_send = move || {
            let text = input.get_text();
            let files = input.take_files();
            if text.trim().is_empty() && files.is_empty() {
                return;
            }

            let channel = match state_send.borrow().current_channel.clone() {
                Some(c) => c,
                None => return,
            };

            input.clear();

            let client = client_send.clone();
            let rt2 = rt_send.clone();
            let mv = message_view_send.clone();
            let state2 = state_send.clone();
            let client2 = client_send.clone();
            let rt3 = rt_send.clone();
            gtk4::glib::spawn_future_local(async move {
                let c = client.clone();
                let ch = channel.clone();
                let txt = text.clone();
                let sent_ts = rt2
                    .spawn(async move {
                        // Upload files first
                        for path in &files {
                            let filename = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "file".into());
                            match tokio::fs::read(path).await {
                                Ok(bytes) => {
                                    // First file gets the text as initial_comment, rest get none
                                    let comment = if path == files.first().unwrap() && !txt.trim().is_empty() {
                                        Some(txt.as_str())
                                    } else {
                                        None
                                    };
                                    if let Err(e) = c.upload_file(&ch, bytes, &filename, comment, None).await {
                                        tracing::error!("Failed to upload file {filename}: {e}");
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to read file {}: {e}", path.display());
                                }
                            }
                        }
                        // If no files, just send a text message and return the ts
                        if files.is_empty() && !txt.trim().is_empty() {
                            match c.post_message(&ch, &txt, None).await {
                                Ok(ts) => Some(ts),
                                Err(e) => {
                                    tracing::error!("Failed to send message: {e}");
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    })
                    .await
                    .unwrap_or(None);

                // Optimistically append the sent message so it's visible immediately
                if let Some(ts) = sent_ts {
                    state2.borrow_mut().sent_ts.insert(ts.clone());
                    // If the socket event already added this message while we
                    // were awaiting the API call, don't add it again.
                    if !list_has_ts(&mv.list_box, &ts) {
                        let st = state2.borrow();
                        let self_uid = st.self_user_id.clone();
                        let users = st.user_names.clone();
                        drop(st);
                        let msg = Message {
                            msg_type: "message".into(),
                            user: Some(self_uid),
                            bot_id: None,
                            text,
                            ts,
                            thread_ts: None,
                            channel: Some(channel),
                            attachments: None,
                            reactions: None,
                            files: None,
                        };
                        mv.append_message(&msg, &users, &client2, &rt3);
                    }
                }
            });
        };

        // Send button click
        let do_send_clone = do_send.clone();
        message_input.send_button.send.connect_clicked(move |_| {
            do_send_clone();
        });

        // Enter to send, Shift+Enter or Ctrl+Enter to insert newline
        let key_controller = gtk::EventControllerKey::new();
        let do_send_key = do_send.clone();
        let tv = message_input.text_view.clone();
        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            if key == gtk4::gdk::Key::Return || key == gtk4::gdk::Key::KP_Enter {
                if modifier.contains(gtk4::gdk::ModifierType::SHIFT_MASK)
                    || modifier.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
                {
                    tv.buffer().insert_at_cursor("\n");
                    return gtk4::glib::Propagation::Stop;
                }
                do_send_key();
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        message_input.text_view.add_controller(key_controller);
    }

    // ── Process real-time events from Socket Mode ──
    {
        let state = state.clone();
        let message_view = message_view.clone();
        let thread_panel = thread_panel.clone();
        let sidebar = sidebar.clone();
        let client_rt = client.clone();
        let rt_rt = rt.clone();
        let db = db.clone();
        let presence_active = presence_active.clone();
        let profile_avatar = profile_avatar.clone();
        let avatar_texture = avatar_texture.clone();
        let emoji_entry_rt = emoji_entry.clone();
        let status_entry_rt = status_entry.clone();
        let self_status_icon_rt = self_status_icon.clone();
        let nav_action = nav_action.clone();
        let (notif_nav_tx, mut notif_nav_rx) = mpsc::unbounded_channel::<String>();

        // Drain notification click navigations on the main thread
        {
            let nav_action = nav_action.clone();
            gtk4::glib::spawn_future_local(async move {
                while let Some(target) = notif_nav_rx.recv().await {
                    nav_action.activate(Some(&target.to_variant()));
                }
            });
        }

        gtk4::glib::spawn_future_local(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    SlackEvent::MessageReceived {
                        channel,
                        user,
                        text,
                        ts,
                        thread_ts,
                        files,
                    } => {
                        let msg = Message {
                            msg_type: "message".into(),
                            user: user.clone(),
                            bot_id: None,
                            text: text.clone(),
                            ts,
                            thread_ts: thread_ts.clone(),
                            channel: Some(channel.clone()),
                            attachments: None,
                            reactions: None,
                            files,
                        };

                        // Skip messages we already appended optimistically
                        if state.borrow_mut().sent_ts.remove(&msg.ts) {
                            // Still need to cache, update activity, and ensure scroll
                            let is_current = state.borrow().current_channel.as_deref() == Some(&channel);
                            if is_current {
                                message_view.scroll_to_bottom();
                            }
                            sidebar.update_activity(&channel, &msg.ts);
                            let db2 = db.clone();
                            let msg2 = msg.clone();
                            let cid = channel.clone();
                            let ts = msg.ts.clone();
                            rt_rt.spawn(async move {
                                db2.append_message(&cid, &msg2).await;
                                db2.update_channel_activity(&cid, &ts).await;
                            });
                            continue;
                        }

                        let (current, current_thread, users) = {
                            let st = state.borrow();
                            (
                                st.current_channel.clone(),
                                st.current_thread.clone(),
                                st.user_names.clone(), // Rc clone, O(1)
                            )
                        };

                        // Append to thread panel if the message belongs to the open thread
                        if let Some((tts, tcid)) = &current_thread {
                            if *tcid == channel
                                && thread_ts.as_deref() == Some(tts.as_str())
                            {
                                thread_panel.append_message(
                                    &msg, &users, &client_rt, &rt_rt,
                                );
                            }
                        }

                        let is_current = current.as_deref() == Some(&channel);

                        // Update thread reply count for thread replies in the current channel
                        if is_current {
                            if let Some(tts) = &thread_ts {
                                if *tts != msg.ts {
                                    let current_count = message_view
                                        .thread_counts
                                        .borrow()
                                        .get(tts)
                                        .copied()
                                        .unwrap_or(0);
                                    message_view.update_thread_count(tts, current_count + 1);
                                }
                            }
                        }

                        // Append to main message view if in current channel
                        // (skip thread replies that aren't top-level, skip already-displayed messages)
                        if is_current {
                            let is_thread_reply = thread_ts
                                .as_ref()
                                .is_some_and(|tts| *tts != msg.ts);
                            if !is_thread_reply {
                                message_view.append_message(
                                    &msg, &users, &client_rt, &rt_rt,
                                );
                            }
                        }

                        // Update unread count for non-active channels
                        if !is_current {
                            let count = {
                                let mut st = state.borrow_mut();
                                let count = st
                                    .unread_counts
                                    .entry(channel.clone())
                                    .or_insert(0);
                                *count += 1;
                                *count
                            };
                            sidebar.set_unread(&channel, count);
                        }

                        // Cache the incoming message, index for search, update activity, mark read
                        {
                            sidebar.update_activity(&channel, &msg.ts);
                            let db2 = db.clone();
                            let client_mark = client_rt.clone();
                            let msg2 = msg.clone();
                            let cid = channel.clone();
                            let ts = msg.ts.clone();
                            let mark_read = is_current;
                            rt_rt.spawn(async move {
                                db2.append_message(&cid, &msg2).await;
                                db2.update_channel_activity(&cid, &ts).await;
                                if mark_read {
                                    let _ = client_mark.mark_channel(&cid, &ts).await;
                                }
                            });
                        }

                        // Desktop notification for messages not in the active channel
                        if !is_current {
                            let st = state.borrow();
                            let sender = user
                                .as_deref()
                                .and_then(|uid| st.user_names.get(uid))
                                .cloned()
                                .unwrap_or_else(|| "Someone".into());
                            let channel_name = st
                                .channels
                                .iter()
                                .find(|c| c.id == channel)
                                .map(|c| {
                                    if c.is_im == Some(true) {
                                        st.user_names
                                            .get(c.user.as_deref().unwrap_or(""))
                                            .cloned()
                                            .unwrap_or_else(|| channel_display_name(c))
                                    } else {
                                        channel_display_name(c)
                                    }
                                })
                                .unwrap_or_else(|| channel.clone());
                            let plain_text = format_message_plain(&text, &st.user_names, &st.subteam_names);
                            drop(st);

                            let title = format!("{sender} in #{channel_name}");
                            let body = if plain_text.len() > 200 {
                                format!("{}...", &plain_text[..200])
                            } else {
                                plain_text
                            };

                            let nav_channel = channel.clone();
                            let nav_ts = msg.ts.clone();
                            let nav_thread_ts = thread_ts.clone();
                            let nav_tx = notif_nav_tx.clone();
                            rt_rt.spawn(async move {
                                // Play notification sound
                                let _ = tokio::process::Command::new("canberra-gtk-play")
                                    .arg("-i").arg("message-new-instant")
                                    .arg("-d").arg("Sludge notification")
                                    .spawn();

                                let mut child = match tokio::process::Command::new("notify-send")
                                    .arg("--app-name=Sludge")
                                    .arg("--urgency=normal")
                                    .arg("--expire-time=15000")
                                    .arg("--action=default=Open")
                                    .arg("--wait")
                                    .arg(&title)
                                    .arg(&body)
                                    .stdout(std::process::Stdio::piped())
                                    .spawn()
                                {
                                    Ok(c) => c,
                                    Err(_) => return,
                                };
                                // Take stdout before waiting so we can still kill on timeout
                                let stdout_handle = child.stdout.take();
                                let result = tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    child.wait(),
                                ).await;
                                match result {
                                    Ok(Ok(_status)) => {
                                        if let Some(stdout) = stdout_handle {
                                            use tokio::io::AsyncReadExt;
                                            let mut buf = String::new();
                                            let mut reader = tokio::io::BufReader::new(stdout);
                                            let _ = reader.read_to_string(&mut buf).await;
                                            if buf.trim() == "default" {
                                                let target = if let Some(tts) = &nav_thread_ts {
                                                    format!("{nav_channel}:{nav_ts}:{tts}")
                                                } else {
                                                    format!("{nav_channel}:{nav_ts}")
                                                };
                                                let _ = nav_tx.send(target);
                                            }
                                        }
                                    }
                                    Ok(Err(_)) => {}
                                    Err(_) => {
                                        let _ = child.kill().await;
                                    }
                                }
                            });
                        }
                    }
                    SlackEvent::PresenceChange { user, presence } => {
                        tracing::debug!("Presence change: {user} -> {presence}");
                        let is_active = presence == "active";
                        let self_uid = state.borrow().self_user_id.clone();
                        let is_self = user.is_empty() || self_uid == user;
                        if is_self {
                            *presence_active.borrow_mut() = is_active;
                            profile_avatar.queue_draw();
                        }
                        // Resolve effective user ID (manual_presence_change has empty user)
                        let effective_uid = if user.is_empty() { self_uid } else { user.clone() };
                        sidebar.set_presence(&effective_uid, is_active);

                        // Notify if this user is on the watch list and came online
                        if is_active {
                            let is_watched = sidebar.is_watched(&effective_uid);
                            if is_watched {
                                let st = state.borrow();
                                let display_name = st.user_names
                                    .get(&effective_uid)
                                    .cloned()
                                    .unwrap_or_else(|| effective_uid.clone());
                                // Find the DM channel for this user
                                let dm_channel_id = st.channels.iter()
                                    .find(|c| c.is_im == Some(true) && c.user.as_deref() == Some(&effective_uid))
                                    .map(|c| c.id.clone());
                                drop(st);

                                let title = format!("{display_name} is online");
                                let nav_tx = notif_nav_tx.clone();
                                let nav_channel = dm_channel_id.clone();
                                rt_rt.spawn(async move {
                                    // Play notification sound
                                    let _ = tokio::process::Command::new("canberra-gtk-play")
                                        .arg("-i").arg("message-new-instant")
                                        .arg("-d").arg("Sludge notification")
                                        .spawn();

                                    let mut child = match tokio::process::Command::new("notify-send")
                                        .arg("--app-name=Sludge")
                                        .arg("--urgency=normal")
                                        .arg("--action=default=Open")
                                        .arg("--wait")
                                        .arg(&title)
                                        .arg(format!("{display_name} is now available"))
                                        .stdout(std::process::Stdio::piped())
                                        .spawn()
                                    {
                                        Ok(c) => c,
                                        Err(_) => return,
                                    };
                                    let stdout_handle = child.stdout.take();
                                    let result = tokio::time::timeout(
                                        std::time::Duration::from_secs(30),
                                        child.wait(),
                                    ).await;
                                    match result {
                                        Ok(Ok(_)) => {
                                            if let (Some(stdout), Some(cid)) = (stdout_handle, nav_channel) {
                                                use tokio::io::AsyncReadExt;
                                                let mut buf = String::new();
                                                let mut reader = tokio::io::BufReader::new(stdout);
                                                let _ = reader.read_to_string(&mut buf).await;
                                                if buf.trim() == "default" {
                                                    // Navigate to the DM channel
                                                    let _ = nav_tx.send(cid.clone());
                                                }
                                            }
                                        }
                                        Ok(Err(_)) => {}
                                        Err(_) => { let _ = child.kill().await; }
                                    }
                                });
                            }
                        }
                    }
                    SlackEvent::UserProfileChanged { user, profile } => {
                        tracing::debug!("Profile changed for user {user}");
                        let is_self = state.borrow().self_user_id == user;

                        // Update display name in state for any user
                        if let Some(display_name) = profile
                            .get("display_name")
                            .and_then(|v| v.as_str())
                            .filter(|n| !n.is_empty())
                            .or_else(|| profile.get("real_name").and_then(|v| v.as_str()))
                        {
                            Rc::make_mut(&mut state.borrow_mut().user_names)
                                .insert(user.clone(), display_name.to_string());
                        }

                        // Update status emoji and text for any user
                        let status_emoji = profile
                            .get("status_emoji")
                            .and_then(|v| v.as_str())
                            .filter(|e| !e.is_empty());
                        sidebar.set_status_emoji(&user, status_emoji);
                        let status_text_val = profile
                            .get("status_text")
                            .and_then(|v| v.as_str())
                            .filter(|t| !t.is_empty());
                        sidebar.set_status_text(&user, status_text_val);

                        // Only update our own profile UI
                        if is_self {
                            if let Some(status_text) =
                                profile.get("status_text").and_then(|v| v.as_str())
                            {
                                emoji_entry_rt.buffer().set_text(
                                    profile
                                        .get("status_emoji")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(""),
                                );
                                status_entry_rt.set_text(status_text);
                            }
                            update_self_status_icon(
                                &self_status_icon_rt,
                                profile.get("status_emoji").and_then(|v| v.as_str()),
                            );

                            let image_url = profile
                                .get("image_72")
                                .or_else(|| profile.get("image_48"))
                                .and_then(|v| v.as_str())
                                .map(String::from);

                            if let Some(url) = image_url {
                                let c = client_rt.clone();
                                let rt = rt_rt.clone();
                                let avatar_texture = avatar_texture.clone();
                                let profile_avatar = profile_avatar.clone();
                                let res = rt
                                    .spawn(async move { c.fetch_image_bytes(&url).await })
                                    .await
                                    .unwrap();

                                if let Ok(bytes) = res {
                                    let gbytes = gtk4::glib::Bytes::from_owned(bytes);
                                    let stream =
                                        gtk4::gio::MemoryInputStream::from_bytes(&gbytes);
                                    if let Ok(pixbuf) =
                                        gtk4::gdk_pixbuf::Pixbuf::from_stream(
                                            &stream,
                                            gtk4::gio::Cancellable::NONE,
                                        )
                                    {
                                        let texture =
                                            gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                                        *avatar_texture.borrow_mut() = Some(texture);
                                        profile_avatar.queue_draw();
                                    }
                                }
                            }
                        }
                    }
                    SlackEvent::ReactionChanged { channel, message_ts, reaction, user, added } => {
                        let db2 = db.clone();
                        let channel2 = channel.clone();
                        let message_ts2 = message_ts.clone();
                        let reaction2 = reaction.clone();
                        let user2 = user.clone();
                        let message_view2 = message_view.clone();
                        let thread_panel2 = thread_panel.clone();
                        let state2 = state.clone();

                        // Update DB and refresh UI
                        let updated = rt_rt.spawn(async move {
                            db2.update_reaction(&channel2, &message_ts2, &reaction2, &user2, added).await
                        }).await;

                        if let Ok(Some(reactions)) = updated {
                            let (is_current, users) = {
                                let st = state2.borrow();
                                (st.current_channel.as_deref() == Some(&channel), st.user_names.clone())
                            };
                            if is_current {
                                message_view2.update_reactions(&message_ts, &reactions, &users);
                                thread_panel2.update_reactions(&message_ts, &reactions, &users);
                            }
                        }
                    }
                    SlackEvent::UserTyping { channel, user } => {
                        let (is_current, name) = {
                            let st = state.borrow();
                            let is_current = st.current_channel.as_deref() == Some(&channel);
                            let name = st.user_names.get(&user).cloned()
                                .unwrap_or_else(|| "Someone".into());
                            (is_current, name)
                        };
                        if is_current {
                            let tl = &message_view.typing_label;
                            tl.set_text(&format!("{name} is typing..."));
                            tl.set_visible(true);

                            // Auto-hide after 3 seconds
                            let tl = tl.clone();
                            gtk4::glib::timeout_add_local_once(
                                std::time::Duration::from_secs(3),
                                move || {
                                    tl.set_visible(false);
                                },
                            );
                        }
                    }
                    SlackEvent::ChannelMarked { channel, unread_count_display } => {
                        {
                            let mut st = state.borrow_mut();
                            if unread_count_display == 0 {
                                st.unread_counts.remove(&channel);
                            } else {
                                st.unread_counts.insert(channel.clone(), unread_count_display);
                            }
                        }
                        sidebar.set_unread(&channel, unread_count_display);
                    }
                    SlackEvent::MessageReplied { channel, thread_ts, reply_count } => {
                        let current = state.borrow().current_channel.clone();
                        if current.as_deref() == Some(&channel) {
                            message_view.update_thread_count(&thread_ts, reply_count);
                        }
                    }
                    SlackEvent::Connected => {
                        tracing::info!("Socket Mode connected (UI notified)");
                    }
                    SlackEvent::Disconnected => {
                        tracing::warn!("Socket Mode disconnected (UI notified)");
                    }
                }
            }
        });
    }

    // Focus message input when window gains focus
    {
        let input = message_input.text_view.clone();
        window.connect_is_active_notify(move |win| {
            if win.is_active() {
                input.grab_focus();
            }
        });
    }

    // Apply CSS
    load_css();

    window.present();
}

/// Check if a ListBox already contains a row with the given widget name (ts).
fn list_has_ts(list_box: &gtk::ListBox, ts: &str) -> bool {
    let mut idx = 0;
    while let Some(row) = list_box.row_at_index(idx) {
        if row.widget_name() == ts {
            return true;
        }
        idx += 1;
    }
    false
}

/// Build emoji path map from cached DB data (name → local path or "alias:name").
/// For non-alias entries, builds the expected cache path from the URL hash.
fn build_emoji_path_map(emoji: &HashMap<String, String>) -> HashMap<String, String> {
    use std::hash::{Hash, Hasher};

    let cache_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("sludge")
        .join("emoji_cache");

    let mut paths = HashMap::new();
    for (name, value) in emoji {
        if value.starts_with("alias:") {
            paths.insert(name.clone(), value.clone());
        } else {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            value.hash(&mut hasher);
            let hash = hasher.finish();
            let path = cache_dir.join(format!("{hash:016x}"));
            if path.exists() {
                paths.insert(name.clone(), path.to_string_lossy().to_string());
            }
        }
    }
    paths
}

/// Download custom emoji images to local cache and return name → path map.
async fn download_custom_emoji(
    client: &Client,
    emoji: &HashMap<String, String>,
) -> HashMap<String, String> {
    use std::hash::{Hash, Hasher};

    let cache_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("sludge")
        .join("emoji_cache");

    let _ = tokio::fs::create_dir_all(&cache_dir).await;

    let mut paths = HashMap::new();
    for (name, value) in emoji {
        if value.starts_with("alias:") {
            paths.insert(name.clone(), value.clone());
            continue;
        }

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        let hash = hasher.finish();
        let path = cache_dir.join(format!("{hash:016x}"));

        // Skip if already cached
        if path.exists() {
            paths.insert(name.clone(), path.to_string_lossy().to_string());
            continue;
        }

        // Download the emoji image
        match client.fetch_image_bytes(value).await {
            Ok(bytes) => {
                if let Err(e) = tokio::fs::write(&path, &bytes).await {
                    tracing::debug!("Failed to cache emoji {name}: {e}");
                } else {
                    paths.insert(name.clone(), path.to_string_lossy().to_string());
                }
            }
            Err(e) => {
                tracing::debug!("Failed to download emoji {name}: {e}");
            }
        }
    }

    tracing::info!("Custom emoji cache: {} of {} downloaded", paths.len(), emoji.len());
    paths
}

/// Update the self-status emoji icon beside the profile avatar.
fn update_self_status_icon(container: &gtk::Box, emoji_code: Option<&str>) {
    use crate::slack::helpers::{get_custom_emoji_path, resolve_slack_shortcode};

    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    let code = match emoji_code.filter(|e| !e.is_empty()) {
        Some(c) => c,
        None => return,
    };

    let trimmed = code.trim_matches(':');

    // Try custom emoji image
    if let Some(path) = get_custom_emoji_path(trimmed) {
        if let Ok(pixbuf) = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(&path, 16, 16, true) {
            let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
            let image = gtk::Image::from_paintable(Some(&texture));
            image.set_pixel_size(16);
            container.append(&image);
            return;
        }
    }

    // Standard emoji as text
    if let Some(emoji_str) = resolve_slack_shortcode(trimmed) {
        let label = Label::new(Some(emoji_str));
        container.append(&label);
    }
}

fn sort_and_filter_channels(channels: Vec<Channel>) -> Vec<Channel> {
    let mut sorted: Vec<_> = channels
        .into_iter()
        .filter(|ch| {
            ch.is_member == Some(true)
                || ch.is_im == Some(true)
                || ch.is_group == Some(true)
        })
        .collect();
    sorted.sort_by(|a, b| {
        let a_im = a.is_im == Some(true);
        let b_im = b.is_im == Some(true);
        a_im.cmp(&b_im).then_with(|| {
            channel_display_name(a)
                .to_lowercase()
                .cmp(&channel_display_name(b).to_lowercase())
        })
    });
    sorted
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        r#"
        .sidebar {
            background-color: @headerbar_bg_color;
        }
        .sidebar listbox {
            background: transparent;
        }
        .sidebar listbox row {
            padding: 2px 4px;
            border-radius: 6px;
            margin: 1px 4px;
        }
        .sidebar listbox row:selected {
            background-color: alpha(@accent_color, 0.2);
        }
        .unread-badge {
            background-color: @accent_color;
            color: white;
            border-radius: 10px;
            padding: 0px 6px;
            font-size: 0.75em;
            font-weight: bold;
            min-width: 16px;
        }
        .mention-link {
            color: @accent_color;
            text-decoration: underline;
        }
        .message-name-btn {
            padding: 0;
            min-height: 0;
        }
        .message-name-btn:hover {
            text-decoration: underline;
        }
        .reaction-btn {
            padding: 1px 6px;
            min-height: 22px;
            font-size: 0.85em;
            border-radius: 12px;
            background-color: alpha(@accent_color, 0.1);
        }
        .reaction-btn:hover {
            background-color: alpha(@accent_color, 0.25);
        }
        .reaction-add-btn {
            padding: 1px 4px;
            min-height: 22px;
            min-width: 22px;
            border-radius: 12px;
            opacity: 0.5;
        }
        .reaction-add-btn:hover {
            opacity: 1.0;
        }
        .emoji-picker-btn {
            padding: 4px;
            min-width: 32px;
            min-height: 32px;
            font-size: 1.3em;
        }
        .thread-btn {
            margin-top: 4px;
            padding: 2px 8px;
            font-size: 0.85em;
            opacity: 0.7;
        }
        .thread-btn:hover {
            opacity: 1.0;
        }
        .delete-btn {
            opacity: 0.3;
            padding: 2px;
            min-height: 0;
            min-width: 0;
        }
        .delete-btn:hover {
            opacity: 1.0;
            color: @error_color;
        }
        .presence-active {
            color: #2BAC76;
        }
        .message-body-textview {
            background: transparent;
            padding: 0;
            margin: 0;
        }
        .message-body-textview text {
            background: transparent;
        }
        .notification-highlight {
            background-color: alpha(@accent_color, 0.15);
            transition: background-color 300ms ease-in;
        }
        "#,
    );
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("no display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Background task: fetch up to 1 year of message history for all channels
/// and index them for full-text search. Backs off exponentially on rate limits.
async fn backfill_history(client: Client, db: Arc<Database>, channel_ids: Vec<String>) {
    use std::time::Duration;

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(30)).timestamp();
    let oldest = format!("{cutoff}.000000");
    let page_size = 200u32;
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(300);
    let recheck_days = 3;

    // Check which channels already have indexed history reaching the 30-day cutoff
    let channel_meta = db.load_all_channel_meta().await;
    let now = chrono::Utc::now();
    let channels_to_backfill: Vec<&String> = channel_ids
        .iter()
        .filter(|id| {
            match channel_meta.get(*id) {
                Some((meta_oldest, _, checked_at)) => {
                    // If we've reached the end of history and checked recently, skip
                    if meta_oldest <= &oldest {
                        if let Some(checked) = checked_at {
                            if let Ok(checked_time) = chrono::DateTime::parse_from_rfc3339(checked) {
                                let age = now.signed_duration_since(checked_time);
                                if age < chrono::Duration::days(recheck_days) {
                                    return false;
                                }
                            }
                        }
                    }
                    meta_oldest > &oldest
                }
                None => true, // No metadata → needs backfill
            }
        })
        .collect();

    if channels_to_backfill.is_empty() {
        tracing::info!(
            "Backfill: all {} channels already have 30 days of history indexed",
            channel_ids.len()
        );
        return;
    }

    tracing::info!(
        "Starting background backfill for {}/{} channels (oldest={})",
        channels_to_backfill.len(),
        channel_ids.len(),
        oldest
    );

    for (i, channel_id) in channels_to_backfill.iter().enumerate() {
        // Start from the oldest already-indexed message so we don't re-fetch
        let mut latest: Option<String> = db.oldest_indexed_ts(channel_id).await;
        let mut total = 0u32;

        loop {
            let result = client
                .conversation_history_page(
                    channel_id,
                    &oldest,
                    latest.as_deref(),
                    page_size,
                )
                .await;

            match result {
                Ok((messages, has_more)) => {
                    // Reset backoff on success
                    backoff = Duration::from_secs(1);

                    if messages.is_empty() {
                        db.mark_backfill_checked(channel_id).await;
                        break;
                    }

                    total += messages.len() as u32;

                    // The oldest message in this batch becomes the next page's `latest`
                    if let Some(oldest_msg) = messages.last() {
                        latest = Some(oldest_msg.ts.clone());
                    }

                    db.index_messages(channel_id, &messages).await;
                    db.update_channel_meta(channel_id, &messages).await;

                    if !has_more {
                        db.mark_backfill_checked(channel_id).await;
                        break;
                    }

                    // Small delay between pages to be polite
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(e) if e.contains("ratelimited") || e.contains("rate_limited") => {
                    tracing::warn!(
                        "Backfill rate limited on channel {} — backing off {:?}",
                        channel_id,
                        backoff
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                    continue; // Retry same page
                }
                Err(e) => {
                    tracing::warn!(
                        "Backfill error for channel {}: {e} — skipping",
                        channel_id
                    );
                    break;
                }
            }
        }

        if total > 0 {
            tracing::debug!(
                "Backfill [{}/{}] channel {}: indexed {} messages",
                i + 1,
                channels_to_backfill.len(),
                channel_id,
                total
            );
        }
    }

    tracing::info!("Background backfill complete");
}


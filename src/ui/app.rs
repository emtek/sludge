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

/// Shared application state accessible from GTK callbacks.
struct AppState {
    channels: Vec<Channel>,
    /// Map from user ID to display name.
    user_names: HashMap<String, String>,
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
}

pub fn build_app(
    app: &Application,
    client: Client,
    rt: tokio::runtime::Handle,
    mut event_rx: mpsc::UnboundedReceiver<SlackEvent>,
    db: Arc<Database>,
    user_id: String,
    presence_tx: mpsc::UnboundedSender<Vec<String>>,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Slack")
        .default_width(1200)
        .default_height(800)
        .build();

    // ── Layout ──
    // [ Sidebar | Message area ]
    //                [ Messages ]
    //                [ Input    ]

    // ── Header bar with profile button ──
    let header_bar = gtk::HeaderBar::new();

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
    let emoji_entry = gtk::Entry::new();
    emoji_entry.set_placeholder_text(Some(":emoji:"));
    emoji_entry.set_width_chars(10);
    emoji_row.append(&emoji_entry);

    // Emoji chooser button
    let emoji_choose_btn = gtk::MenuButton::new();
    emoji_choose_btn.set_icon_name("face-smile-symbolic");
    emoji_choose_btn.add_css_class("flat");
    let emoji_chooser = gtk::EmojiChooser::new();
    let emoji_entry_ref = emoji_entry.clone();
    emoji_chooser.connect_emoji_picked(move |_, emoji| {
        let shortcode = emojis::get(emoji)
            .and_then(|e| e.shortcode())
            .map(|s| format!(":{s}:"))
            .unwrap_or_else(|| emoji.to_string());
        emoji_entry_ref.set_text(&shortcode);
    });
    emoji_choose_btn.set_popover(Some(&emoji_chooser));
    emoji_row.append(&emoji_choose_btn);

    let status_entry = gtk::Entry::new();
    status_entry.set_placeholder_text(Some("What's your status?"));
    status_entry.set_hexpand(true);
    emoji_row.append(&status_entry);
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
                    ee.set_text(&s.emoji);
                    se.set_text(&s.text);
                });

                recent_box.append(&btn);
            }
        })
    };

    let profile_btn = gtk::MenuButton::new();
    profile_btn.set_child(Some(&profile_avatar));
    profile_btn.set_popover(Some(&popover));
    profile_btn.add_css_class("flat");

    header_bar.pack_end(&profile_btn);

    // ── Layout ──
    // [ Sidebar | Messages+Input | ThreadPanel ]
    let main_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    let sidebar = Rc::new(ChannelSidebar::new());

    // Put search in the title bar (left-aligned, compact)
    sidebar.search_entry.set_hexpand(false);
    sidebar.search_entry.set_width_chars(20);
    header_bar.pack_start(&sidebar.search_entry);
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

    window.set_child(Some(&main_box));

    let state = Rc::new(RefCell::new(AppState {
        channels: Vec::new(),
        user_names: HashMap::new(),
        self_user_id: user_id.clone(),
        current_channel: None,
        current_thread: None,
        unread_counts: HashMap::new(),
        pending_scroll: None,
        pending_thread: None,
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
                    emoji_entry.set_text(emoji);
                }

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
                        let gbytes = gtk4::glib::Bytes::from(&bytes);
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

    // ── Status save/clear buttons ──
    {
        let client_save = client.clone();
        let rt_save = rt.clone();
        let db_save = db.clone();
        let popover_save = popover.clone();
        let emoji_save = emoji_entry.clone();
        let status_save = status_entry.clone();
        let rebuild_save = rebuild_recent.clone();
        save_btn.connect_clicked(move |_| {
            let text = status_save.text().to_string();
            let emoji = emoji_save.text().to_string();
            let client = client_save.clone();
            let rt = rt_save.clone();
            let db = db_save.clone();
            let popover = popover_save.clone();
            let rebuild = rebuild_save.clone();
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
                }
                popover.popdown();
            });
        });

        let client_clear = client.clone();
        let rt_clear = rt.clone();
        let popover_clear = popover.clone();
        let emoji_clear = emoji_entry.clone();
        let status_clear = status_entry.clone();
        clear_btn.connect_clicked(move |_| {
            let client = client_clear.clone();
            let rt = rt_clear.clone();
            let popover = popover_clear.clone();
            let emoji_entry = emoji_clear.clone();
            let status_entry = status_clear.clone();
            gtk4::glib::spawn_future_local(async move {
                let result = rt
                    .spawn(async move { client.set_user_status("", "").await })
                    .await
                    .unwrap();
                if let Err(e) = result {
                    tracing::error!("Failed to clear status: {e}");
                }
                status_entry.set_text("");
                emoji_entry.set_text("");
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
        let message_view_tc = message_view.clone();
        let cb: crate::ui::message_view::ThreadOpenCallback =
            Rc::new(move |thread_ts: &str, channel_id: &str| {
                let tp = thread_panel.clone();
                let state = state.clone();
                let client = client.clone();
                let rt = rt.clone();
                let message_view = message_view_tc.clone();
                let thread_ts = thread_ts.to_string();
                let channel_id = channel_id.to_string();

                state.borrow_mut().current_thread =
                    Some((thread_ts.clone(), channel_id.clone()));

                tp.set_channel_id(&channel_id);
                tp.clear();
                tp.show();

                let mv = message_view.clone();
                gtk4::glib::spawn_future_local(async move {
                    let c = client.clone();
                    let cid = channel_id.clone();
                    let tts = thread_ts.clone();
                    let tts2 = tts.clone();
                    let result = rt
                        .spawn(async move { c.conversation_replies(&cid, &tts).await })
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
        message_view.set_mention_callback(mention_cb);
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
                            };
                            if let Err(e) = result {
                                tracing::error!("Channel action failed: {e}");
                            }
                        });

                        // Update cached channels
                        let db2 = db2.clone();
                        let channels = state2.borrow().channels.clone();
                        rt2.spawn(async move {
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
                            // Delete via API and update DB cache
                            let c = c.clone();
                            let db2 = db2.clone();
                            let cid = cid.clone();
                            let ts = ts.clone();
                            rt2.spawn(async move {
                                if let Err(e) = c.delete_message(&cid, &ts).await {
                                    tracing::error!("Failed to delete message: {e}");
                                }
                                // Remove from cached messages
                                if let Some(mut msgs) = db2.load_messages(&cid).await {
                                    msgs.retain(|m| m.ts != ts);
                                    let _ = db2.save_messages(&cid, &msgs).await;
                                }
                            });
                        }
                    },
                );
            });
        message_view.set_delete_callback(delete_cb.clone());
        thread_panel.set_delete_callback(delete_cb);
        thread_panel.set_self_user_id(&user_id);
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
            if text.trim().is_empty() {
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
                let result = rt2
                    .spawn(async move {
                        c.post_message(&cid, &txt, Some(&tts)).await
                    })
                    .await
                    .unwrap();

                if let Err(e) = result {
                    tracing::error!("Failed to send thread reply: {e}");
                    return;
                }

                // Append the sent message locally
                let msg = Message {
                    msg_type: "message".into(),
                    user: None,
                    bot_id: None,
                    text,
                    ts: String::new(),
                    thread_ts: Some(thread_ts),
                    channel: Some(channel_id),
                    attachments: None,
                    reactions: None,
                    files: None,
                };
                let users = state2.borrow().user_names.clone();
                tp.append_message(&msg, &users, &client2, &rt3);
            });
        };

        let do_reply_btn = do_reply.clone();
        let tp_btn = thread_panel.clone();
        tp_btn.send_button.connect_clicked(move |_| {
            do_reply_btn();
        });

        let key_controller = gtk::EventControllerKey::new();
        let do_reply_key = do_reply.clone();
        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            if key == gtk4::gdk::Key::Return
                && modifier.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
            {
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
        let presence_tx = presence_tx.clone();
        gtk4::glib::spawn_future_local(async move {
            // Load cached data in parallel: channels, users, last channel, activity
            let (cached_channels, cached_users, last_channel, activity) = {
                let db_ch = db.clone();
                let db_us = db.clone();
                let db_lc = db.clone();
                let db_act = db.clone();
                let rt2 = rt.clone();
                rt2.spawn(async move {
                    let ch = db_ch.load_channels().await;
                    let us = db_us.load_users().await;
                    let lc = db_lc.load_last_channel().await;
                    let act = db_act.load_all_channel_activity().await;
                    (ch, us, lc, act)
                })
                .await
                .unwrap()
            };

            // Apply activity data
            sidebar.set_activity(activity);

            // Apply cached users
            if let Some(users) = cached_users {
                let names: HashMap<String, String> = users
                    .into_iter()
                    .map(|u| {
                        let name = user_display_name(&u);
                        (u.id, name)
                    })
                    .collect();
                sidebar.set_user_names(&names);
                state.borrow_mut().user_names = names;
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
                let (ch_result, usr_result) = rt2
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
                        (ch, us)
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

                    let names: HashMap<String, String> = users
                        .into_iter()
                        .map(|u| {
                            let name = user_display_name(&u);
                            (u.id, name)
                        })
                        .collect();
                    sidebar.set_user_names(&names);
                    state.borrow_mut().user_names = names;

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
                }
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
            Rc::new(move |row: &gtk::ListBoxRow| {
                let channel_id = row.widget_name().to_string();
                if channel_id.is_empty() {
                    return;
                }

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
                    }
                }

                // Show cached messages immediately, then fetch fresh in background
                let mv = message_view.clone();
                let tp = thread_panel.clone();
                let state2 = state.clone();
                let client_fetch = client.clone();
                let client_img = client.clone();
                let rt2 = rt.clone();
                let rt3 = rt.clone();
                let db2 = db.clone();

                mv.spinner.set_visible(true);
                mv.spinner.start();

                gtk4::glib::spawn_future_local(async move {
                    let cid = channel_id.clone();

                    // Show cached messages first
                    let db_load = db2.clone();
                    let cid_load = cid.clone();
                    let cached = rt2
                        .spawn(async move { db_load.load_messages(&cid_load).await })
                        .await
                        .unwrap();
                    if let Some(ref messages) = cached {
                        let users = state2.borrow().user_names.clone();
                        mv.set_messages(messages, &users, &client_img, &rt3);
                    }

                    // Fetch fresh from Slack
                    let cid_fetch = cid.clone();
                    let c = client_fetch.clone();
                    let result = rt2
                        .spawn(async move { c.conversation_history(&cid_fetch, 25).await })
                        .await
                        .unwrap();

                    mv.spinner.stop();
                    mv.spinner.set_visible(false);

                    if let Ok(messages) = result {
                        let users = state2.borrow().user_names.clone();
                        mv.set_messages(&messages, &users, &client_img, &rt3);

                        // Scroll to pending message if set by notification click
                        let pending = state2.borrow_mut().pending_scroll.take();
                        if let Some(ts) = pending {
                            mv.scroll_to_message(&ts);
                        }

                        // Open pending thread if set by notification click
                        let pending_thread = state2.borrow_mut().pending_thread.take();
                        if let Some((thread_ts, reply_ts)) = pending_thread {
                            tp.pending_scroll.replace(Some(reply_ts));
                            mv.open_thread(&thread_ts, &cid);
                        }

                        // Cache the fresh messages and update activity
                        let last_ts = messages.first().map(|m| m.ts.clone());
                        let db_save = db2.clone();
                        let to_save = messages.clone();
                        let cid_save = cid.clone();
                        rt2.spawn(async move {
                            if let Err(e) = db_save.save_messages(&cid_save, &to_save).await {
                                tracing::error!("Failed to cache messages: {e}");
                            }
                            if let Some(ts) = last_ts {
                                db_save.update_channel_activity(&cid_save, &ts).await;
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
        action.connect_activate(move |_, param| {
            let Some(param) = param.and_then(|p| p.get::<String>()) else { return };
            let parts: Vec<&str> = param.splitn(3, ':').collect();
            if parts.len() < 2 { return; }
            let channel_id = parts[0];
            let message_ts = parts[1];
            // Third part is thread_ts if the message is a thread reply
            let thread_ts = parts.get(2).copied();

            // Raise the window
            window.present();

            let current = state.borrow().current_channel.clone();
            let needs_channel_switch = current.as_deref() != Some(channel_id);

            if needs_channel_switch {
                // Set pending state, then switch channel
                if let Some(tts) = thread_ts {
                    state.borrow_mut().pending_thread = Some((tts.to_string(), message_ts.to_string()));
                } else {
                    state.borrow_mut().pending_scroll = Some(message_ts.to_string());
                }
                sidebar.select_channel_by_id(channel_id);
            } else if let Some(tts) = thread_ts {
                // Already on the channel — open thread and scroll to the reply
                message_view.open_thread(tts, channel_id);
                thread_panel.scroll_to_message(message_ts);
            } else {
                message_view.scroll_to_message(message_ts);
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

    // ── Send message ──
    {
        let state_send = state.clone();
        let input = message_input.clone();
        let rt_send = rt.clone();
        let client_send = client.clone();

        let do_send = move || {
            let text = input.get_text();
            if text.trim().is_empty() {
                return;
            }

            let channel = match state_send.borrow().current_channel.clone() {
                Some(c) => c,
                None => return,
            };

            input.clear();

            let client = client_send.clone();
            let rt2 = rt_send.clone();
            gtk4::glib::spawn_future_local(async move {
                let _ = rt2
                    .spawn(async move {
                        let _ = client.post_message(&channel, &text, None).await;
                    })
                    .await;
            });
        };

        // Send button click
        let do_send_clone = do_send.clone();
        message_input.send_button.connect_clicked(move |_| {
            do_send_clone();
        });

        // Ctrl+Enter to send
        let key_controller = gtk::EventControllerKey::new();
        let do_send_key = do_send.clone();
        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            if key == gtk4::gdk::Key::Return
                && modifier.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
            {
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
                            files: None,
                        };

                        let current = state.borrow().current_channel.clone();
                        let current_thread = state.borrow().current_thread.clone();

                        // Append to thread panel if the message belongs to the open thread
                        if let Some((tts, tcid)) = &current_thread {
                            if *tcid == channel
                                && thread_ts.as_deref() == Some(tts.as_str())
                            {
                                let users = state.borrow().user_names.clone();
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
                        // (skip thread replies that aren't top-level)
                        if is_current {
                            let is_thread_reply = thread_ts
                                .as_ref()
                                .is_some_and(|tts| *tts != msg.ts);
                            if !is_thread_reply {
                                let users = state.borrow().user_names.clone();
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

                        // Cache the incoming message and update activity
                        {
                            let db2 = db.clone();
                            let msg2 = msg.clone();
                            let cid = channel.clone();
                            let ts = msg.ts.clone();
                            rt_rt.spawn(async move {
                                db2.append_message(&cid, &msg2).await;
                                db2.update_channel_activity(&cid, &ts).await;
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
                            let plain_text = format_message_plain(&text, &st.user_names);
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
                                let mut child = match tokio::process::Command::new("notify-send")
                                    .arg("--app-name=Slack")
                                    .arg("--urgency=normal")
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
                        let is_self = user.is_empty() || state.borrow().self_user_id == user;
                        if is_self {
                            *presence_active.borrow_mut() = is_active;
                            profile_avatar.queue_draw();
                        }
                        // Resolve effective user ID (manual_presence_change has empty user)
                        let effective_uid = if user.is_empty() {
                            state.borrow().self_user_id.clone()
                        } else {
                            user.clone()
                        };
                        sidebar.set_presence(&effective_uid, is_active);
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
                            state
                                .borrow_mut()
                                .user_names
                                .insert(user.clone(), display_name.to_string());
                        }

                        // Only update our own profile UI
                        if is_self {
                            if let Some(status_text) =
                                profile.get("status_text").and_then(|v| v.as_str())
                            {
                                emoji_entry_rt.set_text(
                                    profile
                                        .get("status_emoji")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(""),
                                );
                                status_entry_rt.set_text(status_text);
                            }

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
                                    let gbytes = gtk4::glib::Bytes::from(&bytes);
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
                        let state2 = state.clone();

                        // Update DB and refresh UI
                        let updated = rt_rt.spawn(async move {
                            db2.update_reaction(&channel2, &message_ts2, &reaction2, &user2, added).await
                        }).await;

                        if let Ok(Some(reactions)) = updated {
                            let is_current = state2.borrow().current_channel.as_deref() == Some(&channel);
                            if is_current {
                                let users = state2.borrow().user_names.clone();
                                message_view2.update_reactions(&message_ts, &reactions, &users);
                            }
                        }
                    }
                    SlackEvent::UserTyping { channel, user, thread_ts: _ } => {
                        let current = state.borrow().current_channel.clone();
                        if current.as_deref() == Some(&channel) {
                            let name = state.borrow().user_names.get(&user).cloned()
                                .unwrap_or_else(|| "Someone".into());
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

    // Focus search entry when window gains focus
    {
        let search = sidebar.search_entry.clone();
        window.connect_is_active_notify(move |win| {
            if win.is_active() {
                search.grab_focus();
            }
        });
    }

    // Apply CSS
    load_css();

    window.present();
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


use gtk4::prelude::*;
use gtk4::{self as gtk, Label, ListBox, ListBoxRow, ScrolledWindow, SearchEntry};
use slacko::types::Channel;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::slack::helpers::channel_display_name;

/// Channel actions triggered from context menus.
#[derive(Debug, Clone)]
pub enum ChannelAction {
    Leave,
    Archive,
    Close,
    /// Start watching a user's presence (user_id).
    WatchPresence(String),
    /// Stop watching a user's presence (user_id).
    UnwatchPresence(String),
}

/// Callback type for channel actions: (action, channel_id)
pub type ChannelActionCallback = Rc<dyn Fn(ChannelAction, &str)>;

/// Callback type for the "create group" button.
pub type CreateGroupCallback = Rc<dyn Fn()>;

/// Callback type for the "create channel" button.
pub type CreateChannelCallback = Rc<dyn Fn()>;

pub struct ChannelSidebar {
    pub widget: gtk::Box,
    pub channels_list: ListBox,
    pub dm_list: ListBox,
    pub group_list: ListBox,
    pub search_entry: SearchEntry,
    channels: Rc<RefCell<Vec<Channel>>>,
    dms: Rc<RefCell<Vec<Channel>>>,
    groups: Rc<RefCell<Vec<Channel>>>,
    badges: Rc<RefCell<HashMap<String, Label>>>,
    display_names: Rc<RefCell<HashMap<String, String>>>,
    user_names: Rc<RefCell<HashMap<String, String>>>,
    /// Last activity timestamp per channel ID (for sorting by recency).
    activity: Rc<RefCell<HashMap<String, String>>>,
    /// Presence indicator labels keyed by user ID (green/hollow circle).
    presence_icons: Rc<RefCell<HashMap<String, Label>>>,
    /// Status emoji containers keyed by user ID (holds Label or Image).
    status_icons: Rc<RefCell<HashMap<String, gtk::Box>>>,
    /// Presence state keyed by user ID.
    presence_state: Rc<RefCell<HashMap<String, bool>>>,
    /// DM rows keyed by user ID (for showing/hiding based on presence).
    dm_rows: Rc<RefCell<HashMap<String, ListBoxRow>>>,
    /// Whether to show only online DM users.
    show_online_only: Rc<RefCell<bool>>,
    /// Callback for channel context menu actions.
    action_callback: Rc<RefCell<Option<ChannelActionCallback>>>,
    /// User IDs being watched for presence notifications.
    watched_users: Rc<RefCell<std::collections::HashSet<String>>>,
    /// Watch indicator labels keyed by user ID (for toggling eyes emoji).
    watch_labels: Rc<RefCell<HashMap<String, Label>>>,
    /// User status emoji keyed by user ID (Slack shortcode, e.g. ":coffee:").
    status_emoji: Rc<RefCell<HashMap<String, String>>>,
    /// User status text keyed by user ID.
    status_text: Rc<RefCell<HashMap<String, String>>>,
    /// Callback for the "create group" button.
    create_group_callback: Rc<RefCell<Option<CreateGroupCallback>>>,
    /// Callback for the "create channel" button.
    create_channel_callback: Rc<RefCell<Option<CreateChannelCallback>>>,
}

impl ChannelSidebar {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_width_request(240);
        container.add_css_class("sidebar");

        let search_entry = SearchEntry::new();
        search_entry.set_placeholder_text(Some("Filter channels..."));
        search_entry.set_margin_start(8);
        search_entry.set_margin_end(8);
        search_entry.set_margin_top(4);
        search_entry.set_margin_bottom(4);
        search_entry.set_visible(false);

        let scrolled = ScrolledWindow::new();
        scrolled.set_vexpand(true);

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inner.append(&search_entry);

        // DM section
        let dm_header = Label::new(Some("Direct Messages"));
        dm_header.add_css_class("heading");
        dm_header.add_css_class("dim-label");
        dm_header.set_halign(gtk::Align::Start);
        dm_header.set_margin_top(8);
        dm_header.set_margin_start(12);
        dm_header.set_margin_bottom(4);
        inner.append(&dm_header);

        let dm_list = ListBox::new();
        dm_list.set_selection_mode(gtk::SelectionMode::Single);
        dm_list.add_css_class("navigation-sidebar");
        inner.append(&dm_list);

        // Channels section
        let channels_header_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        channels_header_box.set_margin_top(12);
        channels_header_box.set_margin_start(12);
        channels_header_box.set_margin_end(8);
        channels_header_box.set_margin_bottom(4);

        let channels_header = Label::new(Some("Channels"));
        channels_header.add_css_class("heading");
        channels_header.add_css_class("dim-label");
        channels_header.set_halign(gtk::Align::Start);
        channels_header.set_hexpand(true);
        channels_header_box.append(&channels_header);

        let create_channel_callback: Rc<RefCell<Option<CreateChannelCallback>>> =
            Rc::new(RefCell::new(None));
        let create_channel_btn = gtk::Button::from_icon_name("list-add-symbolic");
        create_channel_btn.add_css_class("flat");
        create_channel_btn.add_css_class("dim-label");
        create_channel_btn.set_tooltip_text(Some("New channel"));
        let ccc = create_channel_callback.clone();
        create_channel_btn.connect_clicked(move |_| {
            if let Some(cb) = ccc.borrow().as_ref() {
                cb();
            }
        });
        channels_header_box.append(&create_channel_btn);

        inner.append(&channels_header_box);

        let channels_list = ListBox::new();
        channels_list.set_selection_mode(gtk::SelectionMode::Single);
        channels_list.add_css_class("navigation-sidebar");
        inner.append(&channels_list);

        // Groups section
        let group_header_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        group_header_box.set_margin_top(12);
        group_header_box.set_margin_start(12);
        group_header_box.set_margin_end(8);
        group_header_box.set_margin_bottom(4);

        let group_header = Label::new(Some("Groups"));
        group_header.add_css_class("heading");
        group_header.add_css_class("dim-label");
        group_header.set_halign(gtk::Align::Start);
        group_header.set_hexpand(true);
        group_header_box.append(&group_header);

        let create_group_callback: Rc<RefCell<Option<CreateGroupCallback>>> =
            Rc::new(RefCell::new(None));
        let create_group_btn = gtk::Button::from_icon_name("list-add-symbolic");
        create_group_btn.add_css_class("flat");
        create_group_btn.add_css_class("dim-label");
        create_group_btn.set_tooltip_text(Some("New group chat"));
        let cgc = create_group_callback.clone();
        create_group_btn.connect_clicked(move |_| {
            if let Some(cb) = cgc.borrow().as_ref() {
                cb();
            }
        });
        group_header_box.append(&create_group_btn);

        inner.append(&group_header_box);

        let group_list = ListBox::new();
        group_list.set_selection_mode(gtk::SelectionMode::Single);
        group_list.add_css_class("navigation-sidebar");
        inner.append(&group_list);

        scrolled.set_child(Some(&inner));
        container.append(&scrolled);

        let channels: Rc<RefCell<Vec<Channel>>> = Rc::new(RefCell::new(Vec::new()));
        let dms: Rc<RefCell<Vec<Channel>>> = Rc::new(RefCell::new(Vec::new()));
        let groups: Rc<RefCell<Vec<Channel>>> = Rc::new(RefCell::new(Vec::new()));
        let badges: Rc<RefCell<HashMap<String, Label>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let display_names: Rc<RefCell<HashMap<String, String>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let user_names: Rc<RefCell<HashMap<String, String>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let activity: Rc<RefCell<HashMap<String, String>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let presence_icons: Rc<RefCell<HashMap<String, Label>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let status_icons: Rc<RefCell<HashMap<String, gtk::Box>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let presence_state: Rc<RefCell<HashMap<String, bool>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let dm_rows: Rc<RefCell<HashMap<String, ListBoxRow>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let show_online_only: Rc<RefCell<bool>> = Rc::new(RefCell::new(true));
        let action_callback: Rc<RefCell<Option<ChannelActionCallback>>> =
            Rc::new(RefCell::new(None));

        // Deselect other lists when one is selected
        {
            let all_lists = [channels_list.clone(), dm_list.clone(), group_list.clone()];
            for (i, list) in all_lists.iter().enumerate() {
                let others: Vec<ListBox> = all_lists
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, l)| l.clone())
                    .collect();
                list.connect_row_selected(move |_, row| {
                    if row.is_some() {
                        for other in &others {
                            other.unselect_all();
                        }
                    }
                });
            }
        }

        // Filter and sort on search
        let channels_clone = channels.clone();
        let dms_clone = dms.clone();
        let groups_clone = groups.clone();
        let ch_list_clone = channels_list.clone();
        let dm_list_clone = dm_list.clone();
        let gr_list_clone = group_list.clone();
        let user_names_clone = user_names.clone();
        let dn_filter = display_names.clone();
        let presence_state_filter = presence_state.clone();
        let show_online_filter = show_online_only.clone();
        let activity_filter = activity.clone();
        let scrolled_filter = scrolled.clone();
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_lowercase();
            let chs = channels_clone.borrow();
            let dms = dms_clone.borrow();
            let grs = groups_clone.borrow();
            let names = user_names_clone.borrow();
            let act = activity_filter.borrow();
            Self::filter_list(&ch_list_clone, &chs, &query, &names, &act);
            Self::filter_dm_list(
                &dm_list_clone, &dms, &query, &names,
                &presence_state_filter.borrow(),
                *show_online_filter.borrow(),
                &act,
            );
            Self::filter_list(&gr_list_clone, &grs, &query, &names, &act);

            if query.is_empty() {
                // Restore activity-based sort (most recent first, then alphabetical)
                for list in [&ch_list_clone, &dm_list_clone, &gr_list_clone] {
                    let act2 = activity_filter.clone();
                    let dn2 = dn_filter.clone();
                    list.set_sort_func(move |a, b| {
                        let act = act2.borrow();
                        let dn = dn2.borrow();
                        let a_id = a.widget_name().to_string();
                        let b_id = b.widget_name().to_string();
                        let a_ts = act.get(&a_id).map(|s| s.as_str()).unwrap_or("0");
                        let b_ts = act.get(&b_id).map(|s| s.as_str()).unwrap_or("0");
                        let a_name = dn.get(&a_id).cloned().unwrap_or_default();
                        let b_name = dn.get(&b_id).cloned().unwrap_or_default();
                        match b_ts.cmp(a_ts).then_with(|| a_name.cmp(&b_name)) {
                            std::cmp::Ordering::Less => gtk::Ordering::Smaller,
                            std::cmp::Ordering::Equal => gtk::Ordering::Equal,
                            std::cmp::Ordering::Greater => gtk::Ordering::Larger,
                        }
                    });
                }
            } else {
                for list in [&ch_list_clone, &dm_list_clone, &gr_list_clone] {
                    let dn = dn_filter.clone();
                    let q = query.clone();
                    list.set_sort_func(move |a, b| {
                        Self::search_cmp(a, b, &q, &dn.borrow())
                    });
                }
            }
            ch_list_clone.invalidate_sort();
            dm_list_clone.invalidate_sort();
            gr_list_clone.invalidate_sort();

            // Scroll to top so first results are visible
            scrolled_filter.vadjustment().set_value(0.0);
        });

        // Down arrow moves focus into the first visible row across all lists
        let all_down = [channels_list.clone(), dm_list.clone(), group_list.clone()];
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gtk4::gdk::Key::Down {
                for list in &all_down {
                    if let Some(row) = Self::first_visible_row(list) {
                        list.select_row(Some(&row));
                        row.grab_focus();
                        return gtk4::glib::Propagation::Stop;
                    }
                }
            }
            gtk4::glib::Propagation::Proceed
        });
        search_entry.add_controller(key_controller);

        // Enter selects the first visible row across all lists
        let all_enter = [channels_list.clone(), dm_list.clone(), group_list.clone()];
        let search_entry_enter = search_entry.clone();
        search_entry.connect_activate(move |_| {
            for list in &all_enter {
                if let Some(row) = Self::first_visible_row(list) {
                    list.select_row(Some(&row));
                    row.activate();
                    search_entry_enter.set_text("");
                    return;
                }
            }
        });

        Self {
            widget: container,
            channels_list,
            dm_list,
            group_list,
            search_entry,
            channels,
            dms,
            groups,
            badges,
            display_names,
            user_names,
            activity,
            presence_icons,
            status_icons,
            presence_state,
            dm_rows,
            show_online_only,
            action_callback,
            watched_users: Rc::new(RefCell::new(std::collections::HashSet::new())),
            watch_labels: Rc::new(RefCell::new(HashMap::new())),
            status_emoji: Rc::new(RefCell::new(HashMap::new())),
            status_text: Rc::new(RefCell::new(HashMap::new())),
            create_group_callback,
            create_channel_callback,
        }
    }

    pub fn set_action_callback(&self, cb: ChannelActionCallback) {
        *self.action_callback.borrow_mut() = Some(cb);
    }

    pub fn set_create_group_callback(&self, cb: CreateGroupCallback) {
        *self.create_group_callback.borrow_mut() = Some(cb);
    }

    pub fn set_create_channel_callback(&self, cb: CreateChannelCallback) {
        *self.create_channel_callback.borrow_mut() = Some(cb);
    }

    /// Set the list of user IDs being watched for presence notifications.
    pub fn set_watched_users(&self, users: std::collections::HashSet<String>) {
        *self.watched_users.borrow_mut() = users;
    }

    /// Check if a user is being watched for presence notifications.
    pub fn is_watched(&self, user_id: &str) -> bool {
        self.watched_users.borrow().contains(user_id)
    }

    /// Set channel activity timestamps (channel_id -> Slack ts).
    pub fn set_activity(&self, activity: HashMap<String, String>) {
        *self.activity.borrow_mut() = activity;
    }

    /// Update activity for a single channel (e.g. from a socket message).
    /// Makes the channel row visible if it was previously hidden due to inactivity.
    pub fn update_activity(&self, channel_id: &str, ts: &str) {
        self.activity.borrow_mut().insert(channel_id.to_string(), ts.to_string());

        // Make the row visible in case it was hidden as inactive
        for list in [&self.channels_list, &self.dm_list, &self.group_list] {
            let mut idx = 0;
            while let Some(row) = list.row_at_index(idx) {
                if row.widget_name() == channel_id {
                    row.set_visible(true);
                    return;
                }
                idx += 1;
            }
        }
    }

    pub fn set_user_names(&self, names: &HashMap<String, String>) {
        *self.user_names.borrow_mut() = names.clone();
    }

    /// Detect mpdm (multi-party DM) channels by name prefix.
    fn is_mpdm(ch: &Channel) -> bool {
        ch.name
            .as_deref()
            .is_some_and(|n| n.starts_with("mpdm-"))
    }

    /// Format an mpdm channel name by resolving user names from the `mpdm-user1--user2--...-N` pattern.
    fn group_display_name(channel: &Channel, user_names: &HashMap<String, String>) -> String {
        if let Some(name) = &channel.name {
            if let Some(rest) = name.strip_prefix("mpdm-") {
                // Strip trailing `-N` (the numeric suffix)
                let without_suffix = if let Some(pos) = rest.rfind('-') {
                    if rest[pos + 1..].chars().all(|c| c.is_ascii_digit()) {
                        &rest[..pos]
                    } else {
                        rest
                    }
                } else {
                    rest
                };

                let parts: Vec<&str> = without_suffix.split("--").collect();
                let resolved: Vec<String> = parts
                    .iter()
                    .map(|part| {
                        // Try to find a user whose name matches this part
                        user_names
                            .values()
                            .find(|v| {
                                v.to_lowercase().replace(' ', ".") == part.to_lowercase()
                                    || v.to_lowercase() == part.to_lowercase()
                            })
                            .cloned()
                            .unwrap_or_else(|| (*part).to_string())
                    })
                    .collect();

                return resolved.join(", ");
            }
        }
        channel_display_name(channel)
    }

    /// Returns the 2-week activity cutoff as a Slack-style timestamp string.
    fn activity_cutoff() -> String {
        let two_weeks_ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_sub(14 * 24 * 60 * 60);
        format!("{two_weeks_ago}")
    }

    /// Whether a channel has recent activity (within 2 weeks).
    fn is_recent(activity: &HashMap<String, String>, id: &str, cutoff: &str) -> bool {
        match activity.get(id) {
            Some(ts) => ts.as_str() >= cutoff,
            None => false,
        }
    }

    pub fn set_channels(&self, all: &[Channel]) {
        let names = self.user_names.borrow();

        let act = self.activity.borrow();
        let cutoff = Self::activity_cutoff();

        let mut ch_list: Vec<Channel> = Vec::new();
        let mut dm_list: Vec<Channel> = Vec::new();
        let mut gr_list: Vec<Channel> = Vec::new();

        for ch in all {
            if Self::is_mpdm(ch) {
                gr_list.push(ch.clone());
            } else if ch.is_im == Some(true) {
                dm_list.push(ch.clone());
            } else if ch.is_group == Some(true) {
                // Private channels that aren't mpdm go with channels
                ch_list.push(ch.clone());
            } else {
                ch_list.push(ch.clone());
            }
        }

        // Sort by last activity (most recent first), then alphabetical as tiebreaker
        let activity_sort = |a_id: &str, b_id: &str, a_name: &str, b_name: &str| -> std::cmp::Ordering {
            let a_ts = act.get(a_id).map(|s| s.as_str()).unwrap_or("0");
            let b_ts = act.get(b_id).map(|s| s.as_str()).unwrap_or("0");
            // Compare timestamps as strings (Slack ts are epoch-based, lexicographic works)
            b_ts.cmp(a_ts)
                .then_with(|| a_name.to_lowercase().cmp(&b_name.to_lowercase()))
        };

        ch_list.sort_by(|a, b| {
            activity_sort(
                &a.id, &b.id,
                &channel_display_name(a),
                &channel_display_name(b),
            )
        });
        dm_list.sort_by(|a, b| {
            activity_sort(
                &a.id, &b.id,
                &Self::dm_display_name(a, &names),
                &Self::dm_display_name(b, &names),
            )
        });
        gr_list.sort_by(|a, b| {
            activity_sort(
                &a.id, &b.id,
                &Self::group_display_name(a, &names),
                &Self::group_display_name(b, &names),
            )
        });

        // Rebuild channels list
        while let Some(child) = self.channels_list.first_child() {
            self.channels_list.remove(&child);
        }
        let mut new_badges = HashMap::new();
        let acb = self.action_callback.borrow().clone();
        let watched = &self.watched_users;
        for ch in &ch_list {
            let (row, badge) = Self::make_channel_row(ch);
            row.set_visible(Self::is_recent(&act, &ch.id, &cutoff));
            Self::attach_context_menu(&row, &ch.id, false, None, &acb, watched, &self.watch_labels, &self.status_emoji, &self.status_text);
            self.channels_list.append(&row);
            new_badges.insert(ch.id.clone(), badge);
        }

        // Rebuild DM list
        while let Some(child) = self.dm_list.first_child() {
            self.dm_list.remove(&child);
        }
        let mut new_presence_icons = HashMap::new();
        let mut new_status_icons = HashMap::new();
        let mut new_dm_rows = HashMap::new();
        let mut new_watch_labels = HashMap::new();
        let online_only = *self.show_online_only.borrow();
        let presence = self.presence_state.borrow();
        let watched_set = self.watched_users.borrow();
        for ch in &dm_list {
            let user_watched = ch.user.as_ref()
                .is_some_and(|uid| watched_set.contains(uid));
            let (row, badge, presence_lbl, status_box, watch_label) = Self::make_dm_row(ch, &names, user_watched);
            Self::attach_context_menu(&row, &ch.id, true, ch.user.as_deref(), &acb, watched, &self.watch_labels, &self.status_emoji, &self.status_text);
            // Show/hide based on activity recency and presence state
            if !Self::is_recent(&act, &ch.id, &cutoff) {
                row.set_visible(false);
            } else if online_only {
                let is_active = ch.user.as_ref()
                    .and_then(|uid| presence.get(uid))
                    .copied()
                    .unwrap_or(false);
                row.set_visible(is_active);
            }
            self.dm_list.append(&row);
            new_badges.insert(ch.id.clone(), badge);
            if let Some(uid) = &ch.user {
                new_presence_icons.insert(uid.clone(), presence_lbl);
                new_status_icons.insert(uid.clone(), status_box);
                new_dm_rows.insert(uid.clone(), row);
                new_watch_labels.insert(uid.clone(), watch_label);
            }
        }
        drop(watched_set);
        drop(presence);

        // Rebuild group list
        while let Some(child) = self.group_list.first_child() {
            self.group_list.remove(&child);
        }
        for ch in &gr_list {
            let (row, badge) = Self::make_group_row(ch, &names);
            row.set_visible(Self::is_recent(&act, &ch.id, &cutoff));
            Self::attach_context_menu(&row, &ch.id, true, None, &acb, watched, &self.watch_labels, &self.status_emoji, &self.status_text);
            self.group_list.append(&row);
            new_badges.insert(ch.id.clone(), badge);
        }

        // Build display name lookup for search sorting
        let mut dn = HashMap::new();
        for ch in &ch_list {
            dn.insert(ch.id.clone(), channel_display_name(ch).to_lowercase());
        }
        for ch in &dm_list {
            dn.insert(
                ch.id.clone(),
                Self::dm_display_name(ch, &names).to_lowercase(),
            );
        }
        for ch in &gr_list {
            dn.insert(
                ch.id.clone(),
                Self::group_display_name(ch, &names).to_lowercase(),
            );
        }

        *self.display_names.borrow_mut() = dn;
        *self.badges.borrow_mut() = new_badges;
        *self.presence_icons.borrow_mut() = new_presence_icons;
        *self.status_icons.borrow_mut() = new_status_icons;
        *self.dm_rows.borrow_mut() = new_dm_rows;
        *self.watch_labels.borrow_mut() = new_watch_labels;
        *self.channels.borrow_mut() = ch_list;
        *self.dms.borrow_mut() = dm_list;
        *self.groups.borrow_mut() = gr_list;
        self.search_entry.set_text("");
    }

    /// Add a single channel to the sidebar (e.g. when the user is added to a new channel at runtime).
    /// Returns true if the channel was added, false if it already exists.
    pub fn add_channel(&self, channel: &Channel) -> bool {
        // Check if it already exists in any list
        let id = &channel.id;
        if self.channels.borrow().iter().any(|c| c.id == *id)
            || self.dms.borrow().iter().any(|c| c.id == *id)
            || self.groups.borrow().iter().any(|c| c.id == *id)
        {
            return false;
        }

        let names = self.user_names.borrow();
        let acb = self.action_callback.borrow().clone();
        let watched = &self.watched_users;

        if Self::is_mpdm(channel) {
            let (row, badge) = Self::make_group_row(channel, &names);
            row.set_visible(true);
            Self::attach_context_menu(&row, id, true, None, &acb, watched, &self.watch_labels, &self.status_emoji, &self.status_text);
            self.group_list.append(&row);
            self.badges.borrow_mut().insert(id.clone(), badge);
            self.display_names.borrow_mut().insert(
                id.clone(),
                Self::group_display_name(channel, &names).to_lowercase(),
            );
            self.groups.borrow_mut().push(channel.clone());
        } else if channel.is_im == Some(true) {
            let user_watched = channel.user.as_ref()
                .is_some_and(|uid| self.watched_users.borrow().contains(uid));
            let (row, badge, presence_lbl, status_box, watch_label) =
                Self::make_dm_row(channel, &names, user_watched);
            row.set_visible(true);
            Self::attach_context_menu(&row, id, true, channel.user.as_deref(), &acb, watched, &self.watch_labels, &self.status_emoji, &self.status_text);
            self.dm_list.append(&row);
            self.badges.borrow_mut().insert(id.clone(), badge);
            self.display_names.borrow_mut().insert(
                id.clone(),
                Self::dm_display_name(channel, &names).to_lowercase(),
            );
            if let Some(uid) = &channel.user {
                self.presence_icons.borrow_mut().insert(uid.clone(), presence_lbl);
                self.status_icons.borrow_mut().insert(uid.clone(), status_box);
                self.dm_rows.borrow_mut().insert(uid.clone(), row);
                self.watch_labels.borrow_mut().insert(uid.clone(), watch_label);
            }
            self.dms.borrow_mut().push(channel.clone());
        } else {
            let (row, badge) = Self::make_channel_row(channel);
            row.set_visible(true);
            Self::attach_context_menu(&row, id, false, None, &acb, watched, &self.watch_labels, &self.status_emoji, &self.status_text);
            self.channels_list.append(&row);
            self.badges.borrow_mut().insert(id.clone(), badge);
            self.display_names.borrow_mut().insert(
                id.clone(),
                channel_display_name(channel).to_lowercase(),
            );
            self.channels.borrow_mut().push(channel.clone());
        }

        true
    }

    fn attach_context_menu(
        row: &ListBoxRow,
        channel_id: &str,
        is_dm: bool,
        user_id: Option<&str>,
        action_cb: &Option<ChannelActionCallback>,
        watched_users: &Rc<RefCell<std::collections::HashSet<String>>>,
        watch_labels: &Rc<RefCell<HashMap<String, Label>>>,
        status_emoji: &Rc<RefCell<HashMap<String, String>>>,
        status_text: &Rc<RefCell<HashMap<String, String>>>,
    ) {
        let Some(acb) = action_cb.clone() else { return };

        let popover = gtk::Popover::new();
        popover.set_parent(row);
        popover.set_autohide(true);

        // Unparent popover when row is destroyed to avoid leak
        let popover_weak = popover.downgrade();
        row.connect_destroy(move |_| {
            if let Some(p) = popover_weak.upgrade() {
                p.unparent();
            }
        });

        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        let cid = channel_id.to_string();
        let uid = user_id.map(|s| s.to_string());
        let watched = watched_users.clone();
        let wl = watch_labels.clone();
        let se = status_emoji.clone();
        let st = status_text.clone();
        let popover_weak = popover.downgrade();
        gesture.connect_released(move |_, _, _, _| {
            let Some(popover) = popover_weak.upgrade() else { return };

            // Rebuild menu content each time so labels reflect current state
            let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

            // Show user status as the first row for DM context menus
            if is_dm {
                if let Some(uid) = &uid {
                    let emoji = se.borrow().get(uid.as_str()).cloned().unwrap_or_default();
                    let text = st.borrow().get(uid.as_str()).cloned().unwrap_or_default();
                    if !emoji.is_empty() || !text.is_empty() {
                        let emoji_display = if !emoji.is_empty() {
                            Self::shortcode_to_display(&emoji)
                        } else {
                            String::new()
                        };
                        let status_str = format!("{emoji_display} {text}").trim().to_string();
                        let status_label = Label::new(Some(&status_str));
                        status_label.set_halign(gtk::Align::Start);
                        status_label.set_margin_start(8);
                        status_label.set_margin_end(8);
                        status_label.set_margin_top(4);
                        status_label.set_margin_bottom(4);
                        status_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                        status_label.set_max_width_chars(30);
                        status_label.add_css_class("dim-label");
                        menu_box.append(&status_label);

                        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
                        sep.set_margin_top(2);
                        sep.set_margin_bottom(2);
                        menu_box.append(&sep);
                    }
                }
            }

            if is_dm {
                {
                    let close_btn = gtk::Button::with_label("Close conversation");
                    close_btn.add_css_class("flat");
                    let cid = cid.clone();
                    let acb2 = acb.clone();
                    let pop = popover.clone();
                    close_btn.connect_clicked(move |_| {
                        acb2(ChannelAction::Close, &cid);
                        pop.popdown();
                    });
                    menu_box.append(&close_btn);
                }

                // Watch/unwatch presence
                if let Some(uid) = &uid {
                    let is_watched = watched.borrow().contains(uid);
                    let watch_btn = if is_watched {
                        gtk::Button::with_label("Stop watching presence")
                    } else {
                        gtk::Button::with_label("Notify when online")
                    };
                    watch_btn.add_css_class("flat");
                    let cid = cid.clone();
                    let uid = uid.clone();
                    let acb2 = acb.clone();
                    let pop = popover.clone();
                    let watched = watched.clone();
                    let wl = wl.clone();
                    watch_btn.connect_clicked(move |_| {
                        let currently_watched = watched.borrow().contains(&uid);
                        if currently_watched {
                            watched.borrow_mut().remove(&uid);
                            if let Some(lbl) = wl.borrow().get(&uid) {
                                lbl.set_visible(false);
                            }
                            acb2(ChannelAction::UnwatchPresence(uid.clone()), &cid);
                        } else {
                            watched.borrow_mut().insert(uid.clone());
                            if let Some(lbl) = wl.borrow().get(&uid) {
                                lbl.set_visible(true);
                            }
                            acb2(ChannelAction::WatchPresence(uid.clone()), &cid);
                        }
                        pop.popdown();
                    });
                    menu_box.append(&watch_btn);
                }
            } else {
                {
                    let leave_btn = gtk::Button::with_label("Leave channel");
                    leave_btn.add_css_class("flat");
                    let cid = cid.clone();
                    let acb2 = acb.clone();
                    let pop = popover.clone();
                    leave_btn.connect_clicked(move |_| {
                        acb2(ChannelAction::Leave, &cid);
                        pop.popdown();
                    });
                    menu_box.append(&leave_btn);
                }

                {
                    let archive_btn = gtk::Button::with_label("Archive channel");
                    archive_btn.add_css_class("flat");
                    let cid = cid.clone();
                    let acb2 = acb.clone();
                    let pop = popover.clone();
                    archive_btn.connect_clicked(move |_| {
                        acb2(ChannelAction::Archive, &cid);
                        pop.popdown();
                    });
                    menu_box.append(&archive_btn);
                }
            }

            popover.set_child(Some(&menu_box));
            popover.popup();
        });
        row.add_controller(gesture);
    }

    fn make_channel_row(channel: &Channel) -> (ListBoxRow, Label) {
        let row = ListBoxRow::new();

        let label_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        label_box.set_margin_top(4);
        label_box.set_margin_bottom(4);
        label_box.set_margin_start(12);
        label_box.set_margin_end(8);

        let prefix = if channel.is_private == Some(true) || channel.is_group == Some(true) {
            "\u{1f512}"
        } else {
            "#"
        };

        let icon_label = Label::new(Some(prefix));
        icon_label.add_css_class("dim-label");
        label_box.append(&icon_label);

        let name = channel_display_name(channel);
        let name_label = Label::new(Some(&name));
        name_label.set_halign(gtk::Align::Start);
        name_label.set_hexpand(true);
        name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label_box.append(&name_label);

        let badge = Label::new(None);
        badge.add_css_class("unread-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_visible(false);
        label_box.append(&badge);

        row.set_child(Some(&label_box));
        row.set_widget_name(&channel.id);

        (row, badge)
    }

    fn make_dm_row(
        channel: &Channel,
        user_names: &HashMap<String, String>,
        is_watched: bool,
    ) -> (ListBoxRow, Label, Label, gtk::Box, Label) {
        let row = ListBoxRow::new();

        let label_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        label_box.set_margin_top(4);
        label_box.set_margin_bottom(4);
        label_box.set_margin_start(12);
        label_box.set_margin_end(8);

        // Presence indicator (green/hollow circle)
        let presence_label = Label::new(Some("\u{25cb}"));
        presence_label.add_css_class("dim-label");
        label_box.append(&presence_label);

        let name = Self::dm_display_name(channel, user_names);
        let name_label = Label::new(Some(&name));
        name_label.set_halign(gtk::Align::Start);
        name_label.set_hexpand(true);
        name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label_box.append(&name_label);

        // Status emoji (after the name)
        let status_container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        label_box.append(&status_container);

        // Eyes emoji for watched users
        let watch_label = Label::new(Some("\u{1f440}"));
        watch_label.set_visible(is_watched);
        label_box.append(&watch_label);

        let badge = Label::new(None);
        badge.add_css_class("unread-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_visible(false);
        label_box.append(&badge);

        row.set_child(Some(&label_box));
        row.set_widget_name(&channel.id);

        (row, badge, presence_label, status_container, watch_label)
    }

    fn make_group_row(
        channel: &Channel,
        user_names: &HashMap<String, String>,
    ) -> (ListBoxRow, Label) {
        let row = ListBoxRow::new();

        let label_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        label_box.set_margin_top(4);
        label_box.set_margin_bottom(4);
        label_box.set_margin_start(12);
        label_box.set_margin_end(8);

        let icon_label = Label::new(Some("\u{1f465}"));
        icon_label.add_css_class("dim-label");
        label_box.append(&icon_label);

        let name = Self::group_display_name(channel, user_names);
        let name_label = Label::new(Some(&name));
        name_label.set_halign(gtk::Align::Start);
        name_label.set_hexpand(true);
        name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label_box.append(&name_label);

        let badge = Label::new(None);
        badge.add_css_class("unread-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_visible(false);
        label_box.append(&badge);

        row.set_child(Some(&label_box));
        row.set_widget_name(&channel.id);

        (row, badge)
    }

    fn dm_display_name(channel: &Channel, user_names: &HashMap<String, String>) -> String {
        if channel.is_im == Some(true) {
            if let Some(uid) = &channel.user {
                if let Some(name) = user_names.get(uid) {
                    return name.clone();
                }
            }
        }
        channel_display_name(channel)
    }

    /// Set a user's status emoji (Slack shortcode like ":coffee:").
    /// Updates the presence icon to show the emoji if the user is online.
    pub fn set_status_emoji(&self, user_id: &str, emoji: Option<&str>) {
        let emoji = emoji.filter(|e| !e.is_empty());
        if let Some(e) = emoji {
            self.status_emoji.borrow_mut().insert(user_id.to_string(), e.to_string());
        } else {
            self.status_emoji.borrow_mut().remove(user_id);
        }
        // Refresh the icon display
        let active = self.presence_state.borrow().get(user_id).copied().unwrap_or(false);
        self.update_presence_icon(user_id, active);
    }

    /// Set status emoji and text for multiple users at once (from initial user list load).
    pub fn set_all_status(&self, emoji_map: HashMap<String, String>, text_map: HashMap<String, String>) {
        *self.status_emoji.borrow_mut() = emoji_map;
        *self.status_text.borrow_mut() = text_map;
    }

    /// Set a user's status text.
    pub fn set_status_text(&self, user_id: &str, text: Option<&str>) {
        let text = text.filter(|t| !t.is_empty());
        if let Some(t) = text {
            self.status_text.borrow_mut().insert(user_id.to_string(), t.to_string());
        } else {
            self.status_text.borrow_mut().remove(user_id);
        }
    }

    /// Update the presence indicator for a user's DM row.
    pub fn set_presence(&self, user_id: &str, active: bool) {
        self.presence_state.borrow_mut().insert(user_id.to_string(), active);
        self.update_presence_icon(user_id, active);

        // Show/hide DM row based on online-only filter + activity recency
        if *self.show_online_only.borrow() {
            if let Some(row) = self.dm_rows.borrow().get(user_id) {
                let cutoff = Self::activity_cutoff();
                let cid = row.widget_name().to_string();
                let recent = Self::is_recent(&self.activity.borrow(), &cid, &cutoff);
                row.set_visible(active && recent);
            }
        }
    }

    /// Update the presence dot and status emoji for a user's DM row.
    fn update_presence_icon(&self, user_id: &str, active: bool) {
        use crate::slack::helpers::get_custom_emoji_path;

        // Update presence dot (green circle / hollow circle)
        if let Some(icon) = self.presence_icons.borrow().get(user_id) {
            if active {
                icon.set_text("\u{25cf}"); // ● filled circle
                icon.remove_css_class("dim-label");
                icon.add_css_class("presence-active");
            } else {
                icon.set_text("\u{25cb}"); // ○ hollow circle
                icon.remove_css_class("presence-active");
                icon.add_css_class("dim-label");
            }
        }

        // Update status emoji container (only shown when active)
        if let Some(container) = self.status_icons.borrow().get(user_id) {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }

            if !active {
                return;
            }

            let status = self.status_emoji.borrow();
            if let Some(emoji_code) = status.get(user_id) {
                let trimmed = emoji_code.trim_matches(':');
                // Try custom emoji image
                if let Some(path) = get_custom_emoji_path(trimmed) {
                    if let Ok(pixbuf) = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(
                        &path, 16, 16, true,
                    ) {
                        let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                        let image = gtk::Image::from_paintable(Some(&texture));
                        image.set_pixel_size(16);
                        container.append(&image);
                        return;
                    }
                }
                // Standard emoji as text
                let display = Self::shortcode_to_display(emoji_code);
                if display != *emoji_code {
                    let label = Label::new(Some(&display));
                    container.append(&label);
                }
            }
        }
    }

    /// Convert a Slack emoji shortcode (e.g. ":coffee:") to a display string.
    fn shortcode_to_display(code: &str) -> String {
        let trimmed = code.trim_matches(':');
        if let Some(emoji_str) = crate::slack::helpers::resolve_slack_shortcode(trimmed) {
            emoji_str.to_string()
        } else {
            // Custom emoji — show as :name: (images not supported in Labels)
            code.to_string()
        }
    }

    pub fn set_unread(&self, channel_id: &str, count: u32) {
        if let Some(badge) = self.badges.borrow().get(channel_id) {
            if count == 0 {
                badge.set_visible(false);
            } else {
                badge.set_text(&count.to_string());
                badge.set_visible(true);
            }
        }
    }

    pub fn select_channel_by_id(&self, channel_id: &str) -> bool {
        for list in [&self.channels_list, &self.dm_list, &self.group_list] {
            let mut idx = 0;
            while let Some(row) = list.row_at_index(idx) {
                if row.widget_name() == channel_id {
                    // Ensure the row is visible (e.g. hidden by online-only filter)
                    row.set_visible(true);
                    list.select_row(Some(&row));
                    return true;
                }
                idx += 1;
            }
        }
        false
    }

    /// Return the first visible row in visual (sorted) order.
    fn first_visible_row(list: &ListBox) -> Option<ListBoxRow> {
        // Collect visible rows and sort them by their on-screen position
        // to respect the active sort function.
        let mut visible = Vec::new();
        let mut idx = 0;
        while let Some(row) = list.row_at_index(idx) {
            if row.is_visible() {
                visible.push(row);
            }
            idx += 1;
        }
        // Sort by the allocation y-position to match visual order
        visible.sort_by_key(|row| {
            row.compute_bounds(list)
                .map(|b| b.y() as i32)
                .unwrap_or(0)
        });
        visible.into_iter().next()
    }

    fn filter_list(
        list_box: &ListBox,
        channels: &[Channel],
        query: &str,
        user_names: &HashMap<String, String>,
        activity: &HashMap<String, String>,
    ) {
        let name_map: HashMap<String, String> = channels
            .iter()
            .map(|ch| {
                let name = if Self::is_mpdm(ch) {
                    Self::group_display_name(ch, user_names).to_lowercase()
                } else if ch.is_im == Some(true) {
                    Self::dm_display_name(ch, user_names).to_lowercase()
                } else {
                    channel_display_name(ch).to_lowercase()
                };
                (ch.id.clone(), name)
            })
            .collect();

        let cutoff = Self::activity_cutoff();
        let mut idx = 0;
        while let Some(row) = list_box.row_at_index(idx) {
            let cid = row.widget_name().to_string();
            if query.is_empty() {
                // When not searching, only show recent channels
                row.set_visible(Self::is_recent(activity, &cid, &cutoff));
            } else {
                // When searching, show all channels that match (regardless of activity)
                let visible = name_map
                    .get(&cid)
                    .is_some_and(|name| name.contains(query));
                row.set_visible(visible);
            }
            idx += 1;
        }
    }

    fn filter_dm_list(
        list_box: &ListBox,
        channels: &[Channel],
        query: &str,
        user_names: &HashMap<String, String>,
        presence_state: &HashMap<String, bool>,
        online_only: bool,
        activity: &HashMap<String, String>,
    ) {
        let name_map: HashMap<String, String> = channels
            .iter()
            .map(|ch| {
                let name = Self::dm_display_name(ch, user_names).to_lowercase();
                (ch.id.clone(), name)
            })
            .collect();

        // Build channel_id -> user_id map for presence lookup
        let user_map: HashMap<String, String> = channels
            .iter()
            .filter_map(|ch| Some((ch.id.clone(), ch.user.clone()?)))
            .collect();

        // Never hide the currently selected row (prevents losing selection
        // when clearing search re-applies the online-only filter).
        let selected_name = list_box
            .selected_row()
            .map(|r| r.widget_name().to_string());

        let cutoff = Self::activity_cutoff();
        let mut idx = 0;
        while let Some(row) = list_box.row_at_index(idx) {
            let cid = row.widget_name().to_string();
            let is_selected = selected_name.as_deref() == Some(&cid);
            if query.is_empty() {
                // When not searching, apply both activity and presence filters
                let recent = Self::is_recent(activity, &cid, &cutoff);
                let is_active = if online_only && !is_selected {
                    user_map
                        .get(&cid)
                        .and_then(|uid| presence_state.get(uid))
                        .copied()
                        .unwrap_or(false)
                } else {
                    true
                };
                row.set_visible(recent && is_active);
            } else {
                // When searching, show all DMs that match (regardless of activity/presence)
                let matches = name_map.get(&cid).is_some_and(|name| name.contains(query));
                row.set_visible(matches);
            }
            idx += 1;
        }
    }

    fn search_cmp(
        a: &ListBoxRow,
        b: &ListBoxRow,
        query: &str,
        display_names: &HashMap<String, String>,
    ) -> gtk::Ordering {
        let name_a = display_names
            .get(&a.widget_name().to_string())
            .cloned()
            .unwrap_or_default();
        let name_b = display_names
            .get(&b.widget_name().to_string())
            .cloned()
            .unwrap_or_default();

        let a_prefix = name_a.starts_with(query);
        let b_prefix = name_b.starts_with(query);

        match (a_prefix, b_prefix) {
            (true, false) => gtk::Ordering::Smaller,
            (false, true) => gtk::Ordering::Larger,
            _ => {
                if name_a < name_b {
                    gtk::Ordering::Smaller
                } else if name_a > name_b {
                    gtk::Ordering::Larger
                } else {
                    gtk::Ordering::Equal
                }
            }
        }
    }
}

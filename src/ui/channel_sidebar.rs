use gtk4::prelude::*;
use gtk4::{self as gtk, Label, ListBox, ListBoxRow, ScrolledWindow, SearchEntry};
use slacko::types::Channel;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::slack::helpers::channel_display_name;

/// Channel actions triggered from context menus.
#[derive(Debug, Clone, Copy)]
pub enum ChannelAction {
    Leave,
    Archive,
    Close,
}

/// Callback type for channel actions: (action, channel_id)
pub type ChannelActionCallback = Rc<dyn Fn(ChannelAction, &str)>;

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
    /// Presence indicator labels keyed by user ID (for DM rows).
    presence_icons: Rc<RefCell<HashMap<String, Label>>>,
    /// Presence state keyed by user ID.
    presence_state: Rc<RefCell<HashMap<String, bool>>>,
    /// DM rows keyed by user ID (for showing/hiding based on presence).
    dm_rows: Rc<RefCell<HashMap<String, ListBoxRow>>>,
    /// Whether to show only online DM users.
    show_online_only: Rc<RefCell<bool>>,
    /// Callback for channel context menu actions.
    action_callback: Rc<RefCell<Option<ChannelActionCallback>>>,
}

impl ChannelSidebar {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_width_request(240);
        container.add_css_class("sidebar");

        let search_entry = SearchEntry::new();
        search_entry.set_placeholder_text(Some("Search..."));
        search_entry.set_placeholder_text(Some("Search..."));

        let scrolled = ScrolledWindow::new();
        scrolled.set_vexpand(true);

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Channels section
        let channels_header = Label::new(Some("Channels"));
        channels_header.add_css_class("heading");
        channels_header.add_css_class("dim-label");
        channels_header.set_halign(gtk::Align::Start);
        channels_header.set_margin_top(8);
        channels_header.set_margin_start(12);
        channels_header.set_margin_bottom(4);
        inner.append(&channels_header);

        let channels_list = ListBox::new();
        channels_list.set_selection_mode(gtk::SelectionMode::Single);
        channels_list.add_css_class("navigation-sidebar");
        inner.append(&channels_list);

        // DM section
        let dm_header = Label::new(Some("Direct Messages"));
        dm_header.add_css_class("heading");
        dm_header.add_css_class("dim-label");
        dm_header.set_halign(gtk::Align::Start);
        dm_header.set_margin_top(12);
        dm_header.set_margin_start(12);
        dm_header.set_margin_bottom(4);
        inner.append(&dm_header);

        let dm_list = ListBox::new();
        dm_list.set_selection_mode(gtk::SelectionMode::Single);
        dm_list.add_css_class("navigation-sidebar");
        inner.append(&dm_list);

        // Groups section
        let group_header = Label::new(Some("Groups"));
        group_header.add_css_class("heading");
        group_header.add_css_class("dim-label");
        group_header.set_halign(gtk::Align::Start);
        group_header.set_margin_top(12);
        group_header.set_margin_start(12);
        group_header.set_margin_bottom(4);
        inner.append(&group_header);

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
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_lowercase();
            let chs = channels_clone.borrow();
            let dms = dms_clone.borrow();
            let grs = groups_clone.borrow();
            let names = user_names_clone.borrow();
            Self::filter_list(&ch_list_clone, &chs, &query, &names);
            // DM filtering: when searching, show all matches; when not searching, respect online-only
            Self::filter_dm_list(
                &dm_list_clone, &dms, &query, &names,
                &presence_state_filter.borrow(),
                *show_online_filter.borrow() && query.is_empty(),
            );
            Self::filter_list(&gr_list_clone, &grs, &query, &names);

            if query.is_empty() {
                ch_list_clone.set_sort_func(|_, _| gtk::Ordering::Equal);
                dm_list_clone.set_sort_func(|_, _| gtk::Ordering::Equal);
                gr_list_clone.set_sort_func(|_, _| gtk::Ordering::Equal);
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
            presence_state,
            dm_rows,
            show_online_only,
            action_callback,
        }
    }

    pub fn set_action_callback(&self, cb: ChannelActionCallback) {
        *self.action_callback.borrow_mut() = Some(cb);
    }

    /// Set channel activity timestamps (channel_id -> Slack ts).
    pub fn set_activity(&self, activity: HashMap<String, String>) {
        *self.activity.borrow_mut() = activity;
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

    pub fn set_channels(&self, all: &[Channel]) {
        let names = self.user_names.borrow();

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

        let act = self.activity.borrow();

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
        for ch in &ch_list {
            let (row, badge) = Self::make_channel_row(ch);
            Self::attach_context_menu(&row, &ch.id, false, &acb);
            self.channels_list.append(&row);
            new_badges.insert(ch.id.clone(), badge);
        }

        // Rebuild DM list
        while let Some(child) = self.dm_list.first_child() {
            self.dm_list.remove(&child);
        }
        let mut new_presence_icons = HashMap::new();
        let mut new_dm_rows = HashMap::new();
        let online_only = *self.show_online_only.borrow();
        let presence = self.presence_state.borrow();
        for ch in &dm_list {
            let (row, badge, icon) = Self::make_dm_row(ch, &names);
            Self::attach_context_menu(&row, &ch.id, true, &acb);
            // Show/hide based on known presence state
            if online_only {
                let is_active = ch.user.as_ref()
                    .and_then(|uid| presence.get(uid))
                    .copied()
                    .unwrap_or(false);
                row.set_visible(is_active);
            }
            self.dm_list.append(&row);
            new_badges.insert(ch.id.clone(), badge);
            if let Some(uid) = &ch.user {
                new_presence_icons.insert(uid.clone(), icon);
                new_dm_rows.insert(uid.clone(), row);
            }
        }
        drop(presence);

        // Rebuild group list
        while let Some(child) = self.group_list.first_child() {
            self.group_list.remove(&child);
        }
        for ch in &gr_list {
            let (row, badge) = Self::make_group_row(ch, &names);
            Self::attach_context_menu(&row, &ch.id, true, &acb);
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
        *self.dm_rows.borrow_mut() = new_dm_rows;
        *self.channels.borrow_mut() = ch_list;
        *self.dms.borrow_mut() = dm_list;
        *self.groups.borrow_mut() = gr_list;
        self.search_entry.set_text("");
    }

    fn attach_context_menu(
        row: &ListBoxRow,
        channel_id: &str,
        is_dm: bool,
        action_cb: &Option<ChannelActionCallback>,
    ) {
        let Some(acb) = action_cb.clone() else { return };

        let popover = gtk::Popover::new();
        let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        if is_dm {
            let close_btn = gtk::Button::with_label("Close conversation");
            close_btn.add_css_class("flat");
            let cid = channel_id.to_string();
            let acb2 = acb.clone();
            let pop = popover.clone();
            close_btn.connect_clicked(move |_| {
                acb2(ChannelAction::Close, &cid);
                pop.popdown();
            });
            menu_box.append(&close_btn);
        } else {
            let leave_btn = gtk::Button::with_label("Leave channel");
            leave_btn.add_css_class("flat");
            let cid = channel_id.to_string();
            let acb2 = acb.clone();
            let pop = popover.clone();
            leave_btn.connect_clicked(move |_| {
                acb2(ChannelAction::Leave, &cid);
                pop.popdown();
            });
            menu_box.append(&leave_btn);

            let archive_btn = gtk::Button::with_label("Archive channel");
            archive_btn.add_css_class("flat");
            let cid = channel_id.to_string();
            let acb2 = acb.clone();
            let pop = popover.clone();
            archive_btn.connect_clicked(move |_| {
                acb2(ChannelAction::Archive, &cid);
                pop.popdown();
            });
            menu_box.append(&archive_btn);
        }

        popover.set_child(Some(&menu_box));
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
        let popover_weak = popover.downgrade();
        gesture.connect_released(move |_, _, _, _| {
            if let Some(p) = popover_weak.upgrade() {
                p.popup();
            }
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
    ) -> (ListBoxRow, Label, Label) {
        let row = ListBoxRow::new();

        let label_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        label_box.set_margin_top(4);
        label_box.set_margin_bottom(4);
        label_box.set_margin_start(12);
        label_box.set_margin_end(8);

        let icon_label = Label::new(Some("\u{25cb}"));
        icon_label.add_css_class("dim-label");
        label_box.append(&icon_label);

        let name = Self::dm_display_name(channel, user_names);
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

        (row, badge, icon_label)
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

    /// Update the presence indicator for a user's DM row.
    pub fn set_presence(&self, user_id: &str, active: bool) {
        self.presence_state.borrow_mut().insert(user_id.to_string(), active);

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

        // Show/hide DM row based on online-only filter
        if *self.show_online_only.borrow() {
            if let Some(row) = self.dm_rows.borrow().get(user_id) {
                row.set_visible(active);
            }
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

    fn first_visible_row(list: &ListBox) -> Option<ListBoxRow> {
        let mut idx = 0;
        while let Some(row) = list.row_at_index(idx) {
            if row.is_visible() {
                return Some(row);
            }
            idx += 1;
        }
        None
    }

    fn filter_list(
        list_box: &ListBox,
        channels: &[Channel],
        query: &str,
        user_names: &HashMap<String, String>,
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

        let mut idx = 0;
        while let Some(row) = list_box.row_at_index(idx) {
            if query.is_empty() {
                row.set_visible(true);
            } else {
                let cid = row.widget_name().to_string();
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

        let mut idx = 0;
        while let Some(row) = list_box.row_at_index(idx) {
            let cid = row.widget_name().to_string();
            let matches_query = query.is_empty()
                || name_map.get(&cid).is_some_and(|name| name.contains(query));
            let is_active = if online_only {
                user_map
                    .get(&cid)
                    .and_then(|uid| presence_state.get(uid))
                    .copied()
                    .unwrap_or(false)
            } else {
                true
            };
            row.set_visible(matches_query && is_active);
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

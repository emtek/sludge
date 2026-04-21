use gtk4::prelude::*;
use gtk4::{self as gtk, Button, Label, ListBox, ListBoxRow, Picture, ScrolledWindow};
use slacko::types::Message;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::slack::client::Client;
use crate::slack::helpers::{format_message_markup, html_to_pango, looks_like_html};

/// Callback type for starting a reply to a message:
/// (thread_ts, channel_id, user_display_name, text_preview)
pub type ThreadOpenCallback = Rc<dyn Fn(&str, &str, &str, &str)>;

/// Callback type for opening a DM with a user: (user_id)
pub type MentionCallback = Rc<dyn Fn(&str)>;

/// Callback type for toggling a reaction: (channel_id, message_ts, emoji_name, button)
pub type ReactionCallback = Rc<dyn Fn(&str, &str, &str, &gtk::Button)>;

/// Callback type for deleting a message: (channel_id, message_ts, row)
pub type DeleteCallback = Rc<dyn Fn(&str, &str, &ListBoxRow)>;

/// Callback type for editing a message: (channel_id, message_ts, current_text, outer_box)
/// The outer_box is the message container; the body widget is the child after the header.
pub type EditCallback = Rc<dyn Fn(&str, &str, &str, &gtk::Box)>;

/// Callback for loading older messages: called with channel_id and oldest message ts.
pub type LoadMoreCallback = Rc<dyn Fn(&str, &str)>;

/// Callback for loading newer messages: called with channel_id and newest message ts.
pub type LoadNewerCallback = Rc<dyn Fn(&str, &str)>;

/// Callback for clicking a search result: (channel_id, message_ts, thread_ts_if_reply)
pub type SearchResultCallback = Rc<dyn Fn(&str, &str, Option<&str>)>;

/// Callback for expanding/collapsing a thread inline: (thread_ts, channel_id, parent_row)
/// Called when the user toggles the thread expander. The implementation should fetch
/// replies and call `insert_thread_replies` / `remove_thread_replies` on the MessageView.
pub type ThreadExpandCallback = Rc<dyn Fn(&str, &str, &ListBoxRow)>;

pub struct MessageView {
    pub widget: gtk::Box,
    pub list_box: ListBox,
    scrolled: ScrolledWindow,
    pub header_label: Label,
    pub search_entry: gtk::SearchEntry,
    pub spinner: gtk::Spinner,
    pub thread_callback: RefCell<Option<ThreadOpenCallback>>,
    pub mention_callback: RefCell<Option<MentionCallback>>,
    pub reaction_callback: RefCell<Option<ReactionCallback>>,
    pub delete_callback: RefCell<Option<DeleteCallback>>,
    pub edit_callback: RefCell<Option<EditCallback>>,
    pub self_user_id: RefCell<String>,
    channel_id: Rc<RefCell<Option<String>>>,
    /// Known thread reply counts: thread_ts -> reply count (excluding parent).
    pub thread_counts: Rc<RefCell<HashMap<String, usize>>>,
    /// Thread button labels keyed by message ts, for updating counts after loading.
    pub thread_labels: Rc<RefCell<HashMap<String, Label>>>,
    /// Reaction FlowBox containers keyed by message ts, for live reaction updates.
    pub reaction_boxes: Rc<RefCell<HashMap<String, gtk::FlowBox>>>,
    /// Typing indicator label in the channel header.
    pub typing_label: Label,
    /// Button to show channel members / invite users.
    pub members_button: Button,
    /// Button to start a Google Meet call.
    pub call_button: Button,
    /// Generation counter; incremented on clear() so in-flight image loads detect staleness.
    pub image_generation: Rc<Cell<u64>>,
    subteam_names: RefCell<Rc<HashMap<String, String>>>,
    /// Reaction picker cells — take + unparent on clear.
    pub picker_cells: Rc<RefCell<Vec<Rc<RefCell<Option<gtk::Popover>>>>>>,
    /// Stored textures from image loads — cleared on channel switch to free GPU/pixel memory
    /// even before GTK finalizes the widget tree.
    pub stored_textures: Rc<RefCell<Vec<Rc<RefCell<Option<gtk4::gdk::Texture>>>>>>,
    /// Whether a load-more request is in flight (prevents duplicate fetches).
    loading_more: Rc<Cell<bool>>,
    /// Set to false when the server returns fewer messages than requested (no more history).
    has_more: Rc<Cell<bool>>,
    load_more_callback: RefCell<Option<LoadMoreCallback>>,
    load_newer_callback: RefCell<Option<LoadNewerCallback>>,
    /// Whether there are newer messages to load when scrolling to the bottom.
    pub has_more_newer: Rc<Cell<bool>>,
    search_result_callback: RefCell<Option<SearchResultCallback>>,
    /// Active search query — when set, scroll-to-top loads more search results.
    pub search_query: Rc<RefCell<Option<String>>>,
    /// Callback for expanding/collapsing a thread inline.
    pub thread_expand_callback: RefCell<Option<ThreadExpandCallback>>,
    /// Tracks which threads are expanded and holds their reply row widgets.
    pub expanded_threads: Rc<RefCell<HashMap<String, Vec<ListBoxRow>>>>,
}

impl MessageView {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);

        // Channel header
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_margin_top(8);
        header.set_margin_bottom(8);
        header.set_margin_start(16);
        header.set_margin_end(16);
        header.add_css_class("toolbar");

        let header_label = Label::new(Some("Select a channel"));
        header_label.add_css_class("title-3");
        header_label.set_halign(gtk::Align::Start);
        header_label.set_hexpand(true);

        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("Search messages..."));
        search_entry.set_hexpand(true);
        search_entry.set_visible(false);

        let header_stack = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header_stack.set_hexpand(true);
        header_stack.append(&header_label);
        header_stack.append(&search_entry);
        header.append(&header_stack);

        let typing_label = Label::new(None);
        typing_label.add_css_class("dim-label");
        typing_label.add_css_class("caption");
        typing_label.set_halign(gtk::Align::End);
        typing_label.set_visible(false);
        header.append(&typing_label);

        let members_button = Button::from_icon_name("system-users-symbolic");
        members_button.set_tooltip_text(Some("Channel members"));
        members_button.add_css_class("flat");
        header.append(&members_button);

        let call_button = Button::from_icon_name("call-start-symbolic");
        call_button.set_tooltip_text(Some("Start Google Meet call"));
        call_button.add_css_class("flat");
        header.append(&call_button);

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        spinner.set_size_request(16, 16);
        header.append(&spinner);

        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);

        container.append(&header);
        container.append(&separator);

        // Messages list
        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::Single);
        list_box.set_focusable(true);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(8);
        list_box.set_margin_end(8);
        list_box.set_margin_bottom(8);

        let scrolled = ScrolledWindow::new();
        scrolled.set_vexpand(true);
        scrolled.set_hexpand(true);
        scrolled.set_focusable(true);
        scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        scrolled.set_child(Some(&list_box));

        container.append(&scrolled);

        // Auto-scroll to bottom when content grows (e.g. images loading),
        // but only if the user was already near the bottom.
        let adj = scrolled.vadjustment();
        adj.connect_upper_notify(move |adj| {
            let max_scroll = adj.upper() - adj.page_size();
            if max_scroll <= 0.0 {
                return;
            }
            let distance_from_bottom = max_scroll - adj.value();
            if distance_from_bottom < 400.0 {
                adj.set_value(max_scroll);
            }
        });

        let loading_more = Rc::new(Cell::new(false));
        let has_more = Rc::new(Cell::new(true));
        let load_more_callback: RefCell<Option<LoadMoreCallback>> = RefCell::new(None);

        Self {
            widget: container,
            list_box,
            scrolled,
            header_label,
            search_entry,
            spinner,
            thread_callback: RefCell::new(None),
            mention_callback: RefCell::new(None),
            reaction_callback: RefCell::new(None),
            delete_callback: RefCell::new(None),
            edit_callback: RefCell::new(None),
            self_user_id: RefCell::new(String::new()),
            channel_id: Rc::new(RefCell::new(None)),
            thread_counts: Rc::new(RefCell::new(HashMap::new())),
            thread_labels: Rc::new(RefCell::new(HashMap::new())),
            reaction_boxes: Rc::new(RefCell::new(HashMap::new())),
            typing_label,
            members_button,
            call_button,
            image_generation: Rc::new(Cell::new(0)),
            subteam_names: RefCell::new(Rc::new(HashMap::new())),
            picker_cells: Rc::new(RefCell::new(Vec::new())),
            loading_more,
            has_more,
            load_more_callback,
            load_newer_callback: RefCell::new(None),
            has_more_newer: Rc::new(Cell::new(false)),
            search_result_callback: RefCell::new(None),
            search_query: Rc::new(RefCell::new(None)),
            thread_expand_callback: RefCell::new(None),
            expanded_threads: Rc::new(RefCell::new(HashMap::new())),
            stored_textures: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn set_thread_callback(&self, cb: ThreadOpenCallback) {
        *self.thread_callback.borrow_mut() = Some(cb);
    }

    pub fn set_thread_expand_callback(&self, cb: ThreadExpandCallback) {
        *self.thread_expand_callback.borrow_mut() = Some(cb);
    }

    /// Insert reply rows below the parent message row for inline thread expansion.
    pub fn insert_thread_replies(&self, thread_ts: &str, parent_row: &ListBoxRow, rows: Vec<ListBoxRow>) {
        // Find the parent row's index
        let mut parent_idx = None;
        let mut idx = 0;
        while let Some(r) = self.list_box.row_at_index(idx) {
            if r == *parent_row {
                parent_idx = Some(idx);
                break;
            }
            idx += 1;
        }
        let Some(parent_idx) = parent_idx else { return };

        // Insert reply rows after the parent
        for (i, reply_row) in rows.iter().enumerate() {
            self.list_box.insert(reply_row, parent_idx + 1 + i as i32);
        }

        self.expanded_threads.borrow_mut().insert(thread_ts.to_string(), rows);
    }

    /// Remove reply rows for a collapsed thread.
    pub fn remove_thread_replies(&self, thread_ts: &str) {
        if let Some(rows) = self.expanded_threads.borrow_mut().remove(thread_ts) {
            for row in &rows {
                Self::unparent_floating_recursive(row);
                self.list_box.remove(row);
            }
        }
    }

    /// Check if a thread is currently expanded.
    pub fn is_thread_expanded(&self, thread_ts: &str) -> bool {
        self.expanded_threads.borrow().contains_key(thread_ts)
    }

    /// Promote a message row to a thread parent: hide its Reply button and
    /// show the thread expander. Called when the first reply is posted to
    /// a previously-unthreaded message.
    pub fn promote_to_thread(&self, ts: &str) {
        let mut idx = 0;
        while let Some(row) = self.list_box.row_at_index(idx) {
            if row.widget_name() == ts {
                fn swap(widget: &gtk::Widget) {
                    if let Some(btn) = widget.downcast_ref::<gtk::Button>() {
                        if btn.has_css_class("thread-btn") {
                            btn.set_visible(true);
                        } else if btn.has_css_class("reply-btn") {
                            btn.set_visible(false);
                        }
                    }
                    let mut child = widget.first_child();
                    while let Some(c) = child {
                        swap(&c);
                        child = c.next_sibling();
                    }
                }
                swap(row.upcast_ref());
                return;
            }
            idx += 1;
        }
    }

    pub fn set_mention_callback(&self, cb: MentionCallback) {
        *self.mention_callback.borrow_mut() = Some(cb);
    }

    pub fn set_reaction_callback(&self, cb: ReactionCallback) {
        *self.reaction_callback.borrow_mut() = Some(cb);
    }

    pub fn set_delete_callback(&self, cb: DeleteCallback) {
        *self.delete_callback.borrow_mut() = Some(cb);
    }

    pub fn set_edit_callback(&self, cb: EditCallback) {
        *self.edit_callback.borrow_mut() = Some(cb);
    }

    pub fn set_self_user_id(&self, uid: &str) {
        *self.self_user_id.borrow_mut() = uid.to_string();
    }

    pub fn set_search_result_callback(&self, cb: SearchResultCallback) {
        *self.search_result_callback.borrow_mut() = Some(cb);
    }

    /// Set the callback for loading older messages.
    /// Called with (channel_id, oldest_message_ts).
    pub fn set_load_more_callback(&self, cb: LoadMoreCallback) {
        *self.load_more_callback.borrow_mut() = Some(cb);
    }

    /// Set the callback for loading newer messages from the DB.
    /// Called with (channel_id, newest_message_ts).
    pub fn set_load_newer_callback(&self, cb: LoadNewerCallback) {
        *self.load_newer_callback.borrow_mut() = Some(cb);
    }

    /// Connect the edge-reached handler for both top (older) and bottom (newer).
    /// Must be called after setting both callbacks.
    pub fn connect_edge_loading(&self) {
        let loading_more = self.loading_more.clone();
        let has_more = self.has_more.clone();
        let has_more_newer = self.has_more_newer.clone();
        let channel_id = self.channel_id.clone();
        let list_box = self.list_box.clone();
        let load_older = self.load_more_callback.borrow().clone();
        let load_newer = self.load_newer_callback.borrow().clone();
        let search_query = self.search_query.clone();
        self.scrolled.connect_edge_reached(move |_, pos| {
            if loading_more.get() {
                return;
            }
            // Don't load more during search
            if search_query.borrow().is_some() {
                return;
            }
            let Some(cid) = channel_id.borrow().clone() else { return };
            match pos {
                gtk::PositionType::Top => {
                    if !has_more.get() {
                        return;
                    }
                    let oldest_ts = list_box
                        .row_at_index(0)
                        .map(|r| r.widget_name().to_string())
                        .filter(|ts| !ts.is_empty());
                    let Some(ts) = oldest_ts else { return };
                    loading_more.set(true);
                    if let Some(ref cb) = load_older {
                        cb(&cid, &ts);
                    }
                }
                gtk::PositionType::Bottom => {
                    if !has_more_newer.get() {
                        return;
                    }
                    // Find the newest (last) row's ts
                    let mut last_ts = None;
                    let mut idx = 0;
                    while let Some(row) = list_box.row_at_index(idx) {
                        let name = row.widget_name().to_string();
                        if !name.is_empty() {
                            last_ts = Some(name);
                        }
                        idx += 1;
                    }
                    let Some(ts) = last_ts else { return };
                    loading_more.set(true);
                    if let Some(ref cb) = load_newer {
                        cb(&cid, &ts);
                    }
                }
                _ => {}
            }
        });
    }

    /// Prepend older messages at the top, preserving scroll position.
    /// `fetched_count` is the number requested — if fewer arrived, there's no more history.
    pub fn prepend_messages(
        &self,
        messages: &[Message],
        users: &HashMap<String, String>,
        client: &Client,
        rt: &tokio::runtime::Handle,
        fetched_count: usize,
    ) {
        if messages.is_empty() {
            self.has_more.set(false);
            self.loading_more.set(false);
            return;
        }
        if messages.len() < fetched_count {
            self.has_more.set(false);
        }

        let thread_cb = self.thread_callback.borrow();
        let mention_cb = self.mention_callback.borrow();
        let reaction_cb = self.reaction_callback.borrow();
        let delete_cb = self.delete_callback.borrow();
        let edit_cb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let channel_id = self.channel_id.borrow();
        let subteam_names = self.subteam_names.borrow();

        // Record current scroll height so we can restore position after prepend
        let adj = self.scrolled.vadjustment();
        let old_upper = adj.upper();

        // Messages from the API are newest-first; prepend oldest-first (i.e. iterate forward)
        let thread_expand_cb = self.thread_expand_callback.borrow();
        for msg in messages.iter().rev() {
            let row = make_message_row(
                msg, users, &subteam_names, client, rt,
                &thread_cb, &mention_cb, &reaction_cb, &delete_cb, &edit_cb,
                channel_id.as_deref(), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
                &self.image_generation, &self.picker_cells,
                &thread_expand_cb, &self.expanded_threads, &self.stored_textures,
            );
            self.list_box.prepend(&row);
        }

        // After layout, adjust scroll so the previously visible content stays in place
        let adj2 = self.scrolled.vadjustment();
        let loading_more = self.loading_more.clone();
        let signal_id: Rc<RefCell<Option<gtk4::glib::SignalHandlerId>>> =
            Rc::new(RefCell::new(None));
        let signal_id2 = signal_id.clone();
        let id = adj2.connect_changed(move |adj| {
            let new_upper = adj.upper();
            let delta = new_upper - old_upper;
            if delta > 0.0 {
                adj.set_value(adj.value() + delta);
            }
            loading_more.set(false);
            if let Some(id) = signal_id2.borrow_mut().take() {
                adj.disconnect(id);
            }
        });
        *signal_id.borrow_mut() = Some(id);
    }

    /// Append newer messages at the bottom.
    /// If fewer than `fetched_count` arrived, there are no more newer messages.
    pub fn append_newer_messages(
        &self,
        messages: &[Message],
        users: &HashMap<String, String>,
        client: &Client,
        rt: &tokio::runtime::Handle,
        fetched_count: usize,
    ) {
        if messages.is_empty() {
            self.has_more_newer.set(false);
            self.loading_more.set(false);
            return;
        }
        if messages.len() < fetched_count {
            self.has_more_newer.set(false);
        }

        let thread_cb = self.thread_callback.borrow();
        let mention_cb = self.mention_callback.borrow();
        let reaction_cb = self.reaction_callback.borrow();
        let delete_cb = self.delete_callback.borrow();
        let edit_cb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let channel_id = self.channel_id.borrow();
        let subteam_names = self.subteam_names.borrow();

        // Messages are already in ASC order from the DB query
        let thread_expand_cb = self.thread_expand_callback.borrow();
        for msg in messages {
            let row = make_message_row(
                msg, users, &subteam_names, client, rt,
                &thread_cb, &mention_cb, &reaction_cb, &delete_cb, &edit_cb,
                channel_id.as_deref(), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
                &self.image_generation, &self.picker_cells,
                &thread_expand_cb, &self.expanded_threads, &self.stored_textures,
            );
            self.list_box.append(&row);
        }

        self.loading_more.set(false);
    }

    /// Reset the loading_more flag (e.g. on error).
    pub fn reset_loading_more(&self) {
        self.loading_more.set(false);
    }

    /// Apply a batch of reply counts, refresh thread labels, and promote rows
    /// to thread parents (showing the expander instead of the Reply button).
    /// Typically called right after `set_messages` to populate counts from the DB.
    pub fn apply_thread_counts(&self, counts: HashMap<String, usize>) {
        let mut tc = self.thread_counts.borrow_mut();
        for (ts, count) in &counts {
            tc.insert(ts.clone(), *count);
        }
        drop(tc);
        let labels = self.thread_labels.borrow();
        for (ts, count) in &counts {
            if let Some(label) = labels.get(ts.as_str()) {
                label.set_text(&format!(
                    "{count} {}",
                    if *count == 1 { "reply" } else { "replies" }
                ));
            }
            if *count > 0 {
                self.promote_to_thread(ts);
            }
        }
    }

    /// Update the thread button label for a message after loading its replies.
    pub fn update_thread_count(&self, ts: &str, count: usize) {
        self.thread_counts.borrow_mut().insert(ts.to_string(), count);
        if let Some(label) = self.thread_labels.borrow().get(ts) {
            if count > 0 {
                label.set_text(&format!("{count} {}", if count == 1 { "reply" } else { "replies" }));
            }
        }
    }

    /// Rebuild the reaction buttons for a message after a reaction change.
    pub fn update_reactions(
        &self,
        ts: &str,
        reactions: &[slacko::types::Reaction],
        users: &HashMap<String, String>,
    ) {
        let boxes = self.reaction_boxes.borrow();
        let Some(flow_box) = boxes.get(ts) else { return };

        // Remove all children except the last one (the add-reaction MenuButton)
        while flow_box.child_at_index(0).is_some() {
            let count = {
                let mut n = 0;
                while flow_box.child_at_index(n).is_some() { n += 1; }
                n
            };
            if count <= 1 { break; }
            if let Some(child) = flow_box.child_at_index(0) {
                // Unparent any popovers attached to this button before removing
                Self::unparent_floating_recursive(&child);
                flow_box.remove(&child);
            }
        }

        let reaction_cb = self.reaction_callback.borrow().clone();
        let channel_id = self.channel_id.borrow().clone();

        for reaction in reactions {
            let btn = make_reaction_button(
                reaction, users, ts,
                &reaction_cb, channel_id.as_deref(),
            );
            let insert_pos = {
                let mut n = 0;
                while flow_box.child_at_index(n).is_some() { n += 1; }
                if n > 0 { n as i32 - 1 } else { 0 }
            };
            flow_box.insert(&btn, insert_pos);
        }
    }

    pub fn set_channel_name(&self, name: &str) {
        self.header_label.set_text(&format!("# {name}"));
    }

    pub fn set_channel_id(&self, id: &str) {
        *self.channel_id.borrow_mut() = Some(id.to_string());
    }

    pub fn set_subteam_names(&self, names: Rc<HashMap<String, String>>) {
        *self.subteam_names.borrow_mut() = names;
    }

    pub fn clear(&self) {

        // Bump generation so in-flight image downloads from the previous channel bail out
        self.image_generation.set(self.image_generation.get() + 1);

        // Eagerly release Texture pixel data so we don't wait for GTK widget finalization.
        for tex in self.stored_textures.borrow_mut().drain(..) {
            tex.borrow_mut().take();
        }

        // Explicitly unparent all tracked emoji choosers — the tree walk may miss
        // popovers because GTK4 set_parent'd popovers don't always appear in
        // first_child() traversal depending on the container nesting.
        for cell in self.picker_cells.borrow_mut().drain(..) {
            if let Some(chooser) = cell.borrow_mut().take() {
                chooser.unparent();
            }
        }

        // Walk the tree and unparent remaining popovers (reaction who-reacted).
        let mut idx = 0;
        while let Some(row) = self.list_box.row_at_index(idx) {
            Self::unparent_floating_recursive(&row);
            Self::clear_pictures_recursive(&row);
            idx += 1;
        }
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }
        self.thread_counts.borrow_mut().clear();
        self.thread_labels.borrow_mut().clear();
        self.reaction_boxes.borrow_mut().clear();
        self.expanded_threads.borrow_mut().clear();
    }

    /// Recursively find and unparent all Popover and EmojiChooser widgets in the tree.
    /// GTK's `remove()` does not emit `destroy`, so connect_destroy cleanup never fires.
    /// Popovers attached via `set_parent()` are not regular children — they must be
    /// explicitly unparented or they leak and cause SEGV on finalization.
    pub fn unparent_floating_recursive(widget: &impl IsA<gtk::Widget>) {
        let widget = widget.as_ref();
        // Check direct children first
        let mut child = widget.first_child();
        while let Some(c) = child {
            // Grab next sibling BEFORE we potentially unparent this child
            let next = c.next_sibling();
            if c.downcast_ref::<gtk::Popover>().is_some()
                || c.downcast_ref::<gtk::EmojiChooser>().is_some()
            {
                c.unparent();
            } else {
                Self::unparent_floating_recursive(&c);
            }
            child = next;
        }
    }

    /// Recursively clear paintables from Picture widgets in a widget tree.
    fn clear_pictures_recursive(widget: &impl IsA<gtk::Widget>) {
        let widget = widget.as_ref();
        if let Some(pic) = widget.downcast_ref::<Picture>() {
            pic.set_paintable(None::<&gtk4::gdk::Texture>);
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            Self::clear_pictures_recursive(&c);
            child = c.next_sibling();
        }
    }

    pub fn set_messages(
        &self,
        messages: &[Message],
        users: &HashMap<String, String>,
        client: &Client,
        rt: &tokio::runtime::Handle,
    ) {
        self.clear();
        self.has_more.set(true);
        self.has_more_newer.set(false);
        self.loading_more.set(false);
        *self.search_query.borrow_mut() = None;

        let thread_cb = self.thread_callback.borrow();
        let mention_cb = self.mention_callback.borrow();
        let reaction_cb = self.reaction_callback.borrow();
        let delete_cb = self.delete_callback.borrow();
        let edit_cb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let channel_id = self.channel_id.borrow();
        let subteam_names = self.subteam_names.borrow();

        // Slack returns newest-first; display oldest-first
        let thread_expand_cb = self.thread_expand_callback.borrow();
        for msg in messages.iter().rev() {
            let row = make_message_row(
                msg, users, &subteam_names, client, rt,
                &thread_cb, &mention_cb, &reaction_cb, &delete_cb, &edit_cb,
                channel_id.as_deref(), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
                &self.image_generation, &self.picker_cells,
                &thread_expand_cb, &self.expanded_threads, &self.stored_textures,
            );
            self.list_box.append(&row);
        }

        self.scroll_to_bottom();
    }

    /// Display full-text search results. Each row is clickable and navigates
    /// to the source channel/message via the search_result_callback.
    pub fn set_search_results(
        &self,
        results: &[(String, Message)],
        users: &HashMap<String, String>,
        channels: &[slacko::types::Channel],
        client: &Client,
        rt: &tokio::runtime::Handle,
    ) {
        self.clear();
        self.set_channel_name("Search Results");
        self.set_channel_id("");

        let thread_cb = self.thread_callback.borrow();
        let thread_expand_cb = self.thread_expand_callback.borrow();
        let mention_cb = self.mention_callback.borrow();
        let reaction_cb = self.reaction_callback.borrow();
        let delete_cb = self.delete_callback.borrow();
        let edit_cb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let search_cb = self.search_result_callback.borrow();
        let subteam_names = self.subteam_names.borrow();

        for (channel_id, msg) in results {
            // Build channel label
            let ch_name = channels
                .iter()
                .find(|c| c.id == *channel_id)
                .map(|c| {
                    if c.is_im == Some(true) {
                        users
                            .get(c.user.as_deref().unwrap_or(""))
                            .map(|s| s.as_str())
                            .or(c.name.as_deref())
                            .unwrap_or_default()
                            .to_string()
                    } else {
                        format!("#{}", c.name.as_deref().unwrap_or_default())
                    }
                })
                .unwrap_or_default();

            let row = make_message_row(
                msg, users, &subteam_names, client, rt,
                &thread_cb, &mention_cb, &reaction_cb, &delete_cb, &edit_cb,
                Some(channel_id), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
                &self.image_generation, &self.picker_cells,
                &thread_expand_cb, &self.expanded_threads, &self.stored_textures,
            );

            // Prepend a channel name label to the row
            if let Some(child) = row.child() {
                if let Some(outer) = child.downcast_ref::<gtk::Box>() {
                    let ch_label = Label::new(Some(&ch_name));
                    ch_label.add_css_class("dim-label");
                    ch_label.add_css_class("caption");
                    ch_label.set_halign(gtk::Align::Start);
                    ch_label.set_margin_start(52); // align with message text
                    outer.prepend(&ch_label);
                }
            }

            // Make the row clickable for navigation
            if let Some(ref cb) = *search_cb {
                let cb = cb.clone();
                let cid = channel_id.clone();
                let ts = msg.ts.clone();
                // If this is a thread reply (thread_ts is set and differs from ts),
                // pass it along so the parent thread can be opened.
                let thread_ts = msg.thread_ts.clone().filter(|tts| *tts != msg.ts);
                let gesture = gtk::GestureClick::new();
                gesture.connect_released(move |_, _, _, _| {
                    cb(&cid, &ts, thread_ts.as_deref());
                });
                row.add_controller(gesture);
            }

            self.list_box.append(&row);
        }

        self.scroll_to_bottom();
    }

    /// Display search results within the current channel, with highlighted matching text.
    pub fn set_channel_search_results(
        &self,
        query: &str,
        results: &[(Message, String)],
        users: &HashMap<String, String>,
        client: &Client,
        rt: &tokio::runtime::Handle,
    ) {
        self.clear();
        *self.search_query.borrow_mut() = Some(query.to_string());
        self.has_more.set(false); // no scroll-to-top pagination for search yet

        let thread_cb = self.thread_callback.borrow();
        let thread_expand_cb = self.thread_expand_callback.borrow();
        let mention_cb = self.mention_callback.borrow();
        let reaction_cb = self.reaction_callback.borrow();
        let delete_cb = self.delete_callback.borrow();
        let edit_cb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let channel_id = self.channel_id.borrow();
        let search_cb = self.search_result_callback.borrow();
        let subteam_names = self.subteam_names.borrow();

        for (msg, highlighted) in results {
            let row = make_message_row(
                msg, users, &subteam_names, client, rt,
                &thread_cb, &mention_cb, &reaction_cb, &delete_cb, &edit_cb,
                channel_id.as_deref(), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
                &self.image_generation, &self.picker_cells,
                &thread_expand_cb, &self.expanded_threads, &self.stored_textures,
            );

            // Replace the body widget with a highlighted version
            if !highlighted.is_empty() {
                if let Some(child) = row.child() {
                    if let Some(outer) = child.downcast_ref::<gtk::Box>() {
                        if let Some(header) = outer.first_child() {
                            if let Some(old_body) = header.next_sibling() {
                                if old_body.downcast_ref::<Label>().is_some()
                                    || old_body.downcast_ref::<gtk::TextView>().is_some()
                                {
                                    let body = make_highlighted_body(highlighted);
                                    outer.insert_child_after(&body, Some(&header));
                                    outer.remove(&old_body);
                                }
                            }
                        }
                    }
                }
            }

            // Make the row clickable for navigation
            if let Some(ref cb) = *search_cb {
                let cb = cb.clone();
                let cid = channel_id.clone().unwrap_or_default();
                let ts = msg.ts.clone();
                let thread_ts = msg.thread_ts.clone().filter(|tts| *tts != msg.ts);
                let gesture = gtk::GestureClick::new();
                gesture.connect_released(move |_, _, _, _| {
                    cb(&cid, &ts, thread_ts.as_deref());
                });
                row.add_controller(gesture);
            }

            self.list_box.append(&row);
        }

        self.scroll_to_bottom();
    }

    pub fn append_message(
        &self,
        msg: &Message,
        users: &HashMap<String, String>,
        client: &Client,
        rt: &tokio::runtime::Handle,
    ) {
        let thread_cb = self.thread_callback.borrow();
        let thread_expand_cb = self.thread_expand_callback.borrow();
        let mention_cb = self.mention_callback.borrow();
        let reaction_cb = self.reaction_callback.borrow();
        let delete_cb = self.delete_callback.borrow();
        let edit_cb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let channel_id = self.channel_id.borrow();
        let subteam_names = self.subteam_names.borrow();
        let row = make_message_row(
            msg, users, &subteam_names, client, rt,
            &thread_cb, &mention_cb, &reaction_cb, &delete_cb, &edit_cb,
            channel_id.as_deref(), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
            &self.image_generation, &self.picker_cells,
            &thread_expand_cb, &self.expanded_threads, &self.stored_textures,
        );
        self.list_box.append(&row);
        // Lightweight scroll: wait for GTK to lay out the new row (next frame),
        // then snap to bottom. Unlike scroll_to_bottom(), this does not hold a
        // row reference or start a polling timer — safe to call per-message.
        let scrolled = self.scrolled.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            let adj = scrolled.vadjustment();
            adj.set_value(adj.upper() - adj.page_size());
        });
    }

    pub fn scroll_to_bottom(&self) {
        // Find the last row and use the reliable row-bounds-aware scroll.
        // Defer via idle so newly-appended rows are picked up.
        let list_box = self.list_box.clone();
        let scrolled = self.scrolled.clone();
        gtk4::glib::idle_add_local_once(move || {
            let mut idx = 0;
            let mut last = None;
            while let Some(row) = list_box.row_at_index(idx) {
                last = Some(row);
                idx += 1;
            }
            let Some(row) = last else { return };
            // Poll for layout (newly-inserted rows take a frame or two).
            let scrolled = scrolled.clone();
            let list_box = list_box.clone();
            let attempts = std::rc::Rc::new(std::cell::Cell::new(0u32));
            gtk4::glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
                let n = attempts.get();
                attempts.set(n + 1);
                let Some(bounds) = row.compute_bounds(&list_box) else {
                    if n >= 40 {
                        return gtk4::glib::ControlFlow::Break;
                    }
                    return gtk4::glib::ControlFlow::Continue;
                };
                if (bounds.height() as i32) < 20 {
                    if n >= 40 {
                        return gtk4::glib::ControlFlow::Break;
                    }
                    return gtk4::glib::ControlFlow::Continue;
                }
                let adj = scrolled.vadjustment();
                let row_bottom = (bounds.y() + bounds.height()) as f64;
                let target = (row_bottom - adj.page_size()).max(0.0);
                adj.set_value(target);
                if n >= 5 {
                    gtk4::glib::ControlFlow::Break
                } else {
                    gtk4::glib::ControlFlow::Continue
                }
            });
        });
    }

    /// Scroll so the given row's bottom edge aligns with the viewport's bottom.
    /// Polls at short intervals until the row is laid out, then snaps.
    pub fn scroll_row_to_bottom(&self, row: &ListBoxRow) {
        let scrolled = self.scrolled.clone();
        let list_box = self.list_box.clone();
        let row = row.clone();
        let attempts = std::rc::Rc::new(std::cell::Cell::new(0u32));
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
            let n = attempts.get();
            attempts.set(n + 1);
            let Some(bounds) = row.compute_bounds(&list_box) else {
                if n >= 40 {
                    return gtk4::glib::ControlFlow::Break;
                }
                return gtk4::glib::ControlFlow::Continue;
            };
            // Real rows are at least ~20px tall. Anything smaller means the row
            // hasn't been laid out yet.
            if (bounds.height() as i32) < 20 {
                if n >= 40 {
                    return gtk4::glib::ControlFlow::Break;
                }
                return gtk4::glib::ControlFlow::Continue;
            }
            let adj = scrolled.vadjustment();
            let row_bottom = (bounds.y() + bounds.height()) as f64;
            let target = (row_bottom - adj.page_size()).max(0.0);
            adj.set_value(target);
            // Run a few more times to catch any subsequent layout (images, etc.)
            if n >= 5 {
                gtk4::glib::ControlFlow::Break
            } else {
                gtk4::glib::ControlFlow::Continue
            }
        });
    }

    /// Scroll to a specific message by its ts and briefly highlight it.
    /// Deferred to run after GTK completes its layout pass.
    pub fn scroll_to_message(&self, ts: &str) {
        let list_box = self.list_box.clone();
        let scrolled = self.scrolled.clone();
        let ts = ts.to_string();

        // Defer so GTK finishes laying out the new rows first
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
            let mut idx = 0;
            while let Some(row) = list_box.row_at_index(idx) {
                if row.widget_name() == ts {
                    // Select the row
                    list_box.select_row(Some(&row));

                    let adj = scrolled.vadjustment();
                    if let Some(bounds) = row.compute_bounds(&list_box) {
                        let y = bounds.y() as f64;
                        let row_h = bounds.height() as f64;
                        let page = adj.page_size();
                        // Position the message at the bottom of the visible area
                        let target = (y + row_h - page).max(0.0);
                        adj.set_value(target);
                    }

                    // Flash highlight
                    row.add_css_class("notification-highlight");
                    let row_clone = row.clone();
                    gtk4::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(1500),
                        move || {
                            row_clone.remove_css_class("notification-highlight");
                        },
                    );
                    return;
                }
                idx += 1;
            }
        });
    }
}

pub fn make_message_row(
    msg: &Message,
    users: &HashMap<String, String>,
    subteam_names: &HashMap<String, String>,
    client: &Client,
    rt: &tokio::runtime::Handle,
    thread_cb: &Option<ThreadOpenCallback>,
    mention_cb: &Option<MentionCallback>,
    reaction_cb: &Option<ReactionCallback>,
    delete_cb: &Option<DeleteCallback>,
    edit_cb: &Option<EditCallback>,
    channel_id: Option<&str>,
    thread_counts: &HashMap<String, usize>,
    thread_labels: &Rc<RefCell<HashMap<String, Label>>>,
    reaction_boxes: &Rc<RefCell<HashMap<String, gtk::FlowBox>>>,
    self_user_id: &str,
    image_generation: &Rc<Cell<u64>>,
    picker_cells: &Rc<RefCell<Vec<Rc<RefCell<Option<gtk::Popover>>>>>>,
    thread_expand_cb: &Option<ThreadExpandCallback>,
    expanded_threads: &Rc<RefCell<HashMap<String, Vec<ListBoxRow>>>>,
    stored_textures: &Rc<RefCell<Vec<Rc<RefCell<Option<gtk4::gdk::Texture>>>>>>,
) -> ListBoxRow {
    let row = ListBoxRow::new();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.set_margin_top(6);
    outer.set_margin_bottom(6);
    outer.set_margin_start(8);
    outer.set_margin_end(8);

    // Username + timestamp header + thread button (right-aligned)
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let user_id = msg.user.as_deref()
        .or(msg.bot_id.as_deref())
        .unwrap_or("unknown");
    let display_name = users
        .get(user_id)
        .map(|s| s.as_str())
        .unwrap_or(user_id);

    // Clickable name that opens DM
    let name_btn = gtk::Button::with_label(&display_name);
    name_btn.add_css_class("flat");
    name_btn.add_css_class("heading");
    name_btn.add_css_class("message-name-btn");
    name_btn.set_halign(gtk::Align::Start);
    if let Some(mcb) = mention_cb.clone() {
        let uid = user_id.to_string();
        name_btn.connect_clicked(move |_| {
            mcb(&uid);
        });
    }
    header.append(&name_btn);

    let time_str = format_timestamp(&msg.ts);
    let time_label = Label::new(Some(&time_str));
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    time_label.set_halign(gtk::Align::Start);
    time_label.set_hexpand(true);
    header.append(&time_label);

    // Edit & delete buttons (only for own messages)
    let is_own = msg.user.as_deref() == Some(self_user_id) && !self_user_id.is_empty();
    if is_own {
        if let Some(cid) = channel_id {
            // Edit button
            if let Some(ecb) = edit_cb {
                let edit_btn = gtk::Button::from_icon_name("document-edit-symbolic");
                edit_btn.add_css_class("flat");
                edit_btn.add_css_class("edit-btn");
                edit_btn.set_halign(gtk::Align::End);
                edit_btn.set_tooltip_text(Some("Edit message"));

                let ecb = ecb.clone();
                let cid_e = cid.to_string();
                let ts = msg.ts.clone();
                let text = msg.text.clone();
                let outer_ref = outer.clone();
                edit_btn.connect_clicked(move |_| {
                    ecb(&cid_e, &ts, &text, &outer_ref);
                });

                header.append(&edit_btn);
            }

            // Delete button
            if let Some(dcb) = delete_cb {
                let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
                del_btn.add_css_class("flat");
                del_btn.add_css_class("delete-btn");
                del_btn.set_halign(gtk::Align::End);
                del_btn.set_tooltip_text(Some("Delete message"));

                let dcb = dcb.clone();
                let cid_d = cid.to_string();
                let ts = msg.ts.clone();
                let row_weak = row.downgrade();
                del_btn.connect_clicked(move |_| {
                    if let Some(row) = row_weak.upgrade() {
                        dcb(&cid_d, &ts, &row);
                    }
                });

                header.append(&del_btn);
            }
        }
    }

    // Thread expander and Reply button — both always created, visibility toggled
    // based on whether the message currently has a thread. This lets us swap
    // from "Reply" to "N replies" in-place when the first reply is posted.
    if let Some(cid) = channel_id {
        let has_thread = msg
            .thread_ts
            .as_ref()
            .is_some_and(|tts| *tts == msg.ts);

        // Expander button — shown only when the message has a thread
        let is_expanded = expanded_threads.borrow().contains_key(&msg.ts);
        let expand_btn = gtk::Button::new();
        let btn_content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let arrow_label = if is_expanded { "\u{25bc}" } else { "\u{25b6}" };
        let arrow = Label::new(Some(arrow_label));
        arrow.add_css_class("caption");
        let count = thread_counts.get(&msg.ts).copied().unwrap_or(0);
        let label_text = if count > 0 {
            format!("{count} {}", if count == 1 { "reply" } else { "replies" })
        } else {
            "Thread".to_string()
        };
        let label = Label::new(Some(&label_text));
        label.add_css_class("caption");
        btn_content.append(&arrow);
        btn_content.append(&label);
        expand_btn.set_child(Some(&btn_content));
        expand_btn.add_css_class("flat");
        expand_btn.add_css_class("thread-btn");
        expand_btn.set_halign(gtk::Align::End);
        expand_btn.set_visible(has_thread);

        thread_labels.borrow_mut().insert(msg.ts.clone(), label.clone());

        {
            let ts = msg.ts.clone();
            let cid_e = cid.to_string();
            let row_weak = row.downgrade();
            if let Some(expand_cb) = thread_expand_cb.clone() {
                expand_btn.connect_clicked(move |btn| {
                    if let Some(parent_row) = row_weak.upgrade() {
                        expand_cb(&ts, &cid_e, &parent_row);
                        if let Some(child) = btn.child() {
                            if let Some(bx) = child.downcast_ref::<gtk::Box>() {
                                if let Some(first) = bx.first_child() {
                                    if let Some(lbl) = first.downcast_ref::<Label>() {
                                        let current = lbl.text();
                                        if current == "\u{25b6}" {
                                            lbl.set_text("\u{25bc}");
                                        } else {
                                            lbl.set_text("\u{25b6}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
        }
        header.append(&expand_btn);

        // Reply button — shown only when the message has no thread
        if let Some(cb) = thread_cb {
            let reply_btn = gtk::Button::new();
            let btn_content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let icon = gtk::Image::from_icon_name("mail-reply-sender-symbolic");
            let label = Label::new(Some("Reply"));
            label.add_css_class("caption");
            btn_content.append(&icon);
            btn_content.append(&label);
            reply_btn.set_child(Some(&btn_content));
            reply_btn.add_css_class("flat");
            reply_btn.add_css_class("reply-btn");
            reply_btn.set_halign(gtk::Align::End);
            reply_btn.set_visible(!has_thread);

            // For a non-threaded message, replying uses its own ts as the new thread_ts.
            let reply_thread_ts = msg.thread_ts.clone().unwrap_or_else(|| msg.ts.clone());
            let cid_r = cid.to_string();
            let cb = cb.clone();
            let user_display = display_name.to_string();
            let preview = msg.text.clone();
            reply_btn.connect_clicked(move |_| {
                cb(&reply_thread_ts, &cid_r, &user_display, &preview);
            });

            header.append(&reply_btn);
        }
    }

    outer.append(&header);

    // Message body with clickable @mentions and inline custom emoji
    if !msg.text.is_empty() {
        let body = make_message_body(&msg.text, users, subteam_names, mention_cb);
        outer.append(&body);
    }

    // Render attachment text content (title, pretext, text, fallback)
    // This is crucial for bot/integration messages (e.g. GitHub) that put content in attachments
    if let Some(attachments) = &msg.attachments {
        for att in attachments {
            let render_field = |text: &str| -> String {
                if looks_like_html(text) {
                    html_to_pango(text)
                } else {
                    format_message_markup(text, users, subteam_names)
                }
            };

            let mut markup_parts: Vec<String> = Vec::new();
            if let Some(pretext) = &att.pretext {
                if !pretext.is_empty() {
                    markup_parts.push(render_field(pretext));
                }
            }
            if let Some(title) = &att.title {
                if !title.is_empty() {
                    let title_markup = match &att.title_link {
                        Some(link) if !link.is_empty() => format!(
                            "<a href=\"{}\"><b>{}</b></a>",
                            gtk4::glib::markup_escape_text(link),
                            gtk4::glib::markup_escape_text(title),
                        ),
                        _ => format!("<b>{}</b>", gtk4::glib::markup_escape_text(title)),
                    };
                    markup_parts.push(title_markup);
                }
            }
            if let Some(text) = &att.text {
                if !text.is_empty() {
                    markup_parts.push(render_field(text));
                }
            }
            if markup_parts.is_empty() {
                if let Some(fallback) = &att.fallback {
                    if !fallback.is_empty() {
                        markup_parts.push(render_field(fallback));
                    }
                }
            }

            if !markup_parts.is_empty() {
                let markup = markup_parts.join("\n");

                let att_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);

                // Color bar (left border)
                if let Some(color) = &att.color {
                    let bar = gtk::DrawingArea::new();
                    bar.set_size_request(3, -1);
                    bar.set_vexpand(true);
                    let color = color.clone();
                    bar.set_draw_func(move |_, cr, _w, h| {
                        let hex = color.trim_start_matches('#');
                        if hex.len() == 6 {
                            if let (Ok(r), Ok(g), Ok(b)) = (
                                u8::from_str_radix(&hex[0..2], 16),
                                u8::from_str_radix(&hex[2..4], 16),
                                u8::from_str_radix(&hex[4..6], 16),
                            ) {
                                cr.set_source_rgb(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
                                cr.rectangle(0.0, 0.0, 3.0, h as f64);
                                let _ = cr.fill();
                            }
                        }
                    });
                    att_box.append(&bar);
                }

                let att_label = Label::new(None);
                att_label.set_markup(&markup);
                att_label.set_wrap(true);
                att_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                att_label.set_halign(gtk::Align::Fill);
                att_label.set_hexpand(true);
                att_label.set_selectable(true);
                att_label.set_xalign(0.0);
                att_label.set_margin_start(6);

                if let Some(cb) = mention_cb.clone() {
                    att_label.connect_activate_link(move |_, uri| {
                        if let Some(user_id) = uri.strip_prefix("mention:") {
                            cb(user_id);
                            return gtk4::glib::Propagation::Stop;
                        }
                        gtk4::glib::Propagation::Proceed
                    });
                }

                att_box.append(&att_label);
                outer.append(&att_box);
            }
        }
    }

    // Collect image URLs to load (with fallback candidates per image)
    let mut image_url_sets: Vec<Vec<String>> = Vec::new();

    // File attachments
    if let Some(files) = &msg.files {
        for file in files {
            let is_image = file
                .mimetype
                .as_deref()
                .is_some_and(|m| m.starts_with("image/"));
            if is_image {
                let mut candidates = Vec::new();
                if let Some(url) = &file.url_private_download {
                    candidates.push(url.clone());
                }
                if let Some(url) = &file.url_private {
                    candidates.push(url.clone());
                }
                if let Some(url) = &file.permalink {
                    candidates.push(url.clone());
                }
                if !candidates.is_empty() {
                    image_url_sets.push(candidates);
                }
            } else {
                // Non-image file: show download chip
                let download_url = file.url_private_download.as_deref()
                    .or(file.url_private.as_deref());
                let chip = make_file_attachment_chip(file, download_url, client, rt);
                outer.append(&chip);
            }
        }
    }

    // Images from attachments (link previews)
    if let Some(attachments) = &msg.attachments {
        for att in attachments {
            let mut candidates = Vec::new();
            if let Some(url) = &att.image_url {
                candidates.push(url.clone());
            }
            if let Some(url) = &att.thumb_url {
                candidates.push(url.clone());
            }
            if !candidates.is_empty() {
                image_url_sets.push(candidates);
            }
        }
    }

    // Create placeholder pictures and load them asynchronously
    let image_flow = gtk::FlowBox::new();
    image_flow.set_selection_mode(gtk::SelectionMode::None);
    image_flow.set_halign(gtk::Align::Start);
    image_flow.set_homogeneous(false);
    image_flow.set_row_spacing(4);
    image_flow.set_column_spacing(4);
    image_flow.set_max_children_per_line(10);
    if !image_url_sets.is_empty() {
        outer.append(&image_flow);
    }

    for urls in image_url_sets {
        let picture = Picture::new();
        picture.set_halign(gtk::Align::Start);
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        // Fixed size placeholder so layout is stable before image loads
        picture.set_size_request(200, 150);
        picture.set_can_shrink(true);
        picture.set_cursor_from_name(Some("pointer"));
        image_flow.insert(&picture, -1);

        // Click to open fullscreen viewer (loads full resolution)
        let stored_texture: Rc<RefCell<Option<gtk4::gdk::Texture>>> = Rc::new(RefCell::new(None));
        stored_textures.borrow_mut().push(stored_texture.clone());
        {
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let tex = stored_texture.clone();
            let click_urls = urls.clone();
            let click_client = client.clone();
            let click_rt = rt.clone();
            // Use weak ref to avoid Picture -> GestureClick -> closure -> Picture cycle
            let picture_weak = picture.downgrade();
            click.connect_released(move |_, _, _, _| {
                let Some(picture) = picture_weak.upgrade() else { return };
                if let Some(texture) = tex.borrow().as_ref() {
                    if let Some(root) = picture.root() {
                        if let Some(win) = root.downcast_ref::<gtk::Window>() {
                            crate::ui::image_viewer::show(
                                texture,
                                win,
                                Some(click_urls.clone()),
                                Some(click_client.clone()),
                                Some(click_rt.clone()),
                            );
                        }
                    }
                }
            });
            picture.add_controller(click);
        }

        let client = client.clone();
        let rt = rt.clone();
        let picture_weak = picture.downgrade();
        let img_gen = image_generation.clone();
        let gen_at_start = img_gen.get();
        // Use Weak so the future doesn't keep the texture alive after the row is freed
        let stored_texture_weak = Rc::downgrade(&stored_texture);
        // Fetch all candidate URLs on tokio, decode + display on main thread
        gtk4::glib::spawn_future_local(async move {
            // Move client + urls directly into the tokio task so they are freed as
            // soon as the HTTP request finishes, rather than lingering in the outer
            // future's state until the Texture decode completes on the main thread.
            let bytes_result = rt.spawn(async move {
                for url in &urls {
                    match client.fetch_image_bytes(url).await {
                        Ok(b) => {
                            tracing::debug!("Image loaded from {url}");
                            return Ok(b);
                        }
                        Err(e) => {
                            tracing::debug!("Image URL failed ({url}): {e}");
                        }
                    }
                }
                Err("all URLs failed".to_string())
            }).await;

            if img_gen.get() != gen_at_start { return; }
            let Some(picture) = picture_weak.upgrade() else { return; };
            let Some(stored_texture) = stored_texture_weak.upgrade() else { return; };

            match bytes_result {
                Ok(Ok(bytes)) => {
                    let gbytes = gtk4::glib::Bytes::from_owned(bytes);
                    let stream = gtk4::gio::MemoryInputStream::from_bytes(&gbytes);
                    // Decode at display size to avoid holding full-resolution
                    // RGBA buffers (a 5712x4284 photo = 93 MiB).
                    match gtk4::gdk_pixbuf::Pixbuf::from_stream_at_scale(
                        &stream,
                        400,  // max width
                        150,  // max height
                        true, // preserve_aspect_ratio
                        gtk4::gio::Cancellable::NONE,
                    ) {
                        Ok(pixbuf) => {
                            let w = pixbuf.width();
                            let h = pixbuf.height();
                            tracing::debug!("Image decoded (scaled): {w}x{h}");
                            let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                            picture.set_paintable(Some(&texture));
                            *stored_texture.borrow_mut() = Some(texture);
                        }
                        Err(e) => {
                            tracing::debug!("Failed to decode image: {e}");
                            picture.set_visible(false);
                        }
                    }
                }
                _ => {
                    picture.set_visible(false);
                }
            }
        });
    }

    // ── Reactions display + add button ──
    {
        let reactions_box = gtk::FlowBox::new();
        reactions_box.set_selection_mode(gtk::SelectionMode::None);
        reactions_box.set_halign(gtk::Align::Start);
        reactions_box.set_max_children_per_line(20);
        reactions_box.set_homogeneous(false);
        reactions_box.set_row_spacing(2);
        reactions_box.set_column_spacing(4);

        // Show existing reactions
        if let Some(reactions) = &msg.reactions {
            for reaction in reactions {
                let btn = make_reaction_button(
                    reaction, users, &msg.ts,
                    reaction_cb, channel_id,
                );
                reactions_box.insert(&btn, -1);
            }
        }

        // Add reaction button with emoji autocomplete popover
        if let (Some(rcb), Some(cid)) = (reaction_cb, channel_id) {
            let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
            add_btn.add_css_class("flat");
            add_btn.add_css_class("reaction-add-btn");

            let rcb = rcb.clone();
            let cid = cid.to_string();
            let ts = msg.ts.clone();
            let picker_cell: Rc<RefCell<Option<gtk::Popover>>> = Rc::new(RefCell::new(None));
            picker_cells.borrow_mut().push(picker_cell.clone());

            let btn_weak = add_btn.downgrade();
            add_btn.connect_clicked(move |_| {
                let Some(btn) = btn_weak.upgrade() else { return };
                let first_show = picker_cell.borrow().is_none();
                if first_show {
                    let rcb = rcb.clone();
                    let cid = cid.clone();
                    let ts = ts.clone();
                    let on_react: Rc<dyn Fn(&str)> = Rc::new(move |shortcode: &str| {
                        let dummy = gtk::Button::new();
                        rcb(&cid, &ts, shortcode, &dummy);
                    });
                    let picker = crate::ui::autocomplete::build_reaction_popover(&btn, on_react);
                    *picker_cell.borrow_mut() = Some(picker);
                }
                if let Some(picker) = picker_cell.borrow().as_ref() {
                    if first_show {
                        // Defer popup so the popover widget tree is realized first
                        let p = picker.clone();
                        gtk4::glib::idle_add_local_once(move || p.popup());
                    } else {
                        picker.popup();
                    }
                }
            });

            reactions_box.insert(&add_btn, -1);
        }

        reaction_boxes.borrow_mut().insert(msg.ts.clone(), reactions_box.clone());
        outer.append(&reactions_box);
    }

    row.set_child(Some(&outer));
    row.set_widget_name(&msg.ts);
    row
}

fn format_timestamp(ts: &str) -> String {
    // Slack timestamps are "epoch.sequence"
    let epoch_str = ts.split('.').next().unwrap_or(ts);
    if let Ok(epoch) = epoch_str.parse::<i64>() {
        let dt = chrono::DateTime::from_timestamp(epoch, 0);
        if let Some(dt) = dt {
            return dt
                .with_timezone(&chrono::Local)
                .format("%d %b %Y %H:%M")
                .to_string();
        }
    }
    ts.to_string()
}

/// Create a reaction button with right-click to show who reacted.
pub fn make_reaction_button(
    reaction: &slacko::types::Reaction,
    users: &HashMap<String, String>,
    msg_ts: &str,
    reaction_cb: &Option<ReactionCallback>,
    channel_id: Option<&str>,
) -> gtk::Button {
    use crate::slack::helpers::{resolve_slack_shortcode, get_custom_emoji_path};

    let btn = gtk::Button::new();
    btn.add_css_class("flat");
    btn.add_css_class("reaction-btn");

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);

    if let Some(emoji_str) = resolve_slack_shortcode(&reaction.name) {
        // Standard emoji
        let label = Label::new(Some(&format!("{emoji_str} {}", reaction.count)));
        content.append(&label);
    } else if let Some(path) = get_custom_emoji_path(&reaction.name) {
        // Custom emoji — render as small image
        if let Ok(pixbuf) = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(&path, 18, 18, true) {
            let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
            let image = gtk::Image::from_paintable(Some(&texture));
            image.set_pixel_size(18);
            content.append(&image);
        } else {
            let label = Label::new(Some(&format!(":{}: ", reaction.name)));
            content.append(&label);
        }
        let count_label = Label::new(Some(&format!("{}", reaction.count)));
        content.append(&count_label);
    } else {
        // Unknown emoji — show name
        let label = Label::new(Some(&format!(":{}: {}", reaction.name, reaction.count)));
        content.append(&label);
    }

    btn.set_child(Some(&content));

    // Left-click toggles the reaction
    if let (Some(rcb), Some(cid)) = (reaction_cb, channel_id) {
        let rcb = rcb.clone();
        let name = reaction.name.clone();
        let ts = msg_ts.to_string();
        let cid = cid.to_string();
        btn.connect_clicked(move |btn| {
            rcb(&cid, &ts, &name, btn);
        });
    }

    // Right-click shows who reacted
    let user_names: Vec<&str> = reaction
        .users
        .iter()
        .map(|uid| {
            users
                .get(uid)
                .map(|s| s.as_str())
                .unwrap_or(uid)
        })
        .collect();

    if !user_names.is_empty() {
        let popover = gtk::Popover::new();
        let list_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        list_box.set_margin_top(8);
        list_box.set_margin_bottom(8);
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);

        let emoji_display = resolve_slack_shortcode(&reaction.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let header = Label::new(Some(&format!("{} :{}: ", emoji_display, reaction.name)));
        header.add_css_class("heading");
        header.set_halign(gtk::Align::Start);
        list_box.append(&header);

        for name in &user_names {
            let label = Label::new(Some(name));
            label.set_halign(gtk::Align::Start);
            label.add_css_class("caption");
            list_box.append(&label);
        }

        popover.set_child(Some(&list_box));
        popover.set_parent(&btn);
        popover.set_autohide(true);

        let gesture = gtk::GestureClick::new();
        gesture.set_button(3); // right-click
        let popover_weak = popover.downgrade();
        gesture.connect_released(move |_, _, _, _| {
            if let Some(p) = popover_weak.upgrade() {
                p.popup();
            }
        });
        btn.add_controller(gesture);
    }

    btn
}

/// Build a widget showing a non-image file attachment with a download button.
pub fn make_file_attachment_chip(
    file: &slacko::types::File,
    download_url: Option<&str>,
    client: &Client,
    rt: &tokio::runtime::Handle,
) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    chip.add_css_class("card");
    chip.set_margin_top(4);
    chip.set_margin_bottom(4);
    chip.set_halign(gtk::Align::Start);

    let icon = gtk::Image::from_icon_name("document-save-symbolic");
    icon.set_margin_start(10);
    icon.set_margin_top(6);
    icon.set_margin_bottom(6);
    chip.append(&icon);

    let info_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    info_box.set_margin_top(6);
    info_box.set_margin_bottom(6);

    let name = file.name.as_deref()
        .or(file.title.as_deref())
        .unwrap_or("file");
    let name_label = Label::new(Some(name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    name_label.set_max_width_chars(40);
    name_label.add_css_class("heading");
    info_box.append(&name_label);

    let mut detail_parts: Vec<String> = Vec::new();
    if let Some(size) = file.size {
        detail_parts.push(format_file_size(size));
    }
    if let Some(ft) = &file.filetype {
        detail_parts.push(ft.to_uppercase());
    }
    if !detail_parts.is_empty() {
        let detail_label = Label::new(Some(&detail_parts.join(" · ")));
        detail_label.set_halign(gtk::Align::Start);
        detail_label.add_css_class("dim-label");
        detail_label.add_css_class("caption");
        info_box.append(&detail_label);
    }

    chip.append(&info_box);

    if let Some(url) = download_url {
        // ── View button: download to cache and open with system default viewer ──
        let view_btn = gtk::Button::from_icon_name("document-open-symbolic");
        view_btn.add_css_class("flat");
        view_btn.set_valign(gtk::Align::Center);
        view_btn.set_tooltip_text(Some("Open with system viewer"));

        {
            let url = url.to_string();
            let filename = name.to_string();
            let file_id = file.id.clone();
            let client = client.clone();
            let rt = rt.clone();
            view_btn.connect_clicked(move |btn| {
                let url = url.clone();
                let filename = filename.clone();
                let file_id = file_id.clone();
                let client = client.clone();
                let rt = rt.clone();
                let btn_weak = btn.downgrade();

                btn.set_sensitive(false);
                btn.set_icon_name("process-working-symbolic");

                gtk4::glib::spawn_future_local(async move {
                    let result = ensure_file_cached(&client, &rt, &file_id, &filename, &url).await;

                    if let Some(btn) = btn_weak.upgrade() {
                        btn.set_sensitive(true);
                        btn.set_icon_name("document-open-symbolic");
                    }

                    match result {
                        Ok(path) => {
                            let uri = gtk4::gio::File::for_path(&path).uri();
                            if let Err(e) = gtk4::gio::AppInfo::launch_default_for_uri(
                                &uri,
                                gtk4::gio::AppLaunchContext::NONE,
                            ) {
                                tracing::error!("Failed to open file {}: {e}", path.display());
                                if let Some(btn) = btn_weak.upgrade() {
                                    btn.set_icon_name("dialog-error-symbolic");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to cache file for viewing: {e}");
                            if let Some(btn) = btn_weak.upgrade() {
                                btn.set_icon_name("dialog-error-symbolic");
                            }
                        }
                    }
                });
            });
        }

        chip.append(&view_btn);

        // ── Save button: prompt for a location and write the file there ──
        let dl_btn = gtk::Button::from_icon_name("folder-download-symbolic");
        dl_btn.add_css_class("flat");
        dl_btn.set_valign(gtk::Align::Center);
        dl_btn.set_margin_end(6);
        dl_btn.set_tooltip_text(Some("Save file"));

        let url = url.to_string();
        let filename = name.to_string();
        let client = client.clone();
        let rt = rt.clone();
        dl_btn.connect_clicked(move |btn| {
            let Some(root) = btn.root() else { return };
            let Some(win) = root.downcast_ref::<gtk::Window>() else { return };

            let dialog = gtk::FileDialog::new();
            dialog.set_initial_name(Some(&filename));

            let url = url.clone();
            let client = client.clone();
            let rt = rt.clone();
            let btn_weak = btn.downgrade();
            dialog.save(Some(win), gtk4::gio::Cancellable::NONE, move |result| {
                let Ok(gfile) = result else { return };
                let Some(path) = gfile.path() else { return };

                // Show spinner on button while downloading
                let btn_ref = btn_weak.upgrade();
                if let Some(btn) = &btn_ref {
                    btn.set_sensitive(false);
                    btn.set_icon_name("process-working-symbolic");
                }

                let btn_weak2 = btn_weak.clone();
                gtk4::glib::spawn_future_local(async move {
                    let c = client.clone();
                    let u = url.clone();
                    let p = path.clone();
                    let result = rt.spawn(async move {
                        let bytes = c.fetch_image_bytes(&u).await?;
                        tokio::fs::write(&p, &bytes).await
                            .map_err(|e| format!("Write error: {e}"))
                    }).await;

                    if let Some(btn) = btn_weak2.upgrade() {
                        btn.set_sensitive(true);
                        match result {
                            Ok(Ok(())) => {
                                btn.set_icon_name("emblem-ok-symbolic");
                            }
                            _ => {
                                btn.set_icon_name("dialog-error-symbolic");
                                tracing::error!("Failed to download file");
                            }
                        }
                    }
                });
            });
        });

        chip.append(&dl_btn);
    }

    chip
}

/// Ensure a file is present in the per-user cache and return its path.
/// Files live under `~/.local/share/sludge/file_cache/{file_id}/{name}` so the
/// original filename (and therefore its extension) is preserved — GIO relies
/// on that to pick the right default handler.
async fn ensure_file_cached(
    client: &Client,
    rt: &tokio::runtime::Handle,
    file_id: &str,
    name: &str,
    url: &str,
) -> Result<std::path::PathBuf, String> {
    let cache_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("sludge")
        .join("file_cache")
        .join(file_id);
    let safe_name = sanitize_filename(name);
    let cache_path = cache_dir.join(&safe_name);

    if std::fs::metadata(&cache_path).is_ok() {
        return Ok(cache_path);
    }

    let client = client.clone();
    let url = url.to_string();
    let cache_dir2 = cache_dir.clone();
    let cache_path2 = cache_path.clone();
    rt.spawn(async move {
        let bytes = client.fetch_image_bytes(&url).await?;
        tokio::fs::create_dir_all(&cache_dir2)
            .await
            .map_err(|e| format!("Create cache dir: {e}"))?;
        tokio::fs::write(&cache_path2, &bytes)
            .await
            .map_err(|e| format!("Write cache file: {e}"))
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))??;

    Ok(cache_path)
}

/// Strip path separators and other problematic characters so a Slack-supplied
/// filename can't escape the cache subdirectory.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | '\0') { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Build a message body widget from Slack text.
/// Uses a plain Label for messages without custom emoji,
/// or a non-editable TextView with inline emoji images for messages containing them.
pub fn make_message_body(
    text: &str,
    users: &HashMap<String, String>,
    subteam_names: &HashMap<String, String>,
    mention_cb: &Option<MentionCallback>,
) -> gtk::Widget {
    use crate::slack::helpers::{extract_custom_emoji, get_custom_emoji_path};

    let custom_emoji = extract_custom_emoji(text);

    if custom_emoji.is_empty() {
        // No custom emoji — use a simple Label with Pango markup
        let markup = format_message_markup(text, users, subteam_names);
        let body = Label::new(None);
        body.set_markup(&markup);
        body.set_wrap(true);
        body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        body.set_halign(gtk::Align::Fill);
        body.set_selectable(true);
        body.set_xalign(0.0);

        if let Some(cb) = mention_cb.clone() {
            body.connect_activate_link(move |_, uri| {
                if let Some(user_id) = uri.strip_prefix("mention:") {
                    cb(user_id);
                    return gtk4::glib::Propagation::Stop;
                }
                gtk4::glib::Propagation::Proceed
            });
        }

        body.upcast()
    } else {
        // Has custom emoji — use a TextView with inline paintables
        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        text_view.add_css_class("message-body-textview");

        let buffer = text_view.buffer();

        // Build plain text (with U+FFFC placeholders for custom emoji) and insert paintables
        let plain = crate::slack::helpers::format_message_plain(text, users, subteam_names);
        let custom_emoji_reextracted = extract_custom_emoji(text);

        // Split plain text at U+FFFC boundaries and insert paintables
        let parts: Vec<&str> = plain.split('\u{FFFC}').collect();
        let mut emoji_idx = 0;

        for (i, part) in parts.iter().enumerate() {
            if !part.is_empty() {
                let mut end = buffer.end_iter();
                buffer.insert(&mut end, part);
            }
            // Insert custom emoji image after each split (except after the last part)
            if i < parts.len() - 1 {
                if let Some(shortcode) = custom_emoji_reextracted.get(emoji_idx) {
                    if let Some(path) = get_custom_emoji_path(shortcode) {
                        if let Ok(pixbuf) = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(
                            &path, 20, 20, true,
                        ) {
                            let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                            let mut end = buffer.end_iter();
                            buffer.insert_paintable(&mut end, &texture);
                        } else {
                            // Fallback: show the shortcode as text
                            let mut end = buffer.end_iter();
                            buffer.insert(&mut end, &format!(":{shortcode}:"));
                        }
                    } else {
                        let mut end = buffer.end_iter();
                        buffer.insert(&mut end, &format!(":{shortcode}:"));
                    }
                    emoji_idx += 1;
                }
            }
        }

        text_view.set_halign(gtk::Align::Fill);
        text_view.set_hexpand(true);
        text_view.upcast()
    }
}

fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Build a body label from text with `<b>...</b>` highlight markers.
/// Escapes everything except the bold tags for safe Pango markup.
fn make_highlighted_body(highlighted: &str) -> gtk::Widget {
    // Split on <b> and </b> tags, escape each segment, then reassemble
    let mut markup = String::new();
    let mut rest = highlighted;
    while let Some(start) = rest.find("<b>") {
        // Escape text before the tag
        markup.push_str(&gtk4::glib::markup_escape_text(&rest[..start]));
        rest = &rest[start + 3..];
        // Find closing </b>
        if let Some(end) = rest.find("</b>") {
            markup.push_str("<span background=\"yellow\" foreground=\"black\">");
            markup.push_str(&gtk4::glib::markup_escape_text(&rest[..end]));
            markup.push_str("</span>");
            rest = &rest[end + 4..];
        } else {
            // No closing tag — escape the rest and break
            markup.push_str(&gtk4::glib::markup_escape_text(rest));
            rest = "";
            break;
        }
    }
    // Escape remaining text after the last tag
    if !rest.is_empty() {
        markup.push_str(&gtk4::glib::markup_escape_text(rest));
    }

    let body = Label::new(None);
    body.set_markup(&markup);
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    body.set_halign(gtk::Align::Fill);
    body.set_selectable(true);
    body.set_xalign(0.0);
    body.upcast()
}

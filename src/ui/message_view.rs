use gtk4::prelude::*;
use gtk4::{self as gtk, Label, ListBox, ListBoxRow, Picture, ScrolledWindow};
use slacko::types::Message;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::slack::client::Client;
use crate::slack::helpers::format_message_markup;

/// Callback type for opening a thread: (thread_ts, channel_id)
pub type ThreadOpenCallback = Rc<dyn Fn(&str, &str)>;

/// Callback type for opening a DM with a user: (user_id)
pub type MentionCallback = Rc<dyn Fn(&str)>;

/// Callback type for toggling a reaction: (channel_id, message_ts, emoji_name, button)
pub type ReactionCallback = Rc<dyn Fn(&str, &str, &str, &gtk::Button)>;

/// Callback type for deleting a message: (channel_id, message_ts, row)
pub type DeleteCallback = Rc<dyn Fn(&str, &str, &ListBoxRow)>;

pub struct MessageView {
    pub widget: gtk::Box,
    pub list_box: ListBox,
    scrolled: ScrolledWindow,
    header_label: Label,
    pub spinner: gtk::Spinner,
    thread_callback: RefCell<Option<ThreadOpenCallback>>,
    mention_callback: RefCell<Option<MentionCallback>>,
    reaction_callback: RefCell<Option<ReactionCallback>>,
    delete_callback: RefCell<Option<DeleteCallback>>,
    self_user_id: RefCell<String>,
    channel_id: RefCell<Option<String>>,
    /// Known thread reply counts: thread_ts -> reply count (excluding parent).
    pub thread_counts: Rc<RefCell<HashMap<String, usize>>>,
    /// Thread button labels keyed by message ts, for updating counts after loading.
    thread_labels: Rc<RefCell<HashMap<String, Label>>>,
    /// Reaction FlowBox containers keyed by message ts, for live reaction updates.
    reaction_boxes: Rc<RefCell<HashMap<String, gtk::FlowBox>>>,
    /// Typing indicator label in the channel header.
    pub typing_label: Label,
    /// Generation counter; incremented on clear() so in-flight image loads detect staleness.
    image_generation: Rc<Cell<u64>>,
    /// Emoji chooser cells — take + unparent on clear since tree walk can miss them.
    emoji_chooser_cells: Rc<RefCell<Vec<Rc<RefCell<Option<gtk::EmojiChooser>>>>>>,
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
        header.append(&header_label);

        let typing_label = Label::new(None);
        typing_label.add_css_class("dim-label");
        typing_label.add_css_class("caption");
        typing_label.set_halign(gtk::Align::End);
        typing_label.set_visible(false);
        header.append(&typing_label);

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        spinner.set_size_request(16, 16);
        header.append(&spinner);

        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);

        container.append(&header);
        container.append(&separator);

        // Messages list
        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::None);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(8);
        list_box.set_margin_end(8);

        let scrolled = ScrolledWindow::new();
        scrolled.set_vexpand(true);
        scrolled.set_hexpand(true);
        scrolled.set_child(Some(&list_box));

        container.append(&scrolled);

        // Auto-scroll to bottom when content grows (e.g. images loading),
        // but only if the user was already near the bottom.
        let adj = scrolled.vadjustment();
        adj.connect_upper_notify(|adj| {
            let max_scroll = adj.upper() - adj.page_size();
            if max_scroll <= 0.0 {
                return;
            }
            let distance_from_bottom = max_scroll - adj.value();
            if distance_from_bottom < 400.0 {
                adj.set_value(max_scroll);
            }
        });

        Self {
            widget: container,
            list_box,
            scrolled,
            header_label,
            spinner,
            thread_callback: RefCell::new(None),
            mention_callback: RefCell::new(None),
            reaction_callback: RefCell::new(None),
            delete_callback: RefCell::new(None),
            self_user_id: RefCell::new(String::new()),
            channel_id: RefCell::new(None),
            thread_counts: Rc::new(RefCell::new(HashMap::new())),
            thread_labels: Rc::new(RefCell::new(HashMap::new())),
            reaction_boxes: Rc::new(RefCell::new(HashMap::new())),
            typing_label,
            image_generation: Rc::new(Cell::new(0)),
            emoji_chooser_cells: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn set_thread_callback(&self, cb: ThreadOpenCallback) {
        *self.thread_callback.borrow_mut() = Some(cb);
    }

    /// Invoke the thread open callback programmatically.
    pub fn open_thread(&self, thread_ts: &str, channel_id: &str) {
        if let Some(cb) = self.thread_callback.borrow().as_ref() {
            cb(thread_ts, channel_id);
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

    pub fn set_self_user_id(&self, uid: &str) {
        *self.self_user_id.borrow_mut() = uid.to_string();
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

    pub fn clear(&self) {
        crate::mem::log_mem("MessageView::clear() START");

        // Bump generation so in-flight image downloads from the previous channel bail out
        self.image_generation.set(self.image_generation.get() + 1);

        // Explicitly unparent all tracked emoji choosers — the tree walk may miss
        // popovers because GTK4 set_parent'd popovers don't always appear in
        // first_child() traversal depending on the container nesting.
        for cell in self.emoji_chooser_cells.borrow_mut().drain(..) {
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
        crate::mem::trim_heap();
        crate::mem::log_mem("MessageView::clear() DONE (after trim)");

        let thread_cb = self.thread_callback.borrow().clone();
        let mention_cb = self.mention_callback.borrow().clone();
        let reaction_cb = self.reaction_callback.borrow().clone();
        let delete_cb = self.delete_callback.borrow().clone();
        let self_uid = self.self_user_id.borrow().clone();
        let channel_id = self.channel_id.borrow().clone();
        let tc = self.thread_counts.clone();
        let tl = self.thread_labels.clone();
        let rb = self.reaction_boxes.clone();

        let img_gen = self.image_generation.clone();
        let ecc = self.emoji_chooser_cells.clone();

        // Slack returns newest-first; display oldest-first
        for msg in messages.iter().rev() {
            let row = make_message_row(
                msg, users, client, rt,
                &thread_cb, &mention_cb, &reaction_cb, &delete_cb,
                channel_id.as_deref(), &tc.borrow(), &tl, &rb, &self_uid,
                &img_gen, &ecc,
            );
            self.list_box.append(&row);
        }

        self.scroll_to_bottom();
        crate::mem::log_mem("MessageView::set_messages() DONE");
    }

    pub fn append_message(
        &self,
        msg: &Message,
        users: &HashMap<String, String>,
        client: &Client,
        rt: &tokio::runtime::Handle,
    ) {
        let thread_cb = self.thread_callback.borrow().clone();
        let mention_cb = self.mention_callback.borrow().clone();
        let reaction_cb = self.reaction_callback.borrow().clone();
        let delete_cb = self.delete_callback.borrow().clone();
        let self_uid = self.self_user_id.borrow().clone();
        let channel_id = self.channel_id.borrow().clone();
        let tc = self.thread_counts.clone();
        let tl = self.thread_labels.clone();
        let rb = self.reaction_boxes.clone();
        let img_gen = self.image_generation.clone();
        let ecc = self.emoji_chooser_cells.clone();
        let row = make_message_row(
            msg, users, client, rt,
            &thread_cb, &mention_cb, &reaction_cb, &delete_cb,
            channel_id.as_deref(), &tc.borrow(), &tl, &rb, &self_uid,
            &img_gen, &ecc,
        );
        self.list_box.append(&row);
        self.scroll_to_bottom();
    }

    fn scroll_to_bottom(&self) {
        let adj = self.scrolled.vadjustment();
        // Defer to let GTK lay out the new widget first
        gtk4::glib::idle_add_local_once(move || {
            adj.set_value(adj.upper() - adj.page_size());
        });
    }

    /// Scroll to a specific message by its ts and briefly highlight it.
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
                    let adj = scrolled.vadjustment();
                    if let Some(bounds) = row.compute_bounds(&list_box) {
                        let y = bounds.y() as f64;
                        let page = adj.page_size();
                        let target = (y - page / 2.0).max(0.0);
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

fn make_message_row(
    msg: &Message,
    users: &HashMap<String, String>,
    client: &Client,
    rt: &tokio::runtime::Handle,
    thread_cb: &Option<ThreadOpenCallback>,
    mention_cb: &Option<MentionCallback>,
    reaction_cb: &Option<ReactionCallback>,
    delete_cb: &Option<DeleteCallback>,
    channel_id: Option<&str>,
    thread_counts: &HashMap<String, usize>,
    thread_labels: &Rc<RefCell<HashMap<String, Label>>>,
    reaction_boxes: &Rc<RefCell<HashMap<String, gtk::FlowBox>>>,
    self_user_id: &str,
    image_generation: &Rc<Cell<u64>>,
    emoji_chooser_cells: &Rc<RefCell<Vec<Rc<RefCell<Option<gtk::EmojiChooser>>>>>>,
) -> ListBoxRow {
    let row = ListBoxRow::new();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.set_margin_top(6);
    outer.set_margin_bottom(6);
    outer.set_margin_start(8);
    outer.set_margin_end(8);

    // Username + timestamp header + thread button (right-aligned)
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let user_id = msg.user.as_deref().unwrap_or("unknown");
    let display_name = users
        .get(user_id)
        .cloned()
        .unwrap_or_else(|| user_id.to_string());

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

    // Thread button in the header (right-aligned)
    if let (Some(cb), Some(cid)) = (thread_cb, channel_id) {
        let has_thread = msg
            .thread_ts
            .as_ref()
            .is_some_and(|tts| *tts == msg.ts);

        let thread_btn = gtk::Button::new();
        let btn_content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let icon = gtk::Image::from_icon_name(if has_thread {
            "chat-message-new-symbolic"
        } else {
            "mail-reply-sender-symbolic"
        });
        let label_text = if has_thread {
            let count = thread_counts.get(&msg.ts).copied().unwrap_or(0);
            if count > 0 {
                format!("{count} {}", if count == 1 { "reply" } else { "replies" })
            } else {
                "Thread".to_string()
            }
        } else {
            "Reply".to_string()
        };
        let label = Label::new(Some(&label_text));
        label.add_css_class("caption");
        btn_content.append(&icon);
        btn_content.append(&label);
        thread_btn.set_child(Some(&btn_content));
        thread_btn.add_css_class("flat");
        thread_btn.add_css_class("thread-btn");
        thread_btn.set_halign(gtk::Align::End);

        // Register label for dynamic count updates (all messages, so first
        // reply to a message can update the "Reply" button to show "1 reply")
        thread_labels.borrow_mut().insert(msg.ts.clone(), label.clone());

        let ts = if has_thread {
            msg.thread_ts.clone().unwrap_or_else(|| msg.ts.clone())
        } else {
            msg.ts.clone()
        };
        let cid = cid.to_string();
        let cb = cb.clone();
        thread_btn.connect_clicked(move |_| {
            cb(&ts, &cid);
        });

        header.append(&thread_btn);
    }

    // Delete button (only for own messages)
    let is_own = msg.user.as_deref() == Some(self_user_id) && !self_user_id.is_empty();
    if is_own {
        if let (Some(dcb), Some(cid)) = (delete_cb, channel_id) {
            let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            del_btn.add_css_class("flat");
            del_btn.add_css_class("delete-btn");
            del_btn.set_halign(gtk::Align::End);
            del_btn.set_tooltip_text(Some("Delete message"));

            let dcb = dcb.clone();
            let cid = cid.to_string();
            let ts = msg.ts.clone();
            let row_weak = row.downgrade();
            del_btn.connect_clicked(move |_| {
                if let Some(row) = row_weak.upgrade() {
                    dcb(&cid, &ts, &row);
                }
            });

            header.append(&del_btn);
        }
    }

    outer.append(&header);

    // Message body with clickable @mentions
    if !msg.text.is_empty() {
        let markup = format_message_markup(&msg.text, users);
        let body = Label::new(None);
        body.set_markup(&markup);
        body.set_wrap(true);
        body.set_halign(gtk::Align::Start);
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

        outer.append(&body);
    }

    // Collect image URLs to load (with fallback candidates per image)
    let mut image_url_sets: Vec<Vec<String>> = Vec::new();

    // Images from file uploads
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
    for urls in image_url_sets {
        let picture = Picture::new();
        picture.set_halign(gtk::Align::Start);
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_size_request(-1, 200);
        picture.set_can_shrink(true);
        picture.set_cursor_from_name(Some("pointer"));
        outer.append(&picture);

        // Click to open fullscreen viewer
        let stored_texture: Rc<RefCell<Option<gtk4::gdk::Texture>>> = Rc::new(RefCell::new(None));
        {
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let tex = stored_texture.clone();
            // Use weak ref to avoid Picture -> GestureClick -> closure -> Picture cycle
            let picture_weak = picture.downgrade();
            click.connect_released(move |_, _, _, _| {
                let Some(picture) = picture_weak.upgrade() else { return };
                if let Some(texture) = tex.borrow().as_ref() {
                    if let Some(root) = picture.root() {
                        if let Some(win) = root.downcast_ref::<gtk::Window>() {
                            crate::ui::image_viewer::show(texture, win);
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
            let c = client.clone();
            let urls2 = urls.clone();
            let bytes_result = rt.spawn(async move {
                for url in &urls2 {
                    match c.fetch_image_bytes(url).await {
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
                    let gbytes = gtk4::glib::Bytes::from(&bytes);
                    let stream = gtk4::gio::MemoryInputStream::from_bytes(&gbytes);
                    match gtk4::gdk_pixbuf::Pixbuf::from_stream(
                        &stream,
                        gtk4::gio::Cancellable::NONE,
                    ) {
                        Ok(pixbuf) => {
                            let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                            picture.set_paintable(Some(&texture));
                            *stored_texture.borrow_mut() = Some(texture);
                            let w = pixbuf.width();
                            let h = pixbuf.height();
                            if w > 0 && h > 0 {
                                let display_w = 400.min(w);
                                let display_h =
                                    (h as f64 * display_w as f64 / w as f64) as i32;
                                picture.set_size_request(display_w, display_h);
                            }
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

        // Add reaction button (emoji chooser created lazily on first click)
        if let (Some(rcb), Some(cid)) = (reaction_cb.clone(), channel_id) {
            let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
            add_btn.add_css_class("flat");
            add_btn.add_css_class("reaction-add-btn");

            let rcb2 = rcb.clone();
            let cid2 = cid.to_string();
            let ts2 = msg.ts.clone();
            let chooser_cell: Rc<RefCell<Option<gtk::EmojiChooser>>> = Rc::new(RefCell::new(None));
            emoji_chooser_cells.borrow_mut().push(chooser_cell.clone());

            let cell_click = chooser_cell.clone();
            let btn_weak = add_btn.downgrade();
            add_btn.connect_clicked(move |_| {
                let Some(btn) = btn_weak.upgrade() else { return };
                // Destroy any previous chooser to release memory
                if let Some(old) = cell_click.borrow_mut().take() {
                    old.unparent();
                }
                crate::mem::log_mem("EmojiChooser::new() BEFORE");
                let chooser = gtk::EmojiChooser::new();
                crate::mem::log_mem("EmojiChooser::new() AFTER");
                let rcb3 = rcb2.clone();
                let cid3 = cid2.clone();
                let ts3 = ts2.clone();
                let cell_close = cell_click.clone();
                chooser.connect_emoji_picked(move |_, emoji| {
                    let shortcode = emojis::get(emoji)
                        .and_then(|e| e.shortcode())
                        .unwrap_or(emoji)
                        .to_string();
                    let dummy = gtk::Button::new();
                    rcb3(&cid3, &ts3, &shortcode, &dummy);
                });
                // Destroy the chooser when it closes to free memory
                let cell_closed = cell_click.clone();
                chooser.connect_closed(move |_| {
                    crate::mem::log_mem("EmojiChooser CLOSED (before destroy)");
                    if let Some(old) = cell_closed.borrow_mut().take() {
                        old.unparent();
                    }
                    crate::mem::trim_heap();
                    crate::mem::log_mem("EmojiChooser CLOSED (after destroy+trim)");
                });
                chooser.set_parent(&btn);
                chooser.popup();
                crate::mem::log_mem("EmojiChooser POPUP shown");
                *cell_click.borrow_mut() = Some(chooser);
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
                .format("%H:%M")
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
    let emoji_unicode = emojis::get_by_shortcode(&reaction.name)
        .map(|e| e.as_str().to_string())
        .unwrap_or_else(|| format!(":{}: ", reaction.name));
    let label_text = format!("{} {}", emoji_unicode, reaction.count);
    let btn = gtk::Button::with_label(&label_text);
    btn.add_css_class("flat");
    btn.add_css_class("reaction-btn");

    // Left-click toggles the reaction
    if let (Some(rcb), Some(cid)) = (reaction_cb.clone(), channel_id) {
        let name = reaction.name.clone();
        let ts = msg_ts.to_string();
        let cid = cid.to_string();
        btn.connect_clicked(move |btn| {
            rcb(&cid, &ts, &name, btn);
        });
    }

    // Right-click shows who reacted
    let user_names: Vec<String> = reaction
        .users
        .iter()
        .map(|uid| {
            users
                .get(uid)
                .cloned()
                .unwrap_or_else(|| uid.clone())
        })
        .collect();

    if !user_names.is_empty() {
        let popover = gtk::Popover::new();
        let list_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        list_box.set_margin_top(8);
        list_box.set_margin_bottom(8);
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);

        let header = Label::new(Some(&format!("{} :{}: ", emoji_unicode, reaction.name)));
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

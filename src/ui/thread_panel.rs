use gtk4::prelude::*;
use gtk4::{self as gtk, Button, Label, ListBox, ListBoxRow, Picture, ScrolledWindow, TextView};
use slacko::types::Message;
use std::cell::RefCell;
use std::collections::HashMap;

use crate::slack::client::Client;
use crate::slack::helpers::format_message_markup;
use crate::ui::message_view::ReactionCallback;

pub struct ThreadPanel {
    pub widget: gtk::Box,
    pub list_box: ListBox,
    pub close_button: Button,
    pub send_button: Button,
    pub text_view: TextView,
    pub separator: gtk::Separator,
    header_label: Label,
    scrolled: ScrolledWindow,
    reaction_callback: RefCell<Option<ReactionCallback>>,
    delete_callback: RefCell<Option<crate::ui::message_view::DeleteCallback>>,
    self_user_id: RefCell<String>,
    channel_id: RefCell<Option<String>>,
    /// Message ts to scroll to after thread loads (set by notification click).
    pub pending_scroll: RefCell<Option<String>>,
}

impl ThreadPanel {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_visible(false);

        // Header with title and close button
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_margin_top(8);
        header.set_margin_bottom(8);
        header.set_margin_start(12);
        header.set_margin_end(8);
        header.add_css_class("toolbar");

        let header_label = Label::new(Some("Thread"));
        header_label.add_css_class("title-3");
        header_label.set_halign(gtk::Align::Start);
        header_label.set_hexpand(true);
        header.append(&header_label);

        let close_button = Button::from_icon_name("window-close-symbolic");
        close_button.add_css_class("flat");
        close_button.add_css_class("circular");
        header.append(&close_button);

        let header_sep = gtk::Separator::new(gtk::Orientation::Horizontal);

        container.append(&header);
        container.append(&header_sep);

        // Thread messages list
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

        // Reply input
        let input_sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        container.append(&input_sep);

        let input_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        input_box.set_margin_top(8);
        input_box.set_margin_bottom(8);
        input_box.set_margin_start(8);
        input_box.set_margin_end(8);

        let text_view = TextView::new();
        text_view.set_hexpand(true);
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        text_view.set_top_margin(6);
        text_view.set_bottom_margin(6);
        text_view.set_left_margin(6);
        text_view.set_right_margin(6);
        text_view.add_css_class("card");
        text_view.set_height_request(36);

        let frame = gtk::Frame::new(None);
        frame.set_hexpand(true);
        frame.set_child(Some(&text_view));

        let send_button = Button::with_label("Reply");
        send_button.add_css_class("suggested-action");
        send_button.set_valign(gtk::Align::End);

        input_box.append(&frame);
        input_box.append(&send_button);

        container.append(&input_box);

        // Vertical separator placed to the left of the panel in the parent layout
        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        separator.set_visible(false);

        Self {
            widget: container,
            list_box,
            close_button,
            send_button,
            text_view,
            separator,
            header_label,
            scrolled,
            reaction_callback: RefCell::new(None),
            delete_callback: RefCell::new(None),
            self_user_id: RefCell::new(String::new()),
            channel_id: RefCell::new(None),
            pending_scroll: RefCell::new(None),
        }
    }

    pub fn set_reaction_callback(&self, cb: ReactionCallback) {
        *self.reaction_callback.borrow_mut() = Some(cb);
    }

    pub fn set_delete_callback(&self, cb: crate::ui::message_view::DeleteCallback) {
        *self.delete_callback.borrow_mut() = Some(cb);
    }

    pub fn set_self_user_id(&self, uid: &str) {
        *self.self_user_id.borrow_mut() = uid.to_string();
    }

    pub fn set_channel_id(&self, id: &str) {
        *self.channel_id.borrow_mut() = Some(id.to_string());
    }

    pub fn show(&self) {
        self.widget.set_visible(true);
        self.separator.set_visible(true);
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
        self.separator.set_visible(false);
    }

    pub fn clear(&self) {
        // Unparent floating widgets (popovers, emoji choosers) before removing rows
        let mut idx = 0;
        while let Some(row) = self.list_box.row_at_index(idx) {
            crate::ui::message_view::MessageView::unparent_floating_recursive(&row);
            idx += 1;
        }
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }
        self.text_view.buffer().set_text("");
    }

    pub fn set_messages(
        &self,
        messages: &[Message],
        users: &HashMap<String, String>,
        client: &Client,
        rt: &tokio::runtime::Handle,
    ) {
        self.clear();

        let rcb = self.reaction_callback.borrow().clone();
        let dcb = self.delete_callback.borrow().clone();
        let self_uid = self.self_user_id.borrow().clone();
        let cid = self.channel_id.borrow().clone();
        for msg in messages {
            let row = make_thread_message_row(
                msg, users, client, rt, &rcb, &dcb, cid.as_deref(), &self_uid,
            );
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
        let rcb = self.reaction_callback.borrow().clone();
        let dcb = self.delete_callback.borrow().clone();
        let self_uid = self.self_user_id.borrow().clone();
        let cid = self.channel_id.borrow().clone();
        let row = make_thread_message_row(
            msg, users, client, rt, &rcb, &dcb, cid.as_deref(), &self_uid,
        );
        self.list_box.append(&row);
        self.scroll_to_bottom();
    }

    pub fn get_reply_text(&self) -> String {
        let buffer = self.text_view.buffer();
        let (start, end) = buffer.bounds();
        buffer.text(&start, &end, false).to_string()
    }

    pub fn clear_reply(&self) {
        self.text_view.buffer().set_text("");
    }

    fn scroll_to_bottom(&self) {
        let adj = self.scrolled.vadjustment();
        gtk4::glib::idle_add_local_once(move || {
            adj.set_value(adj.upper() - adj.page_size());
        });
    }

    /// Scroll to a specific message by ts and briefly highlight it.
    pub fn scroll_to_message(&self, ts: &str) {
        let list_box = self.list_box.clone();
        let scrolled = self.scrolled.clone();
        let ts = ts.to_string();

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

fn make_thread_message_row(
    msg: &Message,
    users: &HashMap<String, String>,
    client: &Client,
    rt: &tokio::runtime::Handle,
    reaction_cb: &Option<ReactionCallback>,
    delete_cb: &Option<crate::ui::message_view::DeleteCallback>,
    channel_id: Option<&str>,
    self_user_id: &str,
) -> ListBoxRow {
    let row = ListBoxRow::new();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.set_margin_top(6);
    outer.set_margin_bottom(6);
    outer.set_margin_start(8);
    outer.set_margin_end(8);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let user_id = msg.user.as_deref().unwrap_or("unknown");
    let display_name = users
        .get(user_id)
        .cloned()
        .unwrap_or_else(|| user_id.to_string());

    let name_label = Label::new(Some(&display_name));
    name_label.add_css_class("heading");
    name_label.set_halign(gtk::Align::Start);
    header.append(&name_label);

    let time_str = format_timestamp(&msg.ts);
    let time_label = Label::new(Some(&time_str));
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    time_label.set_halign(gtk::Align::Start);
    time_label.set_hexpand(true);
    header.append(&time_label);

    // Delete button for own messages
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
            let row_ref = row.clone();
            del_btn.connect_clicked(move |_| {
                dcb(&cid, &ts, &row_ref);
            });

            header.append(&del_btn);
        }
    }

    outer.append(&header);

    if !msg.text.is_empty() {
        let markup = format_message_markup(&msg.text, users);
        let body = Label::new(None);
        body.set_markup(&markup);
        body.set_wrap(true);
        body.set_halign(gtk::Align::Start);
        body.set_selectable(true);
        body.set_xalign(0.0);
        outer.append(&body);
    }

    // Images from file uploads
    let mut image_url_sets: Vec<Vec<String>> = Vec::new();
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

    for urls in image_url_sets {
        let picture = Picture::new();
        picture.set_halign(gtk::Align::Start);
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_size_request(-1, 150);
        picture.set_can_shrink(true);
        outer.append(&picture);

        let client = client.clone();
        let rt = rt.clone();
        let picture = picture.clone();
        gtk4::glib::spawn_future_local(async move {
            let mut bytes_result = Err("no URLs".to_string());
            for url in &urls {
                let c = client.clone();
                let u = url.clone();
                let res = rt
                    .spawn(async move { c.fetch_image_bytes(&u).await })
                    .await;
                match res {
                    Ok(Ok(bytes)) => {
                        bytes_result = Ok(bytes);
                        break;
                    }
                    _ => {}
                }
            }
            if let Ok(bytes) = bytes_result {
                let gbytes = gtk4::glib::Bytes::from(&bytes);
                let stream = gtk4::gio::MemoryInputStream::from_bytes(&gbytes);
                if let Ok(pixbuf) = gtk4::gdk_pixbuf::Pixbuf::from_stream(
                    &stream,
                    gtk4::gio::Cancellable::NONE,
                ) {
                    let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                    picture.set_paintable(Some(&texture));
                    let w = pixbuf.width();
                    let h = pixbuf.height();
                    if w > 0 && h > 0 {
                        let display_w = 300.min(w);
                        let display_h = (h as f64 * display_w as f64 / w as f64) as i32;
                        picture.set_size_request(display_w, display_h);
                    }
                } else {
                    picture.set_visible(false);
                }
            } else {
                picture.set_visible(false);
            }
        });
    }

    // ── Reactions ──
    {
        let reactions_box = gtk::FlowBox::new();
        reactions_box.set_selection_mode(gtk::SelectionMode::None);
        reactions_box.set_halign(gtk::Align::Start);
        reactions_box.set_max_children_per_line(20);
        reactions_box.set_homogeneous(false);
        reactions_box.set_row_spacing(2);
        reactions_box.set_column_spacing(4);

        if let Some(reactions) = &msg.reactions {
            for reaction in reactions {
                let btn = crate::ui::message_view::make_reaction_button(
                    reaction, users, &msg.ts,
                    reaction_cb, channel_id,
                );
                reactions_box.insert(&btn, -1);
            }
        }

        if let (Some(rcb), Some(cid)) = (reaction_cb.clone(), channel_id) {
            let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
            add_btn.add_css_class("flat");
            add_btn.add_css_class("reaction-add-btn");

            let rcb2 = rcb.clone();
            let cid2 = cid.to_string();
            let ts2 = msg.ts.clone();
            let chooser_cell: std::rc::Rc<RefCell<Option<gtk::EmojiChooser>>> =
                std::rc::Rc::new(RefCell::new(None));

            let cell_click = chooser_cell.clone();
            let btn_weak = add_btn.downgrade();
            add_btn.connect_clicked(move |_| {
                let Some(btn) = btn_weak.upgrade() else { return };
                let mut cell = cell_click.borrow_mut();
                if cell.is_none() {
                    let chooser = gtk::EmojiChooser::new();
                    let rcb3 = rcb2.clone();
                    let cid3 = cid2.clone();
                    let ts3 = ts2.clone();
                    chooser.connect_emoji_picked(move |_, emoji| {
                        let shortcode = emojis::get(emoji)
                            .and_then(|e| e.shortcode())
                            .unwrap_or(emoji)
                            .to_string();
                        let dummy = gtk::Button::new();
                        rcb3(&cid3, &ts3, &shortcode, &dummy);
                    });
                    chooser.set_parent(&btn);
                    *cell = Some(chooser);
                }
                if let Some(chooser) = cell.as_ref() {
                    chooser.popup();
                }
            });

            reactions_box.insert(&add_btn, -1);
        }

        outer.append(&reactions_box);
    }

    row.set_child(Some(&outer));
    row.set_widget_name(&msg.ts);
    row
}

fn format_timestamp(ts: &str) -> String {
    let epoch_str = ts.split('.').next().unwrap_or(ts);
    if let Ok(epoch) = epoch_str.parse::<i64>() {
        if let Some(dt) = chrono::DateTime::from_timestamp(epoch, 0) {
            return dt
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
        }
    }
    ts.to_string()
}

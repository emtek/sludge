use gtk4::prelude::*;
use gtk4::{self as gtk, Button, Label, ListBox, ScrolledWindow, TextView};
use slacko::types::Message;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::slack::client::Client;
use crate::ui::autocomplete::Autocomplete;
use crate::ui::message_input::{attach_image_paste, rebuild_file_preview};
use crate::ui::message_view::ReactionCallback;
use crate::ui::send_button::SendButton;

pub struct ThreadPanel {
    pub widget: gtk::Box,
    pub list_box: ListBox,
    pub close_button: Button,
    pub send_button: SendButton,
    pub text_view: TextView,
    pub separator: gtk::Separator,
    _header_label: Label,
    scrolled: ScrolledWindow,
    mention_callback: RefCell<Option<crate::ui::message_view::MentionCallback>>,
    reaction_callback: RefCell<Option<ReactionCallback>>,
    delete_callback: RefCell<Option<crate::ui::message_view::DeleteCallback>>,
    edit_callback: RefCell<Option<crate::ui::message_view::EditCallback>>,
    self_user_id: RefCell<String>,
    channel_id: RefCell<Option<String>>,
    /// Message ts to scroll to after thread loads (set by notification click).
    pub pending_scroll: RefCell<Option<String>>,
    picker_cells: Rc<RefCell<Vec<Rc<RefCell<Option<gtk::Popover>>>>>>,
    autocomplete: Autocomplete,
    files: Rc<RefCell<Vec<PathBuf>>>,
    file_preview_box: gtk::Box,
    reaction_boxes: Rc<RefCell<HashMap<String, gtk::FlowBox>>>,
    image_generation: Rc<Cell<u64>>,
    thread_labels: Rc<RefCell<HashMap<String, Label>>>,
    thread_counts: Rc<RefCell<HashMap<String, usize>>>,
    subteam_names: RefCell<Rc<HashMap<String, String>>>,
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
        list_box.set_selection_mode(gtk::SelectionMode::Single);
        list_box.set_focusable(true);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(8);
        list_box.set_margin_end(8);

        let scrolled = ScrolledWindow::new();
        scrolled.set_vexpand(true);
        scrolled.set_hexpand(true);
        scrolled.set_focusable(true);
        scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        scrolled.set_child(Some(&list_box));

        container.append(&scrolled);

        // Reply input
        let input_sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        container.append(&input_sep);

        // File preview area (hidden when empty)
        let file_preview_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        file_preview_box.set_margin_start(8);
        file_preview_box.set_margin_end(8);
        file_preview_box.set_margin_top(4);
        file_preview_box.set_visible(false);
        container.append(&file_preview_box);

        let input_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        input_box.set_margin_top(8);
        input_box.set_margin_bottom(8);
        input_box.set_margin_start(8);
        input_box.set_margin_end(8);

        let attach_button = Button::from_icon_name("mail-attachment-symbolic");
        attach_button.add_css_class("flat");
        attach_button.set_valign(gtk::Align::End);
        attach_button.set_tooltip_text(Some("Attach file"));

        let text_view = TextView::new();
        text_view.set_hexpand(true);
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        text_view.set_top_margin(6);
        text_view.set_bottom_margin(6);
        text_view.set_left_margin(6);
        text_view.set_right_margin(6);
        text_view.add_css_class("card");
        text_view.set_height_request(36);
        text_view.set_extra_menu(None::<&gtk::gio::MenuModel>);

        let frame = gtk::Frame::new(None);
        frame.set_hexpand(true);
        frame.set_child(Some(&text_view));

        let send_button = SendButton::new();

        let files: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));

        input_box.append(&attach_button);
        input_box.append(&frame);
        input_box.append(&send_button.widget);

        container.append(&input_box);

        // Paste images from clipboard
        attach_image_paste(&text_view, &files, &file_preview_box);

        // Wire up attach button to open file chooser
        {
            let files = files.clone();
            let preview = file_preview_box.clone();
            let widget_weak = container.downgrade();
            attach_button.connect_clicked(move |_| {
                let Some(widget) = widget_weak.upgrade() else { return };
                let files = files.clone();
                let preview = preview.clone();
                let dialog = gtk::FileDialog::new();
                dialog.set_title("Attach files");
                if let Some(root) = widget.root() {
                    if let Some(win) = root.downcast_ref::<gtk::Window>() {
                        let files2 = files.clone();
                        let preview2 = preview.clone();
                        dialog.open_multiple(Some(win), gtk::gio::Cancellable::NONE, move |result| {
                            if let Ok(file_list) = result {
                                for i in 0..file_list.n_items() {
                                    if let Some(obj) = file_list.item(i) {
                                        if let Ok(file) = obj.downcast::<gtk::gio::File>() {
                                            if let Some(path) = file.path() {
                                                files2.borrow_mut().push(path);
                                            }
                                        }
                                    }
                                }
                                rebuild_file_preview(&preview2, &files2);
                            }
                        });
                    }
                }
            });
        }

        // Attach shared autocomplete (handles both @mentions and :emoji:)
        let autocomplete = Autocomplete::attach(&text_view);

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
            _header_label: header_label,
            scrolled,
            mention_callback: RefCell::new(None),
            reaction_callback: RefCell::new(None),
            delete_callback: RefCell::new(None),
            edit_callback: RefCell::new(None),
            self_user_id: RefCell::new(String::new()),
            channel_id: RefCell::new(None),
            pending_scroll: RefCell::new(None),
            picker_cells: Rc::new(RefCell::new(Vec::new())),
            autocomplete,
            files,
            file_preview_box,
            reaction_boxes: Rc::new(RefCell::new(HashMap::new())),
            image_generation: Rc::new(Cell::new(0)),
            thread_labels: Rc::new(RefCell::new(HashMap::new())),
            thread_counts: Rc::new(RefCell::new(HashMap::new())),
            subteam_names: RefCell::new(Rc::new(HashMap::new())),
        }
    }

    /// Set the user list for @mention autocomplete in the thread reply input.
    pub fn set_mention_users(&self, users: &HashMap<String, String>) {
        self.autocomplete.set_users(users);
    }

    /// Set a callback invoked when an emoji is picked via autocomplete.
    pub fn set_on_emoji_picked(&self, f: Rc<dyn Fn(&str)>) {
        self.autocomplete.set_on_emoji_picked(f);
    }

    pub fn set_mention_callback(&self, cb: crate::ui::message_view::MentionCallback) {
        *self.mention_callback.borrow_mut() = Some(cb);
    }

    pub fn set_reaction_callback(&self, cb: ReactionCallback) {
        *self.reaction_callback.borrow_mut() = Some(cb);
    }

    pub fn set_delete_callback(&self, cb: crate::ui::message_view::DeleteCallback) {
        *self.delete_callback.borrow_mut() = Some(cb);
    }

    pub fn set_edit_callback(&self, cb: crate::ui::message_view::EditCallback) {
        *self.edit_callback.borrow_mut() = Some(cb);
    }

    pub fn set_self_user_id(&self, uid: &str) {
        *self.self_user_id.borrow_mut() = uid.to_string();
    }

    pub fn set_channel_id(&self, id: &str) {
        *self.channel_id.borrow_mut() = Some(id.to_string());
    }

    pub fn set_subteam_names(&self, names: Rc<HashMap<String, String>>) {
        *self.subteam_names.borrow_mut() = names;
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
        // Explicitly unparent tracked emoji choosers
        for cell in self.picker_cells.borrow_mut().drain(..) {
            if let Some(chooser) = cell.borrow_mut().take() {
                chooser.unparent();
            }
        }
        // Unparent remaining floating widgets (reaction popovers)
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

        let mcb = self.mention_callback.borrow();
        let rcb = self.reaction_callback.borrow();
        let dcb = self.delete_callback.borrow();
        let ecb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let cid = self.channel_id.borrow();
        let subteam_names = self.subteam_names.borrow();
        self.reaction_boxes.borrow_mut().clear();
        let no_expand: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, Vec<gtk::ListBoxRow>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
        for msg in messages {
            let row = crate::ui::message_view::make_message_row(
                msg, users, &subteam_names, client, rt,
                &None, &mcb, &rcb, &dcb, &ecb,
                cid.as_deref(), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
                &self.image_generation, &self.picker_cells,
                &None, &no_expand,
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
        let mcb = self.mention_callback.borrow();
        let rcb = self.reaction_callback.borrow();
        let dcb = self.delete_callback.borrow();
        let ecb = self.edit_callback.borrow();
        let self_uid = self.self_user_id.borrow();
        let cid = self.channel_id.borrow();
        let subteam_names = self.subteam_names.borrow();
        let no_expand: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, Vec<gtk::ListBoxRow>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
        let row = crate::ui::message_view::make_message_row(
            msg, users, &subteam_names, client, rt,
            &None, &mcb, &rcb, &dcb, &ecb,
            cid.as_deref(), &self.thread_counts.borrow(), &self.thread_labels, &self.reaction_boxes, &self_uid,
            &self.image_generation, &self.picker_cells,
            &None, &no_expand,
        );
        self.list_box.append(&row);
        self.scroll_to_bottom();
    }

    pub fn update_reactions(
        &self,
        ts: &str,
        reactions: &[slacko::types::Reaction],
        users: &HashMap<String, String>,
    ) {
        let boxes = self.reaction_boxes.borrow();
        let Some(flow_box) = boxes.get(ts) else { return };

        // Remove all children except the last one (the add-reaction button)
        while flow_box.child_at_index(0).is_some() {
            let count = {
                let mut n = 0;
                while flow_box.child_at_index(n).is_some() { n += 1; }
                n
            };
            if count <= 1 { break; }
            if let Some(child) = flow_box.child_at_index(0) {
                crate::ui::message_view::MessageView::unparent_floating_recursive(&child);
                flow_box.remove(&child);
            }
        }

        let reaction_cb = self.reaction_callback.borrow().clone();
        let channel_id = self.channel_id.borrow().clone();

        for reaction in reactions {
            let btn = crate::ui::message_view::make_reaction_button(
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

    pub fn get_reply_text(&self) -> String {
        let buffer = self.text_view.buffer();
        let (start, end) = buffer.bounds();
        buffer.text(&start, &end, false).to_string()
    }

    pub fn clear_reply(&self) {
        self.text_view.buffer().set_text("");
        self.files.borrow_mut().clear();
        rebuild_file_preview(&self.file_preview_box, &self.files);
    }

    pub fn take_files(&self) -> Vec<PathBuf> {
        let files = self.files.borrow().clone();
        self.files.borrow_mut().clear();
        rebuild_file_preview(&self.file_preview_box, &self.files);
        files
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

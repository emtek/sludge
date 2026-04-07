use gtk4::prelude::*;
use gtk4::{self as gtk, Button, TextView};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::ui::autocomplete::Autocomplete;
use crate::ui::send_button::SendButton;

/// Attach a paste controller to a `TextView` that intercepts image content from
/// the clipboard and saves it to a temp file, adding the path to `files`.
/// Calls `rebuild` afterwards to refresh any preview UI.
pub fn attach_image_paste(
    text_view: &TextView,
    files: &Rc<RefCell<Vec<PathBuf>>>,
    preview_box: &gtk::Box,
) {
    let files = files.clone();
    let preview = preview_box.clone();

    let paste_controller = gtk::EventControllerKey::new();
    let tv = text_view.downgrade();
    paste_controller.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk4::gdk::Key::v
            && modifiers.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
        {
            let Some(text_view) = tv.upgrade() else {
                return gtk4::glib::Propagation::Proceed;
            };
            let clipboard = text_view.clipboard();
            let files = files.clone();
            let preview = preview.clone();
            clipboard.read_texture_async(gtk::gio::Cancellable::NONE, move |result| {
                if let Ok(Some(texture)) = result {
                    // Save the texture as a PNG temp file
                    let dir = std::env::temp_dir().join("sludge-paste");
                    let _ = std::fs::create_dir_all(&dir);
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis();
                    let path = dir.join(format!("paste-{ts}.png"));
                    if texture.save_to_png(&path).is_ok() {
                        files.borrow_mut().push(path);
                        rebuild_file_preview(&preview, &files);
                    }
                }
            });
        }
        // Always proceed so text paste still works for non-image content
        gtk4::glib::Propagation::Proceed
    });
    text_view.add_controller(paste_controller);
}

pub struct MessageInput {
    pub widget: gtk::Box,
    pub text_view: TextView,
    pub send_button: SendButton,
    pub attach_button: Button,
    files: Rc<RefCell<Vec<PathBuf>>>,
    file_preview_box: gtk::Box,
    autocomplete: Autocomplete,
}

impl MessageInput {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_margin_top(8);
        container.set_margin_bottom(8);
        container.set_margin_start(8);
        container.set_margin_end(8);

        // File preview area (hidden when empty)
        let file_preview_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        file_preview_box.set_margin_bottom(4);
        file_preview_box.set_visible(false);
        container.append(&file_preview_box);

        // Input row
        let input_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let attach_button = Button::from_icon_name("mail-attachment-symbolic");
        attach_button.add_css_class("flat");
        attach_button.set_valign(gtk::Align::Center);
        attach_button.set_tooltip_text(Some("Attach file"));

        let text_view = TextView::new();
        text_view.set_hexpand(true);
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        text_view.set_top_margin(8);
        text_view.set_bottom_margin(8);
        text_view.set_left_margin(8);
        text_view.set_right_margin(8);
        text_view.add_css_class("card");

        // Limit height
        text_view.set_height_request(40);

        let frame = gtk::Frame::new(None);
        frame.set_hexpand(true);
        frame.set_child(Some(&text_view));

        let send_button = SendButton::new();

        input_row.append(&attach_button);
        input_row.append(&frame);
        input_row.append(&send_button.widget);

        container.append(&input_row);

        let files: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));

        // Attach shared autocomplete (handles both @mentions and :emoji:)
        let autocomplete = Autocomplete::attach(&text_view);

        // Paste images from clipboard
        attach_image_paste(&text_view, &files, &file_preview_box);

        // Wire up attach button to open file chooser
        let files_click = files.clone();
        let preview_click = file_preview_box.clone();
        let widget_weak = container.downgrade();
        attach_button.connect_clicked(move |_| {
            let Some(widget) = widget_weak.upgrade() else { return };
            let files = files_click.clone();
            let preview = preview_click.clone();

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

        Self {
            widget: container,
            text_view,
            send_button,
            attach_button,
            files,
            file_preview_box,
            autocomplete,
        }
    }

    /// Set the user list for @mention autocomplete.
    pub fn set_mention_users(&self, users: &HashMap<String, String>) {
        self.autocomplete.set_users(users);
    }

    /// Set a callback invoked when an emoji is picked via autocomplete.
    pub fn set_on_emoji_picked(&self, f: Rc<dyn Fn(&str)>) {
        self.autocomplete.set_on_emoji_picked(f);
    }

    pub fn get_text(&self) -> String {
        let buffer = self.text_view.buffer();
        let (start, end) = buffer.bounds();
        buffer.text(&start, &end, false).to_string()
    }

    pub fn clear(&self) {
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

    pub fn has_files(&self) -> bool {
        !self.files.borrow().is_empty()
    }
}

pub fn rebuild_file_preview(preview_box: &gtk::Box, files: &Rc<RefCell<Vec<PathBuf>>>) {
    // Clear existing children
    while let Some(child) = preview_box.first_child() {
        preview_box.remove(&child);
    }

    let file_list = files.borrow();
    preview_box.set_visible(!file_list.is_empty());

    for (idx, path) in file_list.iter().enumerate() {
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chip.add_css_class("card");
        chip.set_margin_top(2);
        chip.set_margin_bottom(2);
        chip.set_margin_start(2);
        chip.set_margin_end(2);

        let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
        icon.set_margin_start(6);
        chip.append(&icon);

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        let label = gtk::Label::new(Some(&name));
        label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        label.set_max_width_chars(20);
        chip.append(&label);

        let remove_btn = Button::from_icon_name("window-close-symbolic");
        remove_btn.add_css_class("flat");
        remove_btn.add_css_class("circular");
        remove_btn.set_margin_start(2);
        remove_btn.set_margin_end(2);

        let files_rm = files.clone();
        let preview_rm = preview_box.clone();
        remove_btn.connect_clicked(move |_| {
            files_rm.borrow_mut().remove(idx);
            rebuild_file_preview(&preview_rm, &files_rm);
        });
        chip.append(&remove_btn);

        preview_box.append(&chip);
    }
}

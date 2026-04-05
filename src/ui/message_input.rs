use gtk4::prelude::*;
use gtk4::{self as gtk, Button, TextView};

pub struct MessageInput {
    pub widget: gtk::Box,
    pub text_view: TextView,
    pub send_button: Button,
}

impl MessageInput {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        container.set_margin_top(8);
        container.set_margin_bottom(8);
        container.set_margin_start(8);
        container.set_margin_end(8);

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

        let send_button = Button::with_label("Send");
        send_button.add_css_class("suggested-action");
        send_button.set_valign(gtk::Align::End);

        container.append(&frame);
        container.append(&send_button);

        Self {
            widget: container,
            text_view,
            send_button,
        }
    }

    pub fn get_text(&self) -> String {
        let buffer = self.text_view.buffer();
        let (start, end) = buffer.bounds();
        buffer.text(&start, &end, false).to_string()
    }

    pub fn clear(&self) {
        self.text_view.buffer().set_text("");
    }
}

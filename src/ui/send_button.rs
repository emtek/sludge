use gtk4::prelude::*;
use gtk4::{self as gtk, Button};

/// A simple send button with a play icon.
pub struct SendButton {
    pub widget: gtk::Box,
    /// The send button — connect to this for immediate send.
    pub send: Button,
}

impl SendButton {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        container.set_valign(gtk::Align::Center);

        let send = Button::from_icon_name("media-playback-start-symbolic");
        send.add_css_class("suggested-action");
        container.append(&send);

        Self {
            widget: container,
            send,
        }
    }
}

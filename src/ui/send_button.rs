use chrono::Datelike;
use gtk4::prelude::*;
use gtk4::{self as gtk, Button};
use std::cell::RefCell;
use std::rc::Rc;

/// A callback invoked with an optional Unix timestamp (None = send now).
pub type ScheduleCallback = Rc<dyn Fn(Option<i64>)>;

/// A split button: [▶ | ▼] where the left side sends immediately
/// and the right side opens a schedule popover.
pub struct SendButton {
    pub widget: gtk::Box,
    /// The main send button (left side) — connect to this for immediate send.
    pub send: Button,
    schedule_callback: Rc<RefCell<Option<ScheduleCallback>>>,
}

impl SendButton {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        container.add_css_class("linked");
        container.set_valign(gtk::Align::End);

        // Main send button with icon
        let send = Button::from_icon_name("media-playback-start-symbolic");
        send.add_css_class("suggested-action");
        container.append(&send);

        // Dropdown arrow button
        let dropdown = Button::from_icon_name("pan-down-symbolic");
        dropdown.add_css_class("suggested-action");
        container.append(&dropdown);

        let schedule_callback: Rc<RefCell<Option<ScheduleCallback>>> =
            Rc::new(RefCell::new(None));

        // Schedule popover
        let popover = gtk::Popover::new();
        popover.set_parent(&dropdown);
        popover.set_autohide(true);
        popover.set_has_arrow(true);
        popover.set_position(gtk::PositionType::Top);

        let popover_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        popover_box.set_margin_top(8);
        popover_box.set_margin_bottom(8);
        popover_box.set_margin_start(4);
        popover_box.set_margin_end(4);
        popover_box.set_width_request(220);

        let header = gtk::Label::new(Some("Schedule message"));
        header.add_css_class("dim-label");
        header.set_halign(gtk::Align::Start);
        header.set_margin_start(12);
        header.set_margin_bottom(4);
        popover_box.append(&header);

        // "Tomorrow at 9:00 AM"
        let tomorrow_btn = gtk::Button::new();
        tomorrow_btn.add_css_class("flat");
        let tomorrow_label = gtk::Label::new(None);
        tomorrow_label.set_halign(gtk::Align::Start);
        tomorrow_label.set_margin_start(8);
        tomorrow_btn.set_child(Some(&tomorrow_label));
        popover_box.append(&tomorrow_btn);

        // "Monday at 9:00 AM"
        let monday_btn = gtk::Button::new();
        monday_btn.add_css_class("flat");
        let monday_label = gtk::Label::new(None);
        monday_label.set_halign(gtk::Align::Start);
        monday_label.set_margin_start(8);
        monday_btn.set_child(Some(&monday_label));
        popover_box.append(&monday_btn);

        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep.set_margin_top(4);
        sep.set_margin_bottom(4);
        popover_box.append(&sep);

        // "Custom time" — opens a date/time picker
        let custom_btn = gtk::Button::with_label("Custom time");
        custom_btn.add_css_class("flat");
        custom_btn.child().unwrap().set_halign(gtk::Align::Start);
        popover_box.append(&custom_btn);

        // Custom time picker (hidden by default)
        let picker_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        picker_box.set_visible(false);
        picker_box.set_margin_top(8);
        picker_box.set_margin_start(8);
        picker_box.set_margin_end(8);

        let date_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let date_label = gtk::Label::new(Some("Date:"));
        let date_entry = gtk::Entry::new();
        date_entry.set_placeholder_text(Some("YYYY-MM-DD"));
        date_entry.set_hexpand(true);
        date_row.append(&date_label);
        date_row.append(&date_entry);
        picker_box.append(&date_row);

        let time_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let time_label = gtk::Label::new(Some("Time:"));
        let time_entry = gtk::Entry::new();
        time_entry.set_placeholder_text(Some("HH:MM"));
        time_entry.set_hexpand(true);
        time_row.append(&time_label);
        time_row.append(&time_entry);
        picker_box.append(&time_row);

        let schedule_confirm = gtk::Button::with_label("Schedule");
        schedule_confirm.add_css_class("suggested-action");
        picker_box.append(&schedule_confirm);

        popover_box.append(&picker_box);
        popover.set_child(Some(&popover_box));

        // Update labels and wire callbacks when popover opens
        {
            let tomorrow_label = tomorrow_label.clone();
            let monday_label = monday_label.clone();
            let date_entry = date_entry.clone();
            let time_entry = time_entry.clone();
            let picker_box = picker_box.clone();
            popover.connect_show(move |_| {
                picker_box.set_visible(false);

                let now = chrono::Local::now();

                // Tomorrow at 9:00 AM
                let tomorrow = (now + chrono::Duration::days(1))
                    .date_naive()
                    .and_hms_opt(9, 0, 0)
                    .unwrap();
                tomorrow_label.set_text(&format!(
                    "Tomorrow at {}",
                    tomorrow.format("%-I:%M %p")
                ));

                // Next Monday at 9:00 AM
                let days_until_monday = (8 - now.weekday().num_days_from_monday()) % 7;
                let days_until_monday = if days_until_monday == 0 { 7 } else { days_until_monday };
                let monday = (now + chrono::Duration::days(days_until_monday as i64))
                    .date_naive()
                    .and_hms_opt(9, 0, 0)
                    .unwrap();
                monday_label.set_text(&format!(
                    "{} at {}",
                    monday.format("%A"),
                    monday.format("%-I:%M %p")
                ));

                // Pre-fill custom date/time with tomorrow
                date_entry.set_text(&tomorrow.format("%Y-%m-%d").to_string());
                time_entry.set_text("09:00");
            });
        }

        // Tomorrow button
        {
            let cb = schedule_callback.clone();
            let popover = popover.clone();
            tomorrow_btn.connect_clicked(move |_| {
                let now = chrono::Local::now();
                let tomorrow = (now + chrono::Duration::days(1))
                    .date_naive()
                    .and_hms_opt(9, 0, 0)
                    .unwrap();
                let ts = tomorrow
                    .and_local_timezone(chrono::Local)
                    .unwrap()
                    .timestamp();
                if let Some(f) = cb.borrow().as_ref() {
                    f(Some(ts));
                }
                popover.popdown();
            });
        }

        // Monday button
        {
            let cb = schedule_callback.clone();
            let popover = popover.clone();
            monday_btn.connect_clicked(move |_| {
                let now = chrono::Local::now();
                let days_until_monday = (8 - now.weekday().num_days_from_monday()) % 7;
                let days_until_monday = if days_until_monday == 0 { 7 } else { days_until_monday };
                let monday = (now + chrono::Duration::days(days_until_monday as i64))
                    .date_naive()
                    .and_hms_opt(9, 0, 0)
                    .unwrap();
                let ts = monday
                    .and_local_timezone(chrono::Local)
                    .unwrap()
                    .timestamp();
                if let Some(f) = cb.borrow().as_ref() {
                    f(Some(ts));
                }
                popover.popdown();
            });
        }

        // Custom time toggle
        {
            let picker_box = picker_box.clone();
            custom_btn.connect_clicked(move |_| {
                picker_box.set_visible(!picker_box.is_visible());
            });
        }

        // Custom time confirm
        {
            let cb = schedule_callback.clone();
            let popover = popover.clone();
            schedule_confirm.connect_clicked(move |_| {
                let date_str = date_entry.text().to_string();
                let time_str = time_entry.text().to_string();
                let datetime_str = format!("{date_str} {time_str}");
                if let Ok(naive) =
                    chrono::NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M")
                {
                    let ts = naive
                        .and_local_timezone(chrono::Local)
                        .unwrap()
                        .timestamp();
                    if let Some(f) = cb.borrow().as_ref() {
                        f(Some(ts));
                    }
                    popover.popdown();
                }
            });
        }

        // Dropdown opens the popover
        {
            let popover = popover.clone();
            dropdown.connect_clicked(move |_| {
                popover.popup();
            });
        }

        // Dismiss popover when the widget is unmapped (parent hidden)
        {
            let popover = popover.clone();
            container.connect_unmap(move |_| {
                popover.popdown();
            });
        }

        Self {
            widget: container,
            send,
            schedule_callback,
        }
    }

    /// Set the callback for scheduled sends. Called with `Some(unix_ts)` for scheduled,
    /// or `None` for immediate (though immediate send uses the `send` button directly).
    pub fn set_schedule_callback(&self, cb: ScheduleCallback) {
        *self.schedule_callback.borrow_mut() = Some(cb);
    }
}

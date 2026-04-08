use gtk4::prelude::*;
use gtk4::{self as gtk, Label, ListBox, ListBoxRow, TextView};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::slack::helpers::{get_custom_emoji_path, get_recent_emoji, resolve_slack_shortcode};

/// Global callback for persisting emoji usage to DB. Set once at startup.
static EMOJI_PERSIST_CB: std::sync::OnceLock<Box<dyn Fn(&str) + Send + Sync>> =
    std::sync::OnceLock::new();

/// Set the global callback that persists emoji usage to the database.
pub fn set_emoji_persist_callback(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = EMOJI_PERSIST_CB.set(Box::new(f));
}

/// Record an emoji as recently used (updates global state + persists via callback).
pub fn record_emoji_used(shortcode: &str) {
    crate::slack::helpers::push_recent_emoji(shortcode);
    if let Some(f) = EMOJI_PERSIST_CB.get() {
        f(shortcode);
    }
}

/// Abstraction over how the autocomplete list is displayed.
/// `Popover` uses a gtk::Popover (creates its own Wayland popup surface).
/// `Inline` embeds the list as a regular widget — safe inside another popover.
#[derive(Clone)]
enum ListHost {
    Popover(gtk::Popover),
    Inline(gtk::Frame),
}

impl ListHost {
    fn show(&self) {
        match self {
            Self::Popover(p) => p.popup(),
            Self::Inline(f) => f.set_visible(true),
        }
    }

    fn hide(&self) {
        match self {
            Self::Popover(p) => p.popdown(),
            Self::Inline(f) => f.set_visible(false),
        }
    }

    fn is_visible(&self) -> bool {
        match self {
            Self::Popover(p) => p.is_visible(),
            Self::Inline(f) => f.is_visible(),
        }
    }
}

/// Shared autocomplete state attached to a `TextView`.
/// Supports `@mention` (user names) and `:emoji:` (shortcode) completion.
pub struct Autocomplete {
    users: Rc<RefCell<HashMap<String, String>>>,
    _host: ListHost,
    _list: ListBox,
    on_emoji_picked: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Mention,
    Emoji,
}

/// Search emoji by query, returning (shortcode, display_text, image_path_for_custom).
/// Results are sorted: recently used first, then by search relevance.
/// When query is empty, returns the most recently used emoji.
fn search_emoji(query: &str) -> Vec<(String, String, Option<String>)> {
    let recent = get_recent_emoji();
    let mut seen = std::collections::HashSet::new();
    let mut m: Vec<(String, String, Option<String>)> = Vec::new();
    let is_empty = query.is_empty();

    // First pass: recently used emoji that match the query
    for sc in &recent {
        if m.len() >= 8 {
            break;
        }
        if !is_empty && !sc.to_lowercase().contains(query) {
            continue;
        }
        if !seen.insert(sc.clone()) {
            continue;
        }
        if let Some(glyph) = resolve_slack_shortcode(sc) {
            m.push((sc.clone(), format!("{glyph} :{sc}:"), None));
        } else if let Some(path) = get_custom_emoji_path(sc) {
            m.push((sc.clone(), format!(":{sc}:"), Some(path)));
        }
    }

    // If query is empty, fill remaining slots with common defaults
    if is_empty {
        const DEFAULTS: &[(&str, &str)] = &[
            ("thumbsup", "👍"),
            ("heart", "❤️"),
            ("laughing", "😆"),
            ("tada", "🎉"),
            ("eyes", "👀"),
            ("white_check_mark", "✅"),
            ("raised_hands", "🙌"),
            ("+1", "👍"),
        ];
        for &(sc, glyph) in DEFAULTS {
            if m.len() >= 8 {
                break;
            }
            if seen.insert(sc.to_string()) {
                m.push((sc.to_string(), format!("{glyph} :{sc}:"), None));
            }
        }
        return m;
    }

    // Second pass: standard emoji search
    for e in emoji::search::search_name(query) {
        if m.len() >= 8 {
            break;
        }
        if e.status != emoji::Status::FullyQualified || e.is_variant {
            continue;
        }
        let sc = emojis::get(e.glyph)
            .and_then(|em| em.shortcode())
            .unwrap_or(e.name)
            .to_string();
        if !sc.to_lowercase().contains(query) {
            continue;
        }
        if !seen.insert(sc.clone()) {
            continue;
        }
        m.push((sc.clone(), format!("{} :{sc}:", e.glyph), None));
    }

    // Third pass: custom emoji
    if let Some(custom) = crate::slack::helpers::get_all_custom_emoji_names() {
        for name in custom {
            if m.len() >= 8 {
                break;
            }
            if !name.to_lowercase().contains(query) {
                continue;
            }
            if !seen.insert(name.clone()) {
                continue;
            }
            let image_path = get_custom_emoji_path(&name);
            m.push((name.clone(), format!(":{name}:"), image_path));
        }
    }

    m.truncate(8);
    m
}

/// Populate a ListBox with emoji/mention match rows.
fn populate_list(list: &ListBox, matches: &[(String, String, Option<String>)]) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for (id, display, emoji_image_path) in matches {
        let row = ListBoxRow::new();
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row_box.set_margin_start(8);
        row_box.set_margin_end(8);
        row_box.set_margin_top(4);
        row_box.set_margin_bottom(4);

        if let Some(path) = emoji_image_path {
            if let Ok(pixbuf) = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(
                path, 20, 20, true,
            ) {
                let texture = gtk4::gdk::Texture::for_pixbuf(&pixbuf);
                let image = gtk::Image::from_paintable(Some(&texture));
                image.set_pixel_size(20);
                row_box.append(&image);
            }
        }

        let label = Label::new(Some(display));
        label.set_halign(gtk::Align::Start);
        row_box.append(&label);

        row.set_child(Some(&row_box));
        row.set_widget_name(id);
        list.append(&row);
    }
}

impl Autocomplete {
    /// Attach autocomplete to the given `TextView`.  Returns a handle to update
    /// the user list later via `set_users`.
    pub fn attach(text_view: &TextView) -> Self {
        let popover = gtk::Popover::new();
        let list = ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("boxed-list");
        list.set_width_request(200);
        popover.set_child(Some(&list));
        popover.set_autohide(false);
        popover.set_has_arrow(false);
        popover.set_parent(text_view);
        popover.set_position(gtk::PositionType::Top);
        let host = ListHost::Popover(popover);
        Self::attach_with_host(text_view, host, list)
    }

    /// Attach autocomplete using an inline widget instead of a Popover.
    /// Returns `(Autocomplete, gtk::Frame)` — the caller must place the frame
    /// in the widget tree (e.g. above the text entry inside an existing popover).
    /// This avoids nested Wayland popup surfaces.
    pub fn attach_inline(text_view: &TextView) -> (Self, gtk::Frame) {
        let list = ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("boxed-list");
        list.set_width_request(200);
        let frame = gtk::Frame::new(None);
        frame.set_child(Some(&list));
        frame.set_visible(false);
        let host = ListHost::Inline(frame.clone());
        (Self::attach_with_host(text_view, host, list), frame)
    }

    fn attach_with_host(text_view: &TextView, host: ListHost, list: ListBox) -> Self {
        let users: Rc<RefCell<HashMap<String, String>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let on_emoji_picked: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>> =
            Rc::new(RefCell::new(None));

        let mode: Rc<RefCell<Mode>> = Rc::new(RefCell::new(Mode::Mention));

        // Monitor text changes
        {
            let users = users.clone();
            let host = host.clone();
            let list = list.clone();
            let tv = text_view.clone();
            let mode = mode.clone();
            text_view.buffer().connect_changed(move |buffer| {
                let insert = buffer.iter_at_mark(&buffer.get_insert());

                // Walk backwards to find trigger (@ or :)
                let mut start = insert;
                let mut trigger = None;
                while start.backward_char() {
                    let ch = start.char();
                    if ch == '@' {
                        trigger = Some(Mode::Mention);
                        break;
                    }
                    if ch == ':' {
                        trigger = Some(Mode::Emoji);
                        break;
                    }
                    if ch == ' ' || ch == '\n' || ch == '<' || ch == '>' {
                        break;
                    }
                }

                let Some(current_mode) = trigger else {
                    host.hide();
                    return;
                };

                // Extract query after the trigger character
                let mut query_start = start;
                query_start.forward_char();
                let query = buffer
                    .text(&query_start, &insert, false)
                    .to_string()
                    .to_lowercase();

                if query.is_empty() && current_mode == Mode::Mention {
                    host.hide();
                    return;
                }

                *mode.borrow_mut() = current_mode;

                let matches: Vec<(String, String, Option<String>)> = match current_mode {
                    Mode::Mention => {
                        let user_map = users.borrow();
                        let mut m: Vec<_> = user_map
                            .iter()
                            .filter(|(_, name)| name.to_lowercase().contains(&query))
                            .map(|(id, name)| (id.clone(), name.clone(), None))
                            .collect();
                        m.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
                        m.truncate(8);
                        m
                    }
                    Mode::Emoji => search_emoji(&query),
                };

                if matches.is_empty() {
                    host.hide();
                    return;
                }

                populate_list(&list, &matches);
                if let Some(first) = list.row_at_index(0) {
                    list.select_row(Some(&first));
                }
                host.show();
                tv.grab_focus();
            });
        }

        // Handle selection from the list
        {
            let host = host.clone();
            let tv = text_view.clone();
            let mode = mode.clone();
            let cb = on_emoji_picked.clone();
            list.connect_row_activated(move |_, row| {
                let id = row.widget_name().to_string();
                if id.is_empty() {
                    return;
                }
                match *mode.borrow() {
                    Mode::Mention => insert_mention(&tv, &id),
                    Mode::Emoji => {
                        insert_emoji(&tv, &id);
                        if let Some(f) = cb.borrow().as_ref() {
                            f(&id);
                        }
                    }
                }
                host.hide();
            });
        }

        // Keyboard navigation
        {
            let host = host.clone();
            let list = list.clone();
            let tv = text_view.clone();
            let cb = on_emoji_picked.clone();
            let key_controller = gtk::EventControllerKey::new();
            key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            key_controller.connect_key_pressed(move |_, key, _, _| {
                if !host.is_visible() {
                    return gtk4::glib::Propagation::Proceed;
                }

                match key {
                    gtk4::gdk::Key::Down => {
                        let selected = list.selected_row();
                        let next = match selected {
                            Some(row) => list.row_at_index(row.index() + 1),
                            None => list.row_at_index(0),
                        };
                        if let Some(row) = next {
                            list.select_row(Some(&row));
                        }
                        gtk4::glib::Propagation::Stop
                    }
                    gtk4::gdk::Key::Up => {
                        if let Some(row) = list.selected_row() {
                            let idx = row.index();
                            if idx > 0 {
                                if let Some(prev) = list.row_at_index(idx - 1) {
                                    list.select_row(Some(&prev));
                                }
                            }
                        }
                        gtk4::glib::Propagation::Stop
                    }
                    gtk4::gdk::Key::Return | gtk4::gdk::Key::Tab => {
                        if let Some(row) = list.selected_row() {
                            let id = row.widget_name().to_string();
                            if !id.is_empty() {
                                match *mode.borrow() {
                                    Mode::Mention => insert_mention(&tv, &id),
                                    Mode::Emoji => {
                                        insert_emoji(&tv, &id);
                                        if let Some(f) = cb.borrow().as_ref() {
                                            f(&id);
                                        }
                                    }
                                }
                                host.hide();
                                return gtk4::glib::Propagation::Stop;
                            }
                        }
                        host.hide();
                        gtk4::glib::Propagation::Proceed
                    }
                    gtk4::gdk::Key::Escape => {
                        host.hide();
                        gtk4::glib::Propagation::Stop
                    }
                    _ => gtk4::glib::Propagation::Proceed,
                }
            });
            text_view.add_controller(key_controller);
        }

        // Dismiss when the text view loses focus or is unmapped
        {
            let h = host.clone();
            let focus_ctl = gtk::EventControllerFocus::new();
            focus_ctl.connect_leave(move |_| {
                h.hide();
            });
            text_view.add_controller(focus_ctl);
        }
        {
            let h = host.clone();
            text_view.connect_unmap(move |_| {
                h.hide();
            });
        }

        Self {
            users,
            _host: host,
            _list: list,
            on_emoji_picked,
        }
    }

    pub fn set_users(&self, users: &HashMap<String, String>) {
        *self.users.borrow_mut() = users.clone();
    }

    /// Set a callback invoked when an emoji is picked (for recording recent usage).
    pub fn set_on_emoji_picked(&self, f: Rc<dyn Fn(&str)>) {
        *self.on_emoji_picked.borrow_mut() = Some(f);
    }
}

/// Replace the `@query` text with a Slack mention `<@USERID> `.
fn insert_mention(text_view: &TextView, user_id: &str) {
    let buffer = text_view.buffer();
    let insert = buffer.iter_at_mark(&buffer.get_insert());

    let mut start = insert;
    while start.backward_char() {
        if start.char() == '@' {
            break;
        }
    }

    let mut end = insert;
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &format!("<@{user_id}> "));
    text_view.grab_focus();
}

/// Replace the `:query` text with the emoji shortcode `:shortcode: `.
fn insert_emoji(text_view: &TextView, shortcode: &str) {
    let buffer = text_view.buffer();
    let insert = buffer.iter_at_mark(&buffer.get_insert());

    let mut start = insert;
    while start.backward_char() {
        if start.char() == ':' {
            break;
        }
    }

    let mut end = insert;
    buffer.delete(&mut start, &mut end);

    // If the emoji has a unicode glyph, insert that directly; otherwise use :shortcode:
    if let Some(glyph) = resolve_slack_shortcode(shortcode) {
        buffer.insert(&mut start, &format!("{glyph} "));
    } else if get_custom_emoji_path(shortcode).is_some() {
        buffer.insert(&mut start, &format!(":{shortcode}: "));
    } else {
        buffer.insert(&mut start, &format!(":{shortcode}: "));
    }
    text_view.grab_focus();
}

/// Build a reaction popover with an emoji autocomplete text input.
/// When an emoji is selected, calls `on_react(shortcode)` and closes the popover.
/// `on_emoji_used` is called to persist the emoji to recent history (optional).
pub fn build_reaction_popover(
    parent: &impl IsA<gtk::Widget>,
    on_react: Rc<dyn Fn(&str)>,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_parent(parent);
    popover.set_autohide(true);
    popover.set_has_arrow(true);
    popover.set_position(gtk::PositionType::Top);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);

    let tv = TextView::new();
    tv.set_width_request(200);
    tv.set_top_margin(6);
    tv.set_bottom_margin(6);
    tv.set_left_margin(6);
    tv.set_right_margin(6);
    tv.add_css_class("card");
    tv.set_accepts_tab(false);
    tv.set_wrap_mode(gtk::WrapMode::None);

    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&tv));
    vbox.append(&frame);
    popover.set_child(Some(&vbox));

    // Suppress Enter from inserting newlines in this single-line input
    {
        let key_ctl = gtk::EventControllerKey::new();
        key_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
        key_ctl.connect_key_pressed(|_, key, _, _| {
            if key == gtk4::gdk::Key::Return || key == gtk4::gdk::Key::KP_Enter {
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
        tv.add_controller(key_ctl);
    }

    // Attach emoji-only autocomplete
    let ac = Autocomplete::attach(&tv);

    // When emoji is picked: call the reaction callback, record recent, close popover
    let popover_weak = popover.downgrade();
    let on_react2 = on_react.clone();
    ac.set_on_emoji_picked(Rc::new(move |shortcode: &str| {
        record_emoji_used(shortcode);
        on_react2(shortcode);
        if let Some(p) = popover_weak.upgrade() {
            p.popdown();
        }
    }));

    // Focus the text view when the popover opens, and pre-fill ":"
    // Deferred so the popover is fully mapped before the autocomplete triggers.
    {
        let tv2 = tv.clone();
        popover.connect_show(move |_| {
            let t = tv2.clone();
            gtk4::glib::idle_add_local_once(move || {
                t.buffer().set_text(":");
                let iter = t.buffer().end_iter();
                t.buffer().place_cursor(&iter);
                t.grab_focus();
            });
        });
    }

    popover
}

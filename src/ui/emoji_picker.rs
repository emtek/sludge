use gtk4::prelude::*;
use gtk4::{self as gtk};
use std::cell::RefCell;
use std::rc::Rc;

const COLUMNS: u32 = 9;
const MAX_RESULTS: usize = 63;
const MAX_RECENT: usize = 18;

/// Category labels with a representative emoji and the `emoji` crate group name.
const CATEGORIES: &[(&str, &str)] = &[
    ("\u{1f600}", "Smileys & Emotion"),
    ("\u{1f44b}", "People & Body"),
    ("\u{1f431}", "Animals & Nature"),
    ("\u{1f34e}", "Food & Drink"),
    ("\u{26bd}",  "Activities"),
    ("\u{1f3e0}", "Travel & Places"),
    ("\u{1f4a1}", "Objects"),
    ("\u{2764}\u{fe0f}", "Symbols"),
    ("\u{1f3f4}", "Flags"),
];

thread_local! {
    static RECENT: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
    /// Cached flat list of all fully-qualified emojis, grouped.
    static ALL_EMOJIS: Vec<&'static emoji::Emoji> = {
        emoji::search::search_name("")
    };
}

fn push_recent(shortcode: &str, glyph: &str) {
    RECENT.with(|r| {
        let mut v = r.borrow_mut();
        v.retain(|(sc, _)| sc != shortcode);
        v.insert(0, (shortcode.to_string(), glyph.to_string()));
        v.truncate(MAX_RECENT);
    });
}

fn get_recent() -> Vec<(String, String)> {
    RECENT.with(|r| r.borrow().clone())
}

fn shortcode_for(e: &emoji::Emoji) -> String {
    emojis::get(e.glyph)
        .and_then(|em| em.shortcode())
        .unwrap_or(e.name)
        .to_string()
}

/// Build a lightweight emoji picker popover attached to `parent`.
pub fn build(parent: &impl IsA<gtk::Widget>, on_pick: Rc<dyn Fn(&str)>) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_parent(parent);
    popover.set_autohide(true);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 4);
    outer.set_margin_top(8);
    outer.set_margin_bottom(8);
    outer.set_margin_start(8);
    outer.set_margin_end(8);
    outer.set_width_request(340);
    outer.set_height_request(320);

    // Search
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search emoji..."));
    outer.append(&search);

    // Category tabs
    let cat_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cat_bar.add_css_class("linked");
    for &(icon, name) in CATEGORIES {
        let btn = gtk::Button::with_label(icon);
        btn.add_css_class("flat");
        btn.set_tooltip_text(Some(name));
        cat_bar.append(&btn);
    }
    outer.append(&cat_bar);

    // Scrollable grid
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let grid = gtk::FlowBox::new();
    grid.set_max_children_per_line(COLUMNS);
    grid.set_min_children_per_line(COLUMNS);
    grid.set_selection_mode(gtk::SelectionMode::None);
    grid.set_homogeneous(true);
    grid.set_row_spacing(2);
    grid.set_column_spacing(2);
    scrolled.set_child(Some(&grid));
    outer.append(&scrolled);

    popover.set_child(Some(&outer));

    // Shared populate function
    let grid_ref = grid.clone();
    let popover_weak = popover.downgrade();
    let on_pick_ref = on_pick.clone();
    let populate: Rc<dyn Fn(Vec<(String, String)>)> = Rc::new(move |items: Vec<(String, String)>| {
        while let Some(child) = grid_ref.first_child() {
            grid_ref.remove(&child);
        }
        for (shortcode, glyph) in &items {
            let btn = gtk::Button::with_label(glyph);
            btn.add_css_class("flat");
            btn.add_css_class("emoji-picker-btn");
            btn.set_tooltip_text(Some(&format!(":{shortcode}:")));
            let pw = popover_weak.clone();
            let on_pick = on_pick_ref.clone();
            let sc = shortcode.clone();
            let gl = glyph.clone();
            btn.connect_clicked(move |_| {
                push_recent(&sc, &gl);
                on_pick(&sc);
                if let Some(p) = pw.upgrade() { p.popdown(); }
            });
            grid_ref.insert(&btn, -1);
        }
    });

    // Category clicks
    {
        let mut idx = 0;
        let mut child = cat_bar.first_child();
        while let Some(w) = child {
            let cat_idx = idx;
            let populate = populate.clone();
            let search_ref = search.clone();
            if let Some(btn) = w.downcast_ref::<gtk::Button>() {
                btn.connect_clicked(move |_| {
                    search_ref.set_text("");
                    populate(get_category(cat_idx));
                });
            }
            child = w.next_sibling();
            idx += 1;
        }
    }

    // Search
    {
        let populate = populate.clone();
        search.connect_search_changed(move |entry| {
            let q = entry.text().to_string();
            if q.is_empty() {
                populate(default_view());
            } else {
                populate(search_emojis(&q));
            }
        });
    }

    // On show: reset to default
    {
        let populate = populate.clone();
        let search_ref = search.clone();
        popover.connect_show(move |_| {
            search_ref.set_text("");
            populate(default_view());
            search_ref.grab_focus();
        });
    }

    popover
}

fn default_view() -> Vec<(String, String)> {
    let mut out = get_recent();
    out.extend(get_category(0));
    out.truncate(MAX_RESULTS);
    out
}

fn get_category(cat_idx: usize) -> Vec<(String, String)> {
    let group = CATEGORIES.get(cat_idx).map(|(_, g)| *g).unwrap_or("");
    ALL_EMOJIS.with(|all| {
        all.iter()
            .filter(|e| e.group == group && e.status == emoji::Status::FullyQualified && !e.is_variant)
            .take(MAX_RESULTS)
            .map(|e| (shortcode_for(e), e.glyph.to_string()))
            .collect()
    })
}

fn search_emojis(query: &str) -> Vec<(String, String)> {
    emoji::search::search_name(query)
        .into_iter()
        .filter(|e| e.status == emoji::Status::FullyQualified && !e.is_variant)
        .take(MAX_RESULTS)
        .map(|e| (shortcode_for(e), e.glyph.to_string()))
        .collect()
}

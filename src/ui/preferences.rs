use gtk4::prelude::*;
use gtk4::{self as gtk, Application, Label};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tokio::runtime::Handle;

use crate::db::{Database, Preferences};

/// Callback invoked whenever the user changes a preference. Runs on the GTK
/// main thread so callers can safely update UI state.
pub type PreferencesCallback = Rc<dyn Fn(&Preferences)>;

/// Show the Preferences window. `current` provides the initial slider values;
/// `on_changed` is invoked for each change and is also responsible for letting
/// the rest of the app react (e.g. re-filtering the sidebar). The new values
/// are persisted to the database automatically.
pub fn show_preferences_window(
    app: &Application,
    parent: &gtk::Window,
    db: Arc<Database>,
    rt: Handle,
    current: Preferences,
    on_changed: PreferencesCallback,
) {
    let dialog = gtk::Window::builder()
        .title("Preferences")
        .transient_for(parent)
        .modal(true)
        .default_width(460)
        .build();

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 18);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    let prefs_state = Rc::new(RefCell::new(current.clone()));

    // ── History length (months) ──
    let hist_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let hist_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let hist_label = Label::new(Some("Message history to fetch"));
    hist_label.add_css_class("heading");
    hist_label.set_halign(gtk::Align::Start);
    hist_label.set_hexpand(true);
    let hist_value = Label::new(Some(&months_label(current.history_months)));
    hist_value.add_css_class("dim-label");
    hist_header.append(&hist_label);
    hist_header.append(&hist_value);
    hist_section.append(&hist_header);

    let hist_desc = Label::new(Some(
        "How far back to backfill messages for search. Takes effect on next launch.",
    ));
    hist_desc.add_css_class("dim-label");
    hist_desc.add_css_class("caption");
    hist_desc.set_halign(gtk::Align::Start);
    hist_desc.set_wrap(true);
    hist_desc.set_xalign(0.0);
    hist_section.append(&hist_desc);

    let hist_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 24.0, 1.0);
    hist_scale.set_digits(0);
    hist_scale.set_draw_value(false);
    hist_scale.set_round_digits(0);
    hist_scale.set_hexpand(true);
    hist_scale.set_value(current.history_months.clamp(1, 24) as f64);
    for m in [1.0, 3.0, 6.0, 12.0, 18.0, 24.0] {
        hist_scale.add_mark(m, gtk::PositionType::Bottom, None);
    }
    hist_section.append(&hist_scale);
    vbox.append(&hist_section);

    // ── Activity recency (weeks) ──
    let act_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let act_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let act_label = Label::new(Some("Recent activity window"));
    act_label.add_css_class("heading");
    act_label.set_halign(gtk::Align::Start);
    act_label.set_hexpand(true);
    let act_value = Label::new(Some(&weeks_label(current.activity_weeks)));
    act_value.add_css_class("dim-label");
    act_header.append(&act_label);
    act_header.append(&act_value);
    act_section.append(&act_header);

    let act_desc = Label::new(Some(
        "Channels and DMs without activity in this window are hidden from the sidebar.",
    ));
    act_desc.add_css_class("dim-label");
    act_desc.add_css_class("caption");
    act_desc.set_halign(gtk::Align::Start);
    act_desc.set_wrap(true);
    act_desc.set_xalign(0.0);
    act_section.append(&act_desc);

    let act_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 12.0, 1.0);
    act_scale.set_digits(0);
    act_scale.set_draw_value(false);
    act_scale.set_round_digits(0);
    act_scale.set_hexpand(true);
    act_scale.set_value(current.activity_weeks.clamp(1, 12) as f64);
    for w in [1.0, 2.0, 4.0, 8.0, 12.0] {
        act_scale.add_mark(w, gtk::PositionType::Bottom, None);
    }
    act_section.append(&act_scale);
    vbox.append(&act_section);

    // ── Cache row ──
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    vbox.append(&sep);

    let cache_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let cache_labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    cache_labels.set_hexpand(true);
    let cache_heading = Label::new(Some("Cache"));
    cache_heading.add_css_class("heading");
    cache_heading.set_halign(gtk::Align::Start);
    let cache_desc = Label::new(Some(
        "Delete cached messages, channel/user data, and downloaded images.",
    ));
    cache_desc.add_css_class("dim-label");
    cache_desc.add_css_class("caption");
    cache_desc.set_halign(gtk::Align::Start);
    cache_desc.set_wrap(true);
    cache_desc.set_xalign(0.0);
    cache_labels.append(&cache_heading);
    cache_labels.append(&cache_desc);
    let clear_btn = gtk::Button::with_label("Clear cache…");
    clear_btn.add_css_class("destructive-action");
    clear_btn.set_valign(gtk::Align::Center);
    cache_row.append(&cache_labels);
    cache_row.append(&clear_btn);
    vbox.append(&cache_row);

    // ── Close button ──
    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk::Align::End);
    btn_row.set_margin_top(4);
    let close = gtk::Button::with_label("Close");
    btn_row.append(&close);
    vbox.append(&btn_row);

    dialog.set_child(Some(&vbox));

    // ── Wire changes ──
    {
        let hist_value = hist_value.clone();
        let prefs_state = prefs_state.clone();
        let db = db.clone();
        let rt = rt.clone();
        let on_changed = on_changed.clone();
        hist_scale.connect_value_changed(move |s| {
            let v = (s.value().round() as u32).max(1);
            let mut p = prefs_state.borrow_mut();
            if p.history_months == v {
                return;
            }
            p.history_months = v;
            hist_value.set_text(&months_label(v));
            let prefs = p.clone();
            drop(p);
            persist(&rt, &db, &prefs);
            on_changed(&prefs);
        });
    }

    {
        let act_value = act_value.clone();
        let prefs_state = prefs_state.clone();
        let db = db.clone();
        let rt = rt.clone();
        let on_changed = on_changed.clone();
        act_scale.connect_value_changed(move |s| {
            let v = (s.value().round() as u32).max(1);
            let mut p = prefs_state.borrow_mut();
            if p.activity_weeks == v {
                return;
            }
            p.activity_weeks = v;
            act_value.set_text(&weeks_label(v));
            let prefs = p.clone();
            drop(p);
            persist(&rt, &db, &prefs);
            on_changed(&prefs);
        });
    }

    {
        let dialog = dialog.clone();
        close.connect_clicked(move |_| dialog.close());
    }

    {
        let app = app.clone();
        let parent = parent.clone();
        let dialog_c = dialog.clone();
        let db = db.clone();
        let rt = rt.clone();
        clear_btn.connect_clicked(move |_| {
            show_clear_cache_confirm(&app, &parent, &dialog_c, db.clone(), rt.clone());
        });
    }

    dialog.present();
}

fn months_label(m: u32) -> String {
    if m == 1 { "1 month".to_string() } else { format!("{m} months") }
}

fn weeks_label(w: u32) -> String {
    if w == 1 { "1 week".to_string() } else { format!("{w} weeks") }
}

fn persist(rt: &Handle, db: &Arc<Database>, prefs: &Preferences) {
    let db = db.clone();
    let prefs = prefs.clone();
    rt.spawn(async move {
        db.save_preferences(&prefs).await;
    });
}

/// Confirm-and-wipe dialog mirroring the behaviour of the old hamburger-menu
/// Clear cache entry: wipes the DB cache + image cache, then re-runs the
/// first-launch flow so the app picks up saved credentials.
fn show_clear_cache_confirm(
    app: &Application,
    parent_window: &gtk::Window,
    prefs_dialog: &gtk::Window,
    db: Arc<Database>,
    rt: Handle,
) {
    let dialog = gtk::Window::builder()
        .title("Clear cache?")
        .transient_for(prefs_dialog)
        .modal(true)
        .default_width(380)
        .build();
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    let msg = Label::new(Some(
        "This will delete all cached messages, channel/user cache, and downloaded images. Your login will not be affected. The app will reload. Continue?",
    ));
    msg.set_wrap(true);
    msg.set_xalign(0.0);
    vbox.append(&msg);
    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Clear cache");
    confirm.add_css_class("destructive-action");
    btn_row.append(&cancel);
    btn_row.append(&confirm);
    vbox.append(&btn_row);
    dialog.set_child(Some(&vbox));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }

    let app = app.clone();
    let parent_window = parent_window.clone();
    let prefs_dialog = prefs_dialog.clone();
    let confirm_dialog = dialog.clone();
    confirm.connect_clicked(move |_| {
        let db_clear = db.clone();
        let db_launch = db.clone();
        let rt_spawn = rt.clone();
        let rt_launch = rt.clone();
        let app_launch = app.clone();
        let window_close = parent_window.clone();
        let prefs_close = prefs_dialog.clone();
        let confirm_close = confirm_dialog.clone();
        gtk4::glib::spawn_future_local(async move {
            let _ = rt_spawn
                .spawn(async move {
                    db_clear.clear_cache().await;
                    let img_dir = dirs::data_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join("sludge")
                        .join("image_cache");
                    let _ = tokio::fs::remove_dir_all(&img_dir).await;
                    let _ = tokio::fs::create_dir_all(&img_dir).await;
                })
                .await;
            confirm_close.close();
            prefs_close.close();
            window_close.close();
            // Re-run the first-launch flow with the saved credentials.
            crate::ui::login::launch_or_login(&app_launch, rt_launch, db_launch);
        });
    });
    dialog.present();
}

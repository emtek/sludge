use gtk4::prelude::*;
use gtk4::{self as gtk, Application, ApplicationWindow, DropDown, Label, PasswordEntry, Stack};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::db::{Database, SavedCredentials};
use crate::slack::client::Client;
use crate::slack::socket::SlackEvent;
use crate::ui::app::build_app;

pub fn show_login(app: &Application, rt: tokio::runtime::Handle, db: Arc<Database>) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Slag — Sign In")
        .default_width(400)
        .default_height(340)
        .resizable(false)
        .build();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 16);
    outer.set_margin_top(24);
    outer.set_margin_bottom(24);
    outer.set_margin_start(32);
    outer.set_margin_end(32);
    outer.set_valign(gtk::Align::Center);
    outer.set_vexpand(true);

    let title = Label::new(Some("Sign in to Slag"));
    title.add_css_class("title-1");
    outer.append(&title);

    let mode_model = gtk::StringList::new(&["Stealth (browser session)", "Bot token"]);
    let mode_dropdown = DropDown::new(Some(mode_model), gtk::Expression::NONE);
    mode_dropdown.set_selected(0);
    outer.append(&mode_dropdown);

    let stack = Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    // Stealth page
    let stealth_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let xoxc_label = Label::new(Some("xoxc Token"));
    xoxc_label.set_halign(gtk::Align::Start);
    stealth_box.append(&xoxc_label);
    let xoxc_entry = PasswordEntry::new();
    xoxc_entry.set_show_peek_icon(true);
    xoxc_entry.set_placeholder_text(Some("xoxc-..."));
    stealth_box.append(&xoxc_entry);
    let xoxd_label = Label::new(Some("xoxd Cookie"));
    xoxd_label.set_halign(gtk::Align::Start);
    stealth_box.append(&xoxd_label);
    let xoxd_entry = PasswordEntry::new();
    xoxd_entry.set_show_peek_icon(true);
    xoxd_entry.set_placeholder_text(Some("xoxd-..."));
    stealth_box.append(&xoxd_entry);
    stack.add_named(&stealth_box, Some("stealth"));

    // Bot page
    let bot_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let bot_label = Label::new(Some("Bot Token"));
    bot_label.set_halign(gtk::Align::Start);
    bot_box.append(&bot_label);
    let bot_entry = PasswordEntry::new();
    bot_entry.set_show_peek_icon(true);
    bot_entry.set_placeholder_text(Some("xoxb-..."));
    bot_box.append(&bot_entry);
    let app_label = Label::new(Some("App Token (optional, for real-time events)"));
    app_label.set_halign(gtk::Align::Start);
    bot_box.append(&app_label);
    let app_entry = PasswordEntry::new();
    app_entry.set_show_peek_icon(true);
    app_entry.set_placeholder_text(Some("xapp-..."));
    bot_box.append(&app_entry);
    stack.add_named(&bot_box, Some("bot"));
    stack.set_visible_child_name("stealth");

    outer.append(&stack);

    let stack_ref = stack.clone();
    mode_dropdown.connect_selected_notify(move |dd| {
        let name = if dd.selected() == 0 { "stealth" } else { "bot" };
        stack_ref.set_visible_child_name(name);
    });

    let error_label = Label::new(None);
    error_label.add_css_class("error");
    error_label.set_wrap(true);
    error_label.set_visible(false);
    outer.append(&error_label);

    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    outer.append(&spinner);

    let connect_btn = gtk::Button::with_label("Connect");
    connect_btn.add_css_class("suggested-action");
    connect_btn.add_css_class("pill");
    outer.append(&connect_btn);

    window.set_child(Some(&outer));

    let app_ref = app.clone();
    let window_ref = window.clone();
    let connecting = Rc::new(RefCell::new(false));

    connect_btn.connect_clicked(move |btn| {
        if *connecting.borrow() {
            return;
        }

        let mode = mode_dropdown.selected();

        // Collect credentials for both client creation and DB storage
        let (saved_creds, client) = if mode == 0 {
            let xoxc = xoxc_entry.text().to_string();
            let xoxd = xoxd_entry.text().to_string();
            if xoxc.is_empty() || xoxd.is_empty() {
                error_label.set_text("Both xoxc token and xoxd cookie are required.");
                error_label.set_visible(true);
                return;
            }
            let creds = SavedCredentials {
                auth_mode: "stealth".into(),
                xoxc_token: Some(xoxc.clone()),
                xoxd_cookie: Some(xoxd.clone()),
                bot_token: None,
                app_token: None,
                workspace_url: None,
            };
            (creds, Client::new_stealth(xoxc, xoxd))
        } else {
            let bot = bot_entry.text().to_string();
            if bot.is_empty() {
                error_label.set_text("Bot token is required.");
                error_label.set_visible(true);
                return;
            }
            let app_tok = {
                let t = app_entry.text().to_string();
                if t.is_empty() { None } else { Some(t) }
            };
            let creds = SavedCredentials {
                auth_mode: "bot".into(),
                xoxc_token: None,
                xoxd_cookie: None,
                bot_token: Some(bot.clone()),
                app_token: app_tok.clone(),
                workspace_url: None,
            };
            (
                creds,
                Client::new_bot(slacko::AuthConfig::bot(&bot), app_tok),
            )
        };

        *connecting.borrow_mut() = true;
        error_label.set_visible(false);
        spinner.set_visible(true);
        spinner.start();
        btn.set_sensitive(false);

        let rt = rt.clone();
        let db = db.clone();
        let app_ref = app_ref.clone();
        let window_ref = window_ref.clone();
        let error_label = error_label.clone();
        let spinner = spinner.clone();
        let btn = btn.clone();
        let connecting = connecting.clone();

        let rt2 = rt.clone();
        gtk4::glib::spawn_future_local(async move {
            // Auth test + save credentials in one spawned task
            let db_save = db.clone();
            let saved_creds_clone = saved_creds.clone();
            let result = rt2
                .spawn(async move {
                    let mut test_client = client.clone();
                    let info = test_client.auth_test().await?;

                    // Save credentials to database
                    let mut creds_to_save = saved_creds_clone;
                    creds_to_save.workspace_url = Some(info.url.clone());
                    if let Err(e) = db_save.save_credentials(&creds_to_save).await {
                        tracing::error!("Failed to save credentials: {e}");
                    }

                    Ok::<(Client, _), String>((test_client, info))
                })
                .await
                .unwrap();

            spinner.stop();
            spinner.set_visible(false);
            btn.set_sensitive(true);
            *connecting.borrow_mut() = false;

            match result {
                Ok((client, info)) => {
                    tracing::info!(
                        "auth.test OK — user: {}, team: {}, url: {}",
                        info.user,
                        info.team,
                        info.url
                    );

                    let (event_tx, event_rx) = mpsc::unbounded_channel::<SlackEvent>();
                    let (presence_tx, presence_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<String>>();
                    if let Some(sm_client) = client.socket_mode_client().cloned() {
                        rt.spawn(crate::slack::socket::run_socket_mode(sm_client, event_tx));
                    } else if let Some((http, xoxc, xoxd, ws_url)) =
                        client.stealth_rtm_params()
                    {
                        rt.spawn(crate::slack::socket::run_rtm_stealth(
                            http, xoxc, xoxd, ws_url, event_tx, presence_rx,
                        ));
                    }

                    build_app(&app_ref, client, rt, event_rx, db, info.user_id, presence_tx);
                    window_ref.close();
                }
                Err(e) => {
                    error_label.set_text(&format!("Authentication failed: {e}"));
                    error_label.set_visible(true);
                }
            }
        });
    });

    window.present();
}

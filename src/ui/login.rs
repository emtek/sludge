use gtk4::prelude::*;
use gtk4::{self as gtk, Application, ApplicationWindow, Label, PasswordEntry};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::db::{Database, SavedCredentials};
use crate::slack::client::Client;
use crate::slack::socket::SlackEvent;
use crate::ui::app::build_app;

/// Try to auto-login with saved credentials; fall back to the login form.
/// Used on initial startup and after "Clear cache" to replay the first-launch flow.
pub fn launch_or_login(app: &Application, rt: tokio::runtime::Handle, db: Arc<Database>) {
    let db_saved = db.clone();
    let rt_saved = rt.clone();
    let app_clone = app.clone();
    gtk4::glib::spawn_future_local(async move {
        let saved = rt_saved
            .spawn(async move { db_saved.load_credentials().await })
            .await
            .unwrap();
        let Some(creds) = saved else {
            show_login(&app_clone, rt_saved, db);
            return;
        };

        let creds_clone = creds.clone();
        let rt2 = rt_saved.clone();
        let result = rt2
            .spawn(async move {
                let xoxc = creds_clone.xoxc_token.unwrap_or_default();
                let xoxd = creds_clone.xoxd_cookie.unwrap_or_default();
                if xoxc.is_empty() || xoxd.is_empty() {
                    return Err::<(crate::slack::client::Client, crate::slack::client::AuthInfo), String>(
                        "incomplete credentials".into()
                    );
                }
                let mut client = crate::slack::client::Client::new(xoxc, xoxd, creds_clone.workspace_url);
                let info = client.auth_test().await?;
                Ok((client, info))
            })
            .await
            .unwrap();

        match result {
            Ok((client, info)) => {
                let (event_tx, event_rx) = mpsc::unbounded_channel::<SlackEvent>();
                let (presence_tx, presence_rx) = mpsc::unbounded_channel::<Vec<String>>();
                let (http, xoxc, xoxd, ws_url) = client.rtm_params();
                rt_saved.spawn(crate::slack::socket::run_rtm_stealth(
                    http, xoxc, xoxd, ws_url, event_tx, presence_rx,
                ));
                build_app(&app_clone, client, rt_saved, event_rx, db, info.user_id, presence_tx, None, false);
            }
            Err(e) => {
                tracing::warn!("Auto-login failed: {e}");
                let _ = rt_saved
                    .spawn({
                        let db = db.clone();
                        async move { db.clear_credentials().await }
                    })
                    .await;
                show_login(&app_clone, rt_saved, db);
            }
        }
    });
}

pub fn show_login(app: &Application, rt: tokio::runtime::Handle, db: Arc<Database>) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Sludge — Sign In")
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

    let title = Label::new(Some("Sign in to Sludge"));
    title.add_css_class("title-1");
    outer.append(&title);

    let fields_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let xoxc_label = Label::new(Some("xoxc Token"));
    xoxc_label.set_halign(gtk::Align::Start);
    fields_box.append(&xoxc_label);
    let xoxc_entry = PasswordEntry::new();
    xoxc_entry.set_show_peek_icon(true);
    xoxc_entry.set_placeholder_text(Some("xoxc-..."));
    fields_box.append(&xoxc_entry);
    let xoxd_label = Label::new(Some("xoxd Cookie"));
    xoxd_label.set_halign(gtk::Align::Start);
    fields_box.append(&xoxd_label);
    let xoxd_entry = PasswordEntry::new();
    xoxd_entry.set_show_peek_icon(true);
    xoxd_entry.set_placeholder_text(Some("xoxd-..."));
    fields_box.append(&xoxd_entry);
    let ws_label = Label::new(Some("Workspace"));
    ws_label.set_halign(gtk::Align::Start);
    fields_box.append(&ws_label);
    let ws_entry = gtk::Entry::new();
    ws_entry.set_placeholder_text(Some("myteam.slack.com"));
    fields_box.append(&ws_entry);
    outer.append(&fields_box);

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

        let xoxc = xoxc_entry.text().to_string();
        let xoxd = xoxd_entry.text().to_string();
        if xoxc.is_empty() || xoxd.is_empty() {
            error_label.set_text("Both xoxc token and xoxd cookie are required.");
            error_label.set_visible(true);
            return;
        }
        let ws = ws_entry.text().to_string();
        let ws_url = if ws.is_empty() {
            None
        } else {
            let ws = ws.trim_start_matches("https://").trim_end_matches('/');
            Some(format!("https://{ws}"))
        };
        let saved_creds = SavedCredentials {
            xoxc_token: Some(xoxc.clone()),
            xoxd_cookie: Some(xoxd.clone()),
            workspace_url: ws_url.clone(),
        };
        let client = Client::new(xoxc, xoxd, ws_url);

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
                    tracing::info!("Attempting auth.test...");
                    let info = test_client.auth_test().await.map_err(|e| {
                        tracing::error!("auth.test failed: {e}");
                        e
                    })?;

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
                    let (http, xoxc, xoxd, ws_url) = client.rtm_params();
                    rt.spawn(crate::slack::socket::run_rtm_stealth(
                        http, xoxc, xoxd, ws_url, event_tx, presence_rx,
                    ));

                    build_app(&app_ref, client, rt, event_rx, db, info.user_id, presence_tx, None, false);
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

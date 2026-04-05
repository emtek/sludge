mod db;
pub mod mem;
mod slack;
mod ui;

use gtk4::prelude::*;
use gtk4::Application;
use std::sync::Arc;
use tokio::sync::mpsc;

use db::Database;
use slack::socket::SlackEvent;

fn main() {
    tracing_subscriber::fmt::init();
    mem::log_mem("startup");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    // Open database before GTK starts
    let database = rt.block_on(async {
        match Database::open(rt.handle()).await {
            Ok(db) => Arc::new(db),
            Err(e) => {
                eprintln!("Failed to open database: {e}");
                std::process::exit(1);
            }
        }
    });

    let app = Application::builder()
        .application_id("dev.slackfrontend.app")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    // Periodic memory reporter — logs RSS every 10 seconds
    gtk4::glib::timeout_add_seconds_local(10, || {
        mem::log_mem("periodic");
        gtk4::glib::ControlFlow::Continue
    });

    let rt_handle = rt.handle().clone();
    let db = database.clone();
    app.connect_activate(move |app| {
        libadwaita::init().expect("Failed to init libadwaita");

        let db = db.clone();
        let rt = rt_handle.clone();
        let app = app.clone();

        // Hold the application alive synchronously before spawning the async block;
        // the guard is moved into the future and dropped after a window exists.
        let hold_guard = gtk4::gio::prelude::ApplicationExtManual::hold(&app);

        // Try auto-login with saved credentials
        gtk4::glib::spawn_future_local(async move {
            let _hold_guard = hold_guard;
            let saved = {
                let db = db.clone();
                let rt2 = rt.clone();
                rt2.spawn(async move { db.load_credentials().await })
                    .await
                    .unwrap()
            };

            if let Some(creds) = saved {
                tracing::info!("Attempting auto-login with saved credentials...");

                // Build and auth-test client in one spawned task so workspace URL is set
                let creds_clone = creds.clone();
                let rt2 = rt.clone();
                let result = rt2
                    .spawn(async move {
                        let mut client = match creds_clone.auth_mode.as_str() {
                            "stealth" => {
                                let xoxc = creds_clone.xoxc_token.unwrap_or_default();
                                let xoxd = creds_clone.xoxd_cookie.unwrap_or_default();
                                if xoxc.is_empty() || xoxd.is_empty() {
                                    return Err("incomplete stealth credentials".into());
                                }
                                slack::client::Client::new_stealth(xoxc, xoxd)
                            }
                            "bot" => {
                                let token = creds_clone.bot_token.unwrap_or_default();
                                if token.is_empty() {
                                    return Err("empty bot token".into());
                                }
                                slack::client::Client::new_bot(
                                    slacko::AuthConfig::bot(&token),
                                    creds_clone.app_token,
                                )
                            }
                            other => return Err(format!("unknown auth mode: {other}")),
                        };
                        let info = client.auth_test().await?;
                        Ok::<(slack::client::Client, slack::client::AuthInfo), String>((client, info))
                    })
                    .await
                    .unwrap();

                match result {
                    Ok((client, info)) => {
                        tracing::info!("Auto-login succeeded");

                        let (event_tx, event_rx) = mpsc::unbounded_channel::<SlackEvent>();
                        let (presence_tx, presence_rx) = mpsc::unbounded_channel::<Vec<String>>();
                        if let Some(sm_client) = client.socket_mode_client().cloned() {
                            rt.spawn(slack::socket::run_socket_mode(sm_client, event_tx));
                        } else if let Some((http, xoxc, xoxd, ws_url)) =
                            client.stealth_rtm_params()
                        {
                            rt.spawn(slack::socket::run_rtm_stealth(
                                http, xoxc, xoxd, ws_url, event_tx, presence_rx,
                            ));
                        }

                        ui::app::build_app(&app, client, rt, event_rx, db, info.user_id, presence_tx);
                    }
                    Err(e) => {
                        tracing::warn!("Saved credentials expired: {e}");
                        let db2 = db.clone();
                        let rt2 = rt.clone();
                        let _ = rt2.spawn(async move { db2.clear_credentials().await }).await;
                        ui::login::show_login(&app, rt, db);
                    }
                }
            } else {
                ui::login::show_login(&app, rt, db);
            }

        });
    });

    app.run();
    drop(rt);
}

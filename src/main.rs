mod db;
pub mod mem;
mod search_provider;
mod slack;
mod ui;

use gtk4::prelude::*;
use gtk4::Application;
use std::sync::Arc;
use tokio::sync::mpsc;

use db::Database;
use slack::socket::SlackEvent;
use ui::app::StartupAction;

struct StartupFlags {
    action: Option<StartupAction>,
    hidden: bool,
}

fn parse_startup_flags() -> StartupFlags {
    let args: Vec<String> = std::env::args().collect();
    let mut action = None;
    let mut hidden = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--open" if i + 1 < args.len() => {
                let val = &args[i + 1];
                if let Some(channel_id) = val.strip_prefix("ch:") {
                    action = Some(StartupAction::OpenChannel(channel_id.to_string()));
                } else if let Some(rest) = val.strip_prefix("msg:") {
                    if let Some((channel_id, ts)) = rest.split_once(':') {
                        action = Some(StartupAction::OpenMessage {
                            channel_id: channel_id.to_string(),
                            message_ts: ts.to_string(),
                        });
                    }
                }
                i += 2;
            }
            "--search" if i + 1 < args.len() => {
                action = Some(StartupAction::Search(args[i + 1].clone()));
                i += 2;
            }
            "--hidden" => {
                hidden = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    StartupFlags { action, hidden }
}

fn main() {
    tracing_subscriber::fmt::init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    let app = Application::builder()
        .application_id("dev.sludge.app")
        .flags(gtk4::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    // Periodic heap trim every 10 seconds
    gtk4::glib::timeout_add_seconds_local(10, || {
        mem::trim_heap();
        gtk4::glib::ControlFlow::Continue
    });

    let startup_flags = parse_startup_flags();

    // Handle command-line for single-instance support: first launch calls activate(),
    // subsequent launches navigate via the existing "navigate" action.
    app.connect_command_line(|app, cmdline| {
        if app.windows().is_empty() {
            // First launch — trigger full UI setup via activate
            app.activate();
        } else {
            // Subsequent launch — present the existing window and optionally
            // navigate to a specific channel/message.
            if let Some(window) = app.windows().first() {
                window.present();
            }
            let args: Vec<String> = cmdline
                .arguments()
                .iter()
                .filter_map(|a| a.to_str().map(|s| s.to_string()))
                .collect();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--open" if i + 1 < args.len() => {
                        let val = &args[i + 1];
                        app.activate_action("navigate", Some(&val.to_variant()));
                        break;
                    }
                    _ => i += 1,
                }
            }
        }
        0.into()
    });

    let rt_handle = rt.handle().clone();
    let activated = std::cell::Cell::new(false);
    app.connect_activate(move |app| {
        if activated.replace(true) {
            // Already activated (D-Bus + command-line race); just present existing window.
            if let Some(window) = app.windows().first() {
                window.present();
            }
            return;
        }
        libadwaita::init().expect("Failed to init libadwaita");

        // Install embedded app icon (PNG) into user icon theme.
        // Only write the PNG files — never touch index.theme as that
        // shadows the system hicolor theme and breaks other apps' icons.
        {
            let base = dirs::data_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("icons/hicolor");
            let icon_data: &[(&str, &[u8])] = &[
                ("256x256", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/256x256/apps/sludge.png"))[..]),
                ("128x128", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/128x128/apps/sludge.png"))[..]),
                ("64x64", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/64x64/apps/sludge.png"))[..]),
                ("48x48", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/48x48/apps/sludge.png"))[..]),
            ];
            for (size, bytes) in icon_data {
                let icon_dir = base.join(format!("{size}/apps"));
                std::fs::create_dir_all(&icon_dir).ok();
                std::fs::write(icon_dir.join("sludge.png"), bytes).ok();
            }

            // Remove any leftover index.theme we may have created previously
            let bad_index = base.join("index.theme");
            if bad_index.exists() {
                std::fs::remove_file(&bad_index).ok();
            }

            gtk4::Window::set_default_icon_name("sludge");
        }

        let rt = rt_handle.clone();
        let app = app.clone();
        let startup_action = startup_flags.action.clone();
        let start_hidden = startup_flags.hidden;

        // Hold the application alive synchronously before spawning the async block;
        // the guard is moved into the future and dropped after a window exists.
        let hold_guard = gtk4::gio::prelude::ApplicationExtManual::hold(&app);

        // Try auto-login with saved credentials
        gtk4::glib::spawn_future_local(async move {
            let _hold_guard = hold_guard;

            // Open database (deferred to here so secondary instances don't block on DB lock)
            let rt_for_db = rt.clone();
            let db = match rt.spawn(async move {
                Database::open(&rt_for_db).await
            }).await.unwrap() {
                Ok(db) => Arc::new(db),
                Err(e) => {
                    eprintln!("Failed to open database: {e}");
                    return;
                }
            };

            // Register the GNOME Shell search provider on D-Bus from within the main app,
            // so it shares the database connection (SurrealKV only allows one process).
            {
                let db = db.clone();
                let rt2 = rt.clone();
                rt2.spawn(async move {
                    match search_provider::register_search_provider(db).await {
                        Ok(conn) => {
                            // Leak the connection to keep it alive for the app's lifetime
                            std::mem::forget(conn);
                        }
                        Err(e) => tracing::warn!("Failed to register search provider: {e}"),
                    }
                });
            }

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
                        let xoxc = creds_clone.xoxc_token.unwrap_or_default();
                        let xoxd = creds_clone.xoxd_cookie.unwrap_or_default();
                        if xoxc.is_empty() || xoxd.is_empty() {
                            return Err("incomplete credentials".into());
                        }
                        let mut client = slack::client::Client::new(xoxc, xoxd, creds_clone.workspace_url);
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
                        let (http, xoxc, xoxd, ws_url) = client.rtm_params();
                        rt.spawn(slack::socket::run_rtm_stealth(
                            http, xoxc, xoxd, ws_url, event_tx, presence_rx,
                        ));

                        ui::app::build_app(&app, client, rt, event_rx, db, info.user_id, presence_tx, startup_action.clone(), start_hidden);
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

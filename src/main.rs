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
        .application_id("dev.slag.app")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    // Periodic memory reporter — logs RSS every 10 seconds (with heap trim)
    gtk4::glib::timeout_add_seconds_local(10, || {
        mem::trim_heap();
        mem::log_mem("periodic (trimmed)");
        gtk4::glib::ControlFlow::Continue
    });

    let rt_handle = rt.handle().clone();
    let db = database.clone();
    app.connect_activate(move |app| {
        libadwaita::init().expect("Failed to init libadwaita");

        // Install embedded app icon (PNG) into user icon theme and set as default
        {
            let base = dirs::data_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("icons/hicolor");
            let sizes = ["256x256", "128x128", "64x64", "48x48"];
            let icon_data: &[(&str, &[u8])] = &[
                ("256x256", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/256x256/apps/slag.png"))[..]),
                ("128x128", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/128x128/apps/slag.png"))[..]),
                ("64x64", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/64x64/apps/slag.png"))[..]),
                ("48x48", &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hicolor/48x48/apps/slag.png"))[..]),
            ];
            for (size, bytes) in icon_data {
                let icon_dir = base.join(format!("{size}/apps"));
                std::fs::create_dir_all(&icon_dir).ok();
                std::fs::write(icon_dir.join("slag.png"), bytes).ok();
            }

            // Ensure index.theme lists all our icon directories so gtk4-update-icon-cache works
            let index_path = base.join("index.theme");
            let needs_update = match std::fs::read_to_string(&index_path) {
                Ok(content) => sizes.iter().any(|s| !content.contains(&format!("{s}/apps"))),
                Err(_) => true,
            };
            if needs_update {
                let dirs_list = sizes.iter().map(|s| format!("{s}/apps")).collect::<Vec<_>>().join(",");
                let index_content = format!(
                    "[Icon Theme]\nName=hicolor\nDirectories={dirs_list}\n\n{}\n",
                    sizes.iter().map(|s| {
                        let num: u32 = s.split('x').next().unwrap().parse().unwrap();
                        format!("[{s}/apps]\nSize={num}\nType=Fixed\n")
                    }).collect::<Vec<_>>().join("\n")
                );
                std::fs::write(&index_path, index_content).ok();
            }

            let _ = std::process::Command::new("gtk4-update-icon-cache")
                .arg("--force")
                .arg(&base)
                .status();
            gtk4::Window::set_default_icon_name("slag");
        }

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

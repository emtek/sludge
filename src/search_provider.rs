use std::collections::HashMap;
use std::sync::Arc;

use zbus::zvariant::{OwnedValue, Value};

use crate::db::Database;

/// Helper to convert a string into an OwnedValue for D-Bus `a{sv}` dicts.
fn str_val(s: impl Into<String>) -> OwnedValue {
    OwnedValue::try_from(Value::from(s.into()))
        .expect("string to OwnedValue conversion should never fail")
}

pub struct SearchProvider {
    db: Arc<Database>,
    /// When running as a headless process, sending args here signals the run loop
    /// to shut down (releasing the DB lock) and launch the main app with these args.
    launch_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<String>>>,
}

impl SearchProvider {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db, launch_tx: None }
    }

    pub fn new_headless(db: Arc<Database>, launch_tx: tokio::sync::mpsc::UnboundedSender<Vec<String>>) -> Self {
        Self { db, launch_tx: Some(launch_tx) }
    }

    /// Launch the main app, either by signaling the headless run loop to exit first
    /// (so it releases the DB lock), or by spawning directly (when running in-process).
    fn launch_app(&self, args: Vec<String>) {
        if let Some(tx) = &self.launch_tx {
            // Headless mode: signal shutdown so DB is released before the app starts
            let _ = tx.send(args);
        } else {
            // In-process: GTK single-instance will forward to the running app.
            // Wait in a background thread so the child doesn't become a zombie.
            if let Ok(mut child) = std::process::Command::new("sludge").args(&args).spawn() {
                std::thread::spawn(move || { let _ = child.wait(); });
            }
        }
    }
}

#[zbus::interface(name = "org.gnome.Shell.SearchProvider2")]
impl SearchProvider {
    async fn get_initial_result_set(&self, terms: Vec<String>) -> Vec<String> {
        let query = terms.join(" ");
        if query.is_empty() {
            return vec![];
        }

        // @query → search users, #query → search channels, otherwise FTS
        if let Some(user_query) = query.strip_prefix('@') {
            let needle = user_query.to_lowercase();
            let mut results = Vec::new();
            if let Some(users) = self.db.load_users().await {
                for u in &users {
                    if u.name.to_lowercase().contains(&needle)
                        || u.real_name
                            .as_deref()
                            .is_some_and(|r| r.to_lowercase().contains(&needle))
                    {
                        results.push(format!("user:{}", u.id));
                        if results.len() >= 10 {
                            break;
                        }
                    }
                }
            }
            return results;
        }

        if let Some(channel_query) = query.strip_prefix('#') {
            let needle = channel_query.to_lowercase();
            let mut results = Vec::new();
            if let Some(channels) = self.db.load_channels().await {
                for ch in &channels {
                    if let Some(name) = &ch.name {
                        if name.to_lowercase().contains(&needle) {
                            results.push(format!("ch:{}", ch.id));
                            if results.len() >= 10 {
                                break;
                            }
                        }
                    }
                }
            }
            return results;
        }

        // Default: full-text search on messages
        let mut results = Vec::new();
        let messages = self.db.search_messages(&query).await;
        for (channel_id, msg) in &messages {
            results.push(format!("msg:{}:{}", channel_id, msg.ts));
            if results.len() >= 10 {
                break;
            }
        }

        results
    }

    async fn get_subsearch_result_set(
        &self,
        _previous_results: Vec<String>,
        terms: Vec<String>,
    ) -> Vec<String> {
        self.get_initial_result_set(terms).await
    }

    async fn get_result_metas(
        &self,
        identifiers: Vec<String>,
    ) -> Vec<HashMap<String, OwnedValue>> {
        let channels = self.db.load_channels().await.unwrap_or_default();
        let users = self.db.load_users().await.unwrap_or_default();

        let mut metas = Vec::new();

        for id in &identifiers {
            let mut meta: HashMap<String, OwnedValue> = HashMap::new();
            meta.insert("id".into(), str_val(id.as_str()));

            if let Some(user_id) = id.strip_prefix("user:") {
                if let Some(u) = users.iter().find(|u| u.id == user_id) {
                    let display = u
                        .real_name
                        .as_deref()
                        .unwrap_or(u.name.as_str());
                    meta.insert("name".into(), str_val(format!("@{}", u.name)));
                    meta.insert("description".into(), str_val(display.to_string()));
                }
            } else if let Some(channel_id) = id.strip_prefix("ch:") {
                if let Some(ch) = channels.iter().find(|c| c.id == channel_id) {
                    let name = ch.name.as_deref().unwrap_or("Unknown");
                    meta.insert("name".into(), str_val(format!("#{name}")));
                    meta.insert("description".into(), str_val("Slack channel"));
                }
            } else if let Some(rest) = id.strip_prefix("msg:") {
                if let Some((channel_id, ts)) = rest.split_once(':') {
                    // Try the JSON cache first, fall back to the FTS index
                    let msg = {
                        let cached = self.db.load_messages(channel_id).await.unwrap_or_default();
                        match cached.into_iter().find(|m| m.ts == ts) {
                            Some(m) => Some(m),
                            None => self.db.get_indexed_message(channel_id, ts).await,
                        }
                    };
                    if let Some(msg) = msg {
                        let channel_name = channels
                            .iter()
                            .find(|c| c.id == channel_id)
                            .and_then(|c| c.name.as_deref())
                            .unwrap_or("DM");

                        let user_name = msg
                            .user
                            .as_ref()
                            .and_then(|uid| users.iter().find(|u| u.id == *uid))
                            .map(|u| u.name.as_str())
                            .unwrap_or("Unknown");

                        meta.insert(
                            "name".into(),
                            str_val(format!("{user_name} in #{channel_name}")),
                        );

                        let desc: String = msg.text.chars().take(100).collect();
                        let desc = if msg.text.chars().count() > 100 {
                            format!("{desc}\u{2026}")
                        } else {
                            desc
                        };
                        meta.insert("description".into(), str_val(desc));
                    }
                }
            }

            metas.push(meta);
        }

        metas
    }

    async fn activate_result(&self, identifier: String, _terms: Vec<String>, _timestamp: u32) {
        self.launch_app(vec!["--open".into(), identifier]);
    }

    async fn launch_search(&self, terms: Vec<String>, _timestamp: u32) {
        let query = terms.join(" ");
        self.launch_app(vec!["--search".into(), query]);
    }
}

/// Register the search provider on the session bus and return the connection.
/// The caller must keep the returned connection alive for the provider to remain active.
pub async fn register_search_provider(db: Arc<Database>) -> Result<zbus::Connection, Box<dyn std::error::Error>> {
    let provider = SearchProvider::new(db);

    let connection = zbus::connection::Builder::session()?
        .name("dev.sludge.app.SearchProvider")?
        .serve_at("/dev/sludge/app/SearchProvider", provider)?
        .build()
        .await?;

    tracing::info!("Search provider registered on D-Bus");
    Ok(connection)
}

/// Reason the headless search provider exited.
pub enum SearchProviderExit {
    /// The main app appeared on D-Bus — no further action needed.
    MainAppTookOver,
    /// A search result was activated — launch the main app with these args.
    Launch(Vec<String>),
}

/// Run the headless search provider until the main app takes over or a result is activated.
pub async fn run_search_provider(db: Arc<Database>) -> Result<SearchProviderExit, Box<dyn std::error::Error>> {
    let (launch_tx, mut launch_rx) = tokio::sync::mpsc::unbounded_channel();
    let provider = SearchProvider::new_headless(db, launch_tx);

    let _connection = zbus::connection::Builder::session()?
        .name("dev.sludge.app.SearchProvider")?
        .serve_at("/dev/sludge/app/SearchProvider", provider)?
        .build()
        .await?;

    tracing::info!("Headless search provider registered on D-Bus");

    // Watch for the main app to appear on D-Bus
    let monitor = zbus::Connection::session().await?;
    let proxy = zbus::fdo::DBusProxy::new(&monitor).await?;
    let mut name_stream = proxy.receive_name_owner_changed().await?;

    use futures_util::StreamExt;
    let exit = tokio::select! {
        Some(args) = launch_rx.recv() => SearchProviderExit::Launch(args),
        _ = async {
            while let Some(signal) = name_stream.next().await {
                if let Ok(args) = signal.args() {
                    if args.name.as_str() == "dev.sludge.app"
                        && args.new_owner.as_ref().is_some_and(|o| !o.is_empty())
                    {
                        tracing::info!("Main app appeared on D-Bus, search provider exiting");
                        return;
                    }
                }
            }
        } => SearchProviderExit::MainAppTookOver,
    };

    // _connection is dropped here, releasing the D-Bus name and the Arc<Database>
    Ok(exit)
}

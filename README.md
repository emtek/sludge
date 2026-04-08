<p align="center">
  <img src="assets/sludge.png" width="128" alt="Sludge logo">
</p>

# Sludge

A native GTK4/libadwaita Slack client for the Linux desktop built with Rust.

Sludge connects to Slack using browser session tokens (xoxc/xoxd) and communicates over Slack's RTM WebSocket API for real-time messaging. It provides a lightweight, keyboard-friendly alternative to the official Electron-based Slack app.

## Features

- Channel and DM browsing with unread counts
- Threaded conversations
- File uploads and image previews
- Emoji and @mention autocomplete
- Reactions (add, remove, view)
- Desktop notifications with click-to-navigate
- Full-text message search
- Google Meet call integration
- Presence indicators and user status
- Local message caching with SurrealDB

## Building

```
cargo build --release
```

Requires GTK4 and libadwaita development libraries.

## License

GPL-3.0-or-later

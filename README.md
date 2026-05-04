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
- Local message caching with SQLite

## Building

```
cargo build --release
```

Requires GTK4 and libadwaita development libraries.

## Getting your Slack tokens

Sludge does not use Slack's official OAuth flow. Instead, it reuses the session
you are already signed into in your browser. You need two values: an **xoxc
token** and an **xoxd cookie**. They are tied to each other — both must come
from the same browser session.

> Treat these values like a password. Anyone with both can act as you on
> Slack. Sludge stores them locally in its SQLite database; never paste them
> into a public place.

### 1. Sign in to Slack in your browser

Open <https://app.slack.com/> in Chrome, Firefox, or any Chromium-based
browser, and sign in to the workspace you want to connect to.

### 2. Open the developer tools

Press <kbd>F12</kbd> (or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>I</kbd>) to
open DevTools.

### 3. Grab the xoxc token (from a network request)

1. Switch to the **Network** tab in DevTools.
2. In the filter box, type `api`.
3. Reload the Slack tab. Many requests will appear.
4. Click any request whose name starts with `api.` — for example
   `client.boot`, `users.counts`, or `conversations.list`.
5. Open the **Payload** tab (Chrome) or **Request** tab (Firefox).
6. Look for a form field called `token`. Its value starts with `xoxc-` —
   copy the entire string. That is your **xoxc token**.

### 4. Grab the xoxd cookie (from cookies)

1. Switch to the **Application** tab (Chrome) or **Storage** tab (Firefox).
2. Expand **Cookies** and select `https://app.slack.com`.
3. Find the cookie named `d`. Its value starts with `xoxd-` (it may appear
   URL-encoded as `xoxd-...%2F...` — copy it as-is, Sludge will handle it).
4. Copy the full value. That is your **xoxd cookie**.

### 5. Sign in to Sludge

Launch Sludge and paste the two values into the login screen, along with
your workspace URL (e.g. `myteam.slack.com`). Sludge will validate the
tokens with Slack and remember them for future launches.

If a token stops working, repeat the steps above to grab fresh values —
Slack rotates session tokens periodically and when you sign out elsewhere.

## License

GPL-3.0-or-later

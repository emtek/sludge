use std::collections::HashMap;
use std::sync::RwLock;

use slacko::types::{Channel, User};

/// Global registry of custom emoji: shortcode → local cached image path.
/// Populated on startup from the Slack API, used by emoji rendering.
static CUSTOM_EMOJI: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// Global ordered list of recently used emoji shortcodes (most recent first).
static RECENT_EMOJI: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Set the custom emoji map (name → local file path for images, or "alias:name" for aliases).
pub fn set_custom_emoji(emoji: HashMap<String, String>) {
    *CUSTOM_EMOJI.write().unwrap() = Some(emoji);
}

/// Look up a custom emoji by shortcode. Returns the local cached image path if available.
pub fn get_custom_emoji_path(shortcode: &str) -> Option<String> {
    let lock = CUSTOM_EMOJI.read().unwrap();
    let map = lock.as_ref()?;
    let mut name = shortcode;
    // Resolve aliases (up to 5 levels to prevent loops)
    for _ in 0..5 {
        let val = map.get(name)?;
        if let Some(alias) = val.strip_prefix("alias:") {
            name = alias;
        } else {
            return Some(val.clone());
        }
    }
    None
}

/// Set the recent emoji list (called on startup from DB).
pub fn set_recent_emoji(emoji: Vec<String>) {
    *RECENT_EMOJI.write().unwrap() = emoji;
}

/// Get the recent emoji list (most recent first).
pub fn get_recent_emoji() -> Vec<String> {
    RECENT_EMOJI.read().unwrap().clone()
}

/// Push a shortcode to the front of the recent emoji list.
pub fn push_recent_emoji(shortcode: &str) {
    let mut list = RECENT_EMOJI.write().unwrap();
    list.retain(|s| s != shortcode);
    list.insert(0, shortcode.to_string());
    list.truncate(50);
}

/// Return all custom emoji shortcode names, or None if not yet loaded.
pub fn get_all_custom_emoji_names() -> Option<Vec<String>> {
    let lock = CUSTOM_EMOJI.read().unwrap();
    lock.as_ref().map(|map| map.keys().cloned().collect())
}

pub fn channel_display_name(channel: &Channel) -> String {
    if channel.is_im == Some(true) {
        return channel.user.clone().unwrap_or_else(|| channel.id.clone());
    }
    channel.name.clone().unwrap_or_else(|| channel.id.clone())
}

pub fn user_display_name(user: &User) -> String {
    user.profile
        .as_ref()
        .and_then(|p| p.display_name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| user.real_name.clone())
        .unwrap_or_else(|| user.name.clone())
}

/// Format a Slack message as plain text for notifications.
/// Replaces `<@UXXXX>` with @Name, `<#CXXXX|name>` with #name,
/// `<url|label>` with label, `<url>` with url, and emoji shortcodes with unicode.
pub fn format_message_plain(text: &str, user_names: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('<') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];

        if let Some(end) = after.find('>') {
            let inner = &after[..end];
            rest = &after[end + 1..];

            if let Some(user_id) = inner.strip_prefix('@') {
                let name = user_names
                    .get(user_id)
                    .cloned()
                    .unwrap_or_else(|| user_id.to_string());
                result.push('@');
                result.push_str(&name);
            } else if inner.starts_with('#') {
                if let Some((_id, name)) = inner[1..].split_once('|') {
                    result.push('#');
                    result.push_str(name);
                } else {
                    result.push('#');
                    result.push_str(&inner[1..]);
                }
            } else if let Some((_url, label)) = inner.split_once('|') {
                result.push_str(label);
            } else {
                result.push_str(inner);
            }
        } else {
            result.push('<');
            rest = after;
        }
    }

    result.push_str(rest);
    replace_emoji_shortcodes(&result)
}

/// Format a Slack message as Pango markup with emoji, clickable @mentions, links, and markdown.
/// The returned string is safe for `Label::set_markup`.
pub fn format_message_markup(text: &str, user_names: &HashMap<String, String>) -> String {
    // 1. Process Slack <...> constructs (mentions, links) — also XML-escapes text
    let with_links = replace_slack_brackets(text, user_names);
    // 2. Apply Slack markdown (bold, italic, strikethrough, code)
    let with_md = apply_slack_markdown(&with_links);
    // 3. Replace emoji shortcodes
    replace_emoji_shortcodes(&with_md)
}

/// Convert Slack markdown to Pango markup.
/// Input is already partially marked up (contains `<a>` tags from bracket processing).
/// Handles: ```code blocks```, `code`, *bold*, _italic_, ~strikethrough~, > blockquote.
fn apply_slack_markdown(text: &str) -> String {
    // First handle code blocks (``` ... ```), which suppress other formatting
    let with_blocks = replace_code_blocks(text);
    // Then handle inline code (` ... `)
    let with_inline = replace_inline_code(&with_blocks);
    // Then handle *bold*, _italic_, ~strikethrough~
    let with_formatting = replace_inline_formatting(&with_inline);
    // Handle > blockquote lines
    replace_blockquotes(&with_formatting)
}

/// Replace ``` ... ``` with `<tt>...</tt>`, preserving content verbatim.
fn replace_code_blocks(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("```") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 3..];

        if let Some(end) = after.find("```") {
            let code = &after[..end];
            // Strip leading newline if present
            let code = code.strip_prefix('\n').unwrap_or(code);
            result.push_str("<tt>");
            result.push_str(code);
            result.push_str("</tt>");
            rest = &after[end + 3..];
        } else {
            // No closing ``` — treat as literal
            result.push_str("```");
            rest = after;
        }
    }

    result.push_str(rest);
    result
}

/// Replace `code` with `<tt>code</tt>`, but skip content inside XML tags.
fn replace_inline_code(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(pos) = rest.find('`') {
        // Check if we're inside an XML tag by looking for unclosed <
        let before = &rest[..pos];
        if before.rfind('<') > before.rfind('>') {
            // Inside a tag — skip this backtick
            result.push_str(&rest[..=pos]);
            rest = &rest[pos + 1..];
            continue;
        }

        result.push_str(before);
        let after = &rest[pos + 1..];

        if let Some(end) = after.find('`') {
            let code = &after[..end];
            if !code.is_empty() && !code.contains('\n') {
                result.push_str("<tt>");
                result.push_str(code);
                result.push_str("</tt>");
                rest = &after[end + 1..];
                continue;
            }
        }
        // No closing backtick or empty — treat as literal
        result.push('`');
        rest = after;
    }

    result.push_str(rest);
    result
}

/// Replace *bold*, _italic_, ~strikethrough~ with Pango equivalents.
/// Skips content inside `<...>` XML tags and `<tt>...</tt>` code spans.
fn replace_inline_formatting(text: &str) -> String {
    let formatters: &[(&str, &str, &str)] = &[
        ("*", "<b>", "</b>"),
        ("_", "<i>", "</i>"),
        ("~", "<s>", "</s>"),
    ];

    let mut current = text.to_string();

    for &(marker, open_tag, close_tag) in formatters {
        let mut result = String::with_capacity(current.len());
        let mut rest = current.as_str();
        let marker_char = marker.as_bytes()[0];

        while let Some(start) = rest.find(marker) {
            let before = &rest[..start];

            // Skip if inside an XML tag
            if before.rfind('<') > before.rfind('>') {
                result.push_str(&rest[..=start]);
                rest = &rest[start + 1..];
                continue;
            }

            // Skip if inside a <tt> block
            let result_so_far = format!("{result}{before}");
            let tt_opens = result_so_far.matches("<tt>").count();
            let tt_closes = result_so_far.matches("</tt>").count();
            if tt_opens > tt_closes {
                result.push_str(&rest[..=start]);
                rest = &rest[start + 1..];
                continue;
            }

            // Check that marker is at a word boundary (not mid-word like a_b)
            let char_before = before.as_bytes().last().copied();
            let is_boundary_before = char_before
                .map(|c| c == b' ' || c == b'\n' || c == b'>' || c == b';')
                .unwrap_or(true);

            if !is_boundary_before && marker_char == b'_' {
                // Underscore mid-word — skip
                result.push_str(&rest[..=start]);
                rest = &rest[start + 1..];
                continue;
            }

            result.push_str(before);
            let after = &rest[start + 1..];

            if let Some(end) = after.find(marker) {
                let content = &after[..end];
                // Must be non-empty, single-line, not start/end with space
                if !content.is_empty()
                    && !content.contains('\n')
                    && !content.starts_with(' ')
                    && !content.ends_with(' ')
                {
                    result.push_str(open_tag);
                    result.push_str(content);
                    result.push_str(close_tag);
                    rest = &after[end + 1..];
                    continue;
                }
            }

            // No closing marker — literal
            result.push_str(marker);
            rest = after;
        }

        result.push_str(rest);
        current = result;
    }

    current
}

/// Convert lines starting with `&gt; ` (XML-escaped `> `) to styled blockquotes.
fn replace_blockquotes(text: &str) -> String {
    text.lines()
        .map(|line| {
            if let Some(content) = line.strip_prefix("&gt; ") {
                format!("<i>\u{2503} {content}</i>")
            } else if line == "&gt;" {
                "<i>\u{2503}</i>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Process all Slack `<...>` constructs:
/// - `<@UXXXX>` → clickable @mention
/// - `<#CXXXX|channel-name>` → #channel-name
/// - `<url>` → clickable link
/// - `<url|label>` → clickable link with label
/// Surrounding text is XML-escaped.
fn replace_slack_brackets(text: &str, user_names: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('<') {
        // Escape and append everything before the <
        result.push_str(&glib::markup_escape_text(&rest[..start]));
        let after = &rest[start + 1..];

        if let Some(end) = after.find('>') {
            let inner = &after[..end];
            rest = &after[end + 1..];

            if let Some(user_id) = inner.strip_prefix('@') {
                // User mention: <@UXXXX>
                let display = user_names
                    .get(user_id)
                    .cloned()
                    .unwrap_or_else(|| user_id.to_string());
                let escaped = glib::markup_escape_text(&display);
                result.push_str(&format!(
                    "<a href=\"mention:{user_id}\">@{escaped}</a>"
                ));
            } else if inner.starts_with('#') {
                // Channel link: <#CXXXX|name>
                if let Some((_id, name)) = inner[1..].split_once('|') {
                    let escaped = glib::markup_escape_text(name);
                    result.push_str(&format!("#{escaped}"));
                } else {
                    let escaped = glib::markup_escape_text(&inner[1..]);
                    result.push_str(&format!("#{escaped}"));
                }
            } else if inner.starts_with("http://")
                || inner.starts_with("https://")
                || inner.starts_with("mailto:")
            {
                // URL: <url> or <url|label>
                if let Some((url, label)) = inner.split_once('|') {
                    let escaped_url = glib::markup_escape_text(url);
                    let escaped_label = glib::markup_escape_text(label);
                    result.push_str(&format!(
                        "<a href=\"{escaped_url}\">{escaped_label}</a>"
                    ));
                } else {
                    let escaped = glib::markup_escape_text(inner);
                    result.push_str(&format!(
                        "<a href=\"{escaped}\">{escaped}</a>"
                    ));
                }
            } else {
                // Unknown bracket content — just show it escaped
                result.push_str(&glib::markup_escape_text(&format!("<{inner}>")));
            }
        } else {
            // No closing > — escape the < and continue
            result.push_str(&glib::markup_escape_text("<"));
            rest = after;
        }
    }

    result.push_str(&glib::markup_escape_text(rest));
    result
}

use gtk4::glib;

/// Resolve a Slack emoji shortcode to a Unicode emoji string.
/// Handles Slack-specific aliases (e.g. `large_green_circle` → `green_circle`)
/// and skin tone variants. Returns None if not a standard emoji.
pub fn resolve_slack_shortcode(code: &str) -> Option<&'static str> {
    // Direct lookup first
    if let Some(emoji) = emojis::get_by_shortcode(code) {
        return Some(emoji.as_str());
    }
    // Slack-specific aliases: try stripping common prefixes/suffixes
    // Slack uses "large_*" for many emoji
    if let Some(stripped) = code.strip_prefix("large_") {
        if let Some(emoji) = emojis::get_by_shortcode(stripped) {
            return Some(emoji.as_str());
        }
    }
    // Slack uses "small_*" for some emoji
    if let Some(stripped) = code.strip_prefix("small_") {
        if let Some(emoji) = emojis::get_by_shortcode(stripped) {
            return Some(emoji.as_str());
        }
    }
    // Slack appends _pad to some emoji (e.g. spiral_calendar_pad → spiral_calendar)
    if let Some(stripped) = code.strip_suffix("_pad") {
        if let Some(emoji) = emojis::get_by_shortcode(stripped) {
            return Some(emoji.as_str());
        }
    }
    // Slack skin tone variants: ":+1::skin-tone-2:" stored as "+1::skin-tone-2"
    if let Some(base) = code.split("::skin-tone-").next() {
        if let Some(emoji) = emojis::get_by_shortcode(base) {
            return Some(emoji.as_str());
        }
    }
    // Try removing trailing digits for keycap variants (e.g. "one" not found, try as-is)
    // Common Slack aliases not in the emojis crate
    match code {
        "slightly_smiling_face" => Some("🙂"),
        "upside_down_face" => Some("🙃"),
        "simple_smile" => Some("🙂"),
        "wfh" => Some("🏠"),
        "white_check_mark" => Some("✅"),
        "heavy_check_mark" => Some("✔️"),
        "x" => Some("❌"),
        "heavy_multiplication_x" => Some("✖️"),
        "bangbang" => Some("‼️"),
        "interrobang" => Some("⁉️"),
        "tada" => Some("🎉"),
        "party_blob" | "party-blob" => Some("🎉"),
        "blob-dance" | "blobdance" => Some("🕺"),
        "spiral_note_pad" | "spiral_notepad" => Some("🗒️"),
        "memo" | "pencil" => Some("📝"),
        "phone" | "telephone_receiver" => Some("📞"),
        "email" | "envelope" => Some("✉️"),
        "thumbsup" | "thumbup" => Some("👍"),
        "thumbsdown" | "thumbdown" => Some("👎"),
        "hankey" | "poop" | "shit" => Some("💩"),
        "hurtrealbad" => Some("🤕"),
        "rage" => Some("😡"),
        "suspect" => Some("🤨"),
        _ => None,
    }
}

/// Replace Slack-style `:shortcode:` emoji with actual Unicode emoji or custom emoji placeholders.
/// Custom emoji are replaced with U+FFFC (object replacement character) for later rendering.
pub fn replace_emoji_shortcodes(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find(':') {
        result.push_str(&rest[..start]);
        let after_colon = &rest[start + 1..];

        if let Some(end) = after_colon.find(':') {
            let code = &after_colon[..end];
            // Shortcodes are alphanumeric with underscores/hyphens, reasonable length
            if !code.is_empty()
                && code.len() <= 50
                && code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'+')
            {
                if let Some(emoji_str) = resolve_slack_shortcode(code) {
                    result.push_str(emoji_str);
                    rest = &after_colon[end + 1..];
                    continue;
                }
                // Check custom emoji — insert object replacement char for inline rendering
                if get_custom_emoji_path(code).is_some() {
                    result.push('\u{FFFC}');
                    rest = &after_colon[end + 1..];
                    continue;
                }
            }
            // Not a valid shortcode — emit the colon literally and continue
            result.push(':');
            rest = after_colon;
        } else {
            // No closing colon — emit the rest and stop
            result.push(':');
            rest = after_colon;
        }
    }

    result.push_str(rest);
    result
}

/// Extract ordered list of custom emoji shortcodes from Slack message text.
/// Returns shortcodes in the order they appear (for pairing with U+FFFC placeholders).
pub fn extract_custom_emoji(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find(':') {
        let after_colon = &rest[start + 1..];
        if let Some(end) = after_colon.find(':') {
            let code = &after_colon[..end];
            if !code.is_empty()
                && code.len() <= 50
                && code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'+')
            {
                // Skip standard Unicode emoji (including Slack aliases)
                if resolve_slack_shortcode(code).is_some() {
                    rest = &after_colon[end + 1..];
                    continue;
                }
                // Custom emoji
                if get_custom_emoji_path(code).is_some() {
                    result.push(code.to_string());
                    rest = &after_colon[end + 1..];
                    continue;
                }
            }
            // Not a shortcode — skip the colon
            rest = after_colon;
        } else {
            break;
        }
    }

    result
}

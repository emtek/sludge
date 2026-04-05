use std::collections::HashMap;

use slacko::types::{Channel, User};

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

/// Replace Slack-style `:shortcode:` emoji with actual Unicode emoji.
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
                if let Some(emoji) = emojis::get_by_shortcode(code) {
                    result.push_str(emoji.as_str());
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

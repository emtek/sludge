use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};
use slacko::types::Message as SlackoMessage;

/// Local extension of `slacko::types::Message` that preserves fields the
/// upstream type drops.
///
/// - `blocks`: needed to render Block Kit messages from bots/integrations.
/// - `username`: Slack puts the user-facing name directly on bot messages
///   (e.g. Geekbot standups) here rather than in `users.list`, so without it
///   we'd fall back to showing the raw bot ID.
///
/// `#[serde(flatten)]` keeps the wire shape identical so cached rows written
/// before these fields existed still deserialize (with the new fields `None`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageExt {
    #[serde(flatten)]
    pub inner: SlackoMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

impl MessageExt {
    pub fn from_inner(inner: SlackoMessage) -> Self {
        Self { inner, blocks: None, username: None }
    }
}

impl From<SlackoMessage> for MessageExt {
    fn from(inner: SlackoMessage) -> Self {
        Self::from_inner(inner)
    }
}

impl Deref for MessageExt {
    type Target = SlackoMessage;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for MessageExt {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

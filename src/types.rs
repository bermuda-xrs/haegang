use serde::{Deserialize, Serialize};

/// One cleaned text message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub sender: String,
    pub timestamp_ms: i64,
    pub text: String,
}

/// Text with its speakers: messages in order.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    pub participants: Vec<String>,
    pub messages: Vec<Message>,
    /// Raw entries dropped (media-only, system lines, empty, malformed, duplicates).
    pub skipped: usize,
}

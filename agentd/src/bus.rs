use std::collections::HashMap;

/// A message waiting in an agent's mailbox.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MailMessage {
    pub from:    String,
    pub content: String,
}

/// Per-agent in-memory mailboxes, keyed by recipient agent ID.
pub type Mailboxes = HashMap<String, Vec<MailMessage>>;

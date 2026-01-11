//! Core data types for the chat system.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier for a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub String);

impl MessageId {
    /// Create a new random message ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Get the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for MessageId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for MessageId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Status of a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    /// Message is queued locally, peer is offline.
    #[default]
    Pending,
    /// Message has been sent, awaiting delivery confirmation.
    Sent,
    /// Peer has received the message.
    Delivered,
    /// Peer has read the message.
    Read,
    /// Failed to send after retries.
    Failed,
}

impl MessageStatus {
    /// Get a human-readable string for this status.
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageStatus::Pending => "pending",
            MessageStatus::Sent => "sent",
            MessageStatus::Delivered => "delivered",
            MessageStatus::Read => "read",
            MessageStatus::Failed => "failed",
        }
    }
}

/// A single chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Unique message identifier.
    pub id: MessageId,
    /// Sender's endpoint ID.
    pub sender_id: String,
    /// Recipient's endpoint ID.
    pub recipient_id: String,
    /// Message content (plain text, max 10KB).
    pub content: String,
    /// When the message was created/sent (UTC).
    pub sent_at: DateTime<Utc>,
    /// When the message was delivered to recipient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
    /// When the message was read by recipient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,
    /// Current status of the message.
    #[serde(default)]
    pub status: MessageStatus,
    /// Sequence number for ordering within a conversation.
    /// Each peer maintains their own sequence counter.
    pub sequence: u64,
}

impl ChatMessage {
    /// Maximum allowed content length (10KB).
    pub const MAX_CONTENT_LENGTH: usize = 10 * 1024;

    /// Create a new outgoing message.
    pub fn new_outgoing(
        sender_id: String,
        recipient_id: String,
        content: String,
        sequence: u64,
    ) -> Self {
        Self {
            id: MessageId::new(),
            sender_id,
            recipient_id,
            content,
            sent_at: Utc::now(),
            delivered_at: None,
            read_at: None,
            status: MessageStatus::Pending,
            sequence,
        }
    }

    /// Create a message from an incoming protocol message.
    pub fn from_incoming(
        sender_id: String,
        recipient_id: String,
        message_id: String,
        content: String,
        sequence: u64,
        sent_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: MessageId(message_id),
            sender_id,
            recipient_id,
            content,
            sent_at,
            delivered_at: Some(Utc::now()),
            read_at: None,
            status: MessageStatus::Delivered,
            sequence,
        }
    }

    /// Check if this message was sent by us (vs received).
    pub fn is_outgoing(&self, our_endpoint_id: &str) -> bool {
        self.sender_id == our_endpoint_id
    }

    /// Mark the message as sent.
    pub fn mark_sent(&mut self) {
        if self.status == MessageStatus::Pending {
            self.status = MessageStatus::Sent;
        }
    }

    /// Mark the message as delivered.
    pub fn mark_delivered(&mut self) {
        if matches!(self.status, MessageStatus::Pending | MessageStatus::Sent) {
            self.status = MessageStatus::Delivered;
            self.delivered_at = Some(Utc::now());
        }
    }

    /// Mark the message as read.
    pub fn mark_read(&mut self) {
        if !matches!(self.status, MessageStatus::Read | MessageStatus::Failed) {
            self.status = MessageStatus::Read;
            self.read_at = Some(Utc::now());
            if self.delivered_at.is_none() {
                self.delivered_at = self.read_at;
            }
        }
    }

    /// Mark the message as failed.
    pub fn mark_failed(&mut self) {
        self.status = MessageStatus::Failed;
    }

    /// Get the peer ID for this message (the other party in the conversation).
    pub fn peer_id(&self, our_endpoint_id: &str) -> &str {
        if self.sender_id == our_endpoint_id {
            &self.recipient_id
        } else {
            &self.sender_id
        }
    }
}

/// Summary of a conversation with a peer.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatConversation {
    /// Peer's endpoint ID (conversation key).
    pub peer_id: String,
    /// Peer's display name (cached from TrustedPeer).
    pub peer_name: String,
    /// Preview of the last message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_preview: Option<String>,
    /// Timestamp of the last message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<DateTime<Utc>>,
    /// Number of unread messages.
    #[serde(default)]
    pub unread_count: u32,
    /// Highest sequence number we've sent.
    #[serde(default)]
    pub last_sent_sequence: u64,
    /// Highest sequence number we've received from this peer.
    #[serde(default)]
    pub last_received_sequence: u64,
    /// Whether the peer is currently typing.
    #[serde(default)]
    pub peer_is_typing: bool,
    /// When the typing indicator expires (for timeout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typing_expires_at: Option<DateTime<Utc>>,
}

impl ChatConversation {
    /// Create a new conversation for a peer.
    pub fn new(peer_id: String, peer_name: String) -> Self {
        Self {
            peer_id,
            peer_name,
            last_message_preview: None,
            last_message_at: None,
            unread_count: 0,
            last_sent_sequence: 0,
            last_received_sequence: 0,
            peer_is_typing: false,
            typing_expires_at: None,
        }
    }

    /// Get the next sequence number for sending a message.
    pub fn next_send_sequence(&mut self) -> u64 {
        self.last_sent_sequence += 1;
        self.last_sent_sequence
    }

    /// Update the conversation after sending a message.
    pub fn on_message_sent(&mut self, message: &ChatMessage) {
        self.last_message_preview = Some(truncate_preview(&message.content));
        self.last_message_at = Some(message.sent_at);
    }

    /// Update the conversation after receiving a message.
    pub fn on_message_received(&mut self, message: &ChatMessage) {
        self.last_message_preview = Some(truncate_preview(&message.content));
        self.last_message_at = Some(message.sent_at);
        self.unread_count += 1;
        if message.sequence > self.last_received_sequence {
            self.last_received_sequence = message.sequence;
        }
    }

    /// Mark all messages as read.
    pub fn mark_all_read(&mut self) {
        self.unread_count = 0;
    }

    /// Set the typing indicator state.
    pub fn set_typing(&mut self, is_typing: bool) {
        self.peer_is_typing = is_typing;
        if is_typing {
            // Typing indicator expires after 5 seconds
            self.typing_expires_at = Some(Utc::now() + chrono::Duration::seconds(5));
        } else {
            self.typing_expires_at = None;
        }
    }

    /// Check and clear expired typing indicator.
    pub fn check_typing_expired(&mut self) -> bool {
        if self.peer_is_typing {
            if let Some(expires) = self.typing_expires_at {
                if Utc::now() > expires {
                    self.peer_is_typing = false;
                    self.typing_expires_at = None;
                    return true;
                }
            }
        }
        false
    }
}

/// Sync state for message history synchronization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncState {
    /// Peer's endpoint ID.
    pub peer_id: String,
    /// Last sequence number we received from this peer.
    pub last_received_sequence: u64,
    /// Last sequence number the peer acknowledged from us.
    pub last_ack_sequence: u64,
    /// When we last synced with this peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,
}

impl SyncState {
    /// Create a new sync state for a peer.
    pub fn new(peer_id: String) -> Self {
        Self {
            peer_id,
            last_received_sequence: 0,
            last_ack_sequence: 0,
            last_sync_at: None,
        }
    }

    /// Update after receiving messages.
    pub fn on_received(&mut self, max_sequence: u64) {
        if max_sequence > self.last_received_sequence {
            self.last_received_sequence = max_sequence;
        }
    }

    /// Update after sync completion.
    pub fn on_sync_complete(&mut self) {
        self.last_sync_at = Some(Utc::now());
    }
}

/// Message data for sync responses (wire format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSyncMessage {
    /// Message ID.
    pub message_id: String,
    /// Sender's endpoint ID.
    pub sender_id: String,
    /// Message content.
    pub content: String,
    /// Sequence number.
    pub sequence: u64,
    /// When sent (Unix timestamp millis).
    pub sent_at: i64,
    /// When delivered (Unix timestamp millis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<i64>,
    /// When read (Unix timestamp millis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<i64>,
}

impl ChatSyncMessage {
    /// Convert from a ChatMessage.
    pub fn from_message(msg: &ChatMessage) -> Self {
        Self {
            message_id: msg.id.0.clone(),
            sender_id: msg.sender_id.clone(),
            content: msg.content.clone(),
            sequence: msg.sequence,
            sent_at: msg.sent_at.timestamp_millis(),
            delivered_at: msg.delivered_at.map(|t| t.timestamp_millis()),
            read_at: msg.read_at.map(|t| t.timestamp_millis()),
        }
    }

    /// Convert to a ChatMessage.
    pub fn to_message(&self, recipient_id: String) -> ChatMessage {
        use chrono::TimeZone;
        ChatMessage {
            id: MessageId(self.message_id.clone()),
            sender_id: self.sender_id.clone(),
            recipient_id,
            content: self.content.clone(),
            sent_at: Utc.timestamp_millis_opt(self.sent_at).unwrap(),
            delivered_at: self.delivered_at.map(|t| Utc.timestamp_millis_opt(t).unwrap()),
            read_at: self.read_at.map(|t| Utc.timestamp_millis_opt(t).unwrap()),
            status: if self.read_at.is_some() {
                MessageStatus::Read
            } else if self.delivered_at.is_some() {
                MessageStatus::Delivered
            } else {
                MessageStatus::Sent
            },
            sequence: self.sequence,
        }
    }
}

/// Truncate content for preview display.
fn truncate_preview(content: &str) -> String {
    const MAX_PREVIEW_LEN: usize = 50;
    if content.len() <= MAX_PREVIEW_LEN {
        content.to_string()
    } else {
        let mut preview: String = content.chars().take(MAX_PREVIEW_LEN - 3).collect();
        preview.push_str("...");
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_id_generation() {
        let id1 = MessageId::new();
        let id2 = MessageId::new();
        assert_ne!(id1, id2);
        assert!(!id1.as_str().is_empty());
    }

    #[test]
    fn test_message_status_progression() {
        let mut msg = ChatMessage::new_outgoing(
            "sender".to_string(),
            "recipient".to_string(),
            "Hello".to_string(),
            1,
        );

        assert_eq!(msg.status, MessageStatus::Pending);

        msg.mark_sent();
        assert_eq!(msg.status, MessageStatus::Sent);

        msg.mark_delivered();
        assert_eq!(msg.status, MessageStatus::Delivered);
        assert!(msg.delivered_at.is_some());

        msg.mark_read();
        assert_eq!(msg.status, MessageStatus::Read);
        assert!(msg.read_at.is_some());
    }

    #[test]
    fn test_message_is_outgoing() {
        let msg = ChatMessage::new_outgoing(
            "alice".to_string(),
            "bob".to_string(),
            "Hi".to_string(),
            1,
        );

        assert!(msg.is_outgoing("alice"));
        assert!(!msg.is_outgoing("bob"));
    }

    #[test]
    fn test_conversation_sequence() {
        let mut conv = ChatConversation::new("peer123".to_string(), "Test Peer".to_string());

        assert_eq!(conv.next_send_sequence(), 1);
        assert_eq!(conv.next_send_sequence(), 2);
        assert_eq!(conv.next_send_sequence(), 3);
    }

    #[test]
    fn test_conversation_unread() {
        let mut conv = ChatConversation::new("peer123".to_string(), "Test Peer".to_string());

        let msg = ChatMessage::from_incoming(
            "peer123".to_string(),
            "us".to_string(),
            "msg1".to_string(),
            "Hello".to_string(),
            1,
            Utc::now(),
        );

        conv.on_message_received(&msg);
        assert_eq!(conv.unread_count, 1);

        conv.on_message_received(&msg);
        assert_eq!(conv.unread_count, 2);

        conv.mark_all_read();
        assert_eq!(conv.unread_count, 0);
    }

    #[test]
    fn test_truncate_preview() {
        assert_eq!(truncate_preview("Short"), "Short");
        assert_eq!(
            truncate_preview("This is a really long message that should be truncated for preview"),
            "This is a really long message that should be tr..."
        );
    }

    #[test]
    fn test_sync_message_roundtrip() {
        let original = ChatMessage::new_outgoing(
            "alice".to_string(),
            "bob".to_string(),
            "Test message".to_string(),
            42,
        );

        let sync_msg = ChatSyncMessage::from_message(&original);
        let restored = sync_msg.to_message("bob".to_string());

        assert_eq!(original.id, restored.id);
        assert_eq!(original.sender_id, restored.sender_id);
        assert_eq!(original.content, restored.content);
        assert_eq!(original.sequence, restored.sequence);
    }
}

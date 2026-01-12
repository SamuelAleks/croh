//! Persistent chat storage using Sled embedded database.

use crate::chat::types::{ChatConversation, ChatMessage, MessageStatus, SyncState};
use crate::error::{Error, Result};
use crate::platform;
use sled::{Db, Tree};
use std::sync::Arc;

/// Sled-based persistent chat storage.
///
/// Database structure:
/// - `conversations`: peer_id -> ChatConversation (JSON)
/// - `messages`: peer_id:sequence -> ChatMessage (JSON)
/// - `pending`: peer_id:message_id -> ChatMessage (JSON) - offline queue
/// - `message_index`: message_id -> peer_id:sequence - for lookups by ID
/// - `sync_states`: peer_id -> SyncState (JSON)
pub struct ChatStore {
    #[allow(dead_code)]
    db: Db,
    /// Tree: peer_id -> ChatConversation (serialized)
    conversations: Tree,
    /// Tree: peer_id:sequence -> ChatMessage (serialized)
    messages: Tree,
    /// Tree: peer_id:message_id -> ChatMessage (offline queue)
    pending: Tree,
    /// Tree: message_id -> peer_id:sequence (index for lookups)
    message_index: Tree,
    /// Tree: peer_id -> SyncState (serialized)
    sync_states: Tree,
}

impl ChatStore {
    /// Open or create the chat database.
    pub fn open() -> Result<Self> {
        let db_path = platform::data_dir().join("chat.db");

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let db = sled::open(&db_path)
            .map_err(|e| Error::Chat(format!("failed to open chat database: {}", e)))?;

        Ok(Self {
            conversations: db
                .open_tree("conversations")
                .map_err(|e| Error::Chat(e.to_string()))?,
            messages: db
                .open_tree("messages")
                .map_err(|e| Error::Chat(e.to_string()))?,
            pending: db
                .open_tree("pending")
                .map_err(|e| Error::Chat(e.to_string()))?,
            message_index: db
                .open_tree("message_index")
                .map_err(|e| Error::Chat(e.to_string()))?,
            sync_states: db
                .open_tree("sync_states")
                .map_err(|e| Error::Chat(e.to_string()))?,
            db,
        })
    }

    /// Open an in-memory database for testing.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let config = sled::Config::new().temporary(true);
        let db = config
            .open()
            .map_err(|e| Error::Chat(format!("failed to open in-memory database: {}", e)))?;

        Ok(Self {
            conversations: db
                .open_tree("conversations")
                .map_err(|e| Error::Chat(e.to_string()))?,
            messages: db
                .open_tree("messages")
                .map_err(|e| Error::Chat(e.to_string()))?,
            pending: db
                .open_tree("pending")
                .map_err(|e| Error::Chat(e.to_string()))?,
            message_index: db
                .open_tree("message_index")
                .map_err(|e| Error::Chat(e.to_string()))?,
            sync_states: db
                .open_tree("sync_states")
                .map_err(|e| Error::Chat(e.to_string()))?,
            db,
        })
    }

    // ==================== Conversation Methods ====================

    /// Get a conversation by peer ID.
    pub fn get_conversation(&self, peer_id: &str) -> Result<Option<ChatConversation>> {
        match self.conversations.get(peer_id.as_bytes()) {
            Ok(Some(data)) => {
                let conv: ChatConversation = serde_json::from_slice(&data).map_err(|e| {
                    Error::Chat(format!("failed to deserialize conversation: {}", e))
                })?;
                Ok(Some(conv))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::Chat(format!("failed to get conversation: {}", e))),
        }
    }

    /// Get or create a conversation for a peer.
    pub fn get_or_create_conversation(
        &self,
        peer_id: &str,
        peer_name: &str,
    ) -> Result<ChatConversation> {
        match self.get_conversation(peer_id)? {
            Some(conv) => Ok(conv),
            None => {
                let conv = ChatConversation::new(peer_id.to_string(), peer_name.to_string());
                self.upsert_conversation(&conv)?;
                Ok(conv)
            }
        }
    }

    /// Insert or update a conversation.
    pub fn upsert_conversation(&self, conversation: &ChatConversation) -> Result<()> {
        let data = serde_json::to_vec(conversation)
            .map_err(|e| Error::Chat(format!("failed to serialize conversation: {}", e)))?;
        self.conversations
            .insert(conversation.peer_id.as_bytes(), data)
            .map_err(|e| Error::Chat(format!("failed to insert conversation: {}", e)))?;
        Ok(())
    }

    /// List all conversations, sorted by last message time (most recent first).
    pub fn list_conversations(&self) -> Result<Vec<ChatConversation>> {
        let mut conversations = Vec::new();
        for item in self.conversations.iter() {
            let (_, data) =
                item.map_err(|e| Error::Chat(format!("failed to iterate conversations: {}", e)))?;
            let conv: ChatConversation = serde_json::from_slice(&data)
                .map_err(|e| Error::Chat(format!("failed to deserialize conversation: {}", e)))?;
            conversations.push(conv);
        }

        // Sort by last message time, most recent first
        conversations.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));

        Ok(conversations)
    }

    /// Delete a conversation and all its messages.
    pub fn delete_conversation(&self, peer_id: &str) -> Result<()> {
        // Delete the conversation
        self.conversations
            .remove(peer_id.as_bytes())
            .map_err(|e| Error::Chat(format!("failed to delete conversation: {}", e)))?;

        // Delete all messages for this peer
        let prefix = format!("{}:", peer_id);
        let mut to_delete = Vec::new();
        for item in self.messages.scan_prefix(prefix.as_bytes()) {
            let (key, _) =
                item.map_err(|e| Error::Chat(format!("failed to scan messages: {}", e)))?;
            to_delete.push(key);
        }
        for key in to_delete {
            self.messages
                .remove(&key)
                .map_err(|e| Error::Chat(format!("failed to delete message: {}", e)))?;
        }

        // Delete pending messages
        let mut to_delete = Vec::new();
        for item in self.pending.scan_prefix(prefix.as_bytes()) {
            let (key, _) =
                item.map_err(|e| Error::Chat(format!("failed to scan pending: {}", e)))?;
            to_delete.push(key);
        }
        for key in to_delete {
            self.pending
                .remove(&key)
                .map_err(|e| Error::Chat(format!("failed to delete pending: {}", e)))?;
        }

        // Delete sync state
        self.sync_states
            .remove(peer_id.as_bytes())
            .map_err(|e| Error::Chat(format!("failed to delete sync state: {}", e)))?;

        Ok(())
    }

    // ==================== Message Methods ====================

    /// Store a message.
    pub fn store_message(&self, msg: &ChatMessage, our_endpoint_id: &str) -> Result<()> {
        // Determine the peer ID (the other party)
        let peer_id = msg.peer_id(our_endpoint_id);

        // Create the key: peer_id:sequence (padded for proper sorting)
        let key = format!("{}:{:020}", peer_id, msg.sequence);

        let data = serde_json::to_vec(msg)
            .map_err(|e| Error::Chat(format!("failed to serialize message: {}", e)))?;

        // Store the message
        self.messages
            .insert(key.as_bytes(), data)
            .map_err(|e| Error::Chat(format!("failed to insert message: {}", e)))?;

        // Create the index entry: message_id -> peer_id:sequence
        self.message_index
            .insert(msg.id.as_str().as_bytes(), key.as_bytes())
            .map_err(|e| Error::Chat(format!("failed to insert message index: {}", e)))?;

        Ok(())
    }

    /// Get a message by ID.
    pub fn get_message(&self, message_id: &str) -> Result<Option<ChatMessage>> {
        // Look up the key in the index
        let key = match self.message_index.get(message_id.as_bytes()) {
            Ok(Some(key)) => key,
            Ok(None) => return Ok(None),
            Err(e) => return Err(Error::Chat(format!("failed to get message index: {}", e))),
        };

        // Get the message
        match self.messages.get(&key) {
            Ok(Some(data)) => {
                let msg: ChatMessage = serde_json::from_slice(&data)
                    .map_err(|e| Error::Chat(format!("failed to deserialize message: {}", e)))?;
                Ok(Some(msg))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::Chat(format!("failed to get message: {}", e))),
        }
    }

    /// Get messages for a peer, with pagination.
    ///
    /// Returns messages in reverse chronological order (newest first).
    /// If `before_seq` is provided, only returns messages with sequence < before_seq.
    pub fn get_messages(
        &self,
        peer_id: &str,
        limit: usize,
        before_seq: Option<u64>,
    ) -> Result<Vec<ChatMessage>> {
        let prefix = format!("{}:", peer_id);
        let mut messages = Vec::new();

        // Collect all messages for this peer
        for item in self.messages.scan_prefix(prefix.as_bytes()) {
            let (_, data) =
                item.map_err(|e| Error::Chat(format!("failed to scan messages: {}", e)))?;
            let msg: ChatMessage = serde_json::from_slice(&data)
                .map_err(|e| Error::Chat(format!("failed to deserialize message: {}", e)))?;

            // Apply before_seq filter
            if let Some(before) = before_seq {
                if msg.sequence >= before {
                    continue;
                }
            }

            messages.push(msg);
        }

        // Sort by sequence descending (newest first)
        messages.sort_by(|a, b| b.sequence.cmp(&a.sequence));

        // Apply limit
        messages.truncate(limit);

        Ok(messages)
    }

    /// Get messages for a peer after a specific sequence (for sync).
    pub fn get_messages_after_sequence(
        &self,
        peer_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let prefix = format!("{}:", peer_id);
        let mut messages = Vec::new();

        for item in self.messages.scan_prefix(prefix.as_bytes()) {
            let (_, data) =
                item.map_err(|e| Error::Chat(format!("failed to scan messages: {}", e)))?;
            let msg: ChatMessage = serde_json::from_slice(&data)
                .map_err(|e| Error::Chat(format!("failed to deserialize message: {}", e)))?;

            if msg.sequence > after_seq {
                messages.push(msg);
            }
        }

        // Sort by sequence ascending (oldest first for sync)
        messages.sort_by(|a, b| a.sequence.cmp(&b.sequence));

        // Apply limit
        messages.truncate(limit);

        Ok(messages)
    }

    /// Update a message's status.
    pub fn update_message_status(&self, message_id: &str, status: MessageStatus) -> Result<bool> {
        // Look up the key in the index
        let key = match self.message_index.get(message_id.as_bytes()) {
            Ok(Some(key)) => key,
            Ok(None) => return Ok(false),
            Err(e) => return Err(Error::Chat(format!("failed to get message index: {}", e))),
        };

        // Get and update the message
        match self.messages.get(&key) {
            Ok(Some(data)) => {
                let mut msg: ChatMessage = serde_json::from_slice(&data)
                    .map_err(|e| Error::Chat(format!("failed to deserialize message: {}", e)))?;

                match status {
                    MessageStatus::Sent => msg.mark_sent(),
                    MessageStatus::Delivered => msg.mark_delivered(),
                    MessageStatus::Read => msg.mark_read(),
                    MessageStatus::Failed => msg.mark_failed(),
                    MessageStatus::Pending => {} // No-op
                }

                let data = serde_json::to_vec(&msg)
                    .map_err(|e| Error::Chat(format!("failed to serialize message: {}", e)))?;
                self.messages
                    .insert(&key, data)
                    .map_err(|e| Error::Chat(format!("failed to update message: {}", e)))?;

                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(Error::Chat(format!("failed to get message: {}", e))),
        }
    }

    /// Mark multiple messages as delivered.
    pub fn mark_delivered(&self, message_ids: &[String]) -> Result<usize> {
        let mut count = 0;
        for id in message_ids {
            if self.update_message_status(id, MessageStatus::Delivered)? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Mark multiple messages as read.
    pub fn mark_read(&self, message_ids: &[String]) -> Result<usize> {
        let mut count = 0;
        for id in message_ids {
            if self.update_message_status(id, MessageStatus::Read)? {
                count += 1;
            }
        }
        Ok(count)
    }

    // ==================== Pending Queue Methods ====================

    /// Add a message to the pending queue (for offline peers).
    pub fn queue_pending(&self, peer_id: &str, msg: &ChatMessage) -> Result<()> {
        let key = format!("{}:{}", peer_id, msg.id.as_str());
        let data = serde_json::to_vec(msg)
            .map_err(|e| Error::Chat(format!("failed to serialize pending message: {}", e)))?;
        self.pending
            .insert(key.as_bytes(), data)
            .map_err(|e| Error::Chat(format!("failed to queue pending message: {}", e)))?;
        Ok(())
    }

    /// Get all pending messages for a peer.
    pub fn get_pending(&self, peer_id: &str) -> Result<Vec<ChatMessage>> {
        let prefix = format!("{}:", peer_id);
        let mut messages = Vec::new();

        for item in self.pending.scan_prefix(prefix.as_bytes()) {
            let (_, data) =
                item.map_err(|e| Error::Chat(format!("failed to scan pending: {}", e)))?;
            let msg: ChatMessage = serde_json::from_slice(&data).map_err(|e| {
                Error::Chat(format!("failed to deserialize pending message: {}", e))
            })?;
            messages.push(msg);
        }

        // Sort by sequence for sending order
        messages.sort_by(|a, b| a.sequence.cmp(&b.sequence));

        Ok(messages)
    }

    /// Remove messages from the pending queue.
    pub fn clear_pending(&self, peer_id: &str, message_ids: &[String]) -> Result<usize> {
        let mut count = 0;
        for id in message_ids {
            let key = format!("{}:{}", peer_id, id);
            if self
                .pending
                .remove(key.as_bytes())
                .map_err(|e| Error::Chat(e.to_string()))?
                .is_some()
            {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Clear all pending messages for a peer.
    pub fn clear_all_pending(&self, peer_id: &str) -> Result<usize> {
        let prefix = format!("{}:", peer_id);
        let mut count = 0;
        let mut to_delete = Vec::new();

        for item in self.pending.scan_prefix(prefix.as_bytes()) {
            let (key, _) =
                item.map_err(|e| Error::Chat(format!("failed to scan pending: {}", e)))?;
            to_delete.push(key);
        }

        for key in to_delete {
            self.pending
                .remove(&key)
                .map_err(|e| Error::Chat(e.to_string()))?;
            count += 1;
        }

        Ok(count)
    }

    // ==================== Sync State Methods ====================

    /// Get the sync state for a peer.
    pub fn get_sync_state(&self, peer_id: &str) -> Result<Option<SyncState>> {
        match self.sync_states.get(peer_id.as_bytes()) {
            Ok(Some(data)) => {
                let state: SyncState = serde_json::from_slice(&data)
                    .map_err(|e| Error::Chat(format!("failed to deserialize sync state: {}", e)))?;
                Ok(Some(state))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::Chat(format!("failed to get sync state: {}", e))),
        }
    }

    /// Update the sync state for a peer.
    pub fn update_sync_state(&self, state: &SyncState) -> Result<()> {
        let data = serde_json::to_vec(state)
            .map_err(|e| Error::Chat(format!("failed to serialize sync state: {}", e)))?;
        self.sync_states
            .insert(state.peer_id.as_bytes(), data)
            .map_err(|e| Error::Chat(format!("failed to update sync state: {}", e)))?;
        Ok(())
    }

    // ==================== Stats Methods ====================

    /// Get the unread count for a specific peer.
    pub fn get_unread_count(&self, peer_id: &str) -> Result<u32> {
        match self.get_conversation(peer_id)? {
            Some(conv) => Ok(conv.unread_count),
            None => Ok(0),
        }
    }

    /// Get the total unread count across all conversations.
    pub fn get_total_unread(&self) -> Result<u32> {
        let mut total = 0;
        for item in self.conversations.iter() {
            let (_, data) =
                item.map_err(|e| Error::Chat(format!("failed to iterate conversations: {}", e)))?;
            let conv: ChatConversation = serde_json::from_slice(&data)
                .map_err(|e| Error::Chat(format!("failed to deserialize conversation: {}", e)))?;
            total += conv.unread_count;
        }
        Ok(total)
    }

    /// Flush the database to disk.
    pub fn flush(&self) -> Result<()> {
        self.conversations
            .flush()
            .map_err(|e| Error::Chat(e.to_string()))?;
        self.messages
            .flush()
            .map_err(|e| Error::Chat(e.to_string()))?;
        self.pending
            .flush()
            .map_err(|e| Error::Chat(e.to_string()))?;
        self.message_index
            .flush()
            .map_err(|e| Error::Chat(e.to_string()))?;
        self.sync_states
            .flush()
            .map_err(|e| Error::Chat(e.to_string()))?;
        Ok(())
    }
}

// Make ChatStore thread-safe
unsafe impl Send for ChatStore {}
unsafe impl Sync for ChatStore {}

/// Convenience type for shared chat store.
pub type SharedChatStore = Arc<ChatStore>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::types::MessageId;
    use chrono::Utc;

    fn create_test_message(sender: &str, recipient: &str, seq: u64) -> ChatMessage {
        ChatMessage {
            id: MessageId::new(),
            sender_id: sender.to_string(),
            recipient_id: recipient.to_string(),
            content: format!("Test message {}", seq),
            sent_at: Utc::now(),
            delivered_at: None,
            read_at: None,
            status: MessageStatus::Pending,
            sequence: seq,
        }
    }

    #[test]
    fn test_conversation_crud() {
        let store = ChatStore::open_in_memory().unwrap();

        // Initially no conversation
        assert!(store.get_conversation("peer1").unwrap().is_none());

        // Create conversation
        let conv = ChatConversation::new("peer1".to_string(), "Peer One".to_string());
        store.upsert_conversation(&conv).unwrap();

        // Retrieve it
        let loaded = store.get_conversation("peer1").unwrap().unwrap();
        assert_eq!(loaded.peer_id, "peer1");
        assert_eq!(loaded.peer_name, "Peer One");

        // List conversations
        let all = store.list_conversations().unwrap();
        assert_eq!(all.len(), 1);

        // Delete conversation
        store.delete_conversation("peer1").unwrap();
        assert!(store.get_conversation("peer1").unwrap().is_none());
    }

    #[test]
    fn test_message_storage() {
        let store = ChatStore::open_in_memory().unwrap();
        let our_id = "us";

        // Store some messages
        let msg1 = create_test_message(our_id, "peer1", 1);
        let msg2 = create_test_message(our_id, "peer1", 2);
        let msg3 = create_test_message("peer1", our_id, 3);

        store.store_message(&msg1, our_id).unwrap();
        store.store_message(&msg2, our_id).unwrap();
        store.store_message(&msg3, our_id).unwrap();

        // Retrieve by ID
        let loaded = store.get_message(msg1.id.as_str()).unwrap().unwrap();
        assert_eq!(loaded.id, msg1.id);

        // Get messages for peer (newest first)
        let messages = store.get_messages("peer1", 10, None).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].sequence, 3); // Newest first
        assert_eq!(messages[2].sequence, 1);

        // Get with pagination
        let messages = store.get_messages("peer1", 10, Some(3)).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].sequence, 2);
    }

    #[test]
    fn test_message_status_updates() {
        let store = ChatStore::open_in_memory().unwrap();
        let our_id = "us";

        let msg = create_test_message(our_id, "peer1", 1);
        store.store_message(&msg, our_id).unwrap();

        // Update to sent
        store
            .update_message_status(msg.id.as_str(), MessageStatus::Sent)
            .unwrap();
        let loaded = store.get_message(msg.id.as_str()).unwrap().unwrap();
        assert_eq!(loaded.status, MessageStatus::Sent);

        // Update to delivered
        store
            .update_message_status(msg.id.as_str(), MessageStatus::Delivered)
            .unwrap();
        let loaded = store.get_message(msg.id.as_str()).unwrap().unwrap();
        assert_eq!(loaded.status, MessageStatus::Delivered);
        assert!(loaded.delivered_at.is_some());

        // Update to read
        store
            .update_message_status(msg.id.as_str(), MessageStatus::Read)
            .unwrap();
        let loaded = store.get_message(msg.id.as_str()).unwrap().unwrap();
        assert_eq!(loaded.status, MessageStatus::Read);
        assert!(loaded.read_at.is_some());
    }

    #[test]
    fn test_pending_queue() {
        let store = ChatStore::open_in_memory().unwrap();

        let msg1 = create_test_message("us", "peer1", 1);
        let msg2 = create_test_message("us", "peer1", 2);

        // Queue messages
        store.queue_pending("peer1", &msg1).unwrap();
        store.queue_pending("peer1", &msg2).unwrap();

        // Get pending
        let pending = store.get_pending("peer1").unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].sequence, 1); // Ordered by sequence

        // Clear specific messages
        store
            .clear_pending("peer1", std::slice::from_ref(&msg1.id.0))
            .unwrap();
        let pending = store.get_pending("peer1").unwrap();
        assert_eq!(pending.len(), 1);

        // Clear all
        store.clear_all_pending("peer1").unwrap();
        let pending = store.get_pending("peer1").unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn test_sync_state() {
        let store = ChatStore::open_in_memory().unwrap();

        // Initially no state
        assert!(store.get_sync_state("peer1").unwrap().is_none());

        // Create state
        let state = SyncState::new("peer1".to_string());
        store.update_sync_state(&state).unwrap();

        // Retrieve it
        let loaded = store.get_sync_state("peer1").unwrap().unwrap();
        assert_eq!(loaded.peer_id, "peer1");
    }

    #[test]
    fn test_unread_counts() {
        let store = ChatStore::open_in_memory().unwrap();

        // Create conversations with unread counts
        let mut conv1 = ChatConversation::new("peer1".to_string(), "Peer One".to_string());
        conv1.unread_count = 5;
        store.upsert_conversation(&conv1).unwrap();

        let mut conv2 = ChatConversation::new("peer2".to_string(), "Peer Two".to_string());
        conv2.unread_count = 3;
        store.upsert_conversation(&conv2).unwrap();

        // Check individual counts
        assert_eq!(store.get_unread_count("peer1").unwrap(), 5);
        assert_eq!(store.get_unread_count("peer2").unwrap(), 3);
        assert_eq!(store.get_unread_count("peer3").unwrap(), 0);

        // Check total
        assert_eq!(store.get_total_unread().unwrap(), 8);
    }
}

//! Chat message handler for sending and receiving chat messages.

use crate::chat::store::ChatStore;
use crate::chat::types::{ChatConversation, ChatMessage, ChatSyncMessage, MessageStatus};
use crate::error::Result;
use crate::iroh::protocol::ControlMessage;
use crate::iroh::ControlConnection;
use chrono::{TimeZone, Utc};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Events emitted by chat operations for UI notification.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// New message received from a peer.
    MessageReceived {
        peer_id: String,
        message: ChatMessage,
    },
    /// Message sent successfully.
    MessageSent {
        peer_id: String,
        message: ChatMessage,
    },
    /// Message delivery confirmed by peer.
    MessageDelivered {
        message_id: String,
    },
    /// Message read by peer.
    MessageRead {
        message_id: String,
    },
    /// Message failed to send.
    MessageFailed {
        message_id: String,
        error: String,
    },
    /// Peer typing state changed.
    TypingChanged {
        peer_id: String,
        is_typing: bool,
    },
    /// Conversation was updated (new message, unread count, etc.).
    ConversationUpdated {
        conversation: ChatConversation,
    },
    /// History sync completed.
    SyncCompleted {
        peer_id: String,
        count: usize,
    },
    /// Pending messages were flushed after peer came online.
    PendingFlushed {
        peer_id: String,
        count: usize,
    },
}

/// Handler for chat protocol operations.
pub struct ChatHandler {
    store: Arc<ChatStore>,
    our_endpoint_id: String,
}

impl ChatHandler {
    /// Create a new chat handler.
    pub fn new(store: Arc<ChatStore>, our_endpoint_id: String) -> Self {
        Self {
            store,
            our_endpoint_id,
        }
    }

    /// Get the chat store.
    pub fn store(&self) -> &Arc<ChatStore> {
        &self.store
    }

    /// Get our endpoint ID.
    pub fn our_endpoint_id(&self) -> &str {
        &self.our_endpoint_id
    }

    // ==================== Sending Methods ====================

    /// Send a chat message to a peer.
    ///
    /// If the connection is available, sends immediately.
    /// Otherwise, queues the message for later delivery.
    pub async fn send_message(
        &self,
        conn: Option<&mut ControlConnection>,
        peer_id: &str,
        peer_name: &str,
        content: String,
    ) -> Result<(ChatMessage, Vec<ChatEvent>)> {
        // Validate content
        if content.is_empty() {
            return Err(crate::error::Error::Chat("message content cannot be empty".to_string()));
        }
        if content.len() > ChatMessage::MAX_CONTENT_LENGTH {
            return Err(crate::error::Error::Chat(format!(
                "message content exceeds maximum length of {} bytes",
                ChatMessage::MAX_CONTENT_LENGTH
            )));
        }

        let mut events = Vec::new();

        // Get or create conversation and get next sequence number
        let mut conversation = self.store.get_or_create_conversation(peer_id, peer_name)?;
        let sequence = conversation.next_send_sequence();

        // Create the message
        let mut message = ChatMessage::new_outgoing(
            self.our_endpoint_id.clone(),
            peer_id.to_string(),
            content.clone(),
            sequence,
        );

        // Try to send if connection is available
        if let Some(conn) = conn {
            let protocol_msg = ControlMessage::ChatMessage {
                message_id: message.id.0.clone(),
                content: content.clone(),
                sequence,
                sent_at: message.sent_at.timestamp_millis(),
            };

            match conn.send(&protocol_msg).await {
                Ok(()) => {
                    message.mark_sent();
                }
                Err(e) => {
                    // Queue for later
                    tracing::warn!("Failed to send chat message, queuing: {}", e);
                    self.store.queue_pending(peer_id, &message)?;
                }
            }
        } else {
            // No connection, queue for later
            self.store.queue_pending(peer_id, &message)?;
        }

        // Store the message
        self.store.store_message(&message, &self.our_endpoint_id)?;

        // Update conversation
        conversation.on_message_sent(&message);
        self.store.upsert_conversation(&conversation)?;

        events.push(ChatEvent::MessageSent {
            peer_id: peer_id.to_string(),
            message: message.clone(),
        });
        events.push(ChatEvent::ConversationUpdated {
            conversation: conversation.clone(),
        });

        Ok((message, events))
    }

    /// Send a typing indicator to a peer.
    pub async fn send_typing(
        &self,
        conn: &mut ControlConnection,
        is_typing: bool,
    ) -> Result<()> {
        let msg = ControlMessage::ChatTyping { is_typing };
        conn.send(&msg).await
    }

    /// Send read receipts to a peer.
    pub async fn send_read_receipts(
        &self,
        conn: &mut ControlConnection,
        message_ids: Vec<String>,
        up_to_sequence: u64,
    ) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }

        let msg = ControlMessage::ChatRead {
            message_ids,
            up_to_sequence,
        };
        conn.send(&msg).await
    }

    // ==================== Receiving Methods ====================

    /// Handle an incoming chat message.
    pub fn handle_chat_message(
        &self,
        sender_id: &str,
        sender_name: &str,
        message_id: String,
        content: String,
        sequence: u64,
        sent_at: i64,
    ) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();

        // Convert timestamp
        let sent_at_dt = Utc.timestamp_millis_opt(sent_at).unwrap();

        // Create the message
        let message = ChatMessage::from_incoming(
            sender_id.to_string(),
            self.our_endpoint_id.clone(),
            message_id,
            content,
            sequence,
            sent_at_dt,
        );

        // Store the message
        self.store.store_message(&message, &self.our_endpoint_id)?;

        // Get or create conversation and update it
        let mut conversation = self.store.get_or_create_conversation(sender_id, sender_name)?;
        conversation.on_message_received(&message);
        self.store.upsert_conversation(&conversation)?;

        events.push(ChatEvent::MessageReceived {
            peer_id: sender_id.to_string(),
            message: message.clone(),
        });
        events.push(ChatEvent::ConversationUpdated {
            conversation,
        });

        Ok(events)
    }

    /// Handle delivery receipts from a peer.
    pub fn handle_delivered(&self, message_ids: Vec<String>) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();

        for id in message_ids {
            if self.store.update_message_status(&id, MessageStatus::Delivered)? {
                events.push(ChatEvent::MessageDelivered {
                    message_id: id,
                });
            }
        }

        Ok(events)
    }

    /// Handle read receipts from a peer.
    pub fn handle_read(&self, message_ids: Vec<String>, _up_to_sequence: u64) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();

        for id in message_ids {
            if self.store.update_message_status(&id, MessageStatus::Read)? {
                events.push(ChatEvent::MessageRead {
                    message_id: id,
                });
            }
        }

        Ok(events)
    }

    /// Handle typing indicator from a peer.
    pub fn handle_typing(&self, peer_id: &str, peer_name: &str, is_typing: bool) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();

        // Update conversation
        if let Some(mut conversation) = self.store.get_conversation(peer_id)? {
            conversation.set_typing(is_typing);
            self.store.upsert_conversation(&conversation)?;

            events.push(ChatEvent::TypingChanged {
                peer_id: peer_id.to_string(),
                is_typing,
            });
            events.push(ChatEvent::ConversationUpdated {
                conversation,
            });
        } else if is_typing {
            // Create conversation if needed when typing starts
            let mut conversation = ChatConversation::new(peer_id.to_string(), peer_name.to_string());
            conversation.set_typing(true);
            self.store.upsert_conversation(&conversation)?;

            events.push(ChatEvent::TypingChanged {
                peer_id: peer_id.to_string(),
                is_typing: true,
            });
            events.push(ChatEvent::ConversationUpdated {
                conversation,
            });
        }

        Ok(events)
    }

    // ==================== Sync Methods ====================

    /// Request history sync from a peer.
    pub async fn request_sync(
        &self,
        conn: &mut ControlConnection,
        peer_id: &str,
        limit: u32,
    ) -> Result<()> {
        // Get the last received sequence for this peer
        let after_sequence = self.store
            .get_sync_state(peer_id)?
            .map(|s| s.last_received_sequence)
            .unwrap_or(0);

        let msg = ControlMessage::ChatSyncRequest {
            after_sequence,
            limit,
        };
        conn.send(&msg).await
    }

    /// Handle a sync request from a peer.
    pub async fn handle_sync_request(
        &self,
        conn: &mut ControlConnection,
        peer_id: &str,
        after_sequence: u64,
        limit: u32,
    ) -> Result<()> {
        // Get messages we've sent to this peer after the given sequence
        let messages = self.store.get_messages_after_sequence(
            peer_id,
            after_sequence,
            limit as usize,
        )?;

        // Filter to only messages we sent
        let our_messages: Vec<_> = messages
            .into_iter()
            .filter(|m| m.sender_id == self.our_endpoint_id)
            .collect();

        let has_more = our_messages.len() == limit as usize;

        let sync_messages: Vec<ChatSyncMessage> = our_messages
            .iter()
            .map(ChatSyncMessage::from_message)
            .collect();

        let response = ControlMessage::ChatSyncResponse {
            messages: sync_messages,
            has_more,
        };

        conn.send(&response).await
    }

    /// Handle a sync response from a peer.
    pub fn handle_sync_response(
        &self,
        peer_id: &str,
        _peer_name: &str,
        messages: Vec<ChatSyncMessage>,
    ) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();
        let count = messages.len();

        if messages.is_empty() {
            return Ok(events);
        }

        // Process each message
        let mut max_sequence = 0u64;
        for sync_msg in messages {
            max_sequence = max_sequence.max(sync_msg.sequence);

            let message = sync_msg.to_message(self.our_endpoint_id.clone());

            // Check if we already have this message
            if self.store.get_message(&message.id.0)?.is_some() {
                continue;
            }

            // Store the message
            self.store.store_message(&message, &self.our_endpoint_id)?;
        }

        // Update sync state
        let mut sync_state = self.store
            .get_sync_state(peer_id)?
            .unwrap_or_else(|| crate::chat::types::SyncState::new(peer_id.to_string()));
        sync_state.on_received(max_sequence);
        sync_state.on_sync_complete();
        self.store.update_sync_state(&sync_state)?;

        // Update conversation
        if let Ok(Some(conversation)) = self.store.get_conversation(peer_id) {
            events.push(ChatEvent::ConversationUpdated { conversation });
        }

        events.push(ChatEvent::SyncCompleted {
            peer_id: peer_id.to_string(),
            count,
        });

        Ok(events)
    }

    // ==================== Offline Queue Methods ====================

    /// Flush pending messages to a peer when they come online.
    pub async fn flush_pending(
        &self,
        conn: &mut ControlConnection,
        peer_id: &str,
    ) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();

        let pending = self.store.get_pending(peer_id)?;
        if pending.is_empty() {
            return Ok(events);
        }

        let mut sent_ids = Vec::new();

        for msg in &pending {
            let protocol_msg = ControlMessage::ChatMessage {
                message_id: msg.id.0.clone(),
                content: msg.content.clone(),
                sequence: msg.sequence,
                sent_at: msg.sent_at.timestamp_millis(),
            };

            match conn.send(&protocol_msg).await {
                Ok(()) => {
                    // Update status to sent
                    self.store.update_message_status(&msg.id.0, MessageStatus::Sent)?;
                    sent_ids.push(msg.id.0.clone());
                }
                Err(e) => {
                    tracing::warn!("Failed to flush pending message {}: {}", msg.id, e);
                    break; // Stop on first failure
                }
            }
        }

        // Clear sent messages from pending queue
        let count = sent_ids.len();
        if !sent_ids.is_empty() {
            self.store.clear_pending(peer_id, &sent_ids)?;

            events.push(ChatEvent::PendingFlushed {
                peer_id: peer_id.to_string(),
                count,
            });
        }

        Ok(events)
    }

    // ==================== Utility Methods ====================

    /// Mark all messages from a peer as read.
    pub fn mark_conversation_read(&self, peer_id: &str) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();

        if let Some(mut conversation) = self.store.get_conversation(peer_id)? {
            if conversation.unread_count > 0 {
                conversation.mark_all_read();
                self.store.upsert_conversation(&conversation)?;

                events.push(ChatEvent::ConversationUpdated { conversation });
            }
        }

        Ok(events)
    }

    /// Get messages for display (newest first, with pagination).
    pub fn get_messages(
        &self,
        peer_id: &str,
        limit: usize,
        before_seq: Option<u64>,
    ) -> Result<Vec<ChatMessage>> {
        self.store.get_messages(peer_id, limit, before_seq)
    }

    /// Get all conversations.
    pub fn get_conversations(&self) -> Result<Vec<ChatConversation>> {
        self.store.list_conversations()
    }

    /// Get a specific conversation.
    pub fn get_conversation(&self, peer_id: &str) -> Result<Option<ChatConversation>> {
        self.store.get_conversation(peer_id)
    }

    /// Get total unread count.
    pub fn get_total_unread(&self) -> Result<u32> {
        self.store.get_total_unread()
    }

    /// Check and clear expired typing indicators.
    pub fn check_typing_expired(&self) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();

        for conv in self.store.list_conversations()? {
            if conv.peer_is_typing {
                let mut updated = conv.clone();
                if updated.check_typing_expired() {
                    self.store.upsert_conversation(&updated)?;
                    events.push(ChatEvent::TypingChanged {
                        peer_id: updated.peer_id.clone(),
                        is_typing: false,
                    });
                    events.push(ChatEvent::ConversationUpdated {
                        conversation: updated,
                    });
                }
            }
        }

        Ok(events)
    }
}

/// Shared chat handler for thread-safe access.
pub type SharedChatHandler = Arc<RwLock<ChatHandler>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_incoming_message() {
        let store = Arc::new(ChatStore::open_in_memory().unwrap());
        let handler = ChatHandler::new(store, "our_endpoint".to_string());

        let events = handler
            .handle_chat_message(
                "peer1",
                "Peer One",
                "msg123".to_string(),
                "Hello!".to_string(),
                1,
                Utc::now().timestamp_millis(),
            )
            .unwrap();

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], ChatEvent::MessageReceived { peer_id, .. } if peer_id == "peer1"));
        assert!(matches!(&events[1], ChatEvent::ConversationUpdated { .. }));

        // Check conversation was created
        let conv = handler.get_conversation("peer1").unwrap().unwrap();
        assert_eq!(conv.unread_count, 1);
    }

    #[test]
    fn test_mark_conversation_read() {
        let store = Arc::new(ChatStore::open_in_memory().unwrap());
        let handler = ChatHandler::new(store, "our_endpoint".to_string());

        // Receive a message
        handler
            .handle_chat_message(
                "peer1",
                "Peer One",
                "msg123".to_string(),
                "Hello!".to_string(),
                1,
                Utc::now().timestamp_millis(),
            )
            .unwrap();

        // Mark as read
        let events = handler.mark_conversation_read("peer1").unwrap();
        assert_eq!(events.len(), 1);

        // Check unread is now 0
        let conv = handler.get_conversation("peer1").unwrap().unwrap();
        assert_eq!(conv.unread_count, 0);
    }

    #[test]
    fn test_typing_indicator() {
        let store = Arc::new(ChatStore::open_in_memory().unwrap());
        let handler = ChatHandler::new(store, "our_endpoint".to_string());

        // Create conversation first
        handler
            .handle_chat_message(
                "peer1",
                "Peer One",
                "msg123".to_string(),
                "Hello!".to_string(),
                1,
                Utc::now().timestamp_millis(),
            )
            .unwrap();

        // Set typing
        let events = handler.handle_typing("peer1", "Peer One", true).unwrap();
        assert!(events.iter().any(|e| matches!(e, ChatEvent::TypingChanged { is_typing: true, .. })));

        let conv = handler.get_conversation("peer1").unwrap().unwrap();
        assert!(conv.peer_is_typing);

        // Clear typing
        let events = handler.handle_typing("peer1", "Peer One", false).unwrap();
        assert!(events.iter().any(|e| matches!(e, ChatEvent::TypingChanged { is_typing: false, .. })));

        let conv = handler.get_conversation("peer1").unwrap().unwrap();
        assert!(!conv.peer_is_typing);
    }
}

//! Screen streaming handlers for the daemon.
//!
//! This module handles incoming screen streaming requests from peers,
//! including permission checks, session management, and input injection.

use std::collections::HashMap;
use std::sync::Arc;

use croh_core::config::ScreenStreamSettings;
use croh_core::error::{Error, Result};
use croh_core::iroh::protocol::{
    ControlMessage, DisplayInfo, ScreenCompression, ScreenQuality,
};
use croh_core::peers::TrustedPeer;
use croh_core::screen::{
    Display, RemoteInputEvent, ScreenStreamManager, StreamCommandSender, StreamEventSender,
};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Screen streaming handler state.
pub struct ScreenHandler {
    /// Stream manager for handling sessions.
    manager: Arc<ScreenStreamManager>,
    /// Active session handles (stream_id -> command sender).
    sessions: Arc<RwLock<HashMap<String, StreamCommandSender>>>,
    /// Screen streaming settings.
    settings: ScreenStreamSettings,
}

impl ScreenHandler {
    /// Create a new screen handler.
    pub fn new(settings: ScreenStreamSettings, event_tx: StreamEventSender) -> Self {
        let manager = Arc::new(ScreenStreamManager::new(event_tx, settings.clone()));
        Self {
            manager,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            settings,
        }
    }

    /// Get the underlying stream manager.
    pub fn manager(&self) -> Arc<ScreenStreamManager> {
        self.manager.clone()
    }

    /// Check if screen streaming is enabled.
    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Handle a display list request.
    ///
    /// Returns a `DisplayListResponse` message with available displays.
    pub async fn handle_display_list_request(&self) -> Result<ControlMessage> {
        if !self.settings.enabled {
            return Err(Error::Screen("Screen streaming is disabled".into()));
        }

        let displays = self.manager.list_displays().await?;
        let display_infos = displays_to_protocol(&displays);

        Ok(ControlMessage::DisplayListResponse {
            displays: display_infos,
            backend: "auto".to_string(), // Backend name determined at capture time
        })
    }

    /// Handle a screen stream request.
    ///
    /// Validates permissions, starts the session, and returns response.
    pub async fn handle_stream_request(
        &self,
        stream_id: String,
        display_id: Option<String>,
        compression: ScreenCompression,
        _quality: ScreenQuality,
        _target_fps: u32,
        peer: &TrustedPeer,
    ) -> Result<ControlMessage> {
        // Check if streaming is enabled
        if !self.settings.enabled {
            return Ok(ControlMessage::ScreenStreamResponse {
                stream_id,
                accepted: false,
                reason: Some("Screen streaming is disabled on this device".into()),
                displays: vec![],
                compression: None,
            });
        }

        // Check peer has screen_view permission
        if !peer.their_permissions.screen_view {
            warn!(
                "Peer {} attempted screen stream without screen_view permission",
                peer.name
            );
            return Ok(ControlMessage::ScreenStreamResponse {
                stream_id,
                accepted: false,
                reason: Some("screen_view permission not granted".into()),
                displays: vec![],
                compression: None,
            });
        }

        // Check if capture is available
        if !self.manager.is_available().await {
            return Ok(ControlMessage::ScreenStreamResponse {
                stream_id,
                accepted: false,
                reason: Some("Screen capture not available on this system".into()),
                displays: vec![],
                compression: None,
            });
        }

        // Get available displays
        let displays = match self.manager.list_displays().await {
            Ok(d) => d,
            Err(e) => {
                error!("Failed to list displays: {}", e);
                return Ok(ControlMessage::ScreenStreamResponse {
                    stream_id,
                    accepted: false,
                    reason: Some(format!("Failed to enumerate displays: {}", e)),
                    displays: vec![],
                    compression: None,
                });
            }
        };

        // Determine which display to capture
        let target_display = if let Some(ref id) = display_id {
            displays
                .iter()
                .find(|d| d.id == *id)
                .map(|d| d.id.clone())
        } else {
            // Default to primary or first display
            displays
                .iter()
                .find(|d| d.is_primary)
                .or(displays.first())
                .map(|d| d.id.clone())
        };

        let target_display = match target_display {
            Some(id) => id,
            None => {
                return Ok(ControlMessage::ScreenStreamResponse {
                    stream_id,
                    accepted: false,
                    reason: Some("No displays available".into()),
                    displays: vec![],
                    compression: None,
                });
            }
        };

        // Check if peer has screen_control permission (for input injection)
        let allow_input = peer.their_permissions.screen_control && self.settings.allow_input;

        // Start the stream session
        match self
            .manager
            .start_stream(
                stream_id.clone(),
                target_display.clone(),
                peer.endpoint_id.clone(),
                allow_input,
            )
            .await
        {
            Ok(()) => {
                info!(
                    "Started screen stream {} for peer {} on display {}",
                    stream_id, peer.name, target_display
                );

                // Store the session handle
                if let Some(handle) = self.manager.get_handle(&stream_id).await {
                    let mut sessions = self.sessions.write().await;
                    sessions.insert(stream_id.clone(), handle);
                }

                let display_infos = displays_to_protocol(&displays);

                Ok(ControlMessage::ScreenStreamResponse {
                    stream_id,
                    accepted: true,
                    reason: None,
                    displays: display_infos,
                    compression: Some(compression),
                })
            }
            Err(e) => {
                error!("Failed to start stream {}: {}", stream_id, e);
                Ok(ControlMessage::ScreenStreamResponse {
                    stream_id,
                    accepted: false,
                    reason: Some(format!("Failed to start stream: {}", e)),
                    displays: vec![],
                    compression: None,
                })
            }
        }
    }

    /// Handle a screen stream stop request.
    pub async fn handle_stream_stop(
        &self,
        stream_id: &str,
        reason: &str,
        peer: &TrustedPeer,
    ) -> Result<()> {
        info!(
            "Stopping stream {} for peer {} (reason: {})",
            stream_id, peer.name, reason
        );

        // Stop the stream
        self.manager.stop_stream(stream_id).await?;

        // Remove from active sessions
        let mut sessions = self.sessions.write().await;
        sessions.remove(stream_id);

        Ok(())
    }

    /// Handle a screen stream adjust request.
    pub async fn handle_stream_adjust(
        &self,
        stream_id: &str,
        quality: Option<ScreenQuality>,
        target_fps: Option<u32>,
        request_keyframe: bool,
        peer: &TrustedPeer,
    ) -> Result<()> {
        debug!(
            "Adjusting stream {} for peer {}: quality={:?}, fps={:?}, keyframe={}",
            stream_id, peer.name, quality, target_fps, request_keyframe
        );

        let sessions = self.sessions.read().await;
        if let Some(handle) = sessions.get(stream_id) {
            // Request keyframe if needed
            if request_keyframe {
                let _ = handle
                    .send(croh_core::screen::StreamCommand::ForceKeyframe)
                    .await;
            }

            // Adjust quality settings
            if quality.is_some() || target_fps.is_some() {
                let _ = handle
                    .send(croh_core::screen::StreamCommand::AdjustQuality {
                        fps: target_fps,
                        quality,
                        bitrate_kbps: None,
                    })
                    .await;
            }
        } else {
            warn!("Stream {} not found for adjust request", stream_id);
        }

        Ok(())
    }

    /// Handle input events from a peer.
    pub async fn handle_input(
        &self,
        stream_id: &str,
        events: Vec<RemoteInputEvent>,
        peer: &TrustedPeer,
    ) -> Result<()> {
        // Check screen_control permission
        if !peer.their_permissions.screen_control {
            warn!(
                "Peer {} attempted input injection without screen_control permission",
                peer.name
            );
            return Err(Error::Screen(
                "screen_control permission not granted".into(),
            ));
        }

        // Check if input is allowed in settings
        if !self.settings.allow_input {
            return Err(Error::Screen(
                "Input injection is disabled on this device".into(),
            ));
        }

        let sessions = self.sessions.read().await;
        if let Some(handle) = sessions.get(stream_id) {
            for event in events {
                let _ = handle
                    .send(croh_core::screen::StreamCommand::InjectInput(event))
                    .await;
            }
            Ok(())
        } else {
            Err(Error::Screen(format!("Stream {} not found", stream_id)))
        }
    }

    /// Handle a frame ACK from a peer.
    pub async fn handle_frame_ack(
        &self,
        stream_id: &str,
        up_to_sequence: u64,
        _estimated_bandwidth: Option<u64>,
        _quality_hint: Option<ScreenQuality>,
    ) -> Result<()> {
        self.manager.process_ack(stream_id, up_to_sequence).await
    }

    /// Stop all active streams.
    pub async fn stop_all(&self) {
        let stream_ids: Vec<String> = {
            let sessions = self.sessions.read().await;
            sessions.keys().cloned().collect()
        };

        for stream_id in stream_ids {
            if let Err(e) = self.manager.stop_stream(&stream_id).await {
                warn!("Error stopping stream {}: {}", stream_id, e);
            }
        }

        let mut sessions = self.sessions.write().await;
        sessions.clear();

        info!("Stopped all screen streaming sessions");
    }

    /// Get the number of active streams.
    pub async fn active_stream_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        sessions.len()
    }
}

/// Convert internal Display structs to protocol DisplayInfo.
fn displays_to_protocol(displays: &[Display]) -> Vec<DisplayInfo> {
    displays
        .iter()
        .map(|d| DisplayInfo {
            id: d.id.clone(),
            name: d.name.clone(),
            width: d.width,
            height: d.height,
            refresh_rate: d.refresh_rate,
            is_primary: d.is_primary,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use croh_core::config::ScreenStreamSettings;
    use croh_core::peers::Permissions;
    use tokio::sync::mpsc;

    fn create_test_peer(screen_view: bool, screen_control: bool) -> TrustedPeer {
        TrustedPeer::new(
            "test-endpoint".to_string(),
            "Test Peer".to_string(),
            Permissions::default(),
            Permissions {
                screen_view,
                screen_control,
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn test_handler_disabled() {
        let (event_tx, _rx) = mpsc::channel(32);
        let settings = ScreenStreamSettings {
            enabled: false,
            ..Default::default()
        };
        let handler = ScreenHandler::new(settings, event_tx);

        let peer = create_test_peer(true, true);

        let response = handler
            .handle_stream_request(
                "test-stream".to_string(),
                None,
                ScreenCompression::Raw,
                ScreenQuality::Balanced,
                30,
                &peer,
            )
            .await
            .unwrap();

        match response {
            ControlMessage::ScreenStreamResponse { accepted, .. } => {
                assert!(!accepted);
            }
            _ => panic!("Expected ScreenStreamResponse"),
        }
    }

    #[tokio::test]
    async fn test_handler_permission_denied() {
        let (event_tx, _rx) = mpsc::channel(32);
        let settings = ScreenStreamSettings {
            enabled: true,
            ..Default::default()
        };
        let handler = ScreenHandler::new(settings, event_tx);

        let peer = create_test_peer(false, false); // No screen permissions

        let response = handler
            .handle_stream_request(
                "test-stream".to_string(),
                None,
                ScreenCompression::Raw,
                ScreenQuality::Balanced,
                30,
                &peer,
            )
            .await
            .unwrap();

        match response {
            ControlMessage::ScreenStreamResponse {
                accepted, reason, ..
            } => {
                assert!(!accepted);
                assert!(reason.unwrap().contains("permission"));
            }
            _ => panic!("Expected ScreenStreamResponse"),
        }
    }

    #[tokio::test]
    async fn test_input_permission_denied() {
        let (event_tx, _rx) = mpsc::channel(32);
        let settings = ScreenStreamSettings {
            enabled: true,
            allow_input: true,
            ..Default::default()
        };
        let handler = ScreenHandler::new(settings, event_tx);

        let peer = create_test_peer(true, false); // screen_view but not screen_control

        let result = handler
            .handle_input(
                "test-stream",
                vec![RemoteInputEvent::MouseMove {
                    x: 100,
                    y: 100,
                    absolute: true,
                }],
                &peer,
            )
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("permission"));
    }
}

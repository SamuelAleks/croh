//! Application state and callback handling.

use croh_core::{
    config::Theme,
    croc::{find_croc_executable, refresh_croc_cache, Curve, CrocEvent, CrocOptions, CrocProcess, HashAlgorithm},
    files, platform, Config, ControlMessage, FileRequest, Identity, IrohEndpoint, PeerStore, Permissions,
    Transfer, TransferId, TransferEvent, TransferHistory, TransferManager, TransferStatus, TransferType,
    TrustedPeer, TrustBundle, push_files, pull_files, handle_incoming_push, handle_incoming_pull, handle_browse_request, browse_remote, default_browsable_paths,
    NodeAddr, NodeId,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, error, info, warn};

use crate::{AppLogic, AppSettings, BrowseEntry, MainWindow, PeerItem, SelectedFile, TransferItem};

/// Connection status for a peer.
#[derive(Clone, Debug, Default)]
pub struct PeerConnectionStatus {
    /// Whether the peer is currently online.
    pub online: bool,
    /// Last successful ping time.
    pub last_ping: Option<std::time::Instant>,
    /// Round-trip latency in milliseconds (from last ping).
    pub latency_ms: Option<u32>,
    /// Rolling average latency (last 10 pings).
    pub avg_latency_ms: Option<u32>,
    /// Recent latency samples for average calculation.
    pub latency_history: Vec<u32>,
    /// Connection type: "direct", "relay", or "unknown".
    pub connection_type: String,
    /// Total number of successful pings.
    pub ping_count: u32,
    /// Last contact time (for display as relative time).
    pub last_contact: Option<chrono::DateTime<chrono::Utc>>,
    /// Last upload speed (from transfers).
    pub last_upload_speed: Option<String>,
    /// Last download speed (from transfers).
    pub last_download_speed: Option<String>,
    /// Remote peer info from StatusResponse.
    pub peer_hostname: Option<String>,
    pub peer_os: Option<String>,
    pub peer_version: Option<String>,
    pub peer_free_space: Option<u64>,
}

/// Application state.
pub struct App {
    window: Weak<MainWindow>,
    selected_files: Arc<RwLock<Vec<SelectedFileData>>>,
    transfer_manager: TransferManager,
    /// Active croc processes mapped by transfer ID.
    active_processes: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    config: Arc<RwLock<Config>>,
    /// Identity for Iroh networking.
    identity: Arc<RwLock<Option<Identity>>>,
    /// Shared Iroh endpoint (created once, used for all connections).
    shared_endpoint: Arc<RwLock<Option<IrohEndpoint>>>,
    /// Peer store for trusted peers.
    peer_store: Arc<RwLock<PeerStore>>,
    /// Flag to track if trust initiation is in progress.
    trust_in_progress: Arc<RwLock<bool>>,
    /// Flag to signal background listener shutdown.
    listener_shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Current browse state (peer_id, current_path, entries with selection state).
    browse_state: Arc<RwLock<BrowseState>>,
    /// Pending trust handshake state (nonce + result channel).
    pending_trust: Arc<RwLock<Option<PendingTrust>>>,
    /// Shared peers model for UI (allows in-place updates).
    peers_model: std::rc::Rc<VecModel<PeerItem>>,
    /// Connection status for each peer (keyed by peer ID).
    peer_status: Arc<RwLock<HashMap<String, PeerConnectionStatus>>>,
    /// Transfer history for completed transfers.
    transfer_history: Arc<RwLock<TransferHistory>>,
}

/// State for file browser dialog.
#[derive(Clone, Debug, Default)]
struct BrowseState {
    peer_id: String,
    peer_name: String,
    current_path: String,
    /// Previous successful path (for "Go Back" when errors occur)
    previous_path: Option<String>,
    entries: Vec<BrowseEntryData>,
}

/// Internal representation of a browse entry.
#[derive(Clone, Debug)]
struct BrowseEntryData {
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<i64>,
    path: String,
    selected: bool,
}

/// Internal representation of a selected file.
#[derive(Clone, Debug)]
struct SelectedFileData {
    name: String,
    size: u64,
    path: String,
}

/// State for pending trust handshake.
/// When initiating trust, we store the nonce here so the background listener
/// can complete the handshake when the peer connects.
struct PendingTrust {
    /// The nonce from the trust bundle we sent.
    nonce: String,
    /// Channel to send the result back to the trust initiation thread.
    result_tx: oneshot::Sender<Result<TrustedPeer, croh_core::Error>>,
}

impl App {
    /// Create a new App instance.
    pub fn new(window: Weak<MainWindow>) -> Self {
        let config = Config::load_with_env().unwrap_or_default();
        let peer_store = PeerStore::load().unwrap_or_default();
        let transfer_history = TransferHistory::load().unwrap_or_default();

        // Create shared peers model and set it on the UI
        let peers_model = std::rc::Rc::new(VecModel::<PeerItem>::default());
        if let Some(win) = window.upgrade() {
            win.global::<AppLogic>().set_peers(peers_model.clone().into());
        }

        Self {
            window,
            selected_files: Arc::new(RwLock::new(Vec::new())),
            transfer_manager: TransferManager::new(),
            active_processes: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(config)),
            identity: Arc::new(RwLock::new(None)),
            shared_endpoint: Arc::new(RwLock::new(None)),
            peer_store: Arc::new(RwLock::new(peer_store)),
            trust_in_progress: Arc::new(RwLock::new(false)),
            listener_shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            browse_state: Arc::new(RwLock::new(BrowseState::default())),
            pending_trust: Arc::new(RwLock::new(None)),
            peers_model,
            peer_status: Arc::new(RwLock::new(HashMap::new())),
            transfer_history: Arc::new(RwLock::new(transfer_history)),
        }
    }

    /// Set up all UI callbacks.
    pub fn setup_callbacks(&self, window: &MainWindow) {
        // Initialize settings in UI
        self.init_settings(window);
        // Initialize identity and peers
        self.init_identity_and_peers(window);

        self.setup_file_callbacks(window);
        self.setup_transfer_callbacks(window);
        self.setup_settings_callbacks(window);
        self.setup_peer_callbacks(window);
        self.setup_browse_callbacks(window);

        // Start background listener for incoming transfers
        self.start_background_listener();

        // Start peer status checker (pings peers periodically)
        self.start_peer_status_checker();
    }

    /// Refresh the peers model with the given items (clears and repopulates).
    fn refresh_peers_model(&self, items: Vec<PeerItem>) {
        // Clear existing items
        while self.peers_model.row_count() > 0 {
            self.peers_model.remove(0);
        }

        // Collect peers that allow push for the send panel dropdown
        let mut pushable_names: Vec<SharedString> = Vec::new();
        let mut pushable_ids: Vec<SharedString> = Vec::new();

        // Add new items
        for item in &items {
            // Check if this peer allows us to push (their_permissions.push == true means can_push)
            if item.can_push {
                pushable_names.push(item.name.clone());
                pushable_ids.push(item.id.clone());
            }
            self.peers_model.push(item.clone());
        }

        // Update the push-to-peer dropdown data
        if let Some(window) = self.window.upgrade() {
            window.global::<AppLogic>().set_peer_names_for_push(
                ModelRc::new(VecModel::from(pushable_names))
            );
            window.global::<AppLogic>().set_pushable_peer_ids(
                ModelRc::new(VecModel::from(pushable_ids))
            );
        }
    }

    /// Start a background task that periodically pings peers to check their status.
    fn start_peer_status_checker(&self) {
        let shared_endpoint = self.shared_endpoint.clone();
        let peer_store = self.peer_store.clone();
        let peer_status = self.peer_status.clone();
        let window_weak = self.window.clone();
        let shutdown = self.listener_shutdown.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                // Wait for endpoint to be ready
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let ep_guard = shared_endpoint.read().await;
                    if ep_guard.is_some() {
                        break;
                    }
                    drop(ep_guard);

                    if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                }

                // Ping loop - check peer status every 30 seconds
                loop {
                    if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }

                    // Get list of peers to ping
                    let peers: Vec<TrustedPeer> = {
                        let store = peer_store.read().await;
                        store.list().to_vec()
                    };

                    // Get the endpoint
                    let endpoint = {
                        let ep_guard = shared_endpoint.read().await;
                        match ep_guard.as_ref() {
                            Some(ep) => ep.clone(),
                            None => {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                continue;
                            }
                        }
                    };

                    // Ping each peer concurrently
                    let mut ping_tasks = Vec::new();
                    for peer in peers {
                        let endpoint = endpoint.clone();
                        let peer_status = peer_status.clone();
                        let peer_id = peer.id.clone();
                        let has_relay = peer.relay_url.is_some();

                        ping_tasks.push(tokio::spawn(async move {
                            let start = std::time::Instant::now();
                            let online = ping_peer(&endpoint, &peer).await;
                            let latency_ms = if online {
                                Some(start.elapsed().as_millis() as u32)
                            } else {
                                None
                            };

                            // Update status with full details
                            let mut status_map = peer_status.write().await;
                            let status = status_map.entry(peer_id).or_default();
                            status.online = online;

                            if online {
                                status.last_ping = Some(std::time::Instant::now());
                                status.last_contact = Some(chrono::Utc::now());
                                status.latency_ms = latency_ms;
                                status.ping_count += 1;

                                // Update latency history (keep last 10)
                                if let Some(lat) = latency_ms {
                                    status.latency_history.push(lat);
                                    if status.latency_history.len() > 10 {
                                        status.latency_history.remove(0);
                                    }
                                    // Calculate average
                                    let sum: u32 = status.latency_history.iter().sum();
                                    status.avg_latency_ms = Some(sum / status.latency_history.len() as u32);
                                }

                                // Determine connection type based on relay presence
                                // (In practice, we'd need to query the connection info)
                                status.connection_type = if has_relay {
                                    "relay".to_string()
                                } else {
                                    "direct".to_string()
                                };
                            }
                        }));
                    }

                    // Wait for all pings to complete
                    for task in ping_tasks {
                        let _ = task.await;
                    }

                    // Get full status snapshot for UI update
                    let status_map = peer_status.read().await;
                    let status_snapshot: HashMap<String, PeerConnectionStatus> = status_map.clone();
                    drop(status_map);

                    let window_weak_ui = window_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = window_weak_ui.upgrade() {
                            let logic = window.global::<AppLogic>();
                            let model = logic.get_peers();

                            // Update status for each peer in the model
                            for i in 0..model.row_count() {
                                if let Some(mut peer) = model.row_data(i) {
                                    let id = peer.id.to_string();
                                    if let Some(status) = status_snapshot.get(&id) {
                                        apply_status_to_peer_item(&mut peer, status);
                                        model.set_row_data(i, peer);
                                    }
                                }
                            }
                        }
                    });

                    // Wait 30 seconds before next ping cycle
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
            });
        });
    }

    /// Start a background listener for incoming peer connections.
    fn start_background_listener(&self) {
        let identity = self.identity.clone();
        let shared_endpoint = self.shared_endpoint.clone();
        let peer_store = self.peer_store.clone();
        let config = self.config.clone();
        let transfer_manager = self.transfer_manager.clone();
        let transfer_history = self.transfer_history.clone();
        let window_weak = self.window.clone();
        let shutdown = self.listener_shutdown.clone();
        let pending_trust = self.pending_trust.clone();
        let peer_status = self.peer_status.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                // Wait for identity to be loaded
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let id_guard = identity.read().await;
                    if id_guard.is_some() {
                        break;
                    }
                    drop(id_guard);

                    if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                }

                // Get identity
                let our_identity = {
                    let id_guard = identity.read().await;
                    match id_guard.as_ref() {
                        Some(id) => id.clone(),
                        None => {
                            error!("Identity not available for background listener");
                            return;
                        }
                    }
                };

                // Create the shared endpoint (used by all Iroh operations)
                let endpoint = match IrohEndpoint::new(our_identity).await {
                    Ok(ep) => ep,
                    Err(e) => {
                        error!("Failed to create shared Iroh endpoint: {}", e);
                        return;
                    }
                };

                // Store the endpoint for use by other operations (trust, push, pull)
                {
                    let mut ep_guard = shared_endpoint.write().await;
                    *ep_guard = Some(endpoint.clone());
                }

                info!("Background listener started, waiting for incoming connections...");

                // Accept connections in a loop
                loop {
                    if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        info!("Background listener shutting down");
                        break;
                    }

                    // Accept with timeout so we can check shutdown flag
                    let accept_result = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        endpoint.accept()
                    ).await;

                    let mut conn = match accept_result {
                        Ok(Ok(conn)) => conn,
                        Ok(Err(e)) => {
                            warn!("Accept error in background listener: {}", e);
                            continue;
                        }
                        Err(_) => {
                            // Timeout, check shutdown flag and continue
                            continue;
                        }
                    };

                    let remote_id = conn.remote_id_string();
                    info!("Background listener: accepted connection from {}", remote_id);

                    // Check if this is a trusted peer
                    let store = peer_store.read().await;
                    let peer = store.find_by_endpoint_id(&remote_id).cloned();
                    drop(store);

                    // Receive the first message to determine what type of request this is
                    let msg = match tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        conn.recv()
                    ).await {
                        Ok(Ok(msg)) => msg,
                        Ok(Err(e)) => {
                            warn!("Failed to receive message from {}: {}", remote_id, e);
                            continue;
                        }
                        Err(_) => {
                            warn!("Timeout waiting for message from {}", remote_id);
                            continue;
                        }
                    };

                    // If this is an unknown peer, check if it's a pending trust handshake
                    if peer.is_none() {
                        // Check if there's a pending trust and this is a TrustConfirm
                        if let ControlMessage::TrustConfirm { peer: their_peer_info, nonce, permissions: their_permissions } = msg {
                            info!("Received TrustConfirm from unknown peer {}", remote_id);

                            // Check if we have a pending trust with matching nonce
                            let mut pending_guard = pending_trust.write().await;
                            if let Some(pending) = pending_guard.take() {
                                if pending.nonce == nonce {
                                    info!("Valid TrustConfirm from {} ({})", their_peer_info.name, remote_id);
                                    if their_peer_info.relay_url.is_some() {
                                        info!("Peer relay URL: {:?}", their_peer_info.relay_url);
                                    }

                                    // Send TrustComplete
                                    let response = ControlMessage::TrustComplete;
                                    if let Err(e) = conn.send(&response).await {
                                        error!("Failed to send TrustComplete: {}", e);
                                        let _ = pending.result_tx.send(Err(croh_core::Error::Iroh(e.to_string())));
                                        continue;
                                    }

                                    // Create the trusted peer with their relay URL for future connections
                                    let new_peer = TrustedPeer::new_with_relay(
                                        their_peer_info.endpoint_id.clone(),
                                        their_peer_info.name.clone(),
                                        Permissions::all(), // We grant all permissions
                                        their_permissions,  // Their permissions to us
                                        their_peer_info.relay_url.clone(), // Store their relay URL
                                    );

                                    // Add to peer store
                                    let mut store = peer_store.write().await;
                                    match store.add_or_update(new_peer.clone()) {
                                        Ok(updated) => {
                                            if updated {
                                                info!("Updated existing peer: {}", new_peer.name);
                                            } else {
                                                info!("Added new peer: {}", new_peer.name);
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to save peer: {}", e);
                                            let _ = pending.result_tx.send(Err(e));
                                            continue;
                                        }
                                    }
                                    drop(store);

                                    // Mark the new peer as online (they just connected to us)
                                    {
                                        let mut status_map = peer_status.write().await;
                                        let status = status_map.entry(new_peer.id.clone()).or_default();
                                        status.online = true;
                                        status.last_ping = Some(std::time::Instant::now());
                                    }

                                    // Close connection gracefully
                                    let _ = conn.close().await;

                                    // Send success result
                                    let _ = pending.result_tx.send(Ok(new_peer));

                                    // Update peers UI with online status
                                    update_peers_ui_with_status(&window_weak, &peer_store, &peer_status).await;
                                    continue;
                                } else {
                                    warn!("Invalid nonce from {}: expected {}, got {}", remote_id, pending.nonce, nonce);
                                    // Put the pending trust back since nonce didn't match
                                    *pending_guard = Some(pending);
                                    let response = ControlMessage::TrustRevoke {
                                        reason: "invalid nonce".to_string(),
                                    };
                                    let _ = conn.send(&response).await;
                                    let _ = conn.close().await;
                                    continue;
                                }
                            } else {
                                warn!("Received TrustConfirm but no pending trust");
                                let _ = conn.close().await;
                                continue;
                            }
                        } else {
                            warn!("Connection from unknown peer {}, ignoring", remote_id);
                            let _ = conn.close().await;
                            continue;
                        }
                    }
                    let peer = peer.unwrap();

                    // Handle different message types
                    match &msg {
                        ControlMessage::PushOffer { files, total_size, .. } => {
                            info!(
                                "Received push offer from {} with {} files ({} bytes)",
                                peer.name, files.len(), total_size
                            );

                            // Check if we allow push from this peer
                            if !peer.permissions_granted.push {
                                warn!("Push not allowed from {}", peer.name);
                                let _ = conn.send(&ControlMessage::PushResponse {
                                    transfer_id: String::new(),
                                    accepted: false,
                                    reason: Some("push not allowed".to_string()),
                                }).await;
                                continue;
                            }

                            // Get download directory
                            let download_dir = {
                                let cfg = config.read().await;
                                cfg.download_dir.clone()
                            };

                            // Create transfer for tracking
                            let file_names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
                            let transfer = Transfer::new_iroh_pull(
                                file_names,
                                peer.endpoint_id.clone(),
                                peer.name.clone(),
                            );
                            let transfer_id = transfer.id.clone();
                            let _ = transfer_manager.add(transfer).await;

                            // Update UI
                            update_transfers_ui(&window_weak, &transfer_manager).await;
                            update_status(&window_weak, &format!("Receiving files from {}...", peer.name));

                            // Create progress channel
                            let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<TransferEvent>(100);

                            // Spawn progress handler
                            let transfer_manager_progress = transfer_manager.clone();
                            let transfer_history_progress = transfer_history.clone();
                            let window_weak_progress = window_weak.clone();
                            let transfer_id_progress = transfer_id.clone();
                            let peer_name = peer.name.clone();
                            tokio::spawn(async move {
                                while let Some(event) = progress_rx.recv().await {
                                    match event {
                                        TransferEvent::Started { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Running;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::Progress { transferred, total, speed, .. } => {
                                            let progress = if total > 0 { transferred as f64 / total as f64 * 100.0 } else { 0.0 };
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.progress = progress;
                                                t.speed = speed.clone();
                                                t.transferred = transferred;
                                                t.total_size = total;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::FileComplete { file, .. } => {
                                            info!("File received: {}", file);
                                        }
                                        TransferEvent::Complete { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Completed;
                                                t.progress = 100.0;
                                                t.completed_at = Some(chrono::Utc::now());
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Received files from {}", peer_name));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                        }
                                        TransferEvent::Failed { error, .. } => {
                                            error!("Transfer failed: {}", error);
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Failed;
                                                t.error = Some(error.clone());
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Receive failed: {}", error));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                        }
                                        TransferEvent::Cancelled { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Cancelled;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                        }
                                    }
                                }
                            });

                            // Handle the incoming push
                            match handle_incoming_push(
                                &mut conn,
                                &remote_id,
                                msg,
                                &download_dir,
                                progress_tx,
                            ).await {
                                Ok(_) => {
                                    info!("Successfully received files from {}", peer.name);
                                }
                                Err(e) => {
                                    error!("Failed to receive files from {}: {}", peer.name, e);
                                    let _ = transfer_manager.update(&transfer_id, |t| {
                                        t.status = TransferStatus::Failed;
                                        t.error = Some(e.to_string());
                                    }).await;
                                    update_transfers_ui(&window_weak, &transfer_manager).await;
                                }
                            }
                        }
                        ControlMessage::TrustConfirm { .. } => {
                            // This is a trust handshake, not a transfer
                            // The dedicated trust handler should deal with this
                            info!("Received TrustConfirm in background listener, ignoring (handled elsewhere)");
                        }
                        ControlMessage::BrowseRequest { path } => {
                            info!("Received browse request from {} for path: {:?}", peer.name, path);

                            // Check if we allow browse from this peer
                            if !peer.permissions_granted.browse {
                                warn!("Browse not allowed from {}", peer.name);
                                let _ = conn.send(&ControlMessage::BrowseResponse {
                                    path: path.clone().unwrap_or_else(|| "/".to_string()),
                                    entries: vec![],
                                    error: Some("browse not allowed".to_string()),
                                }).await;
                                continue;
                            }

                            // Get browse settings from config
                            let cfg = config.read().await;
                            let browse_settings = cfg.browse_settings.clone();
                            let allowed_paths = if browse_settings.allowed_paths.is_empty() {
                                default_browsable_paths()
                            } else {
                                browse_settings.allowed_paths.clone()
                            };
                            drop(cfg);
                            match handle_browse_request(&mut conn, path.clone(), Some(&allowed_paths), &browse_settings).await {
                                Ok(_) => {
                                    info!("Browse request handled successfully for {}", peer.name);
                                }
                                Err(e) => {
                                    error!("Browse request failed for {}: {}", peer.name, e);
                                }
                            }
                            // Close connection gracefully to ensure response is sent
                            let _ = conn.close().await;
                        }
                        ControlMessage::PullRequest { files, .. } => {
                            info!("Received pull request from {} for {} files", peer.name, files.len());

                            // Check if we allow pull from this peer
                            if !peer.permissions_granted.pull {
                                warn!("Pull not allowed from {}", peer.name);
                                let _ = conn.send(&ControlMessage::PullResponse {
                                    transfer_id: String::new(),
                                    files: vec![],
                                    granted: false,
                                    reason: Some("pull not allowed".to_string()),
                                }).await;
                                continue;
                            }

                            // Create transfer for tracking (outgoing for us)
                            let file_names: Vec<String> = files.iter().map(|f| {
                                std::path::Path::new(&f.path)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(&f.path)
                                    .to_string()
                            }).collect();
                            let transfer = Transfer::new_iroh_push(
                                file_names,
                                peer.endpoint_id.clone(),
                                peer.name.clone(),
                            );
                            let transfer_id = transfer.id.clone();
                            let _ = transfer_manager.add(transfer).await;

                            // Update UI
                            update_transfers_ui(&window_weak, &transfer_manager).await;
                            update_status(&window_weak, &format!("Sending files to {} (pull)...", peer.name));

                            // Create progress channel
                            let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<TransferEvent>(100);

                            // Spawn progress handler
                            let transfer_manager_progress = transfer_manager.clone();
                            let transfer_history_progress = transfer_history.clone();
                            let window_weak_progress = window_weak.clone();
                            let transfer_id_progress = transfer_id.clone();
                            let peer_name = peer.name.clone();
                            tokio::spawn(async move {
                                while let Some(event) = progress_rx.recv().await {
                                    match event {
                                        TransferEvent::Started { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Running;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::Progress { transferred, total, speed, .. } => {
                                            let progress = if total > 0 { transferred as f64 / total as f64 * 100.0 } else { 0.0 };
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.progress = progress;
                                                t.speed = speed.clone();
                                                t.transferred = transferred;
                                                t.total_size = total;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::FileComplete { file, .. } => {
                                            info!("File sent: {}", file);
                                        }
                                        TransferEvent::Complete { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Completed;
                                                t.progress = 100.0;
                                                t.completed_at = Some(chrono::Utc::now());
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Pull completed for {}", peer_name));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                        }
                                        TransferEvent::Failed { error, .. } => {
                                            error!("Pull transfer failed: {}", error);
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Failed;
                                                t.error = Some(error.clone());
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Pull failed: {}", error));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                        }
                                        TransferEvent::Cancelled { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Cancelled;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                        }
                                    }
                                }
                            });

                            // Use default browsable paths for validation
                            let allowed_paths = default_browsable_paths();
                            match handle_incoming_pull(
                                &mut conn,
                                msg,
                                Some(&allowed_paths),
                                progress_tx,
                            ).await {
                                Ok(_) => {
                                    info!("Successfully sent files to {} (pull)", peer.name);
                                }
                                Err(e) => {
                                    error!("Failed to handle pull from {}: {}", peer.name, e);
                                    let _ = transfer_manager.update(&transfer_id, |t| {
                                        t.status = TransferStatus::Failed;
                                        t.error = Some(e.to_string());
                                    }).await;
                                    update_transfers_ui(&window_weak, &transfer_manager).await;
                                }
                            }
                        }
                        ControlMessage::PermissionsUpdate { permissions } => {
                            info!("Received permissions update from {}: push={}, pull={}, browse={}",
                                  peer.name, permissions.push, permissions.pull, permissions.browse);

                            // Update our record of what they allow us to do
                            {
                                let mut store = peer_store.write().await;
                                if let Err(e) = store.update(&peer.id, |p| {
                                    p.their_permissions.push = permissions.push;
                                    p.their_permissions.pull = permissions.pull;
                                    p.their_permissions.browse = permissions.browse;
                                    p.their_permissions.status = permissions.status;
                                }) {
                                    error!("Failed to update peer permissions in store: {}", e);
                                }
                            }

                            // Mark peer as online since they just connected to us
                            {
                                let mut status_map = peer_status.write().await;
                                let status = status_map.entry(peer.id.clone()).or_default();
                                status.online = true;
                                status.last_ping = Some(std::time::Instant::now());
                            }

                            // Update UI with status preserved
                            update_peers_ui_with_status(&window_weak, &peer_store, &peer_status).await;
                            update_status(&window_weak, &format!("{} updated permissions", peer.name));

                            // Close connection gracefully
                            let _ = conn.close().await;
                        }
                        ControlMessage::Ping { timestamp } => {
                            debug!("Received ping from {}, responding with pong", peer.name);
                            let _ = conn.send(&ControlMessage::Pong { timestamp: *timestamp }).await;
                            let _ = conn.close().await;
                        }
                        ControlMessage::Pong { .. } => {
                            // Pong received, connection will be closed by caller
                            debug!("Received pong from {}", peer.name);
                            let _ = conn.close().await;
                        }
                        ControlMessage::StatusRequest => {
                            debug!("Received status request from {}", peer.name);
                            let cfg = config.read().await;
                            let download_dir = cfg.download_dir.to_string_lossy().to_string();
                            drop(cfg);

                            let hostname = std::env::var("HOSTNAME")
                                .or_else(|_| std::env::var("COMPUTERNAME"))
                                .unwrap_or_else(|_| "unknown".to_string());

                            let _ = conn.send(&ControlMessage::StatusResponse {
                                hostname,
                                os: std::env::consts::OS.to_string(),
                                free_space: 0, // TODO: Get actual free space
                                download_dir,
                                uptime: 0, // TODO: Track uptime
                                version: env!("CARGO_PKG_VERSION").to_string(),
                                active_transfers: 0, // Simplified for now
                            }).await;
                            let _ = conn.close().await;
                        }
                        ControlMessage::DndStatus { enabled, mode, until: _, message } => {
                            info!("Received DND status from {}: enabled={}, mode={}", peer.name, enabled, mode);

                            // Update peer's DND status in UI
                            let peer_id = peer.id.clone();
                            let dnd_mode = mode.clone();
                            let dnd_message = message.clone().unwrap_or_default();
                            let _ = slint::invoke_from_event_loop({
                                let window_weak = window_weak.clone();
                                move || {
                                    if let Some(window) = window_weak.upgrade() {
                                        // Find and update the peer in the model
                                        let model = window.global::<AppLogic>().get_peers();
                                        for i in 0..model.row_count() {
                                            if let Some(mut item) = model.row_data(i) {
                                                if item.id.as_str() == peer_id {
                                                    item.peer_dnd_mode = SharedString::from(&dnd_mode);
                                                    item.peer_dnd_message = SharedString::from(&dnd_message);
                                                    model.set_row_data(i, item);
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                            let _ = conn.close().await;
                        }
                        ControlMessage::SpeedTestRequest { test_id, size } => {
                            info!("Received speed test request from {}: test_id={}, size={}", peer.name, test_id, size);

                            // Handle the speed test request
                            match croh_core::handle_speed_test_request(&mut conn, test_id.clone(), *size).await {
                                Ok(result) => {
                                    info!(
                                        "Speed test handled for {}: upload={}, download={}, latency={}ms",
                                        peer.name,
                                        result.upload_speed_formatted(),
                                        result.download_speed_formatted(),
                                        result.latency_ms
                                    );
                                }
                                Err(e) => {
                                    error!("Speed test handling failed for {}: {}", peer.name, e);
                                }
                            }
                            let _ = conn.close().await;
                        }
                        other => {
                            warn!("Unexpected message type from {}: {:?}", remote_id, other);
                        }
                    }
                }

                let _ = endpoint.close().await;
                info!("Background listener closed");
            });
        });
    }

    /// Initialize settings in the UI.
    fn init_settings(&self, _window: &MainWindow) {
        let config = self.config.clone();
        let window_weak = self.window.clone();

        // Load settings in background thread
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let config_guard = config.read().await;

                // Check if croc is found
                let (croc_path, croc_found) = match find_croc_executable() {
                    Ok(path) => (path.to_string_lossy().to_string(), true),
                    Err(_) => ("Not found".to_string(), false),
                };

                let download_dir = config_guard.download_dir.to_string_lossy().to_string();
                let default_relay = config_guard.default_relay.clone().unwrap_or_default();
                let theme = config_guard.theme.to_ui_string();

                // Transfer options
                let hash_algorithm = config_guard.default_hash
                    .map(|h| h.as_str().to_string())
                    .unwrap_or_default();
                let curve = config_guard.default_curve
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_default();
                let throttle = config_guard.throttle.clone().unwrap_or_default();
                let no_local = config_guard.no_local;

                // Browse settings
                let browse_show_hidden = config_guard.browse_settings.show_hidden;
                let browse_show_protected = config_guard.browse_settings.show_protected;
                let browse_exclude_patterns = config_guard.browse_settings.exclude_patterns.join(", ");

                // DND settings
                let dnd_mode = config_guard.dnd_mode.to_ui_string().to_string();
                let dnd_message = config_guard.dnd_message.clone().unwrap_or_default();

                drop(config_guard);

                // Update UI on main thread
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = window_weak.upgrade() {
                        let settings = AppSettings {
                            download_dir: SharedString::from(download_dir),
                            default_relay: SharedString::from(default_relay),
                            theme: SharedString::from(theme),
                            croc_path: SharedString::from(croc_path),
                            croc_found,
                            hash_algorithm: SharedString::from(hash_algorithm),
                            curve: SharedString::from(curve),
                            throttle: SharedString::from(throttle),
                            no_local,
                            browse_show_hidden,
                            browse_show_protected,
                            browse_exclude_patterns: SharedString::from(browse_exclude_patterns),
                            dnd_mode: SharedString::from(dnd_mode),
                            dnd_message: SharedString::from(dnd_message),
                        };
                        window.global::<AppLogic>().set_settings(settings);
                    }
                });
            });
        });
    }

    /// Initialize identity and load peers.
    fn init_identity_and_peers(&self, window: &MainWindow) {
        // Load peers synchronously into shared model (Rc can't cross thread boundary)
        // We use try_read() to avoid blocking, falling back to empty if locked
        let rt = tokio::runtime::Runtime::new().unwrap();
        let peer_store = self.peer_store.clone();
        let items: Vec<PeerItem> = rt.block_on(async {
            let peers = peer_store.read().await;
            peers.list().iter().map(trusted_peer_to_item).collect()
        });
        self.refresh_peers_model(items);

        // Load identity in background thread
        let identity_arc = self.identity.clone();
        let window_weak = self.window.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                // Load or create identity
                match Identity::load_or_create() {
                    Ok(id) => {
                        let endpoint_id = id.endpoint_id.clone();
                        // Truncate for display
                        let display_id = if endpoint_id.len() > 12 {
                            format!("{}...{}", &endpoint_id[..6], &endpoint_id[endpoint_id.len()-6..])
                        } else {
                            endpoint_id
                        };

                        *identity_arc.write().await = Some(id);
                        info!("Identity loaded");

                        // Update UI with endpoint ID
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                window.global::<AppLogic>().set_endpoint_id(SharedString::from(display_id));
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to load identity: {}", e);
                    }
                }
            });
        });

        // Mark window as used
        let _ = window;
    }

    fn setup_file_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let selected_files = self.selected_files.clone();

        // Browse files callback
        window.global::<AppLogic>().on_browse_files({
            let window_weak = window_weak.clone();
            let selected_files = selected_files.clone();

            move || {
                let window_weak = window_weak.clone();
                let selected_files = selected_files.clone();

                // Use rfd for native file dialog
                std::thread::spawn(move || {
                    let files = rfd::FileDialog::new()
                        .set_title("Select files to send")
                        .pick_files();

                    if let Some(paths) = files {
                        // Process selected files
                        let mut new_files = Vec::new();
                        for path in paths {
                            if let Ok(metadata) = std::fs::metadata(&path) {
                                let name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let size = metadata.len();
                                let path_str = path.to_string_lossy().to_string();

                                new_files.push(SelectedFileData {
                                    name,
                                    size,
                                    path: path_str,
                                });
                            }
                        }

                        // Update state and UI
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();

                        rt.block_on(async {
                            let mut files_guard = selected_files.write().await;
                            files_guard.extend(new_files);
                            // Convert to sendable data
                            let files_data: Vec<_> = files_guard.iter().map(|f| {
                                (f.name.clone(), files::format_size(f.size), f.path.clone())
                            }).collect();
                            drop(files_guard);

                            // Update UI on main thread
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let items: Vec<SelectedFile> = files_data.into_iter()
                                        .map(|(name, size, path)| SelectedFile {
                                            name: SharedString::from(name),
                                            size: SharedString::from(size),
                                            path: SharedString::from(path),
                                        })
                                        .collect();
                                    let model = ModelRc::new(VecModel::from(items));
                                    window.global::<AppLogic>().set_selected_files(model);
                                }
                            });
                        });
                    }
                });
            }
        });

        // Clear files callback
        window.global::<AppLogic>().on_clear_files({
            let window_weak = window_weak.clone();
            let selected_files = selected_files.clone();

            move || {
                let selected_files = selected_files.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut files_guard = selected_files.write().await;
                        files_guard.clear();
                        drop(files_guard);

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let model: ModelRc<SelectedFile> =
                                    ModelRc::new(VecModel::from(Vec::new()));
                                window.global::<AppLogic>().set_selected_files(model);
                            }
                        });
                    });
                });
            }
        });

        // Remove file callback
        window.global::<AppLogic>().on_remove_file({
            let window_weak = window_weak.clone();
            let selected_files = selected_files.clone();

            move |index| {
                let selected_files = selected_files.clone();
                let window_weak = window_weak.clone();
                let index = index as usize;

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut files_guard = selected_files.write().await;
                        if index < files_guard.len() {
                            files_guard.remove(index);
                            // Convert to sendable data
                            let files_data: Vec<_> = files_guard.iter().map(|f| {
                                (f.name.clone(), files::format_size(f.size), f.path.clone())
                            }).collect();
                            drop(files_guard);

                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let items: Vec<SelectedFile> = files_data.into_iter()
                                        .map(|(name, size, path)| SelectedFile {
                                            name: SharedString::from(name),
                                            size: SharedString::from(size),
                                            path: SharedString::from(path),
                                        })
                                        .collect();
                                    let model = ModelRc::new(VecModel::from(items));
                                    window.global::<AppLogic>().set_selected_files(model);
                                }
                            });
                        }
                    });
                });
            }
        });
    }

    fn setup_transfer_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let selected_files = self.selected_files.clone();
        let transfer_manager = self.transfer_manager.clone();
        let transfer_history = self.transfer_history.clone();
        let active_processes = self.active_processes.clone();
        let config = self.config.clone();
        let identity = self.identity.clone();
        let peer_store = self.peer_store.clone();

        // Start send callback
        window.global::<AppLogic>().on_start_send({
            let window_weak = window_weak.clone();
            let selected_files = selected_files.clone();
            let transfer_manager = transfer_manager.clone();
            let transfer_history = transfer_history.clone();
            let active_processes = active_processes.clone();
            let config = config.clone();

            move |custom_code| {
                let custom_code = custom_code.to_string();
                info!("Start send requested with custom_code: {:?}", if custom_code.is_empty() { "auto" } else { &custom_code });
                let window_weak = window_weak.clone();
                let selected_files = selected_files.clone();
                let transfer_manager = transfer_manager.clone();
                let transfer_history = transfer_history.clone();
                let active_processes = active_processes.clone();
                let config = config.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get selected files
                        let files_guard = selected_files.read().await;
                        if files_guard.is_empty() {
                            warn!("No files selected");
                            return;
                        }

                        let file_paths: Vec<PathBuf> =
                            files_guard.iter().map(|f| PathBuf::from(&f.path)).collect();
                        let file_names: Vec<String> =
                            files_guard.iter().map(|f| f.name.clone()).collect();
                        drop(files_guard);

                        // Create transfer
                        let mut transfer = Transfer::new_send(file_names.clone());
                        let transfer_id = transfer.id.clone();
                        transfer.status = TransferStatus::Running;

                        if let Err(e) = transfer_manager.add(transfer).await {
                            error!("Failed to add transfer: {}", e);
                            return;
                        }

                        // Update UI
                        update_transfers_ui(&window_weak, &transfer_manager).await;
                        update_status(&window_weak, "Starting send...");

                        // Start croc process with options from config
                        let config_guard = config.read().await;
                        let mut options = CrocOptions::new();

                        // Apply custom code if provided
                        if !custom_code.is_empty() {
                            options = options.with_code(custom_code);
                        }

                        // Apply hash algorithm from config, default to md5 (xxhash can hang on large files)
                        let hash = config_guard.default_hash.unwrap_or(HashAlgorithm::Md5);
                        options = options.with_hash(hash);

                        // Apply curve from config
                        if let Some(curve) = config_guard.default_curve {
                            options = options.with_curve(curve);
                        }

                        // Apply throttle from config
                        if let Some(ref throttle) = config_guard.throttle {
                            if !throttle.is_empty() {
                                options = options.with_throttle(throttle.clone());
                            }
                        }

                        // Apply no-local from config
                        if config_guard.no_local {
                            options = options.with_no_local(true);
                        }

                        // Apply relay from config
                        if let Some(ref relay) = config_guard.default_relay {
                            if !relay.is_empty() {
                                info!("Using custom relay: {}", relay);
                                options = options.with_relay(relay.clone());
                            }
                        }
                        drop(config_guard);

                        match CrocProcess::send(&file_paths, &options).await {
                            Ok((mut process, _handle)) => {
                                let id_str = transfer_id.to_string();

                                // Monitor process events
                                loop {
                                    match process.next_event().await {
                                        Some(CrocEvent::CodeReady(code)) => {
                                            info!("Code ready: {}", code);
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.code = Some(code.clone());
                                                })
                                                .await;
                                            update_status(
                                                &window_weak,
                                                &format!("Code: {}", code),
                                            );
                                            update_transfers_ui(&window_weak, &transfer_manager)
                                                .await;
                                        }
                                        Some(CrocEvent::Progress(p)) => {
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.progress = p.percentage;
                                                    t.speed = p.speed.clone();
                                                })
                                                .await;
                                            update_transfers_ui(&window_weak, &transfer_manager)
                                                .await;
                                        }
                                        Some(CrocEvent::Completed) => {
                                            info!("Transfer completed");
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Completed;
                                                    t.progress = 100.0;
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(&window_weak, "Transfer completed!");
                                            update_transfers_ui(&window_weak, &transfer_manager)
                                                .await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;

                                            // Clear selected files
                                            let mut files_guard = selected_files.write().await;
                                            files_guard.clear();
                                            if let Some(window) = window_weak.upgrade() {
                                                let model: ModelRc<SelectedFile> =
                                                    ModelRc::new(VecModel::from(Vec::new()));
                                                window
                                                    .global::<AppLogic>()
                                                    .set_selected_files(model);
                                            }
                                            break;
                                        }
                                        Some(CrocEvent::Failed(err)) => {
                                            error!("Transfer failed: {}", err);
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Failed;
                                                    t.error = Some(err.clone());
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(
                                                &window_weak,
                                                &format!("Failed: {}", err),
                                            );
                                            update_transfers_ui(&window_weak, &transfer_manager)
                                                .await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;
                                            break;
                                        }
                                        Some(CrocEvent::Output(line)) => {
                                            // Debug output
                                            info!("croc: {}", line);
                                        }
                                        Some(_) => {}
                                        None => {
                                            // Channel closed, check process status
                                            break;
                                        }
                                    }
                                }

                                // Check if transfer completed via process exit
                                let current_status = transfer_manager
                                    .get(&transfer_id)
                                    .await
                                    .map(|t| t.status.clone());

                                // If still running, check process exit status
                                if current_status == Some(TransferStatus::Running) {
                                    match process.wait().await {
                                        Ok(status) if status.success() => {
                                            info!("Send process exited successfully");
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Completed;
                                                    t.progress = 100.0;
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(&window_weak, "Transfer completed!");
                                            update_transfers_ui(&window_weak, &transfer_manager).await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;

                                            // Clear selected files
                                            let mut files_guard = selected_files.write().await;
                                            files_guard.clear();
                                            let _ = slint::invoke_from_event_loop({
                                                let window_weak = window_weak.clone();
                                                move || {
                                                    if let Some(window) = window_weak.upgrade() {
                                                        let model: ModelRc<SelectedFile> =
                                                            ModelRc::new(VecModel::from(Vec::new()));
                                                        window.global::<AppLogic>().set_selected_files(model);
                                                    }
                                                }
                                            });
                                        }
                                        Ok(status) => {
                                            warn!("Send process exited with status: {:?}", status);
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Failed;
                                                    t.error = Some("Transfer failed".to_string());
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(&window_weak, "Transfer failed");
                                            update_transfers_ui(&window_weak, &transfer_manager).await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;
                                        }
                                        Err(e) => {
                                            error!("Failed to get process status: {}", e);
                                        }
                                    }
                                }

                                // Remove from active processes
                                active_processes.write().await.remove(&id_str);
                            }
                            Err(e) => {
                                error!("Failed to start croc: {}", e);
                                let _ = transfer_manager
                                    .update(&transfer_id, |t| {
                                        t.status = TransferStatus::Failed;
                                        t.error = Some(e.to_string());
                                    })
                                    .await;
                                update_status(&window_weak, &format!("Error: {}", e));
                                update_transfers_ui(&window_weak, &transfer_manager).await;
                            }
                        }
                    });
                });
            }
        });

        // Start receive callback
        window.global::<AppLogic>().on_start_receive({
            let window_weak = window_weak.clone();
            let transfer_manager = transfer_manager.clone();
            let transfer_history = transfer_history.clone();
            let active_processes = active_processes.clone();
            let config = config.clone();
            let identity = identity.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = self.shared_endpoint.clone();
            let peer_status = self.peer_status.clone();

            move |code| {
                // Sanitize code: remove nul bytes and other control characters
                let code: String = code.chars()
                    .filter(|c| !c.is_control() || *c == ' ')
                    .collect();
                let code = code.trim().to_string();

                if code.is_empty() {
                    warn!("Empty code provided");
                    return;
                }

                info!("Start receive requested with code: {}", code);

                let window_weak = window_weak.clone();
                let transfer_manager = transfer_manager.clone();
                let transfer_history = transfer_history.clone();
                let active_processes = active_processes.clone();
                let config = config.clone();
                let identity = identity.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let peer_status = peer_status.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get download directory and relay from config
                        let config_guard = config.read().await;
                        let download_dir = config_guard.download_dir.clone();
                        let default_relay = config_guard.default_relay.clone();
                        drop(config_guard);

                        // Ensure download directory exists
                        if let Err(e) = std::fs::create_dir_all(&download_dir) {
                            error!("Failed to create download dir: {}", e);
                            update_status(&window_weak, &format!("Error: {}", e));
                            return;
                        }

                        // Canonicalize the path to ensure it's valid
                        let download_dir = match std::fs::canonicalize(&download_dir) {
                            Ok(p) => {
                                // On Windows, canonicalize returns paths with \\?\ prefix
                                // which can cause issues with some programs. Strip it if present.
                                #[cfg(target_os = "windows")]
                                {
                                    let path_str = p.to_string_lossy();
                                    if path_str.starts_with(r"\\?\") {
                                        PathBuf::from(&path_str[4..])
                                    } else {
                                        p
                                    }
                                }
                                #[cfg(not(target_os = "windows"))]
                                p
                            }
                            Err(e) => {
                                error!("Failed to canonicalize download dir: {}", e);
                                update_status(&window_weak, &format!("Invalid path: {}", e));
                                return;
                            }
                        };

                        info!("Using download directory: {:?}", download_dir);

                        // Create transfer
                        let mut transfer = Transfer::new_receive(code.clone());
                        let transfer_id = transfer.id.clone();
                        transfer.status = TransferStatus::Running;

                        if let Err(e) = transfer_manager.add(transfer).await {
                            error!("Failed to add transfer: {}", e);
                            return;
                        }

                        // Update UI
                        update_transfers_ui(&window_weak, &transfer_manager).await;
                        update_status(&window_weak, "Receiving...");

                        // Start croc process with options from config
                        let config_guard = config.read().await;
                        let mut options = CrocOptions::new();

                        // Apply hash algorithm from config, default to md5 (xxhash can hang on large files)
                        let hash = config_guard.default_hash.unwrap_or(HashAlgorithm::Md5);
                        options = options.with_hash(hash);

                        // Apply curve from config
                        if let Some(curve) = config_guard.default_curve {
                            options = options.with_curve(curve);
                        }

                        // Apply throttle from config
                        if let Some(ref throttle) = config_guard.throttle {
                            if !throttle.is_empty() {
                                options = options.with_throttle(throttle.clone());
                            }
                        }

                        // Apply relay from config
                        if let Some(ref relay) = default_relay {
                            if !relay.is_empty() {
                                info!("Using custom relay: {}", relay);
                                options = options.with_relay(relay.clone());
                            }
                        }
                        drop(config_guard);

                        match CrocProcess::receive(&code, &options, Some(&download_dir)).await {
                            Ok((mut process, _handle)) => {
                                let id_str = transfer_id.to_string();

                                // Monitor process events
                                loop {
                                    match process.next_event().await {
                                        Some(CrocEvent::Progress(p)) => {
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.progress = p.percentage;
                                                    t.speed = p.speed.clone();
                                                })
                                                .await;
                                            update_transfers_ui(&window_weak, &transfer_manager)
                                                .await;
                                        }
                                        Some(CrocEvent::Completed) => {
                                            info!("Receive completed");
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Completed;
                                                    t.progress = 100.0;
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(&window_weak, "Receive completed!");
                                            update_transfers_ui(&window_weak, &transfer_manager)
                                                .await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;

                                            // Check for trust bundles in received files
                                            check_and_handle_trust_bundle(&download_dir, &window_weak, &identity, &shared_endpoint, &peer_store, &peer_status).await;
                                            break;
                                        }
                                        Some(CrocEvent::Failed(err)) => {
                                            error!("Receive failed: {}", err);
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Failed;
                                                    t.error = Some(err.clone());
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(
                                                &window_weak,
                                                &format!("Failed: {}", err),
                                            );
                                            update_transfers_ui(&window_weak, &transfer_manager)
                                                .await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;
                                            break;
                                        }
                                        Some(CrocEvent::Output(line)) => {
                                            info!("croc: {}", line);
                                        }
                                        Some(_) => {}
                                        None => break,
                                    }
                                }

                                // Check if transfer completed via process exit
                                let current_status = transfer_manager
                                    .get(&transfer_id)
                                    .await
                                    .map(|t| t.status.clone());

                                // If still running, check process exit status
                                if current_status == Some(TransferStatus::Running) {
                                    match process.wait().await {
                                        Ok(status) if status.success() => {
                                            info!("Receive process exited successfully");
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Completed;
                                                    t.progress = 100.0;
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(&window_weak, "Receive completed!");
                                            update_transfers_ui(&window_weak, &transfer_manager).await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;

                                            // Check for trust bundles in received files
                                            check_and_handle_trust_bundle(&download_dir, &window_weak, &identity, &shared_endpoint, &peer_store, &peer_status).await;
                                        }
                                        Ok(status) => {
                                            warn!("Receive process exited with status: {:?}", status);
                                            let _ = transfer_manager
                                                .update(&transfer_id, |t| {
                                                    t.status = TransferStatus::Failed;
                                                    t.error = Some("Transfer failed".to_string());
                                                    t.completed_at = Some(chrono::Utc::now());
                                                })
                                                .await;
                                            update_status(&window_weak, "Receive failed");
                                            update_transfers_ui(&window_weak, &transfer_manager).await;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id).await;
                                        }
                                        Err(e) => {
                                            error!("Failed to get process status: {}", e);
                                        }
                                    }
                                }

                                // Remove from active processes
                                active_processes.write().await.remove(&id_str);
                            }
                            Err(e) => {
                                error!("Failed to start croc: {}", e);
                                let _ = transfer_manager
                                    .update(&transfer_id, |t| {
                                        t.status = TransferStatus::Failed;
                                        t.error = Some(e.to_string());
                                    })
                                    .await;
                                update_status(&window_weak, &format!("Error: {}", e));
                                update_transfers_ui(&window_weak, &transfer_manager).await;
                            }
                        }
                    });
                });
            }
        });

        // Cancel transfer callback
        window.global::<AppLogic>().on_cancel_transfer({
            let transfer_manager = transfer_manager.clone();
            let active_processes = active_processes.clone();
            let window_weak = window_weak.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Cancel transfer requested: {}", id_str);

                let transfer_manager = transfer_manager.clone();
                let active_processes = active_processes.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Abort the task if running
                        if let Some(handle) = active_processes.write().await.remove(&id_str) {
                            handle.abort();
                        }

                        // Update transfer status
                        let transfer_id = TransferId(id_str.clone());
                        let _ = transfer_manager
                            .update(&transfer_id, |t| {
                                t.status = TransferStatus::Cancelled;
                                t.completed_at = Some(chrono::Utc::now());
                            })
                            .await;

                        update_status(&window_weak, "Transfer cancelled");
                        update_transfers_ui(&window_weak, &transfer_manager).await;
                    });
                });
            }
        });

        // Remove transfer callback
        window.global::<AppLogic>().on_remove_transfer({
            let transfer_manager = transfer_manager.clone();
            let window_weak = window_weak.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Remove transfer requested: {}", id_str);

                let transfer_manager = transfer_manager.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let transfer_id = TransferId(id_str);
                        let _ = transfer_manager.remove(&transfer_id).await;
                        update_transfers_ui(&window_weak, &transfer_manager).await;
                    });
                });
            }
        });

        // Copy code callback
        window.global::<AppLogic>().on_copy_code({
            let window_weak = window_weak.clone();

            move |code| {
                let code_str = code.to_string();
                info!("Copying code to clipboard: {}", code_str);

                // Spawn a thread to handle clipboard - on Linux/Wayland, clipboard content
                // is only available while the source process holds it, so we need to keep
                // the clipboard alive for a bit
                let code_for_clipboard = code_str.clone();
                std::thread::spawn(move || {
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => {
                            if let Err(e) = clipboard.set_text(&code_for_clipboard) {
                                error!("Failed to copy to clipboard: {}", e);
                            } else {
                                // Keep clipboard alive for 30 seconds on Linux/Wayland
                                // This allows time for user to paste
                                #[cfg(target_os = "linux")]
                                std::thread::sleep(std::time::Duration::from_secs(30));
                            }
                        }
                        Err(e) => {
                            error!("Failed to access clipboard: {}", e);
                        }
                    }
                });

                // Update UI to show copied state
                if let Some(window) = window_weak.upgrade() {
                    window.global::<AppLogic>().set_copied_code(SharedString::from(code_str.clone()));

                    // Clear after 2 seconds
                    let window_weak = window_weak.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        if let Some(window) = window_weak.upgrade() {
                            window.global::<AppLogic>().set_copied_code(SharedString::from(""));
                        }
                    });
                }
            }
        });

        // Open folder callback
        window.global::<AppLogic>().on_open_folder({
            let config = config.clone();

            move |_id| {
                info!("Open folder requested");
                let config = config.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let config_guard = config.read().await;
                        let download_dir = config_guard.download_dir.clone();
                        drop(config_guard);

                        if let Err(e) = platform::open_in_explorer(&download_dir) {
                            error!("Failed to open folder: {}", e);
                        }
                    });
                });
            }
        });
    }

    fn setup_settings_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let config = self.config.clone();

        // Browse download directory
        window.global::<AppLogic>().on_browse_download_dir({
            let window_weak = window_weak.clone();
            let config = config.clone();

            move || {
                let window_weak = window_weak.clone();
                let config = config.clone();

                std::thread::spawn(move || {
                    let folder = rfd::FileDialog::new()
                        .set_title("Select Download Directory")
                        .pick_folder();

                    if let Some(path) = folder {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();

                        rt.block_on(async {
                            let mut config_guard = config.write().await;
                            config_guard.download_dir = path.clone();
                            drop(config_guard);

                            // Update UI
                            if let Some(window) = window_weak.upgrade() {
                                let mut settings = window.global::<AppLogic>().get_settings();
                                settings.download_dir = SharedString::from(path.to_string_lossy().to_string());
                                window.global::<AppLogic>().set_settings(settings);
                            }
                        });
                    }
                });
            }
        });

        // Save settings
        window.global::<AppLogic>().on_save_settings({
            let window_weak = window_weak.clone();
            let config = config.clone();

            move |download_dir, relay, theme, hash, curve, throttle, no_local| {
                let download_dir = download_dir.to_string();
                let relay = relay.to_string();
                let theme = theme.to_string();
                let hash = hash.to_string();
                let curve = curve.to_string();
                let throttle = throttle.to_string();

                info!("Saving settings: download_dir={}, relay={}, theme={}, hash={}, curve={}, throttle={}, no_local={}",
                      download_dir, relay, theme, hash, curve, throttle, no_local);

                let window_weak = window_weak.clone();
                let config = config.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut config_guard = config.write().await;

                        config_guard.download_dir = PathBuf::from(&download_dir);
                        config_guard.default_relay = if relay.is_empty() { None } else { Some(relay) };
                        config_guard.theme = Theme::from_ui_string(&theme);

                        // Save transfer options
                        config_guard.default_hash = match hash.as_str() {
                            "xxhash" => Some(HashAlgorithm::Xxhash),
                            "imohash" => Some(HashAlgorithm::Imohash),
                            "md5" => Some(HashAlgorithm::Md5),
                            _ => None,
                        };
                        config_guard.default_curve = match curve.as_str() {
                            "siec" => Some(Curve::Siec),
                            "p256" => Some(Curve::P256),
                            "p384" => Some(Curve::P384),
                            "p521" => Some(Curve::P521),
                            _ => None,
                        };
                        config_guard.throttle = if throttle.is_empty() { None } else { Some(throttle) };
                        config_guard.no_local = no_local;

                        // Save to file
                        if let Err(e) = config_guard.save() {
                            error!("Failed to save config: {}", e);
                            update_status(&window_weak, &format!("Failed to save: {}", e));
                        } else {
                            info!("Settings saved successfully");
                            update_status(&window_weak, "Settings saved");
                        }
                    });
                });
            }
        });

        // Open config folder
        window.global::<AppLogic>().on_open_config_folder({
            move || {
                info!("Opening config folder");
                let config_dir = platform::config_dir();

                // Ensure directory exists
                let _ = std::fs::create_dir_all(&config_dir);

                if let Err(e) = platform::open_in_explorer(&config_dir) {
                    error!("Failed to open config folder: {}", e);
                }
            }
        });

        // Refresh croc detection
        window.global::<AppLogic>().on_refresh_croc({
            let window_weak = window_weak.clone();

            move || {
                info!("Refreshing croc detection");
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    // Clear cache and re-detect
                    let (croc_path, croc_found) = match refresh_croc_cache() {
                        Ok(path) => (path.to_string_lossy().to_string(), true),
                        Err(_) => ("Not found".to_string(), false),
                    };

                    let croc_path_clone = croc_path.clone();

                    // Use invoke_from_event_loop to update UI on main thread
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = window_weak.upgrade() {
                            let mut settings = window.global::<AppLogic>().get_settings();
                            settings.croc_path = SharedString::from(croc_path_clone);
                            settings.croc_found = croc_found;
                            window.global::<AppLogic>().set_settings(settings);

                            if croc_found {
                                window.global::<AppLogic>().set_app_status(SharedString::from("Croc found!"));
                            } else {
                                window.global::<AppLogic>().set_app_status(SharedString::from("Croc not found"));
                            }
                        }
                    });
                });
            }
        });

        // Install croc - open GitHub releases page
        window.global::<AppLogic>().on_install_croc({
            move || {
                info!("Opening croc releases page");
                let url = "https://github.com/schollz/croc/releases";

                #[cfg(target_os = "windows")]
                {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", url])
                        .spawn();
                }

                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open")
                        .arg(url)
                        .spawn();
                }

                #[cfg(target_os = "linux")]
                {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(url)
                        .spawn();
                }
            }
        });

        // Live theme change callback
        window.global::<AppLogic>().on_set_theme({
            let window_weak = window_weak.clone();
            let config = config.clone();

            move |theme_str| {
                let theme_string = theme_str.to_string();
                let window_weak = window_weak.clone();
                let config = config.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Update config
                        {
                            let mut config_guard = config.write().await;
                            config_guard.theme = Theme::from_ui_string(&theme_string);
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save config: {}", e);
                            }
                        }

                        // Update UI settings to trigger theme change
                        let config_guard = config.read().await;
                        let theme = config_guard.theme.to_ui_string();
                        let download_dir = config_guard.download_dir.to_string_lossy().to_string();
                        let default_relay = config_guard.default_relay.clone().unwrap_or_default();
                        let hash_algorithm = config_guard.default_hash
                            .map(|h| h.as_str().to_string())
                            .unwrap_or_default();
                        let curve = config_guard.default_curve
                            .map(|c| c.as_str().to_string())
                            .unwrap_or_default();
                        let throttle = config_guard.throttle.clone().unwrap_or_default();
                        let no_local = config_guard.no_local;
                        let browse_show_hidden = config_guard.browse_settings.show_hidden;
                        let browse_show_protected = config_guard.browse_settings.show_protected;
                        let browse_exclude_patterns = config_guard.browse_settings.exclude_patterns.join(", ");
                        let dnd_mode = config_guard.dnd_mode.to_ui_string().to_string();
                        let dnd_message = config_guard.dnd_message.clone().unwrap_or_default();
                        let (croc_path, croc_found) = match find_croc_executable() {
                            Ok(path) => (path.to_string_lossy().to_string(), true),
                            Err(_) => ("Not found".to_string(), false),
                        };
                        drop(config_guard);

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let settings = AppSettings {
                                    download_dir: SharedString::from(download_dir),
                                    default_relay: SharedString::from(default_relay),
                                    theme: SharedString::from(theme),
                                    croc_path: SharedString::from(croc_path),
                                    croc_found,
                                    hash_algorithm: SharedString::from(hash_algorithm),
                                    curve: SharedString::from(curve),
                                    throttle: SharedString::from(throttle),
                                    no_local,
                                    browse_show_hidden,
                                    browse_show_protected,
                                    browse_exclude_patterns: SharedString::from(browse_exclude_patterns),
                                    dnd_mode: SharedString::from(dnd_mode),
                                    dnd_message: SharedString::from(dnd_message),
                                };
                                window.global::<AppLogic>().set_settings(settings);
                            }
                        });
                    });
                });
            }
        });

        // Auto-save settings callback (called when any setting changes)
        window.global::<AppLogic>().on_auto_save_settings({
            let config = config.clone();

            move |download_dir, relay, theme, hash, curve, throttle, no_local| {
                let download_dir = download_dir.to_string();
                let relay = relay.to_string();
                let theme = theme.to_string();
                let hash = hash.to_string();
                let curve = curve.to_string();
                let throttle = throttle.to_string();
                let config = config.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut config_guard = config.write().await;

                        // Update config values
                        if !download_dir.is_empty() {
                            config_guard.download_dir = PathBuf::from(&download_dir);
                        }
                        config_guard.default_relay = if relay.is_empty() { None } else { Some(relay) };
                        config_guard.theme = Theme::from_ui_string(&theme);
                        config_guard.default_hash = match hash.as_str() {
                            "xxhash" => Some(HashAlgorithm::Xxhash),
                            "imohash" => Some(HashAlgorithm::Imohash),
                            "md5" => Some(HashAlgorithm::Md5),
                            _ => None,
                        };
                        config_guard.default_curve = match curve.as_str() {
                            "siec" => Some(Curve::Siec),
                            "p256" => Some(Curve::P256),
                            "p384" => Some(Curve::P384),
                            "p521" => Some(Curve::P521),
                            _ => None,
                        };
                        config_guard.throttle = if throttle.is_empty() { None } else { Some(throttle) };
                        config_guard.no_local = no_local;

                        if let Err(e) = config_guard.save() {
                            error!("Failed to auto-save config: {}", e);
                        }
                    });
                });
            }
        });

        // Save browse settings callback
        window.global::<AppLogic>().on_save_browse_settings({
            let config = config.clone();
            let window_weak = window_weak.clone();

            move |show_hidden, show_protected, exclude_patterns| {
                let exclude_patterns = exclude_patterns.to_string();
                let config = config.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut config_guard = config.write().await;

                        // Update browse settings
                        config_guard.browse_settings.show_hidden = show_hidden;
                        config_guard.browse_settings.show_protected = show_protected;

                        // Parse comma-separated exclude patterns
                        config_guard.browse_settings.exclude_patterns = exclude_patterns
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();

                        if let Err(e) = config_guard.save() {
                            error!("Failed to save browse settings: {}", e);
                        }

                        // Update UI to reflect changes
                        let patterns_str = config_guard.browse_settings.exclude_patterns.join(", ");
                        let show_hidden = config_guard.browse_settings.show_hidden;
                        let show_protected = config_guard.browse_settings.show_protected;
                        drop(config_guard);

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let mut settings = window.global::<AppLogic>().get_settings();
                                settings.browse_show_hidden = show_hidden;
                                settings.browse_show_protected = show_protected;
                                settings.browse_exclude_patterns = SharedString::from(&patterns_str);
                                window.global::<AppLogic>().set_settings(settings);
                            }
                        });
                    });
                });
            }
        });

        // Set DND mode callback
        window.global::<AppLogic>().on_set_dnd_mode({
            let config = config.clone();
            let window_weak = window_weak.clone();
            let shared_endpoint = self.shared_endpoint.clone();
            let peer_store = self.peer_store.clone();

            move |mode, message| {
                let mode_str = mode.to_string();
                let message_str = message.to_string();
                let config = config.clone();
                let window_weak = window_weak.clone();
                let shared_endpoint = shared_endpoint.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Update config
                        {
                            let mut config_guard = config.write().await;
                            config_guard.dnd_mode = croh_core::DndMode::from_ui_string(&mode_str);
                            config_guard.dnd_message = if message_str.is_empty() { None } else { Some(message_str.clone()) };
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save DND settings: {}", e);
                            }
                        }

                        // Update UI
                        let mode_ui = mode_str.clone();
                        let message_ui = message_str.clone();
                        let _ = slint::invoke_from_event_loop({
                            let window_weak = window_weak.clone();
                            move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let mut settings = window.global::<AppLogic>().get_settings();
                                    settings.dnd_mode = SharedString::from(&mode_ui);
                                    settings.dnd_message = SharedString::from(&message_ui);
                                    window.global::<AppLogic>().set_settings(settings);
                                }
                            }
                        });

                        // Broadcast DND status to all online peers
                        let endpoint_opt = shared_endpoint.read().await.clone();
                        if let Some(endpoint) = endpoint_opt {
                            let store = peer_store.read().await;
                            for peer in store.list() {
                                // Try to connect and send DND status
                                match endpoint.connect(&peer.endpoint_id).await {
                                    Ok(mut conn) => {
                                        let msg = ControlMessage::DndStatus {
                                            enabled: mode_str != "off",
                                            mode: mode_str.clone(),
                                            until: None,
                                            message: if message_str.is_empty() { None } else { Some(message_str.clone()) },
                                        };
                                        if let Err(e) = conn.send(&msg).await {
                                            warn!("Failed to send DND status to {}: {}", peer.name, e);
                                        }
                                    }
                                    Err(e) => {
                                        // Peer may be offline, that's ok
                                        debug!("Failed to connect to {} for DND broadcast: {}", peer.name, e);
                                    }
                                }
                            }
                        }
                    });
                });
            }
        });
    }

    fn setup_peer_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let identity = self.identity.clone();
        let shared_endpoint = self.shared_endpoint.clone();
        let peer_store = self.peer_store.clone();
        let trust_in_progress = self.trust_in_progress.clone();
        let config = self.config.clone();
        let pending_trust = self.pending_trust.clone();
        let peer_status = self.peer_status.clone();
        let transfer_history = self.transfer_history.clone();

        // Initiate trust callback
        window.global::<AppLogic>().on_initiate_trust({
            let window_weak = window_weak.clone();
            let identity = identity.clone();
            let shared_endpoint = shared_endpoint.clone();
            let trust_in_progress = trust_in_progress.clone();
            let config = config.clone();
            let peer_store = peer_store.clone();
            let pending_trust = pending_trust.clone();
            let peer_status = peer_status.clone();

            move || {
                info!("Initiating trust...");
                let window_weak = window_weak.clone();
                let identity = identity.clone();
                let shared_endpoint = shared_endpoint.clone();
                let trust_in_progress = trust_in_progress.clone();
                let pending_trust = pending_trust.clone();
                let config = config.clone();
                let peer_store = peer_store.clone();
                let peer_status = peer_status.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Check if already in progress
                        {
                            let in_progress = trust_in_progress.read().await;
                            if *in_progress {
                                warn!("Trust initiation already in progress");
                                return;
                            }
                        }

                        // Get identity
                        let id = {
                            let id_guard = identity.read().await;
                            match id_guard.as_ref() {
                                Some(id) => id.clone(),
                                None => {
                                    error!("No identity available");
                                    update_status(&window_weak, "Error: No identity");
                                    return;
                                }
                            }
                        };

                        // Mark as in progress
                        *trust_in_progress.write().await = true;

                        // Update UI
                        {
                            let window_weak = window_weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(window) = window_weak.upgrade() {
                                    window.global::<AppLogic>().set_trust_in_progress(true);
                                    window.global::<AppLogic>().set_trust_code(SharedString::from(""));
                                }
                            });
                        }

                        // Wait for shared endpoint to be ready (created by background listener)
                        update_status(&window_weak, "Waiting for network...");
                        let wait_start = std::time::Instant::now();
                        let endpoint = loop {
                            {
                                let ep_guard = shared_endpoint.read().await;
                                if let Some(ep) = ep_guard.as_ref() {
                                    break ep.clone();
                                }
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            // Timeout after 30 seconds
                            if wait_start.elapsed() > std::time::Duration::from_secs(30) {
                                error!("Timeout waiting for shared endpoint");
                                *trust_in_progress.write().await = false;
                                let window_weak_err = window_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak_err.upgrade() {
                                        window.global::<AppLogic>().set_trust_in_progress(false);
                                    }
                                });
                                update_status(&window_weak, "Network not ready - please try again");
                                return;
                            }
                        };

                        // Get relay URL from the shared endpoint
                        update_status(&window_weak, "Getting relay info...");
                        let relay_url = endpoint.wait_for_relay(std::time::Duration::from_secs(10)).await
                            .map(|u| u.to_string());
                        info!("Using shared endpoint, relay URL: {:?}", relay_url);

                        if relay_url.is_none() {
                            warn!("No relay connection established - peer may have connectivity issues");
                        }

                        // Create trust bundle with the relay URL
                        let bundle = TrustBundle::new_with_relay(&id, relay_url);
                        let bundle_path = match bundle.save_to_temp() {
                            Ok(path) => path,
                            Err(e) => {
                                error!("Failed to save trust bundle: {}", e);
                                // Don't close the shared endpoint - it's used by other operations
                                *trust_in_progress.write().await = false;
                                let window_weak_err = window_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak_err.upgrade() {
                                        window.global::<AppLogic>().set_trust_in_progress(false);
                                    }
                                });
                                update_status(&window_weak, &format!("Error: {}", e));
                                return;
                            }
                        };

                        info!("Trust bundle saved to: {:?}", bundle_path);

                        // Start croc send with the bundle file
                        let config_guard = config.read().await;
                        let mut options = CrocOptions::new();

                        // Use md5 hash (more reliable)
                        options = options.with_hash(HashAlgorithm::Md5);

                        // Apply curve from config (must match receiver's curve)
                        if let Some(curve) = config_guard.default_curve {
                            options = options.with_curve(curve);
                        }

                        // Apply relay from config
                        if let Some(ref relay) = config_guard.default_relay {
                            if !relay.is_empty() {
                                options = options.with_relay(relay.clone());
                            }
                        }
                        drop(config_guard);

                        match CrocProcess::send(&[bundle_path.clone()], &options).await {
                            Ok((mut process, _handle)) => {
                                // Monitor for code
                                loop {
                                    match process.next_event().await {
                                        Some(CrocEvent::CodeReady(code)) => {
                                            info!("Trust code ready: {}", code);
                                            let window_weak_code = window_weak.clone();
                                            let code_clone = code.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(window) = window_weak_code.upgrade() {
                                                    window.global::<AppLogic>().set_trust_code(SharedString::from(code_clone));
                                                }
                                            });
                                            update_status(&window_weak, &format!("Share code: {}", code));
                                        }
                                        Some(CrocEvent::Completed) => {
                                            info!("Trust bundle sent successfully, waiting for peer to connect...");
                                            update_status(&window_weak, "Trust bundle sent, waiting for peer to connect...");

                                            // Set up pending trust with channel for background listener
                                            let bundle_nonce = bundle.nonce.clone();
                                            let (result_tx, result_rx) = oneshot::channel();

                                            {
                                                let mut pending_guard = pending_trust.write().await;
                                                *pending_guard = Some(PendingTrust {
                                                    nonce: bundle_nonce.clone(),
                                                    result_tx,
                                                });
                                            }

                                            info!("Set pending trust with nonce, waiting for background listener to handle connection...");

                                            // Wait for the background listener to complete the handshake
                                            let handshake_result = tokio::time::timeout(
                                                std::time::Duration::from_secs(120), // 2 minute timeout
                                                result_rx
                                            ).await;

                                            match handshake_result {
                                                Ok(Ok(Ok(peer))) => {
                                                    info!("Trust established with {}", peer.name);
                                                    update_status(&window_weak, &format!("Trusted peer added: {}", peer.name));

                                                    // Update peers UI with status (background listener already marked peer online)
                                                    update_peers_ui_with_status(&window_weak, &peer_store, &peer_status).await;
                                                }
                                                Ok(Ok(Err(e))) => {
                                                    error!("Handshake failed: {}", e);
                                                    update_status(&window_weak, &format!("Handshake failed: {}", e));
                                                }
                                                Ok(Err(_)) => {
                                                    // Channel was dropped without sending (shouldn't happen)
                                                    error!("Handshake channel dropped unexpectedly");
                                                    update_status(&window_weak, "Handshake failed unexpectedly");
                                                }
                                                Err(_) => {
                                                    warn!("Handshake timed out");
                                                    update_status(&window_weak, "Timed out waiting for peer");
                                                    // Clear the pending trust on timeout
                                                    let mut pending_guard = pending_trust.write().await;
                                                    *pending_guard = None;
                                                }
                                            }

                                            // Don't close the shared endpoint - it's used for background listening

                                            // Reset trust in progress
                                            *trust_in_progress.write().await = false;
                                            let window_weak_done = window_weak.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(window) = window_weak_done.upgrade() {
                                                    window.global::<AppLogic>().set_trust_in_progress(false);
                                                    window.global::<AppLogic>().set_trust_code(SharedString::from(""));
                                                }
                                            });
                                            break;
                                        }
                                        Some(CrocEvent::Failed(err)) => {
                                            error!("Trust bundle send failed: {}", err);
                                            *trust_in_progress.write().await = false;
                                            let window_weak_fail = window_weak.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(window) = window_weak_fail.upgrade() {
                                                    window.global::<AppLogic>().set_trust_in_progress(false);
                                                    window.global::<AppLogic>().set_trust_code(SharedString::from(""));
                                                }
                                            });
                                            update_status(&window_weak, &format!("Failed: {}", err));
                                            break;
                                        }
                                        Some(CrocEvent::Output(line)) => {
                                            info!("croc: {}", line);
                                        }
                                        Some(_) => {}
                                        None => {
                                            // Process ended
                                            *trust_in_progress.write().await = false;
                                            let window_weak_end = window_weak.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(window) = window_weak_end.upgrade() {
                                                    window.global::<AppLogic>().set_trust_in_progress(false);
                                                    window.global::<AppLogic>().set_trust_code(SharedString::from(""));
                                                }
                                            });
                                            break;
                                        }
                                    }
                                }

                                // Clean up temp file
                                let _ = std::fs::remove_file(&bundle_path);
                            }
                            Err(e) => {
                                error!("Failed to start croc for trust: {}", e);
                                *trust_in_progress.write().await = false;
                                let window_weak_start_err = window_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak_start_err.upgrade() {
                                        window.global::<AppLogic>().set_trust_in_progress(false);
                                    }
                                });
                                update_status(&window_weak, &format!("Error: {}", e));
                                // Clean up temp file
                                let _ = std::fs::remove_file(&bundle_path);
                            }
                        }
                    });
                });
            }
        });

        // Cancel trust callback
        window.global::<AppLogic>().on_cancel_trust({
            let window_weak = window_weak.clone();
            let trust_in_progress = trust_in_progress.clone();

            move || {
                info!("Cancelling trust initiation");
                let window_weak = window_weak.clone();
                let trust_in_progress = trust_in_progress.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        *trust_in_progress.write().await = false;

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                window.global::<AppLogic>().set_trust_in_progress(false);
                                window.global::<AppLogic>().set_trust_code(SharedString::from(""));
                            }
                        });
                    });
                });
            }
        });

        // Remove peer callback
        window.global::<AppLogic>().on_remove_peer({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let peers_model = self.peers_model.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Removing peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();

                // Remove from UI model immediately
                let count = peers_model.row_count();
                for i in 0..count {
                    if let Some(peer) = peers_model.row_data(i) {
                        if peer.id.to_string() == id_str {
                            peers_model.remove(i);
                            break;
                        }
                    }
                }

                // Remove from persistent store in background
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = peer_store.write().await;
                        if let Err(e) = store.remove(&id_str) {
                            error!("Failed to remove peer from store: {}", e);
                            update_status(&window_weak, &format!("Error: {}", e));
                            return;
                        }
                        update_status(&window_weak, "Peer removed");
                    });
                });
            }
        });

        // Disconnect peer callback - sends TrustRevoke and removes peer
        window.global::<AppLogic>().on_disconnect_peer({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = self.shared_endpoint.clone();
            let peers_model = self.peers_model.clone();
            let peer_status = self.peer_status.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Disconnecting from peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let peers_model = peers_model.clone();
                let peer_status = peer_status.clone();

                // Remove from UI model immediately
                let count = peers_model.row_count();
                for i in 0..count {
                    if let Some(peer) = peers_model.row_data(i) {
                        if peer.id.to_string() == id_str {
                            peers_model.remove(i);
                            break;
                        }
                    }
                }

                // Send TrustRevoke and remove from persistent store in background
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the peer before removing
                        let peer = {
                            let store = peer_store.read().await;
                            store.find_by_id(&id_str).cloned()
                        };

                        // Try to send TrustRevoke to the peer (best effort)
                        if let Some(peer) = peer {
                            let endpoint_guard = shared_endpoint.read().await;
                            if let Some(endpoint) = endpoint_guard.as_ref() {
                                match endpoint.connect(&peer.endpoint_id).await {
                                    Ok(mut conn) => {
                                        let revoke = ControlMessage::TrustRevoke {
                                            reason: "User disconnected".to_string(),
                                        };
                                        if let Err(e) = conn.send(&revoke).await {
                                            warn!("Failed to send TrustRevoke: {}", e);
                                        }
                                        let _ = conn.close().await;
                                        info!("Sent TrustRevoke to {}", peer.name);
                                    }
                                    Err(e) => {
                                        debug!("Could not connect to peer for TrustRevoke: {}", e);
                                    }
                                }
                            }
                        }

                        // Remove from persistent store
                        {
                            let mut store = peer_store.write().await;
                            if let Err(e) = store.remove(&id_str) {
                                error!("Failed to remove peer from store: {}", e);
                                update_status(&window_weak, &format!("Error: {}", e));
                                return;
                            }
                        }

                        // Remove from status map
                        {
                            let mut status_map = peer_status.write().await;
                            status_map.remove(&id_str);
                        }

                        update_status(&window_weak, "Disconnected from peer");
                    });
                });
            }
        });

        // Refresh peer status callback - triggers immediate ping and status query
        window.global::<AppLogic>().on_refresh_peer_status({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = self.shared_endpoint.clone();
            let peer_status = self.peer_status.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Refreshing status for peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let peer_status = peer_status.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the peer
                        let peer = {
                            let store = peer_store.read().await;
                            store.find_by_id(&id_str).cloned()
                        };

                        let peer = match peer {
                            Some(p) => p,
                            None => {
                                error!("Peer not found: {}", id_str);
                                return;
                            }
                        };

                        let endpoint_guard = shared_endpoint.read().await;
                        let endpoint = match endpoint_guard.as_ref() {
                            Some(ep) => ep.clone(),
                            None => {
                                error!("Endpoint not ready");
                                return;
                            }
                        };
                        drop(endpoint_guard);

                        // Ping the peer
                        let start = std::time::Instant::now();
                        let online = ping_peer(&endpoint, &peer).await;
                        let latency_ms = if online {
                            Some(start.elapsed().as_millis() as u32)
                        } else {
                            None
                        };

                        // Update status
                        {
                            let mut status_map = peer_status.write().await;
                            let status = status_map.entry(id_str.clone()).or_default();
                            status.online = online;
                            if online {
                                status.last_ping = Some(std::time::Instant::now());
                                status.last_contact = Some(chrono::Utc::now());
                                status.latency_ms = latency_ms;
                                status.ping_count += 1;

                                if let Some(lat) = latency_ms {
                                    status.latency_history.push(lat);
                                    if status.latency_history.len() > 10 {
                                        status.latency_history.remove(0);
                                    }
                                    let sum: u32 = status.latency_history.iter().sum();
                                    status.avg_latency_ms = Some(sum / status.latency_history.len() as u32);
                                }

                                status.connection_type = if peer.relay_url.is_some() {
                                    "relay".to_string()
                                } else {
                                    "direct".to_string()
                                };
                            }
                        }

                        // If online, also query StatusRequest for more info
                        if online {
                            match endpoint.connect(&peer.endpoint_id).await {
                                Ok(mut conn) => {
                                    if conn.send(&ControlMessage::StatusRequest).await.is_ok() {
                                        if let Ok(ControlMessage::StatusResponse {
                                            hostname, os, free_space, version, ..
                                        }) = conn.recv().await {
                                            let mut status_map = peer_status.write().await;
                                            if let Some(status) = status_map.get_mut(&id_str) {
                                                status.peer_hostname = Some(hostname);
                                                status.peer_os = Some(os);
                                                status.peer_version = Some(version);
                                                status.peer_free_space = Some(free_space);
                                            }
                                        }
                                    }
                                    let _ = conn.close().await;
                                }
                                Err(e) => {
                                    debug!("Could not connect for status: {}", e);
                                }
                            }
                        }

                        // Update UI
                        update_peers_ui_with_status(&window_weak, &peer_store, &peer_status).await;

                        if online {
                            update_status(&window_weak, &format!("Refreshed status for {}", peer.name));
                        } else {
                            update_status(&window_weak, &format!("{} is offline", peer.name));
                        }
                    });
                });
            }
        });

        // Run speed test callback
        window.global::<AppLogic>().on_run_speed_test({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = self.shared_endpoint.clone();
            let peer_status = self.peer_status.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Starting speed test for peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let peer_status = peer_status.clone();

                // Set speed_test_running to true in UI
                {
                    let window_weak_ui = window_weak.clone();
                    let id_str_ui = id_str.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = window_weak_ui.upgrade() {
                            let model = window.global::<AppLogic>().get_peers();
                            for i in 0..model.row_count() {
                                if let Some(mut item) = model.row_data(i) {
                                    if item.id.as_str() == id_str_ui {
                                        item.speed_test_running = true;
                                        model.set_row_data(i, item);
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the peer
                        let peer = {
                            let store = peer_store.read().await;
                            store.find_by_id(&id_str).cloned()
                        };

                        let peer = match peer {
                            Some(p) => p,
                            None => {
                                error!("Peer not found: {}", id_str);
                                return;
                            }
                        };

                        let endpoint_guard = shared_endpoint.read().await;
                        let endpoint = match endpoint_guard.as_ref() {
                            Some(ep) => ep.clone(),
                            None => {
                                error!("Endpoint not ready");
                                return;
                            }
                        };
                        drop(endpoint_guard);

                        // Connect to peer and run speed test
                        let result = match endpoint.connect(&peer.endpoint_id).await {
                            Ok(mut conn) => {
                                match croh_core::run_speed_test(&mut conn, croh_core::DEFAULT_TEST_SIZE).await {
                                    Ok(result) => {
                                        info!(
                                            "Speed test completed: upload={}, download={}, latency={}ms",
                                            result.upload_speed_formatted(),
                                            result.download_speed_formatted(),
                                            result.latency_ms
                                        );
                                        Some(result)
                                    }
                                    Err(e) => {
                                        error!("Speed test failed: {}", e);
                                        update_status(&window_weak, &format!("Speed test failed: {}", e));
                                        None
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Could not connect to peer for speed test: {}", e);
                                update_status(&window_weak, &format!("Could not connect: {}", e));
                                None
                            }
                        };

                        // Update peer status with results
                        if let Some(ref res) = result {
                            let mut status_map = peer_status.write().await;
                            let status = status_map.entry(id_str.clone()).or_default();
                            status.last_upload_speed = Some(res.upload_speed_formatted());
                            status.last_download_speed = Some(res.download_speed_formatted());
                        }

                        // Prepare status message before moving result
                        let status_msg = result.as_ref().map(|res| {
                            format!(
                                "Speed test: ↑{} ↓{} ({}ms)",
                                res.upload_speed_formatted(),
                                res.download_speed_formatted(),
                                res.latency_ms
                            )
                        });

                        // Update UI with results
                        let id_str_ui = id_str.clone();
                        let window_weak_final = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_final.upgrade() {
                                let model = window.global::<AppLogic>().get_peers();
                                for i in 0..model.row_count() {
                                    if let Some(mut item) = model.row_data(i) {
                                        if item.id.as_str() == id_str_ui {
                                            item.speed_test_running = false;
                                            if let Some(ref res) = result {
                                                item.speed_test_upload = SharedString::from(res.upload_speed_formatted());
                                                item.speed_test_download = SharedString::from(res.download_speed_formatted());
                                                item.speed_test_latency = SharedString::from(format!("{} ms", res.latency_ms));
                                            }
                                            model.set_row_data(i, item);
                                            break;
                                        }
                                    }
                                }
                            }
                        });

                        if let Some(msg) = status_msg {
                            update_status(&window_weak, &msg);
                        }
                    });
                });
            }
        });

        // Update peer permissions callback
        window.global::<AppLogic>().on_update_peer_permissions({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = self.shared_endpoint.clone();

            move |id, allow_push, allow_pull, allow_browse| {
                let id_str = id.to_string();
                info!("Updating permissions for peer {}: push={}, pull={}, browse={}",
                      id_str, allow_push, allow_pull, allow_browse);
                let window_weak_thread = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let id_str_thread = id_str.clone();

                // Spawn thread to update persistent store and notify peer (non-blocking)
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the peer's endpoint_id before updating
                        let endpoint_id = {
                            let store = peer_store.read().await;
                            store.find_by_id(&id_str_thread).map(|p| p.endpoint_id.clone())
                        };

                        // Update local store
                        {
                            let mut store = peer_store.write().await;
                            if let Err(e) = store.update(&id_str_thread, |peer| {
                                peer.permissions_granted.push = allow_push;
                                peer.permissions_granted.pull = allow_pull;
                                peer.permissions_granted.browse = allow_browse;
                            }) {
                                error!("Failed to update peer permissions: {}", e);
                                update_status(&window_weak_thread, &format!("Error: {}", e));
                                return;
                            }
                        }

                        // Notify the peer of the permission change
                        if let Some(endpoint_id) = endpoint_id {
                            let ep_guard = shared_endpoint.read().await;
                            if let Some(ref endpoint) = *ep_guard {
                                match endpoint.connect(&endpoint_id).await {
                                    Ok(mut conn) => {
                                        let permissions = Permissions {
                                            push: allow_push,
                                            pull: allow_pull,
                                            browse: allow_browse,
                                            status: true, // Always allow status
                                        };
                                        let msg = ControlMessage::PermissionsUpdate { permissions };
                                        if let Err(e) = conn.send(&msg).await {
                                            warn!("Failed to send permissions update to peer: {}", e);
                                        } else {
                                            info!("Sent permissions update to peer {}", endpoint_id);
                                        }
                                        let _ = conn.close().await;
                                    }
                                    Err(e) => {
                                        // Peer may be offline - that's ok, they'll get updated permissions
                                        // next time we connect or they connect to us
                                        debug!("Could not connect to peer to send permissions update: {}", e);
                                    }
                                }
                            }
                        }
                    });
                });

                // Update UI model by getting the current model from the window
                // (The model may have been replaced by update_peers_ui, so we need to get it fresh)
                if let Some(window) = window_weak.upgrade() {
                    let model = window.global::<AppLogic>().get_peers();
                    for i in 0..model.row_count() {
                        if let Some(peer) = model.row_data(i) {
                            if peer.id.to_string() == id_str {
                                let updated_peer = PeerItem {
                                    allow_push,
                                    allow_pull,
                                    allow_browse,
                                    ..peer
                                };
                                model.set_row_data(i, updated_peer);
                                break;
                            }
                        }
                    }
                }
            }
        });

        // Set peer expanded state callback
        window.global::<AppLogic>().on_set_peer_expanded({
            let window_weak = window_weak.clone();

            move |id, expanded| {
                let id_str = id.to_string();
                // Update UI model by getting the current model from the window
                if let Some(window) = window_weak.upgrade() {
                    let model = window.global::<AppLogic>().get_peers();
                    for i in 0..model.row_count() {
                        if let Some(peer) = model.row_data(i) {
                            if peer.id.to_string() == id_str {
                                let updated_peer = PeerItem {
                                    expanded,
                                    ..peer
                                };
                                model.set_row_data(i, updated_peer);
                                break;
                            }
                        }
                    }
                }
            }
        });

        // Push to peer callback
        window.global::<AppLogic>().on_push_to_peer({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let selected_files = self.selected_files.clone();
            let transfer_manager = self.transfer_manager.clone();
            let transfer_history = self.transfer_history.clone();
            let shared_endpoint = self.shared_endpoint.clone();

            move |peer_id| {
                let peer_id_str = peer_id.to_string();
                info!("Push to peer requested: {}", peer_id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let selected_files = selected_files.clone();
                let transfer_manager = transfer_manager.clone();
                let transfer_history = transfer_history.clone();
                let shared_endpoint = shared_endpoint.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get selected files
                        let files = selected_files.read().await;
                        if files.is_empty() {
                            update_status(&window_weak, "No files selected for push");
                            return;
                        }
                        let file_paths: Vec<PathBuf> = files.iter().map(|f| PathBuf::from(&f.path)).collect();
                        let file_names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
                        drop(files);

                        // Get the peer
                        let store = peer_store.read().await;
                        let peer = match store.find_by_id(&peer_id_str) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Peer not found: {}", peer_id_str);
                                update_status(&window_weak, &format!("Peer not found: {}", peer_id_str));
                                return;
                            }
                        };
                        drop(store);

                        // Check if peer allows push
                        if !peer.their_permissions.push {
                            update_status(&window_weak, &format!("Peer {} does not allow push", peer.name));
                            return;
                        }

                        // Create transfer
                        let transfer = Transfer::new_iroh_push(
                            file_names.clone(),
                            peer.endpoint_id.clone(),
                            peer.name.clone(),
                        );
                        let transfer_id = transfer.id.clone();
                        let _ = transfer_manager.add(transfer).await;

                        // Update UI with new transfer
                        update_transfers_ui(&window_weak, &transfer_manager).await;
                        update_status(&window_weak, &format!("Starting push to {}...", peer.name));

                        // Get the shared endpoint
                        let endpoint = {
                            let ep_guard = shared_endpoint.read().await;
                            match ep_guard.as_ref() {
                                Some(ep) => ep.clone(),
                                None => {
                                    error!("Shared endpoint not ready");
                                    let _ = transfer_manager.update(&transfer_id, |t| {
                                        t.status = TransferStatus::Failed;
                                        t.error = Some("Network not ready".to_string());
                                    }).await;
                                    update_transfers_ui(&window_weak, &transfer_manager).await;
                                    update_status(&window_weak, "Network not ready - please try again");
                                    return;
                                }
                            }
                        };

                        // Create progress channel
                        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<TransferEvent>(100);

                        // Spawn progress handler
                        let transfer_manager_progress = transfer_manager.clone();
                        let transfer_history_progress = transfer_history.clone();
                        let window_weak_progress = window_weak.clone();
                        let transfer_id_progress = transfer_id.clone();
                        tokio::spawn(async move {
                            while let Some(event) = progress_rx.recv().await {
                                match event {
                                    TransferEvent::Started { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Running;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                    }
                                    TransferEvent::Progress { transferred, total, speed, .. } => {
                                        let progress = if total > 0 { transferred as f64 / total as f64 * 100.0 } else { 0.0 };
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.progress = progress;
                                            t.speed = speed.clone();
                                            t.transferred = transferred;
                                            t.total_size = total;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                    }
                                    TransferEvent::FileComplete { file, .. } => {
                                        info!("File transferred: {}", file);
                                    }
                                    TransferEvent::Complete { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Completed;
                                            t.progress = 100.0;
                                            t.completed_at = Some(chrono::Utc::now());
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, "Push completed successfully");
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                    }
                                    TransferEvent::Failed { error, .. } => {
                                        error!("Transfer failed: {}", error);
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Failed;
                                            t.error = Some(error.clone());
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, &format!("Push failed: {}", error));
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                    }
                                    TransferEvent::Cancelled { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Cancelled;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, "Push cancelled");
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                    }
                                }
                            }
                        });

                        // Start the push
                        match push_files(&endpoint, &peer, &file_paths, progress_tx).await {
                            Ok(_) => {
                                info!("Push to {} completed", peer.name);
                            }
                            Err(e) => {
                                error!("Push to {} failed: {}", peer.name, e);
                                let _ = transfer_manager.update(&transfer_id, |t| {
                                    t.status = TransferStatus::Failed;
                                    t.error = Some(e.to_string());
                                }).await;
                                update_transfers_ui(&window_weak, &transfer_manager).await;
                                update_status(&window_weak, &format!("Push failed: {}", e));
                            }
                        }
                    });
                });
            }
        });
    }

    /// Set up file browser callbacks.
    fn setup_browse_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let peer_store = self.peer_store.clone();
        let shared_endpoint = self.shared_endpoint.clone();
        let browse_state = self.browse_state.clone();
        let config = self.config.clone();
        let transfer_manager = self.transfer_manager.clone();
        let transfer_history = self.transfer_history.clone();

        // Browse peer - open file browser for a peer
        window.global::<AppLogic>().on_browse_peer({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = shared_endpoint.clone();
            let browse_state = browse_state.clone();

            move |peer_id| {
                let peer_id_str = peer_id.to_string();
                info!("Browse peer requested: {}", peer_id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let browse_state = browse_state.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the peer
                        let store = peer_store.read().await;
                        let peer = match store.find_by_id(&peer_id_str) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Peer not found: {}", peer_id_str);
                                update_status(&window_weak, &format!("Peer not found: {}", peer_id_str));
                                return;
                            }
                        };
                        drop(store);

                        // Check if peer allows browse
                        if !peer.their_permissions.browse {
                            update_status(&window_weak, &format!("Peer {} does not allow browse", peer.name));
                            return;
                        }

                        // Set loading state
                        {
                            let mut state = browse_state.write().await;
                            state.peer_id = peer_id_str.clone();
                            state.peer_name = peer.name.clone();
                            state.current_path = "/".to_string();
                            state.entries.clear();
                        }

                        // Update UI to show loading
                        let window_weak_ui = window_weak.clone();
                        let peer_name = peer.name.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_ui.upgrade() {
                                window.global::<AppLogic>().set_browse_open(true);
                                window.global::<AppLogic>().set_browse_peer_id(SharedString::from(&peer_id_str));
                                window.global::<AppLogic>().set_browse_peer_name(SharedString::from(&peer_name));
                                window.global::<AppLogic>().set_browse_current_path(SharedString::from("/"));
                                window.global::<AppLogic>().set_browse_previous_path(SharedString::from(""));
                                window.global::<AppLogic>().set_browse_loading(true);
                                window.global::<AppLogic>().set_browse_error(SharedString::from(""));
                                window.global::<AppLogic>().set_browse_selected_count(0);
                                window.global::<AppLogic>().set_browse_entries(ModelRc::new(VecModel::from(Vec::<BrowseEntry>::new())));
                            }
                        });

                        // Get the shared endpoint
                        let endpoint = {
                            let ep_guard = shared_endpoint.read().await;
                            match ep_guard.as_ref() {
                                Some(ep) => ep.clone(),
                                None => {
                                    error!("Shared endpoint not ready");
                                    let window_weak_err = window_weak.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(window) = window_weak_err.upgrade() {
                                            window.global::<AppLogic>().set_browse_loading(false);
                                            window.global::<AppLogic>().set_browse_error(SharedString::from("Network not ready"));
                                        }
                                    });
                                    return;
                                }
                            }
                        };

                        // Browse the root
                        match browse_remote(&endpoint, &peer, None).await {
                            Ok((path, entries)) => {
                                info!("Browse succeeded: {} entries at {}", entries.len(), path);

                                // If at root with exactly one directory, auto-navigate into it
                                // This provides a better UX as users usually want to browse the home dir
                                if path == "/" && entries.len() == 1 && entries[0].is_dir {
                                    let home_name = entries[0].name.clone();
                                    info!("Auto-navigating into home directory: {}", home_name);

                                    // Browse into the home directory instead
                                    match browse_remote(&endpoint, &peer, Some(&home_name)).await {
                                        Ok((home_path, home_entries)) => {
                                            info!("Browse home succeeded: {} entries at {}", home_entries.len(), home_path);

                                            // Convert to internal state
                                            let browse_entries: Vec<BrowseEntryData> = home_entries.iter().map(|e| {
                                                BrowseEntryData {
                                                    name: e.name.clone(),
                                                    is_dir: e.is_dir,
                                                    size: e.size,
                                                    modified: e.modified,
                                                    path: format!("{}/{}", home_path, e.name),
                                                    selected: false,
                                                }
                                            }).collect();

                                            // Update state - this is the first successful browse, set previous to root
                                            {
                                                let mut state = browse_state.write().await;
                                                state.previous_path = Some("/".to_string());
                                                state.current_path = home_path.clone();
                                                state.entries = browse_entries.clone();
                                            }

                                            // Update UI
                                            let window_weak_ok = window_weak.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(window) = window_weak_ok.upgrade() {
                                                    window.global::<AppLogic>().set_browse_loading(false);
                                                    window.global::<AppLogic>().set_browse_current_path(SharedString::from(&home_path));
                                                    window.global::<AppLogic>().set_browse_previous_path(SharedString::from("/"));
                                                    window.global::<AppLogic>().set_browse_error(SharedString::from(""));

                                                    let ui_entries: Vec<BrowseEntry> = browse_entries.iter().map(|e| {
                                                        BrowseEntry {
                                                            name: SharedString::from(&e.name),
                                                            is_dir: e.is_dir,
                                                            size: SharedString::from(format_size(e.size)),
                                                            modified: SharedString::from(format_timestamp(e.modified)),
                                                            path: SharedString::from(&e.path),
                                                            selected: e.selected,
                                                        }
                                                    }).collect();
                                                    window.global::<AppLogic>().set_browse_entries(ModelRc::new(VecModel::from(ui_entries)));
                                                }
                                            });
                                            return;
                                        }
                                        Err(e) => {
                                            warn!("Failed to auto-navigate into home, showing root: {}", e);
                                            // Fall through to show root entries
                                        }
                                    }
                                }

                                // Convert to internal state
                                let browse_entries: Vec<BrowseEntryData> = entries.iter().map(|e| {
                                    BrowseEntryData {
                                        name: e.name.clone(),
                                        is_dir: e.is_dir,
                                        size: e.size,
                                        modified: e.modified,
                                        path: if path == "/" {
                                            e.name.clone()
                                        } else {
                                            format!("{}/{}", path, e.name)
                                        },
                                        selected: false,
                                    }
                                }).collect();

                                // Update state
                                // Initial browse at root - no previous path
                                {
                                    let mut state = browse_state.write().await;
                                    state.previous_path = None;  // Root is the first location
                                    state.current_path = path.clone();
                                    state.entries = browse_entries.clone();
                                }

                                // Update UI
                                let window_weak_ok = window_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak_ok.upgrade() {
                                        window.global::<AppLogic>().set_browse_loading(false);
                                        window.global::<AppLogic>().set_browse_current_path(SharedString::from(&path));
                                        window.global::<AppLogic>().set_browse_previous_path(SharedString::from(""));
                                        window.global::<AppLogic>().set_browse_error(SharedString::from(""));

                                        let ui_entries: Vec<BrowseEntry> = browse_entries.iter().map(|e| {
                                            BrowseEntry {
                                                name: SharedString::from(&e.name),
                                                is_dir: e.is_dir,
                                                size: SharedString::from(format_size(e.size)),
                                                modified: SharedString::from(format_timestamp(e.modified)),
                                                path: SharedString::from(&e.path),
                                                selected: e.selected,
                                            }
                                        }).collect();
                                        window.global::<AppLogic>().set_browse_entries(ModelRc::new(VecModel::from(ui_entries)));
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Browse failed: {}", e);
                                let err_str = e.to_string();
                                let window_weak_err = window_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak_err.upgrade() {
                                        window.global::<AppLogic>().set_browse_loading(false);
                                        window.global::<AppLogic>().set_browse_error(SharedString::from(&err_str));
                                    }
                                });
                            }
                        }
                    });
                });
            }
        });

        // Browse navigate - navigate to a directory
        window.global::<AppLogic>().on_browse_navigate({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = shared_endpoint.clone();
            let browse_state = browse_state.clone();

            move |path| {
                let path_str = path.to_string();
                info!("Browse navigate: {}", path_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let browse_state = browse_state.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get current peer from browse state
                        let (peer_id, current_path) = {
                            let state = browse_state.read().await;
                            (state.peer_id.clone(), state.current_path.clone())
                        };

                        // Resolve the path
                        let target_path = if path_str == ".." {
                            // Go to parent
                            if current_path == "/" {
                                "/".to_string()
                            } else {
                                let path = std::path::Path::new(&current_path);
                                path.parent()
                                    .and_then(|p| p.to_str())
                                    .map(|s| if s.is_empty() { "/" } else { s })
                                    .unwrap_or("/")
                                    .to_string()
                            }
                        } else {
                            path_str.clone()
                        };

                        // Get the peer
                        let store = peer_store.read().await;
                        let peer = match store.find_by_id(&peer_id) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Peer not found: {}", peer_id);
                                return;
                            }
                        };
                        drop(store);

                        // Set loading
                        let window_weak_loading = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_loading.upgrade() {
                                window.global::<AppLogic>().set_browse_loading(true);
                                window.global::<AppLogic>().set_browse_error(SharedString::from(""));
                            }
                        });

                        // Get the shared endpoint
                        let endpoint = {
                            let ep_guard = shared_endpoint.read().await;
                            match ep_guard.as_ref() {
                                Some(ep) => ep.clone(),
                                None => {
                                    error!("Shared endpoint not ready");
                                    return;
                                }
                            }
                        };

                        // Browse the target path
                        let browse_path = if target_path == "/" { None } else { Some(target_path.as_str()) };
                        match browse_remote(&endpoint, &peer, browse_path).await {
                            Ok((path, entries)) => {
                                info!("Navigate succeeded: {} entries at {}", entries.len(), path);

                                let browse_entries: Vec<BrowseEntryData> = entries.iter().map(|e| {
                                    BrowseEntryData {
                                        name: e.name.clone(),
                                        is_dir: e.is_dir,
                                        size: e.size,
                                        modified: e.modified,
                                        path: if path == "/" {
                                            e.name.clone()
                                        } else {
                                            format!("{}/{}", path, e.name)
                                        },
                                        selected: false,
                                    }
                                }).collect();

                                // Save previous path before updating
                                let previous_path = {
                                    let mut state = browse_state.write().await;
                                    let prev = state.current_path.clone();
                                    state.previous_path = Some(prev.clone());
                                    state.current_path = path.clone();
                                    state.entries = browse_entries.clone();
                                    prev
                                };

                                let window_weak_ok = window_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak_ok.upgrade() {
                                        window.global::<AppLogic>().set_browse_loading(false);
                                        window.global::<AppLogic>().set_browse_current_path(SharedString::from(&path));
                                        window.global::<AppLogic>().set_browse_previous_path(SharedString::from(&previous_path));
                                        window.global::<AppLogic>().set_browse_selected_count(0);
                                        window.global::<AppLogic>().set_browse_error(SharedString::from(""));

                                        let ui_entries: Vec<BrowseEntry> = browse_entries.iter().map(|e| {
                                            BrowseEntry {
                                                name: SharedString::from(&e.name),
                                                is_dir: e.is_dir,
                                                size: SharedString::from(format_size(e.size)),
                                                modified: SharedString::from(format_timestamp(e.modified)),
                                                path: SharedString::from(&e.path),
                                                selected: e.selected,
                                            }
                                        }).collect();
                                        window.global::<AppLogic>().set_browse_entries(ModelRc::new(VecModel::from(ui_entries)));
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Navigate failed: {}", e);
                                let err_str = e.to_string();
                                let window_weak_err = window_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak_err.upgrade() {
                                        window.global::<AppLogic>().set_browse_loading(false);
                                        window.global::<AppLogic>().set_browse_error(SharedString::from(&err_str));
                                    }
                                });
                            }
                        }
                    });
                });
            }
        });

        // Toggle selection of a browse entry
        window.global::<AppLogic>().on_browse_toggle_select({
            let window_weak = window_weak.clone();
            let browse_state = browse_state.clone();

            move |index| {
                let idx = index as usize;
                info!("Toggle select: {}", idx);
                let window_weak = window_weak.clone();
                let browse_state = browse_state.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Toggle selection in state and count selected
                        let (entries, selected_count) = {
                            let mut state = browse_state.write().await;
                            if idx < state.entries.len() {
                                state.entries[idx].selected = !state.entries[idx].selected;
                            }
                            let count = state.entries.iter().filter(|e| e.selected && !e.is_dir).count();
                            (state.entries.clone(), count as i32)
                        };

                        // Update UI
                        let window_weak_ui = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_ui.upgrade() {
                                let ui_entries: Vec<BrowseEntry> = entries.iter().map(|e| {
                                    BrowseEntry {
                                        name: SharedString::from(&e.name),
                                        is_dir: e.is_dir,
                                        size: SharedString::from(format_size(e.size)),
                                        modified: SharedString::from(format_timestamp(e.modified)),
                                        path: SharedString::from(&e.path),
                                        selected: e.selected,
                                    }
                                }).collect();
                                window.global::<AppLogic>().set_browse_entries(ModelRc::new(VecModel::from(ui_entries)));
                                window.global::<AppLogic>().set_browse_selected_count(selected_count);
                            }
                        });
                    });
                });
            }
        });

        // Pull selected files
        window.global::<AppLogic>().on_browse_pull_selected({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = shared_endpoint.clone();
            let browse_state = browse_state.clone();
            let config = config.clone();
            let transfer_manager = transfer_manager.clone();
            let transfer_history = transfer_history.clone();

            move || {
                info!("Pull selected files");
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let browse_state = browse_state.clone();
                let config = config.clone();
                let transfer_manager = transfer_manager.clone();
                let transfer_history = transfer_history.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get selected files from browse state
                        let (peer_id, selected_files) = {
                            let state = browse_state.read().await;
                            let selected: Vec<_> = state.entries.iter()
                                .filter(|e| e.selected && !e.is_dir)
                                .map(|e| FileRequest {
                                    path: e.path.clone(),
                                    hash: None,
                                })
                                .collect();
                            (state.peer_id.clone(), selected)
                        };

                        if selected_files.is_empty() {
                            update_status(&window_weak, "No files selected");
                            return;
                        }

                        // Get the peer
                        let store = peer_store.read().await;
                        let peer = match store.find_by_id(&peer_id) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Peer not found: {}", peer_id);
                                return;
                            }
                        };
                        drop(store);

                        // Close browser dialog
                        let window_weak_close = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_close.upgrade() {
                                window.global::<AppLogic>().set_browse_open(false);
                            }
                        });

                        // Get download directory
                        let download_dir = {
                            let cfg = config.read().await;
                            cfg.download_dir.clone()
                        };

                        // Create transfer for tracking
                        let file_names: Vec<String> = selected_files.iter()
                            .map(|f| std::path::Path::new(&f.path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(&f.path)
                                .to_string())
                            .collect();
                        let transfer = Transfer::new_iroh_pull(
                            file_names.clone(),
                            peer.endpoint_id.clone(),
                            peer.name.clone(),
                        );
                        let transfer_id = transfer.id.clone();
                        let _ = transfer_manager.add(transfer).await;

                        // Update UI
                        update_transfers_ui(&window_weak, &transfer_manager).await;
                        update_status(&window_weak, &format!("Pulling {} files from {}...", selected_files.len(), peer.name));

                        // Get the shared endpoint
                        let endpoint = {
                            let ep_guard = shared_endpoint.read().await;
                            match ep_guard.as_ref() {
                                Some(ep) => ep.clone(),
                                None => {
                                    error!("Shared endpoint not ready");
                                    let _ = transfer_manager.update(&transfer_id, |t| {
                                        t.status = TransferStatus::Failed;
                                        t.error = Some("Network not ready".to_string());
                                    }).await;
                                    update_transfers_ui(&window_weak, &transfer_manager).await;
                                    return;
                                }
                            }
                        };

                        // Create progress channel
                        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<TransferEvent>(100);

                        // Spawn progress handler
                        let transfer_manager_progress = transfer_manager.clone();
                        let transfer_history_progress = transfer_history.clone();
                        let window_weak_progress = window_weak.clone();
                        let transfer_id_progress = transfer_id.clone();
                        let peer_name = peer.name.clone();
                        tokio::spawn(async move {
                            while let Some(event) = progress_rx.recv().await {
                                match event {
                                    TransferEvent::Started { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Running;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                    }
                                    TransferEvent::Progress { transferred, total, speed, .. } => {
                                        let progress = if total > 0 { transferred as f64 / total as f64 * 100.0 } else { 0.0 };
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.progress = progress;
                                            t.speed = speed.clone();
                                            t.transferred = transferred;
                                            t.total_size = total;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                    }
                                    TransferEvent::FileComplete { file, .. } => {
                                        info!("File pulled: {}", file);
                                    }
                                    TransferEvent::Complete { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Completed;
                                            t.progress = 100.0;
                                            t.completed_at = Some(chrono::Utc::now());
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, &format!("Pull from {} completed", peer_name));
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                    }
                                    TransferEvent::Failed { error, .. } => {
                                        error!("Pull failed: {}", error);
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Failed;
                                            t.error = Some(error.clone());
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, &format!("Pull failed: {}", error));
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                    }
                                    TransferEvent::Cancelled { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Cancelled;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress).await;
                                    }
                                }
                            }
                        });

                        // Start the pull
                        match pull_files(&endpoint, &peer, &selected_files, &download_dir, progress_tx).await {
                            Ok(_) => {
                                info!("Pull from {} completed", peer.name);
                            }
                            Err(e) => {
                                error!("Pull from {} failed: {}", peer.name, e);
                                let _ = transfer_manager.update(&transfer_id, |t| {
                                    t.status = TransferStatus::Failed;
                                    t.error = Some(e.to_string());
                                }).await;
                                update_transfers_ui(&window_weak, &transfer_manager).await;
                                update_status(&window_weak, &format!("Pull failed: {}", e));
                            }
                        }
                    });
                });
            }
        });

        // Close browser dialog
        window.global::<AppLogic>().on_browse_close({
            let window_weak = window_weak.clone();
            let browse_state = browse_state.clone();

            move || {
                info!("Close browser");
                let window_weak = window_weak.clone();
                let browse_state = browse_state.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Clear state
                        {
                            let mut state = browse_state.write().await;
                            *state = BrowseState::default();
                        }

                        // Close dialog
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                window.global::<AppLogic>().set_browse_open(false);
                            }
                        });
                    });
                });
            }
        });
    }
}

/// Format file size as human-readable string.
fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "-".to_string();
    }
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Format timestamp as human-readable string.
fn format_timestamp(ts: Option<i64>) -> String {
    match ts {
        Some(secs) => {
            let dt = chrono::DateTime::from_timestamp(secs, 0)
                .unwrap_or_else(|| chrono::Utc::now());
            dt.format("%Y-%m-%d %H:%M").to_string()
        }
        None => "-".to_string(),
    }
}

/// Update the transfers list in the UI.
async fn update_transfers_ui(window_weak: &Weak<MainWindow>, manager: &TransferManager) {
    let mut transfers = manager.list().await;

    // Sort by started_at descending (newest first)
    transfers.sort_by(|a, b| b.started_at.cmp(&a.started_at));

    let items: Vec<TransferItem> = transfers
        .iter()
        .map(|t| TransferItem {
            id: SharedString::from(t.id.to_string()),
            transfer_type: SharedString::from(match t.transfer_type {
                TransferType::Send => "send",
                TransferType::Receive => "receive",
                TransferType::IrohPush => "push",
                TransferType::IrohPull => "pull",
            }),
            status: SharedString::from(match t.status {
                TransferStatus::Pending => "pending",
                TransferStatus::Running => "running",
                TransferStatus::Completed => "completed",
                TransferStatus::Failed => "failed",
                TransferStatus::Cancelled => "cancelled",
            }),
            code: SharedString::from(t.code.as_deref().unwrap_or("")),
            files: SharedString::from(t.files.join(", ")),
            progress: t.progress as f32,
            speed: SharedString::from(&t.speed),
            error: SharedString::from(t.error.as_deref().unwrap_or("")),
        })
        .collect();

    let window_weak = window_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = window_weak.upgrade() {
            window
                .global::<AppLogic>()
                .set_transfers(ModelRc::new(VecModel::from(items)));
        }
    });
}

/// Update the status bar.
fn update_status(window_weak: &Weak<MainWindow>, status: &str) {
    let window_weak = window_weak.clone();
    let status = status.to_string();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = window_weak.upgrade() {
            window
                .global::<AppLogic>()
                .set_app_status(SharedString::from(status));
        }
    });
}

/// Ping a peer to check if they're online.
/// Returns true if the peer responds to ping, false otherwise.
async fn ping_peer(endpoint: &IrohEndpoint, peer: &TrustedPeer) -> bool {
    use std::time::Duration;

    // Parse the node ID
    let node_id: NodeId = match peer.endpoint_id.parse() {
        Ok(id) => id,
        Err(_) => return false,
    };

    // Add peer's address info before connecting (if relay URL is known)
    let mut node_addr = NodeAddr::new(node_id);
    if let Some(relay_url) = &peer.relay_url {
        if let Ok(url) = relay_url.parse() {
            node_addr = node_addr.with_relay_url(url);
        }
    }
    let _ = endpoint.add_node_addr(node_addr);

    // Try to connect with a short timeout
    let connect_result = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(&peer.endpoint_id)
    ).await;

    let mut conn = match connect_result {
        Ok(Ok(c)) => c,
        Ok(Err(_)) | Err(_) => return false,
    };

    // Send ping
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let ping = ControlMessage::Ping { timestamp };
    if conn.send(&ping).await.is_err() {
        let _ = conn.close().await;
        return false;
    }

    // Wait for pong with timeout
    let pong_result = tokio::time::timeout(
        Duration::from_secs(5),
        conn.recv()
    ).await;

    let _ = conn.close().await;

    match pong_result {
        Ok(Ok(ControlMessage::Pong { .. })) => true,
        _ => false,
    }
}

/// Set peers and push-to-peer dropdown data on the window.
fn set_peers_with_push_data(window: &MainWindow, items: Vec<PeerItem>) {
    // Collect peers that allow push for the send panel dropdown
    let mut pushable_names: Vec<SharedString> = Vec::new();
    let mut pushable_ids: Vec<SharedString> = Vec::new();

    for item in &items {
        if item.can_push {
            pushable_names.push(item.name.clone());
            pushable_ids.push(item.id.clone());
        }
    }

    // Update the peers list
    window.global::<AppLogic>().set_peers(ModelRc::new(VecModel::from(items)));

    // Update the push-to-peer dropdown data
    window.global::<AppLogic>().set_peer_names_for_push(
        ModelRc::new(VecModel::from(pushable_names))
    );
    window.global::<AppLogic>().set_pushable_peer_ids(
        ModelRc::new(VecModel::from(pushable_ids))
    );
}

/// Check for trust bundles in a directory and handle them.
async fn check_and_handle_trust_bundle(
    download_dir: &PathBuf,
    window_weak: &Weak<MainWindow>,
    identity: &Arc<RwLock<Option<Identity>>>,
    shared_endpoint: &Arc<RwLock<Option<IrohEndpoint>>>,
    peer_store: &Arc<RwLock<PeerStore>>,
    peer_status: &Arc<RwLock<HashMap<String, PeerConnectionStatus>>>,
) {
    use croh_core::complete_trust_as_receiver;

    // Look for trust bundle files
    let bundle_files: Vec<_> = match std::fs::read_dir(download_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| TrustBundle::is_trust_bundle_filename(n))
                    .unwrap_or(false)
            })
            .collect(),
        Err(e) => {
            error!("Failed to read download dir: {}", e);
            return;
        }
    };

    if bundle_files.is_empty() {
        return;
    }

    info!("Found {} potential trust bundle(s)", bundle_files.len());

    // Load all bundles and find the most recent valid one
    let mut valid_bundles: Vec<(std::path::PathBuf, TrustBundle)> = Vec::new();
    let mut expired_paths: Vec<std::path::PathBuf> = Vec::new();

    for entry in bundle_files {
        let path = entry.path();
        match TrustBundle::load(&path) {
            Ok(bundle) => {
                if bundle.is_valid() {
                    valid_bundles.push((path, bundle));
                } else {
                    warn!("Trust bundle at {:?} has expired, removing", path);
                    expired_paths.push(path);
                }
            }
            Err(e) => {
                warn!("Failed to load trust bundle {:?}: {}", path, e);
            }
        }
    }

    // Clean up expired bundles
    for path in expired_paths {
        let _ = std::fs::remove_file(&path);
    }

    // Sort by creation time, most recent first
    valid_bundles.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at));

    // Only process the most recent valid bundle
    let (path, bundle) = match valid_bundles.into_iter().next() {
        Some(b) => b,
        None => {
            info!("No valid trust bundles found");
            return;
        }
    };

    info!("Processing most recent trust bundle from {} (created at {})",
          bundle.sender.name, bundle.created_at);

    update_status(window_weak, &format!("Trust request from {}...", bundle.sender.name));

    // Get our identity
    let our_identity = {
        let id_guard = identity.read().await;
        match id_guard.as_ref() {
            Some(id) => id.clone(),
            None => {
                error!("No identity available for trust handshake");
                update_status(window_weak, "Error: No identity available");
                let _ = std::fs::remove_file(&path);
                return;
            }
        }
    };

    // Get the shared endpoint (should already be running from background listener)
    let endpoint = {
        let ep_guard = shared_endpoint.read().await;
        match ep_guard.as_ref() {
            Some(ep) => ep.clone(),
            None => {
                error!("No shared endpoint available for trust handshake");
                update_status(window_weak, "Error: Network not ready");
                let _ = std::fs::remove_file(&path);
                return;
            }
        }
    };

    update_status(window_weak, &format!("Connecting to {}...", bundle.sender.name));

    // Perform the handshake as receiver
    match complete_trust_as_receiver(&endpoint, &bundle, &our_identity).await {
        Ok(result) => {
            info!("Trust established with {}", result.peer.name);
            update_status(window_weak, &format!("Trusted peer added: {}", result.peer.name));

            // Add to peer store (or update if peer already exists)
            let mut store = peer_store.write().await;
            match store.add_or_update(result.peer.clone()) {
                Ok(updated) => {
                    if updated {
                        info!("Updated existing peer: {}", result.peer.name);
                    } else {
                        info!("Added new peer: {}", result.peer.name);
                    }
                    drop(store);

                    // Mark the new peer as online (we just connected to them)
                    {
                        let mut status_map = peer_status.write().await;
                        let status = status_map.entry(result.peer.id.clone()).or_default();
                        status.online = true;
                        status.last_ping = Some(std::time::Instant::now());
                    }

                    // Update peers UI with status
                    update_peers_ui_with_status(window_weak, peer_store, peer_status).await;
                }
                Err(e) => {
                    error!("Failed to save peer: {}", e);
                }
            }
        }
        Err(e) => {
            error!("Handshake failed with {}: {}", bundle.sender.name, e);
            update_status(window_weak, &format!("Trust handshake failed: {}", e));
        }
    }

    // Don't close the shared endpoint - it's used for other operations

    // Clean up the bundle file
    if let Err(e) = std::fs::remove_file(&path) {
        warn!("Failed to remove trust bundle file: {}", e);
    }
}

/// Convert a TrustedPeer to PeerItem for UI display.
fn trusted_peer_to_item(p: &TrustedPeer) -> PeerItem {
    let last_seen = p.last_seen
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "Never".to_string());
    let added_at = p.added_at.format("%Y-%m-%d").to_string();
    let endpoint_display = if p.endpoint_id.len() > 16 {
        format!("{}...{}", &p.endpoint_id[..8], &p.endpoint_id[p.endpoint_id.len()-8..])
    } else {
        p.endpoint_id.clone()
    };

    PeerItem {
        id: SharedString::from(p.id.clone()),
        name: SharedString::from(p.name.clone()),
        endpoint_id: SharedString::from(endpoint_display),
        status: SharedString::from("offline"),
        last_seen: SharedString::from(last_seen),
        relay_url: SharedString::from(p.relay_url.clone().unwrap_or_default()),
        added_at: SharedString::from(added_at),
        expanded: false,
        // What they allow us to do
        can_push: p.their_permissions.push,
        can_pull: p.their_permissions.pull,
        can_browse: p.their_permissions.browse,
        // What we allow them to do
        allow_push: p.permissions_granted.push,
        allow_pull: p.permissions_granted.pull,
        allow_browse: p.permissions_granted.browse,
        // Connection status (filled in by apply_status_to_peer_item)
        latency_ms: -1,
        avg_latency_ms: -1,
        connection_type: SharedString::from(""),
        last_contact: SharedString::from(""),
        ping_count: 0,
        last_upload_speed: SharedString::from(""),
        last_download_speed: SharedString::from(""),
        peer_hostname: SharedString::from(""),
        peer_os: SharedString::from(""),
        peer_version: SharedString::from(""),
        peer_free_space: SharedString::from(""),
        // DND status (filled in by status updates)
        peer_dnd_mode: SharedString::from("off"),
        peer_dnd_message: SharedString::from(""),
        // Speed test results (filled in when test completes)
        speed_test_running: false,
        speed_test_upload: SharedString::from(""),
        speed_test_download: SharedString::from(""),
        speed_test_latency: SharedString::from(""),
    }
}

/// Apply connection status to a PeerItem.
fn apply_status_to_peer_item(item: &mut PeerItem, status: &PeerConnectionStatus) {
    item.status = SharedString::from(if status.online { "online" } else { "offline" });
    item.latency_ms = status.latency_ms.map(|v| v as i32).unwrap_or(-1);
    item.avg_latency_ms = status.avg_latency_ms.map(|v| v as i32).unwrap_or(-1);
    item.connection_type = SharedString::from(status.connection_type.clone());
    item.ping_count = status.ping_count as i32;

    // Format last contact as relative time
    item.last_contact = SharedString::from(
        status.last_contact
            .map(|dt| format_relative_time(dt))
            .unwrap_or_default()
    );

    // Transfer speeds
    item.last_upload_speed = SharedString::from(
        status.last_upload_speed.clone().unwrap_or_default()
    );
    item.last_download_speed = SharedString::from(
        status.last_download_speed.clone().unwrap_or_default()
    );

    // Peer info
    item.peer_hostname = SharedString::from(
        status.peer_hostname.clone().unwrap_or_default()
    );
    item.peer_os = SharedString::from(
        status.peer_os.clone().unwrap_or_default()
    );
    item.peer_version = SharedString::from(
        status.peer_version.clone().unwrap_or_default()
    );
    item.peer_free_space = SharedString::from(
        status.peer_free_space
            .map(|bytes| format_size(bytes))
            .unwrap_or_default()
    );
}

/// Format a timestamp as relative time ("Just now", "5 min ago", "2 hours ago", etc.)
fn format_relative_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);

    if duration.num_seconds() < 10 {
        "Just now".to_string()
    } else if duration.num_seconds() < 60 {
        format!("{} sec ago", duration.num_seconds())
    } else if duration.num_minutes() < 60 {
        let mins = duration.num_minutes();
        if mins == 1 {
            "1 min ago".to_string()
        } else {
            format!("{} min ago", mins)
        }
    } else if duration.num_hours() < 24 {
        let hours = duration.num_hours();
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else {
        let days = duration.num_days();
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    }
}

/// Update the peers UI from the peer store with status information.
/// Preserves UI state (like expanded) from the existing model.
async fn update_peers_ui_with_status(
    window_weak: &Weak<MainWindow>,
    peer_store: &Arc<RwLock<PeerStore>>,
    peer_status: &Arc<RwLock<HashMap<String, PeerConnectionStatus>>>,
) {
    let peers = peer_store.read().await;
    let mut new_items: Vec<PeerItem> = peers.list().iter().map(trusted_peer_to_item).collect();
    drop(peers);

    // Apply full status from the peer_status map
    let status_map = peer_status.read().await;
    for item in &mut new_items {
        if let Some(status) = status_map.get(&item.id.to_string()) {
            apply_status_to_peer_item(item, status);
        }
    }
    drop(status_map);

    let window_weak = window_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = window_weak.upgrade() {
            let logic = window.global::<AppLogic>();
            let current_model = logic.get_peers();

            // Build a map of current expanded states by peer ID
            let mut expanded_states: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
            for i in 0..current_model.row_count() {
                if let Some(peer) = current_model.row_data(i) {
                    expanded_states.insert(peer.id.to_string(), peer.expanded);
                }
            }

            // Apply preserved expanded states to new items
            let items_with_state: Vec<PeerItem> = new_items.into_iter().map(|mut item| {
                if let Some(&expanded) = expanded_states.get(&item.id.to_string()) {
                    item.expanded = expanded;
                }
                item
            }).collect();

            // Use the helper to set peers and push data
            set_peers_with_push_data(&window, items_with_state);
        }
    });
}

/// Wait for incoming trust handshake from a peer who received our trust bundle.
/// NOTE: This function is deprecated - trust handshakes are now handled by the background listener.
#[allow(dead_code)]
async fn wait_for_trust_handshake(
    endpoint: &IrohEndpoint,
    expected_nonce: &str,
    peer_store: &Arc<RwLock<PeerStore>>,
) -> croh_core::Result<TrustedPeer> {
    use std::time::Duration;

    info!("Waiting for incoming trust connection...");

    // Accept a connection
    let mut conn = tokio::time::timeout(Duration::from_secs(120), endpoint.accept())
        .await
        .map_err(|_| croh_core::Error::Iroh("timeout waiting for connection".to_string()))??;

    let remote_id = conn.remote_id_string();
    info!("Accepted connection from: {}", remote_id);

    // Receive the TrustConfirm message
    let msg = tokio::time::timeout(Duration::from_secs(30), conn.recv())
        .await
        .map_err(|_| croh_core::Error::Iroh("timeout waiting for message".to_string()))??;

    match msg {
        ControlMessage::TrustConfirm {
            peer: their_peer_info,
            nonce,
            permissions: their_permissions,
        } => {
            // Verify the nonce
            if nonce != expected_nonce {
                warn!("Invalid nonce from {}: expected {}, got {}", remote_id, expected_nonce, nonce);
                let response = ControlMessage::TrustRevoke {
                    reason: "invalid nonce".to_string(),
                };
                let _ = conn.send(&response).await;
                return Err(croh_core::Error::Trust("invalid nonce".to_string()));
            }

            info!("Valid TrustConfirm from {} ({})", their_peer_info.name, remote_id);
            if their_peer_info.relay_url.is_some() {
                info!("Peer relay URL: {:?}", their_peer_info.relay_url);
            }

            // Send TrustComplete
            let response = ControlMessage::TrustComplete;
            conn.send(&response).await?;

            // Give the message time to be delivered before closing
            // QUIC may not deliver data if the endpoint closes too quickly
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Create the trusted peer with their relay URL for future connections
            let peer = TrustedPeer::new_with_relay(
                their_peer_info.endpoint_id.clone(),
                their_peer_info.name.clone(),
                Permissions::all(), // We grant all permissions
                their_permissions,  // Their permissions to us
                their_peer_info.relay_url.clone(), // Store their relay URL
            );

            // Add to peer store (or update if peer already exists)
            let mut store = peer_store.write().await;
            match store.add_or_update(peer.clone()) {
                Ok(updated) => {
                    if updated {
                        info!("Updated existing peer: {}", peer.name);
                    } else {
                        info!("Added new peer: {}", peer.name);
                    }
                }
                Err(e) => {
                    error!("Failed to save peer: {}", e);
                    return Err(e);
                }
            }
            drop(store);

            // Close connection gracefully
            let _ = conn.close().await;

            Ok(peer)
        }
        other => {
            error!("Unexpected message during handshake: {:?}", other);
            Err(croh_core::Error::Iroh(format!(
                "unexpected message: expected TrustConfirm, got {:?}",
                other
            )))
        }
    }
}

/// Save a completed transfer to history.
async fn save_to_history(
    transfer_history: &Arc<RwLock<TransferHistory>>,
    transfer_manager: &TransferManager,
    transfer_id: &TransferId,
) {
    if let Some(transfer) = transfer_manager.get(transfer_id).await {
        if transfer.status.is_terminal() {
            let mut history = transfer_history.write().await;
            if let Err(e) = history.add_and_save(transfer) {
                warn!("Failed to save transfer to history: {}", e);
            }
        }
    }
}

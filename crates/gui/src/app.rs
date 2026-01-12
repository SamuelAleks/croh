//! Application state and callback handling.

use croh_core::{
    config::Theme,
    croc::{find_croc_executable, refresh_croc_cache, Curve, CrocEvent, CrocOptions, CrocProcess, HashAlgorithm},
    files, platform, Config, ControlMessage, FileRequest, Identity, IrohEndpoint, NetworkStore, PeerAddress, PeerInfo, PeerStore, Permissions,
    Transfer, TransferId, TransferEvent, TransferHistory, TransferManager, TransferStatus, TransferType,
    TrustedPeer, TrustBundle, push_files, pull_files, handle_incoming_push, handle_incoming_pull, handle_browse_request, browse_remote, default_browsable_paths,
    NodeAddr, NodeId, generate_id, serde_json,
    ChatStore, ChatHandler,
    stream_screen_from_peer, handle_screen_stream_request, ScreenStreamEvent,
    screen::{
        ScreenViewer, ViewerConfig, ViewerCommand, ViewerEvent,
        viewer_command_channel, viewer_event_channel,
        ViewerCommandSender, RemoteInputEvent,
    },
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, error, info, warn};

use crate::{AppLogic, AppSettings, BrowseEntry, ChatMessageItem, MainWindow, NetworkItem, NetworkMember, PeerItem, SelectedFile, SessionStats, TransferItem};

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
    pub peer_total_space: Option<u64>,
    pub peer_uptime: Option<u64>,
    pub peer_active_transfers: Option<u32>,
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
    /// Connection status for each peer (keyed by peer ID).
    peer_status: Arc<RwLock<HashMap<String, PeerConnectionStatus>>>,
    /// Transfer history for completed transfers.
    transfer_history: Arc<RwLock<TransferHistory>>,
    /// Time when the app was started (for uptime calculation).
    app_start_time: std::time::Instant,
    /// Session statistics (bytes uploaded/downloaded this session).
    session_stats: Arc<RwLock<SessionStatsTracker>>,
    /// Set of peer IDs that are currently disconnected (user paused connections).
    disconnected_peers: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Set of peer IDs that we have revoked trust for (sent TrustRevoke but kept in store).
    revoked_by_us: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Network store for peer networks.
    network_store: Arc<RwLock<NetworkStore>>,
    /// Pending peer introductions (introduction_id -> state).
    pending_introductions: Arc<RwLock<HashMap<String, PendingIntroduction>>>,
    /// Chat store for persistent message history.
    chat_store: Arc<ChatStore>,
    /// Currently active chat peer ID.
    active_chat_peer: Arc<RwLock<Option<String>>>,
    /// Debounce timer for typing indicator.
    typing_debounce: Arc<RwLock<Option<std::time::Instant>>>,
    /// Screen viewer command sender (for controlling the viewer).
    screen_viewer_cmd: Arc<RwLock<Option<ViewerCommandSender>>>,
    /// Currently active screen viewer peer ID.
    active_screen_peer: Arc<RwLock<Option<String>>>,
}

/// Tracks session statistics for uploads and downloads.
#[derive(Debug, Default)]
struct SessionStatsTracker {
    bytes_uploaded: u64,
    bytes_downloaded: u64,
    transfers_completed: u32,
}

/// State for a pending peer introduction.
#[derive(Debug, Clone)]
struct PendingIntroduction {
    /// Unique introduction ID.
    introduction_id: String,
    /// Network ID where the introduction is happening.
    network_id: String,
    /// Peer being introduced (B).
    peer_to_introduce_id: String,
    peer_to_introduce_name: String,
    /// Target network member (C).
    target_member_id: String,
    target_member_name: String,
    /// Whether peer B has accepted.
    peer_b_accepted: bool,
    /// Whether peer C has accepted.
    peer_c_accepted: bool,
    /// When the introduction was started.
    started_at: std::time::Instant,
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
/// When initiating trust, we store the bundle here so the background listener
/// can complete the handshake when the peer connects and create the right peer type.
struct PendingTrust {
    /// The trust bundle we sent (includes nonce, guest_mode, duration).
    bundle: TrustBundle,
    /// Channel to send the result back to the trust initiation thread.
    result_tx: oneshot::Sender<Result<TrustedPeer, croh_core::Error>>,
}

impl App {
    /// Create a new App instance.
    pub fn new(window: Weak<MainWindow>) -> Self {
        let config = Config::load_with_env().unwrap_or_default();
        let peer_store = PeerStore::load().unwrap_or_default();
        let transfer_history = TransferHistory::load().unwrap_or_default();
        let network_store = NetworkStore::load().unwrap_or_default();
        let chat_store = ChatStore::open().expect("Failed to open chat store");

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
            peer_status: Arc::new(RwLock::new(HashMap::new())),
            transfer_history: Arc::new(RwLock::new(transfer_history)),
            app_start_time: std::time::Instant::now(),
            session_stats: Arc::new(RwLock::new(SessionStatsTracker::default())),
            disconnected_peers: Arc::new(RwLock::new(std::collections::HashSet::new())),
            revoked_by_us: Arc::new(RwLock::new(std::collections::HashSet::new())),
            network_store: Arc::new(RwLock::new(network_store)),
            pending_introductions: Arc::new(RwLock::new(HashMap::new())),
            chat_store: Arc::new(chat_store),
            active_chat_peer: Arc::new(RwLock::new(None)),
            typing_debounce: Arc::new(RwLock::new(None)),
            screen_viewer_cmd: Arc::new(RwLock::new(None)),
            active_screen_peer: Arc::new(RwLock::new(None)),
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
        self.setup_network_callbacks(window);
        self.setup_chat_callbacks(window);
        self.setup_screen_viewer_callbacks(window);

        // Start background listener for incoming transfers
        self.start_background_listener();

        // Start peer status checker (pings peers periodically)
        self.start_peer_status_checker();

        // Start guest peer cleanup task (removes expired guests)
        self.start_guest_cleanup_task();
    }

    /// Refresh the peers model with the given items (replaces the model).
    fn refresh_peers_model(&self, items: Vec<PeerItem>) {
        // Collect peers that allow push for the send panel dropdown
        let mut pushable_names: Vec<SharedString> = Vec::new();
        let mut pushable_ids: Vec<SharedString> = Vec::new();

        for item in &items {
            if item.can_push && !item.revoked {
                pushable_names.push(item.name.clone());
                pushable_ids.push(item.id.clone());
            }
        }

        // Update the model and dropdown data
        if let Some(window) = self.window.upgrade() {
            let logic = window.global::<AppLogic>();
            logic.set_peers(ModelRc::new(VecModel::from(items)));
            logic.set_peer_names_for_push(ModelRc::new(VecModel::from(pushable_names)));
            logic.set_pushable_peer_ids(ModelRc::new(VecModel::from(pushable_ids)));
        }
    }

    /// Start a background task that periodically pings peers to check their status.
    fn start_peer_status_checker(&self) {
        let shared_endpoint = self.shared_endpoint.clone();
        let peer_store = self.peer_store.clone();
        let peer_status = self.peer_status.clone();
        let window_weak = self.window.clone();
        let shutdown = self.listener_shutdown.clone();
        let disconnected_peers = self.disconnected_peers.clone();

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

                    // Get list of peers to ping (excluding disconnected ones)
                    let peers: Vec<TrustedPeer> = {
                        let store = peer_store.read().await;
                        let disconnected = disconnected_peers.read().await;
                        store.list()
                            .iter()
                            .filter(|p| !disconnected.contains(&p.id))
                            .cloned()
                            .collect()
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
                        let has_relay = peer.relay_url().is_some();

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

    /// Start a background task that periodically cleans up expired guest peers.
    fn start_guest_cleanup_task(&self) {
        let peer_store = self.peer_store.clone();
        let window_weak = self.window.clone();
        let shutdown = self.listener_shutdown.clone();
        let peer_status = self.peer_status.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                // Check every 5 minutes for expired guests
                loop {
                    if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }

                    // Wait 5 minutes between cleanup checks
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;

                    if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }

                    // Get list of expired peers before cleanup (for UI update)
                    let expired_ids: Vec<String> = {
                        let store = peer_store.read().await;
                        store.list_expired().iter().map(|p| p.id.clone()).collect()
                    };

                    if expired_ids.is_empty() {
                        continue;
                    }

                    // Cleanup expired guests
                    let removed = {
                        let mut store = peer_store.write().await;
                        match store.cleanup_expired() {
                            Ok(count) => count,
                            Err(e) => {
                                warn!("Failed to cleanup expired guests: {}", e);
                                0
                            }
                        }
                    };

                    if removed > 0 {
                        info!("Cleaned up {} expired guest peer(s)", removed);

                        // Remove status entries for expired peers
                        {
                            let mut status_map = peer_status.write().await;
                            for id in &expired_ids {
                                status_map.remove(id);
                            }
                        }

                        // Refresh the peers list in the UI
                        let store = peer_store.read().await;
                        let peer_items: Vec<PeerItem> = store.list().iter().map(trusted_peer_to_item).collect();
                        drop(store);

                        let window_weak_ui = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_ui.upgrade() {
                                let logic = window.global::<AppLogic>();
                                logic.set_peers(ModelRc::new(VecModel::from(peer_items)));
                            }
                        });
                    }
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
        let app_start_time = self.app_start_time;
        let session_stats = self.session_stats.clone();
        let disconnected_peers = self.disconnected_peers.clone();
        let chat_store = self.chat_store.clone();
        let active_chat_peer = self.active_chat_peer.clone();

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

                // Spawn relay status monitoring task
                {
                    let endpoint = endpoint.clone();
                    let window_weak = window_weak.clone();
                    let shutdown = shutdown.clone();
                    tokio::spawn(async move {
                        let mut last_status = String::new();
                        loop {
                            if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }

                            // Check relay status
                            let (status, url) = match endpoint.relay_url() {
                                Some(url) => ("connected".to_string(), url.to_string()),
                                None => ("disconnected".to_string(), String::new()),
                            };

                            // Only update UI if status changed
                            if status != last_status {
                                last_status = status.clone();
                                let window_weak = window_weak.clone();
                                let url_clone = url.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak.upgrade() {
                                        window.global::<AppLogic>().set_relay_status(SharedString::from(&status));
                                        window.global::<AppLogic>().set_relay_url(SharedString::from(&url_clone));
                                    }
                                });
                            }

                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    });
                }

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

                    // Check if this peer is disconnected (user paused connections)
                    if let Some(ref p) = peer {
                        let disconnected = disconnected_peers.read().await;
                        if disconnected.contains(&p.id) {
                            info!("Rejecting connection from disconnected peer: {}", p.name);
                            let _ = conn.close().await;
                            continue;
                        }
                    }

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
                                if pending.bundle.nonce == nonce {
                                    info!("Valid TrustConfirm from {} ({}) - guest_mode={}", their_peer_info.name, remote_id, pending.bundle.guest_mode);
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

                                    // Create the peer - guest or trusted based on bundle settings
                                    let new_peer = if pending.bundle.guest_mode {
                                        // Create a guest peer with time-limited access
                                        let duration_hours = pending.bundle.guest_duration_hours.unwrap_or(24);
                                        let duration = chrono::Duration::hours(duration_hours as i64);

                                        // Build address from relay URL
                                        let address = PeerAddress::Iroh {
                                            relay_url: their_peer_info.relay_url.clone(),
                                            direct_addrs: vec![],
                                        };

                                        info!("Creating guest peer {} with {} hour duration", their_peer_info.name, duration_hours);

                                        TrustedPeer::new_guest(
                                            their_peer_info.endpoint_id.clone(),
                                            their_peer_info.name.clone(),
                                            Permissions::guest_default(), // Grant guest-level permissions
                                            their_permissions,  // Their permissions to us
                                            address,
                                            duration,
                                        )
                                    } else {
                                        // Create a fully trusted peer
                                        TrustedPeer::new_with_relay(
                                            their_peer_info.endpoint_id.clone(),
                                            their_peer_info.name.clone(),
                                            Permissions::all(), // We grant all permissions
                                            their_permissions,  // Their permissions to us
                                            their_peer_info.relay_url.clone(), // Store their relay URL
                                        )
                                    };

                                    // Add to peer store
                                    let mut store = peer_store.write().await;
                                    match store.add_or_update(new_peer.clone()) {
                                        Ok(updated) => {
                                            if updated {
                                                info!("Updated existing peer: {}", new_peer.name);
                                            } else {
                                                info!("Added new {} peer: {}", if new_peer.is_guest { "guest" } else { "trusted" }, new_peer.name);
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
                                    warn!("Invalid nonce from {}: expected {}, got {}", remote_id, pending.bundle.nonce, nonce);
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
                    match msg {
                        ControlMessage::PushOffer { transfer_id: offer_transfer_id, files, total_size } => {
                            info!(
                                "Received push offer from {} with {} files ({} bytes)",
                                peer.name, files.len(), total_size
                            );
                            // Extract values before moving files into msg
                            let total_size_for_stats = total_size;
                            let file_names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
                            // Reconstruct message for handler
                            let msg = ControlMessage::PushOffer {
                                transfer_id: offer_transfer_id,
                                files,
                                total_size,
                            };

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

                            // Check guest policy - if guest and auto-accept disabled, reject
                            // (Future: could show confirmation dialog instead)
                            if peer.is_guest {
                                let cfg = config.read().await;
                                if !cfg.guest_policy.auto_accept_guest_pushes {
                                    warn!("Auto-accept disabled for guest pushes from {}", peer.name);
                                    let _ = conn.send(&ControlMessage::PushResponse {
                                        transfer_id: String::new(),
                                        accepted: false,
                                        reason: Some("guest pushes require approval (disabled in security settings)".to_string()),
                                    }).await;
                                    update_status(&window_weak, &format!("Rejected push from guest {} (requires approval)", peer.name));
                                    continue;
                                }
                            }

                            // Get download directory
                            let download_dir = {
                                let cfg = config.read().await;
                                cfg.download_dir.clone()
                            };

                            // Create transfer for tracking (file_names already extracted above)
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
                            let session_stats_progress = session_stats.clone();
                            let config_progress = config.clone();
                            let window_weak_progress = window_weak.clone();
                            let transfer_id_progress = transfer_id.clone();
                            let peer_name = peer.name.clone();
                            tokio::spawn(async move {
                                while let Some(event) = progress_rx.recv().await {
                                    let keep_completed = config_progress.read().await.keep_completed_transfers;
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
                                            // Update session stats (this is a download/receive)
                                            {
                                                let mut stats = session_stats_progress.write().await;
                                                stats.bytes_downloaded += total_size_for_stats;
                                                stats.transfers_completed += 1;
                                            }
                                            update_session_stats_ui(&window_weak_progress, &session_stats_progress);
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Received files from {}", peer_name));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                            // Refresh UI to clear the completed transfer
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::Failed { error, .. } => {
                                            error!("Transfer failed: {}", error);
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Failed;
                                                t.error = Some(error.clone());
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Receive failed: {}", error));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                            // Refresh UI to clear the failed transfer
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::Cancelled { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Cancelled;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
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
                        ControlMessage::PullRequest { transfer_id: req_transfer_id, files } => {
                            info!("Received pull request from {} for {} files", peer.name, files.len());

                            // Extract file_names before moving files
                            let file_names: Vec<String> = files.iter().map(|f| {
                                std::path::Path::new(&f.path)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(&f.path)
                                    .to_string()
                            }).collect();
                            // Reconstruct message for handler
                            let msg = ControlMessage::PullRequest {
                                transfer_id: req_transfer_id,
                                files,
                            };

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

                            // Create transfer for tracking (outgoing for us, file_names already extracted above)
                            let transfer = Transfer::new_iroh_push(
                                file_names.clone(),
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
                            let session_stats_progress = session_stats.clone();
                            let config_progress = config.clone();
                            let window_weak_progress = window_weak.clone();
                            let transfer_id_progress = transfer_id.clone();
                            let peer_name = peer.name.clone();
                            tokio::spawn(async move {
                                while let Some(event) = progress_rx.recv().await {
                                    let keep_completed = config_progress.read().await.keep_completed_transfers;
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
                                            // Get total size before updating status
                                            let total_bytes = transfer_manager_progress.get(&transfer_id_progress).await
                                                .map(|t| t.total_size).unwrap_or(0);
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Completed;
                                                t.progress = 100.0;
                                                t.completed_at = Some(chrono::Utc::now());
                                            }).await;
                                            // Update session stats (this is an upload/send for incoming pull)
                                            {
                                                let mut stats = session_stats_progress.write().await;
                                                stats.bytes_uploaded += total_bytes;
                                                stats.transfers_completed += 1;
                                            }
                                            update_session_stats_ui(&window_weak_progress, &session_stats_progress);
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Pull completed for {}", peer_name));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                            // Refresh UI to clear the completed transfer
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::Failed { error, .. } => {
                                            error!("Pull transfer failed: {}", error);
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Failed;
                                                t.error = Some(error.clone());
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            update_status(&window_weak_progress, &format!("Pull failed: {}", error));
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                            // Refresh UI to clear the failed transfer
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        }
                                        TransferEvent::Cancelled { .. } => {
                                            let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                                t.status = TransferStatus::Cancelled;
                                            }).await;
                                            update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                            save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
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
                            info!("Received permissions update from {}: push={}, pull={}, browse={}, screen_view={}",
                                  peer.name, permissions.push, permissions.pull, permissions.browse, permissions.screen_view);

                            // Update our record of what they allow us to do
                            {
                                let mut store = peer_store.write().await;
                                if let Err(e) = store.update(&peer.id, |p| {
                                    p.their_permissions.push = permissions.push;
                                    p.their_permissions.pull = permissions.pull;
                                    p.their_permissions.browse = permissions.browse;
                                    p.their_permissions.status = permissions.status;
                                    p.their_permissions.screen_view = permissions.screen_view;
                                    p.their_permissions.screen_control = permissions.screen_control;
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
                            let _ = conn.send(&ControlMessage::Pong { timestamp }).await;
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
                            let download_path = cfg.download_dir.clone();
                            drop(cfg);

                            let hostname = std::env::var("HOSTNAME")
                                .or_else(|_| std::env::var("COMPUTERNAME"))
                                .unwrap_or_else(|_| "unknown".to_string());

                            // Calculate uptime since app start
                            let uptime_secs = app_start_time.elapsed().as_secs();

                            // Get disk space for download directory
                            let (free_space, total_space) = croh_core::get_disk_space(&download_path);

                            let _ = conn.send(&ControlMessage::StatusResponse {
                                hostname,
                                os: std::env::consts::OS.to_string(),
                                free_space,
                                total_space,
                                download_dir,
                                uptime: uptime_secs,
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
                            match croh_core::handle_speed_test_request(&mut conn, test_id.clone(), size).await {
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
                        ControlMessage::TrustRevoke { reason } => {
                            info!("Received TrustRevoke from {}: {}", peer.name, reason);
                            let peer_id = peer.id.clone();
                            let peer_name = peer.name.clone();
                            let window_weak_revoke = window_weak.clone();

                            // Update UI to show peer as revoked
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(window) = window_weak_revoke.upgrade() {
                                    let logic = window.global::<AppLogic>();
                                    let model = logic.get_peers();

                                    // Find and update the peer in the model
                                    for i in 0..model.row_count() {
                                        if let Some(mut peer_item) = model.row_data(i) {
                                            if peer_item.id.to_string() == peer_id {
                                                peer_item.revoked = true;
                                                peer_item.status = SharedString::from("revoked");
                                                model.set_row_data(i, peer_item);
                                                break;
                                            }
                                        }
                                    }

                                    // Update status bar
                                    logic.set_app_status(SharedString::from(format!("{} removed you as a peer", peer_name)));
                                }
                            });
                            let _ = conn.close().await;
                        }
                        ControlMessage::ExtensionRequest { requested_hours, reason } => {
                            info!("Received extension request from {}: {} hours, reason: {:?}", peer.name, requested_hours, reason);

                            // Check if this peer is a guest and if we allow extensions
                            let config_guard = config.read().await;
                            let guest_policy = config_guard.guest_policy.clone();
                            drop(config_guard);

                            let peer_id = peer.id.clone();
                            let peer_name = peer.name.clone();
                            let endpoint_id = peer.endpoint_id.clone();

                            // Calculate if extension is allowed
                            let can_extend = peer.is_guest
                                && guest_policy.allow_extensions
                                && peer.extension_count < guest_policy.max_extensions;

                            let requested_duration = chrono::Duration::hours(requested_hours as i64);
                            let max_duration = chrono::Duration::hours(guest_policy.max_duration_hours as i64);
                            let actual_duration = if requested_duration > max_duration {
                                max_duration
                            } else {
                                requested_duration
                            };

                            if can_extend {
                                // Apply the extension
                                let mut store_guard = peer_store.write().await;
                                match store_guard.extend_guest(&peer_id, actual_duration) {
                                    Ok(Some(new_expiry)) => {
                                        drop(store_guard);
                                        info!("Extended guest {} until {}", peer_name, new_expiry);

                                        // Send approval response
                                        let response = ControlMessage::ExtensionResponse {
                                            approved: true,
                                            new_expires_at: Some(new_expiry.timestamp()),
                                            reason: None,
                                        };
                                        let _ = conn.send(&response).await;

                                        // Update UI
                                        let new_expiry_str = new_expiry.format("%Y-%m-%d %H:%M").to_string();
                                        let time_remaining = {
                                            let now = chrono::Utc::now();
                                            if new_expiry > now {
                                                let duration = new_expiry - now;
                                                let total_secs = duration.num_seconds();
                                                if total_secs < 3600 {
                                                    format!("{}m", total_secs / 60)
                                                } else if total_secs < 86400 {
                                                    format!("{}h {}m", total_secs / 3600, (total_secs % 3600) / 60)
                                                } else {
                                                    format!("{}d {}h", total_secs / 86400, (total_secs % 86400) / 3600)
                                                }
                                            } else {
                                                "Expired".to_string()
                                            }
                                        };
                                        let _ = slint::invoke_from_event_loop({
                                            let window_weak = window_weak.clone();
                                            move || {
                                                if let Some(window) = window_weak.upgrade() {
                                                    let model = window.global::<AppLogic>().get_peers();
                                                    for i in 0..model.row_count() {
                                                        if let Some(mut item) = model.row_data(i) {
                                                            if item.id.as_str() == peer_id {
                                                                item.expires_at = SharedString::from(&new_expiry_str);
                                                                item.time_remaining = SharedString::from(&time_remaining);
                                                                item.extension_count += 1;
                                                                model.set_row_data(i, item);
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    window.global::<AppLogic>().set_app_status(
                                                        SharedString::from(format!("Extended {} access", peer_name))
                                                    );
                                                }
                                            }
                                        });
                                    }
                                    Ok(None) => {
                                        // Not a guest peer
                                        let response = ControlMessage::ExtensionResponse {
                                            approved: false,
                                            new_expires_at: None,
                                            reason: Some("Not a guest peer".to_string()),
                                        };
                                        let _ = conn.send(&response).await;
                                    }
                                    Err(e) => {
                                        error!("Failed to extend guest {}: {}", endpoint_id, e);
                                        let response = ControlMessage::ExtensionResponse {
                                            approved: false,
                                            new_expires_at: None,
                                            reason: Some(format!("Internal error: {}", e)),
                                        };
                                        let _ = conn.send(&response).await;
                                    }
                                }
                            } else {
                                // Extension not allowed
                                let reason_msg = if !peer.is_guest {
                                    "Not a guest peer".to_string()
                                } else if peer.extension_count >= guest_policy.max_extensions {
                                    "Maximum extensions reached".to_string()
                                } else {
                                    "Extensions not allowed".to_string()
                                };

                                let response = ControlMessage::ExtensionResponse {
                                    approved: false,
                                    new_expires_at: None,
                                    reason: Some(reason_msg),
                                };
                                let _ = conn.send(&response).await;
                            }

                            let _ = conn.close().await;
                        }
                        ControlMessage::PromotionRequest { reason } => {
                            info!("Received promotion request from {}: {:?}", peer.name, reason);

                            // Check if this peer is a guest and if we allow promotion requests
                            let config_guard = config.read().await;
                            let allow_requests = config_guard.guest_policy.allow_promotion_requests;
                            drop(config_guard);

                            let peer_id = peer.id.clone();
                            let peer_name = peer.name.clone();

                            if peer.is_guest && allow_requests {
                                // Mark promotion as pending - owner will need to approve manually
                                let mut store_guard = peer_store.write().await;
                                if let Err(e) = store_guard.set_promotion_pending(&peer_id, true) {
                                    error!("Failed to set promotion pending for {}: {}", peer_name, e);
                                }
                                drop(store_guard);

                                info!("Marked {} as pending promotion", peer_name);

                                // Update UI to show promotion pending badge
                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    let peer_id = peer_id.clone();
                                    let peer_name = peer_name.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            let model = window.global::<AppLogic>().get_peers();
                                            for i in 0..model.row_count() {
                                                if let Some(mut item) = model.row_data(i) {
                                                    if item.id.as_str() == peer_id {
                                                        item.promotion_pending = true;
                                                        model.set_row_data(i, item);
                                                        break;
                                                    }
                                                }
                                            }
                                            window.global::<AppLogic>().set_app_status(
                                                SharedString::from(format!("{} requested trusted status", peer_name))
                                            );
                                        }
                                    }
                                });

                                // Response will be sent later when owner approves/denies
                                // For now, just acknowledge receipt (no response needed per protocol)
                            } else {
                                // Send rejection immediately
                                let reason_msg = if !peer.is_guest {
                                    "Already a trusted peer".to_string()
                                } else {
                                    "Promotion requests not allowed".to_string()
                                };

                                let response = ControlMessage::PromotionResponse {
                                    approved: false,
                                    reason: Some(reason_msg),
                                };
                                let _ = conn.send(&response).await;
                            }

                            let _ = conn.close().await;
                        }
                        // ==================== Introduction Message Handlers ====================
                        ControlMessage::IntroductionOffer { introduction_id, network_id: _, peer: offered_peer, message } => {
                            // Someone wants to introduce us to another peer
                            info!("Received introduction offer from {}: {:?} to connect with {}",
                                  peer.name, message, offered_peer.name);

                            // Check if we already know this peer
                            let store_guard = peer_store.read().await;
                            let already_known = store_guard.list().iter()
                                .any(|p| p.endpoint_id == offered_peer.endpoint_id);
                            drop(store_guard);

                            if already_known {
                                // Already know this peer, decline
                                let response = ControlMessage::IntroductionOfferResponse {
                                    introduction_id,
                                    accepted: false,
                                    reason: Some("Already connected to this peer".to_string()),
                                };
                                let _ = conn.send(&response).await;
                            } else {
                                // Accept the introduction offer
                                // In a full implementation, this would show a UI prompt
                                // For now, auto-accept from trusted peers
                                info!("Accepting introduction offer to connect with {}", offered_peer.name);
                                let response = ControlMessage::IntroductionOfferResponse {
                                    introduction_id: introduction_id.clone(),
                                    accepted: true,
                                    reason: None,
                                };
                                let _ = conn.send(&response).await;

                                // Update UI status
                                let offered_name = offered_peer.name.clone();
                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            window.global::<AppLogic>().set_app_status(
                                                SharedString::from(format!("Accepted introduction to {}", offered_name))
                                            );
                                        }
                                    }
                                });
                            }

                            let _ = conn.close().await;
                        }
                        ControlMessage::IntroductionRequest { introduction_id, network_id: _, peer: requesting_peer, message } => {
                            // A network member is asking if we want to be introduced to a peer
                            info!("Received introduction request from {}: {:?} to connect with {}",
                                  peer.name, message, requesting_peer.name);

                            // Check if we already know this peer
                            let store_guard = peer_store.read().await;
                            let already_known = store_guard.list().iter()
                                .any(|p| p.endpoint_id == requesting_peer.endpoint_id);
                            drop(store_guard);

                            if already_known {
                                // Already know this peer, decline
                                let response = ControlMessage::IntroductionRequestResponse {
                                    introduction_id,
                                    accepted: false,
                                    reason: Some("Already connected to this peer".to_string()),
                                };
                                let _ = conn.send(&response).await;
                            } else {
                                // Accept the introduction
                                // In a full implementation, this would show a UI prompt
                                info!("Accepting introduction request to connect with {}", requesting_peer.name);
                                let response = ControlMessage::IntroductionRequestResponse {
                                    introduction_id: introduction_id.clone(),
                                    accepted: true,
                                    reason: None,
                                };
                                let _ = conn.send(&response).await;

                                // Update UI status
                                let requesting_name = requesting_peer.name.clone();
                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            window.global::<AppLogic>().set_app_status(
                                                SharedString::from(format!("Accepted introduction to {}", requesting_name))
                                            );
                                        }
                                    }
                                });
                            }

                            let _ = conn.close().await;
                        }
                        ControlMessage::IntroductionComplete { introduction_id: _, network_id: _, peer: new_peer, trust_bundle_json: _ } => {
                            // Introduction was successful - we can now connect to the new peer
                            info!("Introduction complete! Can now connect to {}", new_peer.name);

                            // Add the new peer as a trusted peer
                            let new_peer_clone = new_peer.clone();
                            let peer_store_clone = peer_store.clone();
                            let window_weak_clone = window_weak.clone();
                            let peer_status_clone = peer_status.clone();

                            tokio::spawn(async move {
                                // Create a new TrustedPeer from the PeerInfo
                                let mut store = peer_store_clone.write().await;

                                // Check if already exists
                                if store.list().iter().any(|p| p.endpoint_id == new_peer_clone.endpoint_id) {
                                    info!("Peer {} already exists, skipping", new_peer_clone.name);
                                    return;
                                }

                                // Create the peer using the constructor
                                let trusted_peer = TrustedPeer::new_with_relay(
                                    new_peer_clone.endpoint_id.clone(),
                                    new_peer_clone.name.clone(),
                                    Permissions::all(),  // Grant full permissions
                                    Permissions::all(),  // Assume they grant full permissions
                                    new_peer_clone.relay_url.clone(),
                                );

                                if let Err(e) = store.add(trusted_peer.clone()) {
                                    error!("Failed to add introduced peer: {}", e);
                                    return;
                                }

                                // Save the peer store
                                if let Err(e) = store.save() {
                                    error!("Failed to save peer store: {}", e);
                                }
                                drop(store);

                                info!("Added {} as trusted peer via introduction", new_peer_clone.name);

                                // Update the peer list UI
                                update_peers_ui_with_status(&window_weak_clone, &peer_store_clone, &peer_status_clone).await;

                                // Update status bar
                                update_status(&window_weak_clone, &format!("Connected to {} via introduction", new_peer_clone.name));
                            });

                            let _ = conn.close().await;
                        }
                        ControlMessage::IntroductionFailed { introduction_id, reason } => {
                            // Introduction failed
                            info!("Introduction {} failed: {}", introduction_id, reason);

                            let _ = slint::invoke_from_event_loop({
                                let window_weak = window_weak.clone();
                                let reason = reason.clone();
                                move || {
                                    if let Some(window) = window_weak.upgrade() {
                                        window.global::<AppLogic>().set_app_status(
                                            SharedString::from(format!("Introduction failed: {}", reason))
                                        );
                                    }
                                }
                            });

                            let _ = conn.close().await;
                        }
                        // ==================== Chat Message Handlers ====================
                        ControlMessage::ChatMessage { message_id, content, sequence, sent_at } => {
                            info!("Received chat message from {}: {} bytes", peer.name, content.len());

                            // Check if chat is allowed from this peer
                            if !peer.permissions_granted.chat {
                                warn!("Chat not allowed from {}", peer.name);
                                let _ = conn.close().await;
                                continue;
                            }

                            // Get our endpoint ID
                            let our_id = {
                                let id_guard = identity.read().await;
                                match id_guard.as_ref() {
                                    Some(id) => id.endpoint_id.clone(),
                                    None => {
                                        error!("No identity for chat");
                                        continue;
                                    }
                                }
                            };

                            // Create handler and process message
                            let handler = ChatHandler::new(chat_store.clone(), our_id.clone());
                            match handler.handle_chat_message(
                                &peer.endpoint_id,
                                &peer.name,
                                message_id.clone(),
                                content.clone(),
                                sequence,
                                sent_at,
                            ) {
                                Ok(events) => {
                                    // Send delivery receipt
                                    let _ = conn.send(&ControlMessage::ChatDelivered {
                                        message_ids: vec![message_id.clone()],
                                    }).await;

                                    // Update UI if chat is open with this peer
                                    let active = active_chat_peer.read().await;
                                    let is_active = active.as_ref().map(|id| id == &peer.endpoint_id).unwrap_or(false);
                                    drop(active);

                                    if is_active {
                                        // Reload messages in the chat panel
                                        let mut messages = chat_store.get_messages(&peer.endpoint_id, 50, None)
                                            .unwrap_or_default();

                                        // Reverse to get chronological order (oldest first for display)
                                        messages.reverse();

                                        let ui_messages: Vec<ChatMessageItem> = messages
                                            .iter()
                                            .map(|m| {
                                                let is_mine = m.sender_id == our_id;
                                                ChatMessageItem {
                                                    id: SharedString::from(m.id.as_str()),
                                                    content: SharedString::from(&m.content),
                                                    is_mine,
                                                    timestamp: SharedString::from(format_chat_time(m.sent_at)),
                                                    status: SharedString::from(m.status.as_str()),
                                                    show_date_divider: false,
                                                    date_label: SharedString::default(),
                                                }
                                            })
                                            .collect();

                                        let _ = slint::invoke_from_event_loop({
                                            let window_weak = window_weak.clone();
                                            move || {
                                                if let Some(window) = window_weak.upgrade() {
                                                    let logic = window.global::<AppLogic>();
                                                    logic.set_chat_messages(ModelRc::new(VecModel::from(ui_messages)));
                                                    logic.set_chat_peer_typing(false);
                                                }
                                            }
                                        });

                                        // Mark as read since chat is open
                                        let _ = handler.mark_conversation_read(&peer.endpoint_id);
                                    } else {
                                        // Update unread count in peer list
                                        let unread = chat_store.get_unread_count(&peer.endpoint_id).unwrap_or(0);
                                        let peer_id = peer.endpoint_id.clone();
                                        let _ = slint::invoke_from_event_loop({
                                            let window_weak = window_weak.clone();
                                            move || {
                                                if let Some(window) = window_weak.upgrade() {
                                                    let logic = window.global::<AppLogic>();
                                                    let model = logic.get_peers();
                                                    for i in 0..model.row_count() {
                                                        if let Some(mut item) = model.row_data(i) {
                                                            if item.endpoint_id.as_str() == peer_id {
                                                                item.unread_count = unread as i32;
                                                                model.set_row_data(i, item);
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        });
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to handle chat message: {}", e);
                                }
                            }

                            let _ = conn.close().await;
                        }
                        ControlMessage::ChatDelivered { message_ids } => {
                            debug!("Received delivery receipt for {} messages from {}", message_ids.len(), peer.name);

                            // Mark messages as delivered
                            let _ = chat_store.mark_delivered(&message_ids);

                            // Update UI if chat is open
                            let active = active_chat_peer.read().await;
                            let is_active = active.as_ref().map(|id| id == &peer.endpoint_id).unwrap_or(false);
                            drop(active);

                            if is_active {
                                let peer_id = peer.endpoint_id.clone();
                                let message_ids_clone = message_ids.clone();
                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            let logic = window.global::<AppLogic>();
                                            let current = logic.get_chat_messages();
                                            let messages: Vec<ChatMessageItem> = (0..current.row_count())
                                                .filter_map(|i| {
                                                    let mut m = current.row_data(i)?;
                                                    if message_ids_clone.contains(&m.id.to_string()) {
                                                        m.status = SharedString::from("delivered");
                                                    }
                                                    Some(m)
                                                })
                                                .collect();
                                            logic.set_chat_messages(ModelRc::new(VecModel::from(messages)));
                                        }
                                    }
                                });
                            }

                            let _ = conn.close().await;
                        }
                        ControlMessage::ChatRead { message_ids, up_to_sequence: _ } => {
                            debug!("Received read receipt for {} messages from {}", message_ids.len(), peer.name);

                            // Mark messages as read
                            let _ = chat_store.mark_read(&message_ids);

                            // Update UI if chat is open
                            let active = active_chat_peer.read().await;
                            let is_active = active.as_ref().map(|id| id == &peer.endpoint_id).unwrap_or(false);
                            drop(active);

                            if is_active {
                                let message_ids_clone = message_ids.clone();
                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            let logic = window.global::<AppLogic>();
                                            let current = logic.get_chat_messages();
                                            let messages: Vec<ChatMessageItem> = (0..current.row_count())
                                                .filter_map(|i| {
                                                    let mut m = current.row_data(i)?;
                                                    if message_ids_clone.contains(&m.id.to_string()) {
                                                        m.status = SharedString::from("read");
                                                    }
                                                    Some(m)
                                                })
                                                .collect();
                                            logic.set_chat_messages(ModelRc::new(VecModel::from(messages)));
                                        }
                                    }
                                });
                            }

                            let _ = conn.close().await;
                        }
                        ControlMessage::ChatTyping { is_typing } => {
                            debug!("Received typing indicator from {}: {}", peer.name, is_typing);

                            // Update UI if chat is open with this peer
                            let active = active_chat_peer.read().await;
                            let is_active = active.as_ref().map(|id| id == &peer.endpoint_id).unwrap_or(false);
                            drop(active);

                            if is_active {
                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            window.global::<AppLogic>().set_chat_peer_typing(is_typing);
                                        }
                                    }
                                });
                            }

                            let _ = conn.close().await;
                        }
                        // ==================== Screen Streaming Handlers ====================
                        ControlMessage::DisplayListRequest => {
                            debug!("Received display list request from {}", peer.name);

                            // Check permission
                            if !peer.permissions_granted.screen_view {
                                warn!("Screen view not allowed from {}", peer.name);
                                let _ = conn.close().await;
                                continue;
                            }

                            // For now, return a simple response (actual implementation in daemon)
                            // The GUI doesn't serve screen streams, it only views them
                            let response = ControlMessage::DisplayListResponse {
                                displays: vec![],
                                backend: "unsupported".to_string(),
                            };
                            let _ = conn.send(&response).await;
                            let _ = conn.close().await;
                        }
                        ControlMessage::ScreenStreamRequest { stream_id, display_id, compression, quality, target_fps } => {
                            info!("Received screen stream request from {} (stream_id={})", peer.name, stream_id);

                            // Get screen stream settings from config
                            let cfg = config.read().await;
                            let screen_settings = cfg.screen_stream.clone();
                            drop(cfg);

                            // Check if screen streaming is enabled
                            if !screen_settings.enabled {
                                warn!("Screen streaming disabled, rejecting request from {}", peer.name);
                                let response = ControlMessage::ScreenStreamResponse {
                                    stream_id,
                                    accepted: false,
                                    reason: Some("Screen streaming is disabled on this device".into()),
                                    displays: vec![],
                                    compression: None,
                                };
                                let _ = conn.send(&response).await;
                                let _ = conn.close().await;
                                continue;
                            }

                            // Check if peer has screen_view permission
                            if !peer.permissions_granted.screen_view {
                                warn!("Screen view not allowed from {}", peer.name);
                                let response = ControlMessage::ScreenStreamResponse {
                                    stream_id,
                                    accepted: false,
                                    reason: Some("screen_view permission not granted".into()),
                                    displays: vec![],
                                    compression: None,
                                };
                                let _ = conn.send(&response).await;
                                let _ = conn.close().await;
                                continue;
                            }

                            // Handle the stream request (this will capture and send frames)
                            match handle_screen_stream_request(
                                &mut conn,
                                stream_id.clone(),
                                display_id,
                                compression,
                                quality,
                                target_fps,
                                &peer,
                                &screen_settings,
                            ).await {
                                Ok(()) => {
                                    info!("Screen stream {} ended normally", stream_id);
                                }
                                Err(e) => {
                                    error!("Screen stream {} error: {}", stream_id, e);
                                }
                            }
                            // Connection is closed by the handler or when stream ends
                        }
                        ControlMessage::ScreenFrame { stream_id, metadata } => {
                            // Received a screen frame - this should be handled by the viewer
                            // For now, just log it
                            debug!(
                                "Received screen frame from {}: stream_id={}, seq={}, {}x{}",
                                peer.name, stream_id, metadata.sequence, metadata.width, metadata.height
                            );
                            // TODO: Forward to active screen viewer if stream_id matches
                            // This will be implemented when viewer connection is wired up
                            let _ = conn.close().await;
                        }
                        ControlMessage::ScreenStreamStop { stream_id, reason } => {
                            info!("Received screen stream stop from {}: stream_id={}, reason={}", peer.name, stream_id, reason);
                            // TODO: Notify viewer that stream has ended
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

                // Privacy settings
                let show_session_stats = config_guard.show_session_stats;

                // Transfer list settings
                let keep_completed_transfers = config_guard.keep_completed_transfers;

                // Security settings
                let security_posture = config_guard.security_posture.to_ui_string().to_string();

                // Network settings
                let relay_preference = config_guard.relay_preference.to_ui_string().to_string();

                // Screen streaming settings
                let screen_streaming_enabled = config_guard.screen_stream.enabled;
                let screen_allow_input = config_guard.screen_stream.allow_input;

                // Device identity
                let device_nickname = config_guard.device_nickname.clone().unwrap_or_default();
                let device_hostname = Config::get_hostname();

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
                            device_nickname: SharedString::from(device_nickname),
                            device_hostname: SharedString::from(device_hostname),
                            hash_algorithm: SharedString::from(hash_algorithm),
                            curve: SharedString::from(curve),
                            throttle: SharedString::from(throttle),
                            no_local,
                            browse_show_hidden,
                            browse_show_protected,
                            browse_exclude_patterns: SharedString::from(browse_exclude_patterns),
                            dnd_mode: SharedString::from(dnd_mode),
                            dnd_message: SharedString::from(dnd_message),
                            show_session_stats,
                            keep_completed_transfers,
                            security_posture: SharedString::from(security_posture),
                            relay_preference: SharedString::from(relay_preference),
                            screen_streaming_enabled,
                            screen_allow_input,
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
        let config = self.config.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                // Load or create identity
                match Identity::load_or_create() {
                    Ok(mut id) => {
                        let endpoint_id = id.endpoint_id.clone();
                        // Truncate for display
                        let display_id = if endpoint_id.len() > 12 {
                            format!("{}...{}", &endpoint_id[..6], &endpoint_id[endpoint_id.len()-6..])
                        } else {
                            endpoint_id
                        };

                        // Sync identity name with configured nickname if set
                        let config_guard = config.read().await;
                        let display_name = config_guard.get_device_display_name();
                        drop(config_guard);

                        if id.name != display_name {
                            if let Err(e) = id.set_name(display_name.clone()) {
                                error!("Failed to sync identity name with config: {}", e);
                            } else {
                                info!("Synced identity name to: {}", display_name);
                            }
                        }

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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;

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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;
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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;

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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;
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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;

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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;
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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;

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
                                            let keep_completed = config.read().await.keep_completed_transfers;
                                            save_to_history(&transfer_history, &transfer_manager, &transfer_id, keep_completed).await;
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
                        let show_session_stats = config_guard.show_session_stats;
                        let keep_completed_transfers = config_guard.keep_completed_transfers;
                        let security_posture = config_guard.security_posture.to_ui_string().to_string();
                        let relay_preference = config_guard.relay_preference.to_ui_string().to_string();
                        let screen_streaming_enabled = config_guard.screen_stream.enabled;
                        let screen_allow_input = config_guard.screen_stream.allow_input;
                        let device_nickname = config_guard.device_nickname.clone().unwrap_or_default();
                        let device_hostname = Config::get_hostname();
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
                                    device_nickname: SharedString::from(device_nickname),
                                    device_hostname: SharedString::from(device_hostname),
                                    hash_algorithm: SharedString::from(hash_algorithm),
                                    curve: SharedString::from(curve),
                                    throttle: SharedString::from(throttle),
                                    no_local,
                                    browse_show_hidden,
                                    browse_show_protected,
                                    browse_exclude_patterns: SharedString::from(browse_exclude_patterns),
                                    dnd_mode: SharedString::from(dnd_mode),
                                    dnd_message: SharedString::from(dnd_message),
                                    show_session_stats,
                                    keep_completed_transfers,
                                    security_posture: SharedString::from(security_posture),
                                    relay_preference: SharedString::from(relay_preference),
                                    screen_streaming_enabled,
                                    screen_allow_input,
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

        // Set security posture callback
        window.global::<AppLogic>().on_set_security_posture({
            let config = config.clone();
            let window_weak = window_weak.clone();

            move |posture| {
                let posture_str = posture.to_string();
                let config = config.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Update config with new posture and regenerate guest policy
                        {
                            let mut config_guard = config.write().await;
                            let new_posture = croh_core::SecurityPosture::from_ui_string(&posture_str);
                            config_guard.security_posture = new_posture;
                            // Apply the new posture's default guest policy
                            config_guard.guest_policy = new_posture.default_guest_policy();
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save security posture: {}", e);
                            }
                        }

                        // Update UI
                        let _ = slint::invoke_from_event_loop({
                            let window_weak = window_weak.clone();
                            let posture_str = posture_str.clone();
                            move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let mut settings = window.global::<AppLogic>().get_settings();
                                    settings.security_posture = SharedString::from(&posture_str);
                                    window.global::<AppLogic>().set_settings(settings);
                                }
                            }
                        });

                        info!("Security posture set to: {}", posture_str);
                    });
                });
            }
        });

        // Set relay preference callback
        window.global::<AppLogic>().on_set_relay_preference({
            let config = config.clone();
            let window_weak = window_weak.clone();

            move |preference| {
                let preference_str = preference.to_string();
                let config = config.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Update config with new relay preference
                        {
                            let mut config_guard = config.write().await;
                            config_guard.relay_preference = croh_core::RelayPreference::from_ui_string(&preference_str);
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save relay preference: {}", e);
                            }
                        }

                        // Update UI
                        let _ = slint::invoke_from_event_loop({
                            let window_weak = window_weak.clone();
                            let preference_str = preference_str.clone();
                            move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let mut settings = window.global::<AppLogic>().get_settings();
                                    settings.relay_preference = SharedString::from(&preference_str);
                                    window.global::<AppLogic>().set_settings(settings);
                                }
                            }
                        });

                        info!("Relay preference set to: {}", preference_str);
                    });
                });
            }
        });

        // Set screen streaming settings callback
        window.global::<AppLogic>().on_set_screen_streaming({
            let config = config.clone();
            let window_weak = window_weak.clone();

            move |enabled, allow_input| {
                let config = config.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Update config with new screen streaming settings
                        {
                            let mut config_guard = config.write().await;
                            config_guard.screen_stream.enabled = enabled;
                            config_guard.screen_stream.allow_input = allow_input;
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save screen streaming settings: {}", e);
                            }
                        }

                        // Update UI
                        let _ = slint::invoke_from_event_loop({
                            let window_weak = window_weak.clone();
                            move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let mut settings = window.global::<AppLogic>().get_settings();
                                    settings.screen_streaming_enabled = enabled;
                                    settings.screen_allow_input = allow_input;
                                    window.global::<AppLogic>().set_settings(settings);
                                }
                            }
                        });

                        info!("Screen streaming settings: enabled={}, allow_input={}", enabled, allow_input);
                    });
                });
            }
        });

        // Set device nickname callback
        window.global::<AppLogic>().on_set_device_nickname({
            let config = config.clone();
            let window_weak = window_weak.clone();
            let identity = self.identity.clone();

            move |nickname| {
                let nickname_str = nickname.to_string();
                info!("Setting device nickname to: '{}'", nickname_str);
                let config = config.clone();
                let window_weak = window_weak.clone();
                let identity = identity.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Update config
                        let display_name = {
                            let mut config_guard = config.write().await;
                            config_guard.device_nickname = if nickname_str.trim().is_empty() {
                                None
                            } else {
                                Some(nickname_str.clone())
                            };
                            let display_name = config_guard.get_device_display_name();
                            info!("Saving device nickname, display_name will be: {}", display_name);
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save device nickname: {}", e);
                            } else {
                                info!("Device nickname saved successfully");
                            }
                            display_name
                        };

                        // Update identity name to match
                        {
                            let mut id_guard = identity.write().await;
                            if let Some(ref mut id) = *id_guard {
                                if let Err(e) = id.set_name(display_name.clone()) {
                                    error!("Failed to update identity name: {}", e);
                                }
                            }
                        }

                        // Update UI
                        let _ = slint::invoke_from_event_loop({
                            let window_weak = window_weak.clone();
                            let nickname_str = nickname_str.clone();
                            move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let mut settings = window.global::<AppLogic>().get_settings();
                                    settings.device_nickname = SharedString::from(&nickname_str);
                                    window.global::<AppLogic>().set_settings(settings);
                                }
                            }
                        });

                        info!("Device nickname set to: {}", if nickname_str.is_empty() { "(hostname)" } else { &nickname_str });
                    });
                });
            }
        });

        // Toggle session stats callback
        window.global::<AppLogic>().on_toggle_session_stats({
            let config = config.clone();
            let window_weak = window_weak.clone();

            move || {
                let config = config.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Toggle the setting
                        let new_value = {
                            let mut config_guard = config.write().await;
                            config_guard.show_session_stats = !config_guard.show_session_stats;
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save session stats setting: {}", e);
                            }
                            config_guard.show_session_stats
                        };

                        // Update UI
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let mut settings = window.global::<AppLogic>().get_settings();
                                settings.show_session_stats = new_value;
                                window.global::<AppLogic>().set_settings(settings);
                            }
                        });
                    });
                });
            }
        });

        // Toggle keep completed transfers callback
        window.global::<AppLogic>().on_toggle_keep_completed_transfers({
            let config = config.clone();
            let window_weak = window_weak.clone();

            move || {
                let config = config.clone();
                let window_weak = window_weak.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Toggle the setting
                        let new_value = {
                            let mut config_guard = config.write().await;
                            config_guard.keep_completed_transfers = !config_guard.keep_completed_transfers;
                            if let Err(e) = config_guard.save() {
                                error!("Failed to save keep_completed_transfers setting: {}", e);
                            }
                            config_guard.keep_completed_transfers
                        };

                        // Update UI
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let mut settings = window.global::<AppLogic>().get_settings();
                                settings.keep_completed_transfers = new_value;
                                window.global::<AppLogic>().set_settings(settings);
                            }
                        });
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

            move |guest_mode: bool, duration_hours: i32| {
                info!("Initiating trust (guest_mode={}, duration_hours={})...", guest_mode, duration_hours);
                let window_weak = window_weak.clone();
                let identity = identity.clone();
                let shared_endpoint = shared_endpoint.clone();
                let trust_in_progress = trust_in_progress.clone();
                let pending_trust = pending_trust.clone();
                let config = config.clone();
                let peer_store = peer_store.clone();
                let peer_status = peer_status.clone();
                let duration_hours = duration_hours as u32;

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
                        // Use guest mode if requested with specified duration
                        let bundle = if guest_mode {
                            let duration = if duration_hours > 0 { Some(duration_hours) } else { None };
                            info!("Creating guest trust bundle with duration: {:?} hours", duration);
                            TrustBundle::new_guest(&id, relay_url, duration)
                        } else {
                            TrustBundle::new_with_relay(&id, relay_url)
                        };
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
                                            let (result_tx, result_rx) = oneshot::channel();

                                            {
                                                let mut pending_guard = pending_trust.write().await;
                                                *pending_guard = Some(PendingTrust {
                                                    bundle: bundle.clone(),
                                                    result_tx,
                                                });
                                            }

                                            info!("Set pending trust with bundle (guest_mode={}), waiting for background listener to handle connection...", bundle.guest_mode);

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
            let peer_status = self.peer_status.clone();
            let disconnected_peers = self.disconnected_peers.clone();
            let shared_endpoint = self.shared_endpoint.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Removing peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let peer_status = peer_status.clone();
                let disconnected_peers = disconnected_peers.clone();
                let shared_endpoint = shared_endpoint.clone();

                // Send TrustRevoke, remove from store, and update UI in background
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the peer info before removing (for TrustRevoke)
                        let peer = {
                            let store = peer_store.read().await;
                            store.find_by_id(&id_str).cloned()
                        };

                        // Send TrustRevoke to notify the peer (best effort)
                        if let Some(peer) = &peer {
                            let endpoint_guard = shared_endpoint.read().await;
                            if let Some(endpoint) = endpoint_guard.as_ref() {
                                match endpoint.connect(&peer.endpoint_id).await {
                                    Ok(mut conn) => {
                                        let revoke = ControlMessage::TrustRevoke {
                                            reason: "Peer removed".to_string(),
                                        };
                                        if let Err(e) = conn.send(&revoke).await {
                                            warn!("Failed to send TrustRevoke: {}", e);
                                        } else {
                                            info!("Sent TrustRevoke to {}", peer.name);
                                        }
                                        // Give time for message to be delivered
                                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                        let _ = conn.close().await;
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

                        // Remove from disconnected set
                        {
                            let mut disconnected = disconnected_peers.write().await;
                            disconnected.remove(&id_str);
                        }

                        // Update UI by refreshing the peers list
                        update_peers_ui_with_status(&window_weak, &peer_store, &peer_status).await;
                        update_status(&window_weak, "Peer removed");
                    });
                });
            }
        });

        // Revoke peer callback - sends TrustRevoke but keeps peer in store
        window.global::<AppLogic>().on_revoke_peer({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let peer_status = self.peer_status.clone();
            let revoked_by_us = self.revoked_by_us.clone();
            let shared_endpoint = self.shared_endpoint.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Revoking trust for peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let peer_status = peer_status.clone();
                let revoked_by_us = revoked_by_us.clone();
                let shared_endpoint = shared_endpoint.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the peer info for TrustRevoke
                        let peer = {
                            let store = peer_store.read().await;
                            store.find_by_id(&id_str).cloned()
                        };

                        // Send TrustRevoke to notify the peer
                        if let Some(peer) = &peer {
                            let endpoint_guard = shared_endpoint.read().await;
                            if let Some(endpoint) = endpoint_guard.as_ref() {
                                match endpoint.connect(&peer.endpoint_id).await {
                                    Ok(mut conn) => {
                                        let revoke = ControlMessage::TrustRevoke {
                                            reason: "Trust revoked by user".to_string(),
                                        };
                                        if let Err(e) = conn.send(&revoke).await {
                                            warn!("Failed to send TrustRevoke: {}", e);
                                        } else {
                                            info!("Sent TrustRevoke to {}", peer.name);
                                        }
                                        // Give time for message to be delivered
                                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                        let _ = conn.close().await;
                                    }
                                    Err(e) => {
                                        debug!("Could not connect to peer for TrustRevoke: {}", e);
                                    }
                                }
                            }
                        }

                        // Mark as revoked by us (don't delete from store)
                        {
                            let mut revoked = revoked_by_us.write().await;
                            revoked.insert(id_str.clone());
                        }

                        // Set the we_revoked flag directly on the UI item
                        let id_str_clone = id_str.clone();
                        let window_weak_set = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_set.upgrade() {
                                let logic = window.global::<AppLogic>();
                                let model = logic.get_peers();
                                for i in 0..model.row_count() {
                                    if let Some(mut peer) = model.row_data(i) {
                                        if peer.id.to_string() == id_str_clone {
                                            peer.we_revoked = true;
                                            peer.status = SharedString::from("offline");
                                            model.set_row_data(i, peer);
                                            break;
                                        }
                                    }
                                }
                            }
                        });

                        update_status(&window_weak, "Trust revoked");
                    });
                });
            }
        });

        // Rename peer callback
        window.global::<AppLogic>().on_rename_peer({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let peer_status = self.peer_status.clone();

            move |id, new_name| {
                let id_str = id.to_string();
                let new_name_str = new_name.to_string();
                info!("Renaming peer {} to: {}", id_str, new_name_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let peer_status = peer_status.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Update the peer's name
                        {
                            let mut store = peer_store.write().await;
                            if let Err(e) = store.update(&id_str, |peer| {
                                peer.name = new_name_str.clone();
                            }) {
                                error!("Failed to rename peer: {}", e);
                                update_status(&window_weak, &format!("Error: {}", e));
                                return;
                            }
                        }

                        // Update UI
                        update_peers_ui_with_status(&window_weak, &peer_store, &peer_status).await;
                        update_status(&window_weak, &format!("Peer renamed to {}", new_name_str));
                    });
                });
            }
        });

        // Disconnect peer callback - pauses connections to/from peer (does not remove)
        window.global::<AppLogic>().on_disconnect_peer({
            let window_weak = window_weak.clone();
            let disconnected_peers = self.disconnected_peers.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Pausing connections with peer: {}", id_str);

                // Add to disconnected set
                let disconnected_peers_clone = disconnected_peers.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        let mut disconnected = disconnected_peers_clone.write().await;
                        disconnected.insert(id_str);
                    });
                });

                // Update UI model to show disconnected state (get fresh model from window)
                if let Some(window) = window_weak.upgrade() {
                    let model = window.global::<AppLogic>().get_peers();
                    for i in 0..model.row_count() {
                        if let Some(mut peer) = model.row_data(i) {
                            if peer.id.to_string() == id.to_string() {
                                peer.connected = false;
                                peer.status = "offline".into();
                                model.set_row_data(i, peer);
                                break;
                            }
                        }
                    }
                }

                update_status(&window_weak, "Paused connections with peer");
            }
        });

        // Connect peer callback - resumes connections to/from peer
        window.global::<AppLogic>().on_connect_peer({
            let window_weak = window_weak.clone();
            let disconnected_peers = self.disconnected_peers.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Resuming connections with peer: {}", id_str);

                // Remove from disconnected set
                let disconnected_peers_clone = disconnected_peers.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        let mut disconnected = disconnected_peers_clone.write().await;
                        disconnected.remove(&id_str);
                    });
                });

                // Update UI model to show connecting state (get fresh model from window)
                if let Some(window) = window_weak.upgrade() {
                    let model = window.global::<AppLogic>().get_peers();
                    for i in 0..model.row_count() {
                        if let Some(mut peer) = model.row_data(i) {
                            if peer.id.to_string() == id.to_string() {
                                peer.connected = true;
                                peer.status = "connecting".into();
                                model.set_row_data(i, peer);
                                break;
                            }
                        }
                    }
                }

                update_status(&window_weak, "Resumed connections with peer");
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

                                status.connection_type = if peer.relay_url().is_some() {
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
                                            hostname, os, free_space, total_space, version, uptime, active_transfers, ..
                                        }) = conn.recv().await {
                                            let mut status_map = peer_status.write().await;
                                            if let Some(status) = status_map.get_mut(&id_str) {
                                                status.peer_hostname = Some(hostname);
                                                status.peer_os = Some(os);
                                                status.peer_version = Some(version);
                                                status.peer_free_space = Some(free_space);
                                                status.peer_total_space = Some(total_space);
                                                status.peer_uptime = Some(uptime);
                                                status.peer_active_transfers = Some(active_transfers);
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

        // Extend guest access callback
        window.global::<AppLogic>().on_extend_guest({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let config = config.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Extending guest access for peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let config = config.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get the extension duration from config
                        let config_guard = config.read().await;
                        let extension_hours = config_guard.guest_policy.default_duration_hours;
                        drop(config_guard);

                        let extension_duration = chrono::Duration::hours(extension_hours as i64);

                        // Extend the guest
                        let mut store = peer_store.write().await;
                        match store.extend_guest(&id_str, extension_duration) {
                            Ok(Some(new_expiry)) => {
                                info!("Extended guest {} until {}", id_str, new_expiry);

                                // Update UI
                                let new_expiry_str = new_expiry.format("%Y-%m-%d %H:%M").to_string();
                                let time_remaining = {
                                    let now = chrono::Utc::now();
                                    if new_expiry > now {
                                        let duration = new_expiry - now;
                                        let total_secs = duration.num_seconds();
                                        if total_secs < 3600 {
                                            format!("{}m", total_secs / 60)
                                        } else if total_secs < 86400 {
                                            format!("{}h {}m", total_secs / 3600, (total_secs % 3600) / 60)
                                        } else {
                                            format!("{}d {}h", total_secs / 86400, (total_secs % 86400) / 3600)
                                        }
                                    } else {
                                        "Expired".to_string()
                                    }
                                };

                                // Get the new extension count
                                let new_ext_count = store.find_by_id(&id_str)
                                    .map(|p| p.extension_count as i32)
                                    .unwrap_or(0);
                                drop(store);

                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    let id_str = id_str.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            let model = window.global::<AppLogic>().get_peers();
                                            for i in 0..model.row_count() {
                                                if let Some(mut item) = model.row_data(i) {
                                                    if item.id.as_str() == id_str {
                                                        item.expires_at = SharedString::from(&new_expiry_str);
                                                        item.time_remaining = SharedString::from(&time_remaining);
                                                        item.extension_count = new_ext_count;
                                                        model.set_row_data(i, item);
                                                        break;
                                                    }
                                                }
                                            }
                                            window.global::<AppLogic>().set_app_status(
                                                SharedString::from("Guest access extended")
                                            );
                                        }
                                    }
                                });
                            }
                            Ok(None) => {
                                warn!("Tried to extend non-guest peer: {}", id_str);
                                update_status(&window_weak, "Cannot extend: not a guest peer");
                            }
                            Err(e) => {
                                error!("Failed to extend guest {}: {}", id_str, e);
                                update_status(&window_weak, &format!("Error extending guest: {}", e));
                            }
                        }
                    });
                });
            }
        });

        // Promote guest to trusted callback
        window.global::<AppLogic>().on_promote_guest({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Promoting guest to trusted peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Promote the guest
                        let mut store = peer_store.write().await;
                        match store.promote_guest(&id_str) {
                            Ok(true) => {
                                info!("Promoted guest {} to trusted peer", id_str);

                                // Get peer name for status message
                                let peer_name = store.find_by_id(&id_str)
                                    .map(|p| p.name.clone())
                                    .unwrap_or_else(|| "Peer".to_string());
                                drop(store);

                                // Update UI - clear guest fields
                                let _ = slint::invoke_from_event_loop({
                                    let window_weak = window_weak.clone();
                                    let id_str = id_str.clone();
                                    let peer_name = peer_name.clone();
                                    move || {
                                        if let Some(window) = window_weak.upgrade() {
                                            let model = window.global::<AppLogic>().get_peers();
                                            for i in 0..model.row_count() {
                                                if let Some(mut item) = model.row_data(i) {
                                                    if item.id.as_str() == id_str {
                                                        item.is_guest = false;
                                                        item.expires_at = SharedString::new();
                                                        item.time_remaining = SharedString::new();
                                                        item.extension_count = 0;
                                                        item.promotion_pending = false;
                                                        model.set_row_data(i, item);
                                                        break;
                                                    }
                                                }
                                            }
                                            window.global::<AppLogic>().set_app_status(
                                                SharedString::from(format!("{} is now a trusted peer", peer_name))
                                            );
                                        }
                                    }
                                });
                            }
                            Ok(false) => {
                                warn!("Tried to promote non-guest peer: {}", id_str);
                                update_status(&window_weak, "Cannot promote: not a guest peer");
                            }
                            Err(e) => {
                                error!("Failed to promote guest {}: {}", id_str, e);
                                update_status(&window_weak, &format!("Error promoting guest: {}", e));
                            }
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

            move |id, allow_push, allow_pull, allow_browse, allow_screen_view| {
                let id_str = id.to_string();
                info!("Updating permissions for peer {}: push={}, pull={}, browse={}, screen_view={}",
                      id_str, allow_push, allow_pull, allow_browse, allow_screen_view);
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
                                peer.permissions_granted.screen_view = allow_screen_view;
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
                                            chat: true,   // Always allow chat
                                            screen_view: allow_screen_view,
                                            screen_control: false, // TODO: Add UI control for screen control
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
                                    allow_screen_view,
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
            let session_stats = self.session_stats.clone();
            let config = config.clone();

            move |peer_id| {
                let peer_id_str = peer_id.to_string();
                info!("Push to peer requested: {}", peer_id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let selected_files = selected_files.clone();
                let transfer_manager = transfer_manager.clone();
                let transfer_history = transfer_history.clone();
                let shared_endpoint = shared_endpoint.clone();
                let session_stats = session_stats.clone();
                let config = config.clone();

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
                        let session_stats_progress = session_stats.clone();
                        let config_progress = config.clone();
                        let window_weak_progress = window_weak.clone();
                        let transfer_id_progress = transfer_id.clone();
                        tokio::spawn(async move {
                            while let Some(event) = progress_rx.recv().await {
                                let keep_completed = config_progress.read().await.keep_completed_transfers;
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
                                        // Get total size before updating status
                                        let total_bytes = transfer_manager_progress.get(&transfer_id_progress).await
                                            .map(|t| t.total_size).unwrap_or(0);
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Completed;
                                            t.progress = 100.0;
                                            t.completed_at = Some(chrono::Utc::now());
                                        }).await;
                                        // Update session stats (push is an upload)
                                        {
                                            let mut stats = session_stats_progress.write().await;
                                            stats.bytes_uploaded += total_bytes;
                                            stats.transfers_completed += 1;
                                        }
                                        update_session_stats_ui(&window_weak_progress, &session_stats_progress);
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, "Push completed successfully");
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                        // Refresh UI to clear the completed transfer
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                    }
                                    TransferEvent::Failed { error, .. } => {
                                        error!("Transfer failed: {}", error);
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Failed;
                                            t.error = Some(error.clone());
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, &format!("Push failed: {}", error));
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                    }
                                    TransferEvent::Cancelled { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Cancelled;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, "Push cancelled");
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
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
        let session_stats = self.session_stats.clone();

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
                                        window.global::<AppLogic>().set_browse_last_selected_index(-1);  // Reset shift-select anchor
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

        // Range selection (shift+click) - select all files between last-selected and current index
        window.global::<AppLogic>().on_browse_range_select({
            let window_weak = window_weak.clone();
            let browse_state = browse_state.clone();

            move |target_index| {
                let target_idx = target_index as usize;
                let window_weak = window_weak.clone();
                let browse_state = browse_state.clone();

                // Get the anchor index from the UI
                let anchor_idx = window_weak.upgrade().map(|w| {
                    w.global::<AppLogic>().get_browse_last_selected_index() as usize
                });

                info!("Range select: anchor={:?} target={}", anchor_idx, target_idx);

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Select all files in range
                        let (entries, selected_count) = {
                            let mut state = browse_state.write().await;

                            // Determine the range
                            let anchor = anchor_idx.unwrap_or(target_idx);
                            let start = anchor.min(target_idx);
                            let end = anchor.max(target_idx);

                            // Select all non-directory entries in the range
                            for i in start..=end {
                                if i < state.entries.len() && !state.entries[i].is_dir {
                                    state.entries[i].selected = true;
                                }
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
            let session_stats = session_stats.clone();

            move || {
                info!("Pull selected files");
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let browse_state = browse_state.clone();
                let config = config.clone();
                let transfer_manager = transfer_manager.clone();
                let transfer_history = transfer_history.clone();
                let session_stats = session_stats.clone();

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
                        let session_stats_progress = session_stats.clone();
                        let config_progress = config.clone();
                        let window_weak_progress = window_weak.clone();
                        let transfer_id_progress = transfer_id.clone();
                        let peer_name = peer.name.clone();
                        tokio::spawn(async move {
                            while let Some(event) = progress_rx.recv().await {
                                let keep_completed = config_progress.read().await.keep_completed_transfers;
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
                                        // Get total size before updating status
                                        let total_bytes = transfer_manager_progress.get(&transfer_id_progress).await
                                            .map(|t| t.total_size).unwrap_or(0);
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Completed;
                                            t.progress = 100.0;
                                            t.completed_at = Some(chrono::Utc::now());
                                        }).await;
                                        // Update session stats (pull is a download)
                                        {
                                            let mut stats = session_stats_progress.write().await;
                                            stats.bytes_downloaded += total_bytes;
                                            stats.transfers_completed += 1;
                                        }
                                        update_session_stats_ui(&window_weak_progress, &session_stats_progress);
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, &format!("Pull from {} completed", peer_name));
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                        // Refresh UI to clear the completed transfer
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                    }
                                    TransferEvent::Failed { error, .. } => {
                                        error!("Pull failed: {}", error);
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Failed;
                                            t.error = Some(error.clone());
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        update_status(&window_weak_progress, &format!("Pull failed: {}", error));
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
                                    }
                                    TransferEvent::Cancelled { .. } => {
                                        let _ = transfer_manager_progress.update(&transfer_id_progress, |t| {
                                            t.status = TransferStatus::Cancelled;
                                        }).await;
                                        update_transfers_ui(&window_weak_progress, &transfer_manager_progress).await;
                                        save_to_history(&transfer_history_progress, &transfer_manager_progress, &transfer_id_progress, keep_completed).await;
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

                        // Close dialog and reset selection state
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                window.global::<AppLogic>().set_browse_open(false);
                                window.global::<AppLogic>().set_browse_last_selected_index(-1);
                            }
                        });
                    });
                });
            }
        });
    }

    /// Set up network management callbacks.
    fn setup_network_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let network_store = self.network_store.clone();
        let peer_store = self.peer_store.clone();

        // Initialize networks UI
        {
            let window_weak = window_weak.clone();
            let network_store = network_store.clone();
            let peer_store = peer_store.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                rt.block_on(async {
                    update_networks_ui(&window_weak, &network_store, &peer_store).await;
                });
            });
        }

        // Create network callback
        window.global::<AppLogic>().on_create_network({
            let window_weak = window_weak.clone();
            let network_store = network_store.clone();
            let peer_store = peer_store.clone();

            move |name, description| {
                let name_str = name.to_string();
                let desc_str = description.to_string();
                info!("Creating network: {}", name_str);
                let window_weak = window_weak.clone();
                let network_store = network_store.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = network_store.write().await;
                        let result = if desc_str.is_empty() {
                            store.create(name_str.clone())
                        } else {
                            store.create_with_description(name_str.clone(), desc_str)
                        };

                        match result {
                            Ok(_) => {
                                info!("Network '{}' created successfully", name_str);
                                drop(store);
                                update_networks_ui(&window_weak, &network_store, &peer_store).await;
                                update_status(&window_weak, &format!("Network '{}' created", name_str));
                            }
                            Err(e) => {
                                error!("Failed to create network: {}", e);
                                update_status(&window_weak, &format!("Error creating network: {}", e));
                            }
                        }
                    });
                });
            }
        });

        // Delete network callback
        window.global::<AppLogic>().on_delete_network({
            let window_weak = window_weak.clone();
            let network_store = network_store.clone();
            let peer_store = peer_store.clone();

            move |id| {
                let id_str = id.to_string();
                info!("Deleting network: {}", id_str);
                let window_weak = window_weak.clone();
                let network_store = network_store.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = network_store.write().await;
                        match store.delete(&id_str) {
                            Ok(_) => {
                                info!("Network deleted successfully");
                                drop(store);
                                update_networks_ui(&window_weak, &network_store, &peer_store).await;
                                update_status(&window_weak, "Network deleted");
                            }
                            Err(e) => {
                                error!("Failed to delete network: {}", e);
                                update_status(&window_weak, &format!("Error deleting network: {}", e));
                            }
                        }
                    });
                });
            }
        });

        // Rename network callback
        window.global::<AppLogic>().on_rename_network({
            let window_weak = window_weak.clone();
            let network_store = network_store.clone();
            let peer_store = peer_store.clone();

            move |id, new_name| {
                let id_str = id.to_string();
                let name_str = new_name.to_string();
                info!("Renaming network {} to {}", id_str, name_str);
                let window_weak = window_weak.clone();
                let network_store = network_store.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = network_store.write().await;
                        match store.rename(&id_str, name_str.clone()) {
                            Ok(_) => {
                                info!("Network renamed successfully");
                                drop(store);
                                update_networks_ui(&window_weak, &network_store, &peer_store).await;
                                update_status(&window_weak, &format!("Network renamed to '{}'", name_str));
                            }
                            Err(e) => {
                                error!("Failed to rename network: {}", e);
                                update_status(&window_weak, &format!("Error renaming network: {}", e));
                            }
                        }
                    });
                });
            }
        });

        // Add peer to network callback
        window.global::<AppLogic>().on_add_peer_to_network({
            let window_weak = window_weak.clone();
            let network_store = network_store.clone();
            let peer_store = peer_store.clone();

            move |network_id, peer_id| {
                let network_id_str = network_id.to_string();
                let peer_id_str = peer_id.to_string();
                info!("Adding peer {} to network {}", peer_id_str, network_id_str);
                let window_weak = window_weak.clone();
                let network_store = network_store.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = network_store.write().await;
                        match store.add_member(&network_id_str, &peer_id_str) {
                            Ok(_) => {
                                info!("Peer added to network successfully");
                                drop(store);
                                update_networks_ui(&window_weak, &network_store, &peer_store).await;
                                update_status(&window_weak, "Peer added to network");
                            }
                            Err(e) => {
                                error!("Failed to add peer to network: {}", e);
                                update_status(&window_weak, &format!("Error: {}", e));
                            }
                        }
                    });
                });
            }
        });

        // Remove peer from network callback
        window.global::<AppLogic>().on_remove_peer_from_network({
            let window_weak = window_weak.clone();
            let network_store = network_store.clone();
            let peer_store = peer_store.clone();

            move |network_id, peer_id| {
                let network_id_str = network_id.to_string();
                let peer_id_str = peer_id.to_string();
                info!("Removing peer {} from network {}", peer_id_str, network_id_str);
                let window_weak = window_weak.clone();
                let network_store = network_store.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = network_store.write().await;
                        match store.remove_member(&network_id_str, &peer_id_str) {
                            Ok(true) => {
                                info!("Peer removed from network successfully");
                                drop(store);
                                update_networks_ui(&window_weak, &network_store, &peer_store).await;
                                update_status(&window_weak, "Peer removed from network");
                            }
                            Ok(false) => {
                                warn!("Peer was not in network");
                                update_status(&window_weak, "Peer was not in network");
                            }
                            Err(e) => {
                                error!("Failed to remove peer from network: {}", e);
                                update_status(&window_weak, &format!("Error: {}", e));
                            }
                        }
                    });
                });
            }
        });

        // Introduce peers callback - initiates three-way consent flow
        window.global::<AppLogic>().on_introduce_peers({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = self.shared_endpoint.clone();
            let identity = self.identity.clone();
            let pending_introductions = self.pending_introductions.clone();

            move |network_id, peer_to_introduce_id, target_member_id| {
                let network_id_str = network_id.to_string();
                let peer_b_id = peer_to_introduce_id.to_string();
                let peer_c_id = target_member_id.to_string();

                info!(
                    "Initiating introduction: peer {} to member {} in network {}",
                    peer_b_id, peer_c_id, network_id_str
                );

                // Update status immediately so user sees feedback
                update_status(&window_weak, "Starting peer introduction...");

                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let identity = identity.clone();
                let pending_introductions = pending_introductions.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Look up both peers
                        let peers = peer_store.read().await;
                        let peer_b = match peers.find_by_id(&peer_b_id) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Peer to introduce not found: {}", peer_b_id);
                                update_status(&window_weak, "Error: Peer not found");
                                return;
                            }
                        };
                        let peer_c = match peers.find_by_id(&peer_c_id) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Target member not found: {}", peer_c_id);
                                update_status(&window_weak, "Error: Target member not found");
                                return;
                            }
                        };
                        drop(peers);

                        // Check that peer B is not a guest (guests can't be introduced)
                        if peer_b.is_guest {
                            warn!("Cannot introduce guest peer: {}", peer_b.name);
                            update_status(&window_weak, "Cannot introduce guest peers");
                            return;
                        }

                        // Get endpoint and identity
                        let endpoint_guard = shared_endpoint.read().await;
                        let endpoint = match endpoint_guard.as_ref() {
                            Some(ep) => ep.clone(),
                            None => {
                                error!("No endpoint available");
                                update_status(&window_weak, "Error: Network not ready");
                                return;
                            }
                        };
                        drop(endpoint_guard);

                        let identity_guard = identity.read().await;
                        let our_identity = match identity_guard.as_ref() {
                            Some(id) => id.clone(),
                            None => {
                                error!("No identity available");
                                update_status(&window_weak, "Error: Identity not ready");
                                return;
                            }
                        };
                        drop(identity_guard);

                        // Generate introduction ID
                        let introduction_id = generate_id();

                        // Create pending introduction
                        let pending = PendingIntroduction {
                            introduction_id: introduction_id.clone(),
                            network_id: network_id_str.clone(),
                            peer_to_introduce_id: peer_b_id.clone(),
                            peer_to_introduce_name: peer_b.name.clone(),
                            target_member_id: peer_c_id.clone(),
                            target_member_name: peer_c.name.clone(),
                            peer_b_accepted: false,
                            peer_c_accepted: false,
                            started_at: std::time::Instant::now(),
                        };

                        // Store pending introduction
                        {
                            let mut intros = pending_introductions.write().await;
                            intros.insert(introduction_id.clone(), pending);
                        }

                        // Build PeerInfo for peer C (the network member) to send to peer B
                        let peer_c_info = PeerInfo {
                            endpoint_id: peer_c.endpoint_id.clone(),
                            name: peer_c.name.clone(),
                            version: env!("CARGO_PKG_VERSION").to_string(),
                            relay_url: peer_c.address.relay_url().map(|u| u.to_string()),
                        };

                        // Send IntroductionOffer to peer B
                        update_status(&window_weak, &format!("Sending introduction offer to {}...", peer_b.name));

                        // Connect to peer B
                        match endpoint.connect(&peer_b.endpoint_id).await {
                            Ok(mut conn) => {
                                // Send IntroductionOffer
                                let offer = ControlMessage::IntroductionOffer {
                                    introduction_id: introduction_id.clone(),
                                    network_id: network_id_str.clone(),
                                    peer: peer_c_info.clone(),
                                    message: Some(format!(
                                        "Would you like to connect with {}?",
                                        peer_c.name
                                    )),
                                };

                                if let Err(e) = conn.send(&offer).await {
                                    error!("Failed to send introduction offer: {}", e);
                                    update_status(&window_weak, &format!("Error: {}", e));
                                    // Remove pending introduction
                                    let mut intros = pending_introductions.write().await;
                                    intros.remove(&introduction_id);
                                    return;
                                }

                                info!("Introduction offer sent to {}", peer_b.name);
                                update_status(
                                    &window_weak,
                                    &format!("Introduction offer sent to {}, waiting for response...", peer_b.name),
                                );

                                // Wait for response (with timeout)
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(60),
                                    conn.recv(),
                                ).await {
                                    Ok(Ok(response)) => {
                                        match response {
                                            ControlMessage::IntroductionOfferResponse {
                                                introduction_id: resp_id,
                                                accepted,
                                                reason,
                                            } => {
                                                if resp_id != introduction_id {
                                                    warn!("Introduction ID mismatch");
                                                    return;
                                                }

                                                if accepted {
                                                    info!("{} accepted introduction offer", peer_b.name);

                                                    // Update pending state
                                                    {
                                                        let mut intros = pending_introductions.write().await;
                                                        if let Some(intro) = intros.get_mut(&introduction_id) {
                                                            intro.peer_b_accepted = true;
                                                        }
                                                    }

                                                    // Now send IntroductionRequest to peer C
                                                    update_status(
                                                        &window_weak,
                                                        &format!("Sending introduction request to {}...", peer_c.name),
                                                    );

                                                    // Build PeerInfo for peer B to send to peer C
                                                    let peer_b_info = PeerInfo {
                                                        endpoint_id: peer_b.endpoint_id.clone(),
                                                        name: peer_b.name.clone(),
                                                        version: env!("CARGO_PKG_VERSION").to_string(),
                                                        relay_url: peer_b.address.relay_url().map(|u| u.to_string()),
                                                    };

                                                    // Connect to peer C
                                                    match endpoint.connect(&peer_c.endpoint_id).await {
                                                        Ok(mut conn_c) => {
                                                            let request = ControlMessage::IntroductionRequest {
                                                                introduction_id: introduction_id.clone(),
                                                                network_id: network_id_str.clone(),
                                                                peer: peer_b_info.clone(),
                                                                message: Some(format!(
                                                                    "{} would like to connect with you",
                                                                    peer_b.name
                                                                )),
                                                            };

                                                            if let Err(e) = conn_c.send(&request).await {
                                                                error!("Failed to send introduction request: {}", e);
                                                                update_status(&window_weak, &format!("Error: {}", e));
                                                                return;
                                                            }

                                                            info!("Introduction request sent to {}", peer_c.name);

                                                            // Wait for response from peer C
                                                            match tokio::time::timeout(
                                                                std::time::Duration::from_secs(60),
                                                                conn_c.recv(),
                                                            ).await {
                                                                Ok(Ok(response_c)) => {
                                                                    match response_c {
                                                                        ControlMessage::IntroductionRequestResponse {
                                                                            introduction_id: resp_id_c,
                                                                            accepted: accepted_c,
                                                                            reason: reason_c,
                                                                        } => {
                                                                            if resp_id_c != introduction_id {
                                                                                warn!("Introduction ID mismatch from peer C");
                                                                                return;
                                                                            }

                                                                            if accepted_c {
                                                                                info!("{} accepted introduction", peer_c.name);

                                                                                // Both accepted! Send IntroductionComplete to both
                                                                                // Serialize peer info as the "trust bundle" - the peers
                                                                                // will establish trust directly when they connect
                                                                                let complete_to_b = ControlMessage::IntroductionComplete {
                                                                                    introduction_id: introduction_id.clone(),
                                                                                    network_id: network_id_str.clone(),
                                                                                    peer: peer_c_info.clone(),
                                                                                    trust_bundle_json: serde_json::to_string(&peer_c_info).unwrap_or_default(),
                                                                                };

                                                                                // Small delay to allow receivers to be ready for new connections
                                                                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                                                                                // Send IntroductionComplete to peer B (reconnect)
                                                                                match endpoint.connect(&peer_b.endpoint_id).await {
                                                                                    Ok(mut conn_b_new) => {
                                                                                        if let Err(e) = conn_b_new.send(&complete_to_b).await {
                                                                                            error!("Failed to send IntroductionComplete to {}: {}", peer_b.name, e);
                                                                                        } else {
                                                                                            info!("Sent IntroductionComplete to {}", peer_b.name);
                                                                                        }
                                                                                        // Properly close the connection
                                                                                        let _ = conn_b_new.close().await;
                                                                                    }
                                                                                    Err(e) => {
                                                                                        error!("Failed to reconnect to {} for IntroductionComplete: {}", peer_b.name, e);
                                                                                    }
                                                                                }

                                                                                let complete_to_c = ControlMessage::IntroductionComplete {
                                                                                    introduction_id: introduction_id.clone(),
                                                                                    network_id: network_id_str.clone(),
                                                                                    peer: peer_b_info.clone(),
                                                                                    trust_bundle_json: serde_json::to_string(&peer_b_info).unwrap_or_default(),
                                                                                };

                                                                                // Close old connection and reconnect to peer C
                                                                                let _ = conn_c.close().await;
                                                                                match endpoint.connect(&peer_c.endpoint_id).await {
                                                                                    Ok(mut conn_c_new) => {
                                                                                        if let Err(e) = conn_c_new.send(&complete_to_c).await {
                                                                                            error!("Failed to send IntroductionComplete to {}: {}", peer_c.name, e);
                                                                                        } else {
                                                                                            info!("Sent IntroductionComplete to {}", peer_c.name);
                                                                                        }
                                                                                        // Properly close the connection
                                                                                        let _ = conn_c_new.close().await;
                                                                                    }
                                                                                    Err(e) => {
                                                                                        error!("Failed to reconnect to {} for IntroductionComplete: {}", peer_c.name, e);
                                                                                    }
                                                                                }

                                                                                // Clean up pending introduction
                                                                                {
                                                                                    let mut intros = pending_introductions.write().await;
                                                                                    intros.remove(&introduction_id);
                                                                                }

                                                                                info!(
                                                                                    "Introduction complete! {} and {} can now connect",
                                                                                    peer_b.name, peer_c.name
                                                                                );
                                                                                update_status(
                                                                                    &window_weak,
                                                                                    &format!(
                                                                                        "Introduction complete! {} and {} can now connect directly",
                                                                                        peer_b.name, peer_c.name
                                                                                    ),
                                                                                );
                                                                            } else {
                                                                                let reason_str = reason_c.unwrap_or_else(|| "No reason given".to_string());
                                                                                info!("{} declined introduction: {}", peer_c.name, reason_str);
                                                                                update_status(
                                                                                    &window_weak,
                                                                                    &format!("{} declined introduction: {}", peer_c.name, reason_str),
                                                                                );

                                                                                // Notify peer B that introduction failed
                                                                                let failed = ControlMessage::IntroductionFailed {
                                                                                    introduction_id: introduction_id.clone(),
                                                                                    reason: format!("{} declined", peer_c.name),
                                                                                };
                                                                                if let Ok(mut conn_b_new) = endpoint.connect(&peer_b.endpoint_id).await {
                                                                                    let _ = conn_b_new.send(&failed).await;
                                                                                }
                                                                            }
                                                                        }
                                                                        _ => {
                                                                            warn!("Unexpected response from peer C");
                                                                        }
                                                                    }
                                                                }
                                                                Ok(Err(e)) => {
                                                                    error!("Error receiving from peer C: {}", e);
                                                                    update_status(&window_weak, &format!("Error: {}", e));
                                                                }
                                                                Err(_) => {
                                                                    warn!("{} did not respond in time", peer_c.name);
                                                                    update_status(
                                                                        &window_weak,
                                                                        &format!("{} did not respond in time", peer_c.name),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                        Err(e) => {
                                                            error!("Failed to connect to peer C: {}", e);
                                                            update_status(&window_weak, &format!("Error connecting to {}: {}", peer_c.name, e));
                                                        }
                                                    }
                                                } else {
                                                    let reason_str = reason.unwrap_or_else(|| "No reason given".to_string());
                                                    info!("{} declined introduction: {}", peer_b.name, reason_str);
                                                    update_status(
                                                        &window_weak,
                                                        &format!("{} declined introduction: {}", peer_b.name, reason_str),
                                                    );
                                                }
                                            }
                                            _ => {
                                                warn!("Unexpected response from peer B");
                                            }
                                        }
                                    }
                                    Ok(Err(e)) => {
                                        error!("Error receiving from peer B: {}", e);
                                        update_status(&window_weak, &format!("Error: {}", e));
                                    }
                                    Err(_) => {
                                        warn!("{} did not respond in time", peer_b.name);
                                        update_status(
                                            &window_weak,
                                            &format!("{} did not respond in time", peer_b.name),
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to connect to peer B: {}", e);
                                update_status(&window_weak, &format!("Error connecting to {}: {}", peer_b.name, e));
                            }
                        }

                        // Clean up pending introduction on any failure path
                        {
                            let mut intros = pending_introductions.write().await;
                            intros.remove(&introduction_id);
                        }
                    });
                });
            }
        });

        // Update network settings callback
        window.global::<AppLogic>().on_update_network_settings({
            let window_weak = window_weak.clone();
            let network_store = network_store.clone();
            let peer_store = peer_store.clone();

            move |network_id, allow_introductions, auto_accept, share_member_list| {
                let network_id_str = network_id.to_string();
                info!(
                    "Updating network {} settings: allow_intro={}, auto_accept={}, share_members={}",
                    network_id_str, allow_introductions, auto_accept, share_member_list
                );

                let window_weak = window_weak.clone();
                let network_store = network_store.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = network_store.write().await;
                        let settings = croh_core::networks::NetworkSettings {
                            allow_introductions,
                            auto_accept_introductions: auto_accept,
                            share_member_list,
                            max_members: 0, // Keep unlimited
                        };

                        match store.update_settings(&network_id_str, settings) {
                            Ok(_) => {
                                info!("Network settings updated successfully");
                                drop(store);
                                update_networks_ui(&window_weak, &network_store, &peer_store).await;
                            }
                            Err(e) => {
                                error!("Failed to update network settings: {}", e);
                                update_status(&window_weak, &format!("Error: {}", e));
                            }
                        }
                    });
                });
            }
        });
    }

    /// Set up chat callbacks for peer messaging.
    fn setup_chat_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let chat_store = self.chat_store.clone();
        let active_chat_peer = self.active_chat_peer.clone();
        let peer_store = self.peer_store.clone();
        let shared_endpoint = self.shared_endpoint.clone();
        let identity = self.identity.clone();
        let peer_status = self.peer_status.clone();

        // Open chat callback - opens chat panel for a specific peer
        window.global::<AppLogic>().on_open_chat({
            let window_weak = window_weak.clone();
            let chat_store = chat_store.clone();
            let active_chat_peer = active_chat_peer.clone();
            let peer_store = peer_store.clone();
            let peer_status = peer_status.clone();
            let identity = identity.clone();

            move |peer_id| {
                let peer_id_str = peer_id.to_string();
                info!("Opening chat with peer: {}", peer_id_str);

                let window_weak = window_weak.clone();
                let chat_store = chat_store.clone();
                let active_chat_peer = active_chat_peer.clone();
                let peer_store = peer_store.clone();
                let peer_status = peer_status.clone();
                let identity = identity.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get peer info (peer_id_str is the peer's internal id from Slint)
                        let peers = peer_store.read().await;
                        let peer = match peers.find_by_id(&peer_id_str) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Peer not found: {}", peer_id_str);
                                return;
                            }
                        };
                        drop(peers);

                        // Use the full endpoint_id for chat operations (not the truncated display version)
                        let peer_endpoint_id = peer.endpoint_id.clone();

                        // Set active chat peer (using endpoint_id for message matching)
                        {
                            let mut active = active_chat_peer.write().await;
                            *active = Some(peer_endpoint_id.clone());
                        }

                        // Check if peer is online
                        let status = peer_status.read().await;
                        let is_online = status.get(&peer_id_str).map(|s| s.online).unwrap_or(false);
                        drop(status);

                        // Get our endpoint ID
                        let our_id = {
                            let id_guard = identity.read().await;
                            id_guard.as_ref().map(|i| i.endpoint_id.clone()).unwrap_or_default()
                        };

                        // Load messages from store (keyed by endpoint_id)
                        let mut messages = chat_store.get_messages(&peer_endpoint_id, 50, None)
                            .unwrap_or_default();

                        // Reverse to get chronological order (oldest first for display)
                        messages.reverse();

                        // Mark messages as read
                        let handler = ChatHandler::new(chat_store.clone(), our_id.clone());
                        if let Err(e) = handler.mark_conversation_read(&peer_endpoint_id) {
                            warn!("Failed to mark messages as read: {}", e);
                        }

                        // Convert to UI format
                        let ui_messages: Vec<ChatMessageItem> = messages
                            .iter()
                            .map(|m| {
                                let is_mine = m.sender_id == our_id;
                                ChatMessageItem {
                                    id: SharedString::from(m.id.as_str()),
                                    content: SharedString::from(&m.content),
                                    is_mine,
                                    timestamp: SharedString::from(format_chat_time(m.sent_at)),
                                    status: SharedString::from(m.status.as_str()),
                                    show_date_divider: false,
                                    date_label: SharedString::default(),
                                }
                            })
                            .collect();

                        // Check if there are more messages
                        let has_more = messages.len() >= 50;

                        // Update UI
                        let window_weak = window_weak.clone();
                        let peer_name = peer.name.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let logic = window.global::<AppLogic>();
                                logic.set_chat_panel_open(true);
                                logic.set_chat_peer_id(SharedString::from(&peer_endpoint_id));
                                logic.set_chat_peer_name(SharedString::from(&peer_name));
                                logic.set_chat_peer_online(is_online);
                                logic.set_chat_peer_typing(false);
                                logic.set_chat_messages(ModelRc::new(VecModel::from(ui_messages)));
                                logic.set_chat_loading(false);
                                logic.set_chat_has_more(has_more);
                            }
                        });
                    });
                });
            }
        });

        // Close chat callback
        window.global::<AppLogic>().on_close_chat({
            let window_weak = window_weak.clone();
            let active_chat_peer = active_chat_peer.clone();

            move || {
                info!("Closing chat panel");

                let window_weak = window_weak.clone();
                let active_chat_peer = active_chat_peer.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Clear active chat peer
                        {
                            let mut active = active_chat_peer.write().await;
                            *active = None;
                        }

                        // Update UI
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let logic = window.global::<AppLogic>();
                                logic.set_chat_panel_open(false);
                                logic.set_chat_peer_id(SharedString::default());
                                logic.set_chat_peer_name(SharedString::default());
                                logic.set_chat_messages(ModelRc::new(VecModel::default()));
                            }
                        });
                    });
                });
            }
        });

        // Send chat message callback
        window.global::<AppLogic>().on_send_chat_message({
            let window_weak = window_weak.clone();
            let chat_store = chat_store.clone();
            let active_chat_peer = active_chat_peer.clone();
            let shared_endpoint = shared_endpoint.clone();
            let identity = identity.clone();
            let peer_store = peer_store.clone();

            move |content| {
                let content_str = content.to_string().trim().to_string();
                if content_str.is_empty() {
                    return;
                }

                let window_weak = window_weak.clone();
                let chat_store = chat_store.clone();
                let active_chat_peer = active_chat_peer.clone();
                let shared_endpoint = shared_endpoint.clone();
                let identity = identity.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get active peer
                        let peer_id = {
                            let active = active_chat_peer.read().await;
                            match active.as_ref() {
                                Some(id) => id.clone(),
                                None => {
                                    warn!("No active chat peer");
                                    return;
                                }
                            }
                        };

                        // Get peer info
                        let peer_name = {
                            let peers = peer_store.read().await;
                            match peers.find_by_id(&peer_id) {
                                Some(p) => p.name.clone(),
                                None => "Unknown".to_string(),
                            }
                        };

                        // Get our endpoint ID
                        let our_id = {
                            let id_guard = identity.read().await;
                            match id_guard.as_ref() {
                                Some(i) => i.endpoint_id.clone(),
                                None => {
                                    error!("No identity available");
                                    return;
                                }
                            }
                        };

                        // Try to get a connection to the peer
                        let mut conn_opt = None;
                        {
                            let ep_guard = shared_endpoint.read().await;
                            if let Some(ref endpoint) = *ep_guard {
                                if let Ok(conn) = endpoint.connect(&peer_id).await {
                                    conn_opt = Some(conn);
                                }
                            }
                        }

                        // Send message via handler
                        let handler = ChatHandler::new(chat_store.clone(), our_id.clone());
                        let result = handler.send_message(
                            conn_opt.as_mut(),
                            &peer_id,
                            &peer_name,
                            content_str.clone(),
                        ).await;

                        // Close connection if we opened one
                        if let Some(mut conn) = conn_opt {
                            let _ = conn.close().await;
                        }

                        match result {
                            Ok((message, _events)) => {
                                // Add message to UI
                                let status = message.status.as_str();
                                let ui_message = ChatMessageItem {
                                    id: SharedString::from(message.id.as_str()),
                                    content: SharedString::from(&message.content),
                                    is_mine: true,
                                    timestamp: SharedString::from(format_chat_time(message.sent_at)),
                                    status: SharedString::from(status),
                                    show_date_divider: false,
                                    date_label: SharedString::default(),
                                };

                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(window) = window_weak.upgrade() {
                                        let logic = window.global::<AppLogic>();
                                        let current = logic.get_chat_messages();
                                        let mut messages: Vec<ChatMessageItem> = (0..current.row_count())
                                            .filter_map(|i| current.row_data(i))
                                            .collect();
                                        messages.push(ui_message);
                                        logic.set_chat_messages(ModelRc::new(VecModel::from(messages)));
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Failed to send message: {}", e);
                            }
                        }
                    });
                });
            }
        });

        // Chat input changed callback (for typing indicator)
        window.global::<AppLogic>().on_chat_input_changed({
            let active_chat_peer = active_chat_peer.clone();
            let shared_endpoint = shared_endpoint.clone();
            let typing_debounce = self.typing_debounce.clone();

            move |content| {
                let content_str = content.to_string();
                let is_typing = !content_str.trim().is_empty();

                // Debounce typing indicator - only send once per second
                let active_chat_peer = active_chat_peer.clone();
                let shared_endpoint = shared_endpoint.clone();
                let typing_debounce = typing_debounce.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Check debounce (but always send stop-typing immediately)
                        if is_typing {
                            let mut debounce = typing_debounce.write().await;
                            let now = std::time::Instant::now();
                            if let Some(last) = *debounce {
                                if now.duration_since(last) < std::time::Duration::from_secs(1) {
                                    return;
                                }
                            }
                            *debounce = Some(now);
                        }

                        // Get active peer
                        let peer_id = {
                            let active = active_chat_peer.read().await;
                            match active.as_ref() {
                                Some(id) => id.clone(),
                                None => return,
                            }
                        };

                        // Send typing indicator
                        let ep_guard = shared_endpoint.read().await;
                        if let Some(ref endpoint) = *ep_guard {
                            if let Ok(mut conn) = endpoint.connect(&peer_id).await {
                                let msg = ControlMessage::ChatTyping { is_typing };
                                let _ = conn.send(&msg).await;
                                let _ = conn.close().await;
                            }
                        }
                    });
                });
            }
        });
    }

    /// Set up screen viewer callbacks.
    fn setup_screen_viewer_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let peer_store = self.peer_store.clone();
        let shared_endpoint = self.shared_endpoint.clone();
        let screen_viewer_cmd = self.screen_viewer_cmd.clone();
        let active_screen_peer = self.active_screen_peer.clone();
        let peer_status = self.peer_status.clone();

        // Open screen viewer callback - opens viewer for a specific peer
        window.global::<AppLogic>().on_open_screen_viewer({
            let window_weak = window_weak.clone();
            let peer_store = peer_store.clone();
            let shared_endpoint = shared_endpoint.clone();
            let screen_viewer_cmd = screen_viewer_cmd.clone();
            let active_screen_peer = active_screen_peer.clone();
            let peer_status = peer_status.clone();

            move |peer_id| {
                let peer_id_str = peer_id.to_string();
                info!("Opening screen viewer for peer: {}", peer_id_str);

                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();
                let shared_endpoint = shared_endpoint.clone();
                let screen_viewer_cmd = screen_viewer_cmd.clone();
                let active_screen_peer = active_screen_peer.clone();
                let peer_status = peer_status.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Get peer info (we receive peer.id from UI)
                        let peers = peer_store.read().await;
                        let peer = match peers.find_by_id(&peer_id_str) {
                            Some(p) => p.clone(),
                            None => {
                                error!("Peer not found by id: {}", peer_id_str);
                                return;
                            }
                        };
                        drop(peers);

                        // Check if peer has granted us screen_view permission
                        if !peer.their_permissions.screen_view {
                            warn!("Peer {} does not have screen_view permission", peer.name);
                            let window_weak = window_weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let logic = window.global::<AppLogic>();
                                    logic.set_screen_viewer_error(SharedString::from(
                                        "Peer has not granted screen viewing permission"
                                    ));
                                }
                            });
                            return;
                        }

                        // Check if peer is online (peer_status is keyed by peer.id)
                        let status = peer_status.read().await;
                        let is_online = status.get(&peer.id).map(|s| s.online).unwrap_or(false);
                        drop(status);

                        if !is_online {
                            warn!("Peer {} is not online", peer.name);
                            let window_weak = window_weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(window) = window_weak.upgrade() {
                                    let logic = window.global::<AppLogic>();
                                    logic.set_screen_viewer_error(SharedString::from(
                                        "Peer is not online"
                                    ));
                                }
                            });
                            return;
                        }

                        // Set active screen peer
                        {
                            let mut active = active_screen_peer.write().await;
                            *active = Some(peer.endpoint_id.clone());
                        }

                        // Create viewer event channel
                        let (event_tx, mut event_rx) = viewer_event_channel(256);

                        // Create viewer command channel
                        let (cmd_tx, cmd_rx) = viewer_command_channel(64);

                        // Store command sender for UI callbacks
                        {
                            let mut cmd_guard = screen_viewer_cmd.write().await;
                            *cmd_guard = Some(cmd_tx.clone());
                        }

                        // Update UI to show viewer is opening
                        let peer_name = peer.name.clone();
                        let peer_endpoint_id = peer.endpoint_id.clone();
                        let window_weak_ui = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_ui.upgrade() {
                                let logic = window.global::<AppLogic>();
                                logic.set_screen_viewer_open(true);
                                logic.set_screen_viewer_peer_id(SharedString::from(&peer_endpoint_id));
                                logic.set_screen_viewer_peer_name(SharedString::from(&peer_name));
                                logic.set_screen_viewer_status(SharedString::from("connecting"));
                                logic.set_screen_viewer_error(SharedString::default());
                            }
                        });

                        // Create the viewer
                        let config = ViewerConfig::default();
                        let mut viewer = ScreenViewer::new(config, event_tx);

                        // Start connection
                        viewer.start_connect(peer.endpoint_id.clone(), None);

                        // Spawn event handler task
                        let window_weak_events = window_weak.clone();
                        let event_handler = tokio::spawn(async move {
                            while let Some(event) = event_rx.recv().await {
                                let window_weak = window_weak_events.clone();
                                match event {
                                    ViewerEvent::StateChanged { new_state, .. } => {
                                        let status_str = new_state.to_string();
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(window) = window_weak.upgrade() {
                                                let logic = window.global::<AppLogic>();
                                                logic.set_screen_viewer_status(SharedString::from(&status_str));
                                            }
                                        });
                                    }
                                    ViewerEvent::FrameReady { width, height, .. } => {
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(window) = window_weak.upgrade() {
                                                let logic = window.global::<AppLogic>();
                                                logic.set_screen_viewer_width(width as i32);
                                                logic.set_screen_viewer_height(height as i32);
                                            }
                                        });
                                    }
                                    ViewerEvent::StatsUpdated(stats) => {
                                        let fps = stats.current_fps;
                                        let bitrate = stats.avg_bitrate_kbps;
                                        let latency = stats.latency_ms;
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(window) = window_weak.upgrade() {
                                                let logic = window.global::<AppLogic>();
                                                logic.set_screen_viewer_fps(fps);
                                                logic.set_screen_viewer_bitrate_kbps(bitrate as i32);
                                                logic.set_screen_viewer_latency_ms(latency as i32);
                                            }
                                        });
                                    }
                                    ViewerEvent::StreamRejected { reason, .. } => {
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(window) = window_weak.upgrade() {
                                                let logic = window.global::<AppLogic>();
                                                logic.set_screen_viewer_status(SharedString::from("error"));
                                                logic.set_screen_viewer_error(SharedString::from(&reason));
                                            }
                                        });
                                    }
                                    ViewerEvent::StreamEnded { reason } => {
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(window) = window_weak.upgrade() {
                                                let logic = window.global::<AppLogic>();
                                                logic.set_screen_viewer_status(SharedString::from("disconnected"));
                                                if !reason.is_empty() && reason != "user request" {
                                                    logic.set_screen_viewer_error(SharedString::from(&reason));
                                                }
                                            }
                                        });
                                    }
                                    ViewerEvent::Error(msg) => {
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(window) = window_weak.upgrade() {
                                                let logic = window.global::<AppLogic>();
                                                logic.set_screen_viewer_error(SharedString::from(&msg));
                                            }
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        });

                        // Get the endpoint for streaming
                        let endpoint = {
                            let ep_guard = shared_endpoint.read().await;
                            match &*ep_guard {
                                Some(ep) => ep.clone(),
                                None => {
                                    error!("Shared endpoint not ready for screen streaming");
                                    viewer.on_error("Endpoint not ready".to_string());
                                    return;
                                }
                            }
                        };

                        // Create cancellation channel
                        let (cancel_tx, cancel_rx) = tokio::sync::mpsc::channel::<()>(1);

                        // Create stream event channel
                        let (stream_event_tx, mut stream_event_rx) = tokio::sync::mpsc::channel::<ScreenStreamEvent>(256);

                        // Clone for the streaming task
                        let peer_clone = peer.clone();

                        // Spawn the streaming task
                        let stream_task = tokio::spawn(async move {
                            match stream_screen_from_peer(&endpoint, &peer_clone, None, stream_event_tx, cancel_rx).await {
                                Ok(stream_id) => {
                                    info!("Screen stream {} ended normally", stream_id);
                                }
                                Err(e) => {
                                    error!("Screen stream error: {}", e);
                                }
                            }
                        });

                        // Process stream events and update viewer
                        let window_weak_stream = window_weak.clone();
                        loop {
                            match stream_event_rx.recv().await {
                                Some(stream_event) => match stream_event {
                                    ScreenStreamEvent::Accepted { stream_id, displays, .. } => {
                                        info!("Stream {} accepted, {} displays available", stream_id, displays.len());
                                        let display_infos: Vec<_> = displays.iter().map(|d| croh_core::iroh::protocol::DisplayInfo {
                                            id: d.id.clone(),
                                            name: d.name.clone(),
                                            width: d.width,
                                            height: d.height,
                                            refresh_rate: d.refresh_rate,
                                            is_primary: d.is_primary,
                                        }).collect();
                                        viewer.on_connected(display_infos);
                                    }
                                    ScreenStreamEvent::Rejected { reason, .. } => {
                                        warn!("Stream rejected: {}", reason);
                                        viewer.on_stream_rejected(reason);
                                        break;
                                    }
                                    ScreenStreamEvent::FrameReceived { metadata, data, .. } => {
                                        info!(
                                            "GUI received frame seq={}: {}x{}, {} bytes",
                                            metadata.sequence, metadata.width, metadata.height, data.len()
                                        );
                                        // Record latency from capture timestamp
                                        viewer.record_latency(metadata.captured_at);

                                        // Pass frame to viewer for decoding and display
                                        if let Err(e) = viewer.on_frame_received(&data, metadata.width, metadata.height, metadata.sequence) {
                                            warn!("Frame processing error: {}", e);
                                            continue;
                                        }

                                        // Get the decoded frame and push to UI
                                        if let Some(frame) = viewer.latest_frame() {
                                            let width = frame.width;
                                            let height = frame.height;
                                            let rgba_data = frame.data.clone();
                                            info!(
                                                "Pushing frame to UI: {}x{}, {} bytes RGBA",
                                                width, height, rgba_data.len()
                                            );

                                            // Update UI with the frame image
                                            let window_weak_frame = window_weak_stream.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(window) = window_weak_frame.upgrade() {
                                                    // Create a Slint image from RGBA data
                                                    let pixel_buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                                        &rgba_data,
                                                        width,
                                                        height,
                                                    );
                                                    let image = slint::Image::from_rgba8(pixel_buffer);

                                                    let logic = window.global::<AppLogic>();
                                                    logic.set_screen_viewer_frame(image);
                                                    logic.set_screen_viewer_width(width as i32);
                                                    logic.set_screen_viewer_height(height as i32);
                                                }
                                            });
                                        } else {
                                            warn!("No frame available from viewer after decode");
                                        }

                                        // Emit stats periodically
                                        if metadata.sequence % 30 == 0 {
                                            viewer.emit_stats();
                                        }
                                    }
                                    ScreenStreamEvent::Ended { reason, .. } => {
                                        info!("Stream ended: {}", reason);
                                        viewer.disconnect(reason);
                                        break;
                                    }
                                    ScreenStreamEvent::Error(msg) => {
                                        error!("Stream error: {}", msg);
                                        viewer.on_error(msg);
                                        break;
                                    }
                                },
                                None => {
                                    // Channel closed, stream ended
                                    info!("Stream event channel closed");
                                    break;
                                }
                            }
                        }

                        // Cancel event handler
                        event_handler.abort();

                        // Wait for stream task to finish
                        let _ = stream_task.await;
                    });
                });
            }
        });

        // Close screen viewer callback
        window.global::<AppLogic>().on_close_screen_viewer({
            let window_weak = window_weak.clone();
            let screen_viewer_cmd = screen_viewer_cmd.clone();
            let active_screen_peer = active_screen_peer.clone();

            move || {
                info!("Closing screen viewer");

                let window_weak = window_weak.clone();
                let screen_viewer_cmd = screen_viewer_cmd.clone();
                let active_screen_peer = active_screen_peer.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        // Send disconnect command if viewer is active
                        {
                            let cmd_guard = screen_viewer_cmd.read().await;
                            if let Some(ref cmd_tx) = *cmd_guard {
                                let _ = cmd_tx.send(ViewerCommand::Disconnect).await;
                            }
                        }

                        // Clear command sender
                        {
                            let mut cmd_guard = screen_viewer_cmd.write().await;
                            *cmd_guard = None;
                        }

                        // Clear active peer
                        {
                            let mut active = active_screen_peer.write().await;
                            *active = None;
                        }

                        // Update UI
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak.upgrade() {
                                let logic = window.global::<AppLogic>();
                                logic.set_screen_viewer_open(false);
                                logic.set_screen_viewer_peer_id(SharedString::default());
                                logic.set_screen_viewer_peer_name(SharedString::default());
                                logic.set_screen_viewer_status(SharedString::from("disconnected"));
                                logic.set_screen_viewer_error(SharedString::default());
                            }
                        });
                    });
                });
            }
        });

        // Disconnect callback (different from close - stays open showing error)
        window.global::<AppLogic>().on_screen_viewer_disconnect({
            let screen_viewer_cmd = screen_viewer_cmd.clone();

            move || {
                info!("Disconnecting screen viewer");

                let screen_viewer_cmd = screen_viewer_cmd.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let cmd_guard = screen_viewer_cmd.read().await;
                        if let Some(ref cmd_tx) = *cmd_guard {
                            let _ = cmd_tx.send(ViewerCommand::Disconnect).await;
                        }
                    });
                });
            }
        });

        // Toggle fullscreen callback
        window.global::<AppLogic>().on_screen_viewer_toggle_fullscreen({
            let window_weak = window_weak.clone();

            move || {
                debug!("Toggling screen viewer fullscreen");

                let window_weak = window_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = window_weak.upgrade() {
                        let logic = window.global::<AppLogic>();
                        let is_fullscreen = logic.get_screen_viewer_fullscreen();
                        logic.set_screen_viewer_fullscreen(!is_fullscreen);
                    }
                });
            }
        });

        // Adjust quality callback
        window.global::<AppLogic>().on_screen_viewer_adjust_quality({
            let screen_viewer_cmd = screen_viewer_cmd.clone();

            move |quality_str| {
                let quality_str = quality_str.to_string();
                debug!("Adjusting screen viewer quality to: {}", quality_str);

                let screen_viewer_cmd = screen_viewer_cmd.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        use croh_core::iroh::protocol::ScreenQuality;

                        let quality = match quality_str.as_str() {
                            "fast" => ScreenQuality::Fast,
                            "balanced" => ScreenQuality::Balanced,
                            "quality" => ScreenQuality::Quality,
                            "auto" => ScreenQuality::Auto,
                            _ => ScreenQuality::Balanced,
                        };

                        let cmd_guard = screen_viewer_cmd.read().await;
                        if let Some(ref cmd_tx) = *cmd_guard {
                            let _ = cmd_tx.send(ViewerCommand::AdjustQuality {
                                quality,
                                fps: None,
                            }).await;
                        }
                    });
                });
            }
        });

        // Send input callback (mouse/keyboard events)
        window.global::<AppLogic>().on_screen_viewer_send_input({
            let screen_viewer_cmd = screen_viewer_cmd.clone();

            move |event_type, x, y, data| {
                // event_type: 0=mouse_move, 1=mouse_down, 2=mouse_up, 3=scroll, 4=key_down, 5=key_up
                let screen_viewer_cmd = screen_viewer_cmd.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let event = match event_type {
                            0 => RemoteInputEvent::MouseMove {
                                x: x as i32,
                                y: y as i32,
                                absolute: true,
                            },
                            1 => {
                                // Mouse button down
                                let button = match data {
                                    1 => croh_core::screen::MouseButton::Left,
                                    2 => croh_core::screen::MouseButton::Right,
                                    3 => croh_core::screen::MouseButton::Middle,
                                    _ => croh_core::screen::MouseButton::Left,
                                };
                                RemoteInputEvent::MouseButton { button, pressed: true }
                            }
                            2 => {
                                // Mouse button up
                                let button = match data {
                                    1 => croh_core::screen::MouseButton::Left,
                                    2 => croh_core::screen::MouseButton::Right,
                                    3 => croh_core::screen::MouseButton::Middle,
                                    _ => croh_core::screen::MouseButton::Left,
                                };
                                RemoteInputEvent::MouseButton { button, pressed: false }
                            }
                            3 => RemoteInputEvent::MouseScroll {
                                dx: x as i32,
                                dy: y as i32,
                            },
                            // Key events would need additional handling
                            _ => return,
                        };

                        let cmd_guard = screen_viewer_cmd.read().await;
                        if let Some(ref cmd_tx) = *cmd_guard {
                            let _ = cmd_tx.send(ViewerCommand::SendInput(event)).await;
                        }
                    });
                });
            }
        });
    }
}

/// Format a timestamp for chat display (e.g., "2:34 PM" or "Yesterday 2:34 PM").
fn format_chat_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let local = dt.with_timezone(&chrono::Local);
    let now = chrono::Local::now();
    let today = now.date_naive();
    let msg_date = local.date_naive();

    if msg_date == today {
        local.format("%l:%M %p").to_string().trim().to_string()
    } else if msg_date == today - chrono::Duration::days(1) {
        format!("Yesterday {}", local.format("%l:%M %p").to_string().trim())
    } else {
        local.format("%b %d, %l:%M %p").to_string()
    }
}

/// Intermediate data structure for network info that is Send-safe.
/// Used to transfer data to the UI thread where ModelRc can be created.
struct NetworkData {
    id: String,
    name: String,
    description: String,
    member_count: i32,
    is_owner: bool,
    allow_introductions: bool,
    auto_accept_introductions: bool,
    share_member_list: bool,
    created_at: String,
    members: Vec<(String, String)>, // (peer_id, name)
}

/// Update the networks list in the UI.
async fn update_networks_ui(
    window_weak: &Weak<MainWindow>,
    network_store: &Arc<RwLock<NetworkStore>>,
    peer_store: &Arc<RwLock<PeerStore>>,
) {
    let store = network_store.read().await;
    let peers = peer_store.read().await;
    let networks = store.list();

    // Collect data in a Send-safe format
    let data: Vec<NetworkData> = networks
        .iter()
        .map(|n| {
            // Build member list with names from peer store
            let members: Vec<(String, String)> = n.members
                .iter()
                .filter_map(|peer_id| {
                    peers.find_by_id(peer_id).map(|peer| (peer_id.clone(), peer.name.clone()))
                })
                .collect();

            NetworkData {
                id: n.id.clone(),
                name: n.name.clone(),
                description: n.description.clone().unwrap_or_default(),
                member_count: n.member_count() as i32,
                is_owner: n.is_owner(),
                allow_introductions: n.can_introduce(),
                auto_accept_introductions: n.settings.auto_accept_introductions,
                share_member_list: n.settings.share_member_list,
                created_at: n.created_at.format("%Y-%m-%d").to_string(),
                members,
            }
        })
        .collect();

    let window_weak = window_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = window_weak.upgrade() {
            // Build NetworkItems with ModelRc on the UI thread
            let items: Vec<NetworkItem> = data
                .into_iter()
                .map(|d| {
                    let members: Vec<NetworkMember> = d.members
                        .into_iter()
                        .map(|(peer_id, name)| NetworkMember {
                            peer_id: SharedString::from(&peer_id),
                            name: SharedString::from(&name),
                        })
                        .collect();

                    NetworkItem {
                        id: SharedString::from(&d.id),
                        name: SharedString::from(&d.name),
                        description: SharedString::from(&d.description),
                        member_count: d.member_count,
                        is_owner: d.is_owner,
                        allow_introductions: d.allow_introductions,
                        auto_accept_introductions: d.auto_accept_introductions,
                        share_member_list: d.share_member_list,
                        created_at: SharedString::from(&d.created_at),
                        members: ModelRc::new(VecModel::from(members)),
                    }
                })
                .collect();

            window.global::<AppLogic>().set_networks(ModelRc::new(VecModel::from(items)));
        }
    });
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
        .map(|t| {
            // Calculate elapsed time and ETA
            let elapsed_secs = (chrono::Utc::now() - t.started_at).num_seconds().max(0) as u64;
            let elapsed = if t.status == TransferStatus::Running && elapsed_secs > 0 {
                croh_core::format_duration(elapsed_secs)
            } else {
                String::new()
            };

            // Calculate ETA based on progress and elapsed time
            let eta = if t.status == TransferStatus::Running && t.progress > 0.0 && t.progress < 100.0 {
                let remaining_percent = 100.0 - t.progress;
                let time_per_percent = elapsed_secs as f64 / t.progress;
                let eta_secs = (remaining_percent * time_per_percent) as u64;
                if eta_secs > 0 {
                    croh_core::format_eta(eta_secs)
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            TransferItem {
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
                // Enhanced stats
                total_size: SharedString::from(if t.total_size > 0 {
                    croh_core::format_size(t.total_size)
                } else {
                    String::new()
                }),
                transferred_size: SharedString::from(if t.transferred > 0 {
                    croh_core::format_size(t.transferred)
                } else {
                    String::new()
                }),
                elapsed: SharedString::from(elapsed),
                eta: SharedString::from(eta),
            }
        })
        .collect();

    // Count active transfers
    let active_count = items.iter().filter(|t| {
        let status = t.status.as_str();
        status == "running" || status == "pending"
    }).count() as i32;

    let window_weak = window_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = window_weak.upgrade() {
            let logic = window.global::<AppLogic>();
            logic.set_transfers(ModelRc::new(VecModel::from(items)));
            logic.set_active_transfer_count(active_count);
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
    if let Some(relay_url) = peer.relay_url() {
        if let Ok(url) = relay_url.parse::<croh_core::iroh::RelayUrl>() {
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
        relay_url: SharedString::from(p.relay_url().unwrap_or_default().to_string()),
        added_at: SharedString::from(added_at),
        expanded: false,
        // What they allow us to do
        can_push: p.their_permissions.push,
        can_pull: p.their_permissions.pull,
        can_browse: p.their_permissions.browse,
        can_screen_view: p.their_permissions.screen_view,
        // What we allow them to do
        allow_push: p.permissions_granted.push,
        allow_pull: p.permissions_granted.pull,
        allow_browse: p.permissions_granted.browse,
        allow_screen_view: p.permissions_granted.screen_view,
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
        peer_total_space: SharedString::from(""),
        peer_storage_percent: 0,
        peer_uptime: SharedString::from(""),
        peer_active_transfers: 0,
        // DND status (filled in by status updates)
        peer_dnd_mode: SharedString::from("off"),
        peer_dnd_message: SharedString::from(""),
        // Speed test results (filled in when test completes)
        speed_test_running: false,
        speed_test_upload: SharedString::from(""),
        speed_test_download: SharedString::from(""),
        speed_test_latency: SharedString::from(""),
        // Connection control (starts connected, user can toggle)
        connected: true,
        revoked: false,
        we_revoked: false,
        // Guest peer fields
        is_guest: p.is_guest,
        expires_at: SharedString::from(
            p.expires_at
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default()
        ),
        time_remaining: SharedString::from(format_time_remaining(p)),
        extension_count: p.extension_count as i32,
        promotion_pending: p.promotion_pending,
        // Chat - unread count is loaded separately from chat store
        unread_count: 0,
    }
}

/// Format the time remaining for a guest peer.
fn format_time_remaining(p: &TrustedPeer) -> String {
    match p.time_remaining() {
        Some(duration) => {
            let total_secs = duration.num_seconds();
            if total_secs <= 0 {
                "Expired".to_string()
            } else if total_secs < 60 {
                format!("{}s", total_secs)
            } else if total_secs < 3600 {
                format!("{}m", total_secs / 60)
            } else if total_secs < 86400 {
                let hours = total_secs / 3600;
                let mins = (total_secs % 3600) / 60;
                format!("{}h {}m", hours, mins)
            } else {
                let days = total_secs / 86400;
                let hours = (total_secs % 86400) / 3600;
                format!("{}d {}h", days, hours)
            }
        }
        None => String::new(),
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
    item.peer_total_space = SharedString::from(
        status.peer_total_space
            .map(|bytes| format_size(bytes))
            .unwrap_or_default()
    );
    // Calculate storage percent (percent FREE)
    item.peer_storage_percent = match (status.peer_free_space, status.peer_total_space) {
        (Some(free), Some(total)) if total > 0 => ((free as f64 / total as f64) * 100.0) as i32,
        _ => 0,
    };
    item.peer_uptime = SharedString::from(
        status.peer_uptime
            .map(|secs| croh_core::format_uptime(secs))
            .unwrap_or_default()
    );
    item.peer_active_transfers = status.peer_active_transfers.unwrap_or(0) as i32;
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

    // Apply full status from the peer_status map and count online peers
    let status_map = peer_status.read().await;
    let mut online_count = 0;
    for item in &mut new_items {
        if let Some(status) = status_map.get(&item.id.to_string()) {
            apply_status_to_peer_item(item, status);
            if status.online {
                online_count += 1;
            }
        }
    }
    drop(status_map);

    let total_count = new_items.len() as i32;
    let window_weak = window_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = window_weak.upgrade() {
            let logic = window.global::<AppLogic>();
            let current_model = logic.get_peers();

            // Build a map of current UI states by peer ID (expanded, connected, revoked, we_revoked)
            #[derive(Default)]
            struct PeerUiState {
                expanded: bool,
                connected: bool,
                revoked: bool,
                we_revoked: bool,
            }
            let mut ui_states: std::collections::HashMap<String, PeerUiState> = std::collections::HashMap::new();
            for i in 0..current_model.row_count() {
                if let Some(peer) = current_model.row_data(i) {
                    ui_states.insert(peer.id.to_string(), PeerUiState {
                        expanded: peer.expanded,
                        connected: peer.connected,
                        revoked: peer.revoked,
                        we_revoked: peer.we_revoked,
                    });
                }
            }

            // Apply preserved UI states to new items
            let items_with_state: Vec<PeerItem> = new_items.into_iter().map(|mut item| {
                if let Some(state) = ui_states.get(&item.id.to_string()) {
                    item.expanded = state.expanded;
                    item.connected = state.connected;
                    item.revoked = state.revoked;
                    item.we_revoked = state.we_revoked;
                    // If disconnected by user or we revoked, show as offline regardless of actual status
                    if !state.connected || state.we_revoked {
                        item.status = SharedString::from("offline");
                    }
                }
                item
            }).collect();

            // Update the peers model by replacing it
            // Note: This creates a fresh model, but preserves all UI states from above
            logic.set_peers(ModelRc::new(VecModel::from(items_with_state.clone())));

            // Update push-to-peer dropdown data
            let mut pushable_names: Vec<SharedString> = Vec::new();
            let mut pushable_ids: Vec<SharedString> = Vec::new();
            for item in &items_with_state {
                if item.can_push && !item.revoked && !item.we_revoked {
                    pushable_names.push(item.name.clone());
                    pushable_ids.push(item.id.clone());
                }
            }
            logic.set_peer_names_for_push(ModelRc::new(VecModel::from(pushable_names)));
            logic.set_pushable_peer_ids(ModelRc::new(VecModel::from(pushable_ids)));

            // Update peer counts in header
            logic.set_online_peer_count(online_count);
            logic.set_total_peer_count(total_count);
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
/// If `keep_completed` is false, the transfer is removed from the manager after saving.
/// If `keep_completed` is true, the transfer remains visible in the active transfer list.
async fn save_to_history(
    transfer_history: &Arc<RwLock<TransferHistory>>,
    transfer_manager: &TransferManager,
    transfer_id: &TransferId,
    keep_completed: bool,
) {
    if let Some(transfer) = transfer_manager.get(transfer_id).await {
        if transfer.status.is_terminal() {
            let mut history = transfer_history.write().await;
            if let Err(e) = history.add_and_save(transfer) {
                warn!("Failed to save transfer to history: {}", e);
            }
            // Only remove from active transfers if keep_completed is false
            if !keep_completed {
                let _ = transfer_manager.remove(transfer_id).await;
            }
        }
    }
}

/// Update session stats in the UI.
fn update_session_stats_ui(
    window_weak: &Weak<MainWindow>,
    session_stats: &Arc<RwLock<SessionStatsTracker>>,
) {
    let window_weak = window_weak.clone();
    let session_stats = session_stats.clone();

    let _ = slint::invoke_from_event_loop(move || {
        // Read stats synchronously since we're in the UI thread
        // We need to use try_read to avoid blocking
        let (uploaded, downloaded, count) = {
            if let Ok(stats) = session_stats.try_read() {
                (stats.bytes_uploaded, stats.bytes_downloaded, stats.transfers_completed)
            } else {
                return;
            }
        };

        if let Some(window) = window_weak.upgrade() {
            let logic = window.global::<AppLogic>();
            logic.set_session_stats(SessionStats {
                total_uploaded: SharedString::from(croh_core::format_size(uploaded)),
                total_downloaded: SharedString::from(croh_core::format_size(downloaded)),
                transfer_count: count as i32,
                avg_speed: SharedString::from(""),
                session_duration: SharedString::from(""),
            });
        }
    });
}

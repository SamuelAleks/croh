//! Application state and callback handling.

use croc_gui_core::{
    config::Theme,
    croc::{find_croc_executable, refresh_croc_cache, Curve, CrocEvent, CrocOptions, CrocProcess, HashAlgorithm},
    files, platform, Config, ControlMessage, Identity, IrohEndpoint, PeerStore, Permissions,
    Transfer, TransferId, TransferManager, TransferStatus, TransferType, TrustedPeer, TrustBundle,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::{AppLogic, AppSettings, MainWindow, PeerItem, SelectedFile, TransferItem};

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
    /// Peer store for trusted peers.
    peer_store: Arc<RwLock<PeerStore>>,
    /// Flag to track if trust initiation is in progress.
    trust_in_progress: Arc<RwLock<bool>>,
}

/// Internal representation of a selected file.
#[derive(Clone, Debug)]
struct SelectedFileData {
    name: String,
    size: u64,
    path: String,
}

impl App {
    /// Create a new App instance.
    pub fn new(window: Weak<MainWindow>) -> Self {
        let config = Config::load_with_env().unwrap_or_default();
        let peer_store = PeerStore::load().unwrap_or_default();

        Self {
            window,
            selected_files: Arc::new(RwLock::new(Vec::new())),
            transfer_manager: TransferManager::new(),
            active_processes: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(config)),
            identity: Arc::new(RwLock::new(None)),
            peer_store: Arc::new(RwLock::new(peer_store)),
            trust_in_progress: Arc::new(RwLock::new(false)),
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
                let theme = match config_guard.theme {
                    Theme::System => "system",
                    Theme::Light => "light",
                    Theme::Dark => "dark",
                };

                // Transfer options
                let hash_algorithm = config_guard.default_hash
                    .map(|h| h.as_str().to_string())
                    .unwrap_or_default();
                let curve = config_guard.default_curve
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_default();
                let throttle = config_guard.throttle.clone().unwrap_or_default();
                let no_local = config_guard.no_local;

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
                        };
                        window.global::<AppLogic>().set_settings(settings);
                    }
                });
            });
        });
    }

    /// Initialize identity and load peers.
    fn init_identity_and_peers(&self, _window: &MainWindow) {
        let identity_arc = self.identity.clone();
        let peer_store = self.peer_store.clone();
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
                        let window_weak_id = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_id.upgrade() {
                                window.global::<AppLogic>().set_endpoint_id(SharedString::from(display_id));
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to load identity: {}", e);
                    }
                }

                // Load peers and update UI
                let peers = peer_store.read().await;
                let peer_items: Vec<_> = peers.list().iter().map(|p| {
                    let last_seen = p.last_seen
                        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "Never".to_string());
                    (
                        p.id.clone(),
                        p.name.clone(),
                        if p.endpoint_id.len() > 16 {
                            format!("{}...{}", &p.endpoint_id[..8], &p.endpoint_id[p.endpoint_id.len()-8..])
                        } else {
                            p.endpoint_id.clone()
                        },
                        "offline".to_string(), // Status - will be updated when we add presence
                        last_seen,
                    )
                }).collect();
                drop(peers);

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = window_weak.upgrade() {
                        let items: Vec<PeerItem> = peer_items.into_iter()
                            .map(|(id, name, endpoint_id, status, last_seen)| PeerItem {
                                id: SharedString::from(id),
                                name: SharedString::from(name),
                                endpoint_id: SharedString::from(endpoint_id),
                                status: SharedString::from(status),
                                last_seen: SharedString::from(last_seen),
                            })
                            .collect();
                        window.global::<AppLogic>().set_peers(ModelRc::new(VecModel::from(items)));
                    }
                });
            });
        });
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
        let active_processes = self.active_processes.clone();
        let config = self.config.clone();
        let identity = self.identity.clone();
        let peer_store = self.peer_store.clone();

        // Start send callback
        window.global::<AppLogic>().on_start_send({
            let window_weak = window_weak.clone();
            let selected_files = selected_files.clone();
            let transfer_manager = transfer_manager.clone();
            let active_processes = active_processes.clone();
            let config = config.clone();

            move |custom_code| {
                let custom_code = custom_code.to_string();
                info!("Start send requested with custom_code: {:?}", if custom_code.is_empty() { "auto" } else { &custom_code });
                let window_weak = window_weak.clone();
                let selected_files = selected_files.clone();
                let transfer_manager = transfer_manager.clone();
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
            let active_processes = active_processes.clone();
            let config = config.clone();
            let identity = identity.clone();
            let peer_store = peer_store.clone();

            move |code| {
                let code = code.to_string();
                info!("Start receive requested with code: {}", code);

                let window_weak = window_weak.clone();
                let transfer_manager = transfer_manager.clone();
                let active_processes = active_processes.clone();
                let config = config.clone();
                let identity = identity.clone();
                let peer_store = peer_store.clone();

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

                                            // Check for trust bundles in received files
                                            check_and_handle_trust_bundle(&download_dir, &window_weak, &identity, &peer_store).await;
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
                        config_guard.theme = match theme.as_str() {
                            "light" => Theme::Light,
                            "dark" => Theme::Dark,
                            _ => Theme::System,
                        };

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
    }

    fn setup_peer_callbacks(&self, window: &MainWindow) {
        let window_weak = self.window.clone();
        let identity = self.identity.clone();
        let peer_store = self.peer_store.clone();
        let trust_in_progress = self.trust_in_progress.clone();
        let config = self.config.clone();

        // Initiate trust callback
        window.global::<AppLogic>().on_initiate_trust({
            let window_weak = window_weak.clone();
            let identity = identity.clone();
            let trust_in_progress = trust_in_progress.clone();
            let config = config.clone();
            let peer_store = peer_store.clone();

            move || {
                info!("Initiating trust...");
                let window_weak = window_weak.clone();
                let identity = identity.clone();
                let trust_in_progress = trust_in_progress.clone();
                let config = config.clone();
                let peer_store = peer_store.clone();

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

                        // Create trust bundle
                        let bundle = TrustBundle::new(&id);
                        let bundle_path = match bundle.save_to_temp() {
                            Ok(path) => path,
                            Err(e) => {
                                error!("Failed to save trust bundle: {}", e);
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
                                            info!("Trust bundle sent successfully, starting Iroh endpoint for handshake");
                                            update_status(&window_weak, "Trust bundle sent, waiting for peer to connect...");

                                            // Create Iroh endpoint and wait for handshake
                                            let id_clone = id.clone();
                                            let bundle_nonce = bundle.nonce.clone();

                                            match IrohEndpoint::new(id_clone).await {
                                                Ok(endpoint) => {
                                                    info!("Iroh endpoint ready, waiting for incoming connection");

                                                    // Wait for incoming connection with timeout
                                                    let handshake_result = tokio::time::timeout(
                                                        std::time::Duration::from_secs(120), // 2 minute timeout
                                                        wait_for_trust_handshake(&endpoint, &bundle_nonce, &peer_store)
                                                    ).await;

                                                    match handshake_result {
                                                        Ok(Ok(peer)) => {
                                                            info!("Trust established with {}", peer.name);
                                                            update_status(&window_weak, &format!("Trusted peer added: {}", peer.name));

                                                            // Update peers UI
                                                            let peers = peer_store.read().await;
                                                            let peer_items: Vec<_> = peers.list().iter().map(|p| {
                                                                let last_seen = p.last_seen
                                                                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                                                    .unwrap_or_else(|| "Never".to_string());
                                                                (
                                                                    p.id.clone(),
                                                                    p.name.clone(),
                                                                    if p.endpoint_id.len() > 16 {
                                                                        format!("{}...{}", &p.endpoint_id[..8], &p.endpoint_id[p.endpoint_id.len()-8..])
                                                                    } else {
                                                                        p.endpoint_id.clone()
                                                                    },
                                                                    "offline".to_string(),
                                                                    last_seen,
                                                                )
                                                            }).collect();
                                                            drop(peers);

                                                            let window_weak_peers = window_weak.clone();
                                                            let _ = slint::invoke_from_event_loop(move || {
                                                                if let Some(window) = window_weak_peers.upgrade() {
                                                                    let items: Vec<PeerItem> = peer_items.into_iter()
                                                                        .map(|(id, name, endpoint_id, status, last_seen)| PeerItem {
                                                                            id: SharedString::from(id),
                                                                            name: SharedString::from(name),
                                                                            endpoint_id: SharedString::from(endpoint_id),
                                                                            status: SharedString::from(status),
                                                                            last_seen: SharedString::from(last_seen),
                                                                        })
                                                                        .collect();
                                                                    window.global::<AppLogic>().set_peers(ModelRc::new(VecModel::from(items)));
                                                                }
                                                            });
                                                        }
                                                        Ok(Err(e)) => {
                                                            error!("Handshake failed: {}", e);
                                                            update_status(&window_weak, &format!("Handshake failed: {}", e));
                                                        }
                                                        Err(_) => {
                                                            warn!("Handshake timed out");
                                                            update_status(&window_weak, "Timed out waiting for peer");
                                                        }
                                                    }

                                                    endpoint.close().await;
                                                }
                                                Err(e) => {
                                                    error!("Failed to create Iroh endpoint: {}", e);
                                                    update_status(&window_weak, &format!("Network error: {}", e));
                                                }
                                            }

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

            move |id| {
                let id_str = id.to_string();
                info!("Removing peer: {}", id_str);
                let window_weak = window_weak.clone();
                let peer_store = peer_store.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async {
                        let mut store = peer_store.write().await;
                        if let Err(e) = store.remove(&id_str) {
                            error!("Failed to remove peer: {}", e);
                            update_status(&window_weak, &format!("Error: {}", e));
                            return;
                        }

                        // Update UI
                        let peer_items: Vec<_> = store.list().iter().map(|p| {
                            let last_seen = p.last_seen
                                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                .unwrap_or_else(|| "Never".to_string());
                            (
                                p.id.clone(),
                                p.name.clone(),
                                if p.endpoint_id.len() > 16 {
                                    format!("{}...{}", &p.endpoint_id[..8], &p.endpoint_id[p.endpoint_id.len()-8..])
                                } else {
                                    p.endpoint_id.clone()
                                },
                                "offline".to_string(),
                                last_seen,
                            )
                        }).collect();
                        drop(store);

                        let window_weak_ui = window_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(window) = window_weak_ui.upgrade() {
                                let items: Vec<PeerItem> = peer_items.into_iter()
                                    .map(|(id, name, endpoint_id, status, last_seen)| PeerItem {
                                        id: SharedString::from(id),
                                        name: SharedString::from(name),
                                        endpoint_id: SharedString::from(endpoint_id),
                                        status: SharedString::from(status),
                                        last_seen: SharedString::from(last_seen),
                                    })
                                    .collect();
                                window.global::<AppLogic>().set_peers(ModelRc::new(VecModel::from(items)));
                            }
                        });

                        update_status(&window_weak, "Peer removed");
                    });
                });
            }
        });
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

/// Check for trust bundles in a directory and handle them.
async fn check_and_handle_trust_bundle(
    download_dir: &PathBuf,
    window_weak: &Weak<MainWindow>,
    identity: &Arc<RwLock<Option<Identity>>>,
    peer_store: &Arc<RwLock<PeerStore>>,
) {
    use croc_gui_core::complete_trust_as_receiver;

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

    for entry in bundle_files {
        let path = entry.path();
        let bundle = match TrustBundle::load(&path) {
            Ok(b) => b,
            Err(e) => {
                warn!("Failed to load trust bundle {:?}: {}", path, e);
                continue;
            }
        };

        info!("Processing trust bundle from {}", bundle.sender.name);

        // Check if expired
        if !bundle.is_valid() {
            warn!("Trust bundle from {} has expired", bundle.sender.name);
            update_status(window_weak, &format!("Trust bundle from {} expired", bundle.sender.name));
            let _ = std::fs::remove_file(&path);
            continue;
        }

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
                    continue;
                }
            }
        };

        // Create Iroh endpoint and connect back
        let endpoint = match IrohEndpoint::new(our_identity.clone()).await {
            Ok(ep) => ep,
            Err(e) => {
                error!("Failed to create Iroh endpoint: {}", e);
                update_status(window_weak, &format!("Network error: {}", e));
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };

        update_status(window_weak, &format!("Connecting to {}...", bundle.sender.name));

        // Perform the handshake as receiver
        match complete_trust_as_receiver(&endpoint, &bundle, &our_identity).await {
            Ok(result) => {
                info!("Trust established with {}", result.peer.name);
                update_status(window_weak, &format!("Trusted peer added: {}", result.peer.name));

                // Add to peer store
                let mut store = peer_store.write().await;
                if let Err(e) = store.add(result.peer.clone()) {
                    error!("Failed to save peer: {}", e);
                } else {
                    // Update peers UI
                    let peer_items: Vec<_> = store.list().iter().map(|p| {
                        let last_seen = p.last_seen
                            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| "Never".to_string());
                        (
                            p.id.clone(),
                            p.name.clone(),
                            if p.endpoint_id.len() > 16 {
                                format!("{}...{}", &p.endpoint_id[..8], &p.endpoint_id[p.endpoint_id.len()-8..])
                            } else {
                                p.endpoint_id.clone()
                            },
                            "offline".to_string(),
                            last_seen,
                        )
                    }).collect();
                    drop(store);

                    let window_weak_peers = window_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = window_weak_peers.upgrade() {
                            let items: Vec<PeerItem> = peer_items.into_iter()
                                .map(|(id, name, endpoint_id, status, last_seen)| PeerItem {
                                    id: SharedString::from(id),
                                    name: SharedString::from(name),
                                    endpoint_id: SharedString::from(endpoint_id),
                                    status: SharedString::from(status),
                                    last_seen: SharedString::from(last_seen),
                                })
                                .collect();
                            window.global::<AppLogic>().set_peers(ModelRc::new(VecModel::from(items)));
                        }
                    });
                }
            }
            Err(e) => {
                error!("Handshake failed with {}: {}", bundle.sender.name, e);
                update_status(window_weak, &format!("Trust handshake failed: {}", e));
            }
        }

        endpoint.close().await;

        // Clean up the bundle file
        if let Err(e) = std::fs::remove_file(&path) {
            warn!("Failed to remove trust bundle file: {}", e);
        }
    }
}

/// Wait for incoming trust handshake from a peer who received our trust bundle.
async fn wait_for_trust_handshake(
    endpoint: &IrohEndpoint,
    expected_nonce: &str,
    peer_store: &Arc<RwLock<PeerStore>>,
) -> croc_gui_core::Result<TrustedPeer> {
    use std::time::Duration;

    info!("Waiting for incoming trust connection...");

    // Accept a connection
    let mut conn = tokio::time::timeout(Duration::from_secs(120), endpoint.accept())
        .await
        .map_err(|_| croc_gui_core::Error::Iroh("timeout waiting for connection".to_string()))??;

    let remote_id = conn.remote_id_string();
    info!("Accepted connection from: {}", remote_id);

    // Receive the TrustConfirm message
    let msg = tokio::time::timeout(Duration::from_secs(30), conn.recv())
        .await
        .map_err(|_| croc_gui_core::Error::Iroh("timeout waiting for message".to_string()))??;

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
                return Err(croc_gui_core::Error::Trust("invalid nonce".to_string()));
            }

            info!("Valid TrustConfirm from {} ({})", their_peer_info.name, remote_id);

            // Send TrustComplete
            let response = ControlMessage::TrustComplete;
            conn.send(&response).await?;

            // Create the trusted peer
            let peer = TrustedPeer::new(
                their_peer_info.endpoint_id.clone(),
                their_peer_info.name.clone(),
                Permissions::all(), // We grant all permissions
                their_permissions,  // Their permissions to us
            );

            // Add to peer store
            let mut store = peer_store.write().await;
            store.add(peer.clone())?;
            drop(store);

            // Close connection gracefully
            let _ = conn.close().await;

            info!("Trust established with {}", peer.name);
            Ok(peer)
        }
        other => {
            error!("Unexpected message during handshake: {:?}", other);
            Err(croc_gui_core::Error::Iroh(format!(
                "unexpected message: expected TrustConfirm, got {:?}",
                other
            )))
        }
    }
}

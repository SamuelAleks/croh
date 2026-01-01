//! Application state and callback handling.

use croc_gui_core::{
    config::Theme,
    croc::{find_croc_executable, refresh_croc_cache, Curve, CrocEvent, CrocOptions, CrocProcess, HashAlgorithm},
    files, platform, Config, Transfer, TransferId, TransferManager, TransferStatus, TransferType,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::{AppLogic, AppSettings, MainWindow, SelectedFile, TransferItem};

/// Application state.
pub struct App {
    window: Weak<MainWindow>,
    selected_files: Arc<RwLock<Vec<SelectedFileData>>>,
    transfer_manager: TransferManager,
    /// Active croc processes mapped by transfer ID.
    active_processes: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    config: Arc<RwLock<Config>>,
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

        Self {
            window,
            selected_files: Arc::new(RwLock::new(Vec::new())),
            transfer_manager: TransferManager::new(),
            active_processes: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(config)),
        }
    }

    /// Set up all UI callbacks.
    pub fn setup_callbacks(&self, window: &MainWindow) {
        // Initialize settings in UI
        self.init_settings(window);
        
        self.setup_file_callbacks(window);
        self.setup_transfer_callbacks(window);
        self.setup_settings_callbacks(window);
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

            move |code| {
                let code = code.to_string();
                info!("Start receive requested with code: {}", code);

                let window_weak = window_weak.clone();
                let transfer_manager = transfer_manager.clone();
                let active_processes = active_processes.clone();
                let config = config.clone();

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

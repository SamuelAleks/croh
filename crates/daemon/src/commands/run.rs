//! Run command - starts the daemon service.

use anyhow::Result;
use croh_core::{platform, Config, TransferManager};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::handlers::ScreenHandler;

/// Daemon state shared across the service.
pub struct DaemonState {
    #[allow(dead_code)] // Used for future features
    pub config: Config,
    pub transfer_manager: TransferManager,
    /// Screen streaming handler (optional, depends on config).
    pub screen_handler: Option<Arc<ScreenHandler>>,
    pub running: bool,
}

pub async fn execute(config_path: Option<String>) -> Result<()> {
    // Load configuration
    let config = if let Some(path) = config_path {
        info!("Loading config from: {}", path);
        let contents = std::fs::read_to_string(&path)?;
        serde_json::from_str(&contents)?
    } else {
        Config::load_with_env()?
    };

    info!("Croc Daemon v{}", env!("CARGO_PKG_VERSION"));
    info!("Download directory: {:?}", config.download_dir);

    // Ensure directories exist
    std::fs::create_dir_all(&config.download_dir)?;
    std::fs::create_dir_all(platform::data_dir())?;

    // Initialize screen streaming handler if enabled
    let screen_handler = if config.screen_stream.enabled {
        let (event_tx, mut event_rx) = mpsc::channel(256);
        let handler = Arc::new(ScreenHandler::new(config.screen_stream.clone(), event_tx));

        // Spawn event handler task
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                debug!("Screen stream event: {:?}", event);
                // Events can be used for monitoring/logging
                // In the future, these could be forwarded to connected clients
            }
        });

        info!(
            "Screen streaming enabled (max {}fps, input: {})",
            config.screen_stream.max_fps,
            if config.screen_stream.allow_input { "allowed" } else { "disabled" }
        );
        Some(handler)
    } else {
        debug!("Screen streaming disabled");
        None
    };

    // Initialize daemon state
    let state = Arc::new(RwLock::new(DaemonState {
        config,
        transfer_manager: TransferManager::new(),
        screen_handler,
        running: true,
    }));

    // Set up signal handlers for graceful shutdown
    let shutdown_state = state.clone();
    
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sighup = signal(SignalKind::hangup())?;
        
        let state_clone = shutdown_state.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = sigterm.recv() => {
                    info!("Received SIGTERM");
                }
                _ = sigint.recv() => {
                    info!("Received SIGINT");
                }
                _ = sighup.recv() => {
                    info!("Received SIGHUP - reloading config");
                    // Reload config on SIGHUP
                    if let Ok(new_config) = Config::load_with_env() {
                        let mut state = state_clone.write().await;
                        state.config = new_config;
                        info!("Configuration reloaded");
                    }
                    return; // Don't shutdown on SIGHUP
                }
            }
            
            // Initiate shutdown
            let mut state = state_clone.write().await;
            state.running = false;
        });
    }
    
    #[cfg(windows)]
    {
        let state_clone = shutdown_state.clone();
        tokio::spawn(async move {
            if let Ok(()) = tokio::signal::ctrl_c().await {
                info!("Received Ctrl+C");
                let mut state = state_clone.write().await;
                state.running = false;
            }
        });
    }

    info!("Daemon running. Press Ctrl+C to stop.");
    
    // Write PID file
    let pid_file = platform::data_dir().join("daemon.pid");
    std::fs::write(&pid_file, std::process::id().to_string())?;
    info!("PID file written to: {:?}", pid_file);

    // Main service loop
    loop {
        // Check if we should shut down
        {
            let state_guard = state.read().await;
            if !state_guard.running {
                break;
            }
        }

        // Sleep a bit before checking again
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Graceful shutdown
    info!("Initiating graceful shutdown...");
    
    // Cancel any active transfers
    {
        let state_guard = state.read().await;
        let active = state_guard.transfer_manager.list_active().await;
        if !active.is_empty() {
            warn!("Cancelling {} active transfer(s)", active.len());
            // In a real implementation, we'd properly cancel each transfer
        }
    }

    // Stop any active screen streaming sessions
    {
        let state_guard = state.read().await;
        if let Some(ref screen_handler) = state_guard.screen_handler {
            let stream_count = screen_handler.active_stream_count().await;
            if stream_count > 0 {
                info!("Stopping {} active screen stream(s)", stream_count);
            }
            screen_handler.stop_all().await;
        }
    }

    // Clean up temporary files
    let upload_dir = platform::upload_dir();
    if upload_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&upload_dir) {
            error!("Failed to clean up upload directory: {}", e);
        } else {
            info!("Cleaned up temporary upload directory");
        }
    }

    // Remove PID file
    if let Err(e) = std::fs::remove_file(&pid_file) {
        warn!("Failed to remove PID file: {}", e);
    }

    info!("Daemon stopped gracefully");
    Ok(())
}

/// Check if the daemon is running by reading the PID file.
pub fn is_daemon_running() -> Option<u32> {
    let pid_file = platform::data_dir().join("daemon.pid");
    
    if !pid_file.exists() {
        return None;
    }

    let pid_str = std::fs::read_to_string(&pid_file).ok()?;
    let pid: u32 = pid_str.trim().parse().ok()?;

    // Check if process is still running
    #[cfg(unix)]
    {
        use std::process::Command;
        let output = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .ok()?;
        
        if output.status.success() {
            return Some(pid);
        }
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid)])
            .output()
            .ok()?;
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains(&pid.to_string()) {
            return Some(pid);
        }
    }

    // PID file exists but process is dead - clean up stale PID file
    let _ = std::fs::remove_file(&pid_file);
    None
}

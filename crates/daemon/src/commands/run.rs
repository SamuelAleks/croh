//! Run command - starts the daemon service.

use anyhow::Result;
use croc_gui_core::Config;
use tracing::info;

pub async fn execute(config_path: Option<String>) -> Result<()> {
    // Load configuration
    let config = if let Some(path) = config_path {
        info!("Loading config from: {}", path);
        let contents = std::fs::read_to_string(&path)?;
        serde_json::from_str(&contents)?
    } else {
        Config::load_with_env()?
    };

    info!("Download directory: {:?}", config.download_dir);

    // Ensure download directory exists
    std::fs::create_dir_all(&config.download_dir)?;

    info!("Daemon running. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;

    info!("Shutting down...");
    Ok(())
}


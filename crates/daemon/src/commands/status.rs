//! Status command - shows daemon status.

use anyhow::Result;
use croc_gui_core::platform;

use super::run::is_daemon_running;

pub async fn execute() -> Result<()> {
    println!("Croc Daemon Status");
    println!("==================");
    println!();

    // Check if daemon is running
    match is_daemon_running() {
        Some(pid) => {
            println!("Status:     \x1b[32m● Running\x1b[0m");
            println!("PID:        {}", pid);
        }
        None => {
            println!("Status:     \x1b[31m○ Stopped\x1b[0m");
        }
    }

    println!();

    // Show paths
    println!("Paths:");
    println!("  Config:    {:?}", platform::config_dir());
    println!("  Data:      {:?}", platform::data_dir());
    println!("  Downloads: {:?}", platform::default_download_dir());

    println!();

    // Show config file status
    let config_file = platform::config_file_path();
    if config_file.exists() {
        println!("Config file: {:?}", config_file);
    } else {
        println!("Config file: Not found (using defaults)");
    }

    // Show peers file status
    let peers_file = platform::peers_file_path();
    if peers_file.exists() {
        println!("Peers file:  {:?}", peers_file);
    } else {
        println!("Peers file:  Not found (no trusted peers)");
    }

    Ok(())
}

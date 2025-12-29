//! Croc Daemon - Headless service for croc file transfers.
//!
//! This daemon runs in the background and:
//! - Accepts incoming trusted peer connections
//! - Processes incoming file transfers
//! - Manages the Iroh endpoint (future)

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

mod commands;

/// Croc Daemon - Headless file transfer service
#[derive(Parser)]
#[command(name = "croc-daemon")]
#[command(about = "Headless daemon for croc file transfers", long_about = None)]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the daemon service
    Run {
        /// Configuration file path
        #[arg(short, long)]
        config: Option<String>,
    },

    /// Receive a file using a croc code
    Receive {
        /// The croc code to receive
        code: String,
    },

    /// Show daemon status
    Status,

    /// List trusted peers
    Peers,

    /// Show or modify configuration
    Config {
        /// Key to get or set
        key: Option<String>,
        /// Value to set
        value: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let level = if cli.verbose {
        Level::DEBUG
    } else {
        Level::INFO
    };

    FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .init();

    match cli.command {
        Commands::Run { config } => {
            info!("Starting croc daemon...");
            commands::run::execute(config).await
        }
        Commands::Receive { code } => {
            info!("Receiving with code: {}", code);
            commands::receive::execute(&code).await
        }
        Commands::Status => {
            commands::status::execute().await
        }
        Commands::Peers => {
            commands::peers::execute().await
        }
        Commands::Config { key, value } => {
            commands::config::execute(key, value).await
        }
    }
}


//! Croc GUI - Native desktop application for croc file transfers.

use anyhow::Result;
use slint::ComponentHandle;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

slint::include_modules!();

mod app;

fn main() -> Result<()> {
    // Initialize logging
    FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    info!("Starting Croc GUI...");

    // Create the main window
    let window = MainWindow::new()?;

    // Initialize app state and wire up callbacks
    let app = app::App::new(window.as_weak());
    app.setup_callbacks(&window);

    info!("Croc GUI initialized successfully");

    // Run the event loop
    window.run()?;

    info!("Croc GUI shutting down");
    Ok(())
}


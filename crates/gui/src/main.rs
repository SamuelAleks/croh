//! Croc GUI - Native desktop application for croc file transfers.

use anyhow::Result;
use croh_core::Config;
use slint::{ComponentHandle, LogicalSize, WindowSize};
use tracing::{error, info, Level};
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

    // Load config to get window size
    let config = Config::load_with_env().unwrap_or_default();
    let initial_width = config.window_size.width;
    let initial_height = config.window_size.height;

    // Create the main window
    let window = MainWindow::new()?;

    // Set initial window size from config
    window
        .window()
        .set_size(WindowSize::Logical(LogicalSize::new(
            initial_width as f32,
            initial_height as f32,
        )));

    // Initialize app state and wire up callbacks
    let app = app::App::new(window.as_weak());
    app.setup_callbacks(&window);

    info!("Croc GUI initialized successfully");

    // Run the event loop
    window.run()?;

    // Save window size on exit
    let final_size = window.window().size();
    let mut config = Config::load_with_env().unwrap_or_default();
    config.window_size.width = final_size.width;
    config.window_size.height = final_size.height;
    if let Err(e) = config.save() {
        error!("Failed to save window size: {}", e);
    }

    info!("Croc GUI shutting down");
    Ok(())
}


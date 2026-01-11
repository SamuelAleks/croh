//! Croc GUI - Native desktop application for croc file transfers.

use anyhow::Result;
use croh_core::Config;
use slint::{ComponentHandle, LogicalSize, WindowSize};
use tracing::{error, info, Level};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::FmtSubscriber;

slint::include_modules!();

mod app;

/// Custom time format that includes instance name prefix if CROH_INSTANCE_NAME is set.
#[derive(Clone)]
struct InstancePrefixTime {
    instance_name: Option<String>,
}

impl FormatTime for InstancePrefixTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        if let Some(ref name) = self.instance_name {
            write!(w, "[{}]", name)
        } else {
            Ok(())
        }
    }
}

fn main() -> Result<()> {
    // Check for instance name (used by multi-instance test script)
    let instance_name = std::env::var("CROH_INSTANCE_NAME").ok();

    // Initialize logging with optional instance prefix
    FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .with_timer(InstancePrefixTime {
            instance_name: instance_name.clone(),
        })
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


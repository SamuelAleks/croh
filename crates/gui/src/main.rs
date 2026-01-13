//! Croc GUI - Native desktop application for croc file transfers.

use anyhow::Result;
use croh_core::Config;
use slint::{ComponentHandle, LogicalSize, Timer, TimerMode, WindowSize};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;
use tracing::{error, info};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::EnvFilter;

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
    // On Windows, ensure we have a console for logging output when run from terminal
    #[cfg(target_os = "windows")]
    {
        // Attach to parent console if available (e.g., when run from cmd/powershell)
        // This is a no-op if already attached or if there's no parent console
        unsafe {
            windows::Win32::System::Console::AttachConsole(
                windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
            )
        };
    }

    // Check for instance name (used by multi-instance test script)
    let instance_name = std::env::var("CROH_INSTANCE_NAME").ok();

    // Initialize logging with optional instance prefix
    // Filter out IPv6 STUN probe warnings from iroh's net_report module
    // These occur when IPv6 is unavailable/blocked and are not actionable
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,netwatch::net_report=error,iroh::magicsock::net_report=error")
    });

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(cfg!(not(target_os = "windows"))) // Disable ANSI on Windows for better compatibility
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

    // Set up debounced window size saver
    // Checks every 5000ms, saves 1 second after resizing stops
    let window_weak = window.as_weak();
    let last_size: Rc<RefCell<(u32, u32)>> = Rc::new(RefCell::new((initial_width, initial_height)));
    let last_change: Rc<RefCell<Option<Instant>>> = Rc::new(RefCell::new(None));
    let saved_size: Rc<RefCell<(u32, u32)>> =
        Rc::new(RefCell::new((initial_width, initial_height)));

    let size_check_timer = Timer::default();
    size_check_timer.start(
        TimerMode::Repeated,
        std::time::Duration::from_millis(5000),
        {
            let last_size = last_size.clone();
            let last_change = last_change.clone();
            let saved_size = saved_size.clone();
            move || {
                if let Some(window) = window_weak.upgrade() {
                    let current = window.window().size();
                    let current_size = (current.width, current.height);

                    // Check if size changed since last check
                    let size_changed = {
                        let last = last_size.borrow();
                        current_size != *last
                    };

                    if size_changed {
                        *last_size.borrow_mut() = current_size;
                        *last_change.borrow_mut() = Some(Instant::now());
                    }

                    // Check if we should save (size changed and stable for 1 second)
                    let should_save = {
                        let change_time = *last_change.borrow();
                        let saved = *saved_size.borrow();
                        if let Some(ct) = change_time {
                            current_size != saved && ct.elapsed().as_secs() >= 1
                        } else {
                            false
                        }
                    };

                    if should_save {
                        // Save the new size
                        if let Ok(mut config) = Config::load_with_env() {
                            config.window_size.width = current_size.0;
                            config.window_size.height = current_size.1;
                            if config.save().is_ok() {
                                *saved_size.borrow_mut() = current_size;
                                *last_change.borrow_mut() = None;
                            }
                        }
                    }
                }
            }
        },
    );

    info!("Croc GUI initialized successfully");

    // Run the event loop
    window.run()?;

    // Stop the timer
    drop(size_check_timer);

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

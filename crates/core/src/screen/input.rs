//! Input injection for remote desktop control.
//!
//! This module provides platform-specific input injection to allow
//! remote control of keyboard and mouse during screen streaming.
//!
//! ## Security
//!
//! Input injection requires explicit `screen_control` permission from the
//! peer. Additionally:
//! - Rate limiting prevents input flooding
//! - Dangerous key combinations can be blocked (configurable)
//! - Input is only accepted during active streaming sessions
//!
//! ## Platform Support
//!
//! | Platform | Backend | Requirements |
//! |----------|---------|--------------|
//! | Linux    | uinput  | /dev/uinput access (root or uinput group) |
//! | Windows  | SendInput | No special privileges |

use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::events::{KeyCode, KeyModifiers, RemoteInputEvent};

/// Input injector trait.
///
/// Platform-specific implementations provide actual input injection.
pub trait InputInjector: Send + Sync {
    /// Get the backend name for logging.
    fn name(&self) -> &'static str;

    /// Check if this backend is available on the current system.
    fn is_available(&self) -> bool;

    /// Initialize the injector (create virtual device, etc.).
    fn init(&mut self) -> Result<()>;

    /// Inject a single input event.
    fn inject(&mut self, event: &RemoteInputEvent) -> Result<()>;

    /// Inject multiple events atomically (for key combinations).
    fn inject_batch(&mut self, events: &[RemoteInputEvent]) -> Result<()> {
        for event in events {
            self.inject(event)?;
        }
        Ok(())
    }

    /// Clean up resources (destroy virtual device, etc.).
    fn shutdown(&mut self) -> Result<()>;

    /// Check if the injector requires elevated privileges.
    fn requires_privileges(&self) -> bool;
}

/// Input security settings.
#[derive(Debug, Clone)]
pub struct InputSecuritySettings {
    /// Maximum input events per second.
    pub max_events_per_second: u32,
    /// Block potentially dangerous key combinations.
    pub block_dangerous_combos: bool,
    /// List of blocked key combinations (e.g., Alt+F4).
    pub blocked_combos: Vec<BlockedCombo>,
}

impl Default for InputSecuritySettings {
    fn default() -> Self {
        Self {
            max_events_per_second: 1000, // Reasonable for normal use
            block_dangerous_combos: true,
            blocked_combos: vec![
                // Default dangerous combinations
                BlockedCombo::alt_f4(),
                BlockedCombo::ctrl_alt_del(),
            ],
        }
    }
}

/// A blocked key combination.
#[derive(Debug, Clone)]
pub struct BlockedCombo {
    /// Description for logging/display.
    pub description: String,
    /// Required key code.
    pub key: KeyCode,
    /// Required modifiers.
    pub modifiers: KeyModifiers,
}

impl BlockedCombo {
    /// Alt+F4 (close window).
    pub fn alt_f4() -> Self {
        Self {
            description: "Alt+F4".to_string(),
            key: KeyCode::F4,
            modifiers: KeyModifiers {
                alt: true,
                ..Default::default()
            },
        }
    }

    /// Ctrl+Alt+Delete.
    pub fn ctrl_alt_del() -> Self {
        Self {
            description: "Ctrl+Alt+Delete".to_string(),
            key: KeyCode::Delete,
            modifiers: KeyModifiers {
                control: true,
                alt: true,
                ..Default::default()
            },
        }
    }

    /// Check if an event matches this blocked combo.
    pub fn matches(&self, key: KeyCode, modifiers: &KeyModifiers) -> bool {
        key == self.key
            && modifiers.shift == self.modifiers.shift
            && modifiers.control == self.modifiers.control
            && modifiers.alt == self.modifiers.alt
            && modifiers.super_key == self.modifiers.super_key
    }
}

/// Rate limiter for input events.
#[derive(Debug)]
pub struct InputRateLimiter {
    /// Maximum events per second.
    max_per_second: u32,
    /// Events in the current window.
    events_in_window: u32,
    /// Start of the current rate limit window.
    window_start: Instant,
    /// Window duration (1 second).
    window_duration: Duration,
}

impl InputRateLimiter {
    /// Create a new rate limiter.
    pub fn new(max_per_second: u32) -> Self {
        Self {
            max_per_second,
            events_in_window: 0,
            window_start: Instant::now(),
            window_duration: Duration::from_secs(1),
        }
    }

    /// Check if an event should be allowed.
    /// Returns true if allowed, false if rate limited.
    pub fn check(&mut self) -> bool {
        let now = Instant::now();

        // Reset window if expired
        if now.duration_since(self.window_start) >= self.window_duration {
            self.window_start = now;
            self.events_in_window = 0;
        }

        // Check rate limit
        if self.events_in_window >= self.max_per_second {
            return false;
        }

        self.events_in_window += 1;
        true
    }

    /// Reset the rate limiter.
    pub fn reset(&mut self) {
        self.events_in_window = 0;
        self.window_start = Instant::now();
    }
}

/// Input handler with security checks.
pub struct SecureInputHandler {
    /// The underlying input injector.
    injector: Box<dyn InputInjector>,
    /// Security settings.
    settings: InputSecuritySettings,
    /// Rate limiter.
    rate_limiter: InputRateLimiter,
    /// Whether the handler is initialized.
    initialized: bool,
}

impl SecureInputHandler {
    /// Create a new secure input handler.
    pub fn new(injector: Box<dyn InputInjector>, settings: InputSecuritySettings) -> Self {
        let rate_limiter = InputRateLimiter::new(settings.max_events_per_second);
        Self {
            injector,
            settings,
            rate_limiter,
            initialized: false,
        }
    }

    /// Initialize the handler.
    pub fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.injector.init()?;
        self.initialized = true;
        Ok(())
    }

    /// Process and inject an input event with security checks.
    pub fn handle_input(&mut self, event: &RemoteInputEvent) -> Result<()> {
        if !self.initialized {
            return Err(Error::Screen("Input handler not initialized".into()));
        }

        // Rate limiting
        if !self.rate_limiter.check() {
            return Err(Error::Screen("Input rate limit exceeded".into()));
        }

        // Check for blocked combinations
        if self.settings.block_dangerous_combos {
            if let RemoteInputEvent::Key { key, pressed: true, modifiers } = event {
                for combo in &self.settings.blocked_combos {
                    if combo.matches(*key, modifiers) {
                        tracing::warn!("Blocked dangerous key combo: {}", combo.description);
                        return Err(Error::Screen(format!(
                            "Blocked key combination: {}",
                            combo.description
                        )));
                    }
                }
            }
        }

        // Inject the event
        self.injector.inject(event)
    }

    /// Shutdown the handler.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.initialized {
            self.injector.shutdown()?;
            self.initialized = false;
        }
        Ok(())
    }

    /// Get the underlying injector name.
    pub fn name(&self) -> &'static str {
        self.injector.name()
    }
}

// Re-export platform backends
#[cfg(target_os = "linux")]
pub use super::uinput::UinputInjector;

#[cfg(target_os = "windows")]
pub use super::windows_input::WindowsInputInjector;

/// Create the appropriate input injector for the current platform.
pub fn create_input_injector() -> Result<Box<dyn InputInjector>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(UinputInjector::new()))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(WindowsInputInjector::new()))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(Error::Screen(
            "Input injection not supported on this platform".into(),
        ))
    }
}

/// Check if input injection is available on this system.
pub fn is_input_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        UinputInjector::check_available()
    }

    #[cfg(target_os = "windows")]
    {
        true // Always available on Windows
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter() {
        let mut limiter = InputRateLimiter::new(5);

        // Should allow first 5 events
        for _ in 0..5 {
            assert!(limiter.check());
        }

        // Should block 6th event
        assert!(!limiter.check());

        // Reset and verify
        limiter.reset();
        assert!(limiter.check());
    }

    #[test]
    fn test_blocked_combo_alt_f4() {
        let combo = BlockedCombo::alt_f4();

        // Should match Alt+F4
        let mods_alt = KeyModifiers {
            alt: true,
            ..Default::default()
        };
        assert!(combo.matches(KeyCode::F4, &mods_alt));

        // Should not match F4 without Alt
        assert!(!combo.matches(KeyCode::F4, &KeyModifiers::default()));

        // Should not match Alt+F5
        assert!(!combo.matches(KeyCode::F5, &mods_alt));
    }

    #[test]
    fn test_blocked_combo_ctrl_alt_del() {
        let combo = BlockedCombo::ctrl_alt_del();

        let mods = KeyModifiers {
            control: true,
            alt: true,
            ..Default::default()
        };
        assert!(combo.matches(KeyCode::Delete, &mods));

        // Should not match with just Alt
        let mods_alt = KeyModifiers {
            alt: true,
            ..Default::default()
        };
        assert!(!combo.matches(KeyCode::Delete, &mods_alt));
    }

    #[test]
    fn test_security_settings_default() {
        let settings = InputSecuritySettings::default();
        assert_eq!(settings.max_events_per_second, 1000);
        assert!(settings.block_dangerous_combos);
        assert_eq!(settings.blocked_combos.len(), 2);
    }
}

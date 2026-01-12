//! Windows SendInput input injection backend.
//!
//! This backend uses the Windows SendInput API to inject keyboard
//! and mouse events.
//!
//! ## Requirements
//!
//! - No special privileges required
//! - Works on Windows Vista and later
//!
//! ## Notes
//!
//! - Some applications with elevated privileges (admin) may not receive
//!   injected input from non-elevated processes
//! - UAC prompts will not receive injected input for security reasons

#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, VIRTUAL_KEY,
};

#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics;

use tracing::{debug, info};

use crate::error::{Error, Result};

use super::events::{KeyCode, MouseButton, RemoteInputEvent};
use super::input::InputInjector;

/// Windows SendInput input injector.
pub struct WindowsInputInjector {
    /// Screen width for coordinate normalization.
    #[allow(dead_code)]
    screen_width: i32,
    /// Screen height for coordinate normalization.
    #[allow(dead_code)]
    screen_height: i32,
    /// Whether initialized.
    initialized: bool,
}

impl WindowsInputInjector {
    /// Create a new Windows input injector.
    pub fn new() -> Self {
        Self {
            screen_width: 1920,
            screen_height: 1080,
            initialized: false,
        }
    }

    /// Convert our KeyCode to Windows virtual key code.
    #[cfg(target_os = "windows")]
    fn keycode_to_vk(key: KeyCode) -> u16 {
        match key {
            // Letters
            KeyCode::A => 0x41,
            KeyCode::B => 0x42,
            KeyCode::C => 0x43,
            KeyCode::D => 0x44,
            KeyCode::E => 0x45,
            KeyCode::F => 0x46,
            KeyCode::G => 0x47,
            KeyCode::H => 0x48,
            KeyCode::I => 0x49,
            KeyCode::J => 0x4A,
            KeyCode::K => 0x4B,
            KeyCode::L => 0x4C,
            KeyCode::M => 0x4D,
            KeyCode::N => 0x4E,
            KeyCode::O => 0x4F,
            KeyCode::P => 0x50,
            KeyCode::Q => 0x51,
            KeyCode::R => 0x52,
            KeyCode::S => 0x53,
            KeyCode::T => 0x54,
            KeyCode::U => 0x55,
            KeyCode::V => 0x56,
            KeyCode::W => 0x57,
            KeyCode::X => 0x58,
            KeyCode::Y => 0x59,
            KeyCode::Z => 0x5A,

            // Numbers
            KeyCode::Key0 => 0x30,
            KeyCode::Key1 => 0x31,
            KeyCode::Key2 => 0x32,
            KeyCode::Key3 => 0x33,
            KeyCode::Key4 => 0x34,
            KeyCode::Key5 => 0x35,
            KeyCode::Key6 => 0x36,
            KeyCode::Key7 => 0x37,
            KeyCode::Key8 => 0x38,
            KeyCode::Key9 => 0x39,

            // Function keys
            KeyCode::F1 => 0x70,
            KeyCode::F2 => 0x71,
            KeyCode::F3 => 0x72,
            KeyCode::F4 => 0x73,
            KeyCode::F5 => 0x74,
            KeyCode::F6 => 0x75,
            KeyCode::F7 => 0x76,
            KeyCode::F8 => 0x77,
            KeyCode::F9 => 0x78,
            KeyCode::F10 => 0x79,
            KeyCode::F11 => 0x7A,
            KeyCode::F12 => 0x7B,

            // Modifiers
            KeyCode::LeftShift => 0xA0,
            KeyCode::RightShift => 0xA1,
            KeyCode::LeftControl => 0xA2,
            KeyCode::RightControl => 0xA3,
            KeyCode::LeftAlt => 0xA4,
            KeyCode::RightAlt => 0xA5,
            KeyCode::LeftSuper => 0x5B,
            KeyCode::RightSuper => 0x5C,

            // Navigation
            KeyCode::Up => 0x26,
            KeyCode::Down => 0x28,
            KeyCode::Left => 0x25,
            KeyCode::Right => 0x27,
            KeyCode::Home => 0x24,
            KeyCode::End => 0x23,
            KeyCode::PageUp => 0x21,
            KeyCode::PageDown => 0x22,
            KeyCode::Insert => 0x2D,
            KeyCode::Delete => 0x2E,

            // Editing
            KeyCode::Backspace => 0x08,
            KeyCode::Tab => 0x09,
            KeyCode::Return => 0x0D,
            KeyCode::Escape => 0x1B,
            KeyCode::Space => 0x20,

            // Punctuation
            KeyCode::Comma => 0xBC,
            KeyCode::Period => 0xBE,
            KeyCode::Slash => 0xBF,
            KeyCode::Backslash => 0xDC,
            KeyCode::Semicolon => 0xBA,
            KeyCode::Apostrophe => 0xDE,
            KeyCode::LeftBracket => 0xDB,
            KeyCode::RightBracket => 0xDD,
            KeyCode::Minus => 0xBD,
            KeyCode::Equals => 0xBB,
            KeyCode::Grave => 0xC0,

            // Numpad
            KeyCode::Numpad0 => 0x60,
            KeyCode::Numpad1 => 0x61,
            KeyCode::Numpad2 => 0x62,
            KeyCode::Numpad3 => 0x63,
            KeyCode::Numpad4 => 0x64,
            KeyCode::Numpad5 => 0x65,
            KeyCode::Numpad6 => 0x66,
            KeyCode::Numpad7 => 0x67,
            KeyCode::Numpad8 => 0x68,
            KeyCode::Numpad9 => 0x69,
            KeyCode::NumpadAdd => 0x6B,
            KeyCode::NumpadSubtract => 0x6D,
            KeyCode::NumpadMultiply => 0x6A,
            KeyCode::NumpadDivide => 0x6F,
            KeyCode::NumpadDecimal => 0x6E,
            KeyCode::NumpadEnter => 0x0D, // Same as regular Enter
            KeyCode::NumLock => 0x90,

            // Other
            KeyCode::CapsLock => 0x14,
            KeyCode::ScrollLock => 0x91,
            KeyCode::PrintScreen => 0x2C,
            KeyCode::Pause => 0x13,
            KeyCode::Menu => 0x5D,

            KeyCode::Unknown(code) => code as u16,
        }
    }
}

impl Default for WindowsInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector for WindowsInputInjector {
    fn name(&self) -> &'static str {
        "SendInput"
    }

    fn is_available(&self) -> bool {
        true // Always available on Windows
    }

    fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        info!("Initializing Windows SendInput injector");

        #[cfg(target_os = "windows")]
        unsafe {
            // Get screen dimensions
            // SM_CXSCREEN = 0, SM_CYSCREEN = 1
            self.screen_width = GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN);
            self.screen_height = GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN);
            info!(
                "Screen dimensions: {}x{}",
                self.screen_width, self.screen_height
            );
        }

        self.initialized = true;
        Ok(())
    }

    fn inject(&mut self, event: &RemoteInputEvent) -> Result<()> {
        if !self.initialized {
            return Err(Error::Screen("Windows input not initialized".into()));
        }

        #[cfg(target_os = "windows")]
        {
            match event {
                RemoteInputEvent::MouseMove { x, y, absolute } => {
                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT::default(),
                        },
                    };

                    unsafe {
                        if *absolute {
                            // Convert to normalized coordinates (0-65535)
                            let norm_x = (*x * 65535) / self.screen_width;
                            let norm_y = (*y * 65535) / self.screen_height;

                            input.Anonymous.mi.dx = norm_x;
                            input.Anonymous.mi.dy = norm_y;
                            input.Anonymous.mi.dwFlags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE;
                        } else {
                            input.Anonymous.mi.dx = *x;
                            input.Anonymous.mi.dy = *y;
                            input.Anonymous.mi.dwFlags = MOUSEEVENTF_MOVE;
                        }

                        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                    }
                }

                RemoteInputEvent::MouseButton { button, pressed } => {
                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT::default(),
                        },
                    };

                    let flags = match (button, pressed) {
                        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                        (MouseButton::Back, true) | (MouseButton::Forward, true) => MOUSEEVENTF_XDOWN,
                        (MouseButton::Back, false) | (MouseButton::Forward, false) => MOUSEEVENTF_XUP,
                        (MouseButton::Other(_), true) => MOUSEEVENTF_XDOWN,
                        (MouseButton::Other(_), false) => MOUSEEVENTF_XUP,
                    };

                    unsafe {
                        input.Anonymous.mi.dwFlags = flags;

                        // For XBUTTON events, set mouseData
                        match button {
                            MouseButton::Back => input.Anonymous.mi.mouseData = 1, // XBUTTON1
                            MouseButton::Forward => input.Anonymous.mi.mouseData = 2, // XBUTTON2
                            MouseButton::Other(n) => input.Anonymous.mi.mouseData = *n as u32,
                            _ => {}
                        }

                        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                    }
                }

                RemoteInputEvent::MouseScroll { dx, dy } => {
                    // Vertical scroll
                    if *dy != 0 {
                        let mut input = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT::default(),
                            },
                        };

                        unsafe {
                            input.Anonymous.mi.dwFlags = MOUSEEVENTF_WHEEL;
                            // WHEEL_DELTA is 120; multiply by scroll amount
                            input.Anonymous.mi.mouseData = (*dy * 120) as u32;

                            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                        }
                    }

                    // Horizontal scroll
                    if *dx != 0 {
                        let mut input = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT::default(),
                            },
                        };

                        unsafe {
                            input.Anonymous.mi.dwFlags = MOUSEEVENTF_HWHEEL;
                            input.Anonymous.mi.mouseData = (*dx * 120) as u32;

                            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                        }
                    }
                }

                RemoteInputEvent::Key { key, pressed, .. } => {
                    let vk = Self::keycode_to_vk(*key);

                    let mut input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT::default(),
                        },
                    };

                    unsafe {
                        input.Anonymous.ki.wVk = VIRTUAL_KEY(vk);
                        input.Anonymous.ki.dwFlags = if *pressed {
                            KEYEVENTF_SCANCODE
                        } else {
                            KEYEVENTF_KEYUP | KEYEVENTF_SCANCODE
                        };

                        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                    }
                }

                RemoteInputEvent::TextInput { text } => {
                    debug!("TextInput not fully supported yet: {}", text);
                    // Could use SendInput with Unicode characters
                }
            }

            Ok(())
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = event;
            Err(Error::Screen("Windows input only available on Windows".into()))
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        self.initialized = false;
        info!("Windows SendInput shutdown");
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        false // No special privileges needed
    }
}

//! Linux uinput input injection backend.
//!
//! This backend uses the Linux uinput subsystem to create a virtual
//! input device and inject keyboard/mouse events.
//!
//! ## Requirements
//!
//! - Access to `/dev/uinput` (typically requires root or uinput group membership)
//! - `CAP_DAC_OVERRIDE` capability, or appropriate permissions on /dev/uinput
//!
//! ## How it works
//!
//! 1. Open `/dev/uinput`
//! 2. Configure the virtual device (enable event types, key codes)
//! 3. Create the device with `UI_DEV_CREATE`
//! 4. Write `input_event` structs to inject input
//! 5. Destroy device with `UI_DEV_DESTROY` on shutdown

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::mem;
use std::os::unix::io::AsRawFd;

use tracing::{debug, info, warn};

use crate::error::{Error, Result};

use super::events::{KeyCode, MouseButton, RemoteInputEvent};
use super::input::InputInjector;

// Linux input event types
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

// Synchronization events
const SYN_REPORT: u16 = 0;

// Relative axis codes
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;
const REL_HWHEEL: u16 = 0x06;

// Absolute axis codes
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

// Mouse button codes (Linux)
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113; // Back
const BTN_EXTRA: u16 = 0x114; // Forward

// uinput ioctl commands
const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
const UI_SET_RELBIT: libc::c_ulong = 0x40045566;
const UI_SET_ABSBIT: libc::c_ulong = 0x40045567;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_DEV_SETUP: libc::c_ulong = 0x405C5503;
const UI_ABS_SETUP: libc::c_ulong = 0x40185504;

/// Input event structure (matches Linux input_event).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

impl Default for InputEvent {
    fn default() -> Self {
        Self {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_: 0,
            code: 0,
            value: 0,
        }
    }
}

/// uinput device setup structure.
#[repr(C)]
#[derive(Debug, Clone)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

/// Input device ID.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

/// Absolute axis setup.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct UinputAbsSetup {
    code: u16,
    absinfo: AbsInfo,
}

/// Absolute axis info.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// Linux uinput input injector.
pub struct UinputInjector {
    /// File handle to /dev/uinput.
    file: Option<File>,
    /// Screen dimensions for absolute positioning.
    screen_width: i32,
    screen_height: i32,
}

impl UinputInjector {
    /// Create a new uinput injector.
    pub fn new() -> Self {
        Self {
            file: None,
            screen_width: 1920, // Default, can be updated
            screen_height: 1080,
        }
    }

    /// Check if uinput is available.
    pub fn check_available() -> bool {
        std::path::Path::new("/dev/uinput").exists()
    }

    /// Set screen dimensions for absolute mouse positioning.
    pub fn set_screen_size(&mut self, width: i32, height: i32) {
        self.screen_width = width;
        self.screen_height = height;
    }

    /// Write an input event.
    fn write_event(&mut self, type_: u16, code: u16, value: i32) -> Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| Error::Screen("uinput device not initialized".into()))?;

        let event = InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        };

        let bytes = unsafe {
            std::slice::from_raw_parts(
                &event as *const InputEvent as *const u8,
                mem::size_of::<InputEvent>(),
            )
        };

        file.write_all(bytes)
            .map_err(|e| Error::Screen(format!("Failed to write input event: {}", e)))?;

        Ok(())
    }

    /// Write a synchronization event.
    fn sync(&mut self) -> Result<()> {
        self.write_event(EV_SYN, SYN_REPORT, 0)
    }

    /// Convert our KeyCode to Linux keycode.
    fn keycode_to_linux(key: KeyCode) -> u16 {
        match key {
            // Letters (KEY_A = 30, etc.)
            KeyCode::A => 30,
            KeyCode::B => 48,
            KeyCode::C => 46,
            KeyCode::D => 32,
            KeyCode::E => 18,
            KeyCode::F => 33,
            KeyCode::G => 34,
            KeyCode::H => 35,
            KeyCode::I => 23,
            KeyCode::J => 36,
            KeyCode::K => 37,
            KeyCode::L => 38,
            KeyCode::M => 50,
            KeyCode::N => 49,
            KeyCode::O => 24,
            KeyCode::P => 25,
            KeyCode::Q => 16,
            KeyCode::R => 19,
            KeyCode::S => 31,
            KeyCode::T => 20,
            KeyCode::U => 22,
            KeyCode::V => 47,
            KeyCode::W => 17,
            KeyCode::X => 45,
            KeyCode::Y => 21,
            KeyCode::Z => 44,

            // Numbers (KEY_1 = 2, etc.)
            KeyCode::Key1 => 2,
            KeyCode::Key2 => 3,
            KeyCode::Key3 => 4,
            KeyCode::Key4 => 5,
            KeyCode::Key5 => 6,
            KeyCode::Key6 => 7,
            KeyCode::Key7 => 8,
            KeyCode::Key8 => 9,
            KeyCode::Key9 => 10,
            KeyCode::Key0 => 11,

            // Function keys
            KeyCode::F1 => 59,
            KeyCode::F2 => 60,
            KeyCode::F3 => 61,
            KeyCode::F4 => 62,
            KeyCode::F5 => 63,
            KeyCode::F6 => 64,
            KeyCode::F7 => 65,
            KeyCode::F8 => 66,
            KeyCode::F9 => 67,
            KeyCode::F10 => 68,
            KeyCode::F11 => 87,
            KeyCode::F12 => 88,

            // Modifiers
            KeyCode::LeftShift => 42,
            KeyCode::RightShift => 54,
            KeyCode::LeftControl => 29,
            KeyCode::RightControl => 97,
            KeyCode::LeftAlt => 56,
            KeyCode::RightAlt => 100,
            KeyCode::LeftSuper => 125,
            KeyCode::RightSuper => 126,

            // Navigation
            KeyCode::Up => 103,
            KeyCode::Down => 108,
            KeyCode::Left => 105,
            KeyCode::Right => 106,
            KeyCode::Home => 102,
            KeyCode::End => 107,
            KeyCode::PageUp => 104,
            KeyCode::PageDown => 109,
            KeyCode::Insert => 110,
            KeyCode::Delete => 111,

            // Editing
            KeyCode::Backspace => 14,
            KeyCode::Tab => 15,
            KeyCode::Return => 28,
            KeyCode::Escape => 1,
            KeyCode::Space => 57,

            // Punctuation
            KeyCode::Comma => 51,
            KeyCode::Period => 52,
            KeyCode::Slash => 53,
            KeyCode::Backslash => 43,
            KeyCode::Semicolon => 39,
            KeyCode::Apostrophe => 40,
            KeyCode::LeftBracket => 26,
            KeyCode::RightBracket => 27,
            KeyCode::Minus => 12,
            KeyCode::Equals => 13,
            KeyCode::Grave => 41,

            // Numpad
            KeyCode::Numpad0 => 82,
            KeyCode::Numpad1 => 79,
            KeyCode::Numpad2 => 80,
            KeyCode::Numpad3 => 81,
            KeyCode::Numpad4 => 75,
            KeyCode::Numpad5 => 76,
            KeyCode::Numpad6 => 77,
            KeyCode::Numpad7 => 71,
            KeyCode::Numpad8 => 72,
            KeyCode::Numpad9 => 73,
            KeyCode::NumpadAdd => 78,
            KeyCode::NumpadSubtract => 74,
            KeyCode::NumpadMultiply => 55,
            KeyCode::NumpadDivide => 98,
            KeyCode::NumpadDecimal => 83,
            KeyCode::NumpadEnter => 96,
            KeyCode::NumLock => 69,

            // Other
            KeyCode::CapsLock => 58,
            KeyCode::ScrollLock => 70,
            KeyCode::PrintScreen => 99,
            KeyCode::Pause => 119,
            KeyCode::Menu => 127,

            KeyCode::Unknown(code) => code as u16,
        }
    }

    /// Convert MouseButton to Linux button code.
    fn mouse_button_to_linux(button: MouseButton) -> u16 {
        match button {
            MouseButton::Left => BTN_LEFT,
            MouseButton::Right => BTN_RIGHT,
            MouseButton::Middle => BTN_MIDDLE,
            MouseButton::Back => BTN_SIDE,
            MouseButton::Forward => BTN_EXTRA,
            MouseButton::Other(n) => BTN_LEFT + n as u16,
        }
    }
}

impl Default for UinputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector for UinputInjector {
    fn name(&self) -> &'static str {
        "uinput"
    }

    fn is_available(&self) -> bool {
        Self::check_available()
    }

    fn init(&mut self) -> Result<()> {
        if self.file.is_some() {
            return Ok(()); // Already initialized
        }

        info!("Initializing uinput device");

        // Open /dev/uinput
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|e| Error::Screen(format!("Failed to open /dev/uinput: {}", e)))?;

        let fd = file.as_raw_fd();

        unsafe {
            // Enable event types
            if libc::ioctl(fd, UI_SET_EVBIT, EV_KEY as libc::c_int) < 0 {
                return Err(Error::Screen("Failed to enable EV_KEY".into()));
            }
            if libc::ioctl(fd, UI_SET_EVBIT, EV_REL as libc::c_int) < 0 {
                return Err(Error::Screen("Failed to enable EV_REL".into()));
            }
            if libc::ioctl(fd, UI_SET_EVBIT, EV_ABS as libc::c_int) < 0 {
                return Err(Error::Screen("Failed to enable EV_ABS".into()));
            }
            if libc::ioctl(fd, UI_SET_EVBIT, EV_SYN as libc::c_int) < 0 {
                return Err(Error::Screen("Failed to enable EV_SYN".into()));
            }

            // Enable all keyboard keys (0-255)
            for key in 0..256u16 {
                libc::ioctl(fd, UI_SET_KEYBIT, key as libc::c_int);
            }

            // Enable mouse buttons
            for btn in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
                libc::ioctl(fd, UI_SET_KEYBIT, btn as libc::c_int);
            }

            // Enable relative axes (mouse movement)
            libc::ioctl(fd, UI_SET_RELBIT, REL_X as libc::c_int);
            libc::ioctl(fd, UI_SET_RELBIT, REL_Y as libc::c_int);
            libc::ioctl(fd, UI_SET_RELBIT, REL_WHEEL as libc::c_int);
            libc::ioctl(fd, UI_SET_RELBIT, REL_HWHEEL as libc::c_int);

            // Enable absolute axes (for absolute positioning)
            libc::ioctl(fd, UI_SET_ABSBIT, ABS_X as libc::c_int);
            libc::ioctl(fd, UI_SET_ABSBIT, ABS_Y as libc::c_int);

            // Setup absolute axis ranges
            let abs_x_setup = UinputAbsSetup {
                code: ABS_X,
                absinfo: AbsInfo {
                    value: 0,
                    minimum: 0,
                    maximum: self.screen_width,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            };
            libc::ioctl(fd, UI_ABS_SETUP, &abs_x_setup);

            let abs_y_setup = UinputAbsSetup {
                code: ABS_Y,
                absinfo: AbsInfo {
                    value: 0,
                    minimum: 0,
                    maximum: self.screen_height,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            };
            libc::ioctl(fd, UI_ABS_SETUP, &abs_y_setup);

            // Setup device info
            let mut setup = UinputSetup {
                id: InputId {
                    bustype: 0x03, // BUS_USB
                    vendor: 0x1234,
                    product: 0x5678,
                    version: 1,
                },
                name: [0; 80],
                ff_effects_max: 0,
            };

            let name = b"croh-virtual-input";
            setup.name[..name.len()].copy_from_slice(name);

            if libc::ioctl(fd, UI_DEV_SETUP, &setup) < 0 {
                return Err(Error::Screen("Failed to setup uinput device".into()));
            }

            // Create the device
            if libc::ioctl(fd, UI_DEV_CREATE) < 0 {
                return Err(Error::Screen("Failed to create uinput device".into()));
            }
        }

        // Give the kernel time to create the device
        std::thread::sleep(std::time::Duration::from_millis(100));

        self.file = Some(file);
        info!("uinput device created successfully");

        Ok(())
    }

    fn inject(&mut self, event: &RemoteInputEvent) -> Result<()> {
        match event {
            RemoteInputEvent::MouseMove { x, y, absolute } => {
                if *absolute {
                    // Absolute positioning
                    self.write_event(EV_ABS, ABS_X, *x)?;
                    self.write_event(EV_ABS, ABS_Y, *y)?;
                } else {
                    // Relative movement
                    self.write_event(EV_REL, REL_X, *x)?;
                    self.write_event(EV_REL, REL_Y, *y)?;
                }
                self.sync()?;
            }

            RemoteInputEvent::MouseButton { button, pressed } => {
                let linux_btn = Self::mouse_button_to_linux(*button);
                self.write_event(EV_KEY, linux_btn, if *pressed { 1 } else { 0 })?;
                self.sync()?;
            }

            RemoteInputEvent::MouseScroll { dx, dy } => {
                if *dy != 0 {
                    self.write_event(EV_REL, REL_WHEEL, *dy)?;
                }
                if *dx != 0 {
                    self.write_event(EV_REL, REL_HWHEEL, *dx)?;
                }
                self.sync()?;
            }

            RemoteInputEvent::Key { key, pressed, .. } => {
                let linux_key = Self::keycode_to_linux(*key);
                self.write_event(EV_KEY, linux_key, if *pressed { 1 } else { 0 })?;
                self.sync()?;
            }

            RemoteInputEvent::TextInput { text } => {
                // For text input, we'd need to convert to key events
                // This is complex and locale-dependent; log a warning for now
                debug!("TextInput not fully supported yet: {}", text);
            }
        }

        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            let fd = file.as_raw_fd();
            unsafe {
                libc::ioctl(fd, UI_DEV_DESTROY);
            }
            info!("uinput device destroyed");
        }
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        true // Needs access to /dev/uinput
    }
}

impl Drop for UinputInjector {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            warn!("Error shutting down uinput: {}", e);
        }
    }
}

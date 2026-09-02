//! What the machine does once a command is understood.
//!
//! Three routes, in order of preference:
//!   - [`open_app`] to launch or focus applications
//!   - [`press`] to send key combinations
//!   - [`applescript`] for what neither covers (volume, media transport)

use std::process::Command;

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

/// macOS virtual key codes.
///
/// These are **positional**, not character-based: code 8 is wherever `C`
/// sits on a US keyboard, which on a Spanish ISO layout is also `C`. That
/// is why Cmd+C works on both. Symbols are the exception — brackets and
/// dashes sit in different places, so avoid them in shortcuts.
pub mod key {
    pub const A: u16 = 0;
    pub const S: u16 = 1;
    pub const F: u16 = 3;
    pub const H: u16 = 4;
    pub const Z: u16 = 6;
    pub const X: u16 = 7;
    pub const C: u16 = 8;
    pub const V: u16 = 9;
    pub const Q: u16 = 12;
    pub const W: u16 = 13;
    pub const R: u16 = 15;
    pub const T: u16 = 17;
    pub const L: u16 = 37;
    pub const M: u16 = 46;
    pub const N: u16 = 45;
    pub const DIGIT_3: u16 = 20;
    pub const DIGIT_4: u16 = 21;
    pub const TAB: u16 = 48;
    pub const SPACE: u16 = 49;
    pub const DELETE: u16 = 51;
    pub const LEFT: u16 = 123;
    pub const RIGHT: u16 = 124;
    pub const DOWN: u16 = 125;
    pub const UP: u16 = 126;
}

/// Modifier keys held during a keystroke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub command: bool,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

impl Mods {
    pub const NONE: Mods = Mods::new(false, false, false, false);
    pub const CMD: Mods = Mods::new(true, false, false, false);
    pub const CMD_SHIFT: Mods = Mods::new(true, true, false, false);
    pub const CTRL: Mods = Mods::new(false, false, false, true);
    pub const CTRL_CMD: Mods = Mods::new(true, false, false, true);

    const fn new(command: bool, shift: bool, option: bool, control: bool) -> Self {
        Self { command, shift, option, control }
    }

    fn flags(self) -> CGEventFlags {
        let mut flags = CGEventFlags::empty();
        if self.command {
            flags |= CGEventFlags::CGEventFlagCommand;
        }
        if self.shift {
            flags |= CGEventFlags::CGEventFlagShift;
        }
        if self.option {
            flags |= CGEventFlags::CGEventFlagAlternate;
        }
        if self.control {
            flags |= CGEventFlags::CGEventFlagControl;
        }
        flags
    }
}

/// Sends a key combination to whatever is in front.
///
/// Needs Accessibility permission. Without it this does not fail — the
/// events are simply swallowed, which is macOS's most baffling failure
/// mode. Check [`has_accessibility_permission`] at startup instead.
pub fn press(code: u16, mods: Mods) -> bool {
    let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        return false;
    };
    let flags = mods.flags();

    for down in [true, false] {
        let Ok(event) = CGEvent::new_keyboard_event(source.clone(), code, down) else {
            return false;
        };
        event.set_flags(flags);
        event.post(CGEventTapLocation::HID);
    }
    true
}

/// Launches the application, or brings it forward if already running.
///
/// `open -b` covers all three cases at once: not running (launches it),
/// running with no windows (asks for one), and running with a window
/// (activates it). The middle case is common on macOS, where closing the
/// last window does not quit the app — and where `focus()` alone would
/// swap the menu bar while showing nothing.
pub fn open_app(bundle_id: &str) -> bool {
    Command::new("/usr/bin/open")
        .arg("-b")
        .arg(bundle_id)
        .spawn()
        .is_ok()
}

/// Asks an application to quit.
///
/// A polite quit, not a kill: if there is unsaved work the app puts up its
/// own save dialog, exactly as ⌘Q would. Nothing is lost without being
/// asked about first.
pub fn quit_app(bundle_id: &str) -> bool {
    applescript(&format!("tell application id \"{bundle_id}\" to quit"))
}

/// Runs an AppleScript snippet.
pub fn applescript(script: &str) -> bool {
    Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .spawn()
        .is_ok()
}

/// Adjusts system output volume by `delta` points on a 0-100 scale.
pub fn adjust_volume(delta: i32) -> bool {
    applescript(&format!(
        "set v to output volume of (get volume settings)\n\
         set volume output volume (v + {delta})"
    ))
}

pub fn set_muted(muted: bool) -> bool {
    applescript(&format!("set volume output muted {muted}"))
}

/// Plays a short system sound as feedback.
pub fn play_sound(path: &str) {
    let _ = Command::new("/usr/bin/afplay").arg(path).spawn();
}

/// Whether we can post keyboard events at all.
pub fn has_accessibility_permission() -> bool {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).is_ok()
}

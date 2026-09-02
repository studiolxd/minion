//! A keyboard shortcut that works from anywhere.
//!
//! Pausing by voice is easy; getting Minion's attention back is not, since
//! a paused microphone hears nothing. The menu bar always works, but it
//! needs the mouse. This watches for one combination system-wide.
//!
//! It runs on a thread of its own with its own run loop. The obvious
//! approach — `NSEvent`'s global monitor — hangs off the main run loop,
//! the same one that tracks the menu bar, and in practice left the menu
//! unable to open at all. An event tap on a separate thread cannot reach
//! the main loop to break it.
//!
//! The tap only listens; it does not consume the keystroke, so the
//! combination still reaches whatever has focus.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};

use crate::actions::Mods;

/// Modifier bits as they arrive in a CGEvent's flags.
mod flags {
    pub const SHIFT: u64 = 0x0002_0000;
    pub const CONTROL: u64 = 0x0004_0000;
    pub const OPTION: u64 = 0x0008_0000;
    pub const COMMAND: u64 = 0x0010_0000;
}

/// Starts watching for `shortcut`, flipping `active` when it arrives.
///
/// Returns whether the shortcut could be read. The thread runs for the life
/// of the process.
pub fn watch(shortcut: &str, active: Arc<AtomicBool>) -> bool {
    let Some((code, mods)) = crate::actions::parse_shortcut(shortcut) else {
        return false;
    };

    std::thread::spawn(move || {
        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            // Listening only: the keystroke still reaches whatever has
            // focus, so the combination is shared rather than claimed.
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::KeyDown],
            move |_proxy, _type, event| {
                let pressed = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                if pressed != i64::from(code) || !modifiers_match(event.get_flags().bits(), mods)
                {
                    return CallbackResult::Keep;
                }
                let now = !active.load(Ordering::Relaxed);
                active.store(now, Ordering::Relaxed);
                crate::journal::write(if now {
                    "resumed by shortcut"
                } else {
                    "paused by shortcut"
                });
                // Kept, not dropped: the combination still reaches whatever
                // has focus, so Minion shares it rather than claiming it.
                CallbackResult::Keep
            },
        );

        let Ok(tap) = tap else {
            crate::journal::write(
                "Could not watch the shortcut: macOS refused the event tap. \
                 Accessibility permission is required.",
            );
            return;
        };

        // Its own run loop, on its own thread. This is the whole point: the
        // main one belongs to the menu bar.
        let source = tap.mach_port().create_runloop_source(0);
        let Ok(source) = source else {
            crate::journal::write("Could not attach the shortcut watcher.");
            return;
        };
        let run_loop = CFRunLoop::get_current();
        unsafe { run_loop.add_source(&source, kCFRunLoopCommonModes) };
        tap.enable();
        CFRunLoop::run_current();
    });

    true
}

/// Whether the event's modifiers are exactly the ones wanted.
///
/// Exactly, not merely including: otherwise ⌥Space would also fire on
/// ⌥⌘Space, which belongs to something else.
fn modifiers_match(bits: u64, wanted: Mods) -> bool {
    let held = |bit: u64| bits & bit != 0;
    held(flags::COMMAND) == wanted.command
        && held(flags::SHIFT) == wanted.shift
        && held(flags::OPTION) == wanted.option
        && held(flags::CONTROL) == wanted.control
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_must_match_exactly() {
        let wanted = Mods { option: true, ..Mods::NONE };
        assert!(modifiers_match(flags::OPTION, wanted));
        // An extra modifier belongs to a different shortcut.
        assert!(!modifiers_match(flags::OPTION | flags::COMMAND, wanted));
    }

    #[test]
    fn a_missing_modifier_does_not_match() {
        let wanted = Mods { option: true, ..Mods::NONE };
        assert!(!modifiers_match(0, wanted));
    }

    #[test]
    fn every_modifier_is_recognised() {
        let all = Mods { command: true, shift: true, option: true, control: true };
        let bits = flags::COMMAND | flags::SHIFT | flags::OPTION | flags::CONTROL;
        assert!(modifiers_match(bits, all));
    }
}

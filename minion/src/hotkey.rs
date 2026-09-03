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

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use core_foundation::runloop::{kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop};
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

/// How long the tap's run loop runs before coming up for air.
///
/// Everything the callback is not allowed to do — writing to the log,
/// turning the tap back on — happens between two of these, so it is short
/// enough not to be noticed and long enough to cost nothing.
const TICK: Duration = Duration::from_millis(500);

/// What the callback saw, for the loop to write down. A number rather than
/// a string because it is set from inside the tap, where the only safe
/// work is an atomic store.
mod pending {
    pub const NOTHING: u8 = 0;
    pub const RESUMED: u8 = 1;
    pub const PAUSED: u8 = 2;
}

/// Starts watching for `shortcut`, flipping `active` when it arrives.
///
/// Returns whether the shortcut could be read. The thread runs for the life
/// of the process.
pub fn watch(shortcut: &str, active: Arc<AtomicBool>) -> bool {
    let Some((code, mods)) = crate::actions::parse_shortcut(shortcut) else {
        return false;
    };

    // macOS switches a tap off when its callback takes too long, and used
    // to do it silently: the shortcut simply stopped working until the next
    // restart. Both flags are raised inside the tap and acted on outside it.
    let switched_off = Arc::new(AtomicBool::new(false));
    let tap_switched_off = Arc::clone(&switched_off);
    let seen = Arc::new(AtomicU8::new(pending::NOTHING));
    let tap_seen = Arc::clone(&seen);

    std::thread::spawn(move || {
        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            // Listening only: the keystroke still reaches whatever has
            // focus, so the combination is shared rather than claimed.
            CGEventTapOptions::ListenOnly,
            vec![
                CGEventType::KeyDown,
                // Not keys: the two ways macOS tells a tap it has been
                // turned off. Without them there is no way to know.
                CGEventType::TapDisabledByTimeout,
                CGEventType::TapDisabledByUserInput,
            ],
            move |_proxy, kind, event| {
                if matches!(
                    kind,
                    CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                ) {
                    tap_switched_off.store(true, Ordering::Relaxed);
                    return CallbackResult::Keep;
                }
                let pressed = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                if pressed != i64::from(code) || !modifiers_match(event.get_flags().bits(), mods)
                {
                    return CallbackResult::Keep;
                }
                let now = !active.load(Ordering::Relaxed);
                active.store(now, Ordering::Relaxed);
                // Nothing slower than an atomic store in here: a callback
                // that dawdles is exactly what gets the tap turned off, and
                // writing to the log opens a file and takes a lock.
                tap_seen.store(
                    if now { pending::RESUMED } else { pending::PAUSED },
                    Ordering::Relaxed,
                );
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

        // Run in slices rather than for ever, so there is somewhere to do
        // the work the callback must not: turning the tap back on, and
        // saying what happened.
        loop {
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, TICK, false);
            if switched_off.swap(false, Ordering::Relaxed) {
                tap.enable();
                crate::journal::write(
                    "macOS switched the shortcut watcher off; switched it back on.",
                );
            }
            match seen.swap(pending::NOTHING, Ordering::Relaxed) {
                pending::RESUMED => crate::journal::write("resumed by shortcut"),
                pending::PAUSED => crate::journal::write("paused by shortcut"),
                _ => {}
            }
        }
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

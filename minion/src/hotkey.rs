//! A keyboard shortcut that works from anywhere.
//!
//! Pausing by voice is easy; getting Minion's attention back is not, since
//! a paused microphone hears nothing. The menu bar always works, but it
//! needs the mouse. This watches for one combination system-wide.
//!
//! Uses a global event monitor rather than the older hotkey registration:
//! it needs the Accessibility permission, which Minion already requires for
//! its key commands, and it does not claim the combination away from other
//! applications — the keystroke still reaches whatever has focus.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use block2::RcBlock;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

use crate::actions::Mods;

/// Watches for the combination and flips `active` when it arrives.
///
/// The returned token must be kept alive; dropping it stops the watch.
pub struct Watch {
    _monitor: Option<objc2::rc::Retained<objc2::runtime::AnyObject>>,
}

/// Starts watching. Returns `None` if the shortcut cannot be read.
pub fn watch(shortcut: &str, active: Arc<AtomicBool>) -> Option<Watch> {
    let (code, mods) = crate::actions::parse_shortcut(shortcut)?;

    let handler = RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
        let event = unsafe { event.as_ref() };
        if event.keyCode() != code {
            return;
        }
        if !modifiers_match(event.modifierFlags(), mods) {
            return;
        }
        let now = !active.load(Ordering::Relaxed);
        active.store(now, Ordering::Relaxed);
        crate::journal::write(if now {
            "resumed by shortcut"
        } else {
            "paused by shortcut"
        });
    });

    let monitor =
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler);
    Some(Watch { _monitor: monitor })
}

/// Whether the event's modifiers are exactly the ones wanted.
///
/// Exactly, not merely including: otherwise ⌥Space would also fire on
/// ⌥⌘Space, which belongs to something else.
fn modifiers_match(flags: NSEventModifierFlags, wanted: Mods) -> bool {
    let held = |flag: NSEventModifierFlags| flags.contains(flag);
    held(NSEventModifierFlags::Command) == wanted.command
        && held(NSEventModifierFlags::Shift) == wanted.shift
        && held(NSEventModifierFlags::Option) == wanted.option
        && held(NSEventModifierFlags::Control) == wanted.control
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_must_match_exactly() {
        let option_only = NSEventModifierFlags::Option;
        let wanted = Mods { option: true, ..Mods::NONE };
        assert!(modifiers_match(option_only, wanted));

        // An extra modifier belongs to a different shortcut.
        let option_and_command = NSEventModifierFlags::Option | NSEventModifierFlags::Command;
        assert!(!modifiers_match(option_and_command, wanted));
    }

    #[test]
    fn a_missing_modifier_does_not_match() {
        let wanted = Mods { option: true, ..Mods::NONE };
        assert!(!modifiers_match(NSEventModifierFlags::empty(), wanted));
    }
}

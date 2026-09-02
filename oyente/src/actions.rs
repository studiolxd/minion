//! What the machine does once a command is understood.
//!
//! Three routes, in order of preference:
//!   - [`open_app`] to launch or focus applications
//!   - [`press`] to send key combinations
//!   - [`applescript`] for what neither covers (volume, media transport)

use std::process::Command;

use core_foundation::base::TCFType;
use objc2_app_kit::NSWorkspace;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::CFString;
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
    pub const Y: u16 = 16;
    pub const I: u16 = 34;
    pub const B: u16 = 11;
    pub const E: u16 = 14;
    pub const L: u16 = 37;
    pub const M: u16 = 46;
    pub const N: u16 = 45;
    pub const DIGIT_1: u16 = 18;
    pub const DIGIT_2: u16 = 19;
    pub const DIGIT_3: u16 = 20;
    pub const DIGIT_4: u16 = 21;
    pub const DIGIT_5: u16 = 23;
    pub const DIGIT_9: u16 = 25;
    pub const TAB: u16 = 48;
    pub const SPACE: u16 = 49;
    pub const DELETE: u16 = 51;
    pub const ESCAPE: u16 = 53;
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
    pub const CTRL_SHIFT: Mods = Mods::new(false, true, false, true);
    pub const OPTION: Mods = Mods::new(false, false, true, false);

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
    // Checked here rather than only at startup: the permission can be
    // granted while Oyente is running and takes effect immediately, so a
    // one-off check at launch goes stale the moment the user turns it on.
    if !has_accessibility_permission() {
        return false;
    }
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

/// Types text into whatever has focus.
///
/// Sends the characters as a Unicode string rather than as key codes, so
/// accents and ñ come out right regardless of keyboard layout — simulating
/// the keystrokes for "acción" on a Spanish ISO layout would be a mess of
/// dead keys.
///
/// Long text is sent in chunks: the event queue drops oversized payloads
/// silently, which would lose the tail of a dictated sentence.
pub fn type_text(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    if !has_accessibility_permission() {
        return false;
    }
    let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        return false;
    };

    const CHUNK_CHARS: usize = 20;
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(CHUNK_CHARS) {
        let piece: String = chunk.iter().collect();
        let Ok(event) = CGEvent::new_keyboard_event(source.clone(), 0, true) else {
            return false;
        };
        event.set_string(&piece);
        event.post(CGEventTapLocation::HID);
        // A short gap keeps the receiving app from dropping characters.
        std::thread::sleep(std::time::Duration::from_millis(6));
    }
    true
}

/// Bundle identifier of the application currently in front.
///
/// What makes a command mean different things in different places: "limpia
/// la pantalla" is ⌃L in a terminal and nothing anywhere else.
pub fn frontmost_app() -> Option<String> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    Some(app.bundleIdentifier()?.to_string())
}

/// Opens a web address, optionally in a named browser.
///
/// With no browser given it goes to the system default. Passing one matters
/// when a different browser is already in front: opening a link in the
/// default browser while you are working in another is jarring, and leaves
/// the page somewhere you were not looking.
pub fn open_url(url: &str, browser_bundle_id: Option<&str>) -> bool {
    let mut command = Command::new("/usr/bin/open");
    if let Some(bundle_id) = browser_bundle_id {
        command.arg("-b").arg(bundle_id);
    }
    command.arg(url).spawn().is_ok()
}

/// Opens a search in Spotify.
///
/// Searching rather than playing: starting a specific track needs the Web
/// API and an OAuth token, which is a different project. This lands on the
/// results with the app in front, one click from playing.
pub fn search_spotify(query: &str) -> bool {
    let encoded: String = query
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_string()
            } else if c == ' ' {
                "%20".to_string()
            } else {
                // Percent-encode everything else, accents included.
                let mut buffer = [0u8; 4];
                c.encode_utf8(&mut buffer)
                    .bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect()
            }
        })
        .collect();
    open_url(&format!("spotify:search:{encoded}"), None)
}

/// Shows a message with a single dismiss button.
pub fn show_message(text: &str) {
    let escaped = text.replace('\\', "").replace('"', "'");
    applescript(&format!(
        "display dialog \"{escaped}\" with title \"Oyente\" buttons {{\"Cerrar\"}} \
         default button \"Cerrar\""
    ));
}

/// Asks a yes/no question. True when the affirmative button was pressed.
///
/// Blocks until answered, so it must not be called from the recognition
/// thread — a dialog waiting for a click would stop everything being heard.
pub fn ask(text: &str, affirmative: &str) -> bool {
    let escaped = text.replace('\\', "").replace('"', "'");
    let script = format!(
        "display dialog \"{escaped}\" with title \"Oyente\" \
         buttons {{\"Cancelar\", \"{affirmative}\"}} default button \"{affirmative}\""
    );
    Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .is_ok_and(|out| {
            String::from_utf8_lossy(&out.stdout).contains(&format!("button returned:{affirmative}"))
        })
}

/// Shows a file in the Finder.
pub fn reveal(path: &str) {
    let _ = Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
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

// Accessibility is granted per binary by macOS, and the only honest way to
// ask about it is AXIsProcessTrusted. Creating an event source succeeds
// either way — the events are simply dropped on the way out — so checking
// that proves nothing.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
}

/// Whether macOS will actually deliver the key events we post.
///
/// Without this permission `press` still returns true and nothing happens:
/// the events are created and then discarded by the window server. It is
/// the quietest failure in the system, so it is worth reporting loudly at
/// startup rather than leaving someone wondering why ⌘W does nothing.
pub fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Asks macOS to prompt for Accessibility, and reports whether it is held.
///
/// This is the part that matters: merely opening the settings pane leaves
/// the user hunting for an app that is not in the list yet. Calling
/// `AXIsProcessTrustedWithOptions` with the prompt option makes macOS show
/// its own dialog and register the app, so there is a switch to turn on.
///
/// The prompt appears only once per app per login session; afterwards this
/// behaves like [`has_accessibility_permission`].
pub fn request_accessibility_permission() -> bool {
    // The constant's value is this string; using it directly avoids linking
    // against the exported symbol.
    let prompt_key = CFString::from_static_string("AXTrustedCheckOptionPrompt");
    let options = CFDictionary::from_CFType_pairs(&[(
        prompt_key.as_CFType(),
        CFBoolean::true_value().as_CFType(),
    )]);
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
}

/// Opens the Accessibility pane of System Settings.
pub fn open_accessibility_settings() -> bool {
    applescript(
        "open location \"x-apple.systempreferences:com.apple.preference.security\
         ?Privacy_Accessibility\"",
    )
}

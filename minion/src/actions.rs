//! What the machine does once a command is understood.
//!
//! Three routes, in order of preference:
//!   - [`open_app`] to launch or focus applications
//!   - [`press`] to send key combinations
//!   - [`applescript`] for what neither covers (volume, media transport)

use std::process::Command;

use core_foundation::base::TCFType;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSWorkspace};
use objc2_foundation::{NSOperationQueue, NSString};
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
    pub const DIGIT_0: u16 = 29;
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
    /// The physical key that reads "=" unshifted and "+" shifted — used
    /// unshifted for a browser's "zoom in", which binds to ⌘= rather than
    /// ⌘+ even though the menu shows a plus sign.
    pub const EQUALS: u16 = 24;
    pub const MINUS: u16 = 27;
}

/// Parses a shortcut such as "cmd-shift-b" into a key and its modifiers.
///
/// Accepts the names people actually write: cmd or command, alt or option,
/// ctrl or control. The key itself comes last.
pub fn parse_shortcut(text: &str) -> Option<(u16, Mods)> {
    let mut mods = Mods::NONE;
    let mut code = None;

    for part in text.split(['-', '+']).map(str::trim) {
        match part.to_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" => mods.command = true,
            "shift" => mods.shift = true,
            "alt" | "option" | "opt" => mods.option = true,
            "ctrl" | "control" => mods.control = true,
            name => code = key_named(name),
        }
    }
    code.map(|code| (code, mods))
}

/// The name of a key, for writing a shortcut back out.
pub fn name_of_key(code: u16) -> Option<&'static str> {
    NAMED
        .iter()
        .find(|(_, candidate)| *candidate == code)
        .map(|(name, _)| *name)
}

/// Writes a shortcut the way it is read back in.
pub fn shortcut_text(code: u16, mods: Mods) -> Option<String> {
    let key = name_of_key(code)?;
    let mut parts = Vec::new();
    if mods.control {
        parts.push("ctrl");
    }
    if mods.option {
        parts.push("alt");
    }
    if mods.shift {
        parts.push("shift");
    }
    if mods.command {
        parts.push("cmd");
    }
    parts.push(key);
    Some(parts.join("-"))
}

/// The virtual key code for a key's spoken or written name.
fn key_named(name: &str) -> Option<u16> {
    NAMED
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, code)| *code)
}

/// Letters and digits sit where a US layout puts them, which is also where
/// a Spanish ISO keyboard puts them for these purposes.
const NAMED: &[(&str, u16)] = &[
        ("a", key::A), ("b", key::B), ("c", key::C), ("d", 2), ("e", key::E),
        ("f", key::F), ("g", 5), ("h", key::H), ("i", key::I), ("j", 38),
        ("k", 40), ("l", key::L), ("m", key::M), ("n", key::N), ("o", 31),
        ("p", 35), ("q", key::Q), ("r", key::R), ("s", key::S), ("t", key::T),
        ("u", 32), ("v", key::V), ("w", key::W), ("x", key::X), ("y", key::Y),
        ("z", key::Z),
        ("0", key::DIGIT_0), ("1", key::DIGIT_1), ("2", key::DIGIT_2),
        ("3", key::DIGIT_3), ("4", key::DIGIT_4), ("5", key::DIGIT_5),
        ("6", 22), ("7", 26), ("8", 28), ("9", key::DIGIT_9),
        ("tab", key::TAB), ("space", key::SPACE), ("escape", key::ESCAPE),
        ("esc", key::ESCAPE), ("delete", key::DELETE), ("backspace", key::DELETE),
        ("return", 36), ("enter", 36),
        ("left", key::LEFT), ("right", key::RIGHT),
        ("up", key::UP), ("down", key::DOWN),
        ("f1", 122), ("f2", 120), ("f3", 99), ("f4", 118), ("f5", 96),
        ("f6", 97), ("f7", 98), ("f8", 100), ("f9", 101), ("f10", 109),
    ("f11", 103), ("f12", 111),
    ("equals", key::EQUALS), ("minus", key::MINUS),
];

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
pub fn press(code: u16, mods: Mods) -> Result<(), String> {
    // Checked here rather than only at startup: the permission can be
    // granted while Minion is running and takes effect immediately, so a
    // one-off check at launch goes stale the moment the user turns it on.
    if !has_accessibility_permission() {
        return Err("Accessibility not granted".to_string());
    }
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|()| "could not create an event source".to_string())?;
    let flags = mods.flags();

    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.clone(), code, down)
            .map_err(|()| "could not create a keyboard event".to_string())?;
        event.set_flags(flags);
        event.post(CGEventTapLocation::HID);
    }
    Ok(())
}

/// Launches the application, or brings it forward if already running.
///
/// `open -b` covers all three cases at once: not running (launches it),
/// running with no windows (asks for one), and running with a window
/// (activates it). The middle case is common on macOS, where closing the
/// last window does not quit the app — and where `focus()` alone would
/// swap the menu bar while showing nothing.
pub fn open_app(bundle_id: &str) -> Result<(), String> {
    run(Command::new("/usr/bin/open").arg("-b").arg(bundle_id))
}

/// Asks an application to quit.
///
/// A polite quit, not a kill: if there is unsaved work the app puts up its
/// own save dialog, exactly as ⌘Q would. Nothing is lost without being
/// asked about first.
pub fn quit_app(bundle_id: &str) -> Result<(), String> {
    applescript(&format!(
        "tell application id \"{}\" to quit",
        applescript_string(bundle_id)
    ))
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
pub fn type_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if !has_accessibility_permission() {
        return Err("Accessibility not granted".to_string());
    }
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|()| "could not create an event source".to_string())?;

    const CHUNK_CHARS: usize = 20;
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(CHUNK_CHARS) {
        let piece: String = chunk.iter().collect();
        let event = CGEvent::new_keyboard_event(source.clone(), 0, true)
            .map_err(|()| "could not create a keyboard event".to_string())?;
        event.set_string(&piece);
        event.post(CGEventTapLocation::HID);
        // A short gap keeps the receiving app from dropping characters.
        std::thread::sleep(std::time::Duration::from_millis(6));
    }
    Ok(())
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
pub fn open_url(url: &str, browser_bundle_id: Option<&str>) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/open");
    if let Some(bundle_id) = browser_bundle_id {
        command.arg("-b").arg(bundle_id);
    }
    command.arg(url);
    run(&mut command)
}

/// Opens a search in Spotify.
///
/// Searching rather than playing: starting a specific track needs the Web
/// API and an OAuth token, which is a different project. This lands on the
/// results with the app in front, one click from playing.
pub fn search_spotify(query: &str) -> Result<(), String> {
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
///
/// Queued on the main thread instead of shown where the caller stands:
/// most callers are worker threads, and AppKit belongs to the main one.
/// The caller does not wait, which is what it did when this was a spawned
/// `osascript`.
pub fn show_message(text: &str) {
    let text = text.to_string();
    let work = block2::RcBlock::new(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            alert(mtm, &text, None);
        }
    });
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&work) };
}

/// Shows a message and returns only once it has been dismissed.
///
/// For the moments before the process exits: a message merely queued on
/// the main thread would be lost to `exit`. From the main thread the alert
/// runs in place; from any other it is queued and this thread waits for the
/// click, which needs the main run loop to be going — true wherever this
/// is called, once the menu bar is up.
pub fn show_message_and_wait(text: &str) {
    if let Some(mtm) = MainThreadMarker::new() {
        alert(mtm, text, None);
        return;
    }
    let (done, dismissed) = std::sync::mpsc::channel();
    let text = text.to_string();
    let work = block2::RcBlock::new(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            alert(mtm, &text, None);
        }
        let _ = done.send(());
    });
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&work) };
    let _ = dismissed.recv();
}

/// Asks a yes/no question. True when the affirmative button was pressed.
///
/// Runs where it is called, so it has to be called from the main thread —
/// which is where it is used, from the run loop timer. Off the main thread
/// there is no honest answer to give, so it says no rather than blocking
/// the recognition loop behind a dialog nobody can see.
pub fn ask(text: &str, affirmative: &str) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        crate::journal::write("a question was asked off the main thread; answering no");
        return false;
    };
    alert(mtm, text, Some(affirmative))
}

/// Puts up an alert and waits for it. True if the first button was used.
///
/// An `NSAlert` rather than AppleScript's `display dialog`: the script had
/// to have its quotes and backslashes filed off the message on the way in,
/// so what the user read was not quite what the program meant to say.
fn alert(mtm: MainThreadMarker, text: &str, affirmative: Option<&str>) -> bool {
    // Minion is an accessory application and never the active one, so
    // without this the alert opens behind whatever is in front.
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Minion"));
    alert.setInformativeText(&NSString::from_str(text));
    match affirmative {
        Some(yes) => {
            // The first button added is the default one, and the one whose
            // return code is `NSAlertFirstButtonReturn`.
            alert.addButtonWithTitle(&NSString::from_str(yes));
            alert.addButtonWithTitle(&NSString::from_str("Cancelar"));
        }
        None => {
            alert.addButtonWithTitle(&NSString::from_str("Cerrar"));
        }
    }
    alert.runModal() == NSAlertFirstButtonReturn
}

/// Shows a file in the Finder.
pub fn reveal(path: &str) -> Result<(), String> {
    Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Runs an AppleScript snippet.
pub fn applescript(script: &str) -> Result<(), String> {
    run(Command::new("/usr/bin/osascript").arg("-e").arg(script))
}

/// Escapes a string for interpolation into an AppleScript string literal
/// (inside the `"..."` quotes).
///
/// AppleScript has no other escape mechanism worth using here: a `"` ends
/// the literal early and a `\` starts an escape, so both have to be
/// backslash-escaped before a config value (a bundle id, say) is spliced
/// into a script — otherwise a value containing one breaks the script, or
/// worse, runs something the value never meant to say.
pub fn applescript_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Adjusts system output volume by `delta` points on a 0-100 scale.
pub fn adjust_volume(delta: i32) -> Result<(), String> {
    applescript(&format!(
        "set v to output volume of (get volume settings)\n\
         set volume output volume (v + {delta})"
    ))
}

pub fn set_muted(muted: bool) -> Result<(), String> {
    applescript(&format!("set volume output muted {muted}"))
}

/// Plays a short system sound as feedback.
pub fn play_sound(path: &str) -> Result<(), String> {
    Command::new("/usr/bin/afplay")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Runs a command to completion and turns a non-zero exit into the reason
/// why, taken from its stderr (or the exit status itself, when the process
/// said nothing about why it failed).
fn run(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        Err(format!("exited with {}", output.status))
    } else {
        Err(stderr)
    }
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
pub fn open_accessibility_settings() -> Result<(), String> {
    applescript(
        "open location \"x-apple.systempreferences:com.apple.preference.security\
         ?Privacy_Accessibility\"",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_shortcuts_the_way_it_reads_them() {
        // Round trip: what the capture writes must parse back the same.
        for text in ["cmd-s", "ctrl-alt-shift-cmd-b", "alt-space", "f5"] {
            let (code, mods) = parse_shortcut(text).expect("should parse");
            let written = shortcut_text(code, mods).expect("should write");
            assert_eq!(
                parse_shortcut(&written),
                Some((code, mods)),
                "«{text}» became «{written}»"
            );
        }
    }

    #[test]
    fn reads_shortcuts_as_written() {
        assert_eq!(parse_shortcut("cmd-s"), Some((key::S, Mods::CMD)));
        assert_eq!(parse_shortcut("cmd-shift-b"), {
            let mods = Mods { command: true, shift: true, ..Mods::NONE };
            Some((key::B, mods))
        });
        assert_eq!(parse_shortcut("command+option+left"), {
            let mods = Mods { command: true, option: true, ..Mods::NONE };
            Some((key::LEFT, mods))
        });
        assert_eq!(parse_shortcut("f5"), Some((96, Mods::NONE)));
    }

    #[test]
    fn reads_the_zoom_shortcuts_a_browser_binds() {
        assert_eq!(parse_shortcut("cmd-equals"), Some((key::EQUALS, Mods::CMD)));
        assert_eq!(parse_shortcut("cmd-minus"), Some((key::MINUS, Mods::CMD)));
    }

    #[test]
    fn rejects_a_shortcut_with_no_key() {
        assert_eq!(parse_shortcut("cmd-shift"), None);
        assert_eq!(parse_shortcut("nonsense"), None);
    }

    #[test]
    fn escapes_quotes_and_backslashes_for_applescript() {
        assert_eq!(applescript_string("com.apple.Safari"), "com.apple.Safari");
        assert_eq!(applescript_string(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(applescript_string(r"C:\path"), r"C:\\path");
        // A value crafted to break out of the string and add a command:
        // the quote must come through escaped, not close the literal.
        assert_eq!(
            applescript_string(r#"x" to quit application "Finder"#),
            r#"x\" to quit application \"Finder"#
        );
    }
}

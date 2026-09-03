//! AppleScript for what a keystroke cannot reach: window geometry, system
//! toggles, clipboard housekeeping.
//!
//! Every command here ends up as `Action::Script` in [`crate::vocabulary`],
//! so everything below is a plain, static `&str` — no runtime
//! interpolation, because [`crate::vocabulary::NAMED_ACTIONS`] is a `const`
//! array and cannot call anything that allocates. The AppleScript itself
//! does the runtime work instead (finding the frontmost window, its
//! screen, and so on), which is why several of these scripts look busier
//! than a one-liner: the busy part had to move from Rust into the script.
//!
//! What was left out, and why:
//!
//!   - **Bluetooth**: only `blueutil` can toggle it from the command line,
//!     and it is a third-party tool nothing here can assume is installed.
//!     Shipping a command that silently fails on every machine without it
//!     is worse than not shipping it.
//!   - **Do Not Disturb**: as of macOS 13 it lives in Control Centre, a
//!     menu-bar popover with no stable accessibility identifiers to drive
//!     from System Events, and `shortcuts run` needs a Focus shortcut the
//!     user would have to create by hand first. Neither is something a
//!     phrase can rely on working, so it was left out rather than shipped
//!     half-working.
//!
//! Brightness uses `key code 144`/`145` through System Events rather than
//! posting a real `NX_KEYTYPE_BRIGHTNESS` system-defined event: building
//! one of those from Rust needs constructing an `NSEvent` of the private
//! `NSSystemDefined` subtype, which is a lot of unsafe surface for two
//! keys. The `key code` route is the one commonly used for exactly this in
//! AppleScript, and is what the brief for this task suggested trying
//! first.
//!
//! Wi-Fi assumes the interface is `en0`, which is the Wi-Fi interface on
//! every Mac this was written against. A machine where `networksetup
//! -listallhardwareports` disagrees needs a different interface name; this
//! module has no way to discover it at compile time.

/// Snaps the frontmost window to the left half of the screen it is on.
pub const WINDOW_LEFT: &str = window_half_script(true);
/// Snaps the frontmost window to the right half of the screen it is on.
pub const WINDOW_RIGHT: &str = window_half_script(false);

const fn window_half_script(left: bool) -> &'static str {
    if left {
        "tell application \"System Events\"\n\
         \tset frontProcess to first application process whose frontmost is true\n\
         \tset frontWindow to front window of frontProcess\n\
         \tset {windowX, windowY} to position of frontWindow\n\
         \tset screenBounds to bounds of desktop 1\n\
         \trepeat with aDesktop in desktops\n\
         \t\tset db to bounds of aDesktop\n\
         \t\tif windowX ≥ (item 1 of db) and windowX < (item 3 of db) then\n\
         \t\t\tset screenBounds to db\n\
         \t\t\texit repeat\n\
         \t\tend if\n\
         \tend repeat\n\
         \tset {screenLeft, screenTop, screenRight, screenBottom} to screenBounds\n\
         \tset halfWidth to (screenRight - screenLeft) / 2\n\
         \tset position of frontWindow to {screenLeft, screenTop}\n\
         \tset size of frontWindow to {halfWidth, screenBottom - screenTop}\n\
         end tell"
    } else {
        "tell application \"System Events\"\n\
         \tset frontProcess to first application process whose frontmost is true\n\
         \tset frontWindow to front window of frontProcess\n\
         \tset {windowX, windowY} to position of frontWindow\n\
         \tset screenBounds to bounds of desktop 1\n\
         \trepeat with aDesktop in desktops\n\
         \t\tset db to bounds of aDesktop\n\
         \t\tif windowX ≥ (item 1 of db) and windowX < (item 3 of db) then\n\
         \t\t\tset screenBounds to db\n\
         \t\t\texit repeat\n\
         \t\tend if\n\
         \tend repeat\n\
         \tset {screenLeft, screenTop, screenRight, screenBottom} to screenBounds\n\
         \tset halfWidth to (screenRight - screenLeft) / 2\n\
         \tset position of frontWindow to {screenLeft + halfWidth, screenTop}\n\
         \tset size of frontWindow to {halfWidth, screenBottom - screenTop}\n\
         end tell"
    }
}

/// Zooms the frontmost window — the same thing clicking the green button
/// does — rather than the full-screen mode `pantalla completa` already
/// covers.
pub const WINDOW_MAXIMIZE: &str = "tell application \"System Events\"\n\
     \tset frontProcess to first application process whose frontmost is true\n\
     \tset zoomed of front window of frontProcess to true\n\
     end tell";

/// Moves the frontmost window to whichever other display's desktop bounds
/// do not match the one it is currently on. A no-op, safely, when there is
/// only one display: the repeat finds nothing that differs and the window
/// stays put.
pub const WINDOW_OTHER_SCREEN: &str = "tell application \"System Events\"\n\
     \tset frontProcess to first application process whose frontmost is true\n\
     \tset frontWindow to front window of frontProcess\n\
     \tset {windowX, windowY} to position of frontWindow\n\
     \tset allDesktops to desktops\n\
     \tset homeBounds to bounds of item 1 of allDesktops\n\
     \trepeat with aDesktop in allDesktops\n\
     \t\tset db to bounds of aDesktop\n\
     \t\tif windowX ≥ (item 1 of db) and windowX < (item 3 of db) then\n\
     \t\t\tset homeBounds to db\n\
     \t\t\texit repeat\n\
     \t\tend if\n\
     \tend repeat\n\
     \trepeat with aDesktop in allDesktops\n\
     \t\tset db to bounds of aDesktop\n\
     \t\tif db is not homeBounds then\n\
     \t\t\tset position of frontWindow to {item 1 of db, item 2 of db}\n\
     \t\t\texit repeat\n\
     \t\tend if\n\
     \tend repeat\n\
     end tell";

/// See the module note on brightness for why this is `key code` and not a
/// posted `NX_KEYTYPE_BRIGHTNESS` event.
pub const BRIGHTNESS_UP: &str = "tell application \"System Events\" to key code 144";
pub const BRIGHTNESS_DOWN: &str = "tell application \"System Events\" to key code 145";

/// Flips System Appearance between light and dark, as suggested in the
/// brief for this task.
pub const DARK_MODE_TOGGLE: &str = "tell application \"System Events\" \
     to tell appearance preferences to set dark mode to not dark mode";

/// Assumes the Wi-Fi interface is `en0`; see the module note above.
pub const WIFI_ON: &str = "do shell script \"/usr/sbin/networksetup -setairportpower en0 on\"";
pub const WIFI_OFF: &str = "do shell script \"/usr/sbin/networksetup -setairportpower en0 off\"";

/// Empties the Trash through Finder, which shows Finder's own confirmation
/// dialog as long as "Warn before emptying the Trash" is on — the default,
/// and left alone here on purpose: no destructive action from a single
/// phrase.
pub const EMPTY_TRASH: &str = "tell application \"Finder\" to empty trash";

pub const SLEEP_DISPLAY: &str = "do shell script \"/usr/bin/pmset displaysleepnow\"";

/// Interactive window capture: click the window to shoot, same as
/// ⇧⌘4-then-space from the keyboard.
pub const SCREENSHOT_WINDOW: &str = "do shell script \"/usr/sbin/screencapture -i -w\"";

pub const CLIPBOARD_CLEAR: &str = "do shell script \"/usr/bin/pbcopy < /dev/null\"";

/// Selects the address bar, copies it, and gets out of the address bar —
/// three keystrokes in a row, which is why this is a script and not a
/// single `keys` entry. Chrome and Safari both bind ⌘L to "select address
/// bar", so this works in either without needing to know which is in
/// front.
pub const COPY_URL: &str = "tell application \"System Events\"\n\
     \tkeystroke \"l\" using command down\n\
     \tdelay 0.1\n\
     \tkeystroke \"c\" using command down\n\
     \tdelay 0.1\n\
     \tkey code 53\n\
     end tell";

/// Neither browser has a default keyboard shortcut for this, so it goes
/// through each one's own scripting dictionary instead.
pub const DUPLICATE_TAB_SAFARI: &str = "tell application \"Safari\"\n\
     \tset newTab to make new tab at end of tabs of front window \
     with properties {URL:URL of current tab of front window}\n\
     \tset current tab of front window to newTab\n\
     end tell";

pub const DUPLICATE_TAB_CHROME: &str = "tell application \"Google Chrome\"\n\
     \ttell front window\n\
     \t\tmake new tab with properties {URL:URL of active tab}\n\
     \t\tset active tab index to (count of tabs)\n\
     \tend tell\n\
     end tell";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_scripts_move_the_front_window_of_the_frontmost_process() {
        for script in [WINDOW_LEFT, WINDOW_RIGHT, WINDOW_MAXIMIZE, WINDOW_OTHER_SCREEN] {
            assert!(script.contains("front window"), "should act on the front window");
            assert!(script.contains("frontmost is true"), "should find the frontmost process");
        }
    }

    #[test]
    fn left_and_right_put_the_window_on_opposite_sides() {
        assert!(WINDOW_LEFT.contains("position of frontWindow to {screenLeft, screenTop}"));
        assert!(WINDOW_RIGHT
            .contains("position of frontWindow to {screenLeft + halfWidth, screenTop}"));
        // Both compute the same half-width, so only where they place it differs.
        assert!(WINDOW_LEFT.contains("set halfWidth to (screenRight - screenLeft) / 2"));
        assert!(WINDOW_RIGHT.contains("set halfWidth to (screenRight - screenLeft) / 2"));
    }

    #[test]
    fn maximize_zooms_rather_than_full_screens() {
        assert!(WINDOW_MAXIMIZE.contains("set zoomed of front window"));
    }

    #[test]
    fn brightness_uses_the_documented_key_codes() {
        assert!(BRIGHTNESS_UP.contains("key code 144"));
        assert!(BRIGHTNESS_DOWN.contains("key code 145"));
    }

    #[test]
    fn dark_mode_toggles_rather_than_forcing_a_state() {
        assert!(DARK_MODE_TOGGLE.contains("not dark mode"));
    }

    #[test]
    fn wifi_scripts_name_the_assumed_interface() {
        assert!(WIFI_ON.contains("en0") && WIFI_ON.contains(" on\""));
        assert!(WIFI_OFF.contains("en0") && WIFI_OFF.contains(" off\""));
    }

    #[test]
    fn empty_trash_goes_through_finder_so_its_confirmation_still_shows() {
        assert_eq!(EMPTY_TRASH, "tell application \"Finder\" to empty trash");
    }

    #[test]
    fn sleep_display_uses_pmset_not_a_keystroke() {
        assert!(SLEEP_DISPLAY.contains("pmset displaysleepnow"));
    }

    #[test]
    fn window_screenshot_is_interactive_so_it_never_fires_blind() {
        assert!(SCREENSHOT_WINDOW.contains("screencapture -i -w"));
    }

    #[test]
    fn clipboard_clear_empties_rather_than_replacing_with_something() {
        assert!(CLIPBOARD_CLEAR.contains("pbcopy < /dev/null"));
    }

    #[test]
    fn copy_url_selects_copies_then_gets_out_of_the_address_bar() {
        let l = COPY_URL.find("\"l\"").expect("selects the address bar");
        let c = COPY_URL.find("\"c\"").expect("copies it");
        let esc = COPY_URL.find("key code 53").expect("presses escape");
        assert!(l < c && c < esc, "the three steps happen in order");
    }

    #[test]
    fn duplicate_tab_scripts_target_their_own_browser() {
        assert!(DUPLICATE_TAB_SAFARI.contains("tell application \"Safari\""));
        assert!(DUPLICATE_TAB_CHROME.contains("tell application \"Google Chrome\""));
    }
}

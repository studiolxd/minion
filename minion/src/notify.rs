//! Notification Center banners.
//!
//! The real API for this — `UNUserNotificationCenter` — needs the app to be
//! signed with a notification entitlement and, in practice, distributed
//! through a mechanism Apple recognises; a locally signed development build
//! is refused. `osascript -e 'display notification'` has none of that: any
//! signed app can ask System Events to post one on its behalf, it has
//! worked unchanged since Yosemite, and it is exactly the same pragmatic
//! choice already made for `actions::show_message`. The trade-off is a
//! banner that says "Script Editor" or "osascript" rather than "Minion" —
//! acceptable for a tool that already trades the Dock and window chrome of
//! a normal app for a menu-bar icon.
//!
//! Turn off with `notifications = false` in `config.toml`; on by default,
//! since a paused Minion that finishes a timer with nothing on screen and
//! `speak = false` would otherwise have no way to say so at all.

use std::process::Command;

/// Posts a notification, titled and worded in Spanish like everything else
/// Minion says. Best-effort: nothing here is worth interrupting a command
/// over, so a failure is silent — same as `actions::show_message`.
pub fn post(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {}",
        crate::actions::applescript_string(body),
        crate::actions::applescript_string(title)
    );
    let _ = Command::new("/usr/bin/osascript").arg("-e").arg(script).status();
}

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_and_backslashes_are_escaped_before_reaching_the_script() {
        // Not a test of osascript itself — never run in a test, or every
        // `cargo test` would pop up a real banner — just that a title or
        // body with a quote in it cannot break out of the AppleScript
        // string literal. See actions::applescript_string, which this
        // borrows.
        let escaped = crate::actions::applescript_string(r#"Han pasado "cinco" minutos"#);
        assert_eq!(escaped, r#"Han pasado \"cinco\" minutos"#);
    }
}

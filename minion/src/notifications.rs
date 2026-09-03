//! Reading back what Notification Center has already shown.
//!
//! There are two ways to reach a notification after the banner is gone,
//! and only one of them works:
//!
//!   * **through the Accessibility API**, by walking the Notification
//!     Center process's own windows. This only sees banners that are on
//!     screen *right now*, which is exactly the case where nobody needs
//!     to ask — «lee la última notificación» is said after the banner has
//!     gone, not while it is up. Measured on this machine: with no banner
//!     showing, the process has no windows to walk;
//!   * **through the database Notification Center keeps**, at
//!     `~/Library/Group Containers/group.com.apple.usernoted/db2/db`.
//!     That holds what was delivered, with the application, the title and
//!     the body, which is the whole of what is asked for.
//!
//! So it is the database, and the database is behind Full Disk Access —
//! a permission Minion does not otherwise need and must not take for
//! granted. Rather than failing with a shrug, [`spoken`] says what is
//! missing and offers to open the right pane of System Settings.
//!
//! Nothing here is written to, ever: the file is opened read-only through
//! `sqlite3`, and each record's payload — a binary property list — is
//! decoded by piping it through `plutil`. Both are in `/usr/bin` on every
//! Mac, which is cheaper than a SQLite crate and a plist crate for one
//! sentence of speech.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// One delivered notification, as much of it as is worth saying aloud.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notification {
    pub app: String,
    pub title: String,
    pub body: String,
}

/// Why there is nothing to read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The database exists but this process may not open it.
    NeedsFullDiskAccess,
    /// It could be opened and still made no sense.
    Unreadable(String),
}

/// What to say when the permission is missing, and what the dialog says.
const NEEDS_ACCESS: &str = "Minion necesita Acceso total al disco para leer las notificaciones.";
const SETTINGS_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// Raised when a spoken request found the permission missing, and lowered
/// by the run loop when it has offered to open System Settings.
///
/// A flag rather than a dialog on the spot: this is reached from the
/// listening thread, and `actions::ask_choice` can only answer honestly
/// on the main one. Same shape as `main.rs`'s other menu requests.
static ASK_FOR_ACCESS: AtomicBool = AtomicBool::new(false);

/// Where Notification Center keeps what it has delivered.
fn db_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty())?;
    Some(PathBuf::from(home).join("Library/Group Containers/group.com.apple.usernoted/db2/db"))
}

/// The last `count` notifications, newest first.
pub fn last(count: usize) -> Result<Vec<Notification>, Problem> {
    let path = db_path().ok_or(Problem::Unreadable("no HOME".to_string()))?;
    // The cheapest possible test of the permission: under TCC an
    // ordinary open fails with "operation not permitted" long before
    // sqlite3 would say anything more useful.
    if std::fs::File::open(&path).is_err() {
        return Err(Problem::NeedsFullDiskAccess);
    }
    let query = format!(
        "select coalesce(a.identifier, ''), hex(r.data) from record r \
         left join app a on a.app_id = r.app_id \
         order by r.delivered_date desc limit {count};"
    );
    let mut command = Command::new("/usr/bin/sqlite3");
    command.arg("-readonly").arg(&path).arg(query);
    let output = crate::files::run_bounded(&mut command, Duration::from_secs(5))
        .map_err(Problem::Unreadable)?;
    Ok(output.lines().filter_map(read_row).collect())
}

/// One `identifier|hex` row, as a notification.
fn read_row(line: &str) -> Option<Notification> {
    let (identifier, payload) = line.split_once('|')?;
    let mut notification = decode(payload).unwrap_or_default();
    notification.app = app_name(identifier);
    (!notification.title.is_empty() || !notification.body.is_empty()).then_some(notification)
}

/// Turns a record's payload — hex, of a binary property list — into the
/// fields worth saying.
fn decode(hex: &str) -> Option<Notification> {
    let bytes = from_hex(hex)?;
    let mut command = Command::new("/usr/bin/plutil");
    command.arg("-convert").arg("json").arg("-o").arg("-").arg("-");
    let json = crate::files::run_bounded_with_input(
        &mut command,
        Some(&bytes),
        Duration::from_secs(5),
    )
    .ok()?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    Some(fields_of(&value))
}

/// The title and body of a decoded record.
///
/// The request is nested under `req`, and its keys are abbreviated the way
/// Notification Center abbreviates them: `titl`, `subt`, `body`. A
/// subtitle is part of the title as far as anybody listening is
/// concerned — "Ana Pérez" under "Mail" is what makes the notification
/// mean anything.
fn fields_of(value: &serde_json::Value) -> Notification {
    let request = value.get("req").unwrap_or(value);
    let text = |key: &str| {
        request.get(key).and_then(|value| value.as_str()).unwrap_or_default().trim().to_string()
    };
    let title = match (text("titl"), text("subt")) {
        (title, subtitle) if subtitle.is_empty() => title,
        (title, subtitle) if title.is_empty() => subtitle,
        (title, subtitle) => format!("{title}, {subtitle}"),
    };
    Notification { app: String::new(), title, body: text("body") }
}

/// Bytes from the hex `sqlite3` prints for a blob.
fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.trim();
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).ok())
        .collect()
}

/// The applications whose bundle identifier says nothing about what they
/// are called out loud. Everything else is named after the last part of
/// its identifier, which is right far more often than it is wrong.
const KNOWN_APPS: &[(&str, &str)] = &[
    ("com.apple.mail", "Mail"),
    ("com.apple.MobileSMS", "Mensajes"),
    ("com.apple.iCal", "Calendario"),
    ("com.apple.reminders", "Recordatorios"),
    ("com.apple.facetime", "FaceTime"),
    ("com.apple.finder", "Finder"),
    ("com.apple.Music", "Música"),
    ("com.apple.systempreferences", "Ajustes"),
    ("com.microsoft.teams2", "Teams"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.hnc.Discord", "Discord"),
    ("net.whatsapp.WhatsApp", "WhatsApp"),
];

/// What to call the application a notification came from.
fn app_name(bundle_id: &str) -> String {
    if let Some((_, name)) = KNOWN_APPS.iter().find(|(id, _)| *id == bundle_id) {
        return (*name).to_string();
    }
    bundle_id.rsplit('.').next().unwrap_or(bundle_id).to_string()
}

/// How much of a notification's body is worth reading aloud.
const MAX_BODY: usize = 140;

/// What to say for a list of notifications.
pub fn spoken_list(notifications: &[Notification]) -> String {
    if notifications.is_empty() {
        return "No hay notificaciones.".to_string();
    }
    notifications
        .iter()
        .map(|notification| {
            let mut said = notification.app.clone();
            if !notification.title.is_empty() {
                said = format!("{said}: {}", notification.title);
            }
            if !notification.body.is_empty() {
                let body: String = notification.body.chars().take(MAX_BODY).collect();
                said = format!("{said}. {body}");
            }
            said.trim_end_matches('.').to_string()
        })
        .collect::<Vec<_>>()
        .join(". ")
        + "."
}

/// «lee la última notificación», «¿qué ha llegado?» — the answer, aloud.
pub fn spoken(count: usize) -> String {
    match last(count) {
        Ok(notifications) => spoken_list(&notifications),
        Err(Problem::NeedsFullDiskAccess) => {
            ASK_FOR_ACCESS.store(true, Ordering::Relaxed);
            crate::journal::write("notif    no Full Disk Access; cannot read notifications");
            NEEDS_ACCESS.to_string()
        }
        Err(Problem::Unreadable(reason)) => {
            crate::journal::write(&format!("notif    unreadable: {reason}"));
            "No he podido leer las notificaciones.".to_string()
        }
    }
}

/// Offers to open the Full Disk Access pane, if a spoken request has just
/// found the permission missing. Called from the run loop, which is the
/// only place a dialog can be answered.
pub fn offer_disk_access_if_asked() {
    if !ASK_FOR_ACCESS.swap(false, Ordering::Relaxed) {
        return;
    }
    if crate::actions::ask(&format!("{NEEDS_ACCESS}\n\nActívalo en Privacidad y seguridad → Acceso total al disco."), "Abrir Ajustes") {
        let _ = crate::actions::open_url(SETTINGS_PANE, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_becomes_bytes() {
        assert_eq!(from_hex("48656C6C6F"), Some(b"Hello".to_vec()));
        assert_eq!(from_hex(""), None);
        assert_eq!(from_hex("ABC"), None, "an odd number of digits is not a blob");
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn a_record_gives_up_its_title_and_body() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"req":{"titl":"Ana","body":"¿Comemos?"}}"#).unwrap();
        assert_eq!(
            fields_of(&value),
            Notification { app: String::new(), title: "Ana".into(), body: "¿Comemos?".into() }
        );
    }

    #[test]
    fn a_subtitle_is_part_of_the_title() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"req":{"titl":"Mail","subt":"Ana","body":"Mañana"}}"#).unwrap();
        assert_eq!(fields_of(&value).title, "Mail, Ana");
    }

    #[test]
    fn a_record_with_nothing_to_say_is_dropped() {
        let empty = serde_json::json!({"req": {}});
        assert_eq!(fields_of(&empty), Notification::default());
        assert_eq!(read_row("com.apple.mail|"), None, "no payload, nothing to read");
        assert_eq!(read_row("no separator here"), None);
    }

    #[test]
    fn applications_are_named_the_way_they_are_said() {
        assert_eq!(app_name("com.apple.mail"), "Mail");
        assert_eq!(app_name("com.apple.MobileSMS"), "Mensajes");
        assert_eq!(app_name("com.spotify.client"), "client");
        assert_eq!(app_name("Spotify"), "Spotify");
    }

    #[test]
    fn a_list_is_read_application_first() {
        let notifications = [
            Notification { app: "Mail".into(), title: "Ana".into(), body: "Mañana a las diez".into() },
            Notification { app: "Mensajes".into(), title: "Luis".into(), body: String::new() },
        ];
        assert_eq!(spoken_list(&notifications), "Mail: Ana. Mañana a las diez. Mensajes: Luis.");
    }

    #[test]
    fn nothing_delivered_is_said_as_much() {
        assert_eq!(spoken_list(&[]), "No hay notificaciones.");
    }

    #[test]
    fn a_long_body_is_cut_rather_than_read_out_whole() {
        let notification = Notification {
            app: "Slack".into(),
            title: "Equipo".into(),
            body: "a".repeat(400),
        };
        let said = spoken_list(std::slice::from_ref(&notification));
        assert!(said.chars().count() < 200, "read back {} characters", said.chars().count());
    }
}

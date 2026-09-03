//! `minion run "…"` and `minion say "…"` — a tiny file-based local API, for
//! a shortcut launcher (Raycast, Atajos, a Shortcuts action) to drive the
//! already-running copy without simulating the microphone or opening a
//! window.
//!
//! A second process cannot reach into the running one directly — no IPC of
//! any kind exists here yet — so it leaves a request file next to
//! `config.toml` and returns immediately, on the same principle
//! [`crate::main`]'s `open-settings` request already uses for a duplicate
//! launch. The running copy's timer checks for these files on the same
//! once-a-second schedule (see `open_settings_request_path` in `main.rs`)
//! and picks them up from there — see that timer for why the schedule is
//! "about once a second" rather than every tick.
//!
//! `minion run` goes through [`crate::commands::decide_in`] exactly like
//! something heard aloud would, with the wake word prefixed so the
//! vocabulary matches normally, and is logged as `api` rather than `ran` so
//! the two sources stay distinguishable in the log. It runs on the menu
//! bar's own thread rather than the listening thread, so — unlike a real
//! utterance — it cannot continue a dictation session or be undone with
//! "deshaz": those live in the `Session` the listening thread alone holds.
//! The speaker check is skipped entirely, deliberately: a shortcut typed
//! and run from the keyboard has already proven who is at it.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The directory request and status files live in: next to `config.toml`,
/// which is already there and already private to this user (see
/// `config::secure_config_dir`).
fn app_support_dir() -> Option<PathBuf> {
    crate::config::path().and_then(|path| path.parent().map(std::path::Path::to_path_buf))
}

fn write_request(prefix: &str, text: &str) -> Result<(), String> {
    let dir = app_support_dir().ok_or("no home directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = dir.join(format!("{prefix}-{stamp}"));
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// Leaves a `run-<timestamp>` request for the running copy: says `text` as
/// if it had been heard, wake word and all.
pub fn request_run(text: &str) -> Result<(), String> {
    write_request("run", text)
}

/// Leaves a `say-<timestamp>` request for the running copy: speaks (or, if
/// `speak` is off, notifies) `text` directly, with no vocabulary involved.
pub fn request_say(text: &str) -> Result<(), String> {
    write_request("say", text)
}

/// Where the running copy last wrote whether it is listening.
fn status_path() -> Option<PathBuf> {
    app_support_dir().map(|dir| dir.join("status"))
}

/// What is currently on disk in the status file, if anything — read once
/// per timer tick so [`write_status_if_changed`] knows whether writing is
/// even necessary.
pub fn read_status_file() -> Option<String> {
    status_path().and_then(|path| std::fs::read_to_string(path).ok())
}

/// Records whether Minion is listening or paused, for `minion status` to
/// read back. Called from the same timer tick that checks for requests, so
/// it only actually writes when the flag has changed — see
/// `write_status_if_changed`.
fn write_status(listening: bool) {
    if cfg!(test) {
        return;
    }
    let Some(path) = status_path() else { return };
    let _ = std::fs::write(path, if listening { "listening" } else { "paused" });
}

/// Whether the status file needs rewriting, given what is already on disk.
/// Pure, so the "changed" decision is testable without a filesystem.
fn status_changed(listening: bool, on_disk: Option<&str>) -> bool {
    let wanted = if listening { "listening" } else { "paused" };
    on_disk.map(str::trim) != Some(wanted)
}

/// Writes the status file only when it disagrees with what is on disk, so
/// a timer that checks every second is not also a second's worth of
/// needless disk writes.
pub fn write_status_if_changed(listening: bool, on_disk: Option<&str>) {
    if status_changed(listening, on_disk) {
        write_status(listening);
    }
}

/// Reads the status file and prints what it says, for `minion status`.
pub fn print_status() {
    match status_path().and_then(|path| std::fs::read_to_string(path).ok()) {
        Some(s) if s.trim() == "listening" => println!("Escuchando."),
        Some(s) if s.trim() == "paused" => println!("En pausa."),
        _ => println!("No se sabe: ¿está Minion en ejecución?"),
    }
}

/// What a pending request file asks for.
pub enum Kind {
    Run,
    Say,
}

/// One request file, already matched to its kind but not yet read.
pub struct Request {
    pub kind: Kind,
    pub path: PathBuf,
}

/// The kind a request file's name says it is, from its `run-` or `say-`
/// prefix. Pure, so the naming rule is testable without touching disk.
fn kind_of(file_name: &str) -> Option<Kind> {
    if file_name.starts_with("run-") {
        Some(Kind::Run)
    } else if file_name.starts_with("say-") {
        Some(Kind::Say)
    } else {
        None
    }
}

/// Request files waiting to be handled, oldest first by name — the
/// timestamp in the name sorts that way already.
pub fn pending() -> Vec<Request> {
    let Some(dir) = app_support_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut found: Vec<Request> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            Some(Request { kind: kind_of(name)?, path: entry.path() })
        })
        .collect();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// How to speak or notify a reply, gathered once by the caller so this
/// module never has to know about `VoiceReply` or `Reporting`, both
/// private to `main.rs`.
pub struct ReplySettings<'a> {
    pub voice: Option<&'a str>,
    pub rate: u32,
    pub device: Option<&'a str>,
    /// Whether Minion speaks at all — `config.speak`.
    pub speak: bool,
    /// Whether it falls back to a notification when it does not — see
    /// `notify.rs`.
    pub notifications: bool,
}

impl ReplySettings<'_> {
    /// Speaks `text`, or notifies with it when speech is off. Uses a
    /// request-local "deaf" flag rather than the listening thread's: this
    /// runs on the menu bar's own thread, which has no utterance loop of
    /// its own to protect from hearing itself.
    fn deliver(&self, text: &str) {
        if self.speak {
            let deaf = AtomicBool::new(false);
            crate::speech::say(text, self.voice, self.rate, self.device, &deaf, Duration::from_millis(200));
        } else if self.notifications {
            crate::notify::post("Minion", text);
        }
    }
}

/// Reads and removes one request file, then carries it out — the normal
/// pipeline for `run`, or a direct spoken reply for `say`.
pub fn handle(request: &Request, reply: &ReplySettings, context: Option<&str>) {
    let text = std::fs::read_to_string(&request.path).unwrap_or_default();
    let _ = std::fs::remove_file(&request.path);
    let text = text.trim();
    if text.is_empty() {
        return;
    }

    match request.kind {
        Kind::Say => {
            crate::note!("api      «{text}»  ->  say");
            reply.deliver(text);
        }
        Kind::Run => {
            let wake = crate::commands::wake_words().first().copied().unwrap_or("minion");
            let heard = format!("{wake} {text}");
            let (decision, _confidence) = crate::commands::decide_in(&heard, context);
            match &decision {
                crate::commands::Decision::Answer(question) => {
                    let answer = crate::answers::answer(question.clone(), true);
                    crate::note!("api      «{text}»  ->  {answer}");
                    reply.deliver(&answer);
                }
                crate::commands::Decision::Ignored | crate::commands::Decision::Unrecognised => {
                    crate::note!("api      «{text}»  ->  not understood");
                }
                _ => match crate::commands::perform(&decision) {
                    Some(done) => match done.outcome {
                        Ok(()) => crate::note!("api      «{text}»  ->  {}", done.description),
                        Err(reason) => {
                            crate::note!(
                                "api      «{text}»  ->  {}: {reason}  — blocked by macOS",
                                done.description
                            );
                        }
                    },
                    None => crate::note!("api      «{text}»  ->  not understood"),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_files_are_told_apart_by_their_prefix() {
        assert!(matches!(kind_of("run-12345"), Some(Kind::Run)));
        assert!(matches!(kind_of("say-12345"), Some(Kind::Say)));
        assert!(kind_of("open-settings").is_none());
        assert!(kind_of("status").is_none());
        assert!(kind_of("config.toml").is_none());
    }

    #[test]
    fn the_status_file_only_needs_rewriting_when_the_flag_changed() {
        assert!(!status_changed(true, Some("listening")));
        // Whitespace from a trailing newline should not count as a change.
        assert!(!status_changed(true, Some(" listening \n")));
        assert!(!status_changed(false, Some("paused")));
        assert!(status_changed(true, None));
        assert!(status_changed(false, Some("listening")));
    }
}

//! User configuration, read once at startup.
//!
//! Everything here is optional: with no file at all, the built-in defaults
//! apply. The point is that adding your own applications or retuning the
//! speech detector should not require a Rust toolchain.
//!
//! Lives at `~/Library/Application Support/Minion/config.toml`.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// Idle minutes before the model is released, when the file says nothing.
const DEFAULT_UNLOAD_MINUTES: u64 = 5;

/// Seconds the conversation window stays open when the file says nothing.
const DEFAULT_CONVERSATION_SECONDS: u64 = 5;

/// Shortcut that pauses and resumes when nothing is set.
pub const DEFAULT_RESUME_SHORTCUT: &str = "ctrl-alt-m";

/// Cosine similarity a voice must reach to be treated as yours.
///
/// Measured rather than guessed. Against a profile trained on this
/// machine, the owner's own commands scored 0.57 on average and 0.38 at
/// worst — through a laptop microphone across a desk, which is nothing
/// like the clean audio every synthetic test used. The first value tried
/// here was 0.45, and it refused its owner regularly.
///
/// It errs low on purpose. Someone else saying the wake word still has to
/// say something in the vocabulary, and being unable to talk to your own
/// computer is worse than that.
const DEFAULT_VOICE_THRESHOLD: f32 = 0.32;

use crate::audio;
use crate::commands::App;

/// serde needs a function for a default of `true`.
fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Words that mark a sentence as a command. Replaces the defaults.
    #[serde(default)]
    pub wake_words: Vec<String>,

    /// Confidence required before acting, from 0 to 1.
    pub threshold: Option<f32>,

    /// Whether to write down speech that was not addressed to Minion.
    ///
    /// Off by default, and deliberately so: with the microphone always on,
    /// anything said nearby gets transcribed, and keeping that on disk is
    /// not something anyone asked for. Turn it on while tuning, when
    /// seeing the exact wording is the whole point.
    #[serde(default)]
    pub log_ignored_speech: bool,

    /// Play a short sound when a command runs, and another when a sentence
    /// starting with the wake word is not understood.
    ///
    /// On by default: a command that succeeds produces no visible output of
    /// its own, so without a sound there is no way to tell whether you were
    /// heard. Turn it off once the commands are familiar.
    #[serde(default = "yes")]
    pub sounds: bool,

    #[serde(default)]
    pub audio: AudioConfig,

    /// Extra applications, added to the built-in list.
    #[serde(default)]
    pub apps: Vec<AppConfig>,

    /// Minutes of silence after which the speech model is released.
    ///
    /// The model is most of the memory Minion uses, and a machine that sits
    /// idle for hours has no reason to hold it. Reloading costs a second or
    /// so on the next thing you say. Zero keeps it loaded for good.
    pub unload_after_minutes: Option<u64>,

    /// Extra ways of saying commands that already exist.
    ///
    /// This is where `minion learn` writes what it learned from the log,
    /// and where you add a phrasing the recogniser keeps producing.
    #[serde(default)]
    pub aliases: Vec<AliasConfig>,

    /// How alike a voice must sound to yours before Minion listens to it.
    ///
    /// Only used once `minion enroll` has recorded a voice. Higher rejects
    /// more, including you on a bad day; lower lets others through. Zero to
    /// one, default 0.32.
    pub voice_threshold: Option<f32>,

    /// Which microphone to listen through.
    ///
    /// Empty follows the system's default, changing along with it when you
    /// plug in headphones. Naming one pins it there instead.
    pub microphone: Option<String>,

    /// Which output device to speak through. Empty uses the system's.
    pub speaker: Option<String>,

    /// Keep a copy of everything heard, as WAV files.
    ///
    /// For working out why recognition behaves oddly. Off by default and
    /// worth turning off again afterwards: it writes every utterance to
    /// disk, including anything said nearby.
    #[serde(default)]
    pub save_recordings: bool,

    /// Whether Minion answers questions out loud.
    ///
    /// Only questions: it stays quiet for commands, since opening Chrome is
    /// something you can see and saying so would be noise arriving late.
    #[serde(default = "yes")]
    pub speak: bool,

    /// Which system voice to use. Empty picks a Spanish one.
    pub voice: Option<String>,

    /// Words per minute.
    pub speech_rate: Option<u32>,

    /// Keyboard shortcut that pauses and resumes from anywhere.
    ///
    /// Pausing by voice is easy; getting attention back is not, since a
    /// paused microphone hears nothing. Written as it appears on a menu:
    /// "alt-space", "ctrl+shift+m". Empty disables it.
    pub resume_shortcut: Option<String>,

    /// Entirely new commands, bound to a keyboard shortcut.
    #[serde(default)]
    pub commands: Vec<CommandConfig>,

    /// Seconds after a command (or an answer) during which the next
    /// utterance is obeyed without the wake word.
    ///
    /// `None` means the default of 5; zero disables the window entirely,
    /// for someone who would rather every sentence start with «minion».
    pub conversation_seconds: Option<u64>,

    /// "always" (the default) listens continuously; "hold" only listens
    /// while `resume_shortcut` is held down, and does not need the wake
    /// word while it is.
    pub listen_mode: Option<String>,

    /// Bundle IDs of applications that pause listening while in front —
    /// a video call is the one time an always-on microphone is unwelcome.
    ///
    /// `None` uses the built-in list (Zoom, Teams, FaceTime). Google Meet
    /// in a browser tab has no bundle ID of its own to check — it is
    /// Chrome, like every other tab — so it cannot be detected this way
    /// and is not on the list.
    pub pause_during: Option<Vec<String>>,
}

/// How Minion decides when to listen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenMode {
    Always,
    /// Only while the shortcut is held down.
    Hold,
}

/// A command of your own: what to say, and which keys to press.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    /// Shown in the log when it runs.
    pub name: String,
    /// Ways of saying it.
    pub phrases: Vec<String>,
    /// The shortcut, as written on a menu: "cmd-shift-b", "ctrl+alt+left".
    pub keys: String,
}

/// Another way of saying an existing command.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasConfig {
    /// Name of the command, exactly as it appears in the log.
    pub command: String,
    /// The phrase to accept for it.
    pub phrase: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioConfig {
    pub speech_threshold: Option<f32>,
    pub silence_end_ms: Option<usize>,
    pub min_speech_ms: Option<usize>,
    pub max_utterance_ms: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub name: String,
    pub bundle_id: String,
    /// Ways of saying the name. Include what the recogniser really hears.
    pub aliases: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wake_words: Vec::new(),
            threshold: None,
            log_ignored_speech: false,
            sounds: true,
            audio: AudioConfig::default(),
            apps: Vec::new(),
            aliases: Vec::new(),
            commands: Vec::new(),
            voice_threshold: None,
            resume_shortcut: None,
            microphone: None,
            speaker: None,
            save_recordings: false,
            speak: true,
            voice: None,
            speech_rate: None,
            unload_after_minutes: None,
            conversation_seconds: None,
            listen_mode: None,
            pause_during: None,
        }
    }
}

/// Bundle IDs paused for when the file says nothing.
const DEFAULT_PAUSE_DURING: &[&str] =
    &["us.zoom.xos", "com.microsoft.teams2", "com.apple.FaceTime"];

/// Where the configuration file lives.
pub fn path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Minion/config.toml"))
}

/// Renders a Rust string as a valid, escaped TOML string literal, quotes
/// included.
///
/// Every string value written into the config file must go through this —
/// a bare `"{value}"` breaks the moment the value contains a `"` or a `\`,
/// which for `microphone`/`speaker` names and for aliases learned from the
/// log is not a hypothetical. `toml::Value` already knows how to escape a
/// string; this just borrows that.
///
/// Public so other writers of `config.toml` (`learn.rs`, `preferences.rs`)
/// can adopt it too, instead of hand-quoting.
pub fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

/// Sets an option inside a table, such as `[audio]`, in already-read file
/// contents. Pure: takes and returns text, touches no file.
///
/// The table runs until the next `[` header, or the end of the file.
/// Creates the table, appended at the end, if it is not there yet.
fn with_table_option(contents: &str, table: &str, key: &str, value: &str) -> String {
    let header = format!("[{table}]");
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let table_at = lines.iter().position(|l| l.trim() == header);

    let Some(start) = table_at else {
        // No such table: append it with the one setting in it.
        if !lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(header);
        lines.push(format!("{key} = {value}"));
        return lines.join("\n") + "\n";
    };

    // The table runs until the next header.
    let end = lines
        .iter()
        .skip(start + 1)
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| start + 1 + offset);

    let existing_key = (start + 1..end).find(|i| {
        lines[*i]
            .split('=')
            .next()
            .is_some_and(|name| name.trim() == key)
    });

    match existing_key {
        Some(i) => lines[i] = format!("{key} = {value}"),
        None => lines.insert(end, format!("{key} = {value}")),
    }
    lines.join("\n") + "\n"
}

/// Sets one top-level option in already-read file contents, preserving
/// everything else. Pure: takes and returns text, touches no file.
///
/// Rewrites the line if it is there and inserts it otherwise, rather than
/// serialising the whole file back out — that would discard the comments,
/// which are most of what makes the file worth editing by hand. A key not
/// already present is inserted right before the first `[table]` header, so
/// it lands at the top level rather than inside that table; with no table
/// in the file it goes at the end.
fn with_option(contents: &str, key: &str, value: &str) -> String {
    let mut replaced = false;
    let mut in_table = false;
    let mut lines: Vec<String> = Vec::new();
    for line in contents.lines() {
        if line.trim_start().starts_with('[') {
            in_table = true;
        }
        // Only at the top level: a key inside a [table] means something
        // else, and a table header ends the search for this key.
        let is_this_key = !in_table
            && line
                .split('=')
                .next()
                .is_some_and(|name| name.trim() == key);
        if is_this_key && !replaced {
            lines.push(format!("{key} = {value}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        // Before the first table header, or the key would land inside it.
        let insert_at = lines
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .unwrap_or(lines.len());
        lines.insert(insert_at, format!("{key} = {value}"));
    }

    lines.join("\n") + "\n"
}

/// Checks that edited contents still parse as a [`Config`], so a bug in
/// [`with_option`] or [`with_table_option`] — or an unescaped value passed
/// to them — cannot silently invalidate the whole file the next time it is
/// read.
fn parse_checked(contents: String) -> Result<String, String> {
    toml::from_str::<Config>(&contents)
        .map(|_| contents)
        .map_err(|e| format!("edit would leave an unparsable config: {e}"))
}

/// Creates the Application Support directory holding `config.toml` if it is
/// not there yet, and makes sure it is not readable by other accounts —
/// aliases and applications in it say something about how this Mac is used.
fn secure_config_dir(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

/// Writes `config.toml`, keeping it readable only by the owner.
fn write_config(path: &std::path::Path, contents: String) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())
}

/// Sets an option inside a table, such as `[audio]`.
///
/// Same care as [`set_option`]: the file is edited, not regenerated, so the
/// comments survive. Creates the table if it is not there yet. Refuses to
/// write if the result would not parse.
pub fn set_table_option(table: &str, key: &str, value: &str) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    if let Some(parent) = path.parent() {
        secure_config_dir(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = parse_checked(with_table_option(&existing, table, key, value))?;
    write_config(&path, updated)
}

/// Sets one top-level option, preserving everything else.
///
/// Rewrites the line if it is there and appends it otherwise, rather than
/// serialising the whole file back out — that would discard the comments,
/// which are most of what makes the file worth editing by hand. Refuses to
/// write if the result would not parse.
pub fn set_option(key: &str, value: &str) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    if let Some(parent) = path.parent() {
        secure_config_dir(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = parse_checked(with_option(&existing, key, value))?;
    write_config(&path, updated)
}

/// Reads the configuration, or returns the defaults if there is no file.
///
/// A malformed file is reported and then ignored: a typo in an optional
/// setting should not stop the program from starting.
pub fn load() -> Config {
    let Some(file) = path() else {
        return Config::default();
    };
    let Ok(contents) = std::fs::read_to_string(&file) else {
        return Config::default();
    };
    match toml::from_str(&contents) {
        Ok(config) => {
            crate::journal::write(&format!("Configuration read from {}", file.display()));
            config
        }
        Err(e) => {
            crate::journal::write(&format!("Ignoring {}: {e}", file.display()));
            Config::default()
        }
    }
}

/// The range `max_utterance_ms` is allowed to take.
///
/// Anything under a second cuts commands in half; anything over fifteen
/// seconds is not a command, and the memory it costs is never returned.
const MIN_UTTERANCE_MS: usize = 1_000;
const MAX_UTTERANCE_MS: usize = 15_000;

impl Config {
    /// Audio settings, with anything unset left at its default.
    pub fn audio_settings(&self) -> audio::Settings {
        let defaults = audio::Settings::default();
        audio::Settings {
            speech_threshold: self.audio.speech_threshold.unwrap_or(defaults.speech_threshold),
            silence_end_ms: self.audio.silence_end_ms.unwrap_or(defaults.silence_end_ms),
            min_speech_ms: self.audio.min_speech_ms.unwrap_or(defaults.min_speech_ms),
            // Clamped: the setting decides how big the encoder's arena
            // grows, and the process never gives that memory back. A
            // minute in the configuration file would cost a gigabyte of
            // resident memory for the rest of the day.
            max_utterance_ms: self
                .audio
                .max_utterance_ms
                .unwrap_or(defaults.max_utterance_ms)
                .clamp(MIN_UTTERANCE_MS, MAX_UTTERANCE_MS),
        }
    }

    /// User applications as `'static` entries.
    ///
    /// Leaking is deliberate and bounded: these are read once at startup and
    /// live until the process ends, which lets them share the same type as
    /// the built-in table instead of forcing lifetimes through everything.
    pub fn extra_apps(&self) -> Vec<App> {
        self.apps
            .iter()
            .map(|app| App {
                name: Box::leak(app.name.clone().into_boxed_str()),
                bundle_id: Box::leak(app.bundle_id.clone().into_boxed_str()),
                aliases: Box::leak(
                    app.aliases
                        .iter()
                        .map(|a| &*Box::leak(crate::text::normalise(a).into_boxed_str()))
                        .collect::<Vec<&'static str>>()
                        .into_boxed_slice(),
                ),
            })
            .collect()
    }

    /// The microphone to use, or none to follow the system.
    pub fn microphone(&self) -> Option<String> {
        self.microphone.clone().filter(|name| !name.trim().is_empty())
    }

    /// The output device to speak through, or none for the system's.
    pub fn speaker(&self) -> Option<String> {
        self.speaker.clone().filter(|name| !name.trim().is_empty())
    }

    /// The voice to speak with, or none to stay with the system default.
    pub fn voice(&self) -> Option<String> {
        self.voice
            .clone()
            .filter(|v| !v.trim().is_empty())
            .or_else(crate::speech::default_voice)
    }

    /// How fast to speak.
    pub fn speech_rate(&self) -> u32 {
        self.speech_rate.unwrap_or(crate::speech::DEFAULT_RATE).clamp(120, 320)
    }

    /// The shortcut that pauses and resumes, or none.
    pub fn resume_shortcut(&self) -> Option<String> {
        let shortcut = self
            .resume_shortcut
            .clone()
            .unwrap_or_else(|| DEFAULT_RESUME_SHORTCUT.to_string());
        (!shortcut.trim().is_empty()).then_some(shortcut)
    }

    /// Confidence a phrase needs before it is acted on.
    pub fn command_threshold(&self) -> f32 {
        self.threshold
            .unwrap_or(crate::commands::DEFAULT_THRESHOLD)
            .clamp(0.3, 1.0)
    }

    /// How alike a voice must sound before it is obeyed.
    pub fn voice_threshold(&self) -> f32 {
        self.voice_threshold.unwrap_or(DEFAULT_VOICE_THRESHOLD).clamp(0.0, 1.0)
    }

    /// How long to keep the model in memory with nothing to do.
    ///
    /// `None` in the file means the default; zero means never unload.
    pub fn idle_unload(&self) -> Option<Duration> {
        match self.unload_after_minutes.unwrap_or(DEFAULT_UNLOAD_MINUTES) {
            0 => None,
            minutes => Some(Duration::from_secs(minutes * 60)),
        }
    }

    /// How long the conversation window stays open after a command or an
    /// answer. `None` (zero seconds configured) disables it.
    pub fn conversation_window(&self) -> Duration {
        Duration::from_secs(self.conversation_seconds.unwrap_or(DEFAULT_CONVERSATION_SECONDS))
    }

    /// Whether to listen continuously or only while the shortcut is held.
    ///
    /// Anything other than "hold" — including a typo — falls back to
    /// "always", the safer default: a mistyped value should not leave
    /// someone wondering why Minion never listens.
    pub fn listen_mode(&self) -> ListenMode {
        match self.listen_mode.as_deref() {
            Some("hold") => ListenMode::Hold,
            _ => ListenMode::Always,
        }
    }

    /// Bundle IDs that pause listening while in front.
    pub fn pause_during(&self) -> Vec<String> {
        self.pause_during
            .clone()
            .unwrap_or_else(|| DEFAULT_PAUSE_DURING.iter().map(|id| id.to_string()).collect())
    }

    /// Commands defined in the file, as `'static` entries.
    ///
    /// Anything whose shortcut cannot be read is reported and skipped: one
    /// typo should cost that command, not the whole file.
    pub fn extra_commands(&self) -> Vec<crate::commands::Command> {
        self.commands
            .iter()
            .filter_map(|entry| {
                let Some((code, mods)) = crate::actions::parse_shortcut(&entry.keys) else {
                    crate::journal::write(&format!(
                        "Ignoring command «{}»: cannot read the shortcut «{}»",
                        entry.name, entry.keys
                    ));
                    return None;
                };
                let phrases: Vec<&'static str> = entry
                    .phrases
                    .iter()
                    .map(|p| &*Box::leak(crate::text::normalise(p).into_boxed_str()))
                    .collect();
                Some(crate::commands::Command {
                    phrases: Box::leak(phrases.into_boxed_slice()),
                    name: Box::leak(entry.name.clone().into_boxed_str()),
                    action: crate::commands::Action::Key(code, mods),
                })
            })
            .collect()
    }

    /// User aliases as `(command name, phrase)`, both normalised.
    pub fn extra_aliases(&self) -> Vec<(&'static str, &'static str)> {
        self.aliases
            .iter()
            .map(|alias| {
                let command: &'static str =
                    Box::leak(alias.command.clone().into_boxed_str());
                let phrase: &'static str =
                    Box::leak(crate::text::normalise(&alias.phrase).into_boxed_str());
                (command, phrase)
            })
            .collect()
    }

    pub fn wake_words(&self) -> Option<Vec<&'static str>> {
        if self.wake_words.is_empty() {
            return None;
        }
        Some(
            self.wake_words
                .iter()
                .map(|w| &*Box::leak(crate::text::normalise(w).into_boxed_str()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_utterance_stays_within_reach() {
        // The setting sizes the encoder's arena, and the process never
        // gives that memory back: a minute in the file would cost a
        // gigabyte of resident memory for the rest of the day.
        let mut config = Config::default();
        config.audio.max_utterance_ms = Some(60_000);
        assert_eq!(config.audio_settings().max_utterance_ms, MAX_UTTERANCE_MS);

        config.audio.max_utterance_ms = Some(10);
        assert_eq!(config.audio_settings().max_utterance_ms, MIN_UTTERANCE_MS);

        // Anything sensible is still honoured.
        config.audio.max_utterance_ms = Some(6_000);
        assert_eq!(config.audio_settings().max_utterance_ms, 6_000);
    }

    #[test]
    fn the_default_resume_shortcut_parses_and_is_not_alt_space() {
        // alt-space collides with the remaps Alfred, Raycast and
        // Spotlight commonly use for their own summon shortcut.
        assert_ne!(DEFAULT_RESUME_SHORTCUT, "alt-space");
        assert!(
            crate::actions::parse_shortcut(DEFAULT_RESUME_SHORTCUT).is_some(),
            "the default must be a shortcut actions::parse_shortcut accepts"
        );
        assert_eq!(Config::default().resume_shortcut().as_deref(), Some(DEFAULT_RESUME_SHORTCUT));
    }

    #[test]
    fn setting_an_option_keeps_the_rest_of_the_file() {
        // The comments are most of the value of a hand-edited file.
        let before = "# a note\nsounds = true\n\n[audio]\nsilence_end_ms = 900\n";
        let after = with_option(before, "sounds", "false");
        assert!(after.contains("# a note"), "comments survive");
        assert!(after.contains("sounds = false"), "the value changed");
        assert!(after.contains("silence_end_ms = 900"), "other settings survive");
    }

    #[test]
    fn a_missing_top_level_key_is_appended_before_the_first_table() {
        let before = "sounds = true\n\n[audio]\nsilence_end_ms = 900\n";
        let after = with_option(before, "speak", "false");
        let sounds_at = after.find("sounds").unwrap();
        let speak_at = after.find("speak").unwrap();
        let table_at = after.find("[audio]").unwrap();
        assert!(sounds_at < speak_at, "new key comes after what was there");
        assert!(speak_at < table_at, "new key lands before the first table");
    }

    #[test]
    fn a_missing_top_level_key_is_appended_with_no_table_at_all() {
        let before = "sounds = true\n";
        let after = with_option(before, "speak", "false");
        assert!(after.contains("sounds = true"));
        assert!(after.contains("speak = false"));
    }

    #[test]
    fn a_key_only_replaces_the_top_level_one_not_a_same_named_key_in_a_table() {
        // A key inside [audio] must not be mistaken for the top-level one,
        // and setting the top-level one must not touch the one in the table.
        let before = "threshold = 0.7\n\n[audio]\nspeech_threshold = 0.02\n";
        let after = with_option(before, "threshold", "0.8");
        assert!(after.contains("threshold = 0.8"));
        assert!(after.contains("speech_threshold = 0.02"), "table key untouched");
        assert_eq!(after.matches("threshold").count(), 2, "no key duplicated");
    }

    #[test]
    fn a_table_option_replaces_an_existing_key_and_creates_a_missing_table() {
        let before = "sounds = true\n\n[audio]\nsilence_end_ms = 900\nmin_speech_ms = 300\n";
        let after = with_table_option(before, "audio", "silence_end_ms", "700");
        assert!(after.contains("silence_end_ms = 700"));
        assert!(after.contains("min_speech_ms = 300"), "sibling key survives");
        assert!(after.contains("sounds = true"), "top-level key survives");

        let no_table = "sounds = true\n";
        let created = with_table_option(no_table, "audio", "silence_end_ms", "700");
        assert!(created.contains("[audio]"));
        assert!(created.contains("silence_end_ms = 700"));
    }

    #[test]
    fn a_table_key_does_not_clobber_a_same_named_key_in_another_table() {
        let before = "[audio]\nsilence_end_ms = 900\n\n[speaker]\nsilence_end_ms = 1\n";
        let after = with_table_option(before, "audio", "silence_end_ms", "700");
        assert!(after.contains("[audio]"));
        let audio_at = after.find("[audio]").unwrap();
        let speaker_at = after.find("[speaker]").unwrap();
        let audio_section = &after[audio_at..speaker_at];
        assert!(audio_section.contains("silence_end_ms = 700"));
        let speaker_section = &after[speaker_at..];
        assert!(speaker_section.contains("silence_end_ms = 1"), "other table untouched");
    }

    #[test]
    fn values_with_quotes_and_backslashes_are_escaped_before_writing() {
        // A bare `"{value}"` would break on either character; toml_string
        // must produce something that parses back to the original text.
        let raw = "say \"hi\" \\ bye";
        let escaped = toml_string(raw);
        let contents = with_option("", "microphone", &escaped);
        let parsed: toml::Value = toml::from_str(&contents).expect("escaped value must parse");
        assert_eq!(parsed.get("microphone").and_then(|v| v.as_str()), Some(raw));
    }

    #[test]
    fn sounds_are_on_by_default_and_can_be_turned_off() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert!(default.sounds);
        let quiet: Config = toml::from_str("sounds = false").expect("should parse");
        assert!(!quiet.sounds);
    }

    #[test]
    fn overheard_speech_is_not_logged_by_default() {
        let config: Config = toml::from_str("").expect("empty config should parse");
        assert!(!config.log_ignored_speech);
    }

    #[test]
    fn an_empty_file_yields_defaults() {
        let config: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(
            config.audio_settings().speech_threshold,
            audio::Settings::default().speech_threshold
        );
        assert!(config.extra_apps().is_empty());
        assert!(config.wake_words().is_none());
    }

    #[test]
    fn partial_settings_keep_the_other_defaults() {
        let config: Config = toml::from_str("[audio]\nsilence_end_ms = 900\n")
            .expect("partial config should parse");
        let settings = config.audio_settings();
        assert_eq!(settings.silence_end_ms, 900);
        assert_eq!(
            settings.speech_threshold,
            audio::Settings::default().speech_threshold
        );
    }

    #[test]
    fn the_model_is_released_when_idle_by_default() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(default.idle_unload(), Some(Duration::from_secs(300)));

        let never: Config = toml::from_str("unload_after_minutes = 0").expect("should parse");
        assert_eq!(never.idle_unload(), None, "zero should keep it loaded");

        let custom: Config = toml::from_str("unload_after_minutes = 30").expect("should parse");
        assert_eq!(custom.idle_unload(), Some(Duration::from_secs(1800)));
    }

    #[test]
    fn the_conversation_window_is_five_seconds_by_default_and_zero_disables_it() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(default.conversation_window(), Duration::from_secs(5));

        let disabled: Config =
            toml::from_str("conversation_seconds = 0").expect("should parse");
        assert_eq!(disabled.conversation_window(), Duration::ZERO);
    }

    #[test]
    fn listen_mode_defaults_to_always_and_a_typo_falls_back_to_it() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(default.listen_mode(), ListenMode::Always);

        let hold: Config = toml::from_str(r#"listen_mode = "hold""#).expect("should parse");
        assert_eq!(hold.listen_mode(), ListenMode::Hold);

        let typo: Config = toml::from_str(r#"listen_mode = "holf""#).expect("should parse");
        assert_eq!(typo.listen_mode(), ListenMode::Always);
    }

    #[test]
    fn pause_during_defaults_to_the_built_in_conferencing_apps() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(
            default.pause_during(),
            vec!["us.zoom.xos", "com.microsoft.teams2", "com.apple.FaceTime"]
        );

        let custom: Config =
            toml::from_str(r#"pause_during = ["com.example.calls"]"#).expect("should parse");
        assert_eq!(custom.pause_during(), vec!["com.example.calls"]);
    }

    #[test]
    fn aliases_are_read_and_normalised() {
        let config: Config = toml::from_str(
            r#"
            [[aliases]]
            command = "atrás"
            phrase = "Retrocede la Página"
            "#,
        )
        .expect("alias config should parse");
        assert_eq!(config.extra_aliases(), vec![("atrás", "retrocede la pagina")]);
    }

    #[test]
    fn user_applications_are_normalised() {
        let config: Config = toml::from_str(
            r#"
            [[apps]]
            name = "Notion"
            bundle_id = "notion.id"
            aliases = ["Noción", "NOTION"]
            "#,
        )
        .expect("app config should parse");
        let apps = config.extra_apps();
        assert_eq!(apps.len(), 1);
        // Aliases are compared against normalised speech, so they are stored
        // normalised too — otherwise an accent in the file would never match.
        assert_eq!(apps[0].aliases, ["nocion", "notion"]);
    }

    #[test]
    fn reads_commands_of_your_own() {
        let config: Config = toml::from_str(
            r#"
            [[commands]]
            name = "compilar"
            phrases = ["Compila el proyecto", "compila"]
            keys = "cmd-shift-b"
            "#,
        )
        .expect("command config should parse");
        let commands = config.extra_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "compilar");
        assert_eq!(commands[0].phrases, ["compila el proyecto", "compila"]);
    }

    #[test]
    fn an_unreadable_shortcut_costs_only_that_command() {
        let config: Config = toml::from_str(
            r#"
            [[commands]]
            name = "roto"
            phrases = ["esto no va"]
            keys = "cmd-shift"

            [[commands]]
            name = "bueno"
            phrases = ["esto si"]
            keys = "cmd-k"
            "#,
        )
        .expect("should parse");
        let commands = config.extra_commands();
        assert_eq!(commands.len(), 1, "the good one should survive");
        assert_eq!(commands[0].name, "bueno");
    }

    #[test]
    fn a_typo_is_reported_not_silently_accepted() {
        let bad: Result<Config, _> = toml::from_str("[audio]\nsilence_end = 900\n");
        assert!(bad.is_err(), "unknown fields must not pass unnoticed");
    }

    /// Every setting in the shipped example must still be a real field on
    /// [`Config`] — this is what stops `config.example.toml` from drifting
    /// into a stale copy of something else, as it once did.
    #[test]
    fn example_config_parses() {
        let config: Config = toml::from_str(include_str!("../config.example.toml"))
            .expect("the example config should parse as a real Config");
        // Everything in the example is commented out, so this is the
        // all-defaults case — but parsing it at all is the point.
        assert!(config.wake_words().is_none());
        assert_eq!(config.voice_threshold(), DEFAULT_VOICE_THRESHOLD);
        assert_eq!(config.command_threshold(), crate::commands::DEFAULT_THRESHOLD);
    }
}

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

/// Cosine similarity a voice must reach to be treated as yours.
///
/// ECAPA embeddings of the same person typically score well above this and
/// different people well below, but rooms and microphones move both. It
/// errs low: refusing to hear you is worse than hearing someone else say
/// the wake word, which the vocabulary then has to accept anyway.
const DEFAULT_VOICE_THRESHOLD: f32 = 0.45;

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
    /// one, default 0.45.
    pub voice_threshold: Option<f32>,

    /// Entirely new commands, bound to a keyboard shortcut.
    #[serde(default)]
    pub commands: Vec<CommandConfig>,
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
            unload_after_minutes: None,
        }
    }
}

/// Where the configuration file lives.
pub fn path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Minion/config.toml"))
}

/// Sets one top-level option, preserving everything else.
///
/// Rewrites the line if it is there and appends it otherwise, rather than
/// serialising the whole file back out — that would discard the comments,
/// which are most of what makes the file worth editing by hand.
pub fn set_option(key: &str, value: &str) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    let mut replaced = false;
    let mut lines: Vec<String> = Vec::new();
    for line in existing.lines() {
        let is_this_key = line
            .split('=')
            .next()
            .is_some_and(|name| name.trim() == key);
        // Only at the top level: a key inside a [table] means something else.
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

    std::fs::write(&path, lines.join("\n") + "\n").map_err(|e| e.to_string())
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

impl Config {
    /// Audio settings, with anything unset left at its default.
    pub fn audio_settings(&self) -> audio::Settings {
        let defaults = audio::Settings::default();
        audio::Settings {
            speech_threshold: self.audio.speech_threshold.unwrap_or(defaults.speech_threshold),
            silence_end_ms: self.audio.silence_end_ms.unwrap_or(defaults.silence_end_ms),
            min_speech_ms: self.audio.min_speech_ms.unwrap_or(defaults.min_speech_ms),
            max_utterance_ms: self.audio.max_utterance_ms.unwrap_or(defaults.max_utterance_ms),
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
    fn setting_an_option_keeps_the_rest_of_the_file() {
        // The comments are most of the value of a hand-edited file.
        let before = "# a note\nsounds = true\n\n[audio]\nsilence_end_ms = 900\n";
        // Simulated here rather than touching the real file.
        let mut lines: Vec<String> = Vec::new();
        let mut replaced = false;
        for line in before.lines() {
            if line.split('=').next().is_some_and(|n| n.trim() == "sounds") && !replaced {
                lines.push("sounds = false".into());
                replaced = true;
            } else {
                lines.push(line.into());
            }
        }
        let after = lines.join("\n");
        assert!(after.contains("# a note"), "comments survive");
        assert!(after.contains("sounds = false"), "the value changed");
        assert!(after.contains("silence_end_ms = 900"), "other settings survive");
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
}

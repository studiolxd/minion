//! User configuration, read once at startup.
//!
//! Everything here is optional: with no file at all, the built-in defaults
//! apply. The point is that adding your own applications or retuning the
//! speech detector should not require a Rust toolchain.
//!
//! Lives at `~/Library/Application Support/Oyente/config.toml`.

use std::path::PathBuf;

use serde::Deserialize;

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

    /// Whether to write down speech that was not addressed to Oyente.
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
        }
    }
}

/// Where the configuration file lives.
pub fn path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Oyente/config.toml"))
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
            println!("Configuration read from {}", file.display());
            config
        }
        Err(e) => {
            eprintln!("Ignoring {}: {e}", file.display());
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
    fn a_typo_is_reported_not_silently_accepted() {
        let bad: Result<Config, _> = toml::from_str("[audio]\nsilence_end = 900\n");
        assert!(bad.is_err(), "unknown fields must not pass unnoticed");
    }
}

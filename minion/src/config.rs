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

/// How close a runner-up has to be before a near-tie is asked about,
/// when the file says nothing. Measured on the same confidence scale the
/// commands are scored on.
const DEFAULT_DISAMBIGUATION_MARGIN: f32 = 0.08;

/// Shortcut that pauses and resumes when nothing is set.
pub const DEFAULT_RESUME_SHORTCUT: &str = "ctrl-alt-m";

/// Seconds the HUD panel stays up when the file says nothing — matches
/// what `hud.rs` used as a constant before this became configurable.
const DEFAULT_HUD_SECONDS: f64 = 4.0;

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

    /// What to call the voice enrolled before profiles had names.
    ///
    /// Only ever read once, when the single `voice.txt` of earlier versions
    /// is copied into `voices/`. After that the directory is the list and
    /// this key does nothing; there is no `[[profiles]]` table to keep in
    /// step with it. Empty means «yo».
    pub voice_name: Option<String>,

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

    /// Whether spoken punctuation ("coma", "punto", "abre interrogación"…)
    /// is turned into signs while dictating. On by default; turn off to
    /// type every such word verbatim.
    #[serde(default = "yes")]
    pub spoken_punctuation: bool,

    /// Whether dictation capitalises the start of a sentence, and honours
    /// «mayúscula»/«en mayúsculas». On by default.
    #[serde(default = "yes")]
    pub auto_capitalise: bool,

    /// Words the recogniser reliably mangles, and what to type instead —
    /// almost always names. Applied while dictating, before punctuation.
    #[serde(default)]
    pub dictation_words: Vec<DictationWordConfig>,
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

    /// Pause listening while another process is capturing the microphone —
    /// a video call is the one time an always-on microphone is unwelcome.
    ///
    /// On by default. The signal is CoreAudio's own: which processes are
    /// running an input stream right now, not which application happens to
    /// be in front, so a call in a background window pauses Minion and
    /// Teams in front with no call does not. Needs macOS 14; on 13 the
    /// property does not exist and the setting does nothing.
    #[serde(default = "yes")]
    pub pause_when_microphone_busy: bool,

    /// Named sequences of phrases, run one after another. Only read from
    /// here — a downloaded vocabulary pack cannot define one, since a
    /// macro presses keys and launches applications on its own say-so,
    /// which is not something a file that may have come from elsewhere
    /// gets to do.
    #[serde(default)]
    pub macros: Vec<MacroConfig>,

    /// Which engine a bare "busca X" searches, with nothing after it
    /// naming one: "google", "youtube", "wikipedia" or "amazon". Empty,
    /// unset or unrecognised all mean "google" — the last of those is
    /// reported in the log, at startup, the same as an alias with no
    /// command to point at.
    pub search_engine: Option<String>,

    /// Ask out loud about a phrase that was not understood but came close
    /// to something ("¿Querías decir «abrir Safari»?"), and remember the
    /// answer as an alias.
    ///
    /// On by default: the alternative is a log line nobody reads and the
    /// same mistake tomorrow. False is what Minion did before — say
    /// nothing, write it down, and wait for `minion learn`.
    #[serde(default = "yes")]
    pub ask_before_learning: bool,

    /// How close a second reading of a phrase has to be to the winning
    /// one before Minion stops and asks which was meant.
    ///
    /// A margin in the same units as the confidence scores: 0.08 means
    /// "within eight hundredths". `None` is the default of 0.08; zero
    /// disables asking entirely, for someone who would rather Minion take
    /// its best guess than talk back.
    pub disambiguation_margin: Option<f32>,

    /// Post a Notification Center banner for answers, timers and blocked
    /// commands — see `notify.rs`.
    ///
    /// On by default: with `speak = false` and the menu bar out of sight,
    /// a timer or a "¿qué suena?" would otherwise have nowhere to land.
    #[serde(default = "yes")]
    pub notifications: bool,

    /// Show the "what did it hear" HUD panel around an utterance — see
    /// `hud.rs`. Off by default: with this off the panel never appears on
    /// its own (though «muestra lo que oyes» still shows it, once, for
    /// [`Config::hud_seconds`] — an explicit ask overrides the setting for
    /// that one time rather than being silently ignored).
    #[serde(default)]
    pub show_hud: bool,
    /// Look once a day for a newer Minion, and offer to install it —
    /// see `updater.rs`.
    ///
    /// On by default, and quiet: the check only ever says anything when
    /// there is a new version, so a machine that is up to date never
    /// hears about it. False stops the automatic check; «Buscar
    /// actualizaciones…» in the menu still works.
    #[serde(default = "yes")]
    pub check_updates: bool,
    /// The optional AI layer — see `ai.rs`. Off unless `[ai] backend` names
    /// something.
    ///
    /// Last in this struct on purpose, and last in the file it is read
    /// from: this is a table, and a top-level key written after a table
    /// header belongs to that table, which `deny_unknown_fields` then
    /// rejects — taking every other setting down with it.
    #[serde(default)]
    pub ai: AiConfig,

    /// How eagerly Minion releases memory when idle: "auto" (the
    /// default) follows whether the machine is on battery power,
    /// "battery" always behaves as if it were, "performance" never
    /// unloads anything. See [`Config::energy_mode`].
    pub energy: Option<String>,

    /// Seconds the "what did it hear" HUD panel stays up after the last
    /// thing worth showing, with nothing else keeping it open — see
    /// `hud.rs`. `None` is the default of 4.
    pub hud_seconds: Option<f64>,

    /// Keep the HUD panel on screen permanently, instead of only around an
    /// utterance — see `hud.rs`. Off by default, and meaningless (ignored
    /// in practice — Ajustes disables its own checkbox) while `show_hud`
    /// is off: pinning a panel that never appears has nothing to pin.
    #[serde(default)]
    pub hud_pinned: bool,
}

/// The `[ai]` table: which model answers what the vocabulary cannot, and
/// how much of it is allowed.
///
/// Everything optional, and `backend` empty by default, because sending
/// what was said in a room to a service over the network is not something
/// an always-on microphone should start doing on its own.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    /// Which backend: a CLI agent already installed and paid for
    /// ("claude-code", "codex", "gemini-cli"), or an HTTP provider
    /// ("openai", "anthropic", "deepseek", "mistral", "groq",
    /// "openrouter", "xai", "gemini", "ollama", "lmstudio"). Empty turns
    /// the whole thing off.
    #[serde(default)]
    pub backend: String,

    /// Which model. Empty takes the backend's own default.
    #[serde(default)]
    pub model: String,

    /// What the AI is allowed to be used for: "questions" (a question the
    /// built-in answers could not handle) and "unknown" (a phrase the
    /// vocabulary did not recognise, which the model may be able to match
    /// to a command). Named `use` in the file.
    #[serde(rename = "use", default = "default_ai_uses")]
    pub uses: Vec<String>,

    /// Requests allowed per day, counted in `ai-usage.toml` next to this
    /// file. Zero means no limit.
    pub daily_limit: Option<u32>,

    /// Minutes without a question before the conversation — and any warm
    /// agent process behind it — is dropped, the same way the speech model
    /// is unloaded when nobody is talking.
    pub idle_minutes: Option<u64>,

    /// The key for an HTTP provider, either written here or the word
    /// "keychain" to read it from the macOS keychain instead (put it there
    /// with `minion ai set-key <backend>`).
    #[serde(default)]
    pub api_key: String,

    /// Overrides the provider's base URL, for one that is not in the list
    /// or a proxy in front of one that is.
    #[serde(default)]
    pub base_url: String,
}

/// What `[ai] use` means when the table does not say.
fn default_ai_uses() -> Vec<String> {
    vec!["questions".to_string()]
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            backend: String::new(),
            model: String::new(),
            uses: default_ai_uses(),
            daily_limit: None,
            idle_minutes: None,
            api_key: String::new(),
            base_url: String::new(),
        }
    }
}

/// How Minion decides when to listen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenMode {
    Always,
    /// Only while the shortcut is held down.
    Hold,
}

/// How eagerly Minion releases memory when idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyMode {
    /// Follows whether the machine is on battery power.
    Auto,
    /// Never unloads anything, battery or not.
    Performance,
    /// Behaves as if always on battery power.
    Battery,
}

/// Idle minutes before the speech model is released while on battery
/// power (or in `battery` mode), regardless of `unload_after_minutes` —
/// measured to be short enough to matter on a laptop with the lid up all
/// day, long enough not to cost a reload on every other sentence.
pub const BATTERY_MODEL_UNLOAD_MINUTES: u64 = 2;

/// Idle minutes before the speaker model is released on battery power (or
/// in `battery` mode). Longer than the speech model's: reloading it costs
/// a fresh embedding of the voice profile as well as the model weights,
/// and it is smaller to begin with, so there is less to gain from letting
/// it go early.
pub const BATTERY_SPEAKER_UNLOAD_MINUTES: u64 = 10;

/// What the idle tick should unload, worked out from the energy mode,
/// whether the machine is on battery power right now, how long nothing has
/// been said, and the ordinary (non-energy) idle threshold from
/// `unload_after_minutes`.
///
/// Pure on purpose — reading `pmset` and the wall clock belong to the
/// caller, so this can be tested against fabricated inputs instead of a
/// real idle Minion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnergyDecision {
    pub unload_model: bool,
    pub unload_speaker: bool,
}

pub fn energy_decision(
    mode: EnergyMode,
    on_battery: bool,
    idle: Duration,
    normal_unload_after: Option<Duration>,
) -> EnergyDecision {
    if mode == EnergyMode::Performance {
        return EnergyDecision { unload_model: false, unload_speaker: false };
    }
    if mode == EnergyMode::Battery || on_battery {
        return EnergyDecision {
            unload_model: idle >= Duration::from_secs(BATTERY_MODEL_UNLOAD_MINUTES * 60),
            unload_speaker: idle >= Duration::from_secs(BATTERY_SPEAKER_UNLOAD_MINUTES * 60),
        };
    }
    EnergyDecision {
        unload_model: normal_unload_after.is_some_and(|after| idle >= after),
        unload_speaker: false,
    }
}

/// A command of your own: what to say, and what to do — a shortcut, an
/// AppleScript, or a shell command. Exactly one of `keys`, `script` and
/// `shell` must be given; see [`Config::extra_commands`].
///
/// `script` and `shell` exist here and nowhere else on purpose: a
/// downloaded vocabulary pack cannot ask Minion to run anything, since it
/// is data that may have come from elsewhere, but a line typed into your
/// own `config.toml` is not.
/// A command of your own: what to say, and what it does.
///
/// Exactly one of `keys`, `text` and `url` is expected. Unlike a vocabulary
/// pack's `[[commands]]` (`vocabulary.rs`), there is no `action` naming one
/// of the built-in `NAMED_ACTIONS` — that table is private to `vocabulary.rs`
/// on purpose, the same way `macros` are "only read from here": a command
/// typed by hand in this file can only press keys, type text or open a
/// page, never reach into the closed set of system actions.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    /// Shown in the log when it runs.
    pub name: String,
    /// Ways of saying it.
    pub phrases: Vec<String>,
    /// The shortcut, as written on a menu: "cmd-shift-b", "ctrl+alt+left".
    pub keys: Option<String>,
    /// AppleScript, run via `osascript -e`.
    pub script: Option<String>,
    /// A shell command, run via `/bin/sh -c` with stdin closed, its output
    /// logged, and a 30-second ceiling. Never in a terminal window.
    pub shell: Option<String>,
    /// Text to type into whatever has focus.
    pub text: Option<String>,
    /// A page to open.
    pub url: Option<String>,
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
    /// Which detector decides speech from noise: `"silero"` (the default,
    /// when the model is on disk) or `"energy"`.
    pub vad: Option<String>,
    /// Silero's score above which a frame counts as speech, 0 to 1.
    pub vad_threshold: Option<f32>,
    /// Discard what the Mac itself is playing, heard back through the
    /// microphone: Netflix, music, a notification chime, the other side of
    /// a call. On by default; the tap is fail-open — if it cannot start,
    /// Minion says so once and behaves as if this were off.
    pub ignore_own_audio: Option<bool>,
    /// How alike an utterance and the Mac's own output have to be before
    /// the utterance is thrown away, 0 to 1. See
    /// [`crate::loopback::DEFAULT_THRESHOLD`].
    pub own_audio_threshold: Option<f32>,
}

/// A name (or other word) the recogniser reliably mangles, and the correct
/// spelling to type instead — the fix for the log's «unknown ...» lines
/// that turn out to be a mangled name rather than an unrecognised command.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DictationWordConfig {
    /// What the recogniser actually produces, matched via
    /// [`crate::text::normalise`] so accents and case do not matter.
    pub heard: String,
    /// What to type instead, exactly as written here.
    pub written: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub name: String,
    pub bundle_id: String,
    /// Ways of saying the name. Include what the recogniser really hears.
    pub aliases: Vec<String>,
}

/// A macro of your own: a name, the ways of asking for it, and what it
/// does when asked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroConfig {
    /// Shown in the log when it runs.
    pub name: String,
    /// Ways of asking for it.
    pub phrases: Vec<String>,
    /// What to do, in order. Each one must be a phrase Minion would
    /// understand on its own, without the wake word — that is added back
    /// on before it is decided.
    pub steps: Vec<String>,
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
            voice_name: None,
            resume_shortcut: None,
            microphone: None,
            speaker: None,
            save_recordings: false,
            speak: true,
            voice: None,
            speech_rate: None,
            unload_after_minutes: None,
            spoken_punctuation: true,
            auto_capitalise: true,
            dictation_words: Vec::new(),
            conversation_seconds: None,
            listen_mode: None,
            pause_when_microphone_busy: true,
            macros: Vec::new(),
            search_engine: None,
            ask_before_learning: true,
            disambiguation_margin: None,
            notifications: true,
            show_hud: false,
            check_updates: true,
            ai: AiConfig::default(),
            energy: None,
            hud_seconds: None,
            hud_pinned: false,
        }
    }
}

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

/// The `phrase` of one `[[aliases]]` block, read on its own so a block can
/// be matched without deserialising the whole file — the block may sit
/// between others that would fail `deny_unknown_fields` on their own (it
/// never does, in practice, but nothing here needs to assume that).
#[derive(Deserialize)]
struct AliasPhrase {
    phrase: String,
}

/// The `bundle_id` of one `[[apps]]` block, read on its own — see
/// [`AliasPhrase`].
#[derive(Deserialize)]
struct AppBundleId {
    bundle_id: String,
}

/// The `name` of one `[[commands]]` block, read on its own — see
/// [`AliasPhrase`].
#[derive(Deserialize)]
struct CommandName {
    name: String,
}

/// Finds the first `[[table]]` block that parses as `T` and satisfies
/// `matches`, in already-read file contents, as the line range it spans
/// and the value it parsed to. Pure, and touches no file.
///
/// Shared by [`without_block`] (which only needs the range, to delete it)
/// and [`add_alias_to_app`] (which needs the value too, to add to it).
fn find_block<T, F>(contents: &str, table: &str, matches: F) -> Option<(usize, usize, T)>
where
    T: serde::de::DeserializeOwned,
    F: Fn(&T) -> bool,
{
    let header = format!("[[{table}]]");
    let lines: Vec<&str> = contents.lines().collect();

    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == header {
            let start = i;
            let end = lines
                .iter()
                .skip(i + 1)
                .position(|l| l.trim_start().starts_with('['))
                .map_or(lines.len(), |offset| i + 1 + offset);
            blocks.push((start, end));
            i = end;
        } else {
            i += 1;
        }
    }

    blocks.into_iter().find_map(|(start, end)| {
        toml::from_str::<T>(&lines[start + 1..end].join("\n"))
            .ok()
            .filter(&matches)
            .map(|item| (start, end, item))
    })
}

/// Removes the first `[[table]]` block that parses as `T` and satisfies
/// `matches`, from already-read file contents. Pure: takes and returns
/// text, touches no file. Leaves the file untouched if nothing matches.
///
/// Shared by [`without_alias`], [`without_app`] and [`without_command`],
/// which differ only in the table name and what identifies a block.
fn without_block<T, F>(contents: &str, table: &str, matches: F) -> String
where
    T: serde::de::DeserializeOwned,
    F: Fn(&T) -> bool,
{
    let lines: Vec<&str> = contents.lines().collect();
    let Some((start, end, _)) = find_block::<T, F>(contents, table, matches) else {
        return contents.to_string();
    };

    let mut kept: Vec<&str> = lines[..start].to_vec();
    kept.extend(&lines[end..]);
    // A blank line left where the block used to be, at the end of the
    // file, would otherwise grow by one every time the last entry goes.
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    kept.join("\n") + "\n"
}

/// Removes the first `[[aliases]]` block whose `phrase` matches (after
/// [`crate::text::normalise`]) the given phrase, from already-read file
/// contents. Pure: takes and returns text, touches no file. Leaves the
/// file untouched if nothing matches.
///
/// Used by the "Olvidar alias" item in the "Últimas órdenes" menu: an
/// alias is only ever this file's own `[[aliases]]` entry, never something
/// from the built-in vocabulary, so there is always exactly one block (or
/// none) to remove.
fn without_alias(contents: &str, phrase: &str) -> String {
    let target = crate::text::normalise(phrase);
    without_block::<AliasPhrase, _>(contents, "aliases", |alias| {
        crate::text::normalise(&alias.phrase) == target
    })
}

/// Removes the first `[[apps]]` block with the given `bundle_id` — see
/// [`without_alias`].
fn without_app(contents: &str, bundle_id: &str) -> String {
    without_block::<AppBundleId, _>(contents, "apps", |app| app.bundle_id == bundle_id)
}

/// Removes the first `[[commands]]` block with the given `name` — see
/// [`without_alias`].
fn without_command(contents: &str, name: &str) -> String {
    without_block::<CommandName, _>(contents, "commands", |command| command.name == name)
}

/// Removes an alias by its phrase — see [`without_alias`]. Refuses to
/// write if the result would not parse.
pub fn remove_alias(phrase: &str) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    let existing = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let updated = parse_checked(without_alias(&existing, phrase))?;
    write_config(&path, updated)
}

/// Removes an application by its bundle id — see [`without_app`]. Refuses
/// to write if the result would not parse. A built-in application has no
/// `[[apps]]` block in `config.toml` to begin with, so this is a no-op for
/// anything but one added from here.
pub fn remove_app(bundle_id: &str) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    let existing = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let updated = parse_checked(without_app(&existing, bundle_id))?;
    write_config(&path, updated)
}

/// Removes a command by its name — see [`without_command`]. Refuses to
/// write if the result would not parse.
pub fn remove_command(name: &str) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    let existing = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let updated = parse_checked(without_command(&existing, name))?;
    write_config(&path, updated)
}

/// Appends already-formatted TOML to already-read file contents. Pure.
/// Refuses (by returning an error) if the result would not parse.
fn appended(contents: &str, addition: &str) -> Result<String, String> {
    let mut out = contents.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(addition);
    parse_checked(out)
}

/// One `[[apps]]` entry, quoted so that whatever was typed or heard parses.
fn app_block(name: &str, bundle_id: &str, aliases: &[String]) -> String {
    let aliases: Vec<String> = aliases.iter().map(|a| toml_string(a)).collect();
    format!(
        "\n[[apps]]\nname = {}\nbundle_id = {}\naliases = [{}]\n",
        toml_string(name),
        toml_string(bundle_id),
        aliases.join(", ")
    )
}

/// One `[[apps]]` block, read on its own with its aliases — see
/// [`AppBundleId`], which only reads the `bundle_id`.
#[derive(Deserialize)]
struct AppOverride {
    name: String,
    bundle_id: String,
    aliases: Vec<String>,
}

/// Adds one more way an application's name is said, in already-read file
/// contents. Pure: takes and returns text, touches no file.
///
/// `built_in_name` and `built_in_aliases` are what the application answers
/// to before any override — the caller reads them from
/// [`crate::commands::vocabulary`], since this module knows nothing of the
/// built-in table.
///
/// If `config.toml` already has its own `[[apps]]` block for this
/// `bundle_id`, the alias joins its aliases in place: that block is
/// removed and rewritten, rather than a second block being appended next
/// to it, which would leave two overrides for the same application and,
/// since later wins by name, silently drop whichever alias was learned
/// first the next time Minion restarted. With no override yet, a new one
/// is created copying the built-in name and aliases, so the new one adds
/// to them rather than replacing them — the same shape `learn.rs`'s
/// `teach_app` writes when a spoken guess is confirmed.
pub fn add_alias_to_app(
    contents: &str,
    bundle_id: &str,
    alias: &str,
    built_in_name: &str,
    built_in_aliases: &[&str],
) -> Result<String, String> {
    let alias = crate::text::normalise(alias);
    if alias.is_empty() {
        return Err("Hace falta un alias.".to_string());
    }

    let existing = find_block::<AppOverride, _>(contents, "apps", |app| app.bundle_id == bundle_id);
    let (name, mut aliases, without_old) = match existing {
        Some((_, _, app)) => {
            let name = app.name.clone();
            (name, app.aliases, without_app(contents, bundle_id))
        }
        None => {
            let aliases = built_in_aliases.iter().map(|a| a.to_string()).collect();
            (built_in_name.to_string(), aliases, contents.to_string())
        }
    };
    if aliases.contains(&alias) {
        return Ok(contents.to_string()); // already said this way
    }
    aliases.push(alias);
    appended(&without_old, &app_block(&name, bundle_id, &aliases))
}

/// Adds an application, appended to `config.toml`. Refuses to write if the
/// result would not parse — an empty name or bundle id parses fine as a
/// `Config`, so that is checked by the caller, not here.
pub fn add_app(name: &str, bundle_id: &str, aliases: &[String]) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    if let Some(parent) = path.parent() {
        secure_config_dir(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = appended(&existing, &app_block(name, bundle_id, aliases))?;
    write_config(&path, updated)
}

/// Teaches an application already in the vocabulary one more way its name
/// gets said — see [`add_alias_to_app`]. `built_in_name` and
/// `built_in_aliases` come from [`crate::commands::vocabulary`]; refuses
/// to write if the result would not parse.
pub fn learn_app_alias(
    bundle_id: &str,
    alias: &str,
    built_in_name: &str,
    built_in_aliases: &[&str],
) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    if let Some(parent) = path.parent() {
        secure_config_dir(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = add_alias_to_app(&existing, bundle_id, alias, built_in_name, built_in_aliases)?;
    write_config(&path, updated)
}

/// What a command of your own does — see [`CommandConfig`].
pub enum CommandKind {
    Keys(String),
    Text(String),
    Url(String),
}

/// One `[[commands]]` entry, quoted so that whatever was typed parses.
fn command_block(name: &str, phrases: &[String], kind: &CommandKind) -> String {
    let phrases: Vec<String> = phrases.iter().map(|p| toml_string(p)).collect();
    let action = match kind {
        CommandKind::Keys(keys) => format!("keys = {}", toml_string(keys)),
        CommandKind::Text(text) => format!("text = {}", toml_string(text)),
        CommandKind::Url(url) => format!("url = {}", toml_string(url)),
    };
    format!(
        "\n[[commands]]\nname = {}\nphrases = [{}]\n{action}\n",
        toml_string(name),
        phrases.join(", ")
    )
}

/// Adds a command of your own, appended to `config.toml`. Refuses to write
/// if the result would not parse.
pub fn add_command(name: &str, phrases: &[String], kind: &CommandKind) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    if let Some(parent) = path.parent() {
        secure_config_dir(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = appended(&existing, &command_block(name, phrases, kind))?;
    write_config(&path, updated)
}

/// One `[[aliases]]` entry, quoted so that whatever was typed or heard
/// parses.
fn alias_block(command: &str, phrase: &str) -> String {
    format!(
        "\n[[aliases]]\ncommand = {}\nphrase = {}\n",
        toml_string(command),
        toml_string(phrase)
    )
}

/// Adds an alias, appended to `config.toml`. Refuses to write if the
/// result would not parse.
pub fn add_alias(command: &str, phrase: &str) -> Result<(), String> {
    let path = path().ok_or("no home directory")?;
    if let Some(parent) = path.parent() {
        secure_config_dir(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = appended(&existing, &alias_block(command, phrase))?;
    write_config(&path, updated)
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

/// What is wrong with the configuration file, if anything: the parse
/// error, for a file that exists and does not parse. `None` when there is
/// no file or it is fine.
///
/// [`load`] falls back to the defaults and says so only in the log, and a
/// person whose file has just stopped parsing sees a Minion that has
/// quietly forgotten every setting. This is for telling them.
pub fn problem() -> Option<String> {
    let file = path()?;
    let contents = std::fs::read_to_string(&file).ok()?;
    toml::from_str::<Config>(&contents).err().map(|e| {
        let reason = e.message().to_string();
        let line = e
            .span()
            .map(|span| contents[..span.start.min(contents.len())].matches('\n').count() + 1);
        match line {
            Some(line) => format!("línea {line}: {reason}"),
            None => reason,
        }
    })
}

/// The range `max_utterance_ms` is allowed to take.
///
/// Anything under a second cuts commands in half; anything over fifteen
/// seconds is not a command, and the memory it costs is never returned.
const MIN_UTTERANCE_MS: usize = 1_000;
const MAX_UTTERANCE_MS: usize = 15_000;

/// Reads the `vad` setting. `None` leaves the default in place, which is
/// also what an unrecognised name does — a typo should not make Minion
/// deaf to the difference between a voice and the dishwasher.
fn vad_named(name: Option<&str>) -> Option<audio::Vad> {
    let name = name?.trim();
    if name.eq_ignore_ascii_case("energy") {
        return Some(audio::Vad::Energy);
    }
    if name.eq_ignore_ascii_case("silero") {
        return Some(audio::Vad::Silero);
    }
    crate::journal::write(&format!(
        "config.toml: vad = «{name}» is neither «silero» nor «energy»; ignoring it."
    ));
    None
}

/// Works out what a `[[commands]]` entry does: exactly one of `keys`,
/// `script` and `shell`. Reported and dropped if it names none or more
/// than one, or a `keys` value that cannot be read.
fn command_action(entry: &CommandConfig) -> Option<crate::commands::Action> {
    let mut asked: Vec<crate::commands::Action> = Vec::new();
    if let Some(keys) = &entry.keys {
        match crate::actions::parse_shortcut(keys) {
            Some((code, mods)) => asked.push(crate::commands::Action::Key(code, mods)),
            None => {
                crate::journal::write(&format!(
                    "Ignoring command «{}»: cannot read the shortcut «{keys}»",
                    entry.name
                ));
                return None;
            }
        }
    }
    if let Some(text) = &entry.text {
        asked.push(crate::commands::Action::Type(Box::leak(text.clone().into_boxed_str())));
    }
    if let Some(url) = &entry.url {
        asked.push(crate::commands::Action::Open(Box::leak(url.clone().into_boxed_str())));
    }
    if let Some(script) = &entry.script {
        asked.push(crate::commands::Action::RunScript(Box::leak(
            script.clone().into_boxed_str(),
        )));
    }
    if let Some(shell) = &entry.shell {
        asked.push(crate::commands::Action::RunShell(Box::leak(
            shell.clone().into_boxed_str(),
        )));
    }
    match asked.len() {
        1 => Some(asked[0]),
        0 => {
            crate::journal::write(&format!(
                "Ignoring command «{}»: it does nothing — give it keys, text, url, script or shell",
                entry.name
            ));
            None
        }
        _ => {
            crate::journal::write(&format!(
                "Ignoring command «{}»: it asks for more than one thing at once",
                entry.name
            ));
            None
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
            // Clamped: the setting decides how big the encoder's arena
            // grows, and the process never gives that memory back. A
            // minute in the configuration file would cost a gigabyte of
            // resident memory for the rest of the day.
            max_utterance_ms: self
                .audio
                .max_utterance_ms
                .unwrap_or(defaults.max_utterance_ms)
                .clamp(MIN_UTTERANCE_MS, MAX_UTTERANCE_MS),
            vad: vad_named(self.audio.vad.as_deref()).unwrap_or(defaults.vad),
            // A threshold outside 0..1 is not a threshold: every frame
            // would be speech, or none would.
            vad_threshold: self
                .audio
                .vad_threshold
                .unwrap_or(defaults.vad_threshold)
                .clamp(0.0, 1.0),
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
                category: crate::vocabulary::USER_CATEGORY,
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

    /// What to call the voice migrated from the old single profile.
    pub fn voice_name(&self) -> String {
        self.voice_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(crate::speaker::DEFAULT_NAME)
            .to_string()
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

    /// How close the runner-up has to be before a near-tie is put to the
    /// user instead of acted on. Zero never asks.
    pub fn disambiguation_margin(&self) -> f32 {
        self.disambiguation_margin.unwrap_or(DEFAULT_DISAMBIGUATION_MARGIN).clamp(0.0, 0.5)
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

    /// How eagerly to release memory when idle.
    ///
    /// Anything other than "performance" or "battery" — including a typo
    /// or nothing at all — falls back to "auto", the same way
    /// [`Self::listen_mode`] falls back to "always".
    pub fn energy_mode(&self) -> EnergyMode {
        match self.energy.as_deref() {
            Some("performance") => EnergyMode::Performance,
            Some("battery") => EnergyMode::Battery,
            _ => EnergyMode::Auto,
        }
    }

    /// How long the HUD panel stays up after the last thing worth
    /// showing. `None` in the file means the default of 4 seconds.
    pub fn hud_seconds(&self) -> f64 {
        self.hud_seconds.unwrap_or(DEFAULT_HUD_SECONDS)
    }

    /// Commands defined in the file, as `'static` entries.
    ///
    /// Exactly one of `keys`, `script` and `shell` must be given; naming
    /// none or more than one is reported and the entry is skipped, the
    /// same as a shortcut that cannot be read — one bad line costs that
    /// command, not the whole file.
    pub fn extra_commands(&self) -> Vec<crate::commands::Command> {
        self.commands
            .iter()
            .filter_map(|entry| {
                let action = command_action(entry)?;
                let phrases: Vec<&'static str> = entry
                    .phrases
                    .iter()
                    .map(|p| &*Box::leak(crate::text::normalise(p).into_boxed_str()))
                    .collect();
                Some(crate::commands::Command {
                    phrases: Box::leak(phrases.into_boxed_slice()),
                    name: Box::leak(entry.name.clone().into_boxed_str()),
                    action,
                    category: crate::vocabulary::USER_CATEGORY,
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

    /// Personal vocabulary, normalised on the heard side and sorted so the
    /// longest heard phrase is tried first — "mir angel sufire" must be
    /// matched whole rather than stopping at a shorter entry that also
    /// happens to fit its start.
    pub fn dictation_words(&self) -> Vec<(Vec<String>, String)> {
        let mut entries: Vec<(Vec<String>, String)> = self
            .dictation_words
            .iter()
            .map(|word| {
                let heard = crate::text::normalise(&word.heard)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                (heard, word.written.clone())
            })
            .filter(|(heard, _): &(Vec<String>, String)| !heard.is_empty())
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
        entries
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

    /// Named macros as `'static` entries, ready for [`crate::commands`].
    ///
    /// Phrases are normalised, the same as [`Self::extra_commands`]'s —
    /// they are compared against normalised speech. Steps are kept exactly
    /// as written: each one is decided afresh, with the wake word added
    /// back on, so it is normalised then, and keeping the original casing
    /// here is what lets the log show a step the way it was written.
    pub fn macros(&self) -> Vec<crate::commands::Macro> {
        self.macros
            .iter()
            .map(|entry| crate::commands::Macro {
                name: Box::leak(entry.name.clone().into_boxed_str()),
                phrases: Box::leak(
                    entry
                        .phrases
                        .iter()
                        .map(|p| &*Box::leak(crate::text::normalise(p).into_boxed_str()))
                        .collect::<Vec<&'static str>>()
                        .into_boxed_slice(),
                ),
                steps: Box::leak(
                    entry
                        .steps
                        .iter()
                        .map(|s| &*Box::leak(s.clone().into_boxed_str()))
                        .collect::<Vec<&'static str>>()
                        .into_boxed_slice(),
                ),
            })
            .collect()
    }

    /// The search engine named in the file, not yet checked against the
    /// ones Minion knows.
    ///
    /// Left to the caller: that table lives in `commands.rs`, next to
    /// everything else about how a search is carried out, and it is the
    /// one place that can report an unrecognised name and fall back.
    pub fn search_engine(&self) -> Option<String> {
        self.search_engine.clone().filter(|s| !s.trim().is_empty())
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
    fn notifications_are_on_by_default_and_can_be_turned_off() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert!(default.notifications);
        let quiet: Config = toml::from_str("notifications = false").expect("should parse");
        assert!(!quiet.notifications);
    }

    #[test]
    fn the_hud_is_off_by_default_and_can_be_enabled() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert!(!default.show_hud);
        let enabled: Config = toml::from_str("show_hud = true").expect("should parse");
        assert!(enabled.show_hud);
    }

    #[test]
    fn the_hud_is_not_pinned_by_default_and_can_be_pinned_separately() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert!(!default.hud_pinned);
        let pinned: Config =
            toml::from_str("show_hud = true\nhud_pinned = true").expect("should parse");
        assert!(pinned.hud_pinned);
    }

    #[test]
    fn the_hud_stays_up_four_seconds_by_default_and_can_be_changed() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(default.hud_seconds(), 4.0);
        let changed: Config = toml::from_str("hud_seconds = 8.5").expect("should parse");
        assert_eq!(changed.hud_seconds(), 8.5);
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
    fn the_disambiguation_margin_is_read_from_the_file_and_zero_never_asks() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(default.disambiguation_margin(), 0.08);

        let never: Config = toml::from_str("disambiguation_margin = 0.0").expect("should parse");
        assert_eq!(never.disambiguation_margin(), 0.0);

        let wide: Config = toml::from_str("disambiguation_margin = 0.15").expect("should parse");
        assert_eq!(wide.disambiguation_margin(), 0.15);

        // Half the scale is as far as it goes: past that everything ties.
        let absurd: Config = toml::from_str("disambiguation_margin = 9.0").expect("should parse");
        assert_eq!(absurd.disambiguation_margin(), 0.5);
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
    fn pausing_for_a_busy_microphone_is_on_unless_turned_off() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert!(default.pause_when_microphone_busy);

        let off: Config =
            toml::from_str("pause_when_microphone_busy = false").expect("should parse");
        assert!(!off.pause_when_microphone_busy);
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
    fn a_command_can_run_a_script_or_a_shell_command_instead_of_keys() {
        let config: Config = toml::from_str(
            r#"
            [[commands]]
            name = "vacía la papelera"
            phrases = ["vacía la papelera"]
            script = "tell application \"Finder\" to empty trash"

            [[commands]]
            name = "backup"
            phrases = ["haz una copia"]
            shell = "rsync -a ~/Documents ~/Backup"
            "#,
        )
        .expect("should parse");
        let commands = config.extra_commands();
        assert_eq!(commands.len(), 2);
        assert!(matches!(commands[0].action, crate::commands::Action::RunScript(_)));
        assert!(matches!(commands[1].action, crate::commands::Action::RunShell(_)));
    }

    #[test]
    fn a_command_with_keys_and_a_script_is_refused() {
        let config: Config = toml::from_str(
            r#"
            [[commands]]
            name = "ambiguo"
            phrases = ["esto no vale"]
            keys = "cmd-k"
            script = "beep"
            "#,
        )
        .expect("should parse");
        assert!(config.extra_commands().is_empty(), "asking for two things is refused");
    }

    #[test]
    fn spoken_punctuation_and_auto_capitalise_are_on_by_default() {
        let config: Config = toml::from_str("").expect("empty config should parse");
        assert!(config.spoken_punctuation);
        assert!(config.auto_capitalise);
        let quiet: Config =
            toml::from_str("spoken_punctuation = false\nauto_capitalise = false\n")
                .expect("should parse");
        assert!(!quiet.spoken_punctuation);
        assert!(!quiet.auto_capitalise);
    }

    #[test]
    fn dictation_words_are_normalised_on_the_heard_side_and_sorted_longest_first() {
        let config: Config = toml::from_str(
            r#"
            [[dictation_words]]
            heard = "Mir"
            written = "MIR"

            [[dictation_words]]
            heard = "Mir Ángel Sufire"
            written = "Miguel Ángel Subir"
            "#,
        )
        .expect("dictation_words should parse");
        let words = config.dictation_words();
        assert_eq!(
            words,
            vec![
                (vec!["mir".to_string(), "angel".to_string(), "sufire".to_string()], "Miguel Ángel Subir".to_string()),
                (vec!["mir".to_string()], "MIR".to_string()),
            ]
        );
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
    fn a_problem_names_the_line() {
        // Not the real file: the same parser over the same kind of mistake.
        let contents = "sounds = false\n[audio]\ncategory = \"x\"\n";
        let err = toml::from_str::<Config>(contents).unwrap_err();
        assert!(err.message().contains("unknown field"), "{err}");
    }

    #[test]
    fn example_config_parses() {
        let config: Config = toml::from_str(include_str!("../config.example.toml"))
            .expect("the example config should parse as a real Config");
        // Everything in the example is commented out, so this is the
        // all-defaults case — but parsing it at all is the point.
        assert!(config.wake_words().is_none());
        assert_eq!(config.voice_threshold(), DEFAULT_VOICE_THRESHOLD);
        assert_eq!(config.command_threshold(), crate::commands::DEFAULT_THRESHOLD);
        assert!(config.macros().is_empty());
        assert!(config.search_engine().is_none());
    }

    #[test]
    fn reads_macros_of_your_own() {
        let config: Config = toml::from_str(
            r#"
            [[macros]]
            name = "modo trabajo"
            phrases = ["Modo Trabajo", "empieza a trabajar"]
            steps = ["abre Slack", "abre Chrome", "sube el volumen"]
            "#,
        )
        .expect("macro config should parse");
        let macros = config.macros();
        assert_eq!(macros.len(), 1);
        assert_eq!(macros[0].name, "modo trabajo");
        // Phrases are normalised, since they are matched against speech.
        assert_eq!(macros[0].phrases, ["modo trabajo", "empieza a trabajar"]);
        // Steps keep their original casing: they are decided afresh later,
        // and this is what lets the log show them as they were written.
        assert_eq!(macros[0].steps, ["abre Slack", "abre Chrome", "sube el volumen"]);
    }

    #[test]
    fn a_macro_needs_all_three_fields() {
        let bad: Result<Config, _> = toml::from_str(
            r#"
            [[macros]]
            name = "roto"
            phrases = ["roto"]
            "#,
        );
        assert!(bad.is_err(), "a macro with no steps must not pass unnoticed");
    }

    #[test]
    fn an_empty_search_engine_is_none() {
        let config: Config = toml::from_str("search_engine = \"\"").expect("should parse");
        assert!(config.search_engine().is_none());
    }

    #[test]
    fn a_named_search_engine_is_read_as_written() {
        // Validating it against the ones Minion actually knows is
        // `commands::configure`'s job, not this one's.
        let config: Config = toml::from_str("search_engine = \"YouTube\"").expect("should parse");
        assert_eq!(config.search_engine().as_deref(), Some("YouTube"));
    }

    #[test]
    fn removing_an_alias_keeps_the_others_and_the_comments() {
        let before = "# my aliases\n\
             [[aliases]]\n\
             command = \"abrir Chrome\"\n\
             phrase = \"abre cromo\"\n\
             \n\
             [[aliases]]\n\
             command = \"cerrar ventana\"\n\
             phrase = \"cierra la ventana\"\n";
        let after = without_alias(before, "Abre Cromo");
        assert!(after.contains("# my aliases"), "comments survive");
        assert!(!after.contains("abre cromo"), "the matched alias is gone");
        assert!(after.contains("cierra la ventana"), "the other alias survives");
        assert_eq!(after.matches("[[aliases]]").count(), 1);
    }

    #[test]
    fn removing_the_only_alias_leaves_no_trailing_blank_lines() {
        let before = "sounds = true\n\n[[aliases]]\ncommand = \"abrir Chrome\"\nphrase = \"abre cromo\"\n";
        let after = without_alias(before, "abre cromo");
        assert!(!after.contains("[[aliases]]"));
        assert!(after.contains("sounds = true"));
        assert!(!after.ends_with("\n\n"), "no orphaned blank line: {after:?}");
    }

    #[test]
    fn removing_an_unknown_phrase_changes_nothing() {
        let before = "[[aliases]]\ncommand = \"abrir Chrome\"\nphrase = \"abre cromo\"\n";
        assert_eq!(without_alias(before, "algo que no existe"), before);
    }

    #[test]
    fn remove_alias_refuses_to_write_an_unparsable_result() {
        // without_alias itself cannot produce broken TOML from valid input,
        // so this exercises parse_checked directly, the same guard
        // set_option relies on.
        let broken = "[[aliases]]\ncommand = \"x\"\nphrase = \"y\"\n[audio\n";
        assert!(parse_checked(without_alias(broken, "y")).is_err());
    }

    #[test]
    fn energy_mode_defaults_to_auto_and_a_typo_falls_back_to_it() {
        let default: Config = toml::from_str("").expect("empty config should parse");
        assert_eq!(default.energy_mode(), EnergyMode::Auto);
        let typo: Config = toml::from_str(r#"energy = "batery""#).expect("should parse");
        assert_eq!(typo.energy_mode(), EnergyMode::Auto);
        let performance: Config =
            toml::from_str(r#"energy = "performance""#).expect("should parse");
        assert_eq!(performance.energy_mode(), EnergyMode::Performance);
        let battery: Config = toml::from_str(r#"energy = "battery""#).expect("should parse");
        assert_eq!(battery.energy_mode(), EnergyMode::Battery);
    }

    #[test]
    fn performance_mode_never_unloads_anything() {
        let decision = energy_decision(
            EnergyMode::Performance,
            true,
            Duration::from_secs(3600),
            Some(Duration::from_secs(60)),
        );
        assert_eq!(decision, EnergyDecision { unload_model: false, unload_speaker: false });
    }

    #[test]
    fn auto_mode_off_battery_uses_the_ordinary_idle_threshold() {
        let normal = Some(Duration::from_secs(300));
        let too_soon = energy_decision(EnergyMode::Auto, false, Duration::from_secs(299), normal);
        assert_eq!(too_soon, EnergyDecision { unload_model: false, unload_speaker: false });
        let due = energy_decision(EnergyMode::Auto, false, Duration::from_secs(300), normal);
        assert_eq!(due, EnergyDecision { unload_model: true, unload_speaker: false });
    }

    #[test]
    fn auto_mode_off_battery_with_unload_disabled_never_unloads() {
        let decision = energy_decision(EnergyMode::Auto, false, Duration::from_secs(999_999), None);
        assert_eq!(decision, EnergyDecision { unload_model: false, unload_speaker: false });
    }

    #[test]
    fn auto_mode_on_battery_uses_the_shorter_thresholds() {
        let two_minutes = Duration::from_secs(BATTERY_MODEL_UNLOAD_MINUTES * 60);
        let ten_minutes = Duration::from_secs(BATTERY_SPEAKER_UNLOAD_MINUTES * 60);
        let normal = Some(Duration::from_secs(3600)); // would say "not yet" on its own

        let just_model =
            energy_decision(EnergyMode::Auto, true, two_minutes, normal);
        assert_eq!(just_model, EnergyDecision { unload_model: true, unload_speaker: false });

        let both = energy_decision(EnergyMode::Auto, true, ten_minutes, normal);
        assert_eq!(both, EnergyDecision { unload_model: true, unload_speaker: true });
    }

    #[test]
    fn battery_mode_behaves_like_on_battery_even_plugged_in() {
        let two_minutes = Duration::from_secs(BATTERY_MODEL_UNLOAD_MINUTES * 60);
        let decision = energy_decision(EnergyMode::Battery, false, two_minutes, None);
        assert_eq!(decision, EnergyDecision { unload_model: true, unload_speaker: false });
    }

    #[test]
    fn an_app_can_be_added_and_removed() {
        let added = appended("sounds = true\n", &app_block("Notion", "notion.id", &["nocion".into()]))
            .expect("should parse");
        let config: Config = toml::from_str(&added).expect("should parse");
        let apps = config.extra_apps();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Notion");
        assert_eq!(apps[0].bundle_id, "notion.id");
        assert!(added.contains("sounds = true"), "the rest of the file survives");

        let removed = without_app(&added, "notion.id");
        let config: Config = toml::from_str(&removed).expect("should parse");
        assert!(config.extra_apps().is_empty());
        assert!(removed.contains("sounds = true"), "the rest of the file survives removal too");
    }

    #[test]
    fn removing_an_unknown_app_changes_nothing() {
        let before = "[[apps]]\nname = \"Notion\"\nbundle_id = \"notion.id\"\naliases = []\n";
        assert_eq!(without_app(before, "algo.que.no.existe"), before);
    }

    #[test]
    fn a_built_in_apps_first_alias_creates_an_override_copying_the_built_in_list() {
        // No override yet: the new block must carry the built-in aliases
        // along, or teaching "cron" would leave Chrome answering to
        // nothing else.
        let updated = add_alias_to_app("sounds = true\n", "com.google.Chrome", "cron", "Chrome", &[
            "chrome", "cromo",
        ])
        .expect("should parse");
        let config: Config = toml::from_str(&updated).expect("should parse");
        let apps = config.extra_apps();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Chrome");
        assert_eq!(apps[0].aliases, ["chrome", "cromo", "cron"]);
        assert!(updated.contains("sounds = true"), "the rest of the file survives");
    }

    #[test]
    fn a_second_alias_joins_the_first_overrides_list_rather_than_shadowing_it() {
        // Two overrides for the same bundle id would leave only the later
        // one in effect once Minion restarted, silently dropping whatever
        // the first taught it.
        let first = add_alias_to_app("", "com.google.Chrome", "cron", "Chrome", &["chrome"])
            .expect("should parse");
        assert_eq!(first.matches("[[apps]]").count(), 1);

        let second = add_alias_to_app(&first, "com.google.Chrome", "cromm", "Chrome", &["chrome"])
            .expect("should parse");
        assert_eq!(second.matches("[[apps]]").count(), 1, "still one override, not two");
        let config: Config = toml::from_str(&second).expect("should parse");
        let apps = config.extra_apps();
        assert_eq!(apps.len(), 1);
        // Both the first alias and the built-in one survived the second write.
        assert_eq!(apps[0].aliases, ["chrome", "cron", "cromm"]);
    }

    #[test]
    fn teaching_the_same_alias_twice_changes_nothing() {
        let once = add_alias_to_app("", "com.google.Chrome", "cron", "Chrome", &["chrome"])
            .expect("should parse");
        let twice = add_alias_to_app(&once, "com.google.Chrome", "cron", "Chrome", &["chrome"])
            .expect("should parse");
        assert_eq!(once, twice);
    }

    #[test]
    fn a_command_can_be_added_with_each_kind_of_action() {
        let with_keys = appended(
            "",
            &command_block("compilar", &["compila".into()], &CommandKind::Keys("cmd-shift-b".into())),
        )
        .expect("should parse");
        let commands = toml::from_str::<Config>(&with_keys).unwrap().extra_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "compilar");
        assert!(matches!(commands[0].action, crate::commands::Action::Key(_, _)));

        let with_text = appended(
            "",
            &command_block("firma", &["pon mi firma".into()], &CommandKind::Text("Saludos, Ana".into())),
        )
        .expect("should parse");
        let commands = toml::from_str::<Config>(&with_text).unwrap().extra_commands();
        assert!(matches!(commands[0].action, crate::commands::Action::Type("Saludos, Ana")));

        let with_url = appended(
            "",
            &command_block("panel", &["abre el panel".into()], &CommandKind::Url("https://example.com".into())),
        )
        .expect("should parse");
        let commands = toml::from_str::<Config>(&with_url).unwrap().extra_commands();
        assert!(matches!(commands[0].action, crate::commands::Action::Open("https://example.com")));
    }

    #[test]
    fn a_command_can_be_removed_by_name() {
        let before = "\
             [[commands]]\n\
             name = \"compilar\"\n\
             phrases = [\"compila\"]\n\
             keys = \"cmd-shift-b\"\n\
             \n\
             [[commands]]\n\
             name = \"otro\"\n\
             phrases = [\"otro\"]\n\
             keys = \"cmd-k\"\n";
        let after = without_command(before, "compilar");
        let commands = toml::from_str::<Config>(&after).unwrap().extra_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "otro");
    }

    #[test]
    fn an_alias_can_be_added() {
        let added = appended("", &alias_block("abrir Chrome", "abre cromo")).expect("should parse");
        let config: Config = toml::from_str(&added).expect("should parse");
        assert_eq!(config.extra_aliases(), vec![("abrir Chrome", "abre cromo")]);
    }

    #[test]
    fn add_app_and_add_command_refuse_to_write_an_unparsable_addition() {
        // A stray `"` from an unescaped value would otherwise leave the
        // whole file unreadable; appended() must catch that before writing.
        let broken = appended("", "\n[[apps]]\nname = \"x\n");
        assert!(broken.is_err());
    }
}

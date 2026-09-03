//! The optional AI layer: a large language model behind one small door.
//!
//! Minion's vocabulary is a table, and a table cannot answer «¿cuánto es el
//! 15 % de 340?» or guess what «pon la ventana a la derecha del todo» meant.
//! This module is where that goes when — and only when — the user turns it
//! on: `[ai] backend` is empty by default, and with it empty nothing here
//! ever runs.
//!
//! What is sent is **text**, never audio: the transcript Parakeet already
//! produced, which is the same thing that already goes to the log. Every
//! request is written down as `ai       «…» -> <backend> (1.8 s)` so the
//! log says exactly how often the microphone in the room turned into a
//! request over the network.
//!
//! Two kinds of backend sit behind [`Backend`]:
//!
//! * **HTTP** — OpenAI-compatible Chat Completions (`openai`, `deepseek`,
//!   `mistral`, `groq`, `openrouter`, `xai`, `gemini`, and the two local
//!   ones, `ollama` and `lmstudio`), plus Anthropic's own Messages API.
//!   See [`openai_compat`] and [`anthropic`].
//! * **CLI sessions** — the coding agents the user already pays for
//!   (Claude Code, Codex, Gemini CLI), driven over stdio. See [`cli`].
//!
//! There is no HTTP client crate here. Requests go through `/usr/bin/curl`,
//! the same way `models.rs` fetches the speech model: it is on every Mac,
//! it does TLS properly, and it saves a dependency tree bigger than the
//! rest of this program. The whole request — headers, key and body — is
//! handed to it as a config file **on stdin** (`curl --config -`), so the
//! API key never appears in `ps` output and never touches the disk.
//!
//! Context lives as long as the backend does: a warm Claude Code process
//! keeps its conversation, and both it and an HTTP backend's history are
//! dropped after `[ai] idle_minutes`, exactly like the speech model is
//! unloaded when nobody is talking. [`forget`] does the same on demand, for
//! a later «olvida la conversación».

// The engine is complete and proven from the command line (`minion ai`),
// but nothing inside the listening loop calls it yet: wiring it into
// questions, unknown phrases, the idle timer and the statistics report is
// a separate change. Until then, `ask_for_command`, `forget`,
// `unload_if_idle` and `requests_today` have no caller in the binary, and
// this keeps that from being fourteen warnings that hide a real one.
#![allow(dead_code)]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::note;

pub mod anthropic;
pub mod cli;
pub mod openai_compat;

/// How long an HTTP request may take.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one request to a CLI agent may take. Higher than the HTTP one
/// on purpose: these start a whole Node process on a cold call.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the authentication probe in [`detect`] may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Requests per day when the file says nothing.
const DEFAULT_DAILY_LIMIT: u32 = 200;

/// Minutes of not being asked anything before a backend is dropped.
const DEFAULT_IDLE_MINUTES: u64 = 10;

/// Turns of conversation an HTTP backend remembers. Each turn is a
/// question and an answer, and the whole history is re-sent every time, so
/// this is a bill as much as a memory.
const HISTORY_TURNS: usize = 8;

/// What Minion tells the model it is.
///
/// Spanish, and blunt about length: this is read out loud by the system
/// synthesiser, where a paragraph is a punishment and a Markdown bullet is
/// read as the word "asterisco".
pub const SYSTEM_PROMPT: &str = "Eres el cerebro de Minion, un asistente de voz en español. \
     Responde siempre en español, en texto plano, sin Markdown y sin emojis. \
     Sé breve: una o dos frases, porque tu respuesta se lee en voz alta. \
     Si no sabes algo, dilo en una frase.";

/// What the AI layer is being asked for, and the name the config file uses
/// for it in `[ai] use`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A question the built-in answers could not handle.
    Questions,
    /// A phrase the vocabulary did not recognise, to be turned into a
    /// command if the model can see one in it.
    Unknown,
}

impl Purpose {
    /// The word `[ai] use` lists it under.
    pub fn key(self) -> &'static str {
        match self {
            Purpose::Questions => "questions",
            Purpose::Unknown => "unknown",
        }
    }
}

/// Why an AI request produced nothing.
///
/// `Display` is in Spanish: every one of these ends up either spoken, shown
/// in a notification, or printed by `minion ai`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// No backend configured, or not configured for this purpose.
    Disabled,
    /// Today's request budget is spent.
    Budget,
    /// A backend is named but cannot be used: no key, no such preset, the
    /// CLI is not installed.
    NotConfigured(String),
    /// The backend answered, and the answer was a failure.
    Backend(String),
    /// It did not answer in time.
    Timeout,
    /// It answered with something this code could not read.
    Parse(String),
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::Disabled => write!(f, "La ayuda con IA está desactivada."),
            AiError::Budget => write!(f, "Se ha alcanzado el límite de peticiones de hoy."),
            AiError::NotConfigured(why) => write!(f, "La IA no está configurada: {why}"),
            AiError::Backend(why) => write!(f, "El servicio de IA ha fallado: {why}"),
            AiError::Timeout => write!(f, "El servicio de IA ha tardado demasiado."),
            AiError::Parse(why) => write!(f, "No se entendió la respuesta de la IA: {why}"),
        }
    }
}

impl std::error::Error for AiError {}

/// A command the model may suggest for a phrase the vocabulary missed.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The name of an existing command, exactly as the catalogue spells it.
    pub command: String,
    /// How sure the model says it is, 0 to 1.
    pub confidence: f32,
}

/// One line of the catalogue shown to the model when it is asked to match a
/// phrase to a command: the command's name and one way of saying it.
#[derive(Debug, Clone)]
pub struct CommandSummary {
    pub name: String,
    pub phrase: String,
}

/// One conversational turn, as both HTTP APIs happen to spell it.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: &'static str,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user", content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant", content: content.into() }
    }
}

/// Something that can be asked a question.
///
/// Deliberately tiny. Whatever a backend needs to keep — a child process, a
/// conversation history, a session id — it keeps itself; the engine above
/// only knows how to ask, how to make it forget, and what to call it in the
/// log.
pub trait Backend: Send {
    /// What the log calls it.
    fn name(&self) -> &str;

    /// Asks, remembering the exchange for the next call.
    fn ask(&mut self, prompt: &str, system: &str) -> Result<String, AiError>;

    /// Asks outside the conversation: nothing before it is visible to the
    /// model, and nothing about it is remembered afterwards. This is what
    /// [`ask_for_command`] uses, so a one-off "which command is this?"
    /// cannot leak into the next spoken question.
    fn ask_once(&mut self, prompt: &str, system: &str) -> Result<String, AiError>;

    /// Drops the conversation and any warm process behind it.
    fn forget(&mut self);
}

/// The `[ai]` settings, resolved once and copied here so the engine does
/// not hold a borrow of the whole `Config`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub backend: String,
    pub model: String,
    pub uses: Vec<String>,
    pub daily_limit: u32,
    pub idle: Duration,
    pub api_key: String,
    pub base_url: String,
}

impl Settings {
    /// Reads the `[ai]` table, filling in the defaults.
    pub fn from_config(config: &crate::config::AiConfig) -> Self {
        Self {
            backend: config.backend.trim().to_string(),
            model: config.model.trim().to_string(),
            uses: config.uses.clone(),
            daily_limit: config.daily_limit.unwrap_or(DEFAULT_DAILY_LIMIT),
            idle: Duration::from_secs(config.idle_minutes.unwrap_or(DEFAULT_IDLE_MINUTES) * 60),
            api_key: config.api_key.trim().to_string(),
            base_url: config.base_url.trim().to_string(),
        }
    }

    /// Whether anything at all is turned on.
    pub fn enabled(&self) -> bool {
        !self.backend.is_empty()
    }

    /// Whether this purpose is one of the things `[ai] use` allows.
    pub fn allows(&self, purpose: Purpose) -> bool {
        self.uses.iter().any(|use_| use_.trim().eq_ignore_ascii_case(purpose.key()))
    }
}

/// The settings, as last read from the config file.
static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);

/// The live backend, if one has been built and is still warm.
static ENGINE: Mutex<Option<Warm>> = Mutex::new(None);

/// A backend and when it was last useful.
struct Warm {
    backend: Box<dyn Backend>,
    /// Which `[ai] backend` value built it, so a settings change replaces
    /// it rather than being ignored until the next restart.
    built_for: Settings,
    last_used: Instant,
}

/// Reads `[ai]` out of the configuration. Called once at startup, and
/// again by anything that reloads the config.
pub fn configure(config: &crate::config::Config) {
    let settings = Settings::from_config(&config.ai);
    if let Ok(mut held) = SETTINGS.lock() {
        *held = Some(settings);
    }
}

/// The settings in force, or the defaults (which mean "off").
fn settings() -> Settings {
    SETTINGS.lock().ok().and_then(|held| held.clone()).unwrap_or_default()
}

/// Drops the conversation: the next question starts from nothing.
///
/// For «olvida la conversación», and for the idle timer.
pub fn forget() {
    if let Ok(mut held) = ENGINE.lock() {
        if let Some(warm) = held.as_mut() {
            warm.backend.forget();
        }
        *held = None;
    }
}

/// Drops the backend if nothing has been asked of it for `[ai] idle_minutes`.
///
/// Meant for the same once-a-second timer that already unloads the speech
/// model. Cheap when there is nothing to drop.
pub fn unload_if_idle() {
    let idle = settings().idle;
    if idle.is_zero() {
        return;
    }
    let expired = ENGINE
        .lock()
        .ok()
        .and_then(|held| held.as_ref().map(|warm| warm.last_used.elapsed() >= idle))
        .unwrap_or(false);
    if expired {
        note!("ai       conversación olvidada tras {} min sin usarse", idle.as_secs() / 60);
        forget();
    }
}

/// Builds the backend `[ai] backend` names.
fn build(settings: &Settings) -> Result<Box<dyn Backend>, AiError> {
    let name = settings.backend.to_ascii_lowercase();
    match name.as_str() {
        "claude-code" | "claude" => Ok(Box::new(cli::claude_code(&settings.model))),
        "codex" => Ok(Box::new(cli::codex(&settings.model))),
        "gemini-cli" => Ok(Box::new(cli::gemini_cli(&settings.model))),
        "anthropic" => {
            let key = api_key(settings)?;
            Ok(Box::new(anthropic::Anthropic::new(key, &settings.model, &settings.base_url)))
        }
        _ => {
            let preset = openai_compat::preset(&name).ok_or_else(|| {
                AiError::NotConfigured(format!("no conozco el backend «{}»", settings.backend))
            })?;
            // The two local servers answer without a key; everything else
            // needs one before it is worth opening a socket.
            let key = if preset.needs_key { api_key(settings)? } else { String::new() };
            Ok(Box::new(openai_compat::OpenAiCompatible::new(
                preset,
                key,
                &settings.model,
                &settings.base_url,
            )))
        }
    }
}

/// The API key for the configured backend: either written in the config
/// file, or — with `api_key = "keychain"` — read out of the macOS keychain.
fn api_key(settings: &Settings) -> Result<String, AiError> {
    if settings.api_key.is_empty() {
        return Err(AiError::NotConfigured(
            "falta «api_key» en la sección [ai] de config.toml".into(),
        ));
    }
    if settings.api_key != "keychain" {
        return Ok(settings.api_key.clone());
    }
    keychain_key(&settings.backend).ok_or_else(|| {
        AiError::NotConfigured(format!(
            "no hay clave en el llavero para «{}»; guárdala con «minion ai set-key {}»",
            settings.backend, settings.backend
        ))
    })
}

/// The keychain service name every key is filed under.
const KEYCHAIN_SERVICE: &str = "minion-ai";

/// Where API keys are kept, as a pair of functions rather than a direct
/// call to `security`.
///
/// The indirection exists for one reason: **a test must never touch the
/// real keychain**. It is the user's, it is shared by every program on the
/// machine, and reaching it from a test run puts a macOS dialog on the
/// screen of whoever is running `cargo test`. Same rule as
/// `save_profile_for` and the log writer — see CLAUDE.md.
pub struct Keychain {
    pub read: fn(&str) -> Option<String>,
    pub write: fn(&str, &str) -> Result<(), String>,
}

/// The one in force: the real keychain outside tests, an in-memory map
/// inside them.
fn keychain() -> Keychain {
    if cfg!(test) {
        Keychain { read: fake_keychain::read, write: fake_keychain::write }
    } else {
        Keychain { read: security_read, write: security_write }
    }
}

/// Reads one key out of the login keychain.
///
/// `security` prints the key on stdout and nothing else; a missing entry is
/// a non-zero exit, which is `None` here. Never logged, on the way in or
/// on the way out.
fn security_read(provider: &str) -> Option<String> {
    let output = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", provider, "-w"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let key = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

/// Writes one key into the login keychain.
fn security_write(provider: &str, key: &str) -> Result<(), String> {
    let status = std::process::Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-U", // replace an existing entry rather than refusing
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            provider,
            "-w",
            key,
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("el llavero rechazó la clave".into())
    }
}

/// The keychain a test run gets: a map in this process, and nothing else.
#[cfg(test)]
mod fake_keychain {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static STORED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    pub fn read(provider: &str) -> Option<String> {
        STORED.lock().ok()?.as_ref()?.get(provider).cloned()
    }

    pub fn write(provider: &str, key: &str) -> Result<(), String> {
        let mut held = STORED.lock().map_err(|_| "llavero bloqueado".to_string())?;
        held.get_or_insert_with(HashMap::new).insert(provider.to_string(), key.to_string());
        Ok(())
    }
}

/// Reads one key, through whichever keychain is in force.
fn keychain_key(provider: &str) -> Option<String> {
    (keychain().read)(provider)
}

/// Stores a key for `provider`, read from stdin.
///
/// From stdin and never from `argv`: an argument is visible to every other
/// process on the machine through `ps`, and lands in the shell's history
/// besides.
pub fn set_key_from_stdin(provider: &str) -> Result<(), String> {
    use std::io::Read;
    let mut key = String::new();
    std::io::stdin().read_to_string(&mut key).map_err(|e| e.to_string())?;
    set_key(provider, key.trim())
}

/// Stores a key. Split from [`set_key_from_stdin`] so the storing can be
/// tested without a fake stdin.
fn set_key(provider: &str, key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("no se ha leído ninguna clave".into());
    }
    (keychain().write)(provider, key)
}

/// Asks the configured backend a question.
///
/// The whole door: everything else in Minion that wants the AI goes
/// through here or through [`ask_for_command`]. Refuses before spending
/// anything when the layer is off, when `[ai] use` does not list this
/// purpose, or when today's budget is gone.
pub fn ask(text: &str, purpose: Purpose) -> Result<String, AiError> {
    let settings = settings();
    if !settings.enabled() || !settings.allows(purpose) {
        return Err(AiError::Disabled);
    }
    check_budget()?;
    let started = Instant::now();
    let answer = with_backend(&settings, |backend| backend.ask(text, SYSTEM_PROMPT));
    if !never_sent(&answer) {
        count_request();
    }
    log_request(text, &settings.backend, started, &answer);
    answer
}

/// Asks the model which known command a phrase was meant to be.
///
/// Answers with strict JSON, on purpose: the caller needs a name it can
/// look up and a number it can threshold, not a sentence. Outside the
/// conversation ([`Backend::ask_once`]) so a stray "no sé" here does not
/// become context for the next thing said out loud. Anything unusable —
/// no answer, a name that is not in the catalogue, unreadable JSON —
/// comes back as `None`; this is a suggestion, and a bad one is worse
/// than none.
pub fn ask_for_command(transcript: &str, catalogue: &[CommandSummary]) -> Option<Suggestion> {
    let settings = settings();
    if !settings.enabled() || !settings.allows(Purpose::Unknown) || catalogue.is_empty() {
        return None;
    }
    if check_budget().is_err() {
        return None;
    }
    let prompt = command_prompt(transcript, catalogue);
    let started = Instant::now();
    let answer = with_backend(&settings, |backend| backend.ask_once(&prompt, COMMAND_SYSTEM_PROMPT));
    if !never_sent(&answer) {
        count_request();
    }
    log_request(transcript, &settings.backend, started, &answer);
    let suggestion = parse_suggestion(&answer.ok()?)?;
    // A name the vocabulary does not have is not a command, however sure
    // the model sounds about it.
    catalogue
        .iter()
        .any(|entry| entry.name == suggestion.command)
        .then_some(suggestion)
}

/// What the model is told when it is matching a phrase to a command.
const COMMAND_SYSTEM_PROMPT: &str =
    "Eres un clasificador. Respondes únicamente con JSON, sin texto alrededor y sin bloques de \
     código. No expliques nada.";

/// The question itself: the phrase, the catalogue, and the exact shape the
/// answer has to take.
fn command_prompt(transcript: &str, catalogue: &[CommandSummary]) -> String {
    let mut list = String::new();
    for entry in catalogue {
        list.push_str(&format!("- {} (por ejemplo: «{}»)\n", entry.name, entry.phrase));
    }
    format!(
        "Un reconocedor de voz en español ha oído esto y no lo ha entendido:\n\n«{transcript}»\n\n\
         Estos son los comandos que existen:\n{list}\n\
         Si la frase era claramente uno de ellos, responde exactamente con\n\
         {{\"command\": \"<el nombre exacto de la lista>\", \"confidence\": <0.0 a 1.0>}}\n\
         Si no era ninguno, responde exactamente con null."
    )
}

/// Reads the model's answer to [`command_prompt`].
///
/// Robust on purpose: a model told to answer with bare JSON will still
/// wrap it in ```json fences, or put a sentence in front of it. This
/// finds the first balanced object in the text and reads that, and treats
/// a bare `null` — the honest "none of these" — as no suggestion.
pub fn parse_suggestion(answer: &str) -> Option<Suggestion> {
    let text = strip_code_fences(answer);
    if text.trim().trim_end_matches('.').eq_ignore_ascii_case("null") {
        return None;
    }
    let object = first_json_object(text)?;
    let value: serde_json::Value = serde_json::from_str(object).ok()?;
    let command = value.get("command")?.as_str()?.trim().to_string();
    if command.is_empty() {
        return None;
    }
    let confidence = value
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0) as f32;
    Some(Suggestion { command, confidence })
}

/// Drops a ```/```json fence around a block, if there is one.
pub fn strip_code_fences(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // The opening fence may carry a language ("```json"); the rest of that
    // line is never part of the payload.
    let body = rest.find('\n').map_or("", |newline| &rest[newline + 1..]);
    match body.rfind("```") {
        Some(end) => body[..end].trim(),
        None => body.trim(),
    }
}

/// The first `{…}` in the text, balanced, ignoring braces inside strings.
///
/// A model that adds "Claro, aquí lo tienes:" before the JSON is still
/// answering the question.
fn first_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for index in start..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Runs `work` against the live backend, building or rebuilding it first.
fn with_backend<T>(
    settings: &Settings,
    work: impl FnOnce(&mut Box<dyn Backend>) -> Result<T, AiError>,
) -> Result<T, AiError> {
    let mut held = ENGINE.lock().map_err(|_| AiError::Backend("estado bloqueado".into()))?;
    let stale = held.as_ref().is_some_and(|warm| {
        &warm.built_for != settings || (!settings.idle.is_zero() && warm.last_used.elapsed() >= settings.idle)
    });
    if stale {
        if let Some(warm) = held.as_mut() {
            warm.backend.forget();
        }
        *held = None;
    }
    if held.is_none() {
        *held = Some(Warm {
            backend: build(settings)?,
            built_for: settings.clone(),
            last_used: Instant::now(),
        });
    }
    let warm = held.as_mut().expect("just built");
    warm.last_used = Instant::now();
    let result = work(&mut warm.backend);
    warm.last_used = Instant::now();
    result
}

/// Writes the one log line every AI request gets.
///
/// The transcript goes in verbatim, like every other line in this log:
/// knowing what was sent over the network is the entire point of writing
/// it down.
fn log_request<T>(text: &str, backend: &str, started: Instant, result: &Result<T, AiError>) {
    let seconds = started.elapsed().as_secs_f32();
    match result {
        Ok(_) => note!("ai       «{text}»  ->  {backend} ({seconds:.1} s)"),
        Err(why) => note!("ai       «{text}»  ->  {backend} falló: {why} ({seconds:.1} s)"),
    }
}

// ---------------------------------------------------------------- budget

/// Where the day's request count is kept: next to `config.toml`, in the
/// directory that is already this user's alone.
fn usage_path() -> Option<std::path::PathBuf> {
    crate::config::path().and_then(|path| path.parent().map(|dir| dir.join("ai-usage.toml")))
}

/// What has been spent today, as it sits on disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Usage {
    /// The day the count belongs to, as `YYYY-MM-DD`.
    pub day: String,
    pub count: u32,
}

/// Reads the file, without deciding anything about it.
fn read_usage() -> Usage {
    let Some(path) = usage_path() else { return Usage::default() };
    let Ok(contents) = std::fs::read_to_string(path) else { return Usage::default() };
    parse_usage(&contents)
}

/// Parses `ai-usage.toml`. Anything unreadable counts as a fresh day: a
/// corrupt counter must not be able to lock the feature out for good.
pub fn parse_usage(contents: &str) -> Usage {
    #[derive(serde::Deserialize)]
    struct Stored {
        #[serde(default)]
        day: String,
        #[serde(default)]
        count: u32,
    }
    toml::from_str::<Stored>(contents)
        .map(|stored| Usage { day: stored.day, count: stored.count })
        .unwrap_or_default()
}

/// The count after one more request on `today`, given what is on disk.
///
/// Pure, and separate from the file, so the midnight roll-over can be
/// tested by handing it two different days instead of waiting for one.
/// Over the limit it returns the usage unchanged, and `false`.
pub fn next_usage(stored: &Usage, today: &str, limit: u32) -> (Usage, bool) {
    if stored.day != today {
        return (Usage { day: today.to_string(), count: 1 }, true);
    }
    if limit > 0 && stored.count >= limit {
        return (stored.clone(), false);
    }
    (Usage { day: today.to_string(), count: stored.count + 1 }, true)
}

/// Today, as the counter file spells it.
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Refuses when today's budget is already spent. Counts nothing.
///
/// Separate from [`count_request`] so a request that never reaches a
/// backend at all — no key, no such preset, the agent is not installed —
/// does not cost one of the day's. Repeating a misconfigured question
/// twenty times should produce twenty complaints, not a spent budget.
fn check_budget() -> Result<(), AiError> {
    let limit = settings().daily_limit;
    let (_, allowed) = next_usage(&read_usage(), &today(), limit);
    if allowed {
        Ok(())
    } else {
        Err(AiError::Budget)
    }
}

/// Counts one request that actually reached a backend.
fn count_request() {
    let limit = settings().daily_limit;
    let (updated, allowed) = next_usage(&read_usage(), &today(), limit);
    if allowed {
        write_usage(&updated);
    }
}

/// Whether a failure means nothing was ever sent, and so nothing is owed.
fn never_sent<T>(result: &Result<T, AiError>) -> bool {
    matches!(result, Err(AiError::NotConfigured(_)))
}

/// Saves the counter. Never under test: the file belongs to the person
/// running Minion, exactly like the log and the voice profile.
fn write_usage(usage: &Usage) {
    if cfg!(test) {
        return;
    }
    let Some(path) = usage_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let contents = format!(
        "# Written by Minion. Counts AI requests so [ai] daily_limit can be\n\
         # enforced; resets on its own at the first request of a new day.\n\
         day = {}\ncount = {}\n",
        crate::config::toml_string(&usage.day),
        usage.count
    );
    let _ = std::fs::write(path, contents);
}

/// How many AI requests have been made today, for the statistics report.
pub fn requests_today() -> u32 {
    let usage = read_usage();
    if usage.day == today() {
        usage.count
    } else {
        0
    }
}

// ------------------------------------------------------------- detection

/// What is known about one CLI agent on this machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    /// The `[ai] backend` value that selects it.
    pub backend: &'static str,
    /// What it is called in the status report.
    pub label: &'static str,
    /// Where the binary is, if it is installed at all.
    pub path: Option<String>,
    /// Whether a real request to it came back with an answer.
    pub authenticated: bool,
    /// What it said, when it did not work.
    pub problem: Option<String>,
    /// How long the probe took.
    pub latency: Option<Duration>,
}

/// The detection result, kept for the life of the process: each probe is a
/// real request to a real service, and repeating it on every glance at the
/// menu would be both slow and billable.
static DETECTED: Mutex<Option<Vec<Detected>>> = Mutex::new(None);

/// Looks for each CLI agent, and asks the ones that are there to say "OK".
pub fn detect() -> Vec<Detected> {
    if let Ok(held) = DETECTED.lock() {
        if let Some(cached) = held.as_ref() {
            return cached.clone();
        }
    }
    let found: Vec<Detected> = cli::AGENTS.iter().map(cli::probe).collect();
    if let Ok(mut held) = DETECTED.lock() {
        *held = Some(found.clone());
    }
    found
}

/// The Spanish report `minion ai status` prints.
pub fn status_text() -> String {
    let settings = settings();
    let mut report = String::from("Minion — ayuda con IA\n\n");
    if settings.enabled() {
        report.push_str(&format!("Backend:   {}\n", settings.backend));
        let model = if settings.model.is_empty() { "(el del proveedor)" } else { &settings.model };
        report.push_str(&format!("Modelo:    {model}\n"));
        report.push_str(&format!("Se usa en: {}\n", settings.uses.join(", ")));
        report.push_str(&format!(
            "Peticiones hoy: {} de {}\n",
            requests_today(),
            settings.daily_limit
        ));
        report.push_str(&format!(
            "Olvida la conversación tras {} minutos sin usarse.\n",
            settings.idle.as_secs() / 60
        ));
    } else {
        report.push_str(
            "Desactivada. Para encenderla, pon «backend» en la sección [ai] de\n\
             config.toml (por ejemplo: backend = \"claude-code\").\n",
        );
    }
    report.push_str("\nAgentes de terminal en este Mac:\n");
    for found in detect() {
        report.push_str(&describe(&found));
    }
    report
}

/// One line of the status report, per agent.
fn describe(found: &Detected) -> String {
    let Some(path) = &found.path else {
        return format!("  {:<12} no instalado\n", found.label);
    };
    if found.authenticated {
        let millis = found.latency.map_or(0, |latency| latency.as_millis());
        format!("  {:<12} listo — {path} ({millis} ms)\n", found.label)
    } else {
        let why = found.problem.as_deref().unwrap_or("no responde");
        format!("  {:<12} instalado pero no utilizable — {path}\n               {why}\n", found.label)
    }
}

/// Answers one `minion ai "…"` from the terminal and prints the result.
///
/// The proof that the engine works without any of the voice machinery: it
/// reads the same config, spends from the same budget and writes the same
/// log line as a spoken question would.
pub fn run_from_terminal(text: &str) {
    match ask(text, Purpose::Questions) {
        Ok(answer) => println!("{answer}"),
        Err(why) => {
            eprintln!("{why}");
            std::process::exit(1);
        }
    }
}

// ------------------------------------------------------------------ curl

/// Sends one JSON request and returns the body that came back.
///
/// Everything — the URL, the headers, the key and the body — is written
/// into a curl config file handed over on **stdin**, so no part of it
/// reaches `argv` (where `ps` would show it to every process on the
/// machine) or the disk.
pub fn post_json(
    url: &str,
    headers: &[String],
    body: &str,
    timeout: Duration,
) -> Result<String, AiError> {
    use std::io::Write;

    let mut config = String::new();
    config.push_str("silent\nshow-error\n");
    config.push_str(&format!("max-time = {}\n", timeout.as_secs()));
    config.push_str("request = \"POST\"\n");
    config.push_str(&format!("url = {}\n", curl_quote(url)));
    for header in headers {
        config.push_str(&format!("header = {}\n", curl_quote(header)));
    }
    config.push_str(&format!("data-raw = {}\n", curl_quote(body)));

    let mut child = std::process::Command::new("/usr/bin/curl")
        .arg("--config")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AiError::Backend(format!("no se pudo ejecutar curl: {e}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| AiError::Backend("curl sin entrada".into()))?
        .write_all(config.as_bytes())
        .map_err(|e| AiError::Backend(e.to_string()))?;
    let output = child
        .wait_with_output()
        .map_err(|e| AiError::Backend(format!("curl falló: {e}")))?;
    if !output.status.success() {
        // 28 is curl's own timeout, and the one that happens in practice.
        if output.status.code() == Some(28) {
            return Err(AiError::Timeout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AiError::Backend(if stderr.is_empty() {
            "curl no pudo conectar".into()
        } else {
            stderr
        }));
    }
    String::from_utf8(output.stdout).map_err(|e| AiError::Parse(e.to_string()))
}

/// Renders a value as a quoted string for a curl config file.
///
/// Same reasoning as `config::toml_string`: the body is JSON full of
/// quotes and backslashes, and a bare `"{value}"` would end the string
/// at the first one.
pub fn curl_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// Keeps an HTTP backend's history down to the last few turns.
pub fn trim_history(history: &mut Vec<Message>) {
    let keep = HISTORY_TURNS * 2;
    if history.len() > keep {
        history.drain(..history.len() - keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str) -> CommandSummary {
        CommandSummary { name: name.into(), phrase: "algo".into() }
    }

    #[test]
    fn a_purpose_not_listed_in_use_is_refused() {
        let settings = Settings {
            backend: "claude-code".into(),
            uses: vec!["questions".into()],
            ..Settings::default()
        };
        assert!(settings.allows(Purpose::Questions));
        assert!(!settings.allows(Purpose::Unknown));
    }

    #[test]
    fn an_empty_backend_means_off() {
        assert!(!Settings::default().enabled());
    }

    #[test]
    fn plain_json_is_read() {
        let parsed = parse_suggestion(r#"{"command": "abrir Chrome", "confidence": 0.9}"#);
        assert_eq!(
            parsed,
            Some(Suggestion { command: "abrir Chrome".into(), confidence: 0.9 })
        );
    }

    #[test]
    fn fenced_json_is_read() {
        let parsed = parse_suggestion("```json\n{\"command\":\"cerrar pestaña\",\"confidence\":0.4}\n```");
        assert_eq!(parsed.unwrap().command, "cerrar pestaña");
    }

    #[test]
    fn json_with_prose_around_it_is_read() {
        let parsed = parse_suggestion(
            "Claro, aquí lo tienes:\n{\"command\": \"subir volumen\", \"confidence\": 1}\nEspero que ayude.",
        );
        assert_eq!(parsed.unwrap().command, "subir volumen");
    }

    #[test]
    fn null_is_no_suggestion() {
        assert_eq!(parse_suggestion("null"), None);
        assert_eq!(parse_suggestion("```json\nnull\n```"), None);
        assert_eq!(parse_suggestion("  null.  "), None);
    }

    #[test]
    fn nonsense_is_no_suggestion() {
        assert_eq!(parse_suggestion("no sé de qué me hablas"), None);
        assert_eq!(parse_suggestion("{\"command\": \"\"}"), None);
        assert_eq!(parse_suggestion("{unbalanced"), None);
    }

    #[test]
    fn a_brace_inside_a_string_does_not_end_the_object() {
        let parsed = parse_suggestion(r#"{"command": "escribe }", "confidence": 0.5}"#);
        assert_eq!(parsed.unwrap().command, "escribe }");
    }

    #[test]
    fn a_missing_confidence_is_zero_not_an_error() {
        let parsed = parse_suggestion(r#"{"command": "abrir Chrome"}"#).unwrap();
        assert_eq!(parsed.confidence, 0.0);
    }

    #[test]
    fn confidence_is_kept_inside_zero_and_one() {
        assert_eq!(parse_suggestion(r#"{"command":"x","confidence":7}"#).unwrap().confidence, 1.0);
        assert_eq!(parse_suggestion(r#"{"command":"x","confidence":-2}"#).unwrap().confidence, 0.0);
    }

    #[test]
    fn the_prompt_lists_every_command_by_name() {
        let prompt = command_prompt("abre cromo", &[summary("abrir Chrome"), summary("abrir Safari")]);
        assert!(prompt.contains("abre cromo"));
        assert!(prompt.contains("abrir Chrome"));
        assert!(prompt.contains("abrir Safari"));
    }

    #[test]
    fn the_first_request_of_a_new_day_starts_the_count_again() {
        let yesterday = Usage { day: "2026-09-02".into(), count: 200 };
        let (updated, allowed) = next_usage(&yesterday, "2026-09-03", 200);
        assert!(allowed);
        assert_eq!(updated, Usage { day: "2026-09-03".into(), count: 1 });
    }

    #[test]
    fn the_count_rises_within_the_day() {
        let stored = Usage { day: "2026-09-03".into(), count: 4 };
        let (updated, allowed) = next_usage(&stored, "2026-09-03", 200);
        assert!(allowed);
        assert_eq!(updated.count, 5);
    }

    #[test]
    fn the_limit_refuses_and_does_not_keep_counting() {
        let stored = Usage { day: "2026-09-03".into(), count: 200 };
        let (updated, allowed) = next_usage(&stored, "2026-09-03", 200);
        assert!(!allowed);
        assert_eq!(updated.count, 200);
    }

    #[test]
    fn a_zero_limit_is_no_limit() {
        let stored = Usage { day: "2026-09-03".into(), count: 9_000 };
        let (_, allowed) = next_usage(&stored, "2026-09-03", 0);
        assert!(allowed);
    }

    #[test]
    fn an_unreadable_counter_reads_as_empty_rather_than_locking_the_feature() {
        assert_eq!(parse_usage("this is not toml ]["), Usage::default());
        assert_eq!(
            parse_usage("day = \"2026-09-03\"\ncount = 7\n"),
            Usage { day: "2026-09-03".into(), count: 7 }
        );
    }

    #[test]
    fn a_key_goes_in_and_comes_back_out_without_touching_the_real_keychain() {
        assert_eq!(keychain_key("un-proveedor-de-prueba"), None);
        set_key("un-proveedor-de-prueba", "sk-secreto").unwrap();
        assert_eq!(keychain_key("un-proveedor-de-prueba").as_deref(), Some("sk-secreto"));
    }

    #[test]
    fn an_empty_key_is_refused_before_it_is_stored() {
        assert!(set_key("otro-proveedor-de-prueba", "").is_err());
        assert_eq!(keychain_key("otro-proveedor-de-prueba"), None);
    }

    #[test]
    fn a_backend_asking_for_the_keychain_with_nothing_in_it_says_so() {
        let settings = Settings {
            backend: "openai".into(),
            api_key: "keychain".into(),
            ..Settings::default()
        };
        assert!(matches!(api_key(&settings), Err(AiError::NotConfigured(_))));
    }

    #[test]
    fn a_request_that_never_left_the_machine_is_not_charged() {
        assert!(never_sent(&Result::<(), _>::Err(AiError::NotConfigured("sin clave".into()))));
        assert!(!never_sent(&Result::<(), _>::Err(AiError::Timeout)));
        assert!(!never_sent(&Result::<(), _>::Err(AiError::Backend("500".into()))));
        assert!(!never_sent(&Ok::<(), AiError>(())));
    }

    #[test]
    fn quoting_survives_quotes_and_backslashes() {
        assert_eq!(curl_quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(curl_quote("line\nbreak"), "\"line\\nbreak\"");
    }

    #[test]
    fn history_is_trimmed_to_the_last_turns() {
        let mut history: Vec<Message> = (0..40).map(|n| Message::user(n.to_string())).collect();
        trim_history(&mut history);
        assert_eq!(history.len(), HISTORY_TURNS * 2);
        assert_eq!(history.last().unwrap().content, "39");
    }

    #[test]
    fn an_unknown_backend_name_is_refused_before_anything_is_sent() {
        let settings = Settings { backend: "hal9000".into(), ..Settings::default() };
        let built = build(&settings);
        assert!(matches!(built, Err(AiError::NotConfigured(_))));
    }

    #[test]
    fn an_http_backend_without_a_key_is_refused_before_anything_is_sent() {
        let settings = Settings { backend: "openai".into(), ..Settings::default() };
        assert!(matches!(build(&settings), Err(AiError::NotConfigured(_))));
    }

    #[test]
    fn a_local_server_needs_no_key() {
        let settings = Settings { backend: "ollama".into(), ..Settings::default() };
        assert!(build(&settings).is_ok());
    }

    #[test]
    fn errors_are_written_in_spanish() {
        assert!(AiError::Budget.to_string().contains("límite"));
        assert!(AiError::Timeout.to_string().contains("demasiado"));
    }
}

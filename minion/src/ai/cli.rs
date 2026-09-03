//! The coding agents already installed on this Mac, used as a brain.
//!
//! The point of this module is that the user has already paid for one.
//! Claude Code, Codex and Gemini CLI all sit behind a subscription and all
//! speak some machine-readable protocol over stdio, so Minion can ask them
//! a question without an API key and without a second bill.
//!
//! ## What was actually observed (2026-09-03, on this machine)
//!
//! **Claude Code 2.1.259** — one long-lived process:
//!
//! ```text
//! claude -p --input-format stream-json --output-format stream-json --verbose
//! ```
//!
//! Each request is one line of NDJSON on stdin:
//!
//! ```json
//! {"type":"user","message":{"role":"user","content":[{"type":"text","text":"…"}]}}
//! ```
//!
//! and the reply arrives on stdout as several lines — `system`/`init`,
//! `rate_limit_event`, one or more `assistant` messages — closed by exactly
//! one line per request:
//!
//! ```json
//! {"type":"result","subtype":"success","is_error":false,"result":"OK", …}
//! ```
//!
//! `result` is the plain-text answer, which is all Minion wants, so the
//! `assistant` lines are skipped and only `type == "result"` is read. The
//! process keeps its conversation between requests — asking "¿qué te acabo
//! de preguntar?" as a second turn answered correctly — which is exactly
//! the context lifetime this feature wants. Measured: 3.3 s for the first
//! request of a session, 2.1 s for each one after it.
//!
//! The flags matter as much as the protocol. Plain `claude -p` loads this
//! user's whole working setup — MCP servers, plugins, skills, hooks, every
//! built-in tool — which for "¿cuánto es el 15 % de 340?" is several
//! seconds and a great deal of surface area. `--safe-mode` (all
//! customisations off, authentication untouched), `--strict-mcp-config`,
//! `--tools ""` (no tools at all) and `--no-session-persistence` cut it
//! down to a chat. `--bare` was considered and rejected: it refuses to read
//! the keychain, which is where the subscription's credentials are.
//!
//! **Codex 0.149.0** — `codex app-server` speaks JSON-RPC, and getting a
//! single question through it needs an initialise handshake, a thread, and
//! an event subscription. Per the brief, the per-call fallback is used
//! instead: `codex exec --json`, one process per question, no conversation
//! carried between them. Its stream, observed here:
//!
//! ```json
//! {"type":"thread.started","thread_id":"…"}
//! {"type":"turn.started"}
//! {"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"…"}}
//! {"type":"turn.completed", …}
//! ```
//!
//! and, when it fails, `{"type":"error","message":"…"}` followed by
//! `turn.failed`. On this machine every request fails that way: the CLI is
//! installed and signed in with a ChatGPT account, but every model it
//! offers is refused with *"is not supported when using Codex with a
//! ChatGPT account"*. The success shape above is therefore taken from the
//! protocol rather than seen here.
//!
//! **Gemini CLI 0.26.0** — `--experimental-acp` speaks the Agent Client
//! Protocol, same story; the fallback here is `-o json` with the question
//! on stdin. It is installed and has cached credentials, but the account
//! needs `GOOGLE_CLOUD_PROJECT` set before it will answer at all, so
//! nothing could be observed beyond the failure. Its success shape,
//! `{"response":"…"}`, is read leniently for that reason.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::{AiError, Backend, Detected, CLI_TIMEOUT, PROBE_TIMEOUT, SYSTEM_PROMPT};

/// One CLI agent Minion knows how to drive.
pub struct Agent {
    /// The `[ai] backend` value that selects it.
    pub backend: &'static str,
    /// What the status report calls it.
    pub label: &'static str,
    /// The binary's name.
    pub program: &'static str,
}

pub const AGENTS: &[Agent] = &[
    Agent { backend: "claude-code", label: "Claude Code", program: "claude" },
    Agent { backend: "codex", label: "Codex", program: "codex" },
    Agent { backend: "gemini-cli", label: "Gemini CLI", program: "gemini" },
];

/// The aliases `claude --model` accepts (`claude --help`), paired with
/// what the popup calls them.
pub const CLAUDE_CODE_MODELS: &[(&str, &str)] =
    &[("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")];

/// Gemini CLI's `-o json`/`--model` flag only takes these two.
pub const GEMINI_MODELS: &[&str] = &["gemini-2.5-pro", "gemini-2.5-flash"];

/// The `model` key of the user's own `~/.codex/config.toml`, read fresh
/// each time.
///
/// Codex has no closed list of model ids — `codex --help` documents
/// `-m/--model <MODEL>` as free text, and no subcommand enumerates what a
/// ChatGPT plan actually allows (the module documentation above shows one
/// getting refused at request time instead). So rather than guess a fixed
/// list, the popup is offered exactly the one id this Mac's own config
/// already names, labelled as such.
pub fn codex_configured_model() -> Option<String> {
    codex_configured_model_at(&codex_config_path()?)
}

fn codex_config_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::Path::new(&home).join(".codex/config.toml"))
}

/// Reads `model` out of a Codex config file at `path`. Split from
/// [`codex_configured_model`] so a test can hand it a temp file instead of
/// this machine's real `~/.codex/config.toml`.
pub fn codex_configured_model_at(path: &std::path::Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    codex_configured_model_from(&contents)
}

/// Parses the `model` key out of Codex config TOML. Pure, and public
/// mainly so a fixture string can be checked without touching a file at
/// all.
pub fn codex_configured_model_from(contents: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        model: String,
    }
    toml::from_str::<Partial>(contents)
        .ok()
        .map(|partial| partial.model)
        .filter(|model| !model.is_empty())
}

/// Where these tools install themselves, for when `PATH` is no help.
///
/// Minion is normally started by launchd, whose `PATH` is
/// `/usr/bin:/bin:/usr/sbin:/sbin` and contains none of them. Looking in
/// the places npm, Homebrew and Claude Code's own installer actually use is
/// the difference between "no instalado" and working.
const LIKELY_DIRECTORIES: &[&str] =
    &["/opt/homebrew/bin", "/usr/local/bin", "~/.local/bin", "~/.bun/bin", "~/.npm-global/bin"];

/// The full path to `program`, or `None` if it is not on this machine.
pub fn locate(program: &str) -> Option<String> {
    if let Ok(output) = Command::new("/usr/bin/which").arg(program).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    LIKELY_DIRECTORIES.iter().find_map(|directory| {
        let directory = directory.replacen('~', &home, 1);
        let candidate = std::path::Path::new(&directory).join(program);
        candidate.is_file().then(|| candidate.to_string_lossy().into_owned())
    })
}

// ------------------------------------------------------------ Claude Code

/// The flags that turn Claude Code into a chat: no customisations, no MCP
/// servers, no tools, no session files. See the module documentation for
/// why each one is here.
fn claude_arguments(model: &str) -> Vec<String> {
    let mut arguments: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--safe-mode",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--permission-prompts",
        "none",
        "--tools",
        "",
    ]
    .iter()
    .map(|argument| (*argument).to_string())
    .collect();
    if !model.is_empty() {
        arguments.push("--model".into());
        arguments.push(model.to_string());
    }
    arguments
}

/// A Claude Code process kept warm between questions.
pub struct ClaudeSession {
    program: String,
    arguments: Vec<String>,
    timeout: Duration,
    live: Option<Live>,
}

/// The running child and the thread reading its stdout.
///
/// stdout is read on a thread and delivered through a channel because a
/// pipe has no read timeout: without it, a Claude Code that hangs would
/// hang the listening thread with it, and an always-on microphone that has
/// stopped listening is the worst failure this program has.
struct Live {
    child: Child,
    lines: Receiver<String>,
}

/// Builds the backend `[ai] backend = "claude-code"` selects.
pub fn claude_code(model: &str) -> ClaudeSession {
    ClaudeSession {
        program: locate("claude").unwrap_or_else(|| "claude".into()),
        arguments: claude_arguments(model),
        timeout: CLI_TIMEOUT,
        live: None,
    }
}

impl ClaudeSession {
    /// Same session against another command — a fake child, in the tests.
    #[cfg(test)]
    fn with_command(program: &str, arguments: &[&str], timeout: Duration) -> Self {
        Self {
            program: program.into(),
            arguments: arguments.iter().map(|argument| (*argument).to_string()).collect(),
            timeout,
            live: None,
        }
    }

    /// Starts the process if it is not already running.
    fn start(&mut self, system: &str) -> Result<(), AiError> {
        if self.live.is_some() {
            return Ok(());
        }
        let mut arguments = self.arguments.clone();
        if !system.is_empty() && self.program.ends_with("claude") {
            arguments.push("--system-prompt".into());
            arguments.push(system.to_string());
        }
        let mut child = Command::new(&self.program)
            .args(&arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                AiError::NotConfigured(format!("no se pudo iniciar «{}»: {e}", self.program))
            })?;
        let stdout = child.stdout.take().ok_or_else(|| AiError::Backend("sin salida".into()))?;
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        self.live = Some(Live { child, lines });
        Ok(())
    }

    /// Sends one question and waits for its `result` line.
    fn exchange(&mut self, prompt: &str, system: &str) -> Result<String, AiError> {
        self.start(system)?;
        let timeout = self.timeout;
        let outcome = self.exchange_inner(prompt, timeout);
        // A session that timed out or broke is not worth keeping: its
        // stdin may be half-written and its next answer would belong to
        // this question.
        if outcome.is_err() {
            self.forget();
        }
        outcome
    }

    fn exchange_inner(&mut self, prompt: &str, timeout: Duration) -> Result<String, AiError> {
        let live = self.live.as_mut().ok_or_else(|| AiError::Backend("sin sesión".into()))?;
        let stdin = live.child.stdin.as_mut().ok_or_else(|| AiError::Backend("sin entrada".into()))?;
        stdin
            .write_all(user_message(prompt).as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| AiError::Backend(format!("no se pudo enviar la pregunta: {e}")))?;

        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(AiError::Timeout);
            }
            match live.lines.recv_timeout(left) {
                Ok(line) => {
                    if let Some(outcome) = parse_result_line(&line) {
                        return outcome;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Err(AiError::Timeout),
                // The child closed its stdout: it exited, and no answer is
                // coming.
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(AiError::Backend("el agente se cerró sin responder".into()))
                }
            }
        }
    }
}

/// One NDJSON line asking a question, newline included.
pub fn user_message(text: &str) -> String {
    let message = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": [{"type": "text", "text": text}]},
    });
    format!("{message}\n")
}

/// Reads one line of Claude Code's output stream.
///
/// `None` for every line that is not the end of a turn — `system`,
/// `assistant`, `rate_limit_event` and whatever is added next; this is a
/// protocol that grows, and a new event type must not be mistaken for an
/// answer.
pub fn parse_result_line(line: &str) -> Option<Result<String, AiError>> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("type")?.as_str()? != "result" {
        return None;
    }
    let text = value.get("result").and_then(serde_json::Value::as_str).unwrap_or("").trim();
    let failed = value.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false)
        || value.get("subtype").and_then(serde_json::Value::as_str) != Some("success");
    if failed {
        let why = if text.is_empty() { "el agente falló" } else { text };
        return Some(Err(AiError::Backend(why.to_string())));
    }
    if text.is_empty() {
        return Some(Err(AiError::Parse("respuesta vacía".into())));
    }
    Some(Ok(text.to_string()))
}

impl Backend for ClaudeSession {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn ask(&mut self, prompt: &str, system: &str) -> Result<String, AiError> {
        self.exchange(prompt, system)
    }

    /// Outside the conversation means outside the process: a second,
    /// short-lived session, so nothing about this question is in the warm
    /// one's context afterwards.
    fn ask_once(&mut self, prompt: &str, system: &str) -> Result<String, AiError> {
        let mut once = ClaudeSession {
            program: self.program.clone(),
            arguments: self.arguments.clone(),
            timeout: self.timeout,
            live: None,
        };
        let answer = once.exchange(prompt, system);
        once.forget();
        answer
    }

    fn forget(&mut self) {
        if let Some(mut live) = self.live.take() {
            // Closing stdin is the polite way out; killing is the one that
            // always works. Both, in that order.
            drop(live.child.stdin.take());
            let _ = live.child.kill();
            let _ = live.child.wait();
        }
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.forget();
    }
}

// ------------------------------------------------------- Codex and Gemini

/// An agent driven one process per question, with no conversation kept.
///
/// See the module documentation: both of their session protocols were
/// judged not worth reverse-engineering for a single question, so
/// [`Backend::ask`] and [`Backend::ask_once`] do the same thing here and
/// [`Backend::forget`] has nothing to forget.
pub struct OneShot {
    name: &'static str,
    program: String,
    arguments: Vec<String>,
    timeout: Duration,
    /// Reads the answer out of whatever the tool printed.
    read: fn(&str) -> Result<String, AiError>,
}

/// Builds the backend `[ai] backend = "codex"` selects.
pub fn codex(model: &str) -> OneShot {
    let mut arguments: Vec<String> = ["exec", "--json", "--skip-git-repo-check", "--ephemeral"]
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();
    // Codex is a coding agent and will happily run commands; this is a
    // voice assistant answering a question, so it gets no write access to
    // anything.
    arguments.push("--sandbox".into());
    arguments.push("read-only".into());
    if !model.is_empty() {
        arguments.push("--model".into());
        arguments.push(model.to_string());
    }
    // A bare `-` makes it read the prompt from stdin, which keeps the
    // transcript out of `argv` where `ps` would show it to every process
    // on the machine.
    arguments.push("-".into());
    OneShot {
        name: "codex",
        program: locate("codex").unwrap_or_else(|| "codex".into()),
        arguments,
        timeout: CLI_TIMEOUT,
        read: parse_codex_stream,
    }
}

/// Builds the backend `[ai] backend = "gemini-cli"` selects.
pub fn gemini_cli(model: &str) -> OneShot {
    // «default» prompts before any tool runs, which a plain question never
    // triggers; «plan» would be the read-only choice but needs Gemini's
    // experimental.plan setting and fails loudly without it.
    let mut arguments: Vec<String> = ["-o", "json", "--approval-mode", "default"]
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();
    if !model.is_empty() {
        arguments.push("--model".into());
        arguments.push(model.to_string());
    }
    OneShot {
        name: "gemini-cli",
        program: locate("gemini").unwrap_or_else(|| "gemini".into()),
        arguments,
        timeout: CLI_TIMEOUT,
        read: parse_gemini_output,
    }
}

impl OneShot {
    fn run(&self, prompt: &str, system: &str) -> Result<String, AiError> {
        // Neither tool takes a system prompt of its own, so it goes in
        // front of the question.
        let text = if system.is_empty() {
            prompt.to_string()
        } else {
            format!("{system}\n\n{prompt}")
        };
        let output = run_with_timeout(&self.program, &self.arguments, &text, self.timeout)?;
        (self.read)(&output)
    }
}

impl Backend for OneShot {
    fn name(&self) -> &str {
        self.name
    }

    fn ask(&mut self, prompt: &str, system: &str) -> Result<String, AiError> {
        self.run(prompt, system)
    }

    fn ask_once(&mut self, prompt: &str, system: &str) -> Result<String, AiError> {
        self.run(prompt, system)
    }

    fn forget(&mut self) {}
}

/// Runs a command with the prompt on stdin and a deadline, and returns
/// what it printed.
///
/// `wait_with_output` has no timeout of its own, so the child is polled
/// and killed if it outstays the deadline: an agent stuck on a login
/// prompt must not become a Minion stuck on an agent.
fn run_with_timeout(
    program: &str,
    arguments: &[String],
    stdin_text: &str,
    timeout: Duration,
) -> Result<String, AiError> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AiError::NotConfigured(format!("no se pudo iniciar «{program}»: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_text.as_bytes());
        let _ = stdin.flush();
    }
    let stdout = child.stdout.take().ok_or_else(|| AiError::Backend("sin salida".into()))?;
    let (sender, output) = channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let mut reader = BufReader::new(stdout);
        let _ = std::io::Read::read_to_string(&mut reader, &mut text);
        let _ = sender.send(text);
    });
    // stderr is read too: an agent that prints nothing on stdout usually
    // said why on stderr («This account requires setting
    // GOOGLE_CLOUD_PROJECT…»), and «no respondió nada» hides that.
    let (err_sender, errors) = channel();
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut text = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = std::io::Read::read_to_string(&mut reader, &mut text);
            let _ = err_sender.send(text);
        });
    }
    match output.recv_timeout(timeout) {
        Ok(text) => {
            let _ = child.wait();
            if text.trim().is_empty() {
                if let Ok(stderr) = errors.recv_timeout(Duration::from_secs(2)) {
                    if let Some(reason) = first_complaint(&stderr) {
                        return Err(AiError::Backend(reason));
                    }
                }
            }
            Ok(text)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(AiError::Timeout)
        }
    }
}

/// Reads Codex's JSONL stream: the last `agent_message` is the answer.
pub fn parse_codex_stream(output: &str) -> Result<String, AiError> {
    let mut answer: Option<String> = None;
    let mut problem: Option<String> = None;
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("item.completed") => {
                let Some(item) = value.get("item") else { continue };
                match item.get("type").and_then(serde_json::Value::as_str) {
                    Some("agent_message") => {
                        answer = item
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .map(|text| text.trim().to_string());
                    }
                    // Codex reports non-fatal trouble (an unknown model,
                    // say) as an error item mid-stream. Remembered, but
                    // only used if no answer arrives.
                    Some("error") => {
                        problem = item
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                    }
                    _ => {}
                }
            }
            Some("error") | Some("turn.failed") => {
                let message = value
                    .get("message")
                    .or_else(|| value.pointer("/error/message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("el turno falló");
                problem = Some(codex_reason(message));
            }
            _ => {}
        }
    }
    match answer {
        Some(text) if !text.is_empty() => Ok(text),
        _ => Err(AiError::Backend(problem.unwrap_or_else(|| "Codex no respondió nada".into()))),
    }
}

/// Codex wraps the API's own JSON error inside the string it reports.
/// Unwrapping it turns an unreadable blob into the one sentence that
/// matters.
fn codex_reason(message: &str) -> String {
    serde_json::from_str::<serde_json::Value>(message)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| message.to_string())
}

/// The first line of an agent's stderr that reads like a reason, minus
/// the stack trace and the log noise around it.
fn first_complaint(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("at ") && !line.starts_with('['))
        .map(|line| line.trim_start_matches("An unexpected critical error occurred:").trim())
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect())
}

/// Reads Gemini CLI's `-o json` output.
///
/// Lenient on purpose: nothing could be observed on this machine (the
/// account needs `GOOGLE_CLOUD_PROJECT`), so a plain-text answer is
/// accepted as well as the documented `{"response": "…"}`.
pub fn parse_gemini_output(output: &str) -> Result<String, AiError> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Err(AiError::Backend("Gemini no respondió nada".into()));
    }
    let Some(start) = trimmed.find('{') else {
        return Ok(trimmed.to_string());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&trimmed[start..]) else {
        return Ok(trimmed.to_string());
    };
    if let Some(message) = value
        .pointer("/error/message")
        .or_else(|| value.get("error"))
        .and_then(serde_json::Value::as_str)
    {
        return Err(AiError::Backend(message.to_string()));
    }
    value
        .get("response")
        .and_then(serde_json::Value::as_str)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| AiError::Parse(format!("respuesta inesperada: {trimmed}")))
}

// ------------------------------------------------------------- detection

/// What the probe asks. Short, so a working agent costs almost nothing to
/// find and an answer is unmistakable.
const PROBE_PROMPT: &str = "Responde solo con OK.";

/// Looks for one agent and, if it is there, asks it to say OK.
pub fn probe(agent: &Agent) -> Detected {
    let Some(path) = locate(agent.program) else {
        return Detected {
            backend: agent.backend,
            label: agent.label,
            path: None,
            authenticated: false,
            problem: None,
            latency: None,
        };
    };
    let started = Instant::now();
    let answer = probe_request(agent, &path);
    let latency = started.elapsed();
    Detected {
        backend: agent.backend,
        label: agent.label,
        path: Some(path),
        authenticated: answer.is_ok(),
        problem: answer.err().map(|why| why.to_string()),
        latency: Some(latency),
    }
}

/// The probe request itself, against whichever backend the agent is.
fn probe_request(agent: &Agent, path: &str) -> Result<String, AiError> {
    match agent.backend {
        "claude-code" => {
            let mut session = ClaudeSession {
                program: path.to_string(),
                arguments: claude_arguments(""),
                timeout: PROBE_TIMEOUT,
                live: None,
            };
            let answer = session.ask(PROBE_PROMPT, SYSTEM_PROMPT);
            session.forget();
            answer
        }
        "codex" => {
            let mut once = codex("");
            once.program = path.to_string();
            once.timeout = PROBE_TIMEOUT;
            once.ask(PROBE_PROMPT, "")
        }
        _ => {
            let mut once = gemini_cli("");
            once.program = path.to_string();
            once.timeout = PROBE_TIMEOUT;
            once.ask(PROBE_PROMPT, "")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for Claude Code: reads one request per line and answers
    /// with the same three-line shape the real one produces.
    const FAKE_AGENT: &str = r#"
        while IFS= read -r line; do
          printf '%s\n' '{"type":"system","subtype":"init","session_id":"x"}'
          printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"no leído"}]}}'
          printf '%s\n' '{"type":"rate_limit_event"}'
          printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"El 15 % de 340 es 51."}'
        done
    "#;

    /// Reads the request and never answers.
    const SILENT_AGENT: &str = "while IFS= read -r line; do sleep 30; done";

    /// Answers with a failed turn.
    const FAILING_AGENT: &str = r#"
        while IFS= read -r line; do
          printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"se acabó el crédito"}'
        done
    "#;

    fn fake(script: &str, timeout: Duration) -> ClaudeSession {
        ClaudeSession::with_command("/bin/sh", &["-c", script], timeout)
    }

    #[test]
    fn a_request_is_one_line_of_ndjson() {
        let line = user_message("¿qué hora es?");
        assert!(line.ends_with('\n'));
        assert_eq!(line.lines().count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["type"], "user");
        assert_eq!(parsed["message"]["role"], "user");
        assert_eq!(parsed["message"]["content"][0]["text"], "¿qué hora es?");
    }

    #[test]
    fn a_question_with_quotes_and_newlines_still_makes_one_line() {
        let line = user_message("dice \"hola\"\ny se va");
        assert_eq!(line.lines().count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["message"]["content"][0]["text"], "dice \"hola\"\ny se va");
    }

    #[test]
    fn only_the_result_line_is_an_answer() {
        assert!(parse_result_line(r#"{"type":"system","subtype":"init"}"#).is_none());
        assert!(parse_result_line(r#"{"type":"assistant","message":{}}"#).is_none());
        assert!(parse_result_line(r#"{"type":"rate_limit_event"}"#).is_none());
        assert!(parse_result_line("not json at all").is_none());
        assert!(parse_result_line("").is_none());
    }

    #[test]
    fn a_successful_result_is_the_answer() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"OK","duration_ms":4544}"#;
        assert_eq!(parse_result_line(line), Some(Ok("OK".to_string())));
    }

    #[test]
    fn a_failed_result_carries_its_reason() {
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom"}"#;
        assert_eq!(parse_result_line(line), Some(Err(AiError::Backend("boom".into()))));
    }

    #[test]
    fn the_framing_survives_the_noise_around_the_answer() {
        let mut session = fake(FAKE_AGENT, Duration::from_secs(10));
        assert_eq!(session.ask("¿cuánto es el 15 % de 340?", "").unwrap(), "El 15 % de 340 es 51.");
        session.forget();
    }

    #[test]
    fn a_warm_session_answers_a_second_question_too() {
        let mut session = fake(FAKE_AGENT, Duration::from_secs(10));
        assert!(session.ask("una", "").is_ok());
        assert!(session.ask("dos", "").is_ok());
        session.forget();
    }

    #[test]
    fn an_agent_that_never_answers_times_out_and_is_dropped() {
        let mut session = fake(SILENT_AGENT, Duration::from_millis(300));
        assert_eq!(session.ask("hola", ""), Err(AiError::Timeout));
        // Dropped, so the next question starts a process that is not
        // half-way through this one.
        assert!(session.live.is_none());
    }

    #[test]
    fn a_failing_agent_is_reported_not_retried_forever() {
        let mut session = fake(FAILING_AGENT, Duration::from_secs(10));
        assert_eq!(session.ask("hola", ""), Err(AiError::Backend("se acabó el crédito".into())));
    }

    #[test]
    fn a_missing_binary_is_a_configuration_problem_not_a_crash() {
        let mut session =
            ClaudeSession::with_command("/nonexistent/claude", &[], Duration::from_secs(1));
        assert!(matches!(session.ask("hola", ""), Err(AiError::NotConfigured(_))));
    }

    #[test]
    fn a_binary_that_is_not_here_is_not_located() {
        assert_eq!(locate("definitely-not-a-real-program-xyz"), None);
    }

    #[test]
    fn a_binary_that_is_here_is_located() {
        assert_eq!(locate("sh").as_deref(), Some("/bin/sh"));
    }

    #[test]
    fn detection_of_a_missing_agent_reports_it_without_probing() {
        let agent = Agent {
            backend: "claude-code",
            label: "Claude Code",
            program: "definitely-not-a-real-program-xyz",
        };
        let found = probe(&agent);
        assert_eq!(found.path, None);
        assert!(!found.authenticated);
        assert_eq!(found.latency, None);
    }

    #[test]
    fn the_claude_flags_leave_no_tools_and_no_customisations() {
        let arguments = claude_arguments("");
        assert!(arguments.iter().any(|argument| argument == "--safe-mode"));
        assert!(arguments.iter().any(|argument| argument == "--strict-mcp-config"));
        assert!(arguments.iter().any(|argument| argument == "--tools"));
        assert!(!arguments.iter().any(|argument| argument == "--bare"));
        assert!(!arguments.iter().any(|argument| argument == "--model"));
        assert!(claude_arguments("opus").iter().any(|argument| argument == "opus"));
    }

    #[test]
    fn codex_is_asked_read_only_and_through_stdin() {
        let backend = codex("");
        assert!(backend.arguments.iter().any(|argument| argument == "read-only"));
        assert_eq!(backend.arguments.last().unwrap(), "-");
    }

    #[test]
    fn the_codex_answer_is_the_agent_message() {
        let stream = concat!(
            r#"{"type":"thread.started","thread_id":"01a0"}"#,
            "\n",
            r#"{"type":"turn.started"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"El 15 % de 340 es 51."}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{}}"#,
            "\n",
        );
        assert_eq!(parse_codex_stream(stream).unwrap(), "El 15 % de 340 es 51.");
    }

    #[test]
    fn a_codex_failure_is_unwrapped_to_the_sentence_that_matters() {
        // Exactly what this machine produces today.
        let stream = concat!(
            r#"{"type":"thread.started","thread_id":"01a0"}"#,
            "\n",
            r#"{"type":"error","message":"{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-5.6-sol' model is not supported when using Codex with a ChatGPT account.\"}}"}"#,
            "\n",
        );
        let failure = parse_codex_stream(stream).unwrap_err();
        assert_eq!(
            failure,
            AiError::Backend(
                "The 'gpt-5.6-sol' model is not supported when using Codex with a ChatGPT account."
                    .into()
            )
        );
    }

    #[test]
    fn a_codex_stream_with_nothing_in_it_is_a_failure() {
        assert!(parse_codex_stream("").is_err());
    }

    /// The real thing, end to end, against the Claude Code on this
    /// machine — a real request against the user's own subscription.
    ///
    /// Ignored, so `cargo test` stays offline and free; run it by hand
    /// with `cargo test -- --ignored real_claude` when the protocol or the
    /// flags above change, which is the only time anything here can break
    /// without a unit test noticing.
    #[test]
    #[ignore = "sends a real request to Claude Code"]
    fn real_claude_answers_and_remembers() {
        let mut session = claude_code("");
        let first = Instant::now();
        let answer = session.ask("¿cuánto es el 15 % de 340?", SYSTEM_PROMPT).unwrap();
        println!("primera respuesta en {:?}: {answer}", first.elapsed());
        assert!(answer.contains("51"), "{answer}");

        // The conversation survives between questions, which is the whole
        // reason the process is kept warm.
        let second = Instant::now();
        let followed = session.ask("¿y la mitad de eso?", SYSTEM_PROMPT).unwrap();
        println!("segunda respuesta en {:?}: {followed}", second.elapsed());
        assert!(followed.contains("25") || followed.contains("veinticinco"), "{followed}");

        // ask_once runs outside it: this question cannot see the two above.
        let outside = session.ask_once("Responde solo con OK.", SYSTEM_PROMPT).unwrap();
        assert!(outside.to_uppercase().contains("OK"), "{outside}");

        session.forget();
        assert!(session.live.is_none());
    }

    #[test]
    fn the_gemini_answer_is_the_response_field() {
        assert_eq!(
            parse_gemini_output(r#"{"response":"El 15 % de 340 es 51.","stats":{}}"#).unwrap(),
            "El 15 % de 340 es 51."
        );
    }

    #[test]
    fn gemini_warnings_before_the_json_are_ignored() {
        let output = "[WARN] Skipping unreadable directory\n{\"response\":\"OK\"}\n";
        assert_eq!(parse_gemini_output(output).unwrap(), "OK");
    }

    #[test]
    fn a_gemini_error_is_reported() {
        let output = r#"{"error":{"type":"ProjectIdRequiredError","message":"This account requires setting GOOGLE_CLOUD_PROJECT"}}"#;
        assert!(matches!(parse_gemini_output(output), Err(AiError::Backend(_))));
    }

    #[test]
    fn plain_text_from_gemini_is_taken_as_the_answer() {
        assert_eq!(parse_gemini_output("  OK  ").unwrap(), "OK");
    }

    #[test]
    fn the_model_is_read_out_of_a_temp_codex_config() {
        let path = std::env::temp_dir()
            .join(format!("minion-codex-config-test-{}.toml", std::process::id()));
        std::fs::write(&path, "model = \"gpt-5.6-sol\"\npersonality = \"pragmatic\"\n").unwrap();
        assert_eq!(codex_configured_model_at(&path).as_deref(), Some("gpt-5.6-sol"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_config_without_a_model_key_has_none() {
        let path = std::env::temp_dir()
            .join(format!("minion-codex-config-empty-test-{}.toml", std::process::id()));
        std::fs::write(&path, "personality = \"pragmatic\"\n").unwrap();
        assert_eq!(codex_configured_model_at(&path), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_config_file_has_none() {
        assert_eq!(
            codex_configured_model_at(std::path::Path::new("/nonexistent/config.toml")),
            None
        );
    }

    #[test]
    fn the_reason_on_stderr_survives_and_the_stack_trace_does_not() {
        let stderr = "[WARN] something\nAn unexpected critical error occurred:Error: This account requires setting the GOOGLE_CLOUD_PROJECT env var.\n    at setupUser (file:///x.js:85:15)\n";
        assert_eq!(
            first_complaint(stderr).as_deref(),
            Some("Error: This account requires setting the GOOGLE_CLOUD_PROJECT env var.")
        );
        assert!(first_complaint("   \n").is_none());
    }
}

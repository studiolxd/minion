//! Minion — control your Mac by speaking Spanish.
//!
//! Listens on the microphone, transcribes with Parakeet, and carries out
//! sentences that open with the wake word. Lives in the menu bar.
//!
//! Thread layout matters here: AppKit insists the menu bar is created and
//! serviced on the main thread, so recognition — the expensive part — runs
//! on its own.

mod actions;
mod ai;
mod answers;
mod api;
mod audio;
mod commands;
mod config;
mod corpus;
mod dictation;
mod enroll;
mod fbank;
mod hotkey;
mod hud;
mod icon;
mod journal;
mod learn;
mod loopback;
mod metrics;
mod microphone;
mod models;
mod notify;
mod onboarding;
mod packs;
mod preferences;
mod session;
mod shortcuts;
mod spanish;
mod speech;
mod startup;
mod speaker;
mod system;
mod text;
mod vocabulary;
mod vocabulary_editor;
mod timers;
mod updater;

use std::cell::Cell;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use ort::session::builder::SessionBuilder;
use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIconBuilder;

use commands::Decision;
use session::{Outcome, Reply, Session, Undoable};

/// What the tooltip says when there is nothing more particular to report.
const TOOLTIP_IDLE: &str = concat!("Minion ", env!("CARGO_PKG_VERSION"), " — control por voz");
const TOOLTIP_LISTENING: &str = "Minion — escuchando";
const TOOLTIP_PAUSED: &str = "Minion — en pausa";
/// Push-to-talk's own pair, used instead of the two above when
/// `listen_mode = "hold"`.
const TOOLTIP_HOLD_ACTIVE: &str = "Minion — escuchando (mantén pulsado)";
const TOOLTIP_HOLD_IDLE: &str = "Minion — pulsa para hablar";

/// How much of a transcript the tooltip carries.
///
/// Long enough to recognise the sentence, short enough that the tooltip
/// stays one line. What was heard in full is in the log.
const TOOLTIP_TRANSCRIPT: usize = 48;

/// Cuts `text` to at most `max` characters, marking that it was cut.
fn shorten(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() > max {
        trimmed.chars().take(max - 1).collect::<String>() + "…"
    } else {
        trimmed.to_string()
    }
}

/// The tooltip line for an utterance and what came of it.
///
/// This is the whole visible trace of what Minion just did: the sounds can
/// be turned off, the icon only blinks, and the log is a file. Pure, so
/// the shortening can be tested.
fn last_utterance_tooltip(transcript: &str, outcome: &str) -> String {
    format!("Minion — última: “{}” → {outcome}", shorten(transcript, TOOLTIP_TRANSCRIPT))
}

/// The first sentence of `text`, kept whole with its closing punctuation —
/// what `brief_answers` speaks instead of the whole reply. What is logged,
/// shown in the HUD and notified stays the full text; only the spoken
/// version is cut short.
fn first_sentence(text: &str) -> String {
    match text.find(['.', '!', '?']) {
        Some(at) => text[..=at].to_string(),
        None => text.to_string(),
    }
}

/// Recovers `(transcript, outcome)` from a tooltip written by
/// [`last_utterance_tooltip`] — the inverse, used to feed the "Últimas
/// órdenes" menu without the listening loop having to know about it.
///
/// A first version, deliberately: the loop that decides what happened to
/// an utterance is not this file's to change, and the tooltip is already
/// everything it tells the rest of the program. A queue pushed to
/// directly, from wherever `set_status` is called with
/// `last_utterance_tooltip`, would not lose the truncation this goes
/// through — see the report for that follow-up.
fn parse_last_utterance(tooltip: &str) -> Option<(String, String)> {
    let rest = tooltip.strip_prefix("Minion — última: “")?;
    let (text, outcome) = rest.split_once("” → ")?;
    Some((text.to_string(), outcome.to_string()))
}

/// How many recent utterances the "Últimas órdenes" menu keeps.
const HISTORY_LEN: usize = 5;

/// One entry in that menu: the phrase with the wake word already stripped
/// — ready for [`api::request_run`] — and the alias it came from, if
/// `[[aliases]]` in config.toml is what matched it (the only case
/// "Olvidar alias" has anything to remove).
#[derive(Clone)]
struct HistorySlot {
    phrase: String,
    alias_phrase: Option<String>,
}

/// What the «Actualizar vocabulario…» worker thread leaves behind for the
/// menu-bar timer to show — owned strings, not [`packs::Outcome`] or
/// [`packs::Installed`] directly, so nothing about the thread that produced
/// them has to cross into the dialog that reports them.
enum PacksUpdateResult {
    Installed { version: String, packs: usize },
    Failed(String),
}

/// What a worker thread found out about a new version of Minion itself,
/// left for the UI timer to act on — see `updater.rs`. `asked` is true
/// when a person chose «Buscar actualizaciones…»: the daily check keeps
/// quiet about everything except an update that exists.
enum UpdateResult {
    Checked { check: updater::Check, asked: bool },
    Installed(String),
    Failed(String),
}

/// How often the daily check looks at its own timestamp file. The rule is
/// once a day (`updater::daily_check_due`); this is only how often that
/// question is asked, kept well away from once a second.
const UPDATE_CHECK_SECONDS: f64 = 300.0;

/// Where to send someone whose microphone Minion cannot use.
const MICROPHONE_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

/// System sounds used as feedback. A command that runs produces no visible
/// output, so without these you cannot tell whether you were heard.
mod sounds {
    pub const DONE: &str = "/System/Library/Sounds/Pop.aiff";
    pub const UNSURE: &str = "/System/Library/Sounds/Tink.aiff";
    /// Understood perfectly and refused by macOS. A different sound from
    /// UNSURE on purpose: "say it again" and "grant the permission" are
    /// different problems, and they used to be indistinguishable.
    pub const BLOCKED: &str = "/System/Library/Sounds/Basso.aiff";
    /// An utterance was addressed to Minion and is about to be decided —
    /// played before there is anything to say yet, so `[feedback]` in
    /// "sounds" or "quiet" mode still has something to notice, since it
    /// never speaks. Distinct from DONE: this fires whether or not
    /// anything is understood.
    pub const HEARD: &str = "/System/Library/Sounds/Morse.aiff";
    /// A question was just asked — a confirmation, a guess, a choice —
    /// and is waiting for its answer.
    pub const QUESTION: &str = "/System/Library/Sounds/Ping.aiff";
}

/// The toggle's two faces. It names the action, not the state: a menu item
/// is something you do, so while listening it offers to pause.
const MENU_PAUSE: &str = "Pausar";
const MENU_LISTEN: &str = "Escuchar";

/// How long the icon shows that a command ran.
const BLINK_SECONDS: f64 = 0.45;

/// How often the run loop checks for changes.
///
/// Fast enough that a slider's readout keeps up with the thumb, which is
/// what makes the preferences window feel like a window rather than a form.
const UI_REFRESH_SECONDS: f64 = 0.05;

/// How many ticks of the UI timer make up one check, while nobody is
/// looking at the settings window and no blink is pending.
///
/// Five ticks of 50 ms is 250 ms — quick enough that a menu toggle still
/// repaints promptly, slow enough that an idle Minion is not doing the
/// full repaint eighteen times a second for no one.
const UI_THROTTLE_TICKS: u32 = 5;

/// Ticks of the 50 ms timer between the two talking frames: 150 ms, about
/// the pace of syllables, which is what makes the mouth read as talking.
const SPEAKING_FRAME_TICKS: u32 = 3;

/// How often the recognition loop wakes up between utterances.
///
/// Short, because this is also how quickly it notices that someone has
/// started talking while the model is unloaded: a second of loading that
/// happens while the sentence is still being spoken is a second the
/// speaker never waits for. The wake-up itself is one atomic read.
const IDLE_CHECK: Duration = Duration::from_millis(250);

/// Smart auto-pause: checked on the same idle tick as everything else
/// above, rather than through `NSWorkspace`/distributed-notification
/// observers. Those need a run loop of their own to deliver blocks on, and
/// the idle tick already polls a few times a second for other reasons —
/// one more cheap check costs nothing extra.
mod auto_pause {
    use crate::microphone;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::CFString;
    use std::ffi::c_void;

    /// Whether the login session is locked (password or Touch ID lock, the
    /// screen saver locking the screen).
    ///
    /// `CGSessionCopyCurrentDictionary` is undocumented but has been the
    /// standard way to ask this without a helper process for as long as
    /// the alternative — watching `com.apple.screenIsLocked` — has existed;
    /// both end up reading the same session dictionary.
    pub fn screen_locked() -> bool {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
        }
        unsafe {
            let raw = CGSessionCopyCurrentDictionary();
            if raw.is_null() {
                // No session at all, e.g. before anyone has logged in.
                // Reading that as "locked forever" would leave nothing to
                // resume it, so the safer answer is "not locked".
                return false;
            }
            let dict: CFDictionary<*const c_void, *const c_void> =
                TCFType::wrap_under_create_rule(raw);
            let key = CFString::from_static_string("CGSSessionScreenIsLocked");
            dict.find(key.as_CFTypeRef())
                .and_then(|value| CFType::wrap_under_get_rule(*value).downcast::<CFBoolean>())
                .map(bool::from)
                .unwrap_or(false)
        }
    }

    /// Whether the main display is asleep — the screen turned off, whether
    /// because the whole Mac slept or only the display did.
    pub fn display_asleep() -> bool {
        core_graphics::display::CGDisplay::main().is_asleep()
    }

    /// How a microphone already in use is written down, if it is in use.
    ///
    /// Kept apart from [`reason`] so the wording can be tested without a
    /// microphone: everything else in here reads the live system.
    pub fn microphone_reason(capturing: &[microphone::Capture]) -> Option<String> {
        let first = capturing.first()?;
        let others = capturing.len() - 1;
        Some(match others {
            0 => format!("microphone in use by {first}"),
            1 => format!("microphone in use by {first} and 1 other"),
            _ => format!("microphone in use by {first} and {others} others"),
        })
    }

    /// Why listening should be paused right now, if it should.
    ///
    /// Checked in order of how much it would cost to keep listening
    /// wrongly: a locked or sleeping Mac hears nothing useful at all,
    /// while somebody else recording is the milder case.
    ///
    /// `watch_microphone` is the `pause_when_microphone_busy` setting,
    /// already lowered to false where macOS cannot answer the question.
    pub fn reason(watch_microphone: bool) -> Option<String> {
        if screen_locked() {
            return Some("the screen is locked".to_string());
        }
        if display_asleep() {
            return Some("the display is asleep".to_string());
        }
        if watch_microphone && microphone::in_use_by_others() == Some(true) {
            // Same second-long cache as the check above, so naming who it
            // is costs nothing more than asking whether anyone is.
            let capturing = microphone::others_capturing().unwrap_or_default();
            return Some(
                microphone_reason(&capturing)
                    .unwrap_or_else(|| "the microphone is in use elsewhere".to_string()),
            );
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn nobody_recording_is_not_a_reason_to_pause() {
            assert_eq!(microphone_reason(&[]), None);
        }

        #[test]
        fn the_reason_names_who_is_recording() {
            let teams = microphone::Capture {
                pid: 501,
                bundle_id: Some("com.microsoft.teams2".to_string()),
            };
            let nameless = microphone::Capture { pid: 777, bundle_id: None };
            assert_eq!(
                microphone_reason(std::slice::from_ref(&teams)),
                Some("microphone in use by com.microsoft.teams2 (pid 501)".to_string())
            );
            assert_eq!(
                microphone_reason(&[teams.clone(), nameless.clone()]),
                Some("microphone in use by com.microsoft.teams2 (pid 501) and 1 other".to_string())
            );
            assert_eq!(
                microphone_reason(&[teams, nameless.clone(), nameless]),
                Some(
                    "microphone in use by com.microsoft.teams2 (pid 501) and 2 others".to_string()
                )
            );
        }

        #[test]
        fn the_display_answers_something_either_way() {
            // Whichever it is on the machine running the tests, it must not
            // panic or hang — that is the only thing worth checking here.
            let _ = display_asleep();
            let _ = screen_locked();
        }
    }
}

/// Whether `executable` lives inside a `.app` bundle.
///
/// Shared with [`relaunch_arguments`]'s bundle detection: same question,
/// "is this a bundled copy or a bare binary".
fn is_bundled(executable: &Path) -> bool {
    executable
        .ancestors()
        .any(|path| path.extension().is_some_and(|kind| kind == "app"))
}

/// Where the model might be, given where the executable is running from.
///
/// The bundle candidates (`Contents/Resources/model` and the sibling of the
/// binary) are always worth a look — a bundled Minion has no other way to
/// find its model. The working directory is only worth a look for a
/// development build run from the repo: a bundled app's cwd is `/` and
/// checking it there risks matching an unrelated `model` folder that
/// happens to sit wherever the double-click launched from.
///
/// Pure — no filesystem access — so the candidate list can be checked for
/// both cases without a real executable path or a real bundle.
fn model_candidates(executable: &Path, cwd_allowed: bool) -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Some(macos_dir) = executable.parent() {
        candidates.push(macos_dir.join("../Resources/model"));
        candidates.push(macos_dir.join("model"));
    }
    if cwd_allowed {
        candidates.push(Path::new("model").to_path_buf());
        candidates.push(Path::new("../model").to_path_buf());
    }
    candidates
}

/// Locates the speech model, without fetching anything.
///
/// Order: explicit argument, `MINION_MODEL`, the app bundle's Resources,
/// then — for a development build only, never a bundled one — the working
/// directory, then the downloaded copy in Application Support. The bundle
/// case is what makes double-click launching work, since a bundled app
/// starts with `/` as its directory.
///
/// Separate from downloading it because the two belong to different
/// moments: this answers "is it here?" in microseconds, while fetching it
/// takes minutes and must happen where its progress can be shown.
fn find_model(argument: Option<String>) -> Option<String> {
    if let Some(path) = argument {
        return Some(path);
    }
    if let Ok(path) = std::env::var("MINION_MODEL") {
        return Some(path);
    }
    // The name from when this was called Oyente. Still read, so an
    // existing shell profile keeps working, but it says so.
    if let Ok(path) = std::env::var("OYENTE_MODEL") {
        note!("OYENTE_MODEL is deprecated — rename it to MINION_MODEL.");
        return Some(path);
    }

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    match std::env::current_exe() {
        Ok(executable) => {
            let cwd_allowed = !is_bundled(&executable);
            candidates.extend(model_candidates(&executable, cwd_allowed));
        }
        // No way to tell where we are running from — fall back to the
        // development-build behaviour, which is also the safer guess.
        Err(_) => {
            candidates.push(Path::new("model").to_path_buf());
            candidates.push(Path::new("../model").to_path_buf());
        }
    }

    // Downloaded on first run, and kept outside the bundle so reinstalling
    // does not fetch 670 MB again.
    if let Some(downloaded) = models::directory() {
        candidates.push(downloaded);
    }

    for candidate in candidates {
        if models::present(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Finds the model, downloading it if it is not there yet.
///
/// The command line — `minion enroll` — has a terminal to print to, so
/// this is where the old behaviour lives on. The menu bar app cannot use
/// it: it has to have an icon before a 670 MB download starts.
fn locate_model(argument: Option<String>) -> Result<String> {
    if let Some(found) = find_model(argument) {
        return Ok(found);
    }
    let target = models::directory()
        .ok_or_else(|| anyhow!("no home directory to download the model into"))?;
    println!("Descargando el modelo de reconocimiento (una sola vez, ~670 MB)…");
    models::fetch(&target, |progress| {
        println!("  {progress}");
    })
    .map_err(|e| anyhow!("{e}"))?;
    Ok(target.to_string_lossy().into_owned())
}

/// How ONNX Runtime should be set up.
///
/// The defaults are tuned for throughput on a server: four threads and a
/// memory arena that reserves generously and never gives anything back.
/// This is a menu bar app that spends almost all its time idle, so the
/// trade runs the other way — a little slower per utterance in exchange
/// for not holding on to memory between them.
fn inference_config() -> ExecutionConfig {
    ExecutionConfig {
        // Utterances are a second or two; two threads keep the latency
        // well under a person's reaction time.
        intra_threads: 2,
        inter_threads: 1,
        configure: Some(std::rc::Rc::new(|builder: SessionBuilder| {
            // Memory patterns pre-allocate for the largest shape seen so far
            // and hold it, so with variable-length audio the longest
            // utterance of a session sets the floor for the rest of it.
            let builder = builder.with_memory_pattern(false)?;
            // Prepacking rewrites weights into a layout that multiplies
            // faster and keeps the original alongside it. Turning it off is
            // what takes this from 1830 MB down to 934 MB.
            let builder = builder.with_config_entry("session.disable_prepacking", "1")?;
            // Read initializers straight from the mapped file rather than
            // copying them into the arena first.
            let builder =
                builder.with_config_entry("session.use_device_allocator_for_initializers", "1")?;
            Ok(builder)
        })),
        ..Default::default()
    }
}

/// Resident memory of this process, as a human-readable string.
///
/// Reported at startup because it is the number that decides whether this
/// is something you can leave running all day.
fn resident_memory() -> String {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p"])
        .arg(std::process::id().to_string())
        .output();
    match output {
        Ok(out) => {
            let kb: f64 = String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0.0);
            format!("Using {:.0} MB.", kb / 1024.0)
        }
        Err(_) => String::new(),
    }
}

/// An idle duration, rounded to the minute, for the log.
fn format_idle(idle: Duration) -> String {
    format!("{} min", idle.as_secs() / 60)
}

/// Whether the machine is currently running on battery power.
///
/// `pmset -g batt` rather than IOKit/`ioreg`: it says "Battery Power" or
/// "AC Power" on its first line and needs no framework linking for a value
/// checked once every 30 seconds, not on a hot path.
fn on_battery() -> bool {
    std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "batt"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("Battery Power"))
}

/// The recognition loop. Owns the model and runs on its own thread.
/// Loads the speech model.
fn load_model(model_path: &str) -> Result<ParakeetTDT> {
    ParakeetTDT::from_pretrained(model_path, Some(inference_config()))
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading the model from '{model_path}'"))
}

struct Voice {
    model: speaker::Speaker,
    /// Everyone enrolled. Empty means nobody is, and anyone is obeyed.
    profiles: Vec<speaker::Profile>,
    threshold: f32,
}

/// What the tooltip should say, written by whoever knows and applied by
/// the run loop. The menu bar belongs to the main thread; the download and
/// the recognition loop do not.
type Status = Arc<Mutex<String>>;

/// Sets the tooltip text, if anyone can still read it.
fn set_status(status: &Status, text: &str) {
    if let Ok(mut current) = status.lock() {
        if *current != text {
            *current = text.to_string();
        }
    }
}

/// Voice training, shared between the window and the listening loop.
///
/// `Some` while training is under way. The window sets it going and reads
/// the message; the loop that owns the microphone does the work.
type Training = Arc<Mutex<Option<enroll::Session>>>;

/// Everything the listening loop needs, gathered rather than passed one by
/// one: eight parameters in a row is a list nobody reads.
struct Listening {
    model_path: String,
    audio: audio::Settings,
    log_ignored_speech: Arc<AtomicBool>,
    play_sounds: Arc<AtomicBool>,
    idle_unload: Option<Duration>,
    /// How long the conversation window stays open after a command or an
    /// answer. Zero disables it.
    conversation_window: Duration,
    /// Set while the conversation window is open, so the menu bar can show
    /// the attentive face.
    window_open: Arc<AtomicBool>,
    /// `listen_mode = "hold"`: no wake word is needed while `active` is
    /// true, since that only happens while the shortcut is held.
    hold_mode: bool,
    /// Pause listening while another process is capturing the microphone.
    pause_when_microphone_busy: bool,
    voice: Option<Voice>,
    training: Training,
    active: Arc<AtomicBool>,
    /// Raised when something runs, so the menu bar can acknowledge it.
    acted: Arc<AtomicBool>,
    /// Set while in dictation mode, so the menu bar can show it.
    dictating: Arc<AtomicBool>,
    /// Set while the model is being reloaded after a decision is needed —
    /// the one delay long enough (~1 s) to be worth showing a face for.
    thinking: Arc<AtomicBool>,
    /// Set while `speech::say` is talking back.
    speaking: Arc<AtomicBool>,
    /// How to answer questions aloud.
    voice_reply: Option<VoiceReply>,
    /// Which microphone to use, or none to follow the system.
    microphone: Option<String>,
    /// Raised to ask the menu bar to open the list of commands.
    show_catalogue: Arc<AtomicBool>,
    /// Keep a copy of what was heard, for diagnosis.
    save_recordings: bool,
    /// What the tooltip should say.
    status: Status,
    /// Raised once for every `Outcome::Answer` — the onboarding assistant's
    /// "Prueba" page waits on this rather than re-parsing the tooltip.
    answered: Arc<AtomicBool>,
    /// Raised, and never lowered, once the microphone is proven to deliver
    /// only silence. See `audio::SilenceWatch`.
    mic_denied: Arc<AtomicBool>,
}

/// Settings for speaking back.
struct VoiceReply {
    voice: Option<String>,
    rate: u32,
    device: Option<String>,
}

fn listen_and_obey(setup: Listening) -> Result<()> {
    let Listening {
        model_path,
        audio: settings,
        log_ignored_speech,
        play_sounds,
        idle_unload,
        conversation_window,
        window_open,
        hold_mode,
        pause_when_microphone_busy,
        mut voice,
        training,
        active,
        acted,
        dictating,
        thinking,
        speaking,
        voice_reply,
        microphone,
        show_catalogue,
        save_recordings,
        status,
        answered,
        mic_denied,
    } = setup;

    // While Minion is speaking it must not act on what it hears: it listens
    // continuously, so its own voice comes straight back in.
    let deaf = Arc::new(AtomicBool::new(false));
    // Held in an Option so it can be dropped while idle. It is loaded now
    // rather than on first use, so the first thing said after starting is
    // as quick as the rest.
    let mut model = Some(load_model(&model_path)?);
    let mut last_used = Instant::now();
    // Dictation, "otra vez" and "deshaz" all remember something from one
    // utterance to the next. That memory, and the rules that go with it,
    // are in `session`; what follows only carries them out.
    let mut session = Session::new();
    // Whether a phrase that was nearly understood is worth a question.
    // Read here rather than passed in: it is only ever wanted by the loop.
    let startup = config::load();
    session.asks_before_learning(startup.ask_before_learning);
    // And how close a second reading has to be before the choice is put to
    // the user instead of guessed at. Read from the same file, once.
    session.disambiguates(startup.disambiguation_margin());
    // And how sure a costly command has to be before it just runs instead
    // of being confirmed — see `commands::confirm_question`.
    session.confirms_below(startup.confirm_below());
    // How to signal what is happening — earcons, speech, both or neither
    // — and whether a spoken confirmation or answer is kept brief. Read
    // once, the same as everything else above: none of it is live-
    // reloaded mid-session.
    let feedback_mode = startup.feedback();
    let brief_answers = startup.brief_answers;
    // Built fresh each time dictation starts, so its state (the pending
    // capital, an open quote) never spans two dictation sessions, and a
    // vocabulary edited while Minion was running takes effect right away.
    let mut transformer: Option<dictation::Transformer> = None;
    // Set once smart auto-pause has paused listening on its own, so it
    // knows the pause is its own to lift — a manual pause never sets this,
    // and so never gets silently overridden once the reason clears.
    let mut auto_paused = false;
    // «espera diez minutos», «no me escuches hasta las cinco»: when this
    // is due, listening resumes on its own — checked on the idle tick
    // below. Cleared, rather than acted on, the moment `active` is found
    // already true: that means something else (the menu, the shortcut)
    // resumed it early, and the pause is no longer this feature's to end.
    let mut paused_until: Option<DateTime<Local>> = None;
    // Energy: "auto" follows the battery, "battery" always behaves as if
    // on one, "performance" never unloads. Checking `pmset` on every 250 ms
    // tick would be wasteful for a value that changes on the order of
    // hours, so it is cached and refreshed every `BATTERY_POLL_TICKS`.
    let energy_mode = startup.energy_mode();
    let mut on_battery_now = on_battery();
    let mut battery_poll_ticks: u32 = 0;
    const BATTERY_POLL_TICKS: u32 = 120; // 120 * 250 ms = 30 s
    let mut battery_like =
        energy_mode == config::EnergyMode::Battery || on_battery_now;
    // Set when the speaker model is released for being idle, so the next
    // utterance knows to reload it rather than leave it unloaded for good
    // (which `voice` starting as `None` — never enrolled — also looks
    // like).
    let mut voice_unloaded_for_idle = false;
    // The CoreAudio process objects that say who is recording arrived in
    // macOS 14. On 13 the question cannot be asked at all, so the setting
    // is lowered here — said once, rather than on every tick.
    let watch_microphone = pause_when_microphone_busy && microphone::available();
    if pause_when_microphone_busy && !watch_microphone {
        note!("auto-pause: this macOS cannot say which apps use the microphone (needs macOS 14).");
    }
    note!("Model loaded. {}", resident_memory());

    // How long to stay deaf after speaking: the segmenter needs
    // `silence_end_ms` of quiet before it closes an utterance, so anything
    // shorter hands Minion its own answer just after the flag comes down.
    let speech_tail = Duration::from_millis(settings.silence_end_ms as u64 + 200);
    let listener =
        audio::start(
            settings,
            Arc::clone(&active),
            microphone,
            Arc::clone(&deaf),
            Some(model_path.clone()),
        )
        .context("opening the microphone")?;
    note!(
        "Microphone: {} Hz, {} channel(s). {} phrases understood.",
        listener.source_hz,
        listener.channels,
        commands::phrase_count()
    );
    // The Mac's own output, so hearing Netflix back through the microphone
    // is not mistaken for someone talking. Opened *after* the microphone
    // on purpose: CoreAudio serialises IOProc creation across the whole
    // client, and the microphone is the one stream that must not wait.
    // Kept for the life of the loop — the tap is torn down when it drops.
    let own_audio_threshold = startup
        .audio
        .own_audio_threshold
        .unwrap_or(loopback::DEFAULT_THRESHOLD);
    let own_audio = if startup.audio.ignore_own_audio.unwrap_or(true) {
        match loopback::Loopback::start() {
            Ok(tap) => {
                note!(
                    "Ignoring the Mac's own audio: output tap open at {} Hz, threshold {own_audio_threshold:.2}.",
                    tap.source_hz
                );
                Some(tap)
            }
            Err(e) => {
                // Said, not fatal: the microphone still works, and the
                // speaker check still stands behind it.
                note!("own-audio tap unavailable: {e:#}");
                None
            }
        }
    } else {
        None
    };
    // The wake word can be changed in preferences, and telling someone to
    // say «minion» when it no longer answers to that is worse than saying
    // nothing.
    let wake = commands::wake_words().first().copied().unwrap_or("minion");
    note!("Listening. Say: «{wake}, abre Chrome»");

    // Puts a question to whoever is there, and says whether anybody could
    // have heard it. A question that arrives as a silent banner has nobody
    // waiting for the answer, so only a spoken one is worth opening a
    // window for — both callers below rely on that `false`.
    let ask_aloud = |text: &str| -> bool {
        match &voice_reply {
            Some(settings) => {
                speaking.store(true, Ordering::Relaxed);
                speech::say(
                    text,
                    settings.voice.as_deref(),
                    settings.rate,
                    settings.device.as_deref(),
                    &deaf,
                    speech_tail,
                );
                speaking.store(false, Ordering::Relaxed);
                // Its own voice came back in while it was talking; none of
                // that is an answer.
                while listener.utterances.try_recv().is_ok() {}
                true
            }
            None => {
                notify::post("Minion", text);
                false
            }
        }
    };
    // A short earcon for a question just asked — a confirmation, a guess,
    // a choice — under `[feedback]`'s "sounds"/"both". Kept apart from
    // `ask_aloud`: a confirmation in brief mode plays this instead of
    // speaking, while every other question plays it alongside speech.
    let sound_question = || {
        if feedback_mode.sounds() && play_sounds.load(Ordering::Relaxed) {
            let _ = actions::play_sound(sounds::QUESTION);
        }
    };

    loop {
        // A question waits a few seconds for its answer, and silence is
        // one of the answers it can get. Checked here as well as on the next
        // utterance, so the log says so when it happens rather than
        // whenever somebody next speaks.
        if let Some(phrase) = session.question_timed_out(Instant::now()) {
            note!("declined «{phrase}»  ->  no answer");
            hud::set_question_pending(false);
        }
        // A bounded wait, so idleness can be noticed while nothing is being
        // said. A plain recv() would block until the next utterance, and
        // the model would stay loaded through an empty afternoon.
        let utterance = match listener.utterances.recv_timeout(IDLE_CHECK) {
            Ok(utterance) => utterance,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The window has no timer of its own — it just stops being
                // open once `Instant::now()` passes its deadline — so this
                // is where that becomes visible to the menu bar.
                window_open.store(session.window_open(Instant::now()), Ordering::Relaxed);
                // «espera diez minutos», «no me escuches hasta las cinco»:
                // due, or already lifted some other way.
                if let Some(until) = paused_until {
                    if active.load(Ordering::Relaxed) {
                        paused_until = None;
                    } else if Local::now() >= until {
                        active.store(true, Ordering::Relaxed);
                        paused_until = None;
                        note!("resumed  the pause ended — listening again");
                        set_status(&status, TOOLTIP_LISTENING);
                    }
                }
                // Smart auto-pause. Only in "always" mode: push-to-talk
                // already gates listening on the key, and layering this on
                // top of it would fight that — resuming on its own the
                // moment a meeting ends, key or no key.
                if !hold_mode {
                    let listening = active.load(Ordering::Relaxed);
                    if listening && auto_paused {
                        // Turned back on by something other than this
                        // check — the menu, the shortcut, a spoken order —
                        // so the pause is no longer this feature's to lift.
                        auto_paused = false;
                    }
                    let reason = auto_pause::reason(watch_microphone);
                    if let Some(reason) = reason {
                        if listening {
                            active.store(false, Ordering::Relaxed);
                            auto_paused = true;
                            note!("paused automatically — {reason}");
                        }
                    } else if auto_paused {
                        active.store(true, Ordering::Relaxed);
                        auto_paused = false;
                        note!("resumed automatically — the reason has gone away");
                    }
                }
                // Battery state changes on the order of hours, not 250 ms —
                // polled here, on the tick that already runs every quarter
                // second for everything else above.
                battery_poll_ticks += 1;
                if battery_poll_ticks >= BATTERY_POLL_TICKS {
                    battery_poll_ticks = 0;
                    on_battery_now = on_battery();
                    let now_battery_like =
                        energy_mode == config::EnergyMode::Battery || on_battery_now;
                    if now_battery_like != battery_like {
                        battery_like = now_battery_like;
                        if battery_like {
                            note!(
                                "energy   battery — model released after {} min idle, \
                                 speaker after {} min",
                                config::BATTERY_MODEL_UNLOAD_MINUTES,
                                config::BATTERY_SPEAKER_UNLOAD_MINUTES
                            );
                        } else {
                            note!("energy   AC power — the ordinary idle threshold applies again");
                        }
                    }
                }
                // The HUD appears the instant Silero opens an utterance —
                // long before there is a transcript, let alone a decision.
                // `speech_open` is a level, not a latch (unlike
                // `speech_started` below): calling this every tick while
                // still speaking is harmless, since both of the panel's
                // lines are already pending.
                if listener.speech_open.load(Ordering::Relaxed) {
                    hud::note_speech_open();
                }
                // Someone has started talking. If the model was released
                // while idle, load it now: the sentence and the silence
                // that closes it take longer than the load, so this hides
                // the second the user used to wait through after speaking.
                let speech_starting = listener.speech_started.swap(false, Ordering::Relaxed);
                if speech_starting && model.is_none() && active.load(Ordering::Relaxed) {
                    match load_model(&model_path) {
                        Ok(loaded) => {
                            model = Some(loaded);
                            // Counts as use, or the idle check below would
                            // release it again before a word is transcribed.
                            last_used = Instant::now();
                            note!("Speech starting — model reloaded. {}", resident_memory());
                        }
                        Err(e) => note!("error    could not reload the model: {e:#}"),
                    }
                }
                // Same idea for the speaker model, released separately (and
                // later) on battery — see the energy decision below. Only
                // reloaded here if it was this feature that let it go: a
                // `voice` that is `None` because nothing was ever enrolled
                // must stay that way.
                if speech_starting
                    && voice_unloaded_for_idle
                    && voice.is_none()
                    && active.load(Ordering::Relaxed)
                {
                    let started = Instant::now();
                    match speaker::Speaker::load(&model_path) {
                        Ok(model) => {
                            let profiles = speaker::load_profiles_for(&model_path);
                            voice = Some(Voice { model, profiles, threshold: config_voice_threshold() });
                            voice_unloaded_for_idle = false;
                            note!(
                                "Speech starting — speaker model reloaded in {} ms.",
                                started.elapsed().as_millis()
                            );
                        }
                        Err(e) => note!("error    could not reload the speaker model: {e:#}"),
                    }
                }
                // Nothing but exact zeros since the stream opened: macOS
                // denied the microphone. Said once, with somewhere to go.
                if listener.silent.swap(false, Ordering::Relaxed) {
                    mic_denied.store(true, Ordering::Relaxed);
                    note!("deaf     the microphone delivers only silence — permission denied?");
                    let _ = std::process::Command::new("/usr/bin/open")
                        .arg(MICROPHONE_SETTINGS)
                        .status();
                    actions::show_message(
                        "Minion no puede oír: activa el micrófono para Minion en \
                         Ajustes del Sistema → Privacidad y seguridad → Micrófono",
                    );
                }
                let energy = config::energy_decision(
                    energy_mode,
                    on_battery_now,
                    last_used.elapsed(),
                    idle_unload,
                );
                if energy.unload_model && model.is_some() {
                    model = None;
                    note!("Idle for {} — model released. {}",
                          format_idle(last_used.elapsed()), resident_memory());
                }
                if energy.unload_speaker && voice.is_some() {
                    voice = None;
                    voice_unloaded_for_idle = true;
                    note!("Idle for {} — speaker model released.", format_idle(last_used.elapsed()));
                }
                // Same idea, on its own clock (`[ai] idle_minutes`): the
                // context — a warm Claude Code process, an HTTP backend's
                // history — dies with an idle Minion the same way the
                // speech model does.
                ai::unload_if_idle();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if !active.load(Ordering::Relaxed) {
            continue;
        }
        let seconds = utterance.samples.len() as f32 / audio::TARGET_HZ as f32;
        let started = Instant::now();

        if save_recordings {
            // The date, not just the time: recordings from different days
            // otherwise collide and overwrite each other past midnight.
            let name = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
            match audio::save_recording(&utterance.samples, &name) {
                Ok(path) => note!("saved    {}", path.display()),
                Err(e) => note!("could not save the recording: {e}"),
            }
        }

        // Training takes precedence: while it runs, every utterance is a
        // sample of the person's voice rather than something to obey.
        let training_now = training.lock().is_ok_and(|session| session.is_some());
        if training_now {
            // The speaker model may not be loaded yet — it is only kept
            // when there is a profile to compare against.
            if voice.is_none() {
                match speaker::Speaker::load(&model_path) {
                    Ok(model) => {
                        voice = Some(Voice {
                            model,
                            profiles: Vec::new(),
                            threshold: f32::MAX, // nothing matches until trained
                        });
                    }
                    Err(e) => {
                        note!("cannot train: {e:#}");
                        if let Ok(mut session) = training.lock() {
                            *session = None;
                        }
                        continue;
                    }
                }
            }
            let embedding = voice.as_mut().and_then(|v| v.model.embed(utterance.speech()));
            if let Ok(mut session) = training.lock() {
                if let Some(active_session) = session.as_mut() {
                    if active_session.accept(embedding) {
                        note!("voice training finished");
                        // Adopt what was just learned, without a restart —
                        // and re-read the lot, since the new voice joins
                        // whoever was already enrolled.
                        let profiles = speaker::load_profiles_for(&model_path);
                        if !profiles.is_empty() {
                            if let Some(v) = voice.as_mut() {
                                v.profiles = profiles;
                                v.threshold = config_voice_threshold();
                            }
                        }
                    }
                }
            }
            continue;
        }

        // The Mac's own output, heard back through the microphone. Asked
        // before the speaker check because it is the cheaper of the two —
        // an envelope correlation against a buffer already in memory,
        // against an ECAPA embedding — and because it answers a question
        // the speaker check cannot: a recording of the owner's own voice
        // coming out of the speakers passes the voice check.
        if let Some(tap) = own_audio.as_ref() {
            let late = utterance.captured_at.elapsed();
            if let Some(likeness) = tap.resemblance(&utterance.samples, late) {
                if likeness >= own_audio_threshold {
                    note!("own      {seconds:.1}s of the Mac's own audio ({likeness:.2})");
                    continue;
                }
            }
        }

        // Whose voice this is, decided before transcribing: someone else's
        // speech should not reach the recogniser at all, let alone the log.
        if let Some(voice) = voice.as_mut() {
            // A model loaded for training but with no profile yet means
            // there is nothing to compare against, so anyone is obeyed.
            let known_voice = !voice.profiles.is_empty();
            if known_voice {
                match voice.model.embed(utterance.speech()) {
                    Some(heard) => {
                        // Whoever it sounds most like, and only then whether
                        // it sounds like them enough. Everyone enrolled may
                        // do everything: the name is for the log and for
                        // «¿quién soy?», never a permission.
                        let Some((name, likeness)) =
                            speaker::best_match(&voice.profiles, &heard)
                        else {
                            continue;
                        };
                        if likeness < voice.threshold {
                            note!("heard    {seconds:.1}s in another voice ({likeness:.2})");
                            continue;
                        }
                        // Logged on the way through as well: without both
                        // sides, there is no way to tell a threshold that
                        // is too high from a profile that is wrong.
                        note!("voice    {name} matched at {likeness:.2}");
                        speaker::remember_match(name);
                    }
                    // Under the hard floor: a cough, a door, half a
                    // syllable. Let through, but say so, because this is
                    // the one path where the voice check does not run.
                    None => note!("voice    {seconds:.1}s too short to check — let through"),
                }
            }
        }

        // Reload if it was released while idle. Costs about a second, once —
        // long enough that it is the only case worth a "thinking" face for;
        // the usual 150–300 ms of transcription and the speaker check is a
        // flicker not worth showing.
        if model.is_none() {
            thinking.store(true, Ordering::Relaxed);
            let reloaded = load_model(&model_path);
            thinking.store(false, Ordering::Relaxed);
            match reloaded {
                Ok(loaded) => {
                    model = Some(loaded);
                    note!("Speech heard — model reloaded. {}", resident_memory());
                }
                Err(e) => {
                    note!("error    could not reload the model: {e:#}");
                    continue;
                }
            }
        }
        last_used = Instant::now();

        let Some(loaded) = model.as_mut() else {
            continue;
        };
        let transcript = match loaded.transcribe_samples(utterance.samples, audio::TARGET_HZ, 1, None) {
            Ok(result) => result.text.trim().to_string(),
            Err(e) => {
                note!("error    transcription failed: {e}");
                continue;
            }
        };
        if transcript.is_empty() {
            // Heard, and nothing came back. Worth writing down: it looks
            // identical to not being heard at all, and without a line here
            // the two are impossible to tell apart afterwards.
            note!("blank    {seconds:.1}s of audio, nothing recognised");
            continue;
        }
        let elapsed_ms = started.elapsed().as_millis();

        // The transcript is known now, the decision is not — the HUD's
        // first line can already stop being an ellipsis while the second
        // keeps animating through the speaker check and `session::resolve`.
        hud::push_update(hud::Update { heard: Some(transcript.clone()), outcome: None });

        // One sentence can hold several instructions joined by "y luego".
        for part in session.split(&transcript) {
            // Which application is in front decides what some phrases mean,
            // and it is read per instruction: the first of a chain may well
            // have changed which application that is.
            let context = actions::frontmost_app();
            let now = Instant::now();
            if let Some(phrase) = session.question_timed_out(now) {
                note!("declined «{phrase}»  ->  no answer");
                hud::set_question_pending(false);
            }
            // Whatever was said now, the pending question is over.
            if session.question_open(now) {
                hud::set_question_pending(false);
            }
            // A question that is still open takes precedence over
            // everything else, the conversation window included: what was
            // just said is an answer to it, or it is not an answer at all
            // and the question is dropped rather than left hanging.
            match session.answer_question(&part, now) {
                Some(Reply::Yes { phrase, suggestion }) => {
                    // Through `interpret` like anything else, so that what
                    // was learned can be repeated with "otra vez" and taken
                    // back with "deshaz".
                    let outcome =
                        session.interpret(&phrase, suggestion.decision.clone(), context.as_deref());
                    let (decision, repeats) = match outcome {
                        Outcome::Perform { decision, repeats } => (decision, repeats),
                        _ => (suggestion.decision.clone(), 1),
                    };
                    let ran = report(
                        &phrase,
                        &decision,
                        suggestion.score,
                        repeats,
                        &Reporting {
                            seconds,
                            elapsed_ms,
                            log_ignored_speech: log_ignored_speech.load(Ordering::Relaxed),
                            play_sounds: play_sounds.load(Ordering::Relaxed) && feedback_mode.sounds(),
                            acted: &acted,
                            status: &status,
                        },
                    );
                    match ran {
                        // Refused by macOS: nothing happened, so there is
                        // nothing to undo.
                        Ran::Blocked => session.forget_undo(),
                        Ran::Yes if !hold_mode => {
                            session.open_window(now, conversation_window);
                            window_open.store(session.window_open(now), Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    match suggestion.learn() {
                        Ok(()) => note!("taught   «{phrase}»  ->  {}", suggestion.description),
                        Err(reason) => note!("error    could not learn «{phrase}»: {reason}"),
                    }
                    continue;
                }
                Some(Reply::Chose { phrase, candidate }) => {
                    // One of two readings that were too close to choose
                    // between, picked out loud. Through `interpret` like
                    // anything else, so it can be repeated with "otra vez"
                    // and taken back with "deshaz".
                    note!("chosen   {} (asked)", candidate.name);
                    let outcome = session.interpret(
                        &phrase,
                        candidate.decision.clone(),
                        context.as_deref(),
                    );
                    let (decision, repeats) = match outcome {
                        Outcome::Perform { decision, repeats } => (decision, repeats),
                        _ => (candidate.decision.clone(), 1),
                    };
                    let ran = report(
                        &phrase,
                        &decision,
                        candidate.score,
                        repeats,
                        &Reporting {
                            seconds,
                            elapsed_ms,
                            log_ignored_speech: log_ignored_speech.load(Ordering::Relaxed),
                            play_sounds: play_sounds.load(Ordering::Relaxed) && feedback_mode.sounds(),
                            acted: &acted,
                            status: &status,
                        },
                    );
                    match ran {
                        Ran::Blocked => session.forget_undo(),
                        Ran::Yes if !hold_mode => {
                            session.open_window(now, conversation_window);
                            window_open.store(session.window_open(now), Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    continue;
                }
                Some(Reply::AiYes { phrase, suggestion }) => {
                    // Through `interpret` like anything else, so that what
                    // the AI suggested can be repeated with "otra vez" and
                    // taken back with "deshaz" — but never learned, unlike
                    // `Reply::Yes`: a model's guess is not a nearby alias.
                    let outcome =
                        session.interpret(&phrase, suggestion.decision.clone(), context.as_deref());
                    let (decision, repeats) = match outcome {
                        Outcome::Perform { decision, repeats } => (decision, repeats),
                        _ => (suggestion.decision.clone(), 1),
                    };
                    let ran = report(
                        &phrase,
                        &decision,
                        1.0,
                        repeats,
                        &Reporting {
                            seconds,
                            elapsed_ms,
                            log_ignored_speech: log_ignored_speech.load(Ordering::Relaxed),
                            play_sounds: play_sounds.load(Ordering::Relaxed) && feedback_mode.sounds(),
                            acted: &acted,
                            status: &status,
                        },
                    );
                    if ran == Ran::Yes {
                        note!("ai-run   «{phrase}»  ->  {}", suggestion.description);
                    }
                    match ran {
                        Ran::Blocked => session.forget_undo(),
                        Ran::Yes if !hold_mode => {
                            session.open_window(now, conversation_window);
                            window_open.store(session.window_open(now), Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    continue;
                }
                Some(Reply::ConfirmYes { phrase, decision, repeats }) => {
                    // The decision was already made before it was asked
                    // about — see the confirmation check below, which
                    // never calls `session.interpret` — so this only
                    // needs to record it, the same way `interpret` would.
                    session.interpret(&phrase, decision.clone(), context.as_deref());
                    let ran = report(
                        &phrase,
                        &decision,
                        1.0,
                        repeats,
                        &Reporting {
                            seconds,
                            elapsed_ms,
                            log_ignored_speech: log_ignored_speech.load(Ordering::Relaxed),
                            play_sounds: play_sounds.load(Ordering::Relaxed) && feedback_mode.sounds(),
                            acted: &acted,
                            status: &status,
                        },
                    );
                    match ran {
                        Ran::Blocked => session.forget_undo(),
                        Ran::Yes if !hold_mode => {
                            session.open_window(now, conversation_window);
                            window_open.store(session.window_open(now), Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    continue;
                }
                Some(Reply::No { phrase, answered }) => {
                    note!("declined «{phrase}»");
                    // A "no" was the answer and is spent. Anything else was
                    // somebody carrying on talking, and still has to be
                    // listened to on its own terms.
                    if answered {
                        continue;
                    }
                }
                None => {}
            }
            // Push-to-talk: every utterance heard was, by definition, said
            // while the shortcut was held, so none of it needs the wake
            // word — there is no window to time out or to log about.
            let resolved = if hold_mode {
                session.resolve_held(&part, context.as_deref())
            } else {
                let resolved = session.resolve(&part, now, context.as_deref());
                if let Some(after) = resolved.window_after {
                    note!(
                        "window   «{part}»  (no wake word, {after:.1} s after the last command)"
                    );
                }
                resolved
            };
            let (decision, confidence) = (resolved.decision, resolved.confidence);
            window_open.store(!hold_mode && session.window_open(now), Ordering::Relaxed);

            // Addressed to Minion — a quick earcon before the (possibly
            // slower) decision below produces its own done/unsure/blocked
            // sound. Never for `Ignored`: that fires on every stray word
            // said in the room, and beeping at all of it would be exactly
            // the noise `[feedback]` is meant to cut down on.
            if decision != Decision::Ignored
                && feedback_mode.sounds()
                && play_sounds.load(Ordering::Relaxed)
            {
                let _ = actions::play_sound(sounds::HEARD);
            }

            // Two readings of the same sentence, neither of them clearly
            // ahead. Asked before anything is carried out, because the
            // point is that neither should be: doing the wrong one and
            // being corrected afterwards is worse than a short question.
            if let Some(question) =
                session.ask_between(&resolved.phrase, &decision, context.as_deref())
            {
                note!("asking   «{part}»  ->  {}?", question.description);
                // Spoken when there is a voice, a banner when there is
                // not — and the window opens either way, unlike a guess.
                // A guess nobody heard must not swallow the next
                // utterance; a choice has already stopped a command, and
                // leaving it unanswerable would only lose it. Timed from
                // after it spoke: the seconds are the ones the person has
                // to answer in.
                ask_aloud(&question.text);
                sound_question();
                session.open_question(&resolved.phrase, question, Instant::now());
                hud::set_question_pending(true);
                continue;
            }

            // A costly command — closing a window, quitting an app,
            // emptying the Trash and the like — matched below
            // `confirm_below`: asked about before it runs, through the
            // same question machinery, rather than simply carried out.
            if let Some(question) = session.ask_confirm(&decision, confidence, 1) {
                note!("asking   «{part}»  ->  {}?  (confirm)", question.description);
                // `brief_answers` makes a confirmation an earcon rather
                // than a spoken question — the point of brief mode.
                if brief_answers {
                    let _ = actions::play_sound(sounds::QUESTION);
                } else {
                    ask_aloud(&question.text);
                    sound_question();
                }
                session.open_question(&resolved.phrase, question, Instant::now());
                hud::set_question_pending(true);
                continue;
            }

            match session.interpret(&part, decision, context.as_deref()) {
                Outcome::EnterDictation => {
                    note!("dictation started — say «deja de dictar» to stop");
                    transformer = Some(dictation::Transformer::new(&config::load()));
                    dictating.store(true, Ordering::Relaxed);
                    acted.store(true, Ordering::Relaxed);
                }
                Outcome::LeaveDictation => {
                    transformer = None;
                    hud::set_dictation_text("");
                    dictating.store(false, Ordering::Relaxed);
                    note!("dictation ended");
                }
                Outcome::DictateInto { destination, recipient } => {
                    match commands::named_destination(destination) {
                        Some(entry) => match prepare_destination(entry, recipient.as_deref()) {
                            Ok(()) => {
                                session.confirm_dictation();
                                note!(
                                    "dictation started in {} — say «deja de dictar» to stop",
                                    entry.name
                                );
                                transformer = Some(dictation::Transformer::new(&config::load()));
                                dictating.store(true, Ordering::Relaxed);
                                acted.store(true, Ordering::Relaxed);
                            }
                            Err(reason) => note!(
                                "BLOCKED  «{part}»  ->  dictar en {}: {reason}",
                                entry.name
                            ),
                        },
                        None => note!(
                            "unknown  «{part}»  ->  no destination called «{destination}»"
                        ),
                    }
                }
                Outcome::NotDictating => note!("not dictating"),
                // Heard while dictating, with nothing in it to type.
                Outcome::Nothing => {}
                Outcome::Type(typed) => {
                    // `session` recorded an undo length in spoken
                    // characters (see `Undoable::Typed` in session.rs,
                    // which this module must not touch); what actually
                    // reaches the keyboard is the rendered text, so any
                    // difference is written down rather than left to make
                    // "deshaz" silently wrong.
                    let rendered = transformer
                        .as_mut()
                        .map(|t| t.render(&typed))
                        .unwrap_or_else(|| typed.clone());
                    hud::set_dictation_text(&rendered);
                    match actions::type_text(&format!("{rendered} ")) {
                        Ok(()) => {
                            note!("typed    «{rendered}»");
                            let (spoken_len, rendered_len) =
                                (typed.chars().count(), rendered.chars().count());
                            if rendered_len != spoken_len {
                                note!(
                                    "dictation rendered {rendered_len} chars from {spoken_len} spoken"
                                );
                                // Plus the space typed after it, as the
                                // session counted for the spoken text.
                                session.retype_length(rendered_len + 1);
                            }
                            acted.store(true, Ordering::Relaxed);
                        }
                        Err(reason) => note!("BLOCKED  «{rendered}»  ->  escribir texto: {reason}"),
                    }
                }
                Outcome::EditDictation(intent) => {
                    let press_deletes = |n: usize| -> Result<(), String> {
                        for _ in 0..n {
                            actions::press(actions::key::DELETE, actions::Mods::NONE)?;
                        }
                        Ok(())
                    };
                    match transformer.as_mut().map(|t| t.edit(&intent)) {
                        Some(Ok(dictation::Edit::DeleteChars(n))) => match press_deletes(n) {
                            Ok(()) => {
                                note!("edited   deleted {n} characters");
                                acted.store(true, Ordering::Relaxed);
                            }
                            Err(reason) => note!("BLOCKED  edit ({n} characters): {reason}"),
                        },
                        Some(Ok(dictation::Edit::Retype { delete, text })) => {
                            match press_deletes(delete) {
                                Ok(()) => match actions::type_text(&format!("{text} ")) {
                                    Ok(()) => {
                                        note!("edited   retyped «{text}»");
                                        acted.store(true, Ordering::Relaxed);
                                    }
                                    Err(reason) => note!(
                                        "BLOCKED  «{text}»  ->  escribir texto: {reason}"
                                    ),
                                },
                                Err(reason) => {
                                    note!("BLOCKED  edit ({delete} characters): {reason}")
                                }
                            }
                        }
                        Some(Err(reason)) => note!("edit     {reason}"),
                        None => note!("edit     not dictating"),
                    }
                }
                Outcome::Answer(question) => {
                    // "¿Qué puedes hacer?" is answered by showing the list.
                    if question == answers::Question::Help {
                        show_catalogue.store(true, Ordering::Relaxed);
                    }
                    let listening = active.load(Ordering::Relaxed);
                    let reply = answers::answer(question, listening);
                    note!("asked    «{part}»  ->  {reply}");
                    set_status(&status, &last_utterance_tooltip(&part, &reply));
                    hud::push_update(hud::Update {
                        heard: Some(part.clone()),
                        outcome: Some(reply.clone()),
                    });
                    acted.store(true, Ordering::Relaxed);
                    answered.store(true, Ordering::Relaxed);
                    match &voice_reply {
                        Some(settings) => {
                            speaking.store(true, Ordering::Relaxed);
                            let spoken =
                                if brief_answers { first_sentence(&reply) } else { reply.clone() };
                            speech::say(
                                &spoken,
                                settings.voice.as_deref(),
                                settings.rate,
                                settings.device.as_deref(),
                                &deaf,
                                speech_tail,
                            );
                            speaking.store(false, Ordering::Relaxed);
                            // Whatever arrived while it was talking is its
                            // own voice, or was said over it. Either way it
                            // was not meant as an instruction.
                            while listener.utterances.try_recv().is_ok() {}
                        }
                        None => actions::show_message(&reply),
                    }
                    if !hold_mode {
                        session.open_window(now, conversation_window);
                        window_open.store(session.window_open(now), Ordering::Relaxed);
                    }
                }
                Outcome::ForgetAiConversation => {
                    ai::forget();
                    note!("ai       conversación olvidada — pedido por voz");
                    acted.store(true, Ordering::Relaxed);
                    let reply = "Conversación olvidada.";
                    set_status(&status, &last_utterance_tooltip(&part, reply));
                    hud::push_update(hud::Update {
                        heard: Some(part.clone()),
                        outcome: Some(reply.to_string()),
                    });
                    match &voice_reply {
                        Some(settings) => {
                            speaking.store(true, Ordering::Relaxed);
                            speech::say(
                                reply,
                                settings.voice.as_deref(),
                                settings.rate,
                                settings.device.as_deref(),
                                &deaf,
                                speech_tail,
                            );
                            speaking.store(false, Ordering::Relaxed);
                            while listener.utterances.try_recv().is_ok() {}
                        }
                        None => actions::show_message(reply),
                    }
                }
                Outcome::Cancel { cancelled_question, closed_window } => {
                    // Stops speech mid-sentence and marks a running macro
                    // to stop at its next step boundary — both are no-ops
                    // when there is nothing to stop. What `Session` itself
                    // held (a pending question, the conversation window)
                    // is already gone by the time this runs.
                    let was_speaking = speaking.load(Ordering::Relaxed);
                    speech::stop();
                    commands::request_cancel();
                    hud::set_question_pending(false);
                    window_open.store(false, Ordering::Relaxed);
                    let mut cancelled = Vec::new();
                    if was_speaking {
                        cancelled.push("speech");
                    }
                    if cancelled_question {
                        cancelled.push("a pending question");
                    }
                    if closed_window {
                        cancelled.push("the conversation window");
                    }
                    let what =
                        if cancelled.is_empty() { "nothing pending".to_string() } else { cancelled.join(", ") };
                    note!("cancel   «{part}»  ->  {what}");
                    acted.store(true, Ordering::Relaxed);
                    hud::push_update(hud::Update {
                        heard: Some(part.clone()),
                        outcome: Some("cancelado".to_string()),
                    });
                }
                Outcome::Pause(spec) => {
                    let (resume_at, label) = match &spec {
                        commands::PauseSpec::For(duration, label) => {
                            (timers::at_duration_from_now(*duration), label.clone())
                        }
                        commands::PauseSpec::At(time, label) => {
                            (timers::next_occurrence(*time), label.clone())
                        }
                    };
                    active.store(false, Ordering::Relaxed);
                    paused_until = Some(resume_at);
                    let until = resume_at.format("%H:%M");
                    note!("paused   «{part}»  ->  {label}, until {until}");
                    set_status(&status, &format!("Minion — en pausa hasta las {until}"));
                    hud::push_update(hud::Update {
                        heard: Some(part.clone()),
                        outcome: Some(format!("en pausa hasta las {until}")),
                    });
                    acted.store(true, Ordering::Relaxed);
                }
                Outcome::AskAi(text) => {
                    // The [ai] table is re-read here: switching the AI on
                    // from Ajustes used to need a restart, and the answer
                    // in between was «desactivada» with the switch on.
                    ai::configure(&config::load());
                    // Thinking can take a while (a cold CLI agent process,
                    // or a slow HTTP round trip), so both faces the model
                    // reload already uses are reused here: the menu bar's
                    // and the HUD's "thinking" state, and the HUD is kept
                    // on screen for as long as it lasts.
                    thinking.store(true, Ordering::Relaxed);
                    hud::set_busy(true);
                    let result = ai::ask(&text, ai::Purpose::Questions);
                    hud::set_busy(false);
                    thinking.store(false, Ordering::Relaxed);
                    let reply = match result {
                        Ok(answer) => answer,
                        // Neither of these ever reaches a backend, so
                        // `ai::ask` never logs them itself — said here
                        // instead, or the log would have no trace of them.
                        Err(ai::AiError::Disabled) => {
                            note!("ai       «{text}»  ->  desactivada");
                            "La IA está desactivada; actívala en Ajustes.".to_string()
                        }
                        Err(ai::AiError::Budget) => {
                            note!("ai       «{text}»  ->  límite diario alcanzado");
                            ai::AiError::Budget.to_string()
                        }
                        Err(ai::AiError::NotConfigured(why)) => why,
                        // The detail is already in the log (ai.rs writes
                        // it); reading a parser error or a stack trace
                        // aloud helps nobody.
                        Err(_) => "No se ha entendido la respuesta de la IA.".to_string(),
                    };
                    set_status(&status, &last_utterance_tooltip(&part, &reply));
                    hud::push_update(hud::Update {
                        heard: Some(part.clone()),
                        outcome: Some(reply.clone()),
                    });
                    acted.store(true, Ordering::Relaxed);
                    answered.store(true, Ordering::Relaxed);
                    match &voice_reply {
                        Some(settings) => {
                            speaking.store(true, Ordering::Relaxed);
                            let spoken =
                                if brief_answers { first_sentence(&reply) } else { reply.clone() };
                            speech::say(
                                &spoken,
                                settings.voice.as_deref(),
                                settings.rate,
                                settings.device.as_deref(),
                                &deaf,
                                speech_tail,
                            );
                            speaking.store(false, Ordering::Relaxed);
                            while listener.utterances.try_recv().is_ok() {}
                        }
                        None => notify::post("Minion", &reply),
                    }
                    if !hold_mode {
                        session.open_window(now, conversation_window);
                        window_open.store(session.window_open(now), Ordering::Relaxed);
                    }
                }
                Outcome::Undo(taken) => match taken {
                    Some(Undoable::Typed(length)) => {
                        let mut blocked = None;
                        for _ in 0..length {
                            if let Err(reason) =
                                actions::press(actions::key::DELETE, actions::Mods::NONE)
                            {
                                blocked = Some(reason);
                                break;
                            }
                        }
                        match blocked {
                            None => {
                                note!("undid    typing ({length} characters)");
                                acted.store(true, Ordering::Relaxed);
                            }
                            Some(reason) => {
                                note!("BLOCKED  undo typing ({length} characters): {reason}");
                            }
                        }
                    }
                    Some(Undoable::Launched { previous }) => match previous {
                        Some(bundle) => match actions::open_app(&bundle) {
                            Ok(()) => {
                                note!("undid    going back to {bundle}");
                                acted.store(true, Ordering::Relaxed);
                            }
                            Err(reason) => {
                                note!("BLOCKED  undo going back to {bundle}: {reason}");
                            }
                        },
                        None => note!("nothing to go back to"),
                    },
                    None => note!("nothing of mine to undo"),
                },
                Outcome::NothingToRepeat => {
                    note!("unknown  «{part}»  ->  nothing to repeat yet");
                }
                Outcome::Perform { decision, repeats } => {
                    let ran = report(
                        &part,
                        &decision,
                        confidence,
                        repeats,
                        &Reporting {
                            seconds,
                            elapsed_ms,
                            log_ignored_speech: log_ignored_speech.load(Ordering::Relaxed),
                            play_sounds: play_sounds.load(Ordering::Relaxed) && feedback_mode.sounds(),
                            acted: &acted,
                            status: &status,
                        },
                    );
                    match ran {
                        Ran::Blocked => {
                            // Nothing happened, so there is nothing to undo.
                            session.forget_undo();
                        }
                        Ran::Yes if !hold_mode => {
                            session.open_window(now, conversation_window);
                            window_open.store(session.window_open(now), Ordering::Relaxed);
                        }
                        Ran::Yes => {}
                        Ran::Nothing => {}
                    }

                    // Not understood, but nearly something. Ask, once,
                    // and only if there is a voice to ask with: a question
                    // that arrives as a silent banner has nobody waiting
                    // for the answer, so it is left as a notice.
                    if decision == Decision::Unrecognised {
                        let mut asked = false;
                        if let Some(question) = session.ask_about(&part, Instant::now()) {
                            note!("asking   «{part}»  ->  {}?", question.description);
                            if ask_aloud(&question.text) {
                                sound_question();
                                // Timed from here, not from before it
                                // spoke: the six seconds are the ones the
                                // person has to answer in.
                                session.open_question(&part, question, Instant::now());
                                hud::set_question_pending(true);
                                asked = true;
                            }
                        }
                        // Only once the near-miss question above did not
                        // open: a guess from the vocabulary itself beats
                        // one from the model, and asking both would leave
                        // the next "sí" answering whichever was asked
                        // last. `ai::ask_for_command` is self-gating —
                        // off, or `[ai] use` without "unknown", both come
                        // back `None` at no cost beyond the check.
                        if !asked {
                            ai::configure(&config::load());
                            if let Some(suggestion) =
                                ai::ask_for_command(&part, &commands::ai_catalogue())
                            {
                                if suggestion.confidence >= 0.6 {
                                    if let Some(decision) =
                                        commands::decision_for_ai_suggestion(&suggestion.command)
                                    {
                                        let ai_suggestion = session::AiSuggestion {
                                            decision,
                                            description: suggestion.command.clone(),
                                        };
                                        if let Some(question) = session
                                            .ask_ai_suggestion(ai_suggestion, Instant::now())
                                        {
                                            note!(
                                                "asking   «{part}»  ->  {}?  (ai, {:.0}%)",
                                                question.description,
                                                suggestion.confidence * 100.0
                                            );
                                            if ask_aloud(&question.text) {
                                                sound_question();
                                                session.open_question(
                                                    &part,
                                                    question,
                                                    Instant::now(),
                                                );
                                                hud::set_question_pending(true);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if commands::is_sleep(&decision) {
                        active.store(false, Ordering::Relaxed);
                        note!("paused by voice — resume from the menu bar");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Everything a report needs beyond the decision itself: what was heard,
/// how long it took, and where to say so.
///
/// Gathered rather than passed one by one — the flags are read per part, as
/// they were before, so a switch flipped in the preferences window applies
/// to the very next instruction.
struct Reporting<'a> {
    seconds: f32,
    elapsed_ms: u128,
    log_ignored_speech: bool,
    play_sounds: bool,
    acted: &'a AtomicBool,
    status: &'a Status,
}

/// What actually happened to a decision, once it was carried out.
///
/// A plain `bool` used to say only whether it was refused; the conversation
/// window needs the third case too, since neither "not addressed to me" nor
/// "not understood" is a command that ran and is worth extending it for.
#[derive(PartialEq, Eq)]
enum Ran {
    /// `Ignored` or `Unrecognised`: nothing was carried out.
    Nothing,
    /// Understood and carried out.
    Yes,
    /// Understood, but macOS refused it.
    Blocked,
}

/// Carries out an `Outcome::DictateInto` destination: brings its
/// application forward (unless there is none — «dicta en el documento»
/// dictates into whatever is already there), waits for it to actually
/// become frontmost, then types the recipient — through the personal
/// vocabulary, the same as anything else dictated — and the keys that
/// follow it.
///
/// Returns as soon as the destination is ready for dictation to begin;
/// entering dictation itself is `Outcome::DictateInto`'s caller's job, once
/// this returns `Ok`.
fn prepare_destination(destination: &commands::Destination, recipient: Option<&str>) -> Result<(), String> {
    if let Some(bundle_id) = destination.bundle_id {
        actions::open_app(bundle_id)?;
        if !wait_for_frontmost(bundle_id, Duration::from_secs(3)) {
            return Err(format!("{bundle_id} never came to the front"));
        }
    }
    for (code, mods) in destination.keys_before_typing {
        actions::press(*code, *mods)?;
    }
    if let Some(name) = recipient {
        let spelled = dictation::spell_recipient(name, &config::load());
        actions::type_text(&spelled)?;
        for (code, mods) in destination.keys_after_recipient {
            actions::press(*code, *mods)?;
        }
    }
    Ok(())
}

/// Polls [`actions::frontmost_app`] until `bundle_id` is in front, or
/// `timeout` runs out. A launched application takes a moment to come
/// forward, and typing into whatever was in front before it does would
/// send a recipient's name to the wrong window entirely.
fn wait_for_frontmost(bundle_id: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if actions::frontmost_app().as_deref() == Some(bundle_id) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Carries out a decision and writes down what happened.
fn report(
    transcript: &str,
    decision: &Decision,
    confidence: f32,
    repeats: usize,
    at: &Reporting,
) -> Ran {
    let seconds = at.seconds;
    let mut ran = Ran::Nothing;
    match decision {
        Decision::Ignored => {
            // Speech that was not for us. The wording is only written
            // down when explicitly asked for: see log_ignored_speech.
            if at.log_ignored_speech {
                note!("heard    «{transcript}»  (not addressed to me)");
            } else {
                note!("heard    {seconds:.1}s of speech, not addressed to me");
            }
            set_status(at.status, &last_utterance_tooltip(transcript, "no era para mí"));
            hud::push_update(hud::Update {
                heard: Some(transcript.to_string()),
                outcome: Some("no era para mí".to_string()),
            });
        }
        Decision::Unrecognised => {
            note!("unknown  «{transcript}»  ->  not understood");
            set_status(at.status, &last_utterance_tooltip(transcript, "no entendido"));
            hud::push_update(hud::Update {
                heard: Some(transcript.to_string()),
                outcome: Some("no entendido".to_string()),
            });
            if at.play_sounds {
                let _ = actions::play_sound(sounds::UNSURE);
            }
        }
        _ => {
            let mut outcome = None;
            for _ in 0..repeats.max(1) {
                outcome = commands::perform(decision);
            }
            if let Some(done) = outcome {
                ran = if done.outcome.is_err() { Ran::Blocked } else { Ran::Yes };
                if let Err(reason) = &done.outcome {
                    // Understood perfectly and refused by the system.
                    // Almost always the Accessibility permission.
                    note!(
                        "BLOCKED  «{transcript}»  ->  {}: {reason}  — macOS refused it. \
                         Grant Accessibility in System Settings.",
                        done.description
                    );
                    set_status(
                        at.status,
                        &last_utterance_tooltip(transcript, "bloqueado por macOS"),
                    );
                    hud::push_update(hud::Update {
                        heard: Some(transcript.to_string()),
                        outcome: Some("bloqueado por macOS".to_string()),
                    });
                    if at.play_sounds {
                        let _ = actions::play_sound(sounds::BLOCKED);
                    }
                } else {
                    let again = if repeats > 1 {
                        format!(" ×{repeats}")
                    } else {
                        String::new()
                    };
                    let elapsed_ms = at.elapsed_ms;
                    note!(
                        "ran      «{transcript}»  ->  {}{again}  \
                         [{:.0}% · {seconds:.1}s audio · {elapsed_ms} ms]",
                        done.description,
                        confidence * 100.0
                    );
                    at.acted.store(true, Ordering::Relaxed);
                    set_status(at.status, &last_utterance_tooltip(transcript, &done.description));
                    hud::push_update(hud::Update {
                        heard: Some(transcript.to_string()),
                        outcome: Some(done.description.clone()),
                    });
                    if at.play_sounds {
                        let _ = actions::play_sound(sounds::DONE);
                    }
                }
            }
        }
    }
    ran
}

/// Rewrites the "Últimas órdenes" submenu to show `history`, newest first,
/// and refreshes `slots` — the copy of the same information the
/// menu-event thread reads a click's phrase from — to match.
///
/// `items` has one `(slot submenu, "Repetir", "Crear alias…", "Olvidar
/// alias")` per position; a position past the end of `history` is shown
/// disabled rather than removed, since ids are meant to stay put across a
/// rebuild — see where `items` is built, in `run_menu_bar`.
fn refresh_history_menu(
    history: &std::collections::VecDeque<(String, String)>,
    items: &[(Submenu, MenuItem, MenuItem, MenuItem)],
    slots: &Arc<Mutex<Vec<Option<HistorySlot>>>>,
) {
    let config = config::load();
    let Ok(mut slots) = slots.lock() else { return };
    for (n, (slot_menu, repeat, alias, forget)) in items.iter().enumerate() {
        let Some((text, outcome)) = history.get(n) else {
            slot_menu.set_text("(vacío)");
            slot_menu.set_enabled(false);
            repeat.set_enabled(false);
            alias.set_enabled(false);
            forget.set_enabled(false);
            slots[n] = None;
            continue;
        };
        let normalised = text::normalise(text);
        let phrase = commands::strip_wake_word(&normalised).unwrap_or(&normalised).to_string();
        let alias_phrase = config
            .aliases
            .iter()
            .find(|entry| text::normalise(&entry.phrase) == phrase)
            .map(|entry| text::normalise(&entry.phrase));

        slot_menu.set_text(format!("{} → {}", shorten(text, 40), shorten(outcome, 24)));
        slot_menu.set_enabled(true);
        repeat.set_enabled(true);
        alias.set_enabled(true);
        forget.set_enabled(alias_phrase.is_some());
        slots[n] = Some(HistorySlot { phrase, alias_phrase });
    }
}

/// Builds the menu bar item and hands control to AppKit. Never returns.
/// The voice threshold as configured, read fresh.
fn config_voice_threshold() -> f32 {
    config::load().voice_threshold()
}

/// What the menu bar needs to know that is not a switch.
/// Which face the menu bar is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
    Awake,
    Asleep,
    Dictating,
    /// From the end of an utterance until a slow decision (a model reload).
    Thinking,
    /// While `speech::say` is talking back.
    Speaking,
    /// The conversation window is open: the next utterance needs no wake
    /// word. Reuses the "acting" drawing — attentive rather than idle.
    Listening,
    /// The blink after a command; never the resting state.
    Acting,
}

struct Bar {
    /// Where the model is, or will be once it has been downloaded.
    model_path: String,
    /// Set while the first download is running.
    downloading: Arc<AtomicBool>,
    /// The tooltip, as the rest of the program would like it.
    status: Status,
    /// Set while dictating: a different face.
    dictating: Arc<AtomicBool>,
    /// Set while the model is reloading after an utterance: a different face.
    thinking: Arc<AtomicBool>,
    /// Set while speaking a reply aloud: a different face.
    speaking: Arc<AtomicBool>,
    /// Set while the conversation window is open: a different face.
    window_open: Arc<AtomicBool>,
    /// Whether `listen_mode = "hold"`, so the tooltip can say so.
    hold_mode: bool,
    /// Raised once for every `Outcome::Answer` — read by the onboarding
    /// assistant's "Prueba" page.
    answered: Arc<AtomicBool>,
    /// Raised, and never lowered, once the microphone is proven to
    /// deliver only silence.
    mic_denied: Arc<AtomicBool>,
}

fn run_menu_bar(
    bar: Bar,
    active: Arc<AtomicBool>,
    sounds_on: Arc<AtomicBool>,
    log_voices_on: Arc<AtomicBool>,
    training: Training,
    acted: Arc<AtomicBool>,
    // Raised when someone asks aloud what they can say.
    catalogue_asked: Arc<AtomicBool>,
) -> Result<()> {
    let Bar {
        model_path,
        downloading,
        status,
        dictating,
        thinking,
        speaking,
        window_open,
        hold_mode,
        answered,
        mic_denied,
    } = bar;
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow!("the menu bar must be built on the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    // Accessory: menu bar only, no Dock icon and no window.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let menu = Menu::new();
    // One item for one piece of state. Two — "Escuchar" and "Pausar" — made
    // the reader work out which one applied right now.
    let toggle = MenuItem::new(MENU_PAUSE, true, None);
    // No ellipsis: it acts on what it finds and reports, rather than
    // opening something for you to fill in.
    let learn = MenuItem::new("Aprender", true, None);
    let show_log = MenuItem::new("Ver el registro", true, None);
    let stats_item = MenuItem::new("Estadísticas…", true, None);

    // The wake word, a learned alias and a voice profile are all read once
    // at startup, so three different places tell the user to restart
    // Minion from the menu. Until now the menu had no such item.
    let restart = MenuItem::new("Reiniciar", true, None);
    // An ellipsis, unlike "Reiniciar" beside it: this opens a download and
    // a dialog, rather than acting outright.
    let update_packs = MenuItem::new("Actualizar vocabulario…", true, None);
    // Minion itself, rather than its vocabulary. Same ellipsis, same
    // reason: it opens a download and a question.
    let check_update = MenuItem::new("Buscar actualizaciones…", true, None);
    let commands_item = MenuItem::new("Ayuda", true, None);
    let assistant_item = MenuItem::new("Asistente…", true, None);
    let preferences = MenuItem::new("Ajustes…", true, None);
    let vocabulary_item = MenuItem::new("Vocabulario…", true, None);

    // The two things you do with the log, together. Kept in scope for the
    // life of the menu, like every other item.
    let log_menu = Submenu::new("Registro", true);
    log_menu.append(&learn)?;
    log_menu.append(&show_log)?;
    log_menu.append(&stats_item)?;

    // «Últimas órdenes»: fixed slots, updated in place, rather than menu
    // items created and destroyed on every utterance — a slot with nothing
    // in it yet just shows disabled. Ids stay the same across a rebuild, so
    // the event thread below can address a slot by number without needing
    // to know that a rebuild ever happened.
    let history_menu = Submenu::new("Últimas órdenes", true);
    let mut history_items: Vec<(Submenu, MenuItem, MenuItem, MenuItem)> =
        Vec::with_capacity(HISTORY_LEN);
    for n in 0..HISTORY_LEN {
        let slot = Submenu::with_id(format!("history-slot-{n}"), "(vacío)", false);
        let repeat = MenuItem::with_id(format!("history-repeat-{n}"), "Repetir", false, None);
        let alias =
            MenuItem::with_id(format!("history-alias-{n}"), "Crear alias…", false, None);
        let forget =
            MenuItem::with_id(format!("history-forget-{n}"), "Olvidar alias", false, None);
        slot.append(&repeat)?;
        slot.append(&alias)?;
        slot.append(&forget)?;
        history_menu.append(&slot)?;
        history_items.push((slot, repeat, alias, forget));
    }
    let history_repeat_ids: Vec<_> =
        history_items.iter().map(|(_, item, _, _)| item.id().clone()).collect();
    let history_alias_ids: Vec<_> =
        history_items.iter().map(|(_, _, item, _)| item.id().clone()).collect();
    let history_forget_ids: Vec<_> =
        history_items.iter().map(|(_, _, _, item)| item.id().clone()).collect();

    let quit = MenuItem::new("Salir", true, None);
    menu.append(&toggle)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&history_menu)?;
    menu.append(&log_menu)?;
    menu.append(&preferences)?;
    menu.append(&vocabulary_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&check_update)?;
    menu.append(&update_packs)?;
    menu.append(&PredefinedMenuItem::separator())?;
    // Help lives together, above the two ways out.
    menu.append(&assistant_item)?;
    menu.append(&commands_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&restart)?;
    menu.append(&quit)?;

    let toggle_id = toggle.id().clone();
    let learn_id = learn.id().clone();
    let assistant_id = assistant_item.id().clone();
    let preferences_id = preferences.id().clone();
    let vocabulary_id = vocabulary_item.id().clone();
    let commands_id = commands_item.id().clone();
    let show_log_id = show_log.id().clone();
    let stats_id = stats_item.id().clone();
    let restart_id = restart.id().clone();
    let update_packs_id = update_packs.id().clone();
    let check_update_id = check_update.id().clone();
    let quit_id = quit.id().clone();

    // Built once and reused: reopening should bring back the same window,
    // not stack another one behind it.
    let panel = Rc::new(preferences::Preferences::new(mtm));
    let vocabulary_editor = vocabulary_editor::VocabularyEditor::new(mtm);

    // The "what did it hear" HUD — see hud.rs. Starts enabled/pinned
    // exactly as the settings window already read `show_hud`/`hud_pinned`
    // at construction.
    let hud = Rc::new(hud::Hud::new(
        mtm,
        panel.show_hud_on(),
        panel.hud_pinned_on(),
        config::load().hud_seconds(),
    ));
    // Watches this application's keys, so the shortcut button can be set by
    // pressing a combination rather than typing its name.
    let panel_for_capture = Rc::clone(&panel);
    let _capture = preferences::capture_keys(move |code, mods| {
        panel_for_capture.is_capturing() && panel_for_capture.capture(code, mods)
    });
    // Requests from the menu thread, which must not touch AppKit itself.
    let open_requested = Arc::new(AtomicBool::new(false));
    let learn_requested = Arc::new(AtomicBool::new(false));
    let catalogue_requested = Arc::new(AtomicBool::new(false));
    let stats_requested = Arc::new(AtomicBool::new(false));
    // "Asistente…", from the menu.
    let assistant_requested = Arc::new(AtomicBool::new(false));

    let onboarding = Rc::new(onboarding::Window::new(
        mtm,
        onboarding::Shared {
            status: Arc::clone(&status),
            downloading: Arc::clone(&downloading),
            training: Arc::clone(&training),
            model_path: model_path.clone(),
            answered: Arc::clone(&answered),
            mic_denied: Arc::clone(&mic_denied),
            open_settings: Arc::clone(&open_requested),
            open_catalogue: Arc::clone(&catalogue_requested),
        },
    ));

    // On a fresh install there is nothing to discover from a menu bar icon
    // and a wake word nobody has been told about, so the assistant opens
    // once by itself. The marker goes in the configuration file, which is
    // also what creates it.
    let first_run = config::path().is_none_or(|path| !path.exists());
    if first_run {
        let _ = config::set_option("sounds", "true");
        onboarding.show();
    }
    let report = Rc::new(preferences::Report::new(
        mtm,
        "Aprender del registro",
        objc2_foundation::NSSize::new(480.0, 400.0),
    ));
    let catalogue = Rc::new(preferences::Report::new(
        mtm,
        "Qué puedo decirle",
        objc2_foundation::NSSize::new(560.0, 620.0),
    ));
    let stats_window = Rc::new(preferences::Report::new(
        mtm,
        "Estadísticas",
        objc2_foundation::NSSize::new(620.0, 640.0),
    ));

    // Watches this application's keys, so the shortcut button can be set by
    // pressing a combination rather than typing its name.
    let panel_for_capture = Rc::clone(&panel);
    let _capture = preferences::capture_keys(move |code, mods| {
        panel_for_capture.is_capturing() && panel_for_capture.capture(code, mods)
    });
    // «Vocabulario…», from the menu (the other request flags are declared
    // above, before the assistant, which shares them).
    let open_vocabulary_requested = Arc::new(AtomicBool::new(false));

    // What each "Últimas órdenes" slot currently holds, so the menu-event
    // thread — which owns no state of its own — can look one up by number
    // when its "Repetir" or "Olvidar alias" is clicked. Written by the
    // timer below, on the main thread, whenever the tooltip says something
    // new happened.
    let history_slots: Arc<Mutex<Vec<Option<HistorySlot>>>> =
        Arc::new(Mutex::new(vec![None; HISTORY_LEN]));
    // "Olvidar alias" cannot edit config.toml and ask about a restart from
    // the menu-event thread itself — that means calling into AppKit, which
    // only the main thread may do — so it leaves the phrase here instead,
    // for the timer to act on.
    let forget_alias_requested: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Built before anything slow happens. On a first run the model has yet
    // to be downloaded — 670 MB, several minutes — and the icon used to
    // appear only afterwards: for all that time the menu bar showed
    // nothing at all and the progress went to a stdout nobody sees.
    let busy = downloading.load(Ordering::Relaxed);
    let opening_face = if busy { icon::asleep()? } else { icon::awake()? };
    let opening_tooltip = status
        .lock()
        .ok()
        .filter(|text| !text.is_empty())
        .map_or_else(|| TOOLTIP_IDLE.to_string(), |text| text.clone());

    // Held for the lifetime of the process: dropping it removes the icon.
    let tray = Rc::new(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(opening_face)
            // A template image: macOS tints it to match the menu bar, so it
            // is black on a light one and white on a dark one.
            .with_icon_as_template(true)
            .with_tooltip(&opening_tooltip)
            .build()?,
    );

    // The state can change from the menu or from a spoken "deja de
    // escuchar", and the second happens on the recognition thread, which
    // must not touch AppKit. A timer on the main run loop is the bridge: it
    // polls the flag and repaints the icon and the menu item together.
    let tray_for_timer = Rc::clone(&tray);
    let toggle_for_timer = toggle.clone();
    let active_for_timer = Arc::clone(&active);
    let shown_face = Cell::new(if busy { Face::Asleep } else { Face::Awake });
    // Which of the two talking frames is up, while a reply is spoken.
    let speaking_frame: Cell<u32> = Cell::new(0);
    let dictating_for_timer = Arc::clone(&dictating);
    let thinking_for_timer = Arc::clone(&thinking);
    let speaking_for_timer = Arc::clone(&speaking);
    let window_open_for_timer = Arc::clone(&window_open);
    let shown_tooltip = std::cell::RefCell::new(opening_tooltip);
    let status_for_timer = Arc::clone(&status);
    let downloading_for_timer = Arc::clone(&downloading);
    let acted_for_timer = Arc::clone(&acted);
    let blink_until: Cell<Option<std::time::Instant>> = Cell::new(None);
    let panel_for_timer = Rc::clone(&panel);
    let vocabulary_editor_for_timer = Rc::clone(&vocabulary_editor);
    let hud_for_timer = Rc::clone(&hud);
    let onboarding_for_timer = Rc::clone(&onboarding);
    let assistant_for_timer = Arc::clone(&assistant_requested);
    let open_for_timer = Arc::clone(&open_requested);
    let open_vocabulary_for_timer = Arc::clone(&open_vocabulary_requested);
    let learn_for_timer = Arc::clone(&learn_requested);
    let training_for_timer = Arc::clone(&training);
    let catalogue_for_timer = Arc::clone(&catalogue_requested);
    let catalogue_window = Rc::clone(&catalogue);
    let catalogue_asked_aloud = Arc::clone(&catalogue_asked);
    let stats_for_timer = Arc::clone(&stats_requested);
    let stats_window_for_timer = Rc::clone(&stats_window);
    // The submenu is only ever touched from here — set_text/set_enabled are
    // cheap, so it is simplest to just rebuild the visible slots whenever
    // the parsed history changes, rather than diffing against what is
    // already shown.
    let history_items_for_timer = history_items.clone();
    let history_slots_for_timer = Arc::clone(&history_slots);
    let history: Rc<std::cell::RefCell<std::collections::VecDeque<(String, String)>>> =
        Rc::new(std::cell::RefCell::new(std::collections::VecDeque::with_capacity(HISTORY_LEN)));
    let forget_alias_for_timer = Arc::clone(&forget_alias_requested);
    let training_model_path = model_path;
    let report_for_timer = Rc::clone(&report);
    let sounds_for_timer = Arc::clone(&sounds_on);
    let voices_for_timer = Arc::clone(&log_voices_on);
    let restart_requested = Arc::new(AtomicBool::new(false));
    let quit_requested = Arc::new(AtomicBool::new(false));
    let restart_for_timer = Arc::clone(&restart_requested);
    let quit_for_timer = Arc::clone(&quit_requested);
    // "Actualizar vocabulario…": the click is noticed here, but the
    // download itself runs on its own worker thread — like the model
    // download, it must not block the run loop this timer fires on.
    // `packs_updating` stops a second click from starting a second
    // download while one is already in flight; `packs_update_result` is
    // where the worker leaves what happened for this timer to show.
    let packs_update_requested = Arc::new(AtomicBool::new(false));
    let packs_updating = Arc::new(AtomicBool::new(false));
    let packs_update_result: Arc<Mutex<Option<PacksUpdateResult>>> = Arc::new(Mutex::new(None));
    let packs_update_for_timer = Arc::clone(&packs_update_requested);
    let packs_updating_for_timer = Arc::clone(&packs_updating);
    let packs_update_result_for_timer = Arc::clone(&packs_update_result);
    // «Buscar actualizaciones…» and the daily automatic check, which are
    // the same work told apart by `update_check_asked`: both look, only
    // the one someone asked for says so when there is nothing new.
    // Network and unzipping happen on a worker thread, like the packs
    // download above; everything with a dialog in it happens here.
    let update_check_requested = Arc::new(AtomicBool::new(false));
    let update_check_asked = Arc::new(AtomicBool::new(false));
    let update_busy = Arc::new(AtomicBool::new(false));
    let update_result: Arc<Mutex<Option<UpdateResult>>> = Arc::new(Mutex::new(None));
    let update_check_for_timer = Arc::clone(&update_check_requested);
    let update_asked_for_timer = Arc::clone(&update_check_asked);
    let update_busy_for_timer = Arc::clone(&update_busy);
    let update_result_for_timer = Arc::clone(&update_result);
    // The copy the last update left behind is only thrown away once this
    // version has started, which is the one proof the replacement works.
    let previous_discarded: Cell<bool> = Cell::new(false);
    let update_ticks: Cell<u32> = Cell::new(0);
    let update_check_period =
        ticks_per_second(UI_REFRESH_SECONDS).saturating_mul(UPDATE_CHECK_SECONDS as u32);
    // How many times the timer has fired since it started, kept apart from
    // the UI throttle's own counter so the two gates — "once a second" and
    // whatever the throttle needs — can be reasoned about, and changed,
    // independently of each other.
    let request_check_ticks: Cell<u32> = Cell::new(0);
    let request_check_period = ticks_per_second(UI_REFRESH_SECONDS);
    // Its own counter, independent of the one above: the two gates run on
    // different periods and neither should have to know the other exists.
    let throttle_ticks: Cell<u32> = Cell::new(0);
    let repaint = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        // A second copy that lost the single-instance lock left this
        // instead of opening a window of its own — see
        // `request_settings_open`. Checked on a schedule, not every tick:
        // metadata() is cheap, but the timer fires far more often than the
        // file could plausibly appear.
        let request_check_tick = request_check_ticks.get().wrapping_add(1);
        request_check_ticks.set(request_check_tick);
        if on_schedule(request_check_tick, request_check_period) {
            if let Some(path) = open_settings_request_path() {
                if std::fs::metadata(&path).is_ok() {
                    let _ = std::fs::remove_file(&path);
                    open_for_timer.store(true, Ordering::Relaxed);
                }
            }

            // Timers and alarms due since the last check — see
            // answers::announce_due_timers for the chime, the spoken
            // reply and the notification each one gets.
            answers::announce_due_timers();

            // Requests left by `minion run "…"` / `minion say "…"`: a
            // second process, typically a keyboard shortcut, asking this
            // running copy to act or speak. Handled here rather than on
            // the listening thread — see api.rs for why — so a `run`
            // cannot continue a dictation session or be undone with
            // "deshaz", and the speaker check does not apply to it at all.
            let requests = api::pending();
            if !requests.is_empty() {
                let config = config::load();
                let voice = config.voice();
                let device = config.speaker();
                let context = actions::frontmost_app();
                let reply = api::ReplySettings {
                    voice: voice.as_deref(),
                    rate: config.speech_rate(),
                    device: device.as_deref(),
                    speak: config.speak,
                    notifications: config.notifications,
                };
                for request in &requests {
                    api::handle(request, &reply, context.as_deref());
                }
            }

            // Written only when it disagrees with what is already on
            // disk, so `minion status` has something to read without the
            // timer rewriting the file once a second regardless.
            let listening = active_for_timer.load(Ordering::Relaxed);
            let on_disk = api::read_status_file();
            api::write_status_if_changed(listening, on_disk.as_deref());
        }

        // Once this version is running, the copy the previous update
        // kept has served its purpose. Done here rather than at startup
        // so it happens on the run loop, with everything else that
        // touches the disk outside the listening thread.
        if !previous_discarded.replace(true) {
            updater::discard_previous();
        }

        // The daily look for a new Minion. The rule itself is a day (see
        // `updater::daily_check_due`); this only asks the question every
        // few minutes, and never while a check or an install is already
        // running.
        let update_tick = update_ticks.get().wrapping_add(1);
        update_ticks.set(update_tick);
        if on_schedule(update_tick, update_check_period)
            && !update_busy_for_timer.load(Ordering::Relaxed)
            && updater::daily_check_due(&config::load())
        {
            updater::record_check();
            update_asked_for_timer.store(false, Ordering::Relaxed);
            update_check_for_timer.store(true, Ordering::Relaxed);
        }

        // The rest of this closure is the expensive part: reading every
        // control, deciding the menu bar's face, repainting it. Worth doing
        // at full speed while a slider might be moving or the blink that
        // acknowledges a command is still showing; otherwise it is work
        // nobody can see, so it only runs often enough that a menu toggle
        // still repaints promptly.
        let throttle_tick = throttle_ticks.get().wrapping_add(1);
        throttle_ticks.set(throttle_tick);
        let watched = panel_for_timer.is_visible()
            || onboarding_for_timer.is_visible()
            || vocabulary_editor_for_timer.is_visible()
            || blink_until.get().is_some()
            || speaking_for_timer.load(Ordering::Relaxed)
            // The HUD's own ellipsis animates on a 300 ms cycle — the
            // throttled 250 ms tick below would make it stutter.
            || hud_for_timer.is_visible();
        if !watched && !on_schedule(throttle_tick, UI_THROTTLE_TICKS) {
            return;
        }

        // The HUD's own visibility (a four-second countdown since the last
        // thing worth showing) needs checking every throttled tick, not
        // only when the menu bar's own face changes — see hud.rs.
        let hud_now = std::time::Instant::now();
        hud_for_timer.set_enabled(panel_for_timer.show_hud_on());
        hud_for_timer.set_pinned(panel_for_timer.hud_pinned_on());
        hud_for_timer.set_dictating(dictating_for_timer.load(Ordering::Relaxed));
        let hud_awake =
            active_for_timer.load(Ordering::Relaxed) && !downloading_for_timer.load(Ordering::Relaxed);
        hud_for_timer.set_face(if !hud_awake {
            hud::Face::Asleep
        } else if speaking_for_timer.load(Ordering::Relaxed) {
            hud::Face::Speaking
        } else if thinking_for_timer.load(Ordering::Relaxed) {
            hud::Face::Thinking
        } else if dictating_for_timer.load(Ordering::Relaxed) {
            hud::Face::Dictating
        } else if window_open_for_timer.load(Ordering::Relaxed) {
            hud::Face::Acting
        } else {
            hud::Face::Awake
        });
        hud_for_timer.tick(hud_now);

        if open_for_timer.swap(false, Ordering::Relaxed) {
            panel_for_timer.show();
        }
        if assistant_for_timer.swap(false, Ordering::Relaxed) {
            onboarding_for_timer.show();
        }
        // Reads the assistant's controls and repaints its page — see
        // `onboarding::Window::poll`. Before the training-session block
        // below, so a finished session's message is seen here first.
        onboarding_for_timer.poll();
        if open_vocabulary_for_timer.swap(false, Ordering::Relaxed) {
            vocabulary_editor_for_timer.show();
        }
        // Leaving, by restart or quit: let the settings window save what
        // it still holds (a wake word typed but not yet committed) first.
        let restarting = restart_for_timer.swap(false, Ordering::Relaxed);
        let quitting = quit_for_timer.swap(false, Ordering::Relaxed);
        if restarting || quitting {
            panel_for_timer.poll();
            if restarting {
                note!("restarting from the menu");
                relaunch();
            }
            // Zero: neither is a crash, and the launch agent must not race
            // the copy that was just started.
            std::process::exit(0);
        }
        // Voice training: the window asks, the listening loop answers.
        if let Some(name) = panel_for_timer.take_training_request() {
            if let Ok(mut session) = training_for_timer.lock() {
                *session = Some(enroll::Session::starting(training_model_path.clone(), &name));
                panel_for_timer.show_training(
                    &session.as_ref().map_or(String::new(), |s| s.message.clone()),
                    false,
                );
            }
        } else if let Ok(session) = training_for_timer.lock() {
            if let Some(active_session) = session.as_ref() {
                panel_for_timer
                    .show_training(&active_session.message, active_session.finished);
            }
        }
        // «Cancelar» in the window: forget the session before it finishes.
        if panel_for_timer.take_cancel_request() {
            if let Ok(mut session) = training_for_timer.lock() {
                *session = None;
            }
        }
        // Clear a finished session once its message has been shown.
        if let Ok(mut session) = training_for_timer.lock() {
            if session.as_ref().is_some_and(|s| s.finished) {
                *session = None;
            }
        }

        if catalogue_asked_aloud.swap(false, Ordering::Relaxed)
            || catalogue_for_timer.swap(false, Ordering::Relaxed)
        {
            catalogue_window.show(&commands::catalogue());
        }
        if stats_for_timer.swap(false, Ordering::Relaxed) {
            // The last 30 days: recent enough to act on, short enough that
            // the window does not turn into the whole rotated log.
            stats_window_for_timer.show(&metrics::report_text(Some(30)));
        }
        if learn_for_timer.swap(false, Ordering::Relaxed) {
            let config = config::load();
            let lesson = learn::analyse(&config);
            report_for_timer.show(&lesson.summary());
            if !lesson.teachable.is_empty()
                && actions::ask(
                    &format!(
                        "{} frase(s) se parecen a una orden que ya existe.\n\
                         ¿Añadirlas como alias?",
                        lesson.teachable.len()
                    ),
                    "Añadir",
                )
            {
                match learn::apply(&lesson) {
                    Ok(n) if n > 0 => {
                        note!("learned {n} alias(es) from the log");
                        if actions::ask_choice(
                            &format!("Añadidos {n}. Se aplican al reiniciar Minion."),
                            "Reiniciar ahora",
                            "Reiniciar más tarde",
                        ) {
                            restart_for_timer.store(true, Ordering::Relaxed);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => actions::show_message(&e),
                }
            }
        }
        // "Olvidar alias", from the "Últimas órdenes" menu: routed through
        // here rather than acted on directly by the menu-event thread,
        // since removing it means asking a question and possibly a
        // restart, both of which are AppKit calls.
        if let Some(phrase) = forget_alias_for_timer.lock().ok().and_then(|mut p| p.take()) {
            if actions::ask(
                &format!("¿Olvidar el alias «{phrase}»?"),
                "Olvidar",
            ) {
                match config::remove_alias(&phrase) {
                    Ok(()) => {
                        note!("alias «{phrase}» forgotten from the Últimas órdenes menu");
                        if actions::ask_choice(
                            "El cambio se aplica al reiniciar Minion.",
                            "Reiniciar ahora",
                            "Reiniciar más tarde",
                        ) {
                            restart_for_timer.store(true, Ordering::Relaxed);
                        }
                    }
                    Err(e) => actions::show_message(&format!("No se pudo olvidar el alias: {e}")),
                }
            }
        }
        // «Actualizar vocabulario…»: start the download on a worker thread,
        // once, and let it report progress through the same tooltip the
        // model download already uses. Ignored while one is already
        // running, rather than queued — a second click is someone
        // impatient, not a second update to run.
        if packs_update_for_timer.swap(false, Ordering::Relaxed)
            && !packs_updating_for_timer.swap(true, Ordering::Relaxed)
        {
            note!("updating vocabulary packs from the menu");
            let status_for_worker = Arc::clone(&status_for_timer);
            let result_for_worker = Arc::clone(&packs_update_result_for_timer);
            let updating_for_worker = Arc::clone(&packs_updating_for_timer);
            std::thread::spawn(move || {
                let status_for_report = Arc::clone(&status_for_worker);
                let outcome = packs::update(|progress| {
                    if let Ok(mut text) = status_for_report.lock() {
                        *text = format!("Minion — {progress}");
                    }
                });
                let result = match outcome {
                    Ok(installed) => {
                        PacksUpdateResult::Installed { version: installed.version, packs: installed.packs }
                    }
                    Err(reason) => PacksUpdateResult::Failed(reason.message()),
                };
                if let Ok(mut slot) = result_for_worker.lock() {
                    *slot = Some(result);
                }
                updating_for_worker.store(false, Ordering::Relaxed);
            });
        }
        // The worker above finished: show what happened and put the
        // tooltip back to what it would say anyway, since the progress
        // line it wrote is not something the natural refresh below ever
        // overwrites by itself.
        if let Some(result) = packs_update_result_for_timer.lock().ok().and_then(|mut r| r.take()) {
            match result {
                PacksUpdateResult::Installed { version, packs } => {
                    note!("vocabulary {version} installed: {packs} packs");
                    if actions::ask_choice(
                        &format!(
                            "Vocabulario {version} instalado: {packs} packs. \
                             Reinicia para aplicarlo."
                        ),
                        "Reiniciar ahora",
                        "Reiniciar más tarde",
                    ) {
                        restart_for_timer.store(true, Ordering::Relaxed);
                    }
                }
                PacksUpdateResult::Failed(message) => {
                    note!("vocabulary update failed: {message}");
                    actions::show_message(&message);
                }
            }
            if let Ok(mut text) = status_for_timer.lock() {
                *text = if hold_mode {
                    if active_for_timer.load(Ordering::Relaxed) {
                        TOOLTIP_HOLD_ACTIVE
                    } else {
                        TOOLTIP_HOLD_IDLE
                    }
                } else if active_for_timer.load(Ordering::Relaxed) {
                    TOOLTIP_LISTENING
                } else {
                    TOOLTIP_PAUSED
                }
                .to_string();
            }
        }
        // A check for a new Minion, asked for from the menu or by the
        // daily timer above. One at a time: `update_busy` covers the
        // whole check-download-install run, so a second click while an
        // update is downloading is ignored rather than starting it twice.
        if update_check_for_timer.swap(false, Ordering::Relaxed)
            && !update_busy_for_timer.swap(true, Ordering::Relaxed)
        {
            let asked = update_asked_for_timer.load(Ordering::Relaxed);
            note!("updater  checking for a new version ({})", if asked { "asked" } else { "daily" });
            let result_for_worker = Arc::clone(&update_result_for_timer);
            let busy_for_worker = Arc::clone(&update_busy_for_timer);
            std::thread::spawn(move || {
                let check = updater::check();
                if let Ok(mut slot) = result_for_worker.lock() {
                    *slot = Some(UpdateResult::Checked { check, asked });
                }
                busy_for_worker.store(false, Ordering::Relaxed);
            });
        }
        // What the worker found. Everything here is AppKit — a question,
        // a message, a restart — which is why none of it is on the
        // worker itself.
        if let Some(result) = update_result_for_timer.lock().ok().and_then(|mut r| r.take()) {
            match result {
                UpdateResult::Checked { check: updater::Check::Available(release), .. } => {
                    note!("updater  {} is available", release.version);
                    if actions::ask_choice(
                        &format!(
                            "Minion {} disponible. ¿Instalar?\n\n{}",
                            release.version, release.notes
                        ),
                        "Instalar",
                        "Más tarde",
                    ) && !update_busy_for_timer.swap(true, Ordering::Relaxed)
                    {
                        let status_for_worker = Arc::clone(&status_for_timer);
                        let result_for_worker = Arc::clone(&update_result_for_timer);
                        let busy_for_worker = Arc::clone(&update_busy_for_timer);
                        std::thread::spawn(move || {
                            let outcome = updater::install(&release, |progress| {
                                if let Ok(mut text) = status_for_worker.lock() {
                                    *text = format!("Minion — {progress}");
                                }
                            });
                            let result = match outcome {
                                Ok(_) => UpdateResult::Installed(release.version.clone()),
                                Err(reason) => UpdateResult::Failed(reason),
                            };
                            if let Ok(mut slot) = result_for_worker.lock() {
                                *slot = Some(result);
                            }
                            busy_for_worker.store(false, Ordering::Relaxed);
                        });
                    }
                }
                // Nothing new, still private, or a check that failed:
                // said out loud only to whoever asked. The daily check is
                // silent by design — it runs on a machine nobody is
                // looking at.
                UpdateResult::Checked { check, asked } => {
                    note!("updater  {}", check.message());
                    if asked {
                        actions::show_message(&check.message());
                    }
                }
                UpdateResult::Installed(version) => {
                    note!("updater  installed {version}");
                    if actions::ask_choice(
                        &format!("Minion {version} instalado. Reinicia para usarlo."),
                        "Reiniciar ahora",
                        "Reiniciar más tarde",
                    ) {
                        restart_for_timer.store(true, Ordering::Relaxed);
                    }
                }
                UpdateResult::Failed(reason) => {
                    note!("updater  update failed: {reason}");
                    actions::show_message(&format!("No se pudo actualizar Minion: {reason}"));
                }
            }
        }
        // Controls report by being read: see preferences.rs for why.
        if panel_for_timer.poll() {
            sounds_for_timer.store(panel_for_timer.sounds_on(), Ordering::Relaxed);
            voices_for_timer.store(panel_for_timer.log_voices_on(), Ordering::Relaxed);
        }
        if panel_for_timer.take_restart_request() {
            // Picked up on the next tick, after the window has saved.
            restart_for_timer.store(true, Ordering::Relaxed);
        }
        if panel_for_timer.take_edit_vocabulary_request() {
            vocabulary_editor_for_timer.show();
        }
        vocabulary_editor_for_timer.poll();
        if vocabulary_editor_for_timer.take_restart_request() {
            restart_for_timer.store(true, Ordering::Relaxed);
        }

        // A command just ran: acknowledge it for a moment. Silent, which
        // matters once the sounds are turned off.
        if acted_for_timer.swap(false, Ordering::Relaxed) {
            if let Ok(face) = icon::acting() {
                let _ = tray_for_timer.set_icon_with_as_template(Some(face), true);
            }
            blink_until.set(Some(std::time::Instant::now()));
        }
        if let Some(since) = blink_until.get() {
            if since.elapsed().as_secs_f64() >= BLINK_SECONDS {
                blink_until.set(None);
                shown_face.set(Face::Acting); // whatever is right, repaint it
            } else {
                return; // hold the acknowledgement
            }
        }

        // The tooltip is the only place a background app can say what it
        // is doing without interrupting anyone.
        if let Ok(wanted) = status_for_timer.lock() {
            if !wanted.is_empty() && *wanted != *shown_tooltip.borrow() {
                let _ = tray_for_timer.set_tooltip(Some(&*wanted));
                shown_tooltip.replace(wanted.clone());
                // "Últimas órdenes" reads the same line — kept as a tooltip
                // parse on purpose, unlike the HUD (which the listening
                // loop now feeds directly through `hud::push_update`): this
                // menu only wants the finished pair, once, and the tooltip
                // is already exactly that.
                // "no era para mí" is speech Minion decided was not
                // addressed to it at all, so it is not an order to keep.
                if let Some((text, outcome)) = parse_last_utterance(&wanted) {
                    if outcome != "no era para mí" {
                        let mut queue = history.borrow_mut();
                        if queue.len() == HISTORY_LEN {
                            queue.pop_back();
                        }
                        queue.push_front((text, outcome));
                        drop(queue);
                        refresh_history_menu(
                            &history.borrow(),
                            &history_items_for_timer,
                            &history_slots_for_timer,
                        );
                    }
                }
            }
        }

        let listening = active_for_timer.load(Ordering::Relaxed);
        // Downloading is not listening, whatever the switch says.
        let awake = listening && !downloading_for_timer.load(Ordering::Relaxed);
        let wanted = match (
            awake,
            speaking_for_timer.load(Ordering::Relaxed),
            thinking_for_timer.load(Ordering::Relaxed),
            dictating_for_timer.load(Ordering::Relaxed),
            window_open_for_timer.load(Ordering::Relaxed),
        ) {
            (false, _, _, _, _) => Face::Asleep,
            (true, true, _, _, _) => Face::Speaking,
            (true, false, true, _, _) => Face::Thinking,
            (true, false, false, true, _) => Face::Dictating,
            (true, false, false, false, true) => Face::Listening,
            (true, false, false, false, false) => Face::Awake,
        };
        if wanted == shown_face.get() {
            // Talking is the one resting face that moves: two frames,
            // swapped every SPEAKING_FRAME_TICKS ticks of 50 ms.
            if wanted == Face::Speaking {
                let frame = (throttle_tick / SPEAKING_FRAME_TICKS) % 2;
                if frame != speaking_frame.get() {
                    speaking_frame.set(frame);
                    let face = if frame == 0 { icon::speaking() } else { icon::speaking_closed() };
                    if let Ok(face) = face {
                        let _ = tray_for_timer.set_icon_with_as_template(Some(face), true);
                    }
                }
            }
            return;
        }
        let was_awake = shown_face.get() != Face::Asleep;
        shown_face.set(wanted);
        // Pausing and resuming happen from three places — the menu, the
        // shortcut and a spoken order — and this is the one that sees all
        // three, because they all end up flipping the same flag.
        if !downloading_for_timer.load(Ordering::Relaxed) && was_awake != awake {
            if let Ok(mut text) = status_for_timer.lock() {
                *text = if hold_mode {
                    if awake { TOOLTIP_HOLD_ACTIVE } else { TOOLTIP_HOLD_IDLE }
                } else if awake {
                    TOOLTIP_LISTENING
                } else {
                    TOOLTIP_PAUSED
                }
                .to_string();
            }
        }
        let face = match wanted {
            Face::Awake | Face::Acting => icon::awake(),
            Face::Asleep => icon::asleep(),
            Face::Dictating => icon::dictating(),
            Face::Thinking => icon::thinking(),
            Face::Speaking => icon::speaking(),
            Face::Listening => icon::acting(),
        };
        if let Ok(face) = face {
            let _ = tray_for_timer.set_icon_with_as_template(Some(face), true);
        }
        toggle_for_timer.set_text(if listening { MENU_PAUSE } else { MENU_LISTEN });
    });
    // Scheduled in the common modes rather than the default one. While a
    // slider is being dragged, AppKit runs the loop in event-tracking mode
    // and timers registered only for the default mode stop firing — which
    // is exactly when the readout beside the slider needs to keep up.
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_repeats_block(UI_REFRESH_SECONDS, true, &repaint)
    };
    // Lets AppKit coalesce this timer's firing with others rather than
    // waking the process for it alone — the tick counters above already
    // mean most firings do nothing, so exactness buys nothing here.
    timer.setTolerance(0.02);
    unsafe {
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    let _timer = timer;

    // Menu events arrive on a global channel, which is Send, so they can be
    // serviced from another thread while AppKit owns the main one. The icon
    // and the item's text are repainted by the timer above, not from here.
    let open_from_menu = Arc::clone(&open_requested);
    let assistant_from_menu = Arc::clone(&assistant_requested);
    let open_vocabulary_from_menu = Arc::clone(&open_vocabulary_requested);
    let learn_from_menu = Arc::clone(&learn_requested);
    let restart_from_menu = Arc::clone(&restart_requested);
    let quit_from_menu = Arc::clone(&quit_requested);
    let catalogue_from_menu = Arc::clone(&catalogue_requested);
    let stats_from_menu = Arc::clone(&stats_requested);
    let history_slots_from_menu = Arc::clone(&history_slots);
    let forget_alias_from_menu = Arc::clone(&forget_alias_requested);
    let update_packs_from_menu = Arc::clone(&packs_update_requested);
    let update_check_from_menu = Arc::clone(&update_check_requested);
    let update_asked_from_menu = Arc::clone(&update_check_asked);
    std::thread::spawn(move || {
        let events = MenuEvent::receiver();
        while let Ok(event) = events.recv() {
            if event.id == toggle_id {
                let now_listening = !active.load(Ordering::Relaxed);
                active.store(now_listening, Ordering::Relaxed);
                note!(
                    "{} from the menu",
                    if now_listening { "resumed" } else { "paused" }
                );
            } else if event.id == learn_id {
                learn_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == commands_id {
                catalogue_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == assistant_id {
                // Windows belong to the main thread; the timer opens it.
                assistant_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == preferences_id {
                // Windows belong to the main thread; the timer opens it.
                open_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == vocabulary_id {
                open_vocabulary_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == stats_id {
                stats_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == show_log_id {
                if let Some(path) = journal::path() {
                    if let Err(reason) = actions::reveal(&path.to_string_lossy()) {
                        note!("could not reveal the log: {reason}");
                    }
                }
            } else if event.id == restart_id {
                // Handed to the timer: it can flush the settings window
                // first, and a word still being typed there was otherwise
                // lost to the restart meant to apply it.
                restart_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == update_packs_id {
                update_packs_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == check_update_id {
                // Asked for by hand, so it reports either way — including
                // "está al día", which the daily check never says.
                update_asked_from_menu.store(true, Ordering::Relaxed);
                update_check_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == quit_id {
                note!("quit from the menu");
                quit_from_menu.store(true, Ordering::Relaxed);
            } else if let Some(n) = history_repeat_ids.iter().position(|id| *id == event.id) {
                let slot = history_slots_from_menu.lock().ok().and_then(|s| s[n].clone());
                if let Some(slot) = slot {
                    // No AppKit involved — a plain file write, exactly what
                    // `minion run "…"` already does from a second process.
                    if let Err(e) = api::request_run(&slot.phrase) {
                        note!("could not repeat «{}»: {e}", slot.phrase);
                    }
                }
            } else if history_alias_ids.contains(&event.id) {
                // First version, per the report: this reopens the same
                // "Aprender" dialog the menu already has, rather than
                // offering an alias for this one phrase specifically.
                learn_from_menu.store(true, Ordering::Relaxed);
            } else if let Some(n) = history_forget_ids.iter().position(|id| *id == event.id) {
                let alias_phrase = history_slots_from_menu
                    .lock()
                    .ok()
                    .and_then(|s| s[n].clone())
                    .and_then(|slot| slot.alias_phrase);
                if let Some(phrase) = alias_phrase {
                    if let Ok(mut pending) = forget_alias_from_menu.lock() {
                        *pending = Some(phrase);
                    }
                }
            }
        }
    });

    app.run();
    Ok(())
}

/// How to start a fresh copy of Minion, as a command and its arguments.
///
/// Inside a bundle it has to be `open -n` on the `.app` rather than the
/// binary: run directly, the executable loses its Info.plist, and with it
/// the accessory activation policy — a Dock icon appears and the menu bar
/// item does not.
///
/// Split out from [`relaunch`] so the shape can be checked without
/// starting anything.
fn relaunch_arguments(executable: &Path) -> Vec<std::path::PathBuf> {
    let bundle = executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|kind| kind == "app"));
    match bundle {
        Some(app) => vec!["/usr/bin/open".into(), "-n".into(), app.to_path_buf()],
        None => vec![executable.to_path_buf()],
    }
}

/// Starts a fresh copy, a moment after this one has gone.
///
/// The wait is not politeness: the single-instance lock is held by an open
/// file descriptor and only released when the process ends, so a copy that
/// starts too early finds the lock taken and quietly exits, leaving no
/// Minion at all. The paths are passed as arguments rather than
/// interpolated into the script, so nothing about them can be read as
/// shell.
///
/// Under launchd none of that would work: when a job's main process exits,
/// launchd kills everything left in its process group, the waiting `sh`
/// included, and the restart never happens — the first «Reiniciar» closed
/// Minion and nothing came back. So the launch agent, when it is running
/// this copy, is asked to do the restart itself (`kickstart -k` stops and
/// starts the job); only a copy started by hand falls back to the shell,
/// put into a session of its own so the exit cannot take it along.
fn relaunch() {
    if startup::kickstart() {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(r#"sleep 2; exec "$0" "$@""#);
    command.args(relaunch_arguments(&executable));
    // Safety: setsid only detaches the child from this process group and
    // touches no memory shared with the parent.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = command.spawn();
}

/// Refuses to start if another copy is already running.
///
/// Two copies means two faces in the menu bar, two microphones open and
/// every command run twice. The lock is held by the file descriptor, so it
/// is released when the process ends however it ends — no stale lock file
/// to clean up after a crash.
fn claim_sole_instance() -> bool {
    use std::os::unix::io::IntoRawFd;

    let Some(mut path) = config::path() else {
        return true;
    };
    path.set_file_name("running.lock");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    else {
        return true; // cannot lock: better to run than to refuse wrongly
    };

    // Leaked on purpose: the lock must outlive this function and last as
    // long as the process does.
    let fd = file.into_raw_fd();
    let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        unsafe { libc::close(fd) };
    }
    locked
}

/// Where a second copy leaves its "open the settings" request for the
/// running one to find.
///
/// Next to the config file rather than its own new folder: the directory
/// is already there and already private to this user.
fn open_settings_request_path() -> Option<std::path::PathBuf> {
    let mut path = config::path()?;
    path.set_file_name("open-settings");
    Some(path)
}

/// Leaves the request for the running copy, in place of doing anything
/// itself.
///
/// A double-click that finds Minion already running has nowhere else to
/// go: it cannot open a window, because the window belongs to the copy
/// that is about to keep running, not this one.
fn request_settings_open() {
    if cfg!(test) {
        return;
    }
    let Some(path) = open_settings_request_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, "");
}

/// How many ticks of the UI timer make up about a second.
///
/// Pure, so the "once a second" gate below can be tested without a real
/// timer.
fn ticks_per_second(refresh_seconds: f64) -> u32 {
    if refresh_seconds <= 0.0 {
        1
    } else {
        (1.0 / refresh_seconds).round().max(1.0) as u32
    }
}

/// Whether this tick of the UI timer is one of the ones that does
/// something, given how many ticks make up one period.
///
/// Used by the settings-request check below (about once a second); the UI
/// throttle reuses it for its own, shorter period.
fn on_schedule(tick: u32, period_ticks: u32) -> bool {
    period_ticks > 0 && tick.is_multiple_of(period_ticks)
}

/// When this Mac last started, in seconds since the epoch.
///
/// Used as the name of the current login session: it changes at every
/// boot and at nothing else, so a marker carrying it says "already done
/// this time round" without needing a timer or a file to clean up.
fn boot_time() -> Option<i64> {
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let mut boot = libc::timeval { tv_sec: 0, tv_usec: 0 };
    let mut size = std::mem::size_of::<libc::timeval>();
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::addr_of_mut!(boot).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (read == 0).then_some(boot.tv_sec)
}

/// Where the "I already opened the Accessibility pane" marker lives.
fn accessibility_marker() -> Option<std::path::PathBuf> {
    let mut path = config::path()?;
    path.set_file_name("accessibility-prompted");
    Some(path)
}

/// Whether the marker was written during this same boot.
///
/// Pure so the policy can be tested without a filesystem: an unreadable or
/// missing marker, or one from a previous boot, means "not yet".
fn prompted_this_boot(marker: Option<&str>, boot: Option<i64>) -> bool {
    match (marker, boot) {
        (Some(written), Some(now)) => written.trim().parse::<i64>() == Ok(now),
        _ => false,
    }
}

/// Records that the pane has been opened during this boot.
fn remember_accessibility_prompt(boot: Option<i64>) {
    if cfg!(test) {
        return;
    }
    let (Some(path), Some(boot)) = (accessibility_marker(), boot) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, boot.to_string());
}

/// Says plainly whether the key-pressing commands can work at all.
///
/// Worth its own step because the failure is invisible: without the
/// permission, ⌘W is posted and silently dropped, so the log shows the
/// command running while nothing happens on screen.
fn report_permissions() {
    if actions::has_accessibility_permission() {
        note!("Accessibility granted — key commands will work.");
        return;
    }
    note!(
        "NO Accessibility permission. Opening apps, volume and music will \
         work; anything that presses keys (copy, save, close tab) will be \
         silently ignored by macOS."
    );
    // Ask macOS to prompt. This is the call that registers Minion with the
    // system, so that there is a switch to turn on when the pane opens —
    // an app that never asked simply is not in the list.
    actions::request_accessibility_permission();
    println!(
        "\n  Accept the dialog, or switch Minion on in System Settings →\n  \
         Privacy & Security → Accessibility. Then restart Minion from the\n  \
         menu bar: the permission is only read at startup.\n"
    );
    // Once per login, not once per start. The launch agent restarts Minion
    // whenever it exits badly, and each restart used to throw System
    // Settings in the user's face again.
    let boot = boot_time();
    let marker = accessibility_marker().and_then(|path| std::fs::read_to_string(path).ok());
    if prompted_this_boot(marker.as_deref(), boot) {
        note!("Accessibility pane already offered since this Mac started; not reopening it.");
        return;
    }
    if let Err(reason) = actions::open_accessibility_settings() {
        note!("could not open the Accessibility pane: {reason}");
    }
    remember_accessibility_prompt(boot);
}

/// Stops, having said why.
///
/// Everything that can go wrong before Minion is listening — no
/// microphone, no model, a corrupt one — used to reach stderr alone, which
/// under launchd goes to a file nobody opens: the user saw a menu bar with
/// no icon, or an icon that never did anything. So it is written down, and
/// shown.
///
/// Exits with 0 on purpose. The launch agent restarts on a non-zero exit,
/// and a configuration problem does not fix itself between two tries: it
/// would reopen this dialog every `ThrottleInterval` seconds until someone
/// killed it.
fn fatal(message: &str) -> ! {
    note!("fatal    {message}");
    // Waited for, not merely queued: `exit` would otherwise take the
    // dialog with it before anyone saw it.
    actions::show_message_and_wait(message);
    std::process::exit(0)
}

fn main() -> Result<()> {
    // A panic aborts (see [profile.release]), and an abort leaves nothing
    // behind. Written down first, so the restart that follows can be
    // explained afterwards rather than guessed at.
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("panicked");
        let place = info.location().map_or_else(
            || "an unknown place".to_string(),
            |at| format!("{}:{}", at.file(), at.line()),
        );
        journal::write(&format!("panic    {message} at {place}"));
    }));

    // `minion learn` reads the log and turns its failures into vocabulary.
    // It touches neither the microphone nor the model, so it is handled
    // before any of that is set up.
    let first_argument = std::env::args().nth(1);
    if let Some(argument) = first_argument.as_deref() {
        // The version, from the crate metadata: `Cargo.toml` is the one
        // place it is written down, and `build-app.sh` reads the same
        // line for the bundle's Info.plist.
        if matches!(argument, "--version" | "-V" | "version") {
            println!("minion {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        if matches!(argument, "--help" | "-h" | "help") {
            // Spanish: everything the person running this reads is in
            // Spanish, and this is read by nobody else. Without it,
            // `minion --help` went looking for a model called «--help».
            println!(
                "Minion {version} — control por voz en español.\n\n\
                 Uso:\n  \
                 minion                      escucha y obedece (el uso normal)\n  \
                 minion <ruta-al-modelo>     igual, con el modelo de esa carpeta\n  \
                 minion learn [--apply]      convierte en alias lo que no entendió\n  \
                 minion enroll [nombre]      aprende una voz desde la terminal\n\
                 \x20                            (sin nombre, la guarda como «yo»)\n  \
                 minion export-icon <dir>    guarda el icono como .iconset\n  \
                 minion run \"orden\"          la ejecuta, como si la hubieras dicho\n  \
                 minion say \"texto\"          lo dice en voz alta\n  \
                 minion status               si Minion está escuchando o en pausa\n  \
                 minion packs update         descarga el vocabulario de la comunidad\n  \
                 minion mic                  qué apps usan ahora el micrófono\n  \
                 minion loopback [seg]       mide lo que el Mac está sonando\n  \
                 minion ai \"pregunta\"        la responde con la IA configurada\n  \
                 minion ai status            qué backend de IA hay y qué agentes\n  \
                 minion ai models            lista los modelos del backend configurado\n  \
                 minion ai set-key <prov>    guarda una clave en el llavero (por\n  \
                 la entrada estándar)\n  \
                 minion stats [--days N]     informe de reconocimiento (todo el \
                 registro, o los últimos N días)\n  \
                 minion corpus <carpeta> [--save]\n\
                 \x20                            mide el reconocimiento contra un\n\
                 \x20                            corpus de grabaciones (ver\n\
                 \x20                            minion/corpus/README.md)\n  \
                 minion corpus --from-log <grabaciones> <destino>\n\
                 \x20                            arranca un corpus.toml a partir de\n\
                 \x20                            una carpeta de grabaciones y el registro\n  \
                 minion --version            la versión instalada\n  \
                 minion --help               esto\n\n\
                 «run» y «say» dejan un aviso para la copia que ya está en\n\
                 marcha y no hacen nada si no hay ninguna — útil para atajos\n\
                 de teclado (Atajos.app, Raycast): un atajo con\n\
                 «minion run \"cierra la pestaña\"» hace lo mismo que decirlo.\n\n\
                 Variables de entorno:\n  \
                 MINION_MODEL                carpeta del modelo de reconocimiento\n\n\
                 Registro: ~/Library/Logs/minion.log\n\
                 Ajustes:  ~/Library/Application Support/Minion/config.toml",
                version = env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        if argument == "export-icon" {
            let directory = std::env::args().nth(2).unwrap_or_else(|| "Minion.iconset".into());
            icon::export_iconset(&directory)?;
            println!("Icon written to {directory}");
            return Ok(());
        }
        if argument == "enroll" {
            let model_path = locate_model(None)?;
            // Without a name the profile is «yo», which is what the single
            // profile of earlier versions becomes when it is migrated.
            let name = std::env::args()
                .nth(2)
                .unwrap_or_else(|| speaker::DEFAULT_NAME.to_string());
            return enroll::run(&model_path, &name);
        }
        if argument == "learn" {
            let apply = std::env::args().any(|a| a == "--apply");
            let config = config::load();
    if let Some(problem) = config::problem() {
        // Said out loud, not just logged: a file that has stopped parsing
        // means every setting is silently back to its default.
        note!("config   {problem}");
        actions::show_message(&format!(
            "No se puede leer config.toml, así que Minion usa los ajustes por \
             defecto.\n\n{problem}\n\nCorrige el archivo y reinicia Minion."
        ));
    }
            commands::configure(&config);
            learn::run(&config, apply);
            return Ok(());
        }
        if argument == "run" || argument == "say" {
            let Some(text) = std::env::args().nth(2) else {
                eprintln!("Uso: minion {argument} \"texto\"");
                std::process::exit(1);
            };
            let request = if argument == "run" { api::request_run(&text) } else { api::request_say(&text) };
            if let Err(reason) = request {
                eprintln!("No se pudo dejar el aviso para Minion: {reason}");
                std::process::exit(1);
            }
            return Ok(());
        }
        if argument == "status" {
            api::print_status();
            return Ok(());
        }
        if argument == "loopback" {
            let seconds = std::env::args()
                .nth(2)
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(5);
            if let Err(e) = loopback::probe(seconds) {
                eprintln!("No se pudo escuchar la salida del Mac: {e:#}");
                std::process::exit(1);
            }
            return Ok(());
        }
        if argument == "packs" {
            if std::env::args().nth(2).as_deref() != Some("update") {
                eprintln!("Uso: minion packs update");
                std::process::exit(1);
            }
            if let Some(current) = packs::installed_version() {
                println!("Versión instalada actualmente: {current}");
            }
            match packs::update(|progress| println!("{progress}")) {
                Ok(installed) => {
                    println!(
                        "Vocabulario {} instalado: {} packs. Reinicia Minion para aplicarlo.",
                        installed.version, installed.packs
                    );
                    return Ok(());
                }
                Err(outcome) => {
                    eprintln!("{}", outcome.message());
                    std::process::exit(1);
                }
            }
        }
        // The AI layer, from the terminal: the same engine a spoken
        // question would reach, reading the same `[ai]` settings and
        // spending from the same daily budget, with none of the audio
        // machinery in the way.
        if argument == "ai" {
            ai::configure(&config::load());
            match std::env::args().nth(2).as_deref() {
                Some("status") | None => println!("{}", ai::status_text()),
                Some("set-key") => {
                    let Some(provider) = std::env::args().nth(3) else {
                        eprintln!("Uso: minion ai set-key <proveedor>   (la clave se lee de la entrada estándar)");
                        std::process::exit(1);
                    };
                    // The key is read from stdin, never from an argument:
                    // `ps` shows every argument on this machine to every
                    // process on it.
                    if let Err(reason) = ai::set_key_from_stdin(&provider) {
                        eprintln!("No se pudo guardar la clave: {reason}");
                        std::process::exit(1);
                    }
                    println!("Clave guardada en el llavero para «{provider}».");
                }
                Some("models") => {
                    let settings = ai::Settings::from_config(&config::load().ai);
                    if !settings.enabled() {
                        eprintln!("La IA no está configurada: falta «backend» en [ai].");
                        std::process::exit(1);
                    }
                    match ai::list_models(&settings.backend) {
                        Ok(models) => {
                            for option in ai::model_popup_options(&settings.backend, &models) {
                                println!("{}", option.label);
                            }
                        }
                        Err(why) => {
                            eprintln!("No se pudo listar los modelos: {why}");
                            std::process::exit(1);
                        }
                    }
                }
                Some(text) => ai::run_from_terminal(text),
            }
            return Ok(());
        }
        // For checking the automatic pause by hand: pick up a call, run
        // this, and see the same thing Minion sees.
        if argument == "mic" {
            microphone::report();
            return Ok(());
        }
        if argument == "stats" {
            let args: Vec<String> = std::env::args().collect();
            let days = args
                .windows(2)
                .find(|pair| pair[0] == "--days")
                .and_then(|pair| pair[1].parse::<u32>().ok());
            println!("{}", metrics::report_text(days));
            return Ok(());
        }
        // Measures recognition rather than running Minion, so it is handled
        // here alongside `learn` and `enroll`: no microphone, no instance
        // lock, no menu bar.
        if argument == "corpus" {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            if rest.first().map(String::as_str) == Some("--from-log") {
                let (Some(recordings_dir), Some(output_dir)) = (rest.get(1), rest.get(2)) else {
                    eprintln!("Uso: minion corpus --from-log <carpeta-grabaciones> <carpeta-destino>");
                    std::process::exit(1);
                };
                return corpus::bootstrap_from_log(Path::new(recordings_dir), Path::new(output_dir));
            }
            let save = rest.iter().any(|a| a == "--save");
            let Some(dir) = rest.iter().find(|a| *a != "--save") else {
                eprintln!("Uso: minion corpus <carpeta> [--save]");
                std::process::exit(1);
            };
            return corpus::run(Path::new(dir), save);
        }
    }

    if !claim_sole_instance() {
        eprintln!("Minion ya se está ejecutando.");
        request_settings_open();
        return Ok(());
    }

    // Where the model is — or, on a first run, where it is about to be.
    // The download itself happens on the worker thread, once the icon
    // exists to report it.
    let found = find_model(first_argument);
    let downloading = Arc::new(AtomicBool::new(found.is_none()));
    let status: Status = Arc::new(Mutex::new(
        if found.is_some() { TOOLTIP_LISTENING } else { TOOLTIP_IDLE }.to_string(),
    ));
    let Some(model_path) = found.or_else(|| {
        models::directory().map(|path| path.to_string_lossy().into_owned())
    }) else {
        fatal(
            "Minion no encuentra el modelo de reconocimiento y no sabe dónde \
             descargarlo: no hay carpeta personal.",
        );
    };

    let config = config::load();
    commands::configure(&config);
    let audio_settings = config.audio_settings();
    // Shared so the menu can flip them while the loop is running: a switch
    // that needs a restart to take effect is not much of a switch.
    let log_ignored = Arc::new(AtomicBool::new(config.log_ignored_speech));
    let play_sounds = Arc::new(AtomicBool::new(config.sounds));
    let idle_unload = config.idle_unload();

    // Voice recognition is opt-in: it exists only once someone has run
    // `minion enroll`. Without a profile Minion answers anyone who says the
    // wake word, which is the right default for a machine with one user.
    let training: Training = Arc::new(Mutex::new(None));
    let worker_training = Arc::clone(&training);

    let profiles = speaker::load_profiles_for(&model_path);
    let voice = if profiles.is_empty() {
        None
    } else {
        match speaker::Speaker::load(&model_path) {
            Ok(model) => {
                let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
                note!(
                    "Voice profiles loaded ({}) — only these voices will be obeyed.",
                    names.join(", ")
                );
                Some(Voice { model, profiles, threshold: config.voice_threshold() })
            }
            Err(e) => {
                note!("Voice profiles found but the speaker model would not load: {e:#}");
                None
            }
        }
    };

    note!("Minion starting — loading model…");
    if let Some(log) = journal::path() {
        println!("Log: {}", log.display());
    }
    report_permissions();
    let listen_mode = config.listen_mode();
    // Push-to-talk starts silent: nothing is held yet. "Always" starts
    // listening, as it always has.
    let active = Arc::new(AtomicBool::new(listen_mode == config::ListenMode::Always));
    if listen_mode == config::ListenMode::Hold && !downloading.load(Ordering::Relaxed) {
        // The status set before the config was read assumed "always"; say
        // what push-to-talk actually starts as.
        set_status(&status, TOOLTIP_HOLD_IDLE);
    }
    // Raised by the listening loop when a command runs, lowered by the run
    // loop once the icon has blinked.
    let acted = Arc::new(AtomicBool::new(false));
    // Set by the listening loop while dictating, read by the run loop for
    // the face in the menu bar.
    let dictating = Arc::new(AtomicBool::new(false));
    let worker_dictating = Arc::clone(&dictating);
    // Same idea, for the "thinking" and "speaking" faces.
    let thinking = Arc::new(AtomicBool::new(false));
    let worker_thinking = Arc::clone(&thinking);
    let speaking = Arc::new(AtomicBool::new(false));
    let worker_speaking = Arc::clone(&speaking);
    let window_open = Arc::new(AtomicBool::new(false));
    let worker_window_open = Arc::clone(&window_open);
    // Read by the onboarding assistant, on the main thread; written by the
    // listening loop, on its own.
    let answered = Arc::new(AtomicBool::new(false));
    let worker_answered = Arc::clone(&answered);
    let mic_denied = Arc::new(AtomicBool::new(false));
    let worker_mic_denied = Arc::clone(&mic_denied);
    let conversation_window = config.conversation_window();
    let pause_when_microphone_busy = config.pause_when_microphone_busy;

    let worker_active = Arc::clone(&active);
    let worker_log_ignored = Arc::clone(&log_ignored);
    let worker_sounds = Arc::clone(&play_sounds);
    let worker_acted = Arc::clone(&acted);
    let microphone = config.microphone();
    let save_recordings = config.save_recordings;
    let catalogue_asked = Arc::new(AtomicBool::new(false));
    let worker_catalogue = Arc::clone(&catalogue_asked);
    // `[feedback]`'s "sounds" and "quiet" modes silence spoken replies on
    // top of whatever `speak` already says, the same way `feedback_mode`
    // adds to `play_sounds` for earcons rather than replacing it.
    let voice_reply = (config.speak && config.feedback().speaks()).then(|| VoiceReply {
        voice: config.voice(),
        rate: config.speech_rate(),
        device: config.speaker(),
    });
    let worker_downloading = Arc::clone(&downloading);
    let worker_status = Arc::clone(&status);
    let menu_bar = Bar {
        model_path: model_path.clone(),
        downloading: Arc::clone(&downloading),
        status: Arc::clone(&status),
        dictating: Arc::clone(&dictating),
        thinking: Arc::clone(&thinking),
        speaking: Arc::clone(&speaking),
        window_open: Arc::clone(&window_open),
        hold_mode: listen_mode == config::ListenMode::Hold,
        answered: Arc::clone(&answered),
        mic_denied: Arc::clone(&mic_denied),
    };
    std::thread::spawn(move || {
        // First run: 670 MB before anything can be heard. Reported through
        // the tooltip, which is where a menu bar app can say what it is
        // doing without a window and without interrupting anyone.
        if worker_downloading.load(Ordering::Relaxed) {
            note!("Downloading the recognition model (about 670 MB, once).");
            set_status(&worker_status, "Minion — descargando el modelo… 0 %");
            let target = std::path::PathBuf::from(&model_path);
            if let Err(e) = models::fetch(&target, |progress| {
                set_status(&worker_status, &format!("Minion — {progress}"));
            }) {
                fatal(&format!(
                    "Minion no ha podido descargar el modelo de reconocimiento.\n\n{e}\n\n\
                     Comprueba la conexión y vuelve a abrir Minion."
                ));
            }
            note!("Model downloaded.");
            worker_downloading.store(false, Ordering::Relaxed);
            set_status(&worker_status, TOOLTIP_LISTENING);
        }
        if let Err(e) = listen_and_obey(Listening {
            model_path,
            audio: audio_settings,
            log_ignored_speech: worker_log_ignored,
            play_sounds: worker_sounds,
            idle_unload,
            voice,
            training: worker_training,
            active: worker_active,
            acted: worker_acted,
            dictating: worker_dictating,
            thinking: worker_thinking,
            speaking: worker_speaking,
            conversation_window,
            window_open: worker_window_open,
            hold_mode: listen_mode == config::ListenMode::Hold,
            pause_when_microphone_busy,
            voice_reply,
            microphone,
            show_catalogue: worker_catalogue,
            save_recordings,
            status: Arc::clone(&worker_status),
            answered: worker_answered,
            mic_denied: worker_mic_denied,
        }) {
            fatal(&format!(
                "Minion no puede escuchar y va a cerrarse.\n\n{e:#}\n\nComprueba \
                 el micrófono en Ajustes del Sistema → Privacidad y seguridad → \
                 Micrófono."
            ));
        }
    });

    // Kept alive for the life of the process: dropping it stops the watch.
    if let Some(shortcut) = config.resume_shortcut() {
        let mode = match listen_mode {
            config::ListenMode::Always => hotkey::Mode::Toggle,
            config::ListenMode::Hold => hotkey::Mode::Hold,
        };
        if hotkey::watch(&shortcut, Arc::clone(&active), mode) {
            match listen_mode {
                config::ListenMode::Always => note!("Shortcut {shortcut} pauses and resumes."),
                config::ListenMode::Hold => {
                    note!("Push-to-talk: listens only while {shortcut} is held.");
                }
            }
        } else {
            note!("Cannot read the shortcut «{shortcut}»; ignoring it.");
        }
    } else if listen_mode == config::ListenMode::Hold {
        note!(
            "listen_mode is \"hold\" but resume_shortcut is empty — Minion \
             has no key to listen while held and will stay silent."
        );
    }

    run_menu_bar(
        menu_bar,
        active,
        play_sounds,
        log_ignored,
        training,
        acted,
        catalogue_asked,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tooltip_says_what_was_heard_and_what_came_of_it() {
        assert_eq!(
            last_utterance_tooltip("  minion, abre Chrome  ", "abrir Chrome"),
            "Minion — última: “minion, abre Chrome” → abrir Chrome"
        );
    }

    #[test]
    fn a_long_sentence_is_shortened_rather_than_wrapped() {
        let long = "minion, escribe que mañana por la mañana tengo que llamar al fontanero";
        let tooltip = last_utterance_tooltip(long, "escribir");
        assert!(tooltip.contains('…'), "should be cut: {tooltip}");
        assert!(!tooltip.contains("fontanero"), "and cut at the right place: {tooltip}");
    }

    #[test]
    fn first_sentence_keeps_only_the_first_and_its_punctuation() {
        assert_eq!(first_sentence("Han pasado cinco minutos. ¿Cancelo el resto?"), "Han pasado cinco minutos.");
        assert_eq!(first_sentence("Sin punto final"), "Sin punto final");
        assert_eq!(first_sentence("¿Qué hora es?"), "¿Qué hora es?");
        assert_eq!(first_sentence(""), "");
    }

    #[test]
    fn a_tooltip_round_trips_through_parse_last_utterance() {
        let tooltip = last_utterance_tooltip("minion, abre Chrome", "abrir Chrome");
        assert_eq!(
            parse_last_utterance(&tooltip),
            Some(("minion, abre Chrome".to_string(), "abrir Chrome".to_string()))
        );
    }

    #[test]
    fn parse_last_utterance_rejects_an_unrelated_tooltip() {
        assert_eq!(parse_last_utterance(TOOLTIP_IDLE), None);
        assert_eq!(parse_last_utterance(TOOLTIP_LISTENING), None);
    }

    #[test]
    fn a_bundled_minion_restarts_through_its_bundle() {
        let inside = Path::new("/Applications/Minion.app/Contents/MacOS/minion");
        assert_eq!(
            relaunch_arguments(inside),
            vec![
                std::path::PathBuf::from("/usr/bin/open"),
                std::path::PathBuf::from("-n"),
                std::path::PathBuf::from("/Applications/Minion.app"),
            ]
        );
    }

    #[test]
    fn a_bare_binary_restarts_itself() {
        let built = Path::new("/Users/someone/minion/target/release/minion");
        assert_eq!(relaunch_arguments(built), vec![built.to_path_buf()]);
    }

    #[test]
    fn a_marker_from_this_boot_stops_the_pane_reopening() {
        assert!(prompted_this_boot(Some("1725350400"), Some(1_725_350_400)));
        assert!(prompted_this_boot(Some("1725350400\n"), Some(1_725_350_400)));
    }

    #[test]
    fn a_marker_from_a_previous_boot_does_not_count() {
        assert!(!prompted_this_boot(Some("1725350400"), Some(1_725_360_000)));
    }

    #[test]
    fn no_marker_and_no_boot_time_mean_offer_it() {
        assert!(!prompted_this_boot(None, Some(1_725_350_400)));
        assert!(!prompted_this_boot(Some("1725350400"), None));
        assert!(!prompted_this_boot(Some("not a number"), Some(1_725_350_400)));
    }

    #[test]
    fn the_boot_time_is_a_plausible_moment_in_the_past() {
        // It must be stable across calls, or it would be useless as the
        // name of a login session.
        let boot = boot_time().expect("macOS knows when it started");
        assert!(boot > 1_000_000_000, "the epoch is not a boot time");
        assert_eq!(Some(boot), boot_time(), "it must not move while running");
    }

    #[test]
    fn a_bundled_minion_never_looks_at_the_working_directory() {
        let inside = Path::new("/Applications/Minion.app/Contents/MacOS/minion");
        assert!(is_bundled(inside));
        let candidates = model_candidates(inside, !is_bundled(inside));
        assert_eq!(
            candidates,
            vec![
                Path::new("/Applications/Minion.app/Contents/MacOS/../Resources/model"),
                Path::new("/Applications/Minion.app/Contents/MacOS/model"),
            ]
        );
    }

    #[test]
    fn a_development_build_also_checks_the_working_directory() {
        let built = Path::new("/Users/someone/minion/target/debug/minion");
        assert!(!is_bundled(built));
        let candidates = model_candidates(built, !is_bundled(built));
        assert_eq!(
            candidates,
            vec![
                Path::new("/Users/someone/minion/target/debug/../Resources/model"),
                Path::new("/Users/someone/minion/target/debug/model"),
                Path::new("model"),
                Path::new("../model"),
            ]
        );
    }

    #[test]
    fn twenty_ticks_of_fifty_milliseconds_make_a_second() {
        assert_eq!(ticks_per_second(UI_REFRESH_SECONDS), 20);
    }

    #[test]
    fn a_schedule_fires_on_its_multiples_only() {
        assert!(!on_schedule(1, 20));
        assert!(!on_schedule(19, 20));
        assert!(on_schedule(20, 20));
        assert!(on_schedule(40, 20));
    }

    #[test]
    fn the_settings_request_lives_beside_the_config_file() {
        // Reads HOME to build the path but touches no filesystem — same as
        // `config::path()`, which this is deliberately built alongside.
        let request = open_settings_request_path().expect("HOME is set while testing");
        let config = config::path().expect("HOME is set while testing");
        assert_eq!(request.parent(), config.parent());
        assert_eq!(request.file_name().unwrap(), "open-settings");
    }
}

//! Minion — control your Mac by speaking Spanish.
//!
//! Listens on the microphone, transcribes with Parakeet, and carries out
//! sentences that open with the wake word. Lives in the menu bar.
//!
//! Thread layout matters here: AppKit insists the menu bar is created and
//! serviced on the main thread, so recognition — the expensive part — runs
//! on its own.

mod actions;
mod answers;
mod api;
mod audio;
mod commands;
mod config;
mod enroll;
mod fbank;
mod hotkey;
mod icon;
mod journal;
mod learn;
mod models;
mod notify;
mod preferences;
mod session;
mod spanish;
mod speech;
mod startup;
mod speaker;
mod text;
mod timers;

use std::cell::Cell;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use ort::session::builder::SessionBuilder;
use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIconBuilder;

use commands::Decision;
use session::{Outcome, Session, Undoable};

/// What the tooltip says when there is nothing more particular to report.
const TOOLTIP_IDLE: &str = "Minion — control por voz";
const TOOLTIP_LISTENING: &str = "Minion — escuchando";
const TOOLTIP_PAUSED: &str = "Minion — en pausa";

/// How much of a transcript the tooltip carries.
///
/// Long enough to recognise the sentence, short enough that the tooltip
/// stays one line. What was heard in full is in the log.
const TOOLTIP_TRANSCRIPT: usize = 48;

/// The tooltip line for an utterance and what came of it.
///
/// This is the whole visible trace of what Minion just did: the sounds can
/// be turned off, the icon only blinks, and the log is a file. Pure, so
/// the shortening can be tested.
fn last_utterance_tooltip(transcript: &str, outcome: &str) -> String {
    let trimmed = transcript.trim();
    let short: String = if trimmed.chars().count() > TOOLTIP_TRANSCRIPT {
        trimmed.chars().take(TOOLTIP_TRANSCRIPT - 1).collect::<String>() + "…"
    } else {
        trimmed.to_string()
    };
    format!("Minion — última: “{short}” → {outcome}")
}

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

/// How often the recognition loop wakes up between utterances.
///
/// Short, because this is also how quickly it notices that someone has
/// started talking while the model is unloaded: a second of loading that
/// happens while the sentence is still being spoken is a second the
/// speaker never waits for. The wake-up itself is one atomic read.
const IDLE_CHECK: Duration = Duration::from_millis(250);

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

/// The recognition loop. Owns the model and runs on its own thread.
/// Loads the speech model.
fn load_model(model_path: &str) -> Result<ParakeetTDT> {
    ParakeetTDT::from_pretrained(model_path, Some(inference_config()))
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading the model from '{model_path}'"))
}

struct Voice {
    model: speaker::Speaker,
    profile: speaker::Embedding,
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
    voice: Option<Voice>,
    training: Training,
    active: Arc<AtomicBool>,
    /// Raised when something runs, so the menu bar can acknowledge it.
    acted: Arc<AtomicBool>,
    /// Set while in dictation mode, so the menu bar can show it.
    dictating: Arc<AtomicBool>,
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
        mut voice,
        training,
        active,
        acted,
        dictating,
        voice_reply,
        microphone,
        show_catalogue,
        save_recordings,
        status,
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
    note!("Model loaded. {}", resident_memory());

    // How long to stay deaf after speaking: the segmenter needs
    // `silence_end_ms` of quiet before it closes an utterance, so anything
    // shorter hands Minion its own answer just after the flag comes down.
    let speech_tail = Duration::from_millis(settings.silence_end_ms as u64 + 200);
    let listener =
        audio::start(settings, Arc::clone(&active), microphone, Arc::clone(&deaf))
            .context("opening the microphone")?;
    note!(
        "Microphone: {} Hz, {} channel(s). {} phrases understood.",
        listener.source_hz,
        listener.channels,
        commands::phrase_count()
    );
    // The wake word can be changed in preferences, and telling someone to
    // say «minion» when it no longer answers to that is worse than saying
    // nothing.
    let wake = commands::wake_words().first().copied().unwrap_or("minion");
    note!("Listening. Say: «{wake}, abre Chrome»");

    loop {
        // A bounded wait, so idleness can be noticed while nothing is being
        // said. A plain recv() would block until the next utterance, and
        // the model would stay loaded through an empty afternoon.
        let utterance = match listener.utterances.recv_timeout(IDLE_CHECK) {
            Ok(utterance) => utterance,
            Err(mpsc::RecvTimeoutError::Timeout) => {
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
                // Nothing but exact zeros since the stream opened: macOS
                // denied the microphone. Said once, with somewhere to go.
                if listener.silent.swap(false, Ordering::Relaxed) {
                    note!("deaf     the microphone delivers only silence — permission denied?");
                    let _ = std::process::Command::new("/usr/bin/open")
                        .arg(MICROPHONE_SETTINGS)
                        .status();
                    actions::show_message(
                        "Minion no puede oír: activa el micrófono para Minion en \
                         Ajustes del Sistema → Privacidad y seguridad → Micrófono",
                    );
                }
                if let Some(idle_for) = idle_unload {
                    if model.is_some() && last_used.elapsed() >= idle_for {
                        model = None;
                        note!("Idle for {} min — model released. {}",
                              idle_for.as_secs() / 60, resident_memory());
                    }
                }
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
                            profile: Vec::new(),
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
                        // Adopt what was just learned, without a restart.
                        if let Some(profile) = speaker::load_profile_for(&model_path) {
                            if let Some(v) = voice.as_mut() {
                                v.profile = profile;
                                v.threshold = config_voice_threshold();
                            }
                        }
                    }
                }
            }
            continue;
        }

        // Whose voice this is, decided before transcribing: someone else's
        // speech should not reach the recogniser at all, let alone the log.
        if let Some(voice) = voice.as_mut() {
            // A model loaded for training but with no profile yet means
            // there is nothing to compare against, so anyone is obeyed.
            let known_voice = !voice.profile.is_empty();
            if known_voice {
                match voice.model.embed(utterance.speech()) {
                    Some(heard) => {
                        let likeness = speaker::similarity(&heard, &voice.profile);
                        if likeness < voice.threshold {
                            note!("heard    {seconds:.1}s in another voice ({likeness:.2})");
                            continue;
                        }
                        // Logged on the way through as well: without both
                        // sides, there is no way to tell a threshold that
                        // is too high from a profile that is wrong.
                        note!("voice    matched at {likeness:.2}");
                    }
                    // Under the hard floor: a cough, a door, half a
                    // syllable. Let through, but say so, because this is
                    // the one path where the voice check does not run.
                    None => note!("voice    {seconds:.1}s too short to check — let through"),
                }
            }
        }

        // Reload if it was released while idle. Costs about a second, once.
        if model.is_none() {
            match load_model(&model_path) {
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

        // One sentence can hold several instructions joined by "y luego".
        for part in session.split(&transcript) {
            // Which application is in front decides what some phrases mean,
            // and it is read per instruction: the first of a chain may well
            // have changed which application that is.
            let context = actions::frontmost_app();
            let (decision, confidence) = commands::decide_in(&part, context.as_deref());

            match session.interpret(&part, decision, context.as_deref()) {
                Outcome::EnterDictation => {
                    note!("dictation started — say «deja de dictar» to stop");
                    dictating.store(true, Ordering::Relaxed);
                    acted.store(true, Ordering::Relaxed);
                }
                Outcome::LeaveDictation => {
                    dictating.store(false, Ordering::Relaxed);
                    note!("dictation ended");
                }
                Outcome::NotDictating => note!("not dictating"),
                // Heard while dictating, with nothing in it to type.
                Outcome::Nothing => {}
                Outcome::Type(typed) => match actions::type_text(&format!("{typed} ")) {
                    Ok(()) => {
                        note!("typed    «{typed}»");
                        acted.store(true, Ordering::Relaxed);
                    }
                    Err(reason) => note!("BLOCKED  «{typed}»  ->  escribir texto: {reason}"),
                },
                Outcome::Answer(question) => {
                    // "¿Qué puedes hacer?" is answered by showing the list.
                    if question == answers::Question::Help {
                        show_catalogue.store(true, Ordering::Relaxed);
                    }
                    let listening = active.load(Ordering::Relaxed);
                    let reply = answers::answer(question, listening);
                    note!("asked    «{part}»  ->  {reply}");
                    set_status(&status, &last_utterance_tooltip(&part, &reply));
                    acted.store(true, Ordering::Relaxed);
                    match &voice_reply {
                        Some(settings) => {
                            speech::say(
                                &reply,
                                settings.voice.as_deref(),
                                settings.rate,
                                settings.device.as_deref(),
                                &deaf,
                                speech_tail,
                            );
                            // Whatever arrived while it was talking is its
                            // own voice, or was said over it. Either way it
                            // was not meant as an instruction.
                            while listener.utterances.try_recv().is_ok() {}
                        }
                        None => actions::show_message(&reply),
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
                    let refused = report(
                        &part,
                        &decision,
                        confidence,
                        repeats,
                        &Reporting {
                            seconds,
                            elapsed_ms,
                            log_ignored_speech: log_ignored_speech.load(Ordering::Relaxed),
                            play_sounds: play_sounds.load(Ordering::Relaxed),
                            acted: &acted,
                            status: &status,
                        },
                    );
                    if refused {
                        // Nothing happened, so there is nothing to undo.
                        session.forget_undo();
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

/// Carries out a decision and writes down what happened.
fn report(
    transcript: &str,
    decision: &Decision,
    confidence: f32,
    repeats: usize,
    at: &Reporting,
) -> bool {
    let seconds = at.seconds;
    let mut refused = false;
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
        }
        Decision::Unrecognised => {
            note!("unknown  «{transcript}»  ->  not understood");
            set_status(at.status, &last_utterance_tooltip(transcript, "no entendido"));
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
                refused = done.outcome.is_err();
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
                    if at.play_sounds {
                        let _ = actions::play_sound(sounds::DONE);
                    }
                }
            }
        }
    }
    refused
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
    let Bar { model_path, downloading, status, dictating } = bar;
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

    // The wake word, a learned alias and a voice profile are all read once
    // at startup, so three different places tell the user to restart
    // Minion from the menu. Until now the menu had no such item.
    let restart = MenuItem::new("Reiniciar", true, None);
    let commands_item = MenuItem::new("Ayuda", true, None);
    let preferences = MenuItem::new("Ajustes…", true, None);

    // The two things you do with the log, together. Kept in scope for the
    // life of the menu, like every other item.
    let log_menu = Submenu::new("Registro", true);
    log_menu.append(&learn)?;
    log_menu.append(&show_log)?;

    let quit = MenuItem::new("Salir", true, None);
    menu.append(&toggle)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&log_menu)?;
    menu.append(&preferences)?;
    menu.append(&PredefinedMenuItem::separator())?;
    // Help sits with Quit rather than among the working items: it is where
    // you look when you do not know what to do, not part of the routine.
    menu.append(&restart)?;
    menu.append(&commands_item)?;
    menu.append(&quit)?;

    let toggle_id = toggle.id().clone();
    let learn_id = learn.id().clone();
    let preferences_id = preferences.id().clone();
    let commands_id = commands_item.id().clone();
    let show_log_id = show_log.id().clone();
    let restart_id = restart.id().clone();
    let quit_id = quit.id().clone();

    // Built once and reused: reopening should bring back the same window,
    // not stack another one behind it.
    let panel = Rc::new(preferences::Preferences::new(mtm));

    // On a fresh install there is nothing to discover from a menu bar icon
    // and a wake word nobody has been told about, so the window opens once
    // by itself. The marker goes in the configuration file, which is also
    // what creates it.
    let first_run = config::path().is_none_or(|path| !path.exists());
    if first_run {
        let _ = config::set_option("sounds", "true");
        actions::show_message(
            "Minion escucha por el micrófono y obedece cuando empiezas por \
             «minion».\n\nPrueba: «minion, abre Chrome».\n\nEn esta ventana \
             puedes ajustar cómo escucha y enseñarle tu voz.",
        );
        panel.show();
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
    let dictating_for_timer = Arc::clone(&dictating);
    let shown_tooltip = std::cell::RefCell::new(opening_tooltip);
    let status_for_timer = Arc::clone(&status);
    let downloading_for_timer = Arc::clone(&downloading);
    let acted_for_timer = Arc::clone(&acted);
    let blink_until: Cell<Option<std::time::Instant>> = Cell::new(None);
    let panel_for_timer = Rc::clone(&panel);
    let open_for_timer = Arc::clone(&open_requested);
    let learn_for_timer = Arc::clone(&learn_requested);
    let training_for_timer = Arc::clone(&training);
    let catalogue_for_timer = Arc::clone(&catalogue_requested);
    let catalogue_window = Rc::clone(&catalogue);
    let catalogue_asked_aloud = Arc::clone(&catalogue_asked);
    let training_model_path = model_path;
    let report_for_timer = Rc::clone(&report);
    let sounds_for_timer = Arc::clone(&sounds_on);
    let voices_for_timer = Arc::clone(&log_voices_on);
    let restart_requested = Arc::new(AtomicBool::new(false));
    let quit_requested = Arc::new(AtomicBool::new(false));
    let restart_for_timer = Arc::clone(&restart_requested);
    let quit_for_timer = Arc::clone(&quit_requested);
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

        // The rest of this closure is the expensive part: reading every
        // control, deciding the menu bar's face, repainting it. Worth doing
        // at full speed while a slider might be moving or the blink that
        // acknowledges a command is still showing; otherwise it is work
        // nobody can see, so it only runs often enough that a menu toggle
        // still repaints promptly.
        let throttle_tick = throttle_ticks.get().wrapping_add(1);
        throttle_ticks.set(throttle_tick);
        let watched = panel_for_timer.is_visible() || blink_until.get().is_some();
        if !watched && !on_schedule(throttle_tick, UI_THROTTLE_TICKS) {
            return;
        }

        if open_for_timer.swap(false, Ordering::Relaxed) {
            panel_for_timer.show();
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
        if panel_for_timer.take_training_request() {
            if let Ok(mut session) = training_for_timer.lock() {
                *session = Some(enroll::Session::starting(training_model_path.clone()));
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
                        actions::show_message(&format!(
                            "Añadidos {n}. Reinicia Minion desde el menú para que \
                             se apliquen."
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => actions::show_message(&e),
                }
            }
        }
        // Controls report by being read: see preferences.rs for why.
        if panel_for_timer.poll() {
            sounds_for_timer.store(panel_for_timer.sounds_on(), Ordering::Relaxed);
            voices_for_timer.store(panel_for_timer.log_voices_on(), Ordering::Relaxed);
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
            }
        }

        let listening = active_for_timer.load(Ordering::Relaxed);
        // Downloading is not listening, whatever the switch says.
        let awake = listening && !downloading_for_timer.load(Ordering::Relaxed);
        let wanted = match (awake, dictating_for_timer.load(Ordering::Relaxed)) {
            (false, _) => Face::Asleep,
            (true, true) => Face::Dictating,
            (true, false) => Face::Awake,
        };
        if wanted == shown_face.get() {
            return;
        }
        let was_awake = shown_face.get() != Face::Asleep;
        shown_face.set(wanted);
        // Pausing and resuming happen from three places — the menu, the
        // shortcut and a spoken order — and this is the one that sees all
        // three, because they all end up flipping the same flag.
        if !downloading_for_timer.load(Ordering::Relaxed) && was_awake != awake {
            if let Ok(mut text) = status_for_timer.lock() {
                *text = if awake { TOOLTIP_LISTENING } else { TOOLTIP_PAUSED }.to_string();
            }
        }
        let face = match wanted {
            Face::Awake | Face::Acting => icon::awake(),
            Face::Asleep => icon::asleep(),
            Face::Dictating => icon::dictating(),
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
    let learn_from_menu = Arc::clone(&learn_requested);
    let restart_from_menu = Arc::clone(&restart_requested);
    let quit_from_menu = Arc::clone(&quit_requested);
    let catalogue_from_menu = Arc::clone(&catalogue_requested);
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
            } else if event.id == preferences_id {
                // Windows belong to the main thread; the timer opens it.
                open_from_menu.store(true, Ordering::Relaxed);
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
            } else if event.id == quit_id {
                note!("quit from the menu");
                quit_from_menu.store(true, Ordering::Relaxed);
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
        if matches!(argument, "--help" | "-h" | "help") {
            // Spanish: everything the person running this reads is in
            // Spanish, and this is read by nobody else. Without it,
            // `minion --help` went looking for a model called «--help».
            println!(
                "Minion — control por voz en español.\n\n\
                 Uso:\n  \
                 minion                      escucha y obedece (el uso normal)\n  \
                 minion <ruta-al-modelo>     igual, con el modelo de esa carpeta\n  \
                 minion learn [--apply]      convierte en alias lo que no entendió\n  \
                 minion enroll               aprende tu voz desde la terminal\n  \
                 minion export-icon <dir>    guarda el icono como .iconset\n  \
                 minion run \"orden\"          la ejecuta, como si la hubieras dicho\n  \
                 minion say \"texto\"          lo dice en voz alta\n  \
                 minion status               si Minion está escuchando o en pausa\n  \
                 minion --help               esto\n\n\
                 «run» y «say» dejan un aviso para la copia que ya está en\n\
                 marcha y no hacen nada si no hay ninguna — útil para atajos\n\
                 de teclado (Atajos.app, Raycast): un atajo con\n\
                 «minion run \"cierra la pestaña\"» hace lo mismo que decirlo.\n\n\
                 Variables de entorno:\n  \
                 MINION_MODEL                carpeta del modelo de reconocimiento\n\n\
                 Registro: ~/Library/Logs/minion.log\n\
                 Ajustes:  ~/Library/Application Support/Minion/config.toml"
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
            return enroll::run(&model_path);
        }
        if argument == "learn" {
            let apply = std::env::args().any(|a| a == "--apply");
            let config = config::load();
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

    let voice = match speaker::load_profile_for(&model_path) {
        Some(profile) => match speaker::Speaker::load(&model_path) {
            Ok(model) => {
                note!("Voice profile loaded — only your voice will be obeyed.");
                Some(Voice { model, profile, threshold: config.voice_threshold() })
            }
            Err(e) => {
                note!("Voice profile found but the speaker model would not load: {e:#}");
                None
            }
        },
        None => None,
    };

    note!("Minion starting — loading model…");
    if let Some(log) = journal::path() {
        println!("Log: {}", log.display());
    }
    report_permissions();
    let active = Arc::new(AtomicBool::new(true));
    // Raised by the listening loop when a command runs, lowered by the run
    // loop once the icon has blinked.
    let acted = Arc::new(AtomicBool::new(false));
    // Set by the listening loop while dictating, read by the run loop for
    // the face in the menu bar.
    let dictating = Arc::new(AtomicBool::new(false));
    let worker_dictating = Arc::clone(&dictating);

    let worker_active = Arc::clone(&active);
    let worker_log_ignored = Arc::clone(&log_ignored);
    let worker_sounds = Arc::clone(&play_sounds);
    let worker_acted = Arc::clone(&acted);
    let microphone = config.microphone();
    let save_recordings = config.save_recordings;
    let catalogue_asked = Arc::new(AtomicBool::new(false));
    let worker_catalogue = Arc::clone(&catalogue_asked);
    let voice_reply = config.speak.then(|| VoiceReply {
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
            voice_reply,
            microphone,
            show_catalogue: worker_catalogue,
            save_recordings,
            status: Arc::clone(&worker_status),
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
        if hotkey::watch(&shortcut, Arc::clone(&active)) {
            note!("Shortcut {shortcut} pauses and resumes.");
        } else {
            note!("Cannot read the shortcut «{shortcut}»; ignoring it.");
        }
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

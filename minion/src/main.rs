//! Minion — control your Mac by speaking Spanish.
//!
//! Listens on the microphone, transcribes with Parakeet, and carries out
//! sentences that open with the wake word. Lives in the menu bar.
//!
//! Thread layout matters here: AppKit insists the menu bar is created and
//! serviced on the main thread, so recognition — the expensive part — runs
//! on its own.

mod actions;
mod audio;
mod commands;
mod config;
mod enroll;
mod fbank;
mod hotkey;
mod icon;
mod journal;
mod learn;
mod preferences;
mod spanish;
mod startup;
mod speaker;
mod text;

use std::cell::Cell;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
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

/// System sounds used as feedback. A command that runs produces no visible
/// output, so without these you cannot tell whether you were heard.
mod sounds {
    pub const DONE: &str = "/System/Library/Sounds/Pop.aiff";
    pub const UNSURE: &str = "/System/Library/Sounds/Tink.aiff";
}

/// The toggle's two faces. It names the action, not the state: a menu item
/// is something you do, so while listening it offers to pause.
const MENU_PAUSE: &str = "Pausar";
const MENU_LISTEN: &str = "Escuchar";

/// How often the run loop checks for changes.
///
/// Fast enough that a slider's readout keeps up with the thumb, which is
/// what makes the preferences window feel like a window rather than a form.
const UI_REFRESH_SECONDS: f64 = 0.05;

/// How often the recognition loop wakes up to see whether it has gone idle.
const IDLE_CHECK: Duration = Duration::from_secs(20);

/// Locates the speech model.
///
/// Order: explicit argument, `OYENTE_MODEL`, the app bundle's Resources,
/// then the working directory. The bundle case is what makes double-click
/// launching work, since a bundled app starts with `/` as its directory.
fn locate_model(argument: Option<String>) -> Result<String> {
    if let Some(path) = argument {
        return Ok(path);
    }
    if let Ok(path) = std::env::var("OYENTE_MODEL") {
        return Ok(path);
    }

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();

    // Inside the app bundle: Contents/MacOS/minion -> Contents/Resources/model
    if let Ok(executable) = std::env::current_exe() {
        if let Some(macos_dir) = executable.parent() {
            candidates.push(macos_dir.join("../Resources/model"));
            candidates.push(macos_dir.join("model"));
        }
    }
    candidates.push(Path::new("model").to_path_buf());
    candidates.push(Path::new("../model").to_path_buf());

    // Installed alongside user data.
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(Path::new(&home).join("Library/Application Support/Minion/model"));
    }

    for candidate in candidates {
        if candidate.join("vocab.txt").exists() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(anyhow!(
        "speech model not found. Run ./download-model.sh, or pass its path \
         as an argument."
    ))
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

fn listen_and_obey(
    model_path: String,
    settings: audio::Settings,
    log_ignored_speech: Arc<AtomicBool>,
    play_sounds: Arc<AtomicBool>,
    idle_unload: Option<Duration>,
    mut voice: Option<Voice>,
    active: Arc<AtomicBool>,
) -> Result<()> {
    // Held in an Option so it can be dropped while idle. It is loaded now
    // rather than on first use, so the first thing said after starting is
    // as quick as the rest.
    let mut model = Some(load_model(&model_path)?);
    let mut last_used = Instant::now();
    // What "otra vez" refers to.
    let mut last_command: Option<Decision> = None;
    note!("Model loaded. {}", resident_memory());

    let listener =
        audio::start(settings, Arc::clone(&active)).context("opening the microphone")?;
    note!(
        "Microphone: {} Hz, {} channel(s). {} phrases understood.",
        listener.source_hz,
        listener.channels,
        commands::phrase_count()
    );
    note!("Listening. Say: «minion, abre Chrome»");

    loop {
        // A bounded wait, so idleness can be noticed while nothing is being
        // said. A plain recv() would block until the next utterance, and
        // the model would stay loaded through an empty afternoon.
        let utterance = match listener.utterances.recv_timeout(IDLE_CHECK) {
            Ok(utterance) => utterance,
            Err(mpsc::RecvTimeoutError::Timeout) => {
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
        let seconds = utterance.len() as f32 / audio::TARGET_HZ as f32;
        let started = Instant::now();

        // Whose voice this is, decided before transcribing: someone else's
        // speech should not reach the recogniser at all, let alone the log.
        if let Some(voice) = voice.as_mut() {
            if let Some(heard) = voice.model.embed(&utterance) {
                let likeness = speaker::similarity(&heard, &voice.profile);
                if likeness < voice.threshold {
                    note!("heard    {seconds:.1}s in another voice ({likeness:.2})");
                    continue;
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
                    eprintln!("could not reload the model: {e:#}");
                    continue;
                }
            }
        }
        last_used = Instant::now();

        let Some(loaded) = model.as_mut() else {
            continue;
        };
        let transcript = match loaded.transcribe_samples(utterance, audio::TARGET_HZ, 1, None) {
            Ok(result) => result.text.trim().to_string(),
            Err(e) => {
                eprintln!("  transcription failed: {e}");
                continue;
            }
        };
        if transcript.is_empty() {
            continue;
        }
        let elapsed_ms = started.elapsed().as_millis();

        // One sentence can hold several instructions joined by "y luego".
        for part in commands::split_chain(&transcript) {
            // Which application is in front decides what some phrases mean,
            // and it is read per instruction: the first of a chain may well
            // have changed which application that is.
            let context = actions::frontmost_app();
            let (mut decision, confidence) = commands::decide_in(&part, context.as_deref());

            // "otra vez" means whatever was said before it.
            let mut repeats = 1;
            if let commands::Decision::Again(times) = decision {
                match &last_command {
                    Some(previous) => {
                        repeats = times;
                        decision = previous.clone();
                    }
                    None => {
                        note!("unknown  «{part}»  ->  nothing to repeat yet");
                        continue;
                    }
                }
            }

            report(
                &part,
                &decision,
                confidence,
                repeats,
                seconds,
                elapsed_ms,
                log_ignored_speech.load(Ordering::Relaxed),
                play_sounds.load(Ordering::Relaxed),
            );

            if commands::is_sleep(&decision) {
                active.store(false, Ordering::Relaxed);
                note!("paused by voice — resume from the menu bar");
            }
            // Only real actions are worth repeating later.
            if !matches!(
                decision,
                commands::Decision::Ignored | commands::Decision::Unrecognised
            ) {
                last_command = Some(decision);
            }
        }
    }
    Ok(())
}

/// Carries out a decision and writes down what happened.
#[allow(clippy::too_many_arguments)]
fn report(
    transcript: &str,
    decision: &Decision,
    confidence: f32,
    repeats: usize,
    seconds: f32,
    elapsed_ms: u128,
    log_ignored_speech: bool,
    play_sounds: bool,
) {
        match decision {
            Decision::Ignored => {
                // Speech that was not for us. The wording is only written
                // down when explicitly asked for: see log_ignored_speech.
                if log_ignored_speech {
                    note!("heard    «{transcript}»  (not addressed to me)");
                } else {
                    note!("heard    {seconds:.1}s of speech, not addressed to me");
                }
            }
            Decision::Unrecognised => {
                note!("unknown  «{transcript}»  ->  not understood");
                if play_sounds {
                    actions::play_sound(sounds::UNSURE);
                }
            }
            _ => {
                let mut outcome = None;
                for _ in 0..repeats.max(1) {
                    outcome = commands::perform(decision);
                }
                if let Some(done) = outcome {
                    if done.succeeded {
                        let again = if repeats > 1 {
                            format!(" ×{repeats}")
                        } else {
                            String::new()
                        };
                        note!(
                            "ran      «{transcript}»  ->  {}{again}  \
                             [{:.0}% · {seconds:.1}s audio · {elapsed_ms} ms]",
                            done.description,
                            confidence * 100.0
                        );
                        if play_sounds {
                            actions::play_sound(sounds::DONE);
                        }
                    } else {
                        // Understood perfectly and refused by the system.
                        // Almost always the Accessibility permission.
                        note!(
                            "BLOCKED  «{transcript}»  ->  {}  — macOS refused it. \
                             Grant Accessibility in System Settings.",
                            done.description
                        );
                        if play_sounds {
                            actions::play_sound(sounds::UNSURE);
                        }
                    }
                }
            }
        }
}

/// Builds the menu bar item and hands control to AppKit. Never returns.
fn run_menu_bar(
    active: Arc<AtomicBool>,
    sounds_on: Arc<AtomicBool>,
    log_voices_on: Arc<AtomicBool>,
) -> Result<()> {
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

    let preferences = MenuItem::new("Preferencias…", true, None);

    // The two things you do with the log, together. Kept in scope for the
    // life of the menu, like every other item.
    let log_menu = Submenu::new("Registro", true);
    log_menu.append(&learn)?;
    log_menu.append(&show_log)?;

    let quit = MenuItem::new("Salir de Minion", true, None);
    menu.append(&toggle)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&log_menu)?;
    menu.append(&preferences)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let toggle_id = toggle.id().clone();
    let learn_id = learn.id().clone();
    let preferences_id = preferences.id().clone();
    let show_log_id = show_log.id().clone();
    let quit_id = quit.id().clone();

    // Built once and reused: reopening should bring back the same window,
    // not stack another one behind it.
    let panel = Rc::new(preferences::Preferences::new(mtm));
    let report = Rc::new(preferences::Report::new(mtm));

    // Watches this application's keys, so the shortcut button can be set by
    // pressing a combination rather than typing its name.
    let panel_for_capture = Rc::clone(&panel);
    let _capture = preferences::capture_keys(move |code, mods| {
        panel_for_capture.is_capturing() && panel_for_capture.capture(code, mods)
    });
    // Requests from the menu thread, which must not touch AppKit itself.
    let open_requested = Arc::new(AtomicBool::new(false));
    let learn_requested = Arc::new(AtomicBool::new(false));

    // Held for the lifetime of the process: dropping it removes the icon.
    let tray = Rc::new(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon::awake()?)
            // A template image: macOS tints it to match the menu bar, so it
            // is black on a light one and white on a dark one.
            .with_icon_as_template(true)
            .with_tooltip("Minion — control por voz")
            .build()?,
    );

    // The state can change from the menu or from a spoken "deja de
    // escuchar", and the second happens on the recognition thread, which
    // must not touch AppKit. A timer on the main run loop is the bridge: it
    // polls the flag and repaints the icon and the menu item together.
    let tray_for_timer = Rc::clone(&tray);
    let toggle_for_timer = toggle.clone();
    let active_for_timer = Arc::clone(&active);
    let shown_as_listening = Cell::new(true);
    let panel_for_timer = Rc::clone(&panel);
    let open_for_timer = Arc::clone(&open_requested);
    let learn_for_timer = Arc::clone(&learn_requested);
    let report_for_timer = Rc::clone(&report);
    let sounds_for_timer = Arc::clone(&sounds_on);
    let voices_for_timer = Arc::clone(&log_voices_on);
    let repaint = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        if open_for_timer.swap(false, Ordering::Relaxed) {
            panel_for_timer.show();
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
                            "Añadidos {n}. Reinicia Minion para que se apliquen."
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

        let listening = active_for_timer.load(Ordering::Relaxed);
        if listening == shown_as_listening.get() {
            return;
        }
        shown_as_listening.set(listening);
        let face = if listening { icon::awake() } else { icon::asleep() };
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
    unsafe {
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    let _timer = timer;

    // Menu events arrive on a global channel, which is Send, so they can be
    // serviced from another thread while AppKit owns the main one. The icon
    // and the item's text are repainted by the timer above, not from here.
    let open_from_menu = Arc::clone(&open_requested);
    let learn_from_menu = Arc::clone(&learn_requested);
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
            } else if event.id == preferences_id {
                // Windows belong to the main thread; the timer opens it.
                open_from_menu.store(true, Ordering::Relaxed);
            } else if event.id == show_log_id {
                if let Some(path) = journal::path() {
                    actions::reveal(&path.to_string_lossy());
                }
            } else if event.id == quit_id {
                note!("quit from the menu");
                std::process::exit(0);
            }
        }
    });

    app.run();
    Ok(())
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
    actions::open_accessibility_settings();
}

fn main() -> Result<()> {
    // `minion learn` reads the log and turns its failures into vocabulary.
    // It touches neither the microphone nor the model, so it is handled
    // before any of that is set up.
    let first_argument = std::env::args().nth(1);
    if let Some(argument) = first_argument.as_deref() {
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
    }

    if !claim_sole_instance() {
        eprintln!("Minion ya se está ejecutando.");
        return Ok(());
    }

    let model_path = locate_model(first_argument)?;

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
    let voice = match speaker::load_profile() {
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

    let worker_active = Arc::clone(&active);
    let worker_log_ignored = Arc::clone(&log_ignored);
    let worker_sounds = Arc::clone(&play_sounds);
    std::thread::spawn(move || {
        if let Err(e) = listen_and_obey(
            model_path,
            audio_settings,
            worker_log_ignored,
            worker_sounds,
            idle_unload,
            voice,
            worker_active,
        ) {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
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

    run_menu_bar(active, play_sounds, log_ignored)
}

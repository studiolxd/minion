//! Oyente — control your Mac by speaking Spanish.
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
mod journal;
mod learn;
mod spanish;
mod text;

use std::cell::Cell;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::NSTimer;
use ort::session::builder::SessionBuilder;
use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

use commands::Decision;

/// System sounds used as feedback. A command that runs produces no visible
/// output, so without these you cannot tell whether you were heard.
mod sounds {
    pub const DONE: &str = "/System/Library/Sounds/Pop.aiff";
    pub const UNSURE: &str = "/System/Library/Sounds/Tink.aiff";
}

/// What the menu bar shows in each state.
mod icon {
    pub const LISTENING: &str = "🎙";
    pub const PAUSED: &str = "😴";
}

/// How often the menu bar checks whether the state changed.
const ICON_REFRESH_SECONDS: f64 = 0.4;

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

    // Inside the app bundle: Contents/MacOS/oyente -> Contents/Resources/model
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
        candidates.push(Path::new(&home).join("Library/Application Support/Oyente/model"));
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
fn listen_and_obey(
    model_path: String,
    settings: audio::Settings,
    log_ignored_speech: bool,
    play_sounds: bool,
    active: Arc<AtomicBool>,
) -> Result<()> {
    let mut model = ParakeetTDT::from_pretrained(&model_path, Some(inference_config()))
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading the model from '{model_path}'"))?;
    note!("Model loaded. {}", resident_memory());

    let listener =
        audio::start(settings, Arc::clone(&active)).context("opening the microphone")?;
    note!(
        "Microphone: {} Hz, {} channel(s). {} phrases understood.",
        listener.source_hz,
        listener.channels,
        commands::phrase_count()
    );
    note!("Listening. Say: «ordenador, abre Chrome»");

    for utterance in listener.utterances {
        if !active.load(Ordering::Relaxed) {
            continue;
        }
        let seconds = utterance.len() as f32 / audio::TARGET_HZ as f32;
        let started = std::time::Instant::now();

        let transcript = match model.transcribe_samples(utterance, audio::TARGET_HZ, 1, None) {
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

        // Which application is in front decides what some phrases mean, so
        // it is read now rather than when the command runs: by then the
        // command itself may have changed it.
        let context = actions::frontmost_app();
        let (decision, confidence) = commands::decide_in(&transcript, context.as_deref());
        match &decision {
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
                if let Some(done) = commands::perform(&decision) {
                    if done.succeeded {
                        note!(
                            "ran      «{transcript}»  ->  {}  \
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
                if commands::is_sleep(&decision) {
                    active.store(false, Ordering::Relaxed);
                    note!("paused by voice — resume from the menu bar");
                }
            }
        }
    }
    Ok(())
}

/// Builds the menu bar item and hands control to AppKit. Never returns.
fn run_menu_bar(active: Arc<AtomicBool>) -> Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow!("the menu bar must be built on the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    // Accessory: menu bar only, no Dock icon and no window.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let menu = Menu::new();
    let listen = MenuItem::new("Escuchar", true, None);
    let pause = MenuItem::new("Pausar", true, None);
    let quit = MenuItem::new("Salir de Oyente", true, None);
    menu.append(&listen)?;
    menu.append(&pause)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let listen_id = listen.id().clone();
    let pause_id = pause.id().clone();
    let quit_id = quit.id().clone();

    // Held for the lifetime of the process: dropping it removes the icon.
    let tray = Rc::new(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_title(icon::LISTENING)
            .with_tooltip("Oyente — control por voz")
            .build()?,
    );

    // The state can change from the menu or from a spoken "deja de
    // escuchar", and the second happens on the recognition thread, which
    // must not touch AppKit. A timer on the main run loop is the bridge:
    // it polls the flag and repaints the icon when it differs.
    let tray_for_timer = Rc::clone(&tray);
    let active_for_timer = Arc::clone(&active);
    let shown_as_listening = Cell::new(true);
    let repaint = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        let listening = active_for_timer.load(Ordering::Relaxed);
        if listening == shown_as_listening.get() {
            return;
        }
        shown_as_listening.set(listening);
        let title = if listening { icon::LISTENING } else { icon::PAUSED };
        tray_for_timer.set_title(Some(title));
    });
    // Safety: the block only touches the tray icon and an atomic flag, and
    // the timer fires on the main thread, which is where the tray lives.
    let _timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(
            ICON_REFRESH_SECONDS,
            true,
            &repaint,
        )
    };

    // Menu events arrive on a global channel, which is Send, so they can be
    // serviced from another thread while AppKit owns the main one. The icon
    // itself is repainted by the timer above, not from here.
    std::thread::spawn(move || {
        let events = MenuEvent::receiver();
        while let Ok(event) = events.recv() {
            if event.id == listen_id {
                active.store(true, Ordering::Relaxed);
                note!("resumed from the menu");
            } else if event.id == pause_id {
                active.store(false, Ordering::Relaxed);
                note!("paused from the menu");
            } else if event.id == quit_id {
                note!("quit from the menu");
                std::process::exit(0);
            }
        }
    });

    app.run();
    Ok(())
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
    // Ask macOS to prompt. This is the call that registers Oyente with the
    // system, so that there is a switch to turn on when the pane opens —
    // an app that never asked simply is not in the list.
    actions::request_accessibility_permission();
    println!(
        "\n  Accept the dialog, or switch Oyente on in System Settings →\n  \
         Privacy & Security → Accessibility. Then restart Oyente from the\n  \
         menu bar: the permission is only read at startup.\n"
    );
    actions::open_accessibility_settings();
}

fn main() -> Result<()> {
    // `oyente learn` reads the log and turns its failures into vocabulary.
    // It touches neither the microphone nor the model, so it is handled
    // before any of that is set up.
    let first_argument = std::env::args().nth(1);
    if let Some(argument) = first_argument.as_deref() {
        if argument == "learn" {
            let apply = std::env::args().any(|a| a == "--apply");
            let config = config::load();
            commands::configure(&config);
            learn::run(&config, apply);
            return Ok(());
        }
    }

    let model_path = locate_model(first_argument)?;

    let config = config::load();
    commands::configure(&config);
    let audio_settings = config.audio_settings();
    let log_ignored = config.log_ignored_speech;
    let play_sounds = config.sounds;

    note!("Oyente starting — loading model…");
    if let Some(log) = journal::path() {
        println!("Log: {}", log.display());
    }
    report_permissions();
    let active = Arc::new(AtomicBool::new(true));

    let worker_active = Arc::clone(&active);
    std::thread::spawn(move || {
        if let Err(e) = listen_and_obey(
            model_path,
            audio_settings,
            log_ignored,
            play_sounds,
            worker_active,
        ) {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    });

    run_menu_bar(active)
}

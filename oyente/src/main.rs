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
mod spanish;
mod text;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use parakeet_rs::{ParakeetTDT, Transcriber};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

use commands::Decision;

/// System sounds used as feedback. A command that runs produces no visible
/// output, so without these you cannot tell whether you were heard.
mod sounds {
    pub const DONE: &str = "/System/Library/Sounds/Pop.aiff";
    pub const UNSURE: &str = "/System/Library/Sounds/Tink.aiff";
}

/// Locates the speech model.
///
/// Order: explicit argument, `OYENTE_MODEL` in the environment, then the
/// usual spots relative to the working directory.
fn locate_model(argument: Option<String>) -> Result<String> {
    if let Some(path) = argument {
        return Ok(path);
    }
    if let Ok(path) = std::env::var("OYENTE_MODEL") {
        return Ok(path);
    }
    for candidate in ["model", "modelo", "../model", "../../model"] {
        if Path::new(candidate).join("vocab.txt").exists() {
            return Ok(candidate.to_string());
        }
    }
    Err(anyhow!(
        "speech model not found. Run ./download-model.sh, or pass the path \
         as an argument."
    ))
}

/// The recognition loop. Owns the model and runs on its own thread.
fn listen_and_obey(model_path: String, active: Arc<AtomicBool>) -> Result<()> {
    let mut model = ParakeetTDT::from_pretrained(&model_path, None)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading the model from '{model_path}'"))?;
    println!("Model loaded.");

    let listener = audio::start(audio::Settings::default(), Arc::clone(&active))
        .context("opening the microphone")?;
    println!(
        "Microphone: {} Hz, {} channel(s)",
        listener.source_hz, listener.channels
    );
    println!("{} phrases understood.\n", commands::phrase_count());
    println!("Say: «ordenador, abre Chrome»\n");

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

        let (decision, confidence) = commands::decide(&transcript);
        match &decision {
            Decision::Ignored => println!("  «{transcript}»  (not addressed to me)"),
            Decision::Unrecognised => {
                println!("  «{transcript}»  ->  not understood");
                actions::play_sound(sounds::UNSURE);
            }
            _ => {
                if let Some(description) = commands::perform(&decision) {
                    println!(
                        "  «{transcript}»  ->  {description}  \
                         [{:.0}% · {seconds:.1}s audio · {elapsed_ms} ms]",
                        confidence * 100.0
                    );
                    actions::play_sound(sounds::DONE);
                }
                if commands::is_sleep(&decision) {
                    active.store(false, Ordering::Relaxed);
                    println!("  (paused — resume from the menu bar)");
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
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_title("🎙")
        .with_tooltip("Oyente — control por voz")
        .build()?;

    // Menu events arrive on a global channel, which is Send, so they can be
    // serviced from another thread while AppKit owns the main one.
    std::thread::spawn(move || {
        let events = MenuEvent::receiver();
        while let Ok(event) = events.recv() {
            if event.id == listen_id {
                active.store(true, Ordering::Relaxed);
                println!("(listening)");
            } else if event.id == pause_id {
                active.store(false, Ordering::Relaxed);
                println!("(paused)");
            } else if event.id == quit_id {
                println!("Goodbye.");
                std::process::exit(0);
            }
        }
    });

    app.run();
    Ok(())
}

fn main() -> Result<()> {
    let model_path = locate_model(std::env::args().nth(1))?;

    println!("Oyente — loading model…");
    if !actions::has_accessibility_permission() {
        eprintln!(
            "Warning: no Accessibility permission. Commands that open apps\n\
             will work, but those that press keys (copy, save, close tab)\n\
             will do nothing and report no error.\n\
             Grant it under System Settings → Privacy & Security →\n\
             Accessibility.\n"
        );
    }

    let active = Arc::new(AtomicBool::new(true));

    let worker_active = Arc::clone(&active);
    std::thread::spawn(move || {
        if let Err(e) = listen_and_obey(model_path, worker_active) {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    });

    run_menu_bar(active)
}

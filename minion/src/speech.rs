//! Speaking back.
//!
//! Minion stays quiet almost always: opening Chrome is something you can
//! see, and announcing it would be noise arriving after the fact. It speaks
//! when the voice is the only output there is — answering a question.
//!
//! Uses the system's own synthesiser. It is already installed, has Spanish
//! voices, costs nothing to ship, and is good enough to settle the harder
//! question of *when* to speak. A neural voice sounds better and can come
//! later; no amount of audio quality rescues a program that talks too much.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// Voices to prefer, best first, among those macOS ships in Spanish.
const PREFERRED: &[&str] = &["Mónica", "Monica", "Paulina", "Sandy", "Shelley"];

/// Words per minute. The default is slower than anyone wants for a clock.
pub const DEFAULT_RATE: u32 = 190;

/// The voice to use when none is configured.
///
/// Asks the system what it has rather than assuming: voices come and go
/// with macOS versions, and a missing one makes `say` fall back to English.
pub fn default_voice() -> Option<String> {
    let listing = Command::new("/usr/bin/say").arg("-v").arg("?").output().ok()?;
    let listing = String::from_utf8_lossy(&listing.stdout);

    let spanish: Vec<&str> = listing
        .lines()
        .filter(|line| line.contains("es_ES"))
        .filter_map(|line| line.split_whitespace().next())
        .collect();

    PREFERRED
        .iter()
        .find(|wanted| spanish.contains(wanted))
        .map(|found| (*found).to_string())
        .or_else(|| spanish.first().map(|first| (*first).to_string()))
}

/// Says something, and waits until it is finished.
///
/// `deaf` is raised for the duration: Minion listens continuously, so
/// without this it would hear itself, transcribe what it said and possibly
/// act on it. Waiting rather than speaking in the background is what makes
/// the flag reliable — it comes down exactly when the sound stops.
/// The output devices available, by name.
pub fn output_names() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let Ok(devices) = cpal::default_host().output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|device| device.description().ok())
        .map(|description| description.name().to_string())
        .collect()
}

pub fn say(
    text: &str,
    voice: Option<&str>,
    rate: u32,
    device: Option<&str>,
    deaf: &AtomicBool,
    tail: std::time::Duration,
) {
    if text.is_empty() {
        return;
    }
    deaf.store(true, Ordering::Relaxed);

    let mut command = Command::new("/usr/bin/say");
    if let Some(voice) = voice {
        command.arg("-v").arg(voice);
    }
    if let Some(device) = device.filter(|name| !name.trim().is_empty()) {
        command.arg("-a").arg(device);
    }
    command.arg("-r").arg(rate.to_string()).arg(text);
    let _ = command.status();

    // A moment more: the microphone hears the tail of the room, not just
    // the file. Long enough for the segmenter to have closed anything the
    // reply opened — it needs `silence_end_ms` of quiet to do that — or
    // the sentence carrying Minion's own answer arrives just after the
    // flag comes down.
    std::thread::sleep(tail);
    deaf.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_a_spanish_voice_that_exists() {
        // Whatever it picks must be a voice the system actually has, or
        // `say` silently falls back to English.
        if let Some(voice) = default_voice() {
            let listing = Command::new("/usr/bin/say")
                .arg("-v")
                .arg("?")
                .output()
                .expect("say should list its voices");
            let listing = String::from_utf8_lossy(&listing.stdout);
            assert!(
                listing.lines().any(|line| line.starts_with(&voice)),
                "«{voice}» is not installed"
            );
        }
    }

    #[test]
    fn saying_nothing_does_nothing() {
        let deaf = AtomicBool::new(false);
        say("", None, DEFAULT_RATE, None, &deaf, std::time::Duration::ZERO);
        assert!(!deaf.load(Ordering::Relaxed), "should not go deaf for silence");
    }
}

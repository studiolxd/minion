//! Speaking back.
//!
//! Minion stays quiet almost always: opening Chrome is something you can
//! see, and announcing it would be noise arriving after the fact. It speaks
//! when the voice is the only output there is — answering a question, or
//! reading something aloud on request.
//!
//! Uses the system's own synthesiser. It is already installed, has Spanish
//! voices, costs nothing to ship, and is good enough to settle the harder
//! question of *when* to speak. A neural voice sounds better and can come
//! later; no amount of audio quality rescues a program that talks too much.
//!
//! Long text is spoken one sentence at a time, each as its own `say` child
//! process, so "para de leer" can [`stop`] it between sentences rather than
//! waiting for the whole thing to finish.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Voices to prefer, best first, among those macOS ships in Spanish.
const PREFERRED: &[&str] = &["Mónica", "Monica", "Paulina", "Sandy", "Shelley"];

/// Words per minute. The default is slower than anyone wants for a clock.
pub const DEFAULT_RATE: u32 = 190;

/// The `say` process currently reading a sentence, if any — behind a mutex
/// so [`stop`] can reach it from a different thread than the one speaking.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// Raised while [`stop`] wants the current read abandoned. Checked between
/// sentences, and cleared at the start of the next [`say`].
static STOPPED: AtomicBool = AtomicBool::new(false);

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

/// Splits text into sentences, keeping the closing punctuation — `say`
/// reads it fine, and losing it flattens the intonation. This is what makes
/// a long read interruptible: [`say`] checks [`stop`] between sentences
/// rather than only after the whole text has been read.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, byte) in text.bytes().enumerate() {
        if matches!(byte, b'.' | b'!' | b'?') {
            let piece = text[start..=i].trim();
            if !piece.is_empty() {
                out.push(piece);
            }
            start = i + 1;
        }
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// Stops whatever is being read, killing the `say` process mid-sentence.
/// Safe to call when nothing is speaking.
pub fn stop() {
    STOPPED.store(true, Ordering::Relaxed);
    if let Ok(mut slot) = CHILD.lock() {
        if let Some(child) = slot.as_mut() {
            let _ = child.kill();
        }
    }
}

/// Waits for the sentence in [`CHILD`] to finish, or for [`stop`] to kill
/// it. Polls rather than blocking on `wait()` so the mutex is only held for
/// an instant at a time — held across the whole wait, `stop()` on another
/// thread would never get in to call `kill()`.
fn wait_for_child() {
    loop {
        if STOPPED.load(Ordering::Relaxed) {
            if let Ok(mut slot) = CHILD.lock() {
                if let Some(child) = slot.as_mut() {
                    let _ = child.kill();
                }
            }
        }
        let finished = CHILD.lock().ok().is_none_or(|mut slot| match slot.as_mut() {
            Some(child) => child.try_wait().ok().flatten().is_some(),
            None => true,
        });
        if finished {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Says something, and waits until it is finished — or until [`stop`] cuts
/// it short.
///
/// `deaf` is raised for the duration: Minion listens continuously, so
/// without this it would hear itself, transcribe what it said and possibly
/// act on it. Waiting rather than speaking in the background is what makes
/// the flag reliable — it comes down exactly when the sound stops.
pub fn say(
    text: &str,
    voice: Option<&str>,
    rate: u32,
    device: Option<&str>,
    deaf: &AtomicBool,
    tail: Duration,
) {
    if text.is_empty() {
        return;
    }
    deaf.store(true, Ordering::Relaxed);
    STOPPED.store(false, Ordering::Relaxed);

    for sentence in sentences(text) {
        if STOPPED.load(Ordering::Relaxed) {
            break;
        }
        let mut command = Command::new("/usr/bin/say");
        if let Some(voice) = voice {
            command.arg("-v").arg(voice);
        }
        if let Some(device) = device.filter(|name| !name.trim().is_empty()) {
            command.arg("-a").arg(device);
        }
        command.arg("-r").arg(rate.to_string()).arg(sentence);
        let Ok(child) = command.spawn() else { break };
        if let Ok(mut slot) = CHILD.lock() {
            *slot = Some(child);
        }
        wait_for_child();
    }
    if let Ok(mut slot) = CHILD.lock() {
        *slot = None;
    }

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
        say("", None, DEFAULT_RATE, None, &deaf, Duration::ZERO);
        assert!(!deaf.load(Ordering::Relaxed), "should not go deaf for silence");
    }

    #[test]
    fn stopping_with_nothing_speaking_does_not_panic() {
        stop();
        STOPPED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn splits_on_sentence_endings_and_keeps_the_punctuation() {
        assert_eq!(
            sentences("Han pasado cinco minutos. ¿Cancelo el resto?"),
            vec!["Han pasado cinco minutos.", "¿Cancelo el resto?"]
        );
        assert_eq!(sentences("Sin punto final"), vec!["Sin punto final"]);
        assert_eq!(sentences("   "), Vec::<&str>::new());
        assert_eq!(sentences("Una. Dos. Tres."), vec!["Una.", "Dos.", "Tres."]);
    }
}

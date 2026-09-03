//! Microphone capture, chopped into utterances.
//!
//! The microphone delivers blocks at 48 kHz; the model wants 16 kHz mono.
//! And since the recogniser works on whole utterances, something has to
//! decide where one starts and ends — for now, signal energy does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Sample rate the model expects.
pub const TARGET_HZ: u32 = 16_000;

/// Analysis block: 20 ms.
const BLOCK_SAMPLES: usize = TARGET_HZ as usize / 50;
const BLOCK_MS: usize = 20;

/// How much audio may pile up before old samples are dropped. Guards
/// against the queue growing without bound if the model stalls.
const QUEUE_LIMIT_SECONDS: usize = 30;

/// Voice detection tuning.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// RMS energy above which someone is considered to be speaking.
    ///
    /// Used as a floor. The working threshold rises above it when the room
    /// is noisy — see [`NoiseFloor`].
    pub speech_threshold: f32,
    /// Silence that ends an utterance.
    pub silence_end_ms: usize,
    /// Below this it is a noise, not an utterance.
    pub min_speech_ms: usize,
    /// Safety cut so a continuous noise cannot accumulate forever.
    pub max_utterance_ms: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            speech_threshold: 0.015,
            silence_end_ms: 700,
            min_speech_ms: 300,
            max_utterance_ms: 12_000,
        }
    }
}

/// A running microphone. Dropping it stops capture.
pub struct Listener {
    /// Replaced when the system's default input changes, so the field is
    /// held rather than ignored.
    _stream: Arc<Mutex<Option<cpal::platform::Stream>>>,
    /// Completed utterances, as 16 kHz mono samples.
    pub utterances: Receiver<Vec<f32>>,
    pub source_hz: u32,
    pub channels: usize,
}

/// How often to check whether the default microphone changed.
///
/// Plugging in headphones changes it, and a stream opened on the old device
/// keeps delivering audio from a microphone nobody is speaking into — which
/// looks exactly like Minion having gone deaf, with nothing in the log.
const DEVICE_CHECK: Duration = Duration::from_secs(3);

/// Downmixes to mono and drops to 16 kHz by taking every Nth sample.
///
/// This is crude resampling with no anti-alias filter. For speech in a
/// narrow band it is good enough; if quality ever gets in the way, this is
/// the function to replace.
pub(crate) fn to_16k_mono(input: &[f32], channels: usize, source_hz: u32) -> Vec<f32> {
    let channels = channels.max(1);
    let step = (source_hz as f32 / TARGET_HZ as f32).max(1.0);
    let frames = input.len() / channels;
    let mut out = Vec::with_capacity((frames as f32 / step) as usize + 1);
    let mut position = 0.0f32;
    while (position as usize) < frames {
        let start = position as usize * channels;
        if start + channels > input.len() {
            break;
        }
        let mixed: f32 = input[start..start + channels].iter().sum::<f32>() / channels as f32;
        out.push(mixed);
        position += step;
    }
    out
}

fn rms(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt()
}

/// Tracks how loud the room is when nobody is speaking.
///
/// A fixed threshold only suits the room it was measured in: 0.015 is
/// generous in a quiet study and deaf in a café. This follows the quiet
/// moments and lifts the bar above them, so the same setting works in both.
struct NoiseFloor {
    /// Slow-moving estimate of background level.
    level: f32,
    /// Blocks seen, so the estimate can settle before it is trusted.
    samples: usize,
}

impl NoiseFloor {
    /// How much louder than the background speech must be.
    const MARGIN: f32 = 3.0;
    /// Weight of each new quiet block. Small, so a pause in speech does not
    /// drag the estimate up and deafen the detector mid-sentence.
    const ADAPT: f32 = 0.02;
    /// Blocks needed before the estimate is used at all (about a second).
    const SETTLE: usize = 50;
    /// Never let the adaptive threshold climb beyond this multiple of the
    /// configured floor: a persistently loud room should not silence
    /// everything, it should just be harder to talk over.
    const MAX_LIFT: f32 = 4.0;

    fn new() -> Self {
        Self { level: 0.0, samples: 0 }
    }

    /// Feeds a block that was judged not to be speech.
    fn observe_quiet(&mut self, rms: f32) {
        self.level = if self.samples == 0 {
            rms
        } else {
            self.level * (1.0 - Self::ADAPT) + rms * Self::ADAPT
        };
        self.samples += 1;
    }

    /// The level speech must exceed, given the configured floor.
    fn threshold(&self, floor: f32) -> f32 {
        if self.samples < Self::SETTLE {
            return floor;
        }
        (self.level * Self::MARGIN).clamp(floor, floor * Self::MAX_LIFT)
    }
}

/// Writes an utterance to a WAV file, for working out what went wrong.
///
/// Everything else can be reasoned about from the log; audio cannot. When
/// recognition behaves differently from every synthetic test, the only way
/// forward is to listen to what actually arrived.
pub fn save_recording(samples: &[f32], name: &str) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;

    let home = std::env::var("HOME").unwrap_or_default();
    let directory = std::path::PathBuf::from(home).join("Library/Application Support/Minion/recordings");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{name}.wav"));

    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();

    let mut file = std::fs::File::create(&path)?;
    let rate = TARGET_HZ;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data.len() as u32).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&1u16.to_le_bytes())?; // mono
    file.write_all(&rate.to_le_bytes())?;
    file.write_all(&(rate * 2).to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&(data.len() as u32).to_le_bytes())?;
    file.write_all(&data)?;
    Ok(path)
}

/// Splits a stream of blocks into utterances.
///
/// Kept separate from the audio plumbing so its behaviour can be tested
/// with synthetic signals rather than a live microphone.
struct Segmenter {
    settings: Settings,
    current: Vec<f32>,
    speech_blocks: usize,
    silence_blocks: usize,
    speaking: bool,
    noise: NoiseFloor,
}

impl Segmenter {
    fn new(settings: Settings) -> Self {
        Self {
            settings,
            current: Vec::new(),
            speech_blocks: 0,
            silence_blocks: 0,
            speaking: false,
            noise: NoiseFloor::new(),
        }
    }

    /// Feeds one block. Returns a finished utterance when there is one.
    fn push(&mut self, block: &[f32]) -> Option<Vec<f32>> {
        let level = rms(block);
        let threshold = self.noise.threshold(self.settings.speech_threshold);
        let has_speech = level > threshold;

        // Only quiet blocks outside an utterance update the estimate: the
        // gaps between words are not the room, they are part of speech.
        if !has_speech && !self.speaking {
            self.noise.observe_quiet(level);
        }

        if has_speech {
            self.speaking = true;
            self.speech_blocks += 1;
            self.silence_blocks = 0;
        } else if self.speaking {
            self.silence_blocks += 1;
        }

        if self.speaking {
            self.current.extend_from_slice(block);
        }

        let max_samples = self.settings.max_utterance_ms * TARGET_HZ as usize / 1000;
        let ended = self.speaking
            && self.silence_blocks >= self.settings.silence_end_ms / BLOCK_MS;
        let too_long = self.current.len() >= max_samples;

        if !ended && !too_long {
            return None;
        }

        let long_enough = self.speech_blocks >= self.settings.min_speech_ms / BLOCK_MS;
        let utterance = std::mem::take(&mut self.current);
        self.speaking = false;
        self.speech_blocks = 0;
        self.silence_blocks = 0;

        long_enough.then_some(utterance)
    }
}

/// Opens the microphone and starts delivering complete utterances.
///
/// `active` mutes processing without closing the device: while false,
/// incoming audio is discarded as it arrives. Closing and reopening the
/// microphone instead would make macOS re-check permissions each time.
/// The microphones available, by name.
///
/// Used to offer a choice in preferences. Anything that fails to describe
/// itself is left out rather than shown as a blank line.
pub fn input_names() -> Vec<String> {
    let Ok(devices) = cpal::default_host().input_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|device| device.description().ok())
        .map(|description| description.name().to_string())
        .collect()
}

/// Finds a microphone by name, or the system's default.
///
/// Falling back rather than failing: a device named in the configuration
/// may simply be unplugged, and being deaf is worse than using another one.
fn choose_input(preferred: Option<&str>) -> Result<cpal::platform::Device> {
    let host = cpal::default_host();
    if let Some(wanted) = preferred.filter(|name| !name.trim().is_empty()) {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                let matches = device
                    .description()
                    .is_ok_and(|description| description.name() == wanted);
                if matches {
                    return Ok(device);
                }
            }
        }
        crate::journal::write(&format!(
            "Microphone «{wanted}» not found; using the system default."
        ));
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no microphone available"))
}

/// Opens the chosen input, feeding `queue`.
fn open_default(
    queue: &Arc<Mutex<Vec<f32>>>,
    active: &Arc<AtomicBool>,
    preferred: Option<&str>,
) -> Result<(cpal::platform::Stream, u32, usize, cpal::DeviceId)> {
    let device = choose_input(preferred)?;
    let id = device.id()?;
    let config = device.default_input_config()?;
    let source_hz = config.sample_rate();
    let channels = config.channels() as usize;

    let capture_queue = Arc::clone(queue);
    let capture_active = Arc::clone(active);

    let stream = device.build_input_stream(
        config.into(),
        move |input: &[f32], _: &cpal::InputCallbackInfo| {
            // Audio callbacks must stay quick: resample and hand off, no
            // heavy work here or the stream glitches.
            if !capture_active.load(Ordering::Relaxed) {
                return;
            }
            let resampled = to_16k_mono(input, channels, source_hz);
            if let Ok(mut queued) = capture_queue.lock() {
                queued.extend_from_slice(&resampled);
                let limit = TARGET_HZ as usize * QUEUE_LIMIT_SECONDS;
                if queued.len() > limit {
                    let excess = queued.len() - limit;
                    queued.drain(..excess);
                }
            }
        },
        |err| eprintln!("audio error: {err}"),
        None,
    )?;
    stream.play()?;
    Ok((stream, source_hz, channels, id))
}

pub fn start(
    settings: Settings,
    active: Arc<AtomicBool>,
    preferred: Option<String>,
) -> Result<Listener> {
    let queue = Arc::new(Mutex::new(Vec::<f32>::new()));
    let (stream, source_hz, channels, device_id) =
        open_default(&queue, &active, preferred.as_deref())?;

    // Held so it can be swapped when the default input changes.
    let stream = Arc::new(Mutex::new(Some(stream)));

    // Watches for the default microphone changing, and follows it.
    let watch_stream = Arc::clone(&stream);
    let watch_queue = Arc::clone(&queue);
    let watch_active = Arc::clone(&active);
    // Only follows the system when no particular microphone was asked for:
    // choosing one is a decision, and following the default would undo it.
    let follows_system = preferred.as_ref().is_none_or(|name| name.trim().is_empty());
    std::thread::spawn(move || {
        if !follows_system {
            return;
        }
        let mut current = device_id;
        loop {
            std::thread::sleep(DEVICE_CHECK);
            let host = cpal::default_host();
            let Some(device) = host.default_input_device() else {
                continue;
            };
            let Ok(id) = device.id() else { continue };
            if id == current {
                continue;
            }
            // Drop the old stream before opening the new one: two streams
            // on one queue would interleave samples from both microphones.
            if let Ok(mut held) = watch_stream.lock() {
                *held = None;
            }
            match open_default(&watch_queue, &watch_active, None) {
                Ok((fresh, hz, channels, fresh_id)) => {
                    if let Ok(mut held) = watch_stream.lock() {
                        *held = Some(fresh);
                    }
                    current = fresh_id;
                    crate::journal::write(&format!(
                        "Microphone changed — now {hz} Hz, {channels} channel(s)."
                    ));
                }
                Err(e) => crate::journal::write(&format!("Cannot open the new microphone: {e}")),
            }
        }
    });

    let (send, utterances) = mpsc::channel();
    let segment_queue = Arc::clone(&queue);

    std::thread::spawn(move || {
        let mut segmenter = Segmenter::new(settings);
        loop {
            let pending: Vec<f32> = {
                let Ok(mut queued) = segment_queue.lock() else {
                    return;
                };
                if queued.len() < BLOCK_SAMPLES {
                    Vec::new()
                } else {
                    std::mem::take(&mut *queued)
                }
            };

            if pending.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(BLOCK_MS as u64));
                continue;
            }

            for block in pending.chunks(BLOCK_SAMPLES) {
                if let Some(utterance) = segmenter.push(block) {
                    if send.send(utterance).is_err() {
                        return; // nobody is listening any more
                    }
                }
            }
        }
    });

    Ok(Listener {
        _stream: stream,
        utterances,
        source_hz,
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks_of(level: f32, count: usize) -> Vec<Vec<f32>> {
        vec![vec![level; BLOCK_SAMPLES]; count]
    }

    fn feed(segmenter: &mut Segmenter, blocks: Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        blocks
            .iter()
            .filter_map(|b| segmenter.push(b))
            .collect()
    }

    #[test]
    fn emits_an_utterance_after_speech_then_silence() {
        let mut segmenter = Segmenter::new(Settings::default());
        // 1 s of signal, then enough silence to close it.
        assert!(feed(&mut segmenter, blocks_of(0.2, 50)).is_empty());
        let done = feed(&mut segmenter, blocks_of(0.0, 40));
        assert_eq!(done.len(), 1, "silence should close the utterance");
    }

    #[test]
    fn discards_noises_too_short_to_be_speech() {
        let mut segmenter = Segmenter::new(Settings::default());
        // A door slam: loud but brief.
        feed(&mut segmenter, blocks_of(0.5, 3));
        let done = feed(&mut segmenter, blocks_of(0.0, 40));
        assert!(done.is_empty(), "a 60 ms burst is not an utterance");
    }

    #[test]
    fn stays_quiet_through_silence() {
        let mut segmenter = Segmenter::new(Settings::default());
        assert!(feed(&mut segmenter, blocks_of(0.0, 200)).is_empty());
    }

    #[test]
    fn cuts_off_speech_that_never_stops() {
        // Continuous sound must be chopped rather than buffered forever.
        // The cut has to sit above min_speech_ms or nothing is ever emitted.
        let settings = Settings {
            max_utterance_ms: 600,
            min_speech_ms: 200,
            ..Default::default()
        };
        let mut segmenter = Segmenter::new(settings);
        let done = feed(&mut segmenter, blocks_of(0.2, 200));
        assert!(!done.is_empty(), "continuous noise must still be cut");
        assert!(
            done[0].len() <= 600 * TARGET_HZ as usize / 1000 + BLOCK_SAMPLES,
            "the cut should respect max_utterance_ms"
        );
    }

    #[test]
    fn quiet_rooms_keep_the_configured_floor() {
        let mut segmenter = Segmenter::new(Settings::default());
        // Near-silence for a while: the floor should not move.
        feed(&mut segmenter, blocks_of(0.001, 100));
        let threshold = segmenter.noise.threshold(Settings::default().speech_threshold);
        assert!(
            (threshold - Settings::default().speech_threshold).abs() < 1e-6,
            "a quiet room should not raise the bar"
        );
    }

    #[test]
    fn a_noisy_room_raises_the_bar() {
        let mut segmenter = Segmenter::new(Settings::default());
        // Steady background hum, well under the speech threshold but not
        // silence — a fan, a café, a fridge.
        feed(&mut segmenter, blocks_of(0.012, 400));
        let raised = segmenter.noise.threshold(Settings::default().speech_threshold);
        assert!(
            raised > Settings::default().speech_threshold,
            "background noise should lift the threshold, got {raised}"
        );
        assert!(
            raised <= Settings::default().speech_threshold * NoiseFloor::MAX_LIFT,
            "but never past the cap"
        );
    }

    #[test]
    fn speech_still_gets_through_a_noisy_room() {
        let mut segmenter = Segmenter::new(Settings::default());
        feed(&mut segmenter, blocks_of(0.012, 400));
        // Speaking up over that background must still register.
        assert!(feed(&mut segmenter, blocks_of(0.2, 50)).is_empty());
        let done = feed(&mut segmenter, blocks_of(0.012, 60));
        assert_eq!(done.len(), 1, "speech over noise should still be caught");
    }

    #[test]
    fn pauses_between_words_do_not_deafen_it() {
        // The gaps inside a sentence are not the room; if they fed the
        // estimate, the threshold would climb mid-sentence and cut it off.
        let mut segmenter = Segmenter::new(Settings::default());
        feed(&mut segmenter, blocks_of(0.001, 100));
        let before = segmenter.noise.threshold(0.015);
        feed(&mut segmenter, blocks_of(0.3, 20));
        feed(&mut segmenter, blocks_of(0.0, 10));
        feed(&mut segmenter, blocks_of(0.3, 20));
        let after = segmenter.noise.threshold(0.015);
        assert!((before - after).abs() < 1e-6, "speech gaps must not move the floor");
    }

    #[test]
    fn downmix_halves_stereo_and_drops_rate() {
        // 48 kHz stereo in, 16 kHz mono out: a third of the frames.
        let input: Vec<f32> = vec![1.0; 480 * 2];
        let out = to_16k_mono(&input, 2, 48_000);
        assert_eq!(out.len(), 160);
        assert!(out.iter().all(|s| (*s - 1.0).abs() < f32::EPSILON));
    }
}

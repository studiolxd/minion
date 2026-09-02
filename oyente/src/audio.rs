//! Microphone capture, chopped into utterances.
//!
//! The microphone delivers blocks at 48 kHz; the model wants 16 kHz mono.
//! And since the recogniser works on whole utterances, something has to
//! decide where one starts and ends — for now, signal energy does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

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
    _stream: cpal::platform::Stream,
    /// Completed utterances, as 16 kHz mono samples.
    pub utterances: Receiver<Vec<f32>>,
    pub source_hz: u32,
    pub channels: usize,
}

/// Downmixes to mono and drops to 16 kHz by taking every Nth sample.
///
/// This is crude resampling with no anti-alias filter. For speech in a
/// narrow band it is good enough; if quality ever gets in the way, this is
/// the function to replace.
fn to_16k_mono(input: &[f32], channels: usize, source_hz: u32) -> Vec<f32> {
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
}

impl Segmenter {
    fn new(settings: Settings) -> Self {
        Self {
            settings,
            current: Vec::new(),
            speech_blocks: 0,
            silence_blocks: 0,
            speaking: false,
        }
    }

    /// Feeds one block. Returns a finished utterance when there is one.
    fn push(&mut self, block: &[f32]) -> Option<Vec<f32>> {
        let has_speech = rms(block) > self.settings.speech_threshold;

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
pub fn start(settings: Settings, active: Arc<AtomicBool>) -> Result<Listener> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no microphone available"))?;
    let config = device.default_input_config()?;
    let source_hz = config.sample_rate();
    let channels = config.channels() as usize;

    let queue = Arc::new(Mutex::new(Vec::<f32>::new()));
    let capture_queue = Arc::clone(&queue);
    let capture_active = Arc::clone(&active);

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
    fn downmix_halves_stereo_and_drops_rate() {
        // 48 kHz stereo in, 16 kHz mono out: a third of the frames.
        let input: Vec<f32> = vec![1.0; 480 * 2];
        let out = to_16k_mono(&input, 2, 48_000);
        assert_eq!(out.len(), 160);
        assert!(out.iter().all(|s| (*s - 1.0).abs() < f32::EPSILON));
    }
}

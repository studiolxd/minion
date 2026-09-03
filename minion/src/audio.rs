//! Microphone capture, chopped into utterances.
//!
//! The microphone delivers blocks at 48 kHz; the model wants 16 kHz mono.
//! And since the recogniser works on whole utterances, something has to
//! decide where one starts and ends — for now, signal energy does.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// The neural voice detector. Nested here rather than declared in `main`
/// so it stays what it is: a part of how audio is segmented.
#[path = "silero.rs"]
mod silero;
use silero::Silero;

/// Sample rate the model expects.
pub const TARGET_HZ: u32 = 16_000;

/// Analysis block: 20 ms.
const BLOCK_SAMPLES: usize = TARGET_HZ as usize / 50;
const BLOCK_MS: usize = 20;

/// How much audio to keep from before speech was noticed.
///
/// A word is loudest in its middle, so by the time the level crosses the
/// threshold the first consonant is already gone — and "Chrome" arrives at
/// the recogniser as "Core". These blocks are held back and prepended, so
/// the utterance starts where the speaker did rather than where the meter
/// caught up.
const PREROLL_MS: usize = 320;
const PREROLL_BLOCKS: usize = PREROLL_MS / BLOCK_MS;

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
    ///
    /// It also sets the largest tensor the encoder ever sees, and ONNX
    /// Runtime's arena grows to the longest utterance of a session and
    /// stays there. Eight seconds is longer than any command anyone
    /// speaks and keeps that ceiling well below where twelve put it.
    pub max_utterance_ms: usize,
    /// Which detector decides that a sound is a voice.
    pub vad: Vad,
    /// Silero's score, from 0 to 1, above which a frame is speech.
    ///
    /// 0.5 is the model's own recommendation and what its published
    /// examples use.
    pub vad_threshold: f32,
}

/// What judges speech from noise.
///
/// Energy alone opens an utterance on anything louder than the room —
/// dishes, a television, the coffee machine — and each of those wakes the
/// speaker check and the recogniser to say no. Silero is asked the
/// question energy cannot answer, after the energy gate rather than
/// instead of it, so a quiet room still costs nothing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vad {
    /// The adaptive energy detector on its own.
    Energy,
    /// Silero in front of it — falling back to energy if the model is
    /// not on disk.
    Silero,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            speech_threshold: 0.015,
            silence_end_ms: 700,
            min_speech_ms: 300,
            max_utterance_ms: 8_000,
            vad: Vad::Silero,
            vad_threshold: 0.5,
        }
    }
}

/// One finished utterance, and where the speech inside it sits.
///
/// The samples carry the 320 ms preroll in front and the whole
/// `silence_end_ms` hangover behind, because the recogniser needs both: cut
/// the preroll and "Chrome" loses its consonant again. The speaker model
/// wants the opposite — a second of room tone per phrase drags its mean
/// towards the room rather than the voice — so the offsets say where the
/// energy actually crossed the threshold, and [`Utterance::speech`] hands
/// out that stretch alone.
#[derive(Clone, Debug)]
pub struct Utterance {
    /// 16 kHz mono, preroll and hangover included.
    pub samples: Vec<f32>,
    /// Where the first block above the threshold begins.
    pub speech_start: usize,
    /// Where the last block above the threshold ends.
    pub speech_end: usize,
}

impl Utterance {
    /// The speech alone, without the preroll or the trailing silence.
    ///
    /// NOTE: the speaker threshold (0.32) was measured on scores computed
    /// over the whole utterance, silence and all. Feeding only the speech
    /// should raise same-speaker scores — it removes the room tone that was
    /// pulling every embedding towards the same place — so the threshold
    /// wants re-measuring on real recordings before it is trusted as tight
    /// or as loose as it looks now.
    pub fn speech(&self) -> &[f32] {
        let end = self.speech_end.min(self.samples.len());
        let start = self.speech_start.min(end);
        &self.samples[start..end]
    }
}

/// How much perfectly silent audio proves the microphone is not ours.
///
/// Five seconds: long enough that a quiet room cannot be mistaken for it
/// — a real microphone always delivers some noise, never exact zeros —
/// and short enough that the answer arrives while the person is still
/// wondering why nothing happened.
const SILENCE_PROOF_SECONDS: usize = 5;

/// Watches the first seconds of capture for a microphone that was denied.
///
/// When macOS refuses the permission, cpal is not told: the stream opens,
/// the callbacks arrive on time and every sample in them is exactly zero.
/// So Minion says it is listening and hears nothing, for ever, with a
/// perfectly healthy log. Exact zeros are the tell — a working microphone
/// in a silent room still delivers its own noise floor.
#[derive(Default)]
struct SilenceWatch {
    /// Samples seen since the stream opened, while they were all zero.
    samples: AtomicUsize,
    /// Set once the question is answered, either way.
    settled: AtomicBool,
}

impl SilenceWatch {
    /// Feeds one block, raising `silent` if the case is proven.
    fn observe(&self, block: &[f32], silent: &AtomicBool) {
        if self.settled.load(Ordering::Relaxed) {
            return;
        }
        if block.iter().any(|sample| *sample != 0.0) {
            self.settled.store(true, Ordering::Relaxed);
            return;
        }
        let seen = self.samples.fetch_add(block.len(), Ordering::Relaxed) + block.len();
        if seen >= TARGET_HZ as usize * SILENCE_PROOF_SECONDS {
            self.settled.store(true, Ordering::Relaxed);
            silent.store(true, Ordering::Relaxed);
        }
    }

    /// Starts the count again, for a microphone that has just been swapped.
    fn restart(&self) {
        self.samples.store(0, Ordering::Relaxed);
        self.settled.store(false, Ordering::Relaxed);
    }
}

/// A running microphone. Dropping it stops capture.
pub struct Listener {
    /// Replaced when the system's default input changes, so the field is
    /// held rather than ignored.
    _stream: Arc<Mutex<Option<cpal::platform::Stream>>>,
    /// Completed utterances, as 16 kHz mono samples.
    pub utterances: Receiver<Utterance>,
    /// Raised as soon as an utterance opens, and cleared by whoever reads
    /// it. Speech takes a second or two to finish and the model takes about
    /// a second to load, so the loop can use the news to start loading
    /// while the sentence is still being said rather than after it.
    pub speech_started: Arc<AtomicBool>,
    pub source_hz: u32,
    pub channels: usize,
    /// Raised once when the microphone delivered nothing but exact zeros
    /// for [`SILENCE_PROOF_SECONDS`], which on macOS means the permission
    /// was denied. Cleared by whoever reads it.
    pub silent: Arc<AtomicBool>,
}

/// How often to check whether the default microphone changed.
///
/// Plugging in headphones changes it, and a stream opened on the old device
/// keeps delivering audio from a microphone nobody is speaking into — which
/// looks exactly like Minion having gone deaf, with nothing in the log.
const DEVICE_CHECK: Duration = Duration::from_secs(3);

/// Length of the anti-alias filter, in taps.
///
/// Odd, so the delay is a whole number of samples. Thirty-one taps with a
/// Hamming window give about 50 dB of stopband rejection and a transition
/// band of roughly 5 kHz at 48 kHz — enough to bury everything above 8 kHz
/// before it folds down, at a cost of one multiply-add per tap per sample.
const FILTER_TAPS: usize = 31;

/// Where the anti-alias filter stops passing, in Hz.
///
/// Below the 8 kHz Nyquist of the target rate, with room for the
/// transition band to roll off before it. Speech lives well below this;
/// what sits above it is what used to fold back onto the fricatives.
const CUTOFF_HZ: f32 = 7_000.0;

/// A windowed-sinc low-pass, normalised to unit gain at DC.
fn low_pass(cutoff_hz: f32, source_hz: f32, taps: usize) -> Vec<f32> {
    let middle = (taps - 1) as f32 / 2.0;
    let normalised = (cutoff_hz / source_hz).clamp(0.0, 0.5);
    let mut kernel: Vec<f32> = (0..taps)
        .map(|i| {
            let x = i as f32 - middle;
            let sinc = if x.abs() < f32::EPSILON {
                2.0 * normalised
            } else {
                (2.0 * std::f32::consts::PI * normalised * x).sin() / (std::f32::consts::PI * x)
            };
            // Hamming: the ripple it leaves is far below the noise floor of
            // any microphone this will ever run on.
            let window = 0.54
                - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (taps - 1) as f32).cos();
            sinc * window
        })
        .collect();
    let sum: f32 = kernel.iter().sum();
    if sum.abs() > f32::EPSILON {
        kernel.iter_mut().for_each(|tap| *tap /= sum);
    }
    kernel
}

/// Downmixes to mono and converts to 16 kHz, filtering before it decimates.
///
/// Taking every Nth sample without a filter folds everything between 8 and
/// 24 kHz back into the band the model listens to. The fricatives live at
/// the top of that band — which is why "Chrome" and "Safari" came back as
/// "crumb" and "so fuddy" while "terminal" never failed. So: low-pass at
/// [`CUTOFF_HZ`] first, then read the filtered signal at the fractional
/// positions the rate ratio asks for.
///
/// The kernel depends only on the source rate, so it is computed once and
/// kept; the scratch buffers are reused, because this runs inside the
/// CoreAudio callback where an allocation is a glitch waiting to happen.
pub(crate) struct Resampler {
    /// Filter taps for `source_hz`, or empty when no filtering is needed.
    kernel: Vec<f32>,
    /// Input samples per output sample.
    step: f32,
    /// Where in the next block the first output sample falls.
    position: f32,
    /// The tail of the previous block, so the filter has no seam at the
    /// block boundary.
    history: Vec<f32>,
    /// Mono input, history first: reused between calls.
    mono: Vec<f32>,
    /// The filtered signal, one sample per input frame.
    filtered: Vec<f32>,
}

impl Resampler {
    pub(crate) fn new(source_hz: u32) -> Self {
        // Upsampling cannot alias: a signal already band-limited to the
        // source Nyquist stays band-limited. Filtering there would only
        // eat into the speech it is meant to protect.
        let kernel = if source_hz > TARGET_HZ {
            low_pass(CUTOFF_HZ, source_hz as f32, FILTER_TAPS)
        } else {
            Vec::new()
        };
        let history = vec![0.0; kernel.len().saturating_sub(1)];
        Self {
            kernel,
            step: source_hz as f32 / TARGET_HZ as f32,
            position: 0.0,
            history,
            mono: Vec::new(),
            filtered: Vec::new(),
        }
    }

    /// Resamples one block into the reusable scratch buffer, and returns it.
    pub(crate) fn process(&mut self, input: &[f32], channels: usize) -> &[f32] {
        let channels = channels.max(1);
        let frames = input.len() / channels;

        // Mono, with the previous block's tail in front of it so the first
        // filtered samples see the same history as the rest.
        self.mono.clear();
        self.mono.extend_from_slice(&self.history);
        let offset = self.history.len();
        for frame in 0..frames {
            let start = frame * channels;
            let sum: f32 = input[start..start + channels].iter().sum();
            self.mono.push(sum / channels as f32);
        }

        self.filtered.clear();
        if self.kernel.is_empty() {
            self.filtered.extend_from_slice(&self.mono[offset..]);
        } else {
            for frame in 0..frames {
                let window = &self.mono[frame..frame + offset + 1];
                let value: f32 = self
                    .kernel
                    .iter()
                    .zip(window.iter().rev())
                    .map(|(tap, sample)| tap * sample)
                    .sum();
                self.filtered.push(value);
            }
        }

        // Keep the tail for the next block.
        if offset > 0 {
            let start = self.mono.len() - offset;
            self.history.clear();
            self.history.extend_from_slice(&self.mono[start..]);
        }

        // Read the filtered signal where the rate ratio asks, interpolating
        // between neighbours: a device at 44 100 Hz lands between samples
        // two times out of three.
        self.mono.clear(); // reused as the output scratch
        while self.position < frames as f32 {
            let index = self.position as usize;
            let fraction = self.position - index as f32;
            let here = self.filtered[index];
            let next = *self.filtered.get(index + 1).unwrap_or(&here);
            self.mono.push(here + (next - here) * fraction);
            self.position += self.step;
        }
        self.position -= frames as f32;
        &self.mono
    }
}

/// Downmixes to mono and converts to 16 kHz.
///
/// A one-shot wrapper around [`Resampler`] for callers holding a whole
/// recording. The live path keeps a resampler instead, so the kernel is
/// built once rather than per block — which leaves this used only by the
/// tests, here and in `speaker`.
#[cfg(test)]
pub(crate) fn to_16k_mono(input: &[f32], channels: usize, source_hz: u32) -> Vec<f32> {
    Resampler::new(source_hz).process(input, channels).to_vec()
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
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let home = std::env::var("HOME").unwrap_or_default();
    let directory = std::path::PathBuf::from(home).join("Library/Application Support/Minion/recordings");
    std::fs::create_dir_all(&directory)?;
    // Raw audio of whatever was said near the machine: the directory and
    // the files in it should not be readable by other accounts.
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let path = directory.join(format!("{name}.wav"));

    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
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
    /// Where the speech starts and ends inside `current`.
    speech_start: usize,
    speech_end: usize,
    noise: NoiseFloor,
    /// The most recent quiet blocks, kept in case speech starts.
    preroll: std::collections::VecDeque<Vec<f32>>,
    /// The neural detector, when there is one. Without it the energy
    /// gate decides on its own, exactly as it always did.
    voice: Option<Silero>,
    /// Silero's verdict on the most recent frame it completed, or `None`
    /// when it has not heard a whole frame of this burst yet.
    score: Option<f32>,
    /// Blocks left of the window in which Silero may still veto an
    /// utterance that energy opened. `None` once it has spoken up.
    probation: Option<usize>,
    /// Whether Silero has agreed that this utterance is speech.
    approved: bool,
}

/// How long Silero gets to disagree with the energy gate.
///
/// The model needs a couple of frames of a sound before it commits, and a
/// word's first consonant is the part it is least sure about — so the veto
/// is not a single frame but the best score of the first 200 ms. Longer
/// than that and the noises this exists to catch would already have woken
/// the recogniser.
const PROBATION_MS: usize = 200;
const PROBATION_BLOCKS: usize = PROBATION_MS / BLOCK_MS;

impl Segmenter {
    fn new(settings: Settings, voice: Option<Silero>) -> Self {
        Self {
            settings,
            current: Vec::new(),
            speech_blocks: 0,
            silence_blocks: 0,
            speaking: false,
            speech_start: 0,
            speech_end: 0,
            noise: NoiseFloor::new(),
            preroll: std::collections::VecDeque::with_capacity(PREROLL_BLOCKS + 1),
            voice,
            score: None,
            probation: None,
            approved: false,
        }
    }

    /// Silero's answer for this block, or `true` when there is nobody to
    /// ask and when it has not heard enough to answer yet.
    ///
    /// Only ever called for blocks the energy gate already passed, so
    /// silence never costs an inference. The recurrent state is kept
    /// across the quiet blocks inside a sentence — the gaps between words
    /// are part of it — and dropped only when the utterance ends.
    fn voice_agrees(&mut self, block: &[f32], energetic: bool) -> bool {
        let Some(model) = self.voice.as_mut() else {
            return energetic;
        };
        if !energetic {
            return false;
        }
        if let Some(score) = model.push(block) {
            self.score = Some(score);
        }
        // Before the first whole frame there is no verdict yet, and the
        // benefit of the doubt goes to the speaker: the 200 ms window
        // below is what actually settles it.
        self.score.is_none_or(|score| score >= self.settings.vad_threshold)
    }

    /// Feeds one block. Returns a finished utterance when there is one.
    fn push(&mut self, block: &[f32]) -> Option<Utterance> {
        let level = rms(block);
        let threshold = self.noise.threshold(self.settings.speech_threshold);
        let energetic = level > threshold;
        // Energy first, then the model: a block that is not louder than
        // the room is not speech whatever Silero would have said, and
        // asking would cost an inference per 20 ms of silence.
        let has_speech = self.voice_agrees(block, energetic);

        // Only quiet blocks outside an utterance update the estimate: the
        // gaps between words are not the room, they are part of speech.
        // Judged on energy alone — a noise Silero threw out is still not
        // the background level, and letting it in would raise the bar
        // every time the kettle boiled.
        if !energetic && !self.speaking {
            self.noise.observe_quiet(level);
        }

        if has_speech {
            if !self.speaking {
                // Speech is starting: put back what was held.
                for held in self.preroll.drain(..) {
                    self.current.extend_from_slice(&held);
                }
                // Everything replaced above is room tone: the speech proper
                // starts here.
                self.speech_start = self.current.len();
                // Silero now has 200 ms to say this was not a voice.
                self.probation = self.voice.is_some().then_some(PROBATION_BLOCKS);
                self.approved = self.voice.is_none();
            }
            self.speaking = true;
            self.speech_blocks += 1;
            self.silence_blocks = 0;
        } else if self.speaking {
            self.silence_blocks += 1;
        } else {
            // Quiet, and not in an utterance: remember it briefly.
            self.preroll.push_back(block.to_vec());
            if self.preroll.len() > PREROLL_BLOCKS {
                self.preroll.pop_front();
            }
        }

        if self.speaking {
            self.current.extend_from_slice(block);
            if has_speech {
                // The last block with energy in it: everything after this
                // is the hangover that closes the utterance.
                self.speech_end = self.current.len();
            }
        }

        // Silero's veto: an utterance whose first 200 ms never scored as
        // speech is the dishwasher, not an order. Dropped here rather
        // than after the recogniser has been woken to transcribe it.
        if let Some(left) = self.probation {
            if self.score.is_some_and(|score| score >= self.settings.vad_threshold) {
                self.approved = true;
            }
            self.probation = match (self.approved, left - 1) {
                (true, _) => None,
                (false, 0) => {
                    self.reset();
                    return None;
                }
                (false, left) => Some(left),
            };
        }

        let max_samples = self.settings.max_utterance_ms * TARGET_HZ as usize / 1000;
        let ended = self.speaking
            && self.silence_blocks >= self.settings.silence_end_ms / BLOCK_MS;
        let too_long = self.current.len() >= max_samples;

        if !ended && !too_long {
            return None;
        }

        let long_enough = self.speech_blocks >= self.settings.min_speech_ms / BLOCK_MS;
        let samples = std::mem::take(&mut self.current);
        let utterance = Utterance {
            speech_start: self.speech_start,
            speech_end: self.speech_end.max(self.speech_start),
            samples,
        };
        self.reset();

        long_enough.then_some(utterance)
    }

    /// Forgets whatever was being collected.
    fn reset(&mut self) {
        self.current.clear();
        self.speaking = false;
        self.speech_blocks = 0;
        self.silence_blocks = 0;
        self.speech_start = 0;
        self.speech_end = 0;
        self.preroll.clear();
        self.probation = None;
        self.approved = false;
        self.score = None;
        // The next utterance is not a continuation of this one.
        if let Some(model) = self.voice.as_mut() {
            model.reset();
        }
    }

    /// Feeds one block unless Minion is talking.
    ///
    /// It listens continuously, so its own answers come straight back in
    /// through the microphone. Discarding the blocks is not enough on its
    /// own: an utterance that was already open when it started speaking
    /// would otherwise be closed by the reply and handed on as if someone
    /// had said it, so that one is dropped too.
    fn push_unless_deaf(&mut self, block: &[f32], deaf: bool) -> Option<Utterance> {
        if deaf {
            self.reset();
            return None;
        }
        self.push(block)
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
    watch: &Arc<SilenceWatch>,
    silent: &Arc<AtomicBool>,
) -> Result<(cpal::platform::Stream, u32, usize, cpal::DeviceId)> {
    let device = choose_input(preferred)?;
    let id = device.id()?;
    let config = device.default_input_config()?;
    let source_hz = config.sample_rate();
    let channels = config.channels() as usize;

    let capture_queue = Arc::clone(queue);
    let capture_active = Arc::clone(active);
    let capture_watch = Arc::clone(watch);
    let capture_silent = Arc::clone(silent);
    // Built once per stream: the filter kernel and the scratch buffers are
    // kept between callbacks so nothing allocates in the hot path.
    let mut resampler = Resampler::new(source_hz);

    let stream = device.build_input_stream(
        config.into(),
        move |input: &[f32], _: &cpal::InputCallbackInfo| {
            // Audio callbacks must stay quick: resample and hand off, no
            // heavy work here or the stream glitches.
            if !capture_active.load(Ordering::Relaxed) {
                return;
            }
            let resampled = resampler.process(input, channels);
            capture_watch.observe(resampled, &capture_silent);
            if let Ok(mut queued) = capture_queue.lock() {
                queued.extend_from_slice(resampled);
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

/// Loads the neural detector, when it is wanted and it is there.
///
/// Missing model, unreadable model, a build of it this code cannot run:
/// all of them fall back to the energy detector, which is what Minion did
/// before Silero existed. Being slightly too eager to listen is a much
/// smaller failure than being deaf.
fn open_voice(settings: Settings, model_dir: Option<&str>) -> Option<Silero> {
    if settings.vad != Vad::Silero {
        crate::journal::write("Voice detector: energy only, by configuration.");
        return None;
    }
    let directory = std::path::Path::new(model_dir?);
    // An installation made before Silero existed has every other model
    // and never asks for this one again, so fetch it here — 2 MB, once,
    // pinned and hashed like the rest.
    let path = match crate::models::fetch_vad(directory) {
        Ok(path) => path,
        Err(e) => {
            crate::journal::write(&format!(
                "Voice detector: silero_vad.onnx is not there and could not be \
                 downloaded ({e}); using the energy detector."
            ));
            return None;
        }
    };
    match Silero::load(&path) {
        Ok(model) => {
            crate::journal::write("Voice detector: Silero.");
            Some(model)
        }
        Err(e) => {
            crate::journal::write(&format!(
                "Voice detector: cannot load Silero ({e}); using the energy detector."
            ));
            None
        }
    }
}

pub fn start(
    settings: Settings,
    active: Arc<AtomicBool>,
    preferred: Option<String>,
    deaf: Arc<AtomicBool>,
    model_dir: Option<String>,
) -> Result<Listener> {
    let queue = Arc::new(Mutex::new(Vec::<f32>::new()));
    let watch = Arc::new(SilenceWatch::default());
    let silent = Arc::new(AtomicBool::new(false));
    let (stream, source_hz, channels, device_id) =
        open_default(&queue, &active, preferred.as_deref(), &watch, &silent)?;

    // Held so it can be swapped when the default input changes.
    let stream = Arc::new(Mutex::new(Some(stream)));

    // Watches for the default microphone changing, and follows it.
    let watch_stream = Arc::clone(&stream);
    let watch_queue = Arc::clone(&queue);
    let watch_active = Arc::clone(&active);
    let watch_silence = Arc::clone(&watch);
    let watch_silent = Arc::clone(&silent);
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
            // A different device deserves its own five seconds.
            watch_silence.restart();
            match open_default(&watch_queue, &watch_active, None, &watch_silence, &watch_silent) {
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
    let speech_started = Arc::new(AtomicBool::new(false));
    let segment_started = Arc::clone(&speech_started);

    std::thread::spawn(move || {
        // Loaded here, on the segmenter's own thread: the CoreAudio
        // callback must never touch it, and this is the only thread that
        // ever will.
        let voice = open_voice(settings, model_dir.as_deref());
        let mut segmenter = Segmenter::new(settings, voice);
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

            let speaking_now = deaf.load(Ordering::Relaxed);
            for block in pending.chunks(BLOCK_SAMPLES) {
                let utterance = segmenter.push_unless_deaf(block, speaking_now);
                // Announced while it is still being spoken, not when it
                // ends: whoever is waiting has work it can start now.
                if segmenter.speaking {
                    segment_started.store(true, Ordering::Relaxed);
                }
                if let Some(utterance) = utterance {
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
        speech_started,
        source_hz,
        channels,
        silent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A segmenter with no neural detector: the energy path, which is
    /// what almost every test below is about.
    fn plain(settings: Settings) -> Segmenter {
        Segmenter::new(settings, None)
    }

    fn blocks_of(level: f32, count: usize) -> Vec<Vec<f32>> {
        vec![vec![level; BLOCK_SAMPLES]; count]
    }

    fn feed(segmenter: &mut Segmenter, blocks: Vec<Vec<f32>>) -> Vec<Utterance> {
        blocks
            .iter()
            .filter_map(|b| segmenter.push(b))
            .collect()
    }

    #[test]
    fn emits_an_utterance_after_speech_then_silence() {
        let mut segmenter = plain(Settings::default());
        // 1 s of signal, then enough silence to close it.
        assert!(feed(&mut segmenter, blocks_of(0.2, 50)).is_empty());
        let done = feed(&mut segmenter, blocks_of(0.0, 40));
        assert_eq!(done.len(), 1, "silence should close the utterance");
    }

    #[test]
    fn the_start_of_a_word_is_not_lost() {
        // The level crosses the threshold partway into the first syllable,
        // so an utterance that begins exactly where the meter noticed is
        // already missing its opening consonant.
        let mut segmenter = plain(Settings::default());
        feed(&mut segmenter, blocks_of(0.0, 40)); // quiet room
        feed(&mut segmenter, blocks_of(0.2, 50)); // speech
        let done = feed(&mut segmenter, blocks_of(0.0, 80));

        assert_eq!(done.len(), 1);
        let expected = (50 + PREROLL_BLOCKS) * BLOCK_SAMPLES;
        assert!(
            done[0].samples.len() >= expected,
            "the utterance should carry {PREROLL_MS} ms from before it started: \
             got {} samples, expected at least {expected}",
            done[0].samples.len()
        );
    }

    #[test]
    fn the_speech_slice_leaves_the_silence_behind() {
        // The recogniser wants the preroll and the hangover; the speaker
        // model does not — a second of room tone per phrase drags its mean
        // away from the voice.
        let mut segmenter = plain(Settings::default());
        feed(&mut segmenter, blocks_of(0.0, 40)); // quiet room
        feed(&mut segmenter, blocks_of(0.2, 50)); // 1 s of speech
        let done = feed(&mut segmenter, blocks_of(0.0, 80));
        assert_eq!(done.len(), 1);
        let utterance = &done[0];

        assert_eq!(
            utterance.speech_start,
            PREROLL_BLOCKS * BLOCK_SAMPLES,
            "the speech should start where the preroll ends"
        );
        assert_eq!(
            utterance.speech().len(),
            50 * BLOCK_SAMPLES,
            "the speech slice should be the speech and nothing else"
        );
        assert!(
            utterance.samples.len() > utterance.speech().len(),
            "while the samples themselves keep both margins"
        );
        assert!(
            utterance.speech().iter().all(|s| *s > 0.1),
            "no silence should have survived the trim"
        );
    }

    #[test]
    fn a_speech_slice_is_always_inside_its_samples() {
        // Whatever the offsets say, slicing must not panic.
        let utterance = Utterance {
            samples: vec![0.0; 10],
            speech_start: 8,
            speech_end: 400,
        };
        assert_eq!(utterance.speech().len(), 2);
        let backwards = Utterance {
            samples: vec![0.0; 10],
            speech_start: 9,
            speech_end: 3,
        };
        assert!(backwards.speech().is_empty());
    }

    #[test]
    fn nothing_heard_while_minion_is_speaking_becomes_an_utterance() {
        // Its own voice, arriving through the microphone while it talks.
        let mut segmenter = plain(Settings::default());
        for block in blocks_of(0.2, 100) {
            assert!(segmenter.push_unless_deaf(&block, true).is_none());
        }
        // And the silence that follows must not close anything either:
        // there is nothing open to close.
        let done = feed(&mut segmenter, blocks_of(0.0, 80));
        assert!(done.is_empty(), "its own voice must not become an order");
    }

    #[test]
    fn an_utterance_open_when_minion_starts_speaking_is_dropped() {
        // Half a sentence plus Minion's reply is not a sentence.
        let mut segmenter = plain(Settings::default());
        feed(&mut segmenter, blocks_of(0.2, 50));
        assert!(segmenter.push_unless_deaf(&vec![0.2; BLOCK_SAMPLES], true).is_none());
        let done = feed(&mut segmenter, blocks_of(0.0, 80));
        assert!(done.is_empty(), "what was half-said should be forgotten");
    }

    #[test]
    fn silence_alone_never_becomes_an_utterance() {
        // The held-back blocks must not accumulate into one.
        let mut segmenter = plain(Settings::default());
        assert!(feed(&mut segmenter, blocks_of(0.0, 500)).is_empty());
    }

    #[test]
    fn discards_noises_too_short_to_be_speech() {
        let mut segmenter = plain(Settings::default());
        // A door slam: loud but brief.
        feed(&mut segmenter, blocks_of(0.5, 3));
        let done = feed(&mut segmenter, blocks_of(0.0, 40));
        assert!(done.is_empty(), "a 60 ms burst is not an utterance");
    }

    #[test]
    fn stays_quiet_through_silence() {
        let mut segmenter = plain(Settings::default());
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
        let mut segmenter = plain(settings);
        let done = feed(&mut segmenter, blocks_of(0.2, 200));
        assert!(!done.is_empty(), "continuous noise must still be cut");
        assert!(
            done[0].samples.len() <= 600 * TARGET_HZ as usize / 1000 + BLOCK_SAMPLES,
            "the cut should respect max_utterance_ms"
        );
    }

    #[test]
    fn quiet_rooms_keep_the_configured_floor() {
        let mut segmenter = plain(Settings::default());
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
        let mut segmenter = plain(Settings::default());
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
        let mut segmenter = plain(Settings::default());
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
        let mut segmenter = plain(Settings::default());
        feed(&mut segmenter, blocks_of(0.001, 100));
        let before = segmenter.noise.threshold(0.015);
        feed(&mut segmenter, blocks_of(0.3, 20));
        feed(&mut segmenter, blocks_of(0.0, 10));
        feed(&mut segmenter, blocks_of(0.3, 20));
        let after = segmenter.noise.threshold(0.015);
        assert!((before - after).abs() < 1e-6, "speech gaps must not move the floor");
    }

    /// The detector, if it has been downloaded. Absent, the tests that
    /// need it are skipped rather than failed — it is not in git, same as
    /// the speaker model.
    fn voice_if_present() -> Option<Silero> {
        let mut candidates = vec![std::path::PathBuf::from("model")];
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                std::path::PathBuf::from(home).join("Library/Application Support/Minion/model"),
            );
        }
        candidates
            .into_iter()
            .map(|dir| dir.join("silero_vad.onnx"))
            .find(|path| path.exists())
            .and_then(|path| Silero::load(&path).ok())
    }

    /// Cuts a signal into the 20 ms blocks the segmenter is fed.
    fn as_blocks(samples: &[f32]) -> Vec<Vec<f32>> {
        samples.chunks(BLOCK_SAMPLES).map(|b| b.to_vec()).collect()
    }

    /// Loud white noise: the extractor fan, the coffee machine, the tap.
    fn hiss(level: f32, seconds: f32) -> Vec<Vec<f32>> {
        as_blocks(&silero::sounds::noise(level, seconds))
    }

    #[test]
    fn a_loud_noise_no_longer_becomes_an_utterance() {
        // The whole point: energy alone opens on this, and then the
        // speaker model and Parakeet are woken up to decide it was the
        // kitchen.
        let Some(voice) = voice_if_present() else { return };
        let mut segmenter = Segmenter::new(Settings::default(), Some(voice));
        assert!(feed(&mut segmenter, hiss(0.3, 2.0)).is_empty(), "noise is not an order");
        assert!(feed(&mut segmenter, blocks_of(0.0, 40)).is_empty());
    }

    #[test]
    fn a_voice_still_gets_through_silero() {
        // And the other half: what Silero rejects must not include speech.
        let Some(voice) = voice_if_present() else { return };
        let Some(speech) = silero::sounds::spoken("Minion, abre Chrome.") else { return };
        let mut segmenter = Segmenter::new(Settings::default(), Some(voice));
        feed(&mut segmenter, as_blocks(&speech));
        let done = feed(&mut segmenter, blocks_of(0.0, 60));
        assert_eq!(done.len(), 1, "a spoken order must still be heard");
    }

    #[test]
    fn without_the_model_the_energy_detector_decides_alone() {
        // The fallback has to behave exactly as it did before, or a
        // missing 2 MB file changes what Minion hears.
        let mut segmenter = Segmenter::new(Settings::default(), None);
        feed(&mut segmenter, hiss(0.3, 1.0));
        let done = feed(&mut segmenter, blocks_of(0.0, 40));
        assert_eq!(done.len(), 1, "energy alone still opens on noise");
    }

    #[test]
    fn a_microphone_that_only_ever_delivers_zeros_is_reported() {
        let watch = SilenceWatch::default();
        let silent = AtomicBool::new(false);
        let quiet = vec![0.0f32; BLOCK_SAMPLES];
        let blocks = TARGET_HZ as usize * SILENCE_PROOF_SECONDS / BLOCK_SAMPLES;
        for _ in 0..blocks - 1 {
            watch.observe(&quiet, &silent);
            assert!(!silent.load(Ordering::Relaxed), "not proven yet");
        }
        watch.observe(&quiet, &silent);
        assert!(silent.load(Ordering::Relaxed), "five seconds of exact zeros");
    }

    #[test]
    fn a_quiet_room_is_not_a_denied_microphone() {
        // A working microphone delivers its own noise floor, never exact
        // zeros — so anything at all settles the question for good.
        let watch = SilenceWatch::default();
        let silent = AtomicBool::new(false);
        watch.observe(&[0.0, 0.0, 1e-7, 0.0], &silent);
        for _ in 0..TARGET_HZ as usize * SILENCE_PROOF_SECONDS / BLOCK_SAMPLES + 10 {
            watch.observe(&vec![0.0f32; BLOCK_SAMPLES], &silent);
        }
        assert!(!silent.load(Ordering::Relaxed), "silence after speech is just silence");
    }

    #[test]
    fn downmix_halves_stereo_and_drops_rate() {
        // 48 kHz stereo in, 16 kHz mono out: a third of the frames.
        let input: Vec<f32> = vec![1.0; 480 * 2];
        let out = to_16k_mono(&input, 2, 48_000);
        assert_eq!(out.len(), 160);
        // A constant is DC: a unit-gain low-pass leaves it alone, once the
        // filter's own history has filled with it.
        assert!(out[60..].iter().all(|s| (*s - 1.0).abs() < 1e-3));
    }

    /// A tone of `hz`, `seconds` long, sampled at `rate`.
    fn tone(hz: f32, rate: f32, seconds: f32) -> Vec<f32> {
        let count = (rate * seconds) as usize;
        (0..count)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate).sin())
            .collect()
    }

    fn rms_of(samples: &[f32]) -> f32 {
        rms(samples)
    }

    #[test]
    fn resampling_kills_what_would_fold_back() {
        // 12 kHz at 48 kHz has nowhere to go at 16 kHz: without a filter it
        // reappears at 4 kHz, at full strength, right on top of the
        // fricatives. This is the whole reason the filter exists.
        let input = tone(12_000.0, 48_000.0, 1.0);
        let out = to_16k_mono(&input, 1, 48_000);
        // Skip the start, where the filter is still filling up.
        let level = rms_of(&out[200..]);
        let attenuation = 20.0 * (level / 0.707).log10();
        assert!(
            attenuation < -30.0,
            "12 kHz should be buried, got {attenuation:.1} dB"
        );
    }

    #[test]
    fn resampling_leaves_speech_alone() {
        // 1 kHz is squarely in the band the model listens to; the filter
        // must not touch it.
        let input = tone(1_000.0, 48_000.0, 1.0);
        let out = to_16k_mono(&input, 1, 48_000);
        let level = rms_of(&out[200..]);
        let change = 20.0 * (level / 0.707).log10();
        assert!(
            change.abs() < 1.0,
            "1 kHz should come through unchanged, got {change:.2} dB"
        );
    }

    #[test]
    fn every_source_rate_gives_the_right_number_of_samples() {
        // Including rates below the target: those must be stretched, not
        // passed through at the wrong speed and played back as chipmunks.
        for (rate, seconds) in [(48_000u32, 1.0f32), (44_100, 1.0), (16_000, 1.0), (8_000, 1.0)] {
            let input = tone(440.0, rate as f32, seconds);
            let out = to_16k_mono(&input, 1, rate);
            let expected = (TARGET_HZ as f32 * seconds) as usize;
            let slack = 2;
            assert!(
                out.len().abs_diff(expected) <= slack,
                "{rate} Hz should give about {expected} samples, got {}",
                out.len()
            );
        }
    }

    #[test]
    fn block_by_block_matches_one_long_call() {
        // The live path feeds the resampler in 10 ms blocks; the tests feed
        // it whole recordings. The two must agree, or the filter has a seam
        // at every block boundary.
        let input = tone(1_000.0, 48_000.0, 0.5);
        let whole = to_16k_mono(&input, 1, 48_000);

        let mut piecewise = Vec::new();
        let mut resampler = Resampler::new(48_000);
        for block in input.chunks(480) {
            piecewise.extend_from_slice(resampler.process(block, 1));
        }

        assert!(whole.len().abs_diff(piecewise.len()) <= 1);
        let shared = whole.len().min(piecewise.len());
        for i in 0..shared {
            assert!(
                (whole[i] - piecewise[i]).abs() < 1e-4,
                "sample {i} differs: {} vs {}",
                whole[i],
                piecewise[i]
            );
        }
    }
}

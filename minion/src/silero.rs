//! Telling speech from noise, with Silero VAD.
//!
//! The energy detector cannot hear the difference between a sentence and a
//! coffee machine: anything louder than the room opens an utterance, and
//! then the speaker model and Parakeet are woken up to decide that the
//! dishwasher is not its owner. Silero is a 2 MB network that answers the
//! one question energy cannot — is this a voice? — for a fraction of the
//! cost of the models it keeps from running.
//!
//! It works on fixed 512-sample frames (32 ms at 16 kHz) and carries two
//! things from one frame to the next: an LSTM state, and the last 64
//! samples of audio, which are prepended to the next frame. Both must be
//! reset when the audio jumps, and the second is not optional — feeding
//! bare 512-sample frames looks like it works and scores clear speech at
//! 0.2, which is to say it rejects everything.

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::TensorRef;

/// Samples of new audio per frame. The model accepts nothing else at
/// 16 kHz.
pub const FRAME_SAMPLES: usize = 512;

/// How much of the previous frame goes in front of this one.
///
/// The model's own wrapper does this and the network was trained with it:
/// without the overlap every score collapses towards zero.
const CONTEXT_SAMPLES: usize = 64;

/// Shape of the recurrent state the model hands back with every frame.
///
/// `[2, 1, 128]`: the two halves are the LSTM's hidden and cell vectors,
/// which v5 carries in one tensor rather than two inputs.
const STATE_SHAPE: [i64; 3] = [2, 1, 128];
const STATE_SAMPLES: usize = 2 * 128;

/// The sample rate, as the model's `sr` input wants it.
const RATE: [i64; 1] = [crate::audio::TARGET_HZ as i64];

pub struct Silero {
    session: Session,
    /// The LSTM state, carried from one frame to the next.
    state: Vec<f32>,
    /// The tail of the previous frame, prepended to the next one.
    context: Vec<f32>,
    /// Samples that did not fill a frame, kept for the next push.
    pending: Vec<f32>,
    /// Context and frame together: reused so a frame costs no allocation.
    input: Vec<f32>,
}

impl Silero {
    /// Loads the model from `path`.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        // Set up like the speaker model, and for the same reasons: one
        // small question at a time, so the defaults' thread per core and
        // ever-growing arena are all cost and no benefit.
        let session = Session::builder()
            .map_err(|e| anyhow!("{e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow!("{e}"))?
            .with_inter_threads(1)
            .map_err(|e| anyhow!("{e}"))?
            .with_memory_pattern(false)
            .map_err(|e| anyhow!("{e}"))?
            .with_config_entry("session.disable_prepacking", "1")
            .map_err(|e| anyhow!("{e}"))?
            .with_config_entry("session.use_device_allocator_for_initializers", "1")
            .map_err(|e| anyhow!("{e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("loading the voice detector from {}", path.display()))?;
        Ok(Self {
            session,
            state: vec![0.0; STATE_SAMPLES],
            context: vec![0.0; CONTEXT_SAMPLES],
            pending: Vec::with_capacity(FRAME_SAMPLES * 2),
            input: Vec::with_capacity(CONTEXT_SAMPLES + FRAME_SAMPLES),
        })
    }

    /// Forgets everything heard so far.
    ///
    /// Both the state and the overlap summarise the audio that came
    /// before, so carrying them across a gap would have the model judge
    /// one sound by another.
    pub fn reset(&mut self) {
        self.state.clear();
        self.state.resize(STATE_SAMPLES, 0.0);
        self.context.clear();
        self.context.resize(CONTEXT_SAMPLES, 0.0);
        self.pending.clear();
    }

    /// Feeds audio, returning the score of the last frame it completed.
    ///
    /// `None` when there was not enough for a whole frame: the samples are
    /// kept and joined to the next push rather than judged short.
    pub fn push(&mut self, samples: &[f32]) -> Option<f32> {
        self.pending.extend_from_slice(samples);
        let mut latest = None;
        while self.pending.len() >= FRAME_SAMPLES {
            self.input.clear();
            self.input.extend_from_slice(&self.context);
            self.input.extend(self.pending.drain(..FRAME_SAMPLES));
            let start = self.input.len() - CONTEXT_SAMPLES;
            self.context.clear();
            self.context.extend_from_slice(&self.input[start..]);
            latest = self.score();
        }
        latest
    }

    /// Runs the frame sitting in `input`, advancing the state.
    ///
    /// Everything the session reads is moved out of `self` first: it needs
    /// `&mut self`, so nothing borrowed from `self` may be alive at the
    /// same time. The buffers are put straight back, so this still costs
    /// no allocation per frame.
    fn score(&mut self) -> Option<f32> {
        let input = std::mem::take(&mut self.input);
        let state = std::mem::take(&mut self.state);
        let outcome = run(&mut self.session, &input, &state);
        self.input = input;
        match outcome {
            Some((score, next)) => {
                self.state = next;
                Some(score)
            }
            None => {
                self.state = state;
                None
            }
        }
    }
}

/// One inference: the score for this frame, and the state after it.
fn run(session: &mut Session, input: &[f32], state: &[f32]) -> Option<(f32, Vec<f32>)> {
    let audio = TensorRef::from_array_view(([1_i64, input.len() as i64], input)).ok()?;
    let hidden = TensorRef::from_array_view((STATE_SHAPE, state)).ok()?;
    let rate = TensorRef::from_array_view(([1_i64], RATE.as_slice())).ok()?;
    let outputs = session
        .run(ort::inputs!["input" => audio, "state" => hidden, "sr" => rate])
        .ok()?;
    let (_, scores) = outputs["output"].try_extract_tensor::<f32>().ok()?;
    let score = *scores.first()?;
    let (_, next) = outputs["stateN"].try_extract_tensor::<f32>().ok()?;
    Some((score, next.to_vec()))
}

/// Sounds to test a voice detector with, shared with the segmenter's own
/// tests next door.
#[cfg(test)]
pub(crate) mod sounds {
    /// Real speech, spoken by the system voice.
    ///
    /// Synthetic voiced sounds — a pitch with harmonics, however shaped —
    /// score near zero: Silero is not fooled by them, which is the point
    /// of it. So the test speaks, and skips if it cannot.
    pub fn spoken(phrase: &str) -> Option<Vec<f32>> {
        let path = std::env::temp_dir().join("minion-test-silero-speech.wav");
        let spoke = std::process::Command::new("/usr/bin/say")
            .arg("-o")
            .arg(&path)
            .args(["--data-format=LEI16@16000", phrase])
            .status()
            .ok()?
            .success();
        if !spoke {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        samples_of(&bytes)
    }

    /// The samples out of a WAV, by walking its chunks.
    ///
    /// `say` writes JUNK and FLLR padding before the audio, so the usual
    /// "skip 44 bytes" reads the padding as if it were speech — and
    /// padding is silence, which scores like silence.
    fn samples_of(bytes: &[u8]) -> Option<Vec<f32>> {
        let mut at = 12; // past "RIFF", the size, and "WAVE"
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
            let body = at + 8;
            if id == b"data" {
                let end = (body + size).min(bytes.len());
                return Some(
                    bytes[body..end]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| i16::from_le_bytes(*pair) as f32 / 32768.0)
                        .collect(),
                );
            }
            at = body + size + (size & 1); // chunks are word-aligned
        }
        None
    }

    /// White noise at `level`: a fan, a tap running, a machine in the room.
    pub fn noise(level: f32, seconds: f32) -> Vec<f32> {
        let count = (16_000.0 * seconds) as usize;
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        (0..count)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed >> 40) as f32 / 4096.0 - 0.5) * level
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::sounds::{noise, spoken};
    use super::*;

    /// The model lives beside the speech model and is not in the
    /// repository, so these skip rather than fail when it is absent —
    /// same as the speaker tests.
    fn model_if_present() -> Option<Silero> {
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

    /// The highest score over a whole signal, from a clean start.
    fn best(model: &mut Silero, samples: &[f32]) -> f32 {
        model.reset();
        let mut top: f32 = 0.0;
        for block in samples.chunks(FRAME_SAMPLES) {
            if let Some(score) = model.push(block) {
                top = top.max(score);
            }
        }
        top
    }

    #[test]
    fn a_voice_scores_high_and_noise_does_not() {
        let Some(mut model) = model_if_present() else { return };
        let Some(speech) = spoken("Minion, abre Chrome. ¿Qué hora es?") else { return };
        let voice = best(&mut model, &speech);
        let hiss = best(&mut model, &noise(0.3, 2.0));
        println!("  speech: {voice:.3}, noise: {hiss:.3}");
        assert!(voice > 0.5, "speech must read as speech, got {voice:.3}");
        assert!(hiss < 0.3, "noise must not, got {hiss:.3}");
    }

    #[test]
    fn silence_is_not_speech() {
        let Some(mut model) = model_if_present() else { return };
        let quiet = best(&mut model, &vec![0.0; 16_000]);
        assert!(quiet < 0.3, "silence should not read as speech, got {quiet:.3}");
    }

    #[test]
    fn the_overlap_between_frames_is_what_makes_it_work() {
        // Feeding bare 512-sample frames, with no context in front of
        // them, is the mistake this model invites: it runs, it answers,
        // and it scores clear speech at 0.2 — a detector that rejects
        // everything. The property is worth a test of its own.
        let Some(mut model) = model_if_present() else { return };
        let Some(speech) = spoken("Minion, abre Chrome.") else { return };
        let with_context = best(&mut model, &speech);

        model.reset();
        let mut without: f32 = 0.0;
        for frame in speech.as_chunks::<FRAME_SAMPLES>().0 {
            let state = std::mem::take(&mut model.state);
            if let Some((score, next)) = run(&mut model.session, frame, &state) {
                model.state = next;
                without = without.max(score);
            } else {
                model.state = state;
            }
        }
        println!("  with overlap: {with_context:.3}, without: {without:.3}");
        assert!(
            with_context > without + 0.3,
            "the overlap must matter: {with_context:.3} vs {without:.3}"
        );
    }

    #[test]
    fn resetting_makes_it_forget() {
        // The state and the overlap summarise what came before, so the
        // same audio after a reset must score exactly as it did the first
        // time — otherwise an utterance is judged by the one before it.
        let Some(mut model) = model_if_present() else { return };
        let Some(speech) = spoken("Minion, abre Chrome.") else { return };
        let first = best(&mut model, &speech);
        let second = best(&mut model, &speech);
        assert!(
            (first - second).abs() < 1e-6,
            "a reset model must answer the same, got {first:.4} then {second:.4}"
        );
    }

    #[test]
    fn without_a_reset_the_state_carries() {
        // The other half of the same property: run twice without
        // resetting and the second pass sees a history the first did not.
        let Some(mut model) = model_if_present() else { return };
        let Some(speech) = spoken("Minion, abre Chrome.") else { return };
        model.reset();
        let mut first = Vec::new();
        for block in speech.chunks(FRAME_SAMPLES) {
            first.extend(model.push(block));
        }
        let mut second = Vec::new();
        for block in speech.chunks(FRAME_SAMPLES) {
            second.extend(model.push(block));
        }
        assert_eq!(first.len(), second.len());
        assert!(
            first.iter().zip(&second).any(|(a, b)| (a - b).abs() > 1e-6),
            "carrying the state must change something, or it is not being carried"
        );
    }

    #[test]
    fn partial_frames_are_kept_rather_than_judged_short() {
        let Some(mut model) = model_if_present() else { return };
        model.reset();
        assert!(model.push(&vec![0.1; FRAME_SAMPLES - 1]).is_none(), "not a frame yet");
        assert!(model.push(&[0.1]).is_some(), "the last sample completes it");
    }

    #[test]
    fn block_size_does_not_change_the_answer() {
        // The live path feeds 20 ms blocks of 320 samples, which never
        // line up with the model's 512. Whatever the caller's block size,
        // the frames — and so the scores — must be the same.
        let Some(mut model) = model_if_present() else { return };
        let Some(speech) = spoken("Minion, abre Chrome.") else { return };

        model.reset();
        let mut whole = Vec::new();
        for block in speech.chunks(FRAME_SAMPLES) {
            whole.extend(model.push(block));
        }

        model.reset();
        let mut piecewise = Vec::new();
        for block in speech.chunks(320) {
            piecewise.extend(model.push(block));
        }

        assert_eq!(whole.len(), piecewise.len(), "the same audio is the same frames");
        for (i, (a, b)) in whole.iter().zip(&piecewise).enumerate() {
            assert!((a - b).abs() < 1e-5, "frame {i}: {a} vs {b}");
        }
    }

    /// What one frame costs, so the price of running this on every block
    /// of speech is a measured number rather than a hope. Printed, not
    /// asserted: it is a property of the machine, not of the code.
    #[test]
    fn frame_cost_is_measured() {
        let Some(mut model) = model_if_present() else { return };
        let sound = noise(0.2, 40.0);
        let frames: Vec<&[f32; FRAME_SAMPLES]> =
            sound.as_chunks::<FRAME_SAMPLES>().0.iter().take(1_000).collect();
        model.reset();
        let started = std::time::Instant::now();
        for frame in &frames {
            model.push(frame.as_slice());
        }
        let each = started.elapsed() / frames.len() as u32;
        println!("  Silero: {each:?} per 32 ms frame ({} frames)", frames.len());
    }
}

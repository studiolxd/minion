//! Recognising whose voice it is.
//!
//! With an always-on microphone, everything said nearby reaches the
//! recogniser — a video, a colleague, a phone call on speaker. The wake
//! word stops those from running commands, but they still get transcribed.
//! This compares each utterance against a recording of your own voice and
//! discards the rest before anything else happens.
//!
//! The model is ECAPA-TDNN from wespeaker: 24 MB, 192 numbers per voice.
//! Two recordings of the same person score high against each other and low
//! against anyone else; the threshold decides where the line falls.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::TensorRef;

use crate::fbank::{self, FilterBank, NUM_BINS};

/// Utterances shorter than this carry too little voice to identify.
const MIN_SAMPLES: usize = 16_000 / 2; // half a second

/// A voice, as the model sees it: 192 numbers, unit length.
pub type Embedding = Vec<f32>;

pub struct Speaker {
    session: Session,
    bank: FilterBank,
}

impl Speaker {
    /// Loads the speaker model from the directory holding the speech model.
    pub fn load(model_dir: &str) -> Result<Self> {
        let path = std::path::Path::new(model_dir).join("speaker.onnx");
        let session = Session::builder()
            .map_err(|e| anyhow!("{e}"))?
            .commit_from_file(&path)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("loading the speaker model from {}", path.display()))?;
        Ok(Self { session, bank: FilterBank::new() })
    }

    /// Turns an utterance into an embedding.
    ///
    /// `None` when there is not enough audio to judge — better to admit
    /// that than to compare against a fraction of a word.
    pub fn embed(&mut self, samples: &[f32]) -> Option<Embedding> {
        if samples.len() < MIN_SAMPLES {
            return None;
        }
        let (mut feats, frames) = self.bank.compute(samples);
        if frames == 0 {
            return None;
        }
        fbank::normalise_mean(&mut feats, frames);

        let input = TensorRef::from_array_view((
            [1_i64, frames as i64, NUM_BINS as i64],
            feats.as_slice(),
        ))
        .ok()?;
        let outputs = self.session.run(ort::inputs!["feats" => input]).ok()?;
        let (_, values) = outputs["embs"].try_extract_tensor::<f32>().ok()?;

        Some(unit_length(values.to_vec()))
    }
}

/// Scales a vector to unit length, so comparing two is a plain dot product.
fn unit_length(mut values: Vec<f32>) -> Vec<f32> {
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        values.iter_mut().for_each(|v| *v /= norm);
    }
    values
}

/// How alike two voices are, from -1 to 1.
///
/// Both come out of [`Speaker::embed`] at unit length, so this is cosine
/// similarity without the division.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Where the enrolled voice is kept.
pub fn profile_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Oyente/voice.txt"))
}

/// Reads the enrolled voice, if there is one.
pub fn load_profile() -> Option<Embedding> {
    let contents = fs::read_to_string(profile_path()?).ok()?;
    let values: Vec<f32> = contents
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    (!values.is_empty()).then_some(values)
}

/// Stores an enrolled voice.
pub fn save_profile(embedding: &[f32]) -> Result<()> {
    let path = profile_path().ok_or_else(|| anyhow!("no home directory"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text: Vec<String> = embedding.iter().map(|v| format!("{v:.6}")).collect();
    fs::write(&path, text.join(" "))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Averages several recordings into one voice.
///
/// More than one sample matters: a single phrase carries the intonation of
/// that phrase as much as the voice, and averaging cancels that out.
pub fn average(samples: &[Embedding]) -> Option<Embedding> {
    let first = samples.first()?;
    let mut sum = vec![0.0; first.len()];
    for embedding in samples {
        for (total, value) in sum.iter_mut().zip(embedding) {
            *total += value;
        }
    }
    Some(unit_length(sum))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model lives beside the speech model and is not in the
    /// repository, so these skip rather than fail when it is absent.
    fn model_if_present() -> Option<Speaker> {
        let path = std::path::Path::new("model/speaker.onnx");
        path.exists().then(|| Speaker::load("model").ok()).flatten()
    }

    /// A crude voiced sound: a pitch with harmonics, which is closer to
    /// speech than a pure tone and enough to exercise the whole path.
    fn voiced(pitch: f32, seconds: f32) -> Vec<f32> {
        let count = (16_000.0 * seconds) as usize;
        (0..count)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                let mut sample = 0.0;
                for harmonic in 1..=6 {
                    let f = pitch * harmonic as f32;
                    sample += (2.0 * std::f32::consts::PI * f * t).sin() / harmonic as f32;
                }
                sample * 0.2
            })
            .collect()
    }

    #[test]
    fn the_model_produces_an_embedding() {
        let Some(mut model) = model_if_present() else {
            return;
        };
        let embedding = model.embed(&voiced(120.0, 2.0)).expect("two seconds is enough");
        assert_eq!(embedding.len(), 192, "ECAPA gives 192 numbers per voice");
        let norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "embeddings come out unit length");
    }

    #[test]
    fn the_same_sound_gives_the_same_embedding() {
        let Some(mut model) = model_if_present() else {
            return;
        };
        let sound = voiced(120.0, 2.0);
        let first = model.embed(&sound).unwrap();
        let second = model.embed(&sound).unwrap();
        assert!(
            similarity(&first, &second) > 0.999,
            "the same audio must give the same answer"
        );
    }

    #[test]
    fn different_sounds_give_different_embeddings() {
        let Some(mut model) = model_if_present() else {
            return;
        };
        // Two very different pitches: not two people, but far enough apart
        // that identical embeddings would mean the frontend is feeding the
        // model nothing at all.
        let low = model.embed(&voiced(90.0, 2.0)).unwrap();
        let high = model.embed(&voiced(260.0, 2.0)).unwrap();
        let alike = similarity(&low, &high);
        assert!(alike < 0.95, "these should not look like the same voice ({alike:.3})");
    }

    #[test]
    fn too_little_audio_is_refused() {
        let Some(mut model) = model_if_present() else {
            return;
        };
        assert!(model.embed(&voiced(120.0, 0.2)).is_none(), "a fifth of a second is not a voice");
    }

    #[test]
    fn a_voice_matches_itself() {
        let voice = unit_length(vec![1.0, 2.0, 3.0, 4.0]);
        assert!((similarity(&voice, &voice) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn unrelated_voices_score_low() {
        let one = unit_length(vec![1.0, 0.0, 0.0, 0.0]);
        let other = unit_length(vec![0.0, 1.0, 0.0, 0.0]);
        assert!(similarity(&one, &other).abs() < 1e-5);
    }

    #[test]
    fn averaging_keeps_unit_length() {
        let samples = vec![
            unit_length(vec![1.0, 0.2, 0.0]),
            unit_length(vec![0.9, 0.3, 0.1]),
        ];
        let mean = average(&samples).expect("two samples average");
        let norm: f32 = mean.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn an_average_sits_between_its_parts() {
        let a = unit_length(vec![1.0, 0.0]);
        let b = unit_length(vec![0.0, 1.0]);
        let mean = average(&[a.clone(), b.clone()]).unwrap();
        let to_a = similarity(&mean, &a);
        let to_b = similarity(&mean, &b);
        assert!((to_a - to_b).abs() < 1e-5, "should sit equally between them");
        assert!(to_a > 0.5 && to_a < 1.0);
    }
}

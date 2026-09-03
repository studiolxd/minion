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
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::TensorRef;

use crate::fbank::{self, FilterBank, NUM_BINS};

/// Below this there is no voice to identify at all.
///
/// A hard floor, not a quality bar: a third of a second is a syllable, and
/// anything shorter is a cough or a door. Clips under it are let through
/// unchecked rather than judged on nothing — a brief noise from the wrong
/// person is a smaller problem than refusing the right one.
const MIN_SAMPLES: usize = 16_000 * 3 / 10; // 0.3 seconds

/// How much audio the model wants before it says anything reliable.
///
/// Short clips used to be refused outright, because their embeddings
/// scored near zero against their own speaker — and "minion, Chrome" is
/// barely a second, so most real commands were never verified at all.
/// Repeating the speech until it reaches this length is wespeaker's own
/// trick and gives the statistics pooling enough frames to settle.
const TILE_TO_SAMPLES: usize = 16_000 * 3 / 2; // 1.5 seconds

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
        // Set up like the speech model in main.rs, and for the same
        // reasons. The defaults open a thread per physical core and keep a
        // growing arena, which for a 24 MB model asked one short question
        // at a time is all cost and no benefit: it was most of the process's
        // twenty-two threads and a slice of the idle memory floor.
        let session = Session::builder()
            .map_err(|e| anyhow!("{e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow!("{e}"))?
            .with_inter_threads(1)
            .map_err(|e| anyhow!("{e}"))?
            // Memory patterns pre-allocate for the longest utterance seen
            // and never give it back.
            .with_memory_pattern(false)
            .map_err(|e| anyhow!("{e}"))?
            // Prepacking keeps a second, faster-to-multiply copy of every
            // weight alongside the original.
            .with_config_entry("session.disable_prepacking", "1")
            .map_err(|e| anyhow!("{e}"))?
            // Read initializers straight from the mapped file rather than
            // copying them into the arena first.
            .with_config_entry("session.use_device_allocator_for_initializers", "1")
            .map_err(|e| anyhow!("{e}"))?
            .commit_from_file(&path)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("loading the speaker model from {}", path.display()))?;
        Ok(Self { session, bank: FilterBank::new() })
    }

    /// Turns an utterance into an embedding.
    ///
    /// `None` only when there is no speech worth the name: short clips are
    /// repeated up to [`TILE_TO_SAMPLES`] and judged, rather than waved
    /// through unverified.
    pub fn embed(&mut self, samples: &[f32]) -> Option<Embedding> {
        if samples.len() < MIN_SAMPLES {
            return None;
        }
        let tiled = tile_to(samples, TILE_TO_SAMPLES);
        let samples: &[f32] = tiled.as_deref().unwrap_or(samples);
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

/// Repeats `samples` until it is at least `wanted` long.
///
/// `None` when it is long enough already, so the caller can use the
/// original slice and copy nothing.
fn tile_to(samples: &[f32], wanted: usize) -> Option<Vec<f32>> {
    if samples.is_empty() || samples.len() >= wanted {
        return None;
    }
    let mut tiled = Vec::with_capacity(wanted + samples.len());
    while tiled.len() < wanted {
        tiled.extend_from_slice(samples);
    }
    Some(tiled)
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
    Some(PathBuf::from(home).join("Library/Application Support/Minion/voice.txt"))
}

/// Identifies the model a profile was made with.
///
/// Embeddings only mean anything against the model that produced them: the
/// same voice through a different model gives different numbers, and
/// comparing across the two would quietly stop recognising its owner. The
/// file's size is enough to tell one model from another here.
fn model_fingerprint(model_dir: &str) -> Option<u64> {
    let path = std::path::Path::new(model_dir).join("speaker.onnx");
    fs::metadata(path).ok().map(|m| m.len())
}

/// Reads the enrolled voice, if there is one made with this model.
///
/// A profile from another model is ignored rather than used, and says so:
/// silently failing to recognise someone is the worse outcome.
pub fn load_profile_for(model_dir: &str) -> Option<Embedding> {
    let contents = fs::read_to_string(profile_path()?).ok()?;

    let mut lines = contents.lines();
    let first = lines.next()?;
    let (stored_model, numbers) = match first.strip_prefix("# model ") {
        Some(fingerprint) => (fingerprint.trim().parse::<u64>().ok(), lines.next()?),
        // Written before profiles recorded their model: trust it.
        None => (None, first),
    };

    if let (Some(stored), Some(current)) = (stored_model, model_fingerprint(model_dir)) {
        if stored != current {
            crate::journal::write(
                "The stored voice was made with a different speech model and no \
                 longer applies. Train it again from Preferences.",
            );
            return None;
        }
    }

    let values: Vec<f32> = numbers
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    (!values.is_empty()).then_some(values)
}

/// Whether a voice has been enrolled at all, whatever model made it.
pub fn has_profile() -> bool {
    profile_path().is_some_and(|path| path.exists())
}

/// Stores an enrolled voice, noting which model made it.
///
/// Does nothing under test. The profile belongs to whoever is running
/// Minion, and a test run wrote one made of arithmetic — which would have
/// left the machine refusing to listen to its owner.
///
/// Kept outside the application bundle on purpose, so reinstalling does
/// not lose it.
pub fn save_profile_for(model_dir: &str, embedding: &[f32]) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let path = profile_path().ok_or_else(|| anyhow!("no home directory"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        // The voice profile identifies the owner; the directory holding it
        // should not be readable by other accounts on the machine.
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    let numbers: Vec<String> = embedding.iter().map(|v| format!("{v:.6}")).collect();
    let mut contents = String::new();
    if let Some(fingerprint) = model_fingerprint(model_dir) {
        contents.push_str(&format!("# model {fingerprint}\n"));
    }
    contents.push_str(&numbers.join(" "));
    fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    // A biometric embedding, even a lossy one, is worth keeping private.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("setting permissions on {}", path.display()))?;
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
    /// The speaker model, from the repo's `model/` directory or from the
    /// copy Minion downloaded for itself. Read only: nothing is written.
    /// Absent, and the test that asked is skipped rather than failed —
    /// the model is not in git.
    fn model_if_present() -> Option<Speaker> {
        let mut candidates = vec![std::path::PathBuf::from("model")];
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                std::path::PathBuf::from(home).join("Library/Application Support/Minion/model"),
            );
        }
        candidates
            .into_iter()
            .find(|dir| dir.join("speaker.onnx").exists())
            .and_then(|dir| Speaker::load(&dir.to_string_lossy()).ok())
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

    /// Reads a 16 kHz mono WAV written by `say`, for the tests below.
    fn read_wav(path: &str) -> Option<Vec<f32>> {
        let bytes = std::fs::read(path).ok()?;
        // Skip the header and read little-endian 16-bit samples. Enough
        // for files this test generates itself.
        let start = 44;
        Some(
            bytes[start..]
                .chunks_exact(2)
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
                .collect(),
        )
    }

    /// Two sentences from each of two system voices.
    ///
    /// Synthetic, but it is the property that matters: the same voice
    /// saying different words must look more alike than two voices saying
    /// the same words. Without this, an embedding that merely exists looks
    /// fine while failing to recognise anybody.
    #[test]
    fn tells_one_voice_from_another() {
        let Some(mut model) = model_if_present() else {
            return;
        };
        let files = [
            "/tmp/claude-501/voces/monica1.wav",
            "/tmp/claude-501/voces/monica2.wav",
            "/tmp/claude-501/voces/eddy1.wav",
            "/tmp/claude-501/voces/eddy2.wav",
        ];
        let mut voices = Vec::new();
        for file in files {
            let Some(samples) = read_wav(file) else {
                return; // not generated on this machine; nothing to check
            };
            let Some(embedding) = model.embed(&samples) else {
                return;
            };
            voices.push(embedding);
        }

        let same_a = similarity(&voices[0], &voices[1]); // Mónica vs Mónica
        let same_b = similarity(&voices[2], &voices[3]); // Eddy vs Eddy
        let different = similarity(&voices[0], &voices[2]); // Mónica vs Eddy

        println!("  same voice:  {same_a:.3} and {same_b:.3}");
        println!("  other voice: {different:.3}");

        assert!(
            same_a > different && same_b > different,
            "a voice must look more like itself ({same_a:.2}, {same_b:.2}) \
             than like someone else ({different:.2})"
        );
        assert!(
            same_a > 0.5 && same_b > 0.5,
            "the same voice should score well above the threshold, got \
             {same_a:.2} and {same_b:.2}"
        );
    }

    /// The same recording at 48 kHz, brought down the way the microphone
    /// path does it, must still look like the same voice.
    ///
    /// This is the property the live path depends on and the one that was
    /// missing: transcription tolerates a crude downsample, but speaker
    /// recognition does not — the detail that tells voices apart is
    /// exactly what aliasing destroys.
    #[test]
    fn downsampling_preserves_who_is_speaking() {
        let Some(mut model) = model_if_present() else {
            return;
        };
        let (Some(at_16k), Some(at_48k)) = (
            read_wav("/tmp/claude-501/voces/m16.wav"),
            read_wav("/tmp/claude-501/voces/m48.wav"),
        ) else {
            return;
        };

        let brought_down = crate::audio::to_16k_mono(&at_48k, 1, 48_000);
        let (Some(direct), Some(resampled)) =
            (model.embed(&at_16k), model.embed(&brought_down))
        else {
            return;
        };

        let alike = similarity(&direct, &resampled);
        println!("  same audio, resampled: {alike:.3}");
        assert!(
            alike > 0.8,
            "resampling should not change who is speaking, got {alike:.2}"
        );
    }

    /// Compares a stored profile against known voices, to see what it is.
    ///
    /// Only runs when a broken profile has been set aside for study; it is
    /// a diagnostic, not a property of the program.
    #[test]
    fn inspect_a_saved_profile() {
        let Some(home) = std::env::var("HOME").ok() else { return };
        let path = format!("{home}/Library/Application Support/Minion/voice.txt.roto");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return;
        };
        let stored: Vec<f32> = contents
            .lines()
            .last()
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect();
        if stored.len() != 192 {
            return;
        }
        let Some(mut model) = model_if_present() else { return };

        for name in ["monica1", "monica2", "eddy1", "eddy2"] {
            let Some(samples) = read_wav(&format!("/tmp/claude-501/voces/{name}.wav")) else {
                continue;
            };
            if let Some(embedding) = model.embed(&samples) {
                println!("  profile vs {name}: {:.3}", similarity(&stored, &embedding));
            }
        }
    }

    /// Quiet audio must still identify the speaker.
    ///
    /// A microphone across a desk is far quieter than a synthesised file,
    /// and if the embedding moved with loudness, enrolment and daily use
    /// would never agree.
    #[test]
    fn loudness_does_not_change_who_is_speaking() {
        let Some(mut model) = model_if_present() else { return };
        let Some(loud) = read_wav("/tmp/claude-501/voces/monica1.wav") else { return };

        for gain in [0.5, 0.1, 0.02] {
            let quiet: Vec<f32> = loud.iter().map(|s| s * gain).collect();
            let (Some(a), Some(b)) = (model.embed(&loud), model.embed(&quiet)) else {
                return;
            };
            let alike = similarity(&a, &b);
            println!("  at {:>4.0}% volume: {alike:.3}", gain * 100.0);
            assert!(
                alike > 0.8,
                "volume should not change identity, got {alike:.2} at {gain}"
            );
        }
    }

    /// Compares real recordings against each other and against the stored
    /// profile. A diagnostic, run only when recordings have been kept.
    #[test]
    fn inspect_real_recordings() {
        let Some(home) = std::env::var("HOME").ok() else { return };
        let directory = format!("{home}/Library/Application Support/Minion/recordings");
        let Ok(entries) = std::fs::read_dir(&directory) else { return };
        let Some(mut model) = model_if_present() else { return };

        // Only the recordings the log ties to a command: the rest are
        // whatever else was said nearby, and comparing those to each other
        // says nothing about whether one speaker is recognised.
        let mine: Vec<String> = std::fs::read_to_string(
            crate::journal::path().unwrap_or_default(),
        )
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            line.contains("  ran ") || line.contains("  asked ") || line.contains("  unknown ")
        })
        .filter_map(|line| line.split_whitespace().nth(1).map(|t| t.replace(':', "-")))
        .collect();

        let mut files: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().to_string_lossy().into_owned())
            .filter(|path| path.ends_with(".wav"))
            .filter(|path| {
                mine.iter().any(|stamp| path.contains(stamp.as_str()))
            })
            .collect();
        files.sort();
        if files.len() < 2 {
            return;
        }

        let mut voices = Vec::new();
        for file in &files {
            let Some(samples) = read_wav(file) else { continue };
            let seconds = samples.len() as f32 / 16_000.0;
            match model.embed(&samples) {
                Some(embedding) => voices.push((file.clone(), seconds, embedding)),
                None => println!("  {file}: too short ({seconds:.1}s)"),
            }
        }
        println!("  usable recordings: {}", voices.len());
        if voices.len() < 2 {
            return;
        }

        // How alike are two recordings of the same person?
        let mut total = 0.0;
        let mut pairs = 0;
        let mut worst: f32 = 1.0;
        for i in 0..voices.len() {
            for j in (i + 1)..voices.len() {
                let alike = similarity(&voices[i].2, &voices[j].2);
                total += alike;
                worst = worst.min(alike);
                pairs += 1;
            }
        }
        println!("  same speaker, average: {:.3}", total / pairs as f32);
        println!("  same speaker, worst:   {worst:.3}");

        // And against the profile that was rejecting them.
        let stored = format!("{home}/Library/Application Support/Minion/voice.txt.roto");
        if let Ok(contents) = std::fs::read_to_string(&stored) {
            let profile: Vec<f32> = contents
                .lines()
                .last()
                .unwrap_or("")
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if profile.len() == 192 {
                let scores: Vec<f32> = voices
                    .iter()
                    .map(|(_, _, embedding)| similarity(&profile, embedding))
                    .collect();
                let average = scores.iter().sum::<f32>() / scores.len() as f32;
                let lowest = scores.iter().copied().fold(f32::MAX, f32::min);
                println!("  against the saved profile: {average:.3} (worst {lowest:.3})");

                // And the recordings that were not commands: other voices,
                // the television, whatever was in the room. The gap
                // between these and the ones above is the margin the
                // threshold has to sit in.
                let Ok(all) = std::fs::read_dir(&directory) else { return };
                let mut others = Vec::new();
                for entry in all.filter_map(|e| e.ok()) {
                    let path = entry.path().to_string_lossy().into_owned();
                    if !path.ends_with(".wav") || files.contains(&path) {
                        continue;
                    }
                    if let Some(samples) = read_wav(&path) {
                        if let Some(embedding) = model.embed(&samples) {
                            others.push(similarity(&profile, &embedding));
                        }
                    }
                }
                if !others.is_empty() {
                    let average = others.iter().sum::<f32>() / others.len() as f32;
                    let highest = others.iter().copied().fold(f32::MIN, f32::max);
                    println!(
                        "  everything else:           {average:.3} (highest {highest:.3}, {} clips)",
                        others.len()
                    );
                }
            }
        }
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
    fn tiling_lets_a_short_clip_be_recognised() {
        // "minion, Chrome" is about a second, and a second used to be
        // refused: the commonest commands went through unverified. Repeated
        // up to a second and a half, a fragment must still look like the
        // voice it came from, or tiling has bought nothing.
        let Some(mut model) = model_if_present() else {
            return;
        };
        let whole = voiced(120.0, 2.0);
        let full = model.embed(&whole).expect("two seconds is plenty");
        let threshold = crate::config::Config::default().voice_threshold();
        for tenths in [4usize, 6, 8, 10] {
            let fragment = &whole[..16_000 * tenths / 10];
            let short = model.embed(fragment).expect("a fragment is now tiled, not refused");
            let alike = similarity(&full, &short);
            println!("  {}.{} s tiled vs whole: {alike:.3}", tenths / 10, tenths % 10);
            assert!(
                alike > threshold,
                "a {tenths}/10 s fragment must clear the threshold, got {alike:.2}"
            );
            // A whole command is about a second; that is the length the
            // change is for, and there the fragment should be all but
            // indistinguishable from the recording it came from.
            if tenths >= 8 {
                assert!(
                    alike > 0.8,
                    "a tiled {tenths}/10 s fragment should still be the same voice, \
                     got {alike:.2}"
                );
            }
        }
    }

    #[test]
    fn tiling_only_lengthens_what_is_short() {
        assert!(tile_to(&[1.0, 2.0], 2).is_none(), "long enough is left alone");
        assert!(tile_to(&[], 10).is_none(), "nothing to repeat");
        let tiled = tile_to(&[1.0, 2.0, 3.0], 7).expect("should be lengthened");
        assert!(tiled.len() >= 7);
        assert_eq!(&tiled[..6], &[1.0, 2.0, 3.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn a_profile_records_which_model_made_it() {
        // Embeddings only mean anything against their own model, so the
        // file has to say which one — otherwise a model change would stop
        // recognition working with no explanation.
        let written = "# model 24123456\n0.1 0.2 0.3";
        let mut lines = written.lines();
        let first = lines.next().unwrap();
        let fingerprint = first.strip_prefix("# model ").and_then(|f| f.trim().parse::<u64>().ok());
        assert_eq!(fingerprint, Some(24_123_456));
        assert_eq!(lines.next(), Some("0.1 0.2 0.3"));
    }

    #[test]
    fn a_profile_without_a_model_line_still_reads() {
        // Profiles written before this existed have no header, and should
        // keep working rather than being thrown away.
        let written = "0.1 0.2 0.3";
        let mut lines = written.lines();
        let first = lines.next().unwrap();
        assert!(first.strip_prefix("# model ").is_none());
        let values: Vec<f32> = first.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert_eq!(values.len(), 3);
    }

    #[test]
    fn the_threshold_sits_below_real_speech() {
        // Measured on this machine: the owner's own commands scored 0.38
        // at worst through a laptop microphone. A threshold above that
        // refuses its owner, which is what 0.45 did.
        const WORST_MEASURED: f32 = 0.38;
        let threshold = crate::config::Config::default().voice_threshold();
        assert!(
            threshold < WORST_MEASURED,
            "the threshold ({threshold}) must sit below the worst real \
             score ({WORST_MEASURED}) or it rejects its owner"
        );
    }

    #[test]
    fn short_clips_are_judged_rather_than_skipped() {
        // They used to be refused, which meant most real commands — none
        // of them much longer than a second — never had their speaker
        // checked at all. Now they are tiled and judged.
        let Some(mut model) = model_if_present() else { return };
        let brief = voiced(120.0, 1.0);
        assert!(
            model.embed(&brief).is_some(),
            "a second of audio should now be verified, not waved through"
        );
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

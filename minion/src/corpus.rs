//! `minion corpus`: measuring recognition against a fixed set of recordings.
//!
//! A corpus is a directory: WAVs the same shape [`crate::audio::save_recording`]
//! writes (16 kHz mono PCM), plus a `corpus.toml` saying what each one should
//! produce. `run` sends every file through the same pipeline the app uses —
//! speaker check, Parakeet, [`commands::decide`] — and prints a table plus
//! totals. `--save` freezes the current results as `baseline.toml`; a later
//! run without it compares against that file and fails if a file that used
//! to decide correctly no longer does, or reads noticeably worse.
//!
//! `bootstrap_from_log` builds a first `corpus.toml` from a session recorded
//! with `save_recordings = true`, by pairing each WAV with the log line
//! written for it.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use parakeet_rs::Transcriber;
use serde::{Deserialize, Serialize};

use crate::commands::{self, Decision};
use crate::speaker;

/// One row of `corpus.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// File name inside the corpus directory, e.g. `"001.wav"`.
    pub file: String,
    /// What Parakeet should transcribe, wake word included — or `"unknown"`
    /// when the point of the file is that nothing was heard at all.
    pub expected_transcript: String,
    /// What [`describe`] should say the decision was — `"Unrecognised"` for
    /// a phrase that should not match anything, `"Ignored"` for audio that
    /// should not even look like a command, `"Blank"` for audio Parakeet
    /// should fail to transcribe at all.
    pub expected_decision: String,
    /// Whether this is a recording of the owner's own voice. Recordings of
    /// someone else are how a threshold gets checked from both sides.
    #[serde(default = "default_owner_voice")]
    pub owner_voice: bool,
}

fn default_owner_voice() -> bool {
    true
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CorpusFile {
    #[serde(default)]
    entry: Vec<Entry>,
}

/// One row of `baseline.toml`: enough of a run's result to notice it
/// getting worse, not the whole table.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BaselineEntry {
    file: String,
    decision_correct: bool,
    wer: f32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BaselineFile {
    decision_accuracy: f32,
    mean_wer: f32,
    #[serde(default)]
    result: Vec<BaselineEntry>,
}

/// What running one corpus entry through the pipeline produced.
struct Outcome {
    file: String,
    wer: f32,
    expected_decision: String,
    got_decision: String,
    decision_correct: bool,
    owner_voice: bool,
    voice_score: Option<f32>,
}

/// Runs every file in `dir`'s `corpus.toml` through the recognition
/// pipeline and prints a table plus totals.
///
/// `save` freezes the result as `dir/baseline.toml`; without it, an
/// existing `baseline.toml` is compared against and a regression — a file
/// whose decision used to be right and now is not, or whose word error
/// rate got meaningfully worse — is an error, so this can be wired into
/// `cargo test` (see `tests/corpus.rs`).
pub fn run(dir: &Path, save: bool) -> Result<()> {
    let entries = load_corpus(dir)?;
    if entries.is_empty() {
        println!("{} has no [[entry]] to run.", dir.join("corpus.toml").display());
        return Ok(());
    }

    // The same path the app takes: `locate_model` downloads if it has to,
    // `configure` reads the same vocabulary and threshold `config.toml`
    // would give the running app.
    let model_path = super::locate_model(None)?;
    let mut model = super::load_model(&model_path)?;
    let config = crate::config::load();
    commands::configure(&config);
    let threshold = config.voice_threshold();
    let mut voice = speaker::load_profile_for(&model_path)
        .and_then(|profile| speaker::Speaker::load(&model_path).ok().map(|model| (model, profile)));
    if voice.is_none() {
        println!("No voice profile — every file is judged as if it were the owner's.");
    }

    let baseline = if save { None } else { load_baseline(dir) };

    println!(
        "{:<20} {:>6}  {:<8}  {:<28} {:<28}  got transcript",
        "file", "wer", "voice", "expected decision", "got decision"
    );
    let mut results = Vec::with_capacity(entries.len());
    for entry in &entries {
        let path = dir.join(&entry.file);
        let samples = match read_wav(&path) {
            Ok(samples) => samples,
            Err(e) => {
                println!("{:<20} ERROR reading it: {e}", entry.file);
                continue;
            }
        };

        // The speaker check first, over the raw samples, same as the app —
        // it never sees a transcript.
        let voice_score = voice
            .as_mut()
            .and_then(|(model, profile)| model.embed(&samples).map(|heard| speaker::similarity(&heard, profile)));

        let transcript = match model.transcribe_samples(samples, crate::audio::TARGET_HZ, 1, None) {
            Ok(result) => result.text.trim().to_string(),
            Err(e) => {
                println!("{:<20} ERROR transcribing it: {e}", entry.file);
                continue;
            }
        };
        // Empty is not the same as `Decision::Ignored`: Parakeet returned
        // nothing at all, so the app never gets as far as deciding — see
        // the `blank` log line in `main.rs`. `decide("")` would say
        // `Ignored`, which is a real outcome for real speech and would be
        // impossible to tell apart from this one.
        let got_decision =
            if transcript.is_empty() { "Blank".to_string() } else { describe(&commands::decide(&transcript).0) };
        let got_transcript = if transcript.is_empty() { "unknown".to_string() } else { transcript };

        let wer = word_error_rate(&entry.expected_transcript, &got_transcript);
        let decision_correct = got_decision == entry.expected_decision;

        let voice_field = match voice_score {
            Some(score) if entry.owner_voice && score < threshold => format!("{score:.2} !"),
            Some(score) => format!("{score:.2}"),
            None => "-".to_string(),
        };
        println!(
            "{:<20} {:>6.2}  {:<8}  {:<28} {:<28}  «{}»{}",
            entry.file,
            wer,
            voice_field,
            entry.expected_decision,
            got_decision,
            got_transcript,
            if decision_correct { "" } else { "  <-- mismatch" },
        );

        results.push(Outcome {
            file: entry.file.clone(),
            wer,
            expected_decision: entry.expected_decision.clone(),
            got_decision,
            decision_correct,
            owner_voice: entry.owner_voice,
            voice_score,
        });
    }

    if results.is_empty() {
        return Err(anyhow!("no entry could be read or transcribed"));
    }

    let n = results.len() as f32;
    let decision_accuracy = results.iter().filter(|r| r.decision_correct).count() as f32 / n;
    let mean_wer = results.iter().map(|r| r.wer).sum::<f32>() / n;
    let min_owner_score = results
        .iter()
        .filter(|r| r.owner_voice)
        .filter_map(|r| r.voice_score)
        .fold(f32::INFINITY, f32::min);

    println!();
    println!("decision_accuracy {decision_accuracy:.3}");
    println!("mean_wer {mean_wer:.3}");
    if min_owner_score.is_finite() {
        println!("min_owner_score {min_owner_score:.3}");
    }

    let mut regressions = Vec::new();
    if let Some(baseline) = &baseline {
        for result in &results {
            let Some(base) = baseline.iter().find(|b| b.file == result.file) else { continue };
            if base.decision_correct && !result.decision_correct {
                regressions.push(format!(
                    "{}: decision now «{}», was correct at «{}»",
                    result.file, result.got_decision, result.expected_decision
                ));
            } else if result.wer > base.wer + 0.05 {
                regressions.push(format!(
                    "{}: WER {:.2} is worse than the baseline's {:.2}",
                    result.file, result.wer, base.wer
                ));
            }
        }
    }
    for line in &regressions {
        println!("REGRESSION  {line}");
    }

    if save {
        save_baseline(dir, decision_accuracy, mean_wer, &results)?;
        println!("Wrote {}", dir.join("baseline.toml").display());
    } else if !regressions.is_empty() {
        return Err(anyhow!(
            "{} regression(s) against baseline.toml — rerun with --save once they are expected",
            regressions.len()
        ));
    }

    Ok(())
}

fn load_corpus(dir: &Path) -> Result<Vec<Entry>> {
    let path = dir.join("corpus.toml");
    let contents =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let file: CorpusFile =
        toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file.entry)
}

fn load_baseline(dir: &Path) -> Option<Vec<BaselineEntry>> {
    let contents = std::fs::read_to_string(dir.join("baseline.toml")).ok()?;
    let file: BaselineFile = toml::from_str(&contents).ok()?;
    Some(file.result)
}

fn save_baseline(dir: &Path, decision_accuracy: f32, mean_wer: f32, results: &[Outcome]) -> Result<()> {
    let file = BaselineFile {
        decision_accuracy,
        mean_wer,
        result: results
            .iter()
            .map(|r| BaselineEntry { file: r.file.clone(), decision_correct: r.decision_correct, wer: r.wer })
            .collect(),
    };
    let contents = toml::to_string_pretty(&file).context("formatting baseline.toml")?;
    std::fs::write(dir.join("baseline.toml"), contents).context("writing baseline.toml")
}

/// Describes a [`Decision`] the way `commands::perform`'s `Done.description`
/// does, without carrying out the action — a corpus run must never actually
/// open an app, quit one, or type into whatever has focus.
fn describe(decision: &Decision) -> String {
    match decision {
        Decision::Launch { name, .. } => format!("abrir {name}"),
        Decision::Quit { name, .. } => format!("cerrar {name}"),
        Decision::Browse { url, in_browser } => match in_browser {
            Some(bundle_id) => {
                let name = commands::vocabulary()
                    .apps
                    .iter()
                    .find(|app| app.bundle_id == *bundle_id)
                    .map_or(*bundle_id, |app| app.name);
                format!("abrir {url} en {name}")
            }
            None => format!("abrir {url}"),
        },
        Decision::RunHere(name) | Decision::Run(name) => (*name).to_string(),
        Decision::Type(text) => format!("escribir «{text}»"),
        Decision::SearchMusic(query) => format!("buscar «{query}» en Spotify"),
        Decision::Again(times) => format!("otra vez ×{times}"),
        Decision::Numbered { name, number, .. } => format!("{name} {number}"),
        Decision::StartDictation => "empezar a dictar".to_string(),
        Decision::DictateInto { destination, recipient } => match recipient {
            Some(recipient) => format!("dictar en {destination} a {recipient}"),
            None => format!("dictar en {destination}"),
        },
        Decision::StopDictation => "dejar de dictar".to_string(),
        Decision::UndoLast => "deshacer".to_string(),
        Decision::Answer(question) => format!("responder {question:?}"),
        Decision::Shortcut(name) => format!("atajo «{name}»"),
        Decision::Macro(macro_) => format!("macro «{}»", macro_.name),
        Decision::SearchFinder(query) => format!("buscar «{query}» en el Finder"),
        Decision::Unrecognised => "Unrecognised".to_string(),
        Decision::Ignored => "Ignored".to_string(),
    }
}

/// Word-level edit distance divided by the length of `expected`, the usual
/// definition of WER. Words, not characters: a transcript can be a whole
/// sentence off by one word, and per-character distance buries that under
/// however long the words themselves happen to be.
fn word_error_rate(expected: &str, got: &str) -> f32 {
    let expected_words: Vec<&str> = expected.split_whitespace().collect();
    let got_words: Vec<&str> = got.split_whitespace().collect();
    if expected_words.is_empty() {
        return if got_words.is_empty() { 0.0 } else { 1.0 };
    }
    word_edit_distance(&expected_words, &got_words) as f32 / expected_words.len() as f32
}

/// Levenshtein distance over words rather than characters.
fn word_edit_distance(a: &[&str], b: &[&str]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, word_a) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, word_b) in b.iter().enumerate() {
            let substitute = previous + usize::from(word_a != word_b);
            previous = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// Reads a 16-bit PCM WAV, the only shape [`crate::audio::save_recording`]
/// writes. Walks the chunks rather than assuming a 44-byte header, so a
/// file with an extra `LIST`/`fact` chunk in front of `data` still reads.
fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(anyhow!("{} is not a RIFF/WAVE file", path.display()));
    }
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let start = pos + 8;
        let end = start.saturating_add(size).min(bytes.len());
        if id == b"data" {
            return Ok(bytes[start..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair) as f32 / 32768.0)
                .collect());
        }
        // Chunks are word-aligned: an odd-sized chunk has one pad byte
        // after it that is not part of its declared size.
        pos = start + size + (size % 2);
    }
    Err(anyhow!("{} has a RIFF header but no data chunk", path.display()))
}

/// Builds a first `corpus.toml` from a directory of recordings, by pairing
/// each WAV with the log line written when it was saved.
///
/// `recordings_dir` is read-only — typically
/// `~/Library/Application Support/Minion/recordings`, turned on for a
/// session with `save_recordings = true` — and `output_dir` is where the
/// corpus is being built, inside the repo. Matched WAVs are copied there
/// alongside the generated `corpus.toml`; nothing is written under
/// `recordings_dir` or anywhere else under `~/Library`.
///
/// This is a starting point, not a finished corpus: every entry it writes
/// should be checked against the recording before it is trusted or
/// committed. In particular a `heard` line with no quoted phrase (the
/// ordinary case, since `log_ignored_speech` defaults to off) leaves
/// `expected_transcript` as `"unknown"` — fill in what was actually said if
/// it matters for that file.
pub fn bootstrap_from_log(recordings_dir: &Path, output_dir: &Path) -> Result<()> {
    let log_path =
        crate::journal::path().ok_or_else(|| anyhow!("no home directory, so no default log to read"))?;
    let log = std::fs::read_to_string(&log_path)
        .with_context(|| format!("reading {}", log_path.display()))?;
    let lines: Vec<&str> = log.lines().collect();

    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("creating {}", output_dir.display()))?;

    let mut entries = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(body) = after_timestamp(line) else { continue };
        let Some(saved_path) = body.strip_prefix("saved    ") else { continue };
        let Some(file_name) = Path::new(saved_path.trim()).file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let source = recordings_dir.join(file_name);
        if !source.is_file() {
            // The log outlives any one recordings folder — rotated away,
            // or from a different machine.
            continue;
        }
        let Some((transcript, decision, owner_voice)) = outcome_after(&lines, i + 1) else { continue };

        std::fs::copy(&source, output_dir.join(file_name))
            .with_context(|| format!("copying {}", source.display()))?;
        entries.push(Entry {
            file: file_name.to_string(),
            expected_transcript: transcript,
            expected_decision: decision,
            owner_voice,
        });
    }

    if entries.is_empty() {
        println!(
            "No recording in {} matched a log line in {}.",
            recordings_dir.display(),
            log_path.display()
        );
        return Ok(());
    }

    let toml_path = output_dir.join("corpus.toml");
    let contents =
        toml::to_string_pretty(&CorpusFile { entry: entries.clone() }).context("formatting corpus.toml")?;
    std::fs::write(&toml_path, contents).with_context(|| format!("writing {}", toml_path.display()))?;
    println!("Wrote {} entries to {}", entries.len(), toml_path.display());
    println!("Check expected_transcript and expected_decision by hand before committing.");
    Ok(())
}

/// Everything after a journal line's `"YYYY-MM-DD HH:MM:SS  "` stamp.
fn after_timestamp(line: &str) -> Option<&str> {
    line.get(19..)?.strip_prefix("  ")
}

/// The text between the first `«…»` pair, if there is one.
fn quoted(text: &str) -> Option<String> {
    let start = text.find('«')?;
    let after = &text[start + '«'.len_utf8()..];
    let end = after.find('»')?;
    Some(after[..end].to_string())
}

/// Reads forward from a `saved` line for the outcome it led to: what was
/// transcribed, what Minion decided to do about it (as [`describe`] would
/// print it), and whether the voice check — if there was one — passed.
fn outcome_after(lines: &[&str], from: usize) -> Option<(String, String, bool)> {
    let owner_voice = true;
    for line in lines.iter().skip(from).take(20) {
        let Some(body) = after_timestamp(line) else { continue };
        if body.starts_with("saved    ") {
            return None; // the next recording started; this one was never resolved
        }
        if let Some(rest) = body.strip_prefix("heard    ") {
            if rest.contains("in another voice") {
                // A rejected speaker check is the whole outcome: there is
                // no further "ran"/"unknown" line to wait for, since the
                // utterance is never transcribed at all.
                return Some(("unknown".to_string(), "Ignored".to_string(), false));
            }
            let transcript = quoted(rest).unwrap_or_else(|| "unknown".to_string());
            return Some((transcript, "Ignored".to_string(), owner_voice));
        }
        if body.starts_with("voice    matched") {
            continue; // informational; the decision line follows
        }
        if body.starts_with("blank    ") {
            return Some(("unknown".to_string(), "Blank".to_string(), owner_voice));
        }
        if let Some(rest) = body.strip_prefix("unknown  ") {
            let transcript = quoted(rest).unwrap_or_else(|| "unknown".to_string());
            return Some((transcript, "Unrecognised".to_string(), owner_voice));
        }
        if let Some(rest) = body.strip_prefix("ran      ") {
            let transcript = quoted(rest).unwrap_or_else(|| "unknown".to_string());
            let after_arrow = rest.split_once("->").map_or("", |(_, after)| after).trim();
            // Cut the trailing "[NN% · S.Ss audio · N ms]" and a "×N" repeat
            // marker, in either order, leaving the description alone.
            let described = after_arrow.split('[').next().unwrap_or(after_arrow).trim();
            let described = described.split(" ×").next().unwrap_or(described).trim();
            return Some((transcript, described.to_string(), owner_voice));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wer_is_zero_for_an_exact_match() {
        assert_eq!(word_error_rate("minion abre chrome", "minion abre chrome"), 0.0);
    }

    #[test]
    fn wer_counts_one_substitution_out_of_three_words() {
        let wer = word_error_rate("minion abre chrome", "minion abre safari");
        assert!((wer - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn wer_treats_a_missing_transcript_as_entirely_wrong() {
        assert_eq!(word_error_rate("minion abre chrome", ""), 1.0);
    }

    #[test]
    fn quoted_reads_the_text_between_guillemets() {
        assert_eq!(quoted("«Minion Chrome.»  ->  abrir Chrome"), Some("Minion Chrome.".to_string()));
        assert_eq!(quoted("no quotes here"), None);
    }

    #[test]
    fn after_timestamp_strips_the_stamp_journal_write_adds() {
        assert_eq!(after_timestamp("2026-09-03 10:15:00  ran      «hola»"), Some("ran      «hola»"));
        assert_eq!(after_timestamp("too short"), None);
    }

    #[test]
    fn outcome_after_reads_a_ran_line() {
        let lines = vec![
            "2026-09-03 10:15:00  saved    /x/001.wav",
            "2026-09-03 10:15:01  voice    matched at 0.58",
            "2026-09-03 10:15:01  ran      «Minion Chrome.»  ->  abrir Chrome  [100% · 1.6s audio · 142 ms]",
        ];
        let (transcript, decision, owner) = outcome_after(&lines, 1).unwrap();
        assert_eq!(transcript, "Minion Chrome.");
        assert_eq!(decision, "abrir Chrome");
        assert!(owner);
    }

    #[test]
    fn outcome_after_reads_an_unknown_line() {
        let lines = vec![
            "2026-09-03 10:15:00  saved    /x/002.wav",
            "2026-09-03 10:15:01  unknown  «Minium so fuddy.»  ->  not understood",
        ];
        let (transcript, decision, owner) = outcome_after(&lines, 1).unwrap();
        assert_eq!(transcript, "Minium so fuddy.");
        assert_eq!(decision, "Unrecognised");
        assert!(owner);
    }

    #[test]
    fn outcome_after_notices_another_voice() {
        let lines = vec![
            "2026-09-03 10:15:00  saved    /x/003.wav",
            "2026-09-03 10:15:01  heard    4.8s in another voice (-0.01)",
        ];
        let (_, decision, owner) = outcome_after(&lines, 1).unwrap();
        assert_eq!(decision, "Ignored");
        assert!(!owner);
    }

    #[test]
    fn outcome_after_gives_up_once_the_next_recording_starts() {
        let lines = vec![
            "2026-09-03 10:15:00  saved    /x/004.wav",
            "2026-09-03 10:15:05  saved    /x/005.wav",
        ];
        assert!(outcome_after(&lines, 1).is_none());
    }

    #[test]
    fn read_wav_round_trips_what_save_recording_writes() {
        let dir = std::env::temp_dir().join(format!("minion-corpus-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let samples = [0.0_f32, 0.5, -0.5, 1.0, -1.0];
        // Same layout as `audio::save_recording`, built by hand so this
        // test does not depend on `~/Library` even under `cfg!(test)`.
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
            .collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&32_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&data);
        let path = dir.join("test.wav");
        std::fs::write(&path, &bytes).unwrap();

        let read = read_wav(&path).unwrap();
        assert_eq!(read.len(), samples.len());
        for (a, b) in read.iter().zip(samples.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}

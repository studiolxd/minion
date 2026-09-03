//! Turns the log's failures into vocabulary.
//!
//! Every phrase Minion did not understand is already written down. This
//! reads them back, works out what each was probably meant to be, and can
//! add it to the configuration as an alias — so the same mistake is only
//! made once.
//!
//! Run with `minion learn`, or `minion learn --apply` to write.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;

use crate::{commands, config, journal};

/// How close a failed phrase must be to a command before it is offered as
/// an alias.
///
/// Below this the resemblance is coincidental: "ir a google.com" scores 40%
/// against "adelante" because both mention going somewhere, and teaching
/// that would make navigation fire on web addresses.
const SUGGEST_ABOVE: f32 = 0.55;

/// A phrase that failed, with what it was probably meant to be.
pub struct Candidate {
    pub phrase: String,
    pub times: usize,
    pub command: String,
    pub score: f32,
}

/// What the log has to teach, split by whether it can be acted on.
pub struct Lesson {
    /// Close enough to an existing command to be added as an alias.
    pub teachable: Vec<Candidate>,
    /// Nothing resembles these; they need a command that does not exist.
    pub missing: Vec<Candidate>,
}

impl Lesson {
    pub fn is_empty(&self) -> bool {
        self.teachable.is_empty() && self.missing.is_empty()
    }

    /// A plain-text summary, for the terminal or a dialog.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "Nada que aprender: el registro no tiene frases sin entender.".into();
        }
        let mut out = String::new();
        if !self.teachable.is_empty() {
            out.push_str("Frases que se parecen a una orden que ya existe:\n\n");
            for candidate in &self.teachable {
                let repeats = if candidate.times > 1 {
                    format!("  ×{}", candidate.times)
                } else {
                    String::new()
                };
                let _ = writeln!(
                    out,
                    "  «{}»{}\n      → {} ({:.0}%)",
                    candidate.phrase, repeats, candidate.command, candidate.score * 100.0
                );
            }
        }
        if !self.missing.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Ninguna orden se parece a estas — harían falta nuevas:\n\n");
            for candidate in &self.missing {
                let _ = writeln!(out, "  «{}»", candidate.phrase);
            }
        }
        out
    }
}

/// Reads the log and works out what it has to teach.
pub fn analyse(config: &config::Config) -> Lesson {
    let (teachable, missing) = candidates(config)
        .into_iter()
        .partition(|c| c.score >= SUGGEST_ABOVE);
    Lesson { teachable, missing }
}

/// Writes the teachable phrases into the configuration as aliases.
///
/// Returns how many were added, or an error message.
pub fn apply(lesson: &Lesson) -> Result<usize, String> {
    if lesson.teachable.is_empty() {
        return Ok(0);
    }
    let path = config::path().ok_or("No se encuentra el archivo de configuración.")?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut addition = String::from("\n# Aprendido del registro con `minion learn`.\n");
    for candidate in &lesson.teachable {
        let phrase = without_wake_word(&candidate.phrase);
        // Quoted by the config writer, not by hand: a phrase learned from
        // the log is whatever the recogniser wrote, quotation marks and
        // backslashes included, and one of those in a hand-written
        // `"{...}"` leaves a file that no longer parses.
        let _ = write!(
            addition,
            "\n[[aliases]]\ncommand = {}\nphrase = {}\n",
            config::toml_string(&candidate.command),
            config::toml_string(&phrase)
        );
    }

    let existing = fs::read_to_string(&path).unwrap_or_default();
    fs::write(&path, existing + &addition)
        .map(|()| lesson.teachable.len())
        .map_err(|e| format!("No se pudo escribir {}: {e}", path.display()))
}

/// Reads the phrases the log recorded as not understood.
fn failed_phrases() -> Vec<(String, usize)> {
    let Some(path) = journal::path() else {
        return Vec::new();
    };
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    failed_phrases_in(&contents)
}

/// The same, over the text of a log: the parsing, with no file behind it.
///
/// The format is written in `main.rs` — `unknown  «…»` — and read here, so
/// it is worth a test that does not depend on this machine's log.
fn failed_phrases_in(contents: &str) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in contents.lines() {
        let Some(rest) = line.split("unknown  ").nth(1) else {
            continue;
        };
        // The phrase is wrapped in angle quotes: «…»
        let Some(phrase) = rest.split('«').nth(1).and_then(|p| p.split('»').next()) else {
            continue;
        };
        let phrase = phrase.trim();
        if !phrase.is_empty() {
            *counts.entry(phrase.to_string()).or_default() += 1;
        }
    }

    let mut phrases: Vec<(String, usize)> = counts.into_iter().collect();
    // Most frequent first: those are the ones worth fixing.
    phrases.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    phrases
}

/// Pairs each failed phrase with its closest command.
///
/// Skips anything the current vocabulary already handles: the log holds
/// every failure ever recorded, including the ones since fixed, and
/// offering to teach a phrase that already works is just noise.
fn candidates(config: &config::Config) -> Vec<Candidate> {
    let known: Vec<String> = config
        .aliases
        .iter()
        .map(|a| crate::text::normalise(&a.phrase))
        .collect();

    failed_phrases()
        .into_iter()
        .filter(|(phrase, _)| {
            // Anything but "not understood" means the vocabulary has since
            // learned to handle it — including phrases now deliberately
            // ignored, such as the wake word on its own.
            matches!(
                commands::decide(phrase).0,
                commands::Decision::Unrecognised
            )
        })
        .filter_map(|(phrase, times)| {
            let normalised = crate::text::normalise(&phrase);
            if known.contains(&normalised) {
                return None; // already taught
            }
            let (command, score) = commands::closest_command(&phrase)?;
            Some(Candidate {
                phrase,
                times,
                command: command.to_string(),
                score,
            })
        })
        .collect()
}

/// Strips the wake word so the stored alias is just the command phrasing.
///
/// The real stripper, not a copy of it: it also rejoins a wake word the
/// recogniser split in two, and a copy that did not left aliases with a
/// stray "on" at the front of every "mini on …" phrase.
fn without_wake_word(phrase: &str) -> String {
    let normalised = crate::text::normalise(phrase);
    commands::strip_wake_word(&normalised)
        .unwrap_or(&normalised)
        .to_string()
}

/// Prints the report, and optionally writes the accepted aliases.
pub fn run(config: &config::Config, apply_now: bool) {
    let lesson = analyse(config);
    println!("\n{}", lesson.summary());

    if !apply_now {
        if !lesson.teachable.is_empty() {
            println!("\nRun `minion learn --apply` to add the first group as aliases.");
        }
        return;
    }
    match apply(&lesson) {
        Ok(0) => println!("Nothing close enough to add."),
        Ok(n) => println!("\nAdded {n} alias(es). Restart Minion for them to take effect."),
        Err(e) => eprintln!("\n{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_failures_out_of_a_log() {
        let log = "\
12:00:01  ran      «Minion Chrome.»  ->  abrir Chrome  [100% · 1.6s audio · 142 ms]
12:00:04  unknown  «Minion haz un pino.»  ->  not understood
12:00:09  unknown  «Minion haz un pino.»  ->  not understood
12:00:12  unknown  «Minion ponme un café.»  ->  not understood
12:00:15  heard    1.5s of speech, not addressed to me
12:00:18  unknown  «»  ->  not understood
";
        assert_eq!(
            failed_phrases_in(log),
            vec![
                ("Minion haz un pino.".to_string(), 2),
                ("Minion ponme un café.".to_string(), 1),
            ]
        );
    }

    #[test]
    fn an_alias_keeps_no_half_of_the_wake_word() {
        // "Mini on" is the wake word split in two; both halves must go,
        // or the alias learned from it can never match.
        assert_eq!(without_wake_word("Mini on guarda esto"), "guarda esto");
        assert_eq!(without_wake_word("Minion, guarda esto."), "guarda esto");
        // Nothing to strip: the phrase is kept whole.
        assert_eq!(without_wake_word("guarda esto"), "guarda esto");
    }
}

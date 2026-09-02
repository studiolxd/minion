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
        let _ = write!(
            addition,
            "\n[[aliases]]\ncommand = \"{}\"\nphrase = \"{}\"\n",
            candidate.command, phrase
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
fn without_wake_word(phrase: &str) -> String {
    let normalised = crate::text::normalise(phrase);
    let mut words = normalised.split_whitespace();
    match words.next() {
        Some(first) if commands::is_wake_word(first) => {
            words.collect::<Vec<_>>().join(" ")
        }
        _ => normalised,
    }
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

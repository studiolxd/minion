//! Turns the log's failures into vocabulary.
//!
//! Every phrase Oyente did not understand is already written down. This
//! reads them back, works out what each was probably meant to be, and can
//! add it to the configuration as an alias — so the same mistake is only
//! made once.
//!
//! Run with `oyente aprender`, or `oyente aprender --aplicar` to write.

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
struct Candidate {
    phrase: String,
    times: usize,
    command: String,
    score: f32,
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
pub fn run(config: &config::Config, apply: bool) {
    let all = candidates(config);
    if all.is_empty() {
        println!("Nothing to learn: no unrecognised phrases in the log.");
        return;
    }

    let (worth_teaching, unclear): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|c| c.score >= SUGGEST_ABOVE);

    if !worth_teaching.is_empty() {
        println!("\nPhrases that look like an existing command:\n");
        for candidate in &worth_teaching {
            println!(
                "  «{}»{}\n      → {} ({:.0}% similar)",
                candidate.phrase,
                if candidate.times > 1 {
                    format!("  ×{}", candidate.times)
                } else {
                    String::new()
                },
                candidate.command,
                candidate.score * 100.0
            );
        }
    }

    if !unclear.is_empty() {
        println!("\nNo command resembles these — they may need a new one:\n");
        for candidate in &unclear {
            println!("  «{}»", candidate.phrase);
        }
    }

    if !apply {
        println!(
            "\nRun `oyente aprender --aplicar` to add the first group as aliases."
        );
        return;
    }

    if worth_teaching.is_empty() {
        println!("\nNothing close enough to add.");
        return;
    }

    let Some(path) = config::path() else {
        eprintln!("Cannot locate the configuration file.");
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut addition = String::from("\n# Learned from the log by `oyente aprender`.\n");
    for candidate in &worth_teaching {
        let phrase = without_wake_word(&candidate.phrase);
        let _ = write!(
            addition,
            "\n[[aliases]]\ncommand = \"{}\"\nphrase = \"{}\"\n",
            candidate.command, phrase
        );
    }

    let existing = fs::read_to_string(&path).unwrap_or_default();
    match fs::write(&path, existing + &addition) {
        Ok(()) => println!(
            "\nAdded {} alias(es) to {}.\nRestart Oyente for them to take effect.",
            worth_teaching.len(),
            path.display()
        ),
        Err(e) => eprintln!("\nCould not write {}: {e}", path.display()),
    }
}

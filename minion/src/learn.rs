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
    let mut addition = String::from("\n# Aprendido del registro con `minion learn`.\n");
    for candidate in &lesson.teachable {
        addition.push_str(&alias_entry(&without_wake_word(&candidate.phrase), &candidate.command));
    }
    append(&addition).map(|()| lesson.teachable.len())
}

/// Teaches one phrase, as `minion learn` would teach a whole log's worth.
///
/// The wake word is stripped, since that is not part of what an alias
/// matches, and the phrase reaches the file through the config writer's
/// quoting — a phrase learned by ear is whatever the recogniser wrote,
/// quotation marks and backslashes included.
pub fn teach(phrase: &str, command: &str) -> Result<(), String> {
    append(&format!(
        "\n# Aprendido al preguntar en voz alta.\n{}",
        alias_entry(&without_wake_word(phrase), command)
    ))
}

/// Teaches one more way the recogniser writes an application's name.
///
/// An application is not reached through `[[aliases]]` — those point at
/// commands — so what is written is the whole `[[apps]]` entry, its
/// existing aliases and the new one. That entry then replaces the built-in
/// one by name, which is the price of this route: later versions of Minion
/// can add aliases to the shipped table without them being seen here.
fn teach_app(app: &str, spoken: &str) -> Result<(), String> {
    let known = commands::vocabulary()
        .apps
        .iter()
        .find(|a| a.name == app)
        .ok_or_else(|| format!("No hay ninguna aplicación llamada «{app}»."))?;
    let spoken = crate::text::normalise(spoken);
    if spoken.is_empty() || known.aliases.contains(&spoken.as_str()) {
        return Ok(());
    }
    let mut aliases: Vec<&str> = known.aliases.to_vec();
    aliases.push(&spoken);
    append(&format!(
        "\n# Aprendido al preguntar en voz alta.\n{}",
        app_entry(known.name, known.bundle_id, &aliases)
    ))
}

/// One `[[apps]]` entry, quoted so that whatever was heard parses.
fn app_entry(name: &str, bundle_id: &str, aliases: &[&str]) -> String {
    let aliases: Vec<String> = aliases.iter().map(|a| config::toml_string(a)).collect();
    format!(
        "\n[[apps]]\nname = {}\nbundle_id = {}\naliases = [{}]\n",
        config::toml_string(name),
        config::toml_string(bundle_id),
        aliases.join(", ")
    )
}

/// One `[[aliases]]` entry, quoted so that whatever was said parses.
fn alias_entry(phrase: &str, command: &str) -> String {
    format!(
        "\n[[aliases]]\ncommand = {}\nphrase = {}\n",
        config::toml_string(command),
        config::toml_string(phrase)
    )
}

/// Appends to `config.toml`, but only if the result still parses.
///
/// The file is the one thing Minion cannot start without — a stray
/// character in it and every setting reverts — so what would be written is
/// read back as a `Config` first, and a file that would not survive the
/// round trip is left exactly as it was.
fn append(addition: &str) -> Result<(), String> {
    // Tests must never reach the real configuration, neither to read it
    // nor to write it: `appended` is the whole of what they need, and it
    // is pure.
    if cfg!(test) {
        return Ok(());
    }
    let path = config::path().ok_or("No se encuentra el archivo de configuración.")?;
    let contents = appended(&fs::read_to_string(&path).unwrap_or_default(), addition)?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, contents)
        .map_err(|e| format!("No se pudo escribir {}: {e}", path.display()))
}

/// The configuration with the addition on the end, if it still parses.
fn appended(existing: &str, addition: &str) -> Result<String, String> {
    let candidate = existing.to_string() + addition;
    toml::from_str::<config::Config>(&candidate)
        .map(|_| candidate)
        .map_err(|e| format!("Lo aprendido dejaría la configuración sin poder leerse: {e}"))
}

/// How close a guess has to be before Minion asks about it out loud.
///
/// Lower than [`SUGGEST_ABOVE`]: a question costs nothing but a moment and
/// is answered by the person who knows, while an alias written into the
/// configuration from a report nobody read has to be right on its own.
pub const ASK_ABOVE: f32 = 0.6;

/// What a phrase was probably meant to be: what to do about it, how to say
/// so, and what to remember if the guess turns out to be right.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub decision: commands::Decision,
    /// As the log and the question both word it: "abrir Safari".
    pub description: String,
    pub score: f32,
    lesson: Remember,
}

/// What accepting a suggestion would write down.
#[derive(Clone, Debug)]
enum Remember {
    /// Another way of saying a command in the table.
    Phrase { command: String, phrase: String },
    /// Another way the recogniser writes an application's name.
    AppName { app: String, spoken: String },
}

/// What a phrase that was not understood most likely meant.
///
/// Both halves of the vocabulary are asked — the commands and the
/// applications — and the better answer wins. `None` means nothing came
/// close enough to be worth putting to anyone.
pub fn suggest(phrase: &str) -> Option<Suggestion> {
    let command = commands::closest_command(phrase).map(|(name, score)| Suggestion {
        decision: commands::Decision::Run(name),
        description: name.to_string(),
        score,
        lesson: Remember::Phrase {
            command: name.to_string(),
            phrase: phrase.to_string(),
        },
    });
    let app = commands::closest_app(phrase).map(|guess| Suggestion {
        description: match guess.decision {
            commands::Decision::Quit { .. } => format!("cerrar {}", guess.app),
            _ => format!("abrir {}", guess.app),
        },
        decision: guess.decision,
        score: guess.score,
        lesson: Remember::AppName {
            app: guess.app.to_string(),
            spoken: guess.spoken,
        },
    });

    [command, app]
        .into_iter()
        .flatten()
        .filter(|suggestion| suggestion.score >= ASK_ABOVE)
        .max_by(|a, b| a.score.total_cmp(&b.score))
}

impl Suggestion {
    /// Remembers the guess, now that it has been confirmed out loud.
    pub fn learn(&self) -> Result<(), String> {
        match &self.lesson {
            Remember::Phrase { command, phrase } => teach(phrase, command),
            Remember::AppName { app, spoken } => teach_app(app, spoken),
        }
    }
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
            // Compared without the wake word, which is how the alias was
            // stored: with it, a phrase already taught was offered again
            // on every run and appended to the config once more each time.
            let normalised = without_wake_word(&phrase);
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
    fn a_phrase_already_taught_is_compared_without_the_wake_word() {
        // The alias is stored without «minion»; the log line has it. The
        // two must still be recognised as the same phrase, or the lesson
        // is offered again on every run.
        let known = [without_wake_word("Minion. Deshacer.")];
        assert!(known.contains(&without_wake_word("Minion deshacer")));
        assert!(!known.contains(&without_wake_word("Minion rehacer")));
    }

    #[test]
    fn what_is_learned_is_quoted_before_it_is_written() {
        // Whatever the recogniser wrote, quotation marks included: the
        // entry has to survive being read back.
        let entry = alias_entry(r#"di "hola" \ adios"#, "guardar");
        let written = appended("speak = true\n", &entry).expect("should still parse");
        let config: config::Config = toml::from_str(&written).expect("should parse");
        assert_eq!(config.aliases.len(), 1);
        assert_eq!(config.aliases[0].phrase, r#"di "hola" \ adios"#);
        assert_eq!(config.aliases[0].command, "guardar");
    }

    #[test]
    fn learning_a_name_writes_the_whole_application_back() {
        // An application is not reached through `[[aliases]]`, so what is
        // written is the entry itself: the aliases it had, and the new one.
        let entry = app_entry("Safari", "com.apple.Safari", &["safari", "fari"]);
        let written = appended("", &entry).expect("should still parse");
        let config: config::Config = toml::from_str(&written).expect("should parse");
        let app = &config.extra_apps()[0];
        assert_eq!(app.name, "Safari");
        assert_eq!(app.bundle_id, "com.apple.Safari");
        assert_eq!(app.aliases, ["safari", "fari"]);
    }

    #[test]
    fn a_configuration_that_would_stop_parsing_is_not_written() {
        // The one file Minion cannot start without. Anything that would
        // leave it unreadable is refused, and the file stays as it was.
        let broken = appended("speak = true\n", "\n[[aliases]]\ncommand = \"guardar\"\n");
        assert!(broken.is_err(), "an alias with no phrase must not be written");
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

//! Apple Shortcuts, run by name.
//!
//! `shortcuts run "<name>"` is the whole integration: Minion never inspects
//! or edits a shortcut, only launches the ones already installed, the same
//! way the Shortcuts app itself would. What lives here is finding the
//! installed shortcut a spoken name most likely meant, and starting it
//! without waiting for it to finish.

use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use crate::text::{edits_between, normalise};

/// Installed shortcuts, as `shortcuts list` prints them one per line.
///
/// Read once at startup and kept rather than shelled out to on every
/// utterance — `shortcuts list` takes long enough to notice, and a
/// microphone that is always on cannot afford that on the common case of
/// nothing having changed. [`find`] refreshes it once when a spoken name
/// matches nothing, in case a shortcut was added since.
static INSTALLED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn cache() -> &'static Mutex<Vec<String>> {
    INSTALLED.get_or_init(|| Mutex::new(list_installed()))
}

/// Asks macOS for the installed shortcuts.
///
/// Empty rather than an error when none are installed, or the binary is
/// missing (older macOS, or Shortcuts never opened once to seed its
/// database) — there is simply nothing to name yet, which is not a reason
/// for Minion to fail to start.
fn list_installed() -> Vec<String> {
    Command::new("/usr/bin/shortcuts")
        .arg("list")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Re-reads the installed list from macOS.
///
/// Called once, lazily, the first time a shortcut is asked for — which in
/// practice is at startup, since [`crate::commands::configure`] calls it —
/// and again from [`find`] whenever a spoken name matches nothing, so a
/// shortcut installed after Minion started is still reachable without a
/// restart.
pub fn refresh() {
    let fresh = list_installed();
    *cache().lock().unwrap() = fresh;
}

/// The word that introduces a shortcut's name.
///
/// Covers all three phrasings this is meant to catch — "ejecuta el atajo
/// X", "lanza el atajo X", "atajo X" — since each has "atajo" immediately
/// before the name, regardless of what came before it.
///
/// Plural is deliberately not matched: "¿qué atajos tengo?" is a question
/// about what is installed, not a request to run one, and belongs to
/// whichever command answers it.
const MARKER: &str = "atajo ";

/// Extracts a shortcut's name from a sentence with the wake word already
/// stripped. `None` if the sentence does not mention one.
///
/// Takes the rest of the sentence as the name outright, tolerance included
/// — the name is matched against the installed list afterwards by
/// [`find`], which is where recogniser slips are actually absorbed.
pub fn requested_name(rest: &str) -> Option<&str> {
    let at = rest.find(MARKER)?;
    let name = rest[at + MARKER.len()..].trim();
    (!name.is_empty()).then_some(name)
}

/// Whether one word is close enough to another to count as the same word,
/// said with an accent dropped or a syllable misheard.
///
/// A single edit, same as an application alias's tolerant path
/// (`commands::near_alias`) — but without that one's two-letter,
/// five-character floor: a shortcut's name is usually more than one word,
/// so there is no single short word here that tolerance could turn into a
/// different one by accident.
fn word_matches(spoken: &str, installed: &str) -> bool {
    spoken == installed || edits_between(spoken, installed) <= 1
}

/// The installed name closest to what was spoken, if any is close enough.
///
/// Matched whole name against whole name, word for word, in order — not a
/// substring search, for the same reason `commands::find_app` avoids one:
/// a shortcut called "Modo" would otherwise be found inside "Modo Trabajo
/// Extra". Equal word count is required, so only a name spoken with about
/// the right number of words is even considered; among those, the one
/// needing the fewest edits overall wins.
fn best_match<'a>(spoken: &str, installed: &'a [String]) -> Option<&'a str> {
    let spoken_words: Vec<String> = spoken.split_whitespace().map(normalise).collect();
    if spoken_words.is_empty() {
        return None;
    }
    installed
        .iter()
        .filter(|name| {
            let words: Vec<String> = name.split_whitespace().map(normalise).collect();
            words.len() == spoken_words.len()
                && words.iter().zip(&spoken_words).all(|(a, b)| word_matches(b, a))
        })
        .min_by_key(|name| edits_between(&normalise(name), &spoken_words.join(" ")))
        .map(String::as_str)
}

/// Finds the shortcut named in the sentence.
///
/// Refreshes the installed list once and tries again if nothing matched —
/// a shortcut added after Minion started should not need a restart to be
/// reachable.
pub fn find(spoken: &str) -> Option<String> {
    {
        let installed = cache().lock().unwrap();
        if let Some(name) = best_match(spoken, &installed) {
            return Some(name.to_string());
        }
    }
    refresh();
    let installed = cache().lock().unwrap();
    best_match(spoken, &installed).map(str::to_string)
}

/// Runs a shortcut by name, without waiting for it to finish.
///
/// A shortcut can take anywhere from instant to tens of seconds depending
/// on what it does, and Minion has one microphone thread — waiting on it
/// would mean going deaf for however long that takes. So this only starts
/// it: stdin is closed, since a shortcut that reads from it must not hang
/// waiting for input that will never come, and stdout/stderr are
/// discarded, since nothing reads them back. The child is never joined,
/// which is the "spawn and detach" this is meant to be — there is
/// deliberately no 30-second wait to time out of.
pub fn run(name: &str) -> Result<(), String> {
    Command::new("/usr/bin/shortcuts")
        .arg("run")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extracts_the_name_after_the_marker_word() {
        assert_eq!(requested_name("ejecuta el atajo modo trabajo"), Some("modo trabajo"));
        assert_eq!(requested_name("lanza el atajo modo trabajo"), Some("modo trabajo"));
        assert_eq!(requested_name("atajo modo trabajo"), Some("modo trabajo"));
    }

    #[test]
    fn a_plural_mention_is_not_a_request_to_run_one() {
        // "¿qué atajos tengo?" is a question about the installed list, not
        // a request — and belongs to whoever answers that question.
        assert_eq!(requested_name("que atajos tengo"), None);
    }

    #[test]
    fn nothing_named_after_the_marker_is_no_request() {
        assert_eq!(requested_name("ejecuta el atajo"), None);
        assert_eq!(requested_name("cierra la ventana"), None);
    }

    #[test]
    fn finds_the_shortcut_named_outright() {
        let installed = names(&["Modo Trabajo", "Buenas Noches"]);
        assert_eq!(best_match("modo trabajo", &installed), Some("Modo Trabajo"));
        assert_eq!(best_match("buenas noches", &installed), Some("Buenas Noches"));
    }

    #[test]
    fn tolerates_one_slip_per_word() {
        let installed = names(&["Modo Trabajo"]);
        // Straight vowel/consonant slips, one edit each.
        assert_eq!(best_match("modo trabaho", &installed), Some("Modo Trabajo"));
        assert_eq!(best_match("modo trabaja", &installed), Some("Modo Trabajo"));
    }

    #[test]
    fn a_different_word_entirely_does_not_match() {
        let installed = names(&["Modo Trabajo"]);
        assert_eq!(best_match("modo cocina", &installed), None);
    }

    #[test]
    fn a_different_word_count_does_not_match() {
        let installed = names(&["Modo Trabajo"]);
        assert_eq!(best_match("modo", &installed), None);
        assert_eq!(best_match("el modo de trabajo", &installed), None);
    }

    #[test]
    fn the_closest_of_several_names_wins() {
        let installed = names(&["Modo Trabajo", "Modo Trabaho"]);
        // Both are one edit away from nothing, so the exact one wins.
        assert_eq!(best_match("modo trabajo", &installed), Some("Modo Trabajo"));
    }

    #[test]
    fn nothing_matches_an_empty_list() {
        assert_eq!(best_match("modo trabajo", &[]), None);
    }
}

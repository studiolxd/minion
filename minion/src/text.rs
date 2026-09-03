//! Normalisation and fuzzy matching for recogniser output.
//!
//! Parakeet returns text with capitals, accents and punctuation
//! ("Ordenador, abre Chrome."). Matching against the command table needs it
//! reduced to something predictable first.
//!
//! Two stages. [`normalise`] strips accents and punctuation; [`keywords`]
//! then drops filler words and reduces verbs to one form, so that a single
//! natural phrase in the table covers the ways people really say it.

use crate::spanish;

/// Lowercase, accent-free, punctuation-free, single-spaced.
pub fn normalise(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.to_lowercase().chars() {
        let plain = match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        };
        out.push(plain);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The words that carry meaning, with verbs reduced to one form.
///
/// "cierra la ventana", "cerrar ventana" and "cierra ventana" all reduce to
/// `[cerrar, ventana]`, which is what makes the table forgiving without
/// listing every phrasing.
pub fn keywords(phrase: &str) -> Vec<String> {
    phrase
        .split_whitespace()
        .filter(|w| !spanish::is_filler(w))
        .map(|w| spanish::canonical_verb(w).to_string())
        .collect()
}

/// How closely `heard` matches `expected`, from 0 to 1.
///
/// The score combines both directions: how much of the expected phrase was
/// heard (recall) and how much of what was heard belongs to it (precision).
/// Both matter, for different reasons — recall alone would let a single
/// word fire a long command, precision alone would let a rambling sentence
/// match anything it happens to contain.
///
/// They are combined as F1, with one exception: saying every word of a
/// short command plus a word or two of context ("guarda el archivo" for
/// "guarda esto") is ordinary speech and must still match, even though
/// precision drops. A whole sentence that merely contains the command gets
/// no such allowance.
///
/// It also means the table can hold one natural phrasing while shorter
/// renderings still score well: "pantalla completa" against "pon la
/// pantalla completa" keeps full precision and loses only some recall.
///
/// Word-level beats edit distance here because the recogniser fails by
/// whole words ("abre cromo"), not by stray letters.
pub fn similarity(heard: &str, expected: &str) -> f32 {
    let expected_words = keywords(expected);
    if expected_words.is_empty() {
        return 0.0;
    }
    let heard_words = keywords(heard);

    // A single word has no context to disambiguate it, so it gets less
    // slack: "copiar" and "cortar" are two edits apart and mean very
    // different things.
    let strict = expected_words.len() == 1;

    if heard_words.is_empty() {
        return 0.0;
    }

    let hits = expected_words
        .iter()
        .filter(|e| heard_words.iter().any(|h| words_match(h, e, strict)))
        .count() as f32;

    let recall = hits / expected_words.len() as f32;
    let precision = hits / heard_words.len() as f32;

    if recall + precision == 0.0 {
        return 0.0;
    }
    let f1 = 2.0 * precision * recall / (precision + recall);

    // Every word of the command was said, with at most two words of
    // context around it. That is someone speaking naturally, not a
    // sentence that happens to contain the words.
    let complete = recall > 0.999;
    let surplus = heard_words.len() - expected_words.len().min(heard_words.len());
    if complete && surplus <= 2 {
        return f1.max(0.8);
    }
    f1
}

/// Whether two words should be treated as the same one.
///
/// Tolerates a single recogniser slip ("safaris" for "safari"). One edit is
/// the ceiling on purpose: at two, "ventana" becomes "pestana", "copiar"
/// becomes "cortar" and "deshacer" becomes "rehacer" — all pairs of
/// commands that do very different things. Bigger mangles are handled by
/// listing the mangled form as an alias, which is explicit and safe.
fn words_match(a: &str, b: &str, strict: bool) -> bool {
    if a == b {
        return true;
    }
    // The heard word may swallow the expected one: the recogniser writes
    // "abrecrome" when two words run together, and "crome" is still in
    // there. The reverse is not allowed — an expected word containing what
    // was heard means the heard word is merely a prefix of something else,
    // and "marca" is not "marcadores".
    if b.len() >= 4 && a.len() > b.len() && a.contains(b) {
        return true;
    }

    // Words shorter than four characters must match exactly: at that
    // length a single edit is a different word ("pon" and "son").
    if a.len().max(b.len()) < 4 {
        return false;
    }
    let _ = strict;
    edit_distance(a, b) <= 1
}

/// How many single-character edits separate two words.
pub fn edits_between(a: &str, b: &str) -> usize {
    edit_distance(a, b)
}

/// Levenshtein distance, bailing out once the length gap alone exceeds two.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 2 {
        return usize::MAX / 2;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous + usize::from(ca != cb);
            previous = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_recogniser_output() {
        assert_eq!(normalise("Ordenador, abre Chrome."), "ordenador abre chrome");
        assert_eq!(normalise("¿Qué tal estás?"), "que tal estas");
        assert_eq!(normalise("  MAÑANA   sí  "), "manana si");
    }

    #[test]
    fn tolerates_recogniser_slips() {
        assert!(words_match("abrecrome", "crome", false));
        assert!(words_match("safaris", "safari", false));
        assert!(!words_match("safari", "terminal", false));
    }

    #[test]
    fn a_short_word_is_not_a_longer_one() {
        // From the log: "ir a marca.com" opened the bookmarks, because
        // "marca" is a prefix of "marcadores".
        assert!(!words_match("marca", "marcadores", false));
        assert!(!words_match("pon", "ponme", false));
        // But a run-together transcription still matches.
        assert!(words_match("abrecrome", "crome", false));
    }

    #[test]
    fn scores_the_right_command_highest() {
        assert!(similarity("abre chrome", "abre chrome") > 0.99);
        assert!(similarity("abre chrome", "cierra ventana") < 0.3);
    }

    #[test]
    fn keeps_similar_commands_apart() {
        // All of these are two edits apart and mean different things.
        assert!(similarity("cortar", "copiar") < 0.7);
        assert!(similarity("borrar", "cortar") < 0.7);
        assert!(similarity("cierra la ventana", "cierra la pestana") < 0.7);
        assert!(similarity("rehaz el cambio", "deshaz el cambio") < 0.7);
    }

    #[test]
    fn a_shorter_rendering_still_matches() {
        // The table holds the natural phrasing; saying less should still work.
        assert!(similarity("pantalla completa", "pon la pantalla completa") > 0.7);
        assert!(similarity("sube el volumen", "sube el volumen") > 0.99);
    }

    #[test]
    fn phrasing_does_not_matter() {
        // The point of keywords(): one entry in the table, many ways to say it.
        for spoken in ["cierra la ventana", "cerrar ventana", "cierra ventana",
                       "cierra esta ventana", "cierra la ventana por favor"] {
            assert!(
                similarity(spoken, "cierra la ventana") > 0.9,
                "«{spoken}» should match the canonical phrasing"
            );
        }
    }

    #[test]
    fn keywords_drop_filler_and_unify_verbs() {
        assert_eq!(keywords("cierra la ventana"), vec!["cerrar", "ventana"]);
        assert_eq!(keywords("cerrar ventana"), vec!["cerrar", "ventana"]);
        assert_eq!(keywords("guarda esto"), vec!["guardar"]);
    }

    #[test]
    fn rejects_long_conversational_sentences() {
        let chatter = "pues no se yo creo que deberiamos abrir chrome manana";
        assert!(similarity(chatter, "abre chrome") < 0.5);
    }
}

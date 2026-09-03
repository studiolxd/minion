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
///
/// Verbs are canonicalised before fillers are dropped, because one word is
/// both: "para" is a preposition and the imperative of "parar". Dropping
/// first left "para la música" as just `[musica]`, so saying "minion,
/// música" paused Spotify instead of opening it.
pub fn keywords(phrase: &str) -> Vec<String> {
    phrase
        .split_whitespace()
        .map(spanish::canonical_verb)
        .filter(|w| !spanish::is_filler(w))
        .map(str::to_string)
        .collect()
}

/// How a Spanish mouth would say a written word, reduced to its sounds.
///
/// The recogniser is Spanish; the names it is asked to write are mostly
/// English. What comes back is the English name spelled as it sounded to a
/// Spanish ear — "Chrome" as "crome", "cromo" or "crom", "Photoshop" as
/// "fotochop". Those are the same sounds written three ways, and comparing
/// spellings can only ever catch them one alias at a time.
///
/// So spelling is reduced to sound first, using the part of Spanish
/// orthography that is genuinely many-to-one: b and v are one sound, c
/// before a, o, u is k and before e, i is s, z is s too, qu is k, ll is y,
/// h is silent, double letters are single sounds. Two more rules are about
/// loanwords rather than Spanish: "ch" before a consonant is the English
/// /k/ cluster ("chrome"), and Spanish has no word-initial s + consonant,
/// so "espotifai" and "spotify" start alike. Finally the last vowel goes,
/// because that is the vowel Spanish adds to an English name that ends in
/// a consonant, and the one it wavers over ("crome", "cromo", "croma").
///
/// The result is not IPA and is not meant to be read: it only has to be
/// equal for two spellings of the same sound, and different otherwise.
/// `c` in the output means the "ch" sound, since plain c never survives.
pub fn phonetic(phrase: &str) -> String {
    normalise(phrase)
        .split_whitespace()
        .map(phonetic_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_vowel(c: Option<char>) -> bool {
    matches!(c, Some('a' | 'e' | 'i' | 'o' | 'u'))
}

/// Whether a letter is one of the two that soften c and g.
fn is_front(c: Option<char>) -> bool {
    matches!(c, Some('e' | 'i'))
}

fn phonetic_word(word: &str) -> String {
    let letters: Vec<char> = word.chars().collect();
    let mut sounds = String::with_capacity(letters.len());
    let mut at = 0;
    while at < letters.len() {
        let this = letters[at];
        let next = letters.get(at + 1).copied();
        let after = letters.get(at + 2).copied();
        let mut taken = 1;
        match this {
            // Silent, always: "hola" is "ola". The digraphs that use it
            // are taken below, before the h is ever reached on its own.
            'h' => {}
            'c' if next == Some('h') => {
                taken = 2;
                // "chrome", "christian": a cluster Spanish does not have,
                // said /k/. Before a vowel it is the Spanish "ch", which
                // nothing else in this alphabet writes, so it keeps the c.
                sounds.push(if is_vowel(after) { 'c' } else { 'k' });
            }
            'c' => sounds.push(if is_front(next) { 's' } else { 'k' }),
            'q' => {
                sounds.push('k');
                if next == Some('u') {
                    taken = 2;
                }
            }
            'z' => sounds.push('s'),
            'v' => sounds.push('b'),
            'w' => sounds.push('u'),
            'x' => sounds.push_str("ks"),
            'p' if next == Some('h') => {
                taken = 2;
                sounds.push('f');
            }
            's' if next == Some('h') => {
                taken = 2;
                // English /ʃ/ arrives as the nearest Spanish sound, "ch".
                sounds.push('c');
            }
            'g' if is_front(next) => sounds.push('j'),
            'g' if next == Some('u') && is_front(after) => {
                taken = 2;
                sounds.push('g');
            }
            // Spanish writes the English w of a loanword as "gu":
            // "guasap" for WhatsApp, "guisqui" for whisky.
            'g' if next == Some('u') && is_vowel(after) => {
                taken = 2;
                sounds.push('u');
            }
            'l' if next == Some('l') => {
                taken = 2;
                sounds.push('y');
            }
            // A y with no vowel after it is the vowel i: "spotify".
            'y' if !is_vowel(next) => sounds.push('i'),
            other => sounds.push(other),
        }
        at += taken;
        // Double letters are one sound, whatever produced them.
        if sounds.chars().count() >= 2 {
            let mut tail = sounds.chars().rev();
            let (last, before) = (tail.next(), tail.next());
            if last == before {
                sounds.pop();
            }
        }
    }

    // Spanish has no word-initial s + consonant and puts an e in front of
    // one: "spotify" is said "espotifai". Dropping it makes the two spellings
    // start the same way, whichever the recogniser chose to write.
    if let Some(rest) = sounds.strip_prefix('e') {
        if rest.starts_with('s') && !is_vowel(rest.chars().nth(1)) && rest.chars().count() >= 3 {
            sounds = rest.to_string();
        }
    }

    // The final vowel of a name is the one Spanish invents: "Chrome" comes
    // back as "crom", "crome" and "cromo" on different days. Only for words
    // long enough that the vowel is not most of the word.
    if sounds.chars().count() >= 4 && is_vowel(sounds.chars().last()) {
        sounds.pop();
    }
    sounds
}

/// How closely `heard` matches `expected`, from 0 to 1.
///
/// The score has to mean something across its whole range, because the
/// «Sensibilidad» slider cuts it wherever the user puts it. The scale:
///
/// | Score | What it is |
/// |---|---|
/// | 1.0 | every word of the command, said exactly, and nothing else |
/// | 0.9–1.0 | every word, with a slip or a word or two of context |
/// | 0.75–0.9 | every word, but leaning on the fuzzy matching to get there |
/// | below 0.75 | part of the command only |
///
/// Two things are combined. First, how well each expected word was said:
/// exactly (1.0), run together with its neighbour (0.9), one edit away
/// (0.8) or recognised only by its stem (0.75). Second, how much else was
/// said around it. Saying every word of a short command plus a word or two
/// of context ("guarda el archivo" for "guarda esto") is ordinary speech,
/// so each extra word costs a little rather than disqualifying the match.
///
/// A phrase that says the whole command never falls below 0.75, whatever
/// slack it needed, so it always clears the default 0.7 threshold; raising
/// the threshold above that is what makes the app pickier, in steps that
/// each rule out one kind of slack.
///
/// A phrase that says only part of the command is scored as the F1 of
/// recall and precision instead — recall alone would let a single word
/// fire a long command, precision alone would let a rambling sentence
/// match anything it happens to contain. The same applies to a sentence
/// that merely contains the command among many other words: three or more
/// words of surplus is a sentence, not a command with context.
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

    // How well each expected word was said, and what the slack cost.
    let mut hits = 0;
    let mut quality = 0.0;
    let mut slack = 0.0;
    for word in &expected_words {
        let best = heard_words
            .iter()
            .map(|h| match_quality(h, word, strict))
            .fold(0.0, f32::max);
        if best > 0.0 {
            hits += 1;
            quality += best;
            slack += 1.0 - best;
        }
    }
    if hits == 0 {
        return 0.0;
    }

    // Every word of the command was said, with at most two words of
    // context around it. That is someone speaking naturally, not a
    // sentence that happens to contain the words.
    let surplus = heard_words.len() - expected_words.len().min(heard_words.len());
    if hits == expected_words.len() && surplus <= 2 {
        const EXTRA_WORD: f32 = 0.08;
        const FLOOR: f32 = 0.75;
        return (1.0 - slack - EXTRA_WORD * surplus as f32).max(FLOOR);
    }

    let recall = quality / expected_words.len() as f32;
    let precision = quality / heard_words.len() as f32;
    2.0 * precision * recall / (precision + recall)
}

/// How well one heard word stands in for an expected one, 0 if it does not.
///
/// The grades are the ways the recogniser goes wrong, in order of how much
/// benefit of the doubt each one needs. One edit is the ceiling on purpose:
/// at two, "ventana" becomes "pestana", "copiar" becomes "cortar" and
/// "deshacer" becomes "rehacer" — all pairs of commands that do very
/// different things. Bigger mangles are handled by listing the mangled form
/// as an alias, which is explicit and safe.
fn match_quality(heard: &str, expected: &str, strict: bool) -> f32 {
    if heard == expected {
        return 1.0;
    }
    // The heard word may swallow the expected one: the recogniser writes
    // "abrecrome" when two words run together, and "crome" is still in
    // there. The reverse is not allowed — an expected word containing what
    // was heard means the heard word is merely a prefix of something else,
    // and "marca" is not "marcadores".
    if expected.len() >= 4 && heard.len() > expected.len() && heard.contains(expected) {
        return 0.9;
    }

    // A command of a single word has nothing around it to disambiguate,
    // so it gets no slack at all: "contar" is one edit from "cortar" and
    // used to run it at full confidence.
    if strict {
        return 0.0;
    }

    // Words shorter than four characters must match exactly: at that
    // length a single edit is a different word ("pon" and "son").
    if heard.len().max(expected.len()) < 4 {
        return 0.0;
    }
    if edit_distance(heard, expected) <= 1 {
        return 0.8;
    }
    if shares_stem(heard, expected) {
        return 0.75;
    }
    0.0
}

/// Whether two long words are the same word in different clothes.
///
/// The recogniser drifts into English halfway through a Spanish phrase and
/// writes "minimized" for "minimiza": two edits, so a different word by
/// the rule above, yet the first seven letters agree. A shared opening
/// that long, covering nearly all of the shorter word, is one stem with
/// two endings. Short openings do not count — "copiar" and "cortar" share
/// two letters and "maximiza" and "minimiza" share one.
fn shares_stem(a: &str, b: &str) -> bool {
    const MIN_STEM: usize = 6;
    let shorter = a.chars().count().min(b.chars().count());
    if shorter < MIN_STEM {
        return false;
    }
    let shared = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    shared >= MIN_STEM && shared * 4 >= shorter * 3
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

    /// Whether two words match at all, the shape most of these tests want.
    fn words_match(a: &str, b: &str, strict: bool) -> bool {
        match_quality(a, b, strict) > 0.0
    }


    #[test]
    fn saying_the_command_exactly_scores_full() {
        assert_eq!(similarity("cierra la ventana", "cierra la ventana"), 1.0);
        assert_eq!(similarity("cerrar ventana", "cierra la ventana"), 1.0);
    }

    #[test]
    fn slack_costs_score() {
        let perfect = similarity("cierra la ventana", "cierra la ventana");
        // One word one edit away is still the command, but less certainly.
        let slip = similarity("cierra la ventena", "cierra la ventana");
        assert!(slip < perfect, "a slip should score below {perfect}, got {slip}");
        // And context costs by the word, so more of it scores lower.
        let one = similarity("cierra la ventana ahora", "cierra la ventana");
        let two = similarity("cierra la ventana ahora mismo", "cierra la ventana");
        assert!(one < perfect, "one extra word should score below {perfect}");
        assert!(two < one, "two extra words should score below one ({two} vs {one})");
    }

    #[test]
    fn the_default_threshold_accepts_ordinary_speech() {
        // Every phrasing the command suite accepts, scored directly: the
        // default must let all of them through, whatever slack they needed.
        const DEFAULT: f32 = crate::commands::DEFAULT_THRESHOLD;
        for (spoken, canonical) in [
            ("cerrar ventana", "cierra la ventana"),
            ("cierra esta ventana", "cierra la ventana"),
            ("cierra la ventana por favor", "cierra la ventana"),
            ("guarda el archivo", "guarda esto"),
            ("subir el volumen", "sube el volumen"),
            ("actualiza la pagina", "recarga la pagina"),
            ("minimized the ventana", "minimiza la ventana"),
            ("seleccionar todo", "selecciona todo"),
            ("abre una ventana nueva", "ventana nueva"),
        ] {
            let score = similarity(spoken, canonical);
            assert!(
                score >= DEFAULT,
                "«{spoken}» scores {score}, below the default {DEFAULT}"
            );
        }
    }

    #[test]
    fn a_demanding_threshold_rejects_a_slip() {
        // What the «Sensibilidad» slider is for: at its most demanding,
        // only what was said exactly gets through.
        let slip = similarity("cierra la ventena", "cierra la ventana");
        assert!(slip < 0.95, "a one-edit slip should not survive 0.95, got {slip}");
        assert!(slip >= crate::commands::DEFAULT_THRESHOLD);
        assert!(similarity("cierra la ventana", "cierra la ventana") >= 0.95);
    }

    #[test]
    fn a_shared_stem_survives_a_foreign_ending() {
        assert!(words_match("minimized", "minimiza", false));
        assert!(words_match("seleccionado", "seleccionar", false));
        // Different words that merely start alike stay apart.
        assert!(!words_match("maximiza", "minimiza", false));
        assert!(!words_match("cortar", "copiar", false));
        assert!(!words_match("pestana", "ventana", false));
        assert!(!words_match("marcar", "marcadores", false));
    }

    #[test]
    fn a_one_word_command_gets_no_slack() {
        // A single word carries the whole instruction, so a slip in it
        // changes what happens: "corta esto" reduces to one keyword.
        assert!(!words_match("contar", "cortar", true));
        assert!(words_match("contar", "cortar", false));
        // Equality and a run-together word are still enough.
        assert!(words_match("cortar", "cortar", true));
        assert!(words_match("abrecrome", "crome", true));
        // The command it used to run, at full confidence.
        assert!(similarity("contar esto", "corta esto") < 0.7);
    }

    #[test]
    fn one_sound_survives_many_spellings() {
        // Each group is one word the recogniser writes differently from
        // one utterance to the next. Straight from the alias lists and the
        // log: "crome", "cromo" and "grum" are all "Chrome" said in Spanish.
        for group in [
            &["chrome", "crome", "cromo", "croma", "crom", "kromo"][..],
            &["safari", "safary", "zafari", "safarí"][..],
            &["espotifai", "spotifai", "espotifay"][..],
            &["photoshop", "fotochop"][..],
            &["guasap", "uasap", "wasap"][..],
            &["yamada", "llamada"][..],
            &["bale", "vale"][..],
            &["word", "guord"][..],
            &["quiero", "kiero"][..],
        ] {
            let first = phonetic(group[0]);
            for spelling in group {
                assert_eq!(
                    phonetic(spelling),
                    first,
                    "«{spelling}» should sound like «{}»",
                    group[0]
                );
            }
        }
    }

    #[test]
    fn different_words_keep_different_sounds() {
        // The pairs that matter: each of these has been, or could be,
        // mistaken for the other by a looser rule.
        for (a, b) in [
            ("gmail", "mail"),
            ("mallorca", "orca"),
            ("minuto", "minion"),
            ("safari", "terminal"),
            ("chrome", "cromwell"),
            ("copiar", "cortar"),
            ("ventana", "pestana"),
            ("marca", "marcadores"),
        ] {
            assert_ne!(phonetic(a), phonetic(b), "«{a}» and «{b}» are different words");
        }
    }

    #[test]
    fn the_sounds_are_the_ones_spanish_really_merges() {
        // Spelled out, so the rules can be read off the test: b and v, c
        // before a back vowel and k and qu, c before a front vowel and z,
        // ll and y, a silent h, a double letter, and the vowel Spanish
        // adds to the end of an English name.
        assert_eq!(phonetic("vaca"), "bak");
        assert_eq!(phonetic("queso"), "kes");
        assert_eq!(phonetic("zapato"), "sapat");
        assert_eq!(phonetic("hola"), "ola");
        assert_eq!(phonetic("perro"), "per");
        assert_eq!(phonetic("chrome"), "krom");
        // A short word keeps its vowel: there would be nothing left of it.
        assert_eq!(phonetic("no"), "no");
    }

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
        // "para" is a preposition and a verb; as a verb it must survive.
        assert_eq!(keywords("para la musica"), vec!["parar", "musica"]);
    }

    #[test]
    fn rejects_long_conversational_sentences() {
        let chatter = "pues no se yo creo que deberiamos abrir chrome manana";
        assert!(similarity(chatter, "abre chrome") < 0.5);
    }
}

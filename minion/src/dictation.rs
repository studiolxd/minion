//! What continuous dictation ("minion, empieza a dictar") does to the words
//! before they are typed: spoken punctuation, capitalisation and a personal
//! vocabulary for names the recogniser mangles.
//!
//! [`Transformer`] is pure and holds only what one dictation session needs
//! to remember between chunks — whether the last one ended a sentence, and
//! whether an open quote is waiting to be closed. `main.rs` owns one for the
//! life of a dictation session (built fresh on `Outcome::EnterDictation`,
//! dropped on `Outcome::LeaveDictation`) and feeds it every `Outcome::Type`.

use crate::commands::EditIntent;
use crate::config::Config;
use crate::text;

/// An exact instruction for what to type next, computed from what has
/// actually reached the keyboard so far — never from what was spoken,
/// which spoken punctuation, capitalisation and personal vocabulary may
/// have turned into something a different length. `main.rs` carries this
/// out with `actions::press(key::DELETE)` and, for `Retype`, a `type_text`
/// straight after.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Press ⌫ this many times, and nothing else.
    DeleteChars(usize),
    /// Press ⌫ this many times, then type this instead (`main.rs` adds
    /// the trailing space, as it does for an ordinary `Outcome::Type`).
    Retype { delete: usize, text: String },
}

/// One word as it arrived from the recogniser, or one already resolved by
/// the personal vocabulary and no longer open to further interpretation.
enum Tok {
    Word(String),
    Literal(String),
}

/// One piece of the rendered output, with the spacing it needs around it.
struct Atom {
    text: String,
    /// No space before this atom, whatever came before it (closing
    /// punctuation, a closing quote).
    glue_before: bool,
    /// No space after this atom (an opening ¿ ¡ ( or an opening quote).
    no_space_after: bool,
}

fn word_atom(text: String) -> Atom {
    Atom { text, glue_before: false, no_space_after: false }
}

/// Turns spoken Spanish into typed text: punctuation words become signs,
/// sentences get their capitals, and a personal vocabulary fixes names the
/// recogniser cannot spell.
///
/// Pure and `&Config`-derived — nothing here touches the microphone, the
/// keyboard or the log. Built once per dictation session, so the state it
/// keeps (whether the next word starts a sentence, whether a quote is
/// open) only ever spans one session.
pub struct Transformer {
    spoken_punctuation: bool,
    auto_capitalise: bool,
    /// Heard words (normalised) mapped to what to type, longest heard
    /// phrase first so "mir angel sufire" is tried before "mir".
    vocabulary: Vec<(Vec<String>, String)>,
    capitalize_next: bool,
    quote_open: bool,
    /// What was actually typed, one rendered chunk per call to `render`,
    /// oldest first, never including the trailing space `main.rs` types
    /// after each one. What an edit command ("borra la última palabra")
    /// works from, since it must delete exactly what reached the
    /// keyboard, not what was spoken.
    history: Vec<String>,
}

impl Transformer {
    /// Reads the settings this dictation session will use. Config is read
    /// fresh so vocabulary added while Minion is running takes effect the
    /// next time dictation starts, without a restart.
    pub fn new(config: &Config) -> Self {
        Self {
            spoken_punctuation: config.spoken_punctuation,
            auto_capitalise: config.auto_capitalise,
            vocabulary: config.dictation_words(),
            // The very first word of a dictation session starts a sentence.
            capitalize_next: true,
            quote_open: false,
            history: Vec::new(),
        }
    }

    /// Renders one chunk of what was heard while dictating. `main.rs` types
    /// exactly what comes back, followed by its own single space.
    ///
    /// Also remembers it: an edit command a later chunk asks for
    /// (`edit`) is resolved against this, not against what was spoken.
    pub fn render(&mut self, spoken: &str) -> String {
        let rendered = self.render_uncached(spoken);
        if !rendered.is_empty() {
            self.history.push(rendered.clone());
        }
        rendered
    }

    fn render_uncached(&mut self, spoken: &str) -> String {
        if let Some(digits) = self.try_number(spoken) {
            return digits;
        }

        let words: Vec<&str> = spoken.split_whitespace().collect();
        if words.is_empty() {
            return String::new();
        }

        let toks = self.apply_vocabulary(&words);
        let atoms = self.build_atoms(toks);
        assemble(&atoms)
    }

    /// Turns a spoken edit command into an exact edit against what has
    /// actually been typed. `Err` is what to say instead, when there is
    /// nothing to act on: nothing typed yet, or a «cambia X por Y» whose
    /// X is not in the last chunk.
    pub fn edit(&mut self, intent: &EditIntent) -> Result<Edit, String> {
        match intent {
            EditIntent::DeleteLastWord => {
                let chunk = self.history.last().ok_or("nothing typed yet")?;
                let last_word_chars = match chunk.rfind(char::is_whitespace) {
                    Some(byte) => chunk[byte + 1..].chars().count(),
                    None => chunk.chars().count(),
                };
                self.truncate_last_chunk(last_word_chars);
                Ok(Edit::DeleteChars(last_word_chars + 1))
            }
            EditIntent::DeleteLastPhrase => {
                let chunk = self.history.pop().ok_or("nothing typed yet")?;
                Ok(Edit::DeleteChars(chunk.chars().count() + 1))
            }
            EditIntent::Replace { find, replace } => {
                let chunk = self.history.last().ok_or_else(|| not_found(find))?.clone();
                let (start, end) =
                    find_last_words(&chunk, find).ok_or_else(|| not_found(find))?;
                let chars: Vec<char> = chunk.chars().collect();
                let kept: String = chars[..start].iter().collect();
                let tail: String = chars[end..].iter().collect();
                let delete = chars.len() - start + 1; // plus the trailing space typed after the chunk
                let text = format!("{replace}{tail}");
                *self.history.last_mut().expect("checked above") = format!("{kept}{text}");
                Ok(Edit::Retype { delete, text })
            }
        }
    }

    /// Drops the last `chars` characters from the last chunk of history,
    /// so a further edit sees exactly what is left on screen. Removes the
    /// chunk entirely once nothing of it remains.
    fn truncate_last_chunk(&mut self, chars: usize) {
        let Some(last) = self.history.last_mut() else { return };
        let keep = last.chars().count().saturating_sub(chars);
        *last = last.chars().take(keep).collect();
        if last.is_empty() {
            self.history.pop();
        }
    }

    /// «número cuarenta y dos», or a chunk that is nothing but a number
    /// phrase on its own — up to 999 999, which is as far as the brief asks
    /// for. Returns `None` for anything that is not entirely a number, so a
    /// sentence that happens to contain "cinco" falls through to normal
    /// handling untouched.
    fn try_number(&self, spoken: &str) -> Option<String> {
        let normalised = text::normalise(spoken);
        let mut tokens: Vec<&str> = normalised.split_whitespace().collect();
        if tokens.first() == Some(&"numero") {
            tokens.remove(0);
        }
        if tokens.is_empty() {
            return None;
        }
        let (value, rest) = parse_thousands(&tokens)?;
        rest.is_empty().then(|| value.to_string())
    }

    /// Replaces runs of words matching a `[[dictation_words]]` entry with
    /// what it says to type instead, greedily and longest-match-first —
    /// the whole point being that "mir angel sufire" must not stop at
    /// matching just "mir" if a longer entry also fits.
    fn apply_vocabulary(&self, words: &[&str]) -> Vec<Tok> {
        let normalised: Vec<String> = words.iter().map(|w| text::normalise(w)).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < words.len() {
            let matched = self.vocabulary.iter().find(|(heard, _)| {
                let len = heard.len();
                i + len <= normalised.len() && normalised[i..i + len] == heard[..]
            });
            match matched {
                Some((heard, written)) => {
                    out.push(Tok::Literal(written.clone()));
                    i += heard.len();
                }
                None => {
                    out.push(Tok::Word(words[i].to_string()));
                    i += 1;
                }
            }
        }
        out
    }

    /// Interprets a token stream: spoken punctuation, the «literal» escape,
    /// «mayúscula»/«en mayúsculas» and ordinary words needing their case
    /// decided.
    fn build_atoms(&mut self, toks: Vec<Tok>) -> Vec<Atom> {
        let mut atoms = Vec::new();
        let mut shout = false;
        let mut i = 0;
        while i < toks.len() {
            let Tok::Word(word) = &toks[i] else {
                // Already resolved by the vocabulary: typed exactly as
                // given, with none of its own casing changed — but it still
                // occupies a word position, so a capital pending from a
                // sentence boundary is spent here rather than leaking onto
                // whatever ordinary word comes after it.
                let Tok::Literal(text) = &toks[i] else { unreachable!() };
                self.capitalize_next = false;
                atoms.push(word_atom(text.clone()));
                i += 1;
                continue;
            };
            let norm = text::normalise(word);

            if self.spoken_punctuation && norm == "literal" {
                if let Some(Tok::Word(next)) = toks.get(i + 1) {
                    atoms.push(word_atom(self.cased(next, &mut shout)));
                    i += 2;
                    continue;
                }
            }

            if self.spoken_punctuation {
                if let Some((consumed, atom)) = self.match_punctuation(&toks, i) {
                    if let Some(atom) = atom {
                        atoms.push(atom);
                    }
                    i += consumed;
                    continue;
                }
            }

            if self.auto_capitalise && norm == "mayuscula" {
                self.capitalize_next = true;
                i += 1;
                continue;
            }
            if self.auto_capitalise
                && norm == "en"
                && matches!(toks.get(i + 1), Some(Tok::Word(w)) if text::normalise(w) == "mayusculas")
            {
                shout = true;
                i += 2;
                continue;
            }

            atoms.push(word_atom(self.cased(word, &mut shout)));
            i += 1;
        }
        atoms
    }

    /// Applies capitalisation to one ordinary word: shouting from «en
    /// mayúsculas», then a single capital from either «mayúscula» or a
    /// sentence boundary, in that priority.
    fn cased(&mut self, word: &str, shout: &mut bool) -> String {
        if *shout {
            return word.to_uppercase();
        }
        if self.auto_capitalise && self.capitalize_next {
            self.capitalize_next = false;
            return capitalize_first(word);
        }
        word.to_string()
    }

    /// Tries every spoken-punctuation phrase starting at `i`, longest
    /// first so "punto y coma" is not read as "punto" followed by leftover
    /// words. Returns how many tokens it consumed and the atom to emit —
    /// `None` for the atom means it consumed tokens but produced nothing
    /// (a quote toggle still needs an atom; only the case is different).
    fn match_punctuation(&mut self, toks: &[Tok], i: usize) -> Option<(usize, Option<Atom>)> {
        let words = |n: usize| -> Option<Vec<String>> {
            let mut out = Vec::with_capacity(n);
            for tok in toks.get(i..i + n)? {
                match tok {
                    Tok::Word(w) => out.push(text::normalise(w)),
                    Tok::Literal(_) => return None,
                }
            }
            Some(out)
        };

        if let Some(w) = words(3) {
            let phrase = w.join(" ");
            let emit = match phrase.as_str() {
                "punto y coma" => Some(glued(";")),
                "punto y aparte" => {
                    self.capitalize_next = true;
                    Some(glued(".\n"))
                }
                "punto y seguido" => {
                    self.capitalize_next = true;
                    Some(glued("."))
                }
                "salto de linea" => Some(word_atom("\n".to_string())),
                _ => None,
            };
            if emit.is_some() {
                return Some((3, emit));
            }
        }

        if let Some(w) = words(2) {
            let phrase = w.join(" ");
            let emit = match phrase.as_str() {
                "dos puntos" => Some(glued(":")),
                "nueva linea" => Some(word_atom("\n".to_string())),
                "abre interrogacion" => Some(opening("¿")),
                "cierra interrogacion" => Some(glued("?")),
                "abre exclamacion" => Some(opening("¡")),
                "cierra exclamacion" => Some(glued("!")),
                "abre parentesis" => Some(opening("(")),
                "cierra parentesis" => Some(glued(")")),
                "puntos suspensivos" => Some(glued("…")),
                _ => None,
            };
            if emit.is_some() {
                return Some((2, emit));
            }
        }

        if let Some(w) = words(1) {
            let emit = match w[0].as_str() {
                "coma" => Some(glued(",")),
                "punto" => {
                    self.capitalize_next = true;
                    Some(glued("."))
                }
                "interrogacion" => Some(glued("?")),
                "exclamacion" => Some(glued("!")),
                "comillas" => {
                    self.quote_open = !self.quote_open;
                    Some(if self.quote_open { opening("\"") } else { glued("\"") })
                }
                "guion" => Some(fused("-")),
                "arroba" => Some(fused("@")),
                "almohadilla" => Some(fused("#")),
                "barra" => Some(fused("/")),
                _ => None,
            };
            if let Some(emit) = emit {
                return Some((1, Some(emit)));
            }
        }

        None
    }
}

/// A sign that glues to what came before it and takes a normal space
/// after: `,` `.` `;` `:` `?` `!` `)` a closing quote and the ellipsis.
fn glued(text: &str) -> Atom {
    Atom { text: text.to_string(), glue_before: true, no_space_after: false }
}

/// A sign that takes a normal space before it and attaches to what follows:
/// `¿` `¡` `(` and an opening quote.
fn opening(text: &str) -> Atom {
    Atom { text: text.to_string(), glue_before: false, no_space_after: true }
}

/// A sign that fuses to both neighbours, with no space on either side:
/// `-` `@` `#` `/`, for handles, hashtags and addresses.
fn fused(text: &str) -> Atom {
    Atom { text: text.to_string(), glue_before: true, no_space_after: true }
}

/// What «cambia X por Y» says when X is not in the last chunk.
fn not_found(find: &str) -> String {
    format!("no encuentro «{find}»")
}

/// The char range of the *last* run of words in `haystack` that matches
/// `needle`, word for word, normalised (accents, case and any attached
/// punctuation dropped) — so "tal" still finds "tal," or "Tal". `needle`
/// may be several words; the match is a contiguous run of exactly that
/// many.
fn find_last_words(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let needle_words: Vec<String> =
        text::normalise(needle).split_whitespace().map(str::to_string).collect();
    if needle_words.is_empty() {
        return None;
    }

    // char-index spans of each whitespace-delimited word in `haystack`,
    // since `split_whitespace` alone throws the positions away.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut chars = 0;
    for c in haystack.chars() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                spans.push((s, chars));
            }
        } else if start.is_none() {
            start = Some(chars);
        }
        chars += 1;
    }
    if let Some(s) = start {
        spans.push((s, chars));
    }

    let haystack_chars: Vec<char> = haystack.chars().collect();
    let words: Vec<String> = spans
        .iter()
        .map(|&(s, e)| text::normalise(&haystack_chars[s..e].iter().collect::<String>()))
        .collect();

    let n = needle_words.len();
    if n == 0 || n > words.len() {
        return None;
    }
    (0..=words.len() - n)
        .rev()
        .find(|&i| words[i..i + n] == needle_words[..])
        .map(|i| (spans[i].0, spans[i + n - 1].1))
}

fn capitalize_first(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Joins atoms with Spanish typographic spacing: nothing before a glued
/// sign, nothing after one that opens.
fn assemble(atoms: &[Atom]) -> String {
    let mut out = String::new();
    let mut previous_no_space_after = true; // nothing needed before the first atom
    for atom in atoms {
        if !out.is_empty() && !atom.glue_before && !previous_no_space_after {
            out.push(' ');
        }
        out.push_str(&atom.text);
        previous_no_space_after = atom.no_space_after;
    }
    out
}

/// Spanish number words up to 999 999 — enough for anything dictated as a
/// figure, not a full numeral grammar. `mil` may be bare ("mil doscientos")
/// or follow a hundreds phrase ("veinte mil", "novecientos noventa y nueve
/// mil").
fn parse_thousands<'a>(words: &'a [&'a str]) -> Option<(u64, &'a [&'a str])> {
    if words.first() == Some(&"mil") {
        let rest = &words[1..];
        return match parse_hundreds(rest) {
            Some((h, rest2)) => Some((1000 + h, rest2)),
            None => Some((1000, rest)),
        };
    }
    let (n, rest) = parse_hundreds(words)?;
    if rest.first() == Some(&"mil") {
        let after = &rest[1..];
        return match parse_hundreds(after) {
            Some((h, rest2)) => Some((n * 1000 + h, rest2)),
            None => Some((n * 1000, after)),
        };
    }
    Some((n, rest))
}

fn parse_hundreds<'a>(words: &'a [&'a str]) -> Option<(u64, &'a [&'a str])> {
    const HUNDREDS: &[(&str, u64)] = &[
        ("cien", 100),
        ("ciento", 100),
        ("doscientos", 200),
        ("trescientos", 300),
        ("cuatrocientos", 400),
        ("quinientos", 500),
        ("seiscientos", 600),
        ("setecientos", 700),
        ("ochocientos", 800),
        ("novecientos", 900),
    ];
    let first = *words.first()?;
    if let Some(&(_, base)) = HUNDREDS.iter().find(|(w, _)| *w == first) {
        // "cien" alone is exactly 100 and never combines ("cien tres" is
        // not how anyone says 103; "ciento tres" is), so only "ciento"
        // and the two-hundreds-and-up words look for a remainder.
        if first == "cien" {
            return Some((100, &words[1..]));
        }
        return match parse_tens(&words[1..]) {
            Some((tail, rest)) => Some((base + tail, rest)),
            None => Some((base, &words[1..])),
        };
    }
    parse_tens(words)
}

fn parse_tens<'a>(words: &'a [&'a str]) -> Option<(u64, &'a [&'a str])> {
    const TENS: &[(&str, u64)] = &[
        ("treinta", 30),
        ("cuarenta", 40),
        ("cincuenta", 50),
        ("sesenta", 60),
        ("setenta", 70),
        ("ochenta", 80),
        ("noventa", 90),
    ];
    let first = *words.first()?;
    if let Some(&(_, base)) = TENS.iter().find(|(w, _)| *w == first) {
        let rest = &words[1..];
        if rest.first() == Some(&"y") {
            if let Some((unit, rest2)) = parse_unit(&rest[1..]) {
                return Some((base + unit, rest2));
            }
        }
        return Some((base, rest));
    }
    parse_unit(words)
}

fn parse_unit<'a>(words: &'a [&'a str]) -> Option<(u64, &'a [&'a str])> {
    const UNITS: &[(&str, u64)] = &[
        ("cero", 0),
        ("uno", 1),
        ("dos", 2),
        ("tres", 3),
        ("cuatro", 4),
        ("cinco", 5),
        ("seis", 6),
        ("siete", 7),
        ("ocho", 8),
        ("nueve", 9),
        ("diez", 10),
        ("once", 11),
        ("doce", 12),
        ("trece", 13),
        ("catorce", 14),
        ("quince", 15),
        ("dieciseis", 16),
        ("diecisiete", 17),
        ("dieciocho", 18),
        ("diecinueve", 19),
        ("veinte", 20),
        ("veintiuno", 21),
        ("veintidos", 22),
        ("veintitres", 23),
        ("veinticuatro", 24),
        ("veinticinco", 25),
        ("veintiseis", 26),
        ("veintisiete", 27),
        ("veintiocho", 28),
        ("veintinueve", 29),
    ];
    let first = *words.first()?;
    UNITS.iter().find(|(w, _)| *w == first).map(|&(_, v)| (v, &words[1..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DictationWordConfig};

    fn transformer() -> Transformer {
        Transformer::new(&Config::default())
    }

    #[test]
    fn spoken_punctuation_becomes_signs_with_spanish_spacing() {
        let mut t = transformer();
        assert_eq!(t.render("hola coma que tal interrogacion"), "Hola, que tal?");
    }

    #[test]
    fn abre_interrogacion_types_the_opening_mark() {
        let mut t = transformer();
        assert_eq!(t.render("abre interrogacion como estas cierra interrogacion"), "¿Como estas?");
    }

    #[test]
    fn every_mapping_is_understood() {
        let cases: &[(&str, &str)] = &[
            ("coma", ","),
            ("punto y coma", ";"),
            ("dos puntos", ":"),
            ("abre interrogacion", "¿"),
            ("cierra interrogacion", "?"),
            ("interrogacion", "?"),
            ("abre exclamacion", "¡"),
            ("cierra exclamacion", "!"),
            ("exclamacion", "!"),
            ("abre parentesis", "("),
            ("cierra parentesis", ")"),
            ("guion", "-"),
            ("arroba", "@"),
            ("almohadilla", "#"),
            ("barra", "/"),
            ("puntos suspensivos", "…"),
            ("nueva linea", "\n"),
            ("salto de linea", "\n"),
        ];
        for (spoken, sign) in cases {
            let mut t = transformer();
            // "x" before and after so the sign's own spacing rules show up
            // without a sentence-starting capital getting in the way.
            let rendered = t.render(&format!("equis {spoken} equis"));
            assert!(rendered.contains(sign), "«{spoken}» -> expected {sign:?} in {rendered:?}");
        }
    }

    #[test]
    fn punto_ends_a_sentence_and_capitalises_the_next_chunk() {
        let mut t = transformer();
        assert_eq!(t.render("hola punto"), "Hola.");
        // The capital survives across calls: a new sentence starting in
        // the next chunk still gets one.
        assert_eq!(t.render("adios"), "Adios");
    }

    #[test]
    fn punto_y_aparte_breaks_the_paragraph_and_still_capitalises() {
        let mut t = transformer();
        assert_eq!(t.render("primer parrafo punto y aparte"), "Primer parrafo.\n");
        assert_eq!(t.render("segundo parrafo"), "Segundo parrafo");
    }

    #[test]
    fn punto_y_seguido_is_a_plain_period_that_still_capitalises() {
        let mut t = transformer();
        assert_eq!(t.render("hola punto y seguido"), "Hola.");
        assert_eq!(t.render("adios"), "Adios");
    }

    #[test]
    fn en_mayusculas_shouts_the_rest_of_the_chunk() {
        let mut t = transformer();
        assert_eq!(t.render("di esto en mayusculas urgente ahora"), "Di esto URGENTE AHORA");
    }

    #[test]
    fn mayuscula_capitalises_only_the_next_word() {
        let mut t = transformer();
        assert_eq!(t.render("hola mayuscula juan como estas"), "Hola Juan como estas");
    }

    #[test]
    fn literal_escapes_a_single_word() {
        let mut t = transformer();
        assert_eq!(t.render("di literal coma ahora"), "Di coma ahora");
    }

    #[test]
    fn comillas_toggles_open_and_close_and_attaches_to_the_word() {
        let mut t = transformer();
        assert_eq!(t.render("dijo comillas hola comillas y se fue"), "Dijo \"hola\" y se fue");
    }

    #[test]
    fn a_personal_word_is_substituted_inside_a_sentence() {
        let mut config = Config::default();
        config.dictation_words.push(DictationWordConfig {
            heard: "mir angel sufire".to_string(),
            written: "Miguel Ángel Subir".to_string(),
        });
        let mut t = Transformer::new(&config);
        assert_eq!(
            t.render("mir angel sufire garcia firma aqui"),
            "Miguel Ángel Subir garcia firma aqui"
        );
    }

    #[test]
    fn the_longest_personal_entry_wins_over_a_shorter_one() {
        let mut config = Config::default();
        config.dictation_words.push(DictationWordConfig {
            heard: "mir".to_string(),
            written: "MIR".to_string(),
        });
        config.dictation_words.push(DictationWordConfig {
            heard: "mir angel".to_string(),
            written: "Miguel Ángel".to_string(),
        });
        let mut t = Transformer::new(&config);
        assert_eq!(t.render("mir angel llega"), "Miguel Ángel llega");
    }

    #[test]
    fn number_words_become_digits() {
        let cases: &[(&str, &str)] = &[
            ("cinco", "5"),
            ("veintiuno", "21"),
            ("treinta y cinco", "35"),
            ("cien", "100"),
            ("ciento veinte", "120"),
            ("doscientos", "200"),
            ("novecientos noventa y nueve", "999"),
            ("mil", "1000"),
            ("dos mil veinticuatro", "2024"),
            ("veinte mil", "20000"),
            ("novecientos noventa y nueve mil novecientos noventa y nueve", "999999"),
        ];
        for (spoken, digits) in cases {
            let mut t = transformer();
            assert_eq!(t.render(spoken), *digits, "for «{spoken}»");
        }
    }

    #[test]
    fn numero_forces_the_number_reading() {
        let mut t = transformer();
        assert_eq!(t.render("numero cuarenta y dos"), "42");
    }

    #[test]
    fn a_sentence_containing_a_number_word_is_not_a_number() {
        let mut t = transformer();
        assert_eq!(t.render("tengo cinco gatos"), "Tengo cinco gatos");
    }

    #[test]
    fn disabled_by_config_types_the_words_verbatim() {
        let config = Config { spoken_punctuation: false, auto_capitalise: false, ..Config::default() };
        let mut t = Transformer::new(&config);
        assert_eq!(t.render("hola coma que tal interrogacion"), "hola coma que tal interrogacion");
    }

    #[test]
    fn spoken_punctuation_off_still_allows_capitalisation_alone() {
        let config = Config { spoken_punctuation: false, ..Config::default() };
        let mut t = Transformer::new(&config);
        assert_eq!(t.render("hola que tal"), "Hola que tal");
    }

    #[test]
    fn borra_la_ultima_palabra_deletes_exactly_the_last_rendered_word() {
        let mut t = transformer();
        // "Hola, que tal" is 13 characters, plus the space `main.rs` types
        // after it: 14 characters reach the keyboard, as the brief spells
        // out. The edit removes only "tal" and the space after it — 4.
        let rendered = t.render("hola coma que tal");
        assert_eq!(rendered, "Hola, que tal");
        assert_eq!(rendered.chars().count() + 1, 14);
        assert_eq!(t.edit(&EditIntent::DeleteLastWord), Ok(Edit::DeleteChars(4)));
    }

    #[test]
    fn borra_la_ultima_palabra_can_consume_a_whole_one_word_chunk() {
        let mut t = transformer();
        t.render("hola");
        t.render("mundo");
        // "mundo" (5) plus its trailing space.
        assert_eq!(t.edit(&EditIntent::DeleteLastWord), Ok(Edit::DeleteChars(6)));
        // With that chunk gone, the next call reaches into "hola".
        assert_eq!(t.edit(&EditIntent::DeleteLastWord), Ok(Edit::DeleteChars(5)));
        // And with nothing left, there is nothing more to take back.
        assert_eq!(t.edit(&EditIntent::DeleteLastWord), Err("nothing typed yet".to_string()));
    }

    #[test]
    fn borra_la_ultima_frase_deletes_the_whole_last_chunk() {
        let mut t = transformer();
        let rendered = t.render("hola coma que tal");
        assert_eq!(
            t.edit(&EditIntent::DeleteLastPhrase),
            Ok(Edit::DeleteChars(rendered.chars().count() + 1))
        );
        // The chunk is gone: a further edit has nothing left to act on.
        assert_eq!(t.edit(&EditIntent::DeleteLastWord), Err("nothing typed yet".to_string()));
    }

    #[test]
    fn cambia_replaces_the_last_occurrence_and_keeps_what_follows_it() {
        let mut t = transformer();
        let rendered = t.render("el mundo es grande");
        assert_eq!(rendered, "El mundo es grande");
        let edit = t.edit(&EditIntent::Replace {
            find: "mundo".to_string(),
            replace: "planeta".to_string(),
        });
        assert_eq!(
            edit,
            Ok(Edit::Retype { delete: 16, text: "planeta es grande".to_string() })
        );
    }

    #[test]
    fn cambia_matches_the_last_occurrence_when_the_word_repeats() {
        let mut t = transformer();
        t.render("gato come gato");
        let edit = t.edit(&EditIntent::Replace {
            find: "gato".to_string(),
            replace: "perro".to_string(),
        });
        // Only the trailing "gato" is replaced: nothing follows it, so
        // deleting back to it takes 5 characters (the word and its space).
        assert_eq!(edit, Ok(Edit::Retype { delete: 5, text: "perro".to_string() }));
    }

    #[test]
    fn cambia_says_so_when_it_cannot_find_the_word() {
        let mut t = transformer();
        t.render("el mundo es grande");
        assert_eq!(
            t.edit(&EditIntent::Replace {
                find: "marte".to_string(),
                replace: "venus".to_string()
            }),
            Err("no encuentro «marte»".to_string())
        );
    }

    #[test]
    fn an_edit_with_nothing_dictated_yet_says_so() {
        let mut t = transformer();
        assert_eq!(t.edit(&EditIntent::DeleteLastWord), Err("nothing typed yet".to_string()));
        assert_eq!(t.edit(&EditIntent::DeleteLastPhrase), Err("nothing typed yet".to_string()));
    }
}

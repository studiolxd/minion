//! The command vocabulary: what can be said, and what each phrase does.
//!
//! Spoken phrases are Spanish because that is the language being spoken;
//! everything else is English. A command is a phrase opening with the wake
//! word, followed by something recognisable. Anything else is ignored — with
//! a microphone that is always on, ignoring is the default and acting is the
//! exception.
//!
//! What can be said no longer lives here: applications, commands and sites
//! are read from the TOML files in `minion/vocabulary/` by
//! [`crate::vocabulary`], so a new application does not need a Rust
//! toolchain. What stays here is everything that is a code path rather than
//! a table — dictation, undo, repeat, questions, numbered commands, the
//! wake word and the verb lists — and the deciding itself.

use std::sync::OnceLock;

use crate::actions::{self, key, Mods};
use crate::config::Config;
use crate::shortcuts;
use crate::spanish;
use crate::text::{keywords, normalise, similarity};
use crate::vocabulary::Vocabulary;

/// Words that mark a sentence as a command. Only counted at the start.
/// Includes what the recogniser actually produces for the name, not just
/// its spelling: said in Spanish it comes out as "minion", "minión",
/// "miñón", "minial", "mini" — normalisation flattens the accents but not
/// the rest. Anything close enough is accepted anyway; see
/// [`sounds_like_wake_word`].
///
/// "minium" and "minial" are two edits from the name and are here rather
/// than reachable by tolerance: at two edits "mínimo" and "mínima" come
/// too. "minio" is gone for the same reason — it is one edit from
/// "mínimo", and "minio" itself is still one edit from "minion".
pub const DEFAULT_WAKE_WORDS: &[&str] =
    &["minion", "minions", "minon", "minial", "mini", "minium"];

/// Everything that can be said, merged from the vocabulary files, the
/// packs and `config.toml`. Set once at startup.
static VOCABULARY: OnceLock<Vocabulary> = OnceLock::new();

/// Set once at startup from the configuration file. Absent means defaults.
static USER_ALIASES: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
static USER_WAKE_WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
static USER_THRESHOLD: OnceLock<f32> = OnceLock::new();

/// Named macros, from `config.toml` only — see [`Macro`].
static MACROS: OnceLock<Vec<Macro>> = OnceLock::new();
/// Which engine a bare "busca X" searches. Set from `config.toml`;
/// unset, invalid, or absent all mean the first of [`SEARCH_ENGINES`].
static USER_SEARCH_ENGINE: OnceLock<&'static str> = OnceLock::new();

/// The vocabulary in force.
///
/// Falls back to the files built into the binary when [`configure`] has not
/// run, which is the case in tests and in anything that decides before the
/// configuration has been read. Minion must work with nothing else on disk,
/// so that fallback is the ordinary case rather than a degraded one.
pub fn vocabulary() -> &'static Vocabulary {
    VOCABULARY.get_or_init(|| {
        let mut built_in = Vocabulary::built_in();
        built_in.report_conflicts();
        built_in
    })
}

/// Applies the user configuration. Call once, before anything is decided.
pub fn configure(config: &Config) {
    let _ = VOCABULARY.set(Vocabulary::load(config));

    let aliases = config.extra_aliases();
    if !aliases.is_empty() {
        // An alias whose command does not exist can never fire. Said now,
        // once, rather than leaving the user to wonder in front of a
        // microphone that answers nothing.
        for (name, phrase) in &aliases {
            if resolve_target(vocabulary(), name) == Target::Unknown {
                crate::journal::write(&format!(
                    "Ignoring alias «{phrase}»: no command is called «{name}»"
                ));
            }
        }
        let _ = USER_ALIASES.set(aliases);
    }
    if let Some(words) = config.wake_words() {
        let _ = USER_WAKE_WORDS.set(words);
    }
    if let Some(threshold) = config.threshold {
        let _ = USER_THRESHOLD.set(threshold.clamp(0.3, 1.0));
    }
    let _ = MACROS.set(config.macros());
    if let Some(name) = config.search_engine() {
        let normalised = normalise(&name);
        match SEARCH_ENGINES.iter().find(|(engine, _)| *engine == normalised) {
            Some((engine, _)) => {
                let _ = USER_SEARCH_ENGINE.set(engine);
            }
            None => crate::journal::write(&format!(
                "Ignoring search_engine «{name}»: not one of {}",
                SEARCH_ENGINES.iter().map(|(engine, _)| *engine).collect::<Vec<_>>().join(", ")
            )),
        }
    }
    // Cached now rather than on first use, so the list is already warm the
    // first time someone says "atajo …" instead of making that utterance
    // wait on `shortcuts list`.
    shortcuts::refresh();
}

/// Where the command an alias points at lives, if it exists at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A command that works anywhere.
    Global,
    /// One that only exists inside particular applications.
    Contextual,
    /// Nothing of that name: the alias can never fire.
    Unknown,
}

/// Resolves the name an alias points at.
///
/// Commands are identified by their name, spelled out in the log and
/// copied into the configuration by hand, so a misspelling ("atras" for
/// "atrás") produces an alias that silently never fires. Checked at
/// startup instead, where it can be said out loud.
pub fn resolve_target(vocabulary: &Vocabulary, name: &str) -> Target {
    if vocabulary.commands.iter().any(|c| c.name == name) {
        Target::Global
    } else if vocabulary.contextual.iter().any(|c| c.name == name) {
        Target::Contextual
    } else {
        Target::Unknown
    }
}

/// The command with this name.
fn named_command(name: &str) -> Option<&'static Command> {
    vocabulary().commands.iter().find(|c| c.name == name)
}

/// Wake words in force: the user's if configured, otherwise the defaults.
pub fn wake_words() -> &'static [&'static str] {
    USER_WAKE_WORDS.get().map_or(DEFAULT_WAKE_WORDS, |w| w.as_slice())
}

/// Confidence required to act.
fn threshold() -> f32 {
    *USER_THRESHOLD.get().unwrap_or(&DEFAULT_THRESHOLD)
}

/// Every application, from every layer of the vocabulary.
fn all_apps() -> impl Iterator<Item = &'static App> {
    vocabulary().apps.iter()
}

/// Minimum similarity for a phrase to count as a command.
///
/// Raising it means more commands are missed; lowering it means commands
/// fire that were never spoken. With an always-on microphone, firing
/// wrongly is far worse than missing one, so this errs high.
pub const DEFAULT_THRESHOLD: f32 = 0.7;

/// Verbs that introduce an application name, in canonical form.
///
/// Compared against [`text::keywords`] output, so "abre", "ábreme" and
/// "abrir" all arrive here as "abrir".
const APP_VERBS: &[&str] = &[
    "abrir", "ir", "cambiar", "poner", "dar", "traer", "mostrar", "enfocar",
    "sacar", "lanzar", "buscar",
];

/// Verbs asking for an application to be closed.
const QUIT_VERBS: &[&str] = &["cerrar", "salir", "matar", "terminar"];

/// Words after which a song, album or artist name is expected.
const MUSIC_NOUNS: &[&str] = &["cancion", "tema", "disco", "album", "grupo", "artista"];

/// Verbs that introduce text to be typed out.
///
/// Deliberately narrow. "poner" was tried here and had to go: "pon la
/// pantalla completa" is a command, and treating it as dictation typed the
/// rest of the sentence instead of running it.
const DICTATION_VERBS: &[&str] = &["escribir", "anotar", "apuntar", "dictar"];

/// Top-level domains recognised when a web address is spoken.
///
/// The recogniser writes "google.com" with the dot, which normalisation
/// turns into two words, so a domain arrives here as "google com". Saying
/// it out loud gives "google punto com", handled the same way.
const TLDS: &[&str] = &["com", "es", "org", "net", "io", "dev", "app", "co", "ai"];

/// What a command does once it is understood.
///
/// Everything a vocabulary file can ask for, and nothing more: the file
/// names an action, the action itself is Rust. `Script` is reachable only
/// through [`crate::vocabulary`]'s fixed list of named actions, never
/// written out in a file.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    Key(u16, Mods),
    Volume(i32),
    Mute(bool),
    Script(&'static str),
    /// Type a fixed string into whatever has focus.
    Type(&'static str),
    /// Open a web address.
    Open(&'static str),
    /// Stop acting on commands until resumed from the menu bar.
    Sleep,
}

pub struct Command {
    /// Ways of saying it. The first is canonical and gets documented.
    pub phrases: &'static [&'static str],
    /// Stable identifier, also shown in the log.
    pub name: &'static str,
    pub action: Action,
    /// Which vocabulary file it came from, for the catalogue.
    pub category: &'static str,
}

/// An application, with the ways people actually say its name.
pub struct App {
    pub name: &'static str,
    pub bundle_id: &'static str,
    /// Includes what the recogniser really produces, not just correct
    /// spellings: "cromo" is what comes out of saying Chrome in Spanish.
    pub aliases: &'static [&'static str],
    pub category: &'static str,
}

/// A page common enough to name without its domain.
pub struct Site {
    /// Normalised, since it is matched against a word of the sentence.
    pub name: &'static str,
    pub url: &'static str,
    pub category: &'static str,
}

/// A command that only exists inside particular applications.
///
/// Kept apart from the global list rather than adding a context field to
/// every entry: most commands are global, and this way the exceptions are
/// visible in one place. A contextual command beats a global one with the
/// same phrase, which is what lets an app reinterpret a general word. In a
/// vocabulary file it is a `[[commands]]` entry with a `bundles` key.
pub struct ContextualCommand {
    /// Bundle identifiers this applies in.
    pub bundles: &'static [&'static str],
    pub phrases: &'static [&'static str],
    pub name: &'static str,
    pub action: Action,
    pub category: &'static str,
}

/// Browsers, for deciding where a link should open.
///
/// Stays in Rust rather than moving into `browsers.toml`: it is not a
/// phrase anybody says, it is the rule that a link opens in the browser you
/// are already working in.
const BROWSERS: &[&str] = &["com.google.Chrome", "com.apple.Safari", "org.mozilla.firefox"];

/// A named sequence of phrases, defined in `config.toml`.
///
/// Only phrases, deliberately: `shortcut = "…"` and `keys = "…"` steps were
/// considered and dropped. Everything a step could do is already sayable
/// — including running a shortcut, once that lands — so a second kind of
/// step would just be another way to write the same thing. Packs cannot
/// define one: a macro presses keys and launches applications on its own
/// say-so, which is not something data that may have been downloaded gets
/// to do.
#[derive(Debug, PartialEq, Eq)]
pub struct Macro {
    /// Shown in the log when it runs.
    pub name: &'static str,
    /// Ways of asking for it.
    pub phrases: &'static [&'static str],
    /// What to do, in order — each one a phrase Minion would understand on
    /// its own, wake word added back on before it is decided.
    pub steps: &'static [&'static str],
}

/// Named macros in force. Empty until [`configure`] has read them.
fn macros() -> &'static [Macro] {
    MACROS.get().map_or(&[], |m| m.as_slice())
}

/// Search engines a bare or targeted "busca X" can reach, and the URL a
/// query slots into. The first is the default when nothing else is named.
///
/// Lives in code rather than as a `[[sites]]` entry in
/// `vocabulary/sites.toml`: that table only carries a plain URL an app
/// opens as it is, with no place for where a query goes. A `search =
/// "https://…?q={}"` field there is a reasonable next step, once more than
/// the web needs one — kept here until then.
const SEARCH_ENGINES: &[(&str, &str)] = &[
    ("google", "https://www.google.com/search?q={}"),
    ("youtube", "https://www.youtube.com/results?search_query={}"),
    ("wikipedia", "https://es.wikipedia.org/w/index.php?search={}"),
    ("amazon", "https://www.amazon.es/s?k={}"),
];

/// Words that ask for a Finder search instead of one of [`SEARCH_ENGINES`].
const FINDER_WORDS: &[&str] = &["finder", "buscador"];

/// The engine a bare "busca X" reaches: the user's, if `search_engine` in
/// `config.toml` named a real one, otherwise the first of
/// [`SEARCH_ENGINES`].
fn default_search_engine() -> &'static str {
    USER_SEARCH_ENGINE.get().copied().unwrap_or(SEARCH_ENGINES[0].0)
}

/// What was decided, before anything has been done about it.
///
/// Deciding and acting are deliberately separate: it makes the vocabulary
/// testable without applications opening for real.
/// Words that join two instructions in one sentence.
///
/// Only explicit joiners. A bare "y" appears inside titles and dictated
/// text — "pon la canción tú y yo" is one instruction, not two.
const CHAIN_JOINERS: &[&str] = &[" y luego ", " y después ", " y despues ", " y ahora ", " y también ", " y tambien "];

/// Spoken numbers. The recogniser writes digits for some and words for
/// others depending on the sentence, so both forms are here.
const NUMBERS: &[(&str, usize)] = &[
    ("una", 1), ("uno", 1), ("primera", 1), ("1", 1),
    ("dos", 2), ("segunda", 2), ("2", 2),
    ("tres", 3), ("tercera", 3), ("3", 3),
    ("cuatro", 4), ("cuarta", 4), ("4", 4),
    ("cinco", 5), ("quinta", 5), ("5", 5),
    ("seis", 6), ("sexta", 6), ("6", 6),
    ("siete", 7), ("septima", 7), ("7", 7),
    ("ocho", 8), ("octava", 8), ("8", 8),
    ("nueve", 9), ("novena", 9), ("9", 9),
];

/// A number spoken anywhere in the sentence.
fn number_in(words: &[String]) -> Option<usize> {
    words
        .iter()
        .find_map(|word| NUMBERS.iter().find(|(name, _)| name == word))
        .map(|(_, value)| *value)
}

/// Commands that take a number: one entry instead of one per value.
///
/// "pestaña 7" used to need its own table row, so the family stopped at
/// five and every new one was another line to write.
struct Numbered {
    /// Words that must appear, besides the number itself.
    subject: &'static [&'static str],
    name: &'static str,
    /// Turns the number into the key to press.
    key_for: fn(usize) -> Option<(u16, Mods)>,
}

const NUMBERED: &[Numbered] = &[
    Numbered {
        subject: &["pestana"],
        name: "ir a la pestaña",
        key_for: |n| {
            // ⌘1 to ⌘8 select tabs; ⌘9 is the last one, not the ninth.
            let code = match n {
                1 => key::DIGIT_1,
                2 => key::DIGIT_2,
                3 => key::DIGIT_3,
                4 => key::DIGIT_4,
                5 => key::DIGIT_5,
                6 => 22,
                7 => 26,
                8 => 28,
                _ => return None,
            };
            Some((code, Mods::CMD))
        },
    },
];

/// Matches "pestaña 7" and the like.
fn numbered_command(words: &[String]) -> Option<(&'static str, usize, (u16, Mods))> {
    let number = number_in(words)?;
    for entry in NUMBERED {
        let mentions_subject = entry
            .subject
            .iter()
            .all(|needed| words.iter().any(|word| word == needed));
        if !mentions_subject {
            continue;
        }
        if let Some(key) = (entry.key_for)(number) {
            return Some((entry.name, number, key));
        }
    }
    None
}

/// Most times a command will be repeated in one go.
const MAX_REPEATS: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Launch or focus an application.
    Launch { name: &'static str, bundle_id: &'static str },
    /// Quit an application.
    Quit { name: &'static str, bundle_id: &'static str },
    /// Open a web address. `in_browser` names the browser to use when one
    /// is already in front, so a link does not jump to a different app.
    Browse { url: String, in_browser: Option<&'static str> },
    /// Run a command that only exists in the current application.
    RunHere(&'static str),
    /// Type text into whatever has focus.
    Type(String),
    /// Look something up in Spotify.
    SearchMusic(String),
    /// Do the last thing again, this many times.
    Again(usize),
    /// A command that takes a number: which one, and what to press.
    Numbered { name: &'static str, number: usize, key: (u16, Mods) },
    /// Start typing everything said from now on.
    StartDictation,
    /// Stop doing that.
    StopDictation,
    /// Undo whatever Minion last did.
    UndoLast,
    /// A question, to be answered aloud.
    Answer(crate::answers::Question),
    /// Run a command from the table, identified by name.
    Run(&'static str),
    /// Run an installed Apple Shortcut, by its own name.
    Shortcut(String),
    /// Run a named macro: several phrases, in order.
    Macro(&'static Macro),
    /// Search for something in the Finder.
    SearchFinder(String),
    /// Started with the wake word, but nothing was recognised.
    Unrecognised,
    /// Not addressed to the machine.
    Ignored,
}

/// Whether the first word was meant to be the wake word.
///
/// Matched loosely, and it took a log to see why: everything else in the
/// vocabulary is matched with tolerance, while the word that gates all of
/// it was compared literally. "Minion" comes back as "Minial" or "Mini"
/// often enough that whole sentences were being discarded after being
/// understood perfectly.
///
/// The tolerance is bounded — one edit, and the first four letters must
/// agree — so an ordinary word cannot open a command by accident. Two
/// edits were tried and had to go: "mínimo", "mínima", "minie" and
/// "minuto" all reached "minion" that way, and "Minuto abre Chrome"
/// opened Chrome. What the recogniser really writes two edits away is
/// listed above instead, which is explicit and cannot spread.
fn sounds_like_wake_word(word: &str) -> bool {
    let words = wake_words();
    if words.contains(&word) {
        return true;
    }
    words.iter().any(|wake| {
        // Short wake words have no room for tolerance: "mini" is one edit
        // from "mina", "mino" and "mixi", all of them ordinary speech.
        if wake.chars().count() < 5 || word.chars().count() < 4 {
            return false;
        }
        const PREFIX: usize = 4;
        let same_start = word.chars().take(PREFIX).eq(wake.chars().take(PREFIX));
        same_start && crate::text::edits_between(word, wake) <= 1
    })
}

/// Strips the wake word. Returns `None` if the sentence is not a command.
///
/// Handles the wake word arriving as two words. The recogniser splits
/// "Minion" into "Mini on" often enough to matter, and the stray half then
/// sits at the front of the command and stops it matching anything.
pub fn strip_wake_word(phrase: &str) -> Option<&str> {
    let mut words = phrase.split_whitespace();
    let first = words.next()?;
    let rest = phrase[first.len()..].trim();

    // The two halves of a split wake word are tried before the first word
    // on its own: "mine" is not close enough to "minion" to be accepted by
    // itself, and it should not be — only "mine on" is.
    if let Some(second) = words.next() {
        let joined = format!("{first}{second}");
        let split_wake = wake_words()
            .iter()
            .any(|wake| joined == *wake || crate::text::edits_between(&joined, wake) <= 1);
        // Only when joining actually produces the wake word: "mini on"
        // does, "minion abre" does not.
        if split_wake {
            return Some(rest[second.len()..].trim());
        }
    }

    sounds_like_wake_word(first).then_some(rest)
}

/// Reads "otra vez", "repite", "hazlo tres veces" and the like.
///
/// Returns how many times. Absent a number, once.
fn repeat_request(rest: &str) -> Option<usize> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let asks_again = rest.contains("otra vez")
        || rest.starts_with("repite")
        || rest.starts_with("repitelo")
        || rest.starts_with("hazlo");
    if !asks_again {
        return None;
    }
    // "tres veces" — the number sits just before "veces".
    let times = words
        .iter()
        .position(|w| *w == "veces")
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| words.get(i))
        .and_then(|word| NUMBERS.iter().find(|(name, _)| name == word))
        .map_or(1, |(_, n)| *n);
    Some(times.min(MAX_REPEATS))
}

/// Whether the sentence asks for what follows to be typed out.
///
/// Wake word, then a dictation verb. Everything after that is content, so
/// nothing in it may be read as a command — chaining included.
fn is_dictation_phrase(transcript: &str) -> bool {
    let words: Vec<&str> = transcript.split_whitespace().collect();
    let (Some(first), Some(second)) = (words.first(), words.get(1)) else {
        return false;
    };
    if !wake_words().contains(&normalise(first).as_str()) {
        return false;
    }
    let verb = spanish::canonical_verb(&normalise(second)).to_string();
    DICTATION_VERBS.contains(&verb.as_str())
}

/// Splits a sentence that holds more than one instruction.
///
/// The wake word is carried onto each part, since only the first was
/// spoken with it: "minion cierra la pestaña y luego recarga" becomes
/// two sentences that each stand on their own.
///
/// Nothing is split while dictating, and nothing is split unless the first
/// half is itself an instruction — it opens with the wake word and is not
/// a dictation. Otherwise "escribe hola y luego adiós" lost its text and
/// the words the user was dictating came back as commands.
pub fn split_chain(transcript: &str, dictating: bool) -> Vec<String> {
    if dictating {
        return vec![transcript.to_string()];
    }
    let lowered = transcript.to_lowercase();
    let Some(joiner) = CHAIN_JOINERS.iter().find(|j| lowered.contains(*j)) else {
        return vec![transcript.to_string()];
    };
    // Find where the joiner sits in the original, to keep its casing.
    let Some(at) = lowered.find(*joiner) else {
        return vec![transcript.to_string()];
    };
    let head = transcript[..at].trim().to_string();
    let tail = transcript[at + joiner.len()..].trim();

    let Some(wake) = head.split_whitespace().next() else {
        return vec![transcript.to_string()];
    };
    // Only an instruction can be chained: the head has to be addressed to
    // Minion, and must not be dictation.
    if strip_wake_word(&normalise(&head)).is_none() || is_dictation_phrase(&head) {
        return vec![transcript.to_string()];
    }
    let mut parts = vec![head.clone()];
    // The rest may itself be a chain.
    for piece in split_chain(&format!("{wake} {tail}"), false) {
        parts.push(piece);
    }
    parts
}

/// Extracts a song, album or artist name from the sentence.
///
/// Taken from the raw transcript so the title keeps its accents and
/// capitals — "Vértigo" is not "vertigo" when it reaches Spotify.
fn music_query(transcript: &str) -> Option<String> {
    let words: Vec<&str> = transcript.split_whitespace().collect();
    let normalised: Vec<String> = words.iter().map(|w| normalise(w)).collect();

    if normalised.first().is_none_or(|w| !wake_words().contains(&w.as_str())) {
        return None;
    }
    let position = normalised
        .iter()
        .position(|w| MUSIC_NOUNS.contains(&w.as_str()))?;
    let title = words.get(position + 1..)?.join(" ");
    let title = title.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    (!title.is_empty()).then_some(title)
}

/// What "busca X" asked for: where to look, besides what.
enum SearchRequest {
    /// A ready web address, engine already resolved.
    Web(String),
    /// Look in the Finder instead of on the web.
    Finder(String),
}

/// Percent-encodes a query for a URL.
///
/// Mirrors `actions::search_spotify`'s own encoding, which is private to
/// that module and built for a `spotify:` URI rather than a `?q=`.
fn url_encode(query: &str) -> String {
    query
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_string()
            } else if c == ' ' {
                "+".to_string()
            } else {
                let mut buffer = [0u8; 4];
                c.encode_utf8(&mut buffer)
                    .bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect()
            }
        })
        .collect()
}

/// Extracts a search request: "busca X en Google", "busca X en el
/// Finder", or a bare "busca X" for the default engine.
///
/// Works from the raw transcript, not [`keywords`]: "en" and "el", which
/// mark where to search, are exactly the words `keywords` strips as
/// filler. The query keeps its original casing and accents for the same
/// reason [`music_query`] and [`dictation_text`] do — it is content headed
/// for a search box, not something matched against the vocabulary.
fn search_request(transcript: &str) -> Option<SearchRequest> {
    let words: Vec<&str> = transcript.split_whitespace().collect();
    let normalised: Vec<String> = words.iter().map(|w| normalise(w)).collect();

    if normalised.first().is_none_or(|w| !wake_words().contains(&w.as_str())) {
        return None;
    }
    if spanish::canonical_verb(normalised.get(1)?) != "buscar" || words.len() < 3 {
        return None;
    }

    // "en <engine>", found from the end — a query is free text and may
    // itself contain the word "en" ("busca cuando el amor en la ciudad").
    if let Some(en_at) = normalised.iter().rposition(|w| w == "en") {
        if en_at > 1 && en_at + 1 < normalised.len() {
            let mut engine_at = en_at + 1;
            if matches!(normalised[engine_at].as_str(), "el" | "la") {
                engine_at += 1;
            }
            let named_last = engine_at == normalised.len() - 1;
            if named_last {
                if let Some(engine) = normalised.get(engine_at) {
                    let query = words[2..en_at].join(" ");
                    let query = query.trim_matches(|c: char| !c.is_alphanumeric() && c != ' ');
                    if !query.is_empty() {
                        if FINDER_WORDS.contains(&engine.as_str()) {
                            return Some(SearchRequest::Finder(query.to_string()));
                        }
                        if let Some((_, template)) =
                            SEARCH_ENGINES.iter().find(|(name, _)| name == engine)
                        {
                            let url = template.replace("{}", &url_encode(query));
                            return Some(SearchRequest::Web(url));
                        }
                    }
                }
            }
        }
    }

    // No "en …" naming a known engine: the whole remainder is the query,
    // for the default one.
    let query = words[2..].join(" ");
    let query = query.trim_matches(|c: char| !c.is_alphanumeric() && c != ' ');
    if query.is_empty() {
        return None;
    }
    let template = SEARCH_ENGINES
        .iter()
        .find(|(name, _)| *name == default_search_engine())
        .map_or(SEARCH_ENGINES[0].1, |(_, t)| t);
    Some(SearchRequest::Web(template.replace("{}", &url_encode(query))))
}

/// Extracts text to be typed, if the sentence asks for dictation.
///
/// Works on the original transcript rather than the normalised form: what
/// gets typed must keep its accents and capitals. Only the first two words
/// are inspected — wake word, then dictation verb — and everything after
/// them is content, however much it looks like a command.
fn dictation_text(transcript: &str) -> Option<String> {
    let words: Vec<&str> = transcript.split_whitespace().collect();
    if words.len() < 3 || !is_dictation_phrase(transcript) {
        return None;
    }
    let text = words[2..].join(" ");
    // What the old four-character floor was guarding against — "pon la
    // música" becoming a request to type "la música" — is now handled by
    // the verb list, which holds no verb that opens a command as well.
    // Any text at all is text: "minion escribe sí" means sí.
    (!text.trim().is_empty()).then_some(text)
}

/// What an edit command asks for, said while dictating instead of more
/// text to type. Purely a description — [`crate::dictation::Transformer`]
/// is the one that knows what was actually typed, so it is the one that
/// turns this into an exact edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditIntent {
    /// «borra la última palabra» / «borra eso»
    DeleteLastWord,
    /// «borra la última frase»
    DeleteLastPhrase,
    /// «cambia X por Y», exactly as heard — case and accents kept, so the
    /// retyped text still looks dictated rather than transcribed.
    Replace { find: String, replace: String },
}

/// Recognises an edit command, said while dictating instead of more text
/// to type. Only when the edit phrase is the *whole* utterance (the wake
/// word optional): "borra la última palabra que dije" has extra words of
/// its own and is dictated content, not obeyed.
///
/// Works on the original words, not the normalised form: «cambia X por
/// Y» must retype Y with its accents and capitals, the same reason
/// [`dictation_text`] keeps the original transcript.
pub fn dictation_edit(part: &str) -> Option<EditIntent> {
    let normalised = normalise(part);
    let normalised_word_count = normalised.split_whitespace().count();
    let rest = strip_wake_word(&normalised).unwrap_or(&normalised);
    let stripped = normalised_word_count - rest.split_whitespace().count();
    let words: Vec<&str> = part.split_whitespace().skip(stripped).collect();
    if words.is_empty() {
        return None;
    }

    for phrase in ["borra la ultima palabra", "borra eso"] {
        if keywords(rest) == keywords(phrase) {
            return Some(EditIntent::DeleteLastWord);
        }
    }
    if keywords(rest) == keywords("borra la ultima frase") {
        return Some(EditIntent::DeleteLastPhrase);
    }

    dictation_replace(&words)
}

/// «cambia X por Y»: the verb, then whatever comes before the *last* «por»
/// is what to find, and whatever comes after it is what to type instead —
/// the last one, so "por" is free to appear inside X itself.
fn dictation_replace(words: &[&str]) -> Option<EditIntent> {
    let first = words.first()?;
    if spanish::canonical_verb(&normalise(first)) != "cambiar" {
        return None;
    }
    let rest = &words[1..];
    let sep = rest.iter().rposition(|w| normalise(w) == "por")?;
    let find = rest[..sep].join(" ");
    let replace = rest[sep + 1..].join(" ");
    (!find.is_empty() && !replace.is_empty())
        .then_some(EditIntent::Replace { find, replace })
}

/// The browser in front, if the application in front is one.
///
/// Returns the entry from [`BROWSERS`] so the value is `'static` and can
/// travel inside a `Decision`.
fn browser_in_front(context: Option<&str>) -> Option<&'static str> {
    let bundle = context?;
    BROWSERS.iter().copied().find(|b| *b == bundle)
}

/// Finds a web address in the sentence.
///
/// Two shapes: a spelled-out domain ("google com", "studiolxd punto es") or
/// one of the sites in the vocabulary named on its own.
/// A web address found in the sentence, and how it was written.
enum Website {
    /// A spelled-out domain: unmistakable, so no verb is needed.
    Domain(String),
    /// One of the known sites, named on its own. Needs an opening verb, or
    /// "youtube" in the middle of any sentence would navigate.
    Named(String),
}

fn find_website(transcript: &str, words: &[String]) -> Option<Website> {
    // A written domain, taken from the raw transcript so the dot survives.
    // Normalising first would turn "hora.es" and "qué hora es" into the
    // same three words, and the second is a question, not an address.
    for token in transcript.split_whitespace() {
        let cleaned = token
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        let Some((name, tld)) = cleaned.rsplit_once('.') else {
            continue;
        };
        if TLDS.contains(&tld) && !name.is_empty() && !name.contains('.') {
            return Some(Website::Domain(format!("https://{name}.{tld}")));
        }
    }

    // A domain spoken aloud: "github punto com".
    for (i, word) in words.iter().enumerate() {
        if TLDS.contains(&word.as_str()) && i >= 2 && words[i - 1] == "punto" {
            let name = words[i - 2].as_str();
            if !spanish::is_known_verb(name) {
                return Some(Website::Domain(format!("https://{name}.{word}")));
            }
        }
    }

    // A site named without its domain.
    words
        .iter()
        .find_map(|w| vocabulary().sites.iter().find(|site| site.name == w))
        .map(|site| Website::Named(site.url.to_string()))
}

/// Whether the words of `alias` appear, in order, as whole words of the
/// sentence.
///
/// Whole words, not a substring: "mail" is inside "gmail" and "orca" is
/// inside "mallorca", and both used to launch an application instead of
/// opening the page that was asked for.
fn names_alias(words: &[&str], alias: &str) -> bool {
    let wanted: Vec<&str> = alias.split_whitespace().collect();
    if wanted.is_empty() || wanted.len() > words.len() {
        return false;
    }
    words.windows(wanted.len()).any(|window| window == wanted)
}

/// Whether `word` is the alias with another whole word stuck to it.
///
/// The recogniser runs words together — "abrecrome" is "abre" + "crome" —
/// and the application name is still in there. The leftover has to be a
/// word in its own right, which is what tells "abrecrome" apart from
/// "editorial" ("editor" plus "ial") and "gmail" ("g" plus "mail").
fn run_together(word: &str, alias: &str) -> bool {
    if word.len() <= alias.len() {
        return false;
    }
    let Some(at) = word.find(alias) else {
        return false;
    };
    let head = &word[..at];
    let tail = &word[at + alias.len()..];
    let is_word = |part: &str| {
        part.is_empty()
            || spanish::is_known_verb(part)
            || spanish::is_filler(part)
            || sounds_like_wake_word(part)
    };
    is_word(head) && is_word(tail)
}

/// Whether a spoken word is one recogniser slip away from the alias.
///
/// Word against word, never against the sentence: that is what keeps
/// "gmail" from being "mail" and "mallorca" from being "orca", which is
/// how the old substring search read them. Bounded on both sides — a
/// single edit, the same first two letters, and only for names long
/// enough that one edit cannot turn them into a different word.
fn near_alias(word: &str, alias: &str) -> bool {
    const MIN_LENGTH: usize = 5;
    if alias.contains(' ') || alias.chars().count() < MIN_LENGTH {
        return false;
    }
    let same_start = word.chars().take(2).eq(alias.chars().take(2));
    same_start && crate::text::edits_between(word, alias) <= 1
}

/// Whether some run of whole words in the sentence *sounds* like the alias.
///
/// The names are English and the recogniser writes Spanish, so it picks a
/// different spelling for the same sounds every time: "chrome", "crome",
/// "cromo", "croma". [`crate::text::phonetic`] reduces both sides to those
/// sounds, and one comparison then covers every spelling of them —
/// including the ones nobody has said yet, which is what listing them one
/// by one can never do.
///
/// Whole words and equality, never a substring or a near miss: everything
/// this is allowed to forgive is already forgiven inside `phonetic`, and
/// anything looser would undo the work `near_alias` did to keep "gmail"
/// out of Mail. Very short sounds are refused for the same reason — at
/// three letters, two different words share a form too easily.
fn sounds_alias(words: &[&str], alias: &str) -> bool {
    const MIN_SOUNDS: usize = 4;
    let wanted = crate::text::phonetic(alias);
    if wanted.chars().count() < MIN_SOUNDS {
        return false;
    }
    let spread = alias.split_whitespace().count();
    if spread == 0 || spread > words.len() {
        return false;
    }
    words
        .windows(spread)
        .any(|window| crate::text::phonetic(&window.join(" ")) == wanted)
}

/// How much of a word sounds like the alias, from 0 to 1.
///
/// Only ever a guess to put to the user, never grounds for acting: it says
/// "most of these sounds agree", which is true of words that are not the
/// same word. [`sounds_alias`] is the one that decides, and it asks for
/// every sound.
fn sounds_near(word: &str, alias: &str) -> f32 {
    let (heard, wanted) = (crate::text::phonetic(word), crate::text::phonetic(alias));
    let longest = heard.chars().count().max(wanted.chars().count());
    // Short words share sounds by accident, and `edits_between` gives up
    // (returning something enormous) once the two lengths are three apart,
    // which is the point at which one is not a mishearing of the other.
    // Two sounds wrong is the ceiling, the same one the rest of the
    // matching keeps: past that the word is not a mishearing of the name
    // but a different word, and a question about it is noise.
    const MOST_EDITS: usize = 2;
    let edits = crate::text::edits_between(&heard, &wanted);
    if longest < 4 || edits > MOST_EDITS {
        return 0.0;
    }
    // Kept below every grade `find_app` acts on, so a guess can never
    // outrank a real match when the two are compared.
    (1.0 - edits as f32 / longest as f32).min(0.75)
}

/// Finds an application named in the sentence, with its match score.
///
/// Whole words only, in four grades: named outright, run together with
/// another word, said so that it sounds like the name, or one slip away
/// from it. Bigger mangles stay in the alias lists — what the recogniser
/// really writes ("shafari", "grum") is listed, which is explicit and
/// cannot spread.
fn find_app(rest: &str) -> Option<(&'static App, f32)> {
    best_app(rest, false).filter(|(_, score)| *score >= threshold())
}

/// The application a sentence comes closest to naming, and how closely.
///
/// The same search as [`find_app`] without the threshold, so a caller that
/// only wants to *ask* about a guess can see one that was too weak to act
/// on. `guessing` adds a fifth, softer grade below the other four: how
/// much of the word sounds right, a spread rather than a step, so a guess
/// can be ranked against the commands — "abre za fari" is worth mentioning
/// and "abre cron" is not. It is never available to `find_app`, because a
/// word that mostly sounds right is not grounds for doing anything.
fn best_app(rest: &str, guessing: bool) -> Option<(&'static App, f32)> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut best: Option<(&App, f32)> = None;
    for app in all_apps() {
        for alias in app.aliases {
            // A plain mention beats a run-together one, which beats one
            // that only sounds right, which beats a misheard one; longer
            // aliases beat shorter ones, so "vs code" wins over a stray
            // "code".
            let score = if names_alias(&words, alias) {
                0.9 + (alias.len() as f32 / 100.0).min(0.09)
            } else if words.iter().any(|word| run_together(word, alias)) {
                0.9
            } else if sounds_alias(&words, alias) {
                0.85
            } else if words.iter().any(|word| near_alias(word, alias)) {
                0.8
            } else if guessing {
                words.iter().map(|word| sounds_near(word, alias)).fold(0.0, f32::max)
            } else {
                0.0
            };
            if score > 0.0 && best.is_none_or(|(_, b)| score > b) {
                best = Some((app, score));
            }
        }
    }
    best
}

/// Whether the sentence names one of the known sites outright.
///
/// Checked before applications: a site's own name must not be eaten by an
/// application alias that happens to be part of it.
fn names_a_site(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| vocabulary().sites.iter().any(|site| site.name == word))
}

/// Whichever of the table wins the race for a phrase: a built-in or
/// user-defined command, or a macro. Kept as one `Option` in `decide_in`
/// so a macro is exactly as strong a match as a command — neither shadows
/// the other just for being checked first.
#[derive(Clone, Copy)]
enum TableHit {
    Command(&'static Command),
    Macro(&'static Macro),
}

/// Works out what a transcription means with no application context.
pub fn decide(transcript: &str) -> (Decision, f32) {
    decide_in(transcript, None)
}

/// Works out what a transcription means inside a given application.
///
/// `context` is a bundle identifier. Contextual commands are tried first:
/// inside Terminal, "limpia" is ⌃L; anywhere else it means nothing.
pub fn decide_in(transcript: &str, context: Option<&str>) -> (Decision, f32) {
    let normalised = normalise(transcript);
    let Some(rest) = strip_wake_word(&normalised) else {
        return (Decision::Ignored, 0.0);
    };
    if rest.is_empty() {
        // Just the wake word. Not a failure to understand — nothing was
        // asked — so it should not chirp or count as an error.
        return (Decision::Ignored, 0.0);
    }

    // Dictation first: everything after the verb is content, not a command,
    // so it must not be matched against the vocabulary at all.
    if let Some(text) = dictation_text(transcript) {
        return (Decision::Type(text), 1.0);
    }

    // The dictation mode's own switches, and undo. Checked here because
    // they are answered by the caller, which is what holds the state.
    // Stopping is checked before starting: «fin del dictado» contains the
    // one-word way in, «dictado», and must not be taken for it.
    let switches: [(&[&str], Decision); 3] = [
        (&["deja de dictar", "fin del dictado"], Decision::StopDictation),
        (&["empieza a dictar", "modo dictado", "dictado"], Decision::StartDictation),
        (&["deshaz lo que has hecho", "anula eso"], Decision::UndoLast),
    ];
    for (phrases, decision) in switches {
        for phrase in phrases {
            if similarity(rest, phrase) >= threshold() {
                return (decision, 1.0);
            }
        }
    }

    // Commands belonging to the application in front come first: they are
    // the most specific thing that can match.
    if let Some(bundle) = context {
        let mut best_here: Option<(&ContextualCommand, f32)> = None;
        for command in &vocabulary().contextual {
            if !command.bundles.contains(&bundle) {
                continue;
            }
            for phrase in command.phrases {
                let score = similarity(rest, phrase);
                if score >= threshold() && best_here.is_none_or(|(_, b)| score > b) {
                    best_here = Some((command, score));
                }
            }
        }
        if let Some((command, score)) = best_here {
            return (Decision::RunHere(command.name), score);
        }
    }

    // Table commands next: they are more specific than "open something".
    // The user's own are searched alongside the built-in ones, and so are
    // macros — a macro is just as much the user's own as a `[[commands]]`
    // entry, so it races the same way for the same phrase.
    let mut best: Option<(TableHit, f32)> = None;
    for command in &vocabulary().commands {
        for phrase in command.phrases {
            let score = similarity(rest, phrase);
            if score >= threshold() && best.is_none_or(|(_, b)| score > b) {
                best = Some((TableHit::Command(command), score));
            }
        }
    }
    for macro_ in macros() {
        for phrase in macro_.phrases {
            let score = similarity(rest, phrase);
            if score >= threshold() && best.is_none_or(|(_, b)| score > b) {
                best = Some((TableHit::Macro(macro_), score));
            }
        }
    }

    // "otra vez", "repite dos veces": refers to whatever came before, so it
    // is resolved by the caller, which is the only place that remembers.
    if let Some(times) = repeat_request(rest) {
        return (Decision::Again(times), 1.0);
    }

    // A named song, but only if nothing in the vocabulary fits. Otherwise
    // "pon la canción anterior" would search for a track called "anterior"
    // instead of going back one.
    if best.is_none() {
        if let Some(query) = music_query(transcript) {
            return (Decision::SearchMusic(query), 1.0);
        }
    }

    // An installed Apple Shortcut, named outright. Checked here, at the
    // same standing as a table command, since "atajo …" is as explicit a
    // marker as a phrase from the table.
    if best.is_none() {
        if let Some(name) = shortcuts::requested_name(rest) {
            if let Some(installed) = shortcuts::find(name) {
                return (Decision::Shortcut(installed), 1.0);
            }
        }
    }

    // "busca X [en …]": before applications, so "busca chrome en google"
    // is a search and not an attempt to launch Chrome — "en" is as
    // explicit a marker as "atajo" is for a shortcut.
    if best.is_none() {
        if let Some(request) = search_request(transcript) {
            return match request {
                SearchRequest::Web(url) => {
                    (Decision::Browse { url, in_browser: browser_in_front(context) }, 0.9)
                }
                SearchRequest::Finder(query) => (Decision::SearchFinder(query), 0.9),
            };
        }
    }

    // Phrasings the user added, or that were learned from the log. The
    // target may be any command with that name, the user's own included.
    for (name, phrase) in USER_ALIASES.get().into_iter().flatten() {
        let score = similarity(rest, phrase);
        if score < threshold() || !best.is_none_or(|(_, b)| score > b) {
            continue;
        }
        if let Some(command) = named_command(name) {
            best = Some((TableHit::Command(command), score));
        } else if let Some(bundle) = context {
            // A contextual command only exists where it applies, so this
            // is the one place it can be reached by name.
            if vocabulary()
                .contextual
                .iter()
                .any(|c| c.name == *name && c.bundles.contains(&bundle))
            {
                return (Decision::RunHere(name), score);
            }
        }
    }

    // Applications. The leading verb decides what happens to the one named:
    // opening it, closing it, or nothing at all. Without this check, "cierra
    // Safari" would launch Safari, because the name alone used to be enough.
    let spoken_words = keywords(rest);

    // Questions, once nothing in the table fits. After, not before: "deja
    // de escuchar" is an instruction and "¿me escuchas?" is a question,
    // and asking first let the question take both.
    if best.is_none() {
        if let Some(question) = crate::answers::asked(rest, threshold()) {
            return (Decision::Answer(question), 1.0);
        }
    }

    // A number in the sentence: "pestaña 7". After the plain table, so a
    // command that matches outright still wins, and before applications,
    // which would otherwise see only a stray digit.
    if best.is_none() {
        if let Some((name, number, key)) = numbered_command(&spoken_words) {
            return (Decision::Numbered { name, number, key }, 1.0);
        }
    }

    let leading_verb = spoken_words.first().map(String::as_str);
    let asks_to_open = leading_verb.is_some_and(|v| APP_VERBS.contains(&v));
    let asks_to_quit = leading_verb.is_some_and(|v| QUIT_VERBS.contains(&v));
    // A verb we know that asks for neither: the sentence is about something
    // else, even if an application happens to be named in it.
    let other_verb = leading_verb
        .is_some_and(|v| spanish::is_known_verb(v) && !asks_to_open && !asks_to_quit);

    // A web address beats an application name: "abre github" means the site,
    // since there is no GitHub app in the table to confuse it with.
    //
    // A spelled-out domain does not need a verb in front. The recogniser
    // runs words together — "abremarca.com", "iramarca.com" — and there is
    // then no verb left to recognise, but the intent is unmistakable.
    // Worked out once: the website route and the application route both
    // need to know, and asking twice invites the two to disagree.
    let app = if names_a_site(&spoken_words) {
        None
    } else {
        find_app(rest)
    };

    if app.is_none() {
        match find_website(transcript, &spoken_words) {
            Some(Website::Domain(url)) => {
                return (Decision::Browse { url, in_browser: browser_in_front(context) }, 0.9)
            }
            Some(Website::Named(url)) if asks_to_open => {
                return (Decision::Browse { url, in_browser: browser_in_front(context) }, 0.9)
            }
            _ => {}
        }
    }

    if let Some((app, score)) = app {
        let app_wins = best.is_none_or(|(_, b)| score > b);
        if asks_to_quit && app_wins {
            return (
                Decision::Quit { name: app.name, bundle_id: app.bundle_id },
                score,
            );
        }
        // Named without a verb ("minion, Safari") still means open it.
        let named_outright = score > 0.85 && leading_verb.is_none_or(|v| !spanish::is_known_verb(v));
        if (asks_to_open || named_outright) && !other_verb && app_wins {
            return (
                Decision::Launch { name: app.name, bundle_id: app.bundle_id },
                score,
            );
        }
    }

    match best {
        Some((TableHit::Command(command), score)) => (Decision::Run(command.name), score),
        Some((TableHit::Macro(macro_), score)) => (Decision::Macro(macro_), score),
        None => (Decision::Unrecognised, 0.0),
    }
}

/// The result of carrying out a decision.
pub struct Done {
    pub description: String,
    /// `Err` when the action was refused by the system — in practice, a
    /// keystroke dropped for want of Accessibility permission — carrying
    /// why, so the caller's BLOCKED line can say more than "no".
    pub outcome: Result<(), String>,
}

/// Carries out a decision. Returns what was done, and whether it worked.
pub fn perform(decision: &Decision) -> Option<Done> {
    match decision {
        Decision::Launch { name, bundle_id } => Some(Done {
            description: format!("abrir {name}"),
            outcome: actions::open_app(bundle_id),
        }),
        Decision::Quit { name, bundle_id } => Some(Done {
            description: format!("cerrar {name}"),
            outcome: actions::quit_app(bundle_id),
        }),
        Decision::Browse { url, in_browser } => Some(Done {
            description: match in_browser {
                Some(bundle_id) => {
                    let name = vocabulary()
                        .apps
                        .iter()
                        .find(|a| a.bundle_id == *bundle_id)
                        .map_or(*bundle_id, |a| a.name);
                    format!("abrir {url} en {name}")
                }
                None => format!("abrir {url}"),
            },
            outcome: actions::open_url(url, *in_browser),
        }),
        // Answered by the caller, which holds the state they need.
        Decision::StartDictation
        | Decision::StopDictation
        | Decision::UndoLast
        | Decision::Answer(_) => None,
        Decision::Numbered { name, number, key } => Some(Done {
            description: format!("{name} {number}"),
            outcome: actions::press(key.0, key.1),
        }),
        Decision::SearchMusic(query) => Some(Done {
            description: format!("buscar «{query}» en Spotify"),
            outcome: actions::search_spotify(query),
        }),
        Decision::Type(text) => Some(Done {
            description: format!("escribir «{text}»"),
            outcome: actions::type_text(text),
        }),
        Decision::RunHere(name) => {
            let command = vocabulary().contextual.iter().find(|c| c.name == *name)?;
            Some(Done {
                description: (*name).to_string(),
                outcome: run_action(command.action),
            })
        }
        Decision::Run(name) => {
            let command = vocabulary().commands.iter().find(|c| c.name == *name)?;
            Some(Done {
                description: (*name).to_string(),
                outcome: run_action(command.action),
            })
        }
        Decision::SearchFinder(query) => Some(Done {
            description: format!("buscar «{query}» en el Finder"),
            outcome: run_finder_search(query),
        }),
        // Logged in its own line, not the generic "ran … -> …" one, the
        // same way `heard`, `voice` and `blank` are: `None` here is what
        // stops `report` in main.rs from adding a second one.
        Decision::Shortcut(name) => {
            crate::journal::write(&format!("shortcut «{name}»"));
            if let Err(reason) = shortcuts::run(name) {
                crate::journal::write(&format!("BLOCKED  shortcut «{name}»: {reason}"));
            }
            None
        }
        Decision::Macro(macro_) => {
            run_macro(macro_);
            None
        }
        _ => None,
    }
}

/// Opens a Finder search window and fills it in.
///
/// There is no CLI for "open a Finder search with this text" the way
/// `open -b` covers applications: Spotlight (⌘Space) searches everything,
/// not just files, and may be remapped besides, while Finder's own "Find"
/// (⌘F) is scoped to the frontmost window, which is what a spoken "busca X
/// en el Finder" most plausibly means — the folder you are already
/// looking at, not the whole disk. Simulated because there is nothing to
/// script: activate Finder, press ⌘F, type the query. `keystroke` sends
/// Unicode text, the same as `actions::type_text` does through Core
/// Graphics, so accents survive regardless of keyboard layout.
fn run_finder_search(query: &str) -> Result<(), String> {
    actions::applescript(&format!(
        "tell application \"Finder\" to activate\n\
         tell application \"System Events\"\n\
         keystroke \"f\" using {{command down}}\n\
         delay 0.3\n\
         keystroke \"{}\"\n\
         end tell",
        actions::applescript_string(query)
    ))
}

/// What trying one macro step decided.
enum StepResult {
    Ok,
    /// The step itself named another macro.
    Recursive,
    /// The step could not be carried out, or named nothing at all.
    Refused,
}

/// Decides and carries out one macro step, for real.
///
/// Split out from [`run_macro_steps`] so the sequencing — order, stopping
/// at the first refusal, the recursion guard — is testable on its own,
/// with a stand-in for this that touches nothing on the machine.
fn try_macro_step(step: &str) -> StepResult {
    let wake = wake_words().first().copied().unwrap_or("minion");
    let (decision, _) = decide(&format!("{wake} {step}"));
    if matches!(decision, Decision::Macro(_)) {
        return StepResult::Recursive;
    }
    match perform(&decision) {
        Some(done) if done.outcome.is_ok() => StepResult::Ok,
        _ => StepResult::Refused,
    }
}

/// Runs a macro's steps in order, stopping at the first one that fails.
///
/// `try_step` decides what a step means and, unless it names another
/// macro, carries it out. Taken as a parameter rather than calling
/// [`try_macro_step`] directly so a test can supply one that only records
/// what it was asked to do.
fn run_macro_steps(macro_: &Macro, mut try_step: impl FnMut(&str) -> StepResult) {
    let total = macro_.steps.len();
    for (i, step) in macro_.steps.iter().enumerate() {
        crate::journal::write(&format!("macro    {}: {}/{total} {step}", macro_.name, i + 1));
        match try_step(step) {
            StepResult::Ok => {}
            StepResult::Recursive => {
                crate::journal::write(&format!(
                    "macro    {}: a macro cannot call another macro — stopped",
                    macro_.name
                ));
                return;
            }
            StepResult::Refused => {
                crate::journal::write(&format!(
                    "macro    {}: stopped at {}/{total} — «{step}» was refused",
                    macro_.name,
                    i + 1
                ));
                return;
            }
        }
        // A gap between steps, not before the first or after the last.
        // Skipped under test: nothing here should make the suite slow.
        if i + 1 < total && !cfg!(test) {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
}

fn run_macro(macro_: &Macro) {
    run_macro_steps(macro_, try_macro_step);
}

fn run_action(action: Action) -> Result<(), String> {
    match action {
        Action::Key(code, mods) => actions::press(code, mods),
        Action::Volume(delta) => actions::adjust_volume(delta),
        Action::Mute(muted) => actions::set_muted(muted),
        Action::Script(script) => actions::applescript(script),
        Action::Type(text) => actions::type_text(text),
        Action::Open(url) => actions::open_url(url, None),
        Action::Sleep => Ok(()),
    }
}

/// Whether this decision asks Minion to stop listening.
pub fn is_sleep(decision: &Decision) -> bool {
    matches!(decision, Decision::Run(name) if *name == "dormir")
}

/// The command a phrase most resembles, ignoring the confidence threshold.
///
/// Used by `minion learn` to suggest what a misheard phrase was probably
/// meant to be. Deliberately separate from [`decide_in`]: this one always
/// answers, which is useful for a suggestion and dangerous for an action.
pub fn closest_command(phrase: &str) -> Option<(&'static str, f32)> {
    let normalised = normalise(phrase);
    let rest = strip_wake_word(&normalised).unwrap_or(&normalised);
    let mut best: Option<(&'static str, f32)> = None;
    for command in &vocabulary().commands {
        for candidate in command.phrases {
            let score = similarity(rest, candidate);
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((command.name, score));
            }
        }
    }
    best
}

/// A guess at the application a phrase was trying to name.
pub struct AppGuess {
    /// What would be done about it, read off the verb: "cierra za fari"
    /// asks to quit, anything else to open.
    pub decision: Decision,
    /// The application's name, as the catalogue spells it.
    pub app: &'static str,
    /// The word that nearly named it — the one worth learning, rather than
    /// the whole sentence it was said in.
    pub spoken: String,
    pub score: f32,
}

/// The application a phrase most nearly names, ignoring the threshold.
///
/// The companion to [`closest_command`], and used the same way: to suggest
/// what a phrase that was not understood was probably meant to be. It
/// always answers, which is useful for a question and dangerous for an
/// action, so nothing acts on it without being told to.
pub fn closest_app(phrase: &str) -> Option<AppGuess> {
    let normalised = normalise(phrase);
    let rest = strip_wake_word(&normalised).unwrap_or(&normalised);
    let (app, score) = best_app(rest, true)?;

    // Which of the words was it? The alias that scored is not necessarily
    // spelled like anything that was said, so the word is found again here.
    let spoken = rest
        .split_whitespace()
        .max_by(|a, b| {
            let sound = |word: &str| {
                app.aliases.iter().map(|alias| sounds_near(word, alias)).fold(0.0, f32::max)
            };
            sound(a).total_cmp(&sound(b))
        })?
        .to_string();

    let leading_verb = keywords(rest).first().cloned();
    let decision = if leading_verb.is_some_and(|v| QUIT_VERBS.contains(&v.as_str())) {
        Decision::Quit { name: app.name, bundle_id: app.bundle_id }
    } else {
        Decision::Launch { name: app.name, bundle_id: app.bundle_id }
    };
    Some(AppGuess { decision, app: app.name, spoken, score })
}

/// The whole vocabulary, written out for someone to read.
///
/// Generated from the tables rather than kept alongside them: a list of
/// commands that has to be updated by hand is a list that goes stale, and
/// the first thing anyone needs is to know what they can say.
/// The categories present in a list of entries, in the order they first
/// appear — which is the order the vocabulary files were loaded in.
fn categories_of<'a, T: 'a>(
    entries: impl Iterator<Item = &'a T>,
    category: impl Fn(&T) -> &'static str,
) -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for entry in entries {
        let name = category(entry);
        if !seen.contains(&name) {
            seen.push(name);
        }
    }
    seen
}

pub fn catalogue() -> String {
    use std::fmt::Write as _;
    let wake = wake_words().first().copied().unwrap_or("minion");
    let vocabulary = vocabulary();
    let mut out = String::new();

    let _ = writeln!(
        out,
        "Empieza siempre por «{wake}», y solo al principio de la frase.\n\
         No hace falta decirlo exacto: se ignoran los artículos y da igual \
         el tiempo del verbo.\n"
    );

    let _ = writeln!(out, "── APLICACIONES ─────────────────────\n");
    let _ = writeln!(
        out,
        "Para abrir: {}\nPara cerrar: {}\nO solo el nombre: «{wake}, Spotify»",
        APP_VERBS.join(", "),
        QUIT_VERBS.join(", ")
    );
    for category in categories_of(vocabulary.apps.iter(), |a| a.category) {
        let _ = writeln!(out, "\n  {category}");
        for app in vocabulary.apps.iter().filter(|a| a.category == category) {
            let _ = writeln!(out, "    {:<20} {}", app.name, app.aliases.join(" · "));
        }
    }

    let _ = writeln!(out, "\n── ÓRDENES ──────────────────────────");
    for category in categories_of(vocabulary.commands.iter(), |c| c.category) {
        let _ = writeln!(out, "\n  {category}");
        for command in vocabulary.commands.iter().filter(|c| c.category == category) {
            let _ = writeln!(
                out,
                "    {:<20} {}",
                command.name,
                command.phrases.join(" · ")
            );
        }
    }

    let _ = writeln!(out, "\n── SEGÚN DÓNDE ESTÉS ────────────────");
    for category in categories_of(vocabulary.contextual.iter(), |c| c.category) {
        let _ = writeln!(out, "\n  {category}");
        for command in vocabulary.contextual.iter().filter(|c| c.category == category) {
            let apps: Vec<&str> = command
                .bundles
                .iter()
                .map(|bundle| {
                    vocabulary
                        .apps
                        .iter()
                        .find(|a| a.bundle_id == *bundle)
                        .map_or(*bundle, |a| a.name)
                })
                .collect();
            let _ = writeln!(
                out,
                "    {:<20} {}\n    {:<20} en {}",
                command.name,
                command.phrases.join(" · "),
                "",
                apps.join(", ")
            );
        }
    }

    if !vocabulary.sites.is_empty() {
        let _ = writeln!(out, "\n── PÁGINAS POR SU NOMBRE ────────────");
        for category in categories_of(vocabulary.sites.iter(), |s| s.category) {
            let names: Vec<&str> = vocabulary
                .sites
                .iter()
                .filter(|s| s.category == category)
                .map(|s| s.name)
                .collect();
            let _ = writeln!(out, "\n  {category}\n    «{wake}, abre …»  {}", names.join(" · "));
        }
    }

    let _ = writeln!(
        out,
        "\n── ADEMÁS ───────────────────────────\n\n\
           escribir              «{wake}, escribe hola qué tal»\n\
           páginas web           «{wake}, ve a google.com»\n\
           música por nombre     «{wake}, pon la canción Vértigo»\n\
           repetir               «{wake}, otra vez» · «repite tres veces»\n\
           encadenar             «{wake}, cierra la pestaña y luego recarga»\n\
           dictado seguido       «{wake}, empieza a dictar» … «{wake}, deja de dictar»\n\
           deshacer lo suyo      «{wake}, deshaz lo que has hecho»\n\
           una pestaña concreta  «{wake}, pestaña 7»\n\
\n\
           Y preguntas, que responde en voz alta:\n\
           «{wake}, ¿qué hora es?» · «¿qué día es hoy?» · «¿cuánta batería queda?»\n\
           «{wake}, ¿qué volumen tengo?» · «¿me oyes?» · «¿qué he dicho hoy?»"
    );
    out
}

/// Total number of distinct phrases understood, for the startup banner.
pub fn phrase_count() -> usize {
    let from_commands: usize = vocabulary().commands.iter().map(|c| c.phrases.len()).sum();
    let from_apps: usize = all_apps()
        .map(|a| a.aliases.len() * (APP_VERBS.len() + 1))
        .sum();
    from_commands + from_apps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(phrase: &str) -> Decision {
        decide(phrase).0
    }

    fn launches(phrase: &str, expected: &str) {
        match decision(phrase) {
            Decision::Launch { name, .. } => assert_eq!(name, expected, "for «{phrase}»"),
            other => panic!("«{phrase}» should launch {expected}, got {other:?}"),
        }
    }

    #[test]
    fn launches_applications() {
        launches("Minion, abre Chrome.", "Chrome");
        launches("Minion, vete a Safari.", "Safari");
        launches("Minion, tráeme la terminal.", "Terminal");
        // Named outright, without a verb.
        launches("Minion, Spotify.", "Spotify");
    }

    #[test]
    fn survives_recogniser_slips() {
        // Both seen in the real log.
        launches("Minion Abrecrome.", "Chrome");
        launches("minion abre cromo", "Chrome");
    }

    fn quits(phrase: &str, expected: &str) {
        match decision(phrase) {
            Decision::Quit { name, .. } => assert_eq!(name, expected, "for «{phrase}»"),
            other => panic!("«{phrase}» should quit {expected}, got {other:?}"),
        }
    }

    #[test]
    fn closes_applications() {
        // Straight from the log: these used to LAUNCH Safari, because
        // naming an application was enough regardless of the verb.
        quits("Minion cierra Safari.", "Safari");
        quits("Minion Cierra Safari.", "Safari");
        quits("Minion Sal de Safari.", "Safari");
        quits("minion cierra chrome", "Chrome");
        quits("minion sal de spotify", "Spotify");
    }

    #[test]
    fn naming_an_app_does_not_override_the_verb() {
        // The window and tab commands must survive an app name nearby.
        assert_eq!(decision("Minion cierra la pestaña."), Decision::Run("cerrar pestaña"));
        assert_eq!(decision("Minion cierra la ventana."), Decision::Run("cerrar ventana"));
        assert_eq!(decision("Minion minimiza la ventana."), Decision::Run("minimizar"));
        // The recogniser drifts into English halfway through the phrase.
        assert_eq!(decision("Minion minimized the ventana."), Decision::Run("minimizar"));
    }

    #[test]
    fn runs_table_commands() {
        assert_eq!(decision("Minion, guarda esto."), Decision::Run("guardar"));
        assert_eq!(decision("Minion, sube el volumen."), Decision::Run("subir volumen"));
        assert_eq!(decision("Minion, pantalla completa."), Decision::Run("pantalla completa"));
    }

    #[test]
    fn the_wake_word_survives_being_misheard() {
        // Straight from the log. Each of these was a command understood
        // perfectly and then thrown away, because the first word did not
        // match a short list letter for letter.
        for spoken in [
            "Minial Shafari.",
            "Mini Chrome.",
            "Minión, ¿qué hora es?",
            "Minio abre chrome",
            "Miñón abre safari",
            "Minions abre chrome",
            "Minium abre chrome",
            "Mine on abre chrome",
        ] {
            assert_ne!(
                decide(spoken).0,
                Decision::Ignored,
                "«{spoken}» should be taken as a command"
            );
        }
    }

    #[test]
    fn the_wake_word_survives_being_split_in_two() {
        // From the log: "Minion Safari" came back as "Mini on so fuddy",
        // and the stray "on" sat at the front of the command.
        assert_eq!(decide("Mini on abre Chrome").0, decide("Minion abre Chrome").0);
        assert_eq!(decide("mini on guarda esto").0, Decision::Run("guardar"));
    }

    #[test]
    fn a_real_second_word_is_not_eaten() {
        // Joining only happens when it produces the wake word. "minion
        // abre" must keep its verb.
        assert_eq!(decide("minion abre chrome").0, decide("minion chrome").0);
        assert_eq!(decide("minion guarda esto").0, Decision::Run("guardar"));
    }

    #[test]
    fn an_ordinary_word_does_not_open_a_command() {
        // The tolerance has to stop somewhere, or conversation starts
        // running things. One edit and four shared letters is the limit:
        // everything in the second group used to open commands, and
        // "minuto abre Chrome" really did open Chrome.
        for spoken in [
            "millón de gracias",
            "misión cumplida",
            "camión abre chrome",
            "opinión abre chrome",
            "mínimo abre chrome",
            "minuto abre chrome",
            "mina abre chrome",
            "minas abre chrome",
            "minero abre chrome",
            "mínima abre chrome",
            "minie abre chrome",
        ] {
            assert_eq!(
                decide(spoken).0,
                Decision::Ignored,
                "«{spoken}» should not be a command"
            );
        }
    }

    #[test]
    fn ignores_what_is_not_addressed_to_it() {
        // The most important property: with an always-on microphone, most
        // of what is heard is conversation, not instruction.
        assert_eq!(decision("Mañana quedamos a las cinco."), Decision::Ignored);
        assert_eq!(decision("¿Has visto el partido?"), Decision::Ignored);
        assert_eq!(decision("Voy a abrir Chrome a ver qué pasa."), Decision::Ignored);
        assert_eq!(decision("Le dije al ordenador que abriera Chrome."), Decision::Ignored);
        assert_eq!(decision("guarda esto en el cajón"), Decision::Ignored);
    }

    #[test]
    fn admits_when_it_does_not_understand() {
        // Better to do nothing than to guess.
        assert_eq!(decision("Minion, haz un pino."), Decision::Unrecognised);
        assert_eq!(decision("Minion, ponme un café."), Decision::Unrecognised);
    }

    #[test]
    fn the_same_instruction_can_be_phrased_many_ways() {
        // The table holds one natural phrase per command; these are the
        // renderings a person actually produces for the same intent.
        let cases: &[(&str, &str)] = &[
            ("cierra la ventana", "cerrar ventana"),
            ("cierra la ventana", "cierra ventana"),
            ("cierra la ventana", "cierra esta ventana"),
            ("cierra la ventana", "cierra la ventana por favor"),
            ("guarda esto", "guarda"),
            ("guarda esto", "guardar"),
            ("guarda esto", "guarda el archivo"),
            ("copia esto", "copiar"),
            ("sube el volumen", "sube volumen"),
            ("sube el volumen", "subir el volumen"),
            ("recarga la pagina", "recargar"),
            ("recarga la pagina", "actualiza la pagina"),
            ("bloquea la pantalla", "bloquear pantalla"),
            ("minimiza la ventana", "minimizar"),
            ("selecciona todo", "seleccionar todo"),
        ];
        for (canonical, spoken) in cases {
            let expected = vocabulary()
                .commands
                .iter()
                .find(|c| c.phrases.contains(canonical))
                .unwrap_or_else(|| panic!("«{canonical}» is not in the table"));
            let heard = format!("minion {spoken}");
            match decide(&heard).0 {
                Decision::Run(name) if name == expected.name => {}
                other => panic!("«{spoken}» should reach «{}», got {other:?}", expected.name),
            }
        }
    }

    #[test]
    fn applications_take_many_verbs() {
        for spoken in ["abre chrome", "abreme chrome", "ve a chrome",
                       "vete a chrome", "cambia a chrome", "ponme chrome",
                       "traeme chrome", "saca chrome", "abrir chrome"] {
            launches(&format!("minion {spoken}"), "Chrome");
        }
    }

    #[test]
    fn phrases_that_failed_in_the_log_now_work() {
        // Straight from ~/Library/Logs/minion.log, where each of these came
        // back "not understood".
        let cases: &[(&str, &str)] = &[
            ("minion pestaña anterior", "pestaña anterior"),

            ("Minion página atrás.", "atrás"),
            ("Minion página anterior.", "atrás"),
            ("Minion página siguiente.", "adelante"),
            ("Minion retroceder página.", "atrás"),
        ];
        for (spoken, expected) in cases {
            match decide(spoken).0 {
                Decision::Run(name) if name == *expected => {}
                other => panic!("«{spoken}» should be «{expected}», got {other:?}"),
            }
        }
    }

    fn browses(phrase: &str, expected: &str) {
        match decision(phrase) {
            Decision::Browse { url, .. } => assert_eq!(url, expected, "for «{phrase}»"),
            other => panic!("«{phrase}» should open {expected}, got {other:?}"),
        }
    }

    #[test]
    fn a_link_opens_where_you_are_working() {
        // In a browser, the page belongs in that browser rather than in
        // whichever one the system considers default.
        match decide_in("minion abre youtube", Some("com.google.Chrome")).0 {
            Decision::Browse { in_browser, .. } => {
                assert_eq!(in_browser, Some("com.google.Chrome"));
            }
            other => panic!("expected a page, got {other:?}"),
        }
        // Outside a browser there is nothing to prefer, so the default wins.
        for elsewhere in [None, Some("com.apple.Terminal"), Some("com.apple.finder")] {
            match decide_in("minion abre youtube", elsewhere).0 {
                Decision::Browse { in_browser, .. } => {
                    assert_eq!(in_browser, None, "from {elsewhere:?}");
                }
                other => panic!("expected a page from {elsewhere:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn opens_web_addresses() {
        // From the log: this came back not understood.
        browses("Minion ir a google.com", "https://google.com");
        browses("minion abre studiolxd.es", "https://studiolxd.es");
        // Spoken aloud, the dot becomes a word.
        browses("minion ve a github punto com", "https://github.com");
    }

    #[test]
    fn a_domain_needs_no_verb() {
        // From the log: the recogniser runs the words together, leaving no
        // verb to recognise — but ".com" makes the intent unmistakable.
        browses("Minion abremarca.com", "https://abremarca.com");
        browses("Minion iramarca.com", "https://iramarca.com");
        browses("Minion ir a Marca.com", "https://marca.com");
    }

    #[test]
    fn the_wake_word_alone_is_not_an_error() {
        assert_eq!(decision("Minion."), Decision::Ignored);
    }

    #[test]
    fn opens_well_known_sites_by_name() {
        browses("minion abre youtube", "https://www.youtube.com");
        browses("minion ve a wikipedia", "https://es.wikipedia.org");
    }

    #[test]
    fn one_slip_in_the_name_still_finds_the_app() {
        // What the log is full of: the name almost right. A whole word
        // one edit away is the limit, and it scores below a name said
        // properly, so a real mention always wins.
        launches("minion abre safary", "Safari");
        launches("minion abre cromm", "Chrome");
        launches("minion abre spotifi", "Spotify");
        // Two edits is a different word, and so is a different opening.
        assert_eq!(decision("minion abre sarasa"), Decision::Unrecognised);
    }

    #[test]
    fn an_app_name_inside_a_word_is_not_that_app() {
        // All three used to launch an application: "mail" is inside
        // "gmail", "orca" inside "mallorca", "editor" inside "editorial".
        browses("minion abre gmail", "https://mail.google.com");
        browses("minion ve a gmail punto com", "https://gmail.com");
        browses("minion ve a mallorca punto com", "https://mallorca.com");
        // Neither can the misheard-name route bring them back: "gmail" is
        // an insertion away from "mail" but starts elsewhere, and
        // "mallorca" is four edits from "orca".
        assert!(!near_alias("gmail", "mail"));
        assert!(!near_alias("mallorca", "orca"));
        assert!(!near_alias("editorial", "editor"));
        assert_eq!(decision("minion abre el editorial"), Decision::Unrecognised);
    }

    #[test]
    fn applications_still_win_over_sites() {
        // Chrome is an app in the table; it must not become a web search.
        launches("minion abre chrome", "Chrome");
        launches("minion abre safari", "Safari");
    }

    #[test]
    fn the_same_words_mean_different_things_in_different_places() {
        // This is the point of the contextual table: one phrase, read
        // according to where you are, rather than a separate name per app.
        let cases: &[(&str, Option<&str>, &str)] = &[
            // "borra esto": backspace normally, to the Trash in Finder.
            ("borra esto", None, "borrar"),
            ("borra esto", Some("com.google.Chrome"), "borrar"),
            ("borra esto", Some("com.apple.finder"), "a la papelera"),
            // "cancela": Escape normally, ⌃C in a terminal.
            ("cancela esto", None, "cancelar"),
            ("cancela esto", Some("com.apple.Terminal"), "interrumpir"),
            // "sube del todo": scroll, folder up, or previous command.
            ("sube del todo", None, "ir arriba"),
            ("sube del todo", Some("com.apple.finder"), "carpeta superior"),
            ("sube del todo", Some("com.apple.Terminal"), "orden anterior"),
        ];
        for (phrase, context, expected) in cases {
            let spoken = format!("minion {phrase}");
            let name = match decide_in(&spoken, *context).0 {
                Decision::Run(name) | Decision::RunHere(name) => name,
                other => panic!("«{spoken}» in {context:?} gave {other:?}"),
            };
            assert_eq!(name, *expected, "«{phrase}» in {context:?}");
        }
    }

    #[test]
    fn commands_belong_to_their_application() {
        // Inside Terminal these mean something; nowhere else do they.
        assert_eq!(
            decide_in("minion limpia la pantalla", Some("com.apple.Terminal")).0,
            Decision::RunHere("limpiar terminal")
        );
        assert_eq!(
            decide_in("minion limpia la pantalla", Some("com.google.Chrome")).0,
            Decision::Unrecognised
        );
        assert_eq!(
            decide_in("minion limpia la pantalla", None).0,
            Decision::Unrecognised
        );
    }

    #[test]
    fn the_same_phrase_can_differ_by_application() {
        // Chrome and Finder both know "nueva ventana", but only Finder
        // knows "nueva carpeta".
        assert_eq!(
            decide_in("minion nueva carpeta", Some("com.apple.finder")).0,
            Decision::RunHere("carpeta nueva")
        );
        assert_eq!(
            decide_in("minion abre una ventana nueva", Some("com.apple.finder")).0,
            Decision::Run("ventana nueva")
        );
    }

    #[test]
    fn global_commands_still_work_inside_an_application() {
        assert_eq!(
            decide_in("minion guarda esto", Some("com.apple.Terminal")).0,
            Decision::Run("guardar")
        );
    }

    #[test]
    fn every_contextual_command_recognises_itself() {
        for command in &vocabulary().contextual {
            for phrase in command.phrases {
                let spoken = format!("minion {phrase}");
                let bundle = command.bundles[0];
                match decide_in(&spoken, Some(bundle)).0 {
                    Decision::RunHere(name) if name == command.name => {}
                    other => panic!("«{spoken}» in {bundle} should be «{}», got {other:?}",
                                    command.name),
                }
            }
        }
    }

    fn types(phrase: &str, expected: &str) {
        match decision(phrase) {
            Decision::Type(text) => assert_eq!(text, expected, "for «{phrase}»"),
            other => panic!("«{phrase}» should type «{expected}», got {other:?}"),
        }
    }

    #[test]
    fn dictates_text() {
        types("Minion escribe hola qué tal estás",  "hola qué tal estás");
        types("Minion, anota comprar pan mañana", "comprar pan mañana");
        // Accents and capitals survive: the text comes from the original
        // transcript, not the normalised form used for matching.
        types("Minion escribe Señor Muñoz", "Señor Muñoz");
    }

    #[test]
    fn a_short_dictation_is_still_a_dictation() {
        // Four characters used to be the floor, so this was unrecognised.
        types("Minion escribe sí", "sí");
        types("Minion escribe no", "no");
        // What that floor was guarding against still holds: a verb that
        // also opens commands is not a dictation verb.
        assert_eq!(decision("Minion pon la música."), Decision::Run("reproducir"));
    }

    #[test]
    fn dictated_text_is_never_matched_as_a_command() {
        // The whole point: everything after the verb is content, however
        // much it looks like something in the vocabulary.
        types("Minion escribe cierra la ventana", "cierra la ventana");
        types("Minion escribe sube el volumen", "sube el volumen");
    }

    fn searches_music(phrase: &str, expected: &str) {
        match decision(phrase) {
            Decision::SearchMusic(query) => assert_eq!(query, expected, "for «{phrase}»"),
            other => panic!("«{phrase}» should search «{expected}», got {other:?}"),
        }
    }

    #[test]
    fn finds_music_by_name() {
        // From the log, where it came back not understood.
        searches_music("Minion reproduce la canción vértigo.", "vértigo");
        searches_music("minion pon la canción Bohemian Rhapsody", "Bohemian Rhapsody");
        searches_music("minion pon el disco Kind of Blue", "Kind of Blue");
        searches_music("minion pon el grupo Radiohead", "Radiohead");
    }

    #[test]
    fn plain_music_commands_are_not_searches() {
        // No title follows, so these stay transport controls.
        assert_eq!(decision("Minion pon la música."), Decision::Run("reproducir"));
        assert_eq!(decision("Minion para la música."), Decision::Run("pausar"));
        assert_eq!(
            decision("Minion pon la siguiente canción."),
            Decision::Run("canción siguiente")
        );
    }

    #[test]
    fn naming_the_music_opens_spotify() {
        // "para" used to be dropped as a filler, which left "para la
        // música" and "música" looking like the same thing.
        launches("Minion música.", "Spotify");
        launches("Minion la música.", "Spotify");
        assert_eq!(decision("Minion para la música."), Decision::Run("pausar"));
    }

    #[test]
    fn short_phrases_stay_commands() {
        // "pon la música" must not become a request to type "la música".
        assert_eq!(decision("Minion pon la música."), Decision::Run("reproducir"));
    }

    #[test]
    fn many_ways_to_ask_for_quiet() {
        for phrase in [
            "deja de escuchar", "duérmete", "duerme", "silénciate",
            "apágate", "descansa", "cállate",
        ] {
            let spoken = format!("minion {phrase}");
            assert_eq!(
                decide(&spoken).0,
                Decision::Run("dormir"),
                "«{spoken}» should pause"
            );
        }
    }

    #[test]
    fn numbers_fill_a_single_command() {
        // One table entry rather than one per value: the family used to
        // stop at five because each number was another line to write.
        for (spoken, expected) in [
            ("minion pestaña 1", 1),
            ("minion pestaña 7", 7),
            ("minion ve a la pestaña tres", 3),
            ("minion pestaña octava", 8),
        ] {
            match decide(spoken).0 {
                Decision::Numbered { name, number, .. } => {
                    assert_eq!(name, "ir a la pestaña", "for «{spoken}»");
                    assert_eq!(number, expected, "for «{spoken}»");
                }
                other => panic!("«{spoken}» should be tab {expected}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_number_out_of_range_is_not_invented() {
        // ⌘9 is the last tab, not the ninth, so nine has no key of its own.
        assert_eq!(decide("minion pestaña 9").0, Decision::Unrecognised);
    }

    #[test]
    fn plain_tab_commands_still_win() {
        // These mention tabs without a number and must not be swallowed.
        assert_eq!(decide("minion cierra la pestaña").0, Decision::Run("cerrar pestaña"));
        assert_eq!(decide("minion última pestaña").0, Decision::Run("última pestaña"));
    }

    #[test]
    fn questions_do_not_swallow_commands() {
        // "deja de escuchar" is an instruction; "¿me escuchas?" is a
        // question. Asking first let the question take both.
        assert_eq!(decide("minion deja de escuchar").0, Decision::Run("dormir"));
        assert_eq!(
            decide("minion me escuchas").0,
            Decision::Answer(crate::answers::Question::Listening)
        );
    }

    #[test]
    fn answers_questions() {
        use crate::answers::Question;
        assert_eq!(decide("minion qué hora es").0, Decision::Answer(Question::Time));
        assert_eq!(decide("minion qué día es hoy").0, Decision::Answer(Question::Date));
        assert_eq!(
            decide("minion cuánta batería queda").0,
            Decision::Answer(Question::Battery)
        );
    }

    #[test]
    fn dictation_is_a_mode_of_its_own() {
        assert_eq!(decide("minion empieza a dictar").0, Decision::StartDictation);
        assert_eq!(decide("minion modo dictado").0, Decision::StartDictation);
        // The shortest way in: just the mode's name.
        assert_eq!(decide("minion dictado").0, Decision::StartDictation);
        assert_eq!(decide("minion deja de dictar").0, Decision::StopDictation);
        assert_eq!(decide("minion fin del dictado").0, Decision::StopDictation);
    }

    #[test]
    fn edit_commands_are_recognised_with_or_without_the_wake_word() {
        assert_eq!(dictation_edit("minion borra la última palabra"), Some(EditIntent::DeleteLastWord));
        assert_eq!(dictation_edit("borra la última palabra"), Some(EditIntent::DeleteLastWord));
        assert_eq!(dictation_edit("borra eso"), Some(EditIntent::DeleteLastWord));
        assert_eq!(dictation_edit("minion borra la última frase"), Some(EditIntent::DeleteLastPhrase));
        assert_eq!(dictation_edit("borra la última frase"), Some(EditIntent::DeleteLastPhrase));
        assert_eq!(
            dictation_edit("cambia mundo por planeta"),
            Some(EditIntent::Replace { find: "mundo".to_string(), replace: "planeta".to_string() })
        );
        assert_eq!(
            dictation_edit("minion cambia mundo por planeta"),
            Some(EditIntent::Replace { find: "mundo".to_string(), replace: "planeta".to_string() })
        );
    }

    #[test]
    fn cambia_keeps_case_and_accents_and_takes_the_last_por() {
        // "por" is a filler everywhere else, but here it is the separator
        // and must survive even when it is also part of what to find.
        assert_eq!(
            dictation_edit("cambia gato por perro por Águila"),
            Some(EditIntent::Replace {
                find: "gato por perro".to_string(),
                replace: "Águila".to_string()
            })
        );
    }

    #[test]
    fn a_sentence_that_merely_contains_the_edit_words_is_not_an_edit() {
        // Extra content words ("que dije") make this dictated text, not a
        // command: an edit command must be the whole utterance.
        assert_eq!(dictation_edit("borra la última palabra que dije"), None);
        assert_eq!(dictation_edit("minion borra la última palabra que dije"), None);
    }

    #[test]
    fn cambia_needs_both_a_find_and_a_replacement() {
        assert_eq!(dictation_edit("cambia mundo"), None);
        assert_eq!(dictation_edit("cambia por planeta"), None);
        assert_eq!(dictation_edit("minion cierra la ventana"), None);
        assert_eq!(dictation_edit("hola qué tal"), None);
    }

    #[test]
    fn undo_is_about_what_minion_did() {
        // Distinct from "deshaz el cambio", which is ⌘Z in the application.
        assert_eq!(decide("minion deshaz lo que has hecho").0, Decision::UndoLast);
        assert_eq!(decide("minion anula eso").0, Decision::UndoLast);
        assert_eq!(decide("minion deshaz el cambio").0, Decision::Run("deshacer"));
    }

    #[test]
    fn asks_to_repeat() {
        assert_eq!(decision("minion otra vez"), Decision::Again(1));
        assert_eq!(decision("minion repite"), Decision::Again(1));
        assert_eq!(decision("minion hazlo tres veces"), Decision::Again(3));
        assert_eq!(decision("minion repite dos veces"), Decision::Again(2));
    }

    #[test]
    fn a_repeat_is_capped() {
        // Spoken numbers stop at five; anything else falls back to once.
        assert_eq!(decision("minion repite cien veces"), Decision::Again(1));
    }

    #[test]
    fn splits_chained_instructions() {
        assert_eq!(
            split_chain("Minion cierra la pestaña y luego recarga", false),
            vec!["Minion cierra la pestaña", "Minion recarga"]
        );
        // Three in a row.
        assert_eq!(
            split_chain("Minion copia esto y luego abre Chrome y después pega esto", false),
            vec![
                "Minion copia esto",
                "Minion abre Chrome",
                "Minion pega esto"
            ]
        );
    }

    #[test]
    fn a_bare_y_does_not_split() {
        // Titles and dictated text are full of "y"; only explicit joiners
        // count, or "pon la canción tú y yo" would become two commands.
        assert_eq!(
            split_chain("Minion pon la canción tú y yo", false),
            vec!["Minion pon la canción tú y yo"]
        );
    }

    #[test]
    fn dictated_text_is_never_chopped_into_commands() {
        // The text is content: "y luego" belongs to it, not to Minion.
        assert_eq!(
            split_chain("Minion escribe hola y luego adiós", false),
            vec!["Minion escribe hola y luego adiós"]
        );
        // And while dictating, nothing is a chain at all — this used to
        // type "hola hola adiós".
        assert_eq!(
            split_chain("hola y luego adiós", true),
            vec!["hola y luego adiós"]
        );
        // A sentence not addressed to Minion is left whole as well.
        assert_eq!(
            split_chain("quedamos y luego vemos", false),
            vec!["quedamos y luego vemos"]
        );
    }

    #[test]
    fn each_part_of_a_chain_still_resolves() {
        let parts = split_chain("Minion cierra la pestaña y luego recarga", false);
        assert_eq!(decide(&parts[0]).0, Decision::Run("cerrar pestaña"));
        assert_eq!(decide(&parts[1]).0, Decision::Run("recargar"));
    }

    #[test]
    fn an_alias_target_is_resolved_before_it_is_trusted() {
        // A command of the user\'s own is merged into the vocabulary like
        // any other, so it resolves the same way a built-in one does.
        let config: Config = toml::from_str(
            r#"
            [[commands]]
            name = "lo mío"
            phrases = ["haz lo mío"]
            keys = "cmd-a"
            "#,
        )
        .expect("config should parse");
        let mut own = Vocabulary::built_in();
        own.merge_config(&config);

        assert_eq!(resolve_target(&own, "guardar"), Target::Global);
        assert_eq!(resolve_target(&own, "lo mío"), Target::Global);
        assert_eq!(resolve_target(&own, "interrumpir"), Target::Contextual);
        // The whole point: a name that resolves to nothing is found now,
        // not in silence at the microphone.
        assert_eq!(resolve_target(&own, "atras"), Target::Unknown);
        assert_eq!(resolve_target(&own, "atrás"), Target::Global);
        assert_eq!(resolve_target(&own, "lo mio"), Target::Unknown);
    }

    #[test]
    fn no_phrase_is_declared_twice() {
        // Sharing a phrase between commands makes the winner depend on table
        // order, which is a latent bug rather than a choice.
        let mut seen = std::collections::HashMap::new();
        for command in &vocabulary().commands {
            for phrase in command.phrases {
                if let Some(other) = seen.insert(*phrase, command.name) {
                    panic!("«{phrase}» belongs to both «{other}» and «{}»", command.name);
                }
            }
        }
    }

    #[test]
    fn every_command_recognises_itself() {
        // Each declared phrase must reach its own command. Catches entries
        // shadowed by a similar one elsewhere in the vocabulary.
        for command in &vocabulary().commands {
            for phrase in command.phrases {
                let spoken = format!("minion {phrase}");
                match decide(&spoken).0 {
                    Decision::Run(name) if name == command.name => {}
                    other => panic!("«{spoken}» should be «{}», got {other:?}", command.name),
                }
            }
        }
    }

    #[test]
    fn the_catalogue_is_grouped_by_where_the_vocabulary_came_from() {
        // The point of the categories: the list is long enough that an
        // undivided one is unreadable.
        let catalogue = catalogue();
        for category in ["macOS", "Navegadores", "Ofimática", "Música", "Desarrollo", "Webs"] {
            assert!(catalogue.contains(category), "«{category}» should have a heading");
        }
        // And it is still generated, not written by hand.
        for app in all_apps() {
            assert!(catalogue.contains(app.name), "{} should be listed", app.name);
        }
        for command in &vocabulary().commands {
            assert!(catalogue.contains(command.name), "{} should be listed", command.name);
        }
    }

    #[test]
    fn an_app_name_is_found_however_it_is_spelled() {
        // Not in any alias list, and not one edit from anything in one:
        // these only reach their application because they *sound* like it.
        // Every spelling the recogniser has produced so far is already
        // listed by hand, which is exactly what this stops being necessary.
        for (spoken, app) in [
            ("minion abre kromo", "Chrome"),
            ("minion abre crom", "Chrome"),
            ("minion abre zafari", "Safari"),
            ("minion abre safary", "Safari"),
            ("minion abre spotifai", "Spotify"),
            ("minion abre uasap", "WhatsApp"),
            ("minion abre klod", "Claude"),
            ("minion abre faynder", "Finder"),
        ] {
            assert_eq!(
                decide(spoken).0,
                Decision::Launch {
                    name: app,
                    bundle_id: all_apps().find(|a| a.name == app).expect("app").bundle_id,
                },
                "«{spoken}» should open {app}"
            );
        }
    }

    #[test]
    fn sounding_alike_is_not_enough_on_its_own() {
        // The three the substring search used to get wrong, plus the words
        // that merely rhyme with an application. Sounds are compared whole
        // word to whole word, so none of them reaches an application.
        for spoken in [
            "minion abre gmail punto com",
            "minion abre mallorca punto com",
            "minion abre marca punto com",
            "minion abre el codo",
            "minuto abre chrome",
        ] {
            assert!(
                !matches!(decide(spoken).0, Decision::Launch { .. }),
                "«{spoken}» should not open an application"
            );
        }
        // And the commands that live near an application name still win.
        assert_eq!(decision("minion cierra la ventana"), Decision::Run("cerrar ventana"));
        assert_eq!(decision("minion copia esto"), Decision::Run("copiar"));
    }

    #[test]
    fn a_name_that_only_sounds_right_scores_below_one_said_outright() {
        // The grades have to stay in order, or a mishearing outranks the
        // real thing when both are in the same sentence.
        let (_, said) = decide("minion abre chrome");
        let (_, sounded) = decide("minion abre kromo");
        assert!(sounded < said, "{sounded} should be below {said}");
        assert!(sounded >= threshold());
    }

    #[test]
    fn every_app_alias_reaches_its_app() {
        for app in all_apps() {
            for alias in app.aliases {
                let spoken = format!("minion abre {alias}");
                match decide(&spoken).0 {
                    Decision::Launch { name, .. } if name == app.name => {}
                    other => panic!("«{spoken}» should launch {}, got {other:?}", app.name),
                }
            }
        }
    }

    fn browses_to(phrase: &str, expected: &str) {
        match decision(phrase) {
            Decision::Browse { url, .. } => assert_eq!(url, expected, "for «{phrase}»"),
            other => panic!("«{phrase}» should open {expected}, got {other:?}"),
        }
    }

    #[test]
    fn searches_a_named_engine() {
        browses_to(
            "minion busca gatos en google",
            "https://www.google.com/search?q=gatos",
        );
        browses_to(
            "minion busca gatos en youtube",
            "https://www.youtube.com/results?search_query=gatos",
        );
        browses_to(
            "minion busca gatos en wikipedia",
            "https://es.wikipedia.org/w/index.php?search=gatos",
        );
        browses_to(
            "minion busca zapatillas en amazon",
            "https://www.amazon.es/s?k=zapatillas",
        );
    }

    #[test]
    fn a_multi_word_query_survives_the_engine_search() {
        browses_to(
            "minion busca gatos graciosos en google",
            "https://www.google.com/search?q=gatos+graciosos",
        );
    }

    #[test]
    fn a_bare_search_uses_the_default_engine() {
        // No `search_engine` configured in a test, so the default applies.
        browses_to("minion busca gatos", "https://www.google.com/search?q=gatos");
    }

    #[test]
    fn searches_the_finder_instead_of_the_web() {
        assert_eq!(
            decision("minion busca recibos en el finder"),
            Decision::SearchFinder("recibos".to_string())
        );
    }

    #[test]
    fn an_unknown_engine_falls_back_to_a_plain_search() {
        // "en el horno" names nothing Minion knows how to search, so the
        // whole sentence becomes the query instead of being discarded.
        browses_to(
            "minion busca pan en el horno",
            "https://www.google.com/search?q=pan+en+el+horno",
        );
    }

    #[test]
    fn a_search_with_nothing_to_look_for_is_not_a_search() {
        assert_eq!(decision("minion busca"), Decision::Unrecognised);
    }

    #[test]
    fn macro_steps_run_in_order() {
        let seen = std::cell::RefCell::new(Vec::new());
        let steps: &[&str] = &["abre Slack", "abre Chrome", "sube el volumen"];
        let macro_ = Macro { name: "modo trabajo", phrases: &["modo trabajo"], steps };
        run_macro_steps(&macro_, |step| {
            seen.borrow_mut().push(step.to_string());
            StepResult::Ok
        });
        assert_eq!(*seen.borrow(), steps.to_vec());
    }

    #[test]
    fn a_macro_stops_at_the_first_refused_step() {
        let seen = std::cell::RefCell::new(Vec::new());
        let steps: &[&str] = &["uno", "dos", "tres", "cuatro"];
        let macro_ = Macro { name: "prueba", phrases: &["prueba"], steps };
        run_macro_steps(&macro_, |step| {
            seen.borrow_mut().push(step.to_string());
            if step == "dos" { StepResult::Refused } else { StepResult::Ok }
        });
        assert_eq!(*seen.borrow(), vec!["uno".to_string(), "dos".to_string()]);
    }

    #[test]
    fn a_macro_cannot_call_another_macro() {
        let seen = std::cell::RefCell::new(Vec::new());
        let steps: &[&str] = &["uno", "atajo de otro macro", "tres"];
        let macro_ = Macro { name: "prueba", phrases: &["prueba"], steps };
        run_macro_steps(&macro_, |step| {
            seen.borrow_mut().push(step.to_string());
            if step.starts_with("atajo") { StepResult::Recursive } else { StepResult::Ok }
        });
        assert_eq!(*seen.borrow(), vec!["uno".to_string(), "atajo de otro macro".to_string()]);
    }

    #[test]
    fn an_empty_macro_runs_nothing() {
        let seen = std::cell::RefCell::new(Vec::new());
        let macro_ = Macro { name: "vacio", phrases: &["vacio"], steps: &[] };
        run_macro_steps(&macro_, |step| {
            seen.borrow_mut().push(step.to_string());
            StepResult::Ok
        });
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_step_nobody_understands_is_refused_not_ignored() {
        // `try_macro_step` never runs anything real in this suite — there
        // is no macro in the global table for a step to name, since no
        // test calls `configure`, so its `Decision::Macro` branch is only
        // exercised through `run_macro_steps`'s injected `StepResult`
        // above. This checks the other half: a step that names nothing at
        // all must stop the macro rather than being silently skipped.
        // Same phrase `admits_when_it_does_not_understand` already proves
        // decides to `Unrecognised`.
        assert!(matches!(try_macro_step("haz un pino"), StepResult::Refused));
    }
}

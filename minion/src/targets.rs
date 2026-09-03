//! Targets named out loud.
//!
//! Everything else in the vocabulary is a fixed phrase: «abre Chrome» is
//! one entry in a table, and the table is the whole of what can be said.
//! The commands here are different in kind — «abre la carpeta Dev»,
//! «dicta aquí» — because the target is a name that only exists on this
//! machine, at this moment, and no table can hold it.
//!
//! So the phrase is parsed into a *kind* and a *name*, and the name is
//! resolved against the machine: Spotlight for files and folders (see
//! [`crate::files`]), the Accessibility API for the text field in front
//! of you (see [`crate::ax`]).
//!
//! Two things follow from that, and both are deliberate:
//!
//!   * the parsing is pure and tested, the resolving is not. Everything
//!     below `parse_target` can be read as a function of its input;
//!     everything above it asks the machine;
//!   * resolving happens where the answer is needed, not where the phrase
//!     is understood. [`decide`] is the exception it has to be — it looks
//!     a name up so it can return a path to open — and the lookup is
//!     read-only, idempotent and time-bounded, so being asked the same
//!     question twice costs a second search and changes nothing.

use crate::answers::Question;
use crate::commands::Decision;
use crate::files::{self, Kind};
use crate::text::normalise;

/// The words that say a folder is meant, and the ones that say a file is.
const FOLDER_WORDS: &[&str] = &["carpeta", "directorio"];
const FILE_WORDS: &[&str] = &["archivo", "fichero", "documento"];

/// Verbs that mean "open this".
const OPEN_VERBS: &[&str] = &["abrir", "ir", "mostrar", "ver", "traer", "buscar"];

/// Words that turn a target into an existing command rather than a name:
/// «carpeta nueva», «ventana nueva», «la otra ventana».
const NOT_A_NAME: &[&str] = &[
    "nueva", "nuevo", "otra", "otro", "superior", "anterior", "siguiente", "actual", "privada",
    "incognito", "izquierda", "derecha", "arriba", "abajo", "pantalla", "esta", "ese", "esa",
];

/// What follows the marker word, as a name: the words after it, with
/// nothing but fillers and modifiers dropped.
///
/// `None` when nothing usable is left — «crea una carpeta» names no
/// folder, and «cierra la ventana» names no window.
fn name_after(words: &[&str], marker: usize) -> Option<String> {
    let name: Vec<&str> = words[marker + 1..]
        .iter()
        .copied()
        .skip_while(|word| matches!(*word, "de" | "del" | "la" | "el" | "los" | "las" | "un" | "una"))
        .collect();
    if name.is_empty() || name.iter().any(|word| NOT_A_NAME.contains(word)) {
        return None;
    }
    Some(name.join(" "))
}

/// Whether the sentence asks for something to be brought forward.
fn has_verb(words: &[&str], verbs: &[&str]) -> bool {
    words.iter().any(|word| verbs.contains(&crate::spanish::canonical_verb(word)))
}

/// «abre la carpeta Dev», «abre el archivo informe septiembre»: what kind
/// of thing was named, and what it is called.
///
/// Pure. `rest` is what [`crate::commands::decide_in`] has already
/// normalised and stripped the wake word from.
pub fn parse_target(rest: &str) -> Option<(Kind, String)> {
    let normalised = normalise(rest);
    let words: Vec<&str> = normalised.split_whitespace().collect();
    let marker = words.iter().position(|word| {
        FOLDER_WORDS.contains(word) || FILE_WORDS.contains(word)
    })?;
    if !has_verb(&words, OPEN_VERBS) {
        return None;
    }
    let kind = if FOLDER_WORDS.contains(&words[marker]) { Kind::Folder } else { Kind::File };
    Some((kind, name_after(&words, marker)?))
}

/// What «abre la carpeta X» means, if the sentence says that.
///
/// The one thing `commands::decide_in` calls in this module: everything
/// else here is reached through `answers.rs`.
pub fn decide(rest: &str, _transcript: &str, _context: Option<&str>) -> Option<(Decision, f32)> {
    let (kind, name) = parse_target(rest)?;
    let found = files::find(&name, kind);
    let Some(best) = found.first() else {
        crate::journal::write(&format!("targets  «{name}»  ->  nothing on this machine"));
        return Some((Decision::Answer(Question::NotFound(name)), 1.0));
    };
    crate::journal::write(&format!(
        "targets  «{name}»  ->  {} ({} more)",
        best.path,
        found.len() - 1
    ));
    // Two things with the same name in different folders cannot be told
    // apart from the phrase alone. There is no way to put the choice to
    // the user from here — the machinery for that is `session.rs`'s, and
    // it can only offer readings `commands::candidates` produced — so the
    // best one is opened and named out loud instead of opened silently:
    // a wrong guess is then obvious immediately rather than later.
    if found.len() > 1 && tied(&found) {
        return Some((Decision::Answer(Question::OpenTarget(best.path.clone())), 1.0));
    }
    Some((
        Decision::Browse { url: files::file_url(&best.path), in_browser: None },
        1.0,
    ))
}

/// Whether the second-best answer is as good as the best one.
///
/// The same name, the same kind of thing, and the same distance from
/// home: two folders both called Dev, one in Desarrollo and one in
/// Documentos. A copy buried deeper than the other is not a tie — the
/// nearer one is what anybody means.
fn tied(found: &[files::Found]) -> bool {
    match found {
        [best, next, ..] => {
            best.is_folder == next.is_folder
                && normalise(&best.name) == normalise(&next.name)
                && files::depth(&best.path) == files::depth(&next.path)
        }
        _ => false,
    }
}

/// Opens a path and says which one it opened, for when there was more
/// than one to choose from. Called by `answers.rs`.
pub fn open_target(path: &str) -> String {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let home = std::env::var("HOME").unwrap_or_default();
    match crate::actions::open_url(&files::file_url(path), None) {
        Ok(()) => format!("Abro {name} en {}.", files::spoken_location(path, &home)),
        Err(reason) => {
            crate::journal::write(&format!("targets  {path} refused: {reason}"));
            format!("No he podido abrir {name}.")
        }
    }
}

// ── The text field in front of you ──────────────────────────────────────

/// How deep to look for a text field, and how many elements to look at.
///
/// Both bounded: a window's element tree can be thousands of nodes deep
/// in a browser, and this runs on the listening thread with somebody
/// waiting to dictate into whatever it finds.
const MAX_DEPTH: usize = 6;
const MAX_ELEMENTS: usize = 500;

/// Puts the keyboard focus in something that can be typed into.
///
/// Does nothing when the focus is already in a text field, which is the
/// usual case — this exists for the other one, where the front window has
/// a field nobody clicked in yet.
///
/// Returns what got the focus, for the log. Best effort throughout: an
/// application that will not answer the Accessibility API leaves the
/// focus where it was, and dictation goes wherever it would have gone
/// anyway.
pub fn focus_text_field_here() -> Result<String, String> {
    let app = crate::ax::frontmost().ok_or("nothing is in front")?;
    let element = crate::ax::application(app.pid).ok_or("no accessibility permission")?;

    if let Some(focused) = element.element(crate::ax::FOCUSED_UI_ELEMENT) {
        if focused.is_text_input() {
            let role = focused.role().unwrap_or_default();
            return Ok(format!("{role} already focused in {}", app.name));
        }
    }

    let windows = element.elements(crate::ax::WINDOWS);
    let window = windows.into_iter().next().ok_or("no window to look in")?;
    let field = first_text_input(window).ok_or("no text field in the front window")?;
    let role = field.role().unwrap_or_default();
    field.focus()?;
    Ok(format!("{role} in {}", app.name))
}

/// The first thing that can be typed into, breadth-first from a window.
///
/// Breadth-first on purpose: the field somebody means is the one on the
/// window, not the one buried in a drawer six levels down, and the first
/// answer at the shallowest depth is nearly always right.
fn first_text_input(window: crate::ax::Element) -> Option<crate::ax::Element> {
    let mut queue: std::collections::VecDeque<(crate::ax::Element, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((window, 0));
    let mut seen = 0;
    while let Some((element, depth)) = queue.pop_front() {
        seen += 1;
        if seen > MAX_ELEMENTS {
            return None;
        }
        if depth > 0 && element.is_text_input() {
            return Some(element);
        }
        if depth < MAX_DEPTH {
            for child in element.elements(crate::ax::CHILDREN) {
                queue.push_back((child, depth + 1));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_named_after_the_word_carpeta() {
        assert_eq!(parse_target("abre la carpeta dev"), Some((Kind::Folder, "dev".to_string())));
        assert_eq!(
            parse_target("ve a la carpeta descargas"),
            Some((Kind::Folder, "descargas".to_string()))
        );
        assert_eq!(
            parse_target("muestrame el directorio proyectos"),
            Some((Kind::Folder, "proyectos".to_string()))
        );
    }

    #[test]
    fn a_file_is_named_after_the_word_archivo() {
        assert_eq!(
            parse_target("abre el archivo informe septiembre"),
            Some((Kind::File, "informe septiembre".to_string()))
        );
        assert_eq!(
            parse_target("abre el documento presupuesto"),
            Some((Kind::File, "presupuesto".to_string()))
        );
    }

    #[test]
    fn what_the_table_already_answers_is_left_alone() {
        // Every one of these is an existing command, and reading it as a
        // name would take it away from the command that answers it.
        assert_eq!(parse_target("crea una carpeta"), None);
        assert_eq!(parse_target("nueva carpeta"), None);
        assert_eq!(parse_target("sube a la carpeta superior"), None);
    }

    #[test]
    fn nothing_the_vocabulary_already_says_is_read_as_a_target() {
        // `commands::decide_in` asks this module last, but it asks before
        // the table's own answer is returned, so a phrase read as a name
        // here would be taken away from the command that owns it.
        let vocabulary = crate::commands::vocabulary();
        let phrases = vocabulary
            .commands
            .iter()
            .flat_map(|command| command.phrases.iter())
            .chain(vocabulary.contextual.iter().flat_map(|command| command.phrases.iter()));
        for phrase in phrases {
            assert_eq!(parse_target(phrase), None, "«{phrase}» is a command, not a target");
        }
    }

    #[test]
    fn dictating_here_is_a_destination_with_nothing_to_bring_forward() {
        // «dicta aquí» goes through the destination table, not through
        // this module: what makes it different from «dicta en el
        // documento» is only that `main.rs` looks for a text field first,
        // which it does for any destination with no application of its
        // own. See `focus_text_field_here`.
        let here = crate::commands::named_destination("aquí").expect("«aquí»");
        assert_eq!(here.bundle_id, None);
        assert!(!here.takes_recipient);
        assert!(here.keys_before_typing.is_empty());
        assert_eq!(
            crate::commands::decide("minion dicta aqui").0,
            crate::commands::Decision::DictateInto { destination: "aquí", recipient: None }
        );
        // A trigger word is matched against the transcript as written,
        // accents included, so «aquí» with its accent reaches nothing.
        // «campo» is the same destination by a name that cannot lose one.
        let field = crate::commands::named_destination("campo").expect("«campo»");
        assert_eq!(field.bundle_id, None);
        assert_eq!(
            crate::commands::decide("minion dicta en el campo de texto").0,
            crate::commands::Decision::DictateInto { destination: "campo", recipient: None }
        );
    }

    #[test]
    fn a_target_needs_a_verb_that_asks_for_it() {
        // "la carpeta Dev" on its own is somebody talking about a folder.
        assert_eq!(parse_target("la carpeta dev"), None);
    }







    #[test]
    fn two_things_with_the_same_name_are_a_tie() {
        let dev_here = files::Found {
            path: "/Users/ana/Desarrollo/Dev".to_string(),
            name: "Dev".to_string(),
            is_folder: true,
        };
        let dev_there = files::Found {
            path: "/Users/ana/Documentos/Dev".to_string(),
            name: "Dev".to_string(),
            is_folder: true,
        };
        let only_one = files::Found {
            path: "/Users/ana/Desarrollo/Deveras".to_string(),
            name: "Deveras".to_string(),
            is_folder: true,
        };
        let deeper = files::Found {
            path: "/Users/ana/Documentos/proyectos/viejos/Dev".to_string(),
            name: "Dev".to_string(),
            is_folder: true,
        };
        assert!(tied(&[dev_here.clone(), dev_there]));
        assert!(!tied(&[dev_here.clone(), only_one]));
        assert!(!tied(std::slice::from_ref(&dev_here)));
        assert!(!tied(&[dev_here, deeper]), "the nearer copy wins outright");
    }
}

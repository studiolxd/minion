//! Finding a file or a folder by the name somebody said out loud.
//!
//! Spotlight already knows where everything is, so this asks it rather
//! than walking the disk: `mdfind`, scoped to the home folder, one term
//! per spoken word so «informe septiembre» reaches "Informe septiembre
//! 2026.pdf" without the two words having to be adjacent.
//!
//! What Spotlight cannot do is choose. It answers with everything, in no
//! useful order, including the copy inside a `node_modules` and the one in
//! a Time Machine backup, so the ranking below is the real work:
//!
//!   1. the standard folders answer to their Spanish names outright, and
//!      never through Spotlight — «abre la carpeta Descargas» must not
//!      depend on an index that may be rebuilding;
//!   2. an exact name beats a name that starts with what was said, which
//!      beats a name that merely contains it;
//!   3. a folder wins when a folder was asked for, and loses when a file
//!      was;
//!   4. the shallower path wins, since the copy nearer home is nearly
//!      always the one meant.
//!
//! Everything above the process boundary is pure and tested; `mdfind`
//! itself is bounded to five seconds, the same as every other command
//! Minion shells out to on the listening thread.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::text::normalise;

/// What was asked for: «la carpeta X», «el archivo X», or neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Folder,
    File,
}

/// Something Spotlight found, ready to be ranked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: String,
    /// The last component of the path, which is what was said out loud.
    pub name: String,
    pub is_folder: bool,
}

impl Found {
    /// Reads a path as a candidate, asking the file system what it is.
    fn at(path: String) -> Option<Found> {
        let is_folder = Path::new(&path).is_dir();
        Found::named(path, is_folder)
    }

    /// The same without touching the disk, so the ranking can be tested.
    fn named(path: String, is_folder: bool) -> Option<Found> {
        let name = Path::new(&path).file_name()?.to_string_lossy().to_string();
        Some(Found { path, name, is_folder })
    }
}

/// The standard folders, by the names they are asked for in Spanish.
///
/// Listed rather than looked up: macOS shows these localised in the Finder
/// but stores them under their English names, and `NSSearchPathFor…`
/// answers with the English one. Both are here, plus what the recogniser
/// tends to write, so «ve a la carpeta Descargas» is a lookup and not a
/// search.
const HOME_FOLDERS: &[(&str, &str)] = &[
    ("escritorio", "Desktop"),
    ("descargas", "Downloads"),
    ("documentos", "Documents"),
    ("imagenes", "Pictures"),
    ("fotos", "Pictures"),
    ("musica", "Music"),
    ("peliculas", "Movies"),
    ("videos", "Movies"),
    ("aplicaciones", "/Applications"),
    ("papelera", "/.Trash"),
    ("desktop", "Desktop"),
    ("downloads", "Downloads"),
    ("documents", "Documents"),
];

/// Paths nobody means when they say a name out loud.
///
/// The support folders hold thousands of files named after the ones that
/// are actually being asked for — caches, backups, a package manager's
/// copy of the whole world — and Spotlight indexes all of them.
const IGNORED: &[&str] = &[
    "/Library/",
    "/.Trash/",
    "/node_modules/",
    "/.git/",
    "/.build/",
    "/target/debug/",
    "/target/release/",
    "/Backups.backupdb/",
];

/// The home folder, or `None` when there is no `HOME` to speak of.
fn home() -> Option<String> {
    std::env::var("HOME").ok().filter(|home| !home.is_empty())
}

/// One of the standard folders, if that is what was named.
///
/// `home` is a parameter rather than read here so the table can be tested
/// without depending on whose machine the test runs on.
pub fn standard_folder(query: &str, home: &str) -> Option<String> {
    let asked = normalise(query);
    let (_, folder) = HOME_FOLDERS.iter().find(|(spoken, _)| *spoken == asked)?;
    if let Some(absolute) = folder.strip_prefix('/') {
        // "/Applications" and "/.Trash": one is not under the home folder
        // at all, the other only exists there.
        return Some(match absolute {
            ".Trash" => format!("{home}/.Trash"),
            _ => format!("/{absolute}"),
        });
    }
    Some(format!("{home}/{folder}"))
}

/// The Spotlight query for a spoken name.
///
/// One clause per word, on both the file name and the display name, so a
/// file the Finder shows under a different name than it has on disk — a
/// localised folder, mostly — is still reachable. `cd` is Spotlight's own
/// "ignore case and accents", which is exactly what a transcript needs:
/// what was said arrives here without accents already.
fn spotlight_query(words: &[String], kind: Kind) -> String {
    let mut clauses: Vec<String> = words
        .iter()
        .map(|word| {
            let word = word.replace(['\\', '\''], "");
            format!("(kMDItemFSName == '*{word}*'cd || kMDItemDisplayName == '*{word}*'cd)")
        })
        .collect();
    if kind == Kind::Folder {
        clauses.push("kMDItemContentTypeTree == 'public.folder'".to_string());
    }
    clauses.join(" && ")
}

/// The words of a spoken name, dropped of anything that cannot be searched
/// for. Empty when nothing usable is left, which is the caller's cue not
/// to search at all.
fn searchable_words(query: &str) -> Vec<String> {
    normalise(query)
        .split_whitespace()
        .filter(|word| word.chars().any(|c| c.is_alphanumeric()))
        .map(|word| word.to_string())
        .collect()
}

/// How well a name answers to what was said: 3 exact, 2 by its start, 1 by
/// containing it, 0 not at all.
fn name_score(name: &str, query: &str) -> u8 {
    let query = normalise(query);
    if query.is_empty() {
        return 0;
    }
    let full = normalise(name);
    // The extension is not said out loud: "informe" is an exact match for
    // "informe.pdf" as far as anybody speaking is concerned. Taken off
    // before normalising, since normalising turns the dot into a space.
    let stem = normalise(name.rsplit_once('.').map_or(name, |(stem, _)| stem));
    if full == query || stem == query {
        return 3;
    }
    if full.starts_with(&query) {
        return 2;
    }
    if full.contains(&query) || query.split_whitespace().all(|word| full.contains(word)) {
        return 1;
    }
    0
}

/// How deep a path is, so the copy nearer home wins a tie.
pub fn depth(path: &str) -> usize {
    path.matches('/').count()
}

/// Whether a path is somewhere nobody means.
fn ignored(path: &str) -> bool {
    IGNORED.iter().any(|ignored| path.contains(ignored))
        // Anything inside an application bundle is that application's
        // business, not a document.
        || path.contains(".app/")
}

/// Orders what Spotlight found, best first, dropping what does not answer
/// to the name at all. Pure: the whole of the choosing, with none of the
/// searching.
pub fn rank(query: &str, kind: Kind, found: Vec<Found>) -> Vec<Found> {
    let mut scored: Vec<(u8, u8, usize, Found)> = found
        .into_iter()
        .filter(|item| !ignored(&item.path))
        .filter_map(|item| {
            let score = name_score(&item.name, query);
            if score == 0 {
                return None;
            }
            let wanted = u8::from(match kind {
                Kind::Folder => item.is_folder,
                Kind::File => !item.is_folder,
            });
            Some((score, wanted, depth(&item.path), item))
        })
        .collect();
    // Best name first, then the kind that was asked for, then the
    // shallower path, then by path so the order never depends on what
    // Spotlight happened to answer first.
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(b.0.cmp(&a.0))
            .then(a.2.cmp(&b.2))
            .then(a.3.path.cmp(&b.3.path))
    });
    scored.into_iter().map(|(_, _, _, item)| item).collect()
}

/// Everything on this machine that answers to a spoken name, best first.
///
/// The standard folders answer outright; everything else goes to
/// Spotlight, scoped to the home folder.
pub fn find(query: &str, kind: Kind) -> Vec<Found> {
    // Nothing here looks at the machine it is running on during a test:
    // the ranking is tested on paths written out by hand, and a test run
    // must not go rummaging through whoever's home folder it lands in.
    if cfg!(test) {
        return Vec::new();
    }
    let Some(home) = home() else {
        return Vec::new();
    };
    if kind != Kind::File {
        if let Some(path) = standard_folder(query, &home) {
            if Path::new(&path).is_dir() {
                if let Some(found) = Found::named(path, true) {
                    return vec![found];
                }
            }
        }
    }
    let words = searchable_words(query);
    if words.is_empty() {
        return Vec::new();
    }
    let paths = spotlight(&spotlight_query(&words, kind), &home);
    let found: Vec<Found> = paths.into_iter().filter_map(Found::at).collect();
    rank(query, kind, found)
}

/// Asks Spotlight, bounded in time and in how much is read back.
fn spotlight(query: &str, home: &str) -> Vec<String> {
    let mut command = Command::new("/usr/bin/mdfind");
    command.arg("-onlyin").arg(home).arg(query);
    match run_bounded(&mut command, Duration::from_secs(5)) {
        Ok(output) => output.lines().take(400).map(|line| line.to_string()).collect(),
        Err(reason) => {
            crate::journal::write(&format!("targets  mdfind failed: {reason}"));
            Vec::new()
        }
    }
}

/// Runs a command, killing it if it outstays its welcome.
///
/// The same shape as `reminders::run_applescript` and
/// `actions::run_shell`: this runs on the listening thread, where a
/// command that never returns is a Minion that never hears anything
/// again. Shared with `notifications.rs`, which shells out for the same
/// reason.
pub fn run_bounded(command: &mut Command, timeout: Duration) -> Result<String, String> {
    use std::io::Read;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("no terminó en {} ms", timeout.as_millis()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(e.to_string()),
        }
    }?;

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if status.success() {
        return Ok(stdout);
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    Err(if stderr.trim().is_empty() {
        format!("exit {}", status.code().unwrap_or(-1))
    } else {
        stderr.trim().to_string()
    })
}

/// A path as a `file:` address.
///
/// Minion opens a file or a folder the same way it opens a web page —
/// [`crate::commands::Decision::Browse`], `/usr/bin/open` — so the path
/// has to survive being a URL: every byte that is not unreserved is
/// percent-encoded, accents and spaces included, and only the separators
/// are left alone.
pub fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for byte in path.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '/') {
            url.push(c);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// Where a path sits, said the way somebody would say it: the folder it is
/// in, so «Dev en Desarrollo» tells two of them apart. The home folder is
/// named after what it is rather than after whoever owns it — nobody
/// calls it «suvi».
pub fn spoken_location(path: &str, home: &str) -> String {
    let full = PathBuf::from(path);
    let Some(parent) = full.parent() else {
        return path.to_string();
    };
    if parent == Path::new(home) {
        return "tu carpeta personal".to_string();
    }
    match parent.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => parent.to_string_lossy().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str) -> Found {
        Found::named(path.to_string(), true).expect("a path with a name")
    }

    fn file(path: &str) -> Found {
        Found::named(path.to_string(), false).expect("a path with a name")
    }

    #[test]
    fn standard_folders_answer_to_their_spanish_names() {
        assert_eq!(standard_folder("descargas", "/Users/ana").as_deref(), Some("/Users/ana/Downloads"));
        assert_eq!(standard_folder("Escritorio", "/Users/ana").as_deref(), Some("/Users/ana/Desktop"));
        assert_eq!(standard_folder("música", "/Users/ana").as_deref(), Some("/Users/ana/Music"));
        assert_eq!(standard_folder("aplicaciones", "/Users/ana").as_deref(), Some("/Applications"));
        assert_eq!(standard_folder("papelera", "/Users/ana").as_deref(), Some("/Users/ana/.Trash"));
        assert_eq!(standard_folder("informe", "/Users/ana"), None);
    }

    #[test]
    fn a_spotlight_query_asks_for_every_word() {
        let words = searchable_words("informe septiembre");
        let query = spotlight_query(&words, Kind::File);
        assert!(query.contains("'*informe*'cd"));
        assert!(query.contains("'*septiembre*'cd"));
        assert!(query.contains(" && "));
        assert!(!query.contains("public.folder"));
    }

    #[test]
    fn asking_for_a_folder_asks_spotlight_for_one() {
        let query = spotlight_query(&searchable_words("dev"), Kind::Folder);
        assert!(query.contains("kMDItemContentTypeTree == 'public.folder'"));
    }

    #[test]
    fn a_quote_in_what_was_heard_cannot_reach_the_query() {
        // Not an injection worth losing sleep over — the argument is
        // passed as one word, not through a shell — but a stray quote
        // would still make Spotlight refuse the whole query.
        let query = spotlight_query(&searchable_words("informe' || kMDItemFSName == '*"), Kind::File);
        assert_eq!(query.matches('\'').count() % 2, 0, "quotes stay balanced: {query}");
    }

    #[test]
    fn an_exact_name_beats_a_prefix_which_beats_a_mention() {
        assert_eq!(name_score("Dev", "dev"), 3);
        assert_eq!(name_score("Informe.pdf", "informe"), 3, "the extension is not said out loud");
        assert_eq!(name_score("Development", "dev"), 2);
        assert_eq!(name_score("mi-dev-cosas", "dev"), 1);
        assert_eq!(name_score("otra cosa", "dev"), 0);
    }

    #[test]
    fn every_word_counts_towards_a_mention() {
        // Not adjacent, not at the start: still the file that was meant.
        assert_eq!(name_score("Resumen del informe de septiembre.pdf", "informe septiembre"), 1);
        assert_eq!(name_score("Informe septiembre 2026.pdf", "informe septiembre"), 2);
        assert_eq!(name_score("Informe octubre.pdf", "informe septiembre"), 0);
    }

    #[test]
    fn the_shallower_copy_wins_a_tie() {
        let ranked = rank(
            "dev",
            Kind::Folder,
            vec![folder("/Users/ana/Documentos/proyectos/Dev"), folder("/Users/ana/Dev")],
        );
        assert_eq!(ranked[0].path, "/Users/ana/Dev");
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn asking_for_a_folder_puts_folders_first() {
        let ranked = rank(
            "dev",
            Kind::Folder,
            vec![file("/Users/ana/dev.txt"), folder("/Users/ana/Documentos/Dev")],
        );
        assert!(ranked[0].is_folder);
        assert_eq!(ranked[0].path, "/Users/ana/Documentos/Dev");
    }

    #[test]
    fn asking_for_a_file_puts_files_first() {
        let ranked = rank(
            "dev",
            Kind::File,
            vec![folder("/Users/ana/Dev"), file("/Users/ana/Documentos/dev.txt")],
        );
        assert!(!ranked[0].is_folder);
    }

    #[test]
    fn support_folders_and_caches_are_never_answers() {
        let ranked = rank(
            "informe",
            Kind::File,
            vec![
                file("/Users/ana/Library/Caches/informe.pdf"),
                file("/Users/ana/proyecto/node_modules/informe.js"),
                file("/Users/ana/Aplicaciones/Cosa.app/Contents/informe.plist"),
                file("/Users/ana/Documentos/informe.pdf"),
            ],
        );
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].path, "/Users/ana/Documentos/informe.pdf");
    }

    #[test]
    fn what_does_not_answer_to_the_name_is_dropped() {
        assert!(rank("dev", Kind::File, vec![folder("/Users/ana/Documentos")]).is_empty());
    }

    #[test]
    fn a_path_becomes_a_url_that_open_understands() {
        assert_eq!(file_url("/Users/ana/Dev"), "file:///Users/ana/Dev");
        assert_eq!(file_url("/Users/ana/Mi carpeta"), "file:///Users/ana/Mi%20carpeta");
        assert_eq!(file_url("/Users/ana/Música"), "file:///Users/ana/M%C3%BAsica");
    }

    #[test]
    fn a_location_is_named_by_the_folder_it_is_in() {
        assert_eq!(spoken_location("/Users/ana/Desarrollo/Dev", "/Users/ana"), "Desarrollo");
        assert_eq!(spoken_location("/Users/ana/Documentos/Dev", "/Users/ana"), "Documentos");
        assert_eq!(spoken_location("/Users/ana/Dev", "/Users/ana"), "tu carpeta personal");
    }
}

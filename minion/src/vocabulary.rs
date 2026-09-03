//! Where the vocabulary comes from.
//!
//! What can be said used to be a set of Rust tables, which meant adding an
//! application needed a toolchain and a rebuild. It now lives in TOML files
//! grouped by subject — `macos.toml`, `browsers.toml`, `sites.toml` and the
//! rest — and this module reads them, merges them and hands
//! [`crate::commands`] one flat [`Vocabulary`].
//!
//! Three layers, later wins on conflict:
//!
//!   1. the files shipped inside the binary (`include_str!`, listed
//!      explicitly below — Minion has to work with nothing else on disk),
//!   2. every `*.toml` in `~/Library/Application Support/Minion/vocabulary/`,
//!      for packs somebody wrote or downloaded,
//!   3. `[[apps]]` and `[[commands]]` in `config.toml`, as before.
//!
//! What a file may say is deliberately narrow: a name, some phrases, and
//! one of four things to do — press a shortcut, run one of Minion's own
//! named actions, type a string, open a URL. There is no way to put a
//! script in a built-in file or a downloaded pack, because both are data
//! that may have been shared around, and data that runs is not data.
//! `config.toml` is the one exception: `script` (AppleScript) and `shell`
//! (`/bin/sh -c`, 30 s ceiling) are read there and nowhere else — see
//! [`read_action`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::actions::{self, Mods};
use crate::commands::{Action, App, Command, ContextualCommand, Destination, Site};
use crate::config::Config;
use crate::system;
use crate::text::normalise;

/// The files built into the binary, in load order.
///
/// Listed one by one rather than walked at build time: `include_str!` needs
/// a literal path, and a file that silently stops being shipped is a
/// vocabulary that silently shrinks.
const BUILT_IN: &[(&str, &str)] = &[
    ("macos.toml", include_str!("../vocabulary/macos.toml")),
    ("browsers.toml", include_str!("../vocabulary/browsers.toml")),
    ("office.toml", include_str!("../vocabulary/office.toml")),
    ("media.toml", include_str!("../vocabulary/media.toml")),
    ("dev.toml", include_str!("../vocabulary/dev.toml")),
    ("apps.toml", include_str!("../vocabulary/apps.toml")),
    ("sites.toml", include_str!("../vocabulary/sites.toml")),
    ("teams.toml", include_str!("../vocabulary/teams.toml")),
];

/// What `action = "…"` may name.
///
/// A closed list, on purpose. It is the whole of the boundary between the
/// files and the code: a file says *which* action, never *what* the action
/// does. Everything on the right-hand side is ordinary Rust that can be
/// read once and reviewed, rather than a script arriving from elsewhere.
const NAMED_ACTIONS: &[(&str, Action)] = &[
    ("volume:up", Action::Volume(10)),
    ("volume:down", Action::Volume(-10)),
    ("volume:mute", Action::Mute(true)),
    ("volume:unmute", Action::Mute(false)),
    ("music:play", Action::Script("tell application \"Spotify\" to play")),
    ("music:pause", Action::Script("tell application \"Spotify\" to pause")),
    ("music:next", Action::Script("tell application \"Spotify\" to next track")),
    (
        "music:previous",
        Action::Script("tell application \"Spotify\" to previous track"),
    ),
    ("minion:sleep", Action::Sleep),
    ("window:left", Action::Script(system::WINDOW_LEFT)),
    ("window:right", Action::Script(system::WINDOW_RIGHT)),
    ("window:maximize", Action::Script(system::WINDOW_MAXIMIZE)),
    ("window:other-screen", Action::Script(system::WINDOW_OTHER_SCREEN)),
    ("system:brightness-up", Action::Script(system::BRIGHTNESS_UP)),
    ("system:brightness-down", Action::Script(system::BRIGHTNESS_DOWN)),
    ("system:dark-mode", Action::Script(system::DARK_MODE_TOGGLE)),
    ("system:wifi-on", Action::Script(system::WIFI_ON)),
    ("system:wifi-off", Action::Script(system::WIFI_OFF)),
    ("system:empty-trash", Action::Script(system::EMPTY_TRASH)),
    ("system:sleep-display", Action::Script(system::SLEEP_DISPLAY)),
    ("system:screenshot-window", Action::Script(system::SCREENSHOT_WINDOW)),
    ("clipboard:clear", Action::Script(system::CLIPBOARD_CLEAR)),
    ("browser:copy-url", Action::Script(system::COPY_URL)),
    ("browser:duplicate-tab-safari", Action::Script(system::DUPLICATE_TAB_SAFARI)),
    ("browser:duplicate-tab-chrome", Action::Script(system::DUPLICATE_TAB_CHROME)),
    ("hud:show", Action::Hud(true)),
    ("hud:hide", Action::Hud(false)),
];

/// The category given to whatever comes out of `config.toml`.
pub const USER_CATEGORY: &str = "Tuyos";

/// One vocabulary file, as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VocabularyFile {
    /// What to call this group in the catalogue. The file name if absent.
    category: Option<String>,
    #[serde(default)]
    apps: Vec<AppEntry>,
    #[serde(default)]
    commands: Vec<CommandEntry>,
    #[serde(default)]
    sites: Vec<SiteEntry>,
    #[serde(default)]
    destinations: Vec<DestinationEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppEntry {
    name: String,
    bundle_id: String,
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandEntry {
    name: String,
    phrases: Vec<String>,
    /// A keystroke, written as on a menu: "cmd-shift-b".
    keys: Option<String>,
    /// One of [`NAMED_ACTIONS`].
    action: Option<String>,
    /// Text to type into whatever has focus.
    text: Option<String>,
    /// A page to open.
    url: Option<String>,
    /// AppleScript to run, read only from `config.toml`.
    ///
    /// Never from a built-in file or a downloaded pack: those are data
    /// that may have arrived from elsewhere, and running a script is not
    /// something data gets to ask for. See [`read_action`].
    script: Option<String>,
    /// A shell command to run, read only from `config.toml`. Same reason
    /// as `script`.
    shell: Option<String>,
    /// Bundle identifiers this only applies in. Empty means everywhere.
    #[serde(default)]
    bundles: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SiteEntry {
    name: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationEntry {
    /// Stable identifier, shown in the log.
    name: String,
    /// Words that must all appear in what follows «dicta» to pick this
    /// destination.
    trigger: Vec<String>,
    /// The application to bring forward first. Absent means whatever is
    /// already in front — "dicta en el documento".
    bundle_id: Option<String>,
    #[serde(default)]
    takes_recipient: bool,
    /// Shortcuts pressed once the application is frontmost, before
    /// anything is typed, written the same way `keys` is.
    #[serde(default)]
    keys_before_typing: Vec<String>,
    /// Shortcuts pressed after the recipient has been typed, to reach the
    /// body.
    #[serde(default)]
    keys_after_recipient: Vec<String>,
}

/// Everything that can be said, merged and ready to match against.
#[derive(Default)]
pub struct Vocabulary {
    pub apps: Vec<App>,
    /// Commands that work anywhere.
    pub commands: Vec<Command>,
    /// Commands that only exist inside particular applications.
    pub contextual: Vec<ContextualCommand>,
    pub sites: Vec<Site>,
    /// Where «dicta …» can send what follows.
    pub destinations: Vec<Destination>,
}

/// Where downloaded or hand-written packs live.
pub fn packs_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Minion/vocabulary"))
}

/// Keeps a string alive for the rest of the process.
///
/// Deliberate and bounded, the same bargain `config.rs` already makes for
/// user applications: the vocabulary is read once at startup and needed
/// until the process ends, and being `'static` is what lets it share one
/// type with everything that matches against it.
fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn leak_all(items: Vec<String>) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = items.into_iter().map(leak).collect();
    Box::leak(leaked.into_boxed_slice())
}

/// The action a named one stands for.
fn named_action(name: &str) -> Option<Action> {
    NAMED_ACTIONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, action)| *action)
}

/// Every action a file may name, for the log and the documentation.
pub fn action_names() -> Vec<&'static str> {
    NAMED_ACTIONS.iter().map(|(name, _)| *name).collect()
}

/// The category to use when a file does not name one: its own file name.
fn category_from(source: &str) -> String {
    source.trim_end_matches(".toml").to_string()
}

/// Every `.toml` file under `dir`, recursing into subdirectories. A
/// directory that cannot be read (missing, or not a directory at all)
/// contributes nothing rather than being an error — see `merge_dir`.
fn collect_toml(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(collect_toml(&path));
        } else if path.extension().is_some_and(|e| e == "toml") {
            found.push(path);
        }
    }
    found
}

/// Replaces the entry of the same name, or appends a new one.
///
/// Replacing in place rather than pushing to the end keeps the order of
/// the vocabulary stable, and order is what breaks ties between two
/// commands that score the same.
fn replace_or_push<T>(list: &mut Vec<T>, entry: T, name: impl Fn(&T) -> &'static str) {
    match list.iter().position(|existing| name(existing) == name(&entry)) {
        Some(at) => list[at] = entry,
        None => list.push(entry),
    }
}

impl Vocabulary {
    /// The files shipped inside the binary, merged.
    pub fn built_in() -> Self {
        let mut vocabulary = Self::default();
        for (name, contents) in BUILT_IN {
            vocabulary.merge_toml(name, contents);
        }
        vocabulary
    }

    /// The whole thing: built-in files, then packs, then `config.toml`.
    pub fn load(config: &Config) -> Self {
        let mut vocabulary = Self::built_in();
        if let Some(dir) = packs_dir() {
            vocabulary.merge_dir(&dir);
        }
        vocabulary.merge_config(config);
        vocabulary.report_conflicts();
        vocabulary
    }

    /// Merges one file's worth of vocabulary.
    ///
    /// A file that does not parse is reported and skipped: one bad pack
    /// must not cost the rest of the vocabulary.
    pub fn merge_toml(&mut self, source: &str, contents: &str) {
        let file: VocabularyFile = match toml::from_str(contents) {
            Ok(file) => file,
            Err(e) => {
                crate::note!("Ignoring vocabulary {source}: {e}");
                return;
            }
        };
        let category = leak(file.category.unwrap_or_else(|| category_from(source)));
        for app in file.apps {
            self.add_app(app, category);
        }
        for command in file.commands {
            self.add_command(command, category, source);
        }
        for site in file.sites {
            self.add_site(site, category);
        }
        for destination in file.destinations {
            self.add_destination(destination, source);
        }
    }

    /// Merges every `*.toml` under a directory, including subdirectories —
    /// a downloaded pack collection groups related files together (e.g.
    /// `community/adobe.toml`), and a flat scan would silently skip them.
    /// A missing directory is fine — most installations will never have
    /// one.
    pub fn merge_dir(&mut self, dir: &Path) {
        let mut files = collect_toml(dir);
        // Sorted, so which pack wins does not depend on the order the
        // filesystem happens to hand the names back in.
        files.sort();
        for path in files {
            let name = path
                .strip_prefix(dir)
                .map_or_else(|_| path.display().to_string(), |p| p.to_string_lossy().into_owned());
            match std::fs::read_to_string(&path) {
                Ok(contents) => self.merge_toml(&name, &contents),
                Err(e) => crate::note!("Ignoring vocabulary {name}: {e}"),
            }
        }
    }

    /// Merges the applications and commands from `config.toml`, which have
    /// the last word.
    pub fn merge_config(&mut self, config: &Config) {
        for app in config.extra_apps() {
            replace_or_push(&mut self.apps, app, |a| a.name);
        }
        for command in config.extra_commands() {
            replace_or_push(&mut self.commands, command, |c| c.name);
        }
    }

    fn add_app(&mut self, entry: AppEntry, category: &'static str) {
        let app = App {
            name: leak(entry.name),
            bundle_id: leak(entry.bundle_id),
            // Aliases are compared against normalised speech, so they are
            // stored normalised — an accent in the file would never match.
            aliases: leak_all(entry.aliases.iter().map(|a| normalise(a)).collect()),
            category,
        };
        replace_or_push(&mut self.apps, app, |a| a.name);
    }

    fn add_command(&mut self, entry: CommandEntry, category: &'static str, source: &str) {
        let Some(action) = read_action(&entry, source) else {
            return;
        };
        let name = leak(entry.name);
        let phrases = leak_all(entry.phrases.iter().map(|p| normalise(p)).collect());
        if entry.bundles.is_empty() {
            let command = Command { phrases, name, action, category };
            replace_or_push(&mut self.commands, command, |c| c.name);
        } else {
            let command = ContextualCommand {
                bundles: leak_all(entry.bundles),
                phrases,
                name,
                action,
                category,
            };
            replace_or_push(&mut self.contextual, command, |c| c.name);
        }
    }

    fn add_site(&mut self, entry: SiteEntry, category: &'static str) {
        let site = Site {
            // Matched against a normalised word of the sentence.
            name: leak(normalise(&entry.name)),
            url: leak(entry.url),
            category,
        };
        replace_or_push(&mut self.sites, site, |s| s.name);
    }

    fn add_destination(&mut self, entry: DestinationEntry, source: &str) {
        let Some(destination) = read_destination(entry, source) else { return };
        replace_or_push(&mut self.destinations, destination, |d| d.name);
    }

    /// Reports every phrase claimed by more than one command, and lets the
    /// later one keep it.
    ///
    /// Sharing a phrase makes the winner depend on the order the files
    /// happened to load, which is a latent bug rather than a choice. It
    /// cannot be a hard error: a pack the user downloaded must not be able
    /// to stop Minion from starting.
    ///
    /// A global and a contextual command may share a phrase — that is the
    /// point of the contextual table — so the two are checked separately,
    /// and two contextual commands only collide where their applications
    /// overlap.
    pub fn report_conflicts(&mut self) {
        let mut owner: HashMap<&'static str, usize> = HashMap::new();
        let mut lost: Vec<(usize, &'static str)> = Vec::new();
        for (at, command) in self.commands.iter().enumerate() {
            for phrase in command.phrases {
                if let Some(before) = owner.insert(phrase, at) {
                    crate::note!(
                        "vocabulary  «{phrase}» is claimed by both «{}» and «{}»; \
                         the later one wins",
                        self.commands[before].name,
                        command.name
                    );
                    lost.push((before, phrase));
                }
            }
        }
        for (at, phrase) in lost {
            self.commands[at].phrases = without(self.commands[at].phrases, phrase);
        }

        let mut lost: Vec<(usize, &'static str)> = Vec::new();
        for later in 0..self.contextual.len() {
            for earlier in 0..later {
                let overlap = self.contextual[earlier]
                    .bundles
                    .iter()
                    .any(|bundle| self.contextual[later].bundles.contains(bundle));
                if !overlap {
                    continue;
                }
                for phrase in self.contextual[later].phrases {
                    if self.contextual[earlier].phrases.contains(phrase) {
                        crate::note!(
                            "vocabulary  «{phrase}» is claimed by both «{}» and «{}» in the \
                             same application; the later one wins",
                            self.contextual[earlier].name,
                            self.contextual[later].name
                        );
                        lost.push((earlier, phrase));
                    }
                }
            }
        }
        for (at, phrase) in lost {
            self.contextual[at].phrases = without(self.contextual[at].phrases, phrase);
        }
    }
}

/// The same phrases without one of them.
fn without(phrases: &'static [&'static str], unwanted: &str) -> &'static [&'static str] {
    let kept: Vec<&'static str> = phrases.iter().copied().filter(|p| *p != unwanted).collect();
    Box::leak(kept.into_boxed_slice())
}

/// Works out what a command entry does.
///
/// Exactly one of `keys`, `action`, `text` and `url` must be there. Saying
/// none of them is a command that does nothing; saying two is a command
/// whose behaviour depends on which field this function looks at first.
/// Both are reported and the entry is dropped — one bad line costs that
/// command, not the file.
fn read_action(entry: &CommandEntry, source: &str) -> Option<Action> {
    let name = &entry.name;
    // `script`/`shell` are the one thing a vocabulary file cannot ask for
    // on its own say-so: a downloaded pack is data, and data that runs is
    // not data. Only `config.toml` — written by the person running
    // Minion, never fetched — gets to use them. The merge already knows
    // which file this entry came from, so the check is just that.
    if (entry.script.is_some() || entry.shell.is_some()) && source != "config.toml" {
        crate::note!(
            "Ignoring script in pack {source}: scripts are only read from config.toml"
        );
        return None;
    }
    let mut asked: Vec<Action> = Vec::new();
    if let Some(script) = &entry.script {
        asked.push(Action::RunScript(leak(script.clone())));
    }
    if let Some(shell) = &entry.shell {
        asked.push(Action::RunShell(leak(shell.clone())));
    }
    if let Some(keys) = &entry.keys {
        match actions::parse_shortcut(keys) {
            Some((code, mods)) => asked.push(Action::Key(code, mods)),
            None => {
                crate::note!(
                    "Ignoring command «{name}» in {source}: cannot read the shortcut «{keys}»"
                );
                return None;
            }
        }
    }
    if let Some(wanted) = &entry.action {
        match named_action(wanted) {
            Some(action) => asked.push(action),
            None => {
                crate::note!(
                    "Ignoring command «{name}» in {source}: there is no action called \
                     «{wanted}». Known: {}",
                    action_names().join(", ")
                );
                return None;
            }
        }
    }
    if let Some(text) = &entry.text {
        asked.push(Action::Type(leak(text.clone())));
    }
    if let Some(url) = &entry.url {
        asked.push(Action::Open(leak(url.clone())));
    }

    match asked.len() {
        1 => Some(asked[0]),
        0 => {
            crate::note!(
                "Ignoring command «{name}» in {source}: it does nothing — give it keys, \
                 action, text or url"
            );
            None
        }
        _ => {
            crate::note!(
                "Ignoring command «{name}» in {source}: it asks for more than one thing at once"
            );
            None
        }
    }
}

/// Reads a `[[destinations]]` entry into a [`Destination`], or reports why
/// it could not and drops it — one bad entry must not cost the rest of the
/// file.
fn read_destination(entry: DestinationEntry, source: &str) -> Option<Destination> {
    let name = &entry.name;
    if entry.trigger.is_empty() {
        crate::note!("Ignoring destination «{name}» in {source}: it has no trigger words");
        return None;
    }
    let parse_keys = |field: &str, keys: &[String]| -> Option<Vec<(u16, Mods)>> {
        keys.iter()
            .map(|k| {
                actions::parse_shortcut(k).or_else(|| {
                    crate::note!(
                        "Ignoring destination «{name}» in {source}: cannot read the shortcut \
                         «{k}» in {field}"
                    );
                    None
                })
            })
            .collect()
    };
    let before = parse_keys("keys_before_typing", &entry.keys_before_typing)?;
    let after = parse_keys("keys_after_recipient", &entry.keys_after_recipient)?;

    Some(Destination {
        name: leak(entry.name.clone()),
        trigger: leak_all(entry.trigger.iter().map(|t| normalise(t)).collect()),
        bundle_id: entry.bundle_id.map(leak),
        takes_recipient: entry.takes_recipient,
        keys_before_typing: Box::leak(before.into_boxed_slice()),
        keys_after_recipient: Box::leak(after.into_boxed_slice()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_file_parses() {
        // `merge_toml` swallows a broken file so one bad pack cannot cost
        // the rest, which would hide a typo in a shipped file behind a log
        // line nobody reads. Parsed directly here, where it can fail.
        for (name, contents) in BUILT_IN {
            toml::from_str::<VocabularyFile>(contents)
                .unwrap_or_else(|e| panic!("{name} should parse: {e}"));
        }
    }

    #[test]
    fn the_example_of_machine_specific_apps_parses() {
        // Not loaded, but it is the file people are told to copy, so it
        // has to be right.
        let example = include_str!("../vocabulary/local.example.toml");
        toml::from_str::<VocabularyFile>(example).expect("local.example.toml should parse");
    }

    #[test]
    fn every_built_in_command_says_what_it_does() {
        // A shipped entry with no action, an unreadable shortcut or an
        // action that does not exist would be dropped at startup with only
        // a log line to show for it.
        let vocabulary = Vocabulary::built_in();
        let declared: usize = BUILT_IN
            .iter()
            .map(|(_, contents)| {
                let file: VocabularyFile = toml::from_str(contents).expect("should parse");
                file.commands.len()
            })
            .sum();
        assert_eq!(
            vocabulary.commands.len() + vocabulary.contextual.len(),
            declared,
            "every declared command should have survived the load"
        );
    }

    #[test]
    fn the_built_in_vocabulary_needs_no_conflicts_resolved() {
        // `report_conflicts` quietly hands a contested phrase to the later
        // command, which is the right thing for a pack somebody downloaded
        // and the wrong thing to lean on in the files Minion ships.
        let mut vocabulary = Vocabulary::built_in();
        let before: usize = vocabulary
            .commands
            .iter()
            .map(|c| c.phrases.len())
            .chain(vocabulary.contextual.iter().map(|c| c.phrases.len()))
            .sum();
        vocabulary.report_conflicts();
        let after: usize = vocabulary
            .commands
            .iter()
            .map(|c| c.phrases.len())
            .chain(vocabulary.contextual.iter().map(|c| c.phrases.len()))
            .sum();
        assert_eq!(before, after, "a shipped phrase is claimed twice");
    }

    #[test]
    fn a_pack_can_add_a_command_to_an_application_of_its_own() {
        let mut vocabulary = Vocabulary::built_in();
        vocabulary.merge_toml(
            "pack.toml",
            r#"
            [[commands]]
            name = "compilar"
            phrases = ["compila el proyecto"]
            keys = "cmd-shift-b"
            bundles = ["com.microsoft.VSCode"]
            "#,
        );
        let command = vocabulary
            .contextual
            .iter()
            .find(|c| c.name == "compilar")
            .expect("the pack's command should be there");
        assert_eq!(command.bundles, ["com.microsoft.VSCode"]);
        assert!(
            !vocabulary.commands.iter().any(|c| c.name == "compilar"),
            "bundles make it contextual, not global"
        );
    }

    #[test]
    fn a_later_file_replaces_an_application_of_the_same_name() {
        let mut vocabulary = Vocabulary::built_in();
        let before = vocabulary
            .apps
            .iter()
            .position(|a| a.name == "Chrome")
            .expect("Chrome is built in");
        vocabulary.merge_toml(
            "pack.toml",
            r#"
            [[apps]]
            name = "Chrome"
            bundle_id = "com.google.Chrome.canary"
            aliases = ["canary"]
            "#,
        );
        let chrome = &vocabulary.apps[before];
        assert_eq!(chrome.bundle_id, "com.google.Chrome.canary");
        assert_eq!(chrome.aliases, ["canary"]);
        assert_eq!(
            vocabulary.apps.iter().filter(|a| a.name == "Chrome").count(),
            1,
            "replaced, not appended"
        );
    }

    #[test]
    fn a_pack_in_a_directory_overrides_a_built_in_application() {
        // A real directory, in the system temp: never anywhere under
        // ~/Library, which belongs to whoever is running the tests.
        let dir = std::env::temp_dir().join(format!(
            "minion-vocabulary-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("temp directory");
        std::fs::write(
            dir.join("pack.toml"),
            "[[apps]]\nname = \"Finder\"\nbundle_id = \"pack.finder\"\naliases = [\"finder\"]\n",
        )
        .expect("write pack");
        // Not a .toml, so it must be left alone.
        std::fs::write(dir.join("notes.txt"), "not vocabulary").expect("write note");

        let mut vocabulary = Vocabulary::built_in();
        vocabulary.merge_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        let finder = vocabulary
            .apps
            .iter()
            .find(|a| a.name == "Finder")
            .expect("Finder should still be there");
        assert_eq!(finder.bundle_id, "pack.finder");
    }

    #[test]
    fn a_pack_in_a_subdirectory_is_loaded_too() {
        // packs.rs unzips a downloaded release with packs grouped under
        // subdirectories such as `community/` — a scan that only looked
        // at the top level would silently never load them.
        let dir = std::env::temp_dir().join(format!(
            "minion-vocabulary-test-nested-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let nested = dir.join("community");
        std::fs::create_dir_all(&nested).expect("nested temp directory");
        std::fs::write(
            nested.join("adobe.toml"),
            "[[apps]]\nname = \"Photoshop\"\nbundle_id = \"com.adobe.Photoshop\"\naliases = [\"photoshop\"]\n",
        )
        .expect("write nested pack");

        let mut vocabulary = Vocabulary::default();
        vocabulary.merge_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            vocabulary.apps.iter().any(|a| a.name == "Photoshop"),
            "a pack in a subdirectory should still be merged"
        );
    }

    #[test]
    fn config_has_the_last_word_over_a_pack() {
        let mut vocabulary = Vocabulary::built_in();
        vocabulary.merge_toml(
            "pack.toml",
            "[[apps]]\nname = \"Finder\"\nbundle_id = \"pack.finder\"\naliases = [\"finder\"]\n",
        );
        let config: Config = toml::from_str(
            r#"
            [[apps]]
            name = "Finder"
            bundle_id = "mine.finder"
            aliases = ["finder"]
            "#,
        )
        .expect("config should parse");
        vocabulary.merge_config(&config);

        let finder = vocabulary.apps.iter().find(|a| a.name == "Finder").expect("Finder");
        assert_eq!(finder.bundle_id, "mine.finder");
        assert_eq!(finder.category, USER_CATEGORY);
    }

    #[test]
    fn a_phrase_claimed_twice_goes_to_the_later_command() {
        let mut vocabulary = Vocabulary::built_in();
        vocabulary.merge_toml(
            "pack.toml",
            r#"
            [[commands]]
            name = "mi guardado"
            phrases = ["guarda esto", "guarda lo mío"]
            keys = "cmd-alt-s"
            "#,
        );
        vocabulary.report_conflicts();

        let old = vocabulary.commands.iter().find(|c| c.name == "guardar").expect("guardar");
        assert!(!old.phrases.contains(&"guarda esto"), "the earlier one gives it up");
        let new = vocabulary.commands.iter().find(|c| c.name == "mi guardado").expect("mine");
        assert!(new.phrases.contains(&"guarda esto"), "the later one keeps it");
        assert!(new.phrases.contains(&"guarda lo mio"), "phrases are normalised");
    }

    #[test]
    fn two_contextual_commands_only_collide_where_they_overlap() {
        // "sube" means the previous command in Terminal and the parent
        // folder in the Finder. Different applications, so not a conflict.
        let mut vocabulary = Vocabulary::built_in();
        vocabulary.report_conflicts();
        let up = |name: &str| {
            vocabulary
                .contextual
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{name} should be there"))
        };
        assert!(up("carpeta superior").phrases.contains(&"sube"));
        assert!(up("orden anterior").phrases.contains(&"sube"));
    }

    #[test]
    fn a_command_that_does_nothing_is_dropped_and_the_rest_survive() {
        let mut vocabulary = Vocabulary::default();
        vocabulary.merge_toml(
            "pack.toml",
            r#"
            [[commands]]
            name = "vacío"
            phrases = ["esto no hace nada"]

            [[commands]]
            name = "dos cosas"
            phrases = ["esto hace dos"]
            keys = "cmd-k"
            text = "hola"

            [[commands]]
            name = "atajo roto"
            phrases = ["esto no se lee"]
            keys = "cmd-shift"

            [[commands]]
            name = "acción inventada"
            phrases = ["esto no existe"]
            action = "coffee:make"

            [[commands]]
            name = "bueno"
            phrases = ["esto sí"]
            keys = "cmd-k"
            "#,
        );
        let names: Vec<&str> = vocabulary.commands.iter().map(|c| c.name).collect();
        assert_eq!(names, ["bueno"]);
    }

    #[test]
    fn a_file_that_does_not_parse_costs_only_itself() {
        let mut vocabulary = Vocabulary::built_in();
        let before = vocabulary.apps.len();
        vocabulary.merge_toml("broken.toml", "[[apps]]\nnombre = \"Notion\"\n");
        assert_eq!(vocabulary.apps.len(), before);
    }

    #[test]
    fn a_file_without_a_category_is_named_after_itself() {
        let mut vocabulary = Vocabulary::default();
        vocabulary.merge_toml(
            "trabajo.toml",
            "[[apps]]\nname = \"Notion\"\nbundle_id = \"notion.id\"\naliases = [\"notion\"]\n",
        );
        assert_eq!(vocabulary.apps[0].category, "trabajo");
    }

    #[test]
    fn text_and_url_commands_are_read() {
        let mut vocabulary = Vocabulary::default();
        vocabulary.merge_toml(
            "pack.toml",
            r#"
            [[commands]]
            name = "firma"
            phrases = ["pon mi firma"]
            text = "Un saludo,"

            [[commands]]
            name = "el parte"
            phrases = ["abre el parte"]
            url = "https://example.com/parte"
            "#,
        );
        assert!(matches!(vocabulary.commands[0].action, Action::Type("Un saludo,")));
        assert!(matches!(
            vocabulary.commands[1].action,
            Action::Open("https://example.com/parte")
        ));
    }

    #[test]
    fn a_script_or_shell_command_is_accepted_from_config_toml() {
        let mut vocabulary = Vocabulary::default();
        vocabulary.merge_toml(
            "config.toml",
            r#"
            [[commands]]
            name = "vacía la papelera"
            phrases = ["vacía la papelera"]
            script = "tell application \"Finder\" to empty trash"

            [[commands]]
            name = "backup"
            phrases = ["haz una copia"]
            shell = "rsync -a ~/Documents ~/Backup"
            "#,
        );
        let names: Vec<&str> = vocabulary.commands.iter().map(|c| c.name).collect();
        assert_eq!(names, ["vacía la papelera", "backup"]);
        assert!(matches!(vocabulary.commands[0].action, Action::RunScript(_)));
        assert!(matches!(vocabulary.commands[1].action, Action::RunShell(_)));
    }

    #[test]
    fn a_pack_cannot_ask_for_a_script_or_a_shell_command() {
        let mut vocabulary = Vocabulary::default();
        vocabulary.merge_toml(
            "pack.toml",
            r#"
            [[commands]]
            name = "sospechosa"
            phrases = ["borra todo"]
            script = "tell application \"Finder\" to empty trash"

            [[commands]]
            name = "sospechosa2"
            phrases = ["ejecuta esto"]
            shell = "rm -rf ~"
            "#,
        );
        assert!(
            vocabulary.commands.is_empty(),
            "a pack asking for a script or a shell command should be refused entirely"
        );
    }
}

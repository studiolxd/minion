//! The command vocabulary: what can be said, and what each phrase does.
//!
//! Spoken phrases are Spanish because that is the language being spoken;
//! everything else is English. A command is a phrase opening with the wake
//! word, followed by something recognisable. Anything else is ignored — with
//! a microphone that is always on, ignoring is the default and acting is the
//! exception.

use std::sync::OnceLock;

use crate::actions::{self, key, Mods};
use crate::config::Config;
use crate::spanish;
use crate::text::{keywords, normalise, similarity};

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

/// Set once at startup from the configuration file. Absent means defaults.
static USER_APPS: OnceLock<Vec<App>> = OnceLock::new();
static USER_ALIASES: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
static USER_COMMANDS: OnceLock<Vec<Command>> = OnceLock::new();
static USER_WAKE_WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
static USER_THRESHOLD: OnceLock<f32> = OnceLock::new();

/// Applies the user configuration. Call once, before anything is decided.
pub fn configure(config: &Config) {
    let extra = config.extra_apps();
    if !extra.is_empty() {
        let _ = USER_APPS.set(extra);
    }
    let own = config.extra_commands();
    if !own.is_empty() {
        let _ = USER_COMMANDS.set(own);
    }
    let aliases = config.extra_aliases();
    if !aliases.is_empty() {
        // An alias whose command does not exist can never fire. Said now,
        // once, rather than leaving the user to wonder in front of a
        // microphone that answers nothing.
        let own: &[Command] = USER_COMMANDS.get().map_or(&[], Vec::as_slice);
        for (name, phrase) in &aliases {
            if resolve_target(name, own) == Target::Unknown {
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
}

/// Where the command an alias points at lives, if it exists at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// One of the built-in commands.
    Global,
    /// One that only exists inside particular applications.
    Contextual,
    /// One the user declared in `[[commands]]`.
    User,
    /// Nothing of that name: the alias can never fire.
    Unknown,
}

/// Resolves the name an alias points at.
///
/// Commands are identified by their name, spelled out in the log and
/// copied into the configuration by hand, so a misspelling ("atras" for
/// "atrás") produces an alias that silently never fires. Checked at
/// startup instead, where it can be said out loud.
pub fn resolve_target(name: &str, user_commands: &[Command]) -> Target {
    if COMMANDS.iter().any(|c| c.name == name) {
        Target::Global
    } else if user_commands.iter().any(|c| c.name == name) {
        Target::User
    } else if CONTEXTUAL_COMMANDS.iter().any(|c| c.name == name) {
        Target::Contextual
    } else {
        Target::Unknown
    }
}

/// The command with this name, built in or the user's own.
fn named_command(name: &str) -> Option<&'static Command> {
    COMMANDS
        .iter()
        .chain(USER_COMMANDS.get().into_iter().flatten())
        .find(|c| c.name == name)
}

/// Wake words in force: the user's if configured, otherwise the defaults.
pub fn wake_words() -> &'static [&'static str] {
    USER_WAKE_WORDS.get().map_or(DEFAULT_WAKE_WORDS, |w| w.as_slice())
}

/// Confidence required to act.
fn threshold() -> f32 {
    *USER_THRESHOLD.get().unwrap_or(&DEFAULT_THRESHOLD)
}

/// Every application, built in and user-added.
fn all_apps() -> impl Iterator<Item = &'static App> {
    APPS.iter().chain(USER_APPS.get().into_iter().flatten())
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

/// Sites common enough to name without a domain.
const SITES: &[(&str, &str)] = &[
    ("google", "https://www.google.com"),
    ("youtube", "https://www.youtube.com"),
    ("gmail", "https://mail.google.com"),
    ("github", "https://github.com"),
    ("wikipedia", "https://es.wikipedia.org"),
    ("drive", "https://drive.google.com"),
    ("maps", "https://maps.google.com"),
    ("mapas", "https://maps.google.com"),
    ("calendar", "https://calendar.google.com"),
    ("linkedin", "https://www.linkedin.com"),
    ("amazon", "https://www.amazon.es"),
    ("netflix", "https://www.netflix.com"),
];

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Key(u16, Mods),
    Volume(i32),
    Mute(bool),
    Script(&'static str),
    /// Stop acting on commands until resumed from the menu bar.
    Sleep,
}

pub struct Command {
    /// Ways of saying it. The first is canonical and gets documented.
    pub phrases: &'static [&'static str],
    /// Stable identifier, also shown in the log.
    pub name: &'static str,
    pub action: Action,
}

/// An application, with the ways people actually say its name.
pub struct App {
    pub name: &'static str,
    pub bundle_id: &'static str,
    /// Includes what the recogniser really produces, not just correct
    /// spellings: "cromo" is what comes out of saying Chrome in Spanish.
    pub aliases: &'static [&'static str],
}

pub const APPS: &[App] = &[
    App { name: "Chrome", bundle_id: "com.google.Chrome",
          // "grum", "crum", "crumb", "so fuddy": what the recogniser makes of
          // a Spanish mouth saying two English names. Listed as heard, since
          // no tolerance short of reckless would reach them from the real word.
          aliases: &["chrome", "crome", "cromo", "grum", "crum", "crumb",
                     "el navegador", "navegador"] },
    // "shafari", "safaris", "safaria": what the recogniser writes when the
    // word is said quickly. Cheaper and safer than loosening the matcher.
    App { name: "Safari", bundle_id: "com.apple.Safari",
          aliases: &["safari", "shafari", "safaris", "safaria", "so fuddy", "el safari"] },
    App { name: "Terminal", bundle_id: "com.apple.Terminal",
          aliases: &["terminal", "la terminal", "consola"] },
    App { name: "Orca", bundle_id: "com.stablyai.orca",
          aliases: &["orca", "orka", "el editor", "editor"] },
    App { name: "Finder", bundle_id: "com.apple.finder",
          aliases: &["finder", "el buscador", "archivos"] },
    App { name: "Mail", bundle_id: "com.apple.mail",
          aliases: &["mail", "correo", "el correo"] },
    App { name: "Notas", bundle_id: "com.apple.Notes", aliases: &["notas", "las notas"] },
    App { name: "Calendario", bundle_id: "com.apple.iCal",
          aliases: &["calendario", "el calendario", "agenda"] },
    App { name: "Spotify", bundle_id: "com.spotify.client",
          aliases: &["spotify", "espotifai", "musica", "la musica"] },
    App { name: "WhatsApp", bundle_id: "net.whatsapp.WhatsApp",
          aliases: &["whatsapp", "guasap", "wasap"] },
    App { name: "Telegram", bundle_id: "ru.keepcoder.Telegram",
          aliases: &["telegram", "telegrama"] },
    App { name: "Figma", bundle_id: "com.figma.Desktop", aliases: &["figma", "figna"] },
    App { name: "Obsidian", bundle_id: "md.obsidian", aliases: &["obsidian", "obsidiana"] },
    App { name: "Discord", bundle_id: "com.hnc.Discord", aliases: &["discord", "diskord"] },
    App { name: "Teams", bundle_id: "com.microsoft.teams2", aliases: &["teams", "tims"] },
    App { name: "VS Code", bundle_id: "com.microsoft.VSCode",
          aliases: &["visual studio", "vs code", "vsc"] },
    App { name: "Claude", bundle_id: "com.anthropic.claudefordesktop",
          aliases: &["claude", "clod"] },
    App { name: "ChatGPT", bundle_id: "com.openai.codex",
          aliases: &["chat gpt", "chatgpt", "gepete"] },
    App { name: "Ajustes", bundle_id: "com.apple.systempreferences",
          aliases: &["ajustes", "preferencias", "configuracion"] },
    App { name: "Vista Previa", bundle_id: "com.apple.Preview",
          aliases: &["vista previa", "previsualizacion"] },
    App { name: "Monitor de Actividad", bundle_id: "com.apple.ActivityMonitor",
          aliases: &["monitor de actividad", "actividad"] },
];

/// Commands that only exist inside particular applications.
///
/// Kept apart from the global table rather than adding a context field to
/// every entry: most commands are global, and this way the exceptions are
/// visible in one place. A contextual command beats a global one with the
/// same phrase, which is what lets an app reinterpret a general word.
pub struct ContextualCommand {
    /// Bundle identifiers this applies in.
    pub bundles: &'static [&'static str],
    pub phrases: &'static [&'static str],
    pub name: &'static str,
    pub action: Action,
}

const BROWSERS: &[&str] = &["com.google.Chrome", "com.apple.Safari"];
const TERMINALS: &[&str] = &["com.apple.Terminal"];
const FINDERS: &[&str] = &["com.apple.finder"];

pub const CONTEXTUAL_COMMANDS: &[ContextualCommand] = &[
    // --- The same phrase, read differently ---
    //
    // These share their wording with an entry in the global table. The
    // contextual one wins where it applies, which is what lets a phrase
    // mean the right thing in each place instead of needing a new name.
    ContextualCommand { bundles: FINDERS, phrases: &["borra esto"],
                        name: "a la papelera", action: Action::Key(key::DELETE, Mods::CMD) },
    ContextualCommand { bundles: FINDERS, phrases: &["sube del todo", "sube"],
                        name: "carpeta superior", action: Action::Key(key::UP, Mods::CMD) },
    ContextualCommand { bundles: TERMINALS, phrases: &["cancela esto", "cancela"],
                        name: "interrumpir", action: Action::Key(key::C, Mods::CTRL) },
    ContextualCommand { bundles: TERMINALS, phrases: &["sube", "sube del todo"],
                        name: "orden anterior", action: Action::Key(key::UP, Mods::NONE) },
    ContextualCommand { bundles: TERMINALS, phrases: &["baja", "baja del todo"],
                        name: "orden siguiente", action: Action::Key(key::DOWN, Mods::NONE) },

    // --- Terminal ---
    ContextualCommand { bundles: TERMINALS, phrases: &["limpia la pantalla", "limpia"],
                        name: "limpiar terminal", action: Action::Key(key::L, Mods::CTRL) },

    ContextualCommand { bundles: TERMINALS, phrases: &["principio de linea"],
                        name: "inicio de línea", action: Action::Key(key::A, Mods::CTRL) },
    ContextualCommand { bundles: TERMINALS, phrases: &["final de linea"],
                        name: "fin de línea", action: Action::Key(key::E, Mods::CTRL) },

    // --- Navegadores ---
    ContextualCommand { bundles: BROWSERS, phrases: &["abre los favoritos", "marcadores"],
                        name: "favoritos", action: Action::Key(key::B, Mods::CMD_SHIFT) },
    ContextualCommand { bundles: BROWSERS, phrases: &["abre el historial"],
                        name: "historial", action: Action::Key(key::Y, Mods::CMD) },
    ContextualCommand { bundles: BROWSERS, phrases: &["ventana de incognito", "modo incognito"],
                        name: "ventana privada", action: Action::Key(key::N, Mods::CMD_SHIFT) },

    // --- Finder ---
    ContextualCommand { bundles: FINDERS, phrases: &["crea una carpeta", "nueva carpeta"],
                        name: "carpeta nueva", action: Action::Key(key::N, Mods::CMD_SHIFT) },
    ContextualCommand { bundles: FINDERS, phrases: &["muestra la informacion", "informacion"],
                        name: "obtener información", action: Action::Key(key::I, Mods::CMD) },
];

pub const COMMANDS: &[Command] = &[
    // --- Editing ---
    Command { phrases: &["copia esto"], name: "copiar",
              action: Action::Key(key::C, Mods::CMD) },
    Command { phrases: &["pega esto"], name: "pegar",
              action: Action::Key(key::V, Mods::CMD) },
    Command { phrases: &["corta esto"], name: "cortar",
              action: Action::Key(key::X, Mods::CMD) },
    Command { phrases: &["guarda esto"], name: "guardar",
              action: Action::Key(key::S, Mods::CMD) },
    Command { phrases: &["deshaz el cambio"], name: "deshacer",
              action: Action::Key(key::Z, Mods::CMD) },
    Command { phrases: &["rehaz el cambio"], name: "rehacer",
              action: Action::Key(key::Z, Mods::CMD_SHIFT) },
    Command { phrases: &["selecciona todo"], name: "seleccionar todo",
              action: Action::Key(key::A, Mods::CMD) },
    Command { phrases: &["borra esto"], name: "borrar",
              action: Action::Key(key::DELETE, Mods::NONE) },
    Command { phrases: &["borra la palabra"], name: "borrar palabra",
              action: Action::Key(key::DELETE, Mods::OPTION) },
    // From the log: "borra la frase hasta el inicio".
    Command { phrases: &["borra hasta el inicio", "borra la frase"], name: "borrar hasta el inicio",
              action: Action::Key(key::DELETE, Mods::CMD) },
    Command { phrases: &["cancela esto", "cancela"], name: "cancelar",
              action: Action::Key(key::ESCAPE, Mods::NONE) },
    Command { phrases: &["busca en la pagina", "busca aqui"], name: "buscar",
              action: Action::Key(key::F, Mods::CMD) },

    // --- Tabs ---
    Command { phrases: &["abre una pestana nueva"], name: "pestaña nueva",
              action: Action::Key(key::T, Mods::CMD) },
    Command { phrases: &["cierra la pestana"], name: "cerrar pestaña",
              action: Action::Key(key::W, Mods::CMD) },
    Command { phrases: &["recupera la pestana"], name: "reabrir pestaña",
              action: Action::Key(key::T, Mods::CMD_SHIFT) },
    Command { phrases: &["pasa a la siguiente pestana", "siguiente pestana"], name: "pestaña siguiente",
              action: Action::Key(key::TAB, Mods::CTRL) },
    Command { phrases: &["vuelve a la pestana anterior", "pestana anterior"], name: "pestaña anterior",
              action: Action::Key(key::TAB, Mods::CTRL_SHIFT) },
    Command { phrases: &["ultima pestana", "pestana final"], name: "última pestaña",
              action: Action::Key(key::DIGIT_9, Mods::CMD) },

    // --- Windows ---
    Command { phrases: &["cierra la ventana"], name: "cerrar ventana",
              action: Action::Key(key::W, Mods::CMD) },
    Command { phrases: &["abre una ventana nueva"], name: "ventana nueva",
              action: Action::Key(key::N, Mods::CMD) },
    Command { phrases: &["minimiza la ventana", "minimiza"], name: "minimizar",
              action: Action::Key(key::M, Mods::CMD) },
    Command { phrases: &["pon la pantalla completa", "pantalla completa"], name: "pantalla completa",
              action: Action::Key(key::F, Mods::CTRL_CMD) },
    Command { phrases: &["esconde la aplicacion"], name: "ocultar app",
              action: Action::Key(key::H, Mods::CMD) },

    // --- Navigation ---
    Command { phrases: &["vuelve atras", "pagina anterior", "pagina atras",
                         "retrocede la pagina"], name: "atrás",
              action: Action::Key(key::LEFT, Mods::CMD) },
    Command { phrases: &["ve hacia adelante", "ve adelante", "pagina siguiente",
                         "avanza la pagina"], name: "adelante",
              action: Action::Key(key::RIGHT, Mods::CMD) },
    Command { phrases: &["recarga la pagina", "recarga"], name: "recargar",
              action: Action::Key(key::R, Mods::CMD) },
    Command { phrases: &["sube del todo"], name: "ir arriba",
              action: Action::Key(key::UP, Mods::CMD) },
    Command { phrases: &["baja del todo"], name: "ir abajo",
              action: Action::Key(key::DOWN, Mods::CMD) },
    Command { phrases: &["ve a la barra de direcciones", "barra de direcciones"], name: "barra de direcciones",
              action: Action::Key(key::L, Mods::CMD) },

    // --- System ---
    Command { phrases: &["captura la pantalla", "haz una captura"], name: "captura completa",
              action: Action::Key(key::DIGIT_3, Mods::CMD_SHIFT) },
    Command { phrases: &["recorta la pantalla"], name: "captura de zona",
              action: Action::Key(key::DIGIT_4, Mods::CMD_SHIFT) },
    Command { phrases: &["abre spotlight"], name: "Spotlight",
              action: Action::Key(key::SPACE, Mods::CMD) },
    Command { phrases: &["bloquea la pantalla"], name: "bloquear pantalla",
              action: Action::Key(key::Q, Mods::CTRL_CMD) },

    // --- Sound ---
    Command { phrases: &["sube el volumen"], name: "subir volumen",
              action: Action::Volume(10) },
    Command { phrases: &["baja el volumen"], name: "bajar volumen",
              action: Action::Volume(-10) },
    Command { phrases: &["quita el sonido"], name: "silenciar",
              action: Action::Mute(true) },
    Command { phrases: &["devuelve el sonido"], name: "quitar silencio",
              action: Action::Mute(false) },

    // --- Media ---
    Command { phrases: &["pon la musica"], name: "reproducir",
              action: Action::Script("tell application \"Spotify\" to play") },
    Command { phrases: &["para la musica"], name: "pausar",
              action: Action::Script("tell application \"Spotify\" to pause") },
    Command { phrases: &["pon la siguiente cancion", "siguiente cancion"], name: "canción siguiente",
              action: Action::Script("tell application \"Spotify\" to next track") },
    Command { phrases: &["pon la cancion anterior", "cancion anterior"], name: "canción anterior",
              action: Action::Script("tell application \"Spotify\" to previous track") },

    // --- Minion itself ---
    Command { phrases: &["deja de escuchar", "duermete", "duerme", "silenciate",
                         "apagate", "descansa", "callate"], name: "dormir",
              action: Action::Sleep },
];

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
/// one of the sites in [`SITES`] named on its own.
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
        .find_map(|w| SITES.iter().find(|(name, _)| name == w))
        .map(|(_, url)| Website::Named((*url).to_string()))
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

/// Finds an application named in the sentence, with its match score.
///
/// Whole words only, in three grades: named outright, run together with
/// another word, or one slip away from the name. Bigger mangles stay in
/// the alias lists — what the recogniser really writes ("shafari",
/// "cromo") is listed, which is explicit and cannot spread.
fn find_app(rest: &str) -> Option<(&'static App, f32)> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut best: Option<(&App, f32)> = None;
    for app in all_apps() {
        for alias in app.aliases {
            // A plain mention beats a run-together one, which beats a
            // misheard one; longer aliases beat shorter ones, so "vs code"
            // wins over a stray "code".
            let score = if names_alias(&words, alias) {
                0.9 + (alias.len() as f32 / 100.0).min(0.09)
            } else if words.iter().any(|word| run_together(word, alias)) {
                0.9
            } else if words.iter().any(|word| near_alias(word, alias)) {
                0.8
            } else {
                0.0
            };
            if score >= threshold() && best.is_none_or(|(_, b)| score > b) {
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
        .any(|word| SITES.iter().any(|(name, _)| name == word))
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
    for (phrases, decision) in [
        (["empieza a dictar", "modo dictado"], Decision::StartDictation),
        (["deja de dictar", "fin del dictado"], Decision::StopDictation),
        (["deshaz lo que has hecho", "anula eso"], Decision::UndoLast),
    ] {
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
        for command in CONTEXTUAL_COMMANDS {
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
    // The user's own are searched alongside the built-in ones.
    let mut best: Option<(&Command, f32)> = None;
    for command in COMMANDS.iter().chain(USER_COMMANDS.get().into_iter().flatten()) {
        for phrase in command.phrases {
            let score = similarity(rest, phrase);
            if score >= threshold() && best.is_none_or(|(_, b)| score > b) {
                best = Some((command, score));
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

    // Phrasings the user added, or that were learned from the log. The
    // target may be any command with that name, the user's own included.
    for (name, phrase) in USER_ALIASES.get().into_iter().flatten() {
        let score = similarity(rest, phrase);
        if score < threshold() || !best.is_none_or(|(_, b)| score > b) {
            continue;
        }
        if let Some(command) = named_command(name) {
            best = Some((command, score));
        } else if let Some(bundle) = context {
            // A contextual command only exists where it applies, so this
            // is the one place it can be reached by name.
            if CONTEXTUAL_COMMANDS
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
        Some((command, score)) => (Decision::Run(command.name), score),
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
                    let name = APPS
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
            let command = CONTEXTUAL_COMMANDS.iter().find(|c| c.name == *name)?;
            Some(Done {
                description: (*name).to_string(),
                outcome: run_action(command.action),
            })
        }
        Decision::Run(name) => {
            let command = COMMANDS
                .iter()
                .chain(USER_COMMANDS.get().into_iter().flatten())
                .find(|c| c.name == *name)?;
            Some(Done {
                description: (*name).to_string(),
                outcome: run_action(command.action),
            })
        }
        _ => None,
    }
}

fn run_action(action: Action) -> Result<(), String> {
    match action {
        Action::Key(code, mods) => actions::press(code, mods),
        Action::Volume(delta) => actions::adjust_volume(delta),
        Action::Mute(muted) => actions::set_muted(muted),
        Action::Script(script) => actions::applescript(script),
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
    for command in COMMANDS {
        for candidate in command.phrases {
            let score = similarity(rest, candidate);
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((command.name, score));
            }
        }
    }
    best
}

/// The whole vocabulary, written out for someone to read.
///
/// Generated from the tables rather than kept alongside them: a list of
/// commands that has to be updated by hand is a list that goes stale, and
/// the first thing anyone needs is to know what they can say.
pub fn catalogue() -> String {
    use std::fmt::Write as _;
    let wake = wake_words().first().copied().unwrap_or("minion");
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
        "Para abrir: {}\nPara cerrar: {}\nO solo el nombre: «{wake}, Spotify»\n",
        APP_VERBS.join(", "),
        QUIT_VERBS.join(", ")
    );
    for app in all_apps() {
        let _ = writeln!(out, "  {:<22} {}", app.name, app.aliases.join(" · "));
    }

    let _ = writeln!(out, "\n── ÓRDENES ──────────────────────────\n");
    for command in COMMANDS.iter().chain(USER_COMMANDS.get().into_iter().flatten()) {
        let _ = writeln!(
            out,
            "  {:<22} {}",
            command.name,
            command.phrases.join(" · ")
        );
    }

    let _ = writeln!(out, "\n── SEGÚN DÓNDE ESTÉS ────────────────\n");
    for command in CONTEXTUAL_COMMANDS {
        let apps: Vec<&str> = command
            .bundles
            .iter()
            .map(|bundle| {
                APPS.iter()
                    .find(|a| a.bundle_id == *bundle)
                    .map_or(*bundle, |a| a.name)
            })
            .collect();
        let _ = writeln!(
            out,
            "  {:<22} {}\n  {:<22} en {}",
            command.name,
            command.phrases.join(" · "),
            "",
            apps.join(", ")
        );
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
    let from_commands: usize = COMMANDS.iter().map(|c| c.phrases.len()).sum();
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
            let expected = COMMANDS
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
        for command in CONTEXTUAL_COMMANDS {
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
        assert_eq!(decide("minion deja de dictar").0, Decision::StopDictation);
        assert_eq!(decide("minion fin del dictado").0, Decision::StopDictation);
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
        let own = [Command {
            phrases: &["haz lo mio"],
            name: "lo mío",
            action: Action::Key(key::A, Mods::CMD),
        }];
        assert_eq!(resolve_target("guardar", &own), Target::Global);
        assert_eq!(resolve_target("lo mío", &own), Target::User);
        assert_eq!(resolve_target("interrumpir", &own), Target::Contextual);
        // The whole point: a name that resolves to nothing is found now,
        // not in silence at the microphone.
        assert_eq!(resolve_target("atras", &own), Target::Unknown);
        assert_eq!(resolve_target("atrás", &own), Target::Global);
        assert_eq!(resolve_target("lo mio", &own), Target::Unknown);
    }

    #[test]
    fn no_phrase_is_declared_twice() {
        // Sharing a phrase between commands makes the winner depend on table
        // order, which is a latent bug rather than a choice.
        let mut seen = std::collections::HashMap::new();
        for command in COMMANDS {
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
        // shadowed by a similar one elsewhere in the table.
        for command in COMMANDS {
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
}

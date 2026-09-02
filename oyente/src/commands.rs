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
pub const DEFAULT_WAKE_WORDS: &[&str] =
    &["ordenador", "ordenadora", "computador", "computadora"];

/// Set once at startup from the configuration file. Absent means defaults.
static USER_APPS: OnceLock<Vec<App>> = OnceLock::new();
static USER_WAKE_WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
static USER_THRESHOLD: OnceLock<f32> = OnceLock::new();

/// Applies the user configuration. Call once, before anything is decided.
pub fn configure(config: &Config) {
    let extra = config.extra_apps();
    if !extra.is_empty() {
        let _ = USER_APPS.set(extra);
    }
    if let Some(words) = config.wake_words() {
        let _ = USER_WAKE_WORDS.set(words);
    }
    if let Some(threshold) = config.threshold {
        let _ = USER_THRESHOLD.set(threshold.clamp(0.3, 1.0));
    }
}

/// Wake words in force: the user's if configured, otherwise the defaults.
fn wake_words() -> &'static [&'static str] {
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
          aliases: &["chrome", "crome", "cromo", "el navegador", "navegador"] },
    App { name: "Safari", bundle_id: "com.apple.Safari", aliases: &["safari"] },
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
    // Ir a una pestaña concreta. El reconocedor devuelve el número como
    // dígito ("pestaña 1"), así que esa es la forma principal.
    Command { phrases: &["pestana 1", "pestana uno", "primera pestana"], name: "pestaña 1",
              action: Action::Key(key::DIGIT_1, Mods::CMD) },
    Command { phrases: &["pestana 2", "pestana dos"], name: "pestaña 2",
              action: Action::Key(key::DIGIT_2, Mods::CMD) },
    Command { phrases: &["pestana 3", "pestana tres"], name: "pestaña 3",
              action: Action::Key(key::DIGIT_3, Mods::CMD) },
    Command { phrases: &["pestana 4", "pestana cuatro"], name: "pestaña 4",
              action: Action::Key(key::DIGIT_4, Mods::CMD) },
    Command { phrases: &["pestana 5", "pestana cinco"], name: "pestaña 5",
              action: Action::Key(key::DIGIT_5, Mods::CMD) },
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

    // --- Oyente itself ---
    Command { phrases: &["deja de escuchar"], name: "dormir",
              action: Action::Sleep },
];

/// What was decided, before anything has been done about it.
///
/// Deciding and acting are deliberately separate: it makes the vocabulary
/// testable without applications opening for real.
#[derive(Debug, PartialEq, Eq)]
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
    /// Run a command from the table, identified by name.
    Run(&'static str),
    /// Started with the wake word, but nothing was recognised.
    Unrecognised,
    /// Not addressed to the machine.
    Ignored,
}

/// Strips the wake word. Returns `None` if the sentence is not a command.
fn strip_wake_word(phrase: &str) -> Option<&str> {
    let first = phrase.split_whitespace().next()?;
    wake_words()
        .contains(&first)
        .then(|| phrase[first.len()..].trim())
}

/// Extracts text to be typed, if the sentence asks for dictation.
///
/// Works on the original transcript rather than the normalised form: what
/// gets typed must keep its accents and capitals. Only the first two words
/// are inspected — wake word, then dictation verb — and everything after
/// them is content, however much it looks like a command.
fn dictation_text(transcript: &str) -> Option<String> {
    let words: Vec<&str> = transcript.split_whitespace().collect();
    if words.len() < 3 {
        return None;
    }
    if !wake_words().contains(&normalise(words[0]).as_str()) {
        return None;
    }
    let verb = spanish::canonical_verb(&normalise(words[1])).to_string();
    if !DICTATION_VERBS.contains(&verb.as_str()) {
        return None;
    }
    let text = words[2..].join(" ");
    // "pon la música" is a command, not a request to type "la música".
    // Requiring some length keeps short phrases out of dictation.
    (text.chars().count() >= 4).then_some(text)
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

/// Finds an application named in the sentence, with its match score.
fn find_app(rest: &str) -> Option<(&'static App, f32)> {
    let mut best: Option<(&App, f32)> = None;
    for app in all_apps() {
        for alias in app.aliases {
            // A literal mention beats a fuzzy one; longer aliases beat
            // shorter ones, so "vs code" wins over a stray "code".
            let score = if rest.contains(alias) {
                0.9 + (alias.len() as f32 / 100.0).min(0.09)
            } else {
                similarity(rest, alias)
            };
            if score >= threshold() && best.is_none_or(|(_, b)| score > b) {
                best = Some((app, score));
            }
        }
    }
    best
}

/// Works out what a transcription means with no application context.
#[cfg(test)]
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
    let mut best: Option<(&Command, f32)> = None;
    for command in COMMANDS {
        for phrase in command.phrases {
            let score = similarity(rest, phrase);
            if score >= threshold() && best.is_none_or(|(_, b)| score > b) {
                best = Some((command, score));
            }
        }
    }

    // Applications. The leading verb decides what happens to the one named:
    // opening it, closing it, or nothing at all. Without this check, "cierra
    // Safari" would launch Safari, because the name alone used to be enough.
    let spoken_words = keywords(rest);
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
    if find_app(rest).is_none() {
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

    if let Some((app, score)) = find_app(rest) {
        let app_wins = best.is_none_or(|(_, b)| score > b);
        if asks_to_quit && app_wins {
            return (
                Decision::Quit { name: app.name, bundle_id: app.bundle_id },
                score,
            );
        }
        // Named without a verb ("ordenador, Safari") still means open it.
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
    /// False when the action was refused by the system — in practice, a
    /// keystroke dropped for want of Accessibility permission.
    pub succeeded: bool,
}

/// Carries out a decision. Returns what was done, and whether it worked.
pub fn perform(decision: &Decision) -> Option<Done> {
    match decision {
        Decision::Launch { name, bundle_id } => Some(Done {
            description: format!("abrir {name}"),
            succeeded: actions::open_app(bundle_id),
        }),
        Decision::Quit { name, bundle_id } => Some(Done {
            description: format!("cerrar {name}"),
            succeeded: actions::quit_app(bundle_id),
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
            succeeded: actions::open_url(url, *in_browser),
        }),
        Decision::Type(text) => Some(Done {
            description: format!("escribir «{text}»"),
            succeeded: actions::type_text(text),
        }),
        Decision::RunHere(name) => {
            let command = CONTEXTUAL_COMMANDS.iter().find(|c| c.name == *name)?;
            Some(Done {
                description: (*name).to_string(),
                succeeded: run_action(command.action),
            })
        }
        Decision::Run(name) => {
            let command = COMMANDS.iter().find(|c| c.name == *name)?;
            Some(Done {
                description: (*name).to_string(),
                succeeded: run_action(command.action),
            })
        }
        _ => None,
    }
}

fn run_action(action: Action) -> bool {
    match action {
        Action::Key(code, mods) => actions::press(code, mods),
        Action::Volume(delta) => actions::adjust_volume(delta),
        Action::Mute(muted) => actions::set_muted(muted),
        Action::Script(script) => actions::applescript(script),
        Action::Sleep => true,
    }
}

/// Whether this decision asks Oyente to stop listening.
pub fn is_sleep(decision: &Decision) -> bool {
    matches!(decision, Decision::Run(name) if *name == "dormir")
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
        launches("Ordenador, abre Chrome.", "Chrome");
        launches("Ordenador, vete a Safari.", "Safari");
        launches("Ordenador, tráeme la terminal.", "Terminal");
        // Named outright, without a verb.
        launches("Ordenador, Spotify.", "Spotify");
    }

    #[test]
    fn survives_recogniser_slips() {
        // Both seen in the real log.
        launches("Ordenador Abrecrome.", "Chrome");
        launches("ordenador abre cromo", "Chrome");
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
        quits("Ordenador cierra Safari.", "Safari");
        quits("Ordenador Cierra Safari.", "Safari");
        quits("Ordenador Sal de Safari.", "Safari");
        quits("ordenador cierra chrome", "Chrome");
        quits("ordenador sal de spotify", "Spotify");
    }

    #[test]
    fn naming_an_app_does_not_override_the_verb() {
        // The window and tab commands must survive an app name nearby.
        assert_eq!(decision("Ordenador cierra la pestaña."), Decision::Run("cerrar pestaña"));
        assert_eq!(decision("Ordenador cierra la ventana."), Decision::Run("cerrar ventana"));
        assert_eq!(decision("Ordenador minimiza la ventana."), Decision::Run("minimizar"));
    }

    #[test]
    fn runs_table_commands() {
        assert_eq!(decision("Ordenador, guarda esto."), Decision::Run("guardar"));
        assert_eq!(decision("Ordenador, sube el volumen."), Decision::Run("subir volumen"));
        assert_eq!(decision("Ordenador, pantalla completa."), Decision::Run("pantalla completa"));
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
        assert_eq!(decision("Ordenador, haz un pino."), Decision::Unrecognised);
        assert_eq!(decision("Ordenador, qué hora es."), Decision::Unrecognised);
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
            let heard = format!("ordenador {spoken}");
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
            launches(&format!("ordenador {spoken}"), "Chrome");
        }
    }

    #[test]
    fn phrases_that_failed_in_the_log_now_work() {
        // Straight from ~/Library/Logs/oyente.log, where each of these came
        // back "not understood".
        let cases: &[(&str, &str)] = &[
            ("ordenador pestaña anterior", "pestaña anterior"),
            ("Ordenador pestaña 1.", "pestaña 1"),
            ("Ordenador página atrás.", "atrás"),
            ("Ordenador página anterior.", "atrás"),
            ("Ordenador página siguiente.", "adelante"),
            ("Ordenador retroceder página.", "atrás"),
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
        match decide_in("ordenador abre youtube", Some("com.google.Chrome")).0 {
            Decision::Browse { in_browser, .. } => {
                assert_eq!(in_browser, Some("com.google.Chrome"));
            }
            other => panic!("expected a page, got {other:?}"),
        }
        // Outside a browser there is nothing to prefer, so the default wins.
        for elsewhere in [None, Some("com.apple.Terminal"), Some("com.apple.finder")] {
            match decide_in("ordenador abre youtube", elsewhere).0 {
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
        browses("Ordenador ir a google.com", "https://google.com");
        browses("ordenador abre studiolxd.es", "https://studiolxd.es");
        // Spoken aloud, the dot becomes a word.
        browses("ordenador ve a github punto com", "https://github.com");
    }

    #[test]
    fn a_domain_needs_no_verb() {
        // From the log: the recogniser runs the words together, leaving no
        // verb to recognise — but ".com" makes the intent unmistakable.
        browses("Ordenador abremarca.com", "https://abremarca.com");
        browses("Ordenador iramarca.com", "https://iramarca.com");
        browses("Ordenador ir a Marca.com", "https://marca.com");
    }

    #[test]
    fn the_wake_word_alone_is_not_an_error() {
        assert_eq!(decision("Ordenador."), Decision::Ignored);
    }

    #[test]
    fn opens_well_known_sites_by_name() {
        browses("ordenador abre youtube", "https://www.youtube.com");
        browses("ordenador ve a wikipedia", "https://es.wikipedia.org");
    }

    #[test]
    fn applications_still_win_over_sites() {
        // Chrome is an app in the table; it must not become a web search.
        launches("ordenador abre chrome", "Chrome");
        launches("ordenador abre safari", "Safari");
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
            let spoken = format!("ordenador {phrase}");
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
            decide_in("ordenador limpia la pantalla", Some("com.apple.Terminal")).0,
            Decision::RunHere("limpiar terminal")
        );
        assert_eq!(
            decide_in("ordenador limpia la pantalla", Some("com.google.Chrome")).0,
            Decision::Unrecognised
        );
        assert_eq!(
            decide_in("ordenador limpia la pantalla", None).0,
            Decision::Unrecognised
        );
    }

    #[test]
    fn the_same_phrase_can_differ_by_application() {
        // Chrome and Finder both know "nueva ventana", but only Finder
        // knows "nueva carpeta".
        assert_eq!(
            decide_in("ordenador nueva carpeta", Some("com.apple.finder")).0,
            Decision::RunHere("carpeta nueva")
        );
        assert_eq!(
            decide_in("ordenador abre una ventana nueva", Some("com.apple.finder")).0,
            Decision::Run("ventana nueva")
        );
    }

    #[test]
    fn global_commands_still_work_inside_an_application() {
        assert_eq!(
            decide_in("ordenador guarda esto", Some("com.apple.Terminal")).0,
            Decision::Run("guardar")
        );
    }

    #[test]
    fn every_contextual_command_recognises_itself() {
        for command in CONTEXTUAL_COMMANDS {
            for phrase in command.phrases {
                let spoken = format!("ordenador {phrase}");
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
        types("Ordenador escribe hola qué tal estás",  "hola qué tal estás");
        types("Ordenador, anota comprar pan mañana", "comprar pan mañana");
        // Accents and capitals survive: the text comes from the original
        // transcript, not the normalised form used for matching.
        types("Ordenador escribe Señor Muñoz", "Señor Muñoz");
    }

    #[test]
    fn dictated_text_is_never_matched_as_a_command() {
        // The whole point: everything after the verb is content, however
        // much it looks like something in the vocabulary.
        types("Ordenador escribe cierra la ventana", "cierra la ventana");
        types("Ordenador escribe sube el volumen", "sube el volumen");
    }

    #[test]
    fn short_phrases_stay_commands() {
        // "pon la música" must not become a request to type "la música".
        assert_eq!(decision("Ordenador pon la música."), Decision::Run("reproducir"));
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
                let spoken = format!("ordenador {phrase}");
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
                let spoken = format!("ordenador abre {alias}");
                match decide(&spoken).0 {
                    Decision::Launch { name, .. } if name == app.name => {}
                    other => panic!("«{spoken}» should launch {}, got {other:?}", app.name),
                }
            }
        }
    }
}

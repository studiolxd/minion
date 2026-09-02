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

/// Works out what a transcription means. Performs no action.
pub fn decide(transcript: &str) -> (Decision, f32) {
    let normalised = normalise(transcript);
    let Some(rest) = strip_wake_word(&normalised) else {
        return (Decision::Ignored, 0.0);
    };
    if rest.is_empty() {
        return (Decision::Unrecognised, 0.0);
    }

    // Table commands first: they are more specific than "open something".
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
        Decision::Run(name) => {
            let command = COMMANDS.iter().find(|c| c.name == *name)?;
            let succeeded = match command.action {
                Action::Key(code, mods) => actions::press(code, mods),
                Action::Volume(delta) => actions::adjust_volume(delta),
                Action::Mute(muted) => actions::set_muted(muted),
                Action::Script(script) => actions::applescript(script),
                Action::Sleep => true,
            };
            Some(Done { description: (*name).to_string(), succeeded })
        }
        _ => None,
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

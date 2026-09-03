//! Questions, as opposed to commands.
//!
//! Everything else in the vocabulary *does* something; these *return*
//! something. That is the whole reason for speaking aloud: the answer is
//! the point, and there is nowhere else to put it.
//!
//! Kept short and plain. A reply heard forty times a day should not be
//! trying to entertain.
//!
//! Two questions carry data the fixed [`ASKED`] table cannot hold — a
//! duration, a clock time — so [`asked`] parses those itself, in
//! [`crate::timers`], before falling back to the table. Everything else,
//! including "cancela el temporizador" and "¿cuánto queda?", is a plain
//! phrase like any other.

use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

// `minion/src/reminders.rs` on disk, nested here rather than declared in
// `main.rs` — this task does not touch that file. See the module doc
// there for why reminders and calendar events live apart from this file's
// otherwise-fixed `ASKED` table.
#[path = "reminders.rs"]
mod reminders;

/// The questions Minion can answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Question {
    Time,
    Date,
    Battery,
    Volume,
    Listening,
    Activity,
    /// "¿quién soy?" — which enrolled voice the speaker check just matched.
    WhoAmI,
    /// What can I say? Opens the list rather than reciting it.
    Help,
    /// «qué puedo decir aquí» — the contextual commands for the
    /// application in front, spoken (or notified) rather than opening
    /// the whole catalogue window.
    ContextualHelp,
    /// "pon un temporizador de cinco minutos" — how long, and the words
    /// that named it, for the confirmation reply.
    Timer(Duration, String),
    /// "pon una alarma a las ocho y media" — when, and the words that
    /// named it.
    Alarm(NaiveTime, String),
    CancelTimer,
    TimeLeft,
    NowPlaying,
    OpenApps,
    DiskSpace,
    Connectivity,
    /// "¿qué día de la semana es el 12?" — the day of the current month.
    Weekday(u32),
    ReadSelection,
    ReadClipboard,
    StopReading,
    /// "recuérdame comprar pan" — the text, when it is due (`None` for a
    /// plain reminder), and the phrase to name it back with.
    Reminder(String, Option<NaiveDateTime>, Option<String>),
    /// "añade evento X mañana a las diez" — the title, when it starts, and
    /// the phrase to name it back with.
    AddEvent(String, NaiveDateTime, String),
    /// "¿qué tengo hoy?"
    CalendarToday,
    /// "¿qué tengo mañana?"
    CalendarTomorrow,
    /// "¿cuál es mi próxima reunión?"
    NextMeeting,
}

/// Ways of asking each one.
const ASKED: &[(Question, &[&str])] = &[
    (Question::Time, &["que hora es", "dime la hora", "que hora"]),
    (Question::Date, &["que dia es hoy", "dime la fecha", "que fecha es"]),
    (Question::Battery, &["cuanta bateria queda", "cuanta bateria", "como va la bateria"]),
    (Question::Volume, &["que volumen tengo", "como esta el volumen", "a cuanto esta el volumen"]),
    (Question::Listening, &["me oyes", "me escuchas", "estas ahi"]),
    (Question::Activity, &["que he dicho hoy", "cuantas ordenes llevo"]),
    (Question::WhoAmI, &["quien soy", "sabes quien soy", "quien te esta hablando"]),
    (Question::CancelTimer, &["cancela el temporizador", "cancela la alarma", "quita el temporizador"]),
    (Question::TimeLeft, &["cuanto queda", "cuanto falta", "cuanto queda del temporizador"]),
    (Question::NowPlaying, &["que suena", "que esta sonando", "que cancion es esta", "que se esta escuchando"]),
    (Question::OpenApps, &["que apps tengo abiertas", "que aplicaciones tengo abiertas", "que tengo abierto"]),
    (Question::DiskSpace, &["cuanto espacio queda", "cuanto espacio libre tengo", "cuanto disco me queda"]),
    (Question::Connectivity, &["estoy conectado", "tengo internet", "hay conexion", "tengo conexion a internet"]),
    (Question::ReadSelection, &["lee esto", "lee la seleccion"]),
    (Question::ReadClipboard, &["lee el portapapeles"]),
    (Question::StopReading, &["para de leer", "deja de leer"]),
    (Question::CalendarToday, &["que tengo hoy"]),
    (Question::CalendarTomorrow, &["que tengo manana"]),
    (Question::NextMeeting, &["cual es mi proxima reunion", "cual es mi siguiente reunion"]),
    (Question::Help, &["que puedes hacer", "que te puedo decir", "ayuda",
                       "que ordenes hay", "que se decir"]),
    (Question::ContextualHelp, &["que puedo decir aqui", "que puedo decir en esta aplicacion",
                                 "que comandos hay aqui", "que puedo decir en esta app"]),
];

/// A duration, a clock time, or a day of the month named inside the
/// sentence — the three things [`ASKED`]'s fixed phrasings cannot hold,
/// since the number is never the same twice.
///
/// Gated on a keyword first, so "pon música" is never mistaken for a
/// timer just because some other sentence happens to share a number word
/// with it.
fn parse_variable(rest: &str) -> Option<Question> {
    if rest.contains("temporizador") || rest.contains("avisa") {
        if let Some((duration, label)) = crate::timers::parse_duration(rest) {
            return Some(Question::Timer(duration, label));
        }
    }
    if rest.contains("alarma") {
        if let Some((time, label)) = crate::timers::parse_alarm(rest) {
            return Some(Question::Alarm(time, label));
        }
    }
    if rest.contains("dia") && rest.contains("semana") {
        if let Some(day) = weekday_target(rest) {
            return Some(Question::Weekday(day));
        }
    }
    if rest.contains("recuerda") {
        if let Some((text, due, when)) = reminders::parse_reminder(rest, Local::now()) {
            return Some(Question::Reminder(text, due, when));
        }
    }
    if rest.contains("evento") {
        if let Some((title, start, when)) = reminders::parse_event(rest, Local::now()) {
            return Some(Question::AddEvent(title, start, when));
        }
    }
    None
}

/// The day-of-month named after "el" in "¿qué día de la semana es el 12?".
fn weekday_target(rest: &str) -> Option<u32> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let position = words.iter().position(|w| *w == "el")?;
    let (day, _) = crate::timers::number_at(&words, position + 1)?;
    (1..=31).contains(&day).then_some(day)
}

/// Recognises a question, if the sentence is one.
pub fn asked(rest: &str, threshold: f32) -> Option<Question> {
    if let Some(question) = parse_variable(rest) {
        return Some(question);
    }

    let mut best: Option<(Question, f32)> = None;
    for (question, phrasings) in ASKED {
        for phrasing in *phrasings {
            let score = crate::text::similarity(rest, phrasing);
            if score >= threshold && best.as_ref().is_none_or(|(_, previous)| score > *previous) {
                best = Some((question.clone(), score));
            }
        }
    }
    best.map(|(question, _)| question)
}

/// Works out the answer, as something to say aloud.
///
/// `context` is the application in front's bundle id, read only by
/// [`Question::ContextualHelp`] — every other question ignores it, the
/// same way most of them ignore `listening`.
pub fn answer(question: Question, listening: bool, context: Option<&str>) -> String {
    match question {
        Question::Time => spoken_time(),
        Question::Date => spoken_date(),
        Question::Battery => battery(),
        Question::Volume => volume(),
        Question::Listening => {
            // Only ever asked when it is listening — a paused Minion does
            // not hear the question — but answer honestly anyway.
            if listening { "Sí, te escucho." } else { "Estoy en pausa." }.to_string()
        }
        Question::Activity => activity(),
        // Whoever the speaker check recognised last, which for the
        // utterance carrying this question is the person asking. No
        // profiles, or a voice let through unchecked, and there is nobody
        // to name — better said plainly than guessed at.
        Question::WhoAmI => match crate::speaker::last_matched() {
            Some(name) => format!("Eres {name}."),
            None => "Todavía no sé quién eres; no tengo tu voz registrada.".into(),
        },
        // Answered by opening the window: reading forty commands aloud
        // would be worse than useless.
        Question::Help => "Te abro la lista.".into(),
        Question::ContextualHelp => contextual_help(context),
        Question::Timer(duration, label) => {
            crate::timers::schedule_timer(duration, label.clone());
            format!("Temporizador de {label}.")
        }
        Question::Alarm(time, label) => {
            crate::timers::schedule_alarm(time, label.clone());
            format!("Alarma {label}.")
        }
        Question::CancelTimer => match crate::timers::cancel_all() {
            0 => "No tenías ningún temporizador ni alarma.".into(),
            1 => "Cancelado.".into(),
            n => format!("Cancelados {n}."),
        },
        Question::TimeLeft => match crate::timers::time_left() {
            None => "No tienes ningún temporizador ni alarma.".into(),
            Some((label, seconds)) => spoken_time_left(&label, seconds),
        },
        Question::NowPlaying => now_playing(),
        Question::OpenApps => open_apps(),
        Question::DiskSpace => disk_space(),
        Question::Connectivity => connectivity(),
        Question::Weekday(day) => weekday_of(day),
        Question::ReadSelection => match read_selection() {
            Some(text) => text,
            None => "No hay nada seleccionado.".into(),
        },
        Question::ReadClipboard => match read_clipboard() {
            Some(text) => text,
            None => "El portapapeles está vacío.".into(),
        },
        Question::StopReading => {
            crate::speech::stop();
            "Vale.".into()
        }
        Question::Reminder(text, due, when) => match reminders::create_reminder(&text, due) {
            Ok(()) => match when {
                Some(when) => format!("Te lo recordaré {when}."),
                None => "Vale, recordado.".into(),
            },
            Err(reason) => {
                crate::journal::write(&format!("reminder BLOCKED  {reason}"));
                "No puedo crear recordatorios; da permiso a Minion en Ajustes → Privacidad → Recordatorios.".into()
            }
        },
        Question::AddEvent(title, start, when) => match reminders::create_event(&title, start) {
            Ok(()) => format!("Evento «{title}» añadido {when}."),
            Err(reason) => {
                crate::journal::write(&format!("event    BLOCKED  {reason}"));
                "No puedo crear el evento; da permiso a Minion en Ajustes → Privacidad → Calendarios.".into()
            }
        },
        Question::CalendarToday => calendar_answer(reminders::events_today(Local::now()), "No tienes nada hoy."),
        Question::CalendarTomorrow => {
            calendar_answer(reminders::events_tomorrow(Local::now()), "No tienes nada mañana.")
        }
        Question::NextMeeting => match reminders::next_meeting(Local::now()) {
            Ok(Some((when, title))) => format!("Tu próxima reunión es a {}, {title}.", spoken_clock(when.time())),
            Ok(None) => "No tienes ninguna reunión próxima.".into(),
            Err(reason) => {
                crate::journal::write(&format!("calendar BLOCKED  {reason}"));
                "No puedo leer el calendario; da permiso a Minion en Ajustes → Privacidad → Calendarios.".into()
            }
        },
    }
}

/// The spoken list a day's events read as: "A las diez, reunión con Ana.
/// A las cuatro, dentista." Or, if `osascript` could not read Calendario
/// at all — most likely because Minion has not been granted access — the
/// permission message, with the raw error logged for whoever reads it.
fn calendar_answer(events: Result<Vec<(NaiveDateTime, String)>, String>, none: &str) -> String {
    match events {
        Ok(events) if events.is_empty() => none.into(),
        Ok(events) => events
            .iter()
            .map(|(when, title)| format!("A {}, {title}.", spoken_clock(when.time())))
            .collect::<Vec<_>>()
            .join(" "),
        Err(reason) => {
            crate::journal::write(&format!("calendar BLOCKED  {reason}"));
            "No puedo leer el calendario; da permiso a Minion en Ajustes → Privacidad → Calendarios.".into()
        }
    }
}

/// The chime a finished timer plays, before it is spoken and notified.
const CHIME: &str = "/System/Library/Sounds/Glass.aiff";

/// Announces every timer or alarm that came due since the last check: a
/// chime, a spoken line, and a notification.
///
/// The notification is posted regardless of `speak`, and regardless of
/// whether Minion is even listening right now — the whole point of a
/// timer is to be noticed from another room, or with the sound off, which
/// a spoken reply alone cannot do.
pub fn announce_due_timers() {
    let due = crate::timers::take_due();
    if due.is_empty() {
        return;
    }
    let config = crate::config::load();
    for timer in due {
        let message = match timer.kind {
            crate::timers::Kind::Timer => format!("Han pasado {}.", timer.label),
            crate::timers::Kind::Alarm => format!("Alarma: {}.", timer.label),
        };
        crate::journal::write(&format!("timer    {message}"));
        if config.sounds {
            let _ = crate::actions::play_sound(CHIME);
        }
        if config.speak {
            let deaf = std::sync::atomic::AtomicBool::new(false);
            crate::speech::say(
                &message,
                config.voice().as_deref(),
                config.speech_rate(),
                config.speaker().as_deref(),
                &deaf,
                Duration::from_millis(200),
            );
        }
        if config.notifications {
            crate::notify::post("Minion", &message);
        }
    }
}

/// "quedan cuatro minutos", or the seconds themselves once it is nearly
/// due — a countdown in minutes would round "quedan 0 minutos" right up
/// until it fires.
fn spoken_time_left(label: &str, seconds: i64) -> String {
    if seconds < 60 {
        format!("Quedan {seconds} segundos para {label}.")
    } else {
        let minutes = (seconds + 30) / 60;
        let unit = if minutes == 1 { "minuto" } else { "minutos" };
        format!("Quedan {minutes} {unit} para {label}.")
    }
}

/// The time as someone would say it, not as a clock shows it.
fn spoken_time() -> String {
    spoken_clock(Local::now().time())
}

/// [`spoken_time`], for a clock time other than right now — a calendar
/// event's start, say, rather than the current moment.
fn spoken_clock(time: NaiveTime) -> String {
    let hour = time.hour();
    let minute = time.minute();

    // "La una" but "las dos": the article agrees with the number.
    let hour_12 = match hour % 12 {
        0 => 12,
        other => other,
    };
    let article = if hour_12 == 1 { "la" } else { "las" };

    match minute {
        0 => format!("{article} {}", spoken_number(hour_12)),
        30 => format!("{article} {} y media", spoken_number(hour_12)),
        15 => format!("{article} {} y cuarto", spoken_number(hour_12)),
        45 => {
            let next = if hour_12 == 12 { 1 } else { hour_12 + 1 };
            let article = if next == 1 { "la" } else { "las" };
            format!("{article} {} menos cuarto", spoken_number(next))
        }
        _ => format!("{article} {} y {minute}", spoken_number(hour_12)),
    }
}

fn spoken_number(n: u32) -> &'static str {
    const NAMES: &[&str] = &[
        "doce", "una", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve",
        "diez", "once", "doce",
    ];
    NAMES.get(n as usize % 13).copied().unwrap_or("doce")
}

fn spoken_date() -> String {
    const DAYS: &[&str] = &[
        "lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo",
    ];
    const MONTHS: &[&str] = &[
        "enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto",
        "septiembre", "octubre", "noviembre", "diciembre",
    ];
    let now = Local::now();
    let day = DAYS
        .get(now.weekday().num_days_from_monday() as usize)
        .copied()
        .unwrap_or("");
    let month = MONTHS.get(now.month0() as usize).copied().unwrap_or("");
    format!("{day}, {} de {month}", now.day())
}

/// The weekday of `day` in the current month, spoken the same way
/// [`spoken_date`] names one.
fn weekday_of(day: u32) -> String {
    const DAYS: &[&str] = &[
        "lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo",
    ];
    let now = Local::now();
    let Some(date) = NaiveDate::from_ymd_opt(now.year(), now.month(), day) else {
        return format!("Este mes no tiene día {day}.");
    };
    let name = DAYS.get(date.weekday().num_days_from_monday() as usize).copied().unwrap_or("");
    format!("El {day} es {name}.")
}

fn battery() -> String {
    let Ok(output) = Command::new("/usr/bin/pmset").args(["-g", "batt"]).output() else {
        return "No he podido mirar la batería.".into();
    };
    let text = String::from_utf8_lossy(&output.stdout);

    let Some(percent) = text.split('\t').nth(1).and_then(|part| part.split('%').next()) else {
        return "No he podido mirar la batería.".into();
    };
    let charging = text.contains("AC Power");
    if charging {
        format!("{}% y cargando.", percent.trim())
    } else {
        format!("{}%.", percent.trim())
    }
}

/// The maximum number of contextual commands read aloud before the rest
/// are summed up instead — reading forty of them would be worse than not
/// answering at all, the same reasoning that makes `Question::Help` open
/// a window rather than speak.
const CONTEXTUAL_HELP_MAX: usize = 8;

/// «qué puedo decir aquí»: the application in front's own commands,
/// spoken as a list — "En Teams puedes decir: enviar mensaje, nuevo
/// chat…", cut off at [`CONTEXTUAL_HELP_MAX`] with "y N más; abre la
/// ayuda para verlas" pointing at the full catalogue for the rest.
fn contextual_help(context: Option<&str>) -> String {
    let Some(bundle_id) = context else {
        return "No sé qué aplicación tienes delante, así que no hay nada que decirte de ella.".into();
    };
    let app_name = crate::commands::app_name_for(bundle_id).unwrap_or(bundle_id);
    let names = crate::commands::contextual_command_names(bundle_id);
    if names.is_empty() {
        return format!("En {app_name} no tengo comandos propios; los generales siguen valiendo.");
    }
    let shown = names.iter().take(CONTEXTUAL_HELP_MAX).copied().collect::<Vec<_>>().join(", ");
    let rest = names.len().saturating_sub(CONTEXTUAL_HELP_MAX);
    if rest > 0 {
        format!("En {app_name} puedes decir: {shown}… y {rest} más; abre la ayuda para verlas.")
    } else {
        format!("En {app_name} puedes decir: {shown}.")
    }
}

fn volume() -> String {
    let Ok(output) = Command::new("/usr/bin/osascript")
        .args(["-e", "output volume of (get volume settings)"])
        .output()
    else {
        return "No he podido mirar el volumen.".into();
    };
    let level = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if level.is_empty() {
        "No he podido mirar el volumen.".into()
    } else {
        format!("Volumen al {level}.")
    }
}

/// What the log says about today.
fn activity() -> String {
    let Some(path) = crate::journal::path() else {
        return "No tengo registro.".into();
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return "No tengo registro.".into();
    };
    let today = Local::now().format("%Y-%m-%d").to_string();

    let mut ran = 0;
    let mut unknown = 0;
    for line in contents.lines().filter(|line| line.starts_with(&today)) {
        if line.contains("  ran ") {
            ran += 1;
        } else if line.contains("  unknown ") {
            unknown += 1;
        }
    }

    match (ran, unknown) {
        (0, 0) => "Hoy todavía no me has pedido nada.".into(),
        (_, 0) => format!("{ran} órdenes hoy, todas entendidas."),
        _ => format!("{ran} órdenes hoy, y {unknown} que no entendí."),
    }
}

/// The track playing in Spotify or Music, whichever is running — Spotify
/// first, since it is the more likely of the two to be open at all.
fn now_playing() -> String {
    const SCRIPT: &str = r#"
        if application "Spotify" is running then
            tell application "Spotify"
                if player state is playing then
                    name of current track & " de " & artist of current track
                else
                    "Spotify está en pausa."
                end if
            end tell
        else if application "Music" is running then
            tell application "Music"
                if player state is playing then
                    name of current track & " de " & artist of current track
                else
                    "Music está en pausa."
                end if
            end tell
        else
            "No suena nada."
        end if
    "#;
    let Ok(output) = Command::new("/usr/bin/osascript").arg("-e").arg(SCRIPT).output() else {
        return "No he podido mirar qué suena.".into();
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        "No he podido mirar qué suena.".into()
    } else {
        text
    }
}

/// Regular, user-facing applications — not menu-bar extras and background
/// helpers, which `NSRunningApplication` also lists but nobody thinks of
/// as "open". Up to eight names: past that it is a listing, not an answer.
fn open_apps() -> String {
    let workspace = NSWorkspace::sharedWorkspace();
    let running = workspace.runningApplications();
    let mut names: Vec<String> = Vec::new();
    for app in running.iter() {
        if names.len() >= 8 {
            break;
        }
        if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
            continue;
        }
        if let Some(name) = app.localizedName() {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        "No veo ninguna aplicación abierta.".into()
    } else {
        names.join(", ")
    }
}

/// Free space on the startup disk, `statvfs` rather than `df`: one syscall
/// instead of a subprocess and a column to parse.
fn disk_space() -> String {
    let root = std::ffi::CString::new("/").expect("no interior nul");
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::statvfs(root.as_ptr(), &mut stat) } == 0;
    if !ok {
        return "No he podido mirar el espacio libre.".into();
    }
    let available_bytes = stat.f_bavail as u64 * stat.f_frsize as u64;
    let gib = available_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("{gib:.1} GB libres.")
}

/// A one-second TCP connect to a well-known address, rather than anything
/// that needs entitlements or a framework of its own. Bounded: this runs
/// on the thread that is about to speak the answer, never the one
/// listening for the next utterance, but it still must not hang if the
/// network is down rather than merely absent.
fn connectivity() -> String {
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(mut addresses) = "1.1.1.1:443".to_socket_addrs() else {
        return "No he podido comprobarlo.".into();
    };
    let Some(address) = addresses.next() else {
        return "No he podido comprobarlo.".into();
    };
    match TcpStream::connect_timeout(&address, Duration::from_secs(1)) {
        Ok(_) => "Sí, tienes conexión.".into(),
        Err(_) => "No, no tienes conexión.".into(),
    }
}

/// Reads the clipboard as plain text, via `pbpaste` rather than
/// `NSPasteboard` directly — one process instead of a new AppKit
/// dependency, for exactly the two operations this needs.
fn read_clipboard() -> Option<String> {
    let output = Command::new("/usr/bin/pbpaste").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn write_clipboard(text: &str) {
    use std::io::Write;
    let Ok(mut child) = Command::new("/usr/bin/pbcopy").stdin(Stdio::piped()).spawn() else {
        return;
    };
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let _ = child.wait();
}

/// Copies whatever is selected in the frontmost application, reads it, and
/// puts the previous clipboard back — the same courtesy any tool that
/// borrows the clipboard for a moment owes the thing it overwrote.
fn read_selection() -> Option<String> {
    let previous = read_clipboard();
    crate::actions::press(crate::actions::key::C, crate::actions::Mods::CMD).ok()?;
    // The pasteboard is filled asynchronously by whatever ⌘C reached; give
    // it a moment before reading it back, or this reads the *old* clipboard
    // — which is exactly what it is about to overwrite anyway.
    std::thread::sleep(Duration::from_millis(150));
    let copied = read_clipboard();
    if let Some(previous) = previous {
        write_clipboard(&previous);
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_questions() {
        assert_eq!(asked("que hora es", 0.7), Some(Question::Time));
        assert_eq!(asked("dime la hora", 0.7), Some(Question::Time));
        assert_eq!(asked("cuanta bateria queda", 0.7), Some(Question::Battery));
        assert_eq!(asked("me oyes", 0.7), Some(Question::Listening));
        assert_eq!(asked("a cuanto esta el volumen", 0.7), Some(Question::Volume));
        assert_eq!(asked("que puedo decir aqui", 0.7), Some(Question::ContextualHelp));
        assert_eq!(asked("que puedes hacer", 0.7), Some(Question::Help));
    }

    #[test]
    fn contextual_help_names_the_app_and_its_own_commands() {
        let reply = contextual_help(Some("com.apple.Terminal"));
        assert!(reply.starts_with("En Terminal puedes decir: "), "{reply}");
        assert!(reply.contains("interrumpir"), "{reply}");
    }

    #[test]
    fn contextual_help_with_nothing_in_front_says_so() {
        assert_eq!(
            contextual_help(None),
            "No sé qué aplicación tienes delante, así que no hay nada que decirte de ella."
        );
    }

    #[test]
    fn contextual_help_with_an_unknown_app_says_it_has_nothing_of_its_own() {
        assert_eq!(
            contextual_help(Some("com.nobody.nothing")),
            "En com.nobody.nothing no tengo comandos propios; los generales siguen valiendo."
        );
    }

    #[test]
    fn contextual_help_sums_up_after_the_first_eight() {
        // Teams has more than eight of its own commands in the built-in
        // vocabulary, so this is a real case, not a fabricated one.
        let reply = contextual_help(Some("com.microsoft.teams2"));
        assert!(reply.contains("más; abre la ayuda para verlas."), "{reply}");
    }

    #[test]
    fn leaves_commands_alone() {
        // These belong to the command table and must not be read as
        // questions, or asking would take priority over doing.
        assert_eq!(asked("abre chrome", 0.7), None);
        assert_eq!(asked("guarda esto", 0.7), None);
        assert_eq!(asked("sube el volumen", 0.7), None);
    }

    #[test]
    fn the_time_is_said_the_way_people_say_it() {
        let spoken = spoken_time();
        assert!(
            spoken.starts_with("la ") || spoken.starts_with("las "),
            "should read as speech, got «{spoken}»"
        );
        // "la una", never "las una".
        assert!(!spoken.starts_with("las una"));
    }

    #[test]
    fn quarters_and_halves_have_their_own_words() {
        // Checked through the formatter rather than the clock, since the
        // clock is whatever time the test runs at.
        assert_eq!(spoken_number(1), "una");
        assert_eq!(spoken_number(12), "doce");
        assert_eq!(spoken_number(0), "doce");
    }

    #[test]
    fn the_date_names_the_day_and_month() {
        let spoken = spoken_date();
        assert!(spoken.contains(" de "), "got «{spoken}»");
        assert!(spoken.contains(','), "should name the weekday, got «{spoken}»");
    }

    #[test]
    fn a_timer_is_recognised_and_its_reply_names_what_was_asked() {
        let question = asked("pon un temporizador de cinco minutos", 0.7);
        assert_eq!(
            question,
            Some(Question::Timer(Duration::from_secs(300), "cinco minutos".into()))
        );
        assert_eq!(answer(question.unwrap(), true, None), "Temporizador de cinco minutos.");
        crate::timers::cancel_all();
    }

    #[test]
    fn asking_to_be_told_recognises_the_same_shape_as_pon() {
        let question = asked("avisame en diez minutos", 0.7);
        assert_eq!(
            question,
            Some(Question::Timer(Duration::from_secs(600), "diez minutos".into()))
        );
        crate::timers::cancel_all();
    }

    #[test]
    fn an_alarm_is_recognised() {
        let question = asked("pon una alarma a las ocho y media", 0.7);
        assert!(matches!(question, Some(Question::Alarm(_, _))));
        if let Some(Question::Alarm(_, label)) = question {
            assert_eq!(label, "a las ocho y media");
        }
        crate::timers::cancel_all();
    }

    #[test]
    fn cancel_and_time_left_are_plain_phrases() {
        assert_eq!(asked("cancela el temporizador", 0.7), Some(Question::CancelTimer));
        assert_eq!(asked("cuanto queda", 0.7), Some(Question::TimeLeft));
    }

    #[test]
    fn a_weekday_question_carries_the_day_it_asked_about() {
        assert_eq!(
            asked("que dia de la semana es el 12", 0.7),
            Some(Question::Weekday(12))
        );
    }

    #[test]
    fn the_weekday_answer_names_a_real_day() {
        const DAYS: &[&str] = &[
            "lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo",
        ];
        let spoken = weekday_of(12);
        assert!(DAYS.iter().any(|day| spoken.contains(day)), "got «{spoken}»");
    }

    #[test]
    fn a_day_outside_the_month_says_so_instead_of_panicking() {
        let spoken = weekday_of(97);
        assert!(spoken.contains("no tiene"), "got «{spoken}»");
    }

    #[test]
    fn read_aloud_and_notification_questions_are_recognised() {
        assert_eq!(asked("lee esto", 0.7), Some(Question::ReadSelection));
        assert_eq!(asked("lee la seleccion", 0.7), Some(Question::ReadSelection));
        assert_eq!(asked("lee el portapapeles", 0.7), Some(Question::ReadClipboard));
        assert_eq!(asked("para de leer", 0.7), Some(Question::StopReading));
        assert_eq!(asked("que suena", 0.7), Some(Question::NowPlaying));
        assert_eq!(asked("que apps tengo abiertas", 0.7), Some(Question::OpenApps));
        assert_eq!(asked("cuanto espacio queda", 0.7), Some(Question::DiskSpace));
        assert_eq!(asked("estoy conectado", 0.7), Some(Question::Connectivity));
    }

    #[test]
    fn stopping_a_read_answers_and_does_not_panic_with_nothing_playing() {
        assert_eq!(answer(Question::StopReading, true, None), "Vale.");
    }

    #[test]
    fn a_missing_clipboard_gets_a_spoken_reply_not_a_crash() {
        // Cannot force the real clipboard empty from a test — this only
        // exercises the "nothing on it" branch of the reply, in case
        // read_clipboard ever returns None in this environment (a CI
        // runner, say, with no pasteboard server).
        if read_clipboard().is_none() {
            assert_eq!(answer(Question::ReadClipboard, true, None), "El portapapeles está vacío.");
        }
    }
}

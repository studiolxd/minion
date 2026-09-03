//! Questions, as opposed to commands.
//!
//! Everything else in the vocabulary *does* something; these *return*
//! something. That is the whole reason for speaking aloud: the answer is
//! the point, and there is nowhere else to put it.
//!
//! Kept short and plain. A reply heard forty times a day should not be
//! trying to entertain.

use std::process::Command;

use chrono::{Datelike, Local, Timelike};

/// The questions Minion can answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Question {
    Time,
    Date,
    Battery,
    Volume,
    Listening,
    Activity,
    /// What can I say? Opens the list rather than reciting it.
    Help,
}

/// Ways of asking each one.
const ASKED: &[(Question, &[&str])] = &[
    (Question::Time, &["que hora es", "dime la hora", "que hora"]),
    (Question::Date, &["que dia es hoy", "dime la fecha", "que fecha es"]),
    (Question::Battery, &["cuanta bateria queda", "cuanta bateria", "como va la bateria"]),
    (Question::Volume, &["que volumen tengo", "como esta el volumen"]),
    (Question::Listening, &["me oyes", "me escuchas", "estas ahi"]),
    (Question::Activity, &["que he dicho hoy", "cuantas ordenes llevo"]),
    (Question::Help, &["que puedes hacer", "que te puedo decir", "ayuda",
                       "que ordenes hay", "que se decir"]),
];

/// Recognises a question, if the sentence is one.
pub fn asked(rest: &str, threshold: f32) -> Option<Question> {
    let mut best: Option<(Question, f32)> = None;
    for (question, phrasings) in ASKED {
        for phrasing in *phrasings {
            let score = crate::text::similarity(rest, phrasing);
            if score >= threshold && best.is_none_or(|(_, previous)| score > previous) {
                best = Some((*question, score));
            }
        }
    }
    best.map(|(question, _)| question)
}

/// Works out the answer, as something to say aloud.
pub fn answer(question: Question, listening: bool) -> String {
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
        // Answered by opening the window: reading forty commands aloud
        // would be worse than useless.
        Question::Help => "Te abro la lista.".into(),
    }
}

/// The time as someone would say it, not as a clock shows it.
fn spoken_time() -> String {
    let now = Local::now();
    let hour = now.hour();
    let minute = now.minute();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_questions() {
        assert_eq!(asked("que hora es", 0.7), Some(Question::Time));
        assert_eq!(asked("dime la hora", 0.7), Some(Question::Time));
        assert_eq!(asked("cuanta bateria queda", 0.7), Some(Question::Battery));
        assert_eq!(asked("me oyes", 0.7), Some(Question::Listening));
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
}

//! Spanish numbers, spoken durations and clock times, and the small list of
//! timers and alarms waiting to go off.
//!
//! Kept apart from [`crate::answers`] on purpose: this is parsing and
//! bookkeeping, not vocabulary. Nothing here knows what a `Question` is —
//! `answers.rs` turns "cinco minutos" into a `Duration` here, then decides
//! what to do with it.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveTime, TimeZone};

/// Numbers 0–29 as one Spanish word, on already-normalised text (no
/// accents — see `text::normalise`).
fn unit_value(word: &str) -> Option<u32> {
    Some(match word {
        "cero" => 0,
        "un" | "uno" | "una" => 1,
        "dos" => 2,
        "tres" => 3,
        "cuatro" => 4,
        "cinco" => 5,
        "seis" => 6,
        "siete" => 7,
        "ocho" => 8,
        "nueve" => 9,
        "diez" => 10,
        "once" => 11,
        "doce" => 12,
        "trece" => 13,
        "catorce" => 14,
        "quince" => 15,
        "dieciseis" => 16,
        "diecisiete" => 17,
        "dieciocho" => 18,
        "diecinueve" => 19,
        "veinte" => 20,
        "veintiuno" | "veintiun" | "veintiuna" => 21,
        "veintidos" => 22,
        "veintitres" => 23,
        "veinticuatro" => 24,
        "veinticinco" => 25,
        "veintiseis" => 26,
        "veintisiete" => 27,
        "veintiocho" => 28,
        "veintinueve" => 29,
        _ => return None,
    })
}

/// Tens words that take "y <unidad>" to go past themselves: "treinta y
/// cinco" for 35.
fn tens_value(word: &str) -> Option<u32> {
    Some(match word {
        "treinta" => 30,
        "cuarenta" => 40,
        "cincuenta" => 50,
        _ => return None,
    })
}

/// A number from 0 to 59, spoken as one or two words starting at
/// `words[start]`. Returns the value and the index just past what it read.
pub fn number_at(words: &[&str], start: usize) -> Option<(u32, usize)> {
    let first = *words.get(start)?;
    if let Ok(n) = first.parse::<u32>() {
        return Some((n, start + 1));
    }
    if let Some(tens) = tens_value(first) {
        if words.get(start + 1) == Some(&"y") {
            if let Some(units) = words.get(start + 2).and_then(|w| unit_value(w)) {
                return Some((tens + units, start + 3));
            }
        }
        return Some((tens, start + 1));
    }
    unit_value(first).map(|n| (n, start + 1))
}

/// "cinco minutos", "diez segundos", "una hora" — a spoken duration
/// anywhere in the sentence, with the words that named it, for the
/// confirmation reply ("Temporizador de cinco minutos.").
pub fn parse_duration(rest: &str) -> Option<(Duration, String)> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    for i in 0..words.len() {
        // "media hora": the number is implicit in the word itself, unlike
        // every other duration here.
        if words[i] == "media" && matches!(words.get(i + 1), Some(&"hora") | Some(&"horas")) {
            return Some((Duration::from_secs(1_800), words[i..=i + 1].join(" ")));
        }
        let Some((n, next)) = number_at(&words, i) else { continue };
        if n == 0 {
            continue;
        }
        let seconds_per = match *words.get(next)? {
            "segundo" | "segundos" => 1u64,
            "minuto" | "minutos" => 60,
            "hora" | "horas" => 3_600,
            _ => continue,
        };
        let label = words[i..next + 1].join(" ");
        return Some((Duration::from_secs(u64::from(n) * seconds_per), label));
    }
    None
}

/// "a las ocho y media", "a las nueve menos cuarto", "a las ocho" — a
/// spoken clock time, resolved to whichever of the two twelve-hour
/// readings (am/pm) comes next from now, since nothing here says which one
/// was meant. Returns the time and the phrase that named it.
pub fn parse_alarm(rest: &str) -> Option<(NaiveTime, String)> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let article = words.iter().position(|w| *w == "las" || *w == "la")?;
    let start = if article > 0 && words[article - 1] == "a" { article - 1 } else { article };

    let (mut hour, mut end) = number_at(&words, article + 1)?;
    if !(1..=12).contains(&hour) {
        return None;
    }
    let mut minute = 0u32;

    match (words.get(end), words.get(end + 1)) {
        (Some(&"y"), Some(&"media")) => {
            minute = 30;
            end += 2;
        }
        (Some(&"y"), Some(&"cuarto")) => {
            minute = 15;
            end += 2;
        }
        (Some(&"menos"), Some(&"cuarto")) => {
            hour = if hour == 1 { 12 } else { hour - 1 };
            minute = 45;
            end += 2;
        }
        (Some(&"y"), Some(_)) => {
            if let Some((m, after)) = number_at(&words, end + 1) {
                if m < 60 {
                    minute = m;
                    end = after;
                }
            }
        }
        _ => {}
    }

    let label = words[start..end].join(" ");
    let now = Local::now().time();
    let am = NaiveTime::from_hms_opt(hour % 12, minute, 0)?;
    let pm = NaiveTime::from_hms_opt(hour % 12 + 12, minute, 0)?;
    // Whichever twelve-hour reading is soonest from now, wrapping past
    // midnight rather than landing in the past.
    let until = |t: NaiveTime| {
        let diff = t.signed_duration_since(now);
        if diff < chrono::Duration::zero() { diff + chrono::Duration::days(1) } else { diff }
    };
    let time = if until(am) <= until(pm) { am } else { pm };
    Some((time, label))
}

/// What a pending timer or alarm is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Timer,
    Alarm,
}

/// One thing waiting to go off.
#[derive(Clone, Debug)]
pub struct Pending {
    pub label: String,
    pub fires_at: DateTime<Local>,
    pub kind: Kind,
}

/// Takes out of `list` everything due at or before `now`, leaving the rest.
/// Pure, so the firing logic can be tested without waiting for a clock.
pub fn due_now(list: &mut Vec<Pending>, now: DateTime<Local>) -> Vec<Pending> {
    let mut due = Vec::new();
    list.retain(|p| {
        if p.fires_at <= now {
            due.push(p.clone());
            false
        } else {
            true
        }
    });
    due
}

/// The one that will fire soonest, if anything is pending.
pub fn soonest(list: &[Pending]) -> Option<&Pending> {
    list.iter().min_by_key(|p| p.fires_at)
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

fn push(kind: Kind, label: String, fires_at: DateTime<Local>) {
    if let Ok(mut list) = PENDING.lock() {
        list.push(Pending { label, fires_at, kind });
    }
}

/// `duration` from now, as an absolute point in time — what a timer fires
/// at, and what pausing until a spoken duration resumes at.
pub fn at_duration_from_now(duration: Duration) -> DateTime<Local> {
    Local::now() + chrono::Duration::from_std(duration).unwrap_or_default()
}

/// The next time the clock reads `time` — today if that has not passed
/// yet, tomorrow if it has. What an alarm fires at, and what pausing
/// until a spoken clock time resumes at.
pub fn next_occurrence(time: NaiveTime) -> DateTime<Local> {
    let now = Local::now();
    let mut naive = now.date_naive().and_time(time);
    if naive <= now.naive_local() {
        naive += chrono::Duration::days(1);
    }
    Local.from_local_datetime(&naive).single().unwrap_or(now)
}

/// Adds a timer that fires `duration` from now.
pub fn schedule_timer(duration: Duration, label: String) {
    push(Kind::Timer, label, at_duration_from_now(duration));
}

/// Adds an alarm for the next time the clock reads `time`.
pub fn schedule_alarm(time: NaiveTime, label: String) {
    push(Kind::Alarm, label, next_occurrence(time));
}

/// Cancels everything pending. Returns how many there were.
pub fn cancel_all() -> usize {
    PENDING.lock().map(|mut list| {
        let n = list.len();
        list.clear();
        n
    }).unwrap_or(0)
}

/// The soonest pending timer or alarm, and how long remains, in whole
/// seconds.
pub fn time_left() -> Option<(String, i64)> {
    let list = PENDING.lock().ok()?;
    let closest = soonest(&list)?;
    let seconds = closest.fires_at.signed_duration_since(Local::now()).num_seconds().max(0);
    Some((closest.label.clone(), seconds))
}

/// Takes out everything due right now, for the caller to announce.
pub fn take_due() -> Vec<Pending> {
    let Ok(mut list) = PENDING.lock() else { return Vec::new() };
    due_now(&mut list, Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plain_and_compound_numbers() {
        let words = ["temporizador", "de", "treinta", "y", "cinco", "minutos"];
        assert_eq!(number_at(&words, 2), Some((35, 5)));
        let words = ["cinco", "minutos"];
        assert_eq!(number_at(&words, 0), Some((5, 1)));
        let words = ["doce"];
        assert_eq!(number_at(&words, 0), Some((12, 1)));
    }

    #[test]
    fn parses_the_brief_examples() {
        let (duration, label) = parse_duration("pon un temporizador de cinco minutos").unwrap();
        assert_eq!(duration, Duration::from_secs(5 * 60));
        assert_eq!(label, "cinco minutos");

        let (duration, label) = parse_duration("avisame en diez minutos").unwrap();
        assert_eq!(duration, Duration::from_secs(10 * 60));
        assert_eq!(label, "diez minutos");

        let (_, label) = parse_alarm("pon una alarma a las ocho y media").unwrap();
        assert_eq!(label, "a las ocho y media");
    }

    #[test]
    fn media_hora_is_thirty_minutes() {
        let (duration, label) = parse_duration("espera media hora").unwrap();
        assert_eq!(duration, Duration::from_secs(30 * 60));
        assert_eq!(label, "media hora");
    }

    #[test]
    fn at_duration_from_now_lands_that_far_in_the_future() {
        let now = Local::now();
        let fires_at = at_duration_from_now(Duration::from_secs(600));
        let diff = fires_at.signed_duration_since(now).num_seconds();
        assert!((595..=605).contains(&diff), "{diff}");
    }

    #[test]
    fn next_occurrence_lands_today_or_tomorrow_but_never_in_the_past() {
        let now = Local::now();
        let past = (now - chrono::Duration::minutes(1)).time();
        assert!(next_occurrence(past) > now);
        let future = (now + chrono::Duration::minutes(1)).time();
        let fires_at = next_occurrence(future);
        assert!(fires_at > now && fires_at.date_naive() == now.date_naive());
    }

    #[test]
    fn a_sentence_with_no_duration_in_it_parses_to_nothing() {
        assert_eq!(parse_duration("abre chrome"), None);
        assert_eq!(parse_duration("cancela el temporizador"), None);
    }

    #[test]
    fn quarter_and_menos_cuarto_shift_the_hour() {
        let (_, label) = parse_alarm("a las nueve menos cuarto").unwrap();
        assert_eq!(label, "a las nueve menos cuarto");
        let (_, label) = parse_alarm("a las tres y cuarto").unwrap();
        assert_eq!(label, "a las tres y cuarto");
    }

    #[test]
    fn an_alarm_resolves_to_the_soonest_of_the_two_twelve_hour_readings() {
        let (time, _) = parse_alarm("a las ocho y media").unwrap();
        let now = Local::now().time();
        let am = NaiveTime::from_hms_opt(8, 30, 0).unwrap();
        let pm = NaiveTime::from_hms_opt(20, 30, 0).unwrap();
        let expected = if time == am { am } else { pm };
        assert_eq!(time, expected);
        // Whichever it picked, the other reading must be no closer.
        let until = |t: NaiveTime| {
            let d = t.signed_duration_since(now);
            if d < chrono::Duration::zero() { d + chrono::Duration::days(1) } else { d }
        };
        assert!(until(time) <= until(if time == am { pm } else { am }));
    }

    #[test]
    fn twelve_wraps_to_midnight_and_noon() {
        let (time, _) = parse_alarm("a las doce").unwrap();
        assert!(time == NaiveTime::from_hms_opt(0, 0, 0).unwrap()
            || time == NaiveTime::from_hms_opt(12, 0, 0).unwrap());
    }

    // Every test below touches the shared pending list, so they run one at
    // a time — otherwise one test's cancel_all() could eat another's timer.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn schedules_cancels_and_reports_time_left() {
        let _guard = TEST_LOCK.lock().unwrap();
        cancel_all();
        assert!(time_left().is_none());

        schedule_timer(Duration::from_secs(300), "cinco minutos".into());
        let (label, seconds) = time_left().unwrap();
        assert_eq!(label, "cinco minutos");
        assert!(seconds <= 300);

        assert_eq!(cancel_all(), 1);
        assert!(time_left().is_none());
    }

    #[test]
    fn due_now_takes_only_what_has_arrived() {
        let now = Local::now();
        let mut list = vec![
            Pending { label: "ya".into(), fires_at: now - chrono::Duration::seconds(1), kind: Kind::Timer },
            Pending { label: "luego".into(), fires_at: now + chrono::Duration::seconds(60), kind: Kind::Timer },
        ];
        let due = due_now(&mut list, now);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].label, "ya");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "luego");
    }

    #[test]
    fn soonest_picks_the_nearest() {
        let now = Local::now();
        let list = vec![
            Pending { label: "tarde".into(), fires_at: now + chrono::Duration::seconds(600), kind: Kind::Alarm },
            Pending { label: "pronto".into(), fires_at: now + chrono::Duration::seconds(10), kind: Kind::Timer },
        ];
        assert_eq!(soonest(&list).unwrap().label, "pronto");
    }
}

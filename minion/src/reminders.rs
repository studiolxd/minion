//! Reminders and calendar: pure Spanish date parsing, then talking to
//! Recordatorios and Calendario via `osascript`.
//!
//! Nested inside [`crate::answers`] (see the `#[path]` mod there) rather
//! than declared in `main.rs`, which this task does not touch. Parsing
//! takes `now` as an argument so tests do not depend on the wall clock;
//! clock-time phrases ("a las cinco", "a las nueve y media") go through
//! [`crate::timers::parse_alarm`] rather than a second parser for the same
//! grammar — that one resolves am/pm against the real clock, same as it
//! does for alarms, so only the date words here are fully pure.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

fn find_word(words: &[String], target: &str) -> Option<usize> {
    words.iter().position(|w| w == target)
}

fn find_seq(words: &[String], seq: &[&str]) -> Option<usize> {
    if seq.is_empty() || words.len() < seq.len() {
        return None;
    }
    words.windows(seq.len()).position(|w| w.iter().map(String::as_str).eq(seq.iter().copied()))
}

fn remove_range(words: &mut Vec<String>, start: usize, len: usize) {
    words.drain(start..start + len);
}

fn weekday_named(word: &str) -> Option<Weekday> {
    Some(match word {
        "lunes" => Weekday::Mon,
        "martes" => Weekday::Tue,
        "miercoles" => Weekday::Wed,
        "jueves" => Weekday::Thu,
        "viernes" => Weekday::Fri,
        "sabado" => Weekday::Sat,
        "domingo" => Weekday::Sun,
        _ => return None,
    })
}

const WEEKDAY_NAMES: &[(Weekday, &str)] = &[
    (Weekday::Mon, "lunes"),
    (Weekday::Tue, "martes"),
    (Weekday::Wed, "miércoles"),
    (Weekday::Thu, "jueves"),
    (Weekday::Fri, "viernes"),
    (Weekday::Sat, "sábado"),
    (Weekday::Sun, "domingo"),
];

/// Accented, for speech — the words this parses are already stripped of
/// accents by [`crate::text::normalise`], but "miercoles" is worse to hear
/// back than to read.
fn spoken_weekday(day: Weekday) -> &'static str {
    WEEKDAY_NAMES.iter().find(|(w, _)| *w == day).map(|(_, name)| *name).unwrap_or("")
}

/// The next date at or after `today` that falls on `target` — `today`
/// itself, if that is already the day named.
fn next_weekday(today: NaiveDate, target: Weekday) -> NaiveDate {
    let from = today.weekday().num_days_from_monday() as i64;
    let to = target.num_days_from_monday() as i64;
    today + chrono::Duration::days((to - from).rem_euclid(7))
}

/// Pulls a relative or named date out of `words`, removing the words that
/// named it, and returns the date, a default time of day for phrases that
/// carry one ("esta tarde" means nothing without a time), and the phrase
/// to name it back in a spoken reply.
fn extract_date(
    words: &mut Vec<String>,
    today: NaiveDate,
) -> (Option<NaiveDate>, Option<NaiveTime>, Option<String>) {
    if let Some(i) = find_seq(words, &["pasado", "manana"]) {
        remove_range(words, i, 2);
        return (Some(today + chrono::Duration::days(2)), None, Some("pasado mañana".into()));
    }
    if let Some(i) = find_word(words, "manana") {
        remove_range(words, i, 1);
        return (Some(today + chrono::Duration::days(1)), None, Some("mañana".into()));
    }
    if let Some(i) = find_seq(words, &["esta", "tarde"]) {
        remove_range(words, i, 2);
        return (Some(today), NaiveTime::from_hms_opt(17, 0, 0), Some("esta tarde".into()));
    }
    if let Some(i) = find_seq(words, &["esta", "noche"]) {
        remove_range(words, i, 2);
        return (Some(today), NaiveTime::from_hms_opt(21, 0, 0), Some("esta noche".into()));
    }
    let mut weekday_at = None;
    for i in 0..words.len() {
        if words[i] == "el" {
            if let Some(day) = words.get(i + 1).and_then(|w| weekday_named(w)) {
                weekday_at = Some((i, day));
                break;
            }
        }
    }
    if let Some((i, day)) = weekday_at {
        remove_range(words, i, 2);
        let date = next_weekday(today, day);
        return (Some(date), None, Some(format!("el {}", spoken_weekday(day))));
    }
    (None, None, None)
}

/// Pulls a spoken clock time out of `words` via
/// [`crate::timers::parse_alarm`], removing the words that named it, along
/// with the phrase itself for the confirmation reply.
fn strip_time(words: &mut Vec<String>) -> Option<(NaiveTime, String)> {
    let joined = words.join(" ");
    let (time, label) = crate::timers::parse_alarm(&joined)?;
    let label_words: Vec<&str> = label.split_whitespace().collect();
    let pos = words
        .windows(label_words.len())
        .position(|w| w.iter().map(String::as_str).eq(label_words.iter().copied()))?;
    words.drain(pos..pos + label_words.len());
    Some((time, label))
}

/// "recuérdame comprar pan", "recuérdame llamar a Ana a las cinco",
/// "recuérdame X mañana a las diez", "recuérdame X el viernes" — the
/// reminder text, when it is due (`None` for a plain reminder with no due
/// time), and the phrase to name it back in the confirmation reply.
pub fn parse_reminder(
    rest: &str,
    now: DateTime<Local>,
) -> Option<(String, Option<NaiveDateTime>, Option<String>)> {
    let mut words: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
    let start = find_word(&words, "recuerdame").or_else(|| find_word(&words, "recuerda"))?;
    remove_range(&mut words, 0, start + 1);

    let today = now.date_naive();
    let (date, default_time, date_label) = extract_date(&mut words, today);
    let (explicit_time, time_label) = match strip_time(&mut words) {
        Some((t, l)) => (Some(t), Some(l)),
        None => (None, None),
    };

    let text = words.join(" ").trim().to_string();
    if text.is_empty() {
        return None;
    }

    let due = match (date, explicit_time.or(default_time)) {
        (Some(d), Some(t)) => Some(NaiveDateTime::new(d, t)),
        (Some(d), None) => Some(NaiveDateTime::new(d, NaiveTime::from_hms_opt(9, 0, 0)?)),
        (None, Some(t)) => Some(NaiveDateTime::new(today, t)),
        (None, None) => None,
    };

    let when = match (date_label, time_label) {
        (Some(d), Some(t)) => Some(format!("{d} {t}")),
        (Some(d), None) => Some(d),
        (None, Some(t)) => Some(t),
        (None, None) => None,
    };

    Some((text, due, when))
}

/// "añade evento X mañana a las diez", "crea un evento X el lunes a las
/// nueve y media" — the title, when it starts (9 in the morning today if
/// nothing was said, since an event needs a start unlike a reminder), and
/// the phrase for the confirmation reply.
pub fn parse_event(rest: &str, now: DateTime<Local>) -> Option<(String, NaiveDateTime, String)> {
    let mut words: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
    let evento = find_word(&words, "evento")?;
    remove_range(&mut words, 0, evento + 1);

    let today = now.date_naive();
    let (date, default_time, date_label) = extract_date(&mut words, today);
    let (explicit_time, time_label) = match strip_time(&mut words) {
        Some((t, l)) => (Some(t), Some(l)),
        None => (None, None),
    };

    let title = words.join(" ").trim().to_string();
    if title.is_empty() {
        return None;
    }

    let date = date.unwrap_or(today);
    let time = explicit_time.or(default_time).or_else(|| NaiveTime::from_hms_opt(9, 0, 0))?;
    let when = match (date_label, time_label) {
        (Some(d), Some(t)) => format!("{d} {t}"),
        (Some(d), None) => d,
        (None, Some(t)) => t,
        (None, None) => "hoy a las nueve".into(),
    };

    Some((title, NaiveDateTime::new(date, time), when))
}

/// Builds a date inside the script from its parts via `current date`
/// rather than a formatted string — a formatted date is read back through
/// whatever locale the Mac running it happens to be set to, and would
/// silently misparse on one where it is not the same as the one that
/// wrote it.
const DATE_HANDLER: &str = "on makeDate(y, m, d, h, mi, s)\n\
    \tset theDate to current date\n\
    \tset day of theDate to 1\n\
    \tset year of theDate to y\n\
    \tset month of theDate to m\n\
    \tset day of theDate to d\n\
    \tset hours of theDate to h\n\
    \tset minutes of theDate to mi\n\
    \tset seconds of theDate to s\n\
    \treturn theDate\n\
    end makeDate";

fn date_args(when: NaiveDateTime) -> String {
    format!(
        "{}, {}, {}, {}, {}, {}",
        when.year(),
        when.month(),
        when.day(),
        when.hour(),
        when.minute(),
        when.second()
    )
}

/// Runs an AppleScript through `osascript`, but never blocks the answer
/// thread past 5 seconds — Calendar and Reminders access can otherwise
/// hang behind a permission dialog Minion cannot see or dismiss. `Command`
/// has no `wait_timeout` in std, so this spawns and polls `try_wait`
/// instead.
fn run_applescript(script: &str) -> Result<String, String> {
    let mut child = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("no ha respondido a tiempo".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
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
    Err(if stderr.trim().is_empty() { "osascript failed".to_string() } else { stderr.trim().to_string() })
}

/// Creates a reminder in the default Recordatorios list.
pub fn create_reminder(text: &str, due: Option<NaiveDateTime>) -> Result<(), String> {
    let name = crate::actions::applescript_string(text);
    let script = match due {
        Some(due) => format!(
            "{DATE_HANDLER}\n\
             tell application \"Reminders\"\n\
             \tmake new reminder with properties {{name:\"{name}\", due date:my makeDate({})}}\n\
             end tell",
            date_args(due)
        ),
        None => format!(
            "tell application \"Reminders\"\n\tmake new reminder with properties {{name:\"{name}\"}}\nend tell"
        ),
    };
    run_applescript(&script).map(|_| ())
}

/// Creates a one-hour event (the brief's stated default) in the first
/// calendar — Calendar.app has no scriptable "default calendar" the way
/// Reminders has a default list, so this is the least surprising stand-in.
pub fn create_event(title: &str, start: NaiveDateTime) -> Result<(), String> {
    let end = start + chrono::Duration::hours(1);
    let name = crate::actions::applescript_string(title);
    let script = format!(
        "{DATE_HANDLER}\n\
         tell application \"Calendar\"\n\
         \ttell calendar 1\n\
         \t\tmake new event with properties {{summary:\"{name}\", start date:my makeDate({}), end date:my makeDate({})}}\n\
         \tend tell\n\
         end tell",
        date_args(start),
        date_args(end)
    );
    run_applescript(&script).map(|_| ())
}

/// Field separator for the events script's output — a control character
/// rather than a comma, since a summary is free text and may contain one.
const FIELD_SEP: char = '\u{1}';

fn events_script(start: NaiveDateTime, end: NaiveDateTime) -> String {
    format!(
        "{DATE_HANDLER}\n\
         set startDate to my makeDate({})\n\
         set endDate to my makeDate({})\n\
         set output to \"\"\n\
         tell application \"Calendar\"\n\
         \trepeat with cal in calendars\n\
         \t\trepeat with e in (events of cal whose start date ≥ startDate and start date < endDate)\n\
         \t\t\tset sd to start date of e\n\
         \t\t\tset output to output & (year of sd) & \"{sep}\" & ((month of sd) as integer) & \"{sep}\" & (day of sd) & \"{sep}\" & (hours of sd) & \"{sep}\" & (minutes of sd) & \"{sep}\" & (summary of e) & linefeed\n\
         \t\tend repeat\n\
         \tend repeat\n\
         end tell\n\
         return output",
        date_args(start),
        date_args(end),
        sep = FIELD_SEP,
    )
}

/// Turns the events script's `year<SEP>month<SEP>day<SEP>hour<SEP>minute<SEP>summary`
/// lines into sorted, typed events. Pure, so it can be tested without a
/// real Calendar behind it.
fn parse_events_output(output: &str) -> Vec<(NaiveDateTime, String)> {
    let mut events: Vec<(NaiveDateTime, String)> = output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(6, FIELD_SEP);
            let y: i32 = parts.next()?.parse().ok()?;
            let m: u32 = parts.next()?.parse().ok()?;
            let d: u32 = parts.next()?.parse().ok()?;
            let h: u32 = parts.next()?.parse().ok()?;
            let mi: u32 = parts.next()?.parse().ok()?;
            let title = parts.next()?.trim().to_string();
            let date = NaiveDate::from_ymd_opt(y, m, d)?;
            let time = NaiveTime::from_hms_opt(h, mi, 0)?;
            Some((NaiveDateTime::new(date, time), title))
        })
        .collect();
    events.sort_by_key(|(when, _)| *when);
    events
}

fn events_between(start: NaiveDateTime, end: NaiveDateTime) -> Result<Vec<(NaiveDateTime, String)>, String> {
    let output = run_applescript(&events_script(start, end))?;
    Ok(parse_events_output(&output).into_iter().take(5).collect())
}

/// The first five events of today, sorted by start time.
pub fn events_today(now: DateTime<Local>) -> Result<Vec<(NaiveDateTime, String)>, String> {
    let start = now.date_naive().and_hms_opt(0, 0, 0).expect("midnight is always valid");
    let end = start + chrono::Duration::days(1);
    events_between(start, end)
}

/// The first five events of tomorrow, sorted by start time.
pub fn events_tomorrow(now: DateTime<Local>) -> Result<Vec<(NaiveDateTime, String)>, String> {
    let start = now.date_naive().and_hms_opt(0, 0, 0).expect("midnight is always valid")
        + chrono::Duration::days(1);
    let end = start + chrono::Duration::days(1);
    events_between(start, end)
}

/// The soonest event from now, searching two weeks ahead rather than only
/// today — "¿cuál es mi próxima reunión?" should still find one next
/// Tuesday.
pub fn next_meeting(now: DateTime<Local>) -> Result<Option<(NaiveDateTime, String)>, String> {
    let start = now.naive_local();
    let end = start + chrono::Duration::days(14);
    let output = run_applescript(&events_script(start, end))?;
    Ok(parse_events_output(&output).into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(&NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, 0).unwrap())
            .unwrap()
    }

    use chrono::TimeZone;

    #[test]
    fn a_plain_reminder_has_no_due_time() {
        let now = at(2026, 9, 3, 12, 0);
        let (text, due, when) = parse_reminder("recuerdame comprar pan", now).unwrap();
        assert_eq!(text, "comprar pan");
        assert_eq!(due, None);
        assert_eq!(when, None);
    }

    #[test]
    fn a_time_without_a_date_is_due_today() {
        let now = at(2026, 9, 3, 12, 0);
        let (text, due, when) = parse_reminder("recuerdame llamar a ana a las cinco", now).unwrap();
        assert_eq!(text, "llamar a ana");
        let due = due.unwrap();
        assert_eq!(due.date(), now.date_naive());
        assert!(
            due.time() == NaiveTime::from_hms_opt(5, 0, 0).unwrap()
                || due.time() == NaiveTime::from_hms_opt(17, 0, 0).unwrap()
        );
        assert_eq!(when, Some("a las cinco".to_string()));
    }

    #[test]
    fn tomorrow_with_a_time_carries_both() {
        let now = at(2026, 9, 3, 12, 0);
        let (text, due, when) =
            parse_reminder("recuerdame comprar pan manana a las diez", now).unwrap();
        assert_eq!(text, "comprar pan");
        assert_eq!(due.unwrap().date(), now.date_naive() + chrono::Duration::days(1));
        assert_eq!(when, Some("mañana a las diez".to_string()));
    }

    #[test]
    fn pasado_manana_is_two_days_out() {
        let now = at(2026, 9, 3, 12, 0);
        let (_, due, when) = parse_reminder("recuerdame pagar el alquiler pasado manana", now).unwrap();
        assert_eq!(due.unwrap().date(), now.date_naive() + chrono::Duration::days(2));
        assert_eq!(when, Some("pasado mañana".to_string()));
    }

    #[test]
    fn a_weekday_with_no_time_defaults_to_nine_in_the_morning() {
        // 2026-09-03 is a Thursday.
        let now = at(2026, 9, 3, 12, 0);
        let (text, due, when) = parse_reminder("recuerdame llamar a ana el viernes", now).unwrap();
        assert_eq!(text, "llamar a ana");
        let due = due.unwrap();
        assert_eq!(due.date(), NaiveDate::from_ymd_opt(2026, 9, 4).unwrap());
        assert_eq!(due.time(), NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(when, Some("el viernes".to_string()));
    }

    #[test]
    fn a_weekday_that_is_today_stays_today() {
        // 2026-09-03 is itself a Thursday.
        let now = at(2026, 9, 3, 12, 0);
        let (_, due, _) = parse_reminder("recuerdame regar las plantas el jueves", now).unwrap();
        assert_eq!(due.unwrap().date(), now.date_naive());
    }

    #[test]
    fn esta_tarde_and_esta_noche_have_their_own_times() {
        let now = at(2026, 9, 3, 8, 0);
        let (_, due, when) = parse_reminder("recuerdame regar las plantas esta tarde", now).unwrap();
        assert_eq!(due.unwrap().time(), NaiveTime::from_hms_opt(17, 0, 0).unwrap());
        assert_eq!(when, Some("esta tarde".to_string()));

        let (_, due, when) = parse_reminder("recuerdame sacar la basura esta noche", now).unwrap();
        assert_eq!(due.unwrap().time(), NaiveTime::from_hms_opt(21, 0, 0).unwrap());
        assert_eq!(when, Some("esta noche".to_string()));
    }

    #[test]
    fn without_the_wake_phrase_there_is_no_reminder() {
        assert_eq!(parse_reminder("abre chrome", at(2026, 9, 3, 12, 0)), None);
    }

    #[test]
    fn recuerda_without_me_is_also_recognised() {
        let now = at(2026, 9, 3, 12, 0);
        let (text, _, _) = parse_reminder("recuerda sacar la basura", now).unwrap();
        assert_eq!(text, "sacar la basura");
    }

    #[test]
    fn an_event_gets_its_title_and_start() {
        let now = at(2026, 9, 3, 8, 0);
        let (title, start, when) =
            parse_event("anade evento cumpleanos de ana manana a las diez", now).unwrap();
        assert_eq!(title, "cumpleanos de ana");
        assert_eq!(start.date(), now.date_naive() + chrono::Duration::days(1));
        assert_eq!(when, "mañana a las diez");
    }

    #[test]
    fn crea_un_evento_reads_the_same_shape() {
        // 2026-09-03 is a Thursday, so "el lunes" is 2026-09-07.
        let now = at(2026, 9, 3, 8, 0);
        let (title, start, when) =
            parse_event("crea un evento reunion el lunes a las nueve y media", now).unwrap();
        assert_eq!(title, "reunion");
        assert_eq!(start.date(), NaiveDate::from_ymd_opt(2026, 9, 7).unwrap());
        assert_eq!(when, "el lunes a las nueve y media");
    }

    #[test]
    fn an_event_with_nothing_said_defaults_to_today_at_nine() {
        let now = at(2026, 9, 3, 8, 0);
        let (title, start, when) = parse_event("evento revision anual", now).unwrap();
        assert_eq!(title, "revision anual");
        assert_eq!(start, NaiveDateTime::new(now.date_naive(), NaiveTime::from_hms_opt(9, 0, 0).unwrap()));
        assert_eq!(when, "hoy a las nueve");
    }

    #[test]
    fn without_the_word_evento_there_is_no_event() {
        assert_eq!(parse_event("abre chrome", at(2026, 9, 3, 12, 0)), None);
    }

    #[test]
    fn next_weekday_keeps_today_when_it_already_matches() {
        let thursday = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        assert_eq!(next_weekday(thursday, Weekday::Thu), thursday);
        assert_eq!(next_weekday(thursday, Weekday::Fri), NaiveDate::from_ymd_opt(2026, 9, 4).unwrap());
        assert_eq!(next_weekday(thursday, Weekday::Wed), NaiveDate::from_ymd_opt(2026, 9, 9).unwrap());
    }

    #[test]
    fn events_output_parses_and_sorts_by_start_time() {
        let output = format!(
            "2026{sep}9{sep}3{sep}16{sep}0{sep}dentista\n2026{sep}9{sep}3{sep}10{sep}0{sep}reunion con ana",
            sep = FIELD_SEP
        );
        let events = parse_events_output(&output);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].1, "reunion con ana");
        assert_eq!(events[1].1, "dentista");
    }

    #[test]
    fn a_blank_or_malformed_line_is_skipped_not_a_panic() {
        let events = parse_events_output("not a valid line\n\n");
        assert!(events.is_empty());
    }
}

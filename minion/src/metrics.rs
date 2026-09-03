//! Recognition metrics, computed from the journal.
//!
//! Everything here is pure and works over the text of the log; `report_text`
//! is the only thin, filesystem-touching wrapper. This does not share code
//! with `learn.rs`'s `failed_phrases_in` — both read one line format
//! (`unknown  «…»`), but `learn.rs` counts phrases and this counts a dozen
//! other line shapes besides, so a shared parser would mostly be plumbing
//! `learn.rs` has no use for.

use std::collections::{BTreeMap, HashMap};
use std::fs;

/// One event a journal line can record, with just enough parsed out to
/// count it. Borrows from the line, so parsing a whole log costs no
/// allocation beyond the counters it feeds.
#[derive(Debug, Clone, PartialEq)]
enum Event<'a> {
    Ran { description: &'a str, ms: u64 },
    Unknown { phrase: &'a str },
    Blocked,
    OtherVoice { likeness: f32 },
    NotAddressed,
    Blank,
    VoiceMatched { likeness: f32 },
    Window,
    Typed,
    Undid,
    Asking,
    Taught,
    Declined,
}

/// Parses one journal line into its date and what it recorded, or `None`
/// for anything that is not one of the shapes this report counts —
/// startup notices, errors, menu clicks, and the rest of what `note!`
/// writes down.
///
/// The format is `journal.rs`'s: `"YYYY-MM-DD HH:MM:SS  {line}"`, and each
/// `{line}` starts with a nine-character label the call sites in `main.rs`
/// pad to line up in the raw log — `"ran      "`, `"unknown  "`, and so on.
fn parse_line(line: &str) -> Option<(&str, Event<'_>)> {
    let date = line.get(0..10)?;
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    // "YYYY-MM-DD HH:MM:SS  " is 21 characters before the label starts.
    let rest = line.get(21..)?;

    let event = if let Some(after) = rest.strip_prefix("ran      «") {
        let (_, tail) = after.split_once("»  ->  ")?;
        let (description, stats) = tail.split_once("  [")?;
        let stats = stats.strip_suffix(']')?;
        let ms: u64 = stats.rsplit(" · ").next()?.strip_suffix(" ms")?.trim().parse().ok()?;
        // Strips the " ×N" a repeated command gets, so "abrir Chrome" and
        // "abrir Chrome ×3" group as the same command.
        let description = match description.rsplit_once(" ×") {
            Some((base, count)) if !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()) => {
                base
            }
            _ => description,
        };
        Event::Ran { description, ms }
    } else if let Some(after) = rest.strip_prefix("unknown  «") {
        let phrase = after.split('»').next()?.trim();
        if phrase.is_empty() {
            return None;
        }
        Event::Unknown { phrase }
    } else if rest.starts_with("BLOCKED  «") {
        Event::Blocked
    } else if rest.starts_with("heard    ") {
        if let Some(paren) = rest.split("in another voice (").nth(1) {
            let likeness: f32 = paren.strip_suffix(')')?.trim().parse().ok()?;
            Event::OtherVoice { likeness }
        } else if rest.contains("not addressed to me") {
            Event::NotAddressed
        } else {
            return None;
        }
    } else if rest.starts_with("blank    ") {
        Event::Blank
    } else if let Some(after) = rest
        .strip_prefix("voice    ")
        .and_then(|line| line.split_once("matched at "))
        .map(|(_, score)| score)
    {
        let likeness: f32 = after.trim().parse().ok()?;
        Event::VoiceMatched { likeness }
    } else if rest.starts_with("window   «") {
        Event::Window
    } else if rest.starts_with("typed    «") {
        Event::Typed
    } else if rest.starts_with("undid    ") {
        Event::Undid
    } else if rest.starts_with("asking   «") {
        Event::Asking
    } else if rest.starts_with("taught   «") {
        Event::Taught
    } else if rest.starts_with("declined «") {
        Event::Declined
    } else {
        return None;
    };
    Some((date, event))
}

/// Utterance outcomes for one day, or the whole period.
#[derive(Default, Clone)]
pub struct Counts {
    pub heard: usize,
    /// The wake word was recognised — includes what came after it: run,
    /// unknown or blocked.
    pub addressed: usize,
    pub ran: usize,
    pub unknown: usize,
    pub blocked: usize,
    pub other_voice: usize,
}

impl Counts {
    fn add(&mut self, event: &Event) {
        match event {
            Event::Ran { .. } => {
                self.heard += 1;
                self.addressed += 1;
                self.ran += 1;
            }
            Event::Unknown { .. } => {
                self.heard += 1;
                self.addressed += 1;
                self.unknown += 1;
            }
            Event::Blocked => {
                self.heard += 1;
                self.addressed += 1;
                self.blocked += 1;
            }
            Event::OtherVoice { .. } => {
                self.heard += 1;
                self.other_voice += 1;
            }
            Event::NotAddressed | Event::Blank => {
                self.heard += 1;
            }
            _ => {}
        }
    }

    fn merge(&mut self, other: &Counts) {
        self.heard += other.heard;
        self.addressed += other.addressed;
        self.ran += other.ran;
        self.unknown += other.unknown;
        self.blocked += other.blocked;
        self.other_voice += other.other_voice;
    }
}

/// Voice scores for one day: how the owner's speech scored against the
/// profile, and the closest any other voice came.
#[derive(Default, Clone)]
pub struct VoiceDay {
    pub owner_min: Option<f32>,
    pub owner_mean: Option<f32>,
    pub owner_median: Option<f32>,
    pub other_max: Option<f32>,
}

impl VoiceDay {
    fn from_scores(owner: &[f32], other: &[f32]) -> Self {
        VoiceDay {
            owner_min: owner.iter().copied().fold(None, |acc, x| Some(acc.map_or(x, |a: f32| a.min(x)))),
            owner_mean: (!owner.is_empty()).then(|| owner.iter().sum::<f32>() / owner.len() as f32),
            owner_median: median(owner),
            other_max: other.iter().copied().fold(None, |acc, x| Some(acc.map_or(x, |a: f32| a.max(x)))),
        }
    }
}

/// The median of a slice of scores, sorted for the purpose. `None` for an
/// empty slice.
fn median(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mid = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) { (sorted[mid - 1] + sorted[mid]) / 2.0 } else { sorted[mid] })
}

/// Mean and 95th percentile (nearest-rank) of a slice of milliseconds.
/// `None` for an empty slice.
fn timing_summary(values: &[u64]) -> Option<(u64, u64)> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mean = sorted.iter().sum::<u64>() / sorted.len() as u64;
    let rank = ((sorted.len() as f64) * 0.95).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    Some((mean, sorted[index]))
}

/// Everything the log has to say about how well Minion is hearing its
/// owner, over whatever period was asked for.
pub struct Report {
    /// One day at a time, oldest first.
    pub days: Vec<(String, Counts)>,
    pub total: Counts,
    pub voice_by_day: Vec<(String, VoiceDay)>,
    /// Most-run command first.
    pub command_runs: Vec<(String, usize)>,
    /// Most-repeated failure first, at most ten.
    pub top_unknown: Vec<(String, usize)>,
    pub typed: usize,
    pub undid: usize,
    pub window_uses: usize,
    pub asking: usize,
    pub taught: usize,
    pub declined: usize,
    pub timing_ms: Vec<u64>,
    pub voice_threshold: f32,
}

/// Reads the log's text and works out what it has to say, from `since`
/// (inclusive, `"YYYY-MM-DD"`) onward — or the whole of it, with `None`.
///
/// Pure: nothing here touches a clock or a file, so it is exercised on
/// fixture text rather than the real log.
pub fn analyse(contents: &str, since: Option<&str>, voice_threshold: f32) -> Report {
    let mut days: BTreeMap<String, Counts> = BTreeMap::new();
    let mut voice_days: BTreeMap<String, (Vec<f32>, Vec<f32>)> = BTreeMap::new();
    let mut command_runs: HashMap<String, usize> = HashMap::new();
    let mut unknown_counts: HashMap<String, usize> = HashMap::new();
    let mut typed = 0usize;
    let mut undid = 0usize;
    let mut window_uses = 0usize;
    let mut asking = 0usize;
    let mut taught = 0usize;
    let mut declined = 0usize;
    let mut timing_ms = Vec::new();

    for line in contents.lines() {
        let Some((date, event)) = parse_line(line) else { continue };
        if since.is_some_and(|cutoff| date < cutoff) {
            continue;
        }

        days.entry(date.to_string()).or_default().add(&event);
        match event {
            Event::Ran { description, ms } => {
                *command_runs.entry(description.to_string()).or_default() += 1;
                timing_ms.push(ms);
            }
            Event::Unknown { phrase } => {
                *unknown_counts.entry(phrase.to_string()).or_default() += 1;
            }
            Event::VoiceMatched { likeness } => {
                voice_days.entry(date.to_string()).or_default().0.push(likeness);
            }
            Event::OtherVoice { likeness } => {
                voice_days.entry(date.to_string()).or_default().1.push(likeness);
            }
            Event::Window => window_uses += 1,
            Event::Typed => typed += 1,
            Event::Undid => undid += 1,
            Event::Asking => asking += 1,
            Event::Taught => taught += 1,
            Event::Declined => declined += 1,
            Event::Blocked | Event::NotAddressed | Event::Blank => {}
        }
    }

    let mut total = Counts::default();
    for c in days.values() {
        total.merge(c);
    }

    let mut command_runs: Vec<(String, usize)> = command_runs.into_iter().collect();
    command_runs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut top_unknown: Vec<(String, usize)> = unknown_counts.into_iter().collect();
    top_unknown.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top_unknown.truncate(10);

    let voice_by_day = voice_days
        .into_iter()
        .map(|(date, (owner, other))| (date, VoiceDay::from_scores(&owner, &other)))
        .collect();

    Report {
        days: days.into_iter().collect(),
        total,
        voice_by_day,
        command_runs,
        top_unknown,
        typed,
        undid,
        window_uses,
        asking,
        taught,
        declined,
        timing_ms,
        voice_threshold,
    }
}

fn pct(n: usize, total: usize) -> String {
    if total == 0 {
        "—".to_string()
    } else {
        format!("{:.0}%", n as f64 / total as f64 * 100.0)
    }
}

fn counts_row(label: &str, c: &Counts) -> String {
    format!(
        "{:<12}{:>7}{:>11}{:>9}{:>9}{:>7}{:>10}\n",
        label, c.heard, c.addressed, c.ran, c.unknown, c.blocked, c.other_voice
    )
}

fn voice_row(date: &str, v: &VoiceDay, threshold: f32) -> String {
    let show = |x: Option<f32>| x.map_or_else(|| "—".to_string(), |x| format!("{x:.2}"));
    let margin =
        v.owner_min.map_or_else(|| "—".to_string(), |min| format!("{:+.2}", min - threshold));
    format!(
        "{:<12}{:>8}{:>8}{:>9}{:>10}{:>9}\n",
        date,
        show(v.owner_min),
        show(v.owner_mean),
        show(v.owner_median),
        show(v.other_max),
        margin
    )
}

/// A one-line piece of advice, picked from whichever of the numbers is
/// most worth acting on. Order matters: a failing phrase is the cheapest
/// to fix, a blocked command points at a permission, and a tight voice
/// margin is the one that silently gets worse.
fn advice(report: &Report) -> String {
    if let Some((phrase, n)) = report.top_unknown.first() {
        let repeats = if *n > 1 { format!(" (×{n})") } else { String::new() };
        return format!(
            "La frase que más falla es «{phrase}»{repeats}: díctala con \
             `minion learn` o deja que Minion pregunte."
        );
    }
    if report.total.blocked > 0 {
        return format!(
            "{} orden(es) bloqueadas por macOS: revisa el permiso de Accesibilidad \
             en Ajustes del Sistema.",
            report.total.blocked
        );
    }
    let tightest_margin = report
        .voice_by_day
        .iter()
        .filter_map(|(_, v)| v.owner_min)
        .map(|min| min - report.voice_threshold)
        .fold(None, |acc: Option<f32>, m| Some(acc.map_or(m, |a| a.min(m))));
    if let Some(margin) = tightest_margin {
        if margin < 0.1 {
            return format!(
                "El margen de voz más ajustado es de {margin:+.2} sobre el umbral: \
                 si empieza a fallar, vuelve a grabar tu voz con `minion enroll`."
            );
        }
    }
    if report.total.heard == 0 {
        return "Sin datos en el periodo elegido.".to_string();
    }
    "Todo va bien: sin frases pendientes de aprender ni órdenes bloqueadas.".to_string()
}

/// Renders a report as plain, monospaced text in Spanish — the body for
/// both `minion stats` and the «Estadísticas…» window.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("Estadísticas de reconocimiento\n\n");

    out.push_str("Por día — cuántas frases se oyeron, cuántas iban dirigidas a Minion, y qué pasó con ellas:\n\n");
    out.push_str(&format!(
        "{:<12}{:>7}{:>11}{:>9}{:>9}{:>7}{:>10}\n",
        "Fecha", "Oídas", "Dirigidas", "Ejecut.", "No ent.", "Bloq.", "Otra voz"
    ));
    if report.days.is_empty() {
        out.push_str("  (sin datos en el periodo elegido)\n");
    }
    for (date, c) in &report.days {
        out.push_str(&counts_row(date, c));
    }
    if report.days.len() > 1 {
        out.push_str(&counts_row("TOTAL", &report.total));
    }
    out.push('\n');

    let t = &report.total;
    out.push_str(&format!(
        "Tasas sobre lo oído: dirigidas {}, ejecutadas {}, no entendidas {}, \
         bloqueadas {}, otra voz {}.\n\n",
        pct(t.addressed, t.heard),
        pct(t.ran, t.heard),
        pct(t.unknown, t.heard),
        pct(t.blocked, t.heard),
        pct(t.other_voice, t.heard)
    ));

    if !report.command_runs.is_empty() {
        out.push_str("Órdenes más usadas:\n\n");
        for (command, n) in report.command_runs.iter().take(10) {
            out.push_str(&format!("  {command:<48}×{n}\n"));
        }
        out.push('\n');
    }

    if !report.top_unknown.is_empty() {
        out.push_str("Frases no entendidas, más frecuentes (máx. 10):\n\n");
        for (phrase, n) in &report.top_unknown {
            let repeats = if *n > 1 { format!("  ×{n}") } else { String::new() };
            out.push_str(&format!("  «{phrase}»{repeats}\n"));
        }
        out.push('\n');
    }

    if !report.voice_by_day.is_empty() {
        out.push_str(&format!("Voz por día — umbral configurado: {:.2}\n\n", report.voice_threshold));
        out.push_str(&format!(
            "{:<12}{:>8}{:>8}{:>9}{:>10}{:>9}\n",
            "Fecha", "Mín.", "Media", "Mediana", "Máx.otra", "Margen"
        ));
        for (date, v) in &report.voice_by_day {
            out.push_str(&voice_row(date, v, report.voice_threshold));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "Dictado: {} fragmento(s) escrito(s), {} deshacer.\n",
        report.typed, report.undid
    ));
    if let Some((mean, p95)) = timing_summary(&report.timing_ms) {
        out.push_str(&format!("Reconocimiento: media {mean} ms, percentil 95 {p95} ms.\n"));
    }
    out.push_str(&format!(
        "Ventana de conversación: {} vez/veces sin repetir la palabra clave.\n",
        report.window_uses
    ));
    out.push_str(&format!(
        "Preguntas de aprendizaje: {} preguntadas, {} enseñadas, {} rechazadas.\n\n",
        report.asking, report.taught, report.declined
    ));

    out.push_str(&format!("Consejo: {}\n", advice(report)));
    out
}

/// The log's current file and, if it exists, the previous generation
/// `journal.rs` rotates to — oldest first, so a day near the boundary is
/// not split in the wrong order.
fn read_log() -> String {
    let Some(path) = crate::journal::path() else { return String::new() };
    let mut contents = fs::read_to_string(path.with_extension("log.1")).unwrap_or_default();
    contents.push_str(&fs::read_to_string(&path).unwrap_or_default());
    contents
}

/// `--days N` means the last `N` days including today, local time.
fn cutoff_date(days: u32) -> String {
    let days_back = days.saturating_sub(1) as i64;
    (chrono::Local::now().date_naive() - chrono::Duration::days(days_back))
        .format("%Y-%m-%d")
        .to_string()
}

/// The report over the real log, ready to print or show in a window. The
/// one function here that touches the filesystem or the clock — everything
/// it calls is pure and tested on fixture text instead.
pub fn report_text(days: Option<u32>) -> String {
    let contents = read_log();
    let threshold = crate::config::load().voice_threshold();
    let cutoff = days.and_then(|n| if n == 0 { None } else { Some(cutoff_date(n)) });
    render(&analyse(&contents, cutoff.as_deref(), threshold))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
2026-09-01 12:00:01  ran      «Minion Chrome.»  ->  abrir Chrome  [100% · 1.6s audio · 142 ms]
2026-09-01 12:00:04  unknown  «Minion haz un pino.»  ->  not understood
2026-09-01 12:00:09  unknown  «Minion haz un pino.»  ->  not understood
2026-09-01 12:00:12  BLOCKED  «Minion cierra la ventana.»  ->  cerrar ventana: not permitted  — macOS refused it. Grant Accessibility in System Settings.
2026-09-01 12:00:15  heard    1.5s of speech, not addressed to me
2026-09-01 12:00:18  heard    4.8s in another voice (-0.01)
2026-09-01 12:00:20  voice    matched at 0.58
2026-09-01 12:00:21  blank    1.8s of audio, nothing recognised
2026-09-01 12:00:22  window   «cierra Safari»  (no wake word, 2.1 s after the last command)
2026-09-01 12:00:23  typed    «hola mundo»
2026-09-01 12:00:24  undid    typing (5 characters)
2026-09-01 12:00:25  asking   «abre chorme»  ->  abrir Chrome?
2026-09-01 12:00:26  taught   «abre chorme»  ->  abrir Chrome
2026-09-01 12:00:27  declined «cierra safiri»
2026-09-02 09:00:00  ran      «Minion Chrome.»  ->  abrir Chrome ×3  [92% · 1.2s audio · 200 ms]
2026-09-02 09:00:05  voice    matched at 0.44
2026-09-02 09:00:06  voice    2.0s too short to check — let through
2026-09-02 09:00:07  Model loaded. 700 MB resident.
";

    #[test]
    fn parses_every_shape_of_line() {
        let events: Vec<_> = LOG.lines().filter_map(parse_line).collect();
        // 16 countable lines: the "Model loaded" line and the "too short"
        // voice line are not any of the shapes this report counts.
        assert_eq!(events.len(), 16);
    }

    #[test]
    fn counts_utterances_by_outcome_and_day() {
        let report = analyse(LOG, None, 0.32);
        assert_eq!(report.days.len(), 2);
        let (date, day1) = &report.days[0];
        assert_eq!(date, "2026-09-01");
        // ran, unknown ×2, blocked, not-addressed, other-voice, blank.
        assert_eq!(day1.heard, 7);
        assert_eq!(day1.addressed, 4); // ran + 2 unknown + blocked
        assert_eq!(day1.ran, 1);
        assert_eq!(day1.unknown, 2);
        assert_eq!(day1.blocked, 1);
        assert_eq!(day1.other_voice, 1);

        assert_eq!(report.total.heard, 8);
        assert_eq!(report.total.ran, 2);
    }

    #[test]
    fn a_repeated_command_groups_with_the_plain_one() {
        let report = analyse(LOG, None, 0.32);
        let (command, n) = report.command_runs.iter().find(|(c, _)| c == "abrir Chrome").unwrap();
        assert_eq!(command, "abrir Chrome");
        assert_eq!(*n, 2);
    }

    #[test]
    fn unknown_phrases_are_grouped_and_counted() {
        let report = analyse(LOG, None, 0.32);
        assert_eq!(report.top_unknown, vec![("Minion haz un pino.".to_string(), 2)]);
    }

    #[test]
    fn voice_scores_are_summarised_per_day() {
        let report = analyse(LOG, None, 0.32);
        let (date, v1) = &report.voice_by_day[0];
        assert_eq!(date, "2026-09-01");
        assert_eq!(v1.owner_min, Some(0.58));
        assert_eq!(v1.owner_mean, Some(0.58));
        assert_eq!(v1.other_max, Some(-0.01));
        // "too short to check" is not a voice score.
        let (_, v2) = &report.voice_by_day[1];
        assert_eq!(v2.owner_min, Some(0.44));
        assert_eq!(v2.other_max, None);
    }

    #[test]
    fn dictation_timing_and_learning_are_counted() {
        let report = analyse(LOG, None, 0.32);
        assert_eq!(report.typed, 1);
        assert_eq!(report.undid, 1);
        assert_eq!(report.window_uses, 1);
        assert_eq!(report.asking, 1);
        assert_eq!(report.taught, 1);
        assert_eq!(report.declined, 1);
        assert_eq!(report.timing_ms, vec![142, 200]);
    }

    #[test]
    fn since_excludes_earlier_days() {
        let report = analyse(LOG, Some("2026-09-02"), 0.32);
        assert_eq!(report.days.len(), 1);
        assert_eq!(report.days[0].0, "2026-09-02");
    }

    #[test]
    fn timing_mean_and_p95_are_computed() {
        assert_eq!(timing_summary(&[]), None);
        assert_eq!(timing_summary(&[100]), Some((100, 100)));
        assert_eq!(timing_summary(&[100, 200, 300, 400, 500]), Some((300, 500)));
    }

    #[test]
    fn median_handles_even_and_odd_counts() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[0.5]), Some(0.5));
        assert_eq!(median(&[0.2, 0.6, 0.4]), Some(0.4));
        assert_eq!(median(&[0.2, 0.4, 0.6, 0.8]), Some(0.5));
    }

    #[test]
    fn advice_points_at_the_worst_failing_phrase_first() {
        let report = analyse(LOG, None, 0.32);
        assert!(advice(&report).contains("Minion haz un pino"));
    }

    #[test]
    fn advice_falls_back_to_blocked_commands_then_a_tight_voice_margin() {
        let no_unknown = "\
2026-09-01 12:00:01  BLOCKED  «Minion cierra la ventana.»  ->  cerrar ventana: not permitted\n";
        assert!(advice(&analyse(no_unknown, None, 0.32)).contains("bloqueadas"));

        let tight_margin = "2026-09-01 12:00:01  voice    matched at 0.35\n";
        assert!(advice(&analyse(tight_margin, None, 0.32)).contains("margen de voz"));

        let comfortable = "\
2026-09-01 12:00:00  ran      «Minion Chrome.»  ->  abrir Chrome  [100% · 1.0s audio · 100 ms]\n\
2026-09-01 12:00:01  voice    matched at 0.90\n";
        assert!(advice(&analyse(comfortable, None, 0.32)).contains("Todo va bien"));
    }

    #[test]
    fn a_voice_match_reads_with_or_without_a_name() {
        // Profiles have names now — "voice    Ana matched at 0.61" — but a
        // log written before they did is still the same file.
        let named = "2026-09-01 12:00:01  voice    Ana matched at 0.35\n";
        assert!(advice(&analyse(named, None, 0.32)).contains("margen de voz"));
        let unnamed = "2026-09-01 12:00:01  voice    matched at 0.35\n";
        assert!(advice(&analyse(unnamed, None, 0.32)).contains("margen de voz"));
    }

    #[test]
    fn renders_a_readable_report() {
        let report = analyse(LOG, None, 0.32);
        let text = render(&report);
        assert!(text.contains("2026-09-01"));
        assert!(text.contains("abrir Chrome"));
        assert!(text.contains("Consejo:"));
    }

    #[test]
    fn cutoff_date_counts_back_including_today() {
        let today = chrono::Local::now().date_naive();
        assert_eq!(cutoff_date(1), today.format("%Y-%m-%d").to_string());
        assert_eq!(
            cutoff_date(7),
            (today - chrono::Duration::days(6)).format("%Y-%m-%d").to_string()
        );
    }
}

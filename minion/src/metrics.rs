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
    pub drift: Drift,
}

/// Reads the log's text and works out what it has to say, from `since`
/// (inclusive, `"YYYY-MM-DD"`) onward — or the whole of it, with `None`.
/// `today` (`"YYYY-MM-DD"`) is [`voice_drift`]'s rolling window, computed
/// over the *whole* log regardless of `since` — a report over the last 30
/// days should not lose the baseline that sits before it.
///
/// Pure: nothing here touches a clock or a file, so it is exercised on
/// fixture text rather than the real log.
pub fn analyse(contents: &str, since: Option<&str>, voice_threshold: f32, today: &str) -> Report {
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
        drift: voice_drift(contents, today),
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

/// How much the rolling median has to fall below the baseline before
/// Minion suggests retraining.
const REENROLMENT_DROP: f32 = 0.15;

/// How many days each window (baseline and rolling) covers.
const DRIFT_WINDOW_DAYS: u32 = 7;

/// Every owner voice score in the log, oldest first, with the date it was
/// logged on — the raw numbers `Report::voice_by_day` already summarises
/// per day, kept here instead so a 7-day window can be drawn across day
/// boundaries.
fn owner_voice_scores(contents: &str) -> Vec<(String, f32)> {
    contents
        .lines()
        .filter_map(parse_line)
        .filter_map(|(date, event)| match event {
            Event::VoiceMatched { likeness } => Some((date.to_string(), likeness)),
            _ => None,
        })
        .collect()
}

/// Whether the owner's voice is scoring meaningfully worse than it did
/// when scoring began, and by how much.
///
/// There is nowhere today that records the median at the moment a voice
/// was actually enrolled — that would mean writing to the profile
/// `speaker.rs` owns, or a sidecar next to it, from `enroll.rs`. Neither
/// is touched here: the baseline is instead the median over the *earliest*
/// `DRIFT_WINDOW_DAYS` days the log has a voice score for, which in
/// practice is the same period, since scoring starts the moment a profile
/// exists. A log that was rotated past that point reads as "no baseline"
/// rather than a wrong one — see the `None` case below.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drift {
    pub baseline_median: Option<f32>,
    pub recent_median: Option<f32>,
    pub recent_min: Option<f32>,
    /// `baseline_median - recent_median`; positive means it got worse.
    pub drop: Option<f32>,
    pub should_suggest: bool,
}

/// Computes [`Drift`] from the log's text and today's date (`"YYYY-MM-DD"`,
/// passed in rather than read from the clock — see the note on
/// [`analyse`]).
pub fn voice_drift(contents: &str, today: &str) -> Drift {
    let scores = owner_voice_scores(contents);
    let Some((earliest, _)) = scores.first() else {
        return Drift {
            baseline_median: None,
            recent_median: None,
            recent_min: None,
            drop: None,
            should_suggest: false,
        };
    };
    let Some(earliest_date) = chrono::NaiveDate::parse_from_str(earliest, "%Y-%m-%d").ok() else {
        return Drift {
            baseline_median: None,
            recent_median: None,
            recent_min: None,
            drop: None,
            should_suggest: false,
        };
    };
    let baseline_end =
        (earliest_date + chrono::Duration::days((DRIFT_WINDOW_DAYS - 1) as i64)).format("%Y-%m-%d").to_string();
    let baseline: Vec<f32> = scores
        .iter()
        .filter(|(date, _)| date.as_str() <= baseline_end.as_str())
        .map(|(_, score)| *score)
        .collect();
    let baseline_median = median(&baseline);

    let recent_start = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .map(|date| back(date, DRIFT_WINDOW_DAYS).format("%Y-%m-%d").to_string());
    let recent: Vec<f32> = match &recent_start {
        Ok(start) => scores
            .iter()
            .filter(|(date, _)| date.as_str() >= start.as_str() && date.as_str() <= today)
            .map(|(_, score)| *score)
            .collect(),
        Err(_) => Vec::new(),
    };
    let recent_median = median(&recent);
    let recent_min =
        recent.iter().copied().fold(None, |acc, x| Some(acc.map_or(x, |a: f32| a.min(x))));

    let drop = match (baseline_median, recent_median) {
        (Some(base), Some(recent)) => Some(base - recent),
        _ => None,
    };
    // The baseline window and the rolling window are the same stretch of
    // log while the owner has fewer than `DRIFT_WINDOW_DAYS` days of
    // history — comparing a window to itself always reads as "no drop",
    // which is the right answer: there is nothing yet to compare against.
    let overlapping = baseline_end.as_str() >= recent_start.as_deref().unwrap_or("");
    let should_suggest = !overlapping && drop.is_some_and(|d| d >= REENROLMENT_DROP);

    Drift { baseline_median, recent_median, recent_min, drop, should_suggest }
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

    out.push_str(&drift_line(&report.drift));
    out.push_str(&format!("Consejo: {}\n", advice(report)));
    out
}

/// The «Reentrenamiento» line, only shown once there is enough history to
/// say anything: fewer than `DRIFT_WINDOW_DAYS` days of voice scores read
/// as "sin datos suficientes" rather than a false "todo bien" or a
/// spurious drop computed against an empty baseline.
fn drift_line(drift: &Drift) -> String {
    match (drift.baseline_median, drift.recent_median) {
        (Some(baseline), Some(recent)) => {
            let verdict = if drift.should_suggest {
                "vuelve a entrenar tu voz con `minion enroll`"
            } else {
                "sin motivo para volver a entrenar"
            };
            format!(
                "Reentrenamiento: línea base {baseline:.2}, últimos {DRIFT_WINDOW_DAYS} días \
                 {recent:.2} (mínimo {}) — {verdict}.\n",
                drift.recent_min.map_or_else(|| "—".to_string(), |m| format!("{m:.2}")),
            )
        }
        _ => format!(
            "Reentrenamiento: sin datos suficientes todavía (hacen falta \
             {DRIFT_WINDOW_DAYS} días de voz registrada).\n"
        ),
    }
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

/// The date `days` back from `from`, inclusive of `from` itself — so
/// `back(from, 1) == from`. Pure: the clock is only ever read by the one
/// caller that needs "now" for real, [`cutoff_date`].
fn back(from: chrono::NaiveDate, days: u32) -> chrono::NaiveDate {
    from - chrono::Duration::days(days.saturating_sub(1) as i64)
}

/// `--days N` means the last `N` days including today, local time.
fn cutoff_date(days: u32) -> String {
    back(chrono::Local::now().date_naive(), days).format("%Y-%m-%d").to_string()
}

/// The report over the real log, ready to print or show in a window. The
/// one function here that touches the filesystem or the clock — everything
/// it calls is pure and tested on fixture text instead.
pub fn report_text(days: Option<u32>) -> String {
    let contents = read_log();
    let threshold = crate::config::load().voice_threshold();
    let cutoff = days.and_then(|n| if n == 0 { None } else { Some(cutoff_date(n)) });
    let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
    let health = build_health(&contents, &current_microphone_name(), ai_backend_name().as_deref());
    let mut out = estado_block(&health);
    out.push_str(&render(&analyse(&contents, cutoff.as_deref(), threshold, &today)));
    out
}

/// [`Drift`] over the real log, as of today.
pub fn current_drift() -> Drift {
    let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
    voice_drift(&read_log(), &today)
}

/// How long a re-enrolment suggestion, once shown, stays shown before it
/// may be shown again — see [`reenrolment_check_due`].
const REENROLMENT_SUGGEST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(86_400);

/// Where the timestamp of the last suggestion lives — same idea as
/// `updater::last_check_path`: the file's own mtime is the timestamp, so
/// there is nothing to parse or get wrong.
fn last_suggested_path() -> Option<std::path::PathBuf> {
    // Tests never touch the user's Application Support.
    if cfg!(test) {
        return None;
    }
    let mut path = crate::config::path()?;
    path.set_file_name("last-reenrolment-suggestion");
    Some(path)
}

/// Whether a re-enrolment notification may fire right now: never more
/// than once a day.
pub fn reenrolment_check_due() -> bool {
    let last = last_suggested_path()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok());
    match last {
        None => true,
        Some(last) => std::time::SystemTime::now()
            .duration_since(last)
            .map(|since| since >= REENROLMENT_SUGGEST_INTERVAL)
            .unwrap_or(false),
    }
}

/// Records that a re-enrolment notification was just shown.
pub fn record_reenrolment_suggested() {
    if let Some(path) = last_suggested_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, "");
    }
}

/// What Minion currently knows about its own health: the tooltip's short
/// line and the «Estado» block in `minion stats` both come from this.
///
/// `microphone` and `ai_backend` are handed in rather than looked up here:
/// they are the *configured* facts, cheap to read from anywhere, while
/// `vad_mode` and `own_audio_tap` are the *live* ones — whether Silero
/// actually loaded, whether the loopback tap actually opened — and the
/// only record of that is the line each one already logs on success or
/// failure (`audio::open_voice`'s three `journal::write` calls, and the
/// two `note!` calls around `loopback::Loopback::start()` in `main.rs`).
/// Reading those back is simpler than threading a second, cross-thread
/// status flag through code this change does not otherwise own.
#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    pub microphone: String,
    pub vad_mode: String,
    pub own_audio_tap: String,
    pub ai_backend: Option<String>,
    /// The last three `error`/`BLOCKED`/`fatal` lines, oldest first.
    pub last_problems: Vec<String>,
}

/// The most recent line saying which voice detector is actually in use,
/// read backwards so a Silero failure logged after a working start does
/// not report the state it started in.
fn last_vad_mode(contents: &str) -> Option<&'static str> {
    contents.lines().rev().find_map(|line| {
        let rest = line.get(21..)?;
        if rest.starts_with("Voice detector: Silero.") {
            Some("Silero")
        } else if rest.starts_with("Voice detector: energy only")
            || rest.starts_with("Voice detector: cannot load Silero")
        {
            Some("energía")
        } else {
            None
        }
    })
}

/// The most recent line saying whether the Mac's own audio is being
/// ignored — the "grifo" (tap) the tooltip's example refers to.
fn last_own_audio_tap(contents: &str) -> Option<&'static str> {
    contents.lines().rev().find_map(|line| {
        let rest = line.get(21..)?;
        if rest.starts_with("Ignoring the Mac's own audio") {
            Some("grifo ok")
        } else if rest.starts_with("own-audio tap unavailable") {
            Some("grifo no disponible")
        } else {
            None
        }
    })
}

/// The last `n` lines that recorded an error, a blocked command, or a
/// fatal exit, oldest first — a much shorter list than reading the whole
/// log, and the three shapes most worth a glance at without opening it.
fn last_problem_lines(contents: &str, n: usize) -> Vec<String> {
    let mut found: Vec<String> = contents
        .lines()
        .filter(|line| {
            line.get(21..).is_some_and(|rest| {
                rest.starts_with("error    ")
                    || rest.starts_with("fatal    ")
                    || rest.starts_with("BLOCKED  ")
            })
        })
        .map(str::to_string)
        .collect();
    let start = found.len().saturating_sub(n);
    found.split_off(start)
}

/// Builds [`Health`] from a log's text and the two facts that only
/// main.rs and this module's own `health()` wrapper can supply cheaply.
/// Pure — exercised on fixture text, same as everything else here.
fn build_health(contents: &str, microphone: &str, ai_backend: Option<&str>) -> Health {
    Health {
        microphone: microphone.to_string(),
        vad_mode: last_vad_mode(contents).unwrap_or("—").to_string(),
        own_audio_tap: last_own_audio_tap(contents).unwrap_or("—").to_string(),
        ai_backend: ai_backend.map(str::to_string),
        last_problems: last_problem_lines(contents, 3),
    }
}

/// The «Estado» block: microphone, voice detector, own-audio tap, AI
/// backend, and the last few problems — the body for both `minion stats`
/// and the «Estadísticas…» window, same as [`render`].
pub fn estado_block(health: &Health) -> String {
    let mut out = String::new();
    out.push_str("Estado\n\n");
    out.push_str(&format!("Micrófono: {}\n", health.microphone));
    out.push_str(&format!("Detector de voz: {}\n", health.vad_mode));
    out.push_str(&format!("Audio propio: {}\n", health.own_audio_tap));
    out.push_str(&format!(
        "IA: {}\n",
        health.ai_backend.as_deref().unwrap_or("desactivada")
    ));
    if health.last_problems.is_empty() {
        out.push_str("Sin errores recientes.\n");
    } else {
        out.push_str("Últimos problemas:\n");
        for line in &health.last_problems {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.push('\n');
    out
}

/// The short line the tooltip appends after «Minion — escuchando» —
/// «micro: MacBook Pro · Silero · grifo ok · IA: Claude Code».
pub fn tooltip_health(health: &Health) -> String {
    format!(
        "micro: {} · {} · {} · IA: {}",
        health.microphone,
        health.vad_mode,
        health.own_audio_tap,
        health.ai_backend.as_deref().unwrap_or("no")
    )
}

/// The microphone Minion would open right now: the configured name, or
/// the system default's. A miniature of `audio::choose_input`'s own
/// resolution — not shared with it, since that function also opens the
/// stream, and this is only ever asked for a name to show.
fn current_microphone_name() -> String {
    use cpal::traits::{DeviceTrait, HostTrait};
    if let Some(name) = crate::config::load().microphone().filter(|n| !n.trim().is_empty()) {
        return name;
    }
    cpal::default_host()
        .default_input_device()
        .and_then(|device| device.description().ok())
        .map(|description| description.name().to_string())
        .unwrap_or_else(|| "predeterminado".to_string())
}

/// The configured `[ai] backend`, or `None` when the AI layer is off.
fn ai_backend_name() -> Option<String> {
    let backend = crate::config::load().ai.backend.trim().to_string();
    (!backend.is_empty()).then_some(backend)
}

/// [`Health`] as of right now, over the real log.
pub fn health() -> Health {
    build_health(&read_log(), &current_microphone_name(), ai_backend_name().as_deref())
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
        let report = analyse(LOG, None, 0.32, "2026-09-02");
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
        let report = analyse(LOG, None, 0.32, "2026-09-02");
        let (command, n) = report.command_runs.iter().find(|(c, _)| c == "abrir Chrome").unwrap();
        assert_eq!(command, "abrir Chrome");
        assert_eq!(*n, 2);
    }

    #[test]
    fn unknown_phrases_are_grouped_and_counted() {
        let report = analyse(LOG, None, 0.32, "2026-09-02");
        assert_eq!(report.top_unknown, vec![("Minion haz un pino.".to_string(), 2)]);
    }

    #[test]
    fn voice_scores_are_summarised_per_day() {
        let report = analyse(LOG, None, 0.32, "2026-09-02");
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
        let report = analyse(LOG, None, 0.32, "2026-09-02");
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
        let report = analyse(LOG, Some("2026-09-02"), 0.32, "2026-09-02");
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
        let report = analyse(LOG, None, 0.32, "2026-09-02");
        assert!(advice(&report).contains("Minion haz un pino"));
    }

    #[test]
    fn advice_falls_back_to_blocked_commands_then_a_tight_voice_margin() {
        let no_unknown = "\
2026-09-01 12:00:01  BLOCKED  «Minion cierra la ventana.»  ->  cerrar ventana: not permitted\n";
        assert!(advice(&analyse(no_unknown, None, 0.32, "2026-09-01")).contains("bloqueadas"));

        let tight_margin = "2026-09-01 12:00:01  voice    matched at 0.35\n";
        assert!(advice(&analyse(tight_margin, None, 0.32, "2026-09-01")).contains("margen de voz"));

        let comfortable = "\
2026-09-01 12:00:00  ran      «Minion Chrome.»  ->  abrir Chrome  [100% · 1.0s audio · 100 ms]\n\
2026-09-01 12:00:01  voice    matched at 0.90\n";
        assert!(advice(&analyse(comfortable, None, 0.32, "2026-09-01")).contains("Todo va bien"));
    }

    #[test]
    fn a_voice_match_reads_with_or_without_a_name() {
        // Profiles have names now — "voice    Ana matched at 0.61" — but a
        // log written before they did is still the same file.
        let named = "2026-09-01 12:00:01  voice    Ana matched at 0.35\n";
        assert!(advice(&analyse(named, None, 0.32, "2026-09-01")).contains("margen de voz"));
        let unnamed = "2026-09-01 12:00:01  voice    matched at 0.35\n";
        assert!(advice(&analyse(unnamed, None, 0.32, "2026-09-01")).contains("margen de voz"));
    }

    #[test]
    fn renders_a_readable_report() {
        let report = analyse(LOG, None, 0.32, "2026-09-02");
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

    /// One `voice matched` line for `date` at `score`.
    fn voice_line(date: &str, score: f32) -> String {
        format!("{date} 09:00:00  voice    matched at {score:.2}\n")
    }

    /// A log with one voice score a day, starting `start` (`"YYYY-MM-DD"`)
    /// and running `days` days, all at `score`.
    fn voice_log(start: chrono::NaiveDate, days: i64, score: f32) -> String {
        (0..days)
            .map(|n| voice_line(&(start + chrono::Duration::days(n)).format("%Y-%m-%d").to_string(), score))
            .collect()
    }

    #[test]
    fn a_steady_voice_suggests_nothing() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        // 30 days at the same score: baseline and rolling windows agree.
        let log = voice_log(start, 30, 0.70);
        let today = (start + chrono::Duration::days(29)).format("%Y-%m-%d").to_string();
        let drift = voice_drift(&log, &today);
        assert_eq!(drift.baseline_median, Some(0.70));
        assert_eq!(drift.recent_median, Some(0.70));
        assert!(!drift.should_suggest);
    }

    #[test]
    fn a_voice_that_drops_a_lot_suggests_retraining() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut log = voice_log(start, DRIFT_WINDOW_DAYS as i64, 0.70);
        let recent_start = start + chrono::Duration::days(20);
        log.push_str(&voice_log(recent_start, DRIFT_WINDOW_DAYS as i64, 0.50));
        let today = (recent_start + chrono::Duration::days((DRIFT_WINDOW_DAYS - 1) as i64))
            .format("%Y-%m-%d")
            .to_string();
        let drift = voice_drift(&log, &today);
        assert_eq!(drift.baseline_median, Some(0.70));
        assert_eq!(drift.recent_median, Some(0.50));
        assert!((drift.drop.unwrap() - 0.20).abs() < 1e-5, "{:?}", drift.drop);
        assert!(drift.should_suggest);
    }

    #[test]
    fn a_small_drop_does_not_suggest_retraining() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut log = voice_log(start, DRIFT_WINDOW_DAYS as i64, 0.70);
        let recent_start = start + chrono::Duration::days(20);
        // 0.10 below the baseline: real, but under the 0.15 bar.
        log.push_str(&voice_log(recent_start, DRIFT_WINDOW_DAYS as i64, 0.60));
        let today = (recent_start + chrono::Duration::days((DRIFT_WINDOW_DAYS - 1) as i64))
            .format("%Y-%m-%d")
            .to_string();
        assert!(!voice_drift(&log, &today).should_suggest);
    }

    #[test]
    fn less_than_a_window_of_history_never_suggests() {
        // Three days total: baseline and rolling windows are the same
        // three days, so a "drop" here would be comparing a number to
        // itself, not real drift.
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let log = voice_log(start, 3, 0.40);
        let today = (start + chrono::Duration::days(2)).format("%Y-%m-%d").to_string();
        let drift = voice_drift(&log, &today);
        assert!(!drift.should_suggest);
    }

    #[test]
    fn no_voice_scores_at_all_reads_as_no_baseline() {
        let drift = voice_drift("", "2026-01-01");
        assert_eq!(drift.baseline_median, None);
        assert_eq!(drift.recent_median, None);
        assert!(!drift.should_suggest);
    }

    #[test]
    fn the_drift_line_names_the_command_once_there_is_a_baseline() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut log = voice_log(start, DRIFT_WINDOW_DAYS as i64, 0.70);
        let recent_start = start + chrono::Duration::days(20);
        log.push_str(&voice_log(recent_start, DRIFT_WINDOW_DAYS as i64, 0.50));
        let today = (recent_start + chrono::Duration::days((DRIFT_WINDOW_DAYS - 1) as i64))
            .format("%Y-%m-%d")
            .to_string();
        let line = drift_line(&voice_drift(&log, &today));
        assert!(line.contains("minion enroll"), "{line}");
    }

    const HEALTH_LOG: &str = "\
2026-09-01 12:00:00  Voice detector: energy only, by configuration.\n\
2026-09-01 12:00:01  own-audio tap unavailable: no output device\n\
2026-09-01 12:00:02  error    could not reload the model: boom\n\
2026-09-01 12:00:03  ran      «Minion Chrome.»  ->  abrir Chrome  [100% · 1.6s audio · 142 ms]\n\
2026-09-01 12:00:04  Voice detector: Silero.\n\
2026-09-01 12:00:05  Ignoring the Mac's own audio: output tap open at 48000 Hz, threshold 0.02.\n\
2026-09-01 12:00:06  BLOCKED  «Minion cierra la ventana.»  ->  cerrar ventana: not permitted\n\
2026-09-01 12:00:07  error    transcription failed: bad frame\n\
2026-09-01 12:00:08  fatal    the model could not be loaded\n\
2026-09-01 12:00:09  error    could not learn «hola»: reason\n\
";

    #[test]
    fn vad_mode_and_tap_read_the_most_recent_line() {
        // Silero and the tap both fail once, then recover — the report
        // must say what is true now, not what happened first.
        assert_eq!(last_vad_mode(HEALTH_LOG), Some("Silero"));
        assert_eq!(last_own_audio_tap(HEALTH_LOG), Some("grifo ok"));
    }

    #[test]
    fn a_log_with_neither_line_reports_neither() {
        assert_eq!(last_vad_mode(""), None);
        assert_eq!(last_own_audio_tap(""), None);
    }

    #[test]
    fn only_the_last_three_problems_are_kept_oldest_first() {
        let problems = last_problem_lines(HEALTH_LOG, 3);
        assert_eq!(problems.len(), 3);
        assert!(problems[0].contains("transcription failed"));
        assert!(problems[1].contains("the model could not be loaded"));
        assert!(problems[2].contains("could not learn"));
        // The first error and the blocked line both fell off the end.
        assert!(!problems.iter().any(|line| line.contains("boom")));
    }

    #[test]
    fn build_health_combines_the_log_with_what_it_is_handed() {
        let health = build_health(HEALTH_LOG, "MacBook Pro", Some("Claude Code"));
        assert_eq!(health.microphone, "MacBook Pro");
        assert_eq!(health.vad_mode, "Silero");
        assert_eq!(health.own_audio_tap, "grifo ok");
        assert_eq!(health.ai_backend.as_deref(), Some("Claude Code"));
        assert_eq!(health.last_problems.len(), 3);
    }

    #[test]
    fn health_with_no_ai_backend_reads_as_off() {
        let health = build_health("", "MacBook Pro", None);
        assert_eq!(health.vad_mode, "—");
        assert_eq!(health.own_audio_tap, "—");
        assert_eq!(health.ai_backend, None);
        assert!(health.last_problems.is_empty());
    }

    #[test]
    fn the_tooltip_line_matches_the_brief_s_example_shape() {
        let health = build_health(HEALTH_LOG, "MacBook Pro", Some("Claude Code"));
        assert_eq!(
            tooltip_health(&health),
            "micro: MacBook Pro · Silero · grifo ok · IA: Claude Code"
        );
    }

    #[test]
    fn the_tooltip_line_says_no_ai_when_it_is_off() {
        let health = build_health(HEALTH_LOG, "MacBook Pro", None);
        assert!(tooltip_health(&health).ends_with("IA: no"));
    }

    #[test]
    fn estado_block_names_every_field_and_a_recent_problem() {
        let health = build_health(HEALTH_LOG, "MacBook Pro", Some("Claude Code"));
        let block = estado_block(&health);
        assert!(block.contains("Estado"));
        assert!(block.contains("MacBook Pro"));
        assert!(block.contains("Silero"));
        assert!(block.contains("grifo ok"));
        assert!(block.contains("Claude Code"));
        assert!(block.contains("could not learn"));
    }

    #[test]
    fn estado_block_says_so_when_there_is_nothing_recent() {
        let health = build_health("", "MacBook Pro", None);
        assert!(estado_block(&health).contains("Sin errores recientes"));
    }
}

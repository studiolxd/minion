//! The "what did it hear" HUD: a small floating panel, top-right of the
//! main screen, that shows the current face, what was last heard and what
//! came of it — and, while dictating, the text typed so far.
//!
//! Deliberately separate from the menu bar's tooltip: a tooltip only shows
//! up when you hover the mouse over it, which is nowhere near where you
//! are looking while talking to the machine. This is the same information,
//! placed somewhere it can actually be seen.
//!
//! The panel itself never takes focus (`NonactivatingPanel`) and lives on
//! every Space, above ordinary windows — see [`Hud::new`]. When it should
//! be visible is decided by [`Visibility`], a small pure state machine with
//! no AppKit in it, so the rules ("hides four seconds after the last
//! update", "stays open while dictating or a question is pending") can be
//! tested without a screen.
//!
//! What the listening loop feeds in — the dictation text, whether a
//! learning question is pending, that an utterance just opened, and the
//! transcript/decision as they arrive — comes through module-level
//! functions ([`set_dictation_text`], [`set_question_pending`],
//! [`note_speech_open`], [`push_update`]) rather than a struct field: the
//! loop that owns that state is not this file's to change (see
//! CLAUDE.md), and a free function is the smallest thing that could be
//! added to it from outside. The panel appears the instant speech opens —
//! before there is anything to show but an animated ellipsis — and its two
//! lines fill in independently as the transcript, then the decision,
//! arrive; see [`Content`].

use std::cell::{Cell, RefCell};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSColor, NSFont, NSImage, NSImageScaling, NSImageView,
    NSLineBreakMode, NSPanel, NSScreen, NSStatusWindowLevel, NSTextField,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSData, NSPoint, NSRect, NSSize, NSString};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

const WIDTH: f64 = 320.0;
const HEIGHT: f64 = 90.0;
/// Gap from the screen's top-right corner.
const MARGIN: f64 = 10.0;
/// Corner radius of the panel itself, in points.
const CORNER_RADIUS: f64 = 14.0;
/// How long the panel stays up in tests, standing in for the configured
/// `hud_seconds` — production code always gets a resolved value from
/// `config::Config::hud_seconds`, via [`Hud::new`].
#[cfg(test)]
const HUD_SECONDS: f64 = 4.0;
/// The face, drawn at this height in points — a third of the menu bar
/// icon's pixel height, which is right for a panel this size.
const FACE_POINTS: f64 = 32.0;

const AWAKE: &str = include_str!("../assets/awake.svg");
const ASLEEP: &str = include_str!("../assets/asleep.svg");
const ACTING: &str = include_str!("../assets/acting.svg");
const DICTATING: &str = include_str!("../assets/dictating.svg");
const THINKING: &str = include_str!("../assets/thinking.svg");
const SPEAKING: &str = include_str!("../assets/speaking.svg");

/// Which drawing the HUD's face shows — the same six faces as the menu
/// bar icon (`icon.rs`), redrawn here at the HUD's own size rather than
/// reused as pixels: a menu bar `Icon` keeps no way to get its pixels back
/// out once built (see `icon.rs`'s `render`), so the source SVGs are the
/// only thing there is to share.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    Awake,
    Asleep,
    Acting,
    Dictating,
    Thinking,
    Speaking,
}

impl Face {
    fn svg(self) -> &'static str {
        match self {
            Face::Awake => AWAKE,
            Face::Asleep => ASLEEP,
            Face::Acting => ACTING,
            Face::Dictating => DICTATING,
            Face::Thinking => THINKING,
            Face::Speaking => SPEAKING,
        }
    }
}

/// Whether the panel should be on screen right now.
///
/// Pure and given `now` explicitly rather than reading the clock itself,
/// so the rules can be tested directly. Four things can hold it open:
/// being pinned from Ajustes, dictation in progress, a learning question
/// waiting for an answer, or simply having heard something recently
/// (`activity_until`). "Esconde lo que oyes" cuts through all but the
/// pin — it is the one thing here that means "no, really, not now".
#[derive(Debug, Default)]
struct Visibility {
    pinned: bool,
    dictating: bool,
    question_pending: bool,
    /// Waiting on the AI layer — see [`crate::ai::ask`]. Can run several
    /// times longer than an ordinary utterance, so it holds the panel open
    /// the same way `question_pending` does rather than relying on
    /// `activity_until`, which would expire long before an answer arrives.
    busy: bool,
    activity_until: Option<Instant>,
    forced_hidden: bool,
    /// How long [`Self::note_heard`] keeps the panel up — `[hud_seconds]`
    /// in `config.toml`, resolved once when the HUD is built.
    hud_seconds: f64,
}

impl Visibility {
    fn with_hud_seconds(hud_seconds: f64) -> Self {
        Self { hud_seconds, ..Self::default() }
    }

    /// A wake word was heard and understood (or not) — keep the panel up
    /// for [`Self::hud_seconds`] from now.
    fn note_heard(&mut self, now: Instant) {
        self.activity_until = Some(now + Duration::from_secs_f64(self.hud_seconds));
        self.forced_hidden = false;
    }

    fn set_dictating(&mut self, on: bool) {
        self.dictating = on;
        if on {
            self.forced_hidden = false;
        }
    }

    fn set_question_pending(&mut self, on: bool) {
        self.question_pending = on;
        if on {
            self.forced_hidden = false;
        }
    }

    fn set_busy(&mut self, on: bool) {
        self.busy = on;
        if on {
            self.forced_hidden = false;
        }
    }

    fn set_pinned(&mut self, on: bool) {
        self.pinned = on;
    }

    /// "Muestra lo que oyes": behaves like something was just heard.
    fn show_forced(&mut self, now: Instant) {
        self.note_heard(now);
    }

    /// "Esconde lo que oyes": hidden until the next thing worth showing.
    fn hide_forced(&mut self) {
        self.forced_hidden = true;
        self.activity_until = None;
    }

    fn visible(&self, now: Instant) -> bool {
        if self.pinned {
            return true;
        }
        if self.forced_hidden {
            return false;
        }
        self.dictating
            || self.question_pending
            || self.busy
            || self.activity_until.is_some_and(|until| now < until)
    }
}

/// What the listening loop found out about the utterance in progress: the
/// transcript, the decision, or both — however much is known so far.
/// Pushed through [`push_update`] rather than parsed back out of the
/// tooltip, which only ever held the finished pair (see the module doc's
/// history and `main.rs`'s `parse_last_utterance`, kept only for "Últimas
/// órdenes").
#[derive(Debug, Default, Clone)]
pub struct Update {
    pub heard: Option<String>,
    pub outcome: Option<String>,
}

/// The panel's two lines: what was heard, and what it became. Each starts
/// "pending" — drawn as the animated ellipsis — the moment speech opens,
/// and stops being pending once [`push_update`] supplies it. Pure, so the
/// sequence (open → heard → outcome) can be tested without AppKit.
#[derive(Debug, Default)]
struct Content {
    heard: String,
    heard_pending: bool,
    outcome: String,
    outcome_pending: bool,
}

impl Content {
    /// Silero opened a new utterance: both lines go back to pending, even
    /// if the last one never resolved (e.g. it turned out to be noise).
    fn note_speech_open(&mut self) {
        self.heard.clear();
        self.heard_pending = true;
        self.outcome.clear();
        self.outcome_pending = true;
    }

    fn push_update(&mut self, update: Update) {
        if let Some(heard) = update.heard {
            self.heard = heard;
            self.heard_pending = false;
        }
        if let Some(outcome) = update.outcome {
            self.outcome = outcome;
            self.outcome_pending = false;
        }
    }
}

/// Which ellipsis frame to show after `elapsed_ms` of waiting — "·", "··",
/// "···" cycling every 300 ms, so a still-open panel visibly breathes
/// rather than looking stuck.
fn ellipsis_frame(elapsed_ms: u64) -> &'static str {
    const FRAMES: [&str; 3] = ["·", "··", "···"];
    const FRAME_MS: u64 = 300;
    FRAMES[(elapsed_ms / FRAME_MS) as usize % FRAMES.len()]
}

/// Cuts `text` to at most `max_chars`, keeping the end — while dictating,
/// what was typed most recently is what the cursor is at, so that is what
/// has to stay on screen when the buffer outgrows the panel.
pub fn tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars || max_chars == 0 {
        return text.to_string();
    }
    let skip = count - (max_chars - 1);
    "…".to_string() + &text.chars().skip(skip).collect::<String>()
}

/// Cuts `text` to at most `max_chars`, keeping the start and marking that
/// it was cut — for the single-line "heard" and "outcome" rows, which have
/// no room to wrap.
pub fn head(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars || max_chars == 0 {
        return trimmed.to_string();
    }
    trimmed.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
}

/// How many lines `text` wraps to at roughly `chars_per_line`, capped at
/// `max_lines` — decides how tall the dictation row needs to be without
/// asking AppKit to measure it. Same approximation `preferences.rs` uses
/// for its hints: erring long only leaves blank space, erring short cuts
/// words off.
fn wrapped_lines(text: &str, chars_per_line: usize, max_lines: usize) -> usize {
    if text.is_empty() {
        return 1;
    }
    let per_line = chars_per_line.max(1);
    let lines = text.chars().count().div_ceil(per_line).max(1);
    lines.min(max_lines)
}

/// Renders one of the faces at [`FACE_POINTS`], as PNG bytes ready for
/// [`NSImage`].
fn render_face(svg: &str) -> Option<Vec<u8>> {
    let tree = Tree::from_str(svg, &Options::default()).ok()?;
    let size = tree.size();
    // Twice the point size, for Retina — the same bargain `icon.rs` makes.
    let scale = (FACE_POINTS as f32 * 2.0) / size.height();
    let width = (size.width() * scale).round().max(1.0) as u32;
    let height = (FACE_POINTS as f32 * 2.0).round() as u32;
    let mut pixmap = Pixmap::new(width, height)?;
    resvg::render(&tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().ok()
}

fn face_image(svg: &str) -> Option<Retained<NSImage>> {
    let png = render_face(svg)?;
    let data = NSData::with_bytes(&png);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    // Points, not pixels: the PNG was rendered at 2x for Retina.
    image.setSize(NSSize::new(FACE_POINTS, FACE_POINTS));
    // A template, like the menu bar icon: black strokes on transparent
    // vanished against the dark HUD material. Tinted by the view below.
    image.setTemplate(true);
    Some(image)
}

fn label(mtm: MainThreadMarker, frame: NSRect, size: f64, secondary: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    field.setFrame(frame);
    field.setFont(Some(&NSFont::systemFontOfSize(size)));
    let color: Retained<NSColor> =
        if secondary { NSColor::secondaryLabelColor() } else { NSColor::labelColor() };
    field.setTextColor(Some(&color));
    field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    field
}

/// The floating panel itself.
pub struct Hud {
    window: Retained<NSPanel>,
    face_view: Retained<NSImageView>,
    heard_label: Retained<NSTextField>,
    outcome_label: Retained<NSTextField>,
    state: RefCell<Visibility>,
    shown: Cell<bool>,
    current_face: Cell<Option<Face>>,
    content: RefCell<Content>,
    /// When the current pending spell (speech open, or waiting on a
    /// transcript/outcome) began — the ellipsis animates from here.
    pending_since: Cell<Option<Instant>>,
    last_dictation: RefCell<String>,
    dictating: Cell<bool>,
}

impl Hud {
    /// Builds the panel, hidden until something calls [`Hud::tick`] with a
    /// reason to show it.
    pub fn new(mtm: MainThreadMarker, pinned: bool, hud_seconds: f64) -> Self {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSPanel>(),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setFloatingPanel(true);
        window.setBecomesKeyOnlyIfNeeded(true);
        window.setHidesOnDeactivate(false);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
        window.setLevel(NSStatusWindowLevel);
        window.setIgnoresMouseEvents(true);
        window.setMovable(false);
        // Every Space, including full-screen ones' own — a HUD that only
        // showed up on the Space it was created on would look broken the
        // moment you switched away from it. `Stationary`: it must not
        // follow you to whichever Space becomes active, since it is not
        // attached to any one window on it.
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );

        let effect = NSVisualEffectView::new(mtm);
        effect.setFrame(frame);
        effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::Active);
        effect.setWantsLayer(true);
        if let Some(layer) = effect.layer() {
            layer.setCornerRadius(CORNER_RADIUS);
            layer.setMasksToBounds(true);
        }

        const PAD: f64 = 14.0;
        let face_frame = NSRect::new(
            NSPoint::new(PAD, HEIGHT - PAD - FACE_POINTS),
            NSSize::new(FACE_POINTS, FACE_POINTS),
        );
        let face_view = NSImageView::new(mtm);
        face_view.setFrame(face_frame);
        face_view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        face_view.setContentTintColor(Some(&NSColor::labelColor()));
        face_view.setAccessibilityElement(false);

        let text_x = PAD + FACE_POINTS + 10.0;
        let text_width = WIDTH - text_x - PAD;
        let heard_label = label(
            mtm,
            NSRect::new(NSPoint::new(text_x, HEIGHT - PAD - 18.0), NSSize::new(text_width, 18.0)),
            13.0,
            false,
        );
        let outcome_label = label(
            mtm,
            NSRect::new(NSPoint::new(text_x, PAD), NSSize::new(text_width, HEIGHT - 2.0 * PAD - 18.0)),
            11.0,
            true,
        );
        outcome_label.setUsesSingleLineMode(false);
        outcome_label.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        outcome_label.setMaximumNumberOfLines(0);

        effect.addSubview(&face_view);
        effect.addSubview(&heard_label);
        effect.addSubview(&outcome_label);
        window.setContentView(Some(&effect));
        position(&window, mtm);

        let mut state = Visibility::with_hud_seconds(hud_seconds);
        state.set_pinned(pinned);
        Self {
            window,
            face_view,
            heard_label,
            outcome_label,
            state: RefCell::new(state),
            shown: Cell::new(false),
            current_face: Cell::new(None),
            content: RefCell::new(Content::default()),
            pending_since: Cell::new(None),
            last_dictation: RefCell::new(String::new()),
            dictating: Cell::new(false),
        }
    }

    pub fn set_pinned(&self, on: bool) {
        self.state.borrow_mut().set_pinned(on);
    }

    /// Whether the panel is on screen right now — used to keep the run
    /// loop's timer at full speed (50 ms) while it is, so the ellipsis
    /// animates smoothly instead of stepping once every throttled tick.
    pub fn is_visible(&self) -> bool {
        self.shown.get()
    }

    pub fn set_dictating(&self, on: bool) {
        self.dictating.set(on);
        self.state.borrow_mut().set_dictating(on);
        if !on {
            self.last_dictation.borrow_mut().clear();
        }
    }

    pub fn set_face(&self, face: Face) {
        if self.current_face.get() == Some(face) {
            return;
        }
        self.current_face.set(Some(face));
        if let Some(image) = face_image(face.svg()) {
            self.face_view.setImage(Some(&image));
        }
    }

    /// Reads what the listening loop last reported through
    /// [`set_dictation_text`], [`set_question_pending`], [`note_speech_open`]
    /// and [`push_update`], applies "muestra/esconde lo que oyes" if one was
    /// said since the last call, and shows or hides the panel — the one
    /// thing to call once a tick.
    pub fn tick(&self, now: Instant) {
        if let Some(show) = take_request() {
            let mut state = self.state.borrow_mut();
            if show {
                state.show_forced(now);
            } else {
                state.hide_forced();
            }
        }
        self.state.borrow_mut().set_question_pending(question_pending());
        self.state.borrow_mut().set_busy(busy());

        if take_speech_open() {
            self.content.borrow_mut().note_speech_open();
            self.pending_since.set(Some(now));
            self.state.borrow_mut().note_heard(now);
        }
        if let Some(update) = take_update() {
            self.content.borrow_mut().push_update(update);
            self.state.borrow_mut().note_heard(now);
        }

        if self.dictating.get() {
            let text = dictation_text();
            if *self.last_dictation.borrow() != text {
                *self.last_dictation.borrow_mut() = text;
            }
        }

        let visible = self.state.borrow().visible(now);
        if visible {
            self.repaint(now);
            if !self.shown.get() {
                self.window.orderFrontRegardless();
                self.shown.set(true);
            }
        } else if self.shown.get() {
            self.window.orderOut(None);
            self.shown.set(false);
        }
    }

    fn repaint(&self, now: Instant) {
        const CHARS_PER_LINE: usize = 44;

        let elapsed_ms = self
            .pending_since
            .get()
            .map(|since| now.saturating_duration_since(since).as_millis() as u64)
            .unwrap_or(0);
        let content = self.content.borrow();

        let heard_line = if content.heard_pending {
            ellipsis_frame(elapsed_ms).to_string()
        } else {
            format!("«{}»", head(&content.heard, CHARS_PER_LINE))
        };
        self.heard_label.setStringValue(&NSString::from_str(&heard_line));

        if self.dictating.get() {
            let text = self.last_dictation.borrow();
            let lines = wrapped_lines(&text, CHARS_PER_LINE, 3);
            let shown = tail(&text, CHARS_PER_LINE * lines);
            self.outcome_label.setStringValue(&NSString::from_str(&shown));
        } else {
            let outcome_line = if content.outcome_pending {
                ellipsis_frame(elapsed_ms).to_string()
            } else {
                head(&content.outcome, CHARS_PER_LINE)
            };
            self.outcome_label.setStringValue(&NSString::from_str(&outcome_line));
        }
    }
}

/// Top-right of the main screen, clear of the menu bar.
fn position(window: &NSPanel, mtm: MainThreadMarker) {
    let Some(screen) = NSScreen::mainScreen(mtm) else {
        return;
    };
    let visible = screen.visibleFrame();
    let origin = NSPoint::new(
        visible.origin.x + visible.size.width - WIDTH - MARGIN,
        visible.origin.y + visible.size.height - HEIGHT - MARGIN,
    );
    window.setFrameOrigin(origin);
}

/// "Muestra lo que oyes" / "esconde lo que oyes" — set from
/// [`crate::commands::run`] when one of those runs, which may be the
/// listening thread; read from the run loop timer on the main thread.
static REQUEST: Mutex<Option<bool>> = Mutex::new(None);

/// Asks the HUD to show (`true`) or hide (`false`) itself, from the named
/// actions `hud:show` / `hud:hide` in `vocabulary.rs`.
pub fn request(show: bool) {
    if let Ok(mut slot) = REQUEST.lock() {
        *slot = Some(show);
    }
}

fn take_request() -> Option<bool> {
    REQUEST.lock().ok().and_then(|mut slot| slot.take())
}

/// Raised by the listening loop the moment Silero opens an utterance —
/// before there is a transcript, let alone a decision — and cleared by
/// [`Hud::tick`] once it has shown the panel and reset both lines to
/// pending. A plain flag rather than a timestamp: [`Hud::tick`] stamps
/// `pending_since` itself, on the main thread's own clock.
static SPEECH_OPEN: Mutex<bool> = Mutex::new(false);

/// Tells the HUD an utterance just opened — call this once per utterance,
/// as soon as [`crate::audio::Listener::speech_open`] turns true.
pub fn note_speech_open() {
    if let Ok(mut slot) = SPEECH_OPEN.lock() {
        *slot = true;
    }
}

fn take_speech_open() -> bool {
    SPEECH_OPEN.lock().map(|mut slot| std::mem::take(&mut *slot)).unwrap_or(false)
}

/// The transcript and/or the decision, queued here by the listening loop
/// rather than parsed back out of the tooltip — see [`Update`]'s doc.
/// Two pushes between ticks merge (each field keeps its latest `Some`)
/// instead of the second overwriting the first outright, so a fast
/// transcript-then-outcome pair is never lost to an unlucky tick.
static UPDATE: Mutex<Option<Update>> = Mutex::new(None);

/// Tells the HUD what was just found out — the transcript, the decision,
/// or both. Call with only the field that is known; the other stays as it
/// was (or pending, if nothing has supplied it yet).
pub fn push_update(update: Update) {
    let Ok(mut slot) = UPDATE.lock() else { return };
    match slot.as_mut() {
        Some(existing) => {
            if update.heard.is_some() {
                existing.heard = update.heard;
            }
            if update.outcome.is_some() {
                existing.outcome = update.outcome;
            }
        }
        None => *slot = Some(update),
    }
}

fn take_update() -> Option<Update> {
    UPDATE.lock().ok().and_then(|mut slot| slot.take())
}

/// The dictated text so far, for the HUD's second line.
///
/// Not part of the tooltip's format, unlike everything else the HUD shows
/// — see the module doc. **Wiring note for the listening loop:** call
/// `hud::set_dictation_text(&typed_so_far)` in `main.rs`'s `Outcome::Type`
/// arm (and `EditDictation`'s), right after the text reaches the keyboard,
/// with whatever `dictation::Transformer` has produced for this session so
/// far — the same value the undo/repeat bookkeeping already tracks by
/// length. Nothing calls it yet; until it does, the HUD shows an empty
/// second line while dictating.
static DICTATION_TEXT: OnceLock<Mutex<String>> = OnceLock::new();

pub fn set_dictation_text(text: &str) {
    let cell = DICTATION_TEXT.get_or_init(|| Mutex::new(String::new()));
    if let Ok(mut slot) = cell.lock() {
        *slot = text.to_string();
    }
}

fn dictation_text() -> String {
    DICTATION_TEXT
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .map(|slot| slot.clone())
        .unwrap_or_default()
}

/// Whether a "¿Querías decir…?" question is waiting for an answer.
///
/// **Wiring note for the listening loop:** call
/// `hud::set_question_pending(true)` right after
/// `session.open_question(...)` in `main.rs`, and `set_question_pending(false)`
/// wherever the question closes — `session.answer_question` returning
/// `Some(_)`, or `session.question_timed_out` returning `Some(_)`.
static QUESTION_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_question_pending(pending: bool) {
    QUESTION_PENDING.store(pending, std::sync::atomic::Ordering::Relaxed);
}

fn question_pending() -> bool {
    QUESTION_PENDING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the AI layer is being waited on right now.
///
/// **Wiring note for the listening loop:** call `hud::set_busy(true)`
/// before `ai::ask` in `main.rs`'s `Outcome::AskAi` arm, and
/// `hud::set_busy(false)` right after it returns.
static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_busy(busy: bool) {
    BUSY.store(busy, std::sync::atomic::Ordering::Relaxed);
}

fn busy() -> bool {
    BUSY.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(seconds: u64) -> Instant {
        // A fixed base moved forward, rather than `Instant::now()` at every
        // call site: the tests read as a timeline this way.
        Instant::now() + Duration::from_secs(seconds)
    }

    #[test]
    fn hidden_until_something_happens() {
        let state = Visibility::with_hud_seconds(HUD_SECONDS);
        assert!(!state.visible(t(0)));
    }

    #[test]
    fn showing_after_being_heard_and_hiding_again_later() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        let heard_at = t(0);
        state.note_heard(heard_at);
        assert!(state.visible(heard_at));
        assert!(state.visible(heard_at + Duration::from_secs_f64(HUD_SECONDS - 0.1)));
        assert!(!state.visible(heard_at + Duration::from_secs_f64(HUD_SECONDS + 0.1)));
    }

    #[test]
    fn stays_open_while_dictating_no_matter_how_long() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.note_heard(t(0));
        state.set_dictating(true);
        assert!(state.visible(t(1000)));
        state.set_dictating(false);
        assert!(!state.visible(t(1000)));
    }

    #[test]
    fn stays_open_while_the_ai_layer_is_busy() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.note_heard(t(0));
        state.set_busy(true);
        assert!(state.visible(t(1000)));
        state.set_busy(false);
        assert!(!state.visible(t(1000)));
    }

    #[test]
    fn stays_open_while_a_question_is_pending() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.note_heard(t(0));
        state.set_question_pending(true);
        assert!(state.visible(t(1000)));
        state.set_question_pending(false);
        assert!(!state.visible(t(1000)));
    }

    #[test]
    fn pinned_from_settings_never_hides() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.set_pinned(true);
        assert!(state.visible(t(0)));
        assert!(state.visible(t(100_000)));
    }

    #[test]
    fn hide_command_overrides_recent_activity() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.note_heard(t(0));
        state.hide_forced();
        assert!(!state.visible(t(0)));
    }

    #[test]
    fn hide_command_does_not_survive_the_next_thing_heard() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.hide_forced();
        state.note_heard(t(1));
        assert!(state.visible(t(1)));
    }

    #[test]
    fn show_command_behaves_like_something_was_heard() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.show_forced(t(0));
        assert!(state.visible(t(0)));
        assert!(!state.visible(t(HUD_SECONDS as u64 + 1)));
    }

    #[test]
    fn hide_command_does_not_defeat_being_pinned() {
        let mut state = Visibility::with_hud_seconds(HUD_SECONDS);
        state.set_pinned(true);
        state.hide_forced();
        assert!(state.visible(t(0)));
    }

    #[test]
    fn short_text_is_kept_whole() {
        assert_eq!(head("abrir Chrome", 40), "abrir Chrome");
        assert_eq!(tail("abrir Chrome", 40), "abrir Chrome");
    }

    #[test]
    fn head_cuts_the_end_and_marks_it() {
        assert_eq!(head("una frase muy muy larga de verdad", 10), "una frase…");
    }

    #[test]
    fn tail_cuts_the_start_and_marks_it() {
        let cut = tail("una frase muy muy larga de verdad", 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.starts_with('…'));
        assert!(cut.ends_with("verdad"));
    }

    #[test]
    fn wrapping_grows_with_text_and_stops_at_the_cap() {
        assert_eq!(wrapped_lines("", 10, 3), 1);
        assert_eq!(wrapped_lines("diez letras", 10, 3), 2);
        assert_eq!(wrapped_lines(&"a".repeat(1000), 10, 3), 3);
    }

    #[test]
    fn the_dictation_wiring_points_are_readable_before_anything_writes_to_them() {
        // Nothing has called `set_dictation_text` or `set_question_pending`
        // yet in this process — a fresh reader must still get a sane
        // default rather than panicking on an uninitialised lock.
        assert!(!question_pending());
        // dictation_text() is exercised indirectly via Hud::repaint in
        // manual testing (it needs a MainThreadMarker); its default is
        // covered by set_dictation_text/dictation_text round-tripping.
        set_dictation_text("hola");
        assert_eq!(dictation_text(), "hola");
    }

    #[test]
    fn speech_opening_puts_both_lines_back_to_pending() {
        let mut content = Content::default();
        content.push_update(Update {
            heard: Some("abre cromo".to_string()),
            outcome: Some("abrir Chrome".to_string()),
        });
        assert!(!content.heard_pending);
        assert!(!content.outcome_pending);

        content.note_speech_open();
        assert!(content.heard_pending);
        assert!(content.outcome_pending);
        assert_eq!(content.heard, "");
        assert_eq!(content.outcome, "");
    }

    #[test]
    fn transcript_fills_the_first_line_while_the_second_stays_pending() {
        let mut content = Content::default();
        content.note_speech_open();

        content.push_update(Update { heard: Some("minion, abre cromo".to_string()), outcome: None });
        assert!(!content.heard_pending);
        assert_eq!(content.heard, "minion, abre cromo");
        assert!(content.outcome_pending, "the decision has not arrived yet");
    }

    #[test]
    fn the_decision_fills_the_second_line_without_touching_the_first() {
        let mut content = Content::default();
        content.note_speech_open();
        content.push_update(Update { heard: Some("minion, abre cromo".to_string()), outcome: None });

        content.push_update(Update { heard: None, outcome: Some("abrir Chrome".to_string()) });
        assert_eq!(content.heard, "minion, abre cromo");
        assert!(!content.outcome_pending);
        assert_eq!(content.outcome, "abrir Chrome");
    }

    #[test]
    fn ellipsis_cycles_every_300_ms() {
        assert_eq!(ellipsis_frame(0), "·");
        assert_eq!(ellipsis_frame(299), "·");
        assert_eq!(ellipsis_frame(300), "··");
        assert_eq!(ellipsis_frame(599), "··");
        assert_eq!(ellipsis_frame(600), "···");
        assert_eq!(ellipsis_frame(899), "···");
        // And back to the first frame — it cycles, it does not stop.
        assert_eq!(ellipsis_frame(900), "·");
    }

    #[test]
    fn the_speech_open_and_update_wiring_points_round_trip() {
        // Mirrors `the_dictation_wiring_points_are_readable_before_anything_
        // writes_to_them` above: a fresh reader must get a sane default,
        // and what is pushed is what comes back out exactly once.
        assert!(!take_speech_open());
        note_speech_open();
        assert!(take_speech_open());
        assert!(!take_speech_open(), "cleared once read");

        assert!(take_update().is_none());
        push_update(Update { heard: Some("hola".to_string()), outcome: None });
        push_update(Update { heard: None, outcome: Some("saludo".to_string()) });
        let merged = take_update().expect("two pushes between ticks must merge");
        assert_eq!(merged.heard.as_deref(), Some("hola"));
        assert_eq!(merged.outcome.as_deref(), Some("saludo"));
        assert!(take_update().is_none(), "cleared once read");
    }

    #[test]
    fn every_face_renders() {
        for face in [
            Face::Awake,
            Face::Asleep,
            Face::Acting,
            Face::Dictating,
            Face::Thinking,
            Face::Speaking,
        ] {
            assert!(render_face(face.svg()).is_some(), "{face:?} should render");
        }
    }
}

//! The first-run assistant.
//!
//! Replaces the old single dialog ("Minion escucha por el micrófono…") with
//! a small window that walks through the same things a person would
//! otherwise have to discover by reading `CLAUDE.md`: that the microphone
//! and Accessibility need permission, that the model takes a few minutes on
//! first run, what the wake word is, that a voice profile is optional, and
//! a way to prove it all works before closing the window.
//!
//! Split the way `session.rs` is: [`Wizard`] is the state machine — which
//! step, whether it can move on, whether the voice step was skipped or the
//! test passed — pure and unit-tested; [`Window`] is the AppKit shell that
//! reads it and paints it. `Window` has no logic of its own worth testing:
//! every branch it takes was already decided by `Wizard`.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFont, NSLineBreakMode, NSTextField, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::Training;

/// One page of the assistant, in the order it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Microphone,
    Accessibility,
    Model,
    WakeWord,
    Voice,
    Test,
    Done,
}

impl Step {
    const ALL: [Step; 8] = [
        Step::Welcome,
        Step::Microphone,
        Step::Accessibility,
        Step::Model,
        Step::WakeWord,
        Step::Voice,
        Step::Test,
        Step::Done,
    ];

    /// Position in the sequence, starting at 1 — what the progress dots
    /// count against.
    pub fn number(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0) + 1
    }

    fn next(self) -> Option<Step> {
        Self::ALL.get(self.number()).copied()
    }

    fn previous(self) -> Option<Step> {
        self.number().checked_sub(2).and_then(|i| Self::ALL.get(i)).copied()
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Bienvenida",
            Step::Microphone => "Micrófono",
            Step::Accessibility => "Accesibilidad",
            Step::Model => "Modelo",
            Step::WakeWord => "Palabra clave",
            Step::Voice => "Tu voz",
            Step::Test => "Prueba",
            Step::Done => "Todo listo",
        }
    }
}

/// Live facts the wizard cannot know on its own — read from the system or
/// from the running process — that decide whether "Siguiente" is allowed.
///
/// Only the model gates progress: a denied microphone or Accessibility
/// permission is shown, with a way to fix it, but does not trap someone on
/// that page — they may be here to read ahead, or fix it afterwards. The
/// model is different: nothing past it (the wake word aside) can be tried
/// without it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gate {
    pub model_ready: bool,
}

/// The assistant's state: which page, and what has happened on the pages
/// that remember something between visits.
pub struct Wizard {
    step: Step,
    voice_skipped: bool,
    test_passed: bool,
}

impl Wizard {
    pub fn new() -> Self {
        Self { step: Step::Welcome, voice_skipped: false, test_passed: false }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// (this page's number, how many pages there are) — what the progress
    /// dots draw.
    pub fn progress(&self) -> (usize, usize) {
        (self.step.number(), Step::ALL.len())
    }

    pub fn can_go_back(&self) -> bool {
        self.step.previous().is_some()
    }

    /// Whether "Siguiente" does anything right now.
    pub fn can_advance(&self, gate: Gate) -> bool {
        match self.step {
            Step::Model => gate.model_ready,
            Step::Done => false,
            _ => true,
        }
    }

    /// Moves to the next page. Returns whether it moved.
    pub fn advance(&mut self, gate: Gate) -> bool {
        if !self.can_advance(gate) {
            return false;
        }
        match self.step.next() {
            Some(next) => {
                self.step = next;
                true
            }
            None => false,
        }
    }

    /// Moves back one page. Returns whether it moved.
    pub fn back(&mut self) -> bool {
        match self.step.previous() {
            Some(previous) => {
                self.step = previous;
                true
            }
            None => false,
        }
    }

    /// "Saltar", on the voice page only.
    pub fn skip_voice(&mut self) {
        if self.step == Step::Voice {
            self.voice_skipped = true;
            self.advance(Gate::default());
        }
    }

    pub fn voice_skipped(&self) -> bool {
        self.voice_skipped
    }

    /// Recorded once the test page hears an `Answer` outcome. Never
    /// cleared by navigation — going back and forward again should not
    /// make the ✓ disappear.
    pub fn mark_test_passed(&mut self) {
        self.test_passed = true;
    }

    pub fn test_passed(&self) -> bool {
        self.test_passed
    }

    /// Back to the first page, everything forgotten — what "Asistente…"
    /// from the menu opens into, rather than wherever a previous run left
    /// off.
    pub fn restart(&mut self) {
        *self = Self::new();
    }
}

impl Default for Wizard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------
// The window. Everything below reads `Wizard` and paints it; none of it
// decides anything `Wizard` did not already decide.
// ---------------------------------------------------------------------

const WIDTH: f64 = 440.0;
const HEIGHT: f64 = 360.0;
const EDGE: f64 = 24.0;

/// A push button and the state it had when it was last read.
///
/// Same reasoning as `preferences::Press`: AppKit only counts clicks for a
/// button with a target, and this window has none, so a click shows up as
/// the button's own state changing between two polls.
struct Press {
    control: Retained<NSButton>,
    last: Cell<isize>,
}

impl Press {
    fn new(control: Retained<NSButton>) -> Self {
        let last = Cell::new(control.state());
        Self { control, last }
    }

    fn clicked(&self) -> bool {
        let now = self.control.state();
        now != self.last.replace(now) && now != 0
    }
}

fn frame(top: f64, height: f64, x: f64, width: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, HEIGHT - top - height), NSSize::new(width, height))
}

fn wrapping_label(mtm: MainThreadMarker, frame: NSRect, big: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    field.setFrame(frame);
    field.setUsesSingleLineMode(false);
    field.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    field.setMaximumNumberOfLines(0);
    if big {
        field.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    } else {
        field.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    field
}

fn button(mtm: MainThreadMarker, title: &str, frame: NSRect) -> Retained<NSButton> {
    // Safety: no target and no action — see `Press` above.
    let control = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    control.setFrame(frame);
    control
}

fn set_text(field: &NSTextField, text: &str) {
    field.setStringValue(&NSString::from_str(text));
}

/// Where "Abrir Ajustes" goes for a microphone Minion cannot use — the
/// same pane `main.rs` opens after `SilenceWatch` proves the permission
/// was denied.
const MICROPHONE_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

fn open_url(url: &str) {
    let _ = std::process::Command::new("/usr/bin/open").arg(url).status();
}

pub struct Window {
    window: Retained<NSWindow>,
    title: Retained<NSTextField>,
    body: Retained<NSTextField>,
    status_line: Retained<NSTextField>,
    dots: Retained<NSTextField>,
    wake_word: Retained<NSTextField>,
    left: Retained<NSButton>,
    left_press: Press,
    right: Retained<NSButton>,
    right_press: Press,
    back: Retained<NSButton>,
    back_press: Press,
    next: Retained<NSButton>,
    next_press: Press,

    wizard: RefCell<Wizard>,
    last_wake_word: RefCell<String>,
    voice_started: Cell<bool>,

    // Shared with the rest of the program — see `main.rs` for who writes
    // these.
    status: Arc<Mutex<String>>,
    downloading: Arc<AtomicBool>,
    training: Training,
    model_path: String,
    /// Raised once by the listening loop when it produces an `Answer`
    /// outcome — the test page's ✓. Consumed here, so a second visit to
    /// the test page needs a fresh one.
    answered: Arc<AtomicBool>,
    /// Raised, and never lowered, once the microphone is proven to
    /// deliver only silence — see `audio::SilenceWatch`.
    mic_denied: Arc<AtomicBool>,
    /// Opens the real preferences window — "Abrir Ajustes" on the last
    /// page. The menu bar's timer is what actually shows it.
    open_settings: Arc<AtomicBool>,
    /// "Ver qué puedo decirle" on the last page.
    open_catalogue: Arc<AtomicBool>,
}

/// Everything `Window` reads or writes that belongs to the rest of the
/// program rather than to the window itself — bundled into one value so
/// the constructor takes one argument instead of one per field.
pub struct Shared {
    pub status: Arc<Mutex<String>>,
    pub downloading: Arc<AtomicBool>,
    pub training: Training,
    pub model_path: String,
    /// Raised once for every `Outcome::Answer` — the "Prueba" page's ✓.
    pub answered: Arc<AtomicBool>,
    /// Raised, and never lowered, once the microphone is proven to
    /// deliver only silence.
    pub mic_denied: Arc<AtomicBool>,
    /// Opens the real preferences window — "Abrir Ajustes" on the last
    /// page. The menu bar's timer is what actually shows it.
    pub open_settings: Arc<AtomicBool>,
    /// "Ver qué puedo decirle" on the last page.
    pub open_catalogue: Arc<AtomicBool>,
}

impl Window {
    pub fn new(mtm: MainThreadMarker, shared: Shared) -> Self {
        let Shared {
            status,
            downloading,
            training,
            model_path,
            answered,
            mic_denied,
            open_settings,
            open_catalogue,
        } = shared;
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("Asistente de Minion"));
        // Kept alive by this struct for the life of the process — closing
        // it must not release it, or reopening from the menu would use
        // freed memory. Same reasoning as `preferences::Preferences`.
        unsafe { window.setReleasedWhenClosed(false) };
        window.center();

        let title = wrapping_label(mtm, frame(20.0, 24.0, EDGE, WIDTH - EDGE * 2.0), true);
        title.setFont(Some(&NSFont::systemFontOfSize(18.0)));
        let body = wrapping_label(mtm, frame(54.0, 90.0, EDGE, WIDTH - EDGE * 2.0), true);
        let status_line = wrapping_label(mtm, frame(150.0, 40.0, EDGE, WIDTH - EDGE * 2.0), false);

        let wake_word = NSTextField::new(mtm);
        wake_word.setFrame(frame(196.0, 24.0, EDGE, 200.0));
        wake_word.setHidden(true);

        let dots = wrapping_label(mtm, frame(238.0, 18.0, EDGE, WIDTH - EDGE * 2.0), false);
        dots.setAlignment(objc2_app_kit::NSTextAlignment::Center);

        let left = button(mtm, "", frame(266.0, 26.0, EDGE, 190.0));
        left.setHidden(true);
        let right = button(mtm, "", frame(266.0, 26.0, WIDTH - EDGE - 190.0, 190.0));
        right.setHidden(true);

        let back = button(mtm, "Atrás", frame(310.0, 26.0, EDGE, 90.0));
        let next = button(mtm, "Siguiente", frame(310.0, 26.0, WIDTH - EDGE - 110.0, 110.0));

        if let Some(content) = window.contentView() {
            content.addSubview(&title);
            content.addSubview(&body);
            content.addSubview(&status_line);
            content.addSubview(&wake_word);
            content.addSubview(&dots);
            content.addSubview(&left);
            content.addSubview(&right);
            content.addSubview(&back);
            content.addSubview(&next);
        }

        let current_wake = crate::config::load()
            .wake_words
            .first()
            .cloned()
            .unwrap_or_else(|| crate::commands::DEFAULT_WAKE_WORDS[0].to_string());
        wake_word.setStringValue(&NSString::from_str(&current_wake));

        let win = Self {
            window,
            title,
            body,
            status_line,
            dots,
            wake_word,
            left_press: Press::new(left.clone()),
            left,
            right_press: Press::new(right.clone()),
            right,
            back_press: Press::new(back.clone()),
            back,
            next_press: Press::new(next.clone()),
            next,
            wizard: RefCell::new(Wizard::new()),
            last_wake_word: RefCell::new(current_wake),
            voice_started: Cell::new(false),
            status,
            downloading,
            training,
            model_path,
            answered,
            mic_denied,
            open_settings,
            open_catalogue,
        };
        win.render();
        win
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Brings the window forward, starting over from the welcome page —
    /// reopening from the menu is "show me again", not "where I left
    /// off".
    pub fn show(&self) {
        self.wizard.borrow_mut().restart();
        self.voice_started.set(false);
        self.render();
        if let Some(mtm) = MainThreadMarker::new() {
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    fn gate(&self) -> Gate {
        Gate { model_ready: !self.downloading.load(Ordering::Relaxed) }
    }

    /// Reads every control and the shared state it depends on, and acts
    /// on anything that changed. Called from the menu bar's run loop
    /// timer, same as `preferences::Preferences::poll`.
    pub fn poll(&self) {
        let step = self.wizard.borrow().step();

        // The wake word: committed once the field stops being edited,
        // same rule as the preferences window and for the same reason —
        // saving on every keystroke would put up a restart dialog for
        // each letter typed.
        if step == Step::WakeWord {
            let typed = self.wake_word.stringValue().to_string();
            let settled = self.wake_word.currentEditor().is_none() || !self.window.isKeyWindow();
            let changed = typed.trim() != self.last_wake_word.borrow().trim();
            if settled && changed && !typed.trim().is_empty() {
                let word = crate::text::normalise(&typed);
                let result = if word == crate::commands::DEFAULT_WAKE_WORDS[0] {
                    crate::config::set_option("wake_words", "[]")
                } else {
                    crate::config::set_option(
                        "wake_words",
                        &format!("[{}]", crate::config::toml_string(&word)),
                    )
                };
                if let Err(e) = result {
                    crate::journal::write(&format!("onboarding: could not save the wake word: {e}"));
                }
                *self.last_wake_word.borrow_mut() = typed;
            }
        }

        if step == Step::Voice {
            if self.left_press.clicked() && !self.voice_started.get() {
                if let Ok(mut session) = self.training.lock() {
                    if session.is_none() {
                        *session = Some(crate::enroll::Session::starting(self.model_path.clone()));
                        self.voice_started.set(true);
                    }
                }
            }
            if self.right_press.clicked() {
                // Forgets an enrolment in progress, too — "Saltar" means
                // stop asking, not keep listening for sentences in the
                // background.
                if let Ok(mut session) = self.training.lock() {
                    *session = None;
                }
                self.voice_started.set(false);
                self.wizard.borrow_mut().skip_voice();
            }
        }

        if step == Step::Test && self.answered.swap(false, Ordering::Relaxed) {
            self.wizard.borrow_mut().mark_test_passed();
        }

        if step == Step::Microphone && self.left_press.clicked() {
            open_url(MICROPHONE_SETTINGS);
        }
        if step == Step::Accessibility && self.left_press.clicked() {
            let _ = crate::actions::open_accessibility_settings();
        }
        if step == Step::Done {
            if self.left_press.clicked() {
                self.open_settings.store(true, Ordering::Relaxed);
            }
            if self.right_press.clicked() {
                self.open_catalogue.store(true, Ordering::Relaxed);
            }
        }

        if self.back_press.clicked() {
            self.wizard.borrow_mut().back();
        }
        if self.next_press.clicked() {
            if step == Step::Done {
                // Nothing left to move on to — "Siguiente" became "Cerrar".
                self.window.orderOut(None);
            } else {
                let gate = self.gate();
                self.wizard.borrow_mut().advance(gate);
            }
        }

        self.render();
    }

    /// Paints whatever page `wizard` is on right now. Idempotent — safe
    /// to call every tick, which is what lets a live status (the download
    /// percentage, a permission granted mid-page) show up without a
    /// click.
    fn render(&self) {
        let wizard = self.wizard.borrow();
        let step = wizard.step();
        let gate = self.gate();

        set_text(&self.title, step.title());
        let (page, total) = wizard.progress();
        let dots: String = (1..=total)
            .map(|n| if n == page { "●" } else { "○" })
            .collect::<Vec<&str>>()
            .join(" ");
        set_text(&self.dots, &dots);

        self.wake_word.setHidden(step != Step::WakeWord);
        self.left.setHidden(true);
        self.right.setHidden(true);
        self.status_line.setStringValue(&NSString::from_str(""));

        match step {
            Step::Welcome => {
                set_text(
                    &self.body,
                    "Minion escucha por el micrófono todo el rato y solo hace \
                     caso cuando empiezas diciendo su nombre. Todo el \
                     reconocimiento ocurre en este Mac: nada de lo que dices \
                     sale de aquí.\n\nEsto solo tardará un minuto.",
                );
            }
            Step::Microphone => {
                set_text(
                    &self.body,
                    "Minion necesita permiso para escuchar el micrófono. Sin \
                     él, verá que escucha pero no oirá nada.",
                );
                if self.mic_denied.load(Ordering::Relaxed) {
                    set_text(&self.status_line, "Denegado.");
                    self.left.setTitle(&NSString::from_str("Abrir Ajustes"));
                    self.left.setHidden(false);
                } else {
                    set_text(&self.status_line, "Concedido (o aún no comprobado).");
                }
            }
            Step::Accessibility => {
                set_text(
                    &self.body,
                    "Y permiso de Accesibilidad, para las órdenes que \
                     pulsan una tecla: copiar, guardar, cerrar una pestaña.",
                );
                if crate::actions::has_accessibility_permission() {
                    set_text(&self.status_line, "Concedido.");
                } else {
                    set_text(&self.status_line, "Denegado.");
                    self.left.setTitle(&NSString::from_str("Abrir Ajustes"));
                    self.left.setHidden(false);
                }
            }
            Step::Model => {
                set_text(
                    &self.body,
                    "El modelo de reconocimiento (unos 670 MB) se descarga \
                     una sola vez.",
                );
                if gate.model_ready {
                    set_text(&self.status_line, "Listo.");
                } else {
                    let progress =
                        self.status.lock().map(|s| s.clone()).unwrap_or_default();
                    set_text(&self.status_line, &progress);
                }
            }
            Step::WakeWord => {
                set_text(
                    &self.body,
                    "Toda orden empieza por esta palabra. El valor por \
                     defecto es «minion»; cámbiala por algo que no digas por \
                     casualidad.",
                );
            }
            Step::Voice => {
                set_text(
                    &self.body,
                    "Opcional: si Minion aprende tu voz, dejará de hacer \
                     caso a cualquier otra. Son cinco frases cortas.",
                );
                let training_message =
                    self.training.lock().ok().and_then(|s| s.as_ref().map(|s| s.message.clone()));
                match training_message {
                    Some(message) => set_text(&self.status_line, &message),
                    None => {
                        self.left.setTitle(&NSString::from_str("Empezar"));
                        self.left.setHidden(false);
                    }
                }
                self.right.setTitle(&NSString::from_str("Saltar"));
                self.right.setHidden(false);
            }
            Step::Test => {
                set_text(&self.body, "Di: «minion, ¿qué hora es?»");
                if wizard.test_passed() {
                    set_text(&self.status_line, "✓ Te ha oído.");
                } else {
                    set_text(&self.status_line, "Esperando…");
                }
            }
            Step::Done => {
                let voice_note = if wizard.voice_skipped() {
                    " Cuando quieras enseñarle tu voz, hazlo desde «Ajustes…» \
                     → «Entrenar mi voz»."
                } else {
                    ""
                };
                set_text(
                    &self.body,
                    &format!(
                        "Minion vive en la barra de menús. Desde ahí se pausa, \
                         se aprenden alias y se cambian los ajustes.{voice_note}"
                    ),
                );
                self.left.setTitle(&NSString::from_str("Abrir Ajustes"));
                self.left.setHidden(false);
                self.right.setTitle(&NSString::from_str("Ver qué puedo decirle"));
                self.right.setHidden(false);
            }
        }

        self.back.setEnabled(wizard.can_go_back());
        self.next.setEnabled(step == Step::Done || wizard.can_advance(gate));
        self.next.setTitle(&NSString::from_str(if step == Step::Done {
            "Cerrar"
        } else {
            "Siguiente"
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Gate {
        Gate { model_ready: true }
    }

    #[test]
    fn starts_at_the_welcome_page() {
        let wizard = Wizard::new();
        assert_eq!(wizard.step(), Step::Welcome);
        assert_eq!(wizard.progress(), (1, 8));
        assert!(!wizard.can_go_back());
    }

    #[test]
    fn walks_through_every_page_in_order() {
        let mut wizard = Wizard::new();
        let order = [
            Step::Microphone,
            Step::Accessibility,
            Step::Model,
            Step::WakeWord,
            Step::Voice,
            Step::Test,
            Step::Done,
        ];
        for expected in order {
            assert!(wizard.advance(ready()));
            assert_eq!(wizard.step(), expected);
        }
        // Nothing past the last page.
        assert!(!wizard.advance(ready()));
        assert_eq!(wizard.step(), Step::Done);
    }

    #[test]
    fn the_model_page_blocks_until_it_is_ready() {
        let mut wizard = Wizard::new();
        wizard.advance(ready()); // Microphone
        wizard.advance(ready()); // Accessibility
        wizard.advance(ready()); // Model
        assert_eq!(wizard.step(), Step::Model);

        let not_yet = Gate { model_ready: false };
        assert!(!wizard.can_advance(not_yet));
        assert!(!wizard.advance(not_yet));
        assert_eq!(wizard.step(), Step::Model, "still waiting for the model");

        assert!(wizard.advance(ready()));
        assert_eq!(wizard.step(), Step::WakeWord);
    }

    #[test]
    fn a_denied_permission_does_not_block_the_page_about_it() {
        // Deliberately not part of Gate: shown, with a way to fix it, but
        // never a wall someone is stuck behind.
        let mut wizard = Wizard::new();
        assert!(wizard.advance(ready())); // Microphone
        assert!(wizard.advance(ready())); // Accessibility, regardless of
                                           // either permission's state.
        assert_eq!(wizard.step(), Step::Accessibility);
    }

    #[test]
    fn back_and_forth_does_not_lose_the_page() {
        let mut wizard = Wizard::new();
        wizard.advance(ready());
        wizard.advance(ready());
        assert_eq!(wizard.step(), Step::Accessibility);
        assert!(wizard.back());
        assert_eq!(wizard.step(), Step::Microphone);
        assert!(wizard.back());
        assert_eq!(wizard.step(), Step::Welcome);
        assert!(!wizard.back(), "nothing before the first page");
    }

    #[test]
    fn skipping_voice_moves_on_and_remembers_it_was_skipped() {
        let mut wizard = Wizard::new();
        for _ in 0..5 {
            wizard.advance(ready());
        }
        assert_eq!(wizard.step(), Step::Voice);
        wizard.skip_voice();
        assert_eq!(wizard.step(), Step::Test);
        assert!(wizard.voice_skipped());
    }

    #[test]
    fn skip_voice_does_nothing_on_another_page() {
        let mut wizard = Wizard::new();
        wizard.skip_voice();
        assert_eq!(wizard.step(), Step::Welcome, "not the voice page yet");
        assert!(!wizard.voice_skipped());
    }

    #[test]
    fn the_test_result_survives_navigation() {
        let mut wizard = Wizard::new();
        for _ in 0..6 {
            wizard.advance(ready());
        }
        assert_eq!(wizard.step(), Step::Test);
        assert!(!wizard.test_passed());
        wizard.mark_test_passed();
        assert!(wizard.test_passed());
        wizard.back();
        wizard.advance(ready());
        assert!(wizard.test_passed(), "going back and forward keeps the ✓");
    }

    #[test]
    fn restart_forgets_everything() {
        let mut wizard = Wizard::new();
        for _ in 0..7 {
            wizard.advance(ready());
        }
        wizard.mark_test_passed();
        assert_eq!(wizard.step(), Step::Done);
        wizard.restart();
        assert_eq!(wizard.step(), Step::Welcome);
        assert!(!wizard.test_passed());
        assert!(!wizard.voice_skipped());
    }

    #[test]
    fn every_page_has_its_own_title() {
        let mut seen = std::collections::HashSet::new();
        for step in Step::ALL {
            assert!(seen.insert(step.title()), "«{}» repeats a title", step.title());
        }
    }
}

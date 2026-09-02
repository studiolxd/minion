//! The preferences window.
//!
//! Built with AppKit directly rather than a UI framework: eight settings do
//! not justify shipping one, and this way the window looks like every other
//! window on the machine.
//!
//! Controls report changes by being read, not by calling back. AppKit
//! delivers actions to an Objective-C target, which from Rust means
//! declaring a class — the most delicate part of the objc2 bridge, for a
//! panel that changes at human speed. Instead the run loop timer that
//! already repaints the menu bar reads the controls a few times a second
//! and writes through anything that moved. It also means the window follows
//! along when a setting changes elsewhere, which a callback would not.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSFont, NSSlider, NSTextField, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::{config, startup};

const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 470.0;
const MARGIN: f64 = 22.0;

/// A slider's range and the setting behind it.
struct Dial {
    control: Retained<NSSlider>,
    readout: Retained<NSTextField>,
    last: Cell<f64>,
}

impl Dial {
    /// Reads the control, and reports the new value if it moved.
    fn moved(&self) -> Option<f64> {
        let now = self.control.doubleValue();
        (now - self.last.get()).abs().gt(&f64::EPSILON).then(|| {
            self.last.set(now);
            now
        })
    }
}

/// A checkbox and the value it last had.
struct Switch {
    control: Retained<NSButton>,
    last: Cell<bool>,
}

impl Switch {
    fn on(&self) -> bool {
        let state = self.control.state();
        state == 1
    }

    fn toggled(&self) -> Option<bool> {
        let now = self.on();
        (now != self.last.get()).then(|| {
            self.last.set(now);
            now
        })
    }

    fn show(&self, on: bool) {
        self.control.setState(isize::from(on));
        self.last.set(on);
    }
}

pub struct Preferences {
    window: Retained<NSWindow>,
    sounds: Switch,
    log_voices: Switch,
    at_login: Switch,
    sensitivity: Dial,
    pause: Dial,
    memory: Dial,
    shortcut: Retained<NSButton>,
    /// The shortcut as stored, e.g. "alt-space".
    shortcut_value: std::cell::RefCell<String>,
    /// True while waiting for the user to press a combination.
    capturing: Cell<bool>,
    /// The button's state last time it was read, to notice a click without
    /// an Objective-C target — see the note at the top of this file.
    button_clicks: Cell<isize>,
}

/// A shortcut written the way macOS shows it: ⌥Space, ⇧⌘B.
fn pretty(shortcut: &str) -> String {
    let Some((code, mods)) = crate::actions::parse_shortcut(shortcut) else {
        return "Ninguno".into();
    };
    let mut out = String::new();
    if mods.control {
        out.push('⌃');
    }
    if mods.option {
        out.push('⌥');
    }
    if mods.shift {
        out.push('⇧');
    }
    if mods.command {
        out.push('⌘');
    }
    let key = crate::actions::name_of_key(code).unwrap_or("?");
    out.push_str(&match key {
        "space" => "Espacio".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "escape" => "Esc".to_string(),
        "return" | "enter" => "↩".to_string(),
        "tab" => "⇥".to_string(),
        "delete" | "backspace" => "⌫".to_string(),
        other => other.to_uppercase(),
    });
    out
}

fn label(mtm: MainThreadMarker, text: &str, frame: NSRect, small: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFrame(frame);
    if small {
        field.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    field
}

fn checkbox(mtm: MainThreadMarker, title: &str, y: f64, on: bool) -> Switch {
    let button = unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    button.setFrame(NSRect::new(
        NSPoint::new(MARGIN, y),
        NSSize::new(WIDTH - MARGIN * 2.0, 20.0),
    ));
    let switch = Switch { control: button, last: Cell::new(on) };
    switch.show(on);
    switch
}

fn slider(mtm: MainThreadMarker, y: f64, range: (f64, f64), value: f64, steps: usize) -> Dial {
    // Safety: no target and no action, so nothing is called back into.
    let control = unsafe { NSSlider::sliderWithTarget_action(None, None, mtm) };
    control.setMinValue(range.0);
    control.setMaxValue(range.1);
    control.setDoubleValue(value);
    control.setNumberOfTickMarks(steps as isize);
    control.setAllowsTickMarkValuesOnly(true);
    control.setFrame(NSRect::new(
        NSPoint::new(MARGIN, y),
        NSSize::new(WIDTH - MARGIN * 2.0 - 74.0, 22.0),
    ));
    let readout = label(
        mtm,
        "",
        NSRect::new(
            NSPoint::new(WIDTH - MARGIN - 68.0, y + 2.0),
            NSSize::new(68.0, 18.0),
        ),
        true,
    );
    // Read back rather than trusting what was set: with tick marks the
    // control snaps to the nearest one, and the difference would look like
    // the user had moved it — writing the setting back on every launch.
    let settled = control.doubleValue();
    Dial { control, readout, last: Cell::new(settled) }
}

/// How the sensitivity number reads to a person.
fn sensitivity_words(value: f64) -> String {
    match value {
        v if v <= 0.6 => "Alta".into(),
        v if v <= 0.7 => "Normal".into(),
        v if v <= 0.8 => "Baja".into(),
        _ => "Muy baja".into(),
    }
}

impl Preferences {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let settings = config::load();

        let window = {
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
            let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    mtm.alloc::<NSWindow>(),
                    frame,
                    style,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            window.setTitle(&NSString::from_str("Preferencias de Minion"));
            // Safety: the window is kept alive by this struct for the life
            // of the process, so closing it must not release it — otherwise
            // reopening from the menu would use freed memory.
            unsafe { window.setReleasedWhenClosed(false) };
            window.center();
            window
        };

        // Laid out from the top down, which is how it reads.
        // Laid out downwards from the top, leaving MARGIN clear at the
        // bottom: the last control was sitting on the window's edge.
        let mut y = HEIGHT - 52.0;
        let content = window.contentView().expect("a window has a content view");

        let add = |view: &NSView| content.addSubview(view);

        add(&label(
            mtm,
            "Comportamiento",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(200.0, 18.0)),
            true,
        ));
        y -= 26.0;

        let sounds = checkbox(mtm, "Sonido al ejecutar una orden", y, settings.sounds);
        add(&sounds.control);
        y -= 26.0;

        let log_voices = checkbox(
            mtm,
            "Registrar las voces ajenas",
            y,
            settings.log_ignored_speech,
        );
        add(&log_voices.control);
        y -= 26.0;

        let at_login = checkbox(mtm, "Abrir al iniciar sesión", y, startup::enabled());
        add(&at_login.control);
        y -= 40.0;

        add(&label(
            mtm,
            "Escucha",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(200.0, 18.0)),
            true,
        ));
        y -= 28.0;

        add(&label(
            mtm,
            "Sensibilidad",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(200.0, 18.0)),
            false,
        ));
        y -= 26.0;
        let sensitivity = slider(mtm, y, (0.55, 0.85), f64::from(settings.command_threshold()), 7);
        add(&sensitivity.control);
        add(&sensitivity.readout);
        y -= 22.0;
        add(&label(
            mtm,
            "Más alta obedece a la primera; más baja se equivoca menos.",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(WIDTH - MARGIN * 2.0, 16.0)),
            true,
        ));
        y -= 34.0;

        add(&label(
            mtm,
            "Pausa que cierra una frase",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(240.0, 18.0)),
            false,
        ));
        y -= 26.0;
        let pause = slider(
            mtm,
            y,
            (400.0, 1200.0),
            settings.audio_settings().silence_end_ms as f64,
            9,
        );
        add(&pause.control);
        add(&pause.readout);
        y -= 22.0;
        add(&label(
            mtm,
            "Más larga si te corta al pensar; más corta si tarda en responder.",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(WIDTH - MARGIN * 2.0, 16.0)),
            true,
        ));
        y -= 34.0;

        add(&label(
            mtm,
            "Liberar memoria tras",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(240.0, 18.0)),
            false,
        ));
        y -= 26.0;
        let minutes = settings
            .idle_unload()
            .map_or(0.0, |d| d.as_secs() as f64 / 60.0);
        let memory = slider(mtm, y, (0.0, 30.0), minutes, 7);
        add(&memory.control);
        add(&memory.readout);
        y -= 40.0;

        add(&label(
            mtm,
            "Atajo para pausar y reanudar",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(260.0, 18.0)),
            false,
        ));
        y -= 28.0;
        let current = settings
            .resume_shortcut()
            .unwrap_or_else(|| config::DEFAULT_RESUME_SHORTCUT.to_string());
        // A button rather than a text field: a shortcut is something you
        // press, and nobody should have to know it is spelled "alt-space".
        // Safety: no target and no action, so nothing is called back into.
        let shortcut = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(&pretty(&current)),
                None,
                None,
                mtm,
            )
        };
        shortcut.setFrame(NSRect::new(
            NSPoint::new(MARGIN, y),
            NSSize::new(170.0, 26.0),
        ));
        add(&shortcut);
        y -= 22.0;
        add(&label(
            mtm,
            "Pulsa el botón y luego la combinación que quieras.",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(WIDTH - MARGIN * 2.0, 16.0)),
            true,
        ));

        let preferences = Self {
            window,
            sounds,
            log_voices,
            at_login,
            sensitivity,
            pause,
            memory,
            shortcut,
            shortcut_value: std::cell::RefCell::new(current),
            capturing: Cell::new(false),
            button_clicks: Cell::new(0),
        };
        preferences.update_readouts();
        preferences
    }

    /// Brings the window forward, creating nothing new.
    ///
    /// The activation matters: Minion runs as an accessory, with no Dock
    /// icon, and such an application is never the active one. Ordering a
    /// window to the front without activating first leaves it behind
    /// whatever you were using — which looks exactly like nothing happened.
    pub fn show(&self) {
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    fn update_readouts(&self) {
        let set = |field: &NSTextField, text: String| {
            field.setStringValue(&NSString::from_str(&text));
        };
        set(
            &self.sensitivity.readout,
            sensitivity_words(self.sensitivity.control.doubleValue()),
        );
        set(
            &self.pause.readout,
            format!("{:.0} ms", self.pause.control.doubleValue()),
        );
        let minutes = self.memory.control.doubleValue();
        set(
            &self.memory.readout,
            if minutes < 1.0 {
                "Nunca".into()
            } else {
                format!("{minutes:.0} min")
            },
        );
    }

    /// Reads the controls and writes through anything that moved.
    ///
    /// Called from the run loop timer. Returns true when something changed,
    /// so the caller can report it.
    pub fn poll(&self) -> bool {
        let mut changed = false;

        if let Some(on) = self.sounds.toggled() {
            save("sounds", if on { "true" } else { "false" });
            changed = true;
        }
        if let Some(on) = self.log_voices.toggled() {
            save("log_ignored_speech", if on { "true" } else { "false" });
            changed = true;
        }
        if let Some(on) = self.at_login.toggled() {
            if let Err(e) = startup::set(on) {
                crate::journal::write(&format!("start at login: {e}"));
            }
            changed = true;
        }
        if let Some(value) = self.sensitivity.moved() {
            save("threshold", &format!("{value:.2}"));
            changed = true;
        }
        if let Some(value) = self.pause.moved() {
            save_audio("silence_end_ms", &format!("{value:.0}"));
            changed = true;
        }
        if let Some(value) = self.memory.moved() {
            save("unload_after_minutes", &format!("{value:.0}"));
            changed = true;
        }

        // A click on the shortcut button starts capture. NSButton counts
        // its own clicks in its cell's tag only when a target is set, so
        // instead the highlight state is read: pressed and released between
        // two polls shows up as a change in the button's state.
        if !self.capturing.get() {
            let clicks = self.shortcut.state();
            if clicks != self.button_clicks.get() {
                self.button_clicks.set(clicks);
                if clicks != 0 {
                    self.begin_capture();
                }
            }
        }

        if changed {
            self.update_readouts();
        }
        changed
    }

    /// Waits for the next key combination and stores it.
    ///
    /// Uses a local event monitor, which only sees events aimed at this
    /// application — the preferences window has focus while it is open, so
    /// the keystroke lands here and nowhere else. The monitor swallows the
    /// event by returning nothing, or pressing ⌘Q to set a shortcut would
    /// also quit something.
    fn begin_capture(&self) {
        self.capturing.set(true);
        self.shortcut
            .setTitle(&NSString::from_str("Pulsa la combinación…"));
    }

    /// Called from the run loop with whatever key was pressed, if capturing.
    pub fn capture(&self, code: u16, mods: crate::actions::Mods) -> bool {
        if !self.capturing.get() {
            return false;
        }
        self.capturing.set(false);
        self.button_clicks.set(self.shortcut.state());

        let Some(text) = crate::actions::shortcut_text(code, mods) else {
            // A key with no name: leave what was there.
            let current = self.shortcut_value.borrow().clone();
            self.shortcut.setTitle(&NSString::from_str(&pretty(&current)));
            return true;
        };
        self.shortcut.setTitle(&NSString::from_str(&pretty(&text)));
        save("resume_shortcut", &format!("\"{text}\""));
        *self.shortcut_value.borrow_mut() = text;
        true
    }

    /// Whether a key press should be taken as the new shortcut.
    pub fn is_capturing(&self) -> bool {
        self.capturing.get()
    }

    pub fn sounds_on(&self) -> bool {
        self.sounds.on()
    }

    pub fn log_voices_on(&self) -> bool {
        self.log_voices.on()
    }
}

fn save(key: &str, value: &str) {
    if let Err(e) = config::set_option(key, value) {
        crate::journal::write(&format!("could not save {key}: {e}"));
    } else {
        crate::journal::write(&format!("{key} = {value}"));
    }
}

fn save_audio(key: &str, value: &str) {
    if let Err(e) = config::set_table_option("audio", key, value) {
        crate::journal::write(&format!("could not save audio.{key}: {e}"));
    } else {
        crate::journal::write(&format!("audio.{key} = {value}"));
    }
}

/// A window showing text, with the usual close button.
///
/// The report used to appear in an AppleScript dialog, which has no window
/// controls and blocks everything behind it until dismissed. A report is
/// something to read next to the log, not a demand for attention.
pub struct Report {
    window: Retained<NSWindow>,
    text: Retained<NSTextField>,
}

impl Report {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(460.0, 380.0));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("Aprender del registro"));
        unsafe { window.setReleasedWhenClosed(false) };
        window.center();

        let text = label(
            mtm,
            "",
            NSRect::new(
                NSPoint::new(MARGIN, MARGIN),
                NSSize::new(460.0 - MARGIN * 2.0, 380.0 - MARGIN * 2.0),
            ),
            false,
        );
        text.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(11.0, 0.0)));
        if let Some(content) = window.contentView() {
            content.addSubview(&text);
        }
        Self { window, text }
    }

    pub fn show(&self, body: &str) {
        self.text.setStringValue(&NSString::from_str(body));
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_reads_as_words() {
        // The number means nothing to a person; the word does.
        assert_eq!(sensitivity_words(0.55), "Alta");
        assert_eq!(sensitivity_words(0.70), "Normal");
        assert_eq!(sensitivity_words(0.80), "Baja");
        assert_eq!(sensitivity_words(0.85), "Muy baja");
    }
}

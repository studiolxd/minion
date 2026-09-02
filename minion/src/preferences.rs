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
    NSBackingStoreType, NSButton, NSColor, NSFont, NSSlider, NSTextField, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::{config, startup};

const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 384.0;
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
    Dial { control, readout, last: Cell::new(value) }
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

        let preferences = Self {
            window,
            sounds,
            log_voices,
            at_login,
            sensitivity,
            pause,
            memory,
        };
        preferences.update_readouts();
        preferences
    }

    /// Brings the window forward, creating nothing new.
    pub fn show(&self) {
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

        if changed {
            self.update_readouts();
        }
        changed
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

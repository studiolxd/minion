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
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAccessibility, NSAutoresizingMaskOptions, NSApplication, NSBackingStoreType, NSButton, NSColor, NSFont, NSLineBreakMode,
    NSPopUpButton, NSScrollView, NSSecureTextField, NSSlider, NSTextField, NSTextView, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::{config, startup};

const WIDTH: f64 = 380.0;
/// Tall enough for the last hint to sit clear of the bottom edge.
///
/// The layout runs downwards from the top, so the window has to be as tall
/// as everything in it plus a margin; too short and the final line simply
/// falls off, which is what it did.
/// The tallest the window will open, scrolling beyond that.
///
/// Leaves room for the menu bar and the Dock rather than filling a laptop
/// screen. The content decides its own height; nothing here has to be
/// corrected when a setting is added.
const MAX_WINDOW_HEIGHT: f64 = 640.0;
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

/// Vertical rhythm, in one place.
///
/// Every spacing decision lives here rather than as a number at each call
/// site. Ten rounds of nudging individual gaps is what made this
/// necessary: a hint too close to the next control, a section heading too
/// far from its first item, and no way to fix one without checking all the
/// others.
pub(crate) mod spacing {
    /// Margin at the edges of the window.
    pub const EDGE: f64 = 22.0;
    /// From the top of the canvas to the first thing on it.
    pub const TOP: f64 = 30.0;
    /// Between two items of the same kind, such as consecutive checkboxes.
    pub const SIBLING: f64 = 10.0;
    /// Between an item and the hint that explains it: close, since the
    /// hint belongs to what is above it.
    pub const BEFORE_HINT: f64 = 3.0;
    /// After a hint, before whatever comes next.
    pub const AFTER_HINT: f64 = 14.0;
    /// Between one group of settings and the next.
    pub const GROUP: f64 = 26.0;
    /// Between a section heading and its first item.
    pub const AFTER_HEADING: f64 = 10.0;
    /// Between a field's label and the field itself.
    pub const AFTER_LABEL: f64 = 6.0;

    /// Heights of the things being placed.
    pub const HEADING: f64 = 18.0;
    pub const LABEL: f64 = 18.0;
    pub const CHECKBOX: f64 = 20.0;
    pub const FIELD: f64 = 24.0;
    pub const BUTTON: f64 = 26.0;
    pub const SLIDER: f64 = 22.0;
    pub const HINT_LINE: f64 = 15.0;
}

/// Places controls down a canvas, applying the rhythm above.
///
/// Nothing outside this struct decides how far apart two things go, and
/// the canvas grows to whatever the contents need instead of being a
/// constant that has to be corrected every time something is added.
pub(crate) struct Layout {
    mtm: MainThreadMarker,
    canvas: Retained<NSView>,
    width: f64,
    /// Distance from the top of the canvas to the next free position.
    used: f64,
    /// The last label written above a control, so the control that follows
    /// can answer to the same name when read aloud.
    last_label: Option<String>,
    /// The last control placed, so a hint can become its description.
    last_control: Option<Retained<NSView>>,
}

impl Layout {
    pub(crate) fn new(mtm: MainThreadMarker, width: f64) -> Self {
        Self {
            mtm,
            canvas: NSView::new(mtm),
            width,
            used: spacing::TOP,
            last_label: None,
            last_control: None,
        }
    }

    pub(crate) fn content_width(&self) -> f64 {
        self.width - spacing::EDGE * 2.0
    }

    /// Reserves a strip of the given height and returns its frame.
    ///
    /// Positions are measured downwards while laying out and flipped at the
    /// end, so adding something never moves what came before it.
    pub(crate) fn place(&mut self, height: f64, indent: f64) -> NSRect {
        let frame = NSRect::new(
            NSPoint::new(spacing::EDGE + indent, -(self.used + height)),
            NSSize::new(self.content_width() - indent, height),
        );
        self.used += height;
        frame
    }

    pub(crate) fn gap(&mut self, amount: f64) {
        self.used += amount;
    }

    pub(crate) fn add(&self, view: &NSView) {
        self.canvas.addSubview(view);
    }

    /// Adds a control and gives it the name a screen reader will say.
    ///
    /// Every control carries the same words as its visible label: a
    /// checkbox announced as "checkbox" and nothing else is unusable, and
    /// a slider with a label beside it has no idea the label is there.
    pub(crate) fn add_control(&mut self, view: &NSView, name: &str) {
        self.add(view);
        view.setAccessibilityLabel(Some(&NSString::from_str(name)));
        self.last_control = Some(Retained::from(view));
    }

    /// The name for a control that follows a label of its own.
    fn borrowed_label(&self) -> String {
        self.last_label.clone().unwrap_or_default()
    }

    /// A section heading.
    pub(crate) fn heading(&mut self, text: &str) {
        if self.used > spacing::TOP {
            self.gap(spacing::GROUP);
        }
        let frame = self.place(spacing::HEADING, 0.0);
        let view = small_label(self.mtm, text, frame);
        self.add(&view);
        self.gap(spacing::AFTER_HEADING);
    }

    /// A label naming the control that follows it.
    pub(crate) fn field_label(&mut self, text: &str) {
        let frame = self.place(spacing::LABEL, 0.0);
        let view = plain_label(self.mtm, text, frame);
        self.add(&view);
        self.last_label = Some(text.to_string());
        self.gap(spacing::AFTER_LABEL);
    }

    /// A line of explanation, belonging to whatever is above it.
    ///
    /// Its height follows the text, so a long one is not clipped — which is
    /// what a fixed height did to half of these.
    pub(crate) fn hint(&mut self, text: &str, indent: f64) {
        self.gap(spacing::BEFORE_HINT);
        let width = self.content_width() - indent;
        let blank = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 0.0));
        let view = small_label(self.mtm, text, blank);
        let frame = self.place(text_height(&view, width, text), indent);
        view.setFrame(frame);
        self.add(&view);
        // The hint explains the control above it, so that is where it
        // belongs for anyone who cannot see the two side by side.
        if let Some(control) = &self.last_control {
            control.setAccessibilityHelp(Some(&NSString::from_str(text)));
        }
        self.gap(spacing::AFTER_HINT);
    }

    fn checkbox(&mut self, title: &str, on: bool) -> Switch {
        let frame = self.place(spacing::CHECKBOX, 0.0);
        let button = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(title),
                None,
                None,
                self.mtm,
            )
        };
        button.setFrame(frame);
        self.add_control(&button, title);
        self.gap(spacing::SIBLING);
        let switch = Switch { control: button, last: Cell::new(on) };
        switch.show(on);
        switch
    }

    /// A slider with its readout to the right.
    fn slider(&mut self, range: (f64, f64), value: f64, steps: usize) -> Dial {
        let frame = self.place(spacing::SLIDER, 0.0);
        const READOUT: f64 = 74.0;

        // Safety: no target and no action, so nothing is called back into.
        let control = unsafe { NSSlider::sliderWithTarget_action(None, None, self.mtm) };
        control.setMinValue(range.0);
        control.setMaxValue(range.1);
        control.setDoubleValue(value);
        control.setNumberOfTickMarks(steps as isize);
        control.setAllowsTickMarkValuesOnly(true);
        // Report while being dragged, not only on release: the readout
        // beside the slider is the whole point of having one.
        control.setContinuous(true);
        control.setFrame(NSRect::new(
            frame.origin,
            NSSize::new(frame.size.width - READOUT, frame.size.height),
        ));
        let name = self.borrowed_label();
        self.add_control(&control, &name);

        let readout = small_label(
            self.mtm,
            "",
            NSRect::new(
                NSPoint::new(
                    frame.origin.x + frame.size.width - READOUT + 6.0,
                    frame.origin.y + 2.0,
                ),
                NSSize::new(READOUT - 6.0, spacing::LABEL),
            ),
        );
        self.add(&readout);

        // Read back rather than trusting what was set: with tick marks the
        // control snaps to the nearest one, and the difference would look
        // like the user had moved it.
        let settled = control.doubleValue();
        Dial { control, readout, last: Cell::new(settled) }
    }

    /// A dropdown over a fixed set of named values, such as "always"/"hold"
    /// — unlike [`Self::chooser`], every entry is one of `options` and
    /// there is no "follow the system" sentinel.
    fn popup(&mut self, options: &[(&str, &str)], current: &str) -> Popup {
        let frame = self.place(spacing::BUTTON, 0.0);
        let control = NSPopUpButton::new(self.mtm);
        control.setFrame(frame);

        let mut values = Vec::with_capacity(options.len());
        for (label, value) in options {
            control.addItemWithTitle(&NSString::from_str(label));
            values.push((*value).to_string());
        }
        let selected = values.iter().position(|v| v == current).unwrap_or(0) as isize;
        control.selectItemAtIndex(selected);
        let name = self.borrowed_label();
        self.add_control(&control, &name);
        self.gap(spacing::SIBLING);

        Popup { control, values, last: Cell::new(selected) }
    }

    /// A dropdown of device names, with "follow the system" first.
    fn chooser(&mut self, names: Vec<String>, current: Option<String>) -> Chooser {
        let frame = self.place(spacing::BUTTON, 0.0);
        let control = NSPopUpButton::new(self.mtm);
        control.setFrame(frame);

        let mut values = vec![String::new()];
        control.addItemWithTitle(&NSString::from_str("Automático (el del sistema)"));
        for name in names {
            control.addItemWithTitle(&NSString::from_str(&name));
            values.push(name);
        }

        let selected = current
            .and_then(|wanted| values.iter().position(|name| *name == wanted))
            .unwrap_or(0) as isize;
        control.selectItemAtIndex(selected);
        let name = self.borrowed_label();
        self.add_control(&control, &name);
        self.gap(spacing::SIBLING);

        Chooser { control, values, last: Cell::new(selected) }
    }

    /// Turns downward positions into the coordinates AppKit wants.
    pub(crate) fn finish(self) -> (Retained<NSView>, f64) {
        let height = self.used + spacing::EDGE;
        self.canvas.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(self.width, height),
        ));
        // Everything was placed with negative y measured from the top;
        // shift it into the canvas now that the height is known.
        for view in self.canvas.subviews().iter() {
            let frame = view.frame();
            view.setFrame(NSRect::new(
                NSPoint::new(frame.origin.x, height + frame.origin.y),
                frame.size,
            ));
        }
        (self.canvas, height)
    }
}

/// Narrows a frame to a fixed width, keeping its position.
///
/// For fields and buttons, which look wrong stretched across the window.
pub(crate) fn narrow(frame: NSRect, width: f64) -> NSRect {
    NSRect::new(frame.origin, NSSize::new(width, frame.size.height))
}

/// A frame for a second control on the same row, after one `width` wide.
pub(crate) fn beside(frame: NSRect, width: f64, own_width: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(frame.origin.x + width + spacing::SIBLING, frame.origin.y),
        NSSize::new(own_width, frame.size.height),
    )
}

/// How long the button waits for a combination before giving up.
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Escape, which cancels the capture rather than becoming the shortcut.
const ESCAPE: u16 = 53;

/// Whether a key code is F1-F12, the only keys usable without a modifier.
fn is_function_key(code: u16) -> bool {
    crate::actions::name_of_key(code)
        .is_some_and(|name| name.starts_with('f') && name[1..].parse::<u8>().is_ok())
}

/// Return, and Return on the numeric keypad: both commit a field.
const RETURN: u16 = 36;
const KEYPAD_ENTER: u16 = 76;

/// Height of a line of the training prompt, which is set larger than a
/// hint because it is read aloud from across the room.
const PROMPT_LINE: f64 = 19.0;

/// Indent for a hint that belongs to a checkbox, lining up with its label.
const INDENT: f64 = 20.0;

/// The height a label needs at a given width, asked of AppKit.
///
/// Counting characters and dividing by an average width is what clipped
/// the accented Spanish hints: «í» and «ó» are not the average character,
/// and the estimate came up a line short on exactly the lines that
/// mattered. The cell lays the text out with the font it will be drawn
/// in, so it knows. [`wrapped_lines`] stays as the fallback for the case
/// where there is no cell to ask.
fn text_height(field: &NSTextField, width: f64, text: &str) -> f64 {
    // A tall box to wrap inside; the answer is the height actually used.
    const ROOM: f64 = 10_000.0;
    let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, ROOM));
    let measured = field
        .cell()
        .map(|cell| cell.cellSizeForBounds(bounds).height)
        .filter(|height| height.is_finite() && *height > 0.0);
    match measured {
        Some(height) => height.ceil().max(spacing::HINT_LINE),
        None => spacing::HINT_LINE * wrapped_lines(text, width),
    }
}

/// How many lines a hint needs at the width it has.
///
/// Approximate — 11-point system text averages close to six points per
/// character — but erring long only leaves a little space, while erring
/// short cuts words off. Only used when the label has no cell to measure.
fn wrapped_lines(text: &str, width: f64) -> f64 {
    let per_line = (width / 5.9).max(10.0);
    ((text.chars().count() as f64 / per_line).ceil()).max(1.0)
}

/// A dropdown of device names, with "follow the system" first.
struct Chooser {
    control: Retained<NSPopUpButton>,
    /// The names behind the entries, in the same order. The first is empty,
    /// meaning follow the system.
    values: Vec<String>,
    last: Cell<isize>,
}

impl Chooser {
    /// The name chosen, or `None` for the system default.
    fn chosen(&self) -> Option<String> {
        let index = self.control.indexOfSelectedItem().max(0) as usize;
        self.values
            .get(index)
            .filter(|name| !name.is_empty())
            .cloned()
    }

    fn changed(&self) -> Option<Option<String>> {
        let now = self.control.indexOfSelectedItem();
        (now != self.last.get()).then(|| {
            self.last.set(now);
            self.chosen()
        })
    }
}

/// A dropdown over a fixed set of named values.
struct Popup {
    control: Retained<NSPopUpButton>,
    /// The value behind each entry, in the same order.
    values: Vec<String>,
    last: Cell<isize>,
}

impl Popup {
    fn value(&self) -> String {
        let index = self.control.indexOfSelectedItem().max(0) as usize;
        self.values.get(index).cloned().unwrap_or_default()
    }

    fn changed(&self) -> Option<String> {
        let now = self.control.indexOfSelectedItem();
        (now != self.last.get()).then(|| {
            self.last.set(now);
            self.value()
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

/// A push button, and the state it had when it was last read.
///
/// AppKit only counts clicks for a button with a target, and this window
/// deliberately has none — see the note at the top. A click shows up
/// instead as a change in the button's state between two polls.
pub(crate) struct Press {
    control: Retained<NSButton>,
    last: Cell<isize>,
}

impl Press {
    pub(crate) fn new(control: Retained<NSButton>) -> Self {
        let last = Cell::new(control.state());
        Self { control, last }
    }

    pub(crate) fn clicked(&self) -> bool {
        let now = self.control.state();
        now != self.last.replace(now) && now != 0
    }
}

/// How many enrolled voices the window shows.
///
/// A household, not a call centre: six rows is more than anyone has asked
/// for, and the rest — if there ever are any — still work, they are simply
/// managed by deleting a file. The rows are built with the window, so this
/// is a fixed number rather than a list that grows.
const MAX_VOICE_ROWS: usize = 6;

/// What the line above the list says about who Minion knows.
fn voices_summary(enrolled: &[String]) -> String {
    match enrolled {
        [] => "Ahora obedece a cualquiera que diga la palabra clave.".to_string(),
        [only] => format!("Minion solo obedece a {only}."),
        many => format!("Minion obedece a {} voces.", many.len()),
    }
}

/// One enrolled voice in the list: the name, and the way to forget it.
struct ProfileRow {
    name: std::cell::RefCell<String>,
    label: Retained<NSTextField>,
    forget: Press,
}

impl ProfileRow {
    /// Shows this row as `name`, or hides it when there is nobody left.
    fn show(&self, name: Option<&str>) {
        match name {
            Some(name) => {
                *self.name.borrow_mut() = name.to_string();
                self.label.setStringValue(&NSString::from_str(name));
                self.label.setHidden(false);
                self.forget.control.setHidden(false);
                self.forget.control.setAccessibilityLabel(Some(&NSString::from_str(
                    &format!("Olvidar la voz de {name}"),
                )));
            }
            None => {
                self.name.borrow_mut().clear();
                self.label.setHidden(true);
                self.forget.control.setHidden(true);
            }
        }
    }
}

pub struct Preferences {
    window: Retained<NSWindow>,
    sounds: Switch,
    log_voices: Switch,
    recordings: Switch,
    at_login: Switch,
    speak: Switch,
    wake_word: Retained<NSTextField>,
    last_wake_word: std::cell::RefCell<String>,
    /// Set by the key watcher below when Return is pressed, so a field can
    /// be committed without waiting for the focus to move.
    entered: Rc<Cell<bool>>,
    /// Kept alive for as long as the window: dropping it stops the watch.
    _keys: KeyCapture,
    microphone: Chooser,
    speaker: Chooser,
    sensitivity: Dial,
    pause: Dial,
    memory: Dial,
    listen_mode: Popup,
    conversation: Dial,
    pause_when_microphone_busy: Switch,
    spoken_punctuation: Switch,
    auto_capitalise: Switch,
    notifications: Switch,
    show_hud: Switch,
    search_engine: Popup,
    shortcut: Retained<NSButton>,
    /// The shortcut as stored, e.g. "alt-space".
    shortcut_value: std::cell::RefCell<String>,
    /// Clears the shortcut, leaving Minion with none.
    clear_shortcut: Press,
    /// True while waiting for the user to press a combination.
    capturing: Cell<bool>,
    /// When the wait started, so it can give up on its own.
    capture_started: Cell<Option<std::time::Instant>>,
    /// Starts and reports voice training.
    train: Retained<NSButton>,
    train_clicks: Cell<isize>,
    train_status: Retained<NSTextField>,
    train_requested: Cell<bool>,
    /// Stops a training session halfway through.
    cancel_train: Press,
    cancel_requested: Cell<bool>,
    restart_requested: Cell<bool>,
    /// The name to file the voice about to be trained under.
    new_name: Retained<NSTextField>,
    /// One row per enrolled voice: who it is, and a button to forget them.
    ///
    /// Built once, since the window is too: the rows beyond the voices
    /// currently enrolled are hidden rather than absent, and
    /// [`Preferences::show_profiles`] fills them in again whenever the list
    /// changes.
    profiles: Vec<ProfileRow>,
    /// The button's state last time it was read, to notice a click without
    /// an Objective-C target — see the note at the top of this file.
    button_clicks: Cell<isize>,
    /// Opens the vocabulary editor window — see `vocabulary_editor.rs`.
    /// `main.rs` owns that window, so this only records the click.
    edit_vocabulary: Press,
    edit_vocabulary_requested: Cell<bool>,
    ai_enabled: Switch,
    ai_backend: Popup,
    ai_use: Popup,
    ai_daily_limit: Retained<NSTextField>,
    last_ai_daily_limit: std::cell::RefCell<String>,
    ai_probe: Press,
    ai_status: Retained<NSTextField>,
    /// Filled in by the probe thread when it finishes; read and cleared on
    /// the next poll. `Arc<Mutex<_>>` rather than a `Cell`: the probe runs
    /// on its own thread, the same way a real question to the AI layer
    /// does in `main.rs`, so this window is never blocked waiting on it.
    ai_probe_result: Arc<Mutex<Option<String>>>,
    ai_probe_running: Cell<bool>,
    /// Re-runs `crate::ai::detect()` on a worker thread — the probes it
    /// runs are real requests to real CLI agents, seconds each, so this
    /// window's own thread never blocks on one.
    ai_check: Press,
    ai_check_status: Retained<NSTextField>,
    ai_detect_result: Arc<Mutex<Option<Vec<crate::ai::Detected>>>>,
    ai_detect_running: Cell<bool>,
    /// Shown only for a backend in [`crate::ai::ProviderInfo::needs_key`].
    ai_key_label: Retained<NSTextField>,
    ai_key_field: Retained<NSSecureTextField>,
    ai_save_key: Press,
    ai_forget_key: Press,
    ai_key_status: Retained<NSTextField>,
    ai_model_field: Retained<NSTextField>,
    last_ai_model: std::cell::RefCell<String>,
    /// Only meaningful for Codex, whose installed CLI may name a model the
    /// user's ChatGPT plan does not actually allow.
    ai_codex_hint: Retained<NSTextField>,
    /// Shown only for an OpenAI-compatible backend — the two local servers
    /// and any cloud preset pointed at a proxy in front of it.
    ai_base_url_label: Retained<NSTextField>,
    ai_base_url_field: Retained<NSTextField>,
    ai_base_url_hint: Retained<NSTextField>,
    last_ai_base_url: std::cell::RefCell<String>,
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

pub(crate) fn small_label(mtm: MainThreadMarker, text: &str, frame: NSRect) -> Retained<NSTextField> {
    label(mtm, text, frame, true)
}

pub(crate) fn plain_label(mtm: MainThreadMarker, text: &str, frame: NSRect) -> Retained<NSTextField> {
    label(mtm, text, frame, false)
}

fn label(mtm: MainThreadMarker, text: &str, frame: NSRect, small: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFrame(frame);
    // Wrap rather than truncate: a prompt someone has to read aloud is
    // useless with its end cut off.
    field.setUsesSingleLineMode(false);
    field.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    field.setMaximumNumberOfLines(0);
    if small {
        field.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    field
}



/// The sensitivity settings, in the order they appear on the slider.
///
/// Discrete steps with a name each, rather than a range mapped to words:
/// with seven positions and four names, two thirds of the travel changed
/// nothing that could be seen.
const SENSITIVITY: &[(&str, f32)] = &[
    ("Muy alta", 0.58),
    ("Alta", 0.64),
    ("Normal", 0.70),
    ("Baja", 0.78),
    ("Muy baja", 0.86),
];

/// The slider position closest to a stored threshold.
fn sensitivity_step(threshold: f32) -> f64 {
    SENSITIVITY
        .iter()
        .enumerate()
        .min_by(|(_, (_, a)), (_, (_, b))| {
            (a - threshold)
                .abs()
                .partial_cmp(&(b - threshold).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(2.0, |(i, _)| i as f64)
}

/// The name and value at a slider position.
fn sensitivity_at(step: f64) -> (&'static str, f32) {
    let index = (step.round().max(0.0) as usize).min(SENSITIVITY.len() - 1);
    SENSITIVITY[index]
}

/// Whether `id` is one of the CLI agents `crate::ai::cli::AGENTS` lists —
/// the ones a real probe can say something about — rather than an HTTP
/// provider, local or not.
fn is_cli_provider(id: &str) -> bool {
    crate::ai::cli::AGENTS.iter().any(|agent| agent.backend == id)
}

/// Whether `id` reaches an OpenAI-compatible endpoint, which is the only
/// kind `[ai] base_url` is offered for — Anthropic's own API and the CLI
/// agents have no base URL worth overriding from this window.
fn is_openai_compatible_provider(id: &str) -> bool {
    crate::ai::openai_compat::preset(id).is_some()
}

/// A duration, written the way a Spanish sentence writes one: a comma for
/// the decimal point.
fn spanish_seconds(seconds: f32) -> String {
    format!("{seconds:.1}").replace('.', ",")
}

/// The state word(s) shown after a provider's name in the backend popup.
///
/// Pure, so every state — including the ones this Mac cannot itself be in
/// right now, such as a Codex model the account refuses — can be checked
/// without a real probe. `detected` is `None` before the worker thread
/// that runs `crate::ai::detect()` has answered even once.
fn provider_state_text(
    is_cli: bool,
    needs_key: bool,
    has_key: bool,
    detected: Option<&crate::ai::Detected>,
) -> String {
    if is_cli {
        return match detected {
            None => "comprobando…".to_string(),
            Some(found) if found.path.is_none() => "no disponible".to_string(),
            Some(found) if found.authenticated => {
                let seconds = found.latency.map_or(0.0, |latency| latency.as_secs_f32());
                format!("listo ({} s)", spanish_seconds(seconds))
            }
            Some(found) => {
                let why = found.problem.as_deref().unwrap_or("no responde");
                format!("instalado, {why}")
            }
        };
    }
    if !needs_key {
        "sin clave".to_string()
    } else if has_key {
        "clave guardada".to_string()
    } else {
        "falta la clave".to_string()
    }
}

/// The backend popup's entries, in `providers`' order. `detected` is the
/// last-known result of `crate::ai::detect()`, or `None` before the first
/// one has come back.
fn ai_backend_labels(
    providers: &[crate::ai::ProviderInfo],
    detected: Option<&[crate::ai::Detected]>,
) -> Vec<String> {
    providers
        .iter()
        .map(|info| {
            let is_cli = is_cli_provider(info.id);
            let found = detected.and_then(|list| list.iter().find(|found| found.backend == info.id));
            let has_key = !is_cli && info.needs_key && crate::ai::has_key(info.id);
            let state = provider_state_text(is_cli, info.needs_key, has_key, found);
            format!("{} — {state}", info.label)
        })
        .collect()
}

/// Lays out a line of explanation exactly like [`Layout::hint`], but hands
/// back the label instead of losing it — for a hint that is only shown for
/// some backends and has to be hidden and shown again later.
fn hidable_hint(layout: &mut Layout, mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    layout.gap(spacing::BEFORE_HINT);
    let width = layout.content_width();
    let blank = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 0.0));
    let view = small_label(mtm, text, blank);
    let frame = layout.place(text_height(&view, width, text), 0.0);
    view.setFrame(frame);
    layout.add(&view);
    layout.gap(spacing::AFTER_HINT);
    view
}

/// What the model field's placeholder says: the backend's own default, or
/// nothing for a CLI agent, which has no fixed one to name.
fn model_placeholder(providers: &[crate::ai::ProviderInfo], backend: &str) -> String {
    providers
        .iter()
        .find(|info| info.id == backend)
        .map(|info| info.default_model)
        .filter(|model| !model.is_empty())
        .map(|model| format!("por defecto: {model}"))
        .unwrap_or_default()
}

impl Preferences {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let settings = config::load();
        let mut layout = Layout::new(mtm, WIDTH);

        layout.heading("Comportamiento");
        let sounds = layout.checkbox("Sonido al ejecutar una orden", settings.sounds);
        let log_voices = layout.checkbox(
            "Anotar en el registro lo que dicen otros",
            settings.log_ignored_speech,
        );
        layout.hint(
            "Con el micrófono abierto se transcribe todo lo que se habla cerca. \
             Normalmente solo se cuenta cuánto se oyó, no qué se dijo.",
            INDENT,
        );
        let recordings = layout.checkbox(
            "Guardar lo que oye en archivos de audio",
            settings.save_recordings,
        );
        layout.hint(
            "Guarda cada frase como WAV en ~/Library/Application \
             Support/Minion/recordings. Actívalo solo mientras depuras.",
            INDENT,
        );
        let at_login = layout.checkbox("Abrir al iniciar sesión", startup::enabled());
        let speak = layout.checkbox("Responder en voz alta", settings.speak);
        layout.hint(
            "Solo a preguntas: «¿qué hora es?», «¿cuánta batería queda?».",
            INDENT,
        );

        // Directly under the behaviour it changes, and above the fold: in
        // a window that scrolls, a section at the bottom is one nobody
        // finds, and this is the one that decides who Minion obeys.
        layout.heading("Voces");
        let enrolled = crate::speaker::profile_names();
        layout.field_label("Nombre de la voz");
        let new_name = NSTextField::new(mtm);
        new_name.setStringValue(&NSString::from_str(if enrolled.is_empty() {
            crate::speaker::DEFAULT_NAME
        } else {
            ""
        }));
        new_name.setFrame(narrow(layout.place(spacing::FIELD, 0.0), 170.0));
        layout.add_control(&new_name, "Nombre de la voz");
        layout.gap(spacing::SIBLING);

        // Safety: no target and no action, so nothing is called back into.
        let train = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Añadir una voz…"),
                None,
                None,
                mtm,
            )
        };
        let voice_row = layout.place(spacing::BUTTON, 0.0);
        train.setFrame(narrow(voice_row, 170.0));
        layout.add_control(&train, "Añadir una voz");
        // Safety: no target and no action, so nothing is called back into.
        let cancel_train = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Cancelar"),
                None,
                None,
                mtm,
            )
        };
        cancel_train.setFrame(beside(voice_row, 170.0, 110.0));
        cancel_train.setAccessibilityLabel(Some(&NSString::from_str(
            "Cancelar el entrenamiento",
        )));
        // Only means anything while training is under way.
        cancel_train.setHidden(true);
        layout.add(&cancel_train);

        // Big enough to read from where you sit to talk to the machine:
        // this line is a sentence to be said aloud, not a footnote.
        let train_status_frame = {
            layout.gap(spacing::BEFORE_HINT);
            layout.place(PROMPT_LINE * 2.0, 0.0)
        };
        let train_status = plain_label(
            mtm,
            &voices_summary(&enrolled),
            train_status_frame,
        );
        train_status.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        layout.add(&train_status);
        layout.gap(spacing::SIBLING);

        // One row per voice, plus a spare: the window is built once and its
        // layout is fixed, so a voice added while it is open needs a row
        // waiting for it. Only the spare is ever blank — reserving six rows
        // for a household that has one would leave a hole in the window.
        let rows = (enrolled.len() + 1).min(MAX_VOICE_ROWS);
        let mut profiles = Vec::with_capacity(rows);
        for index in 0..rows {
            let row = layout.place(spacing::BUTTON, 0.0);
            let label = plain_label(mtm, "", narrow(row, 170.0));
            layout.add(&label);
            // Safety: no target and no action, so nothing is called back into.
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str("Olvidar"),
                    None,
                    None,
                    mtm,
                )
            };
            button.setFrame(beside(row, 170.0, 110.0));
            layout.add(&button);
            layout.gap(spacing::SIBLING);
            let entry = ProfileRow {
                name: std::cell::RefCell::new(String::new()),
                label,
                forget: Press::new(button),
            };
            entry.show(enrolled.get(index).map(String::as_str));
            profiles.push(entry);
        }
        layout.hint(
            "Cada voz se entrena diciendo cinco frases. Todas pueden hacer lo \
             mismo: el nombre solo sirve para el registro y para «¿quién soy?».",
            0.0,
        );

        layout.heading("Palabra clave");
        let current_wake = settings
            .wake_words
            .first()
            .cloned()
            .unwrap_or_else(|| crate::commands::DEFAULT_WAKE_WORDS[0].to_string());
        let wake_word = NSTextField::new(mtm);
        wake_word.setStringValue(&NSString::from_str(&current_wake));
        wake_word.setFrame(narrow(layout.place(spacing::FIELD, 0.0), 170.0));
        layout.add_control(&wake_word, "Palabra clave");
        layout.hint(
            "Toda orden empieza por ella. Elige algo que no digas por casualidad.",
            0.0,
        );

        layout.heading("Escucha");
        layout.field_label("Modo de escucha");
        let listen_mode = layout.popup(
            &[
                ("Siempre", "always"),
                ("Mientras se mantenga pulsado el atajo", "hold"),
            ],
            if settings.listen_mode() == config::ListenMode::Hold { "hold" } else { "always" },
        );
        layout.hint(
            "Con el atajo, Minion solo escucha mientras lo mantienes pulsado \
             y no hace falta decir la palabra clave.",
            0.0,
        );

        layout.field_label("Sensibilidad");
        let sensitivity = layout.slider(
            (0.0, (SENSITIVITY.len() - 1) as f64),
            sensitivity_step(settings.command_threshold()),
            SENSITIVITY.len(),
        );
        layout.hint(
            "Más alta obedece a la primera; más baja se equivoca menos.",
            0.0,
        );

        layout.field_label("Pausa que cierra una frase");
        let pause = layout.slider(
            (400.0, 1200.0),
            settings.audio_settings().silence_end_ms as f64,
            9,
        );
        layout.hint(
            "Más larga si te corta al pensar; más corta si tarda en responder.",
            0.0,
        );

        layout.field_label("Liberar memoria tras");
        let minutes = settings
            .idle_unload()
            .map_or(0.0, |d| d.as_secs() as f64 / 60.0);
        let memory = layout.slider((0.0, 30.0), minutes, 7);
        layout.hint(
            "«Nunca» mantiene el modelo cargado: responde antes, usa ~900 MB.",
            0.0,
        );

        layout.heading("Conversación");
        layout.field_label("Tras una orden, sigue escuchando durante");
        let seconds = settings.conversation_window().as_secs() as f64;
        let conversation = layout.slider((0.0, 15.0), seconds, 16);
        layout.hint(
            "Tras una orden, los siguientes segundos no hace falta decir la \
             palabra clave.",
            0.0,
        );

        layout.heading("Dictado");
        let spoken_punctuation = layout.checkbox(
            "Puntuación hablada: «coma», «punto», «abre interrogación»",
            settings.spoken_punctuation,
        );
        let auto_capitalise =
            layout.checkbox("Mayúscula al empezar una frase", settings.auto_capitalise);
        layout.hint(
            "Para nombres que el reconocedor no acierta, añade \
             [[dictation_words]] en config.toml.",
            INDENT,
        );

        layout.heading("Pausa automática");
        let pause_when_microphone_busy = layout.checkbox(
            "Pausar mientras otra app use el micrófono",
            settings.pause_when_microphone_busy,
        );
        layout.hint(
            "Videollamadas: Minion se pausa al descolgar y vuelve al colgar. \
             Bloquear la pantalla o dormir el Mac siempre pausa.",
            INDENT,
        );

        layout.heading("Avisos");
        let notifications = layout.checkbox(
            "Notificaciones del sistema para respuestas y temporizadores",
            settings.notifications,
        );
        let show_hud = layout.checkbox("Mostrar siempre lo que oye", settings.show_hud);
        layout.hint(
            "Un panel junto a la esquina superior derecha con la cara y la \
             última frase. Sin esto, solo aparece unos segundos tras oír \
             algo — también se dice: «muestra lo que oyes» / «esconde lo \
             que oyes».",
            INDENT,
        );

        layout.heading("Búsqueda");
        layout.field_label("Motor para «busca X» sin nombrar uno");
        let search_engine = layout.popup(
            &[
                ("Google", "google"),
                ("YouTube", "youtube"),
                ("Wikipedia", "wikipedia"),
                ("Amazon", "amazon"),
            ],
            settings.search_engine().as_deref().unwrap_or("google"),
        );

        layout.heading("IA");
        let ai_settings = crate::ai::Settings::from_config(&settings.ai);
        let ai_enabled = layout.checkbox("Usar IA para preguntas y frases no entendidas", ai_settings.enabled());
        layout.hint(
            "Envía lo que se transcribe — nunca el audio — a un asistente cuando \
             se lo pides o cuando una frase no se entiende. Apagado por defecto.",
            INDENT,
        );

        // The full list is built before any probe has run — see
        // `ai_backend_labels` below, called with `None` — so opening this
        // window never waits on one. The probes themselves start on a
        // worker thread a few lines down, and «Comprobar» repeats them.
        let providers = crate::ai::providers();
        let ai_backend_options: Vec<(String, &str)> = ai_backend_labels(providers, None)
            .into_iter()
            .zip(providers.iter())
            .map(|(label, info)| (label, info.id))
            .collect();
        let ai_backend_options_ref: Vec<(&str, &str)> =
            ai_backend_options.iter().map(|(label, id)| (label.as_str(), *id)).collect();
        let current_backend = if ai_settings.backend.is_empty() {
            providers.first().map_or("claude-code", |info| info.id)
        } else {
            ai_settings.backend.as_str()
        };
        layout.field_label("Backend");
        let ai_backend = layout.popup(&ai_backend_options_ref, current_backend);
        layout.gap(spacing::SIBLING);

        let ai_check_row = layout.place(spacing::BUTTON, 0.0);
        // Safety: no target and no action, so nothing is called back into.
        let ai_check = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Comprobar"), None, None, mtm)
        };
        ai_check.setFrame(narrow(ai_check_row, 100.0));
        layout.add_control(&ai_check, "Comprobar los backends de IA");
        let ai_check_status = plain_label(
            mtm,
            "",
            beside(ai_check_row, 100.0, layout.content_width() - 100.0 - spacing::SIBLING),
        );
        layout.add(&ai_check_status);
        layout.gap(spacing::SIBLING);
        layout.hint(
            "Los agentes de terminal (Claude Code, Codex, Gemini CLI) se \
             prueban de verdad, y eso tarda unos segundos; los demás solo \
             se miran para ver si tienen una clave guardada.",
            0.0,
        );

        let key_row = layout.place(spacing::LABEL, 0.0);
        let ai_key_label = plain_label(mtm, "Clave de API", key_row);
        layout.add(&ai_key_label);
        layout.last_label = Some("Clave de API".to_string());
        layout.gap(spacing::AFTER_LABEL);

        let ai_key_field = NSSecureTextField::new(mtm);
        ai_key_field.setPlaceholderString(Some(&NSString::from_str("Clave nueva")));
        ai_key_field.setFrame(layout.place(spacing::FIELD, 0.0));
        layout.add_control(&ai_key_field, "Clave de API");
        layout.gap(spacing::SIBLING);

        let key_buttons_row = layout.place(spacing::BUTTON, 0.0);
        // Safety: no target and no action, so nothing is called back into.
        let ai_save_key = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Guardar clave"),
                None,
                None,
                mtm,
            )
        };
        ai_save_key.setFrame(narrow(key_buttons_row, 120.0));
        layout.add_control(&ai_save_key, "Guardar la clave en el llavero");
        // Safety: no target and no action, so nothing is called back into.
        let ai_forget_key = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Olvidar clave"),
                None,
                None,
                mtm,
            )
        };
        ai_forget_key.setFrame(beside(key_buttons_row, 120.0, 120.0));
        layout.add_control(&ai_forget_key, "Olvidar la clave guardada");
        layout.gap(spacing::SIBLING);

        let ai_key_status = plain_label(mtm, "", layout.place(spacing::LABEL, 0.0));
        layout.add(&ai_key_status);
        layout.gap(spacing::SIBLING);
        layout.hint(
            "La clave se guarda en el llavero de macOS, nunca en config.toml \
             ni en el registro; nunca se vuelve a mostrar aquí. También se \
             puede guardar con «minion ai set-key <backend>» en la terminal.",
            0.0,
        );

        layout.field_label("Modelo");
        let ai_model_field = NSTextField::new(mtm);
        ai_model_field.setStringValue(&NSString::from_str(&ai_settings.model));
        ai_model_field.setPlaceholderString(Some(&NSString::from_str(&model_placeholder(
            providers,
            current_backend,
        ))));
        ai_model_field.setFrame(layout.place(spacing::FIELD, 0.0));
        layout.add_control(&ai_model_field, "Modelo");
        layout.gap(spacing::SIBLING);
        let ai_codex_hint = hidable_hint(
            &mut layout,
            mtm,
            "Codex, con una cuenta de ChatGPT, puede rechazar un modelo \
             que su propio archivo de configuración nombra si el plan no lo \
             permite.",
        );

        let base_url_row = layout.place(spacing::LABEL, 0.0);
        let ai_base_url_label = plain_label(mtm, "URL base", base_url_row);
        layout.add(&ai_base_url_label);
        layout.last_label = Some("URL base".to_string());
        layout.gap(spacing::AFTER_LABEL);
        let ai_base_url_field = NSTextField::new(mtm);
        ai_base_url_field.setStringValue(&NSString::from_str(&ai_settings.base_url));
        ai_base_url_field.setFrame(layout.place(spacing::FIELD, 0.0));
        layout.add_control(&ai_base_url_field, "URL base");
        layout.gap(spacing::SIBLING);
        let ai_base_url_hint = hidable_hint(
            &mut layout,
            mtm,
            "Solo para un servidor local o un proxy delante de un proveedor \
             compatible con OpenAI.",
        );

        layout.field_label("Se usa para");
        let ai_use = layout.popup(
            &[
                ("Solo preguntas", "[\"questions\"]"),
                ("Preguntas y frases no entendidas", "[\"questions\", \"unknown\"]"),
            ],
            if ai_settings.allows(crate::ai::Purpose::Unknown) {
                "[\"questions\", \"unknown\"]"
            } else {
                "[\"questions\"]"
            },
        );

        layout.field_label("Límite de peticiones al día (0 = sin límite)");
        let ai_daily_limit = NSTextField::new(mtm);
        ai_daily_limit.setStringValue(&NSString::from_str(&ai_settings.daily_limit.to_string()));
        ai_daily_limit.setFrame(narrow(layout.place(spacing::FIELD, 0.0), 100.0));
        layout.add_control(&ai_daily_limit, "Límite de peticiones al día");
        layout.gap(spacing::SIBLING);

        let ai_probe_row = layout.place(spacing::BUTTON, 0.0);
        // Safety: no target and no action, so nothing is called back into.
        let ai_probe = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Probar"),
                None,
                None,
                mtm,
            )
        };
        ai_probe.setFrame(narrow(ai_probe_row, 90.0));
        layout.add_control(&ai_probe, "Probar la IA");
        let ai_status = plain_label(
            mtm,
            &format!("Peticiones hoy: {} de {}", crate::ai::requests_today(), ai_settings.daily_limit),
            beside(ai_probe_row, 90.0, layout.content_width() - 90.0 - spacing::SIBLING),
        );
        layout.add(&ai_status);
        layout.gap(spacing::SIBLING);
        layout.hint(
            "«Probar» pregunta «OK» al backend elegido y muestra cuánto ha \
             tardado, sin contar como una de las peticiones de hoy salvo que \
             de verdad llegue a él.",
            0.0,
        );

        layout.heading("Vocabulario");
        // Safety: no target and no action, so nothing is called back into.
        let edit_vocabulary = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Editar vocabulario…"),
                None,
                None,
                mtm,
            )
        };
        edit_vocabulary.setFrame(narrow(layout.place(spacing::BUTTON, 0.0), 200.0));
        layout.add_control(&edit_vocabulary, "Editar vocabulario…");
        layout.hint(
            "Añade tus propias aplicaciones, órdenes y alias, y olvida los \
             que ya no quieras.",
            0.0,
        );

        layout.heading("Atajo para pausar y reanudar");
        let current_shortcut = settings
            .resume_shortcut()
            .unwrap_or_else(|| config::DEFAULT_RESUME_SHORTCUT.to_string());
        // Safety: no target and no action, so nothing is called back into.
        let shortcut = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(&pretty(&current_shortcut)),
                None,
                None,
                mtm,
            )
        };
        let row = layout.place(spacing::BUTTON, 0.0);
        shortcut.setFrame(narrow(row, 170.0));
        layout.add_control(&shortcut, "Atajo para pausar y reanudar");
        // Safety: no target and no action, so nothing is called back into.
        let clear = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Ninguno"),
                None,
                None,
                mtm,
            )
        };
        clear.setFrame(beside(row, 170.0, 100.0));
        clear.setAccessibilityLabel(Some(&NSString::from_str("Quitar el atajo")));
        layout.add(&clear);
        layout.hint(
            "Pulsa el botón y luego la combinación, que debe llevar ⌘, ⌥ o ⌃. \
             Escape cancela; «Ninguno» deja a Minion sin atajo.",
            0.0,
        );

        layout.heading("Dispositivos");
        layout.field_label("Micrófono");
        let microphone = layout.chooser(crate::audio::input_names(), settings.microphone());
        layout.field_label("Altavoz");
        let speaker = layout.chooser(crate::speech::output_names(), settings.speaker());
        layout.hint(
            "En automático cambian con el Mac; fíjalos para que no lo hagan.",
            0.0,
        );

        let (canvas, content_height) = layout.finish();

        let window = {
            let visible = content_height.min(MAX_WINDOW_HEIGHT);
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, visible));
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
            window.setTitle(&NSString::from_str("Ajustes de Minion"));
            // Safety: the window is kept alive by this struct for the life
            // of the process, so closing it must not release it — otherwise
            // reopening from the menu would use freed memory.
            unsafe { window.setReleasedWhenClosed(false) };
            window.center();

            // Scrolls when the settings are taller than a laptop screen.
            let scroll = NSScrollView::new(mtm);
            scroll.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(WIDTH, visible),
            ));
            scroll.setHasVerticalScroller(true);
            scroll.setDrawsBackground(false);
            scroll.setDocumentView(Some(&canvas));
            if let Some(content) = window.contentView() {
                content.addSubview(&scroll);
            }
            window
        };

        // Return commits a field without waiting for the focus to leave it.
        // The watcher never swallows the key: it only takes note.
        let entered = Rc::new(Cell::new(false));
        let pressed = Rc::clone(&entered);
        let keys = capture_keys(move |code, _mods| {
            if code == RETURN || code == KEYPAD_ENTER {
                pressed.set(true);
            }
            false
        });

        let preferences = Self {
            window,
            sounds,
            log_voices,
            recordings,
            at_login,
            speak,
            wake_word,
            last_wake_word: std::cell::RefCell::new(current_wake),
            entered,
            _keys: keys,
            microphone,
            speaker,
            sensitivity,
            pause,
            memory,
            listen_mode,
            conversation,
            pause_when_microphone_busy,
            spoken_punctuation,
            auto_capitalise,
            notifications,
            show_hud,
            search_engine,
            shortcut,
            shortcut_value: std::cell::RefCell::new(current_shortcut),
            clear_shortcut: Press::new(clear),
            capturing: Cell::new(false),
            capture_started: Cell::new(None),
            button_clicks: Cell::new(0),
            train,
            train_clicks: Cell::new(0),
            train_status,
            train_requested: Cell::new(false),
            cancel_train: Press::new(cancel_train),
            cancel_requested: Cell::new(false),
            restart_requested: Cell::new(false),
            new_name,
            profiles,
            edit_vocabulary: Press::new(edit_vocabulary),
            edit_vocabulary_requested: Cell::new(false),
            ai_enabled,
            ai_backend,
            ai_use,
            last_ai_daily_limit: std::cell::RefCell::new(ai_daily_limit.stringValue().to_string()),
            ai_daily_limit,
            ai_probe: Press::new(ai_probe),
            ai_status,
            ai_probe_result: Arc::new(Mutex::new(None)),
            ai_probe_running: Cell::new(false),
            ai_check: Press::new(ai_check),
            ai_check_status,
            ai_detect_result: {
                // The section is on screen the moment this window exists —
                // it is built once at startup and only shown or hidden
                // after that — so the probes start right here, on their
                // own thread, rather than blocking the constructor.
                let result = Arc::new(Mutex::new(None));
                let slot = Arc::clone(&result);
                std::thread::spawn(move || {
                    let detected = crate::ai::detect();
                    if let Ok(mut held) = slot.lock() {
                        *held = Some(detected);
                    }
                });
                result
            },
            ai_detect_running: Cell::new(true),
            ai_key_label,
            ai_key_field,
            ai_save_key: Press::new(ai_save_key),
            ai_forget_key: Press::new(ai_forget_key),
            ai_key_status,
            ai_model_field,
            last_ai_model: std::cell::RefCell::new(ai_settings.model.clone()),
            ai_codex_hint,
            ai_base_url_label,
            ai_base_url_field,
            ai_base_url_hint,
            last_ai_base_url: std::cell::RefCell::new(ai_settings.base_url.clone()),
        };
        preferences.update_readouts();
        preferences.sync_ai_provider_controls();
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
            install_main_menu(mtm);
            let app = NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    /// Whether the window is on screen right now.
    ///
    /// Lets the run loop timer tell "nobody is looking" from "a slider
    /// might be moving", without keeping its own copy of that state.
    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Whether a text field is being typed into right now.
    ///
    /// A field under the cursor owns the window's field editor; when the
    /// focus leaves, that editor goes away. Asking the control is more
    /// reliable than comparing against the first responder, which during
    /// editing is the editor rather than the field.
    fn is_editing(&self, field: &NSTextField) -> bool {
        field.currentEditor().is_some()
    }

    fn update_readouts(&self) {
        let set = |field: &NSTextField, text: String| {
            field.setStringValue(&NSString::from_str(&text));
        };
        set(
            &self.sensitivity.readout,
            sensitivity_at(self.sensitivity.control.doubleValue()).0.to_string(),
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
        let seconds = self.conversation.control.doubleValue();
        set(
            &self.conversation.readout,
            if seconds < 1.0 {
                "Desactivada".into()
            } else {
                format!("{seconds:.0} s")
            },
        );
    }

    /// Rebuilds the backend popup's entries in place, keeping whichever one
    /// is selected.
    ///
    /// `NSPopUpButton` has no "just change this item's title" call, so this
    /// clears it and adds the labels back — the values behind them, and
    /// their order, never change, only what `crate::ai::detect()` (or a
    /// freshly stored key) has to say about each one.
    fn refresh_ai_backend_labels(&self, detected: Option<&[crate::ai::Detected]>) {
        let providers = crate::ai::providers();
        let labels = ai_backend_labels(providers, detected);
        let current = self.ai_backend.value();
        self.ai_backend.control.removeAllItems();
        for label in &labels {
            self.ai_backend.control.addItemWithTitle(&NSString::from_str(label));
        }
        let index = self.ai_backend.values.iter().position(|value| *value == current).unwrap_or(0);
        self.ai_backend.control.selectItemAtIndex(index as isize);
        self.ai_backend.last.set(index as isize);
    }

    /// Shows and hides the key, model and base-URL controls for whichever
    /// backend the popup names right now, and refreshes their text.
    ///
    /// Called once at construction and again every time the popup changes,
    /// since these fields describe the *selected* backend, not necessarily
    /// the one `[ai] backend` currently has active.
    fn sync_ai_provider_controls(&self) {
        let backend = self.ai_backend.value();
        let providers = crate::ai::providers();
        let info = providers.iter().find(|info| info.id == backend);
        let needs_key = info.is_some_and(|info| info.needs_key);

        self.ai_key_label.setHidden(!needs_key);
        self.ai_key_field.setHidden(!needs_key);
        self.ai_save_key.control.setHidden(!needs_key);
        self.ai_forget_key.control.setHidden(!needs_key);
        self.ai_key_status.setHidden(!needs_key);
        if needs_key {
            self.ai_key_field.setStringValue(&NSString::from_str(""));
            let text = if crate::ai::has_key(&backend) {
                "Hay una clave guardada en el llavero.".to_string()
            } else {
                "Falta la clave.".to_string()
            };
            self.ai_key_status.setStringValue(&NSString::from_str(&text));
        }

        self.ai_model_field.setPlaceholderString(Some(&NSString::from_str(&model_placeholder(
            providers, &backend,
        ))));

        self.ai_codex_hint.setHidden(backend != "codex");

        let base_url = is_openai_compatible_provider(&backend);
        self.ai_base_url_label.setHidden(!base_url);
        self.ai_base_url_field.setHidden(!base_url);
        self.ai_base_url_hint.setHidden(!base_url);
    }

    /// Reads the controls and writes through anything that moved.
    ///
    /// Called from the run loop timer. Returns true when something changed,
    /// so the caller can report it.
    pub fn poll(&self) -> bool {
        let mut changed = false;
        let mut needs_restart = false;

        if let Some(on) = self.sounds.toggled() {
            save("sounds", if on { "true" } else { "false" });
            changed = true;
        }
        if let Some(on) = self.log_voices.toggled() {
            save("log_ignored_speech", if on { "true" } else { "false" });
            changed = true;
        }
        if let Some(on) = self.recordings.toggled() {
            save("save_recordings", if on { "true" } else { "false" });
            changed = true;
        }
        if let Some(on) = self.at_login.toggled() {
            if let Err(e) = startup::set(on) {
                crate::journal::write(&format!("start at login: {e}"));
            }
            changed = true;
        }
        if let Some(on) = self.speak.toggled() {
            save("speak", if on { "true" } else { "false" });
            changed = true;
        }
        // The wake word is read once at startup, so changing it needs a
        // restart — and an empty one would leave nothing to say.
        //
        // Committed when the field is done being edited, not on every poll:
        // typing "casa" through a poll that fires between letters used to
        // save "c", "ca", "cas" and put up a restart dialog for each one.
        // Also settled once the window is no longer the key one: closing it
        // or clicking the menu bar with the field still focused leaves the
        // field editor in place, and the word would otherwise never be
        // saved — a restart from the menu then lost it entirely.
        let entered = self.entered.replace(false);
        let settled =
            entered || !self.is_editing(&self.wake_word) || !self.window.isKeyWindow();
        let typed_wake = self.wake_word.stringValue().to_string();
        let wake_changed = typed_wake.trim() != self.last_wake_word.borrow().trim();
        if settled && wake_changed && !typed_wake.trim().is_empty() {
            let word = crate::text::normalise(&typed_wake);
            // The default carries its own misspellings; anything else is
            // taken as written.
            if word == crate::commands::DEFAULT_WAKE_WORDS[0] {
                save("wake_words", "[]");
            } else {
                save("wake_words", &format!("[{}]", config::toml_string(&word)));
            }
            *self.last_wake_word.borrow_mut() = typed_wake;
            needs_restart = true;
            changed = true;
        }

        // Devices need a restart to take effect: the stream is opened once
        // and the listening loop owns it.
        if let Some(chosen) = self.microphone.changed() {
            save("microphone", &config::toml_string(&chosen.unwrap_or_default()));
            needs_restart = true;
            changed = true;
        }
        if let Some(chosen) = self.speaker.changed() {
            save("speaker", &config::toml_string(&chosen.unwrap_or_default()));
            changed = true;
        }
        if let Some(step) = self.sensitivity.moved() {
            save("threshold", &format!("{:.2}", sensitivity_at(step).1));
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
        // Baked into a plain `bool` for the life of the process — see
        // `hold_mode` in `main.rs` — so this one needs a restart.
        if let Some(mode) = self.listen_mode.changed() {
            save("listen_mode", &config::toml_string(&mode));
            needs_restart = true;
            changed = true;
        }
        // Also read once at startup (`conversation_window` in `main.rs`).
        if let Some(value) = self.conversation.moved() {
            save("conversation_seconds", &format!("{value:.0}"));
            needs_restart = true;
            changed = true;
        }
        // Read fresh every time dictation starts, so this takes effect on
        // the next «minion, empieza a dictar» — no restart needed.
        if let Some(on) = self.spoken_punctuation.toggled() {
            save("spoken_punctuation", if on { "true" } else { "false" });
            changed = true;
        }
        if let Some(on) = self.auto_capitalise.toggled() {
            save("auto_capitalise", if on { "true" } else { "false" });
            changed = true;
        }
        // Read fresh for every answer, timer and blocked command — live.
        if let Some(on) = self.notifications.toggled() {
            save("notifications", if on { "true" } else { "false" });
            changed = true;
        }
        // Read fresh every tick by the HUD itself — no restart needed.
        if let Some(on) = self.show_hud.toggled() {
            save("show_hud", if on { "true" } else { "false" });
            changed = true;
        }
        // Read once at startup into a `OnceLock` (`commands::configure`).
        if let Some(engine) = self.search_engine.changed() {
            save("search_engine", &config::toml_string(&engine));
            needs_restart = true;
            changed = true;
        }
        // Read once at startup (`pause_when_microphone_busy` in `main.rs`),
        // so this one needs the restart notice.
        if let Some(on) = self.pause_when_microphone_busy.toggled() {
            save("pause_when_microphone_busy", if on { "true" } else { "false" });
            needs_restart = true;
            changed = true;
        }

        // The AI layer reads its settings fresh from a `Mutex` on every
        // question (`ai::configure`, called by `save_ai` above), so none
        // of this needs a restart.
        if let Some(on) = self.ai_enabled.toggled() {
            let backend = if on { self.ai_backend.value() } else { String::new() };
            save_ai("backend", &config::toml_string(&backend));
            changed = true;
        }
        if let Some(backend) = self.ai_backend.changed() {
            if self.ai_enabled.on() {
                save_ai("backend", &config::toml_string(&backend));
            }
            self.sync_ai_provider_controls();
            changed = true;
        }
        if self.ai_check.clicked() && !self.ai_detect_running.get() {
            self.ai_detect_running.set(true);
            self.ai_check_status.setStringValue(&NSString::from_str("Comprobando…"));
            let slot = Arc::clone(&self.ai_detect_result);
            std::thread::spawn(move || {
                let detected = crate::ai::detect();
                if let Ok(mut held) = slot.lock() {
                    *held = Some(detected);
                }
            });
            changed = true;
        }
        if let Ok(mut slot) = self.ai_detect_result.lock() {
            if let Some(detected) = slot.take() {
                self.refresh_ai_backend_labels(Some(&detected));
                self.ai_check_status.setStringValue(&NSString::from_str(""));
                self.ai_detect_running.set(false);
                changed = true;
            }
        }
        if self.ai_save_key.clicked() {
            let backend = self.ai_backend.value();
            let key = self.ai_key_field.stringValue().to_string();
            let key = key.trim();
            if key.is_empty() {
                self.ai_key_status.setStringValue(&NSString::from_str("Escribe una clave antes de guardarla."));
            } else {
                match crate::ai::store_key(&backend, key) {
                    Ok(()) => {
                        self.ai_key_field.setStringValue(&NSString::from_str(""));
                        if config::load().ai.api_key != "keychain" {
                            save_ai("api_key", &config::toml_string("keychain"));
                        }
                        self.ai_key_status.setStringValue(&NSString::from_str(&format!(
                            "Clave guardada en el llavero ({}).",
                            chrono::Local::now().format("%d/%m/%Y")
                        )));
                        self.refresh_ai_backend_labels(None);
                    }
                    Err(why) => {
                        self.ai_key_status
                            .setStringValue(&NSString::from_str(&format!("No se pudo guardar: {why}")));
                    }
                }
            }
            changed = true;
        }
        if self.ai_forget_key.clicked() {
            let backend = self.ai_backend.value();
            match crate::ai::forget_key(&backend) {
                Ok(()) => {
                    self.ai_key_status.setStringValue(&NSString::from_str("Se ha olvidado la clave."));
                    self.refresh_ai_backend_labels(None);
                }
                Err(why) => {
                    self.ai_key_status
                        .setStringValue(&NSString::from_str(&format!("No se pudo olvidar: {why}")));
                }
            }
            changed = true;
        }
        if !self.is_editing(&self.ai_model_field) {
            let typed = self.ai_model_field.stringValue().to_string();
            if typed.trim() != self.last_ai_model.borrow().trim() {
                save_ai("model", &config::toml_string(typed.trim()));
                *self.last_ai_model.borrow_mut() = typed;
                changed = true;
            }
        }
        if !self.is_editing(&self.ai_base_url_field) {
            let typed = self.ai_base_url_field.stringValue().to_string();
            if typed.trim() != self.last_ai_base_url.borrow().trim() {
                save_ai("base_url", &config::toml_string(typed.trim()));
                *self.last_ai_base_url.borrow_mut() = typed;
                changed = true;
            }
        }
        if let Some(uses) = self.ai_use.changed() {
            // Already a valid TOML array literal — see the popup's own
            // values, both of them lists of purposes.
            save_ai("use", &uses);
            changed = true;
        }
        // Committed once the field is no longer being edited, the same way
        // `search_engine`'s field would be if it had one — no restart, and
        // no key-watcher: a limit typed wrong just does not parse yet.
        if !self.is_editing(&self.ai_daily_limit) {
            let typed = self.ai_daily_limit.stringValue().to_string();
            if typed.trim() != self.last_ai_daily_limit.borrow().trim() {
                if let Ok(limit) = typed.trim().parse::<u32>() {
                    save_ai("daily_limit", &limit.to_string());
                    *self.last_ai_daily_limit.borrow_mut() = typed;
                    changed = true;
                }
            }
        }
        if self.ai_probe.clicked() && !self.ai_probe_running.get() {
            self.ai_probe_running.set(true);
            self.ai_status.setStringValue(&NSString::from_str("Probando…"));
            let result = Arc::clone(&self.ai_probe_result);
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let outcome =
                    crate::ai::ask("Responde solo con OK.", crate::ai::Purpose::Questions);
                let text = match outcome {
                    Ok(_) => format!("Responde correctamente ({} ms).", started.elapsed().as_millis()),
                    Err(why) => why.to_string(),
                };
                if let Ok(mut slot) = result.lock() {
                    *slot = Some(text);
                }
            });
            changed = true;
        }
        if let Ok(mut slot) = self.ai_probe_result.lock() {
            if let Some(text) = slot.take() {
                self.ai_status.setStringValue(&NSString::from_str(&text));
                self.ai_probe_running.set(false);
                changed = true;
            }
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
        } else if self
            .capture_started
            .get()
            .is_some_and(|since| since.elapsed() >= CAPTURE_TIMEOUT)
        {
            // Nothing was pressed: a button reading «Pulsa la combinación…»
            // for the rest of the session looks broken.
            self.end_capture();
        }

        if self.cancel_train.clicked() {
            self.cancel_requested.set(true);
            self.cancel_train.control.setHidden(true);
            // Not through `show_training`: a cancelled session leaves the
            // list as it was, since nothing was learned.
            self.train_status
                .setStringValue(&NSString::from_str("Entrenamiento cancelado."));
        }
        // Each voice has its own «Olvidar»: forgetting one must not touch
        // the rest, which a single button for "the profile" could not do.
        // Every row is read, not just up to the first click: a `Press` that
        // is not polled keeps the state it was left in and reports the
        // click again on the next tick.
        let mut forgotten: Option<String> = None;
        for row in &self.profiles {
            let clicked = row.forget.clicked();
            let name = row.name.borrow().clone();
            if clicked && forgotten.is_none() && !name.is_empty() {
                forgotten = Some(name);
            }
        }
        if let Some(name) = forgotten {
            self.forget_voice(&name);
            changed = true;
        }
        if self.edit_vocabulary.clicked() {
            self.edit_vocabulary_requested.set(true);
        }

        if self.clear_shortcut.clicked() {
            self.end_capture();
            save("resume_shortcut", &config::toml_string(""));
            self.shortcut_value.borrow_mut().clear();
            self.shortcut.setTitle(&NSString::from_str(&pretty("")));
            changed = true;
        }

        if changed {
            self.update_readouts();
        }
        if needs_restart
            && crate::actions::ask_choice(
                "El cambio se aplica al reiniciar Minion.",
                "Reiniciar ahora",
                "Reiniciar más tarde",
            )
        {
            self.restart_requested.set(true);
        }
        changed
    }

    /// Whether «Reiniciar ahora» was chosen since the last call.
    pub fn take_restart_request(&self) -> bool {
        self.restart_requested.replace(false)
    }

    /// Who the person just asked to train, if they did.
    /// Whether «Editar vocabulario…» was clicked since the last call.
    ///
    /// `main.rs` owns the vocabulary editor window, not this one — see the
    /// note at `edit_vocabulary` — so this only hands the request over.
    pub fn take_edit_vocabulary_request(&self) -> bool {
        self.edit_vocabulary_requested.replace(false)
    }

    /// Whether the person just asked to train their voice.
    ///
    /// Cleared by asking, since only the loop that owns the microphone can
    /// act on it. The name comes from the field beside the button; an empty
    /// one becomes «yo», which is also what the profile of the versions
    /// before names is called.
    pub fn take_training_request(&self) -> Option<String> {
        let clicks = self.train.state();
        if clicks != self.train_clicks.get() {
            self.train_clicks.set(clicks);
            if clicks != 0 {
                self.train_requested.set(true);
            }
        }
        if !self.train_requested.replace(false) {
            return None;
        }
        let typed = self.new_name.stringValue().to_string();
        let name = crate::speaker::tidy_name(&typed);
        self.new_name.setStringValue(&NSString::from_str(&name));
        Some(name)
    }

    /// Shows how training is going.
    pub fn show_training(&self, message: &str, finished: bool) {
        self.train_status.setStringValue(&NSString::from_str(message));
        // The way out is only offered while there is something to get out
        // of: five phrases is long enough to change your mind.
        self.cancel_train.control.setHidden(finished);
        if finished {
            // A voice may have just joined the list, so read it again
            // rather than assuming what is in it.
            self.show_profiles();
        }
    }

    /// Fills the list of voices in from what is on disk.
    ///
    /// There are only so many rows — see [`MAX_VOICE_ROWS`] — and the rest,
    /// if a machine ever has that many people on it, are managed by
    /// deleting a file in `voices/`.
    fn show_profiles(&self) {
        let enrolled = crate::speaker::profile_names();
        for (index, row) in self.profiles.iter().enumerate() {
            row.show(enrolled.get(index).map(String::as_str));
        }
        if enrolled.len() > self.profiles.len() {
            crate::journal::write(&format!(
                "{} voices enrolled, more than the settings window shows",
                enrolled.len()
            ));
        }
    }

    /// Whether the person just asked to stop training.
    ///
    /// Cleared by asking, like the training request: only the loop that
    /// owns the microphone can end the session.
    ///
    /// Waiting to be read by the run loop timer in `main.rs`, next to
    /// `take_training_request`; until it is, the button only clears the
    /// window's own prompt.
    pub fn take_cancel_request(&self) -> bool {
        self.cancel_requested.replace(false)
    }

    /// Deletes one voice profile, once.
    ///
    /// Asked about first: it is the one setting here that cannot be undone
    /// without saying five phrases again. Forgetting the last one leaves
    /// Minion obeying anybody again, so that case says so.
    fn forget_voice(&self, name: &str) {
        if !crate::actions::ask(
            &format!("¿Olvidar la voz de {name}? Habrá que volver a entrenarla."),
            "Olvidar",
        ) {
            return;
        }
        match crate::speaker::forget_profile(name) {
            Ok(()) => {
                crate::journal::write(&format!(
                    "voice profile «{name}» deleted from the settings window"
                ));
                self.show_profiles();
                let left = crate::speaker::profile_names();
                self.train_status.setStringValue(&NSString::from_str(
                    &if left.is_empty() {
                        "Voz olvidada. Reinicia Minion: volverá a obedecer a \
                         cualquiera que diga la palabra clave."
                            .to_string()
                    } else {
                        format!("{name} olvidada. Reinicia Minion para que deje de reconocerla.")
                    },
                ));
            }
            Err(e) => {
                crate::journal::write(&format!("could not delete the voice profile: {e}"));
                self.train_status.setStringValue(&NSString::from_str(
                    "No se pudo borrar el perfil de voz.",
                ));
            }
        }
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
        self.capture_started.set(Some(std::time::Instant::now()));
        self.shortcut
            .setTitle(&NSString::from_str("Pulsa la combinación…"));
    }

    /// Stops waiting and puts the shortcut in the button again.
    fn end_capture(&self) {
        self.capturing.set(false);
        self.capture_started.set(None);
        self.button_clicks.set(self.shortcut.state());
        let current = self.shortcut_value.borrow().clone();
        self.shortcut.setTitle(&NSString::from_str(&pretty(&current)));
    }

    /// Called from the run loop with whatever key was pressed, if capturing.
    ///
    /// Escape gets out of it, and a bare key is refused: without a modifier
    /// the shortcut is a letter, and then every «a» typed anywhere on the
    /// machine pauses Minion. Function keys are the exception, since they
    /// carry no character of their own.
    pub fn capture(&self, code: u16, mods: crate::actions::Mods) -> bool {
        if !self.capturing.get() {
            return false;
        }
        if code == ESCAPE && mods == crate::actions::Mods::NONE {
            self.end_capture();
            return true;
        }

        let named = crate::actions::shortcut_text(code, mods);
        let Some(text) = named else {
            // A key with no name: keep waiting for one that has one.
            return true;
        };
        if mods == crate::actions::Mods::NONE && !is_function_key(code) {
            self.capture_started.set(Some(std::time::Instant::now()));
            self.shortcut
                .setTitle(&NSString::from_str("Añade ⌘, ⌥ o ⌃"));
            return true;
        }

        self.capturing.set(false);
        self.capture_started.set(None);
        self.button_clicks.set(self.shortcut.state());
        self.shortcut.setTitle(&NSString::from_str(&pretty(&text)));
        save("resume_shortcut", &config::toml_string(&text));
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

    pub fn show_hud_on(&self) -> bool {
        self.show_hud.on()
    }
}

/// Gives the application the menu its windows need, once.
///
/// An accessory application shows no menu bar, so Minion had none at all —
/// and with no menu there is nothing for ⌘C, ⌘V or ⌘W to go through:
/// AppKit routes a key equivalent by looking for it in `mainMenu` first.
/// The result was a text field that could not be pasted into. The items
/// are the standard responder actions, so whatever has focus answers them.
pub(crate) fn install_main_menu(mtm: MainThreadMarker) {
    use objc2::sel;
    use objc2_app_kit::{NSMenu, NSMenuItem};

    let app = NSApplication::sharedApplication(mtm);
    if app.mainMenu().is_some() {
        return;
    }

    /// A menu item: what it says, what it does, its key equivalent, and
    /// the tag the action reads (only the text finder uses one).
    type Item<'a> = (&'a str, objc2::runtime::Sel, &'a str, isize);

    /// `NSTextFinderActionShowFindInterface`: open the find bar.
    const SHOW_FIND: isize = 1;

    let sections: [(&str, &[Item]); 2] = [
        (
            "Edición",
            &[
                ("Deshacer", sel!(undo:), "z", 0),
                ("Cortar", sel!(cut:), "x", 0),
                ("Copiar", sel!(copy:), "c", 0),
                ("Pegar", sel!(paste:), "v", 0),
                ("Seleccionar todo", sel!(selectAll:), "a", 0),
                ("Buscar…", sel!(performTextFinderAction:), "f", SHOW_FIND),
            ],
        ),
        ("Ventana", &[("Cerrar", sel!(performClose:), "w", 0)]),
    ];

    let bar = NSMenu::new(mtm);
    // AppKit treats the first submenu as the application menu whatever is
    // in it, so an empty one goes first and the real menus keep their
    // names — an accessory application never draws them, but the key
    // equivalents are searched in every menu, including this one.
    let application = NSMenuItem::new(mtm);
    application.setSubmenu(Some(&NSMenu::new(mtm)));
    bar.addItem(&application);
    for (title, items) in sections {
        let menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(title));
        for (name, action, key, tag) in items {
            // Safety: the selectors are the standard responder ones; with
            // no target set they travel up the responder chain, so an item
            // nothing answers is simply greyed out.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    mtm.alloc(),
                    &NSString::from_str(name),
                    Some(*action),
                    &NSString::from_str(key),
                )
            };
            item.setTag(*tag);
            menu.addItem(&item);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        bar.addItem(&holder);
    }
    app.setMainMenu(Some(&bar));
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

/// Saves a key in `[ai]`, and reloads `ai::configure` so it takes effect
/// on the very next question — the AI layer, unlike most of this window,
/// reads its settings fresh from a `Mutex` rather than once at startup,
/// which is exactly what `ai::configure`'s own doc comment says it is for.
fn save_ai(key: &str, value: &str) {
    if let Err(e) = config::set_table_option("ai", key, value) {
        crate::journal::write(&format!("could not save ai.{key}: {e}"));
    } else {
        crate::journal::write(&format!("ai.{key} = {value}"));
        crate::ai::configure(&config::load());
    }
}

/// Watches this application's own key presses, for capturing a shortcut.
///
/// A local monitor, unlike the global watcher in `hotkey`: it only sees
/// events already aimed at Minion, so it cannot interfere with anything
/// else, and the preferences window has focus while it is open.
pub fn capture_keys<F>(handle: F) -> KeyCapture
where
    F: Fn(u16, crate::actions::Mods) -> bool + 'static,
{
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

    let handler = block2::RcBlock::new(
        move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
            let borrowed = unsafe { event.as_ref() };
            let flags = borrowed.modifierFlags();
            let mods = crate::actions::Mods {
                command: flags.contains(NSEventModifierFlags::Command),
                shift: flags.contains(NSEventModifierFlags::Shift),
                option: flags.contains(NSEventModifierFlags::Option),
                control: flags.contains(NSEventModifierFlags::Control),
            };
            if handle(borrowed.keyCode(), mods) {
                return std::ptr::null_mut(); // taken: do not pass it on
            }
            event.as_ptr()
        },
    );
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
    };
    KeyCapture { _monitor: monitor, _handler: handler }
}

pub struct KeyCapture {
    _monitor: Option<Retained<objc2::runtime::AnyObject>>,
    _handler: block2::RcBlock<dyn Fn(std::ptr::NonNull<objc2_app_kit::NSEvent>) -> *mut objc2_app_kit::NSEvent>,
}

/// A window showing text, with the usual close button.
///
/// The report used to appear in an AppleScript dialog, which has no window
/// controls and blocks everything behind it until dismissed. A report is
/// something to read next to the log, not a demand for attention.
pub struct Report {
    window: Retained<NSWindow>,
    text: Retained<NSTextView>,
}

impl Report {
    pub fn new(mtm: MainThreadMarker, title: &str, size: NSSize) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
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
        window.setTitle(&NSString::from_str(title));
        unsafe { window.setReleasedWhenClosed(false) };
        window.center();

        // A text view rather than a label: a thousand phrases are there to
        // be searched and copied, and only a text view brings ⌘F, a
        // selection and the standard Edit menu with it.
        let inner = NSSize::new(size.width - MARGIN * 2.0, size.height - MARGIN * 2.0);
        let text = NSTextView::new(mtm);
        text.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), inner));
        text.setEditable(false);
        text.setSelectable(true);
        text.setRichText(false);
        text.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(11.0, 0.0)));
        // Grows downwards inside the scroll view and never sideways, so
        // the lines wrap instead of running off the right edge.
        text.setVerticallyResizable(true);
        text.setHorizontallyResizable(false);
        text.setMinSize(NSSize::new(0.0, 0.0));
        text.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
        text.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        // Safety: reading the text view's own container, which exists for
        // a view built the ordinary way.
        if let Some(container) = unsafe { text.textContainer() } {
            container.setWidthTracksTextView(true);
            container.setContainerSize(NSSize::new(inner.width, f64::MAX));
        }
        text.setUsesFindBar(true);
        text.setIncrementalSearchingEnabled(true);

        let scroll = NSScrollView::new(mtm);
        scroll.setFrame(NSRect::new(NSPoint::new(MARGIN, MARGIN), inner));
        scroll.setHasVerticalScroller(true);
        scroll.setDocumentView(Some(&text));
        if let Some(content) = window.contentView() {
            content.addSubview(&scroll);
        }
        Self { window, text }
    }

    pub fn show(&self, body: &str) {
        self.text.setString(&NSString::from_str(body));
        if let Some(mtm) = MainThreadMarker::new() {
            install_main_menu(mtm);
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
    fn a_long_hint_gets_more_than_one_line() {
        // A fixed height is what clipped these; the count has to follow
        // the text.
        let short = wrapped_lines("Sonido al ejecutar.", 336.0);
        let long = wrapped_lines(
            "Con el micrófono abierto se transcribe todo lo que se habla \
             cerca. Normalmente solo se cuenta cuánto se oyó, no qué se dijo.",
            336.0,
        );
        assert_eq!(short, 1.0);
        assert!(long >= 2.0, "a two-line hint needs two lines, got {long}");
    }

    #[test]
    fn a_narrower_hint_needs_more_lines() {
        let text = "Toda orden empieza por ella. Elige algo que no digas por casualidad.";
        assert!(wrapped_lines(text, 200.0) > wrapped_lines(text, 400.0));
    }

    #[test]
    fn only_function_keys_stand_alone() {
        // Everything else needs a modifier, or typing pauses Minion.
        assert!(is_function_key(crate::actions::parse_shortcut("f5").unwrap().0));
        assert!(is_function_key(crate::actions::parse_shortcut("f12").unwrap().0));
        assert!(!is_function_key(crate::actions::key::A));
        assert!(!is_function_key(crate::actions::key::SPACE));
    }

    #[test]
    fn no_shortcut_reads_as_none() {
        assert_eq!(pretty(""), "Ninguno");
    }

    #[test]
    fn spacing_is_ordered_from_tight_to_loose() {
        // The rhythm only reads as deliberate if the distances rank the
        // way the relationships do: a hint clings to what it explains, and
        // groups stand furthest apart.
        assert!(spacing::BEFORE_HINT < spacing::SIBLING);
        assert!(spacing::SIBLING < spacing::AFTER_HINT);
        assert!(spacing::AFTER_HINT < spacing::GROUP);
        assert!(spacing::AFTER_LABEL < spacing::AFTER_HEADING);
    }

    #[test]
    fn every_slider_position_has_its_own_name() {
        // Two positions showing the same word means part of the slider
        // does nothing a person can see.
        let mut seen = std::collections::HashSet::new();
        for step in 0..SENSITIVITY.len() {
            let (name, _) = sensitivity_at(step as f64);
            assert!(seen.insert(name), "«{name}» appears at more than one position");
        }
    }

    #[test]
    fn sensitivity_rises_along_the_slider() {
        // Left is more willing to act, right is more cautious.
        for pair in SENSITIVITY.windows(2) {
            assert!(pair[0].1 < pair[1].1, "the values should climb");
        }
    }

    #[test]
    fn a_stored_value_finds_its_position() {
        for (index, (_, value)) in SENSITIVITY.iter().enumerate() {
            assert_eq!(sensitivity_step(*value), index as f64);
        }
        // And something in between lands on the nearest.
        assert_eq!(sensitivity_step(0.71), 2.0);
    }

    fn detected(path: Option<&str>, authenticated: bool, problem: Option<&str>) -> crate::ai::Detected {
        crate::ai::Detected {
            backend: "claude-code",
            label: "Claude Code",
            path: path.map(str::to_string),
            authenticated,
            problem: problem.map(str::to_string),
            latency: Some(std::time::Duration::from_millis(2_900)),
        }
    }

    #[test]
    fn a_duration_reads_with_a_spanish_comma() {
        assert_eq!(spanish_seconds(2.9), "2,9");
        assert_eq!(spanish_seconds(0.0), "0,0");
    }

    #[test]
    fn a_cli_agent_not_yet_probed_says_so() {
        assert_eq!(provider_state_text(true, false, false, None), "comprobando…");
    }

    #[test]
    fn a_cli_agent_not_on_this_mac_is_not_available() {
        let found = detected(None, false, None);
        assert_eq!(provider_state_text(true, false, false, Some(&found)), "no disponible");
    }

    #[test]
    fn a_cli_agent_that_answered_is_ready_with_its_time() {
        let found = detected(Some("/opt/homebrew/bin/claude"), true, None);
        assert_eq!(provider_state_text(true, false, false, Some(&found)), "listo (2,9 s)");
    }

    #[test]
    fn a_cli_agent_installed_but_refusing_names_the_reason() {
        let found = detected(
            Some("/opt/homebrew/bin/codex"),
            false,
            Some("is not supported when using Codex with a ChatGPT account"),
        );
        assert_eq!(
            provider_state_text(true, false, false, Some(&found)),
            "instalado, is not supported when using Codex with a ChatGPT account"
        );
    }

    #[test]
    fn an_http_provider_needing_no_key_just_says_so() {
        assert_eq!(provider_state_text(false, false, false, None), "sin clave");
        // A stray key would still be ignored: no key is ever needed.
        assert_eq!(provider_state_text(false, false, true, None), "sin clave");
    }

    #[test]
    fn an_http_provider_reports_whether_a_key_is_on_file() {
        assert_eq!(provider_state_text(false, true, false, None), "falta la clave");
        assert_eq!(provider_state_text(false, true, true, None), "clave guardada");
    }

    #[test]
    fn every_provider_appears_once_in_the_popup_labels() {
        let providers = crate::ai::providers();
        let labels = ai_backend_labels(providers, None);
        assert_eq!(labels.len(), providers.len());
        for (label, info) in labels.iter().zip(providers.iter()) {
            assert!(label.starts_with(info.label), "«{label}» should start with «{}»", info.label);
        }
    }

    #[test]
    fn a_cli_backend_is_recognised_and_an_http_one_is_not() {
        assert!(is_cli_provider("claude-code"));
        assert!(is_cli_provider("codex"));
        assert!(!is_cli_provider("openai"));
        assert!(!is_cli_provider("ollama"));
    }

    #[test]
    fn only_openai_compatible_presets_get_a_base_url_field() {
        assert!(is_openai_compatible_provider("ollama"));
        assert!(is_openai_compatible_provider("openai"));
        assert!(!is_openai_compatible_provider("anthropic"));
        assert!(!is_openai_compatible_provider("claude-code"));
    }

    #[test]
    fn the_model_placeholder_names_the_backend_default_and_a_cli_agent_has_none() {
        let providers = crate::ai::providers();
        assert_eq!(model_placeholder(providers, "openai"), "por defecto: gpt-4o-mini");
        assert_eq!(model_placeholder(providers, "claude-code"), "");
    }
}

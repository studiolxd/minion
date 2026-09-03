//! The vocabulary editor — «Vocabulario…» in Ajustes → «Editar vocabulario…».
//!
//! A second window, built the same way `preferences.rs` is: AppKit
//! directly, controls polled from the run loop timer rather than wired to
//! an Objective-C target (see the note at the top of that file for why),
//! and [`crate::preferences::Layout`] for placement.
//!
//! What it edits is always this file's own `[[apps]]`, `[[commands]]` and
//! `[[aliases]]` — through the pure functions in `config.rs`, the same
//! ones `preferences.rs` and `learn.rs` already write through — never the
//! built-in vocabulary or a downloaded pack, which stay read-only here.
//!
//! There is no `NSTableView` in this window, on purpose. Every control in
//! this codebase is read by polling rather than driven by an Objective-C
//! target/action or a delegate — `preferences.rs` explains why at its top
//! — and a real, editable `NSTableView` needs exactly the kind of
//! delegate/data-source class this project has never declared. The list
//! it would have shown is read-only anyway (only «Añadir…» and «Olvidar»
//! change anything), so it is built instead from the same `Layout` rows
//! `preferences.rs` already uses, inside a plain `NSScrollView` — visually
//! a table, technically a stack of labels. Adding an entry uses a blocking
//! `NSAlert` with an accessory view, the same pattern `actions::ask`
//! already relies on for "¿Olvidar tu voz?": short-lived, modal, and
//! nothing to poll while it is open.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSBackingStoreType, NSButton, NSFont,
    NSLineBreakMode, NSPopUpButton, NSScrollView, NSTabView, NSTabViewItem, NSTextField, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::preferences::{self, spacing, Layout, Press};
use crate::{commands, config};

const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 460.0;
/// Height of one row: name, bundle id, phrase — all single-line.
const ROW: f64 = 20.0;
/// Width of the trailing «Olvidar» button.
const FORGET_WIDTH: f64 = 66.0;

/// What one row's «Olvidar» button removes, once clicked.
enum RowTarget {
    App(String),
    Command(String),
    Alias(String),
}

/// A row with a button to remove it — only user-added rows have one.
struct Row {
    button: Press,
    target: RowTarget,
}

/// One tab's scroll view and the rows currently shown in it.
struct Tab {
    scroll: Retained<NSScrollView>,
    width: f64,
    rows: RefCell<Vec<Row>>,
    add: RefCell<Press>,
}

pub struct VocabularyEditor {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    apps: Tab,
    commands: Tab,
    aliases: Tab,
    restart_requested: Cell<bool>,
}

/// A single-line label that truncates rather than wraps — for a table
/// cell, unlike `preferences::small_label`/`plain_label`, which wrap on
/// purpose for a hint meant to be read whole.
fn cell(mtm: MainThreadMarker, text: &str, frame: NSRect, bold_header: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFrame(frame);
    field.setUsesSingleLineMode(true);
    field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    field.setMaximumNumberOfLines(1);
    field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    if bold_header {
        field.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    }
    field
}

/// A frame for one column of a row, `x` and `width` measured from the
/// row's own frame.
fn column(row: &NSRect, x: f64, width: f64) -> NSRect {
    NSRect::new(NSPoint::new(row.origin.x + x, row.origin.y), NSSize::new(width, row.size.height))
}

/// Lays out a table row across the widths given, adding a trailing
/// «Olvidar» button when `removable` is true. Returns that button.
fn table_row(
    layout: &mut Layout,
    mtm: MainThreadMarker,
    columns: &[(&str, f64)],
    removable: bool,
    header: bool,
) -> Option<Retained<NSButton>> {
    let frame = layout.place(ROW, 0.0);
    let mut x = 0.0;
    for (text, width) in columns {
        let view = cell(mtm, text, column(&frame, x, *width), header);
        layout.add(&view);
        x += width + spacing::SIBLING;
    }
    if header {
        layout.gap(spacing::SIBLING);
        return None;
    }
    if !removable {
        layout.gap(4.0);
        return None;
    }
    // Safety: no target and no action — see the note at the top of this
    // file and `preferences.rs`.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("Olvidar"), None, None, mtm)
    };
    button.setFrame(column(&frame, x, FORGET_WIDTH));
    button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    layout.add_control(&button, "Olvidar");
    layout.gap(4.0);
    Some(button)
}

/// The «Añadir…» button at the bottom of a tab.
fn add_button(layout: &mut Layout, mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
    layout.gap(spacing::GROUP - 4.0);
    // Safety: no target and no action.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    let frame = layout.place(spacing::BUTTON, 0.0);
    button.setFrame(preferences::narrow(frame, 220.0));
    layout.add_control(&button, title);
    button
}

/// Wraps a canvas built by [`Layout`] in a scroll view sized to the tab.
fn scrolled(mtm: MainThreadMarker, layout: Layout, width: f64, height: f64) -> Retained<NSScrollView> {
    let (canvas, _content_height) = layout.finish();
    let scroll = NSScrollView::new(mtm);
    scroll.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)));
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    scroll.setDocumentView(Some(&canvas));
    scroll
}

/// A tab's whole content view, built fresh — see [`VocabularyEditor::rebuild`].
fn tab_item(mtm: MainThreadMarker, label: &str, view: &NSView) -> Retained<NSTabViewItem> {
    // Safety: `initWithIdentifier` accepts any object as an opaque
    // identifier; `None` means AppKit will not look one up by it, which is
    // fine — the tabs are only ever addressed by index.
    let item = unsafe { NSTabViewItem::initWithIdentifier(mtm.alloc(), None) };
    item.setLabel(&NSString::from_str(label));
    item.setView(Some(view));
    item
}

impl VocabularyEditor {
    pub fn new(mtm: MainThreadMarker) -> Rc<Self> {
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("Vocabulario"));
        // Safety: kept alive by this struct for the life of the process —
        // see the identical note in `preferences::Preferences::new`.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setMinSize(NSSize::new(480.0, 320.0));
        window.center();

        let tab_view = NSTabView::new(mtm);
        let inner = HEIGHT - 40.0;
        tab_view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)));
        if let Some(content) = window.contentView() {
            content.addSubview(&tab_view);
        }

        let apps_layout = Layout::new(mtm, WIDTH - 20.0);
        let commands_layout = Layout::new(mtm, WIDTH - 20.0);
        let aliases_layout = Layout::new(mtm, WIDTH - 20.0);

        let apps_scroll = scrolled(mtm, apps_layout, WIDTH - 20.0, inner - 20.0);
        let commands_scroll = scrolled(mtm, commands_layout, WIDTH - 20.0, inner - 20.0);
        let aliases_scroll = scrolled(mtm, aliases_layout, WIDTH - 20.0, inner - 20.0);

        tab_view.addTabViewItem(&tab_item(mtm, "Aplicaciones", &apps_scroll));
        tab_view.addTabViewItem(&tab_item(mtm, "Órdenes", &commands_scroll));
        tab_view.addTabViewItem(&tab_item(mtm, "Alias", &aliases_scroll));

        let editor = Rc::new(Self {
            mtm,
            window,
            apps: Tab {
                scroll: apps_scroll,
                width: WIDTH - 20.0,
                rows: RefCell::new(Vec::new()),
                add: RefCell::new(Press::new(placeholder_button(mtm))),
            },
            commands: Tab {
                scroll: commands_scroll,
                width: WIDTH - 20.0,
                rows: RefCell::new(Vec::new()),
                add: RefCell::new(Press::new(placeholder_button(mtm))),
            },
            aliases: Tab {
                scroll: aliases_scroll,
                width: WIDTH - 20.0,
                rows: RefCell::new(Vec::new()),
                add: RefCell::new(Press::new(placeholder_button(mtm))),
            },
            restart_requested: Cell::new(false),
        });
        editor.rebuild();
        editor
    }

    pub fn show(&self) {
        self.rebuild();
        if let Some(mtm) = MainThreadMarker::new() {
            preferences::install_main_menu(mtm);
            let app = NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Whether «Reiniciar ahora» was chosen since the last call.
    pub fn take_restart_request(&self) -> bool {
        self.restart_requested.replace(false)
    }

    /// Rebuilds all three tabs from the vocabulary as it stands right now.
    ///
    /// Simplest way to keep the list honest after an add or a remove:
    /// diffing the rows against what changed is what `main.rs` already
    /// decided against for "Últimas órdenes", for the same reason — a
    /// handful of rows, rebuilt a few times a session, costs nothing.
    fn rebuild(&self) {
        self.rebuild_apps();
        self.rebuild_commands();
        self.rebuild_aliases();
    }

    fn rebuild_apps(&self) {
        let mtm = self.mtm;
        let mut layout = Layout::new(mtm, self.apps.width);
        let w = layout.content_width() - FORGET_WIDTH - spacing::SIBLING * 4.0;
        let widths = [w * 0.26, w * 0.24, w * 0.34, w * 0.16];
        layout.heading("Aplicaciones");
        table_row(
            &mut layout,
            mtm,
            &[
                ("Nombre", widths[0]),
                ("Bundle ID", widths[1]),
                ("Alias", widths[2]),
                ("Origen", widths[3]),
            ],
            false,
            true,
        );

        let mut apps: Vec<_> = commands::vocabulary().apps.iter().collect();
        apps.sort_by_key(|a| a.name);
        let mut rows = Vec::new();
        for app in apps {
            let is_user = app.category == crate::vocabulary::USER_CATEGORY;
            let source = if is_user { "Tuyo" } else { "Minion" };
            let button = table_row(
                &mut layout,
                mtm,
                &[
                    (app.name, widths[0]),
                    (app.bundle_id, widths[1]),
                    (&app.aliases.join(", "), widths[2]),
                    (source, widths[3]),
                ],
                is_user,
                false,
            );
            if let Some(button) = button {
                rows.push(Row { button: Press::new(button), target: RowTarget::App(app.bundle_id.to_string()) });
            }
        }
        let add = add_button(&mut layout, mtm, "Añadir aplicación…");
        *self.apps.rows.borrow_mut() = rows;
        *self.apps.add.borrow_mut() = Press::new(add);
        self.apps.scroll.setDocumentView(Some(&layout.finish().0));
    }

    fn rebuild_commands(&self) {
        let mtm = self.mtm;
        let mut layout = Layout::new(mtm, self.commands.width);
        let w = layout.content_width() - FORGET_WIDTH - spacing::SIBLING * 4.0;
        let widths = [w * 0.20, w * 0.32, w * 0.30, w * 0.18];
        layout.heading("Órdenes");
        table_row(
            &mut layout,
            mtm,
            &[
                ("Nombre", widths[0]),
                ("Frases", widths[1]),
                ("Acción", widths[2]),
                ("Categoría", widths[3]),
            ],
            false,
            true,
        );

        let mut all: Vec<(&str, String, String, &str, bool)> = Vec::new();
        for command in &commands::vocabulary().commands {
            let is_user = command.category == crate::vocabulary::USER_CATEGORY;
            all.push((
                command.name,
                command.phrases.join(", "),
                describe_action(&command.action),
                command.category,
                is_user,
            ));
        }
        for command in &commands::vocabulary().contextual {
            all.push((
                command.name,
                command.phrases.join(", "),
                describe_action(&command.action),
                command.category,
                false,
            ));
        }
        all.sort_by(|a, b| a.0.cmp(b.0));

        let mut rows = Vec::new();
        for (name, phrases, action, category, is_user) in all {
            let button = table_row(
                &mut layout,
                mtm,
                &[(name, widths[0]), (&phrases, widths[1]), (&action, widths[2]), (category, widths[3])],
                is_user,
                false,
            );
            if let Some(button) = button {
                rows.push(Row { button: Press::new(button), target: RowTarget::Command(name.to_string()) });
            }
        }
        let add = add_button(&mut layout, mtm, "Añadir orden…");
        *self.commands.rows.borrow_mut() = rows;
        *self.commands.add.borrow_mut() = Press::new(add);
        self.commands.scroll.setDocumentView(Some(&layout.finish().0));
    }

    fn rebuild_aliases(&self) {
        let mtm = self.mtm;
        let mut layout = Layout::new(mtm, self.aliases.width);
        let w = layout.content_width() - FORGET_WIDTH - spacing::SIBLING * 2.0;
        let widths = [w * 0.55, w * 0.45];
        layout.heading("Alias");
        table_row(&mut layout, mtm, &[("Frase", widths[0]), ("Orden", widths[1])], false, true);

        let settings = config::load();
        let mut rows = Vec::new();
        for alias in &settings.aliases {
            let button = table_row(
                &mut layout,
                mtm,
                &[(alias.phrase.as_str(), widths[0]), (alias.command.as_str(), widths[1])],
                true,
                false,
            );
            if let Some(button) = button {
                rows.push(Row {
                    button: Press::new(button),
                    target: RowTarget::Alias(alias.phrase.clone()),
                });
            }
        }
        let add = add_button(&mut layout, mtm, "Añadir alias…");
        *self.aliases.rows.borrow_mut() = rows;
        *self.aliases.add.borrow_mut() = Press::new(add);
        self.aliases.scroll.setDocumentView(Some(&layout.finish().0));
    }

    /// Reads every button, acts on whatever was clicked, and rebuilds if
    /// anything changed. Called from the run loop timer, like
    /// `preferences::Preferences::poll`.
    pub fn poll(&self) -> bool {
        let mut changed = false;
        let mut needs_restart = false;

        for target in clicked_targets(&self.apps.rows) {
            if let RowTarget::App(bundle_id) = target {
                if crate::actions::ask("¿Olvidar esta aplicación?", "Olvidar") {
                    match config::remove_app(&bundle_id) {
                        Ok(()) => {
                            changed = true;
                            needs_restart = true;
                        }
                        Err(e) => crate::journal::write(&format!("could not forget app: {e}")),
                    }
                }
            }
        }
        for target in clicked_targets(&self.commands.rows) {
            if let RowTarget::Command(name) = target {
                if crate::actions::ask("¿Olvidar esta orden?", "Olvidar") {
                    match config::remove_command(&name) {
                        Ok(()) => {
                            changed = true;
                            needs_restart = true;
                        }
                        Err(e) => crate::journal::write(&format!("could not forget command: {e}")),
                    }
                }
            }
        }
        for target in clicked_targets(&self.aliases.rows) {
            if let RowTarget::Alias(phrase) = target {
                if crate::actions::ask("¿Olvidar este alias?", "Olvidar") {
                    match config::remove_alias(&phrase) {
                        Ok(()) => {
                            changed = true;
                            needs_restart = true;
                        }
                        Err(e) => crate::journal::write(&format!("could not forget alias: {e}")),
                    }
                }
            }
        }

        if self.apps.add.borrow().clicked() && self.add_app() {
            changed = true;
            needs_restart = true;
        }
        if self.commands.add.borrow().clicked() && self.add_command() {
            changed = true;
            needs_restart = true;
        }
        if self.aliases.add.borrow().clicked() && self.add_alias() {
            changed = true;
            needs_restart = true;
        }

        if changed {
            self.rebuild();
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

    /// «Añadir aplicación…»: a modal alert with a popup of what is
    /// installed and a field for extra aliases. True if something was
    /// added.
    fn add_app(&self) -> bool {
        let mtm = self.mtm;
        let apps = installed_apps();

        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Añadir aplicación"));
        alert.setInformativeText(&NSString::from_str(
            "Elige una aplicación de /Applications y, si quieres, cómo la llamas al hablar \
             (separadas por comas).",
        ));
        alert.addButtonWithTitle(&NSString::from_str("Añadir"));
        alert.addButtonWithTitle(&NSString::from_str("Cancelar"));

        let accessory = NSView::new(mtm);
        accessory.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 60.0)));

        let picker = NSPopUpButton::new(mtm);
        picker.setFrame(NSRect::new(NSPoint::new(0.0, 34.0), NSSize::new(320.0, 26.0)));
        if apps.is_empty() {
            picker.addItemWithTitle(&NSString::from_str("No se encontró ninguna aplicación"));
            picker.setEnabled(false);
        }
        for (name, bundle_id) in &apps {
            picker.addItemWithTitle(&NSString::from_str(&format!("{name}  —  {bundle_id}")));
        }
        accessory.addSubview(&picker);

        let aliases_field = NSTextField::new(mtm);
        aliases_field.setFrame(NSRect::new(NSPoint::new(0.0, 4.0), NSSize::new(320.0, 24.0)));
        aliases_field.setPlaceholderString(Some(&NSString::from_str("Alias: cromo, croma")));
        accessory.addSubview(&aliases_field);

        alert.setAccessoryView(Some(&accessory));

        if alert.runModal() != NSAlertFirstButtonReturn || apps.is_empty() {
            return false;
        }
        let index = picker.indexOfSelectedItem().max(0) as usize;
        let Some((name, bundle_id)) = apps.get(index).cloned() else {
            return false;
        };
        let aliases = split_list(&aliases_field.stringValue().to_string());
        match config::add_app(&name, &bundle_id, &aliases) {
            Ok(()) => true,
            Err(e) => {
                crate::actions::show_message(&format!("No se pudo añadir la aplicación: {e}"));
                false
            }
        }
    }

    /// «Añadir orden…»: a name, the ways of saying it, what kind of thing
    /// it does, and the value that goes with that kind.
    fn add_command(&self) -> bool {
        let mtm = self.mtm;

        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Añadir orden"));
        alert.setInformativeText(&NSString::from_str(
            "Un nombre, cómo decirla (separadas por comas) y qué hace.",
        ));
        alert.addButtonWithTitle(&NSString::from_str("Añadir"));
        alert.addButtonWithTitle(&NSString::from_str("Cancelar"));

        let accessory = NSView::new(mtm);
        accessory.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(340.0, 118.0)));

        let name_field = NSTextField::new(mtm);
        name_field.setFrame(NSRect::new(NSPoint::new(0.0, 92.0), NSSize::new(340.0, 24.0)));
        name_field.setPlaceholderString(Some(&NSString::from_str("Nombre: compilar")));
        accessory.addSubview(&name_field);

        let phrases_field = NSTextField::new(mtm);
        phrases_field.setFrame(NSRect::new(NSPoint::new(0.0, 62.0), NSSize::new(340.0, 24.0)));
        phrases_field.setPlaceholderString(Some(&NSString::from_str("Frases: compila, compila el proyecto")));
        accessory.addSubview(&phrases_field);

        let kind = NSPopUpButton::new(mtm);
        kind.setFrame(NSRect::new(NSPoint::new(0.0, 32.0), NSSize::new(340.0, 26.0)));
        for title in ["Atajo de teclado", "Escribir texto", "Abrir una URL"] {
            kind.addItemWithTitle(&NSString::from_str(title));
        }
        accessory.addSubview(&kind);

        let value_field = NSTextField::new(mtm);
        value_field.setFrame(NSRect::new(NSPoint::new(0.0, 2.0), NSSize::new(340.0, 24.0)));
        value_field.setPlaceholderString(Some(&NSString::from_str(
            "Valor: cmd-shift-b · el texto a escribir · la URL",
        )));
        accessory.addSubview(&value_field);

        alert.setAccessoryView(Some(&accessory));

        if alert.runModal() != NSAlertFirstButtonReturn {
            return false;
        }

        let name = name_field.stringValue().to_string().trim().to_string();
        let phrases = split_list(&phrases_field.stringValue().to_string());
        let value = value_field.stringValue().to_string().trim().to_string();
        if name.is_empty() || phrases.is_empty() || value.is_empty() {
            crate::actions::show_message("Hace falta un nombre, al menos una frase y un valor.");
            return false;
        }

        let kind = match kind.indexOfSelectedItem() {
            1 => config::CommandKind::Text(value),
            2 => config::CommandKind::Url(value),
            _ => {
                if crate::actions::parse_shortcut(&value).is_none() {
                    crate::actions::show_message(&format!(
                        "«{value}» no se reconoce como atajo de teclado, por ejemplo «cmd-shift-b»."
                    ));
                    return false;
                }
                config::CommandKind::Keys(value)
            }
        };

        match config::add_command(&name, &phrases, &kind) {
            Ok(()) => true,
            Err(e) => {
                crate::actions::show_message(&format!("No se pudo añadir la orden: {e}"));
                false
            }
        }
    }

    /// «Añadir alias…»: another way of saying a command that already
    /// exists, picked from a popup so the name matches exactly.
    fn add_alias(&self) -> bool {
        let mtm = self.mtm;
        let mut names: Vec<&str> = commands::vocabulary()
            .commands
            .iter()
            .map(|c| c.name)
            .chain(commands::vocabulary().contextual.iter().map(|c| c.name))
            .collect();
        names.sort_unstable();
        names.dedup();

        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Añadir alias"));
        alert.setInformativeText(&NSString::from_str("Qué orden, y otra forma de pedirla."));
        alert.addButtonWithTitle(&NSString::from_str("Añadir"));
        alert.addButtonWithTitle(&NSString::from_str("Cancelar"));

        let accessory = NSView::new(mtm);
        accessory.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 60.0)));

        let command_picker = NSPopUpButton::new(mtm);
        command_picker.setFrame(NSRect::new(NSPoint::new(0.0, 34.0), NSSize::new(320.0, 26.0)));
        if names.is_empty() {
            command_picker.addItemWithTitle(&NSString::from_str("No hay ninguna orden todavía"));
            command_picker.setEnabled(false);
        }
        for name in &names {
            command_picker.addItemWithTitle(&NSString::from_str(name));
        }
        accessory.addSubview(&command_picker);

        let phrase_field = NSTextField::new(mtm);
        phrase_field.setFrame(NSRect::new(NSPoint::new(0.0, 4.0), NSSize::new(320.0, 24.0)));
        phrase_field.setPlaceholderString(Some(&NSString::from_str("Frase: abre cromo")));
        accessory.addSubview(&phrase_field);

        alert.setAccessoryView(Some(&accessory));

        if alert.runModal() != NSAlertFirstButtonReturn || names.is_empty() {
            return false;
        }
        let index = command_picker.indexOfSelectedItem().max(0) as usize;
        let Some(command) = names.get(index).copied() else {
            return false;
        };
        let phrase = phrase_field.stringValue().to_string().trim().to_string();
        if phrase.is_empty() {
            crate::actions::show_message("Hace falta una frase.");
            return false;
        }
        match config::add_alias(command, &phrase) {
            Ok(()) => true,
            Err(e) => {
                crate::actions::show_message(&format!("No se pudo añadir el alias: {e}"));
                false
            }
        }
    }
}

/// A throwaway button, so a `Tab`'s `add` field has something to hold
/// before the first [`VocabularyEditor::rebuild`] replaces it.
fn placeholder_button(mtm: MainThreadMarker) -> Retained<NSButton> {
    // Safety: no target and no action.
    unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(""), None, None, mtm) }
}

/// Every row whose «Olvidar» button was clicked since it was last read, as
/// the target it should remove — collected up front so the borrow on
/// `rows` is dropped before a removal triggers a rebuild that would try to
/// borrow it again, mutably.
fn clicked_targets(rows: &RefCell<Vec<Row>>) -> Vec<RowTarget> {
    rows.borrow()
        .iter()
        .filter(|row| row.button.clicked())
        .map(|row| match &row.target {
            RowTarget::App(id) => RowTarget::App(id.clone()),
            RowTarget::Command(name) => RowTarget::Command(name.clone()),
            RowTarget::Alias(phrase) => RowTarget::Alias(phrase.clone()),
        })
        .collect()
}

/// Splits a comma-separated field into trimmed, non-empty parts.
fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// What a command's action does, in one line, for the read-only table.
fn describe_action(action: &commands::Action) -> String {
    use commands::Action;
    match action {
        Action::Key(_, _) => "Atajo de teclado".to_string(),
        Action::Volume(delta) if *delta > 0 => "Subir el volumen".to_string(),
        Action::Volume(_) => "Bajar el volumen".to_string(),
        Action::Mute(true) => "Silenciar".to_string(),
        Action::Mute(false) => "Quitar el silencio".to_string(),
        Action::Script(_) => "Guion del sistema".to_string(),
        Action::Type(text) => format!("Escribir «{text}»"),
        Action::Open(url) => format!("Abrir {url}"),
        Action::Sleep => "Pausar Minion".to_string(),
        Action::Hud(true) => "Mostrar lo que oye".to_string(),
        Action::Hud(false) => "Esconder lo que oye".to_string(),
    }
}

/// Applications found in `/Applications`, as `(name, bundle id)`, sorted by
/// name. Read straight from each bundle's `Info.plist` via `defaults
/// read`, the same tool a person would reach for at the command line —
/// simpler than parsing the plist by hand, and nothing here justifies a
/// new dependency for it.
fn installed_apps() -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir("/Applications") else {
        return Vec::new();
    };
    let mut apps: Vec<(String, String)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "app"))
        .filter_map(|entry| {
            let path = entry.path();
            let info = path.join("Contents/Info");
            let bundle_id = plist_string(&info, "CFBundleIdentifier")?;
            let name = plist_string(&info, "CFBundleDisplayName")
                .or_else(|| plist_string(&info, "CFBundleName"))
                .unwrap_or_else(|| {
                    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                });
            Some((name, bundle_id))
        })
        .collect();
    apps.sort_by(|a, b| a.0.cmp(&b.0));
    apps.dedup_by(|a, b| a.1 == b.1);
    apps
}

/// Reads one string key from a `.plist` file (given without its
/// extension, as `defaults` wants it).
fn plist_string(plist: &std::path::Path, key: &str) -> Option<String> {
    let output = Command::new("/usr/bin/defaults").arg("read").arg(plist).arg(key).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comma_separated_field_is_split_and_trimmed() {
        assert_eq!(split_list(" cromo , croma ,, "), vec!["cromo", "croma"]);
        assert_eq!(split_list(""), Vec::<String>::new());
    }

    #[test]
    fn describes_each_kind_of_action_in_spanish() {
        assert_eq!(describe_action(&commands::Action::Type("hola")), "Escribir «hola»");
        assert_eq!(describe_action(&commands::Action::Open("https://x.com")), "Abrir https://x.com");
        assert!(describe_action(&commands::Action::Key(0, crate::actions::Mods::NONE))
            .contains("Atajo"));
    }
}

//! Targets named out loud.
//!
//! Everything else in the vocabulary is a fixed phrase: «dicta una nota»
//! is one entry in a table, and the table is the whole of what can be
//! said. What this module is for is different in kind — «dicta aquí»
//! names no application and no command, it names *the thing in front of
//! you*, which only exists on this machine, at this moment.
//!
//! So the target is resolved against the machine rather than looked up:
//! the Accessibility API (see [`crate::ax`]) for the window and the text
//! field somebody meant.

// ── The text field in front of you ──────────────────────────────────────

/// How deep to look for a text field, and how many elements to look at.
///
/// Both bounded: a window's element tree can be thousands of nodes deep
/// in a browser, and this runs on the listening thread with somebody
/// waiting to dictate into whatever it finds.
const MAX_DEPTH: usize = 6;
const MAX_ELEMENTS: usize = 500;

/// Puts the keyboard focus in something that can be typed into.
///
/// Does nothing when the focus is already in a text field, which is the
/// usual case — this exists for the other one, where the front window has
/// a field nobody clicked in yet.
///
/// Returns what got the focus, for the log. Best effort throughout: an
/// application that will not answer the Accessibility API leaves the
/// focus where it was, and dictation goes wherever it would have gone
/// anyway.
pub fn focus_text_field_here() -> Result<String, String> {
    let app = crate::ax::frontmost().ok_or("nothing is in front")?;
    let element = crate::ax::application(app.pid).ok_or("no accessibility permission")?;

    if let Some(focused) = element.element(crate::ax::FOCUSED_UI_ELEMENT) {
        if focused.is_text_input() {
            let role = focused.role().unwrap_or_default();
            return Ok(format!("{role} already focused in {}", app.name));
        }
    }

    let windows = element.elements(crate::ax::WINDOWS);
    let window = windows.into_iter().next().ok_or("no window to look in")?;
    let field = first_text_input(window).ok_or("no text field in the front window")?;
    let role = field.role().unwrap_or_default();
    field.focus()?;
    Ok(format!("{role} in {}", app.name))
}

/// The first thing that can be typed into, breadth-first from a window.
///
/// Breadth-first on purpose: the field somebody means is the one on the
/// window, not the one buried in a drawer six levels down, and the first
/// answer at the shallowest depth is nearly always right.
fn first_text_input(window: crate::ax::Element) -> Option<crate::ax::Element> {
    let mut queue: std::collections::VecDeque<(crate::ax::Element, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((window, 0));
    let mut seen = 0;
    while let Some((element, depth)) = queue.pop_front() {
        seen += 1;
        if seen > MAX_ELEMENTS {
            return None;
        }
        if depth > 0 && element.is_text_input() {
            return Some(element);
        }
        if depth < MAX_DEPTH {
            for child in element.elements(crate::ax::CHILDREN) {
                queue.push_back((child, depth + 1));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn dictating_here_is_a_destination_with_nothing_to_bring_forward() {
        // «dicta aquí» goes through the destination table, not through
        // this module: what makes it different from «dicta en el
        // documento» is only that `main.rs` looks for a text field first,
        // which it does for any destination with no application of its
        // own. See `focus_text_field_here`.
        let here = crate::commands::named_destination("aquí").expect("«aquí»");
        assert_eq!(here.bundle_id, None);
        assert!(!here.takes_recipient);
        assert!(here.keys_before_typing.is_empty());
        assert_eq!(
            crate::commands::decide("minion dicta aqui").0,
            crate::commands::Decision::DictateInto { destination: "aquí", recipient: None }
        );
        // A trigger word is matched against the transcript as written,
        // accents included, so «aquí» with its accent reaches nothing.
        // «campo» is the same destination by a name that cannot lose one.
        let field = crate::commands::named_destination("campo").expect("«campo»");
        assert_eq!(field.bundle_id, None);
        assert_eq!(
            crate::commands::decide("minion dicta en el campo de texto").0,
            crate::commands::Decision::DictateInto { destination: "campo", recipient: None }
        );
    }
}

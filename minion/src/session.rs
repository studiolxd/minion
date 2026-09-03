//! What survives between utterances, and what to do about the next one.
//!
//! The listening loop is mostly waiting, recognising and acting — all of it
//! tied to a microphone, a model and the machine. Underneath that sits a
//! small state machine: dictation is a mode, "otra vez" refers to the last
//! command, "deshaz" takes back the last thing that had an honest reverse.
//! That part is pure, and lives here so it can be read and tested without
//! saying a word out loud.
//!
//! [`Session::interpret`] decides; `main.rs` carries the decision out. Every
//! branch that changes the state is here; everything that touches the
//! outside world is there.

use crate::answers;
use crate::commands::{self, Decision};

/// Something Minion did that it knows how to take back.
///
/// Not every action can be undone — closing an application is gone — so
/// only the ones with an honest reverse are recorded. Saying so beats a
/// command that silently does nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undoable {
    /// Text that was typed: remove exactly that many characters.
    Typed(usize),
    /// An application that was brought forward: go back to the previous.
    Launched { previous: Option<String> },
}

/// What should happen to one part of one utterance.
///
/// The state has already been updated by the time this is returned: the
/// caller only has to act on the outside world and write the line.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Dictation has just begun.
    EnterDictation,
    /// Dictation has just ended.
    LeaveDictation,
    /// «deja de dictar» said while not dictating.
    NotDictating,
    /// While dictating: type this word for word, and a space after it.
    Type(String),
    /// Heard while dictating, but there was nothing in it to type.
    Nothing,
    /// A question to be answered aloud.
    Answer(answers::Question),
    /// Take back what was last done, if there was anything.
    Undo(Option<Undoable>),
    /// «otra vez» with nothing said before it.
    NothingToRepeat,
    /// Carry this out, this many times.
    Perform { decision: Decision, repeats: usize },
}

/// What one listening session remembers from one utterance to the next.
#[derive(Debug, Default)]
pub struct Session {
    /// While dictating, everything heard is typed rather than obeyed.
    dictating: bool,
    /// What "deshaz lo que has hecho" would undo.
    undoable: Option<Undoable>,
    /// What "otra vez" refers to.
    last_command: Option<Decision>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decides what one part of an utterance means, and remembers it.
    ///
    /// `context` is the application in front, read once per part because
    /// the part before it may well have changed which one that is.
    pub fn interpret(
        &mut self,
        part: &str,
        decision: Decision,
        context: Option<&str>,
    ) -> Outcome {
        // Dictation is a mode: while it is on, everything is text, except
        // the phrase that turns it off.
        if self.dictating {
            if decision == Decision::StopDictation {
                self.dictating = false;
                return Outcome::LeaveDictation;
            }
            let typed = part.trim().to_string();
            if typed.is_empty() {
                return Outcome::Nothing;
            }
            // One more than the text: the space typed after it.
            self.undoable = Some(Undoable::Typed(typed.chars().count() + 1));
            return Outcome::Type(typed);
        }

        match decision {
            Decision::StartDictation => {
                self.dictating = true;
                return Outcome::EnterDictation;
            }
            Decision::StopDictation => return Outcome::NotDictating,
            Decision::Answer(question) => return Outcome::Answer(question),
            Decision::UndoLast => return Outcome::Undo(self.undoable.take()),
            _ => {}
        }

        // "otra vez" means whatever was said before it.
        let (decision, repeats) = match decision {
            Decision::Again(times) => match &self.last_command {
                Some(previous) => (previous.clone(), times),
                None => return Outcome::NothingToRepeat,
            },
            other => (other, 1),
        };

        // Remember what could be taken back.
        //
        // NOTE: a repeated command records only one repetition. "escribe
        // hola" then "hazlo tres veces" types the text three times and
        // "deshaz" removes the length of one. The same goes for a command
        // macOS refused: it is recorded as undoable even though nothing
        // happened. Both are the behaviour as it stands, kept on purpose.
        match &decision {
            Decision::Type(text) => {
                self.undoable = Some(Undoable::Typed(text.chars().count()));
            }
            Decision::Launch { .. } => {
                self.undoable = Some(Undoable::Launched {
                    previous: context.map(str::to_string),
                });
            }
            _ => {}
        }

        // Only real actions are worth repeating later.
        if !matches!(decision, Decision::Ignored | Decision::Unrecognised) {
            self.last_command = Some(decision.clone());
        }

        Outcome::Perform { decision, repeats }
    }

    /// Splits an utterance into the instructions it holds.
    ///
    /// Here rather than at the call site because whether a sentence may be
    /// split at all depends on the mode: nothing is chained while dictating.
    pub fn split(&self, transcript: &str) -> Vec<String> {
        commands::split_chain(transcript, self.dictating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision a phrase would produce, with no application in front.
    fn decide(phrase: &str) -> Decision {
        commands::decide_in(phrase, None).0
    }

    /// Runs a whole sentence through a session, as the loop would.
    fn say(session: &mut Session, transcript: &str) -> Vec<Outcome> {
        session
            .split(transcript)
            .into_iter()
            .map(|part| {
                let decision = decide(&part);
                session.interpret(&part, decision, None)
            })
            .collect()
    }

    #[test]
    fn dictation_is_a_mode_that_types_what_it_hears() {
        let mut session = Session::new();
        assert_eq!(say(&mut session, "minion empieza a dictar"), vec![
            Outcome::EnterDictation
        ]);
        // A command said while dictating is text, not an order.
        assert_eq!(
            say(&mut session, "minion abre Chrome"),
            vec![Outcome::Type("minion abre Chrome".to_string())]
        );

        assert_eq!(say(&mut session, "minion deja de dictar"), vec![
            Outcome::LeaveDictation
        ]);

        // And once out of it, the same sentence is an order again.
        assert!(matches!(
            say(&mut session, "minion abre Chrome").as_slice(),
            [Outcome::Perform { .. }]
        ));
    }

    #[test]
    fn nothing_is_chained_while_dictating() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        assert_eq!(
            say(&mut session, "minion cierra la pestaña y luego recarga"),
            vec![Outcome::Type(
                "minion cierra la pestaña y luego recarga".to_string()
            )]
        );
    }

    #[test]
    fn an_empty_part_while_dictating_types_nothing() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        assert_eq!(session.interpret("   ", Decision::Unrecognised, None), Outcome::Nothing);
    }

    #[test]
    fn stopping_a_dictation_that_never_started_says_so() {
        let mut session = Session::new();
        assert_eq!(say(&mut session, "minion deja de dictar"), vec![
            Outcome::NotDictating
        ]);
    }

    #[test]
    fn undo_after_a_dictation_removes_what_was_typed() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        say(&mut session, "hola");
        say(&mut session, "minion deja de dictar");
        // Four letters and the space that followed them.
        assert_eq!(say(&mut session, "minion deshaz lo que has hecho"), vec![Outcome::Undo(Some(
            Undoable::Typed(5)
        ))]);
    }

    #[test]
    fn undo_with_nothing_to_undo_says_nothing_is_pending() {
        let mut session = Session::new();
        assert_eq!(say(&mut session, "minion deshaz lo que has hecho"), vec![Outcome::Undo(None)]);
    }

    #[test]
    fn undo_only_takes_back_the_last_thing_once() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        say(&mut session, "hola");
        say(&mut session, "minion deja de dictar");
        assert!(matches!(
            say(&mut session, "minion deshaz lo que has hecho").as_slice(),
            [Outcome::Undo(Some(_))]
        ));
        assert_eq!(say(&mut session, "minion deshaz lo que has hecho"), vec![Outcome::Undo(None)]);
    }

    #[test]
    fn launching_an_application_can_be_taken_back_to_the_previous_one() {
        let mut session = Session::new();
        let decision = decide("minion abre Chrome");
        session.interpret("minion abre Chrome", decision, Some("com.apple.Safari"));
        assert_eq!(
            session.interpret("minion deshaz lo que has hecho", Decision::UndoLast, None),
            Outcome::Undo(Some(Undoable::Launched {
                previous: Some("com.apple.Safari".to_string())
            }))
        );
    }

    #[test]
    fn repeating_with_nothing_said_before_it_repeats_nothing() {
        let mut session = Session::new();
        assert_eq!(say(&mut session, "minion otra vez"), vec![
            Outcome::NothingToRepeat
        ]);
    }

    #[test]
    fn repeating_does_the_last_command_again() {
        let mut session = Session::new();
        let first = decide("minion abre Chrome");
        say(&mut session, "minion abre Chrome");
        assert_eq!(
            say(&mut session, "minion otra vez"),
            vec![Outcome::Perform { decision: first, repeats: 1 }]
        );
    }

    #[test]
    fn a_count_is_carried_through_and_clamped() {
        let mut session = Session::new();
        say(&mut session, "minion abre Chrome");
        assert!(matches!(
            say(&mut session, "minion hazlo tres veces").as_slice(),
            [Outcome::Perform { repeats: 3, .. }]
        ));
        // Ten is as many as a single phrase may ask for.
        assert!(matches!(
            say(&mut session, "minion hazlo cien veces").as_slice(),
            [Outcome::Perform { repeats: 1, .. }]
        ));
    }

    #[test]
    fn an_unrecognised_part_leaves_the_state_alone() {
        let mut session = Session::new();
        let chrome = decide("minion abre Chrome");
        say(&mut session, "minion abre Chrome");
        assert!(matches!(
            session.interpret("minion so fuddy", Decision::Unrecognised, None),
            Outcome::Perform { decision: Decision::Unrecognised, .. }
        ));
        assert!(matches!(
            session.interpret("alguien hablando", Decision::Ignored, None),
            Outcome::Perform { decision: Decision::Ignored, .. }
        ));
        // "otra vez" still means what it meant before them.
        assert_eq!(
            say(&mut session, "minion otra vez"),
            vec![Outcome::Perform { decision: chrome, repeats: 1 }]
        );
    }

    #[test]
    fn a_chain_leaves_the_last_of_its_commands_as_the_one_to_repeat() {
        let mut session = Session::new();
        let outcomes = say(&mut session, "minion abre Chrome y luego abre Safari");
        assert_eq!(outcomes.len(), 2, "two instructions: {outcomes:?}");
        let last = match outcomes.last() {
            Some(Outcome::Perform { decision, .. }) => decision.clone(),
            other => panic!("expected a command, got {other:?}"),
        };
        assert_eq!(
            say(&mut session, "minion otra vez"),
            vec![Outcome::Perform { decision: last, repeats: 1 }]
        );
    }
}

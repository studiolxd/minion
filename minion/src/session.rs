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

use std::time::{Duration, Instant};

use crate::answers;
use crate::commands::{self, Decision, EditIntent};
use crate::learn;

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
    /// An edit command, heard while dictating instead of more text to
    /// type: what it asks for, purely — `main.rs` resolves it against
    /// the transformer's history of what was actually typed, and carries
    /// it out.
    EditDictation(EditIntent),
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
    /// When the conversation window opened, kept only to log how long ago.
    window_opened_at: Option<Instant>,
    /// When it closes. `None` means it is not open.
    window_deadline: Option<Instant>,
    /// A guess put to the user out loud, waiting for a yes or a no.
    pending: Option<Pending>,
    /// Whether to ask at all. Off is what Minion did before there was
    /// anything to ask: say nothing and write the failure down.
    asks: bool,
}

/// How long a question waits for its answer.
///
/// Long enough to think about, short enough that a "sí" meant for someone
/// else in the room has usually stopped being plausible by then.
const QUESTION_SECONDS: Duration = Duration::from_secs(6);

/// A question Minion asked and has not had an answer to yet.
#[derive(Debug)]
struct Pending {
    /// What was not understood, as it will be written down.
    phrase: String,
    suggestion: learn::Suggestion,
    deadline: Instant,
}

/// A question to put to the user, and what saying yes would mean.
#[derive(Debug)]
pub struct Question {
    /// The wording, for the synthesiser or a notification.
    pub text: String,
    /// What it is a guess at: "abrir Safari".
    pub description: String,
    suggestion: learn::Suggestion,
}

/// What an utterance did to a question that was waiting.
#[derive(Debug)]
pub enum Reply {
    /// Yes: do it, and remember it.
    Yes { phrase: String, suggestion: learn::Suggestion },
    /// No, or something that was not an answer at all. `answered` tells
    /// the two apart, because only the first has used up the utterance —
    /// anything else still has to be listened to on its own terms.
    No { phrase: String, answered: bool },
}

/// Whether a phrase is a yes, a no, or neither.
///
/// Short and whole: an answer to a yes-or-no question is one or two words.
/// A long sentence containing "vale" is somebody talking, and a phrase
/// with a "no" anywhere in it is a no whatever else it has in it — "eso
/// no" must not be read as the "eso" it also contains.
fn answer_in(phrase: &str) -> Option<bool> {
    const YES: &[&str] = &["si", "vale", "eso", "exacto", "correcto", "claro", "ese"];
    const NO: &[&str] = &["no", "nada", "dejalo", "olvidalo", "ninguno"];
    let normalised = crate::text::normalise(phrase);
    let words: Vec<&str> = normalised.split_whitespace().collect();
    if words.is_empty() || words.len() > 3 {
        return None;
    }
    if words.iter().any(|word| NO.contains(word)) {
        return Some(false);
    }
    words.iter().any(|word| YES.contains(word)).then_some(true)
}

/// What to run a decision through, once the conversation window has had a
/// say in it.
pub struct Resolved {
    pub decision: Decision,
    pub confidence: f32,
    /// Set when the wake word was missing but the window was open and the
    /// prefixed phrase made sense: seconds since the window opened.
    pub window_after: Option<f32>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the conversation window is open right now.
    pub fn window_open(&self, now: Instant) -> bool {
        self.window_deadline.is_some_and(|deadline| now < deadline)
    }

    /// Opens (or extends) the window, following a command that actually ran
    /// or a question that was answered. Zero duration is a no-op, so a
    /// caller configured with `conversation_seconds = 0` never opens one.
    pub fn open_window(&mut self, now: Instant, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        self.window_opened_at = Some(now);
        self.window_deadline = Some(now + duration);
    }

    /// Closes the window early, before its time is up.
    pub fn close_window(&mut self) {
        self.window_opened_at = None;
        self.window_deadline = None;
    }

    /// Decides what a heard phrase means, consulting the conversation
    /// window when it was not addressed to Minion outright.
    ///
    /// While dictating everything is text regardless, so the window is left
    /// alone: it is only ever consulted for the wake word itself.
    ///
    /// A phrase that still makes no sense once the wake word is assumed
    /// closes the window — the "conversation" was over, whether or not the
    /// caller had anything more to say to it.
    pub fn resolve(&mut self, part: &str, now: Instant, context: Option<&str>) -> Resolved {
        let (decision, confidence) = commands::decide_in(part, context);
        if self.dictating || decision != Decision::Ignored || !self.window_open(now) {
            return Resolved { decision, confidence, window_after: None };
        }

        let wake = commands::wake_words().first().copied().unwrap_or("minion");
        let prefixed = format!("{wake} {part}");
        let (retried, retried_confidence) = commands::decide_in(&prefixed, context);
        if matches!(retried, Decision::Ignored | Decision::Unrecognised) {
            self.close_window();
            return Resolved { decision, confidence, window_after: None };
        }

        let after = self
            .window_opened_at
            .map(|opened| now.saturating_duration_since(opened).as_secs_f32());
        Resolved { decision: retried, confidence: retried_confidence, window_after: after }
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
            if let Some(intent) = commands::dictation_edit(part) {
                // An edit is not itself undoable yet, and it may reach
                // back past what "deshaz" was pointing at — simplest and
                // safest is to drop it rather than leave it referring to
                // text that is no longer there.
                self.undoable = None;
                return Outcome::EditDictation(intent);
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
        // A repeated Type is typed once per repeat with nothing between
        // the repeats (see the loop in main.rs's `report`, which calls
        // `commands::perform` — and so `actions::type_text` — once per
        // repeat with no separator), so undo must remove that many
        // characters, not just one repeat's worth. A repeated Launch still
        // only ever has one previous application to go back to, so it
        // keeps a single undo regardless of `repeats`.
        match &decision {
            Decision::Type(text) => {
                self.undoable = Some(Undoable::Typed(text.chars().count() * repeats));
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

    /// Decides what a heard phrase means for push-to-talk: every utterance
    /// reaching this was, by construction, said while the shortcut was
    /// held, so the wake word is never required — there is no window to
    /// time out or to log about, unlike [`Session::resolve`].
    pub fn resolve_held(&mut self, part: &str, context: Option<&str>) -> (Decision, f32) {
        let (decision, confidence) = commands::decide_in(part, context);
        if self.dictating || decision != Decision::Ignored {
            return (decision, confidence);
        }
        let wake = commands::wake_words().first().copied().unwrap_or("minion");
        let prefixed = format!("{wake} {part}");
        commands::decide_in(&prefixed, context)
    }

    /// Whether to ask about a phrase that was not understood.
    ///
    /// Read from `ask_before_learning`, and set by the caller rather than
    /// defaulted here: a `Session` that has not been told stays silent,
    /// which is what Minion did before it could ask anything.
    pub fn asks_before_learning(&mut self, on: bool) {
        self.asks = on;
    }

    /// The question worth asking about a phrase that was not understood,
    /// if there is one.
    ///
    /// Pure: asking it out loud, and opening the window for the answer,
    /// are the caller's to do — and it only does the second if it managed
    /// the first, since a question nobody heard must not swallow the next
    /// thing said.
    pub fn ask_about(&self, phrase: &str) -> Option<Question> {
        if !self.asks || self.dictating {
            return None;
        }
        let suggestion = learn::suggest(phrase)?;
        Some(Question {
            text: format!("¿Querías decir «{}»?", suggestion.description),
            description: suggestion.description.clone(),
            suggestion,
        })
    }

    /// Starts waiting for the answer to a question that has been asked.
    pub fn open_question(&mut self, phrase: &str, question: Question, now: Instant) {
        // A question takes precedence over the conversation window: the
        // next thing said is an answer, not a command without a wake word.
        self.close_window();
        self.pending = Some(Pending {
            phrase: phrase.to_string(),
            suggestion: question.suggestion,
            deadline: now + QUESTION_SECONDS,
        });
    }

    /// Whether a question is still waiting for its answer.
    pub fn question_open(&self, now: Instant) -> bool {
        self.pending.as_ref().is_some_and(|pending| now < pending.deadline)
    }

    /// Gives up on a question nobody answered, naming the phrase it was
    /// about so the caller can write it down. Called on every utterance
    /// and while waiting for one, so a silence times out on its own.
    pub fn question_timed_out(&mut self, now: Instant) -> Option<String> {
        if self.pending.as_ref().is_some_and(|pending| now >= pending.deadline) {
            return self.pending.take().map(|pending| pending.phrase);
        }
        None
    }

    /// Reads an utterance as the answer to the question that is waiting.
    ///
    /// `None` when there is no question to answer, which is the usual
    /// case: the utterance is then an ordinary one. Answered or not, the
    /// question is over — Minion asks once and does not insist.
    pub fn answer_question(&mut self, part: &str, now: Instant) -> Option<Reply> {
        if self.dictating || !self.question_open(now) {
            return None;
        }
        let pending = self.pending.take()?;
        Some(match answer_in(part) {
            Some(true) => Reply::Yes { phrase: pending.phrase, suggestion: pending.suggestion },
            Some(false) => Reply::No { phrase: pending.phrase, answered: true },
            None => Reply::No { phrase: pending.phrase, answered: false },
        })
    }

    /// Discards whatever "deshaz lo que has hecho" would currently undo.
    ///
    /// `interpret` records a command as undoable the moment it is decided,
    /// before it is carried out — it cannot know whether macOS will refuse
    /// it. The caller finds that out afterwards, from `Done::succeeded`,
    /// and should call this then so a refused command is not offered as
    /// something to undo. `main.rs` should call `session.forget_undo()`
    /// wherever it currently checks `!done.succeeded` in `report`.
    /// Corrects the undo length after dictation rendered the spoken text
    /// into something longer or shorter (punctuation, numbers, personal
    /// vocabulary): «deshaz» must remove what reached the keyboard.
    pub fn retype_length(&mut self, chars: usize) {
        if let Some(Undoable::Typed(_)) = self.undoable {
            self.undoable = Some(Undoable::Typed(chars));
        }
    }

    pub fn forget_undo(&mut self) {
        self.undoable = None;
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

    /// A session that asks about what it did not understand.
    fn asking() -> Session {
        let mut session = Session::new();
        session.asks_before_learning(true);
        session
    }

    /// A phrase the recogniser really might produce for "abre Safari",
    /// close enough to guess at and too far to act on.
    const NEARLY: &str = "minion abreza fari";

    #[test]
    fn a_phrase_that_nearly_named_something_is_asked_about() {
        // Nothing acted on it, which is the whole reason to ask.
        assert_eq!(decide(NEARLY), Decision::Unrecognised);
        let question = asking().ask_about(NEARLY).expect("worth asking about");
        assert_eq!(question.description, "abrir Safari");
        assert_eq!(question.text, "¿Querías decir «abrir Safari»?");
    }

    #[test]
    fn a_weak_guess_is_not_worth_asking_about() {
        // Nothing in the vocabulary is within reach of these, so a
        // question would only be noise.
        for phrase in ["minion de sad", "minion abrecron", "minion escribe"] {
            assert!(asking().ask_about(phrase).is_none(), "«{phrase}» is not worth a question");
        }
        // And with the setting off, nothing is ever asked.
        assert!(Session::new().ask_about(NEARLY).is_none());
    }

    #[test]
    fn nothing_is_asked_while_dictating() {
        let mut session = asking();
        say(&mut session, "minion empieza a dictar");
        assert!(session.ask_about(NEARLY).is_none(), "everything heard is text right now");
    }

    #[test]
    fn saying_yes_runs_the_guess_and_remembers_it() {
        let mut session = asking();
        let now = Instant::now();
        let question = session.ask_about(NEARLY).expect("worth asking about");
        session.open_question(NEARLY, question, now);

        let reply = session
            .answer_question("sí", now + Duration::from_secs(2))
            .expect("an answer was expected");
        match reply {
            Reply::Yes { phrase, suggestion } => {
                assert_eq!(phrase, NEARLY);
                assert_eq!(suggestion.description, "abrir Safari");
                assert!(matches!(
                    suggestion.decision,
                    Decision::Launch { name: "Safari", .. }
                ));
            }
            other => panic!("expected a yes, got {other:?}"),
        }
        // Asked once: the question is over either way.
        assert!(!session.question_open(now));
    }

    #[test]
    fn saying_no_declines_and_teaches_nothing() {
        let mut session = asking();
        let now = Instant::now();
        let question = session.ask_about(NEARLY).expect("worth asking about");
        session.open_question(NEARLY, question, now);

        assert!(matches!(
            session.answer_question("no", now),
            Some(Reply::No { answered: true, .. })
        ));
        assert!(!session.question_open(now));
        // "eso no" is a no, even though "eso" on its own is a yes.
        let question = session.ask_about(NEARLY).expect("worth asking about");
        session.open_question(NEARLY, question, now);
        assert!(matches!(
            session.answer_question("eso no", now),
            Some(Reply::No { answered: true, .. })
        ));
    }

    #[test]
    fn a_question_nobody_answers_times_out() {
        let mut session = asking();
        let now = Instant::now();
        let question = session.ask_about(NEARLY).expect("worth asking about");
        session.open_question(NEARLY, question, now);

        let later = now + Duration::from_secs(7);
        assert!(!session.question_open(later));
        assert_eq!(session.question_timed_out(later).as_deref(), Some(NEARLY));
        // Said once, and only once: there is nothing left to time out.
        assert_eq!(session.question_timed_out(later), None);
        // An answer that arrives too late is not an answer.
        assert!(session.answer_question("sí", later).is_none());
    }

    #[test]
    fn something_that_is_not_an_answer_is_declined_and_still_obeyed() {
        let mut session = asking();
        let now = Instant::now();
        let question = session.ask_about(NEARLY).expect("worth asking about");
        session.open_question(NEARLY, question, now);

        // Somebody carried on talking. The question is dropped, and what
        // was said is still an instruction.
        assert!(matches!(
            session.answer_question("minion abre Chrome", now),
            Some(Reply::No { answered: false, .. })
        ));
        assert!(!session.question_open(now));
        assert!(matches!(
            say(&mut session, "minion abre Chrome").as_slice(),
            [Outcome::Perform { .. }]
        ));
    }

    #[test]
    fn a_question_takes_precedence_over_the_conversation_window() {
        let mut session = asking();
        let now = Instant::now();
        session.open_window(now, Duration::from_secs(5));
        let question = session.ask_about(NEARLY).expect("worth asking about");
        session.open_question(NEARLY, question, now);
        // Otherwise the next "sí" would be tried as a command without a
        // wake word before it was tried as the answer it is.
        assert!(!session.window_open(now));
        assert!(session.question_open(now));
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
    fn undo_after_a_repeated_type_removes_every_repeat() {
        let mut session = Session::new();
        let decision = decide("minion escribe hola");
        session.interpret("minion escribe hola", decision, None);
        session.interpret(
            "minion hazlo tres veces",
            Decision::Again(3),
            None,
        );
        // "hola" is 4 characters, typed three times, with nothing typed
        // between the repeats.
        assert_eq!(
            session.interpret("minion deshaz lo que has hecho", Decision::UndoLast, None),
            Outcome::Undo(Some(Undoable::Typed(12)))
        );
    }

    #[test]
    fn forget_undo_clears_what_deshaz_would_take_back() {
        let mut session = Session::new();
        let decision = decide("minion abre Chrome");
        session.interpret("minion abre Chrome", decision, Some("com.apple.Safari"));
        session.forget_undo();
        assert_eq!(
            session.interpret("minion deshaz lo que has hecho", Decision::UndoLast, None),
            Outcome::Undo(None)
        );
    }

    #[test]
    fn the_undo_length_follows_what_was_really_typed() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        say(&mut session, "hola coma qué tal");
        session.retype_length(12);
        say(&mut session, "minion deja de dictar");
        assert_eq!(
            say(&mut session, "minion deshaz lo que has hecho"),
            vec![Outcome::Undo(Some(Undoable::Typed(12)))]
        );
    }

    #[test]
    fn an_edit_command_while_dictating_is_obeyed_rather_than_typed() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        say(&mut session, "hola");
        assert_eq!(
            say(&mut session, "minion borra la última palabra"),
            vec![Outcome::EditDictation(EditIntent::DeleteLastWord)]
        );
        assert_eq!(
            say(&mut session, "borra la última frase"),
            vec![Outcome::EditDictation(EditIntent::DeleteLastPhrase)]
        );
        assert_eq!(
            say(&mut session, "cambia hola por adiós"),
            vec![Outcome::EditDictation(EditIntent::Replace {
                find: "hola".to_string(),
                replace: "adiós".to_string(),
            })]
        );
    }

    #[test]
    fn a_sentence_that_merely_contains_the_edit_words_is_dictated_as_text() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        assert_eq!(
            say(&mut session, "borra la última palabra que dije"),
            vec![Outcome::Type("borra la última palabra que dije".to_string())]
        );
    }

    #[test]
    fn an_edit_clears_what_deshaz_would_take_back() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        say(&mut session, "hola");
        say(&mut session, "minion borra la última palabra");
        say(&mut session, "minion deja de dictar");
        assert_eq!(
            say(&mut session, "minion deshaz lo que has hecho"),
            vec![Outcome::Undo(None)]
        );
    }

    #[test]
    fn forget_undo_on_an_empty_session_does_nothing_harmful() {
        let mut session = Session::new();
        session.forget_undo();
        assert_eq!(
            session.interpret("minion deshaz lo que has hecho", Decision::UndoLast, None),
            Outcome::Undo(None)
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
    fn a_command_opens_the_window_and_the_next_utterance_needs_no_wake_word() {
        let mut session = Session::new();
        let now = Instant::now();
        session.open_window(now, Duration::from_secs(5));

        let resolved = session.resolve("abre Chrome", now + Duration::from_millis(500), None);
        assert!(!matches!(resolved.decision, Decision::Ignored | Decision::Unrecognised));
        assert_eq!(resolved.window_after, Some(0.5));
    }

    #[test]
    fn a_phrase_that_already_starts_with_the_wake_word_is_unaffected_by_the_window() {
        let mut session = Session::new();
        let now = Instant::now();
        session.open_window(now, Duration::from_secs(5));

        let resolved = session.resolve("minion abre Chrome", now, None);
        assert_eq!(resolved.window_after, None, "the wake word was already there");
    }

    #[test]
    fn the_window_closes_after_its_time_is_up() {
        let mut session = Session::new();
        let now = Instant::now();
        session.open_window(now, Duration::from_secs(5));

        let later = now + Duration::from_secs(6);
        assert!(!session.window_open(later));
        let resolved = session.resolve("abre Chrome", later, None);
        assert_eq!(resolved.decision, Decision::Ignored, "the window had already closed");
    }

    #[test]
    fn a_zero_second_window_never_opens() {
        let mut session = Session::new();
        let now = Instant::now();
        session.open_window(now, Duration::ZERO);
        assert!(!session.window_open(now));
    }

    #[test]
    fn an_utterance_that_still_makes_no_sense_closes_the_window() {
        let mut session = Session::new();
        let now = Instant::now();
        session.open_window(now, Duration::from_secs(5));

        let resolved = session.resolve("so fuddy", now, None);
        assert_eq!(resolved.decision, Decision::Ignored);
        assert_eq!(resolved.window_after, None);
        assert!(!session.window_open(now), "an unrecognised phrase closes it");
    }

    #[test]
    fn a_refused_command_is_never_the_caller_s_reason_to_open_the_window() {
        // The window only opens when the caller — main.rs, once a command
        // has actually run — calls `open_window`. A refused command simply
        // never calls it, so nothing here needs to model "refused" at all:
        // the window stays exactly as closed as it started.
        let session = Session::new();
        assert!(!session.window_open(Instant::now()));
    }

    #[test]
    fn dictating_leaves_the_window_alone() {
        let mut session = Session::new();
        let now = Instant::now();
        session.open_window(now, Duration::from_secs(5));
        say(&mut session, "minion empieza a dictar");

        // Even with the window open, dictation never consults it: whatever
        // is heard is typed, wake word or not.
        let resolved = session.resolve("abre Chrome", now, None);
        assert_eq!(resolved.window_after, None);
        assert!(session.window_open(now), "dictating must not have closed it either");
    }

    #[test]
    fn push_to_talk_never_needs_the_wake_word() {
        let mut session = Session::new();
        let (decision, _) = session.resolve_held("abre Chrome", None);
        assert!(!matches!(decision, Decision::Ignored | Decision::Unrecognised));
    }

    #[test]
    fn push_to_talk_still_says_so_when_nothing_matches() {
        let mut session = Session::new();
        let (decision, _) = session.resolve_held("so fuddy", None);
        assert_eq!(decision, Decision::Unrecognised);
    }

    #[test]
    fn push_to_talk_still_lets_dictation_through_untouched() {
        let mut session = Session::new();
        let decision = decide("minion empieza a dictar");
        session.interpret("minion empieza a dictar", decision, None);
        // Once dictating, resolve_held must not try to prefix the wake
        // word onto what is about to be typed.
        let (decision, _) = session.resolve_held("abre Chrome", None);
        assert_eq!(decision, decide("abre Chrome"));
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

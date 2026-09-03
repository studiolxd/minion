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
use crate::commands::{self, Candidate, Decision, EditIntent};
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
    /// «dicta …»: the named destination should be brought forward, its
    /// recipient (if any) typed, and dictation started once that succeeds.
    /// Naming IO — launching an app, waiting for it, typing into it — that
    /// `Session` itself does not do, unlike `EnterDictation`: dictation
    /// only actually begins once `main.rs` calls back
    /// [`Session::confirm_dictation`] to say the destination is ready.
    DictateInto { destination: &'static str, recipient: Option<String> },
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
    /// «pregunta a la IA …»: ask the AI layer this, and say the answer.
    AskAi(String),
    /// «olvida la conversación»: drop the AI layer's conversation history.
    ForgetAiConversation,
    /// Take back what was last done, if there was anything.
    Undo(Option<Undoable>),
    /// «cancela», «para», «basta»: stop whatever Minion itself is doing.
    /// `main.rs` still has to stop the speech and the macro that may be
    /// running — this only carries what `Session` itself knows was
    /// cancelled, for the log line.
    Cancel { cancelled_question: bool, closed_window: bool },
    /// «espera diez minutos», «no me escuches hasta las cinco»: pause
    /// listening. `main.rs` resolves the spec to an absolute moment (the
    /// wall clock is not this module's to read) and does the actual
    /// pausing and scheduling.
    Pause(commands::PauseSpec),
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
    /// How close the runner-up has to be before a near-tie is put to the
    /// user rather than acted on. Zero never asks, which is what a
    /// `Session` nobody has configured does.
    margin: f32,
    /// Confidence one of the built-in costly commands needs before it just
    /// runs — below it, [`Session::ask_confirm`] asks instead. Zero never
    /// asks, the same way `margin` does, which is what a `Session` nobody
    /// has configured does.
    confirm_below: f32,
}

/// How long a guess waits for its yes or no.
///
/// Long enough to think about, short enough that a "sí" meant for someone
/// else in the room has usually stopped being plausible by then.
const QUESTION_SECONDS: Duration = Duration::from_secs(6);

/// How long a choice between two readings waits.
///
/// Shorter than a guess on purpose: nothing has happened yet and nothing
/// will until this is answered, so the silence is in the way rather than
/// merely unhelpful.
const CHOICE_SECONDS: Duration = Duration::from_secs(4);

/// What a question is about, and so what an answer to it can be.
///
/// The two share everything else — the deadline, the precedence over the
/// conversation window, being asked exactly once — because they are the
/// same mechanism pointed at two different problems.
#[derive(Debug)]
enum Asked {
    /// A guess at a phrase nothing acted on. Yes runs it and learns it.
    Guess(learn::Suggestion),
    /// Two readings of a phrase that were too close to choose between.
    /// Neither has run, and neither will until one is picked.
    Between(Vec<Candidate>),
    /// A command the AI layer matched to a phrase the vocabulary did not
    /// recognise. Yes runs it; unlike [`Asked::Guess`], nothing is learned
    /// from it — a model's guess is not the same as a nearby alias.
    AiSuggestion(AiSuggestion),
    /// A costly command matched below `confirm_below`, not yet run. Yes
    /// carries it out; unlike [`Asked::Guess`], nothing is learned — this
    /// is about how sure Minion was, not about a phrase it did not know.
    Confirm { decision: Decision, repeats: usize },
}

/// A command the AI layer suggested for a phrase the vocabulary missed —
/// [`crate::ai::Suggestion`], resolved against the catalogue into a
/// [`Decision`] that can actually be carried out.
#[derive(Debug, Clone, PartialEq)]
pub struct AiSuggestion {
    pub decision: Decision,
    /// What is said back: "¿Quieres que {description}?".
    pub description: String,
}

/// A question Minion asked and has not had an answer to yet.
#[derive(Debug)]
struct Pending {
    /// What was heard, as it will be written down.
    phrase: String,
    asked: Asked,
    deadline: Instant,
}

/// A question to put to the user, and what each answer would mean.
#[derive(Debug)]
pub struct Question {
    /// The wording, for the synthesiser or a notification.
    pub text: String,
    /// What it is about: "abrir Safari", "Chrome o Chrome Canary".
    pub description: String,
    asked: Asked,
}

/// What an utterance did to a question that was waiting.
#[derive(Debug)]
pub enum Reply {
    /// Yes: do it, and remember it.
    Yes { phrase: String, suggestion: learn::Suggestion },
    /// One of the readings that were offered, picked by the user.
    Chose { phrase: String, candidate: Candidate },
    /// Yes to an AI-suggested command: run it, but do not learn it.
    AiYes { phrase: String, suggestion: AiSuggestion },
    /// Yes to a confirmation: carry out the costly command it was about.
    ConfirmYes { phrase: String, decision: Decision, repeats: usize },
    /// No, or something that was not an answer at all. `answered` tells
    /// the two apart, because only the first has used up the utterance —
    /// anything else still has to be listened to on its own terms.
    No { phrase: String, answered: bool },
}

/// Words that pick one of two readings by where it came rather than by
/// name. Kept short and whole, the same way [`answer_in`] is: an answer to
/// «¿Chrome o Chrome Canary?» is one or two words.
const FIRST: &[&str] = &["primero", "primera", "primer", "uno"];
const SECOND: &[&str] = &["segundo", "segunda", "otro", "otra", "dos"];

/// What an utterance does to a choice that was waiting.
enum Choice {
    /// The reading at this position.
    Made(usize),
    /// Neither of them.
    Declined,
    /// Not an answer at all: somebody carried on talking.
    Elsewhere,
}

/// Reads an utterance as the answer to «¿esto o lo otro?».
///
/// By position first, then by name — an ordinal is unambiguous, while a
/// name is matched with all the tolerance the recogniser needs, and a
/// candidate called "otra pestaña" must not swallow «la otra».
///
/// A name has to fit one of the two better than the other. Two readings
/// close enough to be confused are close enough for one name to reach both
/// — «desactivar wifi» reaches «activar wifi» as well — so a name that
/// does not choose between them has not answered the question, and the
/// ordinals are there for exactly that case.
fn choice_in(phrase: &str, candidates: &[Candidate]) -> Choice {
    // «no», «ninguno», «déjalo»: the same words that decline a guess.
    if answer_in(phrase) == Some(false) {
        return Choice::Declined;
    }
    let normalised = crate::text::normalise(phrase);
    let words: Vec<&str> = normalised.split_whitespace().collect();
    if words.len() <= 3 {
        for (forms, at) in [(FIRST, 0), (SECOND, 1)] {
            if at < candidates.len() && words.iter().any(|word| forms.contains(word)) {
                return Choice::Made(at);
            }
        }
    }
    let mut named: Vec<(usize, f32)> = candidates
        .iter()
        .enumerate()
        .map(|(at, candidate)| (at, commands::candidate_score(phrase, candidate)))
        .filter(|(_, score)| *score >= commands::threshold())
        .collect();
    named.sort_by(|a, b| b.1.total_cmp(&a.1));
    match named.as_slice() {
        [(at, _)] => Choice::Made(*at),
        [(at, best), (_, next), ..] if best > next => Choice::Made(*at),
        _ => Choice::Elsewhere,
    }
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
    /// The phrase the decision was actually made from — `part` itself, or
    /// the same thing with the wake word put back on when the conversation
    /// window supplied it. Anything wanting to ask a second question about
    /// the decision has to ask about the words that produced it.
    pub phrase: String,
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
        let heard = || part.to_string();
        if self.dictating || decision != Decision::Ignored || !self.window_open(now) {
            return Resolved { decision, confidence, phrase: heard(), window_after: None };
        }

        let wake = commands::wake_words().first().copied().unwrap_or("minion");
        let prefixed = format!("{wake} {part}");
        let (retried, retried_confidence) = commands::decide_in(&prefixed, context);
        if matches!(retried, Decision::Ignored | Decision::Unrecognised) {
            self.close_window();
            return Resolved { decision, confidence, phrase: heard(), window_after: None };
        }

        let after = self
            .window_opened_at
            .map(|opened| now.saturating_duration_since(opened).as_secs_f32());
        Resolved {
            decision: retried,
            confidence: retried_confidence,
            phrase: prefixed,
            window_after: after,
        }
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
            Decision::DictateInto { destination, recipient } => {
                return Outcome::DictateInto { destination, recipient };
            }
            Decision::AskAi(text) => return Outcome::AskAi(text),
            Decision::ForgetAiConversation => return Outcome::ForgetAiConversation,
            Decision::Cancel => {
                let cancelled_question = self.pending.take().is_some();
                let closed_window = self.window_opened_at.is_some();
                self.close_window();
                return Outcome::Cancel { cancelled_question, closed_window };
            }
            Decision::Pause(spec) => return Outcome::Pause(spec),
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
    pub fn resolve_held(&mut self, part: &str, context: Option<&str>) -> Resolved {
        let (decision, confidence) = commands::decide_in(part, context);
        if self.dictating || decision != Decision::Ignored {
            return Resolved {
                decision,
                confidence,
                phrase: part.to_string(),
                window_after: None,
            };
        }
        let wake = commands::wake_words().first().copied().unwrap_or("minion");
        let prefixed = format!("{wake} {part}");
        let (decision, confidence) = commands::decide_in(&prefixed, context);
        Resolved { decision, confidence, phrase: prefixed, window_after: None }
    }

    /// Whether to ask about a phrase that was not understood.
    ///
    /// Read from `ask_before_learning`, and set by the caller rather than
    /// defaulted here: a `Session` that has not been told stays silent,
    /// which is what Minion did before it could ask anything.
    pub fn asks_before_learning(&mut self, on: bool) {
        self.asks = on;
    }

    /// How close a runner-up has to be before the choice is put to the
    /// user instead of acted on. Read from `disambiguation_margin`, and
    /// set by the caller for the same reason `asks_before_learning` is: a
    /// `Session` that has not been told never asks.
    pub fn disambiguates(&mut self, margin: f32) {
        self.margin = margin;
    }

    /// Confidence a costly command needs before it just runs. Read from
    /// `[confirm_below]`, and set by the caller for the same reason
    /// `disambiguates` is: a `Session` that has not been told never asks.
    pub fn confirms_below(&mut self, threshold: f32) {
        self.confirm_below = threshold;
    }

    /// The question worth asking before carrying out a costly command
    /// matched below `confirm_below`, if there is one — `None` when the
    /// decision is not one of the built-in costly commands
    /// (`commands::confirm_question` says which), or when it scored well
    /// enough to just run. Never while dictating, and never for a
    /// question or an AI request, since neither ever reaches here: both
    /// are already resolved before `interpret` would produce
    /// `Outcome::Perform`.
    ///
    /// Pure: asking it out loud, and opening the window for the answer,
    /// are the caller's to do.
    pub fn ask_confirm(
        &self,
        decision: &Decision,
        confidence: f32,
        repeats: usize,
    ) -> Option<Question> {
        if self.dictating || self.confirm_below <= 0.0 || confidence >= self.confirm_below {
            return None;
        }
        let text = commands::confirm_question(decision)?;
        Some(Question {
            text: text.clone(),
            description: text,
            asked: Asked::Confirm { decision: decision.clone(), repeats },
        })
    }

    /// The question worth asking about a phrase that was not understood,
    /// if there is one.
    ///
    /// Pure: asking it out loud, and opening the window for the answer,
    /// are the caller's to do — and it only does the second if it managed
    /// the first, since a question nobody heard must not swallow the next
    /// thing said.
    pub fn ask_about(&self, phrase: &str, now: Instant) -> Option<Question> {
        // A choice already on the table is blocking a command; a guess
        // only offers one. Asking both at once would leave the next "sí"
        // answering whichever was asked last, so the guess gives way.
        if !self.asks || self.dictating || self.choosing(now) {
            return None;
        }
        let suggestion = learn::suggest(phrase)?;
        Some(Question {
            text: format!("¿Querías decir «{}»?", suggestion.description),
            description: suggestion.description.clone(),
            asked: Asked::Guess(suggestion),
        })
    }

    /// The question to ask when the phrase came as close to a second
    /// reading as to the one that won.
    ///
    /// Neither is carried out: the whole point is that Minion cannot tell
    /// which was meant, and doing the wrong one and being told so
    /// afterwards is worse than a four-second question. `phrase` is what
    /// the decision was made from — [`Resolved::phrase`], not necessarily
    /// what was said, since the conversation window may have put the wake
    /// word back on.
    ///
    /// Pure: saying it out loud, and opening the window for the answer,
    /// are the caller's to do.
    pub fn ask_between(
        &self,
        phrase: &str,
        decision: &Decision,
        context: Option<&str>,
    ) -> Option<Question> {
        if self.dictating || self.margin <= 0.0 {
            return None;
        }
        // Nothing was going to happen anyway, so there is nothing to stop
        // and ask about. A phrase nobody understood is `ask_about`'s.
        if matches!(decision, Decision::Ignored | Decision::Unrecognised) {
            return None;
        }
        let ranked = commands::decide_ranked(phrase, context);
        let [winner, runner_up, ..] = ranked.as_slice() else {
            return None;
        };
        // The ranking has to agree with what is about to be done, or the
        // question would be about a command nobody asked for.
        if winner.decision != *decision {
            return None;
        }
        let threshold = commands::threshold();
        if runner_up.score < threshold || winner.score < threshold {
            return None;
        }
        if winner.score - runner_up.score > self.margin {
            return None;
        }
        Some(Question {
            text: format!("¿{} o {}?", winner.name, runner_up.name),
            description: format!("{} o {}", winner.name, runner_up.name),
            asked: Asked::Between(vec![winner.clone(), runner_up.clone()]),
        })
    }

    /// The question worth asking about a command the AI layer matched to a
    /// phrase the vocabulary did not recognise, if there is one to ask.
    ///
    /// Mirrors [`Session::ask_about`] — same guard, same shape — except the
    /// guess comes from the model rather than a nearby alias, and it never
    /// checks `asks_before_learning`: offering an AI suggestion is a
    /// different feature from active learning, on by default whenever the
    /// AI layer itself is (see `[ai] use`, checked by the caller through
    /// `ai::ask_for_command` before this is ever reached).
    ///
    /// Pure: asking it out loud, and opening the window for the answer,
    /// are the caller's to do.
    pub fn ask_ai_suggestion(&self, suggestion: AiSuggestion, now: Instant) -> Option<Question> {
        if self.dictating || self.choosing(now) {
            return None;
        }
        Some(Question {
            text: format!("¿Quieres que {}?", suggestion.description),
            description: suggestion.description.clone(),
            asked: Asked::AiSuggestion(suggestion),
        })
    }

    /// Whether the question waiting is one that is holding a command back.
    fn choosing(&self, now: Instant) -> bool {
        self.question_open(now)
            && matches!(self.pending.as_ref().map(|p| &p.asked), Some(Asked::Between(_)))
    }

    /// Starts waiting for the answer to a question that has been asked.
    pub fn open_question(&mut self, phrase: &str, question: Question, now: Instant) {
        // A choice is holding a command back and a guess is not, so a
        // guess never displaces one — the same rule `ask_about` applies
        // before speaking, kept here too so the state cannot be reached
        // by a caller that asked in some other order.
        let choosing = matches!(question.asked, Asked::Between(_));
        if !choosing && self.choosing(now) {
            return;
        }
        // A question takes precedence over the conversation window: the
        // next thing said is an answer, not a command without a wake word.
        self.close_window();
        let waits = if choosing { CHOICE_SECONDS } else { QUESTION_SECONDS };
        self.pending = Some(Pending {
            phrase: phrase.to_string(),
            asked: question.asked,
            deadline: now + waits,
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
        let phrase = pending.phrase;
        Some(match pending.asked {
            Asked::Guess(suggestion) => match answer_in(part) {
                Some(true) => Reply::Yes { phrase, suggestion },
                Some(false) => Reply::No { phrase, answered: true },
                None => Reply::No { phrase, answered: false },
            },
            Asked::Between(candidates) => match choice_in(part, &candidates) {
                Choice::Made(at) => {
                    Reply::Chose { phrase, candidate: candidates[at].clone() }
                }
                Choice::Declined => Reply::No { phrase, answered: true },
                Choice::Elsewhere => Reply::No { phrase, answered: false },
            },
            Asked::AiSuggestion(suggestion) => match answer_in(part) {
                Some(true) => Reply::AiYes { phrase, suggestion },
                Some(false) => Reply::No { phrase, answered: true },
                None => Reply::No { phrase, answered: false },
            },
            Asked::Confirm { decision, repeats } => match answer_in(part) {
                Some(true) => Reply::ConfirmYes { phrase, decision, repeats },
                Some(false) => Reply::No { phrase, answered: true },
                None => Reply::No { phrase, answered: false },
            },
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

    /// Confirms that an `Outcome::DictateInto` destination is ready — its
    /// application is frontmost and the recipient, if any, has been typed —
    /// so dictation itself may begin. `main.rs` calls this after doing that
    /// IO; `interpret` never sets `dictating` for `DictateInto` itself,
    /// since a destination that never came to the front must not silently
    /// start typing into whatever else is in front instead.
    pub fn confirm_dictation(&mut self) {
        self.dictating = true;
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

    /// Two readings the recogniser can leave in a dead heat. With the
    /// words run together — which is what it does — "desactivael wifi"
    /// reaches «activar wifi» and «desactivar wifi» with exactly the same
    /// score, and one of the two is the opposite of what was asked for.
    const TIED: &str = "minion desactivael wifi";

    /// A session that asks which of two close readings was meant.
    fn choosing() -> Session {
        let mut session = Session::new();
        session.disambiguates(0.08);
        session
    }

    /// The question a phrase raises, decided the way the loop decides it.
    fn between(session: &Session, phrase: &str) -> Option<Question> {
        let decision = decide(phrase);
        session.ask_between(phrase, &decision, None)
    }

    #[test]
    fn two_readings_too_close_to_choose_between_are_asked_about() {
        // Nothing hesitated: it was about to turn the wifi back on.
        assert_eq!(decide(TIED), Decision::Run("activar wifi"));
        let question = between(&choosing(), TIED).expect("a tie worth asking about");
        assert_eq!(question.text, "¿activar wifi o desactivar wifi?");
        assert_eq!(question.description, "activar wifi o desactivar wifi");
    }

    #[test]
    fn a_clear_winner_is_carried_out_without_asking() {
        assert!(between(&choosing(), "minion abre Chrome").is_none());
        // A phrase nothing acted on is `ask_about`'s to raise, not this
        // one's: there is no second reading to weigh against a first.
        assert!(between(&choosing(), NEARLY).is_none());
        assert!(between(&choosing(), "alguien hablando").is_none());
        // And with no margin configured, nothing is ever asked.
        assert!(between(&Session::new(), TIED).is_none());
    }

    #[test]
    fn the_margin_says_how_close_is_too_close() {
        // «apaga la pantalla» matches «apagar pantalla» outright and
        // «dormir» nearly: close, but not a tie.
        const NEAR: &str = "minion apaga la pantalla";
        let mut wide = Session::new();
        wide.disambiguates(0.2);
        assert!(between(&wide, NEAR).is_some(), "0.2 is wide enough to notice it");
        let mut narrow = Session::new();
        narrow.disambiguates(0.02);
        assert!(between(&narrow, NEAR).is_none(), "0.02 is not");
        // A dead heat is one at any margin at all.
        assert!(between(&narrow, TIED).is_some());
    }

    #[test]
    fn every_way_of_picking_one_of_the_two_is_understood() {
        for (answer, picked) in [
            ("el primero", "activar wifi"),
            ("la primera", "activar wifi"),
            ("el segundo", "desactivar wifi"),
            ("la segunda", "desactivar wifi"),
            ("desactivar wifi", "desactivar wifi"),
        ] {
            let mut session = choosing();
            let now = Instant::now();
            let question = between(&session, TIED).expect("a tie");
            session.open_question(TIED, question, now);
            match session.answer_question(answer, now + Duration::from_secs(1)) {
                Some(Reply::Chose { phrase, candidate }) => {
                    assert_eq!(candidate.name, picked, "«{answer}»");
                    assert_eq!(phrase, TIED);
                }
                other => panic!("«{answer}» should have picked one, got {other:?}"),
            }
            // Asked once: whichever way it was answered, it is over.
            assert!(!session.question_open(now));
        }
    }

    #[test]
    fn declining_a_choice_runs_neither_of_them() {
        for answer in ["ninguno", "déjalo", "no"] {
            let mut session = choosing();
            let now = Instant::now();
            let question = between(&session, TIED).expect("a tie");
            session.open_question(TIED, question, now);
            assert!(
                matches!(
                    session.answer_question(answer, now),
                    Some(Reply::No { answered: true, .. })
                ),
                "«{answer}» declines both"
            );
        }
    }

    #[test]
    fn a_choice_nobody_makes_times_out_sooner_than_a_guess() {
        let mut session = choosing();
        let now = Instant::now();
        let question = between(&session, TIED).expect("a tie");
        session.open_question(TIED, question, now);

        assert!(session.question_open(now + Duration::from_secs(3)));
        let later = now + Duration::from_secs(5);
        assert!(!session.question_open(later), "four seconds, not six");
        assert_eq!(session.question_timed_out(later).as_deref(), Some(TIED));
        // Nothing ran, and there is nothing left to time out.
        assert_eq!(session.question_timed_out(later), None);
    }

    #[test]
    fn a_guess_and_a_choice_are_never_both_open() {
        let mut session = choosing();
        session.asks_before_learning(true);
        let now = Instant::now();
        let question = between(&session, TIED).expect("a tie");
        session.open_question(TIED, question, now);

        // The choice is holding a command back and the guess is not, so
        // the guess is not even asked while one is waiting.
        assert!(session.ask_about(NEARLY, now).is_none());
        // And asked out of turn it still cannot displace it: the next
        // answer belongs to the choice.
        let guess = asking().ask_about(NEARLY, now).expect("worth asking about");
        session.open_question(NEARLY, guess, now);
        assert!(matches!(
            session.answer_question("el segundo", now),
            Some(Reply::Chose { .. })
        ));
    }

    #[test]
    fn a_choice_takes_precedence_over_the_conversation_window() {
        let mut session = choosing();
        let now = Instant::now();
        session.open_window(now, Duration::from_secs(5));
        let question = between(&session, TIED).expect("a tie");
        session.open_question(TIED, question, now);
        // Otherwise «el segundo» would be tried as a command without a
        // wake word before it was tried as the answer it is.
        assert!(!session.window_open(now));
        assert!(session.question_open(now));
    }

    #[test]
    fn a_phrase_that_nearly_named_something_is_asked_about() {
        // Nothing acted on it, which is the whole reason to ask.
        assert_eq!(decide(NEARLY), Decision::Unrecognised);
        let question = asking().ask_about(NEARLY, Instant::now()).expect("worth asking about");
        assert_eq!(question.description, "abrir Safari");
        assert_eq!(question.text, "¿Querías decir «abrir Safari»?");
    }

    #[test]
    fn a_weak_guess_is_not_worth_asking_about() {
        // Nothing in the vocabulary is within reach of these, so a
        // question would only be noise.
        for phrase in ["minion de sad", "minion abrecasa", "minion escribe"] {
            assert!(asking().ask_about(phrase, Instant::now()).is_none(), "«{phrase}» is not worth a question");
        }
        // And with the setting off, nothing is ever asked.
        assert!(Session::new().ask_about(NEARLY, Instant::now()).is_none());
    }

    #[test]
    fn nothing_is_asked_while_dictating() {
        let mut session = asking();
        session.disambiguates(0.08);
        say(&mut session, "minion empieza a dictar");
        assert!(
            session.ask_about(NEARLY, Instant::now()).is_none(),
            "everything heard is text right now"
        );
        assert!(between(&session, TIED).is_none(), "and so is a phrase that would tie");
    }

    #[test]
    fn saying_yes_runs_the_guess_and_remembers_it() {
        let mut session = asking();
        let now = Instant::now();
        let question = session.ask_about(NEARLY, now).expect("worth asking about");
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

    /// A suggestion the AI layer might have made for a phrase the
    /// vocabulary did not recognise.
    fn ai_suggestion() -> AiSuggestion {
        AiSuggestion {
            decision: Decision::Run("abrir Safari"),
            description: "abrir Safari".to_string(),
        }
    }

    #[test]
    fn an_ai_suggestion_is_asked_about_and_yes_runs_it() {
        let mut session = Session::new();
        let now = Instant::now();
        let question = session.ask_ai_suggestion(ai_suggestion(), now).expect("worth asking about");
        assert_eq!(question.text, "¿Quieres que abrir Safari?");
        session.open_question(NEARLY, question, now);

        let reply = session
            .answer_question("sí", now + Duration::from_secs(2))
            .expect("an answer was expected");
        match reply {
            Reply::AiYes { phrase, suggestion } => {
                assert_eq!(phrase, NEARLY);
                assert_eq!(suggestion.decision, Decision::Run("abrir Safari"));
            }
            other => panic!("expected an AI yes, got {other:?}"),
        }
        assert!(!session.question_open(now));
    }

    #[test]
    fn declining_an_ai_suggestion_runs_nothing() {
        let mut session = Session::new();
        let now = Instant::now();
        let question = session.ask_ai_suggestion(ai_suggestion(), now).expect("worth asking about");
        session.open_question(NEARLY, question, now);
        assert!(matches!(
            session.answer_question("no", now),
            Some(Reply::No { answered: true, .. })
        ));
    }

    #[test]
    fn an_ai_suggestion_nobody_answers_times_out() {
        let mut session = Session::new();
        let now = Instant::now();
        let question = session.ask_ai_suggestion(ai_suggestion(), now).expect("worth asking about");
        session.open_question(NEARLY, question, now);

        let later = now + Duration::from_secs(7);
        assert!(!session.question_open(later));
        assert_eq!(session.question_timed_out(later).as_deref(), Some(NEARLY));
        assert!(session.answer_question("sí", later).is_none());
    }

    #[test]
    fn an_ai_suggestion_is_never_offered_while_dictating() {
        let mut session = Session::new();
        say(&mut session, "minion empieza a dictar");
        assert!(session.ask_ai_suggestion(ai_suggestion(), Instant::now()).is_none());
    }

    #[test]
    fn an_ai_suggestion_never_displaces_a_choice_already_on_the_table() {
        let mut session = choosing();
        let now = Instant::now();
        let question = between(&session, TIED).expect("a tie");
        session.open_question(TIED, question, now);
        assert!(session.ask_ai_suggestion(ai_suggestion(), now).is_none());
    }

    #[test]
    fn saying_no_declines_and_teaches_nothing() {
        let mut session = asking();
        let now = Instant::now();
        let question = session.ask_about(NEARLY, now).expect("worth asking about");
        session.open_question(NEARLY, question, now);

        assert!(matches!(
            session.answer_question("no", now),
            Some(Reply::No { answered: true, .. })
        ));
        assert!(!session.question_open(now));
        // "eso no" is a no, even though "eso" on its own is a yes.
        let question = session.ask_about(NEARLY, now).expect("worth asking about");
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
        let question = session.ask_about(NEARLY, now).expect("worth asking about");
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
        let question = session.ask_about(NEARLY, now).expect("worth asking about");
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
        let question = session.ask_about(NEARLY, now).expect("worth asking about");
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
    fn dictate_into_does_not_enter_dictation_before_confirmed() {
        let mut session = Session::new();
        assert_eq!(
            say(&mut session, "minion dicta una nota"),
            vec![Outcome::DictateInto { destination: "nota", recipient: None }]
        );
        // The destination has not been confirmed ready yet: an ordinary
        // command right after is still an order, not dictated text.
        assert!(matches!(
            say(&mut session, "minion abre Chrome").as_slice(),
            [Outcome::Perform { .. }]
        ));
    }

    #[test]
    fn dictate_into_enters_dictation_once_confirmed() {
        let mut session = Session::new();
        say(&mut session, "minion dicta una nota");
        session.confirm_dictation();
        // Only now does a plain sentence become dictated text rather than
        // an order — the same behaviour `StartDictation` gives, just
        // delayed until the caller says the destination is ready.
        assert_eq!(
            say(&mut session, "minion abre Chrome"),
            vec![Outcome::Type("minion abre Chrome".to_string())]
        );
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
        let decision = session.resolve_held("abre Chrome", None).decision;
        assert!(!matches!(decision, Decision::Ignored | Decision::Unrecognised));
    }

    #[test]
    fn push_to_talk_still_says_so_when_nothing_matches() {
        let mut session = Session::new();
        let decision = session.resolve_held("so fuddy", None).decision;
        assert_eq!(decision, Decision::Unrecognised);
    }

    #[test]
    fn push_to_talk_still_lets_dictation_through_untouched() {
        let mut session = Session::new();
        let decision = decide("minion empieza a dictar");
        session.interpret("minion empieza a dictar", decision, None);
        // Once dictating, resolve_held must not try to prefix the wake
        // word onto what is about to be typed.
        let decision = session.resolve_held("abre Chrome", None).decision;
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

    #[test]
    fn cancel_clears_a_pending_question_and_the_conversation_window() {
        let mut session = asking();
        let now = Instant::now();
        let question = session.ask_about(NEARLY, now).expect("worth asking about");
        session.open_question(NEARLY, question, now);
        assert!(session.question_open(now));

        // A question already closes the window when it opens, so this
        // exercises the pending-question half of cancel; the window half
        // is checked below, on its own.
        assert_eq!(
            session.interpret("minion cancela", Decision::Cancel, None),
            Outcome::Cancel { cancelled_question: true, closed_window: false }
        );
        assert!(!session.question_open(now));
        // Answering "sí" now is just an ordinary phrase: there is nothing
        // left to answer.
        assert!(session.answer_question("si", now).is_none());

        session.open_window(now, Duration::from_secs(5));
        assert_eq!(
            session.interpret("minion cancela", Decision::Cancel, None),
            Outcome::Cancel { cancelled_question: false, closed_window: true }
        );
        assert!(!session.window_open(now));
    }

    #[test]
    fn cancel_with_nothing_pending_says_so() {
        let mut session = Session::new();
        assert_eq!(
            session.interpret("minion cancela", Decision::Cancel, None),
            Outcome::Cancel { cancelled_question: false, closed_window: false }
        );
    }

    /// A session that confirms costly commands scored below 0.85.
    fn confirming() -> Session {
        let mut session = Session::new();
        session.confirms_below(0.85);
        session
    }

    #[test]
    fn a_costly_command_below_the_threshold_is_confirmed_not_run() {
        let question = confirming()
            .ask_confirm(&Decision::Run("cerrar ventana"), 0.8, 1)
            .expect("worth confirming");
        assert_eq!(question.text, "¿Cerrar la ventana?");
    }

    #[test]
    fn a_costly_command_at_or_above_the_threshold_just_runs() {
        assert!(confirming().ask_confirm(&Decision::Run("cerrar ventana"), 0.85, 1).is_none());
        assert!(confirming().ask_confirm(&Decision::Run("cerrar ventana"), 0.95, 1).is_none());
    }

    #[test]
    fn an_ordinary_command_is_never_confirmed() {
        assert!(confirming().ask_confirm(&Decision::Run("abrir Chrome"), 0.5, 1).is_none());
    }

    #[test]
    fn zero_never_confirms() {
        assert!(Session::new().ask_confirm(&Decision::Run("cerrar ventana"), 0.1, 1).is_none());
    }

    #[test]
    fn nothing_is_confirmed_while_dictating() {
        let mut session = confirming();
        say(&mut session, "minion empieza a dictar");
        assert!(session.ask_confirm(&Decision::Run("cerrar ventana"), 0.1, 1).is_none());
    }

    #[test]
    fn confirming_yes_carries_out_the_decision() {
        let mut session = confirming();
        let now = Instant::now();
        let question = session
            .ask_confirm(&Decision::Run("cerrar ventana"), 0.8, 1)
            .expect("worth confirming");
        session.open_question("minion cierra la ventana", question, now);
        match session.answer_question("si", now) {
            Some(Reply::ConfirmYes { decision, repeats, .. }) => {
                assert_eq!(decision, Decision::Run("cerrar ventana"));
                assert_eq!(repeats, 1);
            }
            other => panic!("expected a confirmed yes, got {other:?}"),
        }
    }

    #[test]
    fn declining_a_confirmation_runs_nothing() {
        let mut session = confirming();
        let now = Instant::now();
        let question = session
            .ask_confirm(&Decision::Run("cerrar ventana"), 0.8, 1)
            .expect("worth confirming");
        session.open_question("minion cierra la ventana", question, now);
        assert!(matches!(
            session.answer_question("no", now),
            Some(Reply::No { answered: true, .. })
        ));
    }

    #[test]
    fn quitting_an_application_is_confirmed_by_name() {
        let question = confirming()
            .ask_confirm(
                &Decision::Quit { name: "Chrome", bundle_id: "com.google.Chrome" },
                0.8,
                1,
            )
            .expect("worth confirming");
        assert_eq!(question.text, "¿Salir de Chrome?");
    }

    #[test]
    fn a_pause_decision_becomes_a_pause_outcome() {
        let mut session = Session::new();
        let spec = commands::PauseSpec::For(Duration::from_secs(600), "diez minutos".to_string());
        assert_eq!(
            session.interpret("minion espera diez minutos", Decision::Pause(spec.clone()), None),
            Outcome::Pause(spec)
        );
    }

    #[test]
    fn cancel_bare_word_is_decided_as_cancel() {
        for phrase in ["minion cancela", "minion para", "minion basta"] {
            assert_eq!(decide(phrase), Decision::Cancel, "«{phrase}»");
        }
        // With an object, "cancela" keeps its old meaning (escape).
        assert_eq!(decide("minion cancela esto"), Decision::Run("cancelar"));
    }
}

//! A question a message asks one person, which waits until they answer it.
//!
//! Whoever acts for a person and needs them asks in the conversation: the
//! message says what is asked, and carries the answers as buttons. The
//! question is state of the message, so it is on every device of everyone who
//! reads the conversation, it survives every restart, and it has no time
//! limit: it waits until its person answers it or its asker takes it back.
//!
//! Only the person the asker acts for answers, themselves, at a device of
//! their own ([`Question::answer`]): never an agent, an automation or an app
//! acting for them, never from a device that is not theirs, and nobody else
//! in the workspace. An answer names one of the question's own answers and
//! nothing wider: every kind offers its answers, and an answer carries no
//! words of the person's. What a host does on it (run the one action that
//! asked first) is its kind's, decided where the answer is read and never by
//! the button.
//!
//! What a person wants to say in their own words, they say as a reply in the
//! conversation. A password, a key or another secret is asked for only in the
//! system's protected field on the person's own device, through the vault
//! ([`tilo_actions::remote::person_vault::ASK`]).
//!
//! # Kinds
//!
//! A kind is a name, the answers it offers and what it keeps in
//! [`Question::detail`]. Chat draws the kinds it knows and draws any other as
//! its words over its answers, so a new kind needs no change here to wait, be
//! answered and be withdrawn.
//!
//! | Kind | Answers | Detail | On an answer |
//! |---|---|---|---|
//! | [`CHOICE`] | the asker's own, each with its label | none | the label is posted as the person's own message |
//! | [`ACTION`] | [`ALLOW`], [`DENY`], and [`ALWAYS`] where one event may do that kind of thing unasked from then on, or an agent may use one vault item at the same place | the invocation as its host keeps it; `draft` is the [`tilo_actions::Draft`] the person reviews, and `use.item` says which item of their vault it would use: its kind and where, never its address | its host says it [`STARTED`], runs that one invocation or drops it, and says which ([`Question::settle`]) |
//!
//! To add a kind: give it a name here, say what its detail holds and what
//! its answers mean in this table, draw its card where the others are drawn
//! (`render_chat/question.rs` for Chat's window, `views_question.rs` for the
//! conversation's page), and read its answer where the thing it asks for is
//! done. The verbs stay as they are: `ask`, `answer-question`,
//! `withdraw-question` and `settle-question`.

use serde::{Deserialize, Serialize};
use tilo_actions::policy::MemberId;
use ui::tilo_resource::{self, ActionCx, Atom, JsonSchema, Model, Refusal};

/// The kind whose answers are words to pick from: the one the person presses
/// is posted as their own message.
pub const CHOICE: &str = "choice";
/// The kind that asks about one action its person's grants leave to them.
pub const ACTION: &str = "action";
/// The answer that runs the one action an [`ACTION`] question shows.
pub const ALLOW: &str = "allow";
/// The answer that drops it.
pub const DENY: &str = "deny";
/// The answer that runs it and lets the event it came from do that one kind
/// of thing unasked from then on.
pub const ALWAYS: &str = "always";

/// The outcome while its asker carries the answer out: said before it
/// starts, so an answer is carried out once whatever interrupts its asker.
pub const STARTED: &str = "started";
/// The outcome of an answer its asker carried out.
pub const DONE: &str = "done";
/// The outcome of an answer after which nothing was done.
pub const DROPPED: &str = "dropped";
/// The outcome of an answer its asker could not carry out.
pub const FAILED: &str = "failed";

/// The most answers one question offers.
pub const MAX_ANSWERS: usize = 6;
/// The longest label of an answer's button, in bytes.
pub const MAX_LABEL_BYTES: usize = 80;
/// The longest kind, answer id or outcome, in bytes.
pub const MAX_WORD_BYTES: usize = 64;
/// The most a question's or an answer's detail holds, as JSON, in bytes.
pub const MAX_DETAIL_BYTES: usize = 256 * 1024;
/// The longest reason a withdrawal or an outcome gives, in bytes.
pub const MAX_REASON_BYTES: usize = 1024;

/// What a message asks one person, and where the question stands.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Question {
    /// What kind of question it is: how its card is drawn and what its
    /// answer does ([`CHOICE`], [`ACTION`], or a kind another part of the
    /// system adds).
    pub kind: Atom,
    /// The one person who may answer: the person its asker acts for, as the
    /// asking pinned them. Never an argument.
    pub person: MemberId,
    /// The answers it offers, in the order their buttons stand.
    pub answers: Vec<Choice>,
    /// What its kind keeps with it, as JSON; empty when it keeps nothing.
    pub detail: Atom,
    /// The person's answer, once given.
    pub answered: Option<Answered>,
    /// What its asker did with the answer, once it did.
    pub outcome: Option<Outcome>,
    /// Set when its asker took it back before it was answered.
    pub withdrawn: Option<Withdrawn>,
}

/// One answer a question offers.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Choice {
    /// What the answer is called where it is read back (`allow`, `deny`, or
    /// a word of the asker's own).
    pub id: String,
    /// What its button says. Empty for an answer its kind words itself.
    #[serde(default)]
    pub label: String,
}

/// The answer a person gave.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    /// The answer chosen, by its id.
    pub answer: Atom,
    /// The device of the person's it was pressed on, as that device names
    /// itself; empty when it named none.
    pub device: Atom,
    /// When, in milliseconds since the epoch.
    pub at: i64,
}

/// What came of an answer.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// [`STARTED`], [`DONE`], [`DROPPED`] or [`FAILED`].
    pub outcome: Atom,
    /// When, in milliseconds since the epoch.
    pub at: i64,
    /// What its person is told about it; empty when the outcome says it all.
    pub detail: String,
}

/// A question taken back.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Withdrawn {
    /// When, in milliseconds since the epoch.
    pub at: i64,
    /// Why, in words its person reads; empty when no reason was given.
    pub why: String,
}

/// Where a question stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionState<'a> {
    /// Nobody answered it yet, and it was not taken back.
    Waiting,
    /// Its person answered it, and its asker has not said what came of it.
    Answered(&'a Answered),
    /// Its asker did what the answer asked, or says why not.
    Settled(&'a Answered, &'a Outcome),
    /// Its asker took it back.
    Withdrawn(&'a Withdrawn),
}

/// Whether `word` names a kind, an answer or an outcome: short, lower case,
/// with nothing a label would hold.
fn is_word(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= MAX_WORD_BYTES
        && word.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
}

/// `detail` as a question keeps it: its JSON, bounded.
fn kept_detail(detail: Option<serde_json::Value>) -> Result<Atom, Refusal> {
    let Some(detail) = detail.filter(|detail| !detail.is_null()) else {
        return Ok(Atom::default());
    };
    let text = detail.to_string();
    if text.len() > MAX_DETAIL_BYTES {
        return Err(Refusal::new("a question's detail is too large"));
    }
    Ok(Atom(text))
}

/// The person the invoker of `cx` acts for: who a question it asks is put to.
///
/// # Errors
/// The invoker acts for nobody: a person asks in their own words, and a bot
/// has no person to ask.
pub fn asked_person(cx: &ActionCx) -> Result<MemberId, Refusal> {
    cx.invoked_by()
        .as_ref()
        .and_then(tilo_actions::Acting::acted_for_person)
        .map(MemberId::person)
        .ok_or_else(|| Refusal::new("a question asks the person its asker acts for"))
}

impl Question {
    /// The question of `kind` (a [`CHOICE`] when absent) put to `person`,
    /// offering `answers` and keeping `detail`.
    ///
    /// # Errors
    /// A kind or an answer's id that is no word, no answers, answers that
    /// repeat or are too many, a choice without labelled answers, or a detail
    /// too large.
    pub fn asked(
        person: MemberId,
        kind: Option<String>,
        answers: Vec<Choice>,
        detail: Option<serde_json::Value>,
    ) -> Result<Self, Refusal> {
        let kind = kind.unwrap_or_else(|| CHOICE.to_owned());
        if !is_word(&kind) {
            return Err(Refusal::new("a question's kind is one short word"));
        }
        if answers.is_empty() {
            return Err(Refusal::new(
                "a question offers at least one answer: what your person says in their own words \
                 they say as a reply in the conversation",
            ));
        }
        if answers.len() > MAX_ANSWERS {
            return Err(Refusal::new("a question offers a handful of answers"));
        }
        for (index, answer) in answers.iter().enumerate() {
            if !is_word(&answer.id) {
                return Err(Refusal::new("an answer's id is one short word"));
            }
            if answer.label.len() > MAX_LABEL_BYTES || answer.label.chars().any(char::is_control) {
                return Err(Refusal::new("an answer's label is a few words on one line"));
            }
            if answers
                .iter()
                .take(index)
                .any(|before| before.id == answer.id)
            {
                return Err(Refusal::new("each answer of a question is offered once"));
            }
        }
        if kind == CHOICE && answers.iter().any(|answer| answer.label.trim().is_empty()) {
            return Err(Refusal::new(
                "a choice offers at least one answer, each with what its button says",
            ));
        }
        Ok(Self {
            kind: Atom(kind),
            person,
            answers,
            detail: kept_detail(detail)?,
            answered: None,
            outcome: None,
            withdrawn: None,
        })
    }

    /// Where the question stands. A question taken back stays taken back
    /// though an answer reached it too (the two crossed on their way), unless
    /// its asker had already acted on that answer.
    #[must_use]
    pub fn state(&self) -> QuestionState<'_> {
        match (&self.answered, &self.outcome, &self.withdrawn) {
            (Some(answered), Some(outcome), _) => QuestionState::Settled(answered, outcome),
            (_, _, Some(withdrawn)) => QuestionState::Withdrawn(withdrawn),
            (Some(answered), None, None) => QuestionState::Answered(answered),
            (None, _, None) => QuestionState::Waiting,
        }
    }

    /// Whether it still waits for its person.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.state() == QuestionState::Waiting
    }

    /// Whether `member` is the person it asks.
    #[must_use]
    pub fn asks(&self, member: &MemberId) -> bool {
        self.person == *member && member.is_person()
    }

    /// What its kind keeps with it; `None` when it keeps nothing readable.
    #[must_use]
    pub fn kept(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.detail.0).ok()
    }

    /// The invocation an [`ACTION`] question asks about, as its person
    /// reviews it.
    #[must_use]
    pub fn draft(&self) -> Option<tilo_actions::Draft> {
        serde_json::from_value(self.kept()?.get("draft")?.clone()).ok()
    }

    /// What the button of the answer `id` says, when the asker worded it.
    #[must_use]
    pub fn label(&self, id: &str) -> Option<&str> {
        self.answers
            .iter()
            .find(|answer| answer.id == id)
            .map(|answer| answer.label.trim())
            .filter(|label| !label.is_empty())
    }

    /// Take `answer` as the answer of the invoker of `cx`, pressed on
    /// `device` at `at`.
    ///
    /// Giving the answer already given again changes nothing: the same
    /// person pressed it on a second device.
    ///
    /// # Errors
    /// The invoker is not the person asked, acting themselves at a device of
    /// their own; the question no longer waits; or it offers no such answer.
    pub fn answer(
        &mut self,
        cx: &ActionCx,
        answer: &str,
        device: Option<String>,
        at: i64,
    ) -> Result<(), Refusal> {
        // The envelope's own account of who acts: a person themselves, at a
        // device of theirs. Software acting for them is in the tree as itself,
        // and an action with no invoker is nobody's answer.
        let invoker = cx.invoked_by();
        let direct = invoker
            .as_ref()
            .and_then(tilo_actions::Acting::direct_person);
        if direct.is_none() || direct != self.person.person_id() {
            return Err(Refusal::new(
                "only the person a question asks answers it, themselves, on a device of their own",
            ));
        }
        match self.state() {
            QuestionState::Waiting => {}
            QuestionState::Answered(given) | QuestionState::Settled(given, _)
                if given.answer.0 == answer =>
            {
                return Ok(());
            }
            QuestionState::Answered(_) | QuestionState::Settled(..) => {
                return Err(Refusal::new("the question is already answered"));
            }
            QuestionState::Withdrawn(_) => {
                return Err(Refusal::new("the question was taken back"));
            }
        }
        if !self.answers.iter().any(|offered| offered.id == answer) {
            return Err(Refusal::new("the question offers no such answer"));
        }
        let device = device.unwrap_or_default();
        if device.len() > MAX_LABEL_BYTES || device.chars().any(char::is_control) {
            return Err(Refusal::new("a device is named in a few words"));
        }
        self.answered = Some(Answered {
            answer: Atom(answer.to_owned()),
            device: Atom(device),
            at,
        });
        Ok(())
    }

    /// Take the question back at `at`, for the reason `why`. Taking back one
    /// already taken back changes nothing.
    ///
    /// # Errors
    /// It was answered: there is nothing left to take back.
    pub fn withdraw(&mut self, why: Option<String>, at: i64) -> Result<(), Refusal> {
        match self.state() {
            QuestionState::Waiting => {}
            QuestionState::Withdrawn(_) => return Ok(()),
            QuestionState::Answered(_) | QuestionState::Settled(..) => {
                return Err(Refusal::new("the question is already answered"));
            }
        }
        let why = why.unwrap_or_default().trim().to_owned();
        if why.len() > MAX_REASON_BYTES {
            return Err(Refusal::new("the reason is too long"));
        }
        self.withdrawn = Some(Withdrawn { at, why });
        Ok(())
    }

    /// Record at `at` what came of the answer: `outcome`, and what its
    /// person is told about it. Saying the same outcome again changes
    /// nothing; a question settles once, after it may have said it
    /// [`STARTED`].
    ///
    /// # Errors
    /// Nobody answered it, it was taken back, it settled otherwise before,
    /// or `outcome` is none of [`STARTED`], [`DONE`], [`DROPPED`] and
    /// [`FAILED`].
    pub fn settle(
        &mut self,
        outcome: &str,
        detail: Option<String>,
        at: i64,
    ) -> Result<(), Refusal> {
        if ![STARTED, DONE, DROPPED, FAILED].contains(&outcome) {
            return Err(Refusal::new(
                "what came of an answer is `started`, `done`, `dropped` or `failed`",
            ));
        }
        match self.state() {
            QuestionState::Answered(_) => {}
            QuestionState::Settled(_, settled) if settled.outcome.0 == outcome => return Ok(()),
            // What was started ends done or failed, never as if untouched.
            QuestionState::Settled(_, settled)
                if settled.outcome.0 == STARTED && [DONE, FAILED].contains(&outcome) => {}
            QuestionState::Settled(..) => {
                return Err(Refusal::new("what came of the answer is already said"));
            }
            QuestionState::Waiting => return Err(Refusal::new("nobody answered the question yet")),
            QuestionState::Withdrawn(_) => return Err(Refusal::new("the question was taken back")),
        }
        let detail = detail.unwrap_or_default().trim().to_owned();
        if detail.len() > MAX_REASON_BYTES {
            return Err(Refusal::new("what is told about the outcome is too long"));
        }
        self.outcome = Some(Outcome {
            outcome: Atom(outcome.to_owned()),
            at,
            detail,
        });
        Ok(())
    }
}

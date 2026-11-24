//! The person's yes before an action that cannot be undone runs.
//!
//! An action's contract says when a call waits for the person
//! ([`tilo_actions::ConfirmationPolicy`]), and Shift never skips what Undo cannot take back
//! ([`ui::widget::shift_skips_confirmation`]). A view of the shell presses such an action: the
//! session keeps the one call waiting, publishes it as `desktop:confirm`, and runs it through the
//! API's confirmed path only when the person accepts it. The question is the system's chrome,
//! drawn by the shell's own view, so any shell view and any replacement shell asks the same way
//! and no app can forge the question.

use std::cell::{Cell, RefCell};

use serde_json::{Map, Value};

/// What the confirm question says, in the person's language: what they are about to run, what it
/// acts on, and its two answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Question {
    /// The action, by the label its resource publishes ("Forget the network").
    pub title: String,
    /// What it acts on, and that nothing takes it back ("Home. This can't be undone.").
    pub message: String,
    /// The button that runs it, named for what it does.
    pub accept: String,
    /// The way out, which runs nothing.
    pub cancel: String,
}

/// The question an action that waits for the person's yes asks: its own label, what it acts on,
/// and that nothing takes it back.
#[must_use]
pub(crate) fn question_for(
    action: &tilo_actions::ActionSpec,
    resource: &tilo_actions::ResourceSpec,
) -> Question {
    let subject = resource.label.trim();
    Question {
        title: action.label.clone(),
        message: if subject.is_empty() {
            crate::i18n::t("session-confirm-irreversible")
        } else {
            crate::i18n::t_args(
                "session-confirm-irreversible-on",
                &[("subject", subject.into())],
            )
        },
        accept: action.label.clone(),
        cancel: crate::i18n::t("session-confirm-cancel"),
    }
}

/// One call waiting for the person's yes: the action, its arguments, and the question shown.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Waiting {
    pub resource: String,
    pub action: String,
    pub arguments: Map<String, Value>,
    pub question: Question,
}

/// The call waiting for the person, if one is.
#[derive(Default)]
pub struct Confirmation {
    waiting: RefCell<Option<Waiting>>,
    /// How often the question came or went: what a reader of its reading follows.
    generation: Cell<u64>,
}

impl Confirmation {
    /// Hold `waiting` for the person, replacing a question that was never answered.
    pub(crate) fn ask(&self, waiting: Waiting) {
        *self.waiting.borrow_mut() = Some(waiting);
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Whether a question waits for the person now.
    #[must_use]
    pub fn open(&self) -> bool {
        self.waiting.borrow().is_some()
    }

    /// What the question says now, when one waits.
    #[must_use]
    pub fn question(&self) -> Option<Question> {
        self.waiting.borrow().as_ref().map(|w| w.question.clone())
    }

    /// The call the person accepted, once.
    pub(crate) fn take(&self) -> Option<Waiting> {
        let taken = self.waiting.borrow_mut().take();
        if taken.is_some() {
            self.generation.set(self.generation.get().wrapping_add(1));
        }
        taken
    }

    /// Let the call go: nothing runs.
    pub(crate) fn cancel(&self) {
        if self.waiting.borrow_mut().take().is_some() {
            self.generation.set(self.generation.get().wrapping_add(1));
        }
    }

    /// How often the question came or went.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }
}

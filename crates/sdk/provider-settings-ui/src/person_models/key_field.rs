//! The protected field a provider's key is typed into, in a page.
//!
//! It is the one secret prompt ([`SecretPrompt`]) in its inline form: what
//! is typed never reaches the accessibility tree, a review, presence, a
//! capture or a log. What the person answered is handed over once, in a
//! buffer that zeroes itself.

use std::cell::RefCell;
use std::rc::Rc;

use ui::widget::{AppModel, EventCtx, ModelPath, PromptOutcome, SecretPrompt, Widget as _};
use zeroize::Zeroizing;

/// What each key field was answered with, by provider: the key, or `None`
/// when the person let it go. An answer wakes the window that holds the
/// providers, which takes it at its next pass.
#[derive(Clone)]
pub(super) struct KeyAnswers {
    answered: Rc<RefCell<Vec<KeyAnswer>>>,
    wake: ui::WakeHandle,
}

/// A provider's id, and the key typed for it or `None` when the field was
/// let go.
pub(super) type KeyAnswer = (String, Option<Zeroizing<String>>);

impl KeyAnswers {
    /// The answers of the window being built on this thread.
    pub(super) fn new() -> Self {
        Self {
            answered: Rc::default(),
            wake: ui::WakeHandle::new(),
        }
    }

    pub(super) fn take(&self) -> Vec<KeyAnswer> {
        std::mem::take(&mut *self.answered.borrow_mut())
    }

    pub(super) fn answer(&self, provider: &str, key: Option<Zeroizing<String>>) {
        self.answered.borrow_mut().push((provider.to_owned(), key));
        self.wake.wake();
    }
}

/// The field for `provider`'s key: the prompt, and where its answer goes.
pub(super) struct KeyField {
    prompt: SecretPrompt,
    provider: String,
    answers: KeyAnswers,
}

impl KeyField {
    pub(super) fn new(provider: &str, prompt: SecretPrompt, answers: KeyAnswers) -> Self {
        Self {
            prompt,
            provider: provider.to_owned(),
            answers,
        }
    }

    /// The prompt, for a test that types a key into it.
    #[cfg(test)]
    pub(super) fn prompt_mut(&mut self) -> &mut SecretPrompt {
        &mut self.prompt
    }

    /// Hand over what the person answered, once.
    fn answered(&mut self) {
        if !self.prompt.answer_pending() {
            return;
        }
        match self.prompt.take_outcome() {
            Some(PromptOutcome::Submit(mut answer)) => {
                let key = std::mem::take(&mut answer.secret);
                self.answers.answer(&self.provider, Some(key));
            }
            Some(PromptOutcome::Cancel) => self.answers.answer(&self.provider, None),
            None => {}
        }
    }
}

impl ui::widget::Decorator for KeyField {
    type Inner = SecretPrompt;

    fn inner(&self) -> &SecretPrompt {
        &self.prompt
    }

    fn inner_mut(&mut self) -> &mut SecretPrompt {
        &mut self.prompt
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.prompt.sync(state, path);
        self.answered();
    }

    fn handle_event(&mut self, event: &ui::winit::event::WindowEvent, ctx: &mut EventCtx<'_>) {
        self.prompt.handle_event(event, ctx);
        self.answered();
    }
}

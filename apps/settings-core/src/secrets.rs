//! The one password prompt in Settings: a pane asks for a secret here
//! ([`SecretAsks::ask`]) and the window shows it as the shared prompt
//! (`ui::widget::SecretPrompt`) in a sheet over the pane, never as a field
//! of the pane's own. The answer comes back to the pane that asked, by the
//! key it asked under ([`SecretAsks::take_answer`]). While the pane uses it
//! the sheet says so ([`SecretAsks::busy`]); it closes once the pane is done
//! ([`SecretAsks::close`]) or says why the secret was refused and asks again
//! ([`SecretAsks::reject`]). Cancel closes it at once.
//!
//! Every clone shares one queue: the window and every pane's context hold
//! the same one. A secret passes only in the zeroizing [`SecretAnswer`]
//! the prompt hands out, which the pane moves on to the device.

use std::cell::RefCell;
use std::rc::Rc;

use ui::widget::{ModalDialog, PromptOutcome, SecretPrompt, Widget};

/// What a pane asked of the window's prompt.
pub(crate) enum Ask {
    Open(String, Box<SecretPrompt>),
    Reject {
        key: String,
        field: usize,
        message: String,
    },
    Busy(String, Option<String>),
    Close(String),
}

#[derive(Default)]
struct Queue {
    asks: Vec<Ask>,
    answers: Vec<(String, PromptOutcome)>,
    /// The key asked under while the prompt is open.
    open: Option<String>,
}

/// The prompt's queue. See the [module docs](self).
#[derive(Clone, Default)]
pub struct SecretAsks(Rc<RefCell<Queue>>);

impl std::fmt::Debug for SecretAsks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretAsks")
            .field("open", &self.0.borrow().open)
            .finish_non_exhaustive()
    }
}

impl SecretAsks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask with `prompt` under `key`. A prompt open under another key is
    /// cancelled.
    pub fn ask(&self, key: impl Into<String>, prompt: SecretPrompt) {
        let key = key.into();
        let mut queue = self.0.borrow_mut();
        if let Some(open) = queue.open.take()
            && open != key
        {
            queue.answers.push((open, PromptOutcome::Cancel));
        }
        queue.open = Some(key.clone());
        queue.asks.push(Ask::Open(key, Box::new(prompt)));
    }

    /// Whether the prompt asked under `key` is open.
    #[must_use]
    pub fn is_asking(&self, key: &str) -> bool {
        self.0.borrow().open.as_deref() == Some(key)
    }

    /// The person's answer to the prompt asked under `key`, once.
    pub fn take_answer(&self, key: &str) -> Option<PromptOutcome> {
        let mut queue = self.0.borrow_mut();
        let at = queue.answers.iter().position(|(asked, _)| asked == key)?;
        Some(queue.answers.remove(at).1)
    }

    /// The secret given to the prompt under `key` was refused: say why
    /// under its first field and ask again.
    pub fn reject(&self, key: &str, message: impl Into<String>) {
        self.reject_at(key, 0, message);
    }

    /// As [`Self::reject`], for the field at `field`.
    pub fn reject_at(&self, key: &str, field: usize, message: impl Into<String>) {
        self.push(
            key,
            Ask::Reject {
                key: key.to_owned(),
                field,
                message: message.into(),
            },
        );
    }

    /// The pane is using the answer (`Some("Connecting…")`), or is done
    /// waiting (`None`).
    pub fn busy(&self, key: &str, busy: Option<String>) {
        self.push(key, Ask::Busy(key.to_owned(), busy));
    }

    /// Close the prompt asked under `key`: the pane is done with it.
    pub fn close(&self, key: &str) {
        let mut queue = self.0.borrow_mut();
        if queue.open.as_deref() == Some(key) {
            queue.open = None;
            queue.asks.push(Ask::Close(key.to_owned()));
        }
    }

    fn push(&self, key: &str, ask: Ask) {
        let mut queue = self.0.borrow_mut();
        if queue.open.as_deref() == Some(key) {
            queue.asks.push(ask);
        }
    }

    /// What the panes asked since the window last looked.
    pub(crate) fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut self.0.borrow_mut().asks)
    }

    /// The person answered the prompt asked under `key`; a cancel closes
    /// it.
    pub(crate) fn answer(&self, key: String, outcome: PromptOutcome) {
        let mut queue = self.0.borrow_mut();
        if matches!(outcome, PromptOutcome::Cancel) && queue.open.as_ref() == Some(&key) {
            queue.open = None;
        }
        queue.answers.push((key, outcome));
    }
}

/// The prompt the window shows, with the key it was asked under.
pub(crate) struct ShownPrompt {
    pub key: String,
    pub dialog: ModalDialog<SecretPrompt>,
}

impl ShownPrompt {
    /// Take in what the panes asked: open, refuse, wait or close. Whether
    /// the window's tree changed.
    pub(crate) fn follow(shown: &mut Option<Self>, asks: &SecretAsks) -> bool {
        let mut changed = false;
        for ask in asks.take_asks() {
            match ask {
                Ask::Open(key, prompt) => {
                    let mut dialog = prompt.into_dialog();
                    Widget::set_focus(dialog.content_mut(), true);
                    *shown = Some(Self { key, dialog });
                    changed = true;
                }
                Ask::Reject {
                    key,
                    field,
                    message,
                } => {
                    if let Some(shown) = shown.as_mut().filter(|shown| shown.key == key) {
                        let prompt = shown.dialog.content_mut();
                        prompt.set_busy(None);
                        prompt.reject_at(field, message);
                    }
                }
                Ask::Busy(key, busy) => {
                    if let Some(shown) = shown.as_mut().filter(|shown| shown.key == key) {
                        shown.dialog.content_mut().set_busy(busy);
                    }
                }
                Ask::Close(key) => {
                    if shown.as_ref().is_some_and(|shown| shown.key == key) {
                        *shown = None;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Hand on what the person answered, if they did. Whether the prompt
    /// closed.
    pub(crate) fn answered(shown: &mut Option<Self>, asks: &SecretAsks) -> bool {
        let Some(prompt) = shown.as_mut() else {
            return false;
        };
        if !prompt.dialog.content().answer_pending() {
            return false;
        }
        match prompt.dialog.content_mut().take_outcome() {
            Some(PromptOutcome::Cancel) => {
                let key = prompt.key.clone();
                *shown = None;
                asks.answer(key, PromptOutcome::Cancel);
                true
            }
            Some(outcome) => {
                asks.answer(prompt.key.clone(), outcome);
                false
            }
            None => false,
        }
    }
}

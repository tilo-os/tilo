//! What a shown view waits on and was refused: what the app that placed it says beside it
//! (`ui::written_view::ViewState`).

use std::cell::{Cell, RefCell};

use tilo_actions::Actor;
use tilolang_tilo::runtime::delegate::Invocation;
use ui::written_view::{Suggestion, ViewState};

#[derive(Default)]
pub(crate) struct State {
    held: RefCell<Held>,
    revision: Cell<u64>,
    /// Wakes the window the view is drawn in, so what the person allowed is sent on its next
    /// frame.
    wake: RefCell<Option<ui::wake::WakeHandle>>,
}

#[derive(Default)]
struct Held {
    last: u64,
    /// What the view asked that waits for the person, with the invocation each stands for.
    waiting: Vec<(Suggestion, Invocation)>,
    /// What the person allowed and the view's next turn sends.
    allowed: Vec<Invocation>,
    refused: Vec<String>,
    unread: Vec<String>,
}

impl State {
    pub(crate) fn woken_by(&self, wake: ui::wake::WakeHandle) {
        *self.wake.borrow_mut() = Some(wake);
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
        if let Some(wake) = self.wake.borrow().as_ref() {
            wake.wake();
        }
    }

    /// The view asked for `invocation`, which the policy asks the person about first: it waits.
    pub(crate) fn suggest(&self, app: String, action: String, by: Actor, invocation: Invocation) {
        {
            let mut held = self.held.borrow_mut();
            held.last = held.last.wrapping_add(1);
            let id = held.last;
            held.waiting.push((
                Suggestion {
                    id,
                    app,
                    action,
                    by,
                },
                invocation,
            ));
        }
        self.moved();
    }

    /// What the person allowed since the last call.
    pub(crate) fn take_allowed(&self) -> Vec<Invocation> {
        std::mem::take(&mut self.held.borrow_mut().allowed)
    }

    /// The person may not read the records of the app people call `app`.
    pub(crate) fn refuse(&self, app: &str) {
        let new = {
            let mut held = self.held.borrow_mut();
            let new = !held.refused.iter().any(|held| held == app);
            if new {
                held.refused.push(app.to_owned());
            }
            new
        };
        if new {
            self.moved();
        }
    }

    /// The records of the app people call `app` could not be read here.
    pub(crate) fn leave_unread(&self, app: &str) {
        let new = {
            let mut held = self.held.borrow_mut();
            let new = !held.unread.iter().any(|held| held == app);
            if new {
                held.unread.push(app.to_owned());
            }
            new
        };
        if new {
            self.moved();
        }
    }
}

impl ViewState for State {
    fn suggestions(&self) -> Vec<Suggestion> {
        self.held
            .borrow()
            .waiting
            .iter()
            .map(|(suggestion, _)| suggestion.clone())
            .collect()
    }

    fn allow(&self, id: u64) {
        let allowed = {
            let mut held = self.held.borrow_mut();
            let at = held
                .waiting
                .iter()
                .position(|(waiting, _)| waiting.id == id);
            at.map(|at| {
                let (_, invocation) = held.waiting.remove(at);
                held.allowed.push(invocation);
            })
            .is_some()
        };
        if allowed {
            self.moved();
        }
    }

    fn dismiss(&self, id: u64) {
        let dismissed = {
            let mut held = self.held.borrow_mut();
            let before = held.waiting.len();
            held.waiting.retain(|(waiting, _)| waiting.id != id);
            held.waiting.len() != before
        };
        if dismissed {
            self.moved();
        }
    }

    fn refused(&self) -> Vec<String> {
        self.held.borrow().refused.clone()
    }

    fn unread(&self) -> Vec<String> {
        self.held.borrow().unread.clone()
    }

    fn revision(&self) -> u64 {
        self.revision.get()
    }
}

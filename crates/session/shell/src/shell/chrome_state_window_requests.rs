//! Window operations the action door queues for the overlay host, which owns
//! the windows' rows, and where each one's outcome goes when its caller
//! waits for one.

use std::rc::Rc;

use super::ChromeState;

/// A window operation the action door asks of the overlay host, which owns
/// the windows' rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    /// As if its chord had been pressed once.
    Shortcut(settings_store::hardware::ShortcutAction),
    /// Carry `group`, whole, into `strip`, as the window menu's Move to
    /// strip does.
    MoveGroupToStrip {
        group: crate::shell::state::GroupId,
        strip: crate::shell::state::StripId,
    },
}

/// Whether a window operation changed anything, written once the overlay
/// host ran it; `None` until then.
pub type WindowOutcome = Rc<std::cell::Cell<Option<bool>>>;

impl ChromeState {
    /// Run `action` as if its chord had been pressed once, on the overlay
    /// host's next update.
    #[track_caller]
    pub fn request_shortcut_action(&mut self, action: settings_store::hardware::ShortcutAction) {
        self.request_window(WindowRequest::Shortcut(action));
    }

    /// Run `request` on the overlay host's next update.
    #[track_caller]
    pub fn request_window(&mut self, request: WindowRequest) {
        tracing::info!(
            target: "tilo::shortcut",
            ?request,
            queued = self.shortcut_action_requests.len(),
            from = %std::panic::Location::caller(),
            "window operation requested"
        );
        self.shortcut_action_requests.push((request, None));
        self.bump();
    }

    /// How many window operations wait for the overlay host.
    pub fn window_requests_queued(&self) -> usize {
        self.shortcut_action_requests.len()
    }

    /// Have every window operation queued since the first `from` say, once
    /// the overlay host ran it, whether it changed anything. The outcomes,
    /// in order; empty when nothing was queued since.
    pub fn answer_window_requests_since(&mut self, from: usize) -> Vec<WindowOutcome> {
        self.shortcut_action_requests
            .iter_mut()
            .skip(from)
            .map(|(_, outcome)| Rc::clone(outcome.get_or_insert_with(WindowOutcome::default)))
            .collect()
    }

    pub fn take_window_requests(&mut self) -> Vec<(WindowRequest, Option<WindowOutcome>)> {
        std::mem::take(&mut self.shortcut_action_requests)
    }
}

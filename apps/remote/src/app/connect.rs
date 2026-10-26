//! Connecting to a saved machine, and giving up on it.
//!
//! A connection attempt is a job of the whole system: it says what it is
//! waiting for, it is bounded ([`CONNECT_DEADLINE`]), and the page it opens
//! offers a way out. A live session is not ended by one click either: its
//! scrollback goes with it, so the person is asked.

use super::{
    CONNECT_DEADLINE, ConnectionId, LiveSession, PendingConfirmation, RemoteApp, SessionStatus,
    owner,
};

impl RemoteApp {
    /// A connection attempt the machine has not answered within
    /// [`CONNECT_DEADLINE`] is given up: "Connecting to X" is a claim with a
    /// time on it, not a page you sit on.
    pub(super) fn expire_connect(&mut self) {
        let Some(started) = self.connect_started else {
            return;
        };
        if !matches!(
            self.session.borrow().status(),
            Some(SessionStatus::Connecting)
        ) {
            self.connect_started = None;
            self.end_connect_job(None);
            return;
        }
        if started.elapsed() < CONNECT_DEADLINE {
            return;
        }
        self.connect_started = None;
        self.abandon_connect("The machine did not answer in time.");
    }

    /// Give up the attempt in flight: nothing of it is kept, and the page
    /// says why rather than waiting for ever.
    fn abandon_connect(&mut self, reason: &str) {
        let Some(id) = self.session_for else {
            return;
        };
        {
            let mut session = self.session.borrow_mut();
            session.live = Some(LiveSession::Error(reason.to_owned()));
            session.changed = true;
        }
        self.end_connect_job(Some(reason.to_owned()));
        ui::attention::publish(ui::attention::Notice::did(
            owner(),
            format!("Stopped connecting to {}", self.target_name(id)),
        ));
        self.dirty = true;
    }

    fn end_connect_job(&mut self, failed: Option<String>) {
        let Some((_, job)) = self.connect_job.take() else {
            return;
        };
        match failed {
            Some(reason) => job.failed(reason),
            None => job.stopped(),
        }
    }

    pub(super) fn target_name(&self, id: ConnectionId) -> String {
        self.workspace
            .get(id)
            .map_or_else(|| "this machine".to_owned(), |c| c.host.0.clone())
    }

    /// Ask before a live session ends: one click should not take someone's
    /// scrollback with it.
    pub(super) fn request_disconnect(&mut self) {
        let Some(id) = self.session_for else {
            return;
        };
        let Some(connection) = self.workspace.get(id) else {
            self.disconnect_session();
            return;
        };
        self.pending_confirmation = Some(PendingConfirmation::Disconnect {
            id,
            target: Self::confirmation_target(connection),
        });
        self.show_confirmation();
    }
}

//! What a test of another crate drives a Mail window with: the test
//! servers it reaches, and what it shows.

use std::sync::Arc;

use super::{EngineHandle, MailApp};

impl MailApp {
    /// Reach mail servers through an engine that trusts the test servers'
    /// certificate ([`crate::fake_servers`]).
    ///
    /// # Panics
    /// The engine could not start.
    #[allow(
        clippy::expect_used,
        reason = "test servers: an engine that cannot start fails the test"
    )]
    pub fn reach_test_servers(&mut self) {
        let engine = crate::engine::live::LiveEngine::trusting(
            tilo_content::ContentStore::memory(),
            Arc::new(|| {}),
            crate::fake_servers::accepting_client_config(),
        )
        .expect("the test engine starts");
        let (tx, rx) = crate::engine::spawn_with(Box::new(engine));
        let mut room = self.state();
        // What the window asked the engine it replaces is never answered:
        // that engine is gone, and it is asked again of this one.
        room.pending_engine_requests.clear();
        room.awaiting_engine = false;
        room.resource_refresh_invocation = None;
        room.engine = Some(EngineHandle { tx, rx });
        drop(room);
        self.shared.borrow_mut().sync_error = None;
        // Server updates and sends no window here could carry out before
        // are offered again.
        for projection in [&self.projection, &self.delivery].into_iter().flatten() {
            projection.outside_changed();
        }
    }

    /// Reach no server: what the window answers comes from what this device
    /// keeps.
    pub fn reach_no_server(&mut self) {
        let mut room = self.state();
        room.pending_engine_requests.clear();
        room.awaiting_engine = false;
        room.resource_refresh_invocation = None;
        room.engine = None;
    }

    /// Whether the window waits on the server for something it asked.
    #[must_use]
    pub fn awaiting_server(&self) -> bool {
        let room = self.state();
        room.awaiting_engine || room.resource_refresh_invocation.is_some()
    }

    /// How many messages of `account` this device keeps.
    #[must_use]
    pub fn kept_on_device(&self, account: &str) -> usize {
        self.state().store.load(account).len()
    }

    /// How many messages the window holds.
    #[must_use]
    pub fn held(&self) -> usize {
        self.shared.borrow().mailbox.messages.len()
    }

    /// The draft the window's composer edits, while it is open.
    #[must_use]
    pub fn composing_draft(&self) -> Option<String> {
        self.compose
            .open
            .then(|| self.compose.draft_id().map(str::to_owned))
            .flatten()
    }

    /// The message the window shows.
    #[must_use]
    pub fn shown_message(&self) -> Option<String> {
        self.shared.borrow().selected.map(|id| id.to_string())
    }
}

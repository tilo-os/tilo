//! Each person's own state of the app, as an instance holds it: the view state they set in the
//! app's views and what they have seen ([`tilolang_tilo::personal`]).
//!
//! It is one resource of the person's in the workspace's store, readable by them alone: the
//! instance reads it when it starts and again whenever the store moved, and writes what the
//! person changed of it. So the place a person picked is still theirs after a restart and on
//! their other devices.

use tilolang_tilo::personal::{self, Keep, PersonalState};
use tilolang_tilo::resources::{ResourceStore, Watch};

/// One person's own state of one app, held by an instance of it.
pub struct PersonalDocument {
    app: String,
    person: String,
    /// What the store holds, read again whenever it moved.
    state: PersonalState,
    /// What moved in the store since the state was last read.
    watch: Watch,
    /// Whether the state moved since the instance was last given it.
    moved: bool,
}

impl PersonalDocument {
    /// `person`'s own state of `app`, as `store` holds it now.
    pub fn open(app: &str, person: &str, store: &ResourceStore) -> Self {
        let mut held = Self {
            app: app.to_owned(),
            person: person.to_owned(),
            state: PersonalState::default(),
            watch: store.watch(),
            moved: true,
        };
        held.read(store);
        held
    }

    fn read(&mut self, store: &ResourceStore) {
        let _ = self.watch.take();
        match personal::read(&*store.port(), &self.app, &self.person) {
            Ok(state) => {
                if state != self.state {
                    self.state = state;
                    self.moved = true;
                }
            }
            Err(error) => tracing::warn!(%error, "personal: the person's own state does not read"),
        }
    }

    /// What the store holds of the person's state.
    #[must_use]
    pub fn state(&self) -> &PersonalState {
        &self.state
    }

    /// Whether the state moved since the instance was last given it: read again first when the
    /// store moved.
    pub fn take_moved(&mut self, store: &ResourceStore) -> bool {
        if !self.watch.peek().is_empty() {
            self.read(store);
        }
        std::mem::take(&mut self.moved)
    }

    /// The instance could not take the state in: it is given again on the next turn.
    pub fn moved_again(&mut self) {
        self.moved = true;
    }

    /// Keep `keeps`, what `person` changed of their own state, in the store.
    pub fn keep(&mut self, person: &str, keeps: &[Keep], store: &ResourceStore) {
        if keeps.is_empty() {
            return;
        }
        if let Err(error) = personal::keep(&*store.port(), &self.app, person, keeps) {
            tracing::warn!(%error, "personal: the person's own state was not kept");
            return;
        }
        // The instance holds what it changed already: the state read back is the same, and is
        // not given to it again.
        let moved = self.moved;
        self.read(store);
        self.moved = moved;
    }
}

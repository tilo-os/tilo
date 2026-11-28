//! Each person's own state of the app, as an instance holds it: the view state they set in the
//! app's views and what they have seen ([`tilolang_tilo::personal`]).
//!
//! It is one resource of the person's in the workspace's store, readable by them alone: the
//! instance reads it when it starts and again whenever the store moved, and keeps what the
//! person changed of it as the app's own writes for them. So the place a person picked is still
//! theirs after a restart and on their other devices.

use tilo_resource_store::Port;
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

/// What the person's own state of the app `address` names keeps of the resource `address`'s
/// declared `state`: the system's shell and that app itself read it, and nobody else. An app's
/// own package declaring that it uses another app grants it nothing here: a third app reads
/// this state only through a grant the person or the workspace makes, which no view state has
/// yet. `None` when no window keeps one.
///
/// # Errors
/// `address` names no app, or `reader` may not read that app's windows' state.
pub fn shown_at(
    port: &dyn Port,
    person: &str,
    reader: &str,
    address: &str,
    state: &str,
) -> Result<Option<serde_json::Value>, String> {
    let app = tilolang_tilo::publish::app_of_address(address)
        .ok_or_else(|| format!("`{address}` names no app"))?;
    if !(reader == tilolang_tilo::shell::SHELL_APP || reader == app) {
        return Err(format!(
            "{reader} may not read the state {app}'s windows keep without a grant"
        ));
    }
    personal::shown_json(port, app, person, address, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource_store::LocalPort;

    fn find() -> Keep {
        Keep::View {
            person: "ada".to_owned(),
            window: tilolang_tilo::runtime::view_state::shown_window("so.tilo.notes/notes/n1"),
            state: "find".to_owned(),
            value: serde_json::json!({"query": "x"}),
        }
    }

    /// The read the host makes: the shell and the app itself read a window's state; another app
    /// is refused however it declares its own uses, and only the person's own state is read.
    #[test]
    fn a_windows_state_reads_for_the_shell_and_its_app_and_for_no_other() {
        let port = LocalPort::in_memory();
        personal::keep(&port, "so.tilo.notes", "ada", &[find()]).expect("kept");
        let address = "so.tilo.notes/notes/n1";
        let read = |reader: &str, person: &str| shown_at(&port, person, reader, address, "find");
        assert_eq!(
            read("so.tilo.shell", "ada").expect("the shell reads"),
            Some(serde_json::json!({"query": "x"}))
        );
        assert!(read("so.tilo.notes", "ada").expect("its app").is_some());
        // `so.tilo.tasks` declares in its own package that it uses Notes; a package grants
        // itself nothing, so the read is still refused.
        assert!(
            read("so.tilo.tasks", "ada").is_err(),
            "an app that declares a use is refused: only a grant reads this"
        );
        assert_eq!(
            read("so.tilo.shell", "bo").expect("nobody else's"),
            None,
            "another person keeps nothing here"
        );
    }
}

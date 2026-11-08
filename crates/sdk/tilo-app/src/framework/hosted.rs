//! An app's view side as a client of its one launcher-made connection to the
//! session, over which it is handed readings and submits actions
//! (`docs/architecture/apps.md` §3).
//!
//! An app whose records the session's document host keeps holds no document,
//! no key and no transport of its own: the host runs its model and verbs,
//! publishes the record's tree, answers a reading of it, and admits the
//! actions a view submits (`docs/designs/app-language/one-app-model.md` §4).
//! What the app's view side needs of that is two things, which this module
//! names once:
//!
//! - **a reading in**: the state the session publishes for a resource, as
//!   JSON ([`tilo_resource::Resource::state_json`],
//!   [`tilo_resource::ResourceKind::to_json`]);
//! - **an action out**: the action a control submits, with its arguments,
//!   addressed to the record the session keeps.
//!
//! [`Connection`] is that pair as the app's process reaches it. The launcher
//! makes the connection and the session knows which app and which launch is
//! on it because it made it, so an action over it is decided by the session,
//! never claimed by the app (§4). A host that runs the app inside itself (a
//! test, a capture's fixture) supplies its own [`Answers`], and the view side
//! is the same.
//!
//! The connection never carries a document, a key or another device's address:
//! its reading is a state as JSON, and its action is a name and JSON
//! arguments. Nothing here can reach anything else.

use std::rc::Rc;

use tilo_actions::ActionAddress;
use tilo_resource::Resource;

/// An action an app submits over its connection, with whom it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// The action's address: the resource it changes and its name.
    pub address: ActionAddress,
    /// Its arguments, by parameter name; empty for an action that takes none.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// Whether the person was acting in this app's view when it was
    /// submitted. An action without one is the app's own request, which the
    /// session decides as the app (`docs/architecture/apps.md` §4).
    pub in_person: bool,
}

impl Ask {
    /// The action `action` of `resource`, with `arguments`, submitted by
    /// `in_person`'s gesture — the person's, or the app's own request.
    #[must_use]
    pub fn new(
        resource: impl Into<String>,
        action: impl Into<String>,
        arguments: serde_json::Map<String, serde_json::Value>,
        in_person: bool,
    ) -> Self {
        Self {
            address: ActionAddress::new(resource, action),
            arguments,
            in_person,
        }
    }
}

/// What answers a [`Connection`]: the session's door, or a host that runs the
/// app inside itself in a test or a capture.
pub trait Answers {
    /// The reading of `resource` now, as the host publishes it: the state a
    /// view reads, never a document. `None` when nothing publishes it, or the
    /// caller may not read it (denial is absence, `§6`).
    fn reading(&self, resource: &str) -> Option<serde_json::Value>;

    /// Submit `ask`, answering the door's verdict.
    ///
    /// # Errors
    /// The door's refusal, in its own words (`§4`, `§6`).
    fn submit(&self, ask: &Ask) -> Result<(), String>;
}

/// The app's side of its connection to the session's door.
#[derive(Clone)]
pub struct Connection {
    answers: Rc<dyn Answers>,
}

impl Connection {
    /// The connection the launcher made the app it started: the session's
    /// published readings and its action door, over the one connection the
    /// launch holds. A process the session did not start reaches nothing
    /// through it, as a launch it did not record is refused (`§3`).
    #[must_use]
    pub fn launcher() -> Self {
        Self {
            answers: Rc::new(Session),
        }
    }

    /// A connection answered by `answers` in this process: a test's document
    /// host, or a capture's readings.
    #[must_use]
    pub fn in_process(answers: impl Answers + 'static) -> Self {
        Self {
            answers: Rc::new(answers),
        }
    }

    /// The reading of `resource` now.
    #[must_use]
    pub fn reading(&self, resource: &str) -> Option<serde_json::Value> {
        self.answers.reading(resource)
    }

    /// The state of `R` that `resource` names, read from the host's reading
    /// through `R`'s own model type: what a view reads while its app holds no
    /// document.
    ///
    /// # Errors
    /// Nothing publishes a reading the caller may read, or it is not `R`'s
    /// state.
    pub fn state<R: Resource>(&self, resource: &str) -> Result<R::State, String> {
        let reading = self
            .reading(resource)
            .ok_or_else(|| format!("the session publishes no reading of {resource}"))?;
        tilo_resource::state_from_reading::<R>(&reading).map_err(|error| error.to_string())
    }

    /// Submit `ask`; the door's verdict.
    pub fn submit(&self, ask: &Ask) -> Result<(), String> {
        self.answers.submit(ask)
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Connection")
    }
}

/// The session's side, reached as any app reaches the session's published
/// resources and its door (`ui_core::collaboration::SessionResource`,
/// `ui_core::RemoteActions`).
#[derive(Clone, Copy)]
struct Session;

impl Answers for Session {
    fn reading(&self, resource: &str) -> Option<serde_json::Value> {
        let reading = crate::framework::hosted_readings::reading(resource)?;
        serde_json::from_str(&reading)
            .inspect_err(|error| {
                tracing::warn!(%error, %resource, "the session's reading of a resource is not JSON");
            })
            .ok()
    }

    fn submit(&self, ask: &Ask) -> Result<(), String> {
        crate::framework::hosted_readings::submit(ask)
    }
}

#[cfg(test)]
mod tests;

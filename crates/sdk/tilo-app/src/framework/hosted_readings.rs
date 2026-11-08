//! How an app's [`Connection`](super::Connection) reaches the session: the
//! readings it publishes, and the action door that admits what the app
//! submits (`docs/architecture/apps.md` §2–§4).
//!
//! An app's process is handed one connection by the launcher, and the
//! session's published readings and its door are reached over it. This module
//! is that reaching, in one place, so the view side
//! ([`super::hosted`]) names what it wants and not how a session is spoken to.
//!
//! A reading is watched, not asked for per frame: the session hands each newer
//! one over the same connection (`ui_core::collaboration::SessionResource`),
//! so a view reading it sees a change the host committed without a round
//! trip. An action is submitted through the session's action door
//! (`ui_core::RemoteActions`), which decides who the action is and whether it
//! is admitted — the app names what it asked for and nothing else.
//!
//! Without the broker transport (a build with no session to reach), the
//! reading watch opens nothing and the door refuses: a view built over such a
//! connection shows what it was handed, which is nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::hosted::Ask;

thread_local! {
    /// Each resource's watch, kept for the process: a watch is opened once
    /// and its newest reading is handed to every view that reads it.
    static WATCHES: RefCell<BTreeMap<String, Rc<Watch>>> = const { RefCell::new(BTreeMap::new()) };
}

/// One resource's reading, followed over the session's connection.
struct Watch {
    /// The newest reading since a view last read it, as JSON text, or why the
    /// watch opened nothing.
    newest: std::sync::Mutex<Option<Result<String, String>>>,
    /// The session's watch, kept alive for as long as this is. `None` when the
    /// session published nothing to watch: a process it did not start, or a
    /// build with no session in reach.
    resource: Option<ui_core::collaboration::SessionResource>,
}

impl Watch {
    /// The watch on `resource`, opened the first time it is asked for.
    fn of(resource: &str) -> Rc<Self> {
        WATCHES.with(|watches| {
            Rc::clone(
                watches
                    .borrow_mut()
                    .entry(resource.to_owned())
                    .or_insert_with(|| Rc::new(Self::open(resource))),
            )
        })
    }

    fn open(resource: &str) -> Self {
        match ui_core::collaboration::SessionResource::open(resource) {
            Ok(watch) => Self {
                newest: std::sync::Mutex::new(None),
                resource: Some(watch),
            },
            Err(error) => Self {
                newest: std::sync::Mutex::new(Some(Err(error))),
                resource: None,
            },
        }
    }

    /// The newest reading, if one arrived since the last call.
    fn latest(&self) -> Option<Result<String, String>> {
        self.resource
            .as_ref()
            .and_then(ui_core::collaboration::SessionResource::latest)
            .or_else(|| {
                self.newest
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
            })
    }
}

/// The reading `resource` stands at on the session's connection, as its JSON
/// text; `None` when nothing publishes it or the caller may not read it.
#[must_use]
pub(crate) fn reading(resource: &str) -> Option<String> {
    let watch = Watch::of(resource);
    // The reading an open watch begins at is the first `latest` hands; a
    // later change arrives as the session speaks, and `latest` takes it.
    match watch.latest() {
        Some(Ok(text)) => Some(text),
        _ => None,
    }
}

/// Submit `ask` through the session's action door, as the person acting in
/// this app's view or as the app itself (`docs/architecture/apps.md` §4).
pub(crate) fn submit(ask: &Ask) -> Result<(), String> {
    let remote = ui_core::RemoteActions::current();
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_actions::remote::OWN_PROVIDER.into(),
        address: ask.address.clone(),
    };
    let argument = tilo_actions::remote::InvokeArgument::Json(ask.arguments.clone());
    let call = remote.invoke(target, argument, None);
    // The door answers in one round trip for a quick action
    // (`INVOKE_WAIT_MS`); a longer one stays running and answers its caller
    // as it settles, which the caller sees through the action's own feedback.
    // What is decided now is whether it was admitted at all.
    call.with(|arrival| match arrival {
        ui_core::arriving::Arrival::Ready(update) => match update {
            tilo_actions::InvocationUpdate::Failed { error, .. } => Err(error.message.clone()),
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                Err(message.clone().unwrap_or_else(|| "cancelled".to_owned()))
            }
            _ => Ok(()),
        },
        ui_core::arriving::Arrival::Failed(error) => Err(error.to_string()),
        ui_core::arriving::Arrival::Pending { .. } => Ok(()),
    })
}

//! Reading another provider's action result, kept current.
//!
//! [`RecordEnv::query`](super::RecordEnv::query) invokes a view action of
//! any provider through the session's action door, as the person using the
//! app, and shows its answer as [`Answer`] state a view binds like any
//! other. A live action ([`tilo_actions::ExecutionMode::Live`]) sends a new
//! result whenever its provider's answer changes, and the state follows;
//! an action that answers once is read once. The door decides the read as
//! it decides any invocation: a caller without the permission gets
//! [`Answer::Denied`], never the data.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{ActionHostError, InvokeArgument, RemoteActionTarget};
use ui_core::{Arrival, Followed, LocalState};

/// What a [`Query`] has read.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer<T> {
    /// Nothing has answered yet.
    Loading,
    /// The result, as last read.
    Ready(T),
    /// The caller may not read it; why.
    Denied(String),
    /// The read failed; why.
    Failed(String),
}

impl<T> Answer<T> {
    /// The result, once there is one.
    pub const fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Loading | Self::Denied(_) | Self::Failed(_) => None,
        }
    }
}

/// The action denial codes a provider fails a read with.
const DENIED: &[&str] = &["not_permitted", "permission_denied"];

fn answered<T: DeserializeOwned>(
    call: &Arrival<InvocationUpdate, ActionHostError>,
) -> Option<Answer<T>> {
    let decode = |value: &serde_json::Value| {
        T::deserialize(value).map_or_else(
            |error| Answer::Failed(format!("the answer was not understood: {error}")),
            Answer::Ready,
        )
    };
    match call {
        Arrival::Pending { .. }
        | Arrival::Ready(
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Cancelled { .. },
        ) => None,
        Arrival::Ready(InvocationUpdate::Item { value, .. }) => Some(decode(value)),
        Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Some(decode(
            output.value.as_ref().unwrap_or(&serde_json::Value::Null),
        )),
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
            Some(if DENIED.contains(&error.code.as_str()) {
                Answer::Denied(error.message.clone())
            } else {
                Answer::Failed(error.message.clone())
            })
        }
        Arrival::Failed(ActionHostError::NotPermitted) => {
            Some(Answer::Denied(ActionHostError::NotPermitted.to_string()))
        }
        Arrival::Failed(error) => Some(Answer::Failed(error.to_string())),
    }
}

/// One read [`RecordEnv::query`](super::RecordEnv::query) follows. Clones
/// share it. It is followed while the view scope that asked for it lives,
/// or a clone of it does; then it is cancelled.
pub struct Query<T> {
    answer: LocalState<Answer<T>>,
    inner: Rc<Inner>,
}

impl<T> Clone for Query<T> {
    fn clone(&self) -> Self {
        Self {
            answer: self.answer.clone(),
            inner: Rc::clone(&self.inner),
        }
    }
}

struct Inner {
    /// The door it reads through; none for a read that reaches no
    /// provider ([`Query::fixed`]).
    opener: Option<super::reference::Opener>,
    target: RemoteActionTarget,
    /// Which read's answers count: each [`Query::set_argument`] starts the
    /// next, and an answer the one before sends late is not shown.
    generation: Arc<AtomicU64>,
    followed: RefCell<Option<Followed>>,
}

impl<T: DeserializeOwned + Send + Sync + 'static> Query<T> {
    pub(super) fn start(
        opener: &super::reference::Opener,
        alive: &Weak<()>,
        target: RemoteActionTarget,
        argument: InvokeArgument,
    ) -> Self {
        let query = Self {
            answer: LocalState::new(Answer::Loading),
            inner: Rc::new(Inner {
                opener: Some(opener.clone()),
                target,
                generation: Arc::new(AtomicU64::new(0)),
                followed: RefCell::new(None),
            }),
        };
        query.set_argument(argument);
        opener.keep(
            alive.clone(),
            Rc::clone(&query.inner) as Rc<dyn std::any::Any>,
        );
        query
    }

    /// A read that reaches no provider and holds `answer` (a capture's
    /// stand-in for the action's result), or nothing.
    pub(super) fn fixed(answer: Option<&serde_json::Value>) -> Self {
        Self {
            answer: LocalState::new(answer.map_or(Answer::Loading, |value| {
                T::deserialize(value).map_or_else(
                    |error| Answer::Failed(format!("the answer was not understood: {error}")),
                    Answer::Ready,
                )
            })),
            inner: Rc::new(Inner {
                opener: None,
                target: RemoteActionTarget {
                    provider: String::new(),
                    address: tilo_actions::ActionAddress::new("", ""),
                },
                generation: Arc::new(AtomicU64::new(0)),
                followed: RefCell::new(None),
            }),
        }
    }

    /// Read again with `argument` in place of the last one (a filter
    /// changed). The answer shows the last result until the new one lands.
    pub fn set_argument(&self, argument: InvokeArgument) {
        let Some(opener) = &self.inner.opener else {
            return;
        };
        let generation = self
            .inner
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let current = Arc::clone(&self.inner.generation);
        let answer = self.answer.writer();
        let followed = opener.follow(
            self.inner.target.clone(),
            argument,
            move |call: &Arrival<InvocationUpdate, ActionHostError>| {
                if current.load(Ordering::Acquire) != generation {
                    return;
                }
                if let Some(answered) = answered::<T>(call) {
                    answer.set(answered);
                }
            },
        );
        // The read it replaces is cancelled as it drops.
        self.inner.followed.replace(Some(followed));
    }

    /// What the read has answered, kept current.
    pub fn answer(&self) -> LocalState<Answer<T>> {
        self.answer.clone()
    }
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

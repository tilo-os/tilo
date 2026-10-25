//! Reading another provider's action result, kept current.
//!
//! [`RecordEnv::query`](super::RecordEnv::query) invokes a view action of
//! any provider through the session's action door, as the person using the
//! app, and shows its answer as data that arrives
//! ([`Arriving`](ui_core::arriving::Arriving)): the view says what shows
//! once it is there ([`ViewCx::arriving`](super::ViewCx::arriving)), and the
//! framework shows the placeholder until then. A live action
//! ([`tilo_actions::ExecutionMode::Live`]) sends a new result whenever its
//! provider's answer changes, and the answer follows; an action that
//! answers once is read once. The door decides the read as it decides any
//! invocation: a caller without the permission gets
//! [`ReadFailure::Denied`], never the data.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{ActionHostError, InvokeArgument, RemoteActionTarget};
use ui_core::Followed;
use ui_core::arriving::{Arrival, Arriving, Source};

/// Why a [`Query`] has no answer to show: the caller may not read it, or
/// the read failed. Either is the read's failure ([`Arrival::Failed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// The caller may not read it; why.
    Denied(String),
    /// The read failed; why.
    Failed(String),
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

/// The action denial codes a provider fails a read with.
const DENIED: &[&str] = &["not_permitted", "permission_denied"];

/// What `call`'s latest update says about the read; `None` while it has
/// nothing new to show.
fn answered<T: DeserializeOwned>(
    call: &Arrival<InvocationUpdate, ActionHostError>,
) -> Option<Result<T, ReadFailure>> {
    let decode = |value: &serde_json::Value| {
        T::deserialize(value)
            .map_err(|error| ReadFailure::Failed(format!("the answer was not understood: {error}")))
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
            Some(Err(if DENIED.contains(&error.code.as_str()) {
                ReadFailure::Denied(error.message.clone())
            } else {
                ReadFailure::Failed(error.message.clone())
            }))
        }
        Arrival::Failed(ActionHostError::NotPermitted) => Some(Err(ReadFailure::Denied(
            ActionHostError::NotPermitted.to_string(),
        ))),
        Arrival::Failed(error) => Some(Err(ReadFailure::Failed(error.to_string()))),
    }
}

/// One read [`RecordEnv::query`](super::RecordEnv::query) follows. Clones
/// share it. It is followed while the view scope that asked for it lives,
/// or a clone of it does; then it is cancelled.
pub struct Query<T> {
    answer: Arriving<T, ReadFailure>,
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
    /// The argument it reads with now: what a retry reads again.
    argument: RefCell<InvokeArgument>,
    /// Which read's answers count: each [`Query::set_argument`] starts the
    /// next, and an answer the one before sends late is not shown.
    generation: Arc<AtomicU64>,
    followed: RefCell<Option<Followed>>,
    /// Reads again after a failure; see [`asked_again`].
    again: Option<Box<dyn Fn()>>,
}

thread_local! {
    /// The queries of this thread a retry can reach, by their number: a
    /// retry is asked through the read's handle, which may cross threads,
    /// and the query it reads again lives on this one.
    static QUERIES: RefCell<HashMap<u64, Weak<Inner>>> = RefCell::new(HashMap::new());
}

static NEXT_QUERY: AtomicU64 = AtomicU64::new(1);

/// Read query `number` again, if it still lives on this thread.
fn asked_again(number: u64) {
    let inner = QUERIES.with(|queries| queries.borrow().get(&number).and_then(Weak::upgrade));
    if let Some(again) = inner.as_ref().and_then(|inner| inner.again.as_ref()) {
        again();
    }
}

impl<T: DeserializeOwned + Send + Sync + 'static> Query<T> {
    pub(super) fn start(
        opener: &super::reference::Opener,
        alive: &Weak<()>,
        target: RemoteActionTarget,
        argument: InvokeArgument,
    ) -> Self {
        let number = NEXT_QUERY.fetch_add(1, Ordering::Relaxed);
        let answer = Arriving::pending(Source::Local).on_retry(move |_| asked_again(number));
        let inner = Rc::new_cyclic(|weak: &Weak<Inner>| {
            let (weak, read) = (weak.clone(), answer.clone());
            Inner {
                opener: Some(opener.clone()),
                target,
                argument: RefCell::new(argument),
                generation: Arc::new(AtomicU64::new(0)),
                followed: RefCell::new(None),
                again: Some(Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        follow::<T>(&inner, &read, true);
                    }
                })),
            }
        });
        QUERIES.with(|queries| {
            let mut queries = queries.borrow_mut();
            queries.retain(|_, query| query.strong_count() > 0);
            queries.insert(number, Rc::downgrade(&inner));
        });
        follow::<T>(&inner, &answer, false);
        opener.keep(alive.clone(), Rc::clone(&inner) as Rc<dyn std::any::Any>);
        Self { answer, inner }
    }

    /// A read that reaches no provider and holds `answer` (a capture's
    /// stand-in for the action's result), or nothing.
    pub(super) fn fixed(answer: Option<&serde_json::Value>) -> Self {
        let answer = match answer {
            Some(value) => {
                let read = Arriving::pending(Source::Local);
                read.writer().finish(T::deserialize(value).map_err(|error| {
                    ReadFailure::Failed(format!("the answer was not understood: {error}"))
                }));
                read
            }
            None => Arriving::pending(Source::Local),
        };
        Self {
            answer,
            inner: Rc::new(Inner {
                opener: None,
                target: RemoteActionTarget {
                    provider: String::new(),
                    address: tilo_actions::ActionAddress::new("", ""),
                },
                argument: RefCell::new(InvokeArgument::None),
                generation: Arc::new(AtomicU64::new(0)),
                followed: RefCell::new(None),
                again: None,
            }),
        }
    }

    /// Read again with `argument` in place of the last one (a filter
    /// changed). The answer shows the last result until the new one lands.
    pub fn set_argument(&self, argument: InvokeArgument) {
        if self.inner.opener.is_none() {
            return;
        }
        *self.inner.argument.borrow_mut() = argument;
        follow::<T>(&self.inner, &self.answer, false);
    }

    /// What the read has answered, kept current: build what shows once it
    /// is there with [`ViewCx::arriving`](super::ViewCx::arriving).
    pub fn answer(&self) -> Arriving<T, ReadFailure> {
        self.answer.clone()
    }
}

/// Follow `inner`'s read with its current argument, publishing into
/// `answer`; the read it replaces is cancelled as it drops. A read asked
/// again (`again`) shows its placeholder until it answers; a new argument
/// keeps the last result shown until the new one lands.
fn follow<T: DeserializeOwned + Send + Sync + 'static>(
    inner: &Inner,
    answer: &Arriving<T, ReadFailure>,
    again: bool,
) {
    let Some(opener) = &inner.opener else {
        return;
    };
    let generation = inner
        .generation
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    let current = Arc::clone(&inner.generation);
    let writer = answer.writer();
    let followed = opener.follow(
        inner.target.clone(),
        inner.argument.borrow().clone(),
        move |call: &Arrival<InvocationUpdate, ActionHostError>| {
            if current.load(Ordering::Acquire) != generation {
                return;
            }
            if let Some(answered) = answered::<T>(call) {
                writer.finish(answered);
            }
        },
    );
    // Where the answer comes from is where the door says the call goes.
    let from = followed
        .call()
        .with(|call| call.source().cloned())
        .unwrap_or(Source::Local);
    let shown = answer.with(|read| read.ready().is_some());
    if again || !shown {
        let _ = answer.begin(from);
    }
    inner.followed.replace(Some(followed));
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

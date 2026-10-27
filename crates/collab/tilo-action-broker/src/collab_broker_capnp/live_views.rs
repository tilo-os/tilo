//! Host steps whose result stays current.
//!
//! A live view ([`tilo_actions::ExecutionMode::Live`]) is admitted like any
//! host step and then stays open: the broker reads it once, and again each
//! time the provider says the result may have changed, and sends each new
//! result as the invocation's next `Item`. Before every read it decides the
//! caller's authority and the policy again, so a withdrawn grant ends the
//! view instead of reading on. It ends when the caller cancels it, its
//! execution ends, its permission goes, or the provider fails; only its
//! admission and that end are journalled, its items stay in memory.

use std::collections::HashMap;
use std::sync::Arc;

use super::host_action_admission::AdmittedHostAction;
use super::{CapnpBrokerState, InvocationCaller, InvocationOwner};
use crate::services::LiveView;
use tilo_actions::remote::Caller;
use tilo_actions::{ActionDescriptor, ActionFailure, InvocationUpdate};

#[cfg(test)]
#[path = "live_views_tests.rs"]
mod tests;

/// The open live views, by job.
#[derive(Default)]
pub(super) struct LiveViews(std::sync::Mutex<HashMap<u64, Open>>);

struct Open {
    owner: InvocationOwner,
    caller: Caller,
    update: InvocationUpdate,
    stop: Arc<tokio::sync::Notify>,
}

/// What a running view needs to decide, before each read, that its caller
/// may still read it.
pub(super) struct Following {
    pub caller: Caller,
    pub origin: Option<InvocationCaller>,
    pub room: String,
    pub app_id: String,
    pub descriptor: ActionDescriptor,
    pub witnessed: bool,
}

/// Why a view ended.
enum End {
    Cancelled,
    Failed(ActionFailure),
}

fn failure(code: &str, message: impl Into<String>) -> ActionFailure {
    ActionFailure {
        code: code.into(),
        message: message.into(),
        details: None,
        retryable: false,
    }
}

/// The runtime live views run on: off the broker's local set, so a slow
/// read never holds a connection's turn.
fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tilo_rt::daemon("tilo-live-views")
                .workers(1)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl LiveViews {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Open>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The latest state of open view `id`, as its owner sees it.
    pub(super) fn status(
        &self,
        id: u64,
        owner: Option<&InvocationOwner>,
    ) -> Option<InvocationUpdate> {
        self.lock()
            .get(&id)
            .filter(|open| owner.is_none_or(|owner| owner == &open.owner))
            .map(|open| open.update.clone())
    }

    /// Ask open view `id` to stop, for `owner` when it is not already
    /// known to be the caller's. `None` when no view by that id is open.
    pub(super) fn cancel(
        &self,
        id: u64,
        owner: Option<&InvocationOwner>,
    ) -> Option<Result<(), String>> {
        let (holder, stop) = self
            .lock()
            .get(&id)
            .map(|open| (open.owner.clone(), Arc::clone(&open.stop)))?;
        if owner.is_some_and(|owner| owner != &holder) {
            return Some(Err("only the invocation owner may cancel it".into()));
        }
        stop.notify_one();
        Some(Ok(()))
    }

    /// Stop every view `caller`'s run follows: the run has ended.
    pub(super) fn retire(&self, caller: &Caller) {
        let Some(execution) = caller.execution_id() else {
            return;
        };
        for open in self.lock().values() {
            if open.caller.execution_id() == Some(execution) {
                open.stop.notify_one();
            }
        }
    }

    fn insert(&self, id: u64, open: Open) {
        self.lock().insert(id, open);
    }

    fn remove(&self, id: u64) {
        self.lock().remove(&id);
    }

    fn set(&self, id: u64, update: InvocationUpdate) {
        if let Some(open) = self.lock().get_mut(&id) {
            open.update = update;
        }
    }
}

impl CapnpBrokerState {
    /// Run `view`, admitted as `admitted`, until it ends.
    pub(super) fn follow_live_view(
        self: &Arc<Self>,
        admitted: AdmittedHostAction,
        following: Following,
        view: LiveView,
    ) -> Result<(), String> {
        let runtime = match runtime() {
            Ok(runtime) => runtime,
            Err(reason) => {
                self.end_live_view(
                    &admitted,
                    End::Failed(failure("live_view_failed", reason.clone())),
                );
                return Err(reason);
            }
        };
        let stop = Arc::new(tokio::sync::Notify::new());
        self.live_views.insert(
            admitted.id,
            Open {
                owner: admitted.owner.clone(),
                caller: following.caller.clone(),
                update: InvocationUpdate::Accepted {
                    invocation: admitted.invocation,
                },
                stop: Arc::clone(&stop),
            },
        );
        let state = Arc::clone(self);
        runtime.spawn(async move {
            let end = state
                .run_live_view(&admitted, &following, view, &stop)
                .await;
            state.end_live_view(&admitted, end);
        });
        Ok(())
    }

    async fn run_live_view(
        &self,
        admitted: &AdmittedHostAction,
        following: &Following,
        view: LiveView,
        stop: &tokio::sync::Notify,
    ) -> End {
        let LiveView {
            mut read,
            mut changes,
        } = view;
        let item_schema = following
            .descriptor
            .contract
            .execution
            .item_schema()
            .cloned();
        let mut last: Option<serde_json::Value> = None;
        let mut sequence = 0_u64;
        loop {
            if let Err(error) = self.live_view_permitted(following) {
                return End::Failed(failure("not_permitted", error.to_string()));
            }
            changes.borrow_and_update();
            let value = tokio::select! {
                value = read() => value,
                () = stop.notified() => return End::Cancelled,
            };
            let value = match value {
                Some(Ok(value)) => value,
                Some(Err(error)) => return End::Failed(*error),
                None => {
                    return End::Failed(failure("live_view_failed", "the view's read stopped"));
                }
            };
            if let Some(schema) = &item_schema
                && let Err(error) = tilo_actions::validate_json_schema(&value, schema)
            {
                return End::Failed(failure(
                    "live_view_failed",
                    format!("the view's result violated its contract: {error}"),
                ));
            }
            if last.as_ref() != Some(&value) {
                sequence = sequence.saturating_add(1);
                last = Some(value.clone());
                self.live_views.set(
                    admitted.id,
                    InvocationUpdate::Item {
                        invocation: admitted.invocation,
                        sequence,
                        value,
                    },
                );
                self.invocation_changed();
            }
            tokio::select! {
                changed = changes.changed() => if changed.is_err() {
                    return End::Failed(failure("live_view_failed", "the view's provider stopped"));
                },
                () = stop.notified() => return End::Cancelled,
            }
        }
    }

    /// Whether the view's caller may still read it, decided now.
    fn live_view_permitted(
        &self,
        following: &Following,
    ) -> Result<(), tilo_actions::remote::ActionHostError> {
        let (workspace, actor, now) = self.current_framework_authority(
            &following.caller,
            following.origin.as_ref(),
            &following.room,
        )?;
        self.framework_policy(
            (&workspace, &actor, now),
            (&following.app_id, &following.descriptor),
            None,
            following.witnessed,
            false,
        )
    }

    fn end_live_view(&self, admitted: &AdmittedHostAction, end: End) {
        let invocation = admitted.invocation;
        let update = match end {
            End::Cancelled => InvocationUpdate::Cancelled {
                invocation,
                message: None,
            },
            End::Failed(error) => InvocationUpdate::Failed { invocation, error },
        };
        // Retained as ended before it stops being open, so an observer
        // finds it one way or the other.
        if let Err(error) = self.complete_host_action(admitted, update) {
            tracing::warn!(?error, "could not retain a live view's end");
        }
        self.live_views.remove(admitted.id);
        self.invocation_changed();
    }
}

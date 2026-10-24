//! A workspace's activity, served to apps.
//!
//! The session keeps the history of the active workspace in its activity
//! model ([`tilo_desktop::activity::RoomActivityPane`]), on the thread that
//! draws the desktop, for the top bar's glance, the inferred status and the
//! recap on return. This module serves the same model to apps through the
//! action broker, under [`wire::PROVIDER`]: its reads are live views the
//! broker reads again whenever the model moves ([`changed`]), and its writes
//! run on the model's thread when the loop next pumps ([`take_jobs`]).
//!
//! What this device has seen is the session's own: only the person at the
//! device marks it, and it never reaches a shared record.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tilo_action_broker::services::{
    HostStepOutcome, HostStepRequest, HostStepServices, LiveRead, LiveView,
};
use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionKind, ExecutionLocus,
    InputSpec, OperationKind,
};

use crate::shell::i18n::t;
use crate::shell::overlay_host::ShellOverlayHost;

/// One piece of work on the activity model, run on its thread.
pub(crate) type Job = Box<dyn FnOnce(&mut ShellOverlayHost) + Send>;

/// How the broker's threads reach the activity model.
struct Port {
    jobs: Mutex<Vec<Job>>,
    /// Moves whenever a served read may read differently.
    changes: tokio::sync::watch::Sender<u64>,
    /// Wakes the thread holding the model to pump it.
    wake: Mutex<Option<crate::session_runtime::Wake>>,
}

fn port() -> &'static Port {
    static PORT: OnceLock<Port> = OnceLock::new();
    PORT.get_or_init(|| Port {
        jobs: Mutex::new(Vec::new()),
        changes: tokio::sync::watch::channel(0).0,
        wake: Mutex::new(None),
    })
}

/// Wake the thread holding the activity model with `wake` whenever the
/// broker asks something of it.
pub(crate) fn wake_with(wake: crate::session_runtime::Wake) {
    *port().wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
}

/// What the broker asked of the activity model since the last pump.
pub(crate) fn take_jobs() -> Vec<Job> {
    std::mem::take(&mut *port().jobs.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Say the served reads may read differently now.
pub(crate) fn changed() {
    port()
        .changes
        .send_modify(|revision| *revision = revision.wrapping_add(1));
}

/// Run `work` on the activity model's thread; its answer.
fn ask<T: Send + 'static>(
    work: impl FnOnce(&mut ShellOverlayHost) -> T + Send + 'static,
) -> Pending<T> {
    let (answer, answered) = pending();
    port()
        .jobs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Box::new(move |host| answer.complete(work(host))));
    let wake = port()
        .wake
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(wake) = wake {
        wake();
    }
    answered
}

#[allow(
    clippy::unnecessary_box_returns,
    reason = "the broker carries a step's failure boxed"
)]
fn failure(code: &str, message: impl Into<String>) -> Box<ActionFailure> {
    Box::new(ActionFailure {
        code: code.into(),
        message: message.into(),
        details: None,
        retryable: false,
    })
}

/// The typed arguments an argument slot carries; none for none.
fn arguments_of(
    argument: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(argument) = argument else {
        return Ok(serde_json::Map::new());
    };
    let json = argument
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .ok_or("workspace activity takes typed arguments")?;
    serde_json::from_str(json).map_err(|error| error.to_string())
}

/// The broker's door to the activity model.
pub(crate) fn broker_door() -> Arc<dyn HostStepServices> {
    Arc::new(BrokerDoor)
}

struct BrokerDoor;

/// The reads, followed live.
const READS: [&str; 5] = [
    wire::FEED,
    wire::SUMMARY,
    wire::REPLAY,
    wire::DETAIL,
    wire::FOCUS,
];

/// The writes only the person at this device asks for: what they saw,
/// which sources they hear about each morning.
const PERSONAL: [&str; 4] = [
    wire::MARK_VIEWED,
    wire::FOCUSED,
    wire::CHOOSE_SOURCE,
    wire::SOURCE,
];

fn descriptor(action: &str, label: &str, kind: OperationKind) -> ActionDescriptor {
    let contract = if READS.contains(&action) {
        ActionContract::live_view(serde_json::json!({ "type": "object" }))
    } else if kind == OperationKind::View {
        ActionContract::view()
    } else {
        ActionContract {
            kind: ActionKind::Undoable,
            ..ActionContract::default()
        }
    };
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new(wire::RESOURCE, action),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("shell-activity-title"),
        action_label: t(label),
        description: None,
        locus: ExecutionLocus::default(),
        kind,
        contract,
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

impl HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        wire::APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        use OperationKind::{Document, View};
        [
            (wire::FEED, "host-activity-action-feed", View),
            (wire::SUMMARY, "host-activity-action-summary", View),
            (wire::REPLAY, "host-activity-action-replay", View),
            (wire::DETAIL, "host-activity-action-detail", View),
            (wire::FOCUS, "host-activity-action-focus", View),
            (wire::MARK_VIEWED, "host-activity-action-mark-viewed", View),
            (wire::FOCUSED, "host-activity-action-focused", View),
            (wire::NAME, "host-activity-action-name", Document),
            (wire::SPLIT, "host-activity-action-split", Document),
            (wire::MERGE, "host-activity-action-merge", Document),
            (
                wire::MOVE_BOUNDARY,
                "host-activity-action-move-boundary",
                Document,
            ),
            (wire::UNDO, "host-activity-action-undo", Document),
            (wire::RESTORE, "host-activity-action-restore", Document),
            (wire::UNDO_RUN, "host-activity-action-undo-run", Document),
            (wire::OPEN, "host-activity-action-open", View),
            (
                wire::CHOOSE_SOURCE,
                "host-activity-action-choose-source",
                View,
            ),
            (wire::SOURCE, "host-activity-action-source", View),
        ]
        .into_iter()
        .map(|(action, label, kind)| descriptor(action, label, kind))
        .collect()
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let action = request.address.action.clone();
        if PERSONAL.contains(&action.as_str()) && request.actor.is_agent() {
            return Err(t("host-activity-personal-only"));
        }
        let arguments = arguments_of(request.argument)?;
        let room = request.room.to_owned();
        Ok(ask(move |host| {
            host.act_on_activity(&room, &action, &arguments)
        }))
    }

    fn watch(&self, request: &HostStepRequest<'_>) -> Result<LiveView, String> {
        let action = request.address.action.clone();
        if !READS.contains(&action.as_str()) {
            return Err("this step has no live result".into());
        }
        let arguments = arguments_of(request.argument)?;
        if action == wire::FEED
            && request.actor.is_agent()
            && serde_json::from_value::<wire::FeedQuery>(serde_json::Value::Object(
                arguments.clone(),
            ))
            .is_ok_and(|query| query.scope == wire::Scope::AllWorkspaces)
        {
            return Err(t("host-activity-personal-only"));
        }
        let room = request.room.to_owned();
        // Only the person reading what is new makes it seen; an agent that
        // reads the summary changes nothing for them.
        let shown = (action == wire::SUMMARY && !request.actor.is_agent()).then(|| {
            let begun = room.clone();
            drop(ask(move |host| host.begin_activity_summary(&begun)));
            SummaryShown(room.clone())
        });
        let last: Arc<Mutex<Option<serde_json::Value>>> = Arc::default();
        let read = move || -> Pending<LiveRead> {
            let _shown = &shown;
            let (room, action, arguments) = (room.clone(), action.clone(), arguments.clone());
            let last = Arc::clone(&last);
            ask(move |host| host.read_activity(&room, &action, &arguments)).map(move |read| {
                let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
                match read {
                    Ok(Some(value)) => {
                        *last = Some(value.clone());
                        Ok(value)
                    }
                    // The workspace is not the one shown: what was read of
                    // it while it was, or that it is still to be read.
                    Ok(None) => Ok(last
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({ "pending": true }))),
                    Err(error) => Err(error),
                }
            })
        };
        Ok(LiveView {
            read: Box::new(read),
            changes: port().changes.subscribe(),
        })
    }
}

/// The person is reading what is new in a room: once they stop, what they
/// were shown counts as seen.
struct SummaryShown(String);

impl Drop for SummaryShown {
    fn drop(&mut self) {
        let room = std::mem::take(&mut self.0);
        drop(ask(move |host| host.end_activity_summary(&room)));
    }
}

/// A write that could not run, said as the person reads it.
pub(crate) fn refused(message: impl Into<String>) -> HostStepOutcome {
    Err(failure("activity_refused", message))
}

/// A read that could not run.
#[allow(
    clippy::unnecessary_box_returns,
    reason = "the broker carries a read's failure boxed"
)]
pub(crate) fn unreadable(message: impl Into<String>) -> Box<ActionFailure> {
    failure("activity_unreadable", message)
}

#[cfg(test)]
#[path = "activity_provider_tests.rs"]
mod tests;

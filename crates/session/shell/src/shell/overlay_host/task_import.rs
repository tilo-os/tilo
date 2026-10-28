//! Moving the tasks a workspace's room kept into its Tasks tracker.
//!
//! Once a session, for each workspace the person opens: the tasks the room
//! still keeps that the tracker has not imported go to the tracker's own
//! `import`, invoked as the person like any action. Importing is
//! idempotent, so two devices doing it at once make one issue of each.

use std::collections::HashSet;

use serde_json::{Map, Value};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHostError, InvocationReceipt, InvokeArgument, InvokeRequest, Pending, PendingPoll,
    ReceiptStatus,
};

use crate::shell::certified_action_session::{ActionSessionSource, DesktopActionSession};
use crate::shell::search::invocation_wake::{poll, waker, watch};

/// The rooms looked at this session, and the import under way.
#[derive(Default)]
pub(super) struct TaskImports {
    looked: HashSet<crate::shell::state::RoomId>,
    going: Option<Going>,
}

struct Going {
    room: String,
    tasks: usize,
    /// Kept so the invocation lives until it answers.
    _session: DesktopActionSession,
    answer: Pending<Result<InvocationReceipt, ActionHostError>>,
}

impl TaskImports {
    /// Import what `room` still keeps, the first time it is seen this
    /// session, as `person`.
    pub(super) fn pump(
        &mut self,
        room: crate::shell::state::RoomId,
        person: &crate::shell::state::UserId,
        tracker: &crate::shell::broker_services::NativeTrackerRuns,
        source: impl FnOnce() -> Option<ActionSessionSource>,
    ) {
        self.settle();
        if self.going.is_some() || self.looked.contains(&room) {
            return;
        }
        let Some(source) = source() else {
            return;
        };
        self.looked.insert(room);
        let room = &room.to_string();
        let tasks = match tracker.unimported(room) {
            Ok(tasks) if tasks.is_empty() => return,
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::debug!(%error, room, "the room's tasks could not be read for import");
                return;
            }
        };
        let Ok(session) = source.open(room, Some(person)) else {
            return;
        };
        let count = tasks.len();
        let Ok(tasks) = serde_json::to_value(tasks) else {
            return;
        };
        let answer = watch(session.invoke(import_request(room, tasks)));
        self.going = Some(Going {
            room: room.to_owned(),
            tasks: count,
            _session: session,
            answer,
        });
    }

    fn settle(&mut self) {
        let Some(going) = &mut self.going else {
            return;
        };
        match poll(&mut going.answer, &waker()) {
            PendingPoll::Waiting => return,
            PendingPoll::Ready(Ok(InvocationReceipt {
                status: ReceiptStatus::Completed { .. } | ReceiptStatus::Accepted,
                ..
            })) => {
                tracing::info!(room = %going.room, tasks = going.tasks, "imported the room's tasks");
            }
            PendingPoll::Ready(Ok(InvocationReceipt { status, .. })) => {
                tracing::warn!(room = %going.room, ?status, "the room's tasks were not imported");
            }
            PendingPoll::Ready(Err(error)) => {
                tracing::warn!(room = %going.room, ?error, "the room's tasks were not imported");
            }
            PendingPoll::Abandoned => {}
        }
        self.going = None;
    }
}

fn import_request(room: &str, tasks: Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::application_action_target(
            room,
            tilo_action_broker::services::TRACKER_APP,
            tilo_action_broker::services::TRACKER_RESOURCE.to_owned(),
            "import".to_owned(),
        ),
        argument: InvokeArgument::Json(Map::from_iter([("tasks".to_owned(), tasks)])),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        appearance: None,
        wait_ms: Some(1_000),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, reason = "in a test these are the assertion")]

    use super::import_request;

    #[test]
    fn the_import_is_the_trackers_own_action_on_its_record() {
        let request = import_request("room", serde_json::json!([{"task": "t1"}]));
        assert_eq!(
            (
                request.target.address.resource.as_str(),
                request.target.address.action.as_str()
            ),
            ("tasks.tracker", "import")
        );
        let tilo_actions::remote::InvokeArgument::Json(arguments) = request.argument else {
            panic!("the tasks travel as the action's argument");
        };
        assert_eq!(arguments["tasks"][0]["task"], "t1");
    }
}

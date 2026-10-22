//! The Tasks app's tracker, read from the room's committed record for the
//! runs of its issues. Nothing here writes it: runs move only by the
//! tracker's own actions.

use std::collections::HashMap;
use std::sync::Mutex;

use tasks::model::{ConfirmationState, Issue, MessageKind, Tracker, issue_resource};
use tilo_action_broker::services::{
    NativeTaskActionRequirement, TRACKER_APP, TrackerAnswer, TrackerDirection, TrackerQuestion,
    TrackerRun, TrackerRuns,
};
use tilo_lock::MutexRecover as _;
use tilo_resource::{Id, Reader};

use super::NativeOwnerCommitService;

/// The tracker's record: the app's one record of the workspace.
const TRACKER_RECORD: &str = "";
/// The largest tracker the broker reads.
const MAX_TRACKER_BYTES: usize = 16 * 1024 * 1024;

/// The revision a room's tracker was read at, and the runs read from it.
type ReadRuns = ([u8; 32], Vec<TrackerRun>);

/// The room's tracker, read through its owner, and the runs last read from
/// each room's revision.
pub(crate) struct NativeTrackerRuns {
    pub(crate) owner: NativeOwnerCommitService,
    read: Mutex<HashMap<String, ReadRuns>>,
}

impl NativeTrackerRuns {
    pub(crate) fn new(owner: NativeOwnerCommitService) -> Self {
        Self {
            owner,
            read: Mutex::new(HashMap::new()),
        }
    }
}

/// The runs `tracker` holds: every issue with a run, as the broker reads it.
#[must_use]
pub(crate) fn runs_of(tracker: &Tracker) -> Vec<TrackerRun> {
    tracker
        .issues
        .iter()
        .filter_map(|(id, issue)| run_of(id, issue))
        .collect()
}

fn run_of(id: &Id, issue: &Issue) -> Option<TrackerRun> {
    let run = issue.run.as_ref()?;
    let agent = run.agent.0.strip_prefix("agent:").unwrap_or(&run.agent.0);
    let directions = issue
        .comments
        .iter()
        .filter(|(_, message)| message.kind == MessageKind::ToRun && message.run == run.epoch)
        .map(|(message_id, message)| TrackerDirection {
            id: message_id.to_string(),
            author: message.author.clone(),
            text: message.text.clone(),
            at_ms: message.at_ms,
            incorporated: message.incorporated,
        })
        .collect();
    let question = run.confirmation.as_ref().map(|question| TrackerQuestion {
        id: question.id.clone(),
        app: question.app.clone(),
        resource: question.resource.clone(),
        action: question.action.clone(),
        arguments_digest: question.arguments_digest.clone(),
        draft: question.draft.clone(),
        answer: match question.state {
            ConfirmationState::Waiting => TrackerAnswer::Waiting,
            ConfirmationState::Confirmed => TrackerAnswer::Confirmed {
                edits: question.edits.clone(),
            },
            ConfirmationState::Declined => TrackerAnswer::Declined {
                reason: question.reason.clone(),
            },
        },
    });
    Some(TrackerRun {
        issue: issue_resource(id),
        title: issue.title.clone(),
        instruction: issue.instruction(),
        agent: agent.to_owned(),
        epoch: run.epoch,
        starter: run.starter.clone(),
        active: run.phase.active(),
        budget: issue
            .budget
            .map(|budget| collab_types::task_budget::TaskExecutionBudget {
                turn_ms: u32::try_from(budget.turn_ms).unwrap_or(u32::MAX),
                total_ms: u32::try_from(budget.total_ms).unwrap_or(u32::MAX),
            }),
        subject: issue
            .subject
            .as_ref()
            .and_then(|subject| serde_json::from_value(subject.clone()).ok()),
        model: run
            .model
            .as_ref()
            .map(|model| runtime_collab::TaskRunModel {
                profile: model.profile.clone(),
                model: model.model.clone(),
            }),
        directions,
        question,
    })
}

impl TrackerRuns for NativeTrackerRuns {
    fn requires_room(&self, room: &str) -> bool {
        tilo_owner_commit::OwnerCommitService::requires_room(&self.owner, room)
    }

    fn runs(&self, room: &str) -> Result<Vec<TrackerRun>, String> {
        use super::super::native_workspace_owner::DocumentRead;
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid tracker room")?;
        let known = self
            .read
            .lock_recover()
            .get(room)
            .map(|(revision, _)| *revision);
        let owner = &self.owner.opened_owner("tracker owner unavailable")?;
        let read = owner.with_current(room_id, |view| {
            view.read_document_with_history(
                view.device().device_public_key,
                (TRACKER_APP, TRACKER_RECORD),
                known,
                MAX_TRACKER_BYTES,
                std::time::SystemTime::now(),
                &mut |envelope| {
                    self.owner
                        .history_keys
                        .as_ref()
                        .and_then(tilo_action_broker::channel_encryption::ChannelKeyProvider::get)
                        .ok_or("tracker history keys unavailable")?
                        .open_history_write(room_id, envelope)
                },
            )
        });
        let mut held = self.read.lock_recover();
        match read {
            Ok(DocumentRead::Captured(document)) => {
                let doc = automerge::AutoCommit::load(&document.bytes)
                    .map_err(|error| format!("the tracker does not load: {error}"))?;
                let tracker = Reader::<Tracker>::read(&doc)
                    .map_err(|error| format!("the tracker does not read: {error}"))?;
                let runs = runs_of(tracker.state());
                held.insert(room.to_owned(), (document.revision, runs.clone()));
                Ok(runs)
            }
            Ok(DocumentRead::Unchanged(_)) => Ok(held
                .get(room)
                .map(|(_, runs)| runs.clone())
                .unwrap_or_default()),
            // A room with no tracker yet holds no runs.
            Ok(DocumentRead::CheckpointPending) | Err(_) if known.is_none() => Ok(Vec::new()),
            Ok(DocumentRead::CheckpointPending) => Err("the tracker is between checkpoints".into()),
            Err(error) => Err(error),
        }
    }

    fn authorize_action(
        &self,
        room: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid tracker room")?;
        let owner = &self.owner.opened_owner("tracker owner unavailable")?;
        owner.with_current(room_id, |view| {
            let now = std::time::SystemTime::now();
            view.peer()
                .workspace()
                .check_task_execution_actor(&run.agent, None, now)?;
            requirement.check_tracker(view.peer(), room, run, now)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion"
    )]

    use tasks::model::{Tracker, tracker};
    use tilo_resource::Replica;

    use super::runs_of;

    #[test]
    fn an_issue_assigned_to_an_agent_is_a_run_once_a_person_approves_it() {
        let mut person = Replica::<Tracker>::new().as_author("ada");
        person
            .apply(Tracker::CREATE.with(tracker::Create {
                title: "Tidy the docs".into(),
                parent: None,
                body: Some("Keep the headings".into()),
                status: None,
                assignee: Some("agent:claude".into()),
            }))
            .unwrap();
        assert!(runs_of(&person.state().unwrap()).is_empty());
        let issue = person
            .state()
            .unwrap()
            .issues
            .ids()
            .next()
            .cloned()
            .unwrap();
        person
            .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
                issue: issue.clone(),
            }))
            .unwrap();
        person
            .apply(Tracker::SEND_MESSAGE.with(tracker::SendMessage {
                issue,
                text: "Use short lines".into(),
            }))
            .unwrap();
        let runs = runs_of(&person.state().unwrap());
        let [run] = runs.as_slice() else {
            panic!("one run: {runs:?}");
        };
        assert_eq!(
            (
                run.agent.as_str(),
                run.starter.as_str(),
                run.epoch,
                run.active
            ),
            ("claude", "ada", 1, true)
        );
        assert!(
            run.issue
                .starts_with(tilo_action_broker::services::TRACKER_ISSUE_PREFIX)
        );
        assert!(run.instruction.contains("Keep the headings"));
        assert_eq!(run.directions.len(), 1);
        assert_eq!((run.applied_revision(), run.latest_revision()), (0, 1));
    }
}

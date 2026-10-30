//! The Tasks app's tracker, read from the room's committed record for the
//! runs of its issues. Nothing here writes it: runs move only by the
//! tracker's own actions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tasks::model::{
    ConfirmationState, Issue, MessageKind, ModelChoice, RunPhase, Tracker, issue_resource,
};
use tilo_action_broker::services::{
    NativeTaskActionRequirement, TRACKER_APP, TRACKER_RECORD, TrackerAnswer, TrackerDirection,
    TrackerQuestion, TrackerRun, TrackerRuns,
};
use tilo_lock::MutexRecover as _;
use tilo_resource::{Id, Reader};

use super::NativeOwnerCommitService;

/// The largest tracker the broker reads.
const MAX_TRACKER_BYTES: usize = 16 * 1024 * 1024;
/// How far up a chain of parents a run's model is looked for. A tracker
/// cannot hold a cycle, so this only bounds a walk that never ends.
const MAX_PARENT_DEPTH: usize = 64;

/// A room's tracker as read at one revision.
type Read = ([u8; 32], Arc<TrackerView>);

/// What the shell reads of a room's tracker: its runs, as the broker and as
/// the composer read them.
#[derive(Debug, Default)]
pub(crate) struct TrackerView {
    pub runs: Vec<TrackerRun>,
    pub followed: Vec<FollowedRun>,
}

impl TrackerView {
    fn of(tracker: &Tracker) -> Self {
        Self {
            runs: runs_of(tracker),
            followed: followed_runs(tracker),
        }
    }
}

/// The room's tracker, read through its owner, as last read at each room's
/// revision.
pub(crate) struct NativeTrackerRuns {
    pub(crate) owner: NativeOwnerCommitService,
    read: Mutex<HashMap<String, Read>>,
    /// What each going run's current model turn streams, by room and
    /// issue: its epoch and the latest snapshot. Never written anywhere.
    live: Mutex<HashMap<(String, String), (u64, runtime_collab::TaskPresentation)>>,
}

/// How many of a run's own reports a follower is shown: the steps so far.
const SHOWN_STEPS: usize = 4;

/// A run of an issue as whoever follows it sees it: the composer that asked
/// for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FollowedRun {
    /// The issue's address (`tasks.tracker/issues/<id>`).
    pub issue: String,
    pub title: String,
    /// The agent, as the door names it (`agent:<id>`).
    pub agent: String,
    pub starter: String,
    pub epoch: u64,
    pub phase: RunPhase,
    /// The messages sent to the run, and how many of them its agent took in.
    pub messages: u64,
    pub incorporated: u64,
    /// What its agent said last in the run.
    pub said: Option<String>,
    /// What its agent reported in the run so far, oldest first: its steps,
    /// the last few of them.
    pub steps: Vec<String>,
    /// The programs it ran so far, oldest first, with what each answered:
    /// the last [`SHOWN_PROGRAMS`] of them.
    pub programs: Vec<tilo_reference::ProgramStep>,
    /// The view it answered with, when it answered with one: the resource
    /// keeping it.
    pub answer: Option<tilo_reference::ResourceAt>,
}

/// How many of a run's programs a follower is shown: enough to follow a
/// task's work, bounded so a long task does not grow the composer without end.
const SHOWN_PROGRAMS: usize = 24;

/// Every issue's latest run in `tracker`, as it is followed.
#[must_use]
fn followed_runs(tracker: &Tracker) -> Vec<FollowedRun> {
    tracker
        .issues
        .iter()
        .filter_map(|(id, issue)| {
            let run = issue.run.as_ref()?;
            let of_run = |kind| {
                issue
                    .comments
                    .iter()
                    .map(|(_, message)| message)
                    .filter(move |message| message.kind == kind && message.run == run.epoch)
            };
            let count = |messages: usize| u64::try_from(messages).unwrap_or(u64::MAX);
            Some(FollowedRun {
                issue: issue_resource(id),
                title: issue.title.clone(),
                agent: run.agent.0.clone(),
                starter: run.starter.clone(),
                epoch: run.epoch,
                phase: run.phase,
                messages: count(of_run(MessageKind::ToRun).count()),
                incorporated: count(
                    of_run(MessageKind::ToRun)
                        .filter(|message| message.incorporated)
                        .count(),
                ),
                said: of_run(MessageKind::FromRun)
                    .next_back()
                    .map(|message| message.text.clone()),
                steps: {
                    let reported: Vec<String> = of_run(MessageKind::FromRun)
                        .map(|message| message.text.clone())
                        .collect();
                    let skip = reported.len().saturating_sub(SHOWN_STEPS);
                    reported.into_iter().skip(skip).collect()
                },
                programs: {
                    let ran: Vec<tilo_reference::ProgramStep> = of_run(MessageKind::Step)
                        .filter_map(|message| serde_json::from_str(&message.step).ok())
                        .collect();
                    let skip = ran.len().saturating_sub(SHOWN_PROGRAMS);
                    ran.into_iter().skip(skip).collect()
                },
                answer: of_run(MessageKind::Answer)
                    .next_back()
                    .and_then(|message| serde_json::from_str(&message.lookup).ok()),
            })
        })
        .collect()
}

impl NativeTrackerRuns {
    pub(crate) fn new(owner: NativeOwnerCommitService) -> Self {
        Self {
            owner,
            read: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
        }
    }

    /// What run `epoch` of `issue` in `room` streams now, while it goes.
    pub(crate) fn live(
        &self,
        room: &str,
        issue: &str,
        epoch: u64,
    ) -> Option<runtime_collab::TaskPresentation> {
        self.live
            .lock_recover()
            .get(&(room.to_owned(), issue.to_owned()))
            .filter(|(of, _)| *of == epoch)
            .map(|(_, presentation)| presentation.clone())
    }

    /// The room's tracker as committed now; `None` in a room with none yet.
    pub(crate) fn tracker(&self, room: &str) -> Result<Option<Arc<TrackerView>>, String> {
        use super::super::native_workspace_owner::DocumentRead;
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid tracker room")?;
        let known = self
            .read
            .lock_recover()
            .get(room)
            .map(|(revision, _)| *revision);
        // A sealed room's owner keeps no replica of its records: the tracker
        // is what the sealed writes this device holds of it make, as an
        // app's read of it is.
        let route = tilo_action_broker::route::CrdtRoute::record(
            TRACKER_APP,
            room.to_owned(),
            TRACKER_RECORD,
        );
        if self.owner.reads_sealed_writes(room_id, &route) {
            return self.sealed_tracker(room, room_id, &route, known);
        }
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
                let tracker = view_of(&doc)?;
                held.insert(room.to_owned(), (document.revision, tracker.clone()));
                Ok(Some(tracker))
            }
            Ok(DocumentRead::Unchanged(_)) => {
                Ok(held.get(room).map(|(_, tracker)| tracker.clone()))
            }
            // A room with no tracker yet holds no runs.
            Ok(DocumentRead::CheckpointPending) if known.is_none() => Ok(None),
            Err(error) if known.is_none() => {
                tracing::debug!(room, %error, "the room's tracker could not be read; it holds no runs yet");
                Ok(None)
            }
            Ok(DocumentRead::CheckpointPending) => Err("the tracker is between checkpoints".into()),
            Err(error) => Err(error),
        }
    }
}

impl NativeTrackerRuns {
    /// Sealed `room`'s tracker as the sealed writes this device holds of it
    /// make it; the one read last while the channel's keys are on their way.
    fn sealed_tracker(
        &self,
        room: &str,
        room_id: super::super::state::RoomId,
        route: &tilo_action_broker::route::CrdtRoute,
        known: Option<[u8; 32]>,
    ) -> Result<Option<Arc<TrackerView>>, String> {
        use super::committed_document::HeldSealed;
        let held_view = |held: &HashMap<String, Read>| held.get(room).map(|(_, view)| view.clone());
        match self
            .owner
            .held_sealed_record(room_id, route, MAX_TRACKER_BYTES)?
        {
            HeldSealed::Readable(mut doc) => {
                let revision = tilo_peer_state::committed_document_revision(&doc.get_heads());
                if known == Some(revision) {
                    return Ok(held_view(&self.read.lock_recover()));
                }
                let tracker = view_of(&doc)?;
                self.read
                    .lock_recover()
                    .insert(room.to_owned(), (revision, tracker.clone()));
                Ok(Some(tracker))
            }
            HeldSealed::Waiting => Ok(held_view(&self.read.lock_recover())),
            HeldSealed::Incomplete { missing } => Err(format!(
                "the sealed writes this device holds of the tracker build on {missing} \
                 change(s) it does not hold"
            )),
        }
    }
}

/// The tracker `doc` holds, as the shell reads it.
fn view_of(doc: &automerge::AutoCommit) -> Result<Arc<TrackerView>, String> {
    Ok(Arc::new(TrackerView::of(
        Reader::<Tracker>::read(doc)
            .map_err(|error| format!("the tracker does not read: {error}"))?
            .state(),
    )))
}

/// The runs `tracker` holds: every issue with a run, as the broker reads it.
#[must_use]
pub(crate) fn runs_of(tracker: &Tracker) -> Vec<TrackerRun> {
    tracker
        .issues
        .iter()
        .filter_map(|(id, issue)| run_of(tracker, id, issue))
        .collect()
}

/// The model a sub-issue's run plans with: its own, else its parent's, so a
/// part of a task works on the model the task was set to and not on
/// whichever model its own agent happens to plan with.
///
/// A run records the model it captured when it started, and an issue records
/// the one a person chose for it afterwards; either answers for the issue.
fn inherited_model(tracker: &Tracker, issue: &Issue) -> Option<ModelChoice> {
    let mut seen: usize = 0;
    let mut node = issue;
    loop {
        if let Some(model) = node
            .model
            .clone()
            .or_else(|| node.run.as_ref().and_then(|run| run.model.clone()))
        {
            return Some(model);
        }
        seen = seen.saturating_add(1);
        let parent = tracker.issues.get(node.parent.as_ref()?)?;
        if seen > MAX_PARENT_DEPTH {
            return None;
        }
        node = parent;
    }
}

/// The most characters of earlier conversation a run's instruction carries,
/// all messages together: the newest are kept and the older ones counted,
/// so a long chat stays within what a run's instruction may hold.
const CONVERSATION_CHARS: usize = 12 * 1024;
/// The most characters of one earlier message a run is told.
const SAID_CHARS: usize = 1_500;

/// `instruction` with the conversation before the run appended, as the
/// agent reads a request that continues one
/// ([`runtime_collab::INSTRUCTION_CONVERSATION`]): one line per message,
/// the person's and the agent's own told apart. A follow-up in a chat
/// ("now do the same for the other two") is understood through it.
fn with_conversation(instruction: String, said: &[tasks::model::Said]) -> String {
    if said.is_empty() {
        return instruction;
    }
    let mut kept = Vec::new();
    let mut room = CONVERSATION_CHARS;
    for message in said.iter().rev() {
        let mut text: String = message.text.chars().take(SAID_CHARS).collect();
        if text.len() < message.text.len() {
            text.push('…');
        }
        let line = format!(
            "\n{}{}",
            if message.agent { "You: " } else { "Person: " },
            text.replace('\n', "\n  ")
        );
        if line.len() > room {
            break;
        }
        room = room.saturating_sub(line.len());
        kept.push(line);
    }
    let mut out = instruction;
    out.push_str(runtime_collab::INSTRUCTION_CONVERSATION);
    let left_out = said.len().saturating_sub(kept.len());
    if left_out > 0 {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!("\n({left_out} earlier messages left out)"),
        );
    }
    for line in kept.into_iter().rev() {
        out.push_str(&line);
    }
    out
}

fn run_of(tracker: &Tracker, id: &Id, issue: &Issue) -> Option<TrackerRun> {
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
        before: question.preview.clone(),
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
        instruction: with_conversation(issue.instruction(), &issue.conversation_before(run.epoch)),
        agent: agent.to_owned(),
        epoch: run.epoch,
        starter: run.starter.clone(),
        active: run.phase.active(),
        paused_through: run.paused.as_ref().map(|paused| paused.through),
        budget: issue.budget.as_ref().map(|budget| {
            collab_types::task_budget::TaskExecutionBudget {
                turn_ms: u32::try_from(budget.turn_ms).unwrap_or(u32::MAX),
                total_ms: u32::try_from(budget.total_ms).unwrap_or(u32::MAX),
                max_turns: budget
                    .max_turns
                    .map(|turns| u32::try_from(turns).unwrap_or(u32::MAX)),
            }
        }),
        subject: issue
            .subject
            .as_ref()
            .and_then(|subject| serde_json::from_value(subject.clone()).ok()),
        model: inherited_model(tracker, issue).map(|model| runtime_collab::TaskRunModel {
            profile: model.profile.clone(),
            model: model.model,
        }),
        directions,
        question,
        // A delegation that does not decode is carried with why: the agent's
        // check refuses the run so, never reading it as a collaborator's.
        delegation: run.delegation.as_deref().map(|hex| {
            tilo_peer_state::join::task_delegation::TaskDelegation::from_hex(hex)
                .map(|delegation| delegation.encode())
                .map_err(|error| error.to_string())
        }),
    })
}

impl TrackerRuns for NativeTrackerRuns {
    /// Every room whose replica this device holds keeps its tracker here:
    /// one of committed documents and one of sealed channels alike, whose
    /// sealed writes [`Self::tracker`] opens with the room's history keys.
    /// Only the first used to count, so in a sealed room (a fresh person's
    /// own) a task agent's join found "the room's tracker is unavailable"
    /// and no run ever reached its agent.
    fn requires_room(&self, room: &str) -> bool {
        tilo_owner_commit::OwnerCommitService::records_effects(&self.owner, room)
    }

    /// A room whose documents this device commits confines its agents to
    /// their runs, as it always has. A room of sealed channels (a person's
    /// own) serves runs too now, but whether an agent there may also act on
    /// a grant alone (an agent on a box beside the desktop, through the
    /// door) is its own decision, not a side effect of reading the tracker:
    /// it stays as it was.
    fn confines_agents_to_tasks(&self, room: &str) -> bool {
        tilo_owner_commit::OwnerCommitService::requires_room(&self.owner, room)
    }

    /// Keep the newest snapshot of each run's turn: a later epoch, a later
    /// turn, or a later sequence of the same turn replaces it; an ended run
    /// is forgotten. Whoever follows the run is woken to show it.
    fn presented(
        &self,
        room: &str,
        issue: &str,
        epoch: u64,
        presentation: Option<&runtime_collab::TaskPresentation>,
    ) {
        let key = (room.to_owned(), issue.to_owned());
        let mut live = self.live.lock_recover();
        let Some(presentation) = presentation else {
            if live.get(&key).is_some_and(|(of, _)| *of <= epoch) {
                live.remove(&key);
            }
            return;
        };
        let newer = live.get(&key).is_none_or(|(of, known)| {
            epoch > *of
                || (epoch == *of
                    && (known.run != presentation.run || presentation.sequence > known.sequence))
        });
        if newer {
            live.insert(key, (epoch, presentation.clone()));
            drop(live);
            crate::shell::wake_compositor();
        }
    }

    fn runs(&self, room: &str) -> Result<Vec<TrackerRun>, String> {
        Ok(self
            .tracker(room)?
            .map(|tracker| tracker.runs.clone())
            .unwrap_or_default())
    }

    fn check_agent(&self, room: &str, run: &TrackerRun) -> Result<(), String> {
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid tracker room")?;
        let owner = &self.owner.opened_owner("tracker owner unavailable")?;
        owner.with_current(room_id, |view| {
            view.peer().workspace().check_admitted_agent(
                &run.agent,
                run.delegation()?,
                std::time::SystemTime::now(),
            )
        })
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
                .check_admitted_agent(&run.agent, run.delegation()?, now)?;
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

    use tasks::model::{ModelChoice, Tracker, tracker};
    use tilo_resource::Replica;

    use super::{inherited_model, runs_of};

    #[test]
    fn an_issue_assigned_to_an_agent_is_a_run_once_a_person_approves_it() {
        let mut person = Replica::<Tracker>::new().as_author("ada");
        person
            .apply(Tracker::CREATE.with(tracker::Create {
                title: "Tidy the docs".parse().expect("not blank"),
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
                text: "Use short lines".parse().expect("not blank"),
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

    /// A chat's run reaches its agent with the chat's whole budget: its turn
    /// ceiling as well as its time, and the tracker admits no more turns
    /// than the run's envelope does.
    #[test]
    fn a_chats_run_carries_its_turn_ceiling_to_the_agent() {
        assert_eq!(
            tasks::model::MAX_TURNS,
            u64::from(collab_types::task_budget::MAX_RUN_TURNS)
        );
        let mut person = Replica::<Tracker>::new().as_author("ada");
        person
            .apply(Tracker::ASK_AGENT.with(tracker::AskAgent {
                ask: tasks::model::AgentRequest {
                    id: tilo_resource::Id::from_raw("chat"),
                    title: "Review today's applications".into(),
                    body: None,
                    agent: "claude".into(),
                    model: None,
                    subject: None,
                    quick: false,
                    chat: true,
                    delegation: None,
                },
            }))
            .unwrap();
        let runs = runs_of(&person.state().unwrap());
        let [run] = runs.as_slice() else {
            panic!("one run: {runs:?}");
        };
        let budget = run.budget.expect("a chat has a budget");
        budget.validate().unwrap();
        assert_eq!(
            budget.max_turns.map(u64::from),
            Some(tasks::model::CHAT_TURNS)
        );
        assert_eq!(u64::from(budget.total_ms), tasks::model::MAX_TOTAL_MS);
    }

    /// A conversation goes on after a run ended: what the person tells the
    /// agent then starts the next run, and the broker hands that run the
    /// message as its one direction, never the earlier run's.
    #[test]
    fn a_message_after_a_run_ended_is_the_next_runs_direction() {
        let mut person = Replica::<Tracker>::new().as_author("ada");
        person
            .apply(Tracker::CREATE.with(tracker::Create {
                title: "Review the applications".parse().expect("not blank"),
                parent: None,
                body: None,
                status: None,
                assignee: Some("agent:claude".into()),
            }))
            .unwrap();
        let issue = person
            .state()
            .unwrap()
            .issues
            .ids()
            .next()
            .cloned()
            .unwrap();
        let tell = |text: &str| {
            Tracker::TELL_AGENT.with(tracker::TellAgent {
                issue: issue.clone(),
                text: text.parse().expect("not blank"),
            })
        };
        person.apply(tell("Start with the newest")).unwrap();
        let mut agent = person.fork().as_agent_for("agent:claude", "ada");
        agent
            .apply(Tracker::FINISH_RUN.with(tracker::FinishRun {
                issue: issue.clone(),
                outcome: tasks::model::RunOutcome::Done,
                summary: None,
            }))
            .unwrap();
        person.sync_with(&mut agent).unwrap();
        assert!(
            runs_of(&person.state().unwrap())
                .iter()
                .all(|run| !run.active)
        );

        person.apply(tell("Now draft the replies")).unwrap();
        let runs = runs_of(&person.state().unwrap());
        let [run] = runs.as_slice() else {
            panic!("one run: {runs:?}");
        };
        assert_eq!((run.epoch, run.active), (2, true));
        let directions: Vec<&str> = run
            .directions
            .iter()
            .map(|direction| direction.text.as_str())
            .collect();
        assert_eq!(directions, ["Now draft the replies"]);
    }

    /// A follow-up run is told the conversation before it: what the person
    /// asked and what the agent answered and did, oldest first, and not the
    /// message it is to act on now, which is its direction. A long chat is
    /// told its newest messages, the older ones counted.
    #[test]
    fn a_follow_up_run_is_told_the_conversation_before_it() {
        let mut person = Replica::<Tracker>::new().as_author("ada");
        person
            .apply(Tracker::CREATE.with(tracker::Create {
                title: "Review the applications".parse().expect("not blank"),
                parent: None,
                body: Some("Jane Doe, Ravi Patel and Mei Lin applied today".into()),
                status: None,
                assignee: Some("agent:claude".into()),
            }))
            .unwrap();
        let issue = person
            .state()
            .unwrap()
            .issues
            .ids()
            .next()
            .cloned()
            .unwrap();
        let tell = |text: &str| {
            Tracker::TELL_AGENT.with(tracker::TellAgent {
                issue: issue.clone(),
                text: text.parse().expect("not blank"),
            })
        };
        person.apply(tell("Start with Jane Doe")).unwrap();
        let mut agent = person.fork().as_agent_for("agent:claude", "ada");
        agent
            .apply(Tracker::REPORT_RUN.with(tracker::ReportRun {
                issue: issue.clone(),
                phase: tasks::model::RunPhase::Executing,
                text: Some("1 of 1: reviewed Jane Doe".parse().expect("not blank")),
            }))
            .unwrap();
        agent
            .apply(
                Tracker::FINISH_RUN.with(tracker::FinishRun {
                    issue: issue.clone(),
                    outcome: tasks::model::RunOutcome::Done,
                    summary: Some(
                        "Reviewed Jane Doe and drafted a reply"
                            .parse()
                            .expect("not blank"),
                    ),
                }),
            )
            .unwrap();
        person.sync_with(&mut agent).unwrap();
        person
            .apply(tell("Now do the same for the other two"))
            .unwrap();
        let runs = runs_of(&person.state().unwrap());
        let [run] = runs.as_slice() else {
            panic!("one run: {runs:?}");
        };
        let conversation = run
            .instruction
            .split(runtime_collab::INSTRUCTION_CONVERSATION)
            .nth(1)
            .expect("the run is told the conversation");
        assert_eq!(
            conversation,
            "\nPerson: Start with Jane Doe\nYou: 1 of 1: reviewed Jane Doe\nYou: Reviewed Jane Doe and drafted a reply"
        );
        assert!(
            run.instruction
                .starts_with("Review the applications\n\nJane Doe, Ravi Patel")
        );
        assert!(
            !conversation.contains("other two"),
            "its own message is its direction"
        );

        let many: Vec<tasks::model::Said> = (0..400)
            .map(|index| tasks::model::Said {
                agent: index % 2 == 1,
                text: format!("message {index} {}", "x".repeat(100)),
            })
            .collect();
        let told = super::with_conversation("Go on".into(), &many);
        assert!(
            told.len() < super::CONVERSATION_CHARS + 200,
            "{}",
            told.len()
        );
        assert!(told.contains("earlier messages left out"));
        assert!(told.ends_with(&many[399].text), "the newest is kept");
    }

    /// A part of a task works on the model the task was set to, so every step
    /// of one job records the same provider and model.
    #[test]
    fn a_subtask_of_a_task_runs_on_the_model_its_task_chose() {
        let mut person = Replica::<Tracker>::new().as_author("ada");
        person
            .apply(Tracker::CREATE.with(tracker::Create {
                title: "Ship the release".parse().expect("not blank"),
                parent: None,
                body: None,
                status: None,
                assignee: None,
            }))
            .unwrap();
        let task = person
            .state()
            .unwrap()
            .issues
            .ids()
            .next()
            .cloned()
            .unwrap();
        person
            .apply(Tracker::CHOOSE_MODEL.with(tracker::ChooseModel {
                issue: task.clone(),
                model: Some(ModelChoice {
                    profile: "work".into(),
                    model: "large".into(),
                }),
            }))
            .unwrap();
        person
            .apply(Tracker::CREATE.with(tracker::Create {
                title: "Write the notes".parse().expect("not blank"),
                parent: Some(task),
                body: None,
                status: None,
                assignee: None,
            }))
            .unwrap();
        let state = person.state().unwrap();
        let (part, subtask) = state
            .issues
            .iter()
            .find(|(_, issue)| issue.parent.is_some())
            .unwrap();
        let part = part.clone();
        assert!(
            subtask.run.is_none() && subtask.model.is_none(),
            "the part chose nothing of its own"
        );
        let chosen = inherited_model(&state, subtask).expect("the task's model is inherited");
        assert_eq!(
            (chosen.profile.as_str(), chosen.model.as_str()),
            ("work", "large")
        );
        // The part's own choice answers for it, and its own run's capture
        // does too, before the task's is reached.
        person
            .apply(Tracker::CHOOSE_MODEL.with(tracker::ChooseModel {
                issue: part.clone(),
                model: Some(ModelChoice {
                    profile: "fast".into(),
                    model: "small".into(),
                }),
            }))
            .unwrap();
        let state = person.state().unwrap();
        let subtask = state.issues.get(&part).unwrap();
        let chosen = inherited_model(&state, subtask).expect("the part's own model answers first");
        assert_eq!(
            (chosen.profile.as_str(), chosen.model.as_str()),
            ("fast", "small")
        );
    }
}

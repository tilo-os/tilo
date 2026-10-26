//! Host-owned adaptation of authenticated task channels to the canonical owner.
//! No caller certificate, actor label or new grant is accepted from task arguments.

use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "task_execution_tests.rs"]
mod tests;

#[derive(Clone, Debug)]
pub struct NativeTaskBinding {
    pub task_id: String,
    pub agent_id: String,
    pub epoch: u64,
    pub revision: u64,
    /// The person whose approval started the run, as the owner recorded it
    /// from their device's certificate: the run's actions are admitted as
    /// the agent acting for them. `None` where only a record of an earlier
    /// admission is at hand; the owner's check reads the live approval.
    pub starter: Option<String>,
}

impl NativeTaskBinding {
    /// Who the run's actions are admitted as in the broker: the agent acting
    /// for its starter, carrying the delegation a person's device gives its
    /// agents, working on the issue. It may do what the starter may, held to
    /// the answers the room keeps for the agent and the issue, never to the
    /// preset it was admitted with, which bounds only what it does on its
    /// own. Effects stay attributed to the agent.
    ///
    /// # Errors
    /// The binding does not name its starter.
    pub fn actor(&self) -> Result<crate::collab_broker_capnp::BrokerActor, String> {
        let starter = self.starter.clone().ok_or("native task starter absent")?;
        Ok(
            crate::collab_broker_capnp::BrokerActor::agent(format!("agent:{}", self.agent_id))
                .for_person(starter)
                .session_delegated()
                .working_on(&self.task_id),
        )
    }
}

/// A current published contract requirement, never a grant from task arguments.
#[derive(Clone, Debug)]
pub struct NativeTaskActionRequirement {
    /// Supplied by the broker's authenticated run channel, never action arguments.
    pub binding: NativeTaskBinding,
    pub app: String,
    pub resource_kind: String,
    pub resource: String,
    pub action: String,
    pub need: tilo_actions::ActionNeed,
    /// The action's contract asks for confirmation with these arguments
    /// (it sends, publishes or deletes for good): the agent's person is
    /// asked first unless they allowed this very action.
    pub consequential: bool,
    /// The action needs its person attending ([`tilo_actions::ATTENDED_TAG`]):
    /// it asks them every time, whatever they answered, even for this very
    /// action.
    pub attended: bool,
    /// The person the run acts for confirmed this invocation, or it was
    /// admitted on their confirmation: an answer that asks first is
    /// answered.
    pub confirmed: bool,
}

impl NativeTaskActionRequirement {
    /// Apply the room's grants to `requirement` for a run of the Tasks app's
    /// tracker: the agent working on the issue, acting for the run's starter
    /// on the starter's live grants. Grants naming the issue decide for the
    /// run as the agent's own do, the most specific first. The tracker
    /// freezes no scope: the starter's live grants and the agent's and
    /// issue's bound it.
    pub fn check_tracker(
        &self,
        peer: &tilo_peer_state::StatePeer,
        room: &str,
        run: &TrackerRun,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        use tilo_action_policy::{Answer, Authority, Context, roles};
        let doc = peer
            .admitted_room()
            .ok_or("tracker action policy unavailable")?;
        let policy = tilo_desktop_model::projection::room_policy_of(doc, room);
        let node = tilo_actions::action_node(
            &policy.node(),
            &self.app,
            &self.resource_kind,
            &self.resource,
            self.need.field.as_deref(),
            &self.action,
        );
        let cx = Context::at_time(now);
        let binding = &self.binding;
        if binding.agent_id != run.agent || binding.task_id != run.issue || self.need.privileged {
            return Err("tracker run binding invalid or action privileged".into());
        }
        if !run.active || binding.epoch != run.epoch {
            return Err("this run has ended or was replaced".into());
        }
        let delegation = roles::agent_delegation(room, &run.agent, &run.starter);
        let issued = tilo_action_policy::Subject::Task(run.issue.clone());
        let answers = tilo_action_policy::agent_answers::Answers::of(&policy.grants, &run.issue);
        let party = delegation.party(&policy.grants).also_as(issued.clone());
        // Nobody told the issue's work what it may do yet: it is answered
        // for the person's decision to start it, and nothing else, so the
        // room's grants for the agent do not stand in for an answer no one
        // gave it.
        let party = if answers.covers(&node, self.need.kind, &cx) {
            party
        } else {
            party.unanswered_issue(issued)
        };
        let verdict = Authority::person(party).decide(&node, self.need.kind, &cx);
        let foreign = roles::foreign_device_bound();
        let starter = policy
            .person_on_device(&run.starter, false, &foreign)
            .decide(&node, self.need.kind, &cx)
            .answer;
        if !policy.is_member(&run.starter) || starter == Answer::Deny {
            return Err("this run's action is no longer authorized by its starter".into());
        }
        self.answer(&verdict, starter)
    }

    /// The answer of every party to the action: the agent's and the issue's
    /// grants (as `verdict` decided them) and the starter's (`starter`).
    /// Deny refuses; Ask pauses for the person unless this invocation is
    /// [`Self::confirmed`].
    ///
    /// Undo is a system service, so a change that can be taken back (an
    /// undoable action, not one that sends, buys or deletes for good) runs
    /// in the run's scope as well as it runs there for a person: a person's
    /// own grant that says "ask" asks them when *they* press it, their
    /// gesture being their answer, and never pauses a run they started on a
    /// change it can undo. What the agent's and the issue's own grants say
    /// still decides: an answer asking the agent first keeps asking.
    fn answer(
        &self,
        verdict: &tilo_action_policy::Verdict<'_>,
        starter: tilo_action_policy::Answer,
    ) -> Result<(), String> {
        use tilo_action_policy::{Answer, agent_answers};
        let agent_answer = verdict.parts.first().map_or(Answer::Deny, |decision| {
            agent_answers::held_to_contract(decision, self.consequential)
        });
        let agent_answer = if self.attended {
            agent_answer.min(Answer::Ask)
        } else {
            agent_answer
        };
        if agent_answer == Answer::Deny {
            return Err(not_allowed(self.need.kind, &agent_basis(verdict)));
        }
        let undoable =
            !self.consequential && self.need.kind == tilo_action_policy::ActionKind::Undoable;
        let declared = agent_answer.min(starter);
        let answer = if declared == Answer::Ask
            && undoable
            && agent_answer == Answer::Allow
            && starter == Answer::Ask
        {
            Answer::Allow
        } else {
            declared
        };
        if answer == Answer::Ask && !self.confirmed {
            let whose = if agent_answer == Answer::Ask {
                agent_basis(verdict)
            } else {
                "the person's own grants".to_owned()
            };
            return Err(format!(
                "{}: {} asks a person first: {whose} answer \"ask\" for {}",
                tilo_actions::draft::AWAITING_CONFIRMATION,
                self.action,
                self.need.kind.phrase()
            ));
        }
        Ok(())
    }

    /// Whether `error`, from [`Self::check_tracker`], says the action waits for the
    /// person's confirmation rather than being refused.
    #[must_use]
    pub fn awaits_confirmation(error: &str) -> bool {
        error.contains(tilo_actions::draft::AWAITING_CONFIRMATION)
    }
}

/// The agent's grants that decided `verdict`, by the grant, so the agent (and
/// whoever reads its report) learns why rather than retrying.
fn agent_basis(verdict: &tilo_action_policy::Verdict<'_>) -> String {
    match verdict.parts.first().map(|part| part.basis) {
        Some(tilo_action_policy::Basis::Grant(grant)) => format!(
            "the agent's grants in this workspace (grant {} on {})",
            grant.id, grant.node
        ),
        Some(tilo_action_policy::Basis::Unanswered) => format!(
            "nobody has said what the work on this issue may do about {}",
            verdict.node
        ),
        _ => format!(
            "the agent's grants in this workspace (no grant covers {})",
            verdict.node
        ),
    }
}

/// Why a task run may not take an action `whose` grants deny.
fn not_allowed(kind: tilo_action_policy::ActionKind, whose: &str) -> String {
    format!(
        "native task action is not allowed: {whose} do not allow {}",
        kind.phrase()
    )
}

/// The resource prefix of an issue in the Tasks app's tracker. A run whose
/// task id is an issue's address is that issue's run, kept in the Tasks
/// app's record ([`TrackerRuns`]) rather than in the room's task records.
pub const TRACKER_ISSUE_PREFIX: &str = "tasks.tracker/issues/";
/// The application that keeps the tracker.
pub const TRACKER_APP: &str = "so.tilo.tasks";
/// The tracker's own resource, where its verbs are invoked with the issue
/// they act on.
pub const TRACKER_RESOURCE: &str = "tasks.tracker";

#[must_use]
pub fn is_tracker_issue(task_id: &str) -> bool {
    task_id.starts_with(TRACKER_ISSUE_PREFIX)
}

/// A message to a run, in the issue's thread: steering the run reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerDirection {
    /// The message's id in the thread.
    pub id: String,
    pub author: String,
    pub text: String,
    pub at_ms: i64,
    /// Its run's agent already took it in.
    pub incorporated: bool,
}

/// What a person answered a run's question.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackerAnswer {
    Waiting,
    Confirmed { edits: serde_json::Value },
    Declined { reason: String },
}

/// The action a run asks its person about, as the run recorded it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackerQuestion {
    pub id: String,
    pub app: String,
    pub resource: String,
    pub action: String,
    /// Hex of [`tilo_actions::draft::arguments_digest`] of the arguments.
    pub arguments_digest: String,
    /// The [`tilo_actions::Draft`] the person was shown.
    pub draft: serde_json::Value,
    /// The change the step makes, before and after, where it previews one
    /// ([`Preview`]): the person decides on the difference rather than on
    /// the end state alone. Carried as the app recorded it beside the draft,
    /// so what the sheet puts up is what the run wrote.
    pub before: Option<serde_json::Value>,
    pub answer: TrackerAnswer,
}

/// What one side of a change preview shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSide {
    /// The tree, or the item, as it is (or will be) written.
    pub text: String,
    /// Its lines, for a person who cannot see characters line up.
    pub spoken: String,
}

/// The change a step makes, in tree form: the resource as it stands and as
/// it will stand. A person approving a bulk move sees what it becomes
/// before any of it happens; a script or a screen reader reads the same two
/// sides as text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub before: PreviewSide,
    pub after: PreviewSide,
}

/// The draft of `question` as a person reading it with a preview reads it:
/// the step as the run recorded it, and what the step before it left.
/// Nothing the asking agent sent decides either.
#[must_use]
pub fn confirmation_preview(question: &TrackerQuestion) -> (tilo_actions::Draft, Option<Preview>) {
    (
        serde_json::from_value(question.draft.clone()).unwrap_or_default(),
        // A preview this build cannot read is no preview: the person is
        // asked on the draft alone rather than on a difference shown wrong.
        question
            .before
            .clone()
            .and_then(|preview| serde_json::from_value(preview).ok()),
    )
}

/// An agent's run of one issue, as the Tasks app's record holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackerRun {
    /// The issue's resource address (`tasks.tracker/issues/<id>`).
    pub issue: String,
    pub title: String,
    /// What the agent is asked: the issue's title and description.
    pub instruction: String,
    /// The agent, by its bare id.
    pub agent: String,
    pub epoch: u64,
    /// The person who approved the run: the agent acts for them.
    pub starter: String,
    /// Whether the run is still going.
    pub active: bool,
    pub budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    /// What the issue is about, when it was asked about a selection: where
    /// to act, granting nothing.
    pub subject: Option<tilo_actions::SelectionSubject>,
    /// The model chosen for the run; the agent's own without.
    pub model: Option<runtime_collab::TaskRunModel>,
    /// The messages to this run, in thread order.
    pub directions: Vec<TrackerDirection>,
    /// What the run asks or last asked its person.
    pub question: Option<TrackerQuestion>,
}

impl TrackerRun {
    /// How many of the run's messages its agent took in: messages are taken
    /// in in order, so this is the revision the agent's steering reached.
    #[must_use]
    pub fn applied_revision(&self) -> u64 {
        let applied = self
            .directions
            .iter()
            .take_while(|direction| direction.incorporated)
            .count();
        u64::try_from(applied).unwrap_or(u64::MAX)
    }

    /// The revision of the run's newest message.
    #[must_use]
    pub fn latest_revision(&self) -> u64 {
        u64::try_from(self.directions.len()).unwrap_or(u64::MAX)
    }
}

/// The Tasks app's tracker, read for the runs it holds. Runs are written
/// only through the tracker's own actions, invoked by the broker as the
/// run's agent.
pub trait TrackerRuns: Send + Sync {
    fn requires_room(&self, room: &str) -> bool;
    /// Every run the room's tracker holds, going or ended.
    ///
    /// # Errors
    /// The room's tracker cannot be read now.
    fn runs(&self, room: &str) -> Result<Vec<TrackerRun>, String>;
    /// Whether `agent` is still admitted to `room`, as the room's owner
    /// holds it now: a removed agent reads its run no more.
    ///
    /// # Errors
    /// The agent is no longer admitted, or the room cannot be read.
    fn check_agent(&self, room: &str, agent: &str) -> Result<(), String>;
    /// Apply the room's grants for the run's agent, acting for the run's
    /// starter, to `requirement`, as [`NativeTaskActionRequirement::check_tracker`]
    /// does for a room task. Where any of them asks first, the error starts
    /// with [`tilo_actions::draft::AWAITING_CONFIRMATION`].
    fn authorize_action(
        &self,
        room: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String>;
}

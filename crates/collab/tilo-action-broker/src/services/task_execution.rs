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
    /// The turn the agent acts for: the person's message whose conversation it
    /// takes part in. Every op the run's agent makes carries it, so the
    /// receiving device decides it against that message; no turn yet means the
    /// run was asked before turns carried conversations.
    pub turn: Option<tilo_action_policy::turn::Turn>,
}

impl NativeTaskBinding {
    /// Who the run's actions are admitted as in the broker: the agent acting
    /// for its starter, carrying the delegation a person's device gives its
    /// agents, working on the issue. It may do what the starter may, held to
    /// the answers the workspace keeps for the agent and the issue, never to the
    /// preset it was admitted with, which bounds only what it does on its
    /// own. Effects stay attributed to the agent.
    pub fn actor(&self) -> Result<crate::collab_app_broker::BrokerActor, String> {
        let starter = self.starter.clone().ok_or("native task starter absent")?;
        Ok(
            crate::collab_app_broker::BrokerActor::agent(format!("agent:{}", self.agent_id))
                .for_person(starter)
                .session_delegated()
                .on_turn(self.turn.clone())
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
    /// Apply the workspace's grants to `requirement` for a run of the Tasks app's
    /// tracker: the agent working on the issue, acting for the run's starter
    /// on the starter's live grants. Grants naming the issue decide for the
    /// run as the agent's own do, the most specific first. The tracker
    /// freezes no scope: the starter's live grants and the agent's and
    /// issue's bound it.
    pub fn check_tracker(
        &self,
        peer: &tilo_peer_state::StatePeer,
        workspace: &str,
        run: &TrackerRun,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        let doc = peer
            .admitted_workspace()
            .ok_or("tracker action policy unavailable")?;
        let policy = tilo_desktop_model::projection::workspace_policy_of(doc, workspace);
        self.check_tracker_policy(&policy, workspace, run, None, now)
    }

    /// [`Self::check_tracker`] for a workspace whose grants are already at hand,
    /// as the session hosting the run holds them, with what the receiving
    /// device looked up about the turn's cause (`looked`; `None` when its cause
    /// has not arrived here yet, which holds the op).
    ///
    /// # Errors
    /// The run's binding, its epoch, its turn or the workspace's grants refuse
    /// the action; a turn waiting for its cause says so
    /// ([`Self::awaits_cause`]).
    pub fn check_tracker_policy(
        &self,
        policy: &tilo_action_policy::WorkspacePolicy,
        workspace: &str,
        run: &TrackerRun,
        looked: Option<&tilo_action_policy::turn::Looked>,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        use tilo_action_policy::{Answer, Authority, Context, roles};
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
        let delegation = self.turn_authority(workspace, run, looked)?;
        let verdict = Authority::person(
            delegation
                .party(&policy.grants)
                .also_as(tilo_action_policy::Subject::Task(run.issue.clone())),
        )
        .decide(&node, self.need.kind, &cx);
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

    /// The authority the run acts on: the turn its agent takes part in, when
    /// the run names one, bounded by the starter's grants, the editor preset
    /// and the agent's own bound ([`tilo_action_policy::turn`]); a run asked
    /// before turns carried conversations acts on the editor preset alone.
    fn turn_authority(
        &self,
        workspace: &str,
        run: &TrackerRun,
        looked: Option<&tilo_action_policy::turn::Looked>,
    ) -> Result<tilo_action_policy::Bound, String> {
        use tilo_action_policy::{roles, turn};
        let Some(cause) = self.binding.turn.as_ref() else {
            return Ok(roles::agent_turn(workspace, &run.agent, &run.starter, &[]));
        };
        turn::authority(workspace, &run.agent, &run.starter, Some(cause), |_| {
            looked.cloned()
        })
        .map_err(|held| {
            if held.waits() {
                format!("{}: {}", tilo_actions::draft::AWAITING_CAUSE, held.reason())
            } else {
                format!("this run's turn does not authorize it: {}", held.reason())
            }
        })
    }

    /// Whether `error`, from [`Self::check_tracker`], says the action waits for the
    /// person's confirmation rather than being refused.
    #[must_use]
    pub fn awaits_confirmation(error: &str) -> bool {
        error.contains(tilo_actions::draft::AWAITING_CONFIRMATION)
    }

    /// Whether `error`, from [`Self::check_tracker`], says the action waits for
    /// the message that caused its turn to reach this device rather than being
    /// refused.
    #[must_use]
    pub fn awaits_cause(error: &str) -> bool {
        error.contains(tilo_actions::draft::AWAITING_CAUSE)
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
/// app's record ([`TrackerRuns`]) rather than in the workspace's task records.
pub const TRACKER_ISSUE_PREFIX: &str = "tasks.tracker/issues/";
/// The application that keeps the tracker.
pub const TRACKER_APP: &str = "so.tilo.tasks";
/// The tracker's own resource, where its verbs are invoked with the issue
/// they act on.
pub const TRACKER_RESOURCE: &str = "tasks.tracker";
/// The tracker's record: the app's one record of a workspace, its package primary.
pub const TRACKER_RECORD: &str = "";

/// A write on `route` was admitted to this device's channels. Where it is a
/// workspace's tracker, its readers (the broker's runs, the composer following
/// one) are told to read it again, as the owner's publication tells them in
/// a workspace it commits: a sealed workspace's tracker changes only here.
pub fn tracker_written(route: &crate::route::CrdtRoute) {
    if route.app_id == TRACKER_APP && route.record_id == TRACKER_RECORD {
        tilo_identity_host::policy_change::announce();
    }
}

#[must_use]
pub fn is_tracker_issue(task_id: &str) -> bool {
    task_id.starts_with(TRACKER_ISSUE_PREFIX)
}

/// The prefix of a run that answers a person's message in a conversation
/// (`crates/session/session/src/conversations.rs`): Chat's record, the
/// conversation's id, and the message the agent answers. A run's task id is
/// that address, so an agent's own invocations on the turn are its actions,
/// not a task of the Tasks tracker.
pub const CONVERSATION_RUN_PREFIX: &str = "chat.workspace/channels/";
/// What separates a conversation's address from the message it answers in a
/// run's task id.
const CONVERSATION_MESSAGE: &str = "/messages/";

/// Whether `task_id` names a conversation turn's message: a conversation of
/// Chat's record and one message in it, each a plain id (`chat.workspace/\
/// channels/<conversation>/messages/<message>`). A run whose task id is that
/// is the answer to a person's message, and the turn is its work.
#[must_use]
pub fn is_conversation_turn(task_id: &str) -> bool {
    let Some(rest) = task_id.strip_prefix(CONVERSATION_RUN_PREFIX) else {
        return false;
    };
    let Some((conversation, message)) = rest.rsplit_once(CONVERSATION_MESSAGE) else {
        return false;
    };
    let id = |part: &str| !part.is_empty() && !part.contains('/');
    id(conversation) && id(message)
}

/// The prefix of a run an agent door's admitted connection works in
/// (`crates/session/session/src/door_runs.rs`): the desktop starts one for each
/// connection its person admits, named by an id of its own beneath this.
pub const DOOR_RUN_PREFIX: &str = "so.tilo.session/agent-door/";

/// Whether `task_id` names an agent door connection's run: [`DOOR_RUN_PREFIX`]
/// and one plain id. A run whose task id is that is the connection's work, so
/// the agent's own invocations in it are the run's.
#[must_use]
pub fn is_door_run(task_id: &str) -> bool {
    task_id.strip_prefix(DOOR_RUN_PREFIX).is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
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
    let draft = match serde_json::from_value(question.draft.clone()) {
        Ok(draft) => draft,
        Err(error) => {
            // The sheet then shows no fields to review, only the action.
            tracing::warn!(
                question = %question.id,
                app = %question.app,
                action = %question.action,
                %error,
                "the run's recorded draft does not decode"
            );
            tilo_actions::Draft::default()
        }
    };
    // A preview this build cannot read is no preview: the person is
    // asked on the draft alone rather than on a difference shown wrong.
    let preview = question.before.clone().and_then(|preview| {
        match serde_json::from_value(preview) {
            Ok(preview) => Some(preview),
            Err(error) => {
                tracing::warn!(
                    question = %question.id,
                    %error,
                    "the run's recorded change preview does not decode; asking on the draft alone"
                );
                None
            }
        }
    });
    (draft, preview)
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
    /// Whether the run ended by its own agent's report (done or failed),
    /// rather than by its person's Stop or a newer run: its agent may still
    /// hear how its own last steps ended, and acts no more.
    pub finished_by_agent: bool,
    /// Where a person paused it: the messages to it its agent had taken in
    /// then. Its agent takes no new step until a person lets it go on.
    pub paused_through: Option<u64>,
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
    /// The turn the run's agent acts inside: the person's message in a
    /// conversation it takes part in. Every op the run makes carries it; a
    /// run whose causing message the receiver does not hold yet waits for it.
    pub turn: Option<tilo_action_policy::turn::Turn>,
    /// The task delegation the asking device signed for a task agent's run,
    /// encoded (`TaskDelegation::encode`), or why what the run carries is
    /// none; `None` for a collaborator's run, whose membership is its
    /// certification. Read through [`Self::delegation`].
    pub delegation: Option<Result<Vec<u8>, String>>,
}

impl TrackerRun {
    /// The run's task delegation, encoded; `None` for a collaborator's run.
    pub fn delegation(&self) -> Result<Option<&[u8]>, String> {
        match &self.delegation {
            None => Ok(None),
            Some(Ok(encoded)) => Ok(Some(encoded)),
            Some(Err(error)) => Err(format!(
                "the run's task delegation does not decode: {error}"
            )),
        }
    }

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

/// The agents a workspace keeps, as its own record of them names them
/// (`chat.agent`): an agent works here because the record names a keeper for
/// it, and that keeper is the person whose session its device joins on.
pub trait AgentRoster: Send + Sync {
    /// The person who keeps `agent` in `workspace`, while the workspace's
    /// record names it active there; `None` for an agent it does not name.
    fn keeper(&self, workspace: &str, agent: &str) -> Option<String>;
    /// What `agent` may spend on the day its keeper counts: the limit its
    /// keeper set and what it spent of it. `None` where this host reads no
    /// such record, and then the agent works unmetered.
    fn spend(&self, workspace: &str, agent: &str) -> Option<runtime_collab::TaskRunSpend> {
        let _ = (workspace, agent);
        None
    }
}

/// The Tasks app's tracker, read for the runs it holds. Runs are written
/// only through the tracker's own actions, invoked by the broker as the
/// run's agent.
pub trait TrackerRuns: Send + Sync {
    fn requires_workspace(&self, workspace: &str) -> bool;
    /// Whether an agent in `workspace` acts only through its run's admission,
    /// never on a workspace grant alone. Every workspace whose tracker is kept here
    /// unless the host says otherwise: the workspace's tracker serving runs
    /// ([`Self::requires_workspace`]) and agents being confined to them are two
    /// decisions.
    fn confines_agents_to_tasks(&self, workspace: &str) -> bool {
        self.requires_workspace(workspace)
    }
    /// Every run the workspace's tracker holds, going or ended.
    fn runs(&self, workspace: &str) -> Result<Vec<TrackerRun>, String>;
    /// Whether `run`'s agent is still admitted to `workspace`, as the workspace's
    /// owner holds it now: a collaborator by its membership, a task agent by
    /// its run's task delegation. A removed agent, or a task agent whose
    /// device was revoked, reads its run no more.
    fn check_agent(&self, workspace: &str, run: &TrackerRun) -> Result<(), String>;
    /// Apply the workspace's grants for the run's agent, acting for the run's
    /// starter, to `requirement`, as [`NativeTaskActionRequirement::check_tracker`]
    /// does for a workspace task. Where any of them asks first, the error starts
    /// with [`tilo_actions::draft::AWAITING_CONFIRMATION`].
    fn authorize_action(
        &self,
        workspace: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String>;
    /// What run `epoch` of issue `issue` in `workspace` shows of its current
    /// model turn as it streams, a bounded snapshot replacing the last one;
    /// `None` once the run ended. Private to whoever follows the run on this
    /// device, never written to the tracker: the run's findings reach the
    /// issue through the tracker's own actions.
    fn presented(
        &self,
        _workspace: &str,
        _issue: &str,
        _epoch: u64,
        _presentation: Option<&runtime_collab::TaskPresentation>,
    ) {
    }

    /// What run `epoch` of issue `issue` in `workspace` reported of itself: its
    /// agent took it up, took in the messages sent to it, is working, said
    /// something, ended. A tracker that keeps the run itself records it here;
    /// one that reads it from a record of its own has nothing to record.
    fn reported(
        &self,
        _workspace: &str,
        _issue: &str,
        _epoch: u64,
        _update: &runtime_collab::TaskRunUpdate,
    ) {
    }
}

//! Host-owned adaptation of authenticated task channels to the canonical owner.
//! No caller certificate, actor label or new grant is accepted from task arguments.
use crate::collab_broker_capnp::InvocationJob;
use tilo_peer_state::workspace_task::{WorkspaceTaskCommand, WorkspaceTaskSnapshot};

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
    /// agents. It may do what the starter may, held to the answers the room
    /// keeps for the agent, never to the preset it was admitted with, which
    /// bounds only what it does on its own. Effects stay attributed to the
    /// agent.
    ///
    /// # Errors
    /// The binding does not name its starter.
    pub fn actor(&self) -> Result<crate::collab_broker_capnp::BrokerActor, String> {
        let starter = self.starter.clone().ok_or("native task starter absent")?;
        Ok(
            crate::collab_broker_capnp::BrokerActor::agent(format!("agent:{}", self.agent_id))
                .for_person(starter)
                .session_delegated(),
        )
    }
}

/// A current published contract requirement, never a grant from task arguments.
#[derive(Clone, Debug)]
pub struct NativeTaskActionRequirement {
    /// Supplied by the broker's authenticated run channel, never action arguments.
    pub binding: Option<NativeTaskBinding>,
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
    /// Apply the room's grants for the agent to this action, on the live
    /// admitted document, held to the exact task/run's frozen authority.
    /// Where any of them asks first, the run pauses for its person: unless
    /// the invocation is [`Self::confirmed`], the answer is an error starting
    /// with [`tilo_actions::draft::AWAITING_CONFIRMATION`].
    ///
    /// # Errors
    /// The room is unavailable, the current agent is not allowed this
    /// action, or it waits for its person's confirmation.
    pub fn check(
        &self,
        peer: &tilo_peer_state::StatePeer,
        room: &str,
        agent: &str,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        use tilo_action_policy::{Answer, Authority, Context, roles};
        let doc = peer
            .admitted_room()
            .ok_or("native action policy unavailable")?;
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
        let binding = self
            .binding
            .as_ref()
            .ok_or("native task action binding absent")?;
        if binding.agent_id != agent || self.need.privileged {
            return Err("native task action binding invalid or action privileged".into());
        }
        let state = tilo_peer_state::workspace_task::snapshot(doc)?;
        let task = state
            .tasks
            .iter()
            .find(|task| task.id == binding.task_id)
            .ok_or("native task absent")?;
        let control = state
            .controls
            .get(&binding.task_id)
            .ok_or("native task control absent")?;
        let run = control.active_run.as_ref().ok_or("native task run ended")?;
        if run.epoch != binding.epoch
            || run.agent_id != agent
            || task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
            || task.assigned_agent.as_deref() != Some(agent)
        {
            return Err("native task run replaced or stopped".into());
        }
        let approval = run.approval.as_ref().ok_or("native task starter absent")?;
        // The run acts for its starter, so the agent is bounded as the
        // starter's delegate, as every member admits its writes: the agent
        // delegation and the answers the room holds for the agent. The
        // preset it was admitted with bounds what it does on its own, not
        // what a person asked of it.
        let delegation = roles::agent_delegation(room, agent, &approval.person);
        let verdict =
            Authority::person(delegation.party(&policy.grants)).decide(&node, self.need.kind, &cx);
        let member = peer.permissions(approval.device, now)?;
        let foreign = tilo_action_policy::roles::foreign_device_bound();
        let starter = member
            .authority(&foreign)
            .decide(&node, self.need.kind, &cx)
            .answer;
        if member.person != approval.person || starter == Answer::Deny {
            return Err("native task action is no longer authorized by its starter".into());
        }
        let bound = control
            .execution_authority
            .as_ref()
            .ok_or("native task authority absent")?;
        let frozen = if bound.scope.covers(&node) {
            bound.starter.answer(&node, self.need.kind, &cx)
        } else {
            Answer::Deny
        };
        if bound.epoch != binding.epoch || frozen == Answer::Deny {
            return Err("native task action exceeds its frozen execution authority".into());
        }
        self.answer(
            &policy,
            (&node, &cx),
            &binding.task_id,
            (verdict, [starter, frozen]),
        )
    }

    /// Apply the room's grants to `requirement` for a run of the Tasks app's
    /// tracker: as [`Self::check`], the agent acting for the run's starter
    /// on the starter's live grants, held to the issue's own grant set.
    /// The tracker freezes no scope: the starter's live grants and the
    /// agent's bound it.
    ///
    /// # Errors
    /// The room is unavailable, the run ended or was replaced, the agent is
    /// not allowed this action, or it waits for its person's confirmation.
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
        let binding = self.binding.as_ref().ok_or("tracker run binding absent")?;
        if binding.agent_id != run.agent || binding.task_id != run.issue || self.need.privileged {
            return Err("tracker run binding invalid or action privileged".into());
        }
        if !run.active || binding.epoch != run.epoch {
            return Err("this run has ended or was replaced".into());
        }
        let delegation = roles::agent_delegation(room, &run.agent, &run.starter);
        let verdict =
            Authority::person(delegation.party(&policy.grants)).decide(&node, self.need.kind, &cx);
        let foreign = roles::foreign_device_bound();
        let starter = policy
            .person_on_device(&run.starter, false, &foreign)
            .decide(&node, self.need.kind, &cx)
            .answer;
        if !policy.is_member(&run.starter) || starter == Answer::Deny {
            return Err("this run's action is no longer authorized by its starter".into());
        }
        self.answer(
            &policy,
            (&node, &cx),
            &run.issue,
            (verdict, [starter, Answer::Allow]),
        )
    }

    /// The answer of every party to the action: the agent's grants (as
    /// `verdict` decided them), the starter's and the run's frozen bound
    /// (`bounds`), and the task's own grant set. Deny refuses; Ask pauses
    /// for the person unless this invocation is [`Self::confirmed`].
    fn answer(
        &self,
        policy: &tilo_action_policy::RoomPolicy,
        (node, cx): (
            &tilo_action_policy::TreePath,
            &tilo_action_policy::Context<'_>,
        ),
        task_id: &str,
        (verdict, bounds): (
            tilo_action_policy::Verdict<'_>,
            [tilo_action_policy::Answer; 2],
        ),
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
        let task = policy
            .task_answer(task_id, node, self.need.kind, cx)
            .unwrap_or(Answer::Allow);
        if agent_answer == Answer::Deny {
            return Err(not_allowed(self.need.kind, &agent_basis(&verdict)));
        }
        if task == Answer::Deny {
            return Err(not_allowed(self.need.kind, "this task's own grants"));
        }
        let answer = [agent_answer, bounds[0], bounds[1], task]
            .into_iter()
            .fold(Answer::Allow, Answer::min);
        if answer == Answer::Ask && !self.confirmed {
            let whose = if agent_answer == Answer::Ask {
                agent_basis(&verdict)
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

    /// Whether `error`, from [`Self::check`], says the action waits for the
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

pub trait WorkspaceTaskExecution: Send + Sync {
    fn requires_room(&self, room: &str) -> bool;
    /// Read the canonical task controls in this native workspace.
    ///
    /// # Errors
    /// The owner is unavailable or this service does not govern the room.
    fn snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String>;
    /// Read instructions only while the assigned agent remains a current member.
    ///
    /// # Errors
    /// The owner is unavailable or current agent admission ended.
    fn agent_snapshot(&self, room: &str, agent: &str) -> Result<WorkspaceTaskSnapshot, String>;
    /// The broker supplies a binding derived from its authenticated channel.
    /// Authority uses the host's existing enrolled attestation, never a wire grant.
    fn report(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        command: WorkspaceTaskCommand,
    ) -> tilo_owner_commit::CommitFuture;
    /// Check the current owner capability policy before observing retained action output.
    ///
    /// # Errors
    /// The room or actor is unavailable, or its action capability was revoked.
    fn authorize_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String>;
    /// Admit a host activation in the same serialized task turn as an action.
    /// The callback must retain custody before invoking the ordinary launcher.
    ///
    /// # Errors
    /// Membership, starter authority, frozen scope or instruction frontier changed.
    fn admit_host_launch(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String>;

    /// Runs ordinary action admission inside the existing serialized task turn.
    ///
    /// # Errors
    /// Current membership, approval or instruction frontier changed, or admission fails.
    fn admit(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<InvocationJob, String>,
    ) -> Result<InvocationJob, String>;
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

/// Whether `task_id` names an issue of the tracker.
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
    pub answer: TrackerAnswer,
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
    /// Apply the room's grants for the run's agent, acting for the run's
    /// starter, to `requirement`, as [`NativeTaskActionRequirement::check`]
    /// does for a room task. Where any of them asks first, the error starts
    /// with [`tilo_actions::draft::AWAITING_CONFIRMATION`].
    ///
    /// # Errors
    /// The action is not allowed, or waits for the starter's confirmation.
    fn authorize_action(
        &self,
        room: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String>;
}

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
        use tilo_action_policy::{Answer, Authority, Context, Subject, agent_answers};
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
        let verdict = Authority::person(policy.party_of(Subject::agent(agent))).decide(
            &node,
            self.need.kind,
            &cx,
        );
        let agent_answer = verdict.parts.first().map_or(Answer::Deny, |decision| {
            agent_answers::held_to_contract(decision, self.consequential)
        });
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
        let task = policy
            .task_answer(&binding.task_id, &node, self.need.kind, &cx)
            .unwrap_or(Answer::Allow);
        if agent_answer == Answer::Deny {
            return Err(not_allowed(self.need.kind, &agent_basis(&verdict)));
        }
        if task == Answer::Deny {
            return Err(not_allowed(self.need.kind, "this task's own grants"));
        }
        let answer = [agent_answer, starter, frozen, task]
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

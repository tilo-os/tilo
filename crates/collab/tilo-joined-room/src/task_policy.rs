//! Canonical native-task policy. Wire metadata describes an invocation; it never grants it.
use std::time::SystemTime;
use tilo_action_policy::{Answer, Authority, Context, agent_answers, roles};

#[path = "task_policy/confirmation.rs"]
mod confirmation;

/// Private admission provenance, never reconstructed from a starter label.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct RetainedNativeTaskSource {
    approval: tilo_desktop_model::WorkspaceTaskApproval,
    authority: tilo_desktop_model::WorkspaceTaskExecutionAuthority,
}

impl RetainedNativeTaskSource {
    pub(crate) fn from_control(control: &tilo_desktop_model::WorkspaceTaskControl) -> Option<Self> {
        Some(Self {
            approval: control.active_run.as_ref()?.approval.clone()?,
            authority: control.execution_authority.clone()?,
        })
    }

    fn answer(
        &self,
        peer: &tilo_peer_state::StatePeer,
        binding: &NativeTaskBinding,
        node: &tilo_action_policy::TreePath,
        kind: tilo_action_policy::ActionKind,
        now: SystemTime,
    ) -> Result<Answer, String> {
        let member = peer.permissions(self.approval.device, now)?;
        if binding.starter.as_deref() != Some(self.approval.person.as_str())
            || member.person != self.approval.person
            || self.authority.epoch != binding.epoch
            || !self.authority.scope.covers(node)
        {
            return Err("native task starter or frozen execution scope changed".into());
        }
        let foreign = roles::foreign_device_bound();
        let cx = Context::at_time(now);
        let live = member.authority(&foreign).decide(node, kind, &cx).answer;
        let frozen = self.authority.starter.answer(node, kind, &cx);
        if live == Answer::Deny || frozen == Answer::Deny {
            return Err("native task action is no longer authorized by its starter".into());
        }
        Ok(live.min(frozen))
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NativeTaskBinding {
    pub task_id: String,
    pub agent_id: String,
    pub epoch: u64,
    pub revision: u64,
    pub starter: Option<String>,
}

/// Current published contract and authenticated run identity, not action input.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NativeTaskActionRequirement {
    pub binding: Option<NativeTaskBinding>,
    pub app: String,
    pub resource_kind: String,
    pub resource: String,
    pub action: String,
    pub need: tilo_actions::ActionNeed,
    /// Preflight hint only. Exact admission derives this from the frozen contract.
    pub consequential: bool,
    /// The unique owner-recorded asking UUID, never the reusable draft ID.
    pub confirmation: Option<String>,
    pub invocation: Option<NativeTaskInvocation>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NativeTaskInvocation {
    pub id: uuid::Uuid,
    pub request: uuid::Uuid,
    pub provider: String,
    /// Exact private wire arguments, retained only in owner/host custody.
    pub arguments: Option<String>,
    pub arguments_digest: [u8; 32],
    pub contract_digest: [u8; 32],
    pub contract: tilo_actions::ActionContract,
    pub private_run_digest: [u8; 32],
    /// A required host open is part of this exact reviewed child, not a second child.
    pub activation_for: Option<Box<NativeTaskActionRequirement>>,
}

impl std::fmt::Debug for NativeTaskInvocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeTaskInvocation { private: redacted }")
    }
}

impl NativeTaskInvocation {
    /// Freeze metadata at the authenticated broker door.
    /// # Errors
    /// Contract or arguments cannot be serialized.
    pub fn new(
        id: uuid::Uuid,
        provider: &str,
        arguments: Option<&str>,
        contract: &tilo_actions::ActionContract,
        private_run: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            id,
            request: id,
            provider: provider.into(),
            arguments: arguments.map(str::to_owned),
            arguments_digest: arguments_digest(arguments)?,
            contract_digest: digest(contract)?,
            contract: contract.clone(),
            private_run_digest: *blake3::hash(private_run.as_bytes()).as_bytes(),
            activation_for: None,
        })
    }
}

fn digest(value: &impl serde::Serialize) -> Result<[u8; 32], String> {
    Ok(*blake3::hash(&serde_json::to_vec(value).map_err(|e| e.to_string())?).as_bytes())
}

fn arguments_digest(arguments: Option<&str>) -> Result<[u8; 32], String> {
    let value = arguments.map_or(serde_json::Value::Null, |text| {
        let json = text
            .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
            .unwrap_or(text);
        serde_json::from_str(json).unwrap_or_else(|_| serde_json::Value::String(text.into()))
    });
    digest(&(arguments.is_some(), value))
}

impl NativeTaskActionRequirement {
    /// Entire signed ceiling, including any reviewed child used for activation.
    /// # Errors
    /// Metadata cannot be serialized.
    pub fn ceiling_digest(&self) -> Result<[u8; 32], String> {
        digest(&(
            &self.binding,
            &self.app,
            &self.resource_kind,
            &self.resource,
            &self.action,
            &self.need,
            &self.confirmation,
            self.invocation
                .as_ref()
                .and_then(|i| i.activation_for.as_ref()),
        ))
    }

    /// Request-time ceiling to freeze in OWNER custody under the asking UUID.
    /// Excludes final arguments and invocation IDs so approved edits can run.
    /// # Errors
    /// No exact invocation is present or metadata cannot be serialized.
    pub fn confirmation_ceiling_digest(&self) -> Result<[u8; 32], String> {
        let invocation = self
            .invocation
            .as_ref()
            .ok_or("native exact invocation absent")?;
        digest(&(
            &self.binding,
            &self.app,
            &self.resource_kind,
            &self.resource,
            &self.action,
            &self.need,
            &invocation.provider,
            invocation.contract_digest,
            invocation.private_run_digest,
        ))
    }

    fn consequential(&self) -> bool {
        self.invocation
            .as_ref()
            .map_or(self.consequential, |invocation| {
                let argument = invocation.arguments.as_deref().map(|text| {
                    let json = text
                        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                        .unwrap_or(text);
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                        .map_or_else(
                            |_| text.to_owned(),
                            |value| {
                                format!(
                                    "{}{}",
                                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                                    serde_json::Value::Object(value)
                                )
                            },
                        )
                });
                invocation
                    .contract
                    .confirmation_for_argument(argument.as_deref())
                    != tilo_actions::ConfirmationPolicy::None
            })
    }

    fn answer(
        &self,
        peer: &tilo_peer_state::StatePeer,
        room: &str,
        agent: &str,
        now: SystemTime,
    ) -> Result<Answer, String> {
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
        let expected_epoch = binding.epoch;
        if run.epoch != expected_epoch
            || control.run_epoch != expected_epoch
            || run.agent_id != agent
            || task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
            || task.assigned_agent.as_deref() != Some(agent)
        {
            return Err("native task run replaced or stopped".into());
        }
        let approval = run.approval.as_ref().ok_or("native task starter absent")?;
        if binding
            .starter
            .as_deref()
            .is_some_and(|starter| starter != approval.person)
        {
            return Err("native task starter changed".into());
        }
        let delegation = roles::agent_delegation(room, agent, &approval.person);
        let verdict =
            Authority::person(delegation.party(&policy.grants)).decide(&node, self.need.kind, &cx);
        let agent_answer = verdict.parts.first().map_or(Answer::Deny, |decision| {
            agent_answers::held_to_contract(decision, self.consequential())
        });
        let starter = RetainedNativeTaskSource::from_control(control)
            .ok_or("native task authority absent")?
            .answer(peer, binding, &node, self.need.kind, now)?;
        let task_answer = policy
            .task_answer(&binding.task_id, &node, self.need.kind, &cx)
            .unwrap_or(Answer::Allow);
        if agent_answer == Answer::Deny || task_answer == Answer::Deny {
            return Err(format!(
                "native task action is not allowed: grants deny {}",
                self.need.kind.phrase()
            ));
        }
        Ok([agent_answer, starter, task_answer]
            .into_iter()
            .fold(Answer::Allow, Answer::min))
    }

    fn waiting(&self) -> String {
        format!(
            "{}: {} asks a person first",
            tilo_actions::draft::AWAITING_CONFIRMATION,
            self.action
        )
    }

    fn waiting_with_basis(
        &self,
        peer: &tilo_peer_state::StatePeer,
        room: &str,
        agent: &str,
        now: SystemTime,
    ) -> Result<String, String> {
        let policy = tilo_desktop_model::projection::room_policy_of(
            peer.admitted_room().ok_or("native room absent")?,
            room,
        );
        let node = tilo_actions::action_node(
            &policy.node(),
            &self.app,
            &self.resource_kind,
            &self.resource,
            self.need.field.as_deref(),
            &self.action,
        );
        let binding = self.binding.as_ref().ok_or("native task binding absent")?;
        let snapshot = tilo_peer_state::workspace_task::snapshot(
            peer.admitted_room().ok_or("native room absent")?,
        )?;
        let person = snapshot
            .controls
            .get(&binding.task_id)
            .and_then(|c| c.active_run.as_ref())
            .and_then(|r| r.approval.as_ref())
            .ok_or("native starter absent")?;
        let delegation = roles::agent_delegation(room, agent, &person.person);
        let verdict = Authority::person(delegation.party(&policy.grants)).decide(
            &node,
            self.need.kind,
            &Context::at_time(now),
        );
        let basis = match verdict.parts.first().map(|decision| decision.basis) {
            Some(tilo_action_policy::Basis::Grant(grant)) => {
                format!("grant {} on {}", grant.id, grant.node)
            }
            _ => format!("grants on {node}"),
        };
        Ok(format!("{} ({basis})", self.waiting()))
    }

    /// Preflight never treats an unverified reference as person authority.
    /// # Errors
    /// Current policy denies, or a person must answer first.
    pub fn check(
        &self,
        peer: &tilo_peer_state::StatePeer,
        room: &str,
        agent: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let snapshot = tilo_peer_state::workspace_task::snapshot(
            peer.admitted_room().ok_or("native room absent")?,
        )?;
        self.check_with_snapshot(peer, &snapshot, room, agent, now)
    }

    /// Check policy against an owner-private, authenticated confirmation snapshot.
    /// # Errors
    /// A live ceiling denies or a person has not approved the exact draft.
    pub fn check_with_snapshot(
        &self,
        peer: &tilo_peer_state::StatePeer,
        snapshot: &tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
        room: &str,
        agent: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let answer = self.answer(peer, room, agent, now)?;
        if let Some(child) = self
            .invocation
            .as_ref()
            .and_then(|i| i.activation_for.as_ref())
        {
            child.check_with_snapshot(peer, snapshot, room, agent, now)?;
            if answer == Answer::Ask && child.person_confirmation(snapshot)?.is_none() {
                return Err(self.waiting_with_basis(peer, room, agent, now)?);
            }
        } else if answer == Answer::Ask && self.person_confirmation(snapshot)?.is_none() {
            return Err(self.waiting_with_basis(peer, room, agent, now)?);
        }
        Ok(())
    }

    /// # Errors
    /// Any frozen metadata or live host/task ceiling differs.
    fn check_metadata(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        let run = self
            .binding
            .as_ref()
            .ok_or("native invocation run absent")?;
        let invocation = self
            .invocation
            .as_ref()
            .ok_or("native exact invocation absent")?;
        if self.ceiling_digest()? != exact.ceiling_digest
            || invocation.contract.need() != self.need
            || digest(&invocation.contract)? != invocation.contract_digest
            || arguments_digest(invocation.arguments.as_deref())? != invocation.arguments_digest
            || invocation.id != exact.invocation
            || invocation.request != exact.request
            || invocation.provider != exact.provider
            || invocation.arguments_digest != exact.arguments_digest
            || invocation.contract_digest != exact.contract_digest
            || invocation.private_run_digest != exact.private_run_digest
            || run.task_id != exact.task
            || run.agent_id != exact.agent
            || run.epoch != exact.epoch
            || run.revision != exact.revision
            || self.app != exact.app
            || self.resource != exact.resource
            || self.action != exact.action
        {
            return Err("native exact invocation ceiling or metadata changed".into());
        }
        let node = tilo_actions::action_node(
            &tilo_action_policy::TreePath::room(exact.room.to_string()),
            &self.app,
            &self.resource_kind,
            &self.resource,
            self.need.field.as_deref(),
            &self.action,
        );
        if !peer
            .permissions(exact.host, now)?
            .allows(&node, self.need.kind, now)
        {
            return Err("native physical host role or device scope denies this action".into());
        }
        Ok(())
    }

    fn check_exact(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<Answer, String> {
        self.check_metadata(peer, exact, now)?;
        self.answer(peer, &exact.room.to_string(), &exact.agent, now)
    }

    /// Validate exact admission; owner custody must also freeze the request-time
    /// confirmation ceiling and atomically consume its reference across child IDs.
    /// # Errors
    /// Metadata, live policy or the owner's person answer does not authorize it.
    pub fn check_invocation(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        self.confirmation_binding(peer, exact, now).map(drop)
    }

    /// Exact policy check using the private confirmation body from owner custody.
    /// # Errors
    /// Frozen metadata, person answer or current authority differs.
    pub fn check_invocation_with_snapshot(
        &self,
        peer: &tilo_peer_state::StatePeer,
        snapshot: &tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        self.confirmation_binding_with_snapshot(peer, snapshot, exact, now)
            .map(drop)
    }

    /// Match the actual registered inbox effect to its admitted exact arguments.
    /// # Errors
    /// Target, invocation or private payload differs from durable admission.
    pub fn check_task_message_payload(
        &self,
        recipient: &str,
        invocation: uuid::Uuid,
        send: &tilo_actions::task_messaging::TaskMessageSend,
    ) -> Result<(), String> {
        let exact = self
            .invocation
            .as_ref()
            .ok_or("native message invocation absent")?;
        if self.app != "so.tilo.desktop"
            || self.resource_kind != "workspace-task"
            || self.resource != format!("desktop:{recipient}")
            || self.action != "send-message"
            || self.need.field.as_deref() != Some("inbox")
            || exact.id != invocation
        {
            return Err("native message effect target differs from admission".into());
        }
        let text = exact
            .arguments
            .as_deref()
            .ok_or("native message admitted arguments absent")?;
        let json = text
            .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
            .unwrap_or(text);
        let actual: serde_json::Value =
            serde_json::from_str(json).map_err(|_| "native message admitted arguments invalid")?;
        if actual != serde_json::to_value(send).map_err(|e| e.to_string())? {
            return Err("native message payload differs from admitted exact arguments".into());
        }
        Ok(())
    }

    /// Host-only live ceiling check after exact signed governor approval has been
    /// verified by the dispatch turn. This method alone is not admission authority.
    /// # Errors
    /// Any exact metadata or live ceiling denies the effect.
    pub fn check_dispatch_ceiling(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        self.check_exact(peer, exact, now)?;
        if let Some(child) = self
            .invocation
            .as_ref()
            .and_then(|i| i.activation_for.as_ref())
        {
            child.answer(peer, &exact.room.to_string(), &exact.agent, now)?;
        }
        Ok(())
    }

    /// Metadata alone cannot authenticate a retained read. Use the private
    /// ledger's `check_retained_action` or the committed owner's retained door.
    /// # Errors
    /// Always refuses: this entry point has no private dispatch provenance.
    pub fn check_retained(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<(), String> {
        self.check_metadata(peer, exact, now)?;
        Err("retained native observation requires owner ledger provenance".into())
    }

    pub(crate) fn check_retained_source(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        source: &RetainedNativeTaskSource,
        now: SystemTime,
    ) -> Result<(), String> {
        self.check_metadata(peer, exact, now)?;
        peer.workspace()
            .check_task_execution_actor(&exact.agent, None, now)?;
        self.retained_answer(peer, exact, source, now)?;
        if let Some(child) = self
            .invocation
            .as_ref()
            .and_then(|i| i.activation_for.as_ref())
        {
            // Observation cannot bypass a newly denied reviewed child either.
            child.retained_answer(peer, exact, source, now)?;
        }
        Ok(())
    }

    fn retained_answer(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        source: &RetainedNativeTaskSource,
        now: SystemTime,
    ) -> Result<(), String> {
        let binding = self
            .binding
            .as_ref()
            .ok_or("retained native binding absent")?;
        let starter = &source.approval.person;
        if binding.task_id != exact.task
            || binding.agent_id != exact.agent
            || binding.epoch != exact.epoch
        {
            return Err("retained native source binding differs".into());
        }
        let room = exact.room.to_string();
        let policy = tilo_desktop_model::projection::room_policy_of(
            peer.admitted_room().ok_or("native room absent")?,
            &room,
        );
        let node = tilo_actions::action_node(
            &policy.node(),
            &self.app,
            &self.resource_kind,
            &self.resource,
            self.need.field.as_deref(),
            &self.action,
        );
        let cx = Context::at_time(now);
        source.answer(peer, binding, &node, self.need.kind, now)?;
        let delegation = roles::agent_delegation(&room, &exact.agent, starter);
        if Authority::person(delegation.party(&policy.grants))
            .decide(&node, self.need.kind, &cx)
            .answer
            == Answer::Deny
            || policy.task_answer(&exact.task, &node, self.need.kind, &cx) == Some(Answer::Deny)
        {
            return Err("current policy denies retained native observation".into());
        }
        Ok(())
    }

    #[must_use]
    pub fn awaits_confirmation(error: &str) -> bool {
        error.contains(tilo_actions::draft::AWAITING_CONFIRMATION)
    }
}

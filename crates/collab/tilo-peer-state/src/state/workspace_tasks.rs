use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{StatePeer, SystemTime};
use crate::workspace_task::{
    AdmittedTaskActor, AppliedTaskCommand, WorkspaceTaskAuthority, WorkspaceTaskCommand,
};
use tilo_action_policy::{ActionKind, TreePath};

impl StatePeer {
    fn mint_task_authority(
        &self,
        doc: &mut automerge::AutoCommit,
        actor: &AdmittedTaskActor,
        task_id: &str,
        epoch: u64,
        now: SystemTime,
    ) -> Result<(), String> {
        let task = tilo_desktop_model::projection::workspace_tasks_of(doc)
            .into_iter()
            .find(|task| task.id == task_id)
            .ok_or("task disappeared")?;
        let mut controls = crate::workspace_task::controls_of(doc)?;
        let device = match actor {
            AdmittedTaskActor::Person { device, .. } => *device,
            AdmittedTaskActor::Host { .. } => controls
                .get(task_id)
                .and_then(|control| control.receipts.first())
                .map(|receipt| receipt.device)
                .ok_or("task creator device evidence absent")?,
            AdmittedTaskActor::Agent { .. } => {
                return Err("an agent cannot mint task authority".into());
            }
        };
        let member = self.permissions(device, now)?;
        if matches!(actor, AdmittedTaskActor::Host { .. }) && member.person != task.created_by {
            return Err("task creator evidence was replaced".into());
        }
        let mut scope = TreePath::room(&self.core.room);
        if let Some(resource) = &task.resource {
            let app = resource
                .app_id
                .as_ref()
                .ok_or("attached task scope has no app identity")?;
            scope = scope.app(app).kind(&resource.kind).resource(&resource.id);
        }
        let control = controls
            .get_mut(task_id)
            .ok_or("task control disappeared")?;
        let run = control.active_run.as_mut().ok_or("task run disappeared")?;
        // Host starts inherit the creator's authority, not the host's authority.
        if run.approval.is_none() {
            run.approval = Some(tilo_desktop_model::WorkspaceTaskApproval {
                person: member.person.clone(),
                device,
                approved_at: run.started_at,
                instruction_revision: control.directions.last().map_or(0, |value| value.revision),
            });
        }
        control.execution_authority = Some(tilo_desktop_model::WorkspaceTaskExecutionAuthority {
            epoch,
            scope,
            starter: member.standing(),
        });
        crate::workspace_task::project_controls(doc, &controls)?;
        doc.commit();
        Ok(())
    }

    fn check_task_steering(
        &self,
        actor: &AdmittedTaskActor,
        command: &WorkspaceTaskCommand,
        doc: &automerge::AutoCommit,
        now: SystemTime,
    ) -> Result<(), String> {
        let (WorkspaceTaskCommand::Direction { task_id, .. }
        | WorkspaceTaskCommand::DirectionForRun { task_id, .. }
        | WorkspaceTaskCommand::CancelForRun { task_id, .. }) = command
        else {
            return Ok(());
        };
        let AdmittedTaskActor::Person { device, .. } = actor else {
            return Err("only a person may steer a task run".into());
        };
        let member = self.permissions(*device, now)?;
        let controls = crate::workspace_task::controls_of(doc)?;
        let control = controls.get(task_id).ok_or("task control absent")?;
        let Some(run) = control.active_run.as_ref() else {
            return Ok(());
        };
        let bound = control
            .execution_authority
            .as_ref()
            .filter(|bound| bound.epoch == run.epoch)
            .filter(|bound| bound.epoch == control.run_epoch)
            .ok_or("task execution authority absent or stale")?;
        if bound.scope.room_id() != Some(self.core.room.as_str())
            || bound
                .scope
                .segments()
                .iter()
                .any(|segment| segment.id().is_none())
        {
            return Err("task execution scope is not concrete in this room".into());
        }
        let caller = member.standing();
        // An unbounded current owner structurally allows the entire room.
        if caller.policy.owner.as_ref() == Some(&caller.person) && caller.bounds.is_empty() {
            return Ok(());
        }
        let patterns = [&bound.starter, &caller]
            .into_iter()
            .flat_map(|standing| {
                standing
                    .policy
                    .grants
                    .iter()
                    .chain(standing.bounds.iter().flat_map(|bound| &bound.grants))
            })
            .map(|grant| &grant.node)
            .filter(|node| node.overlaps(&bound.scope))
            .collect::<Vec<_>>();
        let cx = tilo_action_policy::Context::at_time(now);
        for node in task_policy_cells(&bound.scope, &patterns)? {
            for kind in ActionKind::ALL {
                if bound.starter.answer(&node, kind, &cx) == tilo_action_policy::Answer::Allow
                    && caller.answer(&node, kind, &cx) != tilo_action_policy::Answer::Allow
                {
                    return Err("task steering requires every execution permission".into());
                }
            }
        }
        Ok(())
    }

    fn check_agent_task_approval(
        &self,
        actor: &AdmittedTaskActor,
        command: &WorkspaceTaskCommand,
        room: &automerge::AutoCommit,
        now: SystemTime,
    ) -> Result<(), String> {
        if let AdmittedTaskActor::Agent { id, .. } = actor {
            let task_id = match command {
                WorkspaceTaskCommand::AppliedAck { task_id, .. }
                | WorkspaceTaskCommand::Finish { task_id, .. }
                | WorkspaceTaskCommand::ExecutionPhase { task_id, .. }
                | WorkspaceTaskCommand::PublicProgress { task_id, .. } => Some(task_id),
                _ => None,
            };
            let state = crate::workspace_task::snapshot(room)?;
            let approval = task_id
                .and_then(|id| state.controls.get(id))
                .and_then(|control| control.active_run.as_ref())
                .and_then(|run| run.approval.as_ref());
            self.workspace()
                .check_task_execution_actor(id, approval, now)?;
        }
        Ok(())
    }
}
impl PeerWorkspace<'_> {
    /// Revalidate the assigned agent and the exact person's retained run approval.
    /// This is a check on existing capabilities, not a run-created grant.
    /// # Errors
    /// An actor was removed/revoked, its certificate expired, or write approval ended.
    pub fn check_task_execution_actor(
        self,
        agent_id: &str,
        approval: Option<&tilo_desktop_model::WorkspaceTaskApproval>,
        now: SystemTime,
    ) -> Result<(), String> {
        let doc = self
            .peer
            .core
            .room_doc
            .as_deref()
            .ok_or("admitted task document unavailable")?;
        let policy = tilo_desktop_model::projection::room_policy_of(doc, &self.peer.core.room);
        if !super::agent_is_member(&policy, agent_id, now) {
            return Err("assigned task agent is no longer admitted".into());
        }
        if let Some(approval) = approval {
            let member = self.peer.permissions(approval.device, now)?;
            if member.person != approval.person
                || !member.allows(
                    &TreePath::room(&self.peer.core.room),
                    ActionKind::Undoable,
                    now,
                )
            {
                return Err("task run approval is no longer authorized".into());
            }
        }
        Ok(())
    }
}
impl PeerWorkspaceMut<'_> {
    /// Apply one typed task command against the current admitted room document.
    /// The caller or host certificate is rechecked here, in the same serialized
    /// owner turn as the mutation; strings in the command confer no authority.
    ///
    /// # Errors
    /// The authority is not current, the command is stale or malformed, or
    /// the admitted room document cannot be updated.
    pub fn apply_workspace_task_command(
        self,
        invocation: uuid::Uuid,
        request_digest: [u8; 32],
        authority: &WorkspaceTaskAuthority,
        command: &WorkspaceTaskCommand,
        now: SystemTime,
    ) -> Result<(bool, tilo_desktop_model::WorkspaceTaskMutationResult), String> {
        let certificate = authority.certificate();
        if self
            .peer
            .core
            .certificates
            .get(&certificate.device_public_key)
            .map(AsRef::as_ref)
            != Some(certificate)
        {
            return Err("workspace task certificate is not current admitted evidence".into());
        }
        self.peer.certificate(&certificate.device_public_key, now)?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "workspace task clock is before the Unix epoch")?
            .as_secs();
        let room = self
            .peer
            .core
            .room_doc
            .as_deref()
            .ok_or("workspace task room document is absent")?;
        let policy = tilo_desktop_model::projection::room_policy_of(room, &self.peer.core.room);
        let owner = policy.owner.clone();
        let agent_member = |agent: &str| super::agent_is_member(&policy, agent, now);
        if let Some(agent) = command
            .agent()
            .map(str::to_owned)
            .or(direction_agent(command, room)?)
            && !agent_member(&agent)
        {
            return Err("workspace task agent is not a current room member".into());
        }
        let actor = match authority {
            WorkspaceTaskAuthority::Person { caller } => {
                let member = self.peer.permissions(caller.device_public_key, now)?;
                if !member.allows(
                    &TreePath::room(&self.peer.core.room),
                    ActionKind::Undoable,
                    now,
                ) {
                    return Err("workspace task person lacks write authority".into());
                }
                AdmittedTaskActor::Person {
                    id: member.person,
                    device: caller.device_public_key,
                }
            }
            WorkspaceTaskAuthority::Host { attestor } => {
                if !attestor.capabilities.can_act_as_host
                    && attestor.device_public_key != self.peer.core.authority.device_public_key
                {
                    return Err("workspace task attestor is not a host".into());
                }
                AdmittedTaskActor::Host {
                    device: attestor.device_public_key,
                }
            }
            WorkspaceTaskAuthority::Agent { attestor, agent_id } => {
                if (!attestor.capabilities.can_act_as_host
                    && attestor.device_public_key != self.peer.core.authority.device_public_key)
                    || !agent_member(agent_id)
                {
                    return Err("workspace task agent lacks current host attestation".into());
                }
                AdmittedTaskActor::Agent {
                    id: agent_id.clone(),
                    device: attestor.device_public_key,
                }
            }
        };
        require_budget_owner(command, &actor, owner.as_ref())?;
        self.peer
            .check_agent_task_approval(&actor, command, room, now)?;
        self.peer.check_task_steering(&actor, command, room, now)?;
        let mut candidate = room.clone();
        let AppliedTaskCommand { changed, result } = crate::workspace_task::apply(
            &mut candidate,
            invocation,
            request_digest,
            &actor,
            command,
            i64::try_from(seconds).unwrap_or(i64::MAX),
        )?;
        if changed {
            if let tilo_desktop_model::WorkspaceTaskMutationResult::RunStarted { task_id, epoch } =
                &result
            {
                self.peer
                    .mint_task_authority(&mut candidate, &actor, task_id, *epoch, now)?;
            }
            self.peer.core.room_doc = Some(candidate.into());
        }
        Ok((changed, result))
    }
}

/// Concrete representatives of every policy cell intersecting the execution
/// scope. A cell is the product of grant-pattern automaton states: all paths
/// in it match exactly the same grants, so specificity and answers are constant.
/// Named segments plus one fresh name per level partition the infinite alphabet.
/// This also handles interior `**` and arbitrary descendant depth, without ever
/// asking the policy to decide on an unresolved wildcard as if it were a node.
fn task_policy_cells(scope: &TreePath, patterns: &[&TreePath]) -> Result<Vec<TreePath>, String> {
    use std::collections::{BTreeSet, VecDeque};
    use tilo_action_policy::{Level, Segment};

    const MAX_CELLS: usize = 4096;
    if scope
        .segments()
        .iter()
        .any(|segment| segment.id().is_none())
    {
        return Err("task execution scope is not concrete".into());
    }
    let mut alphabet = patterns
        .iter()
        .flat_map(|path| path.segments())
        .chain(scope.segments())
        .filter(|segment| matches!(segment, Segment::Name(..)))
        .cloned()
        .collect::<BTreeSet<_>>();
    for level in Level::ALL {
        let mut fresh = "task-policy-other".to_owned();
        while alphabet.contains(&Segment::Name(level, fresh.clone())) {
            fresh.push('_');
        }
        alphabet.insert(Segment::Name(level, fresh));
    }
    let mut initial = patterns
        .iter()
        .map(|pattern| task_pattern_closure(pattern, vec![0]))
        .collect::<Vec<_>>();
    // Consume the concrete scope first: this intersects every pattern with it,
    // including wildcard ancestors with concrete overrides inside the scope.
    for segment in scope.segments() {
        initial = patterns
            .iter()
            .zip(&initial)
            .map(|(pattern, state)| task_pattern_step(pattern, state, segment))
            .collect();
    }
    let mut seen = BTreeSet::from([initial.clone()]);
    let mut queue = VecDeque::from([(initial, scope.clone())]);
    let mut cells = Vec::new();
    while let Some((states, node)) = queue.pop_front() {
        for segment in &alphabet {
            let next = patterns
                .iter()
                .zip(&states)
                .map(|(pattern, state)| task_pattern_step(pattern, state, segment))
                .collect::<Vec<_>>();
            if seen.insert(next.clone()) {
                if seen.len() > MAX_CELLS {
                    return Err("task permission containment could not be established".into());
                }
                let Segment::Name(level, id) = segment else {
                    return Err("task policy alphabet is not concrete".into());
                };
                queue.push_back((next, node.clone().push(*level, id)));
            }
        }
        cells.push(node);
    }
    Ok(cells)
}

fn task_pattern_closure(pattern: &TreePath, mut state: Vec<usize>) -> Vec<usize> {
    use tilo_action_policy::Segment;
    let mut pending = std::collections::VecDeque::from(state.clone());
    while let Some(position) = pending.pop_front() {
        if pattern.segments().get(position) == Some(&Segment::Rest)
            && let Some(next) = position.checked_add(1)
            && !state.contains(&next)
        {
            state.push(next);
            pending.push_back(next);
        }
    }
    state.sort_unstable();
    state.dedup();
    state
}

fn task_pattern_step(
    pattern: &TreePath,
    state: &[usize],
    segment: &tilo_action_policy::Segment,
) -> Vec<usize> {
    use tilo_action_policy::Segment;
    let next = state
        .iter()
        .filter_map(|&position| match pattern.segments().get(position) {
            // A grant covers every descendant once its pattern has matched.
            None | Some(Segment::Rest) => Some(position),
            Some(name @ Segment::Name(..)) if name == segment => position.checked_add(1),
            Some(Segment::Any(level)) if segment.level() == Some(*level) => position.checked_add(1),
            _ => None,
        })
        .collect();
    task_pattern_closure(pattern, next)
}

fn require_budget_owner(
    command: &WorkspaceTaskCommand,
    actor: &AdmittedTaskActor,
    owner: Option<&String>,
) -> Result<(), String> {
    if matches!(command, WorkspaceTaskCommand::SetExecutionBudget { .. })
        && !matches!(actor, AdmittedTaskActor::Person { id, .. } if owner == Some(id))
    {
        return Err("only the current workspace owner may configure task execution budgets".into());
    }
    Ok(())
}

fn direction_agent(
    command: &WorkspaceTaskCommand,
    room: &automerge::AutoCommit,
) -> Result<Option<String>, String> {
    let (WorkspaceTaskCommand::Direction { task_id, .. }
    | WorkspaceTaskCommand::DirectionForRun { task_id, .. }
    | WorkspaceTaskCommand::CancelForRun { task_id, .. }) = command
    else {
        return Ok(None);
    };
    tilo_desktop_model::projection::workspace_tasks_of(room)
        .into_iter()
        .find(|task| &task.id == task_id)
        .ok_or_else(|| "workspace task no longer exists".into())
        .map(|task| task.assigned_agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SeedState, seed_room};
    use ed25519_dalek::SigningKey;
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    use tilo_desktop_model::{WorkspaceTaskRoute, WorkspaceTaskStatus};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
    use web_time::Duration;

    const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn certificate(root: &SigningKey, device: &SigningKey, label: &str) -> DeviceCert {
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, root).unwrap();
        certificate
    }

    fn peer() -> (StatePeer, DeviceCert, DeviceCert, SystemTime) {
        let governor_root = SigningKey::from_bytes(&[1; 32]);
        let governor_key = SigningKey::from_bytes(&[2; 32]);
        let governor = certificate(&governor_root, &governor_key, "Governor");
        let other_root = SigningKey::from_bytes(&[3; 32]);
        let other_key = SigningKey::from_bytes(&[4; 32]);
        let other = certificate(&other_root, &other_key, "Other primary");
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2))
            .unwrap();
        let mut peer = StatePeer::new(
            ROOM,
            governor.clone(),
            vec![governor.clone(), other.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(
            SeedState {
                room_changes: seed_room(ROOM, &governor, &[governor.clone(), other.clone()])
                    .unwrap(),
                documents: vec![],
            },
            &governor_key,
            now,
        )
        .unwrap();
        peer.governance_mut()
            .put_governed_grants(&[agent_grant("agent-scribe", ROOM)], &governor_key, now)
            .unwrap();
        (peer, governor, other, now)
    }

    fn agent_grant(id: &str, room: &str) -> Grant {
        Grant::new(
            id,
            Subject::agent("scribe"),
            TreePath::room(room),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Subject::person("governor"),
        )
    }

    fn assert_stop_refused_after_agent_removal(
        peer: &mut StatePeer,
        governor: &DeviceCert,
        task_id: &str,
        now: SystemTime,
    ) {
        assert!(
            peer.workspace_mut()
                .apply_workspace_task_command(
                    uuid::Uuid::from_u128(5),
                    [5; 32],
                    &WorkspaceTaskAuthority::Person {
                        caller: governor.clone(),
                    },
                    &WorkspaceTaskCommand::CancelForRun {
                        task_id: task_id.into(),
                        run_epoch: 1,
                        after_revision: 0,
                    },
                    now,
                )
                .is_err(),
            "a removed agent cannot retain an owner-side stop door"
        );
    }

    /// Make the other person an editor who may only read the room.
    fn read_only_member(peer: &mut StatePeer, other: &DeviceCert, now: SystemTime) {
        let key = SigningKey::from_bytes(&[2; 32]);
        let mut doc = peer.core.room_doc.clone().unwrap();
        let before = doc.get_heads();
        let members = automerge::ReadDoc::get(&*doc, automerge::ROOT, "members")
            .unwrap()
            .unwrap()
            .1;
        crate::state::put_member(
            &mut doc,
            &members,
            other.user_public_key,
            collab_types::Role::Editor,
        )
        .unwrap();
        doc.commit();
        peer.apply_governance_doc(doc, &before, &key, now).unwrap();
        peer.governance_mut()
            .put_governed_grants(
                &[Grant::new(
                    "read-only-member",
                    Subject::person(hex::encode(other.user_public_key)),
                    TreePath::room(ROOM),
                    KindSet::UNDOABLE,
                    Answer::Deny,
                    Subject::person("governor"),
                )],
                &key,
                now,
            )
            .unwrap();
    }

    struct SteeringFixture {
        peer: StatePeer,
        governor: DeviceCert,
        caller: WorkspaceTaskAuthority,
        now: SystemTime,
        scope: TreePath,
        wildcard: TreePath,
        subject: Subject,
        task_id: String,
    }

    impl SteeringFixture {
        fn new() -> Self {
            use crate::workspace_membership::WorkspaceMembershipCommand;
            use tilo_action_policy::Level;
            let (mut peer, governor, other, now) = peer();
            let key = SigningKey::from_bytes(&[2; 32]);
            read_only_member(&mut peer, &other, now);
            peer.workspace_mut()
                .apply_workspace_membership_command(
                    &governor,
                    &WorkspaceMembershipCommand::Revoke {
                        grant: tilo_action_policy::GrantId::new("read-only-member"),
                    },
                    &key,
                    now,
                )
                .unwrap();
            let documents = TreePath::room(ROOM).app("notes").kind("document");
            let scope = documents.clone().resource("A");
            let wildcard = documents.any(Level::Resource);
            let subject = Subject::person(hex::encode(other.user_public_key));
            peer.governance_mut()
                .put_governed_grants(
                    &[
                        Grant::new(
                            "starter-wildcard-deny",
                            subject.clone(),
                            wildcard.clone(),
                            KindSet::UNDOABLE,
                            Answer::Deny,
                            Subject::person("governor"),
                        ),
                        Grant::new(
                            "starter-concrete-allow",
                            subject.clone(),
                            scope.clone(),
                            KindSet::UNDOABLE,
                            Answer::Allow,
                            Subject::person("governor"),
                        ),
                    ],
                    &key,
                    now,
                )
                .unwrap();
            let caller = WorkspaceTaskAuthority::Person {
                caller: other.clone(),
            };

            let task_id = Self::start_attached_run(&mut peer, &governor, &caller, now);
            Self {
                peer,
                governor,
                caller,
                now,
                scope,
                wildcard,
                subject,
                task_id,
            }
        }

        fn start_attached_run(
            peer: &mut StatePeer,
            governor: &DeviceCert,
            caller: &WorkspaceTaskAuthority,
            now: SystemTime,
        ) -> String {
            let (_, queued) = peer
                .workspace_mut()
                .apply_workspace_task_command(
                    uuid::Uuid::new_v4(),
                    [1; 32],
                    caller,
                    &WorkspaceTaskCommand::Queue {
                        title: "Edit attached document".into(),
                        assigned_agent: "scribe".into(),
                        route: WorkspaceTaskRoute::Model,
                        resource: Some(Box::new(tilo_desktop_model::WorkspaceTaskResource {
                            app_id: Some("notes".into()),
                            app_label: Some("Notes".into()),
                            id: "A".into(),
                            kind: "document".into(),
                            label: "Document A".into(),
                            agent_request: None,
                            anchor: None,
                        })),
                    },
                    now,
                )
                .unwrap();
            let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = queued else {
                panic!("queued task")
            };
            peer.workspace_mut()
                .apply_workspace_task_command(
                    uuid::Uuid::new_v4(),
                    [2; 32],
                    &WorkspaceTaskAuthority::Host {
                        attestor: governor.clone(),
                    },
                    &WorkspaceTaskCommand::StartRun {
                        task_id: task_id.clone(),
                        agent_id: "scribe".into(),
                    },
                    now,
                )
                .unwrap();

            task_id
        }

        fn freeze_and_check_boundaries(&mut self) {
            use crate::workspace_membership::WorkspaceMembershipCommand;
            use tilo_action_policy::Level;
            let now = self.now;
            let key = SigningKey::from_bytes(&[2; 32]);
            let Self {
                peer,
                governor,
                caller,
                wildcard,
                subject,
                task_id,
                ..
            } = self;
            // The ceiling retains the concrete override, even after its live grant
            // is revoked. Live caller authority is now the ordinary Editor preset.
            for grant in ["starter-wildcard-deny", "starter-concrete-allow"] {
                peer.workspace_mut()
                    .apply_workspace_membership_command(
                        governor,
                        &WorkspaceMembershipCommand::Revoke {
                            grant: tilo_action_policy::GrantId::new(grant),
                        },
                        &key,
                        now,
                    )
                    .unwrap();
            }
            let command = WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Append the result".into(),
            };
            // An allow on one concrete item must not stand in for every item
            // matched by a wildcard deny: the unnamed remainder is a separate cell.
            peer.governance_mut()
                .put_governed_grants(
                    &[Grant::new(
                        "caller-selected-item-allow",
                        subject.clone(),
                        wildcard.clone().item("selected").action("append"),
                        KindSet::UNDOABLE,
                        Answer::Allow,
                        Subject::person("governor"),
                    )],
                    &key,
                    now,
                )
                .unwrap();
            // Each denial crosses a different policy boundary inside the same
            // concrete attachment, including a concrete exception to a wildcard.
            for node in [
                wildcard.clone().action("append"),
                wildcard.clone().any(Level::Item).action("append"),
                wildcard.clone().item("selected").action("append"),
                wildcard.clone().rest().action("append"),
            ] {
                peer.governance_mut()
                    .put_governed_grants(
                        &[Grant::new(
                            "caller-missing-append",
                            subject.clone(),
                            node,
                            KindSet::UNDOABLE,
                            Answer::Deny,
                            Subject::person("governor"),
                        )],
                        &key,
                        now,
                    )
                    .unwrap();
                let before = peer.durable().unwrap();
                assert_eq!(
                    peer.workspace_mut()
                        .apply_workspace_task_command(
                            uuid::Uuid::new_v4(),
                            [3; 32],
                            caller,
                            &command,
                            now,
                        )
                        .unwrap_err(),
                    "task steering requires every execution permission"
                );
                assert_eq!(peer.durable().unwrap(), before);
                peer.workspace_mut()
                    .apply_workspace_membership_command(
                        governor,
                        &WorkspaceMembershipCommand::Revoke {
                            grant: tilo_action_policy::GrantId::new("caller-missing-append"),
                        },
                        &key,
                        now,
                    )
                    .unwrap();
            }
        }

        fn check_revoked_scope(&mut self) {
            let now = self.now;
            let key = SigningKey::from_bytes(&[2; 32]);
            let Self {
                peer,
                caller,
                scope,
                subject,
                task_id,
                ..
            } = self;
            let command = WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Append the result".into(),
            };

            // Revoking a live permission that the frozen ceiling still allows
            // must not shrink the ceiling used to authorize steering.
            peer.governance_mut()
                .put_governed_grants(
                    &[Grant::new(
                        "caller-attached-revoked",
                        subject.clone(),
                        scope.clone(),
                        KindSet::UNDOABLE,
                        Answer::Deny,
                        Subject::person("governor"),
                    )],
                    &key,
                    now,
                )
                .unwrap();
            let before = peer.durable().unwrap();
            assert!(
                peer.workspace_mut()
                    .apply_workspace_task_command(
                        uuid::Uuid::new_v4(),
                        [4; 32],
                        caller,
                        &command,
                        now,
                    )
                    .is_err()
            );
            assert_eq!(peer.durable().unwrap(), before);
        }

        fn check_owner_epoch(&mut self) {
            let now = self.now;
            let Self {
                peer,
                governor,
                task_id,
                ..
            } = self;
            let command = WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Append the result".into(),
            };

            // Even an owner cannot steer using another epoch's frozen ceiling.
            let saved_room = peer.core.room_doc.clone();
            let mut stale_doc = saved_room.clone().unwrap();
            let mut controls = crate::workspace_task::controls_of(&stale_doc).unwrap();
            controls
                .get_mut(task_id.as_str())
                .unwrap()
                .execution_authority
                .as_mut()
                .unwrap()
                .epoch = 0;
            crate::workspace_task::project_controls(&mut stale_doc, &controls).unwrap();
            stale_doc.commit();
            peer.core.room_doc = Some(stale_doc);
            let owner = WorkspaceTaskAuthority::Person {
                caller: governor.clone(),
            };
            let before = peer.durable().unwrap();
            assert!(
                peer.workspace_mut()
                    .apply_workspace_task_command(
                        uuid::Uuid::new_v4(),
                        [5; 32],
                        &owner,
                        &command,
                        now,
                    )
                    .is_err()
            );
            assert_eq!(peer.durable().unwrap(), before);
            peer.core.room_doc = saved_room;
            // A current unbounded owner still has ordinary full steering rights.
            assert!(
                peer.workspace_mut()
                    .apply_workspace_task_command(
                        uuid::Uuid::new_v4(),
                        [5; 32],
                        &owner,
                        &command,
                        now,
                    )
                    .unwrap()
                    .0
            );
        }
    }

    #[test]
    fn steering_intersects_frozen_wildcards_with_attached_scope_and_live_grants() {
        let mut fixture = SteeringFixture::new();
        fixture.freeze_and_check_boundaries();
        fixture.check_revoked_scope();
        fixture.check_owner_epoch();
    }

    #[test]
    fn foreign_room_membership_cannot_authorize_an_agent_task() {
        let (mut peer, governor, _, now) = peer();
        let key = SigningKey::from_bytes(&[2; 32]);
        peer.workspace_mut()
            .apply_workspace_membership_command(
                &governor,
                &crate::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                    agent: "scribe".into(),
                },
                &key,
                now,
            )
            .unwrap();
        assert!(
            peer.governance_mut()
                .put_governed_grants(
                    &[agent_grant(
                        "other-room-agent",
                        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
                    )],
                    &key,
                    now,
                )
                .is_err(),
            "a grant on another room is not this room's policy"
        );
        let command = WorkspaceTaskCommand::Queue {
            title: "Keep room boundaries".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            resource: None,
        };
        let authority = WorkspaceTaskAuthority::Person { caller: governor };
        let before = peer.durable().unwrap();
        assert!(
            peer.workspace_mut()
                .apply_workspace_task_command(
                    uuid::Uuid::from_u128(1),
                    [1; 32],
                    &authority,
                    &command,
                    now
                )
                .is_err()
        );
        assert_eq!(peer.durable().unwrap(), before);
        peer.governance_mut()
            .put_governed_grants(&[agent_grant("actual-room-agent", ROOM)], &key, now)
            .unwrap();
        assert!(
            peer.workspace_mut()
                .apply_workspace_task_command(
                    uuid::Uuid::from_u128(1),
                    [1; 32],
                    &authority,
                    &command,
                    now
                )
                .is_ok()
        );
    }

    /// A grant given on another device reaches the room through its
    /// governor, which decides it as it would for a person at its own
    /// screen: only someone holding the grant action gives one.
    #[test]
    fn the_governor_records_a_grant_given_elsewhere_by_whoever_may_grant() {
        use crate::workspace_membership::WorkspaceMembershipCommand;
        let (mut peer, governor, other, now) = peer();
        let key = SigningKey::from_bytes(&[2; 32]);
        let reader = Grant::new(
            "reader-mail",
            Subject::agent("reader"),
            TreePath::room(ROOM).app("so.tilo.mail"),
            KindSet::READ,
            Answer::Allow,
            Subject::person("anyone"),
        );
        let give = WorkspaceMembershipCommand::Grant {
            grant: reader.clone(),
        };
        let held = |peer: &StatePeer| {
            peer.permissions(governor.device_public_key, now)
                .unwrap()
                .policy
                .grants
                .into_iter()
                .find(|grant| grant.id == reader.id)
        };
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(&other, &give, &key, now)
                .is_err(),
            "someone without the grant action gives nothing"
        );
        assert!(held(&peer).is_none());
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(&governor, &give, &key, now)
                .unwrap()
        );
        let recorded = held(&peer).expect("recorded");
        assert_eq!(recorded.kinds, KindSet::READ);
        assert_ne!(
            recorded.granted_by,
            Subject::person("anyone"),
            "the governor records who gave it, not what the request claimed"
        );
        peer.workspace_mut()
            .apply_workspace_membership_command(
                &governor,
                &WorkspaceMembershipCommand::Revoke {
                    grant: reader.id.clone(),
                },
                &key,
                now,
            )
            .unwrap();
        assert!(held(&peer).is_none(), "taken back");
    }

    /// A task carries its own grant set, changed as the room's governance
    /// by someone who may work in the room, and ending with the task.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end scenario: its setup and assertions stay together"
    )]
    fn a_tasks_grant_set_is_edited_through_commands_and_ends_with_the_task() {
        use crate::workspace_membership::{WorkspaceMembershipCommand, task_grant_id};
        let (mut peer, governor, _, now) = peer();
        let key = SigningKey::from_bytes(&[2; 32]);
        let authority = WorkspaceTaskAuthority::Person {
            caller: governor.clone(),
        };
        peer.workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(7),
                [7; 32],
                &authority,
                &WorkspaceTaskCommand::Queue {
                    title: "Tidy the inbox".into(),
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                    resource: None,
                },
                now,
            )
            .unwrap();
        let snapshot = |peer: &StatePeer| {
            crate::workspace_task::snapshot(peer.core.room_doc.as_deref().unwrap())
        };
        let task = snapshot(&peer).unwrap().tasks[0].id.clone();
        let mail = TreePath::root().app("so.tilo.mail");
        let grant = |task: &str| WorkspaceMembershipCommand::GrantForTask {
            task: task.into(),
            node: mail.clone(),
            kinds: KindSet::READ_UNDOABLE,
            answer: Answer::Allow,
        };
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(&governor, &grant("task:absent"), &key, now)
                .is_err(),
            "only an open task carries grants"
        );
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(&governor, &grant(&task), &key, now)
                .unwrap()
        );
        let held = snapshot(&peer).unwrap().grants;
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].subject, Subject::Task(task.clone()));
        assert_eq!(held[0].node, TreePath::room(ROOM).join(&mail));
        assert_eq!(
            held[0].lifetime,
            tilo_action_policy::Lifetime::Task(task.clone())
        );
        assert!(
            peer.permissions(governor.device_public_key, now)
                .unwrap()
                .policy
                .grants
                .iter()
                .any(|grant| grant.subject == Subject::Task(task.clone())),
            "the task's grants are the room's while it is open"
        );

        peer.workspace_mut()
            .apply_workspace_membership_command(
                &governor,
                &WorkspaceMembershipCommand::RevokeForTask {
                    task: task.clone(),
                    grant: task_grant_id(&task, &mail),
                },
                &key,
                now,
            )
            .unwrap();
        assert!(snapshot(&peer).unwrap().grants.is_empty());

        peer.workspace_mut()
            .apply_workspace_membership_command(&governor, &grant(&task), &key, now)
            .unwrap();
        peer.workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(8),
                [8; 32],
                &authority,
                &WorkspaceTaskCommand::CancelForRun {
                    task_id: task.clone(),
                    run_epoch: 0,
                    after_revision: 0,
                },
                now,
            )
            .unwrap();
        assert!(
            snapshot(&peer).unwrap().grants.is_empty(),
            "the grant set ends with the task"
        );
        let room = peer.core.room_doc.as_deref().unwrap();
        let policy = tilo_desktop_model::projection::room_policy_of(room, ROOM);
        assert!(
            !holds_task_grant(&policy, &task),
            "and no longer counts as the room's"
        );
        let held = peer.permissions(governor.device_public_key, now).unwrap();
        assert!(
            !holds_task_grant(&held.policy, &task),
            "a receiver deciding on the room's grants, as the relay does, no longer holds it"
        );
    }

    fn holds_task_grant(policy: &tilo_action_policy::RoomPolicy, task: &str) -> bool {
        policy
            .grants
            .iter()
            .any(|grant| grant.subject == Subject::Task(task.to_owned()))
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end scenario: its setup and assertions stay together"
    )]
    fn task_execution_budget_requires_actual_owner_and_exact_inactive_frontier() {
        use collab_types::task_budget::TaskExecutionBudget;
        let (mut peer, governor, other, now) = peer();
        let owner = WorkspaceTaskAuthority::Person {
            caller: governor.clone(),
        };
        let (_, queued) = peer
            .workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(80),
                [80; 32],
                &owner,
                &WorkspaceTaskCommand::Queue {
                    title: "Bound coding work".into(),
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                    resource: None,
                },
                now,
            )
            .unwrap();
        let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = queued else {
            panic!("queued task")
        };
        let budget = TaskExecutionBudget {
            turn_ms: 900_000,
            total_ms: 7_200_000,
        };
        let command = WorkspaceTaskCommand::SetExecutionBudget {
            task_id: task_id.clone(),
            expected_run_epoch: 0,
            expected_budget: None,
            budget: Some(budget),
        };
        let before = peer.durable().unwrap();
        for authority in [
            WorkspaceTaskAuthority::Person { caller: other },
            WorkspaceTaskAuthority::Host {
                attestor: governor.clone(),
            },
            WorkspaceTaskAuthority::Agent {
                attestor: governor.clone(),
                agent_id: "scribe".into(),
            },
        ] {
            assert!(
                peer.workspace_mut()
                    .apply_workspace_task_command(
                        uuid::Uuid::new_v4(),
                        [1; 32],
                        &authority,
                        &command,
                        now
                    )
                    .is_err()
            );
            assert_eq!(peer.durable().unwrap(), before);
        }
        let invocation = uuid::Uuid::from_u128(81);
        let result = peer
            .workspace_mut()
            .apply_workspace_task_command(invocation, [81; 32], &owner, &command, now)
            .unwrap();
        let saved = peer.durable().unwrap();
        assert_eq!(
            peer.workspace_mut()
                .apply_workspace_task_command(invocation, [81; 32], &owner, &command, now)
                .unwrap(),
            (false, result.1)
        );
        assert_eq!(peer.durable().unwrap(), saved);
        assert!(
            peer.workspace_mut()
                .apply_workspace_task_command(uuid::Uuid::new_v4(), [3; 32], &owner, &command, now)
                .is_err()
        );
        assert_eq!(peer.durable().unwrap(), saved);
        assert_eq!(
            crate::workspace_task::controls_of(peer.core.room_doc.as_deref().unwrap()).unwrap()
                [&task_id]
                .execution_budget,
            Some(budget)
        );
        peer.workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(82),
                [82; 32],
                &WorkspaceTaskAuthority::Host { attestor: governor },
                &WorkspaceTaskCommand::StartRun {
                    task_id: task_id.clone(),
                    agent_id: "scribe".into(),
                },
                now,
            )
            .unwrap();
        let active = peer.durable().unwrap();
        let clear = WorkspaceTaskCommand::SetExecutionBudget {
            task_id,
            expected_run_epoch: 1,
            expected_budget: Some(budget),
            budget: None,
        };
        assert!(
            peer.workspace_mut()
                .apply_workspace_task_command(uuid::Uuid::new_v4(), [4; 32], &owner, &clear, now)
                .is_err()
        );
        assert_eq!(peer.durable().unwrap(), active);
    }

    #[test]
    fn task_execution_budget_rejects_invalid_caps_without_mutation() {
        use collab_types::task_budget::TaskExecutionBudget;
        let (mut peer, governor, _, now) = peer();
        let owner = WorkspaceTaskAuthority::Person { caller: governor };
        let (_, queued) = peer
            .workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(90),
                [90; 32],
                &owner,
                &WorkspaceTaskCommand::Queue {
                    title: "Bound coding work".into(),
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                    resource: None,
                },
                now,
            )
            .unwrap();
        let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = queued else {
            panic!("queued task")
        };
        let before = peer.durable().unwrap();
        for invalid in [
            TaskExecutionBudget {
                turn_ms: 0,
                total_ms: 1,
            },
            TaskExecutionBudget {
                turn_ms: 900_001,
                total_ms: 7_200_000,
            },
            TaskExecutionBudget {
                turn_ms: 1,
                total_ms: 7_200_001,
            },
            TaskExecutionBudget {
                turn_ms: 101,
                total_ms: 100,
            },
        ] {
            let bad = WorkspaceTaskCommand::SetExecutionBudget {
                task_id: task_id.clone(),
                expected_run_epoch: 0,
                expected_budget: None,
                budget: Some(invalid),
            };
            assert!(
                peer.workspace_mut()
                    .apply_workspace_task_command(uuid::Uuid::new_v4(), [2; 32], &owner, &bad, now)
                    .is_err()
            );
            assert_eq!(peer.durable().unwrap(), before);
        }
    }

    #[test]
    fn execution_phase_rechecks_current_agent_membership_before_mutation() {
        use tilo_desktop_model::WorkspaceTaskExecutionPhase;
        let (mut peer, governor, _, now) = peer();
        let (_, queued) = peer
            .workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(1),
                [1; 32],
                &WorkspaceTaskAuthority::Person {
                    caller: governor.clone(),
                },
                &WorkspaceTaskCommand::Queue {
                    title: "Private full instruction is not a phase".into(),
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                    resource: None,
                },
                now,
            )
            .unwrap();
        let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = queued else {
            panic!("queue result")
        };
        peer.workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(2),
                [2; 32],
                &WorkspaceTaskAuthority::Host {
                    attestor: governor.clone(),
                },
                &WorkspaceTaskCommand::StartRun {
                    task_id: task_id.clone(),
                    agent_id: "scribe".into(),
                },
                now,
            )
            .unwrap();
        let authority = WorkspaceTaskAuthority::Agent {
            attestor: governor.clone(),
            agent_id: "scribe".into(),
        };
        peer.workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(3),
                [3; 32],
                &authority,
                &WorkspaceTaskCommand::ExecutionPhase {
                    task_id: task_id.clone(),
                    run_epoch: 1,
                    sequence: 0,
                    phase: WorkspaceTaskExecutionPhase::Accepted,
                },
                now,
            )
            .unwrap();
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(
                    &governor,
                    &crate::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                        agent: "scribe".into()
                    },
                    &SigningKey::from_bytes(&[2; 32]),
                    now,
                )
                .unwrap()
        );
        let before = peer.durable().unwrap();
        assert!(
            peer.workspace_mut()
                .apply_workspace_task_command(
                    uuid::Uuid::from_u128(4),
                    [4; 32],
                    &authority,
                    &WorkspaceTaskCommand::ExecutionPhase {
                        task_id: task_id.clone(),
                        run_epoch: 1,
                        sequence: 1,
                        phase: WorkspaceTaskExecutionPhase::Planning
                    },
                    now,
                )
                .is_err()
        );
        assert_eq!(peer.durable().unwrap(), before);
        assert_stop_refused_after_agent_removal(&mut peer, &governor, &task_id, now);
        assert_eq!(peer.durable().unwrap(), before);
        let phase = crate::workspace_task::snapshot(peer.admitted_room().unwrap())
            .unwrap()
            .controls[&task_id]
            .execution
            .clone()
            .unwrap();
        assert_eq!(phase.phase, WorkspaceTaskExecutionPhase::Accepted);
        assert_eq!(phase.sequence, 0);
        let encoded = serde_json::to_string(&phase).unwrap();
        assert!(!encoded.contains("Private full instruction"));
        assert!(!encoded.contains("message"));
    }

    #[test]
    fn manual_creation_keeps_current_membership_write_admission() {
        let (mut peer, _, other, now) = peer();
        read_only_member(&mut peer, &other, now);
        let before = peer.durable().unwrap();
        let result = peer.workspace_mut().apply_workspace_task_command(
            uuid::Uuid::from_u128(81),
            [81; 32],
            &WorkspaceTaskAuthority::Person { caller: other },
            &WorkspaceTaskCommand::CreateManual {
                title: "Review mapped tile".into(),
                resource: None,
            },
            now,
        );
        assert_eq!(
            result.unwrap_err(),
            "workspace task person lacks write authority"
        );
        assert_eq!(peer.durable().unwrap(), before);
        assert!(
            crate::workspace_task::snapshot(peer.admitted_room().unwrap())
                .unwrap()
                .tasks
                .is_empty()
        );
    }

    #[test]
    fn current_member_with_read_restriction_cannot_queue_a_task() {
        let (mut peer, _, other, now) = peer();
        read_only_member(&mut peer, &other, now);
        let before = peer.durable().unwrap();
        let result = peer.workspace_mut().apply_workspace_task_command(
            uuid::Uuid::from_u128(1),
            [1; 32],
            &WorkspaceTaskAuthority::Person { caller: other },
            &WorkspaceTaskCommand::Queue {
                title: "Must not execute".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
            now,
        );
        assert_eq!(
            result.unwrap_err(),
            "workspace task person lacks write authority"
        );
        assert_eq!(peer.durable().unwrap(), before);
        assert!(
            crate::workspace_task::snapshot(peer.admitted_room().unwrap())
                .unwrap()
                .tasks
                .is_empty()
        );
    }

    #[test]
    fn exact_primary_governor_may_supervise_but_another_primary_may_not() {
        let (mut peer, governor, other, now) = peer();
        assert!(!governor.capabilities.can_act_as_host);
        assert!(!other.capabilities.can_act_as_host);
        let queue = WorkspaceTaskCommand::Queue {
            title: "Prepare the release notes".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            resource: None,
        };
        let (_, result) = peer
            .workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(1),
                [1; 32],
                &WorkspaceTaskAuthority::Person {
                    caller: governor.clone(),
                },
                &queue,
                now,
            )
            .unwrap();
        let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = result else {
            panic!("queue result")
        };
        let start = WorkspaceTaskCommand::StartRun {
            task_id: task_id.clone(),
            agent_id: "scribe".into(),
        };
        let refused = peer
            .workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(2),
                [2; 32],
                &WorkspaceTaskAuthority::Host { attestor: other },
                &start,
                now,
            )
            .unwrap_err();
        assert!(refused.contains("attestor is not a host"), "{refused}");
        peer.workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(3),
                [3; 32],
                &WorkspaceTaskAuthority::Host { attestor: governor },
                &start,
                now,
            )
            .unwrap();
        let task = crate::workspace_task::snapshot(peer.admitted_room().unwrap())
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.id == task_id)
            .unwrap();
        assert_eq!(task.status, WorkspaceTaskStatus::InProgress);
    }
}

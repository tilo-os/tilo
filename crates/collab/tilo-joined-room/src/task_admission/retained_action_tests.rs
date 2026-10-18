//! Retained reads exercise the durable owner ledger, not caller starter labels.
use super::*;
use crate::task_admission::tests::local_admission_tests::action_requirement;

fn admit_action(
    f: &Fixture,
    binding: &NativeInvocationBinding,
) -> crate::task_policy::NativeTaskActionRequirement {
    let requirement = action_requirement(f, binding);
    admit_requirement(f, binding, &requirement);
    requirement
}

fn admit_requirement(
    f: &Fixture,
    binding: &NativeInvocationBinding,
    requirement: &crate::task_policy::NativeTaskActionRequirement,
) {
    let exact = confirmation::exact(requirement, binding.room, binding.host).unwrap();
    f.owner
        .admit_local_native_invocation(
            exact,
            &f.certificate,
            &f.key,
            |view, exact, now| view.check_native_task_action(requirement, exact, now),
            || Ok(()),
        )
        .unwrap();
}

fn observe(
    f: &Fixture,
    requirement: &crate::task_policy::NativeTaskActionRequirement,
) -> Result<(), String> {
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let invocation = requirement.invocation.as_ref().unwrap().id;
    f.owner
        .authorize_retained_native_invocation(room, invocation, |view, exact, now| {
            view.check_retained_native_task_action(requirement, exact, now)
        })
}

fn set_task_answer(
    f: &Fixture,
    binding: &NativeInvocationBinding,
    answer: tilo_action_policy::Answer,
) {
    let messages = f
        .owner
        .with_current(binding.room, |view| {
            let mut peer = view.peer().clone();
            let node = tilo_action_policy::TreePath::room(peer.room());
            peer.governance_mut().put_governed_grants(
                &[tilo_action_policy::Grant::new(
                    "retained-action-ceiling",
                    tilo_action_policy::Subject::Task(binding.task.clone()),
                    node,
                    tilo_action_policy::KindSet::ALL,
                    answer,
                    tilo_action_policy::Subject::person("governor"),
                )],
                &f.key,
                f.now,
            )?;
            peer.durable_mutation_messages(
                view.peer(),
                &f.key,
                f.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        f.owner
            .receive(
                binding.room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: f.certificate.device_public_key,
                },
                message,
                f.now,
            )
            .unwrap();
    }
}

#[test]
fn retained_action_uses_canonical_device_authority_and_survives_terminal_reopen() {
    let (mut f, binding) = running_with_model(true);
    let requirement = admit_action(&f, &binding);
    observe(&f, &requirement).unwrap();
    f.owner
        .commit_workspace_task_command(
            OwnerCommitRequest {
                room: binding.room.to_string(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::new_v4(),
                    authority: WorkspaceTaskAuthority::Agent {
                        attestor: f.certificate.clone(),
                        agent_id: binding.agent.clone(),
                    },
                    command: WorkspaceTaskCommand::Finish {
                        task_id: binding.task.clone(),
                        run_epoch: binding.epoch,
                        outcome: tilo_peer_state::workspace_task::WorkspaceTaskFinish::Done,
                        summary: "Completed review".into(),
                    },
                },
            },
            f.now,
        )
        .unwrap();
    assert!(
        f.owner
            .workspace_task_snapshot(binding.room)
            .unwrap()
            .controls[&binding.task]
            .active_run
            .is_none()
    );
    f.owner = crate::open_file(&f.directory.path().join("owner.json")).unwrap();
    observe(&f, &requirement).unwrap();
    assert!(
        f.owner
            .admit_local_native_invocation(
                confirmation::exact(&requirement, binding.room, binding.host).unwrap(),
                &f.certificate,
                &f.key,
                |view, exact, now| view.check_native_task_action(&requirement, exact, now),
                || -> Result<(), String> { panic!("terminal retained result replayed") },
            )
            .is_err()
    );
}

#[test]
fn retained_action_read_uses_device_role_and_rechecks_current_deny() {
    let (f, binding) = running_with_model(true);
    let mut requirement = action_requirement(&f, &binding);
    let original = requirement.invocation.as_ref().unwrap();
    let contract = tilo_actions::ActionContract {
        kind: tilo_action_policy::ActionKind::Read,
        ..Default::default()
    };
    let mut invocation = crate::task_policy::NativeTaskInvocation::new(
        original.id,
        &original.provider,
        original.arguments.as_deref(),
        &contract,
        "replaced by owner digest below",
    )
    .unwrap();
    invocation.private_run_digest = original.private_run_digest;
    requirement.invocation = Some(invocation);
    requirement.need = contract.need();
    admit_requirement(&f, &binding, &requirement);
    observe(&f, &requirement).unwrap();
    set_task_answer(&f, &binding, tilo_action_policy::Answer::Deny);
    assert!(observe(&f, &requirement).is_err());
}

#[test]
fn retained_action_ask_observes_but_deny_and_forged_source_metadata_do_not() {
    let (f, binding) = running_with_model(true);
    let requirement = admit_action(&f, &binding);
    for change in 0..6 {
        let mut forged = requirement.clone();
        match change {
            0 => forged.binding.as_mut().unwrap().starter = Some("another person".into()),
            1 => forged.invocation.as_mut().unwrap().provider = "another-provider".into(),
            2 => forged.invocation.as_mut().unwrap().arguments = Some("changed arguments".into()),
            3 => forged.invocation.as_mut().unwrap().private_run_digest = [44; 32],
            4 => {
                forged.invocation.as_mut().unwrap().contract.kind =
                    tilo_action_policy::ActionKind::Read;
            }
            _ => forged.invocation.as_mut().unwrap().id = uuid::Uuid::new_v4(),
        }
        assert!(
            observe(&f, &forged).is_err(),
            "forged source variant {change}"
        );
    }
    set_task_answer(&f, &binding, tilo_action_policy::Answer::Ask);
    observe(&f, &requirement).unwrap();
    let mut fresh = requirement.clone();
    let id = uuid::Uuid::new_v4();
    fresh.invocation.as_mut().unwrap().id = id;
    fresh.invocation.as_mut().unwrap().request = id;
    assert!(
        f.owner
            .admit_local_native_invocation(
                confirmation::exact(&fresh, binding.room, binding.host).unwrap(),
                &f.certificate,
                &f.key,
                |view, exact, now| view.check_native_task_action(&fresh, exact, now),
                || -> Result<(), String> { panic!("retained Ask became a fresh admission") },
            )
            .is_err()
    );
    set_task_answer(&f, &binding, tilo_action_policy::Answer::Deny);
    assert!(observe(&f, &requirement).is_err());
}

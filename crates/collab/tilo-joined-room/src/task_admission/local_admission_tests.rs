//! Local admission exercises the real durable store and owner mutation gate.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct ObservedStore {
    inner: Arc<dyn crate::Store>,
    publications: AtomicUsize,
    fail: AtomicBool,
}
impl crate::Store for ObservedStore {
    fn healthy(&self) -> bool {
        self.inner.healthy()
    }
    fn uncertain(&self, room: RoomId) -> bool {
        self.inner.uncertain(room)
    }
    fn slot_room(&self, slot: crate::WorkspaceId) -> Option<RoomId> {
        self.inner.slot_room(slot)
    }
    fn put_history(&self, bytes: &[u8]) -> Result<[u8; 32], String> {
        self.inner.put_history(bytes)
    }
    fn read_history(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        self.inner.read_history(hash)
    }
    fn publish(
        &self,
        next: &crate::NativeAuthorityStorage,
        change: crate::Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        self.publications.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err("injected durable publication failure".into());
        }
        self.inner.publish(next, change, install)
    }
}
fn observed(f: &mut Fixture) -> Arc<ObservedStore> {
    let store = Arc::new(ObservedStore {
        inner: f.owner.0.store.clone(),
        publications: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let authority = f.owner.0.authority.read().clone();
    f.owner = NativeWorkspaceOwner::restore(store.clone(), authority, SystemTime::now()).unwrap();
    store
}
fn local<T>(
    f: &Fixture,
    binding: NativeInvocationBinding,
    effect: impl FnOnce() -> Result<T, String>,
) -> Result<(T, NativeInvocationPermit), String> {
    f.owner
        .admit_local_native_invocation(binding, &f.certificate, &f.key, |_, _, _| Ok(()), effect)
}

#[test]
fn local_admission_publishes_once_before_effect_and_retains_signed_consumption_after_reopen() {
    let (mut f, binding) = running();
    let store = observed(&mut f);
    let (_, permit) = local(&f, binding.clone(), || {
        assert_eq!(store.publications.load(Ordering::SeqCst), 1);
        assert!(
            f.owner.0.mutation.try_lock().is_none(),
            "enqueue escaped the Stop mutation turn"
        );
        let disk = crate::open_file(&f.directory.path().join("owner.json")).unwrap();
        disk.with_current(binding.room, |view| {
            assert_eq!(
                view.retained_native_invocation(binding.invocation)?,
                &binding
            );
            Ok(())
        })?;
        Ok(())
    })
    .unwrap();
    permit.verify().unwrap();
    assert_eq!(permit.reservation.binding, binding);
    assert_eq!(store.publications.load(Ordering::SeqCst), 1);
    let reopened = crate::open_file(&f.directory.path().join("owner.json")).unwrap();
    assert!(
        reopened
            .admit_local_native_invocation(
                binding.clone(),
                &f.certificate,
                &f.key,
                |_, _, _| Ok(()),
                || -> Result<(), String> { panic!("consumed local invocation replayed") }
            )
            .is_err()
    );
    let mut second = binding.clone();
    second.invocation = uuid::Uuid::new_v4();
    assert!(
        local(&f, second, || -> Result<(), String> {
            panic!("request reused with a second UUID")
        })
        .is_err()
    );
    assert_eq!(store.publications.load(Ordering::SeqCst), 1);
}

#[test]
fn local_admission_failed_publication_cannot_enqueue() {
    let (mut f, binding) = running();
    let store = observed(&mut f);
    store.fail.store(true, Ordering::SeqCst);
    let error = local(&f, binding.clone(), || -> Result<(), String> {
        panic!("failed publication enqueued")
    })
    .unwrap_err();
    assert!(
        error.contains("injected durable publication failure"),
        "{error}"
    );
    assert_eq!(store.publications.load(Ordering::SeqCst), 1);
    let disk = crate::open_file(&f.directory.path().join("owner.json")).unwrap();
    disk.with_current(binding.room, |view| {
        assert!(view.retained_native_invocation(binding.invocation).is_err());
        Ok(())
    })
    .unwrap();
    store.fail.store(false, Ordering::SeqCst);
    local(&f, binding, || Ok(())).unwrap();
}

#[test]
fn local_admission_callback_error_remains_unknown_consumed_and_cannot_replay() {
    let (mut f, binding) = running();
    let store = observed(&mut f);
    let result = local(&f, binding.clone(), || -> Result<(), String> {
        Err("transport outcome unknown".into())
    });
    assert_eq!(result.unwrap_err(), "transport outcome unknown");
    assert_eq!(store.publications.load(Ordering::SeqCst), 1);
    f.owner = crate::open_file(&f.directory.path().join("owner.json")).unwrap();
    f.owner
        .with_current(binding.room, |view| {
            let entry = &view.record.task_admission.reservations[&binding.invocation];
            assert!(entry.host_started);
            assert_eq!(entry.custody, Custody::Dispatched);
            assert!(entry.completion.is_none());
            entry.permit.as_ref().unwrap().verify()
        })
        .unwrap();
    assert!(
        local(&f, binding, || -> Result<(), String> {
            panic!("ambiguous effect replayed")
        })
        .is_err()
    );
}

#[test]
fn local_admission_stop_and_foreign_physical_host_refuse_before_publication() {
    let (mut f, binding) = running();
    let mut foreign = binding.clone();
    foreign.host = [77; 32];
    let store = observed(&mut f);
    assert!(
        local(&f, foreign, || -> Result<(), String> {
            panic!("foreign host used local fast path")
        })
        .is_err()
    );
    assert_eq!(store.publications.load(Ordering::SeqCst), 0);
    let fence = f
        .owner
        .stop_native_admission(binding.room, &binding.task, binding.epoch, &f.key)
        .unwrap();
    assert_eq!(
        f.owner
            .native_task_admission_state(binding.room, &binding.task, binding.epoch)
            .unwrap(),
        NativeTaskAdmissionState::Stopping,
        "unexposed local preparation still requires a durable physical-host fence"
    );
    assert!(
        local(&f, binding.clone(), || -> Result<(), String> {
            panic!("stopped invocation enqueued")
        })
        .is_err()
    );
    assert_eq!(store.publications.load(Ordering::SeqCst), 1);
    let ack = f
        .owner
        .acknowledge_native_host_fence(&fence, &f.certificate, &f.key)
        .unwrap();
    assert!(ack.dispatched.is_empty());
    assert_eq!(
        f.owner
            .native_task_admission_state(binding.room, &binding.task, binding.epoch)
            .unwrap(),
        NativeTaskAdmissionState::Stopped
    );
    assert_eq!(store.publications.load(Ordering::SeqCst), 2);
}

#[test]
fn local_admission_final_live_check_can_refuse_without_publication() {
    let (mut f, binding) = running();
    let store = observed(&mut f);
    let checks = AtomicUsize::new(0);
    let result = f.owner.admit_local_native_invocation(
        binding,
        &f.certificate,
        &f.key,
        |_, _, _| {
            if checks.fetch_add(1, Ordering::SeqCst) == 2 {
                Err("live contract revoked at final host check".into())
            } else {
                Ok(())
            }
        },
        || -> Result<(), String> { panic!("revoked effect enqueued") },
    );
    assert_eq!(
        result.unwrap_err(),
        "live contract revoked at final host check"
    );
    assert_eq!(checks.load(Ordering::SeqCst), 3);
    assert_eq!(store.publications.load(Ordering::SeqCst), 0);
}

#[test]
fn local_admission_fresh_agent_revocation_denies_without_publication() {
    let (mut f, binding) = running();
    let messages = f
        .owner
        .with_current(binding.room, |view| {
            let mut peer = view.peer().clone();
            peer.governance_mut().remove_governed_grants(
                &[tilo_action_policy::GrantId::new("admission-test-agent")],
                &f.key,
                f.now,
            )?;
            peer.durable_mutation_messages(view.peer(), &f.key, f.now, view.generation() + 1)
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
    let store = observed(&mut f);
    assert!(
        local(&f, binding, || -> Result<(), String> {
            panic!("revoked agent enqueued")
        })
        .is_err()
    );
    assert_eq!(store.publications.load(Ordering::SeqCst), 0);
}

fn current_run(f: &Fixture, room: RoomId, task: &str) -> crate::task_policy::NativeTaskBinding {
    let snapshot = f.owner.workspace_task_snapshot(room).unwrap();
    let control = &snapshot.controls[task];
    let active = control.active_run.as_ref().unwrap();
    crate::task_policy::NativeTaskBinding {
        task_id: task.into(),
        agent_id: active.agent_id.clone(),
        epoch: active.epoch,
        revision: control
            .directions
            .last()
            .map_or(0, |direction| direction.revision),
        starter: Some(active.approval.as_ref().unwrap().person.clone()),
    }
}

pub(super) fn action_requirement(
    f: &Fixture,
    exact: &NativeInvocationBinding,
) -> crate::task_policy::NativeTaskActionRequirement {
    use crate::task_policy::{NativeTaskActionRequirement, NativeTaskInvocation};
    let run = current_run(f, exact.room, &exact.task);
    let contract = tilo_actions::ActionContract {
        kind: tilo_action_policy::ActionKind::Undoable,
        ..Default::default()
    };
    let mut invocation = NativeTaskInvocation::new(
        exact.invocation,
        &exact.provider,
        Some(r#"{"text":"original"}"#),
        &contract,
        "replaced below",
    )
    .unwrap();
    invocation.private_run_digest = f
        .owner
        .with_current(exact.room, |view| {
            NativeWorkspaceOwner::current_private_run_digest(view, &run)
        })
        .unwrap();
    NativeTaskActionRequirement {
        binding: Some(run),
        app: exact.app.clone(),
        resource_kind: "document".into(),
        resource: exact.resource.clone(),
        action: exact.action.clone(),
        need: tilo_actions::ActionNeed {
            kind: contract.kind,
            field: None,
            privileged: false,
        },
        consequential: false,
        confirmation: None,
        invocation: Some(invocation),
    }
}

#[test]
fn local_admission_rechecks_canonical_arguments_and_private_owner_digest() {
    let (mut f, original) = running_with_model(true);
    let requirement = action_requirement(&f, &original);
    let exact = confirmation::exact(&requirement, original.room, original.host).unwrap();
    let store = observed(&mut f);
    for private in [false, true] {
        let mut changed = requirement.clone();
        if private {
            changed.invocation.as_mut().unwrap().private_run_digest = [55; 32];
        } else {
            changed.invocation.as_mut().unwrap().arguments =
                Some(r#"{"text":"substituted"}"#.into());
        }
        let error = f
            .owner
            .admit_local_native_invocation(
                exact.clone(),
                &f.certificate,
                &f.key,
                |view, binding, now| view.check_native_task_action(&changed, binding, now),
                || -> Result<(), String> { panic!("changed canonical/private arguments enqueued") },
            )
            .unwrap_err();
        if private {
            assert!(error.contains("private run snapshot changed"), "{error}");
        } else {
            assert!(
                error.contains("metadata") || error.contains("arguments"),
                "{error}"
            );
        }
    }
    assert_eq!(store.publications.load(Ordering::SeqCst), 0);
    f.owner
        .admit_local_native_invocation(
            exact,
            &f.certificate,
            &f.key,
            |view, binding, now| view.check_native_task_action(&requirement, binding, now),
            || Ok(()),
        )
        .unwrap();
    assert_eq!(store.publications.load(Ordering::SeqCst), 1);
}

#[test]
fn local_model_admission_refuses_changed_body_and_private_run_before_enqueue() {
    use crate::task_admission::model::*;
    struct Authority(NativeModelContext);
    impl NativeModelAuthority for Authority {
        fn current_model_context(
            &self,
            _: &NativeCommittedView,
            _: SystemTime,
        ) -> Result<NativeModelContext, String> {
            Ok(self.0.clone())
        }
    }
    let (mut f, binding) = running_with_model(true);
    let action = action_requirement(&f, &binding);
    let run = action.binding.unwrap();
    let private = action.invocation.unwrap().private_run_digest;
    let contract = NativeModelContract::for_task(
        &binding.room.to_string(),
        &binding.task,
        tilo_action_policy::ActionKind::Outside,
        FrozenNativeModelSelection {
            app: "so.tilo.agent".into(),
            provider: "local".into(),
            model: "selected".into(),
            contract: serde_json::json!({"endpoint":"http://localhost"}),
        },
    );
    let mut requirement = TypedNativeModelRequirement::freeze(
        TypedNativeModelRequirement {
            room: binding.room,
            run: run.clone(),
            invocation: binding.invocation,
            request: binding.request,
            host: binding.host,
            private_run_digest: private,
            modelselection: contract.clone(),
            requestdigest: [0; 32],
        },
        b"frozen body",
    )
    .unwrap();
    let authority = Authority(NativeModelContext {
        actor: run.agent_id,
        task: run.task_id,
        epoch: run.epoch,
        revision: run.revision,
        planning: NativeModelPlanning::Model,
        private_run_digest: private,
        contract,
    });
    let store = observed(&mut f);
    let gate = HostDispatchModel {
        owner: &f.owner,
        authority: &authority,
    };
    let custody = || NativeModelGovernorCustody {
        host: &f.certificate,
        key: &f.key,
        now: SystemTime::now(),
    };
    let error = gate
        .dispatch_local(
            &requirement,
            custody(),
            b"substituted body",
            |_| -> Result<(), String> { panic!("changed HTTP body enqueued") },
        )
        .unwrap_err();
    assert_eq!(error, "native MODEL final HTTP body changed");
    requirement.private_run_digest = [56; 32];
    let error = gate
        .dispatch_local(
            &requirement,
            custody(),
            b"frozen body",
            |_| -> Result<(), String> { panic!("changed private model run enqueued") },
        )
        .unwrap_err();
    assert_eq!(error, "native MODEL private owner envelope changed");
    assert_eq!(store.publications.load(Ordering::SeqCst), 0);
}

struct ApprovedModel;
impl crate::task_admission::model::NativeModelStartAuthority for ApprovedModel {
    fn approved_model_start(
        &self,
        view: &NativeCommittedView,
        task: &str,
        _: &tilo_user_identity::DeviceCert,
        _: SystemTime,
    ) -> Result<crate::task_admission::model::NativeModelContract, String> {
        use crate::task_admission::model::*;
        Ok(NativeModelContract::for_task(
            &view.record.room.to_string(),
            task,
            tilo_action_policy::ActionKind::Outside,
            FrozenNativeModelSelection {
                app: "so.tilo.agent".into(),
                provider: "local".into(),
                model: "selected".into(),
                contract: serde_json::json!({"endpoint":"http://localhost"}),
            },
        ))
    }
}

fn grant_model(f: &Fixture, room: RoomId, task: &str) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject};
    let messages = f
        .owner
        .with_current(room, |view| {
            let mut peer = view.peer().clone();
            let person = peer
                .permissions(f.certificate.device_public_key, f.now)?
                .person;
            let node = crate::task_admission::model::NativeModelContract::task_resource(
                &room.to_string(),
                task,
            )
            .action("requestModel");
            let grants = [Subject::person(person.clone()), Subject::agent("scribe")]
                .into_iter()
                .enumerate()
                .map(|(i, subject)| {
                    Grant::new(
                        format!("local-model-{i}"),
                        subject,
                        node.clone(),
                        KindSet::OUTSIDE,
                        Answer::Allow,
                        Subject::person(person.clone()),
                    )
                })
                .collect::<Vec<_>>();
            peer.governance_mut()
                .put_governed_grants(&grants, &f.key, f.now)?;
            peer.durable_mutation_messages(view.peer(), &f.key, f.now, view.generation() + 1)
        })
        .unwrap();
    for message in messages {
        f.owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: f.certificate.device_public_key,
                },
                message,
                f.now,
            )
            .unwrap();
    }
}

fn approved_model() -> (
    Fixture,
    crate::task_admission::model::TypedNativeModelRequirement,
) {
    use crate::task_admission::model::*;
    let f = fixture();
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    grant_running_agent(&f, room, true);
    let request = |command| OwnerCommitRequest {
        room: room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: f.certificate.clone(),
            },
            command,
        },
    };
    f.owner
        .commit_workspace_task_command(
            request(WorkspaceTaskCommand::QueueInstruction {
                title: "Review".into(),
                instruction: "Keep the approved scope".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            }),
            f.now,
        )
        .unwrap();
    let task = f.owner.workspace_task_snapshot(room).unwrap().tasks[0]
        .id
        .clone();
    grant_model(&f, room, &task);
    f.owner
        .commit_workspace_task_command_with_model_start(
            request(WorkspaceTaskCommand::ApproveRun {
                task_id: task.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            }),
            &f.key,
            &ApprovedModel,
            f.now,
        )
        .unwrap();
    let run = current_run(&f, room, &task);
    let (contract, private) = f
        .owner
        .with_current(room, |view| {
            Ok((
                NativeModelContract::approved_for_run(view, &run)?,
                NativeWorkspaceOwner::current_private_run_digest(view, &run)?,
            ))
        })
        .unwrap();
    let requirement = TypedNativeModelRequirement::freeze(
        TypedNativeModelRequirement {
            room,
            run,
            invocation: uuid::Uuid::new_v4(),
            request: uuid::Uuid::new_v4(),
            host: f.certificate.device_public_key,
            private_run_digest: private,
            modelselection: contract,
            requestdigest: [0; 32],
        },
        b"exact model request",
    )
    .unwrap();
    (f, requirement)
}

#[test]
fn local_model_admission_publishes_once_with_actual_typed_policy_and_signed_provenance() {
    use crate::task_admission::model::*;
    struct Authority(NativeModelContext);
    impl NativeModelAuthority for Authority {
        fn current_model_context(
            &self,
            _: &NativeCommittedView,
            _: SystemTime,
        ) -> Result<NativeModelContext, String> {
            Ok(self.0.clone())
        }
    }
    let (mut f, requirement) = approved_model();
    let store = observed(&mut f);
    let authority = Authority(NativeModelContext {
        actor: requirement.run.agent_id.clone(),
        task: requirement.run.task_id.clone(),
        epoch: requirement.run.epoch,
        revision: requirement.run.revision,
        planning: NativeModelPlanning::Model,
        private_run_digest: requirement.private_run_digest,
        contract: requirement.modelselection.clone(),
    });
    let ((), permit) = HostDispatchModel {
        owner: &f.owner,
        authority: &authority,
    }
    .dispatch_local(
        &requirement,
        NativeModelGovernorCustody {
            host: &f.certificate,
            key: &f.key,
            now: SystemTime::now(),
        },
        b"exact model request",
        |body| {
            assert_eq!(body, b"exact model request");
            assert_eq!(store.publications.load(Ordering::SeqCst), 1);
            assert!(f.owner.0.mutation.try_lock().is_none());
            let disk = crate::open_file(&f.directory.path().join("owner.json"))?;
            disk.with_current(requirement.room, |view| {
                assert_eq!(
                    view.retained_native_invocation(requirement.invocation)?,
                    &requirement.binding()?
                );
                Ok(())
            })
        },
    )
    .unwrap();
    permit.verify().unwrap();
    assert_eq!(store.publications.load(Ordering::SeqCst), 1);
    assert_eq!(permit.reservation.binding, requirement.binding().unwrap());
}

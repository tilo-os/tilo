use super::NativeTaskAdmissionTransfer;
use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

#[test]
fn foreign_governing_caller_is_uncapped_but_captured_ceiling_and_agent_still_apply() {
    use super::NativeTaskAdmissionCaller;
    use crate::services::{NativeTaskActionRequirement, NativeTaskBinding};
    use ed25519_dalek::SigningKey;
    use tilo_joined_room::testing;
    use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
    use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("owner.json")).unwrap();
    let root = SigningKey::from_bytes(&[91; 32]);
    let key = SigningKey::from_bytes(&[92; 32]);
    let mut certificate = testing::certificate(&root, &key, "Governor");
    certificate.capabilities.foreign = true;
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
    let room = tilo_desktop_model::RoomId::parse(testing::ROOM).unwrap();
    let now = std::time::SystemTime::now();
    owner
        .commit_workspace_membership_command(
            OwnerCommitRequest {
                room: testing::ROOM.into(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: certificate.clone(),
                    command: WorkspaceMembershipCommand::AddAgent {
                        agent: "scribe".into(),
                        preset: tilo_action_policy::roles::AgentPreset::Observer,
                    },
                },
            },
            &certificate,
            &key,
            now,
        )
        .unwrap();
    let requirement = NativeTaskActionRequirement {
        binding: Some(NativeTaskBinding {
            task_id: "task".into(),
            agent_id: "scribe".into(),
            epoch: 1,
            revision: 0,
            starter: None,
        }),
        app: "so.tilo.notes".into(),
        resource_kind: "document".into(),
        resource: "notes:release".into(),
        action: "append".into(),
        need: tilo_actions::ActionNeed {
            kind: tilo_action_policy::ActionKind::Undoable,
            field: None,
            privileged: false,
        },
        consequential: false,
        confirmation: None,
        invocation: None,
    };
    let caller = NativeTaskAdmissionCaller {
        certificate,
        agent_id: "scribe".into(),
        room: testing::ROOM.into(),
        capped: false,
    };
    owner
        .with_current(room, |view| {
            view.peer()
                .workspace()
                .check_workspace_administration(caller.certificate.device_public_key, now)?;
            caller.check(view, &requirement, now)?;
            let mut capped = caller.clone();
            capped.capped = true;
            assert_eq!(
                capped.check(view, &requirement, now).unwrap_err(),
                "native task original caller delegation is capped"
            );
            // Matching caller/run labels alone cannot admit an absent agent.
            let mut absent = caller.clone();
            absent.agent_id = "absent-agent".into();
            let mut absent_requirement = requirement.clone();
            absent_requirement.binding.as_mut().unwrap().agent_id = absent.agent_id.clone();
            assert!(absent.check(view, &absent_requirement, now).is_err());
            Ok(())
        })
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn local_enqueue_retains_the_workers_exact_turn_until_completion() {
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::task::LocalSet::new()
            .run_until(async {
                let turn = Arc::new(std::sync::Mutex::new(()));
                let worker_turn = turn.clone();
                let (mut transfer, ready) = NativeTaskAdmissionTransfer::channel();
                let worker = tokio::task::spawn_blocking(move || {
                    let result = {
                        let _guard = worker_turn.lock().unwrap();
                        transfer.ready_and_wait()
                    };
                    transfer.finish(result);
                });
                let ready = ready.await.unwrap();
                assert!(turn.try_lock().is_err(), "owner guard must remain held");
                // This original enqueue closure cannot cross a Send boundary.
                let queued = Rc::new(Cell::new(0));
                let enqueue = || queued.set(queued.get() + 1);
                // Another local task still progresses while the owner waits.
                let progress = queued.clone();
                tokio::task::spawn_local(async move {
                    assert_eq!(progress.get(), 0);
                })
                .await
                .unwrap();
                enqueue();
                ready.complete(Ok(())).await.unwrap();
                worker.await.unwrap();
                assert_eq!(queued.get(), 1);
                assert!(turn.try_lock().is_ok());
            })
            .await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dropping_ready_cancels_the_guarded_callback() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut transfer, ready) = NativeTaskAdmissionTransfer::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let result = transfer.ready_and_wait();
            assert!(result.as_ref().unwrap_err().contains("cancelled"));
            assert!(
                transfer
                    .ready_and_wait()
                    .unwrap_err()
                    .contains("already used")
            );
            transfer.finish(result);
        });
        drop(ready.await.unwrap());
        worker.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dropping_pending_admission_never_grants_readiness() {
    let (mut transfer, ready) = NativeTaskAdmissionTransfer::channel();
    drop(ready);
    tokio::task::spawn_blocking(move || {
        let result = transfer.ready_and_wait();
        assert!(result.as_ref().unwrap_err().contains("cancelled"));
        transfer.finish(result);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn owner_denial_and_interruption_before_readiness_fail_closed() {
    let (transfer, ready) = NativeTaskAdmissionTransfer::channel();
    transfer.finish(Err("owner policy denied".into()));
    assert_eq!(ready.await.err().unwrap(), "owner policy denied");

    let (transfer, ready) = NativeTaskAdmissionTransfer::channel();
    drop(transfer);
    assert!(ready.await.err().unwrap().contains("interrupted"));

    let (transfer, ready) = NativeTaskAdmissionTransfer::channel();
    transfer.finish(Ok(()));
    assert!(ready.await.err().unwrap().contains("without readiness"));
}

#[tokio::test]
async fn enqueue_error_and_final_owner_error_reach_local_caller() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut transfer, ready) = NativeTaskAdmissionTransfer::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let result = transfer.ready_and_wait();
            assert_eq!(result, Err("enqueue refused".into()));
            transfer.finish(result);
        });
        let ready = ready.await.unwrap();
        assert_eq!(
            ready.complete(Err("enqueue refused".into())).await,
            Err("enqueue refused".into())
        );
        worker.await.unwrap();

        let (mut transfer, ready) = NativeTaskAdmissionTransfer::channel();
        let worker = tokio::task::spawn_blocking(move || {
            transfer.ready_and_wait().unwrap();
            transfer.finish(Err("owner completion failed; outcome unknown".into()));
        });
        let ready = ready.await.unwrap();
        assert_eq!(
            ready.complete(Ok(())).await,
            Err("owner completion failed; outcome unknown".into())
        );
        worker.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn owner_interruption_after_readiness_never_reports_success() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut transfer, ready) = NativeTaskAdmissionTransfer::channel();
        let worker = tokio::task::spawn_blocking(move || {
            transfer.ready_and_wait().unwrap();
            drop(transfer);
        });
        let ready = ready.await.unwrap();
        assert!(
            ready
                .complete(Ok(()))
                .await
                .unwrap_err()
                .contains("unknown")
        );
        worker.await.unwrap();
    })
    .await
    .unwrap();
}

//! Registered inbox waits through the authenticated native child dispatcher.
use super::*;

fn inbox_permission(f: &Fixture) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    put_grant(
        f,
        Grant::new(
            "native-inbox-read",
            Subject::agent("scribe"),
            TreePath::room(room().to_string()).app("so.tilo.desktop"),
            KindSet::READ,
            Answer::Allow,
            Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
}

async fn wait_request(
    f: &Fixture,
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: uuid::Uuid,
    target: (&str, u64),
    timeout_ms: u32,
    hold_ms: u32,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    wait_request_after(f, channel, run, invocation, target, timeout_ms, hold_ms, 17).await
}

async fn wait_request_after(
    f: &Fixture,
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: uuid::Uuid,
    target: (&str, u64),
    timeout_ms: u32,
    hold_ms: u32,
    after_sequence: u64,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let peer = f.state.workspace_membership_peers.lock().unwrap()[&room().to_string()];
    let mut request = channel.invoke_task_action_request();
    let mut p = request.get();
    p.set_run_id(run.run_id);
    p.set_lease(&run.lease);
    p.set_peer_id(peer);
    p.set_resource(format!("desktop:{}", target.0));
    p.set_action("wait-task-message");
    p.set_invocation(invocation.to_string());
    p.set_has_argument(true);
    p.set_argument(
        serde_json::json!({
            "run_epoch":target.1,"after_sequence":after_sequence,"timeout_ms":timeout_ms,
        })
        .to_string(),
    );
    p.set_wait_ms(hold_ms);
    p.set_seen_json(serde_json::to_string(&tilo_actions::remote::UpdateMark::Accepted).unwrap());
    let response = request.send().promise.await?;
    let reply = response.get()?;
    Ok((
        reply.get_id(),
        serde_json::from_str(reply.get_update_json()?.to_str().unwrap()).unwrap(),
    ))
}

#[tokio::test(flavor = "current_thread")]
async fn registered_native_wait_retains_identity_until_bounded_empty_owner_result() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            inbox_permission(&f);
            f.state.refresh_workspace_membership_services().unwrap();
            let channel = f.channel(100, true);
            let run = delivered(&channel).await;
            let invocation = uuid::Uuid::from_u128(8801);
            let (wire, update) = wait_request(&f, &channel, &run, invocation, (&f.task, 1), 200, 0)
                .await
                .unwrap();
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            assert!(
                f.state
                    .pending_invocations
                    .lock()
                    .unwrap()
                    .contains_key(&wire)
            );
            let (same_wire, terminal) = tokio::time::timeout(
                Duration::from_secs(2),
                wait_request(&f, &channel, &run, invocation, (&f.task, 1), 200, 1500),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(same_wire, wire);
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = terminal else {
                panic!("owner-observed empty inbox must finish successfully");
            };
            let page: tilo_actions::task_messaging::TaskMessageWaitResult =
                serde_json::from_value(output.value.unwrap()).unwrap();
            assert_eq!(page.task_run.task_id, f.task);
            assert_eq!(page.task_run.run_epoch, 1);
            assert_eq!(page.task_run.agent_id, "scribe");
            assert_eq!(page.next_sequence, 17);
            assert!(page.messages.is_empty());
            assert!(page.timed_out);
            assert!(!page.has_more);
            assert!(
                !f.state
                    .pending_invocations
                    .lock()
                    .unwrap()
                    .contains_key(&wire)
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn registered_wait_refuses_another_epoch_without_inbox_contents() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            inbox_permission(&f);
            f.state.refresh_workspace_membership_services().unwrap();
            let channel = f.channel(100, true);
            let run = delivered(&channel).await;
            let invocation = uuid::Uuid::from_u128(8802);
            wait_request(&f, &channel, &run, invocation, (&f.task, 2), 200, 0)
                .await
                .unwrap();
            let (_, terminal) = tokio::time::timeout(
                Duration::from_secs(2),
                wait_request(&f, &channel, &run, invocation, (&f.task, 2), 200, 1500),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(matches!(
                terminal,
                tilo_actions::InvocationUpdate::Failed { .. }
            ));
            assert!(
                !serde_json::to_string(&terminal)
                    .unwrap()
                    .contains("messages")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn person_action_door_cannot_obtain_a_native_task_inbox() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let result = compute::invoke_hook(
                &f,
                &format!("desktop:{}", f.task),
                "wait-task-message",
                Some(serde_json::json!({"run_epoch":1,"after_sequence":0,"timeout_ms":100})),
            )
            .await;
            assert!(matches!(
                result,
                tilo_actions::InvocationUpdate::Failed { .. }
            ));
            assert!(!serde_json::to_string(&result).unwrap().contains("messages"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_owner_private_mail_wakes_registered_wait_without_broker_refresh() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use tilo_actions::task_messaging::{TaskMessageSend, TaskRunAddress};
            let f = Fixture::new();
            f.approve();
            inbox_permission(&f);
            f.state.refresh_workspace_membership_services().unwrap();
            let channel = f.channel(100, true);
            let run = delivered(&channel).await;
            let invocation = uuid::Uuid::from_u128(8810);
            let (wire, _) =
                wait_request_after(&f, &channel, &run, invocation, (&f.task, 1), 30_000, 0, 0)
                    .await
                    .unwrap();
            tokio::time::timeout(Duration::from_secs(2), f.native.inbox_observed.notified())
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(150),
                    f.native.inbox_observed.notified(),
                )
                .await
                .is_err(),
                "an unchanged owner must not be sampled"
            );
            let owner_changes = f.native.owner.workspace_task_changes();
            // Publish directly at the owner, not through execute_task_message:
            // only the post-commit owner watch can wake this retained child.
            f.native
                .owner
                .send_task_message(
                    room(),
                    TaskRunAddress {
                        task_id: f.task.clone(),
                        run_epoch: 1,
                        agent_id: "scribe".into(),
                    },
                    &f.task,
                    uuid::Uuid::from_u128(8901),
                    TaskMessageSend {
                        run_epoch: 1,
                        text: "private owner context".into(),
                    },
                    |_| Ok(()),
                )
                .unwrap();
            assert!(owner_changes.has_changed().unwrap());
            let (same_wire, terminal) = tokio::time::timeout(
                Duration::from_secs(2),
                wait_request_after(
                    &f,
                    &channel,
                    &run,
                    invocation,
                    (&f.task, 1),
                    30_000,
                    1500,
                    0,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(same_wire, wire);
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = terminal else {
                panic!("accepted owner mail must finish the pending wait");
            };
            let page: tilo_actions::task_messaging::TaskMessageWaitResult =
                serde_json::from_value(output.value.unwrap()).unwrap();
            assert_eq!(page.messages.len(), 1);
            assert_eq!(page.messages[0].sequence, 1);
            assert!(!page.timed_out);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn room_stop_is_not_blocked_by_a_registered_private_inbox_wait() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            inbox_permission(&f);
            f.state.refresh_workspace_membership_services().unwrap();
            let channel = f.channel(100, true);
            let run = delivered(&channel).await;
            let invocation = uuid::Uuid::from_u128(8803);
            let (wire, _) = wait_request(&f, &channel, &run, invocation, (&f.task, 1), 30_000, 0)
                .await
                .unwrap();
            assert!(
                f.state
                    .pending_invocations
                    .lock()
                    .unwrap()
                    .contains_key(&wire)
            );
            let stopped = tokio::time::timeout(
                Duration::from_secs(2),
                compute::invoke_hook(
                    &f,
                    &format!("desktop:{}", f.task),
                    "stop",
                    Some(serde_json::json!({"run_epoch":1,"after_revision":0})),
                ),
            )
            .await
            .unwrap();
            assert!(matches!(
                stopped,
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
            // The wait itself detects the stopped owner run; no model request or
            // model delivery acknowledgement is needed to retire its child.
            let mut changes = f.state.invocation_changes();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(
                        f.state.invocation_status(wire),
                        Some(tilo_actions::InvocationUpdate::Cancelled { .. })
                    ) {
                        break;
                    }
                    changes.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retained_native_wait_does_not_restart_elapsed_invocation_timeout() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            inbox_permission(&f);
            f.state.refresh_workspace_membership_services().unwrap();
            let channel = f.channel(100, true);
            let run = delivered(&channel).await;
            let invocation = uuid::Uuid::from_u128(8812);
            let (wire, _) = wait_request(&f, &channel, &run, invocation, (&f.task, 1), 30_000, 0)
                .await
                .unwrap();
            let mut pending = f.state.pending_invocations.lock().unwrap()[&wire].clone();
            // Model a retained child whose original admission is already old.
            // Keep its real caller, exact requirement and durable record intact.
            pending.last_activity = std::time::Instant::now() - Duration::from_secs(31);
            let envelope = InvocationEnvelope {
                id: wire,
                invocation: tilo_actions::InvocationId(invocation),
                resource: format!("desktop:{}", f.task),
                action: "wait-task-message".into(),
                argument: Some(
                    serde_json::json!({
                        "run_epoch":1,"after_sequence":17,"timeout_ms":30_000,
                    })
                    .to_string(),
                ),
                actor: "scribe".into(),
                context: None,
            };
            let result = tokio::time::timeout(
                Duration::from_millis(250),
                f.state
                    .execute_task_message_wait(&room().to_string(), &envelope, &pending),
            )
            .await
            .expect("a retained invocation must not receive a fresh thirty seconds");
            match result {
                Err(error) => assert_eq!(error, "task inbox owner observation deadline exceeded"),
                Ok(Some(value)) => {
                    let page: tilo_actions::task_messaging::TaskMessageWaitResult =
                        serde_json::from_value(value).unwrap();
                    assert!(page.timed_out);
                    assert!(page.messages.is_empty());
                }
                Ok(None) => panic!("the registered mailbox read must be handled"),
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn registered_wait_is_capped_by_owner_run_budget_across_reasks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.person_command(WorkspaceTaskCommand::SetExecutionBudget {
                task_id: f.task.clone(),
                expected_run_epoch: 0,
                expected_budget: None,
                budget: Some(collab_types::task_budget::TaskExecutionBudget {
                    turn_ms: 1200,
                    total_ms: 1200,
                }),
            })
            .unwrap();
            f.approve();
            inbox_permission(&f);
            f.state.refresh_workspace_membership_services().unwrap();
            let channel = f.channel(100, true);
            let run = delivered(&channel).await;
            let invocation = uuid::Uuid::from_u128(8811);
            let (wire, _) = wait_request(&f, &channel, &run, invocation, (&f.task, 1), 30_000, 0)
                .await
                .unwrap();
            let (same_wire, terminal) = tokio::time::timeout(
                Duration::from_secs(2),
                wait_request(&f, &channel, &run, invocation, (&f.task, 1), 30_000, 1900),
            )
            .await
            .expect("owner budget must cap the thirty-second inbox wait")
            .unwrap();
            assert_eq!(same_wire, wire);
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = terminal else {
                panic!("an owner-observed empty inbox must time out within the run budget");
            };
            let page: tilo_actions::task_messaging::TaskMessageWaitResult =
                serde_json::from_value(output.value.unwrap()).unwrap();
            assert!(page.messages.is_empty());
            assert!(page.timed_out);
            let (retained_wire, retained) =
                wait_request(&f, &channel, &run, invocation, (&f.task, 1), 30_000, 0)
                    .await
                    .unwrap();
            assert_eq!(retained_wire, wire);
            assert!(retained.is_terminal(), "a re-ask must not restart the wait");
        })
        .await;
}

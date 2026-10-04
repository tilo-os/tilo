//! Synthetic execution fixtures: no model, transport grants, UI input or capture.
use super::*;
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use tilo_actions::remote::{
    Caller, CatalogueQuery, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
};

fn fixture() -> (
    Arc<CapnpBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    let mut workspace = Workspace::new(WorkspaceId(52), "Synthetic execution presence");
    workspace.set_owner("a1".repeat(32));
    let room = workspace.room.to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let broker =
        CapnpBrokerState::test_broker_with_authority(membership.authority(), membership.services());
    let state = Arc::clone(broker.state());
    state.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.test".into(),
        route: CrdtRoute {
            app_id: "so.tilo.test".into(),
            cluster_id: room.clone(),
            record_id: "record-a".into(),
        },
        actions: vec![PublishedAction {
            resource: "test:block-a".into(),
            action: "update".into(),
            resource_kind: "test.block".into(),
            resource_label: "Block".into(),
            action_label: "Update".into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..Default::default()
            },
            input: PublishedInput::None,
            view: None,
        }],
        live: true,
    }]);
    let receiver = state.capture_invocations_for_test(7);
    let caller = Caller::local_person("a1".repeat(32))
        .in_room(room)
        .in_new_execution();
    (state, caller, receiver)
}

fn request() -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: "7".into(),
            address: tilo_actions::ActionAddress::new("test:block-a", "update"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
    }
}

#[tokio::test]
async fn only_admitted_execution_and_validated_terminal_emit_activity() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_query(&caller, &CatalogueQuery::default())
        .poll_ready();
    assert!(events.try_recv().is_err(), "resource discovery is not work");
    let request = request();
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &request)
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let delivered = app.try_recv().unwrap();
    let accepted = events.try_recv().unwrap();
    assert_eq!(accepted.execution, caller.execution_id().unwrap());
    assert_eq!(accepted.invocation, request.invocation);
    assert_eq!(accepted.principal, "a1".repeat(32));
    assert_eq!(accepted.peer_id, 7);
    assert_eq!(accepted.route.record_id, "record-a");
    assert_eq!(accepted.address, request.target.address);
    assert_eq!(accepted.status, ExecutionStatus::Accepted);
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &request)
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert!(
        events.try_recv().is_err(),
        "a receipt retry is not a new operation"
    );
    assert!(app.try_recv().is_err());
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Failed {
                invocation: delivered.invocation,
                error: tilo_actions::ActionFailure {
                    code: "synthetic_failure".into(),
                    message: "Synthetic failure".into(),
                    details: None,
                    retryable: false,
                },
            },
            None,
        )
        .await
        .unwrap();
    let failed = events.try_recv().unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    assert_eq!(failed.invocation, accepted.invocation);
    assert_eq!(failed.execution, accepted.execution);
    assert!(failed.sequence > accepted.sequence);
    assert!(accepted.source_time_ms > 0);
    assert!(failed.source_time_ms > 0);
    assert!(events.try_recv().is_err());
}

#[test]
fn refused_actions_and_observer_disconnect_do_not_schedule_work() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let mut refused = request();
    refused.target.provider = "999".into();
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &refused)
            .poll_ready(),
        PendingPoll::Ready(Err(_))
    ));
    assert!(events.try_recv().is_err());
    assert!(app.try_recv().is_err());
    drop(events);
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &request())
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert!(
        app.try_recv().is_ok(),
        "dropping an observer must not end the broker"
    );
}

#[tokio::test]
async fn wrong_peer_stale_progress_and_post_terminal_updates_emit_nothing() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let request = request();
    let _ = state
        .action_invoke(&caller, None, None, &request)
        .poll_ready();
    let delivered = app.try_recv().unwrap();
    events.try_recv().unwrap();
    let progress = tilo_actions::InvocationUpdate::Progress {
        invocation: delivered.invocation,
        sequence: 2,
        phase: "Synthetic operation".into(),
        completed: Some(1),
        total: Some(2),
        message: None,
    };
    assert!(
        state
            .apply_invocation_update(8, delivered.id, progress.clone(), None)
            .await
            .is_err()
    );
    assert!(events.try_recv().is_err());
    state
        .apply_invocation_update(7, delivered.id, progress.clone(), None)
        .await
        .unwrap();
    assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Progress);
    assert!(
        state
            .apply_invocation_update(7, delivered.id, progress.clone(), None)
            .await
            .is_err()
    );
    assert!(events.try_recv().is_err());
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Cancelled {
                invocation: delivered.invocation,
                message: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::Cancelled
    );
    assert!(
        state
            .apply_invocation_update(7, delivered.id, progress, None)
            .await
            .is_err()
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn actions_without_a_host_minted_run_do_not_invent_execution_presence() {
    let (state, caller, mut app) = fixture();
    let ordinary = Caller::local_person("a1".repeat(32)).in_room(caller.room().unwrap());
    let mut events = state.subscribe_execution_activity();
    assert!(matches!(
        state
            .action_invoke(&ordinary, None, None, &request())
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert!(app.try_recv().is_ok());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn eighty_synthetic_runs_keep_exact_record_and_invocation_identity() {
    let (state, caller, mut first_app) = fixture();
    let mut surfaces = state.published_surfaces();
    let mut second = surfaces[0].clone();
    second.peer_id = 8;
    second.route.record_id = "record-b".into();
    surfaces.push(second);
    state.replace_published_surfaces_for_test(surfaces);
    let mut second_app = state.capture_invocations_for_test(8);
    let mut events = state.subscribe_execution_activity();
    let mut pending = Vec::new();
    let mut runs = std::collections::HashSet::new();
    for index in 0..80 {
        let run = caller.clone().in_new_execution();
        assert!(runs.insert(run.execution_id().unwrap()));
        let mut request = request();
        let peer = if index % 2 == 0 { 7 } else { 8 };
        request.target.provider = peer.to_string();
        assert!(matches!(
            state.action_invoke(&run, None, None, &request).poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        let delivered = if peer == 7 {
            first_app.try_recv().unwrap()
        } else {
            second_app.try_recv().unwrap()
        };
        let admitted = events.try_recv().unwrap();
        assert_eq!(admitted.execution, run.execution_id().unwrap());
        assert_eq!(admitted.peer_id, peer);
        assert_eq!(
            admitted.route.record_id,
            if peer == 7 { "record-a" } else { "record-b" }
        );
        assert_eq!(admitted.invocation, request.invocation);
        pending.push((peer, delivered, admitted));
    }
    assert_eq!(
        state.pending_invocations.lock().unwrap().len(),
        80,
        "all eighty synthetic runs are pending together"
    );
    for (peer, delivered, admitted) in pending {
        state
            .apply_invocation_update(
                peer,
                delivered.id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivered.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                None,
            )
            .await
            .unwrap();
        let done = events.try_recv().unwrap();
        assert_eq!(done.status, ExecutionStatus::Succeeded);
        assert_eq!(done.execution, admitted.execution);
        assert_eq!(done.invocation, admitted.invocation);
        assert_eq!(done.route, admitted.route);
    }
    assert_eq!(runs.len(), 80);
    assert!(state.pending_invocations.lock().unwrap().is_empty());
    assert!(events.try_recv().is_err());
}

#[test]
fn observer_backpressure_is_bounded_and_reports_loss_without_replaying_actions() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    for _ in 0..300 {
        assert!(matches!(
            state
                .action_invoke(&caller, None, None, &request())
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        assert!(app.try_recv().is_ok());
    }
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(44))
    ));
    let first = events.try_recv().unwrap();
    assert_eq!(first.sequence, 45);
    let mut count = 1;
    while events.try_recv().is_ok() {
        count += 1;
    }
    assert_eq!(count, 256);
    assert!(
        app.try_recv().is_err(),
        "observation does not execute or replay actions"
    );
}

#[test]
fn disconnect_and_silence_produce_terminal_observations_and_new_brokers_have_new_epochs() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    app.try_recv().unwrap();
    let first = events.try_recv().unwrap();
    state.expire_stale_invocations_at(std::time::Instant::now(), Duration::ZERO, Duration::ZERO);
    assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Failed);
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    app.try_recv().unwrap();
    events.try_recv().unwrap();
    state.remove_connection(7);
    assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Failed);
    assert!(state.pending_invocations.lock().unwrap().is_empty());
    let (other, caller, mut app) = fixture();
    let mut other_events = other.subscribe_execution_activity();
    let _ = other
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    app.try_recv().unwrap();
    assert_ne!(other_events.try_recv().unwrap().epoch, first.epoch);
}

#[test]
fn closed_application_stream_never_emits_acceptance() {
    let (state, caller, app) = fixture();
    let mut events = state.subscribe_execution_activity();
    drop(app);
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &request())
            .poll_ready(),
        PendingPoll::Ready(Err(_))
    ));
    assert!(events.try_recv().is_err());
    assert!(state.pending_invocations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_is_a_request_until_the_application_confirms_it() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let request = request();
    let _ = state
        .action_invoke(&caller, None, None, &request)
        .poll_ready();
    let delivered = app.try_recv().unwrap();
    events.try_recv().unwrap();
    assert!(state.action_cancel(&caller, &request.invocation).is_ok());
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::CancellationRequested
    );
    assert!(state.action_cancel(&caller, &request.invocation).is_ok());
    assert!(
        events.try_recv().is_err(),
        "a repeated cancel does not create another activity"
    );
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Progress {
                invocation: delivered.invocation,
                sequence: 1,
                phase: "Finishing".into(),
                completed: None,
                total: None,
                message: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::CancellationRequested
    );
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Cancelled {
                invocation: delivered.invocation,
                message: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::Cancelled
    );
}

#[test]
fn broker_shutdown_closes_existing_and_later_observers_with_state_retained() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    app.try_recv().unwrap();
    assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Accepted);
    state.stop();
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Closed)
    ));
    assert!(matches!(
        state.subscribe_execution_activity().try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Closed)
    ));
    state.stop();
}

#[tokio::test]
async fn failed_terminal_journal_barrier_cannot_publish_success() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    let delivered = app.try_recv().unwrap();
    assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Accepted);
    let root = tempfile::tempdir().unwrap();
    let mut journal = InvocationJournal::open(&root.path().join("journal")).unwrap();
    {
        let mut registry = state.pending_invocations.lock().unwrap();
        let pending = registry.get_mut(&delivered.id).unwrap();
        let key = InvocationKey {
            room: pending.cluster_id.clone(),
            host: journal.host(),
            invocation: delivered.invocation,
            owner: pending.owner.clone(),
        };
        journal
            .admit(key.clone(), serde_json::json!({"synthetic":true}), 1)
            .unwrap();
        pending.journal_key = Some(key);
        drop(registry);
    }
    journal.fail_at = Some(invocation_journal::BarrierFailure::File);
    *state.journal.lock().unwrap() = Ok(Some(journal));
    assert!(
        state
            .apply_invocation_update(
                7,
                delivered.id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivered.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                None
            )
            .await
            .is_err()
    );
    assert!(
        events.try_recv().is_err(),
        "uncommitted success must never escape"
    );
    assert!(matches!(
        state.pending_invocations.lock().unwrap()[&delivered.id].update,
        tilo_actions::InvocationUpdate::Accepted { .. }
    ));
    state.stop();
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Closed)
    ));
}

#[tokio::test]
async fn retiring_a_run_clears_only_its_presence_without_cancelling_work() {
    let (state, caller, mut app) = fixture();
    let other = caller.clone().in_new_execution();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    let first = app.try_recv().unwrap();
    let buffered = events.try_recv().unwrap();
    assert!(buffered.execution_is_live());
    let _ = state
        .action_invoke(&other, None, None, &request())
        .poll_ready();
    let second = app.try_recv().unwrap();
    events.try_recv().unwrap();
    state.retire_execution(&caller);
    let ended = events.try_recv().unwrap();
    assert_eq!(ended.status, ExecutionStatus::Ended);
    assert_eq!(ended.execution, caller.execution_id().unwrap());
    assert!(!caller.execution_is_live());
    assert!(!buffered.execution_is_live());
    assert!(other.execution_is_live());
    assert_eq!(state.pending_invocations.lock().unwrap().len(), 2);
    for (delivery, visible) in [(first, false), (second, true)] {
        state
            .apply_invocation_update(
                7,
                delivery.id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivery.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(events.try_recv().is_ok(), visible);
    }
    state.retire_execution(&caller);
    assert!(events.try_recv().is_err());
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    assert!(
        events.try_recv().is_err(),
        "a retained caller cannot revive presence"
    );
}

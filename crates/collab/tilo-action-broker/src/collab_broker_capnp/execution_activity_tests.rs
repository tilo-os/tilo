//! Synthetic execution fixtures: no model, transport grants, UI input or capture.
use super::*;
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, PendingPoll, RemoteActionTarget,
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
            semantic_identity: None,
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
    // This fixture consumes the full InvocationEnvelope UUID, like a stable subscriber.
    state
        .stable_invocation_subscribers
        .lock()
        .unwrap()
        .insert(7);
    let caller = Caller::local_person("a1".repeat(32))
        .in_room(room)
        .in_new_execution();
    (state, caller, receiver)
}

fn request() -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: "7".into(),
            address: tilo_actions::ActionAddress::new("test:block-a", "update"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        fresh_binding: None,
        wait_ms: None,
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
fn terminal_update_keeps_status_visible_during_the_pending_to_completed_handoff() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let request = request();
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &request)
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let delivered = app.try_recv().expect("directed invocation");
    let job = delivered.id;
    let _accepted = events.try_recv().expect("accepted activity");
    let terminal = tilo_actions::InvocationUpdate::Succeeded {
        invocation: delivered.invocation,
        output: tilo_actions::ActionOutput::default(),
    };

    // Stall retained-completion insertion. The production terminal updater
    // must continue holding pending custody until that insertion succeeds.
    let completed = state
        .completed_invocations
        .lock()
        .expect("completed registry");
    let updating = Arc::clone(&state);
    let update = terminal.clone();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("terminal update runtime")
            .block_on(updating.apply_invocation_update(7, job, update, None))
    });
    let activity_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let activity = loop {
        if let Ok(activity) = events.try_recv() {
            break activity;
        }
        if std::time::Instant::now() >= activity_deadline {
            drop(completed);
            let update = worker.join().expect("terminal update thread");
            panic!("terminal activity was not published; updater returned {update:?}");
        }
        std::thread::yield_now();
    };
    if activity.status != ExecutionStatus::Succeeded {
        drop(completed);
        let update = worker.join().expect("terminal update thread");
        panic!(
            "terminal activity had status {:?}; updater returned {update:?}",
            activity.status
        );
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    let mut pending_unlocked = false;
    while std::time::Instant::now() < deadline {
        if state.pending_invocations.try_lock().is_ok() {
            pending_unlocked = true;
            break;
        }
        std::thread::yield_now();
    }
    if pending_unlocked {
        drop(completed);
        let update = worker.join().expect("terminal update thread");
        panic!("terminal status vanished before retained completion; updater returned {update:?}");
    }
    let observing = Arc::clone(&state);
    let observation = std::thread::spawn(move || observing.invocation_status(job));
    drop(completed);
    worker
        .join()
        .expect("terminal update thread")
        .expect("terminal update");
    assert_eq!(observation.join().expect("status observer"), Some(terminal));
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
    state
        .stable_invocation_subscribers
        .lock()
        .unwrap()
        .insert(8);
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

/// A workspace whose agent `scribe` may write to the test app but is denied
/// its `erase` action, with a second writing person `b2…`, and the app
/// publishing `update`, `edit` and `erase`.
fn continuation_fixture() -> (
    Arc<CapnpBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    use collab_types::{CapOverride, Effect, Scope, Subject, Verb};
    let mut workspace = Workspace::new(WorkspaceId(53), "Effect continuation");
    workspace.set_owner("a1".repeat(32));
    let room = workspace.room.to_string();
    workspace.grant_capability(CapOverride::membership(
        Subject::agent("scribe"),
        room.clone(),
        Verb::Write,
    ));
    workspace.grant_capability(CapOverride::grant(
        Subject::agent("scribe"),
        Scope::App("so.tilo.test".into()),
        Verb::Write,
    ));
    workspace.grant_capability(CapOverride::membership(
        Subject::person("b2".repeat(32)),
        room.clone(),
        Verb::Write,
    ));
    workspace.grant_capability(CapOverride::new(
        Subject::agent("scribe"),
        Scope::Action {
            app: "so.tilo.test".into(),
            resource: "test:block-a".into(),
            action: "erase".into(),
        },
        Effect::Restrict,
        Verb::Read,
    ));
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let broker =
        CapnpBrokerState::test_broker_with_authority(membership.authority(), membership.services());
    let state = Arc::clone(broker.state());
    let action = |action: &str, kind: tilo_actions::OperationKind| PublishedAction {
        semantic_identity: None,
        resource: "test:block-a".into(),
        action: action.into(),
        resource_kind: "test.block".into(),
        resource_label: "Block".into(),
        action_label: action.into(),
        description: None,
        operation_kind: kind,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        },
        input: PublishedInput::None,
        view: None,
    };
    state.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.test".into(),
        route: CrdtRoute {
            app_id: "so.tilo.test".into(),
            cluster_id: room.clone(),
            record_id: "record-a".into(),
        },
        actions: vec![
            action("update", tilo_actions::OperationKind::Effect),
            action("edit", tilo_actions::OperationKind::Effect),
            action("erase", tilo_actions::OperationKind::Effect),
        ],
        live: true,
    }]);
    let app = state.capture_invocations_for_test(7);
    let agent = Caller::local_agent("scribe").in_room(room);
    (state, agent, app)
}

/// An edit an effect submits while running for an agent is admitted as
/// that agent, with their grants: an action their role allows but a grant
/// denies them stays denied, through the effect as everywhere.
#[tokio::test]
async fn an_effects_edit_is_admitted_for_its_invoker_with_their_grants() {
    let (state, agent, mut app) = continuation_fixture();
    let invoke = |action: &str| InvokeRequest {
        target: RemoteActionTarget {
            provider: "7".into(),
            address: tilo_actions::ActionAddress::new("test:block-a", action),
        },
        ..request()
    };

    assert!(matches!(
        state
            .action_invoke(&agent, None, None, &invoke("update"))
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let running = app.try_recv().unwrap();
    assert_eq!(running.actor, "agent:scribe");

    // The app's connection names the effect it runs; nobody else's does.
    let continued = state
        .continuing_caller(7, running.invocation)
        .expect("the running effect's caller");
    assert_eq!(continued, agent);
    assert!(state.continuing_caller(8, running.invocation).is_none());

    // What the role allows is admitted for the agent; what a grant denies
    // them is refused.
    assert!(matches!(
        state
            .action_invoke(&continued, Some(7), None, &invoke("edit"))
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert_eq!(app.try_recv().unwrap().actor, "agent:scribe");
    let refused = state
        .action_invoke(&continued, Some(7), None, &invoke("erase"))
        .poll_ready();
    // An action a grant denies is absent to the caller, as in the catalogue.
    assert!(
        matches!(
            refused,
            PendingPoll::Ready(Err(tilo_actions::remote::ActionHostError::UnknownAction))
        ),
        "the denied edit was refused"
    );
    assert!(app.try_recv().is_err(), "the denied edit reaches no app");
    let owner = Caller::local_person("a1".repeat(32)).in_room(continued.room().unwrap().to_owned());
    assert!(matches!(
        state
            .action_invoke(&owner, Some(7), None, &invoke("erase"))
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert!(app.try_recv().is_ok(), "the action itself is offered");

    // Once the effect ended it continues nothing.
    state
        .apply_invocation_update(
            7,
            running.id,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: running.invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            None,
        )
        .await
        .unwrap();
    assert!(state.continuing_caller(7, running.invocation).is_none());
}

/// Doors that take the caller's own invocation id can admit two callers'
/// invocations under one id on the same app. An edit submitted on behalf of
/// that id names no single caller, so it is admitted for neither, never for
/// whichever one the registry happens to find first.
#[test]
fn an_invocation_id_two_callers_share_continues_neither() {
    let (state, agent, _app) = continuation_fixture();
    let room = agent.room().unwrap().to_owned();
    let other = "b2".repeat(32);
    state
        .stable_invocation_subscribers
        .lock()
        .unwrap()
        .insert(7);
    let shared = tilo_actions::InvocationId::new();
    let start = |person: &str| {
        let caller = Caller::local_person(person)
            .in_room(room.clone())
            .in_new_execution();
        state
            .start_invocation_confirmed_for_origin(
                7,
                "test:block-a",
                "update",
                InvocationInputs {
                    requested: Some(shared),
                    caller: Some(&caller),
                    execution: ExecutionScope::for_call(&caller, shared),
                    managed_interaction: None,
                    argument: None,
                    actor: &BrokerActor::person(person),
                    confirmation_token: None,
                    context: None,
                },
                None,
            )
            .unwrap();
        caller
    };
    let first = start(&"a1".repeat(32));
    assert_eq!(state.continuing_caller(7, shared), Some(first));
    start(&other);
    assert!(
        state.continuing_caller(7, shared).is_none(),
        "an ambiguous invocation continues for nobody"
    );
}

#[test]
fn same_principal_runs_receive_distinct_host_owned_participant_views() {
    let (state, first, mut app) = fixture();
    let second = first.clone().in_new_execution();
    let forged = tilo_actions::ParticipantId::new("person-window-forged").unwrap();
    let mut participants = Vec::new();
    for caller in [&first, &second, &first] {
        let mut request = request();
        request.context = Some(tilo_actions::ActionContext::default().with_invoker(
            tilo_actions::ActionInvoker::actor("a1".repeat(32)).with_participant(forged.clone()),
        ));
        assert!(matches!(
            state
                .action_invoke(caller, None, None, &request)
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        let delivery = app.try_recv().unwrap();
        let invoker =
            tilo_actions::ActionInvoker::delivered(&delivery.actor, delivery.context.as_ref());
        assert_eq!(invoker.actor, "a1".repeat(32));
        let participant = invoker.participant.expect("a run owns a participant view");
        assert_ne!(participant, forged);
        participants.push(participant);
    }
    assert_ne!(participants[0], participants[1]);
    assert_eq!(participants[0], participants[2]);
    first.retire_execution();
    assert!(matches!(
        state
            .action_invoke(&first, None, None, &request())
            .poll_ready(),
        PendingPoll::Ready(Err(_))
    ));
    assert!(
        app.try_recv().is_err(),
        "closed run cannot fall back to the person's view"
    );
}

#[test]
fn retired_execution_cannot_query_observe_or_cancel_but_legacy_callers_can() {
    let (state, caller, mut app) = fixture();
    let scoped_request = request();
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &scoped_request)
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let delivered = app.try_recv().unwrap();
    assert!(
        state
            .action_observe(&caller, &scoped_request.invocation)
            .is_some()
    );
    assert!(matches!(
        state
            .action_query(&caller, &CatalogueQuery::default())
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let retained = caller.clone();
    caller.retire_execution();
    assert!(matches!(
        state
            .action_query(&retained, &CatalogueQuery::default())
            .poll_ready(),
        PendingPoll::Ready(Err(_))
    ));
    assert!(
        state
            .action_observe(&retained, &scoped_request.invocation)
            .is_none()
    );
    assert!(
        state
            .action_cancel(&retained, &scoped_request.invocation)
            .is_err()
    );
    assert!(
        state
            .remote_job(&retained, &scoped_request.invocation)
            .is_none()
    );
    assert!(
        state.pending_invocations.lock().unwrap()[&delivered.id]
            .cancellation_requested_at
            .is_none(),
        "retirement and refused cancellation must not cancel an admitted job"
    );

    let legacy = Caller::local_person("a1".repeat(32)).in_room(caller.room().unwrap());
    legacy.retire_execution();
    assert!(matches!(
        state
            .action_query(&legacy, &CatalogueQuery::default())
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let legacy_request = request();
    assert!(matches!(
        state
            .action_invoke(&legacy, None, None, &legacy_request)
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    app.try_recv().unwrap();
    assert!(
        state
            .action_observe(&legacy, &legacy_request.invocation)
            .is_some()
    );
    assert!(
        state
            .action_cancel(&legacy, &legacy_request.invocation)
            .is_ok()
    );
}

/// The broker's door, counting how often a shortcut run observes through it.
struct CountedDoor {
    door: action_host::BrokerDoor,
    observes: Arc<std::sync::atomic::AtomicUsize>,
}

impl ActionHost for CountedDoor {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.door.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.door.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.observes.fetch_add(1, Ordering::Relaxed);
        self.door.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.door.next_change(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        self.door.cancel(caller, id)
    }
}

/// Hooks for a run whose host says when a step changes, so nothing may fall
/// back to polling it.
struct NoPolling(crate::automation::DoorHooks);

impl tilo_automation::RunHooks for NoPolling {
    fn confirm(
        &self,
        request: tilo_automation::ConfirmationRequest,
    ) -> Pending<Option<tilo_actions::remote::Confirmation>> {
        self.0.confirm(request)
    }

    fn ask(&self, request: tilo_automation::AskRequest) -> Pending<Option<serde_json::Value>> {
        self.0.ask(request)
    }

    fn settle(&self, invocation: tilo_actions::InvocationId) -> Pending<()> {
        panic!("the broker's door says when {invocation:?} changes; nothing polls it")
    }
}

/// A shortcut step that runs in the background is observed once per change
/// it makes, not once per poll interval for as long as it runs.
#[tokio::test]
async fn shortcut_step_observes_once_per_change() {
    const PROGRESS_UPDATES: u64 = 3;
    // The look for an earlier invocation, one wake for the admission itself,
    // then one per progress update and one for the outcome.
    const MOST_OBSERVES: usize = 6;
    let (state, caller, mut app) = fixture();
    let observes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shortcut = tilo_automation::Shortcut {
        id: "export".into(),
        name: "Export".into(),
        inputs: Vec::new(),
        steps: vec![tilo_automation::Step::new(
            "update",
            tilo_automation::StepKind::Invoke {
                app_id: "so.tilo.test".into(),
                resource: tilo_automation::Selector::Fixed("test:block-a".into()),
                action: "update".into(),
                args: std::collections::BTreeMap::new(),
            },
        )],
        output: None,
        grant: vec![collab_types::PermissionProfileRule::grant(
            collab_types::Scope::App("so.tilo.test".into()),
            collab_types::Verb::Write,
        )],
        triggers: Vec::new(),
        consents: Vec::new(),
        saved_by: None,
    };
    let hooks = NoPolling(crate::automation::DoorHooks::new(
        None,
        tokio::runtime::Handle::current(),
    ));
    let running = tokio::spawn({
        let state = Arc::clone(&state);
        let observes = Arc::clone(&observes);
        async move {
            let door = CountedDoor {
                door: action_host::BrokerDoor::new(state),
                observes,
            };
            tilo_automation::run(
                &door,
                &hooks,
                tilo_automation::RunRequest {
                    shortcut: &shortcut,
                    run: tilo_automation::RunId::new(),
                    caller,
                    mode: tilo_automation::RunMode::Unattended,
                    trigger: tilo_automation::RunTrigger::Manual,
                    inputs: serde_json::Map::new(),
                    consents: Vec::new(),
                },
            )
            .await
        }
    });

    let delivered = app.recv().await.expect("the step reaches the app");
    // Each pause is many of the old 25 ms poll intervals and well inside the
    // door's safety interval: only a change may wake the run.
    let pause = std::time::Duration::from_millis(150);
    for sequence in 1..=PROGRESS_UPDATES {
        tokio::time::sleep(pause).await;
        state
            .apply_invocation_update(
                7,
                delivered.id,
                tilo_actions::InvocationUpdate::Progress {
                    invocation: delivered.invocation,
                    sequence,
                    phase: "Exporting".into(),
                    completed: Some(sequence),
                    total: Some(PROGRESS_UPDATES),
                    message: None,
                },
                None,
            )
            .await
            .unwrap();
    }
    tokio::time::sleep(pause).await;
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: delivered.invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            None,
        )
        .await
        .unwrap();

    let report = running.await.expect("the run finishes");
    assert_eq!(
        report.outcome,
        tilo_automation::RunOutcome::Completed,
        "{report:?}"
    );
    let observes = observes.load(Ordering::Relaxed);
    assert!(
        observes <= MOST_OBSERVES,
        "{observes} observations for {PROGRESS_UPDATES} progress updates"
    );
}

pub(super) fn native_history_invocation_fixture(
    history: Vec<u8>,
) -> NativeHistoryInvocationFixture {
    let captures = Arc::new(std::sync::Mutex::new(Captures {
        committed_history_fixture: Some(history),
        invocation_fixture_enabled: true,
        invocation_execution: Some(Ok(false)),
        ..Captures::default()
    }));
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let refresh_captures = captures.clone();
    let invocation_captures = captures.clone();
    let execution_captures = captures.clone();
    let terminal_captures = captures.clone();
    let update_captures = captures.clone();
    NativeHistoryInvocationFixture {
        manager: CollaborationManager::from_sdk_fd_for_test(
            fd,
            "so.tilo.initial-history-probe",
            Uuid::new_v4(),
        ),
        refresh: Box::new(move |history, gate| {
            let mut captures = refresh_captures.lock().unwrap();
            captures.committed_history_fixture = Some(history);
            captures.committed_revision = Some([8; 32]);
            captures.committed_recheck_gate = Some(gate);
        }),
        invoke: Box::new(move |id, uuid| {
            invocation_captures
                .lock()
                .unwrap()
                .invocations
                .push_back(runtime_collab::Invocation {
                    id,
                    invocation_uuid: *uuid.as_bytes(),
                    resource: "initial-probe".into(),
                    action: "inspect".into(),
                    argument: None,
                    actor: "fixture-user".into(),
                    context_json: String::new(),
                })
        }),
        received: Box::new(move || captures.lock().unwrap().received_invocations),
        execution: Box::new(move |state| {
            execution_captures.lock().unwrap().invocation_execution = Some(state)
        }),
        terminals: Box::new(move || terminal_captures.lock().unwrap().terminal_updates),
        updates: Box::new(move || update_captures.lock().unwrap().invocation_updates.clone()),
        thread,
    }
}

pub(super) fn native_refresh_fixture(
    app: &str,
    room: Uuid,
    history: Vec<u8>,
) -> (CollaborationManager, NativeRefreshProbe) {
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(history),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, captures, _thread) = stand_up_fake_broker(broker);
    let replace = captures.clone();
    let probe = NativeRefreshProbe {
        committed_read_count: Box::new(move || captures.lock().unwrap().committed_reads.len()),
        replace_history: Box::new(move |history| {
            let mut captures = replace.lock().unwrap();
            captures.committed_history_fixture = Some(history);
            captures.committed_revision = Some([8; 32]);
        }),
    };
    (
        CollaborationManager::from_sdk_fd_for_test(fd, app, room),
        probe,
    )
}

pub(super) fn native_initial_fixture(
    app: &str,
    room: Uuid,
    history: Vec<u8>,
) -> (CollaborationManager, Box<dyn Fn() -> usize>) {
    native_initial_fixture_gated(app, room, history, None)
}

pub(super) fn native_initial_fixture_gated(
    app: &str,
    room: Uuid,
    history: Vec<u8>,
    gate: Option<tokio::sync::oneshot::Receiver<()>>,
) -> (CollaborationManager, Box<dyn Fn() -> usize>) {
    let initial = Captures {
        committed_history_fixture: Some(history),
        committed_recheck_gate: gate,
        ..Captures::default()
    };
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(initial)),
        ..FakeBroker::default()
    };
    let (fd, captures, _thread) = stand_up_fake_broker(broker);
    (
        CollaborationManager::from_sdk_fd_for_test(fd, app, room),
        Box::new(move || captures.lock().unwrap().joined.len()),
    )
}

pub(super) fn native_initial_invocation_fixture(
    app: &str,
    room: Uuid,
    history: Vec<u8>,
    refused_channels: Vec<String>,
) -> (
    CollaborationManager,
    Box<dyn Fn() -> Vec<(tilo_actions::ActionAddress, bool)>>,
) {
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(history),
            committed_open_refused_channels: refused_channels,
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, captures, _thread) = stand_up_fake_broker(broker);
    (
        CollaborationManager::from_sdk_fd_for_test(fd, app, room),
        Box::new(move || captures.lock().unwrap().own_invocations.clone()),
    )
}

pub(super) fn native_initial_parked_fixture(
    app: &str,
    room: Uuid,
    history: Vec<u8>,
    gate: tokio::sync::oneshot::Receiver<()>,
) -> (CollaborationManager, Box<dyn Fn() -> usize>) {
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(history),
            committed_recheck_gate: Some(gate),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, captures, _thread) = stand_up_fake_broker(broker);
    (
        CollaborationManager::from_sdk_fd_for_test(fd, app, room),
        Box::new(move || {
            captures
                .lock()
                .unwrap()
                .committed_reads
                .iter()
                .filter(|request| {
                    matches!(
                        request,
                        runtime_collab::committed_document::Request::Open {
                            known_revision: Some(_)
                        } | runtime_collab::committed_document::Request::OpenChanges { .. }
                    )
                })
                .count()
        }),
    )
}

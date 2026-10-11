/// Withhold a real RPC acknowledgement, rather than treating UI body equality
/// as evidence that the original document route remains healthy.
#[test]
fn terminal_bridge_failure_withdraws_reads_before_disconnected_health() {
    use automerge::transaction::Transactable as _;
    let mut graph = automerge::AutoCommit::new();
    graph
        .put(automerge::ROOT, "body", "committed before fencing")
        .unwrap();
    graph.commit();
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(graph.save()),
            committed_private_binding: Some(7),
            ..Captures::default()
        })),
        op_backlog: vec![],
        presence_backlog: vec![],
        context_backlog: vec![],
        scrollback_response: None,
    };
    let (fd, _captures, thread) = stand_up_fake_broker(broker);
    let mut manager =
        CollaborationManager::from_sdk_fd_for_test(fd, "original-app", Uuid::new_v4());
    let state = broker_state(&manager);
    let key = manager.primary_route().unwrap().route.channel_id();
    let ready = state.document_reads.delivery_generation(&key).unwrap();
    let initial = poll_until(&mut manager, |events| {
        events
            .iter()
            .any(|event| matches!(event, CollabEvent::CommittedDocument { .. }))
    });
    for event in initial {
        if let CollabEvent::CommittedDocument { read, .. } = event {
            read.applied();
        }
    }
    tilo_test_env::wait_until("the original acknowledged read to become ready", || {
        let _ = manager.poll();
        ready.load(Ordering::Acquire)
    });
    // Hold the actual delivery turn so asynchronous read-loop cleanup cannot
    // repair the observation after the terminal transition has been published.
    let semaphore = state.document_reads.delivery_turn(&key).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let turn = runtime
        .block_on(tilo_test_env::within(
            "the original committed delivery turn",
            semaphore.acquire_owned(),
        ))
        .unwrap();
    state.fail_closed("the original native reader was fenced");
    let healthy = state.is_healthy();
    let read_ready = ready.load(Ordering::Acquire);
    let publishable = state.document_reads.publishable(&key);
    drop(turn);
    drop(manager);
    thread.join().unwrap();
    assert!(
        !healthy,
        "terminal failure must withdraw collaboration health"
    );
    assert!(
        !read_ready,
        "read readiness must be withdrawn before disconnected health is visible"
    );
    assert!(
        !publishable,
        "a fenced original read must never remain publishable"
    );
}

#[test]
fn committed_capture_ack_deadline_renews_original_root_without_admitting_old_bytes() {
    held_capture_acknowledgement(AcknowledgementOutcome::Renew);
}
#[test]
fn a_second_committed_capture_deadline_closes_the_original_root() {
    held_capture_acknowledgement(AcknowledgementOutcome::RepeatedDeadline);
}
#[test]
fn committed_capture_deadline_renewal_rejects_changed_private_custody() {
    held_capture_acknowledgement(AcknowledgementOutcome::ChangedCustody);
}
#[test]
fn committed_capture_broker_refusal_never_renews_when_named_as_a_deadline() {
    held_capture_acknowledgement(AcknowledgementOutcome::BrokerRefusal);
}
#[test]
fn committed_capture_invalid_acknowledgement_never_renews() {
    held_capture_acknowledgement(AcknowledgementOutcome::InvalidReply);
}
#[derive(Clone, Copy)]
enum AcknowledgementOutcome {
    Renew,
    RepeatedDeadline,
    ChangedCustody,
    BrokerRefusal,
    InvalidReply,
}
fn held_capture_acknowledgement(outcome: AcknowledgementOutcome) {
    use automerge::transaction::Transactable as _;
    let mut graph = automerge::AutoCommit::new();
    graph
        .put(automerge::ROOT, "body", "before the held acknowledgement")
        .unwrap();
    graph.commit();
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(graph.save()),
            committed_private_binding: Some(7),
            ..Captures::default()
        })),
        op_backlog: vec![],
        presence_backlog: vec![],
        context_backlog: vec![],
        scrollback_response: None,
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let mut manager =
        CollaborationManager::from_sdk_fd_for_test(fd, "original-app", Uuid::new_v4());
    let state = broker_state(&manager);
    let key = manager.primary_route().unwrap().route.channel_id();
    let original = state.document_reads.delivery_generation(&key).unwrap();
    let initial = poll_until(&mut manager, |events| {
        events
            .iter()
            .any(|event| matches!(event, CollabEvent::CommittedDocument { .. }))
    });
    for event in initial {
        if let CollabEvent::CommittedDocument { read, .. } = event {
            read.with_mutation(|mutation| {
                assert!(matches!(
                    mutation,
                    collab_types::DocumentMutation::Snapshot(_)
                ))
            })
            .unwrap();
            read.applied();
        }
    }
    tilo_test_env::wait_until("the original acknowledged read to become current", || {
        let _ = manager.poll();
        original.load(Ordering::Acquire)
    });
    graph
        .put(automerge::ROOT, "body", "after the held acknowledgement")
        .unwrap();
    graph.commit();
    let expected = graph.save();
    let (entered, observed) = mpsc::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    {
        let mut captures = captures.lock().unwrap();
        captures.committed_history_fixture = Some(expected.clone());
        captures.committed_revision = Some([8; 32]);
        match outcome {
            AcknowledgementOutcome::BrokerRefusal => {
                // A server-provided string cannot masquerade as our local clock.
                captures.committed_ack_failure =
                    Some("committed read acknowledgement timed out".into());
            }
            AcknowledgementOutcome::InvalidReply => {
                captures.committed_ack_reply =
                    Some(runtime_collab::committed_document::Reply::Unavailable);
            }
            _ => captures.committed_ack_gate = Some((entered, gate)),
        }
    }
    if matches!(
        outcome,
        AcknowledgementOutcome::BrokerRefusal | AcknowledgementOutcome::InvalidReply
    ) {
        tilo_test_env::wait_until("the actual broker refusal to close the route", || {
            !manager.is_connected()
        });
    } else {
        tilo_test_env::recv(
            "the broker to withhold the changed capture acknowledgement",
            &observed,
        );
    }
    assert!(!original.load(Ordering::Acquire));
    let second_release = match outcome {
        AcknowledgementOutcome::Renew
        | AcknowledgementOutcome::BrokerRefusal
        | AcknowledgementOutcome::InvalidReply => None,
        AcknowledgementOutcome::RepeatedDeadline => {
            let (entered, observed) = mpsc::channel();
            let (release, gate) = tokio::sync::oneshot::channel();
            captures.lock().unwrap().committed_ack_gate = Some((entered, gate));
            Some((release, observed))
        }
        AcknowledgementOutcome::ChangedCustody => {
            captures.lock().unwrap().committed_private_binding = Some(42);
            None
        }
    };
    let mut delivered = Vec::new();
    tilo_test_env::wait_until("a replacement capture or terminal route outcome", || {
        delivered.extend(manager.poll());
        !manager.is_connected()
            || delivered
                .iter()
                .any(|event| matches!(event, CollabEvent::CommittedDocument { .. }))
    });
    let connected = manager.is_connected();
    let joins = captures.lock().unwrap().joined.clone();
    let mut verified = 0;
    for event in delivered {
        if let CollabEvent::CommittedDocument { read, .. } = event {
            read.with_mutation(|mutation| {
                assert_eq!(
                    mutation,
                    &collab_types::DocumentMutation::Snapshot(expected.clone())
                );
            })
            .unwrap();
            read.applied();
            verified += 1;
        }
    }
    let second_observed = second_release
        .as_ref()
        .map(|(_, observed)| observed.try_recv().is_ok());
    let _ = release.send(());
    if let Some((release, _observed)) = second_release {
        let _ = release.send(());
    }
    drop(manager);
    thread.join().unwrap();
    let terminal_reply = matches!(
        outcome,
        AcknowledgementOutcome::BrokerRefusal | AcknowledgementOutcome::InvalidReply
    );
    assert_eq!(
        joins.len(),
        if terminal_reply { 1 } else { 2 },
        "only one local deadline permits bounded renewal; broker replies never renew"
    );
    if !terminal_reply {
        assert_eq!(
            joins[0], joins[1],
            "renewal retains the exact route and identity presentation"
        );
    }
    assert!(
        !original.load(Ordering::Acquire),
        "a late acknowledgement cannot reopen the old epoch"
    );
    match outcome {
        AcknowledgementOutcome::Renew => {
            assert!(
                connected,
                "an acknowledgement deadline must retire the capture, then renew through the original admitted root"
            );
            assert_eq!(
                verified, 1,
                "only the newly acknowledged capture reaches the owner"
            );
        }
        AcknowledgementOutcome::RepeatedDeadline
        | AcknowledgementOutcome::ChangedCustody
        | AcknowledgementOutcome::BrokerRefusal
        | AcknowledgementOutcome::InvalidReply => {
            if matches!(outcome, AcknowledgementOutcome::RepeatedDeadline) {
                assert_eq!(
                    second_observed,
                    Some(true),
                    "the renewed capture actually reaches its second held acknowledgement"
                );
            }
            assert!(
                !connected,
                "a second deadline or changed private custody must close the root"
            );
            assert_eq!(
                verified, 0,
                "no unacknowledged or changed-custody capture reaches the owner"
            );
        }
    }
}

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

fn open_count(captures: &Captures) -> usize {
    captures
        .committed_reads
        .iter()
        .filter(|request| {
            matches!(
                request,
                runtime_collab::committed_document::Request::Open { .. }
            )
        })
        .count()
}

/// One original reader whose first committed read was applied and is ready.
struct ReadyReader {
    manager: CollaborationManager,
    captures: SharedCaptures,
    thread: thread::JoinHandle<()>,
    state: BridgeState,
    key: String,
    graph: automerge::AutoCommit,
    /// Committed reads delivered while the test waited for something else.
    delivered: Vec<CommittedDocument>,
}

impl ReadyReader {
    fn start() -> Self {
        use automerge::transaction::Transactable as _;
        let mut graph = automerge::AutoCommit::new();
        graph.put(automerge::ROOT, "body", "first read").unwrap();
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
        let manager =
            CollaborationManager::from_sdk_fd_for_test(fd, "original-app", Uuid::new_v4());
        let state = broker_state(&manager);
        let key = manager.primary_route().unwrap().route.channel_id();
        let mut reader = Self {
            manager,
            captures,
            thread,
            state,
            key,
            graph,
            delivered: Vec::new(),
        };
        assert_eq!(reader.apply_next_read("the first committed read"), 1);
        reader
    }

    fn ready(&self) -> bool {
        self.state
            .document_reads
            .delivery_generation(&self.key)
            .is_some_and(|ready| ready.load(Ordering::Acquire))
    }

    fn joins(&self) -> usize {
        self.captures.lock().unwrap().joined.len()
    }

    fn opens(&self) -> usize {
        open_count(&self.captures.lock().unwrap())
    }

    /// Change the committed document, so the next full read must carry it.
    fn commit(&mut self, body: &str) {
        use automerge::transaction::Transactable as _;
        self.graph.put(automerge::ROOT, "body", body).unwrap();
        self.graph.commit();
        self.captures.lock().unwrap().committed_history_fixture = Some(self.graph.save());
    }

    /// Hold the broker's answer to the next unchanged poll until the returned
    /// sender fires: from the reader's side, a poll starved past its deadline.
    fn starve_next_poll(&self) -> tokio::sync::oneshot::Sender<()> {
        let (release, gate) = tokio::sync::oneshot::channel();
        self.captures.lock().unwrap().committed_recheck_gate = Some(gate);
        release
    }

    /// Apply every committed read the owner receives until the route is
    /// ready again or closed; returns how many reads were applied.
    fn apply_next_read(&mut self, what: &str) -> usize {
        let mut applied = 0;
        let expected = self.graph.save();
        tilo_test_env::wait_until(what, || {
            self.pump();
            for read in self.delivered.drain(..) {
                read.with_mutation(|mutation| {
                    assert_eq!(
                        mutation,
                        &collab_types::DocumentMutation::Snapshot(expected.clone()),
                        "only a capture of the current committed document reaches the owner"
                    );
                })
                .unwrap();
                read.applied();
                applied += 1;
            }
            !self.manager.is_connected() || (applied > 0 && self.ready())
        });
        applied
    }

    /// Poll the manager, keeping every committed read for the owner.
    fn pump(&mut self) {
        for event in self.manager.poll() {
            if let CollabEvent::CommittedDocument { read, .. } = event {
                self.delivered.push(read);
            }
        }
    }

    /// Wait until the reader either closed or joined `joins` times in total.
    fn wait_for_join_or_close(&mut self, joins: usize, what: &str) {
        tilo_test_env::wait_until(what, || {
            self.pump();
            !self.manager.is_connected() || self.joins() >= joins
        });
    }

    fn wait_for_close(&mut self, what: &str) {
        tilo_test_env::wait_until(what, || {
            self.pump();
            !self.manager.is_connected()
        });
    }

    /// One slow poll, one renewal, and a successful read on the renewed
    /// channel, as on a runner that stalls once.
    fn recover_from_one_slow_poll(&mut self) -> tokio::sync::oneshot::Sender<()> {
        let joins = self.joins();
        self.commit("after the first slow poll");
        let release = self.starve_next_poll();
        self.wait_for_join_or_close(joins + 1, "the first slow poll to renew");
        assert!(
            self.manager.is_connected(),
            "one slow poll renews instead of closing"
        );
        assert_eq!(self.apply_next_read("the first renewed read"), 1);
        assert!(self.ready(), "the renewed channel publishes readiness");
        release
    }

    fn finish(self) {
        drop(self.manager);
        self.thread.join().unwrap();
    }
}

/// A poll starved past its deadline after the reader already renewed once
/// and read successfully on the renewed channel: the laptop that suspends
/// twice, the runner that stalls twice.
#[test]
fn a_slow_poll_after_a_renewed_read_renews_again() {
    let mut reader = ReadyReader::start();
    let _first = reader.recover_from_one_slow_poll();
    reader.commit("after the second slow poll");
    let _second = reader.starve_next_poll();
    reader.wait_for_join_or_close(3, "the second slow poll to renew");
    let connected = reader.manager.is_connected();
    let joins = reader.joins();
    let applied = if connected {
        reader.apply_next_read("the second renewed read")
    } else {
        0
    };
    let ready = reader.ready();
    reader.finish();
    assert!(
        connected,
        "a slow poll after a successful renewed read must renew again, not disable collaboration"
    );
    assert_eq!(
        joins, 3,
        "each recovery rejoins through the original root once"
    );
    assert_eq!(applied, 1, "the second renewed read reaches the owner");
    assert!(ready, "the route is ready again after the second recovery");
}

/// The poll misses its deadline, then the rejoin misses its own, as when a
/// runner stalls for longer than both (3.8 s in run 97868).
#[test]
fn a_slow_renewal_is_retried_instead_of_closing() {
    let mut reader = ReadyReader::start();
    let (release_join, held_join) = tokio::sync::oneshot::channel();
    reader
        .captures
        .lock()
        .unwrap()
        .join_gates
        .push_back(held_join);
    reader.commit("after the slow renewal");
    let _poll = reader.starve_next_poll();
    reader.wait_for_join_or_close(3, "the slow renewal to be retried");
    let connected = reader.manager.is_connected();
    let ready_while_renewing = reader.ready();
    let applied = if connected {
        reader.apply_next_read("the read on the retried renewal")
    } else {
        0
    };
    let ready = reader.ready();
    let joins = reader.captures.lock().unwrap().joined.clone();
    let _ = release_join.send(());
    reader.finish();
    assert!(
        connected,
        "a renewal that misses its local deadline is retried, not terminal"
    );
    assert!(
        !ready_while_renewing,
        "readiness stays withheld while the renewal is retried"
    );
    assert_eq!(joins.len(), 3);
    assert!(
        joins[1] == joins[0] && joins[2] == joins[0],
        "every retry presents the exact original route and identity"
    );
    assert_eq!(applied, 1, "the retried renewal's read reaches the owner");
    assert!(ready);
}

/// Revocation after a recovery: the broker refuses the next poll, and the
/// reader closes on that very poll, exactly as it does without a renewal.
#[test]
fn a_refused_poll_after_a_renewed_read_still_closes_on_that_poll() {
    let mut reader = ReadyReader::start();
    let _first = reader.recover_from_one_slow_poll();
    // Count and revoke under one lock, so no unrefused poll slips between.
    let opens = {
        let mut captures = reader.captures.lock().unwrap();
        captures.committed_open_failure = Some("committed read refused: peer revoked".into());
        open_count(&captures)
    };
    reader.wait_for_close("the revoked peer's refused poll to close the reader");
    let ready = reader.ready();
    let refused_opens = reader.opens() - opens;
    let joins = reader.joins();
    let delivered = reader.delivered.len();
    reader.finish();
    assert!(!ready, "a refused peer keeps no read readiness");
    assert_eq!(delivered, 0, "a refused peer receives no committed read");
    assert_eq!(
        refused_opens, 1,
        "the first refused poll closes the reader: no retry, no extra latency"
    );
    assert_eq!(
        joins, 2,
        "a broker refusal never renews, even with renewal budget left"
    );
}

#[test]
fn a_refused_renewal_after_a_slow_poll_still_closes() {
    refused_renewal(false);
}

#[test]
fn a_refused_renewal_after_a_held_one_still_closes() {
    refused_renewal(true);
}

/// A revoked peer whose poll stalls: the broker's fresh admission check
/// refuses its rejoin and the reader closes, whether or not an earlier rejoin
/// only missed its deadline.
fn refused_renewal(held_first: bool) {
    let mut reader = ReadyReader::start();
    let _first = reader.recover_from_one_slow_poll();
    let mut release = None;
    {
        let mut captures = reader.captures.lock().unwrap();
        if held_first {
            let (sender, gate) = tokio::sync::oneshot::channel();
            captures.join_gates.push_back(gate);
            release = Some(sender);
        }
        captures.join_refusal = Some("collaboration join identity rejected: revoked".into());
    }
    let _poll = reader.starve_next_poll();
    reader.wait_for_close("the refused renewal to close the reader");
    let ready = reader.ready();
    let joins = reader.joins();
    let delivered = reader.delivered.len();
    if let Some(release) = release {
        let _ = release.send(());
    }
    reader.finish();
    assert!(!ready, "a refused renewal keeps no read readiness");
    assert_eq!(delivered, 0, "a refused renewal delivers no committed read");
    assert_eq!(
        joins,
        if held_first { 4 } else { 3 },
        "the first refused rejoin closes the reader; it is never retried"
    );
}

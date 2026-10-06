use super::*;
use crate::host_worker::managed_sender::{NativeSenderOutboxV1, attempt::RequestView};
use crate::host_worker::private_actions::{
    PrivateReadOutcome, SenderScope, committed_test_fixture,
};
fn caller() -> CallerView<'static> {
    CallerView {
        root: [1; 32],
        device: [2; 32],
        app: "caller-app",
        record: "caller-record",
        agent: None,
    }
}
fn binding() -> PublishedInvocationBinding {
    PublishedInvocationBinding {
        host: [3; 16],
        peer_id: 7,
        app_id: "target-app".into(),
        record_id: "target-record".into(),
        root: [4; 32],
        device: [5; 32],
        target_binding: [6; 32],
    }
}
fn intent(binding: &PublishedInvocationBinding) -> Intent<'_> {
    Intent {
        binding,
        revision: "original",
        resource: "record",
        action: "edit",
        argument: Some(""),
    }
}
fn commit(sender: &mut ManagedSender) -> Completion {
    for _ in 0..8 {
        if let Some(done) = sender.advance().unwrap() {
            return done;
        }
    }
    panic!("original physical mutation did not complete")
}
#[test]
fn managed_sender_lifecycle_real_barrier_before_original_uuid_attempt() {
    let (_root, current, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let ticket = sender.begin_new(caller(), intent(&binding)).unwrap();
    assert!(matches!(
        sender.request(ticket, caller(), Operation::Invoke, "{}", Instant::now()),
        Err(Failure::Busy)
    ));
    assert_eq!(commit(&mut sender), Completion::Began(ticket));
    let attempt = sender
        .request(ticket, caller(), Operation::Invoke, "{}", Instant::now())
        .unwrap();
    let route = crate::collaboration::RouteSpec::record("caller-app", sender.room, "caller-record");
    let Some(Ok(RequestView::Invoke {
        invocation,
        binding,
        argument,
        ..
    })) = attempt.claim(&route)
    else {
        panic!("actual retained request missing")
    };
    assert_eq!(invocation, &ticket.invocation);
    assert_eq!(binding.host, ticket.host);
    assert_eq!(argument, Some(""));
    assert!(matches!(
        sender.request(ticket, caller(), Operation::Invoke, "{}", Instant::now()),
        Err(Failure::Busy)
    ));
}
#[test]
fn managed_sender_lifecycle_terminal_barrier_and_inflight_consumption_fence() {
    let (_root, current, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let ticket = sender.begin_new(caller(), intent(&binding)).unwrap();
    commit(&mut sender);
    assert!(sender.terminal_receipt(ticket, caller()).is_err());
    let attempt = sender
        .request(ticket, caller(), Operation::Observe, "", Instant::now())
        .unwrap();
    let route = crate::collaboration::RouteSpec::record("caller-app", sender.room, "caller-record");
    assert!(attempt.claim(&route).unwrap().is_ok());
    let reply = attempt
        .finish(Ok(runtime_collab::PublishedInvocationObservation {
            host: ticket.host,
            invocation: ticket.invocation,
            result: Some(runtime_collab::PublishedInvocationResult {
                id: 7,
                update_json: format!(
                    r#"{{"state":"succeeded","invocation":"{}","output":{{}}}}"#,
                    uuid::Uuid::from_bytes(ticket.invocation)
                ),
                uncertain: false,
                cancellation_requested: false,
            }),
        }))
        .unwrap();
    let still_in_flight = reply.clone();
    sender.observe_result(reply, caller()).unwrap();
    assert!(sender.terminal_receipt(ticket, caller()).is_err());
    assert_eq!(commit(&mut sender), Completion::Observed(ticket));
    let receipt = sender.terminal_receipt(ticket, caller()).unwrap();
    assert!(matches!(
        sender.consume(receipt, caller()),
        Err(Failure::Busy)
    ));
    drop(still_in_flight);
    let receipt = sender.terminal_receipt(ticket, caller()).unwrap();
    sender.consume(receipt, caller()).unwrap();
    assert_eq!(commit(&mut sender), Completion::Consumed(ticket));
    assert!(matches!(
        sender.request(ticket, caller(), Operation::Invoke, "{}", Instant::now()),
        Err(Failure::NotRetained)
    ));
}

fn reopen(root: &std::path::Path) -> (ManagedSender, crate::portable_storage::PortableStorage) {
    let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let scope = SenderScope::native(uuid::Uuid::from_bytes([1; 16]), [2; 32], [3; 32]).unwrap();
    let file = PrivateActionsFile::from_held(
        tilo_files::native::atomic::StorageDirectory::open(root).unwrap(),
        scope,
        &cache,
    )
    .unwrap_or_else(|e| panic!("reopen {:?}", e.code()));
    let read = match file.read_existing() {
        PrivateReadOutcome::Committed(read) => read,
        PrivateReadOutcome::Failed(error) => panic!("read {:?}", error.code()),
    };
    let state = read
        .hydrate_current()
        .unwrap_or_else(|e| panic!("hydrate {:?}", e.code()));
    (ManagedSender::open(state).unwrap(), cache)
}
#[test]
fn managed_sender_lifecycle_restart_retains_committed_before_send_original_identity() {
    let (root, current, cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let original = sender.begin_new(caller(), intent(&binding)).unwrap();
    commit(&mut sender);
    drop(sender);
    drop(cache);
    let (mut reopened, _cache) = reopen(root.path());
    let recovered = reopened.recover_pending(caller()).unwrap();
    assert_eq!(
        recovered.iter().flatten().copied().collect::<Vec<_>>(),
        vec![original]
    );
    let attempt = reopened
        .request(
            original,
            caller(),
            Operation::Invoke,
            "current confirmation",
            Instant::now(),
        )
        .unwrap();
    let route =
        crate::collaboration::RouteSpec::record("caller-app", reopened.room, "caller-record");
    let Some(Ok(RequestView::Invoke {
        invocation,
        binding,
        revision,
        argument,
        confirmation_json,
        ..
    })) = attempt.claim(&route)
    else {
        panic!("missing original request")
    };
    assert_eq!(invocation, &original.invocation);
    assert_eq!(binding.target_binding, [6; 32]);
    assert_eq!(revision, "original");
    assert_eq!(argument, Some(""));
    assert_eq!(confirmation_json, "current confirmation");
}
#[test]
fn managed_sender_lifecycle_busy_observation_retains_actual_result_custody() {
    let (_root, current, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let first = sender.begin_new(caller(), intent(&binding)).unwrap();
    commit(&mut sender);
    let attempt = sender
        .request(first, caller(), Operation::Observe, "", Instant::now())
        .unwrap();
    let route = crate::collaboration::RouteSpec::record("caller-app", sender.room, "caller-record");
    assert!(attempt.claim(&route).unwrap().is_ok());
    let reply = attempt
        .finish(Ok(runtime_collab::PublishedInvocationObservation {
            host: first.host,
            invocation: first.invocation,
            result: None,
        }))
        .unwrap();
    sender.begin_new(caller(), intent(&binding)).unwrap();
    let refusal = sender.observe_result(reply, caller()).unwrap_err();
    let owner = sender
        .active
        .iter()
        .flatten()
        .find(|owner| owner.ticket() == first)
        .unwrap();
    assert!(
        !owner.quiescent(),
        "busy response must remain owned for later owner-thread processing"
    );
    assert_eq!(refusal.failure(), Failure::Busy);
    commit(&mut sender);
    assert_eq!(
        sender
            .observe_result(refusal.into_result(), caller())
            .unwrap(),
        ReplyDisposition::Absent
    );
    assert!(
        sender
            .active
            .iter()
            .flatten()
            .find(|owner| owner.ticket() == first)
            .unwrap()
            .quiescent()
    );
}

fn terminal(sender: &mut ManagedSender, ticket: Ticket) {
    let attempt = sender
        .request(ticket, caller(), Operation::Observe, "", Instant::now())
        .unwrap();
    let route = crate::collaboration::RouteSpec::record("caller-app", sender.room, "caller-record");
    assert!(attempt.claim(&route).unwrap().is_ok());
    let reply=attempt.finish(Ok(runtime_collab::PublishedInvocationObservation{host:ticket.host,invocation:ticket.invocation,result:Some(runtime_collab::PublishedInvocationResult{id:7,update_json:format!(r#"{{"state":"succeeded","invocation":"{}","output":{{"value":"original result"}}}}"#,uuid::Uuid::from_bytes(ticket.invocation)),uncertain:false,cancellation_requested:false})})).unwrap();
    sender.observe_result(reply, caller()).unwrap();
    assert_eq!(commit(sender), Completion::Observed(ticket));
}
#[test]
fn managed_sender_lifecycle_original_committed_result_borrow_and_scope() {
    let (_root, current, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let ticket = sender.begin_new(caller(), intent(&binding)).unwrap();
    commit(&mut sender);
    terminal(&mut sender, ticket);
    {
        let read = sender.read_result(ticket, caller()).unwrap();
        assert!(read.json().contains("original result"));
        let current = sender.current().unwrap().borrow_current().unwrap();
        assert!(
            current
                .payload()
                .as_ptr_range()
                .contains(&read.json().as_ptr()),
            "lend original paid backing without another result copy"
        );
    }
    for foreign in [
        CallerView {
            app: "another",
            ..caller()
        },
        CallerView {
            agent: Some("agent"),
            ..caller()
        },
        CallerView {
            root: [9; 32],
            ..caller()
        },
    ] {
        assert!(matches!(
            sender.read_result(ticket, foreign),
            Err(Failure::Scope)
        ));
        assert!(matches!(
            sender.request(ticket, foreign, Operation::Invoke, "{}", Instant::now()),
            Err(Failure::Scope)
        ));
        assert!(
            sender
                .recover_pending(foreign)
                .unwrap()
                .iter()
                .all(Option::is_none)
        );
    }
}
#[test]
fn managed_sender_lifecycle_uncertain_actual_barrier_sends_nothing_and_reopens_original_row() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let (root, current, cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let ticket = sender.begin_new(caller(), intent(&binding)).unwrap();
    assert_eq!(sender.advance().unwrap(), None); // exact capture
    assert_eq!(sender.advance().unwrap(), None); // actual prepared write
    assert!(with_failure(Checkpoint::BeforeCommit, || sender.advance()).is_err());
    assert!(matches!(
        sender.request(ticket, caller(), Operation::Invoke, "{}", Instant::now()),
        Err(Failure::Busy)
    ));
    assert!(sender.active.iter().all(Option::is_none));
    // Simulated process loss after rename: the existing-only reader performs
    // its genuine reconciliation barrier before it can expose this original row.
    drop(sender);
    drop(cache);
    let (mut reopened, _cache) = reopen(root.path());
    assert_eq!(
        reopened
            .recover_pending(caller())
            .unwrap()
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>(),
        vec![ticket]
    );
    assert!(
        reopened
            .request(ticket, caller(), Operation::Invoke, "{}", Instant::now())
            .is_ok()
    );
}
#[test]
fn managed_sender_lifecycle_terminal_consume_reuses_capacity_beyond_old_lifetime_ceiling() {
    let (_root, current, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let mut first = None;
    let mut last = None;
    for _ in 0..4097 {
        let ticket = sender.begin_new(caller(), intent(&binding)).unwrap();
        assert_ne!(Some(ticket), last);
        first.get_or_insert(ticket);
        last = Some(ticket);
        commit(&mut sender);
        terminal(&mut sender, ticket);
        let receipt = sender.terminal_receipt(ticket, caller()).unwrap();
        sender.consume(receipt, caller()).unwrap();
        assert_eq!(commit(&mut sender), Completion::Consumed(ticket));
        let current = sender.current().unwrap().borrow_current().unwrap();
        assert_eq!(current.payload(), NativeSenderOutboxV1::empty_bytes());
    }
    assert!(matches!(
        sender.request(
            first.unwrap(),
            caller(),
            Operation::Invoke,
            "{}",
            Instant::now()
        ),
        Err(Failure::NotRetained)
    ));
    assert!(
        sender
            .recover_pending(caller())
            .unwrap()
            .iter()
            .all(Option::is_none)
    );
}

#[test]
fn managed_sender_lifecycle_active_byte_backpressure_preserves_every_unresolved_row() {
    let (_root, current, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut sender = ManagedSender::open(current).unwrap();
    let binding = binding();
    let mut tickets = Vec::new();
    for _ in 0..RECOVERY_SLOTS {
        let ticket = sender.begin_new(caller(), intent(&binding)).unwrap();
        assert_eq!(commit(&mut sender), Completion::Began(ticket));
        tickets.push(ticket);
    }
    assert!(matches!(
        sender.begin_new(caller(), intent(&binding)),
        Err(Failure::Capacity)
    ));
    let mut recovered: Vec<_> = sender
        .recover_pending(caller())
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    tickets.sort_by_key(|ticket| ticket.invocation);
    recovered.sort_by_key(|ticket| ticket.invocation);
    assert_eq!(recovered, tickets);
    assert!(sender.active.iter().all(Option::is_none));
}

#[test]
fn managed_sender_lifecycle_invalid_codec_preserves_actual_committed_state() {
    let (_root, current, _cache) = committed_test_fixture(b"invalid native outbox");
    let Err(refusal) = ManagedSender::open(current) else {
        panic!("malformed committed bytes became a sender")
    };
    assert_eq!(refusal.failure, Failure::Invalid);
    let original = refusal.into_state();
    assert_eq!(
        original.borrow_current().unwrap().payload(),
        b"invalid native outbox"
    );
}

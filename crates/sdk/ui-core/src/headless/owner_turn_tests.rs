use super::*;

use crate::owner_wake::{OwnerTurnBudget, OwnerTurnCutoff};
use crate::time::{Duration, Instant};

fn budget(max_steps: usize) -> OwnerTurnBudget {
    OwnerTurnBudget {
        max_steps,
        deadline: None,
    }
}

#[test]
fn bounded_turn_leaves_older_owners_before_callback_late_work() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6100, false);
    let (b, sb, _) = prepared(&host, &queue, 6101, false);
    let (c, sc, _) = prepared(&host, &queue, 6102, false);
    let (d, sd, _) = prepared(&host, &queue, 6103, false);
    let (mut frame, visits, _) = frame(&queue, vec![a, b, c, d]);
    sa.order.borrow_mut().clear();
    let before = visits.get();
    let aw = sa.wake.borrow().as_ref().unwrap().clone();
    let dw = sd.wake.borrow().as_ref().unwrap().clone();
    *sa.on_sync.borrow_mut() = Some(Box::new(move || {
        aw.notify_events();
        aw.wake();
        dw.wake();
    }));
    for seen in [&sa, &sb, &sc] {
        seen.wake.borrow().as_ref().unwrap().wake();
    }
    let report = frame.service_owner_turn(budget(1));
    let completed = report.completed;
    let observed = sa.order.borrow().clone();
    let visit_count = visits.get() - before;
    assert_eq!(report.attempted, 1);
    assert_eq!(report.remaining, 4);
    assert_eq!(report.cutoff, OwnerTurnCutoff::StepLimit);
    for _ in 0..4 {
        assert_eq!(frame.service_owner_turn(budget(1)).completed, 1);
    }
    assert_eq!(&*sa.order.borrow(), &[6100, 6101, 6102, 6100, 6103]);
    close(&mut frame, 4);
    assert_eq!(completed, 1, "one-step turn must return after A");
    assert_eq!(observed, [6100]);
    assert_eq!(visit_count, 1);
}

#[test]
fn bounded_turn_deadline_charges_slow_callback_and_never_enters_b() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6110, false);
    let (b, sb, _) = prepared(&host, &queue, 6111, false);
    let (mut frame, visits, _) = frame(&queue, vec![a, b]);
    let start = Instant::now();
    let elapsed = Rc::new(Cell::new(Duration::ZERO));
    let during = elapsed.clone();
    *sa.on_sync.borrow_mut() = Some(Box::new(move || during.set(Duration::from_millis(7))));
    sa.order.borrow_mut().clear();
    let before = visits.get();
    sa.wake.borrow().as_ref().unwrap().wake();
    sb.wake.borrow().as_ref().unwrap().wake();
    let report = frame.service_owner_turn_with_clock(
        OwnerTurnBudget {
            max_steps: 8,
            deadline: start.checked_add(Duration::from_millis(2)),
        },
        || start.checked_add(elapsed.get()).unwrap(),
    );
    assert_eq!(
        (report.attempted, report.completed, report.remaining),
        (1, 1, 1)
    );
    assert_eq!(report.cutoff, OwnerTurnCutoff::Deadline);
    assert_eq!(report.elapsed, Duration::from_millis(7));
    assert_eq!(report.overrun, Duration::from_millis(5));
    assert_eq!(&*sa.order.borrow(), &[6110]);
    assert_eq!(visits.get() - before, 1);
    assert_eq!(frame.service_owner_turn(budget(1)).completed, 1);
    close(&mut frame, 2);
}

#[test]
fn bounded_turn_empty_zero_and_expired_do_not_retire_maps_or_visit() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6120, false);
    let (mut frame, visits, _) = frame(&queue, vec![a]);
    frame.try_relayout().unwrap();
    let before = visits.get();
    let built = frame.layout_built;
    let empty = frame.service_owner_turn_with_clock(budget(8), || panic!("idle clock read"));
    assert_eq!(empty.cutoff, OwnerTurnCutoff::Idle);
    sa.wake.borrow().as_ref().unwrap().wake();
    let zero = frame.service_owner_turn_with_clock(budget(0), || panic!("zero budget clock read"));
    assert_eq!((zero.attempted, zero.remaining), (0, 1));
    let now = Instant::now();
    let expired = frame.service_owner_turn_with_clock(
        OwnerTurnBudget {
            max_steps: 1,
            deadline: Some(now),
        },
        || now,
    );
    assert_eq!(expired.cutoff, OwnerTurnCutoff::Deadline);
    assert_eq!((expired.attempted, expired.remaining), (0, 1));
    assert_eq!(visits.get(), before);
    assert_eq!(frame.layout_built, built);
    assert!(!frame.owner_service_layout_retired);
    assert_eq!(frame.service_owner_turn(budget(1)).completed, 1);
    close(&mut frame, 1);
}

#[test]
fn bounded_turn_busy_middle_restores_old_prefix_and_charges_failed_attempt() {
    let host = ProbeHost::new();
    let busy_host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6130, false);
    let (b, sb, _) = prepared(&busy_host, &queue, 6131, false);
    let bid = b.component.record().id.clone();
    let (c, sc, _) = prepared(&host, &queue, 6132, false);
    let (d, sd, _) = prepared(&host, &queue, 6133, false);
    let (mut frame, _, _) = frame(&queue, vec![a, b, c, d]);
    let aw = sa.wake.borrow().as_ref().unwrap().clone();
    let bw = sb.wake.borrow().as_ref().unwrap().clone();
    let dw = sd.wake.borrow().as_ref().unwrap().clone();
    *sa.on_sync.borrow_mut() = Some(Box::new(move || {
        aw.notify_events();
        aw.wake();
        bw.notify_events();
        dw.wake();
    }));
    sa.order.borrow_mut().clear();
    for seen in [&sa, &sb, &sc] {
        seen.wake.borrow().as_ref().unwrap().wake();
    }
    let report = busy_host
        .records
        .with_action_state(&bid, |_, _, _| frame.service_owner_turn(budget(3)))
        .unwrap();
    assert_eq!(
        (report.attempted, report.completed, report.remaining),
        (2, 1, 4)
    );
    assert_eq!(report.cutoff, OwnerTurnCutoff::Blocked);
    assert!(matches!(
        report.result,
        Err(BoundDispatchError::Record(
            crate::record::RecordOpenError::Busy
        ))
    ));
    let pending = queue.take_pending();
    assert_eq!(
        pending
            .iter()
            .map(|r| r.binding().instance_id)
            .collect::<Vec<_>>(),
        [6131, 6132, 6130, 6133]
    );
    assert!(pending[0].requires_sync());
    assert!(pending[2].requires_sync());
    for request in pending {
        assert!(queue.requeue(request));
    }
    assert_eq!(frame.service_owner_turn(budget(8)).completed, 4);
    assert_eq!(&*sa.order.borrow(), &[6130, 6131, 6132, 6130, 6133]);
    close(&mut frame, 4);
}

#[test]
fn bounded_turn_unwind_restores_unvisited_before_untaken_tail() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let mut seen = Vec::new();
    let owners = (0..4)
        .map(|i| {
            let (owner, observation, _) = prepared(&host, &queue, 6140 + i, false);
            seen.push(observation);
            owner
        })
        .collect();
    let (mut frame, _, _) = frame(&queue, owners);
    seen[1].panic.set(true);
    for owner in &seen {
        owner.wake.borrow().as_ref().unwrap().wake();
    }
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || frame.service_owner_turn(budget(3))
        ))
        .is_err()
    );
    let pending = queue.take_pending();
    assert_eq!(
        pending
            .iter()
            .map(|r| r.binding().instance_id)
            .collect::<Vec<_>>(),
        [6141, 6142, 6143]
    );
    for request in pending {
        assert!(queue.requeue(request));
    }
    assert_eq!(frame.service_owner_turn(budget(1)).completed, 1);
    assert_eq!(frame.service_pending_owner_models().unwrap(), 2);
    close(&mut frame, 4);
}

#[test]
fn bounded_queue_intake_and_repeated_restore_touch_only_small_prefix() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let mut seen = Vec::new();
    let owners = (0..256)
        .map(|i| {
            let (owner, observation, _) = prepared(&host, &queue, 6200 + i, false);
            seen.push(observation);
            owner
        })
        .collect();
    let (mut frame, _, _) = frame(&queue, owners);
    for observation in &seen {
        observation.wake.borrow().as_ref().unwrap().wake();
    }
    crate::owner_wake::test_support::take_touches(&queue);
    for _ in 0..16 {
        let batch = queue.take_batch(2);
        assert_eq!(batch.front().unwrap().binding().instance_id, 6200);
        assert_eq!(
            queue.pending_count(),
            254,
            "bounded intake must leave the untouched tail queued"
        );
        seen[0].wake.borrow().as_ref().unwrap().notify_events();
        seen[1].wake.borrow().as_ref().unwrap().wake();
        drop(batch);
        let touched = crate::owner_wake::test_support::take_touches(&queue);
        assert!(
            touched <= 24,
            "two-entry intake/retry touched {touched} nodes in a 256-owner backlog"
        );
        assert_eq!(queue.pending_count(), 256);
    }
    assert_eq!(frame.service_owner_turn(budget(2)).completed, 2);
    assert_eq!(frame.service_pending_owner_models().unwrap(), 254);
    close(&mut frame, 256);
}

#[test]
fn bounded_queue_public_snapshot_reentry_sees_late_requests_and_coalesces_retry() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6500, false);
    let (b, sb, _) = prepared(&host, &queue, 6501, false);
    let (c, sc, _) = prepared(&host, &queue, 6502, false);
    let (mut frame, _, _) = frame(&queue, vec![a, b, c]);
    for observation in [&sa, &sb, &sc] {
        observation.wake.borrow().as_ref().unwrap().notify_events();
    }
    let mut batch = queue.take_batch(2);
    batch.complete_front();
    sb.wake.borrow().as_ref().unwrap().wake();
    let reentrant = queue.clone();
    let late = reentrant.take_pending();
    assert_eq!(
        late.iter()
            .map(|r| r.binding().instance_id)
            .collect::<Vec<_>>(),
        [6502, 6501]
    );
    assert!(late[1].requires_sync());
    for request in late {
        assert!(reentrant.requeue(request));
    }
    drop(batch);
    let restored = queue.take_pending();
    assert_eq!(
        restored
            .iter()
            .map(|r| r.binding().instance_id)
            .collect::<Vec<_>>(),
        [6501, 6502]
    );
    assert!(restored[0].requires_sync());
    for request in restored {
        assert!(queue.requeue(request));
    }
    assert_eq!(frame.service_owner_turn(budget(8)).completed, 2);
    close(&mut frame, 3);
}

// The binding still belongs to a mounted component while its queue registration
// can be independently retired/replaced. Capture actual original sink events.
fn manually_registered(
    host: &ProbeHost,
    queue: &OwnerWakeQueue,
    id: u64,
) -> (
    BoundAppWidget,
    Seen,
    crate::owner_wake::OwnerWakeRegistration,
    crate::owner_wake::OwnerWakeSink,
    crate::wake::WakeEvent,
) {
    let seen = Seen {
        order: ORDER.with_borrow(Clone::clone),
        ..Seen::default()
    };
    NEXT.with_borrow_mut(|next| *next = Some((seen.clone(), id, false)));
    let definition = probe::definition(factory);
    let context = host.prepare(&definition, id, &LaunchTarget::New).unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    component
        .mount(move |event| sink.lock().unwrap().push(event))
        .unwrap();
    let (registration, intake) = queue.register(component.callback_binding()).unwrap();
    let mut bound = BoundAppWidget { component };
    bound.sync(&mut AutoCommit::new(), &ROOT);
    seen.wake.borrow().as_ref().unwrap().wake();
    let event = received.lock().unwrap().pop().unwrap();
    (bound, seen, registration, intake, event)
}

#[test]
fn bounded_turn_closed_taken_registration_cannot_use_live_binding_or_replace_new_token() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6600, false);
    let (b, sb, registration, intake, event) = manually_registered(&host, &queue, 6601);
    let binding = b.component.callback_binding();
    let held = Rc::new(RefCell::new(Some(registration)));
    let replacement = Rc::new(RefCell::new(None));
    let during = held.clone();
    let (mut frame, _, _) = frame(&queue, vec![a, b]);
    let before = sb.syncs.get();
    *sa.on_sync.borrow_mut() = Some(Box::new(move || {
        drop(during.borrow_mut().take());
    }));
    sa.wake.borrow().as_ref().unwrap().wake();
    assert!(intake.enqueue(event.clone()));
    let now = Instant::now();
    let report = frame.service_owner_turn_with_clock(
        OwnerTurnBudget {
            max_steps: 2,
            deadline: now.checked_add(Duration::from_secs(1)),
        },
        || {
            // Register only after A's canonical loan returned. A live binding
            // cannot authorize cross-record registration from inside A.
            if held.borrow().is_none() && replacement.borrow().is_none() {
                let (fresh, fresh_sink) = queue.register(binding.clone()).unwrap();
                assert!(fresh_sink.enqueue(event.clone()));
                *replacement.borrow_mut() = Some(fresh);
            }
            now
        },
    );
    assert_eq!(
        (report.attempted, report.completed, report.remaining),
        (2, 1, 1)
    );
    assert!(matches!(
        report.result,
        Err(BoundDispatchError::ClosedLifetime)
    ));
    assert_eq!(sb.syncs.get(), before);
    assert!(!intake.enqueue(event));
    assert_eq!(frame.service_owner_turn(budget(1)).completed, 1);
    assert_eq!(sb.syncs.get(), before + 1);
    drop(replacement.borrow_mut().take());
    close(&mut frame, 2);
}

#[test]
fn bounded_queue_drop_after_close_never_resurrects_retry() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6610, false);
    let (mut frame, _, _) = frame(&queue, vec![a]);
    sa.wake.borrow().as_ref().unwrap().wake();
    let batch = queue.take_batch(1);
    close(&mut frame, 1);
    drop(batch);
    assert_eq!(queue.pending_count(), 0);
    let report = frame.service_owner_turn_with_clock(budget(1), || panic!("closed idle clock"));
    assert_eq!((report.attempted, report.completed), (0, 0));
}

#[test]
fn bounded_turn_unknown_visitor_refusal_counts_attempt_and_restoration_time() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6700, false);
    let (b, sb, _) = prepared(&host, &queue, 6701, false);
    let (mut frame, visits, refuse) = frame(&queue, vec![a, b]);
    sa.wake.borrow().as_ref().unwrap().wake();
    sb.wake.borrow().as_ref().unwrap().wake();
    let before = visits.get();
    let callbacks = (sa.syncs.get(), sb.syncs.get());
    refuse.set(true);
    let start = Instant::now();
    let mut samples = Vec::new();
    let report = frame.service_owner_turn_with_clock(
        OwnerTurnBudget {
            max_steps: 2,
            deadline: start.checked_add(Duration::from_secs(1)),
        },
        || {
            samples.push(queue.pending_count());
            start
                .checked_add(Duration::from_millis(u64::try_from(samples.len()).unwrap()))
                .unwrap()
        },
    );
    assert_eq!(
        (report.attempted, report.completed, report.remaining),
        (1, 0, 2)
    );
    assert!(matches!(
        report.result,
        Err(BoundDispatchError::CrossRecordReentry)
    ));
    assert_eq!(report.cutoff, OwnerTurnCutoff::Blocked);
    assert_eq!(
        samples,
        [2, 0, 2],
        "measurement includes intake and restored retry custody"
    );
    assert_eq!(report.elapsed, Duration::from_millis(2));
    assert_eq!(visits.get(), before + 1);
    assert_eq!((sa.syncs.get(), sb.syncs.get()), callbacks);
    assert!(frame.owner_service_layout_retired);
    refuse.set(false);
    assert_eq!(frame.service_owner_turn(budget(2)).completed, 2);
    close(&mut frame, 2);
}

#[test]
fn bounded_turn_rechecks_canonical_closing_gate_and_retains_refused_owner() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6710, false);
    let closing = a.component.callback_binding().closing;
    let (mut frame, _, _) = frame(&queue, vec![a]);
    sa.wake.borrow().as_ref().unwrap().wake();
    closing.set(true);
    let before = sa.syncs.get();
    let report = frame.service_owner_turn(budget(1));
    assert_eq!(
        (report.attempted, report.completed, report.remaining),
        (1, 0, 1)
    );
    assert!(matches!(
        report.result,
        Err(BoundDispatchError::ClosedLifetime)
    ));
    assert_eq!(sa.syncs.get(), before);
    closing.set(false);
    assert_eq!(frame.service_owner_turn(budget(1)).completed, 1);
    close(&mut frame, 1);
}

#[test]
fn bounded_turn_intake_crossing_deadline_restores_without_retiring_maps() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 6720, false);
    let (mut frame, visits, _) = frame(&queue, vec![a]);
    frame.try_relayout().unwrap();
    sa.wake.borrow().as_ref().unwrap().wake();
    let start = Instant::now();
    let mut reads = 0;
    let before = visits.get();
    let report = frame.service_owner_turn_with_clock(
        OwnerTurnBudget {
            max_steps: 1,
            deadline: start.checked_add(Duration::from_millis(2)),
        },
        || {
            reads += 1;
            start
                .checked_add(Duration::from_millis(if reads == 1 { 0 } else { 3 }))
                .unwrap()
        },
    );
    assert_eq!(
        (report.attempted, report.completed, report.remaining),
        (0, 0, 1)
    );
    assert_eq!(report.cutoff, OwnerTurnCutoff::Deadline);
    assert_eq!(report.elapsed, Duration::from_millis(3));
    assert_eq!(report.overrun, Duration::from_millis(1));
    assert_eq!(visits.get(), before);
    assert!(!frame.owner_service_layout_retired);
    assert_eq!(frame.service_owner_turn(budget(1)).completed, 1);
    close(&mut frame, 1);
}

fn registration_retired_during_traversal(bounded: bool) {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, registration, intake, event) = manually_registered(&host, &queue, 6800);
    let binding = owner.component.callback_binding();
    let held = Rc::new(RefCell::new(Some(registration)));
    let fresh = Rc::new(RefCell::new(None));
    let before_visit = Rc::new(RefCell::new(None));
    let inputs = probe::runner_inputs();
    let mut frame = FrameHarness::new_with_controlled_inputs(
        State::new(),
        Box::new(HiddenOwners {
            before_visit: before_visit.clone(),
            owners: vec![owner],
            visits: Rc::default(),
            refuse: Rc::default(),
        }),
        (320, 180),
        inputs.theme,
        inputs.text_renderer,
        probe::author(),
    );
    frame.attach_owner_wake_queue(queue.clone()).unwrap();
    let callbacks = (seen.updates.get(), seen.syncs.get(), seen.saves.get());
    let during = held.clone();
    let replacement = fresh.clone();
    let pending = queue.clone();
    let late = event.clone();
    *before_visit.borrow_mut() = Some(Box::new(move || {
        // This runs after the host precheck but before forwarding the retained
        // owner, with no canonical document loan. The binding remains live.
        drop(during.borrow_mut().take());
        let (registration, sink) = pending.register(binding).unwrap();
        assert!(sink.enqueue(late));
        *replacement.borrow_mut() = Some(registration);
    }));
    assert!(intake.enqueue(event));
    let (attempted, completed, result) = if bounded {
        let report = frame.service_owner_turn(budget(1));
        assert_eq!(report.remaining, 1);
        (report.attempted, report.completed, report.result)
    } else {
        let report = frame.service_pending_owner_models_report();
        (
            usize::from(report.attempted),
            report.completed,
            report.result,
        )
    };
    assert_eq!((attempted, completed), (1, 0));
    assert!(matches!(result, Err(BoundDispatchError::ClosedLifetime)));
    assert_eq!(
        (seen.updates.get(), seen.syncs.get(), seen.saves.get()),
        callbacks
    );
    assert_eq!(
        queue.pending_count(),
        1,
        "only the fresh token retains custody"
    );
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    assert_eq!(
        (seen.updates.get(), seen.syncs.get(), seen.saves.get()),
        (callbacks.0 + 1, callbacks.1 + 1, callbacks.2 + 1)
    );
    drop(fresh.borrow_mut().take());
    close(&mut frame, 1);
}

#[test]
fn bounded_turn_refuses_registration_retired_during_actual_traversal() {
    registration_retired_during_traversal(true);
}

#[test]
fn unlimited_service_refuses_registration_retired_during_actual_traversal() {
    registration_retired_during_traversal(false);
}

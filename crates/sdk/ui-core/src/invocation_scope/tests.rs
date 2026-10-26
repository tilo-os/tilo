use super::*;
use automerge::{ROOT, ReadDoc};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
struct OriginalCaller {
    actor: String,
    expiry: u64,
}

fn fixture(id: u64) -> crate::bound_dispatch::legacy_document_tests::Fixture {
    let fixture = crate::bound_dispatch::legacy_document_tests::Fixture::in_room(
        id,
        "77777777-7777-4777-8777-777777777777",
    );
    fixture
        .store
        .require_invocation_provenance(&fixture.checkout)
        .unwrap();
    fixture
}

fn scope(
    fixture: &crate::bound_dispatch::legacy_document_tests::Fixture,
    id: u64,
    actor: &str,
    expiry: u64,
    time: &Arc<AtomicU64>,
) -> ResourceInvocationScope {
    let caller = OriginalCaller {
        actor: actor.into(),
        expiry,
    };
    let sampled = time.clone();
    let request = PublishedInvocationRequest {
        wire_id: id,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
        address: tilo_actions::ActionAddress::new("fixture", "write"),
        argument: None,
        actor: actor.into(),
        participant: None,
    };
    let lease = PublishedInvocationLease::new(
        request,
        fixture.target.owner.broker_route().unwrap(),
        Arc::new(move || {
            (sampled.load(Ordering::Acquire) < expiry)
                .then_some(())
                .ok_or_else(|| "original caller grant expired".into())
        }),
    )
    .with_owner_token(Arc::new(caller))
    .unwrap();
    ResourceInvocationScope::for_owner(fixture.target.owner.clone(), lease).unwrap()
}

fn publication(
    fixture: &crate::bound_dispatch::legacy_document_tests::Fixture,
    from: &[ChangeHash],
) -> Result<
    (
        collab_types::DocumentMutation,
        DocumentAuthorization,
        Vec<ChangeHash>,
    ),
    String,
> {
    let provenance = fixture
        .store
        .invocation_provenance(&fixture.checkout.id)
        .unwrap();
    fixture
        .store
        .with_checked(&fixture.checkout.id, |document| {
            let bytes = document.save_after(from);
            let mutation = collab_types::DocumentMutation::Incremental(bytes);
            let actual = document
                .get_changes(from)
                .into_iter()
                .map(|change| change.hash())
                .collect();
            let authorization = provenance
                .borrow()
                .capture(document, from, &mutation)?
                .ok_or("missing required authorization")?;
            Ok((mutation, authorization, actual))
        })
        .unwrap()
}

#[test]
fn causal_scope_nested_contributors_survive_partial_error_and_match_actual_range() {
    let fixture = fixture(951);
    let time = Arc::new(AtomicU64::new(99));
    let a = scope(&fixture, 1, "caller-a", 100, &time);
    let b = scope(&fixture, 2, "caller-b", 200, &time);
    let before = fixture
        .store
        .read(&fixture.checkout.id, |doc| doc.base_heads())
        .unwrap();
    let result = fixture
        .store
        .with_checked(&fixture.checkout.id, |doc| {
            a.with_document_mutation(doc, |doc| {
                doc.put(ROOT, "a-before", 1).unwrap();
                b.with_document_mutation(doc, |doc| {
                    doc.put(ROOT, "b", 1).unwrap();
                    Ok(())
                })?;
                doc.put(ROOT, "a-after", 1).unwrap();
                Err::<(), _>("fixture partial error".into())
            })
        })
        .unwrap();
    assert_eq!(result.unwrap_err(), "fixture partial error");
    let (body, authorization, actual) = publication(&fixture, &before).unwrap();
    assert_eq!(authorization.0.contributions.len(), 2);
    assert!(authorization.check(a.lease.route(), &body).is_ok());
    assert!(
        authorization
            .validate_owner_base(&actual, |_| false)
            .is_ok()
    );
    let mut seen = BTreeSet::new();
    authorization
        .validate_owner_contributors(|request, route, token| {
            let original = token
                .downcast_ref::<OriginalCaller>()
                .ok_or("foreign token")?;
            if original.actor != request.actor
                || route != a.lease.route()
                || time.load(Ordering::Acquire) >= original.expiry
            {
                return Err("current original caller denied".into());
            }
            seen.insert(original.actor.clone());
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, ["caller-a".into(), "caller-b".into()].into());
    assert!(
        authorization
            .validate_owner_base(&actual[..actual.len() - 1], |_| true)
            .is_err()
    );
    let wrong_body = collab_types::DocumentMutation::Incremental(vec![1, 2, 3]);
    assert!(
        authorization
            .check_binding(a.lease.route(), &wrong_body)
            .is_err()
    );
    time.store(100, Ordering::Release);
    assert!(
        authorization.check(a.lease.route(), &body).is_err(),
        "both true contributors remain required after capture"
    );
}

#[test]
fn causal_scope_two_retained_jobs_expire_independently_without_resurrection() {
    let fixture = fixture(952);
    let time = Arc::new(AtomicU64::new(99));
    let jobs = [
        scope(&fixture, 3, "caller-a", 100, &time),
        scope(&fixture, 4, "caller-b", 200, &time),
    ];
    let before = fixture
        .store
        .read(&fixture.checkout.id, |doc| doc.base_heads())
        .unwrap();
    time.store(100, Ordering::Release);
    fixture
        .store
        .with_checked(&fixture.checkout.id, |doc| {
            assert!(
                jobs[0]
                    .with_document_mutation(doc, |doc| {
                        doc.put(ROOT, "expired", true).unwrap();
                        Ok(())
                    })
                    .is_err()
            );
            jobs[1]
                .with_document_mutation(doc, |doc| {
                    doc.put(ROOT, "valid", 1).unwrap();
                    Ok(())
                })
                .unwrap();
            jobs[1]
                .with_document_mutation(doc, |doc| {
                    doc.put(ROOT, "valid", 2).unwrap();
                    Ok(())
                })
                .unwrap();
            assert!(doc.get(ROOT, "expired").unwrap().is_none());
        })
        .unwrap();
    let (_, authorization, _) = publication(&fixture, &before).unwrap();
    assert_eq!(authorization.0.contributions.len(), 1);
    assert_eq!(
        authorization.0.contributions[0].lease.request().actor,
        "caller-b"
    );
    time.store(99, Ordering::Release);
    assert!(
        jobs[0].check().is_err(),
        "a terminally denied job cannot resurrect"
    );
    assert!(jobs[1].check().is_ok());
}

#[test]
fn causal_scope_never_claims_unscoped_edits_or_a_copied_record() {
    let fixture = fixture(953);
    let time = Arc::new(AtomicU64::new(99));
    let job = scope(&fixture, 5, "caller", 100, &time);
    let mut copied = fixture
        .store
        .read(&fixture.checkout.id, Clone::clone)
        .unwrap();
    assert!(job.with_document_mutation(&mut copied, |_| Ok(())).is_err());
    fixture
        .store
        .with_checked(&fixture.checkout.id, |doc| {
            doc.put(ROOT, "unscoped", true).unwrap();
            doc.get_heads();
            assert!(
                job.with_document_mutation(doc, |doc| {
                    doc.put(ROOT, "claimed", true).unwrap();
                    Ok(())
                })
                .is_err()
            );
            assert!(doc.get(ROOT, "claimed").unwrap().is_none());
        })
        .unwrap();
    assert!(publication(&fixture, &[]).is_err());
}

#[test]
fn causal_scope_baseline_cannot_reset_over_later_unscoped_changes() {
    let fixture = fixture(954);
    fixture
        .store
        .with_checked(&fixture.checkout.id, |doc| {
            doc.put(ROOT, "factory-default", true).unwrap();
            doc.get_heads();
        })
        .unwrap();
    assert!(
        fixture
            .store
            .require_invocation_provenance(&fixture.checkout)
            .is_err()
    );
    assert!(publication(&fixture, &[]).is_err());
}

#[test]
fn causal_scope_worker_effect_rechecks_original_caller_and_retires_same_job() {
    let fixture = fixture(955);
    let time = Arc::new(AtomicU64::new(99));
    let a = scope(&fixture, 6, "caller-a", 100, &time);
    let b = scope(&fixture, 7, "caller-b", 200, &time);
    let a_effect = a.effect_scope().unwrap();
    let b_effect = b.effect_scope().unwrap();
    let effects = Arc::new(AtomicU64::new(0));
    let written = effects.clone();
    let (ready, start) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        start.recv().unwrap();
        assert!(
            a_effect
                .with_effect(|| {
                    written.fetch_add(100, Ordering::SeqCst);
                    Ok(())
                })
                .is_err()
        );
        b_effect
            .with_effect(|| {
                written.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
    });
    time.store(100, Ordering::Release);
    ready.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    time.store(99, Ordering::Release);
    assert!(a.check().is_err());
    assert!(b.check().is_ok());
}

#[test]
fn causal_scope_observes_only_newly_applied_remote_range_without_laundering_local_edits() {
    for unscoped_local in [false, true] {
        let fixture = fixture(if unscoped_local { 961 } else { 960 });
        let time = Arc::new(AtomicU64::new(99));
        let job = scope(&fixture, 10, "current", 200, &time);
        let mut remote = fixture
            .store
            .read(&fixture.checkout.id, Clone::clone)
            .unwrap();
        let before = remote.base_heads();
        remote.set_actor(automerge::ActorId::from(b"remote-admitted".as_slice()));
        remote.put(ROOT, "remote", true).unwrap();
        remote.get_heads();
        let incoming = collab_types::DocumentMutation::Incremental(remote.save_after(&before));
        if unscoped_local {
            fixture
                .store
                .with_checked(&fixture.checkout.id, |doc| {
                    doc.put(ROOT, "unscoped-local", true).unwrap();
                    doc.get_heads();
                })
                .unwrap();
        }
        assert_eq!(
            fixture.target.owner.merge_remote(&incoming).unwrap(),
            Some(true)
        );
        let result = fixture
            .store
            .with_checked(&fixture.checkout.id, |doc| {
                job.with_document_mutation(doc, |doc| {
                    doc.put(ROOT, "after-merge", true).unwrap();
                    Ok(())
                })
            })
            .unwrap();
        assert_eq!(
            result.is_err(),
            unscoped_local,
            "only the actual newly applied input range may become known owner data"
        );
    }
}

#[test]
fn causal_scope_required_unadapted_handler_refuses_before_start_and_cancel_fences_worker() {
    let mut fixture = fixture(962);
    let now = Arc::new(AtomicU64::new(99));
    let job = scope(&fixture, 11, "caller", 200, &now);
    let encoded = tilo_actions::EncodedAction {
        type_id: "controlled.effect".into(),
        payload: vec![],
    };
    let result = crate::bound_dispatch::legacy_document::invoke_legacy_effect(
        &mut fixture.tile,
        &fixture.target,
        job.invocation(),
        tilo_actions::OperationKind::Effect,
        &encoded,
        &mut AutoCommit::new(),
        Some(&job),
    );
    assert!(result.is_err());
    assert_eq!(
        fixture.probe.starts.get(),
        0,
        "required admission never calls the legacy background handler first"
    );
    let effect = job.effect_scope().unwrap();
    job.cancel();
    let called = Arc::new(AtomicBool::new(false));
    let worker_called = called.clone();
    assert!(
        std::thread::spawn(move || effect.with_effect(|| {
            worker_called.store(true, Ordering::Release);
            Ok(())
        }))
        .join()
        .unwrap()
        .is_err()
    );
    assert!(!called.load(Ordering::Acquire));
}

struct OriginalCallbackGate {
    armed: Arc<AtomicBool>,
    deny_other_checks: Arc<AtomicBool>,
    entered: std::sync::mpsc::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

fn scope_with_original_callback_gate(
    fixture: &crate::bound_dispatch::legacy_document_tests::Fixture,
    id: u64,
) -> (ResourceInvocationScope, OriginalCallbackGate) {
    let armed = Arc::new(AtomicBool::new(false));
    let deny_other_checks = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = std::sync::Mutex::new(released);
    let callback_armed = armed.clone();
    let callback_denied = deny_other_checks.clone();
    let request = PublishedInvocationRequest {
        wire_id: id,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
        address: tilo_actions::ActionAddress::new("fixture", "write"),
        argument: None,
        actor: "original-blocked-caller".into(),
        participant: None,
    };
    let lease = PublishedInvocationLease::new(
        request,
        fixture.target.owner.broker_route().unwrap(),
        Arc::new(move || {
            if callback_armed.swap(false, Ordering::AcqRel) {
                entered_tx.send(()).unwrap();
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(tilo_test_env::hang_bound())
                    .unwrap();
                // The original positive policy sample finishes after another
                // owner/worker has retired this exact shared invocation.
                return Ok(());
            }
            if callback_denied.load(Ordering::Acquire) {
                return Err("original caller denied on concurrent check".into());
            }
            Ok(())
        }),
    )
    .with_owner_token(Arc::new(OriginalCaller {
        actor: "original-blocked-caller".into(),
        expiry: 200,
    }))
    .unwrap();
    let job = ResourceInvocationScope::for_owner(fixture.target.owner.clone(), lease).unwrap();
    (
        job,
        OriginalCallbackGate {
            armed,
            deny_other_checks,
            entered,
            release,
        },
    )
}

#[test]
fn causal_scope_cancel_during_original_callback_fences_worker_effect() {
    for cancel in [false, true] {
        let fixture = fixture(if cancel { 964 } else { 963 });
        let (job, gate) = scope_with_original_callback_gate(&fixture, 12);
        let effect = job.effect_scope().unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let worker_called = called.clone();
        gate.armed.store(true, Ordering::Release);
        let worker = std::thread::spawn(move || {
            effect.with_effect(|| {
                worker_called.store(true, Ordering::Release);
                Ok(())
            })
        });
        tilo_test_env::recv("the gate to be entered", &gate.entered);
        if cancel {
            job.cancel();
        }
        gate.release.send(()).unwrap();
        let result = worker.join().unwrap();
        assert_eq!(
            called.load(Ordering::Acquire),
            !cancel,
            "cancellation completed before the original callback returned"
        );
        assert_eq!(result.is_err(), cancel);
        assert_eq!(job.check().is_err(), cancel);
    }
}

#[test]
fn causal_scope_cancel_during_original_callback_fences_owner_check() {
    let fixture = fixture(965);
    let (job, gate) = scope_with_original_callback_gate(&fixture, 13);
    let concurrent = job.effect_scope().unwrap();
    gate.armed.store(true, Ordering::Release);
    let deny = gate.deny_other_checks.clone();
    let restore = deny.clone();
    let worker = std::thread::spawn(move || {
        tilo_test_env::recv("the gate to be entered", &gate.entered);
        deny.store(true, Ordering::Release);
        let result: Result<(), String> =
            concurrent.with_effect(|| panic!("denied worker effect started"));
        gate.release.send(()).unwrap();
        result
    });
    let result = job.check();
    assert!(worker.join().unwrap().is_err());
    assert!(
        result.is_err(),
        "concurrent worker retirement completed before the owner's callback returned"
    );
    restore.store(false, Ordering::Release);
    assert!(
        job.check().is_err(),
        "retired original custody cannot revive"
    );
}

#[test]
fn invoker_scope_nests_and_gates_remote_view_operations() {
    use tilo_actions::{ActionContract, ActionInvoker, ParticipantId};
    let participant = ParticipantId::for_agent_task("t").unwrap();
    let own_view = ActionContract::view();
    let mut participant_view = ActionContract::view();
    participant_view
        .tags
        .push(tilo_actions::PARTICIPANT_VIEW_TAG.to_owned());
    assert_eq!(super::current_invoker(), None);
    // The local person may always change its own view.
    assert!(super::view_operation_permitted("me", "me", &own_view));
    // A remote actor outside any delivered invocation may not.
    assert!(!super::view_operation_permitted(
        "agent:a",
        "me",
        &participant_view
    ));
    {
        let _outer = super::InvokerScope::enter(
            ActionInvoker::actor("agent:a").with_participant(participant.clone()),
        );
        assert_eq!(super::current_participant(), Some(participant.clone()));
        assert!(super::view_operation_permitted(
            "agent:a",
            "me",
            &participant_view
        ));
        assert!(
            !super::view_operation_permitted("agent:a", "me", &own_view),
            "an untagged View op would move somebody else's view"
        );
        assert!(
            !super::view_operation_permitted("agent:b", "me", &participant_view),
            "only the current invoker's own view"
        );
        {
            let _inner = super::InvokerScope::enter(ActionInvoker::actor("agent:a"));
            assert_eq!(super::current_participant(), None);
            assert!(!super::view_operation_permitted(
                "agent:a",
                "me",
                &participant_view
            ));
        }
        assert_eq!(super::current_participant(), Some(participant));
    }
    assert_eq!(super::current_invoker(), None);
}

/// Reading a document changes nobody's view: an admitted agent may run the
/// app's declared read, though not another View operation.
#[test]
fn a_declared_document_read_is_no_view_move_for_a_remote_actor() {
    use tilo_actions::ActionContract;
    let mut read = ActionContract::view();
    read.tags.push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
    assert!(super::view_operation_permitted("agent:a", "me", &read));
    assert!(!super::view_operation_permitted(
        "agent:a",
        "me",
        &ActionContract::view()
    ));
}

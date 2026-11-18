use super::*;
use automerge::{ROOT, ReadDoc};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
struct OriginalCaller {
    actor: tilo_actions::Actor,
    expiry: u64,
}

type Fixture = crate::history_mode::owner::ResourceOwnerFixture;

fn fixture(id: u64) -> Fixture {
    Fixture::in_workspace(id, "77777777-7777-4777-8777-777777777777")
}

fn scope(
    fixture: &Fixture,
    id: u64,
    actor: &str,
    expiry: u64,
    time: &Arc<AtomicU64>,
) -> ResourceInvocationScope {
    let caller = OriginalCaller {
        actor: tilo_actions::Actor::Member(tilo_actions::MemberId::person(actor)),
        expiry,
    };
    let sampled = time.clone();
    let request = PublishedInvocationRequest {
        wire_id: id,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
        address: tilo_actions::ActionAddress::new("fixture", "write"),
        argument: None,
        actor: tilo_actions::Actor::Member(tilo_actions::MemberId::person(actor)),
        presence: None,
    };
    let lease = PublishedInvocationLease::new(
        request,
        fixture.owner.broker_route().unwrap(),
        Arc::new(move || {
            (sampled.load(Ordering::Acquire) < expiry)
                .then_some(())
                .ok_or_else(|| "original caller grant expired".into())
        }),
    )
    .with_owner_token(Arc::new(caller))
    .unwrap();
    ResourceInvocationScope::for_owner(fixture.owner.clone(), lease).unwrap()
}

fn publication(
    fixture: &Fixture,
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

tilo_test_env::scenario! {
    fn a_causal_scope_claims_only_its_own_changes_and_expires_with_its_caller;

    phase causal_scope_nested_contributors_survive_partial_error_and_match_actual_range;
    {
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
                seen.insert(original.actor.id().to_owned());
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

    phase causal_scope_two_retained_jobs_expire_independently_without_resurrection;
    {
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
            tilo_actions::Actor::Member(tilo_actions::MemberId::person("caller-b"))
        );
        time.store(99, Ordering::Release);
        assert!(
            jobs[0].check().is_err(),
            "a terminally denied job cannot resurrect"
        );
        assert!(jobs[1].check().is_ok());
    }

    phase causal_scope_never_claims_unscoped_edits_or_a_copied_record;
    {
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

    phase causal_scope_baseline_cannot_reset_over_later_unscoped_changes;
    {
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

    phase causal_scope_worker_effect_rechecks_original_caller_and_retires_same_job;
    {
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

    phase causal_scope_observes_only_newly_applied_remote_range_without_laundering_local_edits;
    {
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
            let incoming = remote.save_after(&before);
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
                fixture
                    .owner
                    .merge_remote(
                        crate::collaboration::Incoming::Changes(&incoming),
                        &mut automerge::AutoCommit::new()
                    )
                    .unwrap()
                    .changed,
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

    phase causal_scope_cancel_during_original_callback_fences_worker_effect;
    {
        for cancel in [false, true] {
            let fixture = fixture(if cancel { 964 } else { 963 });
            let (job, check) = scope_with_original_callback_hold(&fixture, 12);
            let effect = job.effect_scope().unwrap();
            let called = Arc::new(AtomicBool::new(false));
            let worker_called = called.clone();
            check.armed.store(true, Ordering::Release);
            let worker = std::thread::spawn(move || {
                effect.with_effect(|| {
                    worker_called.store(true, Ordering::Release);
                    Ok(())
                })
            });
            tilo_test_env::recv("the check to be entered", &check.entered);
            if cancel {
                job.cancel();
            }
            check.release.send(()).unwrap();
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

    phase causal_scope_cancel_during_original_callback_fences_owner_check;
    {
        let fixture = fixture(965);
        let (job, check) = scope_with_original_callback_hold(&fixture, 13);
        let concurrent = job.effect_scope().unwrap();
        check.armed.store(true, Ordering::Release);
        let deny = check.deny_other_checks.clone();
        let restore = deny.clone();
        let worker = std::thread::spawn(move || {
            tilo_test_env::recv("the check to be entered", &check.entered);
            deny.store(true, Ordering::Release);
            let result: Result<(), String> =
                concurrent.with_effect(|| panic!("denied worker effect started"));
            check.release.send(()).unwrap();
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
            "retired original ownership cannot revive"
        );
    }
}

struct OriginalCallbackHold {
    armed: Arc<AtomicBool>,
    deny_other_checks: Arc<AtomicBool>,
    entered: std::sync::mpsc::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

fn scope_with_original_callback_hold(
    fixture: &Fixture,
    id: u64,
) -> (ResourceInvocationScope, OriginalCallbackHold) {
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
        actor: tilo_actions::Actor::Member(tilo_actions::MemberId::person(
            "original-blocked-caller",
        )),
        presence: None,
    };
    let lease = PublishedInvocationLease::new(
        request,
        fixture.owner.broker_route().unwrap(),
        Arc::new(move || {
            if callback_armed.swap(false, Ordering::AcqRel) {
                entered_tx.send(()).unwrap();
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(tilo_test_env::hang_bound())
                    .unwrap();
                return Ok(());
            }
            if callback_denied.load(Ordering::Acquire) {
                return Err("original caller denied on concurrent check".into());
            }
            Ok(())
        }),
    )
    .with_owner_token(Arc::new(OriginalCaller {
        actor: tilo_actions::Actor::Member(tilo_actions::MemberId::person(
            "original-blocked-caller",
        )),
        expiry: 200,
    }))
    .unwrap();
    let job = ResourceInvocationScope::for_owner(fixture.owner.clone(), lease).unwrap();
    (
        job,
        OriginalCallbackHold {
            armed,
            deny_other_checks,
            entered,
            release,
        },
    )
}

tilo_test_env::scenario! {
    fn a_remote_actors_view_operations_are_guarded_by_the_invoker_scope;

    phase invoker_scope_nests_and_checks_remote_view_operations;
    {
        use tilo_actions::{ActionContract, ActionInvoker, PresenceId};
        let presence = PresenceId::new("agent-a-view").unwrap();
        let own_view = ActionContract::view();
        let mut presence_view = ActionContract::view();
        presence_view
            .tags
            .push(tilo_actions::PRESENCE_VIEW_TAG.to_owned());
        assert_eq!(super::current_invoker(), None);
        // The local person may always change its own view.
        assert!(super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::person("me")), &tilo_actions::MemberId::person("me"), &own_view));
        // A remote actor outside any delivered invocation may not.
        assert!(!super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent("a")), &tilo_actions::MemberId::person("me"),
            &presence_view
        ));
        {
            let _outer = super::InvokerScope::enter(
                ActionInvoker::agent("a").with_presence(presence.clone()),
            );
            assert_eq!(super::current_presence(), Some(presence.clone()));
            assert!(super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent("a")), &tilo_actions::MemberId::person("me"),
                &presence_view
            ));
            assert!(
                !super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent("a")), &tilo_actions::MemberId::person("me"), &own_view),
                "an untagged View op would move somebody else's view"
            );
            assert!(
                !super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent("b")), &tilo_actions::MemberId::person("me"), &presence_view),
                "only the current invoker's own view"
            );
            {
                let _inner = super::InvokerScope::enter(ActionInvoker::agent("a"));
                assert_eq!(super::current_presence(), None);
                assert!(!super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent("a")), &tilo_actions::MemberId::person("me"),
                    &presence_view
                ));
            }
            assert_eq!(super::current_presence(), Some(presence));
        }
        assert_eq!(super::current_invoker(), None);
    }

    phase a_declared_document_read_is_no_view_move_for_a_remote_actor;
    // Reading a document changes nobody's view: an admitted agent may run the app's declared read, though
    // not another View operation.
    {
        use tilo_actions::ActionContract;
        let mut read = ActionContract::view();
        read.tags.push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
        assert!(super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent("a")), &tilo_actions::MemberId::person("me"), &read));
        assert!(!super::view_operation_permitted(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent("a")), &tilo_actions::MemberId::person("me"),
            &ActionContract::view()
        ));
    }
}

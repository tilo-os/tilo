//! The host's scoped chooser and URI effects recheck the original admitted
//! caller on the worker, immediately before the effect.

use crate::invocation_scope::{
    PublishedInvocationLease, PublishedInvocationRequest, ResourceInvocationScope,
};
use crate::portal::{
    PickRequest, PortalOutcome, open_uri_scoped, scoped_picker_fixture, scoped_uri_fixture,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
#[test]
fn writer_scoped_picker_rechecks_expired_admitted_job_on_worker() {
    for expired in [false, true] {
        let fixture = crate::bound_dispatch::legacy_document_tests::Fixture::in_room(
            1960 + u64::from(expired),
            "77777777-7777-4777-8777-777777777777",
        );
        fixture
            .store
            .require_invocation_provenance(&fixture.checkout)
            .unwrap();
        let time = Arc::new(AtomicU64::new(99));
        let sampled = time.clone();
        let lease = PublishedInvocationLease::new(
            PublishedInvocationRequest {
                wire_id: 1960,
                invocation: tilo_actions::InvocationId::new(),
                address: tilo_actions::ActionAddress::new("fixture", "choose"),
                argument: None,
                actor: "original-caller".into(),
                participant: None,
            },
            fixture.target.owner.broker_route().unwrap(),
            Arc::new(move || {
                (sampled.load(Ordering::Acquire) < 100)
                    .then_some(())
                    .ok_or_else(|| "original caller expired".into())
            }),
        )
        .with_owner_token(Arc::new("retained fixture caller"))
        .unwrap();
        let scope =
            ResourceInvocationScope::for_owner(fixture.target.owner.clone(), lease).unwrap();
        let effect = scope.effect_scope().unwrap();
        if expired {
            time.store(100, Ordering::Release);
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let fixture = scoped_picker_fixture(Arc::new(move |_| {
            observed.fetch_add(1, Ordering::AcqRel);
            PortalOutcome::Cancelled
        }))
        .unwrap();
        let receiver = PickRequest::open("Original caller", "Open")
            .thread_name("writer-scope-test")
            .send_scoped(effect);
        drop(fixture);
        let outcome = tilo_test_env::recv("the host effect outcome", &receiver);
        assert_eq!(
            calls.load(Ordering::Acquire),
            usize::from(!expired),
            "expired admitted job must not enter actual chooser effect"
        );
        assert_eq!(matches!(outcome, PortalOutcome::Failed(_)), expired);
    }
}

fn worker_case(expired: bool) {
    let fixture = crate::bound_dispatch::legacy_document_tests::Fixture::in_room(
        1970 + u64::from(expired),
        "77777777-7777-4777-8777-777777777777",
    );
    fixture
        .store
        .require_invocation_provenance(&fixture.checkout)
        .unwrap();
    let time = Arc::new(AtomicU64::new(99));
    let sampled = time.clone();
    let lease = PublishedInvocationLease::new(
        PublishedInvocationRequest {
            wire_id: 1970,
            invocation: tilo_actions::InvocationId::new(),
            address: tilo_actions::ActionAddress::new("fixture", "open-uri"),
            argument: None,
            actor: "original-caller".into(),
            participant: None,
        },
        fixture.target.owner.broker_route().unwrap(),
        Arc::new(move || {
            (sampled.load(Ordering::Acquire) < 100)
                .then_some(())
                .ok_or_else(|| "original caller expired".into())
        }),
    )
    .with_owner_token(Arc::new("retained fixture URI caller"))
    .unwrap();
    let scope = ResourceInvocationScope::for_owner(fixture.target.owner.clone(), lease).unwrap();
    let effect = scope.effect_scope().unwrap();
    if expired {
        time.store(100, Ordering::Release);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let owner_thread = std::thread::current().id();
    let guard = scoped_uri_fixture(Arc::new(move |uri| {
        assert_eq!(uri, "https://example.test/original-caller");
        assert_ne!(std::thread::current().id(), owner_thread);
        observed.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }))
    .unwrap();
    let result = open_uri_scoped("https://example.test/original-caller".into(), effect);
    drop(guard); // The dispatched request must own the captured backend.
    let result = tilo_test_env::recv("the dispatched request's result", &result);
    assert_eq!(calls.load(Ordering::Acquire), usize::from(!expired));
    if expired {
        assert_eq!(result.unwrap_err(), "original caller expired");
    } else {
        result.unwrap();
    }
}
#[test]
fn scoped_uri_live_original_caller_reaches_worker_backend() {
    worker_case(false);
}
#[test]
fn scoped_uri_expired_admitted_caller_never_reaches_worker_backend() {
    worker_case(true);
}

use super::*;
use crate::{delivery_http::tests::Fixture, https::tests::data_test_request};
use tokio::time::Instant;

fn attach(f: &mut Fixture) -> (crate::https::Ingress, crate::https::IngressReceiver) {
    let pair = crate::https::ingress_channel();
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.attach_data_resources(&pair.0.host_bytes())
        })
        .unwrap()
        .unwrap();
    pair
}

fn original(
    f: &Fixture,
    ingress: &crate::https::Ingress,
    exact: &Request<'_>,
    id: [u8; 16],
    digest: [u8; 32],
) -> (
    RequestProof,
    BufferedRequest,
    crate::https::tests::HeldPollTestClient,
) {
    let proof = RequestProof::sign(
        id,
        digest,
        exact,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (request, client) = data_test_request(
        ingress,
        match exact.method() {
            tilo_relay::delivery_v3::http_proof::Method::Get => "GET",
            tilo_relay::delivery_v3::http_proof::Method::Post => "POST",
            _ => panic!("unsupported test request method"),
        },
        exact.target(),
        exact.body(),
        &proof.encode().unwrap(),
        Instant::now() + std::time::Duration::from_secs(20),
    );
    (proof, request, client)
}

#[cfg(feature = "test-support")]
#[test]
fn native_proof_admission_uses_owner_200_after_wall_rolls_back_to_50() {
    let mut f = Fixture::new();
    let (clock, wall) = qos_action_broker::peer_actions::OwnerPolicyClock::for_test(f.now);
    f.current = CommittedAuthority::new_with_test_clock(&f.owner, &clock);
    let (ingress, _receiver) = attach(&mut f);
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [211; 32]);
    let exact = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let (proof, request, _client) = original(&f, &ingress, &exact, id, digest);
    wall.set_wall_ns(f.now + 200_000_000_000);
    assert_eq!(clock.now_ns().unwrap(), f.now + 200_000_000_000);
    wall.set_wall_ns(f.now + 50_000_000_000);
    let result = registry.admit_revalidating(
        &f.owner,
        &f.current,
        &ingress.host_bytes(),
        OwnerDataOperation::Replay,
        &proof,
        &exact,
        &request,
    );
    let Err(error) = result else {
        panic!("HTTP proof expired at owner time 200 was admitted using a lower wall sample");
    };
    assert!(
        matches!(
            error.downcast_ref::<http_proof::ProofError>(),
            Some(http_proof::ProofError::Lifetime)
        ),
        "expected expired proof while descriptor/session/cert remain current: {error}"
    );
}

#[cfg(feature = "test-support")]
#[test]
fn native_proof_context_uses_its_final_checked_sample_without_consuming_expired_nonce() {
    let mut f = Fixture::new();
    let (clock, control) = qos_action_broker::peer_actions::OwnerPolicyClock::for_test(f.now);
    f.current = CommittedAuthority::new_with_test_clock(&f.owner, &clock);
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [214; 32]);
    let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let proof = RequestProof::sign(
        id,
        digest,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let guard = f.current.read().unwrap();
    let early = registry
        .native_clock()
        .unwrap()
        .sample(guard.view())
        .unwrap();
    assert_eq!(early, f.now);
    // Real preflight completed. The same owner clock then advances before the
    // actual context constructor checks its retained origin and builds proof policy.
    control.set_wall_ns(f.now + 200_000_000_000);
    assert_eq!(clock.now_ns().unwrap(), f.now + 200_000_000_000);
    control.set_wall_ns(f.now + 50_000_000_000);
    let identity = registry.live.get(&id).unwrap().identity;
    let context_clock = registry.native_clock().unwrap();
    let context = registry
        .context_clock(&f.owner, guard.view(), identity, &context_clock)
        .unwrap();
    let result = registry.admit_in(&proof, &request, &context.verification());
    let Err(error) = result else {
        panic!("expired proof admitted after context observed a newer owner time");
    };
    assert!(matches!(
        error.downcast_ref::<http_proof::ProofError>(),
        Some(http_proof::ProofError::Lifetime)
    ));
    assert_eq!(context.now, f.now + 200_000_000_000);
    assert_eq!(registry.high_water, context.now);
    // Failure must not consume the nonce. A fresh proof using that exact nonce
    // and the still-current session can be admitted at the later owner time.
    let (version, proof_id, nonce, _, _, _): (u16, [u8; 16], [u8; 32], u64, u64, Vec<u8>) =
        postcard::from_bytes(&proof.encode().unwrap()).unwrap();
    let expires = context.now + 30_000_000_000;
    let signed = postcard::to_stdvec(&(
        version,
        proof_id,
        digest,
        request.method(),
        request.target(),
        *blake3::hash(request.body()).as_bytes(),
        nonce,
        context.now,
        expires,
    ))
    .unwrap();
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-delivery-v3-http-request\0");
    hash.update(&signed);
    let signature =
        ed25519_dalek::Signer::sign(&f.owner.identity.native, hash.finalize().as_bytes());
    let wire = postcard::to_stdvec(&(
        version,
        proof_id,
        nonce,
        context.now,
        expires,
        signature.to_bytes().as_slice(),
    ))
    .unwrap();
    let fresh = RequestProof::decode(&wire).unwrap();
    assert!(
        registry
            .admit_in(&fresh, &request, &context.verification())
            .is_ok()
    );
    let replay = registry
        .admit_in(&fresh, &request, &context.verification())
        .err()
        .unwrap();
    assert!(matches!(
        replay.downcast_ref::<http_proof::ProofError>(),
        Some(http_proof::ProofError::Replay)
    ));
}

#[test]
fn native_proof_admission_refuses_same_peer_with_a_different_owner_clock() {
    let mut f = Fixture::new();
    let (ingress, _receiver) = attach(&mut f);
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [212; 32]);
    let exact = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let (proof, request, _client) = original(&f, &ingress, &exact, id, digest);
    let unrelated = CommittedAuthority::new(&f.owner);
    assert!(
        registry
            .admit_revalidating(
                &f.owner,
                &unrelated,
                &ingress.host_bytes(),
                OwnerDataOperation::Replay,
                &proof,
                &exact,
                &request,
            )
            .is_err(),
        "matching peer bytes cannot substitute a different owner clock origin"
    );
    assert!(
        registry
            .admit_revalidating(
                &f.owner,
                &f.current,
                &ingress.host_bytes(),
                OwnerDataOperation::Replay,
                &proof,
                &exact,
                &request,
            )
            .is_ok(),
        "foreign clock rejection must not consume the genuine proof nonce"
    );
}

#[test]
fn native_proof_admission_delayed_original_survives_progress_but_not_recovery_origin() {
    let mut f = Fixture::new();
    let (ingress, _receiver) = attach(&mut f);
    let budget = ingress.host_bytes();
    let mut registry = HttpSessions::new_current(&f.owner, &f.current).unwrap();
    // Use the actual current-time mutual-authentication entry points.
    let nonce = [213; 32];
    let server = auth::ServerProof::decode(
        &registry
            .challenge_current(&f.owner, &f.current, &f.request(nonce))
            .unwrap(),
    )
    .unwrap();
    f.now = f.current.policy_clock().unwrap().now_ns().unwrap();
    let client = f.client(&server, nonce);
    let ready = registry
        .finish_current(&f.owner, &f.current, &client.encode().unwrap())
        .unwrap();
    let (_, id, digest, _, _): (u16, [u8; 16], [u8; 32], u64, u64) =
        postcard::from_bytes(&ready).unwrap();
    let exact = Request::new("POST", "/peer/v3/state", b"original").unwrap();
    let (proof, request, _client) = original(&f, &ingress, &exact, id, digest);
    let ticket = {
        let (admitted, check, _turn) = registry
            .admit_revalidating(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::State {
                    input_len: exact.body().len(),
                },
                &proof,
                &exact,
                &request,
            )
            .unwrap();
        check.capture_continuation(&admitted, &request).unwrap()
    };
    let pending = ticket.park(request).unwrap();
    let original_time = registry.native_time.clone().unwrap();
    let mut scope = f
        .owner
        .prepare_owner_scope(
            &budget,
            crate::owner::resources::OwnerOperation::WorkerCommit { input_len: 8 },
        )
        .unwrap();
    f.owner
        .with_owner_scope(&mut scope, &f.current, |owner| {
            owner.queue_pending("proof-progress", b"progress", false)
        })
        .unwrap()
        .unwrap();
    assert!(
        registry
            .resume_operation(&f.owner, &f.current, &budget, &pending)
            .is_ok()
    );
    crate::committed_authority::enter_test_recovery(&f.current);
    let mut scope = f
        .owner
        .prepare_owner_scope(
            &budget,
            crate::owner::resources::OwnerOperation::WorkerCommit { input_len: 8 },
        )
        .unwrap();
    f.owner
        .with_owner_scope(&mut scope, &f.current, |owner| {
            owner.queue_pending("proof-recovered", b"progress", false)
        })
        .unwrap()
        .unwrap();
    let fresh = f.current.read().unwrap().view().policy_time().unwrap();
    assert!(!fresh.same_origin(&original_time));
    assert!(original_time.now_ns().is_err());
    assert!(
        registry
            .resume_operation(&f.owner, &f.current, &budget, &pending)
            .is_err(),
        "delayed original request rebound to recovered owner's fresh origin"
    );
}

#[test]
fn current_operation_clock_rejection_advances_shared_high_water_after_authority_failure() {
    let mut f = Fixture::new();
    let other = Fixture::new();
    let (ingress, _receiver) = attach(&mut f);
    let budget = ingress.host_bytes();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [112; 32]);
    let exact = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let (proof, original, _client) = original(&f, &ingress, &exact, id, digest);
    let clock = TestClock::new(f.now);
    let (admitted, mut check, _turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &budget,
            OwnerDataOperation::Replay,
            &proof,
            &exact,
            &original,
            &clock,
        )
        .unwrap();
    clock.set_wall(f.now + 10);
    let other_guard = other.current.read().unwrap();
    assert!(
        check
            .check(&other.owner, other_guard.view(), admitted.session())
            .is_err()
    );
    clock.set_wall(f.now + 9);
    let guard = f.current.read().unwrap();
    assert_eq!(
        check
            .check(&f.owner, guard.view(), admitted.session())
            .unwrap_err()
            .to_string(),
        "clock rollback"
    );
    drop(admitted);
    drop(check);
    assert_eq!(registry.high_water, f.now + 10);
}

#[test]
fn current_operation_rejects_original_byte_and_proof_substitution_without_consuming_nonce() {
    let mut f = Fixture::new();
    let (ingress, _receiver) = attach(&mut f);
    let budget = ingress.host_bytes();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [113; 32]);
    let exact = Request::new("POST", "/peer/v3/state", b"original").unwrap();
    let changed = Request::new("POST", "/peer/v3/state", b"changed").unwrap();
    let (proof, original, _client) = original(&f, &ingress, &exact, id, digest);
    let changed_proof = RequestProof::sign(
        id,
        digest,
        &changed,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let other_proof = RequestProof::sign(
        id,
        digest,
        &exact,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let clock = TestClock::new(f.now);
    for (candidate, request) in [(&changed_proof, &changed), (&other_proof, &exact)] {
        assert!(
            registry
                .admit_revalidating_at(
                    &f.owner,
                    &f.current,
                    &budget,
                    OwnerDataOperation::State {
                        input_len: request.body().len()
                    },
                    candidate,
                    request,
                    &original,
                    &clock
                )
                .is_err()
        );
    }
    assert!(
        registry
            .admit_revalidating_at(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::State {
                    input_len: exact.body().len()
                },
                &proof,
                &exact,
                &original,
                &clock
            )
            .is_ok()
    );
}

#[test]
fn current_operation_observes_sender_drop_cancel_deadline_and_session_expiry() {
    for mode in 0..4 {
        let mut f = Fixture::new();
        let (ingress, _receiver) = attach(&mut f);
        let budget = ingress.host_bytes();
        let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        let (id, digest) = f.handshake(&mut registry, [114 + mode; 32]);
        let exact = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let (proof, original, client) = original(&f, &ingress, &exact, id, digest);
        let clock = TestClock::new(f.now);
        let (admitted, mut check, _turn) = registry
            .admit_revalidating_at(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::Replay,
                &proof,
                &exact,
                &original,
                &clock,
            )
            .unwrap();
        let guard = f.current.read().unwrap();
        check
            .check(&f.owner, guard.view(), admitted.session())
            .unwrap();
        let mut client = Some(client);
        match mode {
            0 => drop(client.take()),
            1 => client.as_ref().unwrap().cancel(),
            2 => clock.advance_monotonic(std::time::Duration::from_secs(21)),
            _ => clock.set_wall(admitted.session().expires_at_ns()),
        }
        assert!(
            check
                .check(&f.owner, guard.view(), admitted.session())
                .is_err(),
            "mode {mode}"
        );
    }
}

#[test]
fn current_poll_resume_requires_original_registry_entry_boot_descriptor_and_lifecycle() {
    for mode in 0..6 {
        let mut f = Fixture::new();
        let (ingress, _receiver) = attach(&mut f);
        let budget = ingress.host_bytes();
        let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        let (id, digest) = f.handshake(&mut registry, [121 + mode; 32]);
        let exact = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let (proof, original, client) = original(&f, &ingress, &exact, id, digest);
        let clock = TestClock::new(f.now);
        let (admitted, check, turn) = registry
            .admit_revalidating_at(
                &f.owner,
                &f.current,
                &budget,
                OwnerDataOperation::Replay,
                &proof,
                &exact,
                &original,
                &clock,
            )
            .unwrap();
        let mut polls = poll::Polls::new();
        let ticket = polls
            .capture(&admitted, &original, check.entry_serial())
            .unwrap();
        drop(admitted);
        drop(check);
        drop(turn);
        let poll_id = polls.park(ticket, original).unwrap();
        let pending = polls.take(poll_id).unwrap();
        match mode {
            1 => registry.live.get_mut(&id).unwrap().entry += 1,
            2 => registry.boot[0] ^= 1,
            3 => registry.descriptor[0] ^= 1,
            4 => client.cancel(),
            5 => clock.advance_monotonic(std::time::Duration::from_secs(21)),
            _ => (),
        }
        let resumed = registry.resume_poll_at(&f.owner, &f.current, &budget, &pending, &clock);
        if mode == 0 {
            let (session, mut check, mut turn) = resumed.unwrap();
            let guard = f.current.read().unwrap();
            let now = check.check(&f.owner, guard.view(), session).unwrap();
            f.owner
                .data_source(&mut turn, guard.view(), session, now)
                .unwrap();
        } else {
            assert!(resumed.is_err(), "mode {mode}");
        }
        assert!(
            registry
                .admit(&f.owner, &f.current, &proof, &exact, f.now)
                .is_err()
        );
    }
}

#[test]
fn current_operation_uses_real_turn_and_revalidates_without_second_nonce_admission() {
    let mut f = Fixture::new();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    f.current
        .with_owner(&mut f.owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [111; 32]);
    let exact = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let proof = RequestProof::sign(
        id,
        digest,
        &exact,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let (original, _client) = data_test_request(
        &ingress,
        "GET",
        exact.target(),
        b"",
        &proof.encode().unwrap(),
        Instant::now() + std::time::Duration::from_secs(20),
    );
    let clock = TestClock::new(f.now);
    let (admitted, mut check, mut turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &budget,
            OwnerDataOperation::Replay,
            &proof,
            &exact,
            &original,
            &clock,
        )
        .expect("real current operation admission");
    assert!(check.entry_serial() > 0);
    f.current
        .with_owner_view(&mut f.owner, |view, owner| -> Result<()> {
            let now = check.check(owner, view, admitted.session())?;
            owner.data_source(&mut turn, view, admitted.session(), now)?;
            Ok(())
        })
        .unwrap()
        .unwrap();
    clock.set_wall(f.now + 1);
    let guard = f.current.read().unwrap();
    assert_eq!(
        check
            .check(&f.owner, guard.view(), admitted.session())
            .unwrap(),
        f.now + 1
    );
    drop(guard);
    drop(admitted);
    drop(check);
    drop(turn);
    assert!(
        registry
            .admit(&f.owner, &f.current, &proof, &exact, f.now + 1)
            .is_err()
    );
}

#[test]
fn current_poll_rejects_actual_owner_committed_descriptor_renewal() {
    let mut f = Fixture::new();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [131; 32]);
    let exact = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
    let (proof, original, _client) = original(&f, &ingress, &exact, id, digest);
    let entry = registry.live.get(&id).unwrap().entry;
    let admitted = registry
        .admit(&f.owner, &f.current, &proof, &exact, f.now)
        .unwrap();
    let mut polls = poll::Polls::new();
    let ticket = polls.capture(&admitted, &original, entry).unwrap();
    drop(admitted);
    let poll_id = polls.park(ticket, original).unwrap();
    let pending = polls.take(poll_id).unwrap();
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9444", 0, at(f.now + 1))
        })
        .unwrap()
        .unwrap();
    f.current
        .with_owner(&mut f.owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    let guard = f.current.read().unwrap();
    let replacement = f
        .owner
        .verified_delivery_transport(
            guard.view(),
            at(f.now + 1),
            crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
        )
        .unwrap();
    assert_eq!(replacement.origin(), "https://localhost:9444");
    assert_ne!(replacement.descriptor_digest(), registry.descriptor);
    drop(guard);
    let clock = TestClock::new(f.now + 1);
    let failure = registry
        .resume_poll_at(&f.owner, &f.current, &budget, &pending, &clock)
        .err()
        .expect("old pending descriptor must fail");
    assert_eq!(failure.to_string(), "descriptor replaced");
}

#[test]
fn operation_continuation_captures_only_the_original_admitted_lifecycle() {
    let mut f = Fixture::new();
    let (ingress, _receiver) = attach(&mut f);
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [161; 32]);
    let exact = Request::new("POST", "/peer/v3/state", b"original").unwrap();
    let (proof, original, _client) = original(&f, &ingress, &exact, id, digest);
    let (other, _other_client) = data_test_request(
        &ingress,
        "POST",
        exact.target(),
        exact.body(),
        &proof.encode().unwrap(),
        original.deadline(),
    );
    let clock = TestClock::new(f.now);
    let (admitted, check, _turn) = registry
        .admit_revalidating_at(
            &f.owner,
            &f.current,
            &ingress.host_bytes(),
            OwnerDataOperation::State {
                input_len: exact.body().len(),
            },
            &proof,
            &exact,
            &original,
            &clock,
        )
        .unwrap();
    assert!(check.capture_continuation(&admitted, &other).is_err());
    let wrong_lifecycle = check.capture_continuation(&admitted, &original).unwrap();
    assert!(wrong_lifecycle.park(other).is_err());
    let original_lifecycle = check.capture_continuation(&admitted, &original).unwrap();
    drop(admitted);
    drop(check);
    let _pending = original_lifecycle.park(original).unwrap();
}

#[test]
fn operation_continuation_revalidates_without_second_nonce_and_fences_entry_cancel_expiry() {
    for mode in 0..5 {
        let mut f = Fixture::new();
        let (ingress, _receiver) = attach(&mut f);
        let budget = ingress.host_bytes();
        let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        let (id, digest) = f.handshake(&mut registry, [162 + mode; 32]);
        let exact = Request::new("POST", "/peer/v3/state", b"original").unwrap();
        let operation = OwnerDataOperation::State {
            input_len: exact.body().len(),
        };
        let (proof, original, client) = original(&f, &ingress, &exact, id, digest);
        let clock = TestClock::new(f.now);
        let (admitted, check, turn) = registry
            .admit_revalidating_at(
                &f.owner, &f.current, &budget, operation, &proof, &exact, &original, &clock,
            )
            .unwrap();
        let ticket = check.capture_continuation(&admitted, &original).unwrap();
        drop(admitted);
        drop(check);
        drop(turn);
        let pending = ticket.park(original).unwrap();
        match mode {
            1 => registry.live.get_mut(&id).unwrap().entry += 1,
            2 => {
                client.cancel();
            }
            3 => clock.set_wall(f.now + 900_000_000_000),
            4 => registry.boot = [99; 16],
            _ => {}
        }
        let result = registry.resume_operation_at(&f.owner, &f.current, &budget, &pending, &clock);
        if mode == 0 {
            let (session, mut check, _turn) = result.unwrap();
            let guard = f.current.read().unwrap();
            check.check(&f.owner, guard.view(), session).unwrap();
            drop(guard);
            drop(check);
            assert!(
                registry
                    .admit_revalidating_at(
                        &f.owner,
                        &f.current,
                        &budget,
                        operation,
                        &proof,
                        &exact,
                        pending.original(),
                        &clock
                    )
                    .is_err(),
                "original nonce was consumed exactly once"
            );
        } else {
            assert!(result.is_err(), "mode {mode}");
        }
    }
}

#[test]
fn context_cache_reuses_exact_generation_and_refuses_outside_signed_windows() {
    let mut f = Fixture::new();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    f.current
        .with_owner(&mut f.owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, _digest) = f.handshake(&mut registry, [151; 32]);
    let identity = registry.live.get(&id).unwrap().identity;
    let pins = ContextPins::capture(&registry, identity);
    let mut high_water = f.now;
    // Uncached derivation, then a cached one: identical scope and credentials.
    let fresh = {
        let guard = f.current.read().unwrap();
        let context = pins
            .context(
                &f.owner,
                guard.view(),
                &Clock::Fixed(f.now),
                &mut high_water,
            )
            .unwrap();
        (context.binding.clone(), context.device.clone())
    };
    assert!(registry.context_cache.len() >= 1, "derivation is memoized");
    let cached = {
        let guard = f.current.read().unwrap();
        let context = pins
            .context(
                &f.owner,
                guard.view(),
                &Clock::Fixed(f.now + 1),
                &mut high_water,
            )
            .unwrap();
        assert_eq!(
            context.now,
            f.now + 1,
            "every use keeps its own checked sample"
        );
        (context.binding.clone(), context.device.clone())
    };
    assert_eq!(fresh, cached);
    // Past the signed descriptor window the cache never answers: the full
    // derivation runs again and refuses.
    let late = f.now + 901_000_000_000;
    {
        let guard = f.current.read().unwrap();
        assert!(
            pins.context(&f.owner, guard.view(), &Clock::Fixed(late), &mut high_water)
                .is_err()
        );
    }
    // A new committed generation (descriptor renewal) is a different key.
    let mut f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, _digest) = f.handshake(&mut registry, [152; 32]);
    let pins = ContextPins::capture(&registry, registry.live.get(&id).unwrap().identity);
    let mut high_water = f.now;
    {
        let guard = f.current.read().unwrap();
        pins.context(
            &f.owner,
            guard.view(),
            &Clock::Fixed(f.now),
            &mut high_water,
        )
        .unwrap();
    }
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9444", 0, at(f.now + 1))
        })
        .unwrap()
        .unwrap();
    let guard = f.current.read().unwrap();
    let failure = pins
        .context(
            &f.owner,
            guard.view(),
            &Clock::Fixed(f.now + 2),
            &mut high_water,
        )
        .err()
        .expect("a renewed descriptor is never served from the old generation's cache");
    assert_eq!(failure.to_string(), "descriptor replaced");
}

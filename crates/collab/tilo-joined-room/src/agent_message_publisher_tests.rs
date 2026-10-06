//! Real signed owner admission/persistence with deterministic retirement gates.
//! This does not claim installed SDK transport or actual model execution.
use super::*;
use crate::agent_message_commit::tests::{add_agent, publisher, publisher_registration, source};
use crate::application_record::tests::{Fixture, fixture};
use ed25519_dalek::Signer as _;
use std::sync::atomic::AtomicUsize;

struct GateReset;
impl Drop for GateReset {
    fn drop(&mut self) {
        PUBLICATION_TEST_GATE.with_borrow_mut(|gate| *gate = None);
    }
}

fn at_gate<T>(after: bool, effect: impl Fn() + 'static, run: impl FnOnce() -> T) -> T {
    let observed = Arc::new(AtomicUsize::new(0));
    let count = observed.clone();
    PUBLICATION_TEST_GATE.with_borrow_mut(|gate| {
        assert!(gate.is_none());
        *gate = Some(Box::new(move |reserved| {
            if reserved == after {
                count.fetch_add(1, Ordering::SeqCst);
                effect();
            }
        }));
    });
    let reset = GateReset;
    let result = run();
    drop(reset);
    assert_eq!(
        observed.load(Ordering::SeqCst),
        1,
        "actual final gate was not observed"
    );
    result
}

fn no_input(fixture: &Fixture, generation: u64) {
    fixture
        .owner
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            assert_eq!(view.generation(), generation);
            assert!(view.record.agent_messages.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn actual_signed_registration_and_exact_guard_cannot_be_swapped_or_resurrected() {
    use ed25519_dalek::Signer as _;
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = source(&fixture, 0);
    let original = publisher(&fixture);
    let mut registration = publisher_registration(&fixture);
    registration.data.stream = uuid::Uuid::from_u128(905);
    assert!(
        fixture
            .owner
            .register_agent_message_publisher(&registration, fixture.now)
            .is_err()
    );
    registration.signature = fixture
        .key
        .sign(&registration.signing_bytes().unwrap())
        .to_bytes()
        .to_vec();
    let other = fixture
        .owner
        .register_agent_message_publisher(&registration, fixture.now)
        .unwrap();
    assert!(
        fixture
            .owner
            .commit_agent_messages(
                request.clone(),
                std::slice::from_ref(&proof),
                &other,
                fixture.now
            )
            .unwrap_err()
            .contains("another owner or installation")
    );
    original.retire();
    drop(original);
    let mut fresh = registration;
    fresh.data.installation = uuid::Uuid::from_u128(906);
    fresh.signature = fixture
        .key
        .sign(&fresh.signing_bytes().unwrap())
        .to_bytes()
        .to_vec();
    fixture
        .owner
        .register_agent_message_publisher(&fresh, fixture.now)
        .unwrap();
    assert!(
        fixture
            .owner
            .register_agent_message_publisher(&publisher_registration(&fixture), fixture.now)
            .is_err(),
        "retired signed identity cannot mint a new active guard after collection"
    );
    assert!(
        fixture
            .owner
            .commit_agent_messages(request, &[proof], &other, fixture.now)
            .is_err()
    );
}

#[test]
fn a_different_valid_signer_cannot_install_an_owner_publisher() {
    use ed25519_dalek::Signer as _;
    let fixture = fixture();
    let root = ed25519_dalek::SigningKey::from_bytes(&[101; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[102; 32]);
    let mut registration = publisher_registration(&fixture);
    registration.attestor = crate::testing::certificate(&root, &key, "Other host");
    registration.signature = key
        .sign(&registration.signing_bytes().unwrap())
        .to_bytes()
        .to_vec();
    assert!(
        fixture
            .owner
            .register_agent_message_publisher(&registration, fixture.now)
            .is_err()
    );
    assert!(fixture.owner.0.message_publishers.lock().live.is_empty());
}

#[test]
fn retirement_at_actual_final_gate_refuses_without_partial_publication() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = source(&fixture, 0);
    let lease = publisher(&fixture);
    let generation = fixture
        .owner
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            Ok(view.generation())
        })
        .unwrap();
    let retiring = lease.clone();
    let error = at_gate(
        false,
        move || retiring.retire(),
        || {
            fixture
                .owner
                .commit_agent_messages(request, &[proof], &lease, fixture.now)
        },
    )
    .unwrap_err();
    assert!(error.contains("retired before publication"), "{error}");
    no_input(&fixture, generation);
    assert!(lease.reserve().is_err());
}

#[test]
fn retirement_after_reservation_preserves_commit_and_complete_fixed_uuid_replay() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, mut proof) = source(&fixture, 0);
    let lease = publisher(&fixture);
    let retiring = lease.clone();
    let receipt = at_gate(
        true,
        move || retiring.retire(),
        || {
            fixture.owner.commit_agent_messages(
                request.clone(),
                std::slice::from_ref(&proof),
                &lease,
                fixture.now,
            )
        },
    )
    .unwrap();
    assert!(lease.reserve().is_err());
    let replay = fixture
        .owner
        .commit_agent_messages(
            request.clone(),
            std::slice::from_ref(&proof),
            &lease,
            fixture.now,
        )
        .unwrap();
    assert_eq!(receipt.binding, replay.binding);
    assert_eq!(receipt.generation, replay.generation);
    proof.data.executed_actor = b"substituted-writer".to_vec();
    proof.signature = fixture
        .key
        .sign(&proof.signing_bytes().unwrap())
        .to_bytes()
        .to_vec();
    assert!(
        fixture
            .owner
            .commit_agent_messages(request, &[proof], &lease, fixture.now)
            .is_err(),
        "retired exact retry cannot bypass complete immutable payload equality"
    );
    assert_eq!(lease.0.state.load(Ordering::Acquire), RETIRED);
}

#[test]
fn retirement_while_actual_owner_turn_is_queued_refuses() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = source(&fixture, 0);
    let lease = publisher(&fixture);
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let generation = fixture
        .owner
        .with_current(room, |view| Ok(view.generation()))
        .unwrap();
    let turn = fixture.owner.0.mutation.lock();
    let owner = fixture.owner.clone();
    let pending_lease = lease.clone();
    let now = fixture.now;
    let (entered, waiting) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        entered.send(()).unwrap();
        owner.commit_agent_messages(request, &[proof], &pending_lease, now)
    });
    waiting
        .recv_timeout(
            tilo_test_env::hang_deadline().saturating_duration_since(std::time::Instant::now()),
        )
        .unwrap();
    lease.retire();
    drop(turn);
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .contains("retired before publication")
    );
    no_input(&fixture, generation);
}

#[test]
fn definite_and_uncertain_persistence_release_reservation_without_clearing_retirement() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    for (point, uncertain) in [
        (Checkpoint::BeforePublish, false),
        (Checkpoint::AfterPublish, true),
    ] {
        for retired in [false, true] {
            let fixture = fixture();
            add_agent(&fixture);
            let (request, proof) = source(&fixture, 0);
            let lease = publisher(&fixture);
            let retiring = lease.clone();
            let result = at_gate(
                true,
                move || {
                    if retired {
                        retiring.retire();
                    }
                },
                || {
                    with_failure(point, || {
                        fixture.owner.commit_agent_messages(
                            request.clone(),
                            std::slice::from_ref(&proof),
                            &lease,
                            fixture.now,
                        )
                    })
                },
            );
            assert!(result.is_err());
            assert_eq!(
                lease.0.state.load(Ordering::Acquire),
                if retired { RETIRED } else { 0 }
            );
            assert_eq!(fixture.owner.health().is_err(), uncertain);
            if uncertain {
                assert!(
                    fixture
                        .owner
                        .commit_agent_messages(request, &[proof], &lease, fixture.now)
                        .is_err(),
                    "released atomic reservation cannot un-fence uncertain owner persistence"
                );
            } else if retired {
                assert!(
                    fixture
                        .owner
                        .commit_agent_messages(request, &[proof], &lease, fixture.now)
                        .unwrap_err()
                        .contains("retired before publication")
                );
            } else {
                fixture
                    .owner
                    .commit_agent_messages(request, &[proof], &lease, fixture.now)
                    .unwrap();
            }
        }
    }
}

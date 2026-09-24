use super::*;
use crate::delivery::resources::CodecBudget;
use crate::delivery::resources::tests::{precharged, work};
use automerge::resource_limits::NativeAllowance;
use automerge::resource_limits::NativeWorkTurn;
fn limits() -> DecodeLimits {
    DecodeLimits {
        maximum_bytes: 64 * 1024 * 1024,
        allocation_bytes: 8 * 1024 * 1024,
        collection_entries: 1024 * 1024,
    }
}
fn held() -> (NativeAllowance, NativeWorkTurn, HeldCloudState) {
    held_consumers(0)
}
fn held_consumers(count: usize) -> (NativeAllowance, NativeWorkTurn, HeldCloudState) {
    held_configure(|state| {
        // Inert resource fixture, not a live admitted consumer claim.
        for n in 1..=count {
            let mut item = consumer(1);
            item.id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            item.device[..8].copy_from_slice(&(n as u64).to_le_bytes());
            state
                .delivery
                .as_mut()
                .unwrap()
                .ledger
                .consumers
                .insert(item.device, item);
        }
    })
}
fn held_configure(
    configure: impl FnOnce(&mut State),
) -> (NativeAllowance, NativeWorkTurn, HeldCloudState) {
    let (_root, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    configure(&mut store.state);
    let bytes = serde_json::to_vec(&store.state).unwrap();
    let (allowance, turn) = work(64 * 1024 * 1024);
    let input =
        AggregateBytes::copy_from(&bytes, bytes.len(), &CodecBudget::native(&turn)).unwrap();
    let state = HeldCloudState::decode(&input, limits(), &CodecBudget::native(&turn)).unwrap();
    (allowance, turn, state)
}
#[test]
fn cloud_aggregate_mutation_tree_splits_and_pressure_leave_original_unchanged() {
    for count in [5, 11, 23, 127] {
        let (allowance, turn, state) = held_consumers(count);
        let before = serde_json::to_vec(state.state()).unwrap();
        let candidate = precharged(&allowance, || {
            state.metadata_candidate(
                MetadataChange::Consumer(consumer(250)),
                state.state().generation + 1,
                limits(),
                &CodecBudget::native(&turn),
            )
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            candidate
                .state()
                .delivery
                .as_ref()
                .unwrap()
                .ledger
                .consumers
                .len(),
            count + 1
        );
        assert_eq!(serde_json::to_vec(state.state()).unwrap(), before);
        drop(candidate);
        let remaining = allowance.available_bytes();
        let pressure = turn.reserve(remaining).unwrap();
        assert!(matches!(
            precharged(&allowance, || state.metadata_candidate(
                MetadataChange::Consumer(consumer(250)),
                state.state().generation + 1,
                limits(),
                &CodecBudget::native(&turn)
            )),
            Err(ResourceError::Capacity)
        ));
        drop(pressure);
        assert_eq!(allowance.available_bytes(), remaining);
        assert_eq!(serde_json::to_vec(state.state()).unwrap(), before);
    }
    let (allowance, turn, state) = held_consumers(qos_relay::delivery_v3::commit::MAX_CONSUMERS);
    let before = allowance.available_bytes();
    assert!(matches!(
        state.metadata_candidate(
            MetadataChange::Consumer(consumer(250)),
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn)
        ),
        Err(ResourceError::Capacity)
    ));
    assert_eq!(allowance.available_bytes(), before);
}
#[test]
fn cloud_aggregate_mutation_conflicts_and_policy_refresh_preserve_invariants() {
    let (allowance, turn, state) = held();
    let state = state
        .metadata_candidate(
            MetadataChange::Consumer(consumer(1)),
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
        .unwrap()
        .unwrap();
    let before = serde_json::to_vec(state.state()).unwrap();
    for change in [
        MetadataChange::Consumer(ConsumerState {
            root: [99; 32],
            ..consumer(1)
        }),
        MetadataChange::Consumer(ConsumerState {
            device: [2; 32],
            ..consumer(1)
        }),
        MetadataChange::Ack {
            device: [1; 32],
            consumer: [1; 16],
            cut: CommitRef {
                sequence: state.state().generation,
                digest: [0; 32],
            },
        },
        MetadataChange::Ack {
            device: [1; 32],
            consumer: [2; 16],
            cut: CommitRef {
                sequence: 1,
                digest: [0; 32],
            },
        },
    ] {
        let available = allowance.available_bytes();
        assert!(
            state
                .metadata_candidate(
                    change,
                    state.state().generation + 1,
                    limits(),
                    &CodecBudget::native(&turn)
                )
                .is_err()
        );
        assert_eq!(allowance.available_bytes(), available);
    }
    assert!(
        state
            .metadata_candidate(
                MetadataChange::Consumer(consumer(2)),
                state.state().generation,
                limits(),
                &CodecBudget::native(&turn)
            )
            .is_err()
    );
    let next = state
        .metadata_candidate(
            MetadataChange::Consumer(ConsumerState {
                authorization_view: [77; 32],
                ..consumer(1)
            }),
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        next.state().delivery.as_ref().unwrap().ledger.consumers[&[1; 32]].authorization_view,
        [77; 32]
    );
    assert_eq!(serde_json::to_vec(state.state()).unwrap(), before);
}
#[tokio::test]
async fn cloud_aggregate_mutation_pin_refs_are_paid_and_release_preserves_payloads() {
    let (_transport_root, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (allowance, turn, state) = held();
    let state = state
        .metadata_candidate(
            MetadataChange::Consumer(consumer(1)),
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
        .unwrap()
        .unwrap();
    let delivery = state.state().delivery.as_ref().unwrap();
    let refs =
        PublicationCustody::from_pool(&delivery.pool, delivery.baseline.iter(), &pool).unwrap();
    let watermark = CommitRef {
        sequence: delivery.baseline_generation,
        digest: delivery.ledger.replay[&delivery.baseline_generation].digest,
    };
    let candidate = precharged(&allowance, || {
        state.metadata_candidate(
            MetadataChange::PinInsert {
                id: [3; 16],
                consumer: [1; 16],
                authorization_view: [55; 32],
                watermark: watermark.clone(),
                references: &refs,
                expires_at_ns: 100,
            },
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
    })
    .unwrap()
    .unwrap();
    assert!(
        state
            .state()
            .delivery
            .as_ref()
            .unwrap()
            .ledger
            .pins
            .is_empty()
    );
    let pin = &candidate.state().delivery.as_ref().unwrap().ledger.pins[&[3; 16]];
    assert_eq!(pin.publications.len(), refs.len());
    assert!(
        pin.publications
            .iter()
            .enumerate()
            .all(|(i, p)| Some(p) == refs.reference(i))
    );
    let fake = pin.clone();
    assert!(
        candidate
            .metadata_candidate(
                MetadataChange::PinRenew {
                    id: [3; 16],
                    expected: &fake,
                    expires_at_ns: 200
                },
                candidate.state().generation + 1,
                limits(),
                &CodecBudget::native(&turn)
            )
            .is_err()
    );
    let renewed = precharged(&allowance, || {
        candidate.metadata_candidate(
            MetadataChange::PinRenew {
                id: [3; 16],
                expected: pin,
                expires_at_ns: 200,
            },
            candidate.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
    })
    .unwrap()
    .unwrap();
    let pin = &renewed.state().delivery.as_ref().unwrap().ledger.pins[&[3; 16]];
    assert_eq!(pin.expires_at_ns, 200);
    let released = precharged(&allowance, || {
        renewed.metadata_candidate(
            MetadataChange::PinRelease {
                id: [3; 16],
                expected: pin,
            },
            renewed.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
    })
    .unwrap()
    .unwrap();
    assert!(
        released
            .state()
            .delivery
            .as_ref()
            .unwrap()
            .ledger
            .pins
            .is_empty()
    );
    for i in 0..refs.len() {
        assert_eq!(
            released
                .state()
                .delivery
                .as_ref()
                .unwrap()
                .pool
                .resolve(&refs.reference(i).unwrap().payload)
                .unwrap(),
            refs.get(i).unwrap().bytes
        );
    }
    let available = allowance.available_bytes();
    let output = precharged(&allowance, || {
        crate::delivery::resources::encode_state(
            released.state(),
            64 * 1024 * 1024,
            &CodecBudget::native(&turn),
        )
    })
    .unwrap();
    drop(output);
    assert_eq!(allowance.available_bytes(), available);
}
fn consumer(n: u8) -> ConsumerState {
    ConsumerState {
        id: [n; 16],
        device: [n; 32],
        root: [44; 32],
        authorization_view: [55; 32],
        acknowledged: None,
    }
}
#[test]
fn cloud_aggregate_mutation_consumer_is_paid_and_source_is_unchanged() {
    let (allowance, turn, state) = held();
    let before = serde_json::to_vec(state.state()).unwrap();
    let old_generation = state.state().generation;
    let candidate = precharged(&allowance, || {
        state.metadata_candidate(
            MetadataChange::Consumer(consumer(1)),
            old_generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
    })
    .unwrap()
    .unwrap();
    assert_eq!(serde_json::to_vec(state.state()).unwrap(), before);
    assert_eq!(candidate.state().generation, old_generation + 1);
    let delivery = candidate.state().delivery.as_ref().unwrap();
    assert_eq!(delivery.ledger.consumers.get(&[1; 32]), Some(&consumer(1)));
    assert_eq!(delivery.ledger.resolved_through, old_generation + 1);
    assert!(!delivery.ledger.replay.contains_key(&(old_generation + 1)));
    let unchanged = allowance.available_bytes();
    assert!(
        candidate
            .metadata_candidate(
                MetadataChange::Consumer(consumer(1)),
                old_generation + 2,
                limits(),
                &CodecBudget::native(&turn)
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(allowance.available_bytes(), unchanged);
}
#[test]
fn cloud_aggregate_mutation_ack_uses_exact_existing_cut_without_replay() {
    let (allowance, turn, state) = held();
    let state = state
        .metadata_candidate(
            MetadataChange::Consumer(consumer(1)),
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
        .unwrap()
        .unwrap();
    let delivery = state.state().delivery.as_ref().unwrap();
    let entry = delivery.ledger.replay.values().next().unwrap();
    let cut = CommitRef {
        sequence: entry.owner_generation,
        digest: entry.digest,
    };
    let next = state.state().generation + 1;
    let candidate = precharged(&allowance, || {
        state.metadata_candidate(
            MetadataChange::Ack {
                device: [1; 32],
                consumer: [1; 16],
                cut: cut.clone(),
            },
            next,
            limits(),
            &CodecBudget::native(&turn),
        )
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        candidate
            .state()
            .delivery
            .as_ref()
            .unwrap()
            .ledger
            .consumers[&[1; 32]]
            .acknowledged,
        Some(cut.clone())
    );
    assert!(
        state.state().delivery.as_ref().unwrap().ledger.consumers[&[1; 32]]
            .acknowledged
            .is_none()
    );
    assert!(
        !candidate
            .state()
            .delivery
            .as_ref()
            .unwrap()
            .ledger
            .replay
            .contains_key(&next)
    );
    let occupied = turn.reserve(allowance.available_bytes()).unwrap();
    for change in [
        MetadataChange::Consumer(consumer(1)),
        MetadataChange::Ack {
            device: [1; 32],
            consumer: [1; 16],
            cut: cut.clone(),
        },
    ] {
        assert!(
            precharged(&allowance, || candidate.metadata_candidate(
                change,
                next + 1,
                limits(),
                &CodecBudget::native(&turn)
            ))
            .unwrap()
            .is_none()
        );
    }
    drop(occupied);
    assert_eq!(
        candidate
            .state()
            .delivery
            .as_ref()
            .unwrap()
            .ledger
            .consumers[&[1; 32]]
            .acknowledged,
        Some(cut)
    );
    let refreshed = precharged(&allowance, || {
        candidate.metadata_candidate(
            MetadataChange::Consumer(ConsumerState {
                authorization_view: [77; 32],
                ..consumer(1)
            }),
            next + 1,
            limits(),
            &CodecBudget::native(&turn),
        )
    })
    .unwrap()
    .unwrap();
    assert!(
        refreshed
            .state()
            .delivery
            .as_ref()
            .unwrap()
            .ledger
            .consumers[&[1; 32]]
            .acknowledged
            .is_none()
    );
}

#[tokio::test]
async fn cloud_aggregate_mutation_pin_quota_and_foreign_refs_refuse_before_candidate() {
    let (_root, transport) = crate::https_owner::prepared_transport_test().await;
    let host = transport.host_bytes().unwrap();
    let (allowance, turn, state) = held_configure(|state| {
        let d = state.delivery.as_mut().unwrap();
        d.ledger.consumers.insert([1; 32], consumer(1));
        for id in 1..=MAX_PINS {
            d.ledger.pins.insert(
                [id as u8; 16],
                SnapshotPin {
                    consumer: [1; 16],
                    authorization_view: [55; 32],
                    watermark: CommitRef {
                        sequence: d.baseline_generation,
                        digest: d.ledger.replay[&d.baseline_generation].digest,
                    },
                    publications: d.baseline.clone(),
                    expires_at_ns: 100,
                },
            );
        }
    });
    let d = state.state().delivery.as_ref().unwrap();
    let refs = PublicationCustody::from_pool(&d.pool, d.baseline.iter(), &host).unwrap();
    let watermark = CommitRef {
        sequence: d.baseline_generation,
        digest: d.ledger.replay[&d.baseline_generation].digest,
    };
    let before = allowance.available_bytes();
    assert!(matches!(
        precharged(&allowance, || state.metadata_candidate(
            MetadataChange::PinInsert {
                id: [250; 16],
                consumer: [1; 16],
                authorization_view: [55; 32],
                watermark: watermark.clone(),
                references: &refs,
                expires_at_ns: 100
            },
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Capacity)
    ));
    assert_eq!(allowance.available_bytes(), before);
    assert_eq!(d.ledger.pins.len(), MAX_PINS);
    assert!(
        state
            .metadata_candidate(
                MetadataChange::PinInsert {
                    id: [1; 16],
                    consumer: [1; 16],
                    authorization_view: [55; 32],
                    watermark: watermark.clone(),
                    references: &refs,
                    expires_at_ns: 100
                },
                state.state().generation + 1,
                limits(),
                &CodecBudget::native(&turn)
            )
            .unwrap()
            .is_none()
    );
    let mut other = qos_relay::delivery_v3::commit::PublicationPool::new(1024).unwrap();
    let missing = PublicationRef {
        payload: other.intern(b"unrelated bytes").unwrap(),
        kind: qos_peer_state::transport::Retention::Delta,
    };
    let foreign = PublicationCustody::from_pool(&other, [&missing].into_iter(), &host).unwrap();
    assert!(matches!(
        precharged(&allowance, || state.metadata_candidate(
            MetadataChange::PinInsert {
                id: [250; 16],
                consumer: [1; 16],
                authorization_view: [55; 32],
                watermark,
                references: &foreign,
                expires_at_ns: 100
            },
            state.state().generation + 1,
            limits(),
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Malformed)
    ));
    assert_eq!(allowance.available_bytes(), before);
}

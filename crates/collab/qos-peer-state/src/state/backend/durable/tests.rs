use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};

fn context() -> (NativeAllowance, NativeAllocationSet, NativeWorkTurn) {
    let allowance = NativeAllowance::new(8 * 1024 * 1024).unwrap();
    let registry = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = registry.work_turn(allowance.clone()).unwrap();
    (allowance, registry, turn)
}
fn limits() -> OuterLimits {
    OuterLimits {
        allocation_bytes: 1024 * 1024,
        collection_entries: 65536,
    }
}

#[test]
fn retained_durable_preserves_all_original_wire_versions_without_migration_copy() {
    let (peer, _, _, _, _) = crate::admission_tests::seeded();
    let v2 = peer.durable_v2_for_test().unwrap();
    let v4 = peer.durable().unwrap();
    let base: DurableV4 = postcard::from_bytes(&v4).unwrap();
    let v3 = postcard::to_allocvec(&base.base).unwrap();
    let (allowance, _registry, turn) = context();
    let initial = allowance.retained_bytes();
    for (version, bytes) in [(2, v2), (3, v3), (4, v4)] {
        let holder = assert_precharged(&allowance, || {
            DecodedDurable::decode(&bytes, limits(), limits(), &turn)
        })
        .unwrap();
        assert_eq!(holder.version, version);
        assert!(holder.exact.is_none());
        let encoded = match &holder.wire {
            DurableWire::V2(value) => postcard::to_allocvec(value).unwrap(),
            DurableWire::V3(value) => postcard::to_allocvec(value).unwrap(),
            DurableWire::V4(value) => postcard::to_allocvec(value).unwrap(),
        };
        assert_eq!(encoded, bytes);
        assert_eq!(
            allowance.retained_bytes(),
            initial + limits().allocation_bytes
        );
        drop(holder);
        assert_eq!(allowance.retained_bytes(), initial);
    }
}

#[test]
fn retained_durable_v5_moves_borrowed_exact_input_into_retained_current_turn_storage() {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    peer.select_admission_v5().unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = peer
        .publish_document(
            "test.app",
            "tab:1",
            peer.document("test.app", "tab:1").unwrap(),
            &key,
            now,
        )
        .unwrap()
    else {
        panic!("signed publication");
    };
    let admission = peer
        .lookup_exact_local_document("test.app", "tab:1", &envelope, now)
        .unwrap()
        .unwrap();
    let bytes = peer.durable().unwrap();
    let (allowance, _registry, turn) = context();
    let initial = allowance.retained_bytes();
    let holder = assert_precharged(&allowance, || {
        DecodedDurable::decode(&bytes, limits(), limits(), &turn)
    })
    .unwrap();
    assert_eq!(holder.version, 5);
    assert_eq!(
        holder.exact.as_ref().unwrap().get(&admission.key),
        Some(&admission.operation_id)
    );
    drop(bytes);
    drop(peer);
    allowance.finish_turn();
    assert_eq!(
        holder.exact.as_ref().unwrap().get(&admission.key),
        Some(&admission.operation_id)
    );
    assert_eq!(
        allowance.retained_bytes(),
        initial + 2 * limits().allocation_bytes
    );
    drop(holder);
    assert_eq!(allowance.retained_bytes(), initial);
}

#[test]
fn retained_durable_rejects_outer_and_exact_quotas_bad_envelopes_and_pending_accounting() {
    let (mut peer, _, _, _, _) = crate::admission_tests::seeded();
    let (allowance, _registry, turn) = context();
    let initial = allowance.retained_bytes();
    let v4 = peer.durable().unwrap();
    assert!(matches!(
        assert_precharged(&allowance, || DecodedDurable::decode(
            &v4,
            OuterLimits {
                allocation_bytes: 128,
                ..limits()
            },
            limits(),
            &turn
        )),
        Err(DurableDecodeError::Resource(PeerResourceError::Capacity))
    ));
    let mut wrong: DurableV4 = postcard::from_bytes(&v4).unwrap();
    wrong.pending_bytes = 1;
    let badpending = postcard::to_allocvec(&wrong).unwrap();
    assert!(matches!(
        DecodedDurable::decode(&badpending, limits(), limits(), &turn),
        Err(DurableDecodeError::Pending(
            super::super::pending::PendingError::Bytes
        ))
    ));
    peer.select_admission_v5().unwrap();
    let v5 = peer.durable().unwrap();
    assert!(matches!(
        DecodedDurable::decode(
            &v5,
            limits(),
            OuterLimits {
                allocation_bytes: 0,
                ..limits()
            },
            &turn
        ),
        Err(DurableDecodeError::Resource(PeerResourceError::Capacity))
    ));
    let mut decoded: DurableV5<'_> = postcard::from_bytes(&v5).unwrap();
    decoded.base.version = 3;
    let bad = postcard::to_allocvec(&decoded).unwrap();
    assert!(matches!(
        DecodedDurable::decode(&bad, limits(), limits(), &turn),
        Err(DurableDecodeError::Envelope)
    ));
    let mut trailing = v4;
    trailing.push(0);
    assert!(DecodedDurable::decode(&trailing, limits(), limits(), &turn).is_err());
    assert!(matches!(
        DecodedDurable::decode(&[0], limits(), limits(), &turn),
        Err(DurableDecodeError::UnsupportedVersion)
    ));
    assert_eq!(allowance.retained_bytes(), initial);
}

#[test]
fn retained_durable_current_turn_custody_survives_real_pending_input_and_original_drop() {
    use automerge::transaction::Transactable;
    let (mut source, _, member, cert, now) = crate::admission_tests::seeded();
    let mut receiver = source.clone();
    let mut doc = AutoCommit::load(&source.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "retained-predecessor", true).unwrap();
    doc.commit();
    source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "retained-descendant", true).unwrap();
    doc.commit();
    let descendant = source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(cert.device_public_key),
            descendant.clone(),
            now,
        )
        .unwrap();
    assert_eq!(receiver.pending_count(), 1);
    let bytes = receiver.durable().unwrap();
    let resident = NativeAllowance::new(1024 * 1024).unwrap();
    let registry = NativeAllocationSet::new(resident.clone()).unwrap();
    let current = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let turn = registry.work_turn(current.clone()).unwrap();
    let resident_before = resident.retained_bytes();
    let current_before = current.retained_bytes();
    let held = assert_precharged(&current, || {
        DecodedDurable::decode(&bytes, limits(), limits(), &turn)
    })
    .unwrap();
    assert_eq!(resident.retained_bytes(), resident_before);
    assert_eq!(
        current.retained_bytes(),
        current_before + limits().allocation_bytes
    );
    drop(bytes);
    drop(receiver);
    drop(source);
    current.finish_turn();
    let DurableWire::V4(value) = &held.wire else {
        panic!("original V4 storage");
    };
    assert_eq!(value.pending.len(), 1);
    assert_eq!(value.pending[0].1, descendant);
    assert!(matches!(
        DecodedDurable::decode(&[4], limits(), limits(), &turn),
        Err(DurableDecodeError::Resource(PeerResourceError::Capacity))
    ));
    drop(held);
    assert_eq!(current.retained_bytes(), current_before);
}

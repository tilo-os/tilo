use super::*;
use crate::NeighbourMsg;
use crate::TransportOrigin;
use automerge::AutoCommit;
use automerge::ROOT;
use collab_types::CommitNamed as _;

fn limits() -> OuterLimits {
    OuterLimits {
        allocation_bytes: 1024 * 1024,
        collection_entries: 65536,
    }
}

#[test]
fn retained_durable_v4_decodes_without_migration_copy() {
    let (peer, _, _, _, _) = crate::admission_tests::seeded();
    let bytes = peer.durable().unwrap();
    let holder = DecodedDurable::decode(&bytes, limits(), limits()).unwrap();
    assert_eq!(holder.version, 4);
    assert!(holder.exact.is_none());
    assert_eq!(postcard::to_allocvec(&holder.wire).unwrap(), bytes);
}

#[test]
fn retained_durable_v5_decodes_the_borrowed_exact_index_into_owned_storage() {
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
    let holder = DecodedDurable::decode(&bytes, limits(), limits()).unwrap();
    assert_eq!(holder.version, 5);
    drop(bytes);
    drop(peer);
    assert_eq!(
        holder.exact.as_ref().unwrap().get(&admission.key),
        Some(&admission.operation_id)
    );
}

#[test]
fn retained_durable_rejects_outer_and_exact_limits_bad_envelopes_and_pending_accounting() {
    let (mut peer, _, _, _, _) = crate::admission_tests::seeded();
    let v4 = peer.durable().unwrap();
    assert!(matches!(
        DecodedDurable::decode(
            &v4,
            OuterLimits {
                allocation_bytes: 128,
                ..limits()
            },
            limits(),
        ),
        Err(DurableDecodeError::Resource(PeerResourceError::Capacity))
    ));
    let mut wrong: DurableV4 = postcard::from_bytes(&v4).unwrap();
    wrong.pending_bytes = 1;
    let badpending = postcard::to_allocvec(&wrong).unwrap();
    assert!(matches!(
        DecodedDurable::decode(&badpending, limits(), limits()),
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
        ),
        Err(DurableDecodeError::Resource(PeerResourceError::Capacity))
    ));
    let mut decoded: DurableV5<'_> = postcard::from_bytes(&v5).unwrap();
    decoded.base.version = 3;
    let bad = postcard::to_allocvec(&decoded).unwrap();
    assert!(matches!(
        DecodedDurable::decode(&bad, limits(), limits()),
        Err(DurableDecodeError::Envelope)
    ));
    let mut trailing = v4;
    trailing.push(0);
    assert!(DecodedDurable::decode(&trailing, limits(), limits()).is_err());
    assert!(matches!(
        DecodedDurable::decode(&[0], limits(), limits()),
        Err(DurableDecodeError::UnsupportedVersion)
    ));
}

#[test]
fn retained_durable_keeps_real_pending_input_after_the_original_is_dropped() {
    use automerge::transaction::Transactable;
    let (mut source, _, member, cert, now) = crate::admission_tests::seeded();
    let mut receiver = source.clone();
    let mut doc = AutoCommit::load(&source.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "retained-predecessor", true).unwrap();
    doc.commit_edit("fixture", "retained-predecessor");
    source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "retained-descendant", true).unwrap();
    doc.commit_edit("fixture", "retained-descendant");
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
    let held = DecodedDurable::decode(&bytes, limits(), limits()).unwrap();
    drop(bytes);
    drop(receiver);
    drop(source);
    let value = &held.wire;
    assert_eq!(value.pending.len(), 1);
    assert_eq!(value.pending[0].1, descendant);
    assert!(matches!(
        DecodedDurable::decode(&[4], limits(), limits()),
        Err(DurableDecodeError::Resource(_))
    ));
}

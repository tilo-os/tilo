use super::*;
use crate::owner::resources::OwnedPublicationPin;
use crate::{
    SessionOwner, committed_authority::CommittedAuthority, owner::resources::OwnerOperation,
};
use qos_peer_state::{NeighbourMsg, TransportOrigin};
use tilo_relay::delivery_v3::state::{TransferRecordHeader, VisibleOrder};
use ui::automerge::{ROOT, transaction::Transactable};

fn real_pin(
    large: bool,
) -> (
    OwnedPublicationPin,
    HostBytes,
    usize,
    Vec<Vec<u8>>,
    crate::https::Ingress,
) {
    let tmp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&tmp.path().join("owner")).unwrap();
    if large {
        let mut author = owner.peer().clone();
        let mut doc =
            ui::automerge::AutoCommit::load(&author.document("dev.qos.chat", "").unwrap()).unwrap();
        let heads = doc.get_heads();
        let mut value = vec![0; 600 * 1024];
        blake3::Hasher::new()
            .update(b"real original snapshot bytes")
            .finalize_xof()
            .fill(&mut value);
        doc.put(
            ROOT,
            "large-snapshot",
            ui::automerge::ScalarValue::Bytes(value),
        )
        .unwrap();
        doc.commit();
        let message = author
            .publish_incremental(
                "dev.qos.chat",
                "",
                doc.save_after(&heads),
                &owner.identity.native,
                web_time::SystemTime::now(),
            )
            .unwrap();
        owner
            .receive(
                TransportOrigin::RelayDelta(owner.identity.native.verifying_key().to_bytes()),
                message,
            )
            .unwrap();
    }
    owner.activate_delivery().unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let idle = pool.available_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let expected = owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect();
    let mut scope = owner
        .prepare_owner_scope(&pool, OwnerOperation::Export)
        .unwrap();
    let pin = owner
        .with_owner_scope(&mut scope, &current, |o| o.pin_publication(&pool))
        .unwrap()
        .unwrap();
    drop(scope);
    drop(current);
    drop(owner);
    (pin, pool, idle, expected, ingress)
}
#[test]
fn snapshot_stream_preserves_real_original_records_across_chunk_and_item_boundaries() {
    let (pin, pool, idle, expected, _ingress) = real_pin(true);
    let stream = SnapshotStream::prepare(pin.into_custody(), &pool).unwrap();
    assert_eq!(stream.item_count(), expected.len());
    assert!(stream.chunk_digests().len() > 2);
    let mut materialized = Vec::new();
    let mut scratch = vec![0; SNAPSHOT_CHUNK_BYTES];
    for (i, digest) in stream.chunk_digests().iter().enumerate() {
        let count = stream.fill_chunk(i as u32, &mut scratch).unwrap();
        assert_eq!(blake3::hash(&scratch[..count]).as_bytes(), digest);
        materialized.extend_from_slice(&scratch[..count]);
    }
    assert_eq!(materialized.len() as u64, stream.stream_bytes());
    let mut order = VisibleOrder::new(expected.len() as u32).unwrap();
    let mut remaining = materialized.as_slice();
    for (ordinal, bytes) in expected.iter().enumerate() {
        let header = TransferRecordHeader::decode(&remaining[..45]).unwrap();
        assert_eq!(header.ordinal(), ordinal as u32);
        assert_eq!(header.canonical_size(), bytes.len() as u64);
        assert_eq!(header.canonical_digest(), *blake3::hash(bytes).as_bytes());
        assert_eq!(header.kind(), native_family(bytes).unwrap());
        assert_eq!(&remaining[45..45 + bytes.len()], bytes);
        order
            .push(ordinal as u32, header.canonical_digest())
            .unwrap();
        remaining = &remaining[45 + bytes.len()..];
    }
    assert!(remaining.is_empty());
    assert_eq!(stream.visible_order(), order.finish().unwrap());
    assert!(
        stream
            .fill_chunk(stream.chunk_digests().len() as u32, &mut scratch)
            .is_err()
    );
    assert!(stream.fill_chunk(0, &mut [0; 1]).is_err());
    assert!(
        pool.available_bytes() < idle,
        "original owner custody survives actual owner drop"
    );
    drop(stream);
    assert_eq!(pool.available_bytes(), idle);
}
#[test]
fn snapshot_stream_rejects_foreign_pool_before_metadata_allocation() {
    let (pin, pool, idle, _, _ingress) = real_pin(false);
    let (foreign, _) = crate::https::ingress_channel();
    let foreign = foreign.host_bytes();
    let before = foreign.available_bytes();
    assert!(SnapshotStream::prepare(pin.into_custody(), &foreign).is_err());
    assert_eq!(foreign.available_bytes(), before);
    assert_eq!(pool.available_bytes(), idle);
}
#[test]
fn snapshot_stream_family_tags_match_actual_native_variants_and_reject_unsupported() {
    let (pin, _pool, _idle, _, _ingress) = real_pin(false);
    let mut governance = false;
    let mut state = false;
    for i in 0..pin.len() {
        let bytes = pin.get(i).unwrap().bytes;
        let actual: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        let expected = match actual {
            NeighbourMsg::SyncResp { .. } => {
                governance = true;
                TransferKind::Governance
            }
            NeighbourMsg::AppDocument { .. } => {
                state = true;
                TransferKind::State
            }
            _ => panic!("unexpected original native family"),
        };
        assert_eq!(native_family(bytes).unwrap(), expected);
    }
    assert!(governance && state);
    let unsupported = postcard::to_stdvec(&NeighbourMsg::SyncReq {
        room: "room".into(),
        since: Vec::new(),
        invite_id: None,
    })
    .unwrap();
    assert!(native_family(&unsupported).is_err());
    for invalid in [b"".as_slice(), b"\x81\x00", b"\x8c\x00", b"\xff"] {
        assert!(native_family(invalid).is_err());
    }
}

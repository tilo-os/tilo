use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use crate::resources::NativeAllowance;
use crate::resources::native::{NativeBatch, NativeDocument};
use crate::room::read::LegacyRead;
use automerge::resource_limits::NativeAllocationSet;
use automerge::{AutoCommit, ROOT, ScalarValue, transaction::Transactable};

fn limits() -> OuterLimits {
    OuterLimits {
        allocation_bytes: 256 * 1024,
        collection_entries: 4096,
    }
}
fn stored(version: DocumentWireVersion) -> (AutoCommit, Vec<u8>) {
    let (peer, _, _, _, _) = crate::admission_tests::seeded();
    let snapshot = peer.document("test.app", "tab:1").unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let payload = collab_types::DocumentMutation::Snapshot(snapshot.clone())
        .encode_for(version)
        .unwrap();
    let coverage = Coverage {
        room: peer.room().into(),
        app: "test.app".into(),
        record: "tab:1".into(),
        payload_hash: *blake3::hash(&payload).as_bytes(),
        heads: vec![],
        author: peer.authority().device_public_key,
        sequence: 1,
        watermarks: BTreeMap::new(),
    };
    super::super::put(&mut room, coverage).unwrap();
    (room, snapshot)
}
fn context() -> (NativeAllowance, NativeAllocationSet, NativeWorkTurn) {
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let registry = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = registry.work_turn(allowance.clone()).unwrap();
    (allowance, registry, turn)
}
fn native(
    room: &mut AutoCommit,
    registry: &mut NativeAllocationSet,
    turn: &NativeWorkTurn,
) -> NativeDocument {
    NativeBatch::new(registry, turn, 8 * 1024 * 1024)
        .unwrap()
        .load(&room.save())
        .unwrap()
}

#[test]
fn coverage_reader_resolves_all_actual_document_wire_versions_with_retained_native_work() {
    for version in DocumentWireVersion::ALL {
        let (mut room, snapshot) = stored(version);
        let (allowance, mut registry, turn) = context();
        let native = native(&mut room, &mut registry, &turn);
        let read = native.read(&turn).unwrap();
        let legacy = snapshot_wire_version(
            Some(&LegacyRead(&room)),
            "test.app",
            "tab:1",
            Some(&snapshot),
            &LegacyCoverage,
        )
        .unwrap();
        let bounded = assert_precharged(&allowance, || {
            snapshot_wire_version(
                Some(&read),
                "test.app",
                "tab:1",
                Some(&snapshot),
                &BoundedCoverage {
                    turn: &turn,
                    limits: limits(),
                },
            )
        })
        .unwrap();
        assert_eq!(legacy, Some(version));
        assert_eq!(bounded, legacy);
    }
}

#[test]
fn coverage_reader_preserves_absence_wrong_shape_decode_and_unmatched_checkpoint_semantics() {
    let (mut room, snapshot) = stored(DocumentWireVersion::CURRENT);
    let (allowance, mut registry, turn) = context();
    for mode in 0..4 {
        match mode {
            0 => {
                room.delete(ROOT, super::super::FIELD).unwrap();
            }
            1 => {
                room.put(ROOT, super::super::FIELD, "wrong shape").unwrap();
            }
            2 => {
                room.put(ROOT, super::super::FIELD, ScalarValue::Bytes(vec![0xff]))
                    .unwrap();
            }
            _ => {
                room = stored(DocumentWireVersion::CURRENT).0;
            }
        }
        room.commit();
        let native = native(&mut room, &mut registry, &turn);
        let read = native.read(&turn).unwrap();
        let payload = if mode == 3 {
            b"different snapshot".as_slice()
        } else {
            &snapshot
        };
        let legacy = snapshot_wire_version(
            Some(&LegacyRead(&room)),
            "test.app",
            "tab:1",
            Some(payload),
            &LegacyCoverage,
        );
        let bounded = assert_precharged(&allowance, || {
            snapshot_wire_version(
                Some(&read),
                "test.app",
                "tab:1",
                Some(payload),
                &BoundedCoverage {
                    turn: &turn,
                    limits: limits(),
                },
            )
        });
        match mode {
            0 => {
                assert_eq!(legacy, Ok(None));
                assert_eq!(bounded, Ok(None));
            }
            1 | 2 => {
                assert!(matches!(legacy, Err(WireVersionError::Decode(_))));
                assert!(matches!(
                    bounded,
                    Err(WireVersionError::Decode(PeerResourceError::Malformed))
                ));
            }
            _ => {
                assert_eq!(legacy, Err(WireVersionError::NoMatch));
                assert_eq!(bounded, Err(WireVersionError::NoMatch));
            }
        }
    }
}

#[test]
fn coverage_reader_keeps_genuine_capacity_denial_and_does_not_decode_when_checkpoint_absent() {
    let (mut room, snapshot) = stored(DocumentWireVersion::CURRENT);
    let (allowance, mut registry, turn) = context();
    let native = native(&mut room, &mut registry, &turn);
    let read = native.read(&turn).unwrap();
    let decoder = BoundedCoverage {
        turn: &turn,
        limits: OuterLimits {
            allocation_bytes: 16,
            ..limits()
        },
    };
    assert_eq!(
        assert_precharged(&allowance, || snapshot_wire_version(
            Some(&read),
            "test.app",
            "tab:1",
            None,
            &decoder
        )),
        Ok(None)
    );
    assert!(matches!(
        assert_precharged(&allowance, || snapshot_wire_version(
            Some(&read),
            "test.app",
            "tab:1",
            Some(&snapshot),
            &decoder
        )),
        Err(WireVersionError::Decode(PeerResourceError::Capacity))
    ));
    assert_eq!(
        snapshot_wire_version(
            Some(&LegacyRead(&room)),
            "test.app",
            "unknown",
            Some(&snapshot),
            &LegacyCoverage
        ),
        Ok(None)
    );
}

#[test]
fn coverage_reader_preserves_only_this_historical_fields_trailing_suffix_compatibility() {
    let (mut room, snapshot) = stored(DocumentWireVersion::QosV1);
    let mut bytes = super::super::governed_bytes(&room).unwrap();
    bytes.extend_from_slice(&[0xff, 0, 42]);
    room.put(ROOT, super::super::FIELD, ScalarValue::Bytes(bytes))
        .unwrap();
    room.commit();
    assert!(
        !super::super::entries(&room).unwrap().is_empty(),
        "literal legacy prefix semantics"
    );
    let (allowance, mut registry, turn) = context();
    let native = native(&mut room, &mut registry, &turn);
    let read = native.read(&turn).unwrap();
    assert_eq!(
        snapshot_wire_version(
            Some(&LegacyRead(&room)),
            "test.app",
            "tab:1",
            Some(&snapshot),
            &LegacyCoverage
        )
        .unwrap(),
        Some(DocumentWireVersion::QosV1)
    );
    assert_eq!(
        assert_precharged(&allowance, || snapshot_wire_version(
            Some(&read),
            "test.app",
            "tab:1",
            Some(&snapshot),
            &BoundedCoverage {
                turn: &turn,
                limits: limits()
            }
        ))
        .unwrap(),
        Some(DocumentWireVersion::QosV1)
    );
    let message = crate::NeighbourMsg::SyncResp {
        changes: vec![],
        room: "room".into(),
    };
    let mut encoded = postcard::to_allocvec(&message).unwrap();
    encoded.push(0);
    assert!(
        matches!(
            crate::resources::BoundedPeerMessage::decode(&encoded, limits(), &allowance),
            Err(PeerResourceError::Malformed)
        ),
        "ordinary messages stay strict"
    );
}

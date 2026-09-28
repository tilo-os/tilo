use super::*;
use crate::coverage::Coverage;
use crate::resources::native::{NativeBatch, NativeDocument};
use crate::room::read::LegacyRead;
use automerge::{AutoCommit, ObjType, ROOT, ScalarValue, transaction::Transactable};
use std::collections::BTreeMap;

fn limits() -> OuterLimits {
    OuterLimits {
        allocation_bytes: 256 * 1024,
        collection_entries: 4096,
    }
}
fn stored() -> (AutoCommit, Vec<u8>) {
    let (peer, _, _, _, _) = crate::admission_tests::seeded();
    let snapshot = peer.document("test.app", "tab:1").unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let payload = collab_types::DocumentMutation::Snapshot(snapshot.clone())
        .encode()
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
fn native(room: &mut AutoCommit) -> NativeDocument {
    NativeBatch::new(8 * 1024 * 1024)
        .load(&room.save())
        .unwrap()
}

#[test]
fn coverage_reader_accepts_the_governed_checkpoint_in_both_storage_modes() {
    let (mut room, snapshot) = stored();
    let native = native(&mut room);
    let read = native.read();
    let legacy = governed_snapshot_matches(
        Some(&LegacyRead(&room)),
        "test.app",
        "tab:1",
        Some(&snapshot),
        &LegacyCoverage,
    );
    let bounded = governed_snapshot_matches(
        Some(&read),
        "test.app",
        "tab:1",
        Some(&snapshot),
        &BoundedCoverage { limits: limits() },
    );
    assert_eq!(legacy, Ok(()));
    assert_eq!(bounded, Ok(()));
}

#[test]
fn coverage_reader_preserves_absence_wrong_shape_decode_and_unmatched_checkpoint_semantics() {
    let (mut room, snapshot) = stored();
    let route = super::super::route_key("test.app", "tab:1");
    let route = route.as_str();
    for mode in 0..4 {
        match mode {
            0 => {
                room.delete(ROOT, super::super::FIELD_V2).unwrap();
            }
            1 => {
                let routes = room
                    .put_object(ROOT, super::super::FIELD_V2, ObjType::Map)
                    .unwrap();
                room.put(&routes, route, "wrong shape").unwrap();
            }
            2 => {
                let routes = room
                    .put_object(ROOT, super::super::FIELD_V2, ObjType::Map)
                    .unwrap();
                room.put(&routes, route, ScalarValue::Bytes(vec![0xff]))
                    .unwrap();
            }
            _ => {
                room = stored().0;
            }
        }
        room.commit();
        let native = native(&mut room);
        let read = native.read();
        let payload = if mode == 3 {
            b"different snapshot".as_slice()
        } else {
            &snapshot
        };
        let legacy = governed_snapshot_matches(
            Some(&LegacyRead(&room)),
            "test.app",
            "tab:1",
            Some(payload),
            &LegacyCoverage,
        );
        let bounded = governed_snapshot_matches(
            Some(&read),
            "test.app",
            "tab:1",
            Some(payload),
            &BoundedCoverage { limits: limits() },
        );
        match mode {
            0 | 1 => {
                assert_eq!(legacy, Ok(()));
                assert_eq!(bounded, Ok(()));
            }
            2 => {
                assert!(matches!(legacy, Err(CoverageMatchError::Decode(_))));
                assert!(matches!(
                    bounded,
                    Err(CoverageMatchError::Decode(PeerResourceError::Malformed))
                ));
            }
            _ => {
                assert_eq!(legacy, Err(CoverageMatchError::NoMatch));
                assert_eq!(bounded, Err(CoverageMatchError::NoMatch));
            }
        }
    }
}

#[test]
fn coverage_reader_keeps_genuine_capacity_denial_and_does_not_decode_when_checkpoint_absent() {
    let (mut room, snapshot) = stored();
    let native = native(&mut room);
    let read = native.read();
    let decoder = BoundedCoverage {
        limits: OuterLimits {
            allocation_bytes: 16,
            ..limits()
        },
    };
    assert_eq!(
        governed_snapshot_matches(Some(&read), "test.app", "tab:1", None, &decoder),
        Ok(())
    );
    assert!(matches!(
        governed_snapshot_matches(Some(&read), "test.app", "tab:1", Some(&snapshot), &decoder),
        Err(CoverageMatchError::Decode(PeerResourceError::Capacity))
    ));
    assert_eq!(
        governed_snapshot_matches(
            Some(&LegacyRead(&room)),
            "test.app",
            "unknown",
            Some(&snapshot),
            &LegacyCoverage
        ),
        Ok(())
    );
}

use super::*;
use crate::coverage::Coverage;
use crate::workspace::read::LegacyRead;
use automerge::{AutoCommit, ObjType, ROOT, ScalarValue, transaction::Transactable};
use std::collections::BTreeMap;

fn stored() -> (AutoCommit, Vec<u8>) {
    let (peer, _, _, _, _) = crate::admission_tests::seeded();
    let snapshot = peer.document("test.app", "tab:1").unwrap();
    let mut workspace = peer.admitted_workspace().unwrap().clone();
    let payload = collab_types::DocumentMutation::Snapshot(snapshot.clone())
        .encode()
        .unwrap();
    let coverage = Coverage {
        workspace: peer.workspace_id().into(),
        app: "test.app".into(),
        record: "tab:1".into(),
        payload_hash: *blake3::hash(&payload).as_bytes(),
        heads: vec![],
        author: peer.owner_certificates()[0].device_public_key,
        sequence: 1,
        watermarks: BTreeMap::new(),
    };
    super::super::put(&mut workspace, coverage).unwrap();
    (workspace, snapshot)
}
/// A document that admitted the checkpoint `stored` covers.
fn admitted() -> BTreeMap<String, u64> {
    let (peer, _, _, _, _) = crate::admission_tests::seeded();
    BTreeMap::from([(
        hex::encode(peer.owner_certificates()[0].device_public_key),
        1,
    )])
}

#[test]
fn coverage_reader_accepts_the_governed_checkpoint() {
    let (workspace, snapshot) = stored();
    let legacy = governed_snapshot_matches(
        Some(&LegacyRead(&workspace)),
        ("test.app", "tab:1"),
        &admitted(),
        Some(&snapshot),
        &LegacyCoverage,
    );
    assert_eq!(legacy, Ok(Governed::Current));
}

#[test]
fn coverage_reader_preserves_absence_wrong_shape_decode_and_unmatched_checkpoint_semantics() {
    let (mut workspace, snapshot) = stored();
    let route = super::super::route_key("test.app", "tab:1");
    let route = route.as_str();
    for mode in 0..4 {
        match mode {
            0 => {
                workspace.delete(ROOT, super::super::FIELD_V2).unwrap();
            }
            1 => {
                let routes = workspace
                    .put_object(ROOT, super::super::FIELD_V2, ObjType::Map)
                    .unwrap();
                workspace.put(&routes, route, "wrong shape").unwrap();
            }
            2 => {
                let routes = workspace
                    .put_object(ROOT, super::super::FIELD_V2, ObjType::Map)
                    .unwrap();
                workspace
                    .put(&routes, route, ScalarValue::Bytes(vec![0xff]))
                    .unwrap();
            }
            _ => {
                workspace = stored().0;
            }
        }
        workspace.commit();
        let payload = if mode == 3 {
            b"different snapshot".as_slice()
        } else {
            &snapshot
        };
        let legacy = governed_snapshot_matches(
            Some(&LegacyRead(&workspace)),
            ("test.app", "tab:1"),
            &admitted(),
            Some(payload),
            &LegacyCoverage,
        );
        match mode {
            0 | 1 => {
                assert_eq!(legacy, Ok(Governed::Current));
            }
            2 => {
                assert!(matches!(legacy, Err(CoverageMatchError::Decode(_))));
            }
            _ => {
                assert_eq!(legacy, Err(CoverageMatchError::NoMatch));
            }
        }
    }
}

#[test]
fn coverage_reader_passes_a_route_it_does_not_cover() {
    let (workspace, snapshot) = stored();
    assert_eq!(
        governed_snapshot_matches(
            Some(&LegacyRead(&workspace)),
            ("test.app", "tab:1"),
            &admitted(),
            None,
            &LegacyCoverage
        ),
        Ok(Governed::Current)
    );
    assert_eq!(
        governed_snapshot_matches(
            Some(&LegacyRead(&workspace)),
            ("test.app", "unknown"),
            &admitted(),
            Some(&snapshot),
            &LegacyCoverage
        ),
        Ok(Governed::Current)
    );
}

#[test]
fn coverage_reader_reports_a_checkpoint_whose_body_has_not_been_admitted_as_arriving() {
    let (workspace, _) = stored();
    let older = b"older checkpoint".as_slice();
    let arriving = BTreeMap::new();
    assert_eq!(
        governed_snapshot_matches(
            Some(&LegacyRead(&workspace)),
            ("test.app", "tab:1"),
            &arriving,
            Some(older),
            &LegacyCoverage,
        ),
        Ok(Governed::Arriving)
    );
}

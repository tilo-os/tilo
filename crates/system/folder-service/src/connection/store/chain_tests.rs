//! Host-only historical fixtures. No fixture supplies live endpoint authority.
use super::*;
use crate::connection::transaction::chain::*;
use sha2::{Digest, Sha256};

// Independent wire construction freezes the field order and native units.
fn selection_vector(windows: bool, regular: bool) -> Vec<u8> {
    let mut b = b"QCHSEL1\0\x01".to_vec();
    b.extend([7; 16]);
    for v in [1, 2, 3, 4] {
        b.extend([v; 32]);
    }
    b.extend(9u64.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.push(1);
    b.extend(5u64.to_le_bytes());
    b.extend([8; 32]);
    if windows {
        b.extend([2, 3, 0, 97, 0, 92, 0, 98, 0]);
    } else {
        b.extend([1, 3, 0, 97, 47, 98]);
    }
    b.push(u8::from(windows));
    b.push(u8::from(regular));
    if regular {
        b.extend([6; 32]);
        b.extend(123u64.to_le_bytes());
        b.push(1);
    }
    b.extend(0u64.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.extend(2u64.to_le_bytes());
    let mut h = Sha256::new();
    h.update(b"qos-imager/connections-v1/directory-chain-selection/v1\0");
    h.update(&b);
    b.extend(h.finalize());
    b
}

#[test]
fn chain_selection_vectors_native_roundtrip_and_strict_boundaries() {
    for windows in [false, true] {
        for regular in [false, true] {
            let bytes = selection_vector(windows, regular);
            let selected = DirectoryChainSelectionV1::decode(&bytes)
                .expect("a canonical independent native selection must decode");
            assert_eq!(selected.connection, ConnectionId([7; 16]));
            assert_eq!(selected.review_revision, 9);
            assert_eq!(selected.baseline, Some(5));
            assert_eq!(selected.missing_prefixes, 2);
            assert_eq!(
                selected.from,
                if windows {
                    Side::Destination
                } else {
                    Side::Source
                }
            );
            assert_eq!(
                selected.path,
                if windows {
                    RelativePath::windows(&[97, 92, 98]).unwrap()
                } else {
                    RelativePath::unix(b"a/b").unwrap()
                }
            );
            assert_eq!(
                selected.terminal,
                if regular {
                    DirectoryChainTerminalV1::RegularAdd {
                        content: RegularContentV1 {
                            sha256: [6; 32],
                            bytes: 123,
                            executable: true,
                        },
                    }
                } else {
                    DirectoryChainTerminalV1::DirectoryAdd
                }
            );
            assert_eq!(selected.encode().unwrap(), bytes);
            for cut in 0..bytes.len() {
                assert!(
                    DirectoryChainSelectionV1::decode(&bytes[..cut]).is_err(),
                    "cut {cut}"
                );
            }
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(DirectoryChainSelectionV1::decode(&trailing).is_err());
            for i in 0..bytes.len() {
                let mut corrupt = bytes.clone();
                corrupt[i] ^= 0xff;
                assert!(
                    DirectoryChainSelectionV1::decode(&corrupt).is_err(),
                    "corrupt {i}"
                );
            }
        }
    }
    assert!(DirectoryChainSelectionV1::decode(&vec![0; 12289]).is_err());
}

fn resign(bytes: &mut [u8]) {
    let end = bytes.len() - 32;
    let mut h = Sha256::new();
    h.update(b"qos-imager/connections-v1/directory-chain-selection/v1\0");
    h.update(&bytes[..end]);
    bytes[end..].copy_from_slice(&h.finalize());
}
fn path_vector(depth: usize, masks: [u64; 3], regular: bool) -> Vec<u8> {
    let mut b = selection_vector(false, regular);
    let path = (0..depth).map(|_| "a").collect::<Vec<_>>().join("/");
    // Path begins after magic/version/id/endpoints/revision/fence/baseline/preview.
    b.splice(
        210..216,
        [
            vec![1],
            (path.len() as u16).to_le_bytes().to_vec(),
            path.into_bytes(),
        ]
        .concat(),
    );
    let offset = b.len() - 56;
    for (i, mask) in masks.iter().enumerate() {
        b[offset + i * 8..offset + i * 8 + 8].copy_from_slice(&mask.to_le_bytes());
    }
    resign(&mut b);
    b
}
#[test]
fn chain_selection_members_depth_masks_and_order() {
    for depth in [1, 2, 64] {
        let mask = if depth == 64 {
            u64::MAX - 1
        } else {
            (1u64 << depth) - 2
        };
        for (regular, direction) in [(false, 0), (false, 1), (true, 0), (true, 1)] {
            let mut bytes = path_vector(depth, [0, 0, mask], regular);
            bytes[212 + 2 * depth] = direction;
            resign(&mut bytes);
            let s = DirectoryChainSelectionV1::decode(&bytes).unwrap();
            assert_eq!(usize::from(s.member_count().unwrap()), depth);
            for index in 0..depth {
                let m = s.member(index as u8).unwrap();
                assert_eq!(m.path.components().len(), index + 1);
                assert_eq!(
                    m.from,
                    if direction == 0 {
                        Side::Source
                    } else {
                        Side::Destination
                    }
                );
                if index + 1 < depth {
                    assert_eq!(m.terminal, DirectoryChainTerminalV1::DirectoryAdd);
                } else {
                    assert_eq!(m.terminal, s.terminal);
                }
            }
            assert!(s.member(depth as u8).is_err());
        }
    }
    let s = DirectoryChainSelectionV1::decode(&path_vector(3, [2, 4, 0], true)).unwrap();
    assert_eq!(s.member_count().unwrap(), 1);
    assert_eq!(s.member(0).unwrap().path.components().len(), 3);
    for masks in [[2, 2, 0], [0, 0, 0], [1, 0, 2], [0, 0, 6], [2, 0, 2]] {
        assert!(DirectoryChainSelectionV1::decode(&path_vector(2, masks, false)).is_err());
    }
    assert!(
        DirectoryChainSelectionV1::decode(&path_vector(65, [0, 0, u64::MAX - 1], false)).is_err()
    );
    for (offset, value) in [(8, 2), (169, 2), (210, 3), (216, 2), (217, 2)] {
        let mut b = selection_vector(false, false);
        b[offset] = value;
        resign(&mut b);
        assert!(
            DirectoryChainSelectionV1::decode(&b).is_err(),
            "tag {offset}"
        );
    }
}
#[test]
fn chain_selection_members_observation_correlation() {
    use crate::connection::model::{
        Endpoint, NamePolicy, Observation, ObservedTree, PendingReason, ScopeIssue,
        VerifiedBaseline,
    };
    let s = DirectoryChainSelectionV1::decode(&selection_vector(false, false)).unwrap();
    let parent = RelativePath::unix(b"a").unwrap();
    let endpoint = |identity| Endpoint {
        identity,
        encoding: Encoding::Unix,
        names: NamePolicy::Exact,
    };
    let source = ObservedTree::new(
        endpoint(s.endpoints[0]),
        [
            (parent.clone(), EntryState::Directory),
            (s.path.clone(), EntryState::Directory),
        ],
        Observation::Complete,
    )
    .unwrap();
    let destination =
        ObservedTree::new(endpoint(s.endpoints[1]), [], Observation::Complete).unwrap();
    let baseline =
        VerifiedBaseline::from_verified(s.connection, s.endpoints[0], s.endpoints[1], []).unwrap();
    s.validate_observations(&baseline, &source, &destination)
        .expect("complete exact selected addition");
    let incomplete = ObservedTree::new(
        source.endpoint().clone(),
        source
            .entries()
            .iter()
            .map(|(path, state)| (path.clone(), state.clone())),
        Observation::Incomplete(vec![ScopeIssue {
            path: Some(s.path.clone()),
            reason: PendingReason::Offline,
        }]),
    )
    .unwrap();
    assert!(
        s.validate_observations(&baseline, &incomplete, &destination)
            .is_err()
    );
    let mut wrong = source.clone();
    wrong.entries.insert(
        s.path.clone(),
        EntryState::File {
            sha256: [6; 32],
            bytes: 0,
            executable: false,
        },
    );
    assert!(
        s.validate_observations(&baseline, &wrong, &destination)
            .is_err()
    );
    assert!(
        s.validate_observations(&baseline, &source, &source)
            .is_err()
    );
    let existing = ObservedTree::new(
        endpoint(s.endpoints[1]),
        [(parent, EntryState::Directory)],
        Observation::Complete,
    )
    .unwrap();
    assert!(
        s.validate_observations(&baseline, &source, &existing)
            .is_err()
    );
    let mut ambiguous = source.clone();
    let alias = RelativePath::unix(b"A").unwrap();
    ambiguous
        .entries
        .insert(alias.clone(), EntryState::Directory);
    ambiguous.endpoint.names = NamePolicy::Canonical(std::collections::BTreeMap::from([
        (RelativePath::unix(b"a").unwrap(), vec![b"a".to_vec()]),
        (alias, vec![b"a".to_vec()]),
        (s.path.clone(), vec![b"a".to_vec(), b"b".to_vec()]),
    ]));
    assert!(
        s.validate_observations(&baseline, &ambiguous, &destination)
            .is_err()
    );

    // Existing unbaselined ancestors are receipt work, not extra members.
    let selected = DirectoryChainSelectionV1::decode(&path_vector(3, [2, 4, 0], true)).unwrap();
    let a = RelativePath::unix(b"a").unwrap();
    let aa = RelativePath::unix(b"a/a").unwrap();
    let leaf = EntryState::File {
        sha256: [6; 32],
        bytes: 123,
        executable: true,
    };
    let source = ObservedTree::new(
        endpoint(selected.endpoints[0]),
        [
            (a.clone(), EntryState::Directory),
            (aa.clone(), EntryState::Directory),
            (selected.path.clone(), leaf),
        ],
        Observation::Complete,
    )
    .unwrap();
    let destination = ObservedTree::new(
        endpoint(selected.endpoints[1]),
        [
            (a.clone(), EntryState::Directory),
            (aa, EntryState::Directory),
        ],
        Observation::Complete,
    )
    .unwrap();
    let baseline = VerifiedBaseline::from_verified(
        selected.connection,
        selected.endpoints[0],
        selected.endpoints[1],
        [(a, EntryState::Directory)],
    )
    .unwrap();
    selected
        .validate_observations(&baseline, &source, &destination)
        .unwrap();
    let mut missing_keys = destination.clone();
    missing_keys.endpoint.names = NamePolicy::Canonical(Default::default());
    assert!(
        selected
            .validate_observations(&baseline, &source, &missing_keys)
            .is_err()
    );
    let mut reverse_bytes = path_vector(3, [2, 4, 0], true);
    reverse_bytes[218] = 1;
    resign(&mut reverse_bytes);
    let reverse = DirectoryChainSelectionV1::decode(&reverse_bytes).unwrap();
    let mut reverse_origin = source.clone();
    reverse_origin.endpoint.identity = selected.endpoints[1];
    let mut reverse_target = destination.clone();
    reverse_target.endpoint.identity = selected.endpoints[0];
    reverse
        .validate_observations(&baseline, &reverse_target, &reverse_origin)
        .unwrap();
}

#[test]
fn chain_selection_vectors_resigned_invalid_fields_and_maxima() {
    for offset in [153, 170] {
        let mut b = selection_vector(false, false);
        b[offset..offset + 8].fill(0);
        resign(&mut b);
        assert!(DirectoryChainSelectionV1::decode(&b).is_err());
    }
    let mut future = selection_vector(false, false);
    future[170..178].copy_from_slice(&10u64.to_le_bytes());
    resign(&mut future);
    assert!(DirectoryChainSelectionV1::decode(&future).is_err());
    let mut executable = selection_vector(false, true);
    let offset = executable.len() - 57;
    executable[offset] = 2;
    resign(&mut executable);
    assert!(DirectoryChainSelectionV1::decode(&executable).is_err());
    // 4096 native units,17 components of at most255 units: attainable maximum.
    for windows in [false, true] {
        let mut b = selection_vector(windows, true);
        let components = (0..17).map(|_| "z".repeat(240)).collect::<Vec<_>>();
        let path = components.join(if windows { "\\" } else { "/" });
        assert_eq!(path.len(), 4096);
        let native = if windows {
            path.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        } else {
            path.into_bytes()
        };
        b.splice(
            210..if windows { 219 } else { 216 },
            [
                vec![if windows { 2 } else { 1 }],
                4096u16.to_le_bytes().to_vec(),
                native,
            ]
            .concat(),
        );
        let offset = b.len() - 56;
        b[offset + 16..offset + 24].copy_from_slice(&131070u64.to_le_bytes());
        resign(&mut b);
        let s = DirectoryChainSelectionV1::decode(&b).expect("maximal legal path");
        assert_eq!(s.encode().unwrap(), b);
        assert_eq!(b.len(), if windows { 8504 } else { 4408 });
        let mut excessive = b.clone();
        excessive[211..213].copy_from_slice(&4097u16.to_le_bytes());
        resign(&mut excessive);
        assert!(DirectoryChainSelectionV1::decode(&excessive).is_err());
    }
}

fn empty_history() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("connections");
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let binding = |n, locator: &str| EndpointBinding {
        identity: EndpointIdentity {
            volume: [n; 32],
            root: [n + 1; 32],
        },
        locator: EndpointLocator::new(Path::new(locator)).unwrap(),
    };
    let record = store
        .create_connection(
            ConnectionId([7; 16]),
            binding(1, "/source"),
            binding(3, "/destination"),
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap()
        .value;
    (directory, path, store, record)
}
fn first_regular_wire(store: &ConnectionStore, record: &ConnectionRecord) -> serde_json::Value {
    use crate::connection::transaction::native::descriptor as d;
    let connection = store.connection(record.id).unwrap();
    let path = RelativePath::unix(b"a").unwrap();
    let mut operation = format::Operation::new(
        OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: record.baseline_revision,
            path: path.clone(),
            source_before: Some(EntryState::File {
                sha256: [6; 32],
                bytes: 123,
                executable: true,
            }),
            destination_before: None,
            decision: Decision::Propagate { from: Side::Source },
        },
        connection.serial + 1,
        &connection,
    )
    .unwrap();
    let object = |ino, kind| d::Object {
        dev: 1,
        ino,
        kind,
        generation: [9; 32],
    };
    let mut endpoints = [[0; 64]; 2];
    for (i, e) in [record.source.identity, record.destination.identity]
        .iter()
        .enumerate()
    {
        endpoints[i][..32].copy_from_slice(&e.volume);
        endpoints[i][32..].copy_from_slice(&e.root);
    }
    let admission = d::Admission {
        connection: record.id.0,
        operation: operation.serial,
        revision: record.revision,
        fence: record.fence_generation,
        baseline: record.baseline_revision,
        endpoints,
        direction: 0,
        decision: 1,
        path,
        chains: [vec![object(11, 0o040_000)], vec![object(12, 0o040_000)]],
        states: [
            d::State::Regular {
                object: object(13, 0o100_000),
                stamp: d::Stamp {
                    length: 123,
                    modified: (1, 0),
                    changed: (1, 0),
                    mode: 0o100_700,
                    uid: 0,
                    gid: 0,
                    links: 1,
                },
                content: d::Content {
                    digest: [6; 32],
                    length: 123,
                    executable: true,
                },
            },
            d::State::Absent,
        ],
    };
    operation.native_identity = Some(admission.encode().unwrap());
    operation.native_budget = Some(format::NativeBudget::initial());
    operation.decode().unwrap();
    let mut selected = path_vector(1, [0, 0, 0], true);
    selected[153..161].copy_from_slice(&record.revision.to_le_bytes());
    if let Some(baseline) = record.baseline_revision {
        selected[170..178].copy_from_slice(&baseline.to_le_bytes());
    } else {
        selected[169] = 0;
        selected.drain(170..178);
    }
    resign(&mut selected);
    let digest = selected[selected.len() - 32..].to_vec();
    let mut wire = serde_json::to_value(operation).unwrap();
    wire["directory_chain_anchor"] = serde_json::json!(selected);
    wire["directory_chain_ref"] = serde_json::json!({"anchor":{"connection":record.id.0,"serial":connection.serial+1},"digest":digest,"index":0});
    wire
}
#[test]
fn chain_selection_vectors_json_extensions_are_strict_and_legacy_absent() {
    let (_tmp, _path, store, record) = empty_history();
    let wire = first_regular_wire(&store, &record);
    let op: format::Operation =
        serde_json::from_value(wire.clone()).expect("canonical chain extension must decode");
    op.decode().unwrap();
    assert_eq!(serde_json::to_value(&op).unwrap(), wire);
    for key in [
        "directory_chain_anchor",
        "directory_chain_ref",
        "directory_chain_retired",
        "directory_chain_stop_boundary",
    ] {
        let mut bad = wire.clone();
        bad[key] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<format::Operation>(bad).is_err(),
            "null {key}"
        );
    }
    let mut unknown = wire.clone();
    unknown["directory_chain_ref"]["surplus"] = 1.into();
    assert!(serde_json::from_value::<format::Operation>(unknown).is_err());
    let raw = serde_json::to_string(&wire).unwrap();
    let duplicate = raw.replacen(
        '{',
        &format!(
            "{{\"directory_chain_ref\":{},",
            serde_json::to_string(&wire["directory_chain_ref"]).unwrap()
        ),
        1,
    );
    assert!(serde_json::from_str::<format::Operation>(&duplicate).is_err());
    let mut absent = wire.clone();
    absent
        .as_object_mut()
        .unwrap()
        .remove("directory_chain_anchor");
    absent
        .as_object_mut()
        .unwrap()
        .remove("directory_chain_ref");
    let legacy: format::Operation = serde_json::from_value(absent.clone()).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap(), absent);
}

#[test]
fn chain_selection_vectors_control_and_stop_json_shapes() {
    fn strict_tags<T: serde::de::DeserializeOwned + serde::Serialize>(kinds: &[&str]) {
        for kind in kinds {
            let wire = serde_json::json!({"kind": kind});
            let value: T = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(value).unwrap(), wire);
            let mut bad = wire;
            bad["surplus"] = 1.into();
            assert!(
                serde_json::from_value::<T>(bad).is_err(),
                "unknown field in {kind}"
            );
            let duplicate = format!("{{\"kind\":\"{kind}\",\"kind\":\"{kind}\"}}");
            assert!(
                serde_json::from_str::<T>(&duplicate).is_err(),
                "duplicate tag in {kind}"
            );
        }
    }
    strict_tags::<format::PauseOrCancel>(&["Pause", "Cancel"]);
    strict_tags::<format::ChainRetirementReason>(&["Completed", "Abandoned"]);
    strict_tags::<format::ChainStopKind>(&["NeverBegun", "StagedOnly", "PublishedAccounted"]);
    strict_tags::<format::DirectoryChainScheduleState>(&["Running", "Abandoning", "Disconnected"]);
    strict_tags::<format::DirectoryChainControlTransitionV1>(&[
        "StopAcknowledged",
        "Resume",
        "AbandonRequested",
        "Retire",
    ]);
    let id = serde_json::json!({"connection":([7u8;16]),"serial":1});
    for transition in [
        serde_json::json!({"kind":"StopRequested","reason":{"kind":"Pause"}}),
        serde_json::json!({"kind":"StopRequested","reason":{"kind":"Cancel"}}),
        serde_json::json!({"kind":"StopAcknowledged"}),
        serde_json::json!({"kind":"Resume"}),
        serde_json::json!({"kind":"AbandonRequested"}),
        serde_json::json!({"kind":"Retire"}),
    ] {
        let wire = serde_json::json!({"event":"DirectoryChainControl","connection":([7u8;16]),"anchor":id,"digest":([8u8;32]),"expected_progress":2,"expected_schedule":1,"control_sequence":1,"transition":transition});
        let change: Change =
            serde_json::from_value(wire.clone()).expect("strict tagged control syntax");
        assert_eq!(serde_json::to_value(change).unwrap(), wire);
        for key in ["anchor", "transition"] {
            let mut bad = wire.clone();
            bad[key]["surplus"] = 1.into();
            assert!(
                serde_json::from_value::<Change>(bad).is_err(),
                "unknown {key} in {wire}"
            );
        }
        let mut bad = wire.clone();
        bad["transition"]["kind"] = "resume".into();
        assert!(serde_json::from_value::<Change>(bad).is_err());
    }
    for (kind, evidence) in [
        ("NeverBegun", serde_json::json!({"kind":"Regular"})),
        (
            "NeverBegun",
            serde_json::json!({"kind":"Directory","fact":{"kind":"Untouched"}}),
        ),
        (
            "StagedOnly",
            serde_json::json!({"kind":"Regular","evidence":{"attempt":1,"displaced":null,"outcome":null}}),
        ),
        (
            "StagedOnly",
            serde_json::json!({"kind":"Directory","fact":{"kind":"Staged","attempt":1}}),
        ),
    ] {
        let wire = serde_json::json!({"event":"DirectoryChainStopBoundary","connection":([7u8;16]),"serial":1,"expected":2,"expected_schedule":2,"fence":0,"kind":{"kind":kind},"evidence":evidence});
        let change: Change =
            serde_json::from_value(wire.clone()).expect("typed historical stop syntax");
        assert_eq!(serde_json::to_value(change).unwrap(), wire);
        let mut bad = wire.clone();
        bad["evidence"]["fact"] = serde_json::Value::Null;
        bad["evidence"]["evidence"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Change>(bad).is_err());
        let mut bad = wire.clone();
        bad["evidence"]["kind"] = "regular".into();
        assert!(serde_json::from_value::<Change>(bad).is_err());
    }
}

fn seal_directory(mut b: Vec<u8>) -> Vec<u8> {
    let length = u32::try_from(b.len() + 32).unwrap();
    b[4..8].copy_from_slice(&length.to_le_bytes());
    b.extend(Sha256::digest(&b));
    b
}
fn directory_object(b: &mut Vec<u8>, ino: u8) {
    b.extend(1u64.to_le_bytes());
    b.extend(u64::from(ino).to_le_bytes());
    b.extend(0o040_000u32.to_le_bytes());
    b.extend([ino; 32]);
}
fn directory_header(magic: &[u8; 4], baseline: bool) -> Vec<u8> {
    let mut b = magic.to_vec();
    b.extend(0u32.to_le_bytes());
    b.extend([7; 16]);
    b.extend(1u64.to_le_bytes());
    b.extend(1u64.to_le_bytes());
    for v in [31, 32, 33] {
        b.extend([v; 16]);
    }
    b.extend(2u64.to_le_bytes());
    b.push(u8::from(baseline));
    b.extend(u64::from(baseline).to_le_bytes());
    b.extend(3u64.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b
}
fn directory_path_state(b: &mut Vec<u8>) {
    b.extend([0, 1, 1, 0, b'a']);
    for ino in [11, 12] {
        b.push(1);
        directory_object(b, ino);
    }
    b.push(1);
    directory_object(b, 13);
    b.extend(4096u64.to_le_bytes());
    for v in [1i64, 0, 1, 0] {
        b.extend(v.to_le_bytes());
    }
    b.extend(0o040_700u32.to_le_bytes());
    b.extend(0u32.to_le_bytes());
    b.extend(0u32.to_le_bytes());
    b.extend(2u64.to_le_bytes());
    b.push(0);
}
fn directory_wire_facts(baseline: bool) -> [Vec<u8>; 4] {
    let endpoints = |b: &mut Vec<u8>| {
        for v in [1, 2, 3, 4] {
            b.extend([v; 32]);
        }
        b.extend([0, 1]);
    };
    let mut admission = b"QNA2".to_vec();
    admission.extend(0u32.to_le_bytes());
    admission.extend([7; 16]);
    admission.extend(1u64.to_le_bytes());
    admission.extend(1u64.to_le_bytes());
    admission.extend(0u64.to_le_bytes());
    admission.push(u8::from(baseline));
    admission.extend(u64::from(baseline).to_le_bytes());
    endpoints(&mut admission);
    directory_path_state(&mut admission);
    let admission = seal_directory(admission);
    let mut intent = directory_header(b"QCI1", baseline);
    endpoints(&mut intent);
    directory_object(&mut intent, 14);
    intent.extend(Sha256::digest(&admission));
    intent.extend([0; 34]);
    directory_path_state(&mut intent);
    let intent = seal_directory(intent);
    let mut stage = directory_header(b"QCS1", baseline);
    stage.extend(&intent[intent.len() - 32..]);
    for ino in [15, 13, 12] {
        directory_object(&mut stage, ino);
    }
    stage.push(1);
    stage.extend(Sha256::digest(b"qos-connected-directory-inotify-s-e-v1"));
    stage.push(3);
    let stage = seal_directory(stage);
    let mut outcome = directory_header(b"QCO1", baseline);
    outcome.extend(&intent[intent.len() - 32..]);
    outcome.extend(&stage[stage.len() - 32..]);
    for ino in [13, 15] {
        outcome.push(1);
        directory_object(&mut outcome, ino);
    }
    outcome.extend([3, 3]);
    outcome.extend(1u64.to_le_bytes());
    let outcome = seal_directory(outcome);
    [admission, intent, stage, outcome]
}
#[test]
fn chain_selection_vectors_directory_history_absence_and_native_refusal() {
    use crate::connection::transaction::native::descriptor::directory as d;
    for baseline in [false, true] {
        let [a, i, s, o] = directory_wire_facts(baseline);
        let admission =
            d::HistoricalAdmission::decode(&a).expect("canonical inert Directory admission syntax");
        assert_eq!(admission.baseline, baseline.then_some(1));
        let intent = d::decode_history(&i).unwrap();
        let stage = d::decode_history(&s).unwrap();
        let outcome = d::decode_history(&o).unwrap();
        admission.joined(&a, &intent).unwrap();
        stage.joined(&intent).unwrap();
        outcome.joined_outcome(&intent, &stage).unwrap();
        if baseline {
            assert_eq!(d::Admission::decode(&a).unwrap().encode().unwrap(), a);
            for b in [&i, &s, &o] {
                let decoded = d::decode(b).unwrap();
                assert_eq!(d::encode(&decoded.header, &decoded.body).unwrap(), *b);
            }
        } else {
            assert!(d::Admission::decode(&a).is_err());
            for b in [&i, &s, &o] {
                assert!(d::decode(b).is_err());
                let decoded = d::decode_history(b).unwrap();
                assert!(d::encode(&decoded.header, &decoded.body).is_err());
            }
        }
        for (bytes, offset) in [(&a, 48), (&i, 96), (&s, 96), (&o, 96)] {
            for (tag, value) in [(0, 1u64), (1, 0), (2, 0), (1, 9)] {
                let mut bad = bytes[..bytes.len() - 32].to_vec();
                bad[offset] = tag;
                bad[offset + 1..offset + 9].copy_from_slice(&value.to_le_bytes());
                let bad = seal_directory(bad);
                if offset == 48 {
                    assert!(d::HistoricalAdmission::decode(&bad).is_err());
                } else {
                    assert!(d::decode_history(&bad).is_err());
                }
            }
        }
    }
    let absent = directory_wire_facts(false);
    let present = directory_wire_facts(true);
    assert!(
        d::HistoricalAdmission::decode(&absent[0])
            .unwrap()
            .joined(&absent[0], &d::decode_history(&present[1]).unwrap())
            .is_err()
    );
    assert!(
        d::decode_history(&absent[2])
            .unwrap()
            .joined(&d::decode_history(&present[1]).unwrap())
            .is_err()
    );
}

// Test-only raw history installation uses production framing, then relinquishes
// the owner and asks production reopen/reduction to accept or reject the bytes.
// This is replay evidence, never a successful live append or a native proof.
fn replay_history(
    mut store: ConnectionStore,
    path: &Path,
    change: Change,
) -> Result<ConnectionStore, StoreError> {
    let event = Event {
        schema: 1,
        predecessor: store.digest,
        change,
    };
    format::encode(&event)?;
    let mut sequence = format::next(store.sequence)?;
    framing::write_record(&mut store.log, &mut sequence, &event)?;
    storage::sync_file(&store.log)?;
    drop(store);
    ConnectionStore::open_at(path)
}
fn ordinary_baseline(store: &mut ConnectionStore, record: &ConnectionRecord) {
    let state = EntryState::File {
        sha256: [19; 32],
        bytes: 3,
        executable: false,
    };
    let ack = store
        .begin_intent(OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"old").unwrap(),
            source_before: Some(state.clone()),
            destination_before: Some(state.clone()),
            decision: Decision::VerifyEqual,
        })
        .unwrap();
    let current = store.load_connection(record.id).unwrap();
    store
        .append(Change::Verified {
            connection: record.id.0,
            serial: ack.value.serial,
            expected: current.revision,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
        })
        .unwrap();
    let current = store.load_connection(record.id).unwrap();
    let root = storage::build_pages(
        &store.storage,
        std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
            connection: record.id.0,
            revision: current.revision + 1,
            ordinal: 0,
            path: format::NativePath::Unix(b"old".to_vec()),
            state: state.into(),
        }))),
    )
    .unwrap();
    store
        .append(Change::BaselineCommit {
            directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: record.id.0,
            expected: current.revision,
            root,
            count: 1,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            initial_review: true,
        })
        .unwrap();
}
fn first_directory_wire(store: &ConnectionStore, record: &ConnectionRecord) -> serde_json::Value {
    let mut wire = first_regular_wire(store, record);
    wire["source"] = serde_json::json!({"kind":"Directory"});
    wire["native_kind"] = "DirectoryV1".into();
    let mut admission = directory_wire_facts(record.baseline_revision.is_some())[0].clone();
    admission.truncate(admission.len() - 32);
    admission[24..32]
        .copy_from_slice(&(store.connection(record.id).unwrap().serial + 1).to_le_bytes());
    admission[32..40].copy_from_slice(&record.revision.to_le_bytes());
    admission[49..57].copy_from_slice(&record.baseline_revision.unwrap_or(0).to_le_bytes());
    wire["native_identity"] = serde_json::json!(seal_directory(admission));
    let mut selected = path_vector(1, [0, 0, 0], false);
    selected[153..161].copy_from_slice(&record.revision.to_le_bytes());
    if let Some(baseline) = record.baseline_revision {
        selected[170..178].copy_from_slice(&baseline.to_le_bytes());
    } else {
        selected[169] = 0;
        selected.drain(170..178);
    }
    resign(&mut selected);
    wire["directory_chain_ref"]["digest"] =
        serde_json::json!(selected[selected.len() - 32..].to_vec());
    wire["directory_chain_anchor"] = serde_json::json!(selected);
    wire
}
#[test]
fn chain_first_next_history_first_intent_installs_exact_cursor_and_schedule() {
    for directory in [false, true] {
        for baseline in [false, true] {
            let (_tmp, path, mut store, mut record) = empty_history();
            if baseline {
                ordinary_baseline(&mut store, &record);
                record = store.load_connection(record.id).unwrap();
            }
            let wire = if directory {
                first_directory_wire(&store, &record)
            } else {
                first_regular_wire(&store, &record)
            };
            let reference = wire["directory_chain_ref"].clone();
            let serial = wire["serial"].as_u64().unwrap();
            let operation: format::Operation = serde_json::from_value(wire).unwrap();
            let store = replay_history(store, &path, Change::Intent { operation })
                .expect("canonical first-chain history must reopen");
            let c = serde_json::to_value(store.connection(record.id).unwrap()).unwrap();
            assert_eq!(
                c["directory_chain"],
                serde_json::json!({"anchor":reference["anchor"],"digest":reference["digest"],"next_index":0,"current":{"connection":record.id.0,"serial":serial},"progress_revision":record.revision+1})
            );
            assert_eq!(
                c["directory_chain_schedule"],
                serde_json::json!({"revision":1,"control_sequence":0,"state":{"kind":"Running"}})
            );
            assert_eq!(c["pending"], serial);
            assert_eq!(c["serial"], serial);
            let op = store
                .operation(OperationId {
                    connection: record.id,
                    serial,
                })
                .unwrap();
            assert!(op.initial_review.is_none());
            assert_eq!(op.native_budget, Some(format::NativeBudget::initial()));
        }
    }
}
#[test]
fn chain_first_next_history_rejects_forged_first_selection_joins() {
    for case in 0..8 {
        let (_tmp, path, store, record) = empty_history();
        let mut wire = first_regular_wire(&store, &record);
        let mut selected: Vec<u8> =
            serde_json::from_value(wire["directory_chain_anchor"].clone()).unwrap();
        match case {
            0 => selected[153..161].copy_from_slice(&2u64.to_le_bytes()),
            1 => selected[161..169].copy_from_slice(&1u64.to_le_bytes()),
            2 => selected[25] ^= 1,
            3 => {
                let content = selected.len() - 97;
                selected[content] ^= 1;
            }
            4 => {
                wire["directory_chain_ref"]["anchor"]["serial"] = 2.into();
            }
            5 => {
                wire["directory_chain_ref"]["index"] = 1.into();
            }
            6 => {
                wire["directory_chain_ref"]["predecessor"] =
                    serde_json::json!({"connection":record.id.0,"serial":1});
            }
            _ => {
                wire["directory_chain_ref"]["anchor"]["connection"] =
                    serde_json::json!(([9u8; 16]));
            }
        }
        resign(&mut selected);
        DirectoryChainSelectionV1::decode(&selected).unwrap();
        wire["directory_chain_ref"]["digest"] =
            serde_json::json!(selected[selected.len() - 32..].to_vec());
        wire["directory_chain_anchor"] = serde_json::json!(selected);
        let op: format::Operation = serde_json::from_value(wire).unwrap();
        assert!(
            replay_history(store, &path, Change::Intent { operation: op }).is_err(),
            "forged first case {case}"
        );
    }
}

fn first_history(
    directory: bool,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    let (tmp, path, store, record) = empty_history();
    let wire = if directory {
        first_directory_wire(&store, &record)
    } else {
        first_regular_wire(&store, &record)
    };
    let operation = serde_json::from_value(wire).unwrap();
    let store = replay_history(store, &path, Change::Intent { operation }).unwrap();
    (tmp, path, store, record)
}
fn chain_control(
    store: &ConnectionStore,
    id: ConnectionId,
    transition: format::DirectoryChainControlTransitionV1,
) -> Change {
    let c = store.connection(id).unwrap();
    let cursor = c.directory_chain.unwrap();
    let schedule = c.directory_chain_schedule.unwrap();
    Change::DirectoryChainControl {
        connection: id.0,
        anchor: cursor.anchor,
        digest: cursor.digest,
        expected_progress: cursor.progress_revision,
        expected_schedule: schedule.revision,
        control_sequence: schedule.control_sequence + 1,
        transition,
    }
}
fn read_reserve(store: &ConnectionStore, id: ConnectionId, recovery_only: bool) -> Change {
    let c = store.connection(id).unwrap();
    let op = store
        .operation(OperationId {
            connection: id,
            serial: c.pending.unwrap(),
        })
        .unwrap();
    let previous = op.native_budget.unwrap();
    let next = format::NativeBudget {
        version: 1,
        reserved: previous.reserved + format::READ_QUANTUM,
        sequence: previous.sequence + 1,
    };
    Change::ReadReserve {
        connection: id.0,
        serial: op.serial,
        expected: c.revision,
        fence: c.fence,
        previous,
        next,
        recovery_only,
        operation_expected: recovery_only.then_some(op.revision),
    }
}
#[test]
fn chain_progress_closure_paid_reads_advance_exact_progress_and_keep_allowance() {
    for directory in [false, true] {
        let (_tmp, path, mut store, record) = first_history(directory);
        for sequence in 1..=3 {
            let before = store.connection(record.id).unwrap();
            let change = read_reserve(&store, record.id, false);
            store = replay_history(store, &path, change).unwrap();
            let after = store.connection(record.id).unwrap();
            let operation = store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1,
                })
                .unwrap();
            assert_eq!(
                after.directory_chain.as_ref().unwrap().progress_revision,
                before.revision + 1
            );
            assert_eq!(operation.revision, after.revision);
            assert_eq!(operation.stage, 0);
            assert_eq!(
                operation.native_budget.unwrap(),
                format::NativeBudget {
                    version: 1,
                    reserved: sequence * format::READ_QUANTUM,
                    sequence
                }
            );
            assert_eq!(after.pending, Some(1));
            assert_eq!(after.serial, 1);
        }
    }
}
#[test]
fn chain_controls_stop_request_preserves_ordinary_progress_and_exact_sequence() {
    for reason in [format::PauseOrCancel::Pause, format::PauseOrCancel::Cancel] {
        let (_tmp, path, store, record) = first_history(false);
        let before = store.connection(record.id).unwrap();
        let change = chain_control(
            &store,
            record.id,
            format::DirectoryChainControlTransitionV1::StopRequested { reason },
        );
        let store = replay_history(store, &path, change).expect("exact historical stop request");
        let after = store.connection(record.id).unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.directory_chain, before.directory_chain);
        assert_eq!(after.pending, before.pending);
        assert_eq!(
            after.directory_chain_schedule,
            Some(format::DirectoryChainScheduleV1 {
                revision: 2,
                control_sequence: 1,
                state: format::DirectoryChainScheduleState::Stopping { reason }
            })
        );
        let change = read_reserve(&store, record.id, false);
        assert!(
            replay_history(store, &path, change).is_err(),
            "healthy read reservation while stopping"
        );
    }
}
#[test]
fn chain_stop_history_never_begun_roundtrip_preserves_member_phase_and_budget() {
    for directory in [false, true] {
        for reason in [format::PauseOrCancel::Pause, format::PauseOrCancel::Cancel] {
            let (_tmp, path, store, record) = first_history(directory);
            let change = chain_control(
                &store,
                record.id,
                format::DirectoryChainControlTransitionV1::StopRequested { reason },
            );
            let store = replay_history(store, &path, change).unwrap();
            let before = store.connection(record.id).unwrap();
            let original = store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1,
                })
                .unwrap();
            let evidence = if directory {
                format::DirectoryChainStopEvidenceV1::Directory {
                    fact: format::DirectoryBoundaryFact::Untouched,
                }
            } else {
                format::DirectoryChainStopEvidenceV1::Regular { evidence: None }
            };
            let change = Change::DirectoryChainStopBoundary {
                connection: record.id.0,
                serial: 1,
                expected: before.revision,
                expected_schedule: 2,
                fence: before.fence,
                kind: format::ChainStopKind::NeverBegun,
                evidence: evidence.clone(),
            };
            let store =
                replay_history(store, &path, change).expect("exact never-begun stop accounting");
            let after = store.connection(record.id).unwrap();
            let operation = store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1,
                })
                .unwrap();
            assert_eq!(operation.stage, original.stage);
            assert_eq!(operation.native_budget, original.native_budget);
            assert_eq!(after.pending, Some(1));
            assert_eq!(
                after.directory_chain.as_ref().unwrap().progress_revision,
                after.revision
            );
            assert_eq!(
                operation.directory_chain_stop_boundary,
                Some(format::DirectoryChainStopBoundaryV1 {
                    schedule_revision: 2,
                    operation_revision: after.revision,
                    kind: format::ChainStopKind::NeverBegun,
                    evidence
                })
            );
            let change = chain_control(
                &store,
                record.id,
                format::DirectoryChainControlTransitionV1::StopAcknowledged,
            );
            let store = replay_history(store, &path, change).unwrap();
            assert_eq!(
                store
                    .connection(record.id)
                    .unwrap()
                    .directory_chain_schedule
                    .unwrap()
                    .state,
                format::DirectoryChainScheduleState::Stopped { reason }
            );
            let change = chain_control(
                &store,
                record.id,
                format::DirectoryChainControlTransitionV1::Resume,
            );
            let store = replay_history(store, &path, change).unwrap();
            let after = store.connection(record.id).unwrap();
            assert_eq!(
                after.directory_chain_schedule.unwrap(),
                format::DirectoryChainScheduleV1 {
                    revision: 4,
                    control_sequence: 3,
                    state: format::DirectoryChainScheduleState::Running
                }
            );
            assert_eq!(after.pending, Some(1));
        }
    }
}

fn historical_registration(
    store: &ConnectionStore,
    id: ConnectionId,
) -> format::NativeRegistration {
    let c = store.connection(id).unwrap();
    let operation = store
        .operation(OperationId {
            connection: id,
            serial: c.pending.unwrap(),
        })
        .unwrap();
    format::NativeRegistration {
        native_kind: operation.native_kind,
        connection: id.0,
        serial: operation.serial,
        attempt: 1,
        nonce: [31; 16],
        side: u8::from(operation.decision == 1),
        admission: operation.expected + 1,
        baseline: operation.baseline,
        fence: c.fence,
        intent: None,
        stage: None,
        outcome: None,
        outcome_displaced: None,
    }
}
fn regular_phase_bytes(operation: &format::Operation, progress: u64) -> [Vec<u8>; 3] {
    use crate::connection::transaction::native::descriptor as d;
    let admission = d::Admission::decode(operation.native_identity.as_ref().unwrap()).unwrap();
    let header = d::Header {
        connection: operation.connection,
        operation: operation.serial,
        attempt: 1,
        nonce: [31; 16],
        epoch: [32; 16],
        session: [33; 16],
        admission: operation.expected + 1,
        baseline: operation.baseline,
        progress,
        fence: admission.fence,
    };
    let object = |ino, kind| d::Object {
        dev: 1,
        ino,
        kind,
        generation: [u8::try_from(ino).unwrap(); 32],
    };
    let intent = d::encode(
        &header,
        &d::Body::Intent(d::Intent {
            endpoints: admission.endpoints,
            direction: admission.direction,
            decision: admission.decision,
            namespace: object(14, 0o040_000),
            path: admission.path.clone(),
            chains: admission.chains.clone(),
            expected: admission.states.clone(),
            selection: d::Selection::None,
        }),
    )
    .unwrap();
    let source = usize::from(admission.direction);
    let d::State::Regular {
        object: original,
        stamp,
        content,
    } = &admission.states[source]
    else {
        panic!("regular source");
    };
    let stage = d::encode(
        &header,
        &d::Body::Stage(d::Stage {
            intent: d::decode(&intent).unwrap().digest,
            object: object(15, 0o100_000),
            content: content.clone(),
            source: d::Source::Endpoint {
                side: admission.direction,
                parent: admission.chains[source].last().unwrap().clone(),
                object: original.clone(),
                stamp: stamp.clone(),
            },
        }),
    )
    .unwrap();
    // Independently spell the historical CreatedRegular profile. This is inert
    // data, not the native encoder/creation witness or an executor invocation.
    let mut stage = stage[..stage.len() - 32].to_vec();
    stage[..4].copy_from_slice(b"QNS3");
    let stage = seal_directory(stage);
    let mut states = admission.states.clone();
    states[1 - source] = d::State::Regular {
        object: object(15, 0o100_000),
        stamp: stamp.clone(),
        content: content.clone(),
    };
    let outcome = d::encode(
        &d::Header {
            progress: progress + 1,
            ..header
        },
        &d::Body::Outcome(d::Outcome {
            intent: d::decode(&intent).unwrap().digest,
            stage: Some(d::decode(&stage).unwrap().digest),
            displaced: None,
            states,
            kind: 0,
            barriers: 3,
            recovery: vec![],
            sequence: 1,
        }),
    )
    .unwrap();
    [intent, stage, outcome]
}
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Private fixture offsets and revisions are bounded by the explicit wire vectors and finite test phases."
)]
fn directory_phase_bytes(operation: &format::Operation, progress: u64) -> [Vec<u8>; 3] {
    let [_, intent, stage, outcome] = directory_wire_facts(operation.baseline.is_some());
    let patch_header = |bytes: Vec<u8>, revision: u64| {
        let mut bytes = bytes[..bytes.len() - 32].to_vec();
        bytes[8..24].copy_from_slice(&operation.connection);
        bytes[24..32].copy_from_slice(&operation.serial.to_le_bytes());
        bytes[88..96].copy_from_slice(&(operation.expected + 1).to_le_bytes());
        bytes[97..105].copy_from_slice(&operation.baseline.unwrap_or(0).to_le_bytes());
        bytes[105..113].copy_from_slice(&revision.to_le_bytes());
        bytes
    };
    let mut intent = patch_header(intent, progress);
    intent[303..335].copy_from_slice(&format::digest(operation.native_identity.as_ref().unwrap()));
    let admission = operation.native_identity.as_ref().unwrap();
    intent.truncate(369);
    intent.extend(&admission[187..admission.len() - 32]);
    let intent = seal_directory(intent);
    let mut stage = patch_header(stage, progress);
    stage[121..153].copy_from_slice(&intent[intent.len() - 32..]);
    if operation.path.to_relative().unwrap().components().len() == 2 {
        let mut parent = Vec::new();
        directory_object(&mut parent, 22);
        stage[257..309].copy_from_slice(&parent);
    }
    let stage = seal_directory(stage);
    let mut outcome = patch_header(outcome, progress + 1);
    outcome[121..153].copy_from_slice(&intent[intent.len() - 32..]);
    outcome[153..185].copy_from_slice(&stage[stage.len() - 32..]);
    [intent, stage.clone(), seal_directory(outcome)]
}
fn phase_history(
    directory: bool,
    phase: u8,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    let (tmp, path, store, record) = first_history(directory);
    advance_member(tmp, path, store, record, phase)
}
fn advance_member(
    tmp: tempfile::TempDir,
    path: std::path::PathBuf,
    mut store: ConnectionStore,
    record: ConnectionRecord,
    phase: u8,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    if phase == 0 {
        return (tmp, path, store, record);
    }
    let mut registration = historical_registration(&store, record.id);
    let expected = store.connection(record.id).unwrap().revision;
    store = replay_history(
        store,
        &path,
        Change::NativeReserve {
            registration: registration.clone(),
            expected,
        },
    )
    .unwrap();
    let operation = store
        .operation(OperationId {
            connection: record.id,
            serial: store.connection(record.id).unwrap().pending.unwrap(),
        })
        .unwrap();
    let [intent, stage, outcome] = if operation.native_kind.is_some() {
        directory_phase_bytes(&operation, operation.revision)
    } else {
        regular_phase_bytes(&operation, operation.revision)
    };
    registration.intent = Some(intent);
    registration.stage = Some(stage);
    store = replay_history(
        store,
        &path,
        Change::NativePrepared {
            registration: registration.clone(),
            expected: operation.revision,
        },
    )
    .expect("production prepared member history");
    if phase >= 2 {
        registration.outcome = Some(outcome);
        let expected = store.connection(record.id).unwrap().revision;
        store = replay_history(
            store,
            &path,
            Change::NativeOutcome {
                registration: registration.clone(),
                expected,
                verified: false,
                reconstructed: false,
            },
        )
        .expect("production published member history");
    }
    if phase >= 3 {
        let expected = store.connection(record.id).unwrap().revision;
        store = replay_history(
            store,
            &path,
            Change::NativeOutcome {
                registration,
                expected,
                verified: true,
                reconstructed: false,
            },
        )
        .expect("production verified member history");
    }
    (tmp, path, store, record)
}
#[test]
fn chain_progress_closure_canonical_registration_prepared_published_verified() {
    for directory in [false, true] {
        for phase in 1..=3 {
            let (_tmp, _path, store, record) = phase_history(directory, phase);
            let c = store.connection(record.id).unwrap();
            let op = store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1,
                })
                .unwrap();
            assert_eq!(op.stage, if phase == 1 { 6 } else { phase });
            assert_eq!(c.directory_chain.unwrap().progress_revision, op.revision);
            assert_eq!(op.native_budget, Some(format::NativeBudget::initial()));
            assert_eq!(c.pending, Some(1));
        }
    }
}
#[test]
fn chain_stop_history_staged_and_published_typed_evidence_preserves_phase() {
    for directory in [false, true] {
        for phase in [1, 2, 3] {
            for reason in [format::PauseOrCancel::Pause, format::PauseOrCancel::Cancel] {
                let (_tmp, path, store, record) = phase_history(directory, phase);
                let change = chain_control(
                    &store,
                    record.id,
                    format::DirectoryChainControlTransitionV1::StopRequested { reason },
                );
                let store = replay_history(store, &path, change).unwrap();
                let c = store.connection(record.id).unwrap();
                let original = store
                    .operation(OperationId {
                        connection: record.id,
                        serial: 1,
                    })
                    .unwrap();
                let mut key = format::key(5, record.id.0, 1);
                key.extend(1u32.to_be_bytes());
                let Some(Row::NativeRegistration(registration)) = store.row(&key).unwrap() else {
                    panic!("registered attempt");
                };
                let kind = if phase == 1 {
                    format::ChainStopKind::StagedOnly
                } else {
                    format::ChainStopKind::PublishedAccounted
                };
                let evidence = if directory {
                    format::DirectoryChainStopEvidenceV1::Directory {
                        fact: if phase == 1 {
                            format::DirectoryBoundaryFact::Staged { attempt: 1 }
                        } else {
                            format::DirectoryBoundaryFact::Published {
                                attempt: 1,
                                outcome: registration.outcome,
                            }
                        },
                    }
                } else {
                    format::DirectoryChainStopEvidenceV1::Regular {
                        evidence: Some(format::BoundaryEvidence {
                            attempt: 1,
                            displaced: None,
                            outcome: registration.outcome,
                        }),
                    }
                };
                let store = replay_history(
                    store,
                    &path,
                    Change::DirectoryChainStopBoundary {
                        connection: record.id.0,
                        serial: 1,
                        expected: c.revision,
                        expected_schedule: 2,
                        fence: c.fence,
                        kind,
                        evidence,
                    },
                )
                .expect("canonical typed stop accounting");
                let op = store
                    .operation(OperationId {
                        connection: record.id,
                        serial: 1,
                    })
                    .unwrap();
                assert_eq!(op.stage, original.stage);
                assert_eq!(op.native_budget, original.native_budget);
                assert_eq!(store.connection(record.id).unwrap().pending, Some(1));
            }
        }
    }
}

fn baseline_change(store: &ConnectionStore, id: ConnectionId) -> Change {
    let c = store.connection(id).unwrap();
    let operation = store
        .operation(OperationId {
            connection: id,
            serial: c.pending.unwrap(),
        })
        .unwrap();
    let mut entries = if c.baseline.is_some() {
        store
            .load_baseline(id)
            .unwrap()
            .entries()
            .cloned()
            .unwrap_or_default()
    } else {
        std::collections::BTreeMap::new()
    };
    let state = if operation.decision == 1 {
        operation.source.clone()
    } else {
        operation.destination.clone()
    }
    .unwrap();
    entries.insert(operation.path.to_relative().unwrap(), state.into());
    let count = u32::try_from(entries.len()).unwrap();
    let rows = entries
        .into_iter()
        .enumerate()
        .map(|(ordinal, (path, state))| {
            Ok(Row::Baseline(format::BaselineEntry {
                connection: id.0,
                revision: c.revision + 1,
                ordinal: u32::try_from(ordinal).unwrap(),
                path: format::NativePath::relative(&path),
                state: state.into(),
            }))
        });
    let root = storage::build_pages(&store.storage, rows).unwrap();
    Change::BaselineCommit {
        connection: id.0,
        expected: c.revision,
        root,
        count,
        source: c.source.identity,
        destination: c.destination.identity,
        initial_review: false,
        existing_ancestors: None,
        directory_ancestors: None,
        directory_chain: Some(format::DirectoryChainCommitV1 {
            member: operation.directory_chain_ref.unwrap(),
            expected_progress: c.directory_chain.unwrap().progress_revision,
        }),
    }
}
#[test]
fn chain_atomic_baseline_first_absent_no_prefix_retires_completed_atomically() {
    for directory in [false, true] {
        let (_tmp, path, store, record) = phase_history(directory, 3);
        let change = baseline_change(&store, record.id);
        let before = store.connection(record.id).unwrap();
        let store =
            replay_history(store, &path, change).expect("exact first-chain baseline commit");
        let after = store.connection(record.id).unwrap();
        assert_eq!(after.baseline, Some(before.revision + 1));
        assert_eq!(after.pending, None);
        assert!(after.directory_chain.is_none());
        assert!(after.directory_chain_schedule.is_none());
        let anchor = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap();
        assert_eq!(anchor.stage, 4);
        let retired = anchor.directory_chain_retired.unwrap();
        assert_eq!(retired.reason, format::ChainRetirementReason::Completed);
        assert_eq!(retired.cursor.current, None);
        assert_eq!(retired.cursor.next_index, 1);
        assert_eq!(
            retired.cursor.predecessor,
            Some(format::ChainOperationIdV1 {
                connection: record.id.0,
                serial: 1
            })
        );
        assert_eq!(retired.cursor.progress_revision, after.revision);
        let baseline = store.load_baseline(record.id).unwrap();
        assert_eq!(baseline.entries().unwrap().len(), 1);
        assert_eq!(
            baseline
                .entries()
                .unwrap()
                .get(&RelativePath::unix(b"a").unwrap()),
            Some(&if directory {
                EntryState::Directory
            } else {
                EntryState::File {
                    sha256: [6; 32],
                    bytes: 123,
                    executable: true,
                }
            })
        );
    }
}
#[test]
fn chain_abandon_terminal_never_begun_accounting_and_retirement_keeps_rows() {
    for directory in [false, true] {
        let (_tmp, path, store, record) = first_history(directory);
        let change = chain_control(
            &store,
            record.id,
            format::DirectoryChainControlTransitionV1::AbandonRequested,
        );
        let store = replay_history(store, &path, change).expect("historical abandonment request");
        let c = store.connection(record.id).unwrap();
        let change = if directory {
            Change::DirectoryBoundary {
                connection: record.id.0,
                serial: 1,
                expected: c.revision,
                operation_expected: c.revision,
                fence: c.fence,
                fact: format::DirectoryBoundaryFact::Untouched,
            }
        } else {
            Change::NativeBoundary {
                connection: record.id.0,
                serial: 1,
                expected: c.revision,
                operation_expected: c.revision,
                fence: c.fence,
                kind: 0,
                evidence: None,
            }
        };
        let store = replay_history(store, &path, change).unwrap();
        assert_eq!(store.connection(record.id).unwrap().pending, None);
        let change = chain_control(
            &store,
            record.id,
            format::DirectoryChainControlTransitionV1::Retire,
        );
        let store = replay_history(store, &path, change).expect("accounted abandonment retirement");
        let c = store.connection(record.id).unwrap();
        assert!(c.directory_chain.is_none() && c.directory_chain_schedule.is_none());
        let anchor = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap();
        assert_eq!(anchor.stage, 7);
        assert_eq!(
            anchor.directory_chain_retired.unwrap().reason,
            format::ChainRetirementReason::Abandoned
        );
        assert!(anchor.directory_chain_anchor.is_some());
        assert!(anchor.directory_chain_ref.is_some());
    }
}
#[test]
fn chain_abandon_terminal_live_fence_preserves_pending_and_freezes_progress() {
    for directory in [false, true] {
        for phase in 0..=3 {
            let (_tmp, path, mut store, record) = phase_history(directory, phase);
            let before = store.connection(record.id).unwrap();
            let operation = store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1,
                })
                .unwrap();
            let fence = store
                .request_disconnect(record.id, before.revision)
                .unwrap();
            assert!(matches!(
                store.finish_disconnect(&fence),
                Err(StoreError::RecoveryRequired)
            ));
            drop(store);
            let store = ConnectionStore::open_at(&path).unwrap();
            let after = store.connection(record.id).unwrap();
            assert_eq!(
                after.directory_chain_schedule.unwrap().state,
                format::DirectoryChainScheduleState::Disconnected
            );
            assert_eq!(after.directory_chain, before.directory_chain);
            assert_eq!(after.pending, Some(1));
            assert_eq!(
                store
                    .operation(OperationId {
                        connection: record.id,
                        serial: 1
                    })
                    .unwrap(),
                operation
            );
        }
    }
}
#[test]
fn chain_checkpoint_closure_empty_log_rejects_forged_cursor_join() {
    for directory in [false, true] {
        let (_tmp, path, mut store, record) = first_history(directory);
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert!(store.overlay.is_empty());
        let rows: Vec<_> = store
            .rows()
            .unwrap()
            .map(|row| {
                let mut row = row.unwrap();
                if let Row::Connection(c) = &mut row {
                    c.directory_chain.as_mut().unwrap().digest[0] ^= 1;
                }
                Ok(row)
            })
            .collect();
        let mut next = store.current.clone();
        next.root = storage::build_pages(&store.storage, rows.into_iter()).unwrap();
        store
            .publish_current(&next, &store.log, &store.log_identity)
            .unwrap();
        drop(store);
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "forged cursor digest on empty-log checkpoint for {:?}",
            record.id
        );
    }
}

fn nested_chain_first() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    let (tmp, path, store, record) = empty_history();
    let mut wire = first_directory_wire(&store, &record);
    let mut selection = selection_vector(false, true);
    selection[153..161].copy_from_slice(&record.revision.to_le_bytes());
    selection[169] = 0;
    selection.drain(170..178);
    resign(&mut selection);
    wire["directory_chain_ref"]["digest"] =
        serde_json::json!(selection[selection.len() - 32..].to_vec());
    wire["directory_chain_anchor"] = serde_json::json!(selection);
    let operation = serde_json::from_value(wire).unwrap();
    let store = replay_history(store, &path, Change::Intent { operation }).unwrap();
    advance_member(tmp, path, store, record, 3)
}
fn successor_wire(store: &ConnectionStore, record: &ConnectionRecord) -> format::Operation {
    use crate::connection::transaction::native::descriptor as d;
    let current = store.load_connection(record.id).unwrap();
    let mut wire = first_regular_wire(store, &current);
    let cursor = store
        .connection(record.id)
        .unwrap()
        .directory_chain
        .unwrap();
    wire.as_object_mut()
        .unwrap()
        .remove("directory_chain_anchor");
    wire["directory_chain_ref"] = serde_json::to_value(format::DirectoryChainRefV1 {
        anchor: cursor.anchor,
        digest: cursor.digest,
        index: cursor.next_index,
        predecessor: cursor.predecessor,
    })
    .unwrap();
    let mut operation: format::Operation = serde_json::from_value(wire).unwrap();
    let mut admission = d::Admission::decode(operation.native_identity.as_ref().unwrap()).unwrap();
    // The independent selection vector spells a/b at depth2; source/target parent identities join the
    // inert first Directory outcome, and the regular leaf is a distinct object.
    admission.path = RelativePath::unix(b"a/b").unwrap();
    let object = |ino| d::Object {
        dev: 1,
        ino,
        kind: 0o040_000,
        generation: [u8::try_from(ino).unwrap(); 32],
    };
    admission.chains[0].push(object(13));
    admission.chains[1].push(object(15));
    if let d::State::Regular { object, .. } = &mut admission.states[0] {
        object.ino = 21;
        object.generation = [21; 32];
    }
    operation.path = format::NativePath::relative(&admission.path);
    operation.native_identity = Some(admission.encode().unwrap());
    operation
}
#[test]
fn chain_first_next_history_committed_parent_then_exact_regular_successor() {
    let (tmp, path, store, record) = nested_chain_first();
    let change = baseline_change(&store, record.id);
    let store = replay_history(store, &path, change).unwrap();
    let parent = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    assert_eq!(parent.stage, 4);
    assert!(parent.directory_chain_retired.is_none());
    let before = store.connection(record.id).unwrap();
    assert_eq!(before.directory_chain.as_ref().unwrap().next_index, 1);
    assert_eq!(before.directory_chain.as_ref().unwrap().current, None);
    let operation = successor_wire(&store, &record);
    let selection =
        DirectoryChainSelectionV1::decode(parent.directory_chain_anchor.as_ref().unwrap()).unwrap();
    assert_eq!(
        selection.member(1).unwrap().path,
        operation.path.to_relative().unwrap(),
        "valid successor fixture uses selected path"
    );
    let store = replay_history(store, &path, Change::Intent { operation })
        .expect("exact successor after committed predecessor");
    let (_tmp, path, store, record) = advance_member(tmp, path, store, record, 3);
    let change = baseline_change(&store, record.id);
    let store = replay_history(store, &path, change).unwrap();
    assert!(
        store
            .connection(record.id)
            .unwrap()
            .directory_chain
            .is_none()
    );
    let anchor = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let retired = anchor.directory_chain_retired.unwrap();
    assert_eq!(retired.reason, format::ChainRetirementReason::Completed);
    assert_eq!(retired.cursor.next_index, 2);
    assert_eq!(retired.cursor.predecessor.unwrap().serial, 2);
    let baseline = store.load_baseline(record.id).unwrap();
    let entries = baseline.entries().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[&RelativePath::unix(b"a").unwrap()],
        EntryState::Directory
    );
    assert!(matches!(
        entries[&RelativePath::unix(b"a/b").unwrap()],
        EntryState::File { bytes: 123, .. }
    ));
}

#[test]
fn chain_checkpoint_closure_requires_registration_even_after_completed_retirement() {
    for directory in [false, true] {
        for completed in [false, true] {
            let (_tmp, path, mut store, record) = phase_history(directory, 3);
            if completed {
                let change = baseline_change(&store, record.id);
                store = replay_history(store, &path, change).unwrap();
            }
            store.checkpoint().unwrap();
            let rows: Vec<_> = store
                .rows()
                .unwrap()
                .filter_map(|row| match row.unwrap() {
                    Row::NativeRegistration(_) => None,
                    row => Some(Ok(row)),
                })
                .collect();
            let mut next = store.current.clone();
            next.root = storage::build_pages(&store.storage, rows.into_iter()).unwrap();
            store
                .publish_current(&next, &store.log, &store.log_identity)
                .unwrap();
            drop(store);
            assert!(
                ConnectionStore::open_at(&path).is_err(),
                "required registration removed: directory={directory}, completed={completed}"
            );
        }
    }
}
fn unmarked_intent(store: &ConnectionStore, id: ConnectionId) -> OperationIntent {
    let c = store.load_connection(id).unwrap();
    OperationIntent {
        connection: id,
        expected_revision: c.revision,
        expected_baseline: c.baseline_revision,
        path: RelativePath::unix(b"unrelated").unwrap(),
        source_before: Some(EntryState::Directory),
        destination_before: Some(EntryState::Directory),
        decision: Decision::VerifyEqual,
    }
}
#[test]
fn chain_live_denial_legacy_completed_retained_history_denies_fresh_unmarked_work() {
    for directory in [false, true] {
        for checkpoint in [false, true] {
            let (_tmp, path, store, record) = phase_history(directory, 3);
            let change = baseline_change(&store, record.id);
            let mut store = replay_history(store, &path, change).unwrap();
            if checkpoint {
                store.checkpoint().unwrap();
                drop(store);
                store = ConnectionStore::open_at(&path).unwrap();
                assert!(store.overlay.is_empty());
            }
            let before = store.connection(record.id).unwrap();
            let anchor = store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1,
                })
                .unwrap();
            assert!(before.directory_chain.is_none() && before.directory_chain_schedule.is_none());
            assert!(before.baseline.is_some() && anchor.directory_chain_retired.is_some());
            assert!(
                matches!(
                    store.transaction_session(record.id),
                    Err(StoreError::RecoveryRequired)
                ),
                "retained retirement must deny fresh session"
            );
            let intent = unmarked_intent(&store, record.id);
            let candidate =
                format::Operation::new(intent.clone(), before.serial + 1, &before).unwrap();
            assert!(
                candidate.directory_chain_ref.is_none()
                    && candidate.directory_chain_anchor.is_none()
            );
            assert!(matches!(
                store.begin_intent(intent),
                Err(StoreError::RecoveryRequired)
            ));
            assert!(matches!(
                store.append(Change::Intent {
                    operation: candidate
                }),
                Err(StoreError::RecoveryRequired)
            ));
            assert_eq!(store.connection(record.id).unwrap(), before);
            assert_eq!(
                store
                    .operation(OperationId {
                        connection: record.id,
                        serial: 1
                    })
                    .unwrap(),
                anchor
            );
        }
    }
}
// Private fixture probe selects historical validation only under cfg(test).
// Production live append still rejects NativeReplay and all retained chains.
fn historical_append(store: &mut ConnectionStore, change: Change) -> Result<(), StoreError> {
    let checks = store.replay_checks(&change)?;
    let probe = HistoricalAppendProbe::default();
    store.append_checked_with_baseline(change, checks, BaselineAppendCheck::Historical(&probe))
}
#[derive(Default)]
pub(super) struct HistoricalAppendProbe {
    calls: std::cell::Cell<usize>,
    reject_call: usize,
}
impl HistoricalAppendProbe {
    pub(super) fn validate(&self, store: &ConnectionStore) -> Result<(), StoreError> {
        store.validate()?;
        self.calls.set(self.calls.get() + 1);
        if self.calls.get() == self.reject_call {
            Err(StoreError::RecoveryRequired)
        } else {
            Ok(())
        }
    }
}
#[test]
fn chain_persistence_faults_common_historical_pipeline_preserves_live_mode_barrier() {
    for directory in [false, true] {
        let (_tmp, path, mut store, record) = empty_history();
        let cached = store.transaction_session(record.id).unwrap();
        let wire = if directory {
            first_directory_wire(&store, &record)
        } else {
            first_regular_wire(&store, &record)
        };
        historical_append(
            &mut store,
            Change::Intent {
                operation: serde_json::from_value(wire).unwrap(),
            },
        )
        .unwrap();
        let mut registration = historical_registration(&store, record.id);
        let expected = store.connection(record.id).unwrap().revision;
        historical_append(
            &mut store,
            Change::NativeReserve {
                registration: registration.clone(),
                expected,
            },
        )
        .unwrap();
        let operation = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap();
        let [intent, stage, outcome] = if directory {
            directory_phase_bytes(&operation, operation.revision)
        } else {
            regular_phase_bytes(&operation, operation.revision)
        };
        registration.intent = Some(intent);
        registration.stage = Some(stage);
        historical_append(
            &mut store,
            Change::NativePrepared {
                registration: registration.clone(),
                expected: operation.revision,
            },
        )
        .unwrap();
        registration.outcome = Some(outcome);
        for verified in [false, true] {
            let expected = store.connection(record.id).unwrap().revision;
            let change = Change::NativeOutcome {
                registration: registration.clone(),
                expected,
                verified,
                reconstructed: false,
            };
            assert!(
                matches!(
                    store.append_checked(change.clone(), store.replay_checks(&change).unwrap()),
                    Err(StoreError::RecoveryRequired)
                ),
                "ordinary live append never accepts replay authority"
            );
            historical_append(&mut store, change)
                .expect("private historical fixture uses common append pipeline");
        }
        let change = baseline_change(&store, record.id);
        historical_append(&mut store, change).unwrap();
        let before = store.connection(record.id).unwrap();
        let intent = unmarked_intent(&store, record.id);
        assert!(
            matches!(
                store.begin_native_operation(intent, &cached, None, None, None, None),
                Err(StoreError::RecoveryRequired)
            ),
            "current same-store cached session cannot bypass retained history"
        );
        assert_eq!(store.connection(record.id).unwrap(), before);
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(store.connection(record.id).unwrap(), before);
    }
}

#[cfg(unix)]
#[test]
fn chain_actual_capacity_near_cap_real_connection_preflights_future_schedule() {
    use std::os::unix::ffi::OsStringExt;
    let (_template_tmp, _template_path, template, record) = empty_history();
    let operation: format::Operation =
        serde_json::from_value(first_regular_wire(&template, &record)).unwrap();
    let rows = template.reduce_intent(&operation).unwrap();
    let Row::Connection(prospective) = &rows[0] else {
        panic!("production prospective connection");
    };
    let locator = |units: usize| {
        let mut bytes = vec![200; units];
        bytes[0] = b'/';
        format::NativePath::Unix(bytes)
    };
    let page_size = |units| {
        let mut connection = prospective.clone();
        connection.source.locator = locator(units);
        format::encode(&format::PageNode {
            schema: 1,
            node: format::Node::Leaf(vec![Row::Connection(connection)]),
        })
        .map(|bytes| bytes.len())
    };
    let (mut low, mut high) = (1usize, 32768usize);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if page_size(middle).is_ok() {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let maximal = low;
    assert!(maximal > 32000);
    assert!(page_size(maximal + 1).is_err());
    eprintln!(
        "actual capacity: locator={maximal} native units, prospective singleton={} bytes",
        page_size(maximal).unwrap()
    );
    for (units, succeeds) in [(maximal, false), (maximal - 512, true)] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().canonicalize().unwrap().join("connections");
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let format::NativePath::Unix(bytes) = locator(units) else {
            unreachable!()
        };
        let source_path = std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes));
        let mut source = record.source.clone();
        source.locator = EndpointLocator::new(&source_path).unwrap();
        let record = store
            .create_connection(record.id, source, record.destination.clone(), record.policy)
            .unwrap()
            .value;
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        assert!(store.overlay.is_empty());
        let before = store.connection(record.id).unwrap();
        let operation: format::Operation =
            serde_json::from_value(first_regular_wire(&store, &record)).unwrap();
        let result = historical_append(&mut store, Change::Intent { operation });
        if succeeds {
            result.expect("nearby real connection retains future chain capacity");
            store.checkpoint().unwrap();
            drop(store);
            assert!(
                ConnectionStore::open_at(&path)
                    .unwrap()
                    .connection(record.id)
                    .unwrap()
                    .directory_chain
                    .is_some()
            );
        } else {
            assert!(
                matches!(result, Err(StoreError::LimitExceeded)),
                "first admission must reserve future stop/schedule capacity before serial or pending changes"
            );
            assert_eq!(store.connection(record.id).unwrap(), before);
        }
    }
}

fn unchanged_refusal(store: &mut ConnectionStore, change: Change) {
    let before = format::encode(
        &store
            .rows()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
    )
    .unwrap();
    let length = store.log.metadata().unwrap().len();
    assert!(historical_append(store, change).is_err());
    assert_eq!(
        store.log.metadata().unwrap().len(),
        length,
        "refusal wrote history"
    );
    assert_eq!(
        format::encode(
            &store
                .rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        )
        .unwrap(),
        before,
        "refusal changed durable rows"
    );
}
fn stop_change(store: &ConnectionStore, id: ConnectionId, phase: u8, directory: bool) -> Change {
    let c = store.connection(id).unwrap();
    let outcome = if phase >= 2 {
        let mut key = format::key(5, id.0, 1);
        key.extend(1u32.to_be_bytes());
        let Some(Row::NativeRegistration(r)) = store.row(&key).unwrap() else {
            panic!("attempt");
        };
        r.outcome
    } else {
        None
    };
    let kind = match phase {
        0 => format::ChainStopKind::NeverBegun,
        1 => format::ChainStopKind::StagedOnly,
        _ => format::ChainStopKind::PublishedAccounted,
    };
    let evidence = if directory {
        format::DirectoryChainStopEvidenceV1::Directory {
            fact: match phase {
                0 => format::DirectoryBoundaryFact::Untouched,
                1 => format::DirectoryBoundaryFact::Staged { attempt: 1 },
                _ => format::DirectoryBoundaryFact::Published {
                    attempt: 1,
                    outcome,
                },
            },
        }
    } else {
        format::DirectoryChainStopEvidenceV1::Regular {
            evidence: (phase > 0).then_some(format::BoundaryEvidence {
                attempt: 1,
                displaced: None,
                outcome,
            }),
        }
    };
    Change::DirectoryChainStopBoundary {
        connection: id.0,
        serial: 1,
        expected: c.revision,
        expected_schedule: c.directory_chain_schedule.unwrap().revision,
        fence: c.fence,
        kind,
        evidence,
    }
}
#[test]
fn chain_controls_stale_duplicate_overflow_and_paid_progress_need_fresh_boundary() {
    let (_tmp, _path, mut store, record) = first_history(false);
    let valid = chain_control(
        &store,
        record.id,
        format::DirectoryChainControlTransitionV1::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    for case in 0..7 {
        let mut change = valid.clone();
        if let Change::DirectoryChainControl {
            anchor,
            digest,
            expected_progress,
            expected_schedule,
            control_sequence,
            transition,
            ..
        } = &mut change
        {
            match case {
                0 => anchor.serial += 1,
                1 => digest[0] ^= 1,
                2 => *expected_progress += 1,
                3 => *expected_schedule += 1,
                4 => *control_sequence = 0,
                5 => *control_sequence = u64::MAX,
                _ => *transition = format::DirectoryChainControlTransitionV1::StopAcknowledged,
            }
        }
        unchanged_refusal(&mut store, change);
    }
    historical_append(&mut store, valid.clone()).unwrap();
    unchanged_refusal(&mut store, valid);
    let premature = chain_control(
        &store,
        record.id,
        format::DirectoryChainControlTransitionV1::StopAcknowledged,
    );
    unchanged_refusal(&mut store, premature);
    let boundary = stop_change(&store, record.id, 0, false);
    historical_append(&mut store, boundary).unwrap();
    let paid = read_reserve(&store, record.id, true);
    historical_append(&mut store, paid).unwrap();
    let stale = chain_control(
        &store,
        record.id,
        format::DirectoryChainControlTransitionV1::StopAcknowledged,
    );
    unchanged_refusal(&mut store, stale);
    let boundary = stop_change(&store, record.id, 0, false);
    historical_append(&mut store, boundary).unwrap();
    let ack = chain_control(
        &store,
        record.id,
        format::DirectoryChainControlTransitionV1::StopAcknowledged,
    );
    historical_append(&mut store, ack).unwrap();
    let serial = store.connection(record.id).unwrap().serial;
    let resume = chain_control(
        &store,
        record.id,
        format::DirectoryChainControlTransitionV1::Resume,
    );
    historical_append(&mut store, resume.clone()).unwrap();
    unchanged_refusal(&mut store, resume);
    assert_eq!(store.connection(record.id).unwrap().serial, serial);
}
#[test]
fn chain_stop_history_negative_evidence_and_terminal_boundary_are_not_pause() {
    for directory in [false, true] {
        for phase in 0..=3 {
            let (_tmp, _path, mut store, record) = phase_history(directory, phase);
            let request = chain_control(
                &store,
                record.id,
                format::DirectoryChainControlTransitionV1::StopRequested {
                    reason: format::PauseOrCancel::Cancel,
                },
            );
            historical_append(&mut store, request).unwrap();
            let valid = stop_change(&store, record.id, phase, directory);
            for case in 0..8 {
                let mut bad = valid.clone();
                if let Change::DirectoryChainStopBoundary {
                    serial,
                    expected,
                    expected_schedule,
                    fence,
                    kind,
                    evidence,
                    ..
                } = &mut bad
                {
                    match case {
                        0 => *serial += 1,
                        1 => *expected += 1,
                        2 => *expected_schedule += 1,
                        3 => *fence += 1,
                        4 => {
                            *kind = if phase == 0 {
                                format::ChainStopKind::StagedOnly
                            } else {
                                format::ChainStopKind::NeverBegun
                            }
                        }
                        5 => {
                            *evidence = if directory {
                                format::DirectoryChainStopEvidenceV1::Regular { evidence: None }
                            } else {
                                format::DirectoryChainStopEvidenceV1::Directory {
                                    fact: format::DirectoryBoundaryFact::Untouched,
                                }
                            }
                        }
                        6 => {
                            *evidence = if directory {
                                format::DirectoryChainStopEvidenceV1::Directory {
                                    fact: format::DirectoryBoundaryFact::Published {
                                        attempt: 8,
                                        outcome: None,
                                    },
                                }
                            } else {
                                format::DirectoryChainStopEvidenceV1::Regular {
                                    evidence: Some(format::BoundaryEvidence {
                                        attempt: 8,
                                        displaced: None,
                                        outcome: None,
                                    }),
                                }
                            }
                        }
                        _ => {
                            *evidence = format::DirectoryChainStopEvidenceV1::Regular {
                                evidence: Some(format::BoundaryEvidence {
                                    attempt: 1,
                                    displaced: Some(vec![]),
                                    outcome: None,
                                }),
                            }
                        }
                    }
                }
                unchanged_refusal(&mut store, bad);
            }
            let c = store.connection(record.id).unwrap();
            let terminal = if directory {
                Change::DirectoryBoundary {
                    connection: record.id.0,
                    serial: 1,
                    expected: c.revision,
                    operation_expected: c.revision,
                    fence: c.fence,
                    fact: format::DirectoryBoundaryFact::Untouched,
                }
            } else {
                Change::NativeBoundary {
                    connection: record.id.0,
                    serial: 1,
                    expected: c.revision,
                    operation_expected: c.revision,
                    fence: c.fence,
                    kind: 0,
                    evidence: None,
                }
            };
            unchanged_refusal(&mut store, terminal);
            historical_append(&mut store, valid).unwrap();
        }
    }
}
#[test]
fn chain_progress_closure_excluded_events_and_budget_exhaustion_are_atomic() {
    let (_tmp, _path, mut store, record) = first_history(false);
    let c = store.connection(record.id).unwrap();
    for event in [
        "Published",
        "Verified",
        "IncompleteRecovery",
        "SelectedUntouched",
        "SelectedApplied",
        "SelectedAppliedCarryV1",
        "NativeConflictComplete",
        "SelectedCollisionComplete",
        "NativeBoundary",
        "DirectoryBoundary",
    ] {
        let mut value = serde_json::json!({"event":event,"connection":record.id.0,"serial":1,"expected":c.revision});
        match event {
            "Published" | "Verified" => {
                value["source"] = serde_json::to_value(&c.source.identity).unwrap();
                value["destination"] = serde_json::to_value(&c.destination.identity).unwrap();
            }
            "IncompleteRecovery" => {
                value["slot"] = 0.into();
                value["fence"] = 0.into();
            }
            "SelectedUntouched" | "NativeBoundary" | "DirectoryBoundary" => {
                value["operation_expected"] = c.revision.into();
                value["fence"] = 0.into();
                if event == "NativeBoundary" {
                    value["kind"] = 0.into();
                }
                if event == "DirectoryBoundary" {
                    value["fact"] = serde_json::json!({"kind":"Untouched"});
                }
            }
            "SelectedApplied" | "SelectedAppliedCarryV1" => {
                if event == "SelectedAppliedCarryV1" {
                    value["carry"] = serde_json::json!({"version":1,"prior":[{
                        "operation":1,"operation_revision":2,"applied_revision":2,"fence":0,
                        "outcome":([1u8;32]),"conflict":1,"conflict_revision":1
                    }]});
                }
                value["fence"] = 0.into();
                value["attempt"] = 1.into();
                value["outcome"] = serde_json::json!(([1u8; 32]));
            }
            _ => {
                value["fence"] = 0.into();
                value["conflict_expected"] = 1.into();
                value["capture"] = serde_json::json!(([1u8; 32]));
            }
        }
        unchanged_refusal(&mut store, serde_json::from_value(value).unwrap());
    }
    let mut paid = read_reserve(&store, record.id, false);
    if let Change::ReadReserve { next, .. } = &mut paid {
        next.reserved = format::READ_LIMIT;
    }
    historical_append(&mut store, paid).unwrap();
    let overflow = read_reserve(&store, record.id, false);
    unchanged_refusal(&mut store, overflow);
    let mut wrong = read_reserve(&store, record.id, true);
    if let Change::ReadReserve { serial, .. } = &mut wrong {
        *serial = 2;
    }
    unchanged_refusal(&mut store, wrong);
}
#[test]
fn chain_persistence_faults_prewrite_postfsync_torn_tail_and_checkpoint_reread() {
    for directory in [false, true] {
        for reject_call in [1, 2] {
            let (_tmp, path, mut store, record) = phase_history(directory, 3);
            let change = baseline_change(&store, record.id);
            let old = store.connection(record.id).unwrap();
            let probe = HistoricalAppendProbe {
                reject_call,
                calls: std::cell::Cell::new(0),
            };
            let checks = store.replay_checks(&change).unwrap();
            assert!(
                store
                    .append_checked_with_baseline(
                        change,
                        checks,
                        BaselineAppendCheck::Historical(&probe)
                    )
                    .is_err()
            );
            assert_eq!(probe.calls.get(), reject_call);
            if reject_call == 1 {
                assert_eq!(store.connection(record.id).unwrap(), old);
            } else {
                assert!(matches!(
                    store.connection(record.id),
                    Err(StoreError::RecoveryRequired)
                ));
            }
            drop(store);
            let mut reopened = ConnectionStore::open_at(&path).unwrap();
            let c = reopened.connection(record.id).unwrap();
            if reject_call == 1 {
                assert_eq!(c, old);
            } else {
                assert_eq!(c.baseline, Some(old.revision + 1));
                assert!(c.directory_chain.is_none() && c.pending.is_none());
            }
            reopened.checkpoint().unwrap();
            drop(reopened);
            let reopened = ConnectionStore::open_at(&path).unwrap();
            assert_eq!(reopened.connection(record.id).unwrap(), c);
        }
        for complete in [false, true] {
            let (_tmp, path, mut store, record) = phase_history(directory, 3);
            let change = baseline_change(&store, record.id);
            let old = store.connection(record.id).unwrap();
            let event = Event {
                schema: 1,
                predecessor: store.digest,
                change,
            };
            let mut framed = tempfile::tempfile().unwrap();
            framing::write_record(&mut framed, &mut (store.sequence + 1), &event).unwrap();
            framed.seek(SeekFrom::Start(0)).unwrap();
            let mut bytes = Vec::new();
            framed.read_to_end(&mut bytes).unwrap();
            let take = if complete {
                bytes.len()
            } else {
                bytes.len() - 7
            };
            store.log.write_all(&bytes[..take]).unwrap();
            storage::sync_file(&store.log).unwrap();
            drop(store);
            let store = ConnectionStore::open_at(&path).unwrap();
            let c = store.connection(record.id).unwrap();
            if complete {
                assert!(c.directory_chain.is_none());
                assert_eq!(c.baseline, Some(old.revision + 1));
            } else {
                assert_eq!(c, old);
            }
        }
    }
}

fn terminal_change(
    store: &ConnectionStore,
    id: ConnectionId,
    phase: u8,
    directory: bool,
) -> Change {
    let Change::DirectoryChainStopBoundary {
        connection,
        serial,
        expected,
        fence,
        kind,
        evidence,
        ..
    } = stop_change(store, id, phase, directory)
    else {
        unreachable!()
    };
    match evidence {
        format::DirectoryChainStopEvidenceV1::Directory { fact } => Change::DirectoryBoundary {
            connection,
            serial,
            expected,
            operation_expected: expected,
            fence,
            fact,
        },
        format::DirectoryChainStopEvidenceV1::Regular { evidence } => Change::NativeBoundary {
            connection,
            serial,
            expected,
            operation_expected: expected,
            fence,
            kind: match kind {
                format::ChainStopKind::NeverBegun => 0,
                format::ChainStopKind::StagedOnly => 1,
                format::ChainStopKind::PublishedAccounted => 3,
            },
            evidence,
        },
    }
}
fn replace_checkpoint(store: &ConnectionStore, rows: Vec<Row>) {
    let mut next = store.current.clone();
    next.root = storage::build_pages(&store.storage, rows.into_iter().map(Ok)).unwrap();
    store
        .publish_current(&next, &store.log, &store.log_identity)
        .unwrap();
}
#[test]
fn chain_checkpoint_closure_rejects_wrong_stop_attempt_and_missing_terminal_registration() {
    for directory in [false, true] {
        for terminal in [false, true] {
            let (_tmp, path, mut store, record) = phase_history(directory, 2);
            let transition = if terminal {
                format::DirectoryChainControlTransitionV1::AbandonRequested
            } else {
                format::DirectoryChainControlTransitionV1::StopRequested {
                    reason: format::PauseOrCancel::Pause,
                }
            };
            let change = chain_control(&store, record.id, transition);
            historical_append(&mut store, change).unwrap();
            let change = if terminal {
                terminal_change(&store, record.id, 2, directory)
            } else {
                stop_change(&store, record.id, 2, directory)
            };
            historical_append(&mut store, change).unwrap();
            store.checkpoint().unwrap();
            let rows = store
                .rows()
                .unwrap()
                .filter_map(|row| {
                    let mut row = row.unwrap();
                    if terminal && matches!(row, Row::NativeRegistration(_)) {
                        return None;
                    }
                    if !terminal {
                        if let Row::Operation(op) = &mut row {
                            match &mut op.directory_chain_stop_boundary.as_mut().unwrap().evidence {
                                format::DirectoryChainStopEvidenceV1::Regular { evidence } => {
                                    evidence.as_mut().unwrap().attempt = 8
                                }
                                format::DirectoryChainStopEvidenceV1::Directory {
                                    fact: format::DirectoryBoundaryFact::Published { attempt, .. },
                                } => *attempt = 8,
                                _ => unreachable!(),
                            }
                        }
                    }
                    Some(row)
                })
                .collect();
            replace_checkpoint(&store, rows);
            drop(store);
            assert!(
                ConnectionStore::open_at(&path).is_err(),
                "retained evidence must join its registered attempt directory={directory},terminal={terminal}"
            );
        }
    }
}
#[test]
fn chain_abandon_terminal_all_phases_frozen_accounting_then_live_finish() {
    for directory in [false, true] {
        for phase in 0..=3 {
            let (_tmp, path, mut store, record) = phase_history(directory, phase);
            let before = store.connection(record.id).unwrap();
            let ack = store
                .request_disconnect(record.id, before.revision)
                .unwrap();
            assert!(matches!(
                store.open_recovery_context(
                    OperationId {
                        connection: record.id,
                        serial: 1
                    },
                    before.revision + 1
                ),
                Err(StoreError::RecoveryRequired)
            ));
            assert!(matches!(
                store.finish_disconnect(&ack),
                Err(StoreError::RecoveryRequired)
            ));
            // Canonical terminal accounting keeps the member's pre-Fence revision.
            let mut change = terminal_change(&store, record.id, phase, directory);
            match &mut change {
                Change::NativeBoundary {
                    operation_expected, ..
                }
                | Change::DirectoryBoundary {
                    operation_expected, ..
                } => *operation_expected = before.revision,
                _ => unreachable!(),
            }
            historical_append(&mut store, change).unwrap();
            assert_eq!(
                store
                    .connection(record.id)
                    .unwrap()
                    .directory_chain
                    .as_ref()
                    .unwrap()
                    .progress_revision,
                before.revision
            );
            drop(store);
            let mut store = ConnectionStore::open_at(&path).unwrap();
            let c = store.connection(record.id).unwrap();
            assert!(c.pending.is_none());
            let ack = store.request_disconnect(record.id, c.revision).unwrap();
            store.finish_disconnect(&ack).unwrap();
            assert!(matches!(
                store.transaction_session(record.id),
                Err(StoreError::RecoveryRequired)
            ));
            let intent = unmarked_intent(&store, record.id);
            assert!(matches!(
                store.begin_intent(intent),
                Err(StoreError::RecoveryRequired)
            ));
            let resume = chain_control(
                &store,
                record.id,
                format::DirectoryChainControlTransitionV1::Resume,
            );
            unchanged_refusal(&mut store, resume);
            let c = store.connection(record.id).unwrap();
            assert_eq!(c.lifecycle, 2);
            assert_eq!(
                c.directory_chain.unwrap().progress_revision,
                before.revision
            );
        }
    }
}
#[test]
fn chain_abandon_terminal_running_stopping_stopped_and_schedule_headroom() {
    for source in 0..3 {
        let (_tmp, _path, mut store, record) = first_history(false);
        if source > 0 {
            let change = chain_control(
                &store,
                record.id,
                format::DirectoryChainControlTransitionV1::StopRequested {
                    reason: format::PauseOrCancel::Cancel,
                },
            );
            historical_append(&mut store, change).unwrap();
        }
        if source == 2 {
            let change = stop_change(&store, record.id, 0, false);
            historical_append(&mut store, change).unwrap();
            let change = chain_control(
                &store,
                record.id,
                format::DirectoryChainControlTransitionV1::StopAcknowledged,
            );
            historical_append(&mut store, change).unwrap();
        }
        let request = chain_control(
            &store,
            record.id,
            format::DirectoryChainControlTransitionV1::AbandonRequested,
        );
        historical_append(&mut store, request).unwrap();
        let duplicate = chain_control(
            &store,
            record.id,
            format::DirectoryChainControlTransitionV1::AbandonRequested,
        );
        unchanged_refusal(&mut store, duplicate);
        let premature = chain_control(
            &store,
            record.id,
            format::DirectoryChainControlTransitionV1::Retire,
        );
        unchanged_refusal(&mut store, premature);
        let terminal = terminal_change(&store, record.id, 0, false);
        historical_append(&mut store, terminal).unwrap();
        let retire = chain_control(
            &store,
            record.id,
            format::DirectoryChainControlTransitionV1::Retire,
        );
        historical_append(&mut store, retire).unwrap();
    }
    let (_tmp, path, mut store, record) = first_history(false);
    store.checkpoint().unwrap();
    let rows = store
        .rows()
        .unwrap()
        .map(|row| {
            let mut row = row.unwrap();
            if let Row::Connection(c) = &mut row {
                let s = c.directory_chain_schedule.as_mut().unwrap();
                s.revision = u64::MAX - 1;
                s.control_sequence = u64::MAX - 2;
            }
            row
        })
        .collect();
    replace_checkpoint(&store, rows);
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let change = chain_control(
        &store,
        record.id,
        format::DirectoryChainControlTransitionV1::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    unchanged_refusal(&mut store, change);
    let before = store.connection(record.id).unwrap();
    store
        .request_disconnect(record.id, before.revision)
        .unwrap();
    let c = store.connection(record.id).unwrap();
    assert_eq!(c.directory_chain_schedule.unwrap().revision, u64::MAX);
    assert_eq!(c.directory_chain, before.directory_chain);
}
#[test]
fn chain_actual_capacity_codec_fixed_phase_sizes_are_covered() {
    let (_tmp, _path, store, record) = first_history(false);
    let op = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let [intent, stage, outcome] = regular_phase_bytes(&op, op.revision + 1);
    eprintln!(
        "production regular phase bytes intent={} stage={} outcome={}",
        intent.len(),
        stage.len(),
        outcome.len()
    );
    assert!(
        stage.len() <= 444,
        "current preflight Stage ceiling undercounts actual production bytes"
    );
    assert!(
        outcome.len() <= super::chain::REGULAR_OUTCOME_MAX,
        "current preflight Outcome ceiling undercounts actual production bytes"
    );
}

#[test]
fn chain_checkpoint_closure_advisory_baseline_stop_wrapper_and_swapped_outcome() {
    for case in 0..5 {
        let (_tmp, path, mut store, record) = phase_history(true, if case == 3 { 1 } else { 3 });
        if case < 2 {
            let change = baseline_change(&store, record.id);
            historical_append(&mut store, change).unwrap();
        } else {
            let transition = if case == 4 {
                format::DirectoryChainControlTransitionV1::AbandonRequested
            } else {
                format::DirectoryChainControlTransitionV1::StopRequested {
                    reason: format::PauseOrCancel::Pause,
                }
            };
            let change = chain_control(&store, record.id, transition);
            historical_append(&mut store, change).unwrap();
            let change = if case == 4 {
                terminal_change(&store, record.id, 3, true)
            } else {
                stop_change(&store, record.id, if case == 3 { 1 } else { 3 }, true)
            };
            historical_append(&mut store, change).unwrap();
            if case == 4 {
                let change = chain_control(
                    &store,
                    record.id,
                    format::DirectoryChainControlTransitionV1::Retire,
                );
                historical_append(&mut store, change).unwrap();
            }
        }
        store.checkpoint().unwrap();
        let rows = store
            .rows()
            .unwrap()
            .map(|row| {
                let mut row = row.unwrap();
                match &mut row {
                    Row::Connection(c) if case == 0 => {
                        c.baseline = None;
                        c.baseline_root = None;
                        c.baseline_count = 0;
                    }
                    Row::Connection(c) if case == 1 => c.baseline = Some(c.baseline.unwrap() - 1),
                    Row::Operation(op) if case == 2 || case == 3 => {
                        let fact = op.directory_chain_stop_boundary.as_mut().unwrap();
                        fact.kind = format::ChainStopKind::NeverBegun;
                        fact.evidence =
                            format::DirectoryChainStopEvidenceV1::Regular { evidence: None };
                    }
                    Row::Operation(op) if case == 4 => {
                        let Some(format::DirectoryBoundaryFact::Published {
                            outcome: Some(bytes),
                            ..
                        }) = &mut op.native_directory_boundary
                        else {
                            panic!("published terminal");
                        };
                        bytes.truncate(bytes.len() - 32);
                        bytes[24..32].copy_from_slice(&2u64.to_le_bytes());
                        *bytes = seal_directory(bytes.clone());
                    }
                    _ => (),
                }
                row
            })
            .collect();
        replace_checkpoint(&store, rows);
        drop(store);
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "advisory regression case {case}"
        );
    }
}

// A second fixture driver keeps a cached ordinary session in the same store
// lifetime. Every transition still uses the common production append pipeline.
fn advance_in_place(store: &mut ConnectionStore, id: ConnectionId, phase: u8) {
    if phase == 0 {
        return;
    }
    let mut registration = historical_registration(store, id);
    let expected = store.connection(id).unwrap().revision;
    historical_append(
        store,
        Change::NativeReserve {
            registration: registration.clone(),
            expected,
        },
    )
    .unwrap();
    let operation = store
        .operation(OperationId {
            connection: id,
            serial: store.connection(id).unwrap().pending.unwrap(),
        })
        .unwrap();
    let [intent, stage, outcome] = if operation.native_kind.is_some() {
        directory_phase_bytes(&operation, operation.revision)
    } else {
        regular_phase_bytes(&operation, operation.revision)
    };
    registration.intent = Some(intent);
    registration.stage = Some(stage);
    historical_append(
        store,
        Change::NativePrepared {
            registration: registration.clone(),
            expected: operation.revision,
        },
    )
    .unwrap();
    for verified in [false, true]
        .into_iter()
        .take(usize::from(phase.saturating_sub(1)))
    {
        registration.outcome = Some(outcome.clone());
        let expected = store.connection(id).unwrap().revision;
        historical_append(
            store,
            Change::NativeOutcome {
                registration: registration.clone(),
                expected,
                verified,
                reconstructed: false,
            },
        )
        .unwrap();
    }
}
fn append_control(
    store: &mut ConnectionStore,
    id: ConnectionId,
    transition: format::DirectoryChainControlTransitionV1,
) {
    let change = chain_control(store, id, transition);
    historical_append(store, change).unwrap();
}
#[test]
fn chain_controls_between_members_and_retained_stop_after_resumed_progress() {
    use format::DirectoryChainControlTransitionV1 as T;
    for reason in [format::PauseOrCancel::Pause, format::PauseOrCancel::Cancel] {
        let (_tmp, path, mut store, record) = nested_chain_first();
        let change = baseline_change(&store, record.id);
        historical_append(&mut store, change).unwrap();
        let before = store.connection(record.id).unwrap();
        assert!(before.directory_chain.as_ref().unwrap().current.is_none());
        for transition in [T::StopRequested { reason }, T::StopAcknowledged, T::Resume] {
            append_control(&mut store, record.id, transition);
        }
        assert_eq!(
            store.connection(record.id).unwrap().revision,
            before.revision
        );
        store.checkpoint().unwrap();
        drop(store);
        ConnectionStore::open_at(&path).unwrap();
    }
    for directory in [false, true] {
        for phase in [0, 1] {
            let (_tmp, path, mut store, record) = phase_history(directory, phase);
            append_control(
                &mut store,
                record.id,
                T::StopRequested {
                    reason: format::PauseOrCancel::Pause,
                },
            );
            let stop = stop_change(&store, record.id, phase, directory);
            historical_append(&mut store, stop).unwrap();
            append_control(&mut store, record.id, T::StopAcknowledged);
            append_control(&mut store, record.id, T::Resume);
            if phase == 0 {
                advance_in_place(&mut store, record.id, 3);
            } else {
                let op = store
                    .operation(OperationId {
                        connection: record.id,
                        serial: 1,
                    })
                    .unwrap();
                let mut key = format::key(5, record.id.0, 1);
                key.extend(1u32.to_be_bytes());
                let Some(Row::NativeRegistration(mut reg)) = store.row(&key).unwrap() else {
                    panic!("registration")
                };
                let mut bytes = if directory {
                    directory_phase_bytes(&op, op.revision - 1)[2].clone()
                } else {
                    regular_phase_bytes(&op, op.revision - 1)[2].clone()
                };
                // Join the original immutable intent/stage and use the new progress.
                if directory {
                    bytes.truncate(bytes.len() - 32);
                    bytes[121..153].copy_from_slice(
                        &reg.intent.as_ref().unwrap()[reg.intent.as_ref().unwrap().len() - 32..],
                    );
                    bytes[153..185].copy_from_slice(
                        &reg.stage.as_ref().unwrap()[reg.stage.as_ref().unwrap().len() - 32..],
                    );
                    bytes = seal_directory(bytes);
                } else {
                    use crate::connection::transaction::native::descriptor as d;
                    let mut outcome = d::decode(&bytes).unwrap();
                    if let d::Body::Outcome(body) = &mut outcome.body {
                        body.intent = d::decode(reg.intent.as_ref().unwrap()).unwrap().digest;
                        body.stage = Some(d::decode(reg.stage.as_ref().unwrap()).unwrap().digest);
                    }
                    bytes = d::encode(&outcome.header, &outcome.body).unwrap();
                }
                reg.outcome = Some(bytes);
                for verified in [false, true] {
                    let expected = store.connection(record.id).unwrap().revision;
                    historical_append(
                        &mut store,
                        Change::NativeOutcome {
                            registration: reg.clone(),
                            expected,
                            verified,
                            reconstructed: false,
                        },
                    )
                    .unwrap();
                }
            }
            let change = baseline_change(&store, record.id);
            historical_append(&mut store, change).unwrap();
            store.checkpoint().unwrap();
            drop(store);
            ConnectionStore::open_at(&path)
                .expect("old stop must retain its historical phase after later publish/completion");
        }
    }
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "Private fixture offsets and revisions are bounded by the explicit wire vectors and finite test phases."
)]
fn nested_existing_wire(
    store: &ConnectionStore,
    record: &ConnectionRecord,
    directory: bool,
) -> format::Operation {
    use crate::connection::transaction::native::descriptor as d;
    let wire = if directory {
        first_directory_wire(store, record)
    } else {
        first_regular_wire(store, record)
    };
    let mut op: format::Operation = serde_json::from_value(wire).unwrap();
    let mut selected = selection_vector(false, !directory);
    selected[153..161].copy_from_slice(&record.revision.to_le_bytes());
    if let Some(b) = record.baseline_revision {
        selected[170..178].copy_from_slice(&b.to_le_bytes());
    } else {
        selected[169] = 0;
        selected.drain(170..178);
    }
    let n = selected.len();
    selected[n - 48..n - 40].copy_from_slice(&2u64.to_le_bytes());
    selected[n - 40..n - 32].copy_from_slice(&0u64.to_le_bytes());
    resign(&mut selected);
    op.directory_chain_ref
        .as_mut()
        .unwrap()
        .digest
        .copy_from_slice(&selected[selected.len() - 32..]);
    op.directory_chain_anchor = Some(selected);
    op.path = format::NativePath::Unix(b"a/b".to_vec());
    if directory {
        let old = op.native_identity.as_ref().unwrap();
        let mut bytes = old[..187].to_vec();
        bytes.extend([0, 2, 3, 0, b'a', b'/', b'b']);
        for (root, parent) in [(11, 21), (12, 22)] {
            bytes.push(2);
            directory_object(&mut bytes, root);
            directory_object(&mut bytes, parent);
        }
        bytes.extend(&old[187 + 5 + 2 * (1 + 52)..old.len() - 32]);
        op.native_identity = Some(seal_directory(bytes));
    } else {
        let mut a = d::Admission::decode(op.native_identity.as_ref().unwrap()).unwrap();
        a.path = RelativePath::unix(b"a/b").unwrap();
        for (chain, ino) in a.chains.iter_mut().zip([21, 22]) {
            chain.push(d::Object {
                dev: 1,
                ino,
                kind: 0o040_000,
                generation: [u8::try_from(ino).unwrap(); 32],
            });
        }
        op.native_identity = Some(a.encode().unwrap());
    }
    op.decode()
        .expect("independent nested existing-prefix admission");
    op
}
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Private fixture offsets and revisions are bounded by the explicit wire vectors and finite test phases."
)]
fn nested_baseline_change(store: &ConnectionStore, id: ConnectionId, directory: bool) -> Change {
    let mut change = baseline_change(store, id);
    let c = store.connection(id).unwrap();
    let op = store
        .operation(OperationId {
            connection: id,
            serial: c.pending.unwrap(),
        })
        .unwrap();
    let mut entries = store
        .load_baseline(id)
        .unwrap()
        .entries()
        .cloned()
        .unwrap_or_default();
    entries.insert(RelativePath::unix(b"a").unwrap(), EntryState::Directory);
    entries.insert(
        RelativePath::unix(b"a/b").unwrap(),
        op.source.clone().unwrap().into(),
    );
    let root = storage::build_pages(
        &store.storage,
        entries.into_iter().enumerate().map(|(i, (path, state))| {
            Ok(Row::Baseline(format::BaselineEntry {
                connection: id.0,
                revision: c.revision + 1,
                ordinal: u32::try_from(i).unwrap(),
                path: format::NativePath::relative(&path),
                state: state.into(),
            }))
        }),
    )
    .unwrap();
    if let Change::BaselineCommit {
        root: r,
        count,
        existing_ancestors,
        directory_ancestors,
        ..
    } = &mut change
    {
        *r = root;
        *count = c.baseline_count + 2;
        let admission = format::digest(op.native_identity.as_ref().unwrap());
        if directory {
            *directory_ancestors = Some(format::DirectoryAncestorPatchV1 {
                version: 1,
                admission,
                prefixes: 2,
            });
        } else {
            *existing_ancestors = Some(format::ExistingAncestorPatchV1 {
                version: 1,
                admission,
                prefixes: 2,
            });
        }
    }
    change
}
#[test]
fn chain_atomic_baseline_nested_existing_prefix_receipts_and_candidates() {
    for directory in [false, true] {
        for present in [false, true] {
            let (_tmp, path, mut store, mut record) = empty_history();
            if present {
                ordinary_baseline(&mut store, &record);
                record = store.load_connection(record.id).unwrap();
            }
            let op = nested_existing_wire(&store, &record, directory);
            historical_append(&mut store, Change::Intent { operation: op }).unwrap();
            advance_in_place(&mut store, record.id, 3);
            let valid = nested_baseline_change(&store, record.id, directory);
            for case in 0..6 {
                let mut bad = valid.clone();
                if let Change::BaselineCommit {
                    existing_ancestors,
                    directory_ancestors,
                    directory_chain,
                    root,
                    count,
                    initial_review,
                    ..
                } = &mut bad
                {
                    match case {
                        0 => {
                            *existing_ancestors = None;
                            *directory_ancestors = None;
                        }
                        1 => {
                            if directory {
                                directory_ancestors.as_mut().unwrap().prefixes = 0;
                            } else {
                                existing_ancestors.as_mut().unwrap().prefixes = 0;
                            }
                        }
                        2 => {
                            if directory {
                                directory_ancestors.as_mut().unwrap().admission[0] ^= 1;
                            } else {
                                existing_ancestors.as_mut().unwrap().admission[0] ^= 1;
                            }
                        }
                        3 => directory_chain.as_mut().unwrap().member.index = 1,
                        4 => {
                            *root = None;
                            *count = 0;
                        }
                        _ => *initial_review = true,
                    }
                }
                unchanged_refusal(&mut store, bad);
            }
            historical_append(&mut store, valid).unwrap();
            let baseline = store.load_baseline(record.id).unwrap();
            assert_eq!(
                baseline.entries().unwrap().len(),
                if present { 3 } else { 2 }
            );
            assert_eq!(
                baseline.entries().unwrap()[&RelativePath::unix(b"a").unwrap()],
                EntryState::Directory
            );
            let c = store.connection(record.id).unwrap();
            assert!(c.pending.is_none() && c.directory_chain.is_none());
            store.checkpoint().unwrap();
            drop(store);
            let store = ConnectionStore::open_at(&path).unwrap();
            assert_eq!(store.connection(record.id).unwrap(), c);
        }
    }
}
#[test]
fn chain_checkpoint_closure_inter_member_baseline_and_each_retained_join() {
    for successor in [false, true] {
        for case in 0..8 {
            let (_tmp, path, mut store, record) = nested_chain_first();
            let change = baseline_change(&store, record.id);
            historical_append(&mut store, change).unwrap();
            if successor {
                let op = successor_wire(&store, &record);
                historical_append(&mut store, Change::Intent { operation: op }).unwrap();
            }
            store.checkpoint().unwrap();
            let rows = store
                .rows()
                .unwrap()
                .filter_map(|row| {
                    let mut row = row.unwrap();
                    match &mut row {
                        Row::Connection(c) => match case {
                            0 => {
                                c.baseline = None;
                                c.baseline_root = None;
                                c.baseline_count = 0;
                            }
                            1 => c.baseline = Some(c.baseline.unwrap() - 1),
                            2 => {
                                c.directory_chain
                                    .as_mut()
                                    .unwrap()
                                    .predecessor
                                    .as_mut()
                                    .unwrap()
                                    .serial = 99;
                            }
                            3 => c.directory_chain.as_mut().unwrap().anchor.serial = 99,
                            4 => {
                                c.directory_chain.as_mut().unwrap().current =
                                    Some(format::ChainOperationIdV1 {
                                        connection: c.id,
                                        serial: 99,
                                    });
                            }
                            _ => (),
                        },
                        Row::Operation(op) if op.serial == 1 => match case {
                            5 => return None,
                            6 => {
                                let selected = op.directory_chain_anchor.as_mut().unwrap();
                                selected[180] ^= 1;
                                resign(selected);
                                op.directory_chain_ref
                                    .as_mut()
                                    .unwrap()
                                    .digest
                                    .copy_from_slice(&selected[selected.len() - 32..]);
                            }
                            _ => (),
                        },
                        Row::NativeRegistration(_) if case == 7 => return None,
                        _ => (),
                    }
                    Some(row)
                })
                .collect();
            replace_checkpoint(&store, rows);
            drop(store);
            assert!(
                ConnectionStore::open_at(&path).is_err(),
                "successor={successor} case={case}"
            );
        }
    }
}
#[test]
fn chain_abandon_terminal_full_source_phase_matrix_retains_all_rows() {
    use format::DirectoryChainControlTransitionV1 as T;
    for directory in [false, true] {
        for phase in 0..=3 {
            for source in 0..3 {
                let (_tmp, path, mut store, record) = phase_history(directory, phase);
                if source > 0 {
                    append_control(
                        &mut store,
                        record.id,
                        T::StopRequested {
                            reason: format::PauseOrCancel::Cancel,
                        },
                    );
                }
                if source == 2 {
                    let stop = stop_change(&store, record.id, phase, directory);
                    historical_append(&mut store, stop).unwrap();
                    append_control(&mut store, record.id, T::StopAcknowledged);
                }
                let keys: Vec<_> = store.rows().unwrap().map(|r| r.unwrap().key()).collect();
                append_control(&mut store, record.id, T::AbandonRequested);
                let terminal = terminal_change(&store, record.id, phase, directory);
                historical_append(&mut store, terminal).unwrap();
                append_control(&mut store, record.id, T::Retire);
                assert_eq!(
                    store
                        .rows()
                        .unwrap()
                        .map(|r| r.unwrap().key())
                        .collect::<Vec<_>>(),
                    keys
                );
                store.checkpoint().unwrap();
                drop(store);
                ConnectionStore::open_at(&path).unwrap();
            }
        }
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the finite denial matrix and its before/after persistence assertions together."
)]
fn chain_live_denial_legacy_retirement_matrix_cached_and_reopened() {
    use format::DirectoryChainControlTransitionV1 as T;
    for case in 0..3 {
        for checkpoint in [false, true] {
            let (_tmp, path, mut store, mut record) = empty_history();
            let cached = store.transaction_session(record.id).unwrap();
            if case == 2 {
                ordinary_baseline(&mut store, &record);
                record = store.load_connection(record.id).unwrap();
            }
            let mut wire = first_directory_wire(&store, &record);
            if case == 0 {
                let mut selection = selection_vector(false, true);
                selection[153..161].copy_from_slice(&record.revision.to_le_bytes());
                selection[169] = 0;
                selection.drain(170..178);
                resign(&mut selection);
                wire["directory_chain_ref"]["digest"] =
                    serde_json::json!(selection[selection.len() - 32..].to_vec());
                wire["directory_chain_anchor"] = serde_json::json!(selection);
            }
            historical_append(
                &mut store,
                Change::Intent {
                    operation: serde_json::from_value(wire).unwrap(),
                },
            )
            .unwrap();
            advance_in_place(&mut store, record.id, 3);
            let change = baseline_change(&store, record.id);
            historical_append(&mut store, change).unwrap();
            if case == 0 {
                append_control(&mut store, record.id, T::AbandonRequested);
                append_control(&mut store, record.id, T::Retire);
            }
            if checkpoint {
                store.checkpoint().unwrap();
                assert!(store.overlay.is_empty());
            }
            cached
                .validate(record.id, &store.native_lifetime)
                .expect("cached session remains same lifetime and connection");
            let before = store.connection(record.id).unwrap();
            assert!(before.directory_chain.is_none() && before.directory_chain_schedule.is_none());
            let rows = format::encode(
                &store
                    .rows()
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
            )
            .unwrap();
            let intent = unmarked_intent(&store, record.id);
            assert!(matches!(
                store.begin_native_operation(intent, &cached, None, None, None, None),
                Err(StoreError::RecoveryRequired)
            ));
            assert_eq!(store.connection(record.id).unwrap(), before);
            assert_eq!(
                format::encode(
                    &store
                        .rows()
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap()
                )
                .unwrap(),
                rows
            );
            drop(store);
            let mut store = ConnectionStore::open_at(&path).unwrap();
            assert!(matches!(
                store.transaction_session(record.id),
                Err(StoreError::RecoveryRequired)
            ));
            let intent = unmarked_intent(&store, record.id);
            let candidate =
                format::Operation::new(intent.clone(), before.serial + 1, &before).unwrap();
            assert!(candidate.directory_chain_ref.is_none());
            assert!(matches!(
                store.begin_intent(intent),
                Err(StoreError::RecoveryRequired)
            ));
            assert!(matches!(
                store.append(Change::Intent {
                    operation: candidate
                }),
                Err(StoreError::RecoveryRequired)
            ));
            assert_eq!(store.connection(record.id).unwrap(), before);
            assert_eq!(
                format::encode(
                    &store
                        .rows()
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap()
                )
                .unwrap(),
                rows
            );
            // A different ordinary connection still admits normal work in this store.
            let other = store
                .create_connection(
                    ConnectionId([8; 16]),
                    record.source.clone(),
                    record.destination.clone(),
                    ConnectionPolicy {
                        deletion_retention_seconds: 2_592_000,
                    },
                )
                .unwrap()
                .value;
            store.transaction_session(other.id).unwrap();
            ordinary_baseline(&mut store, &other);
            let c = store.connection(other.id).unwrap();
            assert!(c.baseline.is_some());
            let ack = store.request_disconnect(other.id, c.revision).unwrap();
            store.finish_disconnect(&ack).unwrap();
        }
    }
}
#[test]
fn chain_persistence_faults_checkpoint_baseline_reread_refuses_missing_page() {
    for directory in [false, true] {
        let (_tmp, path, mut store, record) = phase_history(directory, 3);
        let change = baseline_change(&store, record.id);
        historical_append(&mut store, change).unwrap();
        store.checkpoint().unwrap();
        let before = store.connection(record.id).unwrap();
        let current = std::fs::read(path.join("CURRENT")).unwrap();
        let page = path
            .join("pages")
            .join(format::hex(&before.baseline_root.unwrap()));
        let held = page.with_extension("held");
        std::fs::rename(&page, &held).unwrap();
        assert!(
            store.checkpoint().is_err(),
            "checkpoint must observe missing committed baseline page"
        );
        assert_eq!(std::fs::read(path.join("CURRENT")).unwrap(), current);
        std::fs::rename(held, page).unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(store.connection(record.id).unwrap(), before);
    }
}
#[test]
fn chain_actual_capacity_overlay_key_bytes_and_real_log_framing_limits() {
    let (_tmp, _path, store, record) = empty_history();
    let base = store.connection(record.id).unwrap();
    let rows: Vec<_> = (0u32..1024)
        .map(|i| {
            let mut c = base.clone();
            c.id[..4].copy_from_slice(&i.to_le_bytes());
            Row::Connection(c)
        })
        .collect();
    assert!(
        store
            .prepare_rows(rows[..1023].to_vec())
            .unwrap()
            .fits_overlay()
    );
    assert!(
        !store.prepare_rows(rows.clone()).unwrap().fits_overlay(),
        "existing key plus1024 new keys exceeds1024"
    );
    let mut huge = rows[..100].to_vec();
    for row in &mut huge {
        let Row::Connection(c) = row else {
            panic!("expected a Connection fixture row")
        };
        let mut locator = vec![b'x'; 30_000];
        locator[0] = b'/';
        c.source.locator = format::NativePath::Unix(locator);
    }
    let encoded: usize = huge
        .iter()
        .map(|row| format::encode(row).unwrap().len())
        .sum();
    assert!(encoded > 8 * 1024 * 1024);
    assert!(!store.prepare_rows(huge).unwrap().fits_overlay());
    let (_tmp, _path, mut store, record) = first_history(false);
    let before = store.connection(record.id).unwrap();
    let control = chain_control(
        &store,
        record.id,
        format::DirectoryChainControlTransitionV1::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    let normal = stop_change(&store, record.id, 0, false);
    for (change, limit) in [
        (control, 16u64 * 1024 * 1024),
        (normal, 15 * 1024 * 1024),
        (
            Change::Fence {
                connection: record.id.0,
                expected: before.revision,
                generation: before.fence + 1,
            },
            16 * 1024 * 1024,
        ),
    ] {
        let event = Event {
            schema: 1,
            predecessor: store.digest,
            change,
        };
        let encoded = format::encode(&event).unwrap().len();
        let framed = encoded as u64 + 44;
        if matches!(event.change, Change::Fence { .. }) {
            assert!(framed <= 512);
        }
        // Sparse lengths are capacity probes only; they are never replayed as history.
        let original = store.log.metadata().unwrap().len();
        store.log.set_len(limit - framed).unwrap();
        store.check_event_capacity(&event, encoded).unwrap();
        store.log.set_len(limit - framed + 1).unwrap();
        assert!(matches!(
            store.check_event_capacity(&event, encoded),
            Err(StoreError::LimitExceeded)
        ));
        store.log.set_len(original).unwrap();
    }
    store.log_identity = storage::regular(&store.log).unwrap();
    assert_eq!(store.connection(record.id).unwrap(), before);
}

#[test]
fn chain_actual_capacity_maximum_native_paths_and_real_json_history_phases() {
    use crate::connection::transaction::native::descriptor as d;
    for windows in [false, true] {
        let (_tmp, path, mut store, record) = empty_history();
        let mut op: format::Operation =
            serde_json::from_value(first_regular_wire(&store, &record)).unwrap();
        let lengths = [vec![255; 15], vec![160], vec![1; 48]].concat();
        let parts: Vec<_> = lengths.into_iter().map(|n| "z".repeat(n)).collect();
        let text = parts.join(if windows { "\\" } else { "/" });
        assert_eq!(text.len(), 4096);
        assert_eq!(parts.len(), 64);
        let native = if windows {
            text.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        } else {
            text.as_bytes().to_vec()
        };
        let mut selected = selection_vector(windows, true);
        if windows {
            selected[219] = 0;
        }
        selected.splice(
            210..if windows { 219 } else { 216 },
            [
                vec![if windows { 2 } else { 1 }],
                4096u16.to_le_bytes().to_vec(),
                native,
            ]
            .concat(),
        );
        selected[153..161].copy_from_slice(&record.revision.to_le_bytes());
        selected[169] = 0;
        selected.drain(170..178);
        let offset = selected.len() - 56;
        selected[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
        selected[offset + 8..offset + 16].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
        selected[offset + 16..offset + 24].copy_from_slice(&0u64.to_le_bytes());
        resign(&mut selected);
        let selection = DirectoryChainSelectionV1::decode(&selected).unwrap();
        assert_eq!(selection.member_count().unwrap(), 1);
        let mut admission = d::Admission::decode(op.native_identity.as_ref().unwrap()).unwrap();
        admission.path = selection.path.clone();
        for (side, chain) in admission.chains.iter_mut().enumerate() {
            for i in 1..64 {
                chain.push(d::Object {
                    dev: 1,
                    ino: 100 + side as u64 * 100 + i,
                    kind: 0o040_000,
                    generation: [9; 32],
                });
            }
        }
        op.path = format::NativePath::relative(&selection.path);
        op.native_identity = Some(admission.encode().unwrap());
        op.directory_chain_ref.as_mut().unwrap().digest = selection.selection_digest;
        op.directory_chain_anchor = Some(selected);
        historical_append(&mut store, Change::Intent { operation: op })
            .expect("maximum legal native paths fit actual row and future phase ceilings");
        advance_in_place(&mut store, record.id, 3);
        append_control(
            &mut store,
            record.id,
            format::DirectoryChainControlTransitionV1::StopRequested {
                reason: format::PauseOrCancel::Pause,
            },
        );
        let stop = stop_change(&store, record.id, 3, false);
        historical_append(&mut store, stop).unwrap();
        let sizes: Vec<_> = store
            .rows()
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                let object = format::encode(&row).unwrap().len();
                let page = format::encode(&format::PageNode {
                    schema: 1,
                    node: format::Node::Leaf(vec![row]),
                })
                .unwrap()
                .len();
                assert!(object <= 131_072 && page <= 131_072);
                (object, page)
            })
            .collect();
        eprintln!("max native4096/depth64 windows={windows} actual row/singleton sizes={sizes:?}");
        store.checkpoint().unwrap();
        drop(store);
        ConnectionStore::open_at(&path).unwrap();
    }
}

#[test]
fn chain_checkpoint_closure_refuses_future_descriptor_progress() {
    for directory in [false, true] {
        let (_tmp, path, mut store, record) = phase_history(directory, 2);
        let revision = store.connection(record.id).unwrap().revision;
        store.checkpoint().unwrap();
        let rows = store
            .rows()
            .unwrap()
            .map(|row| {
                let mut row = row.unwrap();
                if let Row::NativeRegistration(reg) = &mut row {
                    if directory {
                        let bytes = reg.outcome.as_mut().unwrap();
                        bytes.truncate(bytes.len() - 32);
                        bytes[105..113].copy_from_slice(&(revision + 10).to_le_bytes());
                        *bytes = seal_directory(bytes.clone());
                    } else {
                        use crate::connection::transaction::native::descriptor as d;
                        let mut outcome = d::decode(reg.outcome.as_ref().unwrap()).unwrap();
                        outcome.header.progress = revision + 10;
                        reg.outcome = Some(d::encode(&outcome.header, &outcome.body).unwrap());
                    }
                }
                row
            })
            .collect();
        replace_checkpoint(&store, rows);
        drop(store);
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "future outcome cannot precede its recorded operation directory={directory}"
        );
    }
}
#[test]
fn chain_checkpoint_closure_refuses_missing_committed_existing_prefix() {
    for directory in [false, true] {
        let (_tmp, path, mut store, record) = empty_history();
        let op = nested_existing_wire(&store, &record, directory);
        historical_append(&mut store, Change::Intent { operation: op }).unwrap();
        advance_in_place(&mut store, record.id, 3);
        let change = nested_baseline_change(&store, record.id, directory);
        historical_append(&mut store, change).unwrap();
        store.checkpoint().unwrap();
        let c = store.connection(record.id).unwrap();
        let state = store.load_baseline(record.id).unwrap().entries().unwrap()
            [&RelativePath::unix(b"a/b").unwrap()]
            .clone();
        let root = storage::build_pages(
            &store.storage,
            std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
                connection: record.id.0,
                revision: c.baseline.unwrap(),
                ordinal: 0,
                path: format::NativePath::Unix(b"a/b".to_vec()),
                state: state.into(),
            }))),
        )
        .unwrap();
        let rows = store
            .rows()
            .unwrap()
            .map(|row| {
                let mut row = row.unwrap();
                if let Row::Connection(c) = &mut row {
                    c.baseline_root = root;
                    c.baseline_count = 1;
                }
                row
            })
            .collect();
        replace_checkpoint(&store, rows);
        drop(store);
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "committed existing prefix must survive baseline closure directory={directory}"
        );
    }
}

#[test]
fn chain_first_next_history_wrong_proposal_fields_and_successor_timing() {
    for case in 0..8 {
        let (_tmp, _path, mut store, record) = empty_history();
        let mut op: format::Operation =
            serde_json::from_value(first_regular_wire(&store, &record)).unwrap();
        match case {
            0 => op.baseline = Some(1),
            1 => op.expected += 1,
            2 => op.directory_chain_ref.as_mut().unwrap().digest[0] ^= 1,
            3 => {
                op.initial_review = Some(format::InitialReviewFact {
                    preview: [8; 32],
                    selection: [9; 32],
                    revision: op.expected,
                });
            }
            4 => op.decision = 0,
            5 => op.source = Some(format::State::Directory),
            6 => op.native_kind = Some(format::NativeOperationKind::DirectoryV1),
            _ => {
                op.action = Some(format::SelectedAction::Restore {
                    version: 1,
                    target: 1,
                    replace: false,
                    revision: op.expected,
                    fence: 0,
                    selected: format::SelectedVersion::Recovery {
                        operation: 1,
                        slot: 0,
                        content: op.source.clone().unwrap(),
                    },
                    conflict: None,
                });
            }
        }
        unchanged_refusal(&mut store, Change::Intent { operation: op });
    }
    let (_tmp, _path, mut store, record) = nested_chain_first();
    let mut early = successor_wire(&store, &record);
    let reference = early.directory_chain_ref.as_mut().unwrap();
    reference.index = 1;
    reference.predecessor = Some(reference.anchor);
    unchanged_refusal(&mut store, Change::Intent { operation: early });
    let change = baseline_change(&store, record.id);
    historical_append(&mut store, change).unwrap();
    let valid = successor_wire(&store, &record);
    for case in 0..8 {
        let mut bad = valid.clone();
        match case {
            0 => bad.baseline = None,
            1 => bad.expected += 1,
            2 => bad.serial += 1,
            3 => bad.directory_chain_ref.as_mut().unwrap().digest[0] ^= 1,
            4 => {
                bad.directory_chain_ref
                    .as_mut()
                    .unwrap()
                    .predecessor
                    .as_mut()
                    .unwrap()
                    .serial = 99;
            }
            5 => bad.directory_chain_ref.as_mut().unwrap().index = 0,
            6 => bad.path = format::NativePath::Unix(b"other".to_vec()),
            _ => bad.source_binding.volume[0] ^= 1,
        }
        unchanged_refusal(&mut store, Change::Intent { operation: bad });
    }
    historical_append(&mut store, Change::Intent { operation: valid }).unwrap();
}
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the finite denial matrix and its before/after persistence assertions together."
)]
fn chain_progress_closure_remaining_excluded_variants_and_reservation_sequence() {
    use crate::connection::transaction::native::descriptor as d;
    let (_tmp, _path, mut store, record) = first_history(false);
    let c = store.connection(record.id).unwrap();
    let op = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let a = d::Admission::decode(op.native_identity.as_ref().unwrap()).unwrap();
    let snapshot = d::ConflictSnapshot {
        connection: c.id,
        operation: 1,
        admission: op.expected + 1,
        revision: c.revision,
        fence: c.fence,
        baseline: c.baseline,
        endpoints: a.endpoints,
        path: a.path,
        chains: a.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: a.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: a.states[1].clone(),
                displaced: None,
            },
        ],
    }
    .encode()
    .unwrap();
    let entry = format::Recovery {
        captured: None,
        connection: c.id,
        serial: 1,
        slot: 0,
        side: 1,
        path: op.path,
        reason: 0,
        state: 0,
        content: op.source,
        object: None,
        retained: 0,
    };
    let fact = format::NativeRecoveryFact {
        captured: None,
        connection: c.id,
        serial: 1,
        slot: 0,
        attempt: 1,
        fact: vec![],
        displaced: vec![],
    };
    let source = format::BoundaryEvidence {
        attempt: 1,
        displaced: None,
        outcome: None,
    };
    let changes = vec![
        Change::Reserve {
            entry: entry.clone(),
            expected: c.revision,
            fence: c.fence,
        },
        Change::Bytes {
            entry: entry.clone(),
            expected: c.revision,
            fence: c.fence,
        },
        Change::SelectedCaptureDrain {
            entry: entry.clone(),
            expected: c.revision,
            operation_expected: c.revision,
            fence: c.fence,
            fact: None,
        },
        Change::NativeCaptureDrain {
            entry: entry.clone(),
            expected: c.revision,
            operation_expected: c.revision,
            fence: c.fence,
            fact: None,
        },
        Change::NativeDrain {
            entry: entry.clone(),
            expected: c.revision,
            operation_expected: c.revision,
            fence: c.fence,
            source: source.clone(),
            fact: None,
        },
        Change::SelectedDrain {
            entry,
            expected: c.revision,
            operation_expected: c.revision,
            fence: c.fence,
            source: source.clone(),
            fact: None,
        },
        Change::SelectedBoundary {
            connection: c.id,
            serial: 1,
            expected: c.revision,
            operation_expected: c.revision,
            fence: c.fence,
            kind: 0,
            evidence: source,
        },
        Change::NativeBound {
            fact,
            expected: c.revision,
            fence: c.fence,
        },
        Change::NativeCapture {
            snapshot: snapshot.clone(),
            displaced: vec![],
            expected: c.revision,
            kind: 0,
        },
        Change::NativeSelectedCollision {
            snapshot,
            expected: c.revision,
        },
    ];
    for change in changes {
        assert!(
            matches!(
                store.chain_event_guard(&change),
                Err(StoreError::RecoveryRequired)
            ),
            "closed event table rejected before payload-specific validation: {change:?}"
        );
        unchanged_refusal(&mut store, change);
    }
    let mut paid = read_reserve(&store, record.id, false);
    if let Change::ReadReserve { next, .. } = &mut paid {
        next.sequence = u64::MAX;
    }
    unchanged_refusal(&mut store, paid);
}

fn review_fix1_corrupt_checkpoint(
    mut store: ConnectionStore,
    path: &Path,
    rows: Vec<Row>,
    publish: bool,
) {
    if publish {
        let current = std::fs::read(path.join("CURRENT")).unwrap();
        let prepared = store.prepare_rows(rows).unwrap();
        store.commit_rows(prepared);
        assert!(
            store.checkpoint().is_err(),
            "checkpoint publication must close retained joins"
        );
        assert_eq!(std::fs::read(path.join("CURRENT")).unwrap(), current);
    } else {
        replace_checkpoint(&store, rows);
        drop(store);
        assert!(
            ConnectionStore::open_at(path).is_err(),
            "empty-log reopen must close retained joins"
        );
    }
}
#[test]
fn chain_review_fix1_stopped_requires_boundary_and_exact_schedule_relationships() {
    use format::DirectoryChainControlTransitionV1 as T;
    for directory in [false, true] {
        for publish in [false, true] {
            for case in 0..5 {
                let (_tmp, path, mut store, record) = first_history(directory);
                append_control(
                    &mut store,
                    record.id,
                    T::StopRequested {
                        reason: format::PauseOrCancel::Pause,
                    },
                );
                let stop = stop_change(&store, record.id, 0, directory);
                historical_append(&mut store, stop).unwrap();
                append_control(&mut store, record.id, T::StopAcknowledged);
                if case == 3 {
                    let c = store.connection(record.id).unwrap();
                    store.request_disconnect(record.id, c.revision).unwrap();
                }
                if case == 4 {
                    append_control(&mut store, record.id, T::Resume);
                }
                store.checkpoint().unwrap();
                let schedule = store
                    .connection(record.id)
                    .unwrap()
                    .directory_chain_schedule
                    .unwrap()
                    .revision;
                let rows = store
                    .rows()
                    .unwrap()
                    .map(|row| {
                        let mut row = row.unwrap();
                        match &mut row {
                            Row::Operation(op) if case == 0 => {
                                op.directory_chain_stop_boundary = None
                            }
                            Row::Operation(op) if case == 1 || case == 4 => {
                                op.directory_chain_stop_boundary
                                    .as_mut()
                                    .unwrap()
                                    .schedule_revision = schedule + 1
                            }
                            Row::Connection(c) if case == 2 || case == 3 => {
                                c.directory_chain_schedule
                                    .as_mut()
                                    .unwrap()
                                    .control_sequence = 0
                            }
                            _ => (),
                        }
                        row
                    })
                    .collect();
                review_fix1_corrupt_checkpoint(store, &path, rows, publish);
            }
        }
    }
}
#[test]
fn chain_review_fix1_terminal_and_between_member_progress_is_exact() {
    use format::DirectoryChainControlTransitionV1 as T;
    for publish in [false, true] {
        for case in 0..5 {
            let (_tmp, path, mut store, record) = if case < 2 {
                phase_history(true, if case == 0 { 3 } else { 0 })
            } else {
                nested_chain_first()
            };
            if case != 1 {
                let change = baseline_change(&store, record.id);
                historical_append(&mut store, change).unwrap();
            }
            if case == 1 || case == 2 {
                append_control(&mut store, record.id, T::AbandonRequested);
                if case == 1 {
                    let terminal = terminal_change(&store, record.id, 0, true);
                    historical_append(&mut store, terminal).unwrap();
                }
                append_control(&mut store, record.id, T::Retire);
            }
            if case == 4 {
                let c = store.connection(record.id).unwrap();
                store.request_disconnect(record.id, c.revision).unwrap();
            }
            store.checkpoint().unwrap();
            let rows = store
                .rows()
                .unwrap()
                .map(|row| {
                    let mut row = row.unwrap();
                    match &mut row {
                        Row::Operation(op) if case < 3 => {
                            op.directory_chain_retired
                                .as_mut()
                                .unwrap()
                                .cursor
                                .progress_revision = 1
                        }
                        Row::Connection(c) if case == 3 => {
                            c.revision += 1;
                            c.directory_chain.as_mut().unwrap().progress_revision = c.revision;
                        }
                        Row::Connection(c) if case == 4 => {
                            c.directory_chain.as_mut().unwrap().progress_revision = 1
                        }
                        _ => (),
                    }
                    row
                })
                .collect();
            review_fix1_corrupt_checkpoint(store, &path, rows, publish);
        }
    }
}
#[test]
fn chain_review_fix1_stopped_paid_inspection_and_resumed_old_boundary_remain_valid() {
    use format::DirectoryChainControlTransitionV1 as T;
    for directory in [false, true] {
        let (_tmp, path, mut store, record) = phase_history(directory, 1);
        append_control(
            &mut store,
            record.id,
            T::StopRequested {
                reason: format::PauseOrCancel::Cancel,
            },
        );
        let stop = stop_change(&store, record.id, 1, directory);
        historical_append(&mut store, stop).unwrap();
        append_control(&mut store, record.id, T::StopAcknowledged);
        let before = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap();
        let paid = read_reserve(&store, record.id, true);
        historical_append(&mut store, paid).unwrap();
        let after = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap();
        assert!(
            after.revision
                > before
                    .directory_chain_stop_boundary
                    .as_ref()
                    .unwrap()
                    .operation_revision
        );
        assert_eq!(
            after.directory_chain_stop_boundary,
            before.directory_chain_stop_boundary
        );
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        append_control(&mut store, record.id, T::Resume);
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        append_control(
            &mut store,
            record.id,
            T::StopRequested {
                reason: format::PauseOrCancel::Pause,
            },
        );
        let old = chain_control(&store, record.id, T::StopAcknowledged);
        unchanged_refusal(&mut store, old);
        let stop = stop_change(&store, record.id, 1, directory);
        historical_append(&mut store, stop).unwrap();
        append_control(&mut store, record.id, T::StopAcknowledged);
        store.checkpoint().unwrap();
        drop(store);
        ConnectionStore::open_at(&path).unwrap();
    }
}
#[test]
fn chain_review_fix1_terminal_and_between_member_fence_divergence_remains_valid() {
    use format::DirectoryChainControlTransitionV1 as T;
    for case in 0..4 {
        let (_tmp, path, mut store, record) = if case < 2 {
            phase_history(true, if case == 0 { 3 } else { 0 })
        } else {
            nested_chain_first()
        };
        if case != 1 {
            let change = baseline_change(&store, record.id);
            historical_append(&mut store, change).unwrap();
        }
        if case == 1 || case == 2 {
            append_control(&mut store, record.id, T::AbandonRequested);
            if case == 1 {
                let terminal = terminal_change(&store, record.id, 0, true);
                historical_append(&mut store, terminal).unwrap();
            }
            append_control(&mut store, record.id, T::Retire);
        }
        let before = store.connection(record.id).unwrap();
        let ack = store
            .request_disconnect(record.id, before.revision)
            .unwrap();
        store.finish_disconnect(&ack).unwrap();
        let current = store.connection(record.id).unwrap();
        assert!(current.revision > before.revision);
        let anchor = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap();
        let cursor = anchor
            .directory_chain_retired
            .as_ref()
            .map(|r| &r.cursor)
            .or(current.directory_chain.as_ref())
            .unwrap();
        assert_eq!(cursor.progress_revision, before.revision);
        store.checkpoint().unwrap();
        drop(store);
        ConnectionStore::open_at(&path).unwrap();
    }
}

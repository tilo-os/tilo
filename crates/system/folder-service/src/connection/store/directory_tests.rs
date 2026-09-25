//! Historical wire and reducer fixtures; these never create native authority.
use super::*;
use crate::connection::transaction::native::descriptor as d;

fn object(bytes: &mut Vec<u8>, kind: u32, identity: u8) {
    bytes.extend(1_u64.to_le_bytes());
    bytes.extend(u64::from(identity).to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend([identity; 32]);
}

fn seal(mut bytes: Vec<u8>) -> Vec<u8> {
    let length = u32::try_from(bytes.len() + 32).unwrap();
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes.extend(sha2::Sha256::digest(&bytes));
    bytes
}

fn header(magic: [u8; 4]) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend(0_u32.to_le_bytes());
    bytes.extend([31; 16]); // Connection.
    bytes.extend(7_u64.to_le_bytes()); // Original operation.
    bytes.extend(1_u64.to_le_bytes()); // Attempt.
    bytes.extend([32; 16]); // Nonce.
    bytes.extend([33; 16]); // Epoch.
    bytes.extend([34; 16]); // Session.
    bytes.extend(11_u64.to_le_bytes()); // Admission.
    bytes.push(1); // Existing baseline is present.
    bytes.extend(7_u64.to_le_bytes());
    bytes.extend(12_u64.to_le_bytes()); // Reserved namespace progress.
    bytes.extend(3_u64.to_le_bytes()); // Fence.
    assert_eq!(bytes.len(), 121);
    bytes
}

fn endpoints_and_direction(bytes: &mut Vec<u8>) {
    bytes.extend([41; 64]);
    bytes.extend([42; 64]);
    bytes.extend([0, 1]); // Source -> destination, ordinary Propagate.
}

fn directory_path_chains_and_states(bytes: &mut Vec<u8>, nested: bool) {
    let path = if nested {
        b"a/b/dir".as_slice()
    } else {
        b"dir"
    };
    let depth = if nested { 3 } else { 1 };
    bytes.extend([0, depth]);
    bytes.extend(u16::try_from(path.len()).unwrap().to_le_bytes());
    bytes.extend(path);
    for (root, parents) in [(51, [61, 62]), (52, [71, 72])] {
        bytes.push(depth);
        object(bytes, 0o040_000, root);
        if nested {
            for parent in parents {
                object(bytes, 0o040_000, parent);
            }
        }
    }
    bytes.push(1); // Directory admission state, not regular State.
    object(bytes, 0o040_000, 53);
    bytes.extend(4096_u64.to_le_bytes()); // Directory metadata length.
    for value in [1_i64, 0, 1, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(0o040_700_u32.to_le_bytes());
    bytes.extend(501_u32.to_le_bytes());
    bytes.extend(20_u32.to_le_bytes());
    bytes.extend(2_u64.to_le_bytes());
    bytes.push(0); // Target is absent.
}

fn directory_facts(nested: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    // Construct QNA2 too, so QCI1's admission digest has an exact valid
    // preimage with the same context, path, chains and endpoint states.
    let mut admission = b"QNA2".to_vec();
    admission.extend(0_u32.to_le_bytes());
    admission.extend([31; 16]);
    admission.extend(7_u64.to_le_bytes());
    admission.extend(10_u64.to_le_bytes()); // Revision before admission 11.
    admission.extend(3_u64.to_le_bytes());
    admission.push(1);
    admission.extend(7_u64.to_le_bytes());
    endpoints_and_direction(&mut admission);
    directory_path_chains_and_states(&mut admission, nested);
    let admission = seal(admission);
    assert_eq!(admission.len(), if nested { 658 } else { 446 });

    let mut intent = header(*b"QCI1");
    endpoints_and_direction(&mut intent);
    object(&mut intent, 0o040_000, 54); // Retained namespace.
    intent.extend(sha2::Sha256::digest(&admission));
    intent.extend([0; 34]); // Canonically absent chain authorization.
    directory_path_chains_and_states(&mut intent, nested);
    let intent = seal(intent);
    assert_eq!(intent.len(), if nested { 840 } else { 628 });

    let mut stage = header(*b"QCS1");
    stage.extend(&intent[intent.len() - 32..]); // QCI1 payload checksum.
    object(&mut stage, 0o040_000, 55); // Created Directory.
    object(&mut stage, 0o040_000, 53); // Exact QCI1 source.
    object(&mut stage, 0o040_000, if nested { 72 } else { 52 }); // Target-parent chain tail.
    stage.push(1); // Historical provider version.
    stage.extend(sha2::Sha256::digest(
        b"qos-connected-directory-inotify-s-e-v1",
    ));
    stage.push(3); // Child and namespace barriers.
    let stage = seal(stage);
    assert_eq!(stage.len(), 375);
    (admission, intent, stage)
}

fn directory_descriptors() -> (Vec<u8>, Vec<u8>) {
    let (_, intent, stage) = directory_facts(false);
    (intent, stage)
}

fn rebind_intent_digest(stage: &[u8], intent: &[u8]) -> Vec<u8> {
    let mut payload = stage[..stage.len() - 32].to_vec();
    payload[121..153].copy_from_slice(&intent[intent.len() - 32..]);
    seal(payload)
}

fn registration(intent: &[u8], stage: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "connection": ([31_u8; 16].to_vec()), "serial": 7, "attempt": 1,
        "nonce": ([32_u8; 16].to_vec()), "side": 1, "admission": 11,
        "baseline": 7, "fence": 3, "intent": intent, "stage": stage
    })
}

fn accepted(wire: serde_json::Value) -> bool {
    serde_json::from_value::<format::NativeRegistration>(wire)
        .is_ok_and(|value| value.validate().is_ok())
}

fn regular_descriptors() -> (Vec<u8>, Vec<u8>) {
    let object = |kind, identity: u8| d::Object {
        dev: 1,
        ino: u64::from(identity),
        kind,
        generation: [identity; 32],
    };
    let header = d::Header {
        connection: [31; 16],
        operation: 7,
        attempt: 1,
        nonce: [32; 16],
        epoch: [33; 16],
        session: [34; 16],
        admission: 11,
        baseline: Some(7),
        progress: 12,
        fence: 3,
    };
    let source = object(0o100_000, 53);
    let stamp = d::Stamp {
        length: 3,
        modified: (1, 0),
        changed: (1, 0),
        mode: 0o100_600,
        uid: 501,
        gid: 20,
        links: 1,
    };
    let content = d::Content {
        digest: [61; 32],
        length: 3,
        executable: false,
    };
    let intent = d::encode(
        &header,
        &d::Body::Intent(d::Intent {
            endpoints: [[41; 64], [42; 64]],
            direction: 0,
            decision: 1,
            namespace: object(0o040_000, 54),
            path: RelativePath::unix(b"dir").unwrap(),
            chains: [vec![object(0o040_000, 51)], vec![object(0o040_000, 52)]],
            expected: [
                d::State::Regular {
                    object: source.clone(),
                    stamp: stamp.clone(),
                    content: content.clone(),
                },
                d::State::Absent,
            ],
            selection: d::Selection::None,
        }),
    )
    .unwrap();
    let stage = d::encode(
        &header,
        &d::Body::Stage(d::Stage {
            intent: d::decode(&intent).unwrap().digest,
            object: object(0o100_000, 55),
            content,
            source: d::Source::Endpoint {
                side: 0,
                parent: object(0o040_000, 51),
                object: source,
                stamp,
            },
        }),
    )
    .unwrap();
    (intent, stage)
}

#[test]
fn directory_registration_accepts_canonical_directory_v1_prepared_bytes() {
    let (intent, stage) = directory_descriptors();
    let mut wire = registration(&intent, &stage);
    wire["native_kind"] = "DirectoryV1".into();
    let parsed = serde_json::from_value::<format::NativeRegistration>(wire);
    assert!(
        parsed.is_ok(),
        "canonical DirectoryV1 registration: {parsed:?}"
    );
    let parsed = parsed.unwrap();
    assert!(parsed.validate().is_ok());
    assert_eq!(parsed.intent.as_deref(), Some(intent.as_slice()));
    assert_eq!(parsed.stage.as_deref(), Some(stage.as_slice()));
}

#[test]
fn directory_registration_rejects_absent_unknown_null_and_mixed_kinds() {
    let (intent, stage) = directory_descriptors();
    let (regular_intent, regular_stage) = regular_descriptors();
    // This control proves the mixed-body inputs are real legacy descriptors.
    assert!(accepted(registration(&regular_intent, &regular_stage)));
    assert!(!accepted(registration(&intent, &stage)));
    for kind in [serde_json::Value::Null, "DirectoryV2".into(), 1.into()] {
        let mut wire = registration(&intent, &stage);
        wire["native_kind"] = kind;
        assert!(!accepted(wire));
    }
    // Repair hash joins in mixed pairs, leaving type/body as the refusal.
    let regular_stage_for_directory = rebind_intent_digest(&regular_stage, &intent);
    let directory_stage_for_regular = rebind_intent_digest(&stage, &regular_intent);
    for (intent_body, stage_body) in [
        (&regular_intent, &regular_stage),
        (&intent, &regular_stage_for_directory),
        (&regular_intent, &directory_stage_for_regular),
        (&stage, &stage),
        (&intent, &intent),
    ] {
        let mut wire = registration(intent_body, stage_body);
        wire["native_kind"] = "DirectoryV1".into();
        assert!(!accepted(wire));
    }
    // Absence-only compatibility fields cannot use null to select legacy.
    let mut wire = registration(&regular_intent, &regular_stage);
    wire["native_kind"] = serde_json::Value::Null;
    assert!(!accepted(wire));
}
#[test]
fn directory_operation_accepts_only_exact_typed_admission() {
    let (admission, _, _) = directory_facts(false);
    let wire = serde_json::json!({
        "native_kind": "DirectoryV1", "native_identity": admission,
        "native_budget": format::NativeBudget::initial(),
        "connection": ([31_u8; 16].to_vec()), "serial": 7,
        "revision": 11, "expected": 10, "baseline": 7,
        "path": {"encoding":"Unix","units":b"dir"},
        "source": {"kind":"Directory"}, "destination": null,
        "source_binding": {"volume": ([41_u8;32].to_vec()), "root": ([41_u8;32].to_vec())},
        "destination_binding": {"volume": ([42_u8;32].to_vec()), "root": ([42_u8;32].to_vec())},
        "decision": 1, "stage": 0, "recovery": []
    });
    let parsed = serde_json::from_value::<format::Operation>(wire.clone());
    assert!(
        parsed.is_ok(),
        "typed canonical Directory operation: {parsed:?}"
    );
    assert!(parsed.unwrap().decode().is_ok());
    for (key, value) in [
        ("native_kind", serde_json::Value::Null),
        ("native_identity", serde_json::Value::Null),
        ("decision", 5.into()),
        ("expected", 9.into()),
        (
            "source",
            serde_json::json!({"kind":"File", "sha256": ([0_u8;32].to_vec()), "bytes":0, "executable":false}),
        ),
    ] {
        let mut invalid = wire.clone();
        invalid[key] = value;
        assert!(
            !serde_json::from_value::<format::Operation>(invalid)
                .is_ok_and(|value| value.decode().is_ok())
        );
    }
}

#[test]
fn directory_registration_accepts_joined_outcome_and_refuses_changed_published_identity() {
    let (intent, stage) = directory_descriptors();
    let mut outcome = header(*b"QCO1");
    outcome[105..113].copy_from_slice(&14_u64.to_le_bytes());
    outcome.extend(&intent[intent.len() - 32..]);
    outcome.extend(&stage[stage.len() - 32..]);
    outcome.push(1);
    object(&mut outcome, 0o040_000, 53);
    outcome.push(1);
    object(&mut outcome, 0o040_000, 55);
    outcome.extend([3, 3]);
    outcome.extend(1_u64.to_le_bytes());
    let outcome = seal(outcome);
    assert_eq!(outcome.len(), 333);
    let mut wire = registration(&intent, &stage);
    wire["native_kind"] = "DirectoryV1".into();
    wire["outcome"] = serde_json::json!(outcome);
    assert!(
        accepted(wire.clone()),
        "joined Directory outcome must validate"
    );
    let mut invalid = outcome[..outcome.len() - 32].to_vec();
    invalid[239] ^= 1; // Published object identity, with valid outer checksum.
    wire["outcome"] = serde_json::json!(seal(invalid));
    assert!(!accepted(wire));
}

#[test]
fn directory_history_reduces_prepared_outcome_and_exact_baseline() {
    for nested in [false, true] {
        directory_history(nested, 0);
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "Exercise each durable transition and rejected whole candidate in the same real store history."
)]
fn directory_history(nested: bool, boundary: u8) {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 950);
    commit_test_baseline(&mut store, record.id);
    let record = store.load_connection(record.id).unwrap();
    let connection = store.connection(record.id).unwrap();
    let mut proposal = intent(&record);
    proposal.expected_baseline = record.baseline_revision;
    let leaf = if nested {
        b"a/b/dir".as_slice()
    } else {
        b"dir"
    };
    proposal.path = RelativePath::unix(leaf).unwrap();
    proposal.source_before = Some(EntryState::Directory);
    let operation = format::Operation::new(proposal, connection.serial + 1, &connection).unwrap();
    let (mut admission, _, _) = directory_facts(nested);
    admission.truncate(admission.len() - 32);
    admission[8..24].copy_from_slice(&record.id.0);
    admission[24..32].copy_from_slice(&operation.serial.to_le_bytes());
    admission[32..40].copy_from_slice(&record.revision.to_le_bytes());
    admission[40..48].copy_from_slice(&connection.fence.to_le_bytes());
    admission[49..57].copy_from_slice(&record.baseline_revision.unwrap().to_le_bytes());
    for (index, endpoint) in [record.source.identity, record.destination.identity]
        .iter()
        .enumerate()
    {
        let at = 57 + index * 64;
        admission[at..at + 32].copy_from_slice(&endpoint.volume);
        admission[at + 32..at + 64].copy_from_slice(&endpoint.root);
    }
    let mut wire = serde_json::to_value(operation).unwrap();
    wire["native_kind"] = "DirectoryV1".into();
    wire["native_identity"] = serde_json::json!(seal(admission));
    wire["native_budget"] = serde_json::to_value(format::NativeBudget::initial()).unwrap();
    let operation: format::Operation = serde_json::from_value(wire).unwrap();
    let id = operation.decode().unwrap().id;
    store.preflight_directory_intent(&operation).unwrap();
    let overlay = store.overlay_bytes;
    store.overlay_bytes = 8 * 1024 * 1024;
    assert!(matches!(
        store.preflight_directory_intent(&operation),
        Err(StoreError::LimitExceeded)
    ));
    store.overlay_bytes = overlay;
    let keys = store.overlay.len();
    assert!(keys < 1024);
    let length = store.log.metadata().unwrap().len();
    store.log.set_len(15 * 1024 * 1024 - 44).unwrap();
    store.log_identity = storage::regular(&store.log).unwrap();
    assert!(matches!(
        store.preflight_directory_intent(&operation),
        Err(StoreError::LimitExceeded)
    ));
    store.log.set_len(length).unwrap();
    store.log_identity = storage::regular(&store.log).unwrap();
    assert_eq!(store.connection(record.id).unwrap().pending, None);
    assert!(
        store.append(Change::Intent { operation }).is_ok(),
        "canonical Directory Intent must reach durable reduction"
    );
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(id.serial)
    );
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store.resume_native_for_test(id, &session).unwrap();
    assert_eq!(live.native_kind(), Some(NativeOperationKind::DirectoryV1));
    live.native_kind = None;
    assert!(store.check_native(&live).is_err());
    live.native_kind = Some(NativeOperationKind::DirectoryV1);
    let mut recovery = store.open_recovery_context(id, live.revision).unwrap();
    assert_eq!(recovery.native_kind(), live.native_kind());
    recovery.native_kind = None;
    assert!(store.check_recovery_context(&recovery).is_err());
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 64 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    if boundary == 5 {
        terminal_history(
            store,
            &path,
            &live,
            format::DirectoryBoundaryFact::Untouched,
        );
        return;
    }
    let reserved = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [32; 16])
        .unwrap();
    assert_eq!(
        serde_json::to_value(&reserved.registration).unwrap()["native_kind"],
        "DirectoryV1"
    );
    let (_, mut intent_bytes, mut stage_bytes) = directory_facts(nested);
    let stamp_header = |bytes: &mut Vec<u8>| {
        bytes.truncate(bytes.len() - 32);
        bytes[8..24].copy_from_slice(&id.connection.0);
        bytes[24..32].copy_from_slice(&id.serial.to_le_bytes());
        bytes[88..96].copy_from_slice(&live.admission.to_le_bytes());
        bytes[97..105].copy_from_slice(&live.baseline.unwrap().to_le_bytes());
        bytes[105..113].copy_from_slice(&live.revision.to_le_bytes());
        bytes[113..121].copy_from_slice(&live.fence.to_le_bytes());
    };
    stamp_header(&mut intent_bytes);
    stamp_header(&mut stage_bytes);
    for (index, endpoint) in live.endpoints.iter().enumerate() {
        let at = 121 + index * 64;
        intent_bytes[at..at + 32].copy_from_slice(&endpoint.volume);
        intent_bytes[at + 32..at + 64].copy_from_slice(&endpoint.root);
    }
    intent_bytes[303..335].copy_from_slice(&format::digest(live.native_identity.as_ref().unwrap()));
    let intent_bytes = seal(intent_bytes);
    stage_bytes[121..153].copy_from_slice(&intent_bytes[intent_bytes.len() - 32..]);
    let stage_bytes = seal(stage_bytes);
    let mut registered = reserved.registration;
    registered.intent = Some(intent_bytes.clone());
    registered.stage = Some(stage_bytes.clone());
    store
        .append(Change::NativePrepared {
            registration: registered.clone(),
            expected: live.revision,
        })
        .unwrap();
    live = store.resume_native_for_test(id, &session).unwrap();
    assert_eq!(
        store.operation(id).unwrap().decode().unwrap().stage,
        ReceiptStage::PreparedNoDisplacement
    );
    if matches!(boundary, 1 | 2) {
        let before = store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .clone();
        let wire = serde_json::json!({"event":"DirectoryBoundary","connection":record.id.0,"serial":id.serial,
            "expected":live.revision,"operation_expected":live.revision,"fence":live.fence,
            "fact":if boundary==1 {serde_json::json!({"kind":"Staged","attempt":1})}else{serde_json::json!({"kind":"Published","attempt":1})}});
        for fact in [
            serde_json::json!({"kind":"Untouched"}),
            serde_json::json!({"kind":"Staged","attempt":2}),
            serde_json::json!({"kind":"Staged","attempt":0}),
            serde_json::json!({"kind":"Published","attempt":1,"outcome":null}),
            serde_json::json!({"kind":"Published","attempt":1,"outcome":regular_descriptors().0}),
        ] {
            let mut invalid = wire.clone();
            invalid["fact"] = fact;
            assert!(
                !serde_json::from_value::<Change>(invalid)
                    .is_ok_and(|change| store.append(change).is_ok())
            );
        }
        let bad_regular = Change::NativeBoundary {
            connection: record.id.0,
            serial: id.serial,
            expected: live.revision,
            operation_expected: live.revision,
            fence: live.fence,
            kind: 1,
            evidence: Some(format::BoundaryEvidence {
                attempt: 1,
                displaced: None,
                outcome: None,
            }),
        };
        assert!(store.append(bad_regular).is_err());
        assert_eq!(
            store.connection(record.id).unwrap().pending,
            Some(id.serial)
        );
        let parsed = serde_json::from_value::<Change>(wire);
        assert!(
            parsed.is_ok(),
            "typed Directory staged boundary must deserialize: {parsed:?}"
        );
        store.append(parsed.unwrap()).unwrap();
        assert_eq!(store.connection(record.id).unwrap().pending, None);
        assert_eq!(
            store.executor_receipt(id).unwrap().stage,
            ReceiptStage::AccountedPending
        );
        assert!(store.operation(id).unwrap().recovery.is_empty());
        assert_eq!(
            store.load_baseline(record.id).unwrap().entries().unwrap(),
            &before
        );
        drop(store);
        let reopened = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            reopened.executor_receipt(id).unwrap().stage,
            ReceiptStage::AccountedPending
        );
        assert_eq!(reopened.connection(record.id).unwrap().pending, None);
        assert_eq!(
            reopened
                .load_baseline(record.id)
                .unwrap()
                .entries()
                .unwrap(),
            &before
        );
        return;
    }
    let mut outcome = intent_bytes[..121].to_vec();
    outcome[..4].copy_from_slice(b"QCO1");
    outcome[105..113].copy_from_slice(&live.revision.to_le_bytes());
    outcome.extend(&intent_bytes[intent_bytes.len() - 32..]);
    outcome.extend(&stage_bytes[stage_bytes.len() - 32..]);
    outcome.push(1);
    object(&mut outcome, 0o040_000, 53);
    outcome.push(1);
    object(&mut outcome, 0o040_000, 55);
    outcome.extend([3, 3]);
    outcome.extend(1_u64.to_le_bytes());
    registered.outcome = Some(seal(outcome));
    if boundary == 3 {
        terminal_history(
            store,
            &path,
            &live,
            format::DirectoryBoundaryFact::Published {
                attempt: 1,
                outcome: registered.outcome,
            },
        );
        return;
    }
    for verified in [false, true] {
        let context = store.native_read_context(&live, deadline, &cancel).unwrap();
        store
            .append_native(
                Change::NativeOutcome {
                    registration: registered.clone(),
                    expected: live.revision,
                    verified,
                    reconstructed: false,
                },
                &context,
            )
            .unwrap();
        live = store.resume_native_for_test(id, &session).unwrap();
    }
    if boundary == 4 {
        terminal_history(
            store,
            &path,
            &live,
            format::DirectoryBoundaryFact::Published {
                attempt: 1,
                outcome: registered.outcome,
            },
        );
        return;
    }
    let mut entries = store
        .load_baseline(record.id)
        .unwrap()
        .entries()
        .unwrap()
        .clone();
    if nested {
        entries.insert(RelativePath::unix(b"a").unwrap(), EntryState::Directory);
        entries.insert(RelativePath::unix(b"a/b").unwrap(), EntryState::Directory);
    }
    entries.insert(RelativePath::unix(leaf).unwrap(), EntryState::Directory);
    let root = storage::build_pages(
        &store.storage,
        entries.iter().enumerate().map(|(ordinal, (path, state))| {
            Ok(Row::Baseline(format::BaselineEntry {
                connection: record.id.0,
                revision: live.revision + 1,
                ordinal: ordinal as u32,
                path: path.clone().into(),
                state: state.clone().into(),
            }))
        }),
    )
    .unwrap();
    let context = store.native_read_context(&live, deadline, &cancel).unwrap();
    let mut wire = serde_json::json!({"event":"BaselineCommit", "connection":record.id.0,
        "expected":live.revision, "root":root, "count":entries.len(),
        "source":storage::Identity::from(record.source.identity),
        "destination":storage::Identity::from(record.destination.identity), "initial_review":false});
    if nested {
        wire["directory_ancestors"] = serde_json::json!({"version":1,
            "admission":format::digest(live.native_identity.as_ref().unwrap()),"prefixes":6_u64});
    }
    let change = serde_json::from_value::<Change>(wire.clone());
    assert!(
        change.is_ok(),
        "typed Directory ancestor receipt: {change:?}"
    );
    let receipt = serde_json::json!({"version":1,"admission":format::digest(live.native_identity.as_ref().unwrap()),"prefixes":6_u64});
    for case in 0..5 {
        let mut wrong = wire.clone();
        wrong["directory_ancestors"] = receipt.clone();
        match case {
            0 if nested => {
                wrong.as_object_mut().unwrap().remove("directory_ancestors");
            }
            0 => {
                wrong["directory_ancestors"]["prefixes"] = 0.into();
            }
            1 => {
                wrong["directory_ancestors"]["prefixes"] = 2.into();
            }
            2 => {
                wrong["directory_ancestors"]["prefixes"] = 14.into();
            }
            3 => {
                wrong["directory_ancestors"]["admission"] =
                    serde_json::json!(([99_u8; 32]).to_vec());
            }
            _ => {
                wrong["directory_ancestors"]["version"] = 2.into();
            }
        }
        let wrong: Change = serde_json::from_value(wrong).unwrap();
        let sequence = store.sequence;
        assert!(
            store.append_native(wrong, &context).is_err(),
            "receipt variant {case}"
        );
        assert_eq!(store.sequence, sequence);
    }
    for extra in [false, true] {
        let mut changed = entries.clone();
        if extra {
            changed.insert(RelativePath::unix(b"extra").unwrap(), EntryState::Directory);
        } else {
            changed.remove(&RelativePath::unix(b"file").unwrap());
        }
        assert_ne!(changed, entries);
        let bad_root = storage::build_pages(
            &store.storage,
            changed.iter().enumerate().map(|(ordinal, (path, state))| {
                Ok(Row::Baseline(format::BaselineEntry {
                    connection: record.id.0,
                    revision: live.revision + 1,
                    ordinal: ordinal as u32,
                    path: path.clone().into(),
                    state: state.clone().into(),
                }))
            }),
        )
        .unwrap();
        let mut wrong = wire.clone();
        wrong["root"] = serde_json::to_value(bad_root).unwrap();
        wrong["count"] = serde_json::json!(changed.len());
        assert!(
            store
                .append_native(serde_json::from_value(wrong).unwrap(), &context)
                .is_err(),
            "entire candidate must match old plus exact patch"
        );
    }
    store.append_native(change.unwrap(), &context).unwrap();
    assert_eq!(
        store.load_baseline(record.id).unwrap().entries(),
        Some(&entries)
    );
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.load_baseline(record.id).unwrap().entries(),
        Some(&entries)
    );
}

#[test]
fn directory_canonical_encoders_match_independent_wire() {
    for nested in [false, true] {
        let (admission, intent, stage) = directory_facts(nested);
        assert_eq!(
            d::directory::Admission::decode(&admission)
                .unwrap()
                .encode()
                .unwrap(),
            admission
        );
        for bytes in [intent, stage] {
            let decoded = d::directory::decode(&bytes).unwrap();
            assert_eq!(
                d::directory::encode(&decoded.header, &decoded.body).unwrap(),
                bytes
            );
        }
    }
}

#[test]
fn directory_planner_selects_one_parent_both_directions_with_empty_or_nonempty_source() {
    use crate::connection::{
        model::{Encoding, Endpoint, NamePolicy, Observation, ObservedTree},
        transaction::directory::{DirectoryPlan, plan_directory},
    };
    for reverse in [false, true] {
        for nonempty in [false, true] {
            for nested in [false, true] {
                let (_temp, path) = fixture();
                let mut store = ConnectionStore::open_at(&path).unwrap();
                let record = create(&mut store, 951);
                commit_test_baseline(&mut store, record.id);
                let record = store.load_connection(record.id).unwrap();
                let baseline = store.load_baseline(record.id).unwrap();
                let leaf = RelativePath::unix(if nested { b"a/b/dir" } else { b"dir" }).unwrap();
                let mut common = std::collections::BTreeMap::new();
                if nested {
                    for p in [b"a".as_slice(), b"a/b"] {
                        common.insert(RelativePath::unix(p).unwrap(), EntryState::Directory);
                    }
                }
                let mut added = common.clone();
                added.insert(leaf.clone(), EntryState::Directory);
                if nonempty {
                    added.insert(
                        RelativePath::unix(if nested {
                            b"a/b/dir/child"
                        } else {
                            b"dir/child"
                        })
                        .unwrap(),
                        EntryState::Directory,
                    );
                }
                let tree = |identity, entries| {
                    ObservedTree::new(
                        Endpoint {
                            identity,
                            encoding: Encoding::Unix,
                            names: NamePolicy::Exact,
                        },
                        entries,
                        Observation::Complete,
                    )
                    .unwrap()
                };
                let (left, right) = if reverse {
                    (common, added)
                } else {
                    (added, common)
                };
                let sequence = store.sequence;
                let plan = plan_directory(
                    &store,
                    record.id,
                    &baseline,
                    &tree(record.source.identity, left),
                    &tree(record.destination.identity, right),
                )
                .unwrap();
                let DirectoryPlan::Ready(intent) = plan else {
                    panic!("fresh Directory/Absent proposal must select its parent first")
                };
                assert_eq!(intent.path, leaf);
                assert_eq!(
                    intent.decision,
                    Decision::Propagate {
                        from: if reverse {
                            Side::Destination
                        } else {
                            Side::Source
                        }
                    }
                );
                assert_eq!(
                    store.sequence, sequence,
                    "read-only plan never appends Intent"
                );
                assert!(store.connection(record.id).unwrap().pending.is_none());
            }
        }
    }
}

#[test]
fn directory_terminal_boundary_preserves_baseline_and_receipt_after_reopen() {
    for boundary in 1..=5 {
        directory_history(false, boundary);
    }
}

// Independent encoder fixture: ordered bytes, without using production Writer/encoder.
fn extrema_facts(utf16: bool, maximum: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let depth = if maximum { 64_u8 } else { 1 };
    let mut units = Vec::<u16>::new();
    if maximum {
        for index in 0..64 {
            if index > 0 {
                units.push(if utf16 { 92 } else { 47 });
            }
            units.extend(std::iter::repeat_n(
                if utf16 { 0xD800 } else { 255 },
                if index == 0 { 64 } else { 63 },
            ));
        }
        while units.len() < 4096 {
            units.push(if utf16 { 0xDC00 } else { 254 });
        }
    } else {
        units.push(if utf16 { 0xD800 } else { 255 });
    }
    assert_eq!(units.len(), if maximum { 4096 } else { 1 });
    let mut tail = vec![u8::from(utf16), depth];
    tail.extend(u16::try_from(units.len()).unwrap().to_le_bytes());
    for unit in units {
        if utf16 {
            tail.extend(unit.to_le_bytes());
        } else {
            tail.push(u8::try_from(unit).unwrap());
        }
    }
    for root in [51_u8, 52] {
        tail.push(depth);
        for index in 0..depth {
            object(&mut tail, 0o040_000, root + index);
        }
    }
    let mut source = Vec::new();
    directory_path_chains_and_states(&mut source, false);
    tail.extend(&source[113..]); // After P(7) + two chains(53 each): complete independently constructed leaf states.
    let (base, _, _) = directory_facts(false);
    let mut admission = base[..187].to_vec();
    admission.extend(&tail);
    let admission = seal(admission);
    let mut intent = header(*b"QCI1");
    endpoints_and_direction(&mut intent);
    object(&mut intent, 0o040_000, 54);
    intent.extend(sha2::Sha256::digest(&admission));
    intent.extend([0; 34]);
    intent.extend(tail);
    let intent = seal(intent);
    let mut stage = header(*b"QCS1");
    stage.extend(&intent[intent.len() - 32..]);
    for identity in [55, 53, 52 + depth - 1] {
        object(&mut stage, 0o040_000, identity);
    }
    stage.push(1);
    stage.extend(sha2::Sha256::digest(
        b"qos-connected-directory-inotify-s-e-v1",
    ));
    stage.push(3);
    (admission, intent, seal(stage))
}
#[test]
fn directory_lossless_extrema_and_actual_enclosing_sizes() {
    for utf16 in [false, true] {
        for maximum in [false, true] {
            let (admission, intent, stage) = extrema_facts(utf16, maximum);
            let expected = match (utf16, maximum) {
                (false, false) => 444,
                (true, false) => 445,
                (false, true) => 11091,
                (true, true) => 15187,
            };
            assert_eq!(admission.len(), expected);
            assert_eq!(intent.len(), expected + 182);
            assert_eq!(stage.len(), 375);
            let fact = d::directory::Admission::decode(&admission).unwrap();
            assert_eq!(fact.encode().unwrap(), admission);
            let decoded = d::directory::decode(&intent).unwrap();
            fact.joined(&admission, &decoded).unwrap();
            let staged = d::directory::decode(&stage).unwrap();
            staged.joined(&decoded).unwrap();
            for value in [&decoded, &staged] {
                assert_eq!(
                    d::directory::encode(&value.header, &value.body).unwrap(),
                    if matches!(value.body, d::directory::Body::Intent(_)) {
                        intent.clone()
                    } else {
                        stage.clone()
                    }
                );
            }
            let mut wire = registration(&intent, &stage);
            wire["native_kind"] = "DirectoryV1".into();
            let registration: format::NativeRegistration = serde_json::from_value(wire).unwrap();
            registration.validate().unwrap();
            let row = Row::NativeRegistration(registration.clone());
            let row_bytes = format::encode(&row).unwrap().len();
            let page_bytes = format::encode(&format::PageNode {
                schema: 1,
                node: format::Node::Leaf(vec![row]),
            })
            .unwrap()
            .len();
            let event_bytes = format::encode(&format::Event {
                schema: 1,
                predecessor: [255; 32],
                change: Change::NativePrepared {
                    registration,
                    expected: u64::MAX,
                },
            })
            .unwrap()
            .len();
            assert!(row_bytes <= 131_072 && page_bytes <= 131_072 && event_bytes <= 131_072);
            println!(
                "directory Rust serde utf16={utf16} maximum={maximum}: QNA={} QCI={} QCS={} row={row_bytes} page={page_bytes} event={event_bytes} frame={}",
                admission.len(),
                intent.len(),
                stage.len(),
                event_bytes + 44
            );
            let mut truncated = intent.clone();
            truncated.pop();
            assert!(d::directory::decode(&truncated).is_err());
            let mut chain_present = intent[..intent.len() - 32].to_vec();
            chain_present[335] = 1;
            assert!(d::directory::decode(&seal(chain_present)).is_err());
        }
    }
}

#[test]
fn directory_provider_profile_barriers_and_hash_joins_are_strict() {
    let (_, intent, stage) = directory_facts(false);
    for (at, byte) in [(309, 2), (310, 0), (342, 0), (173, 0)] {
        let mut invalid = stage[..stage.len() - 32].to_vec();
        if at == 173 {
            invalid[173..205].fill(0);
        } else {
            invalid[at] = byte;
        }
        let invalid = seal(invalid);
        let mut wire = registration(&intent, &invalid);
        wire["native_kind"] = "DirectoryV1".into();
        assert!(!accepted(wire));
    }
    for at in [8, 24, 32, 40, 56, 72, 88, 97, 105, 113, 121, 205, 257] {
        let mut invalid = stage[..stage.len() - 32].to_vec();
        invalid[at] ^= 1;
        let invalid = seal(invalid);
        let mut wire = registration(&intent, &invalid);
        wire["native_kind"] = "DirectoryV1".into();
        assert!(!accepted(wire), "mismatched stage join at byte {at}");
    }
}

fn terminal_history(
    mut store: ConnectionStore,
    path: &std::path::Path,
    live: &LiveOperation,
    fact: format::DirectoryBoundaryFact,
) {
    let id = live.id();
    let before = store
        .load_baseline(id.connection)
        .unwrap()
        .entries()
        .unwrap()
        .clone();
    let budget = store.operation(id).unwrap().native_budget;
    let change = Change::DirectoryBoundary {
        connection: id.connection.0,
        serial: id.serial,
        expected: live.revision,
        operation_expected: live.revision,
        fence: live.fence,
        fact,
    };
    let wire = serde_json::to_value(change).unwrap();
    for key in ["serial", "expected", "operation_expected", "fence"] {
        let mut invalid = wire.clone();
        invalid[key] = serde_json::json!(wire[key].as_u64().unwrap() + 1);
        assert!(
            store
                .append(serde_json::from_value(invalid).unwrap())
                .is_err()
        );
    }
    store.append(serde_json::from_value(wire).unwrap()).unwrap();
    assert_eq!(
        store.executor_position(id.connection).unwrap(),
        (None, Some(id))
    );
    assert_eq!(
        store.executor_receipt(id).unwrap().stage,
        ReceiptStage::AccountedPending
    );
    assert_eq!(store.operation(id).unwrap().native_budget, budget);
    assert!(store.operation(id).unwrap().recovery.is_empty());
    drop(store);
    let reopened = ConnectionStore::open_at(path).unwrap();
    assert_eq!(
        reopened.executor_position(id.connection).unwrap(),
        (None, Some(id))
    );
    assert_eq!(
        reopened.executor_receipt(id).unwrap().stage,
        ReceiptStage::AccountedPending
    );
    assert_eq!(reopened.operation(id).unwrap().native_budget, budget);
    assert_eq!(
        reopened
            .load_baseline(id.connection)
            .unwrap()
            .entries()
            .unwrap(),
        &before
    );
}
#[test]
fn directory_new_stage_identity_is_bound_by_outcome_not_absent_admission() {
    let (intent, stage) = directory_descriptors();
    let mut changed = stage[..stage.len() - 32].to_vec();
    changed[173] ^= 1;
    let changed = seal(changed);
    let mut wire = registration(&intent, &changed);
    wire["native_kind"] = "DirectoryV1".into();
    assert!(
        accepted(wire.clone()),
        "created object is introduced by QCS1; QCI1 target was Absent"
    );
    let mut outcome = header(*b"QCO1");
    outcome[105..113].copy_from_slice(&14_u64.to_le_bytes());
    outcome.extend(&intent[intent.len() - 32..]);
    outcome.extend(&changed[changed.len() - 32..]);
    outcome.push(1);
    object(&mut outcome, 0o040_000, 53);
    outcome.push(1);
    object(&mut outcome, 0o040_000, 55);
    outcome.extend([3, 3]);
    outcome.extend(1_u64.to_le_bytes());
    wire["outcome"] = serde_json::json!(seal(outcome));
    assert!(
        !accepted(wire),
        "QCO1 must name the exact QCS1 created object even with repaired descriptor hashes"
    );
}

#[test]
fn directory_actual_operation_and_bounded_unknown_future_encoding_fit() {
    let (admission, intent, stage) = extrema_facts(true, true);
    let fact = d::directory::Admission::decode(&admission).unwrap();
    let operation = serde_json::json!({"native_kind":"DirectoryV1","native_identity":admission,"native_budget":format::NativeBudget::initial(),
        "connection":([31_u8;16].to_vec()),"serial":7,"revision":11,"expected":10,"baseline":7,
        "path":format::NativePath::from(fact.path),"source":{"kind":"Directory"},"destination":null,
        "source_binding":{"volume":([41_u8;32].to_vec()),"root":([41_u8;32].to_vec())},
        "destination_binding":{"volume":([42_u8;32].to_vec()),"root":([42_u8;32].to_vec())},"decision":1,"stage":0,"recovery":[]});
    let mut operation: format::Operation = serde_json::from_value(operation).unwrap();
    operation.decode().unwrap();
    let actual = format::encode(&Row::Operation(operation.clone()))
        .unwrap()
        .len();
    // Ceiling-only future bytes; these placeholders are never decoded or appended as authority.
    operation.revision = u64::MAX;
    operation.stage = 7;
    operation.native_boundary = Some(3);
    operation.native_directory_boundary = Some(format::DirectoryBoundaryFact::Published {
        attempt: 8,
        outcome: Some(vec![255; 333]),
    });
    let row = Row::Operation(operation);
    let future = format::encode(&row).unwrap().len();
    let future_page = format::encode(&format::PageNode {
        schema: 1,
        node: format::Node::Leaf(vec![row]),
    })
    .unwrap()
    .len();
    let mut wire = registration(&intent, &stage);
    wire["native_kind"] = "DirectoryV1".into();
    let mut registration: format::NativeRegistration = serde_json::from_value(wire).unwrap();
    registration.validate().unwrap();
    registration.connection = [255; 16];
    registration.serial = u64::MAX;
    registration.admission = u64::MAX;
    registration.baseline = Some(u64::MAX);
    registration.nonce = [255; 16];
    registration.attempt = 8;
    registration.fence = u64::MAX;
    registration.intent = Some(vec![255; 15_369]);
    registration.stage = Some(vec![255; 375]);
    registration.outcome = Some(vec![255; 333]);
    let row = Row::NativeRegistration(registration.clone());
    let registration_size = format::encode(&row).unwrap().len();
    let page = format::encode(&format::PageNode {
        schema: 1,
        node: format::Node::Leaf(vec![row]),
    })
    .unwrap()
    .len();
    let event = format::encode(&format::Event {
        schema: 1,
        predecessor: [255; 32],
        change: Change::NativeOutcome {
            registration,
            expected: u64::MAX,
            verified: false,
            reconstructed: false,
        },
    })
    .unwrap()
    .len();
    assert!(
        actual <= 131_072
            && future <= 131_072
            && future_page <= 131_072
            && registration_size <= 131_072
            && page <= 131_072
            && event <= 131_072
    );
    println!(
        "directory Rust enclosing actual-operation-row={actual} terminal-ceiling-row={future} terminal-page={future_page} registration-ceiling-row={registration_size} registration-page={page} outcome-event={event} outcome-frame={}",
        event + 44
    );
}

#[cfg(unix)]
mod connection_capacity {
    use super::*;

    const ID: ConnectionId = ConnectionId([95; 16]);

    fn singleton(row: &Row) -> Result<usize, StoreError> {
        format::encode(&format::PageNode {
            schema: 1,
            node: format::Node::Leaf(vec![row.clone()]),
        })
        .map(|bytes| bytes.len())
    }

    fn baseline_root(store: &ConnectionStore) -> [u8; 32] {
        storage::build_pages(
            &store.storage,
            std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
                connection: ID.0,
                revision: 4,
                ordinal: 0,
                path: format::NativePath::Unix(b"file".to_vec()),
                state: format::State::File {
                    sha256: [7; 32],
                    bytes: 3,
                    executable: false,
                },
            }))),
        )
        .unwrap()
        .unwrap()
    }

    fn initial_connection(padding: usize) -> format::Connection {
        let (mut source, destination) = bindings();
        let locator = "/".to_owned() + &"x".repeat(padding);
        source.locator = EndpointLocator::new(Path::new(&locator)).unwrap();
        format::Connection::new(
            ID,
            source,
            destination,
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap()
    }

    // This clone selects padding only; create_connection and real baseline events
    // must independently produce the same state before it is used in preflight.
    fn expected_baseline(padding: usize, root: [u8; 32]) -> format::Connection {
        let mut connection = initial_connection(padding);
        connection.revision = 4;
        connection.serial = 1;
        connection.baseline = Some(4);
        connection.baseline_root = Some(root);
        connection.baseline_count = 1;
        connection.decode().unwrap();
        connection
    }

    fn largest_padding(root: [u8; 32]) -> usize {
        let fits = |padding| singleton(&Row::Connection(expected_baseline(padding, root)));
        assert!(fits(1).is_ok());
        assert!(matches!(fits(32_767), Err(StoreError::LimitExceeded)));
        let (mut low, mut high) = (1, 32_767);
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            match fits(middle) {
                Ok(_) => low = middle,
                Err(StoreError::LimitExceeded) => high = middle,
                Err(error) => panic!("unexpected candidate encoding error: {error:?}"),
            }
        }
        assert!(low > 10);
        assert!(format::MAX_BYTES - fits(low).unwrap() <= 3);
        low
    }

    fn admit_and_reopen(
        mut store: ConnectionStore,
        path: &Path,
        padding: usize,
        root: [u8; 32],
    ) -> ConnectionStore {
        assert_eq!(store.digest, [0; 32]);
        let initial = initial_connection(padding);
        let value = format::encode(&initial).unwrap().len();
        let create_bytes = format::encode(&format::Event {
            schema: 1,
            predecessor: store.digest,
            change: Change::Create {
                record: initial.clone(),
            },
        })
        .unwrap()
        .len();
        assert_eq!(create_bytes, value + 130);
        assert_eq!(
            singleton(&Row::Connection(initial.clone())).unwrap(),
            value + 76
        );
        let record = initial.decode().unwrap();
        store
            .create_connection(ID, record.source, record.destination, record.policy)
            .unwrap();
        assert_eq!(store.connection(ID).unwrap(), initial);
        assert_eq!(commit_test_baseline(&mut store, ID), root);
        let expected = expected_baseline(padding, root);
        assert_eq!(store.connection(ID).unwrap(), expected);
        let row = Row::Connection(expected.clone());
        let row_bytes = format::encode(&row).unwrap().len();
        let page_bytes = singleton(&row).unwrap();
        let root_bytes = format::encode(&root).unwrap().len();
        assert_eq!(page_bytes, value + root_bytes + 69);
        assert_eq!(page_bytes, row_bytes + 45);
        assert!(create_bytes < page_bytes);
        store.checkpoint().unwrap();
        drop(store);
        let reopened = ConnectionStore::open_at(path).unwrap();
        assert_eq!(reopened.connection(ID).unwrap(), expected);
        assert_eq!(
            singleton(&Row::Connection(reopened.connection(ID).unwrap())).unwrap(),
            page_bytes
        );
        let baseline = reopened.load_baseline(ID).unwrap();
        let entries = baseline.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries.get(&RelativePath::unix(b"file").unwrap()),
            intent(&reopened.load_connection(ID).unwrap())
                .source_before
                .as_ref()
        );
        println!(
            "directory Connection padding={padding} create-value={value} create-event={create_bytes} current-row={row_bytes} current-page={page_bytes} root-json={root_bytes}"
        );
        reopened
    }

    fn directory_operation(store: &ConnectionStore) -> format::Operation {
        let record = store.load_connection(ID).unwrap();
        let connection = store.connection(ID).unwrap();
        let mut proposal = intent(&record);
        proposal.expected_baseline = record.baseline_revision;
        proposal.path = RelativePath::unix(b"dir").unwrap();
        proposal.source_before = Some(EntryState::Directory);
        let mut operation =
            format::Operation::new(proposal, connection.serial + 1, &connection).unwrap();
        let (mut admission, _, _) = directory_facts(false);
        admission.truncate(admission.len() - 32);
        admission[8..24].copy_from_slice(&ID.0);
        admission[24..32].copy_from_slice(&operation.serial.to_le_bytes());
        admission[32..40].copy_from_slice(&record.revision.to_le_bytes());
        admission[40..48].copy_from_slice(&connection.fence.to_le_bytes());
        admission[49..57].copy_from_slice(&record.baseline_revision.unwrap().to_le_bytes());
        for (index, endpoint) in [record.source.identity, record.destination.identity]
            .iter()
            .enumerate()
        {
            let at = 57 + index * 64;
            admission[at..at + 32].copy_from_slice(&endpoint.volume);
            admission[at + 32..at + 64].copy_from_slice(&endpoint.root);
        }
        operation.native_kind = Some(NativeOperationKind::DirectoryV1);
        operation.native_identity = Some(seal(admission));
        operation.native_budget = Some(format::NativeBudget::initial());
        operation.decode().unwrap();
        operation
    }

    fn assert_preflight_boundary(store: &ConnectionStore, refuses: bool) {
        let operation = directory_operation(store);
        let current = store.connection(ID).unwrap();
        let current_page = singleton(&Row::Connection(current.clone())).unwrap();
        let rows = store.reduce_intent(&operation).unwrap();
        for row in &rows {
            format::encode(row).unwrap();
            let length = singleton(row).unwrap();
            if matches!(row, Row::Connection(_)) {
                assert_eq!(length, current_page - 3);
                println!(
                    "directory immediate Connection row={} page={length}",
                    format::encode(row).unwrap().len()
                );
            }
        }
        assert!(store.prepare_rows(rows).unwrap().fits_overlay());
        let event = format::encode(&format::Event {
            schema: 1,
            predecessor: store.digest,
            change: Change::Intent {
                operation: operation.clone(),
            },
        })
        .unwrap();
        assert!(store.log.metadata().unwrap().len() + event.len() as u64 + 44 < 15 * 1024 * 1024);
        let mut future = current;
        future.revision = u64::MAX;
        future.serial = operation.serial;
        future.baseline = Some(u64::MAX);
        future.pending = None;
        let row = Row::Connection(future);
        let row_bytes = format::encode(&row).unwrap().len();
        let page = format::PageNode {
            schema: 1,
            node: format::Node::Leaf(vec![row]),
        };
        // Uncapped serde reports the oversize length only; capped production encoding decides admission.
        let page_bytes = serde_json::to_vec(&page).unwrap().len();
        assert_eq!(page_bytes, current_page + 38);
        assert_eq!(row_bytes, current_page - 7);
        if refuses {
            assert!((format::MAX_BYTES + 35..=format::MAX_BYTES + 38).contains(&page_bytes));
            assert!(matches!(
                format::encode(&page),
                Err(StoreError::LimitExceeded)
            ));
        } else {
            assert!(page_bytes <= format::MAX_BYTES - 2);
            assert_eq!(format::encode(&page).unwrap().len(), page_bytes);
        }
        let snapshot = || {
            (
                store.sequence,
                store.digest,
                store.current.generation,
                store.overlay.clone(),
                store.overlay_bytes,
                store.executor_position(ID).unwrap(),
                store.load_baseline(ID).unwrap(),
                store
                    .rows()
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                std::fs::read(
                    store
                        .storage
                        .path
                        .join(format!("log-{}", store.current.log)),
                )
                .unwrap(),
                std::fs::read(store.storage.path.join("CURRENT")).unwrap(),
            )
        };
        let before = snapshot();
        let result = store.preflight_directory_intent(&operation);
        if refuses {
            assert!(matches!(result, Err(StoreError::LimitExceeded)));
        } else {
            result.unwrap();
        }
        assert_eq!(snapshot(), before);
        assert_eq!(store.connection(ID).unwrap().pending, None);
        assert!(
            store
                .row(&format::key(1, ID.0, operation.serial))
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .rows()
                .unwrap()
                .all(|row| !matches!(row.unwrap(), Row::NativeRegistration(_)))
        );
        println!(
            "directory Connection future-row={row_bytes} future-page={page_bytes} intent-event={} refused={refuses}",
            event.len()
        );
    }

    #[test]
    fn directory_connection_singleton_preflight_uses_valid_reopened_history() {
        for shrink in [0, 10] {
            let (_temp, path) = fixture();
            let store = ConnectionStore::open_at(&path).unwrap();
            let root = baseline_root(&store);
            let padding = largest_padding(root) - shrink;
            let store = admit_and_reopen(store, &path, padding, root);
            assert_preflight_boundary(&store, shrink == 0);
        }
    }
}

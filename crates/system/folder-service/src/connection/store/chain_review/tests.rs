use super::super::*;

fn control_wire() -> serde_json::Value {
    serde_json::json!({
        "event": "InitialReviewControl",
        "control": {
            "version": 1, "connection": ([33; 16]), "review": ([8; 32]),
            "origin": 1, "expected_progress": 2, "expected_schedule": 1,
            "control_sequence": 1, "current": {"kind": "None"},
            "transition": {"kind": "StopRequested", "reason": {"kind": "Pause"}}
        }
    })
}

#[test]
fn whole_review_control_wire_is_closed_and_retained() {
    let wire = control_wire();
    let decoded = serde_json::from_value::<Change>(wire.clone());
    assert!(
        decoded.is_ok(),
        "closed inert review control must decode: {decoded:?}"
    );
    assert_eq!(serde_json::to_value(decoded.unwrap()).unwrap(), wire);
    let mut extra = wire;
    extra["control"]["current"]["authority"] = true.into();
    assert!(serde_json::from_value::<Change>(extra).is_err());
}

fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("store");
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let binding = |n| EndpointBinding {
        identity: EndpointIdentity {
            volume: [n; 32],
            root: [n + 1; 32],
        },
        locator: EndpointLocator::new(Path::new(if n == 1 { "/source" } else { "/destination" }))
            .unwrap(),
    };
    let record = store
        .create_connection(
            ConnectionId([33; 16]),
            binding(1),
            binding(3),
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap()
        .value;
    (temp, path, store, record)
}
fn inert_cursor(id: ConnectionId) -> format::InitialReviewCursorV1 {
    format::InitialReviewCursorV1 {
        version: 1,
        connection: id.0,
        review: [8; 32],
        origin: 1,
        progress_revision: 2,
        schedule_revision: 1,
        control_sequence: 0,
        state: format::DirectoryChainScheduleState::Running,
        next_step: 0,
        current: format::InitialReviewCurrentV1::None {},
    }
}
#[test]
fn whole_review_default_denial_orphan_and_cached_no_inner() {
    let (_temp, _path, mut store, record) = fixture();
    let cached = store.transaction_session(record.id).unwrap();
    store.executor_session(record.id, &cached).unwrap();
    for state in [
        format::DirectoryChainScheduleState::Running,
        format::DirectoryChainScheduleState::Stopped {
            reason: format::PauseOrCancel::Pause,
        },
    ] {
        let mut cursor = inert_cursor(record.id);
        cursor.state = state;
        // Deliberately orphaned no-inner ownership: not valid completed history.
        let row = Row::InitialReviewCursor(cursor);
        store.overlay.insert(row.key(), row);
        assert!(
            matches!(
                store.transaction_session(record.id),
                Err(StoreError::RecoveryRequired)
            ),
            "outer ownership, even orphaned, must deny a fresh session"
        );
        assert!(
            matches!(
                store.executor_session(record.id, &cached),
                Err(StoreError::RecoveryRequired)
            ),
            "cached pre-acceptance session must not cross consent"
        );
        let intent = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"a").unwrap(),
            source_before: Some(EntryState::Directory),
            destination_before: None,
            decision: Decision::Propagate { from: Side::Source },
        };
        assert!(matches!(
            store.begin_intent(intent),
            Err(StoreError::RecoveryRequired)
        ));
    }
}

fn review_fixture(
    store: &ConnectionStore,
    record: &ConnectionRecord,
) -> (
    format::InitialChainReviewV1,
    Vec<format::InitialChainReviewEntryV1>,
) {
    use format::{InitialReviewEntrySideV1 as S, InitialReviewSideV1 as V};
    use sha2::{Digest, Sha256};
    let file = format::State::File {
        sha256: [6; 32],
        bytes: 19,
        executable: false,
    };
    let present = |ordinal, state| S::Present {
        entry: V { ordinal, state },
    };
    let data = [
        (b"a".as_slice(), [present(0, file.clone()), S::Absent {}]),
        (b"b".as_slice(), [S::Absent {}, present(0, file.clone())]),
        (
            b"equal".as_slice(),
            [present(1, file.clone()), present(1, file)],
        ),
        (
            b"unsupported-directory".as_slice(),
            [
                present(2, format::State::Directory),
                present(2, format::State::Directory),
            ],
        ),
    ];
    let entries: Vec<_> = data
        .into_iter()
        .enumerate()
        .map(
            |(ordinal, (path, sides))| format::InitialChainReviewEntryV1 {
                version: 1,
                connection: record.id.0,
                origin: record.revision,
                ordinal: ordinal as u32,
                path: format::NativePath::Unix(path.to_vec()),
                sides,
            },
        )
        .collect();
    let root = storage::build_pages(
        &store.storage,
        entries
            .iter()
            .cloned()
            .map(|r| Ok(Row::InitialChainReviewEntry(r))),
    )
    .unwrap()
    .unwrap();
    let c = store.connection(record.id).unwrap();
    let original = format::encode(&c).unwrap();
    let mut digest = Sha256::new();
    digest.update(b"Connected explicit initial preview v1");
    digest.update(&original);
    for side in 0..2 {
        digest.update([side as u8]);
        digest.update(3u64.to_le_bytes());
        for entry in &entries {
            if let S::Present { entry: value } = &entry.sides[side] {
                let bytes = format::encode(&format::BaselineEntry {
                    connection: record.id.0,
                    revision: record.revision,
                    ordinal: value.ordinal,
                    path: entry.path.clone(),
                    state: value.state.clone(),
                })
                .unwrap();
                digest.update((bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            }
        }
    }
    (
        format::InitialChainReviewV1 {
            version: 1,
            connection: record.id.0,
            original_revision: record.revision,
            fence: record.fence_generation,
            preview: digest.finalize().into(),
            endpoints: [c.source.identity, c.destination.identity],
            encodings: [1, 1],
            policies: [
                format::ReviewNamePolicyV1::Exact {},
                format::ReviewNamePolicyV1::Exact {},
            ],
            counts: [3, 3],
            count: 4,
            root,
            original,
        },
        entries,
    )
}
fn accepted_event(review: format::InitialChainReviewV1) -> Change {
    let mut cursor = inert_cursor(ConnectionId(review.connection));
    cursor.origin = review.original_revision;
    cursor.progress_revision = review.original_revision + 1;
    cursor.review = format::digest(&format::encode(&review).unwrap());
    Change::InitialReviewAccepted { review, cursor }
}
fn history_write(store: &mut ConnectionStore, change: Change) {
    let event = Event {
        schema: 1,
        predecessor: store.digest,
        change,
    };
    let mut sequence = store.sequence + 1;
    framing::write_record(&mut store.log, &mut sequence, &event).unwrap();
    storage::sync_file(&store.log).unwrap();
}
#[test]
fn whole_review_complete_siblings_and_equality_replay() {
    let (_temp, path, mut store, record) = fixture();
    let (review, entries) = review_fixture(&store, &record);
    let original = review.original.clone();
    history_write(&mut store, accepted_event(review.clone()));
    drop(store);
    let opened = ConnectionStore::open_at(&path);
    assert!(
        opened.is_ok(),
        "complete inert acceptance must replay without a canonical operation: {:?}",
        opened.as_ref().err()
    );
    let mut store = opened.unwrap();
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    let row = store
        .row(&format::key(7, record.id.0, record.revision))
        .unwrap()
        .unwrap();
    let Row::InitialChainReview(saved) = row else {
        panic!("review fact missing")
    };
    assert_eq!(saved.original, original);
    let stored: Vec<_> = storage::TreeIter::new(&store.storage, Some(saved.root))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        stored
            .iter()
            .map(|r| format::encode(r).unwrap())
            .collect::<Vec<_>>(),
        entries
            .into_iter()
            .map(|r| format::encode(&Row::InitialChainReviewEntry(r)).unwrap())
            .collect::<Vec<_>>()
    );
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert!(matches!(
        store
            .row(&format::key(7, record.id.0, record.revision))
            .unwrap(),
        Some(Row::InitialChainReview(_))
    ));
}

fn observations(record: &ConnectionRecord) -> [crate::connection::model::ObservedTree; 2] {
    use crate::connection::model::{Endpoint, NamePolicy, Observation, ObservedTree};
    let file = EntryState::File {
        sha256: [6; 32],
        bytes: 19,
        executable: false,
    };
    [record.source.identity, record.destination.identity]
        .into_iter()
        .enumerate()
        .map(|(side, identity)| {
            ObservedTree::new(
                Endpoint {
                    identity,
                    encoding: Encoding::Unix,
                    names: NamePolicy::Exact,
                },
                [
                    (
                        RelativePath::unix(if side == 0 { b"a" } else { b"b" }).unwrap(),
                        file.clone(),
                    ),
                    (RelativePath::unix(b"equal").unwrap(), file.clone()),
                    (
                        RelativePath::unix(b"unsupported-directory").unwrap(),
                        EntryState::Directory,
                    ),
                ],
                Observation::Complete,
            )
            .unwrap()
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}
fn owner(
    store: &mut ConnectionStore,
    record: &ConnectionRecord,
) -> crate::connection::transaction::ReviewedInitialSet {
    use crate::connection::transaction::{complete_review_for_test, initial_preview_for_test};
    let session = store.transaction_session(record.id).unwrap();
    let [source, destination] = observations(record);
    let preview =
        initial_preview_for_test(store, record.id, source.clone(), destination.clone()).unwrap();
    complete_review_for_test(store, &session, preview, source, destination).unwrap()
}
#[test]
fn whole_review_private_acceptance_preserves_exact_preimage_and_closes_sessions() {
    let (_temp, path, mut store, record) = fixture();
    let original = store.initial_review_preimage(record.id).unwrap();
    // Independently fixed canonical original record: omitted historical fields are significant.
    let wire: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert!(wire.get("initial_empty").is_none() && wire.get("directory_chain").is_none());
    assert_eq!(wire["serial"], 0);
    assert_eq!(wire["pending"], serde_json::Value::Null);
    let session = store.transaction_session(record.id).unwrap();
    let accepted = owner(&mut store, &record);
    let revision = store.accept_initial_set(accepted).unwrap().value;
    assert_eq!(revision, 2);
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    assert!(store.executor_session(record.id, &session).is_err());
    let Row::InitialChainReview(fact) =
        store.row(&format::key(7, record.id.0, 1)).unwrap().unwrap()
    else {
        panic!()
    };
    assert_eq!(fact.original, original);
    let view = store.read_review(&fact).unwrap();
    assert_eq!(view.entries.len(), 4);
    assert_eq!(view.fact.counts, [3, 3]);
    assert!(matches!(
        store.append(accepted_event(fact)),
        Err(StoreError::RecoveryRequired)
    ));
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert!(store.transaction_session(record.id).is_err());
}
#[test]
fn whole_review_finite_capacity_shared_lease_and_expiry() {
    let (_temp, _path, mut store, record) = fixture();
    let lease = store.review_read_lease().unwrap();
    let nested = store.storage.read_lease(false).unwrap();
    let before = lease.stats();
    store.load_connection(record.id).unwrap();
    let after = nested.stats();
    assert!(after.bytes > before.bytes && after.work > before.work);
    let mut limits = after;
    limits.work += 1;
    lease.limits_for_test(limits);
    assert!(matches!(
        store.load_connection(record.id),
        Err(StoreError::LimitExceeded)
    ));
    drop(nested);
    drop(lease);
    let fresh = store.review_read_lease().unwrap();
    store.load_connection(record.id).unwrap();
    fresh.expire_for_test();
    assert!(matches!(store.checkpoint(), Err(StoreError::LimitExceeded)));
    drop(fresh);
    store.load_connection(record.id).unwrap();
}

fn historical_append(store: &mut ConnectionStore, change: Change) -> Result<(), StoreError> {
    let checks = store.replay_checks(&change)?;
    let probe = super::super::chain_tests::HistoricalAppendProbe::default();
    store.append_checked_with_baseline(change, checks, BaselineAppendCheck::Historical(&probe))
}
fn outer_control(
    store: &ConnectionStore,
    id: ConnectionId,
    transition: format::InitialReviewControlTransitionV1,
) -> Change {
    let cursor = store.review_cursor(id.0, 1).unwrap();
    Change::InitialReviewControl {
        control: format::InitialReviewControlV1 {
            version: 1,
            connection: id.0,
            review: cursor.review,
            origin: 1,
            expected_progress: cursor.progress_revision,
            expected_schedule: cursor.schedule_revision,
            control_sequence: cursor.control_sequence + 1,
            current: cursor.current,
            transition,
        },
    }
}
#[test]
fn whole_review_control_exact_cut_and_fence_no_inner() {
    use format::{DirectoryChainScheduleState as S, InitialReviewControlTransitionV1 as T};
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    for reason in [format::PauseOrCancel::Pause, format::PauseOrCancel::Cancel] {
        for transition in [
            T::StopRequested { reason },
            T::StopAcknowledged {},
            T::Resume {},
        ] {
            let change = outer_control(&store, record.id, transition);
            assert!(
                matches!(
                    store.append(change.clone()),
                    Err(StoreError::RecoveryRequired)
                ),
                "inert controls never select live mode"
            );
            let result = historical_append(&mut store, change);
            assert!(
                result.is_ok(),
                "matching inert outer control must persist: {result:?}"
            );
        }
    }
    let cursor = store.review_cursor(record.id.0, 1).unwrap();
    assert_eq!((cursor.schedule_revision, cursor.control_sequence), (7, 6));
    assert_eq!(cursor.state, S::Running);
    let before = store.connection(record.id).unwrap();
    store
        .append(Change::Fence {
            connection: record.id.0,
            expected: before.revision,
            generation: 1,
        })
        .unwrap();
    let cursor = store.review_cursor(record.id.0, 1).unwrap();
    assert_eq!(
        (
            cursor.progress_revision,
            cursor.schedule_revision,
            cursor.control_sequence
        ),
        (2, 8, 6)
    );
    assert_eq!(cursor.state, S::Disconnected);
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert!(store.transaction_session(record.id).is_err());
}

fn equality_intent(store: &ConnectionStore, id: ConnectionId) -> Change {
    equality_intent_path(store, id, b"equal")
}
fn equality_intent_path(store: &ConnectionStore, id: ConnectionId, name: &[u8]) -> Change {
    use crate::connection::transaction::native::descriptor as d;
    let c = store.connection(id).unwrap();
    let review = store.review_fact(id.0, 1).unwrap();
    let view = store.read_review(&review).unwrap();
    let entry = view
        .entries
        .iter()
        .find(|entry| entry.path.to_relative().unwrap() == RelativePath::unix(name).unwrap())
        .unwrap();
    let format::InitialReviewEntrySideV1::Present { entry: original } = &entry.sides[0] else {
        panic!()
    };
    let format::State::File {
        sha256,
        bytes,
        executable,
    } = original.state.clone()
    else {
        panic!()
    };
    let file = EntryState::File {
        sha256,
        bytes,
        executable,
    };
    let path = RelativePath::unix(name).unwrap();
    let mut operation = format::Operation::new(
        OperationIntent {
            connection: id,
            expected_revision: c.revision,
            expected_baseline: c.baseline,
            path: path.clone(),
            source_before: Some(file.clone()),
            destination_before: Some(file),
            decision: Decision::VerifyEqual,
        },
        c.serial + 1,
        &c,
    )
    .unwrap();
    let object = |ino, kind| d::Object {
        dev: 1,
        ino,
        kind,
        generation: [9; 32],
    };
    let endpoints = [&c.source.identity, &c.destination.identity].map(|identity| {
        let mut bytes = [0; 64];
        bytes[..32].copy_from_slice(&identity.volume);
        bytes[32..].copy_from_slice(&identity.root);
        bytes
    });
    operation.native_identity = Some(
        d::Admission {
            connection: id.0,
            operation: c.serial + 1,
            revision: c.revision,
            fence: 0,
            baseline: c.baseline,
            endpoints,
            direction: 0,
            decision: 0,
            chains: [11, 12].map(|ino| {
                (0..path.components().len())
                    .map(|depth| object(ino + depth as u64 * 2, 0o040_000))
                    .collect()
            }),
            path,
            states: [13, 14].map(|ino| d::State::Regular {
                object: object(ino, 0o100_000),
                stamp: d::Stamp {
                    length: bytes,
                    modified: (1, 0),
                    changed: (1, 0),
                    mode: 0o100_600,
                    uid: 0,
                    gid: 0,
                    links: 1,
                },
                content: d::Content {
                    digest: sha256,
                    length: bytes,
                    executable,
                },
            }),
        }
        .encode()
        .unwrap(),
    );
    operation.native_budget = Some(format::NativeBudget::initial());
    let cursor = store.review_cursor(id.0, 1).unwrap();
    Change::ReviewedIntent {
        operation,
        link: format::InitialReviewLinkV1 {
            version: 1,
            connection: id.0,
            review: cursor.review,
            origin: 1,
            step: cursor.next_step,
            entry: entry.ordinal,
            entry_digest: format::digest(&format::encode(entry).unwrap()),
            operation: format::ChainOperationIdV1 {
                connection: id.0,
                serial: c.serial + 1,
            },
            kind: format::InitialReviewLinkKindV1::Equality {},
        },
        expected_progress: cursor.progress_revision,
        expected_schedule: cursor.schedule_revision,
        control_sequence: cursor.control_sequence,
    }
}
#[test]
fn whole_review_equality_intent_closed_wrapper_is_atomic() {
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let change = equality_intent(&store, record.id);
    assert!(matches!(
        store.append(change.clone()),
        Err(StoreError::RecoveryRequired)
    ));
    let result = historical_append(&mut store, change);
    assert!(
        result.is_ok(),
        "reviewed equality Intent must atomically link canonical and outer rows: {result:?}"
    );
    assert_eq!(
        store.executor_position(record.id).unwrap(),
        (
            Some(OperationId {
                connection: record.id,
                serial: 1
            }),
            Some(OperationId {
                connection: record.id,
                serial: 1
            })
        )
    );
    store.checkpoint().unwrap();
    drop(store);
    ConnectionStore::open_at(&path).unwrap();
}

fn equality_boundary(store: &ConnectionStore, id: ConnectionId, intent_digest: [u8; 32]) -> Change {
    let cursor = store.review_cursor(id.0, 1).unwrap();
    let format::InitialReviewCurrentV1::Equality {
        step,
        entry,
        operation,
    } = cursor.current
    else {
        panic!()
    };
    let op = store
        .operation(OperationId {
            connection: id,
            serial: operation.serial,
        })
        .unwrap();
    Change::ReviewedOrdinaryStopBoundary {
        boundary: format::InitialReviewStopBoundaryV1::EqualityUntouched {
            fact: format::EqualityUntouchedV1 {
                version: 1,
                connection: id.0,
                review: cursor.review,
                origin: 1,
                step,
                entry,
                operation: format::ChainOperationIdV1 {
                    connection: id.0,
                    serial: operation.serial,
                },
                admission_digest: format::digest(op.native_identity.as_ref().unwrap()),
                intent_digest,
                before_revision: op.revision,
                before_progress: cursor.progress_revision,
                schedule_revision: cursor.schedule_revision,
                control_sequence: cursor.control_sequence,
                fence: 0,
                stage: op.stage,
                resulting_revision: op.revision + 1,
                resulting_progress: cursor.progress_revision + 1,
            },
        },
    }
}
#[test]
fn whole_review_equality_stop_intent_verified_preserves_pending_and_saved_cut() {
    use format::InitialReviewControlTransitionV1 as T;
    for stage in [0, 3] {
        let (_temp, path, mut store, record) = fixture();
        let accepted = owner(&mut store, &record);
        store.accept_initial_set(accepted).unwrap();
        let change = equality_intent(&store, record.id);
        historical_append(&mut store, change).unwrap();
        let id = OperationId {
            connection: record.id,
            serial: 1,
        };
        let original = store.operation(id).unwrap();
        let intent_digest = format::digest(&format::encode(&original).unwrap());
        if stage == 3 {
            historical_append(
                &mut store,
                Change::Verified {
                    connection: record.id.0,
                    serial: 1,
                    expected: 3,
                    source: original.source_binding.clone(),
                    destination: original.destination_binding.clone(),
                },
            )
            .unwrap();
        }
        let control = outer_control(
            &store,
            record.id,
            T::StopRequested {
                reason: format::PauseOrCancel::Pause,
            },
        );
        historical_append(&mut store, control).unwrap();
        let before = store.operation(id).unwrap();
        let stop = equality_boundary(&store, record.id, intent_digest);
        assert!(matches!(
            store.append(stop.clone()),
            Err(StoreError::RecoveryRequired)
        ));
        historical_append(&mut store, stop).expect("closed saved-cut equality stop");
        let after = store.operation(id).unwrap();
        assert_eq!(
            (after.stage, after.serial, after.expected),
            (stage, before.serial, before.expected)
        );
        assert_eq!(after.native_identity, before.native_identity);
        assert_eq!(after.native_budget, before.native_budget);
        assert_eq!(after.native_boundary, None);
        assert_eq!(store.connection(record.id).unwrap().pending, Some(1));
        let control = outer_control(&store, record.id, T::StopAcknowledged {});
        historical_append(&mut store, control).unwrap();
        // A real canonical paid-inspection reservation advances later progress;
        // the retained stop still validates its old cut, never the new revision.
        let mut next = after.native_budget.clone().unwrap();
        next.sequence += 1;
        next.reserved += format::READ_QUANTUM;
        historical_append(
            &mut store,
            Change::ReadReserve {
                connection: record.id.0,
                serial: 1,
                expected: after.revision,
                fence: 0,
                previous: after.native_budget.unwrap(),
                next,
                recovery_only: true,
                operation_expected: Some(after.revision),
            },
        )
        .unwrap();
        let control = outer_control(&store, record.id, T::Resume {});
        historical_append(&mut store, control).unwrap();
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        assert!(
            store
                .open_recovery_context(id, store.load_connection(record.id).unwrap().revision)
                .is_err()
        );
        assert_eq!(store.operation(id).unwrap().stage, stage);
    }
}

#[test]
fn whole_review_pages_checkpoint_gc_and_missing_root_refuse() {
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    // Force real implicit checkpoint via replacement pressure, with the new
    // root only a candidate when GC walks dependencies.
    store.overlay_bytes = 8 * 1024 * 1024;
    store.accept_initial_set(accepted).unwrap();
    assert!(store.current.generation > 0);
    let fact = store.review_fact(record.id.0, 1).unwrap();
    assert!(store.page_referenced(fact.root).unwrap());
    store.collect_garbage().unwrap();
    store.read_review(&fact).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    store.read_review(&fact).unwrap();
    drop(store);
    std::fs::remove_file(path.join("pages").join(format::hex(&fact.root))).unwrap();
    assert!(
        ConnectionStore::open_at(&path).is_err(),
        "empty-log open must observe missing review pages"
    );
}
#[test]
fn whole_review_canonical_preimage_ordinals_and_full_closure_refuse_mutations() {
    for damage in 0..9 {
        let (_temp, _path, store, record) = fixture();
        let (mut review, mut entries) = review_fixture(&store, &record);
        match damage {
            0 => review.preview[0] ^= 1,
            1 => review.original.push(b' '),
            2 => {
                let mut wire: serde_json::Value = serde_json::from_slice(&review.original).unwrap();
                wire["serial"] = 1.into();
                review.original = serde_json::to_vec(&wire).unwrap();
            }
            3 => review.counts[0] -= 1,
            4 => entries[0].ordinal = 1,
            5 => {
                if let format::InitialReviewEntrySideV1::Present { entry } =
                    &mut entries[2].sides[0]
                {
                    entry.ordinal = 0;
                }
            }
            6 => review.endpoints[0].root[0] ^= 1,
            7 => {
                entries[0].path = format::NativePath::Unix(b"z".to_vec());
            }
            8 => review.version = 2,
            _ => unreachable!(),
        }
        if matches!(damage, 4 | 5 | 7) {
            // Keep page key order valid so semantic closure, not test scaffolding, refuses.
            if damage == 4 {
                assert!(entries[0].validate().is_ok());
            }
            if damage != 4 {
                review.root = storage::build_pages(
                    &store.storage,
                    entries
                        .into_iter()
                        .map(|r| Ok(Row::InitialChainReviewEntry(r))),
                )
                .unwrap()
                .unwrap();
            } else {
                review.count = 3;
            }
        }
        assert!(store.read_review(&review).is_err(), "damage={damage}");
    }
}
#[test]
fn whole_review_acceptance_stale_gate_and_store_lifetime() {
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    let session = store.transaction_session(record.id).unwrap();
    session.request_disconnect();
    assert!(matches!(
        store.accept_initial_set(accepted),
        Err(StoreError::Cancelled)
    ));
    assert!(store.owned_review(record.id.0).unwrap().is_none());
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let accepted = owner(&mut store, &record);
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert!(matches!(
        store.accept_initial_set(accepted),
        Err(StoreError::StaleRevision)
    ));
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
}
#[test]
fn whole_review_compact_control_uses_reserved_frame_capacity() {
    let (_temp, _path, store, _record) = fixture();
    let change: Change = serde_json::from_value(control_wire()).unwrap();
    let event = Event {
        schema: 1,
        predecessor: store.digest,
        change,
    };
    let bytes = format::encode(&event).unwrap();
    let framed = bytes.len() as u64 + 44;
    let length = store.log.metadata().unwrap().len();
    store.log.set_len(16 * 1024 * 1024 - framed).unwrap();
    let result = store.check_event_capacity(&event, bytes.len());
    store.log.set_len(16 * 1024 * 1024 - framed + 1).unwrap();
    let over = store.check_event_capacity(&event, bytes.len());
    store.log.set_len(length).unwrap();
    assert!(matches!(over, Err(StoreError::LimitExceeded)));
    assert!(
        result.is_ok(),
        "closed compact outer controls need the existing reserve: {result:?}"
    );
    eprintln!("whole-review compact outer control frame={framed}");
}

fn addition_intent(store: &ConnectionStore, id: ConnectionId) -> Change {
    addition_intent_at(store, id, 0)
}
fn addition_intent_at(store: &ConnectionStore, id: ConnectionId, ordinal: u32) -> Change {
    use crate::connection::transaction::native::descriptor as d;
    let Change::ReviewedIntent {
        mut operation,
        mut link,
        expected_progress,
        expected_schedule,
        control_sequence,
    } = equality_intent(store, id)
    else {
        panic!()
    };
    let view = store
        .read_review(&store.review_fact(id.0, 1).unwrap())
        .unwrap();
    let c = store.connection(id).unwrap();
    let (selected, anchor, index, predecessor, first) = if let Some(cursor) = &c.directory_chain {
        let op = store.operation(cursor.anchor.operation().unwrap()).unwrap();
        (
            super::super::chain::first_selection(&op).unwrap(),
            cursor.anchor,
            cursor.next_index,
            cursor.predecessor,
            false,
        )
    } else {
        (
            store
                .checked_review_selection(
                    &view,
                    ordinal,
                    operation.expected,
                    &store.prospective_review_rows(&[], None).unwrap(),
                    None,
                )
                .unwrap(),
            format::ChainOperationIdV1 {
                connection: id.0,
                serial: operation.serial,
            },
            0,
            None,
            true,
        )
    };
    assert_eq!(selected.preview_digest, view.fact.preview);
    let member = selected.member(index).unwrap();
    let from = usize::from(member.from == Side::Destination);
    let (state, directory) = match member.terminal {
        crate::connection::transaction::chain::DirectoryChainTerminalV1::DirectoryAdd => {
            (format::State::Directory, true)
        }
        crate::connection::transaction::chain::DirectoryChainTerminalV1::RegularAdd { content } => {
            (
                format::State::File {
                    sha256: content.sha256,
                    bytes: content.bytes,
                    executable: content.executable,
                },
                false,
            )
        }
    };
    operation.path = member.path.clone().into();
    operation.decision = if from == 0 { 1 } else { 2 };
    operation.source = (from == 0).then_some(state.clone());
    operation.destination = (from == 1).then_some(state.clone());
    if directory {
        assert_eq!(member.path, RelativePath::unix(b"a").unwrap());
        let mut bytes =
            super::super::chain_tests::directory_wire_facts(operation.baseline.is_some())[0]
                .clone();
        bytes.truncate(bytes.len() - 32);
        bytes[8..24].copy_from_slice(&id.0);
        bytes[24..32].copy_from_slice(&operation.serial.to_le_bytes());
        bytes[32..40].copy_from_slice(&operation.expected.to_le_bytes());
        bytes[49..57].copy_from_slice(&operation.baseline.unwrap_or(0).to_le_bytes());
        if from == 1 {
            bytes[185] = 1;
            bytes[186] = 2;
            let source = bytes[298..bytes.len() - 1].to_vec();
            bytes.truncate(298);
            bytes.push(0);
            bytes.extend(source);
        }
        operation.native_identity = Some(super::super::chain_tests::seal_directory(bytes));
        operation.native_kind = Some(format::NativeOperationKind::DirectoryV1);
    } else {
        let mut admission =
            d::Admission::decode(operation.native_identity.as_ref().unwrap()).unwrap();
        admission.path = member.path;
        admission.direction = from as u8;
        admission.decision = operation.decision;
        admission.states[1 - from] = d::State::Absent;
        for chain in &mut admission.chains {
            for object in chain {
                object.generation = [object.ino as u8; 32];
            }
        }
        if admission.path.components().len() == 2 {
            let object = |ino| d::Object {
                dev: 1,
                ino,
                kind: 0o040_000,
                generation: [ino as u8; 32],
            };
            admission.chains[from].push(object(13));
            admission.chains[1 - from].push(object(15));
        }
        let d::State::Regular {
            object,
            content,
            stamp,
        } = &mut admission.states[from]
        else {
            panic!()
        };
        object.ino = 21;
        object.generation = [21; 32];
        let format::State::File {
            sha256,
            bytes,
            executable,
        } = state
        else {
            panic!()
        };
        *content = d::Content {
            digest: sha256,
            length: bytes,
            executable,
        };
        stamp.length = bytes;
        operation.native_identity = Some(admission.encode().unwrap());
    }
    operation.directory_chain_anchor = first.then(|| selected.encode().unwrap());
    operation.directory_chain_ref = Some(format::DirectoryChainRefV1 {
        anchor,
        digest: selected.selection_digest,
        index,
        predecessor,
    });
    link.entry = ordinal;
    link.entry_digest = format::digest(&format::encode(&view.entries[ordinal as usize]).unwrap());
    link.kind = format::InitialReviewLinkKindV1::Chain {
        anchor,
        digest: selected.selection_digest,
        review_revision: selected.review_revision,
    };
    Change::ReviewedIntent {
        operation,
        link,
        expected_progress,
        expected_schedule,
        control_sequence,
    }
}
fn chain_control(
    store: &ConnectionStore,
    id: ConnectionId,
    transition: format::InitialReviewControlTransitionV1,
) -> Change {
    let Change::InitialReviewControl { control } = outer_control(store, id, transition) else {
        panic!()
    };
    let c = store.connection(id).unwrap();
    let cursor = c.directory_chain.unwrap();
    let schedule = c.directory_chain_schedule.unwrap();
    let inner = format::ReviewedInnerControlV1 {
        connection: id.0,
        anchor: cursor.anchor,
        digest: cursor.digest,
        expected_progress: cursor.progress_revision,
        expected_schedule: schedule.revision,
        control_sequence: schedule.control_sequence + 1,
        transition: match control.transition {
            format::InitialReviewControlTransitionV1::StopRequested { reason } => {
                format::DirectoryChainControlTransitionV1::StopRequested { reason }
            }
            format::InitialReviewControlTransitionV1::StopAcknowledged {} => {
                format::DirectoryChainControlTransitionV1::StopAcknowledged
            }
            format::InitialReviewControlTransitionV1::Resume {} => {
                format::DirectoryChainControlTransitionV1::Resume
            }
        },
    };
    Change::ReviewedChainControl { control, inner }
}
#[test]
fn whole_review_inner_controls_keep_canonical_ack_and_selection() {
    use format::InitialReviewControlTransitionV1 as T;
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = addition_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let stop = chain_control(
        &store,
        record.id,
        T::StopRequested {
            reason: format::PauseOrCancel::Cancel,
        },
    );
    historical_append(&mut store, stop).unwrap();
    let ack = chain_control(&store, record.id, T::StopAcknowledged {});
    assert!(
        historical_append(&mut store, ack).is_err(),
        "no synthetic no-inner acknowledgement for a pending addition"
    );
    historical_append(
        &mut store,
        Change::DirectoryChainStopBoundary {
            connection: record.id.0,
            serial: 1,
            expected: 3,
            expected_schedule: 2,
            fence: 0,
            kind: format::ChainStopKind::NeverBegun,
            evidence: format::DirectoryChainStopEvidenceV1::Regular { evidence: None },
        },
    )
    .unwrap();
    let ack = chain_control(&store, record.id, T::StopAcknowledged {});
    historical_append(&mut store, ack).unwrap();
    let resume = chain_control(&store, record.id, T::Resume {});
    historical_append(&mut store, resume).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    ConnectionStore::open_at(&path).unwrap();
}

#[test]
fn whole_review_original_format_bytes_are_not_public_record_projection() {
    let (_temp, _path, store, record) = fixture();
    let octets = |value: u8, count: usize| serde_json::to_string(&vec![value; count]).unwrap();
    let expected = format!(
        concat!(
            r#"{{"id":{},"revision":1,"source":{{"identity":{{"volume":{},"root":{}}},"locator":{{"encoding":"Unix","units":[47,115,111,117,114,99,101]}}}},"destination":{{"identity":{{"volume":{},"root":{}}},"locator":{{"encoding":"Unix","units":[47,100,101,115,116,105,110,97,116,105,111,110]}}}},"retention":2592000,"lifecycle":0,"baseline":null,"baseline_root":null,"baseline_count":0,"fence":0,"serial":0,"pending":null}}"#
        ),
        octets(33, 16),
        octets(1, 32),
        octets(2, 32),
        octets(3, 32),
        octets(4, 32)
    );
    assert_eq!(
        store.initial_review_preimage(record.id).unwrap(),
        expected.as_bytes()
    );
    let (mut fact, _) = review_fixture(&store, &record);
    let mut original = store.connection(record.id).unwrap();
    original.serial = 8;
    fact.original = format::encode(&original).unwrap();
    assert!(
        fact.original_record().is_err(),
        "nondefault serial cannot be zeroed to manufacture a fresh cut"
    );
    original.serial = 0;
    original.pending = Some(1);
    fact.original = format::encode(&original).unwrap();
    assert!(fact.original_record().is_err());
    original.pending = None;
    original.baseline = Some(1);
    original.baseline_count = 1;
    original.baseline_root = Some([9; 32]);
    fact.original = format::encode(&original).unwrap();
    assert!(fact.original_record().is_err());
}
#[test]
fn whole_review_nested_lease_cannot_reset_when_original_owner_drops() {
    let (_temp, _path, store, record) = fixture();
    let first = store.review_read_lease().unwrap();
    let nested = store.review_read_lease().unwrap();
    drop(first);
    let before = nested.stats();
    store.load_connection(record.id).unwrap();
    assert!(nested.stats().bytes > before.bytes);
    nested.expire_for_test();
    assert!(matches!(
        store.storage.read_lease(false),
        Err(StoreError::LimitExceeded)
    ));
}
#[test]
fn whole_review_fault_cuts_never_publish_partial_cursor_or_reissue_consent() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            super::super::tests::FAULT.with(|f| f.set(None));
        }
    }
    let mut refused = 0;
    let mut durable_after_error = 0;
    for point in 0..28 {
        let (_temp, path, mut store, record) = fixture();
        let accepted = owner(&mut store, &record);
        store.overlay_bytes = 8 * 1024 * 1024; // Include the real implicit checkpoint and candidate-root cuts.
        let reset = Reset;
        super::super::tests::FAULT.with(|f| f.set(Some((point, false))));
        let result = store.accept_initial_set(accepted);
        let failed = result.is_err();
        refused += usize::from(failed);
        drop(reset);
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        match store.owned_review(record.id.0).unwrap() {
            Some(fact) => {
                durable_after_error += usize::from(failed);
                store.read_review(&fact).unwrap();
                assert_eq!(
                    store
                        .review_cursor(record.id.0, 1)
                        .unwrap()
                        .progress_revision,
                    2
                );
                assert!(store.transaction_session(record.id).is_err());
            }
            None => {
                assert!(
                    store
                        .row(&format::key(9, record.id.0, 1))
                        .unwrap()
                        .is_none()
                );
                assert_eq!(store.load_connection(record.id).unwrap().revision, 1);
            }
        }
        assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    }
    assert!(
        refused >= 4,
        "page and log durability cuts must actually fire"
    );
    assert!(
        durable_after_error > 0,
        "post-fsync error must be recovered as exact durable consent"
    );
    eprintln!(
        "whole-review fault cuts refused={refused}, durable-after-error={durable_after_error}"
    );
}
#[test]
fn whole_review_stale_control_link_and_equality_stop_refuse_without_rows() {
    use format::InitialReviewControlTransitionV1 as T;
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let initial = equality_intent(&store, record.id);
    for damage in 0..6 {
        let mut bad = initial.clone();
        let Change::ReviewedIntent {
            ref mut link,
            ref mut expected_progress,
            ref mut expected_schedule,
            ref mut control_sequence,
            ..
        } = bad
        else {
            panic!()
        };
        match damage {
            0 => link.review[0] ^= 1,
            1 => link.entry = 0,
            2 => link.entry_digest[0] ^= 1,
            3 => *expected_progress += 1,
            4 => *expected_schedule += 1,
            5 => *control_sequence += 1,
            _ => unreachable!(),
        }
        let before = store.sequence;
        assert!(historical_append(&mut store, bad).is_err());
        assert_eq!(store.sequence, before);
    }
    historical_append(&mut store, initial).unwrap();
    let op = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let digest = format::digest(&format::encode(&op).unwrap());
    let control = outer_control(
        &store,
        record.id,
        T::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    historical_append(&mut store, control).unwrap();
    let boundary = equality_boundary(&store, record.id, digest);
    for damage in 0..8 {
        let mut bad = boundary.clone();
        let Change::ReviewedOrdinaryStopBoundary {
            boundary: format::InitialReviewStopBoundaryV1::EqualityUntouched { ref mut fact },
        } = bad
        else {
            panic!()
        };
        match damage {
            0 => fact.review[0] ^= 1,
            1 => fact.operation.serial = 2,
            2 => fact.before_revision += 1,
            3 => fact.schedule_revision += 1,
            4 => fact.control_sequence += 1,
            5 => fact.stage = 7,
            6 => fact.intent_digest[0] ^= 1,
            7 => fact.admission_digest[0] ^= 1,
            _ => unreachable!(),
        }
        let before = store.sequence;
        assert!(
            historical_append(&mut store, bad).is_err(),
            "damage={damage}"
        );
        assert_eq!(store.sequence, before);
        assert_eq!(store.connection(record.id).unwrap().pending, Some(1));
    }
}
#[cfg(target_os = "linux")]
#[test]
fn whole_review_native_fresh_acceptance_reads_actual_trees() {
    use crate::connection::{endpoint, transaction::initial_preview_for_test};
    let root = std::env::var_os("QOS_ENDPOINT_FIXTURE")
        .expect("requires root-supplied real private ext4/Btrfs fixture");
    let temp = tempfile::tempdir_in(root).unwrap();
    let base = temp.path().canonicalize().unwrap();
    let paths = [base.join("source"), base.join("destination")];
    for path in &paths {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(paths[0].join("a"), b"approved source").unwrap();
    std::fs::write(paths[1].join("b"), b"approved destination").unwrap();
    let probes = paths
        .each_ref()
        .map(|path| endpoint::probe_endpoint(path).unwrap());
    let mut context = endpoint::EndpointContext::default();
    for (path, probe) in paths.iter().zip(&probes) {
        context
            .register(EndpointLocator::new(path).unwrap(), probe.identity)
            .unwrap();
    }
    let pins = probes.each_ref().map(|probe| {
        context
            .open_endpoint(&probe.identity, endpoint::Access::Read)
            .unwrap()
    });
    let mut store = ConnectionStore::open_at(&base.join("store")).unwrap();
    let record = store
        .create_connection(
            ConnectionId([88; 16]),
            EndpointBinding {
                identity: probes[0].identity,
                locator: EndpointLocator::new(&paths[0]).unwrap(),
            },
            EndpointBinding {
                identity: probes[1].identity,
                locator: EndpointLocator::new(&paths[1]).unwrap(),
            },
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap()
        .value;
    let session = store.transaction_session(record.id).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let preview = initial_preview_for_test(
        &store,
        record.id,
        endpoint::observe(&pins[0], &cancel),
        endpoint::observe(&pins[1], &cancel),
    )
    .unwrap();
    std::fs::write(paths[0].join("a"), b"changed source").unwrap();
    assert!(
        preview
            .accept_complete(&store, &session, &pins[0], &pins[1], &cancel)
            .is_err()
    );
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    let preview = initial_preview_for_test(
        &store,
        record.id,
        endpoint::observe(&pins[0], &cancel),
        endpoint::observe(&pins[1], &cancel),
    )
    .unwrap();
    let owner = preview
        .accept_complete(&store, &session, &pins[0], &pins[1], &cancel)
        .unwrap();
    store.accept_initial_set(owner).unwrap();
    assert_eq!(
        std::fs::read(paths[0].join("a")).unwrap(),
        b"changed source"
    );
    assert_eq!(
        std::fs::read(paths[1].join("b")).unwrap(),
        b"approved destination"
    );
    assert!(!paths[0].join("b").exists() && !paths[1].join("a").exists());
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&base.join("store")).unwrap();
    assert!(store.transaction_session(record.id).is_err());
}

#[test]
fn whole_review_consent_cannot_inherit_larger_inert_lease() {
    let (_temp, _path, store, _record) = fixture();
    let inert = store.storage.read_lease(false).unwrap();
    store
        .storage
        .review_charge(512 * 1024 * 1024 + 1, 0, 0, 0)
        .unwrap();
    assert!(
        matches!(store.review_read_lease(), Err(StoreError::LimitExceeded)),
        "consent cannot inherit the larger replay allowance or reset used bytes"
    );
    drop(inert);
    store.review_read_lease().unwrap();
}

fn owner_for_trees(
    store: &mut ConnectionStore,
    record: &ConnectionRecord,
    trees: [crate::connection::model::ObservedTree; 2],
) -> Result<crate::connection::transaction::ReviewedInitialSet, StoreError> {
    use crate::connection::transaction::{complete_review_for_test, initial_preview_for_test};
    let session = store.transaction_session(record.id)?;
    let [source, destination] = trees;
    let preview = initial_preview_for_test(store, record.id, source.clone(), destination.clone())?;
    complete_review_for_test(store, &session, preview, source, destination)
}
#[test]
fn whole_review_union_count_boundary_preserves_every_entry() {
    use crate::connection::model::{Observation, ObservedTree};
    for count in [4096, 4097] {
        let (_temp, path, mut store, record) = fixture();
        let [source, destination] = observations(&record);
        let entries = (0..4096).map(|n| {
            (
                RelativePath::unix(format!("entry-{n:04}").as_bytes()).unwrap(),
                EntryState::Directory,
            )
        });
        let source =
            ObservedTree::new(source.endpoint().clone(), entries, Observation::Complete).unwrap();
        let destination = ObservedTree::new(
            destination.endpoint().clone(),
            if count == 4097 {
                vec![(RelativePath::unix(b"extra").unwrap(), EntryState::Directory)]
            } else {
                vec![]
            },
            Observation::Complete,
        )
        .unwrap();
        let owner = owner_for_trees(&mut store, &record, [source, destination]);
        if count == 4097 {
            assert!(matches!(owner, Err(StoreError::LimitExceeded)));
            assert!(store.owned_review(record.id.0).unwrap().is_none());
        } else {
            let lease = store.review_read_lease().unwrap();
            store.accept_initial_set(owner.unwrap()).unwrap();
            let stats = lease.stats();
            eprintln!(
                "whole-review 4096 acceptance bytes={} pages={} rows={} work={}",
                stats.bytes, stats.pages, stats.rows, stats.work
            );
            drop(lease);
            store.checkpoint().unwrap();
            drop(store);
            let store = ConnectionStore::open_at(&path).unwrap();
            let fact = store.review_fact(record.id.0, 1).unwrap();
            assert_eq!(fact.count, 4096);
            assert_eq!(store.read_review(&fact).unwrap().entries.len(), 4096);
        }
    }
}
fn canonical_policy(last: usize) -> format::ReviewNamePolicyV1 {
    format::ReviewNamePolicyV1::Canonical {
        keys: (0..8)
            .map(|n| format::ReviewNameKeyV1 {
                path: format::NativePath::Unix(format!("key-{n}").into_bytes()),
                components: vec![vec![255; if n == 7 { last } else { 4096 }]],
            })
            .collect(),
    }
}
#[test]
fn whole_review_canonical_policy_actual_encoding_boundary_is_visible() {
    use crate::connection::model::{NamePolicy, Observation, ObservedTree};
    let (_temp, path, mut store, record) = fixture();
    let (mut fact, _) = review_fixture(&store, &record);
    fact.root = [255; 32];
    let fits = |last| {
        let mut fact = fact.clone();
        fact.policies[0] = canonical_policy(last);
        let mut cursor = inert_cursor(record.id);
        cursor.review = [255; 32];
        let event = Change::InitialReviewAccepted {
            review: fact.clone(),
            cursor,
        };
        format::encode(&Event {
            schema: 1,
            predecessor: store.digest,
            change: event,
        })
        .is_ok()
            && format::encode(&format::PageNode {
                schema: 1,
                node: format::Node::Leaf(vec![Row::InitialChainReview(fact)]),
            })
            .is_ok()
    };
    assert!(fits(1));
    assert!(!fits(4096));
    let mut low = 1;
    let mut high = 4096;
    while high - low > 1 {
        let mid = (low + high) / 2;
        if fits(mid) { low = mid } else { high = mid }
    }
    assert_eq!(high, low + 1);
    let trees = |last| {
        let [source, destination] = observations(&record);
        let mut endpoint = source.endpoint().clone();
        let format::ReviewNamePolicyV1::Canonical { keys } = canonical_policy(last) else {
            panic!()
        };
        endpoint.names = NamePolicy::Canonical(
            keys.into_iter()
                .map(|key| (key.path.to_relative().unwrap(), key.components))
                .collect(),
        );
        [
            ObservedTree::new(endpoint, source.entries().clone(), Observation::Complete).unwrap(),
            destination,
        ]
    };
    let too_large = owner_for_trees(&mut store, &record, trees(high)).unwrap();
    assert!(matches!(
        store.accept_initial_set(too_large),
        Err(StoreError::LimitExceeded)
    ));
    assert!(store.owned_review(record.id.0).unwrap().is_none());
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    let accepted = owner_for_trees(&mut store, &record, trees(low)).unwrap();
    store.accept_initial_set(accepted).unwrap();
    let fact = store.review_fact(record.id.0, 1).unwrap();
    assert_eq!(fact.policies[0], canonical_policy(low));
    let event = Event {
        schema: 1,
        predecessor: store.digest,
        change: accepted_event(fact.clone()),
    };
    eprintln!(
        "whole-review Canonical last-key boundary={low}, refused={}, actual-event-bytes={}",
        high,
        format::encode(&event).unwrap().len()
    );
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.review_fact(record.id.0, 1).unwrap().policies[0],
        canonical_policy(low)
    );
}
#[test]
fn whole_review_maximum_native_row_and_aggregate_bytes_are_bounded() {
    use crate::connection::model::{Observation, ObservedTree};
    let path: Vec<u16> = (0..17)
        .flat_map(|n| {
            let mut part = vec![u16::MAX; 240];
            if n != 16 {
                part.push(92)
            }
            part
        })
        .collect();
    assert_eq!(path.len(), 4096);
    RelativePath::windows(&path).unwrap();
    let row = Row::InitialChainReviewEntry(format::InitialChainReviewEntryV1 {
        version: 1,
        connection: [255; 16],
        origin: u64::MAX,
        ordinal: 4095,
        path: format::NativePath::Windows(path),
        sides: std::array::from_fn(|_| format::InitialReviewEntrySideV1::Present {
            entry: format::InitialReviewSideV1 {
                ordinal: 4095,
                state: format::State::File {
                    sha256: [255; 32],
                    bytes: u64::MAX,
                    executable: true,
                },
            },
        }),
    });
    row.validate().unwrap();
    let size = format::encode(&row).unwrap().len();
    assert!(size < format::MAX_BYTES);
    format::encode(&format::PageNode {
        schema: 1,
        node: format::Node::Leaf(vec![row]),
    })
    .unwrap();
    eprintln!("whole-review maximum Windows path + both file states row-bytes={size}");
    let (_temp, _path, mut store, record) = fixture();
    let [source, destination] = observations(&record);
    let mut parent = Vec::new();
    let mut entries = Vec::new();
    for _ in 0..16 {
        if !parent.is_empty() {
            parent.push(b'/')
        }
        parent.extend([255; 240]);
        entries.push((RelativePath::unix(&parent).unwrap(), EntryState::Directory));
    }
    for n in 0..4080u32 {
        let mut leaf = parent.clone();
        leaf.push(b'/');
        leaf.extend([255; 234]);
        leaf.extend(format!("{n:06}").bytes());
        entries.push((RelativePath::unix(&leaf).unwrap(), EntryState::Directory));
    }
    assert_eq!(entries.len(), 4096);
    let source =
        ObservedTree::new(source.endpoint().clone(), entries, Observation::Complete).unwrap();
    let destination =
        ObservedTree::new(destination.endpoint().clone(), [], Observation::Complete).unwrap();
    let owner = owner_for_trees(&mut store, &record, [source, destination]).unwrap();
    assert!(
        matches!(
            store.accept_initial_set(owner),
            Err(StoreError::LimitExceeded)
        ),
        "4096 paths can exceed 32 MiB of encoded rows"
    );
    assert!(store.owned_review(record.id.0).unwrap().is_none());
}
#[test]
fn whole_review_equality_fence_terminal_accounting_freezes_progress() {
    use format::InitialReviewControlTransitionV1 as T;
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let id = OperationId {
        connection: record.id,
        serial: 1,
    };
    let digest = format::digest(&format::encode(&store.operation(id).unwrap()).unwrap());
    let stop = outer_control(
        &store,
        record.id,
        T::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    historical_append(&mut store, stop).unwrap();
    let boundary = equality_boundary(&store, record.id, digest);
    historical_append(&mut store, boundary).unwrap();
    let ack = outer_control(&store, record.id, T::StopAcknowledged {});
    historical_append(&mut store, ack).unwrap();
    let before = store.review_cursor(record.id.0, 1).unwrap();
    let c = store.connection(record.id).unwrap();
    store
        .append(Change::Fence {
            connection: record.id.0,
            expected: c.revision,
            generation: 1,
        })
        .unwrap();
    let c = store.connection(record.id).unwrap();
    let op = store.operation(id).unwrap();
    // Inert canonical terminal accounting; this is not a live untouched proof.
    historical_append(
        &mut store,
        Change::NativeBoundary {
            connection: record.id.0,
            serial: 1,
            expected: c.revision,
            operation_expected: op.revision,
            fence: c.fence,
            kind: 0,
            evidence: None,
        },
    )
    .unwrap();
    assert_eq!(store.operation(id).unwrap().stage, 7);
    assert_eq!(store.connection(record.id).unwrap().pending, None);
    let after = store.review_cursor(record.id.0, 1).unwrap();
    assert_eq!(after.progress_revision, before.progress_revision);
    assert_eq!(after.control_sequence, before.control_sequence);
    assert_eq!(after.schedule_revision, before.schedule_revision + 1);
    assert_eq!(
        after.state,
        format::DirectoryChainScheduleState::Disconnected
    );
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert!(store.transaction_session(record.id).is_err());
}
#[test]
fn whole_review_each_meter_and_checkpoint_repeat_share_finite_context() {
    use storage::ReviewReadStats;
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let review = store.review_fact(record.id.0, 1).unwrap();
    for dimension in 0..4 {
        let lease = store.review_read_lease().unwrap();
        let mut limits = ReviewReadStats {
            bytes: u64::MAX,
            pages: u64::MAX,
            rows: u64::MAX,
            work: u64::MAX,
        };
        match dimension {
            0 => limits.bytes = 0,
            1 => limits.pages = 0,
            2 => limits.rows = 0,
            3 => limits.work = 0,
            _ => unreachable!(),
        }
        lease.limits_for_test(limits);
        assert!(
            matches!(store.read_review(&review), Err(StoreError::LimitExceeded)),
            "dimension={dimension}"
        );
    }
    let lease = store.review_read_lease().unwrap();
    store.checkpoint().unwrap();
    let first = lease.stats();
    store.checkpoint().unwrap();
    let second = lease.stats();
    assert!(
        second.bytes > first.bytes
            && second.pages > first.pages
            && second.rows > first.rows
            && second.work > first.work
    );
    eprintln!(
        "whole-review two-checkpoint bytes={} pages={} rows={} work={}",
        second.bytes, second.pages, second.rows, second.work
    );
    lease.limits_for_test(second);
    assert!(matches!(store.checkpoint(), Err(StoreError::LimitExceeded)));
}
#[test]
fn whole_review_schedule_exhaustion_leaves_fence_headroom() {
    use format::{DirectoryChainScheduleState as S, InitialReviewControlTransitionV1 as T};
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    // Inert counter-boundary fixture; no resumed worker or native owner exists.
    let mut cursor = store.review_cursor(record.id.0, 1).unwrap();
    cursor.schedule_revision = u64::MAX - 1;
    cursor.control_sequence = u64::MAX - 2;
    let row = Row::InitialReviewCursor(cursor);
    store.overlay.insert(row.key(), row);
    store.validate_review_history(&[]).unwrap();
    let control = outer_control(
        &store,
        record.id,
        T::StopRequested {
            reason: format::PauseOrCancel::Cancel,
        },
    );
    assert!(historical_append(&mut store, control).is_err());
    let c = store.connection(record.id).unwrap();
    store
        .append(Change::Fence {
            connection: record.id.0,
            expected: c.revision,
            generation: 1,
        })
        .unwrap();
    let cursor = store.review_cursor(record.id.0, 1).unwrap();
    assert_eq!(cursor.schedule_revision, u64::MAX);
    assert_eq!(cursor.state, S::Disconnected);
}
#[test]
fn whole_review_unregistered_gate_and_expired_owner_refuse_publication() {
    use crate::connection::transaction::{
        AdmissionGate, TransactionSession, complete_review_for_test, initial_preview_for_test,
    };
    let (_temp, _path, mut store, record) = fixture();
    let _registered = store.transaction_session(record.id).unwrap();
    let forged = TransactionSession::new(
        record.id,
        Arc::downgrade(&store.native_lifetime),
        Arc::new(AdmissionGate::new()),
    );
    let [source, destination] = observations(&record);
    let preview =
        initial_preview_for_test(&store, record.id, source.clone(), destination.clone()).unwrap();
    assert!(matches!(
        complete_review_for_test(&store, &forged, preview, source, destination),
        Err(StoreError::Cancelled)
    ));
    let accepted = owner(&mut store, &record);
    let lease = store.review_read_lease().unwrap();
    lease.expire_for_test();
    assert!(matches!(
        store.accept_initial_set(accepted),
        Err(StoreError::LimitExceeded)
    ));
    drop(lease);
    assert!(store.owned_review(record.id.0).unwrap().is_none());
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
}

#[test]
fn whole_review_gc_digest_reads_are_charged_before_io() {
    let (_temp, _path, mut store, record) = fixture();
    let (orphan, _) = review_fixture(&store, &record);
    let lease = store.review_read_lease().unwrap();
    let before = lease.stats();
    assert_eq!(store.collect_garbage().unwrap(), 1);
    assert!(store.storage.page(orphan.root).is_err());
    let used = lease.stats().bytes - before.bytes;
    assert!(
        used >= 1024 * 1024,
        "two small-file digests each request a 256 KiB chunk and EOF chunk, used={used}"
    );
}

#[test]
fn whole_review_empty_log_closure_joins_equality_admission_to_original_endpoints() {
    use crate::connection::transaction::native::descriptor as d;
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let original = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    for damage in 0..3 {
        let mut op = original.clone();
        let mut admission = d::Admission::decode(op.native_identity.as_deref().unwrap()).unwrap();
        match damage {
            0 => {
                op.source_binding.root[0] ^= 1;
                admission.endpoints[0][32] ^= 1
            }
            1 => {
                op.destination_binding.root[0] ^= 1;
                admission.endpoints[1][32] ^= 1
            }
            2 => admission.fence = 1,
            _ => unreachable!(),
        }
        op.native_identity = Some(admission.encode().unwrap());
        op.decode().unwrap();
        assert!(
            store
                .validate_review_history(&[Row::Operation(op)])
                .is_err(),
            "self-consistent operation-local facts are not the original review, damage={damage}"
        );
    }
}

#[test]
fn whole_review_outer_control_exact_tuple_and_closed_states_refuse_mutations() {
    use format::InitialReviewControlTransitionV1 as T;
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let control = outer_control(
        &store,
        record.id,
        T::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    for damage in 0..7 {
        let mut bad = control.clone();
        let Change::InitialReviewControl { ref mut control } = bad else {
            panic!()
        };
        match damage {
            0 => control.review[0] ^= 1,
            1 => control.origin += 1,
            2 => control.expected_progress += 1,
            3 => control.expected_schedule += 1,
            4 => control.control_sequence += 1,
            5 => {
                control.current = format::InitialReviewCurrentV1::Equality {
                    step: 0,
                    entry: 2,
                    operation: format::ChainOperationIdV1 {
                        connection: record.id.0,
                        serial: 1,
                    },
                }
            }
            6 => control.transition = T::Resume {},
            _ => unreachable!(),
        }
        let sequence = store.sequence;
        assert!(
            historical_append(&mut store, bad).is_err(),
            "damage={damage}"
        );
        assert_eq!(store.sequence, sequence);
    }
    let review = store.review_fact(record.id.0, 1).unwrap();
    let mut entries = store.read_review(&review).unwrap().entries;
    entries[0].sides[0] = format::InitialReviewEntrySideV1::Present {
        entry: format::InitialReviewSideV1 {
            ordinal: 0,
            state: format::State::Directory,
        },
    };
    let mut wire = serde_json::to_value(&entries[0]).unwrap();
    wire["sides"][0]["entry"]["state"]["invented"] = true.into();
    assert!(serde_json::from_value::<format::InitialChainReviewEntryV1>(wire).is_err());
    let mut c = store.connection(record.id).unwrap();
    c.source.locator = format::NativePath::Unix(b"/source-x".to_vec());
    c.decode().unwrap();
    assert!(
        store
            .validate_review_history(&[Row::Connection(c)])
            .is_err()
    );
    assert_eq!(store.read_review(&review).unwrap().entries.len(), 4);
}

#[test]
fn whole_review_no_inner_retained_ownership_denies_all_unscoped_entry_points() {
    let (_temp, _path, mut store, record) = fixture();
    let cached = store.transaction_session(record.id).unwrap();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    for cut in 0..3 {
        if cut == 1 {
            let intent = equality_intent(&store, record.id);
            historical_append(&mut store, intent).unwrap();
        }
        if cut == 2 {
            // Guard-only retained equality completion fixture: slice 2 supplies
            // the missing canonical baseline/archive reducer. This is deliberately
            // not accepted as complete replayable review history in slice 1.
            let mut c = store.connection(record.id).unwrap();
            c.pending = None;
            c.baseline = Some(5);
            c.revision = 5;
            let row = Row::Connection(c);
            store.overlay.insert(row.key(), row);
            assert!(store.validate_review_history(&[]).is_err());
        }
        let c = store.connection(record.id).unwrap();
        assert!(c.directory_chain.is_none() && c.directory_chain_schedule.is_none());
        let intent = OperationIntent {
            connection: record.id,
            expected_revision: c.revision,
            expected_baseline: c.baseline,
            path: RelativePath::unix(b"a").unwrap(),
            source_before: Some(EntryState::Directory),
            destination_before: None,
            decision: Decision::Propagate { from: Side::Source },
        };
        assert!(matches!(
            store.transaction_session(record.id),
            Err(StoreError::RecoveryRequired)
        ));
        assert!(matches!(
            store.executor_session(record.id, &cached),
            Err(StoreError::RecoveryRequired)
        ));
        assert!(matches!(
            store.begin_intent(intent.clone()),
            Err(StoreError::RecoveryRequired)
        ));
        assert!(matches!(
            store.open_recovery_context(
                OperationId {
                    connection: record.id,
                    serial: 1
                },
                c.revision
            ),
            Err(StoreError::RecoveryRequired)
        ));
        let [source, destination] = observations(&record);
        assert!(matches!(
            store.initial_review_basis(record.id, &source, &destination),
            Err(StoreError::RecoveryRequired)
        ));
        #[cfg(target_os = "linux")]
        assert!(matches!(
            store.native_admission_basis(&intent),
            Err(StoreError::RecoveryRequired)
        ));
    }
}

#[test]
fn whole_review_fix1_consent_cannot_be_retargeted_after_acceptance() {
    use crate::connection::model::{NamePolicy, Observation, ObservedTree};
    for policy_only in [false, true] {
        let (_temp, path, mut store, record) = fixture();
        let accepted = owner(&mut store, &record);
        let original_preview = accepted.preview();
        let [source, destination] = accepted.trees().map(Clone::clone);
        let mut endpoint = source.endpoint().clone();
        let mut entries = source.entries().clone();
        if policy_only {
            endpoint.names = NamePolicy::Canonical(BTreeMap::from([(
                RelativePath::unix(b"unapproved").unwrap(),
                vec![b"unapproved".to_vec()],
            )]));
        } else {
            entries.insert(
                RelativePath::unix(b"unapproved").unwrap(),
                EntryState::Directory,
            );
        }
        let substitute = ObservedTree::new(endpoint, entries, Observation::Complete).unwrap();
        let substituted_preview = store
            .initial_review_basis(record.id, &substitute, &destination)
            .unwrap()
            .2;
        // Callers can mutate copies from immutable access, never the accepted owner.
        // A replacement fresh comparison must independently match its explicit preview.
        let preview = crate::connection::transaction::initial_preview_for_test(
            &store,
            record.id,
            source,
            destination.clone(),
        )
        .unwrap();
        let session = store.transaction_session(record.id).unwrap();
        assert!(matches!(
            crate::connection::transaction::complete_review_for_test(
                &store,
                &session,
                preview,
                substitute,
                destination
            ),
            Err(StoreError::StaleRevision)
        ));
        assert_eq!(
            original_preview == substituted_preview,
            policy_only,
            "legacy preview digest excludes policy, so immutable ownership must preserve it too"
        );
        store.accept_initial_set(accepted).unwrap();
        let review = store.review_fact(record.id.0, 1).unwrap();
        assert_eq!(
            review.preview, original_preview,
            "publication must retain the comparison actually accepted"
        );
        assert_eq!(review.policies[0], format::ReviewNamePolicyV1::Exact {});
        assert_eq!(store.read_review(&review).unwrap().entries.len(), 4);
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(store.review_fact(record.id.0, 1).unwrap(), review);
    }
}

fn fix1_equality_stop(store: &mut ConnectionStore, id: ConnectionId, stage: u8) {
    use format::InitialReviewControlTransitionV1 as T;
    let intent = equality_intent(store, id);
    historical_append(store, intent).unwrap();
    let op = store
        .operation(OperationId {
            connection: id,
            serial: 1,
        })
        .unwrap();
    let digest = format::digest(&format::encode(&op).unwrap());
    if stage == 3 {
        historical_append(
            store,
            Change::Verified {
                connection: id.0,
                serial: 1,
                expected: op.revision,
                source: op.source_binding,
                destination: op.destination_binding,
            },
        )
        .unwrap();
    }
    let stop = outer_control(
        store,
        id,
        T::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    historical_append(store, stop).unwrap();
    let boundary = equality_boundary(store, id, digest);
    historical_append(store, boundary).unwrap();
}
#[test]
fn whole_review_fix1_saved_verified_cut_cannot_precede_current_intent() {
    for advance in [false, true] {
        let (_temp, path, mut store, record) = fixture();
        let accepted = owner(&mut store, &record);
        store.accept_initial_set(accepted).unwrap();
        fix1_equality_stop(&mut store, record.id, 0);
        if advance {
            use format::InitialReviewControlTransitionV1 as T;
            let ack = outer_control(&store, record.id, T::StopAcknowledged {});
            historical_append(&mut store, ack).unwrap();
            let resume = outer_control(&store, record.id, T::Resume {});
            historical_append(&mut store, resume).unwrap();
            let op = store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1,
                })
                .unwrap();
            historical_append(
                &mut store,
                Change::Verified {
                    connection: record.id.0,
                    serial: 1,
                    expected: op.revision,
                    source: op.source_binding,
                    destination: op.destination_binding,
                },
            )
            .unwrap();
        }
        store.checkpoint().unwrap();
        let key = format::key(11, record.id.0, 1);
        let mut bad = store.row(&key).unwrap().unwrap();
        let Row::InitialReviewStop(format::InitialReviewStopBoundaryV1::EqualityUntouched {
            ref mut fact,
        }) = bad
        else {
            panic!()
        };
        fact.stage = 3;
        bad.validate().unwrap();
        assert!(
            matches!(
                store.validate_review_history(&[bad.clone()]),
                Err(StoreError::Corrupt)
            ),
            "a Verified saved cut cannot be projected from a still-Intent operation"
        );
        store.overlay.insert(key.clone(), bad.clone());
        assert!(matches!(store.checkpoint(), Err(StoreError::Corrupt)));
        drop(store);
        store = ConnectionStore::open_at(&path).unwrap();
        // Install a well-framed/hash-valid checkpoint with only the saved stage changed,
        // bypassing the rejecting checkpoint reducer to exercise empty-log open itself.
        let mut rows = store
            .rows()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        *rows.iter_mut().find(|row| row.key() == key).unwrap() = bad;
        let mut current = store.current.clone();
        current.root = storage::build_pages(&store.storage, rows.into_iter().map(Ok)).unwrap();
        store
            .publish_current(&current, &store.log, &store.log_identity)
            .unwrap();
        drop(store);
        assert!(matches!(
            ConnectionStore::open_at(&path),
            Err(StoreError::Corrupt)
        ));
    }
}
#[test]
fn whole_review_fix1_saved_cut_allows_verified_progress_and_fence_terminals() {
    use format::InitialReviewControlTransitionV1 as T;
    for (saved_stage, advance) in [(0, false), (3, false), (0, true)] {
        let (_temp, path, mut store, record) = fixture();
        let accepted = owner(&mut store, &record);
        store.accept_initial_set(accepted).unwrap();
        fix1_equality_stop(&mut store, record.id, saved_stage);
        let ack = outer_control(&store, record.id, T::StopAcknowledged {});
        historical_append(&mut store, ack).unwrap();
        let id = OperationId {
            connection: record.id,
            serial: 1,
        };
        if advance {
            let resume = outer_control(&store, record.id, T::Resume {});
            historical_append(&mut store, resume).unwrap();
            let op = store.operation(id).unwrap();
            historical_append(
                &mut store,
                Change::Verified {
                    connection: record.id.0,
                    serial: 1,
                    expected: op.revision,
                    source: op.source_binding,
                    destination: op.destination_binding,
                },
            )
            .unwrap();
            assert_eq!(store.operation(id).unwrap().stage, 3);
            store.checkpoint().unwrap();
            drop(store);
            store = ConnectionStore::open_at(&path).unwrap();
        }
        let c = store.connection(record.id).unwrap();
        store
            .append(Change::Fence {
                connection: record.id.0,
                expected: c.revision,
                generation: 1,
            })
            .unwrap();
        let c = store.connection(record.id).unwrap();
        let op = store.operation(id).unwrap();
        historical_append(
            &mut store,
            Change::NativeBoundary {
                connection: record.id.0,
                serial: 1,
                expected: c.revision,
                operation_expected: op.revision,
                fence: c.fence,
                kind: 0,
                evidence: None,
            },
        )
        .unwrap();
        assert_eq!(store.operation(id).unwrap().stage, 7);
        store.checkpoint().unwrap();
        drop(store);
        ConnectionStore::open_at(&path).unwrap();
    }
}

struct Fix1ShortLog<'a> {
    input: std::io::Cursor<Vec<u8>>,
    requests: Vec<usize>,
    interrupt: Option<usize>,
    expire: Option<&'a ReviewReadLease>,
}
impl Fix1ShortLog<'_> {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            input: std::io::Cursor::new(bytes),
            requests: Vec::new(),
            interrupt: None,
            expire: None,
        }
    }
}
impl std::io::Read for Fix1ShortLog<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.requests.push(buf.len());
        if self.interrupt == Some(self.requests.len()) {
            self.interrupt = None;
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if let Some(lease) = self.expire.take() {
            lease.expire_for_test();
        }
        let count = buf.len().min(1);
        self.input.read(&mut buf[..count])
    }
}
fn fix1_frame(value: &serde_json::Value) -> Vec<u8> {
    fix1_frame_at(value, 1)
}
fn fix1_frame_at(value: &serde_json::Value, mut sequence: u64) -> Vec<u8> {
    let mut file = tempfile::tempfile().unwrap();
    framing::write_record(&mut file, &mut sequence, value).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}
#[test]
fn whole_review_fix1_framed_short_reads_refuse_before_unpaid_request() {
    let (_temp, _path, store, _record) = fixture();
    let lease = store.review_read_lease().unwrap();
    let bytes = fix1_frame(&serde_json::json!({"payload":"x".repeat(4096)}));
    let former_reservation = bytes.len() as u64 + format::MAX_BYTES as u64 + 44;
    lease.limits_for_test(storage::ReviewReadStats {
        bytes: former_reservation,
        pages: u64::MAX,
        rows: u64::MAX,
        work: u64::MAX,
    });
    let mut short = Fix1ShortLog::new(bytes);
    short.interrupt = Some(2);
    let result = store
        .storage
        .read_log_record::<serde_json::Value>(&mut short, 1);
    assert!(
        matches!(result, Err(StoreError::LimitExceeded)),
        "short-read retries must spend the same finite lease before each request: {result:?}"
    );
    let paid: u64 = short.requests.iter().map(|n| *n as u64).sum();
    assert_eq!(lease.stats().bytes, paid);
    assert!(paid <= former_reservation);
    let unpaid = short.requests.last().copied().unwrap() as u64 - 1;
    assert!(
        paid + unpaid > former_reservation,
        "the next remaining-buffer request was refused before reaching the reader"
    );
}

#[test]
fn whole_review_fix1_framed_magic_retries_payloads_and_eof_share_actual_charge() {
    let (_temp, _path, store, _record) = fixture();
    let lease = store.review_read_lease().unwrap();
    let values = [
        serde_json::json!({"first":"value"}),
        serde_json::json!({"second":[1,2,3]}),
    ];
    let mut bytes = b"QOSCON1\n".to_vec();
    bytes.extend(fix1_frame_at(&values[0], 1));
    bytes.extend(fix1_frame_at(&values[1], 2));
    let mut short = Fix1ShortLog::new(bytes);
    short.interrupt = Some(2);
    store.storage.read_log_magic(&mut short).unwrap();
    for (n, value) in values.iter().enumerate() {
        assert_eq!(
            store
                .storage
                .read_log_record::<serde_json::Value>(&mut short, n as u64 + 1)
                .unwrap(),
            Some(value.clone())
        );
    }
    let before = lease.stats();
    assert_eq!(
        store
            .storage
            .read_log_record::<serde_json::Value>(&mut short, 3)
            .unwrap(),
        None
    );
    assert_eq!(
        lease.stats().bytes - before.bytes,
        44,
        "EOF itself is a real header request"
    );
    assert_eq!(
        lease.stats().bytes,
        short.requests.iter().map(|n| *n as u64).sum::<u64>()
    );
    assert_eq!(
        &short.requests[..3],
        &[8, 7, 7],
        "Interrupted request is paid again on retry"
    );
}
#[test]
fn whole_review_fix1_framed_refusal_and_expired_eof_are_not_torn_tail() {
    let (_temp, _path, store, _record) = fixture();
    for expire in [false, true] {
        let lease = store.review_read_lease().unwrap();
        let mut short = Fix1ShortLog::new(vec![]);
        if expire {
            short.expire = Some(&lease);
        } else {
            short.interrupt = Some(1);
            lease.limits_for_test(storage::ReviewReadStats {
                bytes: 44,
                pages: u64::MAX,
                rows: u64::MAX,
                work: u64::MAX,
            });
        }
        assert!(matches!(
            store
                .storage
                .read_log_record::<serde_json::Value>(&mut short, 1),
            Err(StoreError::LimitExceeded)
        ));
        assert_eq!(
            short.requests,
            vec![44],
            "unpaid retry must not enter the underlying reader"
        );
        assert_eq!(lease.stats().bytes, 44);
    }
    let lease = store.review_read_lease().unwrap();
    lease.expire_for_test();
    let mut short = Fix1ShortLog::new(b"QOSCON1\n".to_vec());
    assert!(matches!(
        store.storage.read_log_magic(&mut short),
        Err(StoreError::LimitExceeded)
    ));
    assert!(short.requests.is_empty());
}
#[test]
fn whole_review_fix1_framed_short_tail_checksum_and_sequence_keep_old_meaning() {
    let (_temp, _path, store, _record) = fixture();
    let lease = store.review_read_lease().unwrap();
    let value = serde_json::json!({"payload":"bounded"});
    let bytes = fix1_frame(&value);
    for length in [0, 1, 43, 44, bytes.len() - 1] {
        let mut short = Fix1ShortLog::new(bytes[..length].to_vec());
        assert_eq!(
            store
                .storage
                .read_log_record::<serde_json::Value>(&mut short, 1)
                .unwrap(),
            None,
            "torn length={length}"
        );
    }
    let mut corrupt = bytes.clone();
    corrupt[12] ^= 1;
    let mut short = Fix1ShortLog::new(corrupt);
    assert!(matches!(
        store
            .storage
            .read_log_record::<serde_json::Value>(&mut short, 1),
        Err(StoreError::Corrupt)
    ));
    let mut short = Fix1ShortLog::new(bytes);
    assert!(matches!(
        store
            .storage
            .read_log_record::<serde_json::Value>(&mut short, 2),
        Err(StoreError::Corrupt)
    ));
    assert!(lease.stats().bytes > 0);
}
#[test]
fn whole_review_fix1_open_budget_refusal_never_repairs_the_log() {
    let (_temp, path, mut store, record) = fixture();
    let log_path = path.join(format!("log-{}", store.current.log));
    store.log.write_all(&[1, 2, 3]).unwrap();
    drop(store);
    let before = std::fs::read(&log_path).unwrap();
    let (storage, current) = storage::Storage::open(&path).unwrap();
    let lease = storage.read_lease(false).unwrap();
    lease.limits_for_test(storage::ReviewReadStats {
        bytes: 0,
        pages: u64::MAX,
        rows: u64::MAX,
        work: u64::MAX,
    });
    assert!(matches!(
        ConnectionStore::from_storage(storage, current),
        Err(StoreError::LimitExceeded)
    ));
    assert_eq!(std::fs::read(&log_path).unwrap(), before);
    drop(lease);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.load_connection(record.id).unwrap().revision, 1);
    assert_eq!(std::fs::read(&log_path).unwrap().len(), before.len() - 3);
    let lease = store.review_read_lease().unwrap();
    lease.limits_for_test(storage::ReviewReadStats {
        bytes: 0,
        pages: u64::MAX,
        rows: u64::MAX,
        work: u64::MAX,
    });
    assert!(
        matches!(
            store.page_referenced([234; 32]),
            Err(StoreError::LimitExceeded)
        ),
        "active-log reference read must retain its budget refusal"
    );
}

#[test]
fn retired_snapshot_first_reviewed_equality() {
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let c = store.connection(record.id).unwrap();
    historical_append(
        &mut store,
        Change::Verified {
            connection: record.id.0,
            serial: 1,
            expected: c.revision,
            source: c.source.identity,
            destination: c.destination.identity,
        },
    )
    .unwrap();
    let c = store.connection(record.id).unwrap();
    let op = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let cursor = store.review_cursor(record.id.0, 1).unwrap();
    let link = match store
        .row(&format::key(10, record.id.0, 1))
        .unwrap()
        .unwrap()
    {
        Row::InitialReviewLink(link) => link,
        _ => panic!("missing admitted link"),
    };
    let root = storage::build_pages(
        &store.storage,
        [Ok(Row::Baseline(format::BaselineEntry {
            connection: record.id.0,
            revision: c.revision + 1,
            ordinal: 0,
            path: op.path.clone(),
            state: op.source.clone().unwrap(),
        }))]
        .into_iter(),
    )
    .unwrap();
    let wire = serde_json::json!({
        "event": "ReviewedBaselineCommit",
        "body": { "connection": record.id.0, "expected": c.revision,
            "root": root, "count": 1, "source": c.source.identity,
            "destination": c.destination.identity, "initial_review": false },
        "provenance": { "kind": "ReviewedStep",
            "link_digest": format::digest(&format::encode(&link).unwrap()),
            "operation": link.operation, "step": 0, "entry": 2,
            "expected_progress": cursor.progress_revision,
            "expected_schedule": cursor.schedule_revision,
            "control_sequence": cursor.control_sequence,
            "lineage": { "kind": "Absent" } }
    });
    let change: Change =
        serde_json::from_value(wire).expect("closed reviewed baseline wire must decode");
    assert!(matches!(
        store.append(change.clone()),
        Err(StoreError::RecoveryRequired)
    ));
    historical_append(&mut store, change)
        .expect("canonical reviewed equality completes atomically");
    let committed = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    assert_eq!(committed.stage, 4);
    assert_eq!(committed.initial_review, None);
    assert_eq!(committed.expected, op.expected);
    assert_eq!(committed.native_identity, op.native_identity);
    assert_eq!(committed.native_budget, op.native_budget);
    let after = store.connection(record.id).unwrap();
    assert_eq!(after.pending, None);
    assert_eq!(after.baseline, Some(c.revision + 1));
    for table in [12, 13, 14] {
        let mut key = format::key(table, record.id.0, 1);
        if table == 13 {
            key.extend(0u32.to_be_bytes());
        }
        assert!(
            store.row(&key).unwrap().is_some(),
            "missing atomic table {table}"
        );
    }
    assert!(
        store
            .row(&format::key(15, record.id.0, 1))
            .unwrap()
            .is_none(),
        "unsupported directory remains work"
    );
    store.checkpoint().unwrap();
    drop(store);
    ConnectionStore::open_at(&path).unwrap();
}

fn reviewed_commit(store: &ConnectionStore, id: ConnectionId, canonical: Change) -> Change {
    let c = store.connection(id).unwrap();
    let cursor = store.review_cursor(id.0, 1).unwrap();
    let serial = c.pending.unwrap();
    let lineage = match store.row(&format::key(18, id.0, 1)).unwrap() {
        Some(Row::ChainBaselineLineageV1(head)) => head.reference(),
        None => format::LineageRef::Absent {},
        _ => panic!(),
    };
    let provenance = match store.row(&format::key(10, id.0, serial)).unwrap() {
        Some(Row::InitialReviewLink(link)) => format::ReviewedBaselineProvenanceV1::ReviewedStep {
            link_digest: format::digest(&format::encode(&link).unwrap()),
            operation: link.operation,
            step: link.step,
            entry: link.entry,
            expected_progress: cursor.progress_revision,
            expected_schedule: cursor.schedule_revision,
            control_sequence: cursor.control_sequence,
            lineage,
        },
        None => {
            let Some(Row::PostReviewOperationV1(link)) =
                store.row(&format::key(16, id.0, serial)).unwrap()
            else {
                panic!()
            };
            format::ReviewedBaselineProvenanceV1::Continuation {
                provenance_digest: super::continuation_digest(&link).unwrap(),
                operation: link.operation,
                expected_progress: cursor.progress_revision,
                expected_schedule: cursor.schedule_revision,
                control_sequence: cursor.control_sequence,
                lineage,
            }
        }
        _ => panic!(),
    };
    let mut body = serde_json::to_value(canonical).unwrap();
    assert_eq!(
        body.as_object_mut().unwrap().remove("event").unwrap(),
        "BaselineCommit"
    );
    Change::ReviewedBaselineCommit {
        body: serde_json::from_value(body).unwrap(),
        provenance,
    }
}

fn equality_commit(store: &ConnectionStore, id: ConnectionId) -> Change {
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
    entries.insert(
        op.path.to_relative().unwrap(),
        op.source.clone().unwrap().into(),
    );
    let count = entries.len() as u32;
    let root = storage::build_pages(
        &store.storage,
        entries
            .into_iter()
            .enumerate()
            .map(|(ordinal, (path, state))| {
                Ok(Row::Baseline(format::BaselineEntry {
                    connection: id.0,
                    revision: c.revision + 1,
                    ordinal: ordinal as u32,
                    path: format::NativePath::relative(&path),
                    state: state.into(),
                }))
            }),
    )
    .unwrap();
    reviewed_commit(
        store,
        id,
        Change::BaselineCommit {
            directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: id.0,
            expected: c.revision,
            root,
            count,
            source: c.source.identity,
            destination: c.destination.identity,
            initial_review: false,
        },
    )
}
#[test]
fn retired_snapshot_chain_then_equality_keeps_archive_version() {
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = addition_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    super::super::chain_tests::advance_in_place(&mut store, record.id, 3);
    let bare = super::super::chain_tests::baseline_change(&store, record.id);
    let commit = reviewed_commit(&store, record.id, bare);
    historical_append(&mut store, commit).unwrap();
    let archive = store
        .row(&format::key(17, record.id.0, 1))
        .unwrap()
        .expect("atomic archive");
    let old = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let c = store.connection(record.id).unwrap();
    historical_append(
        &mut store,
        Change::Verified {
            connection: record.id.0,
            serial: c.pending.unwrap(),
            expected: c.revision,
            source: c.source.identity,
            destination: c.destination.identity,
        },
    )
    .unwrap();
    let commit = equality_commit(&store, record.id);
    historical_append(&mut store, commit)
        .expect("archive reads its retained baseline after equality advances current");
    assert_eq!(
        store.row(&format::key(17, record.id.0, 1)).unwrap(),
        Some(archive)
    );
    assert_eq!(
        store
            .operation(OperationId {
                connection: record.id,
                serial: 1
            })
            .unwrap(),
        old
    );
    assert!(
        store
            .row(&format::key(19, record.id.0, 1))
            .unwrap()
            .is_some()
    );
    store.checkpoint().unwrap();
    drop(store);
    ConnectionStore::open_at(&path).unwrap();
}

#[test]
fn retired_snapshot_saved_equality_completion_requires_real_precommit() {
    use format::InitialReviewControlTransitionV1 as T;
    for saved in [0, 3] {
        let (_temp, path, mut store, record) = fixture();
        let accepted = owner(&mut store, &record);
        store.accept_initial_set(accepted).unwrap();
        let intent = equality_intent(&store, record.id);
        historical_append(&mut store, intent).unwrap();
        let op = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap();
        let digest = format::digest(&format::encode(&op).unwrap());
        if saved == 3 {
            let c = store.connection(record.id).unwrap();
            historical_append(
                &mut store,
                Change::Verified {
                    connection: record.id.0,
                    serial: 1,
                    expected: c.revision,
                    source: c.source.identity,
                    destination: c.destination.identity,
                },
            )
            .unwrap();
        }
        let stop = outer_control(
            &store,
            record.id,
            T::StopRequested {
                reason: format::PauseOrCancel::Pause,
            },
        );
        historical_append(&mut store, stop).unwrap();
        let boundary = equality_boundary(&store, record.id, digest);
        historical_append(&mut store, boundary).unwrap();
        for t in [T::StopAcknowledged {}, T::Resume {}] {
            let control = outer_control(&store, record.id, t);
            historical_append(&mut store, control).unwrap();
        }
        if saved == 0 {
            let c = store.connection(record.id).unwrap();
            historical_append(
                &mut store,
                Change::Verified {
                    connection: record.id.0,
                    serial: 1,
                    expected: c.revision,
                    source: c.source.identity,
                    destination: c.destination.identity,
                },
            )
            .unwrap();
        }
        let stop_key = format::key(11, record.id.0, 1);
        let saved_stop = store.row(&stop_key).unwrap().unwrap();
        assert_eq!(
            match &saved_stop {
                Row::InitialReviewStop(s) => s.fact().stage,
                _ => panic!(),
            },
            saved
        );
        let precommit = store
            .operation(OperationId {
                connection: record.id,
                serial: 1,
            })
            .unwrap()
            .revision;
        let commit = equality_commit(&store, record.id);
        historical_append(&mut store, commit).unwrap();
        let original = store
            .row(&format::key(12, record.id.0, 1))
            .unwrap()
            .unwrap();
        let Row::ReviewedBaselineResultV1(mut forged) = original.clone() else {
            panic!()
        };
        forged.witness.precommit_revision -= 1;
        store
            .overlay
            .insert(original.key(), Row::ReviewedBaselineResultV1(forged));
        assert!(
            store.validate_review_history(&[]).is_err(),
            "a projected Verified stage cannot replace the actual precommit cut"
        );
        store.overlay.insert(original.key(), original.clone());
        let Row::ReviewedBaselineResultV1(result) = &original else {
            panic!()
        };
        assert_eq!(result.witness.precommit_revision, precommit);
        let equality_baseline = store.connection(record.id).unwrap().baseline_root;
        let intent = addition_intent_at(&store, record.id, 0);
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
        assert_ne!(
            store.connection(record.id).unwrap().baseline_root,
            equality_baseline
        );
        assert_eq!(store.row(&stop_key).unwrap(), Some(saved_stop.clone()));
        assert_eq!(store.row(&original.key()).unwrap(), Some(original.clone()));
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(store.row(&stop_key).unwrap(), Some(saved_stop));
        assert_eq!(store.row(&original.key()).unwrap(), Some(original));
        assert_eq!(
            store
                .operation(OperationId {
                    connection: record.id,
                    serial: 1
                })
                .unwrap()
                .stage,
            4
        );
    }
}

fn reviewed_retirement(
    store: &ConnectionStore,
    id: ConnectionId,
    transition: format::DirectoryChainControlTransitionV1,
) -> Change {
    let cursor = store.review_cursor(id.0, 1).unwrap();
    let c = store.connection(id).unwrap();
    let inner = c.directory_chain.as_ref().unwrap();
    let schedule = c.directory_chain_schedule.as_ref().unwrap();
    Change::ReviewedChainRetirement {
        key: format::ReviewKey {
            connection: id.0,
            review: cursor.review,
            origin: 1,
        },
        before: super::format::ReviewCut::from_cursor(&cursor).unwrap(),
        inner: format::ReviewedInnerControlV1 {
            connection: id.0,
            anchor: inner.anchor,
            digest: inner.digest,
            expected_progress: inner.progress_revision,
            expected_schedule: schedule.revision,
            control_sequence: schedule.control_sequence + 1,
            transition,
        },
        baseline: format::BaselineVersion::from_connection(&c).unwrap(),
        lineage: match store.row(&format::key(18, id.0, 1)).unwrap() {
            Some(Row::ChainBaselineLineageV1(head)) => head.reference(),
            None => format::LineageRef::Absent {},
            _ => panic!(),
        },
    }
}
#[test]
fn retired_snapshot_abandoned_without_baseline_retains_explicit_absence() {
    use format::DirectoryChainControlTransitionV1 as T;
    let (_temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = addition_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let abandon = reviewed_retirement(&store, record.id, T::AbandonRequested);
    historical_append(&mut store, abandon).unwrap();
    let c = store.connection(record.id).unwrap();
    historical_append(
        &mut store,
        Change::NativeBoundary {
            connection: record.id.0,
            serial: 1,
            expected: c.revision,
            operation_expected: c.revision,
            fence: 0,
            kind: 0,
            evidence: None,
        },
    )
    .unwrap();
    let retire = reviewed_retirement(&store, record.id, T::Retire);
    historical_append(&mut store, retire).unwrap();
    let Some(Row::ChainCompletionBaselineV1(archive)) =
        store.row(&format::key(17, record.id.0, 1)).unwrap()
    else {
        panic!()
    };
    assert_eq!(archive.core.baseline, format::BaselineVersion::Absent {});
    assert!(matches!(
        archive.core.created,
        format::ArchiveCreatedV1::Retirement { .. }
    ));
    assert!(
        store
            .row(&format::key(12, record.id.0, 1))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .row(&format::key(19, record.id.0, 1))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .row(&format::key(15, record.id.0, 1))
            .unwrap()
            .is_none()
    );
    store.checkpoint().unwrap();
    drop(store);
    ConnectionStore::open_at(&path).unwrap();
}

fn accept_supported_review(store: &mut ConnectionStore, record: &ConnectionRecord) {
    use crate::connection::model::{Endpoint, NamePolicy, Observation, ObservedTree};
    use crate::connection::transaction::{complete_review_for_test, initial_preview_for_test};
    let file = EntryState::File {
        sha256: format::digest(b"initial content"),
        bytes: 15,
        executable: false,
    };
    let trees: Vec<_> = [record.source.identity, record.destination.identity]
        .into_iter()
        .enumerate()
        .map(|(side, identity)| {
            ObservedTree::new(
                Endpoint {
                    identity,
                    encoding: Encoding::Unix,
                    names: NamePolicy::Exact,
                },
                [
                    (
                        RelativePath::unix(if side == 0 { b"a" } else { b"b" }).unwrap(),
                        file.clone(),
                    ),
                    (RelativePath::unix(b"equal").unwrap(), file.clone()),
                ],
                Observation::Complete,
            )
            .unwrap()
        })
        .collect();
    let session = store.transaction_session(record.id).unwrap();
    let preview =
        initial_preview_for_test(store, record.id, trees[0].clone(), trees[1].clone()).unwrap();
    let accepted =
        complete_review_for_test(store, &session, preview, trees[0].clone(), trees[1].clone())
            .unwrap();
    store.accept_initial_set(accepted).unwrap();
}
fn finish_reviewed_pending(store: &mut ConnectionStore, id: ConnectionId) {
    let c = store.connection(id).unwrap();
    let op = store
        .operation(OperationId {
            connection: id,
            serial: c.pending.unwrap(),
        })
        .unwrap();
    let commit = if op.decision == 0 {
        historical_append(
            store,
            Change::Verified {
                connection: id.0,
                serial: op.serial,
                expected: c.revision,
                source: c.source.identity,
                destination: c.destination.identity,
            },
        )
        .unwrap();
        equality_commit(store, id)
    } else {
        super::super::chain_tests::advance_in_place(store, id, 3);
        reviewed_commit(
            store,
            id,
            super::super::chain_tests::baseline_change(store, id),
        )
    };
    historical_append(store, commit).unwrap();
}
fn continuation_intent(
    store: &ConnectionStore,
    id: ConnectionId,
    operation: format::Operation,
) -> Change {
    let cursor = store.review_cursor(id.0, 1).unwrap();
    let c = store.connection(id).unwrap();
    let Some(Row::InitialReviewCompletedV1(complete)) =
        store.row(&format::key(15, id.0, 1)).unwrap()
    else {
        panic!()
    };
    let provenance = format::PostReviewOperationV1 {
        version: 1,
        key: complete.key.clone(),
        completion_digest: super::completion_digest(&complete).unwrap(),
        operation: format::ChainOperationIdV1 {
            connection: id.0,
            serial: operation.serial,
        },
        intent_digest: super::original_intent_digest(&operation).unwrap(),
        admission_digest: format::digest(operation.native_identity.as_ref().unwrap()),
        expected: operation.expected,
        baseline: format::BaselineVersion::from_connection(&c).unwrap(),
        lineage: match store.row(&format::key(18, id.0, 1)).unwrap() {
            Some(Row::ChainBaselineLineageV1(head)) => head.reference(),
            None => format::LineageRef::Absent {},
            _ => panic!(),
        },
        kind: format::PostReviewKindV1::Ordinary {},
    };
    Change::ReviewedContinuationIntent {
        operation,
        provenance,
        expected_progress: cursor.progress_revision,
        expected_schedule: cursor.schedule_revision,
        control_sequence: cursor.control_sequence,
    }
}
#[test]
fn retired_snapshot_both_directions_complete_and_ordinary_continuation() {
    for equality_first in [false, true] {
        let (_temp, path, mut store, record) = fixture();
        accept_supported_review(&mut store, &record);
        let original = store.review_fact(record.id.0, 1).unwrap();
        let order = if equality_first { [2, 1, 0] } else { [0, 2, 1] };
        for ordinal in order {
            let intent = if ordinal == 2 {
                equality_intent(&store, record.id)
            } else {
                addition_intent_at(&store, record.id, ordinal)
            };
            historical_append(&mut store, intent).unwrap();
            finish_reviewed_pending(&mut store, record.id);
        }
        let Some(Row::InitialReviewCompletedV1(complete)) =
            store.row(&format::key(15, record.id.0, 1)).unwrap()
        else {
            panic!("positive full original coverage")
        };
        assert_eq!(complete.entries, 3);
        assert_eq!(complete.completed_steps, 3);
        let archives: Vec<_> = store
            .rows()
            .unwrap()
            .map(Result::unwrap)
            .filter(|row| matches!(row, Row::ChainCompletionBaselineV1(_)))
            .collect();
        assert_eq!(archives.len(), 2);
        let Change::ReviewedIntent { operation, .. } = equality_intent(&store, record.id) else {
            panic!()
        };
        let change = continuation_intent(&store, record.id, operation);
        historical_append(&mut store, change).unwrap();
        finish_reviewed_pending(&mut store, record.id);
        assert!(
            store
                .row(&format::key(16, record.id.0, 4))
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .row(&format::key(10, record.id.0, 4))
                .unwrap()
                .is_none()
        );
        for archive in archives {
            assert_eq!(store.row(&archive.key()).unwrap(), Some(archive));
        }
        assert_eq!(store.review_fact(record.id.0, 1).unwrap(), original);
        assert_eq!(
            store.row(&format::key(15, record.id.0, 1)).unwrap(),
            Some(Row::InitialReviewCompletedV1(complete))
        );
        store.checkpoint().unwrap();
        drop(store);
        ConnectionStore::open_at(&path).unwrap();
    }
}

// A real durable reservation funds this private inspection context. It supplies
// read credit only: historical append remains the sole fixture event owner.
fn reserved_review_reads(
    store: &mut ConnectionStore,
    id: ConnectionId,
) -> (
    OperationId,
    Arc<crate::connection::transaction::native::OperationReads>,
) {
    let c = store.connection(id).unwrap();
    let operation = OperationId {
        connection: id,
        serial: c.pending.unwrap(),
    };
    let op = store.operation(operation).unwrap();
    let previous = op.native_budget.unwrap();
    let next = format::NativeBudget {
        version: 1,
        reserved: previous.reserved + 1024 * 1024 * 1024,
        sequence: previous.sequence + 1,
    };
    historical_append(
        store,
        Change::ReadReserve {
            connection: id.0,
            serial: operation.serial,
            expected: c.revision,
            fence: c.fence,
            previous: previous.clone(),
            next: next.clone(),
            recovery_only: false,
            operation_expected: None,
        },
    )
    .unwrap();
    let reads = Arc::new(
        crate::connection::transaction::native::OperationReads::reopened(previous.sequence),
    );
    reads
        .grant(next.sequence, next.reserved - previous.reserved)
        .unwrap();
    (operation, reads)
}
fn inspection_context<'a>(
    store: &ConnectionStore,
    id: OperationId,
    reads: Arc<crate::connection::transaction::native::OperationReads>,
    cancel: &'a AtomicBool,
) -> super::super::NativeReadContext<'a> {
    super::super::NativeReadContext {
        id,
        reads,
        lifetime: Arc::downgrade(&store.native_lifetime),
        gate: Arc::new(crate::connection::transaction::AdmissionGate::new()),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        cancel,
        inspection_only: true,
        recovery_only: false,
    }
}
fn remaining_credit(reads: &crate::connection::transaction::native::OperationReads) -> u64 {
    let (mut low, mut high) = (0, 1024 * 1024 * 1024 + 1);
    while low + 1 < high {
        let mid = low + (high - low) / 2;
        if reads.require_credit(mid).is_ok() {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}
#[test]
fn retired_snapshot_paid_checkpoint_charges_every_physical_read() {
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let (id, reads) = reserved_review_reads(&mut store, record.id);
    let cancel = AtomicBool::new(false);
    let context = inspection_context(&store, id, Arc::clone(&reads), &cancel);
    let lease = store.review_read_lease().unwrap();
    let before = lease.stats().bytes;
    let credit = remaining_credit(&reads);
    store.checkpoint_with_context(Some(&context)).unwrap();
    let physical = lease.stats().bytes - before;
    let paid = credit - remaining_credit(&reads);
    assert_eq!(
        paid, physical,
        "CURRENT, original entries, history, duplicate pages and GC must share the supplied paid context"
    );
    assert!(paid > 0);
    eprintln!("paid checkpoint physical_bytes={physical} native_charge={paid}");
}

fn accept_nested_review(store: &mut ConnectionStore, record: &ConnectionRecord, children: bool) {
    accept_nested_review_direction(store, record, children, false)
}
fn accept_nested_review_direction(
    store: &mut ConnectionStore,
    record: &ConnectionRecord,
    children: bool,
    reverse: bool,
) {
    use crate::connection::model::{Endpoint, NamePolicy, Observation, ObservedTree};
    use crate::connection::transaction::{complete_review_for_test, initial_preview_for_test};
    let file = EntryState::File {
        sha256: [6; 32],
        bytes: 19,
        executable: false,
    };
    let mut source = vec![
        (RelativePath::unix(b"a").unwrap(), EntryState::Directory),
        (RelativePath::unix(b"equal").unwrap(), file.clone()),
    ];
    if children {
        for path in [b"a/b", b"a/c"] {
            source.push((RelativePath::unix(path).unwrap(), file.clone()));
        }
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
    let other = vec![(RelativePath::unix(b"equal").unwrap(), file)];
    let (source, destination) = if reverse {
        (other, source)
    } else {
        (source, other)
    };
    let source = tree(record.source.identity, source);
    let destination = tree(record.destination.identity, destination);
    let session = store.transaction_session(record.id).unwrap();
    let preview =
        initial_preview_for_test(store, record.id, source.clone(), destination.clone()).unwrap();
    let accepted = complete_review_for_test(store, &session, preview, source, destination).unwrap();
    store.accept_initial_set(accepted).unwrap();
}
#[test]
fn retired_snapshot_directory_final_and_shared_missing_parent() {
    for (children, reverse) in [(false, false), (true, false), (false, true), (true, true)] {
        let (_temp, path, mut store, record) = fixture();
        accept_nested_review_direction(&mut store, &record, children, reverse);
        let intent = addition_intent_at(&store, record.id, if children { 1 } else { 0 });
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
        if children {
            assert!(matches!(
                store.review_cursor(record.id.0, 1).unwrap().current,
                format::InitialReviewCurrentV1::BetweenMembers { next_index: 1, .. }
            ));
            for transition in [
                format::InitialReviewControlTransitionV1::StopRequested {
                    reason: format::PauseOrCancel::Pause,
                },
                format::InitialReviewControlTransitionV1::StopAcknowledged {},
                format::InitialReviewControlTransitionV1::Resume {},
            ] {
                let control = chain_control(&store, record.id, transition);
                historical_append(&mut store, control)
                    .expect("between-member controls use the canonical inner reducer");
            }
            let intent = addition_intent_at(&store, record.id, 1);
            historical_append(&mut store, intent).unwrap();
            finish_reviewed_pending(&mut store, record.id);
            let intent = addition_intent_at(&store, record.id, 2);
            let Change::ReviewedIntent { operation, .. } = &intent else {
                panic!()
            };
            let selected = super::super::chain::first_selection(operation).unwrap();
            assert_eq!(selected.member_count().unwrap(), 1);
            assert_eq!(selected.baselined_prefixes, 2);
            historical_append(&mut store, intent).unwrap();
            finish_reviewed_pending(&mut store, record.id);
        }
        let intent = equality_intent(&store, record.id);
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
        assert!(
            store
                .row(&format::key(15, record.id.0, 1))
                .unwrap()
                .is_some()
        );
        store.checkpoint().unwrap();
        drop(store);
        ConnectionStore::open_at(&path).unwrap();
    }
}

#[test]
fn retired_snapshot_abandonment_after_parent_and_fence_accounting() {
    use format::DirectoryChainControlTransitionV1 as T;
    for active in [false, true] {
        for fence in [false, true] {
            let (_temp, path, mut store, record) = fixture();
            accept_nested_review(&mut store, &record, true);
            let intent = addition_intent_at(&store, record.id, 1);
            historical_append(&mut store, intent).unwrap();
            finish_reviewed_pending(&mut store, record.id);
            let baseline = store.connection(record.id).unwrap().baseline;
            if active {
                let intent = addition_intent_at(&store, record.id, 1);
                historical_append(&mut store, intent).unwrap();
            }
            let abandon = reviewed_retirement(&store, record.id, T::AbandonRequested);
            historical_append(&mut store, abandon).unwrap();
            if fence {
                let c = store.connection(record.id).unwrap();
                store
                    .append(Change::Fence {
                        connection: record.id.0,
                        expected: c.revision,
                        generation: 1,
                    })
                    .expect("fence retains the exact abandoning cut");
            }
            if active {
                let c = store.connection(record.id).unwrap();
                let op = store
                    .operation(OperationId {
                        connection: record.id,
                        serial: c.pending.unwrap(),
                    })
                    .unwrap();
                historical_append(
                    &mut store,
                    Change::NativeBoundary {
                        connection: record.id.0,
                        serial: op.serial,
                        expected: c.revision,
                        operation_expected: op.revision,
                        fence: c.fence,
                        kind: 0,
                        evidence: None,
                    },
                )
                .unwrap();
            }
            let retire = reviewed_retirement(&store, record.id, T::Retire);
            if fence {
                assert!(
                    historical_append(&mut store, retire).is_err(),
                    "the existing canonical Retire lifecycle refusal remains closed"
                );
            } else {
                historical_append(&mut store, retire).unwrap();
                let Some(Row::ChainCompletionBaselineV1(archive)) =
                    store.row(&format::key(17, record.id.0, 1)).unwrap()
                else {
                    panic!()
                };
                assert_eq!(archive.core.baseline.revision(), baseline);
                assert!(
                    store
                        .row(&format::key(19, record.id.0, 1))
                        .unwrap()
                        .is_none()
                );
            }
            store.checkpoint().unwrap();
            drop(store);
            ConnectionStore::open_at(&path).unwrap();
        }
    }
}

#[test]
fn retired_snapshot_paid_framing_retries_eof_and_refusal_are_exact() {
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let (id, reads) = reserved_review_reads(&mut store, record.id);
    let cancel = AtomicBool::new(false);
    let context = inspection_context(&store, id, Arc::clone(&reads), &cancel);
    let lease = store.review_read_lease().unwrap();
    let value = serde_json::json!({"x":"short retries"});
    let mut short = Fix1ShortLog::new(fix1_frame(&value));
    short.interrupt = Some(2);
    let before = remaining_credit(&reads);
    let stats = lease.stats().bytes;
    assert_eq!(
        store
            .storage
            .read_log_record_with_context::<serde_json::Value>(&mut short, 1, Some(&context))
            .unwrap(),
        Some(value)
    );
    assert_eq!(
        store
            .storage
            .read_log_record_with_context::<serde_json::Value>(&mut short, 2, Some(&context))
            .unwrap(),
        None
    );
    let requested = short.requests.iter().map(|n| *n as u64).sum::<u64>();
    assert_eq!(before - remaining_credit(&reads), requested);
    assert_eq!(lease.stats().bytes - stats, requested);
    struct CancelAtEof<'a>(&'a AtomicBool);
    impl Read for CancelAtEof<'_> {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.0.store(true, std::sync::atomic::Ordering::Release);
            Ok(0)
        }
    }
    assert!(matches!(
        store
            .storage
            .read_log_record_with_context::<serde_json::Value>(
                &mut CancelAtEof(&cancel),
                1,
                Some(&context)
            ),
        Err(StoreError::Cancelled)
    ));
    cancel.store(false, std::sync::atomic::Ordering::Release);
    let header_request = short.requests[0];
    reads
        .charge(remaining_credit(&reads) - header_request as u64)
        .unwrap();
    let mut short = Fix1ShortLog::new(fix1_frame(&serde_json::json!(true)));
    assert!(matches!(
        store
            .storage
            .read_log_record_with_context::<serde_json::Value>(&mut short, 1, Some(&context)),
        Err(StoreError::LimitExceeded)
    ));
    assert_eq!(short.requests, vec![header_request]);
    assert_eq!(remaining_credit(&reads), 0);
}
#[test]
fn retired_snapshot_paid_baseline_reduction_checkpoint_reduction_share_lease() {
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let (id, reads) = reserved_review_reads(&mut store, record.id);
    let c = store.connection(record.id).unwrap();
    historical_append(
        &mut store,
        Change::Verified {
            connection: record.id.0,
            serial: id.serial,
            expected: c.revision,
            source: c.source.identity,
            destination: c.destination.identity,
        },
    )
    .unwrap();
    let event = Event {
        schema: 1,
        predecessor: store.digest,
        change: equality_commit(&store, record.id),
    };
    let cancel = AtomicBool::new(false);
    let context = inspection_context(&store, id, Arc::clone(&reads), &cancel);
    let lease = store.review_read_lease().unwrap();
    let before = lease.stats().bytes;
    let physical_trace = storage::physical_trace::begin();
    let credit = remaining_credit(&reads);
    let mut checks = RecoveryChecks {
        mode: VerificationMode::NativeLive(&context),
        ..RecoveryChecks::default()
    };
    // Exercise the identical production reduction/preparation/checkpoint/read
    // seams with an inspection context; this context cannot append an event.
    store.candidate_pin = store
        .prepare_candidate_pin(&event.change, Some(&context))
        .unwrap();
    let rows = store.reduce(&event, &mut checks).unwrap();
    store
        .prepare_rows_with_context(rows.clone(), Some(&context))
        .unwrap();
    store.checkpoint_with_context(Some(&context)).unwrap();
    let repeated = store.reduce(&event, &mut checks).unwrap();
    assert_eq!(rows, repeated);
    store
        .prepare_rows_with_context(repeated, Some(&context))
        .unwrap();
    assert_eq!(
        credit - remaining_credit(&reads),
        lease.stats().bytes - before
    );
    assert!(matches!(
        store.validate_verification_mode(&event.change, &checks, false),
        Err(StoreError::RecoveryRequired)
    ));
    let trace = physical_trace.finish();
    assert_eq!(trace.reductions.len(), 2);
    let requested: u64 = trace.reads.iter().map(|read| read.requested as u64).sum();
    assert!(
        trace
            .reads
            .iter()
            .all(|read| read.returned.is_some() && !read.name.is_empty())
    );
    assert_eq!(requested, credit - remaining_credit(&reads));
    eprintln!(
        "independent actual Read calls={} requested={requested}",
        trace.reads.len()
    );
    eprintln!(
        "paid two reductions plus checkpoint physical_bytes={} native_charge={}",
        lease.stats().bytes - before,
        credit - remaining_credit(&reads)
    );
}

fn ordinary_edit_intent(
    store: &ConnectionStore,
    id: ConnectionId,
    path: &[u8],
    from: usize,
    old: &[u8],
    new: Option<&[u8]>,
) -> Change {
    use crate::connection::transaction::native::descriptor as d;
    let Change::ReviewedIntent { mut operation, .. } = equality_intent(store, id) else {
        panic!()
    };
    let mut admission = d::Admission::decode(operation.native_identity.as_ref().unwrap()).unwrap();
    admission.path = RelativePath::unix(path).unwrap();
    admission.direction = from as u8;
    admission.decision = if new.is_some() {
        if from == 0 { 1 } else { 2 }
    } else if from == 0 {
        4
    } else {
        3
    };
    for side in 0..2 {
        let bytes = if side == from { new } else { Some(old) };
        if let Some(bytes) = bytes {
            let d::State::Regular { stamp, content, .. } = &mut admission.states[side] else {
                panic!()
            };
            stamp.length = bytes.len() as u64;
            *content = d::Content {
                digest: format::digest(bytes),
                length: bytes.len() as u64,
                executable: false,
            };
        } else {
            admission.states[side] = d::State::Absent;
        }
    }
    let state = |bytes: &[u8]| format::State::File {
        sha256: format::digest(bytes),
        bytes: bytes.len() as u64,
        executable: false,
    };
    operation.path = admission.path.clone().into();
    operation.decision = admission.decision;
    operation.source = if from == 0 {
        new.map(state)
    } else {
        Some(state(old))
    };
    operation.destination = if from == 1 {
        new.map(state)
    } else {
        Some(state(old))
    };
    operation.native_identity = Some(admission.encode().unwrap());
    continuation_intent(store, id, operation)
}
fn finish_ordinary_edit(
    store: &mut ConnectionStore,
    id: ConnectionId,
    parent: &Arc<storage::Directory>,
    old: &[u8],
) -> format::Recovery {
    finish_ordinary_edit_observed(store, id, parent, old, false)
}
fn finish_ordinary_edit_observed(
    store: &mut ConnectionStore,
    id: ConnectionId,
    parent: &Arc<storage::Directory>,
    old: &[u8],
    observe_bound: bool,
) -> format::Recovery {
    use crate::connection::transaction::native::descriptor as d;
    let c = store.connection(id).unwrap();
    let op = store
        .operation(OperationId {
            connection: id,
            serial: c.pending.unwrap(),
        })
        .unwrap();
    let admission = d::Admission::decode(op.native_identity.as_ref().unwrap()).unwrap();
    let from = admission.direction as usize;
    let target = 1 - from;
    let deletion = matches!(op.decision, 3 | 4);
    let mut reg = format::NativeRegistration {
        native_kind: None,
        connection: id.0,
        serial: op.serial,
        attempt: 1,
        nonce: [31; 16],
        side: target as u8,
        admission: op.expected + 1,
        baseline: op.baseline,
        fence: c.fence,
        intent: None,
        stage: None,
        outcome: None,
        outcome_displaced: None,
    };
    historical_append(
        store,
        Change::NativeReserve {
            registration: reg.clone(),
            expected: c.revision,
        },
    )
    .unwrap();
    let progress = store.connection(id).unwrap().revision;
    let header = d::Header {
        connection: id.0,
        operation: op.serial,
        attempt: 1,
        nonce: reg.nonce,
        epoch: [32; 16],
        session: [33; 16],
        admission: op.expected + 1,
        baseline: op.baseline,
        progress,
        fence: c.fence,
    };
    let object = |ino, kind| d::Object {
        dev: 1,
        ino,
        kind,
        generation: [ino as u8; 32],
    };
    let intent = d::encode(
        &header,
        &d::Body::Intent(d::Intent {
            endpoints: admission.endpoints,
            direction: admission.direction,
            decision: admission.decision,
            namespace: object(50, 0o040_000),
            path: admission.path.clone(),
            chains: admission.chains.clone(),
            expected: admission.states.clone(),
            selection: d::Selection::None,
        }),
    )
    .unwrap();
    let intent = d::decode(&intent).unwrap();
    reg.intent = Some(d::encode(&intent.header, &intent.body).unwrap());
    if !deletion {
        let d::State::Regular {
            object: original,
            stamp,
            content,
        } = &admission.states[from]
        else {
            panic!()
        };
        reg.stage = Some(
            d::encode(
                &header,
                &d::Body::Stage(d::Stage {
                    intent: intent.digest,
                    object: object(51, 0o100_000),
                    content: content.clone(),
                    source: d::Source::Endpoint {
                        side: from as u8,
                        parent: admission.chains[from].last().unwrap().clone(),
                        object: original.clone(),
                        stamp: stamp.clone(),
                    },
                }),
            )
            .unwrap(),
        );
    }
    historical_append(
        store,
        Change::NativePrepared {
            registration: reg.clone(),
            expected: progress,
        },
    )
    .unwrap();
    let d::State::Regular {
        object: original,
        stamp,
        content,
    } = &admission.states[target]
    else {
        panic!()
    };
    let stage = reg.stage.as_ref().map(|b| d::decode(b).unwrap());
    let displaced = d::encode(
        &header,
        &d::Body::Displaced(d::Displaced {
            intent: intent.digest,
            stage: stage.as_ref().map(|s| s.digest),
            object: original.clone(),
            stamp: stamp.clone(),
            matches_expected: true,
            side: target as u8,
            parent: admission.chains[target].last().unwrap().clone(),
            leaf: admission.path.components().last().unwrap().clone(),
            barriers: 3,
        }),
    )
    .unwrap();
    let mut entry = format::Recovery {
        captured: None,
        connection: id.0,
        serial: op.serial,
        slot: 0,
        side: target as u8,
        path: op.path.clone(),
        reason: if deletion { 0 } else { 1 },
        state: 0,
        content: None,
        object: None,
        retained: 0,
    };
    let expected = store.connection(id).unwrap().revision;
    historical_append(
        store,
        Change::Reserve {
            entry: entry.clone(),
            expected,
            fence: c.fence,
        },
    )
    .expect("ordinary post-review recovery reservation");
    let name = format!("{}-0", op.serial);
    let mut file = storage::open_file(parent, &name, true).unwrap();
    file.write_all(old).unwrap();
    storage::sync_file(&file).unwrap();
    let identity = storage::regular(&file).unwrap();
    let mut reread = storage::reread_file(parent, &name).unwrap();
    let mut actual = Vec::new();
    reread.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, old);
    assert_eq!(format::digest(&actual), content.digest);
    storage::validate_file(parent, &name, &file, &identity).unwrap();
    storage::sync_directory(parent).unwrap();
    drop(reread);
    drop(file);
    entry.state = 1;
    entry.content = Some(format::State::File {
        sha256: format::digest(old),
        bytes: old.len() as u64,
        executable: false,
    });
    entry.object = Some(format::ObjectIdentity {
        root: store.storage.identity.clone(),
        file_id: storage::file_identity(&identity),
    });
    let expected = store.connection(id).unwrap().revision;
    historical_append(
        store,
        Change::Bytes {
            entry: entry.clone(),
            expected,
            fence: c.fence,
        },
    )
    .unwrap();
    let fact = d::RecoveryFact {
        intent: intent.digest,
        recovery: d::Recovery {
            connection: id.0,
            operation: op.serial,
            slot: 0,
        },
        side: target as u8,
        role: 0,
        endpoint: admission.endpoints[target],
        object: original.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
    };
    let bound = format::NativeRecoveryFact {
        captured: None,
        connection: id.0,
        serial: op.serial,
        slot: 0,
        attempt: 1,
        fact: fact.encode().unwrap(),
        displaced: displaced.clone(),
    };
    if observe_bound {
        let (payer, reads) = reserved_review_reads(store, id);
        store.checkpoint().unwrap();
        let expected = store.connection(id).unwrap().revision;
        let cancel = AtomicBool::new(false);
        let context = inspection_context(store, payer, reads, &cancel);
        fix1_assert_paid_reduction(
            store,
            Change::NativeBound {
                fact: bound.clone(),
                expected,
                fence: c.fence,
            },
            &context,
        );
    }
    let expected = store.connection(id).unwrap().revision;
    historical_append(
        store,
        Change::NativeBound {
            fact: bound,
            expected,
            fence: c.fence,
        },
    )
    .unwrap();
    let mut states = admission.states.clone();
    if deletion {
        states = [d::State::Absent, d::State::Absent];
    } else {
        let d::Body::Stage(staged) = &stage.as_ref().unwrap().body else {
            panic!()
        };
        let d::State::Regular { stamp, .. } = &admission.states[from] else {
            panic!()
        };
        states[target] = d::State::Regular {
            object: staged.object.clone(),
            stamp: stamp.clone(),
            content: staged.content.clone(),
        };
    }
    let expected = store.connection(id).unwrap().revision;
    let outcome = d::encode(
        &d::Header {
            progress: expected,
            ..header
        },
        &d::Body::Outcome(d::Outcome {
            intent: intent.digest,
            stage: stage.as_ref().map(|s| s.digest),
            displaced: Some(d::decode(&displaced).unwrap().digest),
            states,
            kind: if deletion { 2 } else { 1 },
            barriers: 3,
            recovery: vec![d::Reference {
                slot: 0,
                side: target as u8,
                role: 0,
                binding: fact.binding_digest().unwrap(),
            }],
            sequence: 1,
        }),
    )
    .unwrap();
    d::decode(&outcome)
        .unwrap()
        .joined(
            &intent,
            stage.as_ref(),
            Some(&d::decode(&displaced).unwrap()),
        )
        .unwrap();
    reg.outcome = Some(outcome);
    reg.outcome_displaced = Some(displaced);
    for verified in [false, true] {
        let expected = store.connection(id).unwrap().revision;
        historical_append(
            store,
            Change::NativeOutcome {
                registration: reg.clone(),
                expected,
                verified,
                reconstructed: false,
            },
        )
        .unwrap();
    }
    let c = store.connection(id).unwrap();
    let mut entries = store.load_baseline(id).unwrap().entries().cloned().unwrap();
    let path = op.path.to_relative().unwrap();
    if deletion {
        entries.remove(&path);
    } else {
        entries.insert(
            path,
            if from == 0 {
                op.source.unwrap().into()
            } else {
                op.destination.unwrap().into()
            },
        );
    }
    let count = entries.len() as u32;
    let root = storage::build_pages(
        &store.storage,
        entries
            .into_iter()
            .enumerate()
            .map(|(ordinal, (path, state))| {
                Ok(Row::Baseline(format::BaselineEntry {
                    connection: id.0,
                    revision: c.revision + 1,
                    ordinal: ordinal as u32,
                    path: path.into(),
                    state: state.into(),
                }))
            }),
    )
    .unwrap();
    let commit = reviewed_commit(
        store,
        id,
        Change::BaselineCommit {
            directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: id.0,
            expected: c.revision,
            root,
            count,
            source: c.source.identity,
            destination: c.destination.identity,
            initial_review: false,
        },
    );
    historical_append(store, commit).unwrap();
    entry.state = 2;
    entry
}
#[test]
fn retired_snapshot_ordinary_edits_and_recoverable_deletions_keep_archives() {
    let (_temp, path, mut store, record) = fixture();
    accept_supported_review(&mut store, &record);
    for ordinal in [0, 1, 2] {
        let intent = if ordinal == 2 {
            equality_intent(&store, record.id)
        } else {
            addition_intent_at(&store, record.id, ordinal)
        };
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
    }
    let archives: Vec<_> = store
        .rows()
        .unwrap()
        .map(Result::unwrap)
        .filter(|r| matches!(r, Row::ChainCompletionBaselineV1(_)))
        .collect();
    let parent = store
        .storage
        .recovery
        .create_child(std::ffi::OsStr::new(&format::hex(&record.id.0)))
        .unwrap();
    storage::sync_directory(&store.storage.recovery).unwrap();
    for (from, path, old, new) in [
        (
            0,
            b"equal".as_slice(),
            b"initial content".as_slice(),
            Some(b"source edit".as_slice()),
        ),
        (
            1,
            b"equal".as_slice(),
            b"source edit".as_slice(),
            Some(b"destination edit".as_slice()),
        ),
        (0, b"equal".as_slice(), b"destination edit".as_slice(), None),
        (1, b"a".as_slice(), b"initial content".as_slice(), None),
    ] {
        let intent = ordinary_edit_intent(&store, record.id, path, from, old, new);
        historical_append(&mut store, intent).unwrap();
        let entry = finish_ordinary_edit(&mut store, record.id, &parent, old);
        assert_eq!(store.row(&entry.key()).unwrap(), Some(Row::Recovery(entry)));
        for archive in &archives {
            assert_eq!(store.row(&archive.key()).unwrap().as_ref(), Some(archive));
        }
        store.checkpoint().unwrap();
    }
    assert_eq!(
        store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .len(),
        1
    );
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store
            .recovery_page(record.id, None, 32)
            .unwrap()
            .entries
            .len(),
        4
    );
}

#[test]
fn retired_snapshot_paid_context_requires_owner_lease_and_store_lifetime() {
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let (id, reads) = reserved_review_reads(&mut store, record.id);
    let cancel = AtomicBool::new(false);
    let context = inspection_context(&store, id, Arc::clone(&reads), &cancel);
    let mut reader = Fix1ShortLog::new(Vec::new());
    let credit = remaining_credit(&reads);
    assert!(matches!(
        store
            .storage
            .read_log_record_with_context::<serde_json::Value>(&mut reader, 1, Some(&context)),
        Err(StoreError::RecoveryRequired)
    ));
    assert!(reader.requests.is_empty());
    assert_eq!(credit, remaining_credit(&reads));
    assert!(
        matches!(
            store.validate_review_history_with_context(&[], Some(&context)),
            Err(StoreError::RecoveryRequired)
        ),
        "a paid history entry cannot create a replacement store lease"
    );
    assert_eq!(credit, remaining_credit(&reads));
    let (_other_temp, _other_path, other, _) = fixture();
    let _other_lease = other.review_read_lease().unwrap();
    assert!(matches!(
        other.row_with_context(&format::key(0, record.id.0, 0), Some(&context)),
        Err(StoreError::StaleRevision)
    ));
    assert_eq!(credit, remaining_credit(&reads));
    let _lease = store.review_read_lease().unwrap();
    assert_eq!(
        store
            .storage
            .read_log_record_with_context::<serde_json::Value>(&mut reader, 1, Some(&context))
            .unwrap(),
        None
    );
    assert!(!reader.requests.is_empty());
}

fn maximum_review_rows() -> Vec<Row> {
    use format::*;
    let key = ReviewKey {
        connection: [255; 16],
        review: [255; 32],
        origin: u64::MAX - 100,
    };
    let operation = ChainOperationIdV1 {
        connection: key.connection,
        serial: u64::MAX,
    };
    let version = |revision| BaselineVersion::Present {
        revision,
        root: PageRef::Page { hash: [255; 32] },
        count: 4096,
    };
    let before = version(u64::MAX - 3);
    let after = version(u64::MAX - 1);
    let lineage = LineageRef::Present {
        origin: [255; 32],
        index: 65536,
        head: [255; 32],
        version: after.clone(),
    };
    let previous = LineageRef::Present {
        origin: [255; 32],
        index: 65535,
        head: [255; 32],
        version: before.clone(),
    };
    let cut = ReviewCut {
        progress_revision: u64::MAX - 2,
        schedule_revision: u64::MAX - 1,
        control_sequence: u64::MAX - 2,
        current_digest: [255; 32],
    };
    let result = ReviewedBaselineResultV1 {
        version: 1,
        key: key.clone(),
        cut: cut.clone(),
        witness: CommitWitness {
            operation,
            precommit_revision: u64::MAX - 2,
            result_revision: u64::MAX - 1,
            intent_digest: [255; 32],
            admission_digest: [255; 32],
            budget: NativeBudget {
                version: 1,
                reserved: 1024 * 1024 * 1024,
                sequence: 256,
            },
            receipt: CommitReceiptV1::Directory {
                attempt: 8,
                outcome: [255; 32],
            },
            before: before.clone(),
            after: after.clone(),
            existing_ancestors: ReviewPatch::Present {
                value: ExistingAncestorPatchV1 {
                    version: 1,
                    admission: [255; 32],
                    prefixes: u64::MAX - 1,
                },
            },
            directory_ancestors: ReviewPatch::Absent {},
        },
        provenance: ReviewedResultProvenanceV1::ReviewStep {
            step: 4095,
            entry: 4095,
            entry_digest: [255; 32],
            selected: ReviewedSelectionV1::Chain {
                anchor: operation,
                digest: [255; 32],
                index: 63,
                final_member: false,
            },
        },
        lineage: lineage.clone(),
    };
    let step = InitialReviewStepCompletedV1 {
        version: 1,
        key: key.clone(),
        step: 4095,
        entry: 4095,
        entry_digest: [255; 32],
        first_operation: operation,
        terminal: ReviewTerminalV1::Completed {
            operation,
            commit_digest: [255; 32],
            archive: ReviewArchiveRefV1::Present {
                anchor: operation,
                core_digest: [255; 32],
            },
        },
        resulting_progress: u64::MAX - 1,
        schedule_revision: u64::MAX - 1,
        control_sequence: u64::MAX - 2,
    };
    let progress = InitialReviewProgressV1 {
        version: 1,
        key: key.clone(),
        completed_steps: 4096,
        covered: 4096,
        remaining: ReviewRemainingV1 {
            unselected: 0,
            structural: 0,
            conflict: 0,
            abandoned: 0,
        },
        coverage_digest: [255; 32],
        last_step: ReviewLastStepV1::Step {
            step: 4095,
            digest: [255; 32],
        },
        progress_revision: u64::MAX - 1,
    };
    let complete = InitialReviewCompletedV1 {
        version: 1,
        key: key.clone(),
        completed_steps: 4096,
        entries: 4096,
        coverage_digest: [255; 32],
        last_step_digest: [255; 32],
        baseline: after.clone(),
        lineage: lineage.clone(),
        progress_revision: u64::MAX - 1,
        schedule_revision: u64::MAX - 1,
        control_sequence: u64::MAX - 2,
    };
    let post = PostReviewOperationV1 {
        version: 1,
        key: key.clone(),
        completion_digest: [255; 32],
        operation,
        intent_digest: [255; 32],
        admission_digest: [255; 32],
        expected: u64::MAX - 1,
        baseline: after.clone(),
        lineage: lineage.clone(),
        kind: PostReviewKindV1::SelectedRestore {
            action_digest: [255; 32],
        },
    };
    let inner = ReviewedInnerControlV1 {
        connection: key.connection,
        anchor: operation,
        digest: [255; 32],
        expected_progress: u64::MAX - 2,
        expected_schedule: u64::MAX - 2,
        control_sequence: u64::MAX - 2,
        transition: DirectoryChainControlTransitionV1::Retire,
    };
    let endpoints = std::array::from_fn(|_| storage::Identity {
        volume: [255; 32],
        root: [255; 32],
    });
    let archive = ChainCompletionBaselineV1 {
        core: ArchiveCore {
            version: 1,
            key: key.clone(),
            endpoints: endpoints.clone(),
            fence: u64::MAX,
            anchor: operation,
            digest: [255; 32],
            link_digest: [255; 32],
            retirement: DirectoryChainRetirementV1 {
                reason: ChainRetirementReason::Abandoned,
                cursor: DirectoryChainCursorV1 {
                    anchor: operation,
                    digest: [255; 32],
                    next_index: 63,
                    current: Some(operation),
                    predecessor: Some(ChainOperationIdV1 {
                        serial: u64::MAX - 1,
                        ..operation
                    }),
                    progress_revision: u64::MAX - 2,
                },
            },
            baseline: before.clone(),
            created: ArchiveCreatedV1::Retirement {
                before: ReviewCut {
                    schedule_revision: u64::MAX - 2,
                    control_sequence: u64::MAX - 3,
                    ..cut
                },
                inner,
                result: ReviewResultCutV1 {
                    progress_revision: u64::MAX - 2,
                    schedule_revision: u64::MAX - 1,
                    control_sequence: u64::MAX - 2,
                },
            },
        },
        ordinal: 4096,
        previous_set: [255; 32],
        set: [255; 32],
        lineage: previous.clone(),
    };
    let head = ChainBaselineLineageV1 {
        version: 1,
        key: key.clone(),
        endpoints: endpoints.clone(),
        fence: u64::MAX,
        origin: [255; 32],
        origin_version: before.clone(),
        origin_archive: OriginArchiveV1 {
            anchor: operation,
            core_digest: [255; 32],
        },
        head_index: 65536,
        head_digest: [255; 32],
        current_version: after.clone(),
        archives: 4096,
        set: [255; 32],
    };
    let edge = ChainBaselineSupersessionV1 {
        version: 1,
        key: key.clone(),
        endpoints,
        fence: u64::MAX,
        index: 65536,
        previous,
        operation,
        witness_digest: [255; 32],
        admission_digest: [255; 32],
        receipt_digest: [255; 32],
        result: after.clone(),
        archives_before: 4095,
        set_before: [255; 32],
        archives_after: 4096,
        set_after: [255; 32],
        head: [255; 32],
    };
    let candidate = ReviewedCandidateRootsV1 {
        version: 1,
        connection: key.connection,
        expected: u64::MAX - 2,
        checkpoint: PageRef::Page { hash: [255; 32] },
        review: PageRef::Page { hash: [255; 32] },
        before,
        after,
    };
    vec![
        Row::ReviewedBaselineResultV1(result),
        Row::InitialReviewStepCompletedV1(step),
        Row::InitialReviewProgressV1(progress),
        Row::InitialReviewCompletedV1(complete),
        Row::PostReviewOperationV1(post),
        Row::ChainCompletionBaselineV1(archive),
        Row::ChainBaselineLineageV1(head),
        Row::ChainBaselineSupersessionV1(edge),
        Row::ReviewedCandidateRootsV1(candidate),
    ]
}
fn closed_objects(value: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match value {
        serde_json::Value::Object(fields) => {
            out.push(path.clone());
            for (key, value) in fields {
                path.push(key.clone());
                closed_objects(value, path, out);
                path.pop();
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                if value.is_object() || value.is_array() {
                    path.push(index.to_string());
                    closed_objects(value, path, out);
                    path.pop();
                }
            }
        }
        _ => (),
    }
}
#[test]
fn retired_snapshot_strict_wire_maximum_fixed_rows_and_caps() {
    let rows = maximum_review_rows();
    for (row, table) in rows.iter().zip(12u8..=20) {
        row.validate().unwrap();
        assert_eq!(row.key()[0], table);
        let bytes = format::encode(row).unwrap();
        assert_eq!(serde_json::from_slice::<Row>(&bytes).unwrap(), *row);
        let page = format::encode(&format::PageNode {
            schema: 1,
            node: format::Node::Leaf(vec![row.clone()]),
        })
        .unwrap();
        assert!(page.len() < 8192);
        let value = serde_json::to_value(row).unwrap();
        let mut objects = Vec::new();
        closed_objects(&value, &mut Vec::new(), &mut objects);
        for path in objects {
            let mut bad = value.clone();
            let mut at = &mut bad;
            for part in path {
                at = if at.is_array() {
                    &mut at[part.parse::<usize>().unwrap()]
                } else {
                    &mut at[part.as_str()]
                };
            }
            at.as_object_mut()
                .unwrap()
                .insert("unknown".into(), serde_json::Value::Null);
            assert!(
                serde_json::from_value::<Row>(bad).is_err(),
                "table {table} rejects every nested unknown field"
            );
        }
        let raw = String::from_utf8(bytes.clone()).unwrap();
        let duplicate = raw.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
        assert!(serde_json::from_str::<Row>(&duplicate).is_err());
        let mut missing = value.clone();
        let first = missing["value"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        missing["value"].as_object_mut().unwrap().remove(&first);
        assert!(serde_json::from_value::<Row>(missing).is_err());
        eprintln!(
            "slice2 fixed_max table={table} row_bytes={} singleton_page_bytes={}",
            bytes.len(),
            page.len()
        );
    }
    let Row::ReviewedBaselineResultV1(result) = rows[0].clone() else {
        panic!()
    };
    let Row::ChainCompletionBaselineV1(archive) = rows[5].clone() else {
        panic!()
    };
    let format::ArchiveCreatedV1::Retirement { before, inner, .. } = archive.core.created.clone()
    else {
        panic!()
    };
    let body = format::ReviewedBaselineBodyV1 {
        directory_chain: Some(format::DirectoryChainCommitV1 {
            member: format::DirectoryChainRefV1 {
                anchor: result.witness.operation,
                digest: [255; 32],
                index: 63,
                predecessor: Some(result.witness.operation),
            },
            expected_progress: u64::MAX - 2,
        }),
        directory_ancestors: None,
        existing_ancestors: Some(format::ExistingAncestorPatchV1 {
            version: 1,
            admission: [255; 32],
            prefixes: u64::MAX - 1,
        }),
        connection: result.key.connection,
        expected: u64::MAX - 2,
        root: Some([255; 32]),
        count: 4096,
        source: archive.core.endpoints[0].clone(),
        destination: archive.core.endpoints[1].clone(),
        initial_review: false,
    };
    let provenance = format::ReviewedBaselineProvenanceV1::ReviewedStep {
        link_digest: [255; 32],
        operation: result.witness.operation,
        step: 4095,
        entry: 4095,
        expected_progress: u64::MAX - 2,
        expected_schedule: u64::MAX - 1,
        control_sequence: u64::MAX - 2,
        lineage: result.lineage,
    };
    for change in [
        Change::ReviewedBaselineCommit { body, provenance },
        Change::ReviewedChainRetirement {
            key: result.key,
            before,
            inner,
            baseline: archive.core.baseline,
            lineage: archive.lineage,
        },
    ] {
        let event = Event {
            schema: 1,
            predecessor: [255; 32],
            change,
        };
        let bytes = format::encode(&event).unwrap();
        let value = serde_json::to_value(&event.change).unwrap();
        let mut objects = Vec::new();
        closed_objects(&value, &mut Vec::new(), &mut objects);
        for path in objects {
            let mut bad = value.clone();
            let mut at = &mut bad;
            for part in path {
                at = if at.is_array() {
                    &mut at[part.parse::<usize>().unwrap()]
                } else {
                    &mut at[part.as_str()]
                };
            }
            at.as_object_mut()
                .unwrap()
                .insert("unknown".into(), serde_json::Value::Null);
            assert!(serde_json::from_value::<Change>(bad).is_err());
        }
        eprintln!(
            "slice2 fixed_max event={} bytes={} framed={}",
            value["event"],
            bytes.len(),
            bytes.len() + 44
        );
    }
    let Row::ChainBaselineSupersessionV1(mut edge) = rows[7].clone() else {
        panic!()
    };
    edge.index = 65537;
    assert!(edge.validate().is_err());
    let Row::ChainCompletionBaselineV1(mut archive) = rows[5].clone() else {
        panic!()
    };
    archive.ordinal = 4097;
    assert!(archive.validate().is_err());
    let Row::InitialReviewProgressV1(mut progress) = rows[2].clone() else {
        panic!()
    };
    progress.completed_steps = 4097;
    assert!(progress.validate().is_err());
    let Row::ReviewedCandidateRootsV1(mut candidate) = rows[8].clone() else {
        panic!()
    };
    candidate.after = format::BaselineVersion::Present {
        revision: u64::MAX - 1,
        root: format::PageRef::Page { hash: [255; 32] },
        count: 4097,
    };
    assert!(candidate.validate().is_err());
    let mut cursor = inert_cursor(ConnectionId([255; 16]));
    cursor.next_step = 4096;
    cursor.current = format::InitialReviewCurrentV1::StepLimit {
        progress_digest: [255; 32],
    };
    cursor.validate().unwrap();
    cursor.next_step = 4097;
    assert!(cursor.validate().is_err());
}

fn complete_supported_review(store: &mut ConnectionStore, record: &ConnectionRecord) {
    accept_supported_review(store, record);
    for ordinal in [0, 2, 1] {
        let intent = if ordinal == 2 {
            equality_intent(store, record.id)
        } else {
            addition_intent_at(store, record.id, ordinal)
        };
        historical_append(store, intent).unwrap();
        finish_reviewed_pending(store, record.id);
    }
}
#[test]
fn retired_snapshot_active_equality_saved_cut_after_completed_step_refuses() {
    let (_temp, path, mut store, record) = fixture();
    accept_supported_review(&mut store, &record);
    let intent = addition_intent_at(&store, record.id, 0);
    historical_append(&mut store, intent).unwrap();
    finish_reviewed_pending(&mut store, record.id);
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let id = OperationId {
        connection: record.id,
        serial: 2,
    };
    let op = store.operation(id).unwrap();
    let digest = format::digest(&format::encode(&op).unwrap());
    let stop = outer_control(
        &store,
        record.id,
        format::InitialReviewControlTransitionV1::StopRequested {
            reason: format::PauseOrCancel::Pause,
        },
    );
    historical_append(&mut store, stop).unwrap();
    let boundary = equality_boundary(&store, record.id, digest);
    historical_append(&mut store, boundary).unwrap();
    let key = format::key(11, record.id.0, 2);
    let mut bad = store.row(&key).unwrap().unwrap();
    let Row::InitialReviewStop(format::InitialReviewStopBoundaryV1::EqualityUntouched { fact }) =
        &mut bad
    else {
        panic!()
    };
    fact.stage = 3;
    bad.validate().unwrap();
    assert!(
        matches!(
            store.validate_review_history(&[bad]),
            Err(StoreError::Corrupt)
        ),
        "saved Verified cannot be projected from current Intent after a completed sibling"
    );
    let ack = outer_control(
        &store,
        record.id,
        format::InitialReviewControlTransitionV1::StopAcknowledged {},
    );
    historical_append(&mut store, ack).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    ConnectionStore::open_at(&path).unwrap();
}

#[test]
fn retired_snapshot_corrupt_checkpoint_closure_has_no_legacy_fallback() {
    let (_temp, path, mut store, record) = fixture();
    complete_supported_review(&mut store, &record);
    let Change::ReviewedIntent { operation, .. } = equality_intent(&store, record.id) else {
        panic!()
    };
    let intent = continuation_intent(&store, record.id, operation);
    historical_append(&mut store, intent).unwrap();
    finish_reviewed_pending(&mut store, record.id);
    store.checkpoint().unwrap();
    let original = store
        .rows()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let current_bytes = std::fs::read(path.join("CURRENT")).unwrap();
    let mut cases: Vec<(String, Vec<Row>)> = Vec::new();
    for table in [7, 9, 10, 12, 13, 14, 15, 16, 17, 18, 19] {
        let mut rows = original.clone();
        let at = rows.iter().position(|r| r.key()[0] == table).unwrap();
        rows.remove(at);
        cases.push((format!("missing-{table}"), rows));
    }
    for damage in 0..25 {
        let mut rows = original.clone();
        let table = match damage {
            0..=4 => 12,
            5 => 13,
            6 => 14,
            7 => 15,
            8..=9 => 16,
            10..=16 => 17,
            17..=19 => 18,
            _ => 19,
        };
        let row = rows.iter_mut().find(|r| r.key()[0] == table).unwrap();
        match row {
            Row::ReviewedBaselineResultV1(r) => match damage {
                0 => r.witness.intent_digest[0] ^= 1,
                1 => r.witness.admission_digest[0] ^= 1,
                2 => {
                    let format::LineageRef::Present { head, .. } = &mut r.lineage else {
                        panic!()
                    };
                    head[0] ^= 1;
                }
                3 => r.cut.current_digest[0] ^= 1,
                _ => {
                    r.witness.budget.reserved += format::READ_QUANTUM;
                    r.witness.budget.sequence += 1;
                }
            },
            Row::InitialReviewStepCompletedV1(r) => r.entry_digest[0] ^= 1,
            Row::InitialReviewProgressV1(r) => r.coverage_digest[0] ^= 1,
            Row::InitialReviewCompletedV1(r) => r.last_step_digest[0] ^= 1,
            Row::PostReviewOperationV1(r) => {
                if damage == 8 {
                    r.completion_digest[0] ^= 1
                } else {
                    r.intent_digest[0] ^= 1
                }
            }
            Row::ChainCompletionBaselineV1(r) => match damage {
                10 => r.core.endpoints[0].root[0] ^= 1,
                11 => r.core.fence += 1,
                12 => r.core.link_digest[0] ^= 1,
                13 => r.set[0] ^= 1,
                14 => r.core.digest[0] ^= 1,
                15 => {
                    let format::BaselineVersion::Present { root, .. } = &mut r.core.baseline else {
                        panic!()
                    };
                    *root = format::PageRef::Page { hash: [241; 32] };
                }
                _ => r.core.retirement.cursor.progress_revision += 1,
            },
            Row::ChainBaselineLineageV1(r) => match damage {
                17 => r.origin[0] ^= 1,
                18 => r.head_digest[0] ^= 1,
                _ => r.origin_archive.core_digest[0] ^= 1,
            },
            Row::ChainBaselineSupersessionV1(r) => match damage {
                20 => r.witness_digest[0] ^= 1,
                21 => r.admission_digest[0] ^= 1,
                22 => r.receipt_digest[0] ^= 1,
                23 => r.head[0] ^= 1,
                _ => r.set_after[0] ^= 1,
            },
            _ => panic!(),
        }
        row.validate()
            .unwrap_or_else(|e| panic!("local mutation {damage}: {e:?}"));
        cases.push((format!("changed-{damage}-table-{table}"), rows));
    }
    // Hash-valid pages alone do not establish archive ownership or lineage order.
    for table in [12, 13, 16, 17, 19] {
        let mut rows = original.clone();
        let mut orphan = rows.iter().find(|r| r.key()[0] == table).unwrap().clone();
        match &mut orphan {
            Row::ReviewedBaselineResultV1(r) => r.witness.operation.serial += 100,
            Row::InitialReviewStepCompletedV1(r) => r.step += 100,
            Row::PostReviewOperationV1(r) => r.operation.serial += 100,
            Row::ChainCompletionBaselineV1(r) => r.core.anchor.serial += 100,
            Row::ChainBaselineSupersessionV1(r) => {
                r.index += 100;
                let format::LineageRef::Present { index, .. } = &mut r.previous else {
                    panic!()
                };
                *index += 100;
            }
            _ => panic!(),
        };
        orphan
            .validate()
            .unwrap_or_else(|e| panic!("orphan-{table}: {e:?}"));
        rows.push(orphan);
        rows.sort_by_key(Row::key);
        cases.push((format!("orphan-{table}"), rows));
    }
    let count = cases.len();
    drop(store);
    for (name, rows) in cases {
        let store = ConnectionStore::open_at(&path).unwrap();
        let mut current = store.current.clone();
        current.root = storage::build_pages(&store.storage, rows.into_iter().map(Ok)).unwrap();
        store
            .publish_current(&current, &store.log, &store.log_identity)
            .unwrap();
        drop(store);
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "empty-log open accepted {name}"
        );
        std::fs::write(path.join("CURRENT"), &current_bytes).unwrap();
    }
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let mut bad = store
        .row(&format::key(18, record.id.0, 1))
        .unwrap()
        .unwrap();
    let Row::ChainBaselineLineageV1(head) = &mut bad else {
        panic!()
    };
    head.head_digest[0] ^= 1;
    assert!(store.validate_review_history(&[bad.clone()]).is_err());
    store.overlay.insert(bad.key(), bad);
    assert!(store.checkpoint().is_err());
    eprintln!(
        "slice2 empty-log corrupt closure cases={count}; prospective/checkpoint refusal=1 each"
    );
}

#[test]
fn retired_snapshot_retention_all_sources_and_missing_archived_page() {
    let (_temp, path, mut store, record) = fixture();
    complete_supported_review(&mut store, &record);
    let Some(Row::ChainCompletionBaselineV1(archive)) =
        store.row(&format::key(17, record.id.0, 1)).unwrap()
    else {
        panic!()
    };
    let old = archive.core.baseline.root().unwrap();
    assert_ne!(
        Some(old),
        store.connection(record.id).unwrap().baseline_root
    );
    assert!(store.page_referenced(old).unwrap());
    // Isolate the actual earlier active-log dependency while retaining the real
    // framed events. This is a read-only source probe, not a published state.
    let overlay = std::mem::take(&mut store.overlay);
    let main = store.current.root.take();
    assert!(
        store.page_referenced(old).unwrap(),
        "earlier reviewed event retains its candidate after overlay supersession"
    );
    store.overlay = overlay;
    store.current.root = main;
    store.collect_garbage().unwrap();
    store.checkpoint().unwrap();
    assert!(
        store.page_referenced(old).unwrap(),
        "empty active log retains archive through current checkpoint"
    );
    let Change::ReviewedIntent { operation, .. } = equality_intent(&store, record.id) else {
        panic!()
    };
    let intent = continuation_intent(&store, record.id, operation);
    historical_append(&mut store, intent).unwrap();
    let c = store.connection(record.id).unwrap();
    historical_append(
        &mut store,
        Change::Verified {
            connection: record.id.0,
            serial: c.pending.unwrap(),
            expected: c.revision,
            source: c.source.identity,
            destination: c.destination.identity,
        },
    )
    .unwrap();
    let change = equality_commit(&store, record.id);
    store.candidate_pin = store.prepare_candidate_pin(&change, None).unwrap();
    let pin = store.candidate_pin.unwrap();
    let page = store.storage.page(pin).unwrap();
    let format::Node::Leaf(rows) = page.node else {
        panic!()
    };
    assert_eq!(rows.len(), 1);
    let roots = rows[0].review_roots().unwrap();
    assert!(roots.len() <= 4);
    assert!(
        !roots.contains(&old),
        "archive page is an indirect dependency of the candidate's checkpoint"
    );
    let overlay = std::mem::take(&mut store.overlay);
    let main = store.current.root.take();
    assert!(
        store.page_referenced(old).unwrap(),
        "candidate checkpoint child follows archived semantic roots"
    );
    store.overlay = overlay;
    store.current.root = main;
    store.collect_garbage().unwrap();
    store.storage.page(old).unwrap();
    store.candidate_pin = None;
    historical_append(&mut store, change).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.row(&format::key(17, record.id.0, 1)).unwrap(),
        Some(Row::ChainCompletionBaselineV1(archive))
    );
    drop(store);
    std::fs::remove_file(path.join("pages").join(format::hex(&old))).unwrap();
    assert!(
        ConnectionStore::open_at(&path).is_err(),
        "missing archived baseline cannot fall back to today's baseline"
    );
}

fn final_supported_commit(store: &mut ConnectionStore, record: &ConnectionRecord) -> Change {
    accept_supported_review(store, record);
    for ordinal in [0, 2] {
        let intent = if ordinal == 2 {
            equality_intent(store, record.id)
        } else {
            addition_intent_at(store, record.id, ordinal)
        };
        historical_append(store, intent).unwrap();
        finish_reviewed_pending(store, record.id);
    }
    let intent = addition_intent_at(store, record.id, 1);
    historical_append(store, intent).unwrap();
    super::super::chain_tests::advance_in_place(store, record.id, 3);
    reviewed_commit(
        store,
        record.id,
        super::super::chain_tests::baseline_change(store, record.id),
    )
}
#[test]
fn retired_snapshot_actual_atomic_batch_and_event_capacity() {
    let (_temp, _path, mut store, record) = fixture();
    let change = final_supported_commit(&mut store, &record);
    store.candidate_pin = store.prepare_candidate_pin(&change, None).unwrap();
    store.checkpoint().unwrap();
    let event = Event {
        schema: 1,
        predecessor: store.digest,
        change,
    };
    let mut checks = store.replay_checks(&event.change).unwrap();
    let rows = store.reduce(&event, &mut checks).unwrap();
    let prepared = store.prepare_rows(rows.clone()).unwrap();
    assert!(prepared.fits_overlay());
    assert_eq!(
        rows.iter()
            .map(Row::key)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        rows.len()
    );
    let tables: Vec<_> = rows.iter().map(|r| r.key()[0]).collect();
    for table in [0, 1, 9, 12, 13, 14, 15, 17, 18, 19] {
        assert!(
            tables.contains(&table),
            "atomic final batch missing table {table}"
        );
    }
    let batch_bytes: usize = rows.iter().map(|r| format::encode(r).unwrap().len()).sum();
    let mut duplicates = rows.clone();
    duplicates.push(rows[0].clone());
    assert!(matches!(
        store.prepare_rows(duplicates),
        Err(StoreError::Corrupt)
    ));
    let encoded = format::encode(&event).unwrap();
    let framed = encoded.len() as u64 + 44;
    let length = store.log.metadata().unwrap().len();
    store.log.set_len(15 * 1024 * 1024 - framed).unwrap();
    assert!(store.check_event_capacity(&event, encoded.len()).is_ok());
    store.log.set_len(15 * 1024 * 1024 - framed + 1).unwrap();
    assert!(matches!(
        store.check_event_capacity(&event, encoded.len()),
        Err(StoreError::LimitExceeded)
    ));
    store.log.set_len(length).unwrap();
    store.log_identity = storage::regular(&store.log).unwrap();
    // Physical overlay capacity uses actual encoded, locally validated rows.
    // These synthetic independent-connection rows are never published as a history.
    let (_filler_temp, _filler_path, filler_store, filler_record) = fixture();
    let base = filler_store.connection(filler_record.id).unwrap();
    let filler = |index: u32, length: usize| {
        let mut c = base.clone();
        c.id = [211; 16];
        c.id[..4].copy_from_slice(&index.to_be_bytes());
        let mut locator = vec![b'x'; length.max(1)];
        locator[0] = b'/';
        c.source.locator = format::NativePath::Unix(locator);
        let row = Row::Connection(c);
        row.validate().unwrap();
        row
    };
    let mut index = 0u32;
    while store.prepare_rows(rows.clone()).unwrap().fits_overlay() {
        let available = 8 * 1024 * 1024 - store.overlay_bytes;
        let target = available.saturating_sub(batch_bytes / 2);
        let empty_size = format::encode(&filler(index, 1)).unwrap().len();
        let locator = ((target.saturating_sub(empty_size)) / 4 + 1).clamp(1, 30000);
        let next = store.prepare_rows(vec![filler(index, locator)]).unwrap();
        assert!(next.fits_overlay());
        store.commit_rows(next);
        index += 1;
        assert!(index < 1024);
    }
    assert!(store.overlay_bytes <= 8 * 1024 * 1024);
    let refused = store.prepare_rows(rows.clone()).unwrap();
    assert!(refused.overlay_bytes > 8 * 1024 * 1024);
    assert!(refused.overlay_keys < 1024);
    eprintln!(
        "slice2 final atomic rows={} tables={tables:?} row_bytes={batch_bytes} event_bytes={} framed={framed}; actual overlay keys={} bytes={} proposed_bytes={}",
        rows.len(),
        encoded.len(),
        store.overlay.len(),
        store.overlay_bytes,
        refused.overlay_bytes
    );
    store.overlay.clear();
    store.overlay_bytes = 0;
    store.connections = 1;
    store.live_keys = store.rows().unwrap().count() as u64;
    store.active_intents = 1;
    let fill: Vec<_> = (0..(1024 - rows.len() + 1))
        .map(|i| filler(i as u32, 1))
        .collect();
    let prepared = store.prepare_rows(fill).unwrap();
    assert!(prepared.fits_overlay());
    store.commit_rows(prepared);
    let refused = store.prepare_rows(rows).unwrap();
    assert_eq!(refused.overlay_keys, 1025);
    assert!(!refused.fits_overlay());
    eprintln!(
        "slice2 atomic key boundary proposed_keys={} bytes={}",
        refused.overlay_keys, refused.overlay_bytes
    );
    // The complete variable wrapper, including its existing operation body, is
    // checked by the real encoder; standalone field widths do not imply it fits.
    let mut operation = store
        .operation(OperationId {
            connection: record.id,
            serial: store.connection(record.id).unwrap().pending.unwrap(),
        })
        .unwrap();
    operation.native_identity = Some(vec![255; 32768]);
    let Row::PostReviewOperationV1(provenance) = maximum_review_rows()[4].clone() else {
        panic!()
    };
    let oversized = Event {
        schema: 1,
        predecessor: store.digest,
        change: Change::ReviewedContinuationIntent {
            operation,
            provenance,
            expected_progress: 1,
            expected_schedule: 1,
            control_sequence: 0,
        },
    };
    assert!(matches!(
        format::encode(&oversized),
        Err(StoreError::LimitExceeded)
    ));
}

#[test]
fn retired_snapshot_atomic_completion_real_fault_cuts() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            super::super::tests::FAULT.with(|f| f.set(None));
        }
    }
    let mut refused = 0;
    let mut durable = 0;
    let mut old = 0;
    let mut new = 0;
    let mut cuts = 0;
    for point in 0..96 {
        cuts += 1;
        let (_temp, path, mut store, record) = fixture();
        let change = final_supported_commit(&mut store, &record);
        let before = store.connection(record.id).unwrap();
        let last = before.pending.unwrap();
        // Only checkpoint triggering is injected. Candidate, page writes, real
        // framing/fsync and every recovered row use the common production path.
        store.overlay_bytes = 8 * 1024 * 1024;
        let reset = Reset;
        super::super::tests::FAULT.with(|f| f.set(Some((point, false))));
        let result = historical_append(&mut store, change);
        let failed = result.is_err();
        eprintln!("slice2 fault point={point} result={result:?}");
        assert!(matches!(
            result,
            Ok(()) | Err(StoreError::Persistence(_) | StoreError::RecoverySpace)
        ));
        refused += usize::from(failed);
        drop(reset);
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let c = store.connection(record.id).unwrap();
        let op = store
            .operation(OperationId {
                connection: record.id,
                serial: last,
            })
            .unwrap();
        match op.stage {
            3 => {
                old += 1;
                assert_eq!(c, before);
                for table in [12, 17] {
                    assert!(
                        store
                            .row(&format::key(table, record.id.0, last))
                            .unwrap()
                            .is_none()
                    );
                }
                assert!(
                    store
                        .row(&format::key(15, record.id.0, 1))
                        .unwrap()
                        .is_none()
                );
            }
            4 => {
                new += 1;
                durable += usize::from(failed);
                assert!(c.pending.is_none());
                for (table, serial) in [(12, last), (15, 1), (17, last), (18, 1)] {
                    assert!(
                        store
                            .row(&format::key(table, record.id.0, serial))
                            .unwrap()
                            .is_some()
                    );
                }
                store.validate_review_history(&[]).unwrap();
                assert!(store.transaction_session(record.id).is_err());
            }
            _ => panic!("partial canonical operation at fault {point}"),
        }
        let expected = if op.stage == 3 {
            Some(OperationId {
                connection: record.id,
                serial: last,
            })
        } else {
            None
        };
        assert_eq!(
            store.executor_position(record.id).unwrap(),
            (
                expected,
                Some(OperationId {
                    connection: record.id,
                    serial: last
                })
            )
        );
        if !failed {
            break;
        }
    }
    assert!(refused >= 4);
    assert!(old > 0);
    assert!(new > 0);
    assert!(
        durable > 0,
        "post-fsync failure must leave one whole replayable completion"
    );
    eprintln!(
        "slice2 fault cuts={cuts} refused={refused} old_complete={old} new_complete={new} durable_after_error={durable}"
    );
}

#[test]
fn retired_snapshot_abandoned_archive_requires_closed_retirement_cut() {
    use format::DirectoryChainControlTransitionV1 as T;
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let intent = addition_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    let abandon = reviewed_retirement(&store, record.id, T::AbandonRequested);
    historical_append(&mut store, abandon).unwrap();
    let c = store.connection(record.id).unwrap();
    let op = store
        .operation(OperationId {
            connection: record.id,
            serial: c.pending.unwrap(),
        })
        .unwrap();
    historical_append(
        &mut store,
        Change::NativeBoundary {
            connection: record.id.0,
            serial: op.serial,
            expected: c.revision,
            operation_expected: op.revision,
            fence: c.fence,
            kind: 0,
            evidence: None,
        },
    )
    .unwrap();
    let retire = reviewed_retirement(&store, record.id, T::Retire);
    historical_append(&mut store, retire).unwrap();
    let Some(Row::ChainCompletionBaselineV1(archive)) =
        store.row(&format::key(17, record.id.0, 1)).unwrap()
    else {
        panic!()
    };
    for damage in 0..5 {
        let mut bad = archive.clone();
        let format::ArchiveCreatedV1::Retirement { inner, result, .. } = &mut bad.core.created
        else {
            panic!()
        };
        match damage {
            0 => inner.control_sequence += 1,
            1 => inner.expected_progress += 1,
            2 => inner.transition = T::AbandonRequested,
            3 => result.control_sequence += 1,
            _ => result.progress_revision += 1,
        };
        assert!(
            bad.validate().is_err(),
            "archive local retirement cut must refuse damage={damage}"
        );
    }
}

#[test]
fn retired_snapshot_unfunded_append_refuses_before_write() {
    let (_temp, path, mut store, record) = fixture();
    let session = store.transaction_session(record.id).unwrap();
    let file = EntryState::File {
        sha256: [7; 32],
        bytes: 3,
        executable: false,
    };
    let mut live = store
        .begin_native_for_test(
            OperationIntent {
                connection: record.id,
                expected_revision: record.revision,
                expected_baseline: None,
                path: RelativePath::unix(b"equal").unwrap(),
                source_before: Some(file.clone()),
                destination_before: Some(file),
                decision: Decision::VerifyEqual,
            },
            &session,
        )
        .unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let context = store.native_read_context(&live, deadline, &cancel).unwrap();
    assert_eq!(remaining_credit(&live.reads), 0);
    let change = |store: &ConnectionStore, expected| {
        let c = store.connection(record.id).unwrap();
        Change::Verified {
            connection: record.id.0,
            serial: live.id.serial,
            expected,
            source: c.source.identity,
            destination: c.destination.identity,
        }
    };
    let event = change(&store, live.revision);
    let before_log = std::fs::read(path.join(format!("log-{}", store.current.log))).unwrap();
    let before_operation = store.operation(live.id).unwrap();
    assert!(matches!(
        store.append_native(event, &context),
        Err(StoreError::LimitExceeded)
    ));
    assert_eq!(
        before_log,
        std::fs::read(path.join(format!("log-{}", store.current.log))).unwrap()
    );
    assert_eq!(before_operation, store.operation(live.id).unwrap());
    assert_eq!(remaining_credit(&live.reads), 0);
    store
        .reserve_native_reads(&mut live, &session, format::READ_QUANTUM, deadline, &cancel)
        .unwrap();
    let context = store.native_read_context(&live, deadline, &cancel).unwrap();
    let c = store.connection(record.id).unwrap();
    store
        .append_native(
            Change::Verified {
                connection: record.id.0,
                serial: live.id.serial,
                expected: live.revision,
                source: c.source.identity,
                destination: c.destination.identity,
            },
            &context,
        )
        .unwrap();
    assert_eq!(store.operation(live.id).unwrap().stage, 3);
    assert!(remaining_credit(&live.reads) < format::READ_QUANTUM);
}

#[test]
fn retired_snapshot_fix1_nested_first_equality_has_exact_ancestor_receipt() {
    use crate::connection::model::{Endpoint, NamePolicy, Observation, ObservedTree};
    let (_temp, path, mut store, record) = fixture();
    let file = EntryState::File {
        sha256: [6; 32],
        bytes: 19,
        executable: false,
    };
    let trees = [record.source.identity, record.destination.identity].map(|identity| {
        ObservedTree::new(
            Endpoint {
                identity,
                encoding: Encoding::Unix,
                names: NamePolicy::Exact,
            },
            [
                (RelativePath::unix(b"dir").unwrap(), EntryState::Directory),
                (RelativePath::unix(b"dir/file").unwrap(), file.clone()),
            ],
            Observation::Complete,
        )
        .unwrap()
    });
    let accepted = owner_for_trees(&mut store, &record, trees).unwrap();
    store.accept_initial_set(accepted).unwrap();
    let intent = equality_intent_path(&store, record.id, b"dir/file");
    historical_append(&mut store, intent).unwrap();
    let c = store.connection(record.id).unwrap();
    historical_append(
        &mut store,
        Change::Verified {
            connection: record.id.0,
            serial: 1,
            expected: c.revision,
            source: c.source.identity,
            destination: c.destination.identity,
        },
    )
    .unwrap();
    let c = store.connection(record.id).unwrap();
    let op = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let receipt = recovery::ancestor_patch(&op, &BTreeMap::new())
        .unwrap()
        .unwrap();
    assert_eq!(receipt.prefixes, 2);
    let candidate = |entries: Vec<(&[u8], EntryState)>| {
        storage::build_pages(
            &store.storage,
            entries
                .into_iter()
                .enumerate()
                .map(|(ordinal, (path, state))| {
                    Ok(Row::Baseline(format::BaselineEntry {
                        connection: record.id.0,
                        revision: c.revision + 1,
                        ordinal: ordinal as u32,
                        path: format::NativePath::Unix(path.to_vec()),
                        state: state.into(),
                    }))
                }),
        )
        .unwrap()
    };
    let good = reviewed_commit(
        &store,
        record.id,
        Change::BaselineCommit {
            directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: Some(receipt),
            connection: record.id.0,
            expected: c.revision,
            root: candidate(vec![
                (b"dir", EntryState::Directory),
                (b"dir/file", file.clone()),
            ]),
            count: 2,
            source: c.source.identity,
            destination: c.destination.identity,
            initial_review: false,
        },
    );
    let missing_parent = candidate(vec![(b"dir/file", file.clone())]);
    let extra_parent = candidate(vec![
        (b"dir", EntryState::Directory),
        (b"dir/file", file),
        (b"extra", EntryState::Directory),
    ]);
    for damage in 0..6 {
        let mut bad = good.clone();
        let Change::ReviewedBaselineCommit { body, .. } = &mut bad else {
            panic!()
        };
        match damage {
            0 => body.existing_ancestors = None,
            1 => body.existing_ancestors.as_mut().unwrap().admission[0] ^= 1,
            2 => body.existing_ancestors.as_mut().unwrap().prefixes += 1,
            3 => {
                body.root = missing_parent;
                body.count = 1;
            }
            4 => {
                body.root = extra_parent;
                body.count = 3;
            }
            _ => body.initial_review = true,
        }
        let before = store.connection(record.id).unwrap();
        assert!(
            historical_append(&mut store, bad).is_err(),
            "accepted nested ancestor damage {damage}"
        );
        assert_eq!(store.connection(record.id).unwrap(), before);
    }
    historical_append(&mut store, good)
        .expect("genuine nested equality receipt uses canonical patch");
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    let baseline = store.load_baseline(record.id).unwrap();
    assert_eq!(baseline.entries().unwrap().len(), 2);
    assert_eq!(
        baseline
            .entries()
            .unwrap()
            .get(&RelativePath::unix(b"dir").unwrap()),
        Some(&EntryState::Directory)
    );
    assert_eq!(
        store
            .operation(OperationId {
                connection: record.id,
                serial: 1
            })
            .unwrap()
            .stage,
        4
    );
}

#[test]
fn retired_snapshot_fix1_durable_inner_outer_control_join() {
    use format::{
        DirectoryChainScheduleState as S, InitialReviewControlTransitionV1 as T, PauseOrCancel as R,
    };
    for between in [false, true] {
        let (_temp, path, mut store, record) = fixture();
        accept_nested_review(&mut store, &record, true);
        let intent = equality_intent(&store, record.id);
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
        let intent = addition_intent_at(&store, record.id, 1);
        historical_append(&mut store, intent).unwrap();
        if between {
            finish_reviewed_pending(&mut store, record.id);
        }
        for stopped in [false, true] {
            if stopped {
                let control =
                    chain_control(&store, record.id, T::StopRequested { reason: R::Pause });
                historical_append(&mut store, control).unwrap();
                // Between-member stops have no active native member to classify.
                if between {
                    let control = chain_control(&store, record.id, T::StopAcknowledged {});
                    historical_append(&mut store, control).unwrap();
                }
            }
            store.checkpoint().unwrap();
            let rows = store
                .rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let current_bytes = std::fs::read(path.join("CURRENT")).unwrap();
            for damage in 0..3 {
                let mut bad = rows.clone();
                let Row::InitialReviewCursor(cursor) =
                    bad.iter_mut().find(|r| r.key()[0] == 9).unwrap()
                else {
                    panic!()
                };
                match damage {
                    0 => {
                        cursor.state = if stopped {
                            S::Running
                        } else {
                            S::Stopping { reason: R::Pause }
                        }
                    }
                    1 => {
                        cursor.state = if stopped && between {
                            S::Stopped { reason: R::Cancel }
                        } else {
                            S::Stopping { reason: R::Cancel }
                        }
                    }
                    _ => {
                        // Change only the inner progress to keep the outer operation/cursor joins valid.
                        let Row::Connection(c) = bad.iter_mut().find(|r| r.key()[0] == 0).unwrap()
                        else {
                            panic!()
                        };
                        c.directory_chain.as_mut().unwrap().progress_revision -= 1;
                    }
                }
                for row in &bad {
                    row.validate().unwrap();
                }
                let mut current = store.current.clone();
                current.root =
                    storage::build_pages(&store.storage, bad.into_iter().map(Ok)).unwrap();
                store
                    .publish_current(&current, &store.log, &store.log_identity)
                    .unwrap();
                drop(store);
                let reopened = ConnectionStore::open_at(&path);
                assert!(
                    reopened.is_err(),
                    "accepted state/progress damage {damage}, between={between}, stopped={stopped}"
                );
                std::fs::write(path.join("CURRENT"), &current_bytes).unwrap();
                store = ConnectionStore::open_at(&path).unwrap();
            }
        }
    }
}

fn fix1_paid_verified(with_history: bool) {
    let (_temp, path, mut store, record) = fixture();
    if with_history {
        complete_supported_review(&mut store, &record);
    }
    let other = store
        .create_connection(
            ConnectionId([34; 16]),
            record.source.clone(),
            record.destination.clone(),
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap()
        .value;
    let file = EntryState::File {
        sha256: [7; 32],
        bytes: 3,
        executable: false,
    };
    let intent = OperationIntent {
        connection: other.id,
        expected_revision: other.revision,
        expected_baseline: None,
        path: RelativePath::unix(b"other").unwrap(),
        source_before: Some(file.clone()),
        destination_before: Some(file.clone()),
        decision: Decision::VerifyEqual,
    };
    let c = store.connection(other.id).unwrap();
    let operation = format::Operation::new(intent.clone(), 1, &c).unwrap();
    store.append(Change::Intent { operation }).unwrap();
    let c = store.connection(other.id).unwrap();
    store
        .append(Change::Verified {
            connection: other.id.0,
            serial: 1,
            expected: c.revision,
            source: c.source.identity,
            destination: c.destination.identity,
        })
        .unwrap();
    let c = store.connection(other.id).unwrap();
    let root = storage::build_pages(
        &store.storage,
        [Ok(Row::Baseline(format::BaselineEntry {
            connection: other.id.0,
            revision: c.revision + 1,
            ordinal: 0,
            path: format::NativePath::Unix(b"other".to_vec()),
            state: file.into(),
        }))]
        .into_iter(),
    )
    .unwrap();
    historical_append(
        &mut store,
        Change::BaselineCommit {
            directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: other.id.0,
            expected: c.revision,
            root,
            count: 1,
            source: c.source.identity,
            destination: c.destination.identity,
            initial_review: true,
        },
    )
    .unwrap();
    let c = store.connection(other.id).unwrap();
    let session = store.transaction_session(other.id).unwrap();
    let mut live = store
        .begin_native_for_test(
            OperationIntent {
                expected_revision: c.revision,
                expected_baseline: c.baseline,
                ..intent
            },
            &session,
        )
        .unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    store
        .reserve_native_reads(&mut live, &session, 128 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    store.checkpoint().unwrap();
    assert!(store.overlay.is_empty());
    let context = store.native_read_context(&live, deadline, &cancel).unwrap();
    let c = store.connection(other.id).unwrap();
    let good = Change::Verified {
        connection: other.id.0,
        serial: live.id.serial,
        expected: c.revision,
        source: c.source.identity.clone(),
        destination: c.destination.identity.clone(),
    };
    let mut bad = good.clone();
    let Change::Verified { expected, .. } = &mut bad else {
        panic!()
    };
    *expected -= 1;
    for (name, change, success) in [("refused", bad, false), ("verified", good, true)] {
        let before = remaining_credit(&live.reads);
        let observer = storage::physical_trace::begin();
        let result = store.append_native(change, &context);
        let trace = observer.finish();
        let requested: u64 = trace.reads.iter().map(|r| r.requested as u64).sum();
        eprintln!(
            "fix1 paid {name} history={with_history} calls={} requested={requested} charged={} result={result:?}",
            trace.reads.len(),
            before - remaining_credit(&live.reads)
        );
        assert_eq!(result.is_ok(), success, "{name}: {result:?}");
        assert!(trace.reads.iter().any(|r| r.name != "CURRENT"));
        assert_eq!(
            requested,
            before - remaining_credit(&live.reads),
            "every canonical physical read must keep the payer"
        );
    }
    let before = remaining_credit(&live.reads);
    let observer = storage::physical_trace::begin();
    let lease = store.review_read_lease().unwrap();
    store.checkpoint_with_context(Some(&context)).unwrap();
    let trace = observer.finish();
    assert_eq!(
        trace.reads.iter().map(|r| r.requested as u64).sum::<u64>(),
        before - remaining_credit(&live.reads)
    );
    drop(lease);
    if with_history {
        let first = store.connection(record.id).unwrap();
        assert_ne!(first.baseline_root, c.baseline_root);
        let lease = store.review_read_lease().unwrap();
        let snapshot_rows = fix3_snapshot_rows(&store);
        fix3_paid_owner_checks(&store, &snapshot_rows, &context);
        assert!(
            matches!(
                store.read_baseline_with_context(
                    &first,
                    first.baseline_root,
                    first.baseline_count,
                    first.baseline.unwrap(),
                    Some(&context)
                ),
                Err(StoreError::StaleRevision)
            ),
            "ordinary active reader keeps payer/owner binding"
        );
        let mut wrong = first.clone();
        wrong.id = other.id.0;
        assert!(
            store
                .read_baseline_with_context(
                    &wrong,
                    first.baseline_root,
                    first.baseline_count,
                    first.baseline.unwrap(),
                    Some(&context)
                )
                .is_err()
        );
        assert!(
            store
                .read_historical_baseline_with_context(
                    &first,
                    c.baseline_root,
                    c.baseline_count,
                    c.baseline.unwrap(),
                    Some(&context)
                )
                .is_err()
        );
        assert!(
            store
                .read_historical_baseline_with_context(
                    &wrong,
                    first.baseline_root,
                    first.baseline_count,
                    first.baseline.unwrap(),
                    Some(&context)
                )
                .is_err()
        );
        let (_foreign_temp, _foreign_path, foreign, _) = fixture();
        let _foreign_lease = foreign.review_read_lease().unwrap();
        assert!(matches!(
            fix3_snapshot_read(&foreign, &snapshot_rows, Some(&context), |_| Ok(())),
            Err(StoreError::StaleRevision)
        ));
        assert!(matches!(
            foreign.read_historical_baseline_with_context(
                &first,
                first.baseline_root,
                first.baseline_count,
                first.baseline.unwrap(),
                Some(&context)
            ),
            Err(StoreError::StaleRevision)
        ));
        drop(lease);
        assert!(matches!(
            fix3_snapshot_read(&store, &snapshot_rows, Some(&context), |_| Ok(())),
            Err(StoreError::RecoveryRequired)
        ));
        assert!(matches!(
            store.read_historical_baseline_with_context(
                &first,
                first.baseline_root,
                first.baseline_count,
                first.baseline.unwrap(),
                Some(&context)
            ),
            Err(StoreError::RecoveryRequired)
        ));
        let expired = store.storage.read_lease(false).unwrap();
        expired.expire_for_test();
        assert!(matches!(
            fix3_snapshot_read(&store, &snapshot_rows, Some(&context), |_| Ok(())),
            Err(StoreError::LimitExceeded)
        ));
        drop(expired);
    }
    let remaining = remaining_credit(&live.reads);
    live.reads.charge(remaining).unwrap();
    assert_eq!(live.reads.charge(1), Err(()));
    if with_history {
        let lease = store.storage.read_lease(false).unwrap();
        let rows = fix3_snapshot_rows(&store);
        let observer = storage::physical_trace::begin();
        assert!(fix3_snapshot_read(&store, &rows, Some(&context), |_| Ok(())).is_err());
        assert!(
            observer.finish().reads.is_empty(),
            "zero credit refuses before physical requests"
        );
        assert_eq!(remaining_credit(&live.reads), 0);
        drop(lease);
    }
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    let _lease = store.review_read_lease().unwrap();
    assert!(matches!(
        store.validate_review_history_with_context(&[], Some(&context)),
        Err(StoreError::StaleRevision)
    ));
    if with_history {
        let rows = fix3_snapshot_rows(&store);
        assert!(matches!(
            fix3_snapshot_read(&store, &rows, Some(&context), |_| Ok(())),
            Err(StoreError::StaleRevision)
        ));
    }
    assert_eq!(store.operation(live.id).unwrap().stage, 3);
    assert_eq!(
        store
            .load_baseline(other.id)
            .unwrap()
            .entries()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn retired_snapshot_fix1_paid_verified_keeps_every_canonical_read() {
    fix1_paid_verified(false);
}
#[test]
fn retired_snapshot_fix1_other_connection_history_keeps_real_payer() {
    fix1_paid_verified(true);
}

fn fix1_assert_paid_reduction(
    store: &ConnectionStore,
    change: Change,
    context: &NativeReadContext<'_>,
) {
    let mut bad = change.clone();
    match &mut bad {
        Change::NativeBound { expected, .. } | Change::NativeOutcome { expected, .. } => {
            *expected -= 1
        }
        _ => panic!(),
    }
    for (name, change, success) in [("refused", bad, false), ("valid", change, true)] {
        let before = remaining_credit(&context.reads);
        let observer = storage::physical_trace::begin();
        let mut checks = RecoveryChecks {
            mode: VerificationMode::NativeLive(context),
            ..RecoveryChecks::default()
        };
        let result = store.reduce(
            &Event {
                schema: 1,
                predecessor: store.digest,
                change,
            },
            &mut checks,
        );
        let trace = observer.finish();
        let requested: u64 = trace.reads.iter().map(|r| r.requested as u64).sum();
        eprintln!(
            "fix1 canonical paid {name} calls={} requested={requested} charged={} result={:?}",
            trace.reads.len(),
            before - remaining_credit(&context.reads),
            result.as_ref().map(|r| r.len())
        );
        assert_eq!(result.is_ok(), success, "{name}: {result:?}");
        assert!(trace.reads.iter().any(|r| r.name != "CURRENT"));
        assert_eq!(requested, before - remaining_credit(&context.reads));
    }
    let remaining = remaining_credit(&context.reads);
    context.reads.charge(remaining).unwrap();
    assert_eq!(context.reads.charge(1), Err(()));
}

#[test]
fn retired_snapshot_fix1_paid_native_bound_keeps_every_join() {
    let (_temp, _path, mut store, record) = fixture();
    complete_supported_review(&mut store, &record);
    let intent = ordinary_edit_intent(
        &store,
        record.id,
        b"equal",
        0,
        b"initial content",
        Some(b"source edit"),
    );
    historical_append(&mut store, intent).unwrap();
    let parent = store
        .storage
        .recovery
        .create_child(std::ffi::OsStr::new(&format::hex(&record.id.0)))
        .unwrap();
    storage::sync_directory(&store.storage.recovery).unwrap();
    finish_ordinary_edit_observed(&mut store, record.id, &parent, b"initial content", true);
}
#[test]
fn retired_snapshot_fix1_paid_directory_outcome_keeps_every_join() {
    let (_temp, _path, mut store, record) = fixture();
    accept_nested_review(&mut store, &record, true);
    let intent = addition_intent_at(&store, record.id, 1);
    historical_append(&mut store, intent).unwrap();
    super::super::chain_tests::advance_in_place(&mut store, record.id, 2);
    let (payer, reads) = reserved_review_reads(&mut store, record.id);
    store.checkpoint().unwrap();
    let registration = store
        .rows()
        .unwrap()
        .map(Result::unwrap)
        .find_map(|r| match r {
            Row::NativeRegistration(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        registration.native_kind,
        Some(format::NativeOperationKind::DirectoryV1)
    );
    let expected = store.connection(record.id).unwrap().revision;
    let cancel = AtomicBool::new(false);
    let context = inspection_context(&store, payer, reads, &cancel);
    fix1_assert_paid_reduction(
        &store,
        Change::NativeOutcome {
            registration,
            expected,
            verified: true,
            reconstructed: false,
        },
        &context,
    );
}

// Compact retained history, not 4095 claimed physical appends. Every tuple uses
// the production codec/digest owners and must pass the entire production closure
// plus empty-log reopen before any terminal boundary assertion is reachable.
fn fix1_maximum_equality_prehistory() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    use crate::connection::model::{Endpoint, NamePolicy, Observation, ObservedTree};
    use crate::connection::transaction::native::descriptor as d;
    let (_temp, path, mut store, record) = fixture();
    let file = EntryState::File {
        sha256: [6; 32],
        bytes: 19,
        executable: false,
    };
    let trees = [record.source.identity, record.destination.identity].map(|identity| {
        ObservedTree::new(
            Endpoint {
                identity,
                encoding: Encoding::Unix,
                names: NamePolicy::Exact,
            },
            [b"equal".as_slice(), b"last".as_slice()]
                .map(|p| (RelativePath::unix(p).unwrap(), file.clone())),
            Observation::Complete,
        )
        .unwrap()
    });
    let accepted = owner_for_trees(&mut store, &record, trees).unwrap();
    store.accept_initial_set(accepted).unwrap();
    let change = equality_intent(&store, record.id);
    historical_append(&mut store, change).unwrap();
    finish_reviewed_pending(&mut store, record.id);
    store.checkpoint().unwrap();
    let started = std::time::Instant::now();
    let mut rows = store
        .rows()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            (r.key(), r)
        })
        .collect::<BTreeMap<_, _>>();
    let mut c = store.connection(record.id).unwrap();
    let mut cursor = store.review_cursor(record.id.0, 1).unwrap();
    let op_template = store
        .operation(OperationId {
            connection: record.id,
            serial: 1,
        })
        .unwrap();
    let Row::InitialReviewLink(link_template) = rows.get(&format::key(10, record.id.0, 1)).unwrap()
    else {
        panic!()
    };
    let link_template = link_template.clone();
    let Row::ReviewedBaselineResultV1(result_template) =
        rows.get(&format::key(12, record.id.0, 1)).unwrap()
    else {
        panic!()
    };
    let result_template = result_template.clone();
    let terminal_template = rows
        .values()
        .find_map(|r| match r {
            Row::InitialReviewStepCompletedV1(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    for step in 1..4095u32 {
        let mut op = op_template.clone();
        op.serial = u64::from(step) + 1;
        op.expected = c.revision;
        op.revision = c.revision + 3;
        op.baseline = c.baseline;
        let mut admission = d::Admission::decode(op.native_identity.as_ref().unwrap()).unwrap();
        admission.operation = op.serial;
        admission.revision = op.expected;
        admission.baseline = op.baseline;
        op.native_identity = Some(admission.encode().unwrap());
        op.decode().unwrap();
        let mut link = link_template.clone();
        link.step = step;
        link.operation.serial = op.serial;
        let before = format::BaselineVersion::from_connection(&c).unwrap();
        c.revision = op.revision;
        c.serial = op.serial;
        c.baseline = Some(op.revision);
        c.baseline_root = storage::build_pages(
            &store.storage,
            [Ok(Row::Baseline(format::BaselineEntry {
                connection: record.id.0,
                revision: op.revision,
                ordinal: 0,
                path: op.path.clone(),
                state: op.source.clone().unwrap(),
            }))]
            .into_iter(),
        )
        .unwrap();
        let mut result = result_template.clone();
        let format::ReviewedResultProvenanceV1::ReviewStep { step: n, .. } = &mut result.provenance
        else {
            panic!()
        };
        *n = step;
        result.cut.progress_revision = op.revision - 1;
        result.cut.current_digest = format::digest(&format::encode(&link.current()).unwrap());
        result.witness.operation = link.operation;
        result.witness.precommit_revision = op.revision - 1;
        result.witness.result_revision = op.revision;
        result.witness.intent_digest = super::original_intent_digest(&op).unwrap();
        result.witness.admission_digest = format::digest(op.native_identity.as_ref().unwrap());
        result.witness.before = before;
        result.witness.after = format::BaselineVersion::from_connection(&c).unwrap();
        let mut terminal = terminal_template.clone();
        terminal.step = step;
        terminal.first_operation = link.operation;
        terminal.resulting_progress = op.revision;
        terminal.terminal = format::ReviewTerminalV1::Completed {
            operation: link.operation,
            commit_digest: super::result_digest(&result).unwrap(),
            archive: format::ReviewArchiveRefV1::Absent {},
        };
        cursor.next_step = step + 1;
        cursor.progress_revision = c.revision;
        cursor.current = format::InitialReviewCurrentV1::BetweenSteps {
            last_step: step,
            completion_digest: super::terminal_digest(&terminal).unwrap(),
        };
        for row in [
            Row::Operation(op),
            Row::InitialReviewLink(link),
            Row::ReviewedBaselineResultV1(result),
            Row::InitialReviewStepCompletedV1(terminal),
        ] {
            row.validate().unwrap();
            rows.insert(row.key(), row);
        }
    }
    for row in [Row::Connection(c), Row::InitialReviewCursor(cursor.clone())] {
        rows.insert(row.key(), row);
    }
    let review = store.review_fact(record.id.0, 1).unwrap();
    let view = store.read_review(&review).unwrap();
    let lease = store.storage.read_lease(false).unwrap();
    let progress = store.review_coverage(&view, &rows, &cursor, None).unwrap();
    drop(lease);
    assert_eq!(progress.completed_steps, 4095);
    assert_eq!(progress.remaining.total().unwrap(), 1);
    let progress = Row::InitialReviewProgressV1(progress);
    rows.insert(progress.key(), progress);
    eprintln!(
        "fix1 maximum prehistory constructed steps=4095 rows={} elapsed={:?}",
        rows.len(),
        started.elapsed()
    );
    let lease = store.storage.read_lease(false).unwrap();
    let started = std::time::Instant::now();
    let replacements = rows.values().cloned().collect::<Vec<_>>();
    let closure = store.validate_chain_history(&replacements);
    eprintln!(
        "fix1 maximum prehistory closure result={closure:?} stats={:?} elapsed={:?}",
        lease.stats(),
        started.elapsed()
    );
    closure.expect("all4095 retained steps must pass full production closure");
    drop(lease);
    let mut current = store.current.clone();
    current.root = storage::build_pages(&store.storage, rows.into_values().map(Ok)).unwrap();
    store
        .publish_current(&current, &store.log, &store.log_identity)
        .unwrap();
    let lease = store.storage.read_lease(false).unwrap();
    let mut log = storage::read_file(&store.storage.root, &format!("log-{}", current.log)).unwrap();
    store.storage.read_log_magic(&mut log).unwrap();
    let header: format::LogHeader = store.storage.read_log_record(&mut log, 0).unwrap().unwrap();
    assert_eq!(header.schema, 1);
    assert_eq!(header.identity, store.storage.identity);
    assert_eq!(header.generation, current.generation);
    assert_eq!(
        header.first_sequence,
        format::next(current.sequence).unwrap()
    );
    assert_eq!(header.predecessor, current.digest);
    let header_end = log.stream_position().unwrap();
    // A torn frame also decodes as None, so prove no bytes follow the header
    // before asking the production framing reader for the next event.
    assert_eq!(header_end, log.metadata().unwrap().len());
    assert!(
        store
            .storage
            .read_log_record::<Event>(&mut log, format::next(current.sequence).unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(log.stream_position().unwrap(), header_end);
    drop(log);
    drop(lease);
    drop(store);
    let started = std::time::Instant::now();
    let reopened = ConnectionStore::open_at(&path);
    eprintln!(
        "fix1 maximum prehistory empty-log reopen result={:?} elapsed={:?}",
        reopened.as_ref().map(|_| ()),
        started.elapsed()
    );
    let store = reopened.expect("all4095 retained steps must reopen under unchanged finite bounds");
    (_temp, path, store, record)
}
fn fix1_maximum_boundary(complete: bool) {
    let (_temp, path, mut store, record) = fix1_maximum_equality_prehistory();
    let intent = equality_intent_path(&store, record.id, if complete { b"last" } else { b"equal" });
    historical_append(&mut store, intent).expect("real final Intent at4095");
    finish_reviewed_pending(&mut store, record.id);
    let cursor = store.review_cursor(record.id.0, 1).unwrap();
    assert_eq!(cursor.next_step, 4096);
    assert!(if complete {
        matches!(
            cursor.current,
            format::InitialReviewCurrentV1::ReviewComplete { .. }
        )
    } else {
        matches!(
            cursor.current,
            format::InitialReviewCurrentV1::StepLimit { .. }
        )
    });
    let intent = equality_intent(&store, record.id);
    assert!(
        historical_append(&mut store, intent).is_err(),
        "no further Intent at4096"
    );
    drop(store);
    let mut store = ConnectionStore::open_at(&path).expect("real terminal log replay");
    store.checkpoint().expect("real terminal checkpoint");
    drop(store);
    let store = ConnectionStore::open_at(&path).expect("terminal empty-log reopen");
    assert_eq!(store.review_cursor(record.id.0, 1).unwrap(), cursor);
}
#[test]
fn retired_snapshot_fix1_maximum_review_complete_boundary() {
    fix1_maximum_boundary(true);
}
#[test]
fn retired_snapshot_fix1_maximum_step_limit_boundary() {
    fix1_maximum_boundary(false);
}

#[test]
fn retired_snapshot_fix1_predecessor_index_rejects_unjoined_terminals() {
    let (_temp, path, mut store, record) = fixture();
    accept_supported_review(&mut store, &record);
    for ordinal in [0, 2] {
        let intent = if ordinal == 2 {
            equality_intent(&store, record.id)
        } else {
            addition_intent_at(&store, record.id, ordinal)
        };
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
    }
    store.checkpoint().unwrap();
    let original = store
        .rows()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let current_bytes = std::fs::read(path.join("CURRENT")).unwrap();
    let last_expected = store
        .operation(OperationId {
            connection: record.id,
            serial: 2,
        })
        .unwrap()
        .expected;
    for damage in 0..4 {
        let mut rows = original.clone();
        let at = rows
            .iter()
            .position(|r| matches!(r, Row::InitialReviewStepCompletedV1(s) if s.step == 0))
            .unwrap();
        match damage {
            0 => {
                rows.remove(at);
            }
            1 => {
                let mut duplicate = rows[at].clone();
                let Row::InitialReviewStepCompletedV1(step) = &mut duplicate else {
                    panic!()
                };
                step.key.origin += 1;
                rows.push(duplicate);
            }
            2 => {
                let Row::InitialReviewStepCompletedV1(step) = &mut rows[at] else {
                    panic!()
                };
                step.key.connection = [55; 16];
                step.first_operation.connection = [55; 16];
                let format::ReviewTerminalV1::Completed {
                    operation, archive, ..
                } = &mut step.terminal
                else {
                    panic!()
                };
                operation.connection = [55; 16];
                if let format::ReviewArchiveRefV1::Present { anchor, .. } = archive {
                    anchor.connection = [55; 16];
                }
            }
            _ => {
                let Row::InitialReviewStepCompletedV1(step) = &mut rows[at] else {
                    panic!()
                };
                step.resulting_progress = last_expected + 1;
            }
        }
        rows.sort_by_key(Row::key);
        for row in &rows {
            row.validate()
                .unwrap_or_else(|e| panic!("local damage {damage}: {e:?}"));
        }
        let mut current = store.current.clone();
        current.root = storage::build_pages(&store.storage, rows.into_iter().map(Ok)).unwrap();
        store
            .publish_current(&current, &store.log, &store.log_identity)
            .unwrap();
        drop(store);
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "accepted predecessor damage {damage}"
        );
        std::fs::write(path.join("CURRENT"), &current_bytes).unwrap();
        store = ConnectionStore::open_at(&path).unwrap();
    }
    let intent = addition_intent_at(&store, record.id, 1);
    historical_append(&mut store, intent).unwrap();
    finish_reviewed_pending(&mut store, record.id);
    store.checkpoint().unwrap();
    drop(store);
    ConnectionStore::open_at(&path).unwrap();
}

#[test]
fn retired_snapshot_fix3_one_owner_keeps_both_baseline_reads() {
    let (_temp, _path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    let mut roots = Vec::new();
    for _ in 0..2 {
        let intent = equality_intent(&store, record.id);
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
        roots.push(store.connection(record.id).unwrap().baseline_root.unwrap());
    }
    assert_ne!(roots[0], roots[1]);
    store.checkpoint().unwrap();
    assert!(store.overlay.is_empty());
    let root = store.current.root.unwrap();
    let lease = store.storage.read_lease(false).unwrap();
    let observer = storage::physical_trace::begin();
    store
        .validate_review_history_with_context(&[], None)
        .unwrap();
    let trace = observer.finish();
    let eof = |root: [u8; 32]| {
        trace
            .reads
            .iter()
            .filter(|r| r.name == format::hex(&root) && r.returned == Some(0))
            .count()
    };
    assert_eq!(
        eof(roots[0]),
        4,
        "first after and second before each retain constructor/traversal/EOF"
    );
    assert_eq!(
        eof(roots[1]),
        2,
        "second after retains constructor/traversal/EOF"
    );
    eprintln!(
        "fix3 closure owner-root EOF={} before-root EOF={} after-root EOF={} stats={:?}",
        eof(root),
        eof(roots[0]),
        eof(roots[1]),
        lease.stats()
    );
    assert_eq!(
        eof(root),
        3,
        "two snapshot traversal reads plus one canonical owner lookup"
    );
}

fn fix3_snapshot_rows(store: &ConnectionStore) -> BTreeMap<Vec<u8>, Row> {
    store
        .rows()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            (r.key(), r)
        })
        .collect()
}
fn fix3_snapshot_read(
    store: &ConnectionStore,
    rows: &BTreeMap<Vec<u8>, Row>,
    context: Option<&NativeReadContext<'_>>,
    validate: impl FnOnce(
        &dyn Fn(Option<[u8; 32]>, u32, u64) -> Result<VerifiedBaseline, StoreError>,
    ) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let review = rows
        .values()
        .find_map(|r| match r {
            Row::InitialChainReview(r) => Some(r),
            _ => None,
        })
        .unwrap();
    store.with_review_baseline_owner(review, rows, context, validate)
}
fn fix3_snapshot_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    let (temp, path, mut store, record) = fixture();
    let accepted = owner(&mut store, &record);
    store.accept_initial_set(accepted).unwrap();
    for _ in 0..2 {
        let intent = equality_intent(&store, record.id);
        historical_append(&mut store, intent).unwrap();
        finish_reviewed_pending(&mut store, record.id);
    }
    store.checkpoint().unwrap();
    (temp, path, store, record)
}
#[test]
fn retired_snapshot_fix3_snapshot_lease_overlay_and_new_invocation() {
    let (_temp, path, mut store, record) = fix3_snapshot_fixture();
    let rows = fix3_snapshot_rows(&store);
    let c = store.connection(record.id).unwrap();
    let read =
        |reader: &dyn Fn(Option<[u8; 32]>, u32, u64) -> Result<VerifiedBaseline, StoreError>| {
            reader(c.baseline_root, c.baseline_count, c.baseline.unwrap()).map(|_| ())
        };
    assert!(matches!(
        fix3_snapshot_read(&store, &rows, None, read),
        Err(StoreError::RecoveryRequired)
    ));
    let lease = store.storage.read_lease(false).unwrap();
    lease.expire_for_test();
    assert!(matches!(
        fix3_snapshot_read(&store, &rows, None, read),
        Err(StoreError::LimitExceeded)
    ));
    drop(lease);
    let lease = store.storage.read_lease(false).unwrap();
    assert!(matches!(
        fix3_snapshot_read(&store, &rows, None, |reader| {
            read(reader)?;
            lease.expire_for_test();
            read(reader)
        }),
        Err(StoreError::LimitExceeded)
    ));
    drop(lease);
    let lease = store.storage.read_lease(false).unwrap();
    fix3_snapshot_read(&store, &rows, None, read).unwrap();
    for damage in 0..3 {
        let mut bad = rows.clone();
        let Row::Connection(owner) = bad.get_mut(&format::key(0, record.id.0, 0)).unwrap() else {
            panic!()
        };
        match damage {
            0 => owner.id = [91; 16],
            1 => owner.source.identity.root[0] ^= 1,
            _ => owner.destination.identity.root[0] ^= 1,
        }
        assert!(fix3_snapshot_read(&store, &bad, None, read).is_err());
    }
    assert!(matches!(
        fix3_snapshot_read(&store, &rows, None, |reader| reader(
            c.baseline_root,
            c.baseline_count,
            c.revision + 1
        )
        .map(|_| ())),
        Err(StoreError::Corrupt)
    ));
    let mut overlay_owner = c.clone();
    overlay_owner.source.identity.root[0] ^= 1;
    store.overlay.insert(
        format::key(0, record.id.0, 0),
        Row::Connection(overlay_owner),
    );
    assert!(
        matches!(
            fix3_snapshot_read(&store, &rows, None, read),
            Err(StoreError::Corrupt)
        ),
        "must read changed canonical overlay, never stale disk owner"
    );
    store.overlay.remove(&format::key(0, record.id.0, 0));
    fix3_snapshot_read(&store, &rows, None, read).unwrap();
    drop(lease);
    let old_effective = (store.sequence, store.digest);
    let old_root = store.current.root;
    let intent = equality_intent(&store, record.id);
    historical_append(&mut store, intent).unwrap();
    finish_reviewed_pending(&mut store, record.id);
    assert_ne!((store.sequence, store.digest), old_effective);
    assert!(store.overlay.contains_key(&format::key(0, record.id.0, 0)));
    let next_rows = fix3_snapshot_rows(&store);
    let next = store.connection(record.id).unwrap();
    let lease = store.storage.read_lease(false).unwrap();
    let observer = storage::physical_trace::begin();
    fix3_snapshot_read(&store, &next_rows, None, |reader| {
        reader(
            next.baseline_root,
            next.baseline_count,
            next.baseline.unwrap(),
        )
        .map(|_| ())
    })
    .unwrap();
    let trace = observer.finish();
    assert!(
        !trace
            .reads
            .iter()
            .any(|r| r.name == format::hex(&old_root.unwrap())),
        "canonical overlay owner needs no stale disk lookup"
    );
    store
        .validate_review_history_with_context(&[], None)
        .unwrap();
    drop(lease);
    store.checkpoint().unwrap();
    assert_ne!(store.current.root, old_root);
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    let rows = fix3_snapshot_rows(&store);
    let lease = store.storage.read_lease(false).unwrap();
    let observer = storage::physical_trace::begin();
    fix3_snapshot_read(&store, &rows, None, |reader| {
        reader(
            next.baseline_root,
            next.baseline_count,
            next.baseline.unwrap(),
        )
        .map(|_| ())
    })
    .unwrap();
    let trace = observer.finish();
    assert_eq!(trace.reads.iter().filter(|r|r.name==format::hex(&store.current.root.unwrap())&&r.returned==Some(0)).count(),1,"new invocation after publication/reopen performs fresh physical owner lookup");
    drop(lease);
}
#[test]
fn retired_snapshot_fix3_snapshot_backing_data_remains_physical() {
    let (_temp, path, store, record) = fix3_snapshot_fixture();
    let rows = fix3_snapshot_rows(&store);
    let c = store.connection(record.id).unwrap();
    let lease = store.storage.read_lease(false).unwrap();
    let read =
        |reader: &dyn Fn(Option<[u8; 32]>, u32, u64) -> Result<VerifiedBaseline, StoreError>| {
            reader(c.baseline_root, c.baseline_count, c.baseline.unwrap()).map(|_| ())
        };
    let main = path
        .join("pages")
        .join(format::hex(&store.current.root.unwrap()));
    let baseline = path
        .join("pages")
        .join(format::hex(&c.baseline_root.unwrap()));
    for file in [&main, &baseline] {
        let bytes = std::fs::read(file).unwrap();
        let hidden = file.with_extension("hidden");
        std::fs::rename(file, &hidden).unwrap();
        assert!(fix3_snapshot_read(&store, &rows, None, read).is_err());
        std::fs::rename(&hidden, file).unwrap();
        let mut corrupt = bytes.clone();
        corrupt[0] ^= 1;
        std::fs::write(file, &corrupt).unwrap();
        assert!(fix3_snapshot_read(&store, &rows, None, read).is_err());
        std::fs::write(file, &bytes).unwrap();
        fix3_snapshot_read(&store, &rows, None, read).unwrap();
    }
    let bytes = std::fs::read(&baseline).unwrap();
    let hidden = baseline.with_extension("hidden");
    fix3_snapshot_read(&store, &rows, None, |reader| {
        read(reader)?;
        std::fs::rename(&baseline, &hidden).unwrap();
        assert!(read(reader).is_err());
        std::fs::rename(&hidden, &baseline).unwrap();
        let mut corrupt = bytes.clone();
        corrupt[0] ^= 1;
        std::fs::write(&baseline, &corrupt).unwrap();
        assert!(read(reader).is_err());
        std::fs::write(&baseline, &bytes).unwrap();
        read(reader)
    })
    .unwrap();
    let first = rows
        .values()
        .find_map(|r| match r {
            Row::ReviewedBaselineResultV1(r)
                if matches!(r.witness.before, format::BaselineVersion::Absent {}) =>
            {
                Some(&r.witness.after)
            }
            _ => None,
        })
        .unwrap();
    let format::BaselineVersion::Present { root, .. } = first else {
        panic!()
    };
    assert!(
        fix3_snapshot_read(&store, &rows, None, |reader| reader(
            root.root(),
            c.baseline_count,
            c.baseline.unwrap()
        )
        .map(|_| ()))
        .is_err(),
        "wrong historical root/revision still refuses"
    );
    let (_foreign_temp, _foreign_path, foreign, _) = fixture();
    let _foreign_lease = foreign.storage.read_lease(false).unwrap();
    let review = rows
        .values()
        .find_map(|r| match r {
            Row::InitialChainReview(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(
        foreign
            .validate_durable_review(review, &rows, None)
            .is_err(),
        "foreign store cannot authenticate this prospective history"
    );
    store
        .validate_review_history_with_context(&[], None)
        .unwrap();
    drop(lease);
}
fn fix3_paid_owner_checks(
    store: &ConnectionStore,
    rows: &BTreeMap<Vec<u8>, Row>,
    context: &NativeReadContext<'_>,
) {
    let review = rows
        .values()
        .find_map(|r| match r {
            Row::InitialChainReview(r) => Some(r),
            _ => None,
        })
        .unwrap();
    let Row::Connection(c) = rows.get(&format::key(0, review.connection, 0)).unwrap() else {
        panic!()
    };
    assert_ne!(c.id, context.id.connection.0);
    let versions = rows
        .values()
        .filter_map(|r| match r {
            Row::ReviewedBaselineResultV1(r) if r.key.connection == c.id => Some(&r.witness.after),
            _ => None,
        })
        .collect::<Vec<_>>();
    let format::BaselineVersion::Present {
        root: old,
        count: old_count,
        revision: old_revision,
    } = versions[0]
    else {
        panic!()
    };
    let format::BaselineVersion::Present {
        root: new,
        count: new_count,
        revision: new_revision,
    } = versions[versions.len() - 1]
    else {
        panic!()
    };
    assert_ne!(old, new);
    for refused in [false, true] {
        let credit = remaining_credit(&context.reads);
        let observer = storage::physical_trace::begin();
        let result = fix3_snapshot_read(store, rows, Some(context), |reader| {
            reader(old.root(), *old_count, *old_revision)?;
            reader(
                new.root(),
                *new_count,
                if refused {
                    c.revision + 1
                } else {
                    *new_revision
                },
            )?;
            Ok(())
        });
        let trace = observer.finish();
        assert_eq!(result.is_err(), refused);
        let requested = trace.reads.iter().map(|r| r.requested as u64).sum::<u64>();
        assert_eq!(requested, credit - remaining_credit(&context.reads));
        let eof = |root: [u8; 32]| {
            trace
                .reads
                .iter()
                .filter(|r| r.name == format::hex(&root) && r.returned == Some(0))
                .count()
        };
        assert_eq!(
            eof(store.current.root.unwrap()),
            1,
            "one actual main-owner lookup"
        );
        assert_eq!(
            eof(old.root().unwrap()),
            2,
            "old constructor/traversal EOF remains physical"
        );
        assert_eq!(
            eof(new.root().unwrap()),
            if refused { 0 } else { 2 },
            "new constructor/traversal EOF remains physical"
        );
        eprintln!(
            "fix3 paid owner A/payer B refused={refused} actual={} requested={requested} charged={} ownerEOF=1 oldEOF=2 newEOF={}",
            trace.reads.len(),
            credit - remaining_credit(&context.reads),
            if refused { 0 } else { 2 }
        );
    }
}

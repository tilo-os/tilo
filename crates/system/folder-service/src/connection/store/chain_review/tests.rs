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
    use crate::connection::transaction::native::descriptor as d;
    let c = store.connection(id).unwrap();
    let review = store.review_fact(id.0, 1).unwrap();
    let view = store.read_review(&review).unwrap();
    let entry = &view.entries[2];
    let file = EntryState::File {
        sha256: [6; 32],
        bytes: 19,
        executable: false,
    };
    let path = RelativePath::unix(b"equal").unwrap();
    let mut operation = format::Operation::new(
        OperationIntent {
            connection: id,
            expected_revision: c.revision,
            expected_baseline: None,
            path: path.clone(),
            source_before: Some(file.clone()),
            destination_before: Some(file),
            decision: Decision::VerifyEqual,
        },
        1,
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
            operation: 1,
            revision: c.revision,
            fence: 0,
            baseline: None,
            endpoints,
            direction: 0,
            decision: 0,
            path,
            chains: [vec![object(11, 0o040_000)], vec![object(12, 0o040_000)]],
            states: [13, 14].map(|ino| d::State::Regular {
                object: object(ino, 0o100_000),
                stamp: d::Stamp {
                    length: 19,
                    modified: (1, 0),
                    changed: (1, 0),
                    mode: 0o100_600,
                    uid: 0,
                    gid: 0,
                    links: 1,
                },
                content: d::Content {
                    digest: [6; 32],
                    length: 19,
                    executable: false,
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
            step: 0,
            entry: 2,
            entry_digest: format::digest(&format::encode(entry).unwrap()),
            operation: format::ChainOperationIdV1 {
                connection: id.0,
                serial: 1,
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
    let op = store
        .operation(OperationId {
            connection: id,
            serial: 1,
        })
        .unwrap();
    Change::ReviewedOrdinaryStopBoundary {
        boundary: format::InitialReviewStopBoundaryV1::EqualityUntouched {
            fact: format::EqualityUntouchedV1 {
                version: 1,
                connection: id.0,
                review: cursor.review,
                origin: 1,
                step: 0,
                entry: 2,
                operation: format::ChainOperationIdV1 {
                    connection: id.0,
                    serial: 1,
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
    let selected = view.selection(0, operation.expected).unwrap();
    assert_eq!(selected.preview_digest, view.fact.preview);
    assert_eq!(selected.review_revision, 2);
    assert_eq!(view.fact.original_revision, 1);
    let mut admission = d::Admission::decode(operation.native_identity.as_ref().unwrap()).unwrap();
    admission.path = selected.path.clone();
    admission.decision = 1;
    admission.states[1] = d::State::Absent;
    operation.path = selected.path.clone().into();
    operation.decision = 1;
    operation.destination = None;
    operation.native_identity = Some(admission.encode().unwrap());
    let anchor = format::ChainOperationIdV1 {
        connection: id.0,
        serial: 1,
    };
    operation.directory_chain_anchor = Some(selected.encode().unwrap());
    operation.directory_chain_ref = Some(format::DirectoryChainRefV1 {
        anchor,
        digest: selected.selection_digest,
        index: 0,
        predecessor: None,
    });
    link.entry = 0;
    link.entry_digest = format::digest(&format::encode(&view.entries[0]).unwrap());
    link.kind = format::InitialReviewLinkKindV1::Chain {
        anchor,
        digest: selected.selection_digest,
        review_revision: 2,
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

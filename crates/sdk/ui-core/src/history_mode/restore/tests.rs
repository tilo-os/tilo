use super::*;
use automerge::{
    ROOT, ReadDoc, ScalarValue, TextEncoding,
    marks::{ExpandMark, Mark},
    transaction::Transactable,
};

static TEXT: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "test.text",
    version: 1,
    roots: &[HistoryRestoreRoot {
        key: "content",
        required: true,
        expected_type: RestoreValueType::Object(ObjType::Text),
    }],
    selectors: &[],
    preserved: &[],
    validate_result: |_| Ok(()),
    preserved_roots: &["notes_import_v1"],
    validate: |_| Ok(()),
};
static MAP: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "test.map",
    version: 1,
    roots: &[HistoryRestoreRoot {
        key: "content",
        required: true,
        expected_type: RestoreValueType::Object(ObjType::Map),
    }],
    selectors: &[],
    preserved: &[],
    validate_result: |_| Ok(()),
    preserved_roots: &[],
    validate: |_| Ok(()),
};
fn text_fixture() -> (AutoCommit, Vec<ChangeHash>, Vec<ChangeHash>) {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "content", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "one").unwrap();
    let target = doc.get_heads();
    doc.splice_text(&text, 0, 3, "two").unwrap();
    let live = doc.get_heads();
    (doc, target, live)
}
fn content_text(doc: &AutoCommit) -> String {
    doc.text(doc.get(ROOT, "content").unwrap().unwrap().1)
        .unwrap()
}
fn assert_unchanged(doc: &AutoCommit, before: &mut AutoCommit) {
    assert_eq!(doc.pending_ops(), before.pending_ops());
    assert_eq!(doc.get_actor(), before.get_actor());
    let mut after = doc.clone();
    assert_eq!(after.save(), before.save());
}

// Catches an engine that commits, swaps the live document/actor, or truncates history.
#[test]
fn restores_text_in_one_owned_pending_transaction_without_losing_history() {
    let (mut doc, target, live) = text_fixture();
    let actor = doc.get_actor().clone();
    let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    assert_eq!(content_text(&doc), "two");
    assert_eq!(doc.pending_ops(), 0);
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap(),
        RestoreOutcome::Changed
    );
    assert_eq!(content_text(&doc), "one");
    assert!(doc.pending_ops() > 0);
    assert_eq!(doc.get_actor(), &actor);
    let new = doc.commit().unwrap();
    assert!(doc.commit().is_none());
    assert_ne!(new, target[0]);
    for hash in target.iter().chain(live.iter()) {
        assert!(doc.get_change_by_hash(hash).is_some());
    }
}

// Catches restoration escaping the allowlist, or flattening metadata conflicts.
#[test]
fn preserves_current_noncontent_values_objects_and_conflicts() {
    let (mut doc, _, _) = text_fixture();
    let metadata = doc.put_object(ROOT, "qos:record", ObjType::Map).unwrap();
    doc.put(&metadata, "id", "record-live").unwrap();
    for key in [
        "future_root",
        "notes_import_v1",
        "qos:group:a",
        "qos:run:a",
        "editor_disk_base",
    ] {
        doc.put(ROOT, key, "old").unwrap();
    }
    let target = doc.get_heads();
    let text = doc.get(ROOT, "content").unwrap().unwrap().1;
    doc.splice_text(&text, 0, 3, "changed").unwrap();
    let mut peer = doc.fork();
    doc.put(&metadata, "id", "record-current").unwrap();
    peer.put(&metadata, "id", "record-concurrent").unwrap();
    doc.merge(&mut peer).unwrap();
    for key in [
        "future_root",
        "notes_import_v1",
        "qos:group:a",
        "qos:run:a",
        "editor_disk_base",
    ] {
        doc.put(ROOT, key, "current").unwrap();
    }
    let live = doc.get_heads();
    let conflict: Vec<_> = doc
        .get_all(&metadata, "id")
        .unwrap()
        .into_iter()
        .map(|(v, id)| (v.into_owned(), id))
        .collect();
    assert_eq!(conflict.len(), 2);
    let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert_eq!(content_text(&doc), "two");
    assert_eq!(doc.get(ROOT, "qos:record").unwrap().unwrap().1, metadata);
    assert_eq!(doc.get_all(&metadata, "id").unwrap(), conflict);
    for key in [
        "future_root",
        "notes_import_v1",
        "qos:group:a",
        "qos:run:a",
        "editor_disk_base",
    ] {
        assert_eq!(
            doc.get(ROOT, key).unwrap().unwrap().0.as_str(),
            Some("current")
        );
    }
}

// Catches lossy scalars, wrong list order, and failed nested resurrection.
#[test]
fn resurrects_nested_maps_lists_references_and_all_scalar_types() {
    let mut doc = AutoCommit::new();
    let root = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let list = doc.put_object(&root, "list", ObjType::List).unwrap();
    let map = doc.insert_object(&list, 0, ObjType::Map).unwrap();
    doc.put(&map, "uuid", "01-ref-page").unwrap();
    let values = vec![
        ScalarValue::Bytes(vec![0, 255]),
        ScalarValue::Str("str".into()),
        ScalarValue::Int(-8),
        ScalarValue::Uint(8),
        ScalarValue::F64(-0.0),
        ScalarValue::Counter(4.into()),
        ScalarValue::Timestamp(9),
        ScalarValue::Boolean(true),
        ScalarValue::Null,
        ScalarValue::Unknown {
            type_code: 15,
            bytes: vec![5],
        },
        ScalarValue::F64(f64::from_bits(0x7ff8_0000_0000_0123)),
    ];
    for (i, value) in values.iter().enumerate() {
        doc.insert(&list, i.saturating_add(1), value.clone())
            .unwrap();
    }
    doc.increment(&list, 6, 3).unwrap();
    let target = doc.get_heads();
    doc.delete(ROOT, "content").unwrap();
    doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &MAP, &live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    let restored = doc.get(ROOT, "content").unwrap().unwrap().1;
    assert_ne!(restored, root);
    let list = doc.get(&restored, "list").unwrap().unwrap().1;
    assert_eq!(doc.length(&list), 12);
    let map = doc.get(&list, 0).unwrap().unwrap().1;
    assert_eq!(
        doc.get(map, "uuid").unwrap().unwrap().0.as_str(),
        Some("01-ref-page")
    );
    for (i, expected) in values.iter().enumerate() {
        let actual = doc
            .get(&list, i.saturating_add(1))
            .unwrap()
            .unwrap()
            .0
            .as_scalar()
            .unwrap()
            .clone();
        match (expected, &actual) {
            (ScalarValue::F64(a), ScalarValue::F64(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            (ScalarValue::Counter(_), ScalarValue::Counter(c)) => assert_eq!(i64::from(c), 7),
            _ => assert_eq!(&actual, expected),
        }
    }
}

// Catches byte/codepoint index confusion and ignored mark-only changes.
#[test]
fn restores_unicode_text_and_exact_typed_marks_in_every_encoding() {
    for (encoding, end) in [
        (TextEncoding::UnicodeCodePoint, 4),
        (TextEncoding::Utf8CodeUnit, 8),
        (TextEncoding::Utf16CodeUnit, 5),
        (TextEncoding::GraphemeCluster, 4),
    ] {
        let mut doc = AutoCommit::new_with_encoding(encoding);
        let text = doc.put_object(ROOT, "content", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "a🦀éb").unwrap();
        doc.mark(
            &text,
            Mark::new("bold".into(), true, 0, end),
            ExpandMark::Both,
        )
        .unwrap();
        doc.mark(
            &text,
            Mark::new("comment".into(), ScalarValue::Uint(9), 0, 1),
            ExpandMark::None,
        )
        .unwrap();
        let target = doc.get_heads();
        doc.unmark(&text, "bold", 0, end, ExpandMark::None).unwrap();
        doc.mark(
            &text,
            Mark::new("link".into(), "https://example.test", 0, 1),
            ExpandMark::None,
        )
        .unwrap();
        let live = doc.get_heads();
        let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
        apply_prepared_content_restore(&mut doc, &prepared).unwrap();
        let text = doc.get(ROOT, "content").unwrap().unwrap().1;
        assert_eq!(doc.text(&text).unwrap(), "a🦀éb");
        assert_eq!(
            doc.marks(&text).unwrap(),
            vec![
                Mark::new("bold".into(), true, 0, end),
                Mark::new("comment".into(), ScalarValue::Uint(9), 0, 1)
            ]
        );
        // Replayed marks deliberately do not expand at a boundary.
        doc.splice_text(&text, end, 0, "!").unwrap();
        assert_eq!(doc.marks(&text).unwrap()[0].end, end);
    }
}

#[test]
fn rejects_stale_pending_unknown_and_malformed_targets_without_mutation() {
    let (mut doc, target, live) = text_fixture();
    let mut before = doc.clone();
    assert_eq!(
        prepare_content_restore(&doc, &TEXT, &target, &target).unwrap_err(),
        RestoreError::StaleReview
    );
    assert_eq!(
        prepare_content_restore(&doc, &TEXT, &live, &[ChangeHash([42; 32])]).unwrap_err(),
        RestoreError::InvalidTargetHeads
    );
    assert_unchanged(&doc, &mut before);
    doc.put(ROOT, "typing", true).unwrap();
    let mut before = doc.clone();
    assert_eq!(
        prepare_content_restore(&doc, &TEXT, &live, &target).unwrap_err(),
        RestoreError::PendingEdits
    );
    assert_unchanged(&doc, &mut before);
    doc.rollback();
    assert!(matches!(
        prepare_content_restore(&doc, &MAP, &live, &target),
        Err(RestoreError::SchemaInvalid(_))
    ));
}

#[test]
fn rejects_invalid_schema_and_conflicted_target_content() {
    let (mut doc, target, _) = text_fixture();
    for roots in [
        &[][..],
        &[TEXT.roots[0], TEXT.roots[0]][..],
        &[HistoryRestoreRoot {
            key: "qos:record",
            ..TEXT.roots[0]
        }][..],
        &[HistoryRestoreRoot {
            key: "notes_import_v1",
            ..TEXT.roots[0]
        }][..],
    ] {
        let schema = Box::leak(Box::new(HistoryRestoreSchema {
            roots: Box::leak(roots.to_vec().into_boxed_slice()),
            ..TEXT
        }));
        let heads = doc.get_heads();
        assert!(matches!(
            prepare_content_restore(&doc, schema, &heads, &target),
            Err(RestoreError::InvalidSchema(_))
        ));
    }
    let mut peer = doc.fork();
    doc.put_object(ROOT, "content", ObjType::Text).unwrap();
    peer.put_object(ROOT, "content", ObjType::Text).unwrap();
    doc.merge(&mut peer).unwrap();
    let conflicted = doc.get_heads();
    let mut before = doc.clone();
    assert!(matches!(
        prepare_content_restore(&doc, &TEXT, &conflicted, &conflicted),
        Err(RestoreError::UnsupportedConflict(_))
    ));
    assert_unchanged(&doc, &mut before);
}

#[test]
fn unchanged_content_creates_no_transaction_even_when_metadata_differs() {
    let (mut doc, _, _) = text_fixture();
    let target = doc.get_heads();
    doc.put(ROOT, "future", "changed").unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap(),
        RestoreOutcome::Unchanged
    );
    assert_eq!(doc.pending_ops(), 0);
    assert!(doc.commit().is_none());
    assert_eq!(doc.get_heads(), live);
}

#[test]
fn apply_rechecks_exact_heads_and_refuses_pending_work() {
    let (mut doc, target, live) = text_fixture();
    let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    doc.put(ROOT, "qos:run:new", "metadata only").unwrap();
    let mut before = doc.clone();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap_err(),
        RestoreError::PendingEdits
    );
    assert_unchanged(&doc, &mut before);
    doc.commit();
    let mut before = doc.clone();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap_err(),
        RestoreError::StaleReview
    );
    assert_unchanged(&doc, &mut before);
}

fn reject_pending(doc: &AutoCommit) -> Result<(), String> {
    if doc.pending_ops() > 0 {
        Err("reject trial after writes".into())
    } else {
        Ok(())
    }
}
fn reject_live_pending(doc: &AutoCommit) -> Result<(), String> {
    if doc.get_actor() == &automerge::ActorId::from(b"reject-live".as_slice())
        && doc.pending_ops() > 0
    {
        Err("reject live after writes".into())
    } else {
        Ok(())
    }
}
static REJECT_TRIAL: HistoryRestoreSchema = HistoryRestoreSchema {
    validate: reject_pending,
    ..TEXT
};
static REJECT_LIVE: HistoryRestoreSchema = HistoryRestoreSchema {
    validate: reject_live_pending,
    ..TEXT
};

#[test]
fn trial_validator_failure_does_not_touch_live_state() {
    let (doc, target, live) = text_fixture();
    let mut before = doc.clone();
    assert!(matches!(
        prepare_content_restore(&doc, &REJECT_TRIAL, &live, &target),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_unchanged(&doc, &mut before);
}

#[test]
fn live_validator_failure_rolls_back_every_write() {
    let (mut doc, target, live) = text_fixture();
    doc.set_actor(automerge::ActorId::from(b"reject-live".as_slice()));
    let prepared = prepare_content_restore(&doc, &REJECT_LIVE, &live, &target).unwrap();
    let mut before = doc.clone();
    assert!(matches!(
        apply_prepared_content_restore(&mut doc, &prepared),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_eq!(content_text(&doc), "two");
    assert_unchanged(&doc, &mut before);
}

#[test]
fn target_rich_text_blocks_fail_before_live_mutation() {
    let (mut doc, _, _) = text_fixture();
    let text = doc.get(ROOT, "content").unwrap().unwrap().1;
    let block = doc.split_block(&text, 0).unwrap();
    doc.put(block, "kind", "paragraph").unwrap();
    let target = doc.get_heads();
    doc.join_block(&text, 0).unwrap();
    let live = doc.get_heads();
    let mut before = doc.clone();
    assert!(matches!(
        prepare_content_restore(&doc, &TEXT, &live, &target),
        Err(RestoreError::UnsupportedContent(_))
    ));
    assert_unchanged(&doc, &mut before);
}

static OPTIONAL: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "test.optional",
    version: 1,
    roots: &[
        HistoryRestoreRoot {
            key: "content",
            required: true,
            expected_type: RestoreValueType::Object(ObjType::Text),
        },
        HistoryRestoreRoot {
            key: "optional",
            required: false,
            expected_type: RestoreValueType::Scalar(ScalarType::String),
        },
    ],
    selectors: &[],
    preserved: &[],
    validate_result: |_| Ok(()),
    preserved_roots: &[],
    validate: |_| Ok(()),
};

#[test]
fn optional_root_creation_deletion_and_unchanged_content_identity() {
    let (mut doc, _, _) = text_fixture();
    let missing = doc.get_heads();
    doc.put(ROOT, "optional", "original").unwrap();
    let present = doc.get_heads();
    doc.delete(ROOT, "optional").unwrap();
    let live = doc.get_heads();
    let text = doc.get(ROOT, "content").unwrap().unwrap().1;
    let prepared = prepare_content_restore(&doc, &OPTIONAL, &live, &present).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert_eq!(
        doc.get(ROOT, "optional").unwrap().unwrap().0.as_str(),
        Some("original")
    );
    assert_eq!(doc.get(ROOT, "content").unwrap().unwrap().1, text);
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &OPTIONAL, &live, &missing).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert!(doc.get(ROOT, "optional").unwrap().is_none());
    assert_eq!(doc.get(ROOT, "content").unwrap().unwrap().1, text);
}

// An isolated read view must not masquerade as the canonical live frontier.
#[test]
fn rejects_isolated_historical_view_as_live_document() {
    let (mut doc, target, _) = text_fixture();
    doc.isolate(&target);
    let mut before = doc.clone();
    assert_eq!(
        prepare_content_restore(&doc, &TEXT, &target, &target).unwrap_err(),
        RestoreError::StaleReview
    );
    assert_unchanged(&doc, &mut before);
}

#[test]
fn selected_full_frontier_excludes_unrelated_concurrent_content_and_normalizes_heads() {
    let mut doc = AutoCommit::new();
    let root = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    doc.put(&root, "a", "base-a").unwrap();
    doc.put(&root, "b", "base-b").unwrap();
    doc.commit();
    let mut peer = doc.fork();
    doc.put(&root, "a", "selected-a").unwrap();
    let target = doc.get_heads();
    peer.put(&root, "b", "unrelated-b").unwrap();
    doc.merge(&mut peer).unwrap();
    let mut heads = doc.get_heads();
    assert_eq!(heads.len(), 2);
    heads.reverse();
    heads.extend(heads.clone());
    let prepared = prepare_content_restore(&doc, &MAP, &heads, &target).unwrap();
    assert_eq!(prepared.current_heads().len(), 2);
    assert_eq!(prepared.target_heads(), target);
    assert_eq!(prepared.schema().id, "test.map");
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    let root = doc.get(ROOT, "content").unwrap().unwrap().1;
    assert_eq!(
        doc.get(&root, "a").unwrap().unwrap().0.as_str(),
        Some("selected-a")
    );
    assert_eq!(
        doc.get(&root, "b").unwrap().unwrap().0.as_str(),
        Some("base-b")
    );
}

// Inject a corrupt private plan to exercise the live postcondition, rather than
// introducing a production fault-injection hook or mutating the live document.
#[test]
fn live_postcondition_failure_rolls_back_prior_successful_writes() {
    use super::projection::{ExactScalar, Node};
    let (mut doc, target, live) = text_fixture();
    let mut prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    prepared.target.0.insert(
        "z-not-content".into(),
        vec![Node::Scalar(ExactScalar(ScalarValue::Boolean(true)))],
    );
    let mut before = doc.clone();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap_err(),
        RestoreError::ContentPostconditionMismatch
    );
    assert_eq!(content_text(&doc), "two");
    assert!(doc.get(ROOT, "z-not-content").unwrap().is_none());
    assert_unchanged(&doc, &mut before);
}

// A mark on a list provokes a real Automerge InvalidOp only after the root and
// list element have been written. The malformed plan is private test state.
#[test]
fn automerge_write_error_rolls_back_prior_successful_writes() {
    use super::projection::{ExactScalar, Node, SemanticMark};
    let (mut doc, target, live) = text_fixture();
    let mut prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    prepared.target.0.insert(
        "content".into(),
        vec![Node::List(
            vec![vec![Node::Scalar(ExactScalar(ScalarValue::Int(1)))]],
            vec![SemanticMark {
                name: "bad-list-mark".into(),
                start: 0,
                end: 1,
                value: ExactScalar(ScalarValue::Boolean(true)),
            }],
        )],
    );
    let mut before = doc.clone();
    assert!(matches!(
        apply_prepared_content_restore(&mut doc, &prepared),
        Err(RestoreError::Automerge(_))
    ));
    assert_eq!(content_text(&doc), "two");
    assert_unchanged(&doc, &mut before);
}

#[test]
fn changed_text_restores_inserted_marks_and_removes_new_marks() {
    let (mut doc, _, _) = text_fixture();
    let text = doc.get(ROOT, "content").unwrap().unwrap().1;
    doc.mark(
        &text,
        Mark::new("language".into(), "de", 0, 3),
        ExpandMark::Both,
    )
    .unwrap();
    let target = doc.get_heads();
    doc.splice_text(&text, 0, 3, "äbcdef").unwrap();
    doc.mark(
        &text,
        Mark::new("author".into(), "uuid-live", 0, 2),
        ExpandMark::Both,
    )
    .unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    let text = doc.get(ROOT, "content").unwrap().unwrap().1;
    assert_eq!(doc.text(&text).unwrap(), "two");
    assert_eq!(
        doc.marks(&text).unwrap(),
        vec![Mark::new("language".into(), "de", 0, 3)]
    );
}

#[test]
fn nested_target_conflict_is_rejected_even_if_current_winner_is_equal() {
    let mut doc = AutoCommit::new();
    let root = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let list = doc.put_object(&root, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "base").unwrap();
    doc.commit();
    let mut peer = doc.fork();
    doc.put(&list, 0, "left").unwrap();
    peer.put(&list, 0, "right").unwrap();
    doc.merge(&mut peer).unwrap();
    let target = doc.get_heads();
    let winner = doc.get(&list, 0).unwrap().unwrap().0.into_scalar().unwrap();
    doc.put(&list, 0, winner).unwrap();
    let live = doc.get_heads();
    assert_eq!(doc.get_all(&list, 0).unwrap().len(), 1);
    let mut before = doc.clone();
    assert!(matches!(
        prepare_content_restore(&doc, &MAP, &live, &target),
        Err(RestoreError::UnsupportedConflict(_))
    ));
    assert_unchanged(&doc, &mut before);
}

#[test]
fn missing_required_roots_in_live_or_target_fail_without_mutation() {
    let (mut doc, target, live) = text_fixture();
    let mut before = doc.clone();
    assert!(matches!(
        prepare_content_restore(&doc, &TEXT, &live, &[]),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_unchanged(&doc, &mut before);
    doc.delete(ROOT, "content").unwrap();
    let live = doc.get_heads();
    let mut before = doc.clone();
    assert!(matches!(
        prepare_content_restore(&doc, &TEXT, &live, &target),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_unchanged(&doc, &mut before);
}

#[test]
fn protected_rich_text_blocks_and_marks_remain_intact() {
    let (mut doc, target, _) = text_fixture();
    let text = doc
        .put_object(ROOT, "future-rich-text", ObjType::Text)
        .unwrap();
    doc.splice_text(&text, 0, 0, "protected🦀").unwrap();
    let block = doc.split_block(&text, 0).unwrap();
    doc.put(&block, "kind", "paragraph").unwrap();
    doc.mark(
        &text,
        Mark::new("bold".into(), true, 1, 4),
        ExpandMark::Both,
    )
    .unwrap();
    let live = doc.get_heads();
    let marks = doc.marks(&text).unwrap();
    let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert_eq!(doc.get(ROOT, "future-rich-text").unwrap().unwrap().1, text);
    assert_eq!(doc.text(&text).unwrap(), "\u{fffc}protected🦀");
    assert_eq!(doc.marks(&text).unwrap(), marks);
    assert_eq!(doc.get(&text, 0).unwrap().unwrap().1, block);
    assert_eq!(
        doc.get(&block, "kind").unwrap().unwrap().0.as_str(),
        Some("paragraph")
    );
}

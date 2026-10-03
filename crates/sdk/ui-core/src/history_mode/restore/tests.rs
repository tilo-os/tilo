use super::*;
use automerge::{
    AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, TextEncoding, Value,
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
static DEEP_MAP_WITH_PROTECTED_ROOT: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "test.deep-map",
    version: 1,
    roots: &[HistoryRestoreRoot {
        key: "content",
        required: true,
        expected_type: RestoreValueType::Object(ObjType::Map),
    }],
    selectors: &[],
    preserved: &[],
    validate_result: |_| Ok(()),
    preserved_roots: &["protected"],
    validate: |_| Ok(()),
};

#[test]
fn whole_retains_same_kind_map_identity_at_map_properties() {
    let mut doc = AutoCommit::new();
    let root = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let changed = doc.put_object(&root, "changed", ObjType::Map).unwrap();
    let untouched = doc.put_object(&root, "untouched", ObjType::Map).unwrap();
    doc.put(&changed, "value", "target").unwrap();
    doc.put(&untouched, "value", "stable").unwrap();
    let target = doc.get_heads();
    doc.put(&changed, "value", "live").unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &MAP, &live, &target).unwrap();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap(),
        RestoreOutcome::Changed
    );
    assert_eq!(doc.get(ROOT, "content").unwrap().unwrap().1, root);
    assert_eq!(doc.get(&root, "changed").unwrap().unwrap().1, changed);
    assert_eq!(doc.get(&root, "untouched").unwrap().unwrap().1, untouched);
    assert_eq!(
        doc.get(&changed, "value").unwrap().unwrap().0.as_str(),
        Some("target")
    );
    assert!(doc.pending_ops() > 0);
}

#[test]
fn whole_retains_nested_map_identities_with_exact_unknown_keys() {
    let mut doc = AutoCommit::new();
    let root = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let map = doc.put_object(&root, "unknown-map", ObjType::Map).unwrap();
    let untouched = doc.put_object(&root, "untouched", ObjType::Map).unwrap();
    let unchanged_list = doc
        .put_object(&root, "unchanged-list", ObjType::List)
        .unwrap();
    doc.insert(&unchanged_list, 0, "stable").unwrap();
    let unchanged_text = doc
        .put_object(&root, "unchanged-text", ObjType::Text)
        .unwrap();
    doc.splice_text(&unchanged_text, 0, 0, "stable").unwrap();
    doc.put(&map, "value", "target").unwrap();
    doc.put(&untouched, "stable", true).unwrap();
    doc.put(&root, "target-only", 7_i64).unwrap();
    let target = doc.get_heads();
    doc.put(&map, "value", "live").unwrap();
    doc.delete(&root, "target-only").unwrap();
    doc.put(&root, "current-only", 9_i64).unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &MAP, &live, &target).unwrap();
    let target_projection = prepared.target_projection().clone();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap(),
        RestoreOutcome::Changed
    );
    assert_eq!(doc.get(ROOT, "content").unwrap().unwrap().1, root);
    assert_eq!(doc.get(&root, "unknown-map").unwrap().unwrap().1, map);
    assert_eq!(doc.get(&root, "untouched").unwrap().unwrap().1, untouched);
    assert_eq!(
        doc.get(&root, "unchanged-list").unwrap().unwrap().1,
        unchanged_list
    );
    assert_eq!(
        doc.get(&root, "unchanged-text").unwrap().unwrap().1,
        unchanged_text
    );
    assert!(doc.get(&root, "current-only").unwrap().is_none());
    assert_eq!(
        doc.get(&root, "target-only").unwrap().unwrap().0.as_i64(),
        Some(7)
    );
    assert_eq!(selection::content(&doc, &MAP).unwrap(), target_projection);
    doc.commit().unwrap();
    let bytes = doc.clone().save();
    let loaded = AutoCommit::load(&bytes).unwrap();
    assert_eq!(loaded.get(ROOT, "content").unwrap().unwrap().1, root);
    assert_eq!(loaded.get(&root, "unknown-map").unwrap().unwrap().1, map);
}

#[test]
fn whole_kind_replacements_copy_target_and_use_fresh_object_ids() {
    let mut doc = AutoCommit::new();
    let root = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let target_map = doc.put_object(&root, "map", ObjType::Map).unwrap();
    doc.put(&target_map, "value", "map").unwrap();
    let target_list = doc.put_object(&root, "list", ObjType::List).unwrap();
    doc.insert(&target_list, 0, "list").unwrap();
    let target_text = doc.put_object(&root, "text", ObjType::Text).unwrap();
    doc.splice_text(&target_text, 0, 0, "text").unwrap();
    doc.put(&root, "scalar", "scalar").unwrap();
    let target = doc.get_heads();

    let live_map = doc.put_object(&root, "map", ObjType::List).unwrap();
    doc.insert(&live_map, 0, "live").unwrap();
    doc.put(&root, "list", "live-scalar").unwrap();
    let live_text = doc.put_object(&root, "text", ObjType::List).unwrap();
    let _live_scalar = doc.put_object(&root, "scalar", ObjType::Map).unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &MAP, &live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();

    let (map_value, restored_map) = doc.get(&root, "map").unwrap().unwrap();
    let (list_value, restored_list) = doc.get(&root, "list").unwrap().unwrap();
    let (text_value, restored_text) = doc.get(&root, "text").unwrap().unwrap();
    assert!(matches!(map_value, Value::Object(ObjType::Map)));
    assert!(matches!(list_value, Value::Object(ObjType::List)));
    assert!(matches!(text_value, Value::Object(ObjType::Text)));
    assert_ne!(restored_map, live_map);
    assert_ne!(restored_text, live_text);
    assert_ne!(restored_map, target_map);
    assert_ne!(restored_list, target_list);
    assert_ne!(restored_text, target_text);
    assert_eq!(
        doc.get(&restored_map, "value").unwrap().unwrap().0.as_str(),
        Some("map")
    );
    assert_eq!(doc.text(&restored_text).unwrap(), "text");
    assert_eq!(
        doc.get(&root, "scalar").unwrap().unwrap().0.as_str(),
        Some("scalar")
    );
}

#[test]
fn whole_same_key_replacement_keeps_current_id_and_never_revives_old_id() {
    let mut doc = AutoCommit::new();
    let original = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    doc.put(&original, "value", "target").unwrap();
    let target = doc.get_heads();
    doc.delete(ROOT, "content").unwrap();
    let replacement = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    doc.put(&replacement, "value", "target").unwrap();
    let equal_live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &MAP, &equal_live, &target).unwrap();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap(),
        RestoreOutcome::Unchanged
    );
    assert_eq!(doc.pending_ops(), 0);
    assert_eq!(doc.get(ROOT, "content").unwrap().unwrap().1, replacement);
    assert_ne!(replacement, original);

    doc.put(&replacement, "value", "live").unwrap();
    let changed_live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &MAP, &changed_live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert_eq!(doc.get(ROOT, "content").unwrap().unwrap().1, replacement);
    assert_ne!(doc.get(ROOT, "content").unwrap().unwrap().1, original);
    assert_eq!(
        doc.get(&replacement, "value").unwrap().unwrap().0.as_str(),
        Some("target")
    );
}

#[test]
fn whole_optional_map_absence_is_exact_and_recreation_uses_a_fresh_id() {
    static OPTIONAL_MAP: HistoryRestoreSchema = HistoryRestoreSchema {
        id: "test.optional-map",
        version: 1,
        roots: &[HistoryRestoreRoot {
            key: "content",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        }],
        selectors: &[],
        preserved: &[],
        validate_result: |_| Ok(()),
        preserved_roots: &[],
        validate: |_| Ok(()),
    };
    let mut doc = AutoCommit::new();
    let missing = doc.get_heads();
    let historical = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let present = doc.get_heads();
    doc.delete(ROOT, "content").unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &OPTIONAL_MAP, &live, &present).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    let recreated = doc.get(ROOT, "content").unwrap().unwrap().1;
    assert_ne!(recreated, historical);
    doc.commit();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &OPTIONAL_MAP, &live, &missing).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert!(doc.get(ROOT, "content").unwrap().is_none());
}

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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn restore_rebuilds_every_content_type_while_keeping_live_metadata() {
    // phase: restores_text_in_one_owned_pending_transaction_without_losing_history
    // Catches an engine that commits, swaps the live document/actor, or truncates history.
    {
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
    // phase: preserves_current_noncontent_values_objects_and_conflicts
    // Catches restoration escaping the allowlist, or flattening metadata conflicts.
    {
        let (mut doc, _, _) = text_fixture();
        let metadata = doc.put_object(ROOT, "tilo:record", ObjType::Map).unwrap();
        doc.put(&metadata, "id", "record-live").unwrap();
        for key in [
            "future_root",
            "notes_import_v1",
            "tilo:group:a",
            "tilo:run:a",
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
            "tilo:group:a",
            "tilo:run:a",
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
        assert_eq!(doc.get(ROOT, "tilo:record").unwrap().unwrap().1, metadata);
        assert_eq!(doc.get_all(&metadata, "id").unwrap(), conflict);
        for key in [
            "future_root",
            "notes_import_v1",
            "tilo:group:a",
            "tilo:run:a",
            "editor_disk_base",
        ] {
            assert_eq!(
                doc.get(ROOT, key).unwrap().unwrap().0.as_str(),
                Some("current")
            );
        }
    }
    // phase: resurrects_nested_maps_lists_references_and_all_scalar_types
    // Catches lossy scalars, wrong list order, and failed nested resurrection.
    {
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
    // phase: restores_unicode_text_and_exact_typed_marks_in_every_encoding
    // Catches byte/codepoint index confusion and ignored mark-only changes.
    {
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
    // phase: changed_text_restores_inserted_marks_and_removes_new_marks
    {
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
    // phase: protected_rich_text_blocks_and_marks_remain_intact
    {
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn restore_rejects_invalid_targets_before_any_mutation() {
    // phase: rejects_stale_pending_unknown_and_malformed_targets_without_mutation
    {
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
    // phase: rejects_invalid_schema_and_conflicted_target_content
    {
        let (mut doc, target, _) = text_fixture();
        for roots in [
            &[][..],
            &[TEXT.roots[0], TEXT.roots[0]][..],
            &[HistoryRestoreRoot {
                key: "tilo:record",
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
    // phase: rejects_isolated_historical_view_as_live_document
    // An isolated read view must not masquerade as the canonical live frontier.
    {
        let (mut doc, target, _) = text_fixture();
        doc.isolate(&target);
        let mut before = doc.clone();
        assert_eq!(
            prepare_content_restore(&doc, &TEXT, &target, &target).unwrap_err(),
            RestoreError::StaleReview
        );
        assert_unchanged(&doc, &mut before);
    }
    // phase: nested_target_conflict_is_rejected_even_if_current_winner_is_equal
    {
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
    // phase: missing_required_roots_in_live_or_target_fail_without_mutation
    {
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
    // phase: target_rich_text_blocks_fail_before_live_mutation
    {
        let (mut doc, _, _) = text_fixture();
        let text = doc.get(ROOT, "content").unwrap().unwrap().1;
        let block = doc.split_block(&text, 0).unwrap();
        doc.put(block, "kind", "paragraph").unwrap();
        let target = doc.get_heads();
        doc.join_block(&text, 0).unwrap();
        let live = doc.get_heads();
        let mut before = doc.clone();
        let result = prepare_content_restore(&doc, &TEXT, &live, &target);
        assert!(
            matches!(result, Err(RestoreError::UnsupportedContent(_))),
            "unexpected result: {result:?}"
        );
        assert_unchanged(&doc, &mut before);
    }
}

#[test]
fn restore_transactions_track_exact_heads_and_content_identity() {
    // phase: unchanged_content_creates_no_transaction_even_when_metadata_differs
    {
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
    // phase: apply_rechecks_exact_heads_and_refuses_pending_work
    {
        let (mut doc, target, live) = text_fixture();
        let prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
        doc.put(ROOT, "tilo:run:new", "metadata only").unwrap();
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
    // phase: optional_root_creation_deletion_and_unchanged_content_identity
    {
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
    // phase: selected_full_frontier_excludes_unrelated_concurrent_content_and_normalizes_heads
    {
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
fn restore_failures_roll_back_every_prior_write() {
    // phase: trial_validator_failure_does_not_touch_live_state
    {
        let (doc, target, live) = text_fixture();
        let mut before = doc.clone();
        assert!(matches!(
            prepare_content_restore(&doc, &REJECT_TRIAL, &live, &target),
            Err(RestoreError::SchemaInvalid(_))
        ));
        assert_unchanged(&doc, &mut before);
    }
    // phase: live_validator_failure_rolls_back_every_write
    {
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
    // phase: live_postcondition_failure_rolls_back_prior_successful_writes
    // Inject a corrupt private plan to exercise the live postcondition, rather than
    // introducing a production fault-injection hook or mutating the live document.
    {
        use super::projection::{ExactScalar, Node};
        let (mut doc, target, live) = text_fixture();
        let mut prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
        let corrupt = prepared
            .target
            .push_test_node(Node::Scalar(ExactScalar(ScalarValue::Boolean(true))));
        prepared
            .target
            .0
            .insert("z-not-content".into(), vec![corrupt]);
        let mut before = doc.clone();
        assert_eq!(
            apply_prepared_content_restore(&mut doc, &prepared).unwrap_err(),
            RestoreError::ContentPostconditionMismatch
        );
        assert_eq!(content_text(&doc), "two");
        assert!(doc.get(ROOT, "z-not-content").unwrap().is_none());
        assert_unchanged(&doc, &mut before);
    }
    // phase: automerge_write_error_rolls_back_prior_successful_writes
    // A mark on a list provokes a real Automerge InvalidOp only after the root and
    // list element have been written. The malformed plan is private test state.
    {
        use super::projection::{ExactScalar, Node, SemanticMark};
        let (mut doc, target, live) = text_fixture();
        let mut prepared = prepare_content_restore(&doc, &TEXT, &live, &target).unwrap();
        let element = prepared
            .target
            .push_test_node(Node::Scalar(ExactScalar(ScalarValue::Int(1))));
        let corrupt = prepared.target.push_test_node(Node::List(
            vec![vec![element]],
            vec![SemanticMark {
                name: "bad-list-mark".into(),
                start: 0,
                end: 1,
                value: ExactScalar(ScalarValue::Boolean(true)),
            }],
        ));
        prepared.target.0.insert("content".into(), vec![corrupt]);
        let mut before = doc.clone();
        assert!(matches!(
            apply_prepared_content_restore(&mut doc, &prepared),
            Err(RestoreError::Automerge(_))
        ));
        assert_eq!(content_text(&doc), "two");
        assert_unchanged(&doc, &mut before);
    }
}

#[test]
fn selected_text_conflicts_preserve_error_precedence_and_live_state() {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "content", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "x").unwrap();
    doc.commit().unwrap();
    let clean_target = doc.get_heads();

    let mut peer = doc.fork();
    doc.put(&text, 0, "a").unwrap();
    peer.put(&text, 0, "b").unwrap();
    doc.merge(&mut peer).unwrap();
    let conflicted = doc.get_heads();
    assert_eq!(doc.get_all(&text, 0).unwrap().len(), 2);
    let mut before = doc.clone();
    assert_eq!(
        prepare_content_restore(&doc, &TEXT, &conflicted, &clean_target).unwrap_err(),
        RestoreError::UnsupportedConflict("content/0".into())
    );
    assert_unchanged(&doc, &mut before);

    doc.put(&text, 0, "current").unwrap();
    let live = doc.get_heads();
    assert_eq!(doc.get_all(&text, 0).unwrap().len(), 1);
    let mut before = doc.clone();
    assert_eq!(
        prepare_content_restore(&doc, &TEXT, &live, &conflicted).unwrap_err(),
        RestoreError::UnsupportedConflict("content/0".into())
    );
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

fn deep_writer_fixture() -> (
    AutoCommit,
    Vec<ChangeHash>,
    Vec<ChangeHash>,
    Vec<ObjId>,
    ObjId,
    ObjId,
    ObjId,
) {
    let mut doc = AutoCommit::new();
    let content = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    let mut parent = content.clone();
    let mut ancestors = Vec::with_capacity(254);
    for index in 0..254 {
        parent = doc
            .put_object(&parent, format!("d{index}"), ObjType::Map)
            .unwrap();
        ancestors.push(parent.clone());
    }
    let prior = doc.put_object(&parent, "a-map", ObjType::Map).unwrap();
    doc.put(&prior, "leaf", "target").unwrap();
    let list = doc.put_object(&parent, "z-list", ObjType::List).unwrap();
    doc.insert(&list, 0, "target-list").unwrap();
    let protected = doc.put_object(ROOT, "protected", ObjType::Map).unwrap();
    let protected_map = doc.put_object(&protected, "map", ObjType::Map).unwrap();
    doc.put(&protected_map, "value", "protected").unwrap();
    let target = doc.get_heads();
    doc.put(&prior, "leaf", "current").unwrap();
    doc.put(&list, 0, "current-list").unwrap();
    let live = doc.get_heads();
    (
        doc,
        target,
        live,
        ancestors,
        content,
        protected,
        protected_map,
    )
}

fn assert_deep_writer_snapshot(
    doc: &AutoCommit,
    before: &[u8],
    heads: &[ChangeHash],
    actor: &automerge::ActorId,
    ancestors: &[ObjId],
    content: &ObjId,
    protected: &ObjId,
    protected_map: &ObjId,
) {
    assert_eq!(doc.base_heads(), heads);
    assert_eq!(doc.get_actor(), actor);
    assert_eq!(doc.pending_ops(), 0);
    assert_eq!(doc.clone().save(), before);
    assert_eq!(doc.get(ROOT, "content").unwrap().unwrap().1, *content);
    let mut retained = content.clone();
    for (index, expected) in ancestors.iter().enumerate() {
        retained = doc.get(&retained, format!("d{index}")).unwrap().unwrap().1;
        assert_eq!(&retained, expected);
    }
    assert_eq!(doc.get(ROOT, "protected").unwrap().unwrap().1, *protected);
    assert_eq!(
        doc.get(protected, "map").unwrap().unwrap().1,
        *protected_map
    );
    assert_eq!(
        doc.get(protected_map, "value").unwrap().unwrap().0.as_str(),
        Some("protected")
    );
}

#[test]
fn depth_boundary_writer_and_postcondition_failures_roll_back_exact_state() {
    std::thread::Builder::new()
        .name("depth-boundary-error-rollback".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(depth_boundary_writer_and_postcondition_failures_roll_back_exact_state_inner)
        .unwrap()
        .join()
        .unwrap();
}

fn depth_boundary_writer_and_postcondition_failures_roll_back_exact_state_inner() {
    use super::projection::{ExactScalar, Node, SemanticMark};

    let (mut writer, target, live, ancestors, content, protected, protected_map) =
        deep_writer_fixture();
    writer.set_actor(automerge::ActorId::from(b"deep-writer".as_slice()));
    let mut prepared =
        prepare_content_restore(&writer, &DEEP_MAP_WITH_PROTECTED_ROOT, &live, &target).unwrap();
    let [node] = prepared.target.0["content"].as_slice() else {
        panic!("expected projected content")
    };
    let mut node = *node;
    for index in 0..254 {
        let Node::Map(_, fields) = &prepared.target.1[node] else {
            panic!("expected projected boundary map")
        };
        node = fields[&format!("d{index}")][0];
    }
    let Node::Map(_, fields) = &prepared.target.1[node] else {
        panic!("expected projected deepest map")
    };
    let list_node = fields["z-list"][0];
    let Node::List(_, marks) = &mut prepared.target.1[list_node] else {
        panic!("expected projected list")
    };
    marks.push(SemanticMark {
        name: "bad-list-mark".into(),
        start: 0,
        end: 1,
        value: ExactScalar(ScalarValue::Boolean(true)),
    });
    let before_heads = writer.base_heads();
    let before_actor = writer.get_actor().clone();
    let before = writer.clone().save();
    assert!(matches!(
        apply_prepared_content_restore(&mut writer, &prepared),
        Err(RestoreError::Automerge(_))
    ));
    assert_deep_writer_snapshot(
        &writer,
        &before,
        &before_heads,
        &before_actor,
        &ancestors,
        &content,
        &protected,
        &protected_map,
    );
    drop(prepared);
    drop(writer);

    let (mut postcondition, target, live, ancestors, content, protected, protected_map) =
        deep_writer_fixture();
    postcondition.set_actor(automerge::ActorId::from(b"deep-postcondition".as_slice()));
    let mut prepared = prepare_content_restore(
        &postcondition,
        &DEEP_MAP_WITH_PROTECTED_ROOT,
        &live,
        &target,
    )
    .unwrap();
    let corrupt = prepared
        .target
        .push_test_node(Node::Scalar(ExactScalar(ScalarValue::Boolean(true))));
    prepared
        .target
        .0
        .insert("z-not-content".into(), vec![corrupt]);
    let before_heads = postcondition.base_heads();
    let before_actor = postcondition.get_actor().clone();
    let before = postcondition.clone().save();
    assert_eq!(
        apply_prepared_content_restore(&mut postcondition, &prepared).unwrap_err(),
        RestoreError::ContentPostconditionMismatch
    );
    assert_deep_writer_snapshot(
        &postcondition,
        &before,
        &before_heads,
        &before_actor,
        &ancestors,
        &content,
        &protected,
        &protected_map,
    );
    drop(prepared);
    drop(postcondition);
}

#[test]
fn records_containing_table_objects_are_refused_without_mutation() {
    let mut doc = AutoCommit::new();
    let root = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
    doc.put(&root, "value", "target").unwrap();
    doc.put_object(ROOT, "table", ObjType::Table).unwrap();
    let target = doc.get_heads();
    doc.put(&root, "value", "live").unwrap();
    let live = doc.get_heads();
    let before = doc.save();
    assert!(matches!(
        prepare_content_restore(&doc, &MAP, &live, &target),
        Err(RestoreError::UnsupportedContent(_))
    ));
    assert_eq!(doc.save(), before);
}

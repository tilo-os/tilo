use super::projection::{ExactScalar, Node};
use super::*;
use automerge::{
    ObjId, ROOT, ReadDoc, ScalarValue,
    marks::{ExpandMark, Mark},
    transaction::Transactable,
};

const STRING: HistoryRestoreNode = HistoryRestoreNode::Whole {
    expected_type: RestoreValueType::Scalar(ScalarType::String),
};
const WHOLE_MAP: HistoryRestoreNode = HistoryRestoreNode::Whole {
    expected_type: RestoreValueType::Object(ObjType::Map),
};
const WHOLE_MAP_FIELD: HistoryRestoreNode = HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
    fields: &[HistoryMapField {
        key: "payload",
        required: true,
        node: WHOLE_MAP,
    }],
    entries: None,
});
const WHOLE_MAP_ENTRIES: HistoryRestoreNode = HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
    fields: &[],
    entries: Some(&HistoryMapEntries {
        key_limit_bytes: 45,
        node: WHOLE_MAP,
        membership: HistoryMembership::TargetAuthoritative,
        on_create: PreservedOnCreate::Omit,
    }),
});
const CONTENT_EXCEPT_RUNTIME: HistoryRestoreNode = HistoryRestoreNode::MapContentExcept {
    preserved_fields: &["presence", "last_used", "historic_missing", "runtime_text"],
};
static CONTENT_EXCEPT: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "content-except",
    version: 1,
    roots: &[],
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("profile"),
        required: true,
        node: CONTENT_EXCEPT_RUNTIME,
    }],
    preserved_roots: &[],
    preserved: &[],
    validate: |_| Ok(()),
    validate_result: |_| Ok(()),
};

#[test]
fn map_content_except_restores_authored_keys_and_preserves_excluded_state() {
    let mut doc = AutoCommit::new();
    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let presence = doc.put_object(&profile, "presence", ObjType::Map).unwrap();
    doc.put(&profile, "name", "target").unwrap();
    doc.put(&profile, "target-only", 7_i64).unwrap();
    doc.put(&profile, "last_used", 1_i64).unwrap();
    doc.put(&presence, "status", "historic").unwrap();
    let target = doc.get_heads();
    doc.put(&profile, "name", "live").unwrap();
    doc.delete(&profile, "target-only").unwrap();
    doc.put(&profile, "current-only", true).unwrap();
    doc.put(&profile, "last_used", 9_i64).unwrap();
    doc.put(&presence, "status", "current").unwrap();
    assert_eq!(
        restore(&mut doc, &CONTENT_EXCEPT, &target),
        RestoreOutcome::Changed
    );
    assert_eq!(object(&doc, &ROOT, "profile"), profile);
    assert_eq!(object(&doc, &profile, "presence"), presence);
    assert_eq!(
        doc.get(&profile, "name").unwrap().unwrap().0.as_str(),
        Some("target")
    );
    assert_eq!(
        doc.get(&profile, "target-only")
            .unwrap()
            .unwrap()
            .0
            .as_i64(),
        Some(7)
    );
    assert!(doc.get(&profile, "current-only").unwrap().is_none());
    assert_eq!(
        doc.get(&profile, "last_used").unwrap().unwrap().0.as_i64(),
        Some(9)
    );
    assert_eq!(
        doc.get(&presence, "status").unwrap().unwrap().0.as_str(),
        Some("current")
    );
}

const SHEETS_NODE: HistoryRestoreNode = HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
    fields: &[HistoryMapField {
        key: "sheets",
        required: false,
        node: HistoryRestoreNode::Whole {
            expected_type: RestoreValueType::Object(ObjType::List),
        },
    }],
    entries: None,
});
static SHEETS: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "sheets",
    version: 2,
    roots: &[],
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("wb"),
        required: false,
        node: SHEETS_NODE,
    }],
    preserved_roots: &[],
    preserved: &[],
    validate: |_| Ok(()),
    validate_result: |_| Ok(()),
};
fn object(doc: &AutoCommit, parent: &ObjId, key: &str) -> ObjId {
    doc.get(parent, key).unwrap().unwrap().1
}
fn restore(
    doc: &mut AutoCommit,
    schema: &'static HistoryRestoreSchema,
    target: &[ChangeHash],
) -> RestoreOutcome {
    let live = doc.get_heads();
    let prepared = prepare_content_restore(doc, schema, &live, target).unwrap();
    apply_prepared_content_restore(doc, &prepared).unwrap()
}
fn sheets_fixture() -> (AutoCommit, ObjId, Vec<ChangeHash>) {
    let mut doc = AutoCommit::new();
    let wb = doc.put_object(ROOT, "wb", ObjType::Map).unwrap();
    let sheets = doc.put_object(&wb, "sheets", ObjType::List).unwrap();
    doc.insert(&sheets, 0, "sheet-a").unwrap();
    doc.put(&wb, "active", 0_i64).unwrap();
    let target = doc.get_heads();
    doc.insert(&sheets, 1, "sheet-b").unwrap();
    doc.put(&wb, "active", 1_i64).unwrap();
    (doc, wb, target)
}

#[test]
fn selectors_restore_only_selected_children_of_mixed_containers() {
    // phase: sheets_shorter_list_preserves_mixed_parent_and_current_active
    // Replacing wb would lose both its identity and the current presentation state.
    {
        let (mut doc, wb, target) = sheets_fixture();
        let active = doc
            .get_all(&wb, "active")
            .unwrap()
            .into_iter()
            .map(|(v, id)| (v.into_owned(), id))
            .collect::<Vec<_>>();
        assert_eq!(restore(&mut doc, &SHEETS, &target), RestoreOutcome::Changed);
        assert_eq!(object(&doc, &ROOT, "wb"), wb);
        assert_eq!(doc.get_all(&wb, "active").unwrap(), active);
        let sheets = object(&doc, &wb, "sheets");
        assert_eq!(doc.length(&sheets), 1);
        assert_eq!(
            doc.get(&sheets, 0).unwrap().unwrap().0.as_str(),
            Some("sheet-a")
        );
        assert!(doc.pending_ops() > 0);
    }
    // phase: absent_structural_parent_deletes_only_optional_selected_child
    {
        let (mut doc, wb, _) = sheets_fixture();
        assert_eq!(restore(&mut doc, &SHEETS, &[]), RestoreOutcome::Changed);
        assert_eq!(object(&doc, &ROOT, "wb"), wb);
        assert!(doc.get(&wb, "sheets").unwrap().is_none());
        assert_eq!(doc.get(&wb, "active").unwrap().unwrap().0.as_i64(), Some(1));
    }
    // phase: creates_absent_mixed_container_with_selected_children_only
    // A missing live structural container must not import its historical view state.
    {
        let (mut doc, _, target) = sheets_fixture();
        doc.delete(ROOT, "wb").unwrap();
        assert_eq!(restore(&mut doc, &SHEETS, &target), RestoreOutcome::Changed);
        let wb = object(&doc, &ROOT, "wb");
        assert_eq!(doc.length(object(&doc, &wb, "sheets")), 1);
        assert!(doc.get(&wb, "active").unwrap().is_none());
    }
    // phase: optional_selected_child_absence_preserves_present_parent_and_unknown_children
    {
        let mut doc = AutoCommit::new();
        let wb = doc.put_object(ROOT, "wb", ObjType::Map).unwrap();
        doc.put(&wb, "active", 0_i64).unwrap();
        let target = doc.get_heads();
        doc.put_object(&wb, "sheets", ObjType::List).unwrap();
        doc.put(&wb, "future-view", "current").unwrap();
        restore(&mut doc, &SHEETS, &target);
        assert_eq!(object(&doc, &ROOT, "wb"), wb);
        assert!(doc.get(&wb, "sheets").unwrap().is_none());
        assert_eq!(
            doc.get(&wb, "future-view").unwrap().unwrap().0.as_str(),
            Some("current")
        );
    }
    // phase: omission_is_admitted_only_when_combined_result_validator_accepts_it
    {
        let mut doc = AutoCommit::new();
        let connections = doc.put_object(ROOT, "connections", ObjType::Map).unwrap();
        profile(&mut doc, &connections, "stable-id", "historical");
        let target = doc.get_heads();
        doc.delete(&connections, "stable-id").unwrap();
        let live = doc.get_heads();
        let before = doc.save();
        assert!(matches!(
            prepare_content_restore(&doc, &REQUIRE_PRESENCE, &live, &target),
            Err(RestoreError::SchemaInvalid(_))
        ));
        assert_eq!(doc.save(), before);
        assert_eq!(doc.pending_ops(), 0);
    }
}

#[test]
fn selectors_keep_protected_content_and_refuse_selected_conflicts() {
    // phase: nested_protected_historical_differences_are_noop
    {
        let (mut doc, wb, _) = sheets_fixture();
        let target = doc.get_heads();
        doc.put(&wb, "active", 99_i64).unwrap();
        let sheets = object(&doc, &wb, "sheets");
        assert_eq!(
            restore(&mut doc, &SHEETS, &target),
            RestoreOutcome::Unchanged
        );
        assert_eq!(doc.pending_ops(), 0);
        assert_eq!(object(&doc, &wb, "sheets"), sheets);
    }
    // phase: nested_selected_conflicts_refuse_but_protected_conflicts_retain_every_op
    {
        let (mut doc, wb, target) = sheets_fixture();
        doc.commit();
        let mut peer = doc.fork();
        doc.put(&wb, "active", 2_i64).unwrap();
        peer.put(&wb, "active", 3_i64).unwrap();
        doc.merge(&mut peer).unwrap();
        let protected = doc
            .get_all(&wb, "active")
            .unwrap()
            .into_iter()
            .map(|(v, id)| (v.into_owned(), id))
            .collect::<Vec<_>>();
        assert_eq!(protected.len(), 2);
        restore(&mut doc, &SHEETS, &target);
        assert_eq!(doc.get_all(&wb, "active").unwrap(), protected);
        doc.commit();
        let mut peer = doc.fork();
        doc.put_object(&wb, "sheets", ObjType::List).unwrap();
        peer.put_object(&wb, "sheets", ObjType::List).unwrap();
        doc.merge(&mut peer).unwrap();
        let live = doc.get_heads();
        assert!(matches!(
            prepare_content_restore(&doc, &SHEETS, &live, &target),
            Err(RestoreError::UnsupportedConflict(_))
        ));
        assert_eq!(doc.pending_ops(), 0);
    }
    // phase: recursive_overlays_keep_every_mixed_ancestor_and_protected_object
    {
        let mut doc = AutoCommit::new();
        let document = doc.put_object(ROOT, "document", ObjType::Map).unwrap();
        let wb = doc.put_object(&document, "wb", ObjType::Map).unwrap();
        let sheets = doc.put_object(&wb, "sheets", ObjType::List).unwrap();
        doc.insert(&sheets, 0, "original").unwrap();
        let target = doc.get_heads();
        let view = doc.put_object(&document, "view", ObjType::Map).unwrap();
        let selection = doc.put_object(&view, "selection", ObjType::List).unwrap();
        doc.insert(&selection, 0, "protected-id").unwrap();
        doc.put(&wb, "active", 7_i64).unwrap();
        doc.put(&sheets, 0, "edited").unwrap();
        restore(&mut doc, &NESTED, &target);
        assert_eq!(object(&doc, &ROOT, "document"), document);
        assert_eq!(object(&doc, &document, "wb"), wb);
        assert_eq!(object(&doc, &document, "view"), view);
        assert_eq!(object(&doc, &view, "selection"), selection);
        assert_eq!(
            doc.get(&selection, 0).unwrap().unwrap().0.as_str(),
            Some("protected-id")
        );
        assert_eq!(doc.get(&wb, "active").unwrap().unwrap().0.as_i64(), Some(7));
        let sheets = object(&doc, &wb, "sheets");
        assert_eq!(
            doc.get(&sheets, 0).unwrap().unwrap().0.as_str(),
            Some("original")
        );
    }
    // phase: reject_policy_refuses_new_entity_but_allows_retained_profile_edits
    {
        let mut doc = AutoCommit::new();
        let connections = doc.put_object(ROOT, "connections", ObjType::Map).unwrap();
        let retained = profile(&mut doc, &connections, "stable-id", "original");
        let target = doc.get_heads();
        doc.put(&retained, "name", "edited").unwrap();
        let live = doc.get_heads();
        assert!(prepare_content_restore(&doc, &REJECT_RESURRECTION, &live, &target).is_ok());
        doc.delete(&connections, "stable-id").unwrap();
        let live = doc.get_heads();
        assert!(matches!(
            prepare_content_restore(&doc, &REJECT_RESURRECTION, &live, &target),
            Err(RestoreError::SchemaInvalid(_))
        ));
        assert_eq!(doc.pending_ops(), 0);
    }
}

const PROFILE: HistoryRestoreNode = HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
    fields: &[
        HistoryMapField {
            key: "name",
            required: true,
            node: STRING,
        },
        HistoryMapField {
            key: "host",
            required: true,
            node: STRING,
        },
        HistoryMapField {
            key: "port",
            required: true,
            node: HistoryRestoreNode::Whole {
                expected_type: RestoreValueType::Scalar(ScalarType::Int),
            },
        },
        HistoryMapField {
            key: "user",
            required: true,
            node: STRING,
        },
        HistoryMapField {
            key: "kind",
            required: true,
            node: STRING,
        },
        HistoryMapField {
            key: "auth",
            required: true,
            node: STRING,
        },
        HistoryMapField {
            key: "group",
            required: true,
            node: STRING,
        },
        HistoryMapField {
            key: "favorite",
            required: true,
            node: HistoryRestoreNode::Whole {
                expected_type: RestoreValueType::Scalar(ScalarType::Boolean),
            },
        },
    ],
    entries: None,
});
const CONNECTIONS: HistoryRestoreNode = HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
    fields: &[],
    entries: Some(&HistoryMapEntries {
        key_limit_bytes: 128,
        node: PROFILE,
        membership: HistoryMembership::TargetAuthoritative,
        on_create: PreservedOnCreate::Omit,
    }),
});
static REMOTE: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "remote",
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("connections"),
        required: false,
        node: CONNECTIONS,
    }],
    ..SHEETS
};
fn profile(doc: &mut AutoCommit, connections: &ObjId, key: &str, name: &str) -> ObjId {
    let id = doc.put_object(connections, key, ObjType::Map).unwrap();
    for (key, value) in [
        ("name", name),
        ("host", "host.example"),
        ("user", "user"),
        ("kind", "ssh"),
        ("auth", "agent"),
        ("group", "work"),
    ] {
        doc.put(&id, key, value).unwrap();
    }
    doc.put(&id, "port", 22_i64).unwrap();
    doc.put(&id, "favorite", true).unwrap();
    doc.put(&id, "last_used", 123_i64).unwrap();
    let presence = doc.put_object(&id, "presence", ObjType::Map).unwrap();
    doc.put(&presence, "status", "historic-online").unwrap();
    id
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn selectors_preserve_identity_membership_and_roll_back_failures() {
    // phase: remote_retains_operational_identity_deletes_entities_and_resurrects_content_only
    {
        let mut doc = AutoCommit::new();
        let connections = doc.put_object(ROOT, "connections", ObjType::Map).unwrap();
        let retained = profile(&mut doc, &connections, "retained", "old-name");
        profile(&mut doc, &connections, "resurrect", "old-resurrect");
        let target = doc.get_heads();
        doc.put(&retained, "name", "new-name").unwrap();
        doc.put(&retained, "last_used", 999_i64).unwrap();
        let presence = object(&doc, &retained, "presence");
        doc.put(&presence, "status", "live-online").unwrap();
        doc.delete(&connections, "resurrect").unwrap();
        profile(&mut doc, &connections, "delete", "new-delete");
        assert_eq!(restore(&mut doc, &REMOTE, &target), RestoreOutcome::Changed);
        assert_eq!(object(&doc, &ROOT, "connections"), connections);
        assert_eq!(object(&doc, &connections, "retained"), retained);
        assert_eq!(
            doc.get(&retained, "name").unwrap().unwrap().0.as_str(),
            Some("old-name")
        );
        assert_eq!(
            doc.get(&retained, "last_used").unwrap().unwrap().0.as_i64(),
            Some(999)
        );
        assert_eq!(object(&doc, &retained, "presence"), presence);
        assert_eq!(
            doc.get(&presence, "status").unwrap().unwrap().0.as_str(),
            Some("live-online")
        );
        assert!(doc.get(&connections, "delete").unwrap().is_none());
        let resurrect = object(&doc, &connections, "resurrect");
        assert_eq!(
            doc.get(&resurrect, "name").unwrap().unwrap().0.as_str(),
            Some("old-resurrect")
        );
        assert!(doc.get(&resurrect, "presence").unwrap().is_none());
        assert!(doc.get(&resurrect, "last_used").unwrap().is_none());
    }
    // phase: browser_add_remove_membership_and_tombstone_resurrection
    {
        let mut doc = AutoCommit::new();
        let add = doc
            .put_object(ROOT, "browser.bookmarks.v2.add:tag-a", ObjType::Map)
            .unwrap();
        doc.put(&add, "url", "https://example.test").unwrap();
        doc.put(ROOT, "browser.bookmarks.v2.remove:tag-b", true)
            .unwrap();
        doc.put(ROOT, "browser.bookmarks.v2.migrated", false)
            .unwrap();
        let target = doc.get_heads();
        doc.put(ROOT, "browser.bookmarks.v2.remove:tag-a", true)
            .unwrap();
        doc.delete(ROOT, "browser.bookmarks.v2.remove:tag-b")
            .unwrap();
        doc.put_object(ROOT, "browser.bookmarks.v2.add:tag-new", ObjType::Map)
            .unwrap();
        doc.delete(ROOT, "browser.bookmarks.v2.add:tag-a").unwrap();
        doc.put(ROOT, "browser.bookmarks.v2.migrated", true)
            .unwrap();
        assert_eq!(
            restore(&mut doc, &BROWSER, &target),
            RestoreOutcome::Changed
        );
        assert!(
            doc.get(ROOT, "browser.bookmarks.v2.remove:tag-a")
                .unwrap()
                .is_none()
        );
        assert!(
            doc.get(ROOT, "browser.bookmarks.v2.add:tag-new")
                .unwrap()
                .is_none()
        );
        let add = object(&doc, &ROOT, "browser.bookmarks.v2.add:tag-a");
        assert_eq!(
            doc.get(&add, "url").unwrap().unwrap().0.as_str(),
            Some("https://example.test")
        );
        assert_eq!(
            doc.get(ROOT, "browser.bookmarks.v2.remove:tag-b")
                .unwrap()
                .unwrap()
                .0
                .as_bool(),
            Some(true)
        );
        assert_eq!(
            doc.get(ROOT, "browser.bookmarks.v2.migrated")
                .unwrap()
                .unwrap()
                .0
                .as_bool(),
            Some(true)
        );
    }
    // phase: real_browser_tags_and_unicode_family_identities_restore
    // Browser's actual tags contain colons; UTF-8 identity captures are byte-bounded.
    {
        let mut doc = AutoCommit::new();
        for key in [
            "browser.bookmarks.v2.add:bookmark:0123456789abcdef",
            "browser.bookmarks.v2.add:legacy:0123456789abcdef",
        ] {
            let add = doc.put_object(ROOT, key, ObjType::Map).unwrap();
            doc.put(&add, "url", "https://example.test").unwrap();
        }
        let target = doc.get_heads();
        doc.put(
            ROOT,
            "browser.bookmarks.v2.remove:bookmark:0123456789abcdef",
            true,
        )
        .unwrap();
        assert_eq!(
            restore(&mut doc, &BROWSER, &target),
            RestoreOutcome::Changed
        );
        assert!(
            doc.get(
                ROOT,
                "browser.bookmarks.v2.remove:bookmark:0123456789abcdef"
            )
            .unwrap()
            .is_none()
        );
        let unicode = schema(vec![family("draft.", ".text", 2, STRING)]);
        doc.put(ROOT, "draft.é.text", "original").unwrap();
        let target = doc.get_heads();
        doc.put(ROOT, "draft.é.text", "edited").unwrap();
        assert_eq!(restore(&mut doc, unicode, &target), RestoreOutcome::Changed);
        assert_eq!(
            doc.get(ROOT, "draft.é.text").unwrap().unwrap().0.as_str(),
            Some("original")
        );
        let overlap = schema(vec![
            unicode.selectors[0],
            HistoryRootSelector {
                pattern: HistoryRootPattern::Exact("draft.é.text"),
                required: false,
                node: STRING,
            },
        ]);
        let live = doc.get_heads();
        assert!(matches!(
            prepare_content_restore(&doc, overlap, &live, &target),
            Err(RestoreError::InvalidSchema(_))
        ));
    }
    // phase: combined_result_validator_preflight_and_live_failure_roll_back_all_writes
    {
        let (mut doc, wb, target) = sheets_fixture();
        let live = doc.get_heads();
        let before = doc.save();
        assert!(matches!(
            prepare_content_restore(&doc, &REFUSE_TRIAL, &live, &target),
            Err(RestoreError::SchemaInvalid(_))
        ));
        assert_eq!(doc.save(), before);
        doc.set_actor(automerge::ActorId::from(b"selector-live".as_slice()));
        let prepared = prepare_content_restore(&doc, &REFUSE_LIVE, &live, &target).unwrap();
        let before = doc.save();
        assert!(matches!(
            apply_prepared_content_restore(&mut doc, &prepared),
            Err(RestoreError::SchemaInvalid(_))
        ));
        assert_eq!(doc.pending_ops(), 0);
        assert_eq!(doc.save(), before);
        assert_eq!(object(&doc, &ROOT, "wb"), wb);
    }
}

const fn family(
    prefix: &'static str,
    suffix: &'static str,
    bound: usize,
    node: HistoryRestoreNode,
) -> HistoryRootSelector {
    HistoryRootSelector {
        pattern: HistoryRootPattern::Affix {
            prefix,
            suffix,
            max_capture_bytes: bound,
        },
        required: false,
        node,
    }
}
static BROWSER: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "browser",
    selectors: &[
        family(
            "browser.bookmarks.v2.add:",
            "",
            128,
            HistoryRestoreNode::Whole {
                expected_type: RestoreValueType::Object(ObjType::Map),
            },
        ),
        family(
            "browser.bookmarks.v2.remove:",
            "",
            128,
            HistoryRestoreNode::Whole {
                expected_type: RestoreValueType::Scalar(ScalarType::Boolean),
            },
        ),
    ],
    preserved_roots: &["browser.bookmarks", "browser.bookmarks.v2.migrated"],
    ..SHEETS
};

fn reject_pending(doc: &AutoCommit) -> Result<(), String> {
    if doc.pending_ops() > 0 {
        Err("combined result refused".into())
    } else {
        Ok(())
    }
}
fn reject_live(doc: &AutoCommit) -> Result<(), String> {
    if doc.get_actor() == &automerge::ActorId::from(b"selector-live".as_slice()) {
        reject_pending(doc)
    } else {
        Ok(())
    }
}
static REFUSE_TRIAL: HistoryRestoreSchema = HistoryRestoreSchema {
    validate_result: reject_pending,
    ..SHEETS
};
static REFUSE_LIVE: HistoryRestoreSchema = HistoryRestoreSchema {
    validate_result: reject_live,
    ..SHEETS
};

fn schema(selectors: Vec<HistoryRootSelector>) -> &'static HistoryRestoreSchema {
    Box::leak(Box::new(HistoryRestoreSchema {
        selectors: Box::leak(selectors.into_boxed_slice()),
        ..SHEETS
    }))
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn selector_families_and_overlays_are_validated_at_registration_and_capture() {
    // phase: families_accept_disjoint_suffixes_and_reject_static_intersections
    // Valid suffix-separated families must work; registrations admitting shared keys
    // or protected keys must fail independently of whether that key exists today.
    {
        let good = family("notifications.app.", ".banners", 32, STRING);
        let disjoint = family("notifications.app.", ".sounds", 32, STRING);
        let mut doc = AutoCommit::new();
        let target = doc.get_heads();
        doc.put(ROOT, "notifications.app.org.example.banners", "yes")
            .unwrap();
        doc.put(ROOT, "notifications.app.org.example.sounds", "no")
            .unwrap();
        let live = doc.get_heads();
        let accepted = schema(vec![good, disjoint]);
        assert!(prepare_content_restore(&doc, accepted, &live, &target).is_ok());
        for bad in [
            vec![good, good],
            vec![
                good,
                HistoryRootSelector {
                    pattern: HistoryRootPattern::Exact("notifications.app.a.banners"),
                    ..good
                },
            ],
            vec![good, family("notifications.app.a", ".banners", 32, STRING)],
            vec![family("tilo:", "", 32, STRING)],
            vec![HistoryRootSelector {
                pattern: HistoryRootPattern::Exact("tilo:schema_version"),
                required: false,
                node: STRING,
            }],
            vec![family("", "", 32, STRING)],
            vec![family("content.", "", 0, STRING)],
            vec![family("content.", "", 257, STRING)],
        ] {
            assert!(matches!(
                prepare_content_restore(&doc, schema(bad), &live, &target),
                Err(RestoreError::InvalidSchema(_))
            ));
        }
        let preserved = Box::leak(Box::new(HistoryRestoreSchema {
            selectors: accepted.selectors,
            preserved: &[HistoryRootPattern::Affix {
                prefix: "notifications.app.a",
                suffix: ".banners",
                max_capture_bytes: 32,
            }],
            ..SHEETS
        }));
        assert!(matches!(
            prepare_content_restore(&doc, preserved, &live, &target),
            Err(RestoreError::InvalidSchema(_))
        ));
    }
    // phase: family_capture_bounds_and_required_membership_are_checked_on_current_and_target
    {
        let bounded = schema(vec![family("draft.", "", 3, STRING)]);
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "draft.abc", "old").unwrap();
        let target = doc.get_heads();
        doc.put(ROOT, "draft.abc", "new").unwrap();
        let live = doc.get_heads();
        assert!(prepare_content_restore(&doc, bounded, &live, &target).is_ok());
        for key in ["draft.", "draft.abcd", "draft.🦀", "draft.a\n"] {
            let mut invalid = doc.clone();
            invalid.put(ROOT, key, "bad").unwrap();
            let bad = invalid.get_heads();
            assert!(matches!(
                prepare_content_restore(&invalid, bounded, &bad, &target),
                Err(RestoreError::SchemaInvalid(_))
            ));
            invalid.delete(ROOT, key).unwrap();
            let now = invalid.get_heads();
            assert!(matches!(
                prepare_content_restore(&invalid, bounded, &now, &bad),
                Err(RestoreError::SchemaInvalid(_))
            ));
        }
        let required = schema(vec![HistoryRootSelector {
            required: true,
            ..bounded.selectors[0]
        }]);
        assert!(matches!(
            prepare_content_restore(&doc, required, &live, &[]),
            Err(RestoreError::SchemaInvalid(_))
        ));
    }
    // phase: invalid_overlay_shapes_and_recursive_static_graphs_fail_registration
    {
        let mut doc = AutoCommit::new();
        let heads = doc.get_heads();
        assert!(prepare_content_restore(&doc, &SHEETS, &heads, &heads).is_ok());
        for node in [
            HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
                fields: &[],
                entries: None,
            }),
            HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
                fields: &[
                    HistoryMapField {
                        key: "same",
                        required: false,
                        node: STRING,
                    },
                    HistoryMapField {
                        key: "same",
                        required: false,
                        node: STRING,
                    },
                ],
                entries: None,
            }),
            HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
                fields: &[],
                entries: Some(&HistoryMapEntries {
                    key_limit_bytes: 0,
                    node: STRING,
                    membership: HistoryMembership::TargetAuthoritative,
                    on_create: PreservedOnCreate::Omit,
                }),
            }),
            HistoryRestoreNode::MapOverlay(&CYCLIC),
        ] {
            let invalid = schema(vec![HistoryRootSelector {
                node,
                ..SHEETS.selectors[0]
            }]);
            assert!(matches!(
                prepare_content_restore(&doc, invalid, &heads, &heads),
                Err(RestoreError::InvalidSchema(_))
            ));
        }
    }
}

static CYCLIC: HistoryMapOverlay = HistoryMapOverlay {
    fields: &[HistoryMapField {
        key: "cycle",
        required: false,
        node: HistoryRestoreNode::MapOverlay(&CYCLIC),
    }],
    entries: None,
};

static REJECT_RESURRECTION: HistoryRestoreSchema = HistoryRestoreSchema {
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("connections"),
        required: false,
        node: HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
            fields: &[],
            entries: Some(&HistoryMapEntries {
                key_limit_bytes: 128,
                node: PROFILE,
                membership: HistoryMembership::TargetAuthoritative,
                on_create: PreservedOnCreate::Reject,
            }),
        }),
    }],
    ..REMOTE
};

static NESTED: HistoryRestoreSchema = HistoryRestoreSchema {
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("document"),
        required: false,
        node: HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
            fields: &[HistoryMapField {
                key: "wb",
                required: false,
                node: SHEETS_NODE,
            }],
            entries: None,
        }),
    }],
    ..SHEETS
};

fn require_operational_presence(doc: &AutoCommit) -> Result<(), String> {
    if let Some((_, connections)) = doc
        .get(ROOT, "connections")
        .map_err(|err| err.to_string())?
    {
        for key in doc.keys(&connections) {
            let (_, profile) = doc
                .get(&connections, key)
                .map_err(|err| err.to_string())?
                .ok_or("missing profile")?;
            if doc
                .get(&profile, "presence")
                .map_err(|err| err.to_string())?
                .is_none()
            {
                return Err("this app requires operational presence for every identity".into());
            }
        }
    }
    Ok(())
}
static REQUIRE_PRESENCE: HistoryRestoreSchema = HistoryRestoreSchema {
    validate_result: require_operational_presence,
    ..REMOTE
};

fn string_at(doc: &AutoCommit, object: &ObjId, key: &str) -> Option<String> {
    doc.get(object, key)
        .unwrap()
        .unwrap()
        .0
        .as_str()
        .map(str::to_owned)
}

/// Runs a depth-boundary case on a thread with an explicit 2 MiB stack.
fn on_deep_stack(name: &str, case: fn()) {
    std::thread::Builder::new()
        .name(name.into())
        .stack_size(2 * 1024 * 1024)
        .spawn(case)
        .unwrap()
        .join()
        .unwrap();
}

const FIXED_COMPLEMENT: HistoryRestoreNode = HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
    fields: &[HistoryMapField {
        key: "profile",
        required: false,
        node: CONTENT_EXCEPT_RUNTIME,
    }],
    entries: None,
});

const COMPLEMENT_ROWS_OMIT: HistoryRestoreNode =
    HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
        fields: &[],
        entries: Some(&HistoryMapEntries {
            key_limit_bytes: 45,
            membership: HistoryMembership::TargetAuthoritative,
            on_create: PreservedOnCreate::Omit,
            node: CONTENT_EXCEPT_RUNTIME,
        }),
    });
const COMPLEMENT_ROWS_REJECT: HistoryRestoreNode =
    HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
        fields: &[],
        entries: Some(&HistoryMapEntries {
            key_limit_bytes: 45,
            membership: HistoryMembership::TargetAuthoritative,
            on_create: PreservedOnCreate::Reject,
            node: CONTENT_EXCEPT_RUNTIME,
        }),
    });
static FIXED_COMPLEMENT_SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "fixed-complement",
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("outer"),
        required: false,
        node: FIXED_COMPLEMENT,
    }],
    ..CONTENT_EXCEPT
};

static COMPLEMENT_ROWS: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "complement-rows",
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("connections"),
        required: false,
        node: COMPLEMENT_ROWS_OMIT,
    }],
    ..CONTENT_EXCEPT
};

static COMPLEMENT_ROWS_REJECT_SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "complement-rows-reject",
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("connections"),
        required: false,
        node: COMPLEMENT_ROWS_REJECT,
    }],
    ..CONTENT_EXCEPT
};

static COMPLEMENT_REQUIRE_PRESENCE: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "complement-require-presence",
    validate_result: require_operational_presence,
    ..COMPLEMENT_ROWS
};

static COMPLEMENT_REFUSE_LIVE: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "complement-refuse-live",
    validate_result: reject_live,
    ..CONTENT_EXCEPT
};

static COMPLEMENT_REFUSE_TRIAL: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "complement-refuse-trial",
    validate_result: reject_pending,
    ..CONTENT_EXCEPT
};

static COMPLEMENT_FAMILY: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "complement-family",
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Affix {
            prefix: "record:",
            suffix: "",
            max_capture_bytes: 45,
        },
        required: false,
        node: CONTENT_EXCEPT_RUNTIME,
    }],
    ..CONTENT_EXCEPT
};

fn complement_row(doc: &mut AutoCommit, parent: &ObjId, key: &str, name: &str) -> ObjId {
    let row = doc.put_object(parent, key, ObjType::Map).unwrap();
    doc.put(&row, "name", name).unwrap();
    let presence = doc.put_object(&row, "presence", ObjType::Map).unwrap();
    doc.put(&presence, "status", "historical").unwrap();
    doc.put(&row, "last_used", 1_i64).unwrap();
    row
}

#[test]
fn complement_structural_dispatch_works_at_fixed_entry_and_family_positions() {
    let mut fixed = AutoCommit::new();
    let outer = fixed.put_object(ROOT, "outer", ObjType::Map).unwrap();
    let profile = fixed.put_object(&outer, "profile", ObjType::Map).unwrap();
    let outer_view = fixed.put_object(&outer, "view", ObjType::Map).unwrap();
    let presence = fixed
        .put_object(&profile, "presence", ObjType::Map)
        .unwrap();
    fixed.put(&profile, "name", "target").unwrap();
    fixed.put(&presence, "status", "historic").unwrap();
    let target = fixed.get_heads();
    fixed.put(&profile, "name", "live").unwrap();
    fixed.put(&presence, "status", "current").unwrap();
    restore(&mut fixed, &FIXED_COMPLEMENT_SCHEMA, &target);
    assert_eq!(object(&fixed, &ROOT, "outer"), outer);
    assert_eq!(object(&fixed, &outer, "profile"), profile);
    assert_eq!(object(&fixed, &outer, "view"), outer_view);
    assert_eq!(object(&fixed, &profile, "presence"), presence);
    assert_eq!(
        string_at(&fixed, &profile, "name").as_deref(),
        Some("target")
    );
    assert_eq!(
        string_at(&fixed, &presence, "status").as_deref(),
        Some("current")
    );

    let mut entries = AutoCommit::new();
    let connections = entries
        .put_object(ROOT, "connections", ObjType::Map)
        .unwrap();
    let row = complement_row(&mut entries, &connections, "a", "target");
    let presence = object(&entries, &row, "presence");
    let target = entries.get_heads();
    entries.put(&row, "name", "live").unwrap();
    entries.put(&presence, "status", "current").unwrap();
    restore(&mut entries, &COMPLEMENT_ROWS, &target);
    assert_eq!(object(&entries, &ROOT, "connections"), connections);
    assert_eq!(object(&entries, &connections, "a"), row);
    assert_eq!(object(&entries, &row, "presence"), presence);

    let mut family = AutoCommit::new();
    let record = family
        .put_object(ROOT, "record:item-a", ObjType::Map)
        .unwrap();
    let presence = family
        .put_object(&record, "presence", ObjType::Map)
        .unwrap();
    family.put(&record, "name", "target").unwrap();
    family.put(&presence, "status", "historic").unwrap();
    let target = family.get_heads();
    family.put(&record, "name", "live").unwrap();
    family.put(&presence, "status", "current").unwrap();
    restore(&mut family, &COMPLEMENT_FAMILY, &target);
    assert_eq!(object(&family, &ROOT, "record:item-a"), record);
    assert_eq!(object(&family, &record, "presence"), presence);
    assert_eq!(
        string_at(&family, &record, "name").as_deref(),
        Some("target")
    );
}

#[test]
fn complement_membership_resurrection_omits_excluded_fields_and_never_revives_ids() {
    let mut doc = AutoCommit::new();
    let connections = doc.put_object(ROOT, "connections", ObjType::Map).unwrap();
    let retained = complement_row(&mut doc, &connections, "a", "target-a");
    let retained_presence = object(&doc, &retained, "presence");
    let historic_b = complement_row(&mut doc, &connections, "b", "target-b");
    let historic_empty = doc.put_object(&connections, "empty", ObjType::Map).unwrap();
    doc.put(&historic_empty, "last_used", 4_i64).unwrap();
    let target = doc.get_heads();
    doc.put(&retained, "name", "live-a").unwrap();
    doc.put(&retained_presence, "status", "current").unwrap();
    doc.delete(&connections, "b").unwrap();
    doc.delete(&connections, "empty").unwrap();
    complement_row(&mut doc, &connections, "c", "current-c");
    restore(&mut doc, &COMPLEMENT_ROWS, &target);
    assert_eq!(object(&doc, &connections, "a"), retained);
    assert_eq!(object(&doc, &retained, "presence"), retained_presence);
    assert!(doc.get(&connections, "c").unwrap().is_none());
    let restored_b = object(&doc, &connections, "b");
    let restored_empty = object(&doc, &connections, "empty");
    assert_ne!(restored_b, historic_b);
    assert_ne!(restored_empty, historic_empty);
    for row in [&restored_b, &restored_empty] {
        assert!(doc.get(row, "presence").unwrap().is_none());
        assert!(doc.get(row, "last_used").unwrap().is_none());
    }
    assert_eq!(
        string_at(&doc, &restored_b, "name").as_deref(),
        Some("target-b")
    );
    assert_eq!(doc.length(&restored_empty), 0);

    doc.commit();
    doc.delete(&connections, "b").unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &COMPLEMENT_ROWS, &live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    let restored_again = object(&doc, &connections, "b");
    assert_ne!(restored_again, historic_b);
    assert_ne!(restored_again, restored_b);
}

#[test]
fn complement_reject_policy_allows_retained_edits_but_refuses_empty_resurrection() {
    let mut doc = AutoCommit::new();
    let connections = doc.put_object(ROOT, "connections", ObjType::Map).unwrap();
    let retained = complement_row(&mut doc, &connections, "a", "target");
    let target = doc.get_heads();
    doc.put(&retained, "name", "live").unwrap();
    let live = doc.get_heads();
    assert!(prepare_content_restore(&doc, &COMPLEMENT_ROWS_REJECT_SCHEMA, &live, &target).is_ok());

    let empty = doc.put_object(&connections, "empty", ObjType::Map).unwrap();
    doc.put(&empty, "last_used", 1_i64).unwrap();
    let target_with_empty = doc.get_heads();
    doc.delete(&connections, "empty").unwrap();
    let live = doc.get_heads();
    let mut before = doc.clone();
    assert!(matches!(
        prepare_content_restore(
            &doc,
            &COMPLEMENT_ROWS_REJECT_SCHEMA,
            &live,
            &target_with_empty
        ),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_eq!(doc.clone().save(), before.save());
}

#[test]
fn complement_omit_resurrection_still_obeys_the_combined_result_validator() {
    let mut doc = AutoCommit::new();
    let connections = doc.put_object(ROOT, "connections", ObjType::Map).unwrap();
    complement_row(&mut doc, &connections, "missing", "target");
    let target = doc.get_heads();
    doc.delete(&connections, "missing").unwrap();
    let live = doc.get_heads();
    let before = doc.clone().save();
    assert!(matches!(
        prepare_content_restore(&doc, &COMPLEMENT_REQUIRE_PRESENCE, &live, &target),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_eq!(doc.clone().save(), before);
    assert_eq!(doc.pending_ops(), 0);
}

#[test]
fn complement_live_validator_failure_rolls_back_multiple_writes_and_identities() {
    let mut doc = AutoCommit::new();
    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let presence = doc.put_object(&profile, "presence", ObjType::Map).unwrap();
    doc.put(&presence, "status", "current").unwrap();
    doc.put(&profile, "a", "target-a").unwrap();
    doc.put(&profile, "b", "target-b").unwrap();
    let target = doc.get_heads();
    doc.put(&profile, "a", "live-a").unwrap();
    doc.delete(&profile, "b").unwrap();
    doc.put(&profile, "c", "live-c").unwrap();
    let live = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(b"selector-live".as_slice()));
    let prepared = prepare_content_restore(&doc, &COMPLEMENT_REFUSE_LIVE, &live, &target).unwrap();
    let before = doc.clone().save();
    assert!(matches!(
        apply_prepared_content_restore(&mut doc, &prepared),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_eq!(doc.clone().save(), before);
    assert_eq!(doc.pending_ops(), 0);
    assert_eq!(object(&doc, &ROOT, "profile"), profile);
    assert_eq!(object(&doc, &profile, "presence"), presence);
    assert_eq!(string_at(&doc, &profile, "a").as_deref(), Some("live-a"));
    assert!(doc.get(&profile, "b").unwrap().is_none());
    assert_eq!(string_at(&doc, &profile, "c").as_deref(), Some("live-c"));
}

#[test]
fn complement_preserves_conflicted_excluded_alternatives_absence_and_rich_text() {
    let mut doc = AutoCommit::new();
    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    doc.put(&profile, "name", "target").unwrap();
    let runtime_text = doc
        .put_object(&profile, "runtime_text", ObjType::Text)
        .unwrap();
    doc.splice_text(&runtime_text, 0, 0, "runtime").unwrap();
    doc.mark(
        &runtime_text,
        Mark::new("bold".into(), true, 0, 7),
        ExpandMark::Both,
    )
    .unwrap();
    let block = doc.split_block(&runtime_text, 0).unwrap();
    doc.put(&block, "kind", "paragraph").unwrap();
    doc.commit();
    let mut historic_peer = doc.fork();
    doc.put(&profile, "historic_missing", "historical-left")
        .unwrap();
    historic_peer
        .put(&profile, "historic_missing", "historical-right")
        .unwrap();
    doc.merge(&mut historic_peer).unwrap();
    let target = doc.get_heads();
    doc.delete(&profile, "historic_missing").unwrap();
    doc.commit();
    let mut peer = doc.fork();
    doc.put(&profile, "last_used", 1_i64).unwrap();
    peer.put(&profile, "last_used", 2_i64).unwrap();
    let left_presence = doc.put_object(&profile, "presence", ObjType::Map).unwrap();
    doc.put(&left_presence, "status", "left").unwrap();
    let right_presence = peer.put_object(&profile, "presence", ObjType::Map).unwrap();
    peer.put(&right_presence, "status", "right").unwrap();
    doc.merge(&mut peer).unwrap();
    doc.put(&profile, "name", "live").unwrap();
    let live = doc.get_heads();
    let last_used: Vec<_> = doc
        .get_all(&profile, "last_used")
        .unwrap()
        .into_iter()
        .map(|(value, id)| (value.into_owned(), id))
        .collect();
    let presence: Vec<_> = doc
        .get_all(&profile, "presence")
        .unwrap()
        .into_iter()
        .map(|(value, id)| (value.into_owned(), id))
        .collect();
    let left_status: Vec<_> = doc
        .get_all(&left_presence, "status")
        .unwrap()
        .into_iter()
        .map(|(value, id)| (value.into_owned(), id))
        .collect();
    let right_status: Vec<_> = doc
        .get_all(&right_presence, "status")
        .unwrap()
        .into_iter()
        .map(|(value, id)| (value.into_owned(), id))
        .collect();
    let text_marks = doc.marks(&runtime_text).unwrap();
    let prepared = prepare_content_restore(&doc, &CONTENT_EXCEPT, &live, &target).unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert_eq!(doc.get_all(&profile, "last_used").unwrap(), last_used);
    assert_eq!(doc.get_all(&profile, "presence").unwrap(), presence);
    assert_eq!(doc.get_all(&left_presence, "status").unwrap(), left_status);
    assert_eq!(
        doc.get_all(&right_presence, "status").unwrap(),
        right_status
    );
    assert!(doc.get(&profile, "historic_missing").unwrap().is_none());
    assert_eq!(object(&doc, &profile, "runtime_text"), runtime_text);
    assert_eq!(doc.get(&runtime_text, 0).unwrap().unwrap().1, block);
    assert_eq!(doc.marks(&runtime_text).unwrap(), text_marks);
    assert_eq!(string_at(&doc, &profile, "name").as_deref(), Some("target"));
}

#[test]
fn complement_heterogeneous_authored_content_is_exact_and_unbounded_by_registration() {
    let mut doc = AutoCommit::new();
    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let nested_map = doc.put_object(&profile, "map", ObjType::Map).unwrap();
    doc.put(&nested_map, "value", "map").unwrap();
    let list = doc.put_object(&profile, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "first").unwrap();
    doc.insert(&list, 1, "second").unwrap();
    let text = doc.put_object(&profile, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "text").unwrap();
    let values = [
        ScalarValue::Bytes(vec![0, 255]),
        ScalarValue::Str("string".into()),
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
    for (index, value) in values.iter().enumerate() {
        doc.put(&profile, format!("scalar-{index}"), value.clone())
            .unwrap();
    }
    let long_key = "k".repeat(1025);
    doc.put(&profile, long_key.as_str(), "long-key").unwrap();
    doc.put(&profile, "odd/\nname", "literal").unwrap();
    doc.put(&profile, "target-only", "target").unwrap();
    let target = doc.get_heads();
    doc.put(&nested_map, "value", "live").unwrap();
    doc.put(&list, 0, "live").unwrap();
    doc.splice_text(&text, 0, 4, "live").unwrap();
    doc.delete(&profile, "target-only").unwrap();
    doc.put(&profile, "current-only", "current").unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &CONTENT_EXCEPT, &live, &target).unwrap();
    let target_projection = prepared.target_projection().clone();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert_eq!(
        selection::content(&doc, &CONTENT_EXCEPT).unwrap(),
        target_projection
    );
    assert_eq!(object(&doc, &profile, "map"), nested_map);
    assert_eq!(
        doc.get(&profile, long_key.as_str())
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("long-key")
    );
    assert!(doc.get(&profile, "current-only").unwrap().is_none());
}

fn complement_schema(fields: &'static [&'static str]) -> &'static HistoryRestoreSchema {
    schema(vec![HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("profile"),
        required: false,
        node: HistoryRestoreNode::MapContentExcept {
            preserved_fields: fields,
        },
    }])
}

#[test]
fn complement_registration_enforces_only_declared_count_name_and_byte_bounds() {
    let mut doc = AutoCommit::new();
    let heads = doc.get_heads();
    let long_1024: &'static str = Box::leak("é".repeat(512).into_boxed_str());
    let long_1025: &'static str = Box::leak(format!("{}a", "é".repeat(512)).into_boxed_str());
    let one_1024: &'static [&'static str] = Box::leak(vec![long_1024].into_boxed_slice());
    let one_1025: &'static [&'static str] = Box::leak(vec![long_1025].into_boxed_slice());
    let names: Vec<&'static str> = (0..64)
        .map(|index| Box::leak(format!("field-{index}").into_boxed_str()) as &'static str)
        .collect();
    let sixty_four = Box::leak(names.into_boxed_slice());
    for accepted in [
        &["x"][..],
        &["A", "a", "literal/name"][..],
        one_1024,
        &sixty_four[..],
    ] {
        assert!(prepare_content_restore(&doc, complement_schema(accepted), &heads, &heads).is_ok());
    }
    let sixty_five: Vec<&'static str> = (0..65)
        .map(|index| Box::leak(format!("field65-{index}").into_boxed_str()) as &'static str)
        .collect();
    let sixty_five = Box::leak(sixty_five.into_boxed_slice());
    for rejected in [
        &[][..],
        &["same", "same"][..],
        &[""][..],
        one_1025,
        &sixty_five[..],
    ] {
        assert!(matches!(
            prepare_content_restore(&doc, complement_schema(rejected), &heads, &heads),
            Err(RestoreError::InvalidSchema(_))
        ));
    }

    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let authored = "z".repeat(1025);
    doc.put(&profile, authored, "allowed").unwrap();
    doc.put(&profile, "literal/name", "protected").unwrap();
    let heads = doc.get_heads();
    assert!(
        prepare_content_restore(&doc, complement_schema(&["literal/name"]), &heads, &heads).is_ok()
    );
}

fn nested_map_chain(doc: &mut AutoCommit, root: &ObjId, count: usize) {
    let deepest = nested_map_chain_ids(doc, root, count)
        .pop()
        .unwrap_or_else(|| root.clone());
    doc.put(&deepest, "leaf", true).unwrap();
}

fn nested_map_chain_ids(doc: &mut AutoCommit, root: &ObjId, count: usize) -> Vec<ObjId> {
    let mut parent = root.clone();
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        parent = doc
            .put_object(&parent, format!("d{index}"), ObjType::Map)
            .unwrap();
        ids.push(parent.clone());
    }
    ids
}

#[test]
fn complement_codec_depth_and_exact_map_shape_are_enforced() {
    on_deep_stack(
        "complement-depth-boundary",
        complement_codec_depth_and_exact_map_shape_are_enforced_inner,
    );
}

fn complement_codec_depth_and_exact_map_shape_are_enforced_inner() {
    let mut boundary = AutoCommit::new();
    let profile = boundary.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let ancestors = nested_map_chain_ids(&mut boundary, &profile, 255);
    let deepest = ancestors.last().unwrap().clone();
    boundary.put(&deepest, "leaf", "target").unwrap();
    boundary.put(&profile, "sibling", "target-sibling").unwrap();
    let replacement_target = boundary
        .put_object(&profile, "replacement", ObjType::Map)
        .unwrap();
    boundary
        .put(&replacement_target, "value", "target-map")
        .unwrap();
    let presence = boundary
        .put_object(&profile, "presence", ObjType::Map)
        .unwrap();
    boundary
        .put(&presence, "status", "target-protected")
        .unwrap();
    let target = boundary.get_heads();
    boundary.put(&deepest, "leaf", "current").unwrap();
    boundary
        .put(&profile, "sibling", "current-sibling")
        .unwrap();
    boundary
        .put(&presence, "status", "current-protected")
        .unwrap();
    boundary.delete(&profile, "replacement").unwrap();
    let replacement_live = boundary
        .put_object(&profile, "replacement", ObjType::List)
        .unwrap();
    boundary
        .insert(&replacement_live, 0, "current-list")
        .unwrap();
    let live = boundary.get_heads();
    let before_trial = boundary.clone().save();
    assert!(matches!(
        prepare_content_restore(&boundary, &COMPLEMENT_REFUSE_TRIAL, &live, &target),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_eq!(boundary.clone().save(), before_trial);

    let mut live_failure = boundary.clone();
    live_failure.set_actor(automerge::ActorId::from(b"selector-live".as_slice()));
    let failure_actor = live_failure.get_actor().clone();
    let failure_plan =
        prepare_content_restore(&live_failure, &COMPLEMENT_REFUSE_LIVE, &live, &target).unwrap();
    let before_failure = live_failure.clone().save();
    assert!(matches!(
        apply_prepared_content_restore(&mut live_failure, &failure_plan),
        Err(RestoreError::SchemaInvalid(_))
    ));
    assert_eq!(live_failure.pending_ops(), 0);
    assert_eq!(live_failure.get_actor(), &failure_actor);
    assert_eq!(live_failure.clone().save(), before_failure);
    assert_eq!(object(&live_failure, &profile, "presence"), presence);
    drop(failure_plan);
    drop(live_failure);

    let prepared = prepare_content_restore(&boundary, &CONTENT_EXCEPT, &live, &target).unwrap();
    let prepared_copy = prepared.clone();
    assert_eq!(
        prepared.target_projection(),
        prepared_copy.target_projection()
    );
    assert!(format!("{prepared_copy:?}").contains("PreparedContentRestore"));
    drop(prepared_copy);
    assert_eq!(
        apply_prepared_content_restore(&mut boundary, &prepared).unwrap(),
        RestoreOutcome::Changed
    );
    let mut retained = profile.clone();
    for (index, expected) in ancestors.iter().enumerate() {
        retained = object(&boundary, &retained, &format!("d{index}"));
        assert_eq!(&retained, expected);
    }
    assert_eq!(
        string_at(&boundary, &retained, "leaf").as_deref(),
        Some("target")
    );
    assert_eq!(
        string_at(&boundary, &profile, "sibling").as_deref(),
        Some("target-sibling")
    );
    assert_eq!(object(&boundary, &profile, "presence"), presence);
    assert_eq!(
        string_at(&boundary, &presence, "status").as_deref(),
        Some("current-protected")
    );
    let replacement = object(&boundary, &profile, "replacement");
    assert_eq!(boundary.object_type(&replacement).unwrap(), ObjType::Map);
    assert_ne!(replacement, replacement_target);
    assert_ne!(replacement, replacement_live);
    assert_eq!(
        string_at(&boundary, &replacement, "value").as_deref(),
        Some("target-map")
    );
    boundary.commit().unwrap();
    let saved = boundary.save();
    let loaded = AutoCommit::load(&saved).unwrap();
    let loaded_profile = object(&loaded, &ROOT, "profile");
    assert_eq!(loaded_profile, profile);
    assert_eq!(object(&loaded, &loaded_profile, "presence"), presence);
    drop(prepared);
    drop(loaded);
    drop(boundary);

    let mut too_deep = AutoCommit::new();
    let profile = too_deep.put_object(ROOT, "profile", ObjType::Map).unwrap();
    nested_map_chain(&mut too_deep, &profile, 256);
    let heads = too_deep.get_heads();
    assert!(matches!(
        prepare_content_restore(&too_deep, &CONTENT_EXCEPT, &heads, &heads),
        Err(RestoreError::UnsupportedContent(_))
    ));

    let mut deep_target = AutoCommit::new();
    let profile = deep_target
        .put_object(ROOT, "profile", ObjType::Map)
        .unwrap();
    nested_map_chain(&mut deep_target, &profile, 256);
    let target = deep_target.get_heads();
    deep_target.delete(ROOT, "profile").unwrap();
    deep_target
        .put_object(ROOT, "profile", ObjType::Map)
        .unwrap();
    let live = deep_target.get_heads();
    assert!(matches!(
        prepare_content_restore(&deep_target, &CONTENT_EXCEPT, &live, &target),
        Err(RestoreError::UnsupportedContent(_))
    ));

    let mut deep_current = AutoCommit::new();
    let profile = deep_current
        .put_object(ROOT, "profile", ObjType::Map)
        .unwrap();
    deep_current.put(&profile, "leaf", true).unwrap();
    let target = deep_current.get_heads();
    deep_current.delete(ROOT, "profile").unwrap();
    let profile = deep_current
        .put_object(ROOT, "profile", ObjType::Map)
        .unwrap();
    nested_map_chain(&mut deep_current, &profile, 256);
    let live = deep_current.get_heads();
    let before = deep_current.clone().save();
    assert!(matches!(
        prepare_content_restore(&deep_current, &CONTENT_EXCEPT, &live, &target),
        Err(RestoreError::UnsupportedContent(_))
    ));
    assert_eq!(deep_current.clone().save(), before);

    let protected_schema = complement_schema(&["presence"]);
    let mut protected = AutoCommit::new();
    let profile = protected.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let presence = protected
        .put_object(&profile, "presence", ObjType::Map)
        .unwrap();
    nested_map_chain(&mut protected, &presence, 256);
    let heads = protected.get_heads();
    assert!(matches!(
        prepare_content_restore(&protected, protected_schema, &heads, &heads),
        Err(RestoreError::UnsupportedContent(_))
    ));

    let mut far_over = AutoCommit::new();
    let profile = far_over.put_object(ROOT, "profile", ObjType::Map).unwrap();
    far_over.put(&profile, "accepted-first", true).unwrap();
    nested_map_chain(&mut far_over, &profile, 1024);
    let heads = far_over.get_heads();
    let before = far_over.clone().save();
    assert!(matches!(
        prepare_content_restore(&far_over, &CONTENT_EXCEPT, &heads, &heads),
        Err(RestoreError::UnsupportedContent(_))
    ));
    assert_eq!(far_over.clone().save(), before);

    for wrong in [ObjType::List, ObjType::Text] {
        let mut doc = AutoCommit::new();
        doc.put_object(ROOT, "profile", wrong).unwrap();
        let heads = doc.get_heads();
        assert!(matches!(
            prepare_content_restore(&doc, &CONTENT_EXCEPT, &heads, &heads),
            Err(RestoreError::SchemaInvalid(_))
        ));
    }
    let mut scalar = AutoCommit::new();
    scalar.put(ROOT, "profile", "wrong").unwrap();
    let heads = scalar.get_heads();
    assert!(matches!(
        prepare_content_restore(&scalar, &CONTENT_EXCEPT, &heads, &heads),
        Err(RestoreError::SchemaInvalid(_))
    ));
    let missing = AutoCommit::new();
    let mut copy = missing.clone();
    let heads = copy.get_heads();
    assert!(matches!(
        prepare_content_restore(&missing, &CONTENT_EXCEPT, &heads, &heads),
        Err(RestoreError::SchemaInvalid(_))
    ));
}

fn deepest_entry_schema() -> &'static HistoryRestoreSchema {
    let entries = Box::leak(Box::new(HistoryMapEntries {
        key_limit_bytes: 16,
        node: STRING,
        membership: HistoryMembership::TargetAuthoritative,
        on_create: PreservedOnCreate::Omit,
    }));
    let mut node = HistoryRestoreNode::MapOverlay(Box::leak(Box::new(HistoryMapOverlay {
        fields: &[],
        entries: Some(entries),
    })));
    for index in (0..255).rev() {
        let key: &'static str = Box::leak(format!("d{index}").into_boxed_str());
        let fields: &'static [HistoryMapField] = Box::leak(
            vec![HistoryMapField {
                key,
                required: true,
                node,
            }]
            .into_boxed_slice(),
        );
        node = HistoryRestoreNode::MapOverlay(Box::leak(Box::new(HistoryMapOverlay {
            fields,
            entries: None,
        })));
    }
    schema(vec![HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("deep"),
        required: true,
        node,
    }])
}

#[test]
fn accepted_selector_and_protected_codec_boundaries() {
    on_deep_stack(
        "accepted-selector-protected-boundaries",
        accepted_selector_and_protected_codec_boundaries_inner,
    );
}

fn accepted_selector_and_protected_codec_boundaries_inner() {
    let deep_schema = deepest_entry_schema();
    let mut selected = AutoCommit::new();
    let deep = selected.put_object(ROOT, "deep", ObjType::Map).unwrap();
    let parent = nested_map_chain_ids(&mut selected, &deep, 255)
        .pop()
        .unwrap();
    selected.put(&parent, "entry", "target").unwrap();
    let target = selected.get_heads();
    selected.put(&parent, "entry", "current").unwrap();
    let live = selected.get_heads();
    let prepared = prepare_content_restore(&selected, deep_schema, &live, &target).unwrap();
    assert_eq!(
        apply_prepared_content_restore(&mut selected, &prepared).unwrap(),
        RestoreOutcome::Changed
    );
    assert_eq!(
        string_at(&selected, &parent, "entry").as_deref(),
        Some("target")
    );
    drop(prepared);
    drop(selected);

    let protected_schema = complement_schema(&["presence"]);
    let mut doc = AutoCommit::new();
    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    doc.put(&profile, "name", "target").unwrap();
    let presence = doc.put_object(&profile, "presence", ObjType::Map).unwrap();

    let empty_boundary = doc
        .put_object(&presence, "empty-boundary", ObjType::Map)
        .unwrap();
    let empty_ids = nested_map_chain_ids(&mut doc, &empty_boundary, 254);

    let alternatives = doc
        .put_object(&presence, "alternatives", ObjType::Map)
        .unwrap();
    let alternative_ids = nested_map_chain_ids(&mut doc, &alternatives, 253);
    let alternative_parent = alternative_ids.last().unwrap().clone();
    doc.commit().unwrap();
    let mut peer = doc.fork();
    doc.put(&alternative_parent, "leaf", "left").unwrap();
    peer.put(&alternative_parent, "leaf", "right").unwrap();
    doc.merge(&mut peer).unwrap();
    assert_eq!(doc.get_all(&alternative_parent, "leaf").unwrap().len(), 2);

    let list_branch = doc.put_object(&presence, "list", ObjType::Map).unwrap();
    let list_ids = nested_map_chain_ids(&mut doc, &list_branch, 252);
    let list = doc
        .put_object(list_ids.last().unwrap(), "value", ObjType::List)
        .unwrap();
    doc.insert(&list, 0, "protected-list").unwrap();

    let text_branch = doc.put_object(&presence, "text", ObjType::Map).unwrap();
    let text_ids = nested_map_chain_ids(&mut doc, &text_branch, 252);
    let text = doc
        .put_object(text_ids.last().unwrap(), "value", ObjType::Text)
        .unwrap();
    doc.splice_text(&text, 0, 0, "protected-text").unwrap();
    doc.mark(
        &text,
        Mark::new("protected-depth".into(), 7_i64, 1, 10),
        ExpandMark::Both,
    )
    .unwrap();

    let target = doc.get_heads();
    doc.put(&profile, "name", "current").unwrap();
    let live = doc.get_heads();
    let protected_values = doc
        .get_all(&alternative_parent, "leaf")
        .unwrap()
        .into_iter()
        .map(|(value, id)| (value.into_owned(), id))
        .collect::<Vec<_>>();
    let list_elements = doc
        .get_all(&list, 0)
        .unwrap()
        .into_iter()
        .map(|(value, id)| (value.into_owned(), id))
        .collect::<Vec<_>>();
    let text_elements = (0..doc.length(&text))
        .map(|index| {
            doc.get_all(&text, index)
                .unwrap()
                .into_iter()
                .map(|(value, id)| (value.into_owned(), id))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let text_marks = doc.marks(&text).unwrap();
    assert_eq!(
        text_marks,
        vec![Mark::new("protected-depth".into(), 7_i64, 1, 10)]
    );
    let prepared = prepare_content_restore(&doc, protected_schema, &live, &target).unwrap();
    let unapplied_copy = prepared.clone();
    assert_eq!(
        prepared.target_projection(),
        unapplied_copy.target_projection()
    );
    assert!(format!("{unapplied_copy:?}").contains("PreparedContentRestore"));
    drop(unapplied_copy);
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap(),
        RestoreOutcome::Changed
    );
    let retained_profile = object(&doc, &ROOT, "profile");
    assert_eq!(retained_profile, profile);
    let retained_presence = object(&doc, &retained_profile, "presence");
    assert_eq!(retained_presence, presence);

    let mut retained = object(&doc, &retained_presence, "empty-boundary");
    assert_eq!(retained, empty_boundary);
    for (index, expected) in empty_ids.iter().enumerate() {
        retained = object(&doc, &retained, &format!("d{index}"));
        assert_eq!(&retained, expected);
    }

    retained = object(&doc, &retained_presence, "alternatives");
    assert_eq!(retained, alternatives);
    for (index, expected) in alternative_ids.iter().enumerate() {
        retained = object(&doc, &retained, &format!("d{index}"));
        assert_eq!(&retained, expected);
    }
    assert_eq!(retained, alternative_parent);
    assert_eq!(doc.get_all(&retained, "leaf").unwrap(), protected_values);

    retained = object(&doc, &retained_presence, "list");
    assert_eq!(retained, list_branch);
    for (index, expected) in list_ids.iter().enumerate() {
        retained = object(&doc, &retained, &format!("d{index}"));
        assert_eq!(&retained, expected);
    }
    let retained_list = object(&doc, &retained, "value");
    assert_eq!(retained_list, list);
    assert_eq!(doc.get_all(&retained_list, 0).unwrap(), list_elements);
    assert_eq!(
        doc.get(&retained_list, 0).unwrap().unwrap().0.as_str(),
        Some("protected-list")
    );

    retained = object(&doc, &retained_presence, "text");
    assert_eq!(retained, text_branch);
    for (index, expected) in text_ids.iter().enumerate() {
        retained = object(&doc, &retained, &format!("d{index}"));
        assert_eq!(&retained, expected);
    }
    let retained_text = object(&doc, &retained, "value");
    assert_eq!(retained_text, text);
    assert_eq!(
        (0..doc.length(&retained_text))
            .map(|index| doc.get_all(&retained_text, index).unwrap())
            .collect::<Vec<_>>(),
        text_elements
    );
    assert_eq!(doc.text(&retained_text).unwrap(), "protected-text");
    assert_eq!(doc.marks(&retained_text).unwrap(), text_marks);
    drop(prepared);
    drop(doc);
}

#[test]
fn deep_owned_state_restore_stamps_once_undoes_and_refuses_without_action() {
    on_deep_stack(
        "deep-owned-state-restore",
        deep_owned_state_restore_stamps_once_undoes_and_refuses_without_action_inner,
    );
}

fn deep_owned_state_restore_stamps_once_undoes_and_refuses_without_action_inner() {
    use crate::permissions::{PermissionContext, UserId};
    use crate::state::{State, owned_restore::OwnedRestoreError};

    let authority = PermissionContext::permissive();
    let mut state = State::new();
    let profile = state.doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let ancestors = nested_map_chain_ids(&mut state.doc, &profile, 255);
    let deepest = ancestors.last().unwrap().clone();
    state.doc.put(&deepest, "leaf", "target").unwrap();
    let presence = state
        .doc
        .put_object(&profile, "presence", ObjType::Map)
        .unwrap();
    state.doc.put(&presence, "status", "protected").unwrap();
    state.commit_action(None, None);
    let target = state.doc.base_heads();
    state.doc.put(&deepest, "leaf", "current").unwrap();
    state.commit_action(None, None);
    let current = state.doc.base_heads();
    let plan = prepare_content_restore(&state.doc, &CONTENT_EXCEPT, &current, &target).unwrap();
    let plan_copy = plan.clone();
    assert_eq!(plan.target_projection(), plan_copy.target_projection());
    assert!(format!("{plan_copy:?}").contains("PreparedContentRestore"));
    drop(plan_copy);

    let change_count = state.doc.get_changes(&[]).len();
    let revision = state.history_revision();
    let actor = state.doc.get_actor().clone();
    let receipt = state
        .restore_prepared_owned(
            &crate::state::owned_restore_tests::admission(&authority),
            &plan,
        )
        .unwrap();
    assert!(receipt.changed);
    assert_eq!(receipt.after, state.doc.base_heads());
    assert_eq!(state.history_revision(), revision + 1);
    assert_eq!(state.doc.get_changes(&[]).len(), change_count + 1);
    let entry = state.peek_last_undo().unwrap();
    assert_eq!(entry.before, current);
    assert_eq!(entry.after, receipt.after);
    assert_eq!(entry.author, Some(UserId("ada".into())));
    assert_eq!(entry.instance_id, None);
    let change = &state.doc.get_changes(&current)[0];
    assert_eq!(change.deps(), current);
    assert!(change.timestamp() > 0);
    assert_eq!(
        change
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message)),
        Some(crate::state::owned_restore_tests::context())
    );
    assert_eq!(state.doc.get_actor(), &actor);
    let mut retained = profile.clone();
    for (index, expected) in ancestors.iter().enumerate() {
        retained = object(&state.doc, &retained, &format!("d{index}"));
        assert_eq!(&retained, expected);
    }
    assert_eq!(object(&state.doc, &profile, "presence"), presence);
    assert_eq!(
        string_at(&state.doc, &deepest, "leaf").as_deref(),
        Some("target")
    );
    state
        .undo_as(Some(&UserId("ada".into())), None, None)
        .unwrap();
    assert_eq!(
        string_at(&state.doc, &deepest, "leaf").as_deref(),
        Some("current")
    );
    drop(plan);

    let heads = state.doc.base_heads();
    let unchanged = prepare_content_restore(&state.doc, &CONTENT_EXCEPT, &heads, &heads).unwrap();
    let count = state.doc.get_changes(&[]).len();
    let revision = state.history_revision();
    let before_undo = state
        .peek_last_undo()
        .map(|entry| (entry.before.clone(), entry.after.clone()));
    let receipt = state
        .restore_prepared_owned(
            &crate::state::owned_restore_tests::admission(&authority),
            &unchanged,
        )
        .unwrap();
    assert!(!receipt.changed);
    assert_eq!(state.doc.get_changes(&[]).len(), count);
    assert_eq!(state.history_revision(), revision);
    assert_eq!(
        state
            .peek_last_undo()
            .map(|entry| (entry.before.clone(), entry.after.clone())),
        before_undo
    );
    drop(unchanged);

    let heads = state.doc.base_heads();
    let stale = prepare_content_restore(&state.doc, &CONTENT_EXCEPT, &heads, &heads).unwrap();
    state.doc.put(&profile, "after-review", true).unwrap();
    state.commit_action(None, None);
    let refused_heads = state.doc.base_heads();
    let refused_count = state.doc.get_changes(&[]).len();
    let refused_revision = state.history_revision();
    let refused_actor = state.doc.get_actor().clone();
    assert!(matches!(
        state.restore_prepared_owned(
            &crate::state::owned_restore_tests::admission(&authority),
            &stale,
        ),
        Err(OwnedRestoreError::StaleReview)
    ));
    assert_eq!(state.doc.base_heads(), refused_heads);
    assert_eq!(state.doc.get_changes(&[]).len(), refused_count);
    assert_eq!(state.history_revision(), refused_revision);
    assert_eq!(state.doc.get_actor(), &refused_actor);
    assert_eq!(state.doc.pending_ops(), 0);
    drop(stale);
    drop(state);
}

#[test]
fn complement_normalized_absence_noop_and_private_excluded_key_refusal_are_exact() {
    let optional = complement_schema(&["presence"]);
    let mut doc = AutoCommit::new();
    let missing = doc.get_heads();
    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    let presence = doc.put_object(&profile, "presence", ObjType::Map).unwrap();
    doc.put(&presence, "status", "current").unwrap();
    let live = doc.get_heads();
    let before = doc.clone().save();
    let prepared = prepare_content_restore(&doc, optional, &live, &missing).unwrap();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap(),
        RestoreOutcome::Unchanged
    );
    assert_eq!(doc.clone().save(), before);
    assert_eq!(doc.pending_ops(), 0);
    assert_eq!(object(&doc, &ROOT, "profile"), profile);
    assert_eq!(object(&doc, &profile, "presence"), presence);

    doc.put(&profile, "name", "target").unwrap();
    let target = doc.get_heads();
    doc.put(&profile, "name", "live").unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, optional, &live, &target).unwrap();
    let mut corrupted = prepared.clone();
    corrupted.protected.0.clear();
    let before = doc.clone().save();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &corrupted).unwrap_err(),
        RestoreError::ProtectedStateChanged
    );
    assert_eq!(doc.clone().save(), before);

    let mut prepared = prepared;
    let injected = prepared
        .target
        .push_test_node(Node::Scalar(ExactScalar(ScalarValue::Boolean(false))));
    let [profile_node] = prepared.target.0.get("profile").unwrap().as_slice() else {
        panic!("expected projected profile")
    };
    let profile_node = *profile_node;
    let Node::Map(_, fields) = &mut prepared.target.1[profile_node] else {
        panic!("expected projected profile map")
    };
    fields.insert("presence".into(), vec![injected]);
    let before = doc.clone().save();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap_err(),
        RestoreError::ContentPostconditionMismatch
    );
    assert_eq!(doc.clone().save(), before);
    assert_eq!(object(&doc, &profile, "presence"), presence);
}

#[test]
fn complement_freshness_checks_excluded_changes_before_writing() {
    let mut doc = AutoCommit::new();
    let profile = doc.put_object(ROOT, "profile", ObjType::Map).unwrap();
    doc.put(&profile, "name", "target").unwrap();
    doc.put(&profile, "last_used", 1_i64).unwrap();
    let target = doc.get_heads();
    doc.put(&profile, "name", "live").unwrap();
    let live = doc.get_heads();
    let prepared = prepare_content_restore(&doc, &CONTENT_EXCEPT, &live, &target).unwrap();
    doc.put(&profile, "last_used", 2_i64).unwrap();
    doc.commit();
    let before = doc.clone().save();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &prepared).unwrap_err(),
        RestoreError::StaleReview
    );
    assert_eq!(doc.clone().save(), before);
    assert_eq!(string_at(&doc, &profile, "name").as_deref(), Some("live"));
}

#[test]
fn whole_identity_reconciliation_applies_under_fields_entries_and_family_roots() {
    let fixed = schema(vec![HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("outer"),
        required: true,
        node: WHOLE_MAP_FIELD,
    }]);
    let mut fixed_doc = AutoCommit::new();
    let outer = fixed_doc.put_object(ROOT, "outer", ObjType::Map).unwrap();
    let payload = fixed_doc
        .put_object(&outer, "payload", ObjType::Map)
        .unwrap();
    fixed_doc.put(&payload, "value", "target").unwrap();
    let target = fixed_doc.get_heads();
    fixed_doc.put(&payload, "value", "live").unwrap();
    restore(&mut fixed_doc, fixed, &target);
    assert_eq!(object(&fixed_doc, &outer, "payload"), payload);

    let entries = schema(vec![HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("rows"),
        required: true,
        node: WHOLE_MAP_ENTRIES,
    }]);
    let mut entry_doc = AutoCommit::new();
    let rows = entry_doc.put_object(ROOT, "rows", ObjType::Map).unwrap();
    let row = entry_doc.put_object(&rows, "a", ObjType::Map).unwrap();
    entry_doc.put(&row, "value", "target").unwrap();
    let target = entry_doc.get_heads();
    entry_doc.put(&row, "value", "live").unwrap();
    restore(&mut entry_doc, entries, &target);
    assert_eq!(object(&entry_doc, &rows, "a"), row);

    let family_schema = schema(vec![family("whole:", "", 45, WHOLE_MAP)]);
    let mut family_doc = AutoCommit::new();
    let family_root = family_doc
        .put_object(ROOT, "whole:item", ObjType::Map)
        .unwrap();
    family_doc.put(&family_root, "value", "target").unwrap();
    let target = family_doc.get_heads();
    family_doc.put(&family_root, "value", "live").unwrap();
    restore(&mut family_doc, family_schema, &target);
    assert_eq!(object(&family_doc, &ROOT, "whole:item"), family_root);
}

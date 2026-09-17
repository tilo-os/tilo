use super::*;
use automerge::{ObjId, ROOT, ReadDoc, transaction::Transactable};

const STRING: HistoryRestoreNode = HistoryRestoreNode::Whole {
    expected_type: RestoreValueType::Scalar(ScalarType::String),
};
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

// Replacing wb would lose both its identity and the current presentation state.
#[test]
fn sheets_shorter_list_preserves_mixed_parent_and_current_active() {
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
        doc.get(&sheets, 0).unwrap().unwrap().0.to_str(),
        Some("sheet-a")
    );
    assert!(doc.pending_ops() > 0);
}

#[test]
fn absent_structural_parent_deletes_only_optional_selected_child() {
    let (mut doc, wb, _) = sheets_fixture();
    assert_eq!(restore(&mut doc, &SHEETS, &[]), RestoreOutcome::Changed);
    assert_eq!(object(&doc, &ROOT, "wb"), wb);
    assert!(doc.get(&wb, "sheets").unwrap().is_none());
    assert_eq!(doc.get(&wb, "active").unwrap().unwrap().0.to_i64(), Some(1));
}

#[test]
fn nested_protected_historical_differences_are_noop() {
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
fn remote_retains_operational_identity_deletes_entities_and_resurrects_content_only() {
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
        doc.get(&retained, "name").unwrap().unwrap().0.to_str(),
        Some("old-name")
    );
    assert_eq!(
        doc.get(&retained, "last_used").unwrap().unwrap().0.to_i64(),
        Some(999)
    );
    assert_eq!(object(&doc, &retained, "presence"), presence);
    assert_eq!(
        doc.get(&presence, "status").unwrap().unwrap().0.to_str(),
        Some("live-online")
    );
    assert!(doc.get(&connections, "delete").unwrap().is_none());
    let resurrect = object(&doc, &connections, "resurrect");
    assert_eq!(
        doc.get(&resurrect, "name").unwrap().unwrap().0.to_str(),
        Some("old-resurrect")
    );
    assert!(doc.get(&resurrect, "presence").unwrap().is_none());
    assert!(doc.get(&resurrect, "last_used").unwrap().is_none());
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
#[test]
fn browser_add_remove_membership_and_tombstone_resurrection() {
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
        doc.get(&add, "url").unwrap().unwrap().0.to_str(),
        Some("https://example.test")
    );
    assert_eq!(
        doc.get(ROOT, "browser.bookmarks.v2.remove:tag-b")
            .unwrap()
            .unwrap()
            .0
            .to_bool(),
        Some(true)
    );
    assert_eq!(
        doc.get(ROOT, "browser.bookmarks.v2.migrated")
            .unwrap()
            .unwrap()
            .0
            .to_bool(),
        Some(true)
    );
}

#[test]
fn nested_selected_conflicts_refuse_but_protected_conflicts_retain_every_op() {
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
#[test]
fn combined_result_validator_preflight_and_live_failure_roll_back_all_writes() {
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

fn schema(selectors: Vec<HistoryRootSelector>) -> &'static HistoryRestoreSchema {
    Box::leak(Box::new(HistoryRestoreSchema {
        selectors: Box::leak(selectors.into_boxed_slice()),
        ..SHEETS
    }))
}
// Valid suffix-separated families must work; registrations admitting shared keys
// or protected keys must fail independently of whether that key exists today.
#[test]
fn families_accept_disjoint_suffixes_and_reject_static_intersections() {
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
        vec![family("qos:", "", 32, STRING)],
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

#[test]
fn family_capture_bounds_and_required_membership_are_checked_on_current_and_target() {
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

static CYCLIC: HistoryMapOverlay = HistoryMapOverlay {
    fields: &[HistoryMapField {
        key: "cycle",
        required: false,
        node: HistoryRestoreNode::MapOverlay(&CYCLIC),
    }],
    entries: None,
};
#[test]
fn invalid_overlay_shapes_and_recursive_static_graphs_fail_registration() {
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
#[test]
fn reject_policy_refuses_new_entity_but_allows_retained_profile_edits() {
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

// A missing live structural container must not import its historical view state.
#[test]
fn creates_absent_mixed_container_with_selected_children_only() {
    let (mut doc, _, target) = sheets_fixture();
    doc.delete(ROOT, "wb").unwrap();
    assert_eq!(restore(&mut doc, &SHEETS, &target), RestoreOutcome::Changed);
    let wb = object(&doc, &ROOT, "wb");
    assert_eq!(doc.length(object(&doc, &wb, "sheets")), 1);
    assert!(doc.get(&wb, "active").unwrap().is_none());
}

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
#[test]
fn recursive_overlays_keep_every_mixed_ancestor_and_protected_object() {
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
        doc.get(&selection, 0).unwrap().unwrap().0.to_str(),
        Some("protected-id")
    );
    assert_eq!(doc.get(&wb, "active").unwrap().unwrap().0.to_i64(), Some(7));
    let sheets = object(&doc, &wb, "sheets");
    assert_eq!(
        doc.get(&sheets, 0).unwrap().unwrap().0.to_str(),
        Some("original")
    );
}

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
#[test]
fn omission_is_admitted_only_when_combined_result_validator_accepts_it() {
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

#[test]
fn optional_selected_child_absence_preserves_present_parent_and_unknown_children() {
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
        doc.get(&wb, "future-view").unwrap().unwrap().0.to_str(),
        Some("current")
    );
}

// Browser's actual tags contain colons; UTF-8 identity captures are byte-bounded.
#[test]
fn real_browser_tags_and_unicode_family_identities_restore() {
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
        doc.get(ROOT, "draft.é.text").unwrap().unwrap().0.to_str(),
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

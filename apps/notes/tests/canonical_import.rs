use crate::component_support::canonical as support;
use crate::component_support::kind_at;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use notes::{
    actions::{InlineMark, InlineMarkSpan},
    doc::NotesDoc,
    legacy_import::{self, ImportError, LegacySourceKey, destination, import_legacy},
    notebook, storage,
};
use support::LegacyFixture;
use ui::record::{
    self, RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOpenOutcome,
    RecordSaveStatus,
};
use uuid::Uuid;

fn imported(f: &LegacyFixture) -> legacy_import::ImportOutcome {
    import_legacy(&f.records, &f.source, f.key).unwrap()
}
fn refuse_source(f: &LegacyFixture) {
    let before = f.source_bytes();
    let error = import_legacy(&f.records, &f.source, f.key).unwrap_err();
    assert!(matches!(error, ImportError::Source(_)), "{error:?}");
    let checkout = destination(f.key);
    assert!(!f.records.is_ready(&checkout.id));
    assert!(!f.document_path(&checkout).exists());
    assert_eq!(f.source_bytes(), before);
}
fn mark(start: usize, end: usize, mark: InlineMark) -> InlineMarkSpan {
    InlineMarkSpan { start, end, mark }
}

#[allow(
    clippy::too_many_lines,
    reason = "keep the complete hand-checked row and native-mark oracle together"
)]
#[test]
fn rich_import_preserves_content_and_replays_with_stable_ids() {
    // Catches lost structure, formatting, UTF-8 offsets, code rows, or local selection leakage.
    eprintln!("phase rich_import_preserves_tree_blocks_marks_and_source");
    {
        let f = LegacyFixture::rich();
        let before = f.source_bytes();
        let outcome = imported(&f);
        assert!(!outcome.already_imported);
        assert_eq!(outcome.initial_selected, Some(f.child));
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.source_key, "31".repeat(32));
        assert_eq!(
            f.records.save_status(&outcome.checkout.id).unwrap(),
            RecordSaveStatus::Saved
        );
        f.records
            .read(&outcome.checkout.id, |doc| {
                notebook::validate(doc).unwrap();
                let workspace = notebook::project(doc, Some(f.child)).unwrap();
                assert_eq!(workspace.selected(), Some(f.child));
                for page in &f.workspace.data.pages {
                    assert!(workspace.data.pages.contains(page));
                }
                assert_eq!(workspace.data.pages.len(), 3);
                assert_eq!(NotesDoc::list_tags(doc), f.workspace.data.tags);
                assert_eq!(NotesDoc::list_tag_assignments(doc), vec![(f.child, f.tag)]);
                let rows = NotesDoc::page_block_snapshots(doc, f.child);
                let expected = [
                    ("heading1", "Heading one"),
                    ("heading2", "Heading two"),
                    ("heading3", "Heading three"),
                    ("paragraph", "é bold ital λ Projects Empty"),
                    ("bullet:0", "bullet"),
                    ("bullet:1", "nested"),
                    ("numbered:0", "numbered"),
                    ("numbered:1", "nested number"),
                    ("checklist:0:0", "todo"),
                    ("checklist:1:1", "done"),
                    ("code:rust", "let x = 1;"),
                    ("code:rust", "let y = 2;"),
                    ("code:", "plain code"),
                    ("divider", ""),
                ];
                assert_eq!(rows.len(), expected.len());
                let marks = NotesDoc::page_block_marks(doc, f.child);
                for (index, ((id, text), (kind, want))) in rows.iter().zip(expected).enumerate() {
                    assert!(!id.is_empty());
                    assert_eq!(text, want);
                    assert_eq!(kind_at(doc, f.child, index), kind);
                    let mut actual = marks[index].1.clone();
                    actual.sort_by_key(|m| (m.start, m.end));
                    let expected_marks = match index {
                        0 => vec![mark(8, 11, InlineMark::Bold)],
                        1 => vec![mark(8, 11, InlineMark::Italic)],
                        3 => vec![
                            mark(3, 7, InlineMark::Bold),
                            mark(8, 12, InlineMark::Italic),
                            mark(13, 15, InlineMark::Code),
                            mark(
                                16,
                                24,
                                InlineMark::Link {
                                    page: f.root.to_string(),
                                },
                            ),
                            mark(
                                25,
                                30,
                                InlineMark::Link {
                                    page: f.empty.to_string(),
                                },
                            ),
                        ],
                        _ => vec![],
                    };
                    assert_eq!(actual, expected_marks);
                }
                assert_eq!(
                    NotesDoc::page_block_marks(doc, f.root)[1].1,
                    vec![mark(
                        4,
                        10,
                        InlineMark::Link {
                            page: f.child.to_string()
                        }
                    )]
                );
                assert_eq!(
                    NotesDoc::page_block_snapshots(doc, f.empty)
                        .iter()
                        .map(|(_, s)| s.as_str())
                        .collect::<Vec<_>>(),
                    vec![""]
                );
                assert!(doc.get(ROOT, "selected").unwrap().is_none());
                let (_, receipt) = doc.get(ROOT, "notes_import_v1").unwrap().unwrap();
                let mut keys = doc.keys(&receipt).collect::<Vec<_>>();
                keys.sort();
                assert_eq!(
                    keys,
                    [
                        "destination_id",
                        "format",
                        "snapshot_digest",
                        "source_key",
                        "version"
                    ]
                );
            })
            .unwrap();
        assert_eq!(f.source_bytes(), before);
    }

    eprintln!("phase replay_and_reopen_keep_ids_without_reading_source");
    {
        let f = LegacyFixture::rich();
        let outcome = imported(&f);
        let checkout = outcome.checkout;
        let original = f
            .records
            .read(&checkout.id, |d| NotesDoc::page_block_snapshots(d, f.child))
            .unwrap();
        let f = f.reopen();
        assert_eq!(
            f.records.try_open_record(&checkout).unwrap(),
            RecordOpenOutcome::Loaded
        );
        std::fs::rename(&f.source, f.temp.path().join("retained-source")).unwrap();
        let again = imported(&f);
        assert!(again.already_imported);
        assert_eq!(again.initial_selected, None);
        assert_eq!(
            f.records
                .read(&checkout.id, |d| NotesDoc::page_block_snapshots(d, f.child))
                .unwrap(),
            original
        );
        let g = LegacyFixture::rich();
        let other = imported(&g);
        assert_eq!(
            g.records
                .read(&other.checkout.id, |d| NotesDoc::page_block_snapshots(
                    d, g.child
                ))
                .unwrap(),
            original
        );
    }

    eprintln!("phase destination_mapping_is_fixed_for_zero_and_ff_keys");
    {
        for (byte, hex) in [(0, "00"), (255, "ff")] {
            let checkout = destination(LegacySourceKey::from_bytes([byte; 32]));
            assert_eq!(
                checkout.id.as_str(),
                format!(
                    "derived:notes-legacy-v1@derived:key-{half}@derived:key-{half}@tab:0000000000000000",
                    half = hex.repeat(16)
                )
            );
            assert!(checkout.is_valid());
            assert_eq!(checkout.kind.as_str(), "notes.notebook");
        }
    }

    eprintln!("phase explicit_source_location_key_is_stable_and_read_only");
    {
        let f = LegacyFixture::rich();
        let before = f.source_bytes();
        let a = legacy_import::source_key(&f.source).unwrap();
        assert_eq!(a, legacy_import::source_key(&f.source.join(".")).unwrap());
        assert_ne!(a, legacy_import::source_key(f.temp.path()).unwrap());
        assert_eq!(f.source_bytes(), before);
    }

    eprintln!("phase empty_initialization_and_projection_are_read_only");
    {
        let mut doc = AutoCommit::new();
        notebook::initialize_empty(&mut doc).unwrap();
        notebook::validate(&doc).unwrap();
        let heads = doc.get_heads();
        let workspace = notebook::project(&doc, Some(Uuid::from_u128(42))).unwrap();
        assert!(workspace.data.pages.is_empty());
        assert_eq!(workspace.selected(), None);
        assert_eq!(doc.get_heads(), heads);
        let mut keys = doc.keys(ROOT).collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "assets",
                "notes_schema_version",
                "pages",
                "tag_assignments",
                "tags",
            ]
        );
    }
}

#[test]
fn malformed_metadata_never_publishes_and_errors_name_the_page() {
    eprintln!("phase malformed_metadata_models_never_publish");
    {
        for case in [
            "duplicate-page",
            "cycle",
            "self-cycle",
            "orphan",
            "order",
            "selected",
            "duplicate-tag",
            "empty-tag",
            "edge-page",
            "edge-tag",
            "duplicate-edge",
            "ambiguous-title",
        ] {
            let mut f = LegacyFixture::rich();
            let unknown = Uuid::from_u128(0xdead);
            match case {
                "duplicate-page" => {
                    let mut page = f.workspace.data.pages[0].clone();
                    page.order = 2;
                    f.workspace.data.pages.push(page);
                }
                "cycle" => {
                    f.workspace.data.pages[0].parent = Some(f.child);
                    f.workspace.data.pages[2].order = 0;
                }
                "self-cycle" => f.workspace.data.pages[1].parent = Some(f.child),
                "orphan" => f.workspace.data.pages[1].parent = Some(unknown),
                "order" => f.workspace.data.pages[2].order = 3,
                "selected" => f.workspace.data.selected = Some(unknown),
                "duplicate-tag" => f.workspace.data.tags.push(f.workspace.data.tags[0].clone()),
                "empty-tag" => f.workspace.data.tags[0].name = "  ".into(),
                "edge-page" => f.workspace.data.tag_assignments[0].page = unknown,
                "edge-tag" => f.workspace.data.tag_assignments[0].tag = unknown,
                "duplicate-edge" => f
                    .workspace
                    .data
                    .tag_assignments
                    .push(f.workspace.data.tag_assignments[0].clone()),
                "ambiguous-title" => f.workspace.data.pages[2].title = "Projects".into(),
                _ => unreachable!(),
            }
            f.write_metadata();
            refuse_source(&f);
        }
    }

    eprintln!("phase source_errors_name_the_affected_page_and_path");
    {
        let f = LegacyFixture::rich();
        f.write_page(f.child, &[255]);
        let ImportError::Source(message) = import_legacy(&f.records, &f.source, f.key).unwrap_err()
        else {
            panic!("source error required")
        };
        assert!(message.contains(&f.child.to_string()));
        assert!(message.contains(&f.source.display().to_string()));
    }
}

#[test]
fn malformed_files_and_legacy_shapes_never_publish() {
    for case in [
        "missing-metadata",
        "directory-metadata",
        "missing-page",
        "directory-page",
        "empty-metadata",
        "corrupt",
        "scalar",
        "missing-json",
        "bad-json",
        "bad-id",
        "missing-pages",
        "pages-shape",
        "utf8",
        "partial",
    ] {
        let f = LegacyFixture::rich();
        let meta = storage::workspace_file_path(&f.source);
        let page = storage::page_file_path(&f.source, f.child);
        match case {
            "missing-metadata" => std::fs::remove_file(&meta).unwrap(),
            "directory-metadata" => {
                std::fs::remove_file(&meta).unwrap();
                std::fs::create_dir(&meta).unwrap();
            }
            "missing-page" => std::fs::remove_file(&page).unwrap(),
            "directory-page" => {
                std::fs::remove_file(&page).unwrap();
                std::fs::create_dir(&page).unwrap();
            }
            "empty-metadata" => std::fs::write(&meta, []).unwrap(),
            "corrupt" => std::fs::write(&meta, b"not automerge").unwrap(),
            "scalar" | "missing-json" => {
                let mut d = AutoCommit::new();
                if case == "scalar" {
                    d.put(ROOT, "workspace_json", "{}").unwrap();
                }
                std::fs::write(&meta, d.save()).unwrap();
            }
            "bad-json" => std::fs::write(&meta, support::wrapped_json("{")).unwrap(),
            "bad-id" => std::fs::write(
                &meta,
                support::wrapped_json(
                    r#"{"pages":[{"id":"no-uuid","title":"x","parent":null,"order":0}]}"#,
                ),
            )
            .unwrap(),
            "missing-pages" => std::fs::write(&meta, support::wrapped_json("{}")).unwrap(),
            "pages-shape" => {
                std::fs::write(&meta, support::wrapped_json(r#"{"pages":42}"#)).unwrap();
            }
            "utf8" => f.write_page(f.child, &[255, 254]),
            "partial" => {
                let d = AutoCommit::load(&std::fs::read(&meta).unwrap()).unwrap();
                std::fs::write(&meta, support::partial_tail(d)).unwrap();
            }
            _ => unreachable!(),
        }
        refuse_source(&f);
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn import_links_keep_labels_and_resolve_embeds_unambiguously() {
    eprintln!("phase unresolved_links_keep_labels_and_duplicate_unreferenced_titles_are_allowed");
    {
        let mut f = LegacyFixture::rich();
        f.workspace.data.pages[2].title = "Projects".into();
        f.write_metadata();
        f.write_page(f.child, b"See [[Missing]].\n");
        let outcome = imported(&f);
        assert!(outcome.warnings.iter().any(|s| s.contains("Missing")));
        f.records
            .read(&outcome.checkout.id, |d| {
                assert_eq!(
                    NotesDoc::page_block_snapshots(d, f.child)[0].1,
                    "See Missing."
                );
                assert!(NotesDoc::page_block_marks(d, f.child)[0].1.is_empty());
            })
            .unwrap();
    }

    eprintln!("phase embedded_note_references_resolve_preserve_marks_and_refuse_ambiguous_targets");
    {
        let f = LegacyFixture::rich();
        f.write_page(
            f.child,
            "é **before** ![[Projects]] and ![[Missing]]\n".as_bytes(),
        );
        let source = f.source_bytes();
        let outcome = imported(&f);
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("![[Missing]]"));
        let expected = f
            .records
            .read(&outcome.checkout.id, |doc| {
                notebook::validate(doc).unwrap();
                let rows = NotesDoc::page_block_snapshots(doc, f.child);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].1, "é before Projects and Missing");
                let marks = NotesDoc::page_block_marks(doc, f.child);
                assert_eq!(
                    marks[0].1,
                    vec![
                        mark(3, 9, InlineMark::Bold),
                        mark(
                            10,
                            18,
                            InlineMark::Transclusion {
                                page: f.root.to_string()
                            }
                        )
                    ]
                );
                (rows, marks)
            })
            .unwrap();
        assert_eq!(source, f.source_bytes());
        let f = f.reopen();
        f.records.try_open_record(&outcome.checkout).unwrap();
        assert_eq!(
            f.records
                .read(&outcome.checkout.id, |doc| (
                    NotesDoc::page_block_snapshots(doc, f.child),
                    NotesDoc::page_block_marks(doc, f.child)
                ))
                .unwrap(),
            expected
        );

        let mut ambiguous = LegacyFixture::rich();
        ambiguous.workspace.data.pages[2].title = "Projects".into();
        ambiguous.write_metadata();
        ambiguous.write_page(ambiguous.child, b"![[Projects]]\n");
        refuse_source(&ambiguous);
    }

    eprintln!("phase deleting_link_target_retains_surviving_text_and_read_only_projection");
    {
        use tilo_actions::Action;
        let f = LegacyFixture::rich();
        let checkout = imported(&f).checkout;
        let before = f
            .records
            .read(&checkout.id, |d| {
                (
                    NotesDoc::page_block_snapshots(d, f.child),
                    NotesDoc::page_block_marks(d, f.child),
                )
            })
            .unwrap();
        f.records
            .with(&checkout.id, |d| {
                notes::actions::NotesAction::DeletePage {
                    id: f.empty.to_string(),
                }
                .apply(d)
                .unwrap();
            })
            .unwrap();
        let heads = f.records.with(&checkout.id, AutoCommit::get_heads).unwrap();
        f.records
            .read(&checkout.id, |d| {
                notebook::validate(d).unwrap();
                let workspace = notebook::project(d, Some(f.child)).unwrap();
                assert_eq!(workspace.data.pages.len(), 2);
                assert_eq!(workspace.selected(), Some(f.child));
                assert_eq!(
                    (
                        NotesDoc::page_block_snapshots(d, f.child),
                        NotesDoc::page_block_marks(d, f.child)
                    ),
                    before
                );
            })
            .unwrap();
        assert_eq!(
            f.records.with(&checkout.id, AutoCommit::get_heads).unwrap(),
            heads
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn import_destination_is_retained_and_never_reimported() {
    eprintln!("phase busy_destination_refuses_before_source_read_and_retries_once");
    {
        let f = LegacyFixture::rich();
        let checkout = destination(f.key);
        let reservation = f.records.reserve_record(&checkout).unwrap();
        assert!(matches!(
            import_legacy(&f.records, &f.temp.path().join("absent"), f.key),
            Err(ImportError::Record(RecordOpenError::Busy))
        ));
        drop(reservation);
        let first = imported(&f);
        assert!(!first.already_imported);
        assert!(imported(&f).already_imported);
    }

    eprintln!("phase unrelated_failed_writer_does_not_override_saved_import");
    {
        let f = LegacyFixture::rich();
        let other = RecordCheckout::new(
            RecordId::derived("notes-fixture-unrelated", &RecordId::for_tab(0)),
            RecordKind::new("notes.notebook"),
        );
        f.records
            .reserve_record(&other)
            .unwrap()
            .initialize(notebook::initialize_empty)
            .unwrap();
        f.records.try_flush().unwrap();
        let path = f.document_path(&other);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        f.records
            .with(&other.id, |d| d.put(ROOT, "fixture_pending", true).unwrap());
        assert!(f.records.try_flush().is_err());
        let result = imported(&f);
        assert_eq!(
            f.records.save_status(&result.checkout.id).unwrap(),
            RecordSaveStatus::Saved
        );
        assert!(f.records.try_flush().is_err());
        std::fs::remove_dir(path).unwrap();
        f.records.try_flush().unwrap();
    }

    eprintln!("phase deleting_every_page_and_tag_through_actions_does_not_reimport");
    {
        use notes::actions::NotesAction;
        use tilo_actions::Action;
        let f = LegacyFixture::rich();
        let checkout = imported(&f).checkout;
        // Explicit authority is retained for the caller; import itself performs private initialization.
        assert_eq!(f.author.0, "notes-fixture-author");
        let _authority = f.authority.borrow();
        f.records
            .with(&checkout.id, |d| {
                NotesAction::DeletePage {
                    id: f.root.to_string(),
                }
                .apply(d)
                .unwrap();
                NotesAction::DeletePage {
                    id: f.empty.to_string(),
                }
                .apply(d)
                .unwrap();
                NotesAction::DeleteTag {
                    id: f.tag.to_string(),
                }
                .apply(d)
                .unwrap();
            })
            .unwrap();
        let heads = f.records.with(&checkout.id, AutoCommit::get_heads).unwrap();
        let result = imported(&f);
        assert!(result.already_imported);
        assert_eq!(result.initial_selected, None);
        f.records
            .read(&checkout.id, |d| {
                assert!(NotesDoc::list_pages(d).is_empty());
                assert!(NotesDoc::list_tags(d).is_empty());
                notebook::validate(d).unwrap();
            })
            .unwrap();
        assert_eq!(
            f.records.with(&checkout.id, AutoCommit::get_heads).unwrap(),
            heads
        );
    }

    eprintln!("phase existing_conflict_is_byte_identical_and_partial_prefix_is_never_reimported");
    {
        for receipt in ["missing", "mismatched", "matching"] {
            for torn in [false, true] {
                let f = LegacyFixture::rich();
                let checkout = imported(&f).checkout;
                let mut doc = f.records.read(&checkout.id, Clone::clone).unwrap();
                match receipt {
                    "missing" => {
                        doc.delete(ROOT, "notes_import_v1").unwrap();
                    }
                    "mismatched" => {
                        let (_, r) = doc.get(ROOT, "notes_import_v1").unwrap().unwrap();
                        doc.put(&r, "source_key", "ab".repeat(32)).unwrap();
                    }
                    _ => {}
                }
                let bytes = if torn {
                    support::partial_tail(doc)
                } else {
                    doc.save()
                };
                let f = f.reopen();
                f.write_destination(&checkout, &bytes);
                std::fs::rename(&f.source, f.temp.path().join("retained-source")).unwrap();
                let result = import_legacy(&f.records, &f.source, f.key);
                if receipt == "matching" {
                    assert!(result.unwrap().already_imported);
                } else {
                    assert!(matches!(result,Err(ImportError::Conflict(ref c)) if c==&checkout));
                }
                let current = std::fs::read(f.document_path(&checkout)).unwrap();
                if torn {
                    let accepted = AutoCommit::load(&current).unwrap();
                    assert!(accepted.get(ROOT, "fixture_tail").unwrap().is_none());
                    notebook::validate(&accepted).unwrap();
                } else {
                    assert_eq!(current, bytes);
                }
            }
        }
    }
}

#[test]
fn canonical_validation_refuses_versions_and_malformed_shapes_without_reset() {
    for case in [
        "missing-version",
        "future-version",
        "pages",
        "tags",
        "edges",
        "page-id",
        "title",
        "parent",
        "order",
        "body",
    ] {
        let f = LegacyFixture::rich();
        let outcome = imported(&f);
        let mut doc = f.records.read(&outcome.checkout.id, Clone::clone).unwrap();
        let (_, pages) = doc.get(ROOT, "pages").unwrap().unwrap();
        let (_, page) = doc.get(&pages, f.child.to_string()).unwrap().unwrap();
        match case {
            "missing-version" => {
                doc.delete(ROOT, "notes_schema_version").unwrap();
            }
            "future-version" => {
                doc.put(
                    ROOT,
                    "notes_schema_version",
                    u64::from(notebook::NOTES_SCHEMA_VERSION) + 1,
                )
                .unwrap();
            }
            "pages" | "tags" | "edges" => {
                doc.put(
                    ROOT,
                    if case == "edges" {
                        "tag_assignments"
                    } else {
                        case
                    },
                    "bad",
                )
                .unwrap();
            }
            "page-id" => {
                doc.put_object(&pages, "invalid", ObjType::Map).unwrap();
            }
            "title" | "parent" | "order" | "body" => {
                doc.put(&page, case, 42).unwrap();
            }
            _ => unreachable!(),
        }
        let before = doc.get_heads();
        assert!(notebook::validate(&doc).is_err(), "{case}");
        assert!(notebook::project(&doc, None).is_err(), "{case}");
        assert_eq!(doc.get_heads(), before);
    }
}

#[test]
fn refused_imports_retain_destination_bytes_and_migration_errors() {
    eprintln!("phase corruption_schema_and_identity_refusals_retain_destination_bytes");
    {
        for case in ["corrupt", "schema", "identity"] {
            for torn in [false, true] {
                let f = LegacyFixture::rich();
                let checkout = destination(f.key);
                let mut doc = AutoCommit::new();
                let id = if case == "identity" {
                    RecordId::derived("wrong-id", &RecordId::for_tab(0))
                } else {
                    checkout.id.clone()
                };
                record::init_scoped(
                    &mut doc,
                    &id,
                    &checkout.kind,
                    support::ROOM,
                    Some(support::ROOM),
                );
                notebook::initialize_empty(&mut doc).unwrap();
                if case == "schema" {
                    doc.put(
                        ROOT,
                        app_state::SCHEMA_VERSION_KEY,
                        u64::from(app_state::SCHEMA_VERSION) + 1,
                    )
                    .unwrap();
                }
                let bytes = if case == "corrupt" {
                    b"unrecoverable".to_vec()
                } else if torn {
                    support::partial_tail(doc)
                } else {
                    doc.save()
                };
                f.write_destination(&checkout, &bytes);
                let before = f.source_bytes();
                assert!(matches!(
                    import_legacy(&f.records, &f.source, f.key),
                    Err(ImportError::Record(_))
                ));
                assert!(!f.records.is_ready(&checkout.id));
                assert_eq!(std::fs::read(f.document_path(&checkout)).unwrap(), bytes);
                assert_eq!(f.source_bytes(), before);
            }
        }
    }

    eprintln!(
        "phase accepted_prefix_with_missing_notebook_version_returns_retained_migration_error"
    );
    {
        let f = LegacyFixture::rich();
        let checkout = imported(&f).checkout;
        let mut doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        doc.delete(ROOT, "notes_schema_version").unwrap();
        let bytes = support::partial_tail(doc);
        let f = f.reopen();
        f.write_destination(&checkout, &bytes);
        let error = import_legacy(&f.records, &f.source, f.key).unwrap_err();
        assert!(
            matches!(error,ImportError::Record(RecordOpenError::Initialization(ref s)) if s.contains("migration")),
            "{error:?}"
        );
        assert!(f.records.is_ready(&checkout.id));
        let accepted =
            AutoCommit::load(&std::fs::read(f.document_path(&checkout)).unwrap()).unwrap();
        assert!(
            accepted
                .get(ROOT, "notes_schema_version")
                .unwrap()
                .is_none()
        );
        assert!(accepted.get(ROOT, "fixture_tail").unwrap().is_none());
    }
}

// Future/unsupported content must not be silently dropped from a successful import.
#[test]
fn unsupported_legacy_fields_are_refused_without_publication() {
    let mut accepted = Vec::new();
    for location in ["automerge-root", "workspace", "page", "tag", "assignment"] {
        let f = LegacyFixture::rich();
        let metadata = storage::workspace_file_path(&f.source);
        if location == "automerge-root" {
            let mut doc = AutoCommit::load(&std::fs::read(&metadata).unwrap()).unwrap();
            doc.put(ROOT, "attachments", "unsupported retained content")
                .unwrap();
            std::fs::write(&metadata, doc.save()).unwrap();
        } else {
            let mut json = serde_json::to_value(&f.workspace.data).unwrap();
            let target = match location {
                "workspace" => &mut json,
                "page" => &mut json["pages"][0],
                "tag" => &mut json["tags"][0],
                "assignment" => &mut json["tag_assignments"][0],
                _ => unreachable!(),
            };
            target["attachments"] = serde_json::json!(["unsupported retained content"]);
            std::fs::write(&metadata, support::wrapped_json(&json.to_string())).unwrap();
        }
        assert!(storage::decode_workspace(&std::fs::read(&metadata).unwrap()).is_ok());
        let before = f.source_bytes();
        let checkout = destination(f.key);
        let result = import_legacy(&f.records, &f.source, f.key);
        if !matches!(result, Err(ImportError::Source(ref message)) if message.contains("attachments"))
            || f.records.is_ready(&checkout.id)
            || f.document_path(&checkout).exists()
        {
            accepted.push(format!("{location}: {result:?}"));
        }
        assert_eq!(f.source_bytes(), before);
    }
    assert!(
        accepted.is_empty(),
        "unsupported source data accepted: {accepted:?}"
    );
}

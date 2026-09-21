use crate::component_support::*;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use notes::{actions, doc::NotesDoc};

fn select(driver: &mut ui::headless::Driver, hash: automerge::ChangeHash) {
    let id = ui::history_source::entry_id_from_hash(&hash.to_string());
    let target = format!("timeline-entry:{}", ui::widget::history_group::hex16(&id));
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
}

fn perform_named_target(
    driver: &mut ui::headless::Driver,
    name: &str,
    target: &str,
    action: &ui::a11y::Action,
) {
    fn visit_access(
        node: &ui::a11y::AccessNode,
        owner: &str,
        name: &str,
        target: &str,
        found: &mut Vec<(String, Option<String>)>,
    ) {
        if node.name.as_deref() == Some(name) && node.action_target.as_deref() == Some(target) {
            found.push((owner.to_owned(), node.action_target.clone()));
        }
        for child in &node.children {
            visit_access(child, owner, name, target, found);
        }
    }
    fn visit_inspection(
        node: &ui::headless::InspectionNode,
        name: &str,
        target: &str,
        found: &mut Vec<(String, Option<String>)>,
    ) {
        if let Some(access) = &node.semantics {
            visit_access(access, &node.path, name, target, found);
        }
        for child in &node.children {
            visit_inspection(child, name, target, found);
        }
    }

    let mut routes = Vec::new();
    visit_inspection(&driver.inspection().root, name, target, &mut routes);
    assert_eq!(routes.len(), 1, "expected one {name:?}/{target:?} route");
    let (owner, action_target) = routes.pop().unwrap();
    driver
        .perform_semantic_named(&owner, action_target.as_deref(), action)
        .unwrap();
}

// Removing component registration, projecting the outer launcher State, or
// restoring only page titles must fail this real saved-notebook exercise.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one saved-notebook lifecycle keeps all projection and custody assertions together"
)]
fn notebook_shared_history_previews_restores_and_undoes_saved_content() {
    let f = LegacyFixture::rich();
    let checkout = imported(&f);
    let source = f.source_bytes();
    let mut d = driver(&f, 11, &checkout);
    invoke(
        &mut d,
        11,
        actions::page_resource_id(f.child),
        "rename",
        Some("Selected release"),
    );
    let selected = f
        .records
        .read(&checkout.id, |doc| doc.base_heads()[0])
        .unwrap();
    let target = f.records.read(&checkout.id, Clone::clone).unwrap();
    f.records
        .with_owned_action(
            &checkout.id,
            11,
            canonical::PACKAGE,
            f.author.clone(),
            tilo_actions::ActionContext::default(),
            |doc, _| {
                let mut page = NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.child)
                    .unwrap();
                page.parent = None;
                page.order = 2;
                NotesDoc::upsert_page_meta(doc, &page);
                let blocks = NotesDoc::page_blocks_list(doc, f.child).unwrap();
                let text = NotesDoc::block_text_obj(doc, &blocks, 0).unwrap();
                doc.splice_text(
                    &text,
                    0,
                    isize::try_from(doc.text(&text).unwrap().chars().count()).unwrap(),
                    "changed marked block",
                )
                .unwrap();
                NotesDoc::upsert_tag(
                    doc,
                    &notes::model::TagMeta {
                        id: f.tag,
                        name: "Live tag".into(),
                    },
                );
                NotesDoc::set_tag_assignment(doc, f.child, f.tag, false);
                doc.put(ROOT, "workspace_json", "retained legacy metadata")
                    .unwrap();
            },
        )
        .unwrap();

    invoke(
        &mut d,
        11,
        actions::page_resource_id(f.child),
        "rename",
        Some("Live release"),
    );
    let before = f
        .records
        .read(&checkout.id, Transactable::base_heads)
        .unwrap();
    assert!(
        d.targets().contains_key("history-open"),
        "actual canonical Notes needs shared History"
    );
    d.click_named("history-open").unwrap();
    select(&mut d, selected);
    d.click_named("history-next-page").unwrap();
    let preview = format!("{:?}", d.a11y_nodes());
    assert!(preview.contains("Selected release"), "{preview}");
    assert!(
        preview.contains("Projects"),
        "tree navigation must retain the parent page"
    );
    assert_eq!(
        f.records
            .read(&checkout.id, Transactable::base_heads)
            .unwrap(),
        before
    );
    d.type_text("must not edit historical content");
    d.key("Backspace");
    assert_eq!(
        f.records
            .read(&checkout.id, Transactable::base_heads)
            .unwrap(),
        before
    );
    d.click_named("history-before").unwrap();
    assert!(!format!("{:?}", d.a11y_nodes()).contains("changed marked block"));
    d.click_named("history-after").unwrap();
    d.resize((480, 700)).unwrap();
    d.click_named("history-previous-page").unwrap();
    d.perform_named("history-next-page", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("Enter");
    assert!(format!("{:?}", d.a11y_nodes()).contains("Heading one"));
    d.click_named("history-restore").unwrap();
    d.key("Enter");
    f.records
        .read(&checkout.id, |doc| {
            assert_eq!(
                NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.child)
                    .unwrap()
                    .title,
                "Selected release"
            );
            assert_eq!(doc.clone().get_changes(&before).len(), 1);
            notes::notebook::validate(doc).unwrap();
            assert_eq!(
                NotesDoc::page_block_snapshots(doc, f.child),
                NotesDoc::page_block_snapshots(&target, f.child)
            );
            assert_eq!(
                NotesDoc::page_block_marks(doc, f.child),
                NotesDoc::page_block_marks(&target, f.child)
            );
            assert_eq!(NotesDoc::list_pages(doc), NotesDoc::list_pages(&target));
            assert_eq!(NotesDoc::list_tags(doc), NotesDoc::list_tags(&target));
            assert_eq!(
                notes::notebook::project(doc, None)
                    .unwrap()
                    .data
                    .tag_assignments,
                notes::notebook::project(&target, None)
                    .unwrap()
                    .data
                    .tag_assignments
            );
            assert_eq!(
                doc.get(ROOT, "notes_import_v1").unwrap(),
                target.get(ROOT, "notes_import_v1").unwrap()
            );
            assert_eq!(
                doc.get(ROOT, "notes_schema_version").unwrap(),
                target.get(ROOT, "notes_schema_version").unwrap()
            );
            assert_eq!(
                doc.get(ROOT, "workspace_json").unwrap().unwrap().0.to_str(),
                Some("retained legacy metadata")
            );
        })
        .unwrap();
    f.records
        .with_history(&checkout.id, |doc, history| {
            history.undo_in(doc, &f.author, 11, None, None).unwrap();
            assert_eq!(
                NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.child)
                    .unwrap()
                    .title,
                "Live release"
            );
        })
        .unwrap();
    close_driver(&mut d);
    assert_eq!(f.source_bytes(), source);
    drop(d);
    let f = f.reopen();
    f.records.try_open_record(&checkout).unwrap();
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc)
                .into_iter()
                .find(|p| p.id == f.child)
                .unwrap()
                .title)
            .unwrap(),
        "Live release"
    );
}

#[test]
fn legacy_notebook_refuses_a_second_writer_until_first_is_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let first = notes::NotesApp::spawn(temp.path().to_path_buf()).unwrap();
    assert!(notes::NotesApp::spawn(temp.path().to_path_buf()).is_err());
    drop(first);
    assert!(notes::NotesApp::spawn(temp.path().to_path_buf()).is_ok());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one saved-notebook lifecycle keeps all projection and custody assertions together"
)]
fn production_preparation_imports_once_preserves_sources_and_reopens_real_history() {
    let f = LegacyFixture::rich();
    let source = f.source_bytes();
    assert!(
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .is_err()
    );
    let prepared =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, true)
            .unwrap();
    let mut surfaces = 0;
    ui::Widget::visit_framework_history(&prepared.root, &mut |_, _| surfaces += 1);
    assert_eq!(surfaces, 1);
    let checkout = prepared.checkout.clone();
    let original = prepared
        .records
        .read(&checkout.id, Transactable::base_heads)
        .unwrap();
    let records = prepared.records.clone();
    let mut d = ui::headless::Driver::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(prepared.root),
        (1000, 700),
        ui::Theme::dark(),
        renderer(),
        f.author.clone(),
    );
    assert!(d.targets().contains_key("history-open"));
    records
        .with_owned_action(
            &checkout.id,
            1,
            canonical::PACKAGE,
            f.author.clone(),
            tilo_actions::ActionContext::default(),
            |doc, _| {
                let mut page = NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.root)
                    .unwrap();
                page.title = "Saved production change".into();
                NotesDoc::upsert_page_meta(doc, &page);
            },
        )
        .unwrap();
    d.relayout();
    let outer = (
        d.state_mut().doc.base_heads(),
        d.state_mut().doc.pending_ops(),
    );
    let selected = records
        .read(&checkout.id, |doc| doc.base_heads()[0])
        .unwrap();
    d.click_named("history-open").unwrap();
    select(&mut d, selected);
    let pinned = format!("{:?}", d.a11y_nodes());
    assert!(pinned.contains("Saved production change"));
    records
        .with_owned_action(
            &checkout.id,
            2,
            canonical::PACKAGE,
            ui::permissions::UserId("second-owner-fixture".into()),
            tilo_actions::ActionContext::default(),
            |doc, _| {
                let mut page = NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.root)
                    .unwrap();
                page.title = "Later live update".into();
                NotesDoc::upsert_page_meta(doc, &page);
            },
        )
        .unwrap();
    d.relayout();
    assert!(format!("{:?}", d.a11y_nodes()).contains("Saved production change"));
    assert!(!format!("{:?}", d.a11y_nodes()).contains("Later live update"));
    let live_before_restore = records
        .read(&checkout.id, Transactable::base_heads)
        .unwrap();
    d.click_named("history-restore").unwrap();
    d.key("Enter");
    records
        .read(&checkout.id, |doc| {
            assert_eq!(doc.clone().get_changes(&live_before_restore).len(), 1);
            assert_eq!(
                NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.root)
                    .unwrap()
                    .title,
                "Saved production change"
            );
        })
        .unwrap();
    assert_eq!(
        (
            d.state_mut().doc.base_heads(),
            d.state_mut().doc.pending_ops()
        ),
        outer
    );
    d.close_components();
    for report in d.take_component_close_reports() {
        assert_closed(&report);
    }
    assert_eq!(
        records.save_status(&checkout.id).unwrap(),
        ui::record::RecordSaveStatus::Saved
    );
    let heads = records
        .read(&checkout.id, Transactable::base_heads)
        .unwrap();
    assert_ne!(heads, original);
    drop(d);
    drop(records);
    drop(prepared.records);
    let reopened =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .unwrap();
    assert_eq!(reopened.checkout, checkout);
    assert_eq!(
        reopened
            .records
            .read(&checkout.id, Transactable::base_heads)
            .unwrap(),
        heads
    );
    for (path, bytes) in source {
        assert_eq!(std::fs::read(f.source.join(path)).unwrap(), bytes);
    }
    assert!(
        notes::NotesApp::spawn(f.source).is_err(),
        "retired legacy writers must not fork saved notebook content"
    );
}

#[test]
fn retired_source_refuses_wrong_namespace_and_missing_canonical_record() {
    let f = LegacyFixture::rich();
    let prepared =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, true)
            .unwrap();
    let checkout = prepared.checkout.clone();
    drop(prepared);
    assert!(
        notes::production::prepare_notebook(&f.source, &f.state_root, "wrong-room", true).is_err()
    );
    assert!(
        notes::production::prepare_notebook(
            &f.source,
            &f.temp.path().join("wrong-state"),
            canonical::ROOM,
            true
        )
        .is_err()
    );
    std::fs::remove_file(f.document_path(&checkout)).unwrap();
    assert!(
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, true)
            .is_err()
    );
}

#[test]
fn notebook_restore_rechecks_stale_frontier_and_current_authority() {
    for revoke in [false, true] {
        let f = LegacyFixture::rich();
        let checkout = imported(&f);
        let mut d = driver(&f, 11, &checkout);
        invoke(
            &mut d,
            11,
            actions::page_resource_id(f.child),
            "rename",
            Some("Selected"),
        );
        let selected = f
            .records
            .read(&checkout.id, |doc| doc.base_heads()[0])
            .unwrap();
        invoke(
            &mut d,
            11,
            actions::page_resource_id(f.child),
            "rename",
            Some("Live"),
        );
        d.click_named("history-open").unwrap();
        select(&mut d, selected);
        d.click_named("history-restore").unwrap();
        if revoke {
            *f.authority.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
                ui::permissions::Actor::person(None, &f.author.0),
                vec![],
            )
            .at_unix_time(100);
        } else {
            f.records
                .with_owned_action(
                    &checkout.id,
                    12,
                    canonical::PACKAGE,
                    f.author.clone(),
                    tilo_actions::ActionContext::default(),
                    |doc, _| {
                        let mut page = NotesDoc::list_pages(doc)
                            .into_iter()
                            .find(|p| p.id == f.child)
                            .unwrap();
                        page.title = "Concurrent".into();
                        NotesDoc::upsert_page_meta(doc, &page);
                    },
                )
                .unwrap();
        }
        let heads = f
            .records
            .read(&checkout.id, Transactable::base_heads)
            .unwrap();
        d.key("Enter");
        assert_eq!(
            f.records
                .read(&checkout.id, Transactable::base_heads)
                .unwrap(),
            heads
        );
        close_driver(&mut d);
    }
}

#[test]
fn durable_import_without_retirement_marker_reopens_the_same_graph() {
    let f = LegacyFixture::rich();
    let key = notes::legacy_import::source_key(&f.source).unwrap();
    let imported = notes::legacy_import::import_legacy(&f.records, &f.source, key).unwrap();
    let heads = f
        .records
        .read(&imported.checkout.id, Transactable::base_heads)
        .unwrap();
    let checkout = imported.checkout;
    let source = f.source.clone();
    let state_root = f.state_root.clone();
    drop(f.records);
    let prepared =
        notes::production::prepare_notebook(&source, &state_root, canonical::ROOM, false).unwrap();
    assert_eq!(prepared.checkout, checkout);
    assert_eq!(
        prepared
            .records
            .read(&checkout.id, Transactable::base_heads)
            .unwrap(),
        heads
    );
    assert!(source.join(".notes-canonical-v1").is_file());
}

fn assert_historical_qos_custody(imported: bool) {
    let f = LegacyFixture::rich();
    let historical = f.write_qos_canonical_v1(imported);
    let before_document = std::fs::read(&historical.document_path).unwrap();
    let before_marker = std::fs::read(&historical.marker_path).unwrap();
    assert_eq!(before_marker, historical.marker);
    assert!(historical.record.ends_with(&historical.source_key));
    assert!(ui::record::RecordId::parse(&historical.record).is_none());

    let prepared =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .unwrap_or_else(|error| {
                panic!("deployed qOS custody must reopen without migration: {error}")
            });
    assert_eq!(prepared.checkout.id.as_str(), historical.record);
    assert_eq!(prepared.checkout.kind.as_str(), "notes.notebook");
    prepared
        .records
        .read(&prepared.checkout.id, notes::notebook::validate)
        .expect("historical record remains open")
        .expect("historical Notes document remains valid");
    prepared.records.try_flush().unwrap();
    assert_eq!(
        std::fs::read(&historical.marker_path).unwrap(),
        before_marker
    );
    assert_eq!(
        std::fs::read(&historical.document_path).unwrap(),
        before_document
    );

    let checkout = prepared.checkout.clone();
    let records = prepared.records.clone();
    let original_pages = records
        .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
        .unwrap();
    let selected = records
        .read(&checkout.id, |doc| doc.base_heads()[0])
        .unwrap();
    let mut live = ui::headless::Driver::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(prepared.root),
        (1000, 700),
        ui::Theme::dark(),
        renderer(),
        f.author.clone(),
    );
    perform_named_target(
        &mut live,
        "New note",
        "notes:new-page",
        &ui::a11y::Action::Activate,
    );
    // Owned semantic dispatch keeps its framework input frame active through
    // the dispatching relayout. Drive the deterministic ordinary-host sequence
    // that applies the queued intent and closes its admitted action bracket.
    assert!(
        live.wait_until(ui::clock::Duration::ZERO, |_| {
            records
                .read(&checkout.id, |doc| {
                    doc.pending_ops() == 0 && NotesDoc::list_pages(doc).len() == original_pages + 1
                })
                .unwrap_or(false)
        }),
        "the admitted Notes action must commit on the next ordinary host sequence"
    );
    assert_eq!(
        records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
            .unwrap(),
        original_pages + 1
    );
    live.click_named("history-open").unwrap();
    select(&mut live, selected);
    live.click_named("history-restore").unwrap();
    live.key("Enter");
    assert_eq!(
        records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
            .unwrap(),
        original_pages
    );
    close_driver(&mut live);
    let heads = records
        .read(&checkout.id, Transactable::base_heads)
        .unwrap();
    drop(live);
    drop(records);
    drop(prepared.records);

    let reopened =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .unwrap();
    assert_eq!(reopened.checkout.id.as_str(), historical.record);
    assert_eq!(
        reopened
            .records
            .read(&reopened.checkout.id, Transactable::base_heads)
            .unwrap(),
        heads
    );
    assert_eq!(
        std::fs::read(&historical.marker_path).unwrap(),
        before_marker
    );

    let tilo_key = notes::legacy_import::source_key(&f.source).unwrap();
    let tilo = notes::legacy_import::destination(tilo_key);
    assert!(
        !f.document_path(&tilo).exists(),
        "must not fork into Tilo custody"
    );
}

#[test]
fn production_reopens_exact_historical_qos_imported_custody() {
    assert_historical_qos_custody(true);
}

#[test]
fn production_reopens_exact_historical_qos_fresh_custody_without_receipt() {
    assert_historical_qos_custody(false);
}

#[test]
fn historical_qos_marker_tampering_never_opens_or_forks_custody() {
    for case in [
        "version",
        "state_root",
        "workspace",
        "package",
        "record",
        "extra",
    ] {
        let f = LegacyFixture::rich();
        let historical = f.write_qos_canonical_v1(true);
        let document = std::fs::read(&historical.document_path).unwrap();
        let mut marker: serde_json::Value = serde_json::from_slice(&historical.marker).unwrap();
        match case {
            "version" => marker["version"] = 2.into(),
            "state_root" => {
                marker["state_root"] = f
                    .temp
                    .path()
                    .join("other-state")
                    .to_string_lossy()
                    .into_owned()
                    .into();
            }
            "workspace" => marker["workspace"] = "other-room".into(),
            "package" => marker["package"] = "dev.foreign.notes".into(),
            "record" => {
                marker["record"] = format!("derived:notes-legacy-v1@{}", "ab".repeat(32)).into()
            }
            "extra" => marker["unexpected"] = true.into(),
            _ => unreachable!(),
        }
        let marker = serde_json::to_vec(&marker).unwrap();
        std::fs::write(&historical.marker_path, &marker).unwrap();

        assert!(
            notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
                .is_err(),
            "marker tamper {case} must fail"
        );
        assert_eq!(std::fs::read(&historical.marker_path).unwrap(), marker);
        assert_eq!(std::fs::read(&historical.document_path).unwrap(), document);
        let tilo =
            notes::legacy_import::destination(notes::legacy_import::source_key(&f.source).unwrap());
        assert!(!f.document_path(&tilo).exists());
    }
}

#[test]
fn historical_qos_document_and_receipt_tampering_fail_closed() {
    for case in [
        "id",
        "kind",
        "home",
        "room",
        "refs",
        "current-meta",
        "storage-schema",
        "notes-schema",
        "receipt-source",
        "receipt-destination",
        "receipt-format",
        "receipt-version",
        "receipt-snapshot-case",
        "receipt-extra",
    ] {
        let f = LegacyFixture::rich();
        let historical = f.write_qos_canonical_v1(true);
        let marker = std::fs::read(&historical.marker_path).unwrap();
        let mut doc =
            automerge::AutoCommit::load(&std::fs::read(&historical.document_path).unwrap())
                .unwrap();
        let (_, meta) = doc.get(ROOT, "qos:record").unwrap().unwrap();
        let (_, receipt) = doc.get(ROOT, "notes_import_v1").unwrap().unwrap();
        match case {
            "id" | "kind" | "home" | "room" => {
                doc.put(&meta, case, "foreign").unwrap();
            }
            "refs" => {
                doc.put_object(&meta, "refs", automerge::ObjType::Map)
                    .unwrap();
            }
            "current-meta" => {
                doc.put_object(ROOT, "tilo:record", automerge::ObjType::Map)
                    .unwrap();
            }
            "storage-schema" => {
                doc.put(ROOT, "qos:schema_version", 2_u64).unwrap();
            }
            "notes-schema" => {
                doc.put(ROOT, "notes_schema_version", 3_u64).unwrap();
            }
            "receipt-source" => {
                doc.put(&receipt, "source_key", "ab".repeat(32)).unwrap();
            }
            "receipt-destination" => {
                doc.put(
                    &receipt,
                    "destination_id",
                    "derived:foreign@tab:0000000000000000",
                )
                .unwrap();
            }
            "receipt-format" => {
                doc.put(&receipt, "format", "foreign").unwrap();
            }
            "receipt-version" => {
                doc.put(&receipt, "version", 2_u64).unwrap();
            }
            "receipt-snapshot-case" => {
                doc.put(&receipt, "snapshot_digest", "AB".repeat(32))
                    .unwrap();
            }
            "receipt-extra" => {
                doc.put(&receipt, "unexpected", true).unwrap();
            }
            _ => unreachable!(),
        }
        let tampered = doc.save();
        std::fs::write(&historical.document_path, &tampered).unwrap();
        assert!(
            notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
                .is_err(),
            "document tamper {case} must fail"
        );
        assert_eq!(std::fs::read(&historical.marker_path).unwrap(), marker);
        assert_eq!(std::fs::read(&historical.document_path).unwrap(), tampered);
        let tilo =
            notes::legacy_import::destination(notes::legacy_import::source_key(&f.source).unwrap());
        assert!(!f.document_path(&tilo).exists());
    }
}

#[test]
fn historical_qos_missing_corrupt_and_contended_storage_never_falls_back() {
    for case in ["missing", "corrupt", "contended"] {
        let f = LegacyFixture::rich();
        let historical = f.write_qos_canonical_v1(false);
        let marker = std::fs::read(&historical.marker_path).unwrap();
        let mut held = None;
        match case {
            "missing" => std::fs::remove_file(&historical.document_path).unwrap(),
            "corrupt" => std::fs::write(&historical.document_path, b"corrupt").unwrap(),
            "contended" => {
                held = Some(
                    app_state::DocStore::open_canonical_at(
                        canonical::QOS_PACKAGE,
                        historical.document_path.parent().unwrap(),
                    )
                    .unwrap(),
                );
            }
            _ => unreachable!(),
        }
        assert!(
            notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
                .is_err(),
            "unavailable historical storage {case} must fail"
        );
        drop(held);
        assert_eq!(std::fs::read(&historical.marker_path).unwrap(), marker);
        let tilo =
            notes::legacy_import::destination(notes::legacy_import::source_key(&f.source).unwrap());
        assert!(!f.document_path(&tilo).exists());
    }
}

#[test]
fn actual_binary_normal_launch_prepares_default_canonical_notebook_and_history() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("application-state");
    let workspace = "00000000-0000-0000-0000-000000000023";
    let dump = temp.path().join("inspection.json");
    let spec = serde_json::json!({"actions":[{"click_named":"history-open"}],"inspect":dump});
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_notes"))
        .env("XDG_DATA_HOME", &profile)
        .env("TILO_PACKAGE_ID", canonical::PACKAGE)
        .env("TILO_WORKSPACE_ID", workspace)
        .env("TILO_PARTICIPATION", "author")
        .args(["--headless-drive", &spec.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let inspection = std::fs::read_to_string(&dump).unwrap();
    assert!(inspection.contains("activity_timeline"), "{inspection}");
    let checkout = notes::hosting::default_notebook();
    let directory = app_state::DocStore::room_record_dir_at(
        &profile,
        canonical::PACKAGE,
        workspace,
        &checkout.id.storage_key(),
    )
    .unwrap();
    assert!(directory.join(app_state::DOCUMENT_FILE).is_file());
}

#[test]
fn actual_binary_refuses_legacy_notebook_selector_in_headless_mode_before_path_access() {
    let temp = tempfile::tempdir().unwrap();
    let notebook = temp.path().join("must-not-be-created");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_notes"))
        .env("XDG_DATA_HOME", temp.path().join("application-state"))
        .env("TILO_PACKAGE_ID", canonical::PACKAGE)
        .args([
            "--notebook-dir",
            notebook.to_str().unwrap(),
            "--headless-drive",
            "{}",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("legacy notebook recovery cannot run in capture, audit, or headless mode"),
        "stderr={stderr}"
    );
    assert!(!notebook.exists());
}

#[test]
fn production_close_failure_retains_the_saved_record_owner_for_retry() {
    let f = LegacyFixture::rich();
    let prepared =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, true)
            .unwrap();
    let checkout = prepared.checkout.clone();
    let path = f.document_path(&checkout);
    let records = prepared.records.clone();
    let mut d = ui::headless::Driver::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(prepared.root),
        (1000, 700),
        ui::Theme::dark(),
        renderer(),
        f.author.clone(),
    );
    let sibling = ui::record::RecordCheckout::new(
        ui::RecordId::for_tab(9991),
        ui::record::RecordKind::new("unrelated"),
    );
    records
        .reserve_record(&sibling)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    records
        .with(&sibling.id, |doc| {
            doc.put(ROOT, "pending-sibling", "untouched").unwrap();
        })
        .unwrap();
    let sibling_before = records
        .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    records
        .with_owned_action(
            &checkout.id,
            11,
            canonical::PACKAGE,
            f.author.clone(),
            tilo_actions::ActionContext::default(),
            |doc, _| {
                let mut page = NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.root)
                    .unwrap();
                page.title = "Retained unsaved change".into();
                NotesDoc::upsert_page_meta(doc, &page);
            },
        )
        .unwrap();
    d.close_components();
    let mut report = d
        .take_component_close_reports()
        .into_iter()
        .find(|report| report.record_error.is_some())
        .expect("save failure must retain recovery");
    assert!(report.record_retry.is_some());
    assert_eq!(report.record_retry_target.as_ref(), Some(&checkout.id));
    assert_eq!(
        records
            .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        sibling_before
    );
    drop(d);
    drop(records);
    drop(prepared.records);
    assert!(notes::NotesApp::spawn(f.source.clone()).is_err());
    std::fs::remove_dir(&path).unwrap();
    let retained = report.record_retry.as_ref().unwrap().clone();
    report.retry_record().unwrap();
    assert_eq!(
        retained
            .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        sibling_before
    );
    drop(retained);
    assert!(report.record_retry.is_none());
    let reopened =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .unwrap();
    assert_eq!(
        reopened
            .records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc)
                .into_iter()
                .find(|p| p.id == f.root)
                .unwrap()
                .title)
            .unwrap(),
        "Retained unsaved change"
    );
}

#[test]
fn historical_qos_close_failure_retries_the_same_custody_and_reopens() {
    let f = LegacyFixture::rich();
    let historical = f.write_qos_canonical_v1(true);
    let marker = std::fs::read(&historical.marker_path).unwrap();
    let prepared =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .unwrap();
    let checkout = prepared.checkout.clone();
    let records = prepared.records.clone();
    let mut live = ui::headless::Driver::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(prepared.root),
        (1000, 700),
        ui::Theme::dark(),
        renderer(),
        f.author.clone(),
    );
    std::fs::remove_file(&historical.document_path).unwrap();
    std::fs::create_dir(&historical.document_path).unwrap();
    records
        .with_owned_action(
            &checkout.id,
            35,
            canonical::QOS_PACKAGE,
            f.author.clone(),
            tilo_actions::ActionContext::default(),
            |doc, _| {
                let mut page = NotesDoc::list_pages(doc).into_iter().next().unwrap();
                page.title = "Retained qOS change".into();
                NotesDoc::upsert_page_meta(doc, &page);
            },
        )
        .unwrap();
    live.close_components();
    let mut report = live
        .take_component_close_reports()
        .into_iter()
        .find(|report| report.record_error.is_some())
        .expect("historical save failure retains exact recovery owner");
    assert_eq!(report.record_retry_target.as_ref(), Some(&checkout.id));
    assert!(report.record_retry.is_some());
    drop(live);
    drop(records);
    drop(prepared.records);

    std::fs::remove_dir(&historical.document_path).unwrap();
    report.retry_record().unwrap();
    assert!(report.record_retry.is_none());
    let reopened =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .unwrap();
    assert_eq!(reopened.checkout.id.as_str(), historical.record);
    assert_eq!(
        reopened
            .records
            .read(&reopened.checkout.id, |doc| NotesDoc::list_pages(doc)[0]
                .title
                .clone())
            .unwrap(),
        "Retained qOS change"
    );
    assert_eq!(std::fs::read(&historical.marker_path).unwrap(), marker);
    let tilo =
        notes::legacy_import::destination(notes::legacy_import::source_key(&f.source).unwrap());
    assert!(!f.document_path(&tilo).exists());
}

#[derive(Clone, Copy)]
enum OutgoingTransition {
    Rebind,
    Replace,
    MutableReplacement,
}

#[allow(
    clippy::too_many_lines,
    reason = "one failed outgoing-owner lifecycle keeps retry, sibling isolation and durable reopen assertions together"
)]
fn assert_outgoing_notebook_recovery(transition: OutgoingTransition, reject_incoming: bool) {
    use ui::Widget as _;
    let f = LegacyFixture::rich();
    let notes::production::PreparedNotebook {
        root: mut tile,
        records,
        checkout,
    } = notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, true)
        .unwrap();
    let path = f.document_path(&checkout);
    let sibling = ui::record::RecordCheckout::new(
        ui::RecordId::for_tab(9911),
        ui::record::RecordKind::new("sibling"),
    );
    records
        .reserve_record(&sibling)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    records
        .with(&sibling.id, |doc| {
            doc.put(ROOT, "pending", "A sibling").unwrap();
        })
        .unwrap();
    let sibling_before = records
        .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    let incoming = ui::record::RecordOwners::new(f.temp.path().join("incoming"))
        .open(canonical::PACKAGE, canonical::ROOM)
        .unwrap();
    let incoming_checkout = ui::record::RecordCheckout::new(
        ui::RecordId::for_tab(9912),
        ui::record::RecordKind::new("notes.notebook"),
    );
    incoming
        .reserve_record(&incoming_checkout)
        .unwrap()
        .initialize(notes::notebook::initialize_empty)
        .unwrap();
    incoming
        .with(&incoming_checkout.id, |doc| {
            doc.put(ROOT, "pending", "B unfinished").unwrap();
        })
        .unwrap();
    let incoming_before = incoming
        .read(&incoming_checkout.id, |doc| {
            (doc.base_heads(), doc.pending_ops())
        })
        .unwrap();
    let requested = if reject_incoming {
        ui::record::RecordCheckout::new(
            ui::RecordId::for_tab(9913),
            ui::record::RecordKind::new("missing"),
        )
    } else {
        incoming_checkout.clone()
    };
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    records
        .with_owned_action(
            &checkout.id,
            11,
            canonical::PACKAGE,
            f.author.clone(),
            tilo_actions::ActionContext::default(),
            |doc, _| {
                let mut page = NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|p| p.id == f.root)
                    .unwrap();
                page.title = "Recovered after outgoing rebind".into();
                NotesDoc::upsert_page_meta(doc, &page);
            },
        )
        .unwrap();
    // No explicit close: custody-changing APIs must retain the failure themselves.
    match transition {
        OutgoingTransition::Rebind => {}
        OutgoingTransition::Replace => tile.replace_content(ui::widget::TileContent::Notice {
            title: "Replacement".into(),
            detail: String::new(),
        }),
        OutgoingTransition::MutableReplacement => {
            *tile.content_mut() = ui::widget::TileContent::Notice {
                title: "Mutable replacement".into(),
                detail: String::new(),
            }
        }
    }
    tile.bind_record(incoming.clone(), requested.clone());
    tile.bind_record(incoming.clone(), requested);
    let mut reports = tile.take_component_close_reports();
    assert_eq!(
        reports.len(),
        1,
        "one outgoing settlement despite replacement and repeated bind"
    );
    let mut report = reports.pop().unwrap();
    assert!(report.record_error.is_some());
    assert_eq!(report.record_retry_target.as_ref(), Some(&checkout.id));
    assert!(
        report
            .record_retry
            .as_ref()
            .unwrap()
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc)
                .iter()
                .any(|page| page.title == "Recovered after outgoing rebind"))
            .unwrap()
    );
    assert_eq!(
        records
            .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        sibling_before
    );
    assert_eq!(
        incoming
            .read(&incoming_checkout.id, |doc| (
                doc.base_heads(),
                doc.pending_ops()
            ))
            .unwrap(),
        incoming_before
    );
    assert!(tile.take_component_close_reports().is_empty());
    // Only the recovery report may retain A when B admission fails or succeeds.
    drop(tile);
    drop(records);
    drop(f.records);
    drop(f.owners);
    std::fs::remove_dir(&path).unwrap();
    let retained = report.record_retry.as_ref().unwrap().clone();
    report.retry_record().unwrap();
    assert_eq!(
        retained
            .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        sibling_before
    );
    assert_eq!(
        incoming
            .read(&incoming_checkout.id, |doc| (
                doc.base_heads(),
                doc.pending_ops()
            ))
            .unwrap(),
        incoming_before
    );
    drop(retained);
    assert!(report.record_retry.is_none());
    assert!(report.record_retry_target.is_none());
    let reopened =
        notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, false)
            .unwrap();
    assert_eq!(
        reopened
            .records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc)
                .into_iter()
                .find(|p| p.id == f.root)
                .unwrap()
                .title)
            .unwrap(),
        "Recovered after outgoing rebind"
    );
}

#[test]
fn outgoing_notebook_rebind_retains_failed_owner_without_preclose() {
    for rejected in [false, true] {
        assert_outgoing_notebook_recovery(OutgoingTransition::Rebind, rejected);
    }
}
#[test]
fn outgoing_notebook_replacement_retains_failed_owner_without_preclose() {
    for rejected in [false, true] {
        assert_outgoing_notebook_recovery(OutgoingTransition::Replace, rejected);
    }
}
#[test]
fn outgoing_notebook_mutable_replacement_retains_failed_owner_without_preclose() {
    assert_outgoing_notebook_recovery(OutgoingTransition::MutableReplacement, true);
}

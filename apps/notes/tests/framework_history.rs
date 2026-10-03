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

#[allow(
    clippy::too_many_lines,
    reason = "one saved-notebook lifecycle keeps all projection and custody assertions together"
)]
#[test]
fn notebook_history_previews_restores_and_refuses_a_second_writer() {
    notes::app::present_history_button_for_test(false);
    eprintln!("phase notebook_shared_history_previews_restores_and_undoes_saved_content");
    {
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
                    let block = NotesDoc::page_block_snapshots(doc, f.child)[0].0.clone();
                    replace_block_text(doc, f.child, &block, "changed marked block");
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
                    doc.get(ROOT, "workspace_json").unwrap().unwrap().0.as_str(),
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

    eprintln!("phase legacy_notebook_refuses_a_second_writer_until_first_is_dropped");
    {
        let temp = tempfile::tempdir().unwrap();
        let first = notes::NotesApp::spawn(temp.path().to_path_buf()).unwrap();
        assert!(notes::NotesApp::spawn(temp.path().to_path_buf()).is_err());
        drop(first);
        assert!(notes::NotesApp::spawn(temp.path().to_path_buf()).is_ok());
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one saved-notebook lifecycle keeps all projection and custody assertions together"
)]
#[test]
fn production_preparation_imports_once_and_reopens_the_same_graph() {
    notes::app::present_history_button_for_test(false);
    eprintln!(
        "phase production_preparation_imports_once_preserves_sources_and_reopens_real_history"
    );
    {
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
        ui::history_mode::capability::visit_framework_history(&prepared.root, &mut |_, _| {
            surfaces += 1;
        });
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

    eprintln!("phase retired_source_refuses_wrong_namespace_and_missing_canonical_record");
    {
        let f = LegacyFixture::rich();
        let prepared =
            notes::production::prepare_notebook(&f.source, &f.state_root, canonical::ROOM, true)
                .unwrap();
        let checkout = prepared.checkout.clone();
        drop(prepared);
        assert!(
            notes::production::prepare_notebook(&f.source, &f.state_root, "wrong-room", true)
                .is_err()
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

    eprintln!("phase durable_import_without_retirement_marker_reopens_the_same_graph");
    {
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
            notes::production::prepare_notebook(&source, &state_root, canonical::ROOM, false)
                .unwrap();
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
}

#[test]
fn notebook_restore_rechecks_stale_frontier_and_current_authority() {
    notes::app::present_history_button_for_test(false);
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
fn actual_binary_normal_launch_prepares_default_canonical_notebook_and_history() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("application-state");
    let workspace = "00000000-0000-0000-0000-000000000023";
    let dump = temp.path().join("inspection.json");
    let spec = serde_json::json!({"actions":[],"inspect":dump});
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
    // The note header presents the window's History button, in place of the window's own bar.
    assert!(inspection.contains("history-button"), "{inspection}");
    // A launch into a workspace opens the room's shared default record.
    let checkout = notes::hosting::launch_notebook(true);
    let directory = app_state::DocStore::room_record_dir_at(
        &profile,
        canonical::PACKAGE,
        workspace,
        &checkout.id.storage_key(),
    )
    .unwrap();
    assert!(directory.join(app_state::DOCUMENT_FILE).is_file());
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn failed_closes_and_rebinds_retain_the_saved_owner() {
    eprintln!("phase production_close_failure_retains_the_saved_record_owner_for_retry");
    {
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

    eprintln!("phase outgoing_notebook_rebind_retains_failed_owner_without_preclose");
    {
        for rejected in [false, true] {
            assert_outgoing_notebook_recovery(OutgoingTransition::Rebind, rejected);
        }
    }
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
    let mut reports = ui::component_lifecycle::take_component_close_reports(&mut tile);
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
    assert!(ui::component_lifecycle::take_component_close_reports(&mut tile).is_empty());
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
fn outgoing_replacements_retain_the_failed_owner_without_preclose() {
    eprintln!("phase outgoing_notebook_replacement_retains_failed_owner_without_preclose");
    {
        for rejected in [false, true] {
            assert_outgoing_notebook_recovery(OutgoingTransition::Replace, rejected);
        }
    }

    eprintln!("phase outgoing_notebook_mutable_replacement_retains_failed_owner_without_preclose");
    {
        assert_outgoing_notebook_recovery(OutgoingTransition::MutableReplacement, true);
    }
}

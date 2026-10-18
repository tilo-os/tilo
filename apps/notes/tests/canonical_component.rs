use crate::component_support::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use notes::read;
use ui::{Widget, application::LaunchTarget, widget::TileHost};

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn canonical_component_starts_empty_projects_existing_records_read_only() {
    // Catches accidental starter pages, shared New identities, and legacy-source effects.
    eprintln!("phase new_is_empty_schema_ready_and_instances_have_distinct_records");
    {
        let f = NotebookFixture::rich();
        let mut a = component(&f, 11, LaunchTarget::New);
        let mut b = component(&f, 12, LaunchTarget::New);
        assert_ne!(a.record().id, b.record().id);
        for checkout in [a.record(), b.record()] {
            f.records
                .read(&checkout.id, |doc| {
                    let workspace = read::project(doc).unwrap();
                    assert!(workspace.pages().is_empty());
                    assert!(workspace.tags().is_empty());
                })
                .unwrap();
        }
        assert_closed(&a.close());
        assert_closed(&b.close());
    }

    // Catches mount/sync writes, empty-list seeding, or using disk data instead of the record.
    eprintln!("phase existing_rich_record_projects_real_resources_without_writing");
    {
        fn collect(
            widget: &dyn Widget,
            doc: &AutoCommit,
            resources: &mut Vec<tilo_actions::Resource>,
        ) {
            resources.extend(ui::resources::resources(widget, doc));
            for child in widget.children() {
                collect(child, doc, resources);
            }
        }
        fn has_resource(widget: &dyn Widget, doc: &AutoCommit, id: &str) -> bool {
            ui::resources::resource(widget, doc, id).is_some()
                || widget
                    .children()
                    .iter()
                    .any(|child| has_resource(*child, doc, id))
        }
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        // The accepted importer preserves one blank Markdown paragraph. Exercise
        // the canonical empty-list state after explicitly deleting that block.
        f.records
            .with(&checkout.id, |doc| {
                let block_id = read::page_block_snapshots(doc, f.empty)[0].0.clone();
                remove_block(doc, f.empty, &block_id);
                doc.commit();
            })
            .unwrap();
        let heads = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        let mut a = TileHost::from_component(component(&f, 11, existing(&checkout)));
        let mut b = TileHost::from_component(component(&f, 12, existing(&checkout)));
        let outer = AutoCommit::new();
        for _ in 0..3 {
            a.sync(&outer, &ROOT);
            b.sync(&outer, &ROOT);
        }
        let mut resources = Vec::new();
        collect(&a, &outer, &mut resources);
        assert!(!resources.is_empty());
        for page in [f.root, f.child, f.empty] {
            assert!(has_resource(
                &a,
                &outer,
                &notes::actions::page_resource_id(page)
            ));
        }
        f.records
            .read(&checkout.id, |doc| {
                assert_eq!(read::page_block_snapshots(doc, f.empty).len(), 0);
                assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
            })
            .unwrap();
        assert!(outer.base_heads().is_empty());
        assert_eq!(outer.pending_ops(), 0);
        close_widget(&mut a);
        close_widget(&mut b);
    }

    // Catches resurrecting an emptied notebook from a still-present legacy workspace.
    eprintln!("phase reopening_an_emptied_import_never_resurrects_legacy_pages");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut live = driver(&f, 11, &checkout);
        let address = ui::bound_dispatch::BoundActionAddress {
            instance_id: 11,
            address: notes::Notebook::DELETE.on(notes::actions::page_resource_id(f.root)),
        };
        let untouched = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert!(matches!(
            live.invoke_bound_action(&address, &serde_json::Map::new()),
            Err(tilo_actions::DoorError::ConfirmationRequired)
        ));
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            untouched
        );
        assert_eq!(revision(&f, &checkout), 0);
        invoke_confirmed(
            &mut live,
            11,
            notes::actions::page_resource_id(f.root),
            "delete",
        );
        invoke_confirmed(
            &mut live,
            11,
            notes::actions::page_resource_id(f.empty),
            "delete",
        );
        invoke_confirmed(
            &mut live,
            11,
            notes::actions::tag_resource_id(f.tag),
            "delete-tag",
        );
        assert_eq!(revision(&f, &checkout), 3);
        close_driver(&mut live);
        drop(live);
        f.records.try_flush().unwrap();
        let f = f.reopen();
        let mut tile = TileHost::from_component(component(&f, 11, existing(&checkout)));
        let heads = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        tile.sync(&AutoCommit::new(), &ROOT);
        f.records
            .read(&checkout.id, |doc| {
                assert!(read::project(doc).unwrap().pages().is_empty());
                assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
            })
            .unwrap();
        close_widget(&mut tile);
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn canonical_component_status_and_binding_hydrate_without_touching_the_document() {
    eprintln!("phase delivered_record_status_updates_real_notes_status_without_touching_document");
    {
        use automerge::transaction::Transactable;
        use ui::record::RecordSaveStatus;
        let mut doc = AutoCommit::new();
        read::initialize(&mut doc);
        doc.commit();
        let before = (doc.base_heads(), doc.pending_ops());
        let mut app =
            notes::NotesApp::from_document(&doc, &ui::permissions::UserId("same-author".into()))
                .unwrap();
        let saved = ui::LocalState::new(RecordSaveStatus::Saved);
        app.follow_save_status(saved.clone());
        for (status, label) in [
            (RecordSaveStatus::MemoryOnly, "Not saved"),
            (RecordSaveStatus::Saving, "Saving…"),
            (
                RecordSaveStatus::Failed("disk failure".into()),
                "Save failed",
            ),
            (RecordSaveStatus::Saved, "Saved"),
        ] {
            saved.set(status);
            app.sync(&doc, &ROOT);
            assert_eq!(app.status_bar_for_test().save_text(), label);
            app.refresh_status_for_test();
            assert_eq!(app.status_bar_for_test().save_text(), label);
        }
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    }

    eprintln!(
        "phase canonical_binding_hydrates_and_clears_marks_and_reconciles_empty_selection_readonly"
    );
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        let before = (doc.base_heads(), doc.pending_ops());
        let mut app = notes::NotesApp::from_document(&doc, &f.author).unwrap();
        assert_eq!(app.selected_for_test(), Some(f.root));
        app.navigate_to(f.child);
        app.sync(&doc, &ROOT);
        let expected = read::page_block_marks(&doc, f.child)
            .into_iter()
            .map(|(id, mut marks)| {
                marks.sort_by_key(|mark| mark.start);
                (id, marks)
            })
            .collect::<Vec<_>>();
        assert_eq!(app.main_panel_mut().inline_marks(), expected);
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        assert!(
            app.main_panel_mut()
                .bind_canonical_page(&doc, uuid::Uuid::new_v4(), "wrong")
                .is_err()
        );
        assert_eq!(app.main_panel_mut().page_id(), Some(f.child));
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        let first = app.main_panel_mut().editor().blocks()[0].id.clone();
        app.main_panel_mut()
            .editor_mut()
            .focus_block(0, ui::widget::CursorPlacement::Byte(2));
        for _ in 0..3 {
            app.sync(&doc, &ROOT);
        }
        assert_eq!(app.main_panel_mut().editor().blocks()[0].id, first);
        assert_eq!(
            app.main_panel_mut().editor().blocks()[0].line.cursor_pos(),
            2
        );
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        for (block, marks) in expected {
            for span in marks {
                let text = read::page_block_snapshots(&doc, f.child)
                    .into_iter()
                    .find(|(id, _)| *id == block)
                    .unwrap()
                    .1;
                let chars = |byte: usize| text[..byte].chars().count();
                let (name, _) = tilo_rich_text::document_mark(&span.to_ui().unwrap().mark);
                edit_body(
                    &mut doc,
                    f.child,
                    tilo_document::RichTextAction::SetMark {
                        region: tilo_document::action::RegionKind::Body,
                        spans: vec![tilo_document::action::Span {
                            block: block.clone(),
                            start: chars(span.start),
                            end: chars(span.end),
                        }],
                        name: name.into(),
                        value: None,
                        also_clear: Vec::new(),
                    },
                );
            }
        }
        doc.commit();
        let cleared = (doc.base_heads(), doc.pending_ops());
        app.sync(&doc, &ROOT);
        assert!(
            app.main_panel_mut()
                .inline_marks()
                .iter()
                .all(|(_, marks)| marks.is_empty())
        );
        assert_eq!((doc.base_heads(), doc.pending_ops()), cleared);
        tilo_actions::Action::apply(&notes::actions::delete_page_action(f.root), &mut doc).unwrap();
        doc.commit();
        let deleted = (doc.base_heads(), doc.pending_ops());
        app.sync(&doc, &ROOT);
        assert_eq!(app.selected_for_test(), Some(f.empty));
        assert_eq!((doc.base_heads(), doc.pending_ops()), deleted);
        tilo_actions::Action::apply(&notes::actions::delete_page_action(f.empty), &mut doc)
            .unwrap();
        tilo_actions::Action::apply(&notes::actions::delete_tag_action(f.tag), &mut doc).unwrap();
        doc.commit();
        let emptied = (doc.base_heads(), doc.pending_ops());
        app.sync(&doc, &ROOT);
        assert_eq!(app.selected_for_test(), None);
        assert!(app.main_panel_mut().children().is_empty());
        assert!(read::project(&doc).unwrap().tags().is_empty());
        assert_eq!((doc.base_heads(), doc.pending_ops()), emptied);
    }
}

#[test]
fn native_bound_open_binds_the_editor_after_the_worker_has_settled() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut app =
        ui::App::from_component(component(&f, 11, existing(&checkout)), runner_inputs()).unwrap();
    app.init_layout();
    app.controlled_hosted_model_for_test();
    app.controlled_hosted_model_for_test();
    assert!(
        !app.controlled_hosted_model_for_test(),
        "native worker settles before navigation"
    );
    let before = f
        .records
        .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    app.refresh_semantic_tree();
    let target = app
        .semantic_tree()
        .iter()
        .find(|node| node.name.as_deref() == Some("Notebook") && node.action("open").is_some())
        .expect("the notebook opens an item")
        .id
        .clone();
    let launch = notes::actions::page_resource_id(f.child);
    assert!(
        app.invoke_semantic_action(&target, "open", Some(&launch))
            .unwrap()
    );
    let queued = app
        .controlled_native_input_snapshot_for_test()
        .expect("observer before the ordinary navigation pump");
    assert_eq!(queued["pending_page_events"], 1);
    assert_ne!(queued["selected_page"], f.child.to_string());
    assert!(
        app.controlled_hosted_model_for_test(),
        "completed bound navigation schedules its worker"
    );
    let observed = app
        .controlled_native_input_snapshot_for_test()
        .expect("drawn Notes editor observer");
    assert_eq!(observed["bound_page"], f.child.to_string());
    assert_eq!(observed["selected_page"], f.child.to_string());
    assert_projection_observation(&observed, f.child);
    let body = f
        .records
        .read(&checkout.id, |doc| {
            read::page_block_snapshots(doc, f.child)
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap();
    assert!(!body.is_empty());
    assert_eq!(
        observed["body_hash"],
        blake3::hash(body.as_bytes()).to_hex().to_string()
    );
    assert_eq!(observed["body_chars"], body.chars().count());
    assert!(observed["rendered_body_chars"].as_u64().unwrap() > 0);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        before,
        "opening binds the real canonical body without writing it"
    );
    assert!(
        !app.controlled_hosted_model_for_test(),
        "navigation does not keep the worker spinning"
    );
    app.close_components();
    for report in app.take_component_close_reports() {
        assert_closed(&report);
    }
}

#[test]
fn native_accessibility_page_activation_binds_an_empty_note_without_another_input() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut app =
        ui::App::from_component(component(&f, 11, existing(&checkout)), runner_inputs()).unwrap();
    app.controlled_hosted_model_for_test();
    app.controlled_hosted_model_for_test();
    app.init_layout();
    assert!(!app.controlled_hosted_model_for_test());
    let before = f
        .records
        .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    let target = app
        .build_a11y_tree_update(1.0)
        .nodes
        .into_iter()
        .find(|(_, node)| node.label() == Some("Empty"))
        .expect("exported native page row")
        .0;
    assert!(
        app.dispatch_a11y_action(target, &ui::a11y::Action::Activate)
            .is_none(),
        "row activation uses its semantic owner rather than coordinate fallback"
    );
    assert!(
        app.controlled_hosted_model_for_test(),
        "native accessibility selection dirties the worker"
    );
    let observed = app
        .controlled_native_input_snapshot_for_test()
        .expect("native Notes observer");
    assert_eq!(observed["selected_page"], f.empty.to_string());
    assert_eq!(observed["bound_page"], f.empty.to_string());
    assert_projection_observation(&observed, f.empty);
    assert_eq!(observed["body_chars"], 0);
    assert!(!app.controlled_hosted_model_for_test());
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        before
    );
    app.close_components();
    for report in app.take_component_close_reports() {
        assert_closed(&report);
    }
}

/// The live observer distinguishes the queued route from its synchronized
/// editor without exposing document heads, operations or note contents.
fn assert_projection_observation(observed: &serde_json::Value, page: notes::model::PageId) {
    assert_eq!(observed["main_page"], page.to_string());
    assert_eq!(observed["editor_binding_page"], page.to_string());
    assert_eq!(observed["pending_page_events"], 0);
    assert_eq!(observed["adopted"], true);
    let key = &observed["canonical_sync_key"];
    assert_eq!(key["selected_page"], page.to_string());
    assert_eq!(key["pending_operations"], 0);
    assert!(key["heads_count"].as_u64().unwrap() > 0);
    assert_eq!(key["heads_hash"].as_str().unwrap().len(), 64);
    assert!(key.get("heads").is_none());
}

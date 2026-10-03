use crate::component_support::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use notes::{doc::NotesDoc, notebook};
use ui::{Widget, application::LaunchTarget, widget::TileHost};

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn canonical_component_starts_empty_projects_existing_records_read_only() {
    // Catches accidental starter pages, shared New identities, and legacy-source effects.
    eprintln!("phase new_is_empty_schema_ready_and_instances_have_distinct_records");
    {
        let f = LegacyFixture::rich();
        let source = f.source_bytes();
        let mut a = component(&f, 11, LaunchTarget::New);
        let mut b = component(&f, 12, LaunchTarget::New);
        assert_ne!(a.record().id, b.record().id);
        for checkout in [a.record(), b.record()] {
            f.records
                .read(&checkout.id, |doc| {
                    let workspace = notebook::project(doc, None).unwrap();
                    assert!(workspace.pages().is_empty());
                    assert!(workspace.tags().is_empty());
                })
                .unwrap();
        }
        assert_eq!(source, f.source_bytes());
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
        let f = LegacyFixture::rich();
        let checkout = imported(&f);
        // The accepted importer preserves one blank Markdown paragraph. Exercise
        // the canonical empty-list state after explicitly deleting that block.
        f.records
            .with(&checkout.id, |doc| {
                let block_id = NotesDoc::page_block_snapshots(doc, f.empty)[0].0.clone();
                remove_block(doc, f.empty, &block_id);
                doc.commit();
            })
            .unwrap();
        let source = f.source_bytes();
        let heads = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        let mut a = TileHost::from_component(component(&f, 11, existing(&checkout)));
        let mut b = TileHost::from_component(component(&f, 12, existing(&checkout)));
        let mut outer = AutoCommit::new();
        for _ in 0..3 {
            a.sync(&mut outer, &ROOT);
            b.sync(&mut outer, &ROOT);
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
                assert_eq!(NotesDoc::page_block_snapshots(doc, f.empty).len(), 0);
                assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
            })
            .unwrap();
        assert!(outer.base_heads().is_empty());
        assert_eq!(outer.pending_ops(), 0);
        assert_eq!(source, f.source_bytes());
        close_widget(&mut a);
        close_widget(&mut b);
    }

    // Catches resurrecting an emptied notebook from a still-present legacy workspace.
    eprintln!("phase reopening_an_emptied_import_never_resurrects_legacy_pages");
    {
        let f = LegacyFixture::rich();
        let checkout = imported(&f);
        let source = f.source_bytes();
        let mut live = driver(&f, 11, &checkout);
        let address = ui::bound_dispatch::BoundActionAddress {
            instance_id: 11,
            address: tilo_actions::ActionAddress::new(
                notes::actions::page_resource_id(f.root),
                "delete",
            ),
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
            "delete",
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
        tile.sync(&mut AutoCommit::new(), &ROOT);
        f.records
            .read(&checkout.id, |doc| {
                assert!(notebook::project(doc, None).unwrap().pages().is_empty());
                assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
            })
            .unwrap();
        assert_eq!(source, f.source_bytes());
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
        use ui::widget::AppWidget;
        let mut doc = AutoCommit::new();
        notebook::initialize_empty(&mut doc).unwrap();
        doc.commit();
        let before = (doc.base_heads(), doc.pending_ops());
        let mut app =
            notes::NotesApp::from_document(&doc, &ui::permissions::UserId("same-author".into()))
                .unwrap();
        for (status, label) in [
            (RecordSaveStatus::MemoryOnly, "Session only"),
            (RecordSaveStatus::Saving, "Saving…"),
            (
                RecordSaveStatus::Failed("disk failure".into()),
                "Save failed",
            ),
            (RecordSaveStatus::Saved, "Saved"),
        ] {
            app.record_save_status(status);
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
        let f = LegacyFixture::rich();
        let checkout = imported(&f);
        let mut doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        let before = (doc.base_heads(), doc.pending_ops());
        let mut app = notes::NotesApp::from_document(&doc, &f.author).unwrap();
        assert_eq!(app.selected_for_test(), Some(f.root));
        app.navigate_to(f.child);
        app.sync(&mut doc, &ROOT);
        let expected = NotesDoc::page_block_marks(&doc, f.child)
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
            app.sync(&mut doc, &ROOT);
        }
        assert_eq!(app.main_panel_mut().editor().blocks()[0].id, first);
        assert_eq!(
            app.main_panel_mut().editor().blocks()[0].line.cursor_pos(),
            2
        );
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        for (block, marks) in expected {
            for span in marks {
                let text = NotesDoc::page_block_snapshots(&doc, f.child)
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
        app.sync(&mut doc, &ROOT);
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
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.selected_for_test(), Some(f.empty));
        assert_eq!((doc.base_heads(), doc.pending_ops()), deleted);
        tilo_actions::Action::apply(&notes::actions::delete_page_action(f.empty), &mut doc)
            .unwrap();
        NotesDoc::delete_tag(&mut doc, f.tag);
        doc.commit();
        let emptied = (doc.base_heads(), doc.pending_ops());
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.selected_for_test(), None);
        assert!(app.main_panel_mut().children().is_empty());
        assert!(notebook::project(&doc, None).unwrap().tags().is_empty());
        assert_eq!((doc.base_heads(), doc.pending_ops()), emptied);
    }
}

#![allow(
    clippy::float_cmp,
    reason = "exact retained scroll and unchanged layout geometry are the contract"
)]

use crate::component_support::*;
use automerge::transaction::Transactable;
use notes::{actions, read};
use ui::{
    application::RestoreInput,
    headless::{FrameHarness, SemanticExpectation},
};

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn owned_views_restore_selection_and_clamp_utf8_offsets() {
    // Catches reopening the default page, or projecting old text over the latest record.
    eprintln!("phase owned_close_restores_each_instances_selected_page_from_latest_record");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut a = driver(&f, 11, &checkout);
        let mut b = driver(&f, 12, &checkout);
        open_note(&mut a, 11, f.child, None);
        a.perform_named(
            "note_title",
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 0,
            },
        )
        .unwrap();
        a.expect(&SemanticExpectation::named("note_title").value("Launch"))
            .unwrap();
        a.type_text("uncommitted title draft");
        assert_eq!(revision(&f, &checkout), 0);
        a.close_components();
        let reports = a.take_component_close_reports();
        assert_eq!(reports.len(), 1);
        assert_closed(&reports[0]);
        let saved = reports[0].recovery.clone().unwrap();
        drop(a);
        invoke_with(
            &mut b,
            12,
            actions::page_resource_id(f.child),
            "rename",
            serde_json::json!({ "title": "Latest Launch" }),
        );
        let before = f
            .records
            .read(&checkout.id, |doc| {
                (
                    doc.base_heads(),
                    doc.pending_ops(),
                    read::page_block_snapshots(doc, f.child),
                )
            })
            .unwrap();
        let revision_before = revision(&f, &checkout);
        let component = restored_component(&f, 11, existing(&checkout), RestoreInput::Local(saved));
        let mut a = FrameHarness::from_component(component, (1000, 700), runner_inputs()).unwrap();
        a.expect(
            &SemanticExpectation::named("note_title")
                .value("Latest Launch")
                .focused(true),
        )
        .unwrap();
        b.expect(&SemanticExpectation::named("note_title").value("Projects"))
            .unwrap();
        assert_eq!(
            before,
            f.records
                .read(&checkout.id, |doc| (
                    doc.base_heads(),
                    doc.pending_ops(),
                    read::page_block_snapshots(doc, f.child)
                ))
                .unwrap()
        );
        assert_eq!(revision(&f, &checkout), revision_before);
        close_driver(&mut a);
        close_driver(&mut b);
    }

    eprintln!("phase stable_selection_clamps_all_utf8_offsets_and_survives_focus_and_first_layout");
    {
        use ui::application_view::ApplicationViewState;
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut b = driver(&f, 12, &checkout);
        let block = f
            .records
            .read(&checkout.id, |doc| {
                read::page_block_snapshots(doc, f.child)[0].0.clone()
            })
            .unwrap();
        replace_text_through(&mut b, 12, &f, &checkout, &block, "éλx");
        stamp_current_runs(&f, &checkout, f.child, 0);
        let before = fingerprint(&f, &checkout);
        for (offset, expected) in [(0, 0), (1, 0), (2, 2), (3, 2), (4, 4), (5, 5), (900, 5)] {
            let local = payload(f.child, "Editor", Some(&block), None, offset, 0.0);
            let mut app = app_from_payload(&f, &checkout, &local);
            assert_eq!(
                app.main_panel_mut().editor().blocks()[0].line.cursor_pos(),
                expected
            );
            layout(&mut app, 1000.0, 500.0);
            let editor = app.main_panel_mut().editor();
            assert!(editor.is_focused());
            assert!(editor.blocks()[0].line.is_focused());
            assert_eq!(editor.blocks()[0].line.cursor_pos(), expected);
            assert!(
                editor.blocks()[0]
                    .line
                    .author_runs()
                    .iter()
                    .any(|(_, _, author)| author == &f.author.0)
            );
        }
        for focus in ["None", "Sidebar", "Title", "Editor"] {
            let mut app = app_from_payload(
                &f,
                &checkout,
                &payload(f.child, focus, Some(&block), Some(0), 4, 0.0),
            );
            assert_eq!(
                app.main_panel_mut().editor().selected_text().as_deref(),
                Some("éλ")
            );
            layout(&mut app, 1000.0, 500.0);
            assert_eq!(
                app.main_panel_mut().editor().selected_text().as_deref(),
                Some("éλ")
            );
            let captured = app.capture_application_view().unwrap().unwrap();
            let local: notes::local_view::NotesLocalViewV1 =
                serde_json::from_slice(&captured.payload).unwrap();
            assert_eq!(serde_json::to_value(local.focus).unwrap(), focus);
            assert_eq!(
                app.main_panel_mut().editor().is_focused(),
                focus == "Editor"
            );
            assert_eq!(
                app.main_panel_mut().title_is_editing_for_test(),
                focus == "Title"
            );
        }
        assert_eq!(fingerprint(&f, &checkout), before);
        close_driver(&mut b);
    }
}

fn payload(
    page: notes::PageId,
    focus: &str,
    block: Option<&str>,
    anchor: Option<usize>,
    caret: usize,
    scroll: f64,
) -> ui::application_view::ApplicationLocalView {
    ui::application_view::ApplicationLocalView {
        app_id: notes::definition().app_id.into(), version: 1,
        payload: serde_json::to_vec(&serde_json::json!({
            "selected_page": page, "active_tag": null, "drawer_open": false,
            "expanded_pages": [], "editor_scroll": scroll, "focus": focus,
            "anchor": anchor.map(|byte| serde_json::json!({"block_id": block.unwrap(), "byte_offset": byte})),
            "caret": block.map(|id| serde_json::json!({"block_id": id, "byte_offset": caret})),
        })).unwrap(),
    }
}
fn saved_view(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    local: ui::application_view::ApplicationLocalView,
) -> ui::application_view::SavedInstanceView {
    let mut component = component(f, 11, existing(checkout));
    let report = component.close();
    assert_closed(&report);
    let mut saved = report.recovery.unwrap();
    saved.application = Some(local);
    // This uncorrelated title ordinal/opaque state must never overwrite stable editor focus.
    saved.view.focus = Some(ui::view::FocusView {
        ordinal: 1,
        ephemeral: "stale title draft".into(),
    });
    saved
}
fn fingerprint(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
) -> (Vec<automerge::ChangeHash>, usize, Vec<u8>, u64) {
    let (heads, pending, bytes) = f
        .records
        .read(&checkout.id, |doc| {
            (doc.base_heads(), doc.pending_ops(), doc.clone().save())
        })
        .unwrap();
    (heads, pending, bytes, revision(f, checkout))
}
fn stamp_current_runs(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    page: notes::PageId,
    index: usize,
) {
    // Explicit canonical fixture content: bound replace-text deliberately does not
    // synthesize authorship/formatting. Restoration must adopt these existing marks.
    f.records
        .with(&checkout.id, |doc| {
            let snapshots = read::page_block_snapshots(doc, page);
            let body = &snapshots[index].1;
            apply_to(
                doc,
                &actions::set_mark_action(
                    page,
                    &snapshots[index].0,
                    0..body.chars().count(),
                    &ui::widget::marks::Mark::Bold,
                ),
            )
            .unwrap();
            edit_body(
                doc,
                page,
                tilo_document::RichTextAction::SetMark {
                    region: tilo_document::action::RegionKind::Body,
                    spans: vec![tilo_document::action::Span {
                        block: snapshots[index].0.clone(),
                        start: 0,
                        end: body.chars().count(),
                    }],
                    name: tilo_doc::schema::MARK_AUTHOR.to_owned(),
                    value: Some(tilo_document::action::MarkVal::Str(f.author.0.clone())),
                    also_clear: Vec::new(),
                },
            );
            doc.commit();
        })
        .unwrap();
}
fn app_from_payload(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    local: &ui::application_view::ApplicationLocalView,
) -> notes::NotesApp {
    use ui::application_view::ApplicationViewState;
    f.records
        .read(&checkout.id, |doc| {
            let mut app = notes::NotesApp::from_document(doc, &f.author).unwrap();
            assert_eq!(
                app.restore_application_view(local, doc).unwrap(),
                ui::application_view::ApplicationFocusRestore::Application
            );
            app.finish_application_view_restore().unwrap();
            app
        })
        .unwrap()
}
// Real layout/measurement without a mutable document sync: restoration must be clean
// before a host has permission to mount it or deliver events.
fn layout(
    app: &mut notes::NotesApp,
    width: f32,
    height: f32,
) -> (taffy::TaffyTree<()>, taffy::NodeId) {
    use ui::Widget;
    let mut renderer = renderer();
    let theme = ui::Theme::dark();
    let mut laid_out = None;
    for _ in 0..5 {
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let node = app.build_layout(&mut tree, &theme, &mut nodes);
        let root = tree
            .new_with_children(
                taffy::Style {
                    size: taffy::Size::from_lengths(width, height),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        tree.compute_layout_with_measure(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
            |known, available, node, _, _| {
                nodes.get(&node).map_or(taffy::Size::ZERO, |ptr| {
                    // SAFETY: pointers belong to this exclusively borrowed app and exact tree.
                    unsafe { (&mut **ptr).measure(known, available, &mut renderer) }
                })
            },
        )
        .unwrap();
        app.apply_layout(&tree, node, vello::kurbo::Point::ORIGIN);
        laid_out = Some((tree, node));
        if !app.take_structure_dirty() {
            break;
        }
    }
    laid_out.expect("at least one layout pass produces a tree")
}

#[test]
fn first_note_pointer_activation_needs_layout_but_no_paint() {
    use ui::Widget;
    use ui::synthetic_input::{cursor_moved, mouse_press, mouse_release};

    ui::i18n::I18nManager::init("en-US");
    let mut host = ui::framework::testing::TestApp::<notes::NotesShell>::new().unwrap();
    let mut doc = host.peer_document();
    let app = host.surface_mut::<notes::NotesApp>();
    // Discard any geometry the host's initial frame painted. From here through
    // pointer activation only projection, layout and event delivery run.
    app.main_panel_mut().clear();
    assert!(read::list_pages(&doc).is_empty());
    let _ = layout(app, 1000.0, 500.0);
    let first_center = app
        .main_panel_mut()
        .empty_action_center_for_test()
        .expect("initial layout exposes the first-note action before painting");
    let (tree, node) = layout(app, 1200.0, 700.0);
    let center = app
        .main_panel_mut()
        .empty_action_center_for_test()
        .expect("resizing preserves the unpainted first-note action");
    assert_ne!(
        center, first_center,
        "layout updates the hit target on resize"
    );
    let mut focus = None;
    for event in [
        cursor_moved(center.x, center.y),
        mouse_press(),
        mouse_release(),
    ] {
        let mut handled = false;
        app.handle_event(
            &event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &ui::winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &tree,
            },
        );
        if matches!(
            event,
            ui::winit::event::WindowEvent::MouseInput {
                state: ui::winit::event::ElementState::Pressed,
                ..
            }
        ) {
            assert!(handled, "the layout-only action consumes its pointer press");
        }
    }
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(
        read::list_pages(&doc).len(),
        1,
        "one click creates exactly one note"
    );
    assert_eq!(app.workspace_pages_for_test().len(), 1);
    assert!(app.selected_for_test().is_some());
    layout(app, 1200.0, 700.0);
    assert!(
        app.main_panel_mut()
            .empty_action_center_for_test()
            .is_none()
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one complete owned close/sibling edit/reopen/first edit scenario"
)]
fn stale_local_view_first_real_edit_uses_current_text_marks_and_one_owned_action() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let block = f
        .records
        .read(&checkout.id, |doc| {
            read::page_block_snapshots(doc, f.child)[3].0.clone()
        })
        .unwrap();
    let mut a = driver(&f, 11, &checkout);
    let mut b = driver(&f, 12, &checkout);
    open_note(&mut a, 11, f.child, Some(&block));
    a.perform_named("block_editor", &ui::a11y::Action::Focus)
        .unwrap();
    a.key("Home");
    a.set_modifiers(&["shift".into()]);
    a.key("ArrowRight");
    a.set_modifiers(&[]);
    a.close_components();
    let reports = a.take_component_close_reports();
    assert_eq!(reports.len(), 1);
    assert_closed(&reports[0]);
    let saved = reports[0].recovery.clone().unwrap();
    let selected: notes::local_view::NotesLocalViewV1 =
        serde_json::from_slice(&saved.application.as_ref().unwrap().payload).unwrap();
    assert_eq!(selected.anchor.as_ref().unwrap().byte_offset, 0);
    assert_eq!(selected.caret.as_ref().unwrap().byte_offset, 2);
    assert_eq!(selected.caret.as_ref().unwrap().block_id, block);
    drop(a);
    replace_text_through(&mut b, 12, &f, &checkout, &block, "é current");
    // The current native formatting/author projection must survive immutable adoption.
    stamp_current_runs(&f, &checkout, f.child, 3);
    let before = fingerprint(&f, &checkout);
    let projected = app_from_payload(&f, &checkout, saved.application.as_ref().unwrap());
    let mut projected = projected;
    let current_marks = projected.main_panel_mut().inline_marks();
    assert!(
        projected.main_panel_mut().editor().blocks()[3]
            .line
            .author_runs()
            .iter()
            .any(|(_, _, author)| author == &f.author.0)
    );
    let component = restored_component(&f, 11, existing(&checkout), RestoreInput::Local(saved));
    let mut a = FrameHarness::from_component(component, (1000, 500), runner_inputs()).unwrap();
    a.expect(&SemanticExpectation::named("block_editor").focused(true))
        .unwrap();
    assert_eq!(fingerprint(&f, &checkout), before);
    a.type_text("Q");
    // Closing ends the typed word's step.
    close_driver(&mut a);
    assert_eq!(revision(&f, &checkout), before.3 + 1);
    f.records
        .read_history(&checkout.id, |doc, history| {
            assert_eq!(read::page_block_snapshots(doc, f.child)[3].1, "Q current");
            let last = history.next_undo_in(doc, &f.author, 11).unwrap();
            assert_eq!(last.author.as_ref(), Some(&f.author));
            assert_eq!(last.instance_id, Some(11));
            notes::read::validate(doc).unwrap();
            let mut snapshot = doc.clone();
            let changes = snapshot.get_changes(&before.0);
            assert_eq!(
                changes.len(),
                1,
                "the owned edit records who wrote it in the same change"
            );
            // History names the note's body and the edits it took.
            let labels = changes
                .iter()
                .map(|change| {
                    let context =
                        tilo_actions::ActionContext::from_change_message(change.message().unwrap())
                            .unwrap();
                    assert_eq!(
                        context.resource.unwrap().id,
                        actions::page_resource_id(f.child)
                    );
                    match context.what {
                        Some(tilo_actions::Did::Edit(edit)) => edit.field,
                        other => panic!("a field edit: {other:?}"),
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(labels, ["body"]);
        })
        .unwrap();
    for _ in 0..3 {
        b.relayout();
    }
    assert_eq!(revision(&f, &checkout), before.3 + 1);
    // Retained current marks are compared against canonical native spans, not old View bytes.
    assert_eq!(
        current_marks.iter().find(|(id, _)| id == &block).unwrap().1,
        vec![actions::InlineMarkSpan {
            start: 0,
            end: 10,
            mark: actions::InlineMark::Bold
        }]
    );
    f.records
        .read(&checkout.id, |doc| {
            assert_eq!(
                read::page_block_marks(doc, f.child)
                    .iter()
                    .find(|(id, _)| id == &block)
                    .unwrap()
                    .1,
                vec![actions::InlineMarkSpan {
                    start: 1,
                    end: 9,
                    mark: actions::InlineMark::Bold
                }]
            );
        })
        .unwrap();
    close_driver(&mut b);
}

#[test]
fn independent_scroll_close_before_layout_and_narrow_sidebar_resolve_once() {
    use ui::application_view::ApplicationViewState;
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let block = f
        .records
        .read(&checkout.id, |doc| {
            read::page_block_snapshots(doc, f.child)[0].0.clone()
        })
        .unwrap();
    let requested = payload(f.child, "Sidebar", Some(&block), Some(0), 2, 700.0);
    let saved = saved_view(&f, &checkout, requested);
    let before = fingerprint(&f, &checkout);
    let mut unmounted = restored_component(
        &f,
        11,
        existing(&checkout),
        RestoreInput::Local(saved.clone()),
    );
    let report = unmounted.close();
    assert_closed(&report);
    let captured: notes::local_view::NotesLocalViewV1 =
        serde_json::from_slice(&report.recovery.unwrap().application.unwrap().payload).unwrap();
    assert_eq!(
        captured.editor_scroll, 700.0,
        "close before measure retains requested scroll"
    );
    assert_eq!(captured.focus, notes::local_view::NotesFocus::Sidebar);
    let mut app = app_from_payload(&f, &checkout, saved.application.as_ref().unwrap());
    layout(&mut app, 390.0, 220.0);
    assert!(!app.drawer_open_for_test());
    let local: notes::local_view::NotesLocalViewV1 =
        serde_json::from_slice(&app.capture_application_view().unwrap().unwrap().payload).unwrap();
    assert_eq!(local.focus, notes::local_view::NotesFocus::None);
    assert_eq!(
        app.main_panel_mut().editor().selected_text().as_deref(),
        Some("He")
    );
    assert!(app.main_panel_mut().scroll_y_for_test() > 0.0);
    assert!(app.main_panel_mut().scroll_y_for_test() <= 700.0);
    // Moving to wide mode does not reapply the consumed Sidebar intent.
    layout(&mut app, 1000.0, 500.0);
    let local: notes::local_view::NotesLocalViewV1 =
        serde_json::from_slice(&app.capture_application_view().unwrap().unwrap().payload).unwrap();
    assert_eq!(local.focus, notes::local_view::NotesFocus::None);
    let mut a = FrameHarness::from_component(
        restored_component(&f, 11, existing(&checkout), RestoreInput::Local(saved)),
        (1000, 250),
        runner_inputs(),
    )
    .unwrap();
    let mut b = driver(&f, 12, &checkout);
    let a_before = semantic_rect(&a, &format!("notes.block:{block}")).y0;
    let index_block = f
        .records
        .read(&checkout.id, |doc| {
            read::page_block_snapshots(doc, f.root)[0].0.clone()
        })
        .unwrap();
    let b_before = semantic_rect(&b, &format!("notes.block:{index_block}")).y0;
    move_pointer(&mut a, vello::kurbo::Point::new(700.0, 150.0));
    a.scroll(0.0, 4.0);
    a.relayout();
    assert!(semantic_rect(&a, &format!("notes.block:{block}")).y0 > a_before);
    assert_eq!(
        semantic_rect(&b, &format!("notes.block:{index_block}")).y0,
        b_before
    );
    assert_eq!(fingerprint(&f, &checkout), before);
    close_driver(&mut a);
    close_driver(&mut b);
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn stale_or_malformed_view_payloads_fall_back_without_installing_state() {
    eprintln!("phase stale_page_block_and_tag_fall_back_and_empty_page_has_no_stable_caret");
    {
        use ui::application_view::ApplicationViewState;
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let block = f
            .records
            .read(&checkout.id, |doc| {
                read::page_block_snapshots(doc, f.child)[0].0.clone()
            })
            .unwrap();
        let mut local = payload(f.child, "Editor", Some(&block), Some(1), 8, 40.0);
        let mut value: serde_json::Value = serde_json::from_slice(&local.payload).unwrap();
        value["active_tag"] = serde_json::json!(f.tag);
        value["expanded_pages"] = serde_json::json!([f.root, f.root, uuid::Uuid::from_u128(999)]);
        local.payload = serde_json::to_vec(&value).unwrap();
        let saved = saved_view(&f, &checkout, local);
        let mut b = driver(&f, 12, &checkout);
        invoke_confirmed(&mut b, 12, actions::page_resource_id(f.child), "delete");
        invoke_confirmed(&mut b, 12, actions::tag_resource_id(f.tag), "delete-tag");
        let before = fingerprint(&f, &checkout);
        let mut app = app_from_payload(&f, &checkout, saved.application.as_ref().unwrap());
        assert_eq!(app.selected_for_test(), Some(f.root));
        assert_eq!(
            app.main_panel_mut().editor().blocks()[0].line.cursor_pos(),
            0
        );
        assert!(app.main_panel_mut().editor().selected_text().is_none());
        let local: notes::local_view::NotesLocalViewV1 =
            serde_json::from_slice(&app.capture_application_view().unwrap().unwrap().payload)
                .unwrap();
        assert!(local.active_tag.is_none());
        assert_eq!(local.expanded_pages, vec![f.root]);
        let component = restored_component(&f, 11, existing(&checkout), RestoreInput::Local(saved));
        let mut a = FrameHarness::from_component(component, (1000, 500), runner_inputs()).unwrap();
        a.expect(&SemanticExpectation::named("note_title").value("Projects"))
            .unwrap();
        assert_eq!(fingerprint(&f, &checkout), before);
        close_driver(&mut a);
        // Build the empty canonical fixture directly; a View never seeds its content.
        let empty_block = f
            .records
            .read(&checkout.id, |doc| {
                read::page_block_snapshots(doc, f.empty)[0].0.clone()
            })
            .unwrap();
        f.records
            .with(&checkout.id, |doc| {
                remove_block(doc, f.empty, &empty_block);
                doc.commit();
            })
            .unwrap();
        let before = fingerprint(&f, &checkout);
        let mut empty = app_from_payload(
            &f,
            &checkout,
            &payload(f.empty, "Editor", Some(&block), Some(0), 99, 80.0),
        );
        layout(&mut empty, 1000.0, 500.0);
        let local: notes::local_view::NotesLocalViewV1 =
            serde_json::from_slice(&empty.capture_application_view().unwrap().unwrap().payload)
                .unwrap();
        assert!(local.anchor.is_none() && local.caret.is_none());
        assert_eq!(empty.main_panel_mut().scroll_y_for_test(), 0.0);
        assert_eq!(fingerprint(&f, &checkout), before);
        let empty_saved = saved_view(
            &f,
            &checkout,
            payload(f.empty, "Editor", Some(&block), Some(0), 99, 80.0),
        );
        let component = restored_component(
            &f,
            11,
            existing(&checkout),
            RestoreInput::Local(empty_saved),
        );
        let mut a = FrameHarness::from_component(component, (1000, 500), runner_inputs()).unwrap();
        a.type_text("f");
        // Closing ends the typed word's step.
        close_driver(&mut a);
        assert_eq!(revision(&f, &checkout), before.3 + 1);
        f.records
            .read(&checkout.id, |doc| {
                assert_eq!(
                    read::page_block_snapshots(doc, f.empty)
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["f"]
                );
            })
            .unwrap();
        close_driver(&mut b);
    }

    eprintln!("phase strict_payload_errors_retain_setup_owner_and_original_bytes");
    {
        use ui::launch::LaunchError;
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let block = f
            .records
            .read(&checkout.id, |doc| {
                read::page_block_snapshots(doc, f.child)[0].0.clone()
            })
            .unwrap();
        let saved = saved_view(
            &f,
            &checkout,
            payload(f.child, "Editor", Some(&block), None, 1, 0.0),
        );
        let views = ui::application_view::InstanceViewStore::new(
            f.state_root.clone(),
            "fixture-device",
            canonical::ROOM,
        )
        .unwrap();
        views.save(&saved).unwrap();
        let path = f.state_root.join(
            "instance-views/666978747572652d646576696365/666978747572652d726f6f6d/11.postcard",
        );
        let original = std::fs::read(&path).unwrap();
        let before = fingerprint(&f, &checkout);
        let good: serde_json::Value =
            serde_json::from_slice(&saved.application.as_ref().unwrap().payload).unwrap();
        let mut cases = Vec::new();
        for (key, bad) in [
            ("editor_scroll", serde_json::json!(-1)),
            ("drawer_open", serde_json::json!("yes")),
            ("focus", serde_json::json!("Unknown")),
            (
                "selected_page",
                serde_json::json!(f.child.simple().to_string()),
            ),
            ("active_tag", serde_json::json!(uuid::Uuid::nil())),
            ("expanded_pages", serde_json::json!(["invalid"])),
            ("unknown", serde_json::json!(true)),
        ] {
            let mut value = good.clone();
            value[key] = bad;
            cases.push(serde_json::to_vec(&value).unwrap());
        }
        for value in [
            serde_json::json!({"block_id": "", "byte_offset": 0}),
            serde_json::json!({"block_id": block, "byte_offset": -1}),
            serde_json::json!({"block_id": block, "byte_offset": 0, "text": "forbidden"}),
        ] {
            let mut bad = good.clone();
            bad["caret"] = value;
            cases.push(serde_json::to_vec(&bad).unwrap());
        }
        let mut anchor_only = good.clone();
        anchor_only["anchor"] = good["caret"].clone();
        anchor_only["caret"] = serde_json::Value::Null;
        cases.push(serde_json::to_vec(&anchor_only).unwrap());
        cases.push(b"{malformed".to_vec());
        cases.push(
            String::from_utf8(serde_json::to_vec(&good).unwrap())
                .unwrap()
                .replace("\"editor_scroll\":0.0", "\"editor_scroll\":1e999")
                .into_bytes(),
        );
        let mut envelopes = cases
            .into_iter()
            .map(|bytes| {
                let mut bad = saved.clone();
                bad.application.as_mut().unwrap().payload = bytes;
                bad
            })
            .collect::<Vec<_>>();
        let mut unknown = saved.clone();
        unknown.application.as_mut().unwrap().version = 99;
        envelopes.push(unknown);
        let mut unknown = saved.clone();
        unknown.application.as_mut().unwrap().app_id = "other.app".into();
        envelopes.push(unknown);
        let mut foreign = saved;
        foreign.record_id = "foreign-record".into();
        envelopes.push(foreign);
        for bad in envelopes {
            let error =
                try_restored_component(&f, 11, existing(&checkout), RestoreInput::Local(bad))
                    .err()
                    .expect("malformed payload was accepted");
            assert!(matches!(error, LaunchError::ComponentSetup { .. }));
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(fingerprint(&f, &checkout), before);
        }
    }

    eprintln!("phase block_identity_is_preserved_and_shape_errors_install_nothing");
    {
        use automerge::{ReadDoc, transaction::Transactable};
        use ui::application_view::ApplicationViewState;
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let (spelling, page_map) = f
            .records
            .with(&checkout.id, |doc| {
                let (_, pages) = doc.get(automerge::ROOT, "pages").unwrap().unwrap();
                let (_, item) = doc.get(&pages, f.root.to_string()).unwrap().unwrap();
                // A collection keeps each item's fields under `v`.
                let (_, page) = doc.get(&item, "v").unwrap().unwrap();
                (read::page_block_snapshots(doc, f.root)[0].0.clone(), page)
            })
            .unwrap();
        let spelling = spelling.as_str();
        let local = payload(f.root, "Editor", Some(spelling), None, 1, 0.0);
        let mut app = app_from_payload(&f, &checkout, &local);
        assert_eq!(app.main_panel_mut().editor().blocks()[0].id, spelling);
        let captured: notes::local_view::NotesLocalViewV1 =
            serde_json::from_slice(&app.capture_application_view().unwrap().unwrap().payload)
                .unwrap();
        assert_eq!(captured.caret.unwrap().block_id, spelling);
        let saved = saved_view(&f, &checkout, local);
        let component = restored_component(
            &f,
            11,
            existing(&checkout),
            RestoreInput::Local(saved.clone()),
        );
        let mut a = FrameHarness::from_component(component, (1000, 500), runner_inputs()).unwrap();
        a.type_text("Q");
        f.records
            .read(&checkout.id, |doc| {
                assert_eq!(
                    read::page_block_snapshots(doc, f.root)[0],
                    (spelling.into(), "PQroject index".into())
                );
            })
            .unwrap();
        close_driver(&mut a);
        f.records
            .with(&checkout.id, |doc| {
                // A body that is no rich text: a peer's damaged write.
                ui::tilo_resource::Slot::Key(&page_map, "body")
                    .put_scalar(doc, automerge::ScalarValue::Int(42))
                    .unwrap();
                doc.commit();
            })
            .unwrap();
        let before = fingerprint(&f, &checkout);
        let result = f
            .records
            .read(&checkout.id, |doc| {
                app.restore_application_view(saved.application.as_ref().unwrap(), doc)
            })
            .unwrap();
        assert!(result.is_err());
        assert_eq!(
            app.main_panel_mut().editor().blocks()[0].text(),
            "Project index"
        );
        assert_eq!(
            app.main_panel_mut().editor().blocks()[0].line.cursor_pos(),
            1
        );
        let error = try_restored_component(&f, 11, existing(&checkout), RestoreInput::Local(saved))
            .err()
            .unwrap();
        assert!(matches!(
            error,
            ui::launch::LaunchError::ComponentSetup { .. }
        ));
        assert_eq!(fingerprint(&f, &checkout), before);
    }
}

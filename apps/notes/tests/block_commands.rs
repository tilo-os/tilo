//! Whole-block commands through the mounted Notes body and canonical history.
use crate::component_support::*;
use notes::read;
use ui::widget::BlockContent;

fn duplicate(source: &str, target: usize, group: std::ops::Range<usize>, keyboard: bool) {
    let fixture = NotebookFixture::rich();
    fixture.write_page(fixture.child, source.as_bytes());
    let checkout = notebook(&fixture);
    let before = fixture
        .records
        .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
        .unwrap();
    let mut live = driver(&fixture, 97, &checkout);
    open_note(&mut live, 97, fixture.child, Some(&before[target].id));
    let rect = semantic_rect(&live, &format!("notes.block:{}", before[target].id));
    live.click_at(rect.x0 + 8.0, rect.center().y);
    let before_focus = before;
    let before = fixture
        .records
        .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
        .unwrap();
    let mut expected_focus = before_focus;
    if matches!(
        expected_focus[target].kind,
        ui::widget::BlockKind::Table { .. }
    ) {
        expected_focus[target].kind = ui::widget::BlockKind::Table { visual: false };
    }
    assert_eq!(
        before, expected_focus,
        "focusing source only changes the table presentation; content and identities are preserved"
    );
    let revision_before = revision(&fixture, &checkout);
    if keyboard {
        live.set_modifiers(&["ctrl".into()]);
        live.key("d");
        live.set_modifiers(&[]);
    } else {
        live.perform_semantic_named(
            "block_editor",
            Some(&format!("notes.block-duplicate:{}", before[target].id)),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    }
    let after = fixture
        .records
        .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
        .unwrap();
    assert_eq!(
        revision(&fixture, &checkout),
        revision_before.checked_add(1).unwrap(),
        "one block command is one history action"
    );
    assert_copied_blocks(&before, &after, group);
    live.set_modifiers(&["ctrl".into()]);
    live.key("z");
    live.set_modifiers(&[]);
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
            .unwrap(),
        before
    );
    live.set_modifiers(&["ctrl".into(), "shift".into()]);
    live.key("z");
    live.set_modifiers(&[]);
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
            .unwrap(),
        after
    );
    close_driver(&mut live);
}

fn assert_markdown_roundtrip(blocks: &[BlockContent], copied_fence: bool) {
    let mut editor = ui::widget::BlockEditor::new();
    editor.present_blocks(blocks);
    let specs = notes::markdown::blocks_to_specs(editor.blocks());
    if copied_fence {
        assert_eq!(
            specs
                .iter()
                .filter(|spec| matches!(spec, notes::markdown::BlockSpec::CodeBlock { .. }))
                .count(),
            2,
            "the original and duplicate export as distinct authored fences"
        );
    }
    let source = notes::markdown::serialize(&specs);
    assert_eq!(notes::markdown::parse(&source), specs, "{source}");
}

fn assert_copied_blocks(
    before: &[BlockContent],
    after: &[BlockContent],
    group: std::ops::Range<usize>,
) {
    let copy_end = group.end.checked_add(group.len()).unwrap();
    assert_eq!(after.len(), before.len().checked_add(group.len()).unwrap());
    assert_eq!(&after[..group.end], &before[..group.end]);
    assert_eq!(&after[copy_end..], &before[group.end..]);
    let copies = &after[group.end..copy_end];
    for (original, copy) in before[group].iter().zip(copies) {
        assert!(!before.iter().any(|block| block.id == copy.id));
        let mut expected: BlockContent = original.clone();
        expected.id.clone_from(&copy.id);
        if original.code_group.is_some() {
            assert_ne!(
                original.code_group, copy.code_group,
                "the copied fence has its own identity"
            );
            expected.code_group.clone_from(&copy.code_group);
        }
        assert_eq!(
            &expected, copy,
            "all authored text, nesting and inline marks survive duplication"
        );
    }
    if copies.iter().all(|block| block.code_group.is_some()) {
        assert!(
            copies
                .windows(2)
                .all(|pair| pair[0].code_group == pair[1].code_group)
        );
    }
    assert_markdown_roundtrip(after, copies.iter().all(|block| block.code_group.is_some()));
}

#[test]
fn duplicate_prose_copies_the_subtree_and_unicode_marks_in_one_undo_step() {
    duplicate("**Parent**\n\n  *é😀 child*\n\nAfter\n", 0, 0..2, true);
}

#[test]
fn duplicate_code_from_a_source_row_copies_one_distinct_fence() {
    duplicate(
        "```rust\nlet first = 1;\nlet second = 2;\n```\n\nAfter\n",
        1,
        0..2,
        true,
    );
}

#[test]
fn semantic_duplicate_uses_the_explicit_block_id_and_subtree() {
    duplicate(
        "Before\n\n- **Parent**\n  - *Child*\n\nAfter\n",
        1,
        1..3,
        false,
    );
}

fn duplicate_selection_then_type(semantic: bool) {
    let fixture = NotebookFixture::rich();
    fixture.write_page(
        fixture.child,
        b"Parent\n\nFirst child\n\nSecond child\n\nAfter\n",
    );
    let checkout = notebook(&fixture);
    let before = fixture
        .records
        .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
        .unwrap();
    let mut live = driver(&fixture, 97, &checkout);
    open_note(&mut live, 97, fixture.child, Some(&before[1].id));
    live.perform_named(
        "block_editor",
        &ui::a11y::Action::SetSelection {
            anchor: 7,
            focus: 31,
        },
    )
    .unwrap();
    if semantic {
        live.perform_semantic_named(
            "block_editor",
            Some("notes.editor:duplicate"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    } else {
        live.set_modifiers(&["ctrl".into()]);
        live.key("d");
        live.set_modifiers(&[]);
    }
    live.type_text("!");
    let after = fixture
        .records
        .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
        .unwrap();
    assert_eq!(
        &after[..3],
        &before[..3],
        "typing must never replace the original selection"
    );
    assert_eq!(after.len(), 6);
    assert_eq!(after[4].text, "Second child!");
    close_driver(&mut live);
}

#[test]
fn duplicate_table_keeps_its_editable_source_and_shape() {
    duplicate(
        "| First | Second |\n| --- | --- |\n| é | 😀 |\n\nAfter\n",
        0,
        0..1,
        true,
    );
}

#[test]
fn duplicate_refuses_viewers_and_composition_then_converges_with_peer_text() {
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Invoke;
    let mut app = TestApp::<notes::NotesShell>::new().unwrap();
    let page = notes::PageId::new_v4();
    app.invoke(Invoke::apply(&notes::actions::create_page_action(page, None, "Copy")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("Original é😀");
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("d");
    app.harness().set_modifiers(&[]);
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some(&format!("notes.block-duplicate:{}", before[0].id)),
                &ui::a11y::Action::Activate,
            )
            .is_err(),
        "IME owns structural commands from the accessibility door too"
    );
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit(String::new(), None),
    ));
    app.set_role(ui::permissions::Role::Viewer);
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("d");
    app.harness().set_modifiers(&[]);
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    app.set_role(ui::permissions::Role::Editor);
    let mut peer = app.peer_document();
    crate::component_support::edit_page_body(
        &mut peer,
        page,
        &tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block: before[0].id.clone(),
            at: 0,
            del: 0,
            ins: "Peer ".into(),
        },
    )
    .unwrap();
    peer.commit();
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("d");
    app.harness().set_modifiers(&[]);
    app.merge_peer(&mut peer).unwrap();
    let copied = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(copied.len(), 2);
    assert_eq!(copied[0].text, "Peer Original é😀");
    assert_eq!(copied[1].text, "Original é😀");
    assert_ne!(copied[0].id, copied[1].id);
    assert_eq!(copied, read::page_blocks(&peer, page));
    assert!(app.undo());
    let undone = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(undone.len(), 1);
    assert_eq!(undone[0].text, "Peer Original é😀");
}

#[test]
fn typing_after_duplicating_a_selection_edits_a_copy_and_keeps_the_originals() {
    duplicate_selection_then_type(false);
}

#[test]
fn semantic_duplicate_selection_uses_the_same_safe_batch_command() {
    duplicate_selection_then_type(true);
}

#[test]
fn visual_table_composition_keeps_keyboard_and_semantic_copy_out_of_the_cell() {
    let fixture = NotebookFixture::rich();
    fixture.write_page(
        fixture.child,
        b"| Name | Value |\n| --- | --- |\n| first | second |\n",
    );
    let checkout = notebook(&fixture);
    let before = fixture
        .records
        .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
        .unwrap();
    let mut live = driver(&fixture, 97, &checkout);
    open_note(&mut live, 97, fixture.child, Some(&before[0].id));
    let id = &before[0].id;
    live.perform_semantic_named(
        "block_editor",
        Some(&format!("notes.table-cell:{id}:1:0")),
        &ui::a11y::Action::Focus,
    )
    .unwrap();
    live.send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    live.set_modifiers(&["ctrl".into()]);
    live.key("d");
    live.set_modifiers(&[]);
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
            .unwrap(),
        before
    );
    for target in [
        format!("notes.block-duplicate:{id}"),
        "notes.editor:duplicate".into(),
    ] {
        assert!(
            live.perform_semantic_named("block_editor", Some(&target), &ui::a11y::Action::Activate)
                .is_err()
        );
    }
    live.send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Commit("日本".into()),
    ));
    let after = fixture
        .records
        .read(&checkout.id, |doc| read::page_blocks(doc, fixture.child))
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, *id);
    assert!(after[0].text.contains("日本"));
    close_driver(&mut live);
}

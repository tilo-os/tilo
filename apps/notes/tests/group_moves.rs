use crate::component_support::*;
use automerge::AutoCommit;
use notes::{actions, read};
use tilo_automerge::ApplyIncoming;
use tilo_document::RichTextAction;
use tilo_document::action::RegionKind;

type Rows = Vec<(String, String, String, String, u64)>;

fn move_selection(live: &mut ui::headless::Driver, up: bool, semantic: bool) {
    if semantic {
        live.perform_semantic_named(
            "block_editor",
            Some(if up {
                "notes.editor:move-up"
            } else {
                "notes.editor:move-down"
            }),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    } else {
        live.set_modifiers(&["alt".into()]);
        live.key(if up { "ArrowUp" } else { "ArrowDown" });
        live.set_modifiers(&[]);
    }
}

#[test]
fn moving_a_table_during_cell_composition_is_refused_without_losing_the_commit() {
    let f = NotebookFixture::rich();
    f.write_page(
        f.child,
        b"| Name | Value |\n| --- | --- |\n| first | second |\n\nAfter\n",
    );
    let checkout = notebook(&f);
    let before = f
        .records
        .read(&checkout.id, |doc| snapshot(doc, f.child))
        .unwrap();
    let mut live = driver(&f, 88, &checkout);
    open_note(&mut live, 88, f.child, Some(&before[0].0));
    live.perform_semantic_named(
        "block_editor",
        Some(&format!("notes.table-cell:{}:1:0", before[0].0)),
        &ui::a11y::Action::Focus,
    )
    .unwrap();
    live.send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    move_selection(&mut live, false, false);
    assert!(
        live.perform_semantic_named(
            "block_editor",
            Some("notes.editor:move-down"),
            &ui::a11y::Action::Activate
        )
        .is_err()
    );
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap(),
        before
    );
    live.send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Commit("日本".into()),
    ));
    let after = f
        .records
        .read(&checkout.id, |doc| snapshot(doc, f.child))
        .unwrap();
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].0, before[0].0);
    assert!(after[0].1.contains("日本"));
    assert_eq!(after[1], before[1]);
    close_driver(&mut live);
}

#[test]
fn empty_selected_root_stays_in_the_batch_at_the_end_of_the_document() {
    let f = NotebookFixture::rich();
    f.write_page(f.child, b"- A\n- \n- C\n");
    let checkout = notebook(&f);
    let before = f
        .records
        .read(&checkout.id, |doc| snapshot(doc, f.child))
        .unwrap();
    assert_eq!(before.len(), 3);
    assert!(before[1].1.is_empty());
    let mut live = driver(&f, 88, &checkout);
    open_note(&mut live, 88, f.child, Some(&before[0].0));
    live.perform_named(
        "block_editor",
        &ui::a11y::Action::SetSelection {
            anchor: 0,
            focus: 3,
        },
    )
    .unwrap();
    move_selection(&mut live, false, true);
    let expected: Rows = [2, 0, 1].iter().map(|&i| before[i].clone()).collect();
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap(),
        expected
    );
    move_selection(&mut live, true, true);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap(),
        before
    );
    live.key("Escape");
    move_selection(&mut live, false, false);
    let focused_only: Rows = [0, 2, 1].iter().map(|&i| before[i].clone()).collect();
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap(),
        focused_only
    );
    close_driver(&mut live);
}

fn movement_app() -> (
    ui::framework::testing::TestApp<notes::NotesShell>,
    notes::PageId,
) {
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Invoke;
    let mut app = TestApp::<notes::NotesShell>::new().unwrap();
    let page = notes::PageId::new_v4();
    app.invoke(Invoke::apply(&actions::create_page_action(page, None, "Move")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    for text in ["A", "B"] {
        app.harness().type_text(text);
        app.harness().key("Enter");
    }
    app.harness().type_text("C");
    (app, page)
}

#[test]
fn batch_movement_refuses_composition_and_viewers_and_preserves_peer_text_on_undo() {
    let (mut app, page) = movement_app();
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 4,
            },
        )
        .unwrap();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(before.len(), 3);
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    move_selection(app.harness(), false, false);
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.editor:move-down"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit(String::new(), None),
    ));
    app.set_role(ui::permissions::Role::Viewer);
    move_selection(app.harness(), false, false);
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.editor:move-down"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    app.set_role(ui::permissions::Role::Editor);
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 4,
            },
        )
        .unwrap();
    let mut peer = app.peer_document();
    edit(
        &mut peer,
        page,
        RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: before[0].id.clone(),
            at: 0,
            del: 0,
            ins: "Peer ".into(),
        },
    );
    peer.commit();
    move_selection(app.harness(), false, true);
    app.merge_peer(&mut peer).unwrap();
    let moved = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        moved
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["C", "Peer A", "B"]
    );
    assert_eq!(moved, read::page_blocks(&peer, page));
    assert!(app.undo());
    let undone = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        undone
            .iter()
            .map(|block| block.id.as_str())
            .collect::<Vec<_>>(),
        before
            .iter()
            .map(|block| block.id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        undone
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["Peer A", "B", "C"]
    );
}

#[test]
fn selected_sibling_subtrees_move_atomically_and_keep_the_batch_selected() {
    for (semantic, reverse) in [(false, false), (true, true)] {
        let f = NotebookFixture::rich();
        f.write_page(
            f.child,
            "- **Ä parent**\n  - child α\n- *B parent*\n  - child β\n- C\n- D\n".as_bytes(),
        );
        let checkout = notebook(&f);
        let before = f
            .records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap();
        assert_eq!(before.len(), 6);
        let mut live = driver(&f, 88, &checkout);
        open_note(&mut live, 88, f.child, Some(&before[0].0));
        let end = before[..4]
            .iter()
            .map(|row| row.1.chars().count().checked_add(1).unwrap())
            .sum();
        live.perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: if reverse { end } else { 0 },
                focus: if reverse { 0 } else { end },
            },
        )
        .unwrap();
        let prior = revision(&f, &checkout);
        move_selection(&mut live, false, semantic);
        let expected: Rows = [4, 0, 1, 2, 3, 5]
            .iter()
            .map(|&i| before[i].clone())
            .collect();
        assert_eq!(revision(&f, &checkout), prior.checked_add(1).unwrap());
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| snapshot(doc, f.child))
                .unwrap(),
            expected
        );
        assert_visible_order(&live, &expected);
        live.set_modifiers(&["ctrl".into()]);
        live.key("z");
        live.set_modifiers(&[]);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| snapshot(doc, f.child))
                .unwrap(),
            before
        );
        live.set_modifiers(&["ctrl".into(), "shift".into()]);
        live.key("z");
        live.set_modifiers(&[]);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| snapshot(doc, f.child))
                .unwrap(),
            expected
        );
        move_selection(&mut live, false, semantic);
        let twice: Rows = [4, 5, 0, 1, 2, 3]
            .iter()
            .map(|&i| before[i].clone())
            .collect();
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| snapshot(doc, f.child))
                .unwrap(),
            twice
        );
        move_selection(&mut live, true, semantic);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| snapshot(doc, f.child))
                .unwrap(),
            expected
        );
        close_driver(&mut live);
    }
}

fn assert_visible_order(driver: &ui::headless::Driver, rows: &Rows) {
    fn editor(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some("block_editor") {
            return Some(node);
        }
        node.children.iter().find_map(editor)
    }
    let inspection = driver.inspection();
    let editor = editor(&inspection.root).expect("the real editor is mounted");
    let ids = editor
        .semantics
        .as_ref()
        .unwrap()
        .children
        .iter()
        .filter_map(|block| block.semantic_key.as_deref()?.strip_prefix("notes.block:"))
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>()
    );
}

fn snapshot(doc: &AutoCommit, page: notes::PageId) -> Rows {
    let marks = read::page_block_marks(doc, page);
    read::page_blocks(doc, page)
        .into_iter()
        .map(|block| {
            let spans = marks.iter().find(|(id, _)| id == &block.id).unwrap();
            (
                block.id.clone(),
                block.text.clone(),
                block.kind.stored(),
                format!("{:?}", spans.1),
                u64::from(block.nesting),
            )
        })
        .collect()
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers build the edit in place"
)]
fn edit(doc: &mut AutoCommit, page: notes::PageId, edit: RichTextAction) {
    crate::component_support::edit_page_body(doc, page, &edit).unwrap();
}

fn move_group_and_verify(source: &str, order: &[usize], keyboard: bool) {
    let f = NotebookFixture::rich();
    f.write_page(f.child, source.as_bytes());
    let checkout = notebook(&f);
    let before = f
        .records
        .read(&checkout.id, |doc| snapshot(doc, f.child))
        .unwrap();
    assert_eq!(before.len(), order.len());
    let expected: Rows = order.iter().map(|&index| before[index].clone()).collect();
    let mut live = driver(&f, 88, &checkout);
    open_note(&mut live, 88, f.child, Some(&before[0].0));
    let rect = semantic_rect(&live, &format!("notes.block:{}", before[0].0));
    live.click_at(rect.x0 + 8.0, rect.center().y);
    let revision_before = revision(&f, &checkout);
    if keyboard {
        live.set_modifiers(&["alt".into()]);
        live.key("ArrowDown");
        live.set_modifiers(&[]);
    } else {
        live.perform_semantic_named(
            "block_editor",
            Some(&format!("notes.block-move:{}:down", before[0].0)),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    }
    live.relayout();
    assert_eq!(
        revision(&f, &checkout),
        revision_before.saturating_add(1),
        "a whole group is one undoable action"
    );
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap(),
        expected
    );
    assert_visible_order(&live, &expected);
    live.set_modifiers(&["ctrl".into()]);
    live.key("z");
    live.set_modifiers(&[]);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap(),
        before
    );
    assert_visible_order(&live, &before);
    live.set_modifiers(&["ctrl".into(), "shift".into()]);
    live.key("z");
    live.set_modifiers(&[]);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| snapshot(doc, f.child))
            .unwrap(),
        expected
    );
    assert_visible_order(&live, &expected);
    close_driver(&mut live);
    drop(live);
    f.records.try_flush().unwrap();
    let f = f.reopen();
    let mut reopened = driver(&f, 89, &checkout);
    open_note(&mut reopened, 89, f.child, None);
    assert_visible_order(&reopened, &expected);
    f.records
        .read(&checkout.id, |doc| {
            notes::read::validate(doc).unwrap();
            assert_eq!(snapshot(doc, f.child), expected);
        })
        .unwrap();
    close_driver(&mut reopened);
}

#[test]
fn list_subtrees_and_multiline_code_move_as_one_undoable_group() {
    eprintln!("phase mixed_list_subtree_moves_past_next_sibling_subtree_as_one_undoable_action");
    {
        let source = "1. **Parent A**\n  - *Child A*\n    - [x] Task A\n2. Parent B\n  1. Child B\n3. Parent C\n";
        for keyboard in [false, true] {
            move_group_and_verify(source, &[3, 4, 0, 1, 2, 5], keyboard);
        }
    }

    eprintln!("phase multiline_code_moves_as_one_group_and_preserves_all_rows_on_reopen");
    {
        move_group_and_verify(
            "```python\nfirst()\nsecond()\n```\n\nAfter\n",
            &[2, 0, 1],
            false,
        );
    }
}

#[test]
fn list_parents_and_formula_sources_move_their_whole_group() {
    eprintln!(
        "phase moving_a_list_parent_keeps_nested_paragraph_code_and_task_through_undo_and_reopen"
    );
    {
        let source = "1. Parent A\n\n  Paragraph child\n\n  ```python\n  first()\n  second()\n  ```\n\n  - [x] Child task\n    1. Grandchild\n\n2. Parent B\n";
        for keyboard in [false, true] {
            move_group_and_verify(source, &[6, 0, 1, 2, 3, 4, 5], keyboard);
        }
    }

    eprintln!("phase multiline_formula_source_moves_as_one_group");
    {
        move_group_and_verify("```tex\nx + y\n= z\n```\n\nAfter\n", &[2, 0, 1], false);
    }
}

#[test]
fn adjacent_same_language_fences_move_independently_with_undo_and_reopen() {
    for keyboard in [false, true] {
        move_group_and_verify(
            "```python\nfirst()\nmore()\n```\n\n```python\nsecond()\n```\n",
            &[2, 0, 1],
            keyboard,
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn concurrent_moves_and_order_upgrades_converge_without_replacing_text() {
    eprintln!("phase moving_a_block_preserves_concurrent_text_and_marks");
    {
        let f = NotebookFixture::rich();
        f.write_page(f.child, b"**First**\n\nSecond\n");
        let checkout = notebook(&f);
        let mut local = f.records.read(&checkout.id, AutoCommit::clone).unwrap();
        let before = read::page_block_snapshots(&local, f.child);
        assert_eq!(before.len(), 2);
        let mut peer = local.fork();

        edit(
            &mut local,
            f.child,
            RichTextAction::MoveBlocks {
                blocks: vec![before[0].0.clone()],
                after: Some(before[1].0.clone()),
            },
        );
        edit(
            &mut peer,
            f.child,
            RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: before[0].0.clone(),
                at: 5,
                del: 0,
                ins: " from peer".into(),
            },
        );
        let mut local_for_peer = local.clone();
        local.merge_incoming(&mut peer).unwrap();
        peer.merge_incoming(&mut local_for_peer).unwrap();

        let reopened = AutoCommit::load(&local.save()).unwrap();
        for document in [&local, &peer, &reopened] {
            notes::read::validate(document).unwrap();
            assert_eq!(
                read::page_block_snapshots(document, f.child),
                vec![
                    before[1].clone(),
                    (before[0].0.clone(), "First from peer".into())
                ],
                "moving on one peer must not discard simultaneous typing on another"
            );
            assert!(
                read::page_block_marks(document, f.child)[1]
                    .1
                    .iter()
                    .any(|span| { matches!(span.mark, actions::InlineMark::Bold) })
            );
        }
    }

    eprintln!(
        "phase concurrent_insertions_into_one_gap_remain_distinct_and_allow_insertion_between_them"
    );
    {
        let f = NotebookFixture::rich();
        f.write_page(f.child, b"Before\n\nAfter\n");
        let checkout = notebook(&f);
        let mut local = f.records.read(&checkout.id, AutoCommit::clone).unwrap();
        let before = read::page_block_snapshots(&local, f.child);
        let mut peer = local.fork();
        for (doc, name) in [(&mut local, "Local"), (&mut peer, "Peer")] {
            edit(
                doc,
                f.child,
                RichTextAction::InsertParagraph {
                    after: before[0].0.clone(),
                    text: name.into(),
                },
            );
        }
        let mut local_changes = local.clone();
        local.merge_incoming(&mut peer).unwrap();
        peer.merge_incoming(&mut local_changes).unwrap();
        let merged = read::page_block_snapshots(&local, f.child);
        assert_eq!(merged, read::page_block_snapshots(&peer, f.child));
        assert_eq!(merged.len(), 4);
        assert_eq!(merged[0], before[0]);
        assert_eq!(merged[3], before[1]);
        assert_ne!(merged[1].1, merged[2].1);
        edit(
            &mut local,
            f.child,
            RichTextAction::InsertParagraph {
                after: merged[1].0.clone(),
                text: "Between".into(),
            },
        );
        let final_rows = read::page_block_snapshots(&local, f.child);
        assert_eq!(final_rows[1], merged[1]);
        assert_eq!(final_rows[2].1, "Between");
        assert_eq!(final_rows[3], merged[2]);
        notes::read::validate(&local).unwrap();
    }

    eprintln!("phase concurrent_moves_converge");
    {
        let f = NotebookFixture::rich();
        f.write_page(f.child, b"A\n\nB\n\nC\n\nD\n");
        let checkout = notebook(&f);
        let mut local = f.records.read(&checkout.id, AutoCommit::clone).unwrap();
        let before = read::page_block_snapshots(&local, f.child);
        let mut peer = local.fork();
        for (doc, index) in [(&mut local, 0), (&mut peer, 3)] {
            edit(
                doc,
                f.child,
                RichTextAction::MoveBlocks {
                    blocks: vec![before[index].0.clone()],
                    after: Some(before[1].0.clone()),
                },
            );
        }
        let mut local_changes = local.clone();
        local.merge_incoming(&mut peer).unwrap();
        peer.merge_incoming(&mut local_changes).unwrap();
        notes::read::validate(&local).unwrap();
        notes::read::validate(&peer).unwrap();
        let expected = read::page_block_snapshots(&local, f.child);
        assert_eq!(expected, read::page_block_snapshots(&peer, f.child));
        assert_eq!(expected.len(), 4);
        assert_eq!(expected[0], before[1]);
        assert_eq!(expected[3], before[2]);
        assert!(expected[1..3].contains(&before[0]));
        assert!(expected[1..3].contains(&before[3]));
        let reopened = AutoCommit::load(&local.save()).unwrap();
        notes::read::validate(&reopened).unwrap();
        assert_eq!(read::page_block_snapshots(&reopened, f.child), expected);
    }

    eprintln!("phase repeated_middle_insertions_keep_their_order");
    {
        let f = NotebookFixture::rich();
        f.write_page(f.child, b"Before\n\nAfter\n");
        let checkout = notebook(&f);
        let mut doc = f.records.read(&checkout.id, AutoCommit::clone).unwrap();
        let rows = read::page_block_snapshots(&doc, f.child);
        let mut predecessor = rows[0].0.clone();
        let mut expected = vec![rows[0].1.clone()];
        for index in 0..128 {
            let text = format!("Middle {index}");
            edit(
                &mut doc,
                f.child,
                RichTextAction::InsertParagraph {
                    after: predecessor.clone(),
                    text: text.clone(),
                },
            );
            predecessor = read::page_block_snapshots(&doc, f.child)[index + 1]
                .0
                .clone();
            expected.push(text);
        }
        expected.push(rows[1].1.clone());
        let texts = |doc: &AutoCommit| {
            read::page_block_snapshots(doc, f.child)
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&doc), expected);
        let reopened = AutoCommit::load(&doc.save()).unwrap();
        notes::read::validate(&reopened).unwrap();
        assert_eq!(texts(&reopened), expected);
    }
}

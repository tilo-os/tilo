use super::*;
use crate::testing::{Dispatch, dispatch};
use automerge::transaction::Transactable as _;

#[test]
fn retained_block_batches_include_empty_rows_and_refuse_interleaved_peers() {
    let rows = vec![
        BlockContent::paragraph("a", "First"),
        BlockContent::paragraph("empty", ""),
        BlockContent::paragraph("c", "After"),
    ];
    let mut editor = BlockEditor::new();
    editor.present_blocks(&rows);
    editor.retain_block_selection(vec!["a".into(), "empty".into()], false);
    assert_eq!(editor.selected_block_move(false), Some((0..2, 3)));
    editor.present_blocks(&[rows[2].clone(), rows[0].clone(), rows[1].clone()]);
    assert_eq!(editor.selected_block_range(), Some(1..3));
    assert_eq!(editor.selected_block_move(true), Some((1..3, 0)));
    let tree = editor.accessibility().unwrap();
    assert!(
        tree.children
            .iter()
            .find(|row| row.semantic_key.as_deref() == Some("notes.block:empty"))
            .unwrap()
            .state
            .selected
    );
    editor.present_blocks(&[
        rows[0].clone(),
        BlockContent::paragraph("peer", "Peer"),
        rows[1].clone(),
        rows[2].clone(),
    ]);
    assert_eq!(editor.selected_block_move(false), None);
    assert_eq!(editor.document_selection(), None);
    assert!(
        editor
            .blocks()
            .iter()
            .all(|block| block.line.selection_range().is_none())
    );
    editor.present_blocks(&rows);
    assert_eq!(
        editor.selected_block_range(),
        None,
        "peer removal cannot revive an invisible batch"
    );
    assert_eq!(editor.document_selection(), None);
    editor.select_document_range(
        DocumentPosition { block: 0, byte: 0 },
        DocumentPosition { block: 1, byte: 0 },
    );
    assert_eq!(editor.selected_block_range(), Some(0..1));
    editor.focus_block(2, CursorPlacement::End);
    editor.clear_document_selection();
    assert_eq!(editor.selected_block_range(), Some(2..3));
}

#[test]
fn peer_added_descendant_invalidates_retained_batches_for_every_command() {
    let rows = vec![
        BlockContent::paragraph("a", "A"),
        BlockContent::paragraph("b", "B"),
        BlockContent::paragraph("c", "C"),
    ];
    let mut editor = BlockEditor::new();
    editor.present_blocks(&rows);
    editor.retain_block_selection(vec!["a".into(), "b".into()], false);
    let mut child = BlockContent::paragraph("peer", "Peer child");
    child.nesting = 1;
    editor.present_blocks(&[rows[0].clone(), rows[1].clone(), child, rows[2].clone()]);
    assert_eq!(
        editor.selected_block_range(),
        None,
        "duplication has no partial batch"
    );
    assert!(!editor.try_indent_selection(1));
    assert_eq!(editor.selected_block_move(false), None);
    assert_eq!(
        editor.document_selection(),
        None,
        "typing cannot replace peer content"
    );
}

#[test]
fn selected_movement_refuses_mixed_parents_and_keeps_complete_fences() {
    let mut editor = BlockEditor::from_markdown("- Parent\n  - child\n- Next\n");
    editor.select_document_range(
        DocumentPosition { block: 1, byte: 0 },
        DocumentPosition { block: 2, byte: 2 },
    );
    assert_eq!(editor.selected_block_move(true), None);
    editor.clear_document_selection();
    editor.focus_block(1, CursorPlacement::Start);
    assert_eq!(editor.selected_block_move(true), None);
    assert_eq!(editor.selected_block_move(false), None);
    let mut first = Block::code_block(Some("rust".into()), "first");
    let mut second = Block::code_block(Some("rust".into()), "second");
    first.code_group = Some("fence".into());
    second.code_group = Some("fence".into());
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::paragraph("Before"),
        first,
        second,
        Block::paragraph("After"),
    ]);
    editor.select_document_range(
        DocumentPosition { block: 0, byte: 0 },
        DocumentPosition { block: 2, byte: 2 },
    );
    assert_eq!(editor.selected_block_move(false), Some((0..3, 4)));
}

#[test]
fn document_accessibility_exposes_role_focus_tables_and_images() {
    fn accessibility_reports_multiline_document_role() {
        use crate::a11y::Role;
        let ed = BlockEditor::new();
        let node = Widget::accessibility(&ed).expect("editor is a document");
        assert_eq!(node.role, Role::Document);
        assert!(node.state.multiline, "a document is multiline");
        assert!(node.state.focusable);
        assert!(!node.state.focused, "fresh editor is not focused");
        assert_eq!(
            node.value, None,
            "the document text is not dumped into value"
        );
    }

    fn accessibility_focus_tracks_editor_focus() {
        let mut ed = BlockEditor::new();
        Widget::set_focus(&mut ed, true);
        let node = Widget::accessibility(&ed).unwrap();
        assert!(node.state.focused);
    }

    fn markdown_table_exposes_rows_and_cells_to_accessibility() {
        use crate::a11y::Role;
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::table(
            "| Name | Status |\n| --- | --- |\n| Search | Done |",
            false,
        )]);
        let document = Widget::accessibility(&editor).expect("document semantics");
        let table = document.children[0]
            .children
            .iter()
            .find(|node| node.role == Role::Table)
            .expect("table semantics");
        assert_eq!(table.children.len(), 2);
        assert!(table.children.iter().all(|row| row.role == Role::Row));
        assert!(
            table
                .children
                .iter()
                .flat_map(|row| &row.children)
                .all(|cell| cell.role == Role::Cell)
        );
    }

    fn standalone_markdown_image_exposes_alt_text() {
        use crate::a11y::Role;
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("![Architecture](diagram.png)")]);
        let document = Widget::accessibility(&editor).expect("document semantics");
        let image = document.children[0]
            .children
            .iter()
            .find(|node| node.role == Role::Image)
            .expect("image semantics");
        assert_eq!(image.name.as_deref(), Some("Architecture"));
        assert_eq!(image.description.as_deref(), Some("diagram.png"));
    }

    fn table_cell_source_keeps_readable_internal_spaces_and_exact_edge_spaces() {
        assert_eq!(encode_table_cell("Mina Chen"), "Mina Chen");
        assert_eq!(
            decode_table_cell(&encode_table_cell(" Mina Chen ")),
            " Mina Chen "
        );
        assert_eq!(
            encode_table_cell("A | B & C"),
            "A \\| B &amp; C",
            "Markdown syntax is escaped without obscuring ordinary spaces"
        );
    }

    accessibility_reports_multiline_document_role();
    accessibility_focus_tracks_editor_focus();
    markdown_table_exposes_rows_and_cells_to_accessibility();
    standalone_markdown_image_exposes_alt_text();
    table_cell_source_keeps_readable_internal_spaces_and_exact_edge_spaces();
}

fn layout_image_editor(editor: &mut BlockEditor, width: f32, renderer: &mut TextRenderer) {
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = editor.build_layout(&mut tree, &Theme::dark(), &mut nodes);
    let root = tree
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(width),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    tree.compute_layout_with_measure(
        root,
        Size {
            width: taffy::AvailableSpace::Definite(width),
            height: taffy::AvailableSpace::MaxContent,
        },
        |known, available, node, _, _| {
            nodes.get(&node).map_or(Size::ZERO, |widget| {
                // The pointers are the stable widget leaves registered by build_layout.
                unsafe { (**widget).measure(known, available, renderer) }
            })
        },
    )
    .unwrap();
    editor.apply_layout(&tree, node, Point::ORIGIN);
}

#[test]
fn image_attachments_size_reserve_and_guard_their_source() {
    fn image_attachment_sizes_to_available_width_without_layout_feedback() {
        let bytes = crate::png::encode_rgba8(160, 100, &vec![255; 160 * 100 * 4]).unwrap();
        let fetch: crate::image_store::Fetcher = std::sync::Arc::new(move |_| Some(bytes.clone()));
        let store = crate::ImageStore::new(&fetch);
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("![Document scan](assets/scanned-document.png)"),
            Block::paragraph("Text below the scan"),
        ]);
        editor.set_image_store(Some(store));
        assert!(
            editor.prewarm_images_for_shots(),
            "fixture image must decode"
        );
        let mut renderer = TextRenderer::new();
        layout_image_editor(&mut editor, 1000.0, &mut renderer);
        let image_id = editor.blocks[0].id.clone();
        assert!(editor.set_image_source_mode(&image_id, true));
        for width in [1000.0, 320.0, 1000.0] {
            let mut previous = None;
            for _ in 0..3 {
                layout_image_editor(&mut editor, width, &mut renderer);
                let preview = editor.image_preview_rects[0].unwrap();
                // The image stands on the content edge: inside the line's
                // text inset on both sides.
                let inner = f64::from(width) - editor.handle_column() - 2.0 * editor.text_pad;
                assert!((preview.width() - inner).abs() < 0.01, "{preview:?}");
                assert!(
                    (preview.height() - inner * 100.0 / 160.0).abs() <= 0.5,
                    "image must occupy its full available width at its native aspect \
                     (within the layout's whole-pixel rounding): {preview:?}"
                );
                let source = editor.blocks[0].line.get_rect();
                let following = editor.blocks[1].line.get_rect();
                assert!(source.height() >= 24.0, "editable source remains visible");
                assert!(
                    source.y1 < preview.y0 && following.y0 >= preview.y1,
                    "source, image and following text must not overlap"
                );
                if let Some(previous) = previous {
                    assert_eq!(
                        preview, previous,
                        "painting must not feed back into image size"
                    );
                }
                previous = Some(preview);
                editor.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut renderer,
                    &Theme::dark(),
                    &PresenceCtx::empty(),
                );
            }
        }
    }

    fn a_tall_image_takes_at_most_its_share_of_the_viewport_at_its_aspect() {
        let bytes = crate::png::encode_rgba8(300, 1200, &vec![255; 300 * 1200 * 4]).unwrap();
        let fetch: crate::image_store::Fetcher = std::sync::Arc::new(move |_| Some(bytes.clone()));
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("![Phone screenshot](assets/tall.png)"),
            Block::paragraph("Text below the screenshot"),
        ]);
        editor.set_image_store(Some(crate::ImageStore::new(&fetch)));
        assert!(
            editor.prewarm_images_for_shots(),
            "fixture image must decode"
        );
        let mut renderer = TextRenderer::new();
        layout_image_editor(&mut editor, 1000.0, &mut renderer);
        let inner = 1000.0 - editor.handle_column() - 2.0 * editor.text_pad;
        let natural = editor.image_preview_rects[0].unwrap();
        assert!(
            (natural.height() - inner * 4.0).abs() <= 0.5,
            "a host that reports no viewport keeps the native size: {natural:?}"
        );

        assert!(editor.set_viewport_height(Some(900.0)));
        assert!(!editor.set_viewport_height(Some(900.0)), "unchanged");
        layout_image_editor(&mut editor, 1000.0, &mut renderer);
        let preview = editor.image_preview_rects[0].unwrap();
        let cap = 900.0 * IMAGE_VIEWPORT_FRACTION;
        assert!(
            (preview.height() - cap).abs() <= 0.5,
            "a tall picture is capped to {cap}: {preview:?}"
        );
        let following = editor.blocks[1].line.get_rect();
        assert!(following.y0 >= preview.y1, "the text below moves up");
        let painted = fitted_image_rect(preview, (300, 1200));
        assert!(
            (painted.width() / painted.height() - 0.25).abs() < 1e-6,
            "its aspect is kept: {painted:?}"
        );
        assert!((painted.height() - preview.height()).abs() < 1e-6);
        assert!(painted.width() < preview.width(), "so it is drawn narrower");

        // A picture shorter than the cap keeps its native size.
        assert!(editor.set_viewport_height(Some(20_000.0)));
        layout_image_editor(&mut editor, 1000.0, &mut renderer);
        let preview = editor.image_preview_rects[0].unwrap();
        assert!((preview.height() - inner * 4.0).abs() <= 0.5, "{preview:?}");
    }

    fn missing_image_reserves_placeholder_and_wraps_its_editable_source() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph(
        "![A descriptive alternate text that wraps across a narrow document](assets/missing-image.png)"
    )]);
        let mut renderer = TextRenderer::new();
        layout_image_editor(&mut editor, 240.0, &mut renderer);
        let image_id = editor.blocks[0].id.clone();
        assert!(editor.set_image_source_mode(&image_id, true));
        layout_image_editor(&mut editor, 240.0, &mut renderer);
        let source = editor.blocks[0].line.get_rect();
        let preview = editor.image_preview_rects[0].unwrap();
        assert!(
            source.height() > 28.0,
            "long image source must not be clipped to one line"
        );
        // The preview's edges snap to whole pixels below a fractional
        // source block, so its height may differ by up to half a pixel.
        assert!((preview.height() - 220.0).abs() <= 0.5, "{preview:?}");
        assert!(preview.y0 > source.y1);
    }

    fn image_source_mode_is_explicit_stable_and_does_not_mutate_content() {
        fn assert_control(editor: &BlockEditor, target: &str) {
            let document = editor.accessibility().unwrap();
            assert!(
                document
                    .children
                    .iter()
                    .flat_map(|block| &block.children)
                    .any(|control| control.action_target.as_deref() == Some(target)
                        && control
                            .bounds
                            .is_some_and(|bounds| bounds.to_rect().width() > 0.0
                                && bounds.to_rect().height() > 0.0)),
                "image mode control must expose its production semantic target and geometry: {target}"
            );
        }
        let source = "![Scan](assets/scan.png)";
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph(source)]);
        let id = editor.blocks[0].id.clone();
        let mut renderer = TextRenderer::new();
        let mut doc = AutoCommit::new();
        layout_image_editor(&mut editor, 640.0, &mut renderer);
        assert_eq!(
            editor.blocks[0].line.get_rect().height(),
            0.0,
            "source is hidden at rest"
        );
        assert_eq!(
            editor.children().len(),
            1,
            "shared edit button is host-discoverable"
        );
        assert_control(&editor, &format!("notes.image-edit:{id}"));
        editor.begin_edit();
        assert!(editor.perform_semantic_action(
            &format!("notes.image-edit:{id}"),
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(
            editor.take_edit_ops().is_empty(),
            "presentation mode is not a document action"
        );
        layout_image_editor(&mut editor, 640.0, &mut renderer);
        assert!(editor.blocks[0].line.get_rect().height() > 0.0);
        assert_control(&editor, &format!("notes.image-done:{id}"));
        editor.set_focus(false);
        layout_image_editor(&mut editor, 640.0, &mut renderer);
        assert!(
            editor.image_attachments[&id].editing,
            "blur must not collapse the source field"
        );
        editor.blocks[0].line.set_text("![Scan](assets/");
        layout_image_editor(&mut editor, 640.0, &mut renderer);
        assert!(
            editor.image_attachments[&id].editing,
            "a half-edited URL retains its editor"
        );
        assert!(editor.blocks[0].line.get_rect().height() > 0.0);
        assert!(
            editor.image_preview_rects[0].is_some(),
            "last valid preview stays in place"
        );
        editor.blocks[0].line.set_text(source);
        editor.begin_edit();
        assert!(editor.perform_semantic_action(
            &format!("notes.image-done:{id}"),
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(editor.take_edit_ops().is_empty());
        layout_image_editor(&mut editor, 640.0, &mut renderer);
        assert_eq!(editor.blocks[0].line.get_rect().height(), 0.0);
        assert_eq!(editor.blocks[0].text(), source);
        assert!(!editor.perform_semantic_action(
            &format!("notes.image-source:{id}"),
            &crate::a11y::Action::SetValue("hidden overwrite".into()),
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
    }

    fn image_preview_rejects_hidden_typing_and_ime_and_deletes_atomically() {
        use winit::event::{ElementState, Ime};
        let source = "![Scan](assets/scan.png)";
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph(source)]);
        let mut renderer = TextRenderer::new();
        layout_image_editor(&mut editor, 640.0, &mut renderer);
        editor.focus_block(0, CursorPlacement::Start);
        let event = crate::synthetic_input::key_event(
            Key::Character("x".into()),
            Some("x".into()),
            ElementState::Pressed,
        );
        let character = crate::synthetic_input::keyboard_input(event);
        editor.begin_edit();
        for event in [character, WindowEvent::Ime(Ime::Commit("hidden".into()))] {
            let handled = dispatch(&mut editor, &event);
            assert!(handled);
            assert_eq!(editor.blocks[0].text(), source);
        }
        assert!(editor.take_edit_ops().is_empty());
        let key = crate::synthetic_input::key_event(
            Key::Named(NamedKey::Backspace),
            None,
            ElementState::Pressed,
        );
        let event = crate::synthetic_input::keyboard_input(key);
        editor.begin_edit();
        dispatch(&mut editor, &event);
        assert_eq!(
            editor.blocks[0].text(),
            "",
            "Delete removes the attachment rather than one hidden character"
        );
        assert!(!editor.take_edit_ops().is_empty());
    }

    image_attachment_sizes_to_available_width_without_layout_feedback();
    a_tall_image_takes_at_most_its_share_of_the_viewport_at_its_aspect();
    missing_image_reserves_placeholder_and_wraps_its_editable_source();
    image_source_mode_is_explicit_stable_and_does_not_mutate_content();
    image_preview_rejects_hidden_typing_and_ime_and_deletes_atomically();
}

#[test]
fn enter_on_empty_list_outdents_then_exits_and_numbered_lists_continue() {
    fn enter_on_empty_top_level_list_exits_to_paragraph() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::checklist(0, false, "")]);
        editor.apply_signal(0, &LineSignal::Enter { split_at: 0 });
        assert_eq!(editor.blocks().len(), 1);
        assert!(matches!(editor.blocks()[0].kind, BlockKind::Paragraph));
    }

    fn enter_on_empty_nested_list_outdents_before_exiting() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::bullet(2, "")]);
        editor.apply_signal(0, &LineSignal::Enter { split_at: 0 });
        assert!(matches!(
            editor.blocks()[0].kind,
            BlockKind::Bullet { indent: 1 }
        ));
        editor.apply_signal(0, &LineSignal::Enter { split_at: 0 });
        assert!(matches!(
            editor.blocks()[0].kind,
            BlockKind::Bullet { indent: 0 }
        ));
        editor.apply_signal(0, &LineSignal::Enter { split_at: 0 });
        assert!(matches!(editor.blocks()[0].kind, BlockKind::Paragraph));
    }

    fn enter_continues_a_numbered_list() {
        // Pressing Enter mid-numbered-item continues the list: the suffix
        // becomes a NEW numbered item at the same indent (the renderer
        // then numbers it 2).
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::numbered(0, "first");
        e.focused = Some(0);
        e.split_at_cursor(0, 5); // caret at end of "first"
        assert_eq!(e.blocks().len(), 2);
        assert!(matches!(
            e.blocks()[1].kind,
            BlockKind::Numbered { indent: 0 }
        ));
        assert_eq!(numbered_ordinals(e.blocks()), vec![1, 2]);
    }

    enter_on_empty_top_level_list_exits_to_paragraph();
    enter_on_empty_nested_list_outdents_before_exiting();
    enter_continues_a_numbered_list();
}

#[test]
fn block_reorder_emits_stable_id_moves_through_semantic_controls() {
    fn accessible_block_reorder_controls_are_stable_and_action_owned() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("first"), Block::paragraph("second")]);
        let first_id = editor.blocks()[0].id.clone();
        let second_id = editor.blocks()[1].id.clone();
        let node = Widget::accessibility(&editor).expect("document semantics");
        let first = node
            .children
            .iter()
            .find(|child| child.name.as_deref() == Some("Paragraph block 1"))
            .expect("first block group");
        let first_group_key = first.key.expect("block group has durable AT identity");
        assert!(first.children[0].state.disabled, "top block cannot move up");
        let down_target = BlockEditor::block_move_target(&first_id, "down");
        assert_eq!(
            first.children[1].action_target.as_deref(),
            Some(&*down_target)
        );

        assert!(Widget::perform_semantic_action(
            &mut editor,
            &down_target,
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(editor.blocks()[1].id, first_id);
        let reordered = Widget::accessibility(&editor).expect("reordered document semantics");
        let first_semantic_key = format!("notes.block:{first_id}");
        assert_eq!(
            reordered
                .children
                .iter()
                .find(|child| child.semantic_key.as_deref() == Some(first_semantic_key.as_str()))
                .and_then(|child| child.key),
            Some(first_group_key),
            "assistive technology keeps the same block node across reorder",
        );
        assert_eq!(
            editor.take_pending_semantic_ops(),
            vec![BlockEditOp::Move {
                block_id: second_id,
                after: None,
            }]
        );
        assert!(
            !Widget::perform_semantic_action(
                &mut editor,
                "notes.block-move:missing:down",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ),
            "a stale stable id must refuse instead of moving another block"
        );
    }

    fn moving_a_block_emits_a_stable_id_move_operation() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("first"),
            Block::paragraph("second"),
            Block::paragraph("third"),
        ]);
        let first_id = editor.blocks()[0].id.clone();
        let second_id = editor.blocks()[1].id.clone();
        editor.begin_edit();
        assert!(editor.move_block_to(0, 2));
        assert_eq!(editor.blocks()[2].id, first_id);
        assert!(editor.take_structure_dirty());
        let ops = editor.take_edit_ops();
        assert!(ops.contains(&BlockEditOp::Move {
            block_id: second_id,
            after: None,
        }));
    }

    accessible_block_reorder_controls_are_stable_and_action_owned();
    moving_a_block_emits_a_stable_id_move_operation();
}

#[test]
fn pointer_and_keyboard_move_focus_and_selection_across_blocks() {
    fn click_focuses_the_block_under_the_cursor() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{ElementState, MouseButton, WindowEvent};

        let mut e = BlockEditor::new();
        e.set_blocks(vec![Block::paragraph("first"), Block::paragraph("second")]);
        // Stack two rows: block 0 at y[0,20], block 1 at y[24,44].
        e.blocks[0]
            .line
            .apply_layout_external(Rect::new(0.0, 0.0, 200.0, 20.0));
        e.blocks[1]
            .line
            .apply_layout_external(Rect::new(0.0, 24.0, 200.0, 44.0));
        e.focus_block(0, CursorPlacement::End);
        assert_eq!(e.focused, Some(0));

        let dev = winit::event::DeviceId::dummy();
        let moved = WindowEvent::CursorMoved {
            device_id: dev,
            position: PhysicalPosition::new(50.0, 30.0), // inside block 1
        };
        let click = WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };

        for ev in [&moved, &click] {
            dispatch(&mut e, ev);
        }
        assert_eq!(
            e.focused,
            Some(1),
            "clicking inside block 1 must move focus (and the caret) there"
        );
    }

    fn ctrl_shift_arrow_extends_selection_across_blocks() {
        use winit::keyboard::{ModifiersState, NamedKey};

        use crate::synthetic_input::named_key;

        let mut e = BlockEditor::new();
        e.set_blocks(vec![
            Block::paragraph("hello"),
            Block::paragraph("world"),
            Block::paragraph("foo"),
        ]);
        e.focus_block(0, CursorPlacement::Byte(2)); // caret after "he"

        let mut ctrl_shift =
            Dispatch::new().modifiers(ModifiersState::CONTROL | ModifiersState::SHIFT);

        // Down once: selects the tail of block 0 ("llo"), caret into block 1.
        ctrl_shift.send(&mut e, &named_key(NamedKey::ArrowDown));
        assert_eq!(e.focused_index(), Some(1), "active end walked into block 1");
        assert_eq!(
            e.blocks()[0].line.selection_range(),
            Some((2, 5)),
            "the jumped tail of block 0 is now selected (real key path)"
        );

        // Down again: block 1 fully selected too.
        ctrl_shift.send(&mut e, &named_key(NamedKey::ArrowDown));
        assert_eq!(e.focused_index(), Some(2));
        assert_eq!(e.blocks()[0].line.selection_range(), Some((2, 5)));
        assert_eq!(
            e.blocks()[1].line.selection_range(),
            Some((0, 5)),
            "extending over block 1 selects it whole"
        );

        // Retreat up: block 2 cleared, block 1 selected to its end.
        ctrl_shift.send(&mut e, &named_key(NamedKey::ArrowUp));
        assert_eq!(e.focused_index(), Some(1));
        assert_eq!(
            e.blocks()[2].line.selection_range(),
            None,
            "retreating clears the block we left"
        );
        assert_eq!(e.blocks()[1].line.selection_range(), Some((0, 5)));
    }

    fn plain_arrow_does_not_start_a_selection() {
        let mut e = BlockEditor::new();
        e.set_blocks(vec![Block::paragraph("hello"), Block::paragraph("world")]);
        e.focus_block(0, CursorPlacement::Byte(2));
        e.apply_signal(0, &LineSignal::ArrowDown);
        assert_eq!(e.focused_index(), Some(1));
        assert_eq!(e.blocks()[0].line.selection_range(), None);
        assert_eq!(e.blocks()[1].line.selection_range(), None);
    }

    fn slash_menu_owns_ime_without_committing_filter_text_into_the_document() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("/")]);
        editor.focus_block(0, CursorPlacement::End);
        editor.open_slash_menu_for_test();

        let send = |editor: &mut BlockEditor, event: &WindowEvent| {
            assert!(dispatch(editor, event));
        };

        send(
            &mut editor,
            &WindowEvent::Ime(winit::event::Ime::Preedit("h2".into(), Some((1, 1)))),
        );
        assert_eq!(editor.blocks()[0].text(), "/");
        assert_eq!(editor.slash_menu().filter(), "");

        let mut scene = Scene::new();
        let mut renderer = TextRenderer::new();
        editor.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
        assert!(Widget::ime_cursor_rect(&editor).is_some());

        send(
            &mut editor,
            &WindowEvent::Ime(winit::event::Ime::Commit("h2".into())),
        );
        assert_eq!(editor.blocks()[0].text(), "/");
        assert_eq!(editor.slash_menu().filter(), "h2");
        assert_eq!(editor.slash_menu().visible_len(), 1);
    }

    click_focuses_the_block_under_the_cursor();
    ctrl_shift_arrow_extends_selection_across_blocks();
    plain_arrow_does_not_start_a_selection();
    slash_menu_owns_ime_without_committing_filter_text_into_the_document();
}

/// Seed a canonical block list the way an action owner stores it.
fn seed_canonical_blocks(doc: &mut AutoCommit, blocks: &[(&str, &str)]) -> (ObjId, Vec<String>) {
    use automerge::transaction::Transactable;
    let list = doc
        .put_object(automerge::ROOT, "blocks", ObjType::List)
        .unwrap();
    let mut ids = Vec::with_capacity(blocks.len());
    for (index, (kind, text)) in blocks.iter().enumerate() {
        let map = doc.insert_object(&list, index, ObjType::Map).unwrap();
        let id = Uuid::new_v4().to_string();
        doc.put(&map, "id", id.as_str()).unwrap();
        doc.put(&map, "kind", *kind).unwrap();
        let body = doc.put_object(&map, "text", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, text).unwrap();
        ids.push(id);
    }
    (list, ids)
}

/// Replay text and structure operations the way an owner would, over
/// `(id, text)` rows.
fn replay_edit_ops(rows: &mut Vec<(String, String)>, ops: &[BlockEditOp]) {
    let position = |rows: &[(String, String)], id: &str| {
        rows.iter()
            .position(|(row, _)| row == id)
            .unwrap_or_else(|| panic!("op addresses unknown block {id}"))
    };
    let slot_after = |rows: &[(String, String)], after: &Option<String>| {
        after.as_ref().map_or(0, |after| position(rows, after) + 1)
    };
    for op in ops {
        match op {
            BlockEditOp::TextSplice {
                block_id,
                start,
                end,
                text,
            } => {
                let index = position(rows, block_id);
                rows[index].1.replace_range(*start..*end, text);
            }
            BlockEditOp::Insert {
                block_id,
                after,
                text,
                ..
            } => {
                let slot = slot_after(rows, after);
                rows.insert(slot, (block_id.clone(), text.clone()));
            }
            BlockEditOp::Delete { block_id } => {
                let index = position(rows, block_id);
                rows.remove(index);
            }
            BlockEditOp::Move { block_id, after } => {
                let index = position(rows, block_id);
                let row = rows.remove(index);
                let slot = slot_after(rows, after);
                rows.insert(slot, row);
            }
            BlockEditOp::SetKind { .. }
            | BlockEditOp::SetChecklist { .. }
            | BlockEditOp::SetNesting { .. }
            | BlockEditOp::SetCodeGroup { .. } => {}
        }
    }
}

#[test]
fn editor_adopts_canonical_blocks_and_reports_structure_edits_as_ops() {
    fn editor_adopts_blocks_from_doc() {
        let mut doc = AutoCommit::new();
        let (list, ids) = seed_canonical_blocks(&mut doc, &[("heading1", "Title")]);
        let mut ed = BlockEditor::new();
        ed.bind_existing_blocks(&doc, list).unwrap();
        assert_eq!(ed.blocks().len(), 1);
        assert_eq!(ed.blocks()[0].text(), "Title");
        assert_eq!(ed.blocks()[0].id, ids[0]);
        assert!(matches!(ed.blocks()[0].kind, BlockKind::Heading(1)));
    }

    fn late_joining_peer_adopts_the_same_blocks() {
        use automerge::ROOT;
        let mut a_doc = AutoCommit::new();
        let (a_list, ids) = seed_canonical_blocks(&mut a_doc, &[("paragraph", "from-A")]);
        let mut a = BlockEditor::new();
        a.bind_existing_blocks(&a_doc, a_list).unwrap();

        // Peer B joins from A's full state (the late-join handshake).
        let b_doc = AutoCommit::load(&a_doc.save()).unwrap();
        let Ok(Some((Value::Object(ObjType::List), b_list))) = b_doc.get(&ROOT, "blocks") else {
            panic!("blocks list should exist in B's adopted doc");
        };
        let mut b = BlockEditor::new();
        b.bind_existing_blocks(&b_doc, b_list).unwrap();

        assert_eq!(b.blocks()[0].text(), "from-A", "B adopts A's block text");
        assert_eq!(b.blocks()[0].id, a.blocks()[0].id);
        assert_eq!(b.blocks()[0].id, ids[0]);
    }

    fn split_reports_the_new_block_as_ops_without_writing_the_doc() {
        let mut doc = AutoCommit::new();
        let (list, ids) = seed_canonical_blocks(&mut doc, &[("paragraph", "abcdef")]);
        doc.commit();
        let heads = (doc.base_heads(), doc.pending_ops());
        let mut ed = BlockEditor::new();
        ed.bind_existing_blocks(&doc, list.clone()).unwrap();

        ed.begin_edit();
        ed.split_at_cursor(0, 3);
        let new_id = ed.blocks()[1].id.clone();
        assert_eq!(
            ed.take_edit_ops(),
            vec![
                BlockEditOp::TextSplice {
                    block_id: ids[0].clone(),
                    start: 3,
                    end: 6,
                    text: String::new(),
                },
                BlockEditOp::Insert {
                    block_id: new_id,
                    after: Some(ids[0].clone()),
                    kind: "paragraph".into(),
                    text: "def".into(),
                },
            ]
        );
        assert_eq!(doc.length(&list), 1, "the owner applies structure edits");
        assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
    }

    editor_adopts_blocks_from_doc();
    late_joining_peer_adopts_the_same_blocks();
    split_reports_the_new_block_as_ops_without_writing_the_doc();
}

#[test]
fn presence_payload_reports_caret_selection_or_nothing() {
    fn presence_payload_reports_focused_block_caret() {
        use crate::collaboration::PresencePayload;
        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![Block::paragraph("hello world")]);
        ed.focus_block(0, CursorPlacement::Byte(3));
        match crate::presence::capability::presence_payload(&ed) {
            Some((key, PresencePayload::Caret { pos })) => {
                assert_eq!(key, block_presence_key(&ed.blocks[0].id));
                assert_eq!(pos, 3);
            }
            other => panic!("expected caret payload at byte 3, got {other:?}"),
        }
    }

    fn presence_payload_carries_one_ordered_cross_block_selection() {
        use crate::collaboration::PresencePayload;

        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![
            Block::paragraph("alpha"),
            Block::paragraph("middle"),
            Block::paragraph("omega"),
        ]);
        ed.select_document_range(
            DocumentPosition { block: 2, byte: 3 },
            DocumentPosition { block: 0, byte: 2 },
        );

        assert_eq!(
            crate::presence::capability::presence_payload(&ed),
            Some((
                document_selection_presence_key(&ed.blocks[0].id, 2),
                PresencePayload::Selection { start: 2, end: 3 },
            ))
        );
    }

    fn presence_payload_none_when_unfocused() {
        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![Block::paragraph("hi")]);
        // set_blocks leaves the editor unfocused.
        assert!(crate::presence::capability::presence_payload(&ed).is_none());
    }

    presence_payload_reports_focused_block_caret();
    presence_payload_carries_one_ordered_cross_block_selection();
    presence_payload_none_when_unfocused();
}

#[test]
fn cross_block_selection_copies_deletes_pastes_and_replaces_coherently() {
    fn cross_block_selection_serializes_plain_text_in_document_order() {
        let mut e = BlockEditor::new();
        e.set_blocks(vec![
            Block::paragraph("alpha"),
            Block::heading(2, "βeta"),
            Block::paragraph("omega"),
        ]);

        // A backwards drag still copies in reading order. The endpoints are
        // UTF-8 byte offsets: after β in block 1, after "om" in block 2.
        e.paint_cross_block_selection(2, 2, 0, 2);
        assert_eq!(
            e.document_selection(),
            Some(DocumentSelection {
                anchor: DocumentPosition { block: 2, byte: 2 },
                cursor: DocumentPosition { block: 0, byte: 2 },
            })
        );
        assert_eq!(e.selected_text().as_deref(), Some("pha\nβeta\nom"));
    }

    fn deleting_cross_block_selection_merges_prefix_and_suffix() {
        let mut e = BlockEditor::new();
        let mut first = Block::heading(1, "Hello world");
        first.line.set_marks(vec![MarkSpan::new(0..5, Mark::Bold)]);
        let mut last = Block::paragraph("goodbye");
        last.line.set_marks(vec![MarkSpan::new(4..7, Mark::Italic)]);
        e.set_blocks(vec![first, Block::paragraph("middle"), last]);
        e.paint_cross_block_selection(0, 6, 2, 4);

        assert!(e.replace_document_selection(""));
        assert_eq!(e.blocks().len(), 1);
        assert_eq!(e.blocks()[0].text(), "Hello bye");
        assert_eq!(e.blocks()[0].kind, BlockKind::Heading(1));
        assert_eq!(
            e.blocks()[0].line.marks(),
            &[
                MarkSpan::new(0..5, Mark::Bold),
                MarkSpan::new(6..9, Mark::Italic),
            ],
            "unselected inline formatting survives the merge"
        );
        assert_eq!(e.focused_index(), Some(0));
        assert_eq!(e.blocks()[0].line.cursor_pos(), 6);
        assert_eq!(e.document_selection(), None);
        assert!(
            !e.take_edit_ops().is_empty(),
            "the replacement reaches the owner"
        );
        assert!(e.take_edit_ops().is_empty(), "and only once");
    }

    fn multiline_paste_replaces_range_and_uses_enter_continuation_kinds() {
        let mut e = BlockEditor::new();
        e.set_blocks(vec![
            Block::bullet(1, "before selected"),
            Block::paragraph("discard me"),
            Block::paragraph("selected after"),
        ]);
        e.paint_cross_block_selection(0, 7, 2, 8);

        assert!(e.replace_document_selection("one\r\ntwo\rthree\u{0}four"));
        assert_eq!(
            e.blocks().iter().map(Block::text).collect::<Vec<_>>(),
            vec!["before one", "two", "threefour after"]
        );
        assert_eq!(e.blocks()[0].kind, BlockKind::Bullet { indent: 1 });
        assert_eq!(e.blocks()[1].kind, BlockKind::Bullet { indent: 1 });
        assert_eq!(e.blocks()[2].kind, BlockKind::Bullet { indent: 1 });
        assert_eq!(e.focused_index(), Some(2));
        assert_eq!(e.blocks()[2].line.cursor_pos(), "threefour".len());
    }

    fn ctrl_a_then_typing_replaces_the_document_through_the_real_event_path() {
        use winit::event::{ElementState, WindowEvent};
        use winit::keyboard::{Key, ModifiersState};

        fn character(key: &str, text: Option<&str>) -> WindowEvent {
            let event = crate::synthetic_input::key_event(
                Key::Character(key.into()),
                text.map(Into::into),
                ElementState::Pressed,
            );
            crate::synthetic_input::keyboard_input(event)
        }

        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("alpha"),
            Block::heading(2, "middle"),
            Block::paragraph("omega"),
        ]);
        editor.focus_block(1, CursorPlacement::Byte(3));
        assert!(
            Dispatch::new()
                .modifiers(ModifiersState::CONTROL)
                .send(&mut editor, &character("a", None))
                .handled
        );
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("alpha\nmiddle\nomega")
        );

        assert!(dispatch(&mut editor, &character("X", Some("X"))));
        assert_eq!(editor.blocks().len(), 1);
        assert_eq!(editor.blocks()[0].text(), "X");
        assert_eq!(editor.blocks()[0].kind, BlockKind::Paragraph);
        assert!(!editor.take_edit_ops().is_empty());
    }

    fn cross_block_replacement_emits_one_coherent_document_state() {
        let mut doc = AutoCommit::new();
        let (list, ids) = seed_canonical_blocks(
            &mut doc,
            &[
                ("paragraph", "left OLD"),
                ("paragraph", "remove"),
                ("paragraph", "OLD right"),
            ],
        );
        let mut e = BlockEditor::new();
        e.bind_existing_blocks(&doc, list).unwrap();
        e.paint_cross_block_selection(0, 5, 2, 3);

        e.begin_edit();
        assert!(e.replace_document_selection("new\nline"));
        let ops = e.take_edit_ops();

        let mut rows = ids
            .iter()
            .cloned()
            .zip(["left OLD", "remove", "OLD right"].map(String::from))
            .collect::<Vec<_>>();
        replay_edit_ops(&mut rows, &ops);
        assert_eq!(
            rows.iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>(),
            ["left new", "line right"],
            "one batch of ops reproduces the replacement: {ops:?}"
        );
        assert_eq!(
            rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            e.blocks().iter().map(|b| b.id.as_str()).collect::<Vec<_>>()
        );
    }

    cross_block_selection_serializes_plain_text_in_document_order();
    deleting_cross_block_selection_merges_prefix_and_suffix();
    multiline_paste_replaces_range_and_uses_enter_continuation_kinds();
    ctrl_a_then_typing_replaces_the_document_through_the_real_event_path();
    cross_block_replacement_emits_one_coherent_document_state();
}

#[test]
fn block_typography_scales_and_bands_each_kind() {
    fn code_block_cells_render_monospace() {
        let b = Block::code_block(Some("rust".into()), "let x = 1;");
        assert!(
            b.line.inner().is_mono(),
            "code-block cells must shape their text in the monospace face"
        );
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("plain");
        e.focused = Some(0);
        assert!(e.try_convert_to(0, BlockKind::CodeBlock { lang: None }));
        assert!(
            e.blocks()[0].line.inner().is_mono(),
            "converting to a code block switches the cell to monospace"
        );
        assert!(!Block::paragraph("hi").line.inner().is_mono());
    }

    fn heading_paragraph_inline_code_and_numbered_prefix_scale_once() {
        let heading = Block::heading(1, "Heading");
        let mut paragraph = Block::paragraph("inline code");
        paragraph
            .line
            .set_marks(vec![MarkSpan::new(0..6, Mark::Code)]);
        paragraph.line.inner_mut().set_selection(0, 6);
        let numbered = Block::numbered(0, "numbered");
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![heading, paragraph, numbered]);

        // H1 is the `LargeTitle` role; what this test guards is that the
        // shaped size is that role's size once, then exactly doubled at 2×.
        let h1 = BlockKind::Heading(1).font_size();
        assert_eq!(
            h1,
            Theme::dark().text(crate::tokens::TextRole::LargeTitle).size
        );
        layout_and_draw_editor(&mut editor, &Theme::dark());
        assert_eq!(
            editor.blocks[0].line.inner().shaped_font_size_for_test(),
            Some(h1)
        );
        assert_eq!(
            editor.blocks[1].line.inner().shaped_font_size_for_test(),
            Some(BlockKind::Paragraph.font_size())
        );

        let large = Theme::dark().with_text_scale(2.0);
        layout_and_draw_editor(&mut editor, &large);
        assert_eq!(
            editor.blocks[0].line.inner().shaped_font_size_for_test(),
            Some(h1 * 2.0)
        );
        for index in [1, 2] {
            assert_eq!(
                editor.blocks[index]
                    .line
                    .inner()
                    .shaped_font_size_for_test(),
                Some(BlockKind::Paragraph.font_size() * 2.0)
            );
        }
        let selection = editor.blocks[1].line.selection_rect(&large, 0, 6);
        assert!(
            (selection.height() - f64::from(BlockKind::Paragraph.font_size()) * 2.5).abs() < 0.1
        );

        let numbered_line = &editor.blocks[2].line;
        let line_rect = numbered_line.get_rect();
        let gutter = prefix_gutter_for(&editor.blocks[2].kind) * 2.0;
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let (label_width, _) = renderer.measure("1.", BlockKind::Paragraph.font_size(), None);
        let origin = numbered_prefix_origin(
            line_rect,
            line_rect.x0 - gutter,
            f64::from(label_width),
            BlockKind::Paragraph.font_size(),
            2.0,
            large.tokens.metrics.icon_label_gap,
        );
        let caret = numbered_line.caret_rect(&large, 0);
        assert!(
            (origin.y - caret.y0).abs() < 0.1,
            "numbered prefix and shaped item share a baseline band"
        );
        assert!(origin.x < line_rect.x0);
    }

    fn high_text_checklist_and_code_rows_leave_full_scaled_line_bands() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::checklist(0, false, "Ship the high-text layout"),
            Block::code_block(Some("rust".into()), "let scale = 2;"),
        ]);
        let theme = Theme::dark().with_text_scale(2.0);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = editor.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
        editor.apply_layout(&taffy, node, Point::ZERO);

        let checklist = editor.blocks[0].line.get_rect();
        let code = editor.blocks[1].line.get_rect();
        // A row holds its text at its role's leading, scaled with the text.
        let band = |kind: BlockKind| {
            let style = crate::tokens::typography::stock().style(kind.text_role());
            let leading = f64::from(style.line_height_px() / style.size).max(1.0);
            f64::from(kind.font_size()) * 2.0 * leading - 0.01
        };
        assert!(
            checklist.height() >= band(BlockKind::Paragraph),
            "2× checklist text needs its full scaled line band"
        );
        assert!(
            code.height() >= band(BlockKind::CodeBlock { lang: None }),
            "2× code text needs its full scaled line band"
        );
        assert!(checklist.y1 <= code.y0, "scaled rows must not collide");
        assert!(code.x0 >= prefix_gutter_for(&editor.blocks[1].kind) * 2.0);
    }

    fn font_size_per_kind() {
        assert!(BlockKind::Heading(1).font_size() > BlockKind::Heading(2).font_size());
        assert!(BlockKind::Heading(2).font_size() > BlockKind::Heading(3).font_size());
        assert!(BlockKind::Heading(3).font_size() > BlockKind::Paragraph.font_size());
    }

    code_block_cells_render_monospace();
    heading_paragraph_inline_code_and_numbered_prefix_scale_once();
    high_text_checklist_and_code_rows_leave_full_scaled_line_bands();
    font_size_per_kind();
}

fn layout_and_draw_editor(editor: &mut BlockEditor, theme: &Theme) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = editor.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(640.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .expect("block editor layout");
    editor.apply_layout(&taffy, node, Point::ZERO);
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(theme.text_scale);
    editor.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut renderer,
        theme,
        &PresenceCtx::empty(),
    );
}

#[test]
fn rich_link_hits_track_drawn_shaped_and_bidi_ranges() {
    fn rich_link_hits_follow_current_draw_on_first_frame_style_and_scale_changes() {
        let target = PageId::from_u128(42);
        let mut block = Block::paragraph("WWW end");
        block
            .line
            .set_marks(vec![MarkSpan::new(0..3, Mark::Link(target))]);
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![block]);
        for (scale, bold) in [(1.0, false), (1.0, true), (2.0, true), (1.0, false)] {
            let mut marks = vec![MarkSpan::new(0..3, Mark::Link(target))];
            if bold {
                marks.push(MarkSpan::new(0..3, Mark::Bold));
            }
            editor.blocks[0].line.set_marks(marks);
            let theme = Theme::dark().with_text_scale(scale);
            layout_and_draw_editor(&mut editor, &theme);
            let line = &editor.blocks[0].line;
            let right = line.pixel_x_for_byte(&theme, 3);
            let y = line.get_rect().center().y;
            assert_eq!(
                editor.link_at_point(Point::new(right - 0.1, y)),
                Some(LinkTarget::Page(target)),
                "the final painted sliver must link on this frame at {scale}x, bold={bold}"
            );
            assert_eq!(editor.link_at_point(Point::new(right + 0.1, y)), None);
        }
    }

    fn rich_link_hits_cover_the_shaped_rtl_range() {
        let target = PageId::from_u128(43);
        let mut block = Block::paragraph("אבג rest");
        block.line.set_marks(vec![
            MarkSpan::new(0..6, Mark::Link(target)),
            MarkSpan::new(0..6, Mark::Bold),
        ]);
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![block]);
        let theme = Theme::dark();
        layout_and_draw_editor(&mut editor, &theme);
        let band = editor.blocks[0].line.selection_rect(&theme, 0, 6);
        assert_eq!(
            editor.link_at_point(band.center()),
            Some(LinkTarget::Page(target))
        );
    }

    fn rich_link_hits_exclude_the_gap_between_mixed_direction_segments() {
        let target = PageId::from_u128(44);
        let mut block = Block::paragraph("ab אבג cd");
        block.line.set_marks(vec![
            MarkSpan::new(0..2, Mark::Link(target)),
            MarkSpan::new(10..12, Mark::Link(target)),
            MarkSpan::new(0..12, Mark::Bold),
        ]);
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![block]);
        let theme = Theme::dark();
        layout_and_draw_editor(&mut editor, &theme);
        let line = &editor.blocks[0].line;
        let first = line.selection_rect(&theme, 0, 2);
        let last = line.selection_rect(&theme, 10, 12);
        assert_eq!(
            editor.link_at_point(first.center()),
            Some(LinkTarget::Page(target))
        );
        assert_eq!(
            editor.link_at_point(last.center()),
            Some(LinkTarget::Page(target))
        );
        let gap = line.selection_rect(&theme, 3, 9);
        assert!(gap.x0 >= first.x1 && gap.x1 <= last.x0);
        assert_eq!(
            editor.link_at_point(gap.center()),
            None,
            "unmarked Hebrew remains outside the link"
        );
    }

    rich_link_hits_follow_current_draw_on_first_frame_style_and_scale_changes();
    rich_link_hits_cover_the_shaped_rtl_range();
    rich_link_hits_exclude_the_gap_between_mixed_direction_segments();
}

#[test]
fn code_fences_copy_highlight_and_render_formulas() {
    fn code_copy_targets_are_bounded_and_copy_complete_runs() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::code_block(Some("rust".into()), "let a = 1;"),
            Block::code_block(Some("rust".into()), "let b = 2;"),
            Block::paragraph("outside"),
        ]);

        let target = BlockEditor::code_copy_target(&editor.blocks()[0]);
        assert_eq!(editor.resolve_code_copy_target(&target), Some((0, 1)));
        assert!(editor.copy_code_run(0, 1));
        assert_eq!(editor.copied_code_start, Some(0));
        assert!(!editor.copy_code_run(0, 2), "a run cannot include prose");
        assert_eq!(editor.resolve_code_copy_target("notes.code-copy:2:1"), None);
    }

    fn fenced_languages_get_ephemeral_syntax_roles() {
        let mut blocks = vec![Block::code_block(
            Some("rust".into()),
            "let answer = 42; // documented",
        )];
        assert_eq!(refresh_syntax_colors(&mut blocks), 1);
        assert_eq!(
            refresh_syntax_colors(&mut blocks),
            0,
            "an unchanged run keeps its colours"
        );
        let colors = blocks[0].line.transient_colors();
        assert!(
            colors.iter().any(|span| span.kind == TokenKind::Keyword),
            "the Rust grammar should classify `let`"
        );
        assert!(
            colors.iter().any(|span| span.kind == TokenKind::Number),
            "numeric literals should receive the theme's semantic number role"
        );
        assert!(
            colors.iter().any(|span| span.kind == TokenKind::Comment),
            "comments should remain distinct from executable code"
        );
    }

    fn formula_fence_reserves_preview_below_editable_source() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::code_block(Some("tex".into()), r"\frac{x^2+1}{\sqrt{y}}"),
            Block::paragraph("following paragraph"),
        ]);
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = editor.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
        editor.apply_layout(&taffy, node, Point::ZERO);

        let preview = editor.formula_preview_rects[0].expect("TeX preview geometry");
        assert!(preview.height() > 70.0);
        assert!(preview.y0 >= editor.blocks[0].line.get_rect().y1);
        assert!(editor.blocks[1].line.get_rect().y0 > preview.y1);
        assert!(editor.content_bottom() >= editor.blocks[1].line.get_rect().y1);
    }

    fn formula_mode_is_persisted_and_visual_edits_emit_native_source() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::code_block(
            Some("typst".into()),
            "vec(alpha, beta, gamma)",
        )]);
        let original = editor.blocks[0].text().to_string();
        editor.set_formula_mode(0, crate::math::FormulaMode::Visual);
        assert!(matches!(
            &editor.blocks[0].kind,
            BlockKind::CodeBlock { lang: Some(language) } if language == "typst visual"
        ));
        assert_eq!(editor.blocks[0].text(), original);
        editor.set_formula_mode(0, crate::math::FormulaMode::Source);
        assert_eq!(editor.blocks[0].text(), original);
        editor.set_formula_mode(0, crate::math::FormulaMode::Visual);

        let source = editor.blocks[0].text().to_string();
        assert_eq!(source, original);
        editor
            .visual_formulas
            .get_mut(&0)
            .unwrap()
            .editor
            .type_char('+');
        editor.begin_edit();
        editor.commit_visual_formula_edit(0, 0, crate::math::FormulaSyntax::Typst);
        assert!(editor.blocks[0].text().starts_with("vec("));
        assert!(editor.blocks[0].text().ends_with('+'));
        assert!(!editor.take_edit_ops().is_empty());
    }

    code_copy_targets_are_bounded_and_copy_complete_runs();
    fenced_languages_get_ephemeral_syntax_roles();
    formula_fence_reserves_preview_below_editable_source();
    formula_mode_is_persisted_and_visual_edits_emit_native_source();
}

#[test]
fn split_and_merge_blocks_move_the_caret_and_continue_lists() {
    fn new_has_one_empty_paragraph() {
        let e = BlockEditor::new();
        assert_eq!(e.blocks().len(), 1);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
        assert_eq!(e.blocks()[0].text(), "");
    }

    fn split_inside_text_produces_two_blocks() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("abcdefg");
        e.focused = Some(0);
        e.split_at_cursor(0, 3);
        assert_eq!(e.blocks().len(), 2);
        assert_eq!(e.blocks()[0].text(), "abc");
        assert_eq!(e.blocks()[1].text(), "defg");
        assert!(matches!(e.blocks()[1].kind, BlockKind::Paragraph));
        assert_eq!(e.focused_index(), Some(1));
        assert!(e.blocks()[1].line.cursor_at_start());
    }

    fn split_at_end_creates_empty_block_below() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("hello");
        e.focused = Some(0);
        e.split_at_cursor(0, 5);
        assert_eq!(e.blocks().len(), 2);
        assert_eq!(e.blocks()[0].text(), "hello");
        assert_eq!(e.blocks()[1].text(), "");
        assert_eq!(e.focused_index(), Some(1));
    }

    fn merge_joins_and_lands_cursor_at_join() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("abc");
        e.blocks.push(Block::paragraph("def"));
        e.focused = Some(1);
        e.merge_with_previous(1);
        assert_eq!(e.blocks().len(), 1);
        assert_eq!(e.blocks()[0].text(), "abcdef");
        // Caret lands at byte 3 — the join point.
        assert_eq!(e.blocks()[0].line.cursor_pos(), 3);
        assert_eq!(e.focused_index(), Some(0));
    }

    fn merge_at_index_zero_is_noop() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("abc");
        e.focused = Some(0);
        e.merge_with_previous(0);
        assert_eq!(e.blocks().len(), 1);
        assert_eq!(e.blocks()[0].text(), "abc");
    }

    fn split_bullet_continues_list_at_same_indent() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(2, "first");
        e.focused = Some(0);
        e.split_at_cursor(0, 5);
        assert_eq!(e.blocks().len(), 2);
        match &e.blocks()[1].kind {
            BlockKind::Bullet { indent } => assert_eq!(*indent, 2),
            k => panic!("expected Bullet at indent 2, got {k:?}"),
        }
    }

    fn split_checklist_creates_unchecked_followup() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::checklist(1, true, "first");
        e.focused = Some(0);
        e.split_at_cursor(0, 5);
        assert_eq!(e.blocks().len(), 2);
        match &e.blocks()[1].kind {
            BlockKind::Checklist { indent, done } => {
                assert_eq!(*indent, 1);
                assert!(!*done, "new checklist item should default to unchecked");
            }
            k => panic!("expected Checklist, got {k:?}"),
        }
    }

    new_has_one_empty_paragraph();
    split_inside_text_produces_two_blocks();
    split_at_end_creates_empty_block_below();
    merge_joins_and_lands_cursor_at_join();
    merge_at_index_zero_is_noop();
    split_bullet_continues_list_at_same_indent();
    split_checklist_creates_unchecked_followup();
}

#[test]
fn deleting_empty_blocks_shifts_focus_and_keeps_one_block() {
    fn delete_empty_mid_list_shifts_focus_back() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("first");
        e.blocks.push(Block::paragraph(""));
        e.blocks.push(Block::paragraph("third"));
        e.focused = Some(1);
        e.delete_empty(1);
        assert_eq!(e.blocks().len(), 2);
        assert_eq!(e.blocks()[0].text(), "first");
        assert_eq!(e.blocks()[1].text(), "third");
        // Focus walks back to the previous block.
        assert_eq!(e.focused_index(), Some(0));
        // ...with the caret at the end of "first".
        assert_eq!(e.blocks()[0].line.cursor_pos(), 5);
    }

    fn delete_empty_first_block_keeps_at_least_one() {
        let mut e = BlockEditor::new();
        e.focused = Some(0);
        e.delete_empty(0);
        assert_eq!(e.blocks().len(), 1);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
        assert_eq!(e.blocks()[0].text(), "");
    }

    fn focus_block_clamps_to_range() {
        let mut e = BlockEditor::new();
        e.blocks.push(Block::paragraph("second"));
        e.focus_block(99, CursorPlacement::End);
        assert_eq!(e.focused_index(), Some(1));
    }

    fn divider_is_not_editable() {
        assert!(!BlockKind::Divider.is_editable());
        assert!(BlockKind::Paragraph.is_editable());
        assert!(BlockKind::CodeBlock { lang: None }.is_editable());
    }

    delete_empty_mid_list_shifts_focus_back();
    delete_empty_first_block_keeps_at_least_one();
    focus_block_clamps_to_range();
    divider_is_not_editable();
}

#[test]
fn markdown_prefix_promotion_converts_only_prefixed_paragraphs() {
    fn promote_to_h1_from_hash_space() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("# x");
        e.focused = Some(0);
        let promoted = e.try_promote_to_heading(0);
        assert!(promoted);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(1)));
        assert_eq!(e.blocks()[0].text(), "x");
        assert_eq!(e.blocks()[0].line.cursor_pos(), 0);
    }

    fn promote_to_quote_strips_marker_and_keeps_typing_focus() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("> ")]);
        editor.focus_block(0, CursorPlacement::End);

        assert!(editor.try_promote_to_quote(0));
        assert!(matches!(editor.blocks()[0].kind, BlockKind::Quote));
        assert_eq!(editor.blocks()[0].text(), "");
        assert!(editor.blocks()[0].line.is_focused());
    }

    fn promote_to_h2_and_h3() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("## title");
        e.focused = Some(0);
        e.try_promote_to_heading(0);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(2)));
        assert_eq!(e.blocks()[0].text(), "title");

        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("### sub");
        e.try_promote_to_heading(0);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(3)));
        assert_eq!(e.blocks()[0].text(), "sub");
    }

    fn promote_does_nothing_without_prefix() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("hello");
        assert!(!e.try_promote_to_heading(0));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
    }

    fn promote_does_nothing_on_existing_heading() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::heading(1, "# hi");
        // Even though the text starts with "# ", we don't double-
        // promote a heading.
        assert!(!e.try_promote_to_heading(0));
    }

    promote_to_h1_from_hash_space();
    promote_to_quote_strips_marker_and_keeps_typing_focus();
    promote_to_h2_and_h3();
    promote_does_nothing_without_prefix();
    promote_does_nothing_on_existing_heading();
}

#[test]
fn try_convert_changes_kind_preserving_text() {
    fn try_convert_paragraph_to_h1() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("hello");
        assert!(e.try_convert_to(0, BlockKind::Heading(1)));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(1)));
        assert_eq!(e.blocks()[0].text(), "hello");
    }

    fn try_convert_h1_to_bullet_preserves_text_resets_indent() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::heading(1, "task");
        assert!(e.try_convert_to(0, BlockKind::Bullet { indent: 0 }));
        match &e.blocks()[0].kind {
            BlockKind::Bullet { indent } => assert_eq!(*indent, 0),
            k => panic!("expected Bullet, got {k:?}"),
        }
        assert_eq!(e.blocks()[0].text(), "task");
    }

    fn try_convert_paragraph_to_numbered() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("step");
        assert!(e.try_convert_to(0, BlockKind::Numbered { indent: 0 }));
        assert!(matches!(
            e.blocks()[0].kind,
            BlockKind::Numbered { indent: 0 }
        ));
        assert_eq!(e.blocks()[0].text(), "step");
    }

    fn try_convert_bullet_to_paragraph() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(2, "item");
        assert!(e.try_convert_to(0, BlockKind::Paragraph));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
        assert_eq!(e.blocks()[0].text(), "item");
    }

    fn try_convert_to_same_kind_is_noop() {
        let mut e = BlockEditor::new();
        assert!(!e.try_convert_to(0, BlockKind::Paragraph));
    }

    fn try_convert_to_divider_appends_trailing_paragraph_when_focused() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("body");
        e.focused = Some(0);
        e.blocks[0].line.set_focus(true);
        e.is_focused = true;
        assert!(e.try_convert_to(0, BlockKind::Divider));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Divider));
        // Focus moved to the new paragraph spliced after the divider.
        assert_eq!(e.blocks().len(), 2);
        assert!(matches!(e.blocks()[1].kind, BlockKind::Paragraph));
        assert_eq!(e.focused_index(), Some(1));
    }

    try_convert_paragraph_to_h1();
    try_convert_h1_to_bullet_preserves_text_resets_indent();
    try_convert_paragraph_to_numbered();
    try_convert_bullet_to_paragraph();
    try_convert_to_same_kind_is_noop();
    try_convert_to_divider_appends_trailing_paragraph_when_focused();
}

#[test]
fn numbered_ordinals_count_reset_and_resume_within_runs() {
    fn numbered_ordinals_count_within_run() {
        let blocks = vec![
            Block::numbered(0, "a"),
            Block::numbered(0, "b"),
            Block::numbered(0, "c"),
        ];
        assert_eq!(numbered_ordinals(&blocks), vec![1, 2, 3]);
    }

    fn numbered_ordinals_reset_after_interruption() {
        let blocks = vec![
            Block::numbered(0, "a"),
            Block::numbered(0, "b"),
            Block::paragraph("break"),
            Block::numbered(0, "c"),
        ];
        // The paragraph breaks the run; the third item restarts at 1.
        assert_eq!(numbered_ordinals(&blocks), vec![1, 2, 0, 1]);
    }

    fn numbered_ordinals_nested_restart_and_resume() {
        let blocks = vec![
            Block::numbered(0, "one"),
            Block::numbered(1, "inner a"),
            Block::numbered(1, "inner b"),
            Block::numbered(0, "two"),
        ];
        // Outer: 1 … 2; inner level restarts at 1,2; outer resumes at 2.
        assert_eq!(numbered_ordinals(&blocks), vec![1, 1, 2, 2]);
    }

    numbered_ordinals_count_within_run();
    numbered_ordinals_reset_after_interruption();
    numbered_ordinals_nested_restart_and_resume();
}

#[test]
fn try_indent_steps_clamps_and_respects_block_kind() {
    fn try_indent_numbered_steps_up_and_down() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::numbered(0, "a");
        assert!(e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), 1);
        assert!(e.try_indent(0, -1));
        assert_eq!(e.blocks()[0].kind.indent(), 0);
    }

    fn try_indent_bullet_steps_up_and_down() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(0, "a");
        assert!(e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), 1);
        assert!(e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), 2);
        assert!(e.try_indent(0, -1));
        assert_eq!(e.blocks()[0].kind.indent(), 1);
    }

    fn try_indent_clamps_at_max_and_zero() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(MAX_LIST_INDENT, "deep");
        // +1 is clamped, returns false (no change).
        assert!(!e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), MAX_LIST_INDENT);

        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(0, "top");
        assert!(!e.try_indent(0, -1));
        assert_eq!(e.blocks()[0].kind.indent(), 0);
    }

    fn try_indent_on_paragraph_is_noop() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("body");
        assert!(!e.try_indent(0, 1));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
    }

    fn try_indent_paragraph_joins_the_preceding_outline() {
        let mut e = BlockEditor::new();
        e.set_blocks(vec![Block::numbered(0, "item"), Block::paragraph("detail")]);
        assert!(e.try_indent(1, 1));
        assert_eq!(e.blocks()[1].nesting, 1);
    }

    fn try_indent_on_checklist_preserves_done() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::checklist(0, true, "task");
        assert!(e.try_indent(0, 1));
        match &e.blocks()[0].kind {
            BlockKind::Checklist { indent, done } => {
                assert_eq!(*indent, 1);
                assert!(*done, "toggling indent must not change done");
            }
            k => panic!("expected Checklist, got {k:?}"),
        }
    }

    fn supports_indent_predicate() {
        assert!(BlockKind::Bullet { indent: 0 }.supports_indent());
        assert!(
            BlockKind::Checklist {
                indent: 0,
                done: false
            }
            .supports_indent()
        );
        assert!(!BlockKind::Paragraph.supports_indent());
        assert!(!BlockKind::Heading(1).supports_indent());
        assert!(!BlockKind::CodeBlock { lang: None }.supports_indent());
        assert!(!BlockKind::Divider.supports_indent());
    }

    fn parse_indent_two_spaces_per_level() {
        let (l, rest) = parse_list_indent("    item");
        assert_eq!(l, 2);
        assert_eq!(rest, "item");
        let (l, rest) = parse_list_indent("\t\titem");
        assert_eq!(l, 2);
        assert_eq!(rest, "item");
        let (l, rest) = parse_list_indent("no indent");
        assert_eq!(l, 0);
        assert_eq!(rest, "no indent");
    }

    try_indent_numbered_steps_up_and_down();
    try_indent_bullet_steps_up_and_down();
    try_indent_clamps_at_max_and_zero();
    try_indent_on_paragraph_is_noop();
    try_indent_paragraph_joins_the_preceding_outline();
    try_indent_on_checklist_preserves_done();
    supports_indent_predicate();
    parse_indent_two_spaces_per_level();
}

#[test]
fn try_toggle_check_flips_only_checklists() {
    fn try_toggle_check_flips_done_flag() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::checklist(0, false, "task");
        assert_eq!(e.try_toggle_check(0), Some(true));
        match &e.blocks()[0].kind {
            BlockKind::Checklist { done, .. } => assert!(*done),
            _ => panic!("expected Checklist"),
        }
        assert_eq!(e.try_toggle_check(0), Some(false));
    }

    fn try_toggle_check_on_paragraph_is_none() {
        let mut e = BlockEditor::new();
        assert_eq!(e.try_toggle_check(0), None);
    }

    try_toggle_check_flips_done_flag();
    try_toggle_check_on_paragraph_is_none();
}

#[test]
fn canonical_existing_list_restore_and_empty_baseline() {
    // Catches replacing live local state before malformed canonical shape is rejected.
    fn canonical_readonly_restore_rejects_shape_before_installing_editor() {
        let mut doc = AutoCommit::new();
        let list = doc
            .put_object(automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        doc.insert_object(&list, 0, ObjType::Map).unwrap();
        doc.commit();
        let before = (doc.base_heads(), doc.pending_ops());
        let mut editor = BlockEditor::from_markdown("retain me");
        editor.focus_block(0, CursorPlacement::Byte(3));
        let id = editor.blocks()[0].id.clone();
        assert!(editor.bind_existing_blocks(&doc, list).is_err());
        assert_eq!(editor.blocks()[0].text(), "retain me");
        assert_eq!(editor.blocks()[0].id, id);
        assert_eq!(editor.blocks()[0].line.cursor_pos(), 3);
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    }

    // Catch writes/replacement of the presentation paragraph on an empty read-only bind.
    fn canonical_existing_list_empty_noop_retains_caret_and_heads() {
        let mut doc = AutoCommit::new();
        let list = doc
            .put_object(automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        doc.commit();
        let heads = (doc.base_heads(), doc.pending_ops());
        let mut editor = BlockEditor::new();
        editor.bind_existing_blocks(&doc, list.clone()).unwrap();
        editor.set_focus(true);
        editor.focus_block(0, CursorPlacement::Start);
        let id = editor.blocks()[0].id.clone();
        for _ in 0..3 {
            editor.begin_edit();
            assert!(editor.take_edit_ops().is_empty());
            editor.sync(&doc, &automerge::ROOT);
            assert_eq!(editor.blocks()[0].id, id);
            assert_eq!(editor.blocks()[0].line.cursor_pos(), 0);
            assert_eq!(editor.focused_index(), Some(0));
            assert_eq!(doc.length(&list), 0);
            assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
        }
    }

    fn canonical_existing_list_returns_to_empty_baseline_after_removal() {
        let mut doc = AutoCommit::new();
        let (list, _) = seed_canonical_blocks(&mut doc, &[("paragraph", "old")]);
        doc.commit();
        let mut editor = BlockEditor::new();
        editor.bind_existing_blocks(&doc, list.clone()).unwrap();
        editor.set_focus(true);
        editor.focus_block(0, CursorPlacement::End);
        doc.delete(&list, 0).unwrap();
        doc.commit();
        let before = (doc.base_heads(), doc.pending_ops());
        editor.sync(&doc, &automerge::ROOT);
        let placeholder = editor.blocks()[0].id.clone();
        for _ in 0..3 {
            editor.begin_edit();
            assert!(editor.take_edit_ops().is_empty());
            editor.sync(&doc, &automerge::ROOT);
            assert_eq!(editor.blocks()[0].id, placeholder);
            assert_eq!(editor.focused_index(), Some(0));
            assert_eq!(editor.blocks()[0].line.cursor_pos(), 0);
            assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        }
        editor.begin_edit();
        editor.blocks_mut()[0].line.set_text("é");
        assert_eq!(
            editor.take_edit_ops(),
            vec![BlockEditOp::Insert {
                block_id: placeholder,
                after: None,
                kind: "paragraph".into(),
                text: "é".into(),
            }]
        );
        assert!(editor.take_edit_ops().is_empty());
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    }

    fn canonical_existing_list_nonempty_adoption_is_readonly() {
        let mut doc = AutoCommit::new();
        let (list, _) = seed_canonical_blocks(&mut doc, &[("paragraph", "retained")]);
        doc.commit();
        let heads = (doc.base_heads(), doc.pending_ops());
        let mut canonical = BlockEditor::new();
        canonical.bind_existing_blocks(&doc, list).unwrap();
        canonical.sync(&doc, &automerge::ROOT);
        assert_eq!(canonical.blocks()[0].text(), "retained");
        assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
    }

    canonical_readonly_restore_rejects_shape_before_installing_editor();
    canonical_existing_list_empty_noop_retains_caret_and_heads();
    canonical_existing_list_returns_to_empty_baseline_after_removal();
    canonical_existing_list_nonempty_adoption_is_readonly();
}

#[test]
fn canonical_existing_list_first_edits_preserve_every_insert() {
    fn canonical_existing_list_first_typing_inserts_once() {
        let mut doc = AutoCommit::new();
        let list = doc
            .put_object(automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        doc.commit();
        let heads = (doc.base_heads(), doc.pending_ops());
        let mut editor = BlockEditor::new();
        editor.bind_existing_blocks(&doc, list).unwrap();
        let id = editor.blocks()[0].id.clone();
        editor.begin_edit();
        editor.blocks_mut()[0].line.set_text("first é");
        assert_eq!(
            editor.take_edit_ops(),
            vec![BlockEditOp::Insert {
                block_id: id,
                after: None,
                kind: "paragraph".into(),
                text: "first é".into()
            }]
        );
        assert!(editor.take_edit_ops().is_empty());
        assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
    }

    fn canonical_existing_list_first_split_preserves_every_insert() {
        let mut doc = AutoCommit::new();
        let list = doc
            .put_object(automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        doc.commit();
        let heads = (doc.base_heads(), doc.pending_ops());
        let mut editor = BlockEditor::new();
        editor.bind_existing_blocks(&doc, list).unwrap();
        editor.begin_edit();
        editor.split_at_cursor(0, 0);
        let ids = editor
            .blocks()
            .iter()
            .map(|b| b.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            editor.take_edit_ops(),
            vec![
                BlockEditOp::Insert {
                    block_id: ids[0].clone(),
                    after: None,
                    kind: "paragraph".into(),
                    text: String::new()
                },
                BlockEditOp::Insert {
                    block_id: ids[1].clone(),
                    after: Some(ids[0].clone()),
                    kind: "paragraph".into(),
                    text: String::new()
                },
            ]
        );
        assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
    }

    fn canonical_existing_list_first_multiblock_edit_keeps_nonempty_content() {
        let mut doc = AutoCommit::new();
        let list = doc
            .put_object(automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        doc.commit();
        let before = (doc.base_heads(), doc.pending_ops());
        let mut editor = BlockEditor::new();
        editor.bind_existing_blocks(&doc, list).unwrap();
        editor.begin_edit();
        editor.blocks_mut()[0].line.set_text("first ésecond");
        editor.blocks_mut()[0]
            .line
            .set_marks(vec![MarkSpan::new(0.."first ésecond".len(), Mark::Bold)]);
        editor.split_at_cursor(0, "first é".len());
        assert_eq!(
            editor.blocks()[0].line.marks(),
            &[MarkSpan::new(0.."first é".len(), Mark::Bold)]
        );
        assert_eq!(
            editor.blocks()[1].line.marks(),
            &[MarkSpan::new(0.."second".len(), Mark::Bold)]
        );
        let ids = editor
            .blocks()
            .iter()
            .map(|block| block.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            editor.take_edit_ops(),
            vec![
                BlockEditOp::Insert {
                    block_id: ids[0].clone(),
                    after: None,
                    kind: "paragraph".into(),
                    text: "first é".into()
                },
                BlockEditOp::Insert {
                    block_id: ids[1].clone(),
                    after: Some(ids[0].clone()),
                    kind: "paragraph".into(),
                    text: "second".into()
                },
            ]
        );
        assert!(editor.take_edit_ops().is_empty());
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    }

    fn canonical_object_projection_preserves_text_identity_and_ignores_storage_order() {
        let mut doc = AutoCommit::new();
        let (list, _) =
            seed_canonical_blocks(&mut doc, &[("paragraph", "first"), ("paragraph", "second")]);
        let first = list_map(&doc, &list, 0).unwrap();
        let second = list_map(&doc, &list, 1).unwrap();
        let first_text = doc_text_obj(&doc, &list, 0).unwrap();
        let second_text = doc_text_obj(&doc, &list, 1).unwrap();
        doc.commit();
        let heads = (doc.base_heads(), doc.pending_ops());
        let mut editor = BlockEditor::new();
        editor
            .bind_existing_block_objects(&doc, &[second, first.clone()])
            .unwrap();
        editor.focus_block(1, CursorPlacement::Byte(3));
        editor.sync(&doc, &automerge::ROOT);
        assert_eq!(
            editor.blocks().iter().map(Block::text).collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert_eq!(editor.blocks()[0].line.bound_obj(), Some(second_text));
        assert_eq!(editor.blocks()[1].line.bound_obj(), Some(first_text));
        assert_eq!(editor.blocks()[1].line.cursor_pos(), 3);
        assert_eq!((doc.base_heads(), doc.pending_ops()), heads);
        // Invalid projections must not replace the last usable editor state.
        assert!(
            editor
                .bind_existing_block_objects(&doc, &[first.clone(), first])
                .is_err()
        );
        assert_eq!(editor.blocks()[0].text(), "second");
        assert_eq!(editor.blocks()[1].line.cursor_pos(), 3);
    }

    canonical_existing_list_first_typing_inserts_once();
    canonical_existing_list_first_split_preserves_every_insert();
    canonical_existing_list_first_multiblock_edit_keeps_nonempty_content();
    canonical_object_projection_preserves_text_identity_and_ignores_storage_order();
}

#[test]
fn action_bridge_emits_uuid_ops_at_unicode_boundaries() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("café")]);
    let id = editor.blocks()[0].id.clone();
    editor.begin_edit();
    editor.blocks_mut()[0].line.set_text("cafe");
    let ops = editor.take_edit_ops();
    assert_eq!(
        ops,
        vec![BlockEditOp::TextSplice {
            block_id: id,
            start: 3,
            end: 5,
            text: "e".into(),
        }]
    );
}

#[test]
fn hidden_find_controls_do_not_extend_content_and_reopen_after_layout() {
    fn layout(editor: &mut BlockEditor) -> f32 {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = HashMap::new();
        let node = editor.build_layout(&mut taffy, &theme, &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .expect("editor layout");
        editor.apply_layout(&taffy, node, Point::ZERO);
        taffy
            .layout(node)
            .expect("editor content")
            .content_size
            .height
    }

    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("Short draft")]);
    let before = editor.to_markdown();
    let hidden_height = layout(&mut editor);
    assert!(
        hidden_height < 44.0,
        "hidden search must not add its overlay height"
    );
    let _ = editor.take_structure_dirty();
    editor.open_find();
    assert!(
        editor.take_structure_dirty(),
        "opening search rebuilds its layout"
    );
    assert!(layout(&mut editor) >= 44.0);
    assert!(editor.find_field.get_rect().height() > 0.0);
    editor.close_find();
    assert!(
        editor.take_structure_dirty(),
        "closing search removes its layout"
    );
    let closed_height = layout(&mut editor);
    assert!((closed_height - hidden_height).abs() < f32::EPSILON);
    assert_eq!(editor.to_markdown(), before);
    assert!(editor.take_edit_ops().is_empty());
}

#[test]
fn find_matches_unicode_safely_and_navigates_without_editing() {
    fn find_ranges_are_case_insensitive_and_unicode_byte_safe() {
        assert_eq!(
            case_folded_match_ranges("İstanbul CAFÉ café", "i\u{307}"),
            vec![(0, "İ".len())]
        );
        assert_eq!(
            case_folded_match_ranges("İstanbul CAFÉ café", "CAFÉ"),
            vec![(10, 15), (16, 21)]
        );
    }

    fn find_moves_across_blocks_wraps_and_never_edits_content() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("alpha beta alpha"),
            Block::paragraph("ALPHA"),
        ]);
        let before = editor.to_markdown();
        editor.focus_block(0, CursorPlacement::Byte(5));
        editor.find_field.set_query("alpha");
        editor.refresh_find_matches(true);

        assert_eq!(editor.find_matches.len(), 3);
        assert_eq!(editor.find_active, Some(1));
        assert_eq!(editor.focused_index(), Some(0));
        assert_eq!(editor.blocks()[0].line.selection_range(), Some((11, 16)));

        editor.step_find(false);
        assert_eq!(editor.focused_index(), Some(1));
        assert_eq!(editor.blocks()[1].line.selection_range(), Some((0, 5)));
        editor.step_find(false);
        assert_eq!(editor.focused_index(), Some(0));
        assert_eq!(editor.blocks()[0].line.selection_range(), Some((0, 5)));
        editor.step_find(true);
        assert_eq!(editor.focused_index(), Some(1));
        assert_eq!(editor.to_markdown(), before);
        assert!(editor.take_edit_ops().is_empty());
    }

    find_ranges_are_case_insensitive_and_unicode_byte_safe();
    find_moves_across_blocks_wraps_and_never_edits_content();
}

fn presence_paths(editor: &mut BlockEditor, presence: &PresenceCtx) -> usize {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = editor.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(640.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .expect("block editor layout");
    editor.apply_layout(&taffy, node, Point::ZERO);
    let mut renderer = TextRenderer::new();
    let mut scene = Scene::new();
    editor.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        &theme,
        presence,
    );
    scene.encoding().path_tags.len()
}

/// A remote caret names its block by the stable block id: after a block
/// is inserted above it (locally or by a merge), the caret still lands on
/// the same block, and an id this editor does not have draws nothing.
#[test]
fn remote_caret_follows_its_block_id_across_inserted_blocks() {
    let mut ed = BlockEditor::new();
    ed.set_blocks(vec![
        Block::paragraph("alpha"),
        Block::paragraph("target text"),
    ]);
    let target = ed.blocks[1].id.clone();
    ed.focus_block(1, CursorPlacement::Byte(4));
    let Some((published, _)) = crate::presence::capability::presence_payload(&ed) else {
        panic!("focused block publishes a caret");
    };
    assert_eq!(published, block_presence_key(&target));
    // The receiver has an extra block above the target.
    let mut receiver = BlockEditor::new();
    let mut blocks = vec![Block::paragraph("inserted above")];
    for block in &ed.blocks {
        let mut copy = Block::paragraph(block.line.value());
        copy.id = block.id.clone();
        blocks.push(copy);
    }
    receiver.set_blocks(blocks);
    assert_eq!(receiver.blocks[2].id, target);
    let empty = presence_paths(&mut receiver, &PresenceCtx::empty());
    let mut presence = PresenceCtx::empty();
    presence.set_caret(&published, crate::permissions::UserId("peer".into()), 4);
    assert!(
        presence_paths(&mut receiver, &presence) > empty,
        "the caret is drawn"
    );
    let mut stranger = PresenceCtx::empty();
    stranger.set_caret(
        &block_presence_key("missing"),
        crate::permissions::UserId("peer".into()),
        4,
    );
    assert_eq!(presence_paths(&mut receiver, &stranger), empty);
}

#[test]
fn remote_document_selection_resolves_from_its_start_block_id() {
    let mut ed = BlockEditor::new();
    ed.set_blocks(vec![
        Block::paragraph("zero"),
        Block::paragraph("one"),
        Block::paragraph("two"),
        Block::paragraph("three"),
    ]);
    let start = ed.blocks[1].id.clone();
    let mut presence = PresenceCtx::empty();
    let user = crate::permissions::UserId("peer".into());
    presence.set_selection(
        &document_selection_presence_key(&start, 1),
        user.clone(),
        crate::presence::SelectionRange { start: 1, end: 2 },
    );
    let resolved = ed.resolve_document_selections(&presence);
    assert_eq!(resolved.len(), 1);
    assert_eq!((resolved[0].0, resolved[0].1), (1, 2));
    // A block inserted above shifts the indices, not the anchor.
    ed.blocks.insert(0, Block::paragraph("new"));
    let resolved = ed.resolve_document_selections(&presence);
    assert_eq!((resolved[0].0, resolved[0].1), (2, 3));
    // A span past the end is clamped; an unknown start is ignored.
    presence.set_selection(
        &document_selection_presence_key(&start, 40),
        crate::permissions::UserId("p2".into()),
        crate::presence::SelectionRange { start: 0, end: 1 },
    );
    presence.set_selection(
        &document_selection_presence_key("gone", 1),
        crate::permissions::UserId("p3".into()),
        crate::presence::SelectionRange { start: 0, end: 1 },
    );
    drop(user);
    let mut spans = ed
        .resolve_document_selections(&presence)
        .into_iter()
        .map(|row| (row.0, row.1))
        .collect::<Vec<_>>();
    spans.sort_unstable();
    assert_eq!(spans, vec![(2, 3), (2, 4)]);
}

#[test]
fn focused_solid_caret_settles_after_paint_and_focus_changes_redraw() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("stable caret")]);
    editor.set_focus(true);
    assert!(editor.needs_redraw());
    layout_and_draw_editor(&mut editor, &Theme::dark());
    editor.mark_drawn();
    assert!(
        !editor.needs_redraw(),
        "a painted solid caret needs no idle redraw"
    );
    editor.set_focus(false);
    assert!(editor.needs_redraw(), "blur must repaint the caret");
    layout_and_draw_editor(&mut editor, &Theme::dark());
    editor.mark_drawn();
    assert!(!editor.needs_redraw());
    editor.set_focus(true);
    assert!(editor.needs_redraw(), "refocus must repaint the caret");
}

#[test]
fn focused_block_ime_repaints_after_an_idle_paint() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("text")]);
    editor.set_focus(true);
    layout_and_draw_editor(&mut editor, &Theme::dark());
    editor.mark_drawn();
    let handled = dispatch(
        &mut editor,
        &WindowEvent::Ime(winit::event::Ime::Preedit("candidate".into(), Some((0, 9)))),
    );
    assert!(handled);
    assert!(
        editor.needs_redraw(),
        "IME changes must repaint without permanent focus demand"
    );
    assert_eq!(
        editor.blocks()[0].text(),
        "text",
        "preedit must remain uncommitted"
    );
    layout_and_draw_editor(&mut editor, &Theme::dark());
    editor.mark_drawn();
    assert!(!editor.needs_redraw());
}

#[test]
fn toolbar_commands_format_the_focused_block() {
    let mut e = BlockEditor::new();
    e.set_blocks(vec![Block::paragraph("hello world")]);
    assert!(!e.toggle_mark(Mark::Bold), "nothing is focused");
    e.focus_block(0, CursorPlacement::End);
    assert_eq!(e.focused_kind(), Some(&BlockKind::Paragraph));

    // With nothing selected, a mark waits for the text typed next.
    assert!(e.toggle_mark(Mark::Bold));
    assert_eq!(e.active_marks(), [Mark::Bold]);
    assert!(e.toggle_mark(Mark::Bold));
    assert!(e.active_marks().is_empty());

    // Over a selection, it marks the selection.
    e.blocks[0].line.inner_mut().set_selection(0, 5);
    assert!(e.toggle_mark(Mark::Italic));
    assert_eq!(
        e.blocks[0].line.marks(),
        [MarkSpan::new(0..5, Mark::Italic)]
    );
    assert_eq!(e.active_marks(), [Mark::Italic]);

    // A link over the selection, then one inserted at the caret.
    assert!(e.insert_link("https://example.test"));
    assert!(e.blocks[0].line.marks().contains(&MarkSpan::new(
        0..5,
        Mark::ExternalLink("https://example.test".into())
    )));
    e.blocks[0].line.set_cursor_pos(11);
    assert!(!e.insert_link("  "), "a blank target links nothing");
    assert!(e.insert_link("https://b.test"));
    assert_eq!(e.blocks[0].line.value(), "hello worldhttps://b.test");
    assert!(e.blocks[0].line.marks().contains(&MarkSpan::new(
        11..25,
        Mark::ExternalLink("https://b.test".into())
    )));
}

#[test]
fn a_mention_replaces_its_typed_query_as_one_marked_run() {
    let mut e = BlockEditor::new();
    e.set_blocks(vec![Block::paragraph("hi @ad")]);
    e.focus_block(0, CursorPlacement::End);
    assert_eq!(e.text_before_caret(), Some("hi @ad"));
    e.select_before_caret("@ad".len());
    assert!(e.insert_mention("u-ada", "@Ada"));
    assert_eq!(e.blocks[0].line.value(), "hi @Ada ");
    assert_eq!(
        e.blocks[0].line.marks(),
        [MarkSpan::new(3..7, Mark::Mention("u-ada".into()))]
    );
    assert_eq!(
        e.blocks[0].line.cursor_pos(),
        8,
        "the caret is after the space"
    );
    assert!(e.insert_text("🎉"));
    assert_eq!(e.blocks[0].line.value(), "hi @Ada 🎉");
    assert_eq!(
        e.blocks[0].line.marks(),
        [MarkSpan::new(3..7, Mark::Mention("u-ada".into()))],
        "text typed after a mention does not join it"
    );
    e.set_read_only(true);
    assert!(!e.insert_text("x"), "a read-only editor takes nothing");
}

#[test]
fn visual_formula_cut_waits_for_clipboard_acceptance() {
    use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[derive(Default)]
    struct Clipboard {
        accept: AtomicBool,
        stored: InProcessClipboard,
    }
    impl ClipboardBackend for Clipboard {
        fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
            self.accept.load(Ordering::Relaxed) && self.stored.set_text(target, text)
        }
        fn text(&self, target: ClipboardTarget) -> Option<String> {
            self.stored.text(target)
        }
        fn name(&self) -> &'static str {
            "formula-test-clipboard"
        }
    }
    let clipboard = Arc::new(Clipboard::default());
    let services = crate::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = crate::host::enter(&services);
    let mut editor = crate::math::edit::Editor::from_linear("x + y");
    editor.select_all();
    let before = editor.to_linear();
    let selected = editor.selected_linear();
    assert!(selected.is_some());
    let key = Key::Character("x".into());
    let modifiers = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    assert!(!handle_visual_formula_key(&mut editor, &key, modifiers));
    assert_eq!(editor.to_linear(), before);
    assert_eq!(editor.selected_linear(), selected);
    clipboard.accept.store(true, Ordering::Relaxed);
    assert!(handle_visual_formula_key(&mut editor, &key, modifiers));
    assert_eq!(clipboard.text(ClipboardTarget::Clipboard), selected);
    assert!(editor.to_linear().is_empty());
}

#[test]
fn a_block_fields_suggestion_timer_wakes_the_editor() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("so beautifull")]);
    assert_eq!(Widget::next_wake_deadline(&editor), None);

    // Far enough ahead that a slow test run never sees the deadline pass.
    let now = crate::time::now() + crate::time::Duration::from_secs(3600);
    let field = editor.blocks[0].line.inner_mut();
    Widget::set_focus(field, true);
    field.set_cursor_pos("so beautifull".len());
    field.refresh_suggestion_at(now);

    // Block lines are not widget children; their timers still reach the host.
    assert_eq!(
        Widget::next_wake_deadline(&editor),
        Some(now + crate::text_edit::SUGGESTION_SETTLE)
    );
}

#[test]
fn deferred_input_document_tracks_identical_blocks_cross_block_selection_and_read_only() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("same"), Block::paragraph("same")]);
    editor.set_focus(true);
    editor.focus_block(0, CursorPlacement::End);
    let first = editor.ime_edit_fingerprint(&[8; 32]).unwrap();
    editor.focus_block(1, CursorPlacement::End);
    let second = editor.ime_edit_fingerprint(&[8; 32]).unwrap();
    assert_ne!(
        first, second,
        "identical paragraphs have distinct insertion targets"
    );
    editor.select_document_range(
        DocumentPosition { block: 0, byte: 0 },
        DocumentPosition { block: 1, byte: 4 },
    );
    let selection = editor.ime_edit_fingerprint(&[8; 32]).unwrap();
    assert_ne!(selection, second);
    editor.select_document_range(
        DocumentPosition { block: 1, byte: 0 },
        DocumentPosition { block: 1, byte: 4 },
    );
    assert_ne!(
        editor.ime_edit_fingerprint(&[8; 32]).unwrap(),
        selection,
        "changing the document selection anchor must invalidate at an unchanged caret"
    );
    editor.set_read_only(true);
    assert!(editor.ime_edit_fingerprint(&[8; 32]).is_none());
}

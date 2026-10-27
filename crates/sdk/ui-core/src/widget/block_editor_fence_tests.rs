use super::*;
use crate::testing::Dispatch;
use automerge::ROOT;

#[test]
fn code_fence_chrome_copy_feedback_and_header_layout() {
    fn code_copy_feedback_follows_identity_and_expires_after_draw() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::code_block(Some("python".into()), "print(1)")]);
        let mut tree = TaffyTree::new();
        let theme = Theme::dark();
        let node = editor.build_layout(&mut tree, &theme, &mut HashMap::new());
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(360.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .unwrap();
        editor.apply_layout(&tree, node, Point::ZERO);
        let mut text = TextRenderer::new();
        editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        assert!(editor.copy_code_run(0, 0));
        assert_eq!(crate::clipboard_paste().as_deref(), Some("print(1)"));
        let id = editor.blocks[0].id.clone();
        assert_eq!(
            editor.copied_code_feedback.as_ref().map(|(id, _)| id),
            Some(&id)
        );
        assert!(editor.needs_animation_frame());
        editor.copied_code_feedback = Some((
            id,
            crate::time::now()
                .checked_sub(crate::time::Duration::from_secs(3))
                .expect("three seconds is within Instant's representable past"),
        ));
        editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        assert!(editor.copied_code_feedback.is_none());
        assert!(!editor.needs_animation_frame());
    }

    fn code_chrome_has_its_own_header_above_long_narrow_source_lines() {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut editor = BlockEditor::new();
            editor.set_blocks(vec![
                Block::code_block(
                    Some("python".into()),
                    "print('A long first line reaching the language badge')",
                ),
                Block::code_block(Some("python".into()), "print('second line')"),
                Block::code_block(Some("rust".into()), "let another_run = true;"),
                Block::paragraph("After code"),
            ]);
            let mut tree = TaffyTree::new();
            let node = editor.build_layout(&mut tree, &theme, &mut HashMap::new());
            tree.compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(260.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
            editor.apply_layout(&tree, node, Point::ZERO);
            editor.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
            assert_eq!(editor.code_copy_hits.len(), 2);
            for (copy, start, _) in &editor.code_copy_hits {
                let source = editor.blocks[*start].line.get_rect();
                let painted = editor.block_paint_rect(*start).unwrap();
                assert!(
                    copy.y1 <= source.y0,
                    "copy and language header must never overlay source"
                );
                assert!(
                    painted.y0 <= copy.y0,
                    "drag/hover bounds must include code header"
                );
                assert!(
                    (source.y0 - painted.y0 - CODE_HEADER_HEIGHT * f64::from(scale)).abs() < 0.1
                );
            }
            let first = editor.blocks[0].line.get_rect();
            let second = editor.blocks[1].line.get_rect();
            assert!(
                (first.y1 - second.y0).abs() < 0.1,
                "continuation lines do not get repeated headers"
            );
            assert!(
                editor.block_paint_rect(2).unwrap().y0 >= second.y1,
                "a new language's header must not overlap the previous code run"
            );
            assert!(editor.blocks[3].line.get_rect().y0 >= editor.blocks[2].line.get_rect().y1);
        }
    }

    code_copy_feedback_follows_identity_and_expires_after_draw();
    code_chrome_has_its_own_header_above_long_narrow_source_lines();
}

fn send(editor: &mut BlockEditor, doc: &mut AutoCommit, key: Key) {
    let event = crate::synthetic_input::key_event(
        key.clone(),
        match &key {
            Key::Character(text) => Some(text.clone()),
            _ => None,
        },
        ElementState::Pressed,
    );
    let handled = Dispatch::new()
        .doc(doc)
        .send(editor, &crate::synthetic_input::keyboard_input(event))
        .handled;
    assert!(handled);
    editor.sync(doc, &ROOT);
}

fn type_text(editor: &mut BlockEditor, doc: &mut AutoCommit, text: &str) {
    for character in text.chars() {
        send(editor, doc, Key::Character(character.to_string().into()));
    }
}

#[test]
fn typed_fences_promote_track_language_and_undo() {
    fn typed_fence_promotes_immediately_tracks_language_and_emits_body() {
        let mut doc = AutoCommit::new();
        let mut editor = BlockEditor::new();
        editor.focus_block(0, CursorPlacement::Start);
        let code_id = editor.blocks[0].id.clone();
        type_text(&mut editor, &mut doc, "```");
        assert_eq!(editor.blocks[0].kind, BlockKind::CodeBlock { lang: None });
        assert_eq!(editor.blocks[0].text(), "");
        type_text(&mut editor, &mut doc, "pythoX");
        send(&mut editor, &mut doc, Key::Named(NamedKey::Backspace));
        type_text(&mut editor, &mut doc, "n");
        assert_eq!(
            editor.blocks[0].kind,
            BlockKind::CodeBlock {
                lang: Some("python".into())
            }
        );
        send(&mut editor, &mut doc, Key::Named(NamedKey::Enter));
        assert_eq!(
            editor.blocks.len(),
            1,
            "opening fence must not insert a blank code line"
        );
        type_text(&mut editor, &mut doc, "print('hello')");
        send(&mut editor, &mut doc, Key::Named(NamedKey::Enter));
        type_text(&mut editor, &mut doc, "```");
        send(&mut editor, &mut doc, Key::Named(NamedKey::Enter));
        type_text(&mut editor, &mut doc, "After code");
        assert_eq!(editor.blocks.len(), 2);
        assert_eq!(editor.blocks[0].text(), "print('hello')");
        assert_eq!(editor.blocks[1].kind, BlockKind::Paragraph);
        assert_eq!(editor.blocks[1].text(), "After code");
        assert_eq!(editor.blocks[0].id, code_id, "the fence converts in place");
        let ops = editor.take_edit_ops();
        assert!(
            ops.contains(&BlockEditOp::SetKind {
                block_id: code_id.clone(),
                kind: "code:python".into(),
            }),
            "{ops:?}"
        );
        assert!(
            ops.contains(&BlockEditOp::TextSplice {
                block_id: code_id.clone(),
                start: 0,
                end: 0,
                text: "print('hello')".into(),
            }),
            "{ops:?}"
        );
        assert!(
            ops.contains(&BlockEditOp::Insert {
                block_id: editor.blocks[1].id.clone(),
                after: Some(code_id),
                kind: "paragraph".into(),
                text: "After code".into(),
            }),
            "{ops:?}"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, BlockEditOp::Delete { .. })),
            "{ops:?}"
        );
    }

    fn bare_fence_enter_starts_plain_code_and_backspace_can_undo_opener() {
        let mut doc = AutoCommit::new();
        let mut editor = BlockEditor::new();
        editor.focus_block(0, CursorPlacement::Start);
        type_text(&mut editor, &mut doc, "```");
        send(&mut editor, &mut doc, Key::Named(NamedKey::Backspace));
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(editor.blocks[0].text(), "``");
        type_text(&mut editor, &mut doc, "`");
        send(&mut editor, &mut doc, Key::Named(NamedKey::Enter));
        type_text(&mut editor, &mut doc, "plain code");
        assert_eq!(editor.blocks.len(), 1);
        assert_eq!(editor.blocks[0].text(), "plain code");
        assert_eq!(editor.blocks[0].kind, BlockKind::CodeBlock { lang: None });
    }

    fn complete_opener_in_one_input_emits_canonical_kind_and_does_not_eat_body() {
        let mut doc = AutoCommit::new();
        let mut editor = BlockEditor::new();
        editor.focus_block(0, CursorPlacement::Start);
        editor.begin_edit();
        send(&mut editor, &mut doc, Key::Character("```python".into()));
        assert!(
            editor
                .take_edit_ops()
                .iter()
                .any(|op| matches!(op, BlockEditOp::SetKind { kind, .. } if kind == "code:python"))
        );
        assert_eq!(editor.blocks[0].text(), "");
        editor.try_convert_to(0, BlockKind::Paragraph);
        type_text(&mut editor, &mut doc, "ordinary text");
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(editor.blocks[0].text(), "ordinary text");
    }

    fn fence_promotion_and_language_are_undo_checkpoints() {
        let mut doc = AutoCommit::new();
        let mut editor = BlockEditor::new();
        editor.focus_block(0, CursorPlacement::Start);
        let id = editor.blocks[0].id.clone();
        type_text(&mut editor, &mut doc, "``");
        editor.take_edit_ops();
        type_text(&mut editor, &mut doc, "`");
        assert_eq!(
            editor.take_edit_ops(),
            vec![
                BlockEditOp::SetKind {
                    block_id: id.clone(),
                    kind: "code:".into(),
                },
                BlockEditOp::TextSplice {
                    block_id: id.clone(),
                    start: 0,
                    end: 2,
                    text: String::new(),
                },
                BlockEditOp::SetCodeGroup {
                    block_id: id.clone(),
                    group_id: Some(id.clone()),
                },
            ],
            "the promotion reaches the owner as one checkpoint on the same block, \
             opening its own code group"
        );
        type_text(&mut editor, &mut doc, "python");
        assert_eq!(
            editor.take_edit_ops(),
            vec![BlockEditOp::SetKind {
                block_id: id,
                kind: "code:python".into(),
            }]
        );
    }

    typed_fence_promotes_immediately_tracks_language_and_emits_body();
    bare_fence_enter_starts_plain_code_and_backspace_can_undo_opener();
    complete_opener_in_one_input_emits_canonical_kind_and_does_not_eat_body();
    fence_promotion_and_language_are_undo_checkpoints();
}

use super::*;
use automerge::ROOT;

#[test]
fn slash_menu_and_tooltips_are_semantic_and_viewport_bounded() {
    fn visible_slash_menu_is_semantic_and_activation_uses_the_conversion_path() {
        fn find<'a>(
            node: &'a crate::a11y::AccessNode,
            key: &str,
        ) -> Option<&'a crate::a11y::AccessNode> {
            if node.semantic_key.as_deref() == Some(key) {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, key))
        }
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("")]);
        editor.focus_block(0, CursorPlacement::Start);
        editor.open_slash_menu_for_test();
        let tree = Widget::accessibility(&editor).unwrap();
        let menu = find(&tree, "notes.slash-menu").unwrap();
        assert_eq!(menu.role, crate::a11y::Role::Menu);
        assert_eq!(menu.state.expanded, Some(true));
        assert!(!menu.state.hidden);
        let heading = menu
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Heading 1"))
            .and_then(|node| node.action_target.as_deref())
            .unwrap()
            .to_owned();
        let mut doc = AutoCommit::new();
        assert!(editor.perform_semantic_action(
            &heading,
            &crate::a11y::Action::Activate,
            &mut doc,
            &ROOT,
        ));
        assert!(matches!(editor.blocks[0].kind, BlockKind::Heading(1)));
        assert!(!editor.slash_menu_visible());
    }

    fn tooltip_targets_respect_the_host_viewport_for_handles_and_code_controls() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("Partly scrolled behind the title")]);
        editor.blocks[0]
            .line
            .apply_layout_external(Rect::new(60.0, 0.0, 300.0, 24.0));
        let handle = editor.block_drag_handle_rect(0).unwrap();
        let viewport = Rect::new(0.0, handle.center().y, 300.0, 200.0);
        editor.set_tooltip_viewport(Some(viewport));
        assert!(
            editor
                .tooltip_target_at(Point::new(handle.center().x, handle.y0 + 1.0))
                .is_none()
        );
        let (visible_handle, _) = editor
            .tooltip_target_at(Point::new(handle.center().x, handle.y1 - 1.0))
            .unwrap();
        assert_eq!(visible_handle, handle.intersect(viewport));

        let copy = Rect::new(220.0, 0.0, 260.0, 24.0);
        editor.code_copy_hits.push((copy, 0, 0));
        assert!(editor.tooltip_target_at(Point::new(240.0, 1.0)).is_none());
        let (visible_copy, _) = editor.tooltip_target_at(Point::new(240.0, 23.0)).unwrap();
        assert_eq!(visible_copy, copy.intersect(viewport));
        editor.set_tooltip_viewport(Some(Rect::new(0.0, 50.0, 300.0, 200.0)));
        assert!(editor.tooltip_target_at(copy.center()).is_none());
        assert!(editor.tooltip_target_at(handle.center()).is_none());
        editor.set_tooltip_viewport(None);
        assert_eq!(editor.tooltip_target_at(copy.center()).unwrap().0, copy);
    }

    fn checklist_pointer_matches_the_actual_toggle_hit_target() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::checklist(0, false, "Task")]);
        editor.blocks[0]
            .line
            .apply_layout_external(Rect::new(60.0, 0.0, 300.0, 24.0));
        let point = Point::new(60.0 - prefix_gutter_for(&editor.blocks[0].kind) / 2.0, 12.0);
        assert_eq!(editor.checkbox_hit_test(point), Some(0));
        assert_eq!(
            editor.cursor_shape(point),
            Some(winit::window::CursorIcon::Pointer)
        );
    }

    visible_slash_menu_is_semantic_and_activation_uses_the_conversion_path();
    tooltip_targets_respect_the_host_viewport_for_handles_and_code_controls();
    checklist_pointer_matches_the_actual_toggle_hit_target();
}

fn type_text(editor: &mut BlockEditor, doc: &mut AutoCommit, text: &str) {
    for character in text.chars() {
        let mut key: KeyEvent = unsafe { std::mem::zeroed() };
        key.logical_key = Key::Character(character.to_string().into());
        key.text = Some(character.to_string().into());
        key.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        key.location = winit::keyboard::KeyLocation::Standard;
        key.state = ElementState::Pressed;
        let mut handled = false;
        editor.handle_event(
            &WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: key,
                is_synthetic: false,
            },
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: NodeId::new(0),
                taffy: &TaffyTree::new(),
            },
        );
        assert!(handled);
        editor.sync(doc, &ROOT);
    }
}

#[test]
fn typed_list_and_task_markers_promote_preserving_identity_and_depth() {
    fn typed_list_markers_promote_and_preserve_identity_through_reload() {
        for (marker, expected) in [
            ("- ", BlockKind::Bullet { indent: 0 }),
            ("* ", BlockKind::Bullet { indent: 0 }),
            ("+ ", BlockKind::Bullet { indent: 0 }),
            ("1. ", BlockKind::Numbered { indent: 0 }),
            ("1) ", BlockKind::Numbered { indent: 0 }),
            (
                "- [ ] ",
                BlockKind::Checklist {
                    indent: 0,
                    done: false,
                },
            ),
            (
                "- [x] ",
                BlockKind::Checklist {
                    indent: 0,
                    done: true,
                },
            ),
            (
                "[ ] ",
                BlockKind::Checklist {
                    indent: 0,
                    done: false,
                },
            ),
        ] {
            let mut doc = AutoCommit::new();
            let list = doc.put_object(ROOT, "blocks", ObjType::List).unwrap();
            let mut editor = BlockEditor::new();
            editor.bind_blocks(&mut doc, list.clone());
            editor.focus_block(0, CursorPlacement::Start);
            let id = editor.blocks[0].id.clone();
            type_text(&mut editor, &mut doc, marker);
            assert_eq!(editor.blocks[0].kind, expected, "{marker}");
            assert_eq!(editor.blocks[0].text(), "");
            let bound = editor.blocks[0].line.bound_obj();
            type_text(&mut editor, &mut doc, "Buy milk");
            assert_eq!(editor.blocks[0].text(), "Buy milk");
            assert_eq!(editor.blocks[0].id, id);
            assert_eq!(editor.blocks[0].line.bound_obj(), bound);
            let mut reloaded = BlockEditor::new();
            reloaded.bind_blocks(&mut doc, list);
            assert_eq!(reloaded.blocks[0].kind, expected);
            assert_eq!(reloaded.blocks[0].text(), "Buy milk");
            assert_eq!(reloaded.blocks[0].id, id);
        }
    }

    fn typed_task_conversion_preserves_nested_depth_and_literal_prose() {
        let mut doc = AutoCommit::new();
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::bullet(3, "")]);
        editor.focus_block(0, CursorPlacement::Start);
        type_text(&mut editor, &mut doc, "[ ] Nested task");
        assert_eq!(
            editor.blocks[0].kind,
            BlockKind::Checklist {
                indent: 3,
                done: false
            }
        );
        assert_eq!(editor.blocks[0].text(), "Nested task");

        editor.set_blocks(vec![Block::paragraph("A literal ")]);
        editor.focus_block(0, CursorPlacement::End);
        type_text(&mut editor, &mut doc, "- [ ] marker");
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(editor.blocks[0].text(), "A literal - [ ] marker");
    }

    typed_list_markers_promote_and_preserve_identity_through_reload();
    typed_task_conversion_preserves_nested_depth_and_literal_prose();
}

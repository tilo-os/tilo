//! Production keyboard routing across shaped rows and block boundaries.
use std::collections::HashMap;
use taffy::prelude::*;
use ui::presence::PresenceCtx;
use ui::theme::Theme;
use ui::widget::{BlockEditor, CursorPlacement, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[allow(
    clippy::struct_field_names,
    reason = "the fixture's editor field distinguishes the widget from its layout and renderer"
)]
struct Editor {
    editor: BlockEditor,
    tree: TaffyTree<()>,
    node: NodeId,
    renderer: ui::TextRenderer,
}

#[test]
fn arrow_navigation_never_emits_content_edits() {
    let mut fixture = Editor::new(
        "A paragraph long enough to wrap across several visual rows at this narrow width.\n\nAnother paragraph long enough to wrap as well.",
        155.0,
        0,
        3,
    );
    fixture.editor.set_action_interception(true);
    fixture.editor.take_structure_dirty();
    fixture.editor.is_dirty();
    for shift in [false, true] {
        for key in [NamedKey::ArrowDown, NamedKey::ArrowDown, NamedKey::ArrowUp] {
            fixture.editor.begin_action_intercept();
            fixture.key(key, shift);
            assert!(fixture.editor.take_intercepted_ops().is_empty());
            assert!(
                !fixture.editor.is_dirty(),
                "{key:?} must not report a content edit"
            );
        }
    }
}

impl Editor {
    fn new(markdown: &str, width: f32, block: usize, byte: usize) -> Self {
        let mut editor = BlockEditor::from_markdown(markdown).with_wrap_lines(true);
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = editor.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        let mut renderer = crate::component_support::renderer();
        tree.compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::MaxContent,
            },
            |known, available, node, _, _| {
                nodes.get(&node).map_or(Size::ZERO, |ptr| unsafe {
                    (**ptr).measure(known, available, &mut renderer)
                })
            },
        )
        .unwrap();
        editor.apply_layout(&tree, node, Point::new(50.0, 50.0));
        editor.set_focus(true);
        editor.focus_block(block, CursorPlacement::Byte(byte));
        let mut fixture = Self {
            editor,
            tree,
            node,
            renderer,
        };
        fixture.draw();
        fixture
    }

    fn draw(&mut self) {
        self.editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.renderer,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    fn key(&mut self, key: NamedKey, shift: bool) {
        // winit keeps platform fields private; this matches the existing
        // headless keyboard fixtures used throughout the integration suite.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Named(key);
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.text = None;
        let event = WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        };
        let modifiers = Modifiers::from(if shift {
            ModifiersState::SHIFT
        } else {
            ModifiersState::empty()
        });
        let mut handled = false;
        self.editor.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut automerge::AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
            &mut None,
            self.node,
            &self.tree,
        );
        assert!(handled, "editor must consume {key:?}");
        self.draw();
    }
}

#[test]
fn shift_arrows_keep_the_original_anchor_across_three_blocks_and_reverse() {
    let mut fixture = Editor::new("first\n\nsecond\n\nthird", 400.0, 0, 0);
    fixture.key(NamedKey::ArrowDown, true);
    fixture.key(NamedKey::ArrowDown, true);
    assert_eq!(
        fixture.editor.selected_text().as_deref(),
        Some("first\nsecond\n")
    );
    let selection = fixture.editor.document_selection().unwrap();
    assert_eq!((selection.anchor.block, selection.anchor.byte), (0, 0));
    assert_eq!((selection.cursor.block, selection.cursor.byte), (2, 0));
    fixture.key(NamedKey::ArrowUp, true);
    assert_eq!(fixture.editor.selected_text().as_deref(), Some("first\n"));
    fixture.key(NamedKey::ArrowUp, true);
    assert!(fixture.editor.document_selection().is_none());
}

#[test]
fn wrapped_shift_navigation_and_delete_use_the_same_document_range() {
    let first = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda";
    let second = "one two three four five six seven eight nine ten eleven twelve thirteen";
    let mut fixture = Editor::new(&format!("{first}\n\n{second}"), 155.0, 0, 3);
    for _ in 0..20 {
        if fixture.editor.focused_index() == Some(1) {
            break;
        }
        fixture.key(NamedKey::ArrowDown, true);
    }
    let boundary = fixture.editor.document_selection().unwrap();
    assert_eq!(boundary.cursor.block, 1, "must reach the next paragraph");
    assert_eq!((boundary.anchor.block, boundary.anchor.byte), (0, 3));
    fixture.key(NamedKey::ArrowDown, true);
    let selection = fixture.editor.document_selection().unwrap();
    assert_eq!((selection.anchor.block, selection.anchor.byte), (0, 3));
    assert_eq!(selection.cursor.block, 1);
    assert!(
        selection.cursor.byte > boundary.cursor.byte,
        "selection must advance inside the wrapped second paragraph"
    );
    assert_eq!(
        fixture.editor.selected_text().unwrap(),
        format!("{}\n{}", &first[3..], &second[..selection.cursor.byte])
    );
    fixture.key(NamedKey::Delete, false);
    assert_eq!(fixture.editor.blocks().len(), 1);
    assert_eq!(
        fixture.editor.blocks()[0].text(),
        format!("{}{}", &first[..3], &second[selection.cursor.byte..])
    );
}

#[test]
fn vertical_column_survives_a_short_block_in_both_directions() {
    let text = "abcdefghijklmnopqrstuv";
    let mut fixture = Editor::new(&format!("{text}\n\nx\n\n{text}"), 400.0, 0, 15);
    fixture.key(NamedKey::ArrowDown, false);
    assert_eq!(fixture.editor.focused_index(), Some(1));
    fixture.key(NamedKey::ArrowDown, false);
    assert_eq!(fixture.editor.focused_index(), Some(2));
    assert_eq!(fixture.editor.blocks()[2].line.cursor_pos(), 15);
    fixture.key(NamedKey::ArrowUp, false);
    fixture.key(NamedKey::ArrowUp, false);
    assert_eq!(fixture.editor.focused_index(), Some(0));
    assert_eq!(fixture.editor.blocks()[0].line.cursor_pos(), 15);
}

#[test]
fn extending_an_existing_local_selection_preserves_its_anchor() {
    let mut fixture = Editor::new("first\n\nsecond\n\nthird", 400.0, 0, 4);
    fixture.editor.blocks_mut()[0]
        .line
        .inner_mut()
        .set_selection(1, 4);
    fixture.key(NamedKey::ArrowDown, true);
    let selection = fixture.editor.document_selection().unwrap();
    assert_eq!((selection.anchor.block, selection.anchor.byte), (0, 1));
    assert_eq!(selection.cursor.block, 1);
    assert_eq!(
        fixture.editor.selected_text().unwrap(),
        format!("irst\n{}", &"second"[..selection.cursor.byte])
    );
}

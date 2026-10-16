//! Retain the actual layout cache between edits; a fresh tree hides stale heights.
use automerge::AutoCommit;
use std::collections::HashMap;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::bound_dispatch::BoundNodeMap;
use tilo_ui::widget::{Block, BlockEditor, CursorPlacement, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};

struct RetainedCode {
    editor: Box<BlockEditor>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: NodeId,
    text: tilo_ui::TextRenderer,
    theme: tilo_ui::Theme,
    doc: AutoCommit,
}

impl RetainedCode {
    fn new(scale: f32) -> Self {
        let mut editor = Box::new(BlockEditor::new().with_wrap_lines(true));
        editor.set_blocks(vec![
            Block::code_block(Some("python".into()), ""),
            Block::paragraph("Following paragraph"),
        ]);
        editor.focus_block(0, CursorPlacement::Start);
        let theme = tilo_ui::Theme::dark().with_text_scale(scale);
        let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
            tilo_ui::font_catalog::checkout_root(),
            "en-US",
        );
        let mut text = tilo_ui::TextRenderer::with_font_catalog(&catalog).unwrap();
        text.set_text_scale(scale);
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = editor.build_layout(&mut tree, &theme, &mut nodes);
        let mut bindings = BoundNodeMap::new();
        // SAFETY: this boxed editor owns every node and stays alive and in place.
        unsafe {
            bindings.rebuild(editor.as_ref(), &tree, root, &nodes);
        }
        editor.take_structure_dirty();
        let mut fixture = Self {
            editor,
            tree,
            nodes,
            bindings,
            root,
            text,
            theme,
            doc: AutoCommit::new(),
        };
        fixture.frame();
        fixture
    }

    fn frame(&mut self) {
        // SAFETY: no topology change is allowed between keystrokes in this test.
        unsafe {
            tilo_ui::widget::compute_widget_layout(
                &mut self.tree,
                self.root,
                Size {
                    width: AvailableSpace::Definite(280.0),
                    height: AvailableSpace::MaxContent,
                },
                &self.nodes,
                &self.bindings,
                &mut self.text,
            )
            .unwrap();
        }
        self.editor.apply_layout(&self.tree, self.root, Point::ZERO);
        self.editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &tilo_ui::presence::PresenceCtx::empty(),
        );
        self.editor.mark_drawn();
    }

    fn key(&mut self, key: Key) {
        let text = match &key {
            Key::Character(value) => Some(value.clone()),
            _ => None,
        };
        let event = KeyEvent::synthetic(
            RawKeyEvent {
                physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                state: ElementState::Pressed,
            },
            key.clone(),
            text.clone(),
            KeyLocation::Standard,
            false,
            text,
            key,
        );
        let mut handled = false;
        self.editor.handle_event(
            &WindowEvent::KeyboardInput {
                device_id: DeviceId::dummy(),
                event,
                is_synthetic: true,
            },
            &mut tilo_ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: tilo_ui::widget::Edits::lend(&mut self.doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: self.root,
                taffy: &self.tree,
            },
        );
        assert!(handled, "real editor must receive every keystroke");
        self.editor.is_dirty();
        assert!(
            !self.editor.take_structure_dirty(),
            "typing must work without rebuilding the layout tree"
        );
        self.frame();
    }
}

#[test]
fn wrapped_code_grows_without_rebuilding_or_clipping_the_caret() {
    for scale in [1.0, 2.0] {
        let mut fixture = RetainedCode::new(scale);
        let source = "message = \"Every character of this long source line must remain visible while it grows inside the code fence.\"";
        for character in source.chars() {
            fixture.key(Key::Character(character.to_string().into()));
        }
        assert_eq!(fixture.editor.blocks()[0].text(), source);
        let line = &fixture.editor.blocks()[0].line;
        let bounds = line.get_rect();
        assert!(bounds.height() >= line.inner().content_height() - 1.0);
        assert!(bounds.height() > 42.0 * f64::from(scale));
        let caret = line.caret_rect(&fixture.theme, line.cursor_pos());
        assert!(caret.y0 >= bounds.y0 && caret.y1 <= bounds.y1 + 1.0);
        assert!(caret.x0 >= bounds.x0 && caret.x1 <= bounds.x1 + 1.0);
        assert!(fixture.editor.blocks()[1].line.get_rect().y0 >= bounds.y1);
        fixture.key(Key::Named(NamedKey::ArrowUp));
        fixture.key(Key::Named(NamedKey::ArrowDown));
        assert_eq!(fixture.editor.blocks()[0].line.get_rect(), bounds);
        assert_eq!(fixture.editor.blocks()[0].text(), source);
    }
}

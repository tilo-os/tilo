//! Keep the production layout tree alive between keystrokes. Rebuilding it
//! would erase Taffy's intrinsic-size cache and conceal stale paragraph heights.
use super::*;
use crate::bound_dispatch::BoundNodeMap;
use winit::event::MouseButton;
use winit::keyboard::ModifiersState;

struct RetainedEditor {
    editor: Box<BlockEditor>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: NodeId,
    text: TextRenderer,
    theme: Theme,
    doc: AutoCommit,
    width: f32,
    rebuilds: usize,
    /// The host's paint viewport, as the live host pushes the window.
    viewport: Option<Rect>,
}

impl RetainedEditor {
    fn click(&mut self, point: Point) {
        let device_id = unsafe { std::mem::zeroed() };
        for event in [
            WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(point.x, point.y),
            },
            WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ] {
            self.editor.handle_event(
                &event,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &Modifiers::default(),
                    state: &mut self.doc,
                    path: &automerge::ROOT,
                    handled: &mut false,
                    focus_target: &mut None,
                    node_id: self.root,
                    taffy: &self.tree,
                },
            );
            self.frame();
        }
    }

    fn new(width: f32, scale: f32, image: bool) -> Self {
        let mut blocks = vec![
            Block::paragraph(""),
            Block::paragraph("Following paragraph"),
        ];
        if image {
            blocks.push(Block::paragraph("![An attachment](fixture.png)"));
        }
        Self::with_blocks(width, scale, blocks)
    }

    fn with_blocks(width: f32, scale: f32, blocks: Vec<Block>) -> Self {
        let mut editor = Box::new(BlockEditor::new().with_wrap_lines(true));
        editor.set_blocks(blocks);
        editor.focus_block(0, CursorPlacement::Start);
        let theme = Theme::dark().with_text_scale(scale);
        let mut text = crate::font_catalog::fixture_renderer();
        text.set_text_scale(scale);
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = editor.build_layout(&mut tree, &theme, &mut nodes);
        let mut bindings = BoundNodeMap::new();
        // SAFETY: the boxed editor and its blocks remain in place until rebuild.
        unsafe { bindings.rebuild(editor.as_ref(), &tree, root, &nodes) };
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
            width,
            rebuilds: 0,
            viewport: None,
        };
        fixture.frame();
        fixture
    }

    fn rebuild_after_structure_change(&mut self) {
        self.rebuilds = self.rebuilds.saturating_add(1);
        self.bindings.clear();
        self.nodes.clear();
        self.tree = TaffyTree::new();
        self.root = self
            .editor
            .build_layout(&mut self.tree, &self.theme, &mut self.nodes);
        // SAFETY: the old admission map was discarded before replacing nodes.
        unsafe {
            self.bindings
                .rebuild(self.editor.as_ref(), &self.tree, self.root, &self.nodes);
        }
        self.editor.take_structure_dirty();
    }

    fn frame(&mut self) {
        // SAFETY: this fixture exclusively owns the stable boxed editor and
        // matching maps; callbacks never replace blocks while measuring.
        unsafe {
            crate::widget::compute_widget_layout(
                &mut self.tree,
                self.root,
                Size {
                    width: taffy::AvailableSpace::Definite(self.width),
                    height: taffy::AvailableSpace::MaxContent,
                },
                &self.nodes,
                &self.bindings,
                &mut self.text,
            )
            .unwrap();
        }
        self.editor.apply_layout(&self.tree, self.root, Point::ZERO);
        let presence = PresenceCtx::empty();
        if let Some(viewport) = self.viewport {
            presence.push_viewport(viewport);
        }
        self.editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &presence,
        );
        self.editor.mark_drawn();
        for child in self.editor.children_mut() {
            child.mark_drawn();
        }
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.text = match &key {
            Key::Character(s) => Some(s.clone()),
            _ => None,
        };
        event.logical_key = key;
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        let mut handled = false;
        self.editor.handle_event(
            &WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event,
                is_synthetic: false,
            },
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut self.doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: self.root,
                taffy: &self.tree,
            },
        );
        assert!(handled, "editor must receive each test keystroke");
        // Notes publishes edits into its document before the next layout.
        // Draining edit notifications must not drain intrinsic-size dirtiness.
        self.editor.is_dirty();
        if self.editor.take_structure_dirty() {
            self.rebuild_after_structure_change();
        }
        self.frame();
    }

    fn type_text(&mut self, value: &str) {
        for ch in value.chars() {
            self.key(Key::Character(ch.to_string().into()), Modifiers::default());
        }
    }

    fn assert_focused_content_fits(&mut self) {
        let index = self.editor.focused_index().unwrap();
        let line = &mut self.editor.blocks[index].line;
        let rect = line.get_rect();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the finite on-screen width must enter Taffy's f32 coordinate space"
        )]
        let width = rect.width() as f32;
        let natural = line.measure(
            Size {
                width: Some(width),
                height: None,
            },
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut self.text,
        );
        assert!(
            rect.height() + 1.0 >= f64::from(natural.height),
            "retained row clips wrapped content: allocated {rect:?}, required {}",
            natural.height,
        );
        let caret = line.caret_rect(&self.theme, line.cursor_pos());
        assert!(caret.y0 >= rect.y0 && caret.y1 <= rect.y1 + 1.0);
        if let Some(next) = index
            .checked_add(1)
            .and_then(|next| self.editor.blocks.get(next))
        {
            assert!(
                next.line.get_rect().y0 >= rect.y1,
                "following block overlaps wrapped text"
            );
        }
    }
}

#[test]
fn retained_paragraph_layout_tracks_typing_reflow_clicks_and_attachments() {
    fn clicking_another_paragraph_places_caret_at_the_pointer_not_its_old_position() {
        for scale in [1.0, 2.0] {
            let mut fixture = RetainedEditor::new(280.0, scale, false);
            fixture.type_text(
                "The first paragraph has enough words to wrap across several visual lines.",
            );
            let old_position = fixture.editor.blocks[0].line.cursor_pos();
            let second = fixture.editor.blocks[1].line.caret_rect(&fixture.theme, 4);
            fixture.click(Point::new(second.x0 + 0.1, second.center().y));
            assert_eq!(fixture.editor.focused_index(), Some(1));
            assert_eq!(
                fixture.editor.blocks[1].line.cursor_pos(),
                4,
                "first click must use the current pointer, even on a previously unfocused line"
            );
            let target = 37;
            let first = fixture.editor.blocks[0]
                .line
                .caret_rect(&fixture.theme, target);
            assert!(first.y0 > fixture.editor.blocks[0].line.get_rect().y0 + 20.0);
            fixture.click(Point::new(first.x0 + 0.1, first.center().y));
            assert_eq!(fixture.editor.focused_index(), Some(0));
            assert_ne!(fixture.editor.blocks[0].line.cursor_pos(), old_position);
            assert_eq!(
                fixture.editor.blocks[0].line.cursor_pos(),
                target,
                "returning to a wrapped paragraph must hit-test the clicked visual row"
            );
        }
    }

    fn retained_paragraph_grows_and_shrinks_while_typing_without_rebuilding() {
        for scale in [1.0, 2.0] {
            for width in [240.0, 640.0] {
                let mut fixture = RetainedEditor::new(width, scale, false);
                let initial = fixture.editor.blocks[0].line.get_rect().height();
                let sentence =
                    "Let's go. Hi this is ada speaking. Why does pressing Enter not work? ";
                let content = sentence.repeat(3);
                fixture.type_text(&content);
                assert_eq!(
                    fixture.rebuilds, 0,
                    "typing must retain the existing layout tree"
                );
                fixture.assert_focused_content_fits();
                assert!(fixture.editor.blocks[0].line.get_rect().height() > initial * 2.0);
                for _ in content.chars() {
                    fixture.key(Key::Named(NamedKey::Backspace), Modifiers::default());
                }
                fixture.assert_focused_content_fits();
                assert!((fixture.editor.blocks[0].line.get_rect().height() - initial).abs() < 1.0);
                assert_eq!(
                    fixture.rebuilds, 0,
                    "deletion must retain the existing layout tree"
                );
            }
        }
    }

    fn retained_paragraph_reflows_after_enter_and_shift_enter() {
        for shift in [false, true] {
            let mut fixture = RetainedEditor::new(260.0, 1.0, false);
            fixture.type_text("First paragraph");
            let mods = if shift {
                ModifiersState::SHIFT.into()
            } else {
                Modifiers::default()
            };
            fixture.key(Key::Named(NamedKey::Enter), mods);
            let active = usize::from(!shift);
            assert_eq!(fixture.editor.focused_index(), Some(active));
            assert_eq!(fixture.editor.blocks.len(), if shift { 2 } else { 3 });
            if shift {
                assert_eq!(fixture.editor.blocks[0].text(), "First paragraph\n");
                assert_eq!(fixture.rebuilds, 0, "a soft break keeps the same block");
            }
            fixture.type_text(
                &"A continuation should grow visibly as each new line wraps. ".repeat(3),
            );
            fixture.assert_focused_content_fits();
            let text = fixture.editor.blocks[active].text();
            assert!(text.contains("A continuation"));
            if shift {
                assert!(text.starts_with("First paragraph\nA continuation"));
            }
        }
    }

    fn attachment_children_do_not_hide_paragraph_measurement_changes() {
        let mut fixture = RetainedEditor::new(300.0, 1.0, true);
        assert!(
            !fixture.editor.children().is_empty(),
            "exercise the attachment-child branch"
        );
        fixture
            .type_text(&"Words beside a note containing an attachment must still wrap. ".repeat(3));
        fixture.assert_focused_content_fits();
        // Model updates, like remote edits, must request a solve even when the
        // attachment's fixed-size button has no measurement changes of its own.
        fixture.editor.blocks[0]
            .line
            .set_text("A remote replacement paragraph. ".repeat(8).as_str());
        assert!(
            fixture.editor.layout_needs_measure(),
            "attachment must not mask the text mutation"
        );
        fixture.frame();
        fixture.assert_focused_content_fits();
    }

    clicking_another_paragraph_places_caret_at_the_pointer_not_its_old_position();
    retained_paragraph_grows_and_shrinks_while_typing_without_rebuilding();
    retained_paragraph_reflows_after_enter_and_shift_enter();
    attachment_children_do_not_hide_paragraph_measurement_changes();
}

/// Typing in a long document measures and paints what it touched, not the
/// document: the work per keystroke is the same at 200 and 10000 paragraphs.
#[test]
fn keystroke_work_does_not_grow_with_the_document() {
    let mut work = Vec::new();
    for paragraphs in [200_usize, 2000, 10000] {
        let blocks = (0..paragraphs)
            .map(|index| Block::paragraph(&format!("Paragraph {index} of a long note.")))
            .collect();
        let mut fixture = RetainedEditor::with_blocks(480.0, 1.0, blocks);
        fixture.viewport = Some(Rect::new(0.0, 0.0, 480.0, 600.0));
        fixture.type_text("warm ");
        fixture.editor.stack_measures = 0;
        fixture.editor.painted_blocks = 0;
        let keystrokes = "typed".len();
        fixture.type_text("typed");
        assert_eq!(fixture.rebuilds, 0, "typing retains the layout tree");
        assert!(
            fixture.editor.stack_measures <= keystrokes,
            "{paragraphs} paragraphs: each keystroke re-measures only its own line, \
             measured {}",
            fixture.editor.stack_measures,
        );
        work.push((
            fixture.editor.stack_measures,
            fixture.editor.painted_blocks,
            fixture.tree.total_node_count(),
        ));
        let last = fixture.editor.blocks.last().unwrap().line.get_rect();
        assert!(
            last.y0 > 600.0 && last.height() > 0.0,
            "off-screen blocks keep their exact place in the document: {last:?}"
        );
    }
    assert!(
        work.windows(2).all(|pair| pair[0] == pair[1]),
        "measured lines, painted blocks and layout nodes per keystroke: {work:?}"
    );
}

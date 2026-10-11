//! Keep the production layout tree alive between keystrokes. Rebuilding it
//! would erase Taffy's intrinsic-size cache and conceal stale paragraph heights.
use super::*;
use crate::bound_dispatch::BoundNodeMap;
use crate::testing::Dispatch;
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
    scene: Scene,
    damage: Vec<Rect>,
}

impl RetainedEditor {
    fn click(&mut self, point: Point) {
        let device_id = winit::event::DeviceId::dummy();
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
            Dispatch::new()
                .doc(&mut self.doc)
                .layout(&self.tree, self.root)
                .send(&mut *self.editor, &event);
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
            scene: Scene::new(),
            damage: Vec::new(),
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
        self.scene.reset();
        self.editor.draw(
            &mut self.scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &presence,
        );
        self.damage.clear();
        self.editor.collect_damage(&mut self.damage);
        self.editor.mark_drawn();
        for child in self.editor.children_mut() {
            child.mark_drawn();
        }
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        let event = crate::synthetic_input::key_event(
            key.clone(),
            match &key {
                Key::Character(s) => Some(s.clone()),
                _ => None,
            },
            ElementState::Pressed,
        );
        let handled = Dispatch::new()
            .modifiers(modifiers)
            .doc(&mut self.doc)
            .layout(&self.tree, self.root)
            .send(
                &mut *self.editor,
                &WindowEvent::KeyboardInput {
                    device_id: winit::event::DeviceId::dummy(),
                    event,
                    is_synthetic: false,
                },
            )
            .handled;
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
fn retained_paragraph_typing_damage_matches_full_gpu_pixels() {
    let blocks = (0..18)
        .map(|index| Block::paragraph(&format!("Paragraph {index}: an ordinary document line.")))
        .collect();
    let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
    let background = fixture.theme.tokens.colors.surfaces.content;
    let mut retained = crate::offscreen::OffscreenRenderer::new(1200, 900);
    let mut full = crate::offscreen::OffscreenRenderer::new(1200, 900);
    let before = retained.render(&fixture.scene, background);
    fixture.key(Key::Character("x".into()), Modifiers::default());
    assert_eq!(
        fixture.editor.blocks[0].text(),
        "xParagraph 0: an ordinary document line."
    );
    assert!(
        !fixture.damage.is_empty(),
        "ordinary typing must describe its changed paint"
    );
    let area: f64 = fixture.damage.iter().map(Rect::area).sum();
    assert!(
        area < fixture.editor.get_rect().area() / 2.0,
        "a stable paragraph edit must not damage the whole document: {:?}",
        fixture.damage
    );
    let damage: Vec<_> = fixture
        .damage
        .iter()
        .map(|rect| rect.inflate(16.0, 16.0))
        .collect();
    let rasterized = retained.render_damage_regions_gpu_wait(&fixture.scene, background, &damage);
    assert!(
        rasterized < 1200 * 900,
        "the pixel oracle must exercise actual sparse rasterization"
    );
    let after = full.render(&fixture.scene, background);
    assert_ne!(
        before, after,
        "the actual keyboard path changes visible text"
    );
    assert_eq!(
        retained.readback_current(),
        after,
        "all retained pixels must match the full GPU oracle"
    );
}

#[test]
fn retained_heading_list_and_quote_typing_damages_its_row_and_matches_gpu_pixels() {
    let blocks = || {
        vec![
            Block::heading(1, "Plan"),
            Block::paragraph("An ordinary paragraph under the heading."),
            Block::bullet(0, "A bullet"),
            Block::numbered(0, "A numbered step"),
            Block::checklist(0, false, "A task"),
            Block::quote("A quotation"),
            Block::divider(),
            Block::paragraph("The last paragraph."),
        ]
    };
    for (index, before_text) in [
        (0, "Plan"),
        (2, "A bullet"),
        (3, "A numbered step"),
        (4, "A task"),
        (5, "A quotation"),
    ] {
        let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks());
        fixture.editor.focus_block(index, CursorPlacement::End);
        fixture.frame();
        fixture.frame();
        let before = fixture.scene.clone();
        fixture.key(Key::Character("x".into()), Modifiers::default());
        assert_eq!(
            fixture.editor.blocks[index].text(),
            format!("{before_text}x"),
            "the key reached block {index}"
        );
        let area: f64 = fixture.damage.iter().map(Rect::area).sum();
        assert!(
            area < fixture.editor.get_rect().area() / 2.0,
            "typing in block {index} must damage its row, not the document: {:?}",
            fixture.damage
        );
        assert_damage_pixels(&before, &fixture, true);
    }
}

#[test]
fn retained_code_lines_and_prose_beside_code_and_tables_damage_their_row_and_match_gpu_pixels() {
    let blocks = || {
        let mut code = Block::code_block(Some("rust".into()), "fn main() {");
        let group = code.id.clone();
        code.code_group = Some(group.clone());
        let mut body = Block::code_block(Some("rust".into()), "    run();");
        body.code_group = Some(group.clone());
        let mut end = Block::code_block(Some("rust".into()), "}");
        end.code_group = Some(group);
        vec![
            Block::paragraph("Before the code."),
            code,
            body,
            end,
            Block::paragraph("Between code and table."),
            Block::table("| A | B |\n| --- | --- |\n| 1 | 2 |", true),
            Block::paragraph("After the table."),
        ]
    };
    // The run's first line (whose block also paints the run's ground), a
    // later line, its last line, and prose in a note holding code and a
    // table.
    for (index, before_text) in [
        (1, "fn main() {"),
        (2, "    run();"),
        (3, "}"),
        (0, "Before the code."),
        (6, "After the table."),
    ] {
        let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks());
        fixture.editor.focus_block(index, CursorPlacement::End);
        fixture.frame();
        fixture.frame();
        let before = fixture.scene.clone();
        fixture.key(Key::Character("x".into()), Modifiers::default());
        assert_eq!(
            fixture.editor.blocks[index].text(),
            format!("{before_text}x"),
            "the key reached block {index}"
        );
        let area: f64 = fixture.damage.iter().map(Rect::area).sum();
        assert!(
            area < fixture.editor.get_rect().area() / 4.0,
            "typing in block {index} must damage its row, not the document: {:?}",
            fixture.damage
        );
        assert_damage_pixels(&before, &fixture, true);
    }
}

fn assert_damage_pixels(before: &Scene, fixture: &RetainedEditor, sparse: bool) {
    let background = fixture.theme.tokens.colors.surfaces.content;
    let mut retained = crate::offscreen::OffscreenRenderer::new(1200, 1100);
    let mut full = crate::offscreen::OffscreenRenderer::new(1200, 1100);
    retained.render(before, background);
    let damage: Vec<_> = fixture
        .damage
        .iter()
        .map(|rect| rect.inflate(16.0, 16.0))
        .collect();
    assert!(!damage.is_empty(), "the actual edit must invalidate paint");
    let rasterized = retained.render_damage_regions_gpu_wait(&fixture.scene, background, &damage);
    if sparse {
        assert!(
            rasterized < 1200 * 1100,
            "a stable edit must exercise sparse rasterization"
        );
    }
    let expected = full.render(&fixture.scene, background);
    assert_eq!(
        retained.readback_current(),
        expected,
        "retained pixels must match full rendering"
    );
}

#[test]
fn retained_paragraph_delete_and_structure_damage_match_gpu_pixels_at_both_text_scales() {
    for scale in [1.0, 2.0] {
        let blocks = (0..8)
            .map(|index| {
                Block::paragraph(&format!("Paragraph {index}: an ordinary document line."))
            })
            .collect();
        let mut fixture = RetainedEditor::with_blocks(1000.0, scale, blocks);
        fixture.key(Key::Character("x".into()), Modifiers::default());
        let before = fixture.scene.clone();
        fixture.key(Key::Named(NamedKey::Backspace), Modifiers::default());
        assert_eq!(
            fixture.editor.blocks[0].text(),
            "Paragraph 0: an ordinary document line."
        );
        assert!(
            fixture.damage.iter().map(Rect::area).sum::<f64>()
                < fixture.editor.get_rect().area() / 2.0
        );
        assert_damage_pixels(&before, &fixture, true);
        let before = fixture.scene.clone();
        let old_bounds = fixture.editor.get_rect();
        fixture.key(Key::Named(NamedKey::Enter), Modifiers::default());
        assert_eq!(fixture.editor.blocks.len(), 9);
        assert!(
            fixture
                .damage
                .iter()
                .any(|rect| rect.contains_rect(old_bounds)),
            "structure changes preserve all previous paint bounds"
        );
        assert_damage_pixels(&before, &fixture, false);
    }
}

#[test]
fn retained_wrapping_delete_clears_previous_taller_paint_bounds() {
    for scale in [1.0, 2.0] {
        let mut fixture = RetainedEditor::with_blocks(280.0, scale, vec![Block::paragraph("")]);
        fixture.type_text(
            "A paragraph that wraps over multiple lines and becomes shorter after deletion. ",
        );
        let mut proved_shrink = false;
        for _ in 0..80 {
            let before = fixture.scene.clone();
            let old_bounds = fixture.editor.get_rect();
            fixture.key(Key::Named(NamedKey::Backspace), Modifiers::default());
            if fixture.editor.get_rect().height() < old_bounds.height() {
                assert!(
                    fixture
                        .damage
                        .iter()
                        .any(|rect| rect.contains_rect(old_bounds)),
                    "shrinking layout must clear the previous taller extent"
                );
                assert_damage_pixels(&before, &fixture, false);
                proved_shrink = true;
                break;
            }
        }
        assert!(
            proved_shrink,
            "the ordinary delete path must actually shrink wrapped geometry"
        );
    }
}

#[test]
fn retained_scene_release_keeps_first_rebuild_full_and_restores_paragraph_proof() {
    let blocks = (0..8)
        .map(|index| Block::paragraph(&format!("Paragraph {index}: an ordinary document line.")))
        .collect();
    let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
    let before = fixture.scene.clone();
    let old_bounds = fixture.editor.get_rect();
    fixture.editor.release_scene_cache();
    fixture.key(Key::Character("x".into()), Modifiers::default());
    assert!(
        fixture
            .damage
            .iter()
            .any(|rect| rect.contains_rect(old_bounds))
    );
    assert_damage_pixels(&before, &fixture, false);
    let before = fixture.scene.clone();
    fixture.key(Key::Character("y".into()), Modifiers::default());
    assert_eq!(
        fixture.editor.blocks[0].text(),
        "xyParagraph 0: an ordinary document line."
    );
    assert_damage_pixels(&before, &fixture, true);
}

#[test]
fn retained_theme_and_viewport_changes_keep_full_paint_bounds() {
    for viewport in [false, true] {
        let blocks = (0..8)
            .map(|index| {
                Block::paragraph(&format!("Paragraph {index}: an ordinary document line."))
            })
            .collect();
        let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
        let before = fixture.scene.clone();
        let old_bounds = fixture.editor.get_rect();
        if viewport {
            fixture.viewport = Some(Rect::new(0.0, 100.0, 1000.0, 600.0));
        } else {
            fixture.theme = Theme::light();
        }
        fixture.frame();
        assert!(
            fixture
                .damage
                .iter()
                .any(|rect| rect.contains_rect(old_bounds))
        );
        assert_damage_pixels(&before, &fixture, false);
    }
}

#[test]
fn retained_document_selection_and_find_overlay_keep_full_paint_bounds() {
    for find in [false, true] {
        let blocks = (0..8)
            .map(|index| {
                Block::paragraph(&format!("Paragraph {index}: an ordinary document line."))
            })
            .collect();
        let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
        let before = fixture.scene.clone();
        let old_bounds = fixture.editor.get_rect();
        if find {
            fixture.editor.open_find();
            fixture.frame();
            assert!(fixture.editor.find_visible);
        } else {
            fixture.key(Key::Character("a".into()), ModifiersState::CONTROL.into());
            assert!(fixture.editor.document_selection.is_some());
        }
        assert!(
            fixture
                .damage
                .iter()
                .any(|rect| rect.contains_rect(old_bounds)),
            "document selection and global overlays retain full paint bounds"
        );
        assert_damage_pixels(&before, &fixture, false);
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
            let target = 60;
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

/// A block document's drag handles hang in the editor's own handle gutter,
/// outside the text column and inside the editor's rect: a host that puts
/// the rect on its page gutter keeps the handles a gutter clear of the
/// sidebar, and the text starts exactly one handle gutter in.
#[test]
fn handles_hang_in_their_own_gutter_outside_the_text_column() {
    for scale in [1.0, 2.0] {
        let fixture = RetainedEditor::new(480.0, scale, false);
        let editor = &fixture.editor;
        let gutter = editor.handle_gutter(&fixture.theme);
        assert!(gutter > 0.0, "an editable document has a handle gutter");
        assert_eq!(editor.text_inset(&fixture.theme), gutter);
        let line = editor.blocks[1].line.get_rect();
        let text_edge = line.x0 + editor.blocks[1].line.text_inset(&fixture.theme);
        assert!(
            (text_edge - (editor.get_rect().x0 + gutter)).abs() < 1e-6,
            "text starts one handle gutter in at {scale}x: {text_edge}"
        );
        let handle = editor.block_drag_handle_rect(1).unwrap();
        assert!(
            handle.x0 >= editor.get_rect().x0 - 1e-6,
            "the handle stays inside the editor at {scale}x: {handle:?}"
        );
        assert!(
            handle.x1 < text_edge,
            "the handle hangs outside the text column at {scale}x: {handle:?}"
        );
    }
    let read_only = BlockEditor::new().with_read_only(true);
    assert_eq!(read_only.handle_gutter(&Theme::dark()), 0.0);
}

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
                &crate::synthetic_input::keyboard_input(event),
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

#[test]
fn a_key_in_a_markdown_tables_source_damages_its_line_and_cell_not_the_table() {
    let source = "| Item | Amount |\n| --- | --- |\n| Apples | 4 |";
    let blocks = vec![
        Block::paragraph("Before the table."),
        Block::table(source, false),
        Block::paragraph("After the table."),
    ];
    let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
    // Into the amount, so the column keeps its width.
    fixture.editor.focus_block(
        1,
        CursorPlacement::Byte(source.len().saturating_sub(" |".len())),
    );
    fixture.frame();
    fixture.frame();
    let before = fixture.scene.clone();
    let table = fixture.editor.block_paint_rect(1).unwrap();
    fixture.key(Key::Character("2".into()), Modifiers::default());
    assert_eq!(
        fixture.editor.blocks[1].text(),
        "| Item | Amount |\n| --- | --- |\n| Apples | 42 |"
    );
    let area: f64 = fixture.damage.iter().map(Rect::area).sum();
    assert!(
        area < table.area() / 2.0,
        "a key in the source damages its line and the cell, not the table {table:?}: {:?}",
        fixture.damage
    );
    assert_damage_pixels(&before, &fixture, true);
}

/// How far a channel may move between two GPU renders of one scene.
const GPU_ROUNDING: u8 = 1;

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
    let actual = retained.readback_current();
    assert_eq!(actual.len(), expected.len());
    // Where they differ, as pixel bounds, rather than two framebuffers. The
    // GPU rasterizer itself rounds a pixel's channel one unit either way
    // between two renders of the very same scene, so a difference that small
    // is the rasterizer's, not retained paint's.
    let differing: Vec<(usize, usize)> = actual
        .chunks_exact(4)
        .zip(expected.chunks_exact(4))
        .enumerate()
        .filter(|(_, (actual, expected))| {
            actual
                .iter()
                .zip(expected.iter())
                .any(|(a, e)| a.abs_diff(*e) > GPU_ROUNDING)
        })
        .map(|(index, _)| (index % 1200, index / 1200))
        .collect();
    let spread = differing.iter().fold(None, |spread, &(x, y)| {
        Some(spread.map_or(
            (x, y, x, y),
            |(x0, y0, x1, y1): (usize, usize, usize, usize)| {
                (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
            },
        ))
    });
    assert!(
        differing.is_empty(),
        "retained pixels must match full rendering: {} differ within {spread:?} (x0, y0, x1, y1); damage {:?}",
        differing.len(),
        fixture.damage
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
        let old_rows: Vec<Rect> = fixture
            .editor
            .blocks
            .iter()
            .map(|block| block.line.get_rect())
            .collect();
        let rebuilds = fixture.rebuilds;
        fixture.key(Key::Named(NamedKey::Enter), Modifiers::default());
        assert_eq!(fixture.editor.blocks.len(), 9);
        // A block that came is a relayout, not a new tree: no widget came.
        assert_eq!(
            fixture.rebuilds, rebuilds,
            "a new paragraph restacks the blocks without rebuilding the layout tree"
        );
        // Every block below moved: each repaints where it was and is.
        for row in old_rows {
            assert!(
                fixture.damage.iter().any(|rect| rect.contains_rect(row)),
                "a moved block repaints where it was: {row:?} in {:?}",
                fixture.damage
            );
        }
        // A paragraph that came first moves the whole document, which may
        // be half the target: it need not render sparsely.
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
            let old_row = fixture.editor.blocks[0].line.get_rect();
            fixture.key(Key::Named(NamedKey::Backspace), Modifiers::default());
            if fixture.editor.get_rect().height() < old_bounds.height() {
                // The paragraph repaints where it painted, the taller row.
                assert!(
                    fixture
                        .damage
                        .iter()
                        .any(|rect| rect.contains_rect(old_row)),
                    "shrinking layout must clear the previous taller row: {old_row:?} in {:?}",
                    fixture.damage
                );
                assert_damage_pixels(&before, &fixture, true);
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
fn a_collaborators_paragraph_restacks_without_a_rebuild_and_repaints_the_blocks_below() {
    let blocks = (0..6)
        .map(|index| Block::paragraph(&format!("Paragraph {index}: an ordinary document line.")))
        .collect();
    let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
    fixture.frame();
    let before = fixture.scene.clone();
    let above = fixture.editor.blocks[2].line.get_rect();
    // The owning model shows a paragraph a collaborator wrote after the
    // third, as it shows any remote edit.
    let mut shown: Vec<BlockContent> = fixture.editor.blocks.iter().map(Block::content).collect();
    shown.insert(
        3,
        BlockContent::paragraph("remote", "A collaborator's paragraph."),
    );
    assert!(fixture.editor.present_blocks(&shown));
    assert!(
        !fixture.editor.take_structure_dirty(),
        "a paragraph brings no widget: the host keeps its tree"
    );
    assert!(
        fixture.editor.is_layout_dirty(),
        "the host lays the blocks out again"
    );
    fixture.frame();
    let rows: Vec<Rect> = fixture
        .editor
        .blocks
        .iter()
        .map(|block| block.line.get_rect())
        .collect();
    assert_eq!(
        fixture.editor.blocks[3].text(),
        "A collaborator's paragraph."
    );
    assert!(
        rows[3].height() > 0.0 && rows[3].y0 >= rows[2].y1 && rows[3].y1 <= rows[4].y0,
        "the paragraph stands between its neighbours: {rows:?}"
    );
    // The caret's paragraph and those above it keep their pixels.
    assert!(
        fixture
            .damage
            .iter()
            .all(|rect| rect.intersect(above).is_zero_area()),
        "only the new paragraph and those below repaint: {:?}",
        fixture.damage
    );
    assert_damage_pixels(&before, &fixture, true);
}

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

/// A note shaped like a working one, `sections` times over: a heading, a
/// paragraph with a link, a bullet pair, a ten-line Rust fence and a TeX
/// formula, so every per-run derivation the draw makes has a subject.
fn sectioned_note(sections: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    for section in 0..sections {
        blocks.push(Block::heading(2, &format!("Section {section}")));
        let mut paragraph = Block::paragraph(&format!(
            "Paragraph {section:03} links to a meeting among ordinary prose."
        ));
        paragraph
            .line
            .set_marks(vec![MarkSpan::new(23..32, Mark::Link(Uuid::nil()))]);
        blocks.push(paragraph);
        blocks.push(Block::bullet(0, &format!("observation {section}")));
        blocks.push(Block::bullet(0, &format!("follow-up {section}")));
        for line in 0..10 {
            blocks.push(Block::code_block(
                Some("rust".into()),
                &format!("let line_{line} = \"section {section}\"; // note"),
            ));
        }
        blocks.push(Block::paragraph("Between the fence and its formula."));
        blocks.push(Block::code_block(Some("tex".into()), r"\frac{x^2+1}{y}"));
        blocks.push(Block::paragraph("Closing words of the section."));
    }
    blocks
}

/// The editor's first line on screen when it is scrolled to its middle.
fn scrolled_to_middle(fixture: &mut RetainedEditor) -> Rect {
    let middle = fixture.editor.blocks.len() / 2;
    fixture.frame();
    let top = fixture.editor.blocks[middle].line.get_rect().y0;
    let viewport = Rect::new(0.0, top, f64::from(fixture.width), top + 600.0);
    fixture.viewport = Some(viewport);
    fixture.frame();
    viewport
}

fn has_colour(block: &Block, kind: TokenKind) -> bool {
    block
        .line
        .transient_colors()
        .iter()
        .any(|span| span.kind == kind)
}

/// An unchanged redraw highlights no code at all, and a keystroke in one
/// code line re-highlights that line's run and nothing else, however long
/// the document: syntax colours are derived once per change, not per frame.
#[test]
fn syntax_colours_are_derived_per_change_not_per_frame() {
    let mut fixture = RetainedEditor::with_blocks(640.0, 1.0, sectioned_note(125));
    fixture.viewport = Some(Rect::new(0.0, 0.0, 640.0, 600.0));
    fixture.frame();
    let code_lines = fixture
        .editor
        .blocks
        .iter()
        .filter(|block| matches!(block.kind, BlockKind::CodeBlock { .. }))
        .count();
    assert!(code_lines > 1000, "a long document with code: {code_lines}");
    assert!(
        has_colour(&fixture.editor.blocks[4], TokenKind::Keyword),
        "the first frame colours code"
    );
    fixture.editor.highlighted_lines = 0;
    fixture.frame();
    fixture.frame();
    assert_eq!(
        fixture.editor.highlighted_lines, 0,
        "an unchanged redraw highlights no code line"
    );
    // A keystroke in the third line of the first fence.
    fixture.editor.focus_block(6, CursorPlacement::Start);
    fixture.frame();
    fixture.editor.highlighted_lines = 0;
    fixture.type_text("x");
    assert!(fixture.editor.blocks[6].text().starts_with("xlet line_2"));
    assert_eq!(
        fixture.editor.highlighted_lines, 10,
        "a keystroke re-highlights its own ten-line run only"
    );
    assert!(
        has_colour(&fixture.editor.blocks[6], TokenKind::StringLit),
        "the edited line is coloured again"
    );
    // A line that opens a comment carries the parse into the run's next lines.
    fixture.editor.blocks[4].line.set_text("/* an open comment");
    fixture.frame();
    assert!(
        fixture.editor.blocks[5]
            .line
            .transient_colors()
            .iter()
            .all(|span| span.kind == TokenKind::Comment),
        "an unterminated comment colours the rest of its run: {:?}",
        fixture.editor.blocks[5].line.transient_colors()
    );
    // A line that leaves the run for prose drops its colours.
    fixture.editor.blocks[5].kind = BlockKind::Paragraph;
    fixture.frame();
    assert!(fixture.editor.blocks[5].line.transient_colors().is_empty());
    // Its old neighbour now starts a run of its own, outside the comment, and
    // is highlighted afresh.
    assert!(
        has_colour(&fixture.editor.blocks[6], TokenKind::StringLit),
        "{:?}",
        fixture.editor.blocks[6].line.transient_colors()
    );
}

/// Replacing the blocks with fresh lines of the same identity and text
/// still colours them: what was derived lives with the colours, not the ids.
#[test]
fn fresh_lines_with_known_ids_are_coloured() {
    let mut fixture = RetainedEditor::with_blocks(640.0, 1.0, sectioned_note(2));
    fixture.frame();
    let replacement: Vec<Block> = fixture
        .editor
        .blocks
        .iter()
        .map(|block| {
            let mut fresh = match &block.kind {
                BlockKind::CodeBlock { lang } => Block::code_block(lang.clone(), block.text()),
                _ => Block::paragraph(block.text()),
            };
            fresh.id.clone_from(&block.id);
            fresh
        })
        .collect();
    fixture.editor.set_blocks(replacement);
    fixture.rebuild_after_structure_change();
    fixture.frame();
    assert!(
        has_colour(&fixture.editor.blocks[4], TokenKind::Keyword),
        "a fresh line is highlighted even though its id and text are known"
    );
}

/// What a frame derives besides painting follows the viewport, not the
/// document: a ten times longer note with the same screen derives the same
/// code chrome, formula sources and link targets, and paints the same blocks.
#[test]
fn per_frame_derivations_follow_the_viewport_not_the_document() {
    let mut work = Vec::new();
    for sections in [12_usize, 120] {
        let mut fixture = RetainedEditor::with_blocks(640.0, 1.0, sectioned_note(sections));
        fixture.viewport = Some(Rect::new(0.0, 0.0, 640.0, 600.0));
        fixture.frame();
        fixture.frame();
        fixture.editor.painted_blocks = 0;
        fixture.editor.formula_sources = 0;
        fixture.editor.highlighted_lines = 0;
        crate::profiling::frame_probe::enable();
        let started = crate::time::now();
        let frames = 20_u32;
        for _ in 0..frames {
            fixture.frame();
        }
        let elapsed = started.elapsed() / frames;
        let phases = crate::profiling::frame_probe::take();
        eprintln!(
            "block-editor-frame {}",
            serde_json::json!({
                "blocks": fixture.editor.blocks.len(),
                "profile": if cfg!(debug_assertions) { "test-debug" } else { "optimized" },
                "frame_us": elapsed.as_micros(),
                "draw_phases_ns_calls": phases,
            })
        );
        let last = fixture.editor.blocks.last().unwrap().line.get_rect();
        assert!(last.y0 > 1200.0, "the note reaches past the paint window");
        work.push((
            fixture.editor.painted_blocks,
            fixture.editor.formula_sources,
            fixture.editor.highlighted_lines,
            fixture.editor.code_copy_hits.len(),
            fixture.editor.live_link_hits.len(),
        ));
    }
    assert!(
        work[0].1 > 0 && work[0].3 > 0 && work[0].4 > 0,
        "the screen holds formulas, code and links: {work:?}"
    );
    assert_eq!(work[0].2, 0, "unchanged frames highlight no code: {work:?}");
    assert_eq!(
        work[0], work[1],
        "painted blocks, formula sources, highlighted lines, code chrome and link targets \
         per frame: {work:?}"
    );
}

/// The copy button of a code run on screen stays hit-testable, and a link on
/// screen stays clickable, after culling what is off screen.
#[test]
fn culled_frames_keep_on_screen_chrome_and_links_live() {
    let mut fixture = RetainedEditor::with_blocks(640.0, 1.0, sectioned_note(40));
    let viewport = scrolled_to_middle(&mut fixture);
    let blocks = &fixture.editor.blocks;
    let fences_on_screen: Vec<usize> = (0..blocks.len())
        .filter(|&index| {
            code_header_height(blocks, index, 1.0) > 0.0
                && viewport.contains(blocks[index].line.get_rect().origin())
        })
        .collect();
    assert!(!fences_on_screen.is_empty(), "a fence starts on screen");
    for start in &fences_on_screen {
        assert!(
            fixture
                .editor
                .code_copy_hits
                .iter()
                .any(|(rect, hit, _)| hit == start && rect.area() > 0.0),
            "fence {start} on screen has its copy button"
        );
    }
    let linked = (0..blocks.len())
        .find(|&index| {
            !blocks[index].line.marks().is_empty()
                && viewport.contains(blocks[index].line.get_rect().origin())
        })
        .expect("a linked paragraph on screen");
    let link = blocks[linked].line.mark_rects(&fixture.theme, 23..32)[0];
    assert_eq!(
        fixture.editor.link_at_point(link.center()),
        Some(LinkTarget::Page(Uuid::nil()))
    );
    let reach = viewport.inflate(0.0, viewport.height());
    assert!(
        fixture
            .editor
            .code_copy_hits
            .iter()
            .all(|(rect, _, _)| rect.y1 >= reach.y0 - CODE_HEADER_HEIGHT && rect.y0 <= reach.y1),
        "no chrome is derived for a run a screen or more away"
    );
}

/// The window of a note scrolled to its middle, rendered.
fn render_window(fixture: &mut RetainedEditor, viewport: Option<Rect>, top: f64) -> Vec<u8> {
    let presence = PresenceCtx::empty();
    if let Some(viewport) = viewport {
        presence.push_viewport(viewport);
    }
    let mut scene = Scene::new();
    fixture.editor.draw(
        &mut scene,
        Affine::translate((0.0, -top)),
        &mut fixture.text,
        &fixture.theme,
        &presence,
    );
    let background = fixture.theme.tokens.colors.surfaces.content;
    crate::offscreen::OffscreenRenderer::new(640, 600).render(&scene, background)
}

/// Culling and caching change nothing on screen: a culled frame from warm
/// caches renders the window exactly as a frame that paints every block and
/// derives every colour again, in each theme.
#[test]
fn culled_cached_frames_render_what_a_full_frame_does() {
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let mut fixture = RetainedEditor::with_blocks(640.0, 1.0, sectioned_note(30));
        fixture.theme = theme;
        let viewport = scrolled_to_middle(&mut fixture);
        fixture.frame();
        let culled = render_window(&mut fixture, Some(viewport), viewport.y0);
        // Forget every derived colour: the next frame highlights from scratch.
        for block in &mut fixture.editor.blocks {
            block.line.set_transient_colors(Vec::new());
        }
        fixture.editor.highlighted_lines = 0;
        let full = render_window(&mut fixture, None, viewport.y0);
        assert!(
            fixture.editor.highlighted_lines > 0,
            "the full frame derived its colours again"
        );
        let differing = culled
            .chunks_exact(4)
            .zip(full.chunks_exact(4))
            .filter(|(culled, full)| culled != full)
            .count();
        assert!(
            culled.chunks_exact(4).any(|pixel| pixel != &culled[..4]),
            "the window shows the note"
        );
        assert_eq!(differing, 0, "pixels a culled frame paints differently");
    }
}

/// The semantic tree's nodes beneath and including `id`.
fn subtree_len(tree: &crate::a11y_tree::SemanticTree, id: &crate::a11y_tree::SemanticId) -> usize {
    tree.node(id).map_or(0, |node| {
        node.children
            .iter()
            .map(|child| subtree_len(tree, child))
            .sum::<usize>()
            .saturating_add(1)
    })
}

/// What a tree that keeps nothing between frames publishes for the editor
/// now: every node, in reading order.
fn published_afresh(fixture: &RetainedEditor) -> Vec<crate::a11y_tree::SemanticNode> {
    let mut tree = crate::a11y_tree::SemanticTree::new();
    tree.refresh(&*fixture.editor, &fixture.nodes, None, &[]);
    tree.iter().cloned().collect()
}

/// A keystroke builds the accessibility row of the block it typed into, and
/// the semantic tree builds that row's nodes, the document's and the
/// window's: every other block hands the tree the very row it had, which the
/// tree keeps.
/// The work is the same in a document four times as long, and the tree it
/// publishes is the one a full rebuild publishes.
#[test]
fn a_keystroke_builds_the_accessibility_row_of_its_block_alone() {
    let mut built = Vec::new();
    for count in [40, 160] {
        let blocks = (0..count)
            .map(|index| Block::paragraph(&format!("Paragraph {index}: an ordinary line.")))
            .collect();
        let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
        let mut tree = crate::a11y_tree::SemanticTree::new();
        tree.refresh(&*fixture.editor, &fixture.nodes, None, &[]);
        assert!(
            tree.nodes_built_for_test() > count * 6,
            "the first refresh builds every row"
        );
        let rows = fixture.editor.access_row_builds_for_test();

        fixture.key(Key::Character("x".into()), Modifiers::default());
        let delta = tree.refresh(&*fixture.editor, &fixture.nodes, None, &[]);

        assert_eq!(
            fixture.editor.access_row_builds_for_test() - rows,
            1,
            "a keystroke into one of {count} blocks builds one row"
        );
        let typed = tree
            .iter()
            .find(|node| node.name.as_deref() == Some("Paragraph block 1"))
            .map(|node| node.id.clone())
            .expect("the typed block's row");
        assert_eq!(
            tree.nodes_built_for_test(),
            subtree_len(&tree, &typed) + 2,
            "the tree builds the typed row's nodes, the document's and the window's alone, \
             of {count} blocks"
        );
        assert!(
            delta.added.is_empty() && delta.removed.is_empty() && !delta.updated.is_empty(),
            "the keystroke updates nodes, adding and removing none: {delta:?}"
        );
        assert_eq!(
            tree.iter().cloned().collect::<Vec<_>>(),
            published_afresh(&fixture),
            "the kept rows publish what a full rebuild publishes"
        );
        built.push(tree.nodes_built_for_test());
    }
    assert_eq!(
        built[0], built[1],
        "a keystroke costs the same whatever the document's length"
    );
}

/// A row is kept only while everything it is built from is the same: a
/// caret moved, a block indented or a block moved builds the rows it
/// changed and publishes what a full rebuild publishes.
#[test]
fn kept_accessibility_rows_follow_caret_indent_and_moves() {
    let blocks = vec![
        Block::paragraph("First"),
        Block::bullet(0, "Second"),
        Block::bullet(0, "Third"),
        Block::paragraph("Fourth"),
    ];
    let mut fixture = RetainedEditor::with_blocks(1000.0, 1.0, blocks);
    let mut tree = crate::a11y_tree::SemanticTree::new();
    tree.refresh(&*fixture.editor, &fixture.nodes, None, &[]);
    for key in [
        Key::Named(NamedKey::ArrowRight),
        Key::Named(NamedKey::ArrowDown),
        Key::Named(NamedKey::ArrowDown),
        Key::Named(NamedKey::Tab),
    ] {
        fixture.key(key.clone(), Modifiers::default());
        tree.refresh(&*fixture.editor, &fixture.nodes, None, &[]);
        assert_eq!(
            tree.iter().cloned().collect::<Vec<_>>(),
            published_afresh(&fixture),
            "after {key:?} the kept rows publish what a full rebuild publishes"
        );
    }
    let moved = fixture.editor.focused_index().expect("a focused block");
    assert!(fixture.editor.move_block_to(moved, 0), "the block moves up");
    if fixture.editor.take_structure_dirty() {
        fixture.rebuild_after_structure_change();
    }
    fixture.frame();
    tree.refresh(&*fixture.editor, &fixture.nodes, None, &[]);
    assert_eq!(
        tree.iter().cloned().collect::<Vec<_>>(),
        published_afresh(&fixture),
        "after a move the kept rows publish what a full rebuild publishes"
    );
}

/// The drag handles are for the pointer: moving it brings them up, typing
/// hides them, and a few seconds' rest fades them out, after which the editor
/// asks for no frame and sets no wake until something moves again.
#[test]
fn drag_handles_show_on_pointer_motion_and_go_on_typing_or_rest() {
    use super::handle_reveal::HANDLE_HOLD;
    use crate::time::testing::with_monotonic_time;
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
    let mut fixture = with_monotonic_time(at(0), || RetainedEditor::new(480.0, 1.0, false));
    let quiet = |fixture: &RetainedEditor| {
        !fixture.editor.needs_redraw()
            && !Widget::needs_animation_frame(&*fixture.editor)
            && fixture.editor.handle_fade_deadline().is_none()
    };
    assert_eq!(
        fixture.editor.handle_opacity_drawn_for_test(),
        0.0,
        "no handle shows beside the caret before the pointer moves"
    );
    assert!(quiet(&fixture));

    let over_text = fixture.editor.blocks[1].line.get_rect().center();
    let point = |fixture: &mut RetainedEditor, at| {
        with_monotonic_time(at, || {
            Dispatch::new()
                .doc(&mut fixture.doc)
                .layout(&fixture.tree, fixture.root)
                .send(
                    &mut *fixture.editor,
                    &crate::synthetic_input::cursor_moved(over_text.x, over_text.y),
                );
            fixture.frame();
        });
    };
    point(&mut fixture, at(0));
    assert_eq!(
        fixture.editor.handle_opacity_drawn_for_test(),
        1.0,
        "moving the pointer shows the handles"
    );

    with_monotonic_time(at(100), || fixture.type_text("a"));
    assert_eq!(
        fixture.editor.handle_opacity_drawn_for_test(),
        0.0,
        "a keystroke hides them"
    );
    assert!(quiet(&fixture), "hidden handles need nothing");

    point(&mut fixture, at(200));
    assert_eq!(fixture.editor.handle_opacity_drawn_for_test(), 1.0);
    let rest = at(200) + HANDLE_HOLD;
    with_monotonic_time(at(300), || {
        assert!(!Widget::needs_animation_frame(&*fixture.editor));
        assert!(
            !fixture.editor.needs_redraw(),
            "resting handles cost no frames"
        );
        assert_eq!(
            fixture.editor.handle_fade_deadline(),
            Some(rest),
            "the editor sleeps until the fade starts"
        );
        assert!(
            Widget::next_wake_deadline(&*fixture.editor).is_some_and(|wake| wake <= rest),
            "and its host wakes it then"
        );
    });
    let midway = rest + crate::animation::CHROME_FADE / 2;
    with_monotonic_time(midway, || {
        assert!(Widget::needs_animation_frame(&*fixture.editor), "fading");
        fixture.frame();
    });
    let fading = fixture.editor.handle_opacity_drawn_for_test();
    assert!(fading > 0.0 && fading < 1.0, "halfway faded: {fading}");
    let gone = rest + crate::animation::CHROME_FADE;
    with_monotonic_time(midway, || {
        assert_eq!(
            Widget::next_wake_deadline(&*fixture.editor),
            Some(gone),
            "the host wakes it when the fade is over"
        );
    });
    with_monotonic_time(gone, || {
        // That wake's frame takes the faint handles away.
        fixture.frame();
        assert_eq!(fixture.editor.handle_opacity_drawn_for_test(), 0.0);
        assert!(quiet(&fixture), "faded handles need no frame or wake");
    });
}

/// The handles' frames follow the clock, not the draws: an editor that is
/// never drawn (its host culls it) asks for frames only during the fade, and
/// for none once the fade is over, so it cannot keep the window spinning.
#[test]
fn undrawn_drag_handles_stop_asking_for_frames_once_their_fade_is_over() {
    use super::handle_reveal::HANDLE_HOLD;
    use crate::time::testing::with_monotonic_time;
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
    let mut fixture = with_monotonic_time(at(0), || RetainedEditor::new(480.0, 1.0, false));
    let over_text = fixture.editor.blocks[1].line.get_rect().center();
    // The pointer moves over it, and no frame is drawn from here on.
    with_monotonic_time(at(0), || {
        Dispatch::new()
            .doc(&mut fixture.doc)
            .layout(&fixture.tree, fixture.root)
            .send(
                &mut *fixture.editor,
                &crate::synthetic_input::cursor_moved(over_text.x, over_text.y),
            );
    });
    let rest = at(0) + HANDLE_HOLD;
    with_monotonic_time(at(100), || {
        assert!(
            !Widget::needs_animation_frame(&*fixture.editor),
            "the hold needs no frames"
        );
        assert_eq!(fixture.editor.handle_fade_deadline(), Some(rest));
    });
    with_monotonic_time(rest + crate::animation::CHROME_FADE / 2, || {
        assert!(
            Widget::needs_animation_frame(&*fixture.editor),
            "the fade has frames"
        );
    });
    with_monotonic_time(rest + crate::animation::CHROME_FADE, || {
        assert!(
            !Widget::needs_animation_frame(&*fixture.editor),
            "an undrawn editor asks for no frame once the fade is over"
        );
        assert!(fixture.editor.handle_fade_deadline().is_none());
        assert!(Widget::next_wake_deadline(&*fixture.editor).is_none());
    });
}

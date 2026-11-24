//! Text flowing beside a picture set against an edge: the rows of the blocks after it stand in
//! the narrowed column until they pass its bottom, and take the whole width again below it.

use super::*;
use ui::bound_dispatch::BoundNodeMap;
use ui::testing::Dispatch;
use winit::event::MouseButton;

/// One visual row of a line: where its words start and end, and which bytes they are.
struct Row {
    y: f64,
    bytes: std::ops::Range<usize>,
    x0: f64,
    x1: f64,
}

impl Row {
    fn below(&self, y: f64) -> bool {
        self.y >= y
    }
}

struct FloatEditor {
    editor: Box<BlockEditor>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: NodeId,
    text: TextRenderer,
    theme: Theme,
    doc: AutoCommit,
    width: f32,
}

const WORDS: &str = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima \
mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu";

impl FloatEditor {
    fn with_blocks(width: f32, blocks: Vec<Block>) -> Self {
        let mut editor = Box::new(BlockEditor::new().with_wrap_lines(true));
        let has_picture = blocks
            .iter()
            .any(|block| standalone_markdown_image(block.text()).is_some());
        editor.set_blocks(blocks);
        let bytes = ui::png::encode_rgba8(200, 200, &vec![200; 200 * 200 * 4]).unwrap();
        let fetch: ui::image_store::Fetcher = std::sync::Arc::new(move |_| Some(bytes.clone()));
        editor.set_image_store(Some(ui::ImageStore::new(&fetch)));
        if has_picture {
            assert!(
                editor.prewarm_images_for_shots(),
                "the fixture picture must decode"
            );
        }
        let theme = Theme::dark();
        let mut text = ui::font_catalog::fixture_renderer();
        text.set_text_scale(theme.text_scale);
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = editor.build_layout(&mut tree, &theme, &mut nodes);
        let mut bindings = BoundNodeMap::new();
        // SAFETY: the boxed editor and its blocks remain in place until the fixture is dropped.
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
        };
        fixture.frame();
        if has_picture {
            assert_eq!(
                fixture.editor.image_dimensions.get("scan.png"),
                Some(&(200, 200)),
                "the picture's size is known where the rows flow beside it"
            );
        }
        fixture
    }

    /// A document with one picture set against an edge, and words after it to run down beside it.
    fn beside_picture(width: f32, markdown: &str) -> Self {
        Self::with_blocks(
            width,
            vec![
                Block::paragraph(markdown),
                Block::paragraph(&format!("{WORDS} {WORDS} {WORDS} {WORDS}")),
                Block::paragraph("after the words"),
            ],
        )
    }

    fn frame(&mut self) {
        // SAFETY: this fixture exclusively owns the stable boxed editor and the matching maps;
        // callbacks never replace blocks while measuring.
        unsafe {
            ui::widget::compute_widget_layout(
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
    }

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

    /// Where the words of `block` stand on each visual row.
    fn rows(&self, block: usize) -> Vec<Row> {
        let line = &self.editor.blocks[block].line;
        let len = line.value().len();
        let mut rows: Vec<Row> = Vec::new();
        for byte in 0..len {
            let caret = line.caret_rect(&self.theme, byte);
            match rows.last_mut() {
                Some(row) if (row.y - caret.y0).abs() < 0.5 => {
                    row.bytes.end = byte.saturating_add(1);
                }
                _ => rows.push(Row {
                    y: caret.y0,
                    bytes: byte..byte.saturating_add(1),
                    x0: caret.x0,
                    x1: caret.x0,
                }),
            }
        }
        for row in &mut rows {
            row.x0 = line.caret_rect(&self.theme, row.bytes.start).x0;
            row.x1 = line
                .mark_rects(&self.theme, row.bytes.clone())
                .into_iter()
                .map(|rect| rect.x1)
                .fold(row.x0, f64::max);
        }
        rows
    }

    /// The column the words are set in: its left and right edges, without the picture.
    fn column(&self) -> (f64, f64) {
        let rect = self.editor.blocks[1].line.get_rect();
        let pad = self.editor.blocks[1].line.text_inset(&self.theme);
        (rect.x0 + pad, rect.x1 - pad)
    }
}

#[test]
fn rows_beside_a_left_picture_start_past_it_and_reach_the_column_edge() {
    let fixture = FloatEditor::beside_picture(520.0, "![Scan|200|wrap:left](scan.png)");
    let picture = fixture.editor.image_preview_rects[0].expect("the picture is placed");
    assert!(
        picture.width() > 190.0,
        "the picture takes its size: {picture:?}"
    );
    let (left, right) = fixture.column();
    assert!(
        picture.x0 >= left - 10.0,
        "the picture stands at the edge: {picture:?}"
    );
    let rows = fixture.rows(1);
    assert!(
        rows.len() > 2,
        "the words take several rows: {}",
        rows.len()
    );
    let first = &rows[0];
    assert!(
        first.x0 >= picture.x1 + 8.0,
        "a row beside the picture starts past it: row {} picture {}",
        first.x0,
        picture.x1
    );
    assert!(
        first.x1 > right - 140.0 && first.x1 <= right + 1.0,
        "a row beside the picture runs on to the column's far edge: \
         row ends {} column ends {right}",
        first.x1
    );
}

#[test]
fn the_row_after_the_pictures_bottom_takes_the_whole_width() {
    let fixture = FloatEditor::beside_picture(520.0, "![Scan|200|wrap:left](scan.png)");
    let picture = fixture.editor.image_preview_rects[0].expect("the picture is placed");
    let (left, right) = fixture.column();
    let rows = fixture.rows(1);
    let below = rows
        .iter()
        .find(|row| row.below(picture.y1))
        .expect("the words run past the picture's bottom");
    assert!(
        (below.x0 - left).abs() < 1.0,
        "a row below the picture starts at the column's edge: row {} column {left}",
        below.x0
    );
    assert!(
        below.x1 > right - 140.0,
        "a row below the picture takes the whole column: row ends {} column ends {right}",
        below.x1
    );
    // And the rows above it stayed narrowed: the picture really took the room.
    assert!(
        rows[0].x0 > picture.x1,
        "the first row is still beside the picture"
    );
}

#[test]
fn a_right_picture_narrows_the_rows_from_the_right() {
    let fixture = FloatEditor::beside_picture(520.0, "![Scan|200|wrap:right](scan.png)");
    let picture = fixture.editor.image_preview_rects[0].expect("the picture is placed");
    let (left, right) = fixture.column();
    assert!(
        (picture.x1 - right).abs() < 1.0,
        "a picture set right stands at the column's right edge: picture {} column {right}",
        picture.x1
    );
    let rows = fixture.rows(1);
    let first = &rows[0];
    assert!(
        (first.x0 - left).abs() < 1.0,
        "a row beside a right picture starts at the column's edge: {} vs {left}",
        first.x0
    );
    assert!(
        first.x1 <= picture.x0 - 8.0,
        "a row beside a right picture ends before it: row {} picture {}",
        first.x1,
        picture.x0
    );
    let below = rows
        .iter()
        .find(|row| row.below(picture.y1))
        .expect("the words run past the picture's bottom");
    assert!(
        below.x1 > picture.x0 + 40.0,
        "a row below a right picture runs past where the picture ended: row {} picture {}",
        below.x1,
        picture.x0
    );
}

#[test]
fn a_click_on_a_narrowed_row_places_the_caret_in_the_character_clicked() {
    let mut fixture = FloatEditor::beside_picture(520.0, "![Scan|200|wrap:left](scan.png)");
    let picture = fixture.editor.image_preview_rects[0].expect("the picture is placed");
    let rows = fixture.rows(1);
    let row = &rows[0];
    assert!(
        row.x0 > picture.x1,
        "the row is narrowed beside the picture"
    );
    // A character well inside the row, so the click is on its own glyph rather than on an edge.
    let byte = row.bytes.start.saturating_add(20);
    let caret = fixture.editor.blocks[1]
        .line
        .caret_rect(&fixture.theme, byte);
    fixture.click(Point::new(caret.x0 + 1.0, caret.y0 + 2.0));
    assert_eq!(
        fixture.editor.focused,
        Some(1),
        "the click lands in the words beside the picture"
    );
    assert_eq!(
        fixture.editor.blocks[1].line.cursor_pos(),
        byte,
        "the caret stands in the character clicked"
    );
}

#[test]
fn a_picture_that_does_not_fit_moves_to_the_next_page_with_its_anchor() {
    let page = Pages::of("Letter", "", "").unwrap();
    // What a sheet leaves to write on, read from a page of the same document.
    let mut fixture = FloatEditor::with_blocks(520.0, filled(0));
    fixture.editor.set_pages(Some(page.clone()));
    fixture.frame();
    let grid = &fixture.editor.stack.pages;
    let content = grid.sheet - grid.margin_top - grid.margin_bottom;
    let height = clip_height(&fixture);

    // The fewest one-line paragraphs that put the picture past the first page's writing: where
    // the flow alone would leave no room for its own height.
    let (mut low, mut high) = (0usize, 200usize);
    while low < high {
        let middle = low + (high - low) / 2;
        let mut editor = BlockEditor::new().with_wrap_lines(true);
        editor.set_blocks(filled(middle));
        let mut renderer = ui::font_catalog::fixture_renderer();
        layout_image_editor(&mut editor, 520.0, &mut renderer);
        if editor.stack.tops[middle] + height > content {
            high = middle;
        } else {
            low = middle.saturating_add(1);
        }
    }
    let filler = low;

    let mut fixture = FloatEditor::with_blocks(520.0, filled(filler));
    fixture.editor.set_pages(Some(page.clone()));
    fixture.frame();
    let grid = &fixture.editor.stack.pages;
    let picture = fixture.editor.image_preview_rects[filler].expect("the picture is placed");
    let top = fixture.editor.stack.tops[filler];
    assert!(grid.pages.len() > 1, "the document fills more than a page");
    assert!(
        (grid.pages[1] - top).abs() < 0.01,
        "a picture with no room beside the words on its page opens the next one with its block: \
         top {top} pages {:?}",
        grid.pages
    );
    let rows = fixture.rows(filler.saturating_add(1));
    let first = &rows[0];
    assert!(
        first.y >= picture.y0 - 0.5,
        "the words beside the moved picture start with it: {} vs {}",
        first.y,
        picture.y0
    );
    assert!(
        first.x0 > picture.x1,
        "the words on the new page still run beside the picture: {} picture {}",
        first.x0,
        picture.x1
    );

    /// The height the picture of `fixture` takes.
    fn clip_height(fixture: &FloatEditor) -> f32 {
        let picture = fixture.editor.image_preview_rects[0].expect("the picture is placed");
        ui::num::f32_of_f64(picture.height())
    }

    /// A page of one-line paragraphs, then the picture, then the words beside it.
    fn filled(filler: usize) -> Vec<Block> {
        let mut blocks: Vec<Block> = (0..filler)
            .map(|index| Block::paragraph(&format!("line {index}")))
            .collect();
        blocks.push(Block::paragraph("![Scan|200|wrap:left](scan.png)"));
        blocks.push(Block::paragraph(WORDS));
        blocks
    }
}

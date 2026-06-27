//! `WordProcessorWidget` — the `ui::Widget` for the word processor. Renders the
//! `qdoc` document as a centered page (à la LibreOffice Writer) using the
//! `RichTextLayout` primitive: word-wrapped paragraphs, headings, list bullets,
//! inline **bold**/*italic*/underline, a selection highlight, and a caret. Maps
//! pointer + keyboard onto the editing model (selection, formatting, save).

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use qdoc::document::MarkSpan;
use qdoc::snapshot::{AttrVal, BlockView};
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::PresenceCtx;

use crate::word_processor::{Caret, WordProcessor};
use crate::WpFormat;

// Document surface colors (a white page is the familiar word-processor look,
// independent of the OS chrome theme).
const CANVAS: Color = Color::from_rgb8(0x2b, 0x2e, 0x34);
const PAPER: Color = Color::from_rgb8(0xfc, 0xfc, 0xfa);
const SHADOW: Color = Color::from_rgb8(0x16, 0x18, 0x1c);
const INK: Color = Color::from_rgb8(0x1c, 0x1c, 0x20);
const MUTED: Color = Color::from_rgb8(0x8a, 0x8a, 0x92);
const CARET_COLOR: Color = Color::from_rgb8(0x28, 0x5a, 0xc8);
const SELECTION: Color = Color::from_rgba8(0x46, 0x78, 0xe6, 0x4d);

const PAGE_MARGIN: f64 = 64.0;
const PAGE_TOP: f64 = 28.0;
const MAX_PAGE_W: f64 = 820.0;
const BLOCK_SPACING: f64 = 8.0;
const CARET_WIDTH: f64 = 2.0;
const TOOLBAR_H: f64 = 50.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolAction {
    Bold,
    Italic,
    Underline,
    H1,
    H2,
    Body,
    Bullet,
    AlignLeft,
    AlignCenter,
    AlignRight,
    FontCycle,
    SizeUp,
    SizeDown,
    InsertTable,
}

fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices().nth(char_off).map(|(b, _)| b).unwrap_or(text.len())
}
fn byte_to_char(text: &str, byte_off: usize) -> usize {
    let b = byte_off.min(text.len());
    text[..b].chars().count()
}

/// `(font size, whole-block bold, left indent, optional bullet)`. Size is the
/// style default; per-run `fontSize` marks override it in `block_runs`.
fn block_style(b: &BlockView) -> (f32, bool, f64, Option<&'static str>) {
    match b.kind.as_str() {
        "heading" => {
            let level = match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => *n,
                _ => 1,
            };
            let size = match level {
                1 => 30.0,
                2 => 25.0,
                3 => 21.0,
                _ => 18.0,
            };
            (size, true, 0.0, None)
        }
        "list_item" => {
            let ordered = matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));
            (16.5, false, 28.0, Some(if ordered { "1." } else { "•" }))
        }
        _ => (16.5, false, 0.0, None),
    }
}

fn char_has(i: usize, marks: &[MarkSpan], name: &str) -> bool {
    marks.iter().any(|m| m.name == name && i >= m.start && i < m.end)
}

fn parse_family(s: &str) -> FontFamily {
    match s {
        "serif" => FontFamily::Serif,
        "mono" => FontFamily::Mono,
        _ => FontFamily::Sans,
    }
}

/// The `fontSize` mark value covering char `i`, if any.
fn char_font_size(i: usize, marks: &[MarkSpan]) -> Option<f32> {
    marks
        .iter()
        .find(|m| m.name == "fontSize" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_int())
        .map(|n| n as f32)
}

/// The `fontFamily` mark value covering char `i`, if any.
fn char_font_family(i: usize, marks: &[MarkSpan]) -> Option<FontFamily> {
    marks
        .iter()
        .find(|m| m.name == "fontFamily" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
        .map(parse_family)
}

/// Pick a block's horizontal alignment from its `align` attribute.
fn block_align(b: &BlockView) -> TextAlign {
    match b.attrs.get("align") {
        Some(AttrVal::Str(s)) if s == "center" => TextAlign::Center,
        Some(AttrVal::Str(s)) if s == "right" => TextAlign::Right,
        _ => TextAlign::Left,
    }
}

/// Split a block's text into styled runs (bold/italic/size/family) + underlines.
fn block_runs(b: &BlockView) -> (Vec<RichSpan>, Vec<(usize, usize)>) {
    let chars: Vec<char> = b.text.chars().collect();
    let n = chars.len();
    // Run key: f32 size as bits so the tuple is comparable.
    let key = |i: usize| {
        (
            char_has(i, &b.marks, "strong"),
            char_has(i, &b.marks, "em"),
            char_font_size(i, &b.marks).map(f32::to_bits),
            char_font_family(i, &b.marks),
        )
    };
    let mut runs = Vec::new();
    let mut i = 0;
    while i < n {
        let k = key(i);
        let mut j = i;
        while j < n && key(j) == k {
            j += 1;
        }
        let (bold, italic, size_bits, family) = k;
        runs.push(RichSpan {
            text: chars[i..j].iter().collect(),
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    let underlines = b
        .marks
        .iter()
        .filter(|m| m.name == "underline")
        .map(|m| (char_to_byte(&b.text, m.start), char_to_byte(&b.text, m.end)))
        .collect();
    (runs, underlines)
}

/// A block laid out for the current frame; kept for click hit-testing.
struct Laid {
    id: String,
    text: String,
    kind: String,
    size: f32,
    y_top: f64,
    text_x: f64,
    width: f64,
    height: f64,
    layout: RichTextLayout,
    bullet: Option<String>,
    /// Full grid-cell rectangle (table cells only) — for borders + precise hit.
    cell_box: Option<Rect>,
}

fn cell_table(b: &BlockView) -> Option<String> {
    match b.attrs.get("table") {
        Some(AttrVal::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn cell_rc(b: &BlockView) -> (usize, usize) {
    let get = |k: &str| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) if *n >= 0 => *n as usize,
        _ => 0,
    };
    (get("row"), get("col"))
}

pub struct WordProcessorWidget {
    inner: WordProcessor,
    rect: Rect,
    focused: bool,
    dirty: bool,
    needs_commit: bool,
    pointer: Point,
    dragging: bool,
    path: Option<PathBuf>,
    format: Option<WpFormat>,
    blocks: Vec<BlockView>,
    laid: Vec<Laid>,
    toolbar: Vec<(ToolAction, Rect)>,
}

impl Default for WordProcessorWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl WordProcessorWidget {
    pub fn new() -> Self {
        Self {
            inner: WordProcessor::new(),
            rect: Rect::ZERO,
            focused: true,
            dirty: true,
            needs_commit: false,
            pointer: Point::ZERO,
            dragging: false,
            path: None,
            format: None,
            blocks: Vec::new(),
            laid: Vec::new(),
            toolbar: Vec::new(),
        }
    }

    pub fn model(&self) -> &WordProcessor {
        &self.inner
    }
    pub fn model_mut(&mut self) -> &mut WordProcessor {
        &mut self.inner
    }

    pub fn set_file(&mut self, path: PathBuf, format: WpFormat) {
        self.path = Some(path);
        self.format = Some(format);
    }

    fn save(&self, state: &AutoCommit) {
        if let (Some(path), Some(format), Some(c)) =
            (self.path.as_ref(), self.format, self.inner.containers.as_ref())
        {
            let dr = qdoc::DocRef { doc: state, c };
            if let Ok(bytes) = format.export(dr) {
                let _ = std::fs::write(path, bytes);
            }
        }
    }

    fn refresh_blocks(&mut self, state: &AutoCommit) {
        if let Some(c) = self.inner.containers.as_ref() {
            self.blocks = qdoc::model::materialize(state, c).blocks;
        }
    }

    /// Resolve a pointer position to a caret and apply it (optionally extending
    /// the selection for click-drag).
    fn caret_from_pointer(&mut self, p: Point, extend: bool) {
        // Find the laid block under the pointer: cells hit by their full box (so
        // the right column is chosen), normal blocks by their vertical band.
        let mut hit: Option<(String, usize)> = None;
        for l in &self.laid {
            let inside = match l.cell_box {
                Some(bx) => bx.contains(p),
                None => p.y >= l.y_top && p.y < l.y_top + l.height,
            };
            if inside {
                let byte = l.layout.hit((p.x - l.text_x) as f32, (p.y - l.y_top) as f32);
                hit = Some((l.id.clone(), byte_to_char(&l.text, byte)));
                break;
            }
        }
        if hit.is_none() {
            // above first / below last
            if let Some(l) = self.laid.first() {
                if p.y < l.y_top {
                    hit = Some((l.id.clone(), 0));
                }
            }
            if hit.is_none() {
                if let Some(l) = self.laid.last() {
                    let byte = l.layout.hit((p.x - l.text_x) as f32, (l.height - 1.0) as f32);
                    hit = Some((l.id.clone(), byte_to_char(&l.text, byte)));
                }
            }
        }
        if let Some((block, offset)) = hit {
            if extend {
                self.inner.set_anchor();
            } else {
                self.inner.clear_anchor();
            }
            self.inner.set_caret(Caret { block, offset });
            self.dirty = true;
        }
    }
}

impl Widget for WordProcessorWidget {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.inner.bind(state, root);
        self.refresh_blocks(state);
        self.dirty = true;
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        let full = self.rect;
        let tb = Rect::new(full.x0, full.y0, full.x1, full.y0 + TOOLBAR_H);
        self.draw_toolbar(scene, transform, text_renderer, theme, tb);

        let canvas = Rect::new(full.x0, full.y0 + TOOLBAR_H, full.x1, full.y1);
        scene.fill(Fill::NonZero, transform, CANVAS, None, &canvas);

        let page_w = (canvas.width() - 2.0 * PAGE_TOP).clamp(320.0, MAX_PAGE_W);
        let page_x = canvas.x0 + ((canvas.width() - page_w) / 2.0).max(0.0);
        let page_top = canvas.y0 + PAGE_TOP;
        let content_x = page_x + PAGE_MARGIN;
        let content_w = (page_w - 2.0 * PAGE_MARGIN) as f32;

        // Pass 1: shape blocks top-to-bottom. Runs of consecutive `cell` blocks
        // sharing a table id lay out as a grid; everything else stacks normally.
        let blocks = std::mem::take(&mut self.blocks);
        let mut laid: Vec<Laid> = Vec::with_capacity(blocks.len());
        let mut y = page_top + PAGE_MARGIN;
        let mut ol_count = 0u32;
        let cell_pad = 8.0f64;
        let mut bi = 0;
        while bi < blocks.len() {
            let b = &blocks[bi];
            if let Some(tid) = cell_table(b) {
                ol_count = 0;
                let start = bi;
                while bi < blocks.len() && cell_table(&blocks[bi]).as_deref() == Some(tid.as_str()) {
                    bi += 1;
                }
                let cells = &blocks[start..bi];
                let ncols = cells.iter().map(|c| cell_rc(c).1).max().unwrap_or(0) + 1;
                let nrows = cells.iter().map(|c| cell_rc(c).0).max().unwrap_or(0) + 1;
                let col_w = content_w as f64 / ncols as f64;
                for r in 0..nrows {
                    let mut row_items: Vec<(usize, RichTextLayout, &BlockView)> = Vec::new();
                    let mut row_h = 26.0f64;
                    for c in 0..ncols {
                        if let Some(cell) = cells.iter().find(|cl| cell_rc(cl) == (r, c)) {
                            let (runs, underlines) = block_runs(cell);
                            let layout = RichTextLayout::shape(
                                &mut text_renderer.font_system,
                                &runs,
                                false,
                                16.0,
                                (col_w - 2.0 * cell_pad).max(24.0) as f32,
                                block_align(cell),
                                FontFamily::Sans,
                                underlines,
                            );
                            row_h = row_h.max(layout.height() as f64 + 2.0 * cell_pad);
                            row_items.push((c, layout, cell));
                        }
                    }
                    for (c, layout, cell) in row_items {
                        let cell_x = content_x + c as f64 * col_w;
                        let cbox = Rect::new(cell_x, y, cell_x + col_w, y + row_h);
                        laid.push(Laid {
                            id: cell.id.clone(),
                            text: cell.text.clone(),
                            kind: "cell".into(),
                            size: 16.0,
                            y_top: y + cell_pad,
                            text_x: cell_x + cell_pad,
                            width: col_w - 2.0 * cell_pad,
                            height: layout.height() as f64,
                            layout,
                            bullet: None,
                            cell_box: Some(cbox),
                        });
                    }
                    y += row_h;
                }
                y += BLOCK_SPACING;
            } else {
                let (size, base_bold, indent, _bullet) = block_style(b);
                let bullet = if b.kind == "list_item" {
                    if matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true))) {
                        ol_count += 1;
                        Some(format!("{ol_count}."))
                    } else {
                        ol_count = 0;
                        Some("•".to_string())
                    }
                } else {
                    ol_count = 0;
                    None
                };
                let wrap_w = (content_w - indent as f32).max(40.0);
                let (runs, underlines) = block_runs(b);
                let layout = RichTextLayout::shape(
                    &mut text_renderer.font_system,
                    &runs,
                    base_bold,
                    size,
                    wrap_w,
                    block_align(b),
                    FontFamily::Sans,
                    underlines,
                );
                laid.push(Laid {
                    id: b.id.clone(),
                    text: b.text.clone(),
                    kind: b.kind.clone(),
                    size,
                    y_top: y,
                    text_x: content_x + indent,
                    width: wrap_w as f64,
                    height: layout.height() as f64,
                    layout,
                    bullet,
                    cell_box: None,
                });
                y += laid.last().unwrap().height + BLOCK_SPACING;
                bi += 1;
            }
        }
        let content_bottom = y - BLOCK_SPACING + PAGE_MARGIN;
        let page_h = (content_bottom - page_top).max(canvas.height() - 2.0 * PAGE_TOP);

        // Page shadow + paper.
        let shadow = Rect::new(page_x - 1.0, page_top + 4.0, page_x + page_w + 1.0, page_top + page_h + 6.0);
        scene.fill(Fill::NonZero, transform, SHADOW, None, &vello::kurbo::RoundedRect::from_rect(shadow, 4.0));
        let page = Rect::new(page_x, page_top, page_x + page_w, page_top + page_h);
        scene.fill(Fill::NonZero, transform, PAPER, None, &vello::kurbo::RoundedRect::from_rect(page, 3.0));

        // Pass 2: draw cell borders, bullets, selection, text, caret.
        for l in &laid {
            if let Some(bx) = l.cell_box {
                scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, Color::from_rgb8(0xc4, 0xc6, 0xcc), None, &bx);
            }

            if l.kind == "fidelity" {
                let box_rect = Rect::new(l.text_x, l.y_top + 2.0, (page_x + page_w - PAGE_MARGIN).min(l.text_x + 360.0), l.y_top + l.height - 2.0);
                scene.fill(Fill::NonZero, transform, Color::from_rgb8(0xee, 0xee, 0xf2), None, &vello::kurbo::RoundedRect::from_rect(box_rect, 4.0));
                text_renderer.draw_text(scene, "⟦ embedded content ⟧", 14.0, MUTED, transform * Affine::translate((l.text_x + 10.0, l.y_top + 6.0)), None);
                continue;
            }

            if let Some(bul) = &l.bullet {
                text_renderer.draw_text(scene, bul, l.size, INK, transform * Affine::translate((l.text_x - 24.0, l.y_top)), None);
            }

            if let Some((b0, b1)) = self.selection_bytes_for(&l.id, &l.text) {
                for (rx0, ry0, rx1, ry1) in l.layout.selection_rects(b0, b1) {
                    let r = Rect::new(l.text_x + rx0 as f64, l.y_top + ry0 as f64, l.text_x + rx1 as f64, l.y_top + ry1 as f64);
                    scene.fill(Fill::NonZero, transform, SELECTION, None, &r);
                }
            }

            text_renderer.draw_rich(scene, &l.layout, transform * Affine::translate((l.text_x, l.y_top)), INK);

            if self.focused && l.id == self.inner.caret.block && !self.inner.has_selection() {
                let byte = char_to_byte(&l.text, self.inner.caret.offset);
                let (cx, cy, ch) = l.layout.caret_geometry(byte);
                let caret = Rect::new(l.text_x + cx as f64, l.y_top + cy as f64, l.text_x + cx as f64 + CARET_WIDTH, l.y_top + cy as f64 + ch as f64);
                scene.fill(Fill::NonZero, transform, CARET_COLOR, None, &caret);
            }
        }

        self.blocks = blocks;
        self.laid = laid;
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &Modifiers, state: &mut AutoCommit, _path: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
                if self.dragging {
                    self.caret_from_pointer(self.pointer, true);
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if !self.rect.contains(self.pointer) {
                    return;
                }
                self.focused = true;
                if self.pointer.y < self.rect.y0 + TOOLBAR_H {
                    let clicked = self.toolbar.iter().find(|(_, r)| r.contains(self.pointer)).map(|(a, _)| *a);
                    if let Some(action) = clicked {
                        self.apply_tool(state, action);
                        self.refresh_blocks(state);
                        self.dirty = true;
                        self.needs_commit = true;
                    }
                    *handled = true;
                    return;
                }
                self.dragging = true;
                self.caret_from_pointer(self.pointer, shift);
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.dragging = false;
            }
            WindowEvent::KeyboardInput { event: KeyEvent { state: ElementState::Pressed, logical_key, text, .. }, .. } => {
                if !self.focused {
                    return;
                }
                if ctrl {
                    if let Key::Character(s) = logical_key {
                        match s.as_str() {
                            "s" => self.save(state),
                            "b" => self.inner.toggle_mark(state, "strong"),
                            "i" => self.inner.toggle_mark(state, "em"),
                            "u" => self.inner.toggle_mark(state, "underline"),
                            "c" => self.clipboard_copy(),
                            "x" => {
                                self.clipboard_copy();
                                self.inner.delete_selection(state);
                            }
                            "v" => self.clipboard_paste(state),
                            "a" => self.select_all_in_block(state),
                            // z / y (undo/redo) are handled by the host App.
                            _ => {}
                        }
                        self.refresh_blocks(state);
                        self.dirty = true;
                        self.needs_commit = true;
                        *handled = true;
                        return;
                    }
                }

                let mut changed = true;
                let mut motion = false;
                match logical_key {
                    Key::Named(NamedKey::Tab) => {
                        if let Some(next) = self.tab_cell(!shift) {
                            self.inner.clear_anchor();
                            self.inner.set_caret(Caret { block: next, offset: 0 });
                            motion = true;
                        } else {
                            self.inner.insert_str(state, "    ");
                        }
                    }
                    Key::Named(NamedKey::Enter) => self.inner.enter(state),
                    Key::Named(NamedKey::Backspace) => self.inner.backspace(state),
                    Key::Named(NamedKey::Delete) => self.inner.delete_forward(state),
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.extend_or_clear(shift);
                        self.inner.move_left(state);
                        motion = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        self.extend_or_clear(shift);
                        self.inner.move_right(state);
                        motion = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.extend_or_clear(shift);
                        self.inner.move_block_relative(state, -1);
                        motion = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.extend_or_clear(shift);
                        self.inner.move_block_relative(state, 1);
                        motion = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        self.extend_or_clear(shift);
                        self.inner.move_home();
                        motion = true;
                    }
                    Key::Named(NamedKey::End) => {
                        self.extend_or_clear(shift);
                        self.inner.move_end(state);
                        motion = true;
                    }
                    Key::Named(NamedKey::Space) => self.inner.insert_str(state, " "),
                    _ => {
                        let typed: String = text
                            .as_ref()
                            .map(|t| t.chars().filter(|c| !c.is_control()).collect())
                            .unwrap_or_default();
                        if typed.is_empty() {
                            changed = false;
                        } else {
                            self.inner.insert_str(state, &typed);
                        }
                    }
                }
                if changed {
                    if !motion {
                        // editing consumes/collapses any selection
                        self.inner.clear_anchor();
                    }
                    self.refresh_blocks(state);
                    self.dirty = true;
                    self.needs_commit = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "WordProcessor"
    }
    fn test_id(&self) -> Option<&str> {
        Some("word_processor")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        self.dirty = true;
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn needs_commit(&self) -> bool {
        self.needs_commit
    }
    fn on_commit(&mut self) {
        self.needs_commit = false;
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl WordProcessorWidget {
    fn block_index_cached(&self, block: &str) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == block)
    }

    /// Selection endpoints in document order, computed from the block cache
    /// (so it works during `draw`, which has no `state`).
    fn selection_span_cached(&self) -> Option<(Caret, Caret)> {
        let anchor = self.inner.selection_anchor.clone()?;
        let caret = self.inner.caret.clone();
        if anchor == caret {
            return None;
        }
        match (self.block_index_cached(&anchor.block), self.block_index_cached(&caret.block)) {
            (Some(pa), Some(pc)) => {
                if (pa, anchor.offset) <= (pc, caret.offset) {
                    Some((anchor, caret))
                } else {
                    Some((caret, anchor))
                }
            }
            _ => None,
        }
    }

    /// Per-block selected char ranges spanning the selection (cache-based).
    fn selection_blocks_cached(&self) -> Vec<(String, usize, usize)> {
        let Some((start, end)) = self.selection_span_cached() else {
            return vec![];
        };
        let (Some(si), Some(ei)) = (self.block_index_cached(&start.block), self.block_index_cached(&end.block)) else {
            return vec![];
        };
        (si..=ei)
            .map(|i| {
                let b = &self.blocks[i];
                let blen = b.text.chars().count();
                let s = if i == si { start.offset } else { 0 };
                let e = if i == ei { end.offset } else { blen };
                (b.id.clone(), s, e)
            })
            .collect()
    }

    fn selection_bytes_for(&self, block_id: &str, text: &str) -> Option<(usize, usize)> {
        let (_, s, e) = self.selection_blocks_cached().into_iter().find(|(b, _, _)| b == block_id)?;
        if s >= e {
            return None;
        }
        Some((char_to_byte(text, s), char_to_byte(text, e)))
    }

    fn extend_or_clear(&mut self, shift: bool) {
        if shift {
            self.inner.set_anchor();
        } else {
            self.inner.clear_anchor();
        }
    }

    fn mark_active_in_selection(&self, name: &str) -> bool {
        let sel: Vec<_> = self.selection_blocks_cached().into_iter().filter(|(_, s, e)| s < e).collect();
        if sel.is_empty() {
            return false;
        }
        sel.iter().all(|(bid, s, e)| {
            self.blocks
                .iter()
                .find(|b| &b.id == bid)
                .map(|b| b.marks.iter().any(|m| m.name == name && m.start <= *s && m.end >= *e))
                .unwrap_or(false)
        })
    }

    fn current_block_style(&self) -> (String, Option<i64>) {
        let b = self.blocks.iter().find(|b| b.id == self.inner.caret.block);
        let kind = b.map(|b| b.kind.clone()).unwrap_or_else(|| "paragraph".into());
        let level = b.and_then(|b| match b.attrs.get("level") {
            Some(AttrVal::Int(n)) => Some(*n),
            _ => None,
        });
        (kind, level)
    }

    fn apply_tool(&mut self, state: &mut AutoCommit, action: ToolAction) {
        match action {
            ToolAction::Bold => self.inner.toggle_mark(state, "strong"),
            ToolAction::Italic => self.inner.toggle_mark(state, "em"),
            ToolAction::Underline => self.inner.toggle_mark(state, "underline"),
            ToolAction::H1 => self.inner.set_paragraph_style(state, "heading", Some(1), None),
            ToolAction::H2 => self.inner.set_paragraph_style(state, "heading", Some(2), None),
            ToolAction::Body => self.inner.set_paragraph_style(state, "paragraph", None, None),
            ToolAction::Bullet => self.inner.set_paragraph_style(state, "list_item", None, Some(false)),
            ToolAction::AlignLeft => self.inner.set_align(state, "left"),
            ToolAction::AlignCenter => self.inner.set_align(state, "center"),
            ToolAction::AlignRight => self.inner.set_align(state, "right"),
            ToolAction::FontCycle => {
                let name = match self.current_font().next() {
                    FontFamily::Sans => "sans",
                    FontFamily::Serif => "serif",
                    FontFamily::Mono => "mono",
                };
                self.inner.set_font_family(state, name);
            }
            ToolAction::SizeUp => {
                let s = ((self.current_size() + 2.0).round() as i64).min(200);
                self.inner.set_font_size(state, s);
            }
            ToolAction::SizeDown => {
                let s = ((self.current_size() - 2.0).round() as i64).max(6);
                self.inner.set_font_size(state, s);
            }
            ToolAction::InsertTable => self.inner.insert_table(state, 2, 2),
        }
    }

    /// The next/previous cell id within the caret cell's table, if any.
    fn tab_cell(&self, forward: bool) -> Option<String> {
        let idx = self.blocks.iter().position(|b| b.id == self.inner.caret.block)?;
        let table = cell_table(&self.blocks[idx])?;
        let same = |b: &BlockView| cell_table(b).as_deref() == Some(table.as_str());
        if forward {
            self.blocks[idx + 1..].iter().find(|b| same(b)).map(|b| b.id.clone())
        } else {
            self.blocks[..idx].iter().rev().find(|b| same(b)).map(|b| b.id.clone())
        }
    }

    /// The char position the toolbar reports font state for: just left of the
    /// caret (so it reflects the run you're typing into), else the caret.
    fn font_probe(&self) -> Option<(&BlockView, usize)> {
        let b = self.blocks.iter().find(|b| b.id == self.inner.caret.block)?;
        let pos = self.inner.caret.offset.saturating_sub(1);
        Some((b, pos))
    }

    fn current_font(&self) -> FontFamily {
        self.font_probe()
            .and_then(|(b, pos)| char_font_family(pos, &b.marks))
            .unwrap_or_default()
    }

    fn current_size(&self) -> f32 {
        match self.font_probe() {
            Some((b, pos)) => char_font_size(pos, &b.marks).unwrap_or_else(|| block_style(b).0),
            None => 16.5,
        }
    }

    fn current_align(&self) -> String {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .and_then(|b| match b.attrs.get("align") {
                Some(AttrVal::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "left".into())
    }

    fn selection_text(&self) -> Option<String> {
        let sel = self.selection_blocks_cached();
        if sel.is_empty() {
            return None;
        }
        let parts: Vec<String> = sel
            .into_iter()
            .map(|(bid, s, e)| {
                self.blocks
                    .iter()
                    .find(|b| b.id == bid)
                    .map(|b| b.text.chars().skip(s).take(e.saturating_sub(s)).collect::<String>())
                    .unwrap_or_default()
            })
            .collect();
        Some(parts.join("\n"))
    }

    fn clipboard_copy(&self) {
        if let Some(t) = self.selection_text() {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(t);
            }
        }
    }

    fn clipboard_paste(&mut self, state: &mut AutoCommit) {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(t) = cb.get_text() {
                // Multi-paragraph paste: newlines split into new paragraphs.
                self.inner.insert_multiline(state, &t);
            }
        }
    }

    fn select_all_in_block(&mut self, state: &AutoCommit) {
        self.inner.move_home();
        self.inner.clear_anchor();
        self.inner.set_anchor();
        self.inner.move_end(state);
    }

    fn draw_toolbar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, tb: Rect) {
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &tb);
        let border = Rect::new(tb.x0, tb.y1 - 1.0, tb.x1, tb.y1);
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.border_standard, None, &border);

        let bold_a = self.mark_active_in_selection("strong");
        let italic_a = self.mark_active_in_selection("em");
        let under_a = self.mark_active_in_selection("underline");
        let (kind, level) = self.current_block_style();
        let align = self.current_align();
        let font_label = self.current_font().label();

        let white = Color::from_rgb8(0xff, 0xff, 0xff);
        let buttons: [(ToolAction, &str, f64, bool, bool); 14] = [
            (ToolAction::Bold, "B", 34.0, bold_a, true),
            (ToolAction::Italic, "I", 30.0, italic_a, false),
            (ToolAction::Underline, "U", 32.0, under_a, false),
            (ToolAction::H1, "H1", 42.0, kind == "heading" && level == Some(1), false),
            (ToolAction::H2, "H2", 42.0, kind == "heading" && level == Some(2), false),
            (ToolAction::Body, "Body", 58.0, kind == "paragraph", false),
            (ToolAction::Bullet, "• List", 66.0, kind == "list_item", false),
            (ToolAction::AlignLeft, "L", 30.0, align == "left", false),
            (ToolAction::AlignCenter, "C", 30.0, align == "center", false),
            (ToolAction::AlignRight, "R", 30.0, align == "right", false),
            (ToolAction::FontCycle, font_label, 58.0, false, false),
            (ToolAction::SizeDown, "A−", 34.0, false, false),
            (ToolAction::SizeUp, "A+", 34.0, false, false),
            (ToolAction::InsertTable, "⊞ Table", 78.0, kind == "cell", false),
        ];

        self.toolbar.clear();
        let cy = tb.y0 + TOOLBAR_H / 2.0;
        let bh = 34.0;
        let mut x = tb.x0 + 14.0;
        for (i, (action, label, w, active, bold_label)) in buttons.iter().enumerate() {
            if i == 3 || i == 7 || i == 10 || i == 13 {
                let sep = Rect::new(x + 2.0, cy - 12.0, x + 3.0, cy + 12.0);
                scene.fill(Fill::NonZero, transform, theme.tokens.colors.border_standard, None, &sep);
                x += 14.0;
            }
            let r = Rect::new(x, cy - bh / 2.0, x + w, cy + bh / 2.0);
            if *active {
                scene.fill(Fill::NonZero, transform, theme.accent_primary, None, &vello::kurbo::RoundedRect::from_rect(r, 6.0));
            }
            let color = if *active { white } else { theme.text_primary };
            let (lw, _) = tr.measure(label, 15.0, None);
            let lx = x + (*w - lw as f64) / 2.0;
            let ly = cy - 9.0;
            if *bold_label {
                tr.draw_text_weighted(scene, label, 15.0, color, transform * Affine::translate((lx, ly)), None, ui::text_render::TextWeight::Bold);
            } else {
                tr.draw_text(scene, label, 15.0, color, transform * Affine::translate((lx, ly)), None);
            }
            self.toolbar.push((*action, r));
            x += w + 6.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::AvailableSpace;
    use ui::permissions::PermissionContext;

    fn pipeline_draw(wp: &mut WordProcessorWidget, state: &mut ui::State, w: u32, h: u32) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        wp.sync(&mut state.doc, &state.root);
        let child = wp.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        wp.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        wp.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
    }

    #[test]
    fn draw_lays_out_all_blocks() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Hello");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "World");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.laid.len(), 2, "draw laid out both paragraphs");
    }

    #[test]
    fn draw_renders_headings_and_bold() {
        use qdoc::{FormatProvider, MarkdownCodec};
        let mut state = ui::State::new();
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &c, ids: &mut ids };
            MarkdownCodec.import(b"# Title\n\nHello **bold** world\n", &mut dm).unwrap();
        }
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(wp.laid.len() >= 2);
        assert!(wp.blocks.iter().any(|b| b.kind == "heading"));
        assert!(wp.blocks.iter().any(|b| b.marks.iter().any(|m| m.name == "strong")));
    }
}

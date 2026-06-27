//! `WordProcessorWidget` — the `ui::Widget` that renders a `qdoc` document and
//! maps keyboard/pointer events onto the [`WordProcessor`] editing model.
//!
//! Rendering is one block per line (headings larger + bold, paragraphs, list
//! items with a bullet), with inline **bold** marks drawn as separate weighted
//! runs and a caret. Word-wrap and selection are follow-ups; the document model
//! and round-tripping are already complete and tested.

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use vello::Scene;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use qdoc::document::MarkSpan;
use qdoc::snapshot::{AttrVal, BlockView};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use ui::PresenceCtx;

use crate::word_processor::{Caret, WordProcessor};
use crate::WpFormat;

const BLOCK_SPACING: f64 = 6.0;
const CARET_WIDTH: f64 = 2.0;

pub struct WordProcessorWidget {
    inner: WordProcessor,
    rect: Rect,
    focused: bool,
    dirty: bool,
    needs_commit: bool,
    /// Materialized blocks, refreshed in `sync`.
    blocks: Vec<BlockView>,
    /// `(block_id, y_top, line_height)` recorded during `draw`, for click→caret.
    block_bounds: Vec<(String, f64, f64)>,
    /// Last pointer position (logical px), tracked for click resolution.
    pointer: Point,
    /// Backing file + format, for Ctrl+S save.
    path: Option<PathBuf>,
    format: Option<WpFormat>,
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
            blocks: Vec::new(),
            block_bounds: Vec::new(),
            pointer: Point::ZERO,
            path: None,
            format: None,
        }
    }

    /// Access the editing model (tests / host integration).
    pub fn model(&self) -> &WordProcessor {
        &self.inner
    }
    pub fn model_mut(&mut self) -> &mut WordProcessor {
        &mut self.inner
    }

    /// Associate a backing file + format so Ctrl+S can save.
    pub fn set_file(&mut self, path: PathBuf, format: WpFormat) {
        self.path = Some(path);
        self.format = Some(format);
    }

    /// Export the document to its backing file (Ctrl+S). No-op if unset.
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

    fn text_pad(&self) -> f64 {
        16.0
    }
}

fn block_style(b: &BlockView) -> (f32, bool, String) {
    match b.kind.as_str() {
        "heading" => {
            let level = match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => *n,
                _ => 1,
            };
            let size = match level {
                1 => 28.0,
                2 => 24.0,
                3 => 20.0,
                _ => 18.0,
            };
            (size, true, String::new())
        }
        "list_item" => {
            let ordered = matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));
            (16.0, false, if ordered { "1. ".into() } else { "• ".into() })
        }
        _ => (16.0, false, String::new()),
    }
}

fn char_bold(i: usize, marks: &[MarkSpan]) -> bool {
    marks.iter().any(|m| m.name == "strong" && i >= m.start && i < m.end)
}

/// Draw `text` starting at `x`, splitting into weighted runs at bold boundaries.
/// Returns the advance width.
#[allow(clippy::too_many_arguments)]
fn draw_runs(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    color: vello::peniko::Color,
    transform: Affine,
    text: &str,
    marks: &[MarkSpan],
    x: f64,
    y: f64,
    size: f32,
    base_bold: bool,
) -> f64 {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut cx = x;
    let mut i = 0;
    while i < n {
        let bold = base_bold || char_bold(i, marks);
        let mut j = i;
        while j < n && (base_bold || char_bold(j, marks)) == bold {
            j += 1;
        }
        let run: String = chars[i..j].iter().collect();
        let weight = if bold { TextWeight::Bold } else { TextWeight::Regular };
        tr.draw_text_weighted(scene, &run, size, color, transform * Affine::translate((cx, y)), None, weight);
        cx += tr.measure_weighted(&run, size, None, weight).0 as f64;
        i = j;
    }
    cx - x
}

/// X-offset of `offset` chars into `text`, honoring bold-run widths.
fn x_for_offset(tr: &mut TextRenderer, text: &str, marks: &[MarkSpan], size: f32, base_bold: bool, offset: usize) -> f64 {
    let chars: Vec<char> = text.chars().collect();
    let off = offset.min(chars.len());
    let mut x = 0.0;
    let mut i = 0;
    while i < off {
        let bold = base_bold || char_bold(i, marks);
        let mut j = i;
        while j < off && (base_bold || char_bold(j, marks)) == bold {
            j += 1;
        }
        let run: String = chars[i..j].iter().collect();
        let weight = if bold { TextWeight::Bold } else { TextWeight::Regular };
        x += tr.measure_weighted(&run, size, None, weight).0 as f64;
        i = j;
    }
    x
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
        // Paper background.
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &self.rect);

        let pad = self.text_pad();
        let content_x = self.rect.x0 + pad;
        let mut y = self.rect.y0 + pad;

        let blocks = std::mem::take(&mut self.blocks);
        let mut bounds = Vec::with_capacity(blocks.len());
        for block in &blocks {
            let (size, base_bold, prefix) = block_style(block);
            let lh = size as f64 * 1.4;
            bounds.push((block.id.clone(), y, lh));

            let mut x = content_x;
            if !prefix.is_empty() {
                text_renderer.draw_text(scene, &prefix, size, theme.text_primary, transform * Affine::translate((x, y)), None);
                x += text_renderer.measure(&prefix, size, None).0 as f64;
            }
            if block.kind == "fidelity" {
                text_renderer.draw_text(scene, "⟦embedded⟧", size, theme.text_primary, transform * Affine::translate((x, y)), None);
            } else {
                draw_runs(scene, text_renderer, theme.text_primary, transform, &block.text, &block.marks, x, y, size, base_bold);
            }

            if self.focused && block.id == self.inner.caret.block {
                let cx = x + x_for_offset(text_renderer, &block.text, &block.marks, size, base_bold, self.inner.caret.offset);
                let caret = Rect::new(cx, y, cx + CARET_WIDTH, y + lh);
                scene.fill(Fill::NonZero, transform, theme.accent_primary, None, &caret);
            }

            y += lh + BLOCK_SPACING;
        }
        self.blocks = blocks;
        self.block_bounds = bounds;
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &Modifiers, state: &mut AutoCommit, _path: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.rect.contains(self.pointer) {
                    self.focused = true;
                    let p = self.pointer;
                    // caret_from_point needs the text renderer; defer exact x to a
                    // simple path: place on the clicked block at the nearest offset
                    // using cached measure via a transient renderer is not available
                    // here, so we approximate to block start and refine on next API.
                    self.set_caret_to_clicked_block(p);
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { state: ElementState::Pressed, logical_key, text, .. }, .. } => {
                if !self.focused {
                    return;
                }
                // Ctrl+S → save to the backing file.
                if modifiers.state().control_key() {
                    if let Key::Character(s) = logical_key {
                        if s.as_str() == "s" {
                            self.save(state);
                            *handled = true;
                            return;
                        }
                    }
                }
                let mut changed = true;
                match logical_key {
                    Key::Named(NamedKey::Enter) => self.inner.enter(state),
                    Key::Named(NamedKey::Backspace) => self.inner.backspace(state),
                    Key::Named(NamedKey::Delete) => self.inner.delete_forward(state),
                    Key::Named(NamedKey::ArrowLeft) => self.inner.move_left(state),
                    Key::Named(NamedKey::ArrowRight) => self.inner.move_right(state),
                    Key::Named(NamedKey::ArrowUp) => self.inner.move_block_relative(state, -1),
                    Key::Named(NamedKey::ArrowDown) => self.inner.move_block_relative(state, 1),
                    Key::Named(NamedKey::Home) => self.inner.move_home(),
                    Key::Named(NamedKey::End) => self.inner.move_end(state),
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
    /// Place the caret on the clicked block (offset 0). Exact intra-line offset
    /// resolution is a follow-up (needs a `TextRenderer` in the event path).
    fn set_caret_to_clicked_block(&mut self, p: Point) {
        for (id, y, lh) in &self.block_bounds {
            if p.y >= *y && p.y < *y + *lh {
                self.inner.set_caret(Caret { block: id.clone(), offset: 0 });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::AvailableSpace;
    use ui::permissions::PermissionContext;

    /// Run the real ui frame pipeline through `draw` (CPU-only; no GPU
    /// rasterization), so the heading/list/bold-run paths actually execute.
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
                Style {
                    size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w as f32),
                        height: AvailableSpace::Definite(h as f32),
                    },
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

        pipeline_draw(&mut wp, &mut state, 400, 200);
        assert_eq!(wp.block_bounds.len(), 2, "draw laid out both paragraphs");
        assert!(
            wp.block_bounds.iter().any(|(id, _, _)| id == &wp.inner.caret.block),
            "caret block is among the drawn blocks"
        );
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
        pipeline_draw(&mut wp, &mut state, 600, 400);

        assert!(wp.block_bounds.len() >= 2, "heading + paragraph laid out");
        assert!(wp.blocks.iter().any(|b| b.kind == "heading"), "a heading block is present");
        assert!(
            wp.blocks.iter().any(|b| b.marks.iter().any(|m| m.name == "strong")),
            "the bold mark survived into the render cache"
        );
    }
}

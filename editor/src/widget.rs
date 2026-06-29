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
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use qdoc::document::MarkSpan;
use qdoc::snapshot::{AttrVal, BlockView};
use ui::chrome::{self, LabelCue, ToolbarItem};
use ui::gesture::{DragSelect, Granularity};
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::PresenceCtx;

use crate::word_processor::{Caret, ClipBlock, WordProcessor};
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

const PAGE_TOP: f64 = 28.0;
const MAX_PAGE_W: f64 = 820.0;
const CARET_WIDTH: f64 = 2.0;
const TOOLBAR_H: f64 = ui::chrome::TOOLBAR_HEIGHT;
const STATUS_H: f64 = ui::chrome::STATUS_BAR_HEIGHT;
const LAYOUT_BAR_H: f64 = 32.0;
/// Accent for active layout guides + handles.
const GUIDE: Color = Color::from_rgb8(0x46, 0x78, 0xe6);
/// Inactive guide line (semi-transparent accent).
const GUIDE_FAINT: Color = Color::from_rgba8(0x46, 0x78, 0xe6, 0x70);
/// How close (px) the pointer must be to a guide line to grab it.
const GUIDE_GRAB: f64 = 7.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolAction {
    Bold,
    Italic,
    Underline,
    Strike,
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
    TableRowAdd,
    TableRowDel,
    TableColAdd,
    TableColDel,
    TableDelete,
}

/// Which layout element the bottom bar is editing. Each mode shows draggable
/// guides on the page with live preview.
#[derive(Clone, Copy, PartialEq, Debug)]
enum LayoutMode {
    None,
    Margins,
    Border,
    Spacing,
}

/// A per-side measurement (page margins / border insets), in px.
#[derive(Clone, Copy, Debug)]
struct Edges {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

impl Edges {
    fn uniform(v: f64) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }
    fn get(&self, s: Side) -> f64 {
        match s {
            Side::Top => self.top,
            Side::Right => self.right,
            Side::Bottom => self.bottom,
            Side::Left => self.left,
        }
    }
    fn set(&mut self, s: Side, v: f64) {
        match s {
            Side::Top => self.top = v,
            Side::Right => self.right = v,
            Side::Bottom => self.bottom = v,
            Side::Left => self.left = v,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

/// How a guide drag distributes to the other sides.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DragKind {
    /// Plain: move the grabbed side and its opposite symmetrically.
    Pair,
    /// Shift: move only the grabbed side.
    Single,
    /// Ctrl: scale all four sides proportionally.
    Proportional,
}

/// An active guide drag: which layout element, which side, and how it distributes.
#[derive(Clone, Copy)]
struct GuideDrag {
    mode: LayoutMode,
    side: Side,
    kind: DragKind,
}

impl Side {
    fn opposite(self) -> Side {
        match self {
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
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

/// Link blue used for `link`-marked text.
const LINK_RGB: [u8; 3] = [0x1a, 0x56, 0xc4];

/// The text colour covering char `i`: a `link` mark → link blue, else a
/// `color` mark's `#RRGGBB`.
fn char_color(i: usize, marks: &[MarkSpan]) -> Option<[u8; 3]> {
    if marks.iter().any(|m| m.name == "link" && i >= m.start && i < m.end) {
        return Some(LINK_RGB);
    }
    marks
        .iter()
        .find(|m| m.name == "color" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
        .and_then(parse_hex_rgb)
}

fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ])
}

/// Pick a block's horizontal alignment from its `align` attribute.
fn block_align(b: &BlockView, default: TextAlign) -> TextAlign {
    match b.attrs.get("align") {
        Some(AttrVal::Str(s)) if s == "center" => TextAlign::Center,
        Some(AttrVal::Str(s)) if s == "right" => TextAlign::Right,
        Some(AttrVal::Str(s)) if s == "left" => TextAlign::Left,
        _ => default,
    }
}

/// Map a style's font / align strings to the ui enums.
fn parse_font(name: &str) -> FontFamily {
    match name {
        "serif" => FontFamily::Serif,
        "mono" => FontFamily::Mono,
        _ => FontFamily::Sans,
    }
}
fn style_align(s: &str) -> TextAlign {
    match s {
        "center" => TextAlign::Center,
        "right" => TextAlign::Right,
        _ => TextAlign::Left,
    }
}

/// Resolve the style id for a block from its kind / level / explicit `style` attr.
fn block_style_key(b: &BlockView) -> String {
    let level = match b.attrs.get("level") {
        Some(AttrVal::Int(n)) => Some(*n),
        _ => None,
    };
    let explicit = match b.attrs.get("style") {
        Some(AttrVal::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    crate::styles::style_key_for(&b.kind, level, explicit)
}

/// Split a block's text into styled runs (bold/italic/size/family) + underline
/// and strikethrough byte ranges.
fn block_runs(b: &BlockView) -> (Vec<RichSpan>, Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let chars: Vec<char> = b.text.chars().collect();
    let n = chars.len();
    // Run key: f32 size as bits so the tuple is comparable.
    let key = |i: usize| {
        (
            char_has(i, &b.marks, "strong"),
            char_has(i, &b.marks, "em"),
            char_font_size(i, &b.marks).map(f32::to_bits),
            char_font_family(i, &b.marks),
            char_color(i, &b.marks),
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
        let (bold, italic, size_bits, family, color) = k;
        runs.push(RichSpan {
            text: chars[i..j].iter().collect(),
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    let deco = |name: &str| {
        b.marks
            .iter()
            .filter(|m| m.name == name)
            .map(|m| (char_to_byte(&b.text, m.start), char_to_byte(&b.text, m.end)))
            .collect::<Vec<_>>()
    };
    // Links render underlined as well as blue.
    let mut underlines = deco("underline");
    underlines.extend(deco("link"));
    (runs, underlines, deco("strike"))
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
    /// Resolved text colour from the block's style.
    ink: Color,
    /// Resolved paragraph border from the block's style.
    border: Option<crate::styles::BorderProps>,
    /// Left text indent from the block's style (for the border box).
    indent: f64,
    /// Full grid-cell rectangle (table cells only) — for borders + precise hit.
    cell_box: Option<Rect>,
    /// Grid row index (table cells only) — for header tint + zebra striping.
    cell_row: Option<usize>,
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
    drag: DragSelect,
    path: Option<PathBuf>,
    format: Option<WpFormat>,
    blocks: Vec<BlockView>,
    laid: Vec<Laid>,
    toolbar: Vec<(ToolAction, Rect)>,
    /// Last rich copy: `(plain_text, blocks)`. Paste uses it when the system
    /// clipboard still holds this exact plain text.
    rich_clip: Option<(String, Vec<ClipBlock>)>,
    /// Vertical scroll offset (px) and the max scroll (content overflow), updated
    /// each frame from the laid-out content height.
    scroll_y: f64,
    max_scroll: f64,
    /// The document's stylesheet (component-instance styles). Authoritative copy
    /// in the widget; reloaded from the doc when not mid-drag, persisted on edit.
    sheet: crate::styles::StyleSheet,
    /// Page layout — all fully customizable by dragging guides on the page.
    margins: Edges,
    border_on: bool,
    border: Edges,
    layout_mode: LayoutMode,
    /// Active guide drag (the side being dragged), if any.
    guide_drag: Option<GuideDrag>,
    /// The page rect in screen space (post-scroll), cached each frame so the
    /// event handler can hit-test layout guides against it.
    page_rect: Rect,
    /// The bottom layout bar's clickable chips: `(mode, rect)`.
    layout_chips: Vec<(LayoutMode, Rect)>,
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
            drag: DragSelect::new(),
            path: None,
            format: None,
            blocks: Vec::new(),
            laid: Vec::new(),
            toolbar: Vec::new(),
            rich_clip: None,
            scroll_y: 0.0,
            max_scroll: 0.0,
            sheet: crate::styles::StyleSheet::default(),
            margins: Edges::uniform(72.0),
            border_on: false,
            border: Edges::uniform(28.0),
            layout_mode: LayoutMode::None,
            guide_drag: None,
            page_rect: Rect::ZERO,
            layout_chips: Vec::new(),
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

/// Screen axis of a margin/border guide for `side`: `(is_vertical, coordinate)`.
fn guide_axis(page: Rect, e: Edges, side: Side) -> (bool, f64) {
    match side {
        Side::Left => (true, page.x0 + e.left),
        Side::Right => (true, page.x1 - e.right),
        Side::Top => (false, page.y0 + e.top),
        Side::Bottom => (false, page.y1 - e.bottom),
    }
}

/// New inset for `side` from a pointer position, clamped to a sane range so a
/// margin/border can never invert or swallow the whole page.
fn inset_from_pointer(page: Rect, side: Side, p: Point) -> f64 {
    let raw = match side {
        Side::Left => p.x - page.x0,
        Side::Right => page.x1 - p.x,
        Side::Top => p.y - page.y0,
        Side::Bottom => page.y1 - p.y,
    };
    let max = match side {
        Side::Left | Side::Right => (page.width() / 2.0 - 48.0).max(8.0),
        Side::Top | Side::Bottom => (page.height() / 2.0 - 48.0).max(8.0),
    };
    raw.clamp(4.0, max)
}

/// Apply a guide drag to `e`: Pair = grabbed side + its opposite, Single = just
/// the grabbed side, Proportional = scale all four by the same factor.
fn apply_edges(e: &mut Edges, page: Rect, side: Side, p: Point, kind: DragKind) {
    let v = inset_from_pointer(page, side, p);
    match kind {
        DragKind::Single => e.set(side, v),
        DragKind::Pair => {
            e.set(side, v);
            e.set(side.opposite(), v);
        }
        DragKind::Proportional => {
            let f = v / e.get(side).max(1.0);
            let max_h = (page.width() / 2.0 - 48.0).max(8.0);
            let max_v = (page.height() / 2.0 - 48.0).max(8.0);
            e.left = (e.left * f).clamp(4.0, max_h);
            e.right = (e.right * f).clamp(4.0, max_h);
            e.top = (e.top * f).clamp(4.0, max_v);
            e.bottom = (e.bottom * f).clamp(4.0, max_v);
        }
    }
}

impl WordProcessorWidget {
    /// Toggle a layout-edit mode from a chip click. The Border chip cycles
    /// off → on+edit → off so the border can be added and removed in place.
    fn toggle_layout_mode(&mut self, mode: LayoutMode) {
        if mode == LayoutMode::Border {
            if self.layout_mode == LayoutMode::Border {
                self.border_on = false;
                self.layout_mode = LayoutMode::None;
            } else {
                self.border_on = true;
                self.layout_mode = LayoutMode::Border;
            }
            return;
        }
        self.layout_mode = if self.layout_mode == mode { LayoutMode::None } else { mode };
    }

    /// The per-side measurement the active mode edits (margins or border).
    fn active_edges(&self) -> Option<Edges> {
        match self.layout_mode {
            LayoutMode::Margins => Some(self.margins),
            LayoutMode::Border => Some(self.border),
            _ => None,
        }
    }

    /// The style id of the block under the caret.
    fn focused_style_key(&self) -> Option<String> {
        self.blocks.iter().find(|b| b.id == self.inner.caret.block).map(block_style_key)
    }

    /// The `space_after` of the focused block's style.
    fn focused_space_after(&self) -> f64 {
        self.focused_style_key().map(|k| self.sheet.get(&k).space_after as f64).unwrap_or(8.0)
    }

    /// `(x-center, y)` of the spacing grip — centered in the gap *after the
    /// focused block*, so dragging it edits that block's style (every block of
    /// the same style follows).
    fn spacing_handle(&self) -> Option<(f64, f64)> {
        let l = self.laid.iter().find(|l| l.id == self.inner.caret.block).or_else(|| self.laid.first())?;
        let cx = (self.page_rect.x0 + self.page_rect.x1) / 2.0;
        Some((cx, l.y_top + l.height + self.focused_space_after() / 2.0))
    }

    /// Which guide (if any) is under the pointer, for the active layout mode.
    fn hit_guide(&self, p: Point) -> Option<Side> {
        let page = self.page_rect;
        if let Some(e) = self.active_edges() {
            let mut best: Option<(Side, f64)> = None;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let along = if vert { p.y >= page.y0 && p.y <= page.y1 } else { p.x >= page.x0 && p.x <= page.x1 };
                let d = if vert { (p.x - c).abs() } else { (p.y - c).abs() };
                if along && d <= GUIDE_GRAB && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((side, d));
                }
            }
            return best.map(|(s, _)| s);
        }
        if self.layout_mode == LayoutMode::Spacing {
            if let Some((_, hy)) = self.spacing_handle() {
                if (p.y - hy).abs() <= GUIDE_GRAB + 4.0 && p.x >= page.x0 && p.x <= page.x1 {
                    return Some(Side::Bottom); // side unused for spacing
                }
            }
        }
        None
    }

    /// Apply a live guide drag to the relevant layout value.
    fn apply_guide_drag(&mut self, gd: GuideDrag, p: Point) {
        let page = self.page_rect;
        match gd.mode {
            LayoutMode::Margins => apply_edges(&mut self.margins, page, gd.side, p, gd.kind),
            LayoutMode::Border => {
                apply_edges(&mut self.border, page, gd.side, p, gd.kind);
                self.border_on = true;
            }
            LayoutMode::Spacing => {
                // Edit the focused block's *style* spacing — all blocks of that
                // style follow (component-instance model).
                let bottom = self
                    .laid
                    .iter()
                    .find(|l| l.id == self.inner.caret.block)
                    .or_else(|| self.laid.first())
                    .map(|l| l.y_top + l.height);
                let key = self.focused_style_key();
                if let (Some(bottom), Some(key)) = (bottom, key) {
                    let after = (2.0 * (p.y - bottom)).clamp(0.0, 80.0) as f32;
                    if let Some(st) = self.sheet.styles.get_mut(&key) {
                        st.space_after = after;
                    }
                }
            }
            LayoutMode::None => {}
        }
    }

    /// Draggable guides + live value labels for the active layout mode, drawn
    /// over the page content.
    fn draw_layout_overlay(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, page: Rect) {
        if let Some(e) = self.active_edges() {
            let drag = self.guide_drag;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let active = drag
                    .map(|g| match g.kind {
                        DragKind::Proportional => true,
                        DragKind::Pair => g.side == side || g.side.opposite() == side,
                        DragKind::Single => g.side == side,
                    })
                    .unwrap_or(false);
                let col = if active { GUIDE } else { GUIDE_FAINT };
                let t = if active { 2.0 } else { 1.0 };
                let line = if vert {
                    Rect::new(c - t / 2.0, page.y0, c + t / 2.0, page.y1)
                } else {
                    Rect::new(page.x0, c - t / 2.0, page.x1, c + t / 2.0)
                };
                scene.fill(Fill::NonZero, transform, col, None, &line);
                // grip dot at the line's midpoint
                let (gx, gy) = if vert { (c, (page.y0 + page.y1) / 2.0) } else { ((page.x0 + page.x1) / 2.0, c) };
                scene.fill(Fill::NonZero, transform, GUIDE, None, &vello::kurbo::Circle::new((gx, gy), 4.0));
                // value label, offset inside the page
                let label = format!("{}", e.get(side).round() as i64);
                let (lx, ly) = if vert { (c + 6.0, page.y0 + 16.0) } else { (page.x0 + 10.0, c + 5.0) };
                tr.draw_text(scene, &label, 12.0, GUIDE, transform * Affine::translate((lx, ly)), None);
            }
        } else if self.layout_mode == LayoutMode::Spacing {
            if let Some((hx, hy)) = self.spacing_handle() {
                let pill = Rect::new(hx - 20.0, hy - 3.0, hx + 20.0, hy + 3.0);
                scene.fill(Fill::NonZero, transform, GUIDE, None, &vello::kurbo::RoundedRect::from_rect(pill, 3.0));
                let label = format!("{} px", self.focused_space_after().round() as i64);
                tr.draw_text(scene, &label, 12.0, GUIDE, transform * Affine::translate((hx + 30.0, hy - 7.0)), None);
            }
        }
    }

    /// The bottom layout bar: mode chips + a live readout / hint.
    fn draw_layout_bar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, _theme: &Theme, bar: Rect) {
        scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x20, 0x22, 0x28), None, &bar);
        scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x32, 0x35, 0x3e), None, &Rect::new(bar.x0, bar.y0, bar.x1, bar.y0 + 1.0));

        let cy = (bar.y0 + bar.y1) / 2.0;
        let mut x = bar.x0 + 12.0;
        self.layout_chips.clear();
        for (mode, label) in [(LayoutMode::Margins, "Margins"), (LayoutMode::Border, "Border"), (LayoutMode::Spacing, "Spacing")] {
            let w = 16.0 + label.chars().count() as f64 * 7.6;
            let r = Rect::new(x, bar.y0 + 5.0, x + w, bar.y1 - 5.0);
            let active = self.layout_mode == mode;
            let bg = if active { GUIDE } else { Color::from_rgb8(0x2c, 0x2f, 0x38) };
            let fg = if active { Color::from_rgb8(0xff, 0xff, 0xff) } else { Color::from_rgb8(0xc6, 0xca, 0xd2) };
            scene.fill(Fill::NonZero, transform, bg, None, &vello::kurbo::RoundedRect::from_rect(r, 6.0));
            tr.draw_text(scene, label, 13.0, fg, transform * Affine::translate((x + 8.0, cy - 8.0)), None);
            self.layout_chips.push((mode, r));
            x += w + 8.0;
        }

        let readout = match self.layout_mode {
            LayoutMode::Margins => format!(
                "L {}  T {}  R {}  B {}   ·   drag a guide · Ctrl-drag = all sides",
                self.margins.left.round(), self.margins.top.round(), self.margins.right.round(), self.margins.bottom.round()
            ),
            LayoutMode::Border => format!(
                "border  L {}  T {}  R {}  B {}   ·   drag a side · Ctrl-drag = all · click Border to remove",
                self.border.left.round(), self.border.top.round(), self.border.right.round(), self.border.bottom.round()
            ),
            LayoutMode::Spacing => {
                let name = self.focused_style_key().map(|k| self.sheet.get(&k).name).unwrap_or_else(|| "Body".into());
                format!("{} spacing  {} px   ·   drag the handle — every “{}” block follows", name, self.focused_space_after().round(), name)
            }
            LayoutMode::None => "Page layout — pick Margins, Border, or Spacing, then drag on the page".to_string(),
        };
        tr.draw_text(scene, &readout, 12.5, Color::from_rgb8(0x9a, 0x9e, 0xa8), transform * Affine::translate((x + 12.0, cy - 8.0)), None);
    }
}

impl Widget for WordProcessorWidget {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.inner.bind(state, root);
        self.refresh_blocks(state);
        // Reload the stylesheet from the doc unless we're mid-drag (which holds a
        // live local edit not yet persisted).
        if self.guide_drag.is_none() {
            self.sheet = self.inner.stylesheet(state);
        }
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

        let status_top = (full.y1 - STATUS_H).max(full.y0 + TOOLBAR_H);
        let layout_bar_top = (status_top - LAYOUT_BAR_H).max(full.y0 + TOOLBAR_H);
        let canvas = Rect::new(full.x0, full.y0 + TOOLBAR_H, full.x1, layout_bar_top);
        scene.fill(Fill::NonZero, transform, CANVAS, None, &canvas);

        // Per-side page geometry — every margin is independently customizable.
        // The page is a FIXED size (it does not resize with the window); the
        // window just shows more or less of it and centers it.
        let m = self.margins;
        let page_w = MAX_PAGE_W;
        let page_x = canvas.x0 + ((canvas.width() - page_w) / 2.0).max(0.0);
        let page_top = canvas.y0 + PAGE_TOP;
        let content_x = page_x + m.left;
        let content_w = (page_w - m.left - m.right).max(80.0) as f32;

        // Pass 1: shape blocks top-to-bottom. Runs of consecutive `cell` blocks
        // sharing a table id lay out as a grid; everything else stacks normally.
        let blocks = std::mem::take(&mut self.blocks);
        let mut laid: Vec<Laid> = Vec::with_capacity(blocks.len());
        let mut y = page_top + m.top;
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
                            let (runs, underlines, strikes) = block_runs(cell);
                            let layout = RichTextLayout::shape(
                                &mut text_renderer.font_system,
                                &runs,
                                false,
                                16.0,
                                (col_w - 2.0 * cell_pad).max(24.0) as f32,
                                block_align(cell, TextAlign::Left),
                                FontFamily::Sans,
                                underlines,
                                strikes,
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
                            ink: INK,
                            border: None,
                            indent: 0.0,
                            cell_box: Some(cbox),
                            cell_row: Some(r),
                        });
                    }
                    y += row_h;
                }
                y += self.sheet.get("body").space_after as f64;
            } else {
                // Resolve the block's style (component-instance model).
                let st = self.sheet.get(&block_style_key(b));
                let (size, base_bold, indent) = (st.size, st.bold, st.indent as f64);
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
                y += st.space_before as f64;
                let wrap_w = (content_w - indent as f32).max(40.0);
                let (runs, underlines, strikes) = block_runs(b);
                let layout = RichTextLayout::shape(
                    &mut text_renderer.font_system,
                    &runs,
                    base_bold,
                    size,
                    wrap_w,
                    block_align(b, style_align(&st.align)),
                    parse_font(&st.font),
                    underlines,
                    strikes,
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
                    ink: Color::from_rgb8(st.color[0], st.color[1], st.color[2]),
                    border: st.border.clone(),
                    indent,
                    cell_box: None,
                    cell_row: None,
                });
                y += laid.last().unwrap().height + st.space_after as f64;
                bi += 1;
            }
        }
        let content_bottom = y + m.bottom;
        let page_h = (content_bottom - page_top).max(canvas.height() - 2.0 * PAGE_TOP);

        // Scrolling: clamp the offset to the overflow, then shift the laid-out
        // content (+ page) into screen space so hit-testing needs no offset.
        self.max_scroll = (page_h + 2.0 * PAGE_TOP - canvas.height()).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll);
        let sy = self.scroll_y;
        if sy != 0.0 {
            for l in laid.iter_mut() {
                l.y_top -= sy;
                if let Some(bx) = l.cell_box.as_mut() {
                    *bx = Rect::new(bx.x0, bx.y0 - sy, bx.x1, bx.y1 - sy);
                }
            }
        }

        // Clip the document to the canvas so scrolled content never paints over
        // the toolbar / status bar.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, transform, &canvas);

        // Page shadow + paper (the document surface).
        let page = Rect::new(page_x, page_top - sy, page_x + page_w, page_top + page_h - sy);
        chrome::page::draw(scene, transform, page, PAPER, SHADOW);
        self.page_rect = page;

        // Page border (per-side inset). Drawn when enabled, or previewed live in
        // accent while it's being edited.
        if self.border_on || self.layout_mode == LayoutMode::Border {
            let b = self.border;
            let frame = Rect::new(page.x0 + b.left, page.y0 + b.top, page.x1 - b.right, page.y1 - b.bottom);
            let col = if self.layout_mode == LayoutMode::Border { GUIDE } else { Color::from_rgb8(0xb6, 0xb9, 0xc2) };
            scene.stroke(&vello::kurbo::Stroke::new(1.4), transform, col, None, &vello::kurbo::RoundedRect::from_rect(frame, 2.0));
        }

        // Restore the block cache before pass 2 so selection lookups resolve.
        self.blocks = blocks;
        let multi_sel = self.selection_blocks_cached().len() > 1;

        // Pass 2: draw cell borders, bullets, selection, text, caret.
        for l in &laid {
            if let Some(bx) = l.cell_box {
                let fill = match l.cell_row {
                    Some(0) => Color::from_rgb8(0xec, 0xee, 0xf3), // header row tint
                    Some(r) if r % 2 == 0 => Color::from_rgb8(0xf6, 0xf6, 0xf9), // zebra
                    _ => PAPER,
                };
                scene.fill(Fill::NonZero, transform, fill, None, &bx);
                scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, Color::from_rgb8(0xc4, 0xc6, 0xcc), None, &bx);
            }

            if l.kind == "fidelity" {
                let box_rect = Rect::new(l.text_x, l.y_top + 2.0, (page_x + page_w - m.right).min(l.text_x + 360.0), l.y_top + l.height - 2.0);
                scene.fill(Fill::NonZero, transform, Color::from_rgb8(0xee, 0xee, 0xf2), None, &vello::kurbo::RoundedRect::from_rect(box_rect, 4.0));
                text_renderer.draw_text(scene, "⟦ embedded content ⟧", 14.0, MUTED, transform * Affine::translate((l.text_x + 10.0, l.y_top + 6.0)), None);
                continue;
            }

            if let Some(bul) = &l.bullet {
                text_renderer.draw_text(scene, bul, l.size, l.ink, transform * Affine::translate((l.text_x - 24.0, l.y_top)), None);
            }

            if let Some((b0, b1)) = self.selection_bytes_for(&l.id, &l.text) {
                if let (Some(bx), true) = (l.cell_box, multi_sel) {
                    // Multi-cell selection: highlight the whole cell (spreadsheet-style).
                    scene.fill(Fill::NonZero, transform, SELECTION, None, &bx);
                } else {
                    for (rx0, ry0, rx1, ry1) in l.layout.selection_rects(b0, b1) {
                        let r = Rect::new(l.text_x + rx0 as f64, l.y_top + ry0 as f64, l.text_x + rx1 as f64, l.y_top + ry1 as f64);
                        scene.fill(Fill::NonZero, transform, SELECTION, None, &r);
                    }
                }
            }

            // Per-style paragraph border (e.g. a Quote's left rule).
            if let Some(bd) = &l.border {
                let bx0 = l.text_x - bd.pad as f64;
                let bx1 = l.text_x + l.width + bd.pad as f64;
                let by0 = l.y_top - bd.pad as f64 * 0.5;
                let by1 = l.y_top + l.height + bd.pad as f64 * 0.5;
                let col = Color::from_rgb8(bd.color[0], bd.color[1], bd.color[2]);
                let stroke = vello::kurbo::Stroke::new(bd.width as f64);
                let mut seg = |x0: f64, y0: f64, x1: f64, y1: f64| {
                    scene.stroke(&stroke, transform, col, None, &vello::kurbo::Line::new((x0, y0), (x1, y1)));
                };
                if bd.left { seg(bx0, by0, bx0, by1); }
                if bd.right { seg(bx1, by0, bx1, by1); }
                if bd.top { seg(bx0, by0, bx1, by0); }
                if bd.bottom { seg(bx0, by1, bx1, by1); }
            }
            text_renderer.draw_rich(scene, &l.layout, transform * Affine::translate((l.text_x, l.y_top)), l.ink);

            if self.focused && l.id == self.inner.caret.block && !self.inner.has_selection() {
                let byte = char_to_byte(&l.text, self.inner.caret.offset);
                let (cx, cy, ch) = l.layout.caret_geometry(byte);
                let caret = Rect::new(l.text_x + cx as f64, l.y_top + cy as f64, l.text_x + cx as f64 + CARET_WIDTH, l.y_top + cy as f64 + ch as f64);
                scene.fill(Fill::NonZero, transform, CARET_COLOR, None, &caret);
            }
        }

        // Empty-document placeholder.
        if self.blocks.len() == 1 && self.blocks[0].kind == "paragraph" && self.blocks[0].text.is_empty() {
            if let Some(l) = laid.first() {
                text_renderer.draw_text(
                    scene,
                    "Start writing…",
                    16.5,
                    Color::from_rgb8(0xbc, 0xbc, 0xc2),
                    transform * Affine::translate((l.text_x, l.y_top)),
                    None,
                );
            }
        }

        // Layout-edit overlay: draggable guides + live value labels for the
        // active mode (drawn over the content, still inside the page clip).
        if self.layout_mode != LayoutMode::None {
            self.draw_layout_overlay(scene, transform, text_renderer, page);
        }

        scene.pop_layer();

        // Scrollbar thumb (right edge) when the content overflows the canvas.
        if self.max_scroll > 0.0 {
            let track = canvas.height();
            let thumb_h = (track * track / (track + self.max_scroll)).max(28.0);
            let frac = self.scroll_y / self.max_scroll;
            let thumb_y = canvas.y0 + frac * (track - thumb_h);
            let thumb = Rect::new(canvas.x1 - 8.0, thumb_y + 2.0, canvas.x1 - 4.0, thumb_y + thumb_h - 2.0);
            scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x55, 0x58, 0x60), None, &vello::kurbo::RoundedRect::from_rect(thumb, 2.0));
        }

        self.laid = laid;

        let lbar = Rect::new(full.x0, layout_bar_top, full.x1, status_top);
        self.draw_layout_bar(scene, transform, text_renderer, theme, lbar);

        let bar = Rect::new(full.x0, status_top, full.x1, full.y1);
        self.draw_status_bar(scene, transform, text_renderer, theme, bar);
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &Modifiers, state: &mut AutoCommit, _path: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key();
        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                let new = (self.scroll_y - dy).clamp(0.0, self.max_scroll);
                if new != self.scroll_y {
                    self.scroll_y = new;
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
                if let Some(gd) = self.guide_drag {
                    // Live-drag a layout guide (margin / border / spacing).
                    self.apply_guide_drag(gd, self.pointer);
                    self.dirty = true; // repaint every move so the preview is live
                    *handled = true;
                } else if self.drag.drag().is_some() {
                    // Extend the selection: keep the anchor, move the caret.
                    self.caret_from_pointer(self.pointer, true);
                    // Claim the move so the host App repaints (its redraw is gated
                    // behind `handled`).
                    *handled = true;
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
                if self.pointer.y > self.rect.y1 - STATUS_H {
                    return; // status bar — not editable
                }
                // Bottom layout bar (mode chips), just above the status bar.
                let lbar_top = self.rect.y1 - STATUS_H - LAYOUT_BAR_H;
                if self.pointer.y >= lbar_top {
                    if let Some(mode) = self.layout_chips.iter().find(|(_, r)| r.contains(self.pointer)).map(|(m, _)| *m) {
                        self.toggle_layout_mode(mode);
                    }
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // In a layout-edit mode, grabbing a guide starts a drag instead of
                // a text selection.
                if self.layout_mode != LayoutMode::None {
                    if let Some(side) = self.hit_guide(self.pointer) {
                        let kind = if ctrl {
                            DragKind::Proportional
                        } else if shift {
                            DragKind::Single
                        } else {
                            DragKind::Pair
                        };
                        self.guide_drag = Some(GuideDrag { mode: self.layout_mode, side, kind });
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                }
                // Selection gesture (ui::DragSelect): shift extends; double/triple
                // click select word/paragraph; a plain press starts a fresh drag.
                let intent = self.drag.press(self.pointer, shift);
                self.caret_from_pointer(self.pointer, intent.extend);
                match intent.granularity {
                    Granularity::Word => self.inner.select_word(state),
                    Granularity::Line => self.select_all_in_block(state),
                    Granularity::Char => {}
                }
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if let Some(gd) = self.guide_drag.take() {
                    // A style edit (spacing) persists into the document on release.
                    if gd.mode == LayoutMode::Spacing {
                        let sheet = self.sheet.clone();
                        self.inner.set_stylesheet(state, &sheet);
                        self.needs_commit = true;
                    }
                    self.dirty = true;
                    *handled = true;
                } else if self.drag.release() {
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { state: ElementState::Pressed, logical_key, text, .. }, .. } => {
                if !self.focused {
                    return;
                }
                if ctrl {
                    if let Key::Character(s) = logical_key {
                        match s.to_lowercase().as_str() {
                            "s" => self.save(state),
                            "b" => self.inner.toggle_mark(state, "strong"),
                            "i" => self.inner.toggle_mark(state, "em"),
                            "u" => self.inner.toggle_mark(state, "underline"),
                            "c" => self.clipboard_copy(state),
                            "x" => {
                                self.clipboard_copy(state);
                                self.inner.delete_selection(state);
                            }
                            // Ctrl+V keeps formatting; Ctrl+Shift+V pastes plain text.
                            "v" if shift => self.clipboard_paste_plain(state),
                            "v" => self.clipboard_paste(state),
                            "a" => self.inner.select_all(state),
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
                    Key::Named(NamedKey::Space) => {
                        if !self.inner.autoformat_on_space(state) {
                            self.inner.insert_str(state, " ");
                        }
                    }
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
            ToolAction::Strike => self.inner.toggle_mark(state, "strike"),
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
            ToolAction::TableRowAdd => self.inner.table_add_row(state),
            ToolAction::TableRowDel => self.inner.table_del_row(state),
            ToolAction::TableColAdd => self.inner.table_add_col(state),
            ToolAction::TableColDel => self.inner.table_del_col(state),
            ToolAction::TableDelete => self.inner.table_delete(state),
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
            Some((b, pos)) => char_font_size(pos, &b.marks).unwrap_or_else(|| self.sheet.get(&block_style_key(b)).size),
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

    fn clipboard_copy(&mut self, state: &AutoCommit) {
        if !self.inner.has_selection() {
            return;
        }
        let (plain, blocks) = self.inner.copy_fragment(state);
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(plain.clone());
        }
        self.rich_clip = Some((plain, blocks));
    }

    /// Paste with formatting when the system clipboard still holds our last rich
    /// copy; otherwise paste the clipboard's plain text (e.g. from another app).
    fn clipboard_paste(&mut self, state: &mut AutoCommit) {
        let Some(sys) = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()) else {
            return;
        };
        let rich = self.rich_clip.as_ref().filter(|(plain, _)| plain == &sys).map(|(_, blocks)| blocks.clone());
        match rich {
            Some(blocks) => self.inner.insert_rich(state, &blocks),
            None => self.inner.insert_multiline(state, &sys),
        }
    }

    /// Paste plain text only (Ctrl+Shift+V), stripping all formatting.
    fn clipboard_paste_plain(&mut self, state: &mut AutoCommit) {
        if let Some(t) = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()) {
            self.inner.insert_multiline(state, &t);
        }
    }

    fn select_all_in_block(&mut self, state: &AutoCommit) {
        self.inner.move_home();
        self.inner.clear_anchor();
        self.inner.set_anchor();
        self.inner.move_end(state);
    }

    fn draw_status_bar(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, bar: Rect) {
        let words: usize = self.blocks.iter().map(|b| b.text.split_whitespace().count()).sum();
        let chars: usize = self.blocks.iter().map(|b| b.text.chars().count()).sum();
        let left = format!("{words} words    {chars} characters");
        let fname = self.path.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("Untitled");
        let kind = match self.format {
            Some(WpFormat::Odt) => "ODF Text",
            Some(WpFormat::Markdown) => "Markdown",
            None => "",
        };
        let right = if kind.is_empty() { fname.to_string() } else { format!("{fname}    ·    {kind}") };
        chrome::status_bar::draw(scene, transform, tr, theme, bar, &left, &right);
    }

    fn draw_toolbar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, tb: Rect) {
        let bold_a = self.mark_active_in_selection("strong");
        let italic_a = self.mark_active_in_selection("em");
        let under_a = self.mark_active_in_selection("underline");
        let strike_a = self.mark_active_in_selection("strike");
        let (kind, level) = self.current_block_style();
        let align = self.current_align();
        let font_label = self.current_font().label();

        // Build the toolbar spec; the ui::chrome toolbar component owns the look
        // and hands back each button's clickable rect (zipped to its action).
        let cue = |a: ToolAction| match a {
            ToolAction::Bold => LabelCue::Bold,
            ToolAction::Underline => LabelCue::Underline,
            ToolAction::Strike => LabelCue::Strike,
            _ => LabelCue::None,
        };
        let group_start = |a: ToolAction| {
            matches!(a, ToolAction::H1 | ToolAction::AlignLeft | ToolAction::FontCycle | ToolAction::InsertTable | ToolAction::TableRowAdd)
        };
        let mut specs: Vec<(ToolAction, ToolbarItem)> = Vec::new();
        let mut add = |a: ToolAction, label: &str, w: f64, active: bool| {
            specs.push((a, ToolbarItem::new(label, w).active(active).cue(cue(a)).group_start(group_start(a))));
        };
        add(ToolAction::Bold, "B", 34.0, bold_a);
        add(ToolAction::Italic, "I", 30.0, italic_a);
        add(ToolAction::Underline, "U", 32.0, under_a);
        add(ToolAction::Strike, "S", 30.0, strike_a);
        add(ToolAction::H1, "H1", 42.0, kind == "heading" && level == Some(1));
        add(ToolAction::H2, "H2", 42.0, kind == "heading" && level == Some(2));
        add(ToolAction::Body, "Body", 58.0, kind == "paragraph");
        add(ToolAction::Bullet, "• List", 66.0, kind == "list_item");
        add(ToolAction::AlignLeft, "L", 30.0, align == "left");
        add(ToolAction::AlignCenter, "C", 30.0, align == "center");
        add(ToolAction::AlignRight, "R", 30.0, align == "right");
        add(ToolAction::FontCycle, font_label, 58.0, false);
        add(ToolAction::SizeDown, "A−", 34.0, false);
        add(ToolAction::SizeUp, "A+", 34.0, false);
        // Contextual table section: editing controls inside a cell, else insert.
        if kind == "cell" {
            add(ToolAction::TableRowAdd, "+Row", 54.0, false);
            add(ToolAction::TableRowDel, "−Row", 54.0, false);
            add(ToolAction::TableColAdd, "+Col", 52.0, false);
            add(ToolAction::TableColDel, "−Col", 52.0, false);
            add(ToolAction::TableDelete, "✕", 30.0, false);
        } else {
            add(ToolAction::InsertTable, "⊞ Table", 78.0, false);
        }
        drop(add);

        let items: Vec<ToolbarItem> = specs.iter().map(|(_, item)| item.clone()).collect();
        let rects = chrome::toolbar::draw(scene, transform, tr, theme, tb, &items);
        self.toolbar = specs.into_iter().map(|(a, _)| a).zip(rects).collect();
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

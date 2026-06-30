//! [`IconGrid`] — a responsive, selectable grid of icon + label cells.
//!
//! The toolkit had no wrap-flow grid: the file-explorer's "icon view" was
//! hand-built as a `Column` of `Row`s with a hard-coded six-cells-per-row
//! and no selection or keyboard support. `IconGrid` replaces that with a
//! single measured leaf widget that:
//!
//! * **reflows** to the available width — the column count is derived from
//!   the cell size at layout time, so the same grid adapts from a narrow
//!   sidebar to a wide window;
//! * **selects** via the shared [`SelectionModel`](crate::selection::SelectionModel)
//!   (click / Ctrl-click / Shift-click, plus arrow-key navigation that
//!   understands the 2-D layout);
//! * **activates** items on double-click or Enter via `on_activate`;
//! * renders each item's [`Icon`](crate::icons::Icon) as a crisp vector
//!   glyph (not a font codepoint) with a truncated label beneath it.
//!
//! Like the file-explorer's other views it is designed to be rebuilt each
//! frame by a stateless host: seed the selection with
//! [`set_selection`](IconGrid::set_selection) and react to
//! `on_item_click(index, ctrl, shift)`.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::{draw_icon, Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::selection::SelectionModel;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// One cell: a vector glyph and a label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconGridItem {
    pub icon: Icon,
    pub label: String,
}

impl IconGridItem {
    pub fn new(icon: Icon, label: impl Into<String>) -> Self {
        Self { icon, label: label.into() }
    }
}

/// Responsive icon grid widget.
pub struct IconGrid {
    items: Vec<IconGridItem>,
    /// Per-cell box (icon + label) in logical px.
    cell_w: f64,
    cell_h: f64,
    /// Gap between cells (both axes).
    gap: f64,
    /// Icon glyph size within a cell.
    icon_px: f64,

    rect: Rect,
    /// Column count resolved at the last layout, cached for hit-testing +
    /// keyboard navigation. Always `>= 1`.
    columns: usize,

    selection: SelectionModel,
    is_focused: bool,
    hover: Option<usize>,
    last_cursor: Point,
    last_click: Option<(usize, std::time::Instant)>,

    on_item_click: Option<Box<dyn FnMut(usize, bool, bool)>>,
    on_activate: Option<Box<dyn FnMut(usize)>>,
}

impl IconGrid {
    /// A grid with the default cell metrics (96×88 px cells, 12 px gap,
    /// 40 px glyphs) — tuned for a file-manager icon view.
    pub fn new(items: Vec<IconGridItem>) -> Self {
        Self {
            items,
            cell_w: 96.0,
            cell_h: 88.0,
            gap: 12.0,
            icon_px: 40.0,
            rect: Rect::ZERO,
            columns: 1,
            selection: SelectionModel::new(),
            is_focused: false,
            hover: None,
            last_cursor: Point::ZERO,
            last_click: None,
            on_item_click: None,
            on_activate: None,
        }
    }

    /// Override the per-cell metrics.
    pub fn with_cell_size(mut self, w: f64, h: f64) -> Self {
        self.cell_w = w.max(8.0);
        self.cell_h = h.max(8.0);
        self
    }

    /// Override the glyph size.
    pub fn with_icon_size(mut self, px: f64) -> Self {
        self.icon_px = px.max(4.0);
        self
    }

    /// Seed the selection (host owns it across frames).
    pub fn set_selection(&mut self, sel: SelectionModel) {
        self.selection = sel;
    }

    /// Builder form of [`set_selection`](IconGrid::set_selection).
    pub fn with_selection(mut self, sel: SelectionModel) -> Self {
        self.selection = sel;
        self
    }

    /// Borrow the selection.
    pub fn selection(&self) -> &SelectionModel {
        &self.selection
    }

    /// Number of columns resolved at the last layout (always `>= 1`).
    pub fn column_count(&self) -> usize {
        self.columns
    }

    /// Per-click handler `(index, ctrl, shift)`.
    pub fn on_item_click<F: FnMut(usize, bool, bool) + 'static>(mut self, f: F) -> Self {
        self.on_item_click = Some(Box::new(f));
        self
    }

    /// Activation handler (double-click / Enter), given the item index.
    pub fn on_activate<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_activate = Some(Box::new(f));
        self
    }

    /// How many columns fit in `width` given the cell metrics. Always
    /// at least one so a too-narrow grid still shows a single column.
    pub fn columns_for_width(&self, width: f64) -> usize {
        let stride = self.cell_w + self.gap;
        if stride <= 0.0 {
            return 1;
        }
        // width + gap, because the last cell needs no trailing gap.
        (((width + self.gap) / stride).floor() as usize).max(1)
    }

    /// Total content height for `columns` columns of the current items.
    fn content_height(&self, columns: usize) -> f64 {
        let rows = if self.items.is_empty() {
            0
        } else {
            self.items.len().div_ceil(columns.max(1))
        };
        if rows == 0 {
            0.0
        } else {
            rows as f64 * self.cell_h + (rows.saturating_sub(1)) as f64 * self.gap
        }
    }

    /// Top-left origin of cell `i` within `self.rect`.
    fn cell_origin(&self, i: usize) -> Point {
        let col = i % self.columns;
        let row = i / self.columns;
        Point::new(
            self.rect.x0 + col as f64 * (self.cell_w + self.gap),
            self.rect.y0 + row as f64 * (self.cell_h + self.gap),
        )
    }

    fn cell_rect(&self, i: usize) -> Rect {
        let o = self.cell_origin(i);
        Rect::new(o.x, o.y, o.x + self.cell_w, o.y + self.cell_h)
    }

    /// Item index under a logical point, if any.
    pub fn item_at(&self, pt: Point) -> Option<usize> {
        if self.columns == 0 {
            return None;
        }
        let stride_x = self.cell_w + self.gap;
        let stride_y = self.cell_h + self.gap;
        let rel_x = pt.x - self.rect.x0;
        let rel_y = pt.y - self.rect.y0;
        if rel_x < 0.0 || rel_y < 0.0 {
            return None;
        }
        let col = (rel_x / stride_x) as usize;
        let row = (rel_y / stride_y) as usize;
        if col >= self.columns {
            return None;
        }
        // Reject points that land in the inter-cell gap.
        if rel_x - col as f64 * stride_x > self.cell_w {
            return None;
        }
        if rel_y - row as f64 * stride_y > self.cell_h {
            return None;
        }
        let idx = row * self.columns + col;
        if idx < self.items.len() { Some(idx) } else { None }
    }
}

impl Widget for IconGrid {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(Style::default()).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // Pick a width: known wins, else the definite available width,
        // else fall back to enough for one cell.
        let width = known.width.map(|w| w as f64).unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w as f64,
            _ => self.cell_w,
        });
        let columns = self.columns_for_width(width);
        self.columns = columns;
        let height = self.content_height(columns);
        taffy::Size {
            width: width as f32,
            height: height as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        // Resolve columns from the actual allocated width (measure may have
        // run against a different available width).
        self.columns = self.columns_for_width(self.rect.width());
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let label_size = tokens.typography.scale.sm;
        for i in 0..self.items.len() {
            let cell = self.cell_rect(i);
            if presence.should_cull(cell) {
                continue;
            }
            let selected = self.selection.is_selected(i);
            let hovered = self.hover == Some(i) && !selected;
            if selected || hovered {
                let bg = if selected {
                    tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX]
                } else {
                    theme.bg_hover
                };
                let shape = RoundedRect::from_rect(cell, tokens.radii.md as f64);
                scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
            }

            // Icon: a centred square in the upper portion of the cell.
            let icon_x = cell.x0 + (self.cell_w - self.icon_px) / 2.0;
            let icon_y = cell.y0 + 10.0;
            let icon_rect = Rect::new(icon_x, icon_y, icon_x + self.icon_px, icon_y + self.icon_px);
            let glyph_color = if selected {
                vello::peniko::Color::WHITE
            } else {
                tokens.colors.text_primary
            };
            draw_icon(scene, transform, icon_rect, self.items[i].icon, glyph_color, LUCIDE_DEFAULT_STROKE_PX);

            // Label: centred under the icon, truncated to the cell width.
            let label = truncate_to_width(text_renderer, &self.items[i].label, label_size, self.cell_w - 8.0);
            let (tw, _) = text_renderer.measure(&label, label_size, None);
            let tx = cell.x0 + (self.cell_w - tw as f64) / 2.0;
            let ty = icon_y + self.icon_px + 8.0;
            let label_color = if selected {
                vello::peniko::Color::WHITE
            } else {
                tokens.colors.text_primary
            };
            text_renderer.draw_text(scene, &label, label_size, label_color, transform * Affine::translate((tx, ty)), None);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                let new_hover = self.item_at(pt);
                if new_hover != self.hover {
                    self.hover = new_hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if let Some(i) = self.item_at(self.last_cursor) {
                    let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                    let shift = modifiers.state().shift_key();
                    self.selection.apply_click(i, ctrl, shift);
                    *focus_target = Some(node_id);
                    if let Some(cb) = &mut self.on_item_click {
                        cb(i, ctrl, shift);
                    }
                    let now = std::time::Instant::now();
                    let is_double = matches!(
                        self.last_click,
                        Some((pi, t)) if pi == i && now.duration_since(t) < std::time::Duration::from_millis(500)
                    );
                    if is_double {
                        self.last_click = None;
                        if let Some(cb) = &mut self.on_activate {
                            cb(i);
                        }
                    } else {
                        self.last_click = Some((i, now));
                    }
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if self.is_focused && ke.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                let shift = modifiers.state().shift_key();
                let len = self.items.len();
                if len == 0 {
                    return;
                }
                let cols = self.columns.max(1);
                let cur = self.selection.primary();
                let target = match &ke.logical_key {
                    Key::Named(NamedKey::ArrowRight) => Some(cur.map(|c| (c + 1).min(len - 1)).unwrap_or(0)),
                    Key::Named(NamedKey::ArrowLeft) => Some(cur.map(|c| c.saturating_sub(1)).unwrap_or(0)),
                    Key::Named(NamedKey::ArrowDown) => Some(cur.map(|c| (c + cols).min(len - 1)).unwrap_or(0)),
                    Key::Named(NamedKey::ArrowUp) => Some(cur.map(|c| c.saturating_sub(cols)).unwrap_or(0)),
                    Key::Named(NamedKey::Enter) => {
                        if let (Some(c), Some(cb)) = (cur, self.on_activate.as_mut()) {
                            cb(c);
                        }
                        *handled = true;
                        None
                    }
                    Key::Character(s) if (s == "a" || s == "A") && modifiers.state().control_key() => {
                        self.selection.select_all(len);
                        *handled = true;
                        None
                    }
                    _ => None,
                };
                if let Some(t) = target {
                    self.selection.move_to(t, shift);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "IconGrid"
    }

    fn test_id(&self) -> Option<&str> {
        Some("icon_grid")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, f: bool) {
        self.is_focused = f;
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }
}

/// Trim `text` (adding an ellipsis) until it measures within `max_w`.
fn truncate_to_width(tr: &mut TextRenderer, text: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let (w, _) = tr.measure(text, size, None);
    if (w as f64) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let candidate: String = chars[..keep].iter().collect::<String>() + "…";
        let (cw, _) = tr.measure(&candidate, size, None);
        if (cw as f64) <= max_w {
            return candidate;
        }
    }
    "…".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<IconGridItem> {
        (0..n).map(|i| IconGridItem::new(Icon::File, format!("f{i}"))).collect()
    }

    #[test]
    fn columns_scale_with_width() {
        let g = IconGrid::new(items(10)); // 96px cell + 12px gap = 108 stride
        // (width + gap) / stride, floored.
        assert_eq!(g.columns_for_width(108.0), 1);
        assert_eq!(g.columns_for_width(216.0), 2);
        assert_eq!(g.columns_for_width(700.0), 6);
        // Never zero, even for an absurdly narrow box.
        assert_eq!(g.columns_for_width(10.0), 1);
    }

    #[test]
    fn content_height_rounds_up_rows() {
        let g = IconGrid::new(items(7));
        // 3 columns → 3 rows (3,3,1). height = 3*88 + 2*12 = 288.
        assert!((g.content_height(3) - 288.0).abs() < 0.01);
        // Empty grid is zero-height.
        assert_eq!(IconGrid::new(items(0)).content_height(4), 0.0);
    }

    #[test]
    fn item_at_maps_points_and_rejects_gaps() {
        let mut g = IconGrid::new(items(6));
        g.rect = Rect::new(0.0, 0.0, 324.0, 200.0); // 3 columns (324 = 3*108)
        g.columns = 3;
        // Cell 0 centre (~48,44).
        assert_eq!(g.item_at(Point::new(48.0, 44.0)), Some(0));
        // Cell 4 = row 1, col 1 → origin x=108,y=100; centre ~156,144.
        assert_eq!(g.item_at(Point::new(156.0, 144.0)), Some(4));
        // A point in the horizontal gap between col 0 and col 1 (x≈100).
        assert_eq!(g.item_at(Point::new(100.0, 44.0)), None);
        // Row 2 is empty (6 items fill rows 0–1), so it maps to nothing.
        assert_eq!(g.item_at(Point::new(48.0, 244.0)), None);
    }

    #[test]
    fn click_selects_and_reports() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let log = Rc::new(RefCell::new(Vec::<(usize, bool, bool)>::new()));
        let mut g = {
            let log = log.clone();
            IconGrid::new(items(6)).on_item_click(move |i, c, s| log.borrow_mut().push((i, c, s)))
        };
        g.rect = Rect::new(0.0, 0.0, 324.0, 200.0);
        g.columns = 3;

        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let device: winit::event::DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut feed = |g: &mut IconGrid, ev: WindowEvent| {
            let mut h = false;
            let mut f = None;
            g.handle_event(&ev, 1.0, &mods, &mut doc, &root, &mut h, &mut f, node, &tree);
        };
        feed(&mut g, WindowEvent::CursorMoved { device_id: device, position: (156.0, 144.0).into() });
        feed(&mut g, WindowEvent::MouseInput { device_id: device, state: ElementState::Pressed, button: MouseButton::Left });
        assert!(g.selection().is_selected(4));
        assert_eq!(*log.borrow(), vec![(4, false, false)]);
    }
}

//! The spreadsheet grid — a hand-rolled view that paints A1 headers, a
//! scrollable cell matrix, selection (single + range), an in-cell editor,
//! and column/row resize handles directly into the Vello scene.
//!
//! This is intentionally NOT a `ui::Widget`: a spreadsheet needs custom
//! virtualization, per-cell clipping, and pixel-level header chrome that
//! the flexbox-driven widget primitives don't model well. The owning
//! `SheetsApp` widget drives it: it hands the grid a `Rect` + the current
//! cell data each frame, forwards pointer/key events, and applies the
//! [`GridCmd`]s the grid emits back into the CRDT.

use std::collections::HashMap;

use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::ColorCompat;

use crate::model::{addr_to_a1, format_value, Addr, CellFormat, CellValue};

pub const ROW_HEADER_W: f64 = 56.0;
pub const COL_HEADER_H: f64 = 28.0;
pub const DEFAULT_COL_W: f64 = 108.0;
pub const DEFAULT_ROW_H: f64 = 26.0;
const MIN_COL_W: f64 = 40.0;
const MIN_ROW_H: f64 = 18.0;
const RESIZE_GRAB: f64 = 4.0;
const CELL_PAD_X: f64 = 6.0;

/// A mutation the grid wants applied to the document by the app.
#[derive(Clone, Debug)]
pub enum GridCmd {
    /// Write a cell's raw input (empty string clears it).
    SetCell { addr: Addr, raw: String },
    /// Mark a logical undo checkpoint after a burst of edits.
    Commit,
}

#[derive(Clone)]
struct EditState {
    addr: Addr,
    chars: Vec<char>,
    caret: usize,
}

impl EditState {
    fn text(&self) -> String {
        self.chars.iter().collect()
    }
}

#[derive(Clone, Copy)]
enum ResizeTarget {
    Col(u32),
    Row(u32),
}

struct ResizeDrag {
    target: ResizeTarget,
    origin_px: f64,
    origin_size: f64,
}

pub struct Grid {
    pub rect: Rect,
    scroll_x: f64,
    scroll_y: f64,
    col_widths: HashMap<u32, f64>,
    row_heights: HashMap<u32, f64>,
    n_cols: u32,
    n_rows: u32,
    /// Selection anchor + active cell. A single-cell selection has them equal.
    anchor: Addr,
    active: Addr,
    edit: Option<EditState>,
    values: HashMap<Addr, CellValue>,
    raw: HashMap<Addr, String>,
    fmt: HashMap<Addr, CellFormat>,
    last_cursor: Point,
    resize: Option<ResizeDrag>,
    selecting: bool,
    dirty: bool,
}

impl Grid {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            scroll_x: 0.0,
            scroll_y: 0.0,
            col_widths: HashMap::new(),
            row_heights: HashMap::new(),
            n_cols: 48,
            n_rows: 200,
            anchor: (0, 0),
            active: (0, 0),
            edit: None,
            values: HashMap::new(),
            raw: HashMap::new(),
            fmt: HashMap::new(),
            last_cursor: Point::ZERO,
            resize: None,
            selecting: false,
            dirty: true,
        }
    }

    // --- data plumbing ------------------------------------------------------

    /// Replace the rendered cell data (called every frame from the app's
    /// `sync` after recalculation).
    pub fn set_data(
        &mut self,
        raw: HashMap<Addr, String>,
        fmt: HashMap<Addr, CellFormat>,
        values: HashMap<Addr, CellValue>,
    ) {
        // Grow the logical extent so headers cover all populated cells.
        for &(c, r) in raw.keys() {
            self.n_cols = self.n_cols.max(c + 4);
            self.n_rows = self.n_rows.max(r + 8);
        }
        self.raw = raw;
        self.fmt = fmt;
        self.values = values;
        self.dirty = true;
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }
    pub fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn active(&self) -> Addr {
        self.active
    }

    /// Collapse the selection onto a single cell (used on sheet switch and
    /// for the seeded screenshot selection).
    pub fn select_only(&mut self, addr: Addr) {
        self.n_cols = self.n_cols.max(addr.0 + 4);
        self.n_rows = self.n_rows.max(addr.1 + 8);
        self.anchor = addr;
        self.active = addr;
        self.edit = None;
        self.ensure_visible();
        self.dirty = true;
    }

    /// Public hit-test wrapper so the app can detect double-clicks.
    pub fn cell_at_public(&self, p: Point) -> Option<Addr> {
        self.cell_at(p)
    }

    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    /// What the formula bar should show: the live edit buffer if editing,
    /// otherwise the active cell's stored raw input.
    pub fn active_display_raw(&self) -> String {
        if let Some(e) = &self.edit {
            if e.addr == self.active {
                return e.text();
            }
        }
        self.raw.get(&self.active).cloned().unwrap_or_default()
    }

    /// Every cell in the current selection rectangle.
    pub fn selected_cells(&self) -> Vec<Addr> {
        let (c0, c1) = (self.anchor.0.min(self.active.0), self.anchor.0.max(self.active.0));
        let (r0, r1) = (self.anchor.1.min(self.active.1), self.anchor.1.max(self.active.1));
        let mut out = Vec::new();
        for r in r0..=r1 {
            for c in c0..=c1 {
                out.push((c, r));
            }
        }
        out
    }

    /// A1-style label for the selection (`B2` or `B2:D5`).
    pub fn selection_label(&self) -> String {
        if self.anchor == self.active {
            addr_to_a1(self.active.0, self.active.1)
        } else {
            let (c0, c1) = (self.anchor.0.min(self.active.0), self.anchor.0.max(self.active.0));
            let (r0, r1) = (self.anchor.1.min(self.active.1), self.anchor.1.max(self.active.1));
            format!("{}:{}", addr_to_a1(c0, r0), addr_to_a1(c1, r1))
        }
    }

    // --- geometry helpers ---------------------------------------------------

    fn col_w(&self, c: u32) -> f64 {
        self.col_widths.get(&c).copied().unwrap_or(DEFAULT_COL_W)
    }
    fn row_h(&self, r: u32) -> f64 {
        self.row_heights.get(&r).copied().unwrap_or(DEFAULT_ROW_H)
    }

    /// Content-space x (before scroll) of column `c`'s left edge.
    fn col_left(&self, c: u32) -> f64 {
        let mut x = 0.0;
        for i in 0..c {
            x += self.col_w(i);
        }
        x
    }
    fn row_top(&self, r: u32) -> f64 {
        let mut y = 0.0;
        for i in 0..r {
            y += self.row_h(i);
        }
        y
    }

    fn body_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + ROW_HEADER_W,
            self.rect.y0 + COL_HEADER_H,
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn grid_x0(&self) -> f64 {
        self.rect.x0 + ROW_HEADER_W
    }
    fn grid_y0(&self) -> f64 {
        self.rect.y0 + COL_HEADER_H
    }

    /// Screen rect for a cell, in absolute coords (may lie outside the body).
    fn cell_screen_rect(&self, c: u32, r: u32) -> Rect {
        let x = self.grid_x0() + self.col_left(c) - self.scroll_x;
        let y = self.grid_y0() + self.row_top(r) - self.scroll_y;
        Rect::new(x, y, x + self.col_w(c), y + self.row_h(r))
    }

    /// Visible column indices given the current scroll + body width.
    fn visible_cols(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut x = self.grid_x0() - self.scroll_x;
        for c in 0..self.n_cols {
            let w = self.col_w(c);
            if x + w > self.grid_x0() - 0.5 && x < self.rect.x1 {
                out.push(c);
            }
            x += w;
            if x >= self.rect.x1 {
                break;
            }
        }
        out
    }
    fn visible_rows(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut y = self.grid_y0() - self.scroll_y;
        for r in 0..self.n_rows {
            let h = self.row_h(r);
            if y + h > self.grid_y0() - 0.5 && y < self.rect.y1 {
                out.push(r);
            }
            y += h;
            if y >= self.rect.y1 {
                break;
            }
        }
        out
    }

    /// Map a screen point to a cell, or `None` if it's over a header/corner.
    fn cell_at(&self, p: Point) -> Option<Addr> {
        let body = self.body_rect();
        if !body.contains(p) {
            return None;
        }
        let cx = p.x - self.grid_x0() + self.scroll_x;
        let cy = p.y - self.grid_y0() + self.scroll_y;
        let mut x = 0.0;
        let mut col = None;
        for c in 0..self.n_cols {
            let w = self.col_w(c);
            if cx >= x && cx < x + w {
                col = Some(c);
                break;
            }
            x += w;
        }
        let mut y = 0.0;
        let mut row = None;
        for r in 0..self.n_rows {
            let h = self.row_h(r);
            if cy >= y && cy < y + h {
                row = Some(r);
                break;
            }
            y += h;
        }
        Some((col?, row?))
    }

    /// If `p` is near a column/row right/bottom edge in the header strip,
    /// return the resize target.
    fn resize_target_at(&self, p: Point) -> Option<ResizeTarget> {
        // Column edges live in the column-header strip.
        if p.y >= self.rect.y0 && p.y < self.grid_y0() && p.x >= self.grid_x0() {
            let mut x = self.grid_x0() - self.scroll_x;
            for c in 0..self.n_cols {
                x += self.col_w(c);
                if (p.x - x).abs() <= RESIZE_GRAB {
                    return Some(ResizeTarget::Col(c));
                }
                if x > self.rect.x1 {
                    break;
                }
            }
        }
        // Row edges live in the row-header strip.
        if p.x >= self.rect.x0 && p.x < self.grid_x0() && p.y >= self.grid_y0() {
            let mut y = self.grid_y0() - self.scroll_y;
            for r in 0..self.n_rows {
                y += self.row_h(r);
                if (p.y - y).abs() <= RESIZE_GRAB {
                    return Some(ResizeTarget::Row(r));
                }
                if y > self.rect.y1 {
                    break;
                }
            }
        }
        None
    }

    fn total_width(&self) -> f64 {
        (0..self.n_cols).map(|c| self.col_w(c)).sum()
    }
    fn total_height(&self) -> f64 {
        (0..self.n_rows).map(|r| self.row_h(r)).sum()
    }

    /// True once the grid has a usable layout rect (post `apply_layout`).
    fn has_layout(&self) -> bool {
        self.rect.width() > ROW_HEADER_W && self.rect.height() > COL_HEADER_H
    }

    fn clamp_scroll(&mut self) {
        if !self.has_layout() {
            return;
        }
        let body = self.body_rect();
        let max_x = (self.total_width() - body.width()).max(0.0);
        let max_y = (self.total_height() - body.height()).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
    }

    /// Scroll so the active cell is fully inside the body.
    fn ensure_visible(&mut self) {
        if !self.has_layout() {
            return;
        }
        let body = self.body_rect();
        let left = self.col_left(self.active.0);
        let right = left + self.col_w(self.active.0);
        let top = self.row_top(self.active.1);
        let bottom = top + self.row_h(self.active.1);
        if left - self.scroll_x < 0.0 {
            self.scroll_x = left;
        } else if right - self.scroll_x > body.width() {
            self.scroll_x = right - body.width();
        }
        if top - self.scroll_y < 0.0 {
            self.scroll_y = top;
        } else if bottom - self.scroll_y > body.height() {
            self.scroll_y = bottom - body.height();
        }
        self.clamp_scroll();
    }

    // --- editing ------------------------------------------------------------

    /// Begin editing the active cell. `seed` replaces the content (typed
    /// character) or, when `None`, loads the existing raw for in-place edit.
    pub fn begin_edit(&mut self, seed: Option<String>) {
        let chars: Vec<char> = match seed {
            Some(s) => s.chars().collect(),
            None => self.raw.get(&self.active).cloned().unwrap_or_default().chars().collect(),
        };
        let caret = chars.len();
        self.edit = Some(EditState { addr: self.active, chars, caret });
        self.dirty = true;
    }

    fn commit_edit(&mut self, out: &mut Vec<GridCmd>) {
        if let Some(e) = self.edit.take() {
            out.push(GridCmd::SetCell { addr: e.addr, raw: e.text() });
            out.push(GridCmd::Commit);
            self.dirty = true;
        }
    }

    fn cancel_edit(&mut self) {
        self.edit = None;
        self.dirty = true;
    }

    fn move_active(&mut self, dc: i32, dr: i32, extend: bool) {
        let nc = (self.active.0 as i32 + dc).clamp(0, self.n_cols as i32 - 1) as u32;
        let nr = (self.active.1 as i32 + dr).clamp(0, self.n_rows as i32 - 1) as u32;
        self.active = (nc, nr);
        if !extend {
            self.anchor = self.active;
        }
        self.ensure_visible();
        self.dirty = true;
    }

    // --- input handlers (called by the app) --------------------------------

    pub fn on_scroll(&mut self, dx: f64, dy: f64) {
        self.scroll_x += dx;
        self.scroll_y += dy;
        self.clamp_scroll();
        self.dirty = true;
    }

    /// Returns true if the press was consumed by the grid.
    pub fn pointer_down(&mut self, p: Point, shift: bool, out: &mut Vec<GridCmd>) -> bool {
        self.last_cursor = p;
        if !self.rect.contains(p) {
            return false;
        }
        // Start a resize drag if near a header edge.
        if let Some(target) = self.resize_target_at(p) {
            let origin_size = match target {
                ResizeTarget::Col(c) => self.col_w(c),
                ResizeTarget::Row(r) => self.row_h(r),
            };
            let origin_px = match target {
                ResizeTarget::Col(_) => p.x,
                ResizeTarget::Row(_) => p.y,
            };
            self.resize = Some(ResizeDrag { target, origin_px, origin_size });
            return true;
        }
        // Column header click → select the whole column.
        if p.y >= self.rect.y0 && p.y < self.grid_y0() && p.x >= self.grid_x0() {
            if let Some((c, _)) = self.cell_at(Point::new(p.x, self.grid_y0() + 1.0)) {
                self.commit_edit(out);
                self.anchor = (c, self.n_rows.saturating_sub(1));
                self.active = (c, 0);
                self.dirty = true;
                return true;
            }
        }
        // Row header click → select whole row.
        if p.x >= self.rect.x0 && p.x < self.grid_x0() && p.y >= self.grid_y0() {
            if let Some((_, r)) = self.cell_at(Point::new(self.grid_x0() + 1.0, p.y)) {
                self.commit_edit(out);
                self.anchor = (self.n_cols.saturating_sub(1), r);
                self.active = (0, r);
                self.dirty = true;
                return true;
            }
        }
        // Body click → select / extend.
        if let Some(addr) = self.cell_at(p) {
            self.commit_edit(out);
            self.active = addr;
            if !shift {
                self.anchor = addr;
            }
            self.selecting = true;
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn pointer_move(&mut self, p: Point) -> bool {
        self.last_cursor = p;
        if let Some(drag) = &self.resize {
            match drag.target {
                ResizeTarget::Col(c) => {
                    let w = (drag.origin_size + (p.x - drag.origin_px)).max(MIN_COL_W);
                    self.col_widths.insert(c, w);
                }
                ResizeTarget::Row(r) => {
                    let h = (drag.origin_size + (p.y - drag.origin_px)).max(MIN_ROW_H);
                    self.row_heights.insert(r, h);
                }
            }
            self.dirty = true;
            return true;
        }
        if self.selecting {
            if let Some(addr) = self.cell_at(p) {
                if addr != self.active {
                    self.active = addr;
                    self.dirty = true;
                }
            }
            return true;
        }
        false
    }

    pub fn pointer_up(&mut self) {
        self.resize = None;
        self.selecting = false;
    }

    pub fn double_click(&mut self, p: Point, out: &mut Vec<GridCmd>) -> bool {
        let _ = out;
        if let Some(addr) = self.cell_at(p) {
            self.active = addr;
            self.anchor = addr;
            self.begin_edit(None);
            return true;
        }
        false
    }

    /// Handle a keyboard event. Returns true if consumed.
    pub fn key(
        &mut self,
        event: &WindowEvent,
        mods: &Modifiers,
        out: &mut Vec<GridCmd>,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if !ke.state.is_pressed() {
            return false;
        }
        let shift = mods.state().shift_key();
        let ctrl = mods.state().control_key() || mods.state().super_key();

        // Editing mode owns most keys.
        if self.edit.is_some() {
            return self.key_editing(ke, out);
        }

        match &ke.logical_key {
            Key::Named(NamedKey::ArrowUp) => {
                self.move_active(0, -1, shift);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_active(0, 1, shift);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.move_active(-1, 0, shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.move_active(1, 0, shift);
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.move_active(if shift { -1 } else { 1 }, 0, false);
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.move_active(0, if shift { -1 } else { 1 }, false);
                true
            }
            Key::Named(NamedKey::F2) => {
                self.begin_edit(None);
                true
            }
            Key::Named(NamedKey::Home) => {
                self.active.0 = 0;
                if !shift {
                    self.anchor = self.active;
                }
                self.ensure_visible();
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::PageDown) => {
                self.move_active(0, 12, shift);
                true
            }
            Key::Named(NamedKey::PageUp) => {
                self.move_active(0, -12, shift);
                true
            }
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                for addr in self.selected_cells() {
                    out.push(GridCmd::SetCell { addr, raw: String::new() });
                }
                out.push(GridCmd::Commit);
                self.dirty = true;
                true
            }
            Key::Character(s) if !ctrl => {
                // Begin editing, replacing content with the typed char.
                self.begin_edit(Some(s.to_string()));
                true
            }
            _ => false,
        }
    }

    fn key_editing(&mut self, ke: &winit::event::KeyEvent, out: &mut Vec<GridCmd>) -> bool {
        match &ke.logical_key {
            Key::Named(NamedKey::Enter) => {
                self.commit_edit(out);
                self.move_active(0, 1, false);
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.commit_edit(out);
                self.move_active(1, 0, false);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.cancel_edit();
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(e) = &mut self.edit {
                    if e.caret > 0 {
                        e.chars.remove(e.caret - 1);
                        e.caret -= 1;
                    }
                }
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::Delete) => {
                if let Some(e) = &mut self.edit {
                    if e.caret < e.chars.len() {
                        e.chars.remove(e.caret);
                    }
                }
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if let Some(e) = &mut self.edit {
                    e.caret = e.caret.saturating_sub(1);
                }
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                if let Some(e) = &mut self.edit {
                    if e.caret < e.chars.len() {
                        e.caret += 1;
                    }
                }
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::Home) => {
                if let Some(e) = &mut self.edit {
                    e.caret = 0;
                }
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::End) => {
                if let Some(e) = &mut self.edit {
                    e.caret = e.chars.len();
                }
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::Space) => {
                self.insert_text(" ");
                true
            }
            Key::Character(s) => {
                self.insert_text(s);
                true
            }
            _ => {
                // Fall back to the platform-resolved text (handles symbols).
                if let Some(t) = &ke.text {
                    self.insert_text(t);
                    true
                } else {
                    false
                }
            }
        }
    }

    fn insert_text(&mut self, s: &str) {
        if let Some(e) = &mut self.edit {
            for ch in s.chars() {
                e.chars.insert(e.caret, ch);
                e.caret += 1;
            }
            self.dirty = true;
        }
    }

    /// The app calls this when the user types into the formula bar / clicks
    /// "fx" to start an edit seeded with the current cell's content.
    pub fn begin_edit_in_place(&mut self) {
        if self.edit.is_none() {
            self.begin_edit(None);
        }
    }

    /// Commit any pending edit (e.g. before switching sheets).
    pub fn flush_edit(&mut self, out: &mut Vec<GridCmd>) {
        self.commit_edit(out);
    }

    // --- rendering ----------------------------------------------------------

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let body = self.body_rect();
        let cols = self.visible_cols();
        let rows = self.visible_rows();

        // Body background.
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &self.rect);

        // Selection rectangle (translucent accent fill) under the cells.
        let sel = self.selection_screen_rect();
        if let Some(sr) = sel {
            let clipped = intersect(sr, body);
            if let Some(cr) = clipped {
                let fill = Color::from_rgba8(99, 102, 241, 38);
                scene.fill(Fill::NonZero, transform, fill, None, &cr);
            }
        }

        // --- cells + text (clipped to the body) ---
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
        let text_size = theme.tokens.typography.scale.base;
        for &r in &rows {
            for &c in &cols {
                let cell = self.cell_screen_rect(c, r);
                // Skip while editing this exact cell — the editor overlay
                // paints it instead.
                if let Some(e) = &self.edit {
                    if e.addr == (c, r) {
                        continue;
                    }
                }
                if let Some(val) = self.values.get(&(c, r)) {
                    if val.is_empty() {
                        continue;
                    }
                    let fmt = self.fmt.get(&(c, r)).copied().unwrap_or_default();
                    let text = format_value(val, fmt);
                    if text.is_empty() {
                        continue;
                    }
                    let color = if val.is_error() {
                        colors.semantic.danger
                    } else if matches!(val, CellValue::Text(_)) && fmt != CellFormat::Number {
                        colors.text_primary
                    } else {
                        colors.text_primary
                    };
                    let right_align = val.is_numeric() && fmt != CellFormat::Text;
                    self.draw_cell_text(scene, transform, tr, &text, text_size, color, cell, right_align);
                }
            }
        }

        // Grid lines.
        let line_color = colors.border_standard.with_alpha_factor(0.5);
        let stroke = Stroke::new(1.0);
        // verticals
        for &c in &cols {
            let x = (self.grid_x0() + self.col_left(c) - self.scroll_x).round() + 0.5;
            scene.stroke(
                &stroke,
                transform,
                line_color,
                None,
                &Line::new((x, body.y0), (x, body.y1)),
            );
        }
        // last right edge
        if let Some(&lastc) = cols.last() {
            let x = (self.grid_x0() + self.col_left(lastc) + self.col_w(lastc) - self.scroll_x)
                .round()
                + 0.5;
            if x < body.x1 {
                scene.stroke(&stroke, transform, line_color, None, &Line::new((x, body.y0), (x, body.y1)));
            }
        }
        for &r in &rows {
            let y = (self.grid_y0() + self.row_top(r) - self.scroll_y).round() + 0.5;
            scene.stroke(
                &stroke,
                transform,
                line_color,
                None,
                &Line::new((body.x0, y), (body.x1, y)),
            );
        }
        if let Some(&lastr) = rows.last() {
            let y = (self.grid_y0() + self.row_top(lastr) + self.row_h(lastr) - self.scroll_y)
                .round()
                + 0.5;
            if y < body.y1 {
                scene.stroke(&stroke, transform, line_color, None, &Line::new((body.x0, y), (body.x1, y)));
            }
        }

        // Active-cell highlight border.
        if let Some(ar) = intersect(self.cell_screen_rect(self.active.0, self.active.1), body) {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                colors.accent[5],
                None,
                &ar,
            );
        }
        scene.pop_layer();

        // --- headers (painted on top, never clipped by the body) ---
        self.draw_headers(scene, transform, tr, theme, &cols, &rows);

        // --- in-cell editor overlay ---
        if self.edit.is_some() {
            self.draw_editor(scene, transform, tr, theme);
        }
    }

    fn selection_screen_rect(&self) -> Option<Rect> {
        let (c0, c1) = (self.anchor.0.min(self.active.0), self.anchor.0.max(self.active.0));
        let (r0, r1) = (self.anchor.1.min(self.active.1), self.anchor.1.max(self.active.1));
        let tl = self.cell_screen_rect(c0, r0);
        let br = self.cell_screen_rect(c1, r1);
        Some(Rect::new(tl.x0, tl.y0, br.x1, br.y1))
    }

    fn draw_cell_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        text: &str,
        size: f32,
        color: Color,
        cell: Rect,
        right_align: bool,
    ) {
        let inner = Rect::new(cell.x0 + 1.0, cell.y0, cell.x1 - 1.0, cell.y1);
        if inner.width() <= 2.0 {
            return;
        }
        let (tw, _) = tr.measure(text, size, None);
        let ty = cell.y0 + (cell.height() - size as f64) / 2.0;
        let tx = if right_align {
            cell.x1 - CELL_PAD_X - tw as f64
        } else {
            cell.x0 + CELL_PAD_X
        };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &inner);
        tr.draw_text(scene, text, size, color, transform * Affine::translate((tx, ty)), None);
        scene.pop_layer();
    }

    fn draw_headers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cols: &[u32],
        rows: &[u32],
    ) {
        let colors = &theme.tokens.colors;
        let size = theme.tokens.typography.scale.sm;
        let (sc0, sc1) = (self.anchor.0.min(self.active.0), self.anchor.0.max(self.active.0));
        let (sr0, sr1) = (self.anchor.1.min(self.active.1), self.anchor.1.max(self.active.1));

        // Column header strip.
        let col_strip = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.grid_y0());
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &col_strip);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &col_strip);
        for &c in cols {
            let cell = self.cell_screen_rect(c, 0);
            let hx = cell.x0;
            let hrect = Rect::new(hx, self.rect.y0, hx + self.col_w(c), self.grid_y0());
            let selected = c >= sc0 && c <= sc1;
            if selected {
                scene.fill(Fill::NonZero, transform, colors.accent[5].with_alpha_factor(0.30), None, &hrect);
            }
            let label = crate::model::col_to_letters(c);
            let (lw, _) = tr.measure(&label, size, None);
            let tx = hrect.center().x - lw as f64 / 2.0;
            let ty = self.rect.y0 + (COL_HEADER_H - size as f64) / 2.0;
            let color = if selected { colors.text_primary } else { colors.text_dim };
            tr.draw_text(scene, &label, size, color, transform * Affine::translate((tx, ty)), None);
        }
        scene.pop_layer();
        // bottom border of column strip
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &Line::new((self.rect.x0, self.grid_y0()), (self.rect.x1, self.grid_y0())),
        );

        // Row header strip.
        let row_strip = Rect::new(self.rect.x0, self.grid_y0(), self.grid_x0(), self.rect.y1);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &row_strip);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &row_strip);
        for &r in rows {
            let cell = self.cell_screen_rect(0, r);
            let hy = cell.y0;
            let hrect = Rect::new(self.rect.x0, hy, self.grid_x0(), hy + self.row_h(r));
            let selected = r >= sr0 && r <= sr1;
            if selected {
                scene.fill(Fill::NonZero, transform, colors.accent[5].with_alpha_factor(0.30), None, &hrect);
            }
            let label = format!("{}", r + 1);
            let (lw, _) = tr.measure(&label, size, None);
            let tx = hrect.center().x - lw as f64 / 2.0;
            let ty = hy + (self.row_h(r) - size as f64) / 2.0;
            let color = if selected { colors.text_primary } else { colors.text_dim };
            tr.draw_text(scene, &label, size, color, transform * Affine::translate((tx, ty)), None);
        }
        scene.pop_layer();
        // right border of row strip
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &Line::new((self.grid_x0(), self.rect.y0), (self.grid_x0(), self.rect.y1)),
        );

        // Corner box.
        let corner = Rect::new(self.rect.x0, self.rect.y0, self.grid_x0(), self.grid_y0());
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &corner);
    }

    /// Is a cell non-empty in the current data? Used to decide how far the
    /// in-cell editor may overflow before a neighbour blocks it.
    fn cell_is_occupied(&self, addr: Addr) -> bool {
        match self.values.get(&addr) {
            Some(v) => !v.is_empty(),
            None => self.raw.get(&addr).map(|s| !s.is_empty()).unwrap_or(false),
        }
    }

    /// The width the editor wants to display its text comfortably.
    fn editor_desired_width(&self, text: &str, tr: &mut TextRenderer, size: f32) -> f64 {
        let (tw, _) = tr.measure(text, size, None);
        // Text width + padding on both sides + caret room, never narrower
        // than the cell itself.
        (tw as f64 + CELL_PAD_X * 2.0 + 8.0).max(self.col_w(0))
    }

    /// Compute how far the floating editor for cell `addr` may extend to the
    /// right, in absolute screen x. It grows across **empty** neighbours
    /// until it has `desired` width, hits an occupied cell, runs out of
    /// columns, or reaches `body_right`. This is the pure geometry behind
    /// the "overflow into empty cells, clip at a non-empty neighbour" rule.
    fn editor_right_extent(&self, addr: Addr, desired: f64, body_right: f64) -> f64 {
        let (ec, er) = addr;
        let cell = self.cell_screen_rect(ec, er);
        let mut right = cell.x1;
        let mut next_col = ec + 1;
        while right - cell.x0 < desired && next_col < self.n_cols {
            if self.cell_is_occupied((next_col, er)) {
                break;
            }
            right += self.col_w(next_col);
            next_col += 1;
        }
        // Never overrun the visible body, but always show at least the cell.
        right.min(body_right).max(cell.x1.min(body_right))
    }

    /// Like Excel: the active edit is a floating box aligned to the cell
    /// that grows rightward into **empty** neighbours only, stopping at the
    /// first occupied cell or the grid's right edge. It is always clipped to
    /// the grid body so it never paints over the headers.
    fn draw_editor(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(e) = &self.edit else { return };
        let colors = &theme.tokens.colors;
        let (ec, er) = e.addr;
        let cell = self.cell_screen_rect(ec, er);
        let body = self.body_rect();

        // Don't paint an editor that scrolled out from under the body.
        if cell.y1 <= body.y0 || cell.x1 <= body.x0 || cell.x0 >= body.x1 || cell.y0 >= body.y1 {
            return;
        }

        let size = theme.tokens.typography.scale.base;
        let text = e.text();
        let desired = self.editor_desired_width(&text, tr, size);

        // Grow rightward across empty neighbours until we have enough room or
        // we hit an occupied cell / the last column / the grid edge.
        let right = self.editor_right_extent((ec, er), desired, body.x1);
        let edit_rect = Rect::new(cell.x0.max(body.x0), cell.y0, right, cell.y1);

        // Clip everything to the body so the floating editor stays inside the
        // grid even when it overflows past where data ends.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);

        // Solid backing so underlying empty cells/gridlines don't bleed
        // through, then a crisp accent border — the "clean Excel" look.
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &edit_rect);
        scene.stroke(&Stroke::new(2.0), transform, colors.accent[5], None, &edit_rect);

        // Text + caret, clipped to the editor box.
        let ty = edit_rect.y0 + (edit_rect.height() - size as f64) / 2.0;
        let tx = edit_rect.x0 + CELL_PAD_X;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &edit_rect);
        tr.draw_text(
            scene,
            &text,
            size,
            colors.text_primary,
            transform * Affine::translate((tx, ty)),
            None,
        );
        let prefix: String = e.chars[..e.caret.min(e.chars.len())].iter().collect();
        let (pw, _) = tr.measure(&prefix, size, None);
        let caret_x = tx + pw as f64;
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            colors.accent[5],
            None,
            &Line::new((caret_x, edit_rect.y0 + 4.0), (caret_x, edit_rect.y1 - 4.0)),
        );
        scene.pop_layer();
        scene.pop_layer();
    }
}

/// Intersection of two rects, or `None` if they don't overlap.
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x0.max(b.x0);
    let y0 = a.y0.max(b.y0);
    let x1 = a.x1.min(b.x1);
    let y1 = a.y1.min(b.y1);
    if x1 > x0 && y1 > y0 {
        Some(Rect::new(x0, y0, x1, y1))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CellValue;

    /// A grid with a known geometry: each column is the default width, the
    /// body starts at `ROW_HEADER_W` / `COL_HEADER_H` and is wide enough for
    /// many columns. No scroll.
    fn grid_with(values: &[(Addr, CellValue)]) -> Grid {
        let mut g = Grid::new();
        // Body roughly 12 columns wide so overflow has room.
        let width = ROW_HEADER_W + DEFAULT_COL_W * 12.0;
        let height = COL_HEADER_H + DEFAULT_ROW_H * 12.0;
        g.rect = Rect::new(0.0, 0.0, width, height);
        let mut map = HashMap::new();
        for (addr, v) in values {
            map.insert(*addr, v.clone());
        }
        g.values = map;
        g
    }

    #[test]
    fn occupied_detects_value_and_raw() {
        let mut g = grid_with(&[((1, 0), CellValue::Number(5.0))]);
        assert!(g.cell_is_occupied((1, 0)));
        assert!(!g.cell_is_occupied((2, 0)));
        // Empty CellValue is not occupied.
        g.values.insert((3, 0), CellValue::Empty);
        assert!(!g.cell_is_occupied((3, 0)));
        // A raw entry (mid-edit, pre-eval) counts as occupied too.
        g.raw.insert((4, 0), "x".into());
        assert!(g.cell_is_occupied((4, 0)));
    }

    #[test]
    fn editor_overflows_into_empty_neighbours() {
        // A0 is being edited; B0.. are all empty. A wide desired width should
        // grow across several empty columns.
        let g = grid_with(&[]);
        let cell = g.cell_screen_rect(0, 0);
        let body_right = g.body_rect().x1;
        // Desired width spanning ~3 columns.
        let desired = DEFAULT_COL_W * 3.0;
        let right = g.editor_right_extent((0, 0), desired, body_right);
        // It should reach at least 3 columns wide (within the body).
        assert!(
            right >= cell.x0 + DEFAULT_COL_W * 3.0 - 0.5,
            "right={right} expected >= {}",
            cell.x0 + DEFAULT_COL_W * 3.0
        );
        assert!(right <= body_right + 0.5);
    }

    #[test]
    fn editor_clips_at_first_occupied_neighbour() {
        // A0 edited; C0 (two cells over) is occupied. The editor may use B0
        // but must STOP before C0 even though it wants more room.
        let g = grid_with(&[((2, 0), CellValue::Text("blocker".into()))]);
        let cell = g.cell_screen_rect(0, 0);
        let body_right = g.body_rect().x1;
        let desired = DEFAULT_COL_W * 6.0; // wants a lot
        let right = g.editor_right_extent((0, 0), desired, body_right);
        // Reaches the end of B0 (= start of C0) but not into C0.
        let c0_left = g.cell_screen_rect(2, 0).x0;
        assert!((right - c0_left).abs() < 0.5, "right={right} c0_left={c0_left}");
    }

    #[test]
    fn editor_at_least_its_own_cell_when_neighbour_adjacent() {
        // B0 occupied directly to the right of A0 → editor stays cell-wide.
        let g = grid_with(&[((1, 0), CellValue::Number(1.0))]);
        let cell = g.cell_screen_rect(0, 0);
        let body_right = g.body_rect().x1;
        let right = g.editor_right_extent((0, 0), DEFAULT_COL_W * 4.0, body_right);
        assert!((right - cell.x1).abs() < 0.5, "right={right} cell.x1={}", cell.x1);
    }

    #[test]
    fn editor_never_exceeds_body() {
        // Edit the last visible column with a huge desired width.
        let g = grid_with(&[]);
        let body_right = g.body_rect().x1;
        let right = g.editor_right_extent((10, 0), DEFAULT_COL_W * 50.0, body_right);
        assert!(right <= body_right + 0.5, "right={right} body_right={body_right}");
    }
}

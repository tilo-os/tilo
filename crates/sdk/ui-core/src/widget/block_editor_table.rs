//! `Table` — a CRDT-native **editable grid**. Unlike
//! [`crate::widget::data_table::DataTable`] (a read-oriented, typed,
//! sortable list view), this is a structural editor: the user types into
//! cells and adds / removes rows and columns anywhere via hover affordances
//! in the gutters. The widget only reads the document: every change the user
//! makes is a [`TableEdit`] its owner takes ([`Table::take_edits`]) and
//! submits as an action, applying it with [`apply`].
//!
//! # Data model (under the widget's `path`, a Map)
//!
//! * `rows`   — `List<Str>`: the stable row ids, in display order.
//! * `cols`   — `List<Str>`: the stable column ids, in display order.
//! * `cells`  — `Map<Str, Text>`: cell content, keyed `"{row_id}|{col_id}"`.
//! * `widths` — `Map<Str, F64>`: per-column pixel width, keyed by column id.
//! * `header` — `Bool`: render the first row as a header band.
//! * `seq`    — `Int`: monotonic id counter (combined with the actor id).
//!
//! Row and column ids are stable and never renumbered on insert/delete — only
//! the `rows`/`cols` *order lists* change, so inserting a column in the
//! middle is a single list insert plus one empty `Text` per row, merging
//! cleanly with a concurrent edit to any other cell.
//!
//! # Rendering
//!
//! Each cell uses [`crate::widget::TextInput`] in plain-chrome mode, so caret
//! placement, selection, word navigation, and clipboard handling are shared.
//! Cells are laid out with a CSS-grid Taffy node (one track per column, fixed
//! row height); the widget paints the grid lines, header band, row banding,
//! and the insert / delete / resize affordances in the gutters on top.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use crate::widget::TaffyTreeExt;
use automerge::{ObjId, ObjType};
use taffy::style_helpers::length;
use taffy::{Display, NodeId, Rect as TaffyRect, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
pub use crate::table_model::*;
use crate::table_model::{
    DEFAULT_COL_W, MIN_COL_W, cell_key, cells_map, child_obj, col_width, usize_to_f64,
};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TextInput, Widget};

// ---------------------------------------------------------------------------
// Geometry constants (logical px)
// ---------------------------------------------------------------------------

/// Left rail: row-insert boundaries + per-row delete handles live here.
const LEFT_GUTTER: f64 = 24.0;
/// Top rail: column-insert boundaries + per-column delete handles live here.
const TOP_GUTTER: f64 = 24.0;
/// Trailing padding so the far-edge insert pill isn't clipped.
const RIGHT_PAD: f64 = 12.0;
const BOTTOM_PAD: f64 = 12.0;
/// Row height. Must match [`TextInput`]'s intrinsic cell height so the editor
/// fills its grid track exactly.
const ROW_H: f64 = 40.0;
/// How close (px) the pointer must be to a boundary to hit its insert pill.
const BOUNDARY_HIT: f64 = 11.0;
/// Half-width of the drag zone around an interior column boundary (resize).
const RESIZE_HIT: f64 = 4.0;
const PILL_R: f64 = 8.0;
const DELETE_SIDE: f64 = 15.0;
// ===========================================================================
// Interaction state
// ===========================================================================

/// What the pointer is currently over — recomputed on every `CursorMoved`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Hover {
    #[default]
    None,
    /// Insert-column pill at boundary index `0..=cols`.
    InsertCol(usize),
    /// Insert-row pill at boundary index `0..=rows`.
    InsertRow(usize),
    /// Delete-column handle over column `c`.
    DeleteCol(usize),
    /// Delete-row handle over row `r`.
    DeleteRow(usize),
    /// Interior column boundary `1..cols` (drag to resize the column to its
    /// left, `boundary - 1`).
    ResizeCol(usize),
    /// The empty-state "add first cell" affordance.
    EmptyAdd,
}

/// Pixel-snapped grid geometry for one paint: the interior column and row
/// edges, and the body they bound.
struct PaintedGrid {
    xs: Vec<f64>,
    ys: Vec<f64>,
    body: Rect,
}

/// Active column-resize drag.
struct Resize {
    col: usize,
    start_x: f64,
    orig_w: f64,
}

// ===========================================================================
// Widget
// ===========================================================================

pub struct Table {
    id: String,

    // --- model mirror (refreshed each `sync`) ---
    row_ids: Vec<String>,
    col_ids: Vec<String>,
    col_widths: Vec<f64>,
    header: bool,

    /// Cell editors in row-major order (`len == rows * cols`). Rebuilt only
    /// when the set of `(row, col)` ids changes.
    cells: Vec<TextInput>,
    /// What each cell showed at the last sync, so only a change is an edit.
    synced: Vec<String>,
    /// Resolved `cells` map, so children are rooted correctly.
    cells_obj: Option<ObjId>,
    /// The user's changes, waiting for the owner.
    edits: Vec<TableEdit>,

    // --- layout ---
    rect: Rect,

    // --- interaction ---
    last_cursor: Option<Point>,
    hover: Hover,
    resize: Option<Resize>,
    dirty: bool,
    structure_dirty: bool,
    projection_exit: Option<bool>,
}

impl Table {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            row_ids: Vec::new(),
            col_ids: Vec::new(),
            col_widths: Vec::new(),
            header: false,
            cells: Vec::new(),
            synced: Vec::new(),
            cells_obj: None,
            edits: Vec::new(),
            rect: Rect::ZERO,
            last_cursor: None,
            hover: Hover::None,
            resize: None,
            dirty: true,
            structure_dirty: false,
            projection_exit: None,
        }
    }

    /// The user's changes since the last call, in the order made, for the
    /// owner to submit as actions (each applied with [`apply`]).
    pub fn take_edits(&mut self) -> Vec<TableEdit> {
        std::mem::take(&mut self.edits)
    }

    /// Hand each cell's typing since the last sync to the owner.
    fn collect_cell_edits(&mut self) {
        let columns = self.n_cols();
        for (index, cell) in self.cells.iter_mut().enumerate() {
            let Some(text) = cell.take_dirty() else {
                continue;
            };
            if self.synced.get(index) == Some(&text) || columns == 0 {
                continue;
            }
            if let Some(seen) = self.synced.get_mut(index) {
                seen.clone_from(&text);
            }
            self.edits.push(TableEdit::SetCell {
                row: index / columns,
                column: index % columns,
                text,
            });
        }
    }

    fn n_rows(&self) -> usize {
        self.row_ids.len()
    }
    fn n_cols(&self) -> usize {
        self.col_ids.len()
    }
    fn is_empty(&self) -> bool {
        self.n_rows() == 0 || self.n_cols() == 0
    }

    fn projected_index(&self, row: usize, column: usize) -> Option<usize> {
        row.checked_mul(self.n_cols())?
            .checked_add(column)
            .filter(|index| *index < self.cells.len())
    }

    fn projected_position(&self, index: usize) -> Option<(usize, usize)> {
        let columns = self.n_cols();
        Some((index.checked_div(columns)?, index.checked_rem(columns)?))
    }

    fn make_cell(rid: &str, cid: &str) -> TextInput {
        TextInput::new(vec![])
            .with_id(&format!("cell_{rid}_{cid}"))
            .with_plain_chrome()
            .with_text_role(crate::tokens::TextRole::Body)
    }

    /// Local projection for hosts whose canonical table is already represented
    /// by another resource (for example a Markdown block). No shadow CRDT is
    /// created: edits are returned to the host's normal action bridge.
    pub(super) fn project_cells(&mut self, rows: &[Vec<String>]) {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let saved_focus = self.projected_focus().and_then(|(row, column)| {
            let state = crate::undo::get_state(self.cells.get(self.projected_index(row, column)?)?);
            Some((row, column, state))
        });
        let shape_changed = self.n_rows() != rows.len() || self.n_cols() != columns;
        if shape_changed {
            self.row_ids = (0..rows.len()).map(|row| row.to_string()).collect();
            self.col_ids = (0..columns).map(|column| column.to_string()).collect();
            self.cells = (0..rows.len().saturating_mul(columns))
                .map(|_| {
                    let mut input = TextInput::local()
                        .with_plain_chrome()
                        .with_text_role(crate::tokens::TextRole::Body);
                    input.set_multiline(true);
                    input
                })
                .collect();
        }
        for (index, input) in self.cells.iter_mut().enumerate() {
            let value = index
                .checked_div(columns)
                .zip(index.checked_rem(columns))
                .and_then(|(row, column)| rows.get(row).and_then(|row| row.get(column)))
                .map_or("", String::as_str);
            // set_text is a no-op for an unchanged model echo, retaining caret
            // and selection through normal redraws and canonical commits.
            input.set_text(value);
        }
        if shape_changed
            && let Some((row, column, caret)) = saved_focus
            && !rows.is_empty()
            && columns > 0
        {
            let clamped = (
                row.min(rows.len().saturating_sub(1)),
                column.min(columns.saturating_sub(1)),
            );
            self.focus_projected_cell(clamped.0, clamped.1);
            if clamped != (row, column) {
                // The cell is gone: the caret ends the one that took its place.
                if let Some(input) = self
                    .projected_index(clamped.0, clamped.1)
                    .and_then(|index| self.cells.get_mut(index))
                {
                    let end = input.value().len();
                    input.set_cursor_pos(end);
                }
            } else if let Some(caret) = caret {
                self.restore_projected_caret(&caret);
            }
        }
    }

    pub(super) fn project_geometry(
        &mut self,
        rects: &[Rect],
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        for (input, rect) in self.cells.iter_mut().zip(rects) {
            input.apply_layout_external(*rect);
            input.prepare_wrapped_layout(tr, theme);
        }
    }

    pub(super) fn projected_focus(&self) -> Option<(usize, usize)> {
        self.cells
            .iter()
            .position(Widget::is_focused)
            .and_then(|index| self.projected_position(index))
    }

    pub(super) fn projected_is_composing(&self) -> bool {
        self.cells
            .iter()
            .any(|input| input.is_focused() && input.is_composing())
    }

    pub(super) fn projected_caret(&self) -> Option<String> {
        let (row, column) = self.projected_focus()?;
        crate::undo::get_state(self.cells.get(self.projected_index(row, column)?)?)
    }

    pub(super) fn restore_projected_caret(&mut self, state: &str) {
        let Some((row, column)) = self.projected_focus() else {
            return;
        };
        let Some(index) = self.projected_index(row, column) else {
            return;
        };
        let Some(input) = self.cells.get_mut(index) else {
            return;
        };
        crate::undo::set_state(input, state);
        let caret = input.caret_position();
        let selection = input.selection_range_public();
        input.set_caret_position(caret);
        if let Some((start, end)) = selection {
            let anchor = if caret.byte == start { end } else { start };
            let clamped_caret = input.caret_position();
            input.set_cursor_pos(anchor);
            let clamped_anchor = input.cursor_pos();
            input.set_selection_at(clamped_anchor, clamped_caret);
        }
    }

    pub(super) fn focus_projected_cell(&mut self, row: usize, column: usize) -> bool {
        if row >= self.n_rows() || column >= self.n_cols() {
            return false;
        }
        let Some(target) = self.projected_index(row, column) else {
            return false;
        };
        for (index, input) in self.cells.iter_mut().enumerate() {
            input.set_focus(index == target);
        }
        true
    }

    pub(super) fn blur_projection(&mut self) {
        for input in &mut self.cells {
            input.set_focus(false);
        }
    }

    pub(super) fn projected_rect(&self, row: usize, column: usize) -> Option<Rect> {
        self.cells
            .get(row.checked_mul(self.n_cols())?.checked_add(column)?)
            .map(Widget::get_rect)
    }

    pub(super) fn take_projection_exit(&mut self) -> Option<bool> {
        self.projection_exit.take()
    }

    pub(super) fn projected_ime_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let (index, input) = self
            .cells
            .iter()
            .enumerate()
            .find(|(_, input)| input.is_focused())?;
        Some(crate::ime::scoped_fingerprint(
            &index.to_le_bytes(),
            input.ime_edit_fingerprint(key)?,
            key,
        ))
    }

    pub(super) fn projected_ime_rect(&self) -> Option<Rect> {
        self.cells
            .iter()
            .find(|input| input.is_focused())
            .and_then(Widget::ime_cursor_rect)
    }

    #[allow(
        clippy::option_option,
        reason = "outer Option distinguishes an unsupported command; inner Option distinguishes a handled command with no cell edit"
    )]
    pub(super) fn projected_clipboard(
        &mut self,
        command: &str,
    ) -> Option<Option<(usize, usize, String)>> {
        let (row, column) = self.projected_focus()?;
        let index = self.projected_index(row, column)?;
        let input = self.cells.get_mut(index)?;
        let before = input.value().to_owned();
        match command {
            "select-all" => input.set_selection(0, input.value().len()),
            "copy" | "cut" => {
                if let Some((start, end)) = input.selection_range_public()
                    && let Some(selected) = input.value().get(start..end)
                {
                    let accepted = crate::text_selection::clipboard_copy_accepted(selected);
                    if command == "cut" && accepted {
                        input.delete_local_selection();
                    }
                }
            }
            "paste" => {
                if let Some(value) = crate::text_selection::clipboard_paste() {
                    input.insert_at_cursor(&value);
                }
            }
            _ => return None,
        }
        let changed = (input.value() != before).then(|| (row, column, input.value().to_owned()));
        let _ = input.take_dirty();
        Some(changed)
    }

    pub(super) fn draw_projected_focus(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        suppress_caret: bool,
    ) {
        for input in self.cells.iter_mut().filter(|input| input.is_focused()) {
            let rect = input.get_rect();
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &rect,
            );
            input.suppress_caret_paint(suppress_caret);
            input.draw(scene, transform, tr, theme, presence);
            input.suppress_caret_paint(false);
        }
    }

    pub(super) fn handle_projected_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> Option<(usize, usize, String)> {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            if let Some(index) = self.last_cursor.and_then(|point| {
                self.cells
                    .iter()
                    .position(|input| input.get_rect().contains(point))
            }) {
                if let Some((row, column)) = self.projected_position(index) {
                    self.focus_projected_cell(row, column);
                }
                *ctx.focus_target = Some(ctx.node_id);
            } else {
                self.blur_projection();
            }
        }
        let (row, column) = self.projected_focus()?;
        let focused = self.projected_index(row, column)?;
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
        {
            let backwards = ctx.modifiers.state().shift_key();
            let step = match event.logical_key {
                Key::Named(NamedKey::Tab) => Some(1),
                Key::Named(NamedKey::Enter)
                    if !ctx.modifiers.state().control_key()
                        && !ctx.modifiers.state().super_key() =>
                {
                    Some(self.n_cols())
                }
                _ => None,
            };
            if let Some(step) = step {
                let target = if backwards {
                    focused.checked_sub(step)
                } else {
                    focused
                        .checked_add(step)
                        .filter(|target| *target < self.cells.len())
                };
                if let Some(target) = target {
                    if let Some((row, column)) = self.projected_position(target) {
                        self.focus_projected_cell(row, column);
                    }
                } else {
                    self.blur_projection();
                    self.projection_exit = Some(!backwards);
                }
                *ctx.handled = true;
                return None;
            }
            if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                self.blur_projection();
                self.projection_exit = Some(true);
                *ctx.handled = true;
                return None;
            }
        }
        let input = self.cells.get_mut(focused)?;
        input.handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
        input.take_dirty().map(|text| (row, column, text))
    }

    /// Analytic grid geometry from the widget rect + stored widths — matches
    /// the Taffy layout exactly (deterministic; independent of cell rects).
    /// Returns `(xs, ys)`: the `cols+1` vertical and `rows+1` horizontal edges.
    fn grid_edges(&self) -> (Vec<f64>, Vec<f64>) {
        let left = self.rect.x0 + LEFT_GUTTER;
        let top = self.rect.y0 + TOP_GUTTER;
        let mut xs = Vec::with_capacity(self.n_cols().saturating_add(1));
        let mut x = left;
        xs.push(x);
        for w in &self.col_widths {
            x += *w;
            xs.push(x);
        }
        let mut ys = Vec::with_capacity(self.n_rows().saturating_add(1));
        let mut y = top;
        ys.push(y);
        for _ in 0..self.n_rows() {
            y += ROW_H;
            ys.push(y);
        }
        (xs, ys)
    }

    fn grid_right(&self) -> f64 {
        self.rect.x0 + LEFT_GUTTER + self.col_widths.iter().sum::<f64>()
    }
    fn grid_bottom(&self) -> f64 {
        self.rect.y0 + TOP_GUTTER + usize_to_f64(self.n_rows()) * ROW_H
    }

    /// Rect of the delete handle for column `c` (centred in the top gutter).
    /// `c` past the current grid (a stale hover from before a structural
    /// edit) degenerates to a zero-width rect rather than panicking.
    fn delete_col_rect(&self, xs: &[f64], c: usize) -> Rect {
        let x0 = xs.get(c).copied().unwrap_or(self.rect.x0 + LEFT_GUTTER);
        let x1 = xs.get(c.saturating_add(1)).copied().unwrap_or(x0);
        let cx = f64::midpoint(x0, x1);
        let cy = self.rect.y0 + TOP_GUTTER / 2.0;
        Rect::from_center_size((cx, cy), (DELETE_SIDE, DELETE_SIDE))
    }
    /// Rect of the delete handle for row `r` (centred in the left gutter).
    /// Same stale-hover fallback as [`Self::delete_col_rect`].
    fn delete_row_rect(&self, ys: &[f64], r: usize) -> Rect {
        let cx = self.rect.x0 + LEFT_GUTTER / 2.0;
        let y0 = ys.get(r).copied().unwrap_or(self.rect.y0 + TOP_GUTTER);
        let y1 = ys.get(r.saturating_add(1)).copied().unwrap_or(y0);
        let cy = f64::midpoint(y0, y1);
        Rect::from_center_size((cx, cy), (DELETE_SIDE, DELETE_SIDE))
    }

    /// Hit-test the pointer against every affordance, in priority order.
    fn hit(&self, p: Point) -> Hover {
        if !self.rect.contains(p) {
            return Hover::None;
        }
        if self.is_empty() {
            return if self.empty_add_rect().contains(p) {
                Hover::EmptyAdd
            } else {
                Hover::None
            };
        }
        let (xs, ys) = self.grid_edges();
        let in_top = p.y < self.rect.y0 + TOP_GUTTER;
        let in_left = p.x < self.rect.x0 + LEFT_GUTTER;

        // Top gutter → column insert boundaries + column delete handles.
        if in_top && p.x >= self.rect.x0 + LEFT_GUTTER - BOUNDARY_HIT {
            for (i, &bx) in xs.iter().enumerate() {
                if (p.x - bx).abs() <= BOUNDARY_HIT {
                    return Hover::InsertCol(i);
                }
            }
            for c in 0..self.n_cols() {
                if self.delete_col_rect(&xs, c).contains(p) {
                    return Hover::DeleteCol(c);
                }
            }
            return Hover::None;
        }
        // Left gutter → row insert boundaries + row delete handles.
        if in_left && p.y >= self.rect.y0 + TOP_GUTTER - BOUNDARY_HIT {
            for (i, &by) in ys.iter().enumerate() {
                if (p.y - by).abs() <= BOUNDARY_HIT {
                    return Hover::InsertRow(i);
                }
            }
            for r in 0..self.n_rows() {
                if self.delete_row_rect(&ys, r).contains(p) {
                    return Hover::DeleteRow(r);
                }
            }
            return Hover::None;
        }
        // Grid body → interior column boundaries resize the column to the left.
        if p.x >= self.rect.x0 + LEFT_GUTTER && p.y >= self.rect.y0 + TOP_GUTTER {
            for (i, &x) in xs.iter().enumerate().skip(1) {
                if (p.x - x).abs() <= RESIZE_HIT {
                    return Hover::ResizeCol(i);
                }
            }
        }
        Hover::None
    }

    /// The empty-state affordance box.
    fn empty_add_rect(&self) -> Rect {
        let cx = self.rect.x0 + LEFT_GUTTER + DEFAULT_COL_W / 2.0;
        let cy = self.rect.y0 + TOP_GUTTER + ROW_H / 2.0;
        Rect::from_center_size((cx, cy), (DEFAULT_COL_W, ROW_H))
    }

    /// Total widget size for the current model.
    fn total_size(&self) -> (f64, f64) {
        if self.is_empty() {
            return (
                LEFT_GUTTER + DEFAULT_COL_W + RIGHT_PAD,
                TOP_GUTTER + ROW_H + BOTTOM_PAD,
            );
        }
        (
            LEFT_GUTTER + self.col_widths.iter().sum::<f64>() + RIGHT_PAD,
            TOP_GUTTER + usize_to_f64(self.n_rows()) * ROW_H + BOTTOM_PAD,
        )
    }

    /// Draw the placeholder "add first cell" affordance shown when the table
    /// has no rows or columns yet.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        radius: f64,
    ) {
        let tk = &theme.tokens;
        let r = self.empty_add_rect();
        let hovered = self.hover == Hover::EmptyAdd;
        let box_color = if hovered {
            theme.tokens.colors.intent.selection
        } else {
            tk.colors.surfaces.raised
        };
        let rr = RoundedRect::from_rect(r, radius);
        scene.fill(Fill::NonZero, transform, box_color, None, &rr);
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            if hovered {
                theme.tokens.colors.intent.inverted
            } else {
                crate::surface::edge_on(box_color)
            },
            None,
            &rr,
        );
        let c = r.center();
        plus(
            scene,
            transform,
            Point::new(c.x - 34.0, c.y),
            6.0,
            theme.tokens.colors.intent.inverted,
            2.0,
        );
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine composition is float matrix multiplication; it cannot overflow \
                      or panic"
        )]
        let label_transform = transform * Affine::translate((c.x - 20.0, c.y - 9.0));
        text_renderer.draw_text(
            scene,
            "Add cell",
            14.0,
            theme.text_primary,
            label_transform,
            None,
        );
    }

    /// The grid as painted. Painted geometry snaps to whole pixels so a 1 px
    /// rule is exactly one row or column: a column width is a doc value and
    /// fractional after a drag. Hit-testing keeps the exact edges; the
    /// difference is under half a pixel.
    fn painted_grid(&self) -> PaintedGrid {
        let (xs, ys) = self.grid_edges();
        let xs: Vec<f64> = xs.into_iter().map(f64::round).collect();
        let ys: Vec<f64> = ys.into_iter().map(f64::round).collect();
        // Painted only past `is_empty()`, so both edge lists have >= 2
        // entries; the fallbacks below never actually apply.
        let gl = xs.first().copied().unwrap_or(self.rect.x0 + LEFT_GUTTER);
        let gr = self.grid_right().round();
        let gt = ys.first().copied().unwrap_or(self.rect.y0 + TOP_GUTTER);
        let gb = self.grid_bottom().round();
        PaintedGrid {
            xs,
            ys,
            body: Rect::new(gl, gt, gr, gb),
        }
    }

    /// Draw the grid's line work: interior rules, the heavier separator
    /// under a header row, and the always-on insert ticks at every boundary.
    fn draw_grid_lines(&self, ctx: &mut PaintCtx<'_>, grid: &PaintedGrid) {
        let PaintCtx {
            scene,
            transform,
            theme,
            ..
        } = ctx.reborrow();
        let (xs, ys) = (grid.xs.as_slice(), grid.ys.as_slice());
        let Rect {
            x0: gl,
            y0: gt,
            x1: gr,
            y1: gb,
        } = grid.body;
        let tk = &theme.tokens;
        let line = Stroke::new(1.0);
        // Rules derive from the body surface they cross (HIG §9.5): a white
        // veneer vanishes on a light table, a declared border on a dark one.
        let line_color = crate::surface::edge_on(tk.colors.surfaces.raised);
        // A 1 px rule centred on an integer edge smears over two half-alpha
        // columns; the edges are whole pixels (see `draw`), so centre each
        // rule on the pixel just inside the body.
        let snap_x = |x: f64| if x >= gr { x - 0.5 } else { x + 0.5 };
        let snap_y = |y: f64| if y >= gb { y - 0.5 } else { y + 0.5 };
        for &x in xs {
            let x = snap_x(x);
            scene.stroke(
                &line,
                transform,
                line_color,
                None,
                &Line::new((x, gt), (x, gb)),
            );
        }
        for &y in ys {
            let y = snap_y(y);
            scene.stroke(
                &line,
                transform,
                line_color,
                None,
                &Line::new((gl, y), (gr, y)),
            );
        }
        // Heavier separator under the header: the same crisp 1 px rule at
        // the active-glow strength, so it reads darker/brighter rather than
        // as a blurred one-and-a-half pixel band.
        if self.header
            && self.n_rows() >= 1
            && let Some(&y1) = ys.get(1)
        {
            let y = snap_y(y1);
            scene.stroke(
                &line,
                transform,
                crate::surface::edge_on_with(tk.colors.surfaces.raised, crate::chrome::GLOW_ALPHA),
                None,
                &Line::new((gl, y), (gr, y)),
            );
        }

        // Always-on, discoverable insert ticks at every boundary.
        let faint = theme.text_dim.with_alpha_factor(0.35);
        for &x in xs {
            let c = Point::new(x, self.rect.y0 + TOP_GUTTER / 2.0);
            plus(scene, transform, c, 3.5, faint, 1.25);
        }
        for &y in ys {
            let c = Point::new(self.rect.x0 + LEFT_GUTTER / 2.0, y);
            plus(scene, transform, c, 3.5, faint, 1.25);
        }
    }

    /// Draw hover emphasis for the affordance currently under the pointer.
    /// A hover index that no longer matches the current grid (stale from
    /// just before a structural edit) is silently skipped rather than
    /// indexing out of bounds.
    fn draw_hover_affordance(&self, ctx: &mut PaintCtx<'_>, grid: &PaintedGrid) {
        let PaintCtx {
            scene,
            transform,
            theme,
            ..
        } = ctx.reborrow();
        let (xs, ys) = (grid.xs.as_slice(), grid.ys.as_slice());
        let Rect {
            x0: gl,
            y0: gt,
            x1: gr,
            y1: gb,
        } = grid.body;
        let tk = &theme.tokens;
        let accent = theme.tokens.colors.intent.inverted;
        // The glyph inside a gutter affordance is a mark on a themed chip, so
        // it is derived against that chip rather than assumed to be white: on
        // the high-contrast accent white lands a hair under the non-text floor,
        // which is the palette that can least afford it.
        let on_accent_mark = tk.colors.ink_on_any(
            &[accent],
            tk.colors.text_primary,
            crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
        );
        let danger = tk.colors.semantic.danger;
        let on_danger_mark = tk.colors.ink_on_any(
            &[danger],
            Color::WHITE,
            crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
        );

        match self.hover {
            Hover::InsertCol(i) => {
                let Some(&x) = xs.get(i) else { return };
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    accent,
                    None,
                    &Line::new((x, gt), (x, gb)),
                );
                let c = Point::new(x, self.rect.y0 + TOP_GUTTER / 2.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &vello::kurbo::Circle::new(c, PILL_R),
                );
                plus(scene, transform, c, 4.0, on_accent_mark, 1.75);
            }
            Hover::InsertRow(i) => {
                let Some(&y) = ys.get(i) else { return };
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    accent,
                    None,
                    &Line::new((gl, y), (gr, y)),
                );
                let c = Point::new(self.rect.x0 + LEFT_GUTTER / 2.0, y);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &vello::kurbo::Circle::new(c, PILL_R),
                );
                plus(scene, transform, c, 4.0, on_accent_mark, 1.75);
            }
            Hover::DeleteCol(c) => {
                let (Some(&x0), Some(&x1)) = (xs.get(c), xs.get(c.saturating_add(1))) else {
                    return;
                };
                let col = Rect::new(x0, gt, x1, gb);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    danger.with_alpha_factor(0.16),
                    None,
                    &col,
                );
                let hr = self.delete_col_rect(xs, c);
                let rr = RoundedRect::from_rect(hr, 4.0);
                scene.fill(Fill::NonZero, transform, danger, None, &rr);
                cross(scene, transform, hr.center(), 3.5, on_danger_mark, 1.75);
            }
            Hover::DeleteRow(r) => {
                let (Some(&y0), Some(&y1)) = (ys.get(r), ys.get(r.saturating_add(1))) else {
                    return;
                };
                let row = Rect::new(gl, y0, gr, y1);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    danger.with_alpha_factor(0.16),
                    None,
                    &row,
                );
                let hr = self.delete_row_rect(ys, r);
                let rr = RoundedRect::from_rect(hr, 4.0);
                scene.fill(Fill::NonZero, transform, danger, None, &rr);
                cross(scene, transform, hr.center(), 3.5, on_danger_mark, 1.75);
            }
            Hover::ResizeCol(i) => {
                let Some(&x) = xs.get(i) else { return };
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    accent,
                    None,
                    &Line::new((x, gt), (x, gb)),
                );
            }
            Hover::None | Hover::EmptyAdd => {}
        }
    }

    /// Handle a left-button press against whatever affordance the pointer is
    /// currently over. Returns `true` when the press was fully consumed by a
    /// table-level affordance (insert/delete/resize) — in that case the
    /// caller must not also forward the event to the cell editors.
    fn handle_press(&mut self, p: Point, handled: &mut bool) -> bool {
        match self.hit(p) {
            Hover::EmptyAdd => {
                if self.n_cols() == 0 {
                    self.edits.push(TableEdit::InsertColumn { at: 0 });
                }
                if self.n_rows() == 0 {
                    self.edits.push(TableEdit::InsertRow { at: 0 });
                }
                self.dirty = true;
                *handled = true;
                true
            }
            Hover::InsertCol(i) => {
                self.edits.push(TableEdit::InsertColumn { at: i });
                self.dirty = true;
                *handled = true;
                true
            }
            Hover::InsertRow(i) => {
                self.edits.push(TableEdit::InsertRow { at: i });
                self.dirty = true;
                *handled = true;
                true
            }
            Hover::DeleteCol(c) => {
                self.edits.push(TableEdit::RemoveColumn { at: c });
                self.dirty = true;
                *handled = true;
                true
            }
            Hover::DeleteRow(r) => {
                self.edits.push(TableEdit::RemoveRow { at: r });
                self.dirty = true;
                *handled = true;
                true
            }
            Hover::ResizeCol(i) => {
                let Some(col) = i.checked_sub(1) else {
                    return false;
                };
                let Some(&orig_w) = self.col_widths.get(col) else {
                    return false;
                };
                self.resize = Some(Resize {
                    col,
                    start_x: p.x,
                    orig_w,
                });
                *handled = true;
                true
            }
            Hover::None => false,
        }
    }

    /// Forward an event to the cell editors: a pointer-capture short-circuit
    /// to whichever cell is mid-drag, otherwise each cell is tried in order
    /// until one consumes the event.
    fn forward_to_cells(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
        child_nodes: &[NodeId],
    ) {
        let Some(cells_obj) = self.cells_obj.clone() else {
            return;
        };

        // Pointer-capture short-circuit (a cell dragging a selection).
        if let Some(idx) = self
            .cells
            .iter()
            .position(super::Widget::wants_pointer_capture)
            && let Some(&cn) = child_nodes.get(idx)
            && let Some(cell) = self.cells.get_mut(idx)
        {
            cell.handle_event(event, &mut ctx.reborrow(&cells_obj, cn));
            return;
        }
        for (cell, &cn) in self.cells.iter_mut().zip(child_nodes.iter()) {
            cell.handle_event(event, &mut ctx.reborrow(&cells_obj, cn));
            if *ctx.handled {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Small draw helpers
// ---------------------------------------------------------------------------

fn plus(scene: &mut Scene, transform: Affine, c: Point, r: f64, color: Color, w: f64) {
    let s = Stroke::new(w);
    scene.stroke(
        &s,
        transform,
        color,
        None,
        &Line::new((c.x - r, c.y), (c.x + r, c.y)),
    );
    scene.stroke(
        &s,
        transform,
        color,
        None,
        &Line::new((c.x, c.y - r), (c.x, c.y + r)),
    );
}

fn cross(scene: &mut Scene, transform: Affine, c: Point, r: f64, color: Color, w: f64) {
    let s = Stroke::new(w);
    scene.stroke(
        &s,
        transform,
        color,
        None,
        &Line::new((c.x - r, c.y - r), (c.x + r, c.y + r)),
    );
    scene.stroke(
        &s,
        transform,
        color,
        None,
        &Line::new((c.x - r, c.y + r), (c.x + r, c.y - r)),
    );
}

impl Widget for Table {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = crate::doc::document_ref(state, path);

        let new_rows = row_ids(state, path);
        let new_cols = col_ids(state, path);
        self.header = header(state, path);
        self.col_widths = new_cols
            .iter()
            .map(|cid| col_width(state, path, cid))
            .collect();
        self.cells_obj = cells_map(state, path);

        // Rebuild the cell editors only when the id set actually changed.
        if new_rows != self.row_ids || new_cols != self.col_ids {
            let mut cells = Vec::with_capacity(new_rows.len().saturating_mul(new_cols.len()));
            for rid in &new_rows {
                for cid in &new_cols {
                    cells.push(Self::make_cell(rid, cid));
                }
            }
            self.cells = cells;
            self.row_ids = new_rows;
            self.col_ids = new_cols;
            self.structure_dirty = true;
            self.dirty = true;
        }

        // Each cell shows its text in the `cells` map; typing stays local.
        if let Some(cells_obj) = self.cells_obj.clone() {
            let keys: Vec<String> = self
                .row_ids
                .iter()
                .flat_map(|rid| self.col_ids.iter().map(move |cid| cell_key(rid, cid)))
                .collect();
            for (cell, key) in self.cells.iter_mut().zip(&keys) {
                match child_obj(state, &cells_obj, key, ObjType::Text) {
                    Some(obj) if cell.bound_obj() != Some(&obj) => cell.bind_text(obj),
                    Some(_) => {}
                    None => cell.clear_text_binding(),
                }
                cell.sync(state, &cells_obj);
            }
        }
        self.synced = self
            .cells
            .iter()
            .map(|cell| cell.value().to_owned())
            .collect();
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "every value here is a widget/grid geometry constant or a stored column \
                  width, all bounded pixel magnitudes well within f32 range — narrowing to \
                  f32 for Taffy's layout math loses no meaningful precision"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (total_w, total_h) = self.total_size();

        let node = if self.is_empty() {
            taffy.leaf_node(Style {
                size: Size {
                    width: length(total_w as f32),
                    height: length(total_h as f32),
                },
                ..Default::default()
            })
        } else {
            let child_nodes: Vec<NodeId> = self
                .cells
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let cols: Vec<_> = self.col_widths.iter().map(|w| length(*w as f32)).collect();
            taffy.parent_node(
                Style {
                    display: Display::Grid,
                    size: Size {
                        width: length(total_w as f32),
                        height: length(total_h as f32),
                    },
                    padding: TaffyRect {
                        left: length(LEFT_GUTTER as f32),
                        right: length(RIGHT_PAD as f32),
                        top: length(TOP_GUTTER as f32),
                        bottom: length(BOTTOM_PAD as f32),
                    },
                    grid_template_columns: cols,
                    grid_auto_rows: vec![length(ROW_H as f32)],
                    ..Default::default()
                },
                &child_nodes,
            )
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let cell_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(child_nodes) = taffy.children(node) {
            for (cell, &cn) in self.cells.iter_mut().zip(child_nodes.iter()) {
                cell.apply_layout(taffy, cn, cell_origin);
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tk = &theme.tokens;
        let radius = f64::from(tk.radii.item);

        // Outer frame (spans the gutters too).
        let frame = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            tk.colors.surfaces.base,
            None,
            &frame,
        );

        if self.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme, radius);
            return;
        }

        let grid = self.painted_grid();
        let (ys, body) = (&grid.ys, grid.body);
        let (gl, gr) = (body.x0, body.x1);

        // Grid body surface + header band + zebra rows.
        scene.fill(
            Fill::NonZero,
            transform,
            tk.colors.surfaces.raised,
            None,
            &body,
        );
        for (r, w) in ys.windows(2).enumerate() {
            let [y0, y1] = *w else { continue };
            let row_rect = Rect::new(gl, y0, gr, y1);
            let is_header = self.header && r == 0;
            // Header band and zebra stripe are derived tiers on the body
            // surface (HIG §9.5), so they flip polarity on a light palette
            // instead of laying a white veneer over a white table.
            let fill = if is_header || r % 2 == 1 {
                crate::widget::data_table::tier_on(
                    tk.colors.surfaces.raised,
                    crate::chrome::HAIRLINE_ALPHA,
                )
            } else {
                continue;
            };
            scene.fill(Fill::NonZero, transform, fill, None, &row_rect);
        }

        // Cells (real TextInputs paint their own text + caret).
        for cell in &mut self.cells {
            cell.draw(scene, transform, text_renderer, theme, presence);
        }

        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_grid_lines(&mut ctx, &grid);
        self.draw_hover_affordance(&mut ctx, &grid);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let child_nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();

        // ---- active column resize captures the pointer ----
        if let Some(rz) = self.resize.as_ref() {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    let p =
                        Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                    let new_w = (rz.orig_w + (p.x - rz.start_x)).max(MIN_COL_W);
                    // The column follows the pointer here; the owner gets
                    // the width when the drag ends.
                    if let Some(width) = self.col_widths.get_mut(rz.col) {
                        *width = new_w;
                    }
                    self.structure_dirty = true; // re-run build_layout with new tracks
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                } => {
                    if let Some(rz) = self.resize.take()
                        && let Some(&width) = self.col_widths.get(rz.col)
                    {
                        self.edits.push(TableEdit::ResizeColumn {
                            column: rz.col,
                            width,
                        });
                    }
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = Some(p);
                let h = self.hit(p);
                if h != self.hover {
                    self.hover = h;
                    self.dirty = true;
                }
                // fall through so cells update their own hover/drag state
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(p) = self.last_cursor
                    && self.handle_press(p, ctx.handled)
                {
                    return;
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                // Enter moves the caret down one row (Shift+Enter → up).
                if matches!(key_event.logical_key, Key::Named(NamedKey::Enter))
                    && let Some(focused) = self.cells.iter().position(super::Widget::is_focused)
                {
                    let cols = self.n_cols().max(1);
                    let up = ctx.modifiers.state().shift_key();
                    let target = if up {
                        focused.checked_sub(cols)
                    } else {
                        focused.checked_add(cols).filter(|&t| t < self.cells.len())
                    };
                    if let Some(t) = target
                        && let Some(&tn) = child_nodes.get(t)
                    {
                        *ctx.focus_target = Some(tn);
                        *ctx.handled = true;
                        return;
                    }
                }
            }
            _ => {}
        }

        self.forward_to_cells(event, ctx, &child_nodes);
        self.collect_cell_edits();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.cells.iter().map(|c| c as &dyn Widget).collect()
    }
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for cell in &self.cells {
            visitor(cell);
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.cells
            .iter_mut()
            .map(|c| c as &mut (dyn Widget + '_))
            .collect()
    }

    fn name(&self) -> &'static str {
        "Table"
    }
    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        match self.hit(point) {
            Hover::ResizeCol(_) => Some(winit::window::CursorIcon::EwResize),
            Hover::InsertCol(_)
            | Hover::InsertRow(_)
            | Hover::DeleteCol(_)
            | Hover::DeleteRow(_)
            | Hover::EmptyAdd => Some(winit::window::CursorIcon::Pointer),
            Hover::None => None,
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.resize.is_some() || self.cells.iter().any(super::Widget::wants_pointer_capture)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut d = self.structure_dirty;
        self.structure_dirty = false;
        d |= self
            .cells
            .iter_mut()
            .any(super::Widget::take_structure_dirty);
        d
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.cells.iter().any(super::Widget::needs_redraw)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        for c in &mut self.cells {
            c.mark_drawn();
        }
    }
    fn is_layout_dirty(&self) -> bool {
        self.structure_dirty || self.cells.iter().any(super::Widget::is_layout_dirty)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::Table))
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT, transaction::Transactable};
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Size as TSize};

    use winit::event::Modifiers;

    /// Fresh document with an empty table map object at `root["t"]`.
    fn doc_with_table() -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let t = doc.put_object(&ROOT, "t", ObjType::Map).unwrap();
        ensure(&mut doc, &t);
        (doc, t)
    }

    /// Seed a `rows`×`cols` grid and fill each cell with `"r{c}"`.
    fn seed(doc: &mut AutoCommit, t: &ObjId, rows: usize, cols: usize) {
        for _ in 0..cols {
            insert_column(doc, t, usize::MAX);
        }
        for _ in 0..rows {
            insert_row(doc, t, usize::MAX);
        }
        for r in 0..rows {
            for c in 0..cols {
                set_cell_text(doc, t, r, c, &format!("{r}{c}"));
            }
        }
    }

    // ---- pure model-op tests (the production path) --------------------

    #[test]
    fn table_model_inserts_removes_and_keeps_stable_ids() {
        // Case `insert_and_read_roundtrip`.
        {
            let (mut doc, t) = doc_with_table();
            seed(&mut doc, &t, 2, 2);
            assert_eq!(dims(&doc, &t), (2, 2));
            assert_eq!(cell_text(&doc, &t, 0, 0), "00");
            assert_eq!(cell_text(&doc, &t, 1, 1), "11");
        }

        // Case `insert_column_in_the_middle_preserves_and_shifts_content`.
        {
            let (mut doc, t) = doc_with_table();
            // one row, three columns: A B C
            insert_row(&mut doc, &t, 0);
            for _ in 0..3 {
                insert_column(&mut doc, &t, usize::MAX);
            }
            set_cell_text(&mut doc, &t, 0, 0, "A");
            set_cell_text(&mut doc, &t, 0, 1, "B");
            set_cell_text(&mut doc, &t, 0, 2, "C");

            insert_column(&mut doc, &t, 1); // middle, between A and B

            assert_eq!(dims(&doc, &t), (1, 4));
            let texts: Vec<String> = (0..4).map(|c| cell_text(&doc, &t, 0, c)).collect();
            assert_eq!(
                texts,
                vec!["A", "", "B", "C"],
                "new empty column lands in the middle"
            );
        }

        // Case `insert_row_in_the_middle_preserves_and_shifts_content`.
        {
            let (mut doc, t) = doc_with_table();
            insert_column(&mut doc, &t, 0);
            for _ in 0..3 {
                insert_row(&mut doc, &t, usize::MAX);
            }
            set_cell_text(&mut doc, &t, 0, 0, "R0");
            set_cell_text(&mut doc, &t, 1, 0, "R1");
            set_cell_text(&mut doc, &t, 2, 0, "R2");

            insert_row(&mut doc, &t, 1);

            assert_eq!(dims(&doc, &t), (4, 1));
            let texts: Vec<String> = (0..4).map(|r| cell_text(&doc, &t, r, 0)).collect();
            assert_eq!(texts, vec!["R0", "", "R1", "R2"]);
        }

        // Case `remove_row_in_the_middle`.
        {
            let (mut doc, t) = doc_with_table();
            insert_column(&mut doc, &t, 0);
            for i in 0..3 {
                insert_row(&mut doc, &t, usize::MAX);
                set_cell_text(&mut doc, &t, i, 0, &format!("R{i}"));
            }
            remove_row(&mut doc, &t, 1);
            assert_eq!(dims(&doc, &t), (2, 1));
            let texts: Vec<String> = (0..2).map(|r| cell_text(&doc, &t, r, 0)).collect();
            assert_eq!(texts, vec!["R0", "R2"], "the middle row is the one removed");
        }

        // Case `remove_column_in_the_middle`.
        {
            let (mut doc, t) = doc_with_table();
            insert_row(&mut doc, &t, 0);
            for i in 0..3 {
                insert_column(&mut doc, &t, usize::MAX);
                set_cell_text(&mut doc, &t, 0, i, &format!("C{i}"));
            }
            remove_column(&mut doc, &t, 1);
            assert_eq!(dims(&doc, &t), (1, 2));
            let texts: Vec<String> = (0..2).map(|c| cell_text(&doc, &t, 0, c)).collect();
            assert_eq!(texts, vec!["C0", "C2"]);
        }

        // Case `set_cell_text_overwrites`.
        {
            let (mut doc, t) = doc_with_table();
            seed(&mut doc, &t, 1, 1);
            set_cell_text(&mut doc, &t, 0, 0, "first");
            set_cell_text(&mut doc, &t, 0, 0, "second");
            assert_eq!(cell_text(&doc, &t, 0, 0), "second");
        }

        // Case `stable_ids_survive_a_middle_delete`.
        {
            // Deleting a middle column must not disturb the identity/content of
            // the surviving columns (proves the order-list + stable-id design).
            let (mut doc, t) = doc_with_table();
            insert_row(&mut doc, &t, 0);
            for i in 0..4 {
                insert_column(&mut doc, &t, usize::MAX);
                set_cell_text(&mut doc, &t, 0, i, &format!("c{i}"));
            }
            let ids_before = col_ids(&doc, &t);
            remove_column(&mut doc, &t, 2);
            let ids_after = col_ids(&doc, &t);
            assert_eq!(
                ids_after,
                vec![
                    ids_before[0].clone(),
                    ids_before[1].clone(),
                    ids_before[3].clone()
                ]
            );
            let texts: Vec<String> = (0..3).map(|c| cell_text(&doc, &t, 0, c)).collect();
            assert_eq!(texts, vec!["c0", "c1", "c3"]);
        }
    }

    // ---- widget-integration tests (through the real event path) -------

    /// Run `sync` → `build_layout` → `compute_layout` → `apply_layout` at the
    /// table's natural size, returning the taffy tree + root node for event
    /// routing.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test-only layout size is a small pixel value; f32 precision loss is \
                  immaterial"
    )]
    fn layout(table: &mut Table, doc: &AutoCommit, root: &ObjId) -> (TaffyTree<()>, NodeId) {
        table.sync(doc, root);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = table.build_layout(&mut taffy, &theme, &mut nm);
        let (w, h) = table.total_size();
        taffy
            .compute_layout_with_measure(
                node,
                TSize {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    nm.get(&node_id).map_or(TSize::ZERO, |&p| unsafe {
                        (*p).measure(known, available, &mut tr)
                    })
                },
            )
            .unwrap();
        table.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    use crate::synthetic_input::cursor_moved;
    use crate::synthetic_input::mouse_press;
    fn char_key(s: &str) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            Key::Character(winit::keyboard::SmolStr::new(s)),
            Some(winit::keyboard::SmolStr::new(s)),
            ElementState::Pressed,
        ))
    }
    fn enter() -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            Key::Named(NamedKey::Enter),
            None,
            ElementState::Pressed,
        ))
    }

    fn dispatch(
        table: &mut Table,
        taffy: &TaffyTree<()>,
        node: NodeId,
        doc: &mut AutoCommit,
        root: &ObjId,
        ev: &WindowEvent,
        mods: Modifiers,
    ) -> (bool, Option<NodeId>) {
        let mut handled = false;
        let mut focus_target = None;
        table.handle_event(
            ev,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: crate::widget::Edits::lend(doc),
                path: root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy,
            },
        );
        // The owner applies what the table hands it, as an app's action does.
        for edit in table.take_edits() {
            apply(doc, root, &edit);
        }
        (handled, focus_target)
    }

    #[test]
    fn table_affordances_insert_delete_resize_and_create_cells() {
        // Case `clicking_a_middle_boundary_inserts_a_column_there`.
        {
            let (mut doc, t) = doc_with_table();
            // 1 row, 2 columns "A","B".
            insert_row(&mut doc, &t, 0);
            insert_column(&mut doc, &t, 0);
            insert_column(&mut doc, &t, 1);
            set_cell_text(&mut doc, &t, 0, 0, "A");
            set_cell_text(&mut doc, &t, 0, 1, "B");

            let mut table = Table::new("t");
            let (taffy, node) = layout(&mut table, &doc, &t);

            // Boundary between col 0 and col 1: xs[1] = LEFT_GUTTER + DEFAULT_COL_W.
            let bx = LEFT_GUTTER + DEFAULT_COL_W;
            let mods = Modifiers::default();
            dispatch(
                &mut table,
                &taffy,
                node,
                &mut doc,
                &t,
                &cursor_moved(bx, TOP_GUTTER / 2.0),
                mods,
            );
            assert_eq!(
                table.hover,
                Hover::InsertCol(1),
                "hover resolves to the middle insert pill"
            );
            let (handled, _) =
                dispatch(&mut table, &taffy, node, &mut doc, &t, &mouse_press(), mods);

            assert!(handled, "the insert click is consumed");
            assert_eq!(dims(&doc, &t), (1, 3), "a column was inserted");
            let texts: Vec<String> = (0..3).map(|c| cell_text(&doc, &t, 0, c)).collect();
            assert_eq!(
                texts,
                vec!["A", "", "B"],
                "the new column landed between A and B"
            );
        }

        // Case `clicking_a_row_delete_handle_removes_that_row`.
        {
            let (mut doc, t) = doc_with_table();
            insert_column(&mut doc, &t, 0);
            for i in 0..3 {
                insert_row(&mut doc, &t, usize::MAX);
                set_cell_text(&mut doc, &t, i, 0, &format!("R{i}"));
            }
            let mut table = Table::new("t");
            let (taffy, node) = layout(&mut table, &doc, &t);

            // Delete handle for row 1 sits in the left gutter at that row's centre.
            let cy = TOP_GUTTER + 1.5 * ROW_H;
            let mods = Modifiers::default();
            dispatch(
                &mut table,
                &taffy,
                node,
                &mut doc,
                &t,
                &cursor_moved(LEFT_GUTTER / 2.0, cy),
                mods,
            );
            assert_eq!(table.hover, Hover::DeleteRow(1));
            let (handled, _) =
                dispatch(&mut table, &taffy, node, &mut doc, &t, &mouse_press(), mods);

            assert!(handled);
            assert_eq!(dims(&doc, &t), (2, 1));
            let texts: Vec<String> = (0..2).map(|r| cell_text(&doc, &t, r, 0)).collect();
            assert_eq!(texts, vec!["R0", "R2"]);
        }

        // Case `dragging_an_interior_boundary_resizes_the_left_column`.
        {
            let (mut doc, t) = doc_with_table();
            seed(&mut doc, &t, 1, 2);
            let mut table = Table::new("t");
            let (taffy, node) = layout(&mut table, &doc, &t);

            let cid0 = col_ids(&doc, &t)[0].clone();
            let before = col_width(&doc, &t, &cid0);

            // Interior boundary between col 0 and col 1 is at xs[1]; the body row
            // sits below the top gutter.
            let bx = LEFT_GUTTER + DEFAULT_COL_W;
            let by = TOP_GUTTER + ROW_H / 2.0;
            let mods = Modifiers::default();

            dispatch(
                &mut table,
                &taffy,
                node,
                &mut doc,
                &t,
                &cursor_moved(bx, by),
                mods,
            );
            assert_eq!(
                table.hover,
                Hover::ResizeCol(1),
                "interior boundary hover is a resize"
            );
            dispatch(&mut table, &taffy, node, &mut doc, &t, &mouse_press(), mods);
            assert!(
                table.resize.is_some(),
                "press on the boundary starts a resize drag"
            );
            dispatch(
                &mut table,
                &taffy,
                node,
                &mut doc,
                &t,
                &cursor_moved(bx + 40.0, by),
                mods,
            );
            assert!(
                (col_width(&doc, &t, &cid0) - before).abs() < 0.5,
                "the column follows the pointer; the owner gets the width when the drag ends"
            );
            let release = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch(&mut table, &taffy, node, &mut doc, &t, &release, mods);

            let after = col_width(&doc, &t, &cid0);
            assert!(
                (after - (before + 40.0)).abs() < 0.5,
                "column widened by the drag delta: {before} -> {after}"
            );
        }

        // Case `empty_table_add_affordance_creates_a_cell`.
        {
            let (mut doc, t) = doc_with_table();
            let mut table = Table::new("t"); // no seed → empty
            let (taffy, node) = layout(&mut table, &doc, &t);
            assert!(table.is_empty());

            let mods = Modifiers::default();
            let r = table.empty_add_rect();
            let c = r.center();
            dispatch(
                &mut table,
                &taffy,
                node,
                &mut doc,
                &t,
                &cursor_moved(c.x, c.y),
                mods,
            );
            assert_eq!(table.hover, Hover::EmptyAdd);
            dispatch(&mut table, &taffy, node, &mut doc, &t, &mouse_press(), mods);

            assert_eq!(dims(&doc, &t), (1, 1), "empty-add seeds a single cell");
        }
    }

    #[test]
    fn cell_editing_splices_only_changed_text_and_moves_focus() {
        // Case `typing_into_a_focused_cell_reaches_the_owner_as_cell_edits`.
        {
            let (mut doc, t) = doc_with_table();
            seed(&mut doc, &t, 1, 1);
            // Clear the seeded content so we assert on exactly what we type.
            set_cell_text(&mut doc, &t, 0, 0, "");

            let mut table = Table::new("t");
            let (taffy, node) = layout(&mut table, &doc, &t);

            // Focus the only cell (emulating the App's focus dispatch).
            table.cells[0].set_focus(true);

            let mods = Modifiers::default();
            for ch in ["h", "i"] {
                dispatch(&mut table, &taffy, node, &mut doc, &t, &char_key(ch), mods);
            }
            assert_eq!(
                cell_text(&doc, &t, 0, 0),
                "hi",
                "keystrokes reach the cell's Text through the owner"
            );
        }

        // Case `enter_moves_focus_to_the_cell_below`.
        {
            let (mut doc, t) = doc_with_table();
            seed(&mut doc, &t, 2, 2);
            let mut table = Table::new("t");
            let (taffy, node) = layout(&mut table, &doc, &t);

            // Focus the top-left cell (index 0). Cell below is index n_cols == 2.
            table.cells[0].set_focus(true);
            let mods = Modifiers::default();
            let (handled, focus_target) =
                dispatch(&mut table, &taffy, node, &mut doc, &t, &enter(), mods);

            assert!(handled, "Enter is consumed for cell navigation");
            let want = taffy.children(node).unwrap()[2]; // row 1, col 0
            assert_eq!(
                focus_target,
                Some(want),
                "focus moves straight down one row"
            );
        }

        // Case `typing_into_a_cell_splices_only_the_new_text`.
        {
            // `set_cell_text` receives the cell's whole contents on every keystroke.
            // Deleting and reinserting them costs O(len) ops per key and leaves the
            // tombstones behind forever, so a long cell gets slower as it is typed.
            let (mut doc, t) = doc_with_table();
            insert_row(&mut doc, &t, 0);
            insert_column(&mut doc, &t, 0);

            set_cell_text(&mut doc, &t, 0, 0, "");
            let baseline = doc.save().len();
            let mut typed = String::new();
            for i in 0..300 {
                typed.push(if i % 9 == 8 { ' ' } else { 'x' });
                set_cell_text(&mut doc, &t, 0, 0, &typed);
            }
            let grown = doc.save().len() - baseline;

            assert_eq!(
                cell_text(&doc, &t, 0, 0),
                typed,
                "cell must hold what was typed"
            );
            assert!(
                grown < 12 * typed.len(),
                "typing {} chars grew the document by {grown} bytes — set_cell_text \
             is rewriting the cell instead of splicing the diff",
                typed.len()
            );
        }

        // Case `rewriting_a_cell_with_its_current_value_is_free`.
        {
            let (mut doc, t) = doc_with_table();
            seed(&mut doc, &t, 1, 1);
            set_cell_text(&mut doc, &t, 0, 0, "steady");
            let before = doc.save().len();

            for _ in 0..50 {
                set_cell_text(&mut doc, &t, 0, 0, "steady");
            }

            assert_eq!(cell_text(&doc, &t, 0, 0), "steady");
            assert_eq!(
                doc.save().len(),
                before,
                "re-storing an unchanged cell must not grow the document"
            );
        }
    }
}

/// The block editor’s structural table renderer.
pub(crate) type DocumentTable = Table;

//! The results grid renderer + paging/sort state. Draws a [`QueryResult`]
//! as a sortable, scrollable table with NULL styling and right-aligned
//! numerics. Kept separate from the app so both the Query tab and the Data
//! tab can reuse it.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Fill;

use crate::model::{Cell, QueryResult};
use crate::theme_ext::ThemeExt;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

pub const HEADER_H: f64 = 30.0;
pub const ROW_H: f64 = 26.0;
const CELL_PAD: f64 = 10.0;
const MIN_COL_W: f64 = 70.0;

/// View state for a grid: sort column + direction and both-axis scroll.
pub struct GridView {
    pub sort_col: Option<usize>,
    pub sort_asc: bool,
    pub scroll_row: usize,
    /// Horizontal scroll offset in logical px (for results wider than the pane).
    pub scroll_x: f64,
    /// Hovered row index (in display order), for row highlight.
    pub hover_row: Option<usize>,
    /// Cached column pixel widths (computed from content on each result set).
    col_widths: Vec<f64>,
    /// The display-order row index permutation after sorting.
    order: Vec<usize>,
    rect: Rect,
}

impl Default for GridView {
    fn default() -> Self {
        Self::new()
    }
}

impl GridView {
    pub fn new() -> Self {
        Self {
            sort_col: None,
            sort_asc: true,
            scroll_row: 0,
            scroll_x: 0.0,
            hover_row: None,
            col_widths: Vec::new(),
            order: Vec::new(),
            rect: Rect::ZERO,
        }
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Recompute column widths + sort order when a new result lands.
    pub fn bind(&mut self, result: &QueryResult, tr: &mut TextRenderer) {
        let ncols = result.columns.len();
        self.col_widths = vec![MIN_COL_W; ncols];
        for (ci, name) in result.columns.iter().enumerate() {
            let (w, _) = tr.measure(name, 13.0, None);
            self.col_widths[ci] = (f64::from(w) + 2.0 * CELL_PAD + 16.0).max(MIN_COL_W);
        }
        // Sample up to 60 rows to size columns to content.
        for row in result.rows.iter().take(60) {
            for (ci, cell) in row.iter().enumerate() {
                if ci >= ncols {
                    break;
                }
                let s = cell.display();
                let sample: String = s.chars().take(40).collect();
                let (w, _) = tr.measure(&sample, 13.0, None);
                let want = (f64::from(w) + 2.0 * CELL_PAD).max(MIN_COL_W);
                if want > self.col_widths[ci] {
                    self.col_widths[ci] = want.min(360.0);
                }
            }
        }
        self.scroll_row = 0;
        self.scroll_x = 0.0;
        self.recompute_order(result);
    }

    fn recompute_order(&mut self, result: &QueryResult) {
        let mut order: Vec<usize> = (0..result.rows.len()).collect();
        if let Some(col) = self.sort_col {
            let asc = self.sort_asc;
            order.sort_by(|&a, &b| {
                let ca = result.rows[a].get(col);
                let cb = result.rows[b].get(col);
                let ord = compare_cells(ca, cb);
                if asc { ord } else { ord.reverse() }
            });
        }
        self.order = order;
    }

    /// Click on the header of `col`: toggle/establish sort.
    pub fn sort_by(&mut self, col: usize, result: &QueryResult) {
        if self.sort_col == Some(col) {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_col = Some(col);
            self.sort_asc = true;
        }
        self.recompute_order(result);
    }

    pub fn visible_rows(&self) -> usize {
        (((self.rect.height() - HEADER_H) / ROW_H).floor()).max(0.0) as usize
    }

    pub fn scroll(&mut self, delta_rows: isize, total: usize) {
        let vis = self.visible_rows();
        let max_scroll = total.saturating_sub(vis);
        let next = self.scroll_row as isize + delta_rows;
        self.scroll_row = next.clamp(0, max_scroll as isize) as usize;
    }

    /// Scroll horizontally by `delta_px`, clamped so the last column can't be
    /// pulled past the right edge of the pane.
    pub fn scroll_h(&mut self, delta_px: f64) {
        self.scroll_x = (self.scroll_x + delta_px).clamp(0.0, self.max_scroll_x());
    }

    /// How far the content can scroll horizontally (0 when it fits).
    pub fn max_scroll_x(&self) -> f64 {
        (self.total_width() - self.rect.width()).max(0.0)
    }

    /// Total horizontal width of all columns.
    fn total_width(&self) -> f64 {
        self.col_widths.iter().sum()
    }

    /// Hit-test a header click; returns the column index if `p` is on a
    /// header cell. Accounts for the horizontal scroll offset.
    pub fn header_hit(&self, p: Point) -> Option<usize> {
        if p.y < self.rect.y0 || p.y > self.rect.y0 + HEADER_H {
            return None;
        }
        if p.x < self.rect.x0 || p.x > self.rect.x1 {
            return None;
        }
        let mut x = self.rect.x0 - self.scroll_x;
        for (ci, w) in self.col_widths.iter().enumerate() {
            if p.x >= x && p.x < x + w {
                return Some(ci);
            }
            x += w;
        }
        None
    }

    /// Hit-test a body row; returns the display-order index.
    pub fn row_hit(&self, p: Point) -> Option<usize> {
        if p.y < self.rect.y0 + HEADER_H {
            return None;
        }
        let rel = ((p.y - self.rect.y0 - HEADER_H) / ROW_H).floor() as usize;
        let idx = self.scroll_row + rel;
        if idx < self.order.len() {
            Some(idx)
        } else {
            None
        }
    }

    /// Draw the grid for `result`.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        result: &QueryResult,
    ) {
        let p = theme.db();
        let r = self.rect;
        scene.fill(Fill::NonZero, transform, p.panel_bg, None, &r);

        if result.columns.is_empty() {
            tr.draw_text(
                scene,
                if result.is_select {
                    "Query returned no columns."
                } else {
                    "Statement executed."
                },
                14.0,
                theme.text_dim,
                transform * Affine::translate((r.x0 + 16.0, r.y0 + 16.0)),
                None,
            );
            return;
        }

        let total_w = self.total_width();
        // Base X, shifted left by the horizontal scroll offset. Everything
        // inside is clipped to the grid rect so scrolled-off columns and any
        // column straddling an edge never bleed outside the pane.
        let x0 = r.x0 - self.scroll_x;
        let content_right = (x0 + total_w).min(r.x1);
        let body_bottom = (r.y0 + HEADER_H + self.visible_rows() as f64 * ROW_H).min(r.y1);

        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &r,
        );

        // Header background spans the full visible width.
        let header_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + HEADER_H);
        scene.fill(Fill::NonZero, transform, p.header_bg, None, &header_rect);

        // Column headers.
        let mut x = x0;
        for (ci, col) in result.columns.iter().enumerate() {
            let w = self.col_widths[ci];
            if x + w < r.x0 {
                x += w; // fully scrolled off the left
                continue;
            }
            if x > r.x1 {
                break;
            }
            let label = if self.sort_col == Some(ci) {
                let arrow = if self.sort_asc { " ▲" } else { " ▼" };
                format!("{col}{arrow}")
            } else {
                col.clone()
            };
            let max_w = (w - 2.0 * CELL_PAD) as f32;
            tr.draw_text(
                scene,
                &label,
                13.0,
                theme.text_primary,
                transform * Affine::translate((x + CELL_PAD, r.y0 + 8.0)),
                Some(max_w),
            );
            // Column divider.
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                p.grid_line,
                None,
                &Line::new(Point::new(x + w, r.y0), Point::new(x + w, body_bottom)),
            );
            x += w;
        }
        // Header underline.
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            theme.border_standard,
            None,
            &Line::new(
                Point::new(r.x0, r.y0 + HEADER_H),
                Point::new(content_right, r.y0 + HEADER_H),
            ),
        );

        // Body rows.
        let vis = self.visible_rows();
        for vi in 0..vis {
            let oidx = self.scroll_row + vi;
            if oidx >= self.order.len() {
                break;
            }
            let row_idx = self.order[oidx];
            let y = r.y0 + HEADER_H + vi as f64 * ROW_H;
            let row_rect = Rect::new(r.x0, y, content_right, y + ROW_H);

            // Zebra striping + hover.
            if Some(oidx) == self.hover_row {
                scene.fill(Fill::NonZero, transform, p.hover, None, &row_rect);
            } else if oidx % 2 == 1 {
                scene.fill(Fill::NonZero, transform, p.row_alt, None, &row_rect);
            }

            let row = &result.rows[row_idx];
            let mut cx = x0;
            for (ci, w) in self.col_widths.iter().enumerate() {
                if cx + w < r.x0 {
                    cx += w;
                    continue;
                }
                if cx > r.x1 {
                    break;
                }
                let cell = row.get(ci).cloned().unwrap_or(Cell::Null);
                draw_cell(scene, transform, tr, theme, &p, &cell, cx, y, *w);
                cx += w;
            }
        }

        scene.pop_layer();

        // Horizontal scrollbar when the result is wider than the pane.
        let max_x = self.max_scroll_x();
        if max_x > 0.5 {
            let track_y = r.y1 - 4.0;
            let track = Rect::new(r.x0, track_y, r.x1, track_y + 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                p.row_alt,
                None,
                &track.to_rounded_rect(1.5),
            );
            let frac = (r.width() / total_w).clamp(0.08, 1.0);
            let thumb_w = r.width() * frac;
            let travel = r.width() - thumb_w;
            let tx = r.x0 + (self.scroll_x / max_x) * travel;
            let thumb = Rect::new(tx, track_y, tx + thumb_w, track_y + 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.text_dim,
                None,
                &thumb.to_rounded_rect(1.5),
            );
        }

        // Outer border.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &r.to_rounded_rect(0.0),
        );
    }
}

fn draw_cell(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    p: &crate::theme_ext::DbPalette,
    cell: &Cell,
    x: f64,
    y: f64,
    w: f64,
) {
    let max_w = (w - 2.0 * CELL_PAD) as f32;
    let s = cell.display();
    let truncated: String = if s.chars().count() > 60 {
        let t: String = s.chars().take(57).collect();
        format!("{t}…")
    } else {
        s
    };
    let color = if cell.is_null() {
        p.null_fg
    } else if cell.is_numeric() {
        p.numeric_fg
    } else {
        theme.text_primary
    };

    let ty = y + 6.0;
    if cell.is_numeric() {
        // Right-align numerics within the cell.
        let (tw, _) = tr.measure(&truncated, 13.0, None);
        let tx = (x + w - CELL_PAD - f64::from(tw)).max(x + CELL_PAD);
        tr.draw_text(
            scene,
            &truncated,
            13.0,
            color,
            transform * Affine::translate((tx, ty)),
            None,
        );
    } else {
        tr.draw_text(
            scene,
            &truncated,
            13.0,
            color,
            transform * Affine::translate((x + CELL_PAD, ty)),
            Some(max_w),
        );
    }
}

fn compare_cells(a: Option<&Cell>, b: Option<&Cell>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => match (a, b) {
            (Cell::Null, Cell::Null) => Ordering::Equal,
            (Cell::Null, _) => Ordering::Less,
            (_, Cell::Null) => Ordering::Greater,
            _ => {
                let na = cell_number(a);
                let nb = cell_number(b);
                if let (Some(na), Some(nb)) = (na, nb) {
                    na.partial_cmp(&nb).unwrap_or(Ordering::Equal)
                } else {
                    a.display().to_lowercase().cmp(&b.display().to_lowercase())
                }
            }
        },
    }
}

fn cell_number(c: &Cell) -> Option<f64> {
    match c {
        Cell::Integer(i) => Some(*i as f64),
        Cell::Real(r) => Some(*r),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> QueryResult {
        QueryResult {
            columns: vec!["n".into(), "name".into()],
            rows: vec![
                vec![Cell::Integer(3), Cell::Text("c".into())],
                vec![Cell::Integer(1), Cell::Text("a".into())],
                vec![Cell::Null, Cell::Text("b".into())],
            ],
            is_select: true,
            ..Default::default()
        }
    }

    #[test]
    fn sort_ascending_puts_null_first_then_numeric() {
        let r = result();
        let mut g = GridView::new();
        g.sort_by(0, &r);
        // order maps display->source; first display row should be the NULL.
        assert!(r.rows[g.order[0]][0].is_null());
        assert_eq!(r.rows[g.order[1]][0], Cell::Integer(1));
        assert_eq!(r.rows[g.order[2]][0], Cell::Integer(3));
    }

    #[test]
    fn sort_toggles_direction() {
        let r = result();
        let mut g = GridView::new();
        g.sort_by(0, &r);
        assert!(g.sort_asc);
        g.sort_by(0, &r);
        assert!(!g.sort_asc);
    }
}

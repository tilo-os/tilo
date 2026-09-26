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
    /// Header band and row pitch measured from the renderer the grid paints
    /// with (`measure_bands`): the 1× constants until the 13 px cell text
    /// outgrows them, then the measured line plus padding. Paint, hit-testing
    /// and the visible-row count all read these two values.
    header_h: f64,
    row_h: f64,
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
            header_h: HEADER_H,
            row_h: ROW_H,
        }
    }

    /// Re-measure the header band and row pitch with the renderer the grid is
    /// about to paint with. Called by `bind` and by the owner before every
    /// draw, so a text-scale change moves rows, hits and scroll range together.
    pub fn measure_bands(&mut self, tr: &mut TextRenderer) {
        // Cells and header labels are 13 px (a 16.25 px line at 1×): 4 px of
        // padding either side stays inside the 26 px row and 6 px inside the
        // 30 px header, so both floors hold at 1× and only larger text grows
        // the bands.
        let (_, lh) = tr.measure("Ag", 13.0, None);
        let lh = f64::from(lh);
        self.row_h = (lh + 2.0 * 4.0).max(ROW_H);
        self.header_h = (lh + 2.0 * 6.0).max(HEADER_H);
    }

    /// The row pitch painted last (`ROW_H` at 1×).
    pub fn row_height(&self) -> f64 {
        self.row_h
    }

    /// The header band painted last (`HEADER_H` at 1×).
    pub fn header_height(&self) -> f64 {
        self.header_h
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Recompute column widths + sort order when a new result lands.
    pub fn bind(&mut self, result: &QueryResult, tr: &mut TextRenderer) {
        self.measure_bands(tr);
        let ncols = result.columns.len();
        self.col_widths = vec![MIN_COL_W; ncols];
        for (ci, name) in result.columns.iter().enumerate() {
            let (w, _) = tr.measure(name, 13.0, None);
            if let Some(cw) = self.col_widths.get_mut(ci) {
                *cw = (f64::from(w) + 2.0 * CELL_PAD + 16.0).max(MIN_COL_W);
            }
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
                let Some(cw) = self.col_widths.get_mut(ci) else {
                    continue;
                };
                if want > *cw {
                    *cw = want.min(360.0);
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
                let ca = result.rows.get(a).and_then(|row| row.get(col));
                let cb = result.rows.get(b).and_then(|row| row.get(col));
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
        (((self.rect.height() - self.header_h) / self.row_h).floor()).max(0.0) as usize
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
    /// header cell. Accounts for the horizontal scroll offset. The header
    /// band is half-open (`[y0, y0 + HEADER_H)`) so its bottom edge belongs
    /// to the first body row, exactly as painted.
    pub fn header_hit(&self, p: Point) -> Option<usize> {
        if p.y < self.rect.y0 || p.y >= self.rect.y0 + self.header_h {
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

    /// Hit-test a body row; returns the display-order index. Only rows the
    /// body actually paints (`visible_rows`) are hittable: the partial strip
    /// under the last full row is empty on screen and is empty here too.
    pub fn row_hit(&self, p: Point) -> Option<usize> {
        if p.y < self.rect.y0 + self.header_h || p.x < self.rect.x0 || p.x > self.rect.x1 {
            return None;
        }
        let rel = ((p.y - self.rect.y0 - self.header_h) / self.row_h).floor() as usize;
        if rel >= self.visible_rows() {
            return None;
        }
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

        // Everything inside is clipped to the grid rect so scrolled-off
        // columns and any column straddling an edge never bleed outside the
        // pane.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &r,
        );
        self.draw_header(scene, transform, tr, theme, result);
        self.draw_body(scene, transform, tr, theme, result);
        scene.pop_layer();

        self.draw_scrollbar(scene, transform, theme);

        // Outer border.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &r.to_rounded_rect(0.0),
        );
    }

    /// Header background, column labels/dividers and the underline beneath
    /// them. Base X is shifted left by the horizontal scroll offset.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        result: &QueryResult,
    ) {
        let p = theme.db();
        let r = self.rect;
        let x0 = r.x0 - self.scroll_x;
        let header_h = self.header_h;
        let body_bottom = (r.y0 + header_h + self.visible_rows() as f64 * self.row_h).min(r.y1);
        let content_right = (x0 + self.total_width()).min(r.x1);

        // Header background spans the full visible width.
        let header_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + header_h);
        scene.fill(Fill::NonZero, transform, p.header_bg, None, &header_rect);

        // Column headers.
        let mut x = x0;
        for (ci, col) in result.columns.iter().enumerate() {
            // A width missing for a column the driver reported degrades to
            // stopping the header here rather than indexing past the end.
            let Some(&w) = self.col_widths.get(ci) else {
                break;
            };
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
            // Header labels and body cells centre on their bands from the
            // same measured line height, so their baselines line up column
            // to column and at every text scale.
            let (_, lh) = tr.measure(&label, 13.0, None);
            tr.draw_text(
                scene,
                &label,
                13.0,
                theme.text_primary,
                transform
                    * Affine::translate((x + CELL_PAD, r.y0 + (header_h - f64::from(lh)) / 2.0)),
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
        // Header underline, derived against the header band it closes.
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            ui::surface::edge_on(p.header_bg),
            None,
            &Line::new(
                Point::new(r.x0, r.y0 + header_h),
                Point::new(content_right, r.y0 + header_h),
            ),
        );
    }

    /// Visible body rows: zebra striping, hover highlight and cells.
    fn draw_body(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        result: &QueryResult,
    ) {
        let p = theme.db();
        let r = self.rect;
        let x0 = r.x0 - self.scroll_x;
        let content_right = (x0 + self.total_width()).min(r.x1);

        let vis = self.visible_rows();
        for vi in 0..vis {
            let oidx = self.scroll_row + vi;
            // `order` is a permutation sized to the bound result; a stale
            // mismatch degrades to stopping the body here rather than
            // indexing past the end.
            let Some(&row_idx) = self.order.get(oidx) else {
                break;
            };
            let Some(row) = result.rows.get(row_idx) else {
                break;
            };
            let y = r.y0 + self.header_h + vi as f64 * self.row_h;
            let row_rect = Rect::new(r.x0, y, content_right, y + self.row_h);

            // Zebra striping + hover.
            if Some(oidx) == self.hover_row {
                scene.fill(Fill::NonZero, transform, p.hover, None, &row_rect);
            } else if oidx % 2 == 1 {
                scene.fill(Fill::NonZero, transform, p.row_alt, None, &row_rect);
            }

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
                let cell_rect = Rect::new(cx, y, cx + w, y + self.row_h);
                draw_cell(scene, transform, tr, theme, &p, &cell, cell_rect);
                cx += w;
            }
        }
    }

    /// Horizontal scrollbar, drawn only when the result is wider than the pane.
    fn draw_scrollbar(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let p = theme.db();
        let r = self.rect;
        let total_w = self.total_width();
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
    }
}

fn draw_cell(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    p: &crate::theme_ext::DbPalette,
    cell: &Cell,
    cell_rect: Rect,
) {
    let max_w = (cell_rect.width() - 2.0 * CELL_PAD) as f32;
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

    let (_, th) = tr.measure(&truncated, 13.0, None);
    let ty = cell_rect.y0 + (cell_rect.height() - f64::from(th)) / 2.0;
    if cell.is_numeric() {
        // Right-align numerics within the cell.
        let (tw, _) = tr.measure(&truncated, 13.0, None);
        let tx = (cell_rect.x1 - CELL_PAD - f64::from(tw)).max(cell_rect.x0 + CELL_PAD);
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
            transform * Affine::translate((cell_rect.x0 + CELL_PAD, ty)),
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

    /// Ascending sort puts nulls first then numerics, and sorting toggles direction.
    #[test]
    fn grid_sort_orders_nulls_first_and_toggles() {
        // --- phase: sort_ascending_puts_null_first_then_numeric ---
        {
            let r = result();
            let mut g = GridView::new();
            g.sort_by(0, &r);
            // order maps display->source; first display row should be the NULL.
            assert!(r.rows[g.order[0]][0].is_null());
            assert_eq!(r.rows[g.order[1]][0], Cell::Integer(1));
            assert_eq!(r.rows[g.order[2]][0], Cell::Integer(3));
        }

        // --- phase: sort_toggles_direction ---
        {
            let r = result();
            let mut g = GridView::new();
            g.sort_by(0, &r);
            assert!(g.sort_asc);
            g.sort_by(0, &r);
            assert!(!g.sort_asc);
        }
    }

    /// Header and body hits split where the paint does and bands match the constants at 1x with hits following the scaled pitch.
    #[test]
    fn grid_hits_follow_the_painted_scaled_bands() {
        // --- phase: header_and_body_hits_split_exactly_where_the_paint_does ---
        // The header band is `[y0, y0 + HEADER_H)`: its bottom edge is the first
        // body row, and a point under the last painted row hits nothing.
        {
            let mut tr = TextRenderer::new();
            let result = result();
            let mut grid = GridView::new();
            // Header + exactly two full rows + a partial strip.
            grid.set_rect(Rect::new(
                0.0,
                0.0,
                400.0,
                HEADER_H + 2.0 * ROW_H + ROW_H / 2.0,
            ));
            grid.bind(&result, &mut tr);
            let edge = Point::new(20.0, HEADER_H);
            assert_eq!(
                grid.header_hit(edge),
                None,
                "the header's bottom edge is body"
            );
            assert_eq!(grid.row_hit(edge), Some(0));
            assert_eq!(grid.header_hit(Point::new(20.0, HEADER_H - 0.5)), Some(0));
            assert_eq!(
                grid.row_hit(Point::new(20.0, HEADER_H + ROW_H + 1.0)),
                Some(1)
            );
            assert_eq!(
                grid.row_hit(Point::new(20.0, HEADER_H + 2.0 * ROW_H + 1.0)),
                None,
                "the unpainted partial strip under the last full row is not a row"
            );
            assert_eq!(grid.row_hit(Point::new(-1.0, HEADER_H + 1.0)), None);
        }

        // --- phase: bands_match_the_constants_at_1x_and_hits_follow_the_scaled_pitch ---
        // At 1× the measured bands are the historical constants; at 2× they grow
        // to hold the 13 px line, and the hit-test rows move with the painted
        // pitch rather than staying at the 26 px the glyphs have outgrown.
        {
            let result = result();
            let mut tr = TextRenderer::new();
            let mut grid = GridView::new();
            grid.bind(&result, &mut tr);
            assert_eq!((grid.header_height(), grid.row_height()), (HEADER_H, ROW_H));

            tr.set_text_scale(2.0);
            grid.bind(&result, &mut tr);
            let (_, lh) = tr.measure("Ag", 13.0, None);
            let (hh, rh) = (grid.header_height(), grid.row_height());
            assert!(rh >= f64::from(lh) + 2.0 * 4.0 && rh > ROW_H);
            assert!(hh >= f64::from(lh) + 2.0 * 6.0 && hh > HEADER_H);
            grid.set_rect(Rect::new(0.0, 0.0, 400.0, hh + 2.0 * rh + rh / 2.0));
            assert_eq!(grid.visible_rows(), 2);
            assert_eq!(grid.header_hit(Point::new(20.0, hh - 0.5)), Some(0));
            assert_eq!(grid.row_hit(Point::new(20.0, hh)), Some(0));
            assert_eq!(grid.row_hit(Point::new(20.0, hh + rh + 1.0)), Some(1));
            assert_eq!(
                grid.row_hit(Point::new(20.0, HEADER_H + ROW_H + 1.0)),
                Some(0),
                "the old 1× second-row y is still inside the taller first row"
            );
        }
    }
}

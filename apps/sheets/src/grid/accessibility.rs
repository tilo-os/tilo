use vello::kurbo::Rect;

use crate::model::{Addr, format_value};

use super::Grid;

/// Assistive technology receives only the on-screen virtual window. This
/// keeps the semantic tree proportional to a frame, even for a full-size
/// workbook, while the row/column positions below still describe its place in
/// the complete grid.
pub(crate) const A11Y_FALLBACK_COLS: u32 = 8;
pub(crate) const A11Y_FALLBACK_ROWS: u32 = 24;

/// A bounded, rendered slice of the grid for the custom-canvas accessibility
/// adapter. The app turns this into `AccessNode`s because it owns the
/// worksheet's display formatting and document-level semantics.
#[derive(Clone, Debug)]
pub(crate) struct AccessibleGridWindow {
    pub columns: Vec<u32>,
    pub rows: Vec<AccessibleGridRow>,
    pub total_columns: u32,
    pub total_rows: u32,
    pub bounds: Rect,
}

#[derive(Clone, Debug)]
pub(crate) struct AccessibleGridRow {
    pub index: u32,
    pub bounds: Rect,
    pub cells: Vec<AccessibleGridCell>,
}

#[derive(Clone, Debug)]
pub(crate) struct AccessibleGridCell {
    pub addr: Addr,
    pub bounds: Rect,
    pub raw: String,
    pub display: String,
    pub selected: bool,
}

/// Return the viewport-sized semantic model for screen readers. Unlike a
/// naive table export, this never walks every row/cell in a workbook: an
/// unopened million-row sheet contributes only the visible window. Before
/// the first layout, retain a small window around the active cell so an AT
/// can still describe and focus the current cell during startup.
impl Grid {
    pub(crate) fn accessibility_window(&self) -> AccessibleGridWindow {
        let columns = if self.has_layout() {
            self.visible_cols()
        } else {
            bounded_window(self.active.0, self.n_cols, A11Y_FALLBACK_COLS)
        };
        let row_indices = if self.has_layout() {
            self.visible_rows()
        } else {
            bounded_window(self.active.1, self.n_rows, A11Y_FALLBACK_ROWS)
        };
        let rows = row_indices
            .into_iter()
            .map(|row| {
                let cells = columns
                    .iter()
                    .copied()
                    .map(|col| {
                        let addr = (col, row);
                        let raw = self.raw.get(&addr).cloned().unwrap_or_default();
                        let display = if self.show_formulas && !raw.is_empty() {
                            raw.clone()
                        } else {
                            self.values.get(&addr).map_or_else(String::new, |value| {
                                format_value(
                                    value,
                                    self.fmt.get(&addr).copied().unwrap_or_default(),
                                )
                            })
                        };
                        AccessibleGridCell {
                            addr,
                            bounds: self.cell_screen_rect(col, row),
                            raw,
                            display,
                            selected: self.cell_is_selected(addr),
                        }
                    })
                    .collect();
                AccessibleGridRow {
                    index: row,
                    bounds: Rect::new(
                        self.rect.x0,
                        self.cell_screen_rect(0, row).y0,
                        self.rect.x1,
                        self.cell_screen_rect(0, row).y1,
                    ),
                    cells,
                }
            })
            .collect();
        AccessibleGridWindow {
            columns,
            rows,
            total_columns: self.n_cols,
            total_rows: self.n_rows,
            bounds: self.rect,
        }
    }
}

/// A small, active-cell-centred interval used before the grid has received a
/// layout. Saturating arithmetic keeps an outlying active cell inside the
/// published slice without making a startup accessibility request expensive.
fn bounded_window(active: u32, total: u32, limit: u32) -> Vec<u32> {
    if total == 0 || limit == 0 {
        return Vec::new();
    }
    let take = total.min(limit);
    let half = take / 2;
    let start = active
        .min(total.saturating_sub(1))
        .saturating_sub(half)
        .min(total.saturating_sub(take));
    (start..start.saturating_add(take)).collect()
}

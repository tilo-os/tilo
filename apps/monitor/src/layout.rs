//! Pure layout arithmetic for the Monitor.
//!
//! Every region the hand-drawn [`crate::app::MonitorApp`] paints is sized
//! by these functions, *not* by ad-hoc constants scattered through `draw`.
//! Pulling the geometry out here has two payoffs:
//!
//! * **Cards fit their content.** A card height is derived from the rows it
//!   must hold (`rail_spark_panel_h`, `per_core_grid`), so adding a row or a
//!   core can never leave content spilling below the background rectangle —
//!   the bug these helpers exist to kill.
//! * **It's testable.** `draw` consumes nothing the tests can't also call,
//!   so the layout invariants ("the spark card contains all three
//!   sparklines", "the per-core grid cell count equals the core count",
//!   "no process column overflows the table width") are asserted against the
//!   exact arithmetic the renderer uses — no tautological mirror.
//!
//! Nothing here touches Vello, the CRDT, or `sysinfo`; it is plain `f64`
//! geometry over [`vello::kurbo::Rect`].

use vello::kurbo::Rect;

use crate::app::SortKey;

// ---- rail (left column) cards -------------------------------------------

/// Inner padding every rail card insets its content by (matches
/// [`crate::charts::panel`]'s `pad`).
pub const RAIL_CARD_PAD: f64 = 12.0;

/// Fixed height of the processor (CPU ring) card.
pub const RAIL_RING_H: f64 = 168.0;
/// Fixed height of the memory + swap card.
pub const RAIL_MEM_H: f64 = 96.0;

/// One sparkline block inside the net/net/disk card: a label line plus the
/// 32px chart below it. Three of these stack in the card.
pub const SPARK_ROW_STRIDE: f64 = 52.0;
/// Vertical extent of a single sparkline block from its label baseline to
/// the bottom of its 32px chart (`+14` to label-to-chart gap, `+32` chart).
pub const SPARK_ROW_EXTENT: f64 = 46.0;
/// How many sparkline rows live in the net/net/disk card.
pub const SPARK_ROWS: usize = 3;

/// Height the net/net/disk sparkline card must be so its **last** sparkline
/// (the Disk row) sits fully *inside* the card rather than spilling below
/// it. Derived from the three stacked rows plus top+bottom card padding.
///
/// Three rows occupy `2 * stride + extent` of inner height (the first two
/// rows advance by `stride`, the last only needs its own `extent`); add the
/// card's top and bottom padding.
pub fn rail_spark_panel_h() -> f64 {
    let inner = (SPARK_ROWS as f64 - 1.0) * SPARK_ROW_STRIDE + SPARK_ROW_EXTENT;
    inner + 2.0 * RAIL_CARD_PAD
}

// ---- per-core utilisation grid ------------------------------------------

/// One per-core bar-gauge cell is 22px tall; rows are spaced by this stride
/// so the value label above the next bar clears the bar below it.
pub const CORE_ROW_STRIDE: f64 = 30.0;
/// Gap between grid columns / above the first grid row (matches the card
/// title-to-grid gap).
pub const CORE_GAP: f64 = 14.0;
/// Height reserved at the top of the per-core card for its title.
pub const CORE_TITLE_H: f64 = 22.0;

/// Resolved geometry of the per-core utilisation grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreGrid {
    /// Columns the grid is laid out in.
    pub cols: usize,
    /// Rows the grid wraps to.
    pub rows: usize,
}

impl CoreGrid {
    /// The number of cells the grid actually paints — one per core. Used by
    /// tests to assert the grid never drops (or invents) a core.
    pub fn cells(self) -> usize {
        self.cols * self.rows
    }
}

/// Choose a column/row count for `n` cores given the available card width.
/// Columns are capped so a cell stays at least ~150px wide (a bar gauge with
/// a `C12  88%` label needs the room) and never exceed the core count, and
/// rows are derived so **every** core gets a cell — the grid wraps to as
/// many rows as the core count demands instead of clipping the tail.
pub fn per_core_grid(n: usize, avail_w: f64) -> CoreGrid {
    let n = n.max(1);
    // ~150px minimum per cell, at least one column, never more columns than
    // cores. `avail_w` already excludes the card's inner padding.
    let by_width = ((avail_w + CORE_GAP) / (150.0 + CORE_GAP)).floor() as usize;
    let cols = by_width.clamp(1, 8).min(n);
    let rows = n.div_ceil(cols);
    CoreGrid { cols, rows }
}

/// Height the per-core card must be to hold `grid` rows of gauges plus the
/// title — so the last row of cores renders *inside* the card. This is the
/// content-driven height; the caller may still clamp it to a fraction of the
/// viewport when the core count is large (the grid then scrolls with the
/// page rather than overflowing the card).
pub fn per_core_card_h(grid: CoreGrid) -> f64 {
    CORE_TITLE_H + grid.rows as f64 * CORE_ROW_STRIDE + RAIL_CARD_PAD
}

// ---- process table columns ----------------------------------------------

/// Minimum width each process-table column needs so its header and cells
/// don't visually collide with the neighbour. The NAME column is elastic
/// (it absorbs slack) and so isn't pinned here.
pub const COL_PID_W: f64 = 60.0;
pub const COL_NAME_MIN_W: f64 = 120.0;
pub const COL_USER_W: f64 = 116.0;
pub const COL_CPU_W: f64 = 72.0;
pub const COL_MEM_W: f64 = 96.0;
/// "Threads" needs more than the old 52px so the header word and 2-3 digit
/// counts clear the right border instead of overlapping it.
pub const COL_THR_W: f64 = 64.0;
/// Inner horizontal padding inside the table card.
pub const TABLE_PAD: f64 = 12.0;

/// One resolved process-table column.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub key: SortKey,
    pub header: &'static str,
    /// Left edge in the table's *content* coordinate space (x grows right,
    /// starting at the left padding inset). Add the horizontal scroll offset
    /// at paint time.
    pub x0: f64,
    pub w: f64,
    /// Right-aligned (numeric) vs left-aligned (text).
    pub right: bool,
}

impl Column {
    pub fn x1(&self) -> f64 {
        self.x0 + self.w
    }
}

/// Resolve the six process-table columns for a content area `avail_w` wide
/// (already padding-inset). The NAME column flexes to fill slack, but every
/// column keeps its minimum width — so when the window is too narrow for all
/// six minimums the total content width *exceeds* `avail_w` and the table
/// must scroll horizontally rather than overlap its border.
pub fn process_columns(avail_w: f64) -> [Column; 6] {
    let fixed = COL_PID_W + COL_USER_W + COL_CPU_W + COL_MEM_W + COL_THR_W;
    // NAME takes the slack but never drops below its minimum; when there is
    // no slack the columns sum past `avail_w` and the table scrolls.
    let name_w = (avail_w - fixed).max(COL_NAME_MIN_W);
    let mut x = 0.0;
    let mut col = |key: SortKey, header: &'static str, w: f64, right: bool| {
        let c = Column { key, header, x0: x, w, right };
        x += w;
        c
    };
    [
        col(SortKey::Pid, "PID", COL_PID_W, true),
        col(SortKey::Name, "NAME", name_w, false),
        col(SortKey::User, "USER", COL_USER_W, false),
        col(SortKey::Cpu, "CPU%", COL_CPU_W, true),
        col(SortKey::Mem, "MEM", COL_MEM_W, true),
        col(SortKey::Threads, "THR", COL_THR_W, true),
    ]
}

/// Total width every process column occupies — the table's intrinsic
/// content width. When this exceeds the table's inner width the table needs
/// a horizontal scrollbar.
pub fn process_columns_total_w(cols: &[Column]) -> f64 {
    cols.last().map(Column::x1).unwrap_or(0.0)
}

/// Inset the rail-card / panel rectangle by its padding, matching
/// [`crate::charts::panel`]. Handy for tests that need the *inner* content
/// box without invoking the renderer.
pub fn inner(rect: Rect, pad: f64) -> Rect {
    Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spark_card_contains_all_three_sparklines() {
        // The card is positioned at an arbitrary y; its inner content top is
        // `y + pad`, and the last (Disk) sparkline's chart bottom must sit at
        // or above the card's inner bottom — i.e. nothing spills out.
        let y = 240.0_f64;
        let h = rail_spark_panel_h();
        let card = Rect::new(0.0, y, 248.0, y + h);
        let ci = inner(card, RAIL_CARD_PAD);
        // Disk row label baseline + its chart bottom (see `draw_rail`).
        let last_row_top = ci.y0 + (SPARK_ROWS as f64 - 1.0) * SPARK_ROW_STRIDE;
        let last_chart_bottom = last_row_top + SPARK_ROW_EXTENT;
        assert!(
            last_chart_bottom <= ci.y1 + 0.001,
            "disk sparkline bottom {last_chart_bottom} must fit inside card inner bottom {}",
            ci.y1
        );
    }

    #[test]
    fn per_core_grid_cell_count_equals_core_count_for_every_size() {
        // Whatever the core count and width, every core gets exactly one
        // cell — the grid wraps to enough rows, it never clips the tail.
        for &n in &[1usize, 2, 4, 6, 8, 12, 16, 24, 32, 64, 128] {
            for &w in &[200.0_f64, 420.0, 640.0, 900.0, 1400.0] {
                let g = per_core_grid(n, w);
                assert!(g.cols >= 1 && g.cols <= n, "cols in range for n={n} w={w}");
                assert!(
                    g.cells() >= n,
                    "grid must have a cell for every core: n={n} w={w} cells={}",
                    g.cells()
                );
                // And no more than one wasted row.
                assert!(
                    g.cells() < n + g.cols,
                    "grid must not waste a whole row: n={n} w={w}"
                );
            }
        }
    }

    #[test]
    fn per_core_card_grows_with_core_count() {
        let g4 = per_core_grid(4, 420.0);
        let g32 = per_core_grid(32, 420.0);
        assert!(
            per_core_card_h(g32) > per_core_card_h(g4),
            "32 cores must need a taller card than 4"
        );
        // The card holds exactly `rows` strides of gauges: the last row's
        // bottom must sit inside the card.
        let g = per_core_grid(33, 420.0);
        let card_h = per_core_card_h(g);
        let last_row_bottom = CORE_TITLE_H + g.rows as f64 * CORE_ROW_STRIDE;
        assert!(
            last_row_bottom <= card_h + 0.001,
            "last core row bottom {last_row_bottom} must fit card {card_h}"
        );
    }

    #[test]
    fn process_columns_never_overlap_within_their_width() {
        // Wide: NAME flexes, every column right edge ≤ next column left edge,
        // and the total fits the available width.
        let cols = process_columns(900.0);
        for w in cols.windows(2) {
            assert!(
                w[0].x1() <= w[1].x0 + 0.001,
                "column {} overruns {}",
                w[0].header,
                w[1].header
            );
        }
        assert!(
            process_columns_total_w(&cols) <= 900.0 + 0.001,
            "wide table must fit without scrolling"
        );
    }

    #[test]
    fn narrow_table_overflows_so_it_must_scroll_not_overlap() {
        // Too narrow for all six minimums: NAME pins to its minimum and the
        // total exceeds the width — the renderer must scroll, not squeeze
        // THR over the border.
        let avail = 300.0;
        let cols = process_columns(avail);
        let total = process_columns_total_w(&cols);
        assert!(
            total > avail,
            "narrow columns must overflow the width ({total} vs {avail}) so a \
             horizontal scrollbar appears instead of THR overlapping the border"
        );
        // Even overflowing, columns stay in order and don't overlap each
        // other — they just extend past the right edge into scrollable space.
        for w in cols.windows(2) {
            assert!(w[0].x1() <= w[1].x0 + 0.001, "columns stay non-overlapping");
        }
        // THR keeps its full minimum width (it isn't the column that gets
        // squeezed — NAME absorbs/pins the slack).
        assert_eq!(cols[5].w, COL_THR_W, "THR keeps its width under overflow");
    }
}

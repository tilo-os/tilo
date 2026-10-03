//! Pure layout arithmetic for the Monitor.
//!
//! Every region the hand-drawn [`crate::app::MonitorApp`] paints is sized
//! by these functions, *not* by ad-hoc constants scattered through `draw`.
//! Pulling the geometry out here has two payoffs:
//!
//! * **Cards fit their content.** A card height is derived from the rows it
//!   must hold (`per_core_card_h`, `summary_strip_h`, `overview_h`), so
//!   adding a row or a core can never leave content spilling below the
//!   background rectangle — the bug these helpers exist to kill.
//! * **It's testable.** `draw` consumes nothing the tests can't also call,
//!   so the layout invariants ("the summary strip never orphans a cell",
//!   "the per-core grid cell count equals the core count", "no process column
//!   overflows the table width") are asserted against the exact arithmetic
//!   the renderer uses — no tautological mirror.
//!
//! Nothing here touches Vello, the CRDT, or `sysinfo`; it is plain `f64`
//! geometry over [`vello::kurbo::Rect`].

use vello::kurbo::Rect;

use crate::app::SortKey;

// ---- shared card padding -------------------------------------------------

/// Inner padding a content card insets its content by (matches
/// [`crate::paint::panel`]'s `pad`). Used by the per-core grid and the
/// Resources dashboard cards.
pub const RAIL_CARD_PAD: f64 = 12.0;

/// Gap between cards / grid cells, mirrored from the app's outer `GAP` so the
/// wrap-height arithmetic here matches the renderer's spacing.
const GAP: f64 = 14.0;

// ---- summary strip (Processes tab) --------------------------------------
//
// The Processes tab carries a compact horizontal *summary strip* of the four
// headline system stats (CPU / Memory / Network / Disk) — a sparkline plus a
// number each — above a full-width process table. The strip is content, not
// a navigation rail: it wraps to fewer columns as the window narrows instead
// of stealing a fixed left gutter.

/// The four stats the summary strip always shows.
pub const STRIP_CELLS: usize = 4;
/// Height of one summary-strip stat cell (label row + sparkline + padding).
pub const STRIP_CELL_H: f64 = 72.0;
/// Inner padding of a summary-strip cell.
pub const STRIP_CELL_PAD: f64 = 10.0;
/// Minimum width one strip cell keeps before the strip wraps to fewer
/// columns — enough for a `Network  6.7 MB/s` header plus its sparkline.
pub const STRIP_CELL_MIN_W: f64 = 170.0;

/// Resolved wrap geometry of the summary strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripGrid {
    pub cols: usize,
    pub rows: usize,
}

/// Geometry of the four-cell summary strip. The strip stays a **single row**
/// at every width: its height is constant, so the process table below it keeps
/// the available vertical space instead of losing height as the window narrows.
/// The strip adapts *horizontally* — the four cells share the width and each
/// stacks its value under its label when it gets too narrow for both on one
/// line (see `draw_summary_strip`) — mirroring the "adapt horizontally, don't
/// collapse vertically" rule the process table itself follows.
pub fn summary_grid(_avail_w: f64) -> StripGrid {
    StripGrid {
        cols: STRIP_CELLS,
        rows: 1,
    }
}

/// Height the summary strip occupies once wrapped to `g`.
pub fn summary_strip_h(g: StripGrid) -> f64 {
    g.rows as f64 * STRIP_CELL_H + (g.rows as f64 - 1.0) * GAP
}

// ---- Resources dashboard reflow -----------------------------------------
//
// The Resources tab is a real dashboard: an overview row (Processor / Memory
// / System cards), a full-width per-core grid, then a chart grid (CPU /
// Memory / Network / Disk). Every band reflows its column count with the
// window width so nothing overlaps and the stats stay content, never a rail.

/// Fixed height of an overview card when the three sit abreast.
pub const OVERVIEW_H: f64 = 168.0;
/// Natural height of the overview Processor card when the row stacks.
pub const OVERVIEW_RING_H: f64 = 168.0;
/// Natural height of the overview Memory card when the row stacks.
pub const OVERVIEW_MEM_H: f64 = 116.0;
/// Natural height of the overview System card when the row stacks — tall
/// enough for the four host lines plus the CPU-brand line beneath them.
pub const OVERVIEW_SYS_H: f64 = 150.0;

/// Columns the overview row uses: three abreast when wide, else a single
/// full-width stacked column.
pub fn overview_cols(avail_w: f64) -> usize {
    if avail_w >= 720.0 { 3 } else { 1 }
}

/// Height the overview row occupies for `avail_w` — one card tall when the
/// three sit abreast, or the sum of their natural heights when stacked.
pub fn overview_h(avail_w: f64) -> f64 {
    if overview_cols(avail_w) == 3 {
        OVERVIEW_H
    } else {
        OVERVIEW_RING_H + GAP + OVERVIEW_MEM_H + GAP + OVERVIEW_SYS_H
    }
}

/// Columns the Resources chart grid uses: two-up when wide, a single column
/// when narrow so each chart stays legible.
pub fn charts_cols(avail_w: f64) -> usize {
    if avail_w >= 620.0 { 2 } else { 1 }
}

/// Rows the four-chart Resources grid wraps to for `avail_w`.
pub fn charts_rows(avail_w: f64) -> usize {
    4usize.div_ceil(charts_cols(avail_w))
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
///
/// Once the width has fixed the row count, the cores are spread evenly over
/// those rows: eight threads on a card wide enough for seven columns paint as
/// a 4 × 2 grid, not a full row of seven with one orphan gauge beneath it.
/// The card height (`per_core_card_h`) only depends on the row count, so
/// balancing never costs vertical space.
pub fn per_core_grid(n: usize, avail_w: f64) -> CoreGrid {
    let n = n.max(1);
    // ~150px minimum per cell, at least one column, never more columns than
    // cores. `avail_w` already excludes the card's inner padding.
    let by_width = ((avail_w + CORE_GAP) / (150.0 + CORE_GAP)).floor() as usize;
    let max_cols = by_width.clamp(1, 8).min(n);
    let rows = n.div_ceil(max_cols);
    // ceil(n / ceil(n / max_cols)) never exceeds max_cols, so this is the
    // fewest columns that still seat every core in the width-derived rows.
    let cols = n.div_ceil(rows);
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
/// don't visually collide with the neighbour. The regular NAME column is
/// elastic (it absorbs slack) and so isn't pinned here.
pub const COL_PID_W: f64 = 60.0;
pub const COL_NAME_MIN_W: f64 = 120.0;
/// In the compact table, process identity matters more than a generous name
/// column. The label can truncate, but it must leave enough of a name for a
/// user to recognize a row before opening its detail pane.
pub const COL_NAME_COMPACT_MIN_W: f64 = 96.0;
/// At the last useful width the table preserves just process identity and
/// current CPU use. This smaller name floor avoids turning that primary view
/// into a horizontally-scrolled table.
pub const COL_NAME_MICRO_MIN_W: f64 = 88.0;
pub const COL_USER_W: f64 = 116.0;
pub const COL_CPU_W: f64 = 72.0;
pub const COL_MEM_W: f64 = 96.0;
/// "Threads" needs more than the old 52px so the header word and 2-3 digit
/// counts clear the right border instead of overlapping it.
pub const COL_THR_W: f64 = 64.0;
/// Inner horizontal padding inside the table card.
pub const TABLE_PAD: f64 = 12.0;

/// The table's content width at or above which **all six** columns — USER
/// included — fit without a horizontal scroll: the sum of every column's
/// minimum width.
pub const ALL_COLUMNS_MIN_W: f64 =
    COL_PID_W + COL_NAME_MIN_W + COL_USER_W + COL_CPU_W + COL_MEM_W + COL_THR_W;

/// The table's content width at or above which the five **essential** columns
/// (PID / NAME / CPU% / MEM / THR — USER dropped) fit without a horizontal
/// scroll. Below this the renderer changes to a compact primary-data view.
pub const ESSENTIAL_COLUMNS_MIN_W: f64 =
    COL_PID_W + COL_NAME_MIN_W + COL_CPU_W + COL_MEM_W + COL_THR_W;
/// Compact primary scan: process name, CPU and resident memory. Used below
/// the width where PID and thread count would force horizontal scrolling.
pub const COMPACT_COLUMNS_MIN_W: f64 = COL_NAME_COMPACT_MIN_W + COL_CPU_W + COL_MEM_W;
/// Smallest useful table: process name and CPU. This is deliberately a
/// vertical-detail flow rather than a horizontally-scrollable spreadsheet.
pub const MICRO_COLUMNS_MIN_W: f64 = COL_NAME_MICRO_MIN_W + COL_CPU_W;

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

/// Resolve the process-table columns for a content area `avail_w` wide
/// (already padding-inset). The NAME column flexes to fill slack, but every
/// column keeps its minimum width.
///
/// **Column priority.** A wide table shows every field. As width falls, it
/// first drops USER, then moves to the three signals needed to scan a process
/// list (NAME / CPU / MEM), then NAME / CPU, and finally NAME alone.
/// Selecting a row reveals the complete detail pane, so compact widths never
/// require horizontal scrolling merely to learn which process is busy.
pub fn process_columns(avail_w: f64) -> Vec<Column> {
    type ColumnSpec = (SortKey, &'static str, f64, bool);
    // (key, header, width, right-aligned). NAME absorbs any available slack.
    let (fixed, name_min, specs): (f64, f64, Vec<ColumnSpec>) = if avail_w >= ALL_COLUMNS_MIN_W {
        (
            COL_PID_W + COL_USER_W + COL_CPU_W + COL_MEM_W + COL_THR_W,
            COL_NAME_MIN_W,
            vec![
                (SortKey::Pid, "PID", COL_PID_W, true),
                (SortKey::Name, "NAME", 0.0, false),
                (SortKey::User, "USER", COL_USER_W, false),
                (SortKey::Cpu, "CPU%", COL_CPU_W, true),
                (SortKey::Mem, "MEM", COL_MEM_W, true),
                (SortKey::Threads, "THR", COL_THR_W, true),
            ],
        )
    } else if avail_w >= ESSENTIAL_COLUMNS_MIN_W {
        (
            COL_PID_W + COL_CPU_W + COL_MEM_W + COL_THR_W,
            COL_NAME_MIN_W,
            vec![
                (SortKey::Pid, "PID", COL_PID_W, true),
                (SortKey::Name, "NAME", 0.0, false),
                (SortKey::Cpu, "CPU%", COL_CPU_W, true),
                (SortKey::Mem, "MEM", COL_MEM_W, true),
                (SortKey::Threads, "THR", COL_THR_W, true),
            ],
        )
    } else if avail_w >= COMPACT_COLUMNS_MIN_W {
        (
            COL_CPU_W + COL_MEM_W,
            COL_NAME_COMPACT_MIN_W,
            vec![
                (SortKey::Name, "PROCESS", 0.0, false),
                (SortKey::Cpu, "CPU%", COL_CPU_W, true),
                (SortKey::Mem, "MEM", COL_MEM_W, true),
            ],
        )
    } else if avail_w >= MICRO_COLUMNS_MIN_W {
        (
            COL_CPU_W,
            COL_NAME_MICRO_MIN_W,
            vec![
                (SortKey::Name, "PROCESS", 0.0, false),
                (SortKey::Cpu, "CPU%", COL_CPU_W, true),
            ],
        )
    } else {
        // The window is too narrow even for a readable numeric column.
        // Keep the row selectable and identify the process rather than
        // introducing a horizontal-scroll-only layout.
        (0.0, 1.0, vec![(SortKey::Name, "PROCESS", 0.0, false)])
    };
    let name_w = (avail_w - fixed).max(name_min);

    let mut x = 0.0;
    specs
        .into_iter()
        .map(|(key, header, mut w, right)| {
            if key == SortKey::Name {
                w = name_w;
            }
            let c = Column {
                key,
                header,
                x0: x,
                w,
                right,
            };
            x += w;
            c
        })
        .collect()
}

/// Total width every process column occupies — the table's intrinsic
/// content width. When this exceeds the table's inner width the table needs
/// a horizontal scrollbar.
pub fn process_columns_total_w(cols: &[Column]) -> f64 {
    cols.last().map_or(0.0, Column::x1)
}

/// Inset the rail-card / panel rectangle by its padding, matching
/// [`crate::paint::panel`]. Handy for tests that need the *inner* content
/// box without invoking the renderer.
pub fn inner(rect: Rect, pad: f64) -> Rect {
    Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_strip_and_dashboard_reflow_with_width() {
        // Phase `summary_strip_stays_one_row_so_table_keeps_height`.
        {
            // The strip is a single row at EVERY width — its height is constant, so
            // narrowing the window can never steal vertical space from the process
            // table below it. All four cells sit on that one row; the strip adapts
            // horizontally (cells share the width), not vertically.
            let base_h = summary_strip_h(summary_grid(1200.0));
            for &w in &[300.0_f64, 460.0, 520.0, 680.0, 772.0, 1200.0] {
                let g = summary_grid(w);
                assert_eq!(g.rows, 1, "w={w}: the strip must stay a single row");
                assert_eq!(g.cols, STRIP_CELLS, "w={w}: all four cells on the one row");
                assert_eq!(
                    g.cols * g.rows,
                    STRIP_CELLS,
                    "w={w}: holds exactly every cell"
                );
                assert!(
                    (summary_strip_h(g) - base_h).abs() < f64::EPSILON,
                    "w={w}: strip height must be constant across widths"
                );
            }
        }

        // Phase `dashboard_reflows_columns_with_width`.
        {
            // Wide: overview three abreast, charts two-up.
            assert_eq!(overview_cols(1200.0), 3);
            assert_eq!(charts_cols(1200.0), 2);
            // Medium (an 800px window's content band): still the roomy dashboard.
            assert_eq!(overview_cols(772.0), 3);
            assert_eq!(charts_cols(772.0), 2);
            // Narrow: everything stacks to a single column so nothing is squeezed.
            assert_eq!(overview_cols(532.0), 1);
            assert_eq!(charts_cols(532.0), 1);
            // Stacking the overview makes it taller than the abreast row, and the
            // chart grid gains rows — both feed the page-scroll height.
            assert!(overview_h(532.0) > overview_h(1200.0));
            assert_eq!(charts_rows(1200.0), 2);
            assert_eq!(charts_rows(532.0), 4);
        }
    }

    #[test]
    fn per_core_grid_seats_every_core_in_balanced_rows() {
        // Phase `per_core_grid_cell_count_equals_core_count_for_every_size`.
        {
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

        // Phase `per_core_grid_balances_rows_instead_of_orphaning_the_last_core`.
        {
            // A 1172px card (the 1200px window's Resources band) fits seven
            // 150px cells, so eight threads used to paint as 7 + 1. The width
            // still forces two rows; the cores are then split 4 + 4.
            let g = per_core_grid(8, 1172.0);
            assert_eq!((g.cols, g.rows), (4, 2), "8 cores: {g:?}");
            // 24 threads: 7 columns would need 4 rows (7/7/7/3); 6 columns fill
            // the same 4 rows exactly.
            let g = per_core_grid(24, 1172.0);
            assert_eq!((g.cols, g.rows), (6, 4), "24 cores: {g:?}");
            // Balancing never adds a row or exceeds the width-derived column cap,
            // and it uses the fewest columns that still seat every core in those
            // rows — so the trailing row is as full as a fixed-column grid allows.
            for &n in &[2usize, 3, 5, 6, 7, 9, 12, 13, 16, 32, 33, 64] {
                for &w in &[300.0_f64, 640.0, 900.0, 1172.0, 1600.0] {
                    let g = per_core_grid(n, w);
                    let cap = (((w + CORE_GAP) / (150.0 + CORE_GAP)).floor() as usize)
                        .clamp(1, 8)
                        .min(n);
                    assert!(g.cols <= cap, "n={n} w={w}: {g:?} exceeds width cap {cap}");
                    assert_eq!(
                        g.rows,
                        n.div_ceil(cap),
                        "n={n} w={w}: row count is width-derived"
                    );
                    assert!(g.cells() >= n, "n={n} w={w}: {g:?} seats every core");
                    assert!(
                        (g.cols - 1) * g.rows < n,
                        "n={n} w={w}: {g:?} uses more columns than the rows need"
                    );
                }
            }
        }

        // Phase `per_core_card_grows_with_core_count`.
        {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn process_columns_fit_without_horizontal_scroll_at_every_width() {
        // Phase `process_columns_never_overlap_within_their_width`.
        {
            // Wide: all six columns, NAME flexes, every column right edge ≤ next
            // column left edge, and the total fits the available width.
            let cols = process_columns(900.0);
            assert_eq!(cols.len(), 6, "wide table shows every column");
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

        // Phase `narrow_hides_user_so_threads_reads_without_hscroll`.
        {
            // Between the five-essential minimum and the all-six minimum the table
            // hides USER (the droppable text column) so PID / NAME / CPU% / MEM /
            // THR all fit WITHOUT a horizontal scroll. THR is a core numeric
            // column and must never be the one pushed off the right edge — the
            // "THR clipped to 'T' at 560px" defect.
            for &avail in &[
                ESSENTIAL_COLUMNS_MIN_W,
                460.0,
                500.0, // ≈ a 552px window's table band (window − 52px chrome)
                508.0, // ≈ a 560px window's table band
                ALL_COLUMNS_MIN_W - 1.0,
            ] {
                let cols = process_columns(avail);
                assert!(
                    !cols.iter().any(|c| c.key == SortKey::User),
                    "USER hidden at avail={avail} (too narrow for all six)"
                );
                let thr = cols
                    .iter()
                    .find(|c| c.key == SortKey::Threads)
                    .expect("THR is always shown");
                assert_eq!(
                    thr.w, COL_THR_W,
                    "THR keeps its full width at avail={avail}"
                );
                // The whole table fits: THR's right edge is inside the viewport, so
                // its header reads in full with no horizontal scroll.
                assert!(
                    process_columns_total_w(&cols) <= avail + 0.001,
                    "table fits without h-scroll at avail={avail}: total={} thr.x1={}",
                    process_columns_total_w(&cols),
                    thr.x1()
                );
            }
        }

        // Phase `wide_shows_user_column`.
        {
            for &avail in &[ALL_COLUMNS_MIN_W, 720.0, 1200.0] {
                let cols = process_columns(avail);
                assert!(
                    cols.iter().any(|c| c.key == SortKey::User),
                    "USER shown when the table is wide enough (avail={avail})"
                );
                assert_eq!(cols.len(), 6, "all six columns at avail={avail}");
            }
        }

        // Phase `compact_table_keeps_primary_signals_without_horizontal_scroll`.
        {
            // Below the full five-column table, the renderer becomes a compact
            // process list instead of requiring a sideways gesture before the
            // user can see which process is consuming CPU or memory.
            for &(avail, expected) in &[
                (
                    COMPACT_COLUMNS_MIN_W,
                    &[SortKey::Name, SortKey::Cpu, SortKey::Mem][..],
                ),
                (300.0, &[SortKey::Name, SortKey::Cpu, SortKey::Mem][..]),
                (MICRO_COLUMNS_MIN_W, &[SortKey::Name, SortKey::Cpu][..]),
                (220.0, &[SortKey::Name, SortKey::Cpu][..]),
                (100.0, &[SortKey::Name][..]),
            ] {
                let cols = process_columns(avail);
                let keys: Vec<_> = cols.iter().map(|col| col.key).collect();
                assert_eq!(keys, expected, "compact column order at avail={avail}");
                assert!(
                    process_columns_total_w(&cols) <= avail + 0.001,
                    "primary compact table fits at avail={avail}"
                );
            }
        }
    }
}

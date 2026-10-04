//! The hex view's geometry: one [`Geo`] per frame, computed from the window
//! rect, the theme and the measured monospace advance.
//!
//! Paint, hit-testing and keyboard paging all read the same [`Geo`], so a
//! byte's painted cell is its click target at every text scale. The view
//! recomputes it whenever the window, the theme or the chrome (the inspector
//! drawer, the find band) changes, and from the draw path before painting.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::scrollbar::ScrollMetrics;
use ui::widget::{ControlSize, SegmentedFace};
use vello::kurbo::{Point, Rect};

use super::HexView;
use crate::model::{EditMode, Endian};

/// Most bytes a row ever shows, and the fewest.
const MAX_BYTES_PER_ROW: usize = 64;
const MIN_BYTES_PER_ROW: usize = 8;
/// Rows hold a whole number of these, so a row never ends mid-group.
const ROW_STEP: usize = 4;
/// The row length the inspector must leave room for before it docks: below
/// this the inspector is a drawer and the grid keeps the whole window.
const DOCKED_BYTES_PER_ROW: usize = 16;
/// How far the gaps of a row may stretch or shrink so the row fills the
/// width it is given.
const GAP_SCALE: (f64, f64) = (0.5, 2.0);
/// Bytes between the extra group gaps of the hex columns.
pub(super) const GROUP: usize = 8;

/// Labels of the edit-mode strip, in segment order (see [`EditMode`]).
pub(super) const MODE_LABELS: [&str; 2] = ["Overwrite", "Insert"];
/// Labels of the endianness strip, in segment order (see [`Endian`]).
pub(super) const ENDIAN_LABELS: [&str; 2] = ["Little endian", "Big endian"];

/// Segment index of an edit mode in the strip.
pub(super) fn mode_index(mode: EditMode) -> usize {
    match mode {
        EditMode::Overwrite => 0,
        EditMode::Insert => 1,
    }
}

/// Segment index of an endianness in the strip.
pub(super) fn endian_index(endian: Endian) -> usize {
    match endian {
        Endian::Little => 0,
        Endian::Big => 1,
    }
}

/// The monospace metrics every grid pitch derives from: one glyph advance
/// and one line height, measured with the exact style the cells are painted
/// in (the `Code` role).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CellMetrics {
    /// Advance of one monospace glyph.
    pub glyph_w: f64,
    /// Line height of one row of cell text.
    pub line_h: f64,
}

impl CellMetrics {
    /// Metrics before the first draw has measured anything: the 0.6 em
    /// advance and 1.5 leading the `Code` role resolves to at 1×.
    pub const FALLBACK: Self = Self {
        glyph_w: 7.8,
        line_h: 19.5,
    };

    /// Measure the `Code` role with the renderer that will paint it. Eight
    /// glyphs — the offset gutter's width — so a fractional advance is read
    /// at the precision the gutter needs.
    pub fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let (w, h) = tr.measure_style("00000000", None, &theme.text(TextRole::Code));
        if w <= 0.0 || h <= 0.0 {
            return Self::FALLBACK;
        }
        Self {
            glyph_w: f64::from(w) / 8.0,
            line_h: f64::from(h),
        }
    }
}

/// The widths a row of `n` bytes is made of: glyphs that never change and
/// gaps that stretch so the row fills the content width.
struct RowPlan {
    /// Padding before the offset column and after the ASCII column (which
    /// keeps the scroll bar's lane clear).
    left: f64,
    right: f64,
    /// One ASCII cell: a glyph advance and a hairline of air, so a cursor box
    /// around one glyph never touches its neighbours.
    ascii_gap: f64,
    ascii_pitch: f64,
    gutter_w: f64,
    byte_w: f64,
    /// The gaps at their natural size.
    byte_gap: f64,
    group_gap: f64,
    col_gap: f64,
}

impl RowPlan {
    fn new(cell: CellMetrics, theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let s = |v: f32| f64::from(v);
        let ascii_gap = s(sp.s1) / 2.0;
        Self {
            left: s(sp.s4),
            right: s(theme.tokens.metrics.scrollbar_gutter) + s(sp.s1),
            ascii_gap,
            ascii_pitch: cell.glyph_w + ascii_gap,
            gutter_w: cell.glyph_w * 8.0,
            byte_w: cell.glyph_w * 2.0,
            byte_gap: s(sp.s2),
            group_gap: s(sp.s2),
            col_gap: s(sp.s6),
        }
    }

    /// Glyph widths and padding of an `n`-byte row.
    fn fixed(&self, n: usize) -> f64 {
        self.left + self.gutter_w + n as f64 * (self.byte_w + self.ascii_pitch) + self.right
    }

    /// The gaps of an `n`-byte row at their natural size.
    fn gaps(&self, n: usize) -> f64 {
        n.saturating_sub(1) as f64 * self.byte_gap
            + (n.saturating_sub(1) / GROUP) as f64 * self.group_gap
            + 2.0 * self.col_gap
    }

    /// The natural width of an `n`-byte row.
    fn width(&self, n: usize) -> f64 {
        self.fixed(n) + self.gaps(n)
    }
}

/// Where everything is this frame. All in window coordinates.
#[derive(Clone, Debug, Default)]
pub(super) struct Geo {
    /// Everything but a docked inspector.
    pub content: Rect,
    /// The content's header row: the file name and the edit-mode strip.
    pub header: Rect,
    /// The find band, or [`Rect::ZERO`] while find is closed.
    pub find: Rect,
    /// The row of column labels above the grid.
    pub column_head: Rect,
    /// The rows of bytes.
    pub grid: Rect,
    /// The scroll bar's track, along the grid's trailing edge.
    pub scroll_track: Rect,
    /// The persistent status band.
    pub status: Rect,
    /// The inspector panel, docked or floating (off-screen when hidden).
    pub inspector: Rect,
    /// Whether the inspector shows this frame.
    pub inspector_visible: bool,
    /// Whether the inspector docks beside the content (rather than floating).
    pub docked: bool,
    /// Height of the header row, the same for the content and the inspector.
    pub header_h: f64,
    /// The show/hide control of the inspector.
    pub toggle: Rect,
    /// The edit-mode strip.
    pub mode: Rect,
    /// The endianness strip, at the top of the inspector.
    pub endian: Rect,
    /// Where the inspector's properties start.
    pub properties: Rect,
    /// Where the status bubble and toasts float: the content between the
    /// header and the status bar.
    pub bubble_area: Rect,
    /// Row pitch and the measured line height inside it.
    pub row_h: f64,
    pub line_h: f64,
    /// One monospace glyph.
    pub glyph_w: f64,
    /// One ASCII cell (a glyph and the air around it) and that air.
    pub ascii_pitch: f64,
    pub ascii_gap: f64,
    /// Two glyphs: one byte's hex digits.
    pub byte_w: f64,
    /// Space between two bytes' digits.
    pub byte_gap: f64,
    /// Extra space between groups of [`GROUP`] bytes.
    pub group_gap: f64,
    pub gutter_x0: f64,
    pub hex_x0: f64,
    pub ascii_x0: f64,
    pub ascii_x1: f64,
    /// Where the rules between the three columns run.
    pub rule_hex: f64,
    pub rule_ascii: f64,
    pub visible_rows: usize,
}

impl Geo {
    /// Left edge of byte `c`'s hex digits.
    pub fn hex_x(&self, c: usize) -> f64 {
        self.hex_x0 + c as f64 * (self.byte_w + self.byte_gap) + (c / GROUP) as f64 * self.group_gap
    }

    /// Left edge of byte `c`'s ASCII glyph.
    pub fn ascii_x(&self, c: usize) -> f64 {
        self.ascii_x0 + c as f64 * self.ascii_pitch + self.ascii_gap / 2.0
    }

    /// Left edge of byte `c`'s ASCII cell.
    pub fn ascii_cell_x(&self, c: usize) -> f64 {
        self.ascii_x0 + c as f64 * self.ascii_pitch
    }

    /// The column under `x` in the hex region.
    pub fn hex_column_at(&self, x: f64, bytes_per_row: usize) -> Option<usize> {
        let half = self.byte_gap / 2.0;
        (0..bytes_per_row)
            .find(|&c| x >= self.hex_x(c) - half && x < self.hex_x(c) + self.byte_w + half)
    }

    /// The column under `x` in the ASCII region.
    pub fn ascii_column_at(&self, x: f64, bytes_per_row: usize) -> Option<usize> {
        if x < self.ascii_x0 || x >= self.ascii_x1 || self.ascii_pitch <= 0.0 {
            return None;
        }
        let c = ((x - self.ascii_x0) / self.ascii_pitch) as usize;
        (c < bytes_per_row).then_some(c)
    }

    /// The row index under `y`, relative to the first visible row.
    pub fn row_at(&self, y: f64) -> Option<usize> {
        if y < self.grid.y0 || y >= self.grid.y0 + self.visible_rows as f64 * self.row_h {
            return None;
        }
        Some(((y - self.grid.y0) / self.row_h) as usize)
    }
}

impl HexView {
    /// Recompute [`Geo`] for the window rect and `theme`, adopting the
    /// bytes-per-row that fills the content width. `tick` advances the
    /// inspector's slide (once per painted frame).
    pub(super) fn relayout(&mut self, tr: &mut TextRenderer, theme: &Theme, tick: bool) {
        let cell = CellMetrics::measure(tr, theme);
        self.cell = cell;
        let mut geo = Geo::default();
        self.place_inspector(&mut geo, theme, tick);
        self.place_header_controls(&mut geo, tr, theme);
        self.place_bands(&mut geo, theme);
        self.place_grid(&mut geo, cell, theme);
        Self::place_inspector_controls(&mut geo, theme);
        self.visible_rows = geo.visible_rows;
        self.mode_group.apply_layout_external(geo.mode);
        self.endian_group.apply_layout_external(geo.endian);
        self.geo = geo;
    }

    /// The inspector is the trailing region of the window, docked beside the
    /// content when there is room and a floating drawer when not. Resolves
    /// the content, the panel, the header row and the panel's toggle.
    fn place_inspector(&mut self, geo: &mut Geo, theme: &Theme, tick: bool) {
        let sp = &theme.tokens.spacing;
        let outer = self.rect;
        let base = ui::tokens::Metrics::BASE;
        self.drawer.scale_for(theme);
        // The inspector docks while the grid keeps a useful row beside it;
        // narrower than that it is a drawer over a full-width grid.
        let docked_row = RowPlan::new(self.cell, theme).width(DOCKED_BYTES_PER_ROW);
        self.drawer.set_breakpoint(
            docked_row.max(f64::from(base.content_min_width)) + f64::from(base.inspector_width),
        );
        self.drawer
            .set_docked_width(f64::from(base.inspector_width));
        let has_inspector = self.has_file;
        if has_inspector && tick {
            self.drawer.tick(theme);
        }
        let mut content = if has_inspector {
            self.drawer.resolve(outer)
        } else {
            outer
        };
        if has_inspector && std::mem::take(&mut self.reveal_inspector) {
            self.drawer.open();
            self.drawer.snap(true);
            content = self.drawer.resolve(outer);
        }
        let panel = if has_inspector {
            self.drawer.sidebar_rect()
        } else {
            Rect::ZERO
        };
        let docked = self.drawer.is_docked();
        let visible = has_inspector
            && panel.width() > 0.0
            && (docked || panel.x0 < outer.x1 - f64::from(sp.s1))
            && (self.drawer.is_open() || self.drawer.is_animating());

        // The header row: the tallest control with an `s3` inset, never
        // shorter than the shell's header, so the file name sits where every
        // other window's title does.
        let inset = f64::from(sp.s3);
        let control = ControlSize::Regular.height(theme);
        let header_h = f64::from(theme.tokens.metrics.header_height).max(control + 2.0 * inset);

        // The toggle rides the trailing edge of whichever header holds it:
        // the panel's when the panel shows, the window's otherwise.
        let target = f64::from(theme.tokens.metrics.min_pointer_target);
        let right = if visible && (docked || self.drawer.is_open()) {
            panel.x1
        } else {
            outer.x1
        };
        let top = if visible && !docked {
            panel.y0
        } else {
            outer.y0
        };
        geo.toggle = if has_inspector {
            let y0 = top + f64::midpoint(0.0, header_h - target);
            Rect::new(right - inset - target, y0, right - inset, y0 + target)
        } else {
            Rect::ZERO
        };
        geo.content = content;
        geo.inspector = panel;
        geo.inspector_visible = visible;
        geo.docked = docked;
        geo.header_h = header_h;
        geo.header = Rect::new(content.x0, outer.y0, content.x1, outer.y0 + header_h);
    }

    /// The header's edit-mode strip, right-aligned in the content header and
    /// beside the toggle when the toggle sits in this header.
    fn place_header_controls(&self, geo: &mut Geo, tr: &mut TextRenderer, theme: &Theme) {
        if !self.has_file {
            geo.mode = Rect::ZERO;
            return;
        }
        let sp = &theme.tokens.spacing;
        let strip_h = SegmentedFace::height(ControlSize::Small, theme);
        let mode_w = SegmentedFace::intrinsic_width(&MODE_LABELS, ControlSize::Small, tr, theme);
        let toggle_in_content = !(geo.inspector_visible && geo.docked);
        let right = if toggle_in_content {
            geo.toggle.x0 - f64::from(sp.s2)
        } else {
            geo.header.x1 - f64::from(sp.s3)
        };
        let top = geo.header.y0 + f64::midpoint(0.0, geo.header_h - strip_h);
        geo.mode = Rect::new(right - mode_w, top, right, top + strip_h);
    }

    /// The find band under the header and the status bar at the foot.
    fn place_bands(&self, geo: &mut Geo, theme: &Theme) {
        let outer = self.rect;
        let content = geo.content;
        let mut top = geo.header.y1;
        geo.find = if self.find_open {
            let h = self.find_bar.height(theme);
            let band = Rect::new(content.x0, top, content.x1, top + h);
            top += h;
            band
        } else {
            Rect::ZERO
        };
        let (region, status) =
            ui::chrome::status_bar::split(Rect::new(content.x0, top, content.x1, outer.y1), theme);
        geo.status = status;
        geo.grid = region;
        geo.bubble_area = Rect::new(content.x0, geo.header.y1, content.x1, status.y0);
    }

    /// The grid: how many bytes a row holds so the rows fill the content
    /// width, where each column starts, and how many rows fit.
    fn place_grid(&mut self, geo: &mut Geo, cell: CellMetrics, theme: &Theme) {
        let sp = &theme.tokens.spacing;
        let s = |v: f32| f64::from(v);
        let content = geo.content;
        let region = geo.grid;
        let row_h = (cell.line_h + s(sp.s1) / 2.0).ceil();
        let plan = RowPlan::new(cell, theme);
        let fit = (MIN_BYTES_PER_ROW / ROW_STEP..=MAX_BYTES_PER_ROW / ROW_STEP)
            .rev()
            .map(|steps| steps * ROW_STEP)
            .find(|&n| plan.width(n) <= content.width())
            .unwrap_or(MIN_BYTES_PER_ROW);
        if fit != self.model.bytes_per_row() {
            self.model.set_bytes_per_row(fit);
        }
        let bpr = self.model.bytes_per_row();

        // The glyphs keep their size; the gaps between bytes and columns
        // absorb what is left of the width, so the row ends at the scroll
        // bar's lane and no band of empty space is left beside it.
        let scale =
            ((content.width() - plan.fixed(bpr)) / plan.gaps(bpr)).clamp(GAP_SCALE.0, GAP_SCALE.1);
        let (byte_gap, group_gap, col_gap) = (
            plan.byte_gap * scale,
            plan.group_gap * scale,
            plan.col_gap * scale,
        );
        let (byte_w, gutter_w) = (plan.byte_w, plan.gutter_w);
        let hex_span = bpr as f64 * (byte_w + byte_gap) - byte_gap
            + (bpr.saturating_sub(1) / GROUP) as f64 * group_gap;

        let gutter_x0 = content.x0 + plan.left;
        let hex_x0 = gutter_x0 + gutter_w + col_gap;
        let ascii_x0 = hex_x0 + hex_span + col_gap;
        let head_top = region.y0 + s(sp.s2);
        geo.column_head = Rect::new(content.x0, head_top, content.x1, head_top + row_h);
        let grid_top = geo.column_head.y1 + s(sp.s1);
        geo.grid = Rect::new(content.x0, grid_top, content.x1, region.y1.max(grid_top));
        geo.row_h = row_h;
        geo.line_h = cell.line_h;
        geo.glyph_w = cell.glyph_w;
        geo.ascii_pitch = plan.ascii_pitch;
        geo.ascii_gap = plan.ascii_gap;
        geo.byte_w = byte_w;
        geo.byte_gap = byte_gap;
        geo.group_gap = group_gap;
        geo.gutter_x0 = gutter_x0;
        geo.hex_x0 = hex_x0;
        geo.ascii_x0 = ascii_x0;
        geo.ascii_x1 = ascii_x0 + bpr as f64 * plan.ascii_pitch;
        geo.rule_hex = hex_x0 - col_gap / 2.0;
        geo.rule_ascii = ascii_x0 - col_gap / 2.0;
        geo.visible_rows = ((geo.grid.height() / row_h).floor().max(1.0)) as usize;

        // The scroll position: never past the last full page, with a bar down
        // the grid's trailing edge once the buffer overflows it.
        let rows = self.model.row_count();
        self.model.set_top_row(
            self.model
                .top_row()
                .min(rows.saturating_sub(geo.visible_rows)),
        );
        // The thumb is the quiet bar every scroll view has: two thirds of the
        // lane, standing off the region's edge by an `s1` step.
        let lane = s(theme.tokens.metrics.scrollbar_gutter);
        let inset = s(sp.s1);
        geo.scroll_track = Rect::new(
            content.x1 - inset - lane * 2.0 / 3.0,
            geo.grid.y0,
            content.x1 - inset,
            geo.grid.y0 + geo.visible_rows as f64 * row_h,
        );
    }

    /// What the scroll bar represents: the rows, the page and the position.
    pub(super) fn scroll_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.model.row_count() as f64,
            self.geo.visible_rows as f64,
            self.model.top_row() as f64,
        )
    }

    /// The inspector's controls: the endianness strip under its header,
    /// then the properties.
    fn place_inspector_controls(geo: &mut Geo, theme: &Theme) {
        let sp = &theme.tokens.spacing;
        let panel = geo.inspector;
        let strip_h = SegmentedFace::height(ControlSize::Small, theme);
        let left = panel.x0 + f64::from(sp.s4);
        let top = panel.y0 + geo.header_h;
        geo.endian = Rect::new(
            left,
            top,
            (panel.x1 - f64::from(sp.s4)).max(left),
            top + strip_h,
        );
        geo.properties = Rect::new(
            panel.x0 + f64::from(sp.s2),
            geo.endian.y1 + f64::from(sp.s3),
            panel.x1 - f64::from(sp.s2),
            panel.y1,
        );
    }

    /// Recompute the geometry from the event path, with the theme the last
    /// layout pass saw.
    pub(super) fn relayout_now(&mut self) {
        let theme = self.layout_theme.clone();
        ui::text_render::with_layout_renderer(theme.text_scale, |tr| {
            self.relayout(tr, &theme, false);
        });
    }

    /// Map a mouse point to a byte offset + column, if it lands on a cell.
    pub(super) fn hit_test(&self, p: Point) -> Option<(usize, crate::model::Column)> {
        let g = &self.geo;
        if p.x < g.content.x0 || p.x >= g.content.x1 {
            return None;
        }
        let bpr = self.model.bytes_per_row();
        let row = self.model.top_row() + g.row_at(p.y)?;
        if let Some(col) = g.hex_column_at(p.x, bpr) {
            let off = row * bpr + col;
            return (off < self.model.len()).then_some((off, crate::model::Column::Hex));
        }
        let col = g.ascii_column_at(p.x, bpr)?;
        let off = row * bpr + col;
        (off < self.model.len()).then_some((off, crate::model::Column::Ascii))
    }
}

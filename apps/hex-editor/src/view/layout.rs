//! The hex view's geometry: one [`Geo`] per frame from the window rect, the theme and
//! the measured monospace advance. Paint, hit-testing and paging all read it, so a
//! byte's painted cell is its click target at every text scale.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::scrollbar::ScrollMetrics;
use ui::widget::{ControlSize, SegmentedFace};
use vello::kurbo::{Point, Rect};

use super::Editor;
use crate::model::Endian;

const MAX_BYTES_PER_ROW: usize = 64;
const MIN_BYTES_PER_ROW: usize = 8;
/// Rows hold a whole number of these, so a row never ends mid-group.
const ROW_STEP: usize = 4;
/// The row length the inspector must leave room for before it docks.
const DOCKED_BYTES_PER_ROW: usize = 16;
/// How far a row's gaps may stretch or shrink to fill the width it is given.
const GAP_SCALE: (f64, f64) = (0.5, 2.0);
pub(super) const GROUP: usize = 8;

pub(super) const ENDIAN_MESSAGES: [&str; 2] = ["hex-editor-little-endian", "hex-editor-big-endian"];

pub(super) fn endian_index(endian: Endian) -> usize {
    match endian {
        Endian::Little => 0,
        Endian::Big => 1,
    }
}

/// The monospace metrics every pitch derives from, measured with the cells' own
/// `Code` style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CellMetrics {
    pub glyph_w: f64,
    pub line_h: f64,
}

impl CellMetrics {
    /// Before the first draw has measured anything: the `Code` role's advance at 1×.
    pub const FALLBACK: Self = Self {
        glyph_w: 7.8,
        line_h: 19.5,
    };

    /// Measure the `Code` role with the renderer that paints it; eight glyphs, the
    /// gutter's width.
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

/// A row's widths: glyphs that never change and gaps that stretch to fill the
/// content width.
struct RowPlan {
    left: f64,
    right: f64,
    /// One ASCII cell: a glyph and a hairline of air, so a cursor box never touches
    /// its neighbours.
    ascii_gap: f64,
    ascii_pitch: f64,
    gutter_w: f64,
    byte_w: f64,
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

    fn fixed(&self, n: usize) -> f64 {
        self.left + self.gutter_w + n as f64 * (self.byte_w + self.ascii_pitch) + self.right
    }

    fn gaps(&self, n: usize) -> f64 {
        n.saturating_sub(1) as f64 * self.byte_gap
            + (n.saturating_sub(1) / GROUP) as f64 * self.group_gap
            + 2.0 * self.col_gap
    }

    fn width(&self, n: usize) -> f64 {
        self.fixed(n) + self.gaps(n)
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Geo {
    pub content: Rect,
    pub column_head: Rect,
    pub grid: Rect,
    pub scroll_track: Rect,
    pub status: Rect,
    pub inspector: Rect,
    pub inspector_visible: bool,
    pub docked: bool,
    pub header_h: f64,
    pub endian: Rect,
    pub properties: Rect,
    pub row_h: f64,
    pub line_h: f64,
    pub glyph_w: f64,
    pub ascii_pitch: f64,
    pub ascii_gap: f64,
    pub byte_w: f64,
    pub byte_gap: f64,
    pub group_gap: f64,
    pub gutter_x0: f64,
    pub hex_x0: f64,
    pub ascii_x0: f64,
    pub ascii_x1: f64,
    pub rule_hex: f64,
    pub rule_ascii: f64,
    pub visible_rows: usize,
}

impl Geo {
    pub fn hex_x(&self, c: usize) -> f64 {
        self.hex_x0 + c as f64 * (self.byte_w + self.byte_gap) + (c / GROUP) as f64 * self.group_gap
    }

    pub fn ascii_x(&self, c: usize) -> f64 {
        self.ascii_x0 + c as f64 * self.ascii_pitch + self.ascii_gap / 2.0
    }

    pub fn ascii_cell_x(&self, c: usize) -> f64 {
        self.ascii_x0 + c as f64 * self.ascii_pitch
    }

    pub fn hex_column_at(&self, x: f64, bytes_per_row: usize) -> Option<usize> {
        let half = self.byte_gap / 2.0;
        (0..bytes_per_row)
            .find(|&c| x >= self.hex_x(c) - half && x < self.hex_x(c) + self.byte_w + half)
    }

    pub fn ascii_column_at(&self, x: f64, bytes_per_row: usize) -> Option<usize> {
        if x < self.ascii_x0 || x >= self.ascii_x1 || self.ascii_pitch <= 0.0 {
            return None;
        }
        let c = ((x - self.ascii_x0) / self.ascii_pitch) as usize;
        (c < bytes_per_row).then_some(c)
    }

    pub fn row_at(&self, y: f64) -> Option<usize> {
        if y < self.grid.y0 || y >= self.grid.y0 + self.visible_rows as f64 * self.row_h {
            return None;
        }
        Some(((y - self.grid.y0) / self.row_h) as usize)
    }
}

impl Editor {
    /// Recompute [`Geo`] for the window rect and `theme`; `tick` advances the
    /// inspector's slide.
    pub(super) fn relayout(&mut self, tr: &mut TextRenderer, theme: &Theme, tick: bool) {
        let cell = CellMetrics::measure(tr, theme);
        self.cell = cell;
        let mut geo = Geo::default();
        self.place_inspector(&mut geo, theme, tick);
        self.place_bands(&mut geo, theme);
        self.place_grid(&mut geo, cell, theme);
        Self::place_inspector_controls(&mut geo, theme);
        self.visible_rows = geo.visible_rows;
        self.endian_group.apply_layout_external(geo.endian);
        self.geo = geo;
    }

    /// Dock the inspector beside the content when there is room, a floating drawer
    /// when not.
    fn place_inspector(&mut self, geo: &mut Geo, theme: &Theme, tick: bool) {
        let sp = &theme.tokens.spacing;
        let outer = self.rect;
        let base = ui::tokens::Metrics::BASE;
        self.drawer.scale_for(theme);
        // The inspector docks while the grid keeps a useful row beside it.
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
        let content = if has_inspector && std::mem::take(&mut self.reveal_inspector) {
            // Resolve first so the drawer knows its layout before it opens.
            self.drawer.resolve(outer);
            self.drawer.open();
            self.drawer.snap(true);
            self.drawer.resolve(outer)
        } else if has_inspector {
            self.drawer.resolve(outer)
        } else {
            outer
        };
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

        // The heading row is the tallest control with an `s3` inset, never shorter
        // than the shell's header.
        let inset = f64::from(sp.s3);
        let control = ControlSize::Regular.height(theme);
        let header_h = f64::from(theme.tokens.metrics.header_height).max(control + 2.0 * inset);
        geo.content = content;
        geo.inspector = panel;
        geo.inspector_visible = visible;
        geo.docked = docked;
        geo.header_h = header_h;
    }

    fn place_bands(&self, geo: &mut Geo, theme: &Theme) {
        let outer = self.rect;
        let content = geo.content;
        let (region, status) = ui::chrome::status_bar::split(
            Rect::new(content.x0, outer.y0, content.x1, outer.y1),
            theme,
        );
        geo.status = status;
        geo.grid = region;
    }

    /// How many bytes a row holds so the rows fill the width, where each column
    /// starts, and how many rows fit.
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

        // The glyphs keep their size; the gaps absorb the rest of the width, so no
        // band of empty space is left beside the scroll bar's lane.
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

        // The scroll position never passes the last full page; a bar shows once the
        // buffer overflows it.
        let rows = self.model.row_count();
        self.model.set_top_row(
            self.model
                .top_row()
                .min(rows.saturating_sub(geo.visible_rows)),
        );
        // The quiet bar every scroll view has, standing off the edge by an `s1` step.
        let lane = s(theme.tokens.metrics.scrollbar_gutter);
        let inset = s(sp.s1);
        geo.scroll_track = Rect::new(
            content.x1 - inset - lane * 2.0 / 3.0,
            geo.grid.y0,
            content.x1 - inset,
            geo.grid.y0 + geo.visible_rows as f64 * row_h,
        );
    }

    pub(super) fn scroll_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.model.row_count() as f64,
            self.geo.visible_rows as f64,
            self.model.top_row() as f64,
        )
    }

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

    pub(super) fn relayout_now(&mut self) {
        let theme = self.layout_theme.clone();
        ui::text_render::with_layout_renderer(&theme, |tr| {
            self.relayout(tr, &theme, false);
        });
    }

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

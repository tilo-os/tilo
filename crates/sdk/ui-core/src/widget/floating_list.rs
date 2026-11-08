//! `FloatingList` — the one floating option list: a Panel of rows that
//! scrolls, arms a row for the pointer and the keys, and places itself
//! beside its anchor.
//!
//! Owners bring the rows and paint them ([`FloatingRow`]); the list owns the
//! geometry, the scrolling, the arming, the arrival placeholder and the
//! placement, so a suggestion list, a dropdown, a filter menu and a context
//! menu all behave the same.

use std::ops::Range;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::WindowEvent;

use crate::motion::Extent;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::floating_surface;
use crate::widget::scroll_region::ScrollRegion;
use crate::widget::spinner::Spinner;
use crate::widget::{PanelAlign, PopoverSide};

const DEFAULT_MAX_VISIBLE: usize = 8;

/// The pickable row after (or, not `forward`, before) row `from` of
/// `count`: past the last it wraps round to the first, or without `wrap`
/// stays on the last; from no row it is the first (last) pickable row.
/// `None` when no row is pickable.
pub(crate) fn step_pickable(
    count: usize,
    from: Option<usize>,
    forward: bool,
    wrap: bool,
    pickable: impl Fn(usize) -> bool,
) -> Option<usize> {
    let first = (0..count).find(|&i| pickable(i))?;
    let last = (0..count).rev().find(|&i| pickable(i))?;
    let (past_end, past_start) = if wrap { (first, last) } else { (last, first) };
    Some(match from {
        None if forward => first,
        None => last,
        Some(cur) if forward => (cur.saturating_add(1)..count)
            .find(|&i| pickable(i))
            .unwrap_or(past_end),
        Some(cur) => (0..cur.min(count))
            .rev()
            .find(|&i| pickable(i))
            .unwrap_or(past_start),
    })
}

/// The geometry of a floating list in one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatingMetrics {
    /// One row, grown with the label's line at large text.
    pub row: f64,
    /// The list's inset round the rows.
    pub inset: f64,
    /// Between the anchor and the panel.
    pub gap: f64,
    /// Inside a row, before the icon and after the trailing end.
    pub pad: f64,
    /// A row's leading icon.
    pub icon: f64,
    /// A row's leading check.
    pub check: f64,
    /// The lane a scroll bar takes beside the rows.
    pub gutter: f64,
    /// Between the icon, the label and the trailing columns.
    pub spacing: f64,
}

impl FloatingMetrics {
    /// The geometry in `theme` for labels in `role`; `two_line` rows hold a
    /// caption line under the label.
    #[must_use]
    pub fn of(theme: &Theme, role: TextRole, two_line: bool) -> Self {
        let tokens = &theme.tokens;
        let line = f64::from(theme.text(role).line_height_px());
        let lines = if two_line {
            line + f64::from(theme.text(TextRole::Caption).line_height_px())
        } else {
            line
        };
        Self {
            row: f64::from(tokens.metrics.menu_row_height)
                .max((lines + 2.0 * f64::from(tokens.spacing.s1)).ceil()),
            inset: floating_surface::menu_inset(theme),
            gap: f64::from(tokens.spacing.s1),
            pad: f64::from(tokens.spacing.s3),
            icon: f64::from(tokens.metrics.icon_sm).max(line.round() - 2.0),
            check: f64::from(tokens.metrics.icon_sm),
            gutter: f64::from(tokens.metrics.scrollbar_gutter),
            spacing: f64::from(tokens.spacing.s2),
        }
    }

    /// A participant's avatar: a little larger than an icon, inside the row.
    #[must_use]
    pub fn avatar(&self) -> f64 {
        (self.icon + 4.0).min(self.row - 2.0 * self.inset.min(4.0))
    }

    /// The stock geometry, for hit-testing before the first paint.
    #[must_use]
    pub fn stock() -> Self {
        let spacing = crate::tokens::Spacing::BASE;
        let metrics = crate::tokens::Metrics::BASE;
        Self {
            row: f64::from(metrics.menu_row_height),
            inset: f64::from(spacing.s2),
            gap: f64::from(spacing.s1),
            pad: f64::from(spacing.s3),
            icon: f64::from(metrics.icon_sm),
            check: f64::from(metrics.icon_sm),
            gutter: f64::from(metrics.scrollbar_gutter),
            spacing: f64::from(spacing.s2),
        }
    }
}

/// What the list needs to know about a row it lays out; the owner paints it.
pub trait FloatingRow {
    /// What every row of a list shares, computed once per paint.
    type Columns: Default;

    /// This row's height in `metrics`.
    fn height(&self, metrics: &FloatingMetrics) -> f64;

    /// Whether `self` and `other` are the same row: replacing rows with the
    /// same ones then leaves the list scrolled where the person left it. Rows
    /// of plain data compare; a row carrying actions always replaces.
    fn same(&self, _other: &Self) -> bool {
        false
    }

    /// Whether the pointer and the arrow keys arm it.
    fn pickable(&self) -> bool;

    /// Whether it is on its way, showing progress at its content's end.
    fn busy(&self) -> bool {
        false
    }

    /// Whether it holds a caption line under its label: a list with one
    /// gives every row two lines.
    fn two_line(&self) -> bool {
        false
    }

    /// How many cells it offers side by side (an emoji bar), 0 for none.
    fn cell_count(&self) -> usize {
        0
    }

    /// What the row needs across, for a list that sizes itself to its rows.
    fn content_width(
        &self,
        _metrics: &FloatingMetrics,
        _role: TextRole,
        _text: &mut TextRenderer,
        _theme: &Theme,
    ) -> f64 {
        0.0
    }

    /// What every row of `rows` shares.
    fn columns(_rows: &[Self]) -> Self::Columns
    where
        Self: Sized,
    {
        Self::Columns::default()
    }

    /// Paint the row, returning where its content ends when it is busy: the
    /// list puts the progress there.
    fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        state: RowState<'_, Self::Columns>,
    ) -> Option<f64>
    where
        Self: Sized;
}

/// How a row is painted: where it stands, whether it is armed, and what its
/// list's rows share.
pub struct RowState<'a, C> {
    /// The row's index.
    pub index: usize,
    /// The armed row: the one a press or Enter takes.
    pub armed: bool,
    /// The pointer rests on it, the armed row aside.
    pub hovered: bool,
    /// The armed cell, of a row that offers cells.
    pub cell: usize,
    /// The list's geometry.
    pub metrics: FloatingMetrics,
    /// The row labels' style.
    pub role: TextRole,
    /// The panel's corner radius, for a row that must run concentric with it.
    pub radius: f64,
    /// What the list's rows share ([`FloatingRow::columns`]).
    pub columns: &'a C,
}

/// What a list's width was measured with: its label style and geometry, and
/// the renderer's fonts and text scale.
struct WidthKey {
    style: crate::text_render::TextStyle,
    metrics: FloatingMetrics,
    epoch: std::sync::Arc<()>,
    text_scale: f32,
}

/// A floating list of rows under (or, without room, above) an anchor: the
/// one menu list behind [`crate::widget::Autocomplete`], a code editor's
/// caret completion, the filter and slash menus, a dropdown's open list and
/// every context menu.
///
/// It is a floating Panel (`floating_surface`, the panel radius, the floating
/// shadow) placed by `floating_surface::place_aligned`, so it flips above its
/// anchor near the window's bottom and shifts to stay inside it. More rows
/// than fit scroll, the armed row always in view; a list taller than the room
/// beside its anchor shrinks to it. While rows are on their way the
/// framework's placeholder stands where the first will be
/// ([`crate::arriving`]); a status line stands in for rows when there are
/// none.
pub struct FloatingList<R: FloatingRow> {
    rows: Vec<R>,
    /// Each row's height, resolved from [`FloatingRow::height`] and the floor.
    heights: Vec<f64>,
    highlight: Option<usize>,
    hover: Option<usize>,
    /// The armed cell, of a row that offers cells.
    cell: usize,
    /// The rows' scrolling: where they are scrolled to, the bar beside them
    /// while they overflow, and the wheel over them.
    scroll: ScrollRegion,
    /// The theme the geometry was taken from, which the scroll region also
    /// needs between paints (to hit-test and to read a wheel).
    theme: Theme,
    max_visible: usize,
    /// The tallest the rows get before they scroll, in pixels.
    max_height: Option<f64>,
    /// The shortest a row is (a touch target).
    min_row: f64,
    /// A row height pinned by the owner over the theme's (a menu's rows are
    /// one command each, measured once).
    pinned_row: Option<f64>,
    status: Option<String>,
    /// Rows on their way: the timing of their placeholder.
    arriving: Option<crate::arriving::ArrivalClock>,
    /// The placeholder step the last paint drew.
    drawn_step: Option<crate::arriving::Step>,
    anchor: Rect,
    viewport: Option<Rect>,
    /// What the rows' labels are (Body; Code for code completion).
    role: TextRole,
    /// The geometry of the latest paint (stock before it).
    metrics: FloatingMetrics,
    /// The width the rows need, from the latest paint.
    content_width: f64,
    /// What [`Self::content_width`] was measured with, so a repaint reshapes
    /// no row; `None` once the rows changed.
    measured: Option<WidthKey>,
    /// The narrowest the panel is, whatever its anchor and rows need.
    min_width: f64,
    /// A band its owner paints at the top of the panel, above the rows (a
    /// picker's own filter field).
    header: f64,
    /// The side of the anchor the panel opens on when there is room there.
    side: PopoverSide,
    /// Which edge of the anchor the panel lines up with.
    align: PanelAlign,
    /// The panel spans the viewport's width right above its anchor: a
    /// phone's drawer docked above the keyboard, its rows a finger tall.
    docked: bool,
    /// A rect a host placed the panel at, over the list's own placement.
    placed: Option<Rect>,
    /// The panel's height, following its rows on the platform's spring.
    height: Extent,
    /// Turns beside a row whose content is on its way.
    spinner: Spinner,
}

impl<R: FloatingRow> Default for FloatingList<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: FloatingRow> FloatingList<R> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            heights: Vec::new(),
            highlight: None,
            hover: None,
            cell: 0,
            scroll: ScrollRegion::vertical().with_no_bars(),
            theme: Theme::dark(),
            max_visible: DEFAULT_MAX_VISIBLE,
            max_height: None,
            min_row: 0.0,
            pinned_row: None,
            status: None,
            arriving: None,
            drawn_step: None,
            anchor: Rect::ZERO,
            viewport: None,
            role: TextRole::Body,
            metrics: FloatingMetrics::stock(),
            content_width: 0.0,
            measured: None,
            min_width: 0.0,
            header: 0.0,
            side: PopoverSide::Below,
            align: PanelAlign::Leading,
            docked: false,
            placed: None,
            height: Extent::default(),
            spinner: Spinner::new(),
        }
    }

    /// Open on `side` of the anchor when there is room there: above a
    /// composer at the bottom of a window, below a field near its top.
    pub fn set_side(&mut self, side: PopoverSide) {
        self.side = side;
    }

    /// Line the panel up with the anchor's `align` edge: a menu button's
    /// list hangs from its trailing edge.
    pub fn set_align(&mut self, align: PanelAlign) {
        self.align = align;
    }

    /// Span the viewport's width right above the anchor, rows a finger tall:
    /// a phone's drawer docked above the keyboard.
    pub fn set_docked(&mut self, docked: bool) {
        self.docked = docked;
    }

    /// Whether the panel is docked across the viewport.
    #[must_use]
    pub fn is_docked(&self) -> bool {
        self.docked
    }

    /// Whether the panel's height is still moving, or a row is on its way:
    /// the list asks for frames until it rests.
    #[must_use]
    pub fn is_animating(&self, now: crate::time::Instant) -> bool {
        self.height.is_moving(now) || self.rows.iter().any(FloatingRow::busy)
    }

    /// Never narrower than `width`, so a picker over short labels keeps
    /// room for its field and hints.
    #[must_use]
    pub fn with_min_width(mut self, width: f64) -> Self {
        self.set_min_width(width);
        self
    }

    /// See [`Self::with_min_width`].
    pub fn set_min_width(&mut self, width: f64) {
        self.min_width = width.max(0.0);
    }

    /// Keep a band `height` tall at the top of the panel, above the rows,
    /// for its owner to paint in ([`Self::header_rect`]).
    pub fn set_header(&mut self, height: f64) {
        self.header = height.max(0.0);
    }

    /// The header band ([`Self::set_header`]) inside the panel, inset like
    /// the rows. `None` with nothing to show or no band.
    pub fn header_rect(&self) -> Option<Rect> {
        if self.header <= 0.0 {
            return None;
        }
        let panel = self.rect()?;
        let m = &self.metrics;
        Some(Rect::new(
            panel.x0 + m.inset,
            panel.y0 + m.inset,
            panel.x1 - m.inset,
            panel.y0 + m.inset + self.header,
        ))
    }

    /// Set the rows' labels in `role` (a code completion's labels are Code).
    #[must_use]
    pub fn with_text_role(mut self, role: TextRole) -> Self {
        self.role = role;
        self
    }

    /// Rows beyond this scroll instead of growing the panel.
    #[must_use]
    pub fn with_max_visible(mut self, rows: usize) -> Self {
        self.max_visible = rows.max(1);
        self
    }

    /// Rows beyond `rows` scroll instead of growing the panel.
    pub fn set_max_visible(&mut self, rows: usize) {
        self.max_visible = rows.max(1);
    }

    /// Rows taller than `height` together scroll instead of growing the
    /// panel; `None` leaves only [`Self::with_max_visible`].
    pub fn set_max_height(&mut self, height: Option<f64>) {
        self.max_height = height;
        self.place_scroll();
    }

    /// No row shorter than `height`: a touch target on a coarse pointer.
    pub fn set_min_row_height(&mut self, height: f64) {
        self.min_row = height.max(0.0);
        self.metrics.row = self.metrics.row.max(self.min_row);
        self.resolve_heights();
    }

    /// Pin every row's height over the theme's (`None` derives it from the
    /// label's line).
    pub fn set_row_height(&mut self, height: Option<f64>) {
        self.pinned_row = height;
        self.resolve_heights();
    }

    /// Show a bar beside the rows while they overflow, which a drag of its
    /// thumb scrolls as every bar does.
    #[must_use]
    pub fn with_scrollbar(mut self) -> Self {
        self.scroll = ScrollRegion::vertical();
        self
    }

    /// Keep the bar over the rows' edge instead of beside them (a menu).
    pub fn set_overlaid_bars(&mut self) {
        self.scroll = ScrollRegion::vertical().with_overlaid_bars();
    }

    /// Set the rows' label role; see [`Self::with_text_role`].
    pub fn set_text_role(&mut self, role: TextRole) {
        self.role = role;
    }

    /// The theme the geometry came from.
    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The geometry of the latest paint (stock before it).
    #[must_use]
    pub fn metrics(&self) -> &FloatingMetrics {
        &self.metrics
    }

    /// Take `theme`'s geometry now, before a paint measures the labels, so
    /// rows are hit where they will be drawn.
    pub fn set_theme(&mut self, theme: &Theme) {
        let two_line = self.rows.iter().any(FloatingRow::two_line);
        self.metrics = FloatingMetrics::of(theme, self.role, two_line);
        if self.docked {
            // A finger's row: the panel row every touch list has.
            self.metrics.row = self
                .metrics
                .row
                .max(f64::from(theme.tokens.metrics.panel_row_height));
        }
        if let Some(pinned) = self.pinned_row {
            self.metrics.row = pinned;
        }
        self.metrics.row = self.metrics.row.max(self.min_row);
        if self.theme.tokens != theme.tokens {
            self.theme = theme.clone();
        }
        self.resolve_heights();
    }

    /// Each row's height from the geometry and the floor.
    fn resolve_heights(&mut self) {
        let m = self.metrics;
        let min = self.min_row;
        self.heights = self
            .rows
            .iter()
            .map(|row| row.height(&m).max(min))
            .collect();
        self.place_scroll();
    }

    /// Replace the rows. The same rows again leave the list as it is: scrolled
    /// where the person scrolled it and its width measured. Otherwise keeps the
    /// armed row when it still exists and clears the status line once rows
    /// arrive.
    pub fn set_rows(&mut self, rows: Vec<R>) {
        if self.rows.len() == rows.len() && self.rows.iter().zip(&rows).all(|(a, b)| a.same(b)) {
            return;
        }
        self.rows = rows;
        self.measured = None;
        self.heights.clear();
        if !self.rows.is_empty() {
            self.status = None;
            self.arriving = None;
        }
        self.keep_highlight();
        self.resolve_heights();
        self.reveal();
    }

    /// The rows, in order.
    #[must_use]
    pub fn rows(&self) -> &[R] {
        &self.rows
    }

    fn keep_highlight(&mut self) {
        if self
            .highlight
            .is_some_and(|h| self.rows.get(h).is_none_or(|row| !row.pickable()))
        {
            self.highlight = None;
        }
    }

    /// The armed row's index.
    #[must_use]
    pub fn highlight(&self) -> Option<usize> {
        self.highlight
    }

    #[must_use]
    pub fn highlighted(&self) -> Option<&R> {
        self.highlight.and_then(|i| self.rows.get(i))
    }

    /// Arm row `index` (`None` disarms) and scroll it into view.
    pub fn set_highlight(&mut self, index: Option<usize>) {
        self.highlight = index.filter(|&i| self.rows.get(i).is_some_and(FloatingRow::pickable));
        self.reveal();
    }

    /// The armed cell, of a row that offers cells.
    #[must_use]
    pub fn cell(&self) -> usize {
        self.cell
    }

    pub fn set_cell(&mut self, cell: usize) {
        self.cell = cell;
    }

    /// The whole rows in view, at least one.
    fn rows_in_view(&self) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative count of rows in view, floored"
        )]
        let rows = (self.view_height() / self.metrics.row.max(1.0)).floor() as usize;
        rows.max(1)
    }

    /// Arm the row a page (the rows in view) further on or back, stopping at
    /// the ends: Page Down and Page Up.
    pub fn page(&mut self, forward: bool) {
        let pickable: Vec<usize> = (0..self.rows.len())
            .filter(|&i| self.rows[i].pickable())
            .collect();
        let (Some(&first), Some(&last)) = (pickable.first(), pickable.last()) else {
            return;
        };
        let step = self.rows_in_view().saturating_sub(1).max(1);
        let from = self.highlight.unwrap_or(if forward { first } else { last });
        let target = if forward {
            from.saturating_add(step)
        } else {
            from.saturating_sub(step)
        };
        let next = if forward {
            pickable
                .iter()
                .copied()
                .find(|&i| i >= target)
                .unwrap_or(last)
        } else {
            pickable
                .iter()
                .rev()
                .copied()
                .find(|&i| i <= target)
                .unwrap_or(first)
        };
        self.set_highlight(Some(next));
    }

    /// Arm the next (or previous) row, wrapping at both ends.
    pub fn move_highlight(&mut self, forward: bool) {
        self.step_highlight(forward, true);
    }

    /// Arm the next (or previous) row; at either end wrap round, or stay
    /// there without `wrap` (a chooser's list).
    pub fn step_highlight(&mut self, forward: bool, wrap: bool) {
        let next = step_pickable(self.rows.len(), self.highlight, forward, wrap, |i| {
            self.rows.get(i).is_some_and(FloatingRow::pickable)
        });
        if next.is_some() {
            self.set_highlight(next);
        }
    }

    /// A line shown while there are no rows ("Searching…", "No matches");
    /// `None` clears it.
    pub fn set_status(&mut self, status: Option<String>) {
        if self.status != status {
            self.status = status;
            self.measured = None;
        }
    }

    /// Rows are on their way, with `clock`'s timing (asked when, from
    /// where): the framework's placeholder stands where the first will be.
    /// `None` once they are in, or nothing is asked.
    pub fn set_arriving(&mut self, clock: Option<crate::arriving::ArrivalClock>) {
        self.arriving = clock;
    }

    /// The timing of the rows on their way, for an owner that names them.
    #[must_use]
    pub fn arriving(&self) -> Option<&crate::arriving::ArrivalClock> {
        self.arriving.as_ref()
    }

    /// Whether rows are on their way.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.arriving.is_some()
    }

    /// The placeholder step the list shows at `now`, while rows are on
    /// their way and none is here.
    fn arriving_step(&self, now: crate::time::Instant) -> Option<crate::arriving::Step> {
        self.rows
            .is_empty()
            .then_some(self.arriving.as_ref())
            .flatten()
            .map(|clock| clock.step(now))
    }

    /// When the placeholder steps on next.
    pub fn next_arriving_step(&self, now: crate::time::Instant) -> Option<crate::time::Instant> {
        if !self.rows.is_empty() {
            return None;
        }
        self.arriving.as_ref()?.next_step_at(now)
    }

    /// Whether the placeholder stepped on since the last paint.
    pub fn arriving_moved(&self) -> bool {
        self.arriving_step(crate::motion::frame_now()) != self.drawn_step
    }

    /// The rect the list opens from (a field, or the caret's line).
    pub fn set_anchor(&mut self, anchor: Rect) {
        self.anchor = anchor;
        self.place_scroll();
    }

    /// The rect the list opens from.
    #[must_use]
    pub fn anchor(&self) -> Rect {
        self.anchor
    }

    /// The bounds the list stays inside, normally the window.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = Some(viewport);
        self.place_scroll();
    }

    /// Whether there is anything to show: rows, a status line, a header band
    /// or rows on their way.
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.rows.is_empty()
            || self.status.is_some()
            || self.arriving.is_some()
            || self.header > 0.0
    }

    /// Clear rows, status and armed row.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.heights.clear();
        self.measured = None;
        self.highlight = None;
        self.hover = None;
        self.cell = 0;
        self.scroll.set_offset(Vec2::ZERO);
        self.status = None;
        self.arriving = None;
    }

    /// Keep the armed row in view.
    fn reveal(&mut self) {
        let Some(h) = self.highlight else {
            return;
        };
        if self.rect().is_none() {
            return;
        }
        self.place_scroll();
        let top = self.row_top(h);
        self.scroll.scroll_into_view(Rect::new(
            0.0,
            top,
            0.0,
            top + self.heights.get(h).copied().unwrap_or(self.metrics.row),
        ));
    }

    /// The height of every row together.
    #[must_use]
    pub fn content_height(&self) -> f64 {
        self.heights.iter().sum()
    }

    /// Where row `index` starts below the first row.
    #[must_use]
    pub fn row_top(&self, index: usize) -> f64 {
        self.heights.iter().take(index).sum()
    }

    /// The height of the rows in view: every row, up to
    /// [`Self::with_max_visible`] rows, [`Self::set_max_height`] and the
    /// room beside the anchor. One row's for a status line.
    fn view_height(&self) -> f64 {
        let m = &self.metrics;
        if self.rows.is_empty() {
            return m.row;
        }
        let mut height = self
            .content_height()
            .min(crate::num::f64_of_usize(self.max_visible) * m.row);
        if let Some(max) = self.max_height {
            height = height.min(max.max(m.row));
        }
        if let Some(viewport) = self.viewport.filter(|_| !self.docked) {
            use PopoverSide::{Above, Below};
            let room = [Above, Below]
                .into_iter()
                .map(|side| floating_surface::anchored_room(self.anchor, side, m.gap, viewport))
                .fold(0.0, f64::max);
            height = height.min((room - 2.0 * m.inset - self.header).max(0.0));
        }
        height
    }

    /// The box inside `panel` the rows take: inside the inset, under the
    /// header.
    fn rows_area(&self, panel: Rect) -> Rect {
        let m = self.metrics;
        Rect::new(
            panel.x0 + m.inset,
            (panel.y0 + m.inset + self.header).min(panel.y1),
            (panel.x1 - m.inset).max(panel.x0),
            (panel.y1 - m.inset).max(panel.y0),
        )
    }

    /// Give the rows' scroll region its box, how tall the rows are, and the
    /// step a wheel line takes (one row). It keeps the offset inside them.
    pub fn place_scroll(&mut self) {
        let Some(panel) = self.rect() else {
            return;
        };
        let area = self.rows_area(panel);
        self.scroll.set_viewport(area, &self.theme);
        self.scroll
            .set_content_size(area.width(), self.content_height());
        self.scroll.set_line_step(self.metrics.row);
    }

    /// The panel's size for the rows as measured, which is what a host that
    /// places the list by hand needs before it paints once.
    #[must_use]
    pub fn wanted_size(&self) -> (f64, f64) {
        let m = &self.metrics;
        let height = self.view_height() + 2.0 * m.inset + self.header;
        let width = self
            .anchor
            .width()
            .max(self.content_width)
            .max(self.min_width);
        (width, height)
    }

    /// Put the panel where a host placed it, keeping the rows inside: a host
    /// that places the list by the rules of its own surface (a menu beside a
    /// control it must not cover) measures with [`Self::measure`], places,
    /// and hands the rect over here.
    pub fn set_rect(&mut self, rect: Rect) {
        self.placed = Some(rect);
        self.place_scroll();
    }

    /// Take the panel's height as it is now, without growing into it.
    pub fn snap_height(&mut self) {
        self.height
            .snap(self.placed.map_or(0.0, |rect| rect.height()));
    }

    /// The panel's rect: under the anchor, or above it when there is more
    /// room there, inside the viewport. `None` with nothing to show.
    #[must_use]
    pub fn rect(&self) -> Option<Rect> {
        if !self.has_content() {
            return None;
        }
        let m = &self.metrics;
        let height = self.view_height() + 2.0 * m.inset + self.header;
        let viewport = self
            .viewport
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        if let Some(placed) = self.placed {
            return Some(placed);
        }
        if self.docked {
            // Across the viewport, its bottom on the anchor's top: the
            // keyboard and the field stay where they are, the drawer above.
            let y1 = self.anchor.y0 - m.gap;
            let y0 = (y1 - height).max(viewport.y0);
            return Some(Rect::new(viewport.x0, y0, viewport.x1, y1));
        }
        let width = self
            .anchor
            .width()
            .max(self.content_width)
            .max(self.min_width)
            .min(viewport.width().max(1.0));
        Some(
            floating_surface::place_aligned(
                self.anchor,
                (width, height),
                self.side,
                self.align,
                m.gap,
                viewport,
            )
            .0,
        )
    }

    /// The box the rows show in: the region's content.
    fn rows_box(&self) -> Rect {
        self.scroll.content_rect()
    }

    /// Row `index` in content coordinates: the region's transform puts it
    /// where the scroll draws it.
    fn row_in(&self, index: usize) -> Rect {
        let top = self.row_top(index);
        let height = self.heights.get(index).copied().unwrap_or(self.metrics.row);
        Rect::new(0.0, top, self.rows_box().width(), top + height)
    }

    /// The rows at least partly in view.
    pub fn in_view(&self) -> Range<usize> {
        let offset = self.scroll.offset().y;
        let shown = self.rows_box().height();
        let (from, to) = (offset, offset + shown);
        let first = (0..self.rows.len()).find(|&i| self.row_top(i) + self.heights[i] > from);
        let last = (0..self.rows.len()).rev().find(|&i| self.row_top(i) < to);
        match (first, last) {
            (Some(first), Some(last)) => first..last.saturating_add(1),
            _ => 0..0,
        }
    }

    /// The part of row `index` in view, in host coordinates.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        if index >= self.rows.len() {
            return None;
        }
        let shown = self.rows_box();
        let row = self.row_in(index) + (shown.origin().to_vec2() - self.scroll.offset());
        let clipped = row.intersect(shown);
        (!shown.is_zero_area() && clipped.height() > 0.0).then_some(clipped)
    }

    /// The rows' box on screen: where the rows show.
    pub fn rows_rect(&self) -> Rect {
        self.rows_box()
    }

    /// The row under `point`, pickable or not.
    pub fn row_at_point(&self, point: Point) -> Option<usize> {
        let shown = self.rows_box();
        if !shown.contains(point) || self.rows.is_empty() {
            return None;
        }
        let y = point.y - shown.y0 + self.scroll.offset().y;
        let index = (0..self.rows.len()).rev().find(|&i| self.row_top(i) <= y)?;
        (y < self.row_top(index) + self.heights.get(index).copied().unwrap_or(0.0)).then_some(index)
    }

    /// The pickable row under `point`.
    #[must_use]
    pub fn row_at(&self, point: Point) -> Option<usize> {
        self.row_at_point(point)
            .filter(|&i| self.rows[i].pickable())
    }

    /// Arm the row under `point`, as a pointer on a menu does, without
    /// scrolling a row half in view. Whether the armed row changed.
    pub fn arm_at(&mut self, point: Point) -> bool {
        match self.row_at(point) {
            Some(index) if self.highlight != Some(index) => {
                self.highlight = Some(index);
                true
            }
            _ => false,
        }
    }

    /// Track the pointer for the hover lift; returns whether it changed.
    pub fn hover_at(&mut self, point: Point) -> bool {
        let hover = self.row_at(point);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// Scroll by `rows` (positive scrolls down), within the rows.
    pub fn scroll_by(&mut self, rows: i32) {
        self.scroll_px(f64::from(rows) * self.metrics.row);
    }

    /// Scroll by `dy` pixels (positive scrolls down), within the rows.
    pub fn scroll_px(&mut self, dy: f64) {
        self.scroll.scroll_by(Vec2::new(0.0, dy));
    }

    /// Scroll by `delta` (positive scrolls down), within the rows.
    pub fn scroll_delta(&mut self, delta: Vec2) -> bool {
        self.scroll.scroll_by(delta)
    }

    /// How far the rows are scrolled, in pixels.
    #[must_use]
    pub fn scroll_offset(&self) -> f64 {
        self.scroll.offset().y
    }

    /// Whether the rows' bar is held, hovered or has the pointer on it.
    #[must_use]
    pub fn is_scrollbar_active(&self) -> bool {
        self.scroll.is_bar_active()
    }

    /// Whether the bar's thumb is being dragged: the pointer stays captured
    /// until it is released.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.scroll.is_dragging()
    }

    /// The pointer left the window: the bar shows no hover, and a held thumb
    /// is let go (the window reports no release for it).
    pub fn pointer_left(&mut self) {
        self.scroll.cancel_interaction();
    }

    /// Whether the rows' scroll is animating: a glide after a flick, or the
    /// bar fading in or out.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        self.scroll.needs_animation_frame(&self.theme)
    }

    /// Step the scroll's animation to this frame; returns whether a glide
    /// moved the rows.
    pub fn advance(&mut self) -> bool {
        self.scroll.advance()
    }

    /// Forward `event` to the rows' scroll region: the wheel over the rows,
    /// a press on their bar, a drag of its thumb. `ctx.handled` says whether
    /// the region took it; the return value is whether the scroll changed
    /// (a repaint, not consumed input).
    pub fn scroll_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if self.rect().is_none() {
            return false;
        }
        self.place_scroll();
        self.scroll.handle_event(event, ctx, &self.theme)
    }

    /// One row's height.
    #[must_use]
    pub fn row_height(&self) -> f64 {
        self.metrics.row
    }

    /// The height of row `index`.
    #[must_use]
    pub fn row_height_at(&self, index: usize) -> f64 {
        self.heights.get(index).copied().unwrap_or(0.0)
    }

    /// The rows' bar thumb and track.
    #[cfg(test)]
    pub(crate) fn scrollbar_thumb(&self) -> Option<(Rect, Rect)> {
        self.scroll.test_vertical_thumb(&self.theme)
    }

    /// The width every row needs, as the next paint will: a host that
    /// places a list by hand measures before it has painted once.
    pub fn measure(&mut self, text: &mut TextRenderer, theme: &Theme) {
        self.set_theme(theme);
        if self.placed.is_some() {
            return;
        }
        let style = theme.text(self.role);
        let measured = self.measured.as_ref().is_some_and(|key| {
            key.style == style
                && key.metrics == self.metrics
                && key.text_scale.to_bits() == text.text_scale().to_bits()
                && std::sync::Arc::ptr_eq(&key.epoch, text.layout_epoch())
        });
        if measured {
            return;
        }
        let widest = self
            .rows
            .iter()
            .map(|row| row.content_width(&self.metrics, self.role, text, theme))
            .fold(0.0, f64::max);
        let status = self.status.as_ref().map_or(0.0, |status| {
            f64::from(text.measure_style(status, None, &style).0)
                + self.metrics.icon
                + self.metrics.spacing
        });
        self.content_width =
            (widest.max(status) + 2.0 * (self.metrics.pad + self.metrics.inset)).ceil();
        self.measured = Some(WidthKey {
            style,
            metrics: self.metrics,
            epoch: std::sync::Arc::clone(text.layout_epoch()),
            text_scale: text.text_scale(),
        });
    }

    /// The panel's radius: one row alone is concentric with it.
    fn radius(&self, theme: &Theme) -> f64 {
        let single =
            self.header <= 0.0 && self.rows.iter().filter(|row| row.pickable()).count() == 1;
        if single {
            floating_surface::single_row_radius(theme)
        } else {
            floating_surface::radius(theme)
        }
    }

    /// Paint the list in the window's top layer.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.measure(text, theme);
        let Some(target) = self.rect() else {
            return;
        };
        self.place_scroll();
        // The height follows the rows on the platform's spring, growing from
        // the edge that faces the anchor; the rows stay where they will be.
        let now = crate::motion::frame_now();
        self.height.follow(
            target.height(),
            theme.motion(),
            crate::motion::input_source(),
            now,
        );
        let shown = self.height.get(now).clamp(0.0, target.height());
        let above = target.y1 <= self.anchor.y0 + 0.5;
        let panel = if above {
            Rect::new(target.x0, target.y1 - shown, target.x1, target.y1)
        } else {
            Rect::new(target.x0, target.y0, target.x1, target.y0 + shown)
        };
        if let Some(settles) = self.height.settles_at() {
            crate::motion::want_frames_until(settles);
        }
        let radius = self.radius(theme);
        floating_surface::paint_rounded(scene, transform, panel, radius, theme);
        let m = self.metrics;
        self.drawn_step = self.arriving_step(now);
        let columns = R::columns(&self.rows);
        if self.rows.is_empty() {
            self.paint_placeholder(scene, transform, text, theme, panel);
            return;
        }
        let clip = self.scroll.content_rect().intersect(self.rows_area(panel));
        let content = self.scroll.content_transform(transform);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        let mut busy_at = None;
        for index in self.in_view() {
            let Some(item) = self.rows.get(index) else {
                continue;
            };
            let rect = self.row_in(index);
            let busy = item.busy();
            let state = RowState {
                index,
                armed: self.highlight == Some(index),
                hovered: self.hover == Some(index),
                cell: self.cell,
                metrics: m,
                role: self.role,
                radius,
                columns: &columns,
            };
            let mut ctx = PaintCtx::new(scene, content, text, theme);
            let end = item.paint(&mut ctx, rect, state);
            if busy && let Some(end) = end {
                busy_at = Some((rect, end));
            }
        }
        if let Some((rect, end)) = busy_at {
            let side = f64::from(theme.text(TextRole::Caption).line_height_px());
            let x = end + m.spacing;
            self.spinner.set_rect(Rect::new(
                x,
                rect.center().y - side / 2.0,
                x + side,
                rect.center().y + side / 2.0,
            ));
            crate::widget::Widget::draw(
                &mut self.spinner,
                scene,
                content,
                text,
                theme,
                &crate::presence::PresenceCtx::default(),
            );
            if theme.motion().loops() {
                crate::motion::want_frames_until(now + crate::time::Duration::from_millis(100));
            }
        }
        scene.pop_layer();
        self.scroll.note_shown(transform, theme);
        self.scroll.paint_bars(scene, transform, theme);
    }

    /// Rows on their way (the framework's placeholder, stepping on at its
    /// deadlines) or the status line, where the first row would be.
    fn paint_placeholder(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        let m = self.metrics;
        let area = self.rows_area(panel);
        let x = area.x0 + m.pad;
        if let Some(clock) = &self.arriving {
            let mut ctx = PaintCtx::new(scene, transform, text, theme);
            let line = ctx.line_box(self.role);
            let slot = Rect::new(
                x,
                area.center().y - line / 2.0,
                area.x1 - m.pad,
                area.center().y + line / 2.0,
            );
            crate::widget::ArrivalFace::new(
                crate::widget::SkeletonShape::Text { lines: 1 },
                clock,
                crate::motion::frame_now(),
            )
            .paint(&mut ctx, slot);
            return;
        }
        let Some(status) = &self.status else {
            return;
        };
        let style = theme.text(self.role);
        let shown = text.elide_to_width_style(
            status,
            crate::num::f32_of_f64((area.x1 - m.pad - x).max(0.0)),
            &style,
        );
        let (_, h) = text.measure_style(&shown, None, &style);
        text.draw_style(
            scene,
            &shown,
            theme.text_dim,
            transform.pre_translate(Vec2::new(x, area.center().y - f64::from(h) / 2.0)),
            None,
            &style,
        );
    }

    /// The status line the list shows, if any.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }
}

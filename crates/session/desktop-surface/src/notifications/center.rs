//! The notification centre: retained notifications grouped by verified app,
//! a keyboard selection, per-row controls, Clear all with Undo, and the
//! paging that keeps a long history readable in a compact panel. Paint,
//! pointer hit-testing, keyboard and accessibility all read the rows and the
//! one [`CenterLayout`] defined here.
//!
//! The centre holds only viewer-local state (selection, folded groups, the
//! scroll position). The host places it in a panel and owns everything it
//! shows through [`NotificationHost`].

use std::collections::BTreeSet;

use ui::a11y::{AccessNode, AccessState, Role};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, NamedKey};

use super::NotificationHost;
use super::history::{
    HistoryActivation, HistoryRow, RowIdentity, accessible_description, relative_time, source_label,
};
use super::log::NotificationLog;
use super::model::Notification;
use super::reader::NotificationReader;
use crate::drill_in::{self as qs, DrillInList};
use crate::i18n::{t, t_args};
use ui::TextRole;

/// Rows the panel asks room for before it starts paging.
const PREFERRED_ROWS: usize = 7;

/// Lossy but bounded: row counts never approach f64's exact-integer range.
#[allow(
    clippy::cast_precision_loss,
    reason = "row counts stay far below f64's exact-integer range"
)]
fn usize_to_f64_lossy(n: usize) -> f64 {
    n as f64
}

/// Floors a row-count computation; a negative input saturates to 0.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "float-to-usize casts saturate; a negative input already floors to 0"
)]
fn f64_floor_to_usize_saturating(v: f64) -> usize {
    v.floor() as usize
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "device-pixel wrap width; f32 is already the renderer's own unit"
)]
pub(super) fn f64_to_f32_lossy(v: f64) -> f32 {
    v as f32
}

/// Rows promise one line per field; the complete text remains in their
/// accessible description. Use toolkit shaping/elision, including graphemes.
fn preview_line(
    renderer: &mut TextRenderer,
    text: &str,
    width: f32,
    style: &ui::text_render::TextStyle,
) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    renderer.elide_to_width_style(&line, width, style)
}

/// Measured shared action faces and typography, cached by the host for
/// drawing, pointer hit-testing and accessibility between frames.
#[derive(Clone, Copy, Debug)]
pub struct CenterMetrics {
    lines: (f64, f64),
    title_width: f64,
    cleared_width: f64,
    back_width: f64,
    clear_width: f64,
    undo_width: f64,
    read_width: f64,
    height: f64,
}

impl From<(f64, f64)> for CenterMetrics {
    fn from(lines: (f64, f64)) -> Self {
        Self {
            lines,
            title_width: 0.0,
            cleared_width: 0.0,
            back_width: 2.0 * qs::TARGET,
            clear_width: 2.0 * qs::TARGET,
            undo_width: 2.0 * qs::TARGET,
            read_width: 2.0 * qs::TARGET,
            height: qs::TARGET,
        }
    }
}

/// The centre's geometry for one panel rect and type scale, so paint,
/// hit-testing, the visible-row count and the preferred height read one
/// oracle (HIG §7b: one rhythm). Every step is the drill-in panel's shared
/// rhythm ([`crate::drill_in`]).
#[derive(Clone, Copy, Debug)]
pub struct CenterLayout {
    panel: Rect,
    /// Line boxes of a caption and a label at the text scale.
    caption_lh: f64,
    label_lh: f64,
    actions: CenterMetrics,
}

impl CenterLayout {
    /// Space the pager band claims under the last row: a related step, the
    /// band, the panel padding.
    fn footer_reserve(&self) -> f64 {
        qs::RELATED_GAP + self.actions.height + qs::PANEL_PAD
    }

    fn header_height(&self) -> f64 {
        self.actions.height
            + 2.0 * qs::RELATED_GAP
            + if self.wrapped_header() {
                self.label_lh + qs::RELATED_GAP
            } else {
                0.0
            }
    }

    fn wrapped_header(&self) -> bool {
        self.panel.width() > 0.0
            && self.panel.width() - 2.0 * qs::PANEL_PAD
                < self.actions.back_width
                    + self.actions.clear_width
                    + self.actions.title_width
                    + 2.0 * qs::RELATED_GAP
    }

    fn title_rect(&self) -> Rect {
        if self.wrapped_header() {
            let y0 = self.back().y1 + qs::RELATED_GAP;
            Rect::new(
                self.panel.x0 + qs::PANEL_PAD,
                y0,
                self.panel.x1 - qs::PANEL_PAD,
                y0 + self.label_lh,
            )
        } else {
            Rect::new(
                self.back().x1 + qs::RELATED_GAP,
                self.panel.y0,
                self.clear().x0 - qs::RELATED_GAP,
                self.panel.y0 + self.header_height(),
            )
        }
    }

    /// Resolve the same widths and height the shared button painter needs.
    pub fn metrics(theme: &Theme, text: &mut TextRenderer) -> CenterMetrics {
        let width = |key: &str, role, text: &mut TextRenderer| {
            ButtonFace::new(Rect::ZERO, &t(key))
                .role(role)
                .intrinsic_width(text, theme)
                .max(2.0 * qs::TARGET)
        };
        CenterMetrics {
            lines: Self::type_lines(theme),
            title_width: f64::from(
                text.measure_style(
                    &t("notification-center-title"),
                    None,
                    &theme
                        .text(TextRole::Label)
                        .with_weight(TextWeight::Semibold),
                )
                .0,
            ),
            cleared_width: f64::from(
                text.measure_style(
                    &t("notification-center-cleared"),
                    None,
                    &theme.text(TextRole::Caption),
                )
                .0,
            ),
            back_width: width("drill-in-back", ButtonRole::Plain, text),
            clear_width: width("notification-center-clear-all", ButtonRole::Plain, text),
            undo_width: width("notification-center-undo", ButtonRole::Secondary, text),
            read_width: width("notification-center-read", ButtonRole::Plain, text),
            height: ButtonFace::height(ControlSize::Regular, theme),
        }
    }

    /// The line boxes of the caption and label roles the centre paints
    /// with, in `theme`: what [`Self::new`] lays rows out from.
    pub fn type_lines(theme: &ui::Theme) -> (f64, f64) {
        let line = |role| f64::from(theme.text(role).line_height_px());
        (line(TextRole::Caption), line(TextRole::Label))
    }

    /// Use [`Self::metrics`] for measured shared actions. Legacy line-box tuples
    /// from [`Self::type_lines`] remain accepted.
    pub fn new(panel: Rect, metrics: impl Into<CenterMetrics>) -> Self {
        let actions = metrics.into();
        Self {
            panel,
            caption_lh: actions.lines.0,
            label_lh: actions.lines.1,
            actions,
        }
    }
    pub fn panel(&self) -> Rect {
        self.panel
    }
    /// The "Back" chip: two pointer floors wide, on the panel padding,
    /// centred in the title band. A textual Back survives enlarged text
    /// where a small icon would not.
    pub fn back(&self) -> Rect {
        let p = self.panel;
        Rect::new(
            p.x0 + qs::PANEL_PAD,
            p.y0 + qs::RELATED_GAP,
            p.x0 + qs::PANEL_PAD + self.actions.back_width,
            p.y0 + qs::RELATED_GAP + self.actions.height,
        )
    }
    /// The "Clear all" chip, mirroring [`Self::back`] on the trailing padding.
    pub fn clear(&self) -> Rect {
        let p = self.panel;
        Rect::new(
            p.x1 - qs::PANEL_PAD - self.actions.clear_width,
            p.y0 + qs::RELATED_GAP,
            p.x1 - qs::PANEL_PAD,
            p.y0 + qs::RELATED_GAP + self.actions.height,
        )
    }
    /// The Undo button on the "cleared" status band.
    pub fn undo(&self) -> Rect {
        let r = self.footer();
        Rect::new(r.x1 - self.actions.undo_width, r.y0, r.x1, r.y1)
    }
    /// The "cleared" status band at the panel's foot.
    fn cleared_band(&self) -> Rect {
        let mut band = self.footer();
        band.y0 -= self.cleared_extra_height();
        band.inflate(qs::CONTROL_PAD, qs::CONTROL_PAD)
    }

    fn cleared_extra_height(&self) -> f64 {
        if self.panel.width() > 0.0 && self.actions.cleared_width > self.cleared_inline_width() {
            self.caption_lh + qs::RELATED_GAP
        } else {
            0.0
        }
    }

    fn cleared_inline_width(&self) -> f64 {
        self.footer().width() - self.actions.undo_width - 2.0 * qs::RELATED_GAP
    }

    fn cleared_label_rect(&self) -> Rect {
        let band = self.cleared_band();
        let x0 = band.x0 + qs::CONTROL_INSET;
        let y0 = band.y0
            + if self.cleared_extra_height() > 0.0 {
                qs::CONTROL_PAD
            } else {
                qs::CONTROL_INSET
            };
        let x1 = if self.cleared_extra_height() > 0.0 {
            band.x1 - qs::CONTROL_INSET
        } else {
            self.undo().x0 - qs::CONTROL_INSET
        };
        Rect::new(x0, y0, x1, y0 + self.caption_lh)
    }
    /// Top of the position line ("Newest 1–4 of 9"): where a list starts.
    fn position_y(&self) -> f64 {
        self.panel.y0 + self.header_height() + qs::RELATED_GAP
    }
    /// Top of the first row: one related step under the position line.
    fn rows_top(&self) -> f64 {
        self.position_y() + self.caption_lh + qs::RELATED_GAP
    }
    /// One row: source/time, title and body lines on the control padding.
    pub fn row_h(&self) -> f64 {
        2.0 * qs::CONTROL_PAD + 2.0 * self.caption_lh + self.label_lh
    }
    fn pitch(&self) -> f64 {
        self.row_h() + qs::RELATED_GAP
    }
    /// The `index`th visible row.
    pub fn row(&self, index: usize) -> Rect {
        let y0 = self.rows_top() + usize_to_f64_lossy(index) * self.pitch();
        Rect::new(
            self.panel.x0 + qs::PANEL_PAD,
            y0,
            self.panel.x1 - qs::PANEL_PAD,
            y0 + self.row_h(),
        )
    }
    /// The pager band at the panel's foot, on the panel padding.
    pub fn footer(&self) -> Rect {
        let p = self.panel;
        Rect::new(
            p.x0 + qs::PANEL_PAD,
            p.y1 - qs::PANEL_PAD - self.actions.height,
            p.x1 - qs::PANEL_PAD,
            p.y1 - qs::PANEL_PAD,
        )
    }
    /// Complete rows that fit above `bottom_reserve` at the panel's foot.
    fn rows_that_fit(&self, bottom_reserve: f64) -> usize {
        let available = self.panel.y1 - bottom_reserve - self.rows_top() + qs::RELATED_GAP;
        f64_floor_to_usize_saturating(available / self.pitch())
    }
    /// Complete rows the panel shows of `total`. A history that fits ends on
    /// the panel padding and has no pager; only one that overflows gives the
    /// pager band its room — the same rule [`Self::preferred_height`] sizes
    /// the panel by, so the two never leave a blank band or draw the pager
    /// over the last row.
    pub fn visible_rows(&self, total: usize) -> usize {
        if total <= self.rows_that_fit(qs::PANEL_PAD) {
            return total.max(1);
        }
        self.rows_that_fit(self.footer_reserve()).max(1)
    }
    /// The height the centre wants to show `rows` of `total`: the pager band
    /// is reserved only when there is something to page to. An empty centre
    /// keeps the drill-in's one-row band for its message.
    pub fn preferred_height(rows: usize, total: usize, metrics: impl Into<CenterMetrics>) -> f64 {
        let l = Self::new(Rect::ZERO, metrics);
        if rows == 0 {
            return DrillInList::list_height(l.header_height() + qs::RELATED_GAP, 0)
                .max(l.rows_top() + l.caption_lh + qs::PANEL_PAD);
        }
        let foot = if total > rows {
            l.footer_reserve()
        } else {
            qs::PANEL_PAD
        };
        l.rows_top()
            + usize_to_f64_lossy(rows) * l.row_h()
            // `rows` is non-zero past the early return, so this never saturates.
            + usize_to_f64_lossy(rows.saturating_sub(1)) * qs::RELATED_GAP
            + foot
    }
    /// Trailing icon buttons of a row: dismiss on the trailing edge,
    /// hand-to-agent one related step inboard of it. They sit under the
    /// source/time line so the relative time keeps its place.
    pub(super) fn reading_viewport(&self) -> Rect {
        Rect::new(
            self.panel.x0 + qs::PANEL_PAD,
            self.panel.y0 + self.header_height() + qs::RELATED_GAP,
            self.panel.x1 - qs::PANEL_PAD,
            self.panel.y1 - qs::PANEL_PAD,
        )
    }

    fn read_control(&self, row: Rect, with_agent: bool) -> Rect {
        let (dismiss, agent) = self.row_controls(row, with_agent);
        let end = agent.unwrap_or(dismiss).x0 - qs::RELATED_GAP;
        Rect::new(end - self.actions.read_width, dismiss.y0, end, dismiss.y1)
    }

    pub fn row_controls(&self, row: Rect, with_agent: bool) -> (Rect, Option<Rect>) {
        let top = row.y0 + qs::CONTROL_PAD + self.caption_lh;
        let room = (row.y1 - top).max(0.0);
        let size = self.actions.height.min(room);
        let y0 = top + (room - size) / 2.0;
        let x1 = row.x1 - qs::RELATED_GAP;
        let dismiss = Rect::new(x1 - size, y0, x1, y0 + size);
        let agent = with_agent.then(|| {
            let x1 = dismiss.x0 - qs::RELATED_GAP;
            Rect::new(x1 - size, y0, x1, y0 + size)
        });
        (dismiss, agent)
    }
}

/// A trailing icon button on a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowControl {
    Dismiss,
    HandToAgent,
    Read,
}

/// What an input asks of the host beyond what the centre did itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CenterOutcome {
    /// The input was not the centre's.
    Ignored,
    /// The centre acted on it.
    Handled,
    /// The person pressed Back.
    Back,
    /// A retained notification with no live sender asks to open its app.
    OpenApp { app_id: String, label: String },
}

/// Viewer-local state of one notification centre.
#[derive(Debug, Default)]
pub struct NotificationCenter {
    /// First visible row, newest first.
    start: usize,
    /// Touchpad movement not yet amounting to a whole row.
    wheel_remainder: f64,
    /// Keyboard focus on a header action: `Some(false)` Clear all,
    /// `Some(true)` Undo.
    action_focus: Option<bool>,
    back_focused: bool,
    /// The keyboard-selected row, by identity so it survives re-derivation.
    selected: Option<RowIdentity>,
    /// The selected row's focused trailing control.
    row_control: Option<RowControl>,
    /// App groups the viewer folded.
    collapsed: BTreeSet<String>,
    reader: Option<NotificationReader>,
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return from reading without invoking or dismissing the notification.
    pub fn close_reader(&mut self) -> bool {
        self.reader.take().is_some()
    }

    pub fn is_reading(&self) -> bool {
        self.reader.is_some()
    }

    fn reading_notification(&self, host: &dyn NotificationHost) -> Option<Notification> {
        let id = self.reader.as_ref()?.id;
        host.log().history().iter().find(|n| n.id == id).cloned()
    }

    /// Shared scrollbar pointer, wheel and keyboard handling.
    pub fn reader_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        self.reader
            .as_mut()
            .is_some_and(|reader| reader.event(event, ctx))
    }

    pub fn reading_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.reader.as_ref()?.scroll()
    }

    pub fn apply_reading_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.reader
            .as_mut()
            .map_or(ui::widget::PrimaryScrollResult::Unsupported, |reader| {
                reader.apply_scroll(action)
            })
    }

    /// The centre's rows, newest first, honouring the viewer's folded groups.
    pub fn rows(&self, host: &dyn NotificationHost) -> Vec<HistoryRow> {
        host.log().rows(&self.collapsed)
    }

    /// The panel height the centre asks for over `log` at `type_lines`.
    pub fn preferred_height(
        &self,
        log: &NotificationLog,
        metrics: impl Into<CenterMetrics>,
    ) -> f64 {
        let metrics = metrics.into();
        let total = if self.is_reading() {
            PREFERRED_ROWS
        } else {
            log.rows(&self.collapsed).len()
        };
        let height = CenterLayout::preferred_height(total.min(PREFERRED_ROWS), total, metrics);
        if log.can_undo_clear() {
            height + CenterLayout::new(Rect::ZERO, metrics).footer_reserve()
        } else {
            height
        }
    }

    /// Resolve the existing history/Undo budget at the host's actual width.
    pub fn preferred_height_for_width(
        &self,
        log: &NotificationLog,
        metrics: impl Into<CenterMetrics>,
        width: f64,
    ) -> f64 {
        let metrics = metrics.into();
        let layout = CenterLayout::new(Rect::new(0.0, 0.0, width, 0.0), metrics);
        self.preferred_height(log, metrics) + layout.header_height()
            - CenterLayout::new(Rect::ZERO, metrics).header_height()
            + if log.can_undo_clear() {
                layout.cleared_extra_height()
            } else {
                0.0
            }
    }

    /// The first visible row, newest first.
    pub fn first_visible_row(&self) -> usize {
        self.start
    }

    /// Scroll so `row` is the first visible one; the next paint or input
    /// clamps it to the rows there are.
    pub fn set_first_visible_row(&mut self, row: usize) {
        self.start = row;
    }

    /// Complete rows the panel shows now.
    pub fn visible_rows(&self, host: &dyn NotificationHost, layout: &CenterLayout) -> usize {
        layout.visible_rows(self.rows(host).len())
    }

    fn selected_row(&self, rows: &[HistoryRow], history: &[Notification]) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        rows.iter()
            .position(|row| row.identity(history).as_ref() == Some(selected))
    }

    /// Keep the window valid when records expire, are cleared or the panel
    /// shrinks. Returns whether the position moved.
    fn clamp_start(&mut self, layout: &CenterLayout, total: usize) -> bool {
        let max_start = total.saturating_sub(layout.visible_rows(total));
        let next = self.start.min(max_start);
        let changed = next != self.start;
        self.start = next;
        changed
    }

    /// Move through the rows newest first. Positive movement reveals older
    /// rows. Row-based, so wheels and keys land identically.
    fn scroll(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, movement: isize) {
        let total = self.rows(host).len();
        let max_start = total.saturating_sub(layout.visible_rows(total));
        let current = self.start.min(max_start);
        let next = if movement.is_negative() {
            current.saturating_sub(movement.unsigned_abs())
        } else {
            current.saturating_add(movement.unsigned_abs())
        }
        .min(max_start);
        if next != self.start {
            self.start = next;
            host.changed();
        }
    }

    /// Scroll from a pointer wheel over the panel. Pixel deltas accumulate
    /// into 48px row steps; line deltas name rows and apply immediately. A
    /// small touchpad movement stays the centre's until it makes a whole
    /// row, rather than scrolling whatever is under the panel.
    pub fn wheel(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        delta: MouseScrollDelta,
        scale_factor: f64,
    ) {
        if self.is_reading() {
            return;
        }
        let rows = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(y),
            MouseScrollDelta::PixelDelta(position) => -position.y / (48.0 * scale_factor.max(0.1)),
        };
        if rows.abs() <= f64::EPSILON {
            return;
        }
        self.wheel_remainder += rows;
        // `trunc` preserves direction.
        let whole = self.wheel_remainder.trunc();
        if whole.abs() >= 1.0 {
            self.wheel_remainder -= whole;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a wheel delta is bounded by input hardware; this only converts complete row movement"
            )]
            let movement = whole as isize;
            self.scroll(host, layout, movement);
        }
    }

    /// Select row `index` (clamped) and scroll it into view.
    fn select_row(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, index: usize) {
        let rows = self.rows(host);
        let Some(last) = rows.len().checked_sub(1) else {
            self.selected = None;
            return;
        };
        let index = index.min(last);
        let identity = rows
            .get(index)
            .and_then(|row| row.identity(host.log().history()));
        if identity != self.selected {
            self.row_control = None;
        }
        self.selected = identity;
        self.back_focused = false;
        self.action_focus = None;
        let visible = layout.visible_rows(rows.len()).max(1);
        if index < self.start {
            self.start = index;
        } else if index >= self.start.saturating_add(visible) {
            self.start = index.saturating_add(1).saturating_sub(visible);
        }
        host.changed();
    }

    /// Move the selection by `delta` rows. With nothing selected the first
    /// press selects the first visible row.
    fn move_selection(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, delta: isize) {
        let rows = self.rows(host);
        let current = self.selected_row(&rows, host.log().history());
        let next = match current {
            None => self.start,
            Some(index) if delta.is_negative() => index.saturating_sub(delta.unsigned_abs()),
            Some(index) => index.saturating_add(delta.unsigned_abs()),
        };
        self.select_row(host, layout, next);
    }

    /// The agent the notification behind `row` may be handed to.
    fn row_agent(host: &dyn NotificationHost, row: &HistoryRow) -> Option<String> {
        match row {
            HistoryRow::Item { index, .. } => host.agent_for(host.log().history().get(*index)?),
            HistoryRow::Group { .. } => None,
        }
    }

    /// The row (and control) under `p` among the visible rows.
    fn row_hit(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        p: Point,
    ) -> Option<(usize, Option<RowControl>)> {
        let rows = self.rows(host);
        let visible = layout.visible_rows(rows.len());
        for (offset, row) in rows.iter().enumerate().skip(self.start).take(visible) {
            let rect = layout.row(offset.saturating_sub(self.start));
            if !rect.contains(p) {
                continue;
            }
            let with_agent = Self::row_agent(host, row).is_some();
            let (dismiss, agent) = layout.row_controls(rect, with_agent);
            let control = if dismiss.contains(p) {
                Some(RowControl::Dismiss)
            } else if agent.is_some_and(|agent| agent.contains(p)) {
                Some(RowControl::HandToAgent)
            } else if matches!(row, HistoryRow::Item { .. })
                && layout.read_control(rect, with_agent).contains(p)
            {
                Some(RowControl::Read)
            } else {
                None
            };
            return Some((offset, control));
        }
        None
    }

    /// Act on row `index`: a group header folds or unfolds; a notification
    /// invokes its default action, or the given trailing control.
    fn activate_row(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        index: usize,
        control: Option<RowControl>,
    ) -> CenterOutcome {
        let rows = self.rows(host);
        let Some(row) = rows.get(index).cloned() else {
            return CenterOutcome::Handled;
        };
        match (row, control) {
            (_, Some(RowControl::Dismiss)) => self.dismiss_row(host, layout, index),
            (HistoryRow::Group { key, .. }, _) => {
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key.clone());
                }
                self.selected = Some(RowIdentity::Group(key));
                host.changed();
            }
            (HistoryRow::Item { index: at, .. }, Some(RowControl::Read)) => {
                let id = host
                    .log()
                    .history()
                    .get(at)
                    .map(|notification| notification.id);
                if let Some(id) = id {
                    self.selected = Some(RowIdentity::Item(id));
                    self.row_control = Some(RowControl::Read);
                    self.back_focused = false;
                    self.action_focus = None;
                    self.reader = Some(NotificationReader::new(id));
                    host.changed();
                }
            }
            (HistoryRow::Item { index: at, .. }, Some(RowControl::HandToAgent)) => {
                let notification = host.log().history().get(at).cloned();
                if let Some(notification) = notification
                    && host.agent_for(&notification).is_some()
                {
                    host.hand_to_agent(&notification);
                }
            }
            (HistoryRow::Item { index: at, .. }, None) => {
                let id = host.log().history().get(at).map(|n| n.id);
                let Some(id) = id else {
                    return CenterOutcome::Handled;
                };
                let outcome = host.activate_history(id);
                if matches!(
                    outcome,
                    HistoryActivation::Invoked | HistoryActivation::OpenApp { .. }
                ) {
                    self.reselect_after_removal(host, layout, index);
                }
                if let HistoryActivation::OpenApp { app_id, label } = outcome {
                    return CenterOutcome::OpenApp { app_id, label };
                }
            }
        }
        CenterOutcome::Handled
    }

    /// Dismiss row `index`: one notification, or every notification of a
    /// group header.
    fn dismiss_row(&mut self, host: &dyn NotificationHost, layout: &CenterLayout, index: usize) {
        let rows = self.rows(host);
        match rows.get(index) {
            Some(HistoryRow::Group { key, .. }) => {
                host.dismiss_history_group(key);
                self.collapsed.remove(key);
            }
            Some(HistoryRow::Item { index: at, .. }) => {
                let id = host.log().history().get(*at).map(|n| n.id);
                if let Some(id) = id {
                    host.dismiss_history(id);
                }
            }
            None => return,
        }
        self.reselect_after_removal(host, layout, index);
    }

    /// Keep a keyboard selection on the row that slid into the removed
    /// row's place, so repeated Delete walks down the list.
    fn reselect_after_removal(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        index: usize,
    ) {
        if self.selected.is_none() {
            return;
        }
        if self.rows(host).is_empty() {
            self.selected = None;
            self.row_control = None;
            return;
        }
        self.select_row(host, layout, index);
    }

    fn clear(&mut self, host: &dyn NotificationHost) {
        host.clear_history();
        self.start = 0;
        self.wheel_remainder = 0.0;
    }

    /// A press at `p` inside the panel.
    pub fn click(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        p: Point,
    ) -> CenterOutcome {
        if self.is_reading() {
            if self.reading_notification(host).is_none() || layout.back().contains(p) {
                self.close_reader();
                host.changed();
            }
            return CenterOutcome::Handled;
        }
        if layout.back().contains(p) {
            return CenterOutcome::Back;
        }
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        if layout.undo().contains(p) && can_undo {
            host.undo_clear();
            self.back_focused = false;
            self.action_focus = None;
            self.selected = None;
            self.row_control = None;
        } else if layout.clear().contains(p) && !history_empty {
            self.clear(host);
            self.back_focused = false;
            self.action_focus = Some(true);
            self.selected = None;
            self.row_control = None;
        } else if let Some((row, control)) = self.row_hit(host, layout, p) {
            // A pointer press acts on the row without leaving a keyboard
            // selection ring behind it.
            self.selected = None;
            self.row_control = None;
            self.back_focused = false;
            self.action_focus = None;
            return self.activate_row(host, layout, row, control);
        } else {
            return CenterOutcome::Ignored;
        }
        CenterOutcome::Handled
    }

    /// Keyboard navigation, available without pointer hover so the retained
    /// history stays usable by keyboard and assistive-technology users.
    /// Consumes at either edge too, so arrows never start moving whatever is
    /// under the panel once the list has reached its beginning or end.
    fn reading_key(&mut self, host: &dyn NotificationHost, logical_key: &Key) -> bool {
        if let Some(reader) = self.reader.as_mut() {
            if matches!(logical_key, Key::Named(NamedKey::Tab)) {
                reader.toggle_focus();
                host.changed();
                return true;
            }
            if reader.key(logical_key) {
                host.changed();
            }
            let back = matches!(logical_key, Key::Named(NamedKey::Escape))
                || (matches!(logical_key, Key::Named(NamedKey::Enter | NamedKey::Space))
                    && reader.activate_back());
            if back || self.reading_notification(host).is_none() {
                self.close_reader();
                host.changed();
            }
            return true;
        }
        false
    }

    fn row_focus_order(host: &dyn NotificationHost, row: &HistoryRow) -> Vec<Option<RowControl>> {
        let mut order = vec![None];
        if matches!(row, HistoryRow::Item { .. }) {
            order.push(Some(RowControl::Read));
        }
        order.push(Some(RowControl::Dismiss));
        if Self::row_agent(host, row).is_some() {
            order.push(Some(RowControl::HandToAgent));
        }
        order
    }

    fn cycle_row_focus(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        backwards: bool,
    ) {
        let rows = self.rows(host);
        let Some(index) = self.selected_row(&rows, host.log().history()) else {
            self.select_row(host, layout, self.start);
            return;
        };
        let Some(row) = rows.get(index) else {
            return;
        };
        let order = Self::row_focus_order(host, row);
        let current = order
            .iter()
            .position(|control| *control == self.row_control)
            .unwrap_or(0);
        let next = if backwards {
            current.checked_sub(1)
        } else {
            current.checked_add(1).filter(|next| *next < order.len())
        };
        if let Some(next) = next {
            self.row_control = order.get(next).copied().flatten();
        } else {
            let row = if backwards {
                index
                    .checked_sub(1)
                    .unwrap_or_else(|| rows.len().saturating_sub(1))
            } else {
                index.saturating_add(1).checked_rem(rows.len()).unwrap_or(0)
            };
            self.select_row(host, layout, row);
            if backwards {
                self.row_control = rows
                    .get(row)
                    .and_then(|row| Self::row_focus_order(host, row).last().copied())
                    .flatten();
            }
        }
        self.back_focused = false;
        self.action_focus = None;
        host.changed();
    }

    pub fn key(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        logical_key: &Key,
    ) -> CenterOutcome {
        self.key_with_shift(host, layout, logical_key, false)
    }

    pub fn key_with_shift(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        logical_key: &Key,
        backwards: bool,
    ) -> CenterOutcome {
        if self.reading_key(host, logical_key) {
            return CenterOutcome::Handled;
        }
        let rows = self.rows(host);
        let total = rows.len();
        let selected = self.selected_row(&rows, host.log().history());
        let can_undo = host.log().can_undo_clear();
        let page = isize::try_from(layout.visible_rows(total)).unwrap_or(isize::MAX);
        let mut outcome = CenterOutcome::Handled;
        match logical_key {
            Key::Named(NamedKey::Escape) => return CenterOutcome::Back,
            Key::Named(NamedKey::Enter | NamedKey::Space) if self.back_focused => {
                return CenterOutcome::Back;
            }
            Key::Named(NamedKey::Enter | NamedKey::Space)
                if self.action_focus == Some(false) && total > 0 =>
            {
                host.clear_history();
                self.back_focused = false;
                self.action_focus = Some(true);
                self.start = 0;
                self.selected = None;
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if selected.is_some() => {
                if let Some(index) = selected {
                    let control = self.row_control;
                    outcome = self.activate_row(host, layout, index, control);
                }
            }
            Key::Named(NamedKey::Delete) if selected.is_some() => {
                if let Some(index) = selected {
                    self.dismiss_row(host, layout, index);
                }
            }
            Key::Named(direction @ (NamedKey::ArrowRight | NamedKey::ArrowLeft))
                if selected.is_some_and(|index| {
                    matches!(rows.get(index), Some(HistoryRow::Group { expanded, .. })
                        if *expanded == (*direction == NamedKey::ArrowLeft))
                }) =>
            {
                if let Some(index) = selected {
                    outcome = self.activate_row(host, layout, index, None);
                }
            }
            Key::Named(NamedKey::ArrowRight) if selected.is_some() => {
                if selected
                    .is_some_and(|index| matches!(rows.get(index), Some(HistoryRow::Item { .. })))
                {
                    self.row_control = Some(RowControl::Read);
                    host.changed();
                }
            }
            Key::Named(NamedKey::ArrowLeft) if selected.is_some() => {
                self.row_control = None;
                host.changed();
            }
            Key::Named(NamedKey::Tab) => self.cycle_row_focus(host, layout, backwards),
            Key::Named(NamedKey::ArrowDown) => self.move_selection(host, layout, 1),
            Key::Named(NamedKey::ArrowUp) => self.move_selection(host, layout, -1),
            Key::Named(NamedKey::PageDown) if selected.is_some() => {
                self.move_selection(host, layout, page);
            }
            Key::Named(NamedKey::PageUp) if selected.is_some() => {
                self.move_selection(host, layout, page.saturating_neg());
            }
            Key::Named(NamedKey::Home) if selected.is_some() => self.select_row(host, layout, 0),
            Key::Named(NamedKey::End) if selected.is_some() => {
                self.select_row(host, layout, usize::MAX);
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if can_undo => {
                host.undo_clear();
                self.back_focused = false;
                self.action_focus = None;
            }
            Key::Named(NamedKey::Delete) if total > 0 => self.clear(host),
            Key::Named(NamedKey::PageDown) => self.scroll(host, layout, page),
            Key::Named(NamedKey::PageUp) => self.scroll(host, layout, page.saturating_neg()),
            Key::Named(NamedKey::Home) => self.scroll(host, layout, -isize::MAX),
            Key::Named(NamedKey::End) => self.scroll(host, layout, isize::MAX),
            _ => return CenterOutcome::Ignored,
        }
        let total = self.rows(host).len();
        let _ = self.clamp_start(layout, total);
        outcome
    }

    /// `tilo-control list-targets` names for the centre's header chips, rows
    /// and their trailing controls, under `prefix`.
    pub fn semantic_rects(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        prefix: &str,
    ) -> Vec<(String, Rect)> {
        if self.is_reading() {
            return vec![
                (format!("{prefix}notifications_back"), layout.back()),
                (
                    format!("{prefix}notification_reading"),
                    layout.reading_viewport(),
                ),
            ];
        }
        let rows = self.rows(host);
        let visible = layout.visible_rows(rows.len());
        let mut out = vec![(format!("{prefix}notifications_back"), layout.back())];
        if !host.log().history().is_empty() {
            out.push((format!("{prefix}notifications_clear"), layout.clear()));
        }
        for (offset, row) in rows.iter().enumerate().skip(self.start).take(visible) {
            let rect = layout.row(offset.saturating_sub(self.start));
            let agent = Self::row_agent(host, row);
            let (dismiss, agent_rect) = layout.row_controls(rect, agent.is_some());
            out.push((format!("{prefix}notification_row[{offset}]"), rect));
            out.push((format!("{prefix}notification_dismiss[{offset}]"), dismiss));
            if matches!(row, HistoryRow::Item { .. }) {
                out.push((
                    format!("{prefix}notification_read[{offset}]"),
                    layout.read_control(rect, agent.is_some()),
                ));
            }
            if let Some(agent_rect) = agent_rect {
                out.push((format!("{prefix}notification_agent[{offset}]"), agent_rect));
            }
        }
        out
    }

    /// The centre as one modal dialog: the row list, Clear all, and while
    /// it can still be undone, the cleared status and Undo. `now` is one
    /// clock edge for every row, so adjacent rows cannot label their ages
    /// across a relative-time boundary.
    pub fn accessibility(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        now: Instant,
    ) -> AccessNode {
        if let (Some(reader), Some(notification)) =
            (self.reader.as_ref(), self.reading_notification(host))
        {
            return reader.accessibility(&notification, layout);
        }
        let total = self.rows(host).len();
        let visible = layout.visible_rows(total);
        let start = self.start.min(total.saturating_sub(visible));
        let shown = visible.min(total.saturating_sub(start));
        let description = if total == 0 {
            t("a11y-notifications-none")
        } else {
            t_args(
                "a11y-notifications-description",
                &[
                    ("first", start.saturating_add(1).into()),
                    ("last", start.saturating_add(shown).into()),
                    ("total", total.into()),
                ],
            )
        };
        let history = AccessNode::new(Role::List)
            .with_name(t("a11y-notifications-list"))
            .with_children(self.accessibility_rows(host, layout, now));
        let mut children = vec![history];
        if total > 0 {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(t("a11y-notifications-clear"))
                    .with_description(t("a11y-notifications-clear-description"))
                    .with_bounds(layout.clear())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.action_focus == Some(false)),
                    ),
            );
        }
        if host.log().can_undo_clear() {
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(t("a11y-notifications-cleared"))
                    .with_live(ui::a11y::Politeness::Polite),
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(t("a11y-notifications-undo"))
                    .with_description(t("a11y-notifications-undo-description"))
                    .with_bounds(layout.undo())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.action_focus == Some(true)),
                    ),
            );
        }
        children.push(
            AccessNode::new(Role::Button)
                .with_name(t("drill-in-back"))
                .with_bounds(layout.back())
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.back_focused),
                ),
        );
        AccessNode::new(Role::Dialog)
            .with_name(t("a11y-notifications"))
            .with_description(description)
            .with_state(AccessState::new().modal(true))
            .with_children(children)
    }

    /// Appended after existing children so row, Clear and Undo paths stay stable.
    fn back_path_index(host: &dyn NotificationHost) -> usize {
        1usize
            .saturating_add(usize::from(!host.log().history().is_empty()))
            .saturating_add(if host.log().can_undo_clear() { 2 } else { 0 })
    }

    /// The accessibility path index of Undo inside the dialog.
    fn undo_path_index(host: &dyn NotificationHost) -> usize {
        if host.log().history().is_empty() {
            2
        } else {
            3
        }
    }

    /// Bounds of the dialog node at `path` (relative to the dialog).
    pub fn accessibility_rect_for_path(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        path: &[usize],
    ) -> Option<Rect> {
        if self.is_reading() {
            return match path {
                [] => Some(layout.panel()),
                [0] => Some(layout.back()),
                [1] => Some(layout.reading_viewport()),
                _ => None,
            };
        }
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        match path {
            [] => Some(layout.panel()),
            [index] if *index == Self::back_path_index(host) => Some(layout.back()),
            [0, row, rest @ ..] => {
                let items = self.accessibility_rows(host, layout, ui::time::now());
                let item = items.get(*row)?;
                let node = match rest {
                    [] => item,
                    [control] => item.children.get(*control)?,
                    _ => return None,
                };
                node.bounds.map(ui::a11y::AccessBounds::to_rect)
            }
            [1] if !history_empty => Some(layout.clear()),
            [index] if can_undo && *index == Self::undo_path_index(host) => Some(layout.undo()),
            _ => None,
        }
    }

    /// Move keyboard focus to the dialog node at `path` (relative to the
    /// dialog). Returns whether the path named something focusable.
    pub fn focus_accessibility_path(
        &mut self,
        host: &dyn NotificationHost,
        path: &[usize],
    ) -> bool {
        if let Some(reader) = self.reader.as_mut() {
            let focused = reader.focus(path);
            if focused {
                host.changed();
            }
            return focused;
        }
        if path == [Self::back_path_index(host)] {
            self.action_focus = None;
            self.selected = None;
            self.row_control = None;
            self.back_focused = true;
            host.changed();
            return true;
        }
        if let [0, row, rest @ ..] = path {
            let rows = self.rows(host);
            let index = self.start.saturating_add(*row);
            let identity = rows
                .get(index)
                .and_then(|row| row.identity(host.log().history()));
            let Some(row) = rows.get(index) else {
                return false;
            };
            let has_agent = Self::row_agent(host, row).is_some();
            let control = match rest {
                [] => None,
                [0] => Some(RowControl::Dismiss),
                [1] if has_agent => Some(RowControl::HandToAgent),
                [at] if matches!(row, HistoryRow::Item { .. })
                    && *at == if has_agent { 2 } else { 1 } =>
                {
                    Some(RowControl::Read)
                }
                _ => return false,
            };
            let Some(identity) = identity else {
                return false;
            };
            self.selected = Some(identity);
            self.row_control = control;
            self.back_focused = false;
            self.action_focus = None;
            host.changed();
            return true;
        }
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        let undo = match path {
            [1] if !history_empty => false,
            [index] if can_undo && *index == Self::undo_path_index(host) => true,
            _ => return false,
        };
        self.back_focused = false;
        self.action_focus = Some(undo);
        self.selected = None;
        host.changed();
        true
    }

    /// One accessibility node per visible row, matching the painted rows
    /// one-to-one so a focus path names the row it lights. Group headers are
    /// level-one items with an expanded state; their notifications are
    /// level two.
    fn accessibility_rows(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        now: Instant,
    ) -> Vec<AccessNode> {
        let rows = self.rows(host);
        let visible = layout.visible_rows(rows.len());
        let start = self.start.min(rows.len().saturating_sub(visible));
        let shown = visible.min(rows.len().saturating_sub(start));
        let log = host.log();
        let history = log.history();
        let selected = self.selected_row(&rows, history);
        rows.iter()
            .enumerate()
            .skip(start)
            .take(shown)
            .map(|(offset, row)| {
                let position = offset.saturating_sub(start);
                let rect = layout.row(position);
                let is_selected = selected == Some(offset);
                let row_focused = is_selected && self.row_control.is_none();
                let node = match row {
                    HistoryRow::Group {
                        key,
                        label,
                        count,
                        expanded,
                        ..
                    } => {
                        let (dismiss, _) = layout.row_controls(rect, false);
                        AccessNode::new(Role::ListItem)
                            .with_semantic_key(format!("notification-group:{key}"))
                            .with_name(label.clone())
                            .with_description(t_args(
                                "notification-center-group-description",
                                &[
                                    ("count", (*count).into()),
                                    ("expanded", if *expanded { "yes" } else { "no" }.into()),
                                ],
                            ))
                            .with_level(1)
                            .with_state(
                                AccessState::new()
                                    .expanded(*expanded)
                                    .focusable(true)
                                    .focused(row_focused)
                                    .selected(is_selected),
                            )
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name(t_args(
                                        "notification-center-dismiss-all",
                                        &[("app", label.as_str().into())],
                                    ))
                                    .with_bounds(dismiss)
                                    .with_state(AccessState::new().focusable(true).focused(
                                        is_selected
                                            && self.row_control == Some(RowControl::Dismiss),
                                    )),
                            ])
                    }
                    HistoryRow::Item { index, grouped } => match history.get(*index) {
                        Some(notification) => self.item_node(
                            host,
                            layout,
                            rect,
                            notification,
                            (*grouped, is_selected),
                            now,
                        ),
                        None => AccessNode::new(Role::ListItem),
                    },
                };
                node.with_position(position.saturating_add(1), shown)
                    .with_bounds(rect)
            })
            .collect()
    }

    /// The accessibility node of one retained notification row and its
    /// trailing controls.
    fn item_node(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        rect: Rect,
        notification: &Notification,
        (grouped, is_selected): (bool, bool),
        now: Instant,
    ) -> AccessNode {
        let row_focused = is_selected && self.row_control.is_none();
        let agent = host.agent_for(notification);
        let (dismiss, agent_rect) = layout.row_controls(rect, agent.is_some());
        let control_focused = |control| is_selected && self.row_control == Some(control);
        let mut controls = vec![
            AccessNode::new(Role::Button)
                .with_name(t("notification-center-dismiss"))
                .with_bounds(dismiss)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(control_focused(RowControl::Dismiss)),
                ),
        ];
        if let (Some(agent), Some(agent_rect)) = (agent, agent_rect) {
            controls.push(
                AccessNode::new(Role::Button)
                    .with_name(t_args(
                        "notification-center-hand-to",
                        &[("agent", agent.as_str().into())],
                    ))
                    .with_description(t("notification-center-hand-to-description"))
                    .with_bounds(agent_rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(control_focused(RowControl::HandToAgent)),
                    ),
            );
        }
        controls.push(
            AccessNode::new(Role::Button)
                .with_name(t("notification-center-read-description"))
                .with_bounds(layout.read_control(rect, agent_rect.is_some()))
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(control_focused(RowControl::Read)),
                ),
        );
        AccessNode::new(Role::ListItem)
            .with_semantic_key(format!("notification:{}", notification.id))
            .with_name(notification.title.clone())
            .with_description(accessible_description(notification, now))
            .with_level(if grouped { 2 } else { 1 })
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(row_focused)
                    .selected(is_selected),
            )
            .with_children(controls)
    }

    /// Paint the centre into `layout`'s panel. The host paints the panel
    /// surface itself; `cursor` lights the hovered controls.
    #[allow(
        clippy::too_many_lines,
        reason = "the render handles, the host, the layout and the pointer are each independent inputs; paint and hit rectangles share one layout pass so they cannot drift"
    )]
    pub fn draw(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
    ) {
        if let Some(notification) = self.reading_notification(host)
            && let Some(reader) = self.reader.as_mut()
        {
            reader.draw(
                &notification,
                layout,
                scene,
                transform,
                text_renderer,
                theme,
                cursor,
            );
            return;
        }
        self.reader = None;
        let rect = layout.panel();
        let colors = &theme.tokens.colors;
        let caption_style = theme.text(TextRole::Caption);
        let label_style = theme.text(TextRole::Label);
        Self::chip(
            scene,
            transform,
            text_renderer,
            theme,
            (layout.back(), cursor, self.back_focused),
            &t("drill-in-back"),
            ButtonRole::Plain,
        );
        let title_style = label_style.clone().with_weight(TextWeight::Semibold);
        let title_rect = layout.title_rect();
        let title_width = f64_to_f32_lossy(title_rect.width().max(1.0));
        let title = preview_line(
            text_renderer,
            &t("notification-center-title"),
            title_width,
            &title_style,
        );
        let (title_w, title_h) = text_renderer.measure_style(&title, None, &title_style);
        text_renderer.draw_style(
            scene,
            &title,
            colors.text_primary,
            transform.pre_translate(
                (
                    title_rect.center().x - f64::from(title_w) / 2.0,
                    title_rect.center().y - f64::from(title_h) / 2.0,
                )
                    .into(),
            ),
            Some(title_width),
            &title_style,
        );
        let rows = self.rows(host);
        let log = host.log();
        let history = log.history();
        let can_undo = log.can_undo_clear();
        if history.is_empty() {
            self.start = 0;
            DrillInList::draw_empty_from(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                &t("notification-center-empty"),
                layout.position_y() - rect.y0,
            );
            if can_undo {
                Self::draw_cleared_band(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    layout,
                    (cursor, self.action_focus == Some(true)),
                );
            }
            return;
        }

        let visible_rows = layout.visible_rows(rows.len());
        let _ = self.clamp_start(layout, rows.len());
        let selected = self.selected_row(&rows, history);
        // Resizing or growing text must keep the focused row/control visible.
        // Retain the same identity and control, and only move the viewport.
        if let Some(index) = selected {
            if index < self.start {
                self.start = index;
            } else if index >= self.start.saturating_add(visible_rows) {
                self.start = index.saturating_add(1).saturating_sub(visible_rows);
            }
        }
        let start = self.start;
        let shown = visible_rows.min(rows.len().saturating_sub(start));

        Self::chip(
            scene,
            transform,
            text_renderer,
            theme,
            (layout.clear(), cursor, self.action_focus == Some(false)),
            &t("notification-center-clear-all"),
            ButtonRole::Plain,
        );
        let position = if rows.len() > visible_rows {
            t_args(
                "notification-center-position",
                &[
                    ("first", start.saturating_add(1).into()),
                    ("last", start.saturating_add(shown).into()),
                    ("total", rows.len().into()),
                ],
            )
        } else {
            t_args(
                "notification-center-count",
                &[("count", history.len().into())],
            )
        };
        text_renderer.draw_style(
            scene,
            &position,
            colors.text_dim,
            transform.pre_translate((rect.x0 + qs::PANEL_PAD, layout.position_y()).into()),
            Some(f64_to_f32_lossy(rect.width() - 2.0 * qs::PANEL_PAD)),
            &caption_style,
        );

        let now = ui::time::now();
        let hovered = |r: Rect| cursor.is_some_and(|p| r.contains(p));
        for (offset, history_row) in rows.iter().enumerate().skip(start).take(visible_rows) {
            let row = layout.row(offset.saturating_sub(start));
            let row_selected = selected == Some(offset);
            let focused_control = self.row_control.filter(|_| row_selected);
            let (notification, grouped) = match history_row {
                HistoryRow::Group {
                    label,
                    count,
                    expanded,
                    newest,
                    ..
                } => {
                    let Some(newest) = history.get(*newest) else {
                        continue;
                    };
                    ui::surface::card(
                        scene,
                        transform,
                        row,
                        qs::control_radius(theme, row),
                        qs::control_fill(theme, hovered(row)),
                    );
                    Self::draw_group_row(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        (layout, row),
                        (label, *count, *expanded, newest),
                        now,
                    );
                    Self::draw_row_controls(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        layout.row_controls(row, false),
                        (focused_control, cursor),
                    );
                    if row_selected && self.row_control.is_none() {
                        Self::draw_selection(scene, transform, theme, row);
                    }
                    continue;
                }
                HistoryRow::Item { index, grouped } => {
                    let Some(notification) = history.get(*index) else {
                        continue;
                    };
                    (notification, *grouped)
                }
            };
            let agent = host.agent_for(notification);
            let controls = layout.row_controls(row, agent.is_some());
            let read = layout.read_control(row, agent.is_some());
            let text_right = read.x0 - qs::RELATED_GAP;
            // A row is a resting Control surface (HIG §9.5): the lift floor
            // with its derived edge, never a ramp-slot wash.
            ui::surface::card(
                scene,
                transform,
                row,
                qs::control_radius(theme, row),
                qs::control_fill(theme, hovered(row)),
            );
            // Keep severity scannable without taking another line. The
            // accessibility description also names it, so colour is not the
            // only signal.
            let severity_rail = Rect::new(
                row.x0,
                row.y0 + qs::CONTROL_PAD,
                row.x0 + 3.0,
                row.y1 - qs::CONTROL_PAD,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                notification.severity.color(theme),
                None,
                &RoundedRect::from_rect(severity_rail, 1.5),
            );
            let source = source_label(notification);
            let age = relative_time(notification, now);
            let (time_width, _) = text_renderer.measure_style(&age, None, &caption_style);
            // Source/time, title, body: three measured lines on the control
            // padding, inset `s3` like every row glyph in the panel. A
            // grouped notification sits one glyph in, under its header's
            // label, so the fold reads as a hierarchy.
            let x = row.x0 + qs::CONTROL_INSET + if grouped { qs::GLYPH } else { 0.0 };
            let source_y = row.y0 + qs::CONTROL_PAD;
            let source_width = f64_to_f32_lossy(
                (row.x1 - qs::CONTROL_INSET - f64::from(time_width) - qs::RELATED_GAP - x).max(0.0),
            );
            let source = preview_line(text_renderer, &source, source_width, &caption_style);
            text_renderer.draw_style(
                scene,
                &source,
                colors.text_dim,
                transform.pre_translate((x, source_y).into()),
                Some(f64_to_f32_lossy(
                    row.x1 - qs::CONTROL_INSET - f64::from(time_width) - qs::RELATED_GAP - x,
                )),
                &caption_style,
            );
            text_renderer.draw_style(
                scene,
                &age,
                colors.text_dim,
                transform.pre_translate(
                    (row.x1 - qs::CONTROL_INSET - f64::from(time_width), source_y).into(),
                ),
                None,
                &caption_style,
            );
            let title_y = source_y + layout.caption_lh;
            let preview_width = f64_to_f32_lossy((text_right - x).max(0.0));
            let title_style = label_style.clone().with_weight(TextWeight::Semibold);
            let title = preview_line(
                text_renderer,
                &notification.title,
                preview_width,
                &title_style,
            );
            text_renderer.draw_style(
                scene,
                &title,
                colors.text_primary,
                transform.pre_translate((x, title_y).into()),
                Some(f64_to_f32_lossy(text_right - x)),
                &label_style.clone().with_weight(TextWeight::Semibold),
            );
            if notification.show_preview && !notification.body.is_empty() {
                let body = preview_line(
                    text_renderer,
                    &notification.body,
                    preview_width,
                    &caption_style,
                );
                text_renderer.draw_style(
                    scene,
                    &body,
                    colors.text_dim,
                    transform.pre_translate((x, title_y + layout.label_lh).into()),
                    Some(f64_to_f32_lossy(text_right - x)),
                    &caption_style,
                );
            }
            Self::draw_row_controls(
                scene,
                transform,
                text_renderer,
                theme,
                controls,
                (focused_control, cursor),
            );
            Self::chip(
                scene,
                transform,
                text_renderer,
                theme,
                (read, cursor, focused_control == Some(RowControl::Read)),
                &t("notification-center-read"),
                ButtonRole::Plain,
            );

            if row_selected && self.row_control.is_none() {
                Self::draw_selection(scene, transform, theme, row);
            }
        }

        if can_undo {
            Self::draw_cleared_band(
                scene,
                transform,
                text_renderer,
                theme,
                layout,
                (cursor, self.action_focus == Some(true)),
            );
        } else if rows.len() > visible_rows {
            let newer = if start > 0 {
                t("notification-center-newer")
            } else {
                t("notification-center-at-newest")
            };
            let older = if start.saturating_add(shown) < rows.len() {
                t("notification-center-older")
            } else {
                t("notification-center-at-oldest")
            };
            let footer = layout.footer();
            ui::surface::card(
                scene,
                transform,
                footer,
                qs::control_radius(theme, footer),
                qs::control_fill(theme, false),
            );
            let hint = format!("{newer}   \u{00b7}   {older}");
            let (hint_w, hint_h) = text_renderer.measure_style(&hint, None, &caption_style);
            text_renderer.draw_style(
                scene,
                &hint,
                colors.text_primary,
                transform.pre_translate(
                    (
                        footer.center().x - f64::from(hint_w) / 2.0,
                        footer.center().y - f64::from(hint_h) / 2.0,
                    )
                        .into(),
                ),
                Some(f64_to_f32_lossy(footer.width() - 2.0 * qs::CONTROL_INSET)),
                &caption_style,
            );
        }
    }

    /// The toolkit's button face, with the same role, hover and focus
    /// treatment as controls in retained widgets.
    fn chip(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (rect, cursor, focused): (Rect, Option<Point>, bool),
        label: &str,
        role: ButtonRole,
    ) {
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(rect, label)
                .role(role)
                .hovered(cursor.is_some_and(|point| rect.contains(point)))
                .focused(focused),
        );
    }

    /// The "Notifications cleared" band with its Undo button.
    fn draw_cleared_band(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &CenterLayout,
        (cursor, focused): (Option<Point>, bool),
    ) {
        let colors = &theme.tokens.colors;
        let style = theme.text(TextRole::Caption);
        let status = layout.cleared_band();
        scene.fill(
            Fill::NonZero,
            transform,
            colors.lift(0.35),
            None,
            &RoundedRect::from_rect(status, qs::control_radius(theme, status)),
        );
        let label = layout.cleared_label_rect();
        text_renderer.draw_style(
            scene,
            &t("notification-center-cleared"),
            colors.text_primary,
            transform.pre_translate((label.x0, label.y0).into()),
            Some(f64_to_f32_lossy(label.width().max(1.0))),
            &style,
        );
        Self::chip(
            scene,
            transform,
            text_renderer,
            theme,
            (layout.undo(), cursor, focused),
            &t("notification-center-undo"),
            ButtonRole::Secondary,
        );
    }

    /// One group header row: fold chevron, app label, count, the newest
    /// member's age and (while folded) its title.
    fn draw_group_row(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (layout, row): (&CenterLayout, Rect),
        (label, count, expanded, newest): (&str, usize, bool, &Notification),
        now: Instant,
    ) {
        let colors = &theme.tokens.colors;
        let caption_style = theme.text(TextRole::Caption);
        let label_style = theme.text(TextRole::Label);
        let glyph = qs::leading_glyph(row, qs::CONTROL_INSET, qs::GLYPH);
        ui::icons::draw_icon(
            scene,
            transform,
            glyph,
            if expanded {
                ui::icons::Icon::ChevronDown
            } else {
                ui::icons::Icon::ChevronRight
            },
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let (dismiss, _) = layout.row_controls(row, false);
        let x = glyph.x1 + qs::RELATED_GAP;
        let age = relative_time(newest, now);
        let (time_width, _) = text_renderer.measure_style(&age, None, &caption_style);
        let source_y = row.y0 + qs::CONTROL_PAD;
        let count_label = t_args(
            "notification-center-group-count",
            &[("count", count.into())],
        );
        text_renderer.draw_style(
            scene,
            &count_label,
            colors.text_dim,
            transform.pre_translate((x, source_y).into()),
            Some(f64_to_f32_lossy(
                row.x1 - qs::CONTROL_INSET - f64::from(time_width) - qs::RELATED_GAP - x,
            )),
            &caption_style,
        );
        text_renderer.draw_style(
            scene,
            &age,
            colors.text_dim,
            transform.pre_translate(
                (row.x1 - qs::CONTROL_INSET - f64::from(time_width), source_y).into(),
            ),
            None,
            &caption_style,
        );
        let text_w = f64_to_f32_lossy(dismiss.x0 - qs::RELATED_GAP - x);
        let title_y = source_y + layout.caption_lh;
        let title_style = label_style.with_weight(TextWeight::Semibold);
        let label = preview_line(text_renderer, label, text_w, &title_style);
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_primary,
            transform.pre_translate((x, title_y).into()),
            Some(text_w),
            &title_style,
        );
        if !expanded {
            let title = preview_line(text_renderer, &newest.title, text_w, &caption_style);
            text_renderer.draw_style(
                scene,
                &title,
                colors.text_dim,
                transform.pre_translate((x, title_y + layout.label_lh).into()),
                Some(text_w),
                &caption_style,
            );
        }
    }

    /// A row's trailing icon buttons as Controls, the hovered one on the
    /// hover tier and the keyboard-focused one ringed.
    fn draw_row_controls(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (dismiss, agent): (Rect, Option<Rect>),
        (focused, cursor): (Option<RowControl>, Option<Point>),
    ) {
        let mut paint = |rect: Rect, icon: ui::icons::Icon, control, name: &str| {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::icon_only(rect, icon, name)
                    .hovered(cursor.is_some_and(|point| rect.contains(point)))
                    .focused(focused == Some(control)),
            );
        };
        paint(
            dismiss,
            ui::icons::Icon::X,
            RowControl::Dismiss,
            &t("notification-center-dismiss"),
        );
        if let Some(agent) = agent {
            paint(
                agent,
                ui::icons::Icon::Send,
                RowControl::HandToAgent,
                &t("notification-center-hand-to-description"),
            );
        }
    }

    /// Ring the keyboard-selected row.
    fn draw_selection(scene: &mut Scene, transform: Affine, theme: &Theme, row: Rect) {
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            theme.border_focus,
            None,
            &RoundedRect::from_rect(row, qs::control_radius(theme, row)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::stack::tests::TestHost;
    use super::*;

    fn from_app(app: &str, title: &str) -> Notification {
        let mut notification = Notification::new(title, "");
        notification.app_id = Some(app.to_string());
        notification.app_name = Some(app.to_uppercase());
        notification
    }

    fn layout() -> CenterLayout {
        CenterLayout::new(Rect::new(0.0, 0.0, 360.0, 560.0), (12.0, 14.0))
    }

    #[test]
    fn keyboard_walks_folds_and_dismisses_rows() {
        let host = TestHost::default();
        for n in [
            from_app("mail", "m1"),
            from_app("mail", "m2"),
            Notification::new("sys", ""),
        ] {
            host.log.borrow_mut().retain(n);
        }
        let layout = layout();
        let mut center = NotificationCenter::new();
        let down = Key::Named(NamedKey::ArrowDown);
        assert_eq!(center.key(&host, &layout, &down), CenterOutcome::Handled);
        assert_eq!(
            center.selected,
            Some(RowIdentity::Item(host.log().history()[2].id))
        );
        center.key(&host, &layout, &down);
        assert!(matches!(center.selected, Some(RowIdentity::Group(_))));
        // Left folds the expanded group; its members leave the rows.
        center.key(&host, &layout, &Key::Named(NamedKey::ArrowLeft));
        assert_eq!(center.rows(&host).len(), 2);
        // Delete on the header dismisses the whole group.
        center.key(&host, &layout, &Key::Named(NamedKey::Delete));
        assert_eq!(host.log().history().len(), 1);
        assert_eq!(host.log().history()[0].title, "sys");
    }

    #[test]
    fn clear_all_is_undoable_from_the_keyboard_and_pointer() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(Notification::new("one", ""));
        let layout = layout();
        let mut center = NotificationCenter::new();
        let clear = layout.clear().center();
        assert_eq!(center.click(&host, &layout, clear), CenterOutcome::Handled);
        assert!(host.log().history().is_empty());
        assert!(host.log().can_undo_clear());
        let enter = Key::Named(NamedKey::Enter);
        assert_eq!(center.key(&host, &layout, &enter), CenterOutcome::Handled);
        assert_eq!(host.log().history().len(), 1);
        assert_eq!(
            center.click(&host, &layout, layout.back().center()),
            CenterOutcome::Back
        );
    }

    #[test]
    fn resize_and_large_text_keep_the_focused_dismiss_action_visible() {
        let host = TestHost::default();
        for index in 0..10 {
            host.log
                .borrow_mut()
                .retain(Notification::new(format!("notice {index}"), "body"));
        }
        let mut center = NotificationCenter::new();
        assert!(center.focus_accessibility_path(&host, &[0, 6, 0]));
        let selected = center.selected.clone();
        let theme = Theme::dark().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        let panel = Rect::new(0.0, 0.0, 560.0, 300.0);
        let layout = CenterLayout::new(panel, CenterLayout::metrics(&theme, &mut text));
        center.draw(
            &host,
            &layout,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            None,
        );
        assert_eq!(center.selected, selected);
        assert_eq!(center.row_control, Some(RowControl::Dismiss));
        assert!(center.first_visible_row() > 0);
        let dialog = center.accessibility(&host, &layout, ui::time::now());
        let dismiss = &dialog.children[0].children[0].children[0];
        let bounds = dismiss.bounds.map(ui::a11y::AccessBounds::to_rect).unwrap();
        assert!(bounds.y0 >= panel.y0 && bounds.y1 <= panel.y1);
        assert!(bounds.height() >= ButtonFace::height(ControlSize::Regular, &theme));
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Delete)),
            CenterOutcome::Handled
        );
        assert_eq!(host.log().history().len(), 9);
    }

    #[test]
    fn pointer_group_activation_releases_clear_all_focus() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(from_app("mail", "one"));
        host.log.borrow_mut().retain(from_app("mail", "two"));
        let layout = layout();
        let mut center = NotificationCenter::new();
        assert!(center.focus_accessibility_path(&host, &[1]));
        assert_eq!(center.action_focus, Some(false));
        // Fold the group with the pointer after focusing Clear all. Enter
        // must not invoke that old header focus and erase the history.
        center.click(&host, &layout, layout.row(0).center());
        assert_eq!(center.action_focus, None);
        assert_eq!(center.rows(&host).len(), 1);
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Enter)),
            CenterOutcome::Handled
        );
        assert_eq!(center.rows(&host).len(), 3);
        assert_eq!(host.log().history().len(), 2);
    }

    #[test]
    fn escape_leaves_clear_undo_available_and_repeated_clear_restores_history() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(Notification::new("one", ""));
        let layout = layout();
        let mut center = NotificationCenter::new();
        for _ in 0..2 {
            center.click(&host, &layout, layout.clear().center());
            assert!(host.log().history().is_empty());
            assert_eq!(center.action_focus, Some(true));
            assert_eq!(
                center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
                CenterOutcome::Back
            );
            assert!(host.log().can_undo_clear());
            let dialog = center.accessibility(&host, &layout, ui::time::now());
            assert_eq!(
                dialog.children[2]
                    .bounds
                    .map(ui::a11y::AccessBounds::to_rect),
                Some(layout.undo())
            );
            center.click(&host, &layout, layout.undo().center());
            assert_eq!(host.log().history().len(), 1);
            assert_eq!(center.action_focus, None);
            assert_eq!(center.row_control, None);
        }
    }

    #[test]
    fn opening_a_retained_notification_without_a_sender_opens_its_app() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(from_app("mail", "m1"));
        let layout = layout();
        let mut center = NotificationCenter::new();
        let outcome = center.click(&host, &layout, layout.row(0).center());
        assert_eq!(
            outcome,
            CenterOutcome::OpenApp {
                app_id: "mail".into(),
                label: "MAIL".into()
            }
        );
        assert!(host.log().history().is_empty());
    }

    #[test]
    fn accessibility_rows_match_the_painted_rows_and_focus_by_path() {
        let host = TestHost::default();
        host.log
            .borrow_mut()
            .retain(Notification::new("one", "body"));
        host.log.borrow_mut().retain(Notification::new("two", ""));
        let layout = layout();
        let mut center = NotificationCenter::new();
        let dialog = center.accessibility(&host, &layout, ui::time::now());
        assert_eq!(dialog.role, Role::Dialog);
        let list = &dialog.children[0];
        assert_eq!(list.children.len(), 2);
        assert_eq!(list.children[0].name.as_deref(), Some("two"));
        assert_eq!(
            center.accessibility_rect_for_path(&host, &layout, &[0, 1]),
            Some(layout.row(1))
        );
        assert!(center.focus_accessibility_path(&host, &[0, 1, 0]));
        assert_eq!(center.row_control, Some(RowControl::Dismiss));
        assert!(center.focus_accessibility_path(&host, &[1]));
        assert_eq!(center.action_focus, Some(false));
    }

    #[test]
    fn a_history_that_overflows_reserves_the_pager_and_scrolls_by_rows() {
        let host = TestHost::default();
        for i in 0..20 {
            host.log
                .borrow_mut()
                .retain(Notification::new(format!("n{i}"), ""));
        }
        let layout = layout();
        let mut center = NotificationCenter::new();
        let visible = center.visible_rows(&host, &layout);
        assert!(visible < 20);
        center.wheel(&host, &layout, MouseScrollDelta::LineDelta(0.0, -2.0), 1.0);
        assert_eq!(center.start, 2);
        center.key(&host, &layout, &Key::Named(NamedKey::End));
        assert_eq!(center.start, 20 - visible);
    }
    #[test]
    fn full_notification_text_has_a_visible_reading_control() {
        let host = TestHost::default();
        host.log.borrow_mut().retain(Notification::new(
            "Couldn't open Notes",
            "The app stopped before opening. Try again. Your document is safe.",
        ));
        let layout = layout();
        let center = NotificationCenter::new();
        let targets = center.semantic_rects(&host, &layout, "");
        let read = targets
            .iter()
            .find(|(name, _)| name == "notification_read[0]")
            .expect("a visible control opens complete notification text");
        assert!(layout.row(0).contains(read.1.center()));
        let tree = center.accessibility(&host, &layout, ui::time::now());
        assert_eq!(
            tree.children[0].children[0].children[1]
                .bounds
                .map(ui::a11y::AccessBounds::to_rect),
            Some(read.1)
        );
    }

    #[test]
    fn reading_preserves_text_identity_and_never_invokes_or_dismisses() {
        let host = TestHost::default();
        let body = "The app stopped before opening. Try again. Your document is safe.";
        host.log
            .borrow_mut()
            .retain(Notification::new("Couldn't open Notes", body));
        let layout = layout();
        let mut center = NotificationCenter::new();
        center.key(&host, &layout, &Key::Named(NamedKey::ArrowDown));
        center.key(&host, &layout, &Key::Named(NamedKey::ArrowRight));
        assert_eq!(center.row_control, Some(RowControl::Read));
        center.key(&host, &layout, &Key::Named(NamedKey::Enter));
        assert!(center.is_reading());
        let tree = center.accessibility(&host, &layout, ui::time::now());
        assert_eq!(tree.children[1].description.as_deref(), Some(body));
        assert!(tree.children[1].state.focused);
        for key in [NamedKey::Enter, NamedKey::Space, NamedKey::Delete] {
            assert_eq!(
                center.key(&host, &layout, &Key::Named(key)),
                CenterOutcome::Handled
            );
            assert!(center.is_reading());
            assert_eq!(host.log().history().len(), 1);
        }
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        assert_eq!(center.row_control, Some(RowControl::Read));
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Escape)),
            CenterOutcome::Back
        );
        center.key(&host, &layout, &Key::Named(NamedKey::Enter));
        assert!(center.is_reading());
        assert!(center.focus_accessibility_path(&host, &[0]));
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Enter)),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        assert_eq!(host.log().history().len(), 1);
    }

    #[test]
    fn reading_pointer_back_removed_notifications_and_hidden_previews_are_safe() {
        let host = TestHost::default();
        let mut notification = Notification::new("Private message", "Do not reveal this body");
        notification.show_preview = false;
        let id = notification.id;
        host.log.borrow_mut().retain(notification);
        let layout = layout();
        let mut center = NotificationCenter::new();
        let read = center
            .semantic_rects(&host, &layout, "")
            .into_iter()
            .find(|(name, _)| name == "notification_read[0]")
            .unwrap()
            .1;
        assert_eq!(
            center.click(&host, &layout, read.center()),
            CenterOutcome::Handled
        );
        assert!(center.is_reading());
        let tree = center.accessibility(&host, &layout, ui::time::now());
        assert_ne!(
            tree.children[1].description.as_deref(),
            Some("Do not reveal this body")
        );
        assert_eq!(
            center.click(&host, &layout, layout.clear().center()),
            CenterOutcome::Handled
        );
        assert_eq!(host.log().history().len(), 1);
        assert_eq!(
            center.click(&host, &layout, layout.back().center()),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        center.click(&host, &layout, read.center());
        host.log.borrow_mut().take_history(id);
        assert_eq!(
            center.key(&host, &layout, &Key::Named(NamedKey::Enter)),
            CenterOutcome::Handled
        );
        assert!(!center.is_reading());
        assert!(host.log().history().is_empty());
        assert!(!center.focus_accessibility_path(&host, &[0, 0, 1]));
    }
    #[test]
    fn reading_control_tab_order_reverses_and_moves_between_notifications() {
        let host = TestHost::default();
        for title in ["first", "second"] {
            host.log
                .borrow_mut()
                .retain(Notification::new(title, "body"));
        }
        let layout = layout();
        let mut center = NotificationCenter::new();
        let tab = Key::Named(NamedKey::Tab);
        center.key_with_shift(&host, &layout, &tab, false);
        let selected = center.selected.clone();
        assert_eq!(center.row_control, None);
        center.key_with_shift(&host, &layout, &tab, false);
        assert_eq!(center.row_control, Some(RowControl::Read));
        center.key_with_shift(&host, &layout, &tab, true);
        assert_eq!(center.row_control, None);
        assert_eq!(center.selected, selected);
        center.key_with_shift(&host, &layout, &tab, true);
        assert_eq!(center.row_control, Some(RowControl::Dismiss));
        assert_ne!(center.selected, selected);
        center.key_with_shift(&host, &layout, &tab, false);
        assert_eq!(center.row_control, None);
        assert_eq!(center.selected, selected);
        center.key_with_shift(&host, &layout, &tab, false);
        center.key(&host, &layout, &Key::Named(NamedKey::Enter));
        assert!(center.is_reading());
    }
}

#[cfg(test)]
#[path = "center_header_tests.rs"]
mod header_tests;

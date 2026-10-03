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
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, NamedKey};

use super::NotificationHost;
use super::history::{
    HistoryActivation, HistoryRow, RowIdentity, accessible_description, relative_time, source_label,
};
use super::log::NotificationLog;
use super::model::Notification;
use crate::drill_in::{self as qs, DrillInList};
use crate::i18n::{t, t_args};

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
fn f64_to_f32_lossy(v: f64) -> f32 {
    v as f32
}

/// The centre's geometry for one panel rect and type scale, so paint,
/// hit-testing, the visible-row count and the preferred height read one
/// oracle (HIG §7b: one rhythm). Every step is the drill-in panel's shared
/// rhythm ([`crate::drill_in`]).
#[derive(Clone, Copy, Debug)]
pub struct CenterLayout {
    panel: Rect,
    /// Line boxes of a caption (`xs`) and a label (`sm`) at the text scale.
    caption_lh: f64,
    label_lh: f64,
}

impl CenterLayout {
    /// Space the pager band claims under the last row: a related step, the
    /// band, the panel padding.
    const FOOTER_RESERVE: f64 = qs::RELATED_GAP + qs::TARGET + qs::PANEL_PAD;

    /// `type_px` are the resolved caption (`xs`) and label (`sm`) sizes the
    /// centre paints with.
    pub fn new(panel: Rect, type_px: (f32, f32)) -> Self {
        let line = |px: f32| f64::from(ui::text_render::TextStyle::new(px).line_height_px());
        Self {
            panel,
            caption_lh: line(type_px.0),
            label_lh: line(type_px.1),
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
            p.x0 + qs::PANEL_PAD + 2.0 * qs::TARGET,
            p.y0 + qs::RELATED_GAP + qs::TARGET,
        )
    }
    /// The "Clear all" chip, mirroring [`Self::back`] on the trailing padding.
    pub fn clear(&self) -> Rect {
        let p = self.panel;
        Rect::new(
            p.x1 - qs::PANEL_PAD - 2.0 * qs::TARGET,
            p.y0 + qs::RELATED_GAP,
            p.x1 - qs::PANEL_PAD,
            p.y0 + qs::RELATED_GAP + qs::TARGET,
        )
    }
    /// The Undo button on the "cleared" status band.
    pub fn undo(&self) -> Rect {
        let r = self.panel;
        Rect::new(r.x1 - 84.0, r.y1 - 46.0, r.x1 - 12.0, r.y1 - 12.0)
    }
    /// The "cleared" status band at the panel's foot.
    fn cleared_band(&self) -> Rect {
        let r = self.panel;
        Rect::new(r.x0 + 12.0, r.y1 - 50.0, r.x1 - 12.0, r.y1 - 8.0)
    }
    /// Top of the position line ("Newest 1–4 of 9"): where a list starts.
    fn position_y(&self) -> f64 {
        self.panel.y0 + qs::LIST_TOP
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
            p.y1 - qs::PANEL_PAD - qs::TARGET,
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
        self.rows_that_fit(Self::FOOTER_RESERVE).max(1)
    }
    /// The height the centre wants to show `rows` of `total`: the pager band
    /// is reserved only when there is something to page to. An empty centre
    /// keeps the drill-in's one-row band for its message.
    pub fn preferred_height(rows: usize, total: usize, type_px: (f32, f32)) -> f64 {
        if rows == 0 {
            return DrillInList::list_height(qs::LIST_TOP, 0);
        }
        let l = Self::new(Rect::ZERO, type_px);
        let foot = if total > rows {
            Self::FOOTER_RESERVE
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
    pub fn row_controls(&self, row: Rect, with_agent: bool) -> (Rect, Option<Rect>) {
        let top = row.y0 + qs::CONTROL_PAD + self.caption_lh;
        let room = (row.y1 - top).max(0.0);
        let size = qs::TARGET.min(room);
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
    /// The keyboard-selected row, by identity so it survives re-derivation.
    selected: Option<RowIdentity>,
    /// The selected row's focused trailing control.
    row_control: Option<RowControl>,
    /// App groups the viewer folded.
    collapsed: BTreeSet<String>,
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The centre's rows, newest first, honouring the viewer's folded groups.
    pub fn rows(&self, host: &dyn NotificationHost) -> Vec<HistoryRow> {
        host.log().rows(&self.collapsed)
    }

    /// The panel height the centre asks for over `log` at `type_px`.
    pub fn preferred_height(&self, log: &NotificationLog, type_px: (f32, f32)) -> f64 {
        let total = log.rows(&self.collapsed).len();
        CenterLayout::preferred_height(total.min(PREFERRED_ROWS), total, type_px)
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
        if layout.back().contains(p) {
            return CenterOutcome::Back;
        }
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        if layout.undo().contains(p) && can_undo {
            host.undo_clear();
        } else if layout.clear().contains(p) && !history_empty {
            self.clear(host);
            self.selected = None;
        } else if let Some((row, control)) = self.row_hit(host, layout, p) {
            // A pointer press acts on the row without leaving a keyboard
            // selection ring behind it.
            self.selected = None;
            self.row_control = None;
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
    pub fn key(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        logical_key: &Key,
    ) -> CenterOutcome {
        let rows = self.rows(host);
        let total = rows.len();
        let selected = self.selected_row(&rows, host.log().history());
        let can_undo = host.log().can_undo_clear();
        let page = isize::try_from(layout.visible_rows(total)).unwrap_or(isize::MAX);
        let mut outcome = CenterOutcome::Handled;
        match logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space)
                if self.action_focus == Some(false) && total > 0 =>
            {
                host.clear_history();
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
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowLeft) if selected.is_some() => {}
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
                    .with_description(t("a11y-notifications-clear-description")),
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
                    .with_description(t("a11y-notifications-undo-description")),
            );
        }
        AccessNode::new(Role::Dialog)
            .with_name(t("a11y-notifications"))
            .with_description(description)
            .with_state(AccessState::new().modal(true))
            .with_children(children)
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
        let (history_empty, can_undo) = {
            let log = host.log();
            (log.history().is_empty(), log.can_undo_clear())
        };
        match path {
            [] => Some(layout.panel()),
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
        if let [0, row, rest @ ..] = path {
            let rows = self.rows(host);
            let index = self.start.saturating_add(*row);
            let identity = rows
                .get(index)
                .and_then(|row| row.identity(host.log().history()));
            let control = match rest {
                [] => None,
                [0] => Some(RowControl::Dismiss),
                [1] => Some(RowControl::HandToAgent),
                _ => return false,
            };
            let Some(identity) = identity else {
                return false;
            };
            self.selected = Some(identity);
            self.row_control = control;
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
        let rect = layout.panel();
        let colors = &theme.tokens.colors;
        let caption_size = qs::type_size(text_renderer, theme.tokens.typography.scale.xs);
        let label_size = qs::type_size(text_renderer, theme.tokens.typography.scale.sm);
        Self::chip(
            scene,
            transform,
            text_renderer,
            theme,
            (layout.back(), cursor),
            &t("drill-in-back"),
            colors.text_primary,
        );
        let title = t("notification-center-title");
        let (title_w, title_h) =
            text_renderer.measure_weighted(&title, label_size, None, TextWeight::Semibold);
        text_renderer.draw_text_weighted(
            scene,
            &title,
            label_size,
            colors.text_primary,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(title_w) / 2.0,
                    rect.y0 + (qs::HEADER_HEIGHT - f64::from(title_h)) / 2.0,
                )
                    .into(),
            ),
            None,
            TextWeight::Semibold,
        );
        let rows = self.rows(host);
        let log = host.log();
        let history = log.history();
        let can_undo = log.can_undo_clear();
        if history.is_empty() {
            self.start = 0;
            DrillInList::draw_empty(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                &t("notification-center-empty"),
            );
            if can_undo {
                Self::draw_cleared_band(scene, transform, text_renderer, theme, layout);
            }
            return;
        }

        let visible_rows = layout.visible_rows(rows.len());
        let _ = self.clamp_start(layout, rows.len());
        let start = self.start;
        let shown = visible_rows.min(rows.len().saturating_sub(start));
        let selected = self.selected_row(&rows, history);

        Self::chip(
            scene,
            transform,
            text_renderer,
            theme,
            (layout.clear(), cursor),
            &t("notification-center-clear-all"),
            colors.text_dim,
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
        text_renderer.draw_text(
            scene,
            &position,
            caption_size,
            colors.text_dim,
            transform.pre_translate((rect.x0 + qs::PANEL_PAD, layout.position_y()).into()),
            Some(f64_to_f32_lossy(rect.width() - 2.0 * qs::PANEL_PAD)),
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
            let text_right = controls.1.unwrap_or(controls.0).x0 - qs::RELATED_GAP;
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
            let (time_width, _) = text_renderer.measure(&age, caption_size, None);
            // Source/time, title, body: three measured lines on the control
            // padding, inset `s3` like every row glyph in the panel. A
            // grouped notification sits one glyph in, under its header's
            // label, so the fold reads as a hierarchy.
            let x = row.x0 + qs::CONTROL_INSET + if grouped { qs::GLYPH } else { 0.0 };
            let source_y = row.y0 + qs::CONTROL_PAD;
            text_renderer.draw_text(
                scene,
                &source,
                caption_size,
                colors.text_dim,
                transform.pre_translate((x, source_y).into()),
                Some(f64_to_f32_lossy(
                    row.x1 - qs::CONTROL_INSET - f64::from(time_width) - qs::RELATED_GAP - x,
                )),
            );
            text_renderer.draw_text(
                scene,
                &age,
                caption_size,
                colors.text_dim,
                transform.pre_translate(
                    (row.x1 - qs::CONTROL_INSET - f64::from(time_width), source_y).into(),
                ),
                None,
            );
            let title_y = source_y + layout.caption_lh;
            text_renderer.draw_text_weighted(
                scene,
                &notification.title,
                label_size,
                colors.text_primary,
                transform.pre_translate((x, title_y).into()),
                Some(f64_to_f32_lossy(text_right - x)),
                TextWeight::Semibold,
            );
            if notification.show_preview && !notification.body.is_empty() {
                text_renderer.draw_text(
                    scene,
                    &notification.body,
                    caption_size,
                    colors.text_dim,
                    transform.pre_translate((x, title_y + layout.label_lh).into()),
                    Some(f64_to_f32_lossy(text_right - x)),
                );
            }
            Self::draw_row_controls(scene, transform, theme, controls, (focused_control, cursor));
            if row_selected && self.row_control.is_none() {
                Self::draw_selection(scene, transform, theme, row);
            }
        }

        if can_undo {
            Self::draw_cleared_band(scene, transform, text_renderer, theme, layout);
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
            let (hint_w, hint_h) = text_renderer.measure(&hint, caption_size, None);
            text_renderer.draw_text(
                scene,
                &hint,
                caption_size,
                colors.text_primary,
                transform.pre_translate(
                    (
                        footer.center().x - f64::from(hint_w) / 2.0,
                        footer.center().y - f64::from(hint_h) / 2.0,
                    )
                        .into(),
                ),
                Some(f64_to_f32_lossy(footer.width() - 2.0 * qs::CONTROL_INSET)),
            );
        }
    }

    /// A header chip as a Control (HIG §9.1/§9.5): the lift floor with a
    /// derived edge, the hover tier under the pointer, its label centred on
    /// the measured line.
    fn chip(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (r, cursor): (Rect, Option<Point>),
        label: &str,
        ink: Color,
    ) {
        let caption_size = qs::type_size(text_renderer, theme.tokens.typography.scale.xs);
        qs::paint_control(
            scene,
            transform,
            theme,
            r,
            cursor.is_some_and(|p| r.contains(p)),
            false,
        );
        let (w, h) = text_renderer.measure(label, caption_size, None);
        text_renderer.draw_text(
            scene,
            label,
            caption_size,
            ink,
            transform.pre_translate(
                (
                    r.center().x - f64::from(w) / 2.0,
                    r.center().y - f64::from(h) / 2.0,
                )
                    .into(),
            ),
            None,
        );
    }

    /// The "Notifications cleared" band with its Undo button.
    fn draw_cleared_band(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &CenterLayout,
    ) {
        let colors = &theme.tokens.colors;
        let size = theme.tokens.typography.scale.xs;
        let status = layout.cleared_band();
        scene.fill(
            Fill::NonZero,
            transform,
            colors.lift(0.35),
            None,
            &RoundedRect::from_rect(status, 10.0),
        );
        text_renderer.draw_text(
            scene,
            &t("notification-center-cleared"),
            size,
            colors.text_primary,
            transform.pre_translate((status.x0 + 12.0, status.y0 + 12.0).into()),
            Some(f64_to_f32_lossy(status.width() - 96.0)),
        );
        let undo = layout.undo();
        scene.fill(
            Fill::NonZero,
            transform,
            colors.intent.inverted,
            None,
            &RoundedRect::from_rect(undo, 9.0),
        );
        let label = t("notification-center-undo");
        let (w, h) = text_renderer.measure(&label, size, None);
        text_renderer.draw_text(
            scene,
            &label,
            size,
            colors.intent.on_inverted,
            transform.pre_translate(
                (
                    undo.center().x - f64::from(w) / 2.0,
                    undo.center().y - f64::from(h) / 2.0,
                )
                    .into(),
            ),
            None,
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
        let caption_size = qs::type_size(text_renderer, theme.tokens.typography.scale.xs);
        let label_size = qs::type_size(text_renderer, theme.tokens.typography.scale.sm);
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
        let (time_width, _) = text_renderer.measure(&age, caption_size, None);
        let source_y = row.y0 + qs::CONTROL_PAD;
        let count_label = t_args(
            "notification-center-group-count",
            &[("count", count.into())],
        );
        text_renderer.draw_text(
            scene,
            &count_label,
            caption_size,
            colors.text_dim,
            transform.pre_translate((x, source_y).into()),
            Some(f64_to_f32_lossy(
                row.x1 - qs::CONTROL_INSET - f64::from(time_width) - qs::RELATED_GAP - x,
            )),
        );
        text_renderer.draw_text(
            scene,
            &age,
            caption_size,
            colors.text_dim,
            transform.pre_translate(
                (row.x1 - qs::CONTROL_INSET - f64::from(time_width), source_y).into(),
            ),
            None,
        );
        let text_w = f64_to_f32_lossy(dismiss.x0 - qs::RELATED_GAP - x);
        let title_y = source_y + layout.caption_lh;
        text_renderer.draw_text_weighted(
            scene,
            label,
            label_size,
            colors.text_primary,
            transform.pre_translate((x, title_y).into()),
            Some(text_w),
            TextWeight::Semibold,
        );
        if !expanded {
            text_renderer.draw_text(
                scene,
                &newest.title,
                caption_size,
                colors.text_dim,
                transform.pre_translate((x, title_y + layout.label_lh).into()),
                Some(text_w),
            );
        }
    }

    /// A row's trailing icon buttons as Controls, the hovered one on the
    /// hover tier and the keyboard-focused one ringed.
    fn draw_row_controls(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        (dismiss, agent): (Rect, Option<Rect>),
        (focused, cursor): (Option<RowControl>, Option<Point>),
    ) {
        let glyph = qs::GLYPH.min(dismiss.height());
        let mut paint = |rect: Rect, icon: ui::icons::Icon, control| {
            qs::paint_control(
                scene,
                transform,
                theme,
                rect,
                cursor.is_some_and(|p| rect.contains(p)),
                focused == Some(control),
            );
            let c = rect.center();
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    c.x - glyph / 2.0,
                    c.y - glyph / 2.0,
                    c.x + glyph / 2.0,
                    c.y + glyph / 2.0,
                ),
                icon,
                theme.tokens.colors.text_dim,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        };
        paint(dismiss, ui::icons::Icon::X, RowControl::Dismiss);
        if let Some(agent) = agent {
            paint(agent, ui::icons::Icon::Send, RowControl::HandToAgent);
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
}

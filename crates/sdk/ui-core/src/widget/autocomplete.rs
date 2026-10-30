//! `Autocomplete` — a text field with a suggestion list that floats below
//! it — and [`SuggestionList`], the floating list itself, which a code
//! editor's caret completion shares.
//!
//! A real [`TextInput`] (so it inherits selection, clipboard, IME, caret
//! navigation and theming for free, per `crates/sdk/ui-core/HIG.md` §8) with a suggestion
//! popover drawn in the overlay pass so it composites above sibling content.
//!
//! Any app attaches a suggestion source — a closure via
//! [`with_suggester`](Autocomplete::with_suggester), or a reactively-computed
//! list via [`set_suggestions`](Autocomplete::set_suggestions) — and
//! completions recompute whenever the typed text changes.
//!
//! Typing opens the dropdown; Down/Up move the highlight; Enter accepts (the
//! highlighted suggestion, or the typed text, which Enter then goes on to
//! the host with, so a form's default action runs); Tab completes to the
//! highlight without accepting; Esc or an outside click cancels — the
//! outside click is not consumed, so it still reaches the host. The
//! outcome is polled once via [`take_outcome`](Autocomplete::take_outcome),
//! rising-edge like [`TextInput::take_dirty`].
//!
//! The editable path bar in the file manager is the canonical consumer, but
//! the widget knows nothing about paths.

use crate::num::{f32_of_f64, f64_of_usize};
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::scroll_region::ScrollRegion;
use crate::widget::{TextInput, TextInputKind, Widget};

/// One completion offered in the dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The text inserted into the field when this suggestion is accepted.
    pub value: String,
    /// The label shown in the dropdown row. Defaults to `value` via
    /// [`Suggestion::new`].
    pub label: String,
    /// Optional dim secondary text shown right-aligned in the row (e.g. a type
    /// or a count).
    pub detail: Option<String>,
    /// Optional dim line under the label (a page's address under its
    /// title). A list with any such line gives every row two lines.
    pub subtitle: Option<String>,
    /// Optional leading glyph.
    pub icon: Option<Icon>,
    /// A participant this row names: painted as their avatar (or, for
    /// software acting in the room, the agent mark) in place of an icon.
    pub person: Option<SuggestionPerson>,
    /// What the row is: a pickable item (the default), a group's header or a
    /// notice ("No matches").
    pub role: RowRole,
    /// Byte ranges of the label the typed query matched, painted bold.
    pub matched: Vec<std::ops::Range<usize>>,
    /// Dim text right after the label on the same line: a handle, a topic,
    /// a command's parameters.
    pub aside: Option<String>,
    /// A state shown in a trailing check column: checked or not. A list
    /// with any such row keeps the column on every row.
    pub checked: Option<bool>,
    /// The keys that do what the row does, at its trailing end.
    pub shortcut: Option<String>,
}

/// What a row of a [`SuggestionList`] is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowRole {
    /// A suggestion that can be armed and accepted.
    #[default]
    Item,
    /// The header of the group of rows under it; `busy` while that group is
    /// on its way, with a progress indicator beside it.
    Header { busy: bool },
    /// A line that says something about the list ("No matches"), never
    /// armed.
    Notice,
}

impl Suggestion {
    /// Whether the row can be armed and accepted.
    #[must_use]
    pub fn is_pickable(&self) -> bool {
        self.role == RowRole::Item
    }
}

/// Who a [`Suggestion`] names, for its leading avatar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestionPerson {
    /// Their stable id, which also picks the avatar's hue.
    pub identity: String,
    /// The colour the room gives them.
    pub color: (u8, u8, u8),
    /// Software acting in the room rather than a person.
    pub agent: bool,
}

impl Suggestion {
    /// A suggestion whose visible label is its inserted value.
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
            detail: None,
            subtitle: None,
            icon: None,
            person: None,
            role: RowRole::Item,
            matched: Vec::new(),
            aside: None,
            checked: None,
            shortcut: None,
        }
    }

    /// A suggestion whose visible label differs from the inserted value.
    pub fn labelled(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: None,
            subtitle: None,
            icon: None,
            person: None,
            role: RowRole::Item,
            matched: Vec::new(),
            aside: None,
            checked: None,
            shortcut: None,
        }
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Show `subtitle` as a dim line under the label.
    #[must_use]
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Lead the row with `person`'s avatar.
    #[must_use]
    pub fn with_person(mut self, person: SuggestionPerson) -> Self {
        self.person = Some(person);
        self
    }

    /// Show `checked` in the trailing check column.
    #[must_use]
    pub fn with_checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Show the keys that do what the row does at its end.
    #[must_use]
    pub fn with_shortcut(mut self, keys: impl Into<String>) -> Self {
        self.shortcut = Some(keys.into());
        self
    }
}

/// The width a row's trailing detail may take out of `free`: what the label
/// (`label_w` wide) leaves, and at least half the row.
fn detail_room(free: f64, label_w: f64, spacing: f64) -> f64 {
    (free - label_w - 2.0 * spacing).max(free / 2.0)
}

/// The pickable row after (or, not `forward`, before) row `from` of
/// `count`: past the last it wraps round to the first, or without `wrap`
/// stays on the last; from no row it is the first (last) pickable row.
/// `None` when no row is pickable. Every menu's arrow keys step so.
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

/// How the most recent interaction ended. Returned once by
/// [`Autocomplete::take_outcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutocompleteOutcome {
    /// Nothing ended since the last poll.
    None,
    /// The user accepted this value (Enter, or a click on a row).
    Accepted(String),
    /// The user cancelled (Esc, or a click outside).
    Cancelled,
}

/// A suggestion source: given the current text, return the completions to show.
pub type Suggester = Box<dyn Fn(&str) -> Vec<Suggestion>>;

const DEFAULT_MAX_VISIBLE: usize = 8;

/// The geometry of a suggestion list in one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ListMetrics {
    /// One row: a menu row, grown with the label's line at large text.
    row: f64,
    /// The menu inset round the rows.
    inset: f64,
    /// Between the anchor and the panel.
    gap: f64,
    /// Inside a row, before the icon and after the detail.
    pad: f64,
    /// A row's leading icon.
    icon: f64,
    /// The trailing check of a row that shows a state.
    check: f64,
    /// The lane a scroll bar takes beside the rows.
    gutter: f64,
    /// Between the icon, the label and the detail column.
    spacing: f64,
}

impl ListMetrics {
    /// The geometry in `theme` for labels in `role`; `two_line` rows hold
    /// a caption line under the label.
    fn of(theme: &Theme, role: TextRole, two_line: bool) -> Self {
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
            inset: crate::widget::floating_surface::menu_inset(theme),
            gap: f64::from(tokens.spacing.s1),
            pad: f64::from(tokens.spacing.s3),
            icon: f64::from(tokens.metrics.icon_sm).max(line.round() - 2.0),
            check: f64::from(tokens.metrics.icon_sm),
            gutter: f64::from(tokens.metrics.scrollbar_gutter),
            spacing: f64::from(tokens.spacing.s2),
        }
    }

    /// A participant's avatar: a little larger than an icon, inside the row.
    fn avatar(&self) -> f64 {
        (self.icon + 4.0).min(self.row - 2.0 * self.inset.min(4.0))
    }

    /// The stock geometry, for hit-testing before the first paint.
    fn stock() -> Self {
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

/// What a list's width was measured with: its label style and geometry,
/// and the renderer's fonts and text scale.
struct WidthKey {
    style: crate::text_render::TextStyle,
    metrics: ListMetrics,
    epoch: std::sync::Arc<()>,
    text_scale: f32,
}

/// A floating list of suggestions under (or, without room, above) an
/// anchor: the one menu list behind [`Autocomplete`], a code editor's caret
/// completion, the filter and slash menus and a [`crate::widget::Dropdown`]'s
/// open list.
///
/// It is a floating Panel (`surface::panel`, `radii.panel`, the floating
/// shadow) placed by `floating_surface::place_aligned`, so it flips above
/// its anchor near the window's bottom and shifts to stay inside it. Rows
/// are menu rows with the armed-row lift, an optional leading icon, an
/// optional trailing detail column, shortcut and check. More suggestions
/// than fit scroll, the armed row always in view; a list taller than the
/// room beside its anchor shrinks to it and scrolls. While rows are on
/// their way, the framework's placeholder stands where the first will be
/// ([`crate::arriving`]); a status line stands in for rows when there are
/// none.
pub struct SuggestionList {
    suggestions: Vec<Suggestion>,
    highlight: Option<usize>,
    hover: Option<usize>,
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
    metrics: ListMetrics,
    /// The width the rows need, from the latest paint.
    content_width: f64,
    /// What [`Self::content_width`] was measured with, so a repaint
    /// reshapes no row; `None` once the rows changed.
    measured: Option<WidthKey>,
    /// The narrowest the panel is, whatever its anchor and rows need.
    min_width: f64,
    /// A band its owner paints at the top of the panel, above the rows (a
    /// picker's own filter field).
    header: f64,
    /// The side of the anchor the panel opens on when there is room there.
    side: crate::widget::PopoverSide,
    /// Which edge of the anchor the panel lines up with.
    align: crate::widget::PanelAlign,
    /// The panel spans the viewport's width right above its anchor: a phone's
    /// drawer docked above the keyboard, its rows a finger tall.
    docked: bool,
    /// The panel's height, following its rows on the platform's spring.
    height: crate::motion::Extent,
    /// Turns beside a header whose rows are on their way.
    spinner: crate::widget::spinner::Spinner,
}

impl Default for SuggestionList {
    fn default() -> Self {
        Self::new()
    }
}

impl SuggestionList {
    #[must_use]
    pub fn new() -> Self {
        Self {
            suggestions: Vec::new(),
            highlight: None,
            hover: None,
            scroll: ScrollRegion::vertical().with_no_bars(),
            theme: Theme::dark(),
            max_visible: DEFAULT_MAX_VISIBLE,
            max_height: None,
            min_row: 0.0,
            status: None,
            arriving: None,
            drawn_step: None,
            anchor: Rect::ZERO,
            viewport: None,
            role: TextRole::Body,
            metrics: ListMetrics::stock(),
            content_width: 0.0,
            measured: None,
            min_width: 0.0,
            header: 0.0,
            side: crate::widget::PopoverSide::Below,
            align: crate::widget::PanelAlign::Leading,
            docked: false,
            height: crate::motion::Extent::default(),
            spinner: crate::widget::spinner::Spinner::new(),
        }
    }

    /// Open on `side` of the anchor when there is room there: above a
    /// composer at the bottom of a window, below a field near its top.
    pub fn set_side(&mut self, side: crate::widget::PopoverSide) {
        self.side = side;
    }

    /// Line the panel up with the anchor's `align` edge: a menu button's
    /// list hangs from its trailing edge.
    pub fn set_align(&mut self, align: crate::widget::PanelAlign) {
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

    /// Whether the panel's height is still moving, or a header's progress
    /// turns: the list asks for frames until it rests.
    #[must_use]
    pub fn is_animating(&self, now: crate::time::Instant) -> bool {
        self.height.is_moving(now)
            || self
                .suggestions
                .iter()
                .any(|s| matches!(s.role, RowRole::Header { busy: true }))
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
    }

    /// Show a bar beside the rows while they overflow, which a drag of its
    /// thumb scrolls as every bar does.
    #[must_use]
    pub fn with_scrollbar(mut self) -> Self {
        self.scroll = ScrollRegion::vertical();
        self
    }

    /// Set the rows' label role; see [`Self::with_text_role`].
    pub fn set_text_role(&mut self, role: TextRole) {
        self.role = role;
    }

    /// Take `theme`'s geometry now, before a paint measures the labels, so
    /// rows are hit where they will be drawn.
    pub fn set_theme(&mut self, theme: &Theme) {
        let two_line = self.suggestions.iter().any(|s| s.subtitle.is_some());
        self.metrics = ListMetrics::of(theme, self.role, two_line);
        if self.docked {
            // A finger's row: the panel row every touch list has.
            self.metrics.row = self
                .metrics
                .row
                .max(f64::from(theme.tokens.metrics.panel_row_height));
        }
        self.metrics.row = self.metrics.row.max(self.min_row);
        if self.theme.tokens != theme.tokens {
            self.theme = theme.clone();
        }
        self.place_scroll();
    }

    /// Replace the suggestions. Keeps the armed row when it still exists
    /// and clears the status line once rows arrive.
    pub fn set_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        // The same rows again leave the list as it is: scrolled where the
        // person scrolled it, its width measured.
        if self.suggestions == suggestions {
            return;
        }
        self.suggestions = suggestions;
        self.measured = None;
        if !self.suggestions.is_empty() {
            self.status = None;
            self.arriving = None;
        }
        if self
            .highlight
            .is_some_and(|h| self.suggestions.get(h).is_none_or(|s| !s.is_pickable()))
        {
            self.highlight = None;
        }
        self.place_scroll();
        self.reveal();
    }

    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    /// The armed row's index.
    pub fn highlight(&self) -> Option<usize> {
        self.highlight
    }

    pub fn highlighted(&self) -> Option<&Suggestion> {
        self.highlight.and_then(|i| self.suggestions.get(i))
    }

    /// Arm row `index` (clamped; `None` disarms) and scroll it into view.
    pub fn set_highlight(&mut self, index: Option<usize>) {
        self.highlight =
            index.filter(|&i| self.suggestions.get(i).is_some_and(Suggestion::is_pickable));
        self.reveal();
    }

    /// The pickable rows' indices, in order.
    fn pickable(&self) -> Vec<usize> {
        self.suggestions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_pickable())
            .map(|(i, _)| i)
            .collect()
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
        let pickable = self.pickable();
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
        let next = step_pickable(self.suggestions.len(), self.highlight, forward, wrap, |i| {
            self.suggestions.get(i).is_some_and(Suggestion::is_pickable)
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

    /// Whether rows are on their way.
    pub fn is_busy(&self) -> bool {
        self.arriving.is_some()
    }

    /// The placeholder step the list shows at `now`, while rows are on
    /// their way and none is here.
    fn arriving_step(&self, now: crate::time::Instant) -> Option<crate::arriving::Step> {
        self.suggestions
            .is_empty()
            .then_some(self.arriving.as_ref())
            .flatten()
            .map(|clock| clock.step(now))
    }

    /// When the placeholder steps on next.
    fn next_arriving_step(&self, now: crate::time::Instant) -> Option<crate::time::Instant> {
        if !self.suggestions.is_empty() {
            return None;
        }
        self.arriving.as_ref()?.next_step_at(now)
    }

    /// Whether the placeholder stepped on since the last paint.
    fn arriving_moved(&self) -> bool {
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

    /// Whether there is anything to show: rows, a status line or rows on
    /// their way.
    pub fn has_content(&self) -> bool {
        !self.suggestions.is_empty() || self.status.is_some() || self.arriving.is_some()
    }

    /// Clear rows, status and armed row.
    pub fn clear(&mut self) {
        self.suggestions.clear();
        self.measured = None;
        self.highlight = None;
        self.hover = None;
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
        let row = self.metrics.row;
        let top = f64_of_usize(h) * row;
        self.scroll
            .scroll_into_view(Rect::new(0.0, top, 0.0, top + row));
    }

    /// The height of every row together.
    fn content_height(&self) -> f64 {
        f64_of_usize(self.suggestions.len()) * self.metrics.row
    }

    /// The height of the rows in view: every row, up to
    /// [`Self::with_max_visible`] rows, [`Self::set_max_height`] and the
    /// room beside the anchor. One row's for a status line.
    fn view_height(&self) -> f64 {
        let m = &self.metrics;
        if self.suggestions.is_empty() {
            return m.row;
        }
        let mut height = self
            .content_height()
            .min(f64_of_usize(self.max_visible) * m.row);
        if let Some(max) = self.max_height {
            height = height.min(max.max(m.row));
        }
        if let Some(viewport) = self.viewport.filter(|_| !self.docked) {
            use crate::widget::PopoverSide::{Above, Below};
            let room = [Above, Below]
                .into_iter()
                .map(|side| {
                    crate::widget::floating_surface::anchored_room(
                        self.anchor,
                        side,
                        m.gap,
                        viewport,
                    )
                })
                .fold(0.0, f64::max);
            height = height.min((room - 2.0 * m.inset - self.header).max(0.0));
        }
        height
    }

    /// The box inside `panel` the rows take: inside the inset, under the
    /// header, before a bar's lane.
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
    fn place_scroll(&mut self) {
        let Some(panel) = self.rect() else {
            return;
        };
        let area = self.rows_area(panel);
        self.scroll.set_viewport(area, &self.theme);
        self.scroll
            .set_content_size(area.width(), self.content_height());
        self.scroll.set_line_step(self.metrics.row);
    }

    /// The panel's rect: under the anchor, or above it when there is more
    /// room there, inside the viewport. `None` with nothing to show.
    pub fn rect(&self) -> Option<Rect> {
        if !self.has_content() {
            return None;
        }
        let m = &self.metrics;
        let height = self.view_height() + 2.0 * m.inset + self.header;
        let viewport = self
            .viewport
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
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
            crate::widget::floating_surface::place_aligned(
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

    /// The box the rows show in: the region's content, clear of its bar.
    fn rows_box(&self) -> Rect {
        self.scroll.content_rect()
    }

    /// Row `index` in content coordinates: the region's transform puts it
    /// where the scroll draws it.
    fn row_in(&self, index: usize) -> Rect {
        let row = self.metrics.row;
        let top = f64_of_usize(index) * row;
        Rect::new(0.0, top, self.rows_box().width(), top + row)
    }

    /// The rows at least partly in view.
    fn in_view(&self) -> std::ops::Range<usize> {
        let row = self.metrics.row.max(1.0);
        let offset = self.scroll.offset().y;
        let shown = self.rows_box().height();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative row positions, floored and ceiled"
        )]
        let (from, to) = (
            (offset / row).floor() as usize,
            ((offset + shown) / row).ceil() as usize,
        );
        from.min(self.suggestions.len())..to.min(self.suggestions.len())
    }

    /// The part of row `index` in view, in host coordinates.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        if index >= self.suggestions.len() {
            return None;
        }
        let shown = self.rows_box();
        let row = self.row_in(index) + (shown.origin().to_vec2() - self.scroll.offset());
        let clipped = row.intersect(shown);
        (!shown.is_zero_area() && clipped.height() > 0.0).then_some(clipped)
    }

    pub fn row_at(&self, point: Point) -> Option<usize> {
        let shown = self.rows_box();
        if !shown.contains(point) || self.suggestions.is_empty() {
            return None;
        }
        let offset = self.scroll.offset();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the point is inside the rows' box, so its row is non-negative"
        )]
        let index = ((point.y - shown.y0 + offset.y) / self.metrics.row.max(1.0)).floor() as usize;
        self.suggestions
            .get(index)
            .is_some_and(Suggestion::is_pickable)
            .then_some(index)
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

    /// The rows' bar thumb and track.
    #[cfg(test)]
    pub(crate) fn scrollbar_thumb(&self) -> Option<(Rect, Rect)> {
        self.scroll.test_vertical_thumb(&self.theme)
    }

    /// Whether any row shows a state, so every row keeps the check column.
    fn has_check_column(&self) -> bool {
        self.suggestions.iter().any(|s| s.checked.is_some())
    }

    /// The width every row needs: icon, label and the detail column.
    fn content_width_in(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let m = &self.metrics;
        let label_style = theme.text(self.role);
        let detail_style = theme.text(TextRole::Caption);
        let check = if self.has_check_column() {
            m.check + m.spacing
        } else {
            0.0
        };
        let widest = self
            .suggestions
            .iter()
            .map(|s| {
                let mut w = f64::from(text.measure_style(&s.label, None, &label_style).0) + check;
                if let Some(aside) = &s.aside {
                    w += m.spacing + f64::from(text.measure_style(aside, None, &detail_style).0);
                }
                if let Some(subtitle) = &s.subtitle {
                    w = w.max(f64::from(
                        text.measure_style(subtitle, None, &detail_style).0,
                    ));
                }
                if s.person.is_some() {
                    w += m.avatar() + m.spacing;
                } else if s.icon.is_some() {
                    w += m.icon + m.spacing;
                }
                if let Some(detail) = &s.detail {
                    w += 2.0 * m.spacing
                        + f64::from(text.measure_style(detail, None, &detail_style).0);
                }
                if let Some(keys) = &s.shortcut {
                    w += m.spacing + f64::from(text.measure_style(keys, None, &label_style).0);
                }
                w
            })
            .fold(0.0, f64::max);
        let status = self.status.as_ref().map_or(0.0, |status| {
            f64::from(text.measure_style(status, None, &label_style).0) + m.icon + m.spacing
        });
        (widest.max(status) + 2.0 * (m.pad + m.inset)).ceil()
    }

    /// Paint the list in the window's top layer.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.set_theme(theme);
        let style = theme.text(self.role);
        let measured = self.measured.as_ref().is_some_and(|key| {
            key.style == style
                && key.metrics == self.metrics
                && key.text_scale.to_bits() == text.text_scale().to_bits()
                && std::sync::Arc::ptr_eq(&key.epoch, text.layout_epoch())
        });
        if !measured {
            self.content_width = self.content_width_in(text, theme);
            self.measured = Some(WidthKey {
                style,
                metrics: self.metrics,
                epoch: std::sync::Arc::clone(text.layout_epoch()),
                text_scale: text.text_scale(),
            });
        }
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
        let radius = f64::from(theme.tokens.radii.panel);
        crate::surface::shadow(
            scene,
            transform,
            panel,
            radius,
            &theme.tokens.elevation.floating,
        );
        crate::surface::panel(scene, transform, panel, radius, theme);
        let m = self.metrics;
        self.drawn_step = self.arriving_step(crate::motion::frame_now());
        if self.suggestions.is_empty() {
            let row = self.rows_area(panel);
            let x = row.x0 + m.pad;
            if let Some(clock) = &self.arriving {
                // Where the first row will be: the framework's placeholder,
                // stepping on at its deadlines.
                let mut ctx = PaintCtx::new(scene, transform, text, theme);
                let line = ctx.line_box(self.role);
                let slot = Rect::new(
                    x,
                    row.center().y - line / 2.0,
                    row.x1 - m.pad,
                    row.center().y + line / 2.0,
                );
                crate::widget::ArrivalFace::new(
                    crate::widget::SkeletonShape::Text { lines: 1 },
                    clock,
                    crate::motion::frame_now(),
                )
                .paint(&mut ctx, slot);
                return;
            }
            if let Some(status) = &self.status {
                let style = theme.text(self.role);
                let shown = text.elide_to_width_style(
                    status,
                    f32_of_f64((row.x1 - m.pad - x).max(0.0)),
                    &style,
                );
                let (_, h) = text.measure_style(&shown, None, &style);
                text.draw_style(
                    scene,
                    &shown,
                    theme.text_dim,
                    transform.pre_translate(Vec2::new(x, row.center().y - f64::from(h) / 2.0)),
                    None,
                    &style,
                );
            }
            return;
        }
        let clip = self.scroll.content_rect().intersect(self.rows_area(panel));
        let content = self.scroll.content_transform(transform);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        let mut busy_at = None;
        let check_column = self.has_check_column();
        {
            let mut ctx = PaintCtx::new(scene, content, text, theme);
            for index in self.in_view() {
                let Some(s) = self.suggestions.get(index) else {
                    continue;
                };
                let row = self.row_in(index);
                match s.role {
                    RowRole::Item => self.paint_row(&mut ctx, row, index, s, check_column),
                    RowRole::Header { busy } => {
                        let end = self.paint_header(&mut ctx, row, s);
                        if busy {
                            busy_at = Some((row, end));
                        }
                    }
                    RowRole::Notice => self.paint_notice(&mut ctx, row, s),
                }
            }
        }
        if let Some((row, end)) = busy_at {
            let side = f64::from(theme.text(TextRole::Caption).line_height_px());
            let x = end + m.spacing;
            self.spinner.set_rect(Rect::new(
                x,
                row.center().y - side / 2.0,
                x + side,
                row.center().y + side / 2.0,
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

    /// One row: the armed lift (or the hover veneer), the icon, the label,
    /// and the detail in a trailing column.
    fn paint_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        index: usize,
        s: &Suggestion,
        check_column: bool,
    ) {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = ctx.reborrow();
        let m = &self.metrics;
        let armed = self.highlight == Some(index);
        let hovered = self.hover == Some(index) && !armed;
        let ink = if armed {
            crate::widget::floating_surface::paint_armed_row(scene, transform, row, m.inset, theme)
        } else {
            if hovered {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.overlay_medium,
                    None,
                    &RoundedRect::from_rect(row, f64::from(theme.tokens.radii.item)),
                );
            }
            theme.text_primary
        };
        let dim = theme.tokens.colors.ink_on_any(
            &[crate::chrome::panel_ground(theme)],
            theme.text_dim,
            crate::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        let label_style = theme.text(self.role);
        let detail_style = theme.text(TextRole::Caption);
        let mut x = row.x0 + m.pad;
        if let Some(person) = &s.person {
            let side = m.avatar();
            let mark = Rect::new(
                x,
                row.center().y - side / 2.0,
                x + side,
                row.center().y + side / 2.0,
            );
            let colour = theme.tokens.colors.presence.of_rgb(person.color);
            if person.agent {
                crate::agent_identity::draw_agent_identity_mark(
                    scene,
                    transform,
                    mark,
                    colour,
                    theme,
                    crate::agent_identity::AgentIdentityStyle::Solid,
                );
            } else {
                crate::widget::AvatarFace::new(&s.label, &person.identity)
                    .presence(colour)
                    .paint(&mut PaintCtx::new(scene, transform, text, theme), mark);
            }
            x += side + m.spacing;
        } else if let Some(icon) = s.icon {
            let iy = row.center().y - m.icon / 2.0;
            draw_icon(
                scene,
                transform,
                Rect::new(x, iy, x + m.icon, iy + m.icon),
                icon,
                if armed { ink } else { dim },
                LUCIDE_DEFAULT_STROKE_PX,
            );
            x += m.icon + m.spacing;
        }
        // The trailing end: the check column, then the keys that do the
        // same, quieter than the label unless the row is armed.
        let mut end = row.x1 - m.pad;
        if check_column {
            if s.checked == Some(true) {
                let cy = row.center().y;
                draw_icon(
                    scene,
                    transform,
                    Rect::new(end - m.check, cy - m.check / 2.0, end, cy + m.check / 2.0),
                    Icon::Check,
                    ink,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            end -= m.check + m.spacing;
        }
        if let Some(keys) = &s.shortcut {
            let (kw, kh) = text.measure_style(keys, None, &label_style);
            let kx = end - f64::from(kw);
            text.draw_style(
                scene,
                keys,
                if armed { ink } else { dim },
                transform.pre_translate(Vec2::new(kx, row.center().y - f64::from(kh) / 2.0)),
                None,
                &label_style,
            );
            end = kx - m.spacing;
        }
        // The detail column is trailing; the label takes what is left. The
        // detail may use whatever the label leaves free, and never less than
        // half the row, so a short name keeps its whole "Region, Country".
        let label_x1 = if let Some(detail) = &s.detail {
            let free = (end - x).max(0.0);
            let label_w = f64::from(text.measure_style(&s.label, None, &label_style).0);
            let room = f32_of_f64(detail_room(free, label_w, m.spacing));
            let shown = text.elide_to_width_style(detail, room, &detail_style);
            let (dw, dh) = text.measure_style(&shown, None, &detail_style);
            let dx = end - f64::from(dw);
            text.draw_style(
                scene,
                &shown,
                if armed { ink } else { dim },
                transform.pre_translate(Vec2::new(dx, row.center().y - f64::from(dh) / 2.0)),
                None,
                &detail_style,
            );
            dx - 2.0 * m.spacing
        } else {
            end
        };
        let room = f32_of_f64((label_x1 - x).max(0.0));
        let label = text.elide_to_width_style(&s.label, room, &label_style);
        let (lw, lh) = text.measure_style(&label, None, &label_style);
        let subtitle = s.subtitle.as_ref().map(|subtitle| {
            let shown = text.elide_to_width_style(subtitle, room, &detail_style);
            let (_, sh) = text.measure_style(&shown, None, &detail_style);
            (shown, f64::from(sh))
        });
        // The label and its subtitle stand as one block, centred in the row.
        let block = f64::from(lh) + subtitle.as_ref().map_or(0.0, |(_, sh)| *sh);
        let top = row.center().y - block / 2.0;
        if s.matched.is_empty() || label != s.label {
            text.draw_style(
                scene,
                &label,
                ink,
                transform.pre_translate(Vec2::new(x, top)),
                None,
                &label_style,
            );
        } else {
            // The letters the query matched, bold, between the rest.
            let bold = label_style
                .clone()
                .with_weight(crate::text_render::TextWeight::Bold);
            let mut pen = x;
            for (run, strong) in matched_runs(&label, &s.matched) {
                let style = if strong { &bold } else { &label_style };
                text.draw_style(
                    scene,
                    run,
                    ink,
                    transform.pre_translate(Vec2::new(pen, top)),
                    None,
                    style,
                );
                pen += f64::from(text.measure_style(run, None, style).0);
            }
        }
        if let Some(aside) = &s.aside {
            let ax = x + f64::from(lw) + m.spacing;
            let room = f32_of_f64((label_x1 - ax).max(0.0));
            if room > 8.0 {
                let shown = text.elide_to_width_style(aside, room, &detail_style);
                let (_, ah) = text.measure_style(&shown, None, &detail_style);
                text.draw_style(
                    scene,
                    &shown,
                    if armed { ink } else { dim },
                    transform.pre_translate(Vec2::new(
                        ax,
                        top + f64::from(lh) / 2.0 - f64::from(ah) / 2.0,
                    )),
                    None,
                    &detail_style,
                );
            }
        }
        if let Some((shown, _)) = subtitle {
            text.draw_style(
                scene,
                &shown,
                if armed { ink } else { dim },
                transform.pre_translate(Vec2::new(x, top + f64::from(lh))),
                None,
                &detail_style,
            );
        }
    }

    /// A group's header: its title small and dim, like a menu's section.
    /// Returns where the title ends, for a progress indicator beside it.
    fn paint_header(&self, ctx: &mut PaintCtx<'_>, row: Rect, s: &Suggestion) -> f64 {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = ctx.reborrow();
        let m = &self.metrics;
        let style = theme.text(TextRole::Caption);
        let dim = theme.tokens.colors.ink_on_any(
            &[crate::chrome::panel_ground(theme)],
            theme.text_dim,
            crate::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        let x = row.x0 + m.pad;
        let shown =
            text.elide_to_width_style(&s.label, f32_of_f64((row.x1 - m.pad - x).max(0.0)), &style);
        let (w, h) = text.measure_style(&shown, None, &style);
        text.draw_style(
            scene,
            &shown,
            dim,
            transform.pre_translate(Vec2::new(x, row.y1 - f64::from(h) - m.inset.min(4.0))),
            None,
            &style,
        );
        x + f64::from(w)
    }

    /// A line about the list ("No matches"), dim, never armed.
    fn paint_notice(&self, ctx: &mut PaintCtx<'_>, row: Rect, s: &Suggestion) {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = ctx.reborrow();
        let m = &self.metrics;
        let style = theme.text(self.role);
        let dim = theme.tokens.colors.ink_on_any(
            &[crate::chrome::panel_ground(theme)],
            theme.text_dim,
            crate::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        let x = row.x0 + m.pad;
        let shown =
            text.elide_to_width_style(&s.label, f32_of_f64((row.x1 - m.pad - x).max(0.0)), &style);
        let (_, h) = text.measure_style(&shown, None, &style);
        text.draw_style(
            scene,
            &shown,
            dim,
            transform.pre_translate(Vec2::new(x, row.center().y - f64::from(h) / 2.0)),
            None,
            &style,
        );
    }

    /// Accessibility rows for the suggestions in view, keyed by `key`.
    pub(crate) fn access_rows(
        &self,
        key: impl Fn(&Suggestion) -> u64,
    ) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Politeness, Role};
        let Some(panel) = self.rect() else {
            return Vec::new();
        };
        if self.suggestions.is_empty() {
            let busy = self.arriving.is_some();
            let arriving = self.arriving.as_ref().map(|clock| {
                let now = crate::time::now();
                crate::widget::ArrivalFace::new(
                    crate::widget::SkeletonShape::Text { lines: 1 },
                    clock,
                    now,
                )
                .caption()
                .unwrap_or_else(|| crate::i18n::I18nManager::lookup_plain("arriving-loading"))
            });
            return arriving
                .or_else(|| self.status.clone())
                .iter()
                .map(|status| {
                    AccessNode::new(Role::StaticText)
                        .with_name(status)
                        .with_live(Politeness::Polite)
                        .with_bounds(panel)
                        .with_state(AccessState::new().busy(busy))
                })
                .collect();
        }
        self.in_view()
            .filter_map(|index| {
                let s = self.suggestions.get(index)?;
                let bounds = self.row_rect(index)?;
                if !s.is_pickable() {
                    let busy = matches!(s.role, RowRole::Header { busy: true });
                    return Some(
                        AccessNode::new(if busy || s.role == RowRole::Notice {
                            Role::StaticText
                        } else {
                            Role::Heading { level: 3 }
                        })
                        .with_name(&s.label)
                        .with_key(key(s))
                        .with_bounds(bounds)
                        .with_state(AccessState::new().busy(busy)),
                    );
                }
                let mut row = AccessNode::new(Role::Button)
                    .with_name(&s.label)
                    .with_key(key(s))
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.highlight == Some(index)),
                    );
                if let Some(description) = s.subtitle.as_ref().or(s.detail.as_ref()) {
                    row = row.with_description(description);
                }
                Some(row)
            })
            .collect()
    }
}

/// `label` cut into runs, each marked whether a range of `matched` covers it.
fn matched_runs<'a>(label: &'a str, matched: &[std::ops::Range<usize>]) -> Vec<(&'a str, bool)> {
    let mut runs = Vec::new();
    let mut at = 0usize;
    let mut ranges: Vec<std::ops::Range<usize>> = matched
        .iter()
        .map(|range| {
            label.floor_char_boundary(range.start.min(label.len()))
                ..label.floor_char_boundary(range.end.min(label.len()))
        })
        .filter(|range| range.start < range.end)
        .collect();
    ranges.sort_by_key(|range| range.start);
    for range in ranges {
        if range.start < at {
            continue;
        }
        if let Some(plain) = label.get(at..range.start).filter(|run| !run.is_empty()) {
            runs.push((plain, false));
        }
        if let Some(strong) = label.get(range.clone()) {
            runs.push((strong, true));
        }
        at = range.end;
    }
    if let Some(rest) = label.get(at..).filter(|run| !run.is_empty()) {
        runs.push((rest, false));
    }
    runs
}

pub struct Autocomplete {
    id: String,
    /// The real editable field — owns caret / selection / clipboard / IME.
    input: TextInput,
    suggester: Option<Suggester>,
    /// The floating list of suggestions under the field.
    list: SuggestionList,
    open: bool,
    rect: Rect,
    last_pointer: Point,
    outcome: AutocompleteOutcome,
    /// The person changed the text, typing or taking a suggestion, since
    /// [`Self::take_edited`] last ran.
    edited: bool,
}

impl Autocomplete {
    pub fn new() -> Self {
        Self::with_input(TextInput::local())
    }

    /// An autocomplete around a field the app configured itself (its role,
    /// placeholder, size, accessible name); the field becomes a search
    /// field.
    pub fn with_input(input: TextInput) -> Self {
        Self {
            id: String::new(),
            input: input.with_kind(TextInputKind::Search),
            suggester: None,
            list: SuggestionList::new(),
            open: false,
            rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            outcome: AutocompleteOutcome::None,
            edited: false,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn new_with_controlled_input(input: TextInput) -> Self {
        Self::with_input(input)
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self.input.set_id(id);
        self
    }

    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.input.set_placeholder(hint);
        self
    }

    /// Name the field for assistive technology (a Fluent message id).
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.input = self.input.with_accessible_name(name);
        self
    }

    /// Set the field's text in `role` (default [`TextRole::Body`]); the
    /// suggestions set their labels in it too.
    #[must_use]
    pub fn with_text_role(mut self, role: TextRole) -> Self {
        self.input.set_text_role(role);
        self.list.set_text_role(role);
        self
    }

    #[must_use]
    pub fn with_suggester<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Vec<Suggestion> + 'static,
    {
        self.suggester = Some(Box::new(f));
        self
    }

    pub fn set_suggester<F>(&mut self, f: F)
    where
        F: Fn(&str) -> Vec<Suggestion> + 'static,
    {
        self.suggester = Some(Box::new(f));
    }

    /// Rows beyond this scroll rather than growing the panel without bound.
    #[must_use]
    pub fn with_max_visible(mut self, n: usize) -> Self {
        self.list = self.list.with_max_visible(n);
        self
    }

    /// For hosts that compute suggestions reactively rather than through a
    /// [`Suggester`]. Re-clamps the highlight and clears any status line.
    pub fn set_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        self.list.set_suggestions(suggestions);
    }

    /// For reactive/async hosts filling suggestions or a status line that
    /// arrived *after* the keystroke that requested them (which otherwise
    /// leaves the dropdown closed).
    pub fn open(&mut self) {
        self.open = true;
    }

    /// `None` clears it. Pair with [`open`](Autocomplete::open).
    pub fn set_status(&mut self, status: Option<String>) {
        self.list.set_status(status);
    }

    /// Results are on their way, with `clock`'s timing (asked when, from
    /// where): the framework's placeholder stands where the first row will
    /// be, and assistive technology hears the list is busy. `None` once
    /// they are in.
    pub fn set_arriving(&mut self, clock: Option<crate::arriving::ArrivalClock>) {
        self.list.set_arriving(clock);
    }

    /// Opens the dropdown when the field is actively being edited (for
    /// reactive/async hosts receiving results after a round-trip). Closes
    /// it when the list is empty.
    pub fn set_suggestions_open(&mut self, suggestions: Vec<Suggestion>) {
        self.set_suggestions(suggestions);
        if self.is_editing() {
            self.open = !self.list.suggestions().is_empty();
        }
    }

    pub fn text(&self) -> &str {
        self.input.value()
    }

    /// The field itself, for a host that binds its text (a shared draft).
    pub fn input(&self) -> &TextInput {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    /// The field's text after the person's edits since the last call,
    /// typed or chosen from the suggestions, if any.
    pub fn take_edited(&mut self) -> Option<String> {
        let typed = self.input.take_dirty().is_some();
        (std::mem::take(&mut self.edited) || typed).then(|| self.input.value().to_owned())
    }

    /// Replace the field text (does not open the dropdown or recompute);
    /// the caret waits at its end.
    pub fn set_text(&mut self, s: &str) {
        self.input.set_text(s);
    }

    /// Select-all runs **only** on the genuine transition into editing —
    /// never on a redundant call while already focused (a focus/refresh
    /// path can trigger that mid-edit), which would let the next keystroke
    /// overwrite what the user just typed, silently dropping a character.
    pub fn begin_edit(&mut self) {
        let entering = !self.input.is_focused();
        self.input.set_focus(true);
        if entering {
            self.input.select_all();
        }
        self.outcome = AutocompleteOutcome::None;
        self.recompute();
        self.open = !self.list.suggestions().is_empty();
        self.list.set_highlight(None);
    }

    pub fn is_editing(&self) -> bool {
        self.input.is_focused()
    }

    pub fn is_open(&self) -> bool {
        self.open && !self.list.suggestions().is_empty()
    }

    pub fn suggestions(&self) -> &[Suggestion] {
        self.list.suggestions()
    }

    pub fn highlighted(&self) -> Option<&Suggestion> {
        self.list.highlighted()
    }

    /// Clamped to range; `None` clears. Reactive hosts use this to
    /// highlight the top match by default.
    pub fn set_highlight(&mut self, index: Option<usize>) {
        self.list.set_highlight(index);
    }

    /// Close the dropdown (keeps the field text + focus).
    pub fn close(&mut self) {
        self.open = false;
        self.list.set_highlight(None);
        self.list.set_status(None);
        self.list.set_arriving(None);
    }

    /// Rising-edge read of how the most recent interaction ended.
    pub fn take_outcome(&mut self) -> AutocompleteOutcome {
        std::mem::replace(&mut self.outcome, AutocompleteOutcome::None)
    }

    /// For hosts embedding the field in their own immediate-mode chrome.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.input.apply_layout_external(rect);
        self.list.set_anchor(rect);
    }

    /// The suggestion list's rect while it shows, for a host that keeps
    /// its own hit-testing or places something beside it.
    pub fn list_rect(&self) -> Option<Rect> {
        self.dropdown_rect()
    }

    /// The suggestion list itself.
    pub fn list(&self) -> &SuggestionList {
        &self.list
    }

    fn recompute(&mut self) {
        if let Some(f) = &self.suggester {
            let suggestions = f(self.input.value());
            self.list.set_suggestions(suggestions);
        }
    }

    /// Accept `value`: emit the outcome, load it into the field, close.
    fn accept(&mut self, value: String) {
        self.input.set_text(&value);
        self.edited = true;
        self.close();
        self.outcome = AutocompleteOutcome::Accepted(value);
    }

    fn cancel(&mut self) {
        self.close();
        self.input.set_focus(false);
        self.outcome = AutocompleteOutcome::Cancelled;
    }

    /// Keeps the edit open so the user can drill further.
    fn complete_to_highlight(&mut self) {
        let idx = self
            .list
            .highlight()
            .or(if self.list.suggestions().is_empty() {
                None
            } else {
                Some(0)
            });
        if let Some(i) = idx
            && let Some(s) = self.list.suggestions().get(i)
        {
            let value = s.value.clone();
            self.input.set_text(&value);
            self.edited = true;
            self.recompute();
            self.open = !self.list.suggestions().is_empty();
            self.list.set_highlight(None);
        }
    }

    fn move_highlight(&mut self, forward: bool) {
        if self.list.suggestions().is_empty() {
            return;
        }
        self.open = true;
        self.list.move_highlight(forward);
    }

    /// `None` when it should not show. Shows for real rows or a status line.
    fn dropdown_rect(&self) -> Option<Rect> {
        if !self.open {
            return None;
        }
        self.list.rect()
    }

    #[cfg(test)]
    fn row_rect(&self, _panel: Rect, i: usize) -> Rect {
        self.list.row_rect(i).unwrap_or(Rect::ZERO)
    }

    /// The dropdown row under a logical point, if any.
    fn row_at(&self, pt: Point) -> Option<usize> {
        if !self.open {
            return None;
        }
        self.list.row_at(pt)
    }
}

impl Default for Autocomplete {
    fn default() -> Self {
        Self::new()
    }
}

impl Autocomplete {
    /// The `KeyboardInput` arm of [`Widget::handle_event`], split out to
    /// keep that under the line cap. Navigation/accept/cancel keys are
    /// intercepted here since the field itself never consumes them;
    /// everything else forwards to the field and recomputes on change.
    fn handle_key_while_editing(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
        logical_key: &Key,
    ) {
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_highlight(true);
                *ctx.handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_highlight(false);
                *ctx.handled = true;
            }
            // Enter takes the highlighted suggestion. Without one it
            // accepts the typed text and goes on to the host, so a form's
            // or dialog's default action still runs.
            Key::Named(NamedKey::Enter) => {
                if let Some(value) = self.highlighted().map(|s| s.value.clone()) {
                    self.accept(value);
                    *ctx.handled = true;
                } else {
                    let value = self.input.value().to_string();
                    self.accept(value);
                }
            }
            Key::Named(NamedKey::Tab) if !self.list.suggestions().is_empty() => {
                self.complete_to_highlight();
                *ctx.handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                self.cancel();
                *ctx.handled = true;
            }
            _ => {
                // Forward to the field, then recompute if the text changed.
                self.input.handle_event(event, ctx);
                if self.input.take_dirty().is_some() {
                    self.edited = true;
                    self.recompute();
                    self.open = !self.list.suggestions().is_empty();
                    self.list.set_highlight(None);
                }
            }
        }
    }
}

impl Widget for Autocomplete {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.input.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The field's own band: its control size, grown with its line.
        let height = f32_of_f64(self.input.band_in(theme));
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(height),
            },
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.apply_layout_external(crate::widget::layout_rect(taffy, node, origin));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(viewport) = presence.viewport.get() {
            self.list.set_viewport(viewport);
        }
        // The field itself; the suggestion list floats in `draw_overlay`.
        self.input.apply_layout_external(self.rect);
        self.input
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        if let Some(viewport) = presence.viewport.get() {
            self.list.set_viewport(viewport);
        }
        self.list.set_anchor(self.rect);
        self.list.paint(scene, transform, text_renderer, theme);
    }

    fn needs_animation_frame(&self) -> bool {
        self.open && self.list.needs_animation_frame()
    }

    /// A glide after a flick scrolls the list as the wheel does.
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        if self.open {
            self.list.advance();
        }
        false
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        // Rows on their way step their placeholder on at its deadlines;
        // the rows' arrival wakes the window itself.
        self.open
            .then(|| self.list.next_arriving_step(crate::time::now()))
            .flatten()
    }

    fn needs_redraw(&self) -> bool {
        self.open && self.list.arriving_moved()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer = ctx.logical(position);
            if self.open {
                self.list.scroll_event(event, ctx);
                self.list.hover_at(self.last_pointer);
            }
        }
        if !self.is_editing() {
            return;
        }
        match event {
            // Navigation / accept / cancel keys the field itself never consumes,
            // so the wrapper owns them. Intercept before forwarding.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key_while_editing(event, ctx, logical_key),
            // A wheel over an open list scrolls it; one over its inset, just
            // outside the rows, steps a row.
            WindowEvent::MouseWheel { delta, .. }
                if self
                    .dropdown_rect()
                    .is_some_and(|r| r.contains(self.last_pointer)) =>
            {
                if !self.list.scroll_event(event, ctx) {
                    let rows = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => -y.signum(),
                        winit::event::MouseScrollDelta::PixelDelta(p) => {
                            if p.y > 0.0 {
                                -1.0
                            } else if p.y < 0.0 {
                                1.0
                            } else {
                                0.0
                            }
                        }
                    };
                    #[allow(clippy::cast_possible_truncation, reason = "a signum of -1, 0 or 1")]
                    self.list.scroll_by(rows as i32);
                }
                *ctx.handled = true;
            }
            // A left press on a dropdown row accepts it.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.row_at(self.last_pointer).is_some() => {
                if let Some(i) = self.row_at(self.last_pointer)
                    && let Some(s) = self.list.suggestions().get(i)
                {
                    let value = s.value.clone();
                    self.accept(value);
                    *ctx.handled = true;
                }
            }
            // A left press outside the field and the dropdown cancels. Not
            // consumed, so the click still reaches whatever is under it.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.rect.contains(self.last_pointer)
                && self
                    .dropdown_rect()
                    .is_none_or(|r| !r.contains(self.last_pointer)) =>
            {
                self.cancel();
            }
            _ => {
                self.input.handle_event(event, ctx);
                if self.input.take_dirty().is_some() {
                    self.edited = true;
                    self.recompute();
                    self.open = !self.list.suggestions().is_empty();
                    self.list.set_highlight(None);
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "Autocomplete"
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("autocomplete")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// A host that composes the field into a larger form reaches it to
    /// offer suggestions and read what was accepted.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else if self.row_at(point).is_some() {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.input.set_focus(focused);
        if !focused {
            self.open = false;
        }
    }

    fn is_focused(&self) -> bool {
        self.input.is_focused()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.input.ime_edit_fingerprint(key)
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Widget::ime_content(&self.input)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.input.is_dragging()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::hash_key;
        let mut node = self.input.accessibility()?;
        let id = self.test_id().unwrap_or("autocomplete");
        // This widget owns the embedded input and its synthetic result rows.
        // Publish one keyed route for all of them instead of leaking the
        // inner TextInput's otherwise unhandled action target.
        node.action_target = None;
        node.key = Some(hash_key(&id));
        node.bounds = Some(self.rect.into());
        node.state.expanded = Some(self.open);
        node.state.busy = self.open && self.list.is_busy();
        if self.open {
            node.children.extend(self.list.access_rows(|suggestion| {
                hash_key(&("autocomplete-suggestion", suggestion.value.as_str()))
            }));
        }
        Some(node.with_keyed_action_routes(id))
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::{Action, hash_key};
        let own_key = hash_key(&self.test_id().unwrap_or("autocomplete"));
        if key_path.is_empty() || key_path == [own_key] {
            let (state, path) = crate::doc::document(state, path);
            return match action {
                Action::Focus | Action::Expand => {
                    self.begin_edit();
                    true
                }
                Action::Collapse => {
                    self.cancel();
                    true
                }
                _ => {
                    let handled = Widget::perform_action(&mut self.input, action, &[], state, path);
                    if handled && matches!(action, Action::SetValue(_)) {
                        self.input.set_focus(true);
                        self.outcome = AutocompleteOutcome::None;
                        self.recompute();
                        self.open = true;
                        self.list.set_highlight(None);
                    }
                    handled
                }
            };
        }
        if !self.open || !matches!(action, Action::Activate) {
            return false;
        }
        let Some(value) = self
            .list
            .suggestions()
            .iter()
            .find(|suggestion| {
                key_path.last()
                    == Some(&hash_key(&(
                        "autocomplete-suggestion",
                        suggestion.value.as_str(),
                    )))
            })
            .map(|suggestion| suggestion.value.clone())
        else {
            return false;
        };
        self.accept(value);
        true
    }
}

impl crate::widget::controls::ActionControls for Autocomplete {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        self.input.install_control_custody(domain, attachment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dispatch;
    use automerge::AutoCommit;

    #[test]
    fn suggestions_open_highlight_accept_or_cancel() {
        fn accessible_edits_update_suggestions_and_activate_the_selected_value() {
            use crate::a11y::{Action, hash_key};
            let mut ac = suggester_paths();
            let mut state = AutoCommit::new();
            assert!(ac.perform_action(
                &Action::SetValue("al".into()),
                &[],
                &mut crate::widget::Edits::lend(&mut state),
                &automerge::ROOT
            ));
            assert_eq!(ac.suggestions().len(), 2);
            let node = ac.accessibility().unwrap();
            assert_eq!(node.children[1].name.as_deref(), Some("album"));
            let key = hash_key(&("autocomplete-suggestion", "album"));
            assert!(ac.perform_action(
                &Action::Activate,
                &[key],
                &mut crate::widget::Edits::lend(&mut state),
                &automerge::ROOT
            ));
            assert_eq!(ac.text(), "album");
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::Accepted("album".into())
            );
            assert!(!ac.is_open());
            assert!(!ac.perform_action(
                &Action::Activate,
                &[key],
                &mut crate::widget::Edits::lend(&mut state),
                &automerge::ROOT
            ));
        }

        fn typing_recomputes_and_opens_the_dropdown() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            // The field starts empty, so both letters insert normally (no select-all
            // to swallow the first one).
            dispatch(&mut ac, &char_key("a"));
            dispatch(&mut ac, &char_key("l"));
            assert_eq!(ac.text(), "al");
            let vals: Vec<&str> = ac.suggestions().iter().map(|s| s.value.as_str()).collect();
            assert_eq!(vals, ["alpha", "album"], "prefix filter applied");
            assert!(ac.is_open(), "dropdown opens as the user types");
        }

        fn arrows_move_highlight_and_enter_accepts_it() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("a"));
            dispatch(&mut ac, &char_key("l"));
            // No highlight yet → Enter would commit the typed text. Arrow down arms
            // the first suggestion.
            assert!(ac.highlighted().is_none());
            assert!(dispatch(&mut ac, &named_key(NamedKey::ArrowDown)));
            assert_eq!(ac.highlighted().unwrap().value, "alpha");
            assert!(dispatch(&mut ac, &named_key(NamedKey::ArrowDown)));
            assert_eq!(ac.highlighted().unwrap().value, "album");
            assert!(dispatch(&mut ac, &named_key(NamedKey::Enter)));
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::Accepted("album".into())
            );
            assert_eq!(ac.text(), "album", "accepted value loaded into the field");
            assert!(!ac.is_open(), "accept closes the dropdown");
        }

        fn enter_without_a_highlight_commits_the_typed_text() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("b"));
            assert!(
                !dispatch(&mut ac, &named_key(NamedKey::Enter)),
                "Enter goes on to the form's default action"
            );
            assert_eq!(ac.take_outcome(), AutocompleteOutcome::Accepted("b".into()));
        }

        fn escape_cancels() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("a"));
            assert!(dispatch(&mut ac, &named_key(NamedKey::Escape)));
            assert_eq!(ac.take_outcome(), AutocompleteOutcome::Cancelled);
            assert!(!ac.is_open());
        }

        fn clicking_a_row_accepts_that_suggestion() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("a")); // → "alpha","album"
            let panel = ac.dropdown_rect().expect("dropdown open");
            // Click the second row ("album").
            let row = ac.row_rect(panel, 1);
            let c = row.center();
            dispatch(&mut ac, &cursor_moved(c.x, c.y));
            assert!(dispatch(&mut ac, &mouse_press()));
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::Accepted("album".into())
            );
        }

        fn tab_completes_without_accepting() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("a")); // "alpha","album"
            // Tab fills the field with the first suggestion but does not commit.
            assert!(dispatch(&mut ac, &named_key(NamedKey::Tab)));
            assert_eq!(ac.text(), "alpha");
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::None,
                "Tab is not an accept"
            );
        }

        fn click_outside_cancels_without_consuming() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("a"));
            // Move far away, then press: cancels but is not swallowed.
            dispatch(&mut ac, &cursor_moved(900.0, 900.0));
            let consumed = dispatch(&mut ac, &mouse_press());
            assert!(!consumed, "the outside click still reaches the host");
            assert_eq!(ac.take_outcome(), AutocompleteOutcome::Cancelled);
        }

        accessible_edits_update_suggestions_and_activate_the_selected_value();
        typing_recomputes_and_opens_the_dropdown();
        arrows_move_highlight_and_enter_accepts_it();
        enter_without_a_highlight_commits_the_typed_text();
        escape_cancels();
        clicking_a_row_accepts_that_suggestion();
        tab_completes_without_accepting();
        click_outside_cancels_without_consuming();
    }

    #[test]
    fn controlled_autocomplete_with_id_preserves_supplied_input() {
        let autocomplete = Autocomplete::new_with_controlled_input(
            TextInput::local_with_language("en_US")
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_initial_value("sentinel route"),
        )
        .with_id("route-search");

        assert_eq!(Widget::test_id(&autocomplete), Some("route-search"));
        let semantics = Widget::accessibility(&autocomplete).expect("text field semantics");
        // The autocomplete publishes one keyed route under its id for the
        // embedded input and its rows, never the inner input's own target.
        let keyed = semantics.action_target.as_deref().unwrap_or_default();
        assert!(
            keyed.starts_with("tilo.key-action:route-search:"),
            "{keyed}"
        );
        assert_eq!(semantics.semantic_key.as_deref(), Some(keyed));
        assert_eq!(autocomplete.text(), "sentinel route");
        assert_eq!(autocomplete.input.spell_language(), "en_US");
        assert!(!autocomplete.input.spellcheck_enabled());
        assert!(!autocomplete.input.autocorrect_enabled());
        assert_eq!(autocomplete.input.kind(), TextInputKind::Search);
    }

    fn suggester_paths() -> Autocomplete {
        Autocomplete::new().with_suggester(|q| {
            ["alpha", "album", "beta"]
                .iter()
                .filter(|s| s.starts_with(q))
                .map(|s| Suggestion::new(*s))
                .collect()
        })
    }

    use crate::synthetic_input::char_key;

    use crate::synthetic_input::named_key;

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    #[test]
    fn begin_edit_selection_arms_only_for_preseeded_values() {
        fn freshly_focused_empty_field_keeps_every_letter() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("a"));
            assert_eq!(
                ac.input.selection_range_public(),
                None,
                "no selection after first char"
            );
            dispatch(&mut ac, &char_key("b"));
            dispatch(&mut ac, &char_key("c"));
            assert_eq!(ac.text(), "abc", "every letter is kept (not \"bc\")");
        }

        fn begin_edit_on_empty_field_arms_no_selection() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            assert_eq!(
                ac.input.selection_range_public(),
                None,
                "empty focus = no selection"
            );
        }

        fn redundant_begin_edit_does_not_reselect_typed_text() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            dispatch(&mut ac, &char_key("a")); // field now holds "a"
            assert_eq!(ac.text(), "a");
            ac.begin_edit();
            assert_eq!(
                ac.input.selection_range_public(),
                None,
                "typed text is not re-selected"
            );
            dispatch(&mut ac, &char_key("l"));
            assert_eq!(
                ac.text(),
                "al",
                "second char appends — the first letter survives"
            );
        }

        fn begin_edit_still_selects_a_preseeded_value_for_replace() {
            // A genuine fresh entry into a pre-filled field (the path bar seeds
            // the current path) still selects all.
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.set_text("/home/user");
            ac.begin_edit();
            assert_eq!(
                ac.input.selection_range_public(),
                Some((0, "/home/user".len())),
                "fresh entry into a pre-seeded field selects the whole value",
            );
            dispatch(&mut ac, &char_key("x"));
            assert_eq!(
                ac.text(),
                "x",
                "the first keystroke replaces the selected seed"
            );
        }

        freshly_focused_empty_field_keeps_every_letter();
        begin_edit_on_empty_field_arms_no_selection();
        redundant_begin_edit_does_not_reselect_typed_text();
        begin_edit_still_selects_a_preseeded_value_for_replace();
    }

    #[test]
    fn reactive_suggestions_status_line_and_scaled_rows() {
        fn set_suggestions_supports_reactive_hosts() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            ac.set_suggestions(vec![Suggestion::labelled("/home/u/Docs", "Docs")]);
            ac.open = true;
            assert!(ac.is_open());
            assert_eq!(ac.suggestions()[0].value, "/home/u/Docs");
        }

        fn status_line_keeps_the_dropdown_open_with_no_rows() {
            // A reactive host shows a loading line while it has no rows yet.
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            assert!(ac.dropdown_rect().is_none(), "nothing to show yet");
            ac.set_status(Some("Searching…".into()));
            ac.open();
            let panel = ac.dropdown_rect().expect("status keeps the dropdown open");
            assert!(panel.height() > 0.0);
            assert!(ac.highlighted().is_none());
            assert!(
                ac.row_at(panel.center()).is_none(),
                "the status line is not a row"
            );
        }

        fn reactive_suggestions_after_open_replace_the_status_line() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            ac.set_status(Some("Searching…".into()));
            ac.open();
            ac.set_suggestions(vec![Suggestion::new("Paris"), Suggestion::new("Parma")]);
            ac.open();
            assert!(ac.is_open());
            assert_eq!(ac.suggestions().len(), 2);
            dispatch(&mut ac, &named_key(NamedKey::ArrowDown));
            assert_eq!(ac.highlighted().unwrap().value, "Paris");
            dispatch(&mut ac, &named_key(NamedKey::ArrowDown));
            assert_eq!(ac.highlighted().unwrap().value, "Parma");
        }

        fn large_text_scales_dropdown_rows_and_hit_geometry_together() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 68.0));
            ac.set_suggestions(vec![
                Suggestion::new("A very long destination name").with_detail("First detail"),
                Suggestion::new("Second destination").with_detail("Second detail"),
            ]);
            ac.open();
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let theme = Theme::dark().with_text_scale(2.0);
            text.set_text_scale(theme.text_scale);
            ac.draw_overlay(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
            let panel = ac.dropdown_rect().expect("large-text dropdown");
            let first = ac.row_rect(panel, 0);
            let second = ac.row_rect(panel, 1);
            assert!(
                first.height() > f64::from(crate::tokens::Metrics::BASE.menu_row_height),
                "rows grow with the text: {first:?}"
            );
            assert_eq!(first.y1, second.y0, "rows do not overlap");
            assert_eq!(ac.row_at(second.center()), Some(1));
        }

        set_suggestions_supports_reactive_hosts();
        status_line_keeps_the_dropdown_open_with_no_rows();
        reactive_suggestions_after_open_replace_the_status_line();
        large_text_scales_dropdown_rows_and_hit_geometry_together();
    }
    #[test]
    fn a_short_label_leaves_its_detail_the_rest_of_the_row() {
        // "Amsterdam" beside "North Holland, The Netherlands" in a 300 px row:
        // the detail gets what the label leaves, not a fixed half.
        assert!((detail_room(300.0, 80.0, 8.0) - 204.0).abs() < f64::EPSILON);
        assert!((detail_room(300.0, 250.0, 8.0) - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_suggestion_list_flips_scrolls_and_waits() {
        let mut list = SuggestionList::new().with_max_visible(3);
        list.set_suggestions(
            (0..10)
                .map(|i| Suggestion::new(format!("item {i}")).with_detail("fn"))
                .collect(),
        );
        list.set_anchor(Rect::new(10.0, 560.0, 210.0, 592.0));
        list.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        let rect = list.rect().expect("rows show");
        assert!(
            rect.y1 <= 560.0,
            "no room below: the list opens above: {rect:?}"
        );
        list.set_highlight(Some(7));
        assert!(
            list.row_rect(7).is_some(),
            "the armed row scrolls into view"
        );
        assert!(list.row_rect(0).is_none());
        let row = list.row_rect(7).unwrap();
        assert_eq!(list.row_at(row.center()), Some(7));

        let mut busy = SuggestionList::new();
        busy.set_anchor(Rect::new(0.0, 0.0, 200.0, 32.0));
        assert!(busy.rect().is_none());
        let asked = crate::time::now();
        busy.set_arriving(Some(crate::arriving::ArrivalClock::new(
            asked,
            crate::arriving::Source::host("Open-Meteo"),
        )));
        assert!(busy.rect().is_some());
        let rows = busy.access_rows(|_| 0);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].state.busy);
        let mut scene = Scene::new();
        busy.paint(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
        );

        let mut ac = Autocomplete::with_input(TextInput::local().with_placeholder("City"));
        ac.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 32.0));
        ac.set_arriving(Some(crate::arriving::ArrivalClock::new(
            asked,
            crate::arriving::Source::host("Open-Meteo"),
        )));
        ac.open();
        assert!(ac.list_rect().is_some());
        assert!(
            !Widget::needs_animation_frame(&ac),
            "rows on their way draw no frames between their steps"
        );
        assert_eq!(
            Widget::next_wake_deadline(&ac),
            crate::arriving::ArrivalClock::new(asked, crate::arriving::Source::host("Open-Meteo"))
                .next_step_at(crate::time::now()),
            "the placeholder steps on at its deadline"
        );
    }
}

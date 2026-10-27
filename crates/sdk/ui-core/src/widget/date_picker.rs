//! `DatePicker` widget — month-grid calendar picker.
//!
//! Renders a header (prev-month chevron, "Month YYYY" label, next-month
//! chevron) above a 7-column day grid. The first column is **Monday** —
//! this is the locale assumption pinned by
//! `date_picker_first_day_of_week_is_monday` so a future locale change
//! must touch both the test and `first_visible_col`.
//!
//! The visible month remains presentation state. The selected date can stay
//! widget-local, bind directly to typed process-local state with
//! [`DatePicker::from_state`], or be observed with `on_change` for an
//! app-owned durable mutation.

use crate::widget::{AppModel, ModelPath};

mod feed;
use chrono::{Datelike, NaiveDate};
pub use feed::{DateFeed, DateShown};
use std::collections::{HashMap, HashSet};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::text::centered_text_top;

/// Six rows covers every possible month layout — a 31-day month starting
/// on Sunday is the worst case, needing 6 rows to fit the spill-in/out
/// days from neighbouring months.
const GRID_ROWS: usize = 6;
const GRID_COLS: usize = 7;

/// Weekday headings rendered above the grid. Pinned to a Monday-first
/// locale by the `date_picker_first_day_of_week_is_monday` test.
const WEEKDAY_LABELS: [&str; GRID_COLS] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

pub struct DatePicker {
    /// Fluent message id spoken for the composite calendar control.
    accessible_name: Option<String>,
    selected: NaiveDate,
    month_view: (i32, u32),
    on_change: Option<Box<dyn FnMut(NaiveDate)>>,
    local_state: Option<crate::local_state::LocalBinding<NaiveDate>>,
    rect: Rect,
    day_rects: Vec<(NaiveDate, Rect)>,
    prev_button_rect: Rect,
    next_button_rect: Rect,
    last_cursor: Point,
    /// Whether the picker holds keyboard focus. Drives a focus ring around
    /// the whole calendar so the control reads as focused.
    is_focused: bool,
    /// The sidebar presentation: no frame, as wide as its column, as tall
    /// as that width needs.
    navigator: bool,
    /// Days that hold something (an event, a note): a dot under the number.
    marked: HashSet<NaiveDate>,
    /// The day the host calls today; `None` reads the toolkit clock.
    today: Option<NaiveDate>,
    /// The app's day, when the app feeds the picker ([`DateFeed`]).
    feed: Option<DateFeed>,
    /// The feed's revision the picker last read.
    feed_seen: Option<u64>,
}

impl DatePicker {
    pub fn new(initial: NaiveDate) -> Self {
        Self {
            accessible_name: None,
            selected: initial,
            month_view: (initial.year(), initial.month()),
            on_change: None,
            local_state: None,
            rect: Rect::ZERO,
            day_rects: Vec::new(),
            prev_button_rect: Rect::ZERO,
            next_button_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            is_focused: false,
            navigator: false,
            marked: HashSet::new(),
            today: None,
            feed: None,
            feed_seen: None,
        }
    }

    /// The sidebar presentation of a calendar app: a month that navigates
    /// the main view. It stands on its region without a frame, takes its
    /// column's width and is as tall as that width needs.
    #[must_use]
    pub fn navigator(mut self) -> Self {
        self.navigator = true;
        self
    }

    /// Mark `dates` (days with events) with a dot under the number.
    #[must_use]
    pub fn marked(mut self, dates: impl IntoIterator<Item = NaiveDate>) -> Self {
        self.set_marked(dates);
        self
    }

    /// The day the today ring marks, from the host's own notion of today
    /// (its time zone, a staged capture). Without it the picker reads
    /// `ui::clock::today()`.
    #[must_use]
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }

    /// Replace the host's today; see [`Self::with_today`].
    pub fn set_today(&mut self, today: NaiveDate) {
        self.today = Some(today);
    }

    /// Replace the marked days; see [`Self::marked`].
    pub fn set_marked(&mut self, dates: impl IntoIterator<Item = NaiveDate>) {
        self.marked = dates.into_iter().collect();
    }

    /// The height the picker needs `width` wide: the header, the weekday
    /// row and six rows of cells as tall as they are wide (capped).
    #[must_use]
    pub fn intrinsic_height(&self, width: f64) -> f64 {
        let cell = if self.navigator {
            // The smallest cell a narrow sidebar gets is six spacing steps.
            (width / grid_f64(GRID_COLS))
                .clamp(f64::from(crate::tokens::Spacing::BASE.s6), CELL_SIZE)
        } else {
            CELL_SIZE
        };
        (HEADER_HEIGHT + WEEKDAY_HEIGHT + cell * grid_f64(GRID_ROWS)).ceil()
    }

    /// Create a date picker bound to typed process-local state.
    ///
    /// Pointer, keyboard, and semantic changes update `state`; mutations
    /// through any clone update the selected date and visible month on the
    /// next synchronization pass.
    pub fn from_state(state: &crate::LocalState<NaiveDate>) -> Self {
        let (selected, binding) = crate::local_state::LocalBinding::new(state);
        Self {
            local_state: Some(binding),
            ..Self::new(selected)
        }
    }

    pub fn selected(&self) -> NaiveDate {
        self.selected
    }

    /// Name the date picker for assistive technology.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    pub fn month_view(&self) -> (i32, u32) {
        self.month_view
    }

    #[must_use]
    pub fn on_change<F: FnMut(NaiveDate) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Wraps December → January. Does **not** move the selected date —
    /// the user must click a day to change selection.
    pub fn advance_month(&mut self) {
        let (y, m) = self.month_view;
        self.month_view = if m >= 12 {
            (y.saturating_add(1), 1)
        } else {
            (y, m.saturating_add(1))
        };
    }

    /// Wraps January → December.
    pub fn retreat_month(&mut self) {
        let (y, m) = self.month_view;
        self.month_view = if m <= 1 {
            (y.saturating_sub(1), 12)
        } else {
            (y, m.saturating_sub(1))
        };
    }

    /// Set the date programmatically without firing `on_change`.
    /// A bound [`crate::LocalState`] is updated immediately.
    pub fn set_selected(&mut self, date: NaiveDate) {
        self.selected = date;
        self.month_view = (date.year(), date.month());
        self.publish_local_state();
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.local_state {
            binding.publish(&self.selected);
        }
    }

    fn commit_user_date(&mut self, date: NaiveDate) {
        self.selected = date;
        self.month_view = (date.year(), date.month());
        self.publish_local_state();
        if let Some(callback) = self.on_change.as_mut() {
            callback(date);
        }
    }

    /// The visible month follows the selection so arrowing off the end of
    /// a month reveals the next one, matching every native date grid.
    fn move_selection(&mut self, days: i64) {
        if let Some(d) = self
            .selected
            .checked_add_signed(chrono::Duration::days(days))
        {
            self.commit_user_date(d);
        }
    }

    /// Column index (0..=6) of the first-of-month, Monday = 0.
    pub fn first_visible_col(&self) -> usize {
        let (y, m) = self.month_view;
        // Falls back to column 0 on an invalid year range.
        NaiveDate::from_ymd_opt(y, m, 1).map_or(0, |d| d.weekday().num_days_from_monday() as usize)
    }

    /// Shared by the taffy [`Widget::apply_layout`] path and the
    /// taffy-free [`Self::apply_layout_external`] embed path.
    fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;

        self.prev_button_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + CHEVRON_WIDTH,
            self.rect.y0 + HEADER_HEIGHT,
        );
        self.next_button_rect = Rect::new(
            self.rect.x1 - CHEVRON_WIDTH,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + HEADER_HEIGHT,
        );

        // Derived from the host rect, not the build_layout default, so the
        // grid stretches/shrinks with whatever width layout assigns.
        let grid_top = self.rect.y0 + HEADER_HEIGHT + WEEKDAY_HEIGHT;
        let cell_w = self.rect.width() / grid_f64(GRID_COLS);
        let cell_h = (self.rect.y1 - grid_top) / grid_f64(GRID_ROWS);
        self.day_rects.clear();
        for row in 0..GRID_ROWS {
            for col in 0..GRID_COLS {
                if let Some(date) = self.cell_date(row, col) {
                    let x0 = self.rect.x0 + grid_f64(col) * cell_w;
                    let y0 = grid_top + grid_f64(row) * cell_h;
                    let cell_rect = Rect::new(x0, y0, x0 + cell_w, y0 + cell_h);
                    self.day_rects.push((date, cell_rect));
                }
            }
        }
    }

    /// For a host that hand-positions the picker inside a popover; draws
    /// and hit-tests exactly as under [`Widget::apply_layout`].
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.set_rect(rect);
    }

    /// Hit regions for an embedded host's existing retained hit map. The
    /// regions are derived by the same shared layout code used for drawing.
    pub fn hit_regions_external(&self) -> Vec<(ExternalDatePickerHit, Rect)> {
        let mut regions = vec![
            (ExternalDatePickerHit::PreviousMonth, self.prev_button_rect),
            (ExternalDatePickerHit::NextMonth, self.next_button_rect),
        ];
        regions.extend(
            self.day_rects
                .iter()
                .map(|(date, rect)| (ExternalDatePickerHit::Date(*date), *rect)),
        );
        regions
    }

    /// Days before the first-of-month spill in from the previous month;
    /// days past the end spill into the next.
    fn cell_date(&self, row: usize, col: usize) -> Option<NaiveDate> {
        let (y, m) = self.month_view;
        let first = NaiveDate::from_ymd_opt(y, m, 1)?;
        // row < GRID_ROWS (6) and col < GRID_COLS (7), so the grid index
        // and first_visible_col() (0..=6) are always tiny; saturating
        // arithmetic and try_from keep the conversion safe without
        // ever panicking or wrapping even outside that invariant.
        let grid_index =
            i64::try_from(row.saturating_mul(GRID_COLS).saturating_add(col)).unwrap_or(i64::MAX);
        let first_col = i64::try_from(self.first_visible_col()).unwrap_or(0);
        let offset = grid_index.saturating_sub(first_col);
        first.checked_add_signed(chrono::Duration::days(offset))
    }
}

/// Widens a small grid index or dimension (`GRID_ROWS`/`GRID_COLS`, or a
/// loop index bounded by one of them) to `f64`. Both constants are tiny
/// (6 and 7), so the widening can never lose precision.
fn grid_f64(v: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "v is always GRID_ROWS, GRID_COLS, or a loop index bounded by one of them \
                  (6 or 7), far below f64's exact-integer range"
    )]
    let out = v as f64;
    out
}

/// Narrows a small, compile-time-bounded layout dimension to `f32` for
/// taffy. Every call site is `CELL_SIZE`/`HEADER_HEIGHT`/`WEEKDAY_HEIGHT`
/// arithmetic over `GRID_ROWS`/`GRID_COLS`, always far below `f32::MAX`.
fn px(v: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "v is built from small fixed layout constants, never approaching f32::MAX"
    )]
    let out = v as f32;
    out
}

const HEADER_HEIGHT: f64 = 32.0;
const WEEKDAY_HEIGHT: f64 = 20.0;
const CELL_SIZE: f64 = 32.0;
const CHEVRON_WIDTH: f64 = 32.0;

/// A host-side hit from an externally laid-out [`DatePicker`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalDatePickerHit {
    PreviousMonth,
    NextMonth,
    Date(NaiveDate),
}

impl Widget for DatePicker {
    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        if let Some(date) = self
            .local_state
            .as_mut()
            .and_then(crate::local_state::LocalBinding::poll)
        {
            self.selected = date;
            self.month_view = (date.year(), date.month());
        }
        self.pull_feed();
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.pull_feed()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let width = CELL_SIZE * grid_f64(GRID_COLS);
        if self.navigator {
            // As wide as its column; `measure` answers the height.
            let node = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                max_size: Size {
                    width: Dimension::length(px(width)),
                    height: Dimension::auto(),
                },
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        let height = self.intrinsic_height(width);
        let style = Style {
            size: Size::from_lengths(px(width), px(height)),
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        _text: &mut TextRenderer,
    ) -> Size<f32> {
        let full = CELL_SIZE * grid_f64(GRID_COLS);
        let width = known.width.map_or_else(
            || match available.width {
                taffy::AvailableSpace::Definite(room) => f64::from(room).min(full),
                _ => full,
            },
            f64::from,
        );
        Size {
            width: px(width),
            height: known
                .height
                .unwrap_or_else(|| px(self.intrinsic_height(width))),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.set_rect(crate::widget::layout_rect(taffy, node, origin));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;

        if self.navigator {
            // A navigator stands on its region: no face, no edge; focus is
            // the ring round the selected day's cell.
            self.draw_header(scene, transform, text_renderer, theme);
            self.draw_weekday_labels(scene, transform, text_renderer, theme);
            self.draw_day_grid(scene, transform, text_renderer, theme);
            return;
        }
        let radius = f64::from(tokens.radii.control);
        let fill = tokens.colors.surfaces.raised;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            fill,
            None,
            &RoundedRect::from_rect(self.rect, radius),
        );

        // A Control at rest carries an edge derived from its own surface
        // (HIG §9.5) — without it the calendar was a raised rectangle that
        // vanished on a raised card. Focus adds `border_focus` on the same
        // half-pixel-inset outline, so the ring sits exactly over the rest
        // edge and a focused picker is the rest picker plus one stroke.
        let outline = crate::surface::edge_outline(self.rect, radius);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(fill),
            None,
            &outline,
        );
        if self.is_focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                tokens.colors.border_focus,
                None,
                &outline,
            );
        }

        self.draw_header(scene, transform, text_renderer, theme);
        self.draw_weekday_labels(scene, transform, text_renderer, theme);
        self.draw_day_grid(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                // Arrows walk a day/week; PageUp/PageDown page the month;
                // Enter/Space (re-)commit. Every move fires `on_change`.
                match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.move_selection(-1);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        self.move_selection(1);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.move_selection(-7);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.move_selection(7);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.retreat_month();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::PageDown) => {
                        self.advance_month();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        self.commit_user_date(self.selected);
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_cursor;
                if self.prev_button_rect.contains(pt) {
                    self.retreat_month();
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                    return;
                }
                if self.next_button_rect.contains(pt) {
                    self.advance_month();
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                    return;
                }
                // Cloned so mutating `selected` doesn't borrow `self.day_rects`.
                let hit = self
                    .day_rects
                    .iter()
                    .find(|(_, r)| r.contains(pt))
                    .map(|(d, _)| *d);
                if let Some(date) = hit {
                    // Slides the visible month on a spill-in/out click,
                    // matching every common picker UX.
                    self.commit_user_date(date);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "DatePicker"
    }
    fn test_id(&self) -> Option<&str> {
        Some("date_picker")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    /// Value is the selected date in ISO `YYYY-MM-DD` form; `expanded` is
    /// left unset since the grid is rendered inline. A picker the app feeds
    /// ([`DateFeed`]) is addressed: what it chooses goes to the app's feed,
    /// so it is a bound control, reached by its name.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let node = AccessNode::new(Role::ComboBox)
            .with_optional_name(self.accessible_name.as_ref().map(|name| crate::i18n!(name)))
            .with_value(self.selected.format("%Y-%m-%d").to_string())
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused()),
            );
        Some(if self.feed.is_some() {
            node.with_action_target(format!(
                "date-picker:{}",
                self.accessible_name.as_deref().unwrap_or("date")
            ))
        } else {
            node
        })
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        match action {
            Action::SetValue(value) => {
                let Ok(date) = NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d") else {
                    return false;
                };
                self.commit_user_date(date);
                true
            }
            Action::Increment => {
                self.move_selection(1);
                true
            }
            Action::Decrement => {
                self.move_selection(-1);
                true
            }
            _ => false,
        }
    }
}

impl DatePicker {
    /// Month label + prev/next chevrons. Split out of `draw` to keep that
    /// function readable.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let text_style = tokens.typography.style(TextRole::Caption);
        let text_color = tokens.colors.text_primary;

        let (y, m) = self.month_view;
        let month_name = month_name_en(m);
        let header_label = format!("{month_name} {y}");
        // Every line in the picker is centred on its *measured* height; the
        // font size alone sat the 12 px labels 1.5 px low in each band.
        let (label_w, label_h) = text_renderer.measure_style(&header_label, None, &text_style);
        let label_x = self.rect.center().x - f64::from(label_w) / 2.0;
        let label_y = centered_text_top(self.rect.y0, HEADER_HEIGHT, f64::from(label_h));
        text_renderer.draw_style(
            scene,
            &header_label,
            text_color,
            transform.pre_translate(Vec2::new(label_x, label_y)),
            None,
            &text_style,
        );

        // Month steps are chevrons from the one icon set, centred on the
        // header band.
        let edge = f64::from(tokens.metrics.icon_sm);
        for (button, icon) in [
            (self.prev_button_rect, crate::icons::Icon::ChevronLeft),
            (self.next_button_rect, crate::icons::Icon::ChevronRight),
        ] {
            let centre =
                vello::kurbo::Point::new(button.center().x, self.rect.y0 + HEADER_HEIGHT / 2.0);
            crate::icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(centre, (edge, edge)),
                icon,
                text_color,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }

    /// The Mo/Tu/We/… row above the day grid. Split out of `draw` to keep
    /// that function readable.
    fn draw_weekday_labels(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let text_style = tokens.typography.style(TextRole::Caption);
        let dim_color = tokens.colors.text_dim;

        let cell_w = self.rect.width() / grid_f64(GRID_COLS);
        let weekday_band_y0 = self.rect.y0 + HEADER_HEIGHT;
        for (col, label) in WEEKDAY_LABELS.iter().enumerate() {
            let (lw, lh) = text_renderer.measure_style(label, None, &text_style);
            let weekday_y = centered_text_top(weekday_band_y0, WEEKDAY_HEIGHT, f64::from(lh));
            let cx = self.rect.x0 + grid_f64(col) * cell_w + cell_w / 2.0;
            text_renderer.draw_style(
                scene,
                label,
                dim_color,
                transform.pre_translate(Vec2::new(cx - f64::from(lw) / 2.0, weekday_y)),
                None,
                &text_style,
            );
        }
    }

    /// The 7×6 day cells: selection fill, today ring and day-number text.
    /// Split out of `draw` to keep that function readable.
    fn draw_day_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let text_style = tokens.typography.style(TextRole::Caption);
        let text_color = tokens.colors.text_primary;
        let dim_color = tokens.colors.text_dim;

        let today = self.today.unwrap_or_else(crate::clock::today);
        let (vy, vm) = self.month_view;

        for (date, cell_rect) in &self.day_rects {
            let is_in_view_month = date.year() == vy && date.month() == vm;
            let is_selected = *date == self.selected;
            let is_today = *date == today && is_in_view_month;

            // A day is a disc: the selection fill, the today ring and the
            // focus ring share one circle inside the cell.
            let disc = day_disc(*cell_rect);
            if is_selected {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.intent.accent,
                    None,
                    &disc,
                );
            }

            // On top of the cell fill, below the text, so it's visible even
            // when selected.
            if is_today {
                let ring = vello::kurbo::Circle::new(disc.center, disc.radius - 0.5);
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &ring,
                );
            }

            let day_str = date.day().to_string();
            let color = if is_selected {
                tokens.colors.intent.on_accent
            } else if is_in_view_month {
                text_color
            } else {
                dim_color
            };
            let (tw, th) = text_renderer.measure_style(&day_str, None, &text_style);
            let tx = cell_rect.center().x - f64::from(tw) / 2.0;
            let ty = centered_text_top(cell_rect.y0, cell_rect.height(), f64::from(th));
            // A marked day carries a dot under its number, in its ink.
            if self.marked.contains(date) {
                let r = (cell_rect.height().min(cell_rect.width()) * 0.06).max(1.5);
                let dot = vello::kurbo::Circle::new(
                    Point::new(
                        cell_rect.center().x,
                        (ty + f64::from(th) + r + 1.0).min(cell_rect.y1 - r - 1.0),
                    ),
                    r,
                );
                scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &dot);
            }
            if self.navigator && self.is_focused && is_selected {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &vello::kurbo::Circle::new(disc.center, disc.radius + 1.0),
                );
            }
            text_renderer.draw_style(
                scene,
                &day_str,
                color,
                transform.pre_translate(Vec2::new(tx, ty)),
                None,
                &text_style,
            );
        }
    }
}

fn month_name_en(m: u32) -> &'static str {
    match m {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "",
    }
}

/// The disc a day stands on: centred in its cell, a tenth of the cell in
/// from its shorter side.
fn day_disc(cell: Rect) -> vello::kurbo::Circle {
    let side = cell.height().min(cell.width());
    vello::kurbo::Circle::new(cell.center(), side * 0.4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use automerge::AutoCommit;
    use std::cell::RefCell;
    use std::rc::Rc;
    use taffy::prelude::TaffyMaxContent;

    /// A month the app feeds hands what is chosen to the app, so the
    /// native app contract finds it bound, addressed by its name.
    #[test]
    fn a_fed_month_is_a_bound_control() {
        let feed = DateFeed::new();
        let day = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        feed.show(DateShown {
            selected: day,
            today: day,
            marked: Vec::new(),
        });
        let host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            DatePicker::new(day)
                .navigator()
                .with_accessible_name("Month")
                .fed_by(&feed),
            (400, 400),
            Theme::dark(),
        );
        let audit = host.native_app_audit();
        assert!(audit.issues.is_empty(), "{audit:?}");
        assert_eq!(
            host.root::<DatePicker>()
                .accessibility()
                .and_then(|node| node.action_target),
            Some("date-picker:Month".to_owned())
        );
    }

    #[test]
    fn clicking_a_date_focuses_the_grid_for_keyboard_navigation() {
        let initial = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        let state = crate::LocalState::new(initial);
        let mut host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            DatePicker::from_state(&state),
            (400, 400),
            Theme::dark(),
        );
        let target = NaiveDate::from_ymd_opt(2024, 6, 7).unwrap();
        let point = host
            .root::<DatePicker>()
            .day_rects
            .iter()
            .find(|(date, _)| *date == target)
            .unwrap()
            .1
            .center();
        host.click_at(point.x, point.y);
        assert!(host.root::<DatePicker>().is_focused());
        assert_eq!(state.get(), target);
        host.key("Right");
        assert_eq!(state.get(), target.succ_opt().unwrap());
    }

    #[test]
    fn date_picker_month_navigation_and_selection() {
        fn date_picker_initial_state_shows_selected_month() {
            let dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
            assert_eq!(dp.month_view(), (2024, 6));
        }

        fn date_picker_next_month_advances() {
            let mut dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
            dp.advance_month();
            assert_eq!(dp.month_view(), (2024, 7));

            let mut dp_dec = DatePicker::new(NaiveDate::from_ymd_opt(2024, 12, 1).unwrap());
            dp_dec.advance_month();
            assert_eq!(dp_dec.month_view(), (2025, 1));
        }

        fn date_picker_clicking_day_updates_selection() {
            let initial = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();

            let captured: Rc<RefCell<Option<NaiveDate>>> = Rc::new(RefCell::new(None));
            let captured_cb = captured.clone();
            let mut dp = DatePicker::new(initial).on_change(move |d| {
                *captured_cb.borrow_mut() = Some(d);
            });

            let theme = crate::theme::Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = dp.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(node, taffy::Size::MAX_CONTENT)
                .unwrap();
            dp.apply_layout(&taffy, node, Point::ZERO);

            let target = NaiveDate::from_ymd_opt(2024, 6, 7).unwrap();
            let cell = dp
                .day_rects
                .iter()
                .find(|(d, _)| *d == target)
                .map(|(_, r)| *r)
                .expect("June 7 2024 cell exists in the visible grid");

            let pt = cell.center();
            let mut at_node = Dispatch::new().layout(&taffy, node);
            at_node.send(&mut dp, &cursor_moved(pt.x, pt.y));
            at_node.send(&mut dp, &mouse_press());

            assert_eq!(dp.selected(), target);
            assert_eq!(*captured.borrow(), Some(target));
        }

        /// winit's `KeyEvent` has no public constructor, so (as `TimePicker`'s
        /// keyboard tests do) this drives `move_selection` directly.
        fn date_picker_keyboard_nav_moves_selection_and_emits() {
            let captured: Rc<RefCell<Option<NaiveDate>>> = Rc::new(RefCell::new(None));
            let cb = captured.clone();
            let mut dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 30).unwrap())
                .on_change(move |d| *cb.borrow_mut() = Some(d));

            // Crosses June → July, updating both the selection and the visible
            // month, and emitting the new date.
            dp.move_selection(1);
            assert_eq!(dp.selected(), NaiveDate::from_ymd_opt(2024, 7, 1).unwrap());
            assert_eq!(dp.month_view(), (2024, 7));
            assert_eq!(
                *captured.borrow(),
                Some(NaiveDate::from_ymd_opt(2024, 7, 1).unwrap())
            );

            dp.move_selection(7);
            assert_eq!(dp.selected(), NaiveDate::from_ymd_opt(2024, 7, 8).unwrap());
        }

        fn date_picker_first_day_of_week_is_monday() {
            let dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
            let first_col: usize = dp.first_visible_col();
            assert_eq!(first_col, 0_usize);
        }

        /// Header label, chevrons, weekday row and day numbers all centre their
        /// measured line in their band; the font size alone sat each 1.5 px low.
        fn picker_text_centres_its_measured_line_in_every_band() {
            let selected = NaiveDate::from_ymd_opt(2026, 5, 22).expect("valid date");
            let mut picker = DatePicker::new(selected);
            let rect = Rect::new(0.0, 0.0, 224.0, 240.0);
            picker.apply_layout_external(rect);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            picker.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let style = theme.text(TextRole::Caption);
            // Draw order: header label, seven weekday labels, then day cells;
            // the month steps are icons, not glyph runs.
            let header_band = (rect.y0, HEADER_HEIGHT);
            let weekday_band = (rect.y0 + HEADER_HEIGHT, WEEKDAY_HEIGHT);
            let first_cell = picker
                .day_rects
                .first()
                .map(|(_, r)| *r)
                .expect("day cells");
            let expectations: Vec<(&str, (f64, f64))> = std::iter::once(("May 2026", header_band))
                .chain(WEEKDAY_LABELS.iter().map(|label| (*label, weekday_band)))
                .chain(std::iter::once((
                    "27",
                    (first_cell.y0, first_cell.height()),
                )))
                .collect();
            for (index, (text, (band_y0, band_h))) in expectations.into_iter().enumerate() {
                crate::widget::text::assert_glyph_run_centred_in_band(
                    &scene,
                    index,
                    text,
                    &style,
                    band_y0,
                    band_h,
                    &mut tr,
                    theme.text_primary,
                );
            }
        }

        fn focused_date_picker_paints_a_focus_ring() {
            let without = picker_color_fills(false);
            let with = picker_color_fills(true);
            assert_eq!(
                with,
                without + 1,
                "a focused date picker must add exactly one focus-ring stroke ({with} vs {without})",
            );
        }

        date_picker_initial_state_shows_selected_month();
        date_picker_next_month_advances();
        date_picker_clicking_day_updates_selection();
        date_picker_keyboard_nav_moves_selection_and_emits();
        date_picker_first_day_of_week_is_monday();
        picker_text_centres_its_measured_line_in_every_band();
        focused_date_picker_paints_a_focus_ring();
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    #[test]
    fn date_picker_semantics_and_shared_state_track_selection() {
        fn accessibility_reports_combobox_role_and_selected_date_value() {
            use crate::a11y::Role;
            let dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
            let node = Widget::accessibility(&dp).expect("date picker is a selector");
            assert_eq!(node.role, Role::ComboBox);
            assert_eq!(node.value.as_deref(), Some("2024-06-15"));
            assert!(node.state.focusable);
            assert_eq!(node.state.expanded, None, "the grid is rendered inline");
        }

        fn accessibility_value_tracks_new_selection() {
            let mut dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 1).unwrap());
            dp.selected = NaiveDate::from_ymd_opt(2024, 6, 7).unwrap();
            let node = Widget::accessibility(&dp).unwrap();
            assert_eq!(node.value.as_deref(), Some("2024-06-07"));
        }

        fn local_state_keeps_date_pickers_in_sync() {
            let first = NaiveDate::from_ymd_opt(2026, 3, 12).unwrap();
            let second = NaiveDate::from_ymd_opt(2027, 8, 4).unwrap();
            let state = crate::LocalState::new(first);
            let mut left = DatePicker::from_state(&state);
            let mut right = DatePicker::from_state(&state);
            let doc = AutoCommit::new();

            left.set_selected(second);
            assert_eq!(state.with(|date| *date), second);
            Widget::sync(&mut right, &doc, &automerge::ROOT);
            assert_eq!(right.selected(), second);
            assert_eq!(right.month_view(), (2027, 8));
        }

        fn semantic_date_change_updates_state_and_callback() {
            let initial = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
            let expected = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
            let state = crate::LocalState::new(initial);
            let captured = Rc::new(RefCell::new(None));
            let callback = Rc::clone(&captured);
            let mut picker = DatePicker::from_state(&state)
                .on_change(move |date| *callback.borrow_mut() = Some(date));
            let mut doc = AutoCommit::new();

            assert!(Widget::perform_action(
                &mut picker,
                &crate::a11y::Action::SetValue("2026-11-23".into()),
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(state.with(|date| *date), expected);
            assert_eq!(*captured.borrow(), Some(expected));

            assert!(!Widget::perform_action(
                &mut picker,
                &crate::a11y::Action::SetValue("2026-02-30".into()),
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(state.with(|date| *date), expected);
        }

        accessibility_reports_combobox_role_and_selected_date_value();
        accessibility_value_tracks_new_selection();
        local_state_keeps_date_pickers_in_sync();
        semantic_date_change_updates_state_and_callback();
    }

    fn picker_color_fills(focused: bool) -> usize {
        use vello::Scene;
        use vello_encoding::DrawTag;
        let mut dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = dp.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(node, taffy::Size::MAX_CONTENT)
            .unwrap();
        dp.apply_layout(&taffy, node, Point::ZERO);
        dp.set_focus(focused);

        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        dp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }
    #[test]
    fn a_navigator_fills_its_column_and_marks_busy_days() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let picker = DatePicker::new(day)
            .navigator()
            .marked([day, day.succ_opt().unwrap()]);
        assert_eq!(picker.marked.len(), 2);
        let narrow = picker.intrinsic_height(168.0);
        let wide = picker.intrinsic_height(224.0);
        assert!(narrow < wide, "cells follow the column's width");
        let app = crate::headless::FrameHarness::new(
            crate::State::new(),
            Box::new(crate::column![picker.with_accessible_name("Month")]),
            (200, 600),
        );
        let _ = app.targets();
    }
}

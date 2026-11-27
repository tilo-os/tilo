//! `DatePicker` widget — month-grid calendar picker.

use tilo_ui_base::i18n::widget_text;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};

mod feed;
pub use feed::{DateFeed, DateShown};
use jiff::civil::Date;
use std::collections::{HashMap, HashSet};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Shape, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::{widget::binding::BoxedBinding, widget::binding::Pulled};
#[path = "date_picker_component.rs"]
mod component;
use crate::widget::text::centered_text_top;
pub use component::DatePickerProps;
use tilo_ui_base::widget::Widget;

/// Six rows covers every possible month layout — a 31-day month starting
/// on Sunday is the worst case, needing 6 rows to fit the spill-in/out
/// days from neighbouring months.
const GRID_ROWS: usize = 6;
const GRID_COLS: usize = 7;

pub const WEEKDAY_LABELS: [&str; GRID_COLS] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

pub struct DatePicker {
    accessible_name: Option<String>,
    selected: Date,
    month_view: (i16, i8),
    choice: Option<DateChoice>,
    local_state: Option<BoxedBinding<Date>>,
    rect: Rect,
    day_rects: Vec<(Date, Rect)>,
    prev_button_rect: Rect,
    next_button_rect: Rect,
    last_cursor: Point,
    /// The day under the pointer, which lifts.
    hovered: Option<Date>,
    /// Where the chosen day's disc stands: it slides to the day chosen next in the same month.
    disc: tilo_ui_base::motion::MotionValue<Point>,
    /// The day and month `disc` was last placed for.
    disc_for: Option<(Date, (i16, i8))>,
    /// Whether the picker holds keyboard focus. Drives a focus ring around
    /// the whole calendar so the control reads as focused.
    is_focused: bool,
    navigator: bool,
    marked: HashSet<Date>,
    today: Option<Date>,
    feed: Option<DateFeed>,
    feed_seen: Option<u64>,
    /// What the picker last painted, and what the last presented frame
    /// showed: it repaints only when the month it shows changed.
    painted: Option<Face>,
    presented: Option<Face>,
}

#[derive(Clone, Debug, PartialEq)]
struct Face {
    rect: Rect,
    selected: Date,
    month_view: (i16, i8),
    focused: bool,
    hovered: Option<Date>,
    today: Date,
    marked: HashSet<Date>,
}

impl DatePicker {
    pub fn new(initial: Date) -> Self {
        Self {
            accessible_name: None,
            selected: initial,
            month_view: (initial.year(), initial.month()),
            choice: None,
            local_state: None,
            rect: Rect::ZERO,
            day_rects: Vec::new(),
            prev_button_rect: Rect::ZERO,
            next_button_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            hovered: None,
            disc: tilo_ui_base::motion::MotionValue::new(Point::ZERO),
            disc_for: None,
            is_focused: false,
            navigator: false,
            marked: HashSet::new(),
            today: None,
            feed: None,
            feed_seen: None,
            painted: None,
            presented: None,
        }
    }

    fn face(&self) -> Face {
        Face {
            rect: self.rect,
            selected: self.selected,
            month_view: self.month_view,
            focused: self.is_focused,
            hovered: self.hovered,
            today: self.today.unwrap_or_else(tilo_ui_base::clock::today),
            marked: self.marked.clone(),
        }
    }

    #[must_use]
    pub fn navigator(mut self) -> Self {
        self.navigator = true;
        self
    }

    #[must_use]
    pub fn marked(mut self, dates: impl IntoIterator<Item = Date>) -> Self {
        self.set_marked(dates);
        self
    }

    #[must_use]
    pub fn with_today(mut self, today: Date) -> Self {
        self.today = Some(today);
        self
    }

    pub fn set_today(&mut self, today: Date) {
        self.today = Some(today);
    }

    #[must_use]
    pub fn has_marked(&self) -> bool {
        !self.marked.is_empty()
    }

    pub fn set_marked(&mut self, dates: impl IntoIterator<Item = Date>) {
        self.marked = dates.into_iter().collect();
    }

    #[must_use]
    pub fn intrinsic_height(&self, width: f64) -> f64 {
        let cell = if self.navigator {
            // The smallest cell a narrow sidebar gets is six spacing steps.
            (width / grid_f64(GRID_COLS))
                .clamp(f64::from(tilo_ui_base::tokens::Spacing::BASE.s6), CELL_SIZE)
        } else {
            CELL_SIZE
        };
        (HEADER_HEIGHT + WEEKDAY_HEIGHT + cell * grid_f64(GRID_ROWS)).ceil()
    }

    pub fn from_state(state: &tilo_ui_base::LocalState<Date>) -> Self {
        let (selected, binding) = tilo_ui_base::widget::binding::local(state);
        Self {
            local_state: Some(binding),
            ..Self::new(selected)
        }
    }

    /// Create a date picker following a day held as its ISO text (`"2026-06-30"`): one set
    /// there is shown, today when the text names no day.
    pub fn from_text_state(state: &tilo_ui_base::LocalState<String>) -> Self {
        let (selected, binding) = tilo_ui_base::widget::binding::derived(state, |text| {
            tilo_ui_base::component::moment_day(text).unwrap_or_else(tilo_ui_base::clock::today)
        });
        Self {
            local_state: Some(binding),
            ..Self::new(selected)
        }
    }

    pub fn selected(&self) -> Date {
        self.selected
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    pub fn month_view(&self) -> (i16, i8) {
        self.month_view
    }

    /// Called with the day chosen, whether a click, a key or a screen reader
    /// chose it; `id` names the picker. What is chosen is the draft's own
    /// value, never an edit.
    #[must_use]
    pub fn on_choose(mut self, id: impl Into<String>, f: impl FnMut(Date) + 'static) -> Self {
        self.choice = Some(DateChoice {
            id: format!("{}/day", id.into()),
            handler: Box::new(f),
        });
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

    pub fn retreat_month(&mut self) {
        let (y, m) = self.month_view;
        self.month_view = if m <= 1 {
            (y.saturating_sub(1), 12)
        } else {
            (y, m.saturating_sub(1))
        };
    }

    pub fn set_selected(&mut self, date: Date) {
        self.selected = date;
        self.month_view = (date.year(), date.month());
        self.publish_local_state();
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.local_state {
            binding.publish(&self.selected);
        }
    }

    fn commit_user_date(&mut self, date: Date) {
        self.selected = date;
        self.month_view = (date.year(), date.month());
        self.publish_local_state();
        if let Some(choice) = &mut self.choice {
            (choice.handler)(date);
        }
    }

    /// The visible month follows the selection so arrowing off the end of
    /// a month reveals the next one, matching every native date grid.
    fn move_selection(&mut self, days: i64) {
        if let Ok(d) = self.selected.checked_add(jiff::Span::new().days(days)) {
            self.commit_user_date(d);
        }
    }

    fn disc_moving(&self) -> bool {
        self.disc.is_moving(tilo_ui_base::motion::frame_now())
    }

    /// Where the chosen day's disc stands this frame: it slides from the day chosen before
    /// when both are on the month shown, and stands on the day at once otherwise (a month
    /// turned, the first look).
    fn follow_disc(&mut self, motion: tilo_ui_base::tokens::EffectiveMotion) -> Option<Point> {
        use tilo_ui_base::{motion::Direction, motion::ListChange};
        let now = tilo_ui_base::motion::frame_now();
        let target = self
            .day_rects
            .iter()
            .find(|(day, _)| *day == self.selected)
            .map(|(_, cell)| cell.center())?;
        let placed = (self.selected, self.month_view);
        let slides = self
            .disc_for
            .is_some_and(|(day, month)| day != self.selected && month == self.month_view);
        if slides {
            let timing = ListChange::MOVE.timing(
                Direction::Enter,
                motion,
                tilo_ui_base::motion::input_source(),
            );
            self.disc.set(target, timing, now);
            if let Some(until) = self.disc.settles_at() {
                tilo_ui_base::motion::want_frames_until(until);
            }
        } else if self.disc.target() != target {
            self.disc.snap(target);
        }
        self.disc_for = Some(placed);
        Some(self.disc.get(now))
    }

    /// Column index (0..=6) of the first-of-month, Monday = 0.
    pub fn first_visible_col(&self) -> usize {
        let (y, m) = self.month_view;
        // Falls back to column 0 on an invalid year range.
        Date::new(y, m, 1).ok().map_or(0, |d| {
            usize::from(d.weekday().to_monday_zero_offset().unsigned_abs())
        })
    }

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

    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.set_rect(rect);
    }

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

    fn cell_date(&self, row: usize, col: usize) -> Option<Date> {
        let (y, m) = self.month_view;
        let first = Date::new(y, m, 1).ok()?;
        // row < GRID_ROWS (6) and col < GRID_COLS (7), so the grid index
        // and first_visible_col() (0..=6) are always tiny; saturating
        // arithmetic and try_from keep the conversion safe without
        // ever panicking or wrapping even outside that invariant.
        let grid_index =
            i64::try_from(row.saturating_mul(GRID_COLS).saturating_add(col)).unwrap_or(i64::MAX);
        let first_col = i64::try_from(self.first_visible_col()).unwrap_or(0);
        let offset = grid_index.saturating_sub(first_col);
        first.checked_add(jiff::Span::new().days(offset)).ok()
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

fn px(v: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "v is built from small fixed layout constants, never approaching f32::MAX"
    )]
    let out = v as f32;
    out
}

pub const HEADER_HEIGHT: f64 = 32.0;
pub const WEEKDAY_HEIGHT: f64 = 20.0;
const CELL_SIZE: f64 = 32.0;
const CHEVRON_WIDTH: f64 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalDatePickerHit {
    PreviousMonth,
    NextMonth,
    Date(Date),
}

impl Widget for DatePicker {
    tilo_ui_base::described!();

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(binding) = &mut self.local_state
            && let Pulled::Value(date) = binding.pull(state, path)
        {
            self.selected = date;
            self.month_view = (date.year(), date.month());
        }
        self.pull_feed();
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.pull_feed()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let width = CELL_SIZE * grid_f64(GRID_COLS);
        if self.navigator {
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
                taffy::AvailableSpace::Definite(spare) => f64::from(spare).min(full),
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
        self.set_rect(tilo_ui_base::widget::layout_rect(taffy, node, origin));
    }

    /// The grid draws its days at a fixed cell size: its height follows the width it is given,
    /// which the solver re-measures whenever that changes. No frame marks it for another
    /// measure, so a window showing a calendar stays still.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        let tokens = &theme.tokens;
        self.painted = Some(self.face());

        if self.navigator {
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
        let outline = tilo_ui_base::surface::edge_outline(self.rect, radius);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tilo_ui_base::surface::edge_on(fill),
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => match logical_key {
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
                // Home and End go to the week's first and last day.
                Key::Named(NamedKey::Home) => {
                    let back = self.selected.weekday().to_monday_zero_offset();
                    self.move_selection(-i64::from(back));
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::End) => {
                    let ahead = 6 - self.selected.weekday().to_monday_zero_offset();
                    self.move_selection(i64::from(ahead));
                    *ctx.handled = true;
                }
                // Page Up and Down turn a month, with Shift a year.
                Key::Named(NamedKey::PageUp) => {
                    let turns = if ctx.modifiers.state().shift_key() {
                        12
                    } else {
                        1
                    };
                    for _ in 0..turns {
                        self.retreat_month();
                    }
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::PageDown) => {
                    let turns = if ctx.modifiers.state().shift_key() {
                        12
                    } else {
                        1
                    };
                    for _ in 0..turns {
                        self.advance_month();
                    }
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::Enter | NamedKey::Space) => {
                    self.commit_user_date(self.selected);
                    *ctx.handled = true;
                }
                _ => {}
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.hovered = self
                    .day_rects
                    .iter()
                    .find(|(_, cell)| day_disc(*cell).contains(self.last_cursor))
                    .map(|(day, _)| *day);
            }
            WindowEvent::CursorLeft { .. } => self.hovered = None,
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
                    self.commit_user_date(date);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        self.presented.as_ref() != Some(&self.face()) || self.disc_moving()
    }

    fn needs_animation_frame(&self) -> bool {
        self.disc_moving()
    }

    /// A picker the frame left out (scrolled out of its view) shows nothing that could be stale:
    /// it rests as it stands, and is drawn whole once it comes back into view.
    fn mark_drawn(&mut self) {
        self.presented = Some(self.painted.take().unwrap_or_else(|| self.face()));
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
    /// left unset since the grid is rendered inline. A picker told what a choice
    /// does ([`Self::on_choose`]) is reached by its id; a fed picker
    /// ([`DateFeed`]) by its name, so its choices still go to the app's feed.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let node = AccessNode::new(Role::ComboBox)
            .with_optional_name(self.accessible_name.as_ref().map(|name| widget_text(name)))
            .with_value(self.selected.to_string())
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused()),
            );
        Some(if let Some(choice) = &self.choice {
            node.with_action_target(choice.id.clone())
        } else if self.feed.is_some() {
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
        action: &tilo_ui_base::a11y::Action,
        _key_path: &[u64],
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        use tilo_ui_base::a11y::Action;
        match action {
            Action::SetValue(value) => {
                let Ok(date) = Date::strptime("%Y-%m-%d", value.trim()) else {
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

struct DateChoice {
    id: String,
    handler: Box<dyn FnMut(Date)>,
}

impl DatePicker {
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
        let header_label = tilo_ui_base::format::month_heading(
            jiff::civil::Date::new(y, m, 1).unwrap_or(self.selected),
        );
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

        let edge = f64::from(tokens.metrics.icon_sm);
        for (button, icon) in [
            (
                self.prev_button_rect,
                tilo_ui_base::icons::Icon::ChevronLeft,
            ),
            (
                self.next_button_rect,
                tilo_ui_base::icons::Icon::ChevronRight,
            ),
        ] {
            let centre =
                vello::kurbo::Point::new(button.center().x, self.rect.y0 + HEADER_HEIGHT / 2.0);
            tilo_ui_base::icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(centre, (edge, edge)),
                icon,
                text_color,
                tilo_ui_base::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }

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
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let text_style = tokens.typography.style(TextRole::Caption);
        let text_color = tokens.colors.text_primary;
        let dim_color = tokens.colors.text_dim;

        let today = self.today.unwrap_or_else(tilo_ui_base::clock::today);
        let (vy, vm) = self.month_view;
        let chosen_at = self.follow_disc(theme.motion());

        // The chosen day's disc first, where it stands this frame, under every day's number.
        if let (Some(at), Some((_, cell))) = (
            chosen_at,
            self.day_rects.iter().find(|(day, _)| *day == self.selected),
        ) {
            let disc = day_disc(*cell);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.intent.accent,
                None,
                &vello::kurbo::Circle::new(at, disc.radius),
            );
        }
        for (date, cell_rect) in &self.day_rects {
            let is_in_view_month = date.year() == vy && date.month() == vm;
            let disc = day_disc(*cell_rect);
            // A day's number reads on the disc once the disc has reached it.
            let is_selected = *date == self.selected
                && chosen_at.is_none_or(|at| at.distance(disc.center) < disc.radius / 2.0);
            let is_today = *date == today && is_in_view_month;

            if self.hovered == Some(*date) && !is_selected {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.lift_on(tokens.colors.surfaces.raised, 0.5),
                    None,
                    &disc,
                );
            }

            let day_str = date.day().to_string();
            // Today is told by its number in your colour, never by a ring that reads as focus.
            let color = if is_selected {
                tokens.colors.intent.on_accent
            } else if is_today {
                tokens.colors.intent.accent_ink
            } else if is_in_view_month {
                text_color
            } else {
                dim_color
            };
            let (tw, th) = text_renderer.measure_style(&day_str, None, &text_style);
            let tx = cell_rect.center().x - f64::from(tw) / 2.0;
            let ty = centered_text_top(cell_rect.y0, cell_rect.height(), f64::from(th));
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

fn day_disc(cell: Rect) -> vello::kurbo::Circle {
    let side = cell.height().min(cell.width());
    vello::kurbo::Circle::new(cell.center(), side * 0.4)
}

/// What a date picker's tests read of it from outside the module.
#[cfg(feature = "test-support")]
impl crate::widget::DatePicker {
    /// Each day the picker laid out, with its box.
    #[must_use]
    pub fn test_day_rects(&self) -> &[(Date, Rect)] {
        &self.day_rects
    }

    /// The days the picker marks.
    #[must_use]
    pub fn test_marked(&self) -> &std::collections::HashSet<Date> {
        &self.marked
    }
}

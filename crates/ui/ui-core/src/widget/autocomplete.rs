//! `Autocomplete` — a text field with a suggestion list that floats below it — and the [`Suggestion`]
//! rows that list shows.

use std::collections::HashMap;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::search::system::filters::{draw_filter_chip, share_row};
use crate::widget::floating_list::{FloatingList, FloatingMetrics, FloatingRow, RowState};
use tilo_ui_base::chrome::SelectionState;
use tilo_ui_base::num::f32_of_f64;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::{icons::Icon, icons::LUCIDE_DEFAULT_STROKE_PX, icons::draw_icon};
#[path = "autocomplete_component.rs"]
mod component;
use crate::{widget::TextInput, widget::TextInputKind};
pub use component::{AutocompleteProps, SuggestionProps};
use tilo_ui_base::widget::Widget;

pub type SuggestionList = FloatingList<Suggestion>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutocompleteOutcome {
    None,
    Accepted(String),
    Cancelled,
}

pub type Suggester = Box<dyn Fn(&str) -> Vec<Suggestion>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub value: String,
    pub label: String,
    pub detail: Option<String>,
    pub subtitle: Option<String>,
    pub icon: Option<Icon>,
    pub person: Option<tilo_ui_base::component::Person>,
    /// What the row is: a pickable item (the default), a group's header or a
    /// notice ("No matches").
    pub role: RowRole,
    /// Byte ranges of the label the typed query matched, painted bold.
    pub matched: Vec<std::ops::Range<usize>>,
    pub aside: Option<String>,
    pub checked: Option<bool>,
    pub shortcut: Option<String>,
    /// Its value can be stepped in place (a level, a volume): Left and Right on it report the
    /// step through the field's `adjust`.
    pub adjustable: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowRole {
    #[default]
    Item,
    Header {
        busy: bool,
    },
    /// A line that says something about the list ("No matches"), never
    /// armed.
    Notice,
}

impl Suggestion {
    #[must_use]
    pub fn is_pickable(&self) -> bool {
        self.role == RowRole::Item
    }
}

impl Suggestion {
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
            adjustable: false,
        }
    }

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
            adjustable: false,
        }
    }

    /// The heading of a group of suggestions: a small, dim title over the
    /// rows under it, which is never armed or chosen.
    pub fn heading(title: impl Into<String>) -> Self {
        Self {
            role: RowRole::Header { busy: false },
            ..Self::new(title)
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

    #[must_use]
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    #[must_use]
    pub fn with_person(mut self, person: tilo_ui_base::component::Person) -> Self {
        self.person = Some(person);
        self
    }

    #[must_use]
    pub fn with_checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    #[must_use]
    pub fn with_shortcut(mut self, keys: impl Into<String>) -> Self {
        self.shortcut = Some(keys.into());
        self
    }
}

/// The width a row's trailing detail may take out of `free`: what the label
/// (`label_w` wide) leaves, and at least half the row.
fn detail_width(free: f64, label_w: f64, spacing: f64) -> f64 {
    (free - label_w - 2.0 * spacing).max(free / 2.0)
}

/// What every row of a suggestion list shares: whether any row shows a
/// state, so they all keep the check column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckColumn {
    pub check: bool,
}

impl FloatingRow for Suggestion {
    type Columns = CheckColumn;

    fn height(&self, metrics: &FloatingMetrics) -> f64 {
        metrics.row
    }

    fn pickable(&self) -> bool {
        self.is_pickable()
    }

    fn same(&self, other: &Self) -> bool {
        self == other
    }

    fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        state: RowState<'_, CheckColumn>,
    ) -> Option<f64> {
        paint_suggestion(ctx, self, row, state)
    }

    fn two_line(&self) -> bool {
        self.subtitle.is_some()
    }

    fn busy(&self) -> bool {
        matches!(self.role, RowRole::Header { busy: true })
    }

    fn content_width(
        &self,
        m: &FloatingMetrics,
        role: TextRole,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let label_style = theme.text(role);
        let detail_style = theme.text(TextRole::Caption);
        let mut w = f64::from(text.measure_style(&self.label, None, &label_style).0);
        if self.checked.is_some() {
            w += m.check + m.spacing;
        }
        if let Some(aside) = &self.aside {
            w += m.spacing + f64::from(text.measure_style(aside, None, &detail_style).0);
        }
        if let Some(subtitle) = &self.subtitle {
            w = w.max(f64::from(
                text.measure_style(subtitle, None, &detail_style).0,
            ));
        }
        if self.person.is_some() {
            w += m.avatar() + m.spacing;
        } else if self.icon.is_some() {
            w += m.icon + m.spacing;
        }
        if let Some(detail) = &self.detail {
            w += 2.0 * m.spacing + f64::from(text.measure_style(detail, None, &detail_style).0);
        }
        if let Some(keys) = &self.shortcut {
            w += m.spacing + f64::from(text.measure_style(keys, None, &label_style).0);
        }
        w
    }

    fn columns(rows: &[Self]) -> CheckColumn {
        CheckColumn {
            check: rows.iter().any(|s| s.checked.is_some()),
        }
    }
}

fn paint_suggestion(
    ctx: &mut PaintCtx<'_>,
    s: &Suggestion,
    row: Rect,
    state: RowState<'_, CheckColumn>,
) -> Option<f64> {
    match s.role {
        RowRole::Item => {
            paint_item(ctx, s, row, &state);
            None
        }
        RowRole::Header { .. } => Some(paint_header(ctx, s, row, &state)),
        RowRole::Notice => {
            paint_notice(ctx, s, row, &state);
            None
        }
    }
}

fn paint_item(
    ctx: &mut PaintCtx<'_>,
    s: &Suggestion,
    row: Rect,
    state: &RowState<'_, CheckColumn>,
) {
    let PaintCtx {
        scene,
        transform,
        text,
        theme,
    } = ctx.reborrow();
    let m = &state.metrics;
    let armed = state.armed;
    let ink = if armed {
        crate::widget::floating_surface::paint_armed_row_in(
            scene,
            transform,
            row,
            state.radius,
            m.inset,
            theme,
        )
    } else {
        if state.hovered {
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
        &[tilo_ui_base::chrome::panel_ground(theme)],
        theme.text_dim,
        tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST,
    );
    let label_style = theme.text(state.role);
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
        tilo_ui_base::widget::AvatarFace::of(person)
            .paint(&mut PaintCtx::new(scene, transform, text, theme), mark);
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
    // The trailing end: the check column, then the keys that do the same,
    // quieter than the label unless the row is armed.
    let mut end = row.x1 - m.pad;
    if state.columns.check {
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
        let spare = f32_of_f64(detail_width(free, label_w, m.spacing));
        let shown = text.elide_to_width_style(detail, spare, &detail_style);
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
    let spare = f32_of_f64((label_x1 - x).max(0.0));
    let label = text.elide_to_width_style(&s.label, spare, &label_style);
    let (lw, lh) = text.measure_style(&label, None, &label_style);
    let subtitle = s.subtitle.as_ref().map(|subtitle| {
        let shown = text.elide_to_width_style(subtitle, spare, &detail_style);
        let (_, sh) = text.measure_style(&shown, None, &detail_style);
        (shown, f64::from(sh))
    });
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
            .with_weight(tilo_ui_base::text_render::TextWeight::Bold);
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
        let spare = f32_of_f64((label_x1 - ax).max(0.0));
        if spare > 8.0 {
            let shown = text.elide_to_width_style(aside, spare, &detail_style);
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

fn paint_header(
    ctx: &mut PaintCtx<'_>,
    s: &Suggestion,
    row: Rect,
    state: &RowState<'_, CheckColumn>,
) -> f64 {
    let PaintCtx {
        scene,
        transform,
        text,
        theme,
    } = ctx.reborrow();
    let m = &state.metrics;
    let style = theme.text(TextRole::Caption);
    let dim = theme.tokens.colors.ink_on_any(
        &[tilo_ui_base::chrome::panel_ground(theme)],
        theme.text_dim,
        tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST,
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
fn paint_notice(
    ctx: &mut PaintCtx<'_>,
    s: &Suggestion,
    row: Rect,
    state: &RowState<'_, CheckColumn>,
) {
    let PaintCtx {
        scene,
        transform,
        text,
        theme,
    } = ctx.reborrow();
    let m = &state.metrics;
    let style = theme.text(state.role);
    let dim = theme.tokens.colors.ink_on_any(
        &[tilo_ui_base::chrome::panel_ground(theme)],
        theme.text_dim,
        tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST,
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

impl FloatingList<Suggestion> {
    /// The open list as assistive technology reads it: its panel and each row, keyed by `key`.
    pub fn access_rows(
        &self,
        key: impl Fn(&Suggestion) -> u64,
    ) -> Vec<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Politeness, a11y::Role};
        let Some(panel) = self.rect() else {
            return Vec::new();
        };
        if self.rows().is_empty() {
            let busy = self.is_busy();
            let arriving = self.arriving().map(|clock| {
                crate::widget::ArrivalFace::new(
                    tilo_ui_base::widget::SkeletonShape::Text { lines: 1 },
                    clock,
                    crate::time::now(),
                )
                .description()
            });
            return arriving
                .or_else(|| self.status().map(str::to_owned))
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
                let s = self.rows().get(index)?;
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
                            .selected(self.highlight() == Some(index)),
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
    input: TextInput,
    suggester: Option<Suggester>,
    /// The suggestions the view offered: what typing narrows when no [`Suggester`] computes
    /// them, so a fixed list obeys the same search a computed one does.
    offered: Vec<Suggestion>,
    list: SuggestionList,
    open: bool,
    /// Tab and Shift+Tab move between the list's groups instead of completing the field.
    tab_groups: bool,
    /// The rows stand open whenever there are any, instead of opening as the person types or walks
    /// them.
    open_list: bool,
    /// The rows narrow to what is typed; off, they are offered as the host computed them.
    filter: bool,
    /// The kinds of result the rows can be narrowed to, drawn as chips over the rows.
    kinds: Vec<String>,
    /// The kinds whose chips are chosen.
    chosen: Vec<String>,
    /// The chip rectangles, as last drawn.
    chip_rects: Vec<Rect>,
    rect: Rect,
    last_pointer: Point,
    outcome: AutocompleteOutcome,
    edited: bool,
    /// A row or the typed text was taken since the host last heard: an edit alone is not.
    taken: bool,
    /// The modifier a row was taken with, and the row: fired as [`Autocomplete::on_alternate`].
    alternate_pending: Option<String>,
    edits: Option<tilo_ui_base::component::Bound<String>>,
    accepted: Option<tilo_ui_base::component::Event>,
    /// The text as it is edited, fired as [`Autocomplete::on_change`].
    changed: Option<tilo_ui_base::component::Event>,
    alternate: Option<tilo_ui_base::component::Event>,
    filter_kind: Option<tilo_ui_base::component::Event>,
    adjust_event: Option<tilo_ui_base::component::Event>,
}

impl Autocomplete {
    pub fn new() -> Self {
        Self::with_input(TextInput::local())
    }

    pub fn with_input(input: TextInput) -> Self {
        Self {
            id: String::new(),
            input: input.with_kind(TextInputKind::Search).with_clear_button(),
            suggester: None,
            offered: Vec::new(),
            list: SuggestionList::new(),
            open: false,
            tab_groups: false,
            open_list: false,
            filter: true,
            kinds: Vec::new(),
            chosen: Vec::new(),
            chip_rects: Vec::new(),
            rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            outcome: AutocompleteOutcome::None,
            edited: false,
            taken: false,
            alternate_pending: None,
            edits: None,
            accepted: None,
            changed: None,
            alternate: None,
            filter_kind: None,
            adjust_event: None,
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

    /// Rows beyond this scroll rather than growing the panel without bound.
    #[must_use]
    pub fn with_max_visible(mut self, n: usize) -> Self {
        self.list = self.list.with_max_visible(n);
        self
    }

    /// For hosts that compute suggestions reactively rather than through a
    /// [`Suggester`]. Re-clamps the highlight and clears any status line.
    pub fn set_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        self.offered = suggestions;
        self.refilter();
    }

    /// For reactive/async hosts filling suggestions or a status line that
    /// arrived *after* the keystroke that requested them (which otherwise
    /// leaves the dropdown closed).
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn set_status(&mut self, status: Option<String>) {
        self.list.set_status(status);
    }

    /// Tab and Shift+Tab move the highlight to the next group's first row, instead of completing
    /// the field to the highlighted row.
    pub fn set_tab_groups(&mut self, tab_groups: bool) {
        self.tab_groups = tab_groups;
    }

    /// The rows stand open whenever there are any: a palette whose empty field already lists what
    /// it offers.
    pub fn set_open_list(&mut self, open_list: bool) {
        self.open_list = open_list;
        self.open_if_listed();
    }

    /// Open the list while it should stand open and has rows to show, with its first row armed so
    /// Enter opens what a person sees chosen.
    pub fn open_if_listed(&mut self) {
        if !self.open_list || self.list.rows().is_empty() {
            return;
        }
        self.open = true;
        let armed = self
            .list
            .highlight()
            .and_then(|at| self.list.rows().get(at))
            .is_some_and(Suggestion::is_pickable);
        if !armed {
            let first = self.list.rows().iter().position(Suggestion::is_pickable);
            self.list.set_highlight(first);
        }
    }

    /// The rows narrow to what is typed: off when the host already computed them for the typed
    /// text, whose ranking is fuzzier than a substring.
    pub fn set_filter(&mut self, filter: bool) {
        if self.filter != filter {
            self.filter = filter;
            self.refilter();
        }
    }

    /// The kinds of result the rows can be narrowed to, drawn as chips over them, and the chips
    /// that are chosen.
    pub fn set_kinds(&mut self, kinds: Vec<String>, chosen: Vec<String>) {
        if self.kinds != kinds || self.chosen != chosen {
            self.kinds = kinds;
            self.chosen = chosen;
            self.chip_rects.clear();
        }
    }

    /// The event a row taken with Shift, Ctrl or Alt fires: the way it was opened and the row.
    pub fn on_alternate(&mut self, event: tilo_ui_base::component::Event) {
        self.alternate = (!event.is_none()).then_some(event);
    }

    /// The event a kind chip fires when it is pressed.
    pub fn on_filter(&mut self, event: tilo_ui_base::component::Event) {
        self.filter_kind = (!event.is_none()).then_some(event);
    }

    /// The event a step of the selected row's value fires: the step and the row's value.
    pub fn on_adjust(&mut self, event: tilo_ui_base::component::Event) {
        self.adjust_event = (!event.is_none()).then_some(event);
    }

    /// Ask the window to give the field the keyboard: a palette opening to be typed into.
    pub fn request_focus(&mut self) {
        self.input.request_focus();
    }

    pub fn set_arriving(&mut self, clock: Option<tilo_ui_base::arriving::ArrivalClock>) {
        self.list.set_arriving(clock);
    }

    pub fn set_suggestions_open(&mut self, suggestions: Vec<Suggestion>) {
        self.set_suggestions(suggestions);
        if self.is_editing() {
            self.open = !self.list.rows().is_empty();
        }
    }

    pub fn text(&self) -> &str {
        self.input.value()
    }

    pub fn input(&self) -> &TextInput {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub fn take_edited(&mut self) -> Option<String> {
        let typed = self.input.take_dirty().is_some();
        (std::mem::take(&mut self.edited) || typed).then(|| self.input.value().to_owned())
    }

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
        self.open = !self.list.rows().is_empty();
        self.list.set_highlight(None);
    }

    pub fn is_editing(&self) -> bool {
        self.input.is_focused()
    }

    pub fn is_open(&self) -> bool {
        self.open && !self.list.rows().is_empty()
    }

    pub fn suggestions(&self) -> &[Suggestion] {
        self.list.rows()
    }

    pub fn highlighted(&self) -> Option<&Suggestion> {
        self.list.highlighted()
    }

    /// Clamped to range; `None` clears. Reactive hosts use this to
    /// highlight the top match by default.
    pub fn set_highlight(&mut self, index: Option<usize>) {
        self.list.set_highlight(index);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.list.set_highlight(None);
        self.list.set_status(None);
        self.list.set_arriving(None);
    }

    /// Report what the person does to the field to the component's binding `edits` (each text
    /// it holds after an edit) and event `accept` (a suggestion or the typed text taken), as a
    /// described component does. The view's binding path keeps the text; this only says.
    #[must_use]
    pub fn on_edit(
        mut self,
        edits: tilo_ui_base::component::Bound<String>,
        accept: tilo_ui_base::component::Event,
    ) -> Self {
        self.edits = Some(edits);
        self.accepted = Some(accept);
        self
    }

    /// Report each edit of the field's text to `event`, carrying what the field holds after it: a
    /// host that reads the query as the person types ([`Self::on_edit`] also says when a choice is
    /// taken). An edit alone is no choice.
    pub fn on_change(&mut self, event: tilo_ui_base::component::Event) {
        self.changed = (!event.is_none()).then_some(event);
    }

    pub fn take_outcome(&mut self) -> AutocompleteOutcome {
        std::mem::replace(&mut self.outcome, AutocompleteOutcome::None)
    }

    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.input.apply_layout_external(rect);
        self.list.set_anchor(rect);
    }

    pub fn list_rect(&self) -> Option<Rect> {
        self.dropdown_rect()
    }

    pub fn list(&self) -> &SuggestionList {
        &self.list
    }

    fn recompute(&mut self) {
        self.refilter();
    }

    /// The rows the list offers for what is typed: what the [`Suggester`] computes, or the view's
    /// own list narrowed to the words the field holds.
    fn refilter(&mut self) {
        if let Some(f) = &self.suggester {
            let suggestions = f(self.input.value());
            self.list.set_rows(suggestions);
            return;
        }
        let query = self.input.value().trim().to_lowercase();
        let rows = if query.is_empty() || !self.filter {
            self.offered.clone()
        } else {
            // A group's heading stays while at least one row under it matches, and goes with its
            // last row: a heading over nothing reads as a group the person can open, and cannot.
            let mut rows: Vec<Suggestion> = Vec::new();
            let mut heading: Option<Suggestion> = None;
            for suggestion in &self.offered {
                match suggestion.role {
                    RowRole::Header { .. } => heading = Some(suggestion.clone()),
                    RowRole::Item => {
                        if suggestion.label.to_lowercase().contains(&query)
                            || suggestion.value.to_lowercase().contains(&query)
                        {
                            if let Some(heading) = heading.take() {
                                rows.push(heading);
                            }
                            rows.push(suggestion.clone());
                        }
                    }
                    RowRole::Notice => {
                        heading = None;
                        rows.push(suggestion.clone());
                    }
                }
            }
            rows
        };
        self.list.set_rows(rows);
    }

    fn accept(&mut self, value: String) {
        self.input.set_text(&value);
        self.edited = true;
        self.taken = true;
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
        let idx = self.list.highlight().or(if self.list.rows().is_empty() {
            None
        } else {
            Some(0)
        });
        if let Some(i) = idx
            && let Some(s) = self.list.rows().get(i)
        {
            let value = s.value.clone();
            self.input.set_text(&value);
            self.edited = true;
            self.recompute();
            self.open = !self.list.rows().is_empty();
            self.list.set_highlight(None);
        }
    }

    fn move_highlight(&mut self, forward: bool) {
        if self.list.rows().is_empty() {
            return;
        }
        self.open = true;
        self.list.move_highlight(forward);
    }

    /// Put the highlight on the next (or previous) group's first row, wrapping round: what Tab and
    /// Shift+Tab do in a palette whose rows stand under group headings.
    fn step_group(&mut self, forward: bool) {
        let rows = self.list.rows();
        let starts: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(at, row)| {
                row.is_pickable()
                    && (*at == 0 || matches!(rows[at - 1].role, RowRole::Header { .. }))
            })
            .map(|(at, _)| at)
            .collect();
        if starts.is_empty() {
            return;
        }
        let current = self.list.highlight();
        let next = match current {
            Some(current) if forward => starts
                .iter()
                .copied()
                .find(|&start| start > current)
                .unwrap_or(starts[0]),
            Some(current) => starts
                .iter()
                .copied()
                .rev()
                .find(|&start| start < current)
                .unwrap_or_else(|| starts[starts.len() - 1]),
            None => starts[0],
        };
        self.open = true;
        self.list.set_highlight(Some(next));
    }

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
    /// The field's own band, at the top of the widget.
    fn field_band(&self, theme: &Theme) -> f64 {
        self.input
            .band_in(theme)
            .max(f64::from(self.input.interaction().minimum_target()))
    }

    /// How far the kind chips take under the field, none when there are no chips.
    fn strip_band(&self, theme: &Theme) -> f64 {
        if self.kinds.is_empty() {
            0.0
        } else {
            f64::from(theme.tokens.spacing.s2) + f64::from(theme.tokens.metrics.selector_height)
        }
    }

    /// Where the kind chips stand: one row under the field, across the widget.
    fn chip_band(&self, theme: &Theme) -> Rect {
        let top = self.rect.y0 + self.field_band(theme) + f64::from(theme.tokens.spacing.s2);
        Rect::new(
            self.rect.x0,
            top,
            self.rect.x1,
            top + f64::from(theme.tokens.metrics.selector_height),
        )
    }

    /// The chip under `pt`, by its place.
    fn chip_at(&self, pt: Point) -> Option<usize> {
        self.chip_rects.iter().position(|rect| rect.contains(pt))
    }

    /// The `KeyboardInput` arm of [`Widget::handle_event`], split out to
    /// keep that under the line cap. Navigation/accept/cancel keys are
    /// intercepted here since the field itself never consumes them;
    /// everything else forwards to the field and recomputes on change.
    fn handle_key_while_editing(
        &mut self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
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
                let highlighted = self.highlighted().map(|s| s.value.clone());
                let value = highlighted
                    .clone()
                    .unwrap_or_else(|| self.input.value().to_string());
                if highlighted.is_some() {
                    let held = ctx.modifiers.state();
                    let way = if held.shift_key() {
                        Some("here")
                    } else if held.control_key() {
                        Some("beside")
                    } else if held.alt_key() {
                        Some("more")
                    } else {
                        None
                    };
                    if let Some(way) = way
                        && self.alternate.is_some()
                    {
                        self.alternate_pending = Some(format!("{way}\t{value}"));
                    }
                    *ctx.handled = true;
                }
                self.accept(value);
            }
            Key::Named(NamedKey::Tab) if !self.list.rows().is_empty() => {
                if self.tab_groups {
                    self.step_group(!ctx.modifiers.state().shift_key());
                } else {
                    self.complete_to_highlight();
                }
                *ctx.handled = true;
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight)
                if self.open
                    && self
                        .highlighted()
                        .is_some_and(|suggestion| suggestion.adjustable) =>
            {
                let delta = if matches!(logical_key, Key::Named(NamedKey::ArrowRight)) {
                    1
                } else {
                    -1
                };
                if let (Some(row), Some(event)) = (self.highlighted(), &self.adjust_event) {
                    event.fire(Some(tilo_ui_base::component::Value::Text(format!(
                        "{delta}\t{}",
                        row.value
                    ))));
                }
                *ctx.handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                self.cancel();
                *ctx.handled = true;
            }
            _ => {
                self.input.handle_event(event, ctx);
                if self.input.take_dirty().is_some() {
                    self.edited = true;
                    self.recompute();
                    self.open = !self.list.rows().is_empty();
                    self.list.set_highlight(None);
                }
            }
        }
    }
}

impl Widget for Autocomplete {
    tilo_ui_base::provides!(dyn tilo_ui_base::widget::controls::ActionControls);

    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.input.set_interaction_environment(environment);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.input.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let inner = tilo_ui_base::widget::Widget::update(&mut self.input, state, path);
        if self.open {
            self.list.advance();
        }
        if let Some(text) = self.take_edited() {
            if let Some(edits) = &self.edits {
                edits.edited(tilo_ui_base::component::Value::Text(text.clone()));
            }
            // A letter typed is an edit, which the host hears as the text and as the edit it made;
            // only a row or the text taken is a choice.
            if let Some(changed) = &self.changed {
                changed.fire(Some(tilo_ui_base::component::Value::Text(text.clone())));
            }
            if std::mem::take(&mut self.taken) {
                if let Some(pending) = self.alternate_pending.take() {
                    if let Some(alternate) = &self.alternate {
                        alternate.fire(Some(tilo_ui_base::component::Value::Text(pending)));
                    }
                } else if let Some(accepted) = &self.accepted {
                    accepted.fire(Some(tilo_ui_base::component::Value::Text(text)));
                }
            }
        }
        inner
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = f32_of_f64(self.field_band(theme) + self.strip_band(theme));
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
        self.apply_layout_external(tilo_ui_base::widget::layout_rect(taffy, node, origin));
    }

    /// A field's band tall across the width it is given: placed once, it asks for no layout.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        let (scene, transform, text_renderer, theme, env) = ctx.parts();
        if let Some(viewport) = env.viewport {
            self.list.set_viewport(viewport);
        }
        let field = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.field_band(theme),
        );
        self.input.apply_layout_external(field);
        self.input
            .draw_child(scene, transform, text_renderer, theme, &env);
        if self.kinds.is_empty() {
            self.chip_rects.clear();
            return;
        }
        let band = self.chip_band(theme);
        let style = theme.text(TextRole::Label);
        let pad = f64::from(theme.tokens.spacing.s3);
        let gap = f64::from(theme.tokens.spacing.s2);
        let natural: Vec<f64> = self
            .kinds
            .iter()
            .map(|kind| f64::from(text_renderer.measure_style(kind, None, &style).0) + 2.0 * pad)
            .collect();
        let widths = share_row(&natural, band.width(), gap);
        let mut rects = Vec::with_capacity(self.kinds.len());
        let mut x = band.x0;
        for (kind, width) in self.kinds.iter().zip(widths) {
            let rect = Rect::new(x, band.y0, x + width, band.y1);
            let chosen = self.chosen.iter().any(|chosen| chosen == kind);
            let hover = rect.contains(self.last_pointer);
            let label = text_renderer.elide_to_width_style(
                kind,
                f32_of_f64((width - pad).max(1.0)),
                &style,
            );
            draw_filter_chip(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                &label,
                &style,
                SelectionState::new(chosen, hover, false),
            );
            rects.push(rect);
            x += width + gap;
        }
        self.chip_rects = rects;
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if !self.open {
            return;
        }
        if let Some(viewport) = env.viewport {
            self.list.set_viewport(viewport);
        }
        self.list.set_anchor(self.rect);
        self.list.paint(scene, transform, text_renderer, theme);
    }

    fn needs_animation_frame(&self) -> bool {
        self.open && self.list.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.open
            .then(|| self.list.next_arriving_step(crate::time::now()))
            .flatten()
    }

    fn needs_redraw(&self) -> bool {
        self.open && self.list.arriving_moved()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
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
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.chip_at(self.last_pointer).is_some() => {
                if let Some(i) = self.chip_at(self.last_pointer)
                    && let Some(kind) = self.kinds.get(i).cloned()
                    && let Some(event) = &self.filter_kind
                {
                    event.fire(Some(tilo_ui_base::component::Value::Text(kind)));
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.row_at(self.last_pointer).is_some() => {
                if let Some(i) = self.row_at(self.last_pointer)
                    && let Some(s) = self.list.rows().get(i)
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
                    self.open = !self.list.rows().is_empty();
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

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.chip_at(point).is_some() {
            Some(winit::window::CursorIcon::Pointer)
        } else if self.rect.contains(point) {
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

    /// The field's own request for the keyboard, which a palette that opens to be typed into
    /// makes: the request is the list's, and the node the app focuses is the list itself, not the
    /// field inside it.
    fn take_focus_request(&mut self) -> Option<String> {
        self.input
            .take_focus_request()
            .map(|_| self.id.clone())
            .filter(|id| !id.is_empty())
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.input.ime_edit_fingerprint(key)
    }

    fn ime_content(&self) -> Option<tilo_ui_base::ime::InputContent> {
        Widget::ime_content(&self.input)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.input.is_dragging()
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::a11y::hash_key;
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
        if !self.kinds.is_empty() {
            use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
            let mut chips = Vec::with_capacity(self.kinds.len());
            for (i, kind) in self.kinds.iter().enumerate() {
                let chosen = self.chosen.iter().any(|chosen| chosen == kind);
                let rect = self.chip_rects.get(i).copied().unwrap_or(Rect::ZERO);
                chips.push(
                    AccessNode::new(Role::ToggleButton)
                        .with_name(kind.clone())
                        .with_key(hash_key(&("autocomplete-kind", kind.as_str())))
                        .with_bounds(rect)
                        .with_state(AccessState {
                            checked: Some(chosen),
                            selected: chosen,
                            hidden: rect.area() <= 0.0,
                            ..AccessState::default()
                        }),
                );
            }
            node.children.push(
                AccessNode::new(Role::Group)
                    .with_name("Result kinds")
                    .with_children(chips),
            );
        }
        if self.open {
            node.children.extend(self.list.access_rows(|suggestion| {
                hash_key(&("autocomplete-suggestion", suggestion.value.as_str()))
            }));
        }
        Some(node.with_keyed_action_routes(id))
    }

    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        key_path: &[u64],
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use tilo_ui_base::{a11y::Action, a11y::hash_key};
        let own_key = hash_key(&self.test_id().unwrap_or("autocomplete"));
        if let [_, chip_key] = key_path
            && matches!(action, Action::Activate | Action::ToggleCheck)
            && let Some(kind) = self
                .kinds
                .iter()
                .find(|kind| hash_key(&("autocomplete-kind", kind.as_str())) == *chip_key)
            && let Some(event) = &self.filter_kind
        {
            event.fire(Some(tilo_ui_base::component::Value::Text(kind.clone())));
            return true;
        }
        if key_path.is_empty() || key_path == [own_key] {
            let (state, path) = tilo_ui_base::doc::document(state, path);
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
            .rows()
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

impl tilo_ui_base::widget::controls::ActionControls for Autocomplete {
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        self.input.attach_controls(domain, attachment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dispatch;
    use automerge::AutoCommit;
    use vello::Scene;
    use vello::kurbo::Affine;

    fn suggester_paths() -> Autocomplete {
        Autocomplete::new().with_suggester(|q| {
            ["alpha", "album", "beta"]
                .iter()
                .filter(|s| s.starts_with(q))
                .map(|s| Suggestion::new(*s))
                .collect()
        })
    }

    use tilo_ui_base::synthetic_input::char_key;

    use tilo_ui_base::synthetic_input::named_key;

    use tilo_ui_base::synthetic_input::cursor_moved;

    use tilo_ui_base::synthetic_input::mouse_press;

    tilo_test_env::scenario! {
        /// The autocomplete field: its list, its selection and its rows.
        /// Each phase was a test of its own and builds its own fixture.
        fn autocomplete_suggests_accepts_and_keeps_its_input;

        phase suggestions_open_highlight_accept_or_cancel;
        {
            fn accessible_edits_update_suggestions_and_activate_the_selected_value() {
                use tilo_ui_base::{a11y::Action, a11y::hash_key};
                let mut ac = suggester_paths();
                let mut state = AutoCommit::new();
                assert!(ac.perform_action(
                    &Action::SetValue("al".into()),
                    &[],
                    &mut tilo_ui_base::widget::Edits::lend(&mut state),
                    &automerge::ROOT
                ));
                assert_eq!(ac.suggestions().len(), 2);
                let node = ac.accessibility().unwrap();
                assert_eq!(node.children[1].name.as_deref(), Some("album"));
                let key = hash_key(&("autocomplete-suggestion", "album"));
                assert!(ac.perform_action(
                    &Action::Activate,
                    &[key],
                    &mut tilo_ui_base::widget::Edits::lend(&mut state),
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
                    &mut tilo_ui_base::widget::Edits::lend(&mut state),
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

        phase reactive_suggestions_status_line_and_scaled_rows;
        {
            fn status_line_keeps_the_dropdown_open_with_no_rows() {
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
                ac.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    tilo_ui_base::draw::DrawEnv::default(),
                ));
                let panel = ac.dropdown_rect().expect("large-text dropdown");
                let first = ac.row_rect(panel, 0);
                let second = ac.row_rect(panel, 1);
                assert!(
                    first.height() > f64::from(tilo_ui_base::tokens::Metrics::BASE.menu_row_height),
                    "rows grow with the text: {first:?}"
                );
                assert_eq!(first.y1, second.y0, "rows do not overlap");
                assert_eq!(ac.row_at(second.center()), Some(1));
            }

            status_line_keeps_the_dropdown_open_with_no_rows();
            reactive_suggestions_after_open_replace_the_status_line();
            large_text_scales_dropdown_rows_and_hit_geometry_together();
        }

        phase a_short_label_leaves_its_detail_the_rest_of_the_row;
        {
            // "Amsterdam" beside "North Holland, The Netherlands" in a 300 px row:
            // the detail gets what the label leaves, not a fixed half.
            assert!((detail_width(300.0, 80.0, 8.0) - 204.0).abs() < f64::EPSILON);
            assert!((detail_width(300.0, 250.0, 8.0) - 150.0).abs() < f64::EPSILON);
        }

        phase the_suggestion_list_flips_scrolls_and_waits;
        {
            let mut list = SuggestionList::new().with_max_visible(3);
            list.set_rows(
                (0..10)
                    .map(|i| Suggestion::new(format!("item {i}")).with_detail("fn"))
                    .collect(),
            );
            list.set_anchor(Rect::new(10.0, 560.0, 210.0, 592.0));
            list.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            let rect = list.rect().expect("rows show");
            assert!(
                rect.y1 <= 560.0,
                "no area below: the list opens above: {rect:?}"
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
            busy.set_arriving(Some(tilo_ui_base::arriving::ArrivalClock::new(
                asked,
                tilo_ui_base::arriving::Source::Remote,
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
            ac.set_arriving(Some(tilo_ui_base::arriving::ArrivalClock::new(
                asked,
                tilo_ui_base::arriving::Source::Remote,
            )));
            ac.open();
            assert!(ac.list_rect().is_some());
            assert!(
                !Widget::needs_animation_frame(&ac),
                "rows on their way draw no frames between their steps"
            );
            assert_eq!(
                Widget::next_wake_deadline(&ac),
                tilo_ui_base::arriving::ArrivalClock::new(asked, tilo_ui_base::arriving::Source::Remote)
                    .next_step_at(crate::time::now()),
                "the placeholder steps on at its deadline"
            );
        }
    }
}

/// A list the view offers narrows as the field is typed, exactly as a computed one does: the
/// dropdown closes when nothing matches and every row returns when the field is cleared.
#[test]
fn a_view_offered_list_narrows_as_text_is_typed() {
    use crate::testing::dispatch;
    use tilo_ui_base::synthetic_input::char_key;

    let mut ac = Autocomplete::new().with_id("recipient");
    ac.set_suggestions(vec![
        Suggestion::labelled("ada@example.com", "Ada"),
        Suggestion::labelled("bo@example.com", "Bo"),
    ]);
    ac.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 300.0, 34.0));
    ac.begin_edit();
    for ch in "bo".chars() {
        dispatch(&mut ac, &char_key(&ch.to_string()));
    }
    let values: Vec<&str> = ac.suggestions().iter().map(|s| s.value.as_str()).collect();
    assert_eq!(
        values,
        ["bo@example.com"],
        "the view's own list is searched"
    );
    assert!(ac.is_open());
    for ch in "zzz".chars() {
        dispatch(&mut ac, &char_key(&ch.to_string()));
    }
    assert!(ac.suggestions().is_empty(), "nothing matches");
    assert!(!ac.is_open(), "and the dropdown closes");
    ac.set_text("");
    ac.set_suggestions(vec![
        Suggestion::labelled("ada@example.com", "Ada"),
        Suggestion::labelled("bo@example.com", "Bo"),
    ]);
    assert_eq!(ac.suggestions().len(), 2, "an empty field offers them all");
}

/// Typing into the field reports the text as it is edited, so a host that reads the query as the
/// person types hears it; an edit alone is no choice, so `accept` stays silent until a row or the
/// text is taken.
#[test]
fn an_edit_reports_the_text_and_no_choice() {
    use crate::testing::dispatch;
    use tilo_ui_base::synthetic_input::char_key;
    use tilo_ui_base::{component::Bound, component::Event, component::Intent};

    let queue = tilo_ui_base::Intents::default();
    let mut ac = Autocomplete::new().with_id("search").on_edit(
        Bound::default(),
        Event::View(Intent::new(&queue, "search", "accept")),
    );
    ac.on_change(Event::View(Intent::new(&queue, "search", "change")));
    ac.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 300.0, 34.0));
    ac.begin_edit();
    // The host hears the text in its frame, as the driver updates it after each event.
    let doc = automerge::AutoCommit::new();
    for ch in "cal".chars() {
        dispatch(&mut ac, &char_key(&ch.to_string()));
        Widget::update(&mut ac, &doc, &automerge::ROOT);
    }
    let fired: Vec<(String, Option<String>)> = queue
        .take()
        .iter()
        .map(|fired| {
            let text = match fired.value.as_ref() {
                Some(tilo_ui_base::component::Value::Text(text)) => Some(text.clone()),
                _ => None,
            };
            (fired.event.clone(), text)
        })
        .collect();
    let changed: Vec<&(String, Option<String>)> = fired
        .iter()
        .filter(|(event, _)| event == "change")
        .collect();
    assert_eq!(
        changed.len(),
        3,
        "every letter typed is reported as the text: {fired:?}"
    );
    assert_eq!(
        changed.last().and_then(|(_, value)| value.as_deref()),
        Some("cal"),
        "the last report is all that was typed: {fired:?}"
    );
    assert!(
        fired.iter().all(|(event, _)| event != "accept"),
        "a letter typed is no choice: {fired:?}"
    );
}

/// A row taken with Shift, Ctrl or Alt reports the row and the way it opens, which a palette
/// hands to its host as a tab in front, a window beside or the row's own way.
#[test]
fn a_held_modifier_carries_the_row_and_the_way_it_opens() {
    use crate::testing::{Dispatch, dispatch};
    use tilo_ui_base::synthetic_input::named_key;
    use tilo_ui_base::{component::Event, component::Intent};
    use winit::event::Modifiers;
    use winit::keyboard::{ModifiersState, NamedKey};

    let queue = tilo_ui_base::Intents::default();
    let mut ac = Autocomplete::new().with_id("search");
    ac.set_open_list(true);
    ac.on_alternate(Event::View(Intent::new(&queue, "search", "alternate")));
    ac.set_suggestions(vec![Suggestion::new("row:1")]);
    ac.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 300.0, 34.0));
    ac.begin_edit();
    dispatch(&mut ac, &named_key(NamedKey::ArrowDown));
    Dispatch::new()
        .modifiers(Modifiers::from(ModifiersState::SHIFT))
        .send(&mut ac, &named_key(NamedKey::Enter));
    assert_eq!(
        ac.alternate_pending.as_deref(),
        Some("here\trow:1"),
        "the row and the way its host opens it"
    );
    assert_eq!(
        ac.take_outcome(),
        AutocompleteOutcome::Accepted("row:1".into())
    );
}

/// A kind chip is a toggle a person and an assistive technology press: it reports the kind the
/// host narrows the rows to.
#[test]
fn a_kind_chip_reports_the_kind_it_narrows_to() {
    use crate::component_testing::fired;
    use tilo_ui_base::{a11y::Action, a11y::hash_key};
    use tilo_ui_base::{component::Event, component::Intent, component::Value};

    let queue = tilo_ui_base::Intents::default();
    let mut ac = Autocomplete::new().with_id("search");
    ac.set_open_list(true);
    ac.set_kinds(vec!["Apps".into(), "Files".into()], Vec::new());
    ac.on_filter(Event::View(Intent::new(&queue, "search", "filter_kind")));
    ac.set_suggestions(vec![Suggestion::new("row:1")]);
    ac.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 300.0, 34.0));

    let handled = Widget::perform_action(
        &mut ac,
        &Action::Activate,
        &[
            hash_key(&"search"),
            hash_key(&("autocomplete-kind", "Files")),
        ],
        &mut tilo_ui_base::widget::Edits::lend(&mut automerge::AutoCommit::new()),
        &automerge::ROOT,
    );
    assert!(handled, "the chip takes the press");
    assert_eq!(
        queue.take(),
        [fired(
            "search",
            "filter_kind",
            Some(Value::Text("Files".into()))
        )]
    );
}

/// Left and Right on an adjustable row report the step and the row instead of moving the
/// caret, so a level or a volume is stepped where it stands.
#[test]
fn a_step_on_an_adjustable_row_reports_the_step_and_the_row() {
    use crate::component_testing::fired;
    use crate::testing::dispatch;
    use tilo_ui_base::synthetic_input::named_key;
    use tilo_ui_base::{component::Event, component::Intent, component::Value};
    use winit::keyboard::NamedKey;

    let queue = tilo_ui_base::Intents::default();
    let mut ac = Autocomplete::new().with_id("search");
    ac.set_open_list(true);
    let mut volume = Suggestion::new("level:volume");
    volume.adjustable = true;
    ac.set_suggestions(vec![volume]);
    ac.on_adjust(Event::View(Intent::new(&queue, "search", "adjust")));
    ac.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 300.0, 34.0));
    ac.begin_edit();
    dispatch(&mut ac, &named_key(NamedKey::ArrowDown));
    dispatch(&mut ac, &named_key(NamedKey::ArrowRight));
    dispatch(&mut ac, &named_key(NamedKey::ArrowLeft));
    assert_eq!(
        queue.take(),
        [
            fired(
                "search",
                "adjust",
                Some(Value::Text("1\tlevel:volume".into()))
            ),
            fired(
                "search",
                "adjust",
                Some(Value::Text("-1\tlevel:volume".into()))
            ),
        ]
    );
}

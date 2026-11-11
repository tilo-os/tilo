//! `Autocomplete` — a text field with a suggestion list that floats below
//! it — and the [`Suggestion`] rows that list shows.
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

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use crate::num::f32_of_f64;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::DrawChild;
use crate::widget::TaffyTreeExt;
use crate::widget::floating_list::{FloatingList, FloatingMetrics, FloatingRow, RowState};
#[path = "autocomplete_component.rs"]
mod component;
use crate::widget::{TextInput, TextInputKind, Widget};
pub use component::{AutocompleteProps, SuggestionProps};

/// The floating list of suggestions under a field: the one list engine
/// ([`FloatingList`]), its rows [`Suggestion`]s.
pub type SuggestionList = FloatingList<Suggestion>;

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
    /// software acting in the workspace, the agent mark) in place of an icon.
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
    /// Their stable id.
    pub identity: String,
    /// The presence hue the workspace gives them.
    pub hue: crate::tokens::PresenceHue,
    /// Software acting in the workspace rather than a person.
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

/// What every row of a suggestion list shares: whether any row shows a
/// state, so they all keep the check column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckColumn {
    /// A trailing check column is kept on every row.
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

/// Paint one suggestion row: the item, a group's header or a notice.
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

/// One row: the armed lift (or the hover veneer), the icon, the label, its
/// aside and subtitle, and the detail in a trailing column.
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
        &[crate::chrome::panel_ground(theme)],
        theme.text_dim,
        crate::tokens::colors::ON_FILL_MIN_CONTRAST,
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
        let colour = theme.tokens.colors.presence.hue(person.hue);
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

impl FloatingList<Suggestion> {
    /// Accessibility rows for the suggestions in view, keyed by `key`.
    pub(crate) fn access_rows(
        &self,
        key: impl Fn(&Suggestion) -> u64,
    ) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Politeness, Role};
        let Some(panel) = self.rect() else {
            return Vec::new();
        };
        if self.rows().is_empty() {
            let busy = self.is_busy();
            let arriving = self.arriving().map(|clock| {
                crate::widget::ArrivalFace::new(
                    crate::widget::SkeletonShape::Text { lines: 1 },
                    clock,
                    crate::time::now(),
                )
                .caption()
                .unwrap_or_else(|| crate::i18n::I18nManager::lookup_plain("arriving-loading"))
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
    /// What the field's text is reported to, as a described component's binding path does.
    edits: Option<crate::component::Bound<String>>,
    /// What taking a suggestion is reported to, as a described component's event.
    accepted: Option<crate::component::Event>,
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
            edits: None,
            accepted: None,
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
        self.list.set_rows(suggestions);
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
            self.open = !self.list.rows().is_empty();
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

    /// Close the dropdown (keeps the field text + focus).
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
        edits: crate::component::Bound<String>,
        accept: crate::component::Event,
    ) -> Self {
        self.edits = Some(edits);
        self.accepted = Some(accept);
        self
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
            self.list.set_rows(suggestions);
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
            Key::Named(NamedKey::Tab) if !self.list.rows().is_empty() => {
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
                    self.open = !self.list.rows().is_empty();
                    self.list.set_highlight(None);
                }
            }
        }
    }
}

impl Widget for Autocomplete {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.input.set_interaction_environment(environment);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.input.sync(state, path);
    }

    /// A glide after a flick scrolls the list as the wheel does, and what the person typed or
    /// took since the last frame is reported to the component's binding and event, if it has
    /// them: a text set elsewhere (`set_text`) is not a person's edit.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let inner = crate::widget::Widget::update(&mut self.input, state, path);
        if self.open {
            self.list.advance();
        }
        if let Some(text) = self.take_edited() {
            if let Some(edits) = &self.edits {
                edits.edited(crate::component::Value::Text(text.clone()));
            }
            if let Some(accepted) = &self.accepted {
                accepted.fire(Some(crate::component::Value::Text(text)));
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
        // The field's own band: its control size, grown with its line, and a finger's target
        // where input is a finger's.
        let height = f32_of_f64(
            self.input
                .band_in(theme)
                .max(f64::from(self.input.interaction().minimum_target())),
        );
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
            .draw_child(scene, transform, text_renderer, theme, presence);
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

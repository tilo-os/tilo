//! `PropertyList`: labelled values (a contact's details, a file's metadata,
//! a certificate's fields), as a retained widget and as the
//! [`PropertyListFace`] immediate painter with identical geometry.
//!
//! Labels sit in one column, measured from the longest label, in secondary
//! ink; values sit in the column beside it in primary ink (or a status
//! [`Tone`]). Prose wraps, at most a readable measure wide; identifiers,
//! secrets and figures never break and are elided before the action column.
//! When the value column would get too narrow the list stacks: each label
//! above its value. A row may carry up to two trailing actions (copy, open,
//! reveal). The list sits on its ground separated by space, never inside a
//! card; dense inspectors may stripe alternate rows with a derived tint and
//! take the compact density.
//!
//! **One content edge.** Labels start on the column's content edge, like a
//! section's title and a list row's text; a stripe bleeds
//! [`row_bleed`](crate::chrome::row_bleed) past it on both sides. A face
//! paints into the list's outer rect (the content edge grown by the bleed);
//! the retained list grows its own layout box by it. Lists in one
//! [`Section`](crate::widget::Section), and the groups of
//! [`PropertyList::sections`], share one label column.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::chrome::row_bleed;
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextKind, TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::{Colors, SMALL_TEXT_MIN_CONTRAST};
use crate::widget::{TaffyTreeExt, Tone, Widget};

/// The most actions one row carries.
pub const MAX_PROPERTY_ACTIONS: usize = 2;

/// One label and its value, as the face paints it.
#[derive(Clone, Copy, Debug)]
pub struct PropertyRow<'a> {
    pub label: &'a str,
    pub value: &'a str,
    /// What the value is: [`TextKind::Numeric`] for sizes, offsets and
    /// dates, [`TextKind::Identifier`] for addresses and handles,
    /// [`TextKind::Secret`] for keys and hashes. Only prose wraps.
    pub kind: TextKind,
    /// A status the value carries (an expired certificate, a failing check).
    pub tone: Tone,
    /// Up to two trailing actions for this value, in order.
    pub actions: [Option<(Icon, &'a str)>; MAX_PROPERTY_ACTIONS],
}

impl<'a> PropertyRow<'a> {
    #[must_use]
    pub fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            kind: TextKind::Prose,
            tone: Tone::Neutral,
            actions: [None; MAX_PROPERTY_ACTIONS],
        }
    }

    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        self
    }

    /// The value in a status tone, made legible on the list's ground.
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Add a trailing action; a row holds up to two, extra ones are ignored.
    #[must_use]
    pub fn action(mut self, icon: Icon, label: &'a str) -> Self {
        if let Some(slot) = self.actions.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((icon, label));
        }
        self
    }

    fn action_count(&self) -> usize {
        self.actions.iter().flatten().count()
    }

    /// Whether the value keeps to one line, elided rather than broken.
    fn unbreakable(&self) -> bool {
        matches!(
            self.kind,
            TextKind::Identifier | TextKind::Secret | TextKind::Numeric
        )
    }
}

/// How dense a property list is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Density {
    /// Rows a pointer target tall.
    #[default]
    Regular,
    /// Rows a small control tall, for inspectors beside a canvas.
    Compact,
}

/// The resolved geometry of a property list in `theme`.
#[derive(Clone, Debug)]
struct ListMetrics {
    label: TextStyle,
    stacked_label: TextStyle,
    value: TextStyle,
    group: TextStyle,
    pad_x: f64,
    pad_y: f64,
    column_gap: f64,
    stacked_gap: f64,
    min_row: f64,
    max_label_share: f64,
    action: f64,
    action_gap: f64,
    icon: f64,
    radius: f64,
    related: f64,
    sibling: f64,
}

impl ListMetrics {
    fn new(theme: &Theme, density: Density) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let (pad_y, min_row) = match density {
            Density::Regular => (spacing.s1, metrics.min_pointer_target),
            Density::Compact => (spacing.s1 / 2.0, metrics.bar_control_height),
        };
        Self {
            label: theme.text(TextRole::Body),
            stacked_label: theme.text(TextRole::Caption),
            value: theme.text(TextRole::Body),
            group: theme.text(TextRole::Subheading),
            pad_x: row_bleed(theme),
            pad_y: f64::from(pad_y),
            column_gap: f64::from(spacing.s4),
            stacked_gap: 0.0,
            min_row: f64::from(min_row),
            max_label_share: 0.4,
            action: f64::from(metrics.bar_control_height),
            action_gap: f64::from(spacing.s1),
            icon: f64::from(metrics.icon_sm),
            radius: f64::from(theme.tokens.radii.item),
            related: f64::from(spacing.rhythm.related),
            sibling: f64::from(spacing.rhythm.sibling),
        }
    }

    fn value_style(&self, row: &PropertyRow<'_>) -> TextStyle {
        self.value.clone().with_kind(row.kind)
    }

    fn line(&self, text: &mut TextRenderer) -> f64 {
        f64::from(text.measure_style("Ag", None, &self.value).1)
    }
}

/// The widest prose runs, in line boxes of its value text: a readable
/// measure. Identifiers and figures are not prose and use the full column.
const READABLE_LINES: u8 = 32;

/// The value column's minimum width before the list stacks, in line boxes.
const MIN_VALUE_LINES: u8 = 8;

/// The label column `rows` need: the longest label, at most `max_share` of
/// `width`. Several lists that must line up share the widest of theirs.
pub fn property_label_column(
    text: &mut TextRenderer,
    theme: &Theme,
    rows: &[PropertyRow<'_>],
) -> f64 {
    let m = ListMetrics::new(theme, Density::Regular);
    natural_label_column(text, &m, rows)
}

fn natural_label_column(text: &mut TextRenderer, m: &ListMetrics, rows: &[PropertyRow<'_>]) -> f64 {
    rows.iter()
        .map(|row| f64::from(text.measure_style(row.label, None, &m.label).0))
        .fold(0.0, f64::max)
        .ceil()
}

/// The laid-out geometry of one row.
#[derive(Clone, Debug, PartialEq)]
struct RowLayout {
    rect: Rect,
    label: Point,
    value: Point,
    value_width: f64,
    /// The value elided to one line, for an unbreakable value that did not
    /// fit.
    elided: Option<String>,
    actions: [Option<Rect>; MAX_PROPERTY_ACTIONS],
}

/// One group's title, laid out.
#[derive(Clone, Debug, PartialEq)]
struct GroupLayout {
    title_at: Point,
    /// The index of the group's first row.
    first_row: usize,
}

/// The laid-out geometry of a whole list at one width.
#[derive(Clone, Debug, PartialEq)]
struct ListLayout {
    stacked: bool,
    rows: Vec<RowLayout>,
    groups: Vec<GroupLayout>,
    label_column: f64,
    height: f64,
}

impl ListLayout {
    fn empty() -> Self {
        Self {
            stacked: false,
            rows: Vec::new(),
            groups: Vec::new(),
            label_column: 0.0,
            height: 0.0,
        }
    }
}

/// Lay out `groups` (each an optional title and its rows) from `origin` at
/// `width`, the outer width. `label_column` overrides the measured column
/// (a shared one).
fn lay_out(
    text: &mut TextRenderer,
    m: &ListMetrics,
    groups: &[(Option<&str>, &[PropertyRow<'_>])],
    origin: Point,
    width: f64,
    label_column: Option<f64>,
) -> ListLayout {
    let all: Vec<PropertyRow<'_>> = groups
        .iter()
        .flat_map(|(_, rows)| rows.iter().copied())
        .collect();
    let natural = label_column.unwrap_or_else(|| natural_label_column(text, m, &all));
    let label_col = natural.min(width * m.max_label_share);
    let most_actions = all.iter().map(PropertyRow::action_count).max().unwrap_or(0) as f64;
    let action_col = if most_actions > 0.0 {
        most_actions * m.action + (most_actions - 1.0) * m.action_gap + m.column_gap / 2.0
    } else {
        0.0
    };
    let inner = (width - 2.0 * m.pad_x).max(0.0);
    let line = m.line(text);
    let min_value = f64::from(MIN_VALUE_LINES) * line;
    let readable = f64::from(READABLE_LINES) * line;
    let stacked = inner - label_col - m.column_gap - action_col < min_value;
    let mut y = origin.y;
    let mut out = Vec::with_capacity(all.len());
    let mut group_layouts = Vec::new();
    for (group_index, (title, rows)) in groups.iter().enumerate() {
        if let Some(title) = title {
            if group_index > 0 {
                y += m.sibling;
            }
            group_layouts.push(GroupLayout {
                title_at: Point::new(origin.x + m.pad_x, y),
                first_row: out.len(),
            });
            y += f64::from(text.measure_style(title, None, &m.group).1) + m.related;
        }
        for row in *rows {
            let value_style = m.value_style(row);
            let (value_x, value_width, label_top_h) = if stacked {
                let label_h = f64::from(text.measure_style(row.label, None, &m.stacked_label).1);
                (
                    origin.x + m.pad_x,
                    inner - action_col,
                    label_h + m.stacked_gap,
                )
            } else {
                (
                    origin.x + m.pad_x + label_col + m.column_gap,
                    inner - label_col - m.column_gap - action_col,
                    0.0,
                )
            };
            let value_width = if row.unbreakable() {
                value_width
            } else {
                value_width.min(readable)
            };
            let wrap = crate::num::f32_of_f64(value_width.max(1.0));
            let (elided, value_h) = if row.unbreakable() {
                let (w, h) = text.measure_style(row.value, None, &value_style);
                let elided = (f64::from(w) > value_width + 0.5)
                    .then(|| text.elide_to_width_style(row.value, wrap, &value_style));
                (elided, f64::from(h))
            } else {
                (
                    None,
                    f64::from(text.measure_style(row.value, Some(wrap), &value_style).1),
                )
            };
            let label_h = f64::from(text.measure_style(row.label, None, &m.label).1);
            let content = if stacked {
                label_top_h + value_h
            } else {
                value_h.max(label_h)
            };
            let height = (content + 2.0 * m.pad_y).max(m.min_row).ceil();
            let rect = Rect::new(origin.x, y, origin.x + width, y + height);
            let top = y + (height - content) / 2.0;
            let label = Point::new(origin.x + m.pad_x, top);
            let value = Point::new(value_x, top + label_top_h);
            let cy = rect.center().y;
            let mut actions = [None; MAX_PROPERTY_ACTIONS];
            let count = row.action_count();
            let mut right = rect.x1 - m.pad_x;
            for slot in actions.iter_mut().take(count).rev() {
                *slot = Some(Rect::new(
                    right - m.action,
                    cy - m.action / 2.0,
                    right,
                    cy + m.action / 2.0,
                ));
                right -= m.action + m.action_gap;
            }
            out.push(RowLayout {
                rect,
                label,
                value,
                value_width,
                elided,
                actions,
            });
            y += height;
        }
    }
    ListLayout {
        stacked,
        rows: out,
        groups: group_layouts,
        label_column: natural,
        height: y - origin.y,
    }
}

/// Immediate-mode property list. `paint` takes the list's outer rect: the
/// content edge grown by [`row_bleed`](crate::chrome::row_bleed).
#[derive(Clone, Debug)]
pub struct PropertyListFace<'a> {
    rows: &'a [PropertyRow<'a>],
    striped: bool,
    density: Density,
    label_column: Option<f64>,
    hovered_action: Option<(usize, usize)>,
}

impl<'a> PropertyListFace<'a> {
    #[must_use]
    pub fn new(rows: &'a [PropertyRow<'a>]) -> Self {
        Self {
            rows,
            striped: false,
            density: Density::Regular,
            label_column: None,
            hovered_action: None,
        }
    }

    /// Tint alternate rows: for long, dense inspectors where the eye has
    /// to follow a row across.
    #[must_use]
    pub fn striped(mut self) -> Self {
        self.striped = true;
        self
    }

    /// Rows a small control tall: an inspector beside a canvas.
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// The label column every list that must line up uses, from the widest
    /// [`property_label_column`] among them.
    #[must_use]
    pub fn label_column(mut self, width: f64) -> Self {
        self.label_column = Some(width);
        self
    }

    /// The row whose first action the pointer is over.
    #[must_use]
    pub fn hovered_action(mut self, row: Option<usize>) -> Self {
        self.hovered_action = row.map(|row| (row, 0));
        self
    }

    fn layout(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        origin: Point,
        width: f64,
    ) -> ListLayout {
        lay_out(
            text,
            &ListMetrics::new(theme, self.density),
            &[(None, self.rows)],
            origin,
            width,
            self.label_column,
        )
    }

    /// The height the list takes at `width`.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        self.layout(text, theme, Point::ZERO, width).height
    }

    /// Whether the list stacks labels above values at `width`.
    pub fn is_stacked(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> bool {
        self.layout(text, theme, Point::ZERO, width).stacked
    }

    /// Each row's first action target in a list laid out from `origin` at
    /// `width`, by row index.
    pub fn action_rects(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        origin: Point,
        width: f64,
    ) -> Vec<(usize, Rect)> {
        self.action_targets(text, theme, origin, width)
            .into_iter()
            .filter(|((_, action), _)| *action == 0)
            .map(|((row, _), rect)| (row, rect))
            .collect()
    }

    /// Every action target in a list laid out from `origin` at `width`, by
    /// row index and action index.
    pub fn action_targets(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        origin: Point,
        width: f64,
    ) -> Vec<((usize, usize), Rect)> {
        targets(&self.layout(text, theme, origin, width))
    }

    /// Paint the list from `rect`'s top-left at `rect`'s width.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let m = ListMetrics::new(ctx.theme, self.density);
        let layout = self.layout(ctx.text, ctx.theme, rect.origin(), rect.width());
        paint_layout(
            ctx,
            &m,
            &[(None, self.rows)],
            &layout,
            self.striped,
            self.hovered_action,
        );
    }

    /// One accessibility node per row: a labelled value.
    #[must_use]
    pub fn accessibility(&self) -> Vec<AccessNode> {
        self.rows.iter().map(|row| row_node(row, None)).collect()
    }
}

fn targets(layout: &ListLayout) -> Vec<((usize, usize), Rect)> {
    layout
        .rows
        .iter()
        .enumerate()
        .flat_map(|(i, row)| {
            row.actions
                .iter()
                .enumerate()
                .filter_map(move |(a, r)| r.map(|r| ((i, a), r)))
        })
        .collect()
}

fn row_node(row: &PropertyRow<'_>, accessible: Option<&str>) -> AccessNode {
    let mut node = AccessNode::new(Role::StaticText)
        .with_name(row.label)
        .with_value(accessible.unwrap_or(row.value));
    let actions: Vec<AccessNode> = row
        .actions
        .iter()
        .flatten()
        .map(|(_, label)| {
            AccessNode::new(Role::Button)
                .with_name(format!("{label} {}", row.label))
                .with_state(AccessState::new().focusable(true))
        })
        .collect();
    if !actions.is_empty() {
        node = node.with_children(actions);
    }
    node
}

fn paint_layout(
    ctx: &mut PaintCtx<'_>,
    m: &ListMetrics,
    groups: &[(Option<&str>, &[PropertyRow<'_>])],
    layout: &ListLayout,
    striped: bool,
    hovered_action: Option<(usize, usize)>,
) {
    let theme = ctx.theme;
    let ground = theme.tokens.colors.surfaces.content;
    for (group, (title, _)) in layout
        .groups
        .iter()
        .zip(groups.iter().filter(|(title, _)| title.is_some()))
    {
        if let Some(title) = title {
            ctx.text(title, &m.group, theme.text_primary, group.title_at, None);
        }
    }
    let rows = groups.iter().flat_map(|(_, rows)| rows.iter());
    for (index, (row, geo)) in rows.zip(&layout.rows).enumerate() {
        if striped && index % 2 == 1 {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                Colors::hairline_on(ground, STRIPE_ALPHA),
                None,
                &RoundedRect::from_rect(geo.rect, m.radius),
            );
        }
        let label_style = if layout.stacked {
            &m.stacked_label
        } else {
            &m.label
        };
        ctx.text(row.label, label_style, theme.text_dim, geo.label, None);
        let ink = match row.tone {
            Tone::Neutral => theme.text_primary,
            tone => theme
                .tokens
                .colors
                .ink_on(ground, tone.colour(theme), SMALL_TEXT_MIN_CONTRAST),
        };
        let style = m.value_style(row);
        if let Some(elided) = &geo.elided {
            ctx.text(elided, &style, ink, geo.value, None);
        } else if row.unbreakable() {
            ctx.text(row.value, &style, ink, geo.value, None);
        } else {
            ctx.text(
                row.value,
                &style,
                ink,
                geo.value,
                Some(crate::num::f32_of_f64(geo.value_width.max(1.0))),
            );
        }
        for (action_index, (action, target)) in row.actions.iter().zip(&geo.actions).enumerate() {
            let (Some((icon, _)), Some(target)) = (action, target) else {
                continue;
            };
            let hovered = hovered_action == Some((index, action_index));
            if hovered {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    theme.tokens.colors.intent.selection_hover,
                    None,
                    &RoundedRect::from_rect(*target, m.radius),
                );
            }
            let glyph = Rect::from_center_size(target.center(), (m.icon, m.icon));
            let ink = if hovered {
                theme.text_primary
            } else {
                theme.text_dim
            };
            icon.draw(ctx.scene, ctx.transform, glyph, ink);
        }
    }
}

/// Alternate-row tint: a derived hairline wash of the ground.
const STRIPE_ALPHA: u8 = 10;

/// One property of a retained [`PropertyList`].
pub struct Property {
    label: String,
    value: String,
    kind: TextKind,
    tone: Tone,
    accessible_value: Option<String>,
    actions: Vec<PropertyAction>,
}

enum PropertyAction {
    Copy,
    Custom {
        icon: Icon,
        label: String,
        on_activate: Box<dyn FnMut()>,
    },
}

impl Property {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            kind: TextKind::Prose,
            tone: Tone::Neutral,
            accessible_value: None,
            actions: Vec::new(),
        }
    }

    /// See [`PropertyRow::kind`].
    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        self
    }

    /// See [`PropertyRow::tone`].
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// What assistive technology reads for the value when the painted one
    /// is not it (a masked secret, a value shown as a symbol).
    #[must_use]
    pub fn with_accessible_value(mut self, value: impl Into<String>) -> Self {
        self.accessible_value = Some(value.into());
        self
    }

    /// A trailing "Copy" action that puts the value on the clipboard.
    #[must_use]
    pub fn copyable(mut self) -> Self {
        if self.actions.len() < MAX_PROPERTY_ACTIONS {
            self.actions.push(PropertyAction::Copy);
        }
        self
    }

    /// A trailing action of your own (open a link, reveal a secret). A row
    /// holds up to two actions; extra ones are ignored.
    #[must_use]
    pub fn action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        on_activate: impl FnMut() + 'static,
    ) -> Self {
        if self.actions.len() < MAX_PROPERTY_ACTIONS {
            self.actions.push(PropertyAction::Custom {
                icon,
                label: label.into(),
                on_activate: Box::new(on_activate),
            });
        }
        self
    }

    fn row(&self) -> PropertyRow<'_> {
        let mut row = PropertyRow::new(&self.label, &self.value)
            .with_kind(self.kind)
            .tone(self.tone);
        for action in &self.actions {
            row = match action {
                PropertyAction::Copy => row.action(Icon::Copy, "Copy"),
                PropertyAction::Custom { icon, label, .. } => row.action(*icon, label),
            };
        }
        row
    }
}

/// A label column shared by several property lists, so their values line
/// up (the lists of one [`Section`](crate::widget::Section) share one on
/// their own). Clone the handle into each list with
/// [`PropertyList::shared_column`].
#[derive(Clone, Debug, Default)]
pub struct PropertyColumn(Rc<Cell<f64>>);

impl PropertyColumn {
    pub fn new() -> Self {
        Self::default()
    }

    fn offer(&self, width: f64) {
        if width > self.0.get() {
            self.0.set(width);
        }
    }

    fn get(&self) -> f64 {
        self.0.get()
    }
}

/// A retained property list. See the module docs.
pub struct PropertyList {
    groups: Vec<(Option<String>, Vec<Property>)>,
    striped: bool,
    density: Density,
    id: Option<String>,
    column: Option<PropertyColumn>,
    /// The shared column's width the last layout used.
    laid_column: f64,
    metrics: Option<ListMetrics>,
    layout: ListLayout,
    cursor: Point,
    hovered_action: Option<(usize, usize)>,
    pressed_action: crate::pointer_replay::HeldPress<(usize, usize)>,
    rect: Rect,
    dirty: bool,
}

impl PropertyList {
    pub fn new(properties: Vec<Property>) -> Self {
        Self::sections(vec![(String::new(), properties)])
    }

    /// Several titled groups (a contact's phones, addresses, dates) in one
    /// list, sharing one label column. An empty title draws none.
    pub fn sections(groups: Vec<(impl Into<String>, Vec<Property>)>) -> Self {
        Self {
            groups: groups
                .into_iter()
                .map(|(title, rows)| {
                    let title: String = title.into();
                    ((!title.is_empty()).then_some(title), rows)
                })
                .collect(),
            striped: false,
            density: Density::Regular,
            id: None,
            column: None,
            laid_column: 0.0,
            metrics: None,
            layout: ListLayout::empty(),
            cursor: Point::ZERO,
            hovered_action: None,
            pressed_action: crate::pointer_replay::HeldPress::default(),
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// See [`PropertyListFace::striped`].
    #[must_use]
    pub fn striped(mut self) -> Self {
        self.striped = true;
        self
    }

    /// See [`PropertyListFace::compact`].
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// A stable id for semantic resources, inspection and accessibility.
    /// Trailing property actions need this identity to be published as resources.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Share a label column with other lists.
    #[must_use]
    pub fn shared_column(mut self, column: &PropertyColumn) -> Self {
        self.column = Some(column.clone());
        self
    }

    /// Set by the section the list sits in, unless the list has its own.
    pub(crate) fn join_column(&mut self, column: &PropertyColumn) {
        if self.column.is_none() {
            self.column = Some(column.clone());
        }
    }

    fn groups(&self) -> Vec<(Option<&str>, Vec<PropertyRow<'_>>)> {
        self.groups
            .iter()
            .map(|(title, rows)| (title.as_deref(), rows.iter().map(Property::row).collect()))
            .collect()
    }

    fn properties(&self) -> impl Iterator<Item = &Property> {
        self.groups.iter().flat_map(|(_, rows)| rows.iter())
    }

    /// What names a row's action across a rebuild: the list, the row's
    /// label and the action, not their indices, so a rebuilt list whose
    /// rows shifted under the pointer completes no press.
    fn press_key(&self, (row, action): (usize, usize)) -> String {
        let Some(property) = self.properties().nth(row) else {
            return String::new();
        };
        let action = match property.actions.get(action) {
            Some(PropertyAction::Copy) => "copy",
            Some(PropertyAction::Custom { label, .. }) => label.as_str(),
            None => return String::new(),
        };
        format!(
            "property-list:{}:{}:{action}",
            self.id.as_deref().unwrap_or_default(),
            property.label
        )
    }

    /// Whether the last layout stacked labels above values.
    pub fn is_stacked(&self) -> bool {
        self.layout.stacked
    }

    fn outer(&self) -> Rect {
        let bleed = self.metrics.as_ref().map_or(0.0, |m| m.pad_x);
        self.rect.inflate(bleed, 0.0)
    }

    fn lay_out_at(
        &self,
        text: &mut TextRenderer,
        m: &ListMetrics,
        origin: Point,
        width: f64,
    ) -> ListLayout {
        let groups = self.groups();
        let borrowed: Vec<(Option<&str>, &[PropertyRow<'_>])> = groups
            .iter()
            .map(|(title, rows)| (*title, rows.as_slice()))
            .collect();
        let shared = self.column.as_ref().map(|column| {
            let all: Vec<PropertyRow<'_>> =
                groups.iter().flat_map(|(_, r)| r.iter().copied()).collect();
            column.offer(natural_label_column(text, m, &all));
            column.get()
        });
        lay_out(text, m, &borrowed, origin, width, shared)
    }
}

impl Widget for PropertyList {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(ListMetrics::new(theme, self.density));
        let style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(metrics) = self.metrics.clone() else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => crate::num::f32_of_f64(
                metrics.line(text_renderer) * f64::from(MIN_VALUE_LINES) * 3.0,
            ),
        });
        let outer = f64::from(width) + 2.0 * metrics.pad_x;
        let layout = self.lay_out_at(text_renderer, &metrics, Point::ZERO, outer);
        self.laid_column = self.column.as_ref().map_or(0.0, PropertyColumn::get);
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(layout.height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let outer = self.outer();
        if presence.should_cull(outer) {
            return;
        }
        let metrics = ListMetrics::new(theme, self.density);
        let layout = self.lay_out_at(text_renderer, &metrics, outer.origin(), outer.width());
        let groups = self.groups();
        let borrowed: Vec<(Option<&str>, &[PropertyRow<'_>])> = groups
            .iter()
            .map(|(title, rows)| (*title, rows.as_slice()))
            .collect();
        paint_layout(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            &metrics,
            &borrowed,
            &layout,
            self.striped,
            self.hovered_action,
        );
        self.layout = layout;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let target_at = |layout: &ListLayout, point: Point| {
            targets(layout)
                .into_iter()
                .find(|(_, r)| r.contains(point))
                .map(|(target, _)| target)
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = ctx.logical(position);
                let hovered = target_at(&self.layout, self.cursor);
                if hovered != self.hovered_action {
                    self.hovered_action = hovered;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                use crate::pointer_replay::{PressAt, Release};
                let over = targets(&self.layout)
                    .into_iter()
                    .find(|(_, r)| r.contains(self.cursor));
                let key = over.map(|(target, _)| self.press_key(target));
                let at = over
                    .zip(key.as_deref())
                    .map(|((target, rect), key)| PressAt { target, key, rect });
                match state {
                    ElementState::Pressed => {
                        if let Some(at) = at {
                            self.pressed_action.press(at);
                            *ctx.handled = true;
                        }
                    }
                    ElementState::Released => match self.pressed_action.release(at) {
                        Release::NotHeld => {}
                        Release::Cancelled => *ctx.handled = true,
                        Release::Click((row, action)) => {
                            *ctx.handled = true;
                            self.run_action(row, action);
                        }
                    },
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        targets(&self.layout)
            .iter()
            .any(|(_, r)| r.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let ((row, action), rect) = targets(&self.layout)
            .into_iter()
            .find(|(_, r)| r.contains(point))?;
        let property = self.properties().nth(row)?;
        let label = match property.actions.get(action)? {
            PropertyAction::Copy => "Copy".to_owned(),
            PropertyAction::Custom { label, .. } => label.clone(),
        };
        Some((rect, label))
    }

    crate::redraw_flag!();

    /// Laid out again when a list sharing its label column widened it.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
            || self
                .column
                .as_ref()
                .is_some_and(|column| (column.get() - self.laid_column).abs() > 0.5)
    }

    fn name(&self) -> &'static str {
        "PropertyList"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let children = self
            .properties()
            .enumerate()
            .map(|(index, property)| {
                let mut node = row_node(&property.row(), property.accessible_value.as_deref());
                node.key = Some(index as u64);
                for (action, child) in node.children.iter_mut().enumerate() {
                    child.key = Some(action as u64);
                }
                if let Some(layout) = self.layout.rows.get(index) {
                    node = node.with_bounds(layout.rect);
                    for (action, rect) in node
                        .children
                        .iter_mut()
                        .zip(layout.actions.iter().flatten())
                    {
                        action.bounds = Some((*rect).into());
                    }
                }
                node
            })
            .collect();
        let mut node = AccessNode::new(Role::List)
            .with_bounds(self.rect)
            .with_children(children);
        if let Some(id) = &self.id {
            node = node.with_semantic_key(id);
        }
        Some(node.with_keyed_action_routes(self.id.as_deref().unwrap_or("property-list")))
    }

    /// An action addressed to one of a row's action buttons: `[row,
    /// action]`.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        match key_path {
            [row, index] if matches!(action, crate::a11y::Action::Activate) => {
                match (usize::try_from(*row), usize::try_from(*index)) {
                    (Ok(row), Ok(index)) => self.run_action(row, index),
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

impl PropertyList {
    fn run_action(&mut self, row: usize, action: usize) -> bool {
        let Some(property) = self
            .groups
            .iter_mut()
            .flat_map(|(_, rows)| rows.iter_mut())
            .nth(row)
        else {
            return false;
        };
        let value = property.value.clone();
        match property.actions.get_mut(action) {
            Some(PropertyAction::Copy) => {
                crate::text_selection::clipboard_copy(&value);
                true
            }
            Some(PropertyAction::Custom { on_activate, .. }) => {
                on_activate();
                true
            }
            None => false,
        }
    }
}

/// A read-only value as a control: the value a [`Field`](crate::widget::Field)
/// shows where it cannot be edited (an account id, a generated key), in
/// primary ink with its kind's text treatment and an optional trailing copy
/// action. Unbreakable kinds are elided before the action.
pub struct PropertyValue {
    id: Option<String>,
    value: String,
    kind: TextKind,
    copyable: bool,
    accessible_name: Option<String>,
    rect: Rect,
    copy_rect: Option<Rect>,
    hovered: bool,
    pressed: crate::pointer_replay::HeldPress,
    metrics: Option<ListMetrics>,
}

impl PropertyValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            id: None,
            value: value.into(),
            kind: TextKind::Prose,
            copyable: false,
            accessible_name: None,
            rect: Rect::ZERO,
            copy_rect: None,
            hovered: false,
            pressed: crate::pointer_replay::HeldPress::default(),
            metrics: None,
        }
    }

    /// A stable identity for resource publication and accessibility actions.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        self
    }

    /// A trailing copy action.
    #[must_use]
    pub fn copyable(mut self) -> Self {
        self.copyable = true;
        self
    }

    /// The name the value is announced by (its field's label).
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
    }

    fn row(&self) -> PropertyRow<'_> {
        PropertyRow::new("", &self.value).with_kind(self.kind)
    }

    fn action_w(&self, m: &ListMetrics) -> f64 {
        if self.copyable {
            m.action + m.action_gap
        } else {
            0.0
        }
    }
}

impl Widget for PropertyValue {
    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(ListMetrics::new(theme, Density::Regular));
        let style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(m) = self.metrics.clone() else {
            return taffy::Size::ZERO;
        };
        let style = m.value_style(&self.row());
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => crate::num::f32_of_f64(
                f64::from(text.measure_style(&self.value, None, &style).0) + self.action_w(&m),
            ),
        });
        let room = (f64::from(width) - self.action_w(&m)).max(1.0);
        let h = if self.row().unbreakable() {
            text.measure_style(&self.value, None, &style).1
        } else {
            text.measure_style(&self.value, Some(crate::num::f32_of_f64(room)), &style)
                .1
        };
        let min = if self.copyable { m.action } else { 0.0 };
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(f64::from(h).max(min)),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.copy_rect = self.metrics.as_ref().filter(|_| self.copyable).map(|m| {
            let cy = self.rect.center().y;
            Rect::new(
                self.rect.x1 - m.action,
                cy - m.action / 2.0,
                self.rect.x1,
                cy + m.action / 2.0,
            )
        });
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let m = ListMetrics::new(theme, Density::Regular);
        let style = m.value_style(&self.row());
        let room = (self.rect.width() - self.action_w(&m)).max(1.0);
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        let (_, h) = ctx.text.measure_style(&self.value, None, &style);
        if self.row().unbreakable() {
            let shown =
                ctx.text
                    .elide_to_width_style(&self.value, crate::num::f32_of_f64(room), &style);
            let y = self.rect.center().y - f64::from(h) / 2.0;
            ctx.text(
                &shown,
                &style,
                theme.text_primary,
                Point::new(self.rect.x0, y),
                None,
            );
        } else {
            ctx.text(
                &self.value,
                &style,
                theme.text_primary,
                self.rect.origin(),
                Some(crate::num::f32_of_f64(room)),
            );
        }
        if let Some(target) = self.copy_rect {
            if self.hovered {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    theme.tokens.colors.intent.selection_hover,
                    None,
                    &RoundedRect::from_rect(target, m.radius),
                );
            }
            let glyph = Rect::from_center_size(target.center(), (m.icon, m.icon));
            let ink = if self.hovered {
                theme.text_primary
            } else {
                theme.text_dim
            };
            Icon::Copy.draw(ctx.scene, ctx.transform, glyph, ink);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let Some(target) = self.copy_rect else {
            return;
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.hovered = target.contains(point);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                use crate::pointer_replay::{PressAt, Release};
                // The field's name (or, unnamed, the value) names the copy
                // action across a rebuild.
                let key = format!(
                    "property-value:{}",
                    self.accessible_name.as_deref().unwrap_or(&self.value)
                );
                let at = self.hovered.then_some(PressAt::one(&key, target));
                match state {
                    ElementState::Pressed => {
                        if let Some(at) = at {
                            self.pressed.press(at);
                            *ctx.handled = true;
                        }
                    }
                    ElementState::Released => {
                        let release = self.pressed.release(at);
                        *ctx.handled |= release.ended();
                        if release == Release::Click(()) {
                            crate::text_selection::clipboard_copy(&self.value);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.copy_rect
            .is_some_and(|r| r.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.copy_rect
            .filter(|r| r.contains(point))
            .map(|r| (r, "Copy".to_owned()))
    }

    fn name(&self) -> &'static str {
        "PropertyValue"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut node = AccessNode::new(Role::TextInput)
            .with_optional_name(self.accessible_name.clone())
            .with_value(self.value.clone())
            .with_bounds(self.rect)
            .with_state(AccessState::new().read_only(true));
        if self.copyable {
            node = node.with_children(vec![
                AccessNode::new(Role::Button)
                    .with_key(0)
                    .with_name("Copy")
                    .with_state(AccessState::new().focusable(true)),
            ]);
        }
        if let Some(id) = &self.id {
            node = node.with_semantic_key(id);
        }
        Some(node.with_keyed_action_routes(self.id.as_deref().unwrap_or("property-value")))
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        if self.copyable && key_path == [0] && matches!(action, crate::a11y::Action::Activate) {
            crate::text_selection::clipboard_copy(&self.value);
            return true;
        }
        false
    }
}

#[cfg(test)]
#[path = "property_list_tests.rs"]
mod tests;

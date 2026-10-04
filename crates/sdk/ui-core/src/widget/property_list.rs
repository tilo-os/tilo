//! `PropertyList`: labelled values (a contact's details, a file's metadata,
//! a certificate's fields), as a retained widget and as the
//! [`PropertyListFace`] immediate painter with identical geometry.
//!
//! Labels sit in one column, measured from the longest label, in secondary
//! ink; values wrap in the column beside it in primary ink. When the value
//! column would get too narrow the list stacks: each label above its value.
//! A row may carry one trailing action (copy, open, reveal). The list sits on
//! its ground separated by space, never inside a card; dense inspectors may
//! stripe alternate rows with a derived tint.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextKind, TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::Colors;
use crate::widget::{TaffyTreeExt, Widget};
use zeroize::Zeroizing;

/// One label and its value, as the face paints it.
#[derive(Clone, Copy)]
pub struct PropertyRow<'a> {
    pub label: &'a str,
    pub value: &'a str,
    /// What the value is: [`TextKind::Numeric`] for sizes, offsets and
    /// dates, [`TextKind::Identifier`] for addresses and handles,
    /// [`TextKind::Secret`] for keys and hashes.
    pub kind: TextKind,
    /// One trailing action for this value.
    pub action: Option<(Icon, &'a str)>,
    protected: bool,
}

impl<'a> PropertyRow<'a> {
    #[must_use]
    pub fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            kind: TextKind::Prose,
            action: None,
            protected: false,
        }
    }

    /// A value that may be painted but must never enter semantic output or
    /// reusable text caches. `TextKind::Secret` alone only chooses typography.
    #[must_use]
    pub fn secret_value(label: &'a str, value: &'a str) -> Self {
        Self {
            protected: true,
            kind: TextKind::Secret,
            ..Self::new(label, value)
        }
    }

    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        self
    }

    #[must_use]
    pub fn action(mut self, icon: Icon, label: &'a str) -> Self {
        self.action = Some((icon, label));
        self
    }
}

impl std::fmt::Debug for PropertyRow<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PropertyRow")
            .field("label", &self.label)
            .field(
                "value",
                &if self.protected {
                    "[protected]"
                } else {
                    self.value
                },
            )
            .field("kind", &self.kind)
            .field("action", &self.action)
            .finish()
    }
}

/// The resolved geometry of a property list in `theme`.
#[derive(Clone, Debug)]
struct ListMetrics {
    label: TextStyle,
    stacked_label: TextStyle,
    value: TextStyle,
    pad_x: f64,
    pad_y: f64,
    column_gap: f64,
    stacked_gap: f64,
    min_row: f64,
    max_label_share: f64,
    action: f64,
    icon: f64,
    radius: f64,
}

impl ListMetrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            label: theme.text(TextRole::Body),
            stacked_label: theme.text(TextRole::Caption),
            value: theme.text(TextRole::Body),
            pad_x: f64::from(spacing.s2),
            pad_y: f64::from(spacing.s1),
            column_gap: f64::from(spacing.s4),
            stacked_gap: 0.0,
            min_row: f64::from(metrics.min_pointer_target),
            max_label_share: 0.4,
            action: f64::from(metrics.bar_control_height),
            icon: f64::from(metrics.icon_sm),
            radius: f64::from(theme.tokens.radii.item),
        }
    }

    fn value_style(&self, row: &PropertyRow<'_>) -> TextStyle {
        self.value.clone().with_kind(row.kind)
    }
}

/// The widest a list runs, in line boxes of its value text.
const READABLE_LINES: u8 = 32;

/// The value column's minimum width before the list stacks, in line boxes.
const MIN_VALUE_LINES: u8 = 8;

/// The laid-out geometry of one row.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowLayout {
    rect: Rect,
    label: Point,
    value: Point,
    value_width: f64,
    action: Option<Rect>,
}

/// The laid-out geometry of a whole list at one width.
#[derive(Clone, Debug, PartialEq)]
struct ListLayout {
    stacked: bool,
    rows: Vec<RowLayout>,
    height: f64,
}

fn lay_out(
    text: &mut TextRenderer,
    m: &ListMetrics,
    rows: &[PropertyRow<'_>],
    origin: Point,
    width: f64,
) -> ListLayout {
    // A list of facts reads at a bounded measure, so a trailing action stays
    // beside its value on a wide window.
    let readable =
        f64::from(READABLE_LINES) * f64::from(text.measure_style("Ag", None, &m.value).1);
    let width = width.min(readable);
    let label_col = rows
        .iter()
        .map(|row| f64::from(text.measure_style(row.label, None, &m.label).0))
        .fold(0.0, f64::max)
        .ceil()
        .min(width * m.max_label_share);
    let any_action = rows.iter().any(|row| row.action.is_some());
    let action_col = if any_action {
        m.action + m.column_gap / 2.0
    } else {
        0.0
    };
    let inner = (width - 2.0 * m.pad_x).max(0.0);
    // The narrowest useful value column, in lines of value text so it grows
    // with the text scale like the labels beside it.
    let min_value =
        f64::from(MIN_VALUE_LINES) * f64::from(text.measure_style("Ag", None, &m.value).1);
    let stacked = inner - label_col - m.column_gap - action_col < min_value;
    let mut y = origin.y;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
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
        let wrap = crate::num::f32_of_f64(value_width.max(1.0));
        let value_h = f64::from(if row.protected {
            text.measure_secret_style(row.value, Some(wrap), &value_style)
                .1
        } else {
            text.measure_style(row.value, Some(wrap), &value_style).1
        });
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
        let action = row.action.map(|_| {
            let cy = rect.center().y;
            Rect::new(
                rect.x1 - m.pad_x - m.action,
                cy - m.action / 2.0,
                rect.x1 - m.pad_x,
                cy + m.action / 2.0,
            )
        });
        out.push(RowLayout {
            rect,
            label,
            value,
            value_width,
            action,
        });
        y += height;
    }
    ListLayout {
        stacked,
        rows: out,
        height: y - origin.y,
    }
}

/// Immediate-mode property list.
#[derive(Clone, Debug)]
pub struct PropertyListFace<'a> {
    rows: &'a [PropertyRow<'a>],
    striped: bool,
    hovered_action: Option<usize>,
}

impl<'a> PropertyListFace<'a> {
    #[must_use]
    pub fn new(rows: &'a [PropertyRow<'a>]) -> Self {
        Self {
            rows,
            striped: false,
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

    /// The row whose action the pointer is over.
    #[must_use]
    pub fn hovered_action(mut self, row: Option<usize>) -> Self {
        self.hovered_action = row;
        self
    }

    /// The height the list takes at `width`.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        lay_out(
            text,
            &ListMetrics::new(theme),
            self.rows,
            Point::ZERO,
            width,
        )
        .height
    }

    /// Whether the list stacks labels above values at `width`.
    pub fn is_stacked(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> bool {
        lay_out(
            text,
            &ListMetrics::new(theme),
            self.rows,
            Point::ZERO,
            width,
        )
        .stacked
    }

    /// Each row's action target in a list laid out from `origin` at
    /// `width`, by row index.
    pub fn action_rects(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        origin: Point,
        width: f64,
    ) -> Vec<(usize, Rect)> {
        lay_out(text, &ListMetrics::new(theme), self.rows, origin, width)
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| row.action.map(|r| (i, r)))
            .collect()
    }

    /// Paint the list from `rect`'s top-left at `rect`'s width.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let m = ListMetrics::new(ctx.theme);
        let layout = lay_out(ctx.text, &m, self.rows, rect.origin(), rect.width());
        paint_layout(
            ctx,
            &m,
            self.rows,
            &layout,
            self.striped,
            self.hovered_action,
        );
    }

    /// One accessibility node per row: a labelled value.
    #[must_use]
    pub fn accessibility(&self) -> Vec<AccessNode> {
        self.rows
            .iter()
            .map(|row| {
                AccessNode::new(Role::StaticText)
                    .with_name(row.label)
                    .with_optional_value((!row.protected).then(|| row.value.to_owned()))
                    .with_state(AccessState {
                        protected: row.protected,
                        ..Default::default()
                    })
            })
            .collect()
    }
}

fn paint_layout(
    ctx: &mut PaintCtx<'_>,
    m: &ListMetrics,
    rows: &[PropertyRow<'_>],
    layout: &ListLayout,
    striped: bool,
    hovered_action: Option<usize>,
) {
    let theme = ctx.theme;
    let ground = theme.tokens.colors.surfaces.content;
    for (index, (row, geo)) in rows.iter().zip(&layout.rows).enumerate() {
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
        let wrap = Some(crate::num::f32_of_f64(geo.value_width.max(1.0)));
        if row.protected {
            ctx.text.draw_secret_style(
                ctx.scene,
                row.value,
                theme.text_primary,
                ctx.transform * Affine::translate((geo.value.x, geo.value.y)),
                wrap,
                &m.value_style(row),
            );
        } else {
            ctx.text(
                row.value,
                &m.value_style(row),
                theme.text_primary,
                geo.value,
                wrap,
            );
        }
        if let (Some((icon, _)), Some(target)) = (row.action, geo.action) {
            if hovered_action == Some(index) {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    theme.tokens.colors.intent.selection_hover,
                    None,
                    &RoundedRect::from_rect(target, m.radius),
                );
            }
            let glyph = Rect::from_center_size(target.center(), (m.icon, m.icon));
            let ink = if hovered_action == Some(index) {
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
    value: PropertyValue,
    kind: TextKind,
    action: Option<PropertyAction>,
}

enum PropertyValue {
    Public(String),
    Secret(Zeroizing<String>),
}

impl PropertyValue {
    fn text(&self) -> &str {
        match self {
            Self::Public(value) => value,
            Self::Secret(value) => value,
        }
    }
    fn protected(&self) -> bool {
        matches!(self, Self::Secret(_))
    }
}

impl std::fmt::Debug for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.row(), f)
    }
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
            value: PropertyValue::Public(value.into()),
            kind: TextKind::Prose,
            action: None,
        }
    }

    /// A revealed secret whose owned storage is zeroized on drop. Inspection,
    /// accessibility and Debug never disclose it. Painting marks the frame as
    /// protected and bypasses shared text caches. Use an app-owned leased
    /// clipboard action when copying is needed; `copyable` is unavailable.
    #[must_use]
    pub fn secret_value(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: PropertyValue::Secret(Zeroizing::new(value.into())),
            kind: TextKind::Secret,
            action: None,
        }
    }

    /// See [`PropertyRow::kind`].
    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        self
    }

    /// A trailing "Copy" action that puts the value on the clipboard.
    #[must_use]
    pub fn copyable(mut self) -> Self {
        if !self.value.protected() {
            self.action = Some(PropertyAction::Copy);
        }
        self
    }

    /// A trailing action of your own (open a link, reveal a secret).
    #[must_use]
    pub fn action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        on_activate: impl FnMut() + 'static,
    ) -> Self {
        self.action = Some(PropertyAction::Custom {
            icon,
            label: label.into(),
            on_activate: Box::new(on_activate),
        });
        self
    }

    fn row(&self) -> PropertyRow<'_> {
        let mut row = PropertyRow::new(&self.label, self.value.text());
        row.protected = self.value.protected();
        row.kind = self.kind;
        row.action = match &self.action {
            None => None,
            Some(PropertyAction::Copy) => Some((Icon::Copy, "Copy")),
            Some(PropertyAction::Custom { icon, label, .. }) => Some((*icon, label.as_str())),
        };
        row
    }
}

/// A retained property list. Tab enters its action group; arrow keys move
/// between row actions and Enter/Space activate the focused action. Pointer,
/// keyboard and assistive activation all run the same callback.
/// See the module docs.
static NEXT_LIST_ID: AtomicU64 = AtomicU64::new(1);

pub struct PropertyList {
    id: String,
    focused: bool,
    focus_visible: bool,
    focused_action: Option<usize>,
    properties: Vec<Property>,
    striped: bool,
    metrics: Option<ListMetrics>,
    layout: ListLayout,
    cursor: Point,
    hovered_action: Option<usize>,
    pressed_action: Option<usize>,
    rect: Rect,
    dirty: bool,
}

impl PropertyList {
    pub fn new(properties: Vec<Property>) -> Self {
        Self {
            id: format!(
                "property-list-{}",
                NEXT_LIST_ID.fetch_add(1, Ordering::Relaxed)
            ),
            focused: false,
            focus_visible: false,
            focused_action: None,
            properties,
            striped: false,
            metrics: None,
            layout: ListLayout {
                stacked: false,
                rows: Vec::new(),
                height: 0.0,
            },
            cursor: Point::ZERO,
            hovered_action: None,
            pressed_action: None,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// A stable namespace for this list's row-action semantic targets. Use a
    /// distinct ID for each list when an app rebuilds its presentation.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    fn action_target(&self, index: usize) -> String {
        format!("{}.action.{index}", self.id)
    }

    fn action_indices(&self) -> Vec<usize> {
        self.properties
            .iter()
            .enumerate()
            .filter_map(|(index, property)| property.action.as_ref().map(|_| index))
            .collect()
    }

    fn focus_action(&mut self, index: usize) -> bool {
        if self
            .properties
            .get(index)
            .is_none_or(|property| property.action.is_none())
        {
            return false;
        }
        self.focused_action = Some(index);
        self.focused = true;
        self.focus_visible = true;
        self.dirty = true;
        true
    }

    /// See [`PropertyListFace::striped`].
    #[must_use]
    pub fn striped(mut self) -> Self {
        self.striped = true;
        self
    }

    fn rows(&self) -> Vec<PropertyRow<'_>> {
        self.properties.iter().map(Property::row).collect()
    }

    /// Whether the last layout stacked labels above values.
    pub fn is_stacked(&self) -> bool {
        self.layout.stacked
    }
}

impl Widget for PropertyList {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(ListMetrics::new(theme));
        let node = taffy.leaf_node(Style {
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
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
                f64::from(text_renderer.measure_style("Ag", None, &metrics.value).1)
                    * f64::from(MIN_VALUE_LINES)
                    * 3.0,
            ),
        });
        let rows = self.rows();
        let layout = lay_out(
            text_renderer,
            &metrics,
            &rows,
            Point::ZERO,
            f64::from(width),
        );
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(layout.height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
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
        if presence.should_cull(self.rect) {
            return;
        }
        let metrics = ListMetrics::new(theme);
        let rows: Vec<PropertyRow<'_>> = self.properties.iter().map(Property::row).collect();
        let layout = lay_out(
            text_renderer,
            &metrics,
            &rows,
            self.rect.origin(),
            self.rect.width(),
        );
        paint_layout(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            &metrics,
            &rows,
            &layout,
            self.striped,
            self.hovered_action,
        );
        if self.focused
            && self.focus_visible
            && let Some(index) = self.focused_action
            && let Some(target) = layout.rows.get(index).and_then(|row| row.action)
        {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.you,
                None,
                &crate::surface::edge_outline(target, metrics.radius),
            );
        }
        self.layout = layout;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let target_at = |layout: &ListLayout, point: Point| {
            layout
                .rows
                .iter()
                .position(|row| row.action.is_some_and(|r| r.contains(point)))
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                let over = target_at(&self.layout, self.cursor);
                match state {
                    ElementState::Pressed if over.is_some() => {
                        self.pressed_action = over;
                        self.focused_action = over;
                        self.focus_visible = false;
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                    }
                    ElementState::Released if self.pressed_action.is_some() => {
                        let pressed = self.pressed_action.take();
                        *ctx.handled = true;
                        if over == pressed
                            && let Some(index) = pressed
                        {
                            self.run_action(index);
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let indices = self.action_indices();
                let current = self
                    .focused_action
                    .and_then(|index| indices.iter().position(|&i| i == index));
                let next = match event.logical_key {
                    Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => current
                        .and_then(|at| indices.get(at.saturating_add(1)))
                        .or_else(|| indices.first())
                        .copied(),
                    Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => current
                        .and_then(|at| at.checked_sub(1))
                        .and_then(|at| indices.get(at))
                        .or_else(|| indices.last())
                        .copied(),
                    Key::Named(NamedKey::Home) => indices.first().copied(),
                    Key::Named(NamedKey::End) => indices.last().copied(),
                    _ => None,
                };
                if let Some(index) = next {
                    self.focus_action(index);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.layout
            .rows
            .iter()
            .any(|row| row.action.is_some_and(|r| r.contains(point)))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "PropertyList"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        self.properties.iter().any(|p| p.action.is_some())
    }

    fn set_focus(&mut self, focused: bool) {
        if focused && !self.focused {
            self.focus_visible = self.pressed_action.is_none();
        }
        self.focused = focused;
        if focused && self.focused_action.is_none() {
            self.focused_action = self.action_indices().first().copied();
        }
        self.dirty = true;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        if *action == crate::a11y::Action::Activate
            && let Some(index) = self.focused_action
        {
            self.run_action(index);
            return true;
        }
        false
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [index, 0] => self.layout.rows.get(*index).and_then(|row| row.action),
            [index] => self.layout.rows.get(*index).map(|row| row.rect),
            _ => None,
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        match path {
            [index, 0] => self.focus_action(*index),
            _ => false,
        }
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        path: &[u64],
        state: &mut crate::widget::AppModel,
        root: &crate::widget::ModelPath,
    ) -> bool {
        let [row, key] = path else {
            return false;
        };
        let Ok(index) = usize::try_from(*row) else {
            return false;
        };
        let target = self.action_target(index);
        if *key != crate::a11y::hash_key(&target) {
            return false;
        }
        self.perform_semantic_action(&target, action, state, root)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let prefix = format!("{}.action.", self.id);
        let Some(index) = target
            .strip_prefix(&prefix)
            .and_then(|index| index.parse::<usize>().ok())
        else {
            return false;
        };
        if !matches!(
            action,
            crate::a11y::Action::Activate | crate::a11y::Action::Focus
        ) || !self.focus_action(index)
        {
            return false;
        }
        if *action == crate::a11y::Action::Activate {
            self.run_action(index);
        }
        true
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let rows = self.rows();
        let mut nodes = PropertyListFace::new(&rows).accessibility();
        for (index, (node, row)) in nodes.iter_mut().zip(&rows).enumerate() {
            if let Some(geometry) = self.layout.rows.get(index) {
                node.bounds = Some(geometry.rect.into());
            }
            if let Some((_, label)) = row.action {
                let mut action = AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_semantic_key(self.action_target(index))
                    .with_action_target(self.action_target(index))
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.focused && self.focused_action == Some(index),
                        ..Default::default()
                    });
                action.bounds = self
                    .layout
                    .rows
                    .get(index)
                    .and_then(|row| row.action)
                    .map(Into::into);
                node.children.push(action);
            }
        }
        Some(
            AccessNode::new(Role::List)
                .with_semantic_key(&self.id)
                .with_children(nodes),
        )
    }
}

impl PropertyList {
    fn run_action(&mut self, index: usize) {
        let Some(property) = self.properties.get_mut(index) else {
            return;
        };
        match &mut property.action {
            Some(PropertyAction::Copy) => {
                crate::text_selection::clipboard_copy(property.value.text())
            }
            Some(PropertyAction::Custom { on_activate, .. }) => on_activate(),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<PropertyRow<'static>> {
        vec![
            PropertyRow::new("Email", "ada.lovelace@example.org"),
            PropertyRow::new("Phone", "+44 20 7946 0958").with_kind(TextKind::Numeric),
            PropertyRow::new(
                "Address",
                "12 St James's Square, London SW1Y 4JH, United Kingdom",
            ),
            PropertyRow::new("Birthday", "10 December 1815").action(Icon::Copy, "Copy"),
        ]
    }

    #[test]
    fn labels_share_one_measured_column_and_values_line_up() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rows = rows();
        let m = ListMetrics::new(&theme);
        let layout = lay_out(&mut text, &m, &rows, Point::ZERO, 640.0);
        assert!(!layout.stacked);
        let x = layout.rows[0].value.x;
        for row in &layout.rows {
            assert!((row.value.x - x).abs() < 0.01, "values share a column");
            assert!(row.value.x > row.label.x);
            assert!(row.rect.height() >= m.min_row);
        }
        // The longest label decides the column: values start after it.
        let longest = f64::from(text.measure_style("Birthday", None, &m.label).0);
        assert!(x >= m.pad_x + longest + m.column_gap - 0.5);
    }

    #[test]
    fn a_narrow_list_stacks_labels_above_values() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rows = rows();
        let face = PropertyListFace::new(&rows);
        assert!(!face.is_stacked(&mut text, &theme, 640.0));
        assert!(face.is_stacked(&mut text, &theme, 220.0));
        let m = ListMetrics::new(&theme);
        let layout = lay_out(&mut text, &m, &rows, Point::ZERO, 220.0);
        for row in &layout.rows {
            assert!((row.value.x - row.label.x).abs() < 0.01, "one column");
            assert!(row.value.y > row.label.y, "label above value");
        }
        // Larger text stacks sooner.
        let big = Theme::dark().with_text_scale(2.0);
        assert!(!face.is_stacked(&mut text, &theme, 360.0));
        assert!(face.is_stacked(&mut text, &big, 360.0));
        assert!(face.height(&mut text, &big, 640.0) > face.height(&mut text, &theme, 640.0) * 1.5);
    }

    #[test]
    fn actions_sit_at_the_trailing_edge_of_their_row() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rows = rows();
        let face = PropertyListFace::new(&rows);
        let targets = face.action_rects(&mut text, &theme, Point::new(10.0, 20.0), 500.0);
        assert_eq!(targets.len(), 1);
        let (index, rect) = targets[0];
        assert_eq!(index, 3);
        assert!(rect.x1 <= 510.0 && rect.x1 > 480.0);
    }

    #[test]
    fn stripes_are_a_derived_tint_that_registers() {
        for theme in [Theme::dark(), Theme::light()] {
            let ground = theme.tokens.colors.surfaces.content;
            let stripe =
                crate::tokens::colors::composite(Colors::hairline_on(ground, STRIPE_ALPHA), ground);
            assert!(crate::surface::edge_delta(stripe, ground) > 0.0);
        }
    }
}

#[cfg(test)]
mod protected_property_tests {
    use super::*;
    use crate::headless::FrameHarness;
    use crate::secret_paint::SecretPaint;
    use crate::state::State;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn secret_property_is_opaque_in_semantics_inspection_and_debug() {
        let secret = "swordfish-property-unique";
        let property = Property::secret_value("Password", secret).copyable();
        assert!(matches!(property.value, PropertyValue::Secret(_)));
        assert!(
            property.action.is_none(),
            "generic unleased copy is unavailable"
        );
        assert!(!format!("{property:?}").contains(secret));
        assert!(!format!("{:?}", property.row()).contains(secret));
        let list = PropertyList::new(vec![property]);
        let node = list.accessibility().unwrap();
        assert!(node.children[0].state.protected);
        assert!(node.children[0].value.is_none());
        let harness = FrameHarness::for_root(State::new(), list, (640, 360), Theme::dark());
        assert!(!harness.inspection_json().unwrap().contains(secret));
        // Secret typography is still valid for public hashes; protection is explicit.
        let public = PropertyList::new(vec![
            Property::new("Hash", "abc123").with_kind(TextKind::Secret),
        ]);
        assert_eq!(
            public.accessibility().unwrap().children[0].value.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn property_secret_protection_survives_container_cache_replay() {
        let mut property =
            PropertyList::new(vec![Property::secret_value("Password", "shown-secret")]);
        property.rect = Rect::new(0.0, 0.0, 600.0, 100.0);
        let mut column = crate::widget::Column::new().push(Box::new(property));
        let mut text = TextRenderer::new();
        for _ in 0..2 {
            let frame = SecretPaint::begin();
            column.draw_with_presence(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                &PresenceCtx::empty(),
                &crate::permissions::PermissionContext::permissive(),
            );
            assert!(
                frame.finish(),
                "initial paint and cached replay both protect capture"
            );
            column.mark_drawn();
        }
    }

    #[test]
    fn property_actions_share_pointer_keyboard_and_semantic_callbacks() {
        let first = Rc::new(Cell::new(0));
        let second = Rc::new(Cell::new(0));
        let one = first.clone();
        let two = second.clone();
        let list = PropertyList::new(vec![
            Property::secret_value("Password", "revealed-value").action(
                Icon::Eye,
                "Hide password",
                move || one.set(one.get() + 1),
            ),
            Property::new("Identity", "ada@example.org").action(
                Icon::Copy,
                "Copy identity",
                move || two.set(two.get() + 1),
            ),
        ])
        .with_id("test-properties");
        let mut harness = FrameHarness::for_root(State::new(), list, (640, 360), Theme::dark());
        let point = harness.root::<PropertyList>().layout.rows[0]
            .action
            .unwrap()
            .center();
        harness.click_at(point.x, point.y);
        assert_eq!(first.get(), 1);
        harness.key("Enter");
        assert_eq!(
            first.get(),
            2,
            "host keyboard activation uses the same callback"
        );
        harness.key("ArrowDown");
        harness.key("Space");
        assert_eq!(second.get(), 1);
        harness
            .perform_semantic_named(
                "test-properties",
                Some("test-properties.action.0"),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        assert_eq!(first.get(), 3);
        let list = harness.root_mut::<PropertyList>();
        let target = list.action_target(1);
        assert!(list.perform_action_at(
            &crate::a11y::Action::Activate,
            &[1, crate::a11y::hash_key(&target)],
            &mut crate::automerge::AutoCommit::new(),
            &crate::automerge::ROOT,
        ));
        assert_eq!(
            second.get(),
            2,
            "native keyed assistive activation uses the callback"
        );
        let access = list.accessibility().unwrap();
        assert_eq!(
            access.children[0].children[0].name.as_deref(),
            Some("Hide password")
        );
        assert!(access.children[1].children[0].state.focused);
        assert!(access.children[0].children[0].bounds.is_some());
    }
}

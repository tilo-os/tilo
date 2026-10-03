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

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::a11y::{AccessNode, Role};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::Colors;
use crate::widget::{TaffyTreeExt, Widget};

/// One label and its value, as the face paints it.
#[derive(Clone, Copy, Debug)]
pub struct PropertyRow<'a> {
    pub label: &'a str,
    pub value: &'a str,
    /// Set the value in tabular figures (sizes, hashes, offsets, dates).
    pub numeric: bool,
    /// One trailing action for this value.
    pub action: Option<(Icon, &'a str)>,
}

impl<'a> PropertyRow<'a> {
    #[must_use]
    pub fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            numeric: false,
            action: None,
        }
    }

    #[must_use]
    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    #[must_use]
    pub fn action(mut self, icon: Icon, label: &'a str) -> Self {
        self.action = Some((icon, label));
        self
    }
}

/// The resolved geometry of a property list in `theme`.
#[derive(Clone, Debug)]
struct ListMetrics {
    label: TextStyle,
    stacked_label: TextStyle,
    value: TextStyle,
    value_numeric: TextStyle,
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
            value_numeric: theme.text(TextRole::Body).numeric(),
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

    fn value_style(&self, row: &PropertyRow<'_>) -> &TextStyle {
        if row.numeric {
            &self.value_numeric
        } else {
            &self.value
        }
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
        let value_h = f64::from(text.measure_style(row.value, Some(wrap), value_style).1);
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
                    .with_value(row.value)
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
        ctx.text(
            row.value,
            m.value_style(row),
            theme.text_primary,
            geo.value,
            Some(crate::num::f32_of_f64(geo.value_width.max(1.0))),
        );
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
    value: String,
    numeric: bool,
    action: Option<PropertyAction>,
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
            numeric: false,
            action: None,
        }
    }

    /// See [`PropertyRow::numeric`].
    #[must_use]
    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    /// A trailing "Copy" action that puts the value on the clipboard.
    #[must_use]
    pub fn copyable(mut self) -> Self {
        self.action = Some(PropertyAction::Copy);
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
        let mut row = PropertyRow::new(&self.label, &self.value);
        row.numeric = self.numeric;
        row.action = match &self.action {
            None => None,
            Some(PropertyAction::Copy) => Some((Icon::Copy, "Copy")),
            Some(PropertyAction::Custom { icon, label, .. }) => Some((*icon, label.as_str())),
        };
        row
    }
}

/// A retained property list. See the module docs.
pub struct PropertyList {
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

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let rows = self.rows();
        Some(
            AccessNode::new(Role::List).with_children(PropertyListFace::new(&rows).accessibility()),
        )
    }
}

impl PropertyList {
    fn run_action(&mut self, index: usize) {
        let Some(property) = self.properties.get_mut(index) else {
            return;
        };
        match &mut property.action {
            Some(PropertyAction::Copy) => crate::text_selection::clipboard_copy(&property.value),
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
            PropertyRow::new("Phone", "+44 20 7946 0958").numeric(),
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

//! `CurveEditor`: a tone curve a person shapes with their hand — the points that bend it, the
//! straight line a plain picture takes, and the curve drawn between them.
//!
//! A press puts a point where it landed, or grabs the one it landed on; a drag bends the curve
//! through it; letting go well outside the box takes an interior point off again, which is the
//! gesture that needs no second control per point. Both ends of the curve keep their x, because
//! a curve with no value at 0 or 1 has undefined ends.
//!
//! The line is drawn with the same monotone cubic Hermite the colour grade applies
//! (`crates/engines/video-edit/src/curve.rs`), so what a person shapes is what the picture
//! gets: the limiter there is what keeps a segment from overshooting its own ends, and the same
//! limiter is here.

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::ColorCompat;
use crate::a11y::{AccessNode, AccessState, Role};
use crate::component::{Event, Value};
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

/// How close to a point a press grabs it rather than adding another, in units of the box.
pub const POINT_GRAB: f64 = 0.055;

/// How far outside the box a point must be let go to come off the curve.
const DROP_MARGIN: f64 = 0.06;

/// Closer than this two points have no slope worth drawing between them, so an insert that
/// lands there is refused and grabs its neighbour instead.
pub const MIN_X_GAP: f64 = 1.0 / 64.0;

/// The points a curve is drawn through: x and y each in 0..1, sorted by x, both ends present.
#[must_use]
pub fn normalised(points: &[f64]) -> Vec<(f64, f64)> {
    let mut drawn: Vec<(f64, f64)> = points
        .chunks_exact(2)
        .filter(|pair| pair[0].is_finite() && pair[1].is_finite())
        .map(|pair| (pair[0].clamp(0.0, 1.0), pair[1].clamp(0.0, 1.0)))
        .collect();
    drawn.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if drawn.first().is_none_or(|point| point.0 > 1e-9) {
        drawn.insert(0, (0.0, 0.0));
    }
    if drawn.last().is_none_or(|point| point.0 < 1.0 - 1e-9) {
        drawn.push((1.0, 1.0));
    }
    // Points on top of each other are one point: the later one wins.
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(drawn.len());
    for point in drawn {
        match out.last_mut() {
            Some(previous) if (previous.0 - point.0).abs() < 1e-9 => *previous = point,
            _ => out.push(point),
        }
    }
    out
}

/// The value the curve takes at `x`, between the points `points`, as the grade applies it.
#[must_use]
pub fn value_at(points: &[(f64, f64)], x: f64) -> f64 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return x.clamp(0.0, 1.0);
    };
    if points.len() == 1 {
        return first.1;
    }
    let x = x.clamp(0.0, 1.0);
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    let i = points
        .iter()
        .rposition(|point| point.0 <= x)
        .unwrap_or(0)
        .min(points.len() - 2);
    let Some(&[a, b]) = points.get(i..i + 2) else {
        return last.1;
    };
    let width = b.0 - a.0;
    if width <= 1e-12 {
        return b.1;
    }
    let (ma, mb) = tangents(points, i);
    let t = (x - a.0) / width;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    (h00 * a.1 + h10 * width * ma + h01 * b.1 + h11 * width * mb).clamp(0.0, 1.0)
}

/// The Fritsch–Carlson tangents of segment `i`: the limiter is what makes the curve monotone
/// rather than merely smooth.
fn tangents(points: &[(f64, f64)], i: usize) -> (f64, f64) {
    let n = points.len();
    let slope = |k: usize| -> f64 {
        let Some(&[left, right]) = points.get(k..k + 2) else {
            return 0.0;
        };
        let width = right.0 - left.0;
        if width <= 1e-12 {
            0.0
        } else {
            (right.1 - left.1) / width
        }
    };
    let tangent = |k: usize| -> f64 {
        if k == 0 {
            return slope(0);
        }
        if k + 1 == n {
            return slope(n - 2);
        }
        let (before, after) = (slope(k - 1), slope(k));
        if before * after <= 0.0 {
            // A local extremum: a tangent here is an overshooting bulge.
            return 0.0;
        }
        let mean = (before + after) / 2.0;
        let limit = 3.0 * before.abs().min(after.abs());
        mean.clamp(-limit, limit)
    };
    let secant = slope(i);
    if secant.abs() <= 1e-12 {
        // A flat segment stays flat, whatever its neighbours want.
        return (0.0, 0.0);
    }
    let cap = 3.0 * secant.abs();
    (tangent(i).clamp(-cap, cap), tangent(i + 1).clamp(-cap, cap))
}

/// A tone curve a person shapes. See the module docs.
pub struct CurveEditor {
    points: Vec<(f64, f64)>,
    change: Event,
    label: Option<String>,
    height: f64,
    rect: Rect,
    pointer: Point,
    /// The point the hand holds, as its place among the points.
    drag: Option<usize>,
    /// The point the keys move, when the hand is not holding one.
    focused: Option<usize>,
    focused_widget: bool,
    dirty: bool,
    /// Its box stands as tall as it was made: a height given after it was laid out asks the solver
    /// for it again.
    layout_dirty: bool,
}

impl CurveEditor {
    /// An editor over `points` (x and y pairs, each 0..1), reported to `change` as the whole
    /// list.
    #[must_use]
    pub fn new(points: Vec<f64>, change: Event) -> Self {
        Self {
            points: normalised(&points),
            change,
            label: None,
            height: 160.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            drag: None,
            focused: None,
            focused_widget: false,
            dirty: true,
            layout_dirty: true,
        }
    }

    /// What the editor is, for assistive technology.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// How tall the box stands, in logical pixels.
    #[must_use]
    pub fn with_height(mut self, height: f64) -> Self {
        self.height = height.clamp(60.0, 600.0);
        self
    }

    /// The points it draws now.
    #[must_use]
    pub fn points(&self) -> &[(f64, f64)] {
        &self.points
    }

    /// Take the points the view hands it, unless the hand is holding one: the drag's own report
    /// is what the view writes back, and a round trip must not fight it.
    pub fn set_points(&mut self, points: Vec<f64>) {
        if self.drag.is_some() {
            return;
        }
        let drawn = normalised(&points);
        if drawn != self.points {
            self.points = drawn;
            self.dirty = true;
        }
    }

    /// Where the points reached go.
    pub fn set_change(&mut self, change: Event) {
        self.change = change;
    }

    /// What the curve is, for assistive technology.
    pub fn set_label(&mut self, label: Option<String>) {
        self.label = label;
    }

    /// How tall the box stands, in logical pixels.
    pub fn set_height(&mut self, height: f64) {
        let height = height.clamp(60.0, 600.0);
        if (self.height - height).abs() > f64::EPSILON {
            self.height = height;
            self.layout_dirty = true;
        }
    }

    /// Hand the points as they now stand to the view, and draw them again.
    fn report(&mut self) {
        let flat: Vec<Value> = self
            .points
            .iter()
            .flat_map(|(x, y)| [Value::Float(*x), Value::Float(*y)])
            .collect();
        self.dirty = true;
        self.change.fire(Some(Value::List(flat)));
    }

    /// Where a pointer at `point` stands on the box, as x and y each 0..1.
    fn coords(&self, point: Point) -> (f64, f64) {
        let width = self.rect.width().max(1e-6);
        let height = self.rect.height().max(1e-6);
        (
            ((point.x - self.rect.x0) / width).clamp(0.0, 1.0),
            (1.0 - (point.y - self.rect.y0) / height).clamp(0.0, 1.0),
        )
    }

    /// The point nearest to (x, y), within `tolerance` of the box each way.
    fn point_near(&self, x: f64, y: f64, tolerance: f64) -> Option<usize> {
        self.points
            .iter()
            .enumerate()
            .map(|(i, point)| (i, ((point.0 - x).powi(2) + (point.1 - y).powi(2)).sqrt()))
            .filter(|(_, away)| *away <= tolerance)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Press at `point`: grab the point it landed on, or add one and grab that. Too close to a
    /// neighbour for a point to mean anything, it grabs the neighbour rather than doing
    /// nothing.
    fn press(&mut self, point: Point) {
        let (x, y) = self.coords(point);
        if let Some(index) = self.point_near(x, y, POINT_GRAB) {
            self.hold(index);
        } else if let Some(index) = self.add(x, y) {
            self.hold(index);
        } else if let Some(index) = self.point_near(x, y, POINT_GRAB * 2.0) {
            self.hold(index);
        }
    }

    fn hold(&mut self, index: usize) {
        self.drag = Some(index);
        self.focused = Some(index);
        self.dirty = true;
    }

    /// Add a point at (x, y), answering its place, or nothing when the spot has no room.
    fn add(&mut self, x: f64, y: f64) -> Option<usize> {
        if self
            .points
            .iter()
            .any(|point| (point.0 - x).abs() < MIN_X_GAP)
        {
            return None;
        }
        let at = self
            .points
            .iter()
            .position(|point| point.0 > x)
            .unwrap_or(self.points.len());
        self.points.insert(at, (x, y));
        self.report();
        Some(at)
    }

    /// Move the held point to `point`: its ends keep their x, an interior one stays between
    /// its neighbours.
    fn move_to(&mut self, point: Point) {
        let Some(index) = self.drag else {
            return;
        };
        let (x, y) = self.coords(point);
        let count = self.points.len();
        if index >= count {
            return;
        }
        let moved = if index == 0 || index + 1 == count {
            (self.points[index].0, y)
        } else {
            let low = self.points[index - 1].0 + MIN_X_GAP;
            let high = self.points[index + 1].0 - MIN_X_GAP;
            let x = if low <= high {
                x.clamp(low, high)
            } else {
                self.points[index].0
            };
            (x, y)
        };
        if self.points[index] != moved {
            self.points[index] = moved;
            self.report();
        }
    }

    /// Let go: a point dragged well outside the box comes off the curve, ends excepted.
    fn release(&mut self) {
        let Some(index) = self.drag.take() else {
            return;
        };
        self.dirty = true;
        let out = self.pointer.x < self.rect.x0 - self.rect.width() * DROP_MARGIN
            || self.pointer.x > self.rect.x1 + self.rect.width() * DROP_MARGIN
            || self.pointer.y < self.rect.y0 - self.rect.height() * DROP_MARGIN
            || self.pointer.y > self.rect.y1 + self.rect.height() * DROP_MARGIN;
        if out && index > 0 && index + 1 < self.points.len() {
            self.points.remove(index);
            self.focused = None;
            self.report();
        }
    }

    /// Arrow keys move the focused point; Delete takes an interior one off.
    fn nudge(&mut self, key: NamedKey, large: bool) -> bool {
        let Some(index) = self.focused else {
            return false;
        };
        let count = self.points.len();
        if key == NamedKey::Delete || key == NamedKey::Backspace {
            if index > 0 && index + 1 < count {
                self.points.remove(index);
                self.focused = None;
                self.report();
                self.dirty = true;
                return true;
            }
            return false;
        }
        let step = if large { 0.05 } else { 0.01 };
        let Some(held) = self.points.get_mut(index) else {
            return false;
        };
        match key {
            NamedKey::ArrowLeft if index > 0 => held.0 = (held.0 - step).max(0.0),
            NamedKey::ArrowRight if index + 1 < count => held.0 = (held.0 + step).min(1.0),
            NamedKey::ArrowUp => held.1 = (held.1 + step).min(1.0),
            NamedKey::ArrowDown => held.1 = (held.1 - step).max(0.0),
            _ => return false,
        }
        self.dirty = true;
        self.report();
        true
    }

    /// The curve as a path over `rect`: a straight line between x 0 and 1, sampled so a bend
    /// between two points is drawn as the grade makes it.
    fn path(&self, rect: Rect) -> BezPath {
        let mut path = BezPath::new();
        let columns = 64;
        for column in 0..=columns {
            let x = f64::from(column) / f64::from(columns);
            let y = value_at(&self.points, x);
            let point = Point::new(rect.x0 + rect.width() * x, rect.y1 - rect.height() * y);
            if column == 0 {
                path.move_to(point);
            } else {
                path.line_to(point);
            }
        }
        path
    }

    fn point_at(&self, rect: Rect, (x, y): (f64, f64)) -> Point {
        Point::new(rect.x0 + rect.width() * x, rect.y1 - rect.height() * y)
    }

    fn paint(&self, scene: &mut vello::Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let rect = self.rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let ground = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &ground,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.surfaces.content_edge,
            None,
            &ground,
        );
        // The quarters, and the straight line a plain picture takes.
        let faint = colors.text_dim.with_alpha_factor(0.25);
        for quarter in 1..4 {
            let share = f64::from(quarter) / 4.0;
            let x = rect.x0 + rect.width() * share;
            let y = rect.y0 + rect.height() * share;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                faint,
                None,
                &vello::kurbo::Line::new(Point::new(x, rect.y0), Point::new(x, rect.y1)),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                faint,
                None,
                &vello::kurbo::Line::new(Point::new(rect.x0, y), Point::new(rect.x1, y)),
            );
        }
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            faint,
            None,
            &vello::kurbo::Line::new(Point::new(rect.x0, rect.y1), Point::new(rect.x1, rect.y0)),
        );
        let ink: Color = if self.focused_widget {
            colors.intent.accent_ink
        } else {
            colors.text_primary
        };
        scene.stroke(&Stroke::new(2.0), transform, ink, None, &self.path(rect));
        for (i, point) in self.points.iter().enumerate() {
            let held = self.drag == Some(i) || self.focused == Some(i);
            let radius = if held { 6.0 } else { 4.5 };
            let knob = Circle::new(self.point_at(rect, *point), radius);
            scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &knob);
            scene.stroke(&Stroke::new(2.0), transform, ink, None, &knob);
        }
    }
}

#[path = "curve_editor_component.rs"]
mod component;
pub use component::CurveEditorProps;

impl Widget for CurveEditor {
    fn name(&self) -> &'static str {
        "CurveEditor"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: taffy::style_helpers::percent(1.0_f32),
                height: taffy::style_helpers::length(crate::num::f32_of_f64(self.height)),
            },
            flex_shrink: 0.0,
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    /// Its box is the width it is given and the height it was made with, so it asks layout until
    /// it is placed and again when that height changed.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.paint(scene, transform, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.drag.is_some() {
                    self.move_to(self.pointer);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.pointer) => {
                self.focused_widget = true;
                *ctx.focus_target = Some(ctx.node_id);
                self.press(self.pointer);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.drag.is_some() => {
                self.release();
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.focused_widget =>
            {
                if let Key::Named(key) = event.logical_key
                    && self.nudge(key, false)
                {
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused_widget != focused {
            self.focused_widget = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused_widget
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Crosshair)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let percent = |value: f64| format!("{:.0}%", value * 100.0);
        let described: Vec<String> = self
            .points
            .iter()
            .map(|(x, y)| {
                crate::i18n::widget_format(
                    "widget-curve-point",
                    &[("across", percent(*x).into()), ("up", percent(*y).into())],
                )
            })
            .collect();
        Some(
            AccessNode::new(Role::Slider)
                .with_name(
                    self.label
                        .clone()
                        .unwrap_or_else(|| crate::i18n::widget_format("widget-curve-name", &[])),
                )
                .with_description(described.join("; "))
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.focused_widget),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Fired, Intent};
    use crate::testing::dispatch;

    use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};

    /// An editor over a 200 × 200 box at the origin, reporting each change to `queue`.
    fn editor_at(points: &[f64]) -> (CurveEditor, crate::Intents<Fired>) {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let change = Event::View(Intent::new(&queue, "curve", "change"));
        let mut editor = CurveEditor::new(points.to_vec(), change);
        editor.rect = Rect::new(0.0, 0.0, 200.0, 200.0);
        (editor, queue)
    }

    /// The points the last report carried, as pairs.
    fn reported(queue: &crate::Intents<Fired>) -> Vec<(f64, f64)> {
        let fired = queue.take();
        let Some(Fired {
            value: Some(Value::List(items)),
            ..
        }) = fired.last()
        else {
            return Vec::new();
        };
        let numbers: Vec<f64> = items
            .iter()
            .map(|item| match item {
                Value::Float(n) => *n,
                other => panic!("a point reads {other:?}"),
            })
            .collect();
        normalised(&numbers)
    }

    tilo_test_env::scenario! {
        /// The line the editor draws and the hands that shape it.
        fn the_curve_is_the_grades_and_a_hand_bends_it;

        phase the_straight_line_is_the_diagonal_and_a_point_bends_it;
        {
            let straight = normalised(&[]);
            assert_eq!(straight, vec![(0.0, 0.0), (1.0, 1.0)], "an empty curve is the diagonal");
            for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
                assert!((value_at(&straight, x) - x).abs() < 1e-9, "the identity at {x}");
            }
            let bent = normalised(&[0.0, 0.0, 0.5, 0.8, 1.0, 1.0]);
            assert!((value_at(&bent, 0.5) - 0.8).abs() < 1e-9, "it passes through its point");
            // Monotone between two points and never outside the box: the limiter is what stops
            // a steep segment from bulging past its own ends.
            let mut last = -1.0;
            for step in 0..=100 {
                let y = value_at(&bent, f64::from(step) / 100.0);
                assert!((0.0..=1.0).contains(&y), "the curve stays in the box at {step}");
                assert!(y >= last - 1e-9, "and never turns back at {step}");
                last = y;
            }
        }

        phase a_press_adds_a_point_and_a_drag_bends_it;
        {
            let (mut editor, queue) = editor_at(&[]);
            dispatch(&mut editor, &cursor_moved(150.0, 100.0));
            assert!(dispatch(&mut editor, &mouse_press()), "a press in the box is taken");
            let added = reported(&queue);
            assert_eq!(added.len(), 3, "a point lands where the press did: {added:?}");
            assert!((added[1].0 - 0.75).abs() < 1e-9, "three quarters across");
            assert!((added[1].1 - 0.5).abs() < 1e-9, "halfway up");
            dispatch(&mut editor, &cursor_moved(150.0, 40.0));
            let bent = reported(&queue);
            assert!((bent[1].1 - 0.8).abs() < 1e-9, "dragging up raises it: {bent:?}");
            assert!((bent[1].0 - 0.75).abs() < 1e-9, "across stays where the hand put it");
            dispatch(&mut editor, &mouse_release());
            assert_eq!(editor.points().len(), 3, "letting go inside keeps the point");
        }

        phase an_end_keeps_its_x_and_letting_go_outside_takes_a_point_off;
        {
            let (mut editor, queue) = editor_at(&[0.0, 0.0, 0.5, 0.5, 1.0, 1.0]);
            // The point at the left end moves up and down only. Its press is just inside the
            // box: a box holds what is up to its edges, not the far edge itself.
            dispatch(&mut editor, &cursor_moved(0.0, 190.0));
            dispatch(&mut editor, &mouse_press());
            dispatch(&mut editor, &cursor_moved(120.0, 60.0));
            let moved = reported(&queue);
            assert_eq!(moved.first(), Some(&(0.0, 0.7)), "the end keeps its x: {moved:?}");
            dispatch(&mut editor, &mouse_release());
            // The middle point dragged well outside comes off.
            dispatch(&mut editor, &cursor_moved(100.0, 100.0));
            dispatch(&mut editor, &mouse_press());
            assert_eq!(editor.points().len(), 3);
            dispatch(&mut editor, &cursor_moved(400.0, 100.0));
            dispatch(&mut editor, &mouse_release());
            assert_eq!(
                editor.points(),
                &[(0.0, 0.7), (1.0, 1.0)],
                "a point let go outside the box comes off, and the ends stay"
            );
            let left = reported(&queue);
            assert_eq!(left.len(), 2, "the drop is reported: {left:?}");
        }
    }
}

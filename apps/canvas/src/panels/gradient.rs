//! Editing a gradient fill: whether the fill is one colour or a gradient
//! (linear or radial), its stops on a bar that shows it (drag a stop to
//! move it, press the bar to add one, choose a stop to recolour it with the
//! swatches below), and a linear gradient's direction. Every change is one
//! `edit-marks` with the whole gradient, the action an agent uses too.

use std::collections::HashMap;
use std::rc::Rc;

use ui::LocalState;
use ui::a11y::{AccessNode, Role};
use ui::icons::Icon;
use ui::prelude::*;
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect, RoundedRect, Shape as _};
use ui::vello::peniko::{ColorStop, Extend, Fill, Gradient as Brush};
use ui::widget::{AppModel, EventCtx, ModelPath, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::{PresenceCtx, TextRenderer, Theme};

use super::icon_button;
use crate::model::{Gradient, Mark};
use crate::resource::ArtworkResource;

/// How a fill is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Solid,
    Linear,
    Radial,
}

impl Kind {
    pub fn of(mark: &Mark) -> Self {
        match mark.style.gradient.as_ref().map(|g| g.kind.as_str()) {
            Some("radial") => Self::Radial,
            Some(_) => Self::Linear,
            None => Self::Solid,
        }
    }
}

/// The direction of a linear gradient in degrees: 0 runs left to right,
/// 90 top to bottom.
pub fn angle(gradient: &Gradient) -> f64 {
    let dx = gradient.to[0] - gradient.from[0];
    let dy = gradient.to[1] - gradient.from[1];
    if dx.abs() < f64::EPSILON && dy.abs() < f64::EPSILON {
        return 0.0;
    }
    dy.atan2(dx).to_degrees().rem_euclid(360.0)
}

/// `stops` laid across `area` (the mark's own units): a linear gradient
/// through its centre at `degrees`, edge to edge; a radial one from its
/// centre to its farthest side.
pub fn across(kind: Kind, area: Rect, degrees: f64, stops: Vec<(f64, String)>) -> Gradient {
    let centre = area.center();
    let (from, to) = if kind == Kind::Radial {
        let reach = area.width().max(area.height()) / 2.0;
        (centre, Point::new(centre.x + reach, centre.y))
    } else {
        let (sin, cos) = degrees.to_radians().sin_cos();
        // Half the box's extent along the direction, so the ends touch
        // its edges.
        let half = f64::midpoint(area.width() * cos.abs(), area.height() * sin.abs());
        (
            Point::new(centre.x - cos * half, centre.y - sin * half),
            Point::new(centre.x + cos * half, centre.y + sin * half),
        )
    };
    Gradient {
        kind: if kind == Kind::Radial {
            "radial".to_owned()
        } else {
            "linear".to_owned()
        },
        from: [from.x, from.y],
        to: [to.x, to.y],
        stops,
        focal: None,
    }
}

/// The colour a gradient has at `at`, between its stops, as `#rrggbb`.
fn colour_at(stops: &[(f64, String)], at: f64) -> String {
    let parse = |text: &str| crate::render::colour(text, &[]);
    let mut sorted: Vec<&(f64, String)> = stops.iter().collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let below = sorted
        .iter()
        .rev()
        .find(|(t, _)| *t <= at)
        .or_else(|| sorted.first());
    let above = sorted
        .iter()
        .find(|(t, _)| *t >= at)
        .or_else(|| sorted.last());
    let (Some((t0, c0)), Some((t1, c1))) = (below, above) else {
        return "#808080".to_owned(); // a mid grey for a gradient with no stops: document colour
    };
    let (Some(a), Some(b)) = (parse(c0), parse(c1)) else {
        return c0.clone();
    };
    let mix = if t1 > t0 {
        ((at - t0) / (t1 - t0)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (a, b) = (a.to_rgba8(), b.to_rgba8());
    let lerp = |x: u8, y: u8| {
        let value = f64::from(x) + (f64::from(y) - f64::from(x)) * mix;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a channel between two u8 channels, rounded"
        )]
        let value = value.round().clamp(0.0, 255.0) as u8;
        value
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        lerp(a.r, b.r),
        lerp(a.g, b.g),
        lerp(a.b, b.b)
    )
}

/// The bar showing a gradient, with a grip for each stop.
pub struct GradientBar {
    rect: Rect,
    gradient: Gradient,
    /// The stop the swatches recolour.
    active: LocalState<usize>,
    /// The stop being dragged and where it is now, `0..=1`.
    dragging: Option<(usize, f64, bool)>,
    cursor: Point,
    commit: Rc<dyn Fn(Gradient)>,
    /// The strip's height, a small control's, and a grip's half side, from
    /// the theme the bar was laid out in.
    bar: f64,
    grip: f64,
}

impl GradientBar {
    pub fn new(
        gradient: Gradient,
        active: LocalState<usize>,
        commit: Rc<dyn Fn(Gradient)>,
    ) -> Self {
        Self {
            rect: Rect::ZERO,
            gradient,
            active,
            dragging: None,
            cursor: Point::ZERO,
            commit,
            bar: f64::from(ui::tokens::Metrics::BASE.bar_control_height),
            grip: f64::from(ui::tokens::Spacing::BASE.s2),
        }
    }

    /// The strip, inset by a grip on either side so the end grips stay
    /// inside the bar.
    fn strip(&self) -> Rect {
        Rect::new(
            self.rect.x0 + self.grip,
            self.rect.y0,
            self.rect.x1 - self.grip,
            self.rect.y0 + self.bar,
        )
    }

    fn at(&self, x: f64) -> f64 {
        let strip = self.strip();
        ((x - strip.x0) / strip.width().max(1.0)).clamp(0.0, 1.0)
    }

    fn stop_position(&self, index: usize) -> f64 {
        match self.dragging {
            Some((dragged, at, _)) if dragged == index => at,
            _ => self.gradient.stops.get(index).map_or(0.0, |stop| stop.0),
        }
    }

    fn grip(&self, index: usize) -> Point {
        let strip = self.strip();
        Point::new(
            strip.x0 + strip.width() * self.stop_position(index).clamp(0.0, 1.0),
            strip.y1,
        )
    }

    fn grip_at(&self, point: Point) -> Option<usize> {
        (0..self.gradient.stops.len())
            .rev()
            .find(|index| self.grip(*index).distance(point) <= self.grip * 1.5)
    }

    /// The gradient with its stops in order, and where `active` went.
    fn ordered(mut gradient: Gradient, active: usize) -> (Gradient, usize) {
        let mut indexed: Vec<(usize, (f64, String))> =
            gradient.stops.drain(..).enumerate().collect();
        indexed.sort_by(|a, b| a.1.0.total_cmp(&b.1.0));
        let moved = indexed
            .iter()
            .position(|(index, _)| *index == active)
            .unwrap_or(0);
        gradient.stops = indexed.into_iter().map(|(_, stop)| stop).collect();
        (gradient, moved)
    }
}

impl Widget for GradientBar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        self.bar = f64::from(theme.tokens.metrics.bar_control_height);
        self.grip = f64::from(theme.tokens.spacing.s2);
        // The strip, and below it the lower half of the grips on its edge.
        let height = theme.tokens.metrics.bar_control_height + theme.tokens.spacing.s2;
        let style = Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(height),
            },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let strip = self.strip();
        if strip.width() < 2.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.item);
        // A lifted well under the strip with its edge, so a stop that fades
        // out reads as see-through.
        let colors = &theme.tokens.colors;
        ui::surface::card(
            scene,
            transform,
            strip,
            radius,
            colors.lift_on(colors.surfaces.content, 0.5),
        );
        let mut stops: Vec<(f64, ui::vello::peniko::Color)> = self
            .gradient
            .stops
            .iter()
            .enumerate()
            .filter_map(|(index, stop)| {
                let colour = crate::render::colour(&stop.1, &[])?;
                Some((self.stop_position(index), colour))
            })
            .collect();
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        if stops.len() >= 2 {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a stop offset clamped to 0..=1"
            )]
            let colour_stops: Vec<ColorStop> = stops
                .iter()
                .map(|(at, colour)| ColorStop {
                    offset: at.clamp(0.0, 1.0) as f32,
                    color: (*colour).into(),
                })
                .collect();
            let brush = Brush::new_linear(
                Point::new(strip.x0, strip.center().y),
                Point::new(strip.x1, strip.center().y),
            )
            .with_extend(Extend::Pad)
            .with_stops(&colour_stops[..]);
            // The gradient itself: the artwork's own paint, inside the
            // well's edge.
            let inside = RoundedRect::from_rect(strip.inset(-1.0), (radius - 1.0).max(0.0));
            scene.fill(Fill::NonZero, transform, &brush, None, &inside);
        }
        // Each stop is a swatch of its colour on the strip's lower edge,
        // ringed in your colour while the swatches recolour it.
        let active = self.active.with(|active| *active);
        for (index, stop) in self.gradient.stops.iter().enumerate() {
            let at = self.grip(index);
            let colour = crate::render::colour(&stop.1, &[]).unwrap_or(colors.text_dim);
            ui::widget::ColorSwatchFace::new(
                Rect::from_center_size(at, (self.grip * 2.0, self.grip * 2.0)),
                colour,
                stop.1.clone(),
            )
            .selected(index == active)
            .paint(scene, transform, text_renderer, theme);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some((index, _, _)) = self.dragging {
                    let at = self.at(self.cursor.x);
                    self.dragging = Some((index, at, true));
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.cursor) => {
                if let Some(index) = self.grip_at(self.cursor) {
                    self.active.set(index);
                    self.dragging = Some((index, self.stop_position(index), false));
                } else if self.strip().inflate(0.0, 4.0).contains(self.cursor) {
                    // A new stop where the bar was pressed, in the colour
                    // the gradient has there.
                    let at = self.at(self.cursor.x);
                    let mut gradient = self.gradient.clone();
                    gradient.stops.push((at, colour_at(&gradient.stops, at)));
                    let added = gradient.stops.len().saturating_sub(1);
                    let (gradient, active) = Self::ordered(gradient, added);
                    self.active.set(active);
                    (self.commit)(gradient);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((index, at, moved)) = self.dragging.take() {
                    if moved && let Some(stop) = self.gradient.stops.get_mut(index) {
                        stop.0 = (at * 100.0).round() / 100.0;
                        let (gradient, active) = Self::ordered(self.gradient.clone(), index);
                        self.active.set(active);
                        (self.commit)(gradient);
                    }
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.dragging.is_some()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let stops = self
            .gradient
            .stops
            .iter()
            .map(|(at, colour)| format!("{colour} {:.0}%", at * 100.0))
            .collect::<Vec<_>>()
            .join(", ");
        Some(
            AccessNode::new(Role::Image)
                .with_name(ui::i18n::I18nManager::lookup_plain("canvas-gradient-stops"))
                .with_description(stops),
        )
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

    fn name(&self) -> &'static str {
        "GradientBar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The fill's kind, and for a gradient its bar, direction and stops.
pub fn editor(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    mark: &Mark,
    active: &LocalState<usize>,
) -> Element {
    let area = mark.shape.path().bounding_box();
    let gradient = mark.style.gradient.clone();
    let commit: Rc<dyn Fn(Gradient)> = Rc::new({
        let dispatch = cx.dispatcher();
        let targets = ids.to_vec();
        move |gradient: Gradient| {
            dispatch.submit(ArtworkResource::SET_GRADIENT.with((targets.clone(), gradient)));
        }
    });
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .push(kind_picker(cx, ids, mark, area, &commit));
    if let Some(gradient) = gradient {
        column = column
            .push(Box::new(GradientBar::new(
                gradient.clone(),
                active.clone(),
                Rc::clone(&commit),
            )))
            .push(direction_and_stops(
                cx,
                ids,
                Kind::of(mark),
                area,
                gradient,
                active,
            ));
    }
    Box::new(column)
}

/// Solid, linear or radial: a gradient starts from the fill fading to
/// white, and a solid fill takes the gradient's first colour.
fn kind_picker(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    mark: &Mark,
    area: Rect,
    commit: &Rc<dyn Fn(Gradient)>,
) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let kinds = [Kind::Solid, Kind::Linear, Kind::Radial];
    let kind = Kind::of(mark);
    // The brush's starting ink when a shape has no fill: a document colour.
    let fill = mark
        .style
        .fill
        .clone()
        .unwrap_or_else(|| "#2b3f8f".to_owned());
    let gradient = mark.style.gradient.clone();
    let degrees = gradient.as_ref().map_or(90.0, angle);
    let dispatch = cx.dispatcher();
    let targets = ids.to_vec();
    let commit = Rc::clone(commit);
    Box::new(
        ui::widget::ToggleGroup::new(
            vec![
                ui::widget::toggle_group::Segment::new(lookup("canvas-fill-solid")),
                ui::widget::toggle_group::Segment::new(lookup("canvas-fill-linear")),
                ui::widget::toggle_group::Segment::new(lookup("canvas-fill-radial")),
            ],
            kinds.iter().position(|k| *k == kind).unwrap_or(0),
        )
        .fill()
        .with_style(ui::widget::toggle_group::SegmentedStyle::Tabs)
        .with_id("canvas-fill-kind")
        .on_change(move |index| {
            let Some(chosen) = kinds.get(index).copied() else {
                return;
            };
            if chosen == Kind::Solid {
                let colour = gradient
                    .as_ref()
                    .and_then(|g| g.stops.first().map(|stop| stop.1.clone()))
                    .unwrap_or_else(|| fill.clone());
                dispatch.submit(ArtworkResource::SET_FILL.with((targets.clone(), colour)));
            } else {
                let stops = gradient.as_ref().map_or_else(
                    // A fill fading to white: document colours.
                    || vec![(0.0, fill.clone()), (1.0, "#ffffff".to_owned())],
                    |g| g.stops.clone(),
                );
                commit(across(chosen, area, degrees, stops));
            }
        }),
    )
}

/// A linear gradient's angle, and removing the chosen stop when there are
/// more than two.
fn direction_and_stops(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    kind: Kind,
    area: Rect,
    gradient: Gradient,
    active: &LocalState<usize>,
) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let mut row = Row::new()
        .no_wrap()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .with_align_items(taffy::AlignItems::Center);
    if kind == Kind::Linear {
        let turned = LocalState::new(angle(&gradient) / 360.0);
        let label = {
            let turned = turned.clone();
            Text::dynamic(move || format!("{:.0}°", turned.with(|t| t * 360.0)))
                .with_role(TextRole::Caption)
        };
        let stops = gradient.stops.clone();
        let slider = cx.actions().bind(
            Slider::from_state(&turned)
                .with_id("canvas-gradient-angle")
                .with_accessible_name(&lookup("canvas-gradient-angle"))
                .commits_on_release(),
            ArtworkResource::SET_GRADIENT.map({
                let targets = ids.to_vec();
                move |turn: f64| {
                    // Turned in steps of fifteen degrees.
                    let degrees = (turn * 24.0).round() * 15.0;
                    (
                        targets.clone(),
                        across(Kind::Linear, area, degrees, stops.clone()),
                    )
                }
            }),
        );
        row = row
            .push(Box::new(
                Text::new("canvas-gradient-angle").with_role(TextRole::Label),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(0.0)
                    .with_child(Box::new(slider)),
            ))
            .push(Box::new(label));
    } else {
        row = row.push(Box::new(Spacer::new()));
    }
    if gradient.stops.len() > 2 {
        let chosen = active
            .with(|a| *a)
            .min(gradient.stops.len().saturating_sub(1));
        let mut fewer = gradient;
        fewer.stops.remove(chosen);
        row = row.push(Box::new(icon_button(
            cx,
            Icon::Trash2,
            &lookup("canvas-gradient-remove-stop"),
            ArtworkResource::SET_GRADIENT.with((ids.to_vec(), fewer)),
        )));
    }
    Box::new(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_gradient_runs_edge_to_edge_at_its_angle() {
        let area = Rect::new(0.0, 0.0, 200.0, 100.0);
        let stops = vec![(0.0, "#000000".to_owned()), (1.0, "#ffffff".to_owned())];
        let across_x = across(Kind::Linear, area, 0.0, stops.clone());
        let near =
            |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9;
        assert!(near(across_x.from, [0.0, 50.0]), "{:?}", across_x.from);
        assert!(near(across_x.to, [200.0, 50.0]), "{:?}", across_x.to);
        let down = across(Kind::Linear, area, 90.0, stops.clone());
        assert!((down.from[1] - 0.0).abs() < 1e-9 && (down.to[1] - 100.0).abs() < 1e-9);
        assert!((angle(&down) - 90.0).abs() < 1e-9);
        let radial = across(Kind::Radial, area, 0.0, stops);
        assert_eq!(radial.kind, "radial");
        assert!(near(radial.from, [100.0, 50.0]), "{:?}", radial.from);
        assert!(near(radial.to, [200.0, 50.0]), "{:?}", radial.to);
    }

    #[test]
    fn a_stop_added_between_two_takes_the_colour_there() {
        let stops = vec![(0.0, "#000000".to_owned()), (1.0, "#ffffff".to_owned())];
        assert_eq!(colour_at(&stops, 0.5), "#808080");
        assert_eq!(colour_at(&stops, 0.0), "#000000");
    }
}

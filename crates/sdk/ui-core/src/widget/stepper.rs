//! `Stepper`: where a person is in a short sequence of steps (a setup
//! wizard, writing an image, an import).
//!
//! Each step is a station: a disc and its label, joined by a line. A done
//! step's disc is filled with the inverted fill and carries a check; the
//! current one is ringed in primary ink with its number and its label is
//! strong; later ones are ringed in tertiary ink. When the stations do not
//! fit the width, the stepper reads "Step 2 of 4" over the current step's
//! label instead; it never shows a bare "2/4".
//!
//! Before the first step starts (`current(None)`) every station waits. A
//! stepper with [`Stepper::on_station`] lets the person go back to a step
//! that is done: those stations take a pointer, focus and assistive
//! technology's activation.
//!
//! [`StepperFace`] paints the same geometry for immediate-mode callers, and
//! [`StepperFace::station_rects`] says where each station is.

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
use vello::peniko::Fill;

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Widget};

/// The resolved geometry of a stepper in `theme`.
#[derive(Clone, Debug)]
struct StepMetrics {
    label: TextStyle,
    current: TextStyle,
    number: TextStyle,
    position: TextStyle,
    disc: f64,
    gap: f64,
    min_gap: f64,
    rule: f64,
}

impl StepMetrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        Self {
            label: theme.text(TextRole::Label),
            current: theme.text(TextRole::Label).strong(),
            number: theme.text(TextRole::Caption).numeric().strong(),
            position: theme.text(TextRole::Caption),
            disc: f64::from(theme.tokens.metrics.icon_lg),
            gap: f64::from(spacing.s2),
            min_gap: f64::from(spacing.s6),
            rule: f64::from(theme.tokens.metrics.chrome_rule_px),
        }
    }

    fn line(text: &mut TextRenderer, style: &TextStyle) -> f64 {
        f64::from(text.measure_style("Ag", None, style).1)
    }
}

/// Immediate-mode stepper.
#[derive(Clone, Copy, Debug)]
pub struct StepperFace<'a> {
    steps: &'a [&'a str],
    /// `None` before the first step starts.
    current: Option<usize>,
    /// The station under the pointer, for a stepper that can go back.
    hovered: Option<usize>,
    /// The station with keyboard focus.
    focused: Option<usize>,
}

impl<'a> StepperFace<'a> {
    /// `current` is the index of the step in progress; steps before it are
    /// done. `current == steps.len()` means every step is done, `None` that
    /// none has started.
    #[must_use]
    pub fn new(steps: &'a [&'a str], current: impl Into<Option<usize>>) -> Self {
        Self {
            steps,
            current: current.into().map(|current| current.min(steps.len())),
            hovered: None,
            focused: None,
        }
    }

    #[must_use]
    pub fn hovered(mut self, station: Option<usize>) -> Self {
        self.hovered = station;
        self
    }

    /// The station with keyboard focus.
    #[must_use]
    pub fn focused(mut self, station: Option<usize>) -> Self {
        self.focused = station;
        self
    }

    fn is_done(&self, index: usize) -> bool {
        self.current.is_some_and(|current| index < current)
    }

    fn is_current(&self, index: usize) -> bool {
        self.current == Some(index)
    }

    /// Where each station (its disc and its label) is in `rect`; empty in
    /// the compact form, where no station shows.
    pub fn station_rects(&self, text: &mut TextRenderer, theme: &Theme, rect: Rect) -> Vec<Rect> {
        if self.steps.is_empty() || self.is_compact(text, theme, rect.width()) {
            return Vec::new();
        }
        let m = StepMetrics::new(theme);
        let (widths, join) = self.stations(text, &m, rect.width());
        let height = m.disc.max(StepMetrics::line(text, &m.current));
        let mut x = rect.x0;
        widths
            .iter()
            .map(|width| {
                let station = Rect::new(x, rect.y0, x + width, rect.y0 + height);
                x += width + join;
                station
            })
            .collect()
    }

    /// Each station's width and the join between stations at `width`.
    fn stations(&self, text: &mut TextRenderer, m: &StepMetrics, width: f64) -> (Vec<f64>, f64) {
        let widths: Vec<f64> = self
            .steps
            .iter()
            .map(|label| m.disc + m.gap + f64::from(text.measure_style(label, None, &m.current).0))
            .collect();
        let total: f64 = widths.iter().sum();
        let joins = crate::num::f64_of_usize(self.steps.len().saturating_sub(1));
        let join = if joins > 0.0 {
            ((width - total) / joins).max(m.min_gap)
        } else {
            0.0
        };
        (widths, join)
    }

    /// Whether the stations fit `width`, or the stepper shows its compact
    /// "Step n of m" form.
    pub fn is_compact(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> bool {
        let m = StepMetrics::new(theme);
        let needed: f64 = self
            .steps
            .iter()
            .map(|label| m.disc + m.gap + f64::from(text.measure_style(label, None, &m.current).0))
            .sum::<f64>()
            + crate::num::f64_of_usize(self.steps.len().saturating_sub(1)) * m.min_gap;
        needed > width
    }

    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let m = StepMetrics::new(theme);
        if self.is_compact(text, theme, width) {
            StepMetrics::line(text, &m.position) + StepMetrics::line(text, &m.current)
        } else {
            m.disc.max(StepMetrics::line(text, &m.current))
        }
    }

    /// The "Step n of m" line of the compact form ("4 steps" before the
    /// first one starts).
    #[must_use]
    pub fn position_text(&self) -> String {
        match self.current {
            Some(current) => {
                let n = current.min(self.steps.len().saturating_sub(1)) + 1;
                format!("Step {n} of {}", self.steps.len())
            }
            None => format!("{} steps", self.steps.len()),
        }
    }

    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if self.steps.is_empty() || rect.width() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let m = StepMetrics::new(theme);
        if self.is_compact(ctx.text, theme, rect.width()) {
            let position = self.position_text();
            ctx.text(&position, &m.position, theme.text_dim, rect.origin(), None);
            let label = self
                .steps
                .get(self.current.unwrap_or(0).min(self.steps.len() - 1))
                .copied()
                .unwrap_or_default();
            let label = ctx.text.elide_to_width_style(
                label,
                crate::num::f32_of_f64(rect.width()),
                &m.current,
            );
            let y = rect.y0 + StepMetrics::line(ctx.text, &m.position);
            ctx.text(
                &label,
                &m.current,
                theme.text_primary,
                Point::new(rect.x0, y),
                None,
            );
            return;
        }

        let colors = &theme.tokens.colors;
        let (widths, join) = self.stations(ctx.text, &m, rect.width());
        let cy = rect.y0 + m.disc.max(StepMetrics::line(ctx.text, &m.current)) / 2.0;
        let station_h = m.disc.max(StepMetrics::line(ctx.text, &m.current));
        let mut x = rect.x0;
        for (index, (label, width)) in self.steps.iter().zip(&widths).enumerate() {
            let centre = Point::new(x + m.disc / 2.0, cy);
            let radius = m.disc / 2.0;
            let done = self.is_done(index);
            let current = self.is_current(index);
            let station = Rect::new(x, rect.y0, x + width, rect.y0 + station_h)
                .inflate(m.gap / 2.0, m.gap / 2.0);
            if self.hovered == Some(index) {
                let radius = f64::from(theme.tokens.radii.detail);
                ctx.fill_round(
                    station,
                    radius,
                    colors.lift_on(colors.surfaces.content, 0.5),
                );
            }
            if self.focused == Some(index) {
                crate::widget::paint_focus_ring(
                    ctx.scene,
                    ctx.transform,
                    station,
                    f64::from(theme.tokens.radii.detail),
                    theme,
                );
            }
            if done {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    colors.intent.value_fill,
                    None,
                    &Circle::new(centre, radius),
                );
                let glyph = Rect::from_center_size(centre, (radius, radius));
                Icon::Check.draw(ctx.scene, ctx.transform, glyph, colors.surfaces.content);
            } else {
                let (ring, ink) = if current {
                    (theme.text_primary, theme.text_primary)
                } else {
                    (theme.text_tertiary, theme.text_tertiary)
                };
                ctx.scene.stroke(
                    &Stroke::new(m.rule * 1.5),
                    ctx.transform,
                    ring,
                    None,
                    &Circle::new(centre, radius - m.rule),
                );
                let number = (index + 1).to_string();
                let (w, h) = ctx.text.measure_style(&number, None, &m.number);
                ctx.text(
                    &number,
                    &m.number,
                    ink,
                    Point::new(centre.x - f64::from(w) / 2.0, centre.y - f64::from(h) / 2.0),
                    None,
                );
            }
            let style = if current { &m.current } else { &m.label };
            let ink = if current || done {
                theme.text_primary
            } else {
                theme.text_dim
            };
            let line = StepMetrics::line(ctx.text, style);
            ctx.text(
                label,
                style,
                ink,
                Point::new(x + m.disc + m.gap, cy - line / 2.0),
                None,
            );
            x += width;
            if index + 1 < self.steps.len() {
                let rule = Rect::new(
                    x + m.gap,
                    cy - m.rule / 2.0,
                    x + join - m.gap,
                    cy + m.rule / 2.0,
                );
                let ink = if done {
                    theme.text_dim
                } else {
                    crate::surface::edge_on(colors.surfaces.content)
                };
                ctx.scene
                    .fill(Fill::NonZero, ctx.transform, ink, None, &rule);
                x += join;
            }
        }
    }
}

/// A retained stepper. See the module docs.
pub struct Stepper {
    steps: Vec<String>,
    current: Option<usize>,
    theme: Option<Theme>,
    rect: Rect,
    dirty: bool,
    id: String,
    on_station: Option<Box<dyn FnMut(usize)>>,
    /// Where each station was painted, for the pointer.
    stations: Vec<Rect>,
    hovered: Option<usize>,
    focused: bool,
    focused_station: Option<usize>,
    last_cursor: Point,
}

impl Stepper {
    pub fn new(steps: Vec<String>) -> Self {
        Self {
            steps,
            current: Some(0),
            theme: None,
            rect: Rect::ZERO,
            dirty: true,
            id: "stepper".to_owned(),
            on_station: None,
            stations: Vec::new(),
            hovered: None,
            focused: false,
            focused_station: None,
            last_cursor: Point::ZERO,
        }
    }

    /// See [`StepperFace::new`]: an index, or `None` before the first step.
    #[must_use]
    pub fn current(mut self, index: impl Into<Option<usize>>) -> Self {
        self.current = index.into();
        self
    }

    pub fn set_current(&mut self, index: impl Into<Option<usize>>) {
        let index = index.into();
        if self.current != index {
            self.current = index;
            self.dirty = true;
        }
    }

    /// Name the stepper; its stations are `<id>.station.<n>`.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Let the person go back to a step that is done: `f` gets the
    /// station's index.
    #[must_use]
    pub fn on_station(mut self, f: impl FnMut(usize) + 'static) -> Self {
        self.on_station = Some(Box::new(f));
        self
    }

    /// Where each station is (empty in the compact form or before layout).
    pub fn station_rects(&self) -> &[Rect] {
        &self.stations
    }

    /// Whether the person can go to `index`: a step that is done, when the
    /// stepper has [`Self::on_station`].
    pub fn is_station_enabled(&self, index: usize) -> bool {
        self.on_station.is_some() && self.current.is_some_and(|current| index < current)
    }

    fn station_id(&self, index: usize) -> String {
        format!("{}.station.{index}", self.id)
    }

    fn enabled_stations(&self) -> Vec<usize> {
        (0..self.steps.len())
            .filter(|&index| self.is_station_enabled(index))
            .collect()
    }

    fn go_to(&mut self, index: usize) -> bool {
        if !self.is_station_enabled(index) {
            return false;
        }
        if let Some(f) = &mut self.on_station {
            f(index);
        }
        true
    }

    fn station_at(&self, point: Point) -> Option<usize> {
        self.stations
            .iter()
            .position(|rect| rect.contains(point))
            .filter(|&index| self.is_station_enabled(index))
    }

    fn with_face<R>(&self, f: impl FnOnce(&StepperFace<'_>) -> R) -> R {
        let steps: Vec<&str> = self.steps.iter().map(String::as_str).collect();
        f(&StepperFace::new(&steps, self.current)
            .hovered(self.hovered)
            .focused(self.focused.then_some(self.focused_station).flatten()))
    }
}

impl Widget for Stepper {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
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
        let Some(theme) = self.theme.clone() else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        let height = self.with_face(|face| face.height(text_renderer, &theme, f64::from(width)));
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
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
        if presence.should_cull(self.rect) {
            return;
        }
        let rect = self.rect;
        self.stations = self.with_face(|face| {
            let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
            face.paint(&mut ctx, rect);
            face.station_rects(ctx.text, theme, rect)
        });
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if self.on_station.is_none() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                let hovered = self.station_at(self.last_cursor);
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(index) = self.station_at(self.last_cursor)
                    && self.go_to(index)
                {
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                let enabled = self.enabled_stations();
                let at = self
                    .focused_station
                    .and_then(|s| enabled.iter().position(|&e| e == s));
                match &event.logical_key {
                    Key::Named(NamedKey::ArrowRight) => {
                        self.focused_station = at
                            .map_or(enabled.first(), |i| enabled.get(i + 1).or(enabled.get(i)))
                            .copied();
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.focused_station = at
                            .map_or(enabled.first(), |i| enabled.get(i.saturating_sub(1)))
                            .copied();
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if let Some(index) = self.focused_station
                            && self.go_to(index)
                        {
                            *ctx.handled = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.station_at(point)
            .map(|_| winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        !self.enabled_stations().is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        if focused != self.focused {
            self.focused = focused;
            if focused && self.focused_station.is_none() {
                self.focused_station = self.enabled_stations().last().copied();
            }
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(index) = (0..self.steps.len()).find(|&i| self.station_id(i) == target) else {
            return false;
        };
        self.go_to(index)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [index] => self.stations.get(*index).copied(),
            _ => None,
        }
    }

    crate::redraw_flag!();

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Stepper"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn test_id(&self) -> Option<&str> {
        self.on_station.as_ref().map(|_| self.id.as_str())
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let label = self
            .steps
            .get(
                self.current
                    .unwrap_or(0)
                    .min(self.steps.len().saturating_sub(1)),
            )
            .cloned()
            .unwrap_or_default();
        let position = self.with_face(|face| face.position_text());
        let mut node = AccessNode::new(Role::ProgressBar)
            .with_name(label)
            .with_value(position);
        if self.on_station.is_some() {
            node = node.with_children(
                self.enabled_stations()
                    .into_iter()
                    .map(|index| {
                        AccessNode::new(Role::Button)
                            .with_name(self.steps.get(index).cloned().unwrap_or_default())
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(self.focused && self.focused_station == Some(index)),
                            )
                            .with_action_target(self.station_id(index))
                    })
                    .collect(),
            );
        }
        Some(node)
    }
}

impl crate::widget::controls::ActionControls for Stepper {
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        self.enabled_stations()
            .into_iter()
            .map(|index| crate::widget::ControlBindingMetadata::semantic(self.station_id(index)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEPS: [&str; 4] = ["Choose image", "Choose drive", "Write", "Verify"];

    #[test]
    fn narrow_steppers_name_the_step_never_a_bare_number() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let face = StepperFace::new(&STEPS, 1);
        assert!(!face.is_compact(&mut text, &theme, 900.0));
        assert!(face.is_compact(&mut text, &theme, 260.0));
        assert_eq!(face.position_text(), "Step 2 of 4");
        assert!(
            face.height(&mut text, &theme, 260.0) > face.height(&mut text, &theme, 900.0),
            "the compact form stacks its two lines"
        );
        assert!(face.is_compact(&mut text, &Theme::dark().with_text_scale(2.0), 600.0));
        let done = StepperFace::new(&STEPS, 9);
        assert_eq!(done.position_text(), "Step 4 of 4");
    }

    #[test]
    fn done_steps_are_filled_and_checked() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let paint = |current: usize, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            StepperFace::new(&STEPS, current).paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, text, &theme),
                Rect::new(0.0, 0.0, 900.0, 40.0),
            );
            scene.encoding().resources.glyph_runs.len()
        };
        // A done step shows a check, not its number: two fewer number runs.
        assert_eq!(paint(0, &mut text), paint(2, &mut text) + 2);
        let stepper = Stepper::new(STEPS.iter().map(|s| (*s).to_owned()).collect()).current(2);
        let node = Widget::accessibility(&stepper).unwrap();
        assert_eq!(node.name.as_deref(), Some("Write"));
        assert_eq!(node.value.as_deref(), Some("Step 3 of 4"));
    }
}

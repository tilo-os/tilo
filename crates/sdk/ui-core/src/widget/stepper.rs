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
//! [`StepperFace`] paints the same geometry for immediate-mode callers.

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
    current: usize,
}

impl<'a> StepperFace<'a> {
    /// `current` is the index of the step in progress; steps before it are
    /// done. `current == steps.len()` means every step is done.
    #[must_use]
    pub fn new(steps: &'a [&'a str], current: usize) -> Self {
        Self {
            steps,
            current: current.min(steps.len()),
        }
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

    /// The height at `width`.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let m = StepMetrics::new(theme);
        if self.is_compact(text, theme, width) {
            StepMetrics::line(text, &m.position) + StepMetrics::line(text, &m.current)
        } else {
            m.disc.max(StepMetrics::line(text, &m.current))
        }
    }

    /// The "Step n of m" line of the compact form.
    #[must_use]
    pub fn position_text(&self) -> String {
        let n = self.current.min(self.steps.len().saturating_sub(1)) + 1;
        format!("Step {n} of {}", self.steps.len())
    }

    /// Paint into `rect`.
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
                .get(self.current.min(self.steps.len() - 1))
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
        let widths: Vec<f64> = self
            .steps
            .iter()
            .map(|label| {
                m.disc + m.gap + f64::from(ctx.text.measure_style(label, None, &m.current).0)
            })
            .collect();
        let total: f64 = widths.iter().sum();
        let joins = crate::num::f64_of_usize(self.steps.len().saturating_sub(1));
        let join = if joins > 0.0 {
            ((rect.width() - total) / joins).max(m.min_gap)
        } else {
            0.0
        };
        let cy = rect.y0 + m.disc.max(StepMetrics::line(ctx.text, &m.current)) / 2.0;
        let mut x = rect.x0;
        for (index, (label, width)) in self.steps.iter().zip(&widths).enumerate() {
            let centre = Point::new(x + m.disc / 2.0, cy);
            let radius = m.disc / 2.0;
            let done = index < self.current;
            let current = index == self.current;
            if done {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    colors.intent.inverted,
                    None,
                    &Circle::new(centre, radius),
                );
                let glyph = Rect::from_center_size(centre, (radius, radius));
                Icon::Check.draw(ctx.scene, ctx.transform, glyph, colors.intent.on_inverted);
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
    current: usize,
    theme: Option<Theme>,
    rect: Rect,
    dirty: bool,
}

impl Stepper {
    pub fn new(steps: Vec<String>) -> Self {
        Self {
            steps,
            current: 0,
            theme: None,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// See [`StepperFace::new`].
    #[must_use]
    pub fn current(mut self, index: usize) -> Self {
        self.current = index;
        self
    }

    pub fn set_current(&mut self, index: usize) {
        if self.current != index {
            self.current = index;
            self.dirty = true;
        }
    }

    fn with_face<R>(&self, f: impl FnOnce(&StepperFace<'_>) -> R) -> R {
        let steps: Vec<&str> = self.steps.iter().map(String::as_str).collect();
        f(&StepperFace::new(&steps, self.current))
    }
}

impl Widget for Stepper {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let node = taffy.leaf_node(Style {
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
        let l = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
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
        let rect = self.rect;
        self.with_face(|face| {
            face.paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
            );
        });
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
        "Stepper"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let label = self
            .steps
            .get(self.current.min(self.steps.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        let position = self.with_face(|face| face.position_text());
        Some(
            AccessNode::new(Role::ProgressBar)
                .with_name(label)
                .with_value(position),
        )
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
        // Larger text compacts sooner.
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

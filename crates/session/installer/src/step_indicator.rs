//! [`StepIndicator`] — a persistent numbered breadcrumb header.
//!
//! Renders the wizard's steps as a row of numbered circles joined by
//! connectors, with a caption under each circle. The active step is
//! filled with the accent colour and its caption lifts to `text_primary`;
//! completed steps stay accent-filled; upcoming steps render as dim
//! outlined circles. It is a *leaf* widget (no children, not focusable),
//! so it trivially mirrors the taffy tree for the App's focusable-order
//! traversal and never steals keyboard focus.
//!
//! All colours / sizing come from the [`Theme`] — nothing is hard-coded.

use std::collections::HashMap;
use ui::widget::TaffyTreeExt;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// Total header height (circle band + caption row).
const HEIGHT: f32 = 58.0;
/// Circle radius (logical px).
const RADIUS: f64 = 13.0;
/// Vertical centre of the circle band, measured from the widget top.
const CIRCLE_CY: f64 = 15.0;
/// Baseline-ish top of the caption row, measured from the widget top.
const LABEL_TOP: f64 = 33.0;
/// Number glyph size.
const NUMBER_SIZE: f32 = 13.0;
/// Caption glyph size.
const LABEL_SIZE: f32 = 11.0;
/// Below this width seven independent captions cannot remain legible. Use a
/// single current-step label and determinate progress bar instead.
const COMPACT_WIDTH: f64 = 400.0;

pub struct StepIndicator {
    steps: Vec<String>,
    current: usize,
    rect: Rect,
}

impl StepIndicator {
    pub fn new(steps: Vec<String>, current: usize) -> Self {
        Self {
            steps,
            current,
            rect: Rect::ZERO,
        }
    }

    /// Centre x of the `i`th step's circle within the current rect.
    fn column_center_x(&self, i: usize) -> f64 {
        let n = ui::num::f64_of_usize(self.steps.len().max(1));
        let w = self.rect.width();
        self.rect.x0 + (ui::num::f64_of_usize(i) + 0.5) * (w / n)
    }

    fn draw_compact(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let current = self.current.min(self.steps.len().saturating_sub(1));
        let summary = format!(
            "Step {} of {}  ·  {}",
            current.saturating_add(1),
            self.steps.len(),
            self.steps.get(current).map_or("Setup", String::as_str)
        );
        let (summary_width, _) = text_renderer.measure(&summary, NUMBER_SIZE, None);
        text_renderer.draw_text(
            scene,
            &summary,
            NUMBER_SIZE,
            theme.text_primary,
            transform.pre_translate(Vec2::new(
                self.rect.x0 + (self.rect.width() - f64::from(summary_width)) / 2.0,
                self.rect.y0 + 7.0,
            )),
            None,
        );

        let track = Rect::new(
            self.rect.x0,
            self.rect.y0 + 37.0,
            self.rect.x1,
            self.rect.y0 + 43.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(track, 3.0),
        );
        let fraction = ui::num::f64_of_usize(current.saturating_add(1))
            / ui::num::f64_of_usize(self.steps.len());
        let completed = Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * fraction,
            track.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.inverted,
            None,
            &RoundedRect::from_rect(completed, 3.0),
        );
    }
}

impl Widget for StepIndicator {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only errors for a corrupted tree; `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so surfacing a
        // theoretical error here would mean threading a `Result` through the
        // whole toolkit for a branch that can't be taken.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(HEIGHT),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let l = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(l, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.steps.is_empty() {
            return;
        }
        let cy = self.rect.y0 + CIRCLE_CY;
        let accent = theme.tokens.colors.intent.inverted;
        let upcoming_bg = theme.tokens.colors.surfaces.raised;

        if self.rect.width() < COMPACT_WIDTH {
            self.draw_compact(scene, transform, text_renderer, theme);
            return;
        }

        // 1. Connectors (drawn first, behind the circles). A segment is
        //    "done" if the step to its left is completed.
        for i in 0..self.steps.len().saturating_sub(1) {
            let x0 = self.column_center_x(i) + RADIUS;
            let x1 = self.column_center_x(i.saturating_add(1)) - RADIUS;
            let color = if i < self.current {
                accent
            } else {
                theme.border_standard
            };
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                color,
                None,
                &Line::new(Point::new(x0, cy), Point::new(x1, cy)),
            );
        }

        // 2. Circles + numbers + captions.
        for (i, label) in self.steps.iter().enumerate() {
            let cx = self.column_center_x(i);
            let circle = Circle::new(Point::new(cx, cy), RADIUS);
            let done_or_active = i <= self.current;
            if done_or_active {
                scene.fill(Fill::NonZero, transform, accent, None, &circle);
            } else {
                scene.fill(Fill::NonZero, transform, upcoming_bg, None, &circle);
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    theme.border_standard,
                    None,
                    &circle,
                );
            }
            // Active step gets an accent halo so it reads as "you are here".
            if i == self.current {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.border_focus,
                    None,
                    &Circle::new(Point::new(cx, cy), RADIUS + 3.0),
                );
            }

            // Step number, centred in the circle.
            let num = format!("{}", i.saturating_add(1));
            let num_color = if done_or_active {
                Color::WHITE
            } else {
                theme.text_dim
            };
            let (nw, nh) = text_renderer.measure(&num, NUMBER_SIZE, None);
            text_renderer.draw_text(
                scene,
                &num,
                NUMBER_SIZE,
                num_color,
                transform.pre_translate(Vec2::new(
                    cx - f64::from(nw) / 2.0,
                    cy - f64::from(nh) / 2.0,
                )),
                None,
            );

            // Caption under the circle, centred on the column.
            let label_color = if i == self.current {
                theme.text_primary
            } else {
                theme.text_dim
            };
            let (lw, _lh) = text_renderer.measure(label, LABEL_SIZE, None);
            text_renderer.draw_text(
                scene,
                label,
                LABEL_SIZE,
                label_color,
                transform.pre_translate(Vec2::new(
                    cx - f64::from(lw) / 2.0,
                    self.rect.y0 + LABEL_TOP,
                )),
                None,
            );
        }
    }

    fn name(&self) -> &'static str {
        "StepIndicator"
    }

    fn test_id(&self) -> Option<&str> {
        Some("step_indicator")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};

        let count = self.steps.len();
        let current = self.current.min(count.saturating_sub(1));
        let label = self.steps.get(current).map_or("Setup", String::as_str);
        Some(
            AccessNode::new(Role::ProgressBar)
                .with_name(format!("Installation setup: {label}"))
                .with_value(format!("step {} of {count}", current.saturating_add(1))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_semantics_name_the_current_step_and_position() {
        let indicator =
            StepIndicator::new(vec!["Keyboard".into(), "Network".into(), "Disk".into()], 1);
        let node = indicator.accessibility().expect("progress semantics");
        assert_eq!(node.role, ui::a11y::Role::ProgressBar);
        assert_eq!(node.name.as_deref(), Some("Installation setup: Network"));
        assert_eq!(node.value.as_deref(), Some("step 2 of 3"));
    }
}

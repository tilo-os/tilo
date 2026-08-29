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

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

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
        let n = self.steps.len().max(1) as f64;
        let w = self.rect.width();
        self.rect.x0 + (i as f64 + 0.5) * (w / n)
    }
}

impl Widget for StepIndicator {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(HEIGHT),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
        let accent = theme.accent_primary;
        let upcoming_bg = theme.bg_primary;

        // 1. Connectors (drawn first, behind the circles). A segment is
        //    "done" if the step to its left is completed.
        for i in 0..self.steps.len() - 1 {
            let x0 = self.column_center_x(i) + RADIUS;
            let x1 = self.column_center_x(i + 1) - RADIUS;
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
            let num = format!("{}", i + 1);
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
                transform * Affine::translate((cx - f64::from(nw) / 2.0, cy - f64::from(nh) / 2.0)),
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
                transform * Affine::translate((cx - f64::from(lw) / 2.0, self.rect.y0 + LABEL_TOP)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
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
}

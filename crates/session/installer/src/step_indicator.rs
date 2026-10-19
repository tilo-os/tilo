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

use taffy::{NodeId, TaffyTree};
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

    /// Captions must fit their own columns at the renderer's actual text scale.
    /// Reserve the themed gap for related items between neighbouring captions.
    fn needs_compact(&self, text_renderer: &mut TextRenderer, theme: &Theme) -> bool {
        let column_width = self.rect.width() / ui::num::f64_of_usize(self.steps.len().max(1));
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        column_width < 2.0 * (RADIUS + 3.0) + gap
            || self.steps.iter().any(|label| {
                let (width, _) = text_renderer.measure(label, LABEL_SIZE, None);
                f64::from(width) + gap > column_width
            })
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
            theme.tokens.colors.intent.accent,
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
        taffy.widget_leaf(crate::full_width_row(HEIGHT), node_map, self)
    }

    ui::rect_geometry!();

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
        let accent = theme.tokens.colors.intent.accent;
        let upcoming_bg = theme.tokens.colors.surfaces.raised;

        if self.needs_compact(text_renderer, theme) {
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

    fn wizard_indicator(width: f64) -> StepIndicator {
        let mut indicator = StepIndicator::new(
            crate::installer_app::STEP_LABELS
                .iter()
                .map(|label| (*label).to_owned())
                .collect(),
            3,
        );
        indicator.rect = Rect::new(0.0, 0.0, width, f64::from(HEIGHT));
        indicator
    }

    fn required_width(renderer: &mut TextRenderer, theme: &Theme) -> f64 {
        let indicator = wizard_indicator(0.0);
        let widest = indicator
            .steps
            .iter()
            .map(|label| renderer.measure(label, LABEL_SIZE, None).0)
            .fold(0.0_f32, f32::max);
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let column = (f64::from(widest) + gap).max(2.0 * (RADIUS + 3.0) + gap);
        column * ui::num::f64_of_usize(indicator.steps.len())
    }

    #[test]
    fn enlarged_captions_draw_existing_compact_progress_in_every_theme() {
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            ui::text_render::with_layout_renderer(2.0, |renderer| {
                let mut indicator = wizard_indicator(480.0);
                let mut actual = Scene::new();
                indicator.draw(
                    &mut actual,
                    Affine::IDENTITY,
                    renderer,
                    &theme,
                    &PresenceCtx::default(),
                );
                let mut compact = Scene::new();
                indicator.draw_compact(&mut compact, Affine::IDENTITY, renderer, &theme);
                assert_eq!(actual.encoding().path_data, compact.encoding().path_data);
                assert_eq!(actual.encoding().draw_data, compact.encoding().draw_data);
                assert!(actual.encoding().n_paths >= 2, "progress remains painted");
                assert_eq!(indicator.steps.len(), 9);
                assert_eq!(indicator.current, 3);
                let node = indicator.accessibility().expect("progress semantics");
                assert_eq!(node.name.as_deref(), Some("Installation setup: Disk"));
                assert_eq!(node.value.as_deref(), Some("step 4 of 9"));
            });
        }
    }

    #[test]
    fn caption_fit_tracks_measured_type_and_available_columns() {
        let theme = Theme::light();
        let normal =
            ui::text_render::with_layout_renderer(1.0, |renderer| required_width(renderer, &theme));
        let enlarged =
            ui::text_render::with_layout_renderer(1.5, |renderer| required_width(renderer, &theme));
        assert!(enlarged > normal, "larger captions require wider columns");
        // Between the two measured thresholds, all nine captions fit at 1×,
        // but accessibility-sized type must use compact progress.
        let width = f64::midpoint(normal, enlarged);
        for scale in [1.0, 1.5, 2.0] {
            ui::text_render::with_layout_renderer(scale, |renderer| {
                let mut indicator = wizard_indicator(width);
                assert_eq!(
                    indicator.needs_compact(renderer, &theme),
                    scale > 1.0,
                    "normal captions fit; enlarged captions need compact progress"
                );
                let required = required_width(renderer, &theme);
                indicator.rect.x1 = required - 1.0;
                assert!(indicator.needs_compact(renderer, &theme));
                indicator.rect.x1 = required + 1.0;
                assert!(!indicator.needs_compact(renderer, &theme));
                let mut full = Scene::new();
                indicator.draw(
                    &mut full,
                    Affine::IDENTITY,
                    renderer,
                    &theme,
                    &PresenceCtx::default(),
                );
                let mut compact = Scene::new();
                indicator.draw_compact(&mut compact, Affine::IDENTITY, renderer, &theme);
                assert!(full.encoding().n_paths > compact.encoding().n_paths);

                // A translated/renamed caption must select compact even at 1×.
                indicator.steps[0] = "Choose your keyboard layout and language".into();
                assert!(indicator.needs_compact(renderer, &theme));
            });
        }
    }

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

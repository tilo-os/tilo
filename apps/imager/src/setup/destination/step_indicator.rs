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
//! The circles, connectors and the compact progress track are the step
//! picture: they are drawn here, from the theme's radii, rule widths and
//! type roles, and never from a literal.

use std::collections::HashMap;
use ui::widget::TaffyTreeExt;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Vec2};
use vello::peniko::Color;

use ui::paint::{fill_round, stroke_line, stroke_round};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;

/// The step's number, set as one label inside its circle.
fn number_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Label)
}

/// The caption under each circle.
fn caption_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
}

/// A circle's radius: one line of the number it holds, so the number fits
/// when the text scale grows.
fn circle_radius(theme: &Theme) -> f64 {
    f64::from(number_style(theme).size)
}

/// Vertical centre of the circle band, measured from the widget top.
fn circle_center(theme: &Theme) -> f64 {
    circle_radius(theme) + f64::from(theme.tokens.spacing.rhythm.tight)
}

/// Top of the caption row, measured from the widget top.
fn caption_top(theme: &Theme) -> f64 {
    circle_center(theme) + circle_radius(theme) + f64::from(theme.tokens.spacing.rhythm.tight)
}

/// The header's height: the band plus one caption line and its bottom inset.
fn band_height(theme: &Theme) -> f32 {
    ui::num::f32_of_f64(
        caption_top(theme)
            + f64::from(caption_style(theme).line_height_px())
            + f64::from(theme.tokens.spacing.rhythm.related),
    )
}

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

    /// The `i`th circle's fill and number ink. Done and active steps take the
    /// accent with the theme's own label for it: the accent is light in high
    /// contrast and on a soft dark theme, where a white number vanishes.
    fn circle_face(&self, i: usize, theme: &Theme) -> (Color, Color) {
        let colors = &theme.tokens.colors;
        if i <= self.current {
            (colors.intent.accent, colors.intent.on_accent)
        } else {
            (colors.surfaces.raised, theme.text_dim)
        }
    }

    /// The square a circle of `radius` about `(cx, cy)` is painted as.
    fn disc(cx: f64, cy: f64, radius: f64) -> Rect {
        Rect::new(cx - radius, cy - radius, cx + radius, cy + radius)
    }

    /// Captions must fit their own columns at the renderer's actual text scale.
    /// Reserve the themed gap for related items between neighbouring captions.
    fn needs_compact(&self, text_renderer: &mut TextRenderer, theme: &Theme) -> bool {
        let column_width = self.rect.width() / ui::num::f64_of_usize(self.steps.len().max(1));
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let disc = 2.0 * (circle_radius(theme) + f64::from(theme.tokens.spacing.rhythm.tight));
        let style = caption_style(theme);
        column_width < disc + gap
            || self.steps.iter().any(|label| {
                let (width, _) = text_renderer.measure_style(label, None, &style);
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
            "Step {} of {}: {}",
            current.saturating_add(1),
            self.steps.len(),
            self.steps.get(current).map_or("Setup", String::as_str)
        );
        let style = number_style(theme);
        let (summary_width, _) = text_renderer.measure_style(&summary, None, &style);
        text_renderer.draw_style(
            scene,
            &summary,
            theme.text_primary,
            transform.pre_translate(Vec2::new(
                self.rect.x0 + (self.rect.width() - f64::from(summary_width)) / 2.0,
                self.rect.y0 + f64::from(theme.tokens.spacing.rhythm.related),
            )),
            None,
            &style,
        );

        // The progress track sits on the band's bottom edge, its ends and
        // thickness from the tokens.
        let track_h = f64::from(theme.tokens.metrics.meter_height);
        let inset = f64::from(theme.tokens.spacing.rhythm.related);
        let track = Rect::new(
            self.rect.x0,
            self.rect.y1 - inset - track_h,
            self.rect.x1,
            self.rect.y1 - inset,
        );
        let radius = f64::from(theme.tokens.radii.detail);
        fill_round(scene, transform, track, radius, theme.border_standard);
        let fraction = ui::num::f64_of_usize(current.saturating_add(1))
            / ui::num::f64_of_usize(self.steps.len());
        let completed = Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * fraction,
            track.y1,
        );
        fill_round(
            scene,
            transform,
            completed,
            radius,
            theme.tokens.colors.intent.accent,
        );
    }
}

impl Widget for StepIndicator {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            crate::setup::destination::full_width_row(band_height(theme)),
            node_map,
            self,
        )
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
        let radius = circle_radius(theme);
        let cy = self.rect.y0 + circle_center(theme);
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
        let accent = theme.tokens.colors.intent.accent;

        if self.needs_compact(text_renderer, theme) {
            self.draw_compact(scene, transform, text_renderer, theme);
            return;
        }

        // 1. Connectors (drawn first, behind the circles). A segment is
        //    "done" if the step to its left is completed.
        for i in 0..self.steps.len().saturating_sub(1) {
            let x0 = self.column_center_x(i) + radius;
            let x1 = self.column_center_x(i.saturating_add(1)) - radius;
            let color = if i < self.current {
                accent
            } else {
                theme.border_standard
            };
            stroke_line(
                scene,
                transform,
                Line::new(Point::new(x0, cy), Point::new(x1, cy)),
                color,
                2.0 * rule,
            );
        }

        // 2. Circles + numbers + captions.
        let number = number_style(theme);
        let caption = caption_style(theme);
        for (i, label) in self.steps.iter().enumerate() {
            let cx = self.column_center_x(i);
            let disc = Self::disc(cx, cy, radius);
            let (fill, num_color) = self.circle_face(i, theme);
            fill_round(scene, transform, disc, radius, fill);
            if i > self.current {
                stroke_round(
                    scene,
                    transform,
                    disc,
                    radius,
                    theme.border_standard,
                    1.5 * rule,
                );
            }
            // Active step gets an accent halo so it reads as "you are here".
            if i == self.current {
                let halo = radius + f64::from(theme.tokens.spacing.rhythm.tight);
                stroke_round(
                    scene,
                    transform,
                    Self::disc(cx, cy, halo),
                    halo,
                    theme.border_focus,
                    2.0 * rule,
                );
            }

            let num = format!("{}", i.saturating_add(1));
            let (nw, nh) = text_renderer.measure_style(&num, None, &number);
            text_renderer.draw_style(
                scene,
                &num,
                num_color,
                transform.pre_translate(Vec2::new(
                    cx - f64::from(nw) / 2.0,
                    cy - f64::from(nh) / 2.0,
                )),
                None,
                &number,
            );

            // Caption under the circle, centred on the column.
            let label_color = if i == self.current {
                theme.text_primary
            } else {
                theme.text_dim
            };
            let (lw, _lh) = text_renderer.measure_style(label, None, &caption);
            text_renderer.draw_style(
                scene,
                label,
                label_color,
                transform.pre_translate(Vec2::new(
                    cx - f64::from(lw) / 2.0,
                    self.rect.y0 + caption_top(theme),
                )),
                None,
                &caption,
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

    fn wizard_indicator(width: f64, theme: &Theme) -> StepIndicator {
        let mut indicator = StepIndicator::new(
            crate::setup::destination::installer_app::STEP_LABELS
                .iter()
                .map(|label| (*label).to_owned())
                .collect(),
            3,
        );
        indicator.rect = Rect::new(0.0, 0.0, width, f64::from(band_height(theme)));
        indicator
    }

    fn required_width(renderer: &mut TextRenderer, theme: &Theme) -> f64 {
        let indicator = wizard_indicator(0.0, theme);
        let style = caption_style(theme);
        let widest = indicator
            .steps
            .iter()
            .map(|label| renderer.measure_style(label, None, &style).0)
            .fold(0.0_f32, f32::max);
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let disc = 2.0 * (circle_radius(theme) + f64::from(theme.tokens.spacing.rhythm.tight));
        let column = (f64::from(widest) + gap).max(disc + gap);
        column * ui::num::f64_of_usize(indicator.steps.len())
    }

    #[test]
    fn enlarged_captions_draw_existing_compact_progress_in_every_theme() {
        for base in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = base.with_text_scale(2.0);
            ui::text_render::with_layout_renderer(1.0, |renderer| {
                let mut indicator = wizard_indicator(480.0, &theme);
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
    fn every_step_number_reads_on_its_circle_in_every_theme() {
        use ui::theme::Preset;
        use ui::tokens::colors::ON_FILL_MIN_CONTRAST;

        let indicator = wizard_indicator(480.0, &Theme::light());
        for preset in Preset::ALL {
            let theme = Theme::preset(preset);
            for i in 0..indicator.steps.len() {
                let (fill, ink) = indicator.circle_face(i, &theme);
                let ratio = ui::a11y::contrast_ratio(ink, fill);
                assert!(
                    ratio >= ON_FILL_MIN_CONTRAST,
                    "{}: step {} number reads at {ratio:.2}:1 on its circle",
                    preset.id(),
                    i.saturating_add(1)
                );
            }
        }
    }

    #[test]
    fn caption_fit_tracks_measured_type_and_available_columns() {
        // Captions are role text, so their size comes from the theme's text
        // scale; the renderer only measures it.
        let theme = Theme::light();
        let enlarged = theme.clone().with_text_scale(1.5);
        let normal =
            ui::text_render::with_layout_renderer(1.0, |renderer| required_width(renderer, &theme));
        let larger = ui::text_render::with_layout_renderer(1.0, |renderer| {
            required_width(renderer, &enlarged)
        });
        assert!(larger > normal, "larger captions require wider columns");
        // Between the two measured thresholds, all nine captions fit at 1×,
        // but accessibility-sized type must use compact progress.
        let width = f64::midpoint(normal, larger);
        for (scale, candidate) in [
            (1.0, theme.clone()),
            (1.5, enlarged),
            (2.0, theme.with_text_scale(2.0)),
        ] {
            ui::text_render::with_layout_renderer(1.0, |renderer| {
                let mut indicator = wizard_indicator(width, &candidate);
                assert_eq!(
                    indicator.needs_compact(renderer, &candidate),
                    scale > 1.0,
                    "normal captions fit; enlarged captions need compact progress"
                );
                let required = required_width(renderer, &candidate);
                indicator.rect.x1 = required - 1.0;
                assert!(indicator.needs_compact(renderer, &candidate));
                indicator.rect.x1 = required + 1.0;
                assert!(!indicator.needs_compact(renderer, &candidate));
                let mut full = Scene::new();
                indicator.draw(
                    &mut full,
                    Affine::IDENTITY,
                    renderer,
                    &candidate,
                    &PresenceCtx::default(),
                );
                let mut compact = Scene::new();
                indicator.draw_compact(&mut compact, Affine::IDENTITY, renderer, &candidate);
                assert!(full.encoding().n_paths > compact.encoding().n_paths);

                // A translated/renamed caption must select compact even at 1×.
                indicator.steps[0] = "Choose your keyboard layout and language".into();
                assert!(indicator.needs_compact(renderer, &candidate));
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

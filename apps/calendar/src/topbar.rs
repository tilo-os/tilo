//! Responsive calendar navigation bar.
//!
//! The component owns its geometry, responsive switcher placement, drawing,
//! and pointer hit map. `CalendarApp` keeps the navigation policy: it applies
//! the small [`Action`] values emitted here to the current view and anchor.

use chrono::NaiveDate;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::app::{center_text, draw_chevron, draw_hamburger, draw_icon_button, draw_plus};
use crate::draw_util::{fill_rect, fill_round, stroke_line, stroke_round};
use crate::{date, model::View};

pub(crate) const HEIGHT: f64 = 60.0;
const SEGMENT_WIDTH: f64 = 56.0;
const SEGMENTED_WIDTH: f64 = SEGMENT_WIDTH * 4.0;
const NEW_BUTTON_WIDTH: f64 = 60.0;
const MINIMUM_TITLE_WIDTH: f64 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeadingControl {
    None,
    OpenSidebar,
    ExpandSidebar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    View(View),
    Today,
    Previous,
    Next,
    NewEvent,
    OpenSidebar,
    ExpandSidebar,
}

pub(crate) struct Topbar {
    rect: Rect,
    view_rect: Rect,
    leading: LeadingControl,
    switcher_visible: bool,
    hits: Vec<(Rect, Action)>,
}

impl Topbar {
    pub(crate) fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            view_rect: Rect::ZERO,
            leading: LeadingControl::None,
            switcher_visible: false,
            hits: Vec::new(),
        }
    }

    /// Recompute the bar, the view below it, and switcher placement.
    pub(crate) fn layout(&mut self, content: Rect, leading: LeadingControl) {
        self.leading = leading;
        if content.area() <= 0.0 {
            self.rect = Rect::ZERO;
            self.view_rect = Rect::ZERO;
            self.switcher_visible = false;
            self.hits.clear();
            return;
        }
        self.rect = Rect::new(content.x0, content.y0, content.x1, content.y0 + HEIGHT);
        self.view_rect = Rect::new(content.x0, self.rect.y1, content.x1, content.y1);
        self.switcher_visible = switcher_fits(content.width(), leading);
    }

    pub(crate) fn switcher_visible(&self) -> bool {
        self.switcher_visible
    }

    pub(crate) fn view_rect(&self) -> Rect {
        self.view_rect
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        view: View,
        anchor: NaiveDate,
        pointer: Point,
    ) -> &[(Rect, Action)] {
        self.hits.clear();
        if self.rect.area() <= 0.0 {
            return &self.hits;
        }

        let colors = &theme.tokens.colors;
        fill_rect(scene, transform, colors.surfaces.base, self.rect);
        stroke_line(
            scene,
            transform,
            colors.border_standard,
            Point::new(self.rect.x0, self.rect.y1),
            Point::new(self.rect.x1, self.rect.y1),
            1.0,
        );

        let center_y = self.rect.center().y;
        let title_x = self.draw_navigation(scene, transform, text, theme, pointer, center_y);
        let title_right = self.draw_actions(scene, transform, text, theme, view, pointer, center_y);
        self.draw_title(
            scene,
            transform,
            text,
            theme,
            view,
            anchor,
            center_y,
            title_x,
            title_right,
        );
        &self.hits
    }

    fn draw_navigation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        pointer: Point,
        center_y: f64,
    ) -> f64 {
        let colors = &theme.tokens.colors;
        let mut left = self.rect.x0 + 16.0;
        match self.leading {
            LeadingControl::None => {}
            LeadingControl::OpenSidebar => {
                let button = Rect::new(left, center_y - 15.0, left + 30.0, center_y + 15.0);
                draw_icon_button(scene, transform, theme, button, button.contains(pointer));
                draw_hamburger(scene, transform, colors.neutral[8], button.center());
                self.hits.push((button, Action::OpenSidebar));
                left += 38.0;
            }
            LeadingControl::ExpandSidebar => {
                let button = Rect::new(left, center_y - 15.0, left + 30.0, center_y + 15.0);
                draw_icon_button(scene, transform, theme, button, button.contains(pointer));
                draw_chevron(scene, transform, colors.neutral[8], button.center(), false);
                self.hits.push((button, Action::ExpandSidebar));
                left += 38.0;
            }
        }

        let previous = Rect::new(left, center_y - 15.0, left + 30.0, center_y + 15.0);
        let next = Rect::new(left + 34.0, center_y - 15.0, left + 64.0, center_y + 15.0);
        draw_icon_button(
            scene,
            transform,
            theme,
            previous,
            previous.contains(pointer),
        );
        draw_icon_button(scene, transform, theme, next, next.contains(pointer));
        draw_chevron(scene, transform, colors.neutral[8], previous.center(), true);
        draw_chevron(scene, transform, colors.neutral[8], next.center(), false);
        self.hits.push((previous, Action::Previous));
        self.hits.push((next, Action::Next));
        left += 74.0;

        let today = Rect::new(left, center_y - 15.0, left + 60.0, center_y + 15.0);
        draw_icon_button(scene, transform, theme, today, today.contains(pointer));
        center_text(
            scene,
            text,
            "Today",
            13.0,
            colors.neutral[8],
            today,
            transform,
        );
        self.hits.push((today, Action::Today));
        today.x1 + 14.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_actions(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        view: View,
        pointer: Point,
        center_y: f64,
    ) -> f64 {
        let colors = &theme.tokens.colors;
        let segmented_x0 = if self.switcher_visible {
            let x1 = self.rect.x1 - 14.0;
            let x0 = x1 - SEGMENTED_WIDTH;
            let segmented = Rect::new(x0, center_y - 16.0, x1, center_y + 16.0);
            fill_round(scene, transform, colors.surfaces.raised, segmented, 9.0);
            stroke_round(
                scene,
                transform,
                colors.border_standard,
                segmented,
                9.0,
                1.0,
            );
            for (index, candidate) in View::ALL.iter().enumerate() {
                let cell = Rect::new(
                    x0 + SEGMENT_WIDTH * index as f64,
                    segmented.y0,
                    x0 + SEGMENT_WIDTH * (index as f64 + 1.0),
                    segmented.y1,
                );
                if view == *candidate {
                    fill_round(
                        scene,
                        transform,
                        colors.accent[5],
                        cell.inflate(-3.0, -3.0),
                        7.0,
                    );
                } else if cell.contains(pointer) {
                    fill_round(
                        scene,
                        transform,
                        colors.surface_white_5,
                        cell.inflate(-3.0, -3.0),
                        7.0,
                    );
                }
                center_text(
                    scene,
                    text,
                    candidate.label(),
                    13.0,
                    if view == *candidate {
                        Color::WHITE
                    } else {
                        colors.neutral[7]
                    },
                    cell,
                    transform,
                );
                self.hits.push((cell, Action::View(*candidate)));
            }
            x0
        } else {
            self.rect.x1 - 14.0
        };

        let new_event = Rect::new(
            segmented_x0 - NEW_BUTTON_WIDTH - 10.0,
            center_y - 16.0,
            segmented_x0 - 10.0,
            center_y + 16.0,
        );
        fill_round(scene, transform, colors.accent[5], new_event, 9.0);
        draw_plus(
            scene,
            transform,
            Color::WHITE,
            Point::new(new_event.x0 + 16.0, center_y),
        );
        text.draw_text(
            scene,
            "New",
            13.0,
            Color::WHITE,
            transform * Affine::translate((new_event.x0 + 28.0, center_y - 8.0)),
            None,
        );
        self.hits.push((new_event, Action::NewEvent));
        new_event.x0 - 12.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_title(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        view: View,
        anchor: NaiveDate,
        center_y: f64,
        title_x: f64,
        title_right: f64,
    ) {
        let available = (title_right - title_x).max(0.0);
        let variants = period_title_variants(view, anchor);
        let title = variants
            .iter()
            .find(|variant| f64::from(text.measure(variant, 19.0, None).0) <= available)
            .or_else(|| variants.last());
        let Some(title) = title else {
            return;
        };
        let clip = Rect::new(title_x - 2.0, self.rect.y0, title_right, self.rect.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        text.draw_text(
            scene,
            title,
            19.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((title_x, center_y - 11.0)),
            None,
        );
        scene.pop_layer();
    }
}

fn switcher_fits(content_width: f64, leading: LeadingControl) -> bool {
    if content_width <= 0.0 {
        return false;
    }
    let leading_width = if leading == LeadingControl::None {
        0.0
    } else {
        38.0
    };
    let title_x = 16.0 + leading_width + 74.0 + 60.0 + 14.0;
    let segmented_x0 = content_width - 14.0 - SEGMENTED_WIDTH;
    let new_event_x0 = segmented_x0 - NEW_BUTTON_WIDTH - 10.0;
    let title_right = new_event_x0 - 12.0;
    title_right - title_x >= MINIMUM_TITLE_WIDTH
}

fn period_title_variants(view: View, anchor: NaiveDate) -> Vec<String> {
    match view {
        View::Month => vec![date::month_title(anchor)],
        View::Week => {
            let week = date::week_days(anchor);
            let first = week.first().copied().unwrap_or(anchor);
            let last = week.last().copied().unwrap_or(anchor);
            vec![
                date::week_range_long(first, last),
                date::week_range_short(first, last),
            ]
        }
        View::Day => vec![date::fmt_day_long(anchor)],
        View::Agenda => vec!["Upcoming".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_owns_bar_and_view_geometry() {
        let mut bar = Topbar::new();
        let content = Rect::new(252.0, 10.0, 1280.0, 800.0);
        bar.layout(content, LeadingControl::None);
        assert_eq!(bar.view_rect(), Rect::new(252.0, 70.0, 1280.0, 800.0));
    }

    #[test]
    fn responsive_switcher_accounts_for_leading_control() {
        let mut bar = Topbar::new();
        let width = 590.0;
        bar.layout(Rect::new(0.0, 0.0, width, 700.0), LeadingControl::None);
        assert!(bar.switcher_visible());
        bar.layout(
            Rect::new(0.0, 0.0, width, 700.0),
            LeadingControl::OpenSidebar,
        );
        assert!(!bar.switcher_visible());
    }

    #[test]
    fn empty_content_clears_component_geometry() {
        let mut bar = Topbar::new();
        bar.layout(Rect::new(0.0, 0.0, 900.0, 700.0), LeadingControl::None);
        assert!(bar.switcher_visible());
        bar.layout(Rect::ZERO, LeadingControl::None);
        assert!(!bar.switcher_visible());
        assert_eq!(bar.view_rect(), Rect::ZERO);
    }

    #[test]
    fn week_title_keeps_both_endpoints_in_compact_variant() {
        let variants = period_title_variants(
            View::Week,
            NaiveDate::from_ymd_opt(2026, 8, 31).expect("valid test date"),
        );
        assert_eq!(variants.len(), 2);
        assert!(variants[1].contains('–'));
    }

    #[test]
    fn draw_builds_the_owned_hit_map_for_visible_controls() {
        let mut bar = Topbar::new();
        bar.layout(
            Rect::new(0.0, 0.0, 1024.0, 700.0),
            LeadingControl::OpenSidebar,
        );
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let actions: Vec<_> = bar
            .draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                View::Month,
                NaiveDate::from_ymd_opt(2026, 8, 31).expect("valid test date"),
                Point::ZERO,
            )
            .iter()
            .map(|(_, action)| *action)
            .collect();

        assert!(actions.contains(&Action::OpenSidebar));
        assert!(actions.contains(&Action::Today));
        assert!(actions.contains(&Action::Previous));
        assert!(actions.contains(&Action::Next));
        assert!(actions.contains(&Action::NewEvent));
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, Action::View(_)))
                .count(),
            View::ALL.len()
        );
    }
}

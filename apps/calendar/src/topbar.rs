//! Responsive calendar navigation bar.
//!
//! The component owns its geometry, responsive switcher placement, drawing,
//! and pointer hit map. `CalendarApp` keeps the navigation policy: it applies
//! the small [`Action`] values emitted here to the current view and anchor.

use chrono::{Datelike, NaiveDate};
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use crate::app::{center_text, draw_chevron, draw_hamburger, draw_icon_button, draw_plus};
use crate::draw_util::{fill_rect, fill_round, selected_row};
use crate::{date, model::View};
use ui::surface::card;

pub(crate) const HEIGHT: f64 = 60.0;
const LARGE_TEXT_HEIGHT: f64 = 76.0;
const LARGE_TEXT_SCALE: f32 = 1.35;
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
    text_scale: f32,
    hits: Vec<(Rect, Action)>,
}

impl Topbar {
    pub(crate) fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            view_rect: Rect::ZERO,
            leading: LeadingControl::None,
            switcher_visible: false,
            text_scale: 1.0,
            hits: Vec::new(),
        }
    }

    pub(crate) fn set_text_scale(&mut self, scale: f32) {
        self.text_scale = scale.clamp(0.5, 4.0);
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
        self.rect = Rect::new(
            content.x0,
            content.y0,
            content.x1,
            content.y0 + self.bar_height(),
        );
        self.view_rect = Rect::new(content.x0, self.rect.y1, content.x1, content.y1);
        self.switcher_visible = switcher_fits(content.width(), leading, self.text_scale);
    }

    fn compact(&self) -> bool {
        self.text_scale >= LARGE_TEXT_SCALE
    }

    fn bar_height(&self) -> f64 {
        if self.compact() {
            LARGE_TEXT_HEIGHT
        } else {
            HEIGHT
        }
    }

    pub(crate) fn switcher_visible(&self) -> bool {
        self.switcher_visible
    }

    pub(crate) fn view_rect(&self) -> Rect {
        self.view_rect
    }

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
        fill_rect(scene, transform, self.rect, colors.surfaces.base);
        // Bar role: one chrome hairline under the app bar (HIG §9.5).
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::chrome_hairline(theme),
            None,
            &Rect::new(self.rect.x0, self.rect.y1 - 1.0, self.rect.x1, self.rect.y1),
        );

        let center_y = self.rect.center().y;
        let title_x = self.draw_navigation(scene, transform, text, theme, pointer, center_y);
        let title_right = self.draw_actions(scene, transform, text, theme, view, pointer, center_y);
        self.draw_title(
            &mut PaintCtx::new(scene, transform, text, theme),
            &period_title_variants(view, anchor),
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

        let today_width = if self.compact() { 38.0 } else { 60.0 };
        let button_half_height = if self.compact() { 21.0 } else { 15.0 };
        let today = Rect::new(
            left,
            center_y - button_half_height,
            left + today_width,
            center_y + button_half_height,
        );
        draw_icon_button(scene, transform, theme, today, today.contains(pointer));
        if self.compact() {
            // The named, keyboard-focusable semantic control remains the
            // tooltip/accessibility affordance; the visual becomes a compact
            // target so 2x text never collides with the period title.
            center_text(scene, text, "•", 18.0, colors.neutral[8], today, transform);
        } else {
            center_text(
                scene,
                text,
                "Today",
                13.0,
                colors.neutral[8],
                today,
                transform,
            );
        }
        self.hits.push((today, Action::Today));
        today.x1 + 14.0
    }

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
        // Trailing inset mirrors the leading one in `draw_navigation` so the
        // bar's contents sit symmetrically inside the content column.
        let segmented_x0 = if self.switcher_visible {
            let x1 = self.rect.x1 - 16.0;
            let x0 = x1 - SEGMENTED_WIDTH;
            let segmented = Rect::new(x0, center_y - 16.0, x1, center_y + 16.0);
            let track_radius = f64::from(theme.tokens.radii.lg);
            // Segmented track (HIG §9.5): raised tier with its derived edge;
            // the chosen view is the selection lift, never an accent fill.
            card(
                scene,
                transform,
                segmented,
                track_radius,
                colors.surfaces.raised,
            );
            let seg_inset = f64::from(theme.tokens.spacing.s1) / 2.0 + 1.0;
            for (index, candidate) in View::ALL.iter().enumerate() {
                let cell = Rect::new(
                    x0 + SEGMENT_WIDTH * index as f64,
                    segmented.y0,
                    x0 + SEGMENT_WIDTH * (index as f64 + 1.0),
                    segmented.y1,
                );
                selected_row(
                    scene,
                    transform,
                    theme,
                    colors.surfaces.raised,
                    cell.inflate(-seg_inset, -seg_inset),
                    track_radius - seg_inset,
                    view == *candidate,
                    cell.contains(pointer),
                );
                center_text(
                    scene,
                    text,
                    candidate.label(),
                    13.0,
                    if view == *candidate {
                        colors.text_primary
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
            self.rect.x1 - 16.0
        };

        let new_width = if self.compact() {
            48.0
        } else {
            NEW_BUTTON_WIDTH * f64::from(self.text_scale)
        };
        let new_half_height = if self.compact() { 22.0 } else { 16.0 };
        let new_event = Rect::new(
            segmented_x0 - new_width - 10.0,
            center_y - new_half_height,
            segmented_x0 - 10.0,
            center_y + new_half_height,
        );
        fill_round(scene, transform, new_event, 9.0, colors.accent[5]);
        if self.compact() {
            draw_plus(scene, transform, colors.on_accent(4.5), new_event.center());
        } else {
            draw_plus(
                scene,
                transform,
                colors.on_accent(4.5),
                Point::new(new_event.x0 + 16.0, center_y),
            );
            text.draw_text(
                scene,
                "New",
                13.0,
                colors.on_accent(4.5),
                transform * Affine::translate((new_event.x0 + 28.0, center_y - 8.0)),
                None,
            );
        }
        self.hits.push((new_event, Action::NewEvent));
        new_event.x0 - 12.0
    }

    /// Paint the longest of the period title `variants` that fits between
    /// `title_x` and `title_right`, or the shortest clipped to that span.
    fn draw_title(
        &self,
        ctx: &mut PaintCtx<'_>,
        variants: &[String],
        title_x: f64,
        title_right: f64,
    ) {
        let (scene, transform, text) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text);
        let center_y = self.rect.center().y;
        let available = (title_right - title_x).max(0.0);
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
            ctx.theme.tokens.colors.text_primary,
            transform * Affine::translate((title_x, center_y - 11.0)),
            None,
        );
        scene.pop_layer();
    }
}

fn switcher_fits(content_width: f64, leading: LeadingControl, text_scale: f32) -> bool {
    if content_width <= 0.0 {
        return false;
    }
    // At high accessibility scales a four-way segmented control is both too
    // narrow to speak its labels and crowds the primary New action. The same
    // views remain available in the sidebar drawer.
    if text_scale >= LARGE_TEXT_SCALE {
        return false;
    }
    let leading_width = if leading == LeadingControl::None {
        0.0
    } else {
        38.0
    };
    let title_x = 16.0 + leading_width + 74.0 + 60.0 + 14.0;
    // Same trailing inset `draw_actions` paints, so the fit rule and the
    // painted switcher never disagree about where the segments start.
    let segmented_x0 = content_width - 16.0 - SEGMENTED_WIDTH;
    let new_event_x0 = segmented_x0 - NEW_BUTTON_WIDTH - 10.0;
    let title_right = new_event_x0 - 12.0;
    title_right - title_x >= MINIMUM_TITLE_WIDTH
}

fn period_title_variants(view: View, anchor: NaiveDate) -> Vec<String> {
    match view {
        View::Month => vec![
            date::month_title(anchor),
            date::month_title_short(anchor),
            anchor.year().to_string(),
        ],
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
    fn layout_owns_geometry_and_switcher_visibility() {
        // Case `layout_owns_bar_and_view_geometry`.
        {
            let mut bar = Topbar::new();
            let content = Rect::new(252.0, 10.0, 1280.0, 800.0);
            bar.layout(content, LeadingControl::None);
            assert_eq!(bar.view_rect(), Rect::new(252.0, 70.0, 1280.0, 800.0));
        }
        // Case `responsive_switcher_accounts_for_leading_control`.
        {
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
        // Case `empty_content_clears_component_geometry`.
        {
            let mut bar = Topbar::new();
            bar.layout(Rect::new(0.0, 0.0, 900.0, 700.0), LeadingControl::None);
            assert!(bar.switcher_visible());
            bar.layout(Rect::ZERO, LeadingControl::None);
            assert!(!bar.switcher_visible());
            assert_eq!(bar.view_rect(), Rect::ZERO);
        }
    }

    #[test]
    fn period_titles_offer_compact_variants() {
        // Case `week_title_keeps_both_endpoints_in_compact_variant`.
        {
            let variants = period_title_variants(
                View::Week,
                NaiveDate::from_ymd_opt(2026, 8, 31).expect("valid test date"),
            );
            assert_eq!(variants.len(), 2);
            assert!(variants[1].contains('–'));
        }
        // Case `month_title_has_compact_variants_before_it_can_clip_into_new`.
        {
            let anchor = NaiveDate::from_ymd_opt(2026, 9, 3).expect("valid test date");
            let variants = period_title_variants(View::Month, anchor);
            assert_eq!(variants, vec!["September 2026", "Sep 2026", "2026"]);

            let mut bar = Topbar::new();
            bar.layout(Rect::new(0.0, 0.0, 390.0, 844.0), LeadingControl::None);
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let theme = Theme::dark();
            let hits = bar.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                View::Month,
                anchor,
                Point::ZERO,
            );
            let new = hits
                .iter()
                .find(|(_, action)| *action == Action::NewEvent)
                .map(|(rect, _)| *rect)
                .expect("new event hit target");
            let title_right = new.x0 - 12.0;
            let title_x = 16.0 + 74.0 + 60.0 + 14.0;
            let chosen = variants
                .iter()
                .find(|title| f64::from(text.measure(title, 19.0, None).0) <= title_right - title_x)
                .expect("a compact month title fits before New");
            assert_eq!(chosen, "Sep 2026");
        }
    }

    #[test]
    fn draw_hit_map_covers_visible_controls_at_normal_and_large_text() {
        // Case `draw_builds_the_owned_hit_map_for_visible_controls`.
        {
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
        // Case `large_text_controls_are_bounded_and_non_overlapping_at_desktop_width`.
        {
            let content = Rect::new(0.0, 0.0, 1200.0, 900.0);
            let mut bar = Topbar::new();
            bar.set_text_scale(2.0);
            bar.layout(content, LeadingControl::OpenSidebar);
            assert!(
                !bar.switcher_visible(),
                "large-text view controls live in the drawer"
            );
            assert_eq!(bar.view_rect().y0, LARGE_TEXT_HEIGHT);

            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let theme = Theme::dark().with_text_scale(2.0);
            let hits = bar.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                View::Month,
                NaiveDate::from_ymd_opt(2026, 9, 3).expect("valid test date"),
                Point::ZERO,
            );
            assert!(
                hits.iter()
                    .any(|(_, action)| *action == Action::OpenSidebar)
            );
            assert!(hits.iter().any(|(_, action)| *action == Action::Today));
            assert!(hits.iter().any(|(_, action)| *action == Action::NewEvent));
            for (index, (rect, _)) in hits.iter().enumerate() {
                assert!(
                    content.contains_rect(*rect),
                    "control {index} escaped the viewport: {rect:?}"
                );
                for (other, _) in hits.iter().skip(index + 1) {
                    assert!(
                        rect.intersect(*other).area() <= 0.0,
                        "controls overlap: {rect:?} and {other:?}"
                    );
                }
            }
        }
    }
}

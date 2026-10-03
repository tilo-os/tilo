//! Responsive calendar header.
//!
//! The calendar's band of the window's one header row: the sidebar toggle
//! (while the sidebar is hidden), previous / next and Today, the period
//! title, the platform search button, the view switcher and New event. The component owns its geometry,
//! the responsive decisions (the New event label folds to its icon, then the
//! view switcher moves into the sidebar) and its pointer hit map;
//! `CalendarApp` applies the [`Action`] values emitted here.

use chrono::{Datelike, NaiveDate};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, SegmentedFace, SidebarDrawer, ToggleGroup,
};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use crate::sidebar::segment_rects;
use crate::{date, model::View};

const NEW_EVENT: &str = "New event";
const TODAY: &str = "Today";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    View(View),
    Today,
    Previous,
    Next,
    NewEvent,
    ShowSidebar,
    Search,
}

/// Where every control of the header landed, solved once per layout.
#[derive(Clone, Debug, Default)]
struct Geometry {
    toggle: Option<Rect>,
    previous: Rect,
    next: Rect,
    today: Rect,
    today_icon_only: bool,
    title_x0: f64,
    title_x1: f64,
    segments: Vec<Rect>,
    search: Rect,
    new_event: Rect,
    new_event_icon_only: bool,
}

pub(crate) struct Topbar {
    rect: Rect,
    view_rect: Rect,
    geometry: Geometry,
    hits: Vec<(Rect, Action)>,
}

impl Topbar {
    pub(crate) fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            view_rect: Rect::ZERO,
            geometry: Geometry::default(),
            hits: Vec::new(),
        }
    }

    /// Recompute the band, the view below it, and which controls fit.
    /// `show_toggle` puts the sidebar toggle at the leading edge (the sidebar
    /// is hidden); the title variants of `view` at `anchor` decide how much
    /// room the period title needs.
    pub(crate) fn layout(
        &mut self,
        content: Rect,
        show_toggle: bool,
        view: View,
        anchor: NaiveDate,
        theme: &Theme,
    ) {
        if content.area() <= 0.0 {
            self.rect = Rect::ZERO;
            self.view_rect = Rect::ZERO;
            self.geometry = Geometry::default();
            self.hits.clear();
            return;
        }
        let header = f64::from(theme.tokens.metrics.header_height);
        self.rect = Rect::new(content.x0, content.y0, content.x1, content.y0 + header);
        self.view_rect = Rect::new(content.x0, self.rect.y1, content.x1, content.y1);
        let variants = period_title_variants(view, anchor);
        self.geometry = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            solve(self.rect, show_toggle, &variants, text, theme)
        });
    }

    /// Whether the view switcher has room in the header; when not, the
    /// sidebar shows it.
    pub(crate) fn switcher_visible(&self) -> bool {
        !self.geometry.segments.is_empty()
    }

    pub(crate) fn view_rect(&self) -> Rect {
        self.view_rect
    }

    /// Paint the header for the period `view` shows at `anchor`, with the
    /// sidebar toggle drawn by `drawer_toggle`, and return its hit map.
    pub(crate) fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        (view, anchor): (View, NaiveDate),
        pointer: Point,
        drawer_toggle: &SidebarDrawer,
    ) -> &[(Rect, Action)] {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        self.hits.clear();
        if self.rect.area() <= 0.0 {
            return &self.hits;
        }
        // The band stands on the window ground; one hairline under it.
        ui::surface::separator(
            scene,
            transform,
            self.rect.x0,
            self.rect.x1,
            self.rect.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );

        self.draw_navigation(
            &mut PaintCtx::new(scene, transform, text, theme),
            view,
            pointer,
            drawer_toggle,
        );
        let g = &self.geometry;

        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(g.search, Icon::Search, "Search calendar")
                .hovered(g.search.contains(pointer)),
        );
        self.hits.push((g.search, Action::Search));

        if !g.segments.is_empty() {
            let labels: Vec<&str> = View::ALL.iter().map(|v| v.label()).collect();
            let selected = View::ALL.iter().position(|v| *v == view).unwrap_or(0);
            let hovered = g.segments.iter().position(|s| s.contains(pointer));
            ToggleGroup::paint(
                scene,
                transform,
                text,
                theme,
                &SegmentedFace::new(&g.segments, &labels, selected).hovered(hovered),
            );
            for (segment, candidate) in g.segments.iter().zip(View::ALL) {
                self.hits.push((*segment, Action::View(candidate)));
            }
        }

        let new_event = if g.new_event_icon_only {
            ButtonFace::icon_only(g.new_event, Icon::Plus, NEW_EVENT).role(ButtonRole::Primary)
        } else {
            ButtonFace::new(g.new_event, NEW_EVENT)
                .icon(Icon::Plus)
                .role(ButtonRole::Primary)
        };
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &new_event.hovered(g.new_event.contains(pointer)),
        );
        self.hits.push((g.new_event, Action::NewEvent));

        let (title_x0, title_x1) = (g.title_x0, g.title_x1);
        self.draw_title(
            &mut PaintCtx::new(scene, transform, text, theme),
            &period_title_variants(view, anchor),
            title_x0,
            title_x1,
        );
        &self.hits
    }

    /// The leading group: the sidebar toggle while the sidebar is hidden,
    /// previous / next, and Today.
    fn draw_navigation(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        view: View,
        pointer: Point,
        drawer_toggle: &SidebarDrawer,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let g = &self.geometry;
        if let Some(toggle) = g.toggle {
            drawer_toggle.draw_toggle(
                scene,
                transform,
                text,
                theme,
                toggle,
                toggle.contains(pointer),
            );
            self.hits.push((toggle, Action::ShowSidebar));
        }
        let (previous_name, next_name) = match view {
            View::Month => ("Previous month", "Next month"),
            View::Week | View::Agenda => ("Previous week", "Next week"),
            View::Day => ("Previous day", "Next day"),
        };
        for (rect, icon, name, action) in [
            (
                g.previous,
                Icon::ChevronLeft,
                previous_name,
                Action::Previous,
            ),
            (g.next, Icon::ChevronRight, next_name, Action::Next),
        ] {
            let face = ButtonFace::icon_only(rect, icon, name).hovered(rect.contains(pointer));
            Button::paint(scene, transform, text, theme, &face);
            self.hits.push((rect, action));
        }
        let today = if g.today_icon_only {
            ButtonFace::icon_only(g.today, Icon::Calendar, TODAY).role(ButtonRole::Secondary)
        } else {
            ButtonFace::new(g.today, TODAY)
        };
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &today.hovered(g.today.contains(pointer)),
        );
        self.hits.push((g.today, Action::Today));
    }

    /// Paint the longest of the period title `variants` that fits between
    /// `x0` and `x1`, or the shortest elided to that span.
    fn draw_title(&self, ctx: &mut PaintCtx<'_>, variants: &[String], x0: f64, x1: f64) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let style = theme.text(TextRole::Title);
        let available = (x1 - x0).max(0.0);
        let Some(title) = variants
            .iter()
            .find(|variant| f64::from(text.measure_style(variant, None, &style).0) <= available)
            .or_else(|| variants.last())
        else {
            return;
        };
        let shown = text.elide_to_width_style(title, ui::num::f32_of_f64(available), &style);
        let (_, line) = text.measure_style(&shown, None, &style);
        let top =
            ui::text_metrics::centered_text_top(self.rect.y0, self.rect.height(), f64::from(line));
        let clip = Rect::new(x0, self.rect.y0, x1, self.rect.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        text.draw_style(
            scene,
            &shown,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x0, top)),
            None,
            &style,
        );
        scene.pop_layer();
    }
}

/// Lay the header controls out in `band`, folding the New event label, then
/// moving the view switcher out, then folding Today, until the shortest title
/// variant fits between the two groups.
fn solve(
    band: Rect,
    show_toggle: bool,
    variants: &[String],
    text: &mut TextRenderer,
    theme: &Theme,
) -> Geometry {
    let sp = &theme.tokens.spacing;
    let (edge, related, sibling) = (f64::from(sp.s3), f64::from(sp.s2), f64::from(sp.s4));
    let height = ControlSize::Regular.height(theme);
    let mid = band.center().y;
    let control =
        |x0: f64, width: f64| Rect::new(x0, mid - height / 2.0, x0 + width, mid + height / 2.0);
    let square = height;
    // Chrome folds away before the title loses its month: the room it asks
    // for is its compact variant ("Jun 2026"), not the bare year.
    let title_need = variants
        .get(1)
        .or_else(|| variants.last())
        .map_or(0.0, |variant| {
            f64::from(
                text.measure_style(variant, None, &theme.text(TextRole::Title))
                    .0,
            )
        })
        .min(band.width());

    let today_label_w = ButtonFace::new(Rect::ZERO, TODAY).intrinsic_width(text, theme);
    let new_label_w = ButtonFace::new(Rect::ZERO, NEW_EVENT)
        .icon(Icon::Plus)
        .intrinsic_width(text, theme);
    let labels: Vec<&str> = View::ALL.iter().map(|v| v.label()).collect();
    let switcher_w = SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, text, theme);

    // (new event icon-only, switcher shown, today icon-only), most to least
    // generous.
    let steps = [
        (false, true, false),
        (true, true, false),
        (false, false, false),
        (true, false, false),
        (true, false, true),
    ];
    let mut chosen = (true, false, true);
    for step in steps {
        let (new_icon, switcher, today_icon) = step;
        let leading = edge
            + if show_toggle { square + related } else { 0.0 }
            + 2.0 * square
            + related
            + if today_icon { square } else { today_label_w }
            + sibling;
        let trailing = edge
            + if new_icon { square } else { new_label_w }
            + if switcher { switcher_w + sibling } else { 0.0 }
            + square
            + sibling
            + sibling;
        if band.width() - leading - trailing >= title_need {
            chosen = step;
            break;
        }
    }
    let (new_icon, switcher, today_icon) = chosen;

    let mut x = band.x0 + edge;
    let toggle = show_toggle.then(|| {
        let rect = control(x, square);
        x = rect.x1 + related;
        rect
    });
    let previous = control(x, square);
    let next = control(previous.x1, square);
    let today = control(
        next.x1 + related,
        if today_icon { square } else { today_label_w },
    );
    let title_x0 = today.x1 + sibling;

    let new_w = if new_icon { square } else { new_label_w };
    let new_event = control(band.x1 - edge - new_w, new_w);
    let mut x1 = new_event.x0 - sibling;
    let segments = if switcher {
        let strip = Rect::new(
            x1 - switcher_w,
            mid - SegmentedFace::height(ControlSize::Regular, theme) / 2.0,
            x1,
            mid + SegmentedFace::height(ControlSize::Regular, theme) / 2.0,
        );
        x1 = strip.x0 - sibling;
        segment_rects(strip, View::ALL.len())
    } else {
        Vec::new()
    };
    let search = control(x1 - square, square);
    let title_x1 = search.x0 - sibling;

    Geometry {
        toggle,
        previous,
        next,
        today,
        today_icon_only: today_icon,
        title_x0,
        title_x1: title_x1.max(title_x0),
        segments,
        search,
        new_event,
        new_event_icon_only: new_icon,
    }
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
        View::Day => vec![date::fmt_day_long(anchor), date::fmt_day_short(anchor)],
        View::Agenda => vec!["Upcoming".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::Scene;

    fn anchor() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 3).expect("valid test date")
    }

    fn actions(bar: &mut Topbar, theme: &Theme) -> Vec<(Rect, Action)> {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        bar.draw(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, theme),
            (View::Month, anchor()),
            Point::ZERO,
            &SidebarDrawer::new(),
        )
        .to_vec()
    }

    #[test]
    fn layout_owns_geometry_and_switcher_placement() {
        let theme = Theme::dark();
        let header = f64::from(theme.tokens.metrics.header_height);
        // Case `layout_owns_bar_and_view_geometry`.
        {
            let mut bar = Topbar::new();
            let content = Rect::new(280.0, 0.0, 1280.0, 800.0);
            bar.layout(content, false, View::Month, anchor(), &theme);
            assert_eq!(bar.view_rect(), Rect::new(280.0, header, 1280.0, 800.0));
            assert!(bar.switcher_visible());
        }
        // Case `a_phone_width_moves_the_switcher_into_the_sidebar`.
        {
            let mut bar = Topbar::new();
            bar.layout(
                Rect::new(0.0, 0.0, 390.0, 844.0),
                true,
                View::Month,
                anchor(),
                &theme,
            );
            assert!(!bar.switcher_visible());
        }
        // Case `empty_content_clears_component_geometry`.
        {
            let mut bar = Topbar::new();
            bar.layout(
                Rect::new(0.0, 0.0, 900.0, 700.0),
                false,
                View::Month,
                anchor(),
                &theme,
            );
            bar.layout(Rect::ZERO, false, View::Month, anchor(), &theme);
            assert!(!bar.switcher_visible());
            assert_eq!(bar.view_rect(), Rect::ZERO);
        }
    }

    #[test]
    fn period_titles_offer_compact_variants() {
        let variants = period_title_variants(
            View::Week,
            NaiveDate::from_ymd_opt(2026, 8, 31).expect("valid test date"),
        );
        assert_eq!(variants.len(), 2);
        assert!(variants[1].contains('–'));
        let variants = period_title_variants(View::Month, anchor());
        assert_eq!(variants, vec!["September 2026", "Sep 2026", "2026"]);
    }

    #[test]
    fn hit_map_covers_visible_controls_without_overlap() {
        for (theme, width) in [
            (Theme::dark(), 1024.0),
            (Theme::dark(), 800.0),
            (Theme::dark(), 390.0),
            (Theme::dark().with_text_scale(2.0), 1200.0),
        ] {
            let content = Rect::new(0.0, 0.0, width, 700.0);
            let mut bar = Topbar::new();
            bar.layout(content, true, View::Month, anchor(), &theme);
            let hits = actions(&mut bar, &theme);
            for action in [
                Action::Search,
                Action::ShowSidebar,
                Action::Today,
                Action::Previous,
                Action::Next,
                Action::NewEvent,
            ] {
                assert!(
                    hits.iter().any(|(_, a)| *a == action),
                    "{action:?} missing at {width}"
                );
            }
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

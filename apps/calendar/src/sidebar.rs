//! Responsive sidebar chrome.
//!
//! Presentation state and geometry come from the shared
//! [`ui::widget::SidebarDrawer`]: docked beside the calendar when the window
//! is wide, a floating drawer over it when narrow, and the person's show/hide
//! choice per size class. The drawer paints the panel, the scrim and the
//! sidebar toggle; this module adds the header row's overflow menu button and
//! (when the top bar has no room for it) the view switcher. The navigation
//! rows themselves are painted by `CalendarApp`, which owns the model.

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, ControlSize, SegmentedFace, SidebarDrawer, SplitLayout, ToggleGroup,
};
use vello::kurbo::{Point, Rect};

use crate::model::View;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Persistent,
    Overlay,
}

impl Mode {
    fn from_layout(layout: SplitLayout) -> Self {
        match layout {
            SplitLayout::Docked => Mode::Persistent,
            SplitLayout::Overlay => Mode::Overlay,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Dismiss,
    /// A press on the floating panel itself, between its controls.
    Guard,
    Toggle,
    Menu,
    View(View),
}

/// What the chrome pass painted: where the navigation rows start, the theme
/// to paint them with (the sidebar region's, or the floating drawer's), and
/// the hit rects of the header controls.
pub(crate) struct Chrome {
    pub(crate) body_y: f64,
    pub(crate) theme: Theme,
    pub(crate) hits: Vec<(Rect, Action)>,
}

/// The window width below which the sidebar floats as a drawer: the docked
/// sidebar plus a calendar two columns wide (an overview and a detail), so a
/// month grid is never squeezed beside it.
fn dock_breakpoint() -> f64 {
    let m = ui::tokens::Metrics::BASE;
    f64::from(m.sidebar_width + m.overview_width + m.detail_min_width)
}

pub(crate) struct Sidebar {
    drawer: SidebarDrawer,
    /// Open once the next layout has decided docked or drawer, so the
    /// choice lands on the size class the window actually has.
    open_after_layout: bool,
}

impl Sidebar {
    pub(crate) fn new() -> Self {
        Self {
            drawer: SidebarDrawer::new().left().breakpoint(dock_breakpoint()),
            open_after_layout: false,
        }
    }

    /// Recompute the responsive drawer for `window` in `theme` and return the
    /// x coordinate at which the calendar content begins.
    pub(crate) fn layout(&mut self, window: Rect, theme: &Theme) -> f64 {
        self.drawer.scale_for(theme);
        self.drawer.resolve(window);
        if std::mem::take(&mut self.open_after_layout) {
            self.drawer.open();
        }
        // Calendar chrome toggles snap; never tick the eased reveal here so
        // layout stays deterministic for tests and `--shots`.
        let open = self.drawer.is_open();
        self.drawer.snap(open);
        self.drawer.resolve(window).x0
    }

    pub(crate) fn mode(&self) -> Mode {
        Mode::from_layout(self.drawer.layout())
    }

    pub(crate) fn rect(&self) -> Rect {
        self.drawer.sidebar_rect()
    }

    /// The shared drawer, for the header that draws its toggle while the
    /// sidebar is hidden.
    pub(crate) fn drawer(&self) -> &SidebarDrawer {
        &self.drawer
    }

    /// Overlay-drawer open flag (unchecked while docked).
    pub(crate) fn is_open(&self) -> bool {
        self.mode() == Mode::Overlay && self.drawer.is_open()
    }

    pub(crate) fn set_open(&mut self, open: bool) {
        if open {
            self.drawer.open();
        } else {
            self.drawer.close();
        }
        self.drawer.snap(open);
    }

    /// Docked sidebar hidden flag (false while overlay).
    #[cfg(test)]
    pub(crate) fn is_collapsed(&self) -> bool {
        self.mode() == Mode::Persistent && !self.drawer.is_open()
    }

    pub(crate) fn set_collapsed(&mut self, collapsed: bool) {
        self.set_open(!collapsed);
    }

    pub(crate) fn visible(&self) -> bool {
        self.drawer.is_open()
    }

    pub(crate) fn toggle_open(&mut self) {
        self.drawer.toggle();
        self.drawer.snap(self.drawer.is_open());
    }

    /// The sidebar's band of the window's one header row.
    fn header(&self, theme: &Theme) -> Rect {
        let r = self.rect();
        Rect::new(
            r.x0,
            r.y0,
            r.x1,
            r.y0 + f64::from(theme.tokens.metrics.header_height),
        )
    }

    /// A header-row control square at `x0`, centred on the band.
    fn header_control(band: Rect, x0: f64, theme: &Theme) -> Rect {
        let side = f64::from(theme.tokens.metrics.min_pointer_target);
        let mid = band.center().y;
        Rect::new(x0, mid - side / 2.0, x0 + side, mid + side / 2.0)
    }

    /// The show/hide toggle at the sidebar's top-leading corner.
    pub(crate) fn toggle_rect(&self, theme: &Theme) -> Rect {
        let band = self.header(theme);
        Self::header_control(band, band.x0 + f64::from(theme.tokens.spacing.s3), theme)
    }

    /// The overflow menu button at the trailing end of the sidebar header.
    pub(crate) fn menu_rect(&self, theme: &Theme) -> Rect {
        let band = self.header(theme);
        let side = f64::from(theme.tokens.metrics.min_pointer_target);
        Self::header_control(
            band,
            band.x1 - f64::from(theme.tokens.spacing.s3) - side,
            theme,
        )
    }

    /// Paint the scrim (drawer only), the panel, the header controls and,
    /// when `switcher` names the active view, the view switcher under the
    /// header. Returns `None` while the sidebar is hidden.
    pub(crate) fn draw_chrome(
        &self,
        ctx: &mut PaintCtx<'_>,
        switcher: Option<View>,
        pointer: Point,
    ) -> Option<Chrome> {
        if !self.visible() {
            return None;
        }
        let theme = ctx.theme;
        let mut hits = Vec::new();
        if self.mode() == Mode::Overlay {
            self.drawer.draw_scrim(ctx.scene, ctx.transform, theme);
            let scrim = self.drawer.scrim_rect();
            if scrim.area() > 0.0 {
                hits.push((scrim, Action::Dismiss));
            }
            // The panel swallows presses that land between its rows.
            hits.push((self.rect(), Action::Guard));
        }
        let local = self.drawer.paint_panel(ctx.scene, ctx.transform, theme);

        let toggle = self.toggle_rect(&local);
        self.drawer.draw_toggle(
            ctx.scene,
            ctx.transform,
            ctx.text,
            &local,
            toggle,
            toggle.contains(pointer),
        );
        hits.push((toggle, Action::Toggle));

        let menu = self.menu_rect(&local);
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            &local,
            &ButtonFace::icon_only(menu, Icon::MoreHorizontal, "More")
                .hovered(menu.contains(pointer)),
        );
        hits.push((menu, Action::Menu));

        let mut body_y = self.header(&local).y1;
        if let Some(view) = switcher {
            let inset = f64::from(local.tokens.spacing.s3);
            let height = SegmentedFace::height(ControlSize::Regular, &local);
            let r = self.rect();
            let strip = Rect::new(r.x0 + inset, body_y, r.x1 - inset, body_y + height);
            let segments = segment_rects(strip, View::ALL.len());
            let labels: Vec<&str> = View::ALL.iter().map(|v| v.label()).collect();
            let selected = View::ALL.iter().position(|v| *v == view).unwrap_or(0);
            let hovered = segments.iter().position(|s| s.contains(pointer));
            ToggleGroup::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                &local,
                &SegmentedFace::new(&segments, &labels, selected).hovered(hovered),
            );
            for (segment, candidate) in segments.iter().zip(View::ALL) {
                hits.push((*segment, Action::View(candidate)));
            }
            body_y = strip.y1 + inset;
        }

        Some(Chrome {
            body_y,
            theme: local,
            hits,
        })
    }
}

/// `count` equal, contiguous segments across `strip`.
pub(crate) fn segment_rects(strip: Rect, count: usize) -> Vec<Rect> {
    let width = strip.width() / count.max(1) as f64;
    (0..count)
        .map(|index| {
            Rect::new(
                strip.x0 + width * index as f64,
                strip.y0,
                strip.x0 + width * (index as f64 + 1.0),
                strip.y1,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_mode_and_visibility_follow_width_text_scale_and_toggles() {
        let theme = Theme::dark();
        let docked = f64::from(theme.tokens.metrics.sidebar_width);
        // Case `layout_switches_between_docked_sidebar_and_drawer`.
        {
            let mut sidebar = Sidebar::new();
            assert_eq!(
                sidebar.layout(Rect::new(0.0, 0.0, 1280.0, 800.0), &theme),
                docked
            );
            assert_eq!(sidebar.mode(), Mode::Persistent);
            assert_eq!(sidebar.rect().width(), docked);

            assert_eq!(
                sidebar.layout(Rect::new(0.0, 0.0, 800.0, 600.0), &theme),
                0.0
            );
            assert_eq!(sidebar.mode(), Mode::Overlay);
        }
        // Case `visibility_obeys_the_active_presentation`.
        {
            let mut sidebar = Sidebar::new();
            sidebar.layout(Rect::new(0.0, 0.0, 1280.0, 800.0), &theme);
            assert!(sidebar.visible());
            sidebar.toggle_open();
            assert!(!sidebar.visible());
            assert!(sidebar.is_collapsed());

            sidebar.layout(Rect::new(0.0, 0.0, 800.0, 600.0), &theme);
            assert!(!sidebar.visible());
            sidebar.toggle_open();
            assert!(sidebar.visible());
            assert!(sidebar.is_open());
        }
        // Case `large_text_moves_the_breakpoint_so_the_calendar_keeps_its_room`.
        {
            let mut sidebar = Sidebar::new();
            let large = Theme::dark().with_text_scale(2.0);
            assert_eq!(
                sidebar.layout(Rect::new(0.0, 0.0, 1280.0, 900.0), &large),
                0.0
            );
            assert_eq!(sidebar.mode(), Mode::Overlay);
            assert!(
                !sidebar.visible(),
                "the drawer stays hidden until it is opened"
            );
        }
    }

    #[test]
    fn header_controls_sit_in_the_header_row() {
        let theme = Theme::dark();
        let mut sidebar = Sidebar::new();
        sidebar.layout(Rect::new(0.0, 0.0, 1280.0, 800.0), &theme);
        let toggle = sidebar.toggle_rect(&theme);
        let menu = sidebar.menu_rect(&theme);
        let header = f64::from(theme.tokens.metrics.header_height);
        assert!(toggle.y0 >= 0.0 && toggle.y1 <= header);
        assert!((toggle.center().y - menu.center().y).abs() < 1e-9);
        assert!(toggle.x1 < menu.x0);
    }
}

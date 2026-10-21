//! Responsive sidebar chrome.
//!
//! Presentation state and geometry come from the shared
//! [`ui::widget::SidebarDrawer`]: docked beside the calendar when the window
//! is wide, a floating drawer over it when narrow, and the person's show/hide
//! choice per size class. The drawer paints the panel; its toggle, the view
//! switcher and the calendar file commands are the window bar's slots
//! ([`crate::app`]). The navigation rows themselves are painted by
//! `CalendarApp`, which owns the model.

use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::widget::{SidebarDrawer, SplitLayout};
use vello::kurbo::Rect;

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
}

/// What the chrome pass painted: where the navigation rows start, the theme
/// to paint them with (the sidebar region's, or the floating drawer's), and
/// the hit rects of the panel itself.
pub(crate) struct Chrome {
    pub(crate) body_y: f64,
    pub(crate) theme: Theme,
    pub(crate) hits: Vec<(Rect, Action)>,
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
            // Docked while the calendar keeps the room a list of days needs
            // beside it; the timeline narrows its density below that.
            drawer: SidebarDrawer::new()
                .left()
                .content_kind(ui::widget::ContentKind::List),
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

    /// Paint the scrim (drawer only) and the panel. Returns `None` while the
    /// sidebar is hidden.
    pub(crate) fn draw_chrome(&self, ctx: &mut PaintCtx<'_>) -> Option<Chrome> {
        if !self.visible() {
            return None;
        }
        let theme = ctx.theme;
        let mut hits = Vec::new();
        if self.mode() == Mode::Overlay {
            let scrim = self.drawer.scrim_rect();
            if scrim.area() > 0.0 {
                hits.push((scrim, Action::Dismiss));
            }
            // The panel swallows presses that land between its rows.
            hits.push((self.rect(), Action::Guard));
        }
        let local = self.drawer.paint_panel(ctx.scene, ctx.transform, theme);
        let body_y = self.rect().y0 + f64::from(local.tokens.spacing.s3);
        Some(Chrome {
            body_y,
            theme: local,
            hits,
        })
    }
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

            // The sidebar shrinks toward its minimum before it stops docking.
            assert_eq!(
                sidebar.layout(Rect::new(0.0, 0.0, 800.0, 600.0), &theme),
                docked
            );
            assert_eq!(sidebar.mode(), Mode::Persistent);
            assert_eq!(
                sidebar.layout(Rect::new(0.0, 0.0, 480.0, 600.0), &theme),
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

            sidebar.layout(Rect::new(0.0, 0.0, 480.0, 600.0), &theme);
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
                sidebar.layout(Rect::new(0.0, 0.0, 1000.0, 900.0), &theme),
                docked,
                "1000 px docks at 1x"
            );
            let mut sidebar = Sidebar::new();
            assert_eq!(
                sidebar.layout(Rect::new(0.0, 0.0, 1000.0, 900.0), &large),
                0.0
            );
            assert_eq!(sidebar.mode(), Mode::Overlay);
            assert!(
                !sidebar.visible(),
                "the drawer stays hidden until it is opened"
            );
        }
    }
}

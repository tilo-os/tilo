//! Responsive sidebar chrome.
//!
//! Presentation state and geometry come from the shared
//! [`ui::widget::SidebarDrawer`] (docked-vs-overlay, open/collapse memory,
//! content inset). Calendar-specific header controls stay here; lists and
//! tasks remain in `CalendarApp`; the mini-month is its own footer component.

use ui::paint::PaintCtx;
use ui::widget::{SidebarDrawer, SplitLayout};
use vello::kurbo::{Affine, Point, Rect};

use crate::app::{
    center_text, draw_chevron, draw_cross, draw_download, draw_icon_button, draw_upload,
};
use crate::draw_util::{fill_rect, fill_round, hairline, selected_row};
use crate::model::View;
use ui::surface::card;

const WIDTH: f64 = 252.0;
/// Matches the shared drawer family's wide dock threshold used across apps
/// that keep a full calendar column beside the rail.
const PERSISTENT_MIN: f64 = 900.0;
const FULLSCREEN_MAX: f64 = 560.0;
/// At large accessibility text sizes a 252px dock cannot provide usable
/// columns for names, counts, and priority chips.  Treat the dock as a drawer
/// instead, so the calendar keeps a full-width, non-overlapping primary view.
const LARGE_TEXT_DRAWER_SCALE: f32 = 1.35;

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
    ToggleCollapsed,
    View(View),
    Export,
    Import,
}

pub(crate) struct Chrome {
    pub(crate) body_y: f64,
    pub(crate) hits: Vec<(Rect, Action)>,
}

/// The window and the panes beside the sidebar: the overlay drawer dims
/// whichever of `content` and `detail` is showing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Panes {
    pub(crate) window: Rect,
    pub(crate) content: Rect,
    pub(crate) detail: Rect,
}

/// One chrome paint pass: the pointer driving hover, and the hit rects the
/// header controls push.
struct ChromePaint<'a> {
    ctx: PaintCtx<'a>,
    pointer: Point,
    hits: Vec<(Rect, Action)>,
}

pub(crate) struct Sidebar {
    drawer: SidebarDrawer,
    text_scale: f32,
}

impl Sidebar {
    pub(crate) fn new() -> Self {
        Self {
            drawer: SidebarDrawer::new()
                .left()
                .docked_width(WIDTH)
                .overlay_width(WIDTH)
                .breakpoint(PERSISTENT_MIN)
                .rail_width(0.0),
            text_scale: 1.0,
        }
    }

    /// Recompute the responsive drawer and return the x coordinate at which
    /// the calendar content begins.
    pub(crate) fn layout(&mut self, window: Rect) -> f64 {
        let large_text = self.text_scale >= LARGE_TEXT_DRAWER_SCALE;
        // Force overlay when large text needs the full primary column, even on
        // a wide window — same policy as before, expressed as a breakpoint the
        // shared drawer cannot satisfy at this width.
        let breakpoint = if large_text {
            window.width() + 1.0
        } else {
            PERSISTENT_MIN
        };
        let overlay_width = if window.width() < FULLSCREEN_MAX || large_text {
            window.width()
        } else {
            WIDTH
        };
        self.drawer.set_breakpoint(breakpoint);
        self.drawer.set_docked_width(WIDTH);
        self.drawer.set_overlay_width(overlay_width);
        self.drawer.resolve(window);
        // Calendar chrome toggles snap; never tick the eased reveal here so
        // layout stays deterministic for tests and `--shots`.
        let open = self.drawer.is_open();
        self.drawer.snap(open);
        self.drawer.resolve(window).x0
    }

    /// The current theme scale belongs to layout, not durable sidebar state.
    pub(crate) fn set_text_scale(&mut self, scale: f32) {
        self.text_scale = if scale.is_finite() {
            scale.clamp(0.5, 4.0)
        } else {
            1.0
        };
    }

    pub(crate) fn mode(&self) -> Mode {
        Mode::from_layout(self.drawer.layout())
    }

    pub(crate) fn rect(&self) -> Rect {
        self.drawer.sidebar_rect()
    }

    /// Overlay-flap open flag (unchecked while docked).
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

    /// Docked-rail collapsed flag (false while overlay).
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

    pub(crate) fn toggle_collapsed(&mut self) {
        self.toggle_open();
    }

    /// Paint the sidebar ground and header controls. `switcher` is the
    /// active view when the view switcher is the sidebar's to show (it is not
    /// in the top bar).
    pub(crate) fn draw_chrome(
        &self,
        ctx: &mut PaintCtx<'_>,
        panes: Panes,
        switcher: Option<View>,
        pointer: Point,
    ) -> Option<Chrome> {
        if !self.visible() {
            return None;
        }
        let Panes {
            window,
            content,
            detail,
        } = panes;
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let colors = &theme.tokens.colors;
        let mut hits = Vec::new();
        let overlay = self.mode() == Mode::Overlay;
        if overlay && (content.area() > 0.0 || detail.area() > 0.0) {
            let behind = if content.area() > 0.0 {
                content
            } else {
                detail
            };
            fill_rect(scene, transform, behind, colors.surfaces.overlay);
            hits.push((behind, Action::Dismiss));
        }

        let rect = self.rect();
        if overlay {
            // The drawer's elevation over the dimmed content: the theme's
            // shadow token, not a literal black band.
            ui::surface::elevation(scene, transform, rect, &theme.tokens.elevation.e3);
        }
        fill_rect(scene, transform, rect, colors.surfaces.raised);
        hairline(
            scene,
            transform,
            colors.surfaces.raised,
            Point::new(rect.x1, rect.y0),
            Point::new(rect.x1, rect.y1),
        );

        let mut paint = ChromePaint {
            ctx: ctx.reborrow(),
            pointer,
            hits,
        };
        let mut y = rect.y0 + 22.0;
        self.draw_chrome_collapse_or_dismiss(&mut paint, window, y);
        y += 30.0;

        if overlay && let Some(view) = switcher {
            y += self.draw_chrome_view_switcher(&mut paint, view, y);
        }

        self.draw_chrome_footer_buttons(&mut paint, y);

        Some(Chrome {
            body_y: y + 46.0,
            hits: paint.hits,
        })
    }

    /// The collapse (persistent mode) or dismiss (overlay mode, once the
    /// drawer covers the full window) button, top-right of the sidebar
    /// header. Split out of [`Self::draw_chrome`] to keep that under the
    /// line cap.
    fn draw_chrome_collapse_or_dismiss(&self, paint: &mut ChromePaint<'_>, window: Rect, y: f64) {
        let (scene, transform, theme) =
            (&mut *paint.ctx.scene, paint.ctx.transform, paint.ctx.theme);
        let (pointer, hits) = (paint.pointer, &mut paint.hits);
        let rect = self.rect();
        let overlay = self.mode() == Mode::Overlay;
        let pad = 18.0;
        let colors = &theme.tokens.colors;
        if !overlay {
            let button = Rect::new(rect.x1 - pad - 24.0, y - 3.0, rect.x1 - pad, y + 21.0);
            if button.contains(pointer) {
                fill_round(scene, transform, button, 7.0, colors.lift(0.35));
            }
            draw_chevron(scene, transform, colors.neutral[7], button.center(), true);
            hits.push((button, Action::ToggleCollapsed));
        } else if rect.width() >= window.width() - 0.5 {
            let button = Rect::new(rect.x1 - pad - 24.0, y - 4.0, rect.x1 - pad, y + 20.0);
            draw_icon_button(scene, transform, theme, button, button.contains(pointer));
            draw_cross(scene, transform, colors.neutral[8], button.center());
            hits.push((button, Action::Dismiss));
        }
    }

    /// The overlay-mode Month/Week/Day/Agenda segmented switcher, shown only
    /// when the switcher doesn't already live in the top bar. Returns the
    /// vertical space it consumed (`0.0` if not drawn). Split out of
    /// [`Self::draw_chrome`] to keep that under the line cap.
    fn draw_chrome_view_switcher(&self, paint: &mut ChromePaint<'_>, view: View, y: f64) -> f64 {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = &mut paint.ctx;
        let (transform, theme) = (*transform, *theme);
        let (pointer, hits) = (paint.pointer, &mut paint.hits);
        let rect = self.rect();
        let colors = &theme.tokens.colors;
        let inset = f64::from(theme.tokens.spacing.s3);
        let segmented = Rect::new(rect.x0 + inset, y, rect.x1 - inset, y + 36.0);
        let track_radius = f64::from(theme.tokens.radii.lg);
        // Segmented track (HIG §9.5): the base tier with its derived edge; the
        // chosen segment is the selection lift, never an accent fill.
        card(
            scene,
            transform,
            segmented,
            track_radius,
            colors.surfaces.base,
        );
        let cell_width = segmented.width() / View::ALL.len() as f64;
        let seg_inset = f64::from(theme.tokens.spacing.s1) / 2.0 + 1.0;
        for (index, candidate) in View::ALL.iter().enumerate() {
            let cell = Rect::new(
                segmented.x0 + cell_width * index as f64,
                segmented.y0,
                segmented.x0 + cell_width * (index as f64 + 1.0),
                segmented.y1,
            );
            selected_row(
                scene,
                transform,
                theme,
                colors.surfaces.base,
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
            hits.push((cell, Action::View(*candidate)));
        }
        50.0
    }

    /// The Export/Import footer buttons, side by side at the bottom of the
    /// sidebar header. Split out of [`Self::draw_chrome`] to keep that under
    /// the line cap.
    fn draw_chrome_footer_buttons(&self, paint: &mut ChromePaint<'_>, y: f64) {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = &mut paint.ctx;
        let (transform, theme) = (*transform, *theme);
        let (pointer, hits) = (paint.pointer, &mut paint.hits);
        let rect = self.rect();
        let colors = &theme.tokens.colors;
        let button_width = (rect.width() - 24.0 - 8.0) / 2.0;
        let export = Rect::new(rect.x0 + 12.0, y, rect.x0 + 12.0 + button_width, y + 32.0);
        draw_icon_button(scene, transform, theme, export, export.contains(pointer));
        draw_download(
            scene,
            transform,
            colors.neutral[8],
            Point::new(export.x0 + 18.0, export.center().y),
        );
        text.draw_text(
            scene,
            "Export",
            13.0,
            colors.neutral[8],
            transform * Affine::translate((export.x0 + 32.0, export.center().y - 8.0)),
            None,
        );
        hits.push((export, Action::Export));

        let import = Rect::new(export.x1 + 8.0, y, rect.x1 - 12.0, y + 32.0);
        draw_icon_button(scene, transform, theme, import, import.contains(pointer));
        draw_upload(
            scene,
            transform,
            colors.neutral[8],
            Point::new(import.x0 + 18.0, import.center().y),
        );
        text.draw_text(
            scene,
            "Import",
            13.0,
            colors.neutral[8],
            transform * Affine::translate((import.x0 + 32.0, import.center().y - 8.0)),
            None,
        );
        hits.push((import, Action::Import));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_mode_and_visibility_follow_width_text_scale_and_toggles() {
        // Case `layout_switches_between_inline_flap_and_phone_drawer`.
        {
            let mut sidebar = Sidebar::new();
            assert_eq!(sidebar.layout(Rect::new(0.0, 0.0, 1280.0, 800.0)), WIDTH);
            assert_eq!(sidebar.mode(), Mode::Persistent);
            assert_eq!(sidebar.rect().width(), WIDTH);

            assert_eq!(sidebar.layout(Rect::new(0.0, 0.0, 700.0, 800.0)), 0.0);
            assert_eq!(sidebar.mode(), Mode::Overlay);
            assert_eq!(sidebar.rect().width(), WIDTH);

            sidebar.layout(Rect::new(0.0, 0.0, 420.0, 800.0));
            assert_eq!(sidebar.rect().width(), 420.0);
        }
        // Case `visibility_obeys_the_active_presentation`.
        {
            let mut sidebar = Sidebar::new();
            sidebar.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            assert!(sidebar.visible());
            sidebar.toggle_collapsed();
            assert!(!sidebar.visible());

            sidebar.layout(Rect::new(0.0, 0.0, 700.0, 800.0));
            assert!(!sidebar.visible());
            sidebar.toggle_open();
            assert!(sidebar.visible());
        }
        // Case `large_text_uses_a_full_width_drawer_even_on_desktop`.
        {
            let mut sidebar = Sidebar::new();
            sidebar.set_text_scale(2.0);
            assert_eq!(sidebar.layout(Rect::new(0.0, 0.0, 1200.0, 900.0)), 0.0);
            assert_eq!(sidebar.mode(), Mode::Overlay);
            assert_eq!(sidebar.rect().width(), 1200.0);
            assert!(
                !sidebar.visible(),
                "drawer stays behind the menu until opened"
            );
        }
    }
}

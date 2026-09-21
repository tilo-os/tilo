//! Responsive sidebar chrome.
//!
//! This component owns the drawer's presentation state and geometry together
//! with the controls that operate that state. Calendar-specific lists and
//! tasks remain in `CalendarApp`; the mini-month is its own footer component.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::app::{
    center_text, draw_chevron, draw_cross, draw_download, draw_icon_button, draw_upload,
};
use crate::draw_util::{fill_rect, fill_round, hairline, selected_row};
use crate::model::View;
use ui::surface::card;

const WIDTH: f64 = 252.0;
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

pub(crate) struct Sidebar {
    mode: Mode,
    open: bool,
    collapsed: bool,
    rect: Rect,
    text_scale: f32,
}

impl Sidebar {
    pub(crate) fn new() -> Self {
        Self {
            mode: Mode::Persistent,
            open: false,
            collapsed: false,
            rect: Rect::ZERO,
            text_scale: 1.0,
        }
    }

    /// Recompute the responsive drawer and return the x coordinate at which
    /// the calendar content begins.
    pub(crate) fn layout(&mut self, window: Rect) -> f64 {
        self.mode = if window.width() >= PERSISTENT_MIN && self.text_scale < LARGE_TEXT_DRAWER_SCALE
        {
            Mode::Persistent
        } else {
            Mode::Overlay
        };
        let width = if self.mode == Mode::Overlay
            && (window.width() < FULLSCREEN_MAX || self.text_scale >= LARGE_TEXT_DRAWER_SCALE)
        {
            window.width()
        } else {
            WIDTH
        };
        self.rect = Rect::new(window.x0, window.y0, window.x0 + width, window.y1);
        match self.mode {
            Mode::Persistent if !self.collapsed => window.x0 + WIDTH,
            _ => window.x0,
        }
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
        self.mode
    }

    pub(crate) fn rect(&self) -> Rect {
        self.rect
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub(crate) fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    pub(crate) fn set_collapsed(&mut self, collapsed: bool) {
        self.collapsed = collapsed;
    }

    pub(crate) fn visible(&self) -> bool {
        match self.mode {
            Mode::Persistent => !self.collapsed,
            Mode::Overlay => self.open,
        }
    }

    pub(crate) fn toggle_open(&mut self) {
        self.open = !self.open;
    }

    pub(crate) fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "sidebar chrome combines paint context, window/content/detail bounds, view-switcher placement and pointer hover"
    )]
    pub(crate) fn draw_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
        content: Rect,
        detail: Rect,
        switcher_in_topbar: bool,
        view: View,
        pointer: Point,
    ) -> Option<Chrome> {
        if !self.visible() {
            return None;
        }
        let colors = &theme.tokens.colors;
        let mut hits = Vec::new();
        let overlay = self.mode == Mode::Overlay;
        if overlay && (content.area() > 0.0 || detail.area() > 0.0) {
            let behind = if content.area() > 0.0 {
                content
            } else {
                detail
            };
            fill_rect(scene, transform, colors.surfaces.overlay, behind);
            hits.push((behind, Action::Dismiss));
        }

        let rect = self.rect;
        if overlay {
            // The drawer's elevation over the dimmed content: the theme's
            // shadow token, not a literal black band.
            ui::surface::elevation(scene, transform, rect, &theme.tokens.elevation.e3);
        }
        fill_rect(scene, transform, colors.surfaces.raised, rect);
        hairline(
            scene,
            transform,
            colors.surfaces.raised,
            Point::new(rect.x1, rect.y0),
            Point::new(rect.x1, rect.y1),
        );

        let pad = 18.0;
        let mut y = rect.y0 + 22.0;
        Self::draw_chrome_collapse_or_dismiss(
            scene, transform, theme, rect, window, overlay, pointer, pad, y, &mut hits,
        );
        y += 30.0;

        if overlay && !switcher_in_topbar {
            y += Self::draw_chrome_view_switcher(
                scene, transform, text, theme, rect, view, pointer, y, &mut hits,
            );
        }

        Self::draw_chrome_footer_buttons(
            scene, transform, text, theme, rect, y, pointer, &mut hits,
        );

        Some(Chrome {
            body_y: y + 46.0,
            hits,
        })
    }

    /// The collapse (persistent mode) or dismiss (overlay mode, once the
    /// drawer covers the full window) button, top-right of the sidebar
    /// header. Split out of [`Self::draw_chrome`] to keep that under the
    /// line cap.
    fn draw_chrome_collapse_or_dismiss(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        window: Rect,
        overlay: bool,
        pointer: Point,
        pad: f64,
        y: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let colors = &theme.tokens.colors;
        if !overlay {
            let button = Rect::new(rect.x1 - pad - 24.0, y - 3.0, rect.x1 - pad, y + 21.0);
            if button.contains(pointer) {
                fill_round(scene, transform, colors.lift(0.35), button, 7.0);
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
    fn draw_chrome_view_switcher(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        view: View,
        pointer: Point,
        y: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
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
    fn draw_chrome_footer_buttons(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        y: f64,
        pointer: Point,
        hits: &mut Vec<(Rect, Action)>,
    ) {
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
    fn layout_switches_between_inline_flap_and_phone_drawer() {
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

    #[test]
    fn visibility_obeys_the_active_presentation() {
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

    #[test]
    fn large_text_uses_a_full_width_drawer_even_on_desktop() {
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

//! The collapsible side panel every mode shares, over the toolkit's
//! [`SidebarDrawer`]: docked beside the content in a wide window, a floating
//! drawer over it in a narrow one. The panel heads itself with its title and
//! its actions; the modes and the angle unit are the window bar's.

use ui::theme::Theme;
use ui::widget::{SidebarDrawer, SidebarSide, text_control_height};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub height: f64,
    pub inset: f64,
    pub gap: f64,
    pub chip: f64,
}

impl Default for Header {
    fn default() -> Self {
        Self::of(&Theme::dark())
    }
}

impl Header {
    pub fn of(theme: &Theme) -> Self {
        let chip = text_control_height(f64::from(theme.tokens.metrics.min_pointer_target), theme);
        let inset = f64::from(theme.tokens.spacing.s3);
        Self {
            height: f64::from(theme.tokens.metrics.header_height).max(chip + 2.0 * inset),
            inset,
            gap: f64::from(theme.tokens.spacing.s2),
            chip,
        }
    }

    pub fn band(&self, col: Rect) -> Rect {
        Rect::new(col.x0, col.y0, col.x1, (col.y0 + self.height).min(col.y1))
    }
}

/// The narrowest a formula row fits in; below docked width plus the plot's,
/// the panel floats instead.
// ratchet-allow(pixel_const): the width a formula row fits in, measured from its content, not spacing
const PANEL_MIN_WIDTH: f64 = 340.0; // allow-breakpoint: the narrowest the panel may be, not a breakpoint

/// A collapsible side panel over the shared drawer's geometry and state.
pub struct SidePanel {
    drawer: SidebarDrawer,
    column: Rect,
    content: Rect,
    header: Header,
}

impl SidePanel {
    /// `leading` puts the panel at the leading edge; `docked` is its width at
    /// 1× when it sits beside the content.
    pub fn new(leading: bool, docked: f64, kind: ui::widget::ContentKind) -> Self {
        let side = if leading {
            SidebarSide::Left
        } else {
            SidebarSide::Right
        };
        Self {
            drawer: SidebarDrawer::new()
                .side(side)
                .docked_width(docked)
                .content_kind(kind)
                .panel_min_width(PANEL_MIN_WIDTH),
            column: Rect::ZERO,
            content: Rect::ZERO,
            header: Header::default(),
        }
    }

    /// Call from the layout pass.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.drawer.scale_for(theme);
        self.header = Header::of(theme);
    }

    /// One frame of the reveal; draw path only.
    pub fn tick(&mut self, theme: &Theme) {
        self.drawer.tick(theme);
    }

    pub fn layout(&mut self, area: Rect) {
        self.content = self.drawer.resolve(area);
        let visible = self.drawer.reveal() > 0.001 && self.drawer.sidebar_rect().width() > 0.5;
        self.column = if visible {
            self.drawer.sidebar_rect()
        } else {
            Rect::ZERO
        };
    }

    /// Header row included; empty while hidden.
    pub fn column(&self) -> Rect {
        self.column
    }

    /// Beside the panel; the whole window when it floats.
    pub fn content(&self) -> Rect {
        self.content
    }

    /// Empty while hidden.
    pub fn header_band(&self) -> Rect {
        if self.column == Rect::ZERO {
            Rect::ZERO
        } else {
            self.header.band(self.column)
        }
    }

    pub fn header_height(&self) -> f64 {
        self.header.height
    }

    /// Empty while hidden.
    pub fn body(&self) -> Rect {
        if self.column == Rect::ZERO {
            Rect::ZERO
        } else {
            Rect::new(
                self.column.x0,
                (self.column.y0 + self.header.height).min(self.column.y1),
                self.column.x1,
                self.column.y1,
            )
        }
    }

    pub fn is_open(&self) -> bool {
        self.drawer.is_open()
    }

    pub fn is_overlay(&self) -> bool {
        self.drawer.is_overlay()
    }

    pub fn is_animating(&self) -> bool {
        self.drawer.is_animating()
    }

    pub fn scrim_at(&self, p: Point) -> bool {
        self.drawer.scrim_contains(p)
    }

    /// Whether the floating drawer covers `p` and blocks presses beneath.
    pub fn covers(&self, p: Point) -> bool {
        self.drawer.is_overlay() && self.drawer.sidebar_contains(p)
    }

    pub fn toggle(&mut self) {
        self.drawer.toggle();
    }

    pub fn close(&mut self) {
        self.drawer.close();
    }

    /// No animation; for captures and tests.
    pub fn snap(&mut self, open: bool) {
        self.drawer.snap(open);
    }

    /// Returns the theme the panel's contents draw with.
    pub fn paint_panel(&self, scene: &mut Scene, transform: Affine, theme: &Theme) -> Theme {
        self.drawer.paint_panel(scene, transform, theme)
    }
}

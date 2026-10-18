//! The window's header row and the collapsible side panel every mode shares.
//!
//! One header row runs across the top of the window and holds each column's
//! header: the panel's title and its show/hide button, and the content
//! column's mode and angle-unit controls. The panel is the toolkit's
//! [`SidebarDrawer`]: docked beside the content in a wide window, a floating
//! drawer over it in a narrow one.

use ui::theme::Theme;
use ui::widget::{SidebarDrawer, SidebarSide, text_control_height};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

/// The measurements of the header row in one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    /// Height of the whole row.
    pub height: f64,
    /// Inset of a column's controls from its edges.
    pub inset: f64,
    /// Between neighbouring controls.
    pub gap: f64,
    /// Between two clusters of controls on the row (the mode strip and the
    /// angle-unit strip), far enough apart to read as separate controls.
    pub cluster_gap: f64,
    /// Side of an icon button.
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
            cluster_gap: f64::from(theme.tokens.spacing.rhythm.group),
            chip,
        }
    }

    /// The header row of the column `col`.
    pub fn band(&self, col: Rect) -> Rect {
        Rect::new(col.x0, col.y0, col.x1, (col.y0 + self.height).min(col.y1))
    }

    /// An icon button seated at the leading or trailing end of `col`'s header
    /// row.
    pub fn chip_at(&self, col: Rect, leading: bool) -> Rect {
        let y = col.y0 + (self.height - self.chip) / 2.0;
        let x = if leading {
            col.x0 + self.inset
        } else {
            col.x1 - self.inset - self.chip
        };
        Rect::new(x, y, x + self.chip, y + self.chip)
    }
}

/// A collapsible side panel: its geometry, its show/hide button and its
/// remembered state, all from the shared drawer.
pub struct SidePanel {
    drawer: SidebarDrawer,
    leading: bool,
    column: Rect,
    content: Rect,
    toggle: Rect,
    header: Header,
    /// Room kept at the window's trailing corner for the window's History
    /// control, which the trailing show/hide button stands beside.
    trailing_reserve: f64,
}

impl SidePanel {
    /// A panel at the leading edge (`leading`) or the trailing edge, `docked`
    /// wide at 1× when it sits beside the content.
    pub fn new(leading: bool, docked: f64) -> Self {
        let side = if leading {
            SidebarSide::Left
        } else {
            SidebarSide::Right
        };
        Self {
            drawer: SidebarDrawer::new().side(side).docked_width(docked),
            leading,
            column: Rect::ZERO,
            content: Rect::ZERO,
            toggle: Rect::ZERO,
            header: Header::default(),
            trailing_reserve: 0.0,
        }
    }

    /// Adopt the theme's scale and header measurements. Call from the layout
    /// pass with the theme in hand.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.drawer.scale_for(theme);
        self.header = Header::of(theme);
    }

    /// Advance the eased reveal one frame (draw path only).
    pub fn tick(&mut self, theme: &Theme) {
        self.drawer.tick(theme);
    }

    /// Keep `reserve` of the trailing corner free for the window's History
    /// control; the show/hide button of a trailing panel stands beside it.
    pub fn set_trailing_reserve(&mut self, reserve: f64) {
        self.trailing_reserve = reserve;
    }

    /// Resolve the panel, content column and button for the window `area`.
    pub fn layout(&mut self, area: Rect) {
        self.content = self.drawer.resolve(area);
        let visible = self.drawer.reveal() > 0.001 && self.drawer.sidebar_rect().width() > 0.5;
        self.column = if visible {
            self.drawer.sidebar_rect()
        } else {
            Rect::ZERO
        };
        // Docked, the button stays at the window's corner (on the panel while
        // it shows, on the content while it is hidden) so it never chases a
        // sliding edge. A floating drawer carries its own.
        let on = if self.drawer.is_overlay() && self.column != Rect::ZERO {
            self.column
        } else {
            area
        };
        self.toggle = self.header.chip_at(on, self.leading);
        if !self.leading {
            self.toggle = self.toggle - Vec2::new(self.trailing_reserve, 0.0);
        }
    }

    /// The panel's rect (header row included), `Rect::ZERO` while hidden.
    pub fn column(&self) -> Rect {
        self.column
    }

    /// The area beside the panel (the whole window when it floats).
    pub fn content(&self) -> Rect {
        self.content
    }

    /// The panel's header row, `Rect::ZERO` while hidden.
    pub fn header_band(&self) -> Rect {
        if self.column == Rect::ZERO {
            Rect::ZERO
        } else {
            self.header.band(self.column)
        }
    }

    /// Height of the header row, in this theme.
    pub fn header_height(&self) -> f64 {
        self.header.height
    }

    /// The panel below its header row, `Rect::ZERO` while hidden.
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

    pub fn toggle_rect(&self) -> Rect {
        self.toggle
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

    /// Whether the toggle sits in the content column's header (the panel is
    /// hidden) rather than inside the panel's own.
    pub fn toggle_on_content(&self) -> bool {
        !(self.column != Rect::ZERO && self.column.contains_rect(self.toggle))
    }

    pub fn toggle_at(&self, p: Point) -> bool {
        self.toggle != Rect::ZERO && self.toggle.contains(p)
    }

    /// Whether `p` is a press on the dismiss scrim of an open drawer.
    pub fn scrim_at(&self, p: Point) -> bool {
        self.drawer.scrim_contains(p)
    }

    /// Whether the floating drawer covers `p` (so a press there must not
    /// reach what is beneath it).
    pub fn covers(&self, p: Point) -> bool {
        self.drawer.is_overlay() && self.drawer.sidebar_contains(p)
    }

    pub fn toggle(&mut self) {
        self.drawer.toggle();
    }

    pub fn close(&mut self) {
        self.drawer.close();
    }

    /// Snap open or shut with no animation (captures and tests).
    pub fn snap(&mut self, open: bool) {
        self.drawer.snap(open);
    }

    /// Paint the panel's ground and return the theme its contents draw with.
    pub fn paint_panel(&self, scene: &mut Scene, transform: Affine, theme: &Theme) -> Theme {
        self.drawer.paint_panel(scene, transform, theme)
    }

    /// Paint the show/hide button with the theme of the ground it stands on.
    pub fn paint_toggle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        hovered: bool,
    ) {
        if self.toggle != Rect::ZERO {
            self.drawer
                .draw_toggle(scene, transform, text, theme, self.toggle, hovered);
        }
    }
}

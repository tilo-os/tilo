//! The collapsible side panel every mode shares, and its header row.
//!
//! The panel heads itself with its title and its own actions; its show/hide
//! button, the modes and the angle unit are the window bar's slots
//! ([`crate::app`]). The panel is the toolkit's [`SidebarDrawer`]: docked
//! beside the content in a wide window, a floating drawer over it in a
//! narrow one.

use ui::theme::Theme;
use ui::widget::{SidebarDrawer, SidebarSide, text_control_height};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// The measurements of a panel's header row in one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    /// Height of the whole row.
    pub height: f64,
    /// Inset of a column's controls from its edges.
    pub inset: f64,
    /// Between neighbouring controls.
    pub gap: f64,
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
            chip,
        }
    }

    /// The header row of the column `col`.
    pub fn band(&self, col: Rect) -> Rect {
        Rect::new(col.x0, col.y0, col.x1, (col.y0 + self.height).min(col.y1))
    }
}

/// The narrowest the formulas shrink to before the panel stops docking.
///
/// A formula row is a math field between its visibility and delete targets,
/// so the panel needs about this much before it is a row rather than a
/// stack of buttons; below docked width plus the plot's it floats instead.
const PANEL_MIN_WIDTH: f64 = 340.0; // allow-breakpoint: the narrowest the panel may be, not a breakpoint

/// A collapsible side panel: its geometry and its remembered state, both
/// from the shared drawer. Its show/hide button is the window bar's.
pub struct SidePanel {
    drawer: SidebarDrawer,
    column: Rect,
    content: Rect,
    header: Header,
}

impl SidePanel {
    /// A panel at the leading edge (`leading`) or the trailing edge, `docked`
    /// wide at 1× when it sits beside the content — a page of formulas beside
    /// the result it calculates, as the shell's sidebar sits beside a page.
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

    /// Resolve the panel and the content column for the window `area`.
    pub fn layout(&mut self, area: Rect) {
        self.content = self.drawer.resolve(area);
        let visible = self.drawer.reveal() > 0.001 && self.drawer.sidebar_rect().width() > 0.5;
        self.column = if visible {
            self.drawer.sidebar_rect()
        } else {
            Rect::ZERO
        };
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

    pub fn is_open(&self) -> bool {
        self.drawer.is_open()
    }

    pub fn is_overlay(&self) -> bool {
        self.drawer.is_overlay()
    }

    pub fn is_animating(&self) -> bool {
        self.drawer.is_animating()
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
}

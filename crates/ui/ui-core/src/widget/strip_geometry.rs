//! The measures a strip of windows is laid out with: the room kept around it, the gap between two
//! windows, and the share of a display each section of a strip takes. The shell lays strips out
//! from these, and so does the session when it places a window before it is drawn.

use vello::kurbo::Rect;

/// Group widths are the shell model's: a fraction of the strip, never
/// pixels, with one set of tiers and clamps for every host.
pub use tilo_desktop_model::width::{
    WIDTH_FLOOR_PX, WIDTH_MAX_PERMILLE, WIDTH_MIN_PERMILLE, WIDTH_NARROW_PERMILLE,
    WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE, WindowWidth, max_group_px, min_group_px,
    permille_for_px,
};

/// Only reached from a bare `ui` host with no output spans; the shell
/// always pushes real geometry first.
pub const DEFAULT_STRIP_USABLE_PX: f64 = 1280.0;

/// The room kept between a strip's windows and the display's leading and trailing edges.
pub const STRIP_EDGE_GUTTER_PX: f64 = tilo_ui_base::tokens::Spacing::BASE.s8 as f64;
/// The room kept below a strip's windows.
pub const STRIP_BOTTOM_GUTTER_PX: f64 = tilo_ui_base::tokens::Spacing::BASE.s3 as f64;
/// The room kept above a strip's windows.
pub const STRIP_TOP_GAP_PX: f64 = tilo_ui_base::tokens::Spacing::BASE.s1 as f64;

/// The gap between two adjacent windows of a strip: anything that walks the strip's canvas has
/// to step by this between neighbours.
pub const HANDLE_SLOT_PX: f64 = 8.0;

/// One display's share of a strip partitioned across displays: each holds its own groups, laid out
/// and clipped inside its section only, so none straddles a border and panning one never reveals
/// another's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripSection {
    pub output: u32,
    pub area: Rect,
    pub usable: f64,
    pub leading: bool,
}

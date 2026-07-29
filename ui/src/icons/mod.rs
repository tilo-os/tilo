//! Lucide-icon glyph module (gap #5).
//!
//! The shell chrome used to paint its icons as ad-hoc kurbo primitives —
//! four filled squares for the overview glyph, a stroked ring + dot for
//! the "gear", three filled dots for the tray. That reads as a geometric
//! toy next to the concept's lucide-react glyph set. This module gives
//! widgets a single stroked-glyph helper so the shell matches the
//! concept's 1.5 px lucide aesthetic without every call site rolling
//! its own BezPath.
//!
//! # Design
//!
//! * Each [`Icon`] variant resolves to one or more [`BezPath`] subpaths
//!   traced from the upstream lucide-react source (v0.546.0, ISC
//!   licensed). Paths are stored in the lucide 24×24 viewBox; we scale
//!   and translate at draw time so the same [`Icon`] can render at any
//!   pixel size.
//! * Lucide icons are *stroked*, not filled — default attrs set
//!   `stroke-width: 2`, `stroke-linecap: round`, `stroke-linejoin:
//!   round`, `fill: none`. [`draw_icon`] mirrors that exactly.
//! * Some lucide icons are composed of multiple SVG primitives
//!   (`<rect>`, `<line>`, `<circle>`, `<path>`). We flatten those into
//!   a `Vec<BezPath>` up-front so the draw helper only has to walk
//!   strokable subpaths.
//! * This module intentionally stays small. No trait hierarchy, no
//!   icon-theme system, no per-icon caching — just a data table and
//!   one pure draw function. When a new icon is needed, add a variant
//!   + a row to [`icon_subpaths`].
//!
//! # Adding a new icon
//!
//! 1. Pull the `__iconNode` entries from
//!    `nirios-concept/node_modules/lucide-react/dist/esm/icons/<kebab>.js`
//!    (or directly from the lucide-icons repo).
//! 2. Add a variant to [`Icon`].
//! 3. Map it in [`icon_subpaths`] to a `Vec<BezPath>`; prefer
//!    [`path_from_d`] for raw `d=""` strings, and the `rect_path` /
//!    `circle_path` / `line_path` helpers for the non-path primitives.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill};

/// Stable identifier for a lucide glyph. New entries map 1:1 to lucide
/// icon kebab names (e.g. `Icon::LayoutGrid` → `layout-grid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    LayoutGrid,
    Settings2,
    Wifi,
    Bluetooth,
    Monitor,
    Battery,
    Shield,
    Maximize2,
    Minimize2,
    X,
    ChevronUp,
    ChevronDown,
    ChevronLeft,
    Plus,
    Search,
    PenTool,
    Code,
    FileText,
    BarChart,
    Layers,
    Settings,
    Home,
    Camera,
    Power,
    // --- File-manager set (file types) ---
    Folder,
    FolderOpen,
    File,
    Image,
    Music,
    Film,
    Archive,
    FileCode,
    // --- File-manager set (actions / chrome) ---
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ChevronRight,
    RotateCw,
    Copy,
    Scissors,
    ClipboardPaste,
    Trash2,
    Pencil,
    FolderPlus,
    FilePlus,
    List,
    Star,
    HardDrive,
    Check,
    Info,
    CloudDownload,
    ArrowUpDown,
    // --- Extended core set ---
    Bell,
    Play,
    Pause,
    Refresh,
    ExternalLink,
    Trash,
    /// Horizontal "…" overflow / more-actions affordance.
    MoreHorizontal,
    // --- Mail set (folders / actions) ---
    Inbox,
    Send,
    Reply,
    Mail,
    Paperclip,
    Calendar,
    Ban,
    Download,
    // --- On-screen-keyboard set (action keys) ---
    /// Block up-arrow — the Shift key. Rendered filled for a latched
    /// Caps-Lock (see [`Icon::draw_filled`]).
    ArrowBigUp,
    /// Backspace / delete-left key glyph (a leftward tag with an ✕).
    Delete,
    /// Return / Enter key glyph (a down-then-left corner arrow).
    CornerDownLeft,
}

/// Lucide's fixed viewBox. Every stored path assumes this coordinate
/// system; [`draw_icon`] scales to the requested rect.
pub const LUCIDE_VIEWBOX: f64 = 24.0;

/// Lucide's default stroke width (in viewBox units) — 2.0 maps to
/// 1.5 px after the concept's Tailwind tweaks, so we scale by 0.75
/// when translating viewBox stroke width to output pixels. Call sites
/// can still override via the `stroke_px` argument of [`draw_icon`].
pub const LUCIDE_DEFAULT_STROKE_PX: f64 = 1.5;

/// Parse a lucide `d="…"` string into a [`BezPath`].
///
/// Panics if the supplied string is malformed. All paths shipped in
/// this module are unit-tested to parse into non-empty bez paths, so a
/// panic here flags a bad hand-edit rather than a runtime bug.
fn path_from_d(d: &str) -> BezPath {
    BezPath::from_svg(d).expect("lucide path literal must parse")
}

/// Build the stroked outline of a lucide `<rect>` primitive with an
/// optional corner radius (`rx`). Matches the SVG spec's "the edge is
/// stroked, not filled".
fn rect_path(x: f64, y: f64, w: f64, h: f64, rx: f64) -> BezPath {
    let rect = Rect::new(x, y, x + w, y + h);
    if rx > 0.0 {
        // `RoundedRect::into_path` yields the closed outline we want.
        RoundedRect::from_rect(rect, rx).path_elements(0.1).collect()
    } else {
        let mut p = BezPath::new();
        p.move_to(Point::new(rect.x0, rect.y0));
        p.line_to(Point::new(rect.x1, rect.y0));
        p.line_to(Point::new(rect.x1, rect.y1));
        p.line_to(Point::new(rect.x0, rect.y1));
        p.close_path();
        p
    }
}

/// Build the stroked outline of a lucide `<line>` primitive.
fn line_path(x1: f64, y1: f64, x2: f64, y2: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(Point::new(x1, y1));
    p.line_to(Point::new(x2, y2));
    p
}

/// Build the stroked outline of a lucide `<circle>` primitive.
fn circle_path(cx: f64, cy: f64, r: f64) -> BezPath {
    Circle::new(Point::new(cx, cy), r).path_elements(0.1).collect()
}

/// Return the set of strokable subpaths for `icon` in the lucide 24×24
/// viewBox. Widgets normally call [`draw_icon`] rather than inspecting
/// this directly; it's `pub(crate)` so unit tests can poke at the raw
/// shapes without duplicating the data table.
pub(crate) fn icon_subpaths(icon: Icon) -> Vec<BezPath> {
    match icon {
        // `__iconNode` is four 7×7 rects with rx=1 at the corners of a
        // 3/14 grid. We emit the rounded-rect outlines directly.
        Icon::LayoutGrid => vec![
            rect_path(3.0, 3.0, 7.0, 7.0, 1.0),
            rect_path(14.0, 3.0, 7.0, 7.0, 1.0),
            rect_path(14.0, 14.0, 7.0, 7.0, 1.0),
            rect_path(3.0, 14.0, 7.0, 7.0, 1.0),
        ],
        // Two horizontal rails + two ringed dots on opposite corners.
        Icon::Settings2 => vec![
            path_from_d("M14 17H5"),
            path_from_d("M19 7h-9"),
            circle_path(17.0, 17.0, 3.0),
            circle_path(7.0, 7.0, 3.0),
        ],
        // Three arcs + a dot at the base.
        Icon::Wifi => vec![
            path_from_d("M12 20h.01"),
            path_from_d("M2 8.82a15 15 0 0 1 20 0"),
            path_from_d("M5 12.859a10 10 0 0 1 14 0"),
            path_from_d("M8.5 16.429a5 5 0 0 1 7 0"),
        ],
        // Bluetooth: single zigzag path from lucide v0.546.0's
        // `bluetooth.js`. Two diagonal up/down strokes meeting in the
        // middle to form the classic Bluetooth runic glyph.
        Icon::Bluetooth => vec![path_from_d("m7 7 10 10-5 5V2l5 5L7 17")],
        // Screen rect + stand.
        Icon::Monitor => vec![
            rect_path(2.0, 3.0, 20.0, 14.0, 2.0),
            line_path(8.0, 21.0, 16.0, 21.0),
            line_path(12.0, 17.0, 12.0, 21.0),
        ],
        // Body rect + terminal nub.
        Icon::Battery => vec![
            path_from_d("M 22 14 L 22 10"),
            rect_path(2.0, 6.0, 16.0, 12.0, 2.0),
        ],
        // Classic heraldic shield outline (single path).
        Icon::Shield => vec![path_from_d(
            "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z",
        )],
        // Two opposing L-brackets + a diagonal in each.
        Icon::Maximize2 => vec![
            path_from_d("M15 3h6v6"),
            path_from_d("m21 3-7 7"),
            path_from_d("m3 21 7-7"),
            path_from_d("M9 21H3v-6"),
        ],
        Icon::Minimize2 => vec![
            path_from_d("m14 10 7-7"),
            path_from_d("M20 10h-6V4"),
            path_from_d("m3 21 7-7"),
            path_from_d("M4 14h6v6"),
        ],
        Icon::X => vec![
            path_from_d("M18 6 6 18"),
            path_from_d("m6 6 12 12"),
        ],
        Icon::ChevronUp => vec![path_from_d("m18 15-6-6-6 6")],
        Icon::ChevronDown => vec![path_from_d("m6 9 6 6 6-6")],
        Icon::ChevronLeft => vec![path_from_d("m15 18-6-6 6-6")],
        Icon::Plus => vec![
            path_from_d("M5 12h14"),
            path_from_d("M12 5v14"),
        ],
        // Magnifier: circle r=8 at (11, 11) + diagonal handle line
        // (21, 21) → (16.65, 16.65). Transcribed verbatim from
        // lucide's `search.js` (v0.546.0).
        Icon::Search => vec![
            circle_path(11.0, 11.0, 8.0),
            path_from_d("m21 21-4.3-4.3"),
        ],
        // Pen-tool: nib body + handle + diagonal + ink dot. Transcribed
        // from lucide's `pen-tool.js` (v0.546.0).
        Icon::PenTool => vec![
            path_from_d("M15.707 21.293a1 1 0 0 1-1.414 0l-1.586-1.586a1 1 0 0 1 0-1.414l5.586-5.586a1 1 0 0 1 1.414 0l1.586 1.586a1 1 0 0 1 0 1.414z"),
            path_from_d("m18 13-1.375-6.874a1 1 0 0 0-.746-.776L3.235 2.028a1 1 0 0 0-1.207 1.207L5.35 15.879a1 1 0 0 0 .776.746L13 18"),
            path_from_d("m2.3 2.3 7.286 7.286"),
            circle_path(11.0, 11.0, 2.0),
        ],
        // Code: two angle-brackets pointing inward, transcribed from
        // lucide's `code.js` (v0.546.0).
        Icon::Code => vec![
            path_from_d("m16 18 6-6-6-6"),
            path_from_d("m8 6-6 6 6 6"),
        ],
        // File-text: page outline + folded corner + three text lines.
        // Transcribed from lucide's `file-text.js` (v0.546.0).
        Icon::FileText => vec![
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
            path_from_d("M10 9H8"),
            path_from_d("M16 13H8"),
            path_from_d("M16 17H8"),
        ],
        // Bar-chart: lucide v0.546.0's `bar-chart` re-exports
        // `chart-no-axes-column-increasing` — three vertical bars at
        // x=5, 12, 19 with increasing height.
        Icon::BarChart => vec![
            path_from_d("M5 21v-6"),
            path_from_d("M12 21V9"),
            path_from_d("M19 21V3"),
        ],
        // Layers: three stacked diamond-ish quad outlines. Transcribed
        // from lucide's `layers.js` (v0.546.0).
        Icon::Layers => vec![
            path_from_d("M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z"),
            path_from_d("M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12"),
            path_from_d("M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17"),
        ],
        // Settings: 8-tooth gear ring + central r=3 dot. The gear ring
        // is one (long) lucide path; transcribed verbatim from
        // `settings.js` (v0.546.0).
        Icon::Settings => vec![
            path_from_d("M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"),
            circle_path(12.0, 12.0, 3.0),
        ],
        // Home: pitched-roof house outline + central doorway rect.
        // Transcribed from lucide's `house.js` (v0.546.0) — the icon
        // is exported under the `home` alias as well.
        Icon::Home => vec![
            path_from_d("M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"),
            path_from_d("M3 10a2 2 0 0 1 .709-1.528l7-6a2 2 0 0 1 2.582 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"),
        ],
        // Camera: body outline (with the lens-mount notch on top) plus
        // the central r=3 lens circle. Transcribed from lucide's
        // `camera.svg` (v0.469); the body path is one continuous loop
        // and the lens is a separate `<circle>` primitive that we route
        // through `circle_path` to match the existing convention.
        Icon::Camera => vec![
            path_from_d(
                "M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3z",
            ),
            circle_path(12.0, 13.0, 3.0),
        ],
        // Power: vertical stem from (12,2) to (12,12) and the broken
        // arc that wraps the body. Transcribed from lucide's
        // `power.svg` (v0.469).
        Icon::Power => vec![
            path_from_d("M12 2v10"),
            path_from_d("M18.4 6.6a9 9 0 1 1-12.77.04"),
        ],
        // --- File-manager set (file types) — lucide v0.546.0 ---
        // Folder: single tabbed-folder outline.
        Icon::Folder => vec![path_from_d(
            "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
        )],
        // Folder-open: angled open-lid folder.
        Icon::FolderOpen => vec![path_from_d(
            "m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2",
        )],
        // File: page outline + folded corner.
        Icon::File => vec![
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
        ],
        // Image: frame rect + lens dot + mountain diagonal.
        Icon::Image => vec![
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            circle_path(9.0, 9.0, 2.0),
            path_from_d("m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"),
        ],
        // Music: eighth-note stem + two note heads.
        Icon::Music => vec![
            path_from_d("M9 18V5l12-2v13"),
            circle_path(6.0, 18.0, 3.0),
            circle_path(18.0, 16.0, 3.0),
        ],
        // Film: sprocketed filmstrip — frame + interior rails.
        Icon::Film => vec![
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            path_from_d("M7 3v18"),
            path_from_d("M3 7.5h4"),
            path_from_d("M3 12h18"),
            path_from_d("M3 16.5h4"),
            path_from_d("M17 3v18"),
            path_from_d("M17 7.5h4"),
            path_from_d("M17 16.5h4"),
        ],
        // Archive: lid rect + body + handle slot.
        Icon::Archive => vec![
            rect_path(2.0, 3.0, 20.0, 5.0, 1.0),
            path_from_d("M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"),
            path_from_d("M10 12h4"),
        ],
        // File-code: page + folded corner + two angle brackets.
        Icon::FileCode => vec![
            path_from_d("M10 12.5 8 15l2 2.5"),
            path_from_d("m14 12.5 2 2.5-2 2.5"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7z"),
        ],
        // --- File-manager set (actions / chrome) — lucide v0.546.0 ---
        Icon::ArrowLeft => vec![
            path_from_d("m12 19-7-7 7-7"),
            path_from_d("M19 12H5"),
        ],
        Icon::ArrowRight => vec![
            path_from_d("M5 12h14"),
            path_from_d("m12 5 7 7-7 7"),
        ],
        Icon::ArrowUp => vec![
            path_from_d("m5 12 7-7 7 7"),
            path_from_d("M12 19V5"),
        ],
        Icon::ChevronRight => vec![path_from_d("m9 18 6-6-6-6")],
        // Rotate-cw: clockwise arc + arrowhead tick.
        Icon::RotateCw => vec![
            path_from_d("M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8"),
            path_from_d("M21 3v5h-5"),
        ],
        // Copy: foreground sheet rect + background sheet outline.
        // Star: single five-point star outline. Transcribed from
        // lucide's `star.js` (v0.546.0).
        Icon::Star => vec![path_from_d(
            "M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.85a.53.53 0 0 1-.77-.56l.881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 10.633a.53.53 0 0 1 .294-.904l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z",
        )],
        // Bell: dome body + the clapper arc at the base. Transcribed
        // from lucide's `bell.js` (v0.546.0).
        Icon::Bell => vec![
            path_from_d("M10.268 21a2 2 0 0 0 3.464 0"),
            path_from_d("M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"),
        ],
        // Play: single filled-look triangle traced as a stroked path so
        // it matches the lucide stroke aesthetic. From `play.js`.
        Icon::Play => vec![path_from_d(
            "M6 3a1 1 0 0 1 1.514-.857l13 7a1 1 0 0 1 0 1.714l-13 7A1 1 0 0 1 6 21z",
        )],
        // Pause: two rounded vertical bars. From lucide's `pause.js`.
        Icon::Pause => vec![
            rect_path(14.0, 4.0, 4.0, 16.0, 1.0),
            rect_path(6.0, 4.0, 4.0, 16.0, 1.0),
        ],
        // Check: single tick polyline. From lucide's `check.js`.
        Icon::Check => vec![path_from_d("M20 6 9 17l-5-5")],
        // Refresh / rotate-cw: two quarter-arc heads + the circular
        // sweep. Transcribed from lucide's `refresh-cw.js`.
        Icon::Refresh => vec![
            path_from_d("M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"),
            path_from_d("M21 3v5h-5"),
            path_from_d("M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"),
            path_from_d("M8 16H3v5"),
        ],
        // External-link: box (open corner) + the diagonal out-arrow.
        // Transcribed from lucide's `external-link.js`.
        Icon::ExternalLink => vec![
            path_from_d("M15 3h6v6"),
            path_from_d("M10 14 21 3"),
            path_from_d("M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"),
        ],
        // Trash (trash-2): lid line + can body + the two inner streaks.
        // Transcribed from lucide's `trash-2.js`.
        Icon::Trash => vec![
            path_from_d("M3 6h18"),
            path_from_d("M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"),
            line_path(10.0, 11.0, 10.0, 17.0),
            line_path(14.0, 11.0, 14.0, 17.0),
        ],
        // Copy: foreground rounded rect + the offset background corner.
        // Transcribed from lucide's `copy.js`.
        Icon::Copy => vec![
            rect_path(8.0, 8.0, 14.0, 14.0, 2.0),
            path_from_d("M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"),
        ],
        // Scissors: two finger rings + the crossed blades.
        Icon::Scissors => vec![
            circle_path(6.0, 6.0, 3.0),
            path_from_d("M8.12 8.12 12 12"),
            path_from_d("M20 4 8.12 15.88"),
            circle_path(6.0, 18.0, 3.0),
            path_from_d("M14.8 14.8 20 20"),
        ],
        // Clipboard-paste: board + clip + out-arrow.
        Icon::ClipboardPaste => vec![
            path_from_d("M11 14h10"),
            path_from_d("M16 4h2a2 2 0 0 1 2 2v1.344"),
            path_from_d("m17 18 4-4-4-4"),
            path_from_d("M8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 1.793-1.113"),
            rect_path(8.0, 2.0, 8.0, 4.0, 1.0),
        ],
        // Trash-2: two tine lines + can body + lid line + handle.
        Icon::Trash2 => vec![
            path_from_d("M10 11v6"),
            path_from_d("M14 11v6"),
            path_from_d("M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"),
            path_from_d("M3 6h18"),
            path_from_d("M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"),
        ],
        // Pencil: body + nib tick.
        Icon::Pencil => vec![
            path_from_d("M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"),
            path_from_d("m15 5 4 4"),
        ],
        // Folder-plus: plus strokes over a folder body.
        Icon::FolderPlus => vec![
            path_from_d("M12 10v6"),
            path_from_d("M9 13h6"),
            path_from_d("M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"),
        ],
        // File-plus: page + folded corner + plus strokes.
        Icon::FilePlus => vec![
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
            path_from_d("M9 15h6"),
            path_from_d("M12 18v-6"),
        ],
        // List: three bullet ticks + three rules.
        Icon::List => vec![
            path_from_d("M3 5h.01"),
            path_from_d("M3 12h.01"),
            path_from_d("M3 19h.01"),
            path_from_d("M8 5h13"),
            path_from_d("M8 12h13"),
            path_from_d("M8 19h13"),
        ],
        // Hard-drive: divider line + body + two indicator dots.
        Icon::HardDrive => vec![
            line_path(22.0, 12.0, 2.0, 12.0),
            path_from_d("M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"),
            line_path(6.0, 16.0, 6.01, 16.0),
            line_path(10.0, 16.0, 10.01, 16.0),
        ],
        // Info: ring + stem + dot.
        Icon::Info => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("M12 16v-4"),
            path_from_d("M12 8h.01"),
        ],
        // Cloud-download: down-arrow stem + arrowhead + cloud.
        Icon::CloudDownload => vec![
            path_from_d("M12 13v8l-4-4"),
            path_from_d("m12 21 4-4"),
            path_from_d("M4.393 15.269A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.436 8.284"),
        ],
        // Arrow-up-down: opposed arrows for sort direction.
        Icon::ArrowUpDown => vec![
            path_from_d("m21 16-4 4-4-4"),
            path_from_d("M17 20V4"),
            path_from_d("m3 8 4-4 4 4"),
            path_from_d("M7 4v16"),
        ],
        // More-horizontal: three dots. lucide draws these as r≈1
        // circles at x=12, x=19, x=5 on the centre line.
        Icon::MoreHorizontal => vec![
            circle_path(12.0, 12.0, 1.0),
            circle_path(19.0, 12.0, 1.0),
            circle_path(5.0, 12.0, 1.0),
        ],
        // --- Mail set — lucide v0.546.0 ---
        // Inbox: tray divider + body outline.
        Icon::Inbox => vec![
            path_from_d("M22 12h-6l-2 3h-4l-2-3H2"),
            path_from_d("M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"),
        ],
        // Send: paper-plane — outer triangle + inner fold line.
        Icon::Send => vec![
            path_from_d("M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z"),
            path_from_d("m21.854 2.147-10.94 10.939"),
        ],
        // Reply: return arrow.
        Icon::Reply => vec![
            path_from_d("M20 18v-2a4 4 0 0 0-4-4H4"),
            path_from_d("m9 17-5-5 5-5"),
        ],
        // Mail: envelope body rect + flap fold.
        Icon::Mail => vec![
            rect_path(2.0, 4.0, 20.0, 16.0, 2.0),
            path_from_d("m22 7-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 7"),
        ],
        // Paperclip: single hook path.
        Icon::Paperclip => vec![path_from_d(
            "M13.234 20.252 21 12.3a4.75 4.75 0 0 0-6.72-6.71l-8.94 8.94a2.375 2.375 0 0 0 3.36 3.36l7.09-7.09a1.188 1.188 0 0 0-1.68-1.68L7.5 15.7",
        )],
        // Calendar: two hanger ticks + body + header rule.
        Icon::Calendar => vec![
            path_from_d("M8 2v4"),
            path_from_d("M16 2v4"),
            rect_path(3.0, 4.0, 18.0, 18.0, 2.0),
            path_from_d("M3 10h18"),
        ],
        // Ban: ring + diagonal bar (no-entry / junk).
        Icon::Ban => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("m4.9 4.9 14.2 14.2"),
        ],
        // Download: stem + arrowhead + tray.
        Icon::Download => vec![
            path_from_d("M12 15V3"),
            path_from_d("m7 10 5 5 5-5"),
            path_from_d("M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"),
        ],
        // ArrowBigUp: a single closed block up-arrow — shaft x=9..15 under a
        // wide head (base y=12, apex at 12,5). One closed path so `draw_filled`
        // yields the solid caps-lock silhouette.
        Icon::ArrowBigUp => vec![path_from_d("M9 18v-6H5l7-7 7 7h-4v6z")],
        // Delete (backspace): the leftward tag body + the ✕ inside it.
        Icon::Delete => vec![
            path_from_d(
                "M10 5a2 2 0 0 0-1.344.519l-6.328 5.74a1 1 0 0 0 0 1.481l6.328 5.741A2 2 0 0 0 10 19h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2z",
            ),
            path_from_d("m12 9 6 6"),
            path_from_d("m18 9-6 6"),
        ],
        // CornerDownLeft (enter): the hook + the down-left arrowhead.
        Icon::CornerDownLeft => vec![
            path_from_d("M20 4v7a4 4 0 0 1-4 4H4"),
            path_from_d("M9 10 4 15l5 5"),
        ],
    }
}

/// Map a filesystem entry to the most appropriate [`Icon`].
///
/// Directories always resolve to [`Icon::Folder`]. Files are classified
/// by their (lower-cased) extension into the broad media families the
/// file-manager surfaces: images, audio, video, archives, source code,
/// and documents. Anything unrecognised falls back to [`Icon::File`].
///
/// This is the single source of truth for "what glyph represents this
/// path" across the toolkit (the file-picker, list/grid views, …) so
/// every consumer renders a consistent set without re-deriving the map.
pub fn icon_for_path(path: &std::path::Path, is_dir: bool) -> Icon {
    if is_dir {
        return Icon::Folder;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some(
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff"
            | "avif" | "heic",
        ) => Icon::Image,
        Some("mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "opus" | "wma") => {
            Icon::Music
        }
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "flv" | "m4v" | "mpeg" | "mpg") => {
            Icon::Film
        }
        Some(
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "lz" | "lzma",
        ) => Icon::Archive,
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cc" | "go"
            | "java" | "kt" | "rb" | "php" | "swift" | "sh" | "bash" | "fish" | "zsh" | "lua"
            | "toml" | "json" | "yaml" | "yml" | "xml" | "html" | "htm" | "css" | "scss" | "sql",
        ) => Icon::FileCode,
        Some("txt" | "md" | "markdown" | "rst" | "doc" | "docx" | "odt" | "rtf" | "pdf" | "tex") => {
            Icon::FileText
        }
        _ => Icon::File,
    }
}

impl Icon {
    /// Convenience: paint this glyph centred inside `rect`, stroked in
    /// `color` at the concept's default 1.5 px lucide width. Equivalent
    /// to [`draw_icon`] with `self` and [`LUCIDE_DEFAULT_STROKE_PX`].
    ///
    /// `transform` maps the icon's local (`rect`) coordinates into scene
    /// space — pass `Affine::IDENTITY` when `rect` is already absolute.
    pub fn draw(self, scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        draw_icon(scene, transform, rect, self, color, LUCIDE_DEFAULT_STROKE_PX);
    }

    /// Paint the icon as a **solid fill** rather than a stroked outline,
    /// centred within `rect`. Use for glyphs that read as a filled shape when
    /// "on" — a starred/favourited state, a selected pin — where the outline
    /// form is the "off" state. Only closed sub-paths fill meaningfully; the
    /// lucide `Star` (and similar single-loop glyphs) are authored closed, so
    /// they fill cleanly into the same silhouette the outline traces.
    pub fn draw_filled(self, scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        let size = rect.size();
        let min = size.width.min(size.height);
        if min <= 0.0 {
            return;
        }
        let scale = min / LUCIDE_VIEWBOX;
        let glyph = LUCIDE_VIEWBOX * scale;
        let tx = rect.x0 + (size.width - glyph) / 2.0;
        let ty = rect.y0 + (size.height - glyph) / 2.0;
        let local = transform * Affine::translate((tx, ty)) * Affine::scale(scale);
        for sub in icon_subpaths(self) {
            scene.fill(Fill::NonZero, local, color, None, &sub);
        }
    }

    /// Same as [`Icon::draw`] but with an explicit output stroke width in
    /// pixels — use when a call site needs a heavier or lighter glyph than
    /// the 1.5 px default (e.g. a large hero icon).
    pub fn draw_with_stroke(
        self,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        color: Color,
        stroke_px: f64,
    ) {
        draw_icon(scene, transform, rect, self, color, stroke_px);
    }
}

/// Paint `icon` stroked in `color` into `scene`, centred within `rect`.
///
/// * `scale_px` is derived from `rect.size().min() / LUCIDE_VIEWBOX` so
///   the icon always fits the shorter dimension; rectangular cells
///   pad the longer axis.
/// * `stroke_px` is the **output** stroke width in pixels; lucide
///   renders at 2 px on a 24 unit viewBox, the concept trims that to
///   1.5 px — pass [`LUCIDE_DEFAULT_STROKE_PX`] for parity or override
///   per call site. The stroke is divided by `scale_px` before being
///   handed to vello so the stroked path remains consistent across
///   icon sizes.
/// * Caps and joins are rounded, matching lucide defaults.
///
/// This function is intentionally `&Scene`-mutating only; it takes no
/// `&mut self` and touches no state outside `scene`.
pub fn draw_icon(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Icon,
    color: Color,
    stroke_px: f64,
) {
    let size = rect.size();
    let min = size.width.min(size.height);
    if min <= 0.0 {
        return;
    }
    let scale = min / LUCIDE_VIEWBOX;
    // Center the 24×24 glyph inside `rect` when the target is not
    // square. This is what lets a 14×14 icon slot next to 16×22 chrome
    // without a custom transform at each call site.
    let glyph_w = LUCIDE_VIEWBOX * scale;
    let glyph_h = LUCIDE_VIEWBOX * scale;
    let tx = rect.x0 + (size.width - glyph_w) / 2.0;
    let ty = rect.y0 + (size.height - glyph_h) / 2.0;
    let local = transform
        * Affine::translate((tx, ty))
        * Affine::scale(scale);

    let stroke = Stroke::new(stroke_px / scale)
        .with_caps(Cap::Round)
        .with_join(Join::Round);

    for sub in icon_subpaths(icon) {
        scene.stroke(&stroke, local, color, None, &sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a path's elements; returns the count so tests can assert
    /// "non-empty" without matching against internal enum shapes.
    fn element_count(p: &BezPath) -> usize {
        p.elements().iter().count()
    }

    #[test]
    fn icon_layout_grid_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::LayoutGrid);
        assert_eq!(sub.len(), 4, "layout-grid has 4 rect subpaths");
        for p in &sub {
            assert!(element_count(p) > 0, "subpath must emit path elements");
        }
    }

    #[test]
    fn icon_settings2_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::Settings2);
        assert_eq!(sub.len(), 4, "settings-2 has 2 paths + 2 circles");
        for p in &sub {
            assert!(element_count(p) > 0);
        }
    }

    #[test]
    fn icon_shield_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::Shield);
        assert_eq!(sub.len(), 1);
        // The shield path has multiple curve_to + line_to instructions.
        assert!(element_count(&sub[0]) >= 6, "shield path is multi-segment");
    }

    #[test]
    fn icon_wifi_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::Wifi);
        assert_eq!(sub.len(), 4, "wifi = 3 arcs + 1 dot path");
        for p in &sub {
            assert!(element_count(p) > 0);
        }
    }

    #[test]
    fn icon_chevron_up_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::ChevronUp);
        assert_eq!(sub.len(), 1);
        assert!(element_count(&sub[0]) >= 3, "chevron is M,L,L");
    }

    #[test]
    fn icon_plus_has_two_strokes() {
        let sub = icon_subpaths(Icon::Plus);
        assert_eq!(sub.len(), 2, "plus = horizontal + vertical stroke");
    }

    #[test]
    fn icon_search_parses_to_non_empty_bezpath() {
        // Lucide's `search` glyph = one circle (magnifier body) + one
        // diagonal line (handle). Both subpaths must be non-empty so
        // the concept-parity swap in tabs/window-group doesn't ship
        // blank chrome if the enum is accidentally reverted.
        let sub = icon_subpaths(Icon::Search);
        assert_eq!(sub.len(), 2, "search = circle + handle line");
        for p in &sub {
            assert!(element_count(p) > 0, "search subpath must emit elements");
        }
    }

    #[test]
    fn draw_icon_does_not_panic_on_zero_rect() {
        // The guard in draw_icon makes zero-size rects a no-op rather
        // than a divide-by-zero. Cheap regression check.
        let mut scene = Scene::new();
        draw_icon(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 0.0, 0.0),
            Icon::LayoutGrid,
            Color::WHITE,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    #[test]
    fn all_icons_produce_non_empty_subpaths() {
        // Smoke test over the whole enum so a typo in a new d="…"
        // literal is caught at `cargo test` rather than shipping as a
        // blank square in the shell.
        for icon in [
            Icon::LayoutGrid,
            Icon::Settings2,
            Icon::Wifi,
            Icon::Bluetooth,
            Icon::Monitor,
            Icon::Battery,
            Icon::Shield,
            Icon::Maximize2,
            Icon::Minimize2,
            Icon::X,
            Icon::ChevronUp,
            Icon::ChevronDown,
            Icon::ChevronLeft,
            Icon::Plus,
            Icon::Search,
            Icon::PenTool,
            Icon::Code,
            Icon::FileText,
            Icon::BarChart,
            Icon::Layers,
            Icon::Settings,
            Icon::Home,
            Icon::Camera,
            Icon::Power,
            Icon::Star,
            Icon::Bell,
            Icon::Play,
            Icon::Pause,
            Icon::Check,
            Icon::Refresh,
            Icon::ExternalLink,
            Icon::Trash,
            Icon::Copy,
            Icon::Folder,
            Icon::ChevronRight,
            Icon::MoreHorizontal,
            Icon::ArrowBigUp,
            Icon::Delete,
            Icon::CornerDownLeft,
        ] {
            let sub = icon_subpaths(icon);
            assert!(!sub.is_empty(), "{icon:?} produced no subpaths");
            for p in &sub {
                assert!(element_count(p) > 0, "{icon:?} emitted empty subpath");
            }
        }
    }

    /// `Icon::draw` is a thin convenience over `draw_icon` — it must emit
    /// the same stroked subpaths into the scene as the free function. We
    /// assert it produces at least one stroke (LINE/STROKE draw tag) for a
    /// multi-segment glyph rather than silently no-op-ing.
    #[test]
    fn icon_draw_method_emits_strokes() {
        use vello_encoding::DrawTag;
        let mut scene = Scene::new();
        Icon::Check.draw(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 24.0, 24.0),
            Color::WHITE,
        );
        // A stroked glyph emits at least one COLOR draw tag for its path.
        let color_tags = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(color_tags >= 1, "Icon::draw must stroke at least one subpath");
    }

    /// The extended core set must have the expected subpath counts so a
    /// future lucide-upstream restructure of any glyph trips here.
    #[test]
    fn extended_icon_subpath_counts() {
        assert_eq!(icon_subpaths(Icon::Star).len(), 1, "star = 1 outline");
        assert_eq!(icon_subpaths(Icon::Bell).len(), 2, "bell = dome + clapper");
        assert_eq!(icon_subpaths(Icon::Play).len(), 1, "play = 1 triangle path");
        assert_eq!(icon_subpaths(Icon::Pause).len(), 2, "pause = 2 bars");
        assert_eq!(icon_subpaths(Icon::Check).len(), 1, "check = 1 tick");
        assert_eq!(icon_subpaths(Icon::Refresh).len(), 4, "refresh = 2 arcs + 2 heads");
        assert_eq!(icon_subpaths(Icon::ExternalLink).len(), 3, "external-link = box + arrow + diag");
        assert_eq!(icon_subpaths(Icon::Trash).len(), 4, "trash = lid + body + 2 streaks");
        assert_eq!(icon_subpaths(Icon::Copy).len(), 2, "copy = fg rect + bg corner");
        assert_eq!(icon_subpaths(Icon::Folder).len(), 1, "folder = 1 outline");
        assert_eq!(icon_subpaths(Icon::ChevronRight).len(), 1, "chevron-right = 1 path");
        assert_eq!(icon_subpaths(Icon::MoreHorizontal).len(), 3, "more = 3 dots");
        assert_eq!(icon_subpaths(Icon::ArrowBigUp).len(), 1, "arrow-big-up = 1 closed arrow");
        assert_eq!(icon_subpaths(Icon::Delete).len(), 3, "delete = tag body + 2 ✕ strokes");
        assert_eq!(icon_subpaths(Icon::CornerDownLeft).len(), 2, "corner-down-left = hook + head");
    }

    /// The mail glyph set must transcribe into non-empty subpaths so the
    /// mail sidebar / reader / attachment chrome never ships a blank square.
    #[test]
    fn mail_icons_produce_non_empty_subpaths() {
        for icon in [
            Icon::Inbox,
            Icon::Send,
            Icon::Reply,
            Icon::Mail,
            Icon::Paperclip,
            Icon::Calendar,
            Icon::Ban,
            Icon::Download,
        ] {
            let sub = icon_subpaths(icon);
            assert!(!sub.is_empty(), "{icon:?} produced no subpaths");
            for p in &sub {
                assert!(element_count(p) > 0, "{icon:?} emitted empty subpath");
            }
        }
        assert_eq!(icon_subpaths(Icon::Inbox).len(), 2, "inbox = tray + body");
        assert_eq!(icon_subpaths(Icon::Reply).len(), 2, "reply = arm + arrow");
        assert_eq!(icon_subpaths(Icon::Mail).len(), 2, "mail = body + flap");
        assert_eq!(icon_subpaths(Icon::Calendar).len(), 4, "calendar = 2 ticks + body + rule");
        assert_eq!(icon_subpaths(Icon::Ban).len(), 2, "ban = ring + bar");
        assert_eq!(icon_subpaths(Icon::Download).len(), 3, "download = stem + head + tray");
    }

    #[test]
    fn icon_camera_parses_to_non_empty_bezpath() {
        // Lucide's `camera` glyph = body path with lens-mount notch +
        // r=3 lens circle. Both subpaths must be non-empty so the
        // QuickSettings action row's middle tile renders the camera
        // glyph instead of the placeholder ring.
        let sub = icon_subpaths(Icon::Camera);
        assert_eq!(sub.len(), 2, "camera = body path + lens circle");
        for p in &sub {
            assert!(element_count(p) > 0, "camera subpath must emit elements");
        }
    }

    #[test]
    fn icon_power_parses_to_non_empty_bezpath() {
        // Lucide's `power` glyph = vertical stem + broken arc. Two
        // strokable subpaths so the QuickSettings action row's right
        // tile renders the power glyph in red instead of a placeholder
        // ring.
        let sub = icon_subpaths(Icon::Power);
        assert_eq!(sub.len(), 2, "power = stem + arc");
        for p in &sub {
            assert!(element_count(p) > 0, "power subpath must emit elements");
        }
    }

    /// Per-spec subpath counts for the workspace-icon set added for the
    /// wheel-disc concept-parity polish. If lucide upstream restructures
    /// any of these glyphs, these asserts catch the regression.
    #[test]
    fn workspace_icon_subpath_counts() {
        assert_eq!(icon_subpaths(Icon::PenTool).len(), 4, "pen-tool = 3 paths + 1 circle");
        assert_eq!(icon_subpaths(Icon::Code).len(), 2, "code = 2 angle-bracket paths");
        assert_eq!(icon_subpaths(Icon::FileText).len(), 5, "file-text = page + corner + 3 lines");
        assert_eq!(icon_subpaths(Icon::BarChart).len(), 3, "bar-chart = 3 column paths");
        assert_eq!(icon_subpaths(Icon::Layers).len(), 3, "layers = 3 stacked plate paths");
        assert_eq!(icon_subpaths(Icon::Settings).len(), 2, "settings = gear path + dot");
        assert_eq!(icon_subpaths(Icon::Home).len(), 2, "home = roof + doorway paths");
        for icon in [
            Icon::PenTool,
            Icon::Code,
            Icon::FileText,
            Icon::BarChart,
            Icon::Layers,
            Icon::Settings,
            Icon::Home,
        ] {
            for p in icon_subpaths(icon) {
                assert!(element_count(&p) > 0, "{icon:?} subpath empty");
            }
        }
    }

    /// Every file-manager glyph must transcribe into non-empty subpaths
    /// so the picker / list / grid never ship a blank square.
    #[test]
    fn file_manager_icons_produce_non_empty_subpaths() {
        for icon in [
            Icon::Folder,
            Icon::FolderOpen,
            Icon::File,
            Icon::Image,
            Icon::Music,
            Icon::Film,
            Icon::Archive,
            Icon::FileCode,
            Icon::ArrowLeft,
            Icon::ArrowRight,
            Icon::ArrowUp,
            Icon::ChevronRight,
            Icon::RotateCw,
            Icon::Copy,
            Icon::Scissors,
            Icon::ClipboardPaste,
            Icon::Trash2,
            Icon::Pencil,
            Icon::FolderPlus,
            Icon::FilePlus,
            Icon::List,
            Icon::Star,
            Icon::HardDrive,
            Icon::Check,
            Icon::Info,
            Icon::CloudDownload,
            Icon::ArrowUpDown,
        ] {
            let sub = icon_subpaths(icon);
            assert!(!sub.is_empty(), "{icon:?} produced no subpaths");
            for p in &sub {
                assert!(element_count(p) > 0, "{icon:?} emitted empty subpath");
            }
        }
    }

    /// Spot-check the exact subpath counts of the more complex new
    /// glyphs so an upstream lucide restructure is caught at test time.
    #[test]
    fn file_manager_icon_subpath_counts() {
        assert_eq!(icon_subpaths(Icon::Folder).len(), 1);
        assert_eq!(icon_subpaths(Icon::File).len(), 2, "file = page + corner");
        assert_eq!(icon_subpaths(Icon::Image).len(), 3, "image = frame + lens + ridge");
        assert_eq!(icon_subpaths(Icon::Film).len(), 8, "film = frame + 7 rails");
        assert_eq!(icon_subpaths(Icon::Scissors).len(), 5, "scissors = 2 rings + 3 strokes");
        assert_eq!(icon_subpaths(Icon::Trash2).len(), 5, "trash = 2 tines + body + lid + handle");
        assert_eq!(icon_subpaths(Icon::HardDrive).len(), 4, "hard-drive = divider + body + 2 dots");
        assert_eq!(icon_subpaths(Icon::List).len(), 6, "list = 3 bullets + 3 rules");
    }

    #[test]
    fn icon_for_path_classifies_by_extension() {
        use std::path::Path;
        // Directories always fold to the folder glyph regardless of name.
        assert_eq!(icon_for_path(Path::new("/home/user/Pictures"), true), Icon::Folder);
        assert_eq!(icon_for_path(Path::new("/x/archive.zip"), true), Icon::Folder);
        // Media families.
        assert_eq!(icon_for_path(Path::new("a/photo.PNG"), false), Icon::Image, "case-insensitive");
        assert_eq!(icon_for_path(Path::new("song.flac"), false), Icon::Music);
        assert_eq!(icon_for_path(Path::new("clip.mkv"), false), Icon::Film);
        assert_eq!(icon_for_path(Path::new("bundle.tar.gz"), false), Icon::Archive);
        assert_eq!(icon_for_path(Path::new("main.rs"), false), Icon::FileCode);
        assert_eq!(icon_for_path(Path::new("notes.md"), false), Icon::FileText);
        assert_eq!(icon_for_path(Path::new("report.docx"), false), Icon::FileText);
        // Unknown / extension-less → generic file.
        assert_eq!(icon_for_path(Path::new("Makefile"), false), Icon::File);
        assert_eq!(icon_for_path(Path::new("blob.qwerty"), false), Icon::File);
    }
}

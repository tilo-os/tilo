//! Lucide-icon glyphs, stroked to match the concept's 1.5 px aesthetic.
//!
//! Each [`Icon`] variant resolves to one or more [`BezPath`] subpaths
//! traced from upstream lucide-react (v0.546.0, ISC licensed) in the
//! lucide 24×24 viewBox, scaled at draw time. Icons are *stroked*, not
//! filled — [`draw_icon`] mirrors that. Deliberately just a data table +
//! one draw function — no trait hierarchy, icon-theme system, or
//! per-icon caching.
//!
//! To add an icon: pull the `__iconNode` entries from
//! `tilo-concept/node_modules/lucide-react/dist/esm/icons/<kebab>.js`,
//! add a variant to [`Icon`], and map it in `icon_subpaths`.

mod app_image;

pub use app_image::{AppImage, AppImageSource, MAX_DEVICE_PX, device_px, draw_app_image};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill};

/// Stable identifier for a lucide glyph. New entries map 1:1 to lucide
/// icon kebab names (e.g. `Icon::LayoutGrid` → `layout-grid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    LayoutGrid,
    Settings2,
    Mouse,
    Keyboard,
    Users,
    Printer,
    Globe,
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
    Terminal,
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
    ArrowDown,
    ChevronRight,
    RotateCcw,
    RotateCw,
    Copy,
    Scissors,
    ClipboardPaste,
    Trash2,
    Pencil,
    Square,
    /// A window's minimize control.
    Minus,
    FolderPlus,
    FilePlus,
    List,
    Star,
    Pin,
    HardDrive,
    Check,
    // --- Video / media-navigation set ---
    Flame,
    GalleryVerticalEnd,
    History,
    Timer,
    ThumbsUp,
    ThumbsDown,
    ListVideo,
    PictureInPicture2,
    Sparkles,
    Eye,
    MousePointer2,
    Menu,
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
    // --- Quick-settings set ---
    Sun,
    Volume2,
    VolumeX,
    WifiOff,
    /// Lucide `ethernet-port` — the RJ45 socket outline reads as
    /// "cable" at 14 px where a plug icon does not.
    EthernetPort,
    Mic,
    MicOff,
    SkipForward,
    SkipBack,
    // --- On-screen-keyboard set (action keys) ---
    /// The Shift key. Rendered filled for a latched Caps-Lock (see
    /// [`Icon::draw_filled`]).
    ArrowBigUp,
    Delete,
    CornerDownLeft,
    // --- History set ---
    /// Undo — a leftward arrow curving back on itself. Lucide `undo-2`.
    Undo2,
    /// Redo — the mirror of [`Icon::Undo2`]. Lucide `redo-2`.
    Redo2,
    // --- Status set ---
    /// A warning: Lucide `triangle-alert`.
    TriangleAlert,
    // --- Window navigation set ---
    /// Show or hide the sidebar: a window with a rule near its leading
    /// edge. Lucide `panel-left`.
    PanelLeft,
    /// Show or hide the inspector: the mirror of [`Icon::PanelLeft`].
    /// Lucide `panel-right`.
    PanelRight,
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
    #[allow(
        clippy::expect_used,
        reason = "all embedded Lucide path literals are compile-time data covered by parsing tests"
    )]
    BezPath::from_svg(d).expect("lucide path literal must parse")
}

/// Build the stroked outline of a lucide `<rect>`, with optional corner radius.
fn rect_path(left: f64, top: f64, width: f64, height: f64, radius: f64) -> BezPath {
    let rect = Rect::new(left, top, left + width, top + height);
    if radius > 0.0 {
        RoundedRect::from_rect(rect, radius)
            .path_elements(0.1)
            .collect()
    } else {
        let mut path = BezPath::new();
        path.move_to(Point::new(rect.x0, rect.y0));
        path.line_to(Point::new(rect.x1, rect.y0));
        path.line_to(Point::new(rect.x1, rect.y1));
        path.line_to(Point::new(rect.x0, rect.y1));
        path.close_path();
        path
    }
}

fn line_path(x1: f64, y1: f64, x2: f64, y2: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(Point::new(x1, y1));
    p.line_to(Point::new(x2, y2));
    p
}

fn circle_path(cx: f64, cy: f64, r: f64) -> BezPath {
    Circle::new(Point::new(cx, cy), r)
        .path_elements(0.1)
        .collect()
}

/// The strokable subpaths for `icon` in the lucide 24×24 viewBox.
/// `pub(crate)` so unit tests can poke at the raw shapes without
/// duplicating the data table; widgets call [`draw_icon`] instead.
#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive Lucide icon-to-static-path table is intentionally kept in one match"
)]
pub(crate) fn icon_subpaths(icon: Icon) -> Vec<BezPath> {
    match icon {
        Icon::LayoutGrid => vec![
            rect_path(3.0, 3.0, 7.0, 7.0, 1.0),
            rect_path(14.0, 3.0, 7.0, 7.0, 1.0),
            rect_path(14.0, 14.0, 7.0, 7.0, 1.0),
            rect_path(3.0, 14.0, 7.0, 7.0, 1.0),
        ],
        Icon::Settings2 => vec![
            path_from_d("M14 17H5"),
            path_from_d("M19 7h-9"),
            circle_path(17.0, 17.0, 3.0),
            circle_path(7.0, 7.0, 3.0),
        ],
        Icon::Mouse => vec![
            rect_path(5.0, 2.0, 14.0, 20.0, 7.0),
            line_path(12.0, 6.0, 12.0, 10.0),
        ],
        Icon::Keyboard => vec![
            rect_path(2.0, 4.0, 20.0, 16.0, 2.0),
            path_from_d("M6 8h.01M10 8h.01M14 8h.01M18 8h.01"),
            path_from_d("M6 12h.01M10 12h.01M14 12h.01M18 12h.01"),
            line_path(8.0, 16.0, 16.0, 16.0),
        ],
        Icon::Users => vec![
            circle_path(9.0, 7.0, 4.0),
            path_from_d("M2 21v-2a6 6 0 0 1 6-6h2a6 6 0 0 1 6 6v2"),
            path_from_d("M16 3.13a4 4 0 0 1 0 7.75M22 21v-2a6 6 0 0 0-4.5-5.81"),
        ],
        Icon::Printer => vec![
            path_from_d("M6 9V2h12v7"),
            path_from_d(
                "M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2",
            ),
            rect_path(6.0, 14.0, 12.0, 8.0, 0.0),
        ],
        Icon::Globe => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("M2 12h20"),
            path_from_d("M12 2a15.3 15.3 0 0 1 0 20M12 2a15.3 15.3 0 0 0 0 20"),
        ],
        Icon::Wifi => vec![
            path_from_d("M12 20h.01"),
            path_from_d("M2 8.82a15 15 0 0 1 20 0"),
            path_from_d("M5 12.859a10 10 0 0 1 14 0"),
            path_from_d("M8.5 16.429a5 5 0 0 1 7 0"),
        ],
        Icon::Bluetooth => vec![path_from_d("m7 7 10 10-5 5V2l5 5L7 17")],
        Icon::Monitor => vec![
            rect_path(2.0, 3.0, 20.0, 14.0, 2.0),
            line_path(8.0, 21.0, 16.0, 21.0),
            line_path(12.0, 17.0, 12.0, 21.0),
        ],
        Icon::Battery => vec![
            path_from_d("M 22 14 L 22 10"),
            rect_path(2.0, 6.0, 16.0, 12.0, 2.0),
        ],
        Icon::Shield => vec![path_from_d(
            "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z",
        )],
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
        Icon::X => vec![path_from_d("M18 6 6 18"), path_from_d("m6 6 12 12")],
        Icon::ChevronUp => vec![path_from_d("m18 15-6-6-6 6")],
        Icon::ChevronDown => vec![path_from_d("m6 9 6 6 6-6")],
        Icon::ChevronLeft => vec![path_from_d("m15 18-6-6 6-6")],
        Icon::Plus => vec![path_from_d("M5 12h14"), path_from_d("M12 5v14")],
        Icon::Search => vec![circle_path(11.0, 11.0, 8.0), path_from_d("m21 21-4.3-4.3")],
        Icon::PenTool => vec![
            path_from_d(
                "M15.707 21.293a1 1 0 0 1-1.414 0l-1.586-1.586a1 1 0 0 1 0-1.414l5.586-5.586a1 1 0 0 1 1.414 0l1.586 1.586a1 1 0 0 1 0 1.414z",
            ),
            path_from_d(
                "m18 13-1.375-6.874a1 1 0 0 0-.746-.776L3.235 2.028a1 1 0 0 0-1.207 1.207L5.35 15.879a1 1 0 0 0 .776.746L13 18",
            ),
            path_from_d("m2.3 2.3 7.286 7.286"),
            circle_path(11.0, 11.0, 2.0),
        ],
        Icon::Code => vec![path_from_d("m16 18 6-6-6-6"), path_from_d("m8 6-6 6 6 6")],
        Icon::Terminal => vec![path_from_d("M12 19h8"), path_from_d("m4 17 6-6-6-6")],
        Icon::FileText => vec![
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
            path_from_d("M10 9H8"),
            path_from_d("M16 13H8"),
            path_from_d("M16 17H8"),
        ],
        Icon::BarChart => vec![
            path_from_d("M5 21v-6"),
            path_from_d("M12 21V9"),
            path_from_d("M19 21V3"),
        ],
        Icon::Layers => vec![
            path_from_d(
                "M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z",
            ),
            path_from_d(
                "M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12",
            ),
            path_from_d(
                "M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17",
            ),
        ],
        Icon::Settings => vec![
            path_from_d(
                "M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915",
            ),
            circle_path(12.0, 12.0, 3.0),
        ],
        Icon::Home => vec![
            path_from_d("M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"),
            path_from_d(
                "M3 10a2 2 0 0 1 .709-1.528l7-6a2 2 0 0 1 2.582 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
            ),
        ],
        Icon::Camera => vec![
            path_from_d(
                "M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3z",
            ),
            circle_path(12.0, 13.0, 3.0),
        ],
        Icon::Power => vec![
            path_from_d("M12 2v10"),
            path_from_d("M18.4 6.6a9 9 0 1 1-12.77.04"),
        ],
        // --- File-manager set (file types) — lucide v0.546.0 ---
        Icon::Folder => vec![path_from_d(
            "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
        )],
        Icon::FolderOpen => vec![path_from_d(
            "m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2",
        )],
        Icon::File => vec![
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
        ],
        Icon::Image => vec![
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            circle_path(9.0, 9.0, 2.0),
            path_from_d("m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"),
        ],
        Icon::Music => vec![
            path_from_d("M9 18V5l12-2v13"),
            circle_path(6.0, 18.0, 3.0),
            circle_path(18.0, 16.0, 3.0),
        ],
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
        Icon::Archive => vec![
            rect_path(2.0, 3.0, 20.0, 5.0, 1.0),
            path_from_d("M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"),
            path_from_d("M10 12h4"),
        ],
        Icon::FileCode => vec![
            path_from_d("M10 12.5 8 15l2 2.5"),
            path_from_d("m14 12.5 2 2.5-2 2.5"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7z"),
        ],
        // --- File-manager set (actions / chrome) — lucide v0.546.0 ---
        Icon::ArrowLeft => vec![path_from_d("m12 19-7-7 7-7"), path_from_d("M19 12H5")],
        Icon::ArrowRight => vec![path_from_d("M5 12h14"), path_from_d("m12 5 7 7-7 7")],
        Icon::ArrowUp => vec![path_from_d("m5 12 7-7 7 7"), path_from_d("M12 19V5")],
        Icon::ArrowDown => vec![path_from_d("m5 12 7 7 7-7"), path_from_d("M12 5v14")],
        Icon::ChevronRight => vec![path_from_d("m9 18 6-6-6-6")],
        Icon::RotateCcw => vec![
            path_from_d("M3 12a9 9 0 1 0 9-9c-2.52 0-4.93 1-6.74 2.74L3 8"),
            path_from_d("M3 3v5h5"),
        ],
        Icon::RotateCw => vec![
            path_from_d("M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8"),
            path_from_d("M21 3v5h-5"),
        ],
        Icon::Star => vec![path_from_d(
            "M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.85a.53.53 0 0 1-.77-.56l.881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 10.633a.53.53 0 0 1 .294-.904l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z",
        )],
        Icon::Pin => vec![
            path_from_d("M12 17v5"),
            path_from_d("M5 17h14"),
            path_from_d("M5 3h14"),
            path_from_d("M6 3v8l-2 6h16l-2-6V3"),
        ],
        Icon::Sun => vec![
            path_from_d("M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z"),
            path_from_d("M12 2v2"),
            path_from_d("M12 20v2"),
            path_from_d("m4.93 4.93 1.41 1.41"),
            path_from_d("m17.66 17.66 1.41 1.41"),
            path_from_d("M2 12h2"),
            path_from_d("M20 12h2"),
            path_from_d("m6.34 17.66-1.41 1.41"),
            path_from_d("m19.07 4.93-1.41 1.41"),
        ],
        Icon::Volume2 => vec![
            path_from_d(
                "M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z",
            ),
            path_from_d("M16 9a5 5 0 0 1 0 6"),
            path_from_d("M19.364 18.364a9 9 0 0 0 0-12.728"),
        ],
        Icon::VolumeX => vec![
            path_from_d(
                "M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z",
            ),
            line_path(22.0, 9.0, 16.0, 15.0),
            line_path(16.0, 9.0, 22.0, 15.0),
        ],
        Icon::WifiOff => vec![
            path_from_d("M12 20h.01"),
            path_from_d("M8.5 16.429a5 5 0 0 1 7 0"),
            path_from_d("M5 12.859a10 10 0 0 1 5.17-2.69"),
            path_from_d("M19 12.859a10 10 0 0 0-2.007-1.523"),
            path_from_d("M2 8.82a15 15 0 0 1 4.177-2.643"),
            path_from_d("M22 8.82a15 15 0 0 0-11.288-3.764"),
            path_from_d("m2 2 20 20"),
        ],
        Icon::EthernetPort => vec![
            path_from_d(
                "m15 20 3-3h2a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h2l3 3z",
            ),
            path_from_d("M6 8v1"),
            path_from_d("M10 8v1"),
            path_from_d("M14 8v1"),
            path_from_d("M18 8v1"),
        ],
        Icon::Mic => vec![
            path_from_d("M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"),
            path_from_d("M19 10v2a7 7 0 0 1-14 0v-2"),
            line_path(12.0, 19.0, 12.0, 22.0),
        ],
        Icon::MicOff => vec![
            line_path(2.0, 2.0, 22.0, 22.0),
            path_from_d("M18.89 13.23A7.12 7.12 0 0 0 19 12v-2"),
            path_from_d("M5 10v2a7 7 0 0 0 12 5"),
            path_from_d("M15 9.34V5a3 3 0 0 0-5.68-1.33"),
            path_from_d("M9 9v3a3 3 0 0 0 5.12 2.12"),
            line_path(12.0, 19.0, 12.0, 22.0),
        ],
        // Mirror images; a swap yields two glyphs that both look plausible
        // but mean the wrong thing. Forward's triangle points right
        // (apex at x=14, bar at x=21); back is the mirror.
        Icon::SkipForward => vec![
            path_from_d("M3 4a1 1 0 0 1 1.555-.832L14 9.5v5l-9.445 6.332A1 1 0 0 1 3 20z"),
            line_path(21.0, 4.0, 21.0, 20.0),
        ],
        Icon::SkipBack => vec![
            path_from_d("M21 4a1 1 0 0 0-1.555-.832L10 9.5v5l9.445 6.332A1 1 0 0 0 21 20z"),
            line_path(3.0, 4.0, 3.0, 20.0),
        ],
        Icon::Bell => vec![
            path_from_d("M10.268 21a2 2 0 0 0 3.464 0"),
            path_from_d(
                "M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326",
            ),
        ],
        Icon::Play => vec![path_from_d(
            "M6 3a1 1 0 0 1 1.514-.857l13 7a1 1 0 0 1 0 1.714l-13 7A1 1 0 0 1 6 21z",
        )],
        Icon::Pause => vec![
            rect_path(14.0, 4.0, 4.0, 16.0, 1.0),
            rect_path(6.0, 4.0, 4.0, 16.0, 1.0),
        ],
        Icon::Check => vec![path_from_d("M20 6 9 17l-5-5")],
        // --- Video / media-navigation set — lucide v0.546.0 ---
        Icon::Flame => vec![path_from_d(
            "M12 3q1 4 4 6.5t3 5.5a1 1 0 0 1-14 0 5 5 0 0 1 1-3 1 1 0 0 0 5 0c0-2-1.5-3-1.5-5q0-2 2.5-4",
        )],
        Icon::GalleryVerticalEnd => vec![
            path_from_d("M7 2h10"),
            path_from_d("M5 6h14"),
            rect_path(3.0, 10.0, 18.0, 12.0, 2.0),
        ],
        Icon::History => vec![
            path_from_d("M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"),
            path_from_d("M3 3v5h5"),
            path_from_d("M12 7v5l4 2"),
        ],
        Icon::Timer => vec![
            path_from_d("M10 2h4"),
            path_from_d("M12 14v-4"),
            path_from_d("M4 13a8 8 0 1 0 8-8 9.7 9.7 0 0 0-3.24.55"),
        ],
        Icon::ThumbsUp => vec![
            path_from_d("M7 10v12"),
            path_from_d(
                "M15 5.88 14 10h5.83a2 2 0 0 1 1.92 2.56l-2.33 8A2 2 0 0 1 17.5 22H4a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2h2.76a2 2 0 0 0 1.79-1.11L12 2a3.13 3.13 0 0 1 3 3.88Z",
            ),
        ],
        Icon::ThumbsDown => vec![
            path_from_d("M17 14V2"),
            path_from_d(
                "M9 18.12 10 14H4.17a2 2 0 0 1-1.92-2.56l2.33-8A2 2 0 0 1 6.5 2H20a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2.76a2 2 0 0 0-1.79 1.11L12 22a3.13 3.13 0 0 1-3-3.88Z",
            ),
        ],
        Icon::ListVideo => vec![
            path_from_d("M21 5H3"),
            path_from_d("M10 12H3"),
            path_from_d("M10 19H3"),
            path_from_d(
                "M15 12.003a1 1 0 0 1 1.517-.859l4.997 2.997a1 1 0 0 1 0 1.718l-4.997 2.997a1 1 0 0 1-1.517-.86z",
            ),
        ],
        Icon::PictureInPicture2 => vec![
            path_from_d("M21 9V7a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h4"),
            rect_path(12.0, 13.0, 10.0, 7.0, 2.0),
        ],
        Icon::Sparkles => vec![
            path_from_d(
                "M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594z",
            ),
            path_from_d("M20 2v4"),
            path_from_d("M22 4h-4"),
            circle_path(4.0, 20.0, 2.0),
        ],
        Icon::Eye => vec![
            path_from_d(
                "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0",
            ),
            circle_path(12.0, 12.0, 3.0),
        ],
        Icon::MousePointer2 => vec![path_from_d(
            "M4.037 2.927a1 1 0 0 1 1.086-.217l15.323 7.684a1 1 0 0 1-.14 1.838l-6.686 2.39a2 2 0 0 0-1.198 1.198l-2.39 6.686a1 1 0 0 1-1.838.14z",
        )],
        Icon::Menu => vec![
            path_from_d("M4 5h16"),
            path_from_d("M4 12h16"),
            path_from_d("M4 19h16"),
        ],
        Icon::Refresh => vec![
            path_from_d("M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"),
            path_from_d("M21 3v5h-5"),
            path_from_d("M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"),
            path_from_d("M8 16H3v5"),
        ],
        Icon::ExternalLink => vec![
            path_from_d("M15 3h6v6"),
            path_from_d("M10 14 21 3"),
            path_from_d("M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"),
        ],
        Icon::Trash => vec![
            path_from_d("M3 6h18"),
            path_from_d(
                "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2",
            ),
            line_path(10.0, 11.0, 10.0, 17.0),
            line_path(14.0, 11.0, 14.0, 17.0),
        ],
        Icon::Copy => vec![
            rect_path(8.0, 8.0, 14.0, 14.0, 2.0),
            path_from_d("M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"),
        ],
        Icon::Scissors => vec![
            circle_path(6.0, 6.0, 3.0),
            path_from_d("M8.12 8.12 12 12"),
            path_from_d("M20 4 8.12 15.88"),
            circle_path(6.0, 18.0, 3.0),
            path_from_d("M14.8 14.8 20 20"),
        ],
        Icon::ClipboardPaste => vec![
            path_from_d("M11 14h10"),
            path_from_d("M16 4h2a2 2 0 0 1 2 2v1.344"),
            path_from_d("m17 18 4-4-4-4"),
            path_from_d("M8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 1.793-1.113"),
            rect_path(8.0, 2.0, 8.0, 4.0, 1.0),
        ],
        Icon::Trash2 => vec![
            path_from_d("M10 11v6"),
            path_from_d("M14 11v6"),
            path_from_d("M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"),
            path_from_d("M3 6h18"),
            path_from_d("M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"),
        ],
        Icon::Pencil => vec![
            path_from_d(
                "M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z",
            ),
            path_from_d("m15 5 4 4"),
        ],
        Icon::Square => vec![rect_path(3.0, 3.0, 18.0, 18.0, 2.0)],
        Icon::Minus => vec![path_from_d("M5 12h14")],
        Icon::FolderPlus => vec![
            path_from_d("M12 10v6"),
            path_from_d("M9 13h6"),
            path_from_d(
                "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
            ),
        ],
        Icon::FilePlus => vec![
            path_from_d("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"),
            path_from_d("M14 2v4a2 2 0 0 0 2 2h4"),
            path_from_d("M9 15h6"),
            path_from_d("M12 18v-6"),
        ],
        Icon::List => vec![
            path_from_d("M3 5h.01"),
            path_from_d("M3 12h.01"),
            path_from_d("M3 19h.01"),
            path_from_d("M8 5h13"),
            path_from_d("M8 12h13"),
            path_from_d("M8 19h13"),
        ],
        Icon::HardDrive => vec![
            line_path(22.0, 12.0, 2.0, 12.0),
            path_from_d(
                "M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z",
            ),
            line_path(6.0, 16.0, 6.01, 16.0),
            line_path(10.0, 16.0, 10.01, 16.0),
        ],
        Icon::Info => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("M12 16v-4"),
            path_from_d("M12 8h.01"),
        ],
        Icon::CloudDownload => vec![
            path_from_d("M12 13v8l-4-4"),
            path_from_d("m12 21 4-4"),
            path_from_d("M4.393 15.269A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.436 8.284"),
        ],
        Icon::ArrowUpDown => vec![
            path_from_d("m21 16-4 4-4-4"),
            path_from_d("M17 20V4"),
            path_from_d("m3 8 4-4 4 4"),
            path_from_d("M7 4v16"),
        ],
        Icon::MoreHorizontal => vec![
            circle_path(12.0, 12.0, 1.0),
            circle_path(19.0, 12.0, 1.0),
            circle_path(5.0, 12.0, 1.0),
        ],
        // --- Mail set — lucide v0.546.0 ---
        Icon::Inbox => vec![
            path_from_d("M22 12h-6l-2 3h-4l-2-3H2"),
            path_from_d(
                "M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z",
            ),
        ],
        Icon::Send => vec![
            path_from_d(
                "M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z",
            ),
            path_from_d("m21.854 2.147-10.94 10.939"),
        ],
        Icon::Reply => vec![
            path_from_d("M20 18v-2a4 4 0 0 0-4-4H4"),
            path_from_d("m9 17-5-5 5-5"),
        ],
        Icon::Mail => vec![
            rect_path(2.0, 4.0, 20.0, 16.0, 2.0),
            path_from_d("m22 7-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 7"),
        ],
        Icon::Paperclip => vec![path_from_d(
            "M13.234 20.252 21 12.3a4.75 4.75 0 0 0-6.72-6.71l-8.94 8.94a2.375 2.375 0 0 0 3.36 3.36l7.09-7.09a1.188 1.188 0 0 0-1.68-1.68L7.5 15.7",
        )],
        Icon::Calendar => vec![
            path_from_d("M8 2v4"),
            path_from_d("M16 2v4"),
            rect_path(3.0, 4.0, 18.0, 18.0, 2.0),
            path_from_d("M3 10h18"),
        ],
        Icon::Ban => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("m4.9 4.9 14.2 14.2"),
        ],
        Icon::Download => vec![
            path_from_d("M12 15V3"),
            path_from_d("m7 10 5 5 5-5"),
            path_from_d("M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"),
        ],
        // One closed path so `draw_filled` yields a solid silhouette.
        Icon::ArrowBigUp => vec![path_from_d("M9 18v-6H5l7-7 7 7h-4v6z")],
        Icon::Delete => vec![
            path_from_d(
                "M10 5a2 2 0 0 0-1.344.519l-6.328 5.74a1 1 0 0 0 0 1.481l6.328 5.741A2 2 0 0 0 10 19h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2z",
            ),
            path_from_d("m12 9 6 6"),
            path_from_d("m18 9-6 6"),
        ],
        Icon::CornerDownLeft => vec![
            path_from_d("M20 4v7a4 4 0 0 1-4 4H4"),
            path_from_d("M9 10 4 15l5 5"),
        ],
        // Undo2: the left-pointing arrowhead + the loop that doubles back.
        Icon::Undo2 => vec![
            path_from_d("M9 14 4 9l5-5"),
            path_from_d("M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5 5.5 5.5 0 0 1-5.5 5.5H11"),
        ],
        // Redo2: Undo2 mirrored — arrowhead on the right, loop to the left.
        Icon::Redo2 => vec![
            path_from_d("m15 14 5-5-5-5"),
            path_from_d("M20 9H9.5A5.5 5.5 0 0 0 4 14.5 5.5 5.5 0 0 0 9.5 20H13"),
        ],
        Icon::TriangleAlert => vec![
            path_from_d("m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"),
            path_from_d("M12 9v4"),
            path_from_d("M12 17h.01"),
        ],
        Icon::PanelLeft => vec![rect_path(3.0, 3.0, 18.0, 18.0, 2.0), path_from_d("M9 3v18")],
        Icon::PanelRight => vec![
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            path_from_d("M15 3v18"),
        ],
    }
}

/// Map a filesystem entry to the most appropriate [`Icon`]. Directories
/// always resolve to [`Icon::Folder`]; files are classified by
/// (lower-cased) extension, falling back to [`Icon::File`].
///
/// Single source of truth for "what glyph represents this path" across
/// the toolkit, so every consumer renders a consistent set.
pub fn icon_for_path(path: &std::path::Path, is_dir: bool) -> Icon {
    if is_dir {
        return Icon::Folder;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
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
        Some(
            "txt" | "md" | "markdown" | "rst" | "doc" | "docx" | "odt" | "rtf" | "pdf" | "tex",
        ) => Icon::FileText,
        _ => Icon::File,
    }
}

impl Icon {
    /// Paint this glyph centred inside `rect`, stroked in `color` at the
    /// concept's default 1.5 px lucide width — equivalent to [`draw_icon`]
    /// with `self` and [`LUCIDE_DEFAULT_STROKE_PX`]. Pass `Affine::IDENTITY`
    /// for `transform` when `rect` is already in scene space.
    pub fn draw(self, scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        draw_icon(
            scene,
            transform,
            rect,
            self,
            color,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// Paint the icon as a **solid fill** rather than a stroked outline —
    /// for glyphs that read as filled when "on" (starred, a selected pin).
    /// Only closed sub-paths fill meaningfully; `Star` and similar
    /// single-loop glyphs are authored closed for exactly this.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
    )]
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
/// The icon is scaled from `rect.size().min() / LUCIDE_VIEWBOX` so it
/// always fits the shorter dimension, padding the longer axis.
///
/// `stroke_px` is the **output** width in pixels — lucide renders at
/// 2 px on a 24-unit viewBox, trimmed here to 1.5 px by default
/// ([`LUCIDE_DEFAULT_STROKE_PX`]); it's divided by the icon's scale
/// before handing to vello so stroke width stays consistent across sizes.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
)]
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
    let glyph_w = LUCIDE_VIEWBOX * scale;
    let glyph_h = LUCIDE_VIEWBOX * scale;
    let tx = rect.x0 + (size.width - glyph_w) / 2.0;
    let ty = rect.y0 + (size.height - glyph_h) / 2.0;
    let local = transform * Affine::translate((tx, ty)) * Affine::scale(scale);

    let stroke = Stroke::new(stroke_px / scale)
        .with_caps(Cap::Round)
        .with_join(Join::Round);

    for sub in icon_subpaths(icon) {
        scene.stroke(&stroke, local, color, None, &sub);
    }
}

/// [`draw_icon`] with a per-subpath colour. Exists for glyphs whose
/// subpaths mean different things — the Wi-Fi arcs are a *scale*, not a
/// picture, so a two-bar signal should paint two arcs lit and two dim
/// rather than the whole glyph at one alpha (drawing four nested icons
/// at four sizes would not line up; they must come from one path table).
///
/// `color_for` receives the subpath index in the order
/// `icon_subpaths` returns them (for [`Icon::Wifi`]: base dot, outer
/// arc, middle arc, inner arc).
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
)]
pub fn draw_icon_layered(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Icon,
    stroke_px: f64,
    mut color_for: impl FnMut(usize) -> Color,
) {
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
    let stroke = Stroke::new(stroke_px / scale)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    for (i, sub) in icon_subpaths(icon).into_iter().enumerate() {
        scene.stroke(&stroke, local, color_for(i), None, &sub);
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
    fn individual_icons_parse_to_their_expected_non_empty_subpaths() {
        // (icon, subpath count, minimum path elements per subpath, why)
        let cases = [
            (
                Icon::LayoutGrid,
                4,
                Some(1),
                "layout-grid has 4 rect subpaths",
            ),
            (
                Icon::Settings2,
                4,
                Some(1),
                "settings-2 has 2 paths + 2 circles",
            ),
            (Icon::Shield, 1, Some(6), "shield path is multi-segment"),
            (Icon::Wifi, 4, Some(1), "wifi = 3 arcs + 1 dot path"),
            (Icon::ChevronUp, 1, Some(3), "chevron is M,L,L"),
            (Icon::Plus, 2, None, "plus = horizontal + vertical stroke"),
            (Icon::Search, 2, Some(1), "search = circle + handle line"),
            (Icon::Camera, 2, Some(1), "camera = body path + lens circle"),
            (Icon::Power, 2, Some(1), "power = stem + arc"),
        ];
        for (icon, count, min_elements, why) in cases {
            let sub = icon_subpaths(icon);
            assert_eq!(sub.len(), count, "{icon:?}: {why}");
            if let Some(min) = min_elements {
                for p in &sub {
                    assert!(
                        element_count(p) >= min,
                        "{icon:?}: subpath must emit at least {min} path elements ({why})"
                    );
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn icon_families_have_their_lucide_subpath_counts() {
        // phase: status_tray_icons_have_their_expected_subpaths
        // Wi-Fi's subpaths are addressed *by index* to show signal strength
        // (see [`draw_icon_layered`]) — a reordering upstream would silently
        // light the wrong bars rather than fail to compile.
        {
            assert_eq!(
                icon_subpaths(Icon::Wifi).len(),
                4,
                "wifi = base dot + 3 arcs, outermost first — the tray indexes these"
            );
            assert_eq!(icon_subpaths(Icon::WifiOff).len(), 7);
            assert_eq!(
                icon_subpaths(Icon::VolumeX).len(),
                3,
                "cone + 2 cross lines"
            );
            assert_eq!(
                icon_subpaths(Icon::EthernetPort).len(),
                5,
                "socket outline + 4 pin nubs"
            );
            assert_eq!(icon_subpaths(Icon::MicOff).len(), 6);
            assert_eq!(icon_subpaths(Icon::SkipForward).len(), 2, "triangle + bar");
            assert_eq!(icon_subpaths(Icon::SkipBack).len(), 2, "triangle + bar");
        }
        // phase: extended_icon_subpath_counts
        {
            assert_eq!(icon_subpaths(Icon::Star).len(), 1, "star = 1 outline");
            assert_eq!(icon_subpaths(Icon::Bell).len(), 2, "bell = dome + clapper");
            assert_eq!(icon_subpaths(Icon::Play).len(), 1, "play = 1 triangle path");
            assert_eq!(icon_subpaths(Icon::Pause).len(), 2, "pause = 2 bars");
            assert_eq!(icon_subpaths(Icon::Check).len(), 1, "check = 1 tick");
            assert_eq!(
                icon_subpaths(Icon::Refresh).len(),
                4,
                "refresh = 2 arcs + 2 heads"
            );
            assert_eq!(
                icon_subpaths(Icon::ExternalLink).len(),
                3,
                "external-link = box + arrow + diag"
            );
            assert_eq!(
                icon_subpaths(Icon::Trash).len(),
                4,
                "trash = lid + body + 2 streaks"
            );
            assert_eq!(
                icon_subpaths(Icon::Copy).len(),
                2,
                "copy = fg rect + bg corner"
            );
            assert_eq!(icon_subpaths(Icon::Folder).len(), 1, "folder = 1 outline");
            assert_eq!(
                icon_subpaths(Icon::ChevronRight).len(),
                1,
                "chevron-right = 1 path"
            );
            assert_eq!(
                icon_subpaths(Icon::MoreHorizontal).len(),
                3,
                "more = 3 dots"
            );
            assert_eq!(
                icon_subpaths(Icon::ArrowBigUp).len(),
                1,
                "arrow-big-up = 1 closed arrow"
            );
            assert_eq!(
                icon_subpaths(Icon::Delete).len(),
                3,
                "delete = tag body + 2 ✕ strokes"
            );
            assert_eq!(
                icon_subpaths(Icon::CornerDownLeft).len(),
                2,
                "corner-down-left = hook + head"
            );
        }
        // phase: workspace_icon_subpath_counts
        {
            assert_eq!(
                icon_subpaths(Icon::PenTool).len(),
                4,
                "pen-tool = 3 paths + 1 circle"
            );
            assert_eq!(
                icon_subpaths(Icon::Code).len(),
                2,
                "code = 2 angle-bracket paths"
            );
            assert_eq!(
                icon_subpaths(Icon::FileText).len(),
                5,
                "file-text = page + corner + 3 lines"
            );
            assert_eq!(
                icon_subpaths(Icon::BarChart).len(),
                3,
                "bar-chart = 3 column paths"
            );
            assert_eq!(
                icon_subpaths(Icon::Layers).len(),
                3,
                "layers = 3 stacked plate paths"
            );
            assert_eq!(
                icon_subpaths(Icon::Settings).len(),
                2,
                "settings = gear path + dot"
            );
            assert_eq!(
                icon_subpaths(Icon::Home).len(),
                2,
                "home = roof + doorway paths"
            );
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
        // phase: video_navigation_icons_match_lucide_subpath_counts
        {
            for (icon, expected) in [
                (Icon::Flame, 1),
                (Icon::GalleryVerticalEnd, 3),
                (Icon::History, 3),
                (Icon::ThumbsUp, 2),
                (Icon::ThumbsDown, 2),
                (Icon::ListVideo, 4),
                (Icon::Sparkles, 4),
                (Icon::Eye, 2),
                (Icon::Menu, 3),
            ] {
                let subpaths = icon_subpaths(icon);
                assert_eq!(subpaths.len(), expected, "{icon:?} subpath count");
                assert!(
                    subpaths.iter().all(|path| element_count(path) > 0),
                    "{icon:?} emitted an empty subpath"
                );
            }
        }
        // phase: file_manager_icon_subpath_counts
        {
            assert_eq!(icon_subpaths(Icon::Folder).len(), 1);
            assert_eq!(icon_subpaths(Icon::File).len(), 2, "file = page + corner");
            assert_eq!(
                icon_subpaths(Icon::Image).len(),
                3,
                "image = frame + lens + ridge"
            );
            assert_eq!(icon_subpaths(Icon::Film).len(), 8, "film = frame + 7 rails");
            assert_eq!(
                icon_subpaths(Icon::Scissors).len(),
                5,
                "scissors = 2 rings + 3 strokes"
            );
            assert_eq!(
                icon_subpaths(Icon::Trash2).len(),
                5,
                "trash = 2 tines + body + lid + handle"
            );
            assert_eq!(
                icon_subpaths(Icon::HardDrive).len(),
                4,
                "hard-drive = divider + body + 2 dots"
            );
            assert_eq!(
                icon_subpaths(Icon::List).len(),
                6,
                "list = 3 bullets + 3 rules"
            );
        }
    }

    #[test]
    fn icons_draw_every_subpath_as_strokes_pointing_the_named_way() {
        // phase: draw_icon_layered_paints_every_subpath
        {
            let mut scene = Scene::new();
            let mut seen = Vec::new();
            draw_icon_layered(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 16.0, 16.0),
                Icon::Wifi,
                1.5,
                |i| {
                    seen.push(i);
                    Color::WHITE
                },
            );
            assert_eq!(seen, vec![0, 1, 2, 3]);
        }
        // phase: draw_icon_does_not_panic_on_zero_rect
        {
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
        // phase: icon_draw_method_emits_strokes
        {
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
            assert!(
                color_tags >= 1,
                "Icon::draw must stroke at least one subpath"
            );
        }
        // phase: the_skip_glyphs_point_the_way_they_are_named
        // The two skip glyphs are mirror images, so a swap produces two icons
        // that each look fine alone and both point the wrong way. Pin the
        // direction geometrically: the bar sits on the side the triangle points at.
        {
            let bar_x = |icon: Icon| {
                let bar = &icon_subpaths(icon)[1];
                let mut xs: Vec<f64> = Vec::new();
                for el in bar.elements() {
                    if let vello::kurbo::PathEl::MoveTo(p) | vello::kurbo::PathEl::LineTo(p) = el {
                        xs.push(p.x);
                    }
                }
                xs.into_iter().fold(f64::MIN, f64::max)
            };
            assert!(
                bar_x(Icon::SkipForward) > 12.0,
                "skip-forward's bar is on the right, the side it points at"
            );
            assert!(
                bar_x(Icon::SkipBack) < 12.0,
                "skip-back's bar is on the left"
            );
            for icon in [
                Icon::WifiOff,
                Icon::VolumeX,
                Icon::EthernetPort,
                Icon::MicOff,
                Icon::SkipForward,
                Icon::SkipBack,
            ] {
                for p in icon_subpaths(icon) {
                    assert!(element_count(&p) > 0, "{icon:?} subpath must emit elements");
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn every_icon_family_produces_non_empty_subpaths() {
        // phase: all_icons_produce_non_empty_subpaths
        {
            for icon in [
                Icon::LayoutGrid,
                Icon::Settings2,
                Icon::Mouse,
                Icon::Keyboard,
                Icon::Users,
                Icon::Printer,
                Icon::Globe,
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
                Icon::Terminal,
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
                Icon::PictureInPicture2,
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
                Icon::Undo2,
                Icon::Redo2,
                Icon::TriangleAlert,
            ] {
                let sub = icon_subpaths(icon);
                assert!(!sub.is_empty(), "{icon:?} produced no subpaths");
                for p in &sub {
                    assert!(element_count(p) > 0, "{icon:?} emitted empty subpath");
                }
            }
        }
        // phase: mail_icons_produce_non_empty_subpaths
        {
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
            assert_eq!(
                icon_subpaths(Icon::Calendar).len(),
                4,
                "calendar = 2 ticks + body + rule"
            );
            assert_eq!(icon_subpaths(Icon::Ban).len(), 2, "ban = ring + bar");
            assert_eq!(
                icon_subpaths(Icon::Download).len(),
                3,
                "download = stem + head + tray"
            );
        }
        // phase: file_manager_icons_produce_non_empty_subpaths
        {
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
                Icon::RotateCcw,
                Icon::RotateCw,
                Icon::Copy,
                Icon::Scissors,
                Icon::ClipboardPaste,
                Icon::Trash2,
                Icon::Pencil,
                Icon::Square,
                Icon::FolderPlus,
                Icon::FilePlus,
                Icon::List,
                Icon::Star,
                Icon::Pin,
                Icon::HardDrive,
                Icon::Check,
                Icon::Info,
                Icon::CloudDownload,
                Icon::ArrowUpDown,
                Icon::PanelLeft,
                Icon::PanelRight,
            ] {
                let sub = icon_subpaths(icon);
                assert!(!sub.is_empty(), "{icon:?} produced no subpaths");
                for p in &sub {
                    assert!(element_count(p) > 0, "{icon:?} emitted empty subpath");
                }
            }
        }
    }

    #[test]
    fn icon_for_path_classifies_by_extension() {
        use std::path::Path;
        // Directories always fold to the folder glyph regardless of name.
        assert_eq!(
            icon_for_path(Path::new("/home/user/Pictures"), true),
            Icon::Folder
        );
        assert_eq!(
            icon_for_path(Path::new("/x/archive.zip"), true),
            Icon::Folder
        );
        // Media families.
        assert_eq!(
            icon_for_path(Path::new("a/photo.PNG"), false),
            Icon::Image,
            "case-insensitive"
        );
        assert_eq!(icon_for_path(Path::new("song.flac"), false), Icon::Music);
        assert_eq!(icon_for_path(Path::new("clip.mkv"), false), Icon::Film);
        assert_eq!(
            icon_for_path(Path::new("bundle.tar.gz"), false),
            Icon::Archive
        );
        assert_eq!(icon_for_path(Path::new("main.rs"), false), Icon::FileCode);
        assert_eq!(icon_for_path(Path::new("notes.md"), false), Icon::FileText);
        assert_eq!(
            icon_for_path(Path::new("report.docx"), false),
            Icon::FileText
        );
        // Unknown / extension-less → generic file.
        assert_eq!(icon_for_path(Path::new("Makefile"), false), Icon::File);
        assert_eq!(icon_for_path(Path::new("blob.qwerty"), false), Icon::File);
    }
}

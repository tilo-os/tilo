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
    // --- Quick-settings set ---
    /// Brightness. Lucide `sun`.
    Sun,
    /// Volume. Lucide `volume-2`.
    Volume2,
    /// Silenced audio — the speaker with an ✕ where the arcs were.
    /// Lucide `volume-x`.
    VolumeX,
    /// Wi-Fi with a strike through it: the radio is off or associated
    /// with nothing. Lucide `wifi-off`.
    WifiOff,
    /// A wired link. Lucide `ethernet-port` — the RJ45 socket outline,
    /// which reads as "cable" at 14 px where a plug does not.
    EthernetPort,
    /// Microphone with a strike through it, for the mic-mute key's OSD.
    /// Lucide `mic-off`.
    MicOff,
    /// Skip to the next track. Lucide `skip-forward`.
    SkipForward,
    /// Skip to the previous track. Lucide `skip-back`.
    SkipBack,
    // --- On-screen-keyboard set (action keys) ---
    /// Block up-arrow — the Shift key. Rendered filled for a latched
    /// Caps-Lock (see [`Icon::draw_filled`]).
    ArrowBigUp,
    /// Backspace / delete-left key glyph (a leftward tag with an ✕).
    Delete,
    /// Return / Enter key glyph (a down-then-left corner arrow).
    CornerDownLeft,
    // --- Camera / capture set ---
    /// Camera-flip / front-back toggle. Lucide `switch-camera`.
    SwitchCamera,
    /// Camera iris. Lucide `aperture`.
    Aperture,
    /// Countdown / self-timer. Lucide `timer`.
    Timer,
    /// Flash / lightning. Lucide `zap`.
    Zap,
    /// Framing / QR-scan brackets with a sweep line. Lucide `scan-line`.
    ScanLine,
    /// Adjustment sliders. Lucide `sliders-horizontal`.
    SlidersHorizontal,
    /// Rule-of-thirds overlay toggle. Lucide `grid-3x3`.
    Grid3x3,
    // --- People / sharing / places set ---
    /// A single person. Lucide `user`.
    User,
    /// Share graph (three linked nodes). Lucide `share-2`.
    Share2,
    /// Location pin. Lucide `map-pin`.
    MapPin,
    /// Telephone handset. Lucide `phone`.
    Phone,
    /// Chain link. Lucide `link`.
    Link,
    // --- Container / shape set ---
    /// A plain rounded square (also a "stop" affordance when filled).
    /// Lucide `square`.
    Square,
    /// A shipping box / package. Lucide `package`.
    Package,
    /// Command-line prompt. Lucide `terminal`.
    Terminal,
    // --- Media-transport / library set ---
    // (SkipBack, SkipForward and VolumeX already live in the OSK / quick-
    // settings sections above and are reused here.)
    /// Shuffle playback order. Lucide `shuffle`.
    Shuffle,
    /// Repeat the queue. Lucide `repeat`.
    Repeat,
    /// Repeat the current track. Lucide `repeat-1`.
    Repeat1,
    /// Favourite. Lucide `heart`.
    Heart,
    /// A track list. Lucide `list-music`.
    ListMusic,
    /// A numbered / play-queue list. Lucide `list-ordered`.
    ListOrdered,
    /// A vinyl disc / album. Lucide `disc-3`.
    Disc3,
    /// A group of people / artists. Lucide `users`.
    Users,
    // --- Feed / social / media-browser set ---
    /// Trending / hot. Lucide `flame`.
    Flame,
    /// Like / upvote. Lucide `thumbs-up`.
    ThumbsUp,
    /// Dislike / downvote. Lucide `thumbs-down`.
    ThumbsDown,
    /// Highlights / "for you". Lucide `sparkles`.
    Sparkles,
    /// Visibility / views. Lucide `eye`.
    Eye,
    /// Hamburger menu. Lucide `menu`.
    Menu,
    /// A video-subscriptions screen. Lucide `monitor-play`.
    MonitorPlay,
    /// Watch/recent history. Lucide `history`.
    History,
    /// A video playlist. Lucide `list-video`.
    ListVideo,
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
            path_from_d("m21 21-4.34-4.34"),
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
        // `camera.svg` (v0.546.0); the body path is one continuous loop
        // and the lens is a separate `<circle>` primitive that we route
        // through `circle_path` to match the existing convention.
        Icon::Camera => vec![
            path_from_d(
                "M13.997 4a2 2 0 0 1 1.76 1.05l.486.9A2 2 0 0 0 18.003 7H20a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h1.997a2 2 0 0 0 1.759-1.048l.489-.904A2 2 0 0 1 10.004 4z",
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
            "M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.01a.53.53 0 0 1-.77-.56l.881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 9.795a.53.53 0 0 1 .294-.906l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z",
        )],
        // Sun: centre disc plus eight rays. From lucide's `sun.js`.
        Icon::Sun => vec![
            path_from_d(
                "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z",
            ),
            path_from_d("M12 2v2"),
            path_from_d("M12 20v2"),
            path_from_d("m4.93 4.93 1.41 1.41"),
            path_from_d("m17.66 17.66 1.41 1.41"),
            path_from_d("M2 12h2"),
            path_from_d("M20 12h2"),
            path_from_d("m6.34 17.66-1.41 1.41"),
            path_from_d("m19.07 4.93-1.41 1.41"),
        ],
        // Volume2: speaker cone plus two arcs. From lucide's
        // `volume-2.js`.
        Icon::Volume2 => vec![
            path_from_d(
                "M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z",
            ),
            path_from_d("M16 9a5 5 0 0 1 0 6"),
            path_from_d("M19.364 18.364a9 9 0 0 0 0-12.728"),
        ],
        // VolumeX: the same speaker cone, arcs replaced by a cross.
        // From lucide's `volume-x.js`.
        Icon::VolumeX => vec![
            path_from_d(
                "M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z",
            ),
            line_path(22.0, 9.0, 16.0, 15.0),
            line_path(16.0, 9.0, 22.0, 15.0),
        ],
        // WifiOff: the same arc stack as `Wifi`, broken and struck
        // through. From lucide's `wifi-off.js`.
        Icon::WifiOff => vec![
            path_from_d("M12 20h.01"),
            path_from_d("M8.5 16.429a5 5 0 0 1 7 0"),
            path_from_d("M5 12.859a10 10 0 0 1 5.17-2.69"),
            path_from_d("M19 12.859a10 10 0 0 0-2.007-1.523"),
            path_from_d("M2 8.82a15 15 0 0 1 4.177-2.643"),
            path_from_d("M22 8.82a15 15 0 0 0-11.288-3.764"),
            path_from_d("m2 2 20 20"),
        ],
        // EthernetPort: RJ45 socket outline + the four pin nubs. From
        // lucide's `ethernet-port.js`.
        Icon::EthernetPort => vec![
            path_from_d(
                "m15 20 3-3h2a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h2l3 3z",
            ),
            path_from_d("M6 8v1"),
            path_from_d("M10 8v1"),
            path_from_d("M14 8v1"),
            path_from_d("M18 8v1"),
        ],
        // MicOff: mic capsule + stand, struck through. From lucide's
        // `mic-off.js`.
        Icon::MicOff => vec![
            line_path(2.0, 2.0, 22.0, 22.0),
            path_from_d("M18.89 13.23A7.12 7.12 0 0 0 19 12v-2"),
            path_from_d("M5 10v2a7 7 0 0 0 12 5"),
            path_from_d("M15 9.34V5a3 3 0 0 0-5.68-1.33"),
            path_from_d("M9 9v3a3 3 0 0 0 5.12 2.12"),
            line_path(12.0, 19.0, 12.0, 22.0),
        ],
        // SkipForward / SkipBack: triangle + end bar. From lucide's
        // `skip-forward.js` / `skip-back.js`.
        //
        // The two paths are mirror images and the bar sits on the side
        // the triangle points at, so swapping them yields two glyphs
        // that both look plausible and both mean the wrong thing —
        // which is exactly what shipped until an OSD capture showed
        // "Next track" over a leftward arrow. Read the path: forward's
        // triangle starts its sweep at x=3 and reaches its apex at
        // x=14, so it points right, and its bar is at x=21.
        Icon::SkipForward => vec![
            path_from_d("M3 4a1 1 0 0 1 1.555-.832L14 9.5v5l-9.445 6.332A1 1 0 0 1 3 20z"),
            line_path(21.0, 4.0, 21.0, 20.0),
        ],
        Icon::SkipBack => vec![
            path_from_d("M21 4a1 1 0 0 0-1.555-.832L10 9.5v5l9.445 6.332A1 1 0 0 0 21 20z"),
            line_path(3.0, 4.0, 3.0, 20.0),
        ],
        // Bell: dome body + the clapper arc at the base. Transcribed
        // from lucide's `bell.js` (v0.546.0).
        Icon::Bell => vec![
            path_from_d("M10.268 21a2 2 0 0 0 3.464 0"),
            path_from_d("M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"),
        ],
        // Play: single filled-look triangle traced as a stroked path so
        // it matches the lucide stroke aesthetic. From `play.svg` (v0.546.0).
        Icon::Play => vec![path_from_d(
            "M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z",
        )],
        // Pause: two rounded vertical bars. From lucide's `pause.svg` (v0.546.0).
        Icon::Pause => vec![
            rect_path(14.0, 3.0, 5.0, 18.0, 1.0),
            rect_path(5.0, 3.0, 5.0, 18.0, 1.0),
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
            path_from_d("m22 7-8.991 5.727a2 2 0 0 1-2.009 0L2 7"),
        ],
        // Paperclip: single hook path. From lucide's `paperclip.svg` (v0.546.0).
        Icon::Paperclip => vec![path_from_d(
            "m16 6-8.414 8.586a2 2 0 0 0 2.829 2.829l8.414-8.586a4 4 0 1 0-5.657-5.657l-8.379 8.551a6 6 0 1 0 8.485 8.485l8.379-8.551",
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
        // --- Camera / capture set — lucide v0.546.0 ---
        // Switch-camera: two overlapping frame corners, a lens dot, and the
        // two swap-arrow heads. This is the "flip to front/back camera" glyph.
        Icon::SwitchCamera => vec![
            path_from_d("M11 19H4a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h5"),
            path_from_d("M13 5h7a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2h-5"),
            circle_path(12.0, 12.0, 3.0),
            path_from_d("m18 22-3-3 3-3"),
            path_from_d("m6 2 3 3-3 3"),
        ],
        // Aperture: outer ring + six iris blades struck as chords.
        Icon::Aperture => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("m14.31 8 5.74 9.94"),
            path_from_d("M9.69 8h11.48"),
            path_from_d("m7.38 12 5.74-9.94"),
            path_from_d("M9.69 16 3.95 6.06"),
            path_from_d("M14.31 16H2.83"),
            path_from_d("m16.62 12-5.74 9.94"),
        ],
        // Timer: top button bar + the hand + the dial ring.
        Icon::Timer => vec![
            line_path(10.0, 2.0, 14.0, 2.0),
            line_path(12.0, 14.0, 15.0, 11.0),
            circle_path(12.0, 14.0, 8.0),
        ],
        // Zap: single closed lightning-bolt loop.
        Icon::Zap => vec![path_from_d(
            "M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z",
        )],
        // Scan-line: four rounded framing corners + a horizontal sweep.
        Icon::ScanLine => vec![
            path_from_d("M3 7V5a2 2 0 0 1 2-2h2"),
            path_from_d("M17 3h2a2 2 0 0 1 2 2v2"),
            path_from_d("M21 17v2a2 2 0 0 1-2 2h-2"),
            path_from_d("M7 21H5a2 2 0 0 1-2-2v-2"),
            path_from_d("M7 12h10"),
        ],
        // Sliders-horizontal: three rails with offset knobs.
        Icon::SlidersHorizontal => vec![
            path_from_d("M10 5H3"),
            path_from_d("M12 19H3"),
            path_from_d("M14 3v4"),
            path_from_d("M16 17v4"),
            path_from_d("M21 12h-9"),
            path_from_d("M21 19h-5"),
            path_from_d("M21 5h-7"),
            path_from_d("M8 10v4"),
            path_from_d("M8 12H3"),
        ],
        // Grid-3x3: framed rect + two inner rules each axis.
        Icon::Grid3x3 => vec![
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            path_from_d("M3 9h18"),
            path_from_d("M3 15h18"),
            path_from_d("M9 3v18"),
            path_from_d("M15 3v18"),
        ],
        // --- People / sharing / places set — lucide v0.546.0 ---
        // User: shoulders arc + head circle.
        Icon::User => vec![
            path_from_d("M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"),
            circle_path(12.0, 7.0, 4.0),
        ],
        // Share-2: three node dots + the two connecting links.
        Icon::Share2 => vec![
            circle_path(18.0, 5.0, 3.0),
            circle_path(6.0, 12.0, 3.0),
            circle_path(18.0, 19.0, 3.0),
            line_path(8.59, 13.51, 15.42, 17.49),
            line_path(15.41, 6.51, 8.59, 10.49),
        ],
        // Map-pin: teardrop body + inner dot.
        Icon::MapPin => vec![
            path_from_d("M20 10c0 4.993-5.539 10.193-7.399 11.799a1 1 0 0 1-1.202 0C9.539 20.193 4 14.993 4 10a8 8 0 0 1 16 0"),
            circle_path(12.0, 10.0, 3.0),
        ],
        // Phone: single handset loop.
        Icon::Phone => vec![path_from_d(
            "M13.832 16.568a1 1 0 0 0 1.213-.303l.355-.465A2 2 0 0 1 17 15h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2A18 18 0 0 1 2 4a2 2 0 0 1 2-2h3a2 2 0 0 1 2 2v3a2 2 0 0 1-.8 1.6l-.468.351a1 1 0 0 0-.292 1.233 14 14 0 0 0 6.392 6.384",
        )],
        // Link: two interlocking chain halves.
        Icon::Link => vec![
            path_from_d("M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"),
            path_from_d("M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"),
        ],
        // --- Container / shape set — lucide v0.546.0 ---
        // Square: single rounded-rect outline (fills to a "stop" glyph).
        Icon::Square => vec![rect_path(3.0, 3.0, 18.0, 18.0, 2.0)],
        // Package: box body + vertical seam + top fold + side seam.
        Icon::Package => vec![
            path_from_d("M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z"),
            path_from_d("M12 22V12"),
            path_from_d("M3.29 7 12 12 20.71 7"),
            path_from_d("m7.5 4.27 9 5.15"),
        ],
        // Terminal: baseline + the prompt chevron.
        Icon::Terminal => vec![
            path_from_d("M12 19h8"),
            path_from_d("m4 17 6-6-6-6"),
        ],
        // --- Media-transport / library set — lucide v0.546.0 ---
        // (SkipBack / SkipForward / VolumeX are defined in the sections above.)
        // Shuffle: two arrowheads + two crossing rails.
        Icon::Shuffle => vec![
            path_from_d("m18 14 4 4-4 4"),
            path_from_d("m18 2 4 4-4 4"),
            path_from_d("M2 18h1.973a4 4 0 0 0 3.3-1.7l5.454-8.6a4 4 0 0 1 3.3-1.7H22"),
            path_from_d("M2 6h1.972a4 4 0 0 1 3.6 2.2"),
            path_from_d("M22 18h-6.041a4 4 0 0 1-3.3-1.8l-.359-.45"),
        ],
        // Repeat: arrowhead pair + the loop rails.
        Icon::Repeat => vec![
            path_from_d("m17 2 4 4-4 4"),
            path_from_d("M3 11v-1a4 4 0 0 1 4-4h14"),
            path_from_d("m7 22-4-4 4-4"),
            path_from_d("M21 13v1a4 4 0 0 1-4 4H3"),
        ],
        // Repeat-1: repeat loop + the inner "1".
        Icon::Repeat1 => vec![
            path_from_d("m17 2 4 4-4 4"),
            path_from_d("M3 11v-1a4 4 0 0 1 4-4h14"),
            path_from_d("m7 22-4-4 4-4"),
            path_from_d("M21 13v1a4 4 0 0 1-4 4H3"),
            path_from_d("M11 10h1v4"),
        ],
        // Heart: single closed lobe path (fills cleanly for a "liked" state).
        Icon::Heart => vec![path_from_d(
            "M2 9.5a5.5 5.5 0 0 1 9.591-3.676.56.56 0 0 0 .818 0A5.49 5.49 0 0 1 22 9.5c0 2.29-1.5 4-3 5.5l-5.492 5.313a2 2 0 0 1-3 .019L5 15c-1.5-1.5-3-3.2-3-5.5",
        )],
        // List-music: three rules + the note stem and head.
        Icon::ListMusic => vec![
            path_from_d("M16 5H3"),
            path_from_d("M11 12H3"),
            path_from_d("M11 19H3"),
            path_from_d("M21 16V5"),
            circle_path(18.0, 16.0, 3.0),
        ],
        // List-ordered: three rules + the "1" and "2" number strokes.
        Icon::ListOrdered => vec![
            path_from_d("M11 5h10"),
            path_from_d("M11 12h10"),
            path_from_d("M11 19h10"),
            path_from_d("M4 4h1v5"),
            path_from_d("M4 9h2"),
            path_from_d("M6.5 20H3.4c0-1 2.6-1.925 2.6-3.5a1.5 1.5 0 0 0-2.6-1.02"),
        ],
        // Disc-3: outer ring, two label arcs, and the spindle dot.
        Icon::Disc3 => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("M6 12c0-1.7.7-3.2 1.8-4.2"),
            circle_path(12.0, 12.0, 2.0),
            path_from_d("M18 12c0 1.7-.7 3.2-1.8 4.2"),
        ],
        // Users: two overlapping figures (shoulders + heads).
        Icon::Users => vec![
            path_from_d("M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"),
            path_from_d("M16 3.128a4 4 0 0 1 0 7.744"),
            path_from_d("M22 21v-2a4 4 0 0 0-3-3.87"),
            circle_path(9.0, 7.0, 4.0),
        ],
        // --- Feed / social / media-browser set — lucide v0.546.0 ---
        // Flame: single closed flame silhouette.
        Icon::Flame => vec![path_from_d(
            "M12 3q1 4 4 6.5t3 5.5a1 1 0 0 1-14 0 5 5 0 0 1 1-3 1 1 0 0 0 5 0c0-2-1.5-3-1.5-5q0-2 2.5-4",
        )],
        // Thumbs-up: forearm stroke + the closed hand body.
        Icon::ThumbsUp => vec![
            path_from_d("M7 10v12"),
            path_from_d("M15 5.88 14 10h5.83a2 2 0 0 1 1.92 2.56l-2.33 8A2 2 0 0 1 17.5 22H4a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2h2.76a2 2 0 0 0 1.79-1.11L12 2a3.13 3.13 0 0 1 3 3.88Z"),
        ],
        // Thumbs-down: mirror of thumbs-up.
        Icon::ThumbsDown => vec![
            path_from_d("M17 14V2"),
            path_from_d("M9 18.12 10 14H4.17a2 2 0 0 1-1.92-2.56l2.33-8A2 2 0 0 1 6.5 2H20a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2.76a2 2 0 0 0-1.79 1.11L12 22a3.13 3.13 0 0 1-3-3.88Z"),
        ],
        // Sparkles: big four-point star + a small star and a dot.
        Icon::Sparkles => vec![
            path_from_d("M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594z"),
            path_from_d("M20 2v4"),
            path_from_d("M22 4h-4"),
            circle_path(4.0, 20.0, 2.0),
        ],
        // Eye: outer lid loop + the iris.
        Icon::Eye => vec![
            path_from_d("M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0"),
            circle_path(12.0, 12.0, 3.0),
        ],
        // Menu: three equal rules.
        Icon::Menu => vec![
            path_from_d("M4 5h16"),
            path_from_d("M4 12h16"),
            path_from_d("M4 19h16"),
        ],
        // Monitor-play: screen rect + stand + the play triangle inside.
        Icon::MonitorPlay => vec![
            path_from_d("M15.033 9.44a.647.647 0 0 1 0 1.12l-4.065 2.352a.645.645 0 0 1-.968-.56V7.648a.645.645 0 0 1 .967-.56z"),
            path_from_d("M12 17v4"),
            path_from_d("M8 21h8"),
            rect_path(2.0, 3.0, 20.0, 14.0, 2.0),
        ],
        // History: broken clock ring + the return arrowhead + the hands.
        Icon::History => vec![
            path_from_d("M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"),
            path_from_d("M3 3v5h5"),
            path_from_d("M12 7v5l4 2"),
        ],
        // List-video: three rules + the play triangle.
        Icon::ListVideo => vec![
            path_from_d("M21 5H3"),
            path_from_d("M10 12H3"),
            path_from_d("M10 19H3"),
            path_from_d("M15 12.003a1 1 0 0 1 1.517-.859l4.997 2.997a1 1 0 0 1 0 1.718l-4.997 2.997a1 1 0 0 1-1.517-.86z"),
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
    /// Every [`Icon`] variant, in declaration order. This is the single
    /// source of truth for "the whole set" — the gallery example paints
    /// from it and the test-suite audits every glyph through it, so a new
    /// variant is covered the moment it is added here.
    pub const ALL: &'static [Icon] = &[
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
        Icon::Bell,
        Icon::Play,
        Icon::Pause,
        Icon::Refresh,
        Icon::ExternalLink,
        Icon::Trash,
        Icon::MoreHorizontal,
        Icon::Inbox,
        Icon::Send,
        Icon::Reply,
        Icon::Mail,
        Icon::Paperclip,
        Icon::Calendar,
        Icon::Ban,
        Icon::Download,
        Icon::Sun,
        Icon::Volume2,
        Icon::VolumeX,
        Icon::WifiOff,
        Icon::EthernetPort,
        Icon::MicOff,
        Icon::SkipForward,
        Icon::SkipBack,
        Icon::ArrowBigUp,
        Icon::Delete,
        Icon::CornerDownLeft,
        Icon::SwitchCamera,
        Icon::Aperture,
        Icon::Timer,
        Icon::Zap,
        Icon::ScanLine,
        Icon::SlidersHorizontal,
        Icon::Grid3x3,
        Icon::User,
        Icon::Share2,
        Icon::MapPin,
        Icon::Phone,
        Icon::Link,
        Icon::Square,
        Icon::Package,
        Icon::Terminal,
        Icon::Shuffle,
        Icon::Repeat,
        Icon::Repeat1,
        Icon::Heart,
        Icon::ListMusic,
        Icon::ListOrdered,
        Icon::Disc3,
        Icon::Users,
        Icon::Flame,
        Icon::ThumbsUp,
        Icon::ThumbsDown,
        Icon::Sparkles,
        Icon::Eye,
        Icon::Menu,
        Icon::MonitorPlay,
        Icon::History,
        Icon::ListVideo,
    ];

    /// The stable lucide kebab-ish name for this glyph — handy for gallery
    /// labels and debugging. Mirrors the `Debug` variant name.
    pub fn name(self) -> &'static str {
        match self {
            Icon::LayoutGrid => "layout-grid",
            Icon::Settings2 => "settings-2",
            Icon::Wifi => "wifi",
            Icon::Bluetooth => "bluetooth",
            Icon::Monitor => "monitor",
            Icon::Battery => "battery",
            Icon::Shield => "shield",
            Icon::Maximize2 => "maximize-2",
            Icon::Minimize2 => "minimize-2",
            Icon::X => "x",
            Icon::ChevronUp => "chevron-up",
            Icon::ChevronDown => "chevron-down",
            Icon::ChevronLeft => "chevron-left",
            Icon::Plus => "plus",
            Icon::Search => "search",
            Icon::PenTool => "pen-tool",
            Icon::Code => "code",
            Icon::FileText => "file-text",
            Icon::BarChart => "bar-chart",
            Icon::Layers => "layers",
            Icon::Settings => "settings",
            Icon::Home => "home",
            Icon::Camera => "camera",
            Icon::Power => "power",
            Icon::Folder => "folder",
            Icon::FolderOpen => "folder-open",
            Icon::File => "file",
            Icon::Image => "image",
            Icon::Music => "music",
            Icon::Film => "film",
            Icon::Archive => "archive",
            Icon::FileCode => "file-code",
            Icon::ArrowLeft => "arrow-left",
            Icon::ArrowRight => "arrow-right",
            Icon::ArrowUp => "arrow-up",
            Icon::ChevronRight => "chevron-right",
            Icon::RotateCw => "rotate-cw",
            Icon::Copy => "copy",
            Icon::Scissors => "scissors",
            Icon::ClipboardPaste => "clipboard-paste",
            Icon::Trash2 => "trash-2",
            Icon::Pencil => "pencil",
            Icon::FolderPlus => "folder-plus",
            Icon::FilePlus => "file-plus",
            Icon::List => "list",
            Icon::Star => "star",
            Icon::HardDrive => "hard-drive",
            Icon::Check => "check",
            Icon::Info => "info",
            Icon::CloudDownload => "cloud-download",
            Icon::ArrowUpDown => "arrow-up-down",
            Icon::Bell => "bell",
            Icon::Play => "play",
            Icon::Pause => "pause",
            Icon::Refresh => "refresh-cw",
            Icon::ExternalLink => "external-link",
            Icon::Trash => "trash",
            Icon::MoreHorizontal => "more-horizontal",
            Icon::Inbox => "inbox",
            Icon::Send => "send",
            Icon::Reply => "reply",
            Icon::Mail => "mail",
            Icon::Paperclip => "paperclip",
            Icon::Calendar => "calendar",
            Icon::Ban => "ban",
            Icon::Download => "download",
            Icon::Sun => "sun",
            Icon::Volume2 => "volume-2",
            Icon::WifiOff => "wifi-off",
            Icon::EthernetPort => "ethernet-port",
            Icon::MicOff => "mic-off",
            Icon::ArrowBigUp => "arrow-big-up",
            Icon::Delete => "delete",
            Icon::CornerDownLeft => "corner-down-left",
            Icon::SwitchCamera => "switch-camera",
            Icon::Aperture => "aperture",
            Icon::Timer => "timer",
            Icon::Zap => "zap",
            Icon::ScanLine => "scan-line",
            Icon::SlidersHorizontal => "sliders-horizontal",
            Icon::Grid3x3 => "grid-3x3",
            Icon::User => "user",
            Icon::Share2 => "share-2",
            Icon::MapPin => "map-pin",
            Icon::Phone => "phone",
            Icon::Link => "link",
            Icon::Square => "square",
            Icon::Package => "package",
            Icon::Terminal => "terminal",
            Icon::SkipBack => "skip-back",
            Icon::SkipForward => "skip-forward",
            Icon::Shuffle => "shuffle",
            Icon::Repeat => "repeat",
            Icon::Repeat1 => "repeat-1",
            Icon::Heart => "heart",
            Icon::VolumeX => "volume-x",
            Icon::ListMusic => "list-music",
            Icon::ListOrdered => "list-ordered",
            Icon::Disc3 => "disc-3",
            Icon::Users => "users",
            Icon::Flame => "flame",
            Icon::ThumbsUp => "thumbs-up",
            Icon::ThumbsDown => "thumbs-down",
            Icon::Sparkles => "sparkles",
            Icon::Eye => "eye",
            Icon::Menu => "menu",
            Icon::MonitorPlay => "monitor-play",
            Icon::History => "history",
            Icon::ListVideo => "list-video",
        }
    }

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

/// [`draw_icon`] with a per-subpath colour.
///
/// Exists for glyphs whose subpaths mean different things — the Wi-Fi
/// arcs are a *scale*, not a picture, so a two-bar signal should paint
/// two arcs lit and two dim rather than the whole glyph at one alpha.
/// Drawing four separate icons at four nested sizes would not line up;
/// they have to come from the same path table.
///
/// `color_for` receives the subpath index in the order
/// [`icon_subpaths`] returns them (for [`Icon::Wifi`]: base dot, outer
/// arc, middle arc, inner arc).
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

    /// The status-tray glyph set. Subpath counts are pinned because the
    /// Wi-Fi arcs are addressed *by index* — the tray paints arcs 1..=3
    /// at different alphas to show signal strength (see
    /// [`draw_icon_layered`]), so a reordering upstream would silently
    /// light the wrong bars rather than fail to compile.
    #[test]
    fn status_tray_icons_have_their_expected_subpaths() {
        assert_eq!(
            icon_subpaths(Icon::Wifi).len(),
            4,
            "wifi = base dot + 3 arcs, outermost first — the tray indexes these"
        );
        assert_eq!(icon_subpaths(Icon::WifiOff).len(), 7);
        assert_eq!(icon_subpaths(Icon::VolumeX).len(), 3, "cone + 2 cross lines");
        assert_eq!(
            icon_subpaths(Icon::EthernetPort).len(),
            5,
            "socket outline + 4 pin nubs"
        );
        assert_eq!(icon_subpaths(Icon::MicOff).len(), 6);
        assert_eq!(icon_subpaths(Icon::SkipForward).len(), 2, "triangle + bar");
        assert_eq!(icon_subpaths(Icon::SkipBack).len(), 2, "triangle + bar");
    }

    /// The two skip glyphs are mirror images, so a swap produces two
    /// icons that each look fine alone and both point the wrong way.
    /// Pin the direction geometrically: the end bar sits on the side
    /// the triangle points at, so its x-position identifies the glyph.
    #[test]
    fn the_skip_glyphs_point_the_way_they_are_named() {
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

    #[test]
    fn draw_icon_layered_paints_every_subpath() {
        // A layered draw must not silently skip subpaths — the Wi-Fi
        // signal ramp depends on all four being emitted, just in
        // different colours.
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

    /// Centroid of a subpath's bounding box in the 24×24 lucide grid.
    fn subpath_centroid(p: &BezPath) -> (f64, f64) {
        let bb = p.bounding_box();
        ((bb.x0 + bb.x1) / 2.0, (bb.y0 + bb.y1) / 2.0)
    }

    /// A reload glyph must read as a **clockwise** circular arrow: its arrowhead
    /// tick sits in the top-right quadrant of the grid, pointing along the
    /// clockwise sweep. A mirrored / reversed path — the classic "reload drawn
    /// the wrong way around" bug — puts the head in the top-left instead. These
    /// assert on the real arrowhead subpath geometry so a bad hand-edit to the
    /// `d="…"` literal (or an upstream flip) trips here rather than shipping a
    /// backwards reload button.
    #[test]
    fn rotate_cw_arrowhead_reads_clockwise() {
        let sub = icon_subpaths(Icon::RotateCw);
        assert_eq!(sub.len(), 2, "rotate-cw = arc + arrowhead");
        let (cx, cy) = subpath_centroid(&sub[1]);
        assert!(cx > 12.0, "rotate-cw arrowhead must sit right of centre (cx={cx})");
        assert!(cy < 12.0, "rotate-cw arrowhead must sit above centre (cy={cy})");
    }

    #[test]
    fn refresh_cw_reads_clockwise() {
        // refresh-cw = [top arc, top-right head, bottom arc, bottom-left head].
        let sub = icon_subpaths(Icon::Refresh);
        assert_eq!(sub.len(), 4, "refresh = 2 arcs + 2 heads");
        let (tx, ty) = subpath_centroid(&sub[1]);
        let (bx, by) = subpath_centroid(&sub[3]);
        assert!(tx > 12.0 && ty < 12.0, "top arrowhead must be top-right (got ({tx},{ty}))");
        assert!(bx < 12.0 && by > 12.0, "bottom arrowhead must be bottom-left (got ({bx},{by}))");
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

    /// The whole-set audit: **every** [`Icon::ALL`] variant must resolve to
    /// at least one non-empty subpath so no glyph ever ships as a blank
    /// square, and every subpath must stay inside the lucide 24×24 viewBox
    /// (a small tolerance covers arc/stroke overshoot). A misplaced sub-path
    /// — the class of bug this module exists to prevent — lands a shape far
    /// outside the box and trips the bounds assertion here.
    #[test]
    fn every_icon_is_non_empty_and_within_viewbox() {
        use vello::kurbo::Shape;
        // Generous margin: lucide authors to 0..24 but a few arcs and the
        // rounded-rect flattening nudge a hair past the edge.
        const LO: f64 = -1.0;
        const HI: f64 = 25.0;
        for &icon in Icon::ALL {
            let subs = icon_subpaths(icon);
            assert!(!subs.is_empty(), "{icon:?} produced no subpaths");
            for (i, p) in subs.iter().enumerate() {
                assert!(element_count(p) > 0, "{icon:?} subpath {i} is empty");
                let bb = p.bounding_box();
                assert!(
                    bb.x0 >= LO && bb.y0 >= LO && bb.x1 <= HI && bb.y1 <= HI,
                    "{icon:?} subpath {i} escapes the 24x24 viewBox: {bb:?}"
                );
            }
        }
    }

    /// `Icon::ALL` must enumerate the set with no duplicates, and `name()`
    /// must give each variant a distinct label — a copy-paste slip when
    /// adding a variant (forgetting to change the `ALL` entry or the `name`
    /// arm) is caught here rather than showing up as a doubled gallery cell.
    #[test]
    fn all_and_names_are_unique() {
        use std::collections::HashSet;
        let set: HashSet<Icon> = Icon::ALL.iter().copied().collect();
        assert_eq!(set.len(), Icon::ALL.len(), "Icon::ALL has a duplicate variant");
        let names: HashSet<&str> = Icon::ALL.iter().map(|i| i.name()).collect();
        assert_eq!(names.len(), Icon::ALL.len(), "two Icon variants share a name()");
    }

    /// The freshly-added camera/media/container glyphs must transcribe into
    /// non-empty subpaths with the expected primitive counts, so an upstream
    /// lucide restructure or a bad hand-edit trips here.
    #[test]
    fn camera_media_icon_subpath_counts() {
        assert_eq!(icon_subpaths(Icon::SwitchCamera).len(), 5, "switch-camera = 2 frames + lens + 2 arrows");
        assert_eq!(icon_subpaths(Icon::Aperture).len(), 7, "aperture = ring + 6 blades");
        assert_eq!(icon_subpaths(Icon::Timer).len(), 3, "timer = bar + hand + dial");
        assert_eq!(icon_subpaths(Icon::Zap).len(), 1, "zap = 1 bolt loop");
        assert_eq!(icon_subpaths(Icon::ScanLine).len(), 5, "scan-line = 4 corners + sweep");
        assert_eq!(icon_subpaths(Icon::SlidersHorizontal).len(), 9, "sliders = 3 rails + 3 knobs (6 segs)");
        assert_eq!(icon_subpaths(Icon::Grid3x3).len(), 5, "grid = frame + 4 rules");
        assert_eq!(icon_subpaths(Icon::User).len(), 2, "user = shoulders + head");
        assert_eq!(icon_subpaths(Icon::Share2).len(), 5, "share-2 = 3 nodes + 2 links");
        assert_eq!(icon_subpaths(Icon::MapPin).len(), 2, "map-pin = body + dot");
        assert_eq!(icon_subpaths(Icon::Phone).len(), 1, "phone = 1 handset loop");
        assert_eq!(icon_subpaths(Icon::Link).len(), 2, "link = 2 halves");
        assert_eq!(icon_subpaths(Icon::Square).len(), 1, "square = 1 outline");
        assert_eq!(icon_subpaths(Icon::Package).len(), 4, "package = body + seam + fold + side");
        assert_eq!(icon_subpaths(Icon::Terminal).len(), 2, "terminal = baseline + chevron");
    }

    /// The media-transport / library glyphs must transcribe with the expected
    /// primitive counts so the music chrome never ships a blank or half-glyph.
    #[test]
    fn media_library_icon_subpath_counts() {
        assert_eq!(icon_subpaths(Icon::SkipBack).len(), 2, "skip-back = triangle + bar");
        assert_eq!(icon_subpaths(Icon::SkipForward).len(), 2, "skip-forward = bar + triangle");
        assert_eq!(icon_subpaths(Icon::Shuffle).len(), 5, "shuffle = 2 heads + 3 rails");
        assert_eq!(icon_subpaths(Icon::Repeat).len(), 4, "repeat = head + 3 rails");
        assert_eq!(icon_subpaths(Icon::Repeat1).len(), 5, "repeat-1 = repeat + the 1");
        assert_eq!(icon_subpaths(Icon::Heart).len(), 1, "heart = 1 closed lobe");
        assert_eq!(icon_subpaths(Icon::VolumeX).len(), 3, "volume-x = cone + 2 cross strokes");
        assert_eq!(icon_subpaths(Icon::ListMusic).len(), 5, "list-music = 3 rules + stem + head");
        assert_eq!(icon_subpaths(Icon::ListOrdered).len(), 6, "list-ordered = 3 rules + 1 + 2");
        assert_eq!(icon_subpaths(Icon::Disc3).len(), 4, "disc-3 = ring + 2 arcs + spindle");
        assert_eq!(icon_subpaths(Icon::Users).len(), 4, "users = 2 bodies + arc + head");
    }

    /// The feed / social / media-browser glyphs must transcribe with their
    /// expected primitive counts.
    #[test]
    fn feed_social_icon_subpath_counts() {
        assert_eq!(icon_subpaths(Icon::Flame).len(), 1, "flame = 1 silhouette");
        assert_eq!(icon_subpaths(Icon::ThumbsUp).len(), 2, "thumbs-up = arm + hand");
        assert_eq!(icon_subpaths(Icon::ThumbsDown).len(), 2, "thumbs-down = arm + hand");
        assert_eq!(icon_subpaths(Icon::Sparkles).len(), 4, "sparkles = big star + 2 strokes + dot");
        assert_eq!(icon_subpaths(Icon::Eye).len(), 2, "eye = lid + iris");
        assert_eq!(icon_subpaths(Icon::Menu).len(), 3, "menu = 3 rules");
        assert_eq!(icon_subpaths(Icon::MonitorPlay).len(), 4, "monitor-play = play + stand + foot + screen");
        assert_eq!(icon_subpaths(Icon::History).len(), 3, "history = ring + arrow + hands");
        assert_eq!(icon_subpaths(Icon::ListVideo).len(), 4, "list-video = 3 rules + play");
    }
}

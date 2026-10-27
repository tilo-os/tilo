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

mod app_glyph;
mod app_image;
mod service_mark;

pub use app_glyph::app_glyph;
pub use service_mark::{ServiceMark, service_mark};

pub use app_image::{AppImage, AppImageSource, MAX_DEVICE_PX, device_px, draw_app_image};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill};

/// Declares [`Icon`] from one table of variants and their lucide kebab
/// names, so the enum, [`Icon::ALL`] and [`Icon::name`] cannot disagree.
macro_rules! icons {
    ($($(#[$doc:meta])* $icon:ident => $name:literal,)*) => {
        /// Stable identifier for a lucide glyph. New entries map 1:1 to lucide
        /// icon kebab names (e.g. `Icon::LayoutGrid` → `layout-grid`).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($(#[$doc])* $icon,)*
        }

        impl Icon {
            /// Every icon, in declaration order.
            pub const ALL: [Self; [$(stringify!($icon)),*].len()] = [$(Self::$icon),*];

            /// The icon's lucide kebab name (`Icon::LayoutGrid` → `layout-grid`):
            /// how data that cannot name the enum (a resource's search row, a
            /// manifest) names an icon.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$icon => $name,)*
                }
            }
        }
    };
}

icons! {
    LayoutGrid => "layout-grid",
    Settings2 => "settings-2",
    Mouse => "mouse",
    Keyboard => "keyboard",
    Users => "users",
    Printer => "printer",
    Globe => "globe",
    Wifi => "wifi",
    Bluetooth => "bluetooth",
    Monitor => "monitor",
    Battery => "battery",
    Shield => "shield",
    Maximize2 => "maximize-2",
    Minimize2 => "minimize-2",
    X => "x",
    ChevronUp => "chevron-up",
    ChevronDown => "chevron-down",
    ChevronLeft => "chevron-left",
    Plus => "plus",
    Search => "search",
    PenTool => "pen-tool",
    Code => "code",
    Terminal => "terminal",
    FileText => "file-text",
    BarChart => "bar-chart",
    Layers => "layers",
    Settings => "settings",
    Home => "home",
    Camera => "camera",
    Power => "power",
    // --- File-manager set (file types) ---
    Folder => "folder",
    FolderOpen => "folder-open",
    File => "file",
    Image => "image",
    Music => "music",
    Film => "film",
    Archive => "archive",
    FileCode => "file-code",
    // --- File-manager set (actions / chrome) ---
    ArrowLeft => "arrow-left",
    ArrowRight => "arrow-right",
    ArrowUp => "arrow-up",
    ArrowDown => "arrow-down",
    ChevronRight => "chevron-right",
    RotateCcw => "rotate-ccw",
    RotateCw => "rotate-cw",
    Copy => "copy",
    Scissors => "scissors",
    ClipboardPaste => "clipboard-paste",
    Trash2 => "trash-2",
    Pencil => "pencil",
    Square => "square",
    /// Make the text smaller.
    AArrowDown => "a-arrow-down",
    /// Make the text larger.
    AArrowUp => "a-arrow-up",
    /// Show or hide a ruler.
    Ruler => "ruler",
    /// A window's minimize control.
    Minus => "minus",
    FolderPlus => "folder-plus",
    FilePlus => "file-plus",
    List => "list",
    Star => "star",
    Pin => "pin",
    HardDrive => "hard-drive",
    Check => "check",
    // --- Video / media-navigation set ---
    Flame => "flame",
    GalleryVerticalEnd => "gallery-vertical-end",
    History => "history",
    Timer => "timer",
    ThumbsUp => "thumbs-up",
    ThumbsDown => "thumbs-down",
    ListVideo => "list-video",
    PictureInPicture2 => "picture-in-picture-2",
    Sparkles => "sparkles",
    Eye => "eye",
    MousePointer2 => "mouse-pointer-2",
    Menu => "menu",
    Info => "info",
    CloudDownload => "cloud-download",
    ArrowUpDown => "arrow-up-down",
    // --- Extended core set ---
    Bell => "bell",
    BellOff => "bell-off",
    Play => "play",
    Pause => "pause",
    Refresh => "refresh",
    ExternalLink => "external-link",
    Trash => "trash",
    MoreHorizontal => "more-horizontal",
    // --- Mail set (folders / actions) ---
    Inbox => "inbox",
    Send => "send",
    Reply => "reply",
    Mail => "mail",
    Paperclip => "paperclip",
    Calendar => "calendar",
    Ban => "ban",
    Download => "download",
    // --- Quick-settings set ---
    Sun => "sun",
    Volume2 => "volume-2",
    VolumeX => "volume-x",
    WifiOff => "wifi-off",
    /// Lucide `ethernet-port` — the RJ45 socket outline reads as
    /// "cable" at 14 px where a plug icon does not.
    EthernetPort => "ethernet-port",
    Mic => "mic",
    MicOff => "mic-off",
    SkipForward => "skip-forward",
    SkipBack => "skip-back",
    // --- On-screen-keyboard set (action keys) ---
    /// The Shift key. Rendered filled for a latched Caps-Lock (see
    /// [`Icon::draw_filled`]).
    ArrowBigUp => "arrow-big-up",
    Delete => "delete",
    CornerDownLeft => "corner-down-left",
    // --- History set ---
    /// Undo — a leftward arrow curving back on itself. Lucide `undo-2`.
    Undo2 => "undo-2",
    /// Redo — the mirror of [`Icon::Undo2`]. Lucide `redo-2`.
    Redo2 => "redo-2",
    // --- Status set ---
    /// A warning: Lucide `triangle-alert`.
    TriangleAlert => "triangle-alert",
    // --- Window navigation set ---
    /// Show or hide the sidebar: a window with a rule near its leading
    /// edge. Lucide `panel-left`.
    PanelLeft => "panel-left",
    /// Show or hide the inspector: the mirror of [`Icon::PanelLeft`].
    PanelRight => "panel-right",
    /// Show or hide a bottom panel (problems, a terminal): a window with a
    /// rule near its bottom edge. Lucide `panel-bottom`.
    PanelBottom => "panel-bottom",
    // --- Everyday set: people, places, media, text and status (Lucide v0.546.0) ---
    EyeOff => "eye-off",
    Phone => "phone",
    Link => "link",
    MapPin => "map-pin",
    QrCode => "qr-code",
    ScanLine => "scan-line",
    SwitchCamera => "switch-camera",
    User => "user",
    UserPlus => "user-plus",
    Hash => "hash",
    Smile => "smile",
    SmilePlus => "smile-plus",
    Video => "video",
    PhoneOff => "phone-off",
    Lock => "lock",
    MessageSquare => "message-square",
    MessageCircle => "message-circle",
    Clock => "clock",
    AlarmClock => "alarm-clock",
    Hourglass => "hourglass",
    Bookmark => "bookmark",
    Tag => "tag",
    Rss => "rss",
    Upload => "upload",
    Type => "type",
    Box => "box",
    Contrast => "contrast",
    Columns2 => "columns-2",
    Palette => "palette",
    Crop => "crop",
    Usb => "usb",
    CircleCheck => "circle-check",
    Disc3 => "disc-3",
    Bold => "bold",
    Italic => "italic",
    Underline => "underline",
    Strikethrough => "strikethrough",
    Superscript => "superscript",
    Subscript => "subscript",
    Heading1 => "heading-1",
    Heading2 => "heading-2",
    ListOrdered => "list-ordered",
    ListChecks => "list-checks",
    Quote => "quote",
    ReplyAll => "reply-all",
    Forward => "forward",
    MailOpen => "mail-open",
    AlignLeft => "align-left",
    AlignCenter => "align-center",
    AlignRight => "align-right",
    AlignJustify => "align-justify",
    BringToFront => "bring-to-front",
    SendToBack => "send-to-back",
    Baseline => "baseline",
    Highlighter => "highlighter",
    Table => "table",
    ListTree => "list-tree",
    Shuffle => "shuffle",
    Repeat => "repeat",
    Repeat1 => "repeat-1",
    Heart => "heart",
    ListMusic => "list-music",
    Speaker => "speaker",
    Headphones => "headphones",
    /// Lucide `headphone-off`: someone who hears nothing of a call.
    HeadphoneOff => "headphone-off",
    Gauge => "gauge",
    LocateFixed => "locate-fixed",
    Navigation => "navigation",
    Flag => "flag",
    CornerUpLeft => "corner-up-left",
    CornerUpRight => "corner-up-right",
    ArrowUpLeft => "arrow-up-left",
    ArrowUpRight => "arrow-up-right",
    ArrowDownLeft => "arrow-down-left",
    ArrowDownRight => "arrow-down-right",
    Coffee => "coffee",
    Utensils => "utensils",
    Beer => "beer",
    Landmark => "landmark",
    Trees => "trees",
    TrainFront => "train-front",
    BedDouble => "bed-double",
    ShoppingBag => "shopping-bag",
    Hospital => "hospital",
    Droplet => "droplet",
    Wind => "wind",
    Cloud => "cloud",
    CloudRain => "cloud-rain",
    CloudSnow => "cloud-snow",
    Moon => "moon",
    Hand => "hand",
    PawPrint => "paw-print",
    Plane => "plane",
    Trophy => "trophy",
    Lightbulb => "lightbulb",
    /// An SD card: Lucide `sd-card` as it shipped before the set dropped it.
    SdCard => "sd-card",
    // --- Drawing set: Canvas's tools, layers and history ---
    /// A paint brush. Lucide `brush`.
    Brush => "brush",
    /// A circle, for an ellipse tool. Lucide `circle`.
    Circle => "circle",
    /// A diagonal line. Lucide `slash`.
    Slash => "slash",
    /// An eyedropper that picks a colour. Lucide `pipette`.
    Pipette => "pipette",
    /// An eraser. Lucide `eraser`.
    Eraser => "eraser",
    /// Unlocked. Lucide `lock-open`.
    LockOpen => "lock-open",
    /// Linked: moves with what it belongs to. Lucide `link`.
    /// Unlinked: stays put on its own. Lucide `unlink`.
    Unlink => "unlink",
    /// An agent. Lucide `bot`.
    Bot => "bot",
    /// Discord's mark, for linking a Discord account. Not Lucide: drawn
    /// filled ([`Icon::is_brand`]), its eyes left open.
    Discord => "discord",
    /// A rectangle of an area being selected. Lucide `square-dashed`.
    SquareDashed => "square-dashed",
    /// An ellipse of an area being selected. Lucide `circle-dashed`.
    CircleDashed => "circle-dashed",
    /// A freehand selection. Lucide `lasso`.
    Lasso => "lasso",
    /// Selecting by colour, a magic wand. Lucide `wand-sparkles`.
    WandSparkles => "wand-sparkles",
    /// Filling an area with colour. Lucide `paint-bucket`.
    PaintBucket => "paint-bucket",
    /// Shapes united into one. Lucide `squares-unite`.
    SquaresUnite => "squares-unite",
    /// One shape taken out of another. Lucide `squares-subtract`.
    SquaresSubtract => "squares-subtract",
    /// Only where shapes overlap. Lucide `squares-intersect`.
    SquaresIntersect => "squares-intersect",
    /// Where either shape is, but not both. Lucide `squares-exclude`.
    SquaresExclude => "squares-exclude",
    /// A shell on another machine (SSH). Lucide `square-terminal`.
    SquareTerminal => "square-terminal",
    /// Another machine's screen, shared (VNC). Lucide `screen-share`.
    ScreenShare => "screen-share",
    /// A desktop session in a window (RDP). Lucide `app-window`.
    AppWindow => "app-window",
    // --- Capture set: what a screenshot, recording or picture in picture takes ---
    /// Recording: a dot inside a ring, the record button. Lucide `circle-dot`.
    CircleDot => "circle-dot",
    /// A strip of windows side by side, the neighbours cut off at both
    /// edges. Lucide `gallery-horizontal`.
    GalleryHorizontal => "gallery-horizontal",
    /// One of your own devices. Lucide `monitor-smartphone`.
    MonitorSmartphone => "monitor-smartphone",
    AlignEndVertical => "align-end-vertical",
    ALargeSmall => "a-large-small",
    AlignCenterVertical => "align-center-vertical",
    Fingerprint => "fingerprint",
    AlignEndHorizontal => "align-end-horizontal",
    BatteryCharging => "battery-charging",
    Accessibility => "accessibility",
    AlignStartVertical => "align-start-vertical",
    AlignHorizontalDistributeCenter => "align-horizontal-distribute-center",
    CircleAlert => "circle-alert",
    AlignVerticalDistributeCenter => "align-vertical-distribute-center",
    AlignCenterHorizontal => "align-center-horizontal",
    AlignStartHorizontal => "align-start-horizontal",
    Command => "command",
    /// A frame around a smaller screen: covering the whole display.
    Fullscreen => "fullscreen",
    /// Four corners pointing out: a picture taking the whole display.
    Maximize => "maximize",
    /// Four corners pointing in: a picture leaving the whole display.
    Minimize => "minimize",
}

impl Icon {
    /// The icon a lucide kebab name names, as [`Self::name`] spells it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|icon| icon.name() == name)
    }

    /// A service's mark rather than a Lucide glyph: always drawn filled,
    /// as the service draws it.
    #[must_use]
    pub const fn is_brand(self) -> bool {
        matches!(self, Self::Discord)
    }
}

/// `icon` as an app's icon artwork: the glyph stroked in white on a rounded
/// tile of the accent gradient, as a standalone SVG document. A package that
/// names a platform glyph as its icon (a tilolang app's `icon: .note`)
/// ships this as its signed icon file, so the launcher grid, search and the
/// window chrome show the same artwork as any app with its own.
#[must_use]
pub fn icon_svg(icon: Icon) -> String {
    let paths: String = icon_subpaths(icon)
        .iter()
        .map(|path| format!("<path d=\"{}\"/>", path.to_svg()))
        .collect();
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"128\" height=\"128\" \
         viewBox=\"0 0 128 128\"><defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"0\" \
         y2=\"1\"><stop offset=\"0\" stop-color=\"#4c6ef5\"/><stop offset=\"1\" \
         stop-color=\"#8b5cf6\"/></linearGradient></defs><rect width=\"128\" height=\"128\" \
         rx=\"30\" fill=\"url(#g)\"/><g transform=\"translate(28 28) scale(3)\" fill=\"none\" \
         stroke=\"#ffffff\" stroke-width=\"2\" stroke-linecap=\"round\" \
         stroke-linejoin=\"round\">{paths}</g></svg>"
    )
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

/// Discord's mark in the 24×24 viewBox: its outline and its two eyes as
/// inner loops of one path, which an even-odd fill leaves open.
fn discord_mark() -> BezPath {
    let mut mark = path_from_d(
        "M20.32 4.37C18.8 3.67 17.15 3.16 15.43 2.86C15.22 3.24 14.98 3.73 14.82 4.12C12.98 3.84 11.15 3.84 9.34 4.12C9.17 3.73 8.93 3.24 8.71 2.86C7 3.16 5.35 3.67 3.82 4.37C0.53 9.05 -0.32 13.58 0.1 18.06C2.15 19.56 4.14 20.48 6.09 21.08C6.57 20.43 7 19.73 7.37 19.01C6.67 18.75 6.01 18.43 5.39 18.06C5.55 17.94 5.71 17.82 5.87 17.7C9.8 19.49 14.06 19.49 17.94 17.7C18.1 17.82 18.26 17.94 18.42 18.06C17.8 18.43 17.13 18.75 16.44 19.01C16.81 19.73 17.24 20.43 17.71 21.08C19.67 20.48 21.66 19.56 23.71 18.06C24.21 12.88 22.87 8.38 20.32 4.37Z",
    );
    for eye_x in [8.02, 15.98] {
        let eye = vello::kurbo::Ellipse::new(Point::new(eye_x, 12.91), (2.16, 2.42), 0.0);
        mark.extend(eye.path_elements(0.05));
    }
    mark
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
        Icon::AlignEndVertical => vec![
            rect_path(2.0, 4.0, 16.0, 6.0, 2.0),
            rect_path(9.0, 14.0, 9.0, 6.0, 2.0),
            path_from_d("M22 22V2"),
        ],
        Icon::ALargeSmall => [
            "m15 16 2.536-7.328a1.02 1.02 1 0 1 1.928 0L22 16",
            "M15.697 14h5.606",
            "m2 16 4.039-9.69a.5.5 0 0 1 .923 0L11 16",
            "M3.304 13h6.392",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::AlignCenterVertical => [
            "M12 2v20",
            "M8 10H4a2 2 0 0 1-2-2V6c0-1.1.9-2 2-2h4",
            "M16 10h4a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2h-4",
            "M8 20H7a2 2 0 0 1-2-2v-2c0-1.1.9-2 2-2h1",
            "M16 14h1a2 2 0 0 1 2 2v2a2 2 0 0 1-2 2h-1",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::Fingerprint => [
            "M12 10a2 2 0 0 0-2 2c0 1.02-.1 2.51-.26 4",
            "M14 13.12c0 2.38 0 6.38-1 8.88",
            "M17.29 21.02c.12-.6.43-2.3.5-3.02",
            "M2 12a10 10 0 0 1 18-6",
            "M2 16h.01",
            "M21.8 16c.2-2 .131-5.354 0-6",
            "M5 19.5C5.5 18 6 15 6 12a6 6 0 0 1 .34-2",
            "M8.65 22c.21-.66.45-1.32.57-2",
            "M9 6.8a6 6 0 0 1 9 5.2v2",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::AlignEndHorizontal => vec![
            rect_path(4.0, 2.0, 6.0, 16.0, 2.0),
            rect_path(14.0, 9.0, 6.0, 9.0, 2.0),
            path_from_d("M22 22H2"),
        ],
        Icon::BatteryCharging => [
            "m11 7-3 5h4l-3 5",
            "M14.856 6H16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2.935",
            "M22 14v-4",
            "M5.14 18H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h2.936",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::Accessibility => {
            let mut paths = vec![circle_path(16.0, 4.0, 1.0)];
            paths.extend(
                [
                    "m18 19 1-7-6 1",
                    "m5 8 3-3 5.5 3-2.36 3.5",
                    "M4.24 14.5a5 5 0 0 0 6.88 6",
                    "M13.76 17.5a5 5 0 0 0-6.88-6",
                ]
                .into_iter()
                .map(path_from_d),
            );
            paths
        }
        Icon::AlignStartVertical => vec![
            rect_path(6.0, 14.0, 9.0, 6.0, 2.0),
            rect_path(6.0, 4.0, 16.0, 6.0, 2.0),
            path_from_d("M2 2v20"),
        ],
        Icon::AlignHorizontalDistributeCenter => vec![
            rect_path(4.0, 5.0, 6.0, 14.0, 2.0),
            rect_path(14.0, 7.0, 6.0, 10.0, 2.0),
            path_from_d("M17 22v-5"),
            path_from_d("M17 7V2"),
            path_from_d("M7 22v-3"),
            path_from_d("M7 5V2"),
        ],
        Icon::CircleAlert => vec![
            circle_path(12.0, 12.0, 10.0),
            line_path(12.0, 8.0, 12.0, 12.0),
            line_path(12.0, 16.0, 12.01, 16.0),
        ],
        Icon::AlignVerticalDistributeCenter => vec![
            path_from_d("M22 17h-3"),
            path_from_d("M22 7h-5"),
            path_from_d("M5 17H2"),
            path_from_d("M7 7H2"),
            rect_path(5.0, 14.0, 14.0, 6.0, 2.0),
            rect_path(7.0, 4.0, 10.0, 6.0, 2.0),
        ],        Icon::AlignCenterHorizontal => [
            "M2 12h20",
            "M10 16v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2v-4",
            "M10 8V4a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v4",
            "M20 16v1a2 2 0 0 1-2 2h-2a2 2 0 0 1-2-2v-1",
            "M14 8V7c0-1.1.9-2 2-2h2a2 2 0 0 1 2 2v1",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::AlignStartHorizontal => vec![
            rect_path(4.0, 6.0, 6.0, 16.0, 2.0),
            rect_path(14.0, 6.0, 6.0, 9.0, 2.0),
            path_from_d("M22 2H2"),
        ],
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
        Icon::BellOff => vec![
            path_from_d("M10.268 21a2 2 0 0 0 3.464 0"),
            path_from_d("M17 17H4a1 1 0 0 1-.74-1.673C4.59 13.956 6 12.499 6 8a6 6 0 0 1 .258-1.742"),
            line_path(2.0, 2.0, 22.0, 22.0),
            path_from_d("M8.668 3.01A6 6 0 0 1 18 8c0 2.687.77 4.653 1.707 6.05"),
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
        Icon::AArrowDown => vec![
            path_from_d("m14 12 4 4 4-4"),
            path_from_d("M18 16V7"),
            path_from_d("m2 16 4.039-9.69a.5.5 0 0 1 .923 0L11 16"),
            path_from_d("M3.304 13h6.392"),
        ],
        Icon::AArrowUp => vec![
            path_from_d("m14 11 4-4 4 4"),
            path_from_d("M18 16V7"),
            path_from_d("m2 16 4.039-9.69a.5.5 0 0 1 .923 0L11 16"),
            path_from_d("M3.304 13h6.392"),
        ],
        Icon::Ruler => vec![
            path_from_d(
                "M21.3 15.3a2.4 2.4 0 0 1 0 3.4l-2.6 2.6a2.4 2.4 0 0 1-3.4 0L2.7 8.7a2.41 2.41 0 0 1 0-3.4l2.6-2.6a2.41 2.41 0 0 1 3.4 0Z",
            ),
            path_from_d("m14.5 12.5 2-2"),
            path_from_d("m11.5 9.5 2-2"),
            path_from_d("m8.5 6.5 2-2"),
            path_from_d("m17.5 15.5 2-2"),
        ],
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
        Icon::PanelBottom => vec![
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            path_from_d("M3 15h18"),
        ],
        Icon::EyeOff => vec![
            path_from_d(
                "M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49",
            ),
            path_from_d("M14.084 14.158a3 3 0 0 1-4.242-4.242"),
            path_from_d(
                "M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143",
            ),
            path_from_d("m2 2 20 20"),
        ],
        Icon::Phone => vec![path_from_d(
            "M13.832 16.568a1 1 0 0 0 1.213-.303l.355-.465A2 2 0 0 1 17 15h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2A18 18 0 0 1 2 4a2 2 0 0 1 2-2h3a2 2 0 0 1 2 2v3a2 2 0 0 1-.8 1.6l-.468.351a1 1 0 0 0-.292 1.233 14 14 0 0 0 6.392 6.384",
        )],
        Icon::Link => vec![
            path_from_d("M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"),
            path_from_d("M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"),
        ],
        Icon::MapPin => vec![
            path_from_d(
                "M20 10c0 4.993-5.539 10.193-7.399 11.799a1 1 0 0 1-1.202 0C9.539 20.193 4 14.993 4 10a8 8 0 0 1 16 0",
            ),
            circle_path(12.0, 10.0, 3.0),
        ],
        Icon::QrCode => vec![
            rect_path(3.0, 3.0, 5.0, 5.0, 1.0),
            rect_path(16.0, 3.0, 5.0, 5.0, 1.0),
            rect_path(3.0, 16.0, 5.0, 5.0, 1.0),
            path_from_d("M21 16h-3a2 2 0 0 0-2 2v3"),
            path_from_d("M21 21v.01"),
            path_from_d("M12 7v3a2 2 0 0 1-2 2H7"),
            path_from_d("M3 12h.01"),
            path_from_d("M12 3h.01"),
            path_from_d("M12 16v.01"),
            path_from_d("M16 12h1"),
            path_from_d("M21 12v.01"),
            path_from_d("M12 21v-1"),
        ],
        Icon::Maximize => vec![
            path_from_d("M8 3H5a2 2 0 0 0-2 2v3"),
            path_from_d("M21 8V5a2 2 0 0 0-2-2h-3"),
            path_from_d("M3 16v3a2 2 0 0 0 2 2h3"),
            path_from_d("M16 21h3a2 2 0 0 0 2-2v-3"),
        ],
        Icon::Minimize => vec![
            path_from_d("M8 3v3a2 2 0 0 1-2 2H3"),
            path_from_d("M21 8h-3a2 2 0 0 1-2-2V3"),
            path_from_d("M3 16h3a2 2 0 0 1 2 2v3"),
            path_from_d("M16 21v-3a2 2 0 0 1 2-2h3"),
        ],
        Icon::Fullscreen => vec![
            path_from_d("M3 7V5a2 2 0 0 1 2-2h2"),
            path_from_d("M17 3h2a2 2 0 0 1 2 2v2"),
            path_from_d("M21 17v2a2 2 0 0 1-2 2h-2"),
            path_from_d("M7 21H5a2 2 0 0 1-2-2v-2"),
            rect_path(7.0, 8.0, 10.0, 8.0, 1.0),
        ],
        Icon::ScanLine => vec![
            path_from_d("M3 7V5a2 2 0 0 1 2-2h2"),
            path_from_d("M17 3h2a2 2 0 0 1 2 2v2"),
            path_from_d("M21 17v2a2 2 0 0 1-2 2h-2"),
            path_from_d("M7 21H5a2 2 0 0 1-2-2v-2"),
            path_from_d("M7 12h10"),
        ],
        Icon::SwitchCamera => vec![
            path_from_d("M11 19H4a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h5"),
            path_from_d("M13 5h7a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2h-5"),
            circle_path(12.0, 12.0, 3.0),
            path_from_d("m18 22-3-3 3-3"),
            path_from_d("m6 2 3 3-3 3"),
        ],
        Icon::User => vec![
            path_from_d("M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"),
            circle_path(12.0, 7.0, 4.0),
        ],
        Icon::UserPlus => vec![
            path_from_d("M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"),
            circle_path(9.0, 7.0, 4.0),
            line_path(19.0, 8.0, 19.0, 14.0),
            line_path(22.0, 11.0, 16.0, 11.0),
        ],
        Icon::Command => vec![path_from_d(
            "M15 6v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3",
        )],
        Icon::Hash => vec![
            line_path(4.0, 9.0, 20.0, 9.0),
            line_path(4.0, 15.0, 20.0, 15.0),
            line_path(10.0, 3.0, 8.0, 21.0),
            line_path(16.0, 3.0, 14.0, 21.0),
        ],
        Icon::Smile => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("M8 14s1.5 2 4 2 4-2 4-2"),
            line_path(9.0, 9.0, 9.01, 9.0),
            line_path(15.0, 9.0, 15.01, 9.0),
        ],
        Icon::SmilePlus => vec![
            path_from_d("M22 11v1a10 10 0 1 1-9-10"),
            path_from_d("M8 14s1.5 2 4 2 4-2 4-2"),
            line_path(9.0, 9.0, 9.01, 9.0),
            line_path(15.0, 9.0, 15.01, 9.0),
            path_from_d("M16 5h6"),
            path_from_d("M19 2v6"),
        ],
        Icon::Video => vec![
            path_from_d(
                "m16 13 5.223 3.482a.5.5 0 0 0 .777-.416V7.87a.5.5 0 0 0-.752-.432L16 10.5",
            ),
            rect_path(2.0, 6.0, 14.0, 12.0, 2.0),
        ],
        Icon::PhoneOff => vec![
            path_from_d(
                "M10.1 13.9a14 14 0 0 0 3.732 2.668 1 1 0 0 0 1.213-.303l.355-.465A2 2 0 0 1 17 15h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2 18 18 0 0 1-12.728-5.272",
            ),
            path_from_d("M22 2 2 22"),
            path_from_d(
                "M4.76 13.582A18 18 0 0 1 2 4a2 2 0 0 1 2-2h3a2 2 0 0 1 2 2v3a2 2 0 0 1-.8 1.6l-.468.351a1 1 0 0 0-.292 1.233 14 14 0 0 0 .244.473",
            ),
        ],
        Icon::Lock => vec![
            rect_path(3.0, 11.0, 18.0, 11.0, 2.0),
            path_from_d("M7 11V7a5 5 0 0 1 10 0v4"),
        ],
        Icon::MessageSquare => vec![path_from_d(
            "M22 17a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 21.286V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2z",
        )],
        Icon::MessageCircle => vec![path_from_d(
            "M2.992 16.342a2 2 0 0 1 .094 1.167l-1.065 3.29a1 1 0 0 0 1.236 1.168l3.413-.998a2 2 0 0 1 1.099.092 10 10 0 1 0-4.777-4.719",
        )],
        Icon::Clock => vec![path_from_d("M12 6v6l4 2"), circle_path(12.0, 12.0, 10.0)],
        Icon::AlarmClock => vec![
            circle_path(12.0, 13.0, 8.0),
            path_from_d("M12 9v4l2 2"),
            path_from_d("M5 3 2 6"),
            path_from_d("m22 6-3-3"),
            path_from_d("M6.38 18.7 4 21"),
            path_from_d("M17.64 18.67 20 21"),
        ],
        Icon::Hourglass => vec![
            path_from_d("M5 22h14"),
            path_from_d("M5 2h14"),
            path_from_d(
                "M17 22v-4.172a2 2 0 0 0-.586-1.414L12 12l-4.414 4.414A2 2 0 0 0 7 17.828V22",
            ),
            path_from_d("M7 2v4.172a2 2 0 0 0 .586 1.414L12 12l4.414-4.414A2 2 0 0 0 17 6.172V2"),
        ],
        Icon::Bookmark => vec![path_from_d(
            "m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z",
        )],
        Icon::Tag => vec![
            path_from_d(
                "M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z",
            ),
            circle_path(7.5, 7.5, 0.5),
        ],
        Icon::Rss => vec![
            path_from_d("M4 11a9 9 0 0 1 9 9"),
            path_from_d("M4 4a16 16 0 0 1 16 16"),
            circle_path(5.0, 19.0, 1.0),
        ],
        Icon::Upload => vec![
            path_from_d("M12 3v12"),
            path_from_d("m17 8-5-5-5 5"),
            path_from_d("M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"),
        ],
        Icon::Type => vec![
            path_from_d("M12 4v16"),
            path_from_d("M4 7V5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v2"),
            path_from_d("M9 20h6"),
        ],
        Icon::Box => vec![
            path_from_d(
                "M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z",
            ),
            path_from_d("m3.3 7 8.7 5 8.7-5"),
            path_from_d("M12 22V12"),
        ],
        Icon::Contrast => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("M12 18a6 6 0 0 0 0-12v12z"),
        ],
        Icon::Columns2 => vec![
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            path_from_d("M12 3v18"),
        ],
        Icon::Palette => vec![
            path_from_d(
                "M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8z",
            ),
            circle_path(13.5, 6.5, 0.5),
            circle_path(17.5, 10.5, 0.5),
            circle_path(6.5, 12.5, 0.5),
            circle_path(8.5, 7.5, 0.5),
        ],
        Icon::Crop => vec![
            path_from_d("M6 2v14a2 2 0 0 0 2 2h14"),
            path_from_d("M18 22V8a2 2 0 0 0-2-2H2"),
        ],
        Icon::Usb => vec![
            circle_path(10.0, 7.0, 1.0),
            circle_path(4.0, 20.0, 1.0),
            path_from_d("M4.7 19.3 19 5"),
            path_from_d("m21 3-3 1 2 2Z"),
            path_from_d("M9.26 7.68 5 12l2 5"),
            path_from_d("m10 14 5 2 3.5-3.5"),
            path_from_d("m18 12 1-1 1 1-1 1Z"),
        ],
        Icon::CircleCheck => vec![circle_path(12.0, 12.0, 10.0), path_from_d("m9 12 2 2 4-4")],
        Icon::Disc3 => vec![
            circle_path(12.0, 12.0, 10.0),
            path_from_d("M6 12c0-1.7.7-3.2 1.8-4.2"),
            circle_path(12.0, 12.0, 2.0),
            path_from_d("M18 12c0 1.7-.7 3.2-1.8 4.2"),
        ],
        Icon::Bold => vec![path_from_d(
            "M6 12h9a4 4 0 0 1 0 8H7a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h7a4 4 0 0 1 0 8",
        )],
        Icon::Italic => vec![
            line_path(19.0, 4.0, 10.0, 4.0),
            line_path(14.0, 20.0, 5.0, 20.0),
            line_path(15.0, 4.0, 9.0, 20.0),
        ],
        Icon::Underline => vec![
            path_from_d("M6 4v6a6 6 0 0 0 12 0V4"),
            line_path(4.0, 20.0, 20.0, 20.0),
        ],
        Icon::Strikethrough => vec![
            path_from_d("M16 4H9a3 3 0 0 0-2.83 4"),
            path_from_d("M14 12a4 4 0 0 1 0 8H6"),
            line_path(4.0, 12.0, 20.0, 12.0),
        ],
        Icon::Superscript => vec![
            path_from_d("m4 19 8-8"),
            path_from_d("m12 19-8-8"),
            path_from_d(
                "M20 12h-4c0-1.5.442-2 1.5-2.5S20 8.334 20 7.002c0-.472-.17-.93-.484-1.29a2.105 2.105 0 0 0-2.617-.436c-.42.239-.738.614-.899 1.06",
            ),
        ],
        Icon::Subscript => vec![
            path_from_d("m4 5 8 8"),
            path_from_d("m12 5-8 8"),
            path_from_d(
                "M20 19h-4c0-1.5.44-2 1.5-2.5S20 15.33 20 14c0-.47-.17-.93-.48-1.29a2.11 2.11 0 0 0-2.62-.44c-.42.24-.74.62-.9 1.07",
            ),
        ],
        Icon::Heading1 => vec![
            path_from_d("M4 12h8"),
            path_from_d("M4 18V6"),
            path_from_d("M12 18V6"),
            path_from_d("m17 12 3-2v8"),
        ],
        Icon::Heading2 => vec![
            path_from_d("M4 12h8"),
            path_from_d("M4 18V6"),
            path_from_d("M12 18V6"),
            path_from_d("M21 18h-4c0-4 4-3 4-6 0-1.5-2-2.5-4-1"),
        ],
        Icon::ListOrdered => vec![
            path_from_d("M11 5h10"),
            path_from_d("M11 12h10"),
            path_from_d("M11 19h10"),
            path_from_d("M4 4h1v5"),
            path_from_d("M4 9h2"),
            path_from_d("M6.5 20H3.4c0-1 2.6-1.925 2.6-3.5a1.5 1.5 0 0 0-2.6-1.02"),
        ],
        Icon::ListChecks => vec![
            path_from_d("M13 5h8"),
            path_from_d("M13 12h8"),
            path_from_d("M13 19h8"),
            path_from_d("m3 17 2 2 4-4"),
            path_from_d("m3 7 2 2 4-4"),
        ],
        Icon::Quote => vec![
            path_from_d(
                "M16 3a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2 1 1 0 0 1 1 1v1a2 2 0 0 1-2 2 1 1 0 0 0-1 1v2a1 1 0 0 0 1 1 6 6 0 0 0 6-6V5a2 2 0 0 0-2-2z",
            ),
            path_from_d(
                "M5 3a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2 1 1 0 0 1 1 1v1a2 2 0 0 1-2 2 1 1 0 0 0-1 1v2a1 1 0 0 0 1 1 6 6 0 0 0 6-6V5a2 2 0 0 0-2-2z",
            ),
        ],
        Icon::ReplyAll => vec![
            path_from_d("m12 17-5-5 5-5"),
            path_from_d("M22 18v-2a4 4 0 0 0-4-4H7"),
            path_from_d("m7 17-5-5 5-5"),
        ],
        Icon::Forward => vec![
            path_from_d("m15 17 5-5-5-5"),
            path_from_d("M4 18v-2a4 4 0 0 1 4-4h12"),
        ],
        Icon::MailOpen => vec![
            path_from_d(
                "M21.2 8.4c.5.38.8.97.8 1.6v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V10a2 2 0 0 1 .8-1.6l8-6a2 2 0 0 1 2.4 0l8 6Z",
            ),
            path_from_d("m22 10-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 10"),
        ],
        Icon::AlignLeft => vec![
            path_from_d("M21 5H3"),
            path_from_d("M15 12H3"),
            path_from_d("M17 19H3"),
        ],
        Icon::AlignCenter => vec![
            path_from_d("M21 5H3"),
            path_from_d("M17 12H7"),
            path_from_d("M19 19H5"),
        ],
        Icon::AlignRight => vec![
            path_from_d("M21 5H3"),
            path_from_d("M21 12H9"),
            path_from_d("M21 19H7"),
        ],
        Icon::AlignJustify => vec![
            path_from_d("M3 5h18"),
            path_from_d("M3 12h18"),
            path_from_d("M3 19h18"),
        ],
        Icon::BringToFront => vec![
            rect_path(8.0, 8.0, 8.0, 8.0, 2.0),
            path_from_d("M4 10a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2"),
            path_from_d("M14 20a2 2 0 0 0 2 2h4a2 2 0 0 0 2-2v-4a2 2 0 0 0-2-2"),
        ],
        Icon::SendToBack => vec![
            rect_path(14.0, 14.0, 8.0, 8.0, 2.0),
            rect_path(2.0, 2.0, 8.0, 8.0, 2.0),
            path_from_d("M7 14v1a2 2 0 0 0 2 2h1"),
            path_from_d("M14 7h1a2 2 0 0 1 2 2v1"),
        ],
        Icon::Baseline => vec![
            path_from_d("M4 20h16"),
            path_from_d("m6 16 6-12 6 12"),
            path_from_d("M8 12h8"),
        ],
        Icon::Highlighter => vec![
            path_from_d("m9 11-6 6v3h9l3-3"),
            path_from_d("m22 12-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4"),
        ],
        Icon::Table => vec![
            path_from_d("M12 3v18"),
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
            path_from_d("M3 9h18"),
            path_from_d("M3 15h18"),
        ],
        Icon::ListTree => vec![
            path_from_d("M8 5h13"),
            path_from_d("M13 12h8"),
            path_from_d("M13 19h8"),
            path_from_d("M3 10a2 2 0 0 0 2 2h3"),
            path_from_d("M3 5v12a2 2 0 0 0 2 2h3"),
        ],
        Icon::Shuffle => vec![
            path_from_d("m18 14 4 4-4 4"),
            path_from_d("m18 2 4 4-4 4"),
            path_from_d("M2 18h1.973a4 4 0 0 0 3.3-1.7l5.454-8.6a4 4 0 0 1 3.3-1.7H22"),
            path_from_d("M2 6h1.972a4 4 0 0 1 3.6 2.2"),
            path_from_d("M22 18h-6.041a4 4 0 0 1-3.3-1.8l-.359-.45"),
        ],
        Icon::Repeat => vec![
            path_from_d("m17 2 4 4-4 4"),
            path_from_d("M3 11v-1a4 4 0 0 1 4-4h14"),
            path_from_d("m7 22-4-4 4-4"),
            path_from_d("M21 13v1a4 4 0 0 1-4 4H3"),
        ],
        Icon::Repeat1 => vec![
            path_from_d("m17 2 4 4-4 4"),
            path_from_d("M3 11v-1a4 4 0 0 1 4-4h14"),
            path_from_d("m7 22-4-4 4-4"),
            path_from_d("M21 13v1a4 4 0 0 1-4 4H3"),
            path_from_d("M11 10h1v4"),
        ],
        Icon::Heart => vec![path_from_d(
            "M2 9.5a5.5 5.5 0 0 1 9.591-3.676.56.56 0 0 0 .818 0A5.49 5.49 0 0 1 22 9.5c0 2.29-1.5 4-3 5.5l-5.492 5.313a2 2 0 0 1-3 .019L5 15c-1.5-1.5-3-3.2-3-5.5",
        )],
        Icon::ListMusic => vec![
            path_from_d("M16 5H3"),
            path_from_d("M11 12H3"),
            path_from_d("M11 19H3"),
            path_from_d("M21 16V5"),
            circle_path(18.0, 16.0, 3.0),
        ],
        Icon::Speaker => vec![
            rect_path(4.0, 2.0, 16.0, 20.0, 2.0),
            path_from_d("M12 6h.01"),
            circle_path(12.0, 14.0, 4.0),
            path_from_d("M12 14h.01"),
        ],
        Icon::Headphones => vec![path_from_d(
            "M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3",
        )],
        Icon::HeadphoneOff => vec![
            path_from_d("M21 14h-1.343"),
            path_from_d("M9.128 3.47A9 9 0 0 1 21 12v3.343"),
            line_path(2.0, 2.0, 22.0, 22.0),
            path_from_d("M20.414 20.414A2 2 0 0 1 19 21h-1a2 2 0 0 1-2-2v-3"),
            path_from_d(
                "M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 2.636-6.364",
            ),
        ],
        Icon::Gauge => vec![
            path_from_d("m12 14 4-4"),
            path_from_d("M3.34 19a10 10 0 1 1 17.32 0"),
        ],
        Icon::LocateFixed => vec![
            line_path(2.0, 12.0, 5.0, 12.0),
            line_path(19.0, 12.0, 22.0, 12.0),
            line_path(12.0, 2.0, 12.0, 5.0),
            line_path(12.0, 19.0, 12.0, 22.0),
            circle_path(12.0, 12.0, 7.0),
            circle_path(12.0, 12.0, 3.0),
        ],
        Icon::Navigation => vec![path_from_d("M3 11 L22 2 L13 21 L11 13 L3 11 Z")],
        Icon::Flag => vec![path_from_d(
            "M4 22V4a1 1 0 0 1 .4-.8A6 6 0 0 1 8 2c3 0 5 2 7.333 2q2 0 3.067-.8A1 1 0 0 1 20 4v10a1 1 0 0 1-.4.8A6 6 0 0 1 16 16c-3 0-5-2-8-2a6 6 0 0 0-4 1.528",
        )],
        Icon::CornerUpLeft => vec![
            path_from_d("M20 20v-7a4 4 0 0 0-4-4H4"),
            path_from_d("M9 14 4 9l5-5"),
        ],
        Icon::CornerUpRight => vec![
            path_from_d("m15 14 5-5-5-5"),
            path_from_d("M4 20v-7a4 4 0 0 1 4-4h12"),
        ],
        Icon::ArrowUpLeft => vec![path_from_d("M7 17V7h10"), path_from_d("M17 17 7 7")],
        Icon::ArrowUpRight => vec![path_from_d("M7 7h10v10"), path_from_d("M7 17 17 7")],
        Icon::ArrowDownLeft => vec![path_from_d("M17 7 7 17"), path_from_d("M17 17H7V7")],
        Icon::ArrowDownRight => vec![path_from_d("m7 7 10 10"), path_from_d("M17 7v10H7")],
        Icon::Coffee => vec![
            path_from_d("M10 2v2"),
            path_from_d("M14 2v2"),
            path_from_d(
                "M16 8a1 1 0 0 1 1 1v8a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1h14a4 4 0 1 1 0 8h-1",
            ),
            path_from_d("M6 2v2"),
        ],
        Icon::Utensils => vec![
            path_from_d("M3 2v7c0 1.1.9 2 2 2h4a2 2 0 0 0 2-2V2"),
            path_from_d("M7 2v20"),
            path_from_d("M21 15V2a5 5 0 0 0-5 5v6c0 1.1.9 2 2 2h3Zm0 0v7"),
        ],
        Icon::Beer => vec![
            path_from_d("M17 11h1a3 3 0 0 1 0 6h-1"),
            path_from_d("M9 12v6"),
            path_from_d("M13 12v6"),
            path_from_d(
                "M14 7.5c-1 0-1.44.5-3 .5s-2-.5-3-.5-1.72.5-2.5.5a2.5 2.5 0 0 1 0-5c.78 0 1.57.5 2.5.5S9.44 2 11 2s2 1.5 3 1.5 1.72-.5 2.5-.5a2.5 2.5 0 0 1 0 5c-.78 0-1.5-.5-2.5-.5Z",
            ),
            path_from_d("M5 8v12a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V8"),
        ],
        Icon::Landmark => vec![
            path_from_d("M10 18v-7"),
            path_from_d(
                "M11.12 2.198a2 2 0 0 1 1.76.006l7.866 3.847c.476.233.31.949-.22.949H3.474c-.53 0-.695-.716-.22-.949z",
            ),
            path_from_d("M14 18v-7"),
            path_from_d("M18 18v-7"),
            path_from_d("M3 22h18"),
            path_from_d("M6 18v-7"),
        ],
        Icon::Trees => vec![
            path_from_d("M10 10v.2A3 3 0 0 1 8.9 16H5a3 3 0 0 1-1-5.8V10a3 3 0 0 1 6 0Z"),
            path_from_d("M7 16v6"),
            path_from_d("M13 19v3"),
            path_from_d(
                "M12 19h8.3a1 1 0 0 0 .7-1.7L18 14h.3a1 1 0 0 0 .7-1.7L16 9h.2a1 1 0 0 0 .8-1.7L13 3l-1.4 1.5",
            ),
        ],
        Icon::TrainFront => vec![
            path_from_d("M8 3.1V7a4 4 0 0 0 8 0V3.1"),
            path_from_d("m9 15-1-1"),
            path_from_d("m15 15 1-1"),
            path_from_d("M9 19c-2.8 0-5-2.2-5-5v-4a8 8 0 0 1 16 0v4c0 2.8-2.2 5-5 5Z"),
            path_from_d("m8 19-2 3"),
            path_from_d("m16 19 2 3"),
        ],
        Icon::BedDouble => vec![
            path_from_d("M2 20v-8a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v8"),
            path_from_d("M4 10V6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v4"),
            path_from_d("M12 4v6"),
            path_from_d("M2 18h20"),
        ],
        Icon::ShoppingBag => vec![
            path_from_d("M16 10a4 4 0 0 1-8 0"),
            path_from_d("M3.103 6.034h17.794"),
            path_from_d(
                "M3.4 5.467a2 2 0 0 0-.4 1.2V20a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V6.667a2 2 0 0 0-.4-1.2l-2-2.667A2 2 0 0 0 17 2H7a2 2 0 0 0-1.6.8z",
            ),
        ],
        Icon::Hospital => vec![
            path_from_d("M12 7v4"),
            path_from_d("M14 21v-3a2 2 0 0 0-4 0v3"),
            path_from_d("M14 9h-4"),
            path_from_d(
                "M18 11h2a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2v-9a2 2 0 0 1 2-2h2",
            ),
            path_from_d("M18 21V5a2 2 0 0 0-2-2H8a2 2 0 0 0-2 2v16"),
        ],
        Icon::Droplet => vec![path_from_d(
            "M12 22a7 7 0 0 0 7-7c0-2-1-3.9-3-5.5s-3.5-4-4-6.5c-.5 2.5-2 4.9-4 6.5C6 11.1 5 13 5 15a7 7 0 0 0 7 7z",
        )],
        Icon::Wind => vec![
            path_from_d("M12.8 19.6A2 2 0 1 0 14 16H2"),
            path_from_d("M17.5 8a2.5 2.5 0 1 1 2 4H2"),
            path_from_d("M9.8 4.4A2 2 0 1 1 11 8H2"),
        ],
        Icon::Cloud => vec![path_from_d(
            "M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z",
        )],
        Icon::CloudRain => vec![
            path_from_d("M4 14.899A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.242"),
            path_from_d("M16 14v6"),
            path_from_d("M8 14v6"),
            path_from_d("M12 16v6"),
        ],
        Icon::CloudSnow => vec![
            path_from_d("M4 14.899A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.242"),
            path_from_d("M8 15h.01"),
            path_from_d("M8 19h.01"),
            path_from_d("M12 17h.01"),
            path_from_d("M12 21h.01"),
            path_from_d("M16 15h.01"),
            path_from_d("M16 19h.01"),
        ],
        Icon::Moon => vec![path_from_d(
            "M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401",
        )],
        Icon::Hand => vec![
            path_from_d("M18 11V6a2 2 0 0 0-2-2a2 2 0 0 0-2 2"),
            path_from_d("M14 10V4a2 2 0 0 0-2-2a2 2 0 0 0-2 2v2"),
            path_from_d("M10 10.5V6a2 2 0 0 0-2-2a2 2 0 0 0-2 2v8"),
            path_from_d(
                "M18 8a2 2 0 1 1 4 0v6a8 8 0 0 1-8 8h-2c-2.8 0-4.5-.86-5.99-2.34l-3.6-3.6a2 2 0 0 1 2.83-2.82L7 15",
            ),
        ],
        Icon::PawPrint => vec![
            circle_path(11.0, 4.0, 2.0),
            circle_path(18.0, 8.0, 2.0),
            circle_path(20.0, 16.0, 2.0),
            path_from_d(
                "M9 10a5 5 0 0 1 5 5v3.5a3.5 3.5 0 0 1-6.84 1.045Q6.52 17.48 4.46 16.84A3.5 3.5 0 0 1 5.5 10Z",
            ),
        ],
        Icon::Plane => vec![path_from_d(
            "M17.8 19.2 16 11l3.5-3.5C21 6 21.5 4 21 3c-1-.5-3 0-4.5 1.5L13 8 4.8 6.2c-.5-.1-.9.1-1.1.5l-.3.5c-.2.5-.1 1 .3 1.3L9 12l-2 3H4l-1 1 3 2 2 3 1-1v-3l3-2 3.5 5.3c.3.4.8.5 1.3.3l.5-.2c.4-.3.6-.7.5-1.2z",
        )],
        Icon::Trophy => vec![
            path_from_d("M10 14.66v1.626a2 2 0 0 1-.976 1.696A5 5 0 0 0 7 21.978"),
            path_from_d("M14 14.66v1.626a2 2 0 0 0 .976 1.696A5 5 0 0 1 17 21.978"),
            path_from_d("M18 9h1.5a1 1 0 0 0 0-5H18"),
            path_from_d("M4 22h16"),
            path_from_d("M6 9a6 6 0 0 0 12 0V3a1 1 0 0 0-1-1H7a1 1 0 0 0-1 1z"),
            path_from_d("M6 9H4.5a1 1 0 0 1 0-5H6"),
        ],
        Icon::Lightbulb => vec![
            path_from_d(
                "M15 14c.2-1 .7-1.7 1.5-2.5 1-.9 1.5-2.2 1.5-3.5A6 6 0 0 0 6 8c0 1 .2 2.2 1.5 3.5.7.7 1.3 1.5 1.5 2.5",
            ),
            path_from_d("M9 18h6"),
            path_from_d("M10 22h4"),
        ],
        Icon::SdCard => vec![
            path_from_d("M6 22a2 2 0 0 1-2-2V6l4-4h10a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2Z"),
            path_from_d("M8 10V7"),
            path_from_d("M12 10V7"),
            path_from_d("M16 10V7"),
        ],
        Icon::Brush => vec![
            path_from_d("m11 10 3 3"),
            path_from_d("M6.5 21A3.5 3.5 0 1 0 3 17.5a2.62 2.62 0 0 1-.708 1.792A1 1 0 0 0 3 21z"),
            path_from_d("M9.969 17.031 21.378 5.624a1 1 0 0 0-3.002-3.002L6.967 14.031"),
        ],
        Icon::Circle => vec![circle_path(12.0, 12.0, 10.0)],
        Icon::Slash => vec![path_from_d("M22 2 2 22")],
        Icon::Pipette => vec![
            path_from_d(
                "m12 9-8.414 8.414A2 2 0 0 0 3 18.828v1.344a2 2 0 0 1-.586 1.414A2 2 0 0 1 3.828 21h1.344a2 2 0 0 0 1.414-.586L15 12",
            ),
            path_from_d(
                "m18 9 .4.4a1 1 0 1 1-3 3l-3.8-3.8a1 1 0 1 1 3-3l.4.4 3.4-3.4a1 1 0 1 1 3 3z",
            ),
            path_from_d("m2 22 .414-.414"),
        ],
        Icon::Eraser => vec![
            path_from_d(
                "M21 21H8a2 2 0 0 1-1.42-.587l-3.994-3.999a2 2 0 0 1 0-2.828l10-10a2 2 0 0 1 2.829 0l5.999 6a2 2 0 0 1 0 2.828L12.834 21",
            ),
            path_from_d("m5.082 11.09 8.828 8.828"),
        ],
        Icon::LockOpen => vec![
            rect_path(3.0, 11.0, 18.0, 11.0, 2.0),
            path_from_d("M7 11V7a5 5 0 0 1 9.9-1"),
        ],
        Icon::Discord => vec![discord_mark()],
        Icon::Unlink => vec![
            path_from_d(
                "m18.84 12.25 1.72-1.71h-.02a5.004 5.004 0 0 0-.12-7.07 5.006 5.006 0 0 0-6.95 0l-1.72 1.71",
            ),
            path_from_d(
                "m5.17 11.75-1.71 1.71a5.004 5.004 0 0 0 .12 7.07 5.006 5.006 0 0 0 6.95 0l1.71-1.71",
            ),
            line_path(8.0, 2.0, 8.0, 5.0),
            line_path(2.0, 8.0, 5.0, 8.0),
            line_path(16.0, 19.0, 16.0, 22.0),
            line_path(19.0, 16.0, 22.0, 16.0),
        ],
        Icon::Bot => vec![
            path_from_d("M12 8V4H8"),
            rect_path(4.0, 8.0, 16.0, 12.0, 2.0),
            path_from_d("M2 14h2"),
            path_from_d("M20 14h2"),
            path_from_d("M15 13v2"),
            path_from_d("M9 13v2"),
        ],
        Icon::SquareDashed => [
            "M5 3a2 2 0 0 0-2 2",
            "M19 3a2 2 0 0 1 2 2",
            "M21 19a2 2 0 0 1-2 2",
            "M5 21a2 2 0 0 1-2-2",
            "M9 3h1",
            "M9 21h1",
            "M14 3h1",
            "M14 21h1",
            "M3 9v1",
            "M21 9v1",
            "M3 14v1",
            "M21 14v1",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::CircleDashed => [
            "M10.1 2.182a10 10 0 0 1 3.8 0",
            "M13.9 21.818a10 10 0 0 1-3.8 0",
            "M17.609 3.721a10 10 0 0 1 2.69 2.7",
            "M2.182 13.9a10 10 0 0 1 0-3.8",
            "M20.279 17.609a10 10 0 0 1-2.7 2.69",
            "M21.818 10.1a10 10 0 0 1 0 3.8",
            "M3.721 6.391a10 10 0 0 1 2.7-2.69",
            "M6.391 20.279a10 10 0 0 1-2.69-2.7",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::Lasso => vec![
            path_from_d("M7 22a5 5 0 0 1-2-4"),
            path_from_d("M7 16.93c.96.43 1.96.74 2.99.91"),
            path_from_d(
                "M3.34 14A6.8 6.8 0 0 1 2 10c0-4.42 4.48-8 10-8s10 3.58 10 8-4.48 8-10 8a12 12 0 0 1-1.57-.1",
            ),
            circle_path(5.0, 16.0, 2.0),
        ],
        Icon::WandSparkles => [
            "m21.64 3.64-1.28-1.28a1.21 1.21 0 0 0-1.72 0L2.36 18.64a1.21 1.21 0 0 0 0 1.72l1.28 1.28a1.2 1.2 0 0 0 1.72 0L21.64 5.36a1.2 1.2 0 0 0 0-1.72",
            "m14 7 3 3",
            "M5 6v4",
            "M19 14v4",
            "M10 2v2",
            "M7 8H3",
            "M21 16h-4",
            "M11 3H9",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::PaintBucket => [
            "m19 11-8-8-8.6 8.6a2 2 0 0 0 0 2.8l5.2 5.2c.8.8 2 .8 2.8 0L19 11Z",
            "m5 2 5 5",
            "M2 13h15",
            "M22 20a2 2 0 1 1-4 0c0-1.6 1.7-2.4 2-4 .3 1.6 2 2.4 2 4Z",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::SquaresUnite => vec![path_from_d(
            "M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v3a1 1 0 0 0 1 1h3a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2v-3a1 1 0 0 0-1-1z",
        )],
        Icon::SquaresSubtract => [
            "M10 22a2 2 0 0 1-2-2",
            "M16 22h-2",
            "M16 4a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h3a1 1 0 0 0 1-1v-5a2 2 0 0 1 2-2h5a1 1 0 0 0 1-1z",
            "M20 8a2 2 0 0 1 2 2",
            "M22 14v2",
            "M22 20a2 2 0 0 1-2 2",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::SquaresIntersect => [
            "M10 22a2 2 0 0 1-2-2",
            "M14 2a2 2 0 0 1 2 2",
            "M16 22h-2",
            "M2 10V8",
            "M2 4a2 2 0 0 1 2-2",
            "M20 8a2 2 0 0 1 2 2",
            "M22 14v2",
            "M22 20a2 2 0 0 1-2 2",
            "M4 16a2 2 0 0 1-2-2",
            "M8 10a2 2 0 0 1 2-2h5a1 1 0 0 1 1 1v5a2 2 0 0 1-2 2H9a1 1 0 0 1-1-1z",
            "M8 2h2",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::SquaresExclude => [
            "M16 12v2a2 2 0 0 1-2 2H9a1 1 0 0 0-1 1v3a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V10a2 2 0 0 0-2-2h0",
            "M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v3a1 1 0 0 1-1 1h-5a2 2 0 0 0-2 2v2",
        ]
        .into_iter()
        .map(path_from_d)
        .collect(),
        Icon::SquareTerminal => vec![
            path_from_d("m7 11 2-2-2-2"),
            path_from_d("M11 13h4"),
            rect_path(3.0, 3.0, 18.0, 18.0, 2.0),
        ],
        Icon::ScreenShare => vec![
            path_from_d("M13 3H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-3"),
            path_from_d("M8 21h8"),
            path_from_d("M12 17v4"),
            path_from_d("m17 8 5-5"),
            path_from_d("M17 3h5v5"),
        ],
        Icon::AppWindow => vec![
            rect_path(2.0, 4.0, 20.0, 16.0, 2.0),
            path_from_d("M10 4v4"),
            path_from_d("M2 8h20"),
            path_from_d("M6 4v4"),
        ],
        Icon::CircleDot => vec![circle_path(12.0, 12.0, 10.0), circle_path(12.0, 12.0, 1.0)],
        Icon::GalleryHorizontal => vec![
            path_from_d("M2 3v18"),
            rect_path(6.0, 3.0, 12.0, 18.0, 2.0),
            path_from_d("M22 3v18"),
        ],
        Icon::MonitorSmartphone => vec![
            path_from_d("M18 8V6a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h8"),
            path_from_d("M10 19v-3.96 3.15"),
            path_from_d("M7 19h5"),
            rect_path(16.0, 12.0, 6.0, 10.0, 2.0),
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
        // A brand mark fills even-odd, so its inner loops (Discord's eyes)
        // stay open; Lucide glyphs keep their non-zero fill.
        let rule = if self.is_brand() {
            Fill::EvenOdd
        } else {
            Fill::NonZero
        };
        for sub in icon_subpaths(self) {
            scene.fill(rule, local, color, None, &sub);
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

    #[test]
    fn an_icon_svg_is_a_document_of_the_glyph_on_a_tile() {
        let svg = icon_svg(Icon::FileText);
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.ends_with("</svg>"));
        assert_eq!(
            svg.matches("<path d=\"").count(),
            icon_subpaths(Icon::FileText).len()
        );
    }

    #[test]
    fn every_glyph_is_found_by_its_lucide_name() {
        for name in [
            "layout-grid",
            "settings-2",
            "mouse",
            "keyboard",
            "users",
            "printer",
            "globe",
            "wifi",
            "bluetooth",
            "monitor",
            "battery",
            "shield",
            "maximize-2",
            "minimize-2",
            "x",
            "chevron-up",
            "chevron-down",
            "chevron-left",
            "plus",
            "search",
            "pen-tool",
            "code",
            "terminal",
            "file-text",
            "bar-chart",
            "layers",
            "settings",
            "home",
            "camera",
            "power",
            "folder",
            "folder-open",
            "file",
            "image",
            "music",
            "film",
            "archive",
            "file-code",
            "arrow-left",
            "arrow-right",
            "arrow-up",
            "arrow-down",
            "chevron-right",
            "rotate-ccw",
            "rotate-cw",
            "copy",
            "scissors",
            "clipboard-paste",
            "trash-2",
            "pencil",
            "square",
            "minus",
            "folder-plus",
            "file-plus",
            "list",
            "star",
            "pin",
            "hard-drive",
            "check",
            "flame",
            "gallery-vertical-end",
            "history",
            "timer",
            "thumbs-up",
            "thumbs-down",
            "list-video",
            "picture-in-picture-2",
            "sparkles",
            "eye",
            "mouse-pointer-2",
            "menu",
            "info",
            "cloud-download",
            "arrow-up-down",
            "bell",
            "play",
            "pause",
            "refresh",
            "external-link",
            "trash",
            "more-horizontal",
            "inbox",
            "send",
            "reply",
            "mail",
            "paperclip",
            "calendar",
            "ban",
            "download",
            "sun",
            "volume-2",
            "volume-x",
            "wifi-off",
            "ethernet-port",
            "mic",
            "mic-off",
            "skip-forward",
            "skip-back",
            "arrow-big-up",
            "delete",
            "corner-down-left",
            "undo-2",
            "redo-2",
            "brush",
            "circle",
            "slash",
            "type",
            "pipette",
            "eraser",
            "eye-off",
            "lock",
            "lock-open",
            "bot",
            "user",
            "flag",
            "droplet",
            "palette",
            "panel-right",
            "panel-bottom",
            "square-dashed",
            "circle-dashed",
            "lasso",
            "wand-sparkles",
            "paint-bucket",
            "contrast",
            "squares-unite",
            "squares-subtract",
            "squares-intersect",
            "squares-exclude",
        ] {
            let icon = Icon::from_name(name).unwrap_or_else(|| panic!("{name} names a glyph"));
            assert_eq!(icon.name(), name);
        }
        assert_eq!(Icon::from_name("no-such-glyph"), None);
    }

    /// Walk a path's elements; returns the count so tests can assert
    /// "non-empty" without matching against internal enum shapes.
    fn element_count(p: &BezPath) -> usize {
        p.elements().iter().count()
    }

    /// `ALL` lists every variant (the enum's source is the census), and
    /// every name finds its icon again.
    #[test]
    fn the_drawing_set_draws() {
        for icon in [
            Icon::Brush,
            Icon::Circle,
            Icon::Slash,
            Icon::Pipette,
            Icon::Eraser,
            Icon::LockOpen,
            Icon::Bot,
            Icon::SquareDashed,
            Icon::CircleDashed,
            Icon::Lasso,
            Icon::WandSparkles,
            Icon::PaintBucket,
            Icon::SquaresUnite,
            Icon::SquaresSubtract,
            Icon::SquaresIntersect,
            Icon::SquaresExclude,
        ] {
            let sub = icon_subpaths(icon);
            assert!(!sub.is_empty(), "{icon:?} produced no subpaths");
            assert!(sub.iter().all(|p| element_count(p) > 0), "{icon:?}");
        }
    }

    #[test]
    fn every_icon_is_found_by_its_name() {
        let names: std::collections::HashSet<_> = Icon::ALL.iter().map(|i| i.name()).collect();
        assert_eq!(names.len(), Icon::ALL.len(), "two icons share a name");
        for icon in Icon::ALL {
            assert_eq!(Icon::from_name(icon.name()), Some(icon));
        }
        assert_eq!(Icon::LayoutGrid.name(), "layout-grid");
        assert_eq!(Icon::Settings2.name(), "settings-2");
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
            assert_eq!(
                icon_subpaths(Icon::BellOff).len(),
                4,
                "bell-off = clapper + two dome halves + slash"
            );
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
                Icon::PanelBottom,
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
    fn everyday_icons_have_their_lucide_subpath_counts() {
        // Each glyph keeps every element of its Lucide source, and every
        // element is a non-empty stroke.
        let cases = [
            (Icon::EyeOff, 4),
            (Icon::Phone, 1),
            (Icon::Link, 2),
            (Icon::MapPin, 2),
            (Icon::QrCode, 12),
            (Icon::ScanLine, 5),
            (Icon::Maximize, 4),
            (Icon::Minimize, 4),
            (Icon::SwitchCamera, 5),
            (Icon::User, 2),
            (Icon::UserPlus, 4),
            (Icon::Hash, 4),
            (Icon::Smile, 4),
            (Icon::SmilePlus, 6),
            (Icon::Video, 2),
            (Icon::PhoneOff, 3),
            (Icon::Lock, 2),
            (Icon::MessageSquare, 1),
            (Icon::MessageCircle, 1),
            (Icon::ListChecks, 5),
            (Icon::Clock, 2),
            (Icon::AlarmClock, 6),
            (Icon::Hourglass, 4),
            (Icon::Bookmark, 1),
            (Icon::Tag, 2),
            (Icon::Rss, 3),
            (Icon::Upload, 3),
            (Icon::Type, 3),
            (Icon::Box, 3),
            (Icon::Contrast, 2),
            (Icon::Columns2, 2),
            (Icon::Palette, 5),
            (Icon::Crop, 2),
            (Icon::Usb, 7),
            (Icon::CircleCheck, 2),
            (Icon::Circle, 1),
            (Icon::AArrowDown, 4),
            (Icon::AArrowUp, 4),
            (Icon::Ruler, 5),
            (Icon::Disc3, 4),
            (Icon::Bold, 1),
            (Icon::Italic, 3),
            (Icon::Underline, 2),
            (Icon::Strikethrough, 3),
            (Icon::Superscript, 3),
            (Icon::Subscript, 3),
            (Icon::Heading1, 4),
            (Icon::Heading2, 4),
            (Icon::ListOrdered, 6),
            (Icon::Quote, 2),
            (Icon::ReplyAll, 3),
            (Icon::Forward, 2),
            (Icon::MailOpen, 2),
            (Icon::AlignLeft, 3),
            (Icon::AlignCenter, 3),
            (Icon::AlignRight, 3),
            (Icon::AlignJustify, 3),
            (Icon::BringToFront, 3),
            (Icon::SendToBack, 4),
            (Icon::Baseline, 3),
            (Icon::Highlighter, 2),
            (Icon::Table, 4),
            (Icon::ListTree, 5),
            (Icon::Shuffle, 5),
            (Icon::Repeat, 4),
            (Icon::Repeat1, 5),
            (Icon::Heart, 1),
            (Icon::ListMusic, 5),
            (Icon::Speaker, 4),
            (Icon::Headphones, 1),
            (Icon::HeadphoneOff, 5),
            (Icon::Gauge, 2),
            (Icon::LocateFixed, 6),
            (Icon::Navigation, 1),
            (Icon::Flag, 1),
            (Icon::CornerUpLeft, 2),
            (Icon::CornerUpRight, 2),
            (Icon::ArrowUpLeft, 2),
            (Icon::ArrowUpRight, 2),
            (Icon::ArrowDownLeft, 2),
            (Icon::ArrowDownRight, 2),
            (Icon::Coffee, 4),
            (Icon::Utensils, 3),
            (Icon::Beer, 5),
            (Icon::Landmark, 6),
            (Icon::Trees, 4),
            (Icon::TrainFront, 6),
            (Icon::BedDouble, 4),
            (Icon::ShoppingBag, 3),
            (Icon::Hospital, 5),
            (Icon::Droplet, 1),
            (Icon::Wind, 3),
            (Icon::Cloud, 1),
            (Icon::CloudRain, 4),
            (Icon::CloudSnow, 7),
            (Icon::Moon, 1),
            (Icon::Hand, 4),
            (Icon::PawPrint, 4),
            (Icon::Plane, 1),
            (Icon::Trophy, 6),
            (Icon::Lightbulb, 3),
            (Icon::SdCard, 4),
        ];
        for (icon, count) in cases {
            let sub = icon_subpaths(icon);
            assert_eq!(sub.len(), count, "{icon:?}");
            for p in &sub {
                assert!(element_count(p) > 0, "{icon:?} emitted an empty subpath");
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

//! The desktop's top bar: workspace navigation, the centre selector (clock,
//! system monitor, calendar, activity), presence and the status tray.
//!
//! [`TopBarChrome`] implements [`ui::widget::Widget`] and paints onto the
//! root scene. Everything it shows about the device (status, clock, telemetry,
//! overlays, the person's widget choice) comes from the host's
//! [`crate::chrome::DesktopChromeSnapshot`], and everything it asks for goes
//! back as a [`ChromeIntent`]; it never probes or changes the system itself.

use std::collections::HashMap;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::chrome::{
    AppTrayIcon, AppTrayItem, AppTrayRequest, AppTrayStatus, Chrome, ChromeIntent, ClockActivity,
    ClockSnapshot, TelemetrySnapshot, TopBarPresence, TopBarWidget, TopBarWidgets,
};
use crate::i18n::{t, t_args};
use tilo_desktop_model::DesktopTree;
use ui::TextRole;
use ui::color_compat::ColorCompat;
use ui::presence::{PresenceCtx, user_color};
use ui::text_render::TextStyle;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::drag::{DragController, ListMetrics};
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize, ListRowFace, RowLeading};

/// Paint and input share the measured editor geometry, including wrapped Add controls.
#[derive(Default, PartialEq)]
struct EditorGeometry {
    panel: Rect,
    title: Rect,
    hint: Rect,
    rows: Vec<Rect>,
    remove: Vec<Rect>,
    add: [Rect; 3],
    widgets: Vec<TopBarWidget>,
    row_height: f64,
    row_gap: f64,
    compact_rows: bool,
}

fn editor_text_width(width: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded canvas widths use f64 while the shared text renderer measures in f32"
    )]
    {
        width.max(0.0).min(f64::from(f32::MAX)) as f32
    }
}

mod responsive;
use responsive::ResponsiveBar;
mod strips;

/// What the bar leads with: the overview, workspace switches and settings
/// controls, or the workspace's name as one menu that holds them, with the
/// person's own avatar at the other end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarLead {
    Navigation,
    WorkspaceMenu,
}

/// Where a layout stood the bar: in the column above the windows, or over
/// it, hidden by the person and shown while reached for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarStanding {
    InColumn,
    OverColumn,
}

impl BarStanding {
    fn of(overlays: bool) -> Self {
        if overlays {
            Self::OverColumn
        } else {
            Self::InColumn
        }
    }
}

/// A thin strip: every bar control is [`BAR_CONTROL_PX`] (20) with half an
/// `s1` of air above and below, so the bar is 24 logical pixels (48 at scale
/// 2). It was 48, then 40, then 36; what it carries (the workspace, the
/// time, the status glyphs) is a line of text, and every taller step read
/// as a slab over the work area. The bar's controls run to the screen's top
/// edge, where a pointer cannot overshoot them, so their targets keep the
/// pointer floor through their own hit rects rather than a taller bar.
/// Unscaled `BASE` values because layout sizes its node from this constant
/// before any theme is in hand.
pub const TOPBAR_HEIGHT_PX: f32 = BAR_CONTROL_LOGICAL + ui::tokens::Spacing::BASE.s1;

/// The bar's height under `theme`: [`TOPBAR_HEIGHT_PX`], grown with a large
/// text size so the time's line still fits with air above and below. At the
/// default size the line is a little over 20 px and the bar stays 24.
#[must_use]
pub fn topbar_height(theme: &Theme) -> f32 {
    let line = ui::tokens::Typography::default()
        .style(TextRole::Body)
        .line_height_px()
        * theme.text_scale;
    TOPBAR_HEIGHT_PX.max((line + ui::tokens::Spacing::BASE.s1 / 2.0).ceil())
}

const CLOCK_OVERLAY_HEIGHT: f64 = 150.0;

/// Below this width the bar with the workspace menu narrows its centre lane
/// to the time alone.
const NARROW_MENU_BAR_PX: f64 = 720.0;

/// Low enough that the sparkline reads as backdrop, not a fourth element
/// competing with the three metric labels.
const SPARKLINE_FILL_ALPHA: f32 = 0.30;

/// Height of every control in the bar — the nav cluster, the workspace
/// pill, the centre widget, the tray pill and the media indicator — so they
/// share one top and bottom edge: the toolkit's bar control height less two
/// `s1` steps, a caption line with `s1` of air.
const BAR_CONTROL_LOGICAL: f32 =
    ui::tokens::Metrics::BASE.bar_control_height - 2.0 * ui::tokens::Spacing::BASE.s1;
const BAR_CONTROL_PX: f64 = BAR_CONTROL_LOGICAL as f64;
/// Centre-to-centre distance of the bar's avatar chips: 20 px chips whose
/// rings cut only each other's edge.
const AVATAR_STEP: f64 = 20.0;

/// Inset of a lane-filling centre widget (the system monitor, the
/// calendar, a live activity) from its pill's ends, the same each side so
/// the content is centred in the band.
const CENTER_INSET_PX: f64 = 12.0;

/// Diameter of the small Lucide glyph beside the workspace name (the
/// settings gear) and inside the up/down switches (the chevrons).
const SMALL_GLYPH_PX: f64 = 12.0;

/// Diameter of the bar's larger glyphs: the overview grid and the status
/// icons.
const BAR_GLYPH_PX: f64 = 14.0;

/// Scroll-wheel over the center band cycles between these. Nothing marks
/// the position: the time is plain text, and the right-click editor lists
/// the widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CenterWidget {
    #[default]
    Clock,
    SystemMonitor,
    Calendar,
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "layout tests assert exact token-derived pixel values"
)]
mod clock_overlay_geometry_tests {
    use super::*;

    #[test]
    fn overlay_clamps_to_narrow_viewport_and_below_bar() {
        let p = clamp_clock_overlay(
            Point::new(900.0, -20.0),
            (252.0, CLOCK_OVERLAY_HEIGHT),
            (360.0, 640.0),
            40.0,
        );
        assert_eq!(p, Point::new(100.0, 48.0));
    }

    #[test]
    fn overlay_preserves_an_in_bounds_drag_position() {
        let p = clamp_clock_overlay(
            Point::new(120.0, 180.0),
            (252.0, CLOCK_OVERLAY_HEIGHT),
            (900.0, 640.0),
            40.0,
        );
        assert_eq!(p, Point::new(120.0, 180.0));
    }

    #[test]
    fn overlay_cannot_be_dragged_below_the_visible_desktop() {
        let p = clamp_clock_overlay(
            Point::new(120.0, 900.0),
            (252.0, CLOCK_OVERLAY_HEIGHT),
            (900.0, 640.0),
            40.0,
        );
        assert_eq!(p.y, 482.0);
    }

    #[test]
    fn overlay_and_controls_fit_a_phone_width() {
        let width = clock_overlay_width(220.0);
        assert_eq!(width, 204.0);
        let controls = clock_overlay_controls(Rect::new(
            8.0,
            48.0,
            8.0 + width,
            48.0 + CLOCK_OVERLAY_HEIGHT,
        ));
        for control in controls {
            assert!(control.x0 >= 8.0);
            assert!(control.x1 <= 212.0);
            assert!(control.width() >= 44.0);
            assert!(control.height() >= 44.0);
        }
    }

    #[test]
    fn touch_control_commits_only_on_matching_release() {
        let pending = Some((17, 1));
        assert_eq!(
            released_clock_touch_control(pending, 17, true, Some(1)),
            Some(1)
        );
        assert_eq!(released_clock_touch_control(pending, 17, true, None), None);
        assert_eq!(
            released_clock_touch_control(pending, 17, true, Some(0)),
            None
        );
        assert_eq!(
            released_clock_touch_control(pending, 17, false, Some(1)),
            None
        );
        assert_eq!(
            released_clock_touch_control(pending, 99, true, Some(1)),
            None
        );
    }
}

/// Routes through [`TopBarChrome::metric_label_color`] so the per-metric
/// colour (CPU emerald, RAM blue, TMP orange) has one source of truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricKind {
    Cpu,
    Ram,
    Tmp,
}

impl CenterWidget {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            CenterWidget::Clock => CenterWidget::SystemMonitor,
            CenterWidget::SystemMonitor => CenterWidget::Calendar,
            CenterWidget::Calendar => CenterWidget::Clock,
        }
    }
    #[must_use]
    pub fn prev(self) -> Self {
        match self {
            CenterWidget::Clock => CenterWidget::Calendar,
            CenterWidget::SystemMonitor => CenterWidget::Clock,
            CenterWidget::Calendar => CenterWidget::SystemMonitor,
        }
    }
    pub fn index(self) -> usize {
        match self {
            CenterWidget::Clock => 0,
            CenterWidget::SystemMonitor => 1,
            CenterWidget::Calendar => 2,
        }
    }
    pub fn from_index(i: usize) -> Self {
        match i % 3 {
            0 => CenterWidget::Clock,
            1 => CenterWidget::SystemMonitor,
            _ => CenterWidget::Calendar,
        }
    }
}

impl From<TopBarWidget> for CenterWidget {
    fn from(value: TopBarWidget) -> Self {
        match value {
            TopBarWidget::Clock => Self::Clock,
            TopBarWidget::SystemMonitor => Self::SystemMonitor,
            TopBarWidget::Calendar => Self::Calendar,
        }
    }
}

impl From<CenterWidget> for TopBarWidget {
    fn from(value: CenterWidget) -> Self {
        match value {
            CenterWidget::Clock => Self::Clock,
            CenterWidget::SystemMonitor => Self::SystemMonitor,
            CenterWidget::Calendar => Self::Calendar,
        }
    }
}

/// Compared field-for-field to decide whether [`TopBarChrome::cached`] can
/// be reused. Time fields split by mode since each variant consumes a
/// different grain of the host clock.
#[derive(Default, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag independently invalidates retained top-bar paint and hit-test data; combining them would hide cache dependencies"
)]
struct CacheFingerprint {
    /// Full rect, not just size — the per-frame draw repopulates hit
    /// rects from `self.rect`'s corners, so a hit must imply those are
    /// still valid.
    rect_px: (i64, i64, u64, u64),
    presence: TopBarPresence,
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    collaborator_initials: Vec<char>,
    collaborator_names: Vec<String>,
    collaborator_agents: Vec<bool>,
    collaborator_overflow: usize,
    watching: u32,
    sharing: (usize, usize, usize),
    center_widget: CenterWidget,
    widgets: [Option<TopBarWidget>; 3],
    activity_headline: Option<String>,
    activity_unread: usize,
    activity_visible: bool,
    time_bucket: u64,
    /// Quantised, not [`SystemStatus`] itself — it carries an `f32`
    /// volume that would never match frame to frame at full precision.
    status_key: StatusKey,
    media_key: Vec<(u32, u8, String)>,
    media_expanded: bool,
    notification_badge: u32,
    workspace_lens_open: bool,
    focused: bool,
    focused_control: Option<String>,
    toolbar_focus: Vec<bool>,
    clock_activity: Option<(String, u64, bool, bool)>,
    app_tray_revision: u64,
    continuity: Option<String>,
    strips: strips::StripsKey,
    tilo_mark: Option<bool>,
    host_label: Option<String>,
    workspace_menu: bool,
    own: Option<crate::chrome::OwnChip>,
}

/// One application tray icon as last painted: where, and what a click on
/// it addresses.
#[derive(Clone, Debug)]
struct AppTraySlot {
    key: String,
    id: String,
    item_is_menu: bool,
    rect: Rect,
}

/// No calendar source is wired to the shell, so this paints what's
/// actually knowable rather than a plausible fixed string.
fn calendar_strip_parts(now: ClockSnapshot) -> (String, String, String) {
    (now.weekday, now.date, t("topbar-calendar-none"))
}

/// The floating clock's pause, add and close control labels.
fn clock_overlay_labels(activity: &ClockActivity) -> [String; 3] {
    let pause = if activity.paused {
        "topbar-clock-resume"
    } else {
        "topbar-clock-pause"
    };
    let (add, close) = if activity.stopwatch {
        ("topbar-clock-lap", "topbar-clock-close-stopwatch")
    } else {
        ("topbar-clock-add-minute", "topbar-clock-close-timer")
    };
    [t(pause), t(add), t(close)]
}

fn clamp_clock_overlay(
    position: Point,
    overlay: (f64, f64),
    viewport: (f64, f64),
    top: f64,
) -> Point {
    Point::new(
        position
            .x
            .clamp(8.0, (viewport.0 - overlay.0 - 8.0).max(8.0)),
        position
            .y
            .clamp(top + 8.0, (viewport.1 - overlay.1 - 8.0).max(top + 8.0)),
    )
}

fn clock_overlay_width(viewport_width: f64) -> f64 {
    (viewport_width - 16.0).clamp(204.0, 252.0)
}

fn clock_overlay_controls(panel: Rect) -> [Rect; 3] {
    let left = panel.x0 + 12.0;
    let right = panel.x1 - 12.0;
    let gap = 6.0;
    let width = (right - left - gap) / 2.0;
    // The floating surface is commonly used directly on a touch screen.
    // Keep every action at the platform's 44px minimum rather than inheriting
    // the compact 36px desktop-button height.
    let y0 = panel.y1 - 56.0;
    let y1 = panel.y1 - 12.0;
    [
        Rect::new(left, y0, left + width, y1),
        Rect::new(left + width + gap, y0, right, y1),
        Rect::new(
            panel.x1 - 52.0,
            panel.y0 + 8.0,
            panel.x1 - 8.0,
            panel.y0 + 52.0,
        ),
    ]
}

fn clock_overlay_drag_region(panel: Rect) -> Rect {
    Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 66.0)
}

fn released_clock_touch_control(
    pending: Option<(u64, u8)>,
    touch_id: u64,
    ended: bool,
    control_under_finger: Option<u8>,
) -> Option<u8> {
    pending.and_then(|(owner, control)| {
        (ended && owner == touch_id && control_under_finger == Some(control)).then_some(control)
    })
}

/// Rounds a screen-space coordinate into a cache-fingerprint key.
fn fingerprint_i64(v: f64) -> i64 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "screen-space geometry is always far inside i64::MAX; a truncated fingerprint just causes an extra redraw, never unsafety"
    )]
    {
        v.round() as i64
    }
}

/// Rounds a screen-space width/height into a cache-fingerprint key.
fn fingerprint_u64(v: f64) -> u64 {
    let clamped = v.round().max(0.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to non-negative above; screen-space dimensions are always far inside u64 range"
    )]
    {
        clamped as u64
    }
}

/// Buckets a unix timestamp into a cache-fingerprint key.
fn timestamp_bucket(t: i64) -> u64 {
    #[allow(
        clippy::cast_sign_loss,
        reason = "wall-clock timestamps are positive on any real clock; this key only feeds cache-fingerprint equality"
    )]
    {
        t as u64
    }
}

/// Widens a small loop index/count to `f64` for draw-time geometry math.
/// Every call site is a draw-loop counter or a collaborator/slot count
/// bounded well under 2^53, so the integer is exactly representable.
fn small_count_to_f64(n: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded well under 2^53 by every caller (loop index or small on-screen count)"
    )]
    {
        n as f64
    }
}

/// `transform * Affine::translate(xy)`, used at every glyph draw site
/// below. Vello's `Affine::mul` is plain matrix composition with no
/// side effects, but it is a custom `Mul` impl rather than primitive
/// integer arithmetic, which is what trips the lint at each call site.
/// A logical pointer position as the whole-pixel coordinates a tray item's
/// `Activate`/`ContextMenu` call carries.
#[allow(
    clippy::cast_possible_truncation,
    reason = "rounded and clamped into i32's range before the cast"
)]
fn pointer_coordinates(point: Point) -> (i32, i32) {
    let whole = |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    (whole(point.x), whole(point.y))
}

fn at(transform: Affine, xy: (f64, f64)) -> Affine {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine::mul is pure matrix composition (translate here), not primitive integer arithmetic that can silently wrap"
    )]
    {
        transform * Affine::translate(xy)
    }
}

/// Free function so [`TopBarChrome::needs_redraw`] can key on the
/// chrome's status without first copying it onto the widget.
fn status_key_of(s: &crate::chrome::SystemStatus) -> StatusKey {
    StatusKey {
        battery_percent: s.battery_percent(),
        battery_charging: s.battery_is_charging(),
        battery_low: s.battery_is_low(),
        battery_critical: s.battery_is_critical(),
        link: s.link.as_ref().map(|l| {
            let kind = match l.kind {
                crate::chrome::LinkKind::Offline => 0,
                crate::chrome::LinkKind::Wired => 1,
                crate::chrome::LinkKind::Wifi => 2,
            };
            (kind, crate::chrome::link_bars(l))
        }),
        volume_pct: s.volume.map(|v| {
            let pct = (v.level * 100.0).round().clamp(0.0, 100.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to [0.0, 100.0] immediately above, so the cast to u8 cannot truncate or lose sign"
            )]
            {
                pct as u8
            }
        }),
        muted: s.audio_is_silent(),
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent draw-cache invalidation bit, always built via the named `status_key_of` constructor rather than a confusing call-site of positional bools"
)]
struct StatusKey {
    battery_percent: Option<u8>,
    battery_charging: bool,
    battery_low: bool,
    battery_critical: bool,
    link: Option<(u8, u8)>,
    volume_pct: Option<u8>,
    muted: bool,
}

/// Which top bar a display carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarVariant {
    /// The primary display's bar: navigation, centre widget and session.
    Full,
    /// Another display's bar: navigation and the centre widget.
    Compact,
}

/// The Tilo mark's left bar as the brand's `mark.svg` draws it, in its
/// 100 × 100 view box: `M10.98 31.85 Q10.979892448752274 27.96648782251284
/// 14.4 26.12 L20.26 22.97 Q… Z`. The right bar is its mirror image about
/// the box's centre line and the central block a plain rounded rectangle.
fn tilo_mark_side() -> vello::kurbo::BezPath {
    let mut path = vello::kurbo::BezPath::new();
    path.move_to((10.98, 31.85));
    path.quad_to(
        (10.979_892_448_752_274, 27.966_487_822_512_84),
        (14.4, 26.12),
    );
    path.line_to((20.26, 22.97));
    path.quad_to(
        (24.843_373_493_975_903, 20.506_024_096_385_54),
        (24.84, 25.71),
    );
    path.line_to((24.84, 74.29));
    path.quad_to(
        (24.843_373_493_975_903, 79.493_975_903_614_46),
        (20.26, 77.03),
    );
    path.line_to((14.4, 73.88));
    path.quad_to(
        (10.979_892_448_752_274, 72.033_512_177_487_15),
        (10.98, 68.15),
    );
    path.close_path();
    path
}

/// The Tilo mark, the brand's own shapes, centred in `rect` with its view
/// box the largest square that fits. A dark palette takes the brand's dark
/// variant (`mark-dark.svg`): the light one's pale bars glare there.
pub fn paint_tilo_mark(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    static SIDE: std::sync::LazyLock<vello::kurbo::BezPath> =
        std::sync::LazyLock::new(tilo_mark_side);
    let (sides, block) = if theme.tokens.colors.palette.dark {
        (
            Color::from_rgb8(0x3b, 0x43, 0x76),
            Color::from_rgb8(0x6f, 0x82, 0xff),
        )
    } else {
        (
            Color::from_rgb8(0x9f, 0xa6, 0xe0),
            Color::from_rgb8(0x2a, 0x3f, 0xd1),
        )
    };
    let side = rect.width().min(rect.height());
    let center = rect.center();
    let view = transform
        .pre_translate((center.x - side / 2.0, center.y - side / 2.0).into())
        .pre_scale(side / 100.0);
    scene.fill(Fill::NonZero, view, sides, None, &*SIDE);
    let mirrored = view
        .pre_translate((100.0, 0.0).into())
        .pre_scale_non_uniform(-1.0, 1.0);
    scene.fill(Fill::NonZero, mirrored, sides, None, &*SIDE);
    let block_shape = RoundedRect::new(29.0, 16.0, 71.0, 84.0, 6.0);
    scene.fill(Fill::NonZero, view, block, None, &block_shape);
}

/// How the bar's focus last arrived, which decides whether its ring shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusOrigin {
    Keyboard,
    Pointer,
}

/// `DesktopTree`, not `TopBar`, lets `build_tilo_root` stay a pure
/// function of the tree and avoids a circular borrow against the
/// shell's owned logical `TopBar`.
pub struct TopBarChrome {
    responsive: ResponsiveBar,
    explicit_local_user: Option<String>,
    rect: Rect,
    /// Where the last layout stood the bar.
    laid_out: BarStanding,
    /// The compact bar a display other than the primary carries: workspace
    /// navigation and the centre widget only. Session controls, the tray
    /// and collaborators live on the primary display's full bar.
    variant: BarVariant,
    /// Where the bar sits when that is not where layout put it: the
    /// primary display's top edge for the full bar, or its own display's
    /// top edge for a compact one. Height still comes from layout.
    placement: Option<Rect>,
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    /// Most of the bar is stable across frames, so caching cuts the
    /// draw budget at rest by an order of magnitude.
    cached: Scene,
    cache_theme: Option<Theme>,
    /// Reused iff every field matches `self.*` at draw time. See
    /// [`TopBarChrome::current_fingerprint`].
    cache_fp: CacheFingerprint,
    collaborator_initials: Vec<char>,
    collaborator_names: Vec<String>,
    /// Same index as `collaborator_initials`. Resolved from
    /// `cursor_color` when present, else [`ui::presence::user_color`].
    collaborator_colors: Vec<Color>,
    /// Same index as `collaborator_initials`: drawn with the agent mark.
    collaborator_agents: Vec<bool>,
    /// Collaborators beyond the three avatars; nonzero paints a "+N" chip.
    collaborator_overflow: usize,
    /// Copied off the chrome on each revision bump. Empty until
    /// something polls — an empty reading paints no indicator rather
    /// than a plausible default.
    status: crate::chrome::SystemStatus,
    chrome: Option<Chrome>,
    /// Last clock reading, for a bar drawn without a chrome.
    clock: ClockSnapshot,
    /// Copied off the chrome each draw; empty until the host reports.
    telemetry: TelemetrySnapshot,
    synced_revision: u64,
    /// `Some(menu open)` when the bar leads with the Tilo mark
    /// ([`TopBarChrome::with_tilo_mark`]).
    tilo_mark: Option<bool>,
    /// Where the shown workspace is hosted, as a quiet pill before its name;
    /// `None` on the person's own desktop.
    host_label: Option<String>,
    /// What the bar leads with ([`TopBarChrome::with_workspace_menu`]).
    lead: BarLead,
    /// The person's own avatar, as last painted.
    own_avatar_rect: Rect,
    // Click geometry — recomputed every frame so it tracks DPI/resize.
    tilo_mark_rect: Rect,
    overview_btn_rect: Rect,
    up_btn_rect: Rect,
    down_btn_rect: Rect,
    ws_pill_rect: Rect,
    /// The settings gear inside the workspace pill, kept so a test can
    /// prove the pill's trailing padding equals its leading padding.
    ws_gear_rect: Rect,
    tray_pill_rect: Rect,
    /// Application tray icons, right to left, as last painted.
    app_tray: Vec<AppTraySlot>,
    /// Uploaded icon images by item key, reused while the item's pixels
    /// are the same allocation.
    app_tray_images: HashMap<String, (usize, vello::peniko::ImageData)>,
    /// Interaction geometry is deliberately larger than the painted pill:
    /// the screen's top-right edge must remain an easy Quick Settings target.
    tray_hit_rect: Rect,
    media_indicator_rect: Rect,
    media_signature: Vec<(u32, u8, String)>,
    media_started: Option<Instant>,
    media_expanded: bool,
    notification_badge: u32,
    clock_overlay_pos: Option<Point>,
    clock_overlay_rect: Rect,
    clock_pause_rect: Rect,
    clock_add_rect: Rect,
    clock_close_rect: Rect,
    clock_drag_offset: Option<vello::kurbo::Vec2>,
    /// Finger that owns the floating clock gesture. Mouse capture continues
    /// to use `clock_drag_offset`; this prevents a second finger from stealing
    /// an in-progress drag.
    clock_touch_id: Option<u64>,
    /// A touch-started action is only committed when that same finger lifts
    /// over the same control. This mirrors a real button and lets a user slide
    /// off to cancel instead of firing Pause/Close on contact.
    clock_touch_control: Option<(u64, u8)>,
    clock_control_focus: Option<u8>,
    /// Click toggles `overlays.collaborators`.
    avatar_stack_rect: Rect,
    /// Persistent trusted-chrome disclosure of active screen viewers.
    sharing_indicator_rect: Rect,
    /// "Continue <app> from <device>": the offer, and its dismiss chip.
    continuity_rect: Rect,
    continuity_dismiss_rect: Rect,
    /// Scroll over `center_rect` cycles it, dots jump directly.
    center_widget: CenterWidget,
    activity_headline: Option<String>,
    activity_unread: usize,
    activity_pulse: u64,
    activity_started: Option<Instant>,
    /// The centre selector as painted and hit-tested: as wide as its
    /// content needs, never wider than [`Self::center_lane`].
    center_rect: Rect,
    /// The centre lane the bar reserves for the selector (see
    /// [`Self::center_band_width`]). Its neighbours lay out against the
    /// lane, so they hold still while the selector changes widget.
    center_lane: Rect,
    widget_config: TopBarWidgets,
    widget_editing: bool,
    widget_editor_rect: Rect,
    editor_geometry: EditorGeometry,
    editor_submit_key: Option<winit::keyboard::NamedKey>,
    editor_pressed_control: Option<&'static str>,
    editor_pointer_release: Option<winit::event::MouseButton>,
    widget_edit_drag: DragController<TopBarWidget>,
    widget_edit_pressed: Option<usize>,
    /// Matches the sidebar-wheel's scroll contract. `f64` so pixel-delta
    /// scroll events (`f64` in `winit`) accumulate without a truncating
    /// `f64 -> f32` cast.
    center_scroll_accum: f64,
    /// winit delivers position with `CursorMoved` but not `MouseInput`,
    /// so it's cached here for click hit-testing.
    last_cursor: Option<Point>,
    /// The bar is one retained focus stop; this names the synthetic child
    /// currently selected inside it for keyboard and accessibility input.
    focused: bool,
    focused_control: Option<String>,
    /// Whether the bar's focus came from (or was last touched by) the
    /// pointer. Its ring is for keyboard focus only: a click on the bar used
    /// to light the overview button in your colour, which says nothing to a
    /// pointer user.
    focus_origin: FocusOrigin,
    /// When the time the bar shows next goes stale: the end of the current
    /// minute for the clock, second for the monitor, day for the calendar.
    /// Set when the bar draws and advanced by `update` once reached, so an
    /// idle desktop still wakes to repaint the clock.
    time_wake: Option<Instant>,
    /// The active workspace's strips as named places, when the host
    /// reports who is where.
    strips: strips::StripSwitcher,
}

/// Seconds the centre band's time reading stays the same for.
fn time_bucket_secs(activity_visible: bool, center: CenterWidget) -> u64 {
    if activity_visible {
        return 1;
    }
    match center {
        // Clock paints `HH:MM`.
        CenterWidget::Clock => 60,
        // The sparkline derives from `seconds % N`.
        CenterWidget::SystemMonitor => 1,
        // The cache must drop at midnight.
        CenterWidget::Calendar => 86_400,
    }
}

/// How long from `since_epoch` (wall time) until the next whole multiple of
/// `bucket_secs` seconds, when a reading of that grain changes.
fn until_next_time_bucket(since_epoch: Duration, bucket_secs: u64) -> Duration {
    let bucket = Duration::from_secs(bucket_secs.max(1));
    let into = Duration::from_nanos(
        since_epoch
            .as_nanos()
            .checked_rem(bucket.as_nanos())
            .and_then(|into| u64::try_from(into).ok())
            .unwrap_or_default(),
    );
    bucket.saturating_sub(into)
}

/// A selector on the bar. The bar has no surface of its own: its selectors
/// stand on the desktop ground, so they lift off that.
fn bar_selector_body(
    theme: &Theme,
    state: ui::chrome::SelectionState,
) -> ui::chrome::SelectionTier {
    ui::chrome::selector_tier(theme, state, theme.tokens.colors.surfaces.base)
}

/// Paint a selector on the bar at `shape` in `state`, through the one
/// selector painter.
fn paint_bar_selector(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    state: ui::chrome::SelectionState,
    theme: &Theme,
) {
    ui::chrome::paint_selector(
        scene,
        transform,
        shape,
        ui::chrome::SelectorRole::Item(&bar_selector_body(theme, state)),
        theme,
    );
}

/// Measured advances of the system monitor's six runs, at the weight and
/// size each is drawn with, so the column layout can be computed — and
/// tested — without a renderer in hand.
struct MetricWidths {
    cpu_label: f64,
    cpu_value: f64,
    ram_label: f64,
    ram_value: f64,
    tmp_label: f64,
    tmp_value: f64,
}

/// Where the system monitor's columns land in the centre lane. The CPU
/// pair grows rightward from the lane's start; RAM and TMP are anchored to
/// its end, so a wider value moves its label left rather than off the
/// lane. A pair is `None` when the lane has no room for it: they drop from
/// the inside out (RAM first, then TMP) so nothing is ever painted over
/// its neighbour, and what remains still sits on the lane's edges.
struct MonitorColumns {
    cpu_label_x: f64,
    cpu_value_x: f64,
    /// `(label_x, value_x)` of the RAM pair, if it fits.
    ram: Option<(f64, f64)>,
    /// `(label_x, value_x)` of the TMP pair, if it fits.
    tmp: Option<(f64, f64)>,
    /// The sparkline's span: `s3` clear of the CPU value and of whatever
    /// pair (or lane edge) follows. May be empty; the painter skips it.
    spark: (f64, f64),
}

/// Lay the system monitor out in `lane` from measured `widths`, with `s1`
/// between a label and its value and `s3` between pairs.
fn monitor_columns(lane: (f64, f64), widths: &MetricWidths, s1: f64, s3: f64) -> MonitorColumns {
    let (x0, x1) = lane;
    let cpu_label_x = x0;
    let cpu_value_x = cpu_label_x + widths.cpu_label + s1;
    let cpu_end = cpu_value_x + widths.cpu_value;
    let tmp_value_x = x1 - widths.tmp_value;
    let tmp_label_x = tmp_value_x - s1 - widths.tmp_label;
    let ram_value_x = tmp_label_x - s3 - widths.ram_value;
    let ram_label_x = ram_value_x - s1 - widths.ram_label;
    let tmp_fits = tmp_label_x >= cpu_end + s3;
    let ram_fits = tmp_fits && ram_label_x >= cpu_end + s3;
    let next_after_spark = if ram_fits {
        ram_label_x
    } else if tmp_fits {
        tmp_label_x
    } else {
        x1
    };
    MonitorColumns {
        cpu_label_x,
        cpu_value_x,
        ram: ram_fits.then_some((ram_label_x, ram_value_x)),
        tmp: tmp_fits.then_some((tmp_label_x, tmp_value_x)),
        spark: (cpu_end + s3, next_after_spark - s3),
    }
}

/// Parameter object for [`TopBarChrome::draw_metric_label`] — bundled
/// so the call keeps under clippy's argument-count budget.
struct MetricLabelSpec<'a> {
    center_y: f64,
    label_x: f64,
    value_x: f64,
    label: &'a str,
    value_str: &'a str,
    kind: MetricKind,
}

/// Per-frame sizing + palette snapshot for the tray pill, computed once
/// in [`TopBarChrome::draw_tray_pill_body`] and reused by the icon
/// helpers so [`Widget::draw`] stays under the line budget.
struct TrayLayout {
    tray_w: f64,
    tray_slot: f64,
    tray_pad: f64,
    tray_gap: f64,
    icon_px: f64,
    pct_style: TextStyle,
    pct_label: Option<String>,
    /// Width reserved for the unread-count badge, zero without one.
    badge_w: f64,
    slots: usize,
    center_y: f64,
    dim: Color,
    bright: Color,
    warning: Color,
    danger: Color,
    success: Color,
}

impl TrayLayout {
    /// The hit/draw rect for the `n`th 18px tray slot starting at `x`.
    fn slot_rect(&self, x: f64) -> Rect {
        Rect::new(
            x + (self.tray_slot - self.icon_px) / 2.0,
            self.center_y - self.icon_px / 2.0,
            x + f64::midpoint(self.tray_slot, self.icon_px),
            self.center_y + self.icon_px / 2.0,
        )
    }
}

impl TopBarChrome {
    /// The centre selector is visually anchored at the viewport centre, but
    /// its wide-state width must not consume the entire narrow bar. Keep the
    /// established 360 px treatment on laptop-sized and larger viewports and
    /// release the reclaimed lane to the workspace identity below that
    /// point. The lower bound still leaves enough room for the clock and its
    /// three position dots at the large-text setting.
    fn center_band_width(&self, theme: &Theme) -> f64 {
        let text_headroom = (f64::from(theme.text_scale) - 1.0).max(0.0) * 16.0;
        if self.has_workspace_menu() && self.rect.width() < NARROW_MENU_BAR_PX {
            // A phone-wide bar keeps the time and gives the rest of the
            // lane to the workspace name and the people beside it.
            return 64.0 + text_headroom;
        }
        let min_width = (220.0 + text_headroom).min(260.0);
        (self.rect.width() - 580.0).clamp(min_width, 360.0)
    }

    fn clock_viewport(&self) -> (f64, f64) {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.snapshot().viewport_size)
            .unwrap_or_else(|| (self.rect.width(), f64::INFINITY))
    }

    fn publish_clock_activity_command(&self, timer: &str, stopwatch: &str) {
        let activity = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.snapshot().clock_activity);
        let is_stopwatch = activity.as_ref().is_some_and(|activity| activity.stopwatch);
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.intent(ChromeIntent::ClockCommand {
                command: if is_stopwatch { stopwatch } else { timer }.to_string(),
                activity_id: activity.and_then(|activity| activity.activity_id),
            });
        }
    }

    /// Route a point through the exact control rectangles painted for the
    /// floating activity. Shared by mouse and touch so the two modalities can
    /// never drift into different timer/stopwatch behavior.
    fn activate_clock_control_at(&mut self, point: Point) -> bool {
        let Some(control) = self.clock_control_at(point) else {
            return false;
        };
        self.activate_clock_control(control);
        true
    }

    fn clock_control_at(&self, point: Point) -> Option<u8> {
        if self.clock_pause_rect.contains(point) {
            Some(0)
        } else if self.clock_add_rect.contains(point) {
            Some(1)
        } else if self.clock_close_rect.contains(point) {
            Some(2)
        } else {
            None
        }
    }

    fn activate_clock_control(&mut self, control: u8) {
        self.clock_control_focus = Some(control);
        match control {
            0 => self.publish_clock_activity_command("toggle", "toggle-stopwatch"),
            1 => self.publish_clock_activity_command("add-minute", "lap-stopwatch"),
            2 => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.intent(ChromeIntent::HideClock);
                }
                self.publish_clock_activity_command("close", "close-stopwatch");
            }
            _ => return,
        }
        self.cache_fp = CacheFingerprint::default();
    }

    fn begin_clock_drag_at(&mut self, point: Point) -> bool {
        let header = clock_overlay_drag_region(self.clock_overlay_rect);
        if !header.contains(point) {
            return false;
        }
        let origin = self.clock_overlay_rect.origin();
        self.clock_drag_offset = Some(vello::kurbo::Vec2::new(
            point.x - origin.x,
            point.y - origin.y,
        ));
        true
    }

    fn move_clock_drag_to(&mut self, point: Point) -> bool {
        let Some(offset) = self.clock_drag_offset else {
            return false;
        };
        let width = self.clock_overlay_rect.width();
        self.clock_overlay_pos = Some(clamp_clock_overlay(
            Point::new(point.x - offset.x, point.y - offset.y),
            (width, self.clock_overlay_rect.height()),
            self.clock_viewport(),
            self.rect.y1,
        ));
        self.cache_fp = CacheFingerprint::default();
        true
    }
    pub fn from_tree(tree: &DesktopTree) -> Self {
        Self::from_tree_with_identity(tree, None)
    }

    pub fn from_tree_with_local_user(tree: &DesktopTree, local: String) -> Self {
        Self::from_tree_with_identity(tree, Some(local))
    }

    fn from_tree_with_identity(tree: &DesktopTree, explicit_local_user: Option<String>) -> Self {
        let mut this = Self {
            responsive: ResponsiveBar::new(),
            explicit_local_user,
            rect: Rect::ZERO,
            laid_out: BarStanding::InColumn,
            variant: BarVariant::Full,
            placement: None,
            workspace_name: String::new(),
            workspace_index: 0,
            workspace_total: 1,
            cached: Scene::new(),
            cache_theme: None,
            cache_fp: CacheFingerprint::default(),
            collaborator_initials: Vec::new(),
            collaborator_names: Vec::new(),
            collaborator_colors: Vec::new(),
            collaborator_agents: Vec::new(),
            collaborator_overflow: 0,
            status: crate::chrome::SystemStatus::default(),
            chrome: None,
            clock: ClockSnapshot::default(),
            telemetry: TelemetrySnapshot::default(),
            synced_revision: 0,
            tilo_mark: None,
            host_label: None,
            lead: BarLead::Navigation,
            own_avatar_rect: Rect::ZERO,
            tilo_mark_rect: Rect::ZERO,
            overview_btn_rect: Rect::ZERO,
            up_btn_rect: Rect::ZERO,
            down_btn_rect: Rect::ZERO,
            ws_pill_rect: Rect::ZERO,
            ws_gear_rect: Rect::ZERO,
            tray_pill_rect: Rect::ZERO,
            app_tray: Vec::new(),
            app_tray_images: HashMap::new(),
            tray_hit_rect: Rect::ZERO,
            media_indicator_rect: Rect::ZERO,
            media_signature: Vec::new(),
            media_started: None,
            media_expanded: false,
            notification_badge: 0,
            clock_overlay_pos: None,
            clock_overlay_rect: Rect::ZERO,
            clock_pause_rect: Rect::ZERO,
            clock_add_rect: Rect::ZERO,
            clock_close_rect: Rect::ZERO,
            clock_drag_offset: None,
            clock_touch_id: None,
            clock_touch_control: None,
            clock_control_focus: None,
            avatar_stack_rect: Rect::ZERO,
            sharing_indicator_rect: Rect::ZERO,
            continuity_rect: Rect::ZERO,
            continuity_dismiss_rect: Rect::ZERO,
            center_widget: CenterWidget::Clock,
            activity_headline: None,
            activity_unread: 0,
            activity_pulse: 0,
            activity_started: None,
            center_rect: Rect::ZERO,
            center_lane: Rect::ZERO,
            widget_config: TopBarWidgets::default(),
            widget_editing: false,
            widget_editor_rect: Rect::ZERO,
            editor_geometry: EditorGeometry::default(),
            editor_submit_key: None,
            editor_pressed_control: None,
            editor_pointer_release: None,
            widget_edit_drag: DragController::vertical(),
            widget_edit_pressed: None,
            center_scroll_accum: 0.0,
            last_cursor: None,
            focused: false,
            focused_control: None,
            focus_origin: FocusOrigin::Keyboard,
            time_wake: None,
            strips: strips::StripSwitcher::default(),
        };
        this.sync_from_tree(tree);
        this
    }

    fn current_fingerprint(&self) -> CacheFingerprint {
        // Rounded to integer keys so the fingerprint stays stable
        // against benign float noise from fractional-DPI taffy layout.
        let x0 = fingerprint_i64(self.rect.x0);
        let y0 = fingerprint_i64(self.rect.y0);
        let w = fingerprint_u64(self.rect.width());
        let h = fingerprint_u64(self.rect.height());
        let activity_visible = self.activity_is_visible();
        let now = self.live_clock().unix_seconds;
        let time_bucket = timestamp_bucket(now)
            .checked_div(time_bucket_secs(activity_visible, self.center_widget))
            .unwrap_or_default();
        CacheFingerprint {
            rect_px: (x0, y0, w, h),
            presence: self.presence(),
            workspace_name: self.workspace_name.clone(),
            workspace_index: self.workspace_index,
            workspace_total: self.workspace_total,
            collaborator_initials: self.collaborator_initials.clone(),
            collaborator_names: self.collaborator_names.clone(),
            collaborator_agents: self.collaborator_agents.clone(),
            collaborator_overflow: self.collaborator_overflow,
            watching: self.watching(),
            sharing: self.sharing_summary(),
            center_widget: self.center_widget,
            widgets: std::array::from_fn(|index| self.widget_config.widgets.get(index).copied()),
            activity_headline: self.activity_headline.clone(),
            activity_unread: self.activity_unread,
            activity_visible,
            time_bucket,
            status_key: self.status_key(),
            media_key: self.media_signature.clone(),
            media_expanded: self.media_expanded,
            notification_badge: self.notification_badge,
            workspace_lens_open: self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.snapshot().overlays.ws_settings),
            focused: self.focused,
            focused_control: self.focused_control.clone(),
            toolbar_focus: self.responsive.paint_focus_state(),
            clock_activity: self.chrome.as_ref().and_then(|chrome| {
                chrome
                    .snapshot()
                    .clock_activity
                    .map(|a| (a.label.clone(), a.remaining_secs, a.overlay, a.paused))
            }),
            app_tray_revision: self.app_tray_revision(),
            continuity: self.continuity_label(),
            strips: self.strips_key(),
            tilo_mark: self.tilo_mark,
            host_label: self.host_label.clone(),
            workspace_menu: self.has_workspace_menu(),
            own: self.own_chip(),
        }
    }

    fn app_tray_revision(&self) -> u64 {
        self.chrome
            .as_ref()
            .map_or(0, |chrome| chrome.app_tray_revision())
    }

    /// The live continuity offer's label. Read from chrome rather than
    /// cached, so the semantic surface and the next paint agree even before
    /// a frame has drawn the chip.
    fn continuity_label(&self) -> Option<String> {
        self.chrome.as_ref()?.continuity_offer()
    }

    /// The status reading quantised to what the tray paints.
    fn status_key(&self) -> StatusKey {
        status_key_of(&self.status)
    }

    /// The status the chrome currently holds, or the last one copied when
    /// no chrome is attached. Read at draw time rather than pushed by
    /// `update`: `Column`/`Stack`/`Container` inherit the default
    /// [`Widget::update`], which doesn't recurse, so a leaf's `update`
    /// only runs if some ancestor forwards — none of the bar's do.
    /// Hand the person's edited centre widgets to the host to keep.
    fn save_widget_config(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.intent(ChromeIntent::SetTopBarWidgets(
                self.widget_config.clone().normalised(),
            ));
        }
    }

    fn live_status(&self) -> crate::chrome::SystemStatus {
        match self.chrome.as_ref() {
            Some(c) => c.snapshot().status,
            None => self.status.clone(),
        }
    }

    /// Wake again when the time reading just drawn goes stale.
    fn schedule_time_wake(&mut self) {
        let since_epoch = Duration::from_nanos(ui::time::unix_now_ns());
        let bucket = time_bucket_secs(self.activity_is_visible(), self.center_widget);
        self.time_wake = Instant::now().checked_add(until_next_time_bucket(since_epoch, bucket));
    }

    /// Whether the time reading went stale since the bar last drew. Moves
    /// the wake on, so a bar that is not drawn (hidden under fullscreen, a
    /// locked session) never holds an overdue deadline.
    fn take_time_wake(&mut self) -> bool {
        if self.time_wake.is_none_or(|at| Instant::now() < at) {
            return false;
        }
        self.schedule_time_wake();
        self.rect.height() > 0.0 && self.rect.width() > 0.0
    }

    fn live_clock(&self) -> ClockSnapshot {
        self.chrome
            .as_ref()
            .map_or_else(|| self.clock.clone(), |c| c.snapshot().clock)
    }

    /// The full bar's session side: the tray, application tray, media and
    /// sharing indicators, collaborators and the continuity offer. A compact
    /// bar on another display carries none of these.
    fn draw_session_clusters(
        &mut self,
        scene: &mut Scene,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) {
        let transform = Affine::IDENTITY;
        let layout = self.draw_tray_pill_body(scene, transform, theme, text_renderer, center_y);
        self.draw_tray_icons(scene, transform, theme, text_renderer, &layout);
        // s3/s4 match the pad_x/gap used when this pill was sized and
        // placed in `draw_tray_pill_body` (s4 = 16, ≥ 8 px after avatar
        // radius).
        let stack_right_x = self.rect.x1
            - f64::from(theme.tokens.spacing.s3)
            - layout.tray_w
            - f64::from(theme.tokens.spacing.s4);
        let stack_right_x = self.draw_own_avatar(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            stack_right_x,
        );
        let stack_right_x = self.draw_app_tray(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            stack_right_x,
        );
        // Snapshotted before drawing, so `avatar_stack_rect` can be
        // populated once the drawn circle count (incl. overflow) is known.
        let stack_right_x = self.draw_media_indicator(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            stack_right_x,
        );

        let stack_right_x = self.draw_sharing_indicator(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            stack_right_x,
        );
        let watching = self.draw_watching_chip(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            stack_right_x,
        );
        let stack_right_x = if watching.is_zero_area() {
            stack_right_x
        } else {
            watching.x0 - f64::from(theme.tokens.spacing.s2)
        };
        let slot_count = self.draw_avatar_chips(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            stack_right_x,
        );
        if watching.is_zero_area() || slot_count > 0 {
            self.draw_avatar_stack_bounds(
                scene,
                transform,
                theme,
                (center_y, stack_right_x),
                slot_count,
            );
        }
        // Spectators and avatars are one People control: the chip alone
        // still opens it, and the Invite stand-in never paints beside it.
        if !watching.is_zero_area() {
            self.avatar_stack_rect = if slot_count > 0 {
                self.avatar_stack_rect.union(watching)
            } else {
                watching
            };
        }
        let continuity = self.continuity_label();
        self.draw_continuity_chip(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            continuity.as_deref(),
        );
    }

    /// The compact variant a display other than the primary carries.
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.variant = BarVariant::Compact;
        self
    }

    pub fn is_compact(&self) -> bool {
        self.variant == BarVariant::Compact
    }

    /// Sit the bar at `rect`'s top edge, `rect`'s width across, instead of
    /// where layout places it; `None` returns it to layout.
    pub fn set_placement(&mut self, rect: Option<Rect>) {
        if self.placement != rect {
            self.placement = rect;
            if let Some(rect) = rect {
                let height = if self.rect.height() > 0.0 {
                    self.rect.height()
                } else {
                    f64::from(TOPBAR_HEIGHT_PX)
                };
                self.rect = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + height);
            }
            self.cache_fp = CacheFingerprint::default();
        }
    }

    /// Where the bar is drawn.
    pub fn bar_rect(&self) -> Rect {
        self.rect
    }

    /// Lead the bar with the Tilo mark, which asks for
    /// [`ChromeIntent::ToggleTiloMenu`]: a host with no other place for
    /// what Tilo is (the browser) offers it there. Off by default.
    #[must_use]
    pub fn with_tilo_mark(mut self) -> Self {
        self.tilo_mark = Some(false);
        self
    }

    /// The menu the mark opens is shown or gone; the mark reads as pressed
    /// while it is.
    pub fn set_tilo_menu_open(&mut self, open: bool) {
        if let Some(shown) = self.tilo_mark.as_mut() {
            *shown = open;
        }
    }

    /// Where the Tilo mark was last drawn, for anchoring its menu; zero
    /// without the mark.
    pub fn tilo_mark_rect(&self) -> Rect {
        if self.tilo_mark.is_some() {
            self.control_rect(Self::CONTROL_TILO_MARK)
        } else {
            Rect::ZERO
        }
    }

    /// Name the host of the workspace shown (`example.org`) in a quiet
    /// pill before its name; `None` (the person's own desktop) shows none.
    pub fn set_host_label(&mut self, host: Option<String>) {
        self.host_label = host.filter(|host| !host.trim().is_empty());
    }

    /// Lead with the workspace's name as one menu, which asks for
    /// [`ChromeIntent::ToggleWorkspaceMenu`] (the workspaces, where it is
    /// hosted, joining and leaving, renaming, the overview, a new desk),
    /// show desks only when there is more than one, and end with the
    /// person's own avatar, which asks for [`ChromeIntent::ToggleProfile`].
    /// The host shows both.
    #[must_use]
    pub fn with_workspace_menu(mut self) -> Self {
        self.lead = BarLead::WorkspaceMenu;
        self
    }

    /// Whether the bar leads with the workspace menu.
    fn has_workspace_menu(&self) -> bool {
        self.lead == BarLead::WorkspaceMenu
    }

    /// Where the workspace name was last drawn, for anchoring its menu.
    pub fn workspace_menu_rect(&self) -> Rect {
        self.control_rect(Self::CONTROL_WORKSPACE_SETTINGS)
    }

    /// Where the person's own avatar was last drawn, for anchoring their
    /// profile card; zero when it is not shown.
    pub fn own_avatar_rect(&self) -> Rect {
        self.control_rect(Self::CONTROL_PROFILE)
    }

    /// What the own avatar is called to assistive technology.
    fn profile_control_name(&self) -> String {
        let name = self.own_chip().map(|own| own.name).unwrap_or_default();
        t_args("topbar-profile", &[("name", name.into())])
    }

    /// The person's own avatar, when the bar shows it.
    fn own_chip(&self) -> Option<crate::chrome::OwnChip> {
        if !self.has_workspace_menu() || self.is_compact() {
            return None;
        }
        self.chrome.as_ref()?.snapshot().own
    }

    #[must_use]
    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        let rev = chrome.snapshot().revision;
        // Declares this widget synced at the current revision, so
        // `update`'s sync branch won't re-run until it moves — status
        // must be seeded now.
        self.status = chrome.snapshot().status;
        self.chrome = Some(chrome);
        self.synced_revision = rev;
        self
    }

    pub fn sync_from_tree(&mut self, tree: &DesktopTree) {
        self.workspace_total = tree.workspaces.len().max(1);
        let active_ws = tree.workspaces.iter().find(|w| w.id == tree.active);
        let (idx, name) = tree
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, w)| w.id == tree.active)
            .map_or_else(
                || (0, t("topbar-workspace-fallback")),
                |(i, w)| (i, w.name.clone()),
            );
        self.workspace_index = idx;
        self.workspace_name = name;
        // Presence is workspace-scoped. The legacy tree-level cache can
        // contain people active elsewhere and must not leak them into this bar.
        let roster: &[tilo_desktop_model::Collaborator] =
            active_ws.map_or(&[], |workspace| workspace.collaborators.as_slice());
        // An invited-but-not-arrived peer doesn't count as present.
        let present = |c: &&tilo_desktop_model::Collaborator| c.id != "me" && !c.invited;
        self.collaborator_initials.clear();
        self.collaborator_names.clear();
        self.collaborator_colors.clear();
        self.collaborator_agents.clear();
        let now = tilo_clock::unix_now();
        // Shared helper so the overflow chip and the wheel-disc's
        // active-card dot count agree.
        let peer_count = roster.iter().filter(present).count();
        // Settled over the whole room, so a peer's avatar matches the
        // colour their cursor is drawn in elsewhere on the desktop.
        let colors = self.explicit_local_user.as_deref().map_or_else(
            || roster_colors(roster, ""),
            |local| roster_colors(roster, local),
        );
        for c in roster.iter().filter(present).take(3) {
            let Some(ch) = c.name.chars().next() else {
                continue;
            };
            self.collaborator_initials.push(ch.to_ascii_uppercase());
            self.collaborator_names.push(c.name.clone());
            let (r, g, b) = colors
                .get(&c.id)
                .copied()
                .unwrap_or_else(|| user_color(&c.id));
            self.collaborator_colors.push(Color::from_rgb8(r, g, b));
            self.collaborator_agents
                .push(crate::collaborators::collaborator_is_agent(
                    active_ws, c, now,
                ));
        }
        self.collaborator_overflow = peer_count.saturating_sub(3);
        self.sync_strips(tree);
    }

    /// The host's clock reading, already formatted — and pinned by the host
    /// when a screenshot harness needs every frame to hold still.
    fn current_clock_string(&self) -> String {
        self.live_clock().clock
    }

    pub fn collaborator_initials(&self) -> &[char] {
        &self.collaborator_initials
    }

    /// The up-chevron paints live exactly when this holds — the same
    /// condition under which `ChromeState::switch_workspace(-1)`
    /// actually moves.
    pub fn can_switch_up(&self) -> bool {
        self.workspace_index > 0
    }

    pub fn can_switch_down(&self) -> bool {
        self.workspace_index < self.workspace_total.saturating_sub(1)
    }

    pub fn collaborator_colors(&self) -> &[Color] {
        &self.collaborator_colors
    }

    pub fn collaborator_overflow(&self) -> usize {
        self.collaborator_overflow
    }

    pub fn center_widget(&self) -> CenterWidget {
        self.center_widget
    }

    /// Bypasses the scroll/click event path.
    #[cfg(any(test, debug_assertions))]
    pub fn set_center_widget_for_tests(&mut self, w: CenterWidget) {
        self.center_widget = w;
    }

    /// Pinned so a future revert to the old four-rect quad fails loudly
    /// instead of silently regressing.
    pub fn overview_glyph_icon() -> ui::icons::Icon {
        ui::icons::Icon::LayoutGrid
    }

    /// Synthetic control ids are deliberately stable: unlike a child index,
    /// they survive the optional avatar/media controls appearing and
    /// disappearing between frames.
    const CONTROL_TILO_MARK: &'static str = "topbar:tilo-mark";
    const CONTROL_OVERVIEW: &'static str = "topbar:overview";
    const CONTROL_WORKSPACE_UP: &'static str = "topbar:workspace-up";
    const CONTROL_WORKSPACE_DOWN: &'static str = "topbar:workspace-down";
    const WORKSPACE_SETTINGS_ICON: ui::icons::Icon = ui::icons::Icon::Pencil;
    const WORKSPACE_MENU_ICON: ui::icons::Icon = ui::icons::Icon::ChevronDown;

    /// The glyph closing the workspace pill: what pressing it opens.
    fn workspace_pill_icon(&self) -> ui::icons::Icon {
        if self.has_workspace_menu() {
            Self::WORKSPACE_MENU_ICON
        } else {
            Self::WORKSPACE_SETTINGS_ICON
        }
    }
    const CONTROL_PROFILE: &'static str = "topbar:profile";
    const CONTROL_WORKSPACE_SETTINGS: &'static str = "topbar:workspace-settings";
    const CONTROL_CENTER_CLOCK: &'static str = "topbar:center:clock";
    const CONTROL_CENTER_MONITOR: &'static str = "topbar:center:system-monitor";
    const CONTROL_CENTER_CALENDAR: &'static str = "topbar:center:calendar";
    const CONTROL_QUICK_SETTINGS: &'static str = "topbar:quick-settings";
    const CONTROL_COLLABORATORS: &'static str = "topbar:collaborators";
    const CONTROL_MEDIA: &'static str = "topbar:media";
    const CONTROL_CONTINUITY: &'static str = "topbar:continuity";
    const CONTROL_CONTINUITY_DISMISS: &'static str = "topbar:continuity-dismiss";
    const CONTROL_EDIT_WIDGETS: &'static str = "topbar:editor:open";
    const CONTROL_REMOVE_CLOCK: &'static str = "topbar:editor:remove-clock";
    const CONTROL_REMOVE_MONITOR: &'static str = "topbar:editor:remove-system-monitor";
    const CONTROL_REMOVE_CALENDAR: &'static str = "topbar:editor:remove-calendar";
    const CONTROL_ADD_CLOCK: &'static str = "topbar:editor:add-clock";
    const CONTROL_ADD_MONITOR: &'static str = "topbar:editor:add-system-monitor";
    const CONTROL_ADD_CALENDAR: &'static str = "topbar:editor:add-calendar";

    fn center_control(widget: TopBarWidget) -> &'static str {
        match widget {
            TopBarWidget::Clock => Self::CONTROL_CENTER_CLOCK,
            TopBarWidget::SystemMonitor => Self::CONTROL_CENTER_MONITOR,
            TopBarWidget::Calendar => Self::CONTROL_CENTER_CALENDAR,
        }
    }

    fn editor_control(widget: TopBarWidget, present: bool) -> &'static str {
        match (widget, present) {
            (TopBarWidget::Clock, true) => Self::CONTROL_REMOVE_CLOCK,
            (TopBarWidget::SystemMonitor, true) => Self::CONTROL_REMOVE_MONITOR,
            (TopBarWidget::Calendar, true) => Self::CONTROL_REMOVE_CALENDAR,
            (TopBarWidget::Clock, false) => Self::CONTROL_ADD_CLOCK,
            (TopBarWidget::SystemMonitor, false) => Self::CONTROL_ADD_MONITOR,
            (TopBarWidget::Calendar, false) => Self::CONTROL_ADD_CALENDAR,
        }
    }

    fn editor_widget(target: &str) -> Option<(TopBarWidget, bool)> {
        match target {
            Self::CONTROL_REMOVE_CLOCK => Some((TopBarWidget::Clock, true)),
            Self::CONTROL_REMOVE_MONITOR => Some((TopBarWidget::SystemMonitor, true)),
            Self::CONTROL_REMOVE_CALENDAR => Some((TopBarWidget::Calendar, true)),
            Self::CONTROL_ADD_CLOCK => Some((TopBarWidget::Clock, false)),
            Self::CONTROL_ADD_MONITOR => Some((TopBarWidget::SystemMonitor, false)),
            Self::CONTROL_ADD_CALENDAR => Some((TopBarWidget::Calendar, false)),
            _ => None,
        }
    }

    fn cycle_center_widget(&mut self, forward: bool) {
        let widgets = &self.widget_config.widgets;
        if widgets.is_empty() {
            return;
        }
        let Some(current) = widgets
            .iter()
            .position(|widget| *widget == TopBarWidget::from(self.center_widget))
        else {
            if let Some(first) = widgets.first().copied() {
                self.center_widget = first.into();
            }
            return;
        };
        let next = if forward {
            widgets
                .get(current.saturating_add(1))
                .or_else(|| widgets.first())
        } else {
            current
                .checked_sub(1)
                .and_then(|index| widgets.get(index))
                .or_else(|| widgets.last())
        };
        if let Some(next) = next.copied() {
            self.center_widget = next.into();
        }
    }

    fn collaborators_visible(&self) -> bool {
        !self.collaborator_initials.is_empty()
            || self.collaborator_overflow > 0
            || self.watching() > 0
            || self.sharing_summary().0 > 0
    }

    /// People following the room without taking part, from the host.
    fn watching(&self) -> u32 {
        self.chrome
            .as_ref()
            .map_or(0, |chrome| chrome.snapshot().watching)
    }

    /// The empty-room Invite pill stands in for the avatar stack.
    fn invite_visible(&self) -> bool {
        !self.collaborators_visible() && !self.avatar_stack_rect.is_zero_area()
    }

    /// `(viewers, controllers, surfaces)` from the compositor authority.
    fn sharing_summary(&self) -> (usize, usize, usize) {
        let Some(chrome) = self.chrome.as_ref() else {
            return (0, 0, 0);
        };
        let snapshot = chrome.snapshot();
        let mut peers = snapshot
            .active_exposures
            .iter()
            .map(|row| row.peer)
            .collect::<Vec<_>>();
        peers.sort_unstable();
        peers.dedup();
        let mut controllers = snapshot
            .active_exposures
            .iter()
            .filter(|row| row.can_control)
            .map(|row| row.peer)
            .collect::<Vec<_>>();
        controllers.sort_unstable();
        controllers.dedup();
        let mut surfaces = snapshot
            .active_exposures
            .iter()
            .map(|row| row.surface)
            .collect::<Vec<_>>();
        surfaces.sort_unstable();
        surfaces.dedup();
        (peers.len(), controllers.len(), surfaces.len())
    }

    /// Named viewers for disclosure prose. Transport ids are joined to the
    /// active room's live roster through `Collaborator::session`; an unknown
    /// session remains visible under its deterministic peer handle.
    fn sharing_viewers(&self) -> Vec<(u64, String, Option<bool>, bool)> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let snapshot = chrome.snapshot();
        let mut authority = Vec::<(u64, bool)>::new();
        for row in &snapshot.active_exposures {
            if let Some((_, can_control)) = authority.iter_mut().find(|(peer, _)| *peer == row.peer)
            {
                *can_control |= row.can_control;
            } else {
                authority.push((row.peer, row.can_control));
            }
        }
        authority.sort_unstable_by_key(|(peer, _)| *peer);
        let tree = chrome.tree();
        let mut viewers = Vec::new();
        for (peer, can_control) in authority {
            if let Some((workspace, participant)) = tree.workspaces.iter().find_map(|workspace| {
                workspace
                    .collaborators
                    .iter()
                    .find(|person| person.session == Some(peer))
                    .map(|participant| (workspace, participant))
            }) {
                let is_agent = workspace.policy.grants.iter().any(|grant| {
                    matches!(
                        &grant.subject,
                        tilo_action_policy::Subject::Agent(id)
                            if id.eq_ignore_ascii_case(&participant.id)
                                || id.eq_ignore_ascii_case(&participant.name)
                    )
                });
                viewers.push((peer, participant.name.clone(), Some(is_agent), can_control));
            } else {
                viewers.push((peer, format!("p{peer:016x}"), None, can_control));
            }
        }
        viewers
    }

    fn sharing_targets_for_peer(&self, peer: u64) -> (Vec<String>, Vec<String>) {
        let Some(chrome) = self.chrome.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let snapshot = chrome.snapshot();
        let mut authority = Vec::<(u64, bool)>::new();
        for row in snapshot
            .active_exposures
            .iter()
            .filter(|row| row.peer == peer)
        {
            if let Some((_, can_control)) = authority
                .iter_mut()
                .find(|(surface, _)| *surface == row.surface)
            {
                *can_control |= row.can_control;
            } else {
                authority.push((row.surface, row.can_control));
            }
        }
        authority.sort_unstable_by_key(|(surface, _)| *surface);
        let tree = chrome.tree();
        let controlled = authority
            .iter()
            .filter(|(_, can_control)| *can_control)
            .map(|(surface, _)| *surface)
            .collect();
        let view_only = authority
            .iter()
            .filter(|(_, can_control)| !*can_control)
            .map(|(surface, _)| *surface)
            .collect();
        (
            Self::sharing_target_labels(&tree, view_only),
            Self::sharing_target_labels(&tree, controlled),
        )
    }

    /// Glance text puts the privacy state before identity so elision can
    /// shorten a long display name without ever hiding "controls"/"viewing"
    /// or whether the participant is an agent.
    fn sharing_label(&self) -> Option<String> {
        let (viewers, controllers, _) = self.sharing_summary();
        if viewers == 0 {
            return None;
        }
        let named = self.sharing_viewers();
        Some(match named.as_slice() {
            [(_, name, is_agent, controls)] => {
                let key = match (is_agent, controls) {
                    (Some(true), true) => "topbar-sharing-agent-controls",
                    (Some(true), false) => "topbar-sharing-agent-viewing",
                    (Some(false), true) => "topbar-sharing-person-controls",
                    (Some(false), false) => "topbar-sharing-person-viewing",
                    (None, true) => "topbar-sharing-session-controls",
                    (None, false) => "topbar-sharing-session-viewing",
                };
                t_args(key, &[("name", name.as_str().into())])
            }
            _ if controllers > 0 => t_args(
                "topbar-sharing-count-controlling",
                &[
                    ("controllers", controllers.into()),
                    ("viewers", viewers.into()),
                ],
            ),
            _ => t_args("topbar-sharing-count", &[("viewers", viewers.into())]),
        })
    }

    fn sharing_target_labels(tree: &DesktopTree, surfaces: Vec<u64>) -> Vec<String> {
        surfaces
            .into_iter()
            .map(|surface| {
                tree.workspaces
                    .iter()
                    .find_map(|workspace| {
                        workspace
                            .strips
                            .iter()
                            .flat_map(|strip| &strip.window_groups)
                            .flat_map(|group| &group.tabs)
                            .find(|tab| {
                                matches!(
                                    &tab.tile,
                                    tilo_desktop_model::TileContent::Foreign { surface_id, .. }
                                        if *surface_id == surface
                                )
                            })
                            .map(|tab| (workspace, tab))
                    })
                    // A window the tree does not list has no name a person
                    // knows; its surface number would mean nothing to them.
                    .map_or_else(
                        || t("topbar-sharing-unknown-window"),
                        |(workspace, tab)| {
                            if workspace.id == tree.active {
                                tab.title.clone()
                            } else {
                                format!("{} › {}", workspace.name, tab.title)
                            }
                        },
                    )
            })
            .collect()
    }

    /// The same ordered list drives keyboard cycling, synthetic semantics,
    /// and focus-ring geometry. Keeping it in one place prevents an optional
    /// avatar from shifting every subsequent accessibility target.
    fn editor_test_rects(&self) -> [(String, Rect); 6] {
        [
            ("topbar_editor_remove_clock", Self::CONTROL_REMOVE_CLOCK),
            (
                "topbar_editor_remove_system_monitor",
                Self::CONTROL_REMOVE_MONITOR,
            ),
            (
                "topbar_editor_remove_calendar",
                Self::CONTROL_REMOVE_CALENDAR,
            ),
            ("topbar_editor_add_clock", Self::CONTROL_ADD_CLOCK),
            (
                "topbar_editor_add_system_monitor",
                Self::CONTROL_ADD_MONITOR,
            ),
            ("topbar_editor_add_calendar", Self::CONTROL_ADD_CALENDAR),
        ]
        .map(|(name, target)| (name.to_owned(), self.control_rect(target)))
    }

    fn control_targets(&self) -> Vec<&'static str> {
        let mut controls = Vec::new();
        if self.tilo_mark.is_some() {
            controls.push(Self::CONTROL_TILO_MARK);
        }
        if !self.has_workspace_menu() {
            controls.extend([
                Self::CONTROL_OVERVIEW,
                Self::CONTROL_WORKSPACE_UP,
                Self::CONTROL_WORKSPACE_DOWN,
            ]);
        }
        controls.push(Self::CONTROL_WORKSPACE_SETTINGS);
        controls.extend(
            self.widget_config
                .widgets
                .iter()
                .copied()
                .map(Self::center_control),
        );
        if self.is_compact() {
            return controls;
        }
        controls.push(Self::CONTROL_EDIT_WIDGETS);
        controls.push(Self::CONTROL_QUICK_SETTINGS);
        if self.widget_editing {
            controls.extend(self.editor_controls());
        }
        if self.collaborators_visible() || self.invite_visible() {
            controls.push(Self::CONTROL_COLLABORATORS);
        }
        if self.own_chip().is_some() {
            controls.push(Self::CONTROL_PROFILE);
        }
        if !self.media_indicator_rect.is_zero_area() {
            controls.push(Self::CONTROL_MEDIA);
        }
        if self.continuity_label().is_some() {
            controls.push(Self::CONTROL_CONTINUITY);
            controls.push(Self::CONTROL_CONTINUITY_DISMISS);
        }
        controls
    }

    fn control_rect(&self, target: &str) -> Rect {
        if self.responsive.active
            && !matches!(
                target,
                Self::CONTROL_REMOVE_CLOCK
                    | Self::CONTROL_REMOVE_MONITOR
                    | Self::CONTROL_REMOVE_CALENDAR
                    | Self::CONTROL_ADD_CLOCK
                    | Self::CONTROL_ADD_MONITOR
                    | Self::CONTROL_ADD_CALENDAR
            )
        {
            return self.responsive.rect_for(target);
        }
        match target {
            Self::CONTROL_TILO_MARK => self.tilo_mark_rect,
            Self::CONTROL_OVERVIEW => self.overview_btn_rect,
            Self::CONTROL_WORKSPACE_UP => self.up_btn_rect,
            Self::CONTROL_WORKSPACE_DOWN => self.down_btn_rect,
            Self::CONTROL_WORKSPACE_SETTINGS => self.ws_pill_rect,
            // The editor, and the keyboard and assistive choices of the
            // centre widget, all stand on the widget itself, which has no
            // position dots to point at.
            Self::CONTROL_CENTER_CLOCK
            | Self::CONTROL_CENTER_MONITOR
            | Self::CONTROL_CENTER_CALENDAR
            | Self::CONTROL_EDIT_WIDGETS => self.center_rect,
            Self::CONTROL_QUICK_SETTINGS => self.tray_hit_rect,
            Self::CONTROL_PROFILE => self.own_avatar_rect,
            Self::CONTROL_COLLABORATORS => {
                if self.avatar_stack_rect.is_zero_area() {
                    self.sharing_indicator_rect
                } else if self.sharing_indicator_rect.is_zero_area() {
                    self.avatar_stack_rect
                } else {
                    self.avatar_stack_rect.union(self.sharing_indicator_rect)
                }
            }
            Self::CONTROL_MEDIA => self.media_hit_rect(),
            Self::CONTROL_CONTINUITY => self.continuity_rect,
            Self::CONTROL_CONTINUITY_DISMISS => self.continuity_dismiss_rect,
            target if Self::editor_widget(target).is_some() => Self::editor_widget(target)
                .map_or(Rect::ZERO, |(widget, present)| {
                    self.widget_editor_action_rect(widget, present)
                }),
            _ => Rect::ZERO,
        }
    }

    /// Pointer/semantic target of the media indicator. The painted pill is
    /// a quiet 22 px status mark, which is deliberately smaller than a
    /// control; the target it answers to is still the bar's full control
    /// height (HIG §9.6: a small glyph gets a 32 px hit rect). Zero while
    /// nothing is capturing, so the control disappears with the mark.
    fn media_hit_rect(&self) -> Rect {
        if self.media_indicator_rect.is_zero_area() {
            return Rect::ZERO;
        }
        let cy = self.media_indicator_rect.center().y;
        Rect::new(
            self.media_indicator_rect.x0,
            cy - BAR_CONTROL_PX / 2.0,
            self.media_indicator_rect.x1,
            cy + BAR_CONTROL_PX / 2.0,
        )
    }

    fn control_name(&self, target: &str) -> String {
        match target {
            Self::CONTROL_TILO_MARK => t("topbar-tilo-menu"),
            Self::CONTROL_OVERVIEW => t("topbar-open-overview"),
            Self::CONTROL_WORKSPACE_UP => t("topbar-previous-workspace"),
            Self::CONTROL_WORKSPACE_DOWN => t("topbar-next-workspace"),
            Self::CONTROL_WORKSPACE_SETTINGS if self.has_workspace_menu() => t_args(
                "topbar-workspace-menu",
                &[("workspace", self.workspace_name.as_str().into())],
            ),
            Self::CONTROL_PROFILE => self.profile_control_name(),
            Self::CONTROL_WORKSPACE_SETTINGS => t_args(
                "topbar-workspace-settings",
                &[("workspace", self.workspace_name.as_str().into())],
            ),
            Self::CONTROL_CENTER_CLOCK => t("topbar-show-clock"),
            Self::CONTROL_CENTER_MONITOR => t("topbar-show-system-monitor"),
            Self::CONTROL_CENTER_CALENDAR => t("topbar-show-calendar"),
            Self::CONTROL_EDIT_WIDGETS | Self::CONTROL_WIDGET_EDITOR => t("topbar-editor-title"),
            Self::CONTROL_QUICK_SETTINGS => t("topbar-open-quick-settings"),
            Self::CONTROL_COLLABORATORS if self.invite_visible() => t("topbar-invite"),
            Self::CONTROL_COLLABORATORS => {
                let named = self.sharing_viewers();
                let watching = self.watching();
                let name = if named.is_empty() {
                    t("topbar-show-collaborators")
                } else {
                    let viewers = named
                        .iter()
                        .map(|(peer, name, is_agent, _)| {
                            let kind = match is_agent {
                                Some(true) => "agent",
                                Some(false) => "person",
                                None => "session",
                            };
                            let (view_only, controlled) = self.sharing_targets_for_peer(*peer);
                            let mut access = Vec::new();
                            if !controlled.is_empty() {
                                access.push(t_args(
                                    "topbar-viewer-can-control",
                                    &[("targets", controlled.join(", ").into())],
                                ));
                            }
                            if !view_only.is_empty() {
                                access.push(t_args(
                                    "topbar-viewer-view-only",
                                    &[("targets", view_only.join(", ").into())],
                                ));
                            }
                            t_args(
                                "topbar-viewer-entry",
                                &[
                                    ("name", name.as_str().into()),
                                    ("kind", kind.into()),
                                    ("access", access.join("; ").into()),
                                ],
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    t_args(
                        "topbar-collaborators-visible",
                        &[("viewers", viewers.into())],
                    )
                };
                if watching == 0 {
                    name
                } else {
                    format!(
                        "{name}; {}",
                        t_args("topbar-watching-description", &[("count", watching.into())])
                    )
                }
            }
            Self::CONTROL_MEDIA => t("topbar-capture-privacy"),
            Self::CONTROL_CONTINUITY => self
                .continuity_label()
                .unwrap_or_else(|| t("topbar-control")),
            Self::CONTROL_CONTINUITY_DISMISS => t("continuity-dismiss"),
            target if Self::editor_widget(target).is_some() => {
                let Some((widget, present)) = Self::editor_widget(target) else {
                    return t("topbar-editor-control");
                };
                t(match (widget, present) {
                    (TopBarWidget::Clock, true) => "topbar-editor-remove-clock",
                    (TopBarWidget::SystemMonitor, true) => "topbar-editor-remove-system-monitor",
                    (TopBarWidget::Calendar, true) => "topbar-editor-remove-calendar",
                    (TopBarWidget::Clock, false) => "topbar-editor-add-clock",
                    (TopBarWidget::SystemMonitor, false) => "topbar-editor-add-system-monitor",
                    (TopBarWidget::Calendar, false) => "topbar-editor-add-calendar",
                })
            }
            _ => t("topbar-control"),
        }
    }

    fn control_is_disabled(&self, target: &str) -> bool {
        matches!(
            target,
            Self::CONTROL_WORKSPACE_UP if !self.can_switch_up()
        ) || matches!(
            target,
            Self::CONTROL_WORKSPACE_DOWN if !self.can_switch_down()
        )
    }

    fn activate_editor_control(&mut self, target: &str) -> bool {
        let Some((widget, remove)) = Self::editor_widget(target) else {
            return false;
        };
        if remove {
            if self.widget_config.widgets.len() <= 1 {
                return false;
            }
            let Some(index) = self
                .widget_config
                .widgets
                .iter()
                .position(|candidate| *candidate == widget)
            else {
                return false;
            };
            self.widget_config.widgets.remove(index);
        } else {
            if self.widget_config.widgets.contains(&widget) {
                return false;
            }
            self.widget_config.widgets.push(widget);
        }
        self.cancel_editor_drag();
        if !self
            .widget_config
            .widgets
            .contains(&self.center_widget.into())
            && let Some(widget) = self.widget_config.widgets.first()
        {
            self.center_widget = (*widget).into();
        }
        if self
            .focused_control
            .as_deref()
            .and_then(Self::editor_widget)
            .is_some_and(|(focused, _)| focused == widget)
        {
            self.focused_control = Some(Self::editor_control(widget, !remove).into());
        }
        self.save_widget_config();
        self.cache_fp = CacheFingerprint::default();
        true
    }

    /// Bold, so it reads as the chrome's focal glance value.
    pub fn clock_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Semibold — a clickable title without tipping into the clock's
    /// h1/h2 tier.
    pub fn workspace_name_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    /// Secondary ink for every metric label: the words say which reading
    /// is which. They used to wear the status hues (green, blue, amber) as
    /// decoration, which made a calm readout the most colourful thing in
    /// the bar and spent status colour where nothing was wrong.
    pub fn metric_label_color(&self, theme: &Theme, _k: MetricKind) -> Color {
        theme.tokens.colors.text_dim
    }

    /// The label ink at low weight: a backdrop, not a competing element.
    /// `_k` kept so a per-metric variant can be wired in later.
    pub fn sparkline_fill(&self, theme: &Theme, _k: MetricKind) -> Color {
        self.metric_label_color(theme, MetricKind::Cpu)
            .with_alpha_factor(SPARKLINE_FILL_ALPHA)
    }

    pub fn overview_btn_rect(&self) -> Rect {
        self.overview_btn_rect
    }

    /// Whether the last layout stood the bar in or over the column other
    /// than it stands now.
    fn overlay_layout_stale(&self) -> bool {
        let fullscreen = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.snapshot().fullscreen);
        self.laid_out != BarStanding::of(!fullscreen && self.presence().overlays())
    }

    /// Whether the bar, hidden by the person, must stay shown for its own
    /// sake: its widget editor is open, or the keyboard is in it.
    pub fn holds_itself_shown(&self) -> bool {
        self.widget_editing || (self.focused && self.focus_origin == FocusOrigin::Keyboard)
    }

    /// Docked, hidden or shown over the windows, as the host says. A
    /// compact bar on another display always stays docked.
    pub fn presence(&self) -> TopBarPresence {
        if self.is_compact() {
            return TopBarPresence::Docked;
        }
        self.chrome
            .as_ref()
            .map_or(TopBarPresence::Docked, |chrome| chrome.snapshot().top_bar)
    }

    /// The bar shown over the windows stands on a surface of its own, the
    /// width of the display with the bar's hairline under it, so its
    /// controls read over whatever the windows show there.
    fn draw_overlay_surface(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        let rule = Rect::new(self.rect.x0, self.rect.y1 - 1.0, self.rect.x1, self.rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::chrome_hairline(theme),
            None,
            &rule,
        );
    }

    pub fn tray_pill_rect(&self) -> Rect {
        self.tray_pill_rect
    }

    /// A gauge, not a fixed glyph — the fill level is readable without
    /// reading the number.
    fn draw_battery_gauge(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        percent: u8,
        charging: bool,
        hue: Color,
        colors: &ui::tokens::colors::Colors,
    ) {
        // lucide's battery is a 16×12 body at x=2,y=6 in a 24 box, with
        // the nub from x=22. Keep those ratios so the gauge lines up
        // optically with the neighbouring stroked glyphs.
        let s = rect.width().min(rect.height()) / 24.0;
        let ox = rect.x0 + (rect.width() - 24.0 * s) / 2.0;
        let oy = rect.y0 + (rect.height() - 24.0 * s) / 2.0;
        let px = |x: f64| ox + x * s;
        let py = |y: f64| oy + y * s;
        let stroke_w = (ui::icons::LUCIDE_DEFAULT_STROKE_PX).max(1.0);

        let body = RoundedRect::new(px(2.0), py(6.0), px(18.0), py(18.0), 2.0 * s);
        scene.stroke(
            &vello::kurbo::Stroke::new(stroke_w),
            transform,
            colors.text_dim,
            None,
            &body,
        );
        // Terminal nub.
        scene.stroke(
            &vello::kurbo::Stroke::new(stroke_w).with_caps(vello::kurbo::Cap::Round),
            transform,
            colors.text_dim,
            None,
            &vello::kurbo::Line::new(
                vello::kurbo::Point::new(px(21.0), py(10.0)),
                vello::kurbo::Point::new(px(21.0), py(14.0)),
            ),
        );

        // Charge fill, inset one stroke inside the body so the outline
        // stays legible at every level.
        let inset = stroke_w + 0.5;
        let x0 = px(2.0) + inset;
        let x1 = px(18.0) - inset;
        let frac = (f64::from(percent) / 100.0).clamp(0.0, 1.0);
        if frac > 0.0 {
            // A hairline at 1% would disappear; give any nonzero charge
            // at least a visible sliver so "nearly empty" and "empty"
            // do not render identically.
            let w = ((x1 - x0) * frac).max(1.5);
            let fill = RoundedRect::new(
                x0,
                py(6.0) + inset,
                (x0 + w).min(x1),
                py(18.0) - inset,
                1.0 * s,
            );
            scene.fill(Fill::NonZero, transform, hue, None, &fill);
        }

        if charging {
            // Drawn in the surface colour with a faint outline (not an
            // accent) so the bolt reads on top of whatever fill level is
            // underneath it.
            let mut bolt = vello::kurbo::BezPath::new();
            bolt.move_to((px(11.5), py(7.5)));
            bolt.line_to((px(8.0), py(12.6)));
            bolt.line_to((px(11.0), py(12.6)));
            bolt.line_to((px(9.5), py(16.5)));
            bolt.line_to((px(13.5), py(11.0)));
            bolt.line_to((px(10.5), py(11.0)));
            bolt.close_path();
            scene.fill(Fill::NonZero, transform, colors.text_primary, None, &bolt);
            scene.stroke(
                &vello::kurbo::Stroke::new(0.75),
                transform,
                colors.surfaces.raised,
                None,
                &bolt,
            );
        }
    }

    // The following `draw_*` helpers exist only to keep `Widget::draw`
    // under the line budget; none is meaningful on its own.

    /// Overview icon, divider, and up/down workspace chevrons. Returns
    /// the x cursor position immediately after the down chevron, where
    /// the workspace name pill begins.
    #[allow(
        clippy::too_many_lines,
        reason = "one compact cluster owns its shared background, three aligned targets, disabled-state colours and returned layout cursor; splitting those would duplicate or desynchronise geometry"
    )]
    fn draw_workspace_nav(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let pad_x = f64::from(tokens.spacing.s3);
        let icon_size = BAR_GLYPH_PX;
        let control = BAR_CONTROL_PX;

        let mut x = self.draw_tilo_mark(scene, transform, theme, center_y, self.rect.x0 + pad_x);
        if self.has_workspace_menu() {
            // The overview and the other workspaces are in the workspace's
            // own menu: the name, then, is the next thing on the bar, its
            // capsule's `s2` leading pad past the mark's own gap.
            self.overview_btn_rect = Rect::ZERO;
            self.up_btn_rect = Rect::ZERO;
            self.down_btn_rect = Rect::ZERO;
            return x + f64::from(tokens.spacing.s2);
        }
        // The switches are square targets of the shared control height.
        let switch_w = BAR_CONTROL_PX;
        self.overview_btn_rect = Rect::new(
            x,
            center_y - control / 2.0,
            x + control,
            center_y + control / 2.0,
        );
        let overview_open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.snapshot().overlays.overview);
        if overview_open {
            paint_bar_selector(
                scene,
                transform,
                &RoundedRect::from_rect(
                    self.overview_btn_rect,
                    ui::chrome::ChromeRole::Selector.radius(theme, self.overview_btn_rect.height()),
                ),
                ui::chrome::SelectionState::active(true),
                theme,
            );
        }
        let overview_icon_rect = Rect::new(
            x + (control - icon_size) / 2.0,
            center_y - icon_size / 2.0,
            x + f64::midpoint(control, icon_size),
            center_y + icon_size / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            overview_icon_rect,
            Self::overview_glyph_icon(),
            colors.text_primary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += control + f64::from(tokens.spacing.s2);

        // Vertical divider: a separator on the Bar is the Bar's hairline
        // (HIG §9.5), so it reads on every preset the way the bottom edge does.
        let divider = Rect::new(
            x,
            center_y - SMALL_GLYPH_PX / 2.0,
            x + 1.0,
            center_y + SMALL_GLYPH_PX / 2.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::chrome_hairline(theme),
            None,
            &divider,
        );
        x += 1.0 + f64::from(tokens.spacing.s2);

        let chev = SMALL_GLYPH_PX;
        self.up_btn_rect = Rect::new(
            x,
            center_y - control / 2.0,
            x + switch_w,
            center_y + control / 2.0,
        );
        let up_icon_rect = Rect::new(
            x + (switch_w - chev) / 2.0,
            center_y - chev / 2.0,
            x + f64::midpoint(switch_w, chev),
            center_y + chev / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            up_icon_rect,
            ui::icons::Icon::ChevronUp,
            if self.can_switch_up() {
                colors.text_dim
            } else {
                // Disabled ink is the rest ink under the toolkit's disabled
                // alpha (HIG §9.5), not a border token borrowed as a colour.
                colors
                    .text_dim
                    .with_alpha_factor(ui::widget::DISABLED_ALPHA)
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += switch_w;

        self.down_btn_rect = Rect::new(
            x,
            center_y - control / 2.0,
            x + switch_w,
            center_y + control / 2.0,
        );
        let dn_icon_rect = Rect::new(
            x + (switch_w - chev) / 2.0,
            center_y - chev / 2.0,
            x + f64::midpoint(switch_w, chev),
            center_y + chev / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            dn_icon_rect,
            ui::icons::Icon::ChevronDown,
            if self.can_switch_down() {
                colors.text_dim
            } else {
                colors
                    .text_dim
                    .with_alpha_factor(ui::widget::DISABLED_ALPHA)
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        // Where the workspace pill's label starts: `s3` past the down
        // switch, which the pill's own `s2` leading pad turns into an `s1`
        // gutter between the cluster and the pill. The label's x is a
        // published fact — `navigation-layout.json`'s authored contrast
        // probes sample its glyphs — so the padding fix moved the capsule,
        // not the text.
        x + switch_w + f64::from(tokens.spacing.s3)
    }

    /// The Tilo mark as the bar's leading control, starting at `x`; returns
    /// where the next control starts (`x` itself without the mark).
    fn draw_tilo_mark(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
        x: f64,
    ) -> f64 {
        let Some(open) = self.tilo_mark else {
            self.tilo_mark_rect = Rect::ZERO;
            return x;
        };
        // The opt-in web menu keeps its minimum pointer target inside the
        // compact bar; native controls retain the smaller bar-control size.
        let control = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
        self.tilo_mark_rect = Rect::new(
            x,
            center_y - control / 2.0,
            x + control,
            center_y + control / 2.0,
        );
        if open {
            paint_bar_selector(
                scene,
                transform,
                &RoundedRect::from_rect(
                    self.tilo_mark_rect,
                    ui::chrome::ChromeRole::Selector.radius(theme, control),
                ),
                ui::chrome::SelectionState::active(true),
                theme,
            );
        }
        let side = f64::from(theme.tokens.metrics.icon_md);
        paint_tilo_mark(
            scene,
            transform,
            Rect::from_center_size(self.tilo_mark_rect.center(), (side, side)),
            theme,
        );
        x + control + f64::from(theme.tokens.spacing.s2)
    }

    /// The quiet pill naming where the workspace is hosted, at the label x
    /// the workspace pill would have had; returns where that pill's label
    /// starts after it (`x` itself without a host).
    fn draw_host_pill(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        x: f64,
    ) -> f64 {
        // With the workspace menu, where it is hosted is said in the menu.
        let Some(host) = self
            .host_label
            .as_deref()
            .filter(|_| !self.has_workspace_menu())
        else {
            return x;
        };
        let tokens = &theme.tokens;
        let pad = f64::from(tokens.spacing.s2);
        let style = tokens.typography.style(TextRole::Caption);
        // A short name's worth at most: the workspace's own name matters more.
        let host = text_renderer.elide_to_width_style(host, 140.0, &style);
        let (w, h) = text_renderer.measure_style(&host, None, &style);
        let pill = Rect::new(
            x - pad,
            center_y - BAR_CONTROL_PX / 2.0,
            x + f64::from(w) + pad,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::chrome_hairline(theme),
            None,
            &RoundedRect::from_rect(
                pill.inset(-0.5),
                ui::chrome::ChromeRole::Selector.radius(theme, pill.height()),
            ),
        );
        text_renderer.draw_style(
            scene,
            &host,
            tokens.colors.text_dim,
            at(transform, (x, center_y - f64::from(h) / 2.0)),
            None,
            &style,
        );
        pill.x1 + f64::from(tokens.spacing.s1) + pad
    }

    fn workspace_label(
        &self,
        text_renderer: &mut TextRenderer,
        ws_text_style: &TextStyle,
        label_budget: f32,
    ) -> String {
        // Workspace identity has priority over a redundant single-workspace
        // count; the workspace menu lists the others instead.
        let position_suffix = if self.workspace_total <= 1 || self.has_workspace_menu() {
            String::new()
        } else {
            format!(
                "  {}/{}",
                self.workspace_index.saturating_add(1),
                self.workspace_total
            )
        };
        let ws_weight = self.workspace_name_text_weight();
        let suffix_width = text_renderer
            .measure_style(
                &position_suffix,
                None,
                &ws_text_style.clone().with_weight(ws_weight),
            )
            .0;
        let name_budget = (label_budget - suffix_width).max(0.0);
        let visible_name = text_renderer.elide_to_width_style(
            &self.workspace_name,
            name_budget,
            &ws_text_style.clone().with_weight(ws_weight),
        );
        format!("{visible_name}{position_suffix}")
    }

    /// How wide the workspace name may grow in `room`. Named strips follow
    /// the pill, so the name leaves them their width down to a short
    /// name's.
    fn workspace_label_budget(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        room: f64,
    ) -> f32 {
        let strips = self.strips_natural_width(text_renderer, theme);
        let strips_room = if strips > 0.0 {
            strips + f64::from(theme.tokens.spacing.s2)
        } else {
            0.0
        };
        ui::num::f32_of_f64((room - strips_room).max(room.min(96.0)).clamp(32.0, 260.0))
    }

    /// Workspace-name pill (click toggles workspace settings) and its
    /// trailing gear glyph, starting at `x`.
    fn draw_workspace_pill(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        x: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let ws_text_style = tokens.typography.style(TextRole::Caption);
        // Measured at the same weight it's drawn at (Semibold), so the
        // pill width tracks the glyph advance, not the narrower Regular.
        let ws_weight = self.workspace_name_text_weight();
        // The centre carousel owns a fixed central lane.  Long translated or
        // user-authored workspace names must yield to it instead of painting
        // underneath it. Preserve the compact position suffix while eliding
        // only the user-authored portion, so even the compact layout says where
        // the current workspace sits.
        let center_left =
            f64::midpoint(self.rect.x0, self.rect.x1) - self.center_band_width(theme) / 2.0;
        // Capsule chrome around the label: a leading pad, the gap to the
        // gear, the gear itself and a trailing pad of the same size — so
        // the pill reads centred on its contents. The `s4` beyond that is
        // the breathing room `long_workspace_name_yields_to_the_center_lane`
        // keeps between the pill and the centre selector.
        let pad = f64::from(tokens.spacing.s2);
        let gear = SMALL_GLYPH_PX;
        let pill_chrome = pad * 3.0 + gear;
        let breathing = f64::from(tokens.spacing.s4);
        let label_budget = self.workspace_label_budget(
            text_renderer,
            theme,
            center_left - x - pill_chrome - breathing,
        );
        let ws_label = self.workspace_label(text_renderer, &ws_text_style, label_budget);
        let (ws_w, ws_h) = text_renderer.measure_style(
            &ws_label,
            None,
            &ws_text_style.clone().with_weight(ws_weight),
        );
        let ws_y = center_y - f64::from(ws_h) / 2.0;
        // A bar control's height, grown with its label at a large text
        // scale (as `ControlSize` grows), never taller than the bar.
        let pill_h = BAR_CONTROL_PX
            .max((f64::from(ws_h) + f64::from(tokens.spacing.s1) / 2.0).ceil())
            .min(self.rect.height().max(BAR_CONTROL_PX));
        let ws_pill_outer = Rect::new(
            x - pad,
            center_y - pill_h / 2.0,
            x + f64::from(ws_w) + pill_chrome - pad,
            center_y + pill_h / 2.0,
        );
        self.ws_pill_rect = ws_pill_outer;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.settings_anchor(ws_pill_outer);
        }
        // A thing you choose between is a capsule (HIG §9.1, Selector).
        let pill = RoundedRect::from_rect(
            ws_pill_outer,
            ui::chrome::ChromeRole::Selector.radius(theme, ws_pill_outer.height()),
        );
        paint_bar_selector(
            scene,
            transform,
            &pill,
            ui::chrome::SelectionState::active(true),
            theme,
        );
        let workspace_lens_open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.snapshot().overlays.ws_settings);
        if workspace_lens_open {
            let context_rule = RoundedRect::from_rect(
                Rect::new(
                    ws_pill_outer.x0 + pad,
                    ws_pill_outer.y1 - 2.5,
                    ws_pill_outer.x1 - pad,
                    ws_pill_outer.y1 - 0.5,
                ),
                1.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.inverted,
                None,
                &context_rule,
            );
        }
        text_renderer.draw_style(
            scene,
            &ws_label,
            colors.text_primary,
            at(transform, (x, ws_y)),
            None,
            &ws_text_style.clone().with_weight(ws_weight),
        );
        let gear_x = x + f64::from(ws_w) + pad;
        let gear_rect = Rect::new(
            gear_x,
            center_y - gear / 2.0,
            gear_x + gear,
            center_y + gear / 2.0,
        );
        self.ws_gear_rect = gear_rect;
        ui::icons::draw_icon(
            scene,
            transform,
            gear_rect,
            self.workspace_pill_icon(),
            if workspace_lens_open {
                colors.intent.inverted
            } else {
                colors.text_dim
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// The centre band. The clock is plain text on the bar, with no pill
    /// and no position dots (a pill around five characters read as a
    /// second bar inside the bar); the lane-filling widgets keep a quiet
    /// selector pill. Returns `(content_x0, content_x1, content_cx)`, the
    /// bounds the active centre widget draws into.
    fn draw_center_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) -> (f64, f64, f64) {
        let lane_w = self.center_band_width(theme);
        let band_w = self.center_pill_width(theme, text_renderer, lane_w);
        // The same height as every other bar control, so the band's top
        // and bottom edges line up with the pills either side of it.
        let band_h = BAR_CONTROL_PX;
        let band_cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let band = Rect::new(
            band_cx - band_w / 2.0,
            center_y - band_h / 2.0,
            band_cx + band_w / 2.0,
            center_y + band_h / 2.0,
        );
        self.center_rect = band;
        self.center_lane = Rect::new(
            band_cx - lane_w / 2.0,
            band.y0,
            band_cx + lane_w / 2.0,
            band.y1,
        );
        let widgets = &self.widget_config.widgets;
        if !widgets.contains(&TopBarWidget::from(self.center_widget)) {
            self.center_widget = widgets
                .first()
                .copied()
                .unwrap_or(TopBarWidget::Clock)
                .into();
        }
        if !self.center_is_plain_text() {
            let band_rr = RoundedRect::from_rect(
                band,
                ui::chrome::ChromeRole::Selector.radius(theme, band.height()),
            );
            paint_bar_selector(
                scene,
                transform,
                &band_rr,
                ui::chrome::SelectionState::active(true),
                theme,
            );
        }
        let (content_x0, content_x1) = Self::center_content_lane(band);
        let content_cx = f64::midpoint(content_x0, content_x1);
        (content_x0, content_x1, content_cx)
    }

    /// The clock with nothing live over it: drawn as bare text.
    fn center_is_plain_text(&self) -> bool {
        self.center_widget == CenterWidget::Clock && !self.activity_is_visible()
    }

    fn editor_controls(&self) -> Vec<&'static str> {
        let mut controls = Vec::with_capacity(3);
        if self.widget_config.widgets.len() > 1 {
            controls.extend(
                self.widget_config
                    .widgets
                    .iter()
                    .copied()
                    .map(|widget| Self::editor_control(widget, true)),
            );
        }
        controls.extend(
            TopBarWidget::ALL
                .into_iter()
                .filter(|widget| !self.widget_config.widgets.contains(widget))
                .map(|widget| Self::editor_control(widget, false)),
        );
        controls
    }

    fn open_widget_editor(&mut self) -> bool {
        if self.is_compact() {
            return false;
        }
        self.cancel_editor_drag();
        self.responsive.toolbar.close_menus();
        self.responsive.dirty = true;
        self.widget_editing = true;
        self.focused_control = self
            .editor_controls()
            .first()
            .map(|target| (*target).into());
        self.cache_fp = CacheFingerprint::default();
        true
    }

    fn cancel_editor_drag(&mut self) {
        self.widget_edit_drag.cancel();
        self.widget_edit_pressed = None;
        self.editor_pressed_control = None;
    }

    fn close_widget_editor(&mut self) {
        self.cancel_editor_drag();
        self.widget_editing = false;
        if self
            .focused_control
            .as_deref()
            .and_then(Self::editor_widget)
            .is_some()
        {
            self.focused_control = Some(Self::center_control(self.center_widget.into()).into());
        }
        self.cache_fp = CacheFingerprint::default();
    }

    fn widget_editor_row(&self, index: usize) -> Rect {
        self.editor_geometry
            .rows
            .get(index)
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    fn widget_editor_remove_rect(&self, index: usize) -> Rect {
        self.editor_geometry
            .remove
            .get(index)
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    fn widget_editor_add_rect(&self, widget: TopBarWidget) -> Rect {
        let index = TopBarWidget::ALL
            .iter()
            .position(|candidate| *candidate == widget)
            .unwrap_or(0);
        self.editor_geometry
            .add
            .get(index)
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    fn widget_editor_action_rect(&self, widget: TopBarWidget, present: bool) -> Rect {
        if !self.widget_editing {
            return Rect::ZERO;
        }
        if present {
            if self.widget_config.widgets.len() <= 1 {
                return Rect::ZERO;
            }
            self.widget_config
                .widgets
                .iter()
                .position(|candidate| *candidate == widget)
                .map_or(Rect::ZERO, |index| self.widget_editor_remove_rect(index))
        } else {
            self.widget_editor_add_rect(widget)
        }
    }

    fn editor_label(widget: TopBarWidget) -> String {
        t(match widget {
            TopBarWidget::Clock => "topbar-widget-clock",
            TopBarWidget::SystemMonitor => "topbar-widget-system-monitor",
            TopBarWidget::Calendar => "topbar-widget-calendar",
        })
    }

    fn editor_add_label(widget: TopBarWidget) -> String {
        t(match widget {
            TopBarWidget::Clock => "topbar-editor-add-clock-chip",
            TopBarWidget::SystemMonitor => "topbar-editor-add-monitor-chip",
            TopBarWidget::Calendar => "topbar-editor-add-calendar-chip",
        })
    }

    fn editor_metrics(&self) -> ListMetrics {
        ListMetrics::uniform(
            self.editor_geometry
                .rows
                .first()
                .map_or(0.0, |rect| rect.y0),
            self.editor_geometry.row_height,
            self.editor_geometry.row_gap,
            self.widget_config.widgets.len(),
        )
    }

    fn editor_preferred_width(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        remove_width: f64,
        button_height: f64,
    ) -> f64 {
        let pad = f64::from(theme.tokens.spacing.s4);
        let widest = self
            .widget_config
            .widgets
            .iter()
            .copied()
            .map(|widget| {
                f64::from(
                    text.measure_style(
                        &Self::editor_label(widget),
                        None,
                        &theme.text(TextRole::Body),
                    )
                    .0,
                )
            })
            .fold(0.0, f64::max);
        let leading_width = ListRowFace::new("")
            .leading(RowLeading::Icon(ui::icons::Icon::Menu))
            .leading_rect(Rect::new(0.0, 0.0, 1000.0, button_height), text, theme)
            .width();
        (360.0 * f64::from(theme.ui_scale)).max(
            widest
                + remove_width
                + leading_width
                + 2.0 * pad
                + 2.0 * ui::chrome::row_bleed(theme)
                + 2.0 * f64::from(theme.tokens.spacing.s3),
        )
    }

    fn editor_row_face(label: &str, remove_width: f64, compact: bool) -> ListRowFace<'_> {
        let face = ListRowFace::new(label).trailing_width(remove_width);
        if compact {
            face.two_line_title()
        } else {
            face.leading(RowLeading::Icon(ui::icons::Icon::Menu))
        }
    }

    fn editor_remove_metrics(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        content_width: f64,
        remove_width: f64,
    ) -> (f64, bool) {
        let probe = Rect::new(
            0.0,
            0.0,
            content_width,
            ButtonFace::height(ControlSize::Small, theme),
        );
        let compact_rows = self.widget_config.widgets.iter().any(|widget| {
            Self::editor_row_face(&Self::editor_label(*widget), remove_width, false)
                .title_elided(probe, text, theme)
        });
        let remove_width = if compact_rows {
            ButtonFace::icon_only(
                Rect::ZERO,
                ui::icons::Icon::Minus,
                &t("topbar-editor-remove"),
            )
            .control_size(ControlSize::Small)
            .intrinsic_width(text, theme)
        } else {
            remove_width
        };
        (remove_width, compact_rows)
    }

    fn resolve_editor_geometry(&self, text: &mut TextRenderer, theme: &Theme) -> EditorGeometry {
        let pad = f64::from(theme.tokens.spacing.s4);
        let gap = f64::from(theme.tokens.spacing.s2);
        let outer = gap.min(self.rect.width() / 2.0);
        let remove_label = t("topbar-editor-remove");
        let remove_face = ButtonFace::new(Rect::ZERO, &remove_label)
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small);
        let remove_width = remove_face.intrinsic_width(text, theme);
        let button_height = ButtonFace::height(ControlSize::Small, theme);
        let preferred = self.editor_preferred_width(text, theme, remove_width, button_height);
        let width = preferred.min((self.rect.width() - 2.0 * outer).max(0.0));
        let left = (self.center_rect.center().x - width / 2.0)
            .clamp(self.rect.x0 + outer, self.rect.x1 - outer - width);
        let content_width = (width - 2.0 * pad).max(0.0);
        let (remove_width, compact_rows) =
            self.editor_remove_metrics(text, theme, content_width, remove_width);
        let top = self.rect.y1 + gap;
        let title_h = f64::from(
            text.measure_style(
                &t("topbar-editor-title"),
                Some(editor_text_width(content_width)),
                &theme.text(TextRole::Subheading),
            )
            .1,
        );
        let hint_h = f64::from(
            text.measure_style(
                &t("topbar-editor-hint"),
                Some(editor_text_width(content_width)),
                &theme.text(TextRole::Caption),
            )
            .1,
        );
        let mut y = top + pad;
        let title = Rect::new(left + pad, y, left + width - pad, y + title_h);
        y = title.y1 + f64::from(theme.tokens.spacing.s1);
        let hint = Rect::new(left + pad, y, left + width - pad, y + hint_h);
        y = hint.y1 + f64::from(theme.tokens.spacing.s3);
        let row_height = Self::editor_row_face(&remove_label, remove_width, compact_rows)
            .height(text, theme)
            .max(button_height + gap);
        let mut geometry = EditorGeometry {
            title,
            hint,
            row_height,
            row_gap: gap,
            compact_rows,
            widgets: self.widget_config.widgets.clone(),
            ..Default::default()
        };
        for widget in &self.widget_config.widgets {
            let label = Self::editor_label(*widget);
            let row = Rect::new(left + pad, y, left + width - pad, y + row_height);
            let face = Self::editor_row_face(&label, remove_width, compact_rows);
            let slot = face.trailing_rect(row, text, theme).unwrap_or(Rect::ZERO);
            let button = Rect::new(
                slot.x0,
                row.center().y - button_height / 2.0,
                slot.x1,
                row.center().y + button_height / 2.0,
            );
            geometry.rows.push(row);
            geometry.remove.push(button);
            y = row.y1 + gap;
        }
        if !geometry.rows.is_empty() {
            y -= gap;
        }
        let mut x = left + pad;
        let mut add_y = y + f64::from(theme.tokens.spacing.s3);
        let mut has_add = false;
        for (index, widget) in TopBarWidget::ALL.into_iter().enumerate() {
            if self.widget_config.widgets.contains(&widget) {
                continue;
            }
            let label = Self::editor_add_label(widget);
            let button_width = ButtonFace::new(Rect::ZERO, &label)
                .control_size(ControlSize::Small)
                .intrinsic_width(text, theme)
                .min(content_width);
            if x > left + pad && x + button_width > left + width - pad {
                x = left + pad;
                add_y += button_height + gap;
            }
            if let Some(rect) = geometry.add.get_mut(index) {
                *rect = Rect::new(x, add_y, x + button_width, add_y + button_height);
            }
            x += button_width + gap;
            has_add = true;
        }
        if has_add {
            y = add_y + button_height;
        }
        geometry.panel = Rect::new(left, top, left + width, y + pad);
        geometry
    }

    fn editor_button(
        &self,
        rect: Rect,
        label: &str,
        target: &'static str,
        disabled: bool,
        ctx: &mut ui::paint::PaintCtx<'_>,
    ) {
        let face = if self.editor_geometry.compact_rows
            && Self::editor_widget(target).is_some_and(|(_, remove)| remove)
        {
            ButtonFace::icon_only(rect, ui::icons::Icon::Minus, label)
        } else {
            ButtonFace::new(rect, label)
        };
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &face
                .role(
                    if Self::editor_widget(target).is_some_and(|(_, remove)| remove) {
                        ButtonRole::Plain
                    } else {
                        ButtonRole::Secondary
                    },
                )
                .control_size(ControlSize::Small)
                .disabled(disabled)
                .pressed(self.editor_pressed_control == Some(target))
                .hovered(self.last_cursor.is_some_and(|point| rect.contains(point)))
                .focused(self.focused && self.focused_control.as_deref() == Some(target)),
        );
    }

    fn shift_editor_rect(rect: Rect, shift: f64) -> Rect {
        Rect::new(rect.x0, rect.y0 + shift, rect.x1, rect.y1 + shift)
    }

    fn draw_editor_rows(&mut self, ctx: &mut ui::paint::PaintCtx<'_>) {
        let metrics = self.editor_metrics();
        self.widget_edit_drag.sync_insertion(&metrics);
        for (index, widget) in self.widget_config.widgets.iter().copied().enumerate() {
            if self.widget_edit_drag.from_index() == Some(index) {
                continue;
            }
            let shift = self.widget_edit_drag.item_shift(index, &metrics);
            let row = Self::shift_editor_rect(self.widget_editor_row(index), shift);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                ctx.theme.tokens.colors.surfaces.raised,
                None,
                &RoundedRect::from_rect(row, f64::from(ctx.theme.tokens.radii.control)),
            );
            let button = Self::shift_editor_rect(self.widget_editor_remove_rect(index), shift);
            let label = Self::editor_label(widget);
            Self::editor_row_face(&label, button.width(), self.editor_geometry.compact_rows)
                .separator(false)
                .paint(ctx, row);
            self.editor_button(
                button,
                &t("topbar-editor-remove"),
                Self::editor_control(widget, true),
                self.widget_config.widgets.len() <= 1,
                ctx,
            );
        }
        if let Some(ghost) = self.widget_edit_drag.ghost_rect(
            self.widget_editor_rect.x0 + f64::from(ctx.theme.tokens.spacing.s4),
            self.widget_editor_rect.x1 - f64::from(ctx.theme.tokens.spacing.s4),
            &metrics,
        ) {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                ctx.theme.tokens.colors.text_primary.with_alpha(0.22),
                None,
                &RoundedRect::from_rect(ghost, f64::from(ctx.theme.tokens.radii.control)),
            );
        }
    }

    fn draw_widget_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.widget_editing {
            self.widget_editor_rect = Rect::ZERO;
            return;
        }
        let geometry = self.resolve_editor_geometry(text, theme);
        if geometry != self.editor_geometry {
            self.cancel_editor_drag();
        }
        self.editor_geometry = geometry;
        self.widget_editor_rect = self.editor_geometry.panel;
        let panel = self.widget_editor_rect;
        let rr = RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.panel));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &rr,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::surface::edge_on(theme.tokens.colors.surfaces.overlay),
            None,
            &rr,
        );
        for (key, rect, role) in [
            (
                "topbar-editor-title",
                self.editor_geometry.title,
                TextRole::Subheading,
            ),
            (
                "topbar-editor-hint",
                self.editor_geometry.hint,
                TextRole::Caption,
            ),
        ] {
            text.draw_style(
                scene,
                &t(key),
                if role == TextRole::Caption {
                    theme.tokens.colors.text_dim
                } else {
                    theme.tokens.colors.text_primary
                },
                at(transform, (rect.x0, rect.y0)),
                Some(editor_text_width(rect.width())),
                &theme.text(role),
            );
        }
        self.draw_editor_rows(&mut ui::paint::PaintCtx {
            scene,
            transform,
            text,
            theme,
        });
        for widget in TopBarWidget::ALL {
            let rect = self.widget_editor_add_rect(widget);
            if !rect.is_zero_area() {
                self.editor_button(
                    rect,
                    &Self::editor_add_label(widget),
                    Self::editor_control(widget, false),
                    false,
                    &mut ui::paint::PaintCtx {
                        scene,
                        transform,
                        text,
                        theme,
                    },
                );
            }
        }
    }

    /// The lane the active centre widget draws into: the same inset in from
    /// either end of the band. One function so
    /// [`Self::draw_center_band`] and [`Self::activity_content_width`]
    /// cannot disagree about how much room the activity headline has.
    fn center_content_lane(band: Rect) -> (f64, f64) {
        // Symmetric, so the content's centre is the band's centre.
        (band.x0 + CENTER_INSET_PX, band.x1 - CENTER_INSET_PX)
    }

    /// What the clock widget reads: the time, or the running Clocks
    /// timer or stopwatch with its label.
    fn clock_widget_text(&self) -> String {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.snapshot().clock_activity)
            .map_or_else(
                || self.current_clock_string(),
                |activity| {
                    let mins = activity.remaining_secs / 60;
                    let secs = activity.remaining_secs % 60;
                    if activity.label.is_empty() {
                        format!("{mins:02}:{secs:02}")
                    } else {
                        format!("{}  {mins:02}:{secs:02}", activity.label)
                    }
                },
            )
    }

    /// The width of the centre widget inside its `lane_w` lane. The
    /// calendar and the system monitor fill the lane; the clock is a
    /// five-character reading, so its target hugs it: the time and the
    /// content inset either side. The time is measured with every digit set
    /// as the widest one, so the target does not change width minute to
    /// minute.
    fn center_pill_width(&self, theme: &Theme, text: &mut TextRenderer, lane_w: f64) -> f64 {
        if self.activity_is_visible() || self.center_widget != CenterWidget::Clock {
            return lane_w;
        }
        let style = theme
            .tokens
            .typography
            .style(TextRole::Body)
            .with_weight(self.clock_text_weight());
        let widest_digit = ('0'..='9')
            .map(|digit| {
                (
                    text.measure_style(&digit.to_string(), None, &style).0,
                    digit,
                )
            })
            .fold((0.0_f32, '0'), |widest, next| {
                if next.0 > widest.0 { next } else { widest }
            })
            .1;
        let reading: String = self
            .clock_widget_text()
            .chars()
            .map(|c| if c.is_ascii_digit() { widest_digit } else { c })
            .collect();
        let (width, _) = text.measure_style(&reading, None, &style);
        (f64::from(width) + 2.0 * CENTER_INSET_PX).min(lane_w)
    }

    fn draw_clock_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        content_cx: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let s = self.clock_widget_text();
        let style = tokens.typography.style(TextRole::Body);
        // Measured at the draw weight — Bold advances wider
        // than Regular, so centering would drift otherwise.
        let weight = self.clock_text_weight();
        let (tw, th) = text_renderer.measure_style(&s, None, &style.clone().with_weight(weight));
        let tx = content_cx - f64::from(tw) / 2.0;
        let ty = center_y - f64::from(th) / 2.0;
        text_renderer.draw_style(
            scene,
            &s,
            colors.text_primary,
            at(transform, (tx, ty)),
            None,
            &style.with_weight(weight),
        );
    }

    /// One `LABEL` + value-string pair, e.g. `CPU` at `label_x` and
    /// `12%` at `value_x`. Kept as explicit positions rather than a
    /// fixed offset: TMP's value sits 22px right of its label while
    /// CPU/RAM's sits 26px right of theirs.
    fn draw_metric_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        spec: &MetricLabelSpec<'_>,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let label_style = tokens.typography.style(TextRole::Caption);
        let value_style = tokens.typography.style(TextRole::Caption);
        // Each run sits on the bar's centre line by its own measured line
        // height: the label and the value are set at different sizes, so a
        // shared fixed offset would seat one of them off-centre — and
        // drift further the moment the text scale changes.
        let (_, label_h) = text_renderer.measure_style(
            spec.label,
            None,
            &label_style.clone().with_weight(TextWeight::Semibold),
        );
        let (_, value_h) = text_renderer.measure_style(
            spec.value_str,
            None,
            &value_style.clone().with_weight(TextWeight::Bold),
        );
        text_renderer.draw_style(
            scene,
            spec.label,
            self.metric_label_color(theme, spec.kind),
            at(
                transform,
                (spec.label_x, spec.center_y - f64::from(label_h) / 2.0),
            ),
            None,
            &label_style.with_weight(TextWeight::Semibold),
        );
        text_renderer.draw_style(
            scene,
            spec.value_str,
            colors.text_primary,
            at(
                transform,
                (spec.value_x, spec.center_y - f64::from(value_h) / 2.0),
            ),
            None,
            &value_style.with_weight(TextWeight::Bold),
        );
    }

    /// The deterministic-but-varied CPU sparkline bars across `spark_x`,
    /// the room left between the CPU readout and the RAM label. Skipped
    /// when that room is narrower than `s12`: bars painted under a label
    /// would say less than no bars.
    fn draw_cpu_sparkline(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
        spark_x: (f64, f64),
        samples: &[u8],
    ) {
        let (spark_x0, spark_x1) = spark_x;
        let spark_w = spark_x1 - spark_x0;
        if spark_w < f64::from(theme.tokens.spacing.s12) {
            return;
        }
        let bars = samples.len();
        if bars == 0 {
            return;
        }
        let bar_gap = 1.0;
        let bar_w =
            (spark_w - (small_count_to_f64(bars) - 1.0) * bar_gap) / small_count_to_f64(bars);
        for (i, value) in samples.iter().enumerate() {
            let seed = usize::from((*value).min(100));
            let h = 4.0 + small_count_to_f64(seed) * 0.08;
            let bx = spark_x0 + small_count_to_f64(i) * (bar_w + bar_gap);
            let b = Rect::new(bx, center_y + 6.0 - h, bx + bar_w, center_y + 6.0);
            scene.fill(
                Fill::NonZero,
                transform,
                self.sparkline_fill(theme, MetricKind::Cpu),
                None,
                &b,
            );
        }
    }

    fn draw_system_monitor_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        content_x: (f64, f64),
    ) {
        let (content_x0, content_x1) = content_x;
        let cpu_str = self
            .telemetry
            .cpu_percent
            .map_or_else(|| "—".into(), |v| format!("{v}%"));
        let ram_str = self
            .telemetry
            .ram_percent
            .map_or_else(|| "—".into(), |v| format!("{v}%"));
        let tmp_str = self
            .telemetry
            .temperature_celsius
            .map_or_else(|| "—".into(), |v| format!("{v}°"));

        // Positions come from the measured advance of each run at the
        // weight and size it is drawn with, not from fixed offsets: a
        // three-digit value or a larger text scale then pushes its
        // neighbours instead of painting over them.
        let tokens = &theme.tokens;
        let s1 = f64::from(tokens.spacing.s1);
        let s3 = f64::from(tokens.spacing.s3);
        let label_style = tokens.typography.style(TextRole::Caption);
        let value_style = tokens.typography.style(TextRole::Caption);
        let label_w = |tr: &mut TextRenderer, t: &str| {
            f64::from(
                tr.measure_style(
                    t,
                    None,
                    &label_style.clone().with_weight(TextWeight::Semibold),
                )
                .0,
            )
        };
        let value_w = |tr: &mut TextRenderer, t: &str| {
            f64::from(
                tr.measure_style(t, None, &value_style.clone().with_weight(TextWeight::Bold))
                    .0,
            )
        };
        let widths = MetricWidths {
            cpu_label: label_w(text_renderer, "CPU"),
            cpu_value: value_w(text_renderer, &cpu_str),
            ram_label: label_w(text_renderer, "RAM"),
            ram_value: value_w(text_renderer, &ram_str),
            tmp_label: label_w(text_renderer, "TMP"),
            tmp_value: value_w(text_renderer, &tmp_str),
        };
        let columns = monitor_columns((content_x0, content_x1), &widths, s1, s3);

        self.draw_metric_label(
            scene,
            transform,
            theme,
            text_renderer,
            &MetricLabelSpec {
                center_y,
                label_x: columns.cpu_label_x,
                value_x: columns.cpu_value_x,
                label: "CPU",
                value_str: &cpu_str,
                kind: MetricKind::Cpu,
            },
        );
        self.draw_cpu_sparkline(
            scene,
            transform,
            theme,
            center_y,
            columns.spark,
            &self.telemetry.cpu_history,
        );
        if let Some((label_x, value_x)) = columns.ram {
            self.draw_metric_label(
                scene,
                transform,
                theme,
                text_renderer,
                &MetricLabelSpec {
                    center_y,
                    label_x,
                    value_x,
                    label: "RAM",
                    value_str: &ram_str,
                    kind: MetricKind::Ram,
                },
            );
        }
        if let Some((label_x, value_x)) = columns.tmp {
            self.draw_metric_label(
                scene,
                transform,
                theme,
                text_renderer,
                &MetricLabelSpec {
                    center_y,
                    label_x,
                    value_x,
                    label: "TMP",
                    value_str: &tmp_str,
                    kind: MetricKind::Tmp,
                },
            );
        }
    }

    fn draw_calendar_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        content_cx: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let (weekday, date, note) = calendar_strip_parts(self.live_clock());
        let prefix = weekday.as_str();
        let event = date.as_str();
        let time = note.as_str();
        let size = tokens.typography.style(TextRole::Caption);
        // prefix/time dim Medium, event a Semibold primary anchor.
        let prefix_weight = TextWeight::Medium;
        let event_weight = TextWeight::Semibold;
        let time_weight = TextWeight::Medium;
        let (pw, ph) =
            text_renderer.measure_style(prefix, None, &size.clone().with_weight(prefix_weight));
        let (ew, _) =
            text_renderer.measure_style(event, None, &size.clone().with_weight(event_weight));
        let (tw, th) = text_renderer.measure_style(
            time,
            None,
            &tokens
                .typography
                .style(TextRole::Caption)
                .with_weight(time_weight),
        );
        let gap = f64::from(tokens.spacing.s2);
        let total = f64::from(pw) + gap + f64::from(ew) + gap + f64::from(tw);
        let mut tx = content_cx - total / 2.0;
        let ty = center_y - f64::from(ph) / 2.0;
        // The smaller note is centred on its own line height rather than
        // nudged off the prefix's baseline by a fixed pixel.
        let time_y = center_y - f64::from(th) / 2.0;
        text_renderer.draw_style(
            scene,
            prefix,
            colors.text_dim,
            at(transform, (tx, ty)),
            None,
            &size.clone().with_weight(prefix_weight),
        );
        tx += f64::from(pw) + gap;
        text_renderer.draw_style(
            scene,
            event,
            colors.text_primary,
            at(transform, (tx, ty)),
            None,
            &size.with_weight(event_weight),
        );
        tx += f64::from(ew) + gap;
        text_renderer.draw_style(
            scene,
            time,
            colors.text_dim,
            at(transform, (tx, time_y)),
            None,
            &tokens
                .typography
                .style(TextRole::Caption)
                .with_weight(time_weight),
        );
    }

    /// Sizes the tray pill, paints its body + the critical-battery
    /// highlight, and returns the layout the icon helpers reuse.
    fn draw_tray_pill_body(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) -> TrayLayout {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let pad_x = f64::from(tokens.spacing.s3);
        let rx = self.rect.x1 - pad_x;

        // Sizes itself to what's actually knowable; an unarrived
        // reading contributes nothing rather than a plausible default.
        let tray_h = BAR_CONTROL_PX;
        // Square glyph slots the control's height, set close: the cluster is
        // one target, so its glyphs need no air of their own.
        let tray_slot = BAR_CONTROL_PX;
        let tray_pad = f64::from(tokens.spacing.s2);
        let tray_gap = 0.0;
        let icon_px = BAR_GLYPH_PX;
        let pct_style = tokens.typography.style(TextRole::Caption);
        // The unread count rides beside the bell; its width is measured
        // here so the pill is sized for it before anything is painted.
        let badge_w = if self.notification_badge > 0 {
            let label = Self::notification_badge_label(self.notification_badge);
            let (w, h) = text_renderer.measure_style(&label, None, &pct_style);
            Self::notification_badge_size(w, h, theme).0
        } else {
            0.0
        };

        let has_volume = self.status.volume.is_some();
        let has_link = self.status.link.is_some();
        let battery_pct = self.status.battery_percent();
        let pct_label = battery_pct.map(|p| format!("{p}%"));
        let pct_strong = pct_style.clone().with_weight(TextWeight::Semibold);
        let pct_w = pct_label.as_ref().map_or(0.0, |s| {
            f64::from(text_renderer.measure_style(s, None, &pct_strong).0)
        });
        let mut slots = 0usize;
        if has_volume {
            slots = slots.saturating_add(1);
        }
        if has_link {
            slots = slots.saturating_add(1);
        }
        if battery_pct.is_some() {
            slots = slots.saturating_add(1);
        }
        if self.notification_badge > 0 {
            slots = slots.saturating_add(1);
        }
        let mut tray_w = if slots == 0 {
            // Still the quick-settings trigger, so keep one slot for
            // the settings glyph.
            tray_pad * 2.0 + tray_slot
        } else {
            tray_pad * 2.0
                + small_count_to_f64(slots) * tray_slot
                + small_count_to_f64(slots.saturating_sub(1)) * tray_gap
        };
        if pct_w > 0.0 {
            tray_w += 3.0 + pct_w;
        }
        if self.notification_badge > 0 {
            // Bell plus a compact exact count (`99+` once the number no longer
            // fits); the base slot calculation already paid for the bell.
            tray_w += badge_w + tray_gap;
        }

        self.tray_pill_rect = Rect::new(
            rx - tray_w,
            center_y - tray_h / 2.0,
            rx,
            center_y + tray_h / 2.0,
        );
        self.tray_hit_rect = Rect::new(
            self.tray_pill_rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        let tray = RoundedRect::from_rect(
            self.tray_pill_rect,
            ui::chrome::ChromeRole::Selector.radius(theme, tray_h),
        );
        paint_bar_selector(
            scene,
            transform,
            &tray,
            ui::chrome::SelectionState::active(true),
            theme,
        );
        // At 3% the user needs to notice the whole pill, not just read
        // the glyph.
        if self.status.battery_is_critical() {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.semantic.danger.with_alpha_factor(0.18),
                None,
                &tray,
            );
        }

        TrayLayout {
            tray_w,
            tray_slot,
            tray_pad,
            tray_gap,
            icon_px,
            pct_style,
            pct_label,
            badge_w,
            slots,
            center_y,
            dim: colors.text_dim,
            bright: colors.text_primary,
            warning: colors.semantic.warning,
            danger: colors.semantic.danger,
            success: colors.semantic.success,
        }
    }

    fn draw_tray_settings_icon(scene: &mut Scene, transform: Affine, layout: &TrayLayout, sx: f64) {
        ui::icons::draw_icon(
            scene,
            transform,
            layout.slot_rect(sx),
            ui::icons::Icon::Settings2,
            layout.dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// Draws the bell + compact unread count if there are unread
    /// notifications. Returns the x cursor position for the next slot
    /// (unchanged if not drawn).
    fn draw_tray_notification_badge(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        if self.notification_badge == 0 {
            return sx;
        }
        ui::icons::draw_icon(
            scene,
            transform,
            layout.slot_rect(sx),
            ui::icons::Icon::Bell,
            layout.bright,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let label = Self::notification_badge_label(self.notification_badge);
        // The count is set at the chrome's numeric-readout size, so it
        // follows the person's text scale like every other bar label.
        let label_style = &layout.pct_style;
        let (label_w, label_h) = text_renderer.measure_style(&label, None, label_style);
        // The width was reserved in `draw_tray_pill_body` from the same
        // label; reading it back keeps the pill and the badge in agreement.
        let (_, badge_h) = Self::notification_badge_size(label_w, label_h, theme);
        let badge_w = layout.badge_w;
        let badge = Rect::new(
            sx + layout.tray_slot,
            layout.center_y - badge_h / 2.0,
            sx + layout.tray_slot + badge_w,
            layout.center_y + badge_h / 2.0,
        );
        // A count badge is a capsule (Selector radius) in the danger hue;
        // its ink is whichever text tone stays legible on that fill, so a
        // light or high-contrast palette is not handed a fixed white.
        scene.fill(
            Fill::NonZero,
            transform,
            layout.danger,
            None,
            &RoundedRect::from_rect(
                badge,
                ui::chrome::ChromeRole::Selector.radius(theme, badge.height()),
            ),
        );
        let ink = theme
            .tokens
            .colors
            .ink_on(layout.danger, layout.bright, 4.5);
        text_renderer.draw_style(
            scene,
            &label,
            ink,
            at(
                transform,
                (
                    badge.x0 + (badge.width() - f64::from(label_w)) / 2.0,
                    badge.y0 + (badge.height() - f64::from(label_h)) / 2.0,
                ),
            ),
            None,
            label_style,
        );
        sx + layout.tray_slot + badge_w + layout.tray_gap
    }

    /// `99+` once the exact number no longer fits the compact badge.
    fn notification_badge_label(count: u32) -> String {
        if count > 99 {
            "99+".to_string()
        } else {
            count.to_string()
        }
    }

    /// Badge size for a measured count label: `s1` of padding above and
    /// below the line, `s1` either side, never shorter than `s4` nor
    /// narrower than it is tall (so a single digit is a round dot rather
    /// than a sliver). Whole pixels, so the capsule's edges stay crisp.
    fn notification_badge_size(label_w: f32, label_h: f32, theme: &Theme) -> (f64, f64) {
        let s1 = f64::from(theme.tokens.spacing.s1);
        let s4 = f64::from(theme.tokens.spacing.s4);
        let h = (f64::from(label_h) + s1).max(s4).round();
        let w = (f64::from(label_w) + 2.0 * s1).max(h).round();
        (w, h)
    }

    /// Draws the volume glyph if the status has a reading. Returns the
    /// x cursor position for the next slot (unchanged if not drawn).
    fn draw_tray_volume_icon(
        scene: &mut Scene,
        transform: Affine,
        status: &crate::chrome::SystemStatus,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        if status.volume.is_none() {
            return sx;
        }
        let silent = status.audio_is_silent();
        ui::icons::draw_icon(
            scene,
            transform,
            layout.slot_rect(sx),
            if silent {
                ui::icons::Icon::VolumeX
            } else {
                ui::icons::Icon::Volume2
            },
            // Silence is chosen and may want undoing, so it reads
            // full strength; a normal level stays dim.
            if silent { layout.bright } else { layout.dim },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        sx + layout.tray_slot + layout.tray_gap
    }

    /// Draws the network glyph if the status has a reading. Returns the
    /// x cursor position for the next slot (unchanged if not drawn).
    fn draw_tray_network_icon(
        scene: &mut Scene,
        transform: Affine,
        status: &crate::chrome::SystemStatus,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        let Some(link) = status.link.clone() else {
            return sx;
        };
        let r = layout.slot_rect(sx);
        match link.kind {
            crate::chrome::LinkKind::Wired => {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    r,
                    ui::icons::Icon::EthernetPort,
                    layout.dim,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            crate::chrome::LinkKind::Offline => {
                // Warning hue answers "why is nothing loading" at
                // a glance.
                ui::icons::draw_icon(
                    scene,
                    transform,
                    r,
                    ui::icons::Icon::WifiOff,
                    layout.warning,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            crate::chrome::LinkKind::Wifi => {
                // `icon_subpaths(Wifi)` yields [dot, outer, middle,
                // inner], so a weak signal lights fewer arcs — the
                // same glyph, read as a meter.
                let bars = crate::chrome::link_bars(&link);
                let dim = layout.dim;
                let faint = dim.with_alpha_factor(0.25);
                ui::icons::draw_icon_layered(
                    scene,
                    transform,
                    r,
                    ui::icons::Icon::Wifi,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    |i| {
                        // 0 = base dot (always lit), 1 = outer
                        // (needs 4 bars), 2 = middle (3), 3 = inner (2).
                        let needed = match i {
                            0 => 1,
                            1 => 4,
                            2 => 3,
                            _ => 2,
                        };
                        if bars >= needed { dim } else { faint }
                    },
                );
            }
        }
        sx + layout.tray_slot + layout.tray_gap
    }

    /// Draws the battery gauge + percent label if the status has a
    /// reading.
    fn draw_tray_battery_icon(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        status: &crate::chrome::SystemStatus,
        layout: &TrayLayout,
        sx: f64,
        colors: &ui::tokens::colors::Colors,
    ) {
        let Some(pct) = status.battery_percent() else {
            return;
        };
        let charging = status.battery_is_charging();
        let hue = if status.battery_is_critical() {
            layout.danger
        } else if status.battery_is_low() {
            layout.warning
        } else if charging {
            layout.success
        } else {
            layout.dim
        };
        Self::draw_battery_gauge(
            scene,
            transform,
            layout.slot_rect(sx),
            pct,
            charging,
            hue,
            colors,
        );
        let label_x = sx + layout.tray_slot;
        if let Some(label) = layout.pct_label.as_ref() {
            // The gauge is the at-a-glance read; the number is
            // what you check when the gauge worries you.
            let (_, lh) = text_renderer.measure_style(
                label,
                None,
                &layout.pct_style.clone().with_weight(TextWeight::Semibold),
            );
            text_renderer.draw_style(
                scene,
                label,
                if status.battery_is_low() {
                    hue
                } else {
                    layout.bright
                },
                at(
                    transform,
                    (label_x + 3.0, layout.center_y - f64::from(lh) / 2.0),
                ),
                None,
                &layout.pct_style.clone().with_weight(TextWeight::Semibold),
            );
        }
    }

    /// Settings/volume/network/battery glyphs in the already-sized and
    /// already-painted tray pill from [`Self::draw_tray_pill_body`].
    fn draw_tray_icons(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        layout: &TrayLayout,
    ) {
        let colors = &theme.tokens.colors;
        let mut sx = self.tray_pill_rect.x0 + layout.tray_pad;
        if layout.slots == 0 {
            Self::draw_tray_settings_icon(scene, transform, layout, sx);
        }
        sx = self.draw_tray_notification_badge(scene, transform, theme, text_renderer, layout, sx);
        sx = Self::draw_tray_volume_icon(scene, transform, &self.status, layout, sx);
        sx = Self::draw_tray_network_icon(scene, transform, &self.status, layout, sx);
        Self::draw_tray_battery_icon(
            scene,
            transform,
            text_renderer,
            &self.status,
            layout,
            sx,
            colors,
        );
    }

    /// Active microphone/camera use, drawn leftward from `rx`. New use is
    /// announced as a named pill for four seconds, then settles into a
    /// quiet dot; hovering that exact dot expands the same verified app
    /// label again. Clicking opens Quick Settings, whose privacy action
    /// can disconnect the active `PipeWire` clients. Returns the x cursor
    /// position the avatar stack should grow leftward from.
    fn draw_media_indicator(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        rx: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        if self.status.media.is_empty() {
            self.media_indicator_rect = Rect::ZERO;
            return rx;
        }
        let mut labels: Vec<&str> = self.status.media.iter().map(|u| u.label.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        let has_mic = self
            .status
            .media
            .iter()
            .any(|u| u.kind == crate::chrome::MediaKind::Microphone);
        let has_camera = self
            .status
            .media
            .iter()
            .any(|u| u.kind == crate::chrome::MediaKind::Camera);
        let media_color = colors.semantic.success;
        let indicator_h = BAR_CONTROL_PX;
        if self.media_expanded {
            let suffix = if labels.len() > 1 {
                format!(" +{}", labels.len().saturating_sub(1))
            } else {
                String::new()
            };
            let label = format!("{}{suffix}", labels.first().copied().unwrap_or_default());
            let style = tokens.typography.style(TextRole::Caption);
            let (label_w, label_h) = text_renderer.measure_style(
                &label,
                None,
                &style.clone().with_weight(TextWeight::Semibold),
            );
            let icon_count = usize::from(has_mic).saturating_add(usize::from(has_camera));
            let width = 16.0 + small_count_to_f64(icon_count) * 15.0 + f64::from(label_w);
            self.media_indicator_rect = Rect::new(
                rx - width,
                center_y - indicator_h / 2.0,
                rx,
                center_y + indicator_h / 2.0,
            );
            let rr = RoundedRect::from_rect(
                self.media_indicator_rect,
                ui::chrome::ChromeRole::Selector.radius(theme, indicator_h),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                media_color.with_alpha_factor(0.18),
                None,
                &rr,
            );
            let mut ix = self.media_indicator_rect.x0 + 7.0;
            for icon in [
                has_mic.then_some(ui::icons::Icon::Mic),
                has_camera.then_some(ui::icons::Icon::Camera),
            ]
            .into_iter()
            .flatten()
            {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(ix, center_y - 6.0, ix + 12.0, center_y + 6.0),
                    icon,
                    media_color,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                ix += 15.0;
            }
            text_renderer.draw_style(
                scene,
                &label,
                colors.text_primary,
                at(transform, (ix + 2.0, center_y - f64::from(label_h) / 2.0)),
                None,
                &style.with_weight(TextWeight::Semibold),
            );
        } else {
            self.media_indicator_rect = Self::draw_media_glyphs(
                scene,
                transform,
                media_color,
                (has_mic, has_camera),
                (rx, center_y),
            );
        }
        self.media_indicator_rect.x0 - f64::from(tokens.spacing.s3)
    }

    /// The settled media indicator ending at `rx`: what is in use as its
    /// glyph, a microphone or a camera, since a bare dot never said what is
    /// live. Returns where it was drawn.
    fn draw_media_glyphs(
        scene: &mut Scene,
        transform: Affine,
        color: Color,
        (mic, camera): (bool, bool),
        (rx, center_y): (f64, f64),
    ) -> Rect {
        let icons: Vec<ui::icons::Icon> = [
            mic.then_some(ui::icons::Icon::Mic),
            camera.then_some(ui::icons::Icon::Camera),
        ]
        .into_iter()
        .flatten()
        .collect();
        let width = (small_count_to_f64(icons.len()) * 15.0 + 5.0).max(BAR_CONTROL_PX);
        let rect = Rect::new(
            rx - width,
            center_y - BAR_CONTROL_PX / 2.0,
            rx,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        let mut ix = rect.center().x - (small_count_to_f64(icons.len()) * 15.0 - 3.0) / 2.0;
        for icon in icons {
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(ix, center_y - 6.0, ix + 12.0, center_y + 6.0),
                icon,
                color,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            ix += 15.0;
        }
        rect
    }

    /// Up to 3 collaborator chips in reading order, then the overflow "+N"
    /// chip last, the whole stack ending at `stack_right_x`. Returns how
    /// many circles were drawn (chips + the overflow chip, if any).
    fn draw_avatar_chips(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        stack_right_x: f64,
    ) -> usize {
        let colors = &theme.tokens.colors;
        let slots = self
            .collaborator_initials
            .len()
            .saturating_add(usize::from(self.collaborator_overflow > 0));
        // Slot `k` of `slots`, left to right; each chip's ring cuts only the
        // edge of the one before it, never its initial.
        let slot_center = |k: usize| {
            Point::new(
                stack_right_x
                    - 10.0
                    - small_count_to_f64(slots.saturating_sub(1).saturating_sub(k)) * AVATAR_STEP,
                center_y,
            )
        };
        // Routed through `ui::Avatar` so the roster stays pixel-identical
        // to app rosters.
        for (i, initial) in self.collaborator_initials.iter().enumerate() {
            let center = slot_center(i);
            // 20 px chip; large enough that an initial remains legible at 1:1.
            let rect = Rect::new(
                center.x - 10.0,
                center.y - 10.0,
                center.x + 10.0,
                center.y + 10.0,
            );
            let fill = self
                .collaborator_colors
                .get(i)
                .copied()
                .unwrap_or(colors.intent.inverted);
            if self.collaborator_agents.get(i).copied().unwrap_or(false) {
                // Software is never drawn as a person: the shared agent mark,
                // seated on the same ring so the stack still separates.
                let ring = f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.overlay,
                    None,
                    &RoundedRect::from_rect(
                        rect.inflate(ring, ring),
                        f64::from(theme.tokens.radii.detail),
                    ),
                );
                ui::agent_identity::draw_agent_identity_mark(
                    scene,
                    transform,
                    rect,
                    colors.presence.of(fill),
                    theme,
                    ui::agent_identity::AgentIdentityStyle::Solid,
                );
                continue;
            }
            // The ring reads against the bar it's drawn on.
            ui::widget::AvatarFace::new(&initial.to_string(), "")
                .presence(fill)
                .ringed_on(colors.surfaces.overlay)
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                );
        }

        if self.collaborator_overflow > 0 {
            let center = slot_center(self.collaborator_initials.len());
            let rect = Rect::new(
                center.x - 10.0,
                center.y - 10.0,
                center.x + 10.0,
                center.y + 10.0,
            );
            ui::widget::AvatarFace::paint_overflow(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                self.collaborator_overflow,
                Some(colors.surfaces.overlay),
            );
        }

        slots
    }

    /// The person's own avatar, ending at `right_x`, in their colour with
    /// their initials; returns where the controls before it end. It opens
    /// their profile card, so it is set apart from the others' stack.
    fn draw_own_avatar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        let Some(own) = self.own_chip() else {
            self.own_avatar_rect = Rect::ZERO;
            return right_x;
        };
        let side = BAR_CONTROL_PX;
        let chip = Rect::new(
            right_x - side,
            center_y - side / 2.0,
            right_x,
            center_y + side / 2.0,
        );
        let (r, g, b) = own.rgb;
        ui::widget::AvatarFace::new(&own.name, "")
            .presence(Color::from_rgb8(r, g, b))
            .ringed_on(theme.tokens.colors.surfaces.overlay)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                chip,
            );
        self.own_avatar_rect = chip;
        chip.x0 - f64::from(theme.tokens.spacing.s3)
    }

    /// Paint an explicit, persistent disclosure beside the roster. An eye
    /// plus a count is retained even for view-only shares; control changes
    /// the colour and adds a pointer count instead of replacing visibility.
    fn draw_sharing_indicator(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        let (_, controllers, _) = self.sharing_summary();
        // A sharing label exists exactly when someone is viewing.
        let Some(label) = self.sharing_label() else {
            self.sharing_indicator_rect = Rect::ZERO;
            return right_x;
        };
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let color = if controllers > 0 {
            colors.semantic.warning
        } else {
            colors.intent.inverted
        };
        let style = tokens.typography.style(TextRole::Caption);
        let available = (right_x - self.center_lane.x1 - 48.0).clamp(28.0, 180.0);
        let label = text_renderer.elide_to_width_style(
            &label,
            ui::num::f32_of_f64(available),
            &style.clone().with_weight(TextWeight::Semibold),
        );
        let (label_w, label_h) = text_renderer.measure_style(
            &label,
            None,
            &style.clone().with_weight(TextWeight::Semibold),
        );
        let width = f64::from(label_w) + 31.0;
        self.sharing_indicator_rect = Rect::new(
            right_x - width,
            center_y - BAR_CONTROL_PX / 2.0,
            right_x,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        let pill = RoundedRect::from_rect(
            self.sharing_indicator_rect,
            ui::chrome::ChromeRole::Selector.radius(theme, BAR_CONTROL_PX),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            color.with_alpha_factor(0.18),
            None,
            &pill,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                self.sharing_indicator_rect.x0 + 7.0,
                center_y - 6.0,
                self.sharing_indicator_rect.x0 + 19.0,
                center_y + 6.0,
            ),
            ui::icons::Icon::Eye,
            color,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_primary,
            at(
                transform,
                (
                    self.sharing_indicator_rect.x0 + 23.0,
                    center_y - f64::from(label_h) / 2.0,
                ),
            ),
            None,
            &style.with_weight(TextWeight::Semibold),
        );
        self.sharing_indicator_rect.x0 - f64::from(tokens.spacing.s3)
    }

    /// A quiet "+N watching" label for spectators, right of the avatars,
    /// ending at `right_x`. Returns its pointer target (zero when nobody is
    /// watching): the bar's control height, like the avatar stack's.
    fn draw_watching_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> Rect {
        let watching = self.watching();
        if watching == 0 {
            return Rect::ZERO;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let label = t_args("topbar-watching-count", &[("count", watching.into())]);
        let style = tokens
            .typography
            .style(TextRole::Caption)
            .with_weight(TextWeight::Medium);
        let (label_w, label_h) = text_renderer.measure_style(&label, None, &style);
        let pad = f64::from(tokens.spacing.s2);
        // Plain dim text, no chip: spectators are a count, not people.
        let pill = Rect::new(
            right_x - f64::from(label_w) - pad * 2.0,
            center_y - BAR_CONTROL_PX / 2.0,
            right_x,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_dim,
            at(
                transform,
                (pill.x0 + pad, center_y - f64::from(label_h) / 2.0),
            ),
            None,
            &style,
        );
        Rect::new(
            pill.x0,
            center_y - BAR_CONTROL_PX / 2.0,
            pill.x1,
            center_y + BAR_CONTROL_PX / 2.0,
        )
    }

    /// Sets [`Self::avatar_stack_rect`] to either the drawn chip stack's
    /// bounds or the restored empty-room Invite control. `pos` is
    /// `(center_y, stack_right_x)`.
    fn draw_avatar_stack_bounds(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        pos: (f64, f64),
        slot_count: usize,
    ) {
        let (center_y, stack_right_x) = pos;
        if slot_count > 0 {
            // The stack ends at `stack_right_x`; each circle is 20 px wide,
            // one AVATAR_STEP from the next.
            let leftmost_cx = stack_right_x - (small_count_to_f64(slot_count) - 1.0) * AVATAR_STEP;
            let y_mid = center_y;
            self.avatar_stack_rect = Rect::new(
                leftmost_cx - 20.0,
                y_mid - BAR_CONTROL_PX / 2.0,
                stack_right_x,
                y_mid + BAR_CONTROL_PX / 2.0,
            );
            return;
        }
        // The first invite is exactly when there is no avatar to click. A
        // person-plus glyph in an outlined empty seat names the action
        // without pretending a participant is already present; the word
        // "Invite" is its accessible name, not a text button half the width
        // of the status cluster.
        let colors = &theme.tokens.colors;
        let seat = BAR_CONTROL_PX;
        let pill = Rect::new(
            stack_right_x + 2.0 - seat,
            center_y - seat / 2.0,
            stack_right_x + 2.0,
            center_y + seat / 2.0,
        );
        let chip = vello::kurbo::Circle::new(pill.center(), seat / 2.0 - 0.5);
        scene.stroke(
            &Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
            transform,
            ui::surface::edge_on(colors.surfaces.raised),
            None,
            &chip,
        );
        let glyph = SMALL_GLYPH_PX;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(pill.center(), (glyph, glyph)),
            ui::icons::Icon::UserPlus,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        self.avatar_stack_rect = pill;
    }

    /// The click half of [`Widget::handle_event`]'s `MouseInput`
    /// branch, split out to keep that function under the line budget.
    fn handle_left_click(&mut self, handled: &mut bool) {
        if self.activity_is_visible()
            && self
                .last_cursor
                .is_some_and(|p| self.center_rect.contains(p))
            && let Some(chrome) = self.chrome.as_ref()
        {
            let focus = chrome.snapshot().activity_glance.focus;
            chrome.intent(ChromeIntent::ShowActivity(focus));
            self.activity_started = None;
            *handled = true;
            return;
        }
        if let Some(p) = self.last_cursor {
            let target = if self.continuity_dismiss_rect.contains(p) {
                Some(Self::CONTROL_CONTINUITY_DISMISS)
            } else if self.continuity_rect.contains(p) {
                Some(Self::CONTROL_CONTINUITY)
            } else {
                None
            };
            if let Some(target) = target {
                *handled = self.activate_continuity_control(target);
                return;
            }
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // MouseInput has no position; rely on the last CursorMoved.
        let Some(p) = self.last_cursor else {
            return;
        };
        let intent = if self.tilo_mark.is_some() && self.tilo_mark_rect.contains(p) {
            ChromeIntent::ToggleTiloMenu
        } else if self.overview_btn_rect.contains(p) {
            ChromeIntent::ToggleOverview
        } else if self.up_btn_rect.contains(p) {
            ChromeIntent::SwitchWorkspace(-1)
        } else if self.down_btn_rect.contains(p) {
            ChromeIntent::SwitchWorkspace(1)
        } else if self.ws_pill_rect.contains(p) {
            if self.has_workspace_menu() {
                ChromeIntent::ToggleWorkspaceMenu
            } else {
                ChromeIntent::ToggleWorkspaceSettings
            }
        } else if self.own_avatar_rect.contains(p) {
            ChromeIntent::ToggleProfile
        } else if self.tray_hit_rect.contains(p) {
            ChromeIntent::ToggleQuickSettings
        } else if self.media_hit_rect().contains(p) {
            ChromeIntent::ShowQuickSettings
        } else if self.avatar_stack_rect.contains(p) || self.sharing_indicator_rect.contains(p) {
            ChromeIntent::ToggleCollaborators
        } else {
            return;
        };
        *handled = chrome.intent(intent);
    }

    /// Pulled at the top of [`Widget::draw`], not pushed by `update`, so a
    /// just-dropped battery percent is drawn and counted as a change in the
    /// same pass. Also tracks the media-use signature so a newly-started
    /// capture announces itself as a named pill before settling into a dot.
    fn refresh_status_and_media_state(&mut self) {
        self.status = self.live_status();
        if let Some(chrome) = &self.chrome {
            let snapshot = chrome.snapshot();
            self.telemetry = snapshot.telemetry;
            if let Some(widgets) = snapshot.topbar_widgets {
                self.widget_config = widgets;
            }
            if snapshot.clock_activity.is_none() && self.clock_overlay_rect != Rect::ZERO {
                self.clock_drag_offset = None;
                self.clock_touch_id = None;
                self.clock_control_focus = None;
                self.cache_fp = CacheFingerprint::default();
            }
        }
        self.notification_badge = self
            .chrome
            .as_ref()
            .map_or(0, |chrome| chrome.snapshot().notification_badge);
        if let Some(chrome) = self.chrome.as_ref() {
            let glance = chrome.snapshot().activity_glance;
            if glance.pulse != self.activity_pulse {
                self.activity_pulse = glance.pulse;
                self.activity_started =
                    (glance.unread > 0 && glance.headline.is_some()).then(Instant::now);
            }
            if glance.unread == 0 {
                self.activity_started = None;
            }
            self.activity_headline = glance.headline;
            self.activity_unread = glance.unread;
        }
        let signature: Vec<(u32, u8, String)> = self
            .status
            .media
            .iter()
            .map(|use_| {
                (
                    use_.client_id,
                    match use_.kind {
                        crate::chrome::MediaKind::Microphone => 0,
                        crate::chrome::MediaKind::Camera => 1,
                    },
                    use_.label.clone(),
                )
            })
            .collect();
        if signature != self.media_signature {
            self.media_signature = signature;
            self.media_started = (!self.media_signature.is_empty()).then(Instant::now);
        }
        let media_hovered = self
            .last_cursor
            .is_some_and(|p| self.media_hit_rect().contains(p));
        self.media_expanded = !self.media_signature.is_empty()
            && (media_hovered
                || self
                    .media_started
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(4)));
    }

    /// Sizes the center band, then dispatches to whichever center widget
    /// is currently selected. Split out of [`Widget::draw`] to stay under
    /// clippy's line budget — the three arms don't share state beyond the
    /// band's geometry, which none of them need after this call returns.
    fn draw_center_widget(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) {
        let (content_x0, content_x1, content_cx) =
            self.draw_center_band(scene, transform, theme, text_renderer, center_y);
        if self.activity_is_visible() {
            self.draw_activity_widget(scene, transform, theme, text_renderer, center_y, content_cx);
            return;
        }
        match self.center_widget {
            CenterWidget::Clock => {
                self.draw_clock_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            }
            CenterWidget::SystemMonitor => {
                self.draw_system_monitor_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    (content_x0, content_x1),
                );
            }
            CenterWidget::Calendar => {
                self.draw_calendar_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            }
        }
    }

    fn activity_is_visible(&self) -> bool {
        self.activity_headline.is_some()
            && self
                .activity_started
                .is_some_and(|started| started.elapsed() < Duration::from_secs(6))
    }

    fn draw_activity_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        center_x: f64,
    ) {
        let Some(_headline) = self.activity_headline.as_deref() else {
            return;
        };
        let style = theme.text(TextRole::Caption);
        // Keep activity in one line. `measure(..., Some(width))` wraps at
        // word boundaries, which made a large-text notification grow to two
        // lines inside the fixed-height selector and clip into the bar edge.
        // The dots reserve the first three pitches of the band; the trailing
        // inset reserves the right breathing room. Elision preserves that
        // geometry at every text scale and viewport width.
        let max_width = self.activity_content_width(theme);
        let label = self.activity_display_label(text_renderer, &style, max_width);
        let (w, h) = text_renderer.measure_style(
            &label,
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
        text_renderer.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            at(
                transform,
                (center_x - f64::from(w) / 2.0, center_y - f64::from(h) / 2.0),
            ),
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
    }

    /// Width of the centre lane the activity headline may occupy — the
    /// band minus its insets, exactly as
    /// [`Self::draw_center_band`] lays it out.
    fn activity_content_width(&self, theme: &Theme) -> f32 {
        let (x0, x1) = Self::center_content_lane(self.center_rect);
        ui::num::f32_of_f64((x1 - x0).max(f64::from(theme.tokens.spacing.s12)))
    }

    fn activity_display_label(
        &self,
        text_renderer: &mut TextRenderer,
        style: &TextStyle,
        max_width: f32,
    ) -> String {
        let Some(headline) = self.activity_headline.as_deref() else {
            return String::new();
        };
        let full = if self.activity_unread == 0 {
            headline.to_string()
        } else {
            t_args(
                "topbar-activity-unread",
                &[
                    ("headline", headline.into()),
                    ("count", self.activity_unread.into()),
                ],
            )
        };
        let (full_width, _) = text_renderer.measure_style(
            &full,
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
        if full_width <= max_width {
            return full;
        }

        // The primary activity says what happened; the unread count already
        // has a numeric badge, and clicking this pill opens the focused
        // Room Activity detail. Prefer the entire primary sentence plus an
        // explicit detail chevron over eliding its last words to preserve
        // the trailing count in a compact top bar.
        let detail = format!("{headline}  ›");
        let (detail_width, _) = text_renderer.measure_style(
            &detail,
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
        if detail_width <= max_width {
            return detail;
        }
        // Aggregate activity sometimes lists its examples after a colon
        // (for example, the workspace names just recorded). Keep the
        // complete event/count clause and make the detail route explicit
        // instead of cutting it part-way through an example list.
        if let Some((summary, _examples)) = headline.split_once(':') {
            let compact = format!("{summary}  ›");
            let (compact_width, _) = text_renderer.measure_style(
                &compact,
                None,
                &style.clone().with_weight(TextWeight::Medium),
            );
            if compact_width <= max_width {
                return compact;
            }
        }
        text_renderer.elide_to_width_style(
            headline,
            max_width,
            &style.clone().with_weight(TextWeight::Medium),
        )
    }

    fn draw_activity_badge(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        if self.activity_unread == 0 || self.avatar_stack_rect.is_zero_area() {
            return;
        }
        let label = self.activity_unread.min(99).to_string();
        let radius = 8.0;
        let center = Point::new(
            self.avatar_stack_rect.x1 - 2.0,
            self.avatar_stack_rect.y0 + 2.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.inverted,
            None,
            &vello::kurbo::Circle::new(center, radius),
        );
        let style = theme.text(TextRole::Caption);
        let (w, h) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            at(
                transform,
                (center.x - f64::from(w) / 2.0, center.y - f64::from(h) / 2.0),
            ),
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
    }

    /// Keyboard focus is shown; focus a click gave the bar is not.
    fn focus_ring_visible(&self) -> bool {
        self.focused && self.focus_origin == FocusOrigin::Keyboard
    }

    fn draw_focus_indicator(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.focus_ring_visible() {
            return;
        }
        let Some(target) = self.focused_control.as_deref() else {
            return;
        };
        let rect = self.control_rect(target);
        if rect.is_zero_area() {
            return;
        }
        let ring = RoundedRect::from_rect(
            rect.inset(-2.0),
            ui::chrome::ChromeRole::Selector.radius(theme, rect.height() + 4.0),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            theme.border_focus,
            None,
            &ring,
        );
    }

    fn activate_control(&mut self, target: &str) -> bool {
        if self.control_is_disabled(target) {
            return false;
        }
        if matches!(
            target,
            Self::CONTROL_CONTINUITY | Self::CONTROL_CONTINUITY_DISMISS
        ) {
            return self.activate_continuity_control(target);
        }
        if Self::editor_widget(target).is_some() {
            return self.activate_editor_control(target);
        }
        match target {
            Self::CONTROL_EDIT_WIDGETS | Self::CONTROL_WIDGET_EDITOR => self.open_widget_editor(),
            Self::CONTROL_CENTER_CLOCK
            | Self::CONTROL_CENTER_MONITOR
            | Self::CONTROL_CENTER_CALENDAR => {
                self.center_widget = match target {
                    Self::CONTROL_CENTER_CLOCK => CenterWidget::Clock,
                    Self::CONTROL_CENTER_MONITOR => CenterWidget::SystemMonitor,
                    _ => CenterWidget::Calendar,
                };
                true
            }
            Self::CONTROL_TILO_MARK
            | Self::CONTROL_OVERVIEW
            | Self::CONTROL_WORKSPACE_UP
            | Self::CONTROL_WORKSPACE_DOWN
            | Self::CONTROL_WORKSPACE_SETTINGS
            | Self::CONTROL_PROFILE
            | Self::CONTROL_QUICK_SETTINGS
            | Self::CONTROL_COLLABORATORS
            | Self::CONTROL_MEDIA => {
                let Some(chrome) = self.chrome.clone() else {
                    return false;
                };
                let intent = match target {
                    Self::CONTROL_TILO_MARK if self.tilo_mark.is_some() => {
                        ChromeIntent::ToggleTiloMenu
                    }
                    Self::CONTROL_OVERVIEW => ChromeIntent::ToggleOverview,
                    Self::CONTROL_WORKSPACE_UP => ChromeIntent::SwitchWorkspace(-1),
                    Self::CONTROL_WORKSPACE_DOWN => ChromeIntent::SwitchWorkspace(1),
                    Self::CONTROL_WORKSPACE_SETTINGS if self.has_workspace_menu() => {
                        ChromeIntent::ToggleWorkspaceMenu
                    }
                    Self::CONTROL_WORKSPACE_SETTINGS => ChromeIntent::ToggleWorkspaceSettings,
                    Self::CONTROL_PROFILE => ChromeIntent::ToggleProfile,
                    Self::CONTROL_QUICK_SETTINGS | Self::CONTROL_MEDIA => {
                        ChromeIntent::ToggleQuickSettings
                    }
                    Self::CONTROL_COLLABORATORS => ChromeIntent::ToggleCollaborators,
                    _ => return false,
                };
                chrome.intent(intent)
            }
            _ => false,
        }
    }

    /// Accepting focuses the offered tab here; dismissing hides the offer
    /// until that device's focus moves again. Neither happens on its own.
    fn activate_continuity_control(&self, target: &str) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        chrome.intent(if target == Self::CONTROL_CONTINUITY {
            ChromeIntent::AcceptContinuity
        } else {
            ChromeIntent::DismissContinuity
        })
    }

    /// Running applications' status-notifier icons, drawn leftward from
    /// `rx` beside the system tray pill. `Passive` items stay hidden, as the
    /// protocol asks; one that needs attention carries a warning dot.
    /// Returns where the next cluster may end.
    fn draw_app_tray(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        rx: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let items: Vec<AppTrayItem> = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.app_tray())
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.status != AppTrayStatus::Passive)
            .collect();
        self.app_tray.clear();
        self.app_tray_images
            .retain(|key, _| items.iter().any(|item| &item.key == key));
        if items.is_empty() {
            return rx;
        }
        let slot = f64::from(tokens.spacing.s6);
        let gap = f64::from(tokens.spacing.s1);
        let icon_px = 18.0_f64;
        let mut right = rx;
        for item in items.iter().rev() {
            let hit = Rect::new(
                right - slot,
                center_y - BAR_CONTROL_PX / 2.0,
                right,
                center_y + BAR_CONTROL_PX / 2.0,
            );
            let icon_rect = Rect::from_center_size(hit.center(), (icon_px, icon_px));
            self.draw_app_tray_icon(scene, transform, theme, text_renderer, item, icon_rect);
            if item.status == AppTrayStatus::NeedsAttention {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.semantic.warning,
                    None,
                    &vello::kurbo::Circle::new((icon_rect.x1, icon_rect.y0 + 2.0), 3.0),
                );
            }
            self.app_tray.push(AppTraySlot {
                key: item.key.clone(),
                id: item.id.clone(),
                item_is_menu: item.item_is_menu,
                rect: hit,
            });
            right = hit.x0 - gap;
        }
        right - f64::from(tokens.spacing.s3) + gap
    }

    /// One tray item's image, or its letter badge, in `icon_rect`.
    fn draw_app_tray_icon(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        item: &AppTrayItem,
        icon_rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let icon_px = icon_rect.width();
        match &item.icon {
            AppTrayIcon::Pixels {
                width,
                height,
                rgba,
            } => {
                let identity = std::sync::Arc::as_ptr(rgba) as usize;
                let image = match self.app_tray_images.get(&item.key) {
                    Some((cached, image)) if *cached == identity => image.clone(),
                    _ => {
                        let image = ui::raster::rgba8(
                            vello::peniko::Blob::new(rgba.clone()),
                            *width,
                            *height,
                        );
                        self.app_tray_images
                            .insert(item.key.clone(), (identity, image.clone()));
                        image
                    }
                };
                let sx = icon_px / f64::from((*width).max(1));
                let sy = icon_px / f64::from((*height).max(1));
                scene.fill(
                    Fill::NonZero,
                    transform
                        .pre_translate((icon_rect.x0, icon_rect.y0).into())
                        .pre_scale_non_uniform(sx, sy),
                    &vello::peniko::ImageBrush::new(image),
                    None,
                    &Rect::new(0.0, 0.0, f64::from(*width), f64::from(*height)),
                );
            }
            AppTrayIcon::Letter(letter) => {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.raised,
                    None,
                    &vello::kurbo::Circle::new(icon_rect.center(), icon_px / 2.0),
                );
                let label = letter.to_string();
                let style = tokens.typography.style(TextRole::Caption);
                let (w, h) = text_renderer.measure_style(
                    &label,
                    None,
                    &style.clone().with_weight(TextWeight::Semibold),
                );
                text_renderer.draw_style(
                    scene,
                    &label,
                    colors.text_primary,
                    at(
                        transform,
                        (
                            icon_rect.center().x - f64::from(w) / 2.0,
                            icon_rect.center().y - f64::from(h) / 2.0,
                        ),
                    ),
                    None,
                    &style.with_weight(TextWeight::Semibold),
                );
            }
        }
    }

    /// A press on an application tray icon: the primary button activates
    /// the item (or asks for its menu when it has no primary action), the
    /// secondary button asks for its menu, the middle one runs its
    /// secondary action.
    fn handle_app_tray_press(&self, event: &WindowEvent, handled: &mut bool) -> bool {
        let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button,
            ..
        } = event
        else {
            return false;
        };
        let Some(point) = self.last_cursor else {
            return false;
        };
        let Some(slot) = self.app_tray.iter().find(|slot| slot.rect.contains(point)) else {
            return false;
        };
        let (x, y) = pointer_coordinates(point);
        let request = match button {
            winit::event::MouseButton::Left if slot.item_is_menu => {
                AppTrayRequest::ContextMenu { x, y }
            }
            winit::event::MouseButton::Left => AppTrayRequest::Activate { x, y },
            winit::event::MouseButton::Right => AppTrayRequest::ContextMenu { x, y },
            winit::event::MouseButton::Middle => AppTrayRequest::SecondaryActivate { x, y },
            _ => return false,
        };
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.intent(ChromeIntent::AppTray {
                key: slot.key.clone(),
                request,
            });
        }
        *handled = true;
        true
    }

    /// A quiet offer to continue the tab another of this person's devices
    /// just moved to, left of the roster, with its own dismiss chip. It
    /// yields to the centre band: with no room for a readable label it is
    /// not drawn at all (the palette still offers it).
    fn draw_continuity_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        label: Option<&str>,
    ) {
        use ui::widget::{ChipFace, ChipLead};

        self.continuity_rect = Rect::ZERO;
        self.continuity_dismiss_rect = Rect::ZERO;
        let Some(label) = label else {
            return;
        };
        let gap = f64::from(theme.tokens.spacing.s2);
        let height = 24.0;
        let right = self.avatar_stack_rect.x0 - f64::from(theme.tokens.spacing.s3);
        let left_limit = self.center_lane.x1 + f64::from(theme.tokens.spacing.s4);
        let dismiss_w = height;
        let lead = ChipLead::Glyph(ui::icons::Icon::Monitor);
        let natural = ChipFace::intrinsic_width(label, lead, false, text_renderer, theme);
        let room = right - dismiss_w - gap - left_limit;
        // Below this the label elides to a word or two and stops saying
        // what it offers.
        if room < 96.0 {
            return;
        }
        let width = natural.min(room);
        let top = center_y - height / 2.0;
        self.continuity_dismiss_rect = Rect::new(right - dismiss_w, top, right, top + height);
        let chip_right = self.continuity_dismiss_rect.x0 - gap;
        self.continuity_rect = Rect::new(chip_right - width, top, chip_right, top + height);
        ChipFace::new(self.continuity_rect, label).lead(lead).paint(
            scene,
            transform,
            text_renderer,
            theme,
        );
        ChipFace::new(self.continuity_dismiss_rect, "")
            .lead(ChipLead::Glyph(ui::icons::Icon::X))
            .paint(scene, transform, text_renderer, theme);
    }

    fn handle_keyboard(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        use winit::keyboard::{Key, NamedKey};
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return;
        };
        if !self.focused || key.state != winit::event::ElementState::Pressed {
            return;
        }
        let submit = matches!(
            &key.logical_key,
            Key::Named(NamedKey::Enter | NamedKey::Space)
        );
        if submit && (self.editor_submit_key.is_some() || self.widget_editing && key.repeat) {
            *handled = true;
            return;
        }
        let mods = modifiers.state();
        if self.widget_editing && (mods.control_key() || mods.alt_key() || mods.super_key()) {
            return;
        }
        let controls = if self.widget_editing {
            self.editor_controls()
        } else {
            self.control_targets()
        };
        if controls.is_empty() {
            return;
        }
        let current = self
            .focused_control
            .as_deref()
            .and_then(|target| controls.iter().position(|candidate| *candidate == target))
            .unwrap_or(0);
        let focus = match &key.logical_key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp)
                if self.widget_editing || key.logical_key == Key::Named(NamedKey::ArrowLeft) =>
            {
                Some(
                    current
                        .checked_sub(1)
                        .unwrap_or_else(|| controls.len().saturating_sub(1)),
                )
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown)
                if self.widget_editing || key.logical_key == Key::Named(NamedKey::ArrowRight) =>
            {
                Some(
                    current
                        .checked_add(1)
                        .filter(|&index| index < controls.len())
                        .unwrap_or(0),
                )
            }
            Key::Named(NamedKey::Home) => Some(0),
            Key::Named(NamedKey::End) => Some(controls.len().saturating_sub(1)),
            _ => None,
        };
        if let Some(target) = focus.and_then(|index| controls.get(index)) {
            self.focused_control = Some((*target).into());
            *handled = true;
            return;
        }
        match &key.logical_key {
            Key::Named(NamedKey::ContextMenu) => {
                *handled = self.open_widget_editor();
            }
            Key::Named(NamedKey::F10) if mods.shift_key() => {
                *handled = self.open_widget_editor();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(target) = controls.get(current) {
                    if self.widget_editing
                        && let Key::Named(named) = &key.logical_key
                    {
                        self.editor_submit_key = Some(*named);
                    }
                    *handled = self.activate_control(target);
                }
            }
            _ => {}
        }
    }

    fn draw_clock_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        activity: &ClockActivity,
    ) {
        let (width, height) = (clock_overlay_width(self.rect.x1), CLOCK_OVERLAY_HEIGHT);
        let fallback = Point::new((self.rect.x1 - width - 16.0).max(8.0), self.rect.y1 + 16.0);
        let clamped = clamp_clock_overlay(
            self.clock_overlay_pos.unwrap_or(fallback),
            (width, height),
            self.clock_viewport(),
            self.rect.y1,
        );
        let panel = Rect::from_origin_size(clamped, (width, height));
        self.clock_overlay_pos = Some(clamped);
        self.clock_overlay_rect = panel;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(panel, 18.0),
        );
        let grip = Line::new(
            Point::new(panel.center().x - 14.0, panel.y0 + 9.0),
            Point::new(panel.center().x + 14.0, panel.y0 + 9.0),
        );
        scene.stroke(
            &Stroke::new(3.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &grip,
        );
        text_renderer.draw_style(
            scene,
            &activity.label,
            theme.tokens.colors.text_dim,
            at(transform, (panel.x0 + 18.0, panel.y0 + 21.0)),
            None,
            &theme.text(TextRole::Label),
        );
        let elapsed = format!(
            "{:02}:{:02}",
            activity.remaining_secs / 60,
            activity.remaining_secs % 60
        );
        text_renderer.draw_style(
            scene,
            &elapsed,
            theme.tokens.colors.text_primary,
            at(transform, (panel.x0 + 18.0, panel.y0 + 43.0)),
            None,
            &theme.text(TextRole::Heading).with_weight(TextWeight::Bold),
        );
        [
            self.clock_pause_rect,
            self.clock_add_rect,
            self.clock_close_rect,
        ] = clock_overlay_controls(panel);
        let [pause_label, add_label, close_label] = clock_overlay_labels(activity);
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(self.clock_pause_rect, &pause_label)
                .role(ButtonRole::Primary)
                .icon(if activity.paused {
                    ui::icons::Icon::Play
                } else {
                    ui::icons::Icon::Pause
                })
                .focused(self.clock_control_focus == Some(0)),
        );
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(self.clock_add_rect, &add_label)
                .focused(self.clock_control_focus == Some(1)),
        );
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(self.clock_close_rect, "")
                .role(ButtonRole::Plain)
                .icon(ui::icons::Icon::X)
                .accessible_name(&close_label)
                .focused(self.clock_control_focus == Some(2)),
        );
    }

    fn handle_clock_keyboard(
        &mut self,
        logical_key: &winit::keyboard::Key,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        match logical_key {
            Key::Named(NamedKey::Escape) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.intent(ChromeIntent::HideClock);
                }
                self.publish_clock_activity_command("close", "close-stopwatch");
            }
            Key::Named(NamedKey::Tab) => {
                self.clock_control_focus = Some(if modifiers.state().shift_key() {
                    match self.clock_control_focus {
                        None | Some(0 | 3..) => 2,
                        Some(1) => 0,
                        Some(2) => 1,
                    }
                } else {
                    match self.clock_control_focus {
                        None | Some(2..) => 0,
                        Some(0) => 1,
                        Some(1) => 2,
                    }
                });
                self.cache_fp = CacheFingerprint::default();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let Some(control) = self.clock_control_focus else {
                    return false;
                };
                self.activate_clock_control(control);
            }
            Key::Named(
                key @ (NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown),
            ) if modifiers.state().alt_key() => {
                let step = if modifiers.state().shift_key() {
                    32.0
                } else {
                    8.0
                };
                let mut position = self.clock_overlay_pos.unwrap_or_else(|| {
                    Point::new(
                        (self.rect.x1 - clock_overlay_width(self.rect.x1) - 16.0).max(8.0),
                        self.rect.y1 + 16.0,
                    )
                });
                match key {
                    NamedKey::ArrowLeft => position.x -= step,
                    NamedKey::ArrowRight => position.x += step,
                    NamedKey::ArrowUp => position.y -= step,
                    NamedKey::ArrowDown => position.y += step,
                    _ => {}
                }
                self.clock_overlay_pos = Some(clamp_clock_overlay(
                    position,
                    (clock_overlay_width(self.rect.x1), CLOCK_OVERLAY_HEIGHT),
                    self.clock_viewport(),
                    self.rect.y1,
                ));
                self.cache_fp = CacheFingerprint::default();
            }
            _ => return false,
        }
        true
    }

    fn handle_clock_touch(&mut self, touch: &winit::event::Touch, scale_factor: f64) -> bool {
        let point = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            winit::event::TouchPhase::Started => {
                if self.clock_touch_id.is_none()
                    && self.clock_touch_control.is_none()
                    && let Some(control) = self.clock_control_at(point)
                {
                    self.clock_touch_control = Some((touch.id, control));
                    self.clock_control_focus = Some(control);
                    self.cache_fp = CacheFingerprint::default();
                    return true;
                }
                if self.clock_touch_id.is_none()
                    && self.clock_touch_control.is_none()
                    && self.begin_clock_drag_at(point)
                {
                    self.clock_touch_id = Some(touch.id);
                    return true;
                }
            }
            winit::event::TouchPhase::Moved
                if self
                    .clock_touch_control
                    .is_some_and(|(id, _)| id == touch.id) =>
            {
                // Keep ownership so lifting elsewhere cancels the press;
                // never turn a control gesture into an accidental drag.
                return true;
            }
            winit::event::TouchPhase::Moved if self.clock_touch_id == Some(touch.id) => {
                return self.move_clock_drag_to(point);
            }
            winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                if self
                    .clock_touch_control
                    .is_some_and(|(id, _)| id == touch.id) =>
            {
                let pending = self.clock_touch_control.take();
                if let Some(control) = released_clock_touch_control(
                    pending,
                    touch.id,
                    touch.phase == winit::event::TouchPhase::Ended,
                    self.clock_control_at(point),
                ) {
                    self.activate_clock_control(control);
                }
                return true;
            }
            winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                if self.clock_touch_id == Some(touch.id) =>
            {
                self.clock_touch_id = None;
                self.clock_drag_offset = None;
                return true;
            }
            _ => {}
        }
        false
    }

    fn clock_accessibility_node(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let chrome = self.chrome.as_ref()?;
        let snapshot = chrome.snapshot();
        if !snapshot.overlays.clock {
            return None;
        }
        let activity = snapshot.clock_activity.as_ref()?;
        let stopwatch = activity.stopwatch;
        let paused = activity.paused;
        let minutes = activity.remaining_secs / 60;
        let seconds = activity.remaining_secs % 60;
        let reading = format!("{minutes:02}:{seconds:02}");
        Some(
            AccessNode::new(Role::Group)
                .with_name(t(if stopwatch {
                    "topbar-clock-floating-stopwatch"
                } else {
                    "topbar-clock-floating-timer"
                }))
                .with_description(t("topbar-clock-floating-description"))
                .with_bounds(self.clock_overlay_rect)
                .with_children(vec![
                    AccessNode::new(Role::StaticText)
                        .with_name(if activity.label.is_empty() {
                            t(if stopwatch {
                                "topbar-clock-stopwatch"
                            } else {
                                "topbar-clock-timer"
                            })
                        } else {
                            activity.label.clone()
                        })
                        .with_value(&reading)
                        .with_description(t(if paused {
                            "topbar-clock-paused"
                        } else {
                            "topbar-clock-running"
                        })),
                    AccessNode::new(Role::Button)
                        .with_name(t(if paused {
                            "topbar-clock-resume"
                        } else {
                            "topbar-clock-pause"
                        }))
                        .with_action_target("clock.overlay.toggle")
                        .with_bounds(self.clock_pause_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(0)),
                        ),
                    AccessNode::new(Role::Button)
                        .with_name(t(if stopwatch {
                            "topbar-clock-record-lap"
                        } else {
                            "topbar-clock-add-one-minute"
                        }))
                        .with_action_target("clock.overlay.add-minute")
                        .with_bounds(self.clock_add_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(1)),
                        ),
                    AccessNode::new(Role::Button)
                        .with_name(t(if stopwatch {
                            "topbar-clock-close-stopwatch"
                        } else {
                            "topbar-clock-close-timer"
                        }))
                        .with_action_target("clock.overlay.close")
                        .with_bounds(self.clock_close_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(2)),
                        ),
                ]),
        )
    }

    fn handle_widget_or_clock_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        if matches!(event, WindowEvent::Focused(false)) {
            self.set_focus(false);
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Released
            && self
                .editor_submit_key
                .is_some_and(|named| key.logical_key == winit::keyboard::Key::Named(named))
        {
            self.editor_submit_key = None;
            *handled = true;
            return true;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
            && key.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            && self.widget_editing
        {
            self.close_widget_editor();
            *handled = true;
            return true;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.snapshot().overlays.clock)
        {
            *handled = self.handle_clock_keyboard(&key.logical_key, modifiers);
            return true;
        }
        if let WindowEvent::Touch(touch) = event
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.snapshot().overlays.clock)
        {
            *handled = self.handle_clock_touch(touch, scale_factor);
            return true;
        }
        false
    }

    fn handle_center_pointer_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            let point = Point::new(position.x / scale_factor, position.y / scale_factor);
            let previous = self.last_cursor;
            if self.widget_editing
                && self.editor_controls().into_iter().any(|target| {
                    let rect = self.control_rect(target);
                    previous.is_some_and(|old| rect.contains(old)) != rect.contains(point)
                })
            {
                self.cache_fp = CacheFingerprint::default();
            }
            self.last_cursor = Some(point);
            if self.widget_edit_drag.is_pending() {
                *handled = self
                    .widget_edit_drag
                    .pointer_moved(point, &self.editor_metrics());
                self.cache_fp = CacheFingerprint::default();
                return true;
            }
            if self.clock_touch_id.is_none() && self.move_clock_drag_to(point) {
                *handled = true;
            }
            return true;
        }
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        let Some(point) = self.last_cursor else {
            return true;
        };
        if !self.center_rect.contains(point) {
            return true;
        }
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            winit::event::MouseScrollDelta::PixelDelta(position) => position.y / 40.0,
        };
        self.center_scroll_accum += dy;
        for _ in 0..16 {
            if self.center_scroll_accum < 1.0 {
                break;
            }
            self.cycle_center_widget(true);
            self.center_scroll_accum -= 1.0;
        }
        for _ in 0..16 {
            if self.center_scroll_accum > -1.0 {
                break;
            }
            self.cycle_center_widget(false);
            self.center_scroll_accum += 1.0;
        }
        *handled = true;
        true
    }

    fn handle_left_release(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            return false;
        }
        *handled |= self.editor_pointer_release.take() == Some(winit::event::MouseButton::Left);
        if let Some(target) = self.editor_pressed_control.take() {
            if self
                .last_cursor
                .is_some_and(|point| self.control_rect(target).contains(point))
            {
                let _ = self.activate_editor_control(target);
            }
            self.cache_fp = CacheFingerprint::default();
            *handled = true;
        }
        if let Some((from, to)) = self.widget_edit_drag.commit()
            && from < self.widget_config.widgets.len()
            && to < self.widget_config.widgets.len()
        {
            let widget = self.widget_config.widgets.remove(from);
            self.widget_config.widgets.insert(to, widget);
            self.save_widget_config();
            self.cache_fp = CacheFingerprint::default();
            *handled = true;
        }
        self.widget_edit_pressed = None;
        if self.clock_touch_id.is_none() && self.clock_drag_offset.take().is_some() {
            *handled = true;
        }
        true
    }

    fn handle_widget_editor_press(&mut self, point: Point, handled: &mut bool) -> bool {
        if !self.widget_editing || !self.widget_editor_rect.contains(point) {
            return false;
        }
        for index in 0..self.widget_config.widgets.len() {
            let row = self.widget_editor_row(index);
            if !row.contains(point) {
                continue;
            }
            let Some(widget) = self.widget_config.widgets.get(index).copied() else {
                continue;
            };
            if self.widget_editor_remove_rect(index).contains(point) {
                if self.widget_config.widgets.len() > 1 {
                    let target = Self::editor_control(widget, true);
                    self.editor_pressed_control = Some(target);
                    self.editor_pointer_release = Some(winit::event::MouseButton::Left);
                    self.focused_control = Some(target.into());
                }
            } else {
                self.widget_edit_drag
                    .begin(widget, index, point.y - row.y0, point);
                self.widget_edit_pressed = Some(index);
                if self.widget_config.widgets.len() > 1 {
                    self.focused_control = Some(Self::editor_control(widget, true).into());
                }
                self.editor_pointer_release = Some(winit::event::MouseButton::Left);
            }
            self.cache_fp = CacheFingerprint::default();
            *handled = true;
            return true;
        }
        for widget in TopBarWidget::ALL {
            if !self.widget_config.widgets.contains(&widget)
                && self.widget_editor_add_rect(widget).contains(point)
            {
                let target = Self::editor_control(widget, false);
                self.editor_pressed_control = Some(target);
                self.editor_pointer_release = Some(winit::event::MouseButton::Left);
                self.focused_control = Some(target.into());
                self.cache_fp = CacheFingerprint::default();
                *handled = true;
                return true;
            }
        }
        *handled = true;
        true
    }

    fn handle_left_press(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) || self.clock_touch_id.is_some()
        {
            return;
        }
        let Some(point) = self.last_cursor else {
            self.handle_left_click(handled);
            return;
        };
        if self.handle_widget_editor_press(point, handled) {
            return;
        }
        if self.widget_editing {
            self.close_widget_editor();
            *handled = true;
            return;
        }
        if self.center_rect.contains(point) && modifiers.state().control_key() {
            *handled = self.open_widget_editor();
            return;
        }
        if self.activate_clock_control_at(point) || self.begin_clock_drag_at(point) {
            *handled = true;
            return;
        }
        if self.strip_press(point) {
            *handled = true;
            return;
        }
        self.handle_left_click(handled);
    }
}

impl Widget for TopBarChrome {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let responsive_changed = self.consume_responsive(state, path);
        // The time is read when drawing, so nothing else marks the bar
        // dirty when the minute turns over on an idle desktop.
        let time_changed = self.take_time_wake();
        if let Some(chrome) = self.chrome.clone() {
            let rev = chrome.snapshot().revision;
            if rev != self.synced_revision {
                // Clone the tree to avoid re-borrowing chrome while
                // `sync_from_tree` borrows &self mut.
                let tree_snap = chrome.tree();
                self.sync_from_tree(&tree_snap);
                // The status poller bumps the same revision.
                self.status = chrome.snapshot().status;
                self.synced_revision = rev;
                return true;
            }
        }
        time_changed || responsive_changed
    }

    /// The end of the time reading on screen. Only a future instant: once
    /// it passes, `update` reports the bar dirty and schedules the next one.
    fn next_wake_deadline(&self) -> Option<Instant> {
        self.time_wake.filter(|at| Instant::now() < *at)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A client-requested fullscreen owns the whole output. User maximize
        // still fills the workspace, but keeps the OS-owned top bar available
        // for navigation and session controls.
        self.responsive.theme = theme.clone();
        let fullscreen = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.snapshot().fullscreen);
        // A bar the person hides stands over the column rather than in it:
        // the windows take its strip, and showing it later moves nothing.
        let overlays = !fullscreen && self.presence().overlays();
        self.laid_out = BarStanding::of(overlays);
        let style = Style {
            display: if fullscreen {
                Display::None
            } else {
                Display::Flex
            },
            position: if overlays {
                taffy::style::Position::Absolute
            } else {
                taffy::style::Position::Relative
            },
            inset: if overlays {
                taffy::Rect {
                    left: taffy::LengthPercentageAuto::length(0.0),
                    right: taffy::LengthPercentageAuto::auto(),
                    top: taffy::LengthPercentageAuto::length(0.0),
                    bottom: taffy::LengthPercentageAuto::auto(),
                }
            } else {
                taffy::Rect::auto()
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: if fullscreen {
                    Dimension::length(0.0)
                } else {
                    Dimension::length(topbar_height(theme))
                },
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        self.responsive.configure(self.responsive_specs());
        let toolbar_node = self.responsive.toolbar.build_layout(taffy, theme, node_map);
        if let Ok(mut toolbar_style) = taffy.style(toolbar_node).cloned() {
            toolbar_style.position = taffy::style::Position::Absolute;
            if let Err(error) = taffy.set_style(toolbar_node, toolbar_style) {
                tracing::warn!(%error, "topbar: toolbar placement failed");
            }
        }
        let node = match taffy.new_with_children(style, &[toolbar_node]) {
            Ok(node) => node,
            Err(err) => {
                // The only child is the toolbar node allocated above. Keep
                // the session alive if a future Taffy change rejects this
                // fresh composition (production uses panic = "abort").
                tracing::error!("topbar: leaf allocation failed: {err}");
                NodeId::from(0u64)
            }
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            // A missing layout means this node fell out of the tree
            // between `build_layout` and this pass; keep the last known
            // rect rather than crash the session over a stale node.
            tracing::warn!("topbar: layout lookup failed for node {node:?}");
            return;
        };
        let previous = self.rect;
        self.rect = ui::widget::placed(l, origin);
        if let Some(at) = self.placement {
            self.rect = Rect::new(at.x0, at.y0, at.x1, at.y0 + self.rect.height());
        }
        if self.rect.is_zero_area() {
            self.close_widget_editor();
        } else if previous != self.rect {
            self.cancel_editor_drag();
        }

        let theme = self.responsive.theme.clone();
        ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            self.prepare_responsive(&theme, text);
        });
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Zero-height (fullscreen) paints nothing; without this guard
        // the encode below still runs at intrinsic size, overflowing
        // into a stray fragment over the fullscreen window.
        if self.rect.height() <= 0.0 || self.rect.width() <= 0.0 {
            return;
        }
        // A hidden bar paints nothing until it is reached for.
        if self.presence() == TopBarPresence::Hidden {
            self.cache_fp.presence = TopBarPresence::Hidden;
            return;
        }
        // Pulled here, not pushed by update, so a just-dropped battery
        // percent is drawn and counted as a change in the same pass.
        self.refresh_status_and_media_state();
        if !self.is_compact()
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.take_topbar_editor_request())
        {
            self.open_widget_editor();
        }
        self.schedule_time_wake();
        let fp = self.current_fingerprint();
        if fp == self.cache_fp
            && self.cache_theme.as_ref() == Some(theme)
            && fp.rect_px.2 > 0
            && !self.responsive.dirty
        {
            out_scene.append(&self.cached, Some(out_transform));
            return;
        }

        // Theme typography already includes accessibility scale.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);

        // Cache miss — encode at IDENTITY, then splice with `out_transform`.
        let mut local = std::mem::take(&mut self.cached);
        local.reset();
        let scene = &mut local;
        let transform = Affine::IDENTITY;
        let center_y = f64::midpoint(self.rect.y0, self.rect.y1);
        if self.presence() == TopBarPresence::Shown {
            self.draw_overlay_surface(scene, transform, theme);
        }

        if self.use_responsive(theme) {
            self.draw_responsive(scene, text_renderer, theme, presence);
        } else {
            let after_chevrons = self.draw_workspace_nav(scene, transform, theme, center_y);
            let after_chevrons = self.draw_host_pill(
                scene,
                transform,
                theme,
                text_renderer,
                center_y,
                after_chevrons,
            );
            self.draw_workspace_pill(
                scene,
                transform,
                theme,
                text_renderer,
                center_y,
                after_chevrons,
            );
            let center_left =
                f64::midpoint(self.rect.x0, self.rect.x1) - self.center_band_width(theme) / 2.0;
            self.draw_strip_places(
                scene,
                transform,
                theme,
                text_renderer,
                center_y,
                (
                    self.ws_pill_rect.x1 + f64::from(theme.tokens.spacing.s2),
                    center_left - f64::from(theme.tokens.spacing.s4),
                ),
            );

            self.draw_center_widget(scene, transform, theme, text_renderer, center_y);
            if !self.is_compact() {
                self.draw_session_clusters(scene, theme, text_renderer, center_y);
            }
        }
        if !self.responsive.active {
            self.draw_activity_badge(scene, transform, theme, text_renderer);
        }
        if !self.responsive.active || self.widget_editing {
            self.draw_focus_indicator(scene, transform, theme);
        }
        self.draw_widget_editor(scene, transform, text_renderer, theme);
        let clock_activity = self.chrome.as_ref().and_then(|chrome| {
            let snapshot = chrome.snapshot();
            snapshot.overlays.clock.then_some(snapshot.clock_activity)?
        });
        if let Some(activity) = clock_activity {
            self.draw_clock_overlay(scene, transform, text_renderer, theme, &activity);
        }

        if self.responsive.active {
            self.responsive
                .toolbar
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.responsive.dirty = false;
        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_fp = fp;
        self.cache_theme = Some(theme.clone());
        text_renderer.set_text_scale(renderer_scale);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A hidden bar is not there: whatever is under its strip gets the
        // input. The host shows it first when it is reached for.
        if self.presence() == TopBarPresence::Hidden {
            return;
        }
        match event {
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            } => self.focus_origin = FocusOrigin::Pointer,
            WindowEvent::KeyboardInput { event, .. }
                if event.state == winit::event::ElementState::Pressed =>
            {
                self.focus_origin = FocusOrigin::Keyboard;
            }
            _ => {}
        }
        if self.handle_strip_naming_event(event, ctx) {
            return;
        }
        if self.responsive.active && !self.widget_editing {
            if matches!(event, WindowEvent::Focused(false)) {
                self.responsive.toolbar.set_focus(false);
                self.responsive.dirty = true;
                return;
            }
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.last_cursor = Some(ctx.logical(position));
            }
            if self.responsive.toolbar.traps_focus()
                || self.responsive.toolbar.wants_pointer_capture()
            {
                self.responsive.toolbar.handle_event(event, ctx);
                self.responsive.dirty = true;
                return;
            }
            if !self.widget_editing && self.clock_overlay_rect.is_zero_area() {
                self.responsive.toolbar.handle_event(event, ctx);
                self.responsive.dirty |= *ctx.handled;
                return;
            }
        }

        if self.is_compact() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.last_cursor = Some(ctx.logical(position));
                return;
            }
            if self.handle_left_release(event, ctx.handled) {
                return;
            }
            self.handle_left_press(event, *ctx.modifiers, ctx.handled);
            return;
        }
        if self.handle_widget_or_clock_event(event, ctx.scale_factor, *ctx.modifiers, ctx.handled)
            || self.handle_center_pointer_event(event, ctx.scale_factor, ctx.handled)
        {
            return;
        }
        self.handle_keyboard(event, *ctx.modifiers, ctx.handled);
        if *ctx.handled || self.handle_app_tray_press(event, ctx.handled) {
            return;
        }
        if let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Right,
            ..
        } = event
            && self
                .last_cursor
                .is_some_and(|point| self.center_rect.contains(point))
        {
            *ctx.handled = self.activate_control(Self::CONTROL_EDIT_WIDGETS);
            *ctx.focus_target = Some(ctx.node_id);
            return;
        }
        if self.handle_left_release(event, ctx.handled) {
            return;
        }
        self.handle_left_press(event, *ctx.modifiers, ctx.handled);
        if *ctx.handled {
            *ctx.focus_target = Some(ctx.node_id);
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.clock_drag_offset.is_some()
            || self.widget_edit_drag.is_pending()
            || self.editor_pressed_control.is_some()
            || (self.responsive.active && self.responsive.toolbar.wants_pointer_capture())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.responsive.active && !self.widget_editing {
            vec![&self.responsive.toolbar]
        } else {
            Vec::new()
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.responsive.active && !self.widget_editing {
            vec![&mut self.responsive.toolbar]
        } else {
            Vec::new()
        }
    }

    fn traps_focus(&self) -> bool {
        self.widget_editing || (self.responsive.active && self.responsive.toolbar.traps_focus())
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.widget_editing {
            Some(self.widget_editor_rect)
        } else {
            self.responsive
                .active
                .then(|| self.responsive.toolbar.modal_capture())
                .flatten()
        }
    }

    fn name(&self) -> &'static str {
        "TopBarChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// So a harness clicks the pill the renderer painted instead of a
    /// guessed coordinate.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let app_tray = self
            .app_tray
            .iter()
            .map(|slot| (format!("topbar_tray_item_{}", slot.id), slot.rect));
        let mut rects = vec![
            ("topbar_tilo_mark".to_string(), self.tilo_mark_rect()),
            (
                "topbar_overview_button".to_string(),
                self.control_rect(Self::CONTROL_OVERVIEW),
            ),
            (
                "topbar_workspace_up".to_string(),
                self.control_rect(Self::CONTROL_WORKSPACE_UP),
            ),
            (
                "topbar_workspace_down".to_string(),
                self.control_rect(Self::CONTROL_WORKSPACE_DOWN),
            ),
            ("clock_overlay_pause".to_string(), self.clock_pause_rect),
            ("clock_overlay_add_minute".to_string(), self.clock_add_rect),
            ("clock_overlay_close".to_string(), self.clock_close_rect),
            // Carries the same rect only while the switch would
            // actually move (`walk_widget` drops zero-area entries).
            (
                "topbar_workspace_up_enabled".to_string(),
                if self.can_switch_up() {
                    self.control_rect(Self::CONTROL_WORKSPACE_UP)
                } else {
                    Rect::ZERO
                },
            ),
            (
                "topbar_workspace_down_enabled".to_string(),
                if self.can_switch_down() {
                    self.control_rect(Self::CONTROL_WORKSPACE_DOWN)
                } else {
                    Rect::ZERO
                },
            ),
            (
                "topbar_workspace_pill".to_string(),
                self.control_rect(Self::CONTROL_WORKSPACE_SETTINGS),
            ),
            (
                "topbar_profile".to_string(),
                self.control_rect(Self::CONTROL_PROFILE),
            ),
            (
                "topbar_tray_pill".to_string(),
                self.control_rect(Self::CONTROL_QUICK_SETTINGS),
            ),
            (
                "topbar_tray_hit".to_string(),
                self.control_rect(Self::CONTROL_QUICK_SETTINGS),
            ),
            (
                "topbar_media_indicator".to_string(),
                self.control_rect(Self::CONTROL_MEDIA),
            ),
            (
                "topbar_avatar_stack".to_string(),
                self.control_rect(Self::CONTROL_COLLABORATORS),
            ),
            (
                "topbar_sharing_indicator".to_string(),
                self.control_rect(Self::CONTROL_COLLABORATORS),
            ),
            (
                "topbar_continuity".to_string(),
                self.control_rect(Self::CONTROL_CONTINUITY),
            ),
            (
                "topbar_continuity_dismiss".to_string(),
                self.control_rect(Self::CONTROL_CONTINUITY_DISMISS),
            ),
            (
                "topbar_center_clock".to_string(),
                self.control_rect(Self::CONTROL_CENTER_CLOCK),
            ),
            (
                "topbar_center_system_monitor".to_string(),
                self.control_rect(Self::CONTROL_CENTER_MONITOR),
            ),
            (
                "topbar_center_calendar".to_string(),
                self.control_rect(Self::CONTROL_CENTER_CALENDAR),
            ),
        ];
        rects.extend(self.editor_test_rects());
        rects.extend(self.strip_test_rects());
        if !self.responsive.active {
            rects.extend(app_tray);
        }
        rects
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.responsive.active && !self.widget_editing {
            let mut node = ui::a11y::AccessNode::new(ui::a11y::Role::Toolbar)
                .with_semantic_key("topbar")
                .with_name(t("topbar-desktop-navigation"));
            node.children.push(
                ui::a11y::AccessNode::new(ui::a11y::Role::StaticText)
                    .with_hashed_key("topbar-clock-reading")
                    .with_name(self.responsive_reading())
                    .with_description(self.responsive_description())
                    .with_bounds(self.responsive.reading_rect),
            );
            if let Some(clock) = self.clock_accessibility_node() {
                node.children
                    .push(clock.with_hashed_key("topbar-clock-overlay"));
            }
            return Some(node);
        }

        let mut children: Vec<AccessNode> = self
            .control_targets()
            .into_iter()
            .map(|target| {
                let role = match target {
                    Self::CONTROL_CENTER_CLOCK
                    | Self::CONTROL_CENTER_MONITOR
                    | Self::CONTROL_CENTER_CALENDAR => Role::ToggleButton,
                    _ => Role::Button,
                };
                let checked = match target {
                    Self::CONTROL_CENTER_CLOCK => Some(self.center_widget == CenterWidget::Clock),
                    Self::CONTROL_CENTER_MONITOR => {
                        Some(self.center_widget == CenterWidget::SystemMonitor)
                    }
                    Self::CONTROL_CENTER_CALENDAR => {
                        Some(self.center_widget == CenterWidget::Calendar)
                    }
                    _ => None,
                };
                let mut node = AccessNode::new(role)
                    .with_action_target(target)
                    .with_name(self.control_name(target))
                    .with_bounds(self.control_rect(target))
                    .with_state(AccessState {
                        focused: self.focused && self.focused_control.as_deref() == Some(target),
                        focusable: true,
                        disabled: self.control_is_disabled(target),
                        checked,
                        ..Default::default()
                    });
                if matches!(
                    target,
                    Self::CONTROL_WORKSPACE_UP | Self::CONTROL_WORKSPACE_DOWN
                ) {
                    node = node.with_description(t_args(
                        "topbar-workspace-position",
                        &[
                            ("position", self.workspace_index.saturating_add(1).into()),
                            ("total", self.workspace_total.into()),
                        ],
                    ));
                }
                if target == Self::CONTROL_WORKSPACE_SETTINGS
                    && let Some(host) = self.host_label.as_deref()
                {
                    node = node.with_description(t_args(
                        "topbar-workspace-host",
                        &[("host", host.into())],
                    ));
                }
                node
            })
            .collect();
        if let Some(clock) = self.clock_accessibility_node() {
            children.push(clock);
        }
        children.extend(self.strip_accessibility());
        Some(
            AccessNode::new(Role::Toolbar)
                .with_semantic_key(if self.is_compact() {
                    "topbar-compact"
                } else {
                    "topbar"
                })
                .with_name(t(if self.is_compact() {
                    "topbar-display-navigation"
                } else {
                    "topbar-desktop-navigation"
                }))
                .with_state(
                    AccessState::new()
                        .focusable(self.is_focusable())
                        .focused(self.is_focused()),
                )
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        if self.responsive.active && !self.widget_editing {
            return self.responsive.toolbar.synthetic_accessibility_rect(path);
        }
        let index = *path.first()?;
        self.control_targets()
            .get(index)
            .map(|target| self.control_rect(target))
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        if self.responsive.active && !self.widget_editing {
            self.responsive.dirty = true;
            return self.responsive.toolbar.focus_synthetic_accessibility(path);
        }
        let Some(index) = path.first().copied() else {
            return false;
        };
        let Some(target) = self.control_targets().get(index).copied() else {
            return false;
        };
        self.focused_control = Some(target.to_string());
        true
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if matches!(
            target,
            Self::CONTROL_EDIT_WIDGETS | Self::CONTROL_WIDGET_EDITOR
        ) && matches!(action, ui::a11y::Action::Activate)
        {
            return self.open_widget_editor();
        }
        if self.responsive.active && !self.widget_editing {
            let performed = self
                .responsive
                .toolbar
                .perform_semantic_action(target, action, state, path);
            self.responsive.dirty |= performed;
            return performed;
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        if let Some(performed) = self.strip_semantic_action(target) {
            return performed;
        }
        match target {
            "clock.overlay.toggle" => {
                self.publish_clock_activity_command("toggle", "toggle-stopwatch");
                true
            }
            "clock.overlay.add-minute" => {
                self.publish_clock_activity_command("add-minute", "lap-stopwatch");
                true
            }
            "clock.overlay.close" => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.intent(ChromeIntent::HideClock);
                }
                self.publish_clock_activity_command("close", "close-stopwatch");
                true
            }
            _ => self.activate_control(target),
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.responsive.active
            && let Some(cursor) = self.responsive.toolbar.cursor_shape(point)
        {
            return Some(cursor);
        }
        if self.clock_drag_offset.is_some() {
            return Some(winit::window::CursorIcon::Grabbing);
        }
        if self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.snapshot().overlays.clock)
        {
            if self.clock_control_at(point).is_some() {
                return Some(winit::window::CursorIcon::Pointer);
            }
            if clock_overlay_drag_region(self.clock_overlay_rect).contains(point) {
                return Some(winit::window::CursorIcon::Grab);
            }
        }
        if self.strip_hit(point) {
            return Some(winit::window::CursorIcon::Pointer);
        }
        self.control_targets()
            .into_iter()
            .any(|target| self.control_rect(target).contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        // Keyboard traversal reaches the primary display's full bar; a
        // compact bar repeats a subset of it.
        !self.is_compact()
    }

    fn set_focus(&mut self, focused: bool) {
        if !focused {
            self.cancel_strip_naming();
            self.cancel_editor_drag();
            self.editor_submit_key = None;
            self.editor_pointer_release = None;
        }
        self.focused = focused;
        if !focused {
            self.focus_origin = FocusOrigin::Keyboard;
        }
        self.responsive.toolbar.set_focus(focused);
        if focused && self.focused_control.is_none() {
            self.focused_control = self
                .control_targets()
                .first()
                .map(|target| (*target).into());
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    /// Must keep returning `true` until the first successful draw
    /// fills the cache, or the first frame after layout never paints.
    fn needs_redraw(&self) -> bool {
        if self.responsive.active && self.responsive.dirty {
            return true;
        }
        if self.presence() != self.cache_fp.presence {
            return true;
        }
        // `Default` for CacheFingerprint has rect_px = (0,0,0,0) → w=0.
        if self.cache_fp.rect_px.2 == 0 {
            return true;
        }
        // A status change must reach the screen without waiting for an
        // unrelated event — ancestors only rebuild on a dirty descendant.
        let live = self.live_status();
        if status_key_of(&live) != self.cache_fp.status_key
            || live
                .media
                .iter()
                .map(|u| {
                    (
                        u.client_id,
                        match u.kind {
                            crate::chrome::MediaKind::Microphone => 0,
                            crate::chrome::MediaKind::Camera => 1,
                        },
                        u.label.clone(),
                    )
                })
                .collect::<Vec<_>>()
                != self.cache_fp.media_key
        {
            return true;
        }
        if self.app_tray_revision() != self.cache_fp.app_tray_revision
            || self.continuity_label() != self.cache_fp.continuity
        {
            return true;
        }
        if let Some(started) = self.media_started {
            let elapsed = started.elapsed();
            return elapsed < Duration::from_secs(4)
                || (self.cache_fp.media_expanded && elapsed >= Duration::from_secs(4));
        }
        false
    }

    fn mark_drawn(&mut self) {}

    /// Fixed intrinsic size, so the taffy node never needs recomputing
    /// after the first pass.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.overlay_layout_stale()
    }

    /// Docking or hiding the bar moves the windows, so the tree it was laid
    /// out in is rebuilt; showing a hidden bar over them does not.
    fn take_structure_dirty(&mut self) -> bool {
        let stale = self.overlay_layout_stale();
        let mut children = false;
        for child in self.children_mut() {
            children |= child.take_structure_dirty();
        }
        stale || children
    }
}

/// Cursor and avatar colours for a room, settled over the whole roster so
/// a peer's avatar matches the colour their cursor is drawn in elsewhere. An
/// explicit `cursor_color` outranks anything computed; the local user takes
/// part in the settlement (never the avatar stack) so every host agrees.
fn roster_colors(
    roster: &[tilo_desktop_model::Collaborator],
    local: &str,
) -> HashMap<String, (u8, u8, u8)> {
    let explicit: HashMap<_, _> = roster
        .iter()
        .filter_map(|c| {
            c.cursor_color
                .as_deref()
                .and_then(ui::presence::parse_hex_color)
                .map(|rgb| (c.id.clone(), rgb))
        })
        .collect();
    let mut ids: Vec<_> = roster
        .iter()
        .filter(|c| !explicit.contains_key(&c.id))
        .map(|c| c.id.as_str())
        .collect();
    if !local.is_empty() && !explicit.contains_key(local) {
        ids.push(local);
    }
    let mut assigned =
        ui::presence::assign_peer_colors(&ids, &explicit.values().copied().collect::<Vec<_>>());
    assigned.extend(explicit);
    assigned
}
#[cfg(test)]
mod calendar_strip_tests {
    use super::*;

    /// The strip reports the day it is actually run on.
    #[test]
    fn the_strip_reads_the_clock() {
        let day = ClockSnapshot {
            weekday: "Mon".into(),
            date: "9 March".into(),
            ..Default::default()
        };
        let (weekday, date, _) = calendar_strip_parts(day);
        assert_eq!(weekday, "Mon");
        assert_eq!(date, "9 March");

        let other = ClockSnapshot {
            weekday: "Thu".into(),
            date: "25 December".into(),
            ..Default::default()
        };
        let (weekday, date, _) = calendar_strip_parts(other);
        assert_eq!(weekday, "Thu");
        assert_eq!(date, "25 December");
    }

    #[test]
    fn the_event_slot_names_no_meeting() {
        let (_, _, note) = calendar_strip_parts(ClockSnapshot::default());
        assert_eq!(note, "No calendar connected");
        assert!(
            !note.contains("Design Sync"),
            "the slot must not carry an invented appointment"
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "layout tests assert exact token-derived pixel values"
)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use tilo_desktop_model::{
        DesktopTree, Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };

    #[test]
    fn compact_workspace_identity_outranks_redundant_position() {
        let mut bar = sample_chrome();
        bar.workspace_name = "Home".into();
        bar.workspace_total = 1;
        bar.workspace_index = 0;
        let mut tr = TextRenderer::new();
        let label = bar.workspace_label(&mut tr, &Theme::default().text(TextRole::Label), 112.0);
        assert!(label.contains("Home"), "{label}");
    }

    #[test]
    fn workspace_settings_affordance_is_distinct_from_global_controls() {
        assert_ne!(
            TopBarChrome::WORKSPACE_SETTINGS_ICON,
            ui::icons::Icon::Settings2
        );
    }

    #[test]
    fn desktop_topbar_uses_scaled_tokens_once_and_contains_workspace_text() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut bar = sample_chrome();
        bar.workspace_name = "Home".into();
        bar.rect = Rect::new(0.0, 0.0, 960.0, 48.0);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let mut scene = Scene::new();
        bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            &PresenceCtx::default(),
        );
        let runs = &scene.encoding().resources.glyph_runs;
        assert!(!runs.is_empty());
        assert!(
            runs.iter()
                .all(|run| run.font_size <= theme.tokens.typography.size(TextRole::Heading))
        );
        assert_eq!(renderer.text_scale(), 2.0);
        renderer.set_text_scale(1.0);
        // Measured in the style the pill draws its label in.
        let (_, height) = renderer.measure_style(
            "Home  1/1",
            None,
            &theme
                .tokens
                .typography
                .style(TextRole::Caption)
                .with_weight(bar.workspace_name_text_weight()),
        );
        assert!(f64::from(height) <= bar.ws_pill_rect.height());
        assert!(bar.ws_pill_rect.x1 <= bar.rect.center().x - bar.center_band_width(&theme) / 2.0);
    }

    fn sample_chrome() -> TopBarChrome {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        TopBarChrome::from_tree(&tree)
    }

    #[test]
    fn time_bucket_wait_runs_to_the_next_whole_bucket() {
        let minute_in = Duration::from_secs(60 * 1_000 + 12) + Duration::from_millis(500);
        assert_eq!(
            until_next_time_bucket(minute_in, 60),
            Duration::from_millis(47_500)
        );
        assert_eq!(
            until_next_time_bucket(Duration::from_secs(60 * 1_000), 60),
            Duration::from_secs(60),
            "a reading taken on the boundary holds for the whole next minute"
        );
        assert_eq!(
            until_next_time_bucket(Duration::from_millis(4_250), 1),
            Duration::from_millis(750)
        );
    }

    /// The clock is read only when drawing, so an idle desktop repaints it
    /// solely through the bar's wake deadline at the end of the minute.
    #[test]
    fn idle_clock_wakes_and_repaints_when_the_minute_turns() {
        let mut bar = sample_chrome();
        assert_eq!(
            bar.next_wake_deadline(),
            None,
            "nothing drawn, nothing stale"
        );
        assert!(!bar.update(&(), &()));

        lay_out_and_draw(&mut bar, 1200.0);
        let now = Instant::now();
        let wake = bar
            .next_wake_deadline()
            .expect("a drawn clock wakes when its minute ends");
        assert!(wake > now && wake <= now + Duration::from_secs(60));
        assert!(!bar.update(&(), &()), "the minute has not turned yet");

        let overdue = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock is past its origin");
        bar.time_wake = Some(overdue);
        assert_eq!(
            bar.next_wake_deadline(),
            None,
            "an overdue wake must not hold the loop at a zero timeout"
        );
        assert!(
            bar.update(&(), &()),
            "the turned minute marks the bar dirty so the frame repaints it"
        );
        assert!(
            bar.next_wake_deadline()
                .is_some_and(|next| next > Instant::now()),
            "the next minute is scheduled"
        );
        assert!(!bar.update(&(), &()), "one repaint per minute");
    }

    fn floating_clock() -> TopBarChrome {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        {
            let mut state = chrome.borrow_mut();
            state.overlays.clock = true;
            state.clock_activity = Some(ClockActivity {
                label: "Tea".into(),
                remaining_secs: 125,
                overlay: true,
                ..Default::default()
            });
        }
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        bar.clock_overlay_rect = Rect::new(100.0, 80.0, 352.0, 238.0);
        [
            bar.clock_pause_rect,
            bar.clock_add_rect,
            bar.clock_close_rect,
        ] = clock_overlay_controls(bar.clock_overlay_rect);
        bar
    }

    #[test]
    fn floating_clock_cursor_exposes_buttons_and_drag_state() {
        let mut bar = floating_clock();
        assert_eq!(
            bar.cursor_shape(bar.clock_pause_rect.center()),
            Some(winit::window::CursorIcon::Pointer)
        );
        let header = Point::new(
            bar.clock_overlay_rect.x0 + 20.0,
            bar.clock_overlay_rect.y0 + 20.0,
        );
        assert_eq!(
            bar.cursor_shape(header),
            Some(winit::window::CursorIcon::Grab)
        );
        assert!(bar.begin_clock_drag_at(header));
        assert_eq!(
            bar.cursor_shape(Point::new(900.0, 700.0)),
            Some(winit::window::CursorIcon::Grabbing)
        );
    }

    #[test]
    fn floating_clock_accessibility_announces_time_and_focus() {
        let mut bar = floating_clock();
        bar.clock_control_focus = Some(1);
        let node = bar.clock_accessibility_node().expect("visible timer node");
        assert_eq!(node.name.as_deref(), Some("Floating timer"));
        assert!(
            node.description
                .as_deref()
                .is_some_and(|text| text.contains("Alt"))
        );
        assert_eq!(node.children[0].name.as_deref(), Some("Tea"));
        assert_eq!(node.children[0].value.as_deref(), Some("02:05"));
        assert!(node.children[2].state.focused);
    }

    fn lay_out_and_draw_with_theme(bar: &mut TopBarChrome, width: f32, theme: &Theme) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(width, TOPBAR_HEIGHT_PX),
                    ..Default::default()
                },
                &[node],
            )
            .expect("viewport");
        taffy
            .compute_layout_with_measure(
                viewport,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("layout");
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        bar.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
            &PresenceCtx::default(),
        );
    }

    fn lay_out_and_draw(bar: &mut TopBarChrome, width: f32) {
        lay_out_and_draw_with_theme(bar, width, &Theme::default());
    }

    /// Lay `bar` out at the top of a 1280x800 column whose other child
    /// stands for the windows, as the desktop surface stacks them; returns
    /// where the windows start.
    fn lay_out_over_windows(bar: &mut TopBarChrome) -> f32 {
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let windows = taffy
            .new_leaf(Style {
                flex_grow: 1.0,
                ..Default::default()
            })
            .expect("windows");
        let column = taffy
            .new_with_children(
                Style {
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size::from_lengths(1280.0, 800.0),
                    ..Default::default()
                },
                &[node, windows],
            )
            .expect("column");
        taffy
            .compute_layout_with_measure(
                column,
                Size {
                    width: taffy::AvailableSpace::Definite(1280.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("layout");
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        taffy.layout(windows).expect("windows laid out").location.y
    }

    fn painted(bar: &mut TopBarChrome) -> bool {
        let mut scene = Scene::new();
        bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            &PresenceCtx::default(),
        );
        !scene.encoding().is_empty()
    }

    fn click(bar: &mut TopBarChrome, at: Point) -> bool {
        dispatch_topbar_event(bar, &ui::synthetic_input::cursor_moved(at.x, at.y));
        let pressed = dispatch_topbar_event(bar, &ui::synthetic_input::mouse_press());
        let released = dispatch_topbar_event(bar, &ui::synthetic_input::mouse_release());
        pressed || released
    }

    /// The person's "hide the top bar": the windows take its strip at once;
    /// hidden, it paints nothing and every press under it goes to what is
    /// below; shown, it stands over the windows in its docked place,
    /// painted and pressable, and nothing under it moves either way.
    #[test]
    fn a_hidden_bar_gives_its_strip_to_the_windows_and_shows_over_them_in_place() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());

        assert_eq!(lay_out_over_windows(&mut bar), TOPBAR_HEIGHT_PX, "docked");
        assert!(painted(&mut bar));
        let docked = bar.rect;
        let overview = bar.overview_btn_rect.center();

        chrome.borrow_mut().top_bar = TopBarPresence::Hidden;
        assert!(bar.is_layout_dirty(), "hiding re-lays the column out");
        assert!(
            bar.take_structure_dirty(),
            "and rebuilds the tree the bar stands in"
        );
        assert_eq!(
            lay_out_over_windows(&mut bar),
            0.0,
            "the windows take the strip"
        );
        assert!(!bar.is_layout_dirty());
        assert_eq!(bar.rect, docked, "it keeps its place for when it shows");
        assert!(!painted(&mut bar), "hidden paints nothing");
        assert!(!click(&mut bar, overview), "hidden takes no press");
        assert!(chrome.borrow().intents.is_empty());

        chrome.borrow_mut().top_bar = TopBarPresence::Shown;
        assert!(bar.needs_redraw(), "showing repaints the bar");
        assert!(
            !bar.is_layout_dirty() && !bar.take_structure_dirty(),
            "showing over the windows changes no layout: nothing moves"
        );
        assert_eq!(lay_out_over_windows(&mut bar), 0.0);
        assert!(painted(&mut bar));
        assert_eq!(
            bar.overview_btn_rect.center(),
            overview,
            "same place as docked"
        );
        assert!(click(&mut bar, overview), "shown, its controls work");
        assert_eq!(
            chrome.borrow().intents.as_slice(),
            &[ChromeIntent::ToggleOverview]
        );

        chrome.borrow_mut().top_bar = TopBarPresence::Hidden;
        assert!(bar.needs_redraw(), "hiding again clears it from the screen");
        assert!(!painted(&mut bar));
    }

    #[test]
    fn persistent_topbar_controls_have_coherent_targets() {
        let mut bar = sample_chrome();
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(bar.overview_btn_rect.width() >= BAR_CONTROL_PX);
        assert!(bar.overview_btn_rect.height() >= BAR_CONTROL_PX);
        assert!(bar.up_btn_rect.height() >= BAR_CONTROL_PX);
        assert!(bar.up_btn_rect.width() >= BAR_CONTROL_PX);
        assert!(bar.down_btn_rect.height() >= BAR_CONTROL_PX);
        assert!(bar.down_btn_rect.width() >= BAR_CONTROL_PX);
        assert!(bar.ws_pill_rect.height() >= BAR_CONTROL_PX);
        assert!(bar.tray_pill_rect.height() >= BAR_CONTROL_PX);
        assert!(bar.center_rect.height() >= BAR_CONTROL_PX);
    }

    /// HIG §9.6: every pressable thing in the bar shares one height, so
    /// the pills and buttons sit on one top edge and one bottom edge,
    /// centred on the bar.
    #[test]
    fn bar_controls_share_one_height_and_centre_line() {
        let mut bar = sample_chrome();
        lay_out_and_draw(&mut bar, 1280.0);
        let reference = bar.overview_btn_rect;
        let controls = [
            ("overview", bar.overview_btn_rect),
            ("workspace up", bar.up_btn_rect),
            ("workspace down", bar.down_btn_rect),
            ("workspace pill", bar.ws_pill_rect),
            ("centre band", bar.center_rect),
            ("tray pill", bar.tray_pill_rect),
        ];
        for (name, r) in controls {
            assert!(
                (r.height() - BAR_CONTROL_PX).abs() < 0.01,
                "{name} is {} px tall, not the shared {BAR_CONTROL_PX}",
                r.height()
            );
            assert!(
                (r.y0 - reference.y0).abs() < 0.01 && (r.y1 - reference.y1).abs() < 0.01,
                "{name} {r:?} does not share the nav cluster's edges {reference:?}"
            );
        }
        let bar_centre = f64::midpoint(bar.rect.y0, bar.rect.y1);
        assert!(
            (reference.center().y - bar_centre).abs() < 0.01,
            "controls centre at {} in a bar centred on {bar_centre}",
            reference.center().y
        );
        // The clock is the time on the bar: no pill and no position dots,
        // and its keyboard and assistive choices stand on the time itself.
        assert!(bar.center_is_plain_text());
        for target in [
            TopBarChrome::CONTROL_CENTER_CLOCK,
            TopBarChrome::CONTROL_CENTER_MONITOR,
            TopBarChrome::CONTROL_CENTER_CALENDAR,
        ] {
            assert_eq!(bar.control_rect(target), bar.center_rect, "{target}");
        }
        bar.center_widget = CenterWidget::SystemMonitor;
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(
            !bar.center_is_plain_text(),
            "a lane-filling widget keeps its selector pill"
        );
    }

    /// The workspace capsule pads its label on the left exactly as much
    /// as it pads the gear on the right, and the gear sits on the pill's
    /// centre line — so the pill reads centred on its contents.
    #[test]
    fn workspace_pill_pads_its_label_and_gear_symmetrically() {
        let mut bar = sample_chrome();
        let theme = Theme::default();
        lay_out_and_draw_with_theme(&mut bar, 1280.0, &theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        // `draw_workspace_nav` hands the pill an x one `s3` past the
        // down switch; the label starts there, one `s2` inside the pill,
        // which leaves an `s1` gutter between the cluster and the capsule.
        let text_x = bar.down_btn_rect.x1 + f64::from(theme.tokens.spacing.s3);
        assert!(
            (text_x - bar.ws_pill_rect.x0 - pad).abs() < 0.01,
            "leading pad is {} not {pad}",
            text_x - bar.ws_pill_rect.x0
        );
        let gutter = f64::from(theme.tokens.spacing.s1);
        assert!(
            (bar.ws_pill_rect.x0 - bar.down_btn_rect.x1 - gutter).abs() < 0.01,
            "the gutter between the nav cluster and the pill is {} not {gutter}",
            bar.ws_pill_rect.x0 - bar.down_btn_rect.x1
        );
        assert!(
            (bar.ws_pill_rect.x1 - bar.ws_gear_rect.x1 - pad).abs() < 0.01,
            "trailing pad is {} not {pad}",
            bar.ws_pill_rect.x1 - bar.ws_gear_rect.x1
        );
        assert!(
            (bar.ws_gear_rect.center().y - bar.ws_pill_rect.center().y).abs() < 0.01,
            "gear {:?} is off the pill's centre line {:?}",
            bar.ws_gear_rect,
            bar.ws_pill_rect
        );
    }

    /// In a narrow lane the monitor's pairs drop from the inside out
    /// rather than painting over the CPU readout: RAM goes first, then
    /// TMP; whatever remains keeps `s3` clear of its neighbour.
    #[test]
    fn monitor_columns_drop_pairs_from_the_inside_out_in_a_narrow_lane() {
        let widths = MetricWidths {
            cpu_label: 20.0,
            cpu_value: 24.0,
            ram_label: 22.0,
            ram_value: 24.0,
            tmp_label: 22.0,
            tmp_value: 20.0,
        };
        let (s1, s3) = (4.0, 12.0);
        let cpu_end = 20.0 + s1 + 24.0;

        let wide = monitor_columns((0.0, 400.0), &widths, s1, s3);
        let (ram_label_x, _) = wide.ram.expect("a 400 px lane holds every pair");
        let (tmp_label_x, tmp_value_x) = wide.tmp.expect("a 400 px lane holds every pair");
        assert!(
            ram_label_x >= cpu_end + s3,
            "RAM keeps s3 clear of the CPU value"
        );
        assert!(
            (tmp_value_x + 20.0 - 400.0).abs() < 1e-9,
            "TMP's value ends on the lane's edge"
        );
        assert!(tmp_label_x > ram_label_x);
        assert!(wide.spark.0 >= cpu_end + s3 && wide.spark.1 <= ram_label_x - s3);

        // 112 px is the lane a 220 px band leaves at an 800 px viewport.
        let compact = monitor_columns((0.0, 112.0), &widths, s1, s3);
        assert!(compact.ram.is_none(), "RAM is the first pair to yield");
        let (tmp_label_x, _) = compact.tmp.expect("TMP still fits a 112 px lane");
        assert!(
            tmp_label_x >= cpu_end + s3,
            "TMP {tmp_label_x} keeps s3 clear of the CPU value ending at {cpu_end}"
        );
        assert!(compact.spark.1 <= tmp_label_x - s3);

        let tiny = monitor_columns((0.0, 70.0), &widths, s1, s3);
        assert!(
            tiny.ram.is_none() && tiny.tmp.is_none(),
            "nothing overlaps the CPU pair"
        );
        assert_eq!(tiny.cpu_label_x, 0.0);
        assert_eq!(tiny.cpu_value_x, 20.0 + s1);
    }

    /// The bar's density constants are the spacing scale under other
    /// names; this pins them so they cannot drift off it unnoticed.
    #[test]
    fn bar_density_sits_on_the_spacing_scale() {
        let tokens = Theme::default().tokens;
        let spacing = tokens.spacing;
        assert_eq!(
            BAR_CONTROL_PX,
            f64::from(tokens.metrics.bar_control_height - 2.0 * spacing.s1)
        );
        assert_eq!(CENTER_INSET_PX, f64::from(spacing.s3));
        // The bar is its 20 px controls plus half an `s1` of air each side:
        // a 24 px line (48 physical px at scale 2), not the 36, 40 or 48 it
        // grew to.
        assert_eq!(TOPBAR_HEIGHT_PX, 24.0);
        assert_eq!(TOPBAR_HEIGHT_PX, BAR_CONTROL_LOGICAL + spacing.s1);
        assert!(TOPBAR_HEIGHT_PX < tokens.metrics.bar_height);
    }

    /// At the default text size the bar is 24 px; a large text size grows
    /// it so the time's line never spills out of it.
    #[test]
    fn the_bar_grows_with_large_text_and_not_before() {
        assert_eq!(topbar_height(&Theme::default()), TOPBAR_HEIGHT_PX);
        for scale in [1.25_f32, 1.5, 2.0] {
            let theme = Theme::default().with_text_scale(scale);
            let line = theme
                .tokens
                .typography
                .style(TextRole::Body)
                .line_height_px();
            let height = topbar_height(&theme);
            assert!(
                line < height,
                "at {scale}x the time's {line} px line fits the {height} px bar"
            );
        }
    }

    #[test]
    fn large_text_activity_stays_single_line_inside_center_content_lane() {
        let mut bar = sample_chrome();
        bar.center_rect = Rect::new(460.0, 7.0, 820.0, 41.0);
        bar.activity_headline = Some("Recorded 3 workspaces: Home, Research and Design".into());
        bar.activity_unread = 3;
        let mut renderer = TextRenderer::new();
        let theme = Theme::default().with_text_scale(1.5);
        let style = theme.text(TextRole::Caption);
        let max_width = bar.activity_content_width(&theme);
        let label = bar.activity_display_label(&mut renderer, &style, max_width);
        let (_, height) =
            renderer.measure_style(&label, None, &style.clone().with_weight(TextWeight::Medium));
        let (width, _) =
            renderer.measure_style(&label, None, &style.clone().with_weight(TextWeight::Medium));
        assert!(
            !label.contains('\n'),
            "activity headline must never wrap in the fixed topbar pill: {label:?}"
        );
        assert!(
            width <= max_width + 0.5,
            "activity headline width {width} exceeds reserved center lane {max_width}"
        );
        assert!(
            height <= topbar_height(&theme) - 4.0,
            "activity headline height {height} must fit in the compact topbar"
        );
    }

    #[test]
    fn compact_large_text_preserves_the_full_activity_sentence_and_detail_route() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        // The 960px chrome reserves a 360px centre selector; after its dots
        // and breathing room, this is the usable activity lane.
        bar.center_rect = Rect::new(300.0, 7.0, 660.0, 41.0);
        bar.activity_headline = Some("Quinten · 2 permission changes".into());
        bar.activity_unread = 4;
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            ui::font_catalog::checkout_root(),
            "en-US",
        );
        let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
        let theme = Theme::default().with_text_scale(1.3);
        let style = theme.text(TextRole::Caption);
        let max_width = bar.activity_content_width(&theme);
        let label = bar.activity_display_label(&mut renderer, &style, max_width);
        let (width, _) =
            renderer.measure_style(&label, None, &style.clone().with_weight(TextWeight::Medium));
        // The lane is symmetric and wider now, so the unread count fits
        // after the full sentence instead of a bare chevron.
        assert!(
            label.starts_with("Quinten · 2 permission changes"),
            "the full sentence survives: {label:?}"
        );
        assert!(width <= max_width + 0.5);

        bar.activity_started = Some(Instant::now());
        bar.last_cursor = Some(bar.center_rect.center());
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        assert!(handled);
        assert!(
            matches!(
                chrome.borrow().intents.last(),
                Some(crate::chrome::ChromeIntent::ShowActivity(_))
            ),
            "the explicit chevron label points to the activity detail route"
        );
        assert!(!chrome.borrow().overlays.room_activity);
    }

    fn press_named_key(bar: &mut TopBarChrome, key: winit::keyboard::NamedKey) -> bool {
        let key_event = ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(key),
            None,
            winit::event::ElementState::Pressed,
        );
        let event = ui::synthetic_input::keyboard_input(key_event);
        let mut handled = false;
        bar.handle_keyboard(&event, winit::event::Modifiers::default(), &mut handled);
        handled
    }

    fn dispatch_topbar_event(bar: &mut TopBarChrome, event: &WindowEvent) -> bool {
        dispatch_topbar_event_with_modifiers(bar, event, winit::event::Modifiers::default())
    }

    fn dispatch_topbar_event_with_modifiers(
        bar: &mut TopBarChrome,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).expect("test leaf");
        let mut state = AutoCommit::new();
        let mut handled = false;
        let mut focus = None;
        bar.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(&mut state),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn center_editor_opens_by_secondary_click_and_dismisses_conventionally() {
        let mut bar = sample_chrome();
        lay_out_and_draw(&mut bar, 1280.0);
        bar.last_cursor = Some(bar.center_rect.center());
        let secondary = ui::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Right,
        );
        assert!(dispatch_topbar_event(&mut bar, &secondary));
        assert!(bar.widget_editing);
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(
            bar.test_rects()
                .iter()
                .any(|(name, rect)| name == "topbar_editor_remove_clock" && rect.area() > 0.0)
        );
        assert!(bar.accessibility().is_some_and(|node| {
            node.children.iter().any(|child| {
                child.action_target.as_deref() == Some(TopBarChrome::CONTROL_REMOVE_CLOCK)
            })
        }));

        let key_event = ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
            None,
            winit::event::ElementState::Pressed,
        );
        let escape = ui::synthetic_input::keyboard_input(key_event);
        assert!(dispatch_topbar_event(&mut bar, &escape));
        assert!(!bar.widget_editing);

        bar.last_cursor = Some(bar.center_rect.center());
        assert!(dispatch_topbar_event(&mut bar, &secondary));
        lay_out_and_draw(&mut bar, 1280.0);
        bar.last_cursor = Some(Point::new(2.0, 2.0));
        let outside = ui::synthetic_input::mouse_press();
        assert!(dispatch_topbar_event(&mut bar, &outside));
        assert!(!bar.widget_editing);
    }

    fn two_workspace_tree() -> DesktopTree {
        let first = Workspace::new(WorkspaceId(1), "A");
        let second = Workspace::new(WorkspaceId(2), "B");
        let active = first.id;
        DesktopTree {
            workspaces: vec![first, second],
            active,
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        }
    }

    /// Your colour rings a top-bar control only for keyboard focus. Clicking
    /// the bar used to light the overview button, the bar's first stop, in
    /// your colour, though no keyboard was involved.
    #[test]
    fn the_focus_ring_follows_the_keyboard_not_a_click() {
        let mut bar = sample_chrome();
        bar.set_focus(true);
        assert!(
            bar.focus_ring_visible(),
            "keyboard traversal into the bar shows it"
        );

        let press = ui::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Left,
        );
        let _ = dispatch_topbar_event(&mut bar, &press);
        assert!(bar.is_focused());
        assert!(
            !bar.focus_ring_visible(),
            "a click on the bar shows no ring"
        );

        let key = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowRight),
            None,
            winit::event::ElementState::Pressed,
        ));
        let _ = dispatch_topbar_event(&mut bar, &key);
        assert!(bar.focus_ring_visible(), "a key press brings it back");

        let _ = dispatch_topbar_event(&mut bar, &press);
        bar.set_focus(false);
        bar.set_focus(true);
        assert!(
            bar.focus_ring_visible(),
            "focus arriving afresh starts unpointed"
        );
    }

    #[test]
    fn keyboard_navigation_is_one_focus_stop_with_optional_controls() {
        let mut bar = sample_chrome();
        bar.collaborator_initials.push('A');
        bar.media_indicator_rect = Rect::new(1120.0, 8.0, 1152.0, 40.0);
        bar.set_focus(true);

        assert!(bar.is_focusable());
        assert!(bar.is_focused());
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_OVERVIEW)
        );

        assert!(press_named_key(
            &mut bar,
            winit::keyboard::NamedKey::ArrowRight
        ));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_WORKSPACE_UP)
        );
        assert!(press_named_key(
            &mut bar,
            winit::keyboard::NamedKey::ArrowLeft
        ));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_OVERVIEW)
        );
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::End));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_MEDIA)
        );
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Home));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_OVERVIEW)
        );

        // Removing an optional target leaves the focus stop intact and End
        // resolves against the newly published control set.
        bar.media_indicator_rect = Rect::ZERO;
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::End));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_COLLABORATORS)
        );
    }

    #[test]
    fn keyboard_activation_matches_pointer_actions_and_respects_disabled_workspaces() {
        let tree = two_workspace_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        bar.set_focus(true);

        bar.focused_control = Some(TopBarChrome::CONTROL_CENTER_MONITOR.to_string());
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
        assert_eq!(bar.center_widget, CenterWidget::SystemMonitor);

        bar.focused_control = Some(TopBarChrome::CONTROL_OVERVIEW.to_string());
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Space));
        assert!(chrome.borrow().overlays.overview);

        // The first workspace cannot move upward: keyboard activation must
        // report unhandled just like a disabled pointer target.
        bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_UP.to_string());
        assert!(!press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
        let first_id = tree.workspaces.first().expect("first workspace").id;
        assert_eq!(chrome.borrow().tree().active, first_id);

        bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_DOWN.to_string());
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
        let second_id = tree.workspaces.get(1).expect("second workspace").id;
        assert_eq!(chrome.borrow().tree().active, second_id);
    }

    #[test]
    fn accessibility_children_have_stable_semantics_and_synthetic_parity() {
        let tree = two_workspace_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        lay_out_and_draw(&mut bar, 1280.0);
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_CENTER_MONITOR.to_string());

        let node = bar.accessibility().expect("topbar semantics");
        assert_eq!(node.role, ui::a11y::Role::Toolbar);
        assert_eq!(node.semantic_key.as_deref(), Some("topbar"));
        assert_eq!(node.name.as_deref(), Some("Desktop navigation"));
        assert!(node.state.focusable);
        assert!(node.state.focused);

        for target in bar.control_targets() {
            let child = node
                .children
                .iter()
                .find(|child| child.action_target.as_deref() == Some(target))
                .unwrap_or_else(|| panic!("missing semantic child {target}"));
            let expected_name = bar.control_name(target);
            assert_eq!(child.name.as_deref(), Some(expected_name.as_str()));
            assert!(child.state.focusable);
            assert!(child.bounds.is_some(), "{target} needs a hit target");
            let bounds = child.bounds.expect("checked above").to_rect();
            assert!(
                bounds.width() > 0.0 && bounds.height() > 0.0,
                "{target}: {bounds:?}"
            );
            assert_eq!(child.semantic_key.as_deref(), Some(target));
        }

        let up = node
            .children
            .iter()
            .find(|child| {
                child.action_target.as_deref() == Some(TopBarChrome::CONTROL_WORKSPACE_UP)
            })
            .expect("workspace up semantics");
        assert_eq!(up.role, ui::a11y::Role::Button);
        assert!(up.state.disabled);
        assert_eq!(up.description.as_deref(), Some("Workspace 1 of 2"));

        let monitor = node
            .children
            .iter()
            .find(|child| {
                child.action_target.as_deref() == Some(TopBarChrome::CONTROL_CENTER_MONITOR)
            })
            .expect("monitor semantics");
        assert_eq!(monitor.role, ui::a11y::Role::ToggleButton);
        assert_eq!(monitor.state.checked, Some(false));
        assert!(monitor.state.focused);

        let monitor_index = node
            .children
            .iter()
            .position(|child| {
                child.action_target.as_deref() == Some(TopBarChrome::CONTROL_CENTER_MONITOR)
            })
            .expect("monitor child index");
        assert_eq!(
            bar.synthetic_accessibility_rect(&[monitor_index]),
            monitor.bounds.map(ui::a11y::AccessBounds::to_rect)
        );
        assert!(bar.focus_synthetic_accessibility(&[monitor_index]));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_CENTER_MONITOR)
        );

        let mut doc = AutoCommit::new();
        assert!(bar.perform_semantic_action(
            TopBarChrome::CONTROL_CENTER_CALENDAR,
            &ui::a11y::Action::ToggleCheck,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(bar.center_widget, CenterWidget::Calendar);
    }

    #[test]
    fn the_tilo_mark_is_opt_in_leads_the_bar_and_asks_for_its_menu() {
        let tree = two_workspace_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut plain = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        lay_out_and_draw(&mut plain, 1280.0);
        assert!(
            !plain
                .control_targets()
                .contains(&TopBarChrome::CONTROL_TILO_MARK)
        );
        assert_eq!(plain.tilo_mark_rect(), Rect::ZERO);

        let mut bar = TopBarChrome::from_tree(&tree)
            .with_chrome(chrome.clone())
            .with_tilo_mark();
        lay_out_and_draw(&mut bar, 1280.0);
        assert_eq!(
            bar.control_targets().first().copied(),
            Some(TopBarChrome::CONTROL_TILO_MARK)
        );
        let mark = bar.tilo_mark_rect();
        assert!(
            mark.width() >= BAR_CONTROL_PX && mark.height() >= BAR_CONTROL_PX,
            "{mark:?}"
        );
        assert!(mark.x1 <= bar.overview_btn_rect.x0, "the mark leads");
        assert!(
            bar.test_rects()
                .iter()
                .any(|(id, rect)| id == "topbar_tilo_mark" && *rect == mark)
        );
        let node = bar.accessibility().expect("topbar semantics");
        let child = node
            .children
            .iter()
            .find(|child| child.action_target.as_deref() == Some(TopBarChrome::CONTROL_TILO_MARK))
            .expect("the mark is a semantic child");
        assert_eq!(child.name.as_deref(), Some("Tilo menu"));
        assert_eq!(child.role, ui::a11y::Role::Button);

        bar.last_cursor = Some(mark.center());
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        assert!(handled);
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_TILO_MARK.to_string());
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
        let asked = chrome
            .borrow()
            .intents
            .iter()
            .filter(|intent| matches!(intent, ChromeIntent::ToggleTiloMenu))
            .count();
        assert_eq!(asked, 2, "a click and Enter each ask once");
    }

    /// The bar a browser shows a room with: the mark, the workspace's name
    /// as one menu, desks only when there are several, and the person's own
    /// avatar opening their profile card.
    #[test]
    fn the_workspace_menu_bar_leads_with_one_menu_and_ends_with_you() {
        let tree = two_workspace_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        chrome.borrow_mut().own = Some(crate::chrome::OwnChip {
            name: "Kim Lee".into(),
            rgb: (200, 120, 90),
        });
        chrome.borrow_mut().strip_places =
            Some(crate::chrome::StripPlaces::in_active_workspace(&tree, &[]));
        let mut bar = TopBarChrome::from_tree(&tree)
            .with_chrome(chrome.clone())
            .with_tilo_mark()
            .with_workspace_menu();
        bar.set_host_label(Some("tilo.coffee".into()));
        lay_out_and_draw(&mut bar, 1280.0);

        let targets = bar.control_targets();
        for gone in [
            TopBarChrome::CONTROL_OVERVIEW,
            TopBarChrome::CONTROL_WORKSPACE_UP,
            TopBarChrome::CONTROL_WORKSPACE_DOWN,
        ] {
            assert!(!targets.contains(&gone), "{gone} is in the workspace menu");
        }
        assert_eq!(
            &targets[..2],
            &[
                TopBarChrome::CONTROL_TILO_MARK,
                TopBarChrome::CONTROL_WORKSPACE_SETTINGS
            ]
        );
        assert!(targets.contains(&TopBarChrome::CONTROL_PROFILE));
        let mark = bar.tilo_mark_rect();
        let menu = bar.workspace_menu_rect();
        let own = bar.own_avatar_rect();
        assert!(mark.x1 < menu.x0, "the workspace follows the mark");
        assert!(
            menu.x0 - mark.x1 <= f64::from(ui::tokens::Spacing::BASE.s3),
            "with no host chip or switches between them"
        );
        assert!(own.x1 < bar.tray_hit_rect.x0, "you, then quick settings");
        assert!(own.x0 > bar.center_rect.x1, "on the right");
        assert_eq!(bar.ws_gear_rect.width(), SMALL_GLYPH_PX);
        let style = Theme::default().tokens.typography.style(TextRole::Caption);
        assert_eq!(
            bar.workspace_label(&mut TextRenderer::new(), &style, 400.0),
            "A",
            "no position count: the menu lists the others"
        );

        let node = bar.accessibility().expect("topbar semantics");
        let named = |target: &str| {
            node.children
                .iter()
                .find(|child| child.action_target.as_deref() == Some(target))
                .and_then(|child| child.name.clone())
        };
        assert_eq!(
            named(TopBarChrome::CONTROL_PROFILE).as_deref(),
            Some("Kim Lee: your profile")
        );
        assert!(
            named(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
                .is_some_and(|name| name.starts_with("A:"))
        );

        for at in [menu.center(), own.center()] {
            bar.last_cursor = Some(at);
            let mut handled = false;
            bar.handle_left_click(&mut handled);
            assert!(handled);
        }
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS.to_string());
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
        let intents = chrome.borrow().intents.clone();
        assert_eq!(
            intents,
            [
                ChromeIntent::ToggleWorkspaceMenu,
                ChromeIntent::ToggleProfile,
                ChromeIntent::ToggleWorkspaceMenu
            ]
        );
    }

    #[test]
    fn a_room_host_is_named_beside_the_workspace_and_nothing_on_ones_own() {
        let tree = two_workspace_tree();
        let mut own = TopBarChrome::from_tree(&tree);
        own.set_host_label(Some(String::new()));
        lay_out_and_draw(&mut own, 1280.0);
        let mut hosted = TopBarChrome::from_tree(&tree);
        hosted.set_host_label(Some("example.org".into()));
        lay_out_and_draw(&mut hosted, 1280.0);
        assert!(
            hosted.ws_pill_rect.x0 > own.ws_pill_rect.x0,
            "the host pill sits before the workspace pill"
        );
        let described = |bar: &TopBarChrome| {
            bar.accessibility()
                .expect("topbar semantics")
                .children
                .into_iter()
                .find(|child| {
                    child.action_target.as_deref() == Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
                })
                .and_then(|child| child.description)
        };
        assert_eq!(described(&hosted).as_deref(), Some("Hosted at example.org"));
        assert!(!described(&own).is_some_and(|text| text.contains("Hosted")));
    }

    #[test]
    fn long_workspace_name_yields_to_the_center_lane() {
        for width in [800.0, 1280.0, 1600.0] {
            let mut bar = sample_chrome();
            bar.workspace_name =
                "Überaus ausführlicher Arbeitsbereich für lokalisierte Forschungsunterlagen"
                    .to_string();
            lay_out_and_draw(&mut bar, width);
            assert!(
                bar.ws_pill_rect.x1 + 12.0 <= bar.center_rect.x0,
                "workspace pill {:?} must not collide with centre {:?} at {width}px",
                bar.ws_pill_rect,
                bar.center_rect,
            );
        }
    }

    #[test]
    fn narrow_topbar_releases_center_width_for_workspace_identity() {
        let mut short = sample_chrome();
        lay_out_and_draw(&mut short, 800.0);
        assert_eq!(short.center_lane.width(), 220.0);

        let mut long = sample_chrome();
        long.workspace_name =
            "Überaus ausführlicher Arbeitsbereich für lokalisierte Forschungsunterlagen"
                .to_string();
        lay_out_and_draw(&mut long, 800.0);
        assert!(
            long.ws_pill_rect.width() > short.ws_pill_rect.width(),
            "narrow workspace identity should retain more than the position suffix: short={:?}, long={:?}",
            short.ws_pill_rect,
            long.ws_pill_rect,
        );
        assert!(
            long.ws_pill_rect.x1 + 12.0 <= long.center_rect.x0,
            "workspace pill {:?} must keep its breathing room from centre {:?}",
            long.ws_pill_rect,
            long.center_rect,
        );
    }

    #[test]
    fn narrow_large_text_keeps_workspace_and_right_clusters_separate() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0]
            .collaborators
            .push(tilo_desktop_model::Collaborator::new("alice", "Alice"));
        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default().with_text_scale(1.5);
        lay_out_and_draw_with_theme(&mut bar, 800.0, &theme);

        assert!(bar.center_rect.width() < 360.0);
        assert!(bar.ws_pill_rect.x1 + 12.0 <= bar.center_rect.x0);
        assert!(bar.center_rect.x1 + 12.0 <= bar.avatar_stack_rect.x0);
        assert!(bar.avatar_stack_rect.x1 + 8.0 <= bar.tray_pill_rect.x0);
    }

    /// The widget's content is centred in its band: equal insets either
    /// side, so the time does not sit off the band's middle.
    #[test]
    fn center_content_is_centred_in_its_band() {
        let band = Rect::new(620.0, 2.0, 980.0, 22.0);
        let (x0, x1) = TopBarChrome::center_content_lane(band);
        assert!((f64::midpoint(x0, x1) - band.center().x).abs() < 1e-9);
        assert!(x0 > band.x0 && x1 < band.x1);
    }

    /// The clock's pill hugs the time instead of spanning the 360 px lane,
    /// keeps the time on its centre and holds its width as the minutes
    /// change; the widgets that fill the lane keep it, and the bar's
    /// neighbours lay out against the lane either way.
    #[test]
    fn the_clock_pill_hugs_the_time_and_holds_its_width() {
        let pill_at = |clock: &str, widget: CenterWidget| {
            let mut bar = sample_chrome();
            bar.clock = ClockSnapshot {
                clock: clock.into(),
                ..ClockSnapshot::default()
            };
            bar.center_widget = widget;
            lay_out_and_draw(&mut bar, 1600.0);
            (bar.center_rect, bar.center_lane, bar.ws_pill_rect)
        };
        let (clock, lane, ws) = pill_at("11:11", CenterWidget::Clock);
        assert_eq!(lane.width(), 360.0);
        assert!(
            clock.width() < lane.width() / 2.0,
            "a time is not a 360 px pill: {clock:?}"
        );
        assert!((clock.center().x - lane.center().x).abs() < 1e-9);
        let (later, _, _) = pill_at("20:08", CenterWidget::Clock);
        assert_eq!(
            later.width(),
            clock.width(),
            "the pill holds still minute to minute"
        );
        let (monitor, monitor_lane, monitor_ws) = pill_at("11:11", CenterWidget::SystemMonitor);
        assert_eq!(monitor, monitor_lane, "the system monitor fills the lane");
        assert_eq!(monitor_lane, lane);
        assert_eq!(
            monitor_ws, ws,
            "neighbours do not move when the widget changes"
        );
        let (x0, x1) = TopBarChrome::center_content_lane(clock);
        let mut text = TextRenderer::new();
        let theme = Theme::default();
        let (time, _) = text.measure_style(
            "11:11",
            None,
            &theme
                .tokens
                .typography
                .style(TextRole::Body)
                .with_weight(sample_chrome().clock_text_weight()),
        );
        assert!(x1 - x0 >= f64::from(time), "the time fits its content lane");
    }

    #[test]
    fn configured_carousel_order_drives_the_wheel_and_semantics() {
        let mut bar = sample_chrome();
        bar.widget_config = TopBarWidgets {
            widgets: vec![TopBarWidget::Calendar, TopBarWidget::Clock],
        };
        bar.center_widget = CenterWidget::Calendar;
        lay_out_and_draw(&mut bar, 1280.0);

        bar.cycle_center_widget(true);
        assert_eq!(bar.center_widget, CenterWidget::Clock);
        bar.cycle_center_widget(true);
        assert_eq!(bar.center_widget, CenterWidget::Calendar);
        bar.cycle_center_widget(false);
        assert_eq!(bar.center_widget, CenterWidget::Clock);

        let targets = bar.control_targets();
        let calendar = targets
            .iter()
            .position(|target| *target == TopBarChrome::CONTROL_CENTER_CALENDAR)
            .expect("configured calendar target");
        let clock = targets
            .iter()
            .position(|target| *target == TopBarChrome::CONTROL_CENTER_CLOCK)
            .expect("configured clock target");
        assert!(calendar < clock, "semantic order follows configured order");
        assert!(!targets.contains(&TopBarChrome::CONTROL_CENTER_MONITOR));
        assert!(bar.activate_control(TopBarChrome::CONTROL_CENTER_CALENDAR));
        assert_eq!(
            bar.center_widget,
            CenterWidget::Calendar,
            "a semantic choice switches the widget"
        );
    }

    #[test]
    fn new_activity_temporarily_overrides_but_does_not_replace_the_chosen_widget() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        bar.center_widget = CenterWidget::Calendar;

        chrome.borrow_mut().publish_activity_glance(
            Some("Ada · Prepared launch brief".to_string()),
            3,
            42,
            Some(crate::chrome::ActivityFocus {
                task_id: None,
                author: Some("Ada".into()),
                group_id: Some("issue:42".into()),
                entry_id: None,
                resource_id: None,
                resource_version: None,
            }),
        );
        bar.refresh_status_and_media_state();
        assert!(bar.activity_is_visible());
        assert_eq!(bar.center_widget(), CenterWidget::Calendar);

        chrome.borrow_mut().publish_activity_glance(
            Some("Ada · Prepared launch brief".to_string()),
            0,
            42,
            None,
        );
        bar.refresh_status_and_media_state();
        assert!(!bar.activity_is_visible());
        assert_eq!(bar.center_widget(), CenterWidget::Calendar);
    }

    #[test]
    fn clicking_a_person_opens_people_not_history() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        let chip = Rect::new(10.0, 10.0, 30.0, 30.0);
        bar.avatar_stack_rect = chip;
        bar.last_cursor = Some(chip.center());

        let mut handled = false;
        bar.handle_left_click(&mut handled);
        assert!(handled);
        assert!(chrome.borrow().overlays.collaborators);
        assert!(
            !chrome.borrow().overlays.room_activity,
            "a presence face represents People; activity has its own explicit route"
        );
    }

    /// A strip whose windows show the given foreign surfaces under the
    /// given titles, so sharing disclosure can name them.
    fn shared_windows(surfaces: &[(u64, &str)]) -> Strip {
        let mut strip = Strip::new(tilo_desktop_model::StripId(2), "Main");
        for (surface, title) in surfaces {
            strip.window_groups.push(WindowGroup {
                id: surface.saturating_add(1000),
                tabs: vec![Tab {
                    id: *surface,
                    title: (*title).into(),
                    tile: TileContent::Foreign {
                        app_id: "so.tilo.editor".into(),
                        surface_id: *surface,
                    },
                }],
                active_tab: *surface,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        strip
    }

    #[test]
    fn live_share_is_persistent_visible_named_and_accessible() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0]
            .collaborators
            .push(tilo_desktop_model::Collaborator::new("ada", "Ada").with_session(7));
        tree.workspaces[0]
            .strips
            .push(shared_windows(&[(42, "Budget")]));
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        chrome
            .borrow_mut()
            .set_active_exposures(vec![crate::chrome::ActiveExposure {
                peer: 7,
                surface: 42,
                can_control: true,
            }]);
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        lay_out_and_draw(&mut bar, 1600.0);

        assert!(
            !bar.sharing_indicator_rect.is_zero_area(),
            "an active exposure must paint trusted chrome"
        );
        let node = bar.accessibility().expect("topbar semantics");
        assert!(node.children.iter().any(|child| {
            child.action_target.as_deref() == Some(TopBarChrome::CONTROL_COLLABORATORS)
                && child.name.as_deref().is_some_and(|name| {
                    name.contains("visible to Ada (person): can control Budget")
                })
        }));
    }

    #[test]
    fn disclosure_attributes_each_surface_to_its_actual_viewer() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0].collaborators.extend([
            tilo_desktop_model::Collaborator::new("ada", "Ada").with_session(7),
            tilo_desktop_model::Collaborator::new("lin", "Lin").with_session(9),
        ]);
        tree.workspaces[0].strips.push(shared_windows(&[
            (42, "Notes"),
            (43, "Budget"),
            (44, "Slides"),
        ]));
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        chrome.borrow_mut().set_active_exposures(vec![
            crate::chrome::ActiveExposure {
                peer: 7,
                surface: 42,
                can_control: false,
            },
            crate::chrome::ActiveExposure {
                peer: 9,
                surface: 43,
                can_control: true,
            },
            crate::chrome::ActiveExposure {
                peer: 7,
                surface: 44,
                can_control: true,
            },
        ]);
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        let disclosure = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
        assert!(
            disclosure.contains("Ada (person): can control Slides; view only Notes"),
            "{disclosure}"
        );
        assert!(
            disclosure.contains("Lin (person): can control Budget"),
            "{disclosure}"
        );
        assert!(!disclosure.contains("Ada (person): can control Budget"));
        assert!(!disclosure.contains("Lin (person): can control Notes"));
    }

    #[test]
    fn agent_viewer_is_never_described_as_a_person() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let workspace = &mut tree.workspaces[0];
        workspace.set_owner("owner");
        assert!(
            !crate::workspace_settings::admit_agent(
                workspace,
                &"owner".to_string(),
                "scribe",
                crate::workspace_settings::AgentPreset::Operator,
            )
            .is_empty(),
            "owner admits agent"
        );
        workspace.collaborators.push(
            tilo_desktop_model::Collaborator::new("scribe", "Release Notes Agent").with_session(9),
        );
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        chrome
            .borrow_mut()
            .set_active_exposures(vec![crate::chrome::ActiveExposure {
                peer: 9,
                surface: 42,
                can_control: false,
            }]);
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);

        let name = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
        assert!(
            name.contains("Release Notes Agent (agent): view only a window"),
            "{name}"
        );
        assert!(!name.contains("Release Notes Agent (person"), "{name}");
        assert_eq!(
            bar.sharing_label().as_deref(),
            Some("Agent watching: Release Notes Agent"),
            "critical privacy state must precede the elidable display name"
        );
    }

    #[test]
    fn disclosure_keeps_identity_and_target_when_user_switches_workspaces() {
        let mut tree = two_workspace_tree();
        let shared = &mut tree.workspaces[1];
        shared.name = "Private".into();
        shared.set_owner("owner");
        assert!(
            !crate::workspace_settings::admit_agent(
                shared,
                &"owner".to_string(),
                "scribe",
                crate::workspace_settings::AgentPreset::Operator,
            )
            .is_empty()
        );
        shared
            .collaborators
            .push(tilo_desktop_model::Collaborator::new("scribe", "scribe").with_session(9));
        let mut strip = Strip::new(tilo_desktop_model::StripId(2), "Main");
        let tab_id = 42;
        strip.window_groups.push(WindowGroup {
            id: 43,
            tabs: vec![Tab {
                id: tab_id,
                title: "Draft".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.editor".into(),
                    surface_id: 42,
                },
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        shared.strips.push(strip);

        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        chrome
            .borrow_mut()
            .set_active_exposures(vec![crate::chrome::ActiveExposure {
                peer: 9,
                surface: 42,
                can_control: true,
            }]);
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        let name = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
        assert!(
            name.contains("scribe (agent): can control Private › Draft"),
            "{name}"
        );
        assert!(!name.contains("unidentified session"), "{name}");
    }

    /// Status hues mean something is wrong or happening (HIG 9.4); a
    /// resting readout's labels are ink.
    #[test]
    fn system_monitor_metric_labels_are_neutral_ink_not_status_colour() {
        let chrome = sample_chrome();
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high_contrast", Theme::high_contrast()),
        ] {
            let colors = &theme.tokens.colors;
            let sem = &colors.semantic;
            let loud = [
                ("success", sem.success),
                ("info", sem.info),
                ("warning", sem.warning),
                ("danger", sem.danger),
                ("your colour", colors.intent.you),
            ];
            for k in [MetricKind::Cpu, MetricKind::Ram, MetricKind::Tmp] {
                let c = chrome.metric_label_color(&theme, k);
                assert_eq!(
                    c.to_rgba8().to_u32(),
                    colors.text_dim.to_rgba8().to_u32(),
                    "{name}: {k:?} is secondary ink"
                );
                for (what, colour) in loud {
                    assert_ne!(
                        c.to_rgba8().to_u32(),
                        colour.to_rgba8().to_u32(),
                        "{name}: {k:?} label wears {what}"
                    );
                }
            }
        }
    }

    #[test]
    fn sparkline_shares_the_cpu_colour_at_a_backdrop_weight() {
        let chrome = sample_chrome();
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high_contrast", Theme::high_contrast()),
        ] {
            let cpu = chrome.metric_label_color(&theme, MetricKind::Cpu);
            let fill = chrome.sparkline_fill(&theme, MetricKind::Cpu);
            let rgb = |c: Color| {
                let p = c.to_rgba8();
                (p.r, p.g, p.b)
            };
            assert_eq!(
                rgb(fill),
                rgb(cpu),
                "{name}: sparkline ink drifted off the labels"
            );
            assert!(
                fill.components[3] < cpu.components[3],
                "{name}: the sparkline must sit behind the numbers, not compete with them"
            );
            assert_ne!(
                fill.to_rgba8().to_u32(),
                theme.tokens.colors.intent.inverted.to_rgba8().to_u32(),
                "{name}: sparkline reuses the accent"
            );
        }
    }

    #[test]
    fn avatar_stack_has_breathing_room_to_tray_pill() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0]
            .collaborators
            .push(tilo_desktop_model::Collaborator::new("alice", "Alice"));

        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(1600.0, TOPBAR_HEIGHT_PX),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let avatar = bar.avatar_stack_rect;
        let tray = bar.tray_pill_rect();
        assert!(
            avatar.width() > 0.0,
            "test seeded one collaborator but avatar_stack_rect is empty",
        );
        let gap = tray.x0 - avatar.x1;
        assert!(
            gap >= 8.0,
            "avatar→tray gap must be ≥ 8 px (got {gap:.2}); avatar.x1={}, tray.x0={}",
            avatar.x1,
            tray.x0,
        );
    }

    #[test]
    fn empty_roster_restores_the_clickable_invite_affordance() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let chrome = super::fixture::FixtureChrome::new(tree);
        bar = bar.with_chrome(chrome.clone());

        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(1600.0, 40.0),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        assert!(
            bar.collaborator_initials().is_empty() && bar.collaborator_overflow() == 0,
            "this test is about the empty case and the roster is not empty",
        );
        assert!(
            bar.avatar_stack_rect.area() > 0.0,
            "an empty room still needs a People entry point for the first invite",
        );
        assert!(!chrome.borrow().overlays.collaborators);

        bar.last_cursor = Some(bar.avatar_stack_rect.center());
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        assert!(handled);
        assert!(chrome.borrow().overlays.collaborators);

        let node = bar.accessibility().expect("topbar semantics");
        let (index, invite) = node
            .children
            .iter()
            .enumerate()
            .find(|(_, child)| child.name.as_deref() == Some("Invite"))
            .expect("assistive technology reaches the Invite control");
        assert_eq!(invite.role, ui::a11y::Role::Button);
        assert_eq!(
            bar.synthetic_accessibility_rect(&[index]),
            Some(bar.avatar_stack_rect)
        );
        let target = invite.action_target.clone().expect("an action target");
        let mut doc = AutoCommit::new();
        assert!(bar.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            !chrome.borrow().overlays.collaborators,
            "activating Invite toggles the same overlay a click does",
        );
    }

    #[test]
    fn spectators_and_marked_agents_read_in_the_avatar_stack() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let paint = |bar: &mut TopBarChrome| {
            let theme = Theme::default();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
            let vp = taffy
                .new_with_children(
                    Style {
                        size: Size::from_lengths(1600.0, 40.0),
                        ..Default::default()
                    },
                    &[node],
                )
                .unwrap();
            taffy
                .compute_layout_with_measure(
                    vp,
                    Size {
                        width: taffy::AvailableSpace::Definite(1600.0),
                        height: taffy::AvailableSpace::Definite(40.0),
                    },
                    |_, _, _, _, _| taffy::Size::ZERO,
                )
                .unwrap();
            bar.apply_layout(&taffy, node, Point::ORIGIN);
            bar.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::default(),
            );
        };

        // Spectators alone: the chip is the People control, not Invite.
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        chrome.borrow_mut().watching = 2;
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        paint(&mut bar);
        assert!(bar.collaborator_initials().is_empty());
        assert!(bar.avatar_stack_rect.height() >= BAR_CONTROL_PX);
        let node = bar.accessibility().expect("topbar semantics");
        assert!(
            node.children
                .iter()
                .all(|child| child.name.as_deref() != Some("Invite")),
            "an audience is not an empty room"
        );
        assert!(
            node.children.iter().any(|child| child
                .name
                .as_deref()
                .is_some_and(|name| name.contains("2 people watching"))),
            "the bar names its spectators"
        );
        bar.last_cursor = Some(bar.avatar_stack_rect.center());
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        assert!(handled && chrome.borrow().overlays.collaborators);

        // A hosted roster marks software by role id; it draws as an agent.
        let mut agent = tilo_desktop_model::Collaborator::new("scribe", "Scribe");
        agent.role_id = Some(crate::collaborators::AGENT_ROLE_MARKER.into());
        tree.workspaces[0].collaborators =
            vec![agent, tilo_desktop_model::Collaborator::new("ada", "Ada")];
        let mut bar = TopBarChrome::from_tree(&tree);
        bar.sync_from_tree(&tree);
        assert_eq!(bar.collaborator_agents, vec![true, false]);
    }

    #[test]
    fn compact_topbar_aligns_floating_controls_and_keeps_the_corner_target() {
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(800.0, TOPBAR_HEIGHT_PX),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let bar_bottom = bar.rect.y1;
        assert!(
            (bar_bottom - f64::from(TOPBAR_HEIGHT_PX)).abs() < 0.5,
            "topbar y1 ({bar_bottom}) should equal TOPBAR_HEIGHT_PX ({TOPBAR_HEIGHT_PX})",
        );
        assert_eq!(TOPBAR_HEIGHT_PX, 24.0);
        for (label, rect) in [
            ("overview", bar.overview_btn_rect),
            ("workspace", bar.ws_pill_rect),
            ("center", bar.center_rect),
            ("tray", bar.tray_pill_rect),
        ] {
            assert_eq!(rect.y0 - bar.rect.y0, 2.0, "{label} top inset");
            assert_eq!(bar.rect.y1 - rect.y1, 2.0, "{label} bottom inset");
        }
        assert_eq!(bar.tray_hit_rect.y0, bar.rect.y0);
        assert_eq!(bar.tray_hit_rect.y1, bar.rect.y1);
        assert_eq!(bar.tray_hit_rect.x1, bar.rect.x1);
        assert!(bar.tray_hit_rect.contains(Point::new(799.0, 1.0)));
        assert!(!bar.tray_pill_rect.contains(Point::new(799.0, 1.0)));
    }

    /// Both states asserted together, since a stack that's empty (or
    /// full) in both cases would pass only half.
    #[test]
    fn a_present_collaborator_in_this_workspace_gets_an_avatar() {
        let empty = DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        let mut empty = empty;
        empty.active = empty.workspaces[0].id;
        let bar = TopBarChrome::from_tree(&empty);
        assert!(
            bar.collaborator_initials().is_empty(),
            "a workspace nobody else is in has nobody to show",
        );

        let mut elsewhere = empty.clone();
        elsewhere.collaborators = vec![tilo_desktop_model::Collaborator::new("alice", "Alice")];
        let bar = TopBarChrome::from_tree(&elsewhere);
        assert!(
            bar.collaborator_initials().is_empty(),
            "session-wide presence must not leak somebody from another workspace into this bar",
        );

        let mut joined = empty.clone();
        joined.workspaces[0].collaborators = vec![tilo_desktop_model::Collaborator::new(
            "peer@11895642303397647149",
            "Xaver",
        )];
        let bar = TopBarChrome::from_tree(&joined);
        assert_eq!(
            bar.collaborator_initials(),
            &['X'],
            "the bar must show somebody present in this workspace",
        );
        assert_eq!(
            bar.collaborator_overflow(),
            0,
            "one peer is not an overflow",
        );

        // Invited but not redeemed: an avatar here would falsely claim
        // he's in the workspace.
        let mut asked = empty;
        asked.workspaces[0].collaborators = vec![
            tilo_desktop_model::Collaborator::new("peer@11895642303397647149", "Xaver")
                .as_invited(),
        ];
        let bar = TopBarChrome::from_tree(&asked);
        assert!(
            bar.collaborator_initials().is_empty(),
            "somebody invited and not yet arrived is not an avatar in the \
             cluster: {:?}",
            bar.collaborator_initials(),
        );
        assert_eq!(
            bar.collaborator_overflow(),
            0,
            "and does not show up in the +N chip either",
        );
    }
    fn editor_key_event(
        key: winit::keyboard::NamedKey,
        state: winit::event::ElementState,
        repeat: bool,
    ) -> WindowEvent {
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.logical_key = winit::keyboard::Key::Named(key);
        event.text = None;
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = state;
        event.repeat = repeat;
        ui::synthetic_input::keyboard_input(event)
    }

    fn editor_pointer_event(point: Point) -> WindowEvent {
        ui::synthetic_input::cursor_moved(point.x, point.y)
    }

    fn editor_mouse_event(state: winit::event::ElementState) -> WindowEvent {
        ui::synthetic_input::mouse_input(state, winit::event::MouseButton::Left)
    }

    #[test]
    fn editor_production_render_matrix() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/topbar-editor-review");
        std::fs::create_dir_all(&out).expect("capture directory");
        for (name, width, height, theme) in [
            ("dark", 1280_u32, 800_u32, Theme::dark()),
            ("light", 1280, 800, Theme::light()),
            ("high-contrast", 1280, 800, Theme::high_contrast()),
            ("large-text", 1280, 800, Theme::dark().with_text_scale(2.0)),
            (
                "ui-text",
                1280,
                800,
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
            ),
            ("narrow", 320, 600, Theme::light()),
        ] {
            for (state, widgets) in [
                ("all", TopBarWidget::ALL.to_vec()),
                ("add", vec![TopBarWidget::Clock]),
            ] {
                let mut bar = sample_chrome();
                bar.clock = ClockSnapshot {
                    unix_seconds: 1_700_000_000,
                    clock: "09:41".into(),
                    weekday: "Wednesday".into(),
                    date: "7 October".into(),
                };
                bar.widget_config.widgets = widgets;
                bar.widget_editing = true;
                bar.set_focus(true);
                bar.focused_control = Some(
                    if state == "all" {
                        TopBarChrome::CONTROL_REMOVE_MONITOR
                    } else {
                        TopBarChrome::CONTROL_ADD_MONITOR
                    }
                    .into(),
                );
                lay_out_and_draw_with_theme(
                    &mut bar,
                    f32::from(u16::try_from(width).expect("fixture width fits")),
                    &theme,
                );
                let geometry = &bar.editor_geometry;
                assert!(geometry.panel.x0 >= 0.0 && geometry.panel.x1 <= f64::from(width));
                assert!(geometry.panel.y1 <= f64::from(height));
                assert!(geometry.title.y1 <= geometry.hint.y0);
                assert!(geometry.hint.y1 <= geometry.rows[0].y0);
                for (index, row) in geometry.rows.iter().enumerate() {
                    assert!(geometry.remove[index].x0 >= row.x0);
                    assert!(geometry.remove[index].x1 <= row.x1);
                    assert!(geometry.remove[index].y0 >= row.y0);
                    assert!(geometry.remove[index].y1 <= row.y1);
                    if index > 0 {
                        assert!(geometry.rows[index - 1].y1 <= row.y0);
                    }
                    if width == 1280 {
                        let label = TopBarChrome::editor_label(geometry.widgets[index]);
                        assert!(
                            !ListRowFace::new(&label)
                                .leading(RowLeading::Icon(ui::icons::Icon::Menu))
                                .trailing_width(geometry.remove[index].width())
                                .title_elided(*row, &mut TextRenderer::new(), &theme),
                            "{name}: configured widget labels fit"
                        );
                    }
                }
                let mut scene = Scene::new();
                bar.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &theme,
                    &PresenceCtx::default(),
                );
                let path = out.join(format!("{name}-{state}.png"));
                ui::offscreen::OffscreenRenderer::new(width, height).render_png(
                    &scene,
                    theme.tokens.colors.surfaces.base,
                    &path,
                );
                assert!(std::fs::metadata(path).expect("production PNG").len() > 1_000);
                assert!(!bar.widget_editor_rect.is_zero_area());
            }
        }
    }

    #[test]
    fn editor_escape_cancels_drag_before_a_later_release() {
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        lay_out_and_draw(&mut bar, 1280.0);
        let original = bar.widget_config.widgets.clone();
        let from = bar.widget_editor_row(0).center();
        let to = bar.widget_editor_row(2).center();
        dispatch_topbar_event(&mut bar, &editor_pointer_event(from));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(winit::event::ElementState::Pressed)
        ));
        dispatch_topbar_event(&mut bar, &editor_pointer_event(to));
        assert!(bar.widget_edit_drag.is_active());
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(
                winit::keyboard::NamedKey::Escape,
                winit::event::ElementState::Pressed,
                false,
            )
        ));
        assert!(!bar.widget_editing);
        assert!(
            !bar.wants_pointer_capture(),
            "cancel releases the shared drag"
        );
        dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(winit::event::ElementState::Released),
        );
        assert_eq!(bar.widget_config.widgets, original);
    }

    #[test]
    fn editor_activation_keeps_the_same_widget_selected() {
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_REMOVE_MONITOR.into());
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(
                winit::keyboard::NamedKey::Enter,
                winit::event::ElementState::Pressed,
                false,
            )
        ));
        assert!(
            !bar.widget_config
                .widgets
                .contains(&TopBarWidget::SystemMonitor)
        );
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_ADD_MONITOR),
            "focus follows the stable widget's new action, never the overview",
        );
    }
    #[test]
    fn editor_submit_repeats_and_release_do_not_toggle_the_replacement_action() {
        use winit::{event::ElementState as E, keyboard::NamedKey as K};
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_REMOVE_MONITOR.into());
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::Enter, E::Pressed, false)
        ));
        let removed = bar.widget_config.widgets.clone();
        for _ in 0..5 {
            assert!(dispatch_topbar_event(
                &mut bar,
                &editor_key_event(K::Enter, E::Pressed, true)
            ));
            assert_eq!(bar.widget_config.widgets, removed);
        }
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::Escape, E::Pressed, false)
        ));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::Enter, E::Released, false)
        ));
        assert!(!bar.widget_editing);
        assert_eq!(bar.widget_config.widgets, removed);
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::ContextMenu, E::Pressed, false)
        ));
        bar.focused_control = Some(TopBarChrome::CONTROL_ADD_MONITOR.into());
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::Enter, E::Pressed, false)
        ));
        assert!(
            bar.widget_config
                .widgets
                .contains(&TopBarWidget::SystemMonitor)
        );
    }

    #[test]
    fn editor_pointer_buttons_activate_on_release_and_cancel_outside() {
        use winit::event::ElementState as E;
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        lay_out_and_draw(&mut bar, 1280.0);
        let original = bar.widget_config.widgets.clone();
        let remove = bar.widget_editor_remove_rect(1).center();
        dispatch_topbar_event(&mut bar, &editor_pointer_event(remove));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(E::Pressed)
        ));
        assert_eq!(
            bar.widget_config.widgets, original,
            "a press alone does not delete"
        );
        assert!(bar.wants_pointer_capture());
        dispatch_topbar_event(&mut bar, &editor_pointer_event(Point::new(1.0, 400.0)));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(E::Released)
        ));
        assert_eq!(bar.widget_config.widgets, original);
        assert!(!bar.wants_pointer_capture());
        dispatch_topbar_event(&mut bar, &editor_pointer_event(remove));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(E::Pressed)
        ));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(E::Released)
        ));
        assert!(
            !bar.widget_config
                .widgets
                .contains(&TopBarWidget::SystemMonitor)
        );
        lay_out_and_draw(&mut bar, 1280.0);
        let add = bar
            .widget_editor_add_rect(TopBarWidget::SystemMonitor)
            .center();
        dispatch_topbar_event(&mut bar, &editor_pointer_event(add));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(E::Pressed)
        ));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(E::Released)
        ));
        assert!(
            bar.widget_config
                .widgets
                .contains(&TopBarWidget::SystemMonitor)
        );
        lay_out_and_draw(&mut bar, 1280.0);
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_REMOVE_MONITOR)
        );
    }

    #[test]
    fn editor_drag_commits_once_and_interrupted_drags_preserve_the_model() {
        use winit::event::ElementState as E;
        for interruption in ["release", "resize", "focus", "outside"] {
            let mut bar = sample_chrome();
            bar.widget_editing = true;
            lay_out_and_draw(&mut bar, 1280.0);
            let original = bar.widget_config.widgets.clone();
            let from = bar.widget_editor_row(0).center();
            let to = bar.widget_editor_row(2).center();
            dispatch_topbar_event(&mut bar, &editor_pointer_event(from));
            assert!(dispatch_topbar_event(
                &mut bar,
                &editor_mouse_event(E::Pressed)
            ));
            dispatch_topbar_event(&mut bar, &editor_pointer_event(to));
            assert!(bar.wants_pointer_capture());
            match interruption {
                "resize" => lay_out_and_draw(&mut bar, 640.0),
                "focus" => {
                    dispatch_topbar_event(&mut bar, &WindowEvent::Focused(false));
                }
                "outside" => {
                    dispatch_topbar_event(&mut bar, &editor_pointer_event(Point::new(1.0, 400.0)));
                    assert!(dispatch_topbar_event(
                        &mut bar,
                        &editor_mouse_event(E::Pressed)
                    ));
                    assert!(!bar.widget_editing);
                }
                _ => {}
            }
            dispatch_topbar_event(&mut bar, &editor_mouse_event(E::Released));
            let committed = bar.widget_config.widgets.clone();
            dispatch_topbar_event(&mut bar, &editor_mouse_event(E::Released));
            assert_eq!(
                bar.widget_config.widgets, committed,
                "release cannot commit twice"
            );
            assert!(!bar.wants_pointer_capture());
            if interruption == "release" {
                assert_eq!(
                    committed,
                    vec![
                        TopBarWidget::SystemMonitor,
                        TopBarWidget::Calendar,
                        TopBarWidget::Clock
                    ]
                );
            } else {
                assert_eq!(
                    committed, original,
                    "{interruption} cancels the pending reorder"
                );
            }
        }
    }

    #[test]
    fn editor_semantic_open_keyboard_navigation_and_last_remove_are_coherent() {
        use winit::{event::ElementState as E, keyboard::NamedKey as K};
        let mut bar = sample_chrome();
        lay_out_and_draw(&mut bar, 1280.0);
        let node = bar.accessibility().expect("actual semantic toolbar");
        let open = node
            .children
            .iter()
            .find(|n| n.action_target.as_deref() == Some(TopBarChrome::CONTROL_EDIT_WIDGETS))
            .expect("editor action exposed");
        assert!(open.bounds.expect("action bounds").to_rect().width() > 0.0);
        let mut doc = AutoCommit::new();
        assert!(bar.perform_semantic_action(
            TopBarChrome::CONTROL_EDIT_WIDGETS,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        bar.set_focus(true);
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::End, E::Pressed, false)
        ));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_REMOVE_CALENDAR)
        );
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::Home, E::Pressed, false)
        ));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_REMOVE_CLOCK)
        );
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::ArrowDown, E::Pressed, false)
        ));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_REMOVE_MONITOR)
        );
        bar.widget_config.widgets = vec![TopBarWidget::Clock];
        lay_out_and_draw(&mut bar, 1280.0);
        let disabled = bar.widget_editor_remove_rect(0).center();
        dispatch_topbar_event(&mut bar, &editor_pointer_event(disabled));
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_mouse_event(E::Pressed)
        ));
        assert!(
            !bar.wants_pointer_capture(),
            "disabled final Remove never starts a drag"
        );
        dispatch_topbar_event(&mut bar, &editor_mouse_event(E::Released));
        assert_eq!(bar.widget_config.widgets, vec![TopBarWidget::Clock]);
    }

    #[test]
    fn editor_modified_submit_and_orphan_repeats_preserve_widgets() {
        use winit::{
            event::ElementState as E,
            keyboard::{ModifiersState, NamedKey as K},
        };
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_REMOVE_MONITOR.into());
        lay_out_and_draw(&mut bar, 1280.0);
        let original = bar.widget_config.widgets.clone();
        for modifiers in [
            ModifiersState::SUPER,
            ModifiersState::CONTROL,
            ModifiersState::ALT,
        ] {
            dispatch_topbar_event_with_modifiers(
                &mut bar,
                &editor_key_event(K::Space, E::Pressed, false),
                modifiers.into(),
            );
            assert_eq!(bar.widget_config.widgets, original);
        }
        dispatch_topbar_event(&mut bar, &WindowEvent::Focused(false));
        bar.set_focus(true);
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::Enter, E::Pressed, true)
        ));
        assert_eq!(bar.widget_config.widgets, original);
        assert!(dispatch_topbar_event(
            &mut bar,
            &editor_key_event(K::Enter, E::Pressed, false)
        ));
        assert!(
            !bar.widget_config
                .widgets
                .contains(&TopBarWidget::SystemMonitor)
        );
    }

    #[test]
    fn editor_hidden_layout_releases_popup_capture() {
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(bar.modal_capture().is_some());
        let mut taffy = TaffyTree::new();
        let node = taffy
            .new_leaf(Style {
                size: Size::from_lengths(1280.0, 0.0),
                ..Default::default()
            })
            .expect("hidden top-bar leaf");
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(1280.0),
                    height: taffy::AvailableSpace::Definite(0.0),
                },
            )
            .expect("fullscreen hidden layout");
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        assert!(
            bar.modal_capture().is_none(),
            "a hidden bar cannot capture popup input"
        );
        assert!(!bar.traps_focus());
        assert!(!bar.widget_editing);
    }

    #[test]
    fn editor_external_widget_order_repaints_the_actual_scene() {
        let theme = Theme::dark();
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        lay_out_and_draw_with_theme(&mut bar, 1280.0, &theme);
        bar.widget_config.widgets.reverse();
        let mut cached = Scene::new();
        bar.draw(
            &mut cached,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::default(),
        );
        let mut renderer = ui::offscreen::OffscreenRenderer::new(1280, 480);
        let actual = renderer.render_resilient(&cached, theme.tokens.colors.surfaces.base);
        bar.cache_fp = CacheFingerprint::default();
        let mut fresh = Scene::new();
        bar.draw(
            &mut fresh,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::default(),
        );
        let expected = renderer.render_resilient(&fresh, theme.tokens.colors.surfaces.base);
        assert_eq!(
            ui::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected),
            0,
            "a host configuration update invalidates the actual retained pixels"
        );
    }

    #[test]
    fn editor_theme_swap_repaints_cached_scene_and_focus_reaches_actual_button_pixels() {
        let mut bar = sample_chrome();
        bar.widget_editing = true;
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_REMOVE_MONITOR.into());
        lay_out_and_draw_with_theme(&mut bar, 1280.0, &Theme::dark());
        let mut light = Scene::new();
        bar.draw(
            &mut light,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::light(),
            &PresenceCtx::default(),
        );
        let mut renderer = ui::offscreen::OffscreenRenderer::new(1280, 480);
        let actual = renderer.render_resilient(&light, Theme::light().tokens.colors.surfaces.base);
        bar.cache_fp = CacheFingerprint::default();
        let mut fresh = Scene::new();
        bar.draw(
            &mut fresh,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::light(),
            &PresenceCtx::default(),
        );
        let expected =
            renderer.render_resilient(&fresh, Theme::light().tokens.colors.surfaces.base);
        assert_eq!(
            ui::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected),
            0,
            "theme changes invalidate the retained scene"
        );
        let focus_rect = bar.widget_editor_remove_rect(1).inset(4.0);
        bar.set_focus(false);
        let mut unfocused = Scene::new();
        bar.draw(
            &mut unfocused,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::light(),
            &PresenceCtx::default(),
        );
        let no_focus =
            renderer.render_resilient(&unfocused, Theme::light().tokens.colors.surfaces.base);
        let changed = expected
            .chunks_exact(4)
            .zip(no_focus.chunks_exact(4))
            .enumerate()
            .filter(|(i, (a, b))| {
                let x = f64::from(u32::try_from(i % 1280).expect("pixel column"));
                let y = f64::from(u32::try_from(i / 1280).expect("pixel row"));
                focus_rect.contains(Point::new(x, y))
                    && a.iter().zip(b.iter()).any(|(a, b)| a.abs_diff(*b) > 1)
            })
            .count();
        assert!(
            changed > 30,
            "the focused shared face must paint visible pixels: {changed}"
        );
    }
    #[test]
    fn composed_narrow_editor_preserves_both_semantic_targets_and_interruptions() {
        use ui::a11y::Action;
        use winit::{event::ElementState as E, keyboard::NamedKey as K};
        for target in [
            TopBarChrome::CONTROL_EDIT_WIDGETS,
            TopBarChrome::CONTROL_WIDGET_EDITOR,
        ] {
            let mut frame = ui::headless::Framed::new(
                ui::State::new(),
                sample_chrome(),
                (320, 480),
                Theme::dark(),
            );
            let mut doc = AutoCommit::new();
            assert!(
                frame.perform_semantic_action(
                    target,
                    &Action::Activate,
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "original editor target remains admitted at narrow width: {target}"
            );
            ui::headless::Framed::harness_mut(&mut frame)
                .poll_frame()
                .unwrap();
            assert!(frame.widget_editing);
            assert!(frame.traps_focus());
            let original = frame.widget_config.widgets.clone();
            let from = frame.widget_editor_row(0).center();
            let to = frame.widget_editor_row(2).center();
            dispatch_topbar_event(&mut frame, &editor_pointer_event(from));
            dispatch_topbar_event(&mut frame, &editor_mouse_event(E::Pressed));
            dispatch_topbar_event(&mut frame, &editor_pointer_event(to));
            assert!(frame.wants_pointer_capture());
            dispatch_topbar_event(&mut frame, &WindowEvent::Focused(false));
            dispatch_topbar_event(&mut frame, &editor_mouse_event(E::Released));
            assert!(!frame.wants_pointer_capture());
            assert_eq!(frame.widget_config.widgets, original);
            dispatch_topbar_event(&mut frame, &editor_key_event(K::Escape, E::Pressed, false));
            ui::headless::Framed::harness_mut(&mut frame)
                .poll_frame()
                .unwrap();
            assert!(!frame.widget_editing);
            assert!(!frame.traps_focus());
            assert!(!frame.responsive.toolbar.is_overflow_open());
        }
    }
    #[test]
    fn composed_narrow_editor_theme_scale_capture_matrix() {
        use ui::{a11y::Action, headless::Framed};
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/combined-topbar-shots");
        std::fs::create_dir_all(&out).expect("capture directory");
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high-contrast", Theme::high_contrast()),
            ("large-text", Theme::dark().with_text_scale(2.0)),
            (
                "ui-text-scaled",
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
            ),
        ] {
            let mut frame = Framed::new(ui::State::new(), sample_chrome(), (320, 600), theme);
            let mut doc = AutoCommit::new();
            assert!(frame.perform_semantic_action(
                TopBarChrome::CONTROL_EDIT_WIDGETS,
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert!(frame.responsive.active);
            assert!(frame.widget_editing);
            assert!(frame.modal_capture().is_some());
            let geometry = &frame.editor_geometry;
            assert!(geometry.panel.x0 >= 0.0 && geometry.panel.x1 <= 320.0);
            assert!(geometry.panel.y0 >= 0.0 && geometry.panel.y1 <= 600.0);
            assert!(geometry.title.y1 <= geometry.hint.y0);
            assert!(geometry.hint.y1 <= geometry.rows[0].y0);
            for ((row, remove), widget) in geometry
                .rows
                .iter()
                .zip(&geometry.remove)
                .zip(&geometry.widgets)
            {
                assert!(
                    !TopBarChrome::editor_row_face(
                        &TopBarChrome::editor_label(*widget),
                        remove.width(),
                        geometry.compact_rows,
                    )
                    .title_elided(
                        *row,
                        &mut TextRenderer::new(),
                        &frame.responsive.theme
                    ),
                    "{name}: configured widget labels stay readable at narrow scale"
                );
                assert!(remove.x0 >= row.x0 && remove.x1 <= row.x1);
                assert!(remove.y0 >= row.y0 && remove.y1 <= row.y1);
                assert!(remove.width() > 0.0 && remove.height() > 0.0);
            }
            assert!(Framed::harness(&frame).native_app_audit().is_certified());
            let path = out.join(format!("{name}-narrow-editor.png"));
            Framed::harness(&frame).render_png(&path);
            assert!(path.is_file(), "missing actual composed editor capture");
        }
    }
}

#[cfg(test)]
mod fixture {
    use super::*;
    use crate::chrome::*;
    use std::{
        cell::{Ref, RefCell, RefMut},
        rc::Rc,
    };
    #[derive(Default)]
    #[expect(
        clippy::struct_excessive_bools,
        reason = "test fixture mirrors independent overlay toggles"
    )]
    pub struct Overlays {
        pub overview: bool,
        pub collaborators: bool,
        pub room_activity: bool,
        pub ws_settings: bool,
        pub clock: bool,
    }
    pub struct State {
        pub tree: DesktopTree,
        pub overlays: Overlays,
        pub glance: ActivityGlance,
        pub clock_activity: Option<ClockActivity>,
        pub active_exposures: Vec<ActiveExposure>,
        pub watching: u32,
        pub intents: Vec<ChromeIntent>,
        pub top_bar: TopBarPresence,
        pub own: Option<OwnChip>,
        pub strip_places: Option<StripPlaces>,
    }
    impl State {
        pub fn tree(&self) -> &DesktopTree {
            &self.tree
        }
        pub fn publish_activity_glance(
            &mut self,
            headline: Option<String>,
            unread: usize,
            pulse: u64,
            focus: Option<ActivityFocus>,
        ) {
            self.glance = ActivityGlance {
                headline,
                unread,
                pulse,
                focus,
            };
        }
        pub fn set_active_exposures(&mut self, mut exposures: Vec<ActiveExposure>) {
            exposures.sort_unstable();
            exposures.dedup();
            self.active_exposures = exposures;
        }
    }
    pub struct FixtureChrome(RefCell<State>);
    impl FixtureChrome {
        pub fn new(tree: DesktopTree) -> Rc<Self> {
            Rc::new(Self(RefCell::new(State {
                tree,
                overlays: Overlays::default(),
                glance: ActivityGlance::default(),
                clock_activity: None,
                active_exposures: Vec::new(),
                watching: 0,
                intents: Vec::new(),
                top_bar: TopBarPresence::Docked,
                own: None,
                strip_places: None,
            })))
        }
        pub fn borrow(&self) -> Ref<'_, State> {
            self.0.borrow()
        }
        pub fn borrow_mut(&self) -> RefMut<'_, State> {
            self.0.borrow_mut()
        }
    }
    impl DesktopChromeSource for FixtureChrome {
        fn tree(&self) -> DesktopTree {
            self.0.borrow().tree.clone()
        }
        fn snapshot(&self) -> DesktopChromeSnapshot {
            let state = self.0.borrow();
            DesktopChromeSnapshot {
                overlays: ChromeOverlays {
                    overview: state.overlays.overview,
                    ws_settings: state.overlays.ws_settings,
                    clock: state.overlays.clock,
                },
                activity_glance: state.glance.clone(),
                clock_activity: state.clock_activity.clone(),
                active_exposures: state.active_exposures.clone(),
                watching: state.watching,
                top_bar: state.top_bar,
                own: state.own.clone(),
                strip_places: state.strip_places.clone(),
                ..Default::default()
            }
        }
        fn intent(&self, intent: ChromeIntent) -> bool {
            let mut state = self.0.borrow_mut();
            state.intents.push(intent.clone());
            match intent {
                ChromeIntent::ToggleOverview => state.overlays.overview = !state.overlays.overview,
                ChromeIntent::ToggleCollaborators => {
                    state.overlays.collaborators = !state.overlays.collaborators;
                }
                // History opens as a tab the host places; no overlay moves.
                ChromeIntent::ShowActivity(_) => state.overlays.collaborators = false,
                ChromeIntent::HideClock => state.overlays.clock = false,
                ChromeIntent::SwitchWorkspace(delta) => {
                    let Some(index) = state
                        .tree
                        .workspaces
                        .iter()
                        .position(|w| w.id == state.tree.active)
                    else {
                        return false;
                    };
                    let Some(id) = i64::try_from(index)
                        .ok()
                        .and_then(|index| index.checked_add(i64::from(delta)))
                        .and_then(|next| usize::try_from(next).ok())
                        .and_then(|i| state.tree.workspaces.get(i))
                        .map(|w| w.id)
                    else {
                        return false;
                    };
                    state.tree.active = id;
                }
                _ => {}
            }
            true
        }
    }
}

#[cfg(test)]
mod responsive_tests {
    use super::*;
    use ui::headless::Framed;

    fn fixture_bar() -> TopBarChrome {
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(1), "Design review");
        let tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let mut bar = TopBarChrome::from_tree_with_local_user(&tree, "fixture-person".into());
        bar.clock = ClockSnapshot {
            clock: "09:41".into(),
            ..ClockSnapshot::default()
        };
        bar
    }

    #[test]
    fn responsive_primary_bar_visible_targets_do_not_overlap_at_320() {
        let frame = Framed::new(ui::State::new(), fixture_bar(), (320, 480), Theme::dark());
        let bar = &*frame;
        let visible: Vec<_> = bar
            .control_targets()
            .into_iter()
            .map(|target| (target, bar.control_rect(target)))
            .filter(|(_, rect)| !rect.is_zero_area())
            .collect();
        assert!(
            !visible.is_empty(),
            "the actual bar must expose visible controls"
        );
        for (index, (left, a)) in visible.iter().enumerate() {
            assert!(a.x0 >= 0.0 && a.x1 <= 320.0, "{left} escapes: {a:?}");
            for (right, b) in visible.iter().skip(index + 1) {
                assert!(
                    a.intersect(*b).is_zero_area(),
                    "{left} {a:?} overlaps {right} {b:?}"
                );
            }
        }
    }

    #[test]
    fn responsive_primary_bar_leads_with_the_painted_tilo_mark() {
        use ui::a11y::Action;
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(1), "Design review");
        let tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let bar = TopBarChrome::from_tree_with_local_user(&tree, "fixture-person".into())
            .with_chrome(chrome.clone())
            .with_tilo_mark();
        let mut frame = Framed::new(ui::State::new(), bar, (320, 480), Theme::dark());
        assert!(frame.responsive.active);
        let mark = frame.tilo_mark_rect();
        assert!(!mark.is_zero_area(), "the mark stays on a narrow bar");
        let overview = frame.control_rect(TopBarChrome::CONTROL_OVERVIEW);
        assert!(mark.x1 <= overview.x0, "{mark:?} leads {overview:?}");
        Framed::harness_mut(&mut frame).click_at(mark.center().x, mark.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.item.0", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        let intents = &chrome.borrow().intents;
        assert_eq!(intents.len(), 2, "{intents:?}");
        assert!(
            intents
                .iter()
                .all(|intent| matches!(intent, ChromeIntent::ToggleTiloMenu))
        );
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }

    #[test]
    fn responsive_primary_bar_overflow_native_actions_cancel_and_resize() {
        use ui::a11y::Action;
        let mut frame = Framed::new(ui::State::new(), fixture_bar(), (320, 480), Theme::dark());
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
        assert!(frame.responsive.toolbar.has_overflow());
        for _ in 0..3 {
            Framed::harness_mut(&mut frame)
                .perform_named("toolbar.overflow", &Action::Activate)
                .expect("admitted overflow");
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert!(frame.responsive.toolbar.is_overflow_open());
            Framed::harness_mut(&mut frame).key("Escape");
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert!(
                !frame.responsive.toolbar.is_overflow_open(),
                "Escape must close the menu"
            );
        }
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.overflow", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        Framed::harness_mut(&mut frame).resize((1000, 480)).unwrap();
        assert!(!frame.responsive.active);
        assert!(!frame.responsive.toolbar.is_overflow_open());
        Framed::harness_mut(&mut frame).resize((320, 480)).unwrap();
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.overflow", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        Framed::harness_mut(&mut frame).click_at(10.0, 450.0);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(
            !frame.responsive.toolbar.is_overflow_open(),
            "outside click cancels"
        );
    }

    #[test]
    fn responsive_primary_bar_delivers_pointer_and_semantic_intents_once() {
        use ui::a11y::Action;
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(1), "Design review");
        let tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let bar = TopBarChrome::from_tree_with_local_user(&tree, "fixture-person".into())
            .with_chrome(chrome.clone());
        let mut frame = Framed::new(ui::State::new(), bar, (320, 480), Theme::dark());
        let rect = frame.control_rect(TopBarChrome::CONTROL_OVERVIEW);
        assert!(!rect.is_zero_area());
        Framed::harness_mut(&mut frame).click_at(rect.center().x, rect.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(chrome.borrow().intents.len(), 1);
        assert!(matches!(
            chrome.borrow().intents[0],
            ChromeIntent::ToggleOverview
        ));
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.item.0", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(chrome.borrow().intents.len(), 2);
        assert!(
            !chrome.borrow().overlays.overview,
            "repeated activation toggles closed"
        );
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }

    #[test]
    fn responsive_primary_bar_editor_opens_from_overflow_and_escape_closes() {
        use ui::a11y::Action;
        let mut frame = Framed::new(ui::State::new(), fixture_bar(), (320, 480), Theme::dark());
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.overflow", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        let out = std::path::PathBuf::from("target/primary-bar-shots");
        std::fs::create_dir_all(&out).unwrap();
        Framed::harness(&frame).render_png(&out.join("dark-before-editor.png"));
        let rect = Framed::harness(&frame)
            .controls()
            .into_iter()
            .find(|control| control.name.as_deref() == Some(t("topbar-editor-title").as_str()))
            .and_then(|control| control.bounds)
            .expect("painted editor command");
        assert!(!rect.is_zero_area(), "editor row must actually be painted");
        assert!(rect.y0 >= 0.0 && rect.y1 <= 480.0, "{rect:?}");
        Framed::harness_mut(&mut frame).click_at(rect.center().x, rect.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(frame.widget_editing);
        assert!(!frame.responsive.toolbar.is_overflow_open());
        let out = std::path::PathBuf::from("target/primary-bar-shots");
        std::fs::create_dir_all(&out).unwrap();
        Framed::harness(&frame).render_png(&out.join("dark-editor.png"));
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
        Framed::harness_mut(&mut frame)
            .perform_named(TopBarChrome::CONTROL_REMOVE_MONITOR, &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(
            !frame
                .widget_config
                .widgets
                .contains(&TopBarWidget::SystemMonitor)
        );
        Framed::harness_mut(&mut frame).key("Escape");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(!frame.widget_editing);
        assert!(!frame.wants_pointer_capture());
    }

    #[test]
    fn responsive_primary_bar_focus_loss_closes_without_activation() {
        use ui::a11y::Action;
        let mut frame = Framed::new(ui::State::new(), fixture_bar(), (320, 480), Theme::dark());
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.overflow", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(frame.responsive.toolbar.is_overflow_open());
        Framed::harness_mut(&mut frame).send_host_event(&WindowEvent::Focused(false));
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(!frame.responsive.toolbar.is_overflow_open());
        assert!(!frame.wants_pointer_capture());
        assert!(!frame.widget_editing);
    }

    #[test]
    fn responsive_primary_bar_offline_status_is_named_and_painted() {
        let mut bar = fixture_bar();
        bar.status.link = Some(crate::chrome::LinkStatus {
            kind: crate::chrome::LinkKind::Offline,
            name: None,
            signal: 0,
        });
        bar.notification_badge = 3;
        let frame = Framed::new(ui::State::new(), bar, (320, 480), Theme::dark());
        let expected = format!(
            "{} · {}",
            frame.control_name(TopBarChrome::CONTROL_QUICK_SETTINGS),
            t("topbar-network-offline")
        );
        assert!(
            Framed::harness(&frame)
                .controls()
                .iter()
                .any(|control| control.name.as_deref() == Some(expected.as_str()))
        );
        let out = std::path::PathBuf::from("target/primary-bar-shots");
        std::fs::create_dir_all(&out).unwrap();
        Framed::harness(&frame).render_png(&out.join("dark-offline.png"));
    }

    #[test]
    fn responsive_primary_bar_semantic_focus_changes_actual_pixels() {
        use ui::a11y::Action;
        let mut frame = Framed::new(ui::State::new(), fixture_bar(), (320, 480), Theme::dark());
        let out = std::path::PathBuf::from("target/primary-bar-shots");
        std::fs::create_dir_all(&out).unwrap();
        let before = out.join("dark-before-keyboard-focus.png");
        let after = out.join("dark-keyboard-focus.png");
        Framed::harness(&frame).render_png(&before);
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.item.4", &Action::Focus)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        Framed::harness(&frame).render_png(&after);
        assert!(
            std::fs::read(&before).unwrap() != std::fs::read(&after).unwrap(),
            "a certified semantic focus operation must paint visible focus"
        );
        assert!(
            !frame.needs_redraw(),
            "an idle focused bar must not request continuous paint"
        );
    }

    #[test]
    fn responsive_primary_bar_selected_monitor_and_calendar_show_their_readings() {
        use ui::a11y::Action;
        let mut bar = fixture_bar();
        bar.telemetry.cpu_percent = Some(71);
        bar.clock.date = "7 October".into();
        let mut frame = Framed::new(ui::State::new(), bar, (320, 480), Theme::dark());
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.item.5", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        let expected = t_args("topbar-cpu-reading", &[("value", "71%".into())]);
        assert!(
            frame
                .accessibility()
                .unwrap()
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some(expected.as_str()))
        );
        let out = std::path::PathBuf::from("target/primary-bar-shots");
        std::fs::create_dir_all(&out).unwrap();
        Framed::harness(&frame).render_png(&out.join("dark-monitor.png"));
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.overflow", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        Framed::harness_mut(&mut frame)
            .perform_named("toolbar.item.6", &Action::Activate)
            .unwrap();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(
            frame
                .accessibility()
                .unwrap()
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some("7 October"))
        );
        Framed::harness(&frame).render_png(&out.join("dark-calendar.png"));
    }

    #[test]
    fn responsive_primary_bar_production_capture_matrix_is_written() {
        let out = std::env::var_os("TILO_PRIMARY_BAR_SHOTS").map_or_else(
            || std::path::PathBuf::from("target/primary-bar-shots"),
            std::path::PathBuf::from,
        );
        std::fs::create_dir_all(&out).expect("capture directory");
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high-contrast", Theme::high_contrast()),
            ("large-text", Theme::dark().with_text_scale(2.0)),
            (
                "ui-text-scaled",
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
            ),
        ] {
            let mut frame = Framed::new(ui::State::new(), fixture_bar(), (320, 480), theme);
            let path = out.join(format!("{name}-row.png"));
            Framed::harness(&frame).render_png(&path);
            assert!(path.is_file(), "missing actual production capture");
            Framed::harness_mut(&mut frame)
                .perform_named("toolbar.overflow", &ui::a11y::Action::Activate)
                .unwrap();
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            let path = out.join(format!("{name}-overflow.png"));
            Framed::harness(&frame).render_png(&path);
            assert!(path.is_file(), "missing actual overflow capture");
        }
    }
}

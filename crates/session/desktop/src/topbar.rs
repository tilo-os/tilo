//! The desktop's top bar: workspace navigation, the time (or a live
//! activity over it), presence and the status tray.
//!
//! The bar keeps one fixed place for the time. Everything else a person
//! chooses to keep around (a calendar, the system monitor, music, the
//! people here) is a desk widget, chosen in one place (Settings → Desk
//! widgets) and shown by the shell's desk, never a second widget system in
//! the bar.
//!
//! [`TopBarChrome`] implements [`ui::widget::Widget`] and paints onto the
//! root scene. Everything it shows about the device (status, clock,
//! overlays) comes from the host's
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
    ClockSnapshot, TopBarPresence,
};
use crate::i18n::{t, t_args};
use tilo_desktop_model::DesktopTree;
use ui::TextRole;
use ui::color_compat::ColorCompat;
use ui::presence::{PresenceCtx, user_color};
use ui::text_render::TextStyle;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::ButtonRole;
use ui::widget::Widget;

mod strips;

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

/// The desktop's bar: every bar control is [`BAR_CONTROL_PX`] (24) with an
/// `s1` of air above and below, so the bar is 32 logical pixels (64 at scale
/// 2). At 24 it read cramped and out of balance with the windows' bars
/// below it. The controls are the design system's bar control
/// (`ui::widget::BarControlFace`) at the bar's control height. The bar's controls run to the screen's top edge, where a
/// pointer cannot overshoot them, so their targets keep the pointer floor
/// through their own hit rects rather than a taller bar. Unscaled `BASE`
/// values because layout sizes its node from this constant before any theme
/// is in hand.
pub const TOPBAR_HEIGHT_PX: f32 = BAR_CONTROL_LOGICAL + 2.0 * ui::tokens::Spacing::BASE.s1;

/// The bar's height under `theme`: [`TOPBAR_HEIGHT_PX`], grown with a large
/// text size so the time's line still fits with air above and below. At the
/// default size the line is a little over 20 px and the bar stays 24.
#[must_use]
pub fn topbar_height(theme: &Theme) -> f32 {
    let line = ui::tokens::Typography::default()
        .style(TextRole::Body)
        .line_height_px()
        * theme.text_scale;
    TOPBAR_HEIGHT_PX.max((line + 2.0 * ui::tokens::Spacing::BASE.s1).ceil())
}

const CLOCK_OVERLAY_HEIGHT: f64 = 150.0;

/// Below this width the bar narrows its centre lane to the time alone,
/// leaving the rest to the workspace name and the people beside it.
const NARROW_MENU_BAR_PX: f64 = 720.0;

/// Height of every control in the bar — the Tilo mark, the workspace
/// pill, the centre widget, the tray pill and the media indicator — so they
/// share one top and bottom edge: the toolkit's bar control height less one
/// `s1` step, a label line with air above and below.
const BAR_CONTROL_LOGICAL: f32 =
    ui::tokens::Metrics::BASE.bar_control_height - ui::tokens::Spacing::BASE.s1;
const BAR_CONTROL_PX: f64 = BAR_CONTROL_LOGICAL as f64;
/// Centre-to-centre distance of the bar's avatar chips: 20 px chips whose
/// rings cut only each other's edge.
const AVATAR_STEP: f64 = 20.0;

/// Inset of a lane-filling centre widget (the system monitor, the
/// calendar, a live activity) from its pill's ends, the same each side so
/// the content is centred in the band.
const CENTER_INSET_PX: f64 = 12.0;

/// Diameter of the Lucide glyph beside the workspace name (the chevron
/// opening its menu): a control's 16.
const SMALL_GLYPH_PX: f64 = 16.0;

/// Diameter of the bar's status glyphs: a control's 16.
const BAR_GLYPH_PX: f64 = 16.0;

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
    /// The strip the bar names after the workspace pill, if any.
    strip_name: Option<String>,
    collaborator_initials: Vec<char>,
    collaborator_names: Vec<String>,
    collaborator_agents: Vec<bool>,
    collaborator_overflow: usize,
    watching: u32,
    sharing: (usize, usize, usize),
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
    notifications_quiet: bool,
    workspace_lens_open: bool,
    focused: bool,
    focused_control: Option<String>,

    clock_activity: Option<(String, u64, bool, bool)>,
    /// How far the centre's cross-fade between the time and a live activity
    /// has come, in thousandths; `None` at rest.
    center_fade: Option<u16>,
    app_tray_revision: u64,
    continuity: Option<String>,
    strips: strips::StripsKey,
    tilo_mark: Option<bool>,
    own: Option<crate::chrome::OwnChip>,
    /// The bar control under the pointer: it lifts.
    hovered_control: Option<&'static str>,
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

/// How the bar's focus last arrived, which decides whether its ring shows
/// and whether it holds an auto-hiding bar shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusOrigin {
    /// Tab reached the bar, or a key went to it while it had focus.
    Keyboard,
    Pointer,
    /// The shell placed its focus here with no input (the first focusable
    /// control while a window types): nobody is reading the bar.
    Placed,
}

/// The bell in the bar's status cluster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TrayBell {
    /// Unread notifications, counted beside the bell.
    unread: u32,
    /// Fresh banners are held (Do Not Disturb, presenting, a focus
    /// schedule): the bell is silenced.
    quiet: bool,
}

/// `DesktopTree`, not `TopBar`, lets `build_tilo_root` stay a pure
/// function of the tree and avoids a circular borrow against the
/// shell's owned logical `TopBar`.
pub struct TopBarChrome {
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
    synced_revision: u64,
    /// `Some(menu open)` when the bar leads with the Tilo mark
    /// ([`TopBarChrome::with_tilo_mark`]).
    tilo_mark: Option<bool>,
    /// The person's own avatar, as last painted.
    own_avatar_rect: Rect,
    // Click geometry — recomputed every frame so it tracks DPI/resize.
    tilo_mark_rect: Rect,
    ws_pill_rect: Rect,
    /// The current strip's name after the workspace pill, when the bar
    /// names it: a click there opens the strip switcher.
    ws_strip_rect: Rect,
    /// The chevron inside the workspace pill, kept so a test can
    /// prove the pill's trailing padding equals its leading padding.
    ws_chevron_rect: Rect,
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
    /// The bell in the status cluster: the unread count beside it, and
    /// whether fresh banners are held (silenced).
    bell: TrayBell,
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
    activity_headline: Option<String>,
    activity_unread: usize,
    activity_pulse: u64,
    activity_started: Option<Instant>,
    /// The centre swapping the time for a live activity and back: a
    /// cross-fade (`ui::motion::CrossFade`, `true` is the activity).
    center_fade: ui::motion::CrossFade<bool>,
    /// The centre selector as painted and hit-tested: as wide as its
    /// content needs, never wider than [`Self::center_lane`].
    center_rect: Rect,
    /// The centre lane the bar reserves for the selector (see
    /// [`Self::center_band_width`]). Its neighbours lay out against the
    /// lane, so they hold still while the selector changes widget.
    center_lane: Rect,
    /// winit delivers position with `CursorMoved` but not `MouseInput`,
    /// so it's cached here for click hit-testing.
    last_cursor: Option<Point>,
    /// The bar is one retained focus stop; this names the synthetic child
    /// currently selected inside it for keyboard and accessibility input.
    focused: bool,
    focused_control: Option<String>,
    /// Whether the bar's focus came from (or was last touched by) the
    /// pointer. Its ring is for keyboard focus only: a click on the bar used
    /// to light its first control in your colour, which says nothing to a
    /// pointer user.
    focus_origin: FocusOrigin,
    /// When the time the bar shows next goes stale: the end of the current
    /// minute for the clock, the second while a live activity counts.
    /// Set when the bar draws and advanced by `update` once reached, so an
    /// idle desktop still wakes to repaint the clock.
    time_wake: Option<Instant>,
    /// The active workspace's strips as named places, when the host
    /// reports who is where.
    strips: strips::StripSwitcher,
}

/// Seconds the centre band's time reading stays the same for: the clock
/// paints `HH:MM`, a live activity counts seconds.
fn time_bucket_secs(activity_visible: bool) -> u64 {
    if activity_visible { 1 } else { 60 }
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

/// Paint a bar control's lift at `shape` in `state`: the design system's
/// bar control ([`ui::widget::BarControlFace::fill`]), plain at rest, half a
/// lift under the pointer, a whole lift while pressed or open. The bar has
/// no surface of its own: its controls stand on the desktop ground and lift
/// off that.
fn paint_bar_selector(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    state: ui::chrome::SelectionState,
    theme: &Theme,
) {
    let face = ui::widget::BarControlFace::new("")
        .open(state.active)
        .pressed(state.pressed)
        .hovered(state.hover);
    if let Some(fill) = face.fill(theme, theme.tokens.colors.surfaces.base) {
        scene.fill(Fill::NonZero, transform, fill, None, shape);
    }
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
        if self.rect.width() < NARROW_MENU_BAR_PX {
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
            synced_revision: 0,
            tilo_mark: None,
            own_avatar_rect: Rect::ZERO,
            tilo_mark_rect: Rect::ZERO,
            ws_pill_rect: Rect::ZERO,
            ws_chevron_rect: Rect::ZERO,
            ws_strip_rect: Rect::ZERO,
            tray_pill_rect: Rect::ZERO,
            app_tray: Vec::new(),
            app_tray_images: HashMap::new(),
            tray_hit_rect: Rect::ZERO,
            media_indicator_rect: Rect::ZERO,
            media_signature: Vec::new(),
            media_started: None,
            media_expanded: false,
            bell: TrayBell::default(),
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
            activity_headline: None,
            activity_unread: 0,
            activity_pulse: 0,
            activity_started: None,
            center_fade: ui::motion::CrossFade::new(false),
            center_rect: Rect::ZERO,
            center_lane: Rect::ZERO,
            last_cursor: None,
            focused: false,
            focused_control: None,
            focus_origin: FocusOrigin::Placed,
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
            .checked_div(time_bucket_secs(activity_visible))
            .unwrap_or_default();
        CacheFingerprint {
            rect_px: (x0, y0, w, h),
            presence: self.presence(),
            workspace_name: self.workspace_name.clone(),
            strip_name: self.strip_name().map(str::to_owned),
            collaborator_initials: self.collaborator_initials.clone(),
            collaborator_names: self.collaborator_names.clone(),
            collaborator_agents: self.collaborator_agents.clone(),
            collaborator_overflow: self.collaborator_overflow,
            watching: self.watching(),
            sharing: self.sharing_summary(),
            activity_headline: self.activity_headline.clone(),
            activity_unread: self.activity_unread,
            activity_visible,
            time_bucket,
            status_key: self.status_key(),
            media_key: self.media_signature.clone(),
            media_expanded: self.media_expanded,
            notification_badge: self.bell.unread,
            notifications_quiet: self.bell.quiet,
            workspace_lens_open: self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.snapshot().overlays.ws_settings),
            focused: self.focused,
            focused_control: self.focused_control.clone(),
            clock_activity: self.chrome.as_ref().and_then(|chrome| {
                chrome
                    .snapshot()
                    .clock_activity
                    .map(|a| (a.label.clone(), a.remaining_secs, a.overlay, a.paused))
            }),
            center_fade: self.center_fade_key(),
            app_tray_revision: self.app_tray_revision(),
            continuity: self.continuity_label(),
            strips: self.strips_key(),
            tilo_mark: self.tilo_mark,
            own: self.own_chip(),
            hovered_control: self.hovered_control(),
        }
    }

    /// The bar control under the pointer, from the hit rects the last paint
    /// laid down: plain at rest, it lifts while the pointer is over it.
    fn hovered_control(&self) -> Option<&'static str> {
        let point = self.last_cursor?;
        if !self.rect.contains(point) {
            return None;
        }
        self.control_targets()
            .into_iter()
            .find(|target| self.control_rect(target).contains(point))
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

    fn live_status(&self) -> crate::chrome::SystemStatus {
        match self.chrome.as_ref() {
            Some(c) => c.snapshot().status,
            None => self.status.clone(),
        }
    }

    /// Wake again when the time reading just drawn goes stale.
    fn schedule_time_wake(&mut self) {
        let since_epoch = Duration::from_nanos(ui::time::unix_now_ns());
        let bucket = time_bucket_secs(self.activity_is_visible());
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

    /// Where the workspace name was last drawn, for anchoring its menu
    /// ([`ChromeIntent::ToggleWorkspaceMenu`]: the workspaces, where it is
    /// hosted, joining and leaving, renaming, the overview, a new desk).
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
        if self.is_compact() {
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
        // Counted with the same `present` filter as the avatars, so the
        // overflow chip agrees with the row.
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

    /// Whether the workspace pill names the strip shown.
    fn shows_strip_name(&self) -> bool {
        self.strip_name().is_some()
    }

    pub fn collaborator_colors(&self) -> &[Color] {
        &self.collaborator_colors
    }

    pub fn collaborator_overflow(&self) -> usize {
        self.collaborator_overflow
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
    /// The glyph closing the workspace pill: it opens a menu.
    const WORKSPACE_MENU_ICON: ui::icons::Icon = ui::icons::Icon::ChevronDown;
    const CONTROL_PROFILE: &'static str = "topbar:profile";
    const CONTROL_WORKSPACE_SETTINGS: &'static str = "topbar:workspace-settings";
    /// The current strip's name in the workspace pill: opens the strips.
    const CONTROL_STRIP_NAME: &'static str = "topbar:strip-name";
    const CONTROL_QUICK_SETTINGS: &'static str = "topbar:quick-settings";
    const CONTROL_COLLABORATORS: &'static str = "topbar:collaborators";
    const CONTROL_MEDIA: &'static str = "topbar:media";
    const CONTROL_CONTINUITY: &'static str = "topbar:continuity";
    const CONTROL_CONTINUITY_DISMISS: &'static str = "topbar:continuity-dismiss";
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
                                t_args(
                                    "topbar-sharing-window-in-workspace",
                                    &[
                                        ("window", tab.title.as_str().into()),
                                        ("workspace", workspace.name.as_str().into()),
                                    ],
                                )
                            }
                        },
                    )
            })
            .collect()
    }

    fn control_targets(&self) -> Vec<&'static str> {
        let mut controls = Vec::new();
        if self.tilo_mark.is_some() {
            controls.push(Self::CONTROL_TILO_MARK);
        }
        controls.push(Self::CONTROL_WORKSPACE_SETTINGS);
        if self.shows_strip_name() {
            controls.push(Self::CONTROL_STRIP_NAME);
        }
        if self.is_compact() {
            return controls;
        }
        controls.push(Self::CONTROL_QUICK_SETTINGS);
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
        match target {
            Self::CONTROL_TILO_MARK => self.tilo_mark_rect,
            Self::CONTROL_WORKSPACE_SETTINGS => self.ws_pill_rect,
            Self::CONTROL_STRIP_NAME => self.ws_strip_rect,
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

    /// The status cluster's bell, read out: whether fresh banners are
    /// silenced and how many are unread. `None` when no bell is painted.
    fn bell_name(&self) -> Option<String> {
        if self.bell.unread == 0 && !self.bell.quiet {
            return None;
        }
        Some(t_args(
            if self.bell.quiet {
                "topbar-bell-silenced"
            } else {
                "topbar-bell"
            },
            &[("unread", self.bell.unread.into())],
        ))
    }

    fn control_name(&self, target: &str) -> String {
        match target {
            Self::CONTROL_TILO_MARK => t("topbar-tilo-menu"),
            Self::CONTROL_WORKSPACE_SETTINGS => t_args(
                "topbar-workspace-menu",
                &[("workspace", self.workspace_name.as_str().into())],
            ),
            Self::CONTROL_PROFILE => self.profile_control_name(),
            Self::CONTROL_STRIP_NAME => t_args(
                "topbar-strip-name",
                &[("strip", self.strip_name().unwrap_or_default().into())],
            ),
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
            _ => t("topbar-control"),
        }
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
    /// sake: the keyboard is in it.
    pub fn holds_itself_shown(&self) -> bool {
        self.focused && self.focus_origin == FocusOrigin::Keyboard
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
        paint_bar_selector(
            scene,
            transform,
            &RoundedRect::from_rect(self.tilo_mark_rect, f64::from(theme.tokens.radii.control)),
            ui::chrome::SelectionState::new(
                open,
                self.hovered_control() == Some(Self::CONTROL_TILO_MARK),
                false,
            ),
            theme,
        );
        let side = f64::from(theme.tokens.metrics.icon_md);
        paint_tilo_mark(
            scene,
            transform,
            Rect::from_center_size(self.tilo_mark_rect.center(), (side, side)),
            theme,
        );
        x + control + f64::from(theme.tokens.spacing.s2)
    }

    fn workspace_label(
        &self,
        text_renderer: &mut TextRenderer,
        ws_text_style: &TextStyle,
        label_budget: f32,
    ) -> String {
        // The name alone: the workspace menu lists the others, so no
        // position count.
        text_renderer.elide_to_width_style(
            &self.workspace_name,
            label_budget.max(0.0),
            &ws_text_style
                .clone()
                .with_weight(self.workspace_name_text_weight()),
        )
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

    /// Workspace-name pill (click toggles the workspace menu) and its
    /// trailing chevron, starting at `x`.
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
        let ws_text_style = tokens.typography.style(TextRole::Label);
        // Measured at the same weight it's drawn at (Semibold), so the
        // pill width tracks the glyph advance, not the narrower Regular.
        let ws_weight = self.workspace_name_text_weight();
        // The centre carousel owns a fixed central lane.  Long translated or
        // user-authored workspace names must yield to it instead of painting
        // underneath it.
        let center_left =
            f64::midpoint(self.rect.x0, self.rect.x1) - self.center_band_width(theme) / 2.0;
        // Capsule chrome around the label: a leading pad, the gap to the
        // chevron, the chevron itself and a trailing pad of the same size — so
        // the pill reads centred on its contents. The `s4` beyond that is
        // the breathing room `long_workspace_name_yields_to_the_center_lane`
        // keeps between the pill and the centre selector.
        let pad = f64::from(tokens.spacing.s2);
        let chevron = SMALL_GLYPH_PX;
        let pill_chrome = pad * 3.0 + chevron;
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
        // A bar control's height, grown with its label at a large text
        // scale (as `ControlSize` grows), never taller than the bar.
        let pill_h = BAR_CONTROL_PX
            .max((f64::from(ws_h) + f64::from(tokens.spacing.s1) / 2.0).ceil())
            .min(self.rect.height().max(BAR_CONTROL_PX));
        // A bar control: the workspace name and its chevron, plain at rest,
        // lifting under the pointer and while its menu is open.
        let workspace_lens_open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.snapshot().overlays.ws_settings);
        let name = self.control_name(Self::CONTROL_WORKSPACE_SETTINGS);
        let hovered = self.hovered_control() == Some(Self::CONTROL_WORKSPACE_SETTINGS);
        let face = ui::widget::BarControlFace::new(&name)
            .label(&ws_label)
            .trailing(Self::WORKSPACE_MENU_ICON)
            .open(workspace_lens_open)
            .hovered(hovered);
        let pill_w = face.width(text_renderer, theme);
        let ws_pill_outer = Rect::new(
            x - pad,
            center_y - pill_h / 2.0,
            x - pad + pill_w,
            center_y + pill_h / 2.0,
        );
        self.ws_pill_rect = ws_pill_outer;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.settings_anchor(ws_pill_outer);
        }
        let ground = colors.surfaces.base;
        let mut ctx = ui::paint::PaintCtx::new(scene, transform, text_renderer, theme);
        face.paint(&mut ctx, ws_pill_outer, ground);
        let chevron_x = x + f64::from(ws_w) + f64::from(tokens.metrics.icon_label_gap);
        self.ws_chevron_rect = Rect::new(
            chevron_x,
            center_y - chevron / 2.0,
            chevron_x + chevron,
            center_y + chevron / 2.0,
        );
    }

    /// The centre band. The clock is plain text on the bar, with no pill
    /// (a pill around five characters read as a second bar inside the bar);
    /// a live activity over it keeps a quiet selector pill. Returns the
    /// centre line the time or the activity draws on.
    fn draw_center_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) -> f64 {
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
        // The quiet selector pill comes and goes with the activity.
        let pill = self.center_activity_opacity(ui::time::now());
        if pill > 0.0 {
            let band_rr = RoundedRect::from_rect(
                band,
                ui::chrome::ChromeRole::Selector.radius(theme, band.height()),
            );
            if pill < 1.0 {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    pill,
                    transform,
                    &band.inflate(2.0, 2.0),
                );
            }
            paint_bar_selector(
                scene,
                transform,
                &band_rr,
                ui::chrome::SelectionState::active(true),
                theme,
            );
            if pill < 1.0 {
                scene.pop_layer();
            }
        }
        let (content_x0, content_x1) = Self::center_content_lane(band);
        f64::midpoint(content_x0, content_x1)
    }

    /// The clock with nothing live over it: drawn as bare text.
    #[cfg(test)]
    fn center_is_plain_text(&self) -> bool {
        !self.activity_is_visible()
    }

    /// How strongly the live activity (and its pill) shows at `now`, as it
    /// cross-fades with the time.
    fn center_activity_opacity(&self, now: Instant) -> f32 {
        if *self.center_fade.shown() {
            return self.center_fade.incoming(now);
        }
        self.center_fade
            .outgoing(now)
            .filter(|(activity, _)| **activity)
            .map_or(0.0, |(_, opacity)| opacity)
    }

    /// The lane the active centre widget draws into: the same inset in from
    /// either end of the band. One function so
    /// [`Self::draw_center_band`] and [`Self::activity_content_width`]
    /// cannot disagree about how much room the activity headline has.
    fn center_content_lane(band: Rect) -> (f64, f64) {
        // Symmetric, so the content's centre is the band's centre.
        (band.x0 + CENTER_INSET_PX, band.x1 - CENTER_INSET_PX)
    }

    /// The date, said beside the time to assistive technology.
    fn date_description(&self) -> String {
        let clock = self.live_clock();
        format!("{} {}", clock.weekday, clock.date)
            .trim()
            .to_owned()
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
                    let reading = ui::format::timecode(activity.remaining_secs);
                    if activity.label.is_empty() {
                        reading
                    } else {
                        format!("{}  {reading}", activity.label)
                    }
                },
            )
    }

    /// The width of the centre widget inside its `lane_w` lane. A live
    /// activity fills the lane; the clock is a five-character reading, so its target hugs it: the time and the
    /// content inset either side. The time is measured with every digit set
    /// as the widest one, so the target does not change width minute to
    /// minute.
    fn center_pill_width(&self, theme: &Theme, text: &mut TextRenderer, lane_w: f64) -> f64 {
        if self.activity_is_visible() {
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
        let badge_w = if self.bell.unread > 0 {
            let label = Self::notification_badge_label(self.bell.unread);
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
        // One bell slot: the unread count rides beside it, and quiet mode
        // silences it.
        if self.bell.unread > 0 || self.bell.quiet {
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
        if self.bell.unread > 0 {
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
        // Plain at rest, like every control on the bar: the status glyphs
        // stand on the bar itself.
        let tray =
            RoundedRect::from_rect(self.tray_pill_rect, f64::from(theme.tokens.radii.control));
        // The status cluster is one bar control: it lifts under the
        // pointer.
        paint_bar_selector(
            scene,
            transform,
            &tray,
            ui::chrome::SelectionState::new(
                false,
                self.hovered_control() == Some(Self::CONTROL_QUICK_SETTINGS),
                false,
            ),
            theme,
        );
        // At 3% the user needs to notice the whole control, not just read
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
    /// notifications, and the silenced bell while fresh banners are held
    /// (Do Not Disturb): the state lives in the status cluster, not in a
    /// card over the person's work. Returns the x cursor position for the
    /// next slot (unchanged if not drawn).
    fn draw_tray_notification_badge(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        if self.bell.unread == 0 && !self.bell.quiet {
            return sx;
        }
        ui::icons::draw_icon(
            scene,
            transform,
            layout.slot_rect(sx),
            if self.bell.quiet {
                ui::icons::Icon::BellOff
            } else {
                ui::icons::Icon::Bell
            },
            layout.bright,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        if self.bell.unread == 0 {
            return sx + layout.tray_slot + layout.tray_gap;
        }
        let label = Self::notification_badge_label(self.bell.unread);
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
                .unwrap_or(colors.intent.you);
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
            colors.intent.accent_ink
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
        } else if self.ws_strip_rect.contains(p) {
            ChromeIntent::ShowStrips
        } else if self.ws_pill_rect.contains(p) {
            ChromeIntent::ToggleWorkspaceMenu
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
            if snapshot.clock_activity.is_none() && self.clock_overlay_rect != Rect::ZERO {
                self.clock_drag_offset = None;
                self.clock_touch_id = None;
                self.clock_control_focus = None;
                self.cache_fp = CacheFingerprint::default();
            }
        }
        self.bell.unread = self
            .chrome
            .as_ref()
            .map_or(0, |chrome| chrome.snapshot().notification_badge);
        if let Some(chrome) = self.chrome.as_ref() {
            self.bell.quiet = chrome.snapshot().notifications_quiet;
        }
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

    /// Sizes the center band, then draws the time, or a live activity over
    /// it. Split out of [`Widget::draw`] to stay under clippy's line budget.
    fn draw_center_widget(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) {
        let content_cx = self.draw_center_band(scene, transform, theme, text_renderer, center_y);
        // The time and a live activity cross-fade in place.
        let now = ui::time::now();
        let mut layers = Vec::with_capacity(2);
        if let Some((activity, opacity)) = self.center_fade.outgoing(now) {
            layers.push((*activity, opacity));
        }
        layers.push((*self.center_fade.shown(), self.center_fade.incoming(now)));
        for (activity, opacity) in layers {
            let layered = opacity < 1.0;
            if layered {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    opacity,
                    transform,
                    &self.center_lane,
                );
            }
            if activity {
                self.draw_activity_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            } else {
                self.draw_clock_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            }
            if layered {
                scene.pop_layer();
            }
        }
    }

    /// The centre's cross-fade as the cache sees it: its progress in
    /// thousandths while it runs, so each of its frames draws anew.
    fn center_fade_key(&self) -> Option<u16> {
        let now = ui::time::now();
        self.center_fade.is_moving(now).then(|| {
            let permille = (self.center_fade.incoming(now) * 1000.0)
                .round()
                .clamp(0.0, 1000.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0..=1000 just above"
            )]
            let permille = permille as u16;
            permille
        })
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
        // The headline alone: the unread count is the badge's, never joined
        // into the sentence, and clicking the pill opens the activity.
        let medium = style.clone().with_weight(TextWeight::Medium);
        let (full_width, _) = text_renderer.measure_style(headline, None, &medium);
        if full_width <= max_width {
            return headline.to_string();
        }
        // Aggregate activity sometimes lists its examples after a colon
        // (the workspaces just recorded). Keep the whole event clause rather
        // than cutting part-way through the examples.
        if let Some((summary, _examples)) = headline.split_once(':') {
            let (compact_width, _) = text_renderer.measure_style(summary, None, &medium);
            if compact_width <= max_width {
                return summary.to_string();
            }
        }
        text_renderer.elide_to_width_style(headline, max_width, &medium)
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
        // An unread count is filled in the person's own colour.
        let fill = theme.tokens.colors.intent.you;
        scene.fill(
            Fill::NonZero,
            transform,
            fill,
            None,
            &vello::kurbo::Circle::new(center, radius),
        );
        let style = theme.text(TextRole::Caption);
        let (w, h) = text_renderer.measure_style(&label, None, &style);
        let ink = theme
            .tokens
            .colors
            .readable_on(fill, theme.tokens.colors.text_primary);
        text_renderer.draw_style(
            scene,
            &label,
            ink,
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

    fn activate_control(&self, target: &str) -> bool {
        if matches!(
            target,
            Self::CONTROL_CONTINUITY | Self::CONTROL_CONTINUITY_DISMISS
        ) {
            return self.activate_continuity_control(target);
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let intent = match target {
            Self::CONTROL_TILO_MARK if self.tilo_mark.is_some() => ChromeIntent::ToggleTiloMenu,
            Self::CONTROL_WORKSPACE_SETTINGS => ChromeIntent::ToggleWorkspaceMenu,
            Self::CONTROL_STRIP_NAME => ChromeIntent::ShowStrips,
            Self::CONTROL_PROFILE => ChromeIntent::ToggleProfile,
            Self::CONTROL_QUICK_SETTINGS | Self::CONTROL_MEDIA => ChromeIntent::ToggleQuickSettings,
            Self::CONTROL_COLLABORATORS => ChromeIntent::ToggleCollaborators,
            _ => return false,
        };
        chrome.intent(intent)
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

    fn handle_keyboard(&mut self, event: &WindowEvent, handled: &mut bool) {
        use winit::keyboard::{Key, NamedKey};
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return;
        };
        if !self.focused || key.state != winit::event::ElementState::Pressed {
            return;
        }
        let controls = self.control_targets();
        if controls.is_empty() {
            return;
        }
        let current = self
            .focused_control
            .as_deref()
            .and_then(|target| controls.iter().position(|candidate| *candidate == target))
            .unwrap_or(0);
        let focus = match &key.logical_key {
            Key::Named(NamedKey::ArrowLeft) => Some(
                current
                    .checked_sub(1)
                    .unwrap_or_else(|| controls.len().saturating_sub(1)),
            ),
            Key::Named(NamedKey::ArrowRight) => Some(
                current
                    .checked_add(1)
                    .filter(|&index| index < controls.len())
                    .unwrap_or(0),
            ),
            Key::Named(NamedKey::Home) => Some(0),
            Key::Named(NamedKey::End) => Some(controls.len().saturating_sub(1)),
            _ => None,
        };
        if let Some(target) = focus.and_then(|index| controls.get(index)) {
            self.focused_control = Some((*target).into());
            *handled = true;
            return;
        }
        if matches!(
            &key.logical_key,
            Key::Named(NamedKey::Enter | NamedKey::Space)
        ) && let Some(target) = controls.get(current)
        {
            *handled = self.activate_control(target);
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
        let elapsed = ui::format::timecode(activity.remaining_secs);
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
        let reading = ui::format::timecode(activity.remaining_secs);
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

    fn handle_clock_event(
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

    /// Pointer motion: where the pointer is, for clicks, and a floating
    /// clock being dragged. A wheel over the bar is
    /// nobody's: the bar has nothing to scroll and nothing to cycle.
    fn handle_pointer_motion(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::CursorMoved { position, .. } = event else {
            return false;
        };
        let point = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor = Some(point);
        if self.clock_touch_id.is_none() && self.move_clock_drag_to(point) {
            *handled = true;
        }
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
        if self.clock_touch_id.is_none() && self.clock_drag_offset.take().is_some() {
            *handled = true;
        }
        true
    }

    fn handle_left_press(&mut self, event: &WindowEvent, handled: &mut bool) {
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

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
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
        time_changed
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
        let node = match taffy.new_leaf(style) {
            Ok(node) => node,
            Err(err) => {
                // Keep the session alive if a future Taffy change rejects
                // this fresh leaf (production uses panic = "abort").
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
        self.rect = ui::widget::placed(l, origin);
        if let Some(at) = self.placement {
            self.rect = Rect::new(at.x0, at.y0, at.x1, at.y0 + self.rect.height());
        }
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
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
        self.schedule_time_wake();
        let showing_activity = self.activity_is_visible();
        self.center_fade.show(
            showing_activity,
            theme.motion(),
            ui::tokens::MotionSource::Pointer,
            ui::time::now(),
        );
        let fp = self.current_fingerprint();
        if fp == self.cache_fp && self.cache_theme.as_ref() == Some(theme) && fp.rect_px.2 > 0 {
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

        // The workspace's name leads, after the mark when there is one:
        // where its label starts, past its capsule's `s2` leading pad.
        let label_x = self.draw_tilo_mark(
            scene,
            transform,
            theme,
            center_y,
            self.rect.x0 + f64::from(theme.tokens.spacing.s3),
        ) + f64::from(theme.tokens.spacing.s2);
        self.draw_workspace_pill(scene, transform, theme, text_renderer, center_y, label_x);
        let center_left =
            f64::midpoint(self.rect.x0, self.rect.x1) - self.center_band_width(theme) / 2.0;
        // The strip shown, named after the workspace it is in where no
        // strip chips stand: the frequent level, where the person looks for
        // "where am I".
        self.draw_strip_name(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            (
                self.ws_pill_rect.x1,
                center_left - f64::from(theme.tokens.spacing.s4),
            ),
        );
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
        self.draw_activity_badge(scene, transform, theme, text_renderer);
        self.draw_focus_indicator(scene, transform, theme);
        let clock_activity = self.chrome.as_ref().and_then(|chrome| {
            let snapshot = chrome.snapshot();
            snapshot.overlays.clock.then_some(snapshot.clock_activity)?
        });
        if let Some(activity) = clock_activity {
            self.draw_clock_overlay(scene, transform, text_renderer, theme, &activity);
        }
        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_fp = fp;
        self.cache_theme = Some(theme.clone());
        text_renderer.set_text_scale(renderer_scale);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Tab moving focus into the bar is reading it with the keyboard,
        // hidden or not: that focus shows its ring and holds it shown.
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == winit::event::ElementState::Pressed
            && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab)
        {
            self.focus_origin = FocusOrigin::Keyboard;
        }
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
            // A key the focused bar takes is keyboard use of the bar; focus
            // the shell merely placed here is not, until one arrives.
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == winit::event::ElementState::Pressed =>
            {
                self.focus_origin = FocusOrigin::Keyboard;
            }
            _ => {}
        }
        if self.handle_strip_naming_event(event, ctx) {
            return;
        }
        if self.is_compact() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.last_cursor = Some(ctx.logical(position));
                return;
            }
            if self.handle_left_release(event, ctx.handled) {
                return;
            }
            self.handle_left_press(event, ctx.handled);
            return;
        }
        if self.handle_clock_event(event, ctx.scale_factor, *ctx.modifiers, ctx.handled)
            || self.handle_pointer_motion(event, ctx.scale_factor, ctx.handled)
        {
            return;
        }
        self.handle_keyboard(event, ctx.handled);
        if *ctx.handled || self.handle_app_tray_press(event, ctx.handled) {
            return;
        }
        if self.handle_left_release(event, ctx.handled) {
            return;
        }
        self.handle_left_press(event, ctx.handled);
        if *ctx.handled {
            *ctx.focus_target = Some(ctx.node_id);
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.clock_drag_offset.is_some()
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
            ("clock_overlay_pause".to_string(), self.clock_pause_rect),
            ("clock_overlay_add_minute".to_string(), self.clock_add_rect),
            ("clock_overlay_close".to_string(), self.clock_close_rect),
            (
                "topbar_workspace_pill".to_string(),
                self.control_rect(Self::CONTROL_WORKSPACE_SETTINGS),
            ),
            (
                "topbar_profile".to_string(),
                self.control_rect(Self::CONTROL_PROFILE),
            ),
            (
                "topbar_strip_name".to_string(),
                self.control_rect(Self::CONTROL_STRIP_NAME),
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
            ("topbar_center_clock".to_string(), self.center_rect),
        ];
        rects.extend(self.strip_test_rects());
        rects.extend(app_tray);
        rects
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children: Vec<AccessNode> = self
            .control_targets()
            .into_iter()
            .map(|target| {
                let mut node = AccessNode::new(Role::Button)
                    .with_action_target(target)
                    .with_name(self.control_name(target))
                    .with_bounds(self.control_rect(target))
                    .with_state(AccessState {
                        focused: self.focused && self.focused_control.as_deref() == Some(target),
                        focusable: true,
                        ..Default::default()
                    });
                // The bell painted in the status cluster says what it shows.
                if target == Self::CONTROL_QUICK_SETTINGS
                    && let Some(bell) = self.bell_name()
                {
                    node = node
                        .with_description(bell.clone())
                        .with_children(vec![AccessNode::new(Role::Image).with_name(bell)]);
                }
                // The pill names the workspace; where it stands among the
                // others is said to a reader, as the menu lists them.
                if target == Self::CONTROL_WORKSPACE_SETTINGS && self.workspace_total > 1 {
                    node = node.with_description(t_args(
                        "topbar-workspace-position",
                        &[
                            ("position", self.workspace_index.saturating_add(1).into()),
                            ("total", self.workspace_total.into()),
                        ],
                    ));
                }
                node
            })
            .collect();
        // The time: read, not pressed.
        children.push(
            AccessNode::new(Role::StaticText)
                .with_hashed_key("topbar-clock-reading")
                .with_name(self.clock_widget_text())
                .with_description(self.date_description())
                .with_bounds(self.center_rect),
        );
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
        let index = *path.first()?;
        self.control_targets()
            .get(index)
            .map(|target| self.control_rect(target))
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
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
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
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
        }
        self.focused = focused;
        if !focused {
            self.focus_origin = FocusOrigin::Placed;
        }
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
    fn needs_animation_frame(&self) -> bool {
        self.center_fade.is_moving(ui::time::now())
    }

    fn needs_redraw(&self) -> bool {
        if self.center_fade.is_moving(ui::time::now()) {
            return true;
        }
        if self.presence() != self.cache_fp.presence {
            return true;
        }
        if self.hovered_control() != self.cache_fp.hovered_control {
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
            "Home \u{203a} Main",
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
        assert_eq!(node.children[0].value.as_deref(), Some("2:05"));
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
        let menu = bar.ws_pill_rect.center();

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
        assert!(!click(&mut bar, menu), "hidden takes no press");
        assert!(chrome.borrow().intents.is_empty());

        chrome.borrow_mut().top_bar = TopBarPresence::Shown;
        assert!(bar.needs_redraw(), "showing repaints the bar");
        assert!(
            !bar.is_layout_dirty() && !bar.take_structure_dirty(),
            "showing over the windows changes no layout: nothing moves"
        );
        assert_eq!(lay_out_over_windows(&mut bar), 0.0);
        assert!(painted(&mut bar));
        assert_eq!(bar.ws_pill_rect.center(), menu, "same place as docked");
        assert!(click(&mut bar, menu), "shown, its controls work");
        assert_eq!(
            chrome.borrow().intents.as_slice(),
            &[ChromeIntent::ToggleWorkspaceMenu]
        );

        chrome.borrow_mut().top_bar = TopBarPresence::Hidden;
        assert!(bar.needs_redraw(), "hiding again clears it from the screen");
        assert!(!painted(&mut bar));
    }

    /// Do Not Disturb is shown in the status cluster, as a silenced bell in
    /// its own slot, not as a card over the person's work.
    #[test]
    fn quiet_mode_adds_a_silenced_bell_to_the_status_cluster() {
        let mut bar = sample_chrome();
        bar.status.link = Some(crate::chrome::LinkStatus {
            kind: crate::chrome::LinkKind::Wired,
            name: None,
            signal: 0,
        });
        lay_out_and_draw(&mut bar, 1280.0);
        let loud = bar.tray_pill_rect;
        bar.bell.quiet = true;
        lay_out_and_draw(&mut bar, 1280.0);
        let quiet = bar.tray_pill_rect;
        assert!(
            (quiet.width() - loud.width() - BAR_CONTROL_PX).abs() < 1e-6,
            "one glyph slot more: {loud:?} -> {quiet:?}"
        );
        assert_eq!(quiet.x1, loud.x1, "the cluster keeps its right edge");
        bar.bell.unread = 2;
        let node = bar.accessibility().expect("topbar semantics");
        let tray = node
            .children
            .iter()
            .find(|child| {
                child.action_target.as_deref() == Some(TopBarChrome::CONTROL_QUICK_SETTINGS)
            })
            .expect("the status cluster");
        assert_eq!(
            tray.description.as_deref(),
            Some("Notifications silenced, 2 unread"),
            "the silenced bell is named"
        );
        assert!(
            tray.children
                .iter()
                .any(|bell| bell.name.as_deref() == Some("Notifications silenced, 2 unread"))
        );
    }

    #[test]
    fn persistent_topbar_controls_have_coherent_targets() {
        let mut bar = sample_chrome();
        lay_out_and_draw(&mut bar, 1280.0);
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
        let reference = bar.ws_pill_rect;
        let controls = [
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
                "{name} {r:?} does not share the workspace pill's edges {reference:?}"
            );
        }
        let bar_centre = f64::midpoint(bar.rect.y0, bar.rect.y1);
        assert!(
            (reference.center().y - bar_centre).abs() < 0.01,
            "controls centre at {} in a bar centred on {bar_centre}",
            reference.center().y
        );
        // The clock is the time on the bar: no pill, and nothing to press.
        assert!(bar.center_is_plain_text());
    }

    /// The workspace capsule pads its label on the left exactly as much
    /// as it pads the chevron on the right, and the chevron sits on the
    /// pill's centre line — so the pill reads centred on its contents.
    #[test]
    fn workspace_pill_pads_its_label_and_chevron_symmetrically() {
        let mut bar = sample_chrome();
        let theme = Theme::default();
        lay_out_and_draw_with_theme(&mut bar, 1280.0, &theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        // Without the mark the pill leads the bar, `s3` in from its edge;
        // its label starts one `s2` inside it.
        let edge = f64::from(theme.tokens.spacing.s3);
        assert!(
            (bar.ws_pill_rect.x0 - bar.rect.x0 - edge).abs() < 0.01,
            "the pill stands {} in from the bar's edge, not {edge}",
            bar.ws_pill_rect.x0 - bar.rect.x0
        );
        let text_x = bar.rect.x0 + edge + pad;
        assert!(
            (text_x - bar.ws_pill_rect.x0 - pad).abs() < 0.01,
            "leading pad is {} not {pad}",
            text_x - bar.ws_pill_rect.x0
        );
        assert!(
            (bar.ws_pill_rect.x1 - bar.ws_chevron_rect.x1 - pad).abs() < 0.01,
            "trailing pad is {} not {pad}",
            bar.ws_pill_rect.x1 - bar.ws_chevron_rect.x1
        );
        assert!(
            (bar.ws_chevron_rect.center().y - bar.ws_pill_rect.center().y).abs() < 0.01,
            "chevron {:?} is off the pill's centre line {:?}",
            bar.ws_chevron_rect,
            bar.ws_pill_rect
        );
    }

    /// The bar's density constants are the spacing scale under other
    /// names; this pins them so they cannot drift off it unnoticed.
    #[test]
    fn bar_density_sits_on_the_spacing_scale() {
        let tokens = Theme::default().tokens;
        let spacing = tokens.spacing;
        assert_eq!(
            BAR_CONTROL_PX,
            f64::from(tokens.metrics.bar_control_height - spacing.s1)
        );
        assert_eq!(CENTER_INSET_PX, f64::from(spacing.s3));
        // USER REQUIREMENT (2026-10-19): "why is the top bar so tiny, why not
        // a few pixel bigger? it looks super unbalanced" — 24 px controls
        // with an `s1` of air each side: a 32 px bar.
        assert_eq!(TOPBAR_HEIGHT_PX, 32.0);
        assert_eq!(TOPBAR_HEIGHT_PX, BAR_CONTROL_LOGICAL + 2.0 * spacing.s1);
        assert!(TOPBAR_HEIGHT_PX < tokens.metrics.bar_height);
    }

    /// At the default text size the bar is 32 px; a large text size grows
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
        bar.activity_headline = Some("ada · 2 permission changes".into());
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
        // The headline alone, whole: the unread count is the badge's.
        assert!(
            label.starts_with("ada · 2 permission changes"),
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
        bar.handle_keyboard(&event, &mut handled);
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
    /// the bar used to light its first stop in your colour, though no
    /// keyboard was involved. Focus the shell merely placed on the bar (its
    /// first focusable control, while a window has the keyboard) is not
    /// keyboard use either: it shows no ring and holds no auto-hiding bar
    /// shown, or a maximized window's bar stayed up for as long as nobody
    /// pressed a key.
    #[test]
    fn the_focus_ring_follows_the_keyboard_not_a_click() {
        let tab = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
            None,
            winit::event::ElementState::Pressed,
        ));
        let mut bar = sample_chrome();
        bar.set_focus(true);
        assert!(
            !bar.focus_ring_visible() && !bar.holds_itself_shown(),
            "focus placed with no input is nobody reading the bar"
        );
        bar.set_focus(false);

        let _ = dispatch_topbar_event(&mut bar, &tab);
        bar.set_focus(true);
        assert!(
            bar.focus_ring_visible() && bar.holds_itself_shown(),
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
        let _ = dispatch_topbar_event(&mut bar, &tab);
        bar.set_focus(true);
        assert!(
            bar.focus_ring_visible(),
            "focus tabbed into afresh starts unpointed"
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
            Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
        );

        let second = bar.control_targets()[1];
        assert!(press_named_key(
            &mut bar,
            winit::keyboard::NamedKey::ArrowRight
        ));
        assert_eq!(bar.focused_control.as_deref(), Some(second));
        assert!(press_named_key(
            &mut bar,
            winit::keyboard::NamedKey::ArrowLeft
        ));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
        );
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::End));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_MEDIA)
        );
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Home));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
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
    fn keyboard_activation_matches_pointer_actions() {
        let tree = two_workspace_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        bar.set_focus(true);

        bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS.to_string());
        assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Space));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ToggleWorkspaceMenu)
        );
    }

    /// A workspace with two named strips, the second shown.
    fn strips_tree() -> DesktopTree {
        let mut tree = two_workspace_tree();
        tree.workspaces[0].strips = vec![
            tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(10), "Lounge"),
            tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(11), "Research"),
        ];
        let active = tree.active;
        tree.active_strip
            .insert(active, tilo_desktop_model::StripId(11));
        tree
    }

    /// The left end names where you are: the workspace, then the strip
    /// shown. The strip's name is its own target and opens the strip
    /// switcher; the workspace's pill opens the workspace menu.
    #[test]
    fn the_bar_names_the_strip_after_the_workspace_and_it_opens_the_strip_switcher() {
        let tree = strips_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        lay_out_and_draw(&mut bar, 1280.0);
        let targets = bar.control_targets();
        assert_eq!(
            &targets[..2],
            &[
                TopBarChrome::CONTROL_WORKSPACE_SETTINGS,
                TopBarChrome::CONTROL_STRIP_NAME,
            ],
            "workspace, strip: nothing else at the left end"
        );
        assert_eq!(bar.strip_name(), Some("Research"));

        let strip_rect = bar.control_rect(TopBarChrome::CONTROL_STRIP_NAME);
        assert!(strip_rect.width() > 0.0);
        assert!(
            strip_rect.x0 >= bar.ws_pill_rect.x1,
            "the strip follows the workspace's pill"
        );
        assert!(click(&mut bar, strip_rect.center()));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ShowStrips)
        );
        let pill = bar.ws_pill_rect.center();
        assert!(click(&mut bar, pill));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ToggleWorkspaceMenu)
        );
    }

    /// A strip without a name is not named: the pill stands alone.
    #[test]
    fn an_unnamed_strip_leaves_the_workspace_pill_alone() {
        let mut tree = strips_tree();
        tree.workspaces[0].strips[1].name = String::new();
        let mut bar = TopBarChrome::from_tree(&tree);
        lay_out_and_draw(&mut bar, 1280.0);
        assert_eq!(bar.strip_name(), None);
        assert!(
            !bar.control_targets()
                .contains(&TopBarChrome::CONTROL_STRIP_NAME)
        );
        assert!(bar.ws_strip_rect.is_zero_area());
    }

    /// Scrolling over the top of the screen changes nothing: the bar keeps
    /// the time, and a wheel there is not the bar's.
    #[test]
    fn a_wheel_over_the_time_changes_nothing() {
        let tree = two_workspace_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        lay_out_and_draw(&mut bar, 1280.0);
        let clock = bar.center_rect.center();
        dispatch_topbar_event(
            &mut bar,
            &ui::synthetic_input::cursor_moved(clock.x, clock.y),
        );
        let before = scene_fingerprint(&mut bar);
        for delta in [3.0, -7.0, 12.0] {
            assert!(
                !dispatch_topbar_event(&mut bar, &ui::synthetic_input::mouse_wheel(0.0, delta)),
                "the bar does not take the wheel"
            );
        }
        assert!(!bar.needs_redraw());
        assert_eq!(scene_fingerprint(&mut bar), before);
        assert!(bar.center_is_plain_text(), "still the time");
        assert!(chrome.borrow().intents.is_empty());
    }

    /// What the bar paints, to compare: its path data and how many draws
    /// and glyph runs.
    fn scene_fingerprint(bar: &mut TopBarChrome) -> (Vec<u32>, usize, usize) {
        let mut scene = Scene::new();
        bar.cache_fp = CacheFingerprint::default();
        bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            &PresenceCtx::default(),
        );
        let encoding = scene.encoding();
        (
            encoding.path_data.clone(),
            encoding.draw_tags.len(),
            encoding.resources.glyph_runs.len(),
        )
    }

    #[test]
    fn accessibility_children_have_stable_semantics_and_synthetic_parity() {
        let tree = two_workspace_tree();
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        lay_out_and_draw(&mut bar, 1280.0);
        bar.set_focus(true);
        bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS.to_string());

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

        let workspace = node
            .children
            .iter()
            .find(|child| {
                child.action_target.as_deref() == Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
            })
            .expect("workspace semantics");
        assert_eq!(workspace.role, ui::a11y::Role::Button);
        assert!(workspace.state.focused);
        assert_eq!(
            workspace.description.as_deref(),
            Some("Workspace 1 of 2"),
            "the position is said, not painted"
        );
        let clock = node
            .children
            .iter()
            .find(|child| child.role == ui::a11y::Role::StaticText)
            .expect("the time is read out");
        assert_eq!(clock.action_target, None, "the time is not a control");

        let index = node
            .children
            .iter()
            .position(|child| {
                child.action_target.as_deref() == Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
            })
            .expect("workspace child index");
        assert_eq!(
            bar.synthetic_accessibility_rect(&[index]),
            workspace.bounds.map(ui::a11y::AccessBounds::to_rect)
        );
        bar.focused_control = None;
        assert!(bar.focus_synthetic_accessibility(&[index]));
        assert_eq!(
            bar.focused_control.as_deref(),
            Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
        );
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
        assert!(mark.x1 <= bar.ws_pill_rect.x0, "the mark leads");
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

    /// The bar leads with the mark and the workspace's name as one menu,
    /// shows desks only when there are several, and ends with the person's
    /// own avatar opening their profile card.
    #[test]
    fn the_bar_leads_with_one_menu_and_ends_with_you() {
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
            .with_tilo_mark();
        lay_out_and_draw(&mut bar, 1280.0);

        let targets = bar.control_targets();
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
            "with nothing between them"
        );
        assert!(own.x1 < bar.tray_hit_rect.x0, "you, then quick settings");
        assert!(own.x0 > bar.center_rect.x1, "on the right");
        assert_eq!(bar.ws_chevron_rect.width(), SMALL_GLYPH_PX);
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
    /// change; the bar's neighbours lay out against the lane.
    #[test]
    fn the_clock_pill_hugs_the_time_and_holds_its_width() {
        let pill_at = |clock: &str| {
            let mut bar = sample_chrome();
            bar.clock = ClockSnapshot {
                clock: clock.into(),
                ..ClockSnapshot::default()
            };
            lay_out_and_draw(&mut bar, 1600.0);
            (bar.center_rect, bar.center_lane, bar.ws_pill_rect)
        };
        let (clock, lane, ws) = pill_at("11:11");
        assert_eq!(lane.width(), 360.0);
        assert!(
            clock.width() < lane.width() / 2.0,
            "a time is not a 360 px pill: {clock:?}"
        );
        assert!((clock.center().x - lane.center().x).abs() < 1e-9);
        let (later, later_lane, later_ws) = pill_at("20:08");
        assert_eq!(
            later.width(),
            clock.width(),
            "the pill holds still minute to minute"
        );
        assert_eq!((later_lane, later_ws), (lane, ws), "neighbours do not move");
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
    fn new_activity_temporarily_shows_over_the_time() {
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
        assert!(!bar.center_is_plain_text());

        chrome.borrow_mut().publish_activity_glance(
            Some("Ada · Prepared launch brief".to_string()),
            0,
            42,
            None,
        );
        bar.refresh_status_and_media_state();
        assert!(!bar.activity_is_visible());
        assert!(bar.center_is_plain_text(), "the time again");
    }

    /// The time and a live activity swap with the standard cross-fade
    /// (`ui::motion::CrossFade`), which settles and then asks for nothing.
    #[test]
    fn the_time_and_a_live_activity_cross_fade_and_settle() {
        let motion = Theme::dark().motion();
        let now = ui::time::now();
        let mut fade = ui::motion::CrossFade::new(false);
        let mut bar = TopBarChrome::from_tree(&DesktopTree::default());
        bar.center_fade
            .show(true, motion, ui::tokens::MotionSource::Pointer, now);
        assert!(bar.needs_animation_frame(), "the swap animates");
        assert!(
            bar.center_fade_key().is_some(),
            "and redraws on each of its frames"
        );
        fade.show(true, motion, ui::tokens::MotionSource::Pointer, now);
        let settled = fade.settles_at().expect("cross-fading");
        assert!(fade.outgoing(settled).is_none());
        assert_eq!(fade.incoming(settled), 1.0);
        assert_eq!(
            bar.center_activity_opacity(settled),
            1.0,
            "the pill shows with it"
        );
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
            name.contains("scribe (agent): can control Draft in Private"),
            "{name}"
        );
        assert!(!name.contains("unidentified session"), "{name}");
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
        assert_eq!(TOPBAR_HEIGHT_PX, 32.0);
        for (label, rect) in [
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
            "bob",
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
            tilo_desktop_model::Collaborator::new("peer@11895642303397647149", "bob")
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
mod narrow_tests {
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
    fn a_phone_wide_bar_keeps_its_targets_apart_at_320() {
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
            for (right, b) in visible.iter().skip(index + 1).filter(|(_, b)| b != a) {
                assert!(
                    a.intersect(*b).is_zero_area(),
                    "{left} {a:?} overlaps {right} {b:?}"
                );
            }
        }
    }
}

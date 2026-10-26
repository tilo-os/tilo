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
use tilo_sort::CompactSort as _;
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

mod arrival_notice;
mod avatar_ask;
mod clock;
mod controls;
mod people;
mod sharing;
mod strips;
mod tray;
mod wave_notice;
mod workspace;

use clock::{clock_overlay_drag_region, time_bucket_secs, until_next_time_bucket};
use controls::{FocusOrigin, TrayBell, navigates, paint_bar_selector};
use sharing::{FaceLook, roster_colors};
pub use workspace::paint_tilo_mark;

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

/// The desktop's bar: every bar control is `BAR_CONTROL_PX` (24) with an
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
    use super::clock::{
        CLOCK_OVERLAY_HEIGHT, clamp_clock_overlay, clock_overlay_controls, clock_overlay_width,
        released_clock_touch_control,
    };
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
    /// Each face's live look: dimmed, its call ring's level and its
    /// bounce, quantised, so presence repaints the bar only when a face
    /// changes.
    faces: Vec<(bool, Option<u8>, u16)>,
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
    notifications_only: bool,
    workspace_lens_open: bool,
    focused: bool,
    focused_control: Option<String>,

    clock_activity: Option<(String, u64, bool, bool)>,
    /// How far the centre's cross-fade between the time and a live activity
    /// has come, in thousandths; `None` at rest.
    center_fade: Option<u16>,
    /// The centre selector's width while it morphs between the time and a
    /// live activity, in quarter pixels; `None` at rest.
    center_width: Option<u32>,
    app_tray_revision: u64,
    continuity: Option<String>,
    avatar_ask: Option<crate::chrome::AvatarAsk>,
    strips: strips::StripsKey,
    tilo_mark: Option<bool>,
    own: Option<crate::chrome::OwnChip>,
    /// The bar control under the pointer: it lifts.
    hovered_control: Option<String>,
    /// Whether the bar offers its invite button.
    can_invite: bool,
    /// What the bar says about a wave at this person, while it does.
    wave_notice: Option<String>,
    /// The arrival status as it stands, while it is up.
    arrival: Option<(String, bool, u16, u16)>,
}

/// One application tray icon as last painted: where, and what a click on
/// it addresses.
#[derive(Clone, Debug)]
struct AppTraySlot {
    key: String,
    id: String,
    /// The application proven to own the item, when one was.
    owner_app: Option<String>,
    /// What the item is called: the application's own title for it, else
    /// its id.
    title: String,
    item_is_menu: bool,
    rect: Rect,
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
    /// The parts [`Self::cached`] was drawn with, carried into the bar's
    /// section each time it is appended: a change damages its part only.
    cached_parts: Option<ui::scene_parts::SectionParts>,
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
    /// Same index as `collaborator_initials`: the roster id presence knows
    /// them by (idle, in a call, a wave's bounce).
    collaborator_ids: Vec<String>,
    /// Same index as `collaborator_initials`: how each face looks now, read
    /// from presence at each draw.
    face_looks: Vec<FaceLook>,
    /// Where each face was painted, same index.
    face_rects: Vec<Rect>,
    /// When someone here was first seen in a call, so their ring breathes
    /// from its start.
    call_since: Option<Instant>,
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
    /// Where the bell's unread count was painted, `Rect::ZERO` without one.
    bell_badge_rect: Rect,
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
    /// The others' faces (and the "+N" badge): each face opens that
    /// person's card, the badge everyone.
    avatar_stack_rect: Rect,
    /// The people button beside the faces, which opens everyone here (the
    /// panel and its pin); zero when nobody else is here.
    people_button_rect: Rect,
    /// The invite button beside the faces; zero when not offered.
    invite_rect: Rect,
    /// The arrival status as last drawn; zero while it is not up.
    arrival_rect: Rect,
    /// A press on the arrival status opens the person's own menu.
    arrival_pressable: bool,
    /// Where the people cluster (faces, spectators, invite) begins.
    people_left_x: f64,
    /// Persistent trusted-chrome disclosure of active screen viewers.
    sharing_indicator_rect: Rect,
    /// "Continue \<app\> from \<device\>": the offer, and its dismiss chip.
    continuity_rect: Rect,
    continuity_dismiss_rect: Rect,
    /// The ask open beside the own avatar: its pill and its two choices.
    ask: avatar_ask::AskRects,
    activity_headline: Option<String>,
    activity_unread: usize,
    activity_pulse: u64,
    activity_started: Option<Instant>,
    /// The centre swapping the time for a live activity and back: a
    /// cross-fade (`ui::motion::CrossFade`, `true` is the activity).
    center_fade: ui::motion::CrossFade<bool>,
    /// The bar leaving with a window maximizing over it (sliding up as the
    /// window's frame grows to the top), and coming back with its restore.
    /// Open at rest; [`Self::move_with_window`] drives it.
    with_window: ui::motion::Presence,
    /// The centre selector's width following its content (the time, or a
    /// live activity filling the lane) on the standard expand/collapse
    /// motion (`ui::motion::Extent`), so the selector grows into the lane
    /// and shrinks back to the time rather than jumping.
    center_width: ui::motion::Extent,
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
    strips: strips::BarStrips,
}

impl TopBarChrome {
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
            cached_parts: None,
            cache_theme: None,
            cache_fp: CacheFingerprint::default(),
            collaborator_initials: Vec::new(),
            collaborator_names: Vec::new(),
            collaborator_colors: Vec::new(),
            collaborator_agents: Vec::new(),
            collaborator_ids: Vec::new(),
            face_looks: Vec::new(),
            face_rects: Vec::new(),
            call_since: None,
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
            bell_badge_rect: Rect::ZERO,
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
            people_button_rect: Rect::ZERO,
            invite_rect: Rect::ZERO,
            arrival_rect: Rect::ZERO,
            arrival_pressable: false,
            people_left_x: 0.0,
            sharing_indicator_rect: Rect::ZERO,
            continuity_rect: Rect::ZERO,
            continuity_dismiss_rect: Rect::ZERO,
            ask: avatar_ask::AskRects::default(),
            activity_headline: None,
            activity_unread: 0,
            activity_pulse: 0,
            activity_started: None,
            center_fade: ui::motion::CrossFade::new(false),
            with_window: ui::motion::Presence::opened(),
            center_width: ui::motion::Extent::default(),
            center_rect: Rect::ZERO,
            center_lane: Rect::ZERO,
            last_cursor: None,
            focused: false,
            focused_control: None,
            focus_origin: FocusOrigin::Placed,
            time_wake: None,
            strips: strips::BarStrips::default(),
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
            faces: self
                .face_looks
                .iter()
                .map(|look| {
                    (
                        look.dimmed,
                        // A breath in eight steps, a bounce in thousandths.
                        look.call
                            .map(|level| ui::num::u8_of_f64(f64::from(level) * 8.0)),
                        u16::try_from(ui::num::u32_of_f64(look.scale * 1000.0)).unwrap_or(u16::MAX),
                    )
                })
                .collect(),
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
            notifications_only: self.bell.only,
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
            center_width: self.center_width_key(),
            app_tray_revision: self.app_tray_revision(),
            continuity: self.continuity_label(),
            avatar_ask: self.chrome_avatar_ask(),
            strips: self.strips_key(),
            tilo_mark: self.tilo_mark,
            own: self.own_chip(),
            hovered_control: self.hovered_control(),
            can_invite: self.can_invite(),
            wave_notice: self.wave_notice_text(ui::time::now()),
            arrival: self.arrival_shown(ui::time::now()).map(|shown| shown.key()),
        }
    }

    /// Wake again when the time reading just drawn goes stale.
    fn schedule_time_wake(&mut self) {
        let since_epoch = Duration::from_nanos(ui::time::unix_now_ns());
        let bucket = time_bucket_secs(self.activity_is_visible());
        self.time_wake = ui::time::now().checked_add(until_next_time_bucket(since_epoch, bucket));
    }

    /// Whether the time reading went stale since the bar last drew. Moves
    /// the wake on, so a bar that is not drawn (hidden under fullscreen, a
    /// locked session) never holds an overdue deadline.
    fn take_time_wake(&mut self) -> bool {
        if self.time_wake.is_none_or(|at| ui::time::now() < at) {
            return false;
        }
        self.schedule_time_wake();
        self.rect.height() > 0.0 && self.rect.width() > 0.0
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
        let stack_right_x = self.draw_avatar_ask(
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
        let people_left_x = self.draw_people_cluster(
            scene,
            transform,
            theme,
            text_renderer,
            (center_y, stack_right_x),
        );
        // The notices beside the faces come and go on their own clock, and
        // what stands left of one moves with it: one part of the bar, so
        // that change damages where they are painted, not the whole bar.
        ui::scene_parts::part_drawn(
            scene,
            ui::scene_parts::PartKey::new("desktop.topbar.notices", ()),
            |scene| {
                let notices_left_x = self.draw_arrival_notice(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    (center_y, people_left_x),
                );
                self.people_left_x = self.draw_wave_notice(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    (center_y, notices_left_x),
                );
                let continuity = self.continuity_label();
                self.draw_continuity_chip(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    continuity.as_deref(),
                );
                self.notices_painted(people_left_x)
            },
        );
    }

    /// Where the notices left of the faces that start at `faces_x` are
    /// painted: the arrival and wave notices and the continuity offer, the
    /// bar's height across.
    fn notices_painted(&self, faces_x: f64) -> Vec<Rect> {
        let left = if self.continuity_rect.is_zero_area() {
            self.people_left_x
        } else {
            self.people_left_x.min(self.continuity_rect.x0)
        };
        (left < faces_x)
            .then(|| {
                Rect::new(
                    left - 4.0,
                    self.rect.y0 - 2.0,
                    faces_x + 4.0,
                    self.rect.y1 + 2.0,
                )
            })
            .into_iter()
            .collect()
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
        // This person is their own avatar beside the stack, never a face in
        // it, and so are their other devices.
        let local = self.explicit_local_user.as_deref();
        let present = |c: &&tilo_desktop_model::Collaborator| {
            c.id != "me" && Some(c.id.as_str()) != local && !c.invited && !c.own_device
        };
        self.collaborator_initials.clear();
        self.collaborator_names.clear();
        self.collaborator_colors.clear();
        self.collaborator_agents.clear();
        self.collaborator_ids.clear();
        let now = tilo_clock::unix_now();
        // Counted with the same `present` filter as the avatars, so the
        // overflow chip agrees with the row.
        let peer_count = roster.iter().filter(present).count();
        // Settled over the whole room, so a peer's avatar matches the
        // colour their cursor is drawn in elsewhere on the desktop.
        let colors = roster_colors(roster, local.unwrap_or(""));
        let (shown, _) = ui::presence::FaceStack::BAR.split(peer_count);
        for c in roster.iter().filter(present).take(shown) {
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
            self.collaborator_ids.push(c.id.clone());
        }
        self.collaborator_overflow = peer_count.saturating_sub(self.collaborator_initials.len());
        self.sync_strips(tree);
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
    /// A running application's tray item stands on the bar as its own
    /// control: `tray:<key>` names the one the host addresses.
    const APP_TRAY_TARGET: &'static str = "tray:";

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

    /// A window maximizing over the bar's display (`shown` false) or
    /// restoring (`shown` true), moved by `source`: the bar leaves or comes
    /// back with it, over the window's own motion. A key's lands.
    pub fn move_with_window(
        &mut self,
        shown: bool,
        motion: ui::tokens::EffectiveMotion,
        source: ui::tokens::MotionSource,
    ) {
        self.with_window.show(
            shown,
            ui::motion::WindowMotion::MOVE,
            motion,
            source,
            ui::motion::frame_now(),
        );
    }

    fn with_window_moving(&self) -> bool {
        self.with_window.is_moving(ui::motion::frame_now())
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

    /// The end of the time reading on screen. Once it passes it stays
    /// reported as due, so the host runs a frame; that frame's `update`
    /// reports the bar dirty and schedules the next one. Hiding a passed
    /// deadline let the loop sleep through the minute until unrelated input
    /// woke it, and the clock read the minute before.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = ui::time::now();
        // Someone here is in a call: their ring breathes, gently.
        let breath = self.call_since.and_then(|_| {
            ui::motion::Breathe::next_frame(
                now,
                self.cache_theme.as_ref().map_or_else(
                    || ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
                    Theme::motion,
                ),
            )
        });
        [
            self.time_wake,
            breath,
            self.wave_notice_ends(now),
            self.arrival_notice_ends(now),
        ]
        .into_iter()
        .flatten()
        .min()
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
        presence: &PresenceCtx,
    ) {
        // Leaving with a maximizing window, or coming back with its
        // restore: the whole bar slides up off its display as it fades.
        if self.with_window_moving() {
            let open = self.with_window.progress(ui::motion::frame_now());
            let shown = ui::motion::unit_opacity(open);
            if shown <= f32::EPSILON {
                return;
            }
            let lift = vello::kurbo::Vec2::new(0.0, -(1.0 - open) * self.rect.height());
            out_scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                shown,
                out_transform,
                &self.rect,
            );
            self.draw_bar(
                out_scene,
                out_transform.pre_translate(lift),
                text_renderer,
                theme,
                presence,
            );
            out_scene.pop_layer();
            return;
        }
        if self.presence() == TopBarPresence::Hidden && !self.with_window.is_open() {
            return;
        }
        self.draw_bar(out_scene, out_transform, text_renderer, theme, presence);
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
            // Moving through the focused bar with the keyboard is reading it
            // with the keyboard; focus the shell merely placed here, or a
            // click left, is not, until such a key arrives. Any other key
            // (Escape closing a menu, a shortcut) moves nothing and lights
            // nothing: focus-visible follows keyboard navigation only.
            WindowEvent::KeyboardInput { event, .. }
                if self.focused
                    && event.state == winit::event::ElementState::Pressed
                    && navigates(&event.logical_key) =>
            {
                self.focus_origin = FocusOrigin::Keyboard;
            }
            _ => {}
        }
        if self.handle_workspace_rename_event(event, ctx) {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(ctx.logical(position));
        }
        if self.handle_avatar_ask_event(event, ctx.handled) {
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
            ("topbar_ask".to_string(), self.ask.pill),
            ("topbar_ask_answer".to_string(), self.ask.answer),
            ("topbar_ask_later".to_string(), self.ask.later),
            ("topbar_center_clock".to_string(), self.center_rect),
            ("topbar_arrival".to_string(), self.arrival_rect),
        ];
        rects.extend(self.workspace_rename_test_rects());
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
                    .with_action_target(target.clone())
                    .with_name(self.control_name(&target))
                    .with_bounds(self.control_rect(&target))
                    .with_state(AccessState {
                        focused: self.focused
                            && self.focused_control.as_deref() == Some(target.as_str()),
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
                // An icon carrying unread notifications says how many: the
                // bell's total alone never said whose they were.
                if let Some(key) = TopBarChrome::tray_target_key(&target)
                    && let Some(unread) = self
                        .app_tray
                        .iter()
                        .find(|slot| slot.key == key)
                        .and_then(|slot| slot.owner_app.as_deref())
                        .map(|app| self.app_badge(app))
                        .filter(|unread| *unread > 0)
                {
                    node = node.with_description(t_args(
                        "topbar-app-tray-unread",
                        &[("count", unread.min(99).into())],
                    ));
                }
                // Each face in the people control is someone here: say who.
                if target == Self::CONTROL_COLLABORATORS {
                    node = node.with_children(self.face_accessibility());
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
        children.extend(self.workspace_rename_accessibility());
        children.extend(self.arrival_accessibility());
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
        let Some(target) = self.control_targets().get(index).cloned() else {
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
        if target == Self::ARRIVAL_TARGET {
            return self.arrival_pressable
                && self
                    .chrome
                    .as_ref()
                    .is_some_and(|chrome| chrome.intent(ChromeIntent::ToggleProfile));
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
        self.control_targets()
            .into_iter()
            .any(|target| self.control_rect(&target).contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        // Keyboard traversal reaches the primary display's full bar; a
        // compact bar repeats a subset of it.
        !self.is_compact()
    }

    fn set_focus(&mut self, focused: bool) {
        if !focused {
            self.cancel_workspace_rename();
        }
        self.focused = focused;
        if !focused {
            self.focus_origin = FocusOrigin::Placed;
        }
        if focused && self.focused_control.is_none() {
            self.focused_control = self.control_targets().first().map(|target| target.clone());
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    /// Must keep returning `true` until the first successful draw
    /// fills the cache, or the first frame after layout never paints.
    fn needs_animation_frame(&self) -> bool {
        self.center_is_moving()
            || self.center_frame_stale()
            || self.with_window_moving()
            || self.arrival_notice_moving(ui::motion::frame_now())
    }

    fn needs_redraw(&self) -> bool {
        if self.center_is_moving()
            || self.center_frame_stale()
            || self.with_window_moving()
            || self.arrival_notice_moving(ui::motion::frame_now())
        {
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
            || self.chrome_avatar_ask() != self.cache_fp.avatar_ask
        {
            return true;
        }
        if let Some(started) = self.media_started {
            let elapsed = ui::time::now().saturating_duration_since(started);
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

impl TopBarChrome {
    fn draw_bar(
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
        // A hidden bar paints nothing until it is reached for, unless it
        // is leaving with a maximizing window.
        if self.presence() == TopBarPresence::Hidden && !self.with_window_moving() {
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
        self.read_face_looks(presence, theme.motion());
        let fp = self.current_fingerprint();
        if fp == self.cache_fp && self.cache_theme.as_ref() == Some(theme) && fp.rect_px.2 > 0 {
            ui::scene_parts::append(
                out_scene,
                &self.cached,
                self.cached_parts.as_ref(),
                Some(out_transform),
            );
            return;
        }

        // Theme typography already includes accessibility scale.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);

        // Cache miss — encode at IDENTITY, then splice with `out_transform`.
        let mut local = std::mem::take(&mut self.cached);
        local.reset();
        let ((), parts) = ui::scene_parts::record(&mut local, |scene| {
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
            // The strip shown changes as the person moves between strips: it
            // is one part of the bar, so that change damages where it is
            // painted rather than the whole bar.
            let strip_span = (
                self.ws_pill_rect.x1,
                center_left - f64::from(theme.tokens.spacing.s4),
            );
            ui::scene_parts::part_drawn(
                scene,
                ui::scene_parts::PartKey::new("desktop.topbar.strips", ()),
                |scene| {
                    // The strip shown, named after the workspace it is in: the
                    // frequent level, where the person looks for "where am I".
                    self.draw_strip_name(
                        scene,
                        transform,
                        theme,
                        text_renderer,
                        center_y,
                        strip_span,
                    );
                    self.strips_painted()
                        .into_iter()
                        .map(|rect| transform.transform_rect_bbox(rect))
                        .collect()
                },
            );

            self.draw_center_widget(scene, transform, theme, text_renderer, center_y);
            if !self.is_compact() {
                self.draw_session_clusters(scene, theme, text_renderer, center_y);
            }
            self.draw_focus_indicator(scene, transform, theme);
            let clock_activity = self.chrome.as_ref().and_then(|chrome| {
                let snapshot = chrome.snapshot();
                snapshot.overlays.clock.then_some(snapshot.clock_activity)?
            });
            if let Some(activity) = clock_activity {
                self.draw_clock_overlay(scene, transform, text_renderer, theme, &activity);
            }
        });
        ui::scene_parts::append(out_scene, &local, parts.as_ref(), Some(out_transform));
        self.cached = local;
        self.cached_parts = parts;
        self.cache_fp = fp;
        self.cache_theme = Some(theme.clone());
        text_renderer.set_text_scale(renderer_scale);
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "layout tests assert exact token-derived pixel values"
)]
mod tests;

#[cfg(test)]
mod fixture;

#[cfg(test)]
mod narrow_tests;

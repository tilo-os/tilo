//! Rendering widget for the shell top bar.
//!
//! `TopBar` in `shell/topbar.rs` owns the logical state (workspace nav,
//! center-widget cooldown, tooltip timing, collaborator helpers). This file
//! provides a [`TopBarChrome`] that implements [`ui::widget::Widget`] and
//! paints the bar onto the root scene.

use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use chrono::Local;
use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Brush, ColorStop, Extend, Fill, Gradient};
use winit::event::WindowEvent;

use ui::color_compat::ColorCompat;
use ui::presence::{PresenceCtx, user_color};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::peniko::Color;

use crate::shell::chrome_state::Chrome;
use crate::shell::state::WorkspaceTree;

/// Persistent chrome is deliberately a little taller than the original
/// concept's 40 px sketch.  Forty pixels forced the navigation and status
/// controls down to 18--22 px targets; 48 px leaves a consistent 32 px target
/// while retaining substantially more room for content than a conventional
/// desktop panel.
pub const TOPBAR_HEIGHT_PX: f32 = 48.0;

const CLOCK_OVERLAY_HEIGHT: f64 = 150.0;

/// The toolkit's shared chrome opacity, so the bar cannot drift from the
/// popovers hanging off it. [`TopBarChrome::draw`] fills `surfaces.overlay`
/// directly and never consults this constant, which is why the dark bar
/// is darker than the desktop while a panel is lighter.
pub const TOP_BAR_SURFACE_ALPHA: f32 = ui::chrome::shell::CHROME_SURFACE_ALPHA;

/// Low enough that the sparkline reads as backdrop, not a fourth element
/// competing with the three metric labels.
const SPARKLINE_FILL_ALPHA: f32 = 0.30;

/// Scroll-wheel over the center band cycles between these; a three-dot
/// indicator shows the current position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CenterWidget {
    #[default]
    Clock,
    SystemMonitor,
    Calendar,
}

#[cfg(test)]
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

/// Compared field-for-field to decide whether [`TopBarChrome::cached`] can
/// be reused. Time fields split by mode since each variant consumes a
/// different grain of `chrono::Local::now()`.
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
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    collaborator_initials: Vec<char>,
    collaborator_names: Vec<String>,
    collaborator_overflow: usize,
    sharing: (usize, usize, usize),
    center_widget: CenterWidget,
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
    clock_activity: Option<(String, u64, bool, bool)>,
}

/// One dot in the centre band's carousel indicator. Paint and hit testing
/// intentionally share this geometry so the visible control and its target
/// cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterDot {
    pub center: Point,
    pub radius: f64,
    pub slot: Rect,
}

/// No calendar source is wired to the shell, so this paints what's
/// actually knowable rather than a plausible fixed string.
fn calendar_strip_parts(now: chrono::DateTime<Local>) -> (String, String, &'static str) {
    (
        now.format("%a").to_string(),
        now.format("%-d %B").to_string(),
        "No calendar connected",
    )
}

fn publish_clock_command(command: &str, activity_id: Option<&str>) {
    let Some(path) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .map(|p| p.join("tilo/clocks-command.json"))
    else {
        return;
    };
    let tmp = path.with_extension("json.new");
    let payload = serde_json::json!({
        "version": 1,
        "command": command,
        "activity_id": activity_id,
    });
    if std::fs::write(&tmp, payload.to_string()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
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

/// Buckets a `chrono` timestamp into a cache-fingerprint key.
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
fn status_key_of(s: &crate::shell::status::SystemStatus) -> StatusKey {
    StatusKey {
        battery_percent: s.battery_percent(),
        battery_charging: s.battery_is_charging(),
        battery_low: s.battery_is_low(),
        battery_critical: s.battery_is_critical(),
        link: s.link.as_ref().map(|l| {
            let kind = match l.kind {
                os_actions::LinkKind::Offline => 0,
                os_actions::LinkKind::Wired => 1,
                os_actions::LinkKind::Wifi => 2,
            };
            (kind, crate::shell::status::link_bars(l))
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

/// `WorkspaceTree`, not `TopBar`, lets `build_tilo_root` stay a pure
/// function of the tree and avoids a circular borrow against the
/// shell's owned logical `TopBar`.
pub struct TopBarChrome {
    explicit_local_user: Option<String>,
    rect: Rect,
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    /// Most of the bar is stable across frames, so caching cuts the
    /// draw budget at rest by an order of magnitude.
    cached: Scene,
    /// Reused iff every field matches `self.*` at draw time. See
    /// [`TopBarChrome::current_fingerprint`].
    cache_fp: CacheFingerprint,
    collaborator_initials: Vec<char>,
    collaborator_names: Vec<String>,
    /// Same index as `collaborator_initials`. Resolved from
    /// `cursor_color` when present, else [`ui::presence::user_color`].
    collaborator_colors: Vec<Color>,
    /// Collaborators beyond the three avatars; nonzero paints a "+N" chip.
    collaborator_overflow: usize,
    /// Copied off the chrome on each revision bump. Empty until
    /// something polls — an empty reading paints no indicator rather
    /// than a plausible default.
    status: crate::shell::status::SystemStatus,
    chrome: Option<Chrome>,
    synced_revision: u64,
    // Click geometry — recomputed every frame so it tracks DPI/resize.
    overview_btn_rect: Rect,
    up_btn_rect: Rect,
    down_btn_rect: Rect,
    ws_pill_rect: Rect,
    tray_pill_rect: Rect,
    media_indicator_rect: Rect,
    media_signature: Vec<(u32, u8, String)>,
    media_started: Option<Instant>,
    media_expanded: bool,
    notification_badge: u32,
    /// Latest durable due-event identity announced by this shell instance.
    clock_notified_id: Option<String>,
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
    /// Scroll over `center_rect` cycles it, dots jump directly.
    center_widget: CenterWidget,
    activity_headline: Option<String>,
    activity_unread: usize,
    activity_pulse: u64,
    activity_started: Option<Instant>,
    center_rect: Rect,
    center_dot_rects: [Rect; 3],
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
}

/// `surfaces.raised` is `#ffffff` on light, same as the bar's own
/// surface — invisible if used directly — so selection lift derives
/// from luminance instead.
fn bar_selector_body(
    theme: &Theme,
    state: ui::chrome::SelectionState,
) -> ui::chrome::SelectionTier {
    ui::chrome::selection_tier(theme, state)
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
    pct_size: f32,
    pct_label: Option<String>,
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
        let min_width = (220.0 + text_headroom).min(260.0);
        (self.rect.width() - 580.0).clamp(min_width, 360.0)
    }

    fn clock_viewport(&self) -> (f64, f64) {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().viewport_size())
            .unwrap_or_else(|| (self.rect.width(), f64::INFINITY))
    }

    fn publish_clock_activity_command(&self, timer: &str, stopwatch: &str) {
        let activity = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().clock_activity.clone());
        let is_stopwatch = activity.as_ref().is_some_and(|activity| activity.stopwatch);
        publish_clock_command(
            if is_stopwatch { stopwatch } else { timer },
            activity
                .as_ref()
                .and_then(|activity| activity.activity_id.as_deref()),
        );
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
                    chrome.borrow_mut().overlays.clock = false;
                }
                self.publish_clock_activity_command("close", "close-stopwatch");
            }
            _ => return,
        }
        self.cache_fp = CacheFingerprint::default();
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "bounded pointer coordinates minus bounded overlay coordinates produce a screen-space drag offset"
    )]
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
    pub fn from_tree(tree: &WorkspaceTree) -> Self {
        Self::from_tree_with_identity(tree, None)
    }

    pub(crate) fn from_tree_with_local_user(tree: &WorkspaceTree, local: String) -> Self {
        Self::from_tree_with_identity(tree, Some(local))
    }

    fn from_tree_with_identity(tree: &WorkspaceTree, explicit_local_user: Option<String>) -> Self {
        let mut this = Self {
            explicit_local_user,
            rect: Rect::ZERO,
            workspace_name: String::new(),
            workspace_index: 0,
            workspace_total: 1,
            cached: Scene::new(),
            cache_fp: CacheFingerprint::default(),
            collaborator_initials: Vec::new(),
            collaborator_names: Vec::new(),
            collaborator_colors: Vec::new(),
            collaborator_overflow: 0,
            status: crate::shell::status::SystemStatus::default(),
            chrome: None,
            synced_revision: 0,
            overview_btn_rect: Rect::ZERO,
            up_btn_rect: Rect::ZERO,
            down_btn_rect: Rect::ZERO,
            ws_pill_rect: Rect::ZERO,
            tray_pill_rect: Rect::ZERO,
            media_indicator_rect: Rect::ZERO,
            media_signature: Vec::new(),
            media_started: None,
            media_expanded: false,
            notification_badge: 0,
            clock_notified_id: None,
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
            center_widget: CenterWidget::Clock,
            activity_headline: None,
            activity_unread: 0,
            activity_pulse: 0,
            activity_started: None,
            center_rect: Rect::ZERO,
            center_dot_rects: [Rect::ZERO; 3],
            center_scroll_accum: 0.0,
            last_cursor: None,
            focused: false,
            focused_control: None,
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
        let time_bucket = if activity_visible {
            timestamp_bucket(chrono::Local::now().timestamp())
        } else {
            match self.center_widget {
                CenterWidget::Clock => {
                    // Minute granularity: Clock paints `HH:MM`.
                    let t = chrono::Local::now();
                    timestamp_bucket(t.timestamp()) / 60
                }
                CenterWidget::SystemMonitor => {
                    // Second granularity: sparkline derives from `seconds % N`.
                    timestamp_bucket(chrono::Local::now().timestamp())
                }
                CenterWidget::Calendar => {
                    // Day granularity: cache must drop at midnight.
                    timestamp_bucket(chrono::Local::now().timestamp()) / 86_400
                }
            }
        };
        CacheFingerprint {
            rect_px: (x0, y0, w, h),
            workspace_name: self.workspace_name.clone(),
            workspace_index: self.workspace_index,
            workspace_total: self.workspace_total,
            collaborator_initials: self.collaborator_initials.clone(),
            collaborator_names: self.collaborator_names.clone(),
            collaborator_overflow: self.collaborator_overflow,
            sharing: self.sharing_summary(),
            center_widget: self.center_widget,
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
                .is_some_and(|chrome| chrome.borrow().overlays.ws_settings),
            focused: self.focused,
            focused_control: self.focused_control.clone(),
            clock_activity: self.chrome.as_ref().and_then(|chrome| {
                chrome
                    .borrow()
                    .clock_activity
                    .as_ref()
                    .map(|a| (a.label.clone(), a.remaining_secs, a.overlay, a.paused))
            }),
        }
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
    fn live_status(&self) -> crate::shell::status::SystemStatus {
        match self.chrome.as_ref() {
            Some(c) => c.borrow().status.clone(),
            None => self.status.clone(),
        }
    }

    #[must_use]
    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        let rev = chrome.borrow().revision;
        // Declares this widget synced at the current revision, so
        // `update`'s sync branch won't re-run until it moves — status
        // must be seeded now.
        self.status = chrome.borrow().status.clone();
        self.chrome = Some(chrome);
        self.synced_revision = rev;
        self
    }

    pub fn sync_from_tree(&mut self, tree: &WorkspaceTree) {
        self.workspace_total = tree.workspaces.len().max(1);
        let active_ws = tree.workspaces.iter().find(|w| w.id == tree.active);
        let (idx, name) = tree
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, w)| w.id == tree.active)
            .map_or_else(
                || (0, String::from("Workspace")),
                |(i, w)| (i, w.name.clone()),
            );
        self.workspace_index = idx;
        self.workspace_name = name;
        // Presence is workspace-scoped. The legacy tree-level cache can
        // contain people active elsewhere and must not leak them into this bar.
        let roster: &[crate::shell::state::Collaborator] =
            active_ws.map_or(&[], |workspace| workspace.collaborators.as_slice());
        // An invited-but-not-arrived peer doesn't count as present.
        let present = |c: &&crate::shell::state::Collaborator| c.id != "me" && !c.invited;
        self.collaborator_initials.clear();
        self.collaborator_names.clear();
        self.collaborator_colors.clear();
        // Shared helper so the overflow chip and the wheel-disc's
        // active-card dot count agree.
        let peer_count = crate::shell::state::active_workspace_peer_count(tree);
        // Settled over the whole room, so a peer's avatar matches the
        // colour their cursor is drawn in elsewhere on the desktop.
        let colors = self.explicit_local_user.as_deref().map_or_else(
            || crate::shell::state::roster_colors(roster),
            |local| crate::shell::state::roster_colors_with_local_user(roster, local),
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
        }
        self.collaborator_overflow = peer_count.saturating_sub(3);
    }

    /// `TILO_CLOCK` pins it, for the same reason `TILO_SHOTS_THEME` pins
    /// the palette: a screenshot harness that compares whole frames
    /// needs to hold every part still, and a running clock would fail
    /// that comparison once a minute. Unset (every real session) reads
    /// the wall clock.
    fn current_clock_string() -> String {
        match std::env::var("TILO_CLOCK") {
            Ok(pinned) if !pinned.is_empty() => pinned,
            _ => Local::now().format("%H:%M").to_string(),
        }
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

    pub const DOT_BASE_SIZE: f64 = 3.0;
    /// 1.6× the base so the saturated indigo fill reads as focused.
    pub const DOT_ACTIVE_RADIUS: f64 = Self::DOT_BASE_SIZE * 1.6;
    pub const DOT_INACTIVE_RADIUS: f64 = Self::DOT_BASE_SIZE * 1.0;

    const DOT_COLUMN_MARGIN: f64 = 1.0;
    const DOT_SLOT_HALF_W: f64 = 10.0;

    /// Restore the accepted vertical carousel indicator from `7d58e7146`.
    /// The unequal active/inactive diameters are included in the layout so
    /// both visual gaps remain equal and the column stays centred.
    fn center_dot_layout(band: Rect, bar: Rect, active: usize) -> [CenterDot; 3] {
        let radius_of = |i: usize| {
            if i == active {
                Self::DOT_ACTIVE_RADIUS
            } else {
                Self::DOT_INACTIVE_RADIUS
            }
        };
        let ink: f64 = (0..3).map(|i| radius_of(i) * 2.0).sum();
        let gap = ((band.height() - Self::DOT_COLUMN_MARGIN * 2.0 - ink) / 2.0).max(1.0);
        let column_h = ink + gap * 2.0;
        let mut edge = band.center().y - column_h / 2.0;
        let mut centers = [0.0; 3];
        let mut radii = [0.0; 3];
        for i in 0..3 {
            let radius = radius_of(i);
            radii[i] = radius;
            centers[i] = edge + radius;
            edge += radius * 2.0 + gap;
        }
        let boundary = |a: usize, b: usize| (centers[a] + radii[a] + centers[b] - radii[b]) / 2.0;
        let cuts = [boundary(0, 1), boundary(1, 2)];
        let x = Self::dot_column_x(band);
        let edges = [bar.y0, cuts[0], cuts[1], bar.y1];
        std::array::from_fn(|i| CenterDot {
            center: Point::new(x, centers[i]),
            radius: radii[i],
            slot: Rect::new(
                x - Self::DOT_SLOT_HALF_W,
                edges[i],
                x + Self::DOT_SLOT_HALF_W,
                edges[i + 1],
            ),
        })
    }

    fn dot_column_x(band: Rect) -> f64 {
        band.x0 + 12.0
    }

    /// Strictly larger than inactive; the affordance is scale-up +
    /// saturated indigo.
    pub fn active_dot_radius(&self) -> f64 {
        Self::DOT_ACTIVE_RADIUS
    }

    pub fn inactive_dot_radius(&self) -> f64 {
        Self::DOT_INACTIVE_RADIUS
    }

    /// Pinned so a future revert to the old four-rect quad fails loudly
    /// instead of silently regressing.
    pub fn overview_glyph_icon() -> ui::icons::Icon {
        ui::icons::Icon::LayoutGrid
    }

    /// Synthetic control ids are deliberately stable: unlike a child index,
    /// they survive the optional avatar/media controls appearing and
    /// disappearing between frames.
    const CONTROL_OVERVIEW: &'static str = "topbar:overview";
    const CONTROL_WORKSPACE_UP: &'static str = "topbar:workspace-up";
    const CONTROL_WORKSPACE_DOWN: &'static str = "topbar:workspace-down";
    const CONTROL_WORKSPACE_SETTINGS: &'static str = "topbar:workspace-settings";
    const CONTROL_CENTER_CLOCK: &'static str = "topbar:center:clock";
    const CONTROL_CENTER_MONITOR: &'static str = "topbar:center:system-monitor";
    const CONTROL_CENTER_CALENDAR: &'static str = "topbar:center:calendar";
    const CONTROL_QUICK_SETTINGS: &'static str = "topbar:quick-settings";
    const CONTROL_COLLABORATORS: &'static str = "topbar:collaborators";
    const CONTROL_MEDIA: &'static str = "topbar:media";

    fn collaborators_visible(&self) -> bool {
        !self.collaborator_initials.is_empty()
            || self.collaborator_overflow > 0
            || self.sharing_summary().0 > 0
    }

    /// `(viewers, controllers, surfaces)` from the compositor authority.
    fn sharing_summary(&self) -> (usize, usize, usize) {
        let Some(chrome) = self.chrome.as_ref() else {
            return (0, 0, 0);
        };
        let state = chrome.borrow();
        let mut peers = state
            .active_exposures()
            .iter()
            .map(|row| row.peer)
            .collect::<Vec<_>>();
        peers.sort_unstable();
        peers.dedup();
        let mut controllers = state
            .active_exposures()
            .iter()
            .filter(|row| row.can_control)
            .map(|row| row.peer)
            .collect::<Vec<_>>();
        controllers.sort_unstable();
        controllers.dedup();
        let mut surfaces = state
            .active_exposures()
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
        let state = chrome.borrow();
        let mut authority = Vec::<(u64, bool)>::new();
        for row in state.active_exposures() {
            if let Some((_, can_control)) = authority.iter_mut().find(|(peer, _)| *peer == row.peer)
            {
                *can_control |= row.can_control;
            } else {
                authority.push((row.peer, row.can_control));
            }
        }
        authority.sort_unstable_by_key(|(peer, _)| *peer);
        let tree = state.tree();
        let mut viewers = Vec::new();
        for (peer, can_control) in authority {
            if let Some((workspace, participant)) = tree.workspaces.iter().find_map(|workspace| {
                workspace
                    .collaborators
                    .iter()
                    .find(|person| person.session == Some(peer))
                    .map(|participant| (workspace, participant))
            }) {
                let is_agent = workspace.agent_members().iter().any(|(id, _)| {
                    id.eq_ignore_ascii_case(&participant.id)
                        || id.eq_ignore_ascii_case(&participant.name)
                });
                viewers.push((peer, participant.name.clone(), Some(is_agent), can_control));
            } else {
                viewers.push((
                    peer,
                    crate::shell::state::session_handle(peer),
                    None,
                    can_control,
                ));
            }
        }
        viewers
    }

    fn sharing_targets_for_peer(&self, peer: u64) -> (Vec<String>, Vec<String>) {
        let Some(chrome) = self.chrome.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let state = chrome.borrow();
        let mut authority = Vec::<(u64, bool)>::new();
        for row in state
            .active_exposures()
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
        let tree = state.tree();
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
            [(_, name, Some(true), true)] => format!("AGENT CONTROLS · {name}"),
            [(_, name, Some(true), false)] => format!("AGENT VIEWING · {name}"),
            [(_, name, Some(false), true)] => format!("CONTROLS · {name}"),
            [(_, name, Some(false), false)] => format!("VIEWING · {name}"),
            [(_, name, None, true)] => format!("SESSION CONTROLS · {name}"),
            [(_, name, None, false)] => format!("SESSION VIEWING · {name}"),
            _ if controllers > 0 => format!("{controllers} CONTROL · {viewers} VIEWING"),
            _ => format!("{viewers} VIEWING"),
        })
    }

    fn sharing_target_labels(
        tree: &crate::shell::state::WorkspaceTree,
        surfaces: Vec<u64>,
    ) -> Vec<String> {
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
                                    crate::shell::state::TileContent::Foreign { surface_id, .. }
                                        if *surface_id == surface
                                )
                            })
                            .map(|tab| (workspace, tab))
                    })
                    .map_or_else(
                        || format!("surface {surface}"),
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
    fn control_targets(&self) -> Vec<&'static str> {
        let mut controls = vec![
            Self::CONTROL_OVERVIEW,
            Self::CONTROL_WORKSPACE_UP,
            Self::CONTROL_WORKSPACE_DOWN,
            Self::CONTROL_WORKSPACE_SETTINGS,
            Self::CONTROL_CENTER_CLOCK,
            Self::CONTROL_CENTER_MONITOR,
            Self::CONTROL_CENTER_CALENDAR,
            Self::CONTROL_QUICK_SETTINGS,
        ];
        if self.collaborators_visible() {
            controls.push(Self::CONTROL_COLLABORATORS);
        }
        if !self.media_indicator_rect.is_zero_area() {
            controls.push(Self::CONTROL_MEDIA);
        }
        controls
    }

    fn control_rect(&self, target: &str) -> Rect {
        match target {
            Self::CONTROL_OVERVIEW => self.overview_btn_rect,
            Self::CONTROL_WORKSPACE_UP => self.up_btn_rect,
            Self::CONTROL_WORKSPACE_DOWN => self.down_btn_rect,
            Self::CONTROL_WORKSPACE_SETTINGS => self.ws_pill_rect,
            Self::CONTROL_CENTER_CLOCK
            | Self::CONTROL_CENTER_MONITOR
            | Self::CONTROL_CENTER_CALENDAR => {
                let index = match target {
                    Self::CONTROL_CENTER_CLOCK => 0,
                    Self::CONTROL_CENTER_MONITOR => 1,
                    _ => 2,
                };
                self.center_dot_rects
                    .get(index)
                    .copied()
                    .unwrap_or(Rect::ZERO)
            }
            Self::CONTROL_QUICK_SETTINGS => self.tray_pill_rect,
            Self::CONTROL_COLLABORATORS => {
                if self.avatar_stack_rect.is_zero_area() {
                    self.sharing_indicator_rect
                } else if self.sharing_indicator_rect.is_zero_area() {
                    self.avatar_stack_rect
                } else {
                    self.avatar_stack_rect.union(self.sharing_indicator_rect)
                }
            }
            Self::CONTROL_MEDIA => self.media_indicator_rect,
            _ => Rect::ZERO,
        }
    }

    fn control_name(&self, target: &str) -> String {
        match target {
            Self::CONTROL_OVERVIEW => "Open overview".into(),
            Self::CONTROL_WORKSPACE_UP => "Previous workspace".into(),
            Self::CONTROL_WORKSPACE_DOWN => "Next workspace".into(),
            Self::CONTROL_WORKSPACE_SETTINGS => {
                format!("Workspace settings for {}", self.workspace_name)
            }
            Self::CONTROL_CENTER_CLOCK => "Show clock".into(),
            Self::CONTROL_CENTER_MONITOR => "Show system monitor".into(),
            Self::CONTROL_CENTER_CALENDAR => "Show calendar".into(),
            Self::CONTROL_QUICK_SETTINGS => "Open quick settings".into(),
            Self::CONTROL_COLLABORATORS => {
                let named = self.sharing_viewers();
                if named.is_empty() {
                    "Show collaborators".into()
                } else {
                    let viewers = named
                        .iter()
                        .map(|(peer, name, is_agent, _)| {
                            let kind = match is_agent {
                                Some(true) => "agent",
                                Some(false) => "person",
                                None => "unidentified session",
                            };
                            let (view_only, controlled) = self.sharing_targets_for_peer(*peer);
                            let mut access = Vec::new();
                            if !controlled.is_empty() {
                                access.push(format!("can control {}", controlled.join(", ")));
                            }
                            if !view_only.is_empty() {
                                access.push(format!("view only {}", view_only.join(", ")));
                            }
                            format!("{name} ({kind}): {}", access.join("; "))
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!("Screen visible to {viewers}. Show collaborators")
                }
            }
            Self::CONTROL_MEDIA => "Privacy settings for active capture".into(),
            _ => "Top bar control".into(),
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

    /// Bold, so it reads as the chrome's focal glance value.
    pub fn clock_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Semibold — a clickable title without tipping into the clock's
    /// h1/h2 tier.
    pub fn workspace_name_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    /// Distinct from the bold values so the strip is scannable without
    /// every label shouting.
    pub fn system_monitor_label_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    pub fn system_monitor_value_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Routes through the theme's semantic colours rather than a
    /// literal hex, so the cue tracks the theme under high-contrast.
    pub fn metric_label_color(&self, theme: &Theme, k: MetricKind) -> Color {
        let semantic = &theme.tokens.colors.semantic;
        match k {
            MetricKind::Cpu => semantic.success,
            MetricKind::Ram => semantic.info,
            MetricKind::Tmp => semantic.warning,
        }
    }

    /// Always the CPU colour at low weight regardless of what the
    /// chips show — a backdrop, not a competing element. `_k` kept so a
    /// per-metric variant can be wired in later.
    pub fn sparkline_fill(&self, theme: &Theme, _k: MetricKind) -> Color {
        self.metric_label_color(theme, MetricKind::Cpu)
            .with_alpha_factor(SPARKLINE_FILL_ALPHA)
    }

    pub fn overview_btn_rect(&self) -> Rect {
        self.overview_btn_rect
    }

    /// See [`TOP_BAR_SURFACE_ALPHA`]; pinned by `shell_chrome_test` so
    /// the bar can't drift from the constant the popovers share.
    pub fn top_bar_alpha() -> f32 {
        TOP_BAR_SURFACE_ALPHA
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

    /// Bar surface fill, bottom border hairline, and the soft drop-shadow
    /// band beneath the bar.
    fn draw_backdrop(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &self.rect,
        );

        let border_rect = Rect::new(self.rect.x0, self.rect.y1 - 1.0, self.rect.x1, self.rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.border_standard,
            None,
            &border_rect,
        );

        // Masks the abrupt topbar → backdrop transition via a 28px fade.
        let shadow_h = 28.0_f64;
        let shadow_rect = Rect::new(
            self.rect.x0,
            self.rect.y1,
            self.rect.x1,
            self.rect.y1 + shadow_h,
        );
        let shadow_grad = Gradient::new_linear(
            Point::new(0.0, self.rect.y1),
            Point::new(0.0, self.rect.y1 + shadow_h),
        )
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: Color::from_rgba8(0, 0, 0, 46).into(), // ≈ 0.18
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::from_rgba8(0, 0, 0, 0).into(),
                },
            ][..],
        );
        scene.fill(
            Fill::NonZero,
            transform,
            &Brush::Gradient(shadow_grad),
            None,
            &shadow_rect,
        );
    }

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
        let icon_size = 18.0_f64;
        let control = 32.0_f64;

        let mut x = self.rect.x0 + pad_x;
        let switch_w = 32.0_f64;
        let nav_width = control
            + f64::from(tokens.spacing.s2)
            + 1.0
            + f64::from(tokens.spacing.s2)
            + switch_w * 2.0;
        let nav_rect = Rect::new(
            x,
            center_y - control / 2.0,
            x + nav_width,
            center_y + control / 2.0,
        );
        let nav_body = bar_selector_body(theme, ui::chrome::SelectionState::active(false));
        ui::chrome::shell::paint_glass_body(
            scene,
            transform,
            nav_rect,
            ui::chrome::ChromeRole::Selector.radius(theme, nav_rect.height()),
            theme,
            &nav_body,
            1.0,
        );

        self.overview_btn_rect = Rect::new(
            x,
            center_y - control / 2.0,
            x + control,
            center_y + control / 2.0,
        );
        let overview_open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().overlays.overview);
        if overview_open {
            let selected = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
            ui::chrome::shell::paint_glass_body(
                scene,
                transform,
                self.overview_btn_rect,
                ui::chrome::ChromeRole::Selector.radius(theme, self.overview_btn_rect.height()),
                theme,
                &selected,
                1.0,
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

        // Vertical divider
        let divider = Rect::new(x, center_y - 8.0, x + 1.0, center_y + 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.border_standard,
            None,
            &divider,
        );
        x += 1.0 + f64::from(tokens.spacing.s2);

        let chev = 14.0_f64;
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
                colors.border_standard
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
                colors.border_standard
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x + switch_w + f64::from(tokens.spacing.s3)
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
        let ws_text_size = tokens.typography.scale.sm;
        let position_suffix = format!(
            "  {}/{}",
            self.workspace_index.saturating_add(1),
            self.workspace_total
        );
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
        let label_budget = ui::num::f32_of_f64((center_left - x - 54.0).clamp(32.0, 260.0));
        let suffix_width = text_renderer
            .measure_weighted(&position_suffix, ws_text_size, None, ws_weight)
            .0;
        let name_budget = (label_budget - suffix_width).max(0.0);
        let visible_name = text_renderer.elide_to_width_weighted(
            &self.workspace_name,
            ws_text_size,
            name_budget,
            ws_weight,
        );
        let ws_label = format!("{visible_name}{position_suffix}");
        let (ws_w, ws_h) = text_renderer.measure_weighted(&ws_label, ws_text_size, None, ws_weight);
        let ws_y = center_y - f64::from(ws_h) / 2.0;
        let ws_pill_outer = Rect::new(
            x - 6.0,
            center_y - 16.0,
            x + f64::from(ws_w) + 28.0,
            center_y + 16.0,
        );
        self.ws_pill_rect = ws_pill_outer;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().workspace_settings_anchor = ws_pill_outer;
        }
        // A thing you choose between is a capsule (HIG §9.1, Selector).
        let pill = RoundedRect::from_rect(
            ws_pill_outer,
            ui::chrome::ChromeRole::Selector.radius(theme, ws_pill_outer.height()),
        );
        let body = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
        scene.fill(Fill::NonZero, transform, body.fill, None, &pill);
        if let Some(edge) = body.border {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                edge,
                None,
                &pill,
            );
        }
        let workspace_lens_open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().overlays.ws_settings);
        if workspace_lens_open {
            let context_rule = RoundedRect::from_rect(
                Rect::new(
                    ws_pill_outer.x0 + 8.0,
                    ws_pill_outer.y1 - 2.5,
                    ws_pill_outer.x1 - 8.0,
                    ws_pill_outer.y1 - 0.5,
                ),
                1.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &context_rule,
            );
        }
        text_renderer.draw_text_weighted(
            scene,
            &ws_label,
            ws_text_size,
            colors.text_primary,
            at(transform, (x, ws_y)),
            None,
            ws_weight,
        );
        let gear_x = x + f64::from(ws_w) + 6.0;
        let gear_rect = Rect::new(gear_x - 2.0, center_y - 7.0, gear_x + 12.0, center_y + 7.0);
        ui::icons::draw_icon(
            scene,
            transform,
            gear_rect,
            ui::icons::Icon::Settings2,
            if workspace_lens_open {
                colors.accent[5]
            } else {
                colors.text_dim
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// Carousel band background + the 3 position dots. Returns
    /// `(content_x0, content_x1, content_cx)` — the bounds the active
    /// center widget draws into, to the right of the dots.
    fn draw_center_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
    ) -> (f64, f64, f64) {
        let colors = &theme.tokens.colors;
        let band_w = self.center_band_width(theme);
        let band_h = 34.0_f64;
        let band_cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let band = Rect::new(
            band_cx - band_w / 2.0,
            center_y - band_h / 2.0,
            band_cx + band_w / 2.0,
            center_y + band_h / 2.0,
        );
        self.center_rect = band;
        // A Selector: you scroll between its three widgets.
        let band_rr = RoundedRect::from_rect(
            band,
            ui::chrome::ChromeRole::Selector.radius(theme, band.height()),
        );
        let body = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
        scene.fill(Fill::NonZero, transform, body.fill, None, &band_rr);
        if let Some(edge) = body.border {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                edge,
                None,
                &band_rr,
            );
        }

        let active_dot = self.center_widget.index();
        let dots = Self::center_dot_layout(band, self.rect, active_dot);
        for (i, dot) in dots.iter().enumerate() {
            let active = active_dot == i;
            let color = if active {
                colors.accent[5]
            } else {
                colors.neutral[7]
            };
            self.center_dot_rects[i] = dot.slot;
            let c = vello::kurbo::Circle::new(dot.center, dot.radius);
            scene.fill(Fill::NonZero, transform, color, None, &c);
        }

        // Widget content fills the band to the right of the dots.
        let content_x0 = Self::dot_column_x(band) + 24.0;
        let content_x1 = band.x1 - 12.0;
        let content_cx = f64::midpoint(content_x0, content_x1);
        (content_x0, content_x1, content_cx)
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
        let s = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().clock_activity.clone())
            .map_or_else(Self::current_clock_string, |activity| {
                let mins = activity.remaining_secs / 60;
                let secs = activity.remaining_secs % 60;
                if activity.label.is_empty() {
                    format!("{mins:02}:{secs:02}")
                } else {
                    format!("{}  {mins:02}:{secs:02}", activity.label)
                }
            });
        let size = tokens.typography.scale.base;
        // Measured at the draw weight — Bold advances wider
        // than Regular, so centering would drift otherwise.
        let weight = self.clock_text_weight();
        let (tw, th) = text_renderer.measure_weighted(&s, size, None, weight);
        let tx = content_cx - f64::from(tw) / 2.0;
        let ty = center_y - f64::from(th) / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            &s,
            size,
            colors.text_primary,
            at(transform, (tx, ty)),
            None,
            weight,
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
        text_renderer.draw_text_weighted(
            scene,
            spec.label,
            tokens.typography.scale.xs,
            self.metric_label_color(theme, spec.kind),
            at(transform, (spec.label_x, spec.center_y - 6.0)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            spec.value_str,
            tokens.typography.scale.sm,
            colors.text_primary,
            at(transform, (spec.value_x, spec.center_y - 7.0)),
            None,
            TextWeight::Bold,
        );
    }

    /// The deterministic-but-varied CPU sparkline bars between
    /// `content_x0 + 70` and `content_x1 - 100`.
    fn draw_cpu_sparkline(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
        content_x: (f64, f64),
        secs: u32,
    ) {
        let (content_x0, content_x1) = content_x;
        let spark_x0 = content_x0 + 70.0;
        let spark_x1 = content_x1 - 100.0;
        let spark_w = (spark_x1 - spark_x0).max(40.0);
        let bars = 20usize;
        let bar_gap = 1.0;
        let bar_w =
            (spark_w - (small_count_to_f64(bars) - 1.0) * bar_gap) / small_count_to_f64(bars);
        for i in 0..bars {
            // Deterministic but varied so the strip looks "live".
            let seed = (secs as usize).saturating_add(i.saturating_mul(7)) % 100;
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
        // Values derive from clock seconds, so nothing repaints
        // every frame yet values change over time.
        let t = Local::now();
        let secs = t.format("%S").to_string().parse::<u32>().unwrap_or(0);
        let cpu = 12u32.saturating_add(secs % 20);
        let ram = 45u32.saturating_add(secs % 8);
        let tmp = 40u32.saturating_add(secs % 11);
        let cpu_str = format!("{cpu}%");
        let ram_str = format!("{ram}%");
        let tmp_str = format!("{tmp}°");
        self.draw_metric_label(
            scene,
            transform,
            theme,
            text_renderer,
            &MetricLabelSpec {
                center_y,
                label_x: content_x0,
                value_x: content_x0 + 26.0,
                label: "CPU",
                value_str: &cpu_str,
                kind: MetricKind::Cpu,
            },
        );
        self.draw_cpu_sparkline(scene, transform, theme, center_y, content_x, secs);
        self.draw_metric_label(
            scene,
            transform,
            theme,
            text_renderer,
            &MetricLabelSpec {
                center_y,
                label_x: content_x1 - 96.0,
                value_x: content_x1 - 70.0,
                label: "RAM",
                value_str: &ram_str,
                kind: MetricKind::Ram,
            },
        );
        self.draw_metric_label(
            scene,
            transform,
            theme,
            text_renderer,
            &MetricLabelSpec {
                center_y,
                label_x: content_x1 - 40.0,
                value_x: content_x1 - 18.0,
                label: "TMP",
                value_str: &tmp_str,
                kind: MetricKind::Tmp,
            },
        );
    }

    fn draw_calendar_widget(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        content_cx: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let (weekday, date, note) = calendar_strip_parts(Local::now());
        let prefix = weekday.as_str();
        let event = date.as_str();
        let time = note;
        let size = tokens.typography.scale.sm;
        // prefix/time dim Medium, event a Semibold primary anchor.
        let prefix_weight = TextWeight::Medium;
        let event_weight = TextWeight::Semibold;
        let time_weight = TextWeight::Medium;
        let (pw, ph) = text_renderer.measure_weighted(prefix, size, None, prefix_weight);
        let (ew, _) = text_renderer.measure_weighted(event, size, None, event_weight);
        let (tw, _) =
            text_renderer.measure_weighted(time, tokens.typography.scale.xs, None, time_weight);
        let gap = 8.0;
        let total = f64::from(pw) + gap + f64::from(ew) + gap + f64::from(tw);
        let mut tx = content_cx - total / 2.0;
        let ty = center_y - f64::from(ph) / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            prefix,
            size,
            colors.text_dim,
            at(transform, (tx, ty)),
            None,
            prefix_weight,
        );
        tx += f64::from(pw) + gap;
        text_renderer.draw_text_weighted(
            scene,
            event,
            size,
            colors.text_primary,
            at(transform, (tx, ty)),
            None,
            event_weight,
        );
        tx += f64::from(ew) + gap;
        text_renderer.draw_text_weighted(
            scene,
            time,
            tokens.typography.scale.xs,
            colors.text_dim,
            at(transform, (tx, ty + 1.0)),
            None,
            time_weight,
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
        let tray_h = 32.0_f64;
        let tray_slot = 24.0_f64;
        let tray_pad = 8.0_f64;
        let tray_gap = 4.0_f64;
        let icon_px = 17.0_f64;
        let pct_size = tokens.typography.scale.xs;

        let has_volume = self.status.volume.is_some();
        let has_link = self.status.link.is_some();
        let battery_pct = self.status.battery_percent();
        let pct_label = battery_pct.map(|p| format!("{p}%"));
        let pct_w = pct_label.as_ref().map_or(0.0, |s| {
            f64::from(
                text_renderer
                    .measure_weighted(s, pct_size, None, TextWeight::Semibold)
                    .0,
            )
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
            tray_w += 20.0;
        }

        self.tray_pill_rect = Rect::new(
            rx - tray_w,
            center_y - tray_h / 2.0,
            rx,
            center_y + tray_h / 2.0,
        );
        let tray = RoundedRect::from_rect(
            self.tray_pill_rect,
            ui::chrome::ChromeRole::Selector.radius(theme, tray_h),
        );
        let tray_body = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
        scene.fill(Fill::NonZero, transform, tray_body.fill, None, &tray);
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
            pct_size,
            pct_label,
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
        let label = if self.notification_badge > 99 {
            "99+".to_string()
        } else {
            self.notification_badge.to_string()
        };
        let label_size = 9.0;
        let (label_w, label_h) = text_renderer.measure(&label, label_size, None);
        let badge = Rect::new(
            sx + layout.tray_slot - 1.0,
            layout.center_y - 8.0,
            sx + layout.tray_slot + f64::from(label_w) + 9.0,
            layout.center_y + 8.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            layout.danger,
            None,
            &RoundedRect::from_rect(badge, 8.0),
        );
        text_renderer.draw_text(
            scene,
            &label,
            label_size,
            Color::WHITE,
            at(
                transform,
                (
                    badge.x0 + (badge.width() - f64::from(label_w)) / 2.0,
                    badge.y0 + (badge.height() - f64::from(label_h)) / 2.0,
                ),
            ),
            None,
        );
        sx + layout.tray_slot + 20.0 + layout.tray_gap
    }

    /// Draws the volume glyph if the status has a reading. Returns the
    /// x cursor position for the next slot (unchanged if not drawn).
    fn draw_tray_volume_icon(
        scene: &mut Scene,
        transform: Affine,
        status: &crate::shell::status::SystemStatus,
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
        status: &crate::shell::status::SystemStatus,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        let Some(link) = status.link.clone() else {
            return sx;
        };
        let r = layout.slot_rect(sx);
        match link.kind {
            os_actions::LinkKind::Wired => {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    r,
                    ui::icons::Icon::EthernetPort,
                    layout.dim,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            os_actions::LinkKind::Offline => {
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
            os_actions::LinkKind::Wifi => {
                // `icon_subpaths(Wifi)` yields [dot, outer, middle,
                // inner], so a weak signal lights fewer arcs — the
                // same glyph, read as a meter.
                let bars = crate::shell::status::link_bars(&link);
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
        status: &crate::shell::status::SystemStatus,
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
            let (_, lh) =
                text_renderer.measure_weighted(label, layout.pct_size, None, TextWeight::Semibold);
            text_renderer.draw_text_weighted(
                scene,
                label,
                layout.pct_size,
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
                TextWeight::Semibold,
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
        sx = self.draw_tray_notification_badge(scene, transform, text_renderer, layout, sx);
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
            .any(|u| u.kind == os_actions::MediaKind::Microphone);
        let has_camera = self
            .status
            .media
            .iter()
            .any(|u| u.kind == os_actions::MediaKind::Camera);
        let media_color = colors.semantic.success;
        let indicator_h = 22.0;
        if self.media_expanded {
            let suffix = if labels.len() > 1 {
                format!(" +{}", labels.len().saturating_sub(1))
            } else {
                String::new()
            };
            let label = format!("{}{suffix}", labels.first().copied().unwrap_or_default());
            let size = tokens.typography.scale.xs;
            let (label_w, label_h) =
                text_renderer.measure_weighted(&label, size, None, TextWeight::Semibold);
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
            text_renderer.draw_text_weighted(
                scene,
                &label,
                size,
                colors.text_primary,
                at(transform, (ix + 2.0, center_y - f64::from(label_h) / 2.0)),
                None,
                TextWeight::Semibold,
            );
        } else {
            self.media_indicator_rect = Rect::new(
                rx - indicator_h,
                center_y - indicator_h / 2.0,
                rx,
                center_y + indicator_h / 2.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                media_color,
                None,
                &vello::kurbo::Circle::new(self.media_indicator_rect.center(), 4.5),
            );
        }
        self.media_indicator_rect.x0 - f64::from(tokens.spacing.s3)
    }

    /// Up to 3 collaborator chips plus an overflow "+N" chip, growing
    /// leftward from `stack_right_x`. Returns how many circles were
    /// actually drawn (chips + the overflow chip, if any).
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
        // Up to 3 chips, overlapping by 5px, newest on top. Routed
        // through `ui::Avatar` so the roster stays pixel-identical to
        // app rosters.
        for (i, initial) in self.collaborator_initials.iter().enumerate() {
            let cx = stack_right_x - small_count_to_f64(i) * 15.0;
            let center = Point::new(cx - 10.0, center_y);
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
                .unwrap_or(colors.accent[5]);
            ui::widget::Avatar::draw_ringed_at(
                scene,
                transform,
                rect,
                &initial.to_string(),
                fill,
                // Must read against the bar it's drawn on — a literal
                // black is a hard outline on a light bar.
                colors.surfaces.overlay,
                1.5,
                theme,
                text_renderer,
            );
        }

        if self.collaborator_overflow > 0 {
            let i = self.collaborator_initials.len();
            let cx = stack_right_x - small_count_to_f64(i) * 15.0;
            let center = Point::new(cx - 10.0, center_y);
            let rect = Rect::new(
                center.x - 10.0,
                center.y - 10.0,
                center.x + 10.0,
                center.y + 10.0,
            );
            ui::widget::Avatar::draw_overflow_at(
                scene,
                transform,
                rect,
                self.collaborator_overflow,
                colors.surfaces.raised,
                colors.surfaces.overlay,
                1.5,
                colors.text_dim,
                text_renderer,
            );
        }

        self.collaborator_initials
            .len()
            .saturating_add(usize::from(self.collaborator_overflow > 0))
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
        let (viewers, controllers, _) = self.sharing_summary();
        if viewers == 0 {
            self.sharing_indicator_rect = Rect::ZERO;
            return right_x;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let color = if controllers > 0 {
            colors.semantic.warning
        } else {
            colors.accent[5]
        };
        let label = self
            .sharing_label()
            .expect("a non-empty sharing summary has glance text");
        let size = tokens.typography.scale.xs;
        let available = (right_x - self.center_rect.x1 - 48.0).clamp(28.0, 180.0);
        let label = text_renderer.elide_to_width_weighted(
            &label,
            size,
            ui::num::f32_of_f64(available),
            TextWeight::Semibold,
        );
        let (label_w, label_h) =
            text_renderer.measure_weighted(&label, size, None, TextWeight::Semibold);
        let width = f64::from(label_w) + 31.0;
        self.sharing_indicator_rect =
            Rect::new(right_x - width, center_y - 11.0, right_x, center_y + 11.0);
        let pill = RoundedRect::from_rect(
            self.sharing_indicator_rect,
            ui::chrome::ChromeRole::Selector.radius(theme, 22.0),
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
        text_renderer.draw_text_weighted(
            scene,
            &label,
            size,
            colors.text_primary,
            at(
                transform,
                (
                    self.sharing_indicator_rect.x0 + 23.0,
                    center_y - f64::from(label_h) / 2.0,
                ),
            ),
            None,
            TextWeight::Semibold,
        );
        self.sharing_indicator_rect.x0 - f64::from(tokens.spacing.s3)
    }

    /// Sets [`Self::avatar_stack_rect`] to the drawn chip stack's bounds.
    /// An untouched solo workspace deliberately has no collaborator control
    /// in the resting bar; inviting the first person starts in the contextual
    /// workspace sheet opened from the workspace name. `pos` is `(center_y,
    /// stack_right_x)`.
    fn draw_avatar_stack_bounds(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _theme: &Theme,
        _text_renderer: &mut TextRenderer,
        pos: (f64, f64),
        slot_count: usize,
    ) {
        let (center_y, stack_right_x) = pos;
        if slot_count > 0 {
            // Stack grows leftward from `stack_right_x`; each circle is
            // 20 px wide with 5 px overlap, so step = 15 px.
            let step = 15.0_f64;
            let leftmost_cx = stack_right_x - (small_count_to_f64(slot_count) - 1.0) * step;
            let y_mid = center_y;
            self.avatar_stack_rect = Rect::new(
                leftmost_cx - 20.0,
                y_mid - 16.0,
                stack_right_x,
                y_mid + 16.0,
            );
            return;
        }
        self.avatar_stack_rect = Rect::ZERO;
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
            let mut state = chrome.borrow_mut();
            let focus = state.activity_glance.focus.clone();
            state.open_room_activity(focus);
            self.activity_started = None;
            *handled = true;
            return;
        }
        // Dot clicks jump directly — local widget state, no chrome needed.
        if let Some(p) = self.last_cursor {
            for (i, rect) in self.center_dot_rects.iter().enumerate() {
                if rect.contains(p) {
                    self.center_widget = CenterWidget::from_index(i);
                    *handled = true;
                    return;
                }
            }
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // MouseInput has no position; rely on the last CursorMoved.
        let Some(p) = self.last_cursor else {
            return;
        };
        let mut state = chrome.borrow_mut();
        if self.overview_btn_rect.contains(p) {
            state.overlays.overview = !state.overlays.overview;
            state.bump();
            *handled = true;
            return;
        }
        if self.up_btn_rect.contains(p) {
            if state.switch_workspace(-1) {
                *handled = true;
            }
            return;
        }
        if self.down_btn_rect.contains(p) {
            if state.switch_workspace(1) {
                *handled = true;
            }
            return;
        }
        if self.ws_pill_rect.contains(p) {
            state.toggle_workspace_settings();
            *handled = true;
            return;
        }
        if self.tray_pill_rect.contains(p) {
            state.toggle_quick_settings();
            *handled = true;
            return;
        }
        if self.media_indicator_rect.contains(p) {
            state.open_quick_settings();
            *handled = true;
            return;
        }
        if self.avatar_stack_rect.contains(p) || self.sharing_indicator_rect.contains(p) {
            state.overlays.collaborators = !state.overlays.collaborators;
            if state.overlays.collaborators {
                state.overlays.room_activity = false;
            }
            state.bump();
            *handled = true;
        }
    }

    /// Pulled at the top of [`Widget::draw`], not pushed by `update`, so a
    /// just-dropped battery percent is drawn and counted as a change in the
    /// same pass. Also tracks the media-use signature so a newly-started
    /// capture announces itself as a named pill before settling into a dot.
    fn refresh_status_and_media_state(&mut self) {
        self.status = self.live_status();
        if let Some(chrome) = self.chrome.as_ref()
            && let Some(path) = std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .map(|p| p.join("tilo/clocks.json"))
            && let Ok(meta) = std::fs::metadata(&path)
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode().trailing_zeros() >= 6
            && let Ok(bytes) = std::fs::read(path)
            && bytes.len() <= 4096
            && let Ok(activity) =
                serde_json::from_slice::<crate::shell::chrome_state::ClockActivity>(&bytes)
            && activity.version == 1
            && activity.producer_pid > 0
            && std::path::Path::new(&format!("/proc/{}", activity.producer_pid)).exists()
        {
            let mut state = chrome.borrow_mut();
            if let Some(notification_id) = activity.notification_id.as_ref()
                && self.clock_notified_id.as_ref() != Some(notification_id)
            {
                state.push_notification(
                    if activity.notification_title.is_empty() {
                        "Clocks"
                    } else {
                        &activity.notification_title
                    },
                    if activity.notification_body.is_empty() {
                        "Time is up"
                    } else {
                        &activity.notification_body
                    },
                );
                self.clock_notified_id = Some(notification_id.clone());
            }
            if activity.active {
                state.overlays.clock = activity.overlay;
                state.clock_activity = Some(activity);
            } else {
                state.overlays.clock = false;
                state.clock_activity = None;
                self.clock_drag_offset = None;
                self.clock_touch_id = None;
                self.clock_control_focus = None;
                self.cache_fp = CacheFingerprint::default();
            }
        }
        self.notification_badge = self
            .chrome
            .as_ref()
            .map_or(0, |chrome| chrome.borrow().notification_badge_total());
        if let Some(chrome) = self.chrome.as_ref() {
            let glance = chrome.borrow().activity_glance.clone();
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
                        os_actions::MediaKind::Microphone => 0,
                        os_actions::MediaKind::Camera => 1,
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
            .is_some_and(|p| self.media_indicator_rect.contains(p));
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
            self.draw_center_band(scene, transform, theme, center_y);
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
                Self::draw_calendar_widget(
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
        let size = theme.tokens.typography.scale.sm;
        // Keep activity in one line. `measure(..., Some(width))` wraps at
        // word boundaries, which made a large-text notification grow to two
        // lines inside the fixed 34px selector and clip into the bar edge.
        // The dots reserve the first 80px of the band; the trailing inset
        // reserves the right breathing room. Elision preserves that geometry
        // at every text scale and viewport width.
        let max_width = self.activity_content_width();
        let label = self.activity_display_label(text_renderer, size, max_width);
        let (w, h) = text_renderer.measure_weighted(&label, size, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            &label,
            size,
            theme.tokens.colors.text_primary,
            at(
                transform,
                (center_x - f64::from(w) / 2.0, center_y - f64::from(h) / 2.0),
            ),
            None,
            TextWeight::Medium,
        );
    }

    fn activity_content_width(&self) -> f32 {
        // The dots reserve the first 80px of the band; the trailing inset
        // reserves the right breathing room.
        ui::num::f32_of_f64((self.center_rect.width() - 92.0).max(48.0))
    }

    fn activity_display_label(
        &self,
        text_renderer: &mut TextRenderer,
        size: f32,
        max_width: f32,
    ) -> String {
        let Some(headline) = self.activity_headline.as_deref() else {
            return String::new();
        };
        let full = if self.activity_unread == 0 {
            headline.to_string()
        } else {
            format!("{}  ·  {} new", headline, self.activity_unread)
        };
        let (full_width, _) = text_renderer.measure_weighted(&full, size, None, TextWeight::Medium);
        if full_width <= max_width {
            return full;
        }

        // The primary activity says what happened; the unread count already
        // has a numeric badge, and clicking this pill opens the focused
        // Room Activity detail. Prefer the entire primary sentence plus an
        // explicit detail chevron over eliding its last words to preserve
        // the trailing count in a compact top bar.
        let detail = format!("{headline}  ›");
        let (detail_width, _) =
            text_renderer.measure_weighted(&detail, size, None, TextWeight::Medium);
        if detail_width <= max_width {
            return detail;
        }
        // Aggregate activity sometimes lists its examples after a colon
        // (for example, the workspace names just recorded). Keep the
        // complete event/count clause and make the detail route explicit
        // instead of cutting it part-way through an example list.
        if let Some((summary, _examples)) = headline.split_once(':') {
            let compact = format!("{summary}  ›");
            let (compact_width, _) =
                text_renderer.measure_weighted(&compact, size, None, TextWeight::Medium);
            if compact_width <= max_width {
                return compact;
            }
        }
        text_renderer.elide_to_width_weighted(headline, size, max_width, TextWeight::Medium)
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
            theme.tokens.colors.accent[5],
            None,
            &vello::kurbo::Circle::new(center, radius),
        );
        let size = (theme.tokens.typography.scale.sm - 2.0).max(8.0);
        let (w, h) = text_renderer.measure(&label, size, None);
        text_renderer.draw_text_weighted(
            scene,
            &label,
            size,
            theme.tokens.colors.text_primary,
            at(
                transform,
                (center.x - f64::from(w) / 2.0, center.y - f64::from(h) / 2.0),
            ),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_focus_indicator(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.focused {
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
        match target {
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
            Self::CONTROL_OVERVIEW
            | Self::CONTROL_WORKSPACE_UP
            | Self::CONTROL_WORKSPACE_DOWN
            | Self::CONTROL_WORKSPACE_SETTINGS
            | Self::CONTROL_QUICK_SETTINGS
            | Self::CONTROL_COLLABORATORS
            | Self::CONTROL_MEDIA => {
                let Some(chrome) = self.chrome.clone() else {
                    return false;
                };
                let mut state = chrome.borrow_mut();
                match target {
                    Self::CONTROL_OVERVIEW => state.overlays.overview = !state.overlays.overview,
                    Self::CONTROL_WORKSPACE_UP => {
                        if !state.switch_workspace(-1) {
                            return false;
                        }
                    }
                    Self::CONTROL_WORKSPACE_DOWN => {
                        if !state.switch_workspace(1) {
                            return false;
                        }
                    }
                    Self::CONTROL_WORKSPACE_SETTINGS => state.toggle_workspace_settings(),
                    Self::CONTROL_QUICK_SETTINGS | Self::CONTROL_MEDIA => {
                        state.toggle_quick_settings();
                        return true;
                    }
                    Self::CONTROL_COLLABORATORS => {
                        state.overlays.collaborators = !state.overlays.collaborators;
                        if state.overlays.collaborators {
                            state.overlays.room_activity = false;
                        }
                    }
                    _ => return false,
                }
                state.bump();
                true
            }
            _ => false,
        }
    }

    fn handle_keyboard(&mut self, event: &WindowEvent, handled: &mut bool) {
        if !self.focused {
            return;
        }
        let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key,
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return;
        };
        let controls = self.control_targets();
        if controls.is_empty() {
            return;
        }
        let current = self
            .focused_control
            .as_deref()
            .and_then(|target| controls.iter().position(|candidate| *candidate == target))
            .unwrap_or(0);
        let control_at = |index: usize| controls.get(index).copied();
        match logical_key {
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowLeft) => {
                let previous = current
                    .checked_sub(1)
                    .unwrap_or_else(|| controls.len().saturating_sub(1));
                if let Some(target) = control_at(previous) {
                    self.focused_control = Some(target.to_string());
                    *handled = true;
                }
            }
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowRight) => {
                let next = current
                    .checked_add(1)
                    .filter(|&index| index < controls.len())
                    .unwrap_or(0);
                if let Some(target) = control_at(next) {
                    self.focused_control = Some(target.to_string());
                    *handled = true;
                }
            }
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Home) => {
                if let Some(target) = control_at(0) {
                    self.focused_control = Some(target.to_string());
                    *handled = true;
                }
            }
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::End) => {
                if let Some(target) = controls.last().copied() {
                    self.focused_control = Some(target.to_string());
                    *handled = true;
                }
            }
            winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter | winit::keyboard::NamedKey::Space,
            ) => {
                if let Some(target) = control_at(current) {
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
        activity: &crate::shell::chrome_state::ClockActivity,
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
        text_renderer.draw_text_weighted(
            scene,
            &activity.label,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            at(transform, (panel.x0 + 18.0, panel.y0 + 21.0)),
            None,
            TextWeight::Medium,
        );
        let elapsed = format!(
            "{:02}:{:02}",
            activity.remaining_secs / 60,
            activity.remaining_secs % 60
        );
        text_renderer.draw_text_weighted(
            scene,
            &elapsed,
            theme.tokens.typography.scale.lg,
            theme.tokens.colors.text_primary,
            at(transform, (panel.x0 + 18.0, panel.y0 + 43.0)),
            None,
            TextWeight::Bold,
        );
        [
            self.clock_pause_rect,
            self.clock_add_rect,
            self.clock_close_rect,
        ] = clock_overlay_controls(panel);
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(
                self.clock_pause_rect,
                if activity.paused { "Resume" } else { "Pause" },
            )
            .primary()
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
            &ui::widget::ButtonFace::new(
                self.clock_add_rect,
                if activity.stopwatch { "Lap" } else { "+1 min" },
            )
            .focused(self.clock_control_focus == Some(1)),
        );
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(self.clock_close_rect, "")
                .ghost()
                .icon(ui::icons::Icon::X)
                .accessible_name(if activity.stopwatch {
                    "Close floating stopwatch"
                } else {
                    "Close floating timer"
                })
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
                    chrome.borrow_mut().overlays.clock = false;
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
        let state = chrome.borrow();
        if !state.overlays.clock {
            return None;
        }
        let activity = state.clock_activity.as_ref()?;
        let stopwatch = activity.stopwatch;
        let paused = activity.paused;
        let minutes = activity.remaining_secs / 60;
        let seconds = activity.remaining_secs % 60;
        let reading = format!("{minutes:02}:{seconds:02}");
        Some(
            AccessNode::new(Role::Group)
                .with_name(if stopwatch {
                    "Floating stopwatch"
                } else {
                    "Floating timer"
                })
                .with_description(
                    "Drag to reposition. With a keyboard, hold Alt and use the arrow keys; hold Shift for larger steps.",
                )
                .with_bounds(self.clock_overlay_rect)
                .with_children(vec![
                    AccessNode::new(Role::StaticText)
                        .with_name(if activity.label.is_empty() {
                            if stopwatch { "Stopwatch" } else { "Timer" }
                        } else {
                            &activity.label
                        })
                        .with_value(&reading)
                        .with_description(if paused { "Paused" } else { "Running" }),
                    AccessNode::new(Role::Button)
                        .with_name(if paused { "Resume" } else { "Pause" })
                        .with_action_target("clock.overlay.toggle")
                        .with_bounds(self.clock_pause_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(0)),
                        ),
                    AccessNode::new(Role::Button)
                        .with_name(if stopwatch {
                            "Record lap"
                        } else {
                            "Add one minute"
                        })
                        .with_action_target("clock.overlay.add-minute")
                        .with_bounds(self.clock_add_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(1)),
                        ),
                    AccessNode::new(Role::Button)
                        .with_name(if stopwatch {
                            "Close floating stopwatch"
                        } else {
                            "Close floating timer"
                        })
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
}

impl Widget for TopBarChrome {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        if let Some(chrome) = self.chrome.clone() {
            let rev = chrome.borrow().revision;
            if rev != self.synced_revision {
                // Clone the tree to avoid re-borrowing chrome while
                // `sync_from_tree` borrows &self mut.
                let tree_snap = chrome.borrow().tree().clone();
                self.sync_from_tree(&tree_snap);
                // The status poller bumps the same revision.
                self.status = chrome.borrow().status.clone();
                self.synced_revision = rev;
                return true;
            }
        }
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // While any group holds fullscreen, collapse to zero height.
        let fullscreen = self
            .chrome
            .as_ref()
            .is_some_and(|c| c.borrow().is_fullscreen());
        let style = Style {
            display: if fullscreen {
                Display::None
            } else {
                Display::Flex
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: if fullscreen {
                    Dimension::length(0.0)
                } else {
                    Dimension::length(TOPBAR_HEIGHT_PX)
                },
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let node = match taffy.new_leaf(style) {
            Ok(node) => node,
            Err(err) => {
                // `TaffyTree::new_leaf` only ever returns `Ok` for a
                // fresh, unattached leaf (see upstream `new_leaf`); this
                // arm exists so a future upstream change degrades the
                // bar's layout instead of aborting the whole session
                // (this crate builds with `panic = "abort"`).
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
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
        // Pulled here, not pushed by update, so a just-dropped battery
        // percent is drawn and counted as a change in the same pass.
        self.refresh_status_and_media_state();
        let fp = self.current_fingerprint();
        if fp == self.cache_fp && fp.rect_px.2 > 0 {
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

        self.draw_backdrop(scene, transform, theme);

        let after_chevrons = self.draw_workspace_nav(scene, transform, theme, center_y);
        self.draw_workspace_pill(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            after_chevrons,
        );

        self.draw_center_widget(scene, transform, theme, text_renderer, center_y);

        let layout = self.draw_tray_pill_body(scene, transform, theme, text_renderer, center_y);
        self.draw_tray_icons(scene, transform, theme, text_renderer, &layout);
        // s3/s4 match the pad_x/gap used when this pill was sized and
        // placed in `draw_tray_pill_body` (s4 = 16, ≥ 8 px after avatar
        // radius).
        let stack_right_x = self.rect.x1
            - f64::from(theme.tokens.spacing.s3)
            - layout.tray_w
            - f64::from(theme.tokens.spacing.s4);
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
        let slot_count = self.draw_avatar_chips(
            scene,
            transform,
            theme,
            text_renderer,
            center_y,
            stack_right_x,
        );
        self.draw_avatar_stack_bounds(
            scene,
            transform,
            theme,
            text_renderer,
            (center_y, stack_right_x),
            slot_count,
        );
        self.draw_activity_badge(scene, transform, theme, text_renderer);
        self.draw_focus_indicator(scene, transform, theme);
        let clock_activity = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            state.overlays.clock.then(|| state.clock_activity.clone())?
        });
        if let Some(activity) = clock_activity {
            self.draw_clock_overlay(scene, transform, text_renderer, theme, &activity);
        }

        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_fp = fp;
        text_renderer.set_text_scale(renderer_scale);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
            && self
                .chrome
                .as_ref()
                .is_some_and(|c| c.borrow().overlays.clock)
        {
            *handled = self.handle_clock_keyboard(&key.logical_key, *modifiers);
            return;
        }
        if let WindowEvent::Touch(touch) = event
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().overlays.clock)
        {
            *handled = self.handle_clock_touch(touch, scale_factor);
            return;
        }
        // Cached so `MouseInput` (no position) can hit-test against
        // the last draw's geometry.
        if let WindowEvent::CursorMoved { position, .. } = event {
            let point = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.last_cursor = Some(point);
            if self.clock_touch_id.is_none() && self.move_clock_drag_to(point) {
                *handled = true;
            }
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let Some(p) = self.last_cursor else { return };
            if !self.center_rect.contains(p) {
                return;
            }
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
            };
            self.center_scroll_accum += dy;
            // Bounded rather than `while` on a float comparison: a
            // single scroll event can only carry so much delta, and a
            // fixed cap means a pathological float value can never spin
            // this forever.
            for _ in 0..16 {
                if self.center_scroll_accum < 1.0 {
                    break;
                }
                self.center_widget = self.center_widget.next();
                self.center_scroll_accum -= 1.0;
            }
            for _ in 0..16 {
                if self.center_scroll_accum > -1.0 {
                    break;
                }
                self.center_widget = self.center_widget.prev();
                self.center_scroll_accum += 1.0;
            }
            *handled = true;
            return;
        }
        self.handle_keyboard(event, handled);
        if *handled {
            return;
        }
        if let WindowEvent::MouseInput {
            state: winit::event::ElementState::Released,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            if self.clock_touch_id.is_none() && self.clock_drag_offset.take().is_some() {
                *handled = true;
            }
            return;
        }
        let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        if self.clock_touch_id.is_some() {
            return;
        }
        if let Some(point) = self.last_cursor {
            if self.activate_clock_control_at(point) {
                *handled = true;
                return;
            }
            if self.begin_clock_drag_at(point) {
                *handled = true;
                return;
            }
        }
        self.handle_left_click(handled);
        if *handled {
            *focus_target = Some(node_id);
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
        vec![
            ("topbar_overview_button".to_string(), self.overview_btn_rect),
            ("topbar_workspace_up".to_string(), self.up_btn_rect),
            ("topbar_workspace_down".to_string(), self.down_btn_rect),
            ("clock_overlay_pause".to_string(), self.clock_pause_rect),
            ("clock_overlay_add_minute".to_string(), self.clock_add_rect),
            ("clock_overlay_close".to_string(), self.clock_close_rect),
            // Carries the same rect only while the switch would
            // actually move (`walk_widget` drops zero-area entries).
            (
                "topbar_workspace_up_enabled".to_string(),
                if self.can_switch_up() {
                    self.up_btn_rect
                } else {
                    Rect::ZERO
                },
            ),
            (
                "topbar_workspace_down_enabled".to_string(),
                if self.can_switch_down() {
                    self.down_btn_rect
                } else {
                    Rect::ZERO
                },
            ),
            ("topbar_workspace_pill".to_string(), self.ws_pill_rect),
            ("topbar_tray_pill".to_string(), self.tray_pill_rect),
            (
                "topbar_media_indicator".to_string(),
                self.media_indicator_rect,
            ),
            ("topbar_avatar_stack".to_string(), self.avatar_stack_rect),
            (
                "topbar_sharing_indicator".to_string(),
                self.sharing_indicator_rect,
            ),
            ("topbar_center_clock".to_string(), self.center_dot_rects[0]),
            (
                "topbar_center_system_monitor".to_string(),
                self.center_dot_rects[1],
            ),
            (
                "topbar_center_calendar".to_string(),
                self.center_dot_rects[2],
            ),
        ]
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
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
                    node = node.with_description(format!(
                        "Workspace {} of {}",
                        self.workspace_index.saturating_add(1),
                        self.workspace_total
                    ));
                }
                node
            })
            .collect();
        if let Some(clock) = self.clock_accessibility_node() {
            children.push(clock);
        }
        Some(
            AccessNode::new(Role::Toolbar)
                .with_semantic_key("topbar")
                .with_name("Desktop navigation")
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
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
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
                    chrome.borrow_mut().overlays.clock = false;
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
            .is_some_and(|chrome| chrome.borrow().overlays.clock)
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
            .any(|target| self.control_rect(target).contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
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
                            os_actions::MediaKind::Microphone => 0,
                            os_actions::MediaKind::Camera => 1,
                        },
                        u.label.clone(),
                    )
                })
                .collect::<Vec<_>>()
                != self.cache_fp.media_key
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
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod calendar_strip_tests {
    use super::*;
    use chrono::TimeZone;

    /// The strip reports the day it is actually run on.
    #[test]
    fn the_strip_reads_the_clock() {
        let day = Local.with_ymd_and_hms(2026, 3, 9, 11, 0, 0).unwrap();
        let (weekday, date, _) = calendar_strip_parts(day);
        assert_eq!(weekday, "Mon");
        assert_eq!(date, "9 March");

        let other = Local.with_ymd_and_hms(2025, 12, 25, 11, 0, 0).unwrap();
        let (weekday, date, _) = calendar_strip_parts(other);
        assert_eq!(weekday, "Thu");
        assert_eq!(date, "25 December");
    }

    #[test]
    fn the_event_slot_names_no_meeting() {
        let (_, _, note) = calendar_strip_parts(Local::now());
        assert_eq!(note, "No calendar connected");
        assert!(
            !note.contains("Design Sync"),
            "the slot must not carry an invented appointment"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{
        CapOverride, Strip, Subject, Tab, TileContent, Verb, WindowGroup, WindowWidth, Workspace,
        WorkspaceId, WorkspaceTree,
    };

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
                .all(|run| run.font_size <= theme.tokens.typography.scale.lg)
        );
        assert_eq!(renderer.text_scale(), 2.0);
        renderer.set_text_scale(1.0);
        let (_, height) = renderer.measure_weighted(
            "Home  1/1",
            theme.tokens.typography.scale.sm,
            None,
            TextWeight::Semibold,
        );
        assert!(f64::from(height) <= bar.ws_pill_rect.height());
        assert!(bar.ws_pill_rect.x1 <= bar.rect.center().x - bar.center_band_width(&theme) / 2.0);
    }

    fn sample_chrome() -> TopBarChrome {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        TopBarChrome::from_tree(&tree)
    }

    fn floating_clock() -> TopBarChrome {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
        {
            let mut state = chrome.borrow_mut();
            state.overlays.clock = true;
            state.clock_activity = Some(crate::shell::chrome_state::ClockActivity {
                active: true,
                label: "Tea".into(),
                remaining_secs: 125,
                overlay: true,
                producer_pid: std::process::id(),
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
                    size: Size {
                        width: Dimension::length(width),
                        height: Dimension::length(TOPBAR_HEIGHT_PX),
                    },
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

    #[test]
    fn persistent_topbar_controls_have_coherent_targets() {
        let mut bar = sample_chrome();
        lay_out_and_draw(&mut bar, 1280.0);
        assert!(bar.overview_btn_rect.width() >= 32.0);
        assert!(bar.overview_btn_rect.height() >= 32.0);
        assert!(bar.up_btn_rect.height() >= 32.0);
        assert!(bar.up_btn_rect.width() >= 32.0);
        assert!(bar.down_btn_rect.height() >= 32.0);
        assert!(bar.down_btn_rect.width() >= 32.0);
        assert!(bar.ws_pill_rect.height() >= 32.0);
        assert!(bar.tray_pill_rect.height() >= 32.0);
        assert!(bar.center_rect.height() >= 32.0);
    }

    #[test]
    fn large_text_activity_stays_single_line_inside_center_content_lane() {
        let mut bar = sample_chrome();
        bar.center_rect = Rect::new(460.0, 7.0, 820.0, 41.0);
        bar.activity_headline = Some("Recorded 3 workspaces: Home, Research and Design".into());
        bar.activity_unread = 3;
        let mut renderer = TextRenderer::new();
        let theme = Theme::default().with_text_scale(1.5);
        let size = theme.tokens.typography.scale.sm;
        let max_width = bar.activity_content_width();
        let label = bar.activity_display_label(&mut renderer, size, max_width);
        let (_, height) = renderer.measure_weighted(&label, size, None, TextWeight::Medium);
        let (width, _) = renderer.measure_weighted(&label, size, None, TextWeight::Medium);
        assert!(
            !label.contains('\n'),
            "activity headline must never wrap in the fixed topbar pill: {label:?}"
        );
        assert!(
            width <= max_width + 0.5,
            "activity headline width {width} exceeds reserved center lane {max_width}"
        );
        assert!(
            height <= TOPBAR_HEIGHT_PX - 4.0,
            "activity headline height {height} must fit in the 48px topbar"
        );
    }

    #[test]
    fn compact_large_text_preserves_the_full_activity_sentence_and_detail_route() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        // The 960px chrome reserves a 360px centre selector; after its dots
        // and breathing room, this is the usable activity lane.
        bar.center_rect = Rect::new(300.0, 7.0, 660.0, 41.0);
        bar.activity_headline = Some("ada · 2 permission changes".into());
        bar.activity_unread = 4;
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/ui-core/assets/fonts"),
            "en-US",
        );
        let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
        let theme = Theme::default().with_text_scale(1.3);
        let size = theme.tokens.typography.scale.sm;
        let max_width = bar.activity_content_width();
        let label = bar.activity_display_label(&mut renderer, size, max_width);
        let (width, _) = renderer.measure_weighted(&label, size, None, TextWeight::Medium);
        assert_eq!(label, "ada · 2 permission changes  ›");
        assert!(width <= max_width + 0.5);

        bar.activity_started = Some(Instant::now());
        bar.last_cursor = Some(bar.center_rect.center());
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        assert!(handled);
        assert!(
            crate::shell::tab_binding::active_workspace_activity_tab(&chrome.borrow().tree())
                .is_some(),
            "the explicit chevron label points to the activity detail route"
        );
        assert!(!chrome.borrow().overlays.room_activity);
    }

    fn press_named_key(bar: &mut TopBarChrome, key: winit::keyboard::NamedKey) -> bool {
        let mut key_event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        key_event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        key_event.logical_key = winit::keyboard::Key::Named(key);
        key_event.text = None;
        key_event.location = winit::keyboard::KeyLocation::Standard;
        key_event.state = winit::event::ElementState::Pressed;
        key_event.repeat = false;
        let event = WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: key_event,
            is_synthetic: false,
        };
        let mut handled = false;
        bar.handle_keyboard(&event, &mut handled);
        handled
    }

    fn two_workspace_tree() -> WorkspaceTree {
        let first = Workspace::new(WorkspaceId::new(), "A");
        let second = Workspace::new(WorkspaceId::new(), "B");
        let active = first.id;
        WorkspaceTree {
            workspaces: vec![first, second],
            active,
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        }
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
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
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
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
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
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(bar.center_widget, CenterWidget::Calendar);
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
        assert_eq!(short.center_rect.width(), 220.0);

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
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0]
            .collaborators
            .push(crate::shell::state::Collaborator::new("alice", "Alice"));
        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default().with_text_scale(1.5);
        lay_out_and_draw_with_theme(&mut bar, 800.0, &theme);

        assert!(bar.center_rect.width() < 360.0);
        assert!(bar.ws_pill_rect.x1 + 12.0 <= bar.center_rect.x0);
        assert!(bar.center_rect.x1 + 12.0 <= bar.avatar_stack_rect.x0);
        assert!(bar.avatar_stack_rect.x1 + 8.0 <= bar.tray_pill_rect.x0);
    }

    #[test]
    fn active_center_dot_radius_is_strictly_larger_than_inactive() {
        let chrome = sample_chrome();
        assert!(
            chrome.active_dot_radius() > chrome.inactive_dot_radius(),
            "active dot radius {} must be strictly larger than inactive {}",
            chrome.active_dot_radius(),
            chrome.inactive_dot_radius(),
        );
        // Guards the specific 1.6× ratio against a silent regression.
        assert!(
            (chrome.active_dot_radius() - TopBarChrome::DOT_BASE_SIZE * 1.6).abs() < f64::EPSILON,
            "active dot radius should be 1.6× the base size",
        );
    }

    #[test]
    fn center_dots_restore_vertical_non_overlapping_layout() {
        let bar = Rect::new(0.0, 0.0, 1600.0, f64::from(TOPBAR_HEIGHT_PX));
        let band = Rect::new(620.0, 3.0, 980.0, 37.0);
        for active in 0..3 {
            let dots = TopBarChrome::center_dot_layout(band, bar, active);
            assert_eq!(dots[0].center.x, dots[1].center.x);
            assert_eq!(dots[1].center.x, dots[2].center.x);
            for pair in dots.windows(2) {
                assert!(
                    pair[0].center.y + pair[0].radius < pair[1].center.y - pair[1].radius,
                    "vertical dot ink must not overlap"
                );
                assert!(pair[0].slot.y1 <= pair[1].slot.y0);
            }
            for dot in dots {
                assert!(dot.slot.contains(dot.center));
                assert!(dot.center.y - dot.radius >= band.y0);
                assert!(dot.center.y + dot.radius <= band.y1);
            }
        }
    }

    #[test]
    fn new_activity_temporarily_overrides_but_does_not_replace_the_chosen_widget() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        bar.center_widget = CenterWidget::Calendar;

        chrome.borrow_mut().publish_activity_glance(
            Some("Ada · Prepared launch brief".to_string()),
            3,
            42,
            Some(crate::shell::chrome_state::ActivityFocus {
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
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
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

    #[test]
    fn live_share_is_persistent_visible_named_and_accessible() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0]
            .collaborators
            .push(crate::shell::state::Collaborator::new("ada", "Ada").with_session(7));
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
        chrome.borrow_mut().set_active_exposures(vec![
            crate::shell::chrome_state::ActiveExposure {
                peer: 7,
                surface: 42,
                can_control: true,
            },
        ]);
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
                    name.contains("visible to Ada (person): can control surface 42")
                })
        }));
    }

    #[test]
    fn disclosure_attributes_each_surface_to_its_actual_viewer() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0].collaborators.extend([
            crate::shell::state::Collaborator::new("ada", "Ada").with_session(7),
            crate::shell::state::Collaborator::new("lin", "Lin").with_session(9),
        ]);
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
        chrome.borrow_mut().set_active_exposures(vec![
            crate::shell::chrome_state::ActiveExposure {
                peer: 7,
                surface: 42,
                can_control: false,
            },
            crate::shell::chrome_state::ActiveExposure {
                peer: 9,
                surface: 43,
                can_control: true,
            },
            crate::shell::chrome_state::ActiveExposure {
                peer: 7,
                surface: 44,
                can_control: true,
            },
        ]);
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        let disclosure = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
        assert!(
            disclosure.contains("Ada (person): can control surface 44; view only surface 42"),
            "{disclosure}"
        );
        assert!(
            disclosure.contains("Lin (person): can control surface 43"),
            "{disclosure}"
        );
        assert!(!disclosure.contains("Ada (person): can control surface 43"));
        assert!(!disclosure.contains("Lin (person): can control surface 42"));
    }

    #[test]
    fn agent_viewer_is_never_described_as_a_person() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        let workspace = &mut tree.workspaces[0];
        workspace.owner = Some("owner".to_string());
        let room = workspace.id.raw().to_string();
        workspace
            .grant_as(
                &"owner".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
            )
            .expect("owner admits agent");
        workspace.collaborators.push(
            crate::shell::state::Collaborator::new("scribe", "Release Notes Agent").with_session(9),
        );
        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
        chrome.borrow_mut().set_active_exposures(vec![
            crate::shell::chrome_state::ActiveExposure {
                peer: 9,
                surface: 42,
                can_control: false,
            },
        ]);
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);

        let name = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
        assert!(
            name.contains("Release Notes Agent (agent): view only surface 42"),
            "{name}"
        );
        assert!(!name.contains("Release Notes Agent (person"), "{name}");
        assert_eq!(
            bar.sharing_label().as_deref(),
            Some("AGENT VIEWING · Release Notes Agent"),
            "critical privacy state must precede the elidable display name"
        );
    }

    #[test]
    fn disclosure_keeps_identity_and_target_when_user_switches_workspaces() {
        let mut tree = two_workspace_tree();
        let shared = &mut tree.workspaces[1];
        shared.name = "Private".into();
        shared.owner = Some("owner".into());
        let room = shared.id.raw().to_string();
        shared
            .grant_as(
                &"owner".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
            )
            .unwrap();
        shared
            .collaborators
            .push(crate::shell::state::Collaborator::new("scribe", "scribe").with_session(9));
        let mut strip = Strip::new(crate::shell::state::StripId::new(), "Main");
        let tab_id = crate::shell::state::next_id();
        strip.window_groups.push(WindowGroup {
            id: crate::shell::state::next_id(),
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
        });
        shared.strips.push(strip);

        let chrome = crate::shell::chrome_state::ChromeState::new(tree.clone());
        chrome.borrow_mut().set_active_exposures(vec![
            crate::shell::chrome_state::ActiveExposure {
                peer: 9,
                surface: 42,
                can_control: true,
            },
        ]);
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        let name = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
        assert!(
            name.contains("scribe (agent): can control Private › Draft"),
            "{name}"
        );
        assert!(!name.contains("unidentified session"), "{name}");
    }

    /// Asserts the colours differ AND move with the theme, which a
    /// hardcoded hex could never catch.
    #[test]
    fn system_monitor_metric_labels_are_distinct_and_theme_driven() {
        let chrome = sample_chrome();
        let kinds = [MetricKind::Cpu, MetricKind::Ram, MetricKind::Tmp];

        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high_contrast", Theme::high_contrast()),
        ] {
            let c: Vec<Color> = kinds
                .iter()
                .map(|k| chrome.metric_label_color(&theme, *k))
                .collect();
            for i in 0..c.len() {
                for j in (i + 1)..c.len() {
                    assert_ne!(
                        c[i].to_rgba8().to_u32(),
                        c[j].to_rgba8().to_u32(),
                        "{name}: {:?} and {:?} render in the same colour, collapsing the cue",
                        kinds[i],
                        kinds[j]
                    );
                }
            }
            // The accent means live state, not "this is a CPU reading".
            let accent = theme.tokens.colors.accent[5];
            for (k, col) in kinds.iter().zip(&c) {
                assert_ne!(
                    col.to_rgba8().to_u32(),
                    accent.to_rgba8().to_u32(),
                    "{name}: {k:?} label reuses the accent"
                );
            }
            let sem = &theme.tokens.colors.semantic;
            assert_eq!(
                c[0].to_rgba8().to_u32(),
                sem.success.to_rgba8().to_u32(),
                "{name}: CPU"
            );
            assert_eq!(
                c[1].to_rgba8().to_u32(),
                sem.info.to_rgba8().to_u32(),
                "{name}: RAM"
            );
            assert_eq!(
                c[2].to_rgba8().to_u32(),
                sem.warning.to_rgba8().to_u32(),
                "{name}: TMP"
            );
        }

        // A hard-coded palette passes every assertion above and fails this one.
        let dark = chrome.metric_label_color(&Theme::dark(), MetricKind::Cpu);
        let hc = chrome.metric_label_color(&Theme::high_contrast(), MetricKind::Cpu);
        assert_ne!(
            dark.to_rgba8().to_u32(),
            hc.to_rgba8().to_u32(),
            "the CPU label paints the same pixel on dark and high-contrast"
        );
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
                "{name}: sparkline hue drifted off the CPU label"
            );
            assert!(
                fill.components[3] < cpu.components[3],
                "{name}: the sparkline must sit behind the numbers, not compete with them"
            );
            assert_ne!(
                fill.to_rgba8().to_u32(),
                theme.tokens.colors.accent[5].to_rgba8().to_u32(),
                "{name}: sparkline reuses the accent"
            );
        }
    }

    #[test]
    fn avatar_stack_has_breathing_room_to_tray_pill() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.workspaces[0]
            .collaborators
            .push(crate::shell::state::Collaborator::new("alice", "Alice"));

        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(40.0),
                    },
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
    fn empty_roster_leaves_the_resting_bar_quiet() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let chrome = crate::shell::chrome_state::ChromeState::new(tree);
        bar = bar.with_chrome(chrome.clone());

        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(40.0),
                    },
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
            bar.avatar_stack_rect.is_zero_area(),
            "an untouched solo workspace must not advertise collaboration in the resting bar",
        );
        assert!(!chrome.borrow().overlays.collaborators);
    }

    /// The topbar's shadow is its only linear-gradient fill, so pinning
    /// `LINEAR_GRADIENT` guards against a refactor silently dropping it.
    #[test]
    fn topbar_paints_a_bottom_shadow_below_its_rect() {
        use vello_encoding::DrawTag;

        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
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
                    size: Size {
                        width: Dimension::length(800.0),
                        height: Dimension::length(40.0),
                    },
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

        let gradient_fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::LINEAR_GRADIENT)
            .count();
        assert!(
            gradient_fills >= 1,
            "topbar must emit at least one linear-gradient fill (the soft \
             drop-shadow band below its rect); got {gradient_fills}",
        );

        // Shadow rect spans `[rect.y1, rect.y1 + 28]`.
        let bar_bottom = bar.rect.y1;
        assert!(bar_bottom > 0.0, "topbar rect must have non-zero y1");
        assert!(
            (bar_bottom - f64::from(TOPBAR_HEIGHT_PX)).abs() < 0.5,
            "topbar y1 ({bar_bottom}) should equal TOPBAR_HEIGHT_PX ({TOPBAR_HEIGHT_PX})",
        );
    }

    /// Both states asserted together, since a stack that's empty (or
    /// full) in both cases would pass only half.
    #[test]
    fn a_present_collaborator_in_this_workspace_gets_an_avatar() {
        let empty = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
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
        elsewhere.collaborators = vec![crate::shell::state::Collaborator::new("alice", "Alice")];
        let bar = TopBarChrome::from_tree(&elsewhere);
        assert!(
            bar.collaborator_initials().is_empty(),
            "session-wide presence must not leak somebody from another workspace into this bar",
        );

        let mut joined = empty.clone();
        joined.workspaces[0].collaborators = vec![crate::shell::state::Collaborator::new(
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
            crate::shell::state::Collaborator::new("peer@11895642303397647149", "bob")
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

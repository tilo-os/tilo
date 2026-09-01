//! Rendering widget for the shell top bar.
//!
//! `TopBar` in `shell/topbar.rs` owns the logical state (workspace nav,
//! center-widget cooldown, tooltip timing, collaborator helpers). This file
//! provides a [`TopBarChrome`] that implements [`ui::widget::Widget`] and
//! paints the bar onto the root scene.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use chrono::Local;
use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
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

pub const TOPBAR_HEIGHT_PX: f32 = 40.0;

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
struct CacheFingerprint {
    /// Full rect, not just size — the per-frame draw repopulates hit
    /// rects from `self.rect`'s corners, so a hit must imply those are
    /// still valid.
    rect_px: (i64, i64, u64, u64),
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    collaborator_initials: Vec<char>,
    collaborator_overflow: usize,
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

/// `WorkspaceTree`, not `TopBar`, lets `build_nirios_root` stay a pure
/// function of the tree and avoids a circular borrow against the
/// shell's owned logical `TopBar`.
pub struct TopBarChrome {
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
    /// Click toggles `overlays.collaborators`.
    avatar_stack_rect: Rect,
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
    pub fn from_tree(tree: &WorkspaceTree) -> Self {
        let mut this = Self {
            rect: Rect::ZERO,
            workspace_name: String::new(),
            workspace_index: 0,
            workspace_total: 1,
            cached: Scene::new(),
            cache_fp: CacheFingerprint::default(),
            collaborator_initials: Vec::new(),
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
            avatar_stack_rect: Rect::ZERO,
            center_widget: CenterWidget::Clock,
            activity_headline: None,
            activity_unread: 0,
            activity_pulse: 0,
            activity_started: None,
            center_rect: Rect::ZERO,
            center_dot_rects: [Rect::ZERO; 3],
            center_scroll_accum: 0.0,
            last_cursor: None,
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
            collaborator_overflow: self.collaborator_overflow,
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
        // Prefer the workspace's own collaborators, falling back to the
        // tree-level roster only if empty (mirrors
        // `sidebar_wheel_chrome::sync_from_tree`).
        let roster: &[crate::shell::state::Collaborator] = match active_ws {
            Some(ws) if ws.collaborators.iter().any(|c| c.id != "me") => &ws.collaborators,
            _ => &tree.collaborators,
        };
        // An invited-but-not-arrived peer doesn't count as present.
        let present = |c: &&crate::shell::state::Collaborator| c.id != "me" && !c.invited;
        self.collaborator_initials.clear();
        self.collaborator_colors.clear();
        // Shared helper so the overflow chip and the wheel-disc's
        // active-card dot count agree.
        let peer_count = crate::shell::state::active_workspace_peer_count(tree);
        // Settled over the whole room, so a peer's avatar matches the
        // colour their cursor is drawn in elsewhere on the desktop.
        let colors = crate::shell::state::roster_colors(roster);
        for c in roster.iter().filter(present).take(3) {
            let Some(ch) = c.name.chars().next() else {
                continue;
            };
            self.collaborator_initials.push(ch.to_ascii_uppercase());
            let (r, g, b) = colors
                .get(&c.id)
                .copied()
                .unwrap_or_else(|| user_color(&c.id));
            self.collaborator_colors.push(Color::from_rgb8(r, g, b));
        }
        self.collaborator_overflow = peer_count.saturating_sub(3);
    }

    /// `QOS_CLOCK` pins it, for the same reason `QOS_SHOTS_THEME` pins
    /// the palette: a screenshot harness that compares whole frames
    /// needs to hold every part still, and a running clock would fail
    /// that comparison once a minute. Unset (every real session) reads
    /// the wall clock.
    fn current_clock_string() -> String {
        match std::env::var("QOS_CLOCK") {
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
        let icon_size = 14.0_f64;

        let mut x = self.rect.x0 + pad_x;

        self.overview_btn_rect = Rect::new(
            x - 4.0,
            center_y - icon_size / 2.0 - 4.0,
            x + icon_size + 4.0,
            center_y + icon_size / 2.0 + 4.0,
        );
        let overview_icon_rect = Rect::new(
            x,
            center_y - icon_size / 2.0,
            x + icon_size,
            center_y + icon_size / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            overview_icon_rect,
            Self::overview_glyph_icon(),
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += icon_size + f64::from(tokens.spacing.s3);

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

        let chev = 10.0_f64;
        self.up_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let up_icon_rect = Rect::new(x, center_y - chev / 2.0, x + chev, center_y + chev / 2.0);
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
        x += chev + f64::from(tokens.spacing.s2);

        self.down_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let dn_icon_rect = Rect::new(x, center_y - chev / 2.0, x + chev, center_y + chev / 2.0);
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
        x + chev + f64::from(tokens.spacing.s3)
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
        let ws_label = format!(
            "{}  {}/{}",
            self.workspace_name,
            self.workspace_index.saturating_add(1),
            self.workspace_total
        );
        // Measured at the same weight it's drawn at (Semibold), so the
        // pill width tracks the glyph advance, not the narrower Regular.
        let ws_weight = self.workspace_name_text_weight();
        let (ws_w, ws_h) = text_renderer.measure_weighted(&ws_label, ws_text_size, None, ws_weight);
        let ws_y = center_y - f64::from(ws_h) / 2.0;
        let ws_pill_outer = Rect::new(
            x - 6.0,
            center_y - 12.0,
            x + f64::from(ws_w) + 28.0,
            center_y + 12.0,
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
        let gear_rect = Rect::new(gear_x - 2.0, center_y - 6.0, gear_x + 10.0, center_y + 6.0);
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
        let band_w = 360.0_f64;
        let band_h = 28.0_f64;
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

        let dot_x = band.x0 + 12.0;
        let dot_size = Self::DOT_BASE_SIZE;
        let dot_gap = 4.0_f64;
        let dot_col_h = 3.0 * dot_size + 2.0 * dot_gap;
        let dot_top = center_y - dot_col_h / 2.0;
        let active_dot = self.center_widget.index();
        for (i, slot) in self.center_dot_rects.iter_mut().enumerate() {
            let cy = dot_top + small_count_to_f64(i) * (dot_size + dot_gap) + dot_size / 2.0;
            let active = active_dot == i;
            let r = if active {
                Self::DOT_ACTIVE_RADIUS
            } else {
                Self::DOT_INACTIVE_RADIUS
            };
            let color = if active {
                colors.accent[5]
            } else {
                colors.neutral[7]
            };
            // Generous 10 px square hit region for clickability.
            *slot = Rect::new(dot_x - 10.0, cy - 10.0, dot_x + 10.0, cy + 10.0);
            let c = vello::kurbo::Circle::new(Point::new(dot_x, cy), r);
            scene.fill(Fill::NonZero, transform, color, None, &c);
        }

        // Widget content fills the band to the right of the dots.
        let content_x0 = dot_x + 24.0;
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
        let s = Self::current_clock_string();
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
        let tray_h = 22.0_f64;
        let tray_slot = 18.0_f64;
        let tray_pad = 9.0_f64;
        let tray_gap = 4.0_f64;
        let icon_px = 14.0_f64;
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
        // Up to 3 chips, overlapping by 4px, newest on top. Routed
        // through `ui::Avatar` so the roster stays pixel-identical to
        // app rosters.
        for (i, initial) in self.collaborator_initials.iter().enumerate() {
            let cx = stack_right_x - small_count_to_f64(i) * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            // 15 px chip (r = 7.5); ring adds 1.5 px, outer disc r = 9.
            let rect = Rect::new(
                center.x - 7.5,
                center.y - 7.5,
                center.x + 7.5,
                center.y + 7.5,
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
            let cx = stack_right_x - small_count_to_f64(i) * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            let rect = Rect::new(
                center.x - 7.5,
                center.y - 7.5,
                center.x + 7.5,
                center.y + 7.5,
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

    /// Sets [`Self::avatar_stack_rect`] to either the drawn chip stack's
    /// bounds, or — when the roster is empty — draws and bounds the
    /// dashed "Invite" affordance instead. `pos` is `(center_y,
    /// stack_right_x)`.
    fn draw_avatar_stack_bounds(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        pos: (f64, f64),
        slot_count: usize,
    ) {
        let (center_y, stack_right_x) = pos;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        if slot_count > 0 {
            // Stack grows leftward from `stack_right_x`; each circle is
            // 18 px wide with 4 px overlap, so step = 14 px.
            let step = 14.0_f64;
            let leftmost_cx = stack_right_x - (small_count_to_f64(slot_count) - 1.0) * step;
            let y_mid = center_y;
            self.avatar_stack_rect =
                Rect::new(leftmost_cx - 9.0, y_mid - 11.0, stack_right_x, y_mid + 11.0);
            return;
        }
        // An empty roster is when inviting the first collaborator
        // matters most. Dashed ring + unfilled chip: an empty seat
        // isn't a person, so "Invite" is spelled out rather than `+`.
        let label = "Invite";
        let label_size = tokens.typography.scale.xs;
        let (label_w, label_h) =
            text_renderer.measure_weighted(label, label_size, None, TextWeight::Medium);
        let chip_r = 7.5_f64;
        let pill_x1 = stack_right_x + 2.0;
        let chip_cx = pill_x1 - 6.0 - chip_r;
        let label_x = chip_cx - chip_r - 6.0 - f64::from(label_w);
        let pill = Rect::new(label_x - 10.0, center_y - 12.0, pill_x1, center_y + 12.0);
        let pill_rr = RoundedRect::from_rect(pill, f64::from(tokens.radii.sm));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &pill_rr,
        );
        text_renderer.draw_text_weighted(
            scene,
            label,
            label_size,
            colors.text_dim,
            at(transform, (label_x, center_y - f64::from(label_h) / 2.0)),
            None,
            TextWeight::Medium,
        );
        let chip = vello::kurbo::Circle::new(Point::new(chip_cx, center_y), chip_r);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
            transform,
            colors.border_standard,
            None,
            &chip,
        );
        let plus = Rect::new(chip_cx - 5.0, center_y - 5.0, chip_cx + 5.0, center_y + 5.0);
        ui::icons::draw_icon(
            scene,
            transform,
            plus,
            ui::icons::Icon::Plus,
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
            let mut state = chrome.borrow_mut();
            state.overlays.room_activity = true;
            state.overlays.collaborators = false;
            state.bump();
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
            state.overlays.quick_settings = !state.overlays.quick_settings;
            state.bump();
            *handled = true;
            return;
        }
        if self.media_indicator_rect.contains(p) {
            state.overlays.quick_settings = true;
            state.bump();
            *handled = true;
            return;
        }
        if self.avatar_stack_rect.contains(p) {
            state.overlays.collaborators = !state.overlays.collaborators;
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
        let Some(headline) = self.activity_headline.as_deref() else {
            return;
        };
        let label = if self.activity_unread == 0 {
            headline.to_string()
        } else {
            format!("{}  ·  {} new", headline, self.activity_unread)
        };
        let size = theme.tokens.typography.scale.sm;
        let max_width = (self.center_rect.width() - 36.0).max(80.0) as f32;
        let (w, h) = text_renderer.measure(&label, size, Some(max_width));
        text_renderer.draw_text_weighted(
            scene,
            &label,
            size,
            theme.tokens.colors.text_primary,
            at(
                transform,
                (center_x - f64::from(w) / 2.0, center_y - f64::from(h) / 2.0),
            ),
            Some(max_width),
            TextWeight::Medium,
        );
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

        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_fp = fp;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Cached so `MouseInput` (no position) can hit-test against
        // the last draw's geometry.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            ));
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
        let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        self.handle_left_click(handled);
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
        ]
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
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

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
        );
        bar.refresh_status_and_media_state();
        assert!(bar.activity_is_visible());
        assert_eq!(bar.center_widget(), CenterWidget::Calendar);

        chrome.borrow_mut().publish_activity_glance(
            Some("Ada · Prepared launch brief".to_string()),
            0,
            42,
        );
        bar.refresh_status_and_media_state();
        assert!(!bar.activity_is_visible());
        assert_eq!(bar.center_widget(), CenterWidget::Calendar);
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
            collaborators: vec![crate::shell::state::Collaborator::new("alice", "Alice")],
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
    fn empty_roster_still_offers_a_clickable_invite_affordance() {
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
        let stack = bar.avatar_stack_rect;
        assert!(
            stack.area() > 0.0,
            "an empty roster still has to offer a control to click",
        );
        let bar_rect = bar.get_rect();
        assert!(
            stack.y0 >= bar_rect.y0 && stack.y1 <= bar_rect.y1,
            "the affordance sits inside the bar: {stack:?} vs {bar_rect:?}",
        );
        let tray = bar.tray_pill_rect();
        assert!(
            stack.x1 <= tray.x0,
            "the affordance stays left of the tray pill: stack.x1={}, tray.x0={}",
            stack.x1,
            tray.x0,
        );

        assert!(!chrome.borrow().overlays.collaborators);
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = winit::event::Modifiers::default();
        let mut focus: Option<NodeId> = None;
        for event in [
            WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(stack.center().x, stack.center().y),
            },
            WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        ] {
            let mut handled = false;
            bar.handle_event(
                &event,
                1.0,
                &modifiers,
                &mut doc,
                &doc_root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        }
        assert!(
            chrome.borrow().overlays.collaborators,
            "clicking the empty-roster affordance opened the collaborators panel",
        );
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
    fn a_peer_in_the_room_replaces_the_invite_slot_with_their_avatar() {
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
            "a room nobody is in has nobody to show, so the slot offers \
             the invite instead",
        );

        let mut joined = empty.clone();
        joined.collaborators = vec![crate::shell::state::Collaborator::new(
            "peer@11895642303397647149",
            "bob",
        )];
        let bar = TopBarChrome::from_tree(&joined);
        assert_eq!(
            bar.collaborator_initials(),
            &['X'],
            "with somebody in the room the bar must show them, not an \
             offer to invite the first person",
        );
        assert_eq!(
            bar.collaborator_overflow(),
            0,
            "one peer is not an overflow",
        );

        // Invited but not redeemed: an avatar here would falsely claim
        // he's in the workspace.
        let mut asked = empty;
        asked.collaborators = vec![
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

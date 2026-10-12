//! `SidebarWheelDisc` — radial workspace switcher docked off-screen-left,
//! each workspace a spoke; drag vertically to rotate, release snaps to the
//! nearest.
//!
//! `rotation_deg` is absolute (workspace `i` rests at `-i * spoke_gap_deg()`);
//! `draw` uses the offset from the active workspace's rest position instead
//! ([`SidebarWheelDisc::visual_residual_deg`]), so the selected rim marker
//! doesn't march down the arc as you switch.
//!
//! The wheel never edits the desktop itself: it asks its [`WheelHost`] to
//! switch or add a workspace, and reads from it where it was summoned.

use std::collections::HashMap;
use std::rc::Rc;
use ui::num::{f32_of_f64, f32_of_u32, f32_of_usize, f64_of_usize, i32_of_f64};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use taffy::{AlignItems, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::animation::Spring;
use ui::icons::Icon;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::{Gesture, SpringPresets, rubber_band};
use ui::widget::{ChipFace, ChipLead, Widget};

use tilo_desktop_model::WorkspaceId;
use ui::time::{Duration, Instant};

use crate::i18n::{t, t_args};

/// What the wheel asks of the desktop it switches.
pub trait WheelHost {
    /// Where the wheel has been summoned to, if anywhere. A summon is
    /// raised outside the wheel (a keyboard chord on the host).
    fn summon(&self) -> Option<Point>;

    /// The wheel went back to its dock.
    fn dismiss_summon(&self);

    /// The workspace the desktop shows now.
    fn active_workspace(&self) -> Option<WorkspaceId>;

    /// Show `workspace`.
    fn select_workspace(&self, workspace: WorkspaceId);

    /// Add a workspace and show it (the "+" slot).
    fn add_workspace(&self);
}

/// The accepted wheel used a 96 px left gutter. Keep that layout contract even
/// though the rim itself now stops 30 px earlier, leaving a little air between
/// the control and workspace content instead of making the disc visually huge.
pub const SIDEBAR_RESERVE_PX: f32 = 96.0;

/// A single spoke-label. `workspace_id` is an opaque correlator the host
/// uses to map the active spoke back to a workspace in the tree; the
/// widget itself never dereferences it.
#[derive(Clone, Debug)]
pub struct SpokeLabel {
    pub text: String,
    pub workspace_id: Option<u64>,
}

/// In-plane wheel disc. See module-level docs for the interaction model.
pub struct SidebarWheelDisc {
    pub spokes: Vec<SpokeLabel>,
    /// Index of the spoke currently "up" — the workspace the user would
    /// return to if they released without further dragging.
    pub active_idx: usize,
    /// Target rotation in **absolute** degrees (see module docs). The
    /// settle/follow spring's value is what's on screen while one is in
    /// flight — see [`SidebarWheelDisc::visual_rotation_deg`].
    pub rotation_deg: f32,
    /// `Some(origin)` while a drag is in flight. `wants_pointer_capture`
    /// keys off this field.
    pub drag_from: Option<Point>,
    /// Most-recent in-arc cursor position cached during idle hover, used
    /// as the press hit-test anchor. Distinct from `drag_from` so a
    /// hover over the disc never spuriously claims pointer capture.
    pub hover_anchor: Option<Point>,
    /// Rotation at the moment the drag started — anchor for the
    /// per-frame `rotation = origin + dy * DRAG_FACTOR` mapping. Seeded
    /// from where the wheel *visually* is, so grabbing one that is
    /// still settling picks it up where it stands.
    pub drag_rotation_origin: f32,
    /// Drag rotation before overscroll resistance/detent magnetism. Needed
    /// because release must distinguish overpull past an end from falling
    /// short, and the resisted value can't — it compresses any overpull
    /// into the wheel's own range.
    drag_raw_deg: f32,
    /// The disc's visible footprint inside the sidebar column.
    pub rect: Rect,
    /// Fixed, enormous disc diameter in logical pixels. Only its narrow
    /// rightmost sliver is laid out on-screen.
    pub diameter_px: f64,
    /// When present, a snap that lands on a spoke with a known
    /// `workspace_id` shows that workspace through it.
    pub host: Option<Rc<dyn WheelHost>>,
    /// `Some(spring)` while a post-release snap is easing the rim from
    /// the pre-snap rotation to the snapped target.
    pub rotation_spring: Option<Spring>,
    /// Previous `update` tick, used to feed the spring the *real* frame
    /// dt. Under frame drops a fixed dt made the spring run in
    /// slow-motion.
    pub last_update_at: Option<Instant>,
    /// Cached bounding rect of the "+" add-workspace slot. `Rect::ZERO`
    /// until the first draw pass populates it.
    pub plus_rect: Rect,
    /// Most-recent cursor position — used by the plus-slot press
    /// hit-test (which lives outside the rim) and as a fallback drag
    /// anchor for synthetic test sequences.
    pub last_cursor_pos: Option<Point>,
    /// Wheel-scroll accumulator (concept's `wheelAccumulator`). Drained
    /// `WHEEL_SCROLL_TIMEOUT_MS` after the last scroll event.
    pub wheel_accumulator: f32,
    pub wheel_last_event: Option<Instant>,
    /// Scroll pixels spent pushing against an end, where there was no
    /// next workspace to spend them on. Feeds the overscroll pull so
    /// scrolling into an end resists exactly as dragging into one does.
    wheel_overscroll_px: f32,
    /// The settle spring as it stood the moment the current settle
    /// began, so a harness can replay the curve and hold it at a chosen
    /// millisecond. See [`SidebarWheelDisc::hold_rotation_at`].
    settle_origin: Option<Spring>,
    /// Cached bounding rects of each spoke's clickable cluster, in
    /// screen-space. Consumed by `handle_event` to route a press on a
    /// non-active spoke into a `click_to_spoke` animation.
    pub spoke_hit_rects: Vec<Rect>,
    /// Scene cache covering the whole disc paint. At rest the disc is
    /// static, so caching saves the per-spoke trig and many strokes/fills.
    cached_scene: Scene,
    /// Cache-validity fingerprint — see [`SidebarWheelDisc::current_fingerprint`].
    cached_fp: WheelCacheFp,
    /// Origin the cached hit rects currently sit at, advanced by
    /// [`SidebarWheelDisc::retarget_hit_rects`] on each pan frame that takes
    /// the translate fast path, so the shift is applied once per move rather
    /// than re-applied cumulatively.
    hit_rects_origin: Point,
    /// `Some(p)`: summoned — detached from the strip's canvas, rim passing
    /// through `p`. `None`: docked, at the strip's left edge, panning with
    /// it. Summoning exists because docking scrolls off-screen with the strip.
    summon_at: Option<Point>,
}

/// Inputs that change the disc's painted pixels. The visual rotation
/// is bucketed into 0.01-degree integers so spring-in-flight values
/// invalidate on every micro-step while still being comparable.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct WheelCacheFp {
    /// Top-left of the footprint, exactly (`f64` bits). Split out from
    /// `size_px` because a plain strip pan changes only this, not a single
    /// painted pixel — letting `draw` replay the cached scene under a
    /// translation instead of re-encoding it. See
    /// [`WheelCacheFp::content_eq`]. Exact, not rounded: the replay moves
    /// the disc by exactly as far as the strip it rides moved, so a strip
    /// panned by a fraction of a layout pixel (a device pixel on a scaled
    /// display) carries the disc with it to the same place whether it is
    /// drawn on screen or in a retained canvas.
    origin_bits: (u64, u64),
    size_px: (u64, u64),
    /// `visual_rotation_deg() * 100` rounded to i64 — hundredths keep
    /// the cache stable across float noise while still invalidating on
    /// every visible animation tick.
    rotation_centi_deg: i64,
    active_idx: usize,
    spoke_count: usize,
    /// FNV-1a hash of every spoke's `text`, so a rename invalidates the
    /// cache without cloning the label vec for comparison.
    spoke_signature: u64,
    diameter_px: u64,
}

impl WheelCacheFp {
    /// True when the two fingerprints paint the *same pixels*, possibly
    /// at a different place on screen — the pan fast-path: the disc
    /// rides the strip's canvas, so re-encoding it for a pure
    /// translation is what made scrolling feel heavy.
    fn content_eq(&self, other: &Self) -> bool {
        self.size_px == other.size_px
            && self.rotation_centi_deg == other.rotation_centi_deg
            && self.active_idx == other.active_idx
            && self.spoke_count == other.spoke_count
            && self.spoke_signature == other.spoke_signature
            && self.diameter_px == other.diameter_px
    }

    /// Top-left of the footprint.
    fn origin(&self) -> Point {
        Point::new(
            f64::from_bits(self.origin_bits.0),
            f64::from_bits(self.origin_bits.1),
        )
    }

    /// A zero-width footprint means the disc has not laid out yet.
    fn is_populated(&self) -> bool {
        self.size_px.0 > 0
    }
}

/// Fraction of the enormous disc radius visible on screen. The earlier,
/// accepted implementation exposed 96 px of a 600 px radius (`0.16`). We keep
/// a much larger 2200 px radius and expose 66 px: the rim is 30 px farther
/// left, with less curvature rather than a smaller circle.
pub(crate) const VISIBLE_RADIUS_FRACTION: f64 = 0.03;

/// A fixed, deliberately huge disc. Layout must never derive this from the
/// narrow visible sliver: doing so turns the wheel into a small curved rail
/// whose ends stop inside the viewport.
const DEFAULT_DIAMETER_PX: f64 = 4_400.0;

/// Degrees of rotation per pixel of vertical drag.
const DRAG_FACTOR: f32 = 0.15;

/// Angular spacing between adjacent spokes.
const SPOKE_ANGLE_STEP_DEG: f32 = 5.5;

/// Sectors spanning the visible band once there are at least this many
/// workspaces — enough to see where you are going without any one
/// sector too small to carry a name.
const SECTORS_ACROSS_BAND: usize = 5;

/// Widens each sector past the plain `band / sectors` share, so a workspace
/// reads as a place rather than a stripe.
///
/// 5% is what the geometry affords. Past about 7% the third workspace drops
/// off a 400 px-tall sidebar — the invariant
/// `every_workspace_stays_on_the_disc_at_every_size` holds — and the rim's
/// lag behind a fast drag crosses the ceiling
/// `the_rim_trails_a_fast_drag_further_than_a_slow_one` puts on it. Taller
/// sidebars have room for more; this is a single figure for all of them.
const SECTOR_SIZE_BOOST: f32 = 1.05;

/// Floor and ceiling on the derived spoke gap. The floor is load-bearing:
/// [`slot_reach`] divides the sweep by the gap, so a degenerate (few-px-tall)
/// band could drive the gap toward zero and the slot loop toward billions of
/// iterations, wedging the process at full CPU.
const MIN_SPOKE_GAP_DEG: f32 = 1.5;
const MAX_SPOKE_GAP_DEG: f32 = 90.0;

/// Hard cap on slots either side of centre, so an unanticipated geometry
/// degrades into a wrong-looking wheel rather than an unresponsive one.
const MAX_SLOT_REACH: i32 = 64;

/// Pixels of accumulated wheel scroll required to advance one
/// workspace — a discrete detent away from the ends. Against an end,
/// where there's no next detent, the same input becomes resisted
/// overscroll instead, so the control never reads as dead.
const WHEEL_DETENT_PX: f32 = 48.0;

/// Hard cap on detents drained from one `MouseWheel` event. The
/// accumulator normally drains in a handful of steps; this only matters
/// if a delta ever arrives non-finite (e.g. `f32::INFINITY`), where it
/// stops the drain loop from spinning forever instead of ever firing.
const MAX_WHEEL_DETENTS_PER_EVENT: u32 = 4096;

/// Idle gap after which a sub-detent residual `wheel_accumulator` is
/// cleared, so a partial swipe doesn't carry into the next burst. 250 ms
/// outlasts a continuous flick but reads a pause as deliberate.
const WHEEL_SCROLL_TIMEOUT_MS: u64 = 250;

/// How far past the footprint's right edge the rim's shadow may reach;
/// the paint group is clipped this much wider so the shadow can sit
/// outside the rim and still be faded with everything else. Read by the
/// host to size the wallpaper gutter between the disc and the first
/// window (`overlay_host::WHEEL_TO_STRIP_GAP_PX` must exceed it).
pub const RIM_SHADOW_REACH: f64 = 26.0;

/// How close the band's inner arc may reach the left edge (fraction of half
/// the footprint height) — trades band depth for height; at the extreme
/// it's too shallow for a name (1600x900: 109px, "Research" truncates to
/// "Res…").
const BAND_TANGENT_FRACTION: f64 = 0.93;

/// Compact labels sit immediately outside the rim. They are navigation
/// affordances attached to one circle, not full-width slices of that circle.
/// The gap is the `spacing.s3` step; it stays `const` because the label
/// geometry is queried without a theme in hand, and the test
/// `label_metrics_sit_on_the_spacing_scale` pins it to the token.
const LABEL_RIM_GAP: f64 = 12.0;
const LABEL_MAX_W: f64 = 136.0;

/// Narrowest a chip may get before the sector shows no label at all —
/// below this even a truncated name says less than an empty sector does.
const LABEL_MIN_W: f64 = 62.0;

/// Height of an inactive sector's chip; the active sector's is
/// [`SECTOR_CHIP_H_ACTIVE`], one step larger. Sized against the slot: it
/// caps near 330 px so three fit the screen at once, and widening the
/// slot to fit a bigger chip would push the third workspace off screen.
const SECTOR_CHIP_H: f64 = 34.0;

const SECTOR_CHIP_H_ACTIVE: f64 = 42.0;

/// Smallest chip a sector label may step down to before it is truncated:
/// the shared `Chip`'s resting height (HIG §9.6 Selector, 28 px), where
/// the label is set at exactly `scale.sm` — the person's own text size,
/// which is the floor a name may shrink to. Below the two family sizes a
/// chip only gets this small when a name would otherwise lose its letters
/// — the text scale is up, or a name is long. A name that fits at no step
/// at all is elided *here*, at the person's own size, which shows the most
/// letters.
const SECTOR_CHIP_H_MIN: f64 = 28.0;

/// Diameter of the add-workspace button and how far down the band it
/// sits. Labels that would collide with it are dropped instead.
const ADD_BUTTON_D: f64 = 34.0;
const ADD_BAND_FRACTION: f64 = 0.92;

/// Angular slop past the last on-screen degree sectors are still built
/// for, so a divider or tint never stops short of the footprint corner.
const SWEEP_SLOP_DEG: f64 = 8.0;

impl SidebarWheelDisc {
    /// Visible width — `radius * VISIBLE_RADIUS_FRACTION`.
    pub fn visible_width_px(&self) -> f64 {
        (self.diameter_px / 2.0) * VISIBLE_RADIUS_FRACTION
    }

    /// Fresh disc at rest. The huge diameter is intentional: the host lays
    /// out only a narrow sliver while the circle spans beyond both edges.
    pub fn new(spokes: Vec<SpokeLabel>) -> Self {
        Self {
            spokes,
            active_idx: 0,
            rotation_deg: 0.0,
            drag_from: None,
            hover_anchor: None,
            drag_rotation_origin: 0.0,
            drag_raw_deg: 0.0,
            rect: Rect::ZERO,
            diameter_px: DEFAULT_DIAMETER_PX,
            host: None,
            rotation_spring: None,
            last_update_at: None,
            plus_rect: Rect::ZERO,
            last_cursor_pos: None,
            wheel_accumulator: 0.0,
            wheel_last_event: None,
            wheel_overscroll_px: 0.0,
            settle_origin: None,
            spoke_hit_rects: Vec::new(),
            cached_scene: Scene::new(),
            cached_fp: WheelCacheFp::default(),
            hit_rects_origin: Point::ZERO,
            summon_at: None,
        }
    }

    /// Summon the disc to `at`, or return it to the strip's left edge
    /// with `None`. Keeps its footprint and geometry; only where it
    /// sits changes.
    pub fn set_summon(&mut self, at: Option<Point>) {
        self.summon_at = at;
    }

    /// Send a summoned disc back to its dock, telling the host so it
    /// re-anchors it on the next layout.
    pub fn dismiss_summon(&mut self) {
        self.summon_at = None;
        if let Some(host) = self.host.as_ref() {
            host.dismiss_summon();
        }
    }

    /// After a snap commits a new `active_idx`, the disc shows that
    /// workspace through `host`.
    #[must_use]
    pub fn with_host(mut self, host: Rc<dyn WheelHost>) -> Self {
        self.host = Some(host);
        self
    }

    /// Builder-style override for the initially-active spoke, used by
    /// live-shell wiring. Sets the rotation to match, since a disc built
    /// with the index moved but rotation at zero would paint its fan
    /// out of place.
    #[must_use]
    pub fn with_active_idx(mut self, idx: usize) -> Self {
        self.active_idx = idx.min(self.spokes.len().saturating_sub(1));
        self.rotation_deg = self.rotation_for_idx(self.active_idx);
        self
    }

    /// Which workspace, if any, occupies each visible slot, indexed by
    /// `k + slots` for `k` in `-slots..=slots`. Wraps around the active
    /// one; each workspace is claimed at most once, nearest slot first,
    /// so a surplus slot (more slots than workspaces) stays empty
    /// instead of drawing an unreachable duplicate.
    fn slot_indices(&self, slots: i32) -> Vec<Option<usize>> {
        let n = self.spokes.len();
        let slots = slots.max(0);
        let width = usize::try_from(slots)
            .unwrap_or(0)
            .saturating_mul(2)
            .saturating_add(1);
        let mut out = vec![None; width];
        if n == 0 {
            return out;
        }
        let mut claimed = vec![false; n];
        // Nearest the active sector claims first, so surplus empties
        // fall at the outer edges where the band is fading out anyway.
        // `checked_neg` rather than `-slots`: `slots` is clamped
        // non-negative just above, so this never actually saturates —
        // it just keeps the negation from reading as raw arithmetic.
        let neg_slots = slots.checked_neg().unwrap_or(i32::MIN);
        let mut order: Vec<i32> = (neg_slots..=slots).collect();
        order.sort_by_key(|k| k.abs());
        let Ok(n_i32) = i32::try_from(n) else {
            // Absurd spoke count; nothing sane to index by.
            return out;
        };
        let Ok(active_i32) = i32::try_from(self.active_idx) else {
            return out;
        };
        for k in order {
            let Some(shifted) = active_i32.checked_add(k) else {
                continue;
            };
            let Ok(idx) = usize::try_from(shifted.rem_euclid(n_i32)) else {
                continue;
            };
            let Some(claim) = claimed.get_mut(idx) else {
                continue;
            };
            if *claim {
                continue;
            }
            *claim = true;
            let Some(pos) = k.checked_add(slots).and_then(|p| usize::try_from(p).ok()) else {
                continue;
            };
            if let Some(slot) = out.get_mut(pos) {
                *slot = Some(idx);
            }
        }
        out
    }

    /// Degrees between adjacent spokes, derived from the band the disc
    /// was actually laid out into — a fixed angle can't be right at more
    /// than one window size, since arc length per degree grows with
    /// radius. Divides the band's angular height across up to
    /// `SECTORS_ACROSS_BAND` sectors, then widens each by
    /// `SECTOR_SIZE_BOOST`.
    pub fn spoke_gap_deg(&self) -> f32 {
        let n = self.spokes.len();
        // Single-workspace disc is treated as 360° so it can't scroll
        // past its only entry.
        if n <= 1 {
            return 360.0;
        }
        let Some(span) = self.band_angular_height_deg() else {
            return SPOKE_ANGLE_STEP_DEG; // No layout yet.
        };
        let gap = span / f32_of_usize(n.min(SECTORS_ACROSS_BAND)) * SECTOR_SIZE_BOOST;
        if gap.is_finite() {
            gap.clamp(MIN_SPOKE_GAP_DEG, MAX_SPOKE_GAP_DEG)
        } else {
            SPOKE_ANGLE_STEP_DEG
        }
    }

    /// `None` before the first layout pass has given the disc a real rect.
    fn band_angular_height_deg(&self) -> Option<f32> {
        let half_h = self.rect.height() / 2.0 * BAND_TANGENT_FRACTION;
        let radius = self.diameter_px / 2.0;
        if half_h <= 1.0 || radius <= 1.0 {
            return None;
        }
        let ratio = (half_h / radius).clamp(-1.0, 1.0);
        // Angles are always tiny compared to f32's range; the mantissa
        // loss going f64 -> f32 here doesn't move a pixel on screen.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "angle in degrees, bounded well within f32's range"
        )]
        let deg = (2.0 * ratio.asin().to_degrees()) as f32;
        Some(deg)
    }

    /// Doesn't read `self` — kept as an associated fn (`Self::deg_per_pixel()`)
    /// rather than a free fn so it reads next to the geometry it's paired with.
    fn deg_per_pixel() -> f32 {
        DRAG_FACTOR
    }

    /// The absolute rotation at which workspace `idx` sits at the middle
    /// of the visible arc. Later workspaces sit at more negative
    /// rotations.
    fn rotation_for_idx(&self, idx: usize) -> f32 {
        -f32_of_usize(idx) * self.spoke_gap_deg()
    }

    /// The rotations the wheel can actually rest at, as `(last, first)`.
    /// Anything outside is overscroll.
    fn rotation_bounds(&self) -> (f32, f32) {
        let last = self.spokes.len().saturating_sub(1);
        (self.rotation_for_idx(last), 0.0)
    }

    /// Which workspace a rotation puts at the middle of the arc.
    /// Clamped: a rotation past an end still has to name the real
    /// workspace you are pulling away from.
    fn idx_at_rotation(&self, rot: f32) -> usize {
        let n = self.spokes.len();
        let Some(last) = n.checked_sub(1) else {
            return 0;
        };
        let steps = (-rot / self.spoke_gap_deg()).round();
        let clamped = steps.clamp(0.0, f32_of_usize(last));
        // `clamped` is bounded to `[0.0, last]` by the clamp just above,
        // and `last` is a spoke count — never near `usize::MAX` — so the
        // truncation this cast performs never actually discards range.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to [0, last] above; last is a tiny spoke count"
        )]
        let idx = clamped as usize;
        idx.min(last)
    }

    /// How far past an end the wheel may be pulled, asymptotically.
    /// Measured against `SPOKE_ANGLE_STEP_DEG` rather than
    /// `spoke_gap_deg()` on purpose: the single-workspace disc widens
    /// its gap to a full turn, and scaling the give by that would let
    /// it be dragged most of a revolution.
    ///
    /// Doesn't read `self` — kept as an associated fn for the same
    /// reason as [`Self::deg_per_pixel`].
    fn overscroll_reach_deg() -> f32 {
        Gesture::default().overscroll_reach_frac * SPOKE_ANGLE_STEP_DEG
    }

    /// Replace whatever part of `rot` lies past an end with its
    /// resisted equivalent. In-range rotations pass through untouched.
    fn resist_past_ends(&self, rot: f32) -> f32 {
        let (lo, hi) = self.rotation_bounds();
        let reach = Self::overscroll_reach_deg();
        if rot > hi {
            hi + rubber_band(rot - hi, reach)
        } else if rot < lo {
            lo - rubber_band(lo - rot, reach)
        } else {
            rot
        }
    }

    /// Corrective pull toward the nearest detent. A sine of the
    /// sub-detent phase: zero exactly on a detent, zero exactly between
    /// two, steepest a quarter-way in either direction, so crossing a
    /// boundary feels like a give-then-catch rather than a slide.
    /// Suppressed past the ends, where the rubber band pulls instead.
    fn detent_magnet(&self, rot: f32) -> f32 {
        let (lo, hi) = self.rotation_bounds();
        if rot > hi || rot < lo {
            return 0.0;
        }
        let gap = self.spoke_gap_deg();
        let frac = rot / gap;
        let phase = frac - frac.round();
        -Gesture::default().detent_magnet_frac * gap * (std::f32::consts::TAU * phase).sin()
    }

    /// Point the indicator at whichever workspace the wheel is *visibly*
    /// nearest right now. Reads the visual rotation, not the target, so
    /// the notch lights when the wheel passes it rather than when the
    /// hand does. Deliberately does not write to the workspace tree — a
    /// gesture in flight has not chosen anything yet.
    fn sync_active_from_visual(&mut self) {
        if self.spokes.is_empty() {
            return;
        }
        self.active_idx = self.idx_at_rotation(self.visual_rotation_deg());
    }

    /// Chase `to` with the stiff follow spring — the drag phase. Reuses
    /// whatever spring is already in flight, swapping only its
    /// constants, so grabbing a settling wheel hands its velocity to
    /// the drag rather than stopping it dead.
    fn aim_follow_spring(&mut self, from_hint: f32, to: f32) {
        let params = SpringPresets::default().tracking;
        if let Some(s) = self.rotation_spring.as_mut() {
            s.set_params(params);
            s.set_target(to);
        } else {
            let mut s = Spring::new(from_hint, params);
            s.set_target(to);
            self.rotation_spring = Some(s);
        }
    }

    /// Ease to `to` with the heavy settle spring, keeping a running spring's
    /// value/velocity so a flick decelerates instead of snapping. `from_hint`
    /// is load-bearing when no spring survives: seeding fresh from the
    /// already-updated `rotation_deg` would report settled in one frame.
    fn aim_settle_spring(&mut self, from_hint: f32, to: f32) {
        let params = SpringPresets::default().heavy;
        if let Some(s) = self.rotation_spring.as_mut() {
            s.set_params(params);
            s.set_target(to);
        } else {
            let mut s = Spring::new(from_hint, params);
            s.set_target(to);
            self.rotation_spring = Some(s);
        }
        self.settle_origin = self.rotation_spring.clone();
    }

    /// Logical centre of the disc in screen coordinates:
    /// `radius * (1 - VISIBLE_RADIUS_FRACTION)` px left of `rect.x0`.
    fn center(&self) -> Point {
        let radius = self.diameter_px / 2.0;
        let cx = self.rect.x0 - radius * (1.0 - VISIBLE_RADIUS_FRACTION);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        Point::new(cx, cy)
    }

    /// The theme's series colours ([`ui::tokens::PresencePalette::series`]):
    /// equal lightness and chroma, adjacent workspaces far apart in hue.
    ///
    /// Doesn't read `self` — kept as an associated fn (`Self::spoke_color(..)`)
    /// so it reads next to the other per-spoke painting helpers.
    fn spoke_color(idx: usize, colors: &ui::tokens::Colors) -> Color {
        colors.presence.series(idx)
    }

    /// Largest chip size step the active name fits in, from
    /// [`SECTOR_CHIP_H_ACTIVE`] down; a name that fits at no step is
    /// elided at the floor, where the label is the person's own text size.
    /// See [`Self::chip_height_for`].
    fn active_chip_height(
        &self,
        idx: usize,
        chip_w: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> f64 {
        self.chip_height_for(idx, chip_w, SECTOR_CHIP_H_ACTIVE, theme, text_renderer)
            .unwrap_or(SECTOR_CHIP_H_MIN)
    }

    /// One height for every resting sector, so the ring stays uniform: the
    /// smallest step at which every name fits, or the floor when one of
    /// them fits nowhere — the ring steps down as one rather than leaving
    /// a lone small chip beside full-size neighbours.
    fn inactive_chip_height(
        &self,
        chip_w: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> f64 {
        (0..self.spokes.len())
            .map(|idx| {
                self.chip_height_for(idx, chip_w, SECTOR_CHIP_H, theme, text_renderer)
                    .unwrap_or(SECTOR_CHIP_H_MIN)
            })
            .fold(SECTOR_CHIP_H, f64::min)
    }

    /// Largest chip size step, from `max_h` down to [`SECTOR_CHIP_H_MIN`],
    /// whose label size lets the name fit `chip_w` without an ellipsis.
    /// The chip can't widen inward (it would swing through the footprint's
    /// left edge off-centre), so a long name — or the same name at a
    /// larger text scale, where the chip's label grows with it — steps
    /// down a size instead of truncating. `None` when even the floor
    /// overflows: the person's own text size is never undercut, and the
    /// caller decides what to elide.
    fn chip_height_for(
        &self,
        idx: usize,
        chip_w: f64,
        max_h: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> Option<f64> {
        const STEP: f64 = 2.0;
        let Some(spoke) = self.spokes.get(idx) else {
            return Some(max_h);
        };
        let lead = ChipLead::Glyph(self.spoke_icon(idx));
        // Counted as integer steps up from the floor rather than by
        // subtracting floats, so the loop can't accumulate drift.
        let steps = ((max_h - SECTOR_CHIP_H_MIN) / STEP).max(0.0).round();
        // Bounded by the two family sizes, so this is a handful of steps.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative step count of at most (42 - 28) / 2 = 7"
        )]
        let steps = steps as u32;
        for i in (0..=steps).rev() {
            let step = SECTOR_CHIP_H_MIN + f64::from(i) * STEP;
            let need = ChipFace::intrinsic_width(&spoke.text, lead, false, text_renderer, theme);
            if need <= chip_w {
                return Some(step);
            }
        }
        None
    }

    /// The width available to a sector's label chip, in pixels — what
    /// `draw` uses, exposed for callers needing the same answer. Labels sit
    /// outside the rim, so a larger text scale widens them instead of
    /// eliding the name the person enlarged.
    pub fn label_chip_width(theme: &Theme) -> f64 {
        LABEL_MAX_W * f64::from(theme.text_scale.max(1.0))
    }

    /// Lucide icon for the spoke at `idx`, by lower-cased label;
    /// unknown labels default to `Layers`.
    pub fn spoke_icon(&self, idx: usize) -> Icon {
        let label = self
            .spokes
            .get(idx)
            .map(|s| s.text.to_lowercase())
            .unwrap_or_default();
        spoke_icon_for_label(&label)
    }

    /// The absolute rotation currently on screen: the spring's value
    /// while one is in flight, the target once it has landed.
    pub fn visual_rotation_deg(&self) -> f32 {
        match self.rotation_spring.as_ref() {
            Some(s) if !s.is_settled() => s.value(),
            _ => self.rotation_deg,
        }
    }

    /// The workspace the wheel has **committed** to — where it's headed and
    /// what's written to the tree. Distinct from [`SidebarWheelDisc::active_idx`],
    /// the visual indicator: the two differ only mid-gesture, when the
    /// indicator tracks the hand but the tree writes once, at the end.
    pub fn selected_idx(&self) -> usize {
        self.idx_at_rotation(self.rotation_deg)
    }

    /// Visual rotation relative to the active sector's rest position — what
    /// `draw` turns the fan by, so the lit sector doesn't march down the arc
    /// as `active_idx` changes. Zero at rest; ranges roughly ±half a detent
    /// mid-gesture.
    pub fn visual_residual_deg(&self) -> f32 {
        self.visual_rotation_deg() - self.rotation_for_idx(self.active_idx)
    }

    /// Snapshot every input that affects the disc's painted pixels.
    /// Used by [`Widget::draw`] to decide whether `self.cached_scene`
    /// can be reused. See [`WheelCacheFp`] for the field semantics.
    fn current_fingerprint(&self) -> WheelCacheFp {
        let origin_bits = (self.rect.x0.to_bits(), self.rect.y0.to_bits());
        let size_px = (
            f64_round_to_u64_lossy(self.rect.width()),
            f64_round_to_u64_lossy(self.rect.height()),
        );
        // 0.01° granularity: fine enough to invalidate every visible tick,
        // coarse enough to ignore float noise at rest.
        let rotation_centi_deg =
            f64_round_to_i64_lossy(f64::from(self.visual_rotation_deg()) * 100.0);
        // FNV-1a over spoke labels, avoiding a Vec clone just to detect renames.
        let mut hasher = fnv::FnvHasher::default();
        for spoke in &self.spokes {
            std::hash::Hasher::write(&mut hasher, spoke.text.as_bytes());
            // Separator byte so "ab"+"c" and "a"+"bc" hash differently.
            std::hash::Hasher::write_u8(&mut hasher, 0xff);
        }
        let h = std::hash::Hasher::finish(&hasher);
        WheelCacheFp {
            origin_bits,
            size_px,
            rotation_centi_deg,
            active_idx: self.active_idx,
            spoke_count: self.spokes.len(),
            spoke_signature: h,
            diameter_px: f64_round_to_u64_lossy(self.diameter_px),
        }
    }

    /// Inside the rect's footprint (rejecting clicks off-screen-left)
    /// AND within the circle's radius — so a footprint corner the arc
    /// doesn't actually cover is correctly rejected.
    fn hit_visible_arc(&self, p: Point) -> bool {
        if !self.rect.contains(p) {
            return false;
        }
        let c = self.center();
        let radius = self.diameter_px / 2.0;
        let dx = p.x - c.x;
        let dy = p.y - c.y;
        dx * dx + dy * dy <= radius * radius
    }
}

/// Rounds and casts to `i64` for cache-fingerprint fields: screen
/// coordinates and rotation-centidegrees never come remotely close to
/// `i64`'s range, so this never actually truncates.
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen coordinates and rotation centidegrees are always tiny relative to i64::MAX"
)]
fn f64_round_to_i64_lossy(v: f64) -> i64 {
    v.round() as i64
}

/// How far `to` lies from `from`, exactly.
fn offset(from: Point, to: Point) -> (f64, f64) {
    (to.x - from.x, to.y - from.y)
}

/// Same rationale as [`f64_round_to_i64_lossy`], for the always-non-negative
/// size/diameter fingerprint fields.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "screen sizes are always non-negative and tiny relative to u64::MAX"
)]
fn f64_round_to_u64_lossy(v: f64) -> u64 {
    v.round() as u64
}

/// Clamps an alpha-channel value into `u8`'s range before the (otherwise
/// saturating but silently-wrong-looking) narrowing cast, for stroke/fill
/// alphas computed from a fixed base times a `[0, 1]`-ish fade factor.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "explicitly clamped into u8's range on the line above"
)]
fn f32_to_u8_saturating(v: f32) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

/// Safe lookup into a [`SidebarWheelDisc::slot_indices`] result for
/// display offset `k` at reach `slots`: `None` for a `k` outside
/// `[-slots, slots]` or a slot the layout left empty. Shared by both
/// draw passes that walk the fan, so the bounds check lives in one place.
fn slot_at(slot_idx: &[Option<usize>], k: i32, slots: i32) -> Option<usize> {
    let pos = k.checked_add(slots).and_then(|p| usize::try_from(p).ok())?;
    slot_idx.get(pos).copied().flatten()
}

/// How many display slots either side of the active one the sector fan
/// must run to cover `sweep` radians, plus one so an entering sector's
/// leading divider is already drawn. Zero for a gap of half a turn or
/// more — the single-workspace case, where one slot is the whole disc.
fn slot_reach(sweep_rad: f64, gap_deg: f64) -> i32 {
    // A non-positive/non-finite gap saturates the cast to i32::MAX,
    // which the caller then walks one slot at a time.
    if !gap_deg.is_finite() || gap_deg <= 0.0 || gap_deg >= 180.0 {
        return 0;
    }
    let reach = (sweep_rad.to_degrees() / gap_deg).ceil();
    if !reach.is_finite() {
        return 0;
    }
    i32_of_f64(reach).saturating_add(1).clamp(0, MAX_SLOT_REACH)
}

/// Point on the circle of radius `r` about `c` at `angle` radians,
/// measured from due-east and increasing downward (screen coordinates).
fn polar(c: Point, r: f64, angle: f64) -> Point {
    Point::new(c.x + r * angle.cos(), c.y + r * angle.sin())
}

/// Solid pie slice from centre to `r_out` between `a0`/`a1`. Distinct from
/// [`annular_segment`] with `r_in = 0`, which would emit 97 coincident
/// centre points per sector per frame.
fn disc_segment(c: Point, r_out: f64, a0: f64, a1: f64) -> BezPath {
    const STEPS: usize = 96;
    let mut path = BezPath::new();
    let span = a1 - a0;
    path.move_to(c);
    for i in 0..=STEPS {
        path.line_to(polar(
            c,
            r_out,
            a0 + span * (f64_of_usize(i) / f64_of_usize(STEPS)),
        ));
    }
    path.close_path();
    path
}

/// Workspace label → lucide glyph. Inputs are lower-cased;
/// `dev`/`development` both map to `Code`.
fn spoke_icon_for_label(label: &str) -> Icon {
    match label {
        "design" => Icon::PenTool,
        "dev" | "development" => Icon::Code,
        "marketing" => Icon::FileText,
        "analytics" => Icon::BarChart,
        "settings" => Icon::Settings,
        "home" => Icon::Home,
        "research" => Icon::Search,
        // "concept" and every other unmapped label share the fallback.
        _ => Icon::Layers,
    }
}

impl Widget for SidebarWheelDisc {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Leaf reserves the visible-arc width and takes 100% of the
        // parent slot's height — the disc's centre sits off-screen-left,
        // concept-faithful "huge wheel" feel.
        taffy.widget_leaf(
            Style {
                size: Size {
                    width: Dimension::length(f32_of_f64(self.visible_width_px())),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 0.0,
                flex_shrink: 0.0,
                align_self: Some(AlignItems::Stretch),
                ..Default::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Pick up a summon raised elsewhere on the host (the keyboard
        // chord lives with whatever has the pointer); kept as its own copy
        // so tests and hostless wheels can drive it directly through
        // `set_summon`.
        if let Some(host) = self.host.as_ref() {
            self.summon_at = host.summon();
        }
        // `TaffyTree::layout` only ever returns `Err` for a config this
        // compositor never sets (rounding is always on); a stale/foreign
        // `node` would panic inside taffy's own slotmap index before this
        // Result is produced, so `unwrap` here wasn't guarding anything
        // this `let...else` doesn't already cover.
        #[allow(
            clippy::unreachable,
            reason = "verified against taffy 0.10.1's own source \
                      (tree/taffy_tree.rs): layout() always returns Ok in \
                      both the rounded and unrounded branches; a bad NodeId \
                      panics inside the slotmap index first, never through \
                      this Result"
        )]
        let Ok(l) = taffy.layout(node) else {
            unreachable!("TaffyTree::layout only errors on a config this compositor never sets")
        };
        // Keep the docked rail edge-to-edge. The circle itself supplies the
        // taper toward the top and bottom; cropping the slot and then fading
        // it made the rail dissolve before it reached either screen edge.
        let x0 = origin.x + f64::from(l.location.x);
        let y0_raw = origin.y + f64::from(l.location.y);
        let h_raw = f64::from(l.size.height);
        let w = f64::from(l.size.width);
        let y0 = y0_raw;
        let y1 = y0_raw + h_raw;
        self.rect = match self.summon_at {
            // Docked: the slot's own box, already shifted by the pan.
            None => Rect::new(x0, y0, x0 + w, y1),
            // Summoned: same size, re-placed so the rim's rightmost
            // point sits under the pointer, centred vertically. Clamped
            // inside the slot's band so an edge summon slides into view
            // rather than hanging off it.
            Some(p) => {
                let slot_top = y0_raw;
                let slot_bottom = y0_raw + h_raw;
                let height = y1 - y0;
                let x1 = p.x.max(origin.x + w);
                let top =
                    (p.y - height / 2.0).clamp(slot_top, (slot_bottom - height).max(slot_top));
                Rect::new(x1 - w, top, x1, top + height)
            }
        };
        // Keep the disc's radius independent from the sliver the layout gives
        // it. Re-deriving the diameter from this narrow box makes a small rail;
        // the fixed huge circle is what lets one rim continue through both
        // viewport edges with almost no visible curvature.
        // Re-anchor on the workspace index, not the stored rotation — the gap
        // just recomputed changes what workspace it names. Only at rest:
        // mid-gesture this would pull the target onto the indicator and a
        // drag/settle would never arrive.
        let at_rest = self.drag_from.is_none()
            && self
                .rotation_spring
                .as_ref()
                .is_none_or(ui::Spring::is_settled);
        if at_rest {
            self.rotation_deg = self.rotation_for_idx(self.active_idx);
            if let Some(spring) = self.rotation_spring.as_mut() {
                spring.snap(self.rotation_deg);
            }
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
        // Fingerprint every input that affects painted pixels, then try
        // the cache before paying for a fresh encode.
        let fp = self.current_fingerprint();
        if self.try_paint_from_cache(out_scene, out_transform, &fp) {
            return;
        }

        // Cache miss — encode at IDENTITY, then splice with
        // `out_transform`. `mem::take` gives the encode body an
        // exclusive `&mut Scene` without aliasing `self`.
        let mut local = std::mem::take(&mut self.cached_scene);
        local.reset();

        let Some(geom) = self.disc_geometry(theme) else {
            // Restore the (empty) cache without recording a fingerprint
            // so a zero-width rect doesn't lock the cache.
            self.cached_scene = local;
            return;
        };
        let mut ctx = DrawCtx {
            scene: &mut local,
            transform: Affine::IDENTITY,
            theme,
            text_renderer,
            presence,
        };

        // ---- Pass 1: the disc -------------------------------------------
        // Body, edge and drop shadow are one isolated group. The circular
        // silhouette itself tapers toward the screen edges; no opacity mask
        // is applied, so the rail visibly reaches the top and bottom. The
        // horizontal clip is load-bearing — unclipped, the ring would sweep
        // back around off-screen; it runs past the right edge for the shadow.
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &geom.group_clip,
        );
        Self::draw_band_shadow_and_body(&mut ctx, &geom);
        // One uninterrupted rim defines the object. Workspace state is a
        // compact marker on this edge, never a filled pie slice.
        Self::draw_rim(&mut ctx, &geom);

        ctx.scene.pop_layer();

        // ---- Pass 3 & 4: labels, then the add-workspace button ------
        // Labels naturally leave the visible sweep as the wheel turns.
        self.position_add_button(&geom);
        self.draw_labels(&mut ctx, &geom);
        self.draw_add_button(&mut ctx, &geom);

        // Splice the freshly-encoded scene into `out_scene`, then
        // restore `self.cached_scene` and record the fingerprint so
        // the next idle frame takes the fast path.
        out_scene.append(&local, Some(out_transform));
        self.cached_scene = local;
        self.hit_rects_origin = fp.origin();
        self.cached_fp = fp;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => self.handle_left_press(ctx.handled),
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => self.handle_left_release(ctx.handled),
            WindowEvent::MouseWheel { delta, .. } => self.handle_mouse_wheel(delta, ctx.handled),
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag_from.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        // A drag is a stream of `CursorMoved`; the host's incremental gate
        // skips repaint on pointer move unless something reports work —
        // without this, the wheel sits still under the finger and jumps on release.
        let spring_active = self
            .rotation_spring
            .as_ref()
            .is_some_and(|s| !s.is_settled());
        spring_active || self.wheel_last_event.is_some() || self.drag_from.is_some()
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        // Real (clamped) frame dt keeps the spring wall-clock-correct
        // under frame drops. Clamped to 1/30 s: semi-implicit Euler
        // stays stable, and a long stall just resumes rather than
        // exploding.
        let now = Instant::now();
        // `saturating_duration_since` rather than `now - prev`: `Instant`
        // subtraction panics if `prev` is somehow later than `now` (not
        // guaranteed impossible across every platform's clock), and a
        // clamped zero dt for that one frame is a fine answer either way.
        let dt = self.last_update_at.map_or(1.0 / 60.0, |prev| {
            now.saturating_duration_since(prev)
                .as_secs_f32()
                .clamp(0.0, 1.0 / 30.0)
        });
        self.last_update_at = Some(now);
        let mut keep = self.advance(dt);
        // Clears the sub-detent residual `wheel_accumulator` once the
        // user pauses, so a partial scroll doesn't silently bias the
        // next gesture into a switch the user thought they'd cancelled.
        if let Some(last) = self.wheel_last_event {
            if last.elapsed() > Duration::from_millis(WHEEL_SCROLL_TIMEOUT_MS) {
                self.wheel_accumulator = 0.0;
                self.wheel_last_event = None;
                // A scroll wheel has no equivalent of lifting a finger,
                // so the idle gap is the release: any pull held against
                // an end lets go and the wheel returns to the end.
                if self.wheel_overscroll_px != 0.0 {
                    self.wheel_overscroll_px = 0.0;
                    let from = self.visual_rotation_deg();
                    let home = self.rotation_for_idx(self.active_idx);
                    self.rotation_deg = home;
                    self.aim_settle_spring(from, home);
                }
            }
            // Even sub-threshold movement holds the redraw loop awake
            // until the idle timer clears the accumulator.
            keep = true;
        }
        keep
    }

    fn name(&self) -> &'static str {
        "SidebarWheelDisc"
    }

    /// Stable e2e test identifier — scenarios that click the disc itself
    /// (vs. an individual spoke) address it through the driver registry.
    fn test_id(&self) -> Option<&str> {
        Some("wheel_disc")
    }

    /// One button per workspace on the disc plus the add button, each
    /// with the rect the last paint gave it — so a screen reader, a script
    /// and the UX bench address a sector by name rather than by guessing
    /// where a rotated label landed. Names are verbs ("Switch to Home")
    /// so they never collide with the overview's own "Home" row while both
    /// are on screen; the stable `wheel:*` keys are the scriptable
    /// address. A workspace not currently on the band is hidden rather
    /// than dropped, so its ordinal stays fixed.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children: Vec<AccessNode> = self
            .spokes
            .iter()
            .enumerate()
            .map(|(idx, spoke)| {
                let key = Self::spoke_action_target(idx);
                let placed = self.spoke_hit_rects.get(idx).copied().unwrap_or(Rect::ZERO);
                let mut node = AccessNode::new(Role::Button)
                    .with_name(t_args(
                        "a11y-shell-wheel-switch",
                        &[("workspace", (&spoke.text).into())],
                    ))
                    .with_semantic_key(key.clone())
                    .with_action_target(key)
                    .with_key(u64::try_from(idx).unwrap_or(u64::MAX))
                    .with_state(AccessState {
                        selected: idx == self.active_idx,
                        hidden: placed.area() <= 0.0,
                        ..AccessState::default()
                    });
                if placed.area() > 0.0 {
                    node = node.with_bounds(placed);
                }
                node
            })
            .collect();
        let mut add = AccessNode::new(Role::Button)
            .with_name(t("a11y-shell-wheel-add"))
            .with_semantic_key(Self::ADD_ACTION_TARGET)
            .with_action_target(Self::ADD_ACTION_TARGET)
            .with_key(u64::try_from(self.spokes.len()).unwrap_or(u64::MAX))
            .with_state(AccessState {
                hidden: self.plus_rect.area() <= 0.0,
                ..AccessState::default()
            });
        if self.plus_rect.area() > 0.0 {
            add = add.with_bounds(self.plus_rect);
        }
        children.push(add);
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("a11y-shell-wheel"))
                .with_semantic_key("wheel")
                .with_children(children),
        )
    }

    /// Geometry of the node at `path` — a sector's painted hit rect or the
    /// add button — so the host's generic fallback (a synthesised click)
    /// lands on the sector rather than on the middle of the whole disc.
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let &idx = path.first()?;
        let rect = match idx.cmp(&self.spokes.len()) {
            std::cmp::Ordering::Less => self.spoke_hit_rects.get(idx).copied()?,
            std::cmp::Ordering::Equal => self.plus_rect,
            std::cmp::Ordering::Greater => return None,
        };
        (rect.area() > 0.0).then_some(rect)
    }

    /// The production route: the a11y dispatcher hands a node's
    /// `action_target` here. `wheel:workspace:N` turns the wheel to that
    /// sector exactly as a click does; `wheel:add` creates a workspace.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let idx = if target == Self::ADD_ACTION_TARGET {
            self.spokes.len()
        } else if let Some(n) = target
            .strip_prefix("wheel:workspace:")
            .and_then(|n| n.parse::<usize>().ok())
        {
            n
        } else {
            return false;
        };
        self.activate_node(idx)
    }

    /// Key-path route for hosts that address nodes by ordinal; the same
    /// nodes, the same effect as [`Widget::perform_semantic_action`].
    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(&key) = key_path.first() else {
            return false;
        };
        let Ok(idx) = usize::try_from(key) else {
            return false;
        };
        self.activate_node(idx)
    }

    /// Opt into downcasting so the screenshot harness can reach
    /// [`SidebarWheelDisc::hold_rotation_at`].
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Without this override the default leaf impl returns `true` every
    /// frame, defeating the host's "skip redraw on hover-only
    /// `CursorMoved`" gate.
    fn needs_redraw(&self) -> bool {
        // Dirty when the live fingerprint differs from the cached one, or a
        // spring is in flight — needed for a drag's first frame, where the
        // target has moved but the spring hasn't stepped, so the fingerprint
        // still matches and reporting clean would deadlock the gesture
        // (repaint is the only thing that pumps `update`, which moves the spring).
        let spring_in_flight = self
            .rotation_spring
            .as_ref()
            .is_some_and(|s| !s.is_settled());
        spring_in_flight
            || !self.cached_fp.is_populated()
            || self.current_fingerprint() != self.cached_fp
    }

    fn mark_drawn(&mut self) {
        // No-op — `cached_fp` is updated inside `draw` itself.
    }

    /// Fixed-width leaf, so layout never needs recomputing after the
    /// first pass.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

/// The mutable/borrowed painting context `draw`'s passes share: the scene
/// to paint into, the (identity, during a cache-miss encode) transform,
/// theme and text shaping, and the presence roster for peer dots. Bundled
/// so each pass helper takes one argument instead of five.
struct DrawCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    theme: &'a Theme,
    text_renderer: &'a mut TextRenderer,
    presence: &'a PresenceCtx,
}

/// The disc's shape for one `draw` call, computed once from
/// `rect`/`diameter_px` so every pass paints from the same numbers. See
/// [`SidebarWheelDisc::disc_geometry`] for how each field is derived.
struct DiscGeometry {
    /// Logical centre, screen coordinates.
    c: Point,
    r_out: f64,
    sweep: f64,
    half_h: f64,
    group_clip: Rect,
    body_hue: Color,
}

/// Sector-label chip geometry, constant across one call to
/// [`SidebarWheelDisc::draw_labels`].
struct LabelLayout {
    radial_start: f64,
    max_w: f64,
    /// The one height every resting sector's chip is drawn at this
    /// frame — see [`SidebarWheelDisc::inactive_chip_height`].
    inactive_h: f64,
}

/// One resolved display slot in the sector loop — a `k` in
/// `neg_slots..=slots` that survived the sweep/slot-index checks, ready
/// to paint.
struct SectorSlot {
    idx: usize,
    /// Angle in radians from due-east, matching [`polar`].
    mid: f64,
    is_active: bool,
}

impl SidebarWheelDisc {
    /// Cache fast-path: at rest nothing painted changes frame to frame,
    /// so a hit against `fp` lets `draw` replay `self.cached_scene`
    /// instead of re-encoding (hit rects stay valid — the prior
    /// cache-miss draw populated them at the same rotation/rect). A
    /// pan-only change replays under a translation. Returns `true` when
    /// it painted from the cache and `draw` should return without a
    /// fresh encode.
    fn try_paint_from_cache(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        fp: &WheelCacheFp,
    ) -> bool {
        if !fp.is_populated() || !self.cached_fp.is_populated() {
            return false;
        }
        if *fp == self.cached_fp {
            out_scene.append(&self.cached_scene, Some(out_transform));
            return true;
        }
        if fp.content_eq(&self.cached_fp) {
            let shift = Affine::translate(offset(self.cached_fp.origin(), fp.origin()));
            // `Affine * Affine` is matrix composition — total, no
            // overflow/panic path, unlike scalar arithmetic.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "affine composition, not scalar arithmetic"
            )]
            let shifted = out_transform * shift;
            out_scene.append(&self.cached_scene, Some(shifted));
            self.retarget_hit_rects(fp.origin());
            return true;
        }
        false
    }

    /// The disc's shape for this `draw` call — `None` when the footprint
    /// is degenerate (zero-width slot, first layout pass), the signal
    /// `draw` uses to bail without painting or touching the fingerprint.
    fn disc_geometry(&self, theme: &Theme) -> Option<DiscGeometry> {
        let radius = self.diameter_px / 2.0;
        if radius <= 1.0 || self.rect.area() <= 0.0 {
            return None;
        }
        let c = self.center();

        let half_h = self.rect.height() / 2.0;
        let r_out = radius;
        // This is one huge circle. The sweep only bounds tessellation; it is
        // extended a little beyond the viewport so the fill and rim cannot
        // reveal a short end at either edge.
        let sweep = (half_h / r_out).clamp(-1.0, 1.0).asin() + SWEEP_SLOP_DEG.to_radians();
        let group_clip = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1 + RIM_SHADOW_REACH,
            self.rect.y1,
        );
        // Flat fill — the floating-chrome surface, opaque: this renderer
        // cannot blur what is behind the disc.
        let body_hue = theme.tokens.colors.surfaces.overlay;

        Some(DiscGeometry {
            c,
            r_out,
            sweep,
            half_h,
            group_clip,
            body_hue,
        })
    }

    /// Labels stay fully opaque until their sector leaves the circular
    /// sweep. This intentionally has no edge fade: the rail should read as
    /// one solid object continuing all the way to the screen boundaries.
    fn row_fade(&self, y: f64) -> f32 {
        f32::from(self.rect.y0 <= y && y <= self.rect.y1)
    }

    /// Pass 1 — the band: soft multi-ring drop shadow (same falloff as
    /// the window-decoration pills, tinted from the theme's elevation
    /// token rather than a literal black, so a shadow-suppressing theme
    /// gets its own answer), then one flat body fill solid to centre.
    fn draw_band_shadow_and_body(ctx: &mut DrawCtx<'_>, geom: &DiscGeometry) {
        let shadow_hue = ctx.theme.tokens.elevation.glass_outer.color;
        for &(offset, width, alpha) in &[(2.5_f64, 5.0_f64, 44u8), (7.5, 6.0, 20), (14.0, 9.0, 8)] {
            ctx.scene.stroke(
                &Stroke::new(width),
                ctx.transform,
                Color::from_rgba8(shadow_hue.r(), shadow_hue.g(), shadow_hue.b(), alpha),
                None,
                &Circle::new((geom.c.x, geom.c.y), geom.r_out + offset),
            );
        }
        let band = disc_segment(geom.c, geom.r_out, -geom.sweep, geom.sweep);
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, geom.body_hue, None, &band);
    }

    /// The rim is the body's one lit edge, carrying the silhouette on
    /// its own: one soft halo and one definite hairline. A second inset arc
    /// reads as a separate rail at this enormous radius rather than depth.
    fn draw_rim(ctx: &mut DrawCtx<'_>, geom: &DiscGeometry) {
        let edge_hue = ctx.theme.tokens.colors.text_primary;
        let edge = |a: u8| Color::from_rgba8(edge_hue.r(), edge_hue.g(), edge_hue.b(), a);
        let rim = Circle::new((geom.c.x, geom.c.y), geom.r_out);
        ctx.scene
            .stroke(&Stroke::new(5.0), ctx.transform, edge(26), None, &rim);
        ctx.scene
            .stroke(&Stroke::new(1.5), ctx.transform, edge(140), None, &rim);
    }

    /// Seat the add button directly on the lower rim. It belongs to the wheel;
    /// placing it against an imaginary inner band made it look detached.
    fn position_add_button(&mut self, geom: &DiscGeometry) {
        let add_center_y = geom.c.y + geom.half_h * ADD_BAND_FRACTION;
        let add_delta_y = add_center_y - geom.c.y;
        let add_radius = geom.r_out - 6.0;
        let rim_span = (add_radius * add_radius - add_delta_y * add_delta_y)
            .max(0.0)
            .sqrt();
        let add_center_x = geom.c.x + rim_span;
        self.plus_rect = Rect::from_center_size(
            Point::new(add_center_x, add_center_y),
            (ADD_BUTTON_D, ADD_BUTTON_D),
        );
    }

    /// Pass 3 — compact workspace labels and markers attached to the rim.
    /// Each still rotates with the same wheel geometry and claims the same
    /// click target; only the oversized pie-slice presentation is gone. See
    /// [`Self::draw_one_sector_label`].
    fn draw_labels(&mut self, ctx: &mut DrawCtx<'_>, geom: &DiscGeometry) {
        let max_w = Self::label_chip_width(ctx.theme);
        let inactive_h = self.inactive_chip_height(max_w, ctx.theme, ctx.text_renderer);
        let layout = LabelLayout {
            radial_start: geom.c.x + geom.r_out + LABEL_RIM_GAP,
            max_w,
            inactive_h,
        };

        self.spoke_hit_rects.clear();
        self.spoke_hit_rects.resize(self.spokes.len(), Rect::ZERO);
        // A workspace can appear more than once in the sweep when there
        // are few of them; a click goes to the slot nearest the middle.
        let mut hit_claim = vec![f64::INFINITY; self.spokes.len()];

        if self.spokes.is_empty() {
            return;
        }
        let gap = f64::from(self.spoke_gap_deg());
        let rot = f64::from(self.visual_residual_deg());
        let slots = slot_reach(geom.sweep, gap);
        let slot_idx = self.slot_indices(slots);
        // `slots` is always >= 0 (see `slot_reach`); `checked_neg`
        // rather than `-slots` just keeps the negation from reading
        // as raw arithmetic to clippy.
        let neg_slots = slots.checked_neg().unwrap_or(i32::MIN);
        for k in neg_slots..=slots {
            let mid_deg = rot + f64::from(k) * gap;
            let mid = mid_deg.to_radians();
            if mid.abs() > geom.sweep {
                continue;
            }
            let Some(idx) = slot_at(&slot_idx, k, slots) else {
                continue;
            };
            let slot = SectorSlot {
                idx,
                mid,
                is_active: k == 0,
            };
            self.draw_one_sector_label(ctx, geom, &layout, &slot, &mut hit_claim);
        }
    }

    /// One sector's label chip, plus its click-target claim and presence
    /// dots — the per-`k` body factored out of [`Self::draw_labels`].
    fn draw_one_sector_label(
        &mut self,
        ctx: &mut DrawCtx<'_>,
        geom: &DiscGeometry,
        layout: &LabelLayout,
        slot: &SectorSlot,
        hit_claim: &mut [f64],
    ) {
        // Where the sector's centreline meets the rim — the label's y,
        // and what the vertical fade is sampled at.
        let anchor = polar(geom.c, geom.r_out, slot.mid);
        let fade_a = self.row_fade(anchor.y);
        if fade_a <= 0.02 {
            return;
        }

        let lead = ChipLead::Glyph(self.spoke_icon(slot.idx));
        // Both family sizes step down for a name that would not fit,
        // rather than showing "H…" for "Home" at a larger text scale; the
        // resting sectors step down together so the ring stays uniform.
        let row_h = if slot.is_active {
            self.active_chip_height(slot.idx, layout.max_w, ctx.theme, ctx.text_renderer)
        } else {
            layout.inactive_h
        };
        let label = self.spokes.get(slot.idx).map_or("", |s| s.text.as_str());
        let natural_w = ChipFace::intrinsic_width(label, lead, false, ctx.text_renderer, ctx.theme);
        let chip_w = natural_w.clamp(LABEL_MIN_W, layout.max_w);
        let chip = Rect::new(
            layout.radial_start,
            geom.c.y - row_h / 2.0,
            layout.radial_start + chip_w,
            geom.c.y + row_h / 2.0,
        );
        let rotate = Affine::rotate_about(slot.mid, geom.c);

        // Where the chip actually lands once rotated. Used for the
        // click target and to keep labels off the add button.
        let placed = rotate.transform_rect_bbox(chip);
        if self.label_clashes_with_add_button(chip, rotate) {
            return;
        }

        Self::draw_rim_marker(ctx, geom, slot, fade_a);

        // `Affine * Affine` is matrix composition — total, no
        // overflow/panic path, unlike scalar arithmetic.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "affine composition, not scalar arithmetic"
        )]
        let chip_transform = ctx.transform * rotate;
        // Labels hang outside the rim over the workspace's wallpaper, which
        // can be any image under any theme, so each is a raised pill rather
        // than bare text; the active workspace's is lifted.
        ChipFace::new(chip, label)
            .lead(lead)
            .selected(slot.is_active)
            .opacity(fade_a)
            .paint(ctx.scene, chip_transform, ctx.text_renderer, ctx.theme);

        let marker = Rect::from_center_size(anchor, (28.0, 28.0));
        self.claim_hit_rect(hit_claim, slot, placed.union(marker));
        self.draw_presence_dots(ctx, geom, slot);
    }

    /// Restrained selection treatment: a small seat on the rim and, for the
    /// active workspace, a short coloured glint. Nothing reaches toward the
    /// disc centre, so it can never read as a pizza sector.
    fn draw_rim_marker(ctx: &mut DrawCtx<'_>, geom: &DiscGeometry, slot: &SectorSlot, alpha: f32) {
        let colors = &ctx.theme.tokens.colors;
        let base = Self::spoke_color(slot.idx, colors);
        let anchor = polar(geom.c, geom.r_out, slot.mid);
        if slot.is_active {
            let mut glint = BezPath::new();
            let half = 1.15_f64.to_radians();
            for step in 0_u32..=12 {
                let t = f64::from(step) / 12.0;
                let p = polar(geom.c, geom.r_out, slot.mid - half + t * half * 2.0);
                if step == 0 {
                    glint.move_to(p);
                } else {
                    glint.line_to(p);
                }
            }
            ctx.scene.stroke(
                &Stroke::new(3.0),
                ctx.transform,
                base.with_alpha_factor(0.72 * alpha),
                None,
                &glint,
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                base.with_alpha_factor(0.18 * alpha),
                None,
                &Circle::new(anchor, 11.0),
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                base.with_alpha_factor(alpha),
                None,
                &Circle::new(anchor, 5.0),
            );
        } else {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.text_dim.with_alpha_factor(0.72 * alpha),
                None,
                &Circle::new(anchor, 3.0),
            );
        }
    }

    /// Whether `chip` — unrotated, in the sector's own frame — would
    /// overlap the add button once `rotate` places it. Clash test done
    /// in the label's OWN frame (button centre rotated back, against
    /// the unrotated chip) — the rotated chip's axis-aligned bbox is
    /// far bigger than the chip itself and would falsely clash with a
    /// button the label passes clear of.
    fn label_clashes_with_add_button(&self, chip: Rect, rotate: Affine) -> bool {
        // `Affine * Point` applies the transform — total, no
        // overflow/panic path, unlike scalar arithmetic.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "affine point transform, not scalar arithmetic"
        )]
        let local = rotate.inverse() * self.plus_rect.center();
        let reach = ADD_BUTTON_D / 2.0 + 4.0;
        local.x >= chip.x0 - reach
            && local.x <= chip.x1 + reach
            && local.y >= chip.y0 - reach
            && local.y <= chip.y1 + reach
    }

    /// Nearest slot wins a workspace's click target. `slot.idx` is
    /// always in range here (both vecs are sized to `self.spokes.len()`
    /// in [`Self::draw_labels`]), but a `.get_mut()` fallback costs
    /// nothing and can't panic if that invariant ever slips.
    fn claim_hit_rect(&mut self, hit_claim: &mut [f64], slot: &SectorSlot, placed: Rect) {
        if let Some(claim) = hit_claim.get_mut(slot.idx)
            && slot.mid.abs() < *claim
        {
            *claim = slot.mid.abs();
            if let Some(hit_rect) = self.spoke_hit_rects.get_mut(slot.idx) {
                *hit_rect = placed;
            }
        }
    }

    /// Presence dots seated just inside the rim so they ride the
    /// surface with the sector.
    fn draw_presence_dots(&self, ctx: &mut DrawCtx<'_>, geom: &DiscGeometry, slot: &SectorSlot) {
        let Some(ws_id) = self.spokes.get(slot.idx).and_then(|s| s.workspace_id) else {
            return;
        };
        let key = PresenceKey::Workspace {
            id: format!("{ws_id}"),
        };
        let peers = ctx.presence.peers_on(&key);
        if peers.is_empty() {
            return;
        }
        let peer_count = peers.len().min(3);
        let dot_r = geom.r_out - LABEL_RIM_GAP / 2.0;
        let spacing = 7.0_f64 / dot_r;
        let centre_bias = (f64_of_usize(peer_count) - 1.0) / 2.0;
        for (kk, user) in peers.iter().take(peer_count).enumerate() {
            let off = (f64_of_usize(kk) - centre_bias) * spacing;
            let (rr, gg, bb) = ctx.presence.color_for(user);
            // Empty-initials Avatar chip in the peer's own colour,
            // matching the cursor/avatar sites.
            let p = polar(geom.c, dot_r, slot.mid + off);
            let dot_rect = Rect::new(p.x - 3.0, p.y - 3.0, p.x + 3.0, p.y + 3.0);
            ui::widget::AvatarFace::new("", "")
                .presence(Color::from_rgb8(rr, gg, bb))
                .paint(
                    &mut ui::paint::PaintCtx::new(
                        ctx.scene,
                        ctx.transform,
                        ctx.text_renderer,
                        ctx.theme,
                    ),
                    dot_rect,
                );
        }
    }

    /// Pass 4 — the add-workspace button: a round glass button in the
    /// rim's material. Does NOT rotate — it isn't a workspace, it's the
    /// control that makes one, so a spot on the wheel would carry it
    /// out of reach every turn.
    fn draw_add_button(&self, ctx: &mut DrawCtx<'_>, geom: &DiscGeometry) {
        let edge_hue = ctx.theme.tokens.colors.text_primary;
        let edge = |a: u8| Color::from_rgba8(edge_hue.r(), edge_hue.g(), edge_hue.b(), a);
        let add_fade = self.row_fade(self.plus_rect.center().y).max(0.55);
        let add_circle = Circle::new(self.plus_rect.center(), ADD_BUTTON_D / 2.0);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            geom.body_hue.with_alpha_factor(add_fade),
            None,
            &add_circle,
        );
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            edge(f32_to_u8_saturating(74.0 * add_fade)),
            None,
            &add_circle,
        );
        let glyph = Rect::from_center_size(self.plus_rect.center(), (15.0, 15.0));
        Icon::Plus.draw_with_stroke(
            ctx.scene,
            ctx.transform,
            glyph,
            edge_hue.with_alpha_factor(0.72 * add_fade),
            1.6,
        );
    }

    /// `MouseInput { Pressed, Left }`: the add-button, a click onto a
    /// non-active spoke, or the start of a rotation drag — whichever the
    /// press lands on first, in that priority order.
    fn handle_left_press(&mut self, handled: &mut bool) {
        // Plus-slot first — outside the rim, must not be
        // swallowed by the drag-start / spoke-click branches.
        if self.plus_rect.area() > 0.0
            && self
                .last_cursor_pos
                .is_some_and(|p| self.plus_rect.contains(p))
        {
            if let Some(host) = self.host.as_ref() {
                host.add_workspace();
            }
            *handled = true;
            return;
        }

        // A press inside a non-active spoke's cluster animates
        // the wheel to it rather than starting a free drag.
        if let Some(p) = self.last_cursor_pos {
            for (i, hit) in self.spoke_hit_rects.iter().enumerate() {
                if i == self.active_idx {
                    continue;
                }
                if hit.area() > 0.0 && hit.contains(p) {
                    self.click_to_spoke(i);
                    // Picking a workspace answers the question a
                    // summon was called for; it stays up through
                    // a scroll or drag, which are still browsing.
                    self.dismiss_summon();
                    *handled = true;
                    return;
                }
            }
        }

        // Otherwise: start a rotation drag if the press lands
        // inside the visible arc.
        if self.rect.area() <= 0.0 {
            return;
        }
        let anchor = match (self.hover_anchor, self.last_cursor_pos) {
            (Some(h), _) => h,
            (None, Some(p)) => p,
            (None, None) => Point::new(
                f64::midpoint(self.rect.x0, self.rect.x1),
                f64::midpoint(self.rect.y0, self.rect.y1),
            ),
        };
        if self.hit_visible_arc(anchor) {
            // Take the wheel over wherever it visually is, so
            // grabbing a settling wheel catches it mid-flight
            // instead of teleporting to the settle's destination.
            let caught = self.visual_rotation_deg();
            self.rotation_deg = caught;
            self.drag_rotation_origin = caught;
            self.drag_raw_deg = caught;
            if let Some(spring) = self.rotation_spring.as_mut() {
                spring.set_params(SpringPresets::default().tracking);
                spring.set_target(caught);
            }
            self.drag_from = Some(anchor);
            *handled = true;
        }
    }

    /// `CursorMoved`: tracks the pointer always; while dragging, also
    /// re-aims the follow spring at the resisted/magnetised rotation.
    fn handle_cursor_moved(
        &mut self,
        position: &winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor_pos = Some(p);
        if let Some(origin) = self.drag_from {
            let dy = p.y - origin.y;
            let delta_deg = f32_of_f64(dy) * Self::deg_per_pixel();
            // Where the pointer asks the wheel to be, before
            // resistance and detent magnetism have a say.
            let raw = self.drag_rotation_origin + delta_deg;
            self.drag_raw_deg = raw;
            // Both fold into the target rather than the rendered
            // value, so the spring is what the user feels them
            // through.
            let resisted = self.resist_past_ends(raw);
            let from = self.visual_rotation_deg();
            self.rotation_deg = resisted + self.detent_magnet(resisted);
            let target = self.rotation_deg;
            self.aim_follow_spring(from, target);
            self.sync_active_from_visual();
            *handled = true;
        } else if self.hit_visible_arc(p) {
            // Cached for the next press hit-test without
            // claiming pointer capture, reserved for `drag_from`.
            self.hover_anchor = Some(p);
        }
    }

    /// `MouseInput { Released, Left }`: ends a drag, if one was live.
    fn handle_left_release(&mut self, handled: &mut bool) {
        if self.drag_from.is_some() {
            self.drag_from = None;
            self.settle_after_drag();
            *handled = true;
        }
    }

    /// `MouseWheel`: drains the accumulator one detent at a time via
    /// `click_to_spoke`, banking whatever can't be spent as overscroll.
    fn handle_mouse_wheel(&mut self, delta: &winit::event::MouseScrollDelta, handled: &mut bool) {
        // Only consume wheel events when the cursor is over the
        // disc, so scrolls over neighbouring widgets don't
        // accidentally rotate the wheel.
        if !self
            .last_cursor_pos
            .is_some_and(|p| self.hit_visible_arc(p))
        {
            return;
        }
        let dy = match *delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => y * 16.0,
            winit::event::MouseScrollDelta::PixelDelta(p) => f32_of_f64(p.y),
        };
        // Every `WHEEL_DETENT_PX` triggers a workspace advance via
        // `click_to_spoke` (a spring settle, not a teleport); at an
        // end, the unspendable portion resists like a rim-drag.
        self.wheel_accumulator += dy;
        self.wheel_last_event = Some(Instant::now());
        let n = self.spokes.len();
        let n_i32 = i32::try_from(n).ok();
        if n > 1 {
            // `loop` + an explicit break, rather than `while
            // ...abs() >= WHEEL_DETENT_PX`, plus `guard`: a
            // non-finite accumulator (should never happen, but
            // isn't ruled out by the type) must still terminate
            // the drain instead of spinning the compositor.
            let mut guard = 0u32;
            loop {
                if self.wheel_accumulator.abs() < WHEEL_DETENT_PX
                    || guard >= MAX_WHEEL_DETENTS_PER_EVENT
                {
                    break;
                }
                guard = guard.saturating_add(1);
                let neg = self.wheel_accumulator < 0.0;
                let dir: i32 = if neg { 1 } else { -1 };
                // Exact for +-1; avoids a generic `dir as f32` cast.
                let dir_f: f32 = if neg { 1.0 } else { -1.0 };
                // `dir` is opposite-signed to the accumulator, so this
                // drains toward zero; subtracting would hang the loop
                // on any scroll of a full detent.
                self.wheel_accumulator += dir_f * WHEEL_DETENT_PX;
                let target = i32::try_from(self.active_idx)
                    .ok()
                    .and_then(|a| a.checked_add(dir));
                match (target, n_i32) {
                    (Some(target), Some(n_i32)) if (0..n_i32).contains(&target) => {
                        if let Ok(target) = usize::try_from(target) {
                            self.click_to_spoke(target);
                        }
                    }
                    _ => {
                        self.wheel_overscroll_px -= dir_f * WHEEL_DETENT_PX;
                    }
                }
            }
        }
        self.apply_wheel_overscroll();
        *handled = true;
    }
}

impl SidebarWheelDisc {
    /// Scriptable address of the add-workspace button's semantic node.
    const ADD_ACTION_TARGET: &'static str = "wheel:add";

    /// Scriptable address of workspace `idx`'s semantic node — stable
    /// across rotation, unlike where its label happens to be painted.
    fn spoke_action_target(idx: usize) -> String {
        format!("wheel:workspace:{idx}")
    }

    /// Activate semantic node `idx`: a sector (turn the wheel to it, as a
    /// click on the sector does) or, one past the last sector, the add
    /// button. A stale ordinal is refused, never guessed.
    fn activate_node(&mut self, idx: usize) -> bool {
        if idx < self.spokes.len() {
            self.click_to_spoke(idx);
            self.dismiss_summon();
            return true;
        }
        if idx == self.spokes.len() {
            if let Some(host) = self.host.as_ref() {
                host.add_workspace();
            }
            return true;
        }
        false
    }

    /// Slide cached click targets to `target_origin` — used on the pan
    /// fast-path, where the scene replays under a translation instead of
    /// re-encoding, so the absolute-coordinate hit rects would otherwise go stale.
    fn retarget_hit_rects(&mut self, target_origin: Point) {
        let (dx, dy) = offset(self.hit_rects_origin, target_origin);
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        for r in &mut self.spoke_hit_rects {
            if r.area() > 0.0 {
                *r = r.with_origin(Point::new(r.x0 + dx, r.y0 + dy));
            }
        }
        if self.plus_rect.area() > 0.0 {
            self.plus_rect = self
                .plus_rect
                .with_origin(Point::new(self.plus_rect.x0 + dx, self.plus_rect.y0 + dy));
        }
        self.hit_rects_origin = target_origin;
    }

    /// Snap `rotation_deg` to the nearest spoke, exposed so tests can
    /// exercise the snap math without a full winit event sequence. Dragging
    /// down (`rotation_deg > 0`) advances `active_idx` by `-nearest_offset`,
    /// matching "pull down to reveal the next workspace below".
    pub fn snap_to_nearest(&mut self) {
        let idx = self.idx_at_rotation(self.rotation_deg);
        self.commit_to_idx(idx);
    }

    /// Park `idx`'s rotation and write it through to the tree. Idempotent —
    /// running it twice on an unmoved wheel lands on the same workspace.
    fn commit_to_idx(&mut self, idx: usize) {
        let idx = idx.min(self.spokes.len().saturating_sub(1));
        self.rotation_deg = self.rotation_for_idx(idx);
        self.active_idx = idx;
        self.wheel_overscroll_px = 0.0;
        self.publish_active_to_host(idx);
    }

    /// Decide where a released drag ends up. Past an end → back to the end;
    /// moving → project velocity over `fling_projection_ms` and take the
    /// nearest detent (a flick crosses several workspaces); still →
    /// nearest detent. Velocity comes straight off the follow spring — no
    /// separate estimator.
    fn settle_after_drag(&mut self) {
        // Captured before the target moves out from under it.
        let from = self.visual_rotation_deg();
        let (lo, hi) = self.rotation_bounds();
        let velocity = self
            .rotation_spring
            .as_ref()
            .map_or(0.0, ui::Spring::velocity);

        let target = if self.drag_raw_deg > hi {
            hi
        } else if self.drag_raw_deg < lo {
            lo
        } else {
            // Project from where the gesture *ended*, not the trailing rim
            // position — the lag is meant to be visible, not subtracted.
            let coast_s = f32_of_u32(Gesture::default().fling_projection_ms) / 1000.0;
            let projected = self.rotation_deg + velocity * coast_s;
            self.rotation_for_idx(self.idx_at_rotation(projected))
        };

        let idx = self.idx_at_rotation(target);
        self.rotation_deg = target;
        self.wheel_overscroll_px = 0.0;
        // The tree is written exactly once per gesture, here — the
        // indicator's motion during the drag was display only.
        self.publish_active_to_host(idx);
        self.aim_settle_spring(from, target);
        self.sync_active_from_visual();
    }

    /// Same resistance curve a rim-drag gets against an end. Only the
    /// portion of the gesture with nowhere to go pulls: whole detents
    /// that couldn't be spent, plus — once already against the end —
    /// the sub-detent remainder too, since waiting there for a detent
    /// that can never arrive would reinstate the dead zone the detent
    /// model otherwise avoids.
    fn apply_wheel_overscroll(&mut self) {
        let n = self.spokes.len();
        let mut px = self.wheel_overscroll_px;
        if self.active_idx == 0 && self.wheel_accumulator > 0.0 {
            px += self.wheel_accumulator;
        }
        if self.active_idx.saturating_add(1) >= n && self.wheel_accumulator < 0.0 {
            px += self.wheel_accumulator;
        }
        if px == 0.0 {
            return;
        }
        let home = self.rotation_for_idx(self.active_idx);
        let raw = home + px * self.spoke_gap_deg() / WHEEL_DETENT_PX;
        let resisted = self.resist_past_ends(raw);
        let from = self.visual_rotation_deg();
        self.rotation_deg = resisted;
        self.aim_follow_spring(from, resisted);
    }

    /// Animate to `target_idx`. No shortest-path-around-the-back: the
    /// list has ends, so there is exactly one way from here to there,
    /// matching what the sectors show.
    pub fn click_to_spoke(&mut self, target_idx: usize) {
        if target_idx >= self.spokes.len() || target_idx == self.active_idx {
            return;
        }
        let from = self.visual_rotation_deg();
        self.commit_to_idx(target_idx);
        let target = self.rotation_deg;
        self.aim_settle_spring(from, target);
        // Leave the indicator on the wheel, not the destination: `update`
        // re-derives it from the visible rotation every frame, so setting
        // it ahead would flash the target for one frame before snapping
        // back and walking up to it.
        self.sync_active_from_visual();
    }

    /// Split out of [`Widget::update`] so the only thing `update` adds
    /// is reading the wall clock — tests drive this directly with a
    /// fixed dt, still exercising the production path.
    fn advance(&mut self, dt: f32) -> bool {
        let keep = self.tick_rotation_spring(dt);
        self.sync_active_from_visual();
        keep
    }

    /// Drops the spring once settled rather than let the visible
    /// rotation rest fractionally off its detent indefinitely.
    fn tick_rotation_spring(&mut self, dt: f32) -> bool {
        if let Some(spring) = self.rotation_spring.as_mut() {
            if spring.is_settled() {
                self.rotation_spring = None;
                self.last_update_at = None;
                false
            } else {
                spring.step(dt);
                true
            }
        } else {
            false
        }
    }

    /// Replay the current settle from its start and hold it `elapsed`
    /// in, so an offscreen frame can capture a moving wheel. Steps the
    /// same production `Spring` at the same fixed 60 Hz the live loop
    /// uses rather than reconstructing the curve — a still can't show
    /// weight, and rendering costs more wall time than the settle lasts.
    pub fn hold_rotation_at(&mut self, elapsed: Duration) -> bool {
        let Some(origin) = self.settle_origin.clone() else {
            return false;
        };
        let mut s = origin;
        let dt = 1.0 / 60.0;
        // Rust's float-to-int cast saturates; `.max(0.0)` above already
        // rules out the sign-loss case, and this is an offscreen-capture
        // step count, not a value that feeds rendering math elsewhere.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "saturating cast of a non-negative step count"
        )]
        let steps = (elapsed.as_secs_f32() / dt).round().max(0.0) as u32;
        for _ in 0..steps {
            s.step(dt);
        }
        self.rotation_spring = Some(s);
        self.sync_active_from_visual();
        true
    }

    /// The annulus the disc's **labels and add-button** occupy, as
    /// `(inner, outer)` about [`SidebarWheelDisc::disc_centre`]. Not the
    /// extent of the painted surface — the body is a solid segment
    /// reaching the centre; this bounds the readable ring near the rim.
    pub fn label_band_radii(&self) -> (f64, f64) {
        let radius = self.diameter_px / 2.0;
        (radius - ADD_BUTTON_D, radius)
    }

    /// Sits well off-screen left of the footprint, which is what makes
    /// the visible arc a slice of something large.
    pub fn disc_centre(&self) -> Point {
        self.center()
    }

    /// Pins the post-`apply_layout` rect for tests asserting on the
    /// clamped position rather than a transient cache.
    #[cfg(test)]
    pub(crate) fn rect_for_tests(&self) -> Rect {
        self.rect
    }

    /// Replays `draw`'s presence-dot math without touching Vello, so
    /// unit tests can assert dot counts against a `PresenceCtx`.
    #[cfg(test)]
    pub fn presence_dot_hit_count(&self, ctx: &PresenceCtx) -> usize {
        let mut total = 0usize;
        for spoke in &self.spokes {
            let Some(ws_id) = spoke.workspace_id else {
                continue;
            };
            let key = PresenceKey::Workspace {
                id: format!("{ws_id}"),
            };
            let peers = ctx.peers_on(&key);
            if peers.is_empty() {
                continue;
            }
            total = total.saturating_add(peers.len().min(3));
        }
        total
    }

    /// If a host is attached and the active spoke carries a
    /// `workspace_id`, show that workspace through it. A gesture that ends
    /// where it started asks nothing: the host re-syncs everything against
    /// a workspace change.
    fn publish_active_to_host(&self, idx: usize) {
        let Some(host) = self.host.as_ref() else {
            return;
        };
        let Some(id) = self.spokes.get(idx).and_then(|spoke| spoke.workspace_id) else {
            return;
        };
        if host.active_workspace() == Some(WorkspaceId(id)) {
            return;
        }
        host.select_workspace(WorkspaceId(id));
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact wheel gesture endpoints and clamps are the behavior under test"
)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use std::cell::Cell;

    use winit::event::WindowEvent;

    /// A desktop of `workspaces` workspaces numbered from 1, counting
    /// every change the wheel asks of it.
    struct CountingHost {
        workspaces: Cell<usize>,
        active: Cell<u64>,
        changes: Cell<usize>,
        summon: Cell<Option<Point>>,
    }

    impl CountingHost {
        fn new(workspaces: usize) -> Rc<Self> {
            Rc::new(Self {
                workspaces: Cell::new(workspaces),
                active: Cell::new(1),
                changes: Cell::new(0),
                summon: Cell::new(None),
            })
        }
    }

    impl WheelHost for CountingHost {
        fn summon(&self) -> Option<Point> {
            self.summon.get()
        }

        fn dismiss_summon(&self) {
            self.summon.set(None);
        }

        fn active_workspace(&self) -> Option<WorkspaceId> {
            Some(WorkspaceId(self.active.get()))
        }

        fn select_workspace(&self, workspace: WorkspaceId) {
            self.active.set(workspace.0);
            self.changes.set(self.changes.get().saturating_add(1));
        }

        fn add_workspace(&self) {
            let count = self.workspaces.get().saturating_add(1);
            self.workspaces.set(count);
            self.active.set(count as u64);
            self.changes.set(self.changes.get().saturating_add(1));
        }
    }

    fn labels(names: &[&str]) -> Vec<SpokeLabel> {
        names
            .iter()
            .map(|n| SpokeLabel {
                text: (*n).to_string(),
                workspace_id: None,
            })
            .collect()
    }

    fn mk(names: &[&str]) -> SidebarWheelDisc {
        let mut d = SidebarWheelDisc::new(labels(names));
        // Rect spans the disc's full diameter vertically, so the whole
        // visible arc is reachable at `cy = diameter/2`.
        let visible_w = d.visible_width_px();
        d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);
        d
    }

    /// X reliably inside the visible arc at the vertical midline — the
    /// rim's rightmost column.
    fn drag_anchor_x(d: &SidebarWheelDisc) -> f64 {
        d.rect.x1 - 2.0
    }

    use ui::synthetic_input::cursor_moved;

    use ui::synthetic_input::mouse_press;

    /// `lines` is in `LineDelta` units; the widget scales by 16 px.
    fn wheel_event(lines: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, lines),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    use ui::synthetic_input::mouse_release;

    fn pump(d: &mut SidebarWheelDisc, ev: &WindowEvent) -> bool {
        ui::testing::dispatch(d, ev)
    }

    /// `Widget::draw`, which populates the scene cache, the
    /// fingerprint and every cached hit rect.
    fn paint(d: &mut SidebarWheelDisc) {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        d.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// The live shell's slot: narrow and much taller than wide — tests
    /// that care which rows are visible need this, not `mk`'s square rect.
    fn live_slot() -> Rect {
        Rect::new(0.0, 40.0, 234.0, 830.0)
    }

    /// A strip panned by a fraction of a layout pixel (one device pixel at
    /// 150 % is two thirds of one) carries the docked disc exactly as far:
    /// the cached drawing replays where a fresh encode at the new place
    /// would draw, so the disc a retained canvas shows and the one drawn on
    /// screen land on the same pixels, and its click targets follow.
    #[test]
    fn a_fractional_pan_moves_the_cached_disc_exactly_as_far() {
        let spokes = || {
            vec![
                SpokeLabel {
                    text: "Home".into(),
                    workspace_id: None,
                },
                SpokeLabel {
                    text: "Work".into(),
                    workspace_id: None,
                },
            ]
        };
        let mut panned = SidebarWheelDisc::new(spokes());
        layout_into_slot(&mut panned, live_slot());
        paint(&mut panned);
        let before = panned.plus_rect;
        assert!(before.area() > 0.0, "the first paint places the add row");
        let step = 2.0 / 3.0;
        let mut at = live_slot();
        for _ in 0..4 {
            panned.rect.x0 -= step;
            panned.rect.x1 -= step;
            at.x0 -= step;
            at.x1 -= step;
            paint(&mut panned);
            let mut fresh = SidebarWheelDisc::new(spokes());
            layout_into_slot(&mut fresh, at);
            paint(&mut fresh);
            assert!(
                (panned.plus_rect.x0 - fresh.plus_rect.x0).abs() < 1e-9
                    && (panned.plus_rect.y0 - fresh.plus_rect.y0).abs() < 1e-9,
                "the replayed disc's add row {:?} is where a fresh encode puts it, {:?}",
                panned.plus_rect,
                fresh.plus_rect
            );
        }
        assert!(
            (panned.plus_rect.x0 - (before.x0 - 4.0 * step)).abs() < 1e-9,
            "four device-pixel steps move the add row by exactly 8/3 layout pixels"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn sectors_sit_at_their_real_index_with_distinct_names() {
        // Phase `visible_neighbours_carry_a_name_not_just_a_speck`.
        {
            let mut d = mk(&["Home", "Research", "Design"]);
            layout_into_slot(&mut d, live_slot());
            paint(&mut d);

            let any_named = (0..d.spokes.len()).any(|i| {
                i != d.active_idx
                    && d.spoke_hit_rects[i]
                        .width()
                        .max(d.spoke_hit_rects[i].height())
                        >= LABEL_MIN_W
            });
            assert!(
                any_named,
                "no neighbour got a named sector: hit rects {:?}",
                d.spoke_hit_rects
            );
        }

        // Phase `every_sector_name_reads_on_any_wallpaper_in_every_theme`.
        {
            for (name, theme) in [
                ("light", Theme::light()),
                ("dark", Theme::dark()),
                ("high-contrast", Theme::high_contrast()),
            ] {
                // The label is a neutral chip; its plate is drawn over
                // whatever wallpaper sits behind it.
                for wallpaper in [Color::BLACK, Color::WHITE] {
                    for is_active in [false, true] {
                        let chip = ui::widget::ChipStyle::Neutral
                            .colors(&theme, wallpaper, is_active, false);
                        assert_eq!(
                            chip.fill.to_rgba8().a,
                            255,
                            "{name}: the plate must hide whatever wallpaper is behind it"
                        );
                        let ratio = ui::a11y::contrast_ratio(chip.ink, chip.fill);
                        assert!(
                            ratio >= 4.5,
                            "{name} (active: {is_active}): label ink reads {ratio:.2}:1 on its plate"
                        );
                    }
                }
            }
        }

        // Phase `sectors_sit_at_their_real_index_relative_to_the_active_one`.
        {
            // From the middle: one neighbour each side.
            let mut d = mk(&["Home", "Research", "Design"]).with_active_idx(1);
            layout_into_slot(&mut d, live_slot());
            paint(&mut d);
            let cy = f64::midpoint(d.rect.y0, d.rect.y1);
            let placed: Vec<Rect> = d
                .spoke_hit_rects
                .iter()
                .copied()
                .filter(|r| r.area() > 0.0)
                .collect();
            assert_eq!(
                placed.len(),
                3,
                "all three workspaces are on screen: {placed:?}"
            );
            assert!(
                placed.iter().any(|r| r.center().y < cy - 1.0),
                "the workspace before the active one sits above it: {placed:?}"
            );
            assert!(
                placed.iter().any(|r| r.center().y > cy + 1.0),
                "the workspace after the active one sits below it: {placed:?}"
            );

            // From the first workspace: still balanced, because the list
            // wraps — the workspace above Home is the last one.
            let mut d = mk(&["Home", "Research", "Design"]);
            layout_into_slot(&mut d, live_slot());
            paint(&mut d);
            let cy = f64::midpoint(d.rect.y0, d.rect.y1);
            let placed: Vec<Rect> = d
                .spoke_hit_rects
                .iter()
                .copied()
                .filter(|r| r.area() > 0.0)
                .collect();
            assert_eq!(placed.len(), 3, "all three are still on screen: {placed:?}");
            assert!(
                placed.iter().any(|r| r.center().y < cy - 1.0),
                "the wrap has to fill the arc above the first workspace: {placed:?}"
            );

            // Uniqueness on the production slot assignment, not the rects
            // it produces: a rect map is keyed by workspace, so a duplicate
            // would overwrite itself and leave no trace.
            for slots in 1..=6 {
                let assigned: Vec<usize> = d.slot_indices(slots).into_iter().flatten().collect();
                let mut seen = assigned.clone();
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(
                    seen.len(),
                    assigned.len(),
                    "a workspace was given two slots at reach {slots}: {assigned:?}"
                );
                assert_eq!(
                    assigned.len(),
                    d.spokes.len().min(usize::try_from(slots * 2 + 1).unwrap()),
                    "every workspace that fits should get a slot at reach {slots}"
                );
            }
        }

        // Phase `a_sector_keeps_its_name_when_it_only_passes_near_the_add_button`.
        // Driven from the middle workspace so all three sectors are on the
        // band — from the first workspace the last sector is off the band
        // outright, a legitimate reason to have no label that would mask
        // the case this guards.
        {
            let mut d = mk(&["Home", "Research", "Design"]).with_active_idx(1);
            layout_into_slot(&mut d, live_slot());
            paint(&mut d);

            let named = d.spoke_hit_rects.iter().filter(|r| r.area() > 0.0).count();
            assert_eq!(
                named,
                d.spokes.len(),
                "a workspace lost its label with the wheel at rest: {:?}",
                d.spoke_hit_rects
            );
        }

        // Phase `default_workspace_labels_resolve_to_distinct_glyphs`.
        {
            assert_eq!(spoke_icon_for_label("home"), Icon::Home);
            assert_eq!(spoke_icon_for_label("research"), Icon::Search);
            assert_eq!(spoke_icon_for_label("design"), Icon::PenTool);
            // Public API path goes through the same table, lower-casing at
            // the call site.
            let d = mk(&["Home", "Research", "Design"]);
            assert_eq!(d.spoke_icon(0), Icon::Home);
            assert_eq!(d.spoke_icon(1), Icon::Search);
            assert_eq!(d.spoke_icon(2), Icon::PenTool);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn the_disc_fills_its_slot_and_never_loses_a_workspace() {
        // Phase `the_disc_never_empties_and_keeps_every_workspace`.
        {
            // Phase `the_disc_is_never_empty_at_any_rotation`.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                layout_into_slot(&mut d, live_slot());

                let gap = d.spoke_gap_deg();
                // Well past either end, so the rubber band is fully extended.
                let far = gap * 12.0;
                let mut rot = -far;
                // `loop` + an explicit break, rather than `while rot <= far`
                // (same reasoning as the wheel-drain loop below): a non-finite
                // `gap` — should never happen, but isn't ruled out by the type —
                // must still terminate the sweep instead of spinning forever on
                // a float comparison that never resolves.
                loop {
                    if rot > far {
                        break;
                    }
                    d.rotation_deg = d.resist_past_ends(rot);
                    d.rotation_spring = None;
                    d.sync_active_from_visual();
                    paint(&mut d);
                    let named = d.spoke_hit_rects.iter().filter(|r| r.area() > 0.0).count();
                    assert!(
                        named > 0,
                        "the disc went blank at {rot:.1}° (clamped to {:.1}°): {:?}",
                        d.rotation_deg,
                        d.spoke_hit_rects
                    );
                    rot += gap / 4.0;
                }
            }

            // Phase `the_disc_is_never_empty_under_sustained_scrolling`.
            // Same invariant reached via the scroll wheel's separate code path.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                layout_into_slot(&mut d, live_slot());
                // The handler ignores wheel events not over the disc.
                let cx = drag_anchor_x(&d);
                let cy = f64::midpoint(d.rect.y0, d.rect.y1);
                pump(&mut d, &cursor_moved(cx, cy));

                for dir in [-1.0_f32, 1.0] {
                    for _ in 0..60 {
                        pump(&mut d, &wheel_event(dir * 3.0));
                        frames(&mut d, 2);
                        paint(&mut d);
                        let named = d.spoke_hit_rects.iter().filter(|r| r.area() > 0.0).count();
                        assert!(
                            named > 0,
                            "the disc went blank while scrolling: rotation {:.1}°, \
                             residual {:.1}°, active {}",
                            d.rotation_deg,
                            d.visual_residual_deg(),
                            d.active_idx
                        );
                    }
                }
            }

            // Phase `every_workspace_stays_on_the_disc_at_every_size`.
            {
                for w in [140.0_f64, 180.0, 234.0, 320.0] {
                    for h in [400.0_f64, 560.0, 700.0, 790.0, 900.0] {
                        let mut d = mk(&["Home", "Research", "Design"]);
                        layout_into_slot(&mut d, Rect::new(0.0, 40.0, w, 40.0 + h));
                        paint(&mut d);
                        let placed: Vec<Rect> = d
                            .spoke_hit_rects
                            .iter()
                            .copied()
                            .filter(|r| r.area() > 0.0)
                            .collect();
                        assert_eq!(
                            placed.len(),
                            d.spokes.len(),
                            "at {w}x{h} only {} of {} workspaces are on the disc \
                             (chip width {:.1}, names need {LABEL_MIN_W})",
                            placed.len(),
                            d.spokes.len(),
                            SidebarWheelDisc::label_chip_width(&Theme::default())
                        );
                    }
                }
            }
        }

        // Phase `disc_geometry_fills_its_slot_and_only_the_arc_intrudes`.
        {
            // Phase `docked_rail_reaches_both_slot_edges_without_an_inset`.
            // The docked rail owns the complete slot height; its circular profile,
            // not a cropped/faded rectangle, is what makes it taper at the edges.
            {
                let slot = live_slot();
                let mut d = mk(&["Home", "Research", "Design"]);
                layout_into_slot(&mut d, slot);

                assert_eq!(d.rect.y0, slot.y0, "rail must begin at the slot's top edge");
                assert_eq!(d.rect.y1, slot.y1, "rail must reach the slot's bottom edge");
                assert_eq!(d.row_fade(slot.y0), 1.0, "top edge must not fade out");
                assert_eq!(d.row_fade(slot.y1), 1.0, "bottom edge must not fade out");
            }

            // Phase `visible_rail_is_about_thirty_pixels_narrower_at_live_width`.
            // The accepted 96 px wheel moves 30 px left while the circle only gets
            // larger/flatter; shrinking the radius would recreate the short rail.
            {
                let d = mk(&["Home", "Research", "Design"]);
                let accepted_width = 96.0_f64;
                let accepted_radius = 600.0_f64;

                assert!(
                    (accepted_width - d.visible_width_px() - 30.0).abs() < 0.5,
                    "accepted 96px rim should move 30px left; visible width is {}px",
                    d.visible_width_px()
                );
                assert!(
                    d.diameter_px / 2.0 > accepted_radius,
                    "the narrower sliver must come from moving a larger circle left, not shrinking it"
                );
            }

            // Phase `sector_slots_cover_the_whole_sweep`.
            // Display slots must cover the whole visible sweep, not just the
            // workspaces — indexing off the workspace list would leave unsegmented band.
            {
                for sweep_deg in [20.0_f64, 31.0, 38.0, 44.0] {
                    let reach = slot_reach(sweep_deg.to_radians(), f64::from(SPOKE_ANGLE_STEP_DEG));
                    // The fan runs from `-reach` to `+reach` slots, and each slot
                    // spans half a gap either side of its centreline.
                    let covered = f64::from(reach) * f64::from(SPOKE_ANGLE_STEP_DEG)
                        + f64::from(SPOKE_ANGLE_STEP_DEG) / 2.0;
                    assert!(
                        covered >= sweep_deg,
                        "at a {sweep_deg}° sweep the fan only reaches {covered}° — \
                         the rest of the band has no sector on it"
                    );
                }
            }

            // Phase `a_single_workspace_gets_exactly_one_slot`.
            // A one-workspace disc is degenerate: `spoke_gap_deg` widens to a full
            // turn, so the fan must collapse to one slot, not tile copies.
            {
                assert_eq!(slot_reach(40.0_f64.to_radians(), 360.0), 0);
            }

            // Phase `disc_anchors_left_of_viewport_so_only_arc_intrudes`.
            // `cx = rect.x0 - radius * (1 - VISIBLE_RADIUS_FRACTION)`.
            {
                let mut disc = SidebarWheelDisc::new(labels(&["A", "B", "C"]));
                let theme = Theme::default();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let disc_node = disc.build_layout(&mut taffy, &theme, &mut node_map);

                taffy
                    .compute_layout(
                        disc_node,
                        Size {
                            width: taffy::AvailableSpace::Definite(800.0),
                            height: taffy::AvailableSpace::Definite(600.0),
                        },
                    )
                    .unwrap();
                disc.apply_layout(&taffy, disc_node, Point::ORIGIN);
                let rect = disc.rect_for_tests();

                assert!(
                    (rect.width() - disc.visible_width_px()).abs() < 1e-3,
                    "rect width must be the visible-arc sliver ({}px); got {}",
                    disc.visible_width_px(),
                    rect.width()
                );

                let c = disc.center();
                let radius = disc.diameter_px / 2.0;
                assert!(
                    c.x < rect.x0,
                    "disc centre must sit left of the visible footprint; cx={}, rect.x0={}",
                    c.x,
                    rect.x0
                );
                let expected_cx = rect.x0 - radius * (1.0 - VISIBLE_RADIUS_FRACTION);
                assert!(
                    (c.x - expected_cx).abs() < 1e-3,
                    "centre must be `radius * (1 - VISIBLE_RADIUS_FRACTION)` left of \
                     rect.x0; cx={}, expected={}",
                    c.x,
                    expected_cx,
                );
                let rim_right = c.x + radius;
                assert!(
                    rim_right <= rect.x1 + 1e-3,
                    "rightmost rim must land inside the visible footprint; \
                     rim_right={}, rect.x1={}",
                    rim_right,
                    rect.x1
                );
            }

            // Phase `disc_visible_arc_matches_concept_ratio`.
            {
                let disc = mk(&["A", "B", "C"]);
                let c = disc.center();
                let radius = disc.diameter_px / 2.0;
                let mut visible_deg = 0i32;
                for deg in 0..360 {
                    let a = f64::from(deg).to_radians();
                    let x = c.x + radius * a.cos();
                    if x >= disc.rect.x0 - 1e-6 {
                        visible_deg += 1;
                    }
                }
                let expected = 2.0 * (1.0 - VISIBLE_RADIUS_FRACTION).acos().to_degrees();
                let tol = 2.0;
                assert!(
                    (f64::from(visible_deg) - expected).abs() <= tol,
                    "visible arc must be ~{expected:.1}°; counted {visible_deg}°",
                );
                assert!(
                    visible_deg <= 122,
                    "visible arc must not exceed 120°; counted {visible_deg}°",
                );
            }

            // Phase `disc_hit_test_rejects_clicks_left_of_viewport_edge`.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                let c = d.center();
                pump(&mut d, &cursor_moved(c.x, c.y));
                assert!(
                    d.drag_from.is_none(),
                    "off-screen cursor must not seed a drag anchor; drag_from={:?}",
                    d.drag_from
                );
                assert!(
                    !pump(&mut d, &mouse_press()),
                    "press at off-screen-left location must NOT be handled by the disc"
                );
                assert!(
                    d.drag_from.is_none(),
                    "press at off-screen-left location must NOT start a drag"
                );
                assert!(
                    !d.wants_pointer_capture(),
                    "disc must not capture pointer when press lands off-screen"
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn the_add_button_sits_on_the_band_and_adds_a_workspace_at_every_rotation() {
        // Phase `new_starts_with_plus_rect_zero`.
        // Mirrors the `tray_pill_rect` / `avatar_stack_rect` `ZERO`-on-new
        // contract in `TopBarChrome`.
        {
            let d = mk(&["Home", "Research", "Design"]);
            assert_eq!(
                d.plus_rect,
                Rect::ZERO,
                "plus_rect must be ZERO until the first draw pass populates it"
            );
            assert!(d.last_cursor_pos.is_none());
        }

        // Phase `the_add_button_sits_on_the_band`.
        // The add button has to sit ON the band, at every rotation, not
        // hang underneath it on bare wallpaper.
        {
            let mut d = mk(&["Home", "Research", "Design"]);
            layout_into_slot(&mut d, live_slot());

            for step in 0_usize..24 {
                d.rotation_deg = f32_of_usize(step) * 15.0;
                paint(&mut d);
                let (r_in, r_out) = d.label_band_radii();
                let c = d.disc_centre();
                let p = d.plus_rect.center();
                let dist = ((p.x - c.x).powi(2) + (p.y - c.y).powi(2)).sqrt();
                assert!(
                    dist > r_in && dist < r_out,
                    "the add button is off the band at {}°: {dist:.0} px from the centre, \
                     band runs {r_in:.0}..{r_out:.0}",
                    d.rotation_deg
                );
                assert!(
                    d.rect.contains(p),
                    "the add button left the footprint at {}°: {:?} not in {:?}",
                    d.rotation_deg,
                    p,
                    d.rect
                );
            }
        }

        // Phase `the_add_button_stays_pressable_at_every_rotation`.
        // Drives the press path at every position a label passes through, rather
        // than comparing rectangles — the ordering where
        // the button resolves before a sector is the thing that must hold.
        {
            let host = CountingHost::new(1);
            let before = host.workspaces.get();
            let mut d = mk(&["Home", "Research", "Design", "Marketing", "Analytics"])
                .with_host(host.clone());
            layout_into_slot(&mut d, live_slot());

            for step in 0..72 {
                d.rotation_deg = f32_of_usize(step) * 5.0;
                paint(&mut d);
                assert!(
                    d.plus_rect.area() > 0.0,
                    "the add button is placed at {}°",
                    d.rotation_deg
                );

                let expected = before + step + 1;
                let p = d.plus_rect.center();
                pump(&mut d, &cursor_moved(p.x, p.y));
                assert!(
                    pump(&mut d, &mouse_press()),
                    "a press on the add button at {}° was not handled — a label \
                     sweeping over it has taken the press",
                    d.rotation_deg
                );
                assert_eq!(
                    host.workspaces.get(),
                    expected,
                    "the press at {}° did not create a workspace",
                    d.rotation_deg
                );
                pump(&mut d, &mouse_release());
            }
        }

        // Phase `press_on_plus_rect_asks_the_host_to_add_a_workspace`.
        {
            // First boot: one empty workspace — the state where "+"
            // matters most, so it has to work from a single spoke.
            let host = CountingHost::new(1);
            let initial_count = host.workspaces.get();
            let initial_active = host.active_workspace();

            let mut d = SidebarWheelDisc::new(vec![SpokeLabel {
                text: "Home".into(),
                workspace_id: None,
            }])
            .with_host(host.clone());
            layout_into_slot(&mut d, live_slot());
            paint(&mut d);
            assert!(
                d.plus_rect.area() > 0.0,
                "the paint pass must place the add row before it can be clicked"
            );
            assert!(
                d.rect.contains(d.plus_rect.center()),
                "the add row must sit inside the disc's own footprint, not beside it: \
                 tail {:?} footprint {:?}",
                d.plus_rect,
                d.rect
            );
            let slot_center_x = d.plus_rect.center().x;
            let slot_center_y = d.plus_rect.center().y;

            // The press handler hit-tests against the cached
            // `last_cursor_pos`, not the event (which has no position field).
            pump(&mut d, &cursor_moved(slot_center_x, slot_center_y));
            assert!(
                pump(&mut d, &mouse_press()),
                "press on plus slot must be handled"
            );

            assert_eq!(
                host.workspaces.get(),
                initial_count + 1,
                "plus-press must grow the workspace count by exactly 1"
            );
            assert_ne!(
                host.active_workspace(),
                initial_active,
                "plus-press must activate the newly-created workspace"
            );
            // Drag must not start from a plus-slot press — the press path
            // returns early before the drag-anchor block.
            assert!(
                d.drag_from.is_none(),
                "plus-press must not start a rotation drag"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn pointer_input_captures_only_while_pressed_and_reports_dirty_moves() {
        // Phase `pointer_capture_starts_idle_and_ends_on_release`.
        {
            // Phase `new_starts_with_active_idx_zero_and_rotation_zero`.
            {
                let d = mk(&["Home", "Research", "Design"]);
                assert_eq!(d.active_idx, 0);
                assert_eq!(d.rotation_deg, 0.0);
                assert!(d.drag_from.is_none());
                assert!(!d.wants_pointer_capture());
            }

            // Phase `hover_does_not_capture_pointer`.
            // Hover must NOT claim pointer capture — reserved for an
            // actually-in-flight drag.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                let cy = d.diameter_px / 2.0;
                let cx = drag_anchor_x(&d);
                pump(&mut d, &cursor_moved(cx, cy));
                assert!(
                    d.drag_from.is_none(),
                    "hover must not seed `drag_from`; got {:?}",
                    d.drag_from
                );
                assert!(
                    !d.wants_pointer_capture(),
                    "disc must not claim pointer capture on hover-only"
                );
                // Cached for the next press's hit-test, just not via `drag_from`.
                assert!(
                    d.hover_anchor.is_some(),
                    "hover into arc must populate `hover_anchor`"
                );
            }

            // Phase `press_then_release_clears_pointer_capture`.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                let cy = d.diameter_px / 2.0;
                let cx = drag_anchor_x(&d);
                pump(&mut d, &cursor_moved(cx, cy));
                assert!(pump(&mut d, &mouse_press()));
                assert!(
                    d.wants_pointer_capture(),
                    "press inside disc must capture pointer"
                );
                assert!(pump(&mut d, &mouse_release()));
                assert!(
                    d.drag_from.is_none(),
                    "release must clear `drag_from`; got {:?}",
                    d.drag_from
                );
                assert!(
                    !d.wants_pointer_capture(),
                    "release must clear pointer capture"
                );
            }
        }

        // Phase `rim_drags_and_pans_report_dirty_while_pure_pans_hit_the_cache`.
        {
            // Phase `dragging_the_rim_reports_dirty_on_every_move`.
            // Dragging must keep asking for frames — the host skips repaint on a
            // pointer move unless the tree reports dirty.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                let cy = f64::midpoint(d.rect.y0, d.rect.y1);
                let x = drag_anchor_x(&d);
                paint(&mut d);
                assert!(
                    !d.needs_redraw(),
                    "a painted, untouched disc is clean — otherwise this test proves nothing"
                );

                pump(&mut d, &cursor_moved(x, cy));
                assert!(pump(&mut d, &mouse_press()), "press inside the arc grabs");
                let before = d.rotation_deg;

                pump(&mut d, &cursor_moved(x, cy + 60.0));
                assert_ne!(
                    d.rotation_deg, before,
                    "the drag moved the rim, so the next frame would differ"
                );
                assert!(
                    d.needs_redraw(),
                    "rim moved under the pointer but the disc reported clean, so no frame is requested"
                );
                assert!(
                    d.needs_animation_frame(),
                    "an in-flight drag has to hold the redraw pump open"
                );
            }

            // Phase `panning_the_strip_reports_dirty_and_moves_the_click_targets`.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                // Needs the live slot, not `mk`'s full-diameter rect — a label
                // only paints where the band is deep enough to hold one.
                layout_into_slot(&mut d, live_slot());
                paint(&mut d);
                let before: Vec<Rect> = d.spoke_hit_rects.clone();
                assert!(
                    before.iter().any(|r| r.area() > 0.0),
                    "the paint populated at least one label hit rect"
                );

                // What the host does on a pan: same slot, shifted origin.
                let shift = -37.0_f64;
                d.rect = d.rect.with_origin(Point::new(d.rect.x0 + shift, d.rect.y0));
                assert!(
                    d.needs_redraw(),
                    "the disc moved with the canvas, so the frame differs"
                );

                paint(&mut d);
                for (i, (was, now)) in before.iter().zip(d.spoke_hit_rects.iter()).enumerate() {
                    if was.area() <= 0.0 {
                        continue;
                    }
                    assert!(
                        (now.x0 - (was.x0 + shift)).abs() < 1.5,
                        "label {i} click target must travel with the disc: was x0={}, now x0={}, \
                         expected ~{}",
                        was.x0,
                        now.x0,
                        was.x0 + shift
                    );
                }
            }

            // Phase `a_pure_pan_is_a_content_cache_hit`.
            {
                let mut d = mk(&["Home", "Research", "Design"]);
                paint(&mut d);
                let docked = d.current_fingerprint();

                d.rect = d.rect.with_origin(Point::new(d.rect.x0 - 120.0, d.rect.y0));
                let panned = d.current_fingerprint();

                assert_ne!(
                    panned, docked,
                    "the origin moved, so the frame is not identical"
                );
                assert!(
                    panned.content_eq(&docked),
                    "a pan repaints nothing — it must stay a content hit so the scene is replayed, \
                     not re-encoded"
                );
            }
        }
    }

    /// Summoning re-places the disc without resizing — a summon shows
    /// no more of the wheel than the docked state does.
    #[test]
    fn summoning_moves_the_disc_to_the_pointer_at_the_same_size() {
        let mut d = mk(&["Home", "Research", "Design"]);

        let slot = Rect::new(0.0, 40.0, 320.0, 940.0);
        layout_into_slot(&mut d, slot);
        let docked = d.rect;
        assert!(docked.width() > 0.0, "the docked pass produced a footprint");

        let pointer = Point::new(1200.0, 500.0);
        d.set_summon(Some(pointer));
        layout_into_slot(&mut d, slot);

        assert!(
            (d.rect.width() - docked.width()).abs() < 0.5,
            "a summon must not widen the footprint: docked {} vs summoned {}",
            docked.width(),
            d.rect.width()
        );
        assert!(
            (d.rect.height() - docked.height()).abs() < 0.5,
            "a summon must not lengthen the footprint: docked {} vs summoned {}",
            docked.height(),
            d.rect.height()
        );
        assert!(
            (d.rect.x1 - pointer.x).abs() < 1.0,
            "the rim's rightmost point — where the active marker rides — lands on the pointer, \
             got x1={} for pointer x={}",
            d.rect.x1,
            pointer.x
        );
        // The docked rim spans its whole slot, so the vertical centre is the
        // pointer clamped to where the footprint still fits inside the slot.
        let centre_y = f64::midpoint(d.rect.y0, d.rect.y1);
        let half = docked.height() / 2.0;
        let expected_y = pointer
            .y
            .clamp(slot.y0 + half, (slot.y1 - half).max(slot.y0 + half));
        assert!(
            (centre_y - expected_y).abs() < 1.0,
            "the disc centres on the pointer as far as the slot allows, got {centre_y} \
             for pointer y={} (expected {expected_y})",
            pointer.y
        );

        // Near the floor it can't centre without hanging off the slot,
        // so it slides as far as it can and stops flush.
        let low = Point::new(900.0, 930.0);
        d.set_summon(Some(low));
        layout_into_slot(&mut d, slot);
        assert!(
            d.rect.y1 <= slot.y1 + 0.5,
            "a summon near the floor stays inside the slot, got y1={} for slot bottom {}",
            d.rect.y1,
            slot.y1
        );
        assert!(
            (d.rect.height() - docked.height()).abs() < 0.5,
            "clamping slides the disc, it never crops it"
        );

        d.set_summon(None);
        layout_into_slot(&mut d, slot);
        assert_eq!(
            d.rect, docked,
            "dismissing a summon restores the docked footprint"
        );
    }

    /// Real `build_layout` / `apply_layout` against a host slot. The slot may
    /// be hundreds of pixels wide, but the wheel leaf deliberately keeps its
    /// own narrow visible width.
    fn layout_into_slot(d: &mut SidebarWheelDisc, slot: Rect) {
        use std::collections::HashMap;
        use taffy::prelude::TaffyMaxContent;
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = d.build_layout(&mut taffy, &theme, &mut node_map);
        let mut style = taffy.style(node).unwrap().clone();
        style.size = Size {
            width: Dimension::length(f32_of_f64(d.visible_width_px().min(slot.width()))),
            height: Dimension::length(f32_of_f64(slot.height())),
        };
        taffy.set_style(node, style).unwrap();
        taffy
            .compute_layout(node, taffy::Size::MAX_CONTENT)
            .unwrap();
        d.apply_layout(&taffy, node, Point::new(slot.x0, slot.y0));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn drags_and_clicks_settle_on_a_spoke() {
        // Phase `drag_and_release_snaps_to_nearest_spoke`.
        {
            let mut d = mk(&["Home", "Research", "Design"]);
            let gap = d.spoke_gap_deg();
            let diameter = d.diameter_px;
            // Uniform spacing regardless of N — that is the invariant this
            // guards. Asserted against the mapping itself rather than a
            // constant: the gap is derived from the laid-out geometry, so a
            // literal would only pin one window size.
            assert!(gap > 0.0, "degenerate gap {gap}");
            for i in 0..d.spokes.len().saturating_sub(1) {
                let step = d.rotation_for_idx(i) - d.rotation_for_idx(i + 1);
                assert!(
                    (step - gap).abs() < 1e-4,
                    "workspaces {i} and {} are {step}° apart, not {gap}°",
                    i + 1
                );
            }

            let start_y = diameter / 2.0;
            let cx = drag_anchor_x(&d);
            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            assert!(d.drag_from.is_some(), "press inside disc must start drag");
            assert!(d.wants_pointer_capture());

            // `15/0.15 = 100px` is exactly one gap; margin lands cleanly past.
            // Dragged UPWARD: workspace 0 is the first, so pulling down pulls
            // against the top end instead of reaching a neighbour.
            let drag_dy = f64::from(gap / DRAG_FACTOR) + 30.0;
            pump(&mut d, &cursor_moved(cx, start_y - drag_dy));
            assert!(d.rotation_deg < 0.0);

            let before_idx = d.active_idx;
            assert!(pump(&mut d, &mouse_release()));
            assert!(d.drag_from.is_none());
            let ratio = d.rotation_deg / gap;
            assert!(
                (ratio - ratio.round()).abs() < 1e-3,
                "rotation {} must be a multiple of gap {}",
                d.rotation_deg,
                gap
            );
            // `selected_idx` is the gesture's answer. `active_idx` is the
            // indicator, which at this instant is still back where the rim
            // visibly is — it catches up as the settle plays.
            assert_ne!(
                d.selected_idx(),
                before_idx,
                "dragging past one gap must rotate to a new spoke"
            );
            for _ in 0..180 {
                d.tick_rotation_spring(1.0 / 60.0);
                d.sync_active_from_visual();
            }
            assert_eq!(
                d.active_idx,
                d.selected_idx(),
                "once the settle lands, the indicator must agree with the selection"
            );
        }

        // Phase `drag_without_release_keeps_rotation_continuous`.
        {
            let mut d = mk(&["Home", "Research", "Design"]);
            let diameter = d.diameter_px;
            let start_y = diameter / 2.0;
            let cx = drag_anchor_x(&d);
            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));

            let idx_before = d.active_idx;
            // 37 px * DRAG_FACTOR = 5.55° — not a multiple of the 15° gap.
            pump(&mut d, &cursor_moved(cx, start_y + 37.0));
            let gap = d.spoke_gap_deg();
            let ratio = d.rotation_deg / gap;
            assert!(
                (ratio - ratio.round()).abs() > 1e-3,
                "mid-drag rotation {} should NOT be a multiple of gap {}",
                d.rotation_deg,
                gap
            );
            assert_eq!(d.active_idx, idx_before);
            assert!(d.drag_from.is_some());
        }

        // Phase `single_spoke_disc_never_snaps_past_itself`.
        {
            let mut d = mk(&["Only"]);
            assert_eq!(d.spokes.len(), 1);
            let diameter = d.diameter_px;
            let start_y = diameter / 2.0;
            let cx = drag_anchor_x(&d);

            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            // Drag a huge distance in either direction.
            pump(&mut d, &cursor_moved(cx, start_y + 10_000.0));
            assert!(pump(&mut d, &mouse_release()));
            assert_eq!(d.active_idx, 0);

            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            pump(&mut d, &cursor_moved(cx, start_y - 10_000.0));
            assert!(pump(&mut d, &mouse_release()));
            assert_eq!(d.active_idx, 0);
        }

        // Phase `snap_animates_rotation_over_multiple_ticks`.
        {
            // Drag to roughly three-quarters of a gap: far enough that the
            // nearest spoke is the next one, short enough that the wheel is
            // still visibly between two — non-trivial spring animation.
            // Derived from the gap rather than hardcoded, so retuning the
            // geometry keeps the drag in the same relative place.
            let mut d = mk(&["A", "B", "C", "D"]);
            let gap = f64::from(d.spoke_gap_deg());
            assert!(gap > 0.0, "degenerate gap {gap}");
            let drag_px = 0.73 * gap / f64::from(DRAG_FACTOR);
            let diameter = d.diameter_px;
            let start_y = diameter / 2.0;
            let cx = drag_anchor_x(&d);

            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            pump(&mut d, &cursor_moved(cx, start_y - drag_px));
            let pre_snap_deg = d.rotation_deg;
            assert!(pre_snap_deg < 0.0);
            // Let the follow spring actually reach the drag target before
            // releasing — the rim trails the pointer now, so "where the
            // wheel is when you release" only means something once some
            // frames have run.
            for _ in 0..30 {
                d.tick_rotation_spring(1.0 / 60.0);
            }
            let visual_at_release = d.visual_rotation_deg();
            assert!(
                (visual_at_release - pre_snap_deg).abs() < 1e-2,
                "follow spring should have caught up to the drag target {pre_snap_deg} (got {visual_at_release})"
            );
            assert!(pump(&mut d, &mouse_release()));

            // Immediately after release: spring is live and unsettled.
            let spring = d
                .rotation_spring
                .as_ref()
                .expect("release should spawn a snap spring");
            assert!(
                !spring.is_settled(),
                "spring must still be moving on the first frame after release"
            );
            // The spring picks up from where the wheel visibly was, not
            // from the snapped target — otherwise nothing would animate.
            assert!(
                (spring.value() - visual_at_release).abs() < 1e-3,
                "spring should start at the released rotation {} (got {})",
                visual_at_release,
                spring.value()
            );
            // `needs_animation_frame` must light up so `App` keeps pumping
            // redraws while the ease plays out.
            assert!(d.needs_animation_frame());

            // The "heavy" preset is critically damped (ω ≈ 14 rad/s), needing
            // ωt ≈ 7 to land inside `is_settled`'s window; 180 frames (~3s)
            // gives headroom. Deterministic dt via the extracted stepper.
            for _ in 0..180 {
                d.tick_rotation_spring(1.0 / 60.0);
            }

            // `update` drops a settled spring so it stops holding the loop open.
            if let Some(spring) = d.rotation_spring.as_ref() {
                assert!(
                    spring.is_settled(),
                    "spring should be settled after 30 frames"
                );
            }
            assert!(
                !d.needs_animation_frame(),
                "settled spring must stop asking for frames"
            );
            let target = d.rotation_deg;
            let visual = d.rotation_spring.as_ref().map_or(target, ui::Spring::value);
            assert!(
                (visual - target).abs() < 1.0,
                "visual rotation {visual} must be within 1° of target {target}"
            );
        }

        // Phase `click_to_spoke_makes_target_active`.
        {
            let mut d = mk(&["A", "B", "C", "D"]);
            assert_eq!(d.active_idx, 0);
            d.click_to_spoke(2);
            assert_eq!(d.selected_idx(), 2, "the click chose spoke 2");
            let gap = d.spoke_gap_deg();
            let ratio = d.rotation_deg / gap;
            assert!(
                (ratio - ratio.round()).abs() < 1e-3,
                "rotation {} must land on a multiple of gap {}",
                d.rotation_deg,
                gap
            );
            assert!(
                d.rotation_spring.is_some(),
                "click_to_spoke must spawn a snap-spring"
            );
            // The indicator travels there with the wheel instead of
            // teleporting ahead of it.
            for _ in 0..180 {
                d.tick_rotation_spring(1.0 / 60.0);
                d.sync_active_from_visual();
            }
            assert_eq!(d.active_idx, 2, "the indicator lands on the clicked spoke");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn snapping_asks_the_host_only_when_the_workspace_changes() {
        // Phase `snap_shows_the_new_workspace_through_the_host`.
        {
            let host = CountingHost::new(3);
            let ws_ids: Vec<WorkspaceId> = (1..=3).map(WorkspaceId).collect();
            let initial_active = host.active_workspace();
            assert_eq!(initial_active, Some(ws_ids[0]));

            // Spokes carry workspace ids so write-back can map active_idx →
            // WorkspaceId.
            let spokes: Vec<SpokeLabel> = ws_ids
                .iter()
                .enumerate()
                .map(|(i, id)| SpokeLabel {
                    text: format!("WS-{i}"),
                    workspace_id: Some(id.0),
                })
                .collect();
            let mut d = SidebarWheelDisc::new(spokes).with_host(host.clone());
            let visible_w = d.visible_width_px();
            d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);

            // Drag UP: the tree starts on its first workspace, the top end.
            let diameter = d.diameter_px;
            let start_y = diameter / 2.0;
            let cx = drag_anchor_x(&d);
            let drag_dy = f64::from(d.spoke_gap_deg() / DRAG_FACTOR) + 100.0;
            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            pump(&mut d, &cursor_moved(cx, start_y - drag_dy));
            assert!(pump(&mut d, &mouse_release()));

            assert_ne!(
                d.selected_idx(),
                0,
                "a >gap drag must advance past the initial spoke"
            );
            let expected_id = ws_ids[d.selected_idx()];
            assert_eq!(
                host.active_workspace(),
                Some(expected_id),
                "snap_to_nearest must show the new workspace through the host"
            );
            assert_ne!(
                host.active_workspace(),
                initial_active,
                "the host's active workspace must have changed after the snap"
            );
        }

        // Phase `disc_without_host_snaps_without_panicking`.
        {
            let mut d = mk(&["Home", "Research", "Design"]);
            assert!(d.host.is_none());
            let idx_before = d.active_idx;
            let diameter = d.diameter_px;
            let start_y = diameter / 2.0;
            let cx = drag_anchor_x(&d);
            let drag_dy = f64::from(d.spoke_gap_deg() / DRAG_FACTOR) + 100.0;
            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            // Upward — workspace 0 is the top of a finite list.
            pump(&mut d, &cursor_moved(cx, start_y - drag_dy));
            assert!(pump(&mut d, &mouse_release()));
            assert_ne!(d.selected_idx(), idx_before);
        }

        // Phase `a_gesture_that_lands_where_it_started_asks_nothing`.
        // A gesture that ends where it started must not ask the host for a
        // change — the shell re-syncs the whole tree against one.
        {
            let host = CountingHost::new(3);
            let ws_ids: Vec<WorkspaceId> = (1..=3).map(WorkspaceId).collect();
            let spokes: Vec<SpokeLabel> = ws_ids
                .iter()
                .map(|id| SpokeLabel {
                    text: "W".into(),
                    workspace_id: Some(id.0),
                })
                .collect();
            let mut d = SidebarWheelDisc::new(spokes).with_host(host.clone());
            let visible_w = d.visible_width_px();
            d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);

            let before = host.changes.get();
            let cx = drag_anchor_x(&d);
            let start_y = d.diameter_px / 2.0;
            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            // A pull against the top end, which can only come back to where
            // it began.
            drag_to(&mut d, cx, start_y, 400.0, 30);
            assert!(pump(&mut d, &mouse_release()));
            assert_eq!(
                d.selected_idx(),
                0,
                "the pull cannot have selected anything"
            );
            assert_eq!(
                host.changes.get(),
                before,
                "a bounce off the end asked for a change, forcing a full re-sync for a \
                 workspace that never changed"
            );

            // Let the bounce finish first — a drag starting mid-return
            // begins part of a detent from home, so the same pull would no
            // longer reach the next workspace.
            frames(&mut d, 150);
            assert!(
                d.visual_rotation_deg().abs() < 0.1,
                "the bounce has not finished"
            );
            pump(&mut d, &cursor_moved(cx, start_y));
            assert!(pump(&mut d, &mouse_press()));
            // A clear detent and a bit, in gaps rather than pixels — pixel
            // cost per detent depends on the layout geometry.
            let gap = f64::from(d.spoke_gap_deg());
            drag_to(
                &mut d,
                cx,
                start_y,
                -(1.3 * gap / f64::from(DRAG_FACTOR)),
                30,
            );
            assert!(pump(&mut d, &mouse_release()));
            assert_ne!(d.selected_idx(), 0);
            assert_ne!(
                host.changes.get(),
                before,
                "an actual workspace change must still reach the host"
            );
        }
    }

    #[test]
    fn presence_dots_follow_seeded_peers_up_to_three_per_spoke() {
        // Phase `disc_renders_dot_per_seeded_peer_on_workspace`.
        {
            use ui::permissions::UserId;
            use ui::presence::{PresenceCtx, PresenceKey};

            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace {
                id: "77".to_string(),
            };
            ctx.register_presence(&key, UserId("alice".to_string()));
            ctx.register_presence(&key, UserId("bob".to_string()));

            let d = SidebarWheelDisc::new(vec![SpokeLabel {
                text: "Research".to_string(),
                workspace_id: Some(77),
            }]);

            let empty = PresenceCtx::empty();
            assert_eq!(
                d.presence_dot_hit_count(&empty),
                0,
                "baseline with empty presence must not paint any dots"
            );
            assert_eq!(
                d.presence_dot_hit_count(&ctx),
                2,
                "two seeded peers on workspace 77 must yield 2 dots"
            );
        }

        // Phase `disc_caps_presence_dots_at_three_per_spoke`.
        {
            use ui::permissions::UserId;
            use ui::presence::{PresenceCtx, PresenceKey};

            let mut ctx = PresenceCtx::empty();
            let key = PresenceKey::Workspace {
                id: "77".to_string(),
            };
            for name in ["alice", "bob", "charlie", "dave", "eve"] {
                ctx.register_presence(&key, UserId(name.to_string()));
            }

            let d = SidebarWheelDisc::new(vec![SpokeLabel {
                text: "Research".to_string(),
                workspace_id: Some(77),
            }]);

            assert_eq!(
                d.presence_dot_hit_count(&ctx),
                3,
                "5 seeded peers must still cap at 3 dots per spoke"
            );
        }
    }

    // Motion tests: drive `advance` (`update` minus the wall-clock read) at a fixed 60 Hz.

    /// Frames of production motion at 60 Hz.
    fn frames(d: &mut SidebarWheelDisc, n: usize) {
        for _ in 0..n {
            d.advance(1.0 / 60.0);
        }
    }

    /// Drag by `dy` px from the current origin without releasing, then
    /// let `settle_frames` of motion play so the rim catches up.
    fn drag_to(d: &mut SidebarWheelDisc, cx: f64, start_y: f64, dy: f64, settle_frames: usize) {
        pump(d, &cursor_moved(cx, start_y + dy));
        frames(d, settle_frames);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn the_wheel_moves_with_physical_feel_and_resists_at_its_ends() {
        // Phase `the_wheel_steps_settles_trails_and_flicks_with_physical_feel`.
        {
            // Phase `the_indicator_steps_one_workspace_at_a_time_during_a_drag`.
            // Must visit each workspace in turn — never skipping one, never
            // running backwards — and change hands within half a detent of
            // the boundary each time.
            {
                let mut d = mk(&["A", "B", "C", "D", "E"]);
                let gap = d.spoke_gap_deg();
                let cx = drag_anchor_x(&d);
                let start_y = d.diameter_px / 2.0;
                // Start in the middle so there's room to travel without
                // meeting an end.
                pump(&mut d, &cursor_moved(cx, start_y));
                assert!(pump(&mut d, &mouse_press()));

                // One detent is `gap / DRAG_FACTOR` px; walk three in small
                // steps, sampling the indicator continuously.
                let px_per_detent = f64::from(gap / DRAG_FACTOR);
                let mut seen = vec![d.active_idx];
                let mut steps_at = Vec::new();
                let sub_steps = 60;
                for i in 1..=(3 * sub_steps) {
                    let dy = -(px_per_detent * 3.0) * (f64::from(i) / f64::from(3 * sub_steps));
                    drag_to(&mut d, cx, start_y, dy, 2);
                    if *seen.last().expect("seeded") != d.active_idx {
                        steps_at.push((d.active_idx, d.visual_rotation_deg()));
                        seen.push(d.active_idx);
                    }
                }

                assert_eq!(
                    seen,
                    vec![0, 1, 2, 3],
                    "the indicator must walk the list one workspace at a time under the \
                     hand — it went {seen:?}"
                );

                // Each hand-off happens as the wheel crosses the boundary
                // between the two, i.e. half a detent from either's home.
                for (idx, rot) in &steps_at {
                    let boundary = -(f32_of_usize(*idx) - 0.5) * gap;
                    assert!(
                        (rot - boundary).abs() < gap * 0.15,
                        "the indicator changed to {idx} at {rot:.2}°, but the boundary \
                         between {} and {idx} is at {boundary:.2}°",
                        idx - 1
                    );
                }
            }

            // Phase `the_settle_is_slow_enough_to_feel_and_never_overshoots`.
            // The settle must never cross its target (overshoot reads light)
            // and must take visible but bounded time to arrive.
            {
                let mut d = mk(&["A", "B", "C", "D"]);
                let cx = drag_anchor_x(&d);
                let start_y = d.diameter_px / 2.0;
                pump(&mut d, &cursor_moved(cx, start_y));
                assert!(pump(&mut d, &mouse_press()));
                // Just past halfway, then release — a plain one-detent settle
                // with no fling. Stated as a fraction of the gap so retuning
                // the geometry can't turn "past halfway" into two detents.
                let gap = f64::from(d.spoke_gap_deg());
                drag_to(
                    &mut d,
                    cx,
                    start_y,
                    -(0.6 * gap / f64::from(DRAG_FACTOR)),
                    40,
                );
                assert!(pump(&mut d, &mouse_release()));

                let target = d.rotation_deg;
                let start = d.visual_rotation_deg();
                assert!(
                    f64::from((target - start).abs()) > gap * 0.1,
                    "nothing to settle: start {start:.2}° target {target:.2}° \
                     (gap {gap:.2}°)"
                );
                let travel = target - start;

                let mut frames_taken = 0usize;
                let mut worst_overshoot = 0.0f32;
                for i in 1..=600 {
                    d.advance(1.0 / 60.0);
                    // Progress past 1.0 is overshoot, in units of the trip.
                    let progress = (d.visual_rotation_deg() - start) / travel;
                    worst_overshoot = worst_overshoot.max(progress - 1.0);
                    if d.rotation_spring.is_none() {
                        frames_taken = i;
                        break;
                    }
                }

                assert!(frames_taken > 0, "the settle never finished in 10 s");
                assert!(
                    worst_overshoot < 0.005,
                    "the settle overshot its target by {:.1}% of the trip — overshoot \
                     reads light and bouncy, which is the opposite of the intent",
                    worst_overshoot * 100.0
                );

                let ms = f32_of_usize(frames_taken) * 1000.0 / 60.0;
                assert!(
                    (250.0..900.0).contains(&ms),
                    "the settle took {ms:.0} ms. Under ~250 ms it snaps and the weight is \
                     gone; over ~900 ms the control is not heavy, it is slow."
                );
            }

            // Phase `the_rim_trails_a_fast_drag_further_than_a_slow_one`.
            // The rim trails the pointer instead of being welded to it, and
            // the gap grows with speed — rigid tracking reads weightless.
            {
                let lag_for = |gaps_per_frame: f64| -> f32 {
                    let names: Vec<String> = (0..16).map(|i| format!("W{i}")).collect();
                    let refs: Vec<&str> = names.iter().map(std::string::String::as_str).collect();
                    let mut d = mk(&refs);
                    let px_per_frame =
                        gaps_per_frame * f64::from(d.spoke_gap_deg()) / f64::from(DRAG_FACTOR);
                    let cx = drag_anchor_x(&d);
                    let start_y = d.diameter_px / 2.0;
                    pump(&mut d, &cursor_moved(cx, start_y));
                    assert!(pump(&mut d, &mouse_press()));
                    // Long enough for the follow spring to reach steady state.
                    let mut dy = 0.0;
                    for _ in 0..40 {
                        dy -= px_per_frame;
                        pump(&mut d, &cursor_moved(cx, start_y + dy));
                        d.advance(1.0 / 60.0);
                    }
                    (d.rotation_deg - d.visual_rotation_deg()).abs()
                };

                let slow = lag_for(0.02);
                let fast = lag_for(0.14);
                assert!(
                    slow > 0.0,
                    "the rim tracked the pointer exactly — with no lag at all there is \
                     no mass to read"
                );
                assert!(
                    fast > slow * 2.0,
                    "lag barely changed with speed ({slow:.2}° slow vs {fast:.2}° fast). \
                     Inertia shows up as a gap that opens when you move quickly."
                );
                assert!(
                    fast < 15.0,
                    "the rim trails a fast drag by {fast:.1}°, a whole detent — past a \
                     point lag stops reading as weight and starts reading as latency"
                );
            }

            // Phase `a_flick_carries_past_the_workspace_a_slow_release_settles_on`.
            // From the same release position, a flick must land further down
            // the list than a slow let-go.
            {
                let land = |gaps_per_frame: f64, pause_frames: usize| -> usize {
                    let mut d = mk(&["A", "B", "C", "D", "E", "F", "G", "H"]);
                    let px_per_frame =
                        gaps_per_frame * f64::from(d.spoke_gap_deg()) / f64::from(DRAG_FACTOR);
                    let cx = drag_anchor_x(&d);
                    let start_y = d.diameter_px / 2.0;
                    pump(&mut d, &cursor_moved(cx, start_y));
                    assert!(pump(&mut d, &mouse_press()));
                    let mut dy = 0.0;
                    for _ in 0..24 {
                        dy -= px_per_frame;
                        pump(&mut d, &cursor_moved(cx, start_y + dy));
                        d.advance(1.0 / 60.0);
                    }
                    // A pause before release bleeds the velocity off, which is
                    // what separates "thrown" from "placed".
                    frames(&mut d, pause_frames);
                    assert!(pump(&mut d, &mouse_release()));
                    d.selected_idx()
                };

                let flicked = land(0.12, 0);
                let placed = land(0.12, 45);
                assert!(
                    flicked > placed,
                    "a flick landed on workspace {flicked} and a slow release on {placed} \
                     — releasing while still moving has to carry further, or the wheel \
                     has no momentum"
                );
            }
        }

        // Phase `the_list_ends_resist_and_release_without_wrapping`.
        {
            // Phase `pulling_past_the_first_workspace_resists_and_returns_to_it`.
            // Pulling past the first workspace has to give, resist more the
            // further it's pulled, stay bounded, and return to the end itself.
            {
                let mut d = mk(&["A", "B", "C"]);
                let cx = drag_anchor_x(&d);
                let start_y = d.diameter_px / 2.0;
                pump(&mut d, &cursor_moved(cx, start_y));
                assert!(pump(&mut d, &mouse_press()));

                // Pulls are evenly spaced so successive gains are directly
                // comparable — uneven steps wouldn't test "each extra 100 px
                // buys less than the last did".
                let mut last_gain = f32::INFINITY;
                let mut last = 0.0f32;
                let reach = SidebarWheelDisc::overscroll_reach_deg();
                for pull_px in (1..=12).map(|i| f64::from(i) * 100.0) {
                    drag_to(&mut d, cx, start_y, pull_px, 30);
                    let at = d.rotation_deg;
                    assert!(
                        at > last,
                        "the end stopped dead at {pull_px} px — it should always give a \
                         little more, never nothing"
                    );
                    assert!(
                        at < reach,
                        "overscroll reached {at:.1}°, past its {reach:.1}° ceiling — the \
                         wheel can be dragged somewhere it does not belong"
                    );
                    let raw = f32_of_f64(pull_px) * DRAG_FACTOR;
                    assert!(
                        at < raw,
                        "pulling {pull_px} px past the end moved the wheel {at:.1}° of the \
                         {raw:.1}° asked for — that is no resistance at all"
                    );
                    let gain = at - last;
                    assert!(
                        gain < last_gain,
                        "resistance did not grow between pulls (gain {gain:.2}° after \
                         {last_gain:.2}°)"
                    );
                    last_gain = gain;
                    last = at;
                    assert_eq!(d.active_idx, 0, "overscroll must not select a workspace");
                }

                drag_to(&mut d, cx, start_y, 20_000.0, 30);
                assert!(
                    d.rotation_deg > last && d.rotation_deg < reach,
                    "a huge pull landed at {:.2}°, outside ({last:.2}°, {reach:.2}°)",
                    d.rotation_deg
                );

                assert!(pump(&mut d, &mouse_release()));
                assert_eq!(
                    d.rotation_deg, 0.0,
                    "release past an end must return to the end itself, not to a detent \
                     beyond it"
                );
                assert_eq!(d.selected_idx(), 0);
                frames(&mut d, 200);
                assert!(
                    d.visual_rotation_deg().abs() < 0.1,
                    "the wheel did not come back to rest at the end ({:.2}°)",
                    d.visual_rotation_deg()
                );
            }

            // Phase `the_last_workspace_is_an_end_and_the_list_does_not_wrap`.
            {
                let mut d = mk(&["A", "B", "C"]);
                let cx = drag_anchor_x(&d);
                let start_y = d.diameter_px / 2.0;
                pump(&mut d, &cursor_moved(cx, start_y));
                assert!(pump(&mut d, &mouse_press()));
                drag_to(&mut d, cx, start_y, -10_000.0, 60);
                assert_eq!(
                    d.active_idx, 2,
                    "dragging past the last workspace wrapped round to the start"
                );
                assert!(pump(&mut d, &mouse_release()));
                assert_eq!(d.selected_idx(), 2);
            }

            // Phase `scrolling_into_an_end_pulls_against_it_and_lets_go`.
            // Scroll input has to meet the same edge the drag does, or the two
            // ways into the control feel like different controls.
            {
                let mut d = mk(&["A", "B", "C"]);
                let cx = drag_anchor_x(&d);
                let mid_y = d.diameter_px / 2.0;
                pump(&mut d, &cursor_moved(cx, mid_y));

                for _ in 0..6 {
                    pump(&mut d, &wheel_event(4.0));
                }
                frames(&mut d, 20);
                assert_eq!(d.active_idx, 0, "there is no workspace above the first");
                let pulled = d.rotation_deg;
                assert!(
                    pulled > 0.5,
                    "scrolling into the end moved the wheel {pulled:.2}° — with no give \
                     at all the control reads as dead exactly where it most needs to \
                     answer"
                );
                assert!(
                    pulled < SidebarWheelDisc::overscroll_reach_deg(),
                    "scroll overscroll {pulled:.1}° passed its ceiling"
                );

                // Idle is the scroll wheel's version of lifting a finger.
                std::thread::sleep(Duration::from_millis(WHEEL_SCROLL_TIMEOUT_MS + 60));
                let st = AutoCommit::new();
                let obj = automerge::ROOT.clone();
                d.update(&st, &obj);
                assert_eq!(
                    d.rotation_deg, 0.0,
                    "the pull never let go once the gesture ended"
                );
                frames(&mut d, 200);
                assert!(d.visual_rotation_deg().abs() < 0.1);
            }

            // Phase `scrolling_mid_list_still_takes_three_clicks_per_workspace`.
            // The overscroll work must not have quietly turned scroll into a
            // continuous drag: still three clicks per workspace.
            {
                let mut d = mk(&["A", "B", "C", "D", "E"]);
                let cx = drag_anchor_x(&d);
                let mid_y = d.diameter_px / 2.0;
                pump(&mut d, &cursor_moved(cx, mid_y));
                // One wheel click is 16 px; the detent is 48.
                for click in 1..=3 {
                    pump(&mut d, &wheel_event(-1.0));
                    let expected = usize::from(click == 3);
                    assert_eq!(
                        d.selected_idx(),
                        expected,
                        "after {click} wheel click(s) the wheel should be on workspace \
                         {expected}"
                    );
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn sector_labels_step_down_then_elide_at_the_persons_text_size() {
        // Phase `the_active_workspace_name_is_never_truncated_to_fit_its_size_step`.
        // The active chip must never truncate the name to keep its size step.
        {
            let mut d = mk(&["Home", "Research", "Design"]);
            layout_into_slot(&mut d, live_slot());
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let chip_w = SidebarWheelDisc::label_chip_width(&theme);
            assert!(chip_w > LABEL_MIN_W, "band too shallow to test: {chip_w}");

            for (idx, name) in ["Home", "Research", "Design"].iter().enumerate() {
                let h = d.active_chip_height(idx, chip_w, &theme, &mut tr);
                assert!(
                    (SECTOR_CHIP_H..=SECTOR_CHIP_H_ACTIVE).contains(&h),
                    "{name}: chip height {h} outside the family's two sizes"
                );
                let need = ChipFace::intrinsic_width(
                    name,
                    ChipLead::Glyph(d.spoke_icon(idx)),
                    false,
                    &mut tr,
                    &theme,
                );
                assert!(
                    need <= chip_w,
                    "{name} needs {need:.0} px at the chosen {h} px step but the band \
                     only has {chip_w:.0} px — it would be painted with an ellipsis"
                );
            }

            // A short name still gets the full step.
            assert_eq!(
                d.active_chip_height(0, chip_w, &theme, &mut tr),
                SECTOR_CHIP_H_ACTIVE,
                "`Home` fits at the full step and must keep it"
            );
        }

        // Phase `label_metrics_sit_on_the_spacing_scale`.
        // The rim's label gap is the spacing scale under another name; pinned
        // so a later edit cannot move it off the grid unnoticed. Chip heights
        // are the compact chip type scale, not spacing.
        {
            let spacing = Theme::default().tokens.spacing;
            assert_eq!(LABEL_RIM_GAP, f64::from(spacing.s3));
        }

        // Phase `large_text_steps_a_chip_down_before_it_truncates_a_name`.
        // At 200 % text the chip's label grows with it, and at the family's
        // two fixed heights "Home" was painted as "H…". A name steps down to
        // a smaller chip — never below the shared Chip's resting height,
        // where the label is exactly the person's chosen size — before it
        // loses letters. Both the active and the resting size are covered.
        {
            let mut d = mk(&["Home", "Research", "Design"]);
            layout_into_slot(&mut d, live_slot());
            let mut tr = TextRenderer::new();
            let theme = Theme::dark().with_text_scale(2.0);
            let chip_w = SidebarWheelDisc::label_chip_width(&theme);
            assert!(chip_w > LABEL_MIN_W, "band too shallow to test: {chip_w}");

            let inactive_h = d.inactive_chip_height(chip_w, &theme, &mut tr);
            for (idx, name) in ["Home", "Research", "Design"].iter().enumerate() {
                let active_h = d.active_chip_height(idx, chip_w, &theme, &mut tr);
                for h in [active_h, inactive_h] {
                    assert!(
                        (SECTOR_CHIP_H_MIN..=SECTOR_CHIP_H_ACTIVE).contains(&h),
                        "{name}: chip height {h} outside [{SECTOR_CHIP_H_MIN}, {SECTOR_CHIP_H_ACTIVE}]"
                    );
                    let need = ChipFace::intrinsic_width(
                        name,
                        ChipLead::Glyph(d.spoke_icon(idx)),
                        false,
                        &mut tr,
                        &theme,
                    );
                    assert!(
                        need <= chip_w,
                        "{name} at 200% text needs {need:.0} px at the {h} px step but the band \
                         only has {chip_w:.0} px — it would be painted with an ellipsis"
                    );
                }
            }
            // The resting ring is one height, chosen for the longest name.
            assert!(
                inactive_h <= SECTOR_CHIP_H,
                "resting chips never exceed the family size: {inactive_h}"
            );
        }

        // Phase `a_name_that_fits_nowhere_is_elided_at_the_persons_text_size`.
        // A name that fits at no step is elided at the floor — the person's
        // own text size, showing the most letters — and the resting ring
        // steps down with it as one, never a lone small chip beside full-size
        // neighbours.
        {
            let mut d = mk(&[
                "Home",
                "Quarterly planning retrospective for the platform group",
            ]);
            layout_into_slot(&mut d, live_slot());
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let chip_w = SidebarWheelDisc::label_chip_width(&theme);
            assert_eq!(
                d.chip_height_for(1, chip_w, SECTOR_CHIP_H, &theme, &mut tr),
                None,
                "the fixture name must overflow every step for this test to mean anything"
            );
            assert_eq!(
                d.active_chip_height(1, chip_w, &theme, &mut tr),
                SECTOR_CHIP_H_MIN
            );
            assert_eq!(
                d.inactive_chip_height(chip_w, &theme, &mut tr),
                SECTOR_CHIP_H_MIN
            );
        }

        // Phase `a_shallow_slot_at_large_text_elides_at_the_floor_not_the_family_size`.
        // The live desktop's disc slot at 1280×800 is 220 px wide and 343 px
        // tall — far shallower than `live_slot()` — and at 200 % text not even
        // the floor holds "Home". The label must then be elided at the floor
        // (a 24 px "Hom…"), not at the family size (a 38 px "H…").
        {
            fn phase() {
                let mut d = mk(&["Home", "Research", "Design"]);
                layout_into_slot(&mut d, Rect::new(0.0, 168.0, 220.0, 511.0));
                let mut tr = TextRenderer::new();
                let theme = Theme::dark().with_text_scale(2.0);
                let chip_w = SidebarWheelDisc::label_chip_width(&theme);
                let fits_anywhere = (0..3).any(|idx| {
                    d.chip_height_for(idx, chip_w, SECTOR_CHIP_H_ACTIVE, &theme, &mut tr)
                        .is_some()
                });
                if fits_anywhere {
                    // A wider band than expected: then the names simply fit, which
                    // is the better outcome and the other test covers it.
                    return;
                }
                for idx in 0..3 {
                    assert_eq!(
                        d.active_chip_height(idx, chip_w, &theme, &mut tr),
                        SECTOR_CHIP_H_MIN,
                        "spoke {idx} must be elided at the person's size, not the family size"
                    );
                }
                assert_eq!(
                    d.inactive_chip_height(chip_w, &theme, &mut tr),
                    SECTOR_CHIP_H_MIN
                );
            }
            phase();
        }
    }

    /// Every workspace on the disc is a named, bounded semantic target,
    /// as is the add button, and activating one turns the wheel exactly
    /// as a click on its sector does — so a script or a screen reader
    /// never has to work out where a rotated label landed.
    #[test]
    fn every_sector_is_a_bounded_semantic_target_that_activates() {
        use ui::a11y::{Action, Role};
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());
        paint(&mut d);

        let node = d.accessibility().expect("the disc publishes a tree");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.children.len(), d.spokes.len() + 1);
        for (idx, spoke) in d.spokes.iter().enumerate() {
            let child = &node.children[idx];
            assert_eq!(
                child.name.as_deref(),
                Some(format!("Switch to {}", spoke.text).as_str())
            );
            assert_ne!(
                child.name.as_deref(),
                Some(spoke.text.as_str()),
                "a sector must not publish the bare workspace name — the overview's row does"
            );
            let placed = d.spoke_hit_rects[idx];
            assert!(
                placed.area() > 0.0,
                "spoke {idx} is on the band at the live slot"
            );
            assert_eq!(
                child.bounds.map(ui::AccessBounds::to_rect),
                Some(placed),
                "spoke {idx}'s semantic bounds must be the rect its label was painted at"
            );
            assert_eq!(child.state.selected, idx == d.active_idx);
            assert!(!child.state.hidden);
        }
        let add = node
            .children
            .last()
            .expect("the add button is the trailing child");
        assert_eq!(add.name.as_deref(), Some("Add a workspace"));
        assert_eq!(add.bounds.map(ui::AccessBounds::to_rect), Some(d.plus_rect));

        // The dispatcher hands a node's `action_target` to
        // `perform_semantic_action`; the click fallback asks
        // `synthetic_accessibility_rect` where the node is.
        assert_eq!(
            d.synthetic_accessibility_rect(&[2]),
            Some(d.spoke_hit_rects[2]),
            "the routed geometry for sector 2 is its painted rect"
        );
        assert_eq!(d.synthetic_accessibility_rect(&[3]), Some(d.plus_rect));
        let mut doc = automerge::AutoCommit::new();
        let path = automerge::ROOT.clone();
        assert!(d.perform_semantic_action(
            "wheel:workspace:2",
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &path
        ));
        assert_eq!(
            d.selected_idx(),
            2,
            "activating the third sector chooses it"
        );
        assert!(
            !d.perform_semantic_action(
                "wheel:workspace:9",
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &path
            ),
            "a stale address is refused, never guessed"
        );
        assert!(
            !d.perform_action(
                &Action::Activate,
                &[9],
                &mut ui::widget::Edits::lend(&mut doc),
                &path
            ),
            "a stale key is refused, never guessed"
        );
        assert!(d.perform_action(
            &Action::Activate,
            &[1],
            &mut ui::widget::Edits::lend(&mut doc),
            &path
        ));
        assert_eq!(
            d.selected_idx(),
            1,
            "the key-path route reaches the same sectors"
        );
    }
}

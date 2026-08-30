//! `SidebarWheelDisc` — radial workspace switcher docked off-screen-left,
//! each workspace a spoke; drag vertically to rotate, release snaps to the
//! nearest.
//!
//! `rotation_deg` is absolute (workspace `i` rests at `-i * spoke_gap_deg()`);
//! `draw` uses the offset from the active sector's rest position instead
//! ([`SidebarWheelDisc::visual_residual_deg`]), so the lit sector doesn't
//! march down the arc as you switch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{AlignItems, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, ColorStop, Compose, Extend, Fill, Gradient, Mix};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::animation::Spring;
use ui::icons::Icon;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::{Gesture, SpringPresets, rubber_band};
use ui::widget::{Chip, ChipLead, ChipStyle, Widget};

use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceId;

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
    /// Disc diameter in logical pixels; recomputed from the host slot on
    /// layout so the visible arc-fraction stays fixed.
    pub diameter_px: f64,
    /// When present, a snap that lands on a spoke with a known
    /// `workspace_id` writes the new active workspace to `chrome.tree`.
    pub chrome: Option<Rc<RefCell<ChromeState>>>,
    /// `Some(spring)` while a post-release snap is easing the rim from
    /// the pre-snap rotation to the snapped target.
    pub rotation_spring: Option<Spring>,
    /// Previous `update` tick, used to feed the spring the *real* frame
    /// dt. Under frame drops a fixed dt made the spring run in
    /// slow-motion.
    pub last_update_at: Option<std::time::Instant>,
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
    /// Whole-pixel origin the cached hit rects currently sit at, advanced
    /// by [`SidebarWheelDisc::retarget_hit_rects`] on each pan frame that
    /// takes the translate fast path, so the shift is applied once per
    /// move rather than re-applied cumulatively.
    hit_rects_origin_px: (i64, i64),
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
    /// Top-left of the footprint. Split out from `size_px` because a
    /// plain strip pan changes only this, not a single painted pixel —
    /// letting `draw` replay the cached scene under a translation
    /// instead of re-encoding it. See [`WheelCacheFp::content_eq`].
    origin_px: (i64, i64),
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

    /// A zero-width footprint means the disc has not laid out yet.
    fn is_populated(&self) -> bool {
        self.size_px.0 > 0
    }
}

/// Fraction of the disc's radius that protrudes into the viewport:
/// `cx = rect.x0 - radius * (1 - VISIBLE)`. Does not change the
/// footprint — only how tightly the visible sliver curves.
pub(crate) const VISIBLE_RADIUS_FRACTION: f64 = 0.14;

/// Vertical inset applied top/bottom to the taffy slot, so the footprint
/// reads as inset rather than edge-to-edge. Trades height for band
/// depth (the band bends back toward centre to fit a tall narrow slot):
/// at 0.10 the band came out 129 px, too shallow for a workspace name.
pub(crate) const SLOT_VERTICAL_INSET_FRAC: f64 = 0.16;

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

/// Where the disc's vertical fade reaches full strength, as a fraction
/// of the footprint's height (mirrored at the bottom). Must stay above
/// `1 - BAND_TANGENT_FRACTION` so the inner arc's clip is already
/// fading by the time the clip bites.
const FADE_IN_STOP: f32 = 0.13;

/// How far past the footprint's right edge the rim's shadow may reach;
/// the paint group is clipped this much wider so the shadow can sit
/// outside the rim and still be faded with everything else.
const RIM_SHADOW_REACH: f64 = 26.0;

/// Clear space between the footprint's left edge and the band's inner
/// arc, so the band visibly ends on its own curve rather than the clip.
const BAND_LEFT_INSET: f64 = 12.0;

/// How close the band's inner arc may reach the left edge (fraction of half
/// the footprint height) — trades band depth for height; at the extreme
/// it's too shallow for a name (1600x900: 109px, "Research" truncates to
/// "Res…"). Read with [`FADE_IN_STOP`], which fades the drift into the clip.
const BAND_TANGENT_FRACTION: f64 = 0.93;

/// Clearance either side of a sector's label chip. Constant for every
/// sector since the band has a constant radial depth, so labels form a
/// ring rather than a ragged column.
const LABEL_RIM_GAP: f64 = 12.0;
const LABEL_INNER_INSET: f64 = 12.0;

/// Narrowest a chip may get before the sector shows no label at all —
/// below this even a truncated name says less than an empty sector does.
const LABEL_MIN_W: f64 = 62.0;

/// Height of an inactive sector's chip; the active sector's is
/// [`SECTOR_CHIP_H_ACTIVE`], one step larger. Sized against the slot: it
/// caps near 330 px so three fit the screen at once, and widening the
/// slot to fit a bigger chip would push the third workspace off screen.
const SECTOR_CHIP_H: f64 = 48.0;

const SECTOR_CHIP_H_ACTIVE: f64 = 64.0;

/// Fill strength of the active sector's wedge, as a factor on the
/// workspace's colour — enough to be unmistakably lit, quiet enough
/// that the name on top stays the brightest thing in the sector.
const ACTIVE_WEDGE_ALPHA: f32 = 0.17;

/// The same fill where the body beneath is fully opaque: 17% is barely
/// a colour on the high-contrast track's opaque surface, so this is
/// interpolated on the body's own opacity rather than switched per
/// theme.
const ACTIVE_WEDGE_ALPHA_ON_OPAQUE: f32 = 0.55;

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

    /// Fresh disc at rest. `diameter_px` starts at a placeholder,
    /// recomputed from the host slot on first layout.
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
            diameter_px: 10929.0,
            chrome: None,
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
            hit_rects_origin_px: (0, 0),
            summon_at: None,
        }
    }

    /// Summon the disc to `at`, or return it to the strip's left edge
    /// with `None`. Keeps its footprint and geometry; only where it
    /// sits changes.
    pub fn set_summon(&mut self, at: Option<Point>) {
        self.summon_at = at;
    }

    pub fn is_summoned(&self) -> bool {
        self.summon_at.is_some()
    }

    /// Send a summoned disc back to its dock, clearing the shared
    /// chrome flag so the overlay host re-anchors it on the next layout.
    pub fn dismiss_summon(&mut self) {
        self.summon_at = None;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().set_wheel_summon(None);
        }
    }

    /// After a snap commits a new `active_idx`, the disc writes it back
    /// to `chrome.tree` via `set_active` (mirrors `SidebarWheelChrome`).
    #[must_use]
    pub fn with_chrome(mut self, chrome: Rc<RefCell<ChromeState>>) -> Self {
        self.chrome = Some(chrome);
        self
    }

    /// Override the disc diameter — tests use this to validate layout
    /// math without depending on the placeholder default.
    #[must_use]
    pub fn with_diameter(mut self, d: f64) -> Self {
        self.diameter_px = d;
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
    /// [`SECTORS_ACROSS_BAND`] sectors, then widens each by
    /// [`SECTOR_SIZE_BOOST`].
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
        let gap = span / usize_to_f32_lossy(n.min(SECTORS_ACROSS_BAND)) * SECTOR_SIZE_BOOST;
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
        -usize_to_f32_lossy(idx) * self.spoke_gap_deg()
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
        let clamped = steps.clamp(0.0, usize_to_f32_lossy(last));
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

    /// Theming belongs to the user, so this isn't a hardcoded palette —
    /// [`ui::tokens::Colors::categorical`] grows the series from the
    /// theme's own accent, keeping adjacent workspaces 60° of hue apart.
    ///
    /// Doesn't read `self` — kept as an associated fn (`Self::spoke_color(..)`)
    /// so it reads next to the other per-spoke painting helpers.
    fn spoke_color(idx: usize, colors: &ui::tokens::Colors) -> Color {
        colors.categorical(idx)
    }

    /// Largest chip size step the name fits in. The chip can't widen inward
    /// (it would swing through the footprint's left edge off-centre), so a
    /// long name steps down a size instead of truncating; falls back to
    /// [`SECTOR_CHIP_H`] if even that overflows.
    fn active_chip_height(
        &self,
        idx: usize,
        chip_w: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> f64 {
        let Some(spoke) = self.spokes.get(idx) else {
            return SECTOR_CHIP_H_ACTIVE;
        };
        let lead = ChipLead::Glyph(self.spoke_icon(idx));
        let mut chosen = SECTOR_CHIP_H;
        const STEP: f64 = 2.0;
        // (SECTOR_CHIP_H_ACTIVE - SECTOR_CHIP_H) / STEP, spelled as a
        // plain integer bound so the loop counts steps instead of
        // comparing floats against an accumulating float.
        for i in 0_u32..=8 {
            let step = SECTOR_CHIP_H + f64::from(i) * STEP;
            let need = Chip::natural_width_at(&spoke.text, step, lead, theme, text_renderer);
            if need <= chip_w {
                chosen = step;
            }
        }
        chosen
    }

    /// The width available to a sector's label chip, in pixels — what
    /// `draw` uses, exposed for callers needing the same answer.
    pub fn label_chip_width(&self) -> f64 {
        let (r_ref, r_out) = self.label_band_radii();
        (r_out - LABEL_RIM_GAP) - (r_ref + LABEL_INNER_INSET)
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
        let origin_px = (
            f64_round_to_i64_lossy(self.rect.x0),
            f64_round_to_i64_lossy(self.rect.y0),
        );
        let size_px = (
            f64_round_to_u64_lossy(self.rect.width()),
            f64_round_to_u64_lossy(self.rect.height()),
        );
        // 0.01° granularity: fine enough to invalidate every visible tick,
        // coarse enough to ignore float noise at rest.
        let rotation_centi_deg =
            f64_round_to_i64_lossy(f64::from(self.visual_rotation_deg()) * 100.0);
        // FNV-1a over spoke labels, avoiding a Vec clone just to detect renames.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for spoke in &self.spokes {
            for b in spoke.text.as_bytes() {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x100_0000_01b3);
            }
            // Separator byte so "ab"+"c" and "a"+"bc" hash differently.
            h ^= 0xff;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        WheelCacheFp {
            origin_px,
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

/// Lossy but bounded: the wheel never carries anywhere near 2^24
/// workspaces/sectors, so this never actually drops a representable
/// integer — it only silences the generic `usize -> f32` lint.
#[allow(
    clippy::cast_precision_loss,
    reason = "workspace/spoke/sector counts stay far below f32's 24-bit exact-integer range"
)]
fn usize_to_f32_lossy(n: usize) -> f32 {
    n as f32
}

/// Same rationale as [`usize_to_f32_lossy`], for the `f64` call sites.
#[allow(
    clippy::cast_precision_loss,
    reason = "workspace/spoke/sector counts stay far below f64's 53-bit exact-integer range"
)]
fn usize_to_f64_lossy(n: usize) -> f64 {
    n as f64
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

/// Rust's `f64 as i32` cast saturates rather than wrapping, so this is
/// memory-safe regardless of `v`; the caller (`slot_reach`) clamps the
/// result into `[0, MAX_SLOT_REACH]` immediately afterward anyway.
#[allow(
    clippy::cast_possible_truncation,
    reason = "saturating float-to-int cast; result is clamped by the caller"
)]
fn f64_saturating_to_i32(v: f64) -> i32 {
    v as i32
}

/// Narrows an `f64` pixel/angle/duration value to the `f32` that
/// `taffy`/`vello`'s own APIs take. The wheel's geometry never
/// approaches a magnitude where this loses meaningful precision, so
/// this is a deliberate, bounded narrowing rather than an accident.
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space geometry stays well within f32's precision for this UI"
)]
fn f64_to_f32_lossy(v: f64) -> f32 {
    v as f32
}

/// Pixel-coordinate delta, `i64 -> f64`: screen-space deltas between two
/// fingerprints never approach f64's 53-bit exact-integer range.
#[allow(
    clippy::cast_precision_loss,
    reason = "pixel-coordinate deltas are always tiny relative to f64's exact-integer range"
)]
fn i64_to_f64_lossy(v: i64) -> f64 {
    v as f64
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

/// Lossy but bounded: motion-token durations (fling projection, timeouts)
/// are always tiny millisecond counts, nowhere near f32's exact-integer
/// range.
#[allow(
    clippy::cast_precision_loss,
    reason = "motion durations are always tiny millisecond counts"
)]
fn u32_to_f32_lossy(n: u32) -> f32 {
    n as f32
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
    f64_saturating_to_i32(reach)
        .saturating_add(1)
        .clamp(0, MAX_SLOT_REACH)
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
            a0 + span * (usize_to_f64_lossy(i) / usize_to_f64_lossy(STEPS)),
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
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Leaf reserves the visible-arc width and takes 100% of the
        // parent slot's height — the disc's centre sits off-screen-left,
        // concept-faithful "huge wheel" feel.
        // `TaffyTree::new_leaf` only ever inserts into its own slotmaps
        // and returns `Ok` unconditionally (taffy 0.10's implementation
        // has no failure path for a leaf with no child/parent refs) —
        // so this is a `let...else` rather than a real fallible call.
        let Ok(node) = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(f64_to_f32_lossy(self.visible_width_px())),
                height: Dimension::percent(1.0),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            align_self: Some(AlignItems::Stretch),
            ..Default::default()
        }) else {
            unreachable!("TaffyTree::new_leaf has no failure path for a plain leaf node")
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Pick up a summon raised elsewhere in chrome (the keyboard
        // chord lives on the overlay host, which has the pointer); kept
        // as its own copy so tests and non-chrome hosts can drive it
        // directly through `set_summon`.
        if let Some(chrome) = self.chrome.as_ref() {
            self.summon_at = chrome.borrow().wheel_summon();
        }
        // `TaffyTree::layout` only ever returns `Err` for a config this
        // compositor never sets (rounding is always on); a stale/foreign
        // `node` would panic inside taffy's own slotmap index before this
        // Result is produced, so `unwrap` here wasn't guarding anything
        // this `let...else` doesn't already cover.
        let Ok(l) = taffy.layout(node) else {
            unreachable!("TaffyTree::layout only errors on a config this compositor never sets")
        };
        // Trimmed vertically by `SLOT_VERTICAL_INSET_FRAC` at both ends
        // so the wheel reads as inset rather than edge-to-edge.
        let x0 = origin.x + f64::from(l.location.x);
        let y0_raw = origin.y + f64::from(l.location.y);
        let h_raw = f64::from(l.size.height);
        let inset_v = h_raw * SLOT_VERTICAL_INSET_FRAC;
        let w = f64::from(l.size.width);
        let y0 = y0_raw + inset_v;
        let y1 = y0_raw + h_raw - inset_v;
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
        // The sidebar slot's width is viewport-driven, so recompute
        // `diameter_px` on every pass to keep the arc-fraction fixed.
        let rw = self.rect.width();
        if rw > 1.0 {
            self.diameter_px = rw / VISIBLE_RADIUS_FRACTION * 2.0;
        }
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
        // Cache fast-path: at rest nothing painted changes frame to frame, so
        // fingerprint every input and replay on a hit (hit rects stay valid —
        // the prior cache-miss draw populated them at the same rotation/rect).
        // A pan-only change replays under a translation instead of re-encoding.
        let fp = self.current_fingerprint();
        if fp.is_populated() && self.cached_fp.is_populated() {
            if fp == self.cached_fp {
                out_scene.append(&self.cached_scene, Some(out_transform));
                return;
            }
            if fp.content_eq(&self.cached_fp) {
                let dx =
                    i64_to_f64_lossy(fp.origin_px.0.saturating_sub(self.cached_fp.origin_px.0));
                let dy =
                    i64_to_f64_lossy(fp.origin_px.1.saturating_sub(self.cached_fp.origin_px.1));
                let shift = Affine::translate((dx, dy));
                out_scene.append(&self.cached_scene, Some(out_transform * shift));
                self.retarget_hit_rects(fp.origin_px);
                return;
            }
        }

        // Cache miss — encode at IDENTITY, then splice with
        // `out_transform`. `mem::take` gives the encode body an
        // exclusive `&mut Scene` without aliasing `self`.
        let mut local = std::mem::take(&mut self.cached_scene);
        local.reset();
        let scene = &mut local;
        let transform = Affine::IDENTITY;

        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        let radius = self.diameter_px / 2.0;
        if radius <= 1.0 || self.rect.area() <= 0.0 {
            // Restore the (empty) cache without recording a fingerprint
            // so a zero-width rect doesn't lock the cache.
            self.cached_scene = local;
            return;
        }
        let c = self.center();
        let cx = c.x;
        let cy = c.y;

        // ---- Geometry ---------------------------------------------------
        // The visible object is an annular BAND: constant-depth, as deep as
        // it can be while the inner arc stays inside the footprint.
        let half_h = self.rect.height() / 2.0;
        let r_out = radius;
        // `sqrt(d^2 + half_h^2)`: the radius whose arc passes exactly through
        // the footprint's left corners — deepest the band can be on a curve.
        let d_left = (self.rect.x0 - cx + BAND_LEFT_INSET).max(1.0);
        // The body reaches centre, so `r_ref` is a layout reference only
        // (labels, add button) — the solid fill doesn't move with it.
        let tangent_dy = half_h * BAND_TANGENT_FRACTION;
        let r_ref = (d_left * d_left + tangent_dy * tangent_dy)
            .sqrt()
            .clamp(4.0, r_out - 12.0);
        // `atan(half_h / d_left)` is the angle whose radial passes
        // exactly through the footprint's corner, plus slop so an
        // entering divider is already drawn.
        let sweep = (half_h / d_left).atan() + SWEEP_SLOP_DEG.to_radians();

        // ---- Pass 1: the band -------------------------------------------
        // Body, edges and drop shadow are ONE isolated group, faded top/bottom
        // by a single mask, so the band reads as a slice, not a hard cut. The
        // horizontal clip is load-bearing — unclipped, the ring would sweep
        // back around off-screen; it runs past the right edge for the rim shadow.
        // Flat fill — the theme's glass surface, same token as modal surfaces.
        let body_hue = colors.surfaces.glass;
        // Tracks the theme's foreground rather than a hardcoded white,
        // which is a scratch under the light theme and near-invisible
        // under high contrast.
        let edge_hue = colors.text_primary;
        let edge = |a: u8| Color::from_rgba8(edge_hue.r(), edge_hue.g(), edge_hue.b(), a);
        let group_clip = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1 + RIM_SHADOW_REACH,
            self.rect.y1,
        );

        // Vertical fade, used as the group's mask. White because only its
        // alpha channel is read (`DestIn` keeps the backdrop where the
        // mask is opaque); the colour never reaches the screen.
        let vfade = Gradient::new_linear(
            Point::new(self.rect.x0, self.rect.y0),
            Point::new(self.rect.x0, self.rect.y1),
        )
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: Color::from_rgba8(255, 255, 255, 0).into(),
                },
                ColorStop {
                    offset: FADE_IN_STOP,
                    color: Color::WHITE.into(),
                },
                ColorStop {
                    offset: 1.0 - FADE_IN_STOP,
                    color: Color::WHITE.into(),
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::from_rgba8(255, 255, 255, 0).into(),
                },
            ][..],
        );

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &group_clip,
        );

        // Same soft multi-ring falloff as the window-decoration pills,
        // painted first so the band sits on top. Tinted from the
        // theme's elevation token, not a literal black, so a
        // shadow-suppressing theme (high contrast) gets its own answer.
        let shadow_hue = tokens.elevation.glass_outer.color;
        for &(offset, width, alpha) in &[(2.5_f64, 5.0_f64, 44u8), (7.5, 6.0, 20), (14.0, 9.0, 8)] {
            scene.stroke(
                &Stroke::new(width),
                transform,
                Color::from_rgba8(shadow_hue.r(), shadow_hue.g(), shadow_hue.b(), alpha),
                None,
                &Circle::new((cx, cy), radius + offset),
            );
        }

        // The body itself: one flat fill, solid to the centre.
        let band = disc_segment(c, r_out, -sweep, sweep);
        scene.fill(Fill::NonZero, transform, body_hue, None, &band);

        // ---- Pass 2: the sectors ----------------------------------------
        // Display slots tile the sweep at a fixed step and CYCLE through the
        // workspaces — indexing off the workspace list instead left bald
        // unsegmented band whenever workspaces < slots.
        self.spoke_hit_rects.clear();
        self.spoke_hit_rects.resize(self.spokes.len(), Rect::ZERO);
        // A workspace can appear more than once in the sweep when there
        // are few of them; a click goes to the slot nearest the middle.
        let mut hit_claim = vec![f64::INFINITY; self.spokes.len()];

        // Settled before any sector is placed, so labels can be kept out
        // of it. Placed by depth down the band rather than up from the
        // floor — the floor is inside the vertical fade, which would
        // leave the button hanging on bare wallpaper.
        let add_cy = cy + half_h * ADD_BAND_FRACTION;
        let add_dy = add_cy - cy;
        // Tucked against the inner arc rather than mid-band, where a
        // label sweeping past would collide with it.
        let add_span_in = (r_ref * r_ref - add_dy * add_dy).max(0.0).sqrt();
        let add_cx = cx + add_span_in + ADD_BUTTON_D / 2.0 + 8.0;
        self.plus_rect =
            Rect::from_center_size(Point::new(add_cx, add_cy), (ADD_BUTTON_D, ADD_BUTTON_D));

        // Same curve the band is masked with, so a label dissolves where
        // the surface beneath it does rather than ending up a crisp
        // pill on bare wallpaper.
        let row_fade = |y: f64| -> f32 {
            let h = self.rect.height();
            if h <= 0.0 {
                return 0.0;
            }
            let t = f64_to_f32_lossy(((y - self.rect.y0) / h).clamp(0.0, 1.0));
            if t < FADE_IN_STOP {
                t / FADE_IN_STOP
            } else if t > 1.0 - FADE_IN_STOP {
                (1.0 - t) / FADE_IN_STOP
            } else {
                1.0
            }
        };

        if !self.spokes.is_empty() {
            let gap = f64::from(self.spoke_gap_deg());
            let rot = f64::from(self.visual_residual_deg());
            let slots = slot_reach(sweep, gap);
            let body_opacity = body_hue.components[3].clamp(0.0, 1.0);
            let wedge_alpha = ACTIVE_WEDGE_ALPHA
                + (ACTIVE_WEDGE_ALPHA_ON_OPAQUE - ACTIVE_WEDGE_ALPHA) * body_opacity;

            let slot_idx = self.slot_indices(slots);
            // `slots` is always >= 0 (see `slot_reach`); `checked_neg`
            // rather than `-slots` just keeps the negation from reading
            // as raw arithmetic to clippy.
            let neg_slots = slots.checked_neg().unwrap_or(i32::MIN);
            for k in neg_slots..=slots {
                let mid_deg = rot + f64::from(k) * gap;
                let mid = mid_deg.to_radians();
                let a0 = mid - (gap / 2.0).to_radians();
                let a1 = mid + (gap / 2.0).to_radians();
                // Wholly outside the sweep.
                if a0 > sweep || a1 < -sweep {
                    continue;
                }
                let Some(idx) = slot_at(&slot_idx, k, slots) else {
                    continue;
                };
                let is_active = k == 0;
                let base = Self::spoke_color(idx, colors);

                // The wedge itself is lit in the workspace's colour —
                // the thing that turns is also the thing that shows state.
                if is_active {
                    let wedge = disc_segment(c, r_out, a0, a1);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        base.with_alpha_factor(wedge_alpha),
                        None,
                        &wedge,
                    );
                }

                // Leading divider only, so adjacent sectors share one
                // line rather than stacking two translucent strokes.
                let div_a = if is_active { 96 } else { 40 };
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    edge(div_a),
                    None,
                    // From the centre: a divider stopping part-way would
                    // leave the solid body tinted but undivided beyond it.
                    &Line::new(c, polar(c, r_out, a0)),
                );
                // Active sector also owns its upper bound, bracketed by
                // two bright lines instead of one.
                if is_active {
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge(96),
                        None,
                        &Line::new(c, polar(c, r_out, a1)),
                    );
                }
            }
        }

        // The rim is the body's one lit edge, carrying the silhouette on
        // its own — full window-decoration language: soft wide stroke
        // reading as light on a bevel, crisp hairline for a definite
        // position, then an inset glow.
        let rim = Circle::new((cx, cy), r_out);
        scene.stroke(&Stroke::new(5.0), transform, edge(26), None, &rim);
        scene.stroke(&Stroke::new(1.5), transform, edge(140), None, &rim);
        // A 1-px-inset hairline just inside the rim, the disc's version of
        // the active pill's inner glow.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            edge(46),
            None,
            &Circle::new((cx, cy), r_out - 2.5),
        );

        // Fade the whole surface out, top and bottom, in one pass.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::new(Mix::Normal, Compose::DestIn),
            1.0,
            transform,
            &group_clip,
        );
        scene.fill(Fill::NonZero, transform, &vfade, None, &group_clip);
        scene.pop_layer();
        scene.pop_layer();

        // ---- Pass 3: the sector labels ----------------------------------
        // Each label is a chip built in the sector's own frame then
        // rotated about the disc's centre, so every chip sits at the
        // same radius and width — a ring of labels that tips with the
        // wheel, reading as paint on the surface.
        let chip_x1 = cx + r_out - LABEL_RIM_GAP;
        let chip_x0 = cx + r_ref + LABEL_INNER_INSET;
        let chip_w = chip_x1 - chip_x0;
        // Too shallow to carry a name is a reason to drop the name, not
        // the sector — it stays drawn and hit-tested with just its icon.
        let names_fit = chip_w >= LABEL_MIN_W;
        if !self.spokes.is_empty() {
            let gap = f64::from(self.spoke_gap_deg());
            let rot = f64::from(self.visual_residual_deg());
            let slots = slot_reach(sweep, gap);

            let slot_idx = self.slot_indices(slots);
            // `slots` is always >= 0 (see `slot_reach`); `checked_neg`
            // rather than `-slots` just keeps the negation from reading
            // as raw arithmetic to clippy.
            let neg_slots = slots.checked_neg().unwrap_or(i32::MIN);
            for k in neg_slots..=slots {
                let mid_deg = rot + f64::from(k) * gap;
                let mid = mid_deg.to_radians();
                if mid.abs() > sweep {
                    continue;
                }
                let Some(idx) = slot_at(&slot_idx, k, slots) else {
                    continue;
                };
                let is_active = k == 0;
                // Where the sector's centreline meets the rim — the
                // label's y, and what the vertical fade is sampled at.
                let anchor = polar(c, r_out, mid);
                let fade_a = row_fade(anchor.y);
                if fade_a <= 0.02 {
                    continue;
                }

                let lead = ChipLead::Glyph(self.spoke_icon(idx));
                let row_h = if is_active {
                    self.active_chip_height(idx, chip_w, theme, text_renderer)
                } else {
                    SECTOR_CHIP_H
                };
                let chip = Rect::new(chip_x0, cy - row_h / 2.0, chip_x1, cy + row_h / 2.0);
                let rotate = Affine::rotate_about(mid, c);

                // Where the chip actually lands once rotated. Used for
                // the click target and to keep labels off the add button.
                let placed = rotate.transform_rect_bbox(chip);
                // Clash test done in the label's OWN frame (button
                // centre rotated back, against the unrotated chip) — the
                // rotated chip's axis-aligned bbox is far bigger than
                // the chip itself and would falsely clash with a button
                // the label passes clear of.
                let local = rotate.inverse() * self.plus_rect.center();
                let reach = ADD_BUTTON_D / 2.0 + 4.0;
                let clash = local.x >= chip.x0 - reach
                    && local.x <= chip.x1 + reach
                    && local.y >= chip.y0 - reach
                    && local.y <= chip.y1 + reach;
                if clash {
                    continue;
                }

                // The sector is the container — no pill of its own,
                // since a pill inside a lit wedge is a box inside a box.
                // Active label colours as primary text: its own
                // workspace hue would sit on a wedge of that same hue,
                // where identity is already carried by the wedge.
                let style = if is_active {
                    ChipStyle::bare(None)
                } else {
                    // Dimming primary text by a fixed factor is wrong
                    // under high contrast, where secondary copy is kept
                    // near-white to clear AAA — so this is the palette's
                    // own secondary token, not a derived alpha.
                    ChipStyle::bare(Some(colors.text_dim))
                }
                .with_alpha(fade_a);
                Chip::draw_at(
                    scene,
                    transform * rotate,
                    chip,
                    if names_fit {
                        self.spokes.get(idx).map_or("", |s| s.text.as_str())
                    } else {
                        ""
                    },
                    lead,
                    style,
                    theme,
                    text_renderer,
                );

                // Nearest slot wins the workspace's click target. `idx` is
                // always in range here (both vecs are sized to
                // `self.spokes.len()` above), but a `.get_mut()` fallback
                // costs nothing and can't panic if that invariant ever
                // slips.
                if let Some(claim) = hit_claim.get_mut(idx) {
                    if mid.abs() < *claim {
                        *claim = mid.abs();
                        if let Some(hit_rect) = self.spoke_hit_rects.get_mut(idx) {
                            *hit_rect = placed;
                        }
                    }
                }

                // Presence dots seated just inside the rim so they ride
                // the surface with the sector.
                if let Some(ws_id) = self.spokes.get(idx).and_then(|s| s.workspace_id) {
                    let key = PresenceKey::Workspace {
                        id: format!("{ws_id}"),
                    };
                    let peers = presence.peers_on(&key);
                    if !peers.is_empty() {
                        let peer_count = peers.len().min(3);
                        let dot_r = r_out - LABEL_RIM_GAP / 2.0;
                        let spacing = 7.0_f64 / dot_r;
                        let centre_bias = (usize_to_f64_lossy(peer_count) - 1.0) / 2.0;
                        for (kk, user) in peers.iter().take(peer_count).enumerate() {
                            let off = (usize_to_f64_lossy(kk) - centre_bias) * spacing;
                            let (rr, gg, bb) = presence.color_for(user);
                            // Empty-initials Avatar chip in the peer's
                            // own colour, matching the cursor/avatar sites.
                            let p = polar(c, dot_r, mid + off);
                            let dot_rect = Rect::new(p.x - 3.0, p.y - 3.0, p.x + 3.0, p.y + 3.0);
                            ui::widget::Avatar::draw_at(
                                scene,
                                transform,
                                dot_rect,
                                "",
                                Color::from_rgb8(rr, gg, bb),
                                theme,
                                text_renderer,
                            );
                        }
                    }
                }
            }
        }

        // ---- Pass 4: the add-workspace button ---------------------------
        // A round glass button in the rim's material. Does NOT rotate —
        // it isn't a workspace, it's the control that makes one, so a
        // spot on the wheel would carry it out of reach every turn.
        let add_fade = row_fade(self.plus_rect.center().y).max(0.55);
        let add_circle = Circle::new(self.plus_rect.center(), ADD_BUTTON_D / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            body_hue.with_alpha_factor(add_fade),
            None,
            &add_circle,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            edge(f32_to_u8_saturating(74.0 * add_fade)),
            None,
            &add_circle,
        );
        let glyph = Rect::from_center_size(self.plus_rect.center(), (15.0, 15.0));
        Icon::Plus.draw_with_stroke(
            scene,
            transform,
            glyph,
            colors.text_primary.with_alpha_factor(0.72 * add_fade),
            1.6,
        );
        // Splice the freshly-encoded scene into `out_scene`, then
        // restore `self.cached_scene` and record the fingerprint so
        // the next idle frame takes the fast path.
        out_scene.append(&local, Some(out_transform));
        self.cached_scene = local;
        self.hit_rects_origin_px = fp.origin_px;
        self.cached_fp = fp;
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
        match event {
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => self.handle_left_press(handled),
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, scale_factor, handled);
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => self.handle_left_release(handled),
            WindowEvent::MouseWheel { delta, .. } => self.handle_mouse_wheel(delta, handled),
            _ => {}
        }
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
            if let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().add_workspace("Untitled");
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
            let delta_deg = f64_to_f32_lossy(dy) * Self::deg_per_pixel();
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
            winit::event::MouseScrollDelta::PixelDelta(p) => f64_to_f32_lossy(p.y),
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
                guard += 1;
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

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Real (clamped) frame dt keeps the spring wall-clock-correct
        // under frame drops. Clamped to 1/30 s: semi-implicit Euler
        // stays stable, and a long stall just resumes rather than
        // exploding.
        let now = std::time::Instant::now();
        let dt = self.last_update_at.map_or(1.0 / 60.0, |prev| {
            (now - prev).as_secs_f32().clamp(0.0, 1.0 / 30.0)
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

impl SidebarWheelDisc {
    /// Slide cached click targets to `target_origin_px` — used on the pan
    /// fast-path, where the scene replays under a translation instead of
    /// re-encoding, so the absolute-coordinate hit rects would otherwise go stale.
    fn retarget_hit_rects(&mut self, target_origin_px: (i64, i64)) {
        let dx = i64_to_f64_lossy(
            target_origin_px
                .0
                .saturating_sub(self.hit_rects_origin_px.0),
        );
        let dy = i64_to_f64_lossy(
            target_origin_px
                .1
                .saturating_sub(self.hit_rects_origin_px.1),
        );
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
        self.hit_rects_origin_px = target_origin_px;
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
        self.publish_active_to_chrome(idx);
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
            let coast_s = u32_to_f32_lossy(Gesture::default().fling_projection_ms) / 1000.0;
            let projected = self.rotation_deg + velocity * coast_s;
            self.rotation_for_idx(self.idx_at_rotation(projected))
        };

        let idx = self.idx_at_rotation(target);
        self.rotation_deg = target;
        self.wheel_overscroll_px = 0.0;
        // The tree is written exactly once per gesture, here — the
        // indicator's motion during the drag was display only.
        self.publish_active_to_chrome(idx);
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
        let c = self.center();
        let half_h = self.rect.height() / 2.0;
        let d_left = (self.rect.x0 - c.x + BAND_LEFT_INSET).max(1.0);
        let tangent_dy = half_h * BAND_TANGENT_FRACTION;
        let r_in = (d_left * d_left + tangent_dy * tangent_dy)
            .sqrt()
            .clamp(4.0, radius - 12.0);
        (r_in, radius)
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
            total += peers.len().min(3);
        }
        total
    }

    /// If a chrome handle is attached and the active spoke carries a
    /// `workspace_id`, push the new active workspace to `chrome.tree`.
    /// Matches the `SidebarWheelChrome` write pattern: `tree.set_active`
    /// + `bump()` so the rest of the shell re-syncs on the next frame.
    fn publish_active_to_chrome(&self, idx: usize) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let Some(spoke) = self.spokes.get(idx) else {
            return;
        };
        let Some(id) = spoke.workspace_id else {
            return;
        };
        let mut state = chrome.borrow_mut();
        if state.tree().active == WorkspaceId(id) {
            // Already there. Bumping anyway re-syncs the rest of the
            // shell against a tree that didn't change — every gesture
            // that ends where it started does exactly that.
            return;
        }
        state.tree_mut().set_active(WorkspaceId(id));
        state.bump();
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

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

    fn cursor_event(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(x, y),
        }
    }

    fn press_event() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// `lines` is in `LineDelta` units; the widget scales by 16 px.
    fn wheel_event(lines: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, lines),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    fn release_event() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    fn pump(d: &mut SidebarWheelDisc, ev: WindowEvent) -> bool {
        use automerge::AutoCommit;
        let mut doc = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let taffy: TaffyTree<()> = TaffyTree::new();
        // handle_event doesn't consult the node id.
        let dummy_node = unsafe { std::mem::transmute::<u64, NodeId>(0) };
        d.handle_event(
            &ev,
            1.0,
            &mods,
            &mut doc,
            &path,
            &mut handled,
            &mut focus,
            dummy_node,
            &taffy,
        );
        handled
    }

    /// The real `Widget::draw`, which populates the scene cache, the
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

    #[test]
    fn visible_neighbours_carry_a_name_not_just_a_speck() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());
        paint(&mut d);

        let named: Vec<usize> = (0..d.spokes.len())
            .filter(|i| {
                *i != d.active_idx
                    && d.spoke_hit_rects[*i]
                        .width()
                        .max(d.spoke_hit_rects[*i].height())
                        >= LABEL_MIN_W
            })
            .collect();
        assert!(
            !named.is_empty(),
            "no neighbour got a named sector: hit rects {:?}",
            d.spoke_hit_rects
        );
    }

    #[test]
    fn sectors_sit_at_their_real_index_relative_to_the_active_one() {
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
                d.spokes.len().min((slots * 2 + 1) as usize),
                "every workspace that fits should get a slot at reach {slots}"
            );
        }
    }

    #[test]
    fn the_disc_is_never_empty_at_any_rotation() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());

        let gap = d.spoke_gap_deg();
        // Well past either end, so the rubber band is fully extended.
        let far = gap * 12.0;
        let mut rot = -far;
        while rot <= far {
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

    /// Same invariant reached via the scroll wheel's separate code path.
    #[test]
    fn the_disc_is_never_empty_under_sustained_scrolling() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());
        // The handler ignores wheel events not over the disc.
        let cx = drag_anchor_x(&d);
        let cy = f64::midpoint(d.rect.y0, d.rect.y1);
        pump(&mut d, cursor_event(cx, cy));

        for dir in [-1.0_f32, 1.0] {
            for _ in 0..60 {
                pump(&mut d, wheel_event(dir * 3.0));
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

    #[test]
    fn every_workspace_stays_on_the_disc_at_every_size() {
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
                    d.label_chip_width()
                );
            }
        }
    }

    /// Drives the real press path at every position a label passes
    /// through, rather than comparing rectangles — the ordering where
    /// the button resolves before a sector is the thing that must hold.
    #[test]
    fn the_add_button_stays_pressable_at_every_rotation() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;

        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let before = chrome.borrow().tree().workspaces.len();
        let mut d = mk(&["Home", "Research", "Design", "Marketing", "Analytics"])
            .with_chrome(chrome.clone());
        layout_into_slot(&mut d, live_slot());

        for step in 0..72 {
            d.rotation_deg = step as f32 * 5.0;
            paint(&mut d);
            assert!(
                d.plus_rect.area() > 0.0,
                "the add button is placed at {}°",
                d.rotation_deg
            );

            let expected = before + step + 1;
            let p = d.plus_rect.center();
            pump(&mut d, cursor_event(p.x, p.y));
            assert!(
                pump(&mut d, press_event()),
                "a press on the add button at {}° was not handled — a label \
                 sweeping over it has taken the press",
                d.rotation_deg
            );
            assert_eq!(
                chrome.borrow().tree().workspaces.len(),
                expected,
                "the press at {}° did not create a workspace",
                d.rotation_deg
            );
            pump(&mut d, release_event());
        }
    }

    /// The add button has to sit ON the band, at every rotation, not
    /// hang underneath it on bare wallpaper.
    #[test]
    fn the_add_button_sits_on_the_band() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());

        for step in 0..24 {
            d.rotation_deg = step as f32 * 15.0;
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

    /// Driven from the middle workspace so all three sectors are on the
    /// band — from the first workspace the last sector is off the band
    /// outright, a legitimate reason to have no label that would mask
    /// the case this guards.
    #[test]
    fn a_sector_keeps_its_name_when_it_only_passes_near_the_add_button() {
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

    /// Past a point the band's inner arc leaves through the footprint's
    /// left edge and the clip turns it into a straight cut — survivable
    /// only while the fade has already taken the band down. Asserts on
    /// the two constants that decide it, not a rendered pixel.
    #[test]
    fn band_clip_starts_where_the_fade_has_already_taken_hold() {
        // Fraction of the footprint's height between its edge and the
        // point the inner arc starts being clipped.
        let clip_at = (1.0 - BAND_TANGENT_FRACTION) / 2.0;
        // The fade ramps linearly from 0 at the edge to 1 at FADE_IN_STOP.
        let alpha_at_clip = clip_at / f64::from(FADE_IN_STOP);
        assert!(
            alpha_at_clip <= 0.35,
            "the band gets clipped while still at {:.0}% alpha — the cut \
             will read as a straight edge sliced through the surface. \
             Raise BAND_TANGENT_FRACTION ({BAND_TANGENT_FRACTION}) or \
             FADE_IN_STOP ({FADE_IN_STOP}).",
            alpha_at_clip * 100.0
        );
    }

    /// Display slots must cover the whole visible sweep, not just the
    /// workspaces — indexing off the workspace list would leave unsegmented band.
    #[test]
    fn sector_slots_cover_the_whole_sweep() {
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

    /// A one-workspace disc is degenerate: `spoke_gap_deg` widens to a full
    /// turn, so the fan must collapse to one slot, not tile copies.
    #[test]
    fn a_single_workspace_gets_exactly_one_slot() {
        assert_eq!(slot_reach(40.0_f64.to_radians(), 360.0), 0);
    }

    /// Dragging must keep asking for frames — the host skips repaint on a
    /// pointer move unless the tree reports dirty.
    #[test]
    fn dragging_the_rim_reports_dirty_on_every_move() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let cy = f64::midpoint(d.rect.y0, d.rect.y1);
        let x = drag_anchor_x(&d);
        paint(&mut d);
        assert!(
            !d.needs_redraw(),
            "a painted, untouched disc is clean — otherwise this test proves nothing"
        );

        pump(&mut d, cursor_event(x, cy));
        assert!(pump(&mut d, press_event()), "press inside the arc grabs");
        let before = d.rotation_deg;

        pump(&mut d, cursor_event(x, cy + 60.0));
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

    #[test]
    fn panning_the_strip_reports_dirty_and_moves_the_click_targets() {
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

    #[test]
    fn a_pure_pan_is_a_content_cache_hit() {
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
        let centre_y = f64::midpoint(d.rect.y0, d.rect.y1);
        assert!(
            (centre_y - pointer.y).abs() < 1.0,
            "the disc centres on the pointer vertically, got {centre_y} for pointer y={}",
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

    /// Real `build_layout` / `apply_layout` against a slot, as the
    /// overlay host does.
    fn layout_into_slot(d: &mut SidebarWheelDisc, slot: Rect) {
        use std::collections::HashMap;
        use taffy::prelude::TaffyMaxContent;
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = d.build_layout(&mut taffy, &theme, &mut node_map);
        let mut style = taffy.style(node).unwrap().clone();
        style.size = Size {
            width: Dimension::length(slot.width() as f32),
            height: Dimension::length(slot.height() as f32),
        };
        taffy.set_style(node, style).unwrap();
        taffy
            .compute_layout(node, taffy::Size::MAX_CONTENT)
            .unwrap();
        d.apply_layout(&taffy, node, Point::new(slot.x0, slot.y0));
    }

    #[test]
    fn new_starts_with_active_idx_zero_and_rotation_zero() {
        let d = mk(&["Home", "Research", "Design"]);
        assert_eq!(d.active_idx, 0);
        assert_eq!(d.rotation_deg, 0.0);
        assert!(d.drag_from.is_none());
        assert!(!d.wants_pointer_capture());
    }

    #[test]
    fn default_workspace_labels_resolve_to_distinct_glyphs() {
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

    /// Hover must NOT claim pointer capture — reserved for an
    /// actually-in-flight drag.
    #[test]
    fn hover_does_not_capture_pointer() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let cy = d.diameter_px / 2.0;
        let cx = drag_anchor_x(&d);
        pump(&mut d, cursor_event(cx, cy));
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

    #[test]
    fn press_then_release_clears_pointer_capture() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let cy = d.diameter_px / 2.0;
        let cx = drag_anchor_x(&d);
        pump(&mut d, cursor_event(cx, cy));
        assert!(pump(&mut d, press_event()));
        assert!(
            d.wants_pointer_capture(),
            "press inside disc must capture pointer"
        );
        assert!(pump(&mut d, release_event()));
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

    #[test]
    fn drag_and_release_snaps_to_nearest_spoke() {
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
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        assert!(d.drag_from.is_some(), "press inside disc must start drag");
        assert!(d.wants_pointer_capture());

        // `15/0.15 = 100px` is exactly one gap; margin lands cleanly past.
        // Dragged UPWARD: workspace 0 is the first, so pulling down pulls
        // against the top end instead of reaching a neighbour.
        let drag_dy = f64::from(gap / DRAG_FACTOR) + 30.0;
        pump(&mut d, cursor_event(cx, start_y - drag_dy));
        assert!(d.rotation_deg < 0.0);

        let before_idx = d.active_idx;
        assert!(pump(&mut d, release_event()));
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

    #[test]
    fn drag_without_release_keeps_rotation_continuous() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));

        let idx_before = d.active_idx;
        // 37 px * DRAG_FACTOR = 5.55° — not a multiple of the 15° gap.
        pump(&mut d, cursor_event(cx, start_y + 37.0));
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

    #[test]
    fn single_spoke_disc_never_snaps_past_itself() {
        let mut d = mk(&["Only"]);
        assert_eq!(d.spokes.len(), 1);
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);

        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // Drag a huge distance in either direction.
        pump(&mut d, cursor_event(cx, start_y + 10_000.0));
        assert!(pump(&mut d, release_event()));
        assert_eq!(d.active_idx, 0);

        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y - 10_000.0));
        assert!(pump(&mut d, release_event()));
        assert_eq!(d.active_idx, 0);
    }

    #[test]
    fn snap_publishes_active_workspace_to_chrome() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;

        let tree = WorkspaceTree::bootstrap_demo();
        assert_eq!(tree.workspaces.len(), 3);
        let ws_ids: Vec<_> = tree.workspaces.iter().map(|w| w.id).collect();
        let initial_active = tree.active;
        assert_eq!(
            initial_active, ws_ids[0],
            "bootstrap_demo should leave active = ws_ids[0]"
        );

        // Spokes carry workspace ids so write-back can map active_idx →
        // WorkspaceId.
        let chrome = ChromeState::new(tree);
        let spokes: Vec<SpokeLabel> = ws_ids
            .iter()
            .enumerate()
            .map(|(i, id)| SpokeLabel {
                text: format!("WS-{i}"),
                workspace_id: Some(id.0),
            })
            .collect();
        let mut d = SidebarWheelDisc::new(spokes).with_chrome(chrome.clone());
        let visible_w = d.visible_width_px();
        d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);

        // Drag UP: the tree starts on its first workspace, the top end.
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);
        let drag_dy = f64::from(d.spoke_gap_deg() / DRAG_FACTOR) + 100.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y - drag_dy));
        assert!(pump(&mut d, release_event()));

        assert_ne!(
            d.selected_idx(),
            0,
            "a >gap drag must advance past the initial spoke"
        );
        let expected_id = ws_ids[d.selected_idx()];
        assert_eq!(
            chrome.borrow().tree().active,
            expected_id,
            "snap_to_nearest must publish the new active workspace id to chrome"
        );
        assert_ne!(
            chrome.borrow().tree().active,
            initial_active,
            "chrome's tree.active must have changed after the snap"
        );
    }

    #[test]
    fn disc_without_chrome_snaps_without_panicking() {
        let mut d = mk(&["Home", "Research", "Design"]);
        assert!(d.chrome.is_none());
        let idx_before = d.active_idx;
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);
        let drag_dy = f64::from(d.spoke_gap_deg() / DRAG_FACTOR) + 100.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // Upward — workspace 0 is the top of a finite list.
        pump(&mut d, cursor_event(cx, start_y - drag_dy));
        assert!(pump(&mut d, release_event()));
        assert_ne!(d.selected_idx(), idx_before);
    }

    #[test]
    fn snap_animates_rotation_over_multiple_ticks() {
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

        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y - drag_px));
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
        assert!(pump(&mut d, release_event()));

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

    /// Mirrors the `tray_pill_rect` / `avatar_stack_rect` `ZERO`-on-new
    /// contract in `TopBarChrome`.
    #[test]
    fn new_starts_with_plus_rect_zero() {
        let d = mk(&["Home", "Research", "Design"]);
        assert_eq!(
            d.plus_rect,
            Rect::ZERO,
            "plus_rect must be ZERO until the first draw pass populates it"
        );
        assert!(d.last_cursor_pos.is_none());
    }

    #[test]
    fn press_on_plus_rect_calls_chrome_add_workspace() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;

        // First-boot tree: one empty workspace — the state where "+"
        // matters most, so it has to work from a single spoke.
        let tree = WorkspaceTree::bootstrap_default();
        let initial_count = tree.workspaces.len();
        let initial_active = tree.active;
        let chrome = ChromeState::new(tree);

        let mut d = SidebarWheelDisc::new(vec![SpokeLabel {
            text: "Home".into(),
            workspace_id: None,
        }])
        .with_chrome(chrome.clone());
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
        let slot_cx = d.plus_rect.center().x;
        let slot_cy = d.plus_rect.center().y;

        // The press handler hit-tests against the cached
        // `last_cursor_pos`, not the event (which has no position field).
        pump(&mut d, cursor_event(slot_cx, slot_cy));
        assert!(
            pump(&mut d, press_event()),
            "press on plus slot must be handled"
        );

        let state = chrome.borrow();
        assert_eq!(
            state.tree().workspaces.len(),
            initial_count + 1,
            "plus-press must grow the workspace count by exactly 1"
        );
        assert_ne!(
            state.tree().active,
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

    #[test]
    fn disc_renders_dot_per_seeded_peer_on_workspace() {
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

    /// `cx = rect.x0 - radius * (1 - VISIBLE_RADIUS_FRACTION)`.
    #[test]
    fn disc_anchors_left_of_viewport_so_only_arc_intrudes() {
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

    #[test]
    fn disc_visible_arc_matches_concept_ratio() {
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

    #[test]
    fn disc_hit_test_rejects_clicks_left_of_viewport_edge() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let c = d.center();
        pump(&mut d, cursor_event(c.x, c.y));
        assert!(
            d.drag_from.is_none(),
            "off-screen cursor must not seed a drag anchor; drag_from={:?}",
            d.drag_from
        );
        assert!(
            !pump(&mut d, press_event()),
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

    #[test]
    fn disc_caps_presence_dots_at_three_per_spoke() {
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
        pump(d, cursor_event(cx, start_y + dy));
        frames(d, settle_frames);
    }

    /// Must visit each workspace in turn — never skipping one, never
    /// running backwards — and change hands within half a detent of
    /// the boundary each time.
    #[test]
    fn the_indicator_steps_one_workspace_at_a_time_during_a_drag() {
        let mut d = mk(&["A", "B", "C", "D", "E"]);
        let gap = d.spoke_gap_deg();
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        // Start in the middle so there's room to travel without
        // meeting an end.
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));

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
            let boundary = -(*idx as f32 - 0.5) * gap;
            assert!(
                (rot - boundary).abs() < gap * 0.15,
                "the indicator changed to {idx} at {rot:.2}°, but the boundary \
                 between {} and {idx} is at {boundary:.2}°",
                idx - 1
            );
        }
    }

    /// The settle must never cross its target (overshoot reads light)
    /// and must take visible but bounded time to arrive.
    #[test]
    fn the_settle_is_slow_enough_to_feel_and_never_overshoots() {
        let mut d = mk(&["A", "B", "C", "D"]);
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
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
        assert!(pump(&mut d, release_event()));

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

        let ms = frames_taken as f32 * 1000.0 / 60.0;
        assert!(
            (250.0..900.0).contains(&ms),
            "the settle took {ms:.0} ms. Under ~250 ms it snaps and the weight is \
             gone; over ~900 ms the control is not heavy, it is slow."
        );
    }

    /// The rim trails the pointer instead of being welded to it, and
    /// the gap grows with speed — rigid tracking reads weightless.
    #[test]
    fn the_rim_trails_a_fast_drag_further_than_a_slow_one() {
        let lag_for = |gaps_per_frame: f64| -> f32 {
            let names: Vec<String> = (0..16).map(|i| format!("W{i}")).collect();
            let refs: Vec<&str> = names.iter().map(std::string::String::as_str).collect();
            let mut d = mk(&refs);
            let px_per_frame =
                gaps_per_frame * f64::from(d.spoke_gap_deg()) / f64::from(DRAG_FACTOR);
            let cx = drag_anchor_x(&d);
            let start_y = d.diameter_px / 2.0;
            pump(&mut d, cursor_event(cx, start_y));
            assert!(pump(&mut d, press_event()));
            // Long enough for the follow spring to reach steady state.
            let mut dy = 0.0;
            for _ in 0..40 {
                dy -= px_per_frame;
                pump(&mut d, cursor_event(cx, start_y + dy));
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

    /// From the same release position, a flick must land further down
    /// the list than a slow let-go.
    #[test]
    fn a_flick_carries_past_the_workspace_a_slow_release_settles_on() {
        let land = |gaps_per_frame: f64, pause_frames: usize| -> usize {
            let mut d = mk(&["A", "B", "C", "D", "E", "F", "G", "H"]);
            let px_per_frame =
                gaps_per_frame * f64::from(d.spoke_gap_deg()) / f64::from(DRAG_FACTOR);
            let cx = drag_anchor_x(&d);
            let start_y = d.diameter_px / 2.0;
            pump(&mut d, cursor_event(cx, start_y));
            assert!(pump(&mut d, press_event()));
            let mut dy = 0.0;
            for _ in 0..24 {
                dy -= px_per_frame;
                pump(&mut d, cursor_event(cx, start_y + dy));
                d.advance(1.0 / 60.0);
            }
            // A pause before release bleeds the velocity off, which is
            // what separates "thrown" from "placed".
            frames(&mut d, pause_frames);
            assert!(pump(&mut d, release_event()));
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

    /// Pulling past the first workspace has to give, resist more the
    /// further it's pulled, stay bounded, and return to the end itself.
    #[test]
    fn pulling_past_the_first_workspace_resists_and_returns_to_it() {
        let mut d = mk(&["A", "B", "C"]);
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));

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
            let raw = (pull_px as f32) * DRAG_FACTOR;
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

        assert!(pump(&mut d, release_event()));
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

    #[test]
    fn the_last_workspace_is_an_end_and_the_list_does_not_wrap() {
        let mut d = mk(&["A", "B", "C"]);
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        drag_to(&mut d, cx, start_y, -10_000.0, 60);
        assert_eq!(
            d.active_idx, 2,
            "dragging past the last workspace wrapped round to the start"
        );
        assert!(pump(&mut d, release_event()));
        assert_eq!(d.selected_idx(), 2);
    }

    /// Scroll input has to meet the same edge the drag does, or the two
    /// ways into the control feel like different controls.
    #[test]
    fn scrolling_into_an_end_pulls_against_it_and_lets_go() {
        let mut d = mk(&["A", "B", "C"]);
        let cx = drag_anchor_x(&d);
        let mid_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, mid_y));

        for _ in 0..6 {
            pump(&mut d, wheel_event(4.0));
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
        let mut st = AutoCommit::new();
        let obj = automerge::ROOT.clone();
        d.update(&mut st, &obj);
        assert_eq!(
            d.rotation_deg, 0.0,
            "the pull never let go once the gesture ended"
        );
        frames(&mut d, 200);
        assert!(d.visual_rotation_deg().abs() < 0.1);
    }

    /// The overscroll work must not have quietly turned scroll into a
    /// continuous drag: still three clicks per workspace.
    #[test]
    fn scrolling_mid_list_still_takes_three_clicks_per_workspace() {
        let mut d = mk(&["A", "B", "C", "D", "E"]);
        let cx = drag_anchor_x(&d);
        let mid_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, mid_y));
        // One wheel click is 16 px; the detent is 48.
        for click in 1..=3 {
            pump(&mut d, wheel_event(-1.0));
            let expected = usize::from(click == 3);
            assert_eq!(
                d.selected_idx(),
                expected,
                "after {click} wheel click(s) the wheel should be on workspace \
                 {expected}"
            );
        }
    }

    /// The active chip must never truncate the name to keep its size step.
    #[test]
    fn the_active_workspace_name_is_never_truncated_to_fit_its_size_step() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let chip_w = d.label_chip_width();
        assert!(chip_w > LABEL_MIN_W, "band too shallow to test: {chip_w}");

        for (idx, name) in ["Home", "Research", "Design"].iter().enumerate() {
            let h = d.active_chip_height(idx, chip_w, &theme, &mut tr);
            assert!(
                (SECTOR_CHIP_H..=SECTOR_CHIP_H_ACTIVE).contains(&h),
                "{name}: chip height {h} outside the family's two sizes"
            );
            let need = Chip::natural_width_at(
                name,
                h,
                ChipLead::Glyph(d.spoke_icon(idx)),
                &theme,
                &mut tr,
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

    /// A gesture that ends where it started must not bump the chrome
    /// revision — the shell re-syncs the whole tree against it.
    #[test]
    fn a_gesture_that_lands_where_it_started_does_not_bump_the_chrome() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;

        let tree = WorkspaceTree::bootstrap_demo();
        let ws_ids: Vec<_> = tree.workspaces.iter().map(|w| w.id).collect();
        let chrome = ChromeState::new(tree);
        let spokes: Vec<SpokeLabel> = ws_ids
            .iter()
            .map(|id| SpokeLabel {
                text: "W".into(),
                workspace_id: Some(id.0),
            })
            .collect();
        let mut d = SidebarWheelDisc::new(spokes).with_chrome(chrome.clone());
        let visible_w = d.visible_width_px();
        d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);

        let before = chrome.borrow().revision;
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // A pull against the top end, which can only come back to where
        // it began.
        drag_to(&mut d, cx, start_y, 400.0, 30);
        assert!(pump(&mut d, release_event()));
        assert_eq!(
            d.selected_idx(),
            0,
            "the pull cannot have selected anything"
        );
        assert_eq!(
            chrome.borrow().revision,
            before,
            "a bounce off the end bumped the chrome, forcing a full re-sync for a \
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
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
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
        assert!(pump(&mut d, release_event()));
        assert_ne!(d.selected_idx(), 0);
        assert_ne!(
            chrome.borrow().revision,
            before,
            "an actual workspace change must still bump the chrome"
        );
    }

    #[test]
    fn click_to_spoke_makes_target_active() {
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

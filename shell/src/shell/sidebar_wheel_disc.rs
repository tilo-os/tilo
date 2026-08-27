//! `SidebarWheelDisc` — radial workspace switcher anchored off-screen-left,
//! with a narrow arc of its rim protruding into the sidebar column. Each
//! workspace is a spoke; dragging vertically rotates the wheel, and release
//! snaps to the nearest spoke.
//!
//! `rotation_deg` is absolute: workspace `i` rests at `-i * spoke_gap_deg()`,
//! so the list spans `[-(N-1)*gap, 0]` with an end at each side — every drag,
//! flick, click and scroll writes this one number, and `active_idx` is always
//! just "which workspace `rotation_deg` is nearest". `draw` instead wants the
//! *residual* offset from the active sector's rest position
//! (`SidebarWheelDisc::visual_residual_deg`), so the lit sector doesn't march
//! down the arc as you switch workspaces.
//!
//! Pixel-perfect concept reference: `radial-workspace-switcher/src/App.tsx`.

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
    /// Spokes, one per workspace in the tree. Rendered radially around the
    /// rim with active vs inactive styling per the concept reference.
    pub spokes: Vec<SpokeLabel>,
    /// Index of the spoke currently "up" — i.e. the workspace the user
    /// would return to if they released without further dragging.
    pub active_idx: usize,
    /// Where the wheel is being asked to sit, in **absolute** degrees
    /// (see module docs). The *target*; the settle/follow spring's value
    /// is what's on screen while one is in flight — see
    /// [`SidebarWheelDisc::visual_rotation_deg`] and
    /// [`SidebarWheelDisc::visual_residual_deg`].
    pub rotation_deg: f32,
    /// `Some(origin)` while a drag is in flight; `None` when idle.
    /// Set only on a Pressed-inside-arc transition; cleared on Release.
    /// `wants_pointer_capture` keys off this field.
    pub drag_from: Option<Point>,
    /// Most-recent in-arc cursor position cached during idle hover, used
    /// as the press hit-test anchor when `last_cursor_pos` was last
    /// updated outside the arc. Distinct from `drag_from` so a hover
    /// over the disc never spuriously claims pointer capture.
    pub hover_anchor: Option<Point>,
    /// Rotation at the moment the drag started — anchor for the
    /// per-frame `rotation = origin + dy * DRAG_FACTOR` mapping. Seeded
    /// from where the wheel *visually* is, not from `rotation_deg`, so
    /// grabbing one that is still settling picks it up where it stands.
    pub drag_rotation_origin: f32,
    /// The drag's rotation before overscroll resistance and detent
    /// magnetism are applied — i.e. where the pointer is literally
    /// asking the wheel to go.
    ///
    /// Kept because release has to distinguish "let go past the end"
    /// from "let go just short of it", and the resisted value cannot:
    /// resistance compresses a 300 px overpull and a 30 px one into
    /// neighbouring angles, both inside the wheel's own range.
    drag_raw_deg: f32,
    /// Hit-test bounds: the disc's visible footprint inside the sidebar
    /// column. `rect.width()` ≈ `SIDEBAR_WIDTH_PX`; `rect.height()`
    /// fills the parent slot vertically.
    pub rect: Rect,
    /// Disc diameter in logical pixels. Drives the geometry math and the
    /// drag→angle mapping; recomputed from the host slot on layout so
    /// the visible arc matches `SIDEBAR_WIDTH_PX`.
    pub diameter_px: f64,
    /// Optional shared chrome handle. When present, a snap that lands on
    /// a spoke whose `workspace_id` is known writes the new active
    /// workspace back to `chrome.tree`.
    pub chrome: Option<Rc<RefCell<ChromeState>>>,
    /// `Some(spring)` while a post-release snap is easing the visible
    /// rim from the pre-snap rotation to the snapped target. `None` when
    /// idle. Constructed lazily on release using the `snappy` preset.
    pub rotation_spring: Option<Spring>,
    /// Wall-clock timestamp of the previous `update` tick, used to feed
    /// the rotation spring the *real* frame dt instead of a hardcoded
    /// `1/60`. Under frame drops a fixed dt made the spring integrate too
    /// little time and run in slow-motion; real (clamped) dt keeps it
    /// wall-clock-correct. `None` until the first tick.
    pub last_update_at: Option<std::time::Instant>,
    /// Cached bounding rect of the "+" add-workspace slot painted below
    /// the disc's rim. `Rect::ZERO` until the first draw pass populates it.
    pub plus_rect: Rect,
    /// Most-recent cursor position in logical pixels — used by the plus-
    /// slot press hit-test (which lives outside the rim) and as a
    /// fallback drag anchor for synthetic test sequences.
    pub last_cursor_pos: Option<Point>,
    /// Wheel-scroll accumulator (concept's `wheelAccumulator`). Drained
    /// `WHEEL_SCROLL_TIMEOUT_MS` after the last scroll event, at which
    /// point the wheel snaps to the nearest spoke.
    pub wheel_accumulator: f32,
    /// Wall-clock instant of the last `MouseWheel` event. Once the gap
    /// since `now()` exceeds `WHEEL_SCROLL_TIMEOUT_MS`, `update` snaps to
    /// the nearest spoke and clears the accumulator.
    pub wheel_last_event: Option<Instant>,
    /// Scroll pixels spent pushing against an end, where there was no
    /// next workspace to spend them on. Signed the same way
    /// `wheel_accumulator` is. Feeds the overscroll pull so scrolling
    /// into an end resists exactly as dragging into one does; released
    /// by the same idle timer that drains the accumulator.
    wheel_overscroll_px: f32,
    /// The settle spring as it stood the moment the current settle
    /// began, so a harness can replay the curve from its start and hold
    /// it at a chosen millisecond. See
    /// [`SidebarWheelDisc::hold_rotation_at`]. `None` until the first
    /// settle.
    settle_origin: Option<Spring>,
    /// Cached bounding rects of each spoke's clickable cluster (icon
    /// tile + label) in screen-space, populated by `draw`. Consumed by
    /// `handle_event` to route a press on a non-active spoke into a
    /// `click_to_spoke` rotation animation.
    pub spoke_hit_rects: Vec<Rect>,
    /// Scene cache — covers the entire disc paint: shadow bands, disc
    /// fill, rim hairlines, all spokes, the "+" add-workspace slot. At
    /// rest (no rotation animation), the disc is fully static and
    /// caching saves the per-spoke trig + many strokes/fills.
    cached_scene: Scene,
    /// Cache-validity fingerprint. Reused iff every field matches the
    /// current frame — see [`SidebarWheelDisc::current_fingerprint`].
    cached_fp: WheelCacheFp,
    /// Whole-pixel origin the cached hit rects (`spoke_hit_rects`,
    /// `plus_rect`) currently sit at. Equal to `cached_fp.origin_px`
    /// right after a full repaint, then advanced by
    /// [`SidebarWheelDisc::retarget_hit_rects`] on each pan frame that
    /// takes the translate fast path. Tracked separately so the shift
    /// is applied once per move rather than re-applied cumulatively
    /// against the recording origin.
    hit_rects_origin_px: (i64, i64),
    /// When `Some(p)`, the disc is *summoned*: it detaches from the
    /// strip's canvas and places itself so its rim passes through `p`,
    /// vertically centred on `p`. `None` is the docked state, where the
    /// disc sits at the left edge of the strip's content and pans with
    /// it.
    ///
    /// Summoning exists because docking isn't always reachable: the disc
    /// belongs to the workspace, so scrolling right through a strip
    /// carries it off-screen with everything else. The footprint is
    /// unchanged — a summon moves the same object to the pointer, it
    /// doesn't reveal more of it.
    summon_at: Option<Point>,
}

/// Inputs that change the disc's painted pixels. The visual rotation
/// is bucketed into 0.01-degree integers so spring-in-flight values
/// invalidate on every micro-step while still being comparable.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct WheelCacheFp {
    /// Top-left of the disc's footprint, in whole pixels. Split out
    /// from the size because it is the ONE input that changes on a
    /// plain strip pan, and a pan does not change a single painted
    /// pixel — only where they land. Keeping it separate lets `draw`
    /// replay the cached scene under a translation instead of
    /// re-encoding it. See [`WheelCacheFp::content_eq`].
    origin_px: (i64, i64),
    /// Footprint size in whole pixels. Unlike the origin this *does*
    /// change the painted geometry (the disc's curvature is derived
    /// from the rect width), so it stays on the content side.
    size_px: (u64, u64),
    /// `visual_rotation_deg() * 100` rounded to i64. Bucketing to
    /// hundredths keeps the cache stable across float-precision
    /// noise while still invalidating on every visible animation tick.
    rotation_centi_deg: i64,
    active_idx: usize,
    /// Number of spokes. Spoke labels live in `cached_spoke_signature`
    /// because they don't change without a tree mutation that already
    /// bumps the chrome revision.
    spoke_count: usize,
    /// FNV-1a hash of every spoke's `text` field. Catches label
    /// renames without paying the clone cost of the full Vec<String>.
    spoke_signature: u64,
    /// Diameter in integer pixels — paired with `size_px` to cover
    /// every input to `center()` / `radius`.
    diameter_px: u64,
}

impl WheelCacheFp {
    /// True when the two fingerprints paint the *same pixels*, possibly
    /// at a different place on screen — the predicate behind the pan
    /// fast-path: the disc rides the strip's canvas, so its origin
    /// changes on every frame of a horizontal scroll, and re-encoding
    /// the full disc for a pure translation is what made scrolling feel
    /// heavy.
    fn content_eq(&self, other: &Self) -> bool {
        self.size_px == other.size_px
            && self.rotation_centi_deg == other.rotation_centi_deg
            && self.active_idx == other.active_idx
            && self.spoke_count == other.spoke_count
            && self.spoke_signature == other.spoke_signature
            && self.diameter_px == other.diameter_px
    }

    /// Whether a scene has ever been recorded against this fingerprint.
    /// A zero-width footprint means the disc has not laid out yet.
    fn is_populated(&self) -> bool {
        self.size_px.0 > 0
    }
}

/// Fraction of the disc's radius that protrudes into the viewport:
/// `cx = rect.x0 - radius * (1 - VISIBLE)`. Does NOT change the
/// footprint — the visible width is the host's slot; this only sets
/// how tightly the visible sliver curves, and so how many degrees of
/// rim fit in the slot's height.
pub(crate) const VISIBLE_RADIUS_FRACTION: f64 = 0.14;

/// Fractional vertical inset applied at both the top and bottom of the
/// taffy-allocated slot, so the footprint reads as inset from the
/// topbar and floor rather than edge-to-edge — and it buys the band
/// depth: the band bends back toward centre to fit a tall narrow slot,
/// so height and depth trade off. At 0.10 the band came out 129 px, too
/// shallow for a workspace name; 0.16 costs 15% height for a fifth more
/// depth.
pub(crate) const SLOT_VERTICAL_INSET_FRAC: f64 = 0.16;

/// Concept's `DRAG_FACTOR` from `App.tsx:41` — degrees of rotation per
/// pixel of vertical drag.
const DRAG_FACTOR: f32 = 0.15;

/// Angular spacing between adjacent spokes; for `N` workspaces less
/// than the concept's 24 display-spokes, the wheel cycles `N` per
/// `SPOKE_ANGLE_STEP_DEG · N` of rotation.
const SPOKE_ANGLE_STEP_DEG: f32 = 5.5;

/// How many sectors span the visible band when there are at least this
/// many workspaces. Five puts the active sector in the middle with two
/// either side: enough to see where you are going without making any
/// one of them too small to carry a name.
const SECTORS_ACROSS_BAND: usize = 5;

/// Floor and ceiling on the derived spoke gap, in degrees.
///
/// The floor is a safety property, not a taste one: [`slot_reach`]
/// divides the visible sweep by the gap, so a gap approaching zero asks
/// for an unbounded number of slots. A degenerate band — a few pixels
/// tall, which any transient layout can produce — drives the gap to
/// nearly nothing and the slot loop to billions of iterations: the
/// process wedges at full CPU with nothing drawn.
const MIN_SPOKE_GAP_DEG: f32 = 1.5;
const MAX_SPOKE_GAP_DEG: f32 = 90.0;

/// Hard cap on slots either side of centre. No band needs this many;
/// it exists so a geometry nobody anticipated degrades into a wheel
/// that looks wrong rather than a compositor that stops responding.
const MAX_SLOT_REACH: i32 = 64;

/// Pixels of accumulated wheel scroll required to advance one
/// workspace. Away from the ends this is a discrete detent — the wheel
/// always lands on a workspace, never drifts mid-state. Against an end,
/// where there is no next detent to spend the scroll on, the same
/// input becomes resisted overscroll instead, so the control never
/// reads as dead.
const WHEEL_DETENT_PX: f32 = 48.0;

/// Idle gap after which any sub-detent residual `wheel_accumulator` is
/// cleared, so a partial swipe doesn't carry into the next scroll burst
/// and trigger an unintended switch. 250 ms outlasts a continuous flick
/// but reads a pause as deliberate.
const WHEEL_SCROLL_TIMEOUT_MS: u64 = 250;

/// Where the disc's vertical fade reaches full strength, as a fraction of
/// the footprint's height (mirrored at the bottom). Everything the disc
/// paints is faded on this curve, so the whole object dissolves together
/// instead of some parts out-living others.
///
/// Has to stay above `1 - BAND_TANGENT_FRACTION` so the tail the inner
/// arc gets clipped in is already fading by the time the clip bites.
const FADE_IN_STOP: f32 = 0.13;

/// How far past the footprint's right edge the rim's shadow is allowed to
/// reach. The disc's paint group is clipped this much wider so the shadow
/// can sit outside the rim and still be faded with everything else.
const RIM_SHADOW_REACH: f64 = 26.0;

/// Clear space between the footprint's left edge and the band's inner
/// arc at its closest approach. Keeps the arc off the edge so the band
/// visibly ends on its own curve rather than on the clip.
const BAND_LEFT_INSET: f64 = 12.0;

/// How far down the footprint the band's inner arc is allowed to come
/// closest to the left edge, as a fraction of half the footprint height.
///
/// Trades band depth against height: sizing so the arc passes exactly
/// through the footprint's corners spends the whole budget on height,
/// leaving the band too shallow for a name (at 1600x900, 109 px —
/// "Research" truncated to "Res…"). Pulling the tangent point in buys
/// depth back; the arc drifts into the clip only across the last few
/// per cent, where [`FADE_IN_STOP`] has already faded the band nearly
/// to nothing, so read this together with `FADE_IN_STOP`.
const BAND_TANGENT_FRACTION: f64 = 0.93;

/// Clearance between the rim and the outer end of a sector's label, and
/// between the inner arc and the label's inner end. The two together set
/// the chip's width, which is the same for every sector because the band
/// has a constant radial depth — so the labels form a ring rather than a
/// ragged column.
const LABEL_RIM_GAP: f64 = 12.0;
const LABEL_INNER_INSET: f64 = 12.0;

/// Narrowest a sector's chip may get before the sector shows no label at
/// all. Below this even a truncated name is an ellipsis and a letter,
/// which says less than the empty sector does.
const LABEL_MIN_W: f64 = 62.0;

/// Height of an inactive sector's chip; the active sector's is
/// [`SECTOR_CHIP_H_ACTIVE`], one step larger so the set reads as one
/// family in two states.
///
/// Sized against the slot, not in the abstract: a slot caps near 330 px
/// so three fit the screen height at once, and a small chip inside that
/// reads as a tag adrift in an empty wedge. Fix the chip, not the slot
/// — widening the slot to fit a bigger chip pushes the third workspace
/// off screen.
const SECTOR_CHIP_H: f64 = 48.0;

/// Height of the active sector's chip. See [`SECTOR_CHIP_H`].
const SECTOR_CHIP_H_ACTIVE: f64 = 64.0;

/// Fill strength of the active sector's wedge, as a factor on the
/// workspace's own colour. Enough that the wedge is unmistakably the lit
/// one, quiet enough that the name on top of it stays the brightest thing
/// in the sector.
const ACTIVE_WEDGE_ALPHA: f32 = 0.17;

/// The same fill where the body beneath it is fully opaque.
///
/// The wedge is a wash *over* the body: 17% reads clearly on the usual
/// translucent glass fill, but is barely a colour on the high-contrast
/// track's fully opaque surface — the one track where a state indicator
/// failing to indicate is least acceptable. Interpolated on the body's
/// own opacity rather than switched per theme, so any future
/// solid-surface theme gets the right answer for free.
const ACTIVE_WEDGE_ALPHA_ON_OPAQUE: f32 = 0.55;


/// Diameter of the add-workspace button, and how far down the band it
/// sits as a fraction of half the footprint's height. Sector labels that
/// would collide with it are dropped, so the affordance is never covered
/// however far the wheel has been turned.
const ADD_BUTTON_D: f64 = 34.0;
const ADD_BAND_FRACTION: f64 = 0.92;

/// Angular slop past the last on-screen degree that sectors are still
/// built for. Cheap insurance that a divider or a tint never stops short
/// of the footprint's corner.
const SWEEP_SLOP_DEG: f64 = 8.0;

impl SidebarWheelDisc {
    /// Visible width — `radius * VISIBLE_RADIUS_FRACTION`, matching the
    /// live `SIDEBAR_WIDTH_PX` slot.
    pub fn visible_width_px(&self) -> f64 {
        (self.diameter_px / 2.0) * VISIBLE_RADIUS_FRACTION
    }

    /// Construct a fresh disc with the given spoke labels, at rest
    /// (`active_idx = 0`, `rotation_deg = 0.0`). `diameter_px` starts at
    /// a placeholder and is recomputed from the host slot on first
    /// layout — see `update`.
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
    /// with `None`.
    ///
    /// A summoned disc keeps its footprint and its geometry; only where
    /// it sits changes. See [`SidebarWheelDisc::summon_at`].
    pub fn set_summon(&mut self, at: Option<Point>) {
        self.summon_at = at;
    }

    /// Whether the disc is currently summoned away from its dock.
    pub fn is_summoned(&self) -> bool {
        self.summon_at.is_some()
    }

    /// Send a summoned disc back to its dock, clearing the shared
    /// chrome flag so the overlay host re-anchors it to the strip's pan
    /// on the next layout pass.
    pub fn dismiss_summon(&mut self) {
        self.summon_at = None;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().set_wheel_summon(None);
        }
    }

    /// Attach a shared chrome handle. After a snap commits a new
    /// `active_idx`, the disc writes the new active workspace back to
    /// `chrome.tree` via `set_active` (mirrors `SidebarWheelChrome`).
    pub fn with_chrome(mut self, chrome: Rc<RefCell<ChromeState>>) -> Self {
        self.chrome = Some(chrome);
        self
    }

    /// Override the disc diameter (rare — tests use this to validate
    /// layout math without depending on the placeholder default).
    pub fn with_diameter(mut self, d: f64) -> Self {
        self.diameter_px = d;
        self
    }

    /// Builder-style override for the initially-active spoke index. Used
    /// by the live-shell wiring to surface the tree's currently-active
    /// workspace under the indigo glow.
    ///
    /// Sets the rotation to match. `active_idx` and `rotation_deg` are
    /// two views of one position, and a disc built with the index moved
    /// but the rotation still at zero would paint its fan `idx` sectors
    /// out of place.
    pub fn with_active_idx(mut self, idx: usize) -> Self {
        self.active_idx = idx.min(self.spokes.len().saturating_sub(1));
        self.rotation_deg = self.rotation_for_idx(self.active_idx);
        self
    }

    /// Which workspace, if any, occupies each visible slot, indexed by
    /// `k + slots` for `k` in `-slots..=slots`.
    ///
    /// Sectors wrap around the active one so the disc stays balanced on
    /// both sides. Wrapping alone isn't enough when the sweep holds more
    /// slots than workspaces — the naive wrap draws a second copy of a
    /// workspace no click can reach — so each workspace is claimed at
    /// most once, nearest slot first, and any surplus slot stays empty.
    fn slot_indices(&self, slots: i32) -> Vec<Option<usize>> {
        let n = self.spokes.len();
        let mut out = vec![None; (slots * 2 + 1).max(0) as usize];
        if n == 0 {
            return out;
        }
        let mut claimed = vec![false; n];
        // Nearest the active sector claims first, so when there are more
        // slots than workspaces the empties fall at the outer edges
        // where the band is fading out anyway.
        let mut order: Vec<i32> = (-slots..=slots).collect();
        order.sort_by_key(|k| k.abs());
        for k in order {
            let idx = (self.active_idx as i32 + k).rem_euclid(n as i32) as usize;
            if claimed[idx] {
                continue;
            }
            claimed[idx] = true;
            out[(k + slots) as usize] = Some(idx);
        }
        out
    }

    /// Degrees between adjacent spokes, derived from the band the disc
    /// was actually laid out into.
    ///
    /// A fixed angle can't be right at more than one window size — arc
    /// length per degree grows with radius, which is derived from the
    /// slot's width — so the band's own angular height is divided
    /// instead, across up to [`SECTORS_ACROSS_BAND`] sectors (fewer when
    /// there are fewer workspaces).
    pub fn spoke_gap_deg(&self) -> f32 {
        let n = self.spokes.len();
        // Single-workspace disc is treated as 360° to avoid the wheel
        // scrolling past itself unnecessarily.
        if n <= 1 {
            return 360.0;
        }
        let Some(span) = self.band_angular_height_deg() else {
            // No layout yet — early frames and hand-built test discs.
            return SPOKE_ANGLE_STEP_DEG;
        };
        let gap = span / n.min(SECTORS_ACROSS_BAND) as f32;
        if gap.is_finite() {
            gap.clamp(MIN_SPOKE_GAP_DEG, MAX_SPOKE_GAP_DEG)
        } else {
            SPOKE_ANGLE_STEP_DEG
        }
    }

    /// How much angle the visible band covers, or `None` before the
    /// first layout pass has given the disc a real rect.
    fn band_angular_height_deg(&self) -> Option<f32> {
        let half_h = self.rect.height() / 2.0 * BAND_TANGENT_FRACTION;
        let radius = self.diameter_px / 2.0;
        if half_h <= 1.0 || radius <= 1.0 {
            return None;
        }
        let ratio = (half_h / radius).clamp(-1.0, 1.0);
        Some((2.0 * ratio.asin().to_degrees()) as f32)
    }

    /// Degrees-per-pixel drag coefficient. Concept-faithful
    /// `DRAG_FACTOR = 0.15` (App.tsx:41).
    fn deg_per_pixel(&self) -> f32 {
        DRAG_FACTOR
    }

    /// The absolute rotation at which workspace `idx` sits at the middle
    /// of the visible arc. Workspaces run down the wheel in list order,
    /// so later ones sit at more negative rotations.
    fn rotation_for_idx(&self, idx: usize) -> f32 {
        -(idx as f32) * self.spoke_gap_deg()
    }

    /// The rotations the wheel can actually rest at, as `(last, first)`.
    /// Anything outside is overscroll.
    fn rotation_bounds(&self) -> (f32, f32) {
        let last = self.spokes.len().saturating_sub(1);
        (self.rotation_for_idx(last), 0.0)
    }

    /// Which workspace a rotation puts at the middle of the arc.
    /// Clamped, because a rotation past an end still has to name a real
    /// workspace — the one you are pulling away from.
    fn idx_at_rotation(&self, rot: f32) -> usize {
        let n = self.spokes.len();
        if n == 0 {
            return 0;
        }
        let steps = (-rot / self.spoke_gap_deg()).round();
        (steps.clamp(0.0, (n - 1) as f32) as usize).min(n - 1)
    }

    /// How far past an end the wheel may be pulled, asymptotically.
    ///
    /// Measured against `SPOKE_ANGLE_STEP_DEG` rather than
    /// `spoke_gap_deg()` on purpose: the single-workspace disc widens
    /// its gap to a full turn so it cannot scroll past its only entry,
    /// and scaling the give by that would let it be dragged most of a
    /// revolution.
    fn overscroll_reach_deg(&self) -> f32 {
        Gesture::default().overscroll_reach_frac * SPOKE_ANGLE_STEP_DEG
    }

    /// Replace whatever part of `rot` lies past an end with its
    /// resisted equivalent. In-range rotations pass through untouched.
    fn resist_past_ends(&self, rot: f32) -> f32 {
        let (lo, hi) = self.rotation_bounds();
        let reach = self.overscroll_reach_deg();
        if rot > hi {
            hi + rubber_band(rot - hi, reach)
        } else if rot < lo {
            lo - rubber_band(lo - rot, reach)
        } else {
            rot
        }
    }

    /// Corrective pull toward the nearest detent, for a wheel currently
    /// at `rot`.
    ///
    /// A sine of the sub-detent phase: zero exactly on a detent (a
    /// resting wheel isn't pushed off it), zero exactly between two (a
    /// real, if unstable, balance point), steepest a quarter-way in
    /// either direction (crossing a boundary feels like a give-then-catch
    /// rather than a slide). Suppressed past the ends, where the rubber
    /// band is the only force that should be pulling back in.
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
    /// nearest right now.
    ///
    /// This is the live stepping. It reads the visual rotation, not the
    /// target, on purpose: with the disc trailing the pointer through a
    /// spring, the two differ by the follow lag, and the notch should
    /// light when the wheel passes it rather than when the hand does.
    /// Deliberately does NOT write to the workspace tree — a gesture in
    /// flight has not chosen anything yet.
    fn sync_active_from_visual(&mut self) {
        if self.spokes.is_empty() {
            return;
        }
        self.active_idx = self.idx_at_rotation(self.visual_rotation_deg());
    }

    /// Chase `to` with the stiff follow spring — the drag phase.
    ///
    /// Reuses whatever spring is already in flight, swapping only its
    /// constants, so grabbing a settling wheel hands its velocity to the
    /// drag rather than stopping it dead.
    fn aim_follow_spring(&mut self, from_hint: f32, to: f32) {
        let params = SpringPresets::default().tracking;
        match self.rotation_spring.as_mut() {
            Some(s) => {
                s.set_params(params);
                s.set_target(to);
            }
            None => {
                let mut s = Spring::new(from_hint, params);
                s.set_target(to);
                self.rotation_spring = Some(s);
            }
        }
    }

    /// Ease to `to` with the heavy settle spring, keeping the value and
    /// velocity of any spring already running.
    ///
    /// Velocity carries into the heavier spring so a flick coasts and
    /// decelerates instead of snapping. `from_hint` — the wheel's visible
    /// position before the caller moved the target — is load-bearing
    /// whenever no spring survives the previous phase: seeding a fresh
    /// spring from the already-updated `rotation_deg` would start it on
    /// its destination and report settled in one invisible frame.
    fn aim_settle_spring(&mut self, from_hint: f32, to: f32) {
        let params = SpringPresets::default().heavy;
        match self.rotation_spring.as_mut() {
            Some(s) => {
                s.set_params(params);
                s.set_target(to);
            }
            None => {
                let mut s = Spring::new(from_hint, params);
                s.set_target(to);
                self.rotation_spring = Some(s);
            }
        }
        self.settle_origin = self.rotation_spring.clone();
    }

    /// Logical centre of the disc in screen coordinates. The centre sits
    /// `radius * (1 - VISIBLE_RADIUS_FRACTION)` pixels LEFT of `rect.x0`,
    /// so the rim crosses the footprint's left edge at
    /// `±arccos(1 - VISIBLE_RADIUS_FRACTION)` from due-east, with the
    /// slot's height deciding how much of that arc is actually on screen.
    fn center(&self) -> Point {
        let radius = self.diameter_px / 2.0;
        let cx = self.rect.x0 - radius * (1.0 - VISIBLE_RADIUS_FRACTION);
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        Point::new(cx, cy)
    }

    /// The workspace's identifying colour, derived from the theme.
    ///
    /// Theming in qOS belongs to the user, so this can't be a literal
    /// hardcoded palette — [`ui::tokens::Colors::categorical`] grows the
    /// series from the theme's own accent, keeping adjacent workspaces a
    /// plain 60° of hue apart.
    fn spoke_color(&self, idx: usize, colors: &ui::tokens::Colors) -> Color {
        colors.categorical(idx)
    }

    /// How large a size step the active sector's chip gets, given the
    /// width the band actually has for it.
    ///
    /// The chip's width is fixed by geometry and can't widen inward — a
    /// chip reaching further in swings out through the footprint's left
    /// edge once its sector rotates off centre. At the full step a long
    /// name doesn't fit and gets truncated (the live shell's default
    /// workspace rendered as `Resear…`), so this takes the largest step
    /// the name does fit in instead, keeping active/inactive chips one
    /// family in two states rather than trading the name away for size.
    ///
    /// Returns [`SECTOR_CHIP_H`] when even the inactive size overflows.
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
        let mut step = SECTOR_CHIP_H;
        while step <= SECTOR_CHIP_H_ACTIVE {
            let need = Chip::natural_width_at(&spoke.text, step, lead, theme, text_renderer);
            if need <= chip_w {
                chosen = step;
            }
            step += 2.0;
        }
        chosen
    }

    /// The width available to a sector's label chip, in pixels. Exposed
    /// alongside [`SidebarWheelDisc::active_chip_height`] so a caller
    /// can ask the same question `draw` asks.
    pub fn label_chip_width(&self) -> f64 {
        let (r_ref, r_out) = self.label_band_radii();
        (r_out - LABEL_RIM_GAP) - (r_ref + LABEL_INNER_INSET)
    }

    /// Pick a lucide icon for the spoke at `idx`, matching the
    /// `radial-workspace-switcher/src/App.tsx:10-17` mapping by lower-
    /// cased label. Unknown labels default to `Layers`. Public for the
    /// snapshot tests that assert per-workspace glyph fidelity.
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

    /// The workspace the wheel has **committed** to: where it is headed
    /// and what has been written through to the tree.
    ///
    /// Distinct from [`SidebarWheelDisc::active_idx`], the *indicator*
    /// that tracks wherever the disc visibly is. The two are equal at
    /// rest and differ only while a gesture or its settle is in flight —
    /// the highlight steps across the list under the user's hand while
    /// the tree is written once, at the end.
    pub fn selected_idx(&self) -> usize {
        self.idx_at_rotation(self.rotation_deg)
    }

    /// The visible rotation expressed relative to the active sector's
    /// rest position — what `draw` turns the fan by.
    ///
    /// The fan is built outward from the active sector, so it needs the
    /// offset from that sector's home; feeding it the absolute angle
    /// instead would send the lit sector marching one notch further down
    /// the arc with every workspace switched, until it left the slot.
    ///
    /// At rest this is zero; during a gesture it ranges roughly ±half a
    /// detent before the indicator steps and wraps back, further only
    /// when held past an end.
    pub fn visual_residual_deg(&self) -> f32 {
        self.visual_rotation_deg() - self.rotation_for_idx(self.active_idx)
    }

    /// Snapshot every input that affects the disc's painted pixels.
    /// Used by [`Widget::draw`] to decide whether `self.cached_scene`
    /// can be reused. See [`WheelCacheFp`] for the field semantics.
    fn current_fingerprint(&self) -> WheelCacheFp {
        let origin_px = (self.rect.x0.round() as i64, self.rect.y0.round() as i64);
        let size_px = (
            self.rect.width().round() as u64,
            self.rect.height().round() as u64,
        );
        // 0.01° granularity — fine enough to invalidate on every
        // visible animation tick (the spring steps at 60 Hz with
        // sub-degree per-step deltas during the settling phase),
        // coarse enough to ignore float-precision noise at rest.
        let rotation_centi_deg = (self.visual_rotation_deg() as f64 * 100.0).round() as i64;
        // FNV-1a over the concatenated spoke labels. Avoids cloning
        // the Vec<SpokeLabel> for the fingerprint while still
        // detecting renames.
        let mut h: u64 = 0xcbf29ce484222325;
        for spoke in &self.spokes {
            for b in spoke.text.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            // Separator byte so "ab"+"c" and "a"+"bc" hash differently.
            h ^= 0xff;
            h = h.wrapping_mul(0x100000001b3);
        }
        WheelCacheFp {
            origin_px,
            size_px,
            rotation_centi_deg,
            active_idx: self.active_idx,
            spoke_count: self.spokes.len(),
            spoke_signature: h,
            diameter_px: self.diameter_px.round() as u64,
        }
    }

    /// True iff `p` falls inside the **visible** portion of the disc's
    /// rim — i.e. inside the rect's footprint (so we never grab clicks
    /// that landed off-screen-left of the viewport edge) AND within the
    /// circle's radius. Corner pixels of the rect that aren't actually
    /// covered by the arc are correctly rejected.
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

/// How many display slots either side of the active one the sector fan
/// has to run to cover `sweep` radians, plus one so a sector entering the
/// footprint is already drawn when its leading divider appears.
///
/// Returning 0 for a gap of half a turn or more is the single-workspace
/// case: `spoke_gap_deg` widens to 360° there so the wheel cannot scroll
/// past its only entry, and one slot is the whole disc.
fn slot_reach(sweep_rad: f64, gap_deg: f64) -> i32 {
    // A non-positive or non-finite gap has no meaningful slot count, and
    // dividing by it saturates the cast to `i32::MAX` — which the caller
    // then walks, one slot at a time.
    if !gap_deg.is_finite() || gap_deg <= 0.0 || gap_deg >= 180.0 {
        return 0;
    }
    let reach = (sweep_rad.to_degrees() / gap_deg).ceil();
    if !reach.is_finite() {
        return 0;
    }
    (reach as i32).saturating_add(1).clamp(0, MAX_SLOT_REACH)
}

/// Point on the circle of radius `r` about `c` at `angle` radians,
/// measured from due-east and increasing downward (screen coordinates).
fn polar(c: Point, r: f64, angle: f64) -> Point {
    Point::new(c.x + r * angle.cos(), c.y + r * angle.sin())
}

/// A **solid** segment of the disc: the pie from the centre out to
/// `r_out` between `a0` and `a1`.
///
/// Distinct from [`annular_segment`] rather than that function called
/// with `r_in = 0`, which would emit 97 coincident points at the centre
/// for every sector of every frame.
fn disc_segment(c: Point, r_out: f64, a0: f64, a1: f64) -> BezPath {
    const STEPS: usize = 96;
    let mut path = BezPath::new();
    let span = a1 - a0;
    path.move_to(c);
    for i in 0..=STEPS {
        path.line_to(polar(c, r_out, a0 + span * (i as f64 / STEPS as f64)));
    }
    path.close_path();
    path
}

/// Closed path for the slice of ring between radii `r_in`/`r_out` and
/// angles `a0`/`a1`.
///
/// The arcs are walked as polylines rather than built from `kurbo::Arc`
/// so the two ends join into ONE closed subpath — a single winding,
/// filled flat with no seam where the boundaries meet. Step count keeps
/// the chord error far below a pixel: the sweep is at most a few tens
/// of degrees, so 96 steps put the sagitta under 0.05 px even on a
/// 2000-px disc.
fn annular_segment(c: Point, r_in: f64, r_out: f64, a0: f64, a1: f64) -> BezPath {
    const STEPS: usize = 96;
    let mut path = BezPath::new();
    let span = a1 - a0;
    path.move_to(polar(c, r_out, a0));
    for i in 1..=STEPS {
        path.line_to(polar(c, r_out, a0 + span * (i as f64 / STEPS as f64)));
    }
    for i in 0..=STEPS {
        path.line_to(polar(c, r_in, a1 - span * (i as f64 / STEPS as f64)));
    }
    path.close_path();
    path
}

/// Map a workspace label to its lucide glyph. Mirrors `WORKSPACES` in
/// `radial-workspace-switcher/src/App.tsx`, extended with `Home` (the
/// first-boot workspace) and `Research` (the demo tree) so those spokes
/// get a recognisable glyph instead of the generic `Layers` fallback.
/// Inputs are expected lower-cased; matches are exact ASCII, so an
/// upstream label rename here silently falls back to `Layers`.
/// `dev`/`development` both resolve to `Code` (concept id vs display
/// label).
fn spoke_icon_for_label(label: &str) -> Icon {
    match label {
        "design" => Icon::PenTool,
        "dev" | "development" => Icon::Code,
        "marketing" => Icon::FileText,
        "analytics" => Icon::BarChart,
        "concept" => Icon::Layers,
        "settings" => Icon::Settings,
        "home" => Icon::Home,
        "research" => Icon::Search,
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
        // Leaf reserves the visible-arc width (`radius * VISIBLE`) and
        // takes 100% of the parent slot's height. The disc's centre sits
        // off-screen-left and its visible arc spans most of the viewport
        // vertically — concept-faithful "huge wheel" feel.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(self.visible_width_px() as f32),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 0.0,
                flex_shrink: 0.0,
                align_self: Some(AlignItems::Stretch),
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Pick up a summon raised elsewhere in the chrome (the keyboard
        // chord lives on the overlay host, which has the pointer). The
        // widget keeps its own copy so unit tests and any non-chrome
        // host can drive it directly through `set_summon`.
        if let Some(chrome) = self.chrome.as_ref() {
            self.summon_at = chrome.borrow().wheel_summon();
        }
        let l = taffy.layout(node).unwrap();
        // `rect` is the visible footprint inside the sidebar column.
        // The disc's logical centre lives off-screen-left of `rect.x0`;
        // hit-testing (`hit_visible_arc`) excludes the rect's corners
        // that the arc doesn't actually cover.
        //
        // Trimmed vertically by `SLOT_VERTICAL_INSET_FRAC` at both ends
        // so the wheel reads as inset rather than edge-to-edge.
        let x0 = origin.x + l.location.x as f64;
        let y0_raw = origin.y + l.location.y as f64;
        let h_raw = l.size.height as f64;
        let inset_v = h_raw * SLOT_VERTICAL_INSET_FRAC;
        let w = l.size.width as f64;
        let y0 = y0_raw + inset_v;
        let y1 = y0_raw + h_raw - inset_v;
        self.rect = match self.summon_at {
            // Docked: the slot's own box, already shifted by the active
            // strip's pan.
            None => Rect::new(x0, y0, x0 + w, y1),
            // Summoned: same size, re-placed so the rim's rightmost
            // point (`rect.x1`, where the active marker rides) sits
            // under the pointer, centred vertically on it. Clamped back
            // inside the slot's band so a summon near an edge slides
            // into view instead of hanging off it.
            Some(p) => {
                let slot_top = y0_raw;
                let slot_bottom = y0_raw + h_raw;
                let height = y1 - y0;
                let x1 = p.x.max(origin.x + w);
                let top = (p.y - height / 2.0).clamp(slot_top, (slot_bottom - height).max(slot_top));
                Rect::new(x1 - w, top, x1, top + height)
            }
        };
        // The host overrides the sidebar slot to a viewport-driven
        // width, so recompute `diameter_px` from rect.width() / VISIBLE
        // on every layout pass, keeping the visual arc-fraction fixed
        // regardless of slot width.
        let rw = self.rect.width();
        if rw > 1.0 {
            self.diameter_px = rw / VISIBLE_RADIUS_FRACTION * 2.0;
        }
        // Re-anchor the wheel on the workspace it is showing: the gap
        // this pass just recomputed changes what workspace the stored
        // `rotation_deg` names, so anchor to the index instead — the
        // thing that actually has to survive a resize.
        //
        // Only while at rest: re-anchoring mid-gesture or mid-settle
        // would keep pulling the target back onto wherever the
        // indicator has reached, so a drag would give ground as you
        // made it and a settle would never arrive.
        let at_rest = self.drag_from.is_none()
            && self
                .rotation_spring
                .as_ref()
                .map(|s| s.is_settled())
                .unwrap_or(true);
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
        // Cache fast-path: the disc paints a lot per frame (shadow
        // bands, disc fill, rim strokes, every spoke's cluster, the
        // add-workspace slot) and at rest none of it changes
        // frame-to-frame. Fingerprint every input that affects the
        // painted pixels and replay the cached scene on a hit; during
        // an animation every frame still rebuilds. `spoke_hit_rects`
        // and `plus_rect` are valid on a hit because the prior
        // cache-miss draw populated them at the same rotation/rect.
        let fp = self.current_fingerprint();
        // Two fast paths, both requiring a previously recorded scene:
        // (1) identical fingerprint — replay verbatim; (2) same content
        // at a new origin — the disc rides the strip's canvas, so a pan
        // moves it without changing a painted pixel, replayed under a
        // translation instead. The delta is measured from the origin
        // the scene was recorded at, not the last frame's, and hit
        // rects are re-derived from the same delta so a click after a
        // pan doesn't test against stale positions.
        if fp.is_populated() && self.cached_fp.is_populated() {
            if fp == self.cached_fp {
                out_scene.append(&self.cached_scene, Some(out_transform));
                return;
            }
            if fp.content_eq(&self.cached_fp) {
                let dx = (fp.origin_px.0 - self.cached_fp.origin_px.0) as f64;
                let dy = (fp.origin_px.1 - self.cached_fp.origin_px.1) as f64;
                let shift = Affine::translate((dx, dy));
                out_scene.append(&self.cached_scene, Some(out_transform * shift));
                self.retarget_hit_rects(fp.origin_px);
                return;
            }
        }

        // Cache miss — encode at IDENTITY into `self.cached_scene`,
        // then splice with `out_transform`. Same `std::mem::take`
        // dance as the other shell widgets so the encode body has an
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
            // — we don't want a zero-width rect to lock the cache.
            self.cached_scene = local;
            return;
        }
        let c = self.center();
        let cx = c.x;
        let cy = c.y;

        // ---- Geometry ---------------------------------------------------
        // The visible object is an annular BAND: a constant-depth ring
        // between an inner arc and the rim, as deep as it can be while
        // keeping the inner arc inside the footprint everywhere it's on
        // screen — any deeper and it runs out through the left edge on
        // a straight cut instead of a curve.
        let half_h = self.rect.height() / 2.0;
        let r_out = radius;
        // The inner arc's sweep runs further than the rim's — the rim
        // exits top/bottom at `theta_lim`, but the arc is still
        // climbing there and only matches that height further round.
        // `sqrt(d^2 + half_h^2)` is the radius whose arc passes exactly
        // through the footprint's left-hand corners — as deep as the
        // band can be while still ending on a curve.
        let d_left = (self.rect.x0 - cx + BAND_LEFT_INSET).max(1.0);
        // The body reaches the centre, so there's no inner radius to
        // derive from — `r_ref` is kept as a pure layout reference
        // (where labels and the add-workspace button live) so the solid
        // fill doesn't move anything else on the disc.
        let tangent_dy = half_h * BAND_TANGENT_FRACTION;
        let r_ref = (d_left * d_left + tangent_dy * tangent_dy)
            .sqrt()
            .clamp(4.0, r_out - 12.0);
        // How far round the disc must be built for the footprint to be
        // covered: a radial at angle θ crosses the left edge at height
        // `d_left·tan θ`, so `atan(half_h / d_left)` is the angle whose
        // radial passes exactly through the footprint's corner, plus
        // slop so an entering divider is already drawn.
        let sweep = (half_h / d_left).atan() + SWEEP_SLOP_DEG.to_radians();

        // ---- Pass 1: the band -------------------------------------------
        // Body, edges and the drop shadow are ONE isolated group, painted
        // at full strength, faded out top/bottom by a single mask at the
        // end — the fade is what lets the band read as a slice rather
        // than end on a hard horizontal cut where it enters/leaves the
        // tall footprint.
        //
        // The horizontal clip IS load-bearing: an unclipped ring would
        // sweep back around off-screen-left and reappear nowhere useful;
        // it runs a little past the footprint's right edge so the rim's
        // outward shadow has somewhere to land.
        //
        // The fill is FLAT — one colour, the theme's own glass surface,
        // the same token the modal surfaces use, so the wheel reads as
        // the same material as the rest of the chrome.
        let body_hue = colors.surfaces.glass;
        // Edges track the theme's foreground rather than a hardcoded
        // white — a hardcoded white is a scratch under the light theme
        // and vanishes under high contrast, where white-tinted border
        // tokens collapse to near-invisible.
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
                ColorStop { offset: 0.0, color: Color::from_rgba8(255, 255, 255, 0).into() },
                ColorStop { offset: FADE_IN_STOP, color: Color::WHITE.into() },
                ColorStop { offset: 1.0 - FADE_IN_STOP, color: Color::WHITE.into() },
                ColorStop { offset: 1.0, color: Color::from_rgba8(255, 255, 255, 0).into() },
            ][..],
        );

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &group_clip,
        );

        // Shadow just outside the rim, bent around a circle — same soft
        // multi-ring falloff as the window-decoration pills. Painted
        // first so the band sits on top. Tinted from the theme's
        // elevation token rather than a literal black, so a theme that
        // suppresses shadows (high contrast, pure black behind a white
        // rim) gets its own answer.
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
        // Display slots tile the whole visible sweep at a fixed angular
        // step and CYCLE through the workspaces, exactly as the concept's
        // 24 slots did for its 6 workspaces. Indexing slots off the
        // workspace list instead left a stretch of unsegmented band
        // rotating through whenever there were fewer workspaces than the
        // sweep has room for — a wheel with a bald patch.
        //
        // Every sector is drawn on the band and rotates with it. That is
        // the whole design: the dividers, the active tint and the labels
        // all sweep together, so what you see turning is the object, not
        // a set of markers sliding along a fixed curve.
        self.spoke_hit_rects.clear();
        self.spoke_hit_rects.resize(self.spokes.len(), Rect::ZERO);
        // Which slot won each workspace's hit rect, by absolute angle —
        // a workspace can appear more than once in the sweep when there
        // are very few of them, and a click should go to the one nearest
        // the middle of the slot rather than to whichever was drawn last.
        let mut hit_claim = vec![f64::INFINITY; self.spokes.len()];

        // The add-workspace button's box is settled before any sector is
        // placed, so labels can be kept out of it. Placed by how far
        // down the band it sits rather than up from the footprint's
        // floor — the floor is inside the vertical fade, which would
        // leave the button hanging under the band on bare wallpaper.
        let add_cy = cy + half_h * ADD_BAND_FRACTION;
        let add_dy = add_cy - cy;
        // Tucked against the band's inner arc rather than mid-band:
        // labels run the band's full radial depth, so a button parked
        // mid-band sits in the path of whichever sector sweeps over it.
        let add_span_in = (r_ref * r_ref - add_dy * add_dy).max(0.0).sqrt();
        let add_cx = cx + add_span_in + ADD_BUTTON_D / 2.0 + 8.0;
        self.plus_rect = Rect::from_center_size(
            Point::new(add_cx, add_cy),
            (ADD_BUTTON_D, ADD_BUTTON_D),
        );

        // The same curve the band is masked with, so a label dissolves
        // exactly where the surface beneath it does instead of out-living
        // it and ending up a crisp pill on bare wallpaper.
        let row_fade = |y: f64| -> f32 {
            let h = self.rect.height();
            if h <= 0.0 {
                return 0.0;
            }
            let t = ((y - self.rect.y0) / h).clamp(0.0, 1.0) as f32;
            if t < FADE_IN_STOP {
                t / FADE_IN_STOP
            } else if t > 1.0 - FADE_IN_STOP {
                (1.0 - t) / FADE_IN_STOP
            } else {
                1.0
            }
        };

        if !self.spokes.is_empty() {
            let n = self.spokes.len();
            let n_i = n as i32;
            let gap = self.spoke_gap_deg() as f64;
            let rot = self.visual_residual_deg() as f64;
            let slots = slot_reach(sweep, gap);
            let body_opacity = body_hue.components[3].clamp(0.0, 1.0);
            let wedge_alpha = ACTIVE_WEDGE_ALPHA
                + (ACTIVE_WEDGE_ALPHA_ON_OPAQUE - ACTIVE_WEDGE_ALPHA) * body_opacity;

            let slot_idx = self.slot_indices(slots);
            for k in -slots..=slots {
                let mid_deg = rot + k as f64 * gap;
                let mid = mid_deg.to_radians();
                let a0 = mid - (gap / 2.0).to_radians();
                let a1 = mid + (gap / 2.0).to_radians();
                // Wholly outside the sweep — nothing of this sector can
                // land inside the footprint.
                if a0 > sweep || a1 < -sweep {
                    continue;
                }
                let Some(idx) = slot_idx[(k + slots) as usize] else {
                    continue;
                };
                let is_active = k == 0;
                let base = self.spoke_color(idx, colors);

                // Active sector: the wedge itself is lit in the
                // workspace's colour — the sector is the selection, so
                // the thing that turns is also the thing that shows
                // state.
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

                // Leading divider. Each sector draws only its own lower
                // bound, so adjacent sectors share one line rather than
                // stacking two translucent strokes into a brighter seam.
                let div_a = if is_active { 96 } else { 40 };
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    edge(div_a),
                    None,
                    // From the centre, not an inner arc: with the body
                    // solid, a divider stopping part-way would leave the
                    // surface tinted but undivided over the rest.
                    &Line::new(c, polar(c, r_out, a0)),
                );
                // The active sector also owns its upper bound, so the lit
                // wedge is bracketed by two bright lines instead of one.
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
        // Each label is a chip laid ON the disc: built in the sector's
        // own frame then rotated about the disc's centre to its sector,
        // so every chip sits at the same radius and width — a ring of
        // labels that tips with the wheel, reading as paint on the
        // surface rather than a list parked near a curve.
        let chip_x1 = cx + r_out - LABEL_RIM_GAP;
        let chip_x0 = cx + r_ref + LABEL_INNER_INSET;
        let chip_w = chip_x1 - chip_x0;
        // A band too shallow to carry names is a reason to drop the
        // names, not the sectors — sectors stay drawn and hit-tested,
        // falling back to their icon (what the chip does with an empty
        // name anyway).
        let names_fit = chip_w >= LABEL_MIN_W;
        if !self.spokes.is_empty() {
            let n = self.spokes.len();
            let n_i = n as i32;
            let gap = self.spoke_gap_deg() as f64;
            let rot = self.visual_residual_deg() as f64;
            let slots = slot_reach(sweep, gap);

            let slot_idx = self.slot_indices(slots);
            for k in -slots..=slots {
                let mid_deg = rot + k as f64 * gap;
                let mid = mid_deg.to_radians();
                if mid.abs() > sweep {
                    continue;
                }
                let Some(idx) = slot_idx[(k + slots) as usize] else {
                    continue;
                };
                let is_active = k == 0;
                // Where the sector's centreline meets the rim — the
                // label's own y, and what the vertical fade is sampled at.
                let anchor = polar(c, r_out, mid);
                let fade_a = row_fade(anchor.y);
                if fade_a <= 0.02 {
                    continue;
                }

                // The active sector's chip is a size step larger, up to
                // what its name fits — see `active_chip_height`.
                let lead = ChipLead::Glyph(self.spoke_icon(idx));
                let row_h = if is_active {
                    self.active_chip_height(idx, chip_w, theme, text_renderer)
                } else {
                    SECTOR_CHIP_H
                };
                let chip = Rect::new(chip_x0, cy - row_h / 2.0, chip_x1, cy + row_h / 2.0);
                let rotate = Affine::rotate_about(mid, c);

                // Where the chip actually lands once rotated. Used for the
                // click target and to keep labels off the add button.
                let placed = rotate.transform_rect_bbox(chip);
                // Does this label run into the add button? Answered in
                // the label's OWN frame — the button centre rotated back
                // onto the due-east radius, tested against the unrotated
                // chip grown by the button radius. The rotated chip's
                // axis-aligned bbox is far bigger than the chip itself
                // (at 15° a 160-px label picks up ~30 px of empty corner
                // each end), which would falsely clash with a button the
                // label passes clear of.
                let local = rotate.inverse() * self.plus_rect.center();
                let reach = ADD_BUTTON_D / 2.0 + 4.0;
                let clash = local.x >= chip.x0 - reach
                    && local.x <= chip.x1 + reach
                    && local.y >= chip.y0 - reach
                    && local.y <= chip.y1 + reach;
                if clash {
                    continue;
                }

                // The sector is the container, so the label brings no
                // pill of its own — a pill inside a lit wedge is a box
                // inside a box.
                //
                // The active label is primary text, a size step larger;
                // others take secondary text. Colouring the active label
                // with its own workspace hue put it on a wedge of that
                // same hue — the worst place for it, since identity is
                // already carried by the wedge.
                let style = if is_active {
                    ChipStyle::bare(None)
                } else {
                    // Quiet, but how quiet is the palette's call: dimming
                    // primary text by a fixed factor is wrong under high
                    // contrast, where secondary copy is deliberately kept
                    // near-white to clear AAA on black.
                    ChipStyle::bare(Some(colors.text_dim))
                }
                .with_alpha(fade_a);
                Chip::draw_at(
                    scene,
                    transform * rotate,
                    chip,
                    if names_fit {
                        self.spokes[idx].text.as_str()
                    } else {
                        ""
                    },
                    lead,
                    style,
                    theme,
                    text_renderer,
                );

                // Nearest slot wins the workspace's click target.
                if mid.abs() < hit_claim[idx] {
                    hit_claim[idx] = mid.abs();
                    self.spoke_hit_rects[idx] = placed;
                }

                // Per-sector presence dots, seated just inside the rim at
                // the sector's angle so they ride the surface with it.
                if let Some(ws_id) = self.spokes[idx].workspace_id {
                    let key = PresenceKey::Workspace {
                        id: format!("{}", ws_id),
                    };
                    let peers = presence.peers_on(&key);
                    if !peers.is_empty() {
                        let peer_count = peers.len().min(3);
                        let dot_r = r_out - LABEL_RIM_GAP / 2.0;
                        let spacing = 7.0_f64 / dot_r;
                        let centre_bias = (peer_count as f64 - 1.0) / 2.0;
                        for (kk, user) in peers.iter().take(peer_count).enumerate() {
                            let off = (kk as f64 - centre_bias) * spacing;
                            let (rr, gg, bb) = presence.color_for(user);
                            // Per-sector presence dot = empty-initials
                            // `ui::Avatar` chip in the peer's own colour, so
                            // the dot matches the cursor and the other
                            // avatar sites.
                            let p = polar(c, dot_r, mid + off);
                            let dot_rect =
                                Rect::new(p.x - 3.0, p.y - 3.0, p.x + 3.0, p.y + 3.0);
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
        // A round glass button seated on the band at the bottom of the
        // footprint, in the same material as the rim. It does NOT
        // rotate — it isn't a workspace, it's the control that makes
        // one, so a spot on the wheel would carry it out of reach every
        // turn.
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
            edge((74.0 * add_fade) as u8),
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
        // End of cache build — splice the freshly-encoded local scene
        // into the caller's `out_scene` with the host transform, then
        // restore `self.cached_scene` and record the fingerprint so
        // the next idle frame takes the fast path.
        out_scene.append(&local, Some(out_transform));
        self.cached_scene = local;
        // The hit rects were just re-derived at this origin, so the
        // pan fast-path measures its next shift from here.
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
            } => {
                // 1) Plus-slot first — outside the rim, must not be
                //    swallowed by the drag-start / spoke-click branches.
                if self.plus_rect.area() > 0.0
                    && self
                        .last_cursor_pos
                        .map(|p| self.plus_rect.contains(p))
                        .unwrap_or(false)
                {
                    if let Some(chrome) = self.chrome.as_ref() {
                        chrome.borrow_mut().add_workspace("Untitled");
                    }
                    *handled = true;
                    return;
                }

                // 2) Spoke-click (concept's `handleClickItem`) — if the
                //    press lands inside any non-active spoke's cluster
                //    rect, animate the wheel so that spoke becomes active
                //    rather than starting a free-running drag.
                if let Some(p) = self.last_cursor_pos {
                    for (i, hit) in self.spoke_hit_rects.iter().enumerate() {
                        if i == self.active_idx {
                            continue;
                        }
                        if hit.area() > 0.0 && hit.contains(p) {
                            self.click_to_spoke(i);
                            // Picking a workspace is the end of a
                            // summon: the wheel was called out to answer
                            // a question, and it has been answered. It
                            // stays up through a scroll or a drag, which
                            // are still browsing.
                            self.dismiss_summon();
                            *handled = true;
                            return;
                        }
                    }
                }

                // 3) Otherwise: start a rotation drag if the press lands
                //    inside the visible arc.
                if self.rect.area() <= 0.0 {
                    return;
                }
                let anchor = match (self.hover_anchor, self.last_cursor_pos) {
                    (Some(h), _) => h,
                    (None, Some(p)) => p,
                    (None, None) => Point::new(
                        (self.rect.x0 + self.rect.x1) / 2.0,
                        (self.rect.y0 + self.rect.y1) / 2.0,
                    ),
                };
                if self.hit_visible_arc(anchor) {
                    // Take the wheel over wherever it visually is: if a
                    // settle was running, grabbing it catches it
                    // mid-flight rather than teleporting to the settle's
                    // destination.
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
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor_pos = Some(p);
                if let Some(origin) = self.drag_from {
                    let dy = p.y - origin.y;
                    let delta_deg = (dy as f32) * self.deg_per_pixel();
                    // Where the pointer asks the wheel to be, before the
                    // wheel gets a say.
                    let raw = self.drag_rotation_origin + delta_deg;
                    self.drag_raw_deg = raw;
                    // The wheel's say: resistance where the list has run
                    // out, and a tug toward the nearer notch everywhere
                    // else. Both fold into the target rather than into
                    // the rendered value, so the spring is what the user
                    // actually feels them through.
                    let resisted = self.resist_past_ends(raw);
                    let from = self.visual_rotation_deg();
                    self.rotation_deg = resisted + self.detent_magnet(resisted);
                    let target = self.rotation_deg;
                    self.aim_follow_spring(from, target);
                    self.sync_active_from_visual();
                    *handled = true;
                } else if self.hit_visible_arc(p) {
                    // Cache the in-arc cursor for the next press hit-test
                    // without claiming pointer capture (which is reserved
                    // for an actual press-in-flight via `drag_from`).
                    self.hover_anchor = Some(p);
                }
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if self.drag_from.is_some() {
                    self.drag_from = None;
                    self.settle_after_drag();
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Only consume wheel events when the cursor is over the
                // disc, so scrolls over neighbouring widgets don't
                // accidentally rotate the wheel.
                if !self
                    .last_cursor_pos
                    .map(|p| self.hit_visible_arc(p))
                    .unwrap_or(false)
                {
                    return;
                }
                let dy = match *delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 16.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                // Discrete-detent model: every `WHEEL_DETENT_PX` worth
                // of accumulated scroll triggers a workspace advance via
                // `click_to_spoke`, so the visual is a spring settle,
                // not a teleport. At an end, where there's no next
                // workspace, the unspendable portion is diverted into
                // the same resisted pull a rim-drag gets.
                self.wheel_accumulator += dy;
                self.wheel_last_event = Some(Instant::now());
                let n = self.spokes.len();
                if n > 1 {
                    while self.wheel_accumulator.abs() >= WHEEL_DETENT_PX {
                        let dir: i32 = if self.wheel_accumulator < 0.0 { 1 } else { -1 };
                        let target = self.active_idx as i32 + dir;
                        // Drain toward zero: `dir` is the direction of
                        // *travel*, the opposite sign to the
                        // accumulator, so spending a detent ADDS one
                        // detent's worth back. Subtracting instead drives
                        // the accumulator further from zero every pass,
                        // and the loop waiting for it to fall under the
                        // threshold never ends — any scroll of a full
                        // detent or more hangs the compositor.
                        self.wheel_accumulator += dir as f32 * WHEEL_DETENT_PX;
                        if (0..n as i32).contains(&target) {
                            self.click_to_spoke(target as usize);
                        } else {
                            self.wheel_overscroll_px -= dir as f32 * WHEEL_DETENT_PX;
                        }
                    }
                }
                self.apply_wheel_overscroll();
                *handled = true;
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag_from.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the redraw pump running while a post-release snap is
        // easing, a wheel-scroll snap-back is pending, or a drag is in
        // flight.
        //
        // The drag arm makes the rim track the pointer: a drag is a
        // stream of `CursorMoved`, and the host's incremental gate skips
        // the redraw on a pointer move unless something reports work to
        // do. Without it `rotation_deg` advances every move but no
        // frame is requested, so the wheel sits still under the finger
        // and jumps once released.
        let spring_active = self
            .rotation_spring
            .as_ref()
            .map(|s| !s.is_settled())
            .unwrap_or(false);
        spring_active || self.wheel_last_event.is_some() || self.drag_from.is_some()
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Real (clamped) frame dt, so the spring stays wall-clock-correct
        // under frame drops instead of running slow on a fixed 1/60.
        // Clamp to 1/30 s: semi-implicit Euler stays stable, and a long
        // stall just resumes rather than exploding.
        let now = std::time::Instant::now();
        let dt = self
            .last_update_at
            .map(|prev| (now - prev).as_secs_f32().clamp(0.0, 1.0 / 30.0))
            .unwrap_or(1.0 / 60.0);
        self.last_update_at = Some(now);
        let mut keep = self.advance(dt);
        // Wheel-scroll idle reset. The discrete-detent path in
        // `handle_event` already triggers `click_to_spoke` (and its
        // spring animation) for each `WHEEL_DETENT_PX` of scroll, so
        // there's nothing to "snap back" — we just need to clear the
        // sub-detent residual `wheel_accumulator` once the user
        // pauses, otherwise a partial scroll would silently bias the
        // next gesture and a workspace switch could fire from input
        // the user thought they'd cancelled.
        if let Some(last) = self.wheel_last_event {
            if last.elapsed() > Duration::from_millis(WHEEL_SCROLL_TIMEOUT_MS) {
                self.wheel_accumulator = 0.0;
                self.wheel_last_event = None;
                // A scroll wheel has no equivalent of lifting a finger,
                // so the idle gap is the release: whatever pull was
                // being held against an end lets go here and the wheel
                // returns to the end itself.
                if self.wheel_overscroll_px != 0.0 {
                    self.wheel_overscroll_px = 0.0;
                    let from = self.visual_rotation_deg();
                    let home = self.rotation_for_idx(self.active_idx);
                    self.rotation_deg = home;
                    self.aim_settle_spring(from, home);
                }
            }
            // Always keep pumping while a wheel gesture is in flight —
            // even sub-threshold movement holds the redraw loop awake
            // until the idle timer clears the accumulator.
            keep = true;
        }
        keep
    }

    fn name(&self) -> &str {
        "SidebarWheelDisc"
    }

    /// Stable e2e test identifier — `wheel_disc`. Scenarios that need to
    /// click on the disc itself (vs. an individual spoke) address it
    /// by this name through the driver's registry.
    fn test_id(&self) -> Option<&str> {
        Some("wheel_disc")
    }

    /// Opt into downcasting so the screenshot harness can reach
    /// [`SidebarWheelDisc::hold_rotation_at`] through the host's
    /// `Box<dyn Widget>`.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The disc reports clean once the cache has been populated.
    /// Animation-driven invalidation comes through
    /// [`Widget::needs_animation_frame`]; the fingerprint check inside
    /// `draw` detects the rotation delta on each pumped frame.
    ///
    /// Without this override the default leaf impl returns `true` every
    /// frame, defeating the host's "skip redraw on hover-only
    /// CursorMoved" gate.
    fn needs_redraw(&self) -> bool {
        // Dirty exactly when the next paint would differ from the one
        // already on screen: compare the live fingerprint against the
        // cached one. A fresh widget has an unpopulated cache and so
        // reports dirty until the first paint.
        //
        // `needs_animation_frame` alone isn't enough: a rim drag and the
        // strip pan mutate what the disc paints from inside a
        // `CursorMoved`, the one event class the host declines to
        // repaint on unless the tree reports dirty. A spring in flight
        // is also dirty by definition — it will have moved by the next
        // paint even if this frame's fingerprint still matches. That
        // matters on a drag's first frame: the target has moved but the
        // spring hasn't stepped yet, so reporting clean there deadlocks
        // the gesture — the host skips the repaint, and the repaint is
        // the only thing that pumps `update`, which is the only thing
        // that moves the spring.
        let spring_in_flight = self
            .rotation_spring
            .as_ref()
            .map(|s| !s.is_settled())
            .unwrap_or(false);
        spring_in_flight
            || !self.cached_fp.is_populated()
            || self.current_fingerprint() != self.cached_fp
    }

    fn mark_drawn(&mut self) {
        // Leaf widget — `cached_fp` is updated inside `draw` itself,
        // so this is a no-op. Mirrors `Button::mark_drawn`.
    }

    /// The disc's taffy node is a fixed-width leaf (`visible_width_px`),
    /// so its layout never needs recomputing after the first pass.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl SidebarWheelDisc {
    /// Slide the cached click targets to `target_origin_px`.
    ///
    /// Called on the pan fast-path, where the scene is replayed under a
    /// translation rather than re-encoded. `spoke_hit_rects` and
    /// `plus_rect` are absolute-coordinate caches, so without this a
    /// click after a pan would resolve against the markers' old screen
    /// positions.
    fn retarget_hit_rects(&mut self, target_origin_px: (i64, i64)) {
        let dx = (target_origin_px.0 - self.hit_rects_origin_px.0) as f64;
        let dy = (target_origin_px.1 - self.hit_rects_origin_px.1) as f64;
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

    /// Snap `rotation_deg` to the nearest spoke and move `active_idx`
    /// accordingly. Exposed as a helper so unit tests can exercise the
    /// snap math without synthesising a full winit event sequence.
    ///
    /// Sign convention: dragging *down* (positive dy) gives
    /// `rotation_deg > 0`; `active_idx` advances by `-nearest_offset` so
    /// dragging down brings spoke `+1` into view, matching "pull the
    /// wheel down to reveal the next workspace below".
    pub fn snap_to_nearest(&mut self) {
        let idx = self.idx_at_rotation(self.rotation_deg);
        self.commit_to_idx(idx);
    }

    /// Make `idx` the chosen workspace: park the rotation on its detent
    /// and write it through to the tree.
    ///
    /// Because the rotation is absolute, this is idempotent — running it
    /// twice on an unmoved wheel lands on the same workspace.
    fn commit_to_idx(&mut self, idx: usize) {
        let idx = idx.min(self.spokes.len().saturating_sub(1));
        self.rotation_deg = self.rotation_for_idx(idx);
        self.active_idx = idx;
        self.wheel_overscroll_px = 0.0;
        self.publish_active_to_chrome(idx);
    }

    /// Decide where a released drag ends up, and start it on its way.
    ///
    /// Three cases: let go past an end → back to the end; let go moving
    /// → project the velocity forward over `fling_projection_ms` and
    /// take the nearest detent, so a flick crosses several workspaces;
    /// let go still → the projection collapses to "nearest detent".
    ///
    /// The velocity comes straight off the follow spring, which has
    /// been tracking the pointer all gesture — no separate estimator —
    /// and decays on its own if the user pauses before releasing.
    fn settle_after_drag(&mut self) {
        // Where the rim actually is, captured before anything below
        // moves the target out from under it.
        let from = self.visual_rotation_deg();
        let (lo, hi) = self.rotation_bounds();
        let velocity = self
            .rotation_spring
            .as_ref()
            .map(|s| s.velocity())
            .unwrap_or(0.0);

        let target = if self.drag_raw_deg > hi {
            hi
        } else if self.drag_raw_deg < lo {
            lo
        } else {
            // Project from where the gesture *ended* (`rotation_deg`),
            // not where the rim has trailed to — choosing the trailing
            // position would quietly hand back part of every drag,
            // since the lag is meant to be visible, not subtracted from
            // the gesture.
            let coast_s = Gesture::default().fling_projection_ms as f32 / 1000.0;
            let projected = self.rotation_deg + velocity * coast_s;
            self.rotation_for_idx(self.idx_at_rotation(projected))
        };

        let idx = self.idx_at_rotation(target);
        self.rotation_deg = target;
        self.wheel_overscroll_px = 0.0;
        // The tree is written exactly once per gesture, here. Everything
        // the indicator did while the drag was in flight was display.
        self.publish_active_to_chrome(idx);
        self.aim_settle_spring(from, target);
        // ...and the indicator goes on stepping through the coast.
        self.sync_active_from_visual();
    }

    /// Hold the wheel against an end under scroll input, using the same
    /// resistance curve a rim-drag gets there.
    ///
    /// Only the portion of the gesture that has nowhere to go pulls:
    /// `wheel_overscroll_px` is what whole detents could not be spent
    /// on, plus — once the wheel is already against the end — the
    /// sub-detent remainder too, since waiting there for a detent that
    /// can never arrive would reinstate the dead zone the detent model
    /// is otherwise careful to avoid.
    fn apply_wheel_overscroll(&mut self) {
        let n = self.spokes.len();
        let mut px = self.wheel_overscroll_px;
        if self.active_idx == 0 && self.wheel_accumulator > 0.0 {
            px += self.wheel_accumulator;
        }
        if self.active_idx + 1 >= n && self.wheel_accumulator < 0.0 {
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

    /// Concept's `handleClickItem` (App.tsx:70-78): rotate the wheel so
    /// `target_idx` becomes the active spoke, and let the settle spring
    /// carry it there.
    ///
    /// No shortest-path-around-the-back: the list has ends, so there's
    /// exactly one way from here to there. That's also what the sectors
    /// show — a click two workspaces up should turn the wheel two
    /// notches up, not most of a revolution down.
    pub fn click_to_spoke(&mut self, target_idx: usize) {
        if target_idx >= self.spokes.len() || target_idx == self.active_idx {
            return;
        }
        let from = self.visual_rotation_deg();
        self.commit_to_idx(target_idx);
        let target = self.rotation_deg;
        self.aim_settle_spring(from, target);
        // Leave the indicator on the wheel rather than on the
        // destination: `update` re-derives it from the visible rotation
        // every frame, so setting it ahead here would light the target
        // for exactly one frame, snap back to where the disc actually
        // is, and then walk up to it — a flash on every click.
        self.sync_active_from_visual();
    }

    /// One frame of wheel motion: step the spring, then re-derive the
    /// indicator from where that left the disc.
    ///
    /// Split out of [`Widget::update`] so the only thing `update` adds
    /// is reading the wall clock — tests drive this directly with a
    /// fixed dt, still exercising the production path.
    fn advance(&mut self, dt: f32) -> bool {
        let keep = self.tick_rotation_spring(dt);
        // The indicator steps off the *visible* rotation, so it has to
        // be re-derived on every frame the spring moves — that is what
        // carries the stepping through a flick's coast as well as
        // through the drag itself.
        self.sync_active_from_visual();
        keep
    }

    /// Advance the rim snap-spring by `dt` seconds. Drops the spring
    /// (and the dt clock) once settled. Returns whether the spring is
    /// still animating. Split out of `update` so production feeds it the
    /// real frame dt while tests can pump a deterministic fixed dt.
    fn tick_rotation_spring(&mut self, dt: f32) -> bool {
        if let Some(spring) = self.rotation_spring.as_mut() {
            if spring.is_settled() {
                // `is_settled` tolerates 0.1°, so drop the spring rather
                // than let the visible rotation rest a fraction of a
                // degree off its detent for as long as nothing else
                // moves.
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
    /// in, so an offscreen frame can capture a moving wheel.
    ///
    /// Returns whether there was a settle to sample. Steps the same
    /// production `Spring` — same constants, same integrator — at the
    /// same fixed 60 Hz the live loop uses, rather than reconstructing
    /// the curve: a still can't show weight, and rendering costs more
    /// wall time than the settle lasts, so real frames only ever catch
    /// the endpoints.
    pub fn hold_rotation_at(&mut self, elapsed: Duration) -> bool {
        let Some(origin) = self.settle_origin.clone() else {
            return false;
        };
        let mut s = origin;
        let dt = 1.0 / 60.0;
        let steps = (elapsed.as_secs_f32() / dt).round().max(0.0) as u32;
        for _ in 0..steps {
            s.step(dt);
        }
        self.rotation_spring = Some(s);
        self.sync_active_from_visual();
        true
    }

    /// The annulus the disc's **labels and add-button** occupy, as
    /// `(inner, outer)` about [`SidebarWheelDisc::disc_centre`].
    ///
    /// Not the extent of the painted surface — the body is a solid
    /// segment reaching the centre; this bounds the readable ring near
    /// the rim where a name fits, derived rather than configured (see
    /// [`BAND_TANGENT_FRACTION`]). Exposed so callers needing this
    /// (snapshot tests, the add button) don't re-derive it and drift.
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

    /// The disc's logical centre. It sits well off-screen to the left of
    /// the footprint — that is what makes the visible arc a slice of
    /// something large rather than a small circle in a column.
    pub fn disc_centre(&self) -> Point {
        self.center()
    }

    /// Test helper: pins the post-`apply_layout` rect for tests that
    /// assert on the clamped position rather than a transient cache.
    #[cfg(test)]
    pub(crate) fn rect_for_tests(&self) -> Rect {
        self.rect
    }

    /// Test helper: replays the same math as `draw`'s presence-dot pass
    /// and returns the total number of dots that would be painted for
    /// the given `PresenceCtx`, without touching Vello. Mirrors the cap
    /// (3 per spoke), the `workspace_id.is_some()` skip, and the
    /// empty-peers skip. Lets unit tests assert dot counts without a
    /// GPU-backed scene.
    #[cfg(test)]
    pub fn presence_dot_hit_count(&self, ctx: &PresenceCtx) -> usize {
        let mut total = 0usize;
        for spoke in &self.spokes {
            let Some(ws_id) = spoke.workspace_id else {
                continue;
            };
            let key = PresenceKey::Workspace {
                id: format!("{}", ws_id),
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
        // Pin a rect spanning the disc's full diameter vertically so the
        // visible arc is fully accessible at `cy = diameter/2`. The
        // visible-footprint width is `radius * VISIBLE_RADIUS_FRACTION`.
        let visible_w = d.visible_width_px();
        d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);
        d
    }

    /// X coordinate that's reliably inside the visible arc at the
    /// vertical midline. Pick the rightmost rim column — the rim is
    /// rightmost at `cy`, so an x near `rect.x1` is well inside the
    /// circle of radius `r` around `cx = -r·(1 - VISIBLE)`.
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

    /// Synthetic wheel event. `lines` is in `LineDelta` units, which the
    /// widget scales by 16 px — one mouse-wheel click.
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
        // Use a dummy node — the widget's handle_event doesn't consult it.
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

    /// Run the real `Widget::draw`, which is what populates the scene
    /// cache, the fingerprint and every cached hit rect.
    fn paint(d: &mut SidebarWheelDisc) {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        d.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// The slot the live shell gives the disc: narrow, and much taller
    /// than it is wide. The ratio is what decides how much rim is on
    /// screen, so a test that cares about which rows are visible has to
    /// use something close to it rather than the square-ish rect `mk`
    /// pins for the pure-geometry tests.
    fn live_slot() -> Rect {
        Rect::new(0.0, 40.0, 234.0, 830.0)
    }

    /// A switcher exists to say what you would be switching TO. Every
    /// sector on screen carries its workspace's name, and the click target
    /// is grown from the label that was actually painted — so a hit rect
    /// wide enough to hold a name is proof the name is there.
    #[test]
    fn visible_neighbours_carry_a_name_not_just_a_speck() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());
        paint(&mut d);

        let named: Vec<usize> = (0..d.spokes.len())
            .filter(|i| {
                *i != d.active_idx
                    && d.spoke_hit_rects[*i].width().max(d.spoke_hit_rects[*i].height())
                        >= LABEL_MIN_W
            })
            .collect();
        assert!(
            !named.is_empty(),
            "no neighbour got a named sector: hit rects {:?}",
            d.spoke_hit_rects
        );
    }

    /// The workspace list has a first entry and a last one, and the disc
    /// has to show that. Sectors are laid out by their real index
    /// relative to the active one, so the workspace above Home is
    /// nothing at all — the overscroll rubber-band at either end is what
    /// reports the edge to the hand.
    #[test]
    fn sectors_sit_at_their_real_index_relative_to_the_active_one() {
        // From the middle: one neighbour each side.
        let mut d = mk(&["Home", "Research", "Design"]).with_active_idx(1);
        layout_into_slot(&mut d, live_slot());
        paint(&mut d);
        let cy = (d.rect.y0 + d.rect.y1) / 2.0;
        let placed: Vec<Rect> =
            d.spoke_hit_rects.iter().copied().filter(|r| r.area() > 0.0).collect();
        assert_eq!(placed.len(), 3, "all three workspaces are on screen: {placed:?}");
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
        let cy = (d.rect.y0 + d.rect.y1) / 2.0;
        let placed: Vec<Rect> =
            d.spoke_hit_rects.iter().copied().filter(|r| r.area() > 0.0).collect();
        assert_eq!(placed.len(), 3, "all three are still on screen: {placed:?}");
        assert!(
            placed.iter().any(|r| r.center().y < cy - 1.0),
            "the wrap has to fill the arc above the first workspace: {placed:?}"
        );

        // Uniqueness, on the production slot assignment rather than on
        // the rects it produces: a rect map is keyed by workspace, so a
        // workspace drawn twice would overwrite itself and leave no
        // trace.
        for slots in 1..=6 {
            let assigned: Vec<usize> =
                d.slot_indices(slots).into_iter().flatten().collect();
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

    /// The band always holds workspaces. Whatever the rotation, however
    /// hard the wheel has been thrown, at least one sector has to be on
    /// screen and named — a switcher showing an empty curve tells the
    /// user their workspaces are gone.
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

    /// Same invariant under the scroll wheel, which reaches the
    /// rotation through a different path than a rim-drag: an
    /// accumulator spending whole detents, with the remainder diverted
    /// into the end resistance.
    #[test]
    fn the_disc_is_never_empty_under_sustained_scrolling() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());
        // Park the cursor on the arc: the handler ignores wheel events
        // that are not over the disc.
        let cx = drag_anchor_x(&d);
        let cy = (d.rect.y0 + d.rect.y1) / 2.0;
        pump(&mut d, cursor_event(cx, cy));

        for dir in [-1.0_f32, 1.0] {
            for _ in 0..60 {
                pump(&mut d, wheel_event(dir * 3.0));
                frames(&mut d, 2);
                paint(&mut d);
                let named =
                    d.spoke_hit_rects.iter().filter(|r| r.area() > 0.0).count();
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

    /// Every workspace stays on the disc, and stays clickable, at every
    /// size the sidebar can take.
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

    /// The add-workspace button does not rotate, so a sector label
    /// sweeps past it on every turn of the wheel; the button has to
    /// stay usable throughout.
    ///
    /// Drives the real press path at every position a label passes
    /// through between detents, rather than comparing rectangles: the
    /// press handler resolves the button before it resolves a sector,
    /// and that ordering is the thing that has to hold.
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

    /// A label may only be withheld when it genuinely has nowhere to go.
    ///
    /// Driven from the middle workspace so all three sectors are on the
    /// band and any missing name is a withheld one — from the first
    /// workspace the last sector is off the band outright, which is a
    /// legitimate reason to have no label and would mask the case this
    /// guards.
    #[test]
    fn a_sector_keeps_its_name_when_it_only_passes_near_the_add_button() {
        let mut d = mk(&["Home", "Research", "Design"]).with_active_idx(1);
        layout_into_slot(&mut d, live_slot());
        paint(&mut d);

        let named = d
            .spoke_hit_rects
            .iter()
            .filter(|r| r.area() > 0.0)
            .count();
        assert_eq!(
            named,
            d.spokes.len(),
            "a workspace lost its label with the wheel at rest: {:?}",
            d.spoke_hit_rects
        );
    }

    /// The band's inner arc bends back toward the disc's centre faster
    /// than the footprint's left edge does, so past a point it leaves
    /// through that edge and the clip turns the curve into a straight
    /// vertical cut — survivable only while the vertical fade has
    /// already taken the band down.
    ///
    /// Asserts on the two production constants that decide it rather
    /// than on a rendered pixel: change either one alone and this fails.
    #[test]
    fn band_clip_starts_where_the_fade_has_already_taken_hold() {
        // Fraction of the footprint's height between its edge and the
        // point the inner arc starts being clipped.
        let clip_at = (1.0 - BAND_TANGENT_FRACTION) / 2.0;
        // The fade ramps linearly from 0 at the edge to 1 at FADE_IN_STOP.
        let alpha_at_clip = clip_at / FADE_IN_STOP as f64;
        assert!(
            alpha_at_clip <= 0.35,
            "the band gets clipped while still at {:.0}% alpha — the cut \
             will read as a straight edge sliced through the surface. \
             Raise BAND_TANGENT_FRACTION ({BAND_TANGENT_FRACTION}) or \
             FADE_IN_STOP ({FADE_IN_STOP}).",
            alpha_at_clip * 100.0
        );
    }

    /// Display slots have to cover the whole visible sweep, not just the
    /// workspaces — indexing sectors off the workspace list would leave
    /// a stretch of unsegmented band rotating through whenever there
    /// are fewer workspaces than the sweep has room for.
    #[test]
    fn sector_slots_cover_the_whole_sweep() {
        for sweep_deg in [20.0_f64, 31.0, 38.0, 44.0] {
            let reach = slot_reach(sweep_deg.to_radians(), SPOKE_ANGLE_STEP_DEG as f64);
            // The fan runs from `-reach` to `+reach` slots, and each slot
            // spans half a gap either side of its centreline.
            let covered = reach as f64 * SPOKE_ANGLE_STEP_DEG as f64
                + SPOKE_ANGLE_STEP_DEG as f64 / 2.0;
            assert!(
                covered >= sweep_deg,
                "at a {sweep_deg}° sweep the fan only reaches {covered}° — \
                 the rest of the band has no sector on it"
            );
        }
    }

    /// A one-workspace disc is the degenerate case: `spoke_gap_deg` widens
    /// to a full turn so the wheel cannot scroll past its only entry, and
    /// the fan must collapse to that single slot rather than tiling copies
    /// of it around the band.
    #[test]
    fn a_single_workspace_gets_exactly_one_slot() {
        assert_eq!(slot_reach(40.0_f64.to_radians(), 360.0), 0);
    }

    /// Dragging the rim has to keep asking for frames. The rotation is
    /// mutated from inside a `CursorMoved`, and the host skips the
    /// repaint on a pointer move unless the tree reports dirty — so a
    /// disc that reports clean mid-drag renders a stationary wheel
    /// under a moving finger.
    #[test]
    fn dragging_the_rim_reports_dirty_on_every_move() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let cy = (d.rect.y0 + d.rect.y1) / 2.0;
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

    /// The disc rides the strip's canvas, so a horizontal pan changes
    /// where it paints without changing what it paints. That has to
    /// reach the screen: the host only repaints a pointer-driven frame
    /// when something reports dirty.
    #[test]
    fn panning_the_strip_reports_dirty_and_moves_the_click_targets() {
        let mut d = mk(&["Home", "Research", "Design"]);
        // Click targets are grown from the labels, and a label only paints
        // where the band is deep enough to hold one — so this has to run
        // on the slot the live shell hands the disc, not on the
        // full-diameter rect `mk` pins for the pure-geometry tests.
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

    /// The pan fast-path replays a translated copy of the cached scene.
    /// Its whole reason to exist is that the pixels are unchanged, so
    /// the fingerprint has to agree that only the origin moved.
    #[test]
    fn a_pure_pan_is_a_content_cache_hit() {
        let mut d = mk(&["Home", "Research", "Design"]);
        paint(&mut d);
        let docked = d.current_fingerprint();

        d.rect = d.rect.with_origin(Point::new(d.rect.x0 - 120.0, d.rect.y0));
        let panned = d.current_fingerprint();

        assert_ne!(panned, docked, "the origin moved, so the frame is not identical");
        assert!(
            panned.content_eq(&docked),
            "a pan repaints nothing — it must stay a content hit so the scene is replayed, \
             not re-encoded"
        );
    }

    /// Summoning re-places the disc around the pointer without resizing
    /// it. The user's constraint is that a summon shows no more of the
    /// wheel than the docked state does.
    #[test]
    fn summoning_moves_the_disc_to_the_pointer_at_the_same_size() {
        let mut d = mk(&["Home", "Research", "Design"]);

        let slot = Rect::new(0.0, 40.0, 320.0, 940.0);
        layout_into_slot(&mut d, slot);
        let docked = d.rect;
        assert!(docked.width() > 0.0, "the docked pass produced a footprint");

        // A pointer inside the band the disc can actually reach — it is
        // nearly as tall as its slot, so the reachable travel is the
        // slack between the two.
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
        let centre_y = (d.rect.y0 + d.rect.y1) / 2.0;
        assert!(
            (centre_y - pointer.y).abs() < 1.0,
            "the disc centres on the pointer vertically, got {centre_y} for pointer y={}",
            pointer.y
        );

        // A summon near the floor cannot centre without hanging off the
        // slot, so it slides as far as it can and stops flush.
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

        // Releasing the summon returns it to the docked slot exactly.
        d.set_summon(None);
        layout_into_slot(&mut d, slot);
        assert_eq!(
            d.rect, docked,
            "dismissing a summon restores the docked footprint"
        );
    }

    /// Lay the disc out through its real `build_layout` / `apply_layout`
    /// against a slot of the given box, the way the overlay host does.
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

    /// Default workspace labels (`Home` / `Research` / `Design`) seeded
    /// by `WorkspaceTree::bootstrap_demo` must each map to a
    /// distinctive lucide glyph rather than falling through to the
    /// generic `Layers` default. Also pins the public `spoke_icon` API
    /// to confirm it lower-cases the label before resolving.
    #[test]
    fn default_workspace_labels_resolve_to_distinct_glyphs() {
        assert_eq!(spoke_icon_for_label("home"), Icon::Home);
        assert_eq!(spoke_icon_for_label("research"), Icon::Search);
        assert_eq!(spoke_icon_for_label("design"), Icon::PenTool);
        // Public API path goes through the same table; we verify the
        // case-insensitive lower-casing happens at the call site.
        let d = mk(&["Home", "Research", "Design"]);
        assert_eq!(d.spoke_icon(0), Icon::Home);
        assert_eq!(d.spoke_icon(1), Icon::Search);
        assert_eq!(d.spoke_icon(2), Icon::PenTool);
    }

    /// Hover into the visible arc without pressing must NOT make the
    /// disc claim pointer capture — capture is reserved for an
    /// actually-in-flight drag. The hover anchor is cached for the next
    /// press's hit-test in a separate field.
    #[test]
    fn hover_does_not_capture_pointer() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let cy = d.diameter_px / 2.0;
        let cx = drag_anchor_x(&d);
        // Move the cursor into the visible arc without pressing.
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
        // Sanity: the hover anchor IS cached for the next press's
        // hit-test, just not via `drag_from`.
        assert!(
            d.hover_anchor.is_some(),
            "hover into arc must populate `hover_anchor`"
        );
    }

    /// Press inside the disc, then release: pointer capture must clear
    /// so the next idle hover doesn't continue to swallow events.
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

        // With DRAG_FACTOR = 0.15 and a 15° gap, a `15/0.15 = 100 px`
        // drag is exactly one gap. Add a margin to land cleanly past.
        //
        // Dragged UPWARD: the disc starts on workspace 0, the first
        // one, so pulling down from there pulls against the top end
        // rather than travelling to a neighbour — up is where the rest
        // of the list is.
        let drag_dy = (gap / DRAG_FACTOR) as f64 + 30.0;
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

        // Seed a 3-workspace tree. `bootstrap_demo` already seeds
        // Home / Research / Design — three workspaces in a known order
        // matching the spoke layout — so no additional appends are needed.
        let tree = WorkspaceTree::bootstrap_demo();
        assert_eq!(tree.workspaces.len(), 3);
        let ws_ids: Vec<_> = tree.workspaces.iter().map(|w| w.id).collect();
        let initial_active = tree.active;
        assert_eq!(
            initial_active, ws_ids[0],
            "bootstrap_demo should leave active = ws_ids[0]"
        );

        // Wire up chrome + disc. Spokes carry the workspace ids so the
        // write-back can map active_idx → WorkspaceId.
        let chrome = ChromeState::new(tree);
        let spokes: Vec<SpokeLabel> = ws_ids
            .iter()
            .enumerate()
            .map(|(i, id)| SpokeLabel {
                text: format!("WS-{}", i),
                workspace_id: Some(id.0),
            })
            .collect();
        let mut d = SidebarWheelDisc::new(spokes).with_chrome(chrome.clone());
        let visible_w = d.visible_width_px();
        d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);

        // Synthesise press+move+release sequence, dragging UP: the tree
        // starts on its first workspace, and with a finite list that is
        // the top end — the reachable neighbours are all below it.
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);
        let drag_dy = (d.spoke_gap_deg() / DRAG_FACTOR) as f64 + 100.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y - drag_dy));
        assert!(pump(&mut d, release_event()));

        assert_ne!(
            d.selected_idx(),
            0,
            "a >gap drag must advance past the initial spoke"
        );
        // And chrome.tree.active must now point at the workspace id
        // that matches the new active_idx — this is the CRDT write-back
        // contract the disc now fulfils via `publish_active_to_chrome`.
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
        let drag_dy = (d.spoke_gap_deg() / DRAG_FACTOR) as f64 + 100.0;
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
        let gap = d.spoke_gap_deg() as f64;
        assert!(gap > 0.0, "degenerate gap {gap}");
        let drag_px = 0.73 * gap / DRAG_FACTOR as f64;
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
            "follow spring should have caught up to the drag target {} (got {})",
            pre_snap_deg,
            visual_at_release
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

        // Pump the per-frame update hook at 60 Hz. The "heavy" preset
        // (stiffness 392, damping 56, mass 2) has ω = 14 rad/s at
        // ζ = 1.0 — critically damped, so it approaches without ever
        // crossing and needs roughly ωt ≈ 7 to land inside
        // `is_settled`'s 0.1° / 0.1 °/s window. 180 frames (~3 s) is
        // well over that with headroom for the integrator's step bias.
        // Deterministic 1/60 s dt: production uses real wall-clock dt
        // via `update`; the test pumps the extracted stepper instead.
        for _ in 0..180 {
            d.tick_rotation_spring(1.0 / 60.0);
        }

        // After settling, `update` drops the spring so further frames
        // don't keep the animation loop alive. We assert on both the
        // spec'd invariant (spring settled or gone) and the visible
        // rotation landing on the target within 1°.
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
        let visual = d
            .rotation_spring
            .as_ref()
            .map(|s| s.value())
            .unwrap_or(target);
        assert!(
            (visual - target).abs() < 1.0,
            "visual rotation {} must be within 1° of target {}",
            visual,
            target
        );
    }

    /// The plus-slot rect is a `draw`-time artefact — until the widget
    /// has been painted, `handle_event` must see `Rect::ZERO` so the
    /// press path cannot accidentally hit-test an undefined region.
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

    /// Pressing inside the cached `plus_rect` must call
    /// `ChromeState::add_workspace` — the disc's direct-create path for a
    /// new workspace. Mirrors `snap_publishes_active_workspace_to_chrome`'s
    /// chrome-setup pattern but exercises the press-on-plus branch rather
    /// than the drag→snap branch.
    #[test]
    fn press_on_plus_rect_calls_chrome_add_workspace() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;

        // Deliberately the first-boot tree: one empty workspace. That is
        // the state in which the "+" matters most — it is the only way a
        // fresh machine grows a second workspace, so it has to work from
        // exactly one spoke, not just from a populated disc.
        let tree = WorkspaceTree::bootstrap_default();
        let initial_count = tree.workspaces.len();
        let initial_active = tree.active;
        let chrome = ChromeState::new(tree);

        // Lay out and paint for real, then click whatever the paint
        // pass decided the add row's box is — `draw` needs no GPU, it
        // encodes into a `Scene`.
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

        // Move the cursor to the slot's centre so `last_cursor_pos` picks
        // it up — the press handler consults that cache (not the event,
        // which has no position field) to hit-test the plus slot.
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

    /// Each spoke should render one presence dot per peer currently
    /// registered on its workspace key (up to 3). Seeds two peers on
    /// workspace 77 and asserts the helper reports exactly 2 dots,
    /// matching `presence.peers_on(...)` for that spoke's
    /// `PresenceKey::Workspace`.
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

        // Baseline: an empty PresenceCtx produces no dots.
        let empty = PresenceCtx::empty();
        assert_eq!(
            d.presence_dot_hit_count(&empty),
            0,
            "baseline with empty presence must not paint any dots"
        );
        // Seeded: exactly 2 peers → exactly 2 dots on the one spoke.
        assert_eq!(
            d.presence_dot_hit_count(&ctx),
            2,
            "two seeded peers on workspace 77 must yield 2 dots"
        );
    }

    /// The disc's logical centre must sit OFF-SCREEN-LEFT so only the
    /// right portion of the rim protrudes into the visible sidebar
    /// column: `cx = rect.x0 - radius * (1 - VISIBLE_RADIUS_FRACTION)`.
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

    /// Visible arc cap. The user-spec invariant is "at most 120°".
    #[test]
    fn disc_visible_arc_matches_concept_ratio() {
        let disc = mk(&["A", "B", "C"]);
        let c = disc.center();
        let radius = disc.diameter_px / 2.0;
        let mut visible_deg = 0i32;
        for deg in 0..360 {
            let a = (deg as f64).to_radians();
            let x = c.x + radius * a.cos();
            if x >= disc.rect.x0 - 1e-6 {
                visible_deg += 1;
            }
        }
        let expected = 2.0 * (1.0 - VISIBLE_RADIUS_FRACTION).acos().to_degrees();
        let tol = 2.0;
        assert!(
            (visible_deg as f64 - expected).abs() <= tol,
            "visible arc must be ~{:.1}°; counted {}°",
            expected,
            visible_deg,
        );
        // User-spec upper bound (always met).
        assert!(
            visible_deg <= 122,
            "visible arc must not exceed 120°; counted {}°",
            visible_deg,
        );
    }

    /// A press whose anchor lands LEFT of the viewport edge (i.e.
    /// outside `self.rect`) must NOT start a drag — the off-screen part
    /// of the disc isn't grabbable.
    #[test]
    fn disc_hit_test_rejects_clicks_left_of_viewport_edge() {
        let mut d = mk(&["Home", "Research", "Design"]);
        // Cursor at the disc's logical centre (off-screen-left).
        let c = d.center();
        pump(&mut d, cursor_event(c.x, c.y));
        // No drag was anchored on the move — `drag_from` stays None
        // because the cursor is outside the visible-arc hit-test.
        assert!(
            d.drag_from.is_none(),
            "off-screen cursor must not seed a drag anchor; drag_from={:?}",
            d.drag_from
        );
        // And a Pressed event there (without a prior in-arc CursorMoved)
        // must not start a drag either.
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

    /// The disc caps the per-spoke dot cluster at 3 even when more
    /// peers are registered, matching the concept's wheel-card design.
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

    // -----------------------------------------------------------------
    // How it feels, in numbers.
    //
    // A still can't show weight, and neither can a video on its own —
    // these tests pin the quantities the recordings only illustrate,
    // driving `advance` (`update` minus the wall-clock read) at a
    // fixed 60 Hz.
    // -----------------------------------------------------------------

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

    /// Walks a single continuous drag across three detents and records
    /// the indicator at every frame. Requires it to visit each
    /// workspace in turn — never skipping one, never running backwards
    /// — and to change hands within half a detent of the boundary each
    /// time.
    #[test]
    fn the_indicator_steps_one_workspace_at_a_time_during_a_drag() {
        let mut d = mk(&["A", "B", "C", "D", "E"]);
        let gap = d.spoke_gap_deg();
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        // Start on the middle workspace so there is room to travel in
        // the direction under test without meeting an end.
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));

        // One detent is `gap / DRAG_FACTOR` px. Walk three of them in
        // small steps, sampling the indicator continuously.
        let px_per_detent = (gap / DRAG_FACTOR) as f64;
        let mut seen = vec![d.active_idx];
        let mut steps_at = Vec::new();
        let sub_steps = 60;
        for i in 1..=(3 * sub_steps) {
            let dy = -(px_per_detent * 3.0) * (i as f64 / (3 * sub_steps) as f64);
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

    /// Weight, measured. The settle must never cross its target — an
    /// overshoot-and-return is the signature of something light — and it
    /// must take visible but bounded time to arrive.
    #[test]
    fn the_settle_is_slow_enough_to_feel_and_never_overshoots() {
        let mut d = mk(&["A", "B", "C", "D"]);
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // Just past halfway to the next workspace, then release: a
        // plain one-detent settle with no fling in it. Stated as a
        // fraction of a gap rather than in pixels, so retuning the
        // geometry cannot turn "past halfway" into two detents.
        let gap = d.spoke_gap_deg() as f64;
        drag_to(&mut d, cx, start_y, -(0.6 * gap / DRAG_FACTOR as f64), 40);
        assert!(pump(&mut d, release_event()));

        let target = d.rotation_deg;
        let start = d.visual_rotation_deg();
        assert!(
            (target - start).abs() as f64 > gap * 0.1,
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

    /// Follow-lag: the rim trails the pointer instead of being welded to
    /// it, and the gap grows with speed. Rigid tracking reads weightless.
    #[test]
    fn the_rim_trails_a_fast_drag_further_than_a_slow_one() {
        let lag_for = |gaps_per_frame: f64| -> f32 {
            let names: Vec<String> = (0..16).map(|i| format!("W{i}")).collect();
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let mut d = mk(&refs);
            let px_per_frame =
                gaps_per_frame * d.spoke_gap_deg() as f64 / DRAG_FACTOR as f64;
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
        // ...but never so far that it reads as the machine being late.
        assert!(
            fast < 15.0,
            "the rim trails a fast drag by {fast:.1}°, a whole detent — past a \
             point lag stops reading as weight and starts reading as latency"
        );
    }

    /// Momentum: from the same release position, a flick must land
    /// further down the list than a slow let-go.
    #[test]
    fn a_flick_carries_past_the_workspace_a_slow_release_settles_on() {
        let land = |gaps_per_frame: f64, pause_frames: usize| -> usize {
            let mut d = mk(&["A", "B", "C", "D", "E", "F", "G", "H"]);
            let px_per_frame =
                gaps_per_frame * d.spoke_gap_deg() as f64 / DRAG_FACTOR as f64;
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

        // Same distance travelled, released moving vs released still.
        let flicked = land(0.12, 0);
        let placed = land(0.12, 45);
        assert!(
            flicked > placed,
            "a flick landed on workspace {flicked} and a slow release on {placed} \
             — releasing while still moving has to carry further, or the wheel \
             has no momentum"
        );
    }

    /// The ends. Pulling past the first workspace has to give, resist
    /// more the further it is pulled, stay bounded, and come back to the
    /// end itself rather than to anything beyond it.
    #[test]
    fn pulling_past_the_first_workspace_resists_and_returns_to_it() {
        let mut d = mk(&["A", "B", "C"]);
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));

        // Down from workspace 0 is off the top of the list. Pulls are
        // evenly spaced so successive gains are directly comparable —
        // "each extra 100 px buys less than the last 100 px did" is the
        // property, and uneven steps would not test it.
        let mut last_gain = f32::INFINITY;
        let mut last = 0.0f32;
        let reach = d.overscroll_reach_deg();
        for pull_px in (1..=12).map(|i| i as f64 * 100.0) {
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
            // The indicator stays on the workspace being pulled away
            // from: there is nothing past it to point at.
            assert_eq!(d.active_idx, 0, "overscroll must not select a workspace");
        }

        // Far out it has all but stopped, but has still not stopped.
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

    /// The last workspace is an end too: scrolling off the bottom must
    /// not wrap back to the top.
    #[test]
    fn the_last_workspace_is_an_end_and_the_list_does_not_wrap() {
        let mut d = mk(&["A", "B", "C"]);
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // Far past the bottom of a three-workspace list.
        drag_to(&mut d, cx, start_y, -10_000.0, 60);
        assert_eq!(
            d.active_idx, 2,
            "dragging past the last workspace wrapped round to the start"
        );
        assert!(pump(&mut d, release_event()));
        assert_eq!(d.selected_idx(), 2);
    }

    /// Scroll input has to meet the same edge the drag does, or the two
    /// ways into the control feel like two different controls.
    #[test]
    fn scrolling_into_an_end_pulls_against_it_and_lets_go() {
        let mut d = mk(&["A", "B", "C"]);
        let cx = drag_anchor_x(&d);
        let mid_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, mid_y));

        // Scroll hard against the top end, which is where we start.
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
            pulled < d.overscroll_reach_deg(),
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

    /// Scrolling in the middle of the list still costs the documented
    /// three wheel clicks per workspace — the overscroll work must not
    /// have quietly turned scroll into a continuous drag.
    #[test]
    fn scrolling_mid_list_still_takes_three_clicks_per_workspace() {
        let mut d = mk(&["A", "B", "C", "D", "E"]);
        let cx = drag_anchor_x(&d);
        let mid_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, mid_y));
        // One wheel click is 16 px; the detent is 48.
        for click in 1..=3 {
            pump(&mut d, wheel_event(-1.0));
            let expected = if click == 3 { 1 } else { 0 };
            assert_eq!(
                d.selected_idx(), expected,
                "after {click} wheel click(s) the wheel should be on workspace \
                 {expected}"
            );
        }
    }

    /// The active workspace's own name must never be the thing that
    /// gets thrown away to make room for a font size — `Research` did
    /// not fit the band at the full active step on a 900-px-tall
    /// display, showing `Resear…` for the workspace you were standing
    /// in.
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

        // The step is only given up when it has to be: a short name
        // still gets the full one.
        assert_eq!(
            d.active_chip_height(0, chip_w, &theme, &mut tr),
            SECTOR_CHIP_H_ACTIVE,
            "`Home` fits at the full step and must keep it"
        );
    }

    /// A gesture that ends where it started must not tell the rest of
    /// the shell that the workspace changed.
    ///
    /// Every pull against an end does exactly that, and the chrome
    /// revision is what the shell re-syncs the whole tree against — so
    /// an unconditional write turned a bounce off the end into a full
    /// re-sync for no change at all.
    #[test]
    fn a_gesture_that_lands_where_it_started_does_not_bump_the_chrome() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;

        let tree = WorkspaceTree::bootstrap_demo();
        let ws_ids: Vec<_> = tree.workspaces.iter().map(|w| w.id).collect();
        let chrome = ChromeState::new(tree);
        let spokes: Vec<SpokeLabel> = ws_ids
            .iter()
            .map(|id| SpokeLabel { text: "W".into(), workspace_id: Some(id.0) })
            .collect();
        let mut d = SidebarWheelDisc::new(spokes).with_chrome(chrome.clone());
        let visible_w = d.visible_width_px();
        d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);

        let before = chrome.borrow().revision;
        let cx = drag_anchor_x(&d);
        let start_y = d.diameter_px / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // Down from the first workspace: a pull against the top end,
        // which can only ever come back to where it began.
        drag_to(&mut d, cx, start_y, 400.0, 30);
        assert!(pump(&mut d, release_event()));
        assert_eq!(d.selected_idx(), 0, "the pull cannot have selected anything");
        assert_eq!(
            chrome.borrow().revision,
            before,
            "a bounce off the end bumped the chrome, forcing a full re-sync for a \
             workspace that never changed"
        );

        // A gesture that DOES change the workspace still bumps. Let the
        // bounce finish first — starting the next drag while the wheel
        // is still returning means it begins part of a detent from home
        // and the same pull no longer reaches the next workspace.
        frames(&mut d, 150);
        assert!(d.visual_rotation_deg().abs() < 0.1, "the bounce has not finished");
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // A clear detent and a bit, in gaps rather than pixels: how many
        // pixels a detent costs depends on the geometry the disc was
        // laid out into.
        let gap = d.spoke_gap_deg() as f64;
        drag_to(&mut d, cx, start_y, -(1.3 * gap / DRAG_FACTOR as f64), 30);
        assert!(pump(&mut d, release_event()));
        assert_ne!(d.selected_idx(), 0);
        assert_ne!(
            chrome.borrow().revision,
            before,
            "an actual workspace change must still bump the chrome"
        );
    }

    /// `click_to_spoke(target)` rotates the wheel so `target` becomes
    /// active and animates via the snap spring.
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

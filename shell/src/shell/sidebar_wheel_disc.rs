//! `SidebarWheelDisc` — large radial workspace switcher anchored
//! OFF-SCREEN-LEFT, with only a narrow arc of its right edge protruding
//! into the visible sidebar column. How many degrees that is falls out of
//! the slot's height-to-width ratio; on the live sidebar it is around 60°,
//! which carries the active workspace plus two neighbours each side. Each **workspace** is a spoke; the
//! user drags vertically to rotate, and a release snaps to the nearest
//! spoke.
//!
//! ## The design
//!
//! The concept prototype (`radial-workspace-switcher/src/App.tsx`)
//! painted every spoke as a rounded pill card with a vertically-rotated
//! label. On a narrow column that read as crowded — several cards
//! overlapping inside the visible arc — and the per-spoke rotation left
//! the labels at inconsistent angles.
//!
//! What replaced it went too far the other way: a quiet ring where the
//! active workspace got a large name plate and every other workspace was
//! an unlabelled speck. That inverts what a switcher is for. It told you
//! where you already were, and about the places you might go it said only
//! that they existed. The plate and the specks also read as two unrelated
//! kinds of object rather than one set in two states.
//!
//! The current design is a **segmented disc** — the thing you spin:
//! - The **body** is a flat, *solid* segment: one uniform fill of the
//!   theme's glass surface running from the rim all the way in to the
//!   disc's centre. No radial ramp, no concentric strokes, no shading.
//!   An earlier form graded the body from transparent at its inner end
//!   to a peak alpha at the rim, which spread a grey smear across half
//!   the slot and never resolved into an object.
//! - It is solid rather than a ring. A ring was tried, on the theory
//!   that a second concentric edge gives the object a silhouette, and
//!   it does — but a disc segment with a hole punched out of it is not
//!   a disc segment, and the hole is what you notice. Because the
//!   centre sits far off-screen to the left, "solid" in practice means
//!   the fill runs off the left edge of the footprint without ever
//!   ending, which is exactly the read wanted: a piece of something
//!   much larger, continuing past what you can see.
//! - The silhouette therefore comes from the rim and the sector
//!   dividers alone. Both had to carry more for that: the rim keeps
//!   its soft-wide + crisp-hairline pair and its outward shadow, and
//!   the dividers now run the full depth of the segment instead of
//!   stopping at an inner arc, so the surface is legibly *divided*
//!   rather than merely tinted.
//! - The band is divided into **sectors**, one per 15° display slot,
//!   by radial hairlines. The sectors are what turn: spinning sweeps
//!   the dividers, the tints and the labels through the slot together,
//!   so the object visibly rotates rather than having things slide
//!   along a fixed curve.
//! - Display slots tile the whole visible sweep and **cycle** through
//!   the workspaces (the concept's 24 slots for 6 workspaces), so
//!   there is never a stretch of unsegmented disc rotating past.
//! - Each sector carries its workspace's glyph and name on a chip laid
//!   **on the disc**, rotated to its own sector so it lies flat on the
//!   surface. Every chip sits at the same radius and is the same width,
//!   so they form a ring; when the wheel turns, the labels tip with it.
//! - The **active** sector is filled with its workspace's colour, its
//!   two dividers brighten, and its chip is the same chip one size step
//!   larger and tinted. One family of things in two states.
//! - Rim, inner arc and sector dividers are all hairlines derived from
//!   the theme's foreground, in the same translucent-fill + lit-edge +
//!   soft-shadow language the window decorations use, so the disc and
//!   the window chrome read as one system.
//! - Everything is faded on one vertical curve at the very top and
//!   bottom of the footprint, so the band runs off the slot rather than
//!   ending on a straight cut.
//!
//! ## What it feels like
//!
//! The disc is meant to read as a physical wheel with a finite list of
//! notches on it, and three things carry that.
//!
//! **The indicator steps under your hand.** The lit sector advances the
//! moment the wheel's rotation crosses a detent boundary, then the next,
//! for as long as you keep dragging. It used to sit still through the
//! entire gesture and jump once at the end, which made the drag feel
//! like it was addressed to nothing — you were turning a wheel that had
//! no opinion about where it was until you let go. Stepping is a
//! *display* change only: the workspace tree is still written exactly
//! once, on release, so the indicator can range over the list freely
//! without a half-finished gesture switching anybody's workspace.
//!
//! **It has weight.** The disc does not track the pointer rigidly — a
//! thing with mass cannot change velocity instantly, and a surface
//! welded to the cursor is announcing that it has none. The drag sets a
//! target and a stiff critically-damped spring chases it, so the rim
//! trails the hand by an amount proportional to how fast you are moving.
//! Between notches a magnet pulls toward the nearer one, so crossing
//! from one to the next is felt as a small give and catch rather than a
//! smooth slide. A release keeps whatever velocity the wheel had: a
//! flick coasts several workspaces and decelerates, a slow let-go
//! settles where it stands. The settle is critically damped, which is
//! the opposite of bouncy — overshoot reads *light*.
//!
//! **The ends are ends.** The workspace list is finite and does not
//! wrap, so there is a first workspace and a last one. Dragging past
//! either does not run free and does not hit a wall: resistance builds
//! along `L·d/(d+L)`, tracking the hand exactly at first contact and
//! stiffening the whole way out, so you can always pull a little more
//! and never much more. Releasing returns to the end itself, not to
//! some detent beyond it. Scroll input hits the same edge with the same
//! curve — two entry points to one control have to resist alike.
//!
//! Note the difference between the *workspace list*, which is finite,
//! and the *display sectors*, which cycle: the sector fan tiles the
//! whole visible sweep by repeating workspaces so no unsegmented
//! stretch of band ever rotates through. That is a painting decision
//! and it is unaffected by the ends.
//!
//! User-facing spec (verbatim, 2026-04-21 + 2026-04-30):
//! *"No permanent sidebar, but workspaces appear as rotated axes of the
//! wheel disc, meaning you can go to the left and drag it to move between
//! workspaces."*
//!
//! Pixel-perfect concept reference:
//! `radial-workspace-switcher/src/App.tsx` (Apache-2.0). Maps:
//!   - 2000 px disc on a 160 px column → ratio 12.5:1 (radius:column).
//!     Concept centre at `left:-1840px` from the column-left → cos(θ)=0.84,
//!     visible arc spans `2·arccos(0.84) ≈ 66°`.
//!   - 24 display-spokes at 15° increments. We instead render N spokes
//!     for N workspaces but keep the per-spoke gap at `360°/N` so the
//!     snap math remains uniform; for `N == 24` it matches the concept.
//!   - Active spoke icon: 40×40 rounded-12 tile in workspace colour;
//!     uppercase 18-px label letter-spaced 0.15em, both rotated `-90°`
//!     so the cluster reads vertically (bottom-to-top on the right side).
//!   - Inactive spoke: 32×32 rounded-8 white/10 tile, 15-px white/80
//!     label, cluster opacity 0.30.
//!   - Static "centre dots": one per real workspace at `(i - (N-1)/2)·2°`
//!     near the rim, NOT rotating with the wheel. Active dot coloured +
//!     glowing; inactive are tiny white/20 specks.
//!   - Atmosphere: 40-px-thick `border-r-white/0.02` band, soft
//!     `40px 0 120px rgba(0,0,0,0.8)` shadow extending right, plus a
//!     faint disc fill (`bg-white/0.012`) substituting for backdrop-blur.
//!
//! Geometry mapping into our coord system:
//!   - `radius = visible_width / VISIBLE_RADIUS_FRACTION`.
//!   - `cx = rect.x0 - radius * (1 - VISIBLE_RADIUS_FRACTION)` — centre
//!     off-screen-left so the rim's chord at angles
//!     `±arccos(1 - VISIBLE_RADIUS_FRACTION)` lands on `rect.x0`.
//!   - `cy = (rect.y0 + rect.y1) / 2` — disc filled vertically by the
//!     parent slot, so its visible arc spans most of the viewport.
//!
//! Interaction model:
//!   - Press inside the visible arc → capture `drag_from`, and take the
//!     wheel over wherever it visually is, so grabbing one that is still
//!     settling catches it mid-flight instead of teleporting it.
//!   - Vertical motion: `raw = origin + dy * DRAG_FACTOR`
//!     (`DRAG_FACTOR = 0.15`), resisted past the ends, nudged toward the
//!     nearest detent, and handed to the follow spring as a target.
//!   - Click on a non-active spoke (lands inside its cached cluster
//!     rect) → animate that spoke to active via the settle spring.
//!   - Release → project the residual velocity forward, land on the
//!     detent nearest that projection, and settle there; or, if the
//!     gesture ended past an end, return to the end.
//!   - Wheel scroll: discrete detents, `WHEEL_DETENT_PX` of scroll per
//!     workspace. Against an end, where there is no next detent to
//!     spend on, the same input becomes resisted pull instead.
//!
//! ## Coordinates
//!
//! `rotation_deg` is **absolute**: workspace `i` rests at `-i * gap`, so
//! the whole list occupies `[-(N-1)·gap, 0]` and the reachable range has
//! two ends to run into. Everything that moves the wheel — drag, flick,
//! click, scroll — is a write to this one number, and `active_idx` is
//! always just "which workspace is `rotation_deg` nearest", recomputed
//! whenever it changes.
//!
//! `draw`, by contrast, needs the *residual*: the fan is painted around
//! whichever sector is active, so it wants the offset from that sector's
//! rest position (`visual_residual_deg`), not the absolute angle. Paint
//! the absolute angle and the lit sector marches down the arc and off
//! the bottom of the slot as you switch workspaces.
//!
//! Degrees throughout so the detent arithmetic (`rotation_deg / gap`)
//! stays readable; trig converts to radians at the callsite.
//!
//! Drag origin (`drag_from`) and idle-hover cursor cache (`hover_anchor`)
//! are stored in separate fields so pointer-capture is asserted only
//! while a press is actually in flight.

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
    /// Where the wheel is being asked to sit, in **absolute** degrees:
    /// workspace `i` rests at `-i * spoke_gap_deg()`, so the list spans
    /// `[-(N-1)·gap, 0]` and has an end at each side. Values outside
    /// that range are resisted overscroll, not positions.
    ///
    /// This is the *target*. What is on screen is the settle/follow
    /// spring's value while one is in flight — see
    /// [`SidebarWheelDisc::visual_rotation_deg`] — and this again once
    /// it lands. `draw` wants neither directly: it paints the fan
    /// around the active sector, so it uses
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
    /// drag→angle mapping. Default 4500 px — paired with
    /// `VISIBLE_RADIUS_FRACTION = 0.16` this gives a 360 px-wide visible
    /// arc, matching the live `SIDEBAR_WIDTH_PX = 360` slot post the
    /// 2026-05-23 redesign. Math: `visible_width = radius * VISIBLE`,
    /// so `radius = 360 / 0.16 = 2250`, `diameter = 4500`.
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
    /// Summoning exists because docking is correct but not always
    /// reachable: the disc belongs to the workspace, so scrolling right
    /// through a strip carries it off-screen along with everything else
    /// to the left. Without a way to call it back you would have to
    /// scroll the whole strip home just to glance at the workspace list.
    ///
    /// The footprint is unchanged — same width, same radius, same arc.
    /// A summon moves the same object to the pointer; it does not
    /// reveal more of it.
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
    /// at a different place on screen. Every field except
    /// [`Self::origin_px`] participates.
    ///
    /// This is the predicate behind the pan fast-path. Because the disc
    /// now rides the strip's canvas, its origin changes on every frame
    /// of a horizontal scroll — and re-encoding the full disc (fill,
    /// rim, every marker, the label, the add-slot) once per frame for
    /// what is a pure translation is exactly the cost that made
    /// scrolling feel heavy.
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

/// Fraction of the disc's radius that protrudes into the viewport.
/// Drives the off-screen-centre offset
/// (`cx = rect.x0 - radius * (1 - VISIBLE)`) and, with the slot's
/// height, how much of the rim is on screen.
///
/// This does NOT change the footprint. The visible width is whatever
/// the host's slot is; the radius is derived from it, so this constant
/// only sets how tightly the visible sliver curves — and, as a
/// consequence, how many degrees of rim fit in the slot's height.
///
/// It was 0.16, transcribed from the concept prototype's
/// 2000-px-disc-on-a-160-px-column. On the real slot that put the
/// radius near 1460 px, and a 690-px-tall slot spans
/// `2·asin(345/1460) ≈ 27°` of such a circle — less than the 15°
/// between two workspaces plus any margin. So the switcher showed the
/// active workspace and, at best, one neighbour clipping the very
/// corner: a wheel with nothing on it, and no sense of what you were
/// about to switch to.
///
/// At 0.34 the same slot spans ≈ 60°, which carries the active
/// workspace plus two neighbours on each side, and the sliver reads as
/// a gentle arc rather than a straight scratch.
pub(crate) const VISIBLE_RADIUS_FRACTION: f64 = 0.34;

/// Fractional vertical inset applied at both the top and bottom of the
/// taffy-allocated slot. The overlay host allocates the full sidebar
/// slot height to the disc's taffy node; we trim it here so the visible
/// footprint reads as deliberately inset from the topbar and floor
/// rather than running edge-to-edge.
///
/// It also buys the band its depth. The band has to bend back toward the
/// disc's centre to stay inside a tall, narrow slot, so height and depth
/// trade off directly: at the previous 0.10 the footprint was 3.7 times
/// taller than it was wide on a 900-px display and the band came out at
/// 129 px, too shallow to hold a workspace name without truncating it.
/// 0.16 costs 15% of the disc's height and returns a fifth more depth.
pub(crate) const SLOT_VERTICAL_INSET_FRAC: f64 = 0.16;

/// Concept's `DRAG_FACTOR` from `App.tsx:41` — degrees of rotation per
/// pixel of vertical drag.
const DRAG_FACTOR: f32 = 0.15;

/// Angular spacing between adjacent spokes. Concept's `ANGLE_STEP =
/// 360°/24 = 15°` (App.tsx:20, where `DISPLAY_WORKSPACES.length = 24`
/// because the 6 workspaces are repeated 4×). We use the same step so
/// the visual cadence matches; for `N < 24` the wheel cycles `N` per
/// `15° · N` of rotation.
const SPOKE_ANGLE_STEP_DEG: f32 = 15.0;

/// Pixels of accumulated wheel scroll required to advance one
/// workspace. The wheel uses *discrete detents*: scroll input
/// accumulates as raw pixels and every `WHEEL_DETENT_PX` worth of
/// movement triggers a `click_to_spoke` animation to the next
/// workspace. Away from the ends there is no free-running mid-detent
/// rotation — the wheel either sits at a workspace or is animating
/// between two — which reads as "attracted" (always lands on a
/// workspace, never drifts mid-state).
///
/// Against an end the model inverts: there is no next workspace to
/// animate to, so the input becomes resisted overscroll instead and
/// the wheel does move continuously. That is the one place where
/// swallowing the scroll would be worse than moving, because a dead
/// control at an end is indistinguishable from a broken one.
///
/// History: the previous continuous-rotation model combined with the
/// quadratic accumulator bug produced the "maddening speed" of
/// 2026-05-25, and a then-attempted fix (factor 0.006 linear) made
/// the wheel feel locked because tiny rotations rounded to the same
/// detent on snap so no visible motion occurred. Discrete detents
/// sidestep both failure modes at once.
///
/// At ~16 px per mouse-wheel click this is 3 clicks per workspace,
/// and roughly one trackpad flick.
///
/// This was 120 px — eight wheel clicks to move one workspace, which
/// its own tuning note described as "deliberate but not slow". Eight
/// clicks is not deliberate, it is a stuck control: the first several
/// clicks produce no visible response at all, because the detent model
/// has no mid-detent rotation to show progress with. The user reads
/// that dead zone as the switcher ignoring them.
///
/// The two historical failure modes this constant is guarding against
/// are both narrower than that note implies. The 2026-05-25 "maddening
/// speed" came from a quadratic accumulator, not from the threshold;
/// and the "wheel feels locked" regression came from continuous
/// free-running rotation whose sub-degree steps rounded to the same
/// detent on snap, so nothing moved. Neither is reachable from here:
/// accumulation is linear, and every crossing animates through
/// `click_to_spoke`. So the threshold is free to be responsive.
const WHEEL_DETENT_PX: f32 = 48.0;

/// Idle gap after which any sub-detent residual `wheel_accumulator`
/// is cleared. Without this, a partial swipe (say 80 px / 120 px)
/// would carry over into the next scroll burst and cause a workspace
/// switch from a movement the user didn't realise was queued. 250 ms
/// is long enough that a continuous flick isn't broken into separate
/// gestures, short enough that an obvious pause feels deliberate.
const WHEEL_SCROLL_TIMEOUT_MS: u64 = 250;

/// Where the disc's vertical fade reaches full strength, as a fraction of
/// the footprint's height (mirrored at the bottom). Everything the disc
/// paints is faded on this curve, so the whole object dissolves together
/// instead of some parts out-living others.
///
/// It was 0.22 — nearly half the footprint's height was a ramp, which is
/// most of why the body read as a graded smear rather than as a surface.
/// At 0.13 the band is flat across the three quarters of its run you
/// actually look at and only dissolves where it leaves the slot. It also
/// has to stay above `1 - BAND_TANGENT_FRACTION` so the tail the inner
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
/// The band's depth and the height it spans trade off against each other:
/// the arc has to bend back toward the centre to stay inside a tall,
/// narrow slot, and every pixel of height costs depth. Sizing the band so
/// the arc passes exactly through the footprint's corners spends the
/// whole budget on height and leaves the band too shallow to carry a
/// workspace name — at 1600x900 it came out at 109 px, which truncated
/// "Research" to "Res…".
///
/// Pulling the tangent point in buys that depth back. Past it the arc
/// does drift into the clip, but only across the last few per cent of
/// the footprint, where [`FADE_IN_STOP`] has already taken the band well
/// down on its way to nothing — so the cut has almost nothing to cut.
/// The two constants have to be read together: the clip begins at
/// `(1 - BAND_TANGENT_FRACTION) / 2` of the footprint's height, and that
/// has to stay a small fraction of `FADE_IN_STOP`.
/// `band_clip_starts_where_the_fade_has_already_taken_hold` pins it.
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
/// [`SECTOR_CHIP_H_ACTIVE`]. One step apart, so the set reads as one
/// family of things in two states rather than as two kinds of object.
const SECTOR_CHIP_H: f64 = 32.0;

/// Height of the active sector's chip. See [`SECTOR_CHIP_H`].
const SECTOR_CHIP_H_ACTIVE: f64 = 44.0;

/// Fill strength of the active sector's wedge, as a factor on the
/// workspace's own colour. Enough that the wedge is unmistakably the lit
/// one, quiet enough that the name on top of it stays the brightest thing
/// in the sector.
const ACTIVE_WEDGE_ALPHA: f32 = 0.17;

/// The same fill where the body beneath it is fully opaque.
///
/// The wedge is a wash *over* the body, so how much of it reaches the
/// eye depends on what the body is made of. Most themes give the disc a
/// translucent glass fill, and 17% of a colour over that reads clearly.
/// The high-contrast track makes every surface fully opaque black,
/// where the identical 17% is barely a colour at all — and that is the
/// one track where a state indicator failing to indicate is least
/// acceptable.
///
/// So the strength is interpolated on the body's own opacity rather
/// than switched on which theme is loaded: the widget never has to be
/// told, and any future theme with a solid surface gets the right
/// answer for the same reason. The label on top is the theme's primary
/// text — near-white on the opaque track — and holds its own against
/// the stronger wash easily.
const ACTIVE_WEDGE_ALPHA_ON_OPAQUE: f32 = 0.55;


/// Diameter of the add-workspace button, and how far down the band it
/// sits as a fraction of half the footprint's height. Sector labels that
/// would collide with it are dropped, so the affordance is never covered
/// however far the wheel has been turned.
const ADD_BUTTON_D: f64 = 34.0;
const ADD_BAND_FRACTION: f64 = 0.78;

/// Angular slop past the last on-screen degree that sectors are still
/// built for. Cheap insurance that a divider or a tint never stops short
/// of the footprint's corner.
const SWEEP_SLOP_DEG: f64 = 8.0;

/// Per-spoke palette. Maps a workspace's *index* in the tree to one of
/// six concept colours (`pink-500`, `blue-500`, `amber-500`,
/// `emerald-500`, `purple-500`, `zinc-400` — see
/// `radial-workspace-switcher/src/App.tsx:10-17`). Wraps modulo for
/// trees with more than six workspaces.
impl SidebarWheelDisc {
    /// Visible width — `radius * VISIBLE_RADIUS_FRACTION`. For the
    /// default 1200 px diameter (radius 600), this is 96 px,
    /// matching the live `SIDEBAR_WIDTH_PX`.
    pub fn visible_width_px(&self) -> f64 {
        (self.diameter_px / 2.0) * VISIBLE_RADIUS_FRACTION
    }

    /// Construct a fresh disc with the given spoke labels. Starts at
    /// `active_idx = 0`, `rotation_deg = 0.0`, default 1200 px diameter
    /// (gives a 96 px visible-arc width — paired with the host slot).
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
            diameter_px: 4500.0,
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
    /// layout math without binding to the 240 px default).
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

    /// Degrees between adjacent spokes. Concept-faithful: a fixed 15°
    /// regardless of `N`, matching `App.tsx`'s `360°/24 = 15°` cadence.
    /// This gives the same visual density the reference shows
    /// (active + ±15° neighbours visible inside the 66° arc band).
    /// Snap math: `active_idx` advances by `-nearest_offset mod N`, so
    /// a full 360° rotation wraps `24 % N` workspaces — for `N < 24`
    /// the wheel cycles through workspaces multiple times per
    /// revolution but each spoke always lands at a multiple of 15°.
    pub fn spoke_gap_deg(&self) -> f32 {
        // Single-workspace disc is treated as 360° to avoid the wheel
        // scrolling past itself unnecessarily.
        if self.spokes.len() <= 1 {
            360.0
        } else {
            SPOKE_ANGLE_STEP_DEG
        }
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
    /// A sine of the sub-detent phase, which gives the three properties
    /// a notch needs: zero exactly on a detent (so a resting wheel is
    /// not pushed off it), zero exactly between two (so the midpoint is
    /// a real if unstable balance and the user can hold the wheel
    /// there), and steepest a quarter of the way in either direction,
    /// which is what turns crossing a boundary into a give-then-catch
    /// instead of a slide.
    ///
    /// Suppressed past the ends: out there the only force that should
    /// be acting is the rubber band pulling back in.
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
    /// The velocity is the whole point at release: it is what the hand
    /// left behind, and carrying it into a heavier spring is what makes
    /// a flick coast and decelerate instead of snapping.
    /// `from_hint` is where the wheel visibly is *before* the caller
    /// moved the target, and it is load-bearing whenever no spring
    /// survives from the previous phase: seeding a fresh spring from the
    /// already-updated `rotation_deg` would start it on its destination,
    /// so it would report settled immediately and the whole settle would
    /// happen in one invisible frame.
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
    /// `±arccos(1 - VISIBLE_RADIUS_FRACTION)` from due-east — about ±49°
    /// at the current fraction, with the slot's height deciding how much
    /// of that is actually on screen.
    fn center(&self) -> Point {
        let radius = self.diameter_px / 2.0;
        let cx = self.rect.x0 - radius * (1.0 - VISIBLE_RADIUS_FRACTION);
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        Point::new(cx, cy)
    }

    /// The workspace's identifying colour, derived from the theme.
    ///
    /// The disc used to index six literal Tailwind hexes here, which
    /// meant the first workspace arrived as `pink-500` under every
    /// theme — at the active wedge's 17% alpha over a dark glass fill,
    /// a muddy maroon that no theme could reach and nobody chose.
    /// Theming in qOS belongs to the user, and a constant in a widget
    /// is the one place they cannot reach.
    ///
    /// [`ui::tokens::Colors::categorical`] grows the series from the
    /// theme's own accent instead, so each theme gets workspace colours
    /// in its own register while adjacent ones stay a plain 60° of hue
    /// apart.
    fn spoke_color(&self, idx: usize, colors: &ui::tokens::Colors) -> Color {
        colors.categorical(idx)
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
    /// Distinct from [`SidebarWheelDisc::active_idx`], which is the
    /// *indicator* and tracks wherever the disc visibly is at this
    /// instant. The two are equal at rest and differ for exactly as long
    /// as a gesture or its settle is in flight — which is the whole
    /// point of the split: the highlight is free to step across the list
    /// under the user's hand while the workspace tree is written once,
    /// at the end, with the answer.
    pub fn selected_idx(&self) -> usize {
        self.idx_at_rotation(self.rotation_deg)
    }

    /// The visible rotation expressed relative to the active sector's
    /// rest position — what `draw` turns the fan by.
    ///
    /// The fan is built outward from the active sector, so it needs the
    /// offset from that sector's home, not the absolute angle. Feeding
    /// it the absolute angle instead is what would send the lit sector
    /// marching one notch further down the arc with every workspace you
    /// switch to, until it left the slot entirely.
    ///
    /// At rest this is zero. During a gesture it ranges over roughly
    /// ±half a detent before the indicator steps and it wraps back, and
    /// further than that only when the wheel is being held past an end.
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
    if gap_deg >= 180.0 {
        0
    } else {
        (sweep_rad.to_degrees() / gap_deg).ceil() as i32 + 1
    }
}

/// Point on the circle of radius `r` about `c` at `angle` radians,
/// measured from due-east and increasing downward (screen coordinates).
fn polar(c: Point, r: f64, angle: f64) -> Point {
    Point::new(c.x + r * angle.cos(), c.y + r * angle.sin())
}

/// Closed path for the slice of ring between radii `r_in`/`r_out` and
/// angles `a0`/`a1`.
///
/// The arcs are walked as polylines rather than built from `kurbo::Arc`
/// so the two ends join into ONE closed subpath — a shape with a single
/// winding, which is what lets it be filled flat with no seam where the
/// two boundaries meet. Step count is chosen so the chord error stays
/// far below a pixel at the radii this widget uses: the sweep is at most
/// a few tens of degrees, so 96 steps put the sagitta under 0.05 px even
/// on a 2000-px disc.
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
/// `radial-workspace-switcher/src/App.tsx` for the concept's six ids,
/// then extends the table with `Home` — the one workspace
/// `WorkspaceTree::bootstrap_default` starts a machine with — and
/// `Research`, so a first-boot spoke and the demo tree's spokes paint a
/// recognisable glyph instead of falling through to the generic `Layers`
/// default. Inputs are expected
/// to already be lower-cased by the caller; matches are exact ASCII so
/// upstream label-rename ripples here as a missed default. `dev` and
/// `development` both resolve to `Code` (the concept's id is `dev` but
/// the visible label is "Development"). Anything unknown falls back to
/// `Layers`, matching the concept's default tile glyph.
fn spoke_icon_for_label(label: &str) -> Icon {
    match label {
        "design" => Icon::PenTool,
        "dev" | "development" => Icon::Code,
        "marketing" => Icon::FileText,
        "analytics" => Icon::BarChart,
        "concept" => Icon::Layers,
        "settings" => Icon::Settings,
        // `Home` is the first-boot workspace; `Research` comes from the
        // demo tree. `Home` paints lucide's house glyph; `Research` borrows the
        // magnifier (Lucide's `search`) since it's the closest semantic
        // match in the existing icon set.
        "home" => Icon::Home,
        "research" => Icon::Search,
        _ => Icon::Layers,
    }
}

// (Helpers `cluster_label_tail_screen_x` and `build_label_text` lived
// here in the prior pill-card design — every spoke had a rotated cluster
// whose label could overrun the left column edge, so we measured the
// rotated tail and truncated. The 2026-05-23 quiet-ring redesign drops
// per-spoke labels entirely (only the active spoke gets an upright
// horizontal label, drawn in screen space inside the panel) so neither
// helper is needed any more.)

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
        // The disc's logical centre lives off-screen-left of `rect.x0`
        // and only the right ~66° arc paints inside this rect. The
        // hit-testing in `handle_event` uses `hit_visible_arc` so clicks
        // in the rect's corners (outside the arc) don't grab the wheel.
        //
        // 2026-05-23: trim the slot vertically by `SLOT_VERTICAL_INSET_FRAC`
        // at both ends so the wheel reads as roughly 10%–90% of the
        // available sidebar height. Without this trim the disc runs
        // edge-to-edge inside the column and looks like a sliver.
        let x0 = origin.x + l.location.x as f64;
        let y0_raw = origin.y + l.location.y as f64;
        let h_raw = l.size.height as f64;
        let inset_v = h_raw * SLOT_VERTICAL_INSET_FRAC;
        let w = l.size.width as f64;
        let y0 = y0_raw + inset_v;
        let y1 = y0_raw + h_raw - inset_v;
        self.rect = match self.summon_at {
            // Docked: the slot's own box, which the host has already
            // shifted by the active strip's pan.
            None => Rect::new(x0, y0, x0 + w, y1),
            // Summoned: same width and height, re-placed around the
            // pointer. The rim's rightmost point is exactly `rect.x1`
            // (the disc centre sits `radius * (1 - VISIBLE)` to the
            // left of `rect.x0`), and the active marker rides that
            // point — so putting `x1` on the pointer puts the marker
            // under it. Vertically the disc is centred on the pointer
            // for the same reason.
            //
            // Both axes are then clamped back inside the slot's band so
            // a summon near an edge slides into view instead of hanging
            // off it. The size never changes, so the clamp can only
            // move the disc, never grow or crop it.
            Some(p) => {
                let slot_top = y0_raw;
                let slot_bottom = y0_raw + h_raw;
                let height = y1 - y0;
                let x1 = p.x.max(origin.x + w);
                let top = (p.y - height / 2.0).clamp(slot_top, (slot_bottom - height).max(slot_top));
                Rect::new(x1 - w, top, x1, top + height)
            }
        };
        // Keep the disc's curvature locked to the live rect width — the
        // host (overlay_host) overrides the sidebar slot to a viewport-
        // driven width, so the visible-arc width changes on resize. We
        // recompute `diameter_px` from rect.width() / VISIBLE so the
        // visual arc-fraction stays identical regardless of how wide
        // the slot ends up. Constructor / `with_diameter` callers
        // remain useful for early frames (before the first layout pass
        // runs) and for unit tests that hand-build the disc with a
        // synthetic rect.
        let rw = self.rect.width();
        if rw > 1.0 {
            self.diameter_px = rw / VISIBLE_RADIUS_FRACTION * 2.0;
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
        // Cache fast-path. The disc paints a *lot* per frame — eight
        // shadow bands, the full-disc fill, the rim band stroke, the
        // hairline stroke, every visible spoke's cluster (tile + glyph
        // + label, each with a `Affine::rotate` per spoke), plus the
        // "+" add-workspace slot. At rest (no rotation animation), all
        // of those are identical frame-to-frame.
        //
        // We fingerprint every input that affects the painted pixels —
        // rect, rotation (centi-degree bucket so the spring's 60-Hz
        // sub-degree steps still invalidate), active index, spoke
        // labels, diameter — and replay the cached scene with the
        // caller's transform on a hit. During an animation every
        // frame still rebuilds; once the spring settles back to
        // `rotation_deg`, subsequent frames take the fast path.
        //
        // `spoke_hit_rects` and `plus_rect` live in member fields
        // already; the prior cache-miss draw populated them at the
        // same rotation/rect, so a cache hit implies they're still
        // valid.
        let fp = self.current_fingerprint();
        // Two fast paths, both requiring a previously recorded scene.
        //
        // 1. Identical fingerprint — replay verbatim. The at-rest case.
        //
        // 2. Same content at a new origin — the disc rides the strip's
        //    canvas, so a horizontal pan moves it without changing a
        //    single painted pixel. Replay under a translation instead
        //    of re-encoding. The cached geometry was recorded in
        //    absolute coordinates, so the delta is measured from the
        //    origin it was recorded at, NOT from the last frame's.
        //    The cached hit rects were recorded in those same absolute
        //    coordinates, so they are re-derived from the delta too —
        //    otherwise a click after a pan would test against where
        //    the markers used to be.
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
        // The visible object is an annular BAND, not a whole disc: a
        // constant-depth ring between an inner arc and the rim. The
        // depth is the largest that keeps the inner arc inside the
        // footprint everywhere it is on screen — place it any deeper
        // and the arc runs out through the footprint's left edge and
        // the band ends on a straight vertical cut instead of a curve.
        //
        // `theta_lim` is where the rim itself leaves through the top and
        // bottom edges; at exactly that angle the inner arc is tangent
        // to the left edge, and both are inside the vertical fade by
        // then anyway.
        let half_h = self.rect.height() / 2.0;
        let r_out = radius;
        // The inner arc has to clear the footprint's left edge over its
        // OWN sweep, which runs further than the rim's: the rim exits
        // through the top and bottom edges at `theta_lim`, but the inner
        // arc is still climbing at that angle and only reaches the same
        // height further round. Sizing the band off `theta_lim` therefore
        // let the arc drift left of the edge for the last quarter of its
        // run, where the clip turned it into a straight vertical cut with
        // the vertical fade nowhere near it yet.
        //
        // `sqrt(d^2 + half_h^2)` is the radius whose arc passes exactly
        // through the footprint's two left-hand corners, so the band is
        // as deep as it can be while ending on a curve everywhere.
        let d_left = (self.rect.x0 - cx + BAND_LEFT_INSET).max(1.0);
        // The body reaches the centre, so there is no inner radius to
        // derive it from any more. `r_ref` is the same radius the inner
        // arc used to sit at, kept as a pure *layout* reference: it is
        // where the labels and the add-workspace button live, and
        // holding it fixed is what keeps the solid fill from moving
        // anything else on the disc.
        let tangent_dy = half_h * BAND_TANGENT_FRACTION;
        let r_ref = (d_left * d_left + tangent_dy * tangent_dy)
            .sqrt()
            .clamp(4.0, r_out - 12.0);
        // How far round the disc has to be built for the footprint to be
        // covered. A radial at angle θ crosses the footprint's left edge
        // `d_left` from the centre at a height of `d_left·tan θ`, so
        // `atan(half_h / d_left)` is the angle whose radial passes
        // exactly through the footprint's corner and nothing beyond it
        // can show. Plus the usual slop so a divider entering the slot
        // is already drawn when its leading edge appears.
        //
        // Derived from the footprint rather than from a radius, now that
        // the fill has no inner one — and numerically the same sweep the
        // inner arc used to give, so no sector count changes.
        let sweep = (half_h / d_left).atan() + SWEEP_SLOP_DEG.to_radians();

        // ---- Pass 1: the band -------------------------------------------
        // Body, edges and the shadow that lifts the wheel off whatever is
        // behind it are ONE isolated group. Everything inside paints at
        // full strength; a single mask at the end fades the whole group
        // out towards the top and bottom of the footprint.
        //
        // The fade is what lets the band be a slice. The footprint is far
        // taller than the piece of ring that would close inside it, so the
        // band enters through the top edge and leaves through the bottom
        // one — it never meets itself on screen. Any hard clip would
        // therefore end it on a straight horizontal cut in open wallpaper,
        // which instantly reads as a bug. By the time the clip bites there
        // is nothing left to cut.
        //
        // The horizontal clip IS load-bearing: an unclipped ring would
        // sweep back around off-screen-left and reappear nowhere useful.
        // It runs a little past the footprint's right edge so the rim's
        // outward shadow has somewhere to land.
        //
        // The fill is FLAT. One colour, the theme's own glass surface —
        // the same token the modal surfaces use, so the wheel is made of
        // the same material as the rest of the chrome. The body used to
        // be a nine-stop radial ramp from transparent to a peak alpha at
        // the rim; on screen that is a grey smear that never resolves
        // into an object, and it is the single thing that made the disc
        // look unfinished.
        let body_hue = colors.surfaces.glass;
        // Edges are "light catching a rim", so they track the theme's
        // foreground rather than a hardcoded white — otherwise the wheel
        // is a white-on-white scratch under the light theme and loses its
        // only structural line under high contrast, where the white-tinted
        // border tokens collapse to near-invisible.
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

        // Shadow just OUTSIDE the rim, so it follows the arc — the same
        // soft multi-ring falloff the window-decoration pills use, bent
        // around a circle. Painted first so the band sits on top of it.
        // Tinted from the theme's own elevation token rather than from a
        // literal black, so a theme that lifts surfaces differently — or
        // suppresses shadows entirely, as high contrast does by putting
        // everything on pure black behind a white rim — gets its own
        // answer here instead of this one.
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
        // placed, so labels can be kept out of it.
        // Placed by how far down the band it sits rather than by how far
        // up from the footprint's floor: the floor is inside the vertical
        // fade, and a button pinned to it ended up hanging under the band
        // on bare wallpaper — the exact complaint that moved this
        // affordance twice before.
        let add_cy = cy + half_h * ADD_BAND_FRACTION;
        let add_dy = add_cy - cy;
        // Tucked against the band's INNER arc rather than floating in the
        // middle of it. Labels run the band's full radial depth, so a
        // button parked mid-band sits squarely in the path of whichever
        // one sweeps over it, and the sector that loses its name is the
        // one thing a switcher cannot afford to drop. At the inner edge
        // the two rarely meet at all.
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
                let idx = ((self.active_idx as i32 + k).rem_euclid(n_i)) as usize;
                let is_active = k == 0;
                let base = self.spoke_color(idx, colors);

                // Active sector: the wedge itself is lit in the
                // workspace's colour. This is what replaces the old
                // name-plate — the *sector* is the selection, so the
                // thing that turns is also the thing that shows state.
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
                    // From the centre, not from an inner arc: with the
                    // body solid, a divider that stopped part-way would
                    // leave the surface tinted but undivided over the
                    // rest of its depth. Everything left of the
                    // footprint is clipped away regardless.
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

        // The rim — the body's one lit edge, and with the inner arc gone
        // it is carrying the silhouette on its own, so it keeps the full
        // window-decoration language: a soft wide stroke reading as
        // light on a bevel, then a crisp hairline that gives the edge a
        // definite position, then an inset glow.
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
        // Each label is a chip laid ON the disc: built in the sector's own
        // frame (a horizontal box on the due-east radius) and then rotated
        // about the disc's centre to its sector. Every chip is therefore
        // at the same radius and the same width — a ring of labels that
        // tips with the wheel.
        //
        // Rotating the label with its sector is the point. An upright
        // label beside a turning disc reads as a list that happens to sit
        // near a curve; a label that tips reads as paint on the surface.
        let chip_x1 = cx + r_out - LABEL_RIM_GAP;
        let chip_x0 = cx + r_ref + LABEL_INNER_INSET;
        let chip_w = chip_x1 - chip_x0;
        if !self.spokes.is_empty() && chip_w >= LABEL_MIN_W {
            let n = self.spokes.len();
            let n_i = n as i32;
            let gap = self.spoke_gap_deg() as f64;
            let rot = self.visual_residual_deg() as f64;
            let slots = slot_reach(sweep, gap);

            for k in -slots..=slots {
                let mid_deg = rot + k as f64 * gap;
                let mid = mid_deg.to_radians();
                if mid.abs() > sweep {
                    continue;
                }
                let idx = ((self.active_idx as i32 + k).rem_euclid(n_i)) as usize;
                let is_active = k == 0;
                // Where the sector's centreline meets the rim — the
                // label's own y, and what the vertical fade is sampled at.
                let anchor = polar(c, r_out, mid);
                let fade_a = row_fade(anchor.y);
                if fade_a <= 0.02 {
                    continue;
                }

                let row_h = if is_active { SECTOR_CHIP_H_ACTIVE } else { SECTOR_CHIP_H };
                let chip = Rect::new(chip_x0, cy - row_h / 2.0, chip_x1, cy + row_h / 2.0);
                let rotate = Affine::rotate_about(mid, c);

                // Where the chip actually lands once rotated. Used for the
                // click target and to keep labels off the add button.
                let placed = rotate.transform_rect_bbox(chip);
                // Does this label actually run into the add button?
                //
                // Answered in the label's OWN frame — the button's centre
                // rotated back onto the due-east radius, tested against
                // the unrotated chip grown by the button's radius. The
                // axis-aligned bounding box of a rotated chip is far
                // bigger than the chip: at 15° a 160-px label's box picks
                // up 30 px of empty corner at each end, which was enough
                // to report a clash with a button the label passes 30 px
                // clear of, and cost the bottom sector its name.
                let local = rotate.inverse() * self.plus_rect.center();
                let reach = ADD_BUTTON_D / 2.0 + 4.0;
                let clash = local.x >= chip.x0 - reach
                    && local.x <= chip.x1 + reach
                    && local.y >= chip.y0 - reach
                    && local.y <= chip.y1 + reach;
                if clash {
                    continue;
                }

                // The sector is the container, so the label does not
                // bring a pill of its own — a pill inside a lit wedge is a
                // box inside a box, and it was the pill that made the old
                // rail read as a list parked next to a curve.
                //
                // The active label is the theme's primary text and a
                // size step larger; the rest take its secondary text.
                // Colouring the active label with its workspace's own hue
                // put it on a wedge of that same hue, which is the worst
                // place for it — the identity is already carried by the
                // wedge, and the name's only job there is to be the most
                // legible thing in the sector.
                let style = if is_active {
                    ChipStyle::bare(None)
                } else {
                    // Quiet, but how quiet is the palette's call, not
                    // ours. Dimming the primary text colour by a fixed
                    // factor is exactly wrong under high contrast, where
                    // the whole point is that nothing fades: that palette
                    // deliberately puts `text_dim` at near-white so
                    // secondary copy still clears AAA on black.
                    ChipStyle::bare(Some(colors.text_dim))
                }
                .with_alpha(fade_a);
                Chip::draw_at(
                    scene,
                    transform * rotate,
                    chip,
                    &self.spokes[idx].text,
                    ChipLead::Glyph(self.spoke_icon(idx)),
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
        // footprint, in the same material as the rim: translucent fill,
        // hairline edge. It does NOT rotate, and that is deliberate — it
        // is not a workspace, it is the control that makes one, so a spot
        // on the wheel would carry it out of reach every time the wheel
        // turned. It sits on the band rather than floating on bare
        // wallpaper, which is what the flat fill buys: there is now a real
        // surface underneath it everywhere it can land.
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
                    // Take the wheel over wherever it visually is. If a
                    // settle was still running, grabbing it should catch
                    // it mid-flight the way a hand on a spinning wheel
                    // does — anchoring on `rotation_deg` instead would
                    // teleport the disc to the settle's destination the
                    // instant you touched it.
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
                // Discrete-detent model. Accumulate raw pixels of
                // scroll. Every `WHEEL_DETENT_PX` worth in either
                // direction triggers a workspace advance via the
                // shared `click_to_spoke` animation (so the visual is
                // an actual spring settle, not a teleport). Away from
                // the ends there is NO mid-detent rotation — the wheel
                // either rests on a workspace or is animating to the
                // next one.
                //
                // At an end that model has nothing to offer: there is
                // no next workspace, so every pixel would vanish and
                // the control would go dead exactly where the user most
                // needs to be told something. So the unspendable
                // portion is diverted into the same resisted pull a
                // rim-drag gets there.
                self.wheel_accumulator += dy;
                self.wheel_last_event = Some(Instant::now());
                let n = self.spokes.len();
                if n > 1 {
                    while self.wheel_accumulator.abs() >= WHEEL_DETENT_PX {
                        let dir: i32 = if self.wheel_accumulator < 0.0 { 1 } else { -1 };
                        let target = self.active_idx as i32 + dir;
                        self.wheel_accumulator -= dir as f32 * WHEEL_DETENT_PX;
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
        // easing, while a wheel-scroll snap-back is pending, OR while
        // a finger-drag is in flight.
        //
        // The drag arm is what makes the rim track the pointer. A drag
        // is a stream of `CursorMoved`, and the host's incremental
        // gate skips the redraw on a pointer move unless something in
        // the tree reports work to do. Without this arm the rim's
        // `rotation_deg` advanced on every move but no frame was ever
        // requested, so the wheel sat still under the finger and only
        // jumped once the button came back up — the whole gesture read
        // as unresponsive rather than as dragging a physical wheel.
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
        let mut keep = self.tick_rotation_spring(dt);
        // The indicator steps off the *visible* rotation, so it has to
        // be re-derived on every frame the spring moves — that is what
        // carries the stepping through a flick's coast as well as
        // through the drag itself.
        self.sync_active_from_visual();
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

    /// The disc reports clean once the cache has been populated. Animation-
    /// driven invalidation comes through [`Widget::needs_animation_frame`]
    /// (`App` pumps redraws on its own while the spring or wheel snap-
    /// timeout is in flight) and the fingerprint check inside `draw`
    /// detects the rotation delta on each pumped frame.
    ///
    /// Without this override the default leaf impl returns `true` every
    /// frame, defeating the App's "skip the redraw on a hover-only
    /// CursorMoved" gate.
    fn needs_redraw(&self) -> bool {
        // Dirty exactly when the next paint would differ from the one
        // already on screen: compare the live fingerprint against the
        // cached one. A fresh widget has an unpopulated cache and so
        // reports dirty until the first paint.
        //
        // The previous implementation reported clean unconditionally
        // once anything had been drawn, on the theory that animation
        // would be pumped through `needs_animation_frame`. That holds
        // for the snap spring, but not for the two states driven
        // straight off pointer input: a rim drag, and the strip pan
        // that carries the disc along the canvas. Both mutate what the
        // disc would paint from inside a `CursorMoved`, which is the
        // one event class the host declines to repaint on unless the
        // tree reports dirty — so the wheel stopped tracking the input
        // that was moving it.
        // A spring in flight is dirty by definition: it will have moved
        // by the time the next paint happens, even if this frame's
        // fingerprint still matches the last one.
        //
        // That is not a hypothetical. It is exactly the state the FIRST
        // frame of a drag is in, now that the rim follows the pointer
        // through a spring rather than being welded to it: the target
        // has moved but the spring has not been stepped yet, so the
        // visible rotation — and with it the fingerprint — is still
        // last frame's. Reporting clean there deadlocks the whole
        // gesture, because the host skips the repaint, the repaint is
        // what pumps `update`, and `update` is the only thing that
        // would have moved the spring.
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
    /// `plus_rect` are absolute-coordinate caches populated by the last
    /// full paint, so without this a click after a pan would resolve
    /// against the markers' old screen positions — the wheel would look
    /// right and click wrong.
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
    /// `rotation_deg > 0`. The nearest snap offset is negative (we
    /// round to the nearest multiple of `spoke_gap`, which for a small
    /// positive drag is 0 and for a larger one is the next multiple).
    /// `active_idx` advances by `-nearest_offset` so dragging down
    /// brings spoke `+1` into view — matching the user's mental model
    /// of "pull the wheel down to reveal the next workspace below".
    pub fn snap_to_nearest(&mut self) {
        let idx = self.idx_at_rotation(self.rotation_deg);
        self.commit_to_idx(idx);
    }

    /// Make `idx` the chosen workspace: park the rotation on its detent
    /// and write it through to the tree.
    ///
    /// Because the rotation is absolute, this is idempotent — running it
    /// twice on an unmoved wheel lands on the same workspace. The
    /// previous relative form advanced `active_idx` by however many
    /// detents `rotation_deg` was from zero and then *left the rotation
    /// there*, so a second snap re-applied the same shift, and the
    /// selection walked further from the wheel with every gesture.
    fn commit_to_idx(&mut self, idx: usize) {
        let idx = idx.min(self.spokes.len().saturating_sub(1));
        self.rotation_deg = self.rotation_for_idx(idx);
        self.active_idx = idx;
        self.wheel_overscroll_px = 0.0;
        self.publish_active_to_chrome(idx);
    }

    /// Decide where a released drag ends up, and start it on its way.
    ///
    /// Three cases, in the order they are asked:
    ///   - let go past an end → back to the end, because there is
    ///     nothing out there to land on;
    ///   - let go moving → project the velocity forward over
    ///     `fling_projection_ms` and take the detent nearest that, so a
    ///     flick crosses several workspaces;
    ///   - let go still → the projection collapses to the current
    ///     position and this is just "nearest detent".
    ///
    /// The velocity comes straight off the follow spring, which has been
    /// tracking the pointer all gesture: no separate velocity estimator,
    /// and it decays on its own if the user stops before letting go —
    /// which is what makes "drag, pause, release" land where it looks
    /// like it should instead of flinging.
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
            // Project from where the gesture *ended* — `rotation_deg`,
            // the drag target — rather than from where the rim has got
            // to. The rim trails by the follow lag, and choosing the
            // destination from the trailing position would quietly hand
            // back part of every drag: you would pull the wheel a full
            // notch, let go, and watch it settle back to the one you
            // started on. The lag is meant to be visible, not to be
            // subtracted from the gesture.
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
    /// No shortest-path-around-the-back any more: the list has ends, so
    /// there is exactly one way from here to there and it is the direct
    /// one. That is also what the sectors show — a click on a workspace
    /// two above you should turn the wheel two notches up, not most of a
    /// revolution down.
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
    /// Returns whether there was a settle to sample. What renders is the
    /// production spring — the same `Spring`, the same constants, the
    /// same integrator — advanced at the same fixed 60 Hz step the live
    /// loop uses, not a reconstruction of the curve. Mirrors the reveal
    /// and make-space sampling hooks: a still cannot show weight, and a
    /// render costs more wall time than the settle lasts, so stepping
    /// real frames only ever catches the endpoints.
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
    /// This is *not* the extent of the painted surface. The body is a
    /// solid segment reaching the centre; what this bounds is the
    /// readable ring near the rim where a workspace name fits, which is
    /// derived rather than configured — see [`BAND_TANGENT_FRACTION`].
    /// Exposed so callers that need to know where the labels actually
    /// land (the snapshot tests sample it, the add button is placed
    /// against it) do not re-derive the expression and drift out of step.
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

    /// The wheel is a ring, so the sectors have to be balanced around the
    /// workspace you are on. Offsetting by the raw index difference put
    /// every other workspace on ONE side: with three workspaces and the
    /// first one active, the whole upper arc was empty and the other two
    /// stacked below it.
    #[test]
    fn sectors_wrap_around_the_active_workspace() {
        let mut d = mk(&["Home", "Research", "Design"]);
        layout_into_slot(&mut d, live_slot());
        paint(&mut d);

        let cy = (d.rect.y0 + d.rect.y1) / 2.0;
        let placed: Vec<Rect> = d
            .spoke_hit_rects
            .iter()
            .copied()
            .filter(|r| r.area() > 0.0)
            .collect();
        assert_eq!(placed.len(), 3, "all three workspaces are on screen");
        assert!(
            placed.iter().any(|r| r.center().y < cy - 1.0),
            "a workspace has to sit above the active one: {placed:?}"
        );
        assert!(
            placed.iter().any(|r| r.center().y > cy + 1.0),
            "a workspace has to sit below the active one: {placed:?}"
        );
    }

    /// The add-workspace button does not rotate, so a sector label sweeps
    /// past it on every turn of the wheel. The button has to stay usable
    /// throughout — a control the wheel can bury is what drove this
    /// affordance off the rim and then off the disc entirely.
    ///
    /// Drives the real press path at every position a label passes
    /// through between detents, rather than comparing rectangles: the
    /// press handler resolves the button before it resolves a sector, and
    /// that ordering is the thing that has to hold.
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

    /// The add button has to sit ON the band, at every rotation. Pinned to
    /// the footprint's floor it hung underneath, on bare wallpaper, which
    /// is what moved this affordance twice before — the fix was a surface
    /// to sit on, not another spot to float in.
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
    /// The collision test used to run on the axis-aligned bounding box of
    /// the rotated label, which at 15° picks up 30 px of empty corner at
    /// each end — enough to report a clash with a button the label passes
    /// well clear of, so the bottom sector silently lost its name.
    #[test]
    fn a_sector_keeps_its_name_when_it_only_passes_near_the_add_button() {
        let mut d = mk(&["Home", "Research", "Design"]);
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
    /// vertical cut. That is survivable only while the vertical fade has
    /// already taken the band down — a crisp cut through a surface at full
    /// strength is the "someone drew a box" failure the curve exists to
    /// avoid.
    ///
    /// Asserts on the two production constants that decide it rather than
    /// on a rendered pixel, because the relationship is what has to hold:
    /// change either one alone and this fails.
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
    /// workspaces. Indexing sectors off the workspace list left a stretch
    /// of unsegmented band rotating through whenever there were fewer
    /// workspaces than the sweep had room for — a wheel with a bald patch,
    /// which is the one thing that stops it reading as a wheel.
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
    /// generic `Layers` default. Without this guard `Home` and
    /// `Research` both painted the layered-stripes glyph in
    /// `target/inspect/desktop.png`, making the active spoke unreadable.
    /// Also pins the public `spoke_icon` API to confirm it lower-cases
    /// the label before resolving.
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
    /// actually-in-flight drag (B-3 / 3-B). The hover anchor is cached
    /// for the next press's hit-test in a separate field.
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
    /// so the next idle hover doesn't continue to swallow events
    /// (B-3 / 3-B).
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
        // Concept-faithful: 15° between adjacent spokes regardless of N.
        assert!((gap - 15.0).abs() < 1e-4);

        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        assert!(d.drag_from.is_some(), "press inside disc must start drag");
        assert!(d.wants_pointer_capture());

        // With DRAG_FACTOR = 0.15 and a 15° gap, a `15/0.15 = 100 px`
        // drag is exactly one gap. Add a margin to land cleanly past.
        //
        // Dragged UPWARD, because the list no longer wraps: the disc
        // starts on workspace 0, which is the first one, and pulling
        // down from there is pulling against the top end rather than
        // travelling to a neighbour. Up is where the rest of the list
        // is.
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
        // 4-spoke disc, concept-faithful 15° gap. With DRAG_FACTOR = 0.15,
        // a 73 px drag gives ~10.95° of rotation — between gaps (rounds
        // to 15°), guaranteeing `pre_snap_deg != snapped_deg` and a
        // non-trivial spring animation.
        let mut d = mk(&["A", "B", "C", "D"]);
        assert!((d.spoke_gap_deg() - 15.0).abs() < 1e-4);
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);

        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y - 73.0));
        let pre_snap_deg = d.rotation_deg;
        assert!(pre_snap_deg < 0.0);
        // Let the follow spring actually reach the drag target before
        // letting go. The rim trails the pointer now, so "where the
        // wheel is when you release" is only a meaningful quantity once
        // some frames have run — releasing on the same tick as the move
        // means the wheel genuinely never got anywhere.
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
        // Drive the spring with a deterministic 1/60 s dt (production
        // uses real wall-clock dt via `update`; the test pumps the
        // extracted stepper so a tight loop doesn't collapse to ~0 dt).
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

        // Lay out and paint for real, then click whatever the paint pass
        // decided the add row's box is. Synthesising the rect here instead
        // — which this test used to do, to "decouple from the draw-time
        // arithmetic" — meant it went on passing while the production
        // geometry moved underneath it, so it proved the press handler
        // works and nothing at all about the affordance being reachable.
        // `draw` needs no GPU: it encodes into a `Scene`.
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
        // returns early before the drag-anchor block. This guards against
        // the "press-on-plus also grabs the rotation" regression.
        assert!(
            d.drag_from.is_none(),
            "plus-press must not start a rotation drag"
        );
    }

    /// Closes wheel-disc-design known-gap #3: each spoke should render
    /// one presence dot per peer currently registered on its workspace
    /// key (up to 3). Seeds two peers on workspace 77, builds a disc with
    /// a single spoke keyed to that workspace, and asserts the helper
    /// reports exactly 2 dots — matching the `presence.peers_on(...)`
    /// count for that spoke's `PresenceKey::Workspace`.
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

    /// Partial-arc redesign: the disc's logical centre must sit
    /// OFF-SCREEN-LEFT so only the right portion of the rim protrudes
    /// into the visible sidebar column. The 2026-05-01 radial-rebuild
    /// tightens the ratio to `VISIBLE_RADIUS_FRACTION = 0.16` so the
    /// formula is `cx = rect.x0 - radius * (1 - VISIBLE_RADIUS_FRACTION)`.
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

    /// Visible arc cap. The user-spec invariant is "at most 120°" — the
    /// 2026-05-01 radial-rebuild tightens this further to ~66° to match
    /// the reference concept's `2000:160` ratio.
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
    /// of the disc isn't grabbable. Mirrors the user-spec invariant
    /// "should be actually to left of the leftmost window group" by
    /// guaranteeing the disc never claims clicks beyond its visible arc.
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

    /// Closes wheel-disc-design known-gap #3 (cap branch): the disc caps
    /// the per-spoke dot cluster at 3 even when more peers are registered,
    /// matching the concept's wheel-card design. Seeds 5 peers on the
    /// same workspace and asserts the helper reports exactly 3.
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

    /// Click-to-spoke (concept's `handleClickItem`) — calling
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

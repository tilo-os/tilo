//! `SidebarWheelDisc` — large radial workspace switcher anchored
//! OFF-SCREEN-LEFT, with only a ~66° arc of its right edge protruding
//! into the visible sidebar column. Each **workspace** is a spoke; the
//! user drags vertically to rotate, and a release snaps to the nearest
//! spoke.
//!
//! ## 2026-05-23 visual redesign
//!
//! The original concept (`radial-workspace-switcher/src/App.tsx`)
//! painted every spoke as a 40×40 (active) or 32×32 (inactive) rounded
//! pill card with a vertically-rotated label. On a 200 px sidebar that
//! read as **crowded** — 4–5 pill cards overlapping inside the visible
//! 66° arc — and the per-spoke rotation made the labels read at
//! inconsistent angles (a regression user-spotted on the live build).
//!
//! The current design is a **quiet ring**:
//! - A subtle solid-fill background panel covers the visible sidebar
//!   footprint at low alpha — gives the wheel visual weight without
//!   competing with the desktop.
//! - One **thin ring stroke** along the protruding rim defines the
//!   wheel disc. No filled tile cards, no halo'd glow band, no per-
//!   spoke icon tiles.
//! - Each workspace is a **dot marker** on the ring's perimeter. The
//!   active marker is a larger filled circle in the workspace's
//!   palette colour with a soft halo; inactive markers are 4 px low-
//!   alpha white specks. Markers rotate WITH the ring on drag.
//! - The active workspace's **label** renders **upright** (no rotation)
//!   to the right of the active marker — horizontal text the user can
//!   read at a glance regardless of any in-flight wheel rotation. As
//!   the wheel spins, the active marker always sits at "3 o'clock"
//!   (the rightmost visible point of the ring), and the label sits at
//!   a fixed screen-space position next to it. The active workspace
//!   *identity* changes when the snap completes, not when the angle
//!   passes — same write-back semantics as before.
//! - Inactive workspace markers omit text entirely; the active marker
//!   is the only one with a label. Hovering an inactive marker is a
//!   future affordance (tooltip).
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
//! Interaction model (concept-faithful):
//!   - Press inside the visible arc → capture `drag_from`.
//!   - Vertical motion: `tempRotation = rotation + dy * DRAG_FACTOR`
//!     (`DRAG_FACTOR = 0.15`).
//!   - Click on a non-active spoke (lands inside its cached cluster
//!     rect) → animate that spoke to active via the snap spring.
//!   - Release → snap to nearest `spoke_gap_deg()` step.
//!   - Wheel scroll: accumulate `deltaY * 0.04`; `update` snaps once
//!     the gap since the last wheel event > `WHEEL_SCROLL_TIMEOUT_MS`.
//!
//! Rotation math uses degrees internally so the snap threshold arithmetic
//! (`rotation_deg / spoke_gap_deg`) stays readable; trig converts to
//! radians at the callsite.
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
use vello::kurbo::{Affine, BezPath, Circle, PathEl, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::animation::Spring;
use ui::icons::Icon;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::SpringPresets;
use ui::widget::Widget;

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
    /// Current visual rotation in degrees. Snapped to a multiple of
    /// `spoke_gap_deg()` on release; free-running during a drag.
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
    /// per-frame `rotation = origin + dy * DRAG_FACTOR` mapping.
    pub drag_rotation_origin: f32,
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
}

/// Inputs that change the disc's painted pixels. The visual rotation
/// is bucketed into 0.01-degree integers so spring-in-flight values
/// invalidate on every micro-step while still being comparable.
#[derive(Default, Clone, PartialEq, Eq)]
struct WheelCacheFp {
    rect_px: (i64, i64, u64, u64),
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
    /// Diameter in integer pixels — paired with `rect_px` to cover
    /// every input to `center()` / `radius`.
    diameter_px: u64,
}

/// Fraction of the disc's radius that protrudes into the viewport.
/// `0.16` matches the concept's `2000-px-disc-on-160-px-column` ratio
/// (`160/2000 = 0.08` of *diameter* = `0.16` of *radius*). Drives both
/// the off-screen-centre offset (`cx = rect.x0 - radius * (1 - VISIBLE)`)
/// and the visible arc width (`2·arccos(1 - VISIBLE) ≈ 66°`).
pub(crate) const VISIBLE_RADIUS_FRACTION: f64 = 0.16;

/// Fractional vertical inset applied at both the top and bottom of the
/// taffy-allocated slot so the wheel disc occupies roughly 10%–90% of
/// the available height. The overlay host allocates the full sidebar
/// slot height to the disc's taffy node; we trim it here so the
/// visible disc footprint reads as deliberately inset from the topbar
/// and floor rather than running edge-to-edge.
pub(crate) const SLOT_VERTICAL_INSET_FRAC: f64 = 0.10;

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
/// workspace. There is no free-running mid-detent rotation — the
/// wheel either sits at a workspace or is animating between two —
/// which reads as "heavy" (slow to start moving) and "attracted"
/// (always lands on a workspace, never drifts mid-state).
///
/// History: the previous continuous-rotation model combined with the
/// quadratic accumulator bug produced the "maddening speed" of
/// 2026-05-25, and a then-attempted fix (factor 0.006 linear) made
/// the wheel feel locked because tiny rotations rounded to the same
/// detent on snap so no visible motion occurred. Discrete detents
/// sidestep both failure modes at once.
///
/// 120 px is calibrated against ~16 px per mouse-wheel click (so
/// ~8 clicks per workspace, deliberate but not slow) and ~40 px
/// per trackpad pixel-delta tick (3 ticks per workspace).
const WHEEL_DETENT_PX: f32 = 120.0;

/// Idle gap after which any sub-detent residual `wheel_accumulator`
/// is cleared. Without this, a partial swipe (say 80 px / 120 px)
/// would carry over into the next scroll burst and cause a workspace
/// switch from a movement the user didn't realise was queued. 250 ms
/// is long enough that a continuous flick isn't broken into separate
/// gestures, short enough that an obvious pause feels deliberate.
const WHEEL_SCROLL_TIMEOUT_MS: u64 = 250;

/// Per-spoke palette. Maps a workspace's *index* in the tree to one of
/// six concept colours (`pink-500`, `blue-500`, `amber-500`,
/// `emerald-500`, `purple-500`, `zinc-400` — see
/// `radial-workspace-switcher/src/App.tsx:10-17`). Wraps modulo for
/// trees with more than six workspaces.
const SPOKE_PALETTE: &[Color] = &[
    Color::from_rgb8(0xec, 0x48, 0x99), // pink-500
    Color::from_rgb8(0x3b, 0x82, 0xf6), // blue-500
    Color::from_rgb8(0xf5, 0x9e, 0x0b), // amber-500
    Color::from_rgb8(0x10, 0xb9, 0x81), // emerald-500
    Color::from_rgb8(0xa8, 0x55, 0xf7), // purple-500
    Color::from_rgb8(0xa1, 0xa1, 0xaa), // zinc-400
];

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
            rect: Rect::ZERO,
            diameter_px: 4500.0,
            chrome: None,
            rotation_spring: None,
            plus_rect: Rect::ZERO,
            last_cursor_pos: None,
            wheel_accumulator: 0.0,
            wheel_last_event: None,
            spoke_hit_rects: Vec::new(),
            cached_scene: Scene::new(),
            cached_fp: WheelCacheFp::default(),
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
    /// workspace under the indigo glow; tests construct the disc and
    /// mutate `active_idx` directly.
    pub fn with_active_idx(mut self, idx: usize) -> Self {
        self.active_idx = idx;
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

    /// Logical centre of the disc in screen coordinates. The centre sits
    /// `radius * (1 - VISIBLE_RADIUS_FRACTION)` pixels LEFT of `rect.x0`,
    /// so the rim crosses the viewport at `±arccos(1 - VISIBLE)` from
    /// due-east (≈ ±33° for `VISIBLE = 0.16`).
    fn center(&self) -> Point {
        let radius = self.diameter_px / 2.0;
        let cx = self.rect.x0 - radius * (1.0 - VISIBLE_RADIUS_FRACTION);
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        Point::new(cx, cy)
    }

    /// Pick the per-spoke colour from `SPOKE_PALETTE` by workspace index.
    fn spoke_color(&self, idx: usize) -> Color {
        SPOKE_PALETTE[idx % SPOKE_PALETTE.len()]
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

    /// Compute the current visual rotation, preferring the spring's
    /// in-flight value over the snapped `rotation_deg` so post-release
    /// easing reads correctly.
    fn visual_rotation_deg(&self) -> f32 {
        match self.rotation_spring.as_ref() {
            Some(s) if !s.is_settled() => s.value(),
            _ => self.rotation_deg,
        }
    }

    /// Snapshot every input that affects the disc's painted pixels.
    /// Used by [`Widget::draw`] to decide whether `self.cached_scene`
    /// can be reused. See [`WheelCacheFp`] for the field semantics.
    fn current_fingerprint(&self) -> WheelCacheFp {
        let rect_px = (
            self.rect.x0.round() as i64,
            self.rect.y0.round() as i64,
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
            rect_px,
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

/// Map a workspace label to its lucide glyph. Mirrors `WORKSPACES` in
/// `radial-workspace-switcher/src/App.tsx` for the concept's six ids,
/// then extends the table with the labels seeded by
/// `WorkspaceTree::bootstrap_default` (`Home` / `Research`) so the live
/// shell's first-boot spokes paint a recognisable glyph instead of
/// falling through to the generic `Layers` default. Inputs are expected
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
        // Default-tree labels seeded by `WorkspaceTree::bootstrap_default`.
        // `Home` paints lucide's house glyph; `Research` borrows the
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
        let y0 = y0_raw + inset_v;
        let y1 = y0_raw + h_raw - inset_v;
        self.rect = Rect::new(
            x0,
            y0,
            x0 + l.size.width as f64,
            y1,
        );
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
        // `rect_px.2` is the rect's width — zero means we haven't
        // drawn yet, so always go through the slow path.
        if fp == self.cached_fp && fp.rect_px.2 > 0 {
            out_scene.append(&self.cached_scene, Some(out_transform));
            return;
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

        // ---- Pass 1: (removed) quiet background panel -------------------
        // A 2-px-equivalent white/0.02 rectangle used to fill the
        // sidebar's visible footprint here to "anchor" the wheel
        // visually. On real displays it read as a faint grey rectangle
        // against the dark wallpaper grid — exactly the "hard panel
        // edge" the comment originally claimed to avoid. The shadow
        // band (Pass 2) and the rim stroke (Pass 3) are enough visual
        // anchor on their own; removing the rectangular fill keeps the
        // wheel reading as quiet curves over the desktop, not a faint
        // box.

        // ---- Pass 2: subtle right-edge atmosphere -----------------------
        // Thin shadow band JUST past the panel's right edge to lift the
        // sidebar off the desktop. Three bands of decaying alpha — much
        // softer than the previous concept-mimicking 8-band cascade so
        // the wheel reads as quiet, not glowing.
        let shadow_x0 = self.rect.x1;
        for i in 0..3 {
            let t = i as f64 / 3.0;
            let alpha = (0.16 * (1.0 - t).powi(2)).clamp(0.0, 0.16);
            let band = Rect::new(
                shadow_x0 + (i as f64) * 2.0,
                self.rect.y0,
                shadow_x0 + ((i + 1) as f64) * 2.0,
                self.rect.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(0, 0, 0, (alpha * 255.0) as u8),
                None,
                &band,
            );
        }

        // ---- Pass 3: the ring stroke (the wheel itself) -----------------
        // ONE thin stroke along the rim — the curve the user can see is
        // the only chrome that defines the disc. The disc's geometric
        // centre sits off-screen-left, so only the right ~66° arc of
        // this circle lands inside the visible panel. Stroke alpha is
        // moderate so the curve reads clearly as an architectural
        // element ("this is the wheel disc") while staying quiet
        // enough to let the markers and label dominate visually.
        let rim = Circle::new((cx, cy), radius);
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            Color::from_rgba8(255, 255, 255, 64), // white/0.25
            None,
            &rim,
        );

        // ---- Pass 4: workspace marker dots on the ring ------------------
        // One dot per workspace, placed on the ring's perimeter at the
        // spoke's current angle. The active workspace is always at
        // angle 0° (the rightmost visible point of the ring) when the
        // wheel is at rest; during a drag, every marker rotates
        // together with the same logic — consistent rotation across
        // all parts.
        //
        // Active marker: 10 px filled circle in workspace palette
        // colour, soft white halo. Paired with an upright horizontal
        // label rendered separately in Pass 5.
        //
        // Inactive markers: 6 px low-alpha white speck. No label —
        // identity is communicated by position relative to the active
        // marker plus the snap animation. Far simpler, far quieter.
        //
        // We iterate virtual cycles so multi-revolution rotations
        // (more than one full wheel cycle from rest) keep every
        // neighbour painted inside the visible band.
        self.spoke_hit_rects.clear();
        self.spoke_hit_rects.resize(self.spokes.len(), Rect::ZERO);

        // Cache the active marker's screen position + colour so the
        // upright-label pass (Pass 5) knows where to anchor.
        let mut active_marker: Option<(Point, Color)> = None;

        if !self.spokes.is_empty() {
            let n = self.spokes.len();
            // Visible angular band: ±70° from due-east. Actual visible
            // arc is ~66° but a few degrees of slop is fine — the cull
            // just avoids painting markers outside the panel.
            let visible_band_deg = 70.0_f32;
            let gap = self.spoke_gap_deg();

            // ONE marker per real workspace — no virtual cycles. The
            // earlier design repeated each workspace's marker for every
            // (visible_band / (gap * N)) revolutions so that during a
            // multi-cycle rotation neighbours kept painting. But because
            // inactive markers are visually identical 4-px specks, the
            // repeats read as "phantom workspaces": three workspaces
            // would draw nine indistinguishable dots inside the visible
            // band. Painting exactly N markers — culled to the visible
            // band — gives users one dot per workspace, no ghosts.
            for i in 0..n {
                let spoke = &self.spokes[i];
                let virt_offset = i as i32 - self.active_idx as i32;
                let angle_deg = self.visual_rotation_deg() + virt_offset as f32 * gap;
                let is_active = i == self.active_idx;
                let a = ((angle_deg % 360.0) + 360.0) % 360.0;
                let in_band = a < visible_band_deg || a > 360.0 - visible_band_deg;
                if !in_band {
                    continue;
                }

                let angle_rad = (angle_deg as f64).to_radians();
                let marker_x = cx + radius * angle_rad.cos();
                let marker_y = cy + radius * angle_rad.sin();
                let marker_p = Point::new(marker_x, marker_y);

                if is_active {
                    let base = self.spoke_color(i);
                    // Soft halo — three concentric translucent circles
                    // in the workspace colour fading outward. Sized so
                    // the active marker reads as the visual anchor of
                    // the entire sidebar.
                    for &(r_halo, alpha) in &[(28.0_f64, 0.07_f64), (20.0, 0.12), (13.0, 0.20)] {
                        let halo = Circle::new(marker_p, r_halo);
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            Color::from_rgba8(base.r(), base.g(), base.b(), (alpha * 255.0) as u8),
                            None,
                            &halo,
                        );
                    }
                    // Outer crisp ring (white/0.32) — separates the
                    // coloured core from the halo for readability on
                    // bright backdrops.
                    let outer = Circle::new(marker_p, 10.0);
                    scene.stroke(
                        &Stroke::new(1.2),
                        transform,
                        Color::from_rgba8(255, 255, 255, 85),
                        None,
                        &outer,
                    );
                    // Coloured core.
                    let core = Circle::new(marker_p, 8.0);
                    scene.fill(Fill::NonZero, transform, base, None, &core);

                    active_marker = Some((marker_p, base));
                } else {
                    // Inactive: small low-alpha white speck. Visually
                    // present but unobtrusive.
                    let speck = Circle::new(marker_p, 4.0);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        Color::from_rgba8(255, 255, 255, 80), // white/0.31
                        None,
                        &speck,
                    );
                }

                // Generous-but-bounded click hit rect on each marker.
                // 28 px square centred on the marker — easy click target.
                // Adjacent markers are well separated (15° at radius
                // 2250 ≈ 590 px arc length) so there's no overlap.
                let hit_half = 14.0_f64;
                self.spoke_hit_rects[i] = Rect::new(
                    marker_x - hit_half,
                    marker_y - hit_half,
                    marker_x + hit_half,
                    marker_y + hit_half,
                );

                // Per-spoke presence dots: small coloured dots sitting
                // INSIDE the rim (toward the disc centre) at the
                // marker's angle. Keeps them attached visually without
                // being eclipsed by the marker itself.
                if let Some(ws_id) = spoke.workspace_id {
                    let key = PresenceKey::Workspace { id: format!("{}", ws_id) };
                    let peers = presence.peers_on(&key);
                    if !peers.is_empty() {
                        let peer_count = peers.len().min(3);
                        let radial_offset = if is_active { -16.0_f64 } else { -12.0_f64 };
                        let dot_base_x = cx + (radius + radial_offset) * angle_rad.cos();
                        let dot_base_y = cy + (radius + radial_offset) * angle_rad.sin();
                        let perp_x = -angle_rad.sin();
                        let perp_y = angle_rad.cos();
                        let spacing = 7.0_f64;
                        let centre_bias = (peer_count as f64 - 1.0) / 2.0;
                        for (kk, user) in peers.iter().take(peer_count).enumerate() {
                            let off = (kk as f64 - centre_bias) * spacing;
                            let (rr, gg, bb) = presence.color_for(user);
                            let dot = Circle::new(
                                (dot_base_x + perp_x * off, dot_base_y + perp_y * off),
                                3.0,
                            );
                            scene.fill(
                                Fill::NonZero,
                                transform,
                                Color::from_rgb8(rr, gg, bb),
                                None,
                                &dot,
                            );
                        }
                    }
                }
            }
        }

        // ---- Pass 5: active workspace label (upright, horizontal) -------
        // The active workspace's name renders as upright horizontal
        // text next to the active marker. NO rotation — the label
        // reads the same regardless of the wheel's visual angle. This
        // fixes the "text orientation is weird when rotating" feedback
        // from the previous design where every spoke's cluster
        // rotated with the wheel.
        //
        // Position: label sits to the LEFT of the marker (inside the
        // panel, toward the disc centre), centred vertically on the
        // marker's y. A 3-px workspace-colour accent bar sits between
        // the label and the marker to visually bind them.
        if let Some((marker_p, base)) = active_marker {
            let label_idx = self.active_idx;
            let label_raw = self
                .spokes
                .get(label_idx)
                .map(|s| s.text.as_str())
                .unwrap_or("");
            let label_upper = label_raw.to_uppercase();
            let label_size = 22.0_f32;
            let letter_spacing_em = 0.14_f32;
            let (raw_w, text_h) = text_renderer.measure(&label_upper, label_size, None);
            let track = label_upper.chars().count() as f32 * label_size * letter_spacing_em;
            let text_w = (raw_w + track) as f64;

            // Accent bar — 4 px wide colour swatch, 6 px taller than
            // the label. Anchored 18 px to the LEFT of the marker so
            // marker + accent read as a unit without colliding.
            let accent_right = marker_p.x - 18.0;
            let accent_rect = Rect::new(
                accent_right - 4.0,
                marker_p.y - (text_h as f64) / 2.0 - 4.0,
                accent_right,
                marker_p.y + (text_h as f64) / 2.0 + 4.0,
            );
            let accent_rr = RoundedRect::from_rect(accent_rect, 2.0);
            scene.fill(Fill::NonZero, transform, base, None, &accent_rr);

            // Label text — solid white, full alpha. Positioned to the
            // LEFT of the accent bar with a small gap.
            let label_gap = 10.0_f64;
            let label_right_x = accent_right - 4.0 - label_gap;
            let label_x0 = label_right_x - text_w;
            let label_top_y = marker_p.y - (text_h as f64) / 2.0;
            let label_xform = transform * Affine::translate((label_x0, label_top_y));
            text_renderer.draw_text(
                scene,
                &label_upper,
                label_size,
                Color::from_rgba8(255, 255, 255, 255),
                label_xform,
                None,
            );

            // Expand the active spoke's hit rect to cover the label +
            // accent, so a click anywhere on the readable label is
            // captured by the active spoke (and becomes a no-op,
            // rather than landing on the underlying panel/desktop).
            if label_idx < self.spoke_hit_rects.len() {
                let prev = self.spoke_hit_rects[label_idx];
                self.spoke_hit_rects[label_idx] = Rect::new(
                    prev.x0.min(label_x0 - 4.0),
                    prev.y0.min(label_top_y - 4.0),
                    prev.x1.max(marker_p.x + 16.0),
                    prev.y1.max(label_top_y + text_h as f64 + 4.0),
                );
            }
        }

        // ---- Pass 7: "+" add-workspace slot -----------------------------
        // Anchored at the visible-arc's bottom-most tip, centred on the
        // visible footprint horizontally. Cached into `plus_rect` for
        // press hit-testing. 28 px diameter, surfaces.overlay fill,
        // neutral[3] rim, text_primary "+" glyph at 40% of radius.
        let slot_diam = 28.0_f64;
        let slot_radius = slot_diam / 2.0;
        let arc_half = (1.0 - VISIBLE_RADIUS_FRACTION).acos();
        let arc_bottom_y = cy + radius * arc_half.sin();
        let slot_cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let slot_cy = (arc_bottom_y + slot_radius + 8.0).min(self.rect.y1 - slot_radius - 4.0);
        self.plus_rect = Rect::new(
            slot_cx - slot_radius,
            slot_cy - slot_radius,
            slot_cx + slot_radius,
            slot_cy + slot_radius,
        );
        let slot_circle = Circle::new((slot_cx, slot_cy), slot_radius);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &slot_circle,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.neutral[3],
            None,
            &slot_circle,
        );
        let arm = slot_radius * 0.4;
        let plus = BezPath::from_iter([
            PathEl::MoveTo(Point::new(slot_cx - arm, slot_cy)),
            PathEl::LineTo(Point::new(slot_cx + arm, slot_cy)),
            PathEl::MoveTo(Point::new(slot_cx, slot_cy - arm)),
            PathEl::LineTo(Point::new(slot_cx, slot_cy + arm)),
        ]);
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            colors.text_primary,
            None,
            &plus,
        );

        // End of cache build — splice the freshly-encoded local scene
        // into the caller's `out_scene` with the host transform, then
        // restore `self.cached_scene` and record the fingerprint so
        // the next idle frame takes the fast path.
        out_scene.append(&local, Some(out_transform));
        self.cached_scene = local;
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
                    self.rotation_spring = None;
                    self.drag_from = Some(anchor);
                    self.drag_rotation_origin = self.rotation_deg;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor_pos = Some(p);
                if let Some(origin) = self.drag_from {
                    let dy = p.y - origin.y;
                    let delta_deg = (dy as f32) * self.deg_per_pixel();
                    self.rotation_deg = self.drag_rotation_origin + delta_deg;
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
                    let pre_snap_deg = self.rotation_deg;
                    self.snap_to_nearest();
                    let snapped_deg = self.rotation_deg;
                    if (pre_snap_deg - snapped_deg).abs() > f32::EPSILON {
                        let params = SpringPresets::default().snappy;
                        let mut s = Spring::new(pre_snap_deg, params);
                        s.set_target(snapped_deg);
                        self.rotation_spring = Some(s);
                    } else {
                        self.rotation_spring = None;
                    }
                    self.drag_from = None;
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
                // an actual spring settle, not a teleport). There is
                // NO mid-detent rotation — the wheel either rests on
                // a workspace or is animating to the next one.
                self.wheel_accumulator += dy;
                self.wheel_last_event = Some(Instant::now());
                let n = self.spokes.len();
                if n > 1 {
                    while self.wheel_accumulator.abs() >= WHEEL_DETENT_PX {
                        let dir: i32 = if self.wheel_accumulator < 0.0 { 1 } else { -1 };
                        let target =
                            ((self.active_idx as i32 + dir).rem_euclid(n as i32)) as usize;
                        self.click_to_spoke(target);
                        self.wheel_accumulator -= dir as f32 * WHEEL_DETENT_PX;
                    }
                }
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
        // easing OR while a wheel-scroll snap-back is pending.
        let spring_active = self
            .rotation_spring
            .as_ref()
            .map(|s| !s.is_settled())
            .unwrap_or(false);
        spring_active || self.wheel_last_event.is_some()
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        let mut keep = false;
        // Step the snap-spring at fixed 60 Hz.
        if let Some(spring) = self.rotation_spring.as_mut() {
            if spring.is_settled() {
                self.rotation_spring = None;
            } else {
                spring.step(1.0 / 60.0);
                keep = true;
            }
        }
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
        // `WheelCacheFp::default().rect_px.2` is zero, so a fresh widget
        // (no successful draw yet) reports dirty until the first paint.
        self.cached_fp.rect_px.2 == 0
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
        let n = self.spokes.len();
        if n <= 1 {
            // Single-workspace disc: snap rotation back to 0 so the lone
            // label doesn't drift; active_idx is always 0.
            self.rotation_deg = 0.0;
            self.active_idx = 0;
            // Even with a single workspace, keep the chrome's active id
            // in sync so the "first frame after boot" invariant holds —
            // no-op unless the id differs.
            self.publish_active_to_chrome();
            return;
        }
        let gap = self.spoke_gap_deg();
        let nearest_offset = (self.rotation_deg / gap).round();
        self.rotation_deg = nearest_offset * gap;
        let delta = (-nearest_offset).rem_euclid(n as f32);
        let new_idx = ((self.active_idx as f32 + delta).rem_euclid(n as f32)) as usize;
        self.active_idx = new_idx % n;
        self.publish_active_to_chrome();
    }

    /// Concept's `handleClickItem` (App.tsx:70-78): rotate the wheel
    /// so `target_idx` becomes the active spoke, taking the shortest
    /// signed angular path. Animation is via the snap spring.
    pub fn click_to_spoke(&mut self, target_idx: usize) {
        if target_idx >= self.spokes.len() || target_idx == self.active_idx {
            return;
        }
        let n = self.spokes.len() as f32;
        let gap = self.spoke_gap_deg();
        // Shortest signed delta in spoke-index space, wrapped to
        // `[-N/2, N/2)`.
        let mut diff = (target_idx as f32 - self.active_idx as f32) % n;
        if diff > n / 2.0 {
            diff -= n;
        } else if diff < -n / 2.0 {
            diff += n;
        }
        // A `+1` index shift equals a `-gap` wheel rotation (matches the
        // sign convention in `snap_to_nearest`: dragging down rotates
        // forward in degrees but advances active_idx forward in the list,
        // so advancing active_idx forward via click means the wheel
        // rotates BACKWARD).
        let pre_snap_deg = self.rotation_deg;
        let target_rot = pre_snap_deg + (-diff * gap);
        let snapped = (target_rot / gap).round() * gap;
        self.rotation_deg = snapped;
        self.active_idx = target_idx;
        self.publish_active_to_chrome();

        let params = SpringPresets::default().snappy;
        let mut s = Spring::new(pre_snap_deg, params);
        s.set_target(snapped);
        self.rotation_spring = Some(s);
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
            let Some(ws_id) = spoke.workspace_id else { continue };
            let key = PresenceKey::Workspace { id: format!("{}", ws_id) };
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
    fn publish_active_to_chrome(&self) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let Some(spoke) = self.spokes.get(self.active_idx) else {
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
            .map(|n| SpokeLabel { text: (*n).to_string(), workspace_id: None })
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

    #[test]
    fn new_starts_with_active_idx_zero_and_rotation_zero() {
        let d = mk(&["Home", "Research", "Design"]);
        assert_eq!(d.active_idx, 0);
        assert_eq!(d.rotation_deg, 0.0);
        assert!(d.drag_from.is_none());
        assert!(!d.wants_pointer_capture());
    }

    /// Default workspace labels (`Home` / `Research` / `Design`) seeded
    /// by `WorkspaceTree::bootstrap_default` must each map to a
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
        let drag_dy = (gap / DRAG_FACTOR) as f64 + 30.0;
        pump(&mut d, cursor_event(cx, start_y + drag_dy));
        assert!(d.rotation_deg > 0.0);

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
        assert_ne!(
            d.active_idx, before_idx,
            "dragging past one gap must rotate to a new spoke"
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

        // Seed a 3-workspace tree. `bootstrap_default` already seeds
        // Home / Research / Design — three workspaces in a known order
        // matching the spoke layout — so no additional appends are needed.
        let tree = WorkspaceTree::bootstrap_default();
        assert_eq!(tree.workspaces.len(), 3);
        let ws_ids: Vec<_> = tree.workspaces.iter().map(|w| w.id).collect();
        let initial_active = tree.active;
        assert_eq!(
            initial_active, ws_ids[0],
            "bootstrap_default should leave active = ws_ids[0]"
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

        // Synthesise press+move+release sequence. With DRAG_FACTOR = 0.15
        // and a 120° gap, a `gap/0.15 = 800 px` drag is one gap;
        // `snap_to_nearest` shifts active_idx by `-1 mod 3 = 2`.
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = drag_anchor_x(&d);
        let drag_dy = (d.spoke_gap_deg() / DRAG_FACTOR) as f64 + 100.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y + drag_dy));
        assert!(pump(&mut d, release_event()));

        assert_ne!(
            d.active_idx, 0,
            "a >gap drag must advance past the initial spoke"
        );
        // And chrome.tree.active must now point at the workspace id
        // that matches the new active_idx — this is the CRDT write-back
        // contract the disc now fulfils via `publish_active_to_chrome`.
        let expected_id = ws_ids[d.active_idx];
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
        pump(&mut d, cursor_event(cx, start_y + drag_dy));
        assert!(pump(&mut d, release_event()));
        assert_ne!(d.active_idx, idx_before);
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
        pump(&mut d, cursor_event(cx, start_y + 73.0));
        let pre_snap_deg = d.rotation_deg;
        assert!(pre_snap_deg > 0.0);
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
        // The spring starts at the pre-snap rotation, not the snapped
        // target — otherwise nothing would animate visually.
        assert!(
            (spring.value() - pre_snap_deg).abs() < 1e-3,
            "spring should start at pre-snap rotation {} (got {})",
            pre_snap_deg,
            spring.value()
        );
        // `needs_animation_frame` must light up so `App` keeps pumping
        // redraws while the ease plays out.
        assert!(d.needs_animation_frame());

        // Pump the per-frame update hook at 60 Hz. The "snappy" preset
        // (stiffness 260, damping 28, mass 1) has ω ≈ 16.1 rad/s and
        // damping ratio ≈ 0.87 — underdamped, so it takes a few cycles
        // to land inside `is_settled`'s 0.1° / 0.1 °/s window for a
        // 90° displacement. 180 frames (~3 s) is well over the budget
        // with headroom for the integrator's step bias.
        use automerge::AutoCommit;
        let mut doc = AutoCommit::new();
        let path = automerge::ROOT.clone();
        for _ in 0..180 {
            d.update(&mut doc, &path);
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

        // Minimal tree — `bootstrap_default` gives one workspace, which
        // is enough to assert the plus-press grows the count.
        let tree = WorkspaceTree::bootstrap_default();
        let initial_count = tree.workspaces.len();
        let initial_active = tree.active;
        let chrome = ChromeState::new(tree);

        // Manually populate the disc's geometry so we can hit-test without
        // running a full draw pass (there's no wgpu in unit tests — the
        // snapshot harness gates on an offscreen renderer). The new
        // `draw` implementation sets `plus_rect` to a 28 px disc centred
        // on the visible footprint's horizontal midpoint and 8 px below
        // the bottom-most visible-arc tip; we synthesise the same rect
        // here to decouple from the exact draw-time arithmetic.
        let mut d = SidebarWheelDisc::new(vec![SpokeLabel {
            text: "Home".into(),
            workspace_id: None,
        }])
        .with_chrome(chrome.clone());
        let visible_w = d.visible_width_px();
        d.rect = Rect::new(0.0, 0.0, visible_w, d.diameter_px);
        let radius = d.diameter_px / 2.0;
        let cy = d.diameter_px / 2.0;
        let arc_half = (1.0 - VISIBLE_RADIUS_FRACTION).acos();
        let slot_radius = 14.0_f64;
        let slot_cx = (d.rect.x0 + d.rect.x1) / 2.0;
        let slot_cy = (cy + radius * arc_half.sin() + slot_radius + 8.0)
            .min(d.rect.y1 - slot_radius - 4.0);
        d.plus_rect = Rect::new(
            slot_cx - slot_radius,
            slot_cy - slot_radius,
            slot_cx + slot_radius,
            slot_cy + slot_radius,
        );

        // Move the cursor to the slot's centre so `last_cursor_pos` picks
        // it up — the press handler consults that cache (not the event,
        // which has no position field) to hit-test the plus slot.
        pump(&mut d, cursor_event(slot_cx, slot_cy));
        assert!(pump(&mut d, press_event()), "press on plus slot must be handled");

        let state = chrome.borrow();
        assert_eq!(
            state.tree().workspaces.len(),
            initial_count + 1,
            "plus-press must grow the workspace count by exactly 1"
        );
        assert_ne!(
            state.tree().active, initial_active,
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
        let key = PresenceKey::Workspace { id: "77".to_string() };
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
        let key = PresenceKey::Workspace { id: "77".to_string() };
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
        assert_eq!(d.active_idx, 2);
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
    }
}

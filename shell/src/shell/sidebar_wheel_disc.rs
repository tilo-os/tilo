//! `SidebarWheelDisc` — large radial workspace switcher anchored
//! OFF-SCREEN-LEFT, with only a ~66° arc of its right edge protruding
//! into the visible sidebar column. Each **workspace** is a spoke; the
//! user drags vertically to rotate, and a release snaps to the nearest
//! spoke. An additional fixed "ruler" of small dots near the rim
//! indicates the active workspace at a glance.
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

use quick_ui::animation::Spring;
use quick_ui::icons::{self, Icon, LUCIDE_DEFAULT_STROKE_PX};
use quick_ui::presence::{PresenceCtx, PresenceKey};
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::tokens::SpringPresets;
use quick_ui::widget::Widget;

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
    /// drag→angle mapping. Default 1200 px — paired with
    /// `VISIBLE_RADIUS_FRACTION = 0.16` this gives a ~96 px-wide visible
    /// arc, matching the live `SIDEBAR_WIDTH_PX = 96` slot.
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
}

/// Fraction of the disc's radius that protrudes into the viewport.
/// `0.16` matches the concept's `2000-px-disc-on-160-px-column` ratio
/// (`160/2000 = 0.08` of *diameter* = `0.16` of *radius*). Drives both
/// the off-screen-centre offset (`cx = rect.x0 - radius * (1 - VISIBLE)`)
/// and the visible arc width (`2·arccos(1 - VISIBLE) ≈ 66°`).
pub(crate) const VISIBLE_RADIUS_FRACTION: f64 = 0.16;

/// Concept's `DRAG_FACTOR` from `App.tsx:41` — degrees of rotation per
/// pixel of vertical drag.
const DRAG_FACTOR: f32 = 0.15;

/// Angular spacing between adjacent spokes. Concept's `ANGLE_STEP =
/// 360°/24 = 15°` (App.tsx:20, where `DISPLAY_WORKSPACES.length = 24`
/// because the 6 workspaces are repeated 4×). We use the same step so
/// the visual cadence matches; for `N < 24` the wheel cycles `N` per
/// `15° · N` of rotation.
const SPOKE_ANGLE_STEP_DEG: f32 = 15.0;

/// Concept's wheel-scroll multiplier (`App.tsx:56`).
const WHEEL_SCROLL_FACTOR: f32 = 0.04;

/// Concept's wheel-scroll snap timeout (`App.tsx:67`).
const WHEEL_SCROLL_TIMEOUT_MS: u64 = 80;

/// Per-spoke palette. Maps a workspace's *index* in the tree to one of
/// six concept colours (`pink-500`, `blue-500`, `amber-500`,
/// `emerald-500`, `purple-500`, `zinc-400` — see
/// `radial-workspace-switcher/src/App.tsx:10-17`). Wraps modulo for
/// trees with more than six workspaces.
const SPOKE_PALETTE: &[Color] = &[
    Color::rgb8(0xec, 0x48, 0x99), // pink-500
    Color::rgb8(0x3b, 0x82, 0xf6), // blue-500
    Color::rgb8(0xf5, 0x9e, 0x0b), // amber-500
    Color::rgb8(0x10, 0xb9, 0x81), // emerald-500
    Color::rgb8(0xa8, 0x55, 0xf7), // purple-500
    Color::rgb8(0xa1, 0xa1, 0xaa), // zinc-400
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
            diameter_px: 1200.0,
            chrome: None,
            rotation_spring: None,
            plus_rect: Rect::ZERO,
            last_cursor_pos: None,
            wheel_accumulator: 0.0,
            wheel_last_event: None,
            spoke_hit_rects: Vec::new(),
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

/// Project the cluster's left-tail screen x for a label of width `text_w`.
///
/// The cluster is anchored at `(anchor_x, anchor_y)` and rotated by
/// `cluster_rot` radians. In the cluster's local frame the label's tail
/// (its leftmost edge before rotation) sits at local x =
/// `-tile_size - gap - text_w` (because the tile centres at
/// `-tile_size/2` and the label centres at `-tile_size - gap - text_w/2`,
/// so its leftmost point is one further `text_w/2` out). After the
/// rotation that local point becomes a screen-space (x, y); we only
/// need x for the column-edge fit test.
fn cluster_label_tail_screen_x(
    text_w: f64,
    cluster_rot: f64,
    anchor_x: f64,
    tile_size: f64,
    gap: f64,
) -> f64 {
    let local_x = -tile_size - gap - text_w;
    let local_y = 0.0_f64;
    // Affine::rotate followed by translate(anchor): screen_x =
    // anchor_x + cos(θ)·local_x − sin(θ)·local_y.
    anchor_x + cluster_rot.cos() * local_x - cluster_rot.sin() * local_y
}

/// Build the label string for an inactive spoke, truncating to 4-6
/// chars if the full label would overrun the wheel column's left edge
/// (`column_x0`). Active spokes return the full uppercase label.
///
/// Truncation strategy: try the full label first; if the rotated tail
/// overshoots `column_x0` by more than 1 px, drop the tail char until
/// it fits or 4 chars remain. This matches the spec ("first 4-6 chars")
/// without abruptly cutting at 4 when 6 would still fit.
#[allow(clippy::too_many_arguments)]
fn build_label_text(
    raw: &str,
    is_active: bool,
    label_size: f32,
    letter_spacing_em: f32,
    text_renderer: &mut TextRenderer,
    cluster_rot: f64,
    anchor_x: f64,
    tile_size: f64,
    gap: f64,
    column_x0: f64,
) -> String {
    let upper = raw.to_uppercase();
    if is_active {
        return upper;
    }
    let chars: Vec<char> = upper.chars().collect();
    // Try lengths from full → 4 (clamped at the actual length).
    let mut best = upper.clone();
    let max_len = chars.len();
    let min_len = max_len.min(4);
    for take in (min_len..=max_len).rev() {
        let candidate: String = chars.iter().take(take).collect();
        let (raw_w, _) = text_renderer.measure(&candidate, label_size, None);
        let track =
            candidate.chars().count() as f32 * label_size * letter_spacing_em;
        let text_w = (raw_w + track) as f64;
        let tail_x = cluster_label_tail_screen_x(
            text_w,
            cluster_rot,
            anchor_x,
            tile_size,
            gap,
        );
        if tail_x >= column_x0 - 1.0 {
            return candidate;
        }
        best = candidate;
    }
    best
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
        let l = taffy.layout(node).unwrap();
        // `rect` is the visible footprint inside the sidebar column.
        // The disc's logical centre lives off-screen-left of `rect.x0`
        // and only the right ~66° arc paints inside this rect. The
        // hit-testing in `handle_event` uses `hit_visible_arc` so clicks
        // in the rect's corners (outside the arc) don't grab the wheel.
        let x0 = origin.x + l.location.x as f64;
        let y0 = origin.y + l.location.y as f64;
        self.rect = Rect::new(
            x0,
            y0,
            x0 + l.size.width as f64,
            y0 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        let radius = self.diameter_px / 2.0;
        if radius <= 1.0 || self.rect.area() <= 0.0 {
            return;
        }
        let c = self.center();
        let cx = c.x;
        let cy = c.y;

        // ---- Pass 1: shadow / atmosphere --------------------------------
        // Concept: `boxShadow: 40px 0 120px rgba(0,0,0,0.8)` — soft black
        // shadow extending RIGHT of the rim into the desktop. We
        // approximate with a few thin vertical bands of decaying alpha
        // anchored just outside the visible-arc rim. Kept narrow (≤ ~16
        // px) so the shadow reads as "wheel weight" without obscuring
        // the workspace canvas behind it.
        let shadow_x0 = self.rect.x1; // shadow starts just past the visible arc
        let shadow_band_count = 8;
        for i in 0..shadow_band_count {
            let t = i as f64 / shadow_band_count as f64;
            let alpha = (0.32 * (1.0 - t).powi(2)).clamp(0.0, 0.32);
            let band_w = 2.0_f64;
            let band = Rect::new(
                shadow_x0 + (i as f64) * band_w,
                self.rect.y0,
                shadow_x0 + ((i + 1) as f64) * band_w,
                self.rect.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                Color::rgba8(0, 0, 0, (alpha * 255.0) as u8),
                None,
                &band,
            );
        }

        // ---- Pass 2: faint disc fill ------------------------------------
        // Concept: `bg-white/[0.01] backdrop-blur-xl`. Without a real
        // backdrop blur, substitute a flat very-low-alpha fill on the
        // full circle. The off-screen-left part of the disc paints
        // outside the visible canvas — only the visible-arc portion
        // contributes pixels.
        let disc_fill = Circle::new((cx, cy), radius);
        scene.fill(
            Fill::NonZero,
            transform,
            Color::rgba8(255, 255, 255, 3), // ≈ white/0.012
            None,
            &disc_fill,
        );

        // ---- Pass 3: dark rim band --------------------------------------
        // Concept: `border-r-[40px] border-white/[0.02]`. Stroked 40-px
        // band centred at radius `r - 20`. The concept's 0.02 alpha is
        // barely visible against a near-black backdrop; we double it to
        // 0.04 so the band reads as a subtle darker arc without
        // overpowering the rim.
        let band = Circle::new((cx, cy), radius - 20.0);
        scene.stroke(
            &Stroke::new(40.0),
            transform,
            Color::rgba8(255, 255, 255, 10), // ≈ white/0.04
            None,
            &band,
        );

        // ---- Pass 4: rim hairline ---------------------------------------
        // 1.5-px hairline at the very edge — defines the disc's silhouette
        // so the rim band doesn't fade into the backdrop. Tracks the
        // reference screenshot, which shows a clear curved edge through
        // the column.
        let rim = Circle::new((cx, cy), radius);
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            Color::rgba8(255, 255, 255, 36), // ≈ white/0.14
            None,
            &rim,
        );

        // ---- Pass 5: rotating spokes (workspace clusters) ---------------
        // Each spoke's cluster (icon-tile + label) is anchored at the
        // rim, pulled back inward by `pull_back` (concept's `pr-9`),
        // then rotated `+π/2` relative to the spoke direction so the
        // cluster reads vertically (concept's `-rotate-90` in y-down
        // screen coords). Active style: 40 px coloured tile, 18-px
        // white label, gap 16 px. Inactive: 32 px white/10 tile, 15-px
        // white/80 label, gap 12 px, cluster opacity 0.30.
        //
        // To match the concept's `WORKSPACES.length × 4 = 24` display-
        // spoke cycle, we render `CYCLES_PER_SIDE` virtual cycles of
        // the workspace list on each side of the active spoke. For
        // small `N` (3-6) this means the user always sees neighbouring
        // workspaces at ±15° from the active, just like the reference.
        self.spoke_hit_rects.clear();
        self.spoke_hit_rects.resize(self.spokes.len(), Rect::ZERO);
        if !self.spokes.is_empty() {
            let n = self.spokes.len();
            // How many cycles to render on each side of the active.
            // Visible band is ±70°; one cycle is `N * 15°`. For N=3
            // that's 45° per cycle → 1 cycle on each side covers
            // ±45°, plus the active = ±60° spread, well within band.
            // For N>=6, one cycle covers ±90°, so even one side cycle
            // is plenty (the visible_band cull discards the far entries).
            let cycles_per_side: i32 = ((70.0_f32 / (SPOKE_ANGLE_STEP_DEG * n as f32)).ceil() as i32)
                .max(1);
            // Iterate virtual indices in `[-cycles*N, +cycles*N + N)` so
            // each real spoke is rendered in every visible cycle. The
            // mapping `virt_idx → real_idx = virt_idx.rem_euclid(N)`.
            let total_cycles = (cycles_per_side * 2 + 1) as usize;
            let total_virtual = total_cycles * n;
            let virtual_start = -(cycles_per_side * n as i32);
            for k in 0..total_virtual {
                let virt_offset = virtual_start + k as i32;
                // Map virtual index → real workspace. At rest
                // (virt_offset=0, rotation=0), the active workspace
                // (`active_idx`) is the one at angle 0°. Going UP one
                // virtual step (virt_offset=+1) means the wheel has
                // rotated by +gap, putting active_idx+1 at angle 0°.
                // So: `i = (virt_offset + active_idx).rem_euclid(N)`.
                let i = ((virt_offset + self.active_idx as i32)
                    .rem_euclid(n as i32)) as usize;
                let spoke = &self.spokes[i];
                let gap = self.spoke_gap_deg();
                let angle_deg = self.visual_rotation_deg() + virt_offset as f32 * gap;
                let is_active = virt_offset == 0;
                let a = ((angle_deg % 360.0) + 360.0) % 360.0;
                let visible_band = a < 70.0 || a > 290.0;
                if !visible_band {
                    continue;
                }

                let angle_rad = (angle_deg as f64).to_radians();
                let rim_x = cx + radius * angle_rad.cos();
                let rim_y = cy + radius * angle_rad.sin();
                // Pull cluster anchor inward by 36 px (concept's `pr-9`)
                // along the spoke direction.
                let pull_back = 36.0_f64;
                let anchor_x = rim_x - pull_back * angle_rad.cos();
                let anchor_y = rim_y - pull_back * angle_rad.sin();

                let tile_size = if is_active { 40.0_f64 } else { 32.0_f64 };
                let tile_radius = if is_active { 12.0_f64 } else { 8.0_f64 };
                let label_size = if is_active { 18.0_f32 } else { 15.0_f32 };
                let gap = if is_active { 16.0_f64 } else { 12.0_f64 };
                let cluster_alpha = if is_active { 1.0_f32 } else { 0.30_f32 };
                let letter_spacing_em = if is_active { 0.15_f32 } else { 0.10_f32 };

                // Readability flip on the left half (rare with the
                // cull above, but safe under unbounded `rotation_deg`).
                let upside_down = a > 90.0 && a < 270.0;
                let extra_pi = if upside_down { std::f64::consts::PI } else { 0.0 };

                // Label text. Inactive spokes get clipped to fit the
                // wheel column: rotated -90°, the label's "left edge"
                // (which after the cluster rotation is whichever screen-
                // space x-coordinate maps to the local `-text_w/2`
                // tail) can extend past `self.rect.x0`. We measure the
                // would-be screen-space bounding box and truncate to a
                // 4-6 char head if it overruns. The active spoke is
                // exempt — its label is the user's primary readout, and
                // pulling it inside the column would mask the workspace
                // identity. Truncation order is character-by-character
                // until the rotated bbox fits with a 1-px safety margin.
                let label_text: String = build_label_text(
                    &spoke.text,
                    is_active,
                    label_size,
                    letter_spacing_em,
                    text_renderer,
                    angle_rad + extra_pi + std::f64::consts::FRAC_PI_2,
                    anchor_x,
                    tile_size,
                    gap,
                    self.rect.x0,
                );
                let (text_w_raw, text_h) = text_renderer.measure(&label_text, label_size, None);
                let extra_track =
                    label_text.chars().count() as f32 * label_size * letter_spacing_em;
                let text_w = (text_w_raw + extra_track) as f64;

                // Cluster local frame: tile centred at local x = -tile_size/2
                // (so the rim-pulled-back anchor sits at the cluster's
                // RIGHT edge), label past it at x = -tile - gap - text_w/2.
                // After the `-π/2` rotation the tile lands toward the
                // disc centre and the label extends along the rim
                // tangent — concept's vertical reading direction.
                let cluster_xform = transform
                    * Affine::translate((anchor_x, anchor_y))
                    * Affine::rotate(angle_rad + extra_pi + std::f64::consts::FRAC_PI_2);

                let tile_cx_local = -tile_size / 2.0;
                let label_cx_local = -tile_size - gap - text_w / 2.0;

                // -- Tile -----------------------------------------------
                let tile_rect_local = Rect::new(
                    tile_cx_local - tile_size / 2.0,
                    -tile_size / 2.0,
                    tile_cx_local + tile_size / 2.0,
                    tile_size / 2.0,
                );
                let tile_rr = RoundedRect::from_rect(tile_rect_local, tile_radius);

                let tile_fill = if is_active {
                    let base = self.spoke_color(i);
                    Color::rgba8(base.r, base.g, base.b, (255.0 * cluster_alpha) as u8)
                } else {
                    Color::rgba8(255, 255, 255, (255.0 * 0.10 * cluster_alpha) as u8)
                };
                scene.fill(Fill::NonZero, cluster_xform, tile_fill, None, &tile_rr);
                if is_active {
                    // Soft white outer glow
                    // (concept: `drop-shadow-[0_0_15px_rgba(255,255,255,0.2)]`).
                    // Outer wide halo first (radius 14 @ alpha 0.05) to
                    // approximate the concept's 15-px-blur drop-shadow,
                    // then nested inner halos for crisp falloff.
                    for &(r, a) in &[
                        (14.0_f64, 0.05_f32),
                        (10.0_f64, 0.07_f32),
                        (6.0_f64, 0.10_f32),
                        (3.0_f64, 0.14_f32),
                    ] {
                        scene.stroke(
                            &Stroke::new(r * 2.0),
                            cluster_xform,
                            Color::rgba8(255, 255, 255, (a * 255.0) as u8),
                            None,
                            &tile_rr,
                        );
                    }
                    // Crisp white/0.20 border on top.
                    scene.stroke(
                        &Stroke::new(1.0),
                        cluster_xform,
                        Color::rgba8(255, 255, 255, 51),
                        None,
                        &tile_rr,
                    );
                }

                // -- Glyph (per-workspace lucide icon) ------------------
                // Active = solid white, inactive = white/40. Icon is
                // resolved by the spoke's lower-cased label per the
                // concept's `WORKSPACES` table; unknown labels fall back
                // to `Layers`. Tile inset matches the concept's
                // `w-5 h-5` glyph in a 40-px tile (~50 % of edge).
                let glyph_color = if is_active {
                    Color::rgba8(255, 255, 255, (255.0 * cluster_alpha) as u8)
                } else {
                    Color::rgba8(255, 255, 255, (255.0 * 0.40 * cluster_alpha) as u8)
                };
                let glyph_inset = if is_active { 10.0_f64 } else { 9.0_f64 };
                let glyph_rect = Rect::new(
                    tile_cx_local - tile_size / 2.0 + glyph_inset,
                    -tile_size / 2.0 + glyph_inset,
                    tile_cx_local + tile_size / 2.0 - glyph_inset,
                    tile_size / 2.0 - glyph_inset,
                );
                icons::draw_icon(
                    scene,
                    cluster_xform,
                    glyph_rect,
                    spoke_icon_for_label(&spoke.text.to_lowercase()),
                    glyph_color,
                    LUCIDE_DEFAULT_STROKE_PX,
                );

                // -- Label ----------------------------------------------
                let label_color = if is_active {
                    Color::rgba8(255, 255, 255, (255.0 * cluster_alpha) as u8)
                } else {
                    Color::rgba8(255, 255, 255, (255.0 * 0.80 * cluster_alpha) as u8)
                };
                let label_xform = cluster_xform
                    * Affine::translate((
                        label_cx_local - (text_w_raw as f64) / 2.0,
                        -(text_h as f64) / 2.0,
                    ));
                text_renderer.draw_text(
                    scene,
                    &label_text,
                    label_size,
                    label_color,
                    label_xform,
                    None,
                );

                // -- Cache cluster's screen-space hit rect --------------
                let cluster_h = tile_size.max(text_h as f64);
                let cluster_local_x0 = label_cx_local - text_w / 2.0;
                let cluster_local_x1 = tile_cx_local + tile_size / 2.0;
                let cluster_local_y0 = -cluster_h / 2.0;
                let cluster_local_y1 = cluster_h / 2.0;
                let corners_local = [
                    Point::new(cluster_local_x0, cluster_local_y0),
                    Point::new(cluster_local_x1, cluster_local_y0),
                    Point::new(cluster_local_x1, cluster_local_y1),
                    Point::new(cluster_local_x0, cluster_local_y1),
                ];
                let cluster_screen_xform = Affine::translate((anchor_x, anchor_y))
                    * Affine::rotate(angle_rad + extra_pi + std::f64::consts::FRAC_PI_2);
                let mut min_x = f64::INFINITY;
                let mut min_y = f64::INFINITY;
                let mut max_x = f64::NEG_INFINITY;
                let mut max_y = f64::NEG_INFINITY;
                for p in corners_local {
                    let q = cluster_screen_xform * p;
                    min_x = min_x.min(q.x);
                    min_y = min_y.min(q.y);
                    max_x = max_x.max(q.x);
                    max_y = max_y.max(q.y);
                }
                self.spoke_hit_rects[i] = Rect::new(min_x, min_y, max_x, max_y);

                // -- Per-spoke presence dots ----------------------------
                if let Some(ws_id) = spoke.workspace_id {
                    let key = PresenceKey::Workspace { id: format!("{}", ws_id) };
                    let peers = presence.peers_on(&key);
                    if !peers.is_empty() {
                        let peer_count = peers.len().min(3);
                        let dots_radius = radius - 8.0;
                        let tip_x = cx + dots_radius * angle_rad.cos();
                        let tip_y = cy + dots_radius * angle_rad.sin();
                        let perp_x = -angle_rad.sin();
                        let perp_y = angle_rad.cos();
                        let spacing = 10.0_f64;
                        let centre_bias = (peer_count as f64 - 1.0) / 2.0;
                        for (k, user) in peers.iter().take(peer_count).enumerate() {
                            let offset = (k as f64 - centre_bias) * spacing;
                            let dx = perp_x * offset;
                            let dy = perp_y * offset;
                            let (rr, gg, bb) = presence.color_for(user);
                            let color = Color::rgb8(rr, gg, bb);
                            let dot = Circle::new((tip_x + dx, tip_y + dy), 4.0);
                            scene.fill(Fill::NonZero, transform, color, None, &dot);
                        }
                    }
                }
            }
        }

        // ---- Pass 6: static centre-dot ruler ----------------------------
        // Concept (App.tsx:108-122): one tiny dot per real workspace at
        // angles `(i - (N-1)/2) · 2°`, near the rim, NOT rotating with
        // the wheel. Active = coloured + soft white glow (1 px wider
        // than concept's 8px to compensate for our smaller absolute
        // size); inactive = 4-px white/20 specks. Dots are placed
        // INSIDE the rim (`dots_r = radius - 6`) so they read as part
        // of the disc rather than floating outside it.
        if !self.spokes.is_empty() {
            let dots_r = radius - 6.0;
            let n = self.spokes.len() as f32;
            for (i, _) in self.spokes.iter().enumerate() {
                let dot_angle_deg = (i as f32 - (n - 1.0) / 2.0) * 2.0;
                let a_rad = (dot_angle_deg as f64).to_radians();
                let dx = cx + dots_r * a_rad.cos();
                let dy = cy + dots_r * a_rad.sin();
                if i == self.active_idx {
                    // Soft white glow: three concentric translucent
                    // circles fading outward, then the coloured core.
                    for &(r, alpha) in &[(11.0_f64, 0.10_f64), (8.0, 0.18), (6.0, 0.30)] {
                        let glow = Circle::new((dx, dy), r);
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            Color::rgba8(255, 255, 255, (alpha * 255.0) as u8),
                            None,
                            &glow,
                        );
                    }
                    let base = self.spoke_color(i);
                    let core = Circle::new((dx, dy), 4.0);
                    scene.fill(Fill::NonZero, transform, base, None, &core);
                } else {
                    let speck = Circle::new((dx, dy), 2.0);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        Color::rgba8(255, 255, 255, 76), // white/0.30
                        None,
                        &speck,
                    );
                }
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
                self.wheel_accumulator += dy * WHEEL_SCROLL_FACTOR;
                self.wheel_last_event = Some(Instant::now());
                self.rotation_deg -= self.wheel_accumulator;
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
        // Wheel-scroll snap-back: fire once the gap since the last
        // wheel event exceeds `WHEEL_SCROLL_TIMEOUT_MS`.
        if let Some(last) = self.wheel_last_event {
            if last.elapsed() > Duration::from_millis(WHEEL_SCROLL_TIMEOUT_MS) {
                let pre_snap = self.rotation_deg;
                self.snap_to_nearest();
                let snapped = self.rotation_deg;
                if (pre_snap - snapped).abs() > f32::EPSILON {
                    let params = SpringPresets::default().snappy;
                    let mut s = Spring::new(pre_snap, params);
                    s.set_target(snapped);
                    self.rotation_spring = Some(s);
                }
                self.wheel_accumulator = 0.0;
                self.wheel_last_event = None;
                keep = true;
            } else {
                keep = true;
            }
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
        state.tree.set_active(WorkspaceId(id));
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
            chrome.borrow().tree.active,
            expected_id,
            "snap_to_nearest must publish the new active workspace id to chrome"
        );
        assert_ne!(
            chrome.borrow().tree.active,
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
            state.tree.workspaces.len(),
            initial_count + 1,
            "plus-press must grow the workspace count by exactly 1"
        );
        assert_ne!(
            state.tree.active, initial_active,
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
        use quick_ui::permissions::UserId;
        use quick_ui::presence::{PresenceCtx, PresenceKey};

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
        use quick_ui::permissions::UserId;
        use quick_ui::presence::{PresenceCtx, PresenceKey};

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

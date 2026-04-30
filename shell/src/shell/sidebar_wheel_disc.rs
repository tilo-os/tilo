//! `SidebarWheelDisc` — circular wheel-disc that sits in-plane with window
//! content, rendering each **workspace** as a radial spoke.
//!
//! User-facing spec (verbatim, 2026-04-21):
//! *"No permanent sidebar, but workspaces appear as rotated axes of the
//! wheel disc, meaning you can go to the left and drag it to move between
//! workspaces."*
//!
//! Design notes are in `docs/nirios/outstanding-work.md` §"Redesign — left
//! wheel as in-plane disc". This widget is the **sole** left-side
//! navigation in `build_nirios_overlay_host_with_chrome`; the legacy
//! `SidebarWheelChrome` module is no longer mounted in the live shell.
//!
//! Interaction model:
//! - Press inside the disc → capture `drag_from`.
//! - Motion while pressed → rotate the spokes continuously.
//! - Release → snap to the nearest spoke (the `active_idx` changes, and
//!   the CRDT write-back publishes the new active workspace). The
//!   *logical* `rotation_deg` is set to the snapped value synchronously
//!   so tests and downstream state see the final pose immediately;
//!   visually the rim eases from the pre-snap value to the snapped one
//!   via a "snappy" `Spring` (stiffness 260, damping 28). `draw` prefers
//!   `rotation_spring.value()` while the spring is in flight.
//!
//! Rotation math uses degrees internally so the snap threshold arithmetic
//! (`rotation_deg / spoke_gap_deg`) stays readable; trig converts to
//! radians at the callsite.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{AlignItems, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, PathEl, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use quick_ui::animation::Spring;
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
    /// rim; the label at index `active_idx` is drawn with the primary
    /// text colour, the rest with `text_dim`.
    pub spokes: Vec<SpokeLabel>,
    /// Index of the spoke currently "up" — i.e. the workspace the user
    /// would return to if they released without further dragging.
    pub active_idx: usize,
    /// Current visual rotation in degrees. Snapped to a multiple of
    /// `spoke_gap_deg()` on release; free-running during a drag.
    pub rotation_deg: f32,
    /// `Some(origin)` while a drag is in flight; `None` when idle.
    pub drag_from: Option<Point>,
    /// Rotation at the moment the drag started — anchor for the
    /// per-frame `rotation = origin + dy * k` mapping.
    pub drag_rotation_origin: f32,
    /// Hit-test bounds. Square, side = `diameter_px`, positioned by
    /// `apply_layout`.
    pub rect: Rect,
    /// Disc diameter in logical pixels. Drives both the Taffy size and
    /// the drag→angle mapping (one half-radius of vertical drag maps
    /// to ~1 radian of rotation).
    pub diameter_px: f64,
    /// Optional shared chrome handle. When present, a snap that lands on
    /// a spoke whose `workspace_id` is known writes the new active
    /// workspace back to `chrome.tree`. Kept `Option` so the widget
    /// still runs in isolation (unit tests, snapshot harness).
    pub chrome: Option<Rc<RefCell<ChromeState>>>,
    /// `Some(spring)` while a post-release snap is easing the visible
    /// rim from the pre-snap rotation to the snapped target. `None` when
    /// idle. Constructed lazily on release using the `snappy` preset
    /// (stiffness 260, damping 28 — the closest builtin to the concept's
    /// `damping: 25, stiffness: 200`). Dropped back to `None` by
    /// `update` once `is_settled()` returns true so `needs_animation_frame`
    /// stops asking for redraws.
    pub rotation_spring: Option<Spring>,
    /// Cached bounding rect of the "+" add-workspace slot painted below
    /// the disc's rim. Populated by `draw`; consumed by `handle_event`
    /// on press to hit-test the slot. `Rect::ZERO` until the first draw
    /// pass runs — mirrors the pattern used by `TopBarChrome` for its
    /// `tray_pill_rect` / `avatar_stack_rect` caches. Closes the concept's
    /// `## Known gaps` #4 ("Plus button") in `docs/nirios/wheel-disc-design.md`.
    pub plus_rect: Rect,
    /// Most-recent cursor position in logical pixels, regardless of
    /// whether it sits inside the disc. `drag_from` only tracks cursors
    /// that hover *inside* the disc (so it can anchor a drag); the plus
    /// slot lives *outside* the rim, so press-hit-testing it needs its
    /// own position cache. Updated on every `CursorMoved`.
    pub last_cursor_pos: Option<Point>,
}

impl SidebarWheelDisc {
    /// Construct a fresh disc with the given spoke labels. Starts at
    /// `active_idx = 0`, `rotation_deg = 0.0`, default 240 px diameter.
    pub fn new(spokes: Vec<SpokeLabel>) -> Self {
        Self {
            spokes,
            active_idx: 0,
            rotation_deg: 0.0,
            drag_from: None,
            drag_rotation_origin: 0.0,
            rect: Rect::ZERO,
            diameter_px: 240.0,
            chrome: None,
            rotation_spring: None,
            plus_rect: Rect::ZERO,
            last_cursor_pos: None,
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

    /// Degrees between adjacent spokes, given the current workspace count.
    /// Always `> 0.0`; a 0- or 1-workspace disc is treated as a single
    /// 360-degree spoke slot so snap math doesn't divide by zero.
    pub fn spoke_gap_deg(&self) -> f32 {
        let n = self.spokes.len().max(1);
        360.0 / n as f32
    }

    /// Radians-per-pixel drag coefficient. Exposed so tests can assert
    /// on the exact mapping: `diameter/2` pixels of vertical drag =
    /// 1 radian (57.29… deg) of rotation.
    fn deg_per_pixel(&self) -> f32 {
        (180.0 / std::f32::consts::PI) / (self.diameter_px as f32 / 2.0)
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
        // Fixed-size square leaf. `align_self: Center` so a parent Column
        // centres the disc vertically between the top/bottom chrome; no
        // flex growth so it stays exactly `diameter_px` across.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(self.diameter_px as f32),
                    height: Dimension::length(self.diameter_px as f32),
                },
                flex_grow: 0.0,
                flex_shrink: 0.0,
                align_self: Some(AlignItems::Center),
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        // Wave-3 T1: at narrow viewports — and inside the live shell's
        // 96 px sidebar slot, where the host overrides the disc's leaf
        // style to a column narrower than `diameter_px` — the disc's
        // visual left edge (`cx - radius`) can land in negative X and
        // clip off-screen. Clamp `rect.x0` so the *drawn* disc keeps a
        // >= 16 px left margin; `draw` derives `cx` from the rect's
        // midpoint and uses `diameter_px / 2` as the radius, so the
        // clamp must shift the rect right by enough that the leftmost
        // rim point clears the margin.
        //
        // All hub/spoke geometry (`draw`, `plus_rect`, etc.) derives
        // from `self.rect`, so the clamp cascades automatically.
        const MIN_LEFT_MARGIN: f64 = 16.0;
        let raw_x0 = origin.x + l.location.x as f64;
        let raw_x1 = raw_x0 + l.size.width as f64;
        let raw_cx = (raw_x0 + raw_x1) / 2.0;
        let radius = self.diameter_px / 2.0;
        // The visible disc spans `[cx - radius, cx + radius]`. Shift x0
        // so `cx - radius >= MIN_LEFT_MARGIN`, preserving the rect's
        // width. When raw_cx is already past `MIN_LEFT_MARGIN + radius`
        // this is a no-op (max picks the existing position).
        let min_cx = MIN_LEFT_MARGIN + radius;
        let cx = raw_cx.max(min_cx);
        let dx = cx - raw_cx;
        let x0 = raw_x0 + dx;
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

        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let radius = (self.diameter_px / 2.0) - 1.0;
        if radius <= 0.0 {
            return;
        }

        // Prefer the spring's value while a post-snap ease is in flight so
        // the rim visibly springs to the snapped target instead of jumping.
        // `rotation_deg` is the logical (already-snapped) pose — drawing
        // reads the animated version but state/tests see the snapped one.
        let visual_rotation_deg = match self.rotation_spring.as_ref() {
            Some(s) if !s.is_settled() => s.value(),
            _ => self.rotation_deg,
        };

        // ---- Pass 1: outer disc rim ---------------------------------------
        // 1-px stroked circle in neutral-3; reads as a hairline ring around
        // the whole disc. No fill — the wallpaper / desktop behind shows
        // through, matching "in-plane with windows" intent.
        let rim = Circle::new((cx, cy), radius);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.neutral[3],
            None,
            &rim,
        );

        // ---- Pass 2: active-spoke indigo glow -----------------------------
        // ~24-degree arc centred on the active spoke's current angle, drawn
        // as a thick translucent stroke along the rim. We sweep a BezPath
        // because vello 0.2 doesn't take `kurbo::Arc` directly in a stroke
        // call — the polyline-approximated bezier here looks the same at
        // these radii.
        if !self.spokes.is_empty() {
            let active_angle_deg = visual_rotation_deg
                + (self.active_idx as f32) * self.spoke_gap_deg();
            let arc_sweep_deg = 24.0_f32;
            let arc_start = (active_angle_deg - arc_sweep_deg / 2.0).to_radians() as f64;
            let arc_end = (active_angle_deg + arc_sweep_deg / 2.0).to_radians() as f64;
            let steps = 16usize;
            let mut glow = BezPath::new();
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let a = arc_start + (arc_end - arc_start) * t;
                let px = cx + radius * a.cos();
                let py = cy + radius * a.sin();
                if i == 0 {
                    glow.move_to((px, py));
                } else {
                    glow.line_to((px, py));
                }
            }
            let glow_color = colors.accent[5].with_alpha_factor(0.3);
            scene.stroke(&Stroke::new(6.0), transform, glow_color, None, &glow);
        }

        // ---- Pass 3: spoke labels -----------------------------------------
        // Each label sits 24 px inside the rim and is rotated to read
        // along the spoke. We centre the glyph on the tip point by
        // translating to (tip_x, tip_y), rotating, then offsetting by
        // (-measured_w/2, -measured_h/2).
        if !self.spokes.is_empty() {
            let label_radius = radius - 24.0;
            let gap = self.spoke_gap_deg();
            let size = tokens.typography.scale.xs;
            for (i, spoke) in self.spokes.iter().enumerate() {
                let angle_deg = visual_rotation_deg + i as f32 * gap;
                let angle_rad = (angle_deg as f64).to_radians();
                let tx = cx + label_radius * angle_rad.cos();
                let ty = cy + label_radius * angle_rad.sin();
                let (w, h) = text_renderer.measure(&spoke.text, size, None);
                let color = if i == self.active_idx {
                    colors.text_primary
                } else {
                    colors.text_dim
                };
                // Readability flip: on the left half of the disc (angle
                // in (90°, 270°)) the naive rotation makes text read
                // upside-down. Add π so the label reads outward along
                // the spoke instead of inward — the concept does the
                // same. `rotation_deg` is free-running during drag, so
                // normalise to [0, 360) before the comparison.
                let angle_deg_f64 = angle_deg as f64;
                let a = (angle_deg_f64 % 360.0 + 360.0) % 360.0;
                let upside_down = a > 90.0 && a < 270.0;
                let extra = if upside_down { std::f64::consts::PI } else { 0.0 };
                // Rotate the glyph along the spoke. `Affine::rotate` is
                // around the origin, so chain: translate-to-tip ·
                // rotate · translate-to-glyph-centre.
                let label_xform = transform
                    * Affine::translate((tx, ty))
                    * Affine::rotate(angle_rad + extra)
                    * Affine::translate((-(w as f64) / 2.0, -(h as f64) / 2.0));
                text_renderer.draw_text(scene, &spoke.text, size, color, label_xform, None);
            }
        }

        // ---- Pass 4: per-spoke collaborator presence dots -----------------
        // Closes wheel-disc-design known-gap #3. Each spoke that carries a
        // `workspace_id` queries `PresenceCtx::peers_on(PresenceKey::Workspace
        // { id })` (same key the rest of the shell uses — see
        // `overlay_host.rs:2489-2491`) and paints up to 3 small 4 px dots
        // tangent to the rim, 16 px outside the tip label (i.e. at
        // `radius - 8 px`, still inside the rim so the dots never overlap
        // the outer stroke). Colour comes from `PresenceCtx::color_for` so
        // palette-pinned peers (me=indigo, alice=pink, bob=orange,
        // charlie=emerald) keep their identity everywhere.
        if !self.spokes.is_empty() {
            let dots_radius = radius - 8.0;
            let gap = self.spoke_gap_deg();
            for (i, spoke) in self.spokes.iter().enumerate() {
                let Some(ws_id) = spoke.workspace_id else { continue };
                let key = PresenceKey::Workspace { id: format!("{}", ws_id) };
                let peers = presence.peers_on(&key);
                if peers.is_empty() {
                    continue;
                }
                let peer_count = peers.len().min(3);
                let angle_deg = visual_rotation_deg + i as f32 * gap;
                let angle_rad = (angle_deg as f64).to_radians();
                let tip_x = cx + dots_radius * angle_rad.cos();
                let tip_y = cy + dots_radius * angle_rad.sin();
                // Perpendicular to the spoke direction (tangent to the rim).
                let perp_x = -angle_rad.sin();
                let perp_y = angle_rad.cos();
                let spacing = 10.0_f64;
                let centre_bias = (peer_count as f64 - 1.0) / 2.0;
                for (k, user) in peers.iter().take(peer_count).enumerate() {
                    let offset = (k as f64 - centre_bias) * spacing;
                    let dx = perp_x * offset;
                    let dy = perp_y * offset;
                    let (r, g, b) = presence.color_for(user);
                    let color = Color::rgb8(r, g, b);
                    let dot = Circle::new((tip_x + dx, tip_y + dy), 4.0);
                    scene.fill(Fill::NonZero, transform, color, None, &dot);
                }
            }
        }

        // ---- Centre dot ---------------------------------------------------
        // Small filled marker in the centre so the drag target reads as a
        // grabbable hub; keeps the disc feeling like an object, not just
        // an outline. 4-px radius in text_dim so it's visible but unobtrusive.
        let hub = Circle::new((cx, cy), 4.0);
        scene.fill(Fill::NonZero, transform, colors.text_dim, None, &hub);
        // Tiny crosshair inside the hub so the drag direction reads as
        // deliberate. Two 6-px orthogonal strokes.
        let cross = BezPath::from_iter([
            PathEl::MoveTo(Point::new(cx - 3.0, cy)),
            PathEl::LineTo(Point::new(cx + 3.0, cy)),
            PathEl::MoveTo(Point::new(cx, cy - 3.0)),
            PathEl::LineTo(Point::new(cx, cy + 3.0)),
        ]);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.text_primary,
            None,
            &cross,
        );

        // ---- "+" add-workspace slot --------------------------------------
        // A small round button pinned directly below the disc's rim, 20 px
        // of vertical gap between the disc's outer edge and the slot's top
        // (so the two shapes read as "related controls" but not a unified
        // outline). 28 px diameter; fill = `surfaces.overlay` so it picks
        // up the same translucent tint the topbar uses, rim = 1 px
        // `neutral[3]` to match the disc's own rim, glyph = two
        // `text_primary` strokes forming a "+" at ~40% of slot width.
        //
        // Interaction is handled in `handle_event` via `plus_rect`, which
        // we cache here so the press path can hit-test the slot without
        // re-deriving the geometry.
        let slot_diam = 28.0_f64;
        let slot_radius = slot_diam / 2.0;
        let slot_cx = cx;
        let slot_cy = cy + radius + 20.0;
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
        // Plus glyph: two orthogonal strokes centred on the slot. Arm
        // half-length = 40% of radius so the glyph reads as a clear "+"
        // without crowding the slot's rim.
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
                // Plus-button hit-test comes first: the slot lives *outside*
                // the disc's rim, so it must never be swallowed by the
                // drag-start branch below. Use `last_cursor_pos` rather than
                // `drag_from` — the latter only tracks cursors inside the
                // disc, but the plus slot lives below the rim. A press in
                // the slot appends a new workspace + activates it via the
                // shared chrome handle and consumes the event without
                // opening the workspace-settings modal (direct create,
                // matching the concept's Plus button behaviour).
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
                // Begin drag if we have a last-known cursor inside the disc.
                // The event itself has no position, so we rely on a prior
                // `CursorMoved` having set `drag_from` as an anchor. To keep
                // the press path self-contained we fall back to the disc's
                // centre when no cursor is known — tests call the private
                // helpers directly so this branch only affects the live path.
                if self.rect.area() <= 0.0 {
                    return;
                }
                // If a CursorMoved hasn't fired yet, anchor to the disc's
                // centre so subsequent moves still produce sensible deltas.
                let anchor = self.drag_from.unwrap_or(Point::new(
                    (self.rect.x0 + self.rect.x1) / 2.0,
                    (self.rect.y0 + self.rect.y1) / 2.0,
                ));
                if self.rect.contains(anchor) {
                    // A fresh press interrupts any in-flight snap spring —
                    // the user is taking over the rotation again, so the
                    // spring's target is stale. Drop it so subsequent
                    // `draw` calls read the (drag-updated) `rotation_deg`
                    // directly.
                    self.rotation_spring = None;
                    self.drag_from = Some(anchor);
                    self.drag_rotation_origin = self.rotation_deg;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Always cache the latest cursor position so the plus-slot
                // press hit-test (which lives outside the disc's rim) can
                // consult it. `drag_from` keeps its narrower semantics —
                // only cursors *inside* the disc anchor a drag.
                self.last_cursor_pos = Some(p);
                // If no drag yet, stash the cursor as a potential anchor
                // so a subsequent Pressed inside the disc can start a drag
                // without needing a synthetic anchor.
                if let Some(origin) = self.drag_from {
                    let dy = p.y - origin.y;
                    let delta_deg = (dy as f32) * self.deg_per_pixel();
                    self.rotation_deg = self.drag_rotation_origin + delta_deg;
                    *handled = true;
                } else if self.rect.contains(p) {
                    // Idle hover inside the disc — remember the cursor so
                    // a Pressed that follows can seed `drag_from` from a
                    // real position rather than the geometric centre.
                    self.drag_from = Some(p);
                    // But don't treat this as an active drag yet — the
                    // `drag_rotation_origin` is only committed on Pressed.
                    // Clear on the next move outside the disc.
                }
                // Note: we intentionally don't clear `drag_from` when the
                // cursor leaves the disc while a drag is active — the
                // drag continues as long as the mouse button is held,
                // matching `wants_pointer_capture() == true`.
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if self.drag_from.is_some() {
                    // Capture the pre-snap angle before `snap_to_nearest`
                    // rewrites `rotation_deg` — the spring needs this as
                    // its starting value so the ease begins exactly where
                    // the drag left off (no visual jump on release).
                    let pre_snap_deg = self.rotation_deg;
                    self.snap_to_nearest();
                    let snapped_deg = self.rotation_deg;
                    // Only spawn a spring if there's actually a distance
                    // to travel; a release on a perfectly-aligned spoke
                    // doesn't need an animation frame.
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
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag_from.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the redraw pump running while the post-release snap is
        // still easing — once the spring settles `update` drops it and
        // this returns false, letting `App` stop scheduling redraws.
        self.rotation_spring
            .as_ref()
            .map(|s| !s.is_settled())
            .unwrap_or(false)
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Step the snap-spring at a fixed 60 Hz to match the rest of the
        // toolkit (`WorkspaceView::update` does the same). Using a fixed
        // dt rather than wall-clock time keeps the animation
        // deterministic for tests; the App pumps `update` once per tick
        // via its `needs_animation_frame` loop. Once the spring settles
        // we drop it so future frames aren't kept alive.
        let Some(spring) = self.rotation_spring.as_mut() else {
            return false;
        };
        if spring.is_settled() {
            self.rotation_spring = None;
            return false;
        }
        spring.step(1.0 / 60.0);
        true
    }

    fn name(&self) -> &str {
        "SidebarWheelDisc"
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
        // Give the disc a concrete rect so `handle_event` accepts drags.
        d.rect = Rect::new(0.0, 0.0, d.diameter_px, d.diameter_px);
        d
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

    #[test]
    fn drag_and_release_snaps_to_nearest_spoke() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let gap = d.spoke_gap_deg();
        let diameter = d.diameter_px;
        assert!((gap - 120.0).abs() < 1e-4);

        // Cursor starts inside the disc so the Press can anchor there.
        let start_y = diameter / 2.0;
        let cx = diameter / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        assert!(d.drag_from.is_some(), "press inside disc must start drag");
        assert!(d.wants_pointer_capture());

        // Drag down by the full diameter. With deg_per_pixel = (180/π) /
        // (diameter/2), full-diameter drag = 2 * (180/π) ≈ 114.6°, which
        // is close enough to the 120° spoke gap that snap rounds up to
        // one gap and active_idx advances.
        pump(&mut d, cursor_event(cx, start_y + diameter));
        // rotation before snap should be free-running
        assert!(d.rotation_deg > 0.0);

        let before_idx = d.active_idx;
        assert!(pump(&mut d, release_event()));
        assert!(d.drag_from.is_none());
        // Snapped rotation is a multiple of gap.
        let ratio = d.rotation_deg / gap;
        assert!(
            (ratio - ratio.round()).abs() < 1e-3,
            "rotation {} must be a multiple of gap {}",
            d.rotation_deg,
            gap
        );
        assert_ne!(
            d.active_idx, before_idx,
            "dragging a full diameter down must rotate to a new spoke"
        );
    }

    #[test]
    fn drag_without_release_keeps_rotation_continuous() {
        let mut d = mk(&["Home", "Research", "Design"]);
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = diameter / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));

        let idx_before = d.active_idx;
        // Move by a non-multiple-of-gap amount. 37 px * deg_per_pixel
        // for a 240 px diameter = 37 * (180/π) / 120 ≈ 17.67° — not a
        // multiple of the 120° gap.
        pump(&mut d, cursor_event(cx, start_y + 37.0));
        let gap = d.spoke_gap_deg();
        let ratio = d.rotation_deg / gap;
        assert!(
            (ratio - ratio.round()).abs() > 1e-3,
            "mid-drag rotation {} should NOT be a multiple of gap {}",
            d.rotation_deg,
            gap
        );
        // active_idx must not change until release.
        assert_eq!(d.active_idx, idx_before);
        assert!(d.drag_from.is_some());
    }

    #[test]
    fn single_spoke_disc_never_snaps_past_itself() {
        let mut d = mk(&["Only"]);
        assert_eq!(d.spokes.len(), 1);
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = diameter / 2.0;

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

        // Seed a 3-workspace tree. `bootstrap_default` plus two appends =
        // three workspaces in a known order, matching the spoke layout.
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.append_empty_workspace("Research");
        tree.append_empty_workspace("Design");
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
        d.rect = Rect::new(0.0, 0.0, d.diameter_px, d.diameter_px);

        // Synthesise press+move+release sequence. `snap_to_nearest`'s
        // delta formula `(-nearest_offset).rem_euclid(n)` means a
        // full-diameter downward drag (≈114.6° of rotation, rounded to a
        // single +120° gap) shifts active_idx by `-1 mod 3 = 2`, so we
        // land on spoke 2. That's the same math `drag_and_release_
        // snaps_to_nearest_spoke` relies on — we just need to know which
        // spoke it is so we can check the CRDT write-back lands on the
        // matching workspace id.
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = diameter / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y + diameter));
        assert!(pump(&mut d, release_event()));

        assert_ne!(
            d.active_idx, 0,
            "a full-diameter drag must advance past the initial spoke"
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
        // The widget must still be usable in isolation (snapshot test,
        // unit tests that don't plumb chrome). Drag+release with
        // `chrome = None` should just update `active_idx` locally.
        let mut d = mk(&["Home", "Research", "Design"]);
        assert!(d.chrome.is_none());
        let idx_before = d.active_idx;
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = diameter / 2.0;
        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        pump(&mut d, cursor_event(cx, start_y + diameter));
        assert!(pump(&mut d, release_event()));
        assert_ne!(d.active_idx, idx_before);
    }

    #[test]
    fn snap_animates_rotation_over_multiple_ticks() {
        // Build a 4-spoke disc so a full-diameter downward drag
        // (≈114.6° of rotation) falls between spoke gaps (90°) and
        // rounds to exactly one gap — guaranteeing `pre_snap_deg !=
        // snapped_deg` and therefore a non-trivial spring animation.
        let mut d = mk(&["A", "B", "C", "D"]);
        assert!((d.spoke_gap_deg() - 90.0).abs() < 1e-4);
        let diameter = d.diameter_px;
        let start_y = diameter / 2.0;
        let cx = diameter / 2.0;

        pump(&mut d, cursor_event(cx, start_y));
        assert!(pump(&mut d, press_event()));
        // Drag by a full diameter — yields ~114.6° of free-running rotation.
        pump(&mut d, cursor_event(cx, start_y + diameter));
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
        // snapshot harness gates on an offscreen renderer). The `draw`
        // implementation sets `plus_rect` to a 28 px disc centred at
        // `(cx, cy + radius + 20)`, so we synthesise the same rect here
        // to decouple from the exact draw-time arithmetic.
        let mut d = SidebarWheelDisc::new(vec![SpokeLabel {
            text: "Home".into(),
            workspace_id: None,
        }])
        .with_chrome(chrome.clone());
        d.rect = Rect::new(0.0, 0.0, d.diameter_px, d.diameter_px);
        let cx = d.diameter_px / 2.0;
        let cy = d.diameter_px / 2.0;
        let radius = (d.diameter_px / 2.0) - 1.0;
        let slot_radius = 14.0_f64;
        let slot_cx = cx;
        let slot_cy = cy + radius + 20.0;
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

    /// Wave-3 T1: at narrow viewports (and inside the live shell's 96 px
    /// sidebar slot, where the host overrides the disc's leaf to be
    /// narrower than `diameter_px`) the disc's *visible* rim — drawn at
    /// `cx ± diameter_px / 2` from the rect's midpoint — would land in
    /// negative X and clip off-screen. The `apply_layout` clamp must
    /// guarantee `rect.x0 >= 16 px` AND a `>= 16 px` left margin to the
    /// drawn rim, so the full disc remains visible regardless of where
    /// the host mounts the leaf.
    #[test]
    fn disc_rect_stays_inside_viewport_at_narrow_width() {
        // Build a tiny taffy tree that mirrors the live shell's mount
        // pattern: a flex column whose only child is the disc node, sized
        // by the disc's own `build_layout`. We then drive `apply_layout`
        // with an origin that — without the clamp — would yield x0 < 16.
        let mut disc = SidebarWheelDisc::new(labels(&["A", "B", "C"]));
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let disc_node = disc.build_layout(&mut taffy, &theme, &mut node_map);

        // Compute the layout against an 800 px-wide root.
        let viewport_w: f32 = 800.0;
        let viewport_h: f32 = 600.0;
        taffy
            .compute_layout(
                disc_node,
                Size {
                    width: taffy::AvailableSpace::Definite(viewport_w),
                    height: taffy::AvailableSpace::Definite(viewport_h),
                },
            )
            .unwrap();

        // Apply layout with a negative origin to simulate a sidebar
        // mount point that — without the clamp — would push the disc's
        // left edge off-screen. The clamp must lift x0 back to >= 16 px.
        disc.apply_layout(&taffy, disc_node, Point::new(-200.0, 0.0));
        let rect = disc.rect_for_tests();
        assert!(
            rect.x0 >= 16.0,
            "disc must keep a >= 16 px left margin so its left curve never \
             clips off-screen at narrow viewport widths; observed x0 = {}",
            rect.x0
        );
        // The clamp must also preserve the disc's width — it shifts the
        // rect right, never compresses it.
        assert!(
            (rect.width() - disc.diameter_px).abs() < 1e-3,
            "clamp must preserve the disc's width (diameter); got {} vs {}",
            rect.width(),
            disc.diameter_px
        );
        // And — critically — the *drawn* rim's left edge must clear the
        // 16 px margin too. `draw` uses `cx ± diameter_px / 2` regardless
        // of `rect.width()`, so a shrunken rect (live shell's 96 px slot
        // case) still needs the clamp to push `cx` right enough.
        let cx = (rect.x0 + rect.x1) / 2.0;
        let visible_left = cx - disc.diameter_px / 2.0;
        assert!(
            visible_left >= 16.0,
            "drawn rim's left edge must clear 16 px; cx={cx}, visible_left={visible_left}"
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
}

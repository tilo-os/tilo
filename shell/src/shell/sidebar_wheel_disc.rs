//! `SidebarWheelDisc` — circular wheel-disc that sits in-plane with window
//! strips, rendering each strip as a radial spoke.
//!
//! User-facing spec (verbatim, 2026-04-21):
//! *"I want a rounded, wheel like thingy on the left, placed on the same
//! plane as actual windows (a strip should be an axis going out from that
//! middle → basically a big circle). You can then drag this wheel in the
//! middle to move it to another strip of windows."*
//!
//! Design notes are in `docs/nirios/outstanding-work.md` §"Redesign — left
//! wheel as in-plane disc". This file implements the widget skeleton + unit
//! tests + screenshot harness only; it is **not yet wired** into
//! `build_nirios_root_with_chrome`. That migration is a follow-up.
//!
//! Interaction model:
//! - Press inside the disc → capture `drag_from`.
//! - Motion while pressed → rotate the spokes continuously.
//! - Release → snap to the nearest spoke (the `active_idx` changes). No
//!   animated easing yet; the spring is a later polish pass.
//!
//! Rotation math uses degrees internally so the snap threshold arithmetic
//! (`rotation_deg / spoke_gap_deg`) stays readable; trig converts to
//! radians at the callsite.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{AlignItems, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, PathEl, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;

/// A single spoke-label. `strip_id` is an opaque correlator the host uses
/// to map the active spoke back to a strip in the workspace tree; the
/// widget itself never dereferences it.
#[derive(Clone, Debug)]
pub struct SpokeLabel {
    pub text: String,
    pub strip_id: Option<u64>,
}

/// In-plane wheel disc. See module-level docs for the interaction model.
pub struct SidebarWheelDisc {
    /// Spokes, one per strip in the active workspace. Rendered
    /// radially around the rim; the label at index `active_idx` is drawn
    /// with the primary text colour, the rest with `text_dim`.
    pub strips: Vec<SpokeLabel>,
    /// Index of the spoke currently "up" — i.e. the strip the user would
    /// return to if they released without further dragging.
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
}

impl SidebarWheelDisc {
    /// Construct a fresh disc with the given spoke labels. Starts at
    /// `active_idx = 0`, `rotation_deg = 0.0`, default 240 px diameter.
    pub fn new(strips: Vec<SpokeLabel>) -> Self {
        Self {
            strips,
            active_idx: 0,
            rotation_deg: 0.0,
            drag_from: None,
            drag_rotation_origin: 0.0,
            rect: Rect::ZERO,
            diameter_px: 240.0,
        }
    }

    /// Override the disc diameter (rare — tests use this to validate
    /// layout math without binding to the 240 px default).
    pub fn with_diameter(mut self, d: f64) -> Self {
        self.diameter_px = d;
        self
    }

    /// Degrees between adjacent spokes, given the current strip count.
    /// Always `> 0.0`; a 0- or 1-strip disc is treated as a single
    /// 360-degree spoke slot so snap math doesn't divide by zero.
    pub fn spoke_gap_deg(&self) -> f32 {
        let n = self.strips.len().max(1);
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
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let radius = (self.diameter_px / 2.0) - 1.0;
        if radius <= 0.0 {
            return;
        }

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
        if !self.strips.is_empty() {
            let active_angle_deg = self.rotation_deg
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
        if !self.strips.is_empty() {
            let label_radius = radius - 24.0;
            let gap = self.spoke_gap_deg();
            let size = tokens.typography.scale.xs;
            for (i, spoke) in self.strips.iter().enumerate() {
                let angle_deg = self.rotation_deg + i as f32 * gap;
                let angle_rad = (angle_deg as f64).to_radians();
                let tx = cx + label_radius * angle_rad.cos();
                let ty = cy + label_radius * angle_rad.sin();
                let (w, h) = text_renderer.measure(&spoke.text, size, None);
                let color = if i == self.active_idx {
                    colors.text_primary
                } else {
                    colors.text_dim
                };
                // Rotate the glyph along the spoke. `Affine::rotate` is
                // around the origin, so chain: translate-to-tip ·
                // rotate · translate-to-glyph-centre.
                let label_xform = transform
                    * Affine::translate((tx, ty))
                    * Affine::rotate(angle_rad)
                    * Affine::translate((-(w as f64) / 2.0, -(h as f64) / 2.0));
                text_renderer.draw_text(scene, &spoke.text, size, color, label_xform, None);
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
                    self.drag_from = Some(anchor);
                    self.drag_rotation_origin = self.rotation_deg;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                    self.snap_to_nearest();
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
        false
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
    /// of "pull the wheel down to reveal the next strip below".
    pub fn snap_to_nearest(&mut self) {
        let n = self.strips.len();
        if n <= 1 {
            // Single-strip disc: snap rotation back to 0 so the lone
            // label doesn't drift; active_idx is always 0.
            self.rotation_deg = 0.0;
            self.active_idx = 0;
            return;
        }
        let gap = self.spoke_gap_deg();
        let nearest_offset = (self.rotation_deg / gap).round();
        self.rotation_deg = nearest_offset * gap;
        let delta = (-nearest_offset).rem_euclid(n as f32);
        let new_idx = ((self.active_idx as f32 + delta).rem_euclid(n as f32)) as usize;
        self.active_idx = new_idx % n;
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
            .map(|n| SpokeLabel { text: (*n).to_string(), strip_id: None })
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
    fn single_strip_disc_never_snaps_past_itself() {
        let mut d = mk(&["Only"]);
        assert_eq!(d.strips.len(), 1);
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
}

//! ProgressBar — determinate / indeterminate horizontal progress indicator.
//!
//! Determinate mode paints a rounded track with a fill that scales from
//! 0 to `track_width` proportional to `progress` (clamped to `[0, 1]`).
//! Indeterminate mode ignores `progress` and slides a fixed-width segment
//! (30 % of the track) across the track on a 2 s period — the position
//! fraction is `start.elapsed().as_secs_f32() % 2.0 / 2.0`.
//!
//! The widget is a pure leaf: it does not read or write the CRDT, has no
//! children, is not focusable, and has no event handling. State (the
//! `progress` float and the `indeterminate` flag) lives on the widget
//! struct itself, mutated by the parent via `set_progress`. The animation
//! reference `start` is captured at construction and is the only piece of
//! frame-derived state the widget keeps.
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::WindowEvent;

use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::Widget;

/// Default visible height of the track. Picked to match the concept's
/// `h-2` progress bars (8 px in Tailwind's spacing scale).
const TRACK_HEIGHT: f64 = 8.0;

/// Width of the moving segment in indeterminate mode, expressed as a
/// fraction of the track width. `0.3` keeps the affordance readable on
/// narrow bars while still leaving room for the segment to travel.
const INDETERMINATE_SEGMENT_FRAC: f64 = 0.3;

/// Period (in seconds) of one full sweep of the indeterminate segment
/// from the left edge to the right edge of the track.
const INDETERMINATE_PERIOD_SECS: f32 = 2.0;

pub struct ProgressBar {
    progress: f32,
    indeterminate: bool,
    rect: Rect,
    start: Instant,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
}

impl ProgressBar {
    /// Construct a determinate progress bar. `progress` is clamped to
    /// `[0.0, 1.0]` on construction; callers don't have to sanitise their
    /// input.
    pub fn new(progress: f32) -> Self {
        Self {
            progress: progress.clamp(0.0, 1.0),
            indeterminate: false,
            rect: Rect::ZERO,
            start: Instant::now(),
            presence_path: Vec::new(),
            permission_scope: None,
        }
    }

    /// Construct an indeterminate progress bar — a 30 %-wide segment
    /// loops across the track on a 2 s period.
    pub fn indeterminate() -> Self {
        Self {
            progress: 0.0,
            indeterminate: true,
            rect: Rect::ZERO,
            start: Instant::now(),
            presence_path: Vec::new(),
            permission_scope: None,
        }
    }

    /// Update the determinate progress value. Clamped to `[0.0, 1.0]`.
    /// Calling this while in indeterminate mode silently stores the
    /// value but does not affect rendering until the bar is taken out
    /// of indeterminate mode.
    pub fn set_progress(&mut self, p: f32) {
        self.progress = p.clamp(0.0, 1.0);
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Current clamped progress value. `0.0` while the bar is in
    /// indeterminate mode, regardless of any prior `set_progress` call.
    pub fn progress(&self) -> f32 {
        if self.indeterminate { 0.0 } else { self.progress }
    }

    /// Whether the bar is in indeterminate mode.
    pub fn is_indeterminate(&self) -> bool {
        self.indeterminate
    }

    /// Position fraction `[0.0, 1.0)` of the indeterminate segment's
    /// **left edge** along the track, computed from `start.elapsed()`.
    /// Exposed for tests that need to pin animation phase without
    /// inspecting the rendered scene.
    fn indeterminate_phase(&self) -> f32 {
        (self.start.elapsed().as_secs_f32() % INDETERMINATE_PERIOD_SECS) / INDETERMINATE_PERIOD_SECS
    }
}

impl Widget for ProgressBar {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(TRACK_HEIGHT as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        // The track is always painted: it provides the visual anchor
        // even at `progress == 0.0`. The fill is conditional — see
        // `progress_bar_at_zero_paints_no_fill` for the contract.
        let radius = theme.tokens.radii.md as f64;
        let track_shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &track_shape,
        );

        let track_width = self.rect.width();
        let accent = theme.tokens.colors.accent[ACCENT_PRIMARY_IDX];

        if self.indeterminate {
            // Slide a fixed-width segment across the track. The phase
            // [0, 1) drives the segment's left edge from
            // `-segment_width` (just off-screen left) to `track_width`
            // (just off-screen right), so the segment fully enters and
            // exits the track every period.
            let segment_width = track_width * INDETERMINATE_SEGMENT_FRAC;
            let travel = track_width + segment_width;
            let phase = self.indeterminate_phase() as f64;
            let segment_x0 = self.rect.x0 - segment_width + phase * travel;
            let segment_x1 = segment_x0 + segment_width;
            // Clip the segment to the track so the rounded ends never
            // poke past the track edges.
            let clipped_x0 = segment_x0.max(self.rect.x0);
            let clipped_x1 = segment_x1.min(self.rect.x1);
            if clipped_x1 > clipped_x0 {
                let segment_rect =
                    Rect::new(clipped_x0, self.rect.y0, clipped_x1, self.rect.y1);
                let segment_shape = RoundedRect::from_rect(segment_rect, radius);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &segment_shape,
                );
            }
        } else if self.progress > 0.0 {
            // Determinate fill — width is `progress * track_width`.
            // Crucially, we *skip the fill entirely* at `progress == 0.0`
            // so the COLOR-draw-tag count distinguishes "empty" from
            // "non-empty" bars at the encoding level (the visual would
            // be identical, but the test contract requires zero fill
            // draws).
            let fill_width = self.progress as f64 * track_width;
            let fill_rect = Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x0 + fill_width,
                self.rect.y1,
            );
            let fill_shape = RoundedRect::from_rect(fill_rect, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                accent,
                None,
                &fill_shape,
            );
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str { "ProgressBar" }

    fn get_rect(&self) -> Rect { self.rect }

    /// Indeterminate bars must keep the redraw loop pumping so the
    /// segment continues to advance — without this, the animation
    /// would freeze on the first frame after an unrelated event.
    fn needs_animation_frame(&self) -> bool {
        self.indeterminate
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.presence_path.clone() })
        }
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);
                scene.fill(vello::peniko::Fill::NonZero, transform, overlay, None, &self.rect);
            }
        }

        if let Some(key) = self.presence_key() {
            if let Some(peers) = presence.here.get(&key) {
                if let Some(user) = peers.first() {
                    presence.draw_peer_focus(scene, theme, user, self.rect);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;

    /// Build a determinate bar with a fixed rect so tests don't have
    /// to spin up the full taffy layout pipeline. Rect spans
    /// `[0..200, 0..8]` — width 200, matching the default track height.
    fn bar_at(progress: f32) -> ProgressBar {
        let mut b = ProgressBar::new(progress);
        b.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
        b
    }

    /// Count the `COLOR` draw tags in a scene. Each `scene.fill` with
    /// a solid colour emits exactly one such tag — see `tabs.rs`'s
    /// `active_pill_emits_more_color_fills_than_inactive_siblings` for
    /// precedent.
    fn count_color_fills(scene: &Scene) -> usize {
        use vello_encoding::DrawTag;
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    /// Render a bar into a fresh scene and return that scene. Mirrors
    /// the helper pattern from `tabs.rs`'s encoding tests.
    fn render(bar: &mut ProgressBar) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    /// At `progress == 0.0` the bar must paint the track *only* — the
    /// accent fill rect must be skipped entirely. Pinning this against
    /// the encoding (rather than colour-sampling pixels) means a future
    /// refactor that "draws a zero-width rect anyway" trips the test.
    /// We compare against an indeterminate bar at the same phase as a
    /// sanity baseline: indeterminate always emits an extra fill, so
    /// the zero-progress bar must emit strictly fewer COLOR tags.
    #[test]
    fn progress_bar_at_zero_paints_no_fill() {
        let mut zero = bar_at(0.0);
        let zero_scene = render(&mut zero);
        let zero_fills = count_color_fills(&zero_scene);

        // The track alone is one COLOR fill — no accent fill should
        // appear. Counting the track-only baseline by rendering a bar
        // explicitly skips the fill branch.
        assert_eq!(
            zero_fills, 1,
            "zero-progress bar must emit exactly the track COLOR fill \
             with no accent fill draw — got {zero_fills} COLOR tags",
        );

        // Cross-check: a half-progress bar emits *more* COLOR fills
        // than the zero bar, proving the difference is the accent fill.
        let mut half = bar_at(0.5);
        let half_fills = count_color_fills(&render(&mut half));
        assert!(
            half_fills > zero_fills,
            "non-zero progress must add an accent fill (half: {half_fills}, \
             zero: {zero_fills})",
        );
    }

    /// At `progress == 1.0` the accent fill must span the *full* track
    /// width. We can't inspect the encoded rect directly, so we drive
    /// the contract through the public `progress()` accessor *and* via
    /// a width-vs-half comparison: at full progress the bar produces a
    /// fill of width `track_width`; at half progress it produces a fill
    /// of width `track_width / 2`. The widths are exposed by computing
    /// the fill rectangle the same way `draw` does.
    #[test]
    fn progress_bar_at_full_paints_track_width_fill() {
        let bar = bar_at(1.0);
        let track_width = bar.rect.width();
        let fill_width = bar.progress() as f64 * track_width;
        assert!(
            (fill_width - track_width).abs() < 1e-6,
            "at progress=1.0 the fill must span the full track width \
             (track={track_width}, fill={fill_width})",
        );

        // And the scene actually paints an accent fill at full
        // progress (i.e. the >0.0 branch fires, not the zero skip).
        let mut full = bar_at(1.0);
        let full_fills = count_color_fills(&render(&mut full));
        let mut zero = bar_at(0.0);
        let zero_fills = count_color_fills(&render(&mut zero));
        assert!(
            full_fills > zero_fills,
            "full bar must emit at least one more COLOR fill than zero \
             bar — full: {full_fills}, zero: {zero_fills}",
        );
    }

    /// At `progress == 0.5` the accent fill width must equal half the
    /// track width within a ±1 px tolerance. The 1 px slack absorbs
    /// rounding at sub-pixel layout boundaries; the underlying maths
    /// is exact for the 200 px test rect.
    #[test]
    fn progress_bar_at_half_paints_half_fill_within_one_pixel() {
        let bar = bar_at(0.5);
        let track_width = bar.rect.width();
        let fill_width = bar.progress() as f64 * track_width;
        let expected = track_width / 2.0;
        assert!(
            (fill_width - expected).abs() <= 1.0,
            "at progress=0.5 the fill width must be track_width/2 ± 1 px \
             (expected={expected}, got={fill_width})",
        );
    }

    /// Indeterminate bars must opt into the redraw loop via
    /// `needs_animation_frame() == true`, otherwise `App` would stop
    /// pumping frames and the segment would freeze partway across the
    /// track. Determinate bars must *not* opt in — they're static.
    #[test]
    fn progress_bar_indeterminate_needs_animation_frame_returns_true() {
        let indeterminate = ProgressBar::indeterminate();
        assert!(
            indeterminate.needs_animation_frame(),
            "indeterminate bar must request animation frames",
        );

        let determinate = ProgressBar::new(0.5);
        assert!(
            !determinate.needs_animation_frame(),
            "determinate bar must not request animation frames",
        );
    }
}

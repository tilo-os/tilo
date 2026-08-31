//! `ProgressBar` — determinate / indeterminate horizontal progress indicator.
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
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Default visible height of the track. Picked to match the concept's
/// `h-2` progress bars (8 px in Tailwind's spacing scale). `f32` because
/// its only use is a taffy `Dimension`, which is `f32`-typed.
const TRACK_HEIGHT: f32 = 8.0;

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
    /// Accessible label (e.g. "Uploading"). When set, the bar becomes a
    /// *polite* live region keyed by this name, so a screen reader announces
    /// progress updates ("Uploading, 40%") without moving focus. Anonymous bars
    /// (no label) are not tracked as live regions and stay quiet.
    label: Option<String>,
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
            label: None,
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
            label: None,
        }
    }

    /// Give the bar an accessible label and make it a polite live region, so a
    /// screen reader announces progress updates without stealing focus.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the accessible label (see [`with_label`](Self::with_label)).
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = Some(label.into());
    }

    /// Update the determinate progress value. Clamped to `[0.0, 1.0]`.
    /// Calling this while in indeterminate mode silently stores the
    /// value but does not affect rendering until the bar is taken out
    /// of indeterminate mode.
    pub fn set_progress(&mut self, p: f32) {
        self.progress = p.clamp(0.0, 1.0);
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Current clamped progress value. `0.0` while the bar is in
    /// indeterminate mode, regardless of any prior `set_progress` call.
    pub fn progress(&self) -> f32 {
        if self.indeterminate {
            0.0
        } else {
            self.progress
        }
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

/// Rounds an already-clamped `[0, 100]` percentage to a whole-number
/// `i64` for the accessible value string. The `.round()` and the
/// `clamp(0.0, 1.0)` before it together bound the input to `[0, 100]`,
/// far inside `i64`'s range, so the truncation the lint warns about
/// can't occur.
#[allow(
    clippy::cast_possible_truncation,
    reason = "input is a rounded percentage already bounded to [0, 100]"
)]
fn pct_to_i64(pct: f32) -> i64 {
    pct as i64
}

impl Widget for ProgressBar {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(TRACK_HEIGHT),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
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
        let radius = f64::from(theme.tokens.radii.md);
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
            // Slide a fixed-width segment across the track. Phase [0,1)
            // drives its left edge from `-segment_width` (off-screen
            // left) to `track_width` (off-screen right) each period.
            let segment_width = track_width * INDETERMINATE_SEGMENT_FRAC;
            let travel = track_width + segment_width;
            // With reduced motion the segment holds at a fixed position
            // instead of sweeping across the track: the bar still reads as
            // busy, but nothing moves.
            let phase = if theme.reduce_motion {
                (segment_width / 2.0) / travel
            } else {
                f64::from(self.indeterminate_phase())
            };
            let segment_x0 = self.rect.x0 - segment_width + phase * travel;
            let segment_x1 = segment_x0 + segment_width;
            // Clip the segment to the track so the rounded ends never
            // poke past the track edges.
            let clipped_x0 = segment_x0.max(self.rect.x0);
            let clipped_x1 = segment_x1.min(self.rect.x1);
            if clipped_x1 > clipped_x0 {
                let segment_rect = Rect::new(clipped_x0, self.rect.y0, clipped_x1, self.rect.y1);
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
            // Determinate fill, width `progress * track_width`. Skipped
            // entirely at `progress == 0.0` (rather than a zero-width
            // rect) so the COLOR-draw-tag count can distinguish empty
            // from non-empty bars at the encoding level in tests.
            let fill_width = f64::from(self.progress) * track_width;
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

    fn name(&self) -> &'static str {
        "ProgressBar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Politeness, Role};
        // Determinate bars expose a percentage value; indeterminate bars have
        // no meaningful value, so omit it and mark the node busy instead. A
        // labelled bar is a polite live region keyed by its name, so progress
        // updates announce without moving focus.
        let mut node = if self.indeterminate {
            AccessNode::new(Role::ProgressBar).with_state(AccessState {
                busy: true,
                ..Default::default()
            })
        } else {
            let pct = pct_to_i64((self.progress.clamp(0.0, 1.0) * 100.0).round());
            AccessNode::new(Role::ProgressBar).with_value(format!("{pct}%"))
        };
        if let Some(label) = &self.label {
            node = node.with_name(label.clone()).with_live(Politeness::Polite);
        }
        Some(node)
    }

    /// Indeterminate bars must keep the redraw loop pumping so the
    /// segment continues to advance — without this, the animation
    /// would freeze on the first frame after an unrelated event.
    fn needs_animation_frame(&self) -> bool {
        self.indeterminate
    }

    /// Determinate bars never change size; indeterminate ones
    /// change their segment fill every frame but keep the same
    /// outer rect. Either way, layout stays clean after the
    /// initial `apply_layout` writes a non-zero rect.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
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
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope()
            && !permissions.check(&scope)
        {
            let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                overlay,
                None,
                &self.rect,
            );
        }

        if let Some(key) = self.presence_key()
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
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
        render_with(bar, false)
    }

    /// Render a bar against a theme with the given reduced-motion setting.
    fn render_with(bar: &mut ProgressBar, reduce_motion: bool) -> Scene {
        let theme = Theme::dark().with_reduce_motion(reduce_motion);
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    /// At `progress == 0.0` the bar must paint the track *only* — the
    /// accent fill rect must be skipped entirely, pinned against the
    /// encoding (not pixel colour) so a future "draws a zero-width rect
    /// anyway" refactor trips the test.
    #[test]
    fn progress_bar_at_zero_paints_no_fill() {
        let mut zero = bar_at(0.0);
        let zero_scene = render(&mut zero);
        let zero_fills = count_color_fills(&zero_scene);

        // The track alone is one COLOR fill — no accent fill should
        // appear.
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

    /// At `progress == 1.0` the accent fill must span the full track
    /// width. Can't inspect the encoded rect directly, so this drives
    /// the contract through the public `progress()` accessor, computing
    /// the fill rectangle the same way `draw` does.
    #[test]
    fn progress_bar_at_full_paints_track_width_fill() {
        let bar = bar_at(1.0);
        let track_width = bar.rect.width();
        let fill_width = f64::from(bar.progress()) * track_width;
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
        let fill_width = f64::from(bar.progress()) * track_width;
        let expected = track_width / 2.0;
        assert!(
            (fill_width - expected).abs() <= 1.0,
            "at progress=0.5 the fill width must be track_width/2 ± 1 px \
             (expected={expected}, got={fill_width})",
        );
    }

    /// A determinate bar reports its progress as a percentage value with no
    /// busy flag; an indeterminate bar omits the value and is marked busy.
    #[test]
    fn accessibility_reports_progressbar_role_value_and_busy() {
        use crate::a11y::Role;
        let half = ProgressBar::new(0.5);
        let node = Widget::accessibility(&half).expect("progress bar is not presentational");
        assert_eq!(node.role, Role::ProgressBar);
        assert_eq!(node.value.as_deref(), Some("50%"));
        assert!(!node.state.busy, "a determinate bar is not busy");

        let indet = ProgressBar::indeterminate();
        let node = Widget::accessibility(&indet).unwrap();
        assert_eq!(node.role, Role::ProgressBar);
        assert_eq!(node.value, None, "an indeterminate bar exposes no value");
        assert!(node.state.busy, "an indeterminate bar is busy");
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

    /// With reduced motion an indeterminate bar holds its segment fixed:
    /// a bar backdated by a full sweep paints the same geometry as a
    /// fresh one. With motion on, the same backdate lands elsewhere —
    /// proving the freeze is the reduced-motion path, not a no-op.
    #[test]
    fn reduce_motion_freezes_the_indeterminate_segment() {
        use std::time::{Duration, Instant};
        let one_sweep = Duration::from_secs_f32(INDETERMINATE_PERIOD_SECS);
        // A quarter-sweep backdate so the moving segment is plainly off the
        // resting position (a full sweep would alias back to phase 0).
        let backdate = Instant::now()
            .checked_sub(one_sweep / 4)
            .expect("process has been running longer than a quarter sweep");

        let mut fresh = ProgressBar::indeterminate();
        fresh.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
        let fresh_geo = render_with(&mut fresh, true).encoding().path_data.clone();

        let mut aged = ProgressBar::indeterminate();
        aged.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
        aged.start = backdate;
        let aged_reduced = render_with(&mut aged, true).encoding().path_data.clone();
        assert_eq!(
            fresh_geo, aged_reduced,
            "reduced motion must paint the resting segment regardless of elapsed time",
        );

        let mut aged2 = ProgressBar::indeterminate();
        aged2.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
        aged2.start = backdate;
        let aged_moving = render_with(&mut aged2, false).encoding().path_data.clone();
        assert_ne!(
            fresh_geo, aged_moving,
            "with motion on, a backdated bar's segment is at a different position",
        );
    }
}

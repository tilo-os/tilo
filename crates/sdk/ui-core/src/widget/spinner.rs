//! Spinner — a circular indeterminate-progress indicator.
//!
//! Renders four dots orbiting a centre point at constant angular velocity, driven
//! by wall-clock time captured at construction rather than per-frame mutation, so
//! the widget is cheap to keep alive. Opts into [`Widget::needs_animation_frame`]
//! permanently so the host keeps redrawing while any spinner is visible.

use crate::time::Instant;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};

use crate::color_compat::ColorCompat;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Sized to the concept's body text (14px line-height ≈ 20px) while leaving the
/// dots large enough for the alpha ramp to read.
pub const DEFAULT_SIZE_PX: f32 = 24.0;

const ROTATION_PERIOD_MS: f64 = 1500.0;

/// Hard-coded; the alpha ramp must always have exactly this many entries.
const DOT_COUNT: usize = 4;

/// Index 0 is the leading dot (full opacity); the head of the chain is the
/// brightest point on the orbit.
const DOT_ALPHAS: [f32; DOT_COUNT] = [1.0, 0.75, 0.5, 0.25];

/// ~10% of the diameter keeps the dots visually distinct without overlapping.
const DOT_RADIUS_FRAC: f64 = 0.10;

/// 35% leaves a healthy margin between the dot edge and the bounding box.
const ORBIT_RADIUS_FRAC: f64 = 0.35;

pub struct Spinner {
    size: f32,
    start: Instant,
    rect: Rect,
    /// Captured from the last draw. `Widget::needs_animation_frame` has no
    /// theme argument, so this is how the shared spinner tells the host that a
    /// reduced-motion presentation is static and needs no wakeups.
    reduce_motion: bool,
}

impl Spinner {
    /// Two spinners instantiated in the same frame will be in phase.
    pub fn new() -> Self {
        Self::with_size(DEFAULT_SIZE_PX)
    }

    pub fn with_size(size: f32) -> Self {
        Self {
            size,
            start: crate::time::now(),
            rect: Rect::ZERO,
            reduce_motion: false,
        }
    }

    /// For a parent positioning it by hand: centres the orbit in `rect` the same
    /// way `apply_layout` would.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    #[cfg(test)]
    pub(crate) fn size(&self) -> f32 {
        self.size
    }

    /// Backdates the rotation anchor so a test can prove reduced motion
    /// freezes the chain without sleeping in the test body.
    #[cfg(test)]
    pub(crate) fn set_started_for_test(&mut self, at: Instant) {
        self.start = at;
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Spinner {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::length(self.size),
                height: Dimension::length(self.size),
            },
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        self.reduce_motion = theme.reduce_motion;
        let size = f64::from(self.size);
        let dot_r = size * DOT_RADIUS_FRAC;
        let orbit_r = size * ORBIT_RADIUS_FRAC;
        let centre = self.rect.center();

        // Phase ∈ [0, 1); ms (not seconds) keeps the divisor small for
        // long-lived spinners.
        let phase = if theme.reduce_motion {
            0.0
        } else {
            let elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0;
            (elapsed_ms / ROTATION_PERIOD_MS).fract()
        };

        let accent = theme.tokens.colors.intent.inverted;

        // Evenly spaced offsets so the chain covers a quarter of the orbit.
        for (i, &alpha) in DOT_ALPHAS.iter().enumerate() {
            // `i` and `DOT_COUNT` are both < 8 (a hard-coded 4-entry alpha
            // ramp), far below f64's exact-integer range (2^53) — this
            // conversion cannot lose precision.
            #[allow(
                clippy::cast_precision_loss,
                reason = "i and DOT_COUNT are both small, hard-coded constants (DOT_COUNT = 4), far below f64's 2^53 exact-integer range"
            )]
            let angle_frac = phase - (i as f64) / (DOT_COUNT as f64);
            // -π/2 so the leading dot crosses the top at phase = 0; negative
            // direction is clockwise, matching a "loading" spinner convention.
            let theta = -std::f64::consts::TAU * angle_frac - std::f64::consts::FRAC_PI_2;
            let cx = centre.x + orbit_r * theta.cos();
            let cy = centre.y + orbit_r * theta.sin();
            let dot = Circle::new(Point::new(cx, cy), dot_r);
            let colour = accent.with_alpha_factor(alpha);
            scene.fill(vello::peniko::Fill::NonZero, transform, colour, None, &dot);
        }
    }

    fn name(&self) -> &'static str {
        "Spinner"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(AccessNode::new(Role::Spinner).with_state(AccessState {
            busy: true,
            ..Default::default()
        }))
    }

    /// In the normal presentation the default would freeze the orbit at the
    /// first frame. In a reduced-motion presentation the orbit is deliberately
    /// static, so requesting frames would only waste power.
    fn needs_animation_frame(&self) -> bool {
        !self.reduce_motion
    }

    /// `Rect::ZERO` sentinel for the first frame.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use std::collections::HashMap;
    use taffy::prelude::TaffyMaxContent;
    use vello_encoding::DrawTag;

    #[test]
    fn spinner_contract_size_semantics_and_dots() {
        // Case `spinner_default_size_is_24_px`.
        // Pins `DEFAULT_SIZE_PX` against the public contract — a future
        // change to the default has to touch this deliberately.
        {
            let s = Spinner::new();
            assert_eq!(s.size(), 24.0, "Spinner::new() must default to 24 px");

            let mut s = Spinner::new();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = s.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            s.apply_layout(&taffy, node, Point::ZERO);

            assert_eq!(s.get_rect().width(), 24.0);
            assert_eq!(s.get_rect().height(), 24.0);
        }

        // Case `accessibility_reports_spinner_role_and_busy`.
        {
            use crate::a11y::Role;
            let s = Spinner::new();
            let node = Widget::accessibility(&s).expect("spinner is not presentational");
            assert_eq!(node.role, Role::Spinner);
            assert!(node.state.busy, "a spinner is always busy");
            assert_eq!(node.name, None, "a spinner has no text label");
        }

        // Case `spinner_paints_four_dots`.
        // Every dot is a single `scene.fill(NonZero, ..., Circle)`, so the exact
        // `DrawTag::COLOR` count guards against a background ring, a collapsed
        // chain, or stray text/icon fills.
        {
            let mut s = Spinner::new();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = s.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            s.apply_layout(&taffy, node, Point::ZERO);

            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            s.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

            let colour_fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();

            assert_eq!(
                colour_fills, DOT_COUNT,
                "Spinner must emit exactly {DOT_COUNT} colour fills — one per orbiting dot (got {colour_fills})"
            );
        }
    }

    #[test]
    fn spinner_animates_only_when_motion_is_enabled() {
        // Case `spinner_requests_frames_only_when_motion_is_enabled`.
        // Pin so a future refactor that "cleans up" the override doesn't
        // remove the only thing that keeps the dots moving in production.
        {
            let s = Spinner::new();
            assert!(
                Widget::needs_animation_frame(&s),
                "Spinner must request animation frames before its first motion-on draw"
            );

            let s = Spinner::with_size(48.0);
            assert!(
                Widget::needs_animation_frame(&s),
                "Spinner::with_size must also request animation frames before drawing"
            );
        }

        // Case `reduce_motion_freezes_the_orbit_at_phase_zero`.
        // A spinner backdated half a rotation must paint the *same* geometry
        // as a fresh one under reduced motion (proving `draw` ignores elapsed
        // time), but different geometry with motion on (proving the freeze is
        // the reduced-motion path, not a no-op).
        {
            use crate::time::Duration;
            let half_period = Duration::from_secs_f64(ROTATION_PERIOD_MS / 1000.0 / 2.0);
            // `half_period` is ~750ms, so this can only underflow on a machine
            // whose monotonic clock started under a second ago; fall back to
            // "now" (untested edge case) instead of panicking the test.
            let backdate = crate::time::now()
                .checked_sub(half_period)
                .unwrap_or_else(crate::time::now);

            let mut fresh = Spinner::new();
            let fresh_geo = draw_path_data(&mut fresh, true);

            let mut aged = Spinner::new();
            aged.set_started_for_test(backdate);
            let aged_reduced = draw_path_data(&mut aged, true);

            assert_eq!(
                fresh_geo, aged_reduced,
                "reduced motion must paint the resting orbit regardless of elapsed time",
            );

            let mut aged2 = Spinner::new();
            aged2.set_started_for_test(backdate);
            let aged_moving = draw_path_data(&mut aged2, false);
            assert_ne!(
                fresh_geo, aged_moving,
                "with motion on, a backdated spinner is at a different phase",
            );
        }

        // Case `reduced_motion_stops_animation_wakeups_after_the_static_frame`.
        {
            let mut spinner = Spinner::new();
            let _ = draw_path_data(&mut spinner, true);
            assert!(
                !Widget::needs_animation_frame(&spinner),
                "a frozen reduced-motion spinner must not keep the compositor awake"
            );

            let _ = draw_path_data(&mut spinner, false);
            assert!(
                Widget::needs_animation_frame(&spinner),
                "switching back to normal motion must restore spinner animation"
            );
        }
    }

    fn draw_path_data(s: &mut Spinner, reduce_motion: bool) -> Vec<u32> {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = s.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        s.apply_layout(&taffy, node, Point::ZERO);

        let theme = Theme::dark().with_reduce_motion(reduce_motion);
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        s.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene.encoding().path_data.clone()
    }
}

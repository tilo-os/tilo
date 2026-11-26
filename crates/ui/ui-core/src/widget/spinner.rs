//! Spinner — a circular indeterminate-progress indicator.

use crate::time::Instant;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Circle, Point, Rect};

use crate::color_compat::ColorCompat;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;

#[path = "spinner_component.rs"]
mod component;
pub use component::SpinnerProps;

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
    /// When set, the size follows one line of this role on every layout, so
    /// a spinner beside a status line matches the text at any text scale.
    text_role: Option<tilo_ui_base::tokens::TextRole>,
    start: Instant,
    rect: Rect,
    /// Captured from the last draw.
    reduce_motion: bool,
    /// The indicator step last drawn; `None` before the first draw.
    drawn_step: Option<u64>,
    /// The last draw moved to a new step: its box is damage until the frame
    /// is presented (`mark_drawn`).
    stepped: bool,
    /// What is under way ("Loading messages"), as assistive technology
    /// names the spinner.
    label: Option<String>,
}

impl Spinner {
    pub fn new() -> Self {
        Self::with_size(DEFAULT_SIZE_PX)
    }

    /// A spinner whose turn started at `start`: one drawn afresh each frame
    /// keeps turning from there instead of restarting.
    pub(crate) fn started_at(start: Instant) -> Self {
        Self {
            start,
            ..Self::new()
        }
    }

    pub fn with_size(size: f32) -> Self {
        Self {
            size,
            text_role: None,
            start: crate::time::now(),
            rect: Rect::ZERO,
            reduce_motion: false,
            drawn_step: None,
            stepped: true,
            label: None,
        }
    }

    /// Name what is under way ("Loading messages") for assistive
    /// technology.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The step this spinner shows at `now`: its pose, frozen at 0 under
    /// reduced motion.
    fn step_at(&self, now: Instant) -> u64 {
        if self.reduce_motion {
            0
        } else {
            tilo_ui_base::animation::indicator_step(self.start, now)
        }
    }

    /// Sized to one line of `role` text, for a spinner that sits inline with
    /// a status line rather than on its own.
    #[must_use]
    pub fn for_text(role: tilo_ui_base::tokens::TextRole) -> Self {
        Self {
            text_role: Some(role),
            ..Self::new()
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
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Spinner {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if let Some(role) = self.text_role {
            self.size = theme.tokens.typography.style(role).line_height_px();
        }
        let style = Style {
            size: Size::from_lengths(self.size, self.size),
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text_renderer, theme, mut env) = ctx.parts();
        self.reduce_motion = !theme.motion().loops() || crate::time::wall_frozen();
        let now = crate::time::now();
        let step = Some(self.step_at(now));
        self.stepped |= self.drawn_step != step;
        self.drawn_step = step;
        let size = f64::from(self.size);
        let dot_r = size * DOT_RADIUS_FRAC;
        let orbit_r = size * ORBIT_RADIUS_FRAC;
        let centre = self.rect.center();

        // Phase ∈ [0, 1); ms (not seconds) keeps the divisor small for
        // long-lived spinners.
        let phase = if self.reduce_motion {
            0.0
        } else {
            let elapsed_ms =
                tilo_ui_base::animation::indicator_elapsed(self.start, now).as_secs_f64() * 1000.0;
            (elapsed_ms / ROTATION_PERIOD_MS).fract()
        };

        let accent = theme.tokens.colors.intent.value_fill;

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

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        Some(
            AccessNode::new(Role::Spinner)
                .with_optional_name(self.label.clone())
                .with_state(AccessState::new().busy(true)),
        )
    }

    /// In the normal presentation the default would freeze the orbit at the first frame.
    fn needs_animation_frame(&self) -> bool {
        !self.reduce_motion && !crate::time::wall_frozen()
    }

    /// The framework delivers the motion policy before it asks for a frame, so
    /// a spinner built after the theme was applied holds still without waiting
    /// to learn it from a paint.
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
    }

    /// Only a new step changes what the spinner paints: frames between
    /// steps leave its box undamaged, and an unchanged frame does not
    /// present.
    fn needs_redraw(&self) -> bool {
        self.stepped || self.drawn_step != Some(self.step_at(tilo_ui_base::motion::frame_now()))
    }

    fn mark_drawn(&mut self) {
        self.stepped = false;
    }

    /// `Rect::ZERO` sentinel for the first frame.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::presence::PresenceCtx;
    use tilo_ui_base::text_render::TextRenderer;
    use std::collections::HashMap;
    use taffy::prelude::TaffyMaxContent;
    use vello::Scene;
    use vello::kurbo::Affine;
    use vello_encoding::DrawTag;

    tilo_test_env::scenario! {
        /// A spinner.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_spinner_sizes_steps_and_turns_only_with_motion;

        phase spinner_contract_size_semantics_and_dots;
        {
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

            for text_scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(text_scale);
                let line = theme
                    .tokens
                    .typography
                    .style(tilo_ui_base::tokens::TextRole::Caption)
                    .line_height_px();
                let mut s = Spinner::for_text(tilo_ui_base::tokens::TextRole::Caption);
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = s.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy.parent_node(Style::default(), &[node]);
                taffy
                    .compute_layout(root, taffy::Size::MAX_CONTENT)
                    .unwrap();
                s.apply_layout(&taffy, node, Point::ZERO);
                assert!((s.get_rect().height() - f64::from(line)).abs() <= 0.5);
                assert!(
                    text_scale > 1.0 || line < DEFAULT_SIZE_PX,
                    "a caption line ({line}px) must be smaller than the standalone spinner"
                );
            }

            {
                use tilo_ui_base::a11y::Role;
                let s = Spinner::new();
                let node = Widget::accessibility(&s).expect("spinner is not presentational");
                assert_eq!(node.role, Role::Spinner);
                assert!(node.state.busy, "a spinner is always busy");
                assert_eq!(node.name, None, "a spinner has no text label");
            }

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
                s.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));

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

    }
}

//! How a container's parts move when what it holds changes (`motion::ListChange`): a part the person
//! added fades in while the others slide aside to open its slot, a part taken away fades out where it
//! stood while the others close the gap, and a part moved slides from where it was drawn. The
//! container's [`Feel`] paces it; [`Feel::Still`], a change nobody's hand made (a sync, a shortcut, an
//! agent) and reduced motion land at once.

use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

use crate::collection_transition::RowMotions;
use crate::draw::DrawEnv;
use crate::motion::Feel;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::EffectiveMotion;
use crate::widget::{Element, Widget};

/// A part taken out of the container, painted where it stood while it fades out: never hit-tested and
/// not in the accessibility tree, since the container no longer holds it.
struct Leaver {
    key: u64,
    element: Element,
    /// Where it stood, from the container's origin.
    at: Vec2,
}

/// What a part looks like at an instant: where it is drawn and how opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawnPart {
    /// The part's box as drawn, offset from where it is laid out while it slides.
    pub rect: Rect,
    pub opacity: f32,
    /// Whether the part has left the container and is fading out where it stood.
    pub leaving: bool,
}

pub(super) struct PartMotions {
    feel: Feel,
    rows: RowMotions<u64, Vec2>,
    leavers: Vec<Leaver>,
    /// A change waits for the next layout to start moving.
    pending: bool,
    /// The motion the theme and the person allow, at this container's pace.
    motion: EffectiveMotion,
    /// Each moving part and leaver where this frame drew it, and where the frame before did: what
    /// the next frame repaints.
    painted: Vec<Rect>,
    painted_before: Vec<Rect>,
}

impl Default for PartMotions {
    fn default() -> Self {
        Self {
            feel: Feel::Standard,
            rows: RowMotions::default().fading_out_leavers(),
            leavers: Vec::new(),
            pending: false,
            motion: EffectiveMotion::default(),
            painted: Vec::new(),
            painted_before: Vec::new(),
        }
    }
}

impl PartMotions {
    pub(super) fn set_feel(&mut self, feel: Feel) {
        self.feel = feel;
    }

    pub(super) const fn feel(&self) -> Feel {
        self.feel
    }

    /// The motion the theme allows (`theme`): what the next change moves with, at this container's
    /// pace.
    pub(super) fn set_motion(&mut self, theme: EffectiveMotion) {
        self.motion = self.feel.over(theme);
        if self.motion.reduced() && !self.at_rest() {
            self.clear();
        }
    }

    /// Before the parts change: note where each part (`key`, its place from the container's origin)
    /// stands, when the person's hand made the change. The first change of a frame notes them; the
    /// rest of that frame's changes join it.
    pub(super) fn before_change(&mut self, parts: impl Iterator<Item = (u64, Vec2)>) {
        if self.pending {
            return;
        }
        let now = crate::motion::frame_now();
        let Some(source) = RowMotions::<u64, Vec2>::caused_by_hand(now, self.motion.reduced())
        else {
            return;
        };
        self.pending = true;
        self.rows.capture(parts, true, Some(source));
    }

    /// A part (`key`, `element`, standing `at` from the container's origin) left with the change being
    /// made: it fades out where it stood when the change moves.
    pub(super) fn leave(&mut self, key: u64, element: Element, at: Vec2) {
        if !self.pending {
            return;
        }
        self.leavers.retain(|leaver| leaver.key != key);
        self.leavers.push(Leaver { key, element, at });
    }

    /// After the parts were laid out again: start the motion of the change noted before, from where
    /// each part was drawn to where it is laid out now (`shown`).
    pub(super) fn start(&mut self, shown: impl Iterator<Item = (u64, Vec2)>) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        let now = crate::motion::frame_now();
        self.rows.start(shown, self.motion, now);
        let leaving: Vec<u64> = self.rows.leaving(now).map(|(key, _, _)| *key).collect();
        self.leavers.retain(|leaver| leaving.contains(&leaver.key));
    }

    /// Forget every motion: the parts stand where they are laid out.
    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.leavers.clear();
        self.pending = false;
    }

    /// Whether anything is on its way at `now`.
    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.rows.is_moving(now)
    }

    /// Whether nothing moves or waits to, and the last frame drew nothing moving: the container
    /// draws as laid out.
    pub(super) fn at_rest(&self) -> bool {
        self.painted.is_empty() && self.leavers.is_empty() && !self.pending && self.rows.is_empty()
    }

    /// Whether the next frame draws something other than the last: a part on its way, or one the last
    /// frame drew on its way that now rests.
    pub(super) fn needs_redraw(&self, now: Instant) -> bool {
        !self.painted.is_empty() || self.is_moving(now)
    }

    /// How far from its laid-out place part `key` is drawn at `now`, and its opacity.
    pub(super) fn pose(&self, key: u64, now: Instant) -> (Vec2, f32) {
        self.rows.pose(&key, now)
    }

    /// The parts fading out at `now`: each one's index among the leavers, the place it stood from the
    /// container's origin, and its opacity.
    fn leaving(&self, now: Instant) -> Vec<(usize, Vec2, f32)> {
        self.rows
            .leaving(now)
            .filter_map(|(key, at, opacity)| {
                let index = self.leavers.iter().position(|leaver| leaver.key == *key)?;
                Some((index, at, opacity))
            })
            .collect()
    }

    /// The leavers at `now`, as drawn from a container whose origin is `origin`.
    pub(super) fn drawn_leavers(&self, origin: Vec2, now: Instant) -> Vec<DrawnPart> {
        self.leaving(now)
            .into_iter()
            .filter_map(|(index, at, opacity)| {
                let leaver = self.leavers.get(index)?;
                Some(DrawnPart {
                    rect: leaver_rect(leaver, origin, at),
                    opacity,
                    leaving: true,
                })
            })
            .collect()
    }

    /// Forget what has arrived and faded out.
    pub(super) fn settle(&mut self, now: Instant) {
        self.rows.settle(now);
        if self.leaving(now).is_empty() {
            self.leavers.clear();
        }
    }

    /// Paint the parts fading out, beneath the parts the container holds, from a container whose
    /// origin is `origin`.
    pub(super) fn draw_leavers(
        &mut self,
        origin: Vec2,
        now: Instant,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        env: &DrawEnv<'_>,
    ) {
        let leaving = self.leaving(now);
        for (index, at, opacity) in leaving {
            let Some(leaver) = self.leavers.get_mut(index) else {
                continue;
            };
            let rect = leaver_rect(leaver, origin, at);
            let by = rect.origin() - leaver.element.get_rect().origin();
            self.painted.push(rect);
            draw_posed(
                leaver.element.as_mut(),
                (by, opacity),
                scene,
                transform,
                text,
                theme,
                env,
            );
        }
    }

    /// Note that a part was drawn at `rect`, moving: the next frame repaints it there.
    pub(super) fn painted(&mut self, rect: Rect) {
        self.painted.push(rect);
    }

    /// A frame begins drawing.
    pub(super) fn begin_frame(&mut self) {
        self.painted_before = std::mem::take(&mut self.painted);
    }

    /// Where the last two frames drew the moving parts and the leavers.
    pub(super) fn last_painted(&self) -> impl Iterator<Item = &Rect> {
        self.painted.iter().chain(&self.painted_before)
    }

    /// The pose of every moving part at `now` for damage: each part's key and laid-out box.
    pub(super) fn moving_boxes(
        &self,
        parts: impl Iterator<Item = (u64, Rect)>,
        now: Instant,
    ) -> Vec<Rect> {
        parts
            .filter_map(|(key, rect)| {
                let (by, opacity) = self.pose(key, now);
                (by != Vec2::ZERO || opacity < 1.0).then(|| rect + by)
            })
            .collect()
    }

    pub(super) fn leaver_bounds(&self, origin: Vec2) -> Option<Rect> {
        self.leavers
            .iter()
            .map(|leaver| leaver_rect(leaver, origin, leaver.at))
            .reduce(|whole, rect| whole.union(rect))
    }
}

/// Where `leaver` is drawn from a container whose origin is `origin`, having stood `at` from it.
fn leaver_rect(leaver: &Leaver, origin: Vec2, at: Vec2) -> Rect {
    let size = leaver.element.get_rect().size();
    Rect::from_origin_size((origin + at).to_point(), size)
}

/// Draw `child` at `pose`: moved `by` from where it is laid out, at `opacity`.
pub(super) fn draw_posed(
    child: &mut dyn Widget,
    (by, opacity): (Vec2, f32),
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    env: &DrawEnv<'_>,
) {
    if opacity <= 0.0 {
        return;
    }
    let drawn = child.paint_bounds() + by;
    if env.should_cull(drawn) {
        return;
    }
    let faded = opacity < 1.0;
    if faded {
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            transform,
            // What a part paints past its box (a shadow, a focus ring) fades with it.
            &drawn.inflate(16.0, 16.0),
        );
    }
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo Affine composition is floating-point matrix math"
    )]
    let moved = transform * Affine::translate(by);
    crate::widget::draw_child_unculled(child, None, scene, moved, text, theme, env);
    if faded {
        scene.pop_layer();
    }
}

//! Faces: who is here, as a row of overlapping avatars, at every scale the
//! desktop shows people (the top bar, a window's bar, a tab, a card in the
//! overview). One painter so a person looks the same everywhere: their face
//! inside a ring in their colour, dimmed while idle or away, with a soft
//! breathing ring while they are in a call, and a drawn count badge for
//! everyone past the last face.

use vello::kurbo::{Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::paint::PaintCtx;
use crate::widget::AvatarFace;

/// One person's face in a stack.
#[derive(Clone, Debug, PartialEq)]
pub struct Face<'a> {
    pub name: &'a str,
    /// Their stable id: picks their picture and, without a colour, their hue.
    pub identity: &'a str,
    pub colour: Color,
    pub is_agent: bool,
    /// Idle or away: the face dims.
    pub dimmed: bool,
    /// In a voice channel: a soft ring breathes around the face, at this
    /// level (`ui::motion::Breathe`); `None` when not in a call.
    pub in_call: Option<f32>,
    /// A wave's bounce, as a scale (1 at rest; `ui::motion::Attention`).
    pub scale: f64,
}

impl<'a> Face<'a> {
    #[must_use]
    pub fn new(name: &'a str, identity: &'a str, colour: Color) -> Self {
        Self {
            name,
            identity,
            colour,
            is_agent: false,
            dimmed: false,
            in_call: None,
            scale: 1.0,
        }
    }
}

/// How a row of faces is sized: the face's side, how much each overlaps the
/// one before, and how many faces show before the count badge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceStack {
    pub side: f64,
    pub overlap: f64,
    pub max: usize,
}

/// The colour ring's width around a face.
const RING: f64 = 2.0;
/// The gap of ground between a face and its ring, and around the ring
/// where faces overlap.
const GAP: f64 = 1.0;
/// How far the call ring reaches past the colour ring.
const CALL_REACH: f64 = 3.0;

impl FaceStack {
    /// The top bar: up to four faces, 24 px, overlapping by 8.
    pub const BAR: Self = Self {
        side: 24.0,
        overlap: 8.0,
        max: 4,
    };
    /// A window's bar: up to three faces, 20 px.
    pub const WINDOW: Self = Self {
        side: 20.0,
        overlap: 6.0,
        max: 3,
    };
    /// A tab, or a card in the overview: up to three small faces.
    pub const SMALL: Self = Self {
        side: 16.0,
        overlap: 5.0,
        max: 3,
    };

    /// The faces shown of `count` people, and how many the badge counts.
    #[must_use]
    pub fn split(&self, count: usize) -> (usize, usize) {
        if count <= self.max {
            (count, 0)
        } else {
            // The badge takes the last face's place.
            let shown = self.max.saturating_sub(1);
            (shown, count - shown)
        }
    }

    /// How wide a stack of `count` people is, rings included.
    #[must_use]
    pub fn width(&self, count: usize) -> f64 {
        let (shown, more) = self.split(count);
        self.width_of(shown, more)
    }

    /// How wide `shown` faces and a badge counting `more` are.
    #[must_use]
    pub fn width_of(&self, shown: usize, more: usize) -> f64 {
        let slots = shown + usize::from(more > 0);
        if slots == 0 {
            return 0.0;
        }
        let outer = self.side + 2.0 * (RING + GAP);
        outer + (slots - 1) as f64 * (outer - self.overlap)
    }

    /// Paint `faces` (the ones shown, [`Self::split`]) and a badge counting
    /// `more` people past them, ending at `right`, centred on `center_y`,
    /// over `ground`. Returns each face's rect (for hit-testing, by index
    /// into `faces`) and the badge's, if any.
    pub fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        (right, center_y): (f64, f64),
        (faces, more): (&[Face<'_>], usize),
        ground: Color,
    ) -> (Vec<Rect>, Option<Rect>) {
        let shown = faces.len();
        let slots = shown + usize::from(more > 0);
        let outer = self.side + 2.0 * (RING + GAP);
        let step = outer - self.overlap;
        let left = right - self.width_of(shown, more);
        let center_of = |slot: usize| Point::new(left + outer / 2.0 + slot as f64 * step, center_y);
        let mut rects = Vec::with_capacity(shown);
        for (slot, face) in faces.iter().take(shown).enumerate() {
            let center = center_of(slot);
            let side = self.side * face.scale;
            let rect = Rect::from_center_size(center, (side, side));
            paint_face(ctx, rect, face, ground);
            rects.push(rect);
        }
        let badge = (more > 0).then(|| {
            let rect = Rect::from_center_size(center_of(slots - 1), (self.side, self.side));
            AvatarFace::paint_overflow(ctx, rect, more, Some(ground));
            rect
        });
        (rects, badge)
    }
}

impl Face<'_> {
    /// The gap of ground between a face's disc and its colour ring.
    pub const GROUND_GAP: f64 = GAP;
    /// How far a face's ground reaches past its disc: the gap, the colour
    /// ring and the gap around it (the call ring, when shown, reaches further).
    pub const REACH: f64 = GAP + RING + GAP;

    /// The disc of a face whose ring and gap fill `outer` exactly: where a
    /// caller with a fixed cell (a column at a screen edge, a list row)
    /// paints [`Self::paint`] so nothing reaches past the cell.
    #[must_use]
    pub fn disc_in(outer: Rect) -> Rect {
        outer.inset(-(RING + GAP))
    }

    /// Paint this face alone in `rect` over `ground`: the call ring, the
    /// ground gap, the colour ring and the face itself, quietened while idle.
    /// Where a caller lays faces out itself (a column at a screen edge, a
    /// list row), so a person still looks as they do in every stack.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, ground: Color) {
        paint_face(ctx, rect, self, ground);
    }
}

/// One face in `rect`: the call ring, the ground gap, the colour ring and
/// the face. Idle or away, its colour is quietened against `ground`
/// ([`crate::tokens::colors::PresencePalette::idle_on`]) rather than faded
/// through, so the ring and the initial still read on any bar in any theme.
fn paint_face(ctx: &mut PaintCtx<'_>, rect: Rect, face: &Face<'_>, ground: Color) {
    let center = rect.center();
    let r = rect.width() / 2.0;
    let palette = &ctx.theme.tokens.colors.presence;
    let colour = if face.dimmed {
        palette.idle_on(face.colour, ground)
    } else {
        palette.ring_on(face.colour, ground)
    };
    let transform = ctx.transform;
    if let Some(level) = face.in_call {
        ctx.scene.stroke(
            &Stroke::new(2.0),
            transform,
            colour.multiply_alpha(0.55 * level),
            None,
            &Circle::new(center, r + GAP + RING + CALL_REACH - 1.0),
        );
    }
    // Ground under the ring, so overlapping faces separate.
    ctx.scene.fill(
        Fill::NonZero,
        transform,
        ground,
        None,
        &Circle::new(center, r + GAP + RING + GAP),
    );
    ctx.scene.stroke(
        &Stroke::new(RING),
        transform,
        colour,
        None,
        &Circle::new(center, r + GAP + RING / 2.0),
    );
    if face.is_agent {
        crate::agent_identity::draw_agent_identity_mark(
            ctx.scene,
            transform,
            rect,
            colour,
            ctx.theme,
            crate::agent_identity::AgentIdentityStyle::Solid,
        );
    } else {
        // One initial: in a stack the next face covers the edge of this one.
        let first = face.name.split_whitespace().next().unwrap_or(face.name);
        let avatar = AvatarFace::new(first, face.identity).presence(face.colour);
        let avatar = if face.dimmed {
            avatar.idle_on(ground)
        } else {
            avatar
        };
        avatar.paint(ctx, rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Up to the most faces show; past it the badge takes the last face's
    /// place and counts everyone it stands for.
    #[test]
    fn a_stack_shows_its_faces_then_counts_the_rest() {
        let bar = FaceStack::BAR;
        assert_eq!(bar.split(0), (0, 0));
        assert_eq!(bar.split(4), (4, 0));
        assert_eq!(bar.split(5), (3, 2));
        assert_eq!(bar.split(16), (3, 13));
        assert_eq!(bar.width(0), 0.0);
        let one = bar.width(1);
        assert!(
            (bar.width(2) - one - (one - bar.overlap)).abs() < 1e-9,
            "each face overlaps by 8"
        );
        assert_eq!(
            bar.width(4),
            bar.width(16),
            "the badge takes the fourth place"
        );
    }

    /// A dimmed face paints fainter than a present one; a call adds a ring
    /// past the colour ring.
    #[test]
    fn idle_faces_dim_and_a_call_rings_them() {
        use crate::offscreen::OffscreenRenderer;
        let theme = crate::theme::Theme::default();
        let render = |face: Face<'_>| {
            let mut scene = vello::Scene::new();
            let mut text = crate::text_render::TextRenderer::new();
            let mut ctx = PaintCtx::new(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            );
            FaceStack::BAR.paint(&mut ctx, (60.0, 30.0), (&[face], 0), Color::BLACK);
            OffscreenRenderer::new(80, 60).render(&scene, Color::BLACK)
        };
        let blue = Color::from_rgb8(0x25, 0x63, 0xeb);
        let here = render(Face::new("Ada", "ada", blue));
        let mut idle = Face::new("Ada", "ada", blue);
        idle.dimmed = true;
        let idle = render(idle);
        // On the colour ring, left of the face's centre.
        let width = FaceStack::BAR.width(1);
        let center_x = 60.0 - width / 2.0;
        let ring_x = (center_x - 12.0 - GAP - RING / 2.0) as u32;
        let lum = |rgba: &[u8]| {
            let (r, g, b, _) = OffscreenRenderer::sample(rgba, 80, ring_x, 30);
            u32::from(r) + u32::from(g) + u32::from(b)
        };
        assert!(lum(&idle) < lum(&here), "an idle face is fainter");

        let mut calling = Face::new("Ada", "ada", blue);
        calling.in_call = Some(1.0);
        let calling = render(calling);
        let call_x = (center_x - 12.0 - GAP - RING - CALL_REACH + 1.0) as u32;
        let at = |rgba: &[u8]| {
            let (r, g, b, _) = OffscreenRenderer::sample(rgba, 80, call_x, 30);
            u32::from(r) + u32::from(g) + u32::from(b)
        };
        assert!(at(&calling) > at(&here), "someone in a call is ringed");
    }

    /// Every presence hue's face marks against the top bar it sits on, in
    /// dark and light, present or idle: its colour ring as painted clears
    /// non-text contrast (3:1) on the bar's ground, and its initial clears
    /// on-fill contrast on the face. An idle face quietens and never
    /// vanishes into the bar.
    #[test]
    fn every_hue_marks_on_the_bar_in_dark_and_light_present_or_idle() {
        use crate::color::contrast_ratio;
        use crate::offscreen::OffscreenRenderer;
        use crate::tokens::colors::{NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST, PresenceHue};
        for (label, theme) in [
            ("dark", crate::theme::Theme::dark()),
            ("light", crate::theme::Theme::light()),
        ] {
            let bar = theme.tokens.colors.surfaces.base;
            let palette = theme.tokens.colors.presence;
            for hue in PresenceHue::ALL {
                let identity = palette.hue(hue);
                for dimmed in [false, true] {
                    let mut face = Face::new("Ben Carter", "ben", identity);
                    face.dimmed = dimmed;
                    let mut scene = vello::Scene::new();
                    let mut text = crate::text_render::TextRenderer::new();
                    let mut ctx = PaintCtx::new(
                        &mut scene,
                        vello::kurbo::Affine::IDENTITY,
                        &mut text,
                        &theme,
                    );
                    FaceStack::BAR.paint(&mut ctx, (60.0, 30.0), (&[face], 0), bar);
                    let rgba = OffscreenRenderer::new(80, 60).render(&scene, bar);
                    let center_x = 60.0 - FaceStack::BAR.width(1) / 2.0;
                    let ring_x = (center_x - 12.0 - GAP - RING / 2.0) as u32;
                    let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 80, ring_x, 30);
                    let ring = Color::from_rgb8(r, g, b);
                    let ratio = contrast_ratio(ring, bar);
                    assert!(
                        ratio >= NON_TEXT_MIN_CONTRAST - 0.05,
                        "{label} {hue:?} dimmed={dimmed}: ring {ratio:.2}:1 on the bar"
                    );

                    let avatar = AvatarFace::new("Ben", "ben").presence(identity);
                    let avatar = if dimmed { avatar.idle_on(bar) } else { avatar };
                    let fill = avatar.colour(&theme);
                    let ink = crate::widget::avatar::initial_ink(fill, &theme);
                    let ratio = contrast_ratio(ink, fill);
                    assert!(
                        ratio >= ON_FILL_MIN_CONTRAST,
                        "{label} {hue:?} dimmed={dimmed}: initial {ratio:.2}:1 on the face"
                    );
                }
            }
        }
    }
}

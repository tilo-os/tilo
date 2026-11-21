//! A tab pill's faces: the people a view gives it or the presence feed says are on it, how many
//! of them fit beside the title, and how each is drawn.

use vello::kurbo::{Rect, RoundedRect};

use crate::paint::PaintCtx;
use crate::theme::Theme;

/// One face a tab's view gave its pill: a person or an agent, the name they are known by here and
/// the hue they wear. A pill shows these when its view gave any, and the live presence feed's
/// people otherwise.
#[derive(Clone, Debug, PartialEq)]
pub struct TabFaceSpec {
    pub name: String,
    pub hue: crate::tokens::PresenceHue,
    pub agent: bool,
}

/// Peer faces on a pill: at most this many faces before a "+N" disc.
pub(super) const PILL_MAX_FACES: usize = 2;

/// Peer faces a pill shows: `faces` initials, then a "+`overflow`" disc for everyone else (none
/// when zero).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PillFaces {
    pub(super) faces: usize,
    pub(super) overflow: usize,
}

/// Sizes of a pill's peer faces, from the theme: each face is a medium icon wide (large enough
/// for a legible initial), keeps the smallest spacing step from the title, and overlaps its
/// neighbour by only half that step so both initials stay whole.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct FaceMetrics {
    pub(super) face: f64,
    pub(super) gap: f64,
    pub(super) overlap: f64,
}

impl FaceMetrics {
    pub(super) fn of(theme: &Theme) -> Self {
        let gap = f64::from(theme.tokens.spacing.s1);
        Self {
            face: f64::from(theme.tokens.metrics.icon_md),
            gap,
            // The ring alone overlaps the next face onto this one's edge, never onto its
            // initial.
            overlap: 0.0,
        }
    }

    pub(super) fn step(self) -> f64 {
        self.face - self.overlap
    }
}

impl PillFaces {
    pub(super) fn slots(self) -> usize {
        self.faces.saturating_add(usize::from(self.overflow > 0))
    }

    pub(super) fn width(self, metrics: FaceMetrics) -> f64 {
        match self.slots() {
            0 => 0.0,
            parts => {
                metrics.face
                    + crate::num::f64_of_usize(parts.saturating_sub(1)) * metrics.step()
                    + metrics.gap
            }
        }
    }

    /// The most faces for `peers` that still leave the title `keep` px of a `leftover`-wide
    /// zone; fewer faces fold into "+N".
    pub(super) fn fit(peers: usize, leftover: f64, keep: f64, metrics: FaceMetrics) -> Self {
        (1..=peers.min(PILL_MAX_FACES))
            .rev()
            .map(|faces| Self {
                faces,
                overflow: peers.saturating_sub(faces),
            })
            .find(|candidate| leftover - candidate.width(metrics) >= keep)
            .unwrap_or_default()
    }
}

/// One face on a pill: a person's initials in their colour, or an agent's mark, ringed so it
/// stands off whatever is under it.
pub(super) fn paint_tab_face(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    name: &str,
    colour: vello::peniko::Color,
    agent: bool,
    ring: vello::peniko::Color,
) {
    if agent {
        let theme = ctx.theme;
        let ring_px = f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0;
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            ring,
            None,
            &RoundedRect::from_rect(
                rect.inflate(ring_px, ring_px),
                f64::from(theme.tokens.radii.detail),
            ),
        );
        crate::agent_identity::draw_agent_identity_mark(
            ctx.scene,
            ctx.transform,
            rect,
            colour,
            theme,
            crate::agent_identity::AgentIdentityStyle::Solid,
        );
    } else {
        crate::widget::AvatarFace::new(name, "")
            .presence(colour)
            .ringed_on(ring)
            .paint(ctx, rect);
    }
}

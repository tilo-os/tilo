//! A tab pill's faces: the people a view gives it or the presence feed says are on it, how many
//! of them fit beside the title, and how each is drawn.

use vello::kurbo::{Rect, RoundedRect};

use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::theme::Theme;

/// One face a tab's view gave its pill: a person or an agent, the name they are known by here and
/// the hue they wear. A pill shows these when its view gave any, and the live presence feed's
/// people otherwise.
#[derive(Clone, Debug, PartialEq)]
pub struct TabFaceSpec {
    pub name: String,
    pub hue: tilo_ui_base::tokens::PresenceHue,
    pub agent: bool,
    /// The colour the face wears where its view gave one (`#rrggbb`), over the hue's.
    pub rgb: Option<(u8, u8, u8)>,
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
                    + tilo_ui_base::num::f64_of_usize(parts.saturating_sub(1)) * metrics.step()
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

/// One face on a pill: initials on a disc in the member's toned colour, ringed so it stands off
/// whatever is under it. An agent wears the same face as a person does, so it reads as who opened
/// or is at the window rather than as an app's icon, with the small agent mark on its corner to
/// say it is software.
pub(super) fn paint_tab_face(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    name: &str,
    colour: vello::peniko::Color,
    agent: bool,
    ring: vello::peniko::Color,
) {
    let face = tilo_ui_base::widget::AvatarFace::new(name, "")
        .presence(colour)
        .ringed_on(ring);
    face.paint(ctx, rect);
    if !agent {
        return;
    }
    let theme = ctx.theme;
    let badge = face.status_rect(rect, theme);
    // The ring a face's status badge stands on, so the mark reads apart from the face it sits on.
    let ring_px = f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0;
    let outer = badge.inflate(ring_px, ring_px);
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ring,
        None,
        &RoundedRect::from_rect(outer, outer.width() / 2.0),
    );
    tilo_ui_base::agent_identity::draw_agent_identity_mark(
        ctx.scene,
        ctx.transform,
        badge,
        face.colour(theme),
        theme,
        tilo_ui_base::agent_identity::AgentIdentityStyle::Solid,
    );
}

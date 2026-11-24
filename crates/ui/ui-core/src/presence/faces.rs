//! Faces: who is here, as a row of overlapping avatars, at every scale the
//! shell shows people (the top bar, a window's bar, a row, a card). One
//! painter so a person looks the same everywhere: their face inside a ring in
//! their colour, dimmed while idle or away, with a bright arc sweeping round
//! the ring while they are active now (in a call, or an agent at work), and a
//! drawn count badge for everyone past the last face.

use vello::kurbo::{Cap, Circle, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill};

use crate::paint::PaintCtx;
use crate::widget::AvatarFace;

/// One member's face in a stack.
#[derive(Clone, Copy, Debug)]
pub struct Face<'a> {
    /// The face itself, as every surface draws this member ([`AvatarFace::of`]).
    pub avatar: AvatarFace<'a>,
    /// The ring's colour, and the face's.
    pub colour: Color,
    /// Idle or away: the face dims.
    pub dimmed: bool,
    /// Active now, in a voice channel or an agent at work: an arc sweeps round the ring
    /// ([`crate::motion::Sweep`]); `None` otherwise.
    pub active: Option<crate::motion::Sweep>,
    /// A wave's bounce, as a scale (1 at rest; `ui::motion::Attention`).
    pub scale: f64,
    /// They follow the view of the person looking: a small eye on the face's lower edge, in
    /// their colour, says so where their face already stands, with nothing to read or answer.
    pub follows_you: bool,
}

impl<'a> Face<'a> {
    #[must_use]
    pub fn new(name: &'a str, identity: &'a str, colour: Color) -> Self {
        Self::wearing(AvatarFace::new(name, identity), colour)
    }

    /// `who`'s face ([`AvatarFace::of`]), ringed in `colour`.
    #[must_use]
    pub fn of(who: &'a crate::component::Person, colour: Color) -> Self {
        Self::wearing(AvatarFace::of(who), colour)
    }

    /// `member`'s face ([`AvatarFace::member`]), called `name`, ringed in `colour`.
    #[must_use]
    pub fn member(name: &'a str, member: &'a tilo_actions::MemberId, colour: Color) -> Self {
        Self::wearing(AvatarFace::member(name, member), colour)
    }

    /// The face a search row states for its member ([`crate::search::RowFace`]): the same
    /// face and colour, dimmed while they are away.
    #[must_use]
    pub fn of_row(row: &'a crate::search::RowFace) -> Self {
        Self {
            avatar: row.avatar(),
            colour: row.colour,
            dimmed: row.dimmed,
            active: None,
            scale: 1.0,
            follows_you: false,
        }
    }

    /// Software, not a person: the agent's mark ([`AvatarFace::agent`]), for a stack that knows
    /// only whether its member is one.
    #[must_use]
    pub fn agent(mut self) -> Self {
        self.avatar = self.avatar.agent();
        self
    }

    fn wearing(avatar: AvatarFace<'a>, colour: Color) -> Self {
        Self {
            avatar: avatar.presence(colour),
            colour,
            dimmed: false,
            active: None,
            scale: 1.0,
            follows_you: false,
        }
    }
}

/// The ring an active face wears: the track on `track`, held at [`TRACK`], under an arc of
/// [`ARC`] sweeping round `arc` — or, where motion is reduced, the whole of `arc` lit and still.
/// Every face that shows someone active now (in a call, an agent at work), in a stack or alone,
/// wears this one ring.
pub(crate) fn paint_active_ring<S: Shape>(
    ctx: &mut PaintCtx<'_>,
    colour: Color,
    track: &S,
    track_width: f64,
    arc: &S,
    arc_width: f64,
    sweep: crate::motion::Sweep,
) {
    let transform = ctx.transform;
    match sweep {
        crate::motion::Sweep::Still => {
            ctx.scene
                .stroke(&Stroke::new(arc_width), transform, colour, None, arc);
        }
        crate::motion::Sweep::Round(turn) => {
            ctx.scene.stroke(
                &Stroke::new(track_width),
                transform,
                colour.multiply_alpha(TRACK),
                None,
                track,
            );
            let length = arc.perimeter(0.1);
            let lit = length * ARC;
            ctx.scene.stroke(
                &Stroke::new(arc_width)
                    .with_caps(Cap::Round)
                    .with_dashes(-f64::from(turn) * length, [lit, length - lit]),
                transform,
                colour,
                None,
                arc,
            );
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
/// How much of an active face's ring its sweeping arc is.
const ARC: f64 = 0.3;
/// How strong the rest of an active face's ring stays under its arc.
const TRACK: f32 = 0.35;
/// How far an agent's rounded square is drawn inside its face's place, so it covers as much of the
/// bar as a person's disc: a square of the disc's side is a quarter larger than the disc.
const SQUARE_INSET: f64 = 0.05;
/// The widest a face's initials stand, as a share of the face's side: two
/// letters at [`initials_style`](crate::widget::avatar)'s size, a little wide.
const INITIALS_SPAN: f64 = 0.6;
/// The ground a stacked face keeps between its neighbour's initials and the
/// ring of the face drawn over it, so both letters stay whole.
const INITIALS_CLEARANCE: f64 = 1.0;

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

    /// The faces on a control's own face `height` high (a bar's group of who is
    /// here): the ring and the ground around it fit within the control, so no face is cut by
    /// the control's edge.
    #[must_use]
    pub fn in_face(height: f64) -> Self {
        let side = (height - 2.0 * (GAP + RING + GAP)).max(1.0);
        Self {
            side,
            overlap: side / 3.0,
            max: 3,
        }
    }

    /// The faces shown of `count` people, and the badge past them counting the
    /// rest: up to three faces, then a "+N" face of its own (four people read as three faces
    /// and "+1", never as a face counting four).
    #[must_use]
    pub fn group(&self, count: usize) -> (usize, usize) {
        let shown = count.min(self.max);
        (shown, count - shown)
    }

    /// How wide the group of `count` people is ([`Self::group`]), rings included.
    #[must_use]
    pub fn group_width(&self, count: usize) -> f64 {
        let (shown, more) = self.group(count);
        self.width_of(shown, more)
    }

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

    /// How far apart two faces stand: their [`Self::overlap`], but never so
    /// close that the face drawn over covers the one under past its ring and
    /// gap. Each face's initials are centred in its disc, so the ground of
    /// the next face must start past them; otherwise the letters of the
    /// earlier face are cut in half ("CO" reading as "C") and a stack of
    /// people stops naming them.
    #[must_use]
    pub fn step(&self) -> f64 {
        let outer = self.side + 2.0 * (RING + GAP);
        let past_initials = Face::REACH + self.side / 2.0 + self.side * INITIALS_SPAN / 2.0;
        (outer - self.overlap).max(past_initials + INITIALS_CLEARANCE)
    }

    /// How wide `shown` faces and a badge counting `more` are.
    #[must_use]
    pub fn width_of(&self, shown: usize, more: usize) -> f64 {
        let slots = shown + usize::from(more > 0);
        if slots == 0 {
            return 0.0;
        }
        let outer = self.side + 2.0 * (RING + GAP);
        outer + (slots - 1) as f64 * self.step()
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
        let (rects, badge) = self.layout((right, center_y), (faces, more));
        for (face, rect) in faces.iter().zip(&rects) {
            paint_face(ctx, *rect, face, ground);
        }
        if let Some(rect) = badge {
            AvatarFace::paint_overflow(ctx, rect, more, Some(ground));
        }
        (rects, badge)
    }

    /// Where [`Self::paint`] stands each of `faces` and the badge counting
    /// `more`, ending at `right` and centred on `center_y`, without painting:
    /// what a host places its targets by before anything is painted.
    #[must_use]
    pub fn layout(
        &self,
        (right, center_y): (f64, f64),
        (faces, more): (&[Face<'_>], usize),
    ) -> (Vec<Rect>, Option<Rect>) {
        let shown = faces.len();
        let slots = shown + usize::from(more > 0);
        let outer = self.side + 2.0 * (RING + GAP);
        let step = self.step();
        let left = right - self.width_of(shown, more);
        let center_of = |slot: usize| Point::new(left + outer / 2.0 + slot as f64 * step, center_y);
        let rects = faces
            .iter()
            .enumerate()
            .map(|(slot, face)| {
                let side = self.side * face.scale;
                Rect::from_center_size(center_of(slot), (side, side))
            })
            .collect();
        let badge = (more > 0)
            .then(|| Rect::from_center_size(center_of(slots - 1), (self.side, self.side)));
        (rects, badge)
    }
}

impl Face<'_> {
    /// How far a face's ground reaches past its disc: the gap, the colour
    /// ring and the gap around it.
    pub const REACH: f64 = GAP + RING + GAP;

    /// The disc of a face whose ring and gap fill `outer` exactly: where a
    /// caller with a fixed cell (a column at a screen edge, a list row)
    /// paints [`Self::paint`] so nothing reaches past the cell.
    #[must_use]
    pub fn disc_in(outer: Rect) -> Rect {
        outer.inset(-(RING + GAP))
    }

    /// Paint this face alone in `rect` over `ground`: the ground gap, the colour
    /// ring (swept while active) and the face itself, quietened while idle.
    /// Where a caller lays faces out itself (a column at a screen edge, a
    /// list row), so a person still looks as they do in every stack.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, ground: Color) {
        paint_face(ctx, rect, self, ground);
    }
}

/// The shape a face's rings follow, `reach` past its disc: a circle for a person, and for an agent
/// the rounded square its mark is, so software never reads as a person at any scale.
fn outline(face: &Face<'_>, rect: Rect, corner: f64, reach: f64) -> vello::kurbo::BezPath {
    if face.avatar.is_square() {
        RoundedRect::from_rect(rect.inflate(reach, reach), corner + reach).to_path(0.1)
    } else {
        Circle::new(rect.center(), rect.width() / 2.0 + reach).to_path(0.1)
    }
}

/// One face in `rect`: the ground gap, the colour ring and the face. Idle or away, its colour is
/// quietened against `ground` ([`crate::tokens::colors::PresencePalette::idle_on`]) rather than
/// faded through, so the ring and the initial still read on any bar in any theme. Active, the ring
/// widens to the ground's edge and a full-strength arc sweeps round it over the rest of it held
/// quiet: within the face's own place, so a bar no taller than its faces still shows it whole.
fn paint_face(ctx: &mut PaintCtx<'_>, rect: Rect, face: &Face<'_>, ground: Color) {
    let rect = if face.avatar.is_square() {
        rect.inset(-rect.width() * SQUARE_INSET)
    } else {
        rect
    };
    // The agent mark's own corner (`agent_identity`), which its rings keep.
    let corner = (rect.width() * 0.22).min(f64::from(ctx.theme.tokens.radii.detail));
    let around = |reach: f64| outline(face, rect, corner, reach);
    let palette = &ctx.theme.tokens.colors.presence;
    let colour = if face.dimmed {
        palette.idle_on(face.colour, ground)
    } else {
        palette.ring_on(face.colour, ground)
    };
    let transform = ctx.transform;
    // Ground under the ring, so overlapping faces separate.
    ctx.scene.fill(
        Fill::NonZero,
        transform,
        ground,
        None,
        &around(GAP + RING + GAP),
    );
    let ring = around(GAP + RING / 2.0);
    let band = around(GAP + (RING + GAP) / 2.0);
    match face.active {
        None => ctx
            .scene
            .stroke(&Stroke::new(RING), transform, colour, None, &ring),
        Some(sweep) => paint_active_ring(ctx, colour, &ring, RING, &band, RING + GAP, sweep),
    }
    // The same face as everywhere this member is shown, so nobody reads as "ED" to themselves
    // and "E" to everyone else, and an agent wears its mark on the bar as in chat.
    let avatar = if face.dimmed {
        face.avatar.idle_on(ground)
    } else {
        face.avatar
    };
    avatar.paint(ctx, rect);
    if face.follows_you {
        paint_follows_you(ctx, rect, colour, ground);
    }
}

/// How wide the eye that says a face follows the person looking is, against the face's side.
const FOLLOWS_YOU: f64 = 0.5;

/// The eye on the lower right of the face in `rect`: a disc in the face's `colour`, separated from
/// it by `ground`, with the eye drawn in `ground` on it.
fn paint_follows_you(ctx: &mut PaintCtx<'_>, rect: Rect, colour: Color, ground: Color) {
    let radius = rect.width() * FOLLOWS_YOU / 2.0;
    let center = Point::new(rect.x1 - radius / 2.0, rect.y1 - radius / 2.0);
    let transform = ctx.transform;
    ctx.scene.fill(
        Fill::NonZero,
        transform,
        ground,
        None,
        &Circle::new(center, radius + GAP),
    );
    ctx.scene.fill(
        Fill::NonZero,
        transform,
        colour,
        None,
        &Circle::new(center, radius),
    );
    let glyph = Rect::from_center_size(center, (radius * 1.5, radius * 1.5));
    crate::icons::Icon::Eye.draw(ctx.scene, transform, glyph, ground);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint `faces` as the bar's stack over `ground` under `theme`, on an 80 by 60 picture.
    fn render(theme: &crate::theme::Theme, faces: &[Face<'_>], ground: Color) -> Vec<u8> {
        let mut scene = vello::Scene::new();
        let mut text = crate::text_render::TextRenderer::new();
        let mut ctx = PaintCtx::new(&mut scene, vello::kurbo::Affine::IDENTITY, &mut text, theme);
        FaceStack::BAR.paint(&mut ctx, (60.0, 30.0), (faces, 0), ground);
        crate::offscreen::OffscreenRenderer::new(80, 60).render(&scene, ground)
    }

    /// A face in a stack wears the same initials as the person's own
    /// avatar: two visitors who share a first word ("Eager Dolphin",
    /// "Eager Badger") paint apart, and the letters are the first and last
    /// word's, as `initials_for_name` picks them.
    #[test]
    fn stacked_faces_wear_the_full_initials() {
        let theme = crate::theme::Theme::default();
        let blue = Color::from_rgb8(0x25, 0x63, 0xeb);
        let render = |name: &str| render(&theme, &[Face::new(name, "same-id", blue)], Color::BLACK);
        assert_ne!(
            render("Eager Dolphin"),
            render("Eager Badger"),
            "the second initial tells two visitors with one first word apart"
        );
        assert_eq!(
            render("Eager Dolphin"),
            render("Eager Brave Dolphin"),
            "first and last word"
        );
    }

    /// Every presence hue's face marks against the top bar it sits on, in dark and light: its
    /// colour ring clears non-text contrast (3:1) on the bar's ground, quietened and never vanishing
    /// while idle, and its initial clears on-fill contrast on the face. Active now (in a call, an
    /// agent at work), an arc of the ring, wider than the ring at rest, marks at full strength and
    /// sweeps round over the rest held quiet, all within the face's own place; where motion is
    /// reduced, the whole wide ring, still.
    #[test]
    fn every_hue_marks_on_the_bar_idle_or_active() {
        use crate::color::contrast_ratio;
        use crate::motion::Sweep;
        use crate::offscreen::OffscreenRenderer;
        use crate::tokens::colors::{NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST, PresenceHue};
        let center_x = 60.0 - FaceStack::BAR.width(1) / 2.0;
        // The ring at rest, the band past it the active ring widens over, and the band's middle.
        let (ring, outside) = (12.0 + GAP + RING / 2.0, 12.0 + GAP + RING + GAP / 2.0);
        let middle = 12.0 + GAP + (RING + GAP) / 2.0;
        let angles: Vec<f64> = (0..36).map(|i| f64::from(i) * 10f64.to_radians()).collect();
        for (label, theme) in [
            ("dark", crate::theme::Theme::dark()),
            ("light", crate::theme::Theme::light()),
        ] {
            let bar = theme.tokens.colors.surfaces.base;
            // How the pixel whose middle is nearest `radius` out at `angle` stands on the bar.
            let at = |rgba: &[u8], radius: f64, angle: f64| {
                let (x, y) = (center_x + radius * angle.cos(), 30.0 + radius * angle.sin());
                let (r, g, b, _) = OffscreenRenderer::sample(rgba, 80, x as u32, y as u32);
                contrast_ratio(Color::from_rgb8(r, g, b), bar)
            };
            let marks = |rgba: &[u8], radius: f64, angle: f64| {
                at(rgba, radius, angle) >= NON_TEXT_MIN_CONTRAST - 0.05
            };
            for hue in PresenceHue::ALL {
                let colour = theme.tokens.colors.presence.hue(hue);
                let face = |dimmed, active| Face {
                    dimmed,
                    active,
                    ..Face::new("Ben Carter", "ben", colour)
                };
                let (here, idle) = (
                    render(&theme, &[face(false, None)], bar),
                    render(&theme, &[face(true, None)], bar),
                );
                let left = std::f64::consts::PI;
                assert!(
                    marks(&here, ring, left) && marks(&idle, ring, left),
                    "{label} {hue:?}: ring"
                );
                // On the dark bar an idle face is also fainter.
                assert!(
                    label == "light" || at(&idle, ring, left) < at(&here, ring, left),
                    "{label} {hue:?}: idle is fainter"
                );
                for dimmed in [false, true] {
                    let avatar = AvatarFace::new("Ben", "ben").presence(colour);
                    let fill = if dimmed { avatar.idle_on(bar) } else { avatar }.colour(&theme);
                    let ratio =
                        contrast_ratio(crate::widget::avatar::initial_ink(fill, &theme), fill);
                    assert!(
                        ratio >= ON_FILL_MIN_CONTRAST,
                        "{label} {hue:?} dimmed={dimmed}: {ratio:.2}:1"
                    );
                }

                assert!(
                    [0.0, 0.5, 1.0, 1.5]
                        .iter()
                        .all(|&turns| at(&here, outside, turns * left) < 1.1),
                    "{label} {hue:?}: at rest the ring stays narrow"
                );
                let still = render(&theme, &[face(false, Some(Sweep::Still))], bar);
                assert!(
                    angles.iter().all(|&a| marks(&still, middle, a)),
                    "{label} {hue:?}: reduced motion lights the whole wide ring"
                );
                let lit = |turn: f32| {
                    let swept = render(&theme, &[face(false, Some(Sweep::Round(turn)))], bar);
                    angles
                        .iter()
                        .map(|&a| marks(&swept, middle, a))
                        .collect::<Vec<bool>>()
                };
                let (start, later) = (lit(0.0), lit(0.5));
                let count = start.iter().filter(|lit| **lit).count();
                assert!(
                    (6..=16).contains(&count),
                    "{label} {hue:?}: an arc, {count} of 36 lit"
                );
                assert_ne!(start, later, "{label} {hue:?}: the arc moves round");
                // Nothing reaches past the face's place: the bar is no taller than its faces.
                let above = (30.0 - 12.0 - Face::REACH - 1.0) as u32;
                let (r, g, b, _) = OffscreenRenderer::sample(&still, 80, center_x as u32, above);
                assert_eq!(
                    Color::from_rgb8(r, g, b).to_rgba8(),
                    bar.to_rgba8(),
                    "{label}"
                );
            }
        }
    }

    /// An agent's rounded square keeps its shape but weighs what a person's disc does: in the
    /// same colour it covers about as much of the bar as the disc.
    #[test]
    fn an_agent_face_weighs_what_a_person_face_does() {
        let theme = crate::theme::Theme::dark();
        let bar = theme.tokens.colors.surfaces.base;
        let colour = theme
            .tokens
            .colors
            .presence
            .hue(crate::tokens::colors::PresenceHue::ALL[0]);
        let ground = bar.to_rgba8();
        let covered = |face: Face<'_>| {
            render(&theme, &[face], bar)
                .chunks_exact(4)
                .filter(|px| (px[0], px[1], px[2]) != (ground.r, ground.g, ground.b))
                .count()
        };
        let person = covered(Face::new("Ben Carter", "ben", colour));
        let agent = covered(Face::new("Scribe", "scribe", colour).agent());
        assert!(
            agent * 100 <= person * 105 && agent * 100 >= person * 90,
            "an agent covers {agent} px, a person {person} px"
        );
    }

    tilo_test_env::scenario! {
        fn a_face_stack_counts_overlaps_and_keeps_its_initials;

        phase a_stack_shows_its_faces_then_counts_the_rest;
        // Up to the most faces show; past it the badge takes the last face's
        // place and counts everyone it stands for.
        {
            let bar = FaceStack::BAR;
            assert_eq!(bar.split(0), (0, 0));
            assert_eq!(bar.split(4), (4, 0));
            assert_eq!(bar.split(5), (3, 2));
            assert_eq!(bar.split(16), (3, 13));
            assert_eq!(bar.width(0), 0.0);
            let one = bar.width(1);
            assert!(
                (bar.width(2) - one - bar.step()).abs() < 1e-9,
                "each face stands one step past the one before"
            );
            assert!(bar.step() < one, "neighbours overlap");
            assert_eq!(
                bar.width(4),
                bar.width(16),
                "the badge takes the fourth place"
            );
        }

        phase a_group_shows_three_faces_then_counts_the_rest;
        // A group on a control's face shows three faces and a badge past them: four people are
        // three faces and "+1", and the faces stay inside the control they stand on.
        {
            let in_face = FaceStack::in_face(24.0);
            assert_eq!(in_face.group(0), (0, 0));
            assert_eq!(in_face.group(1), (1, 0));
            assert_eq!(in_face.group(3), (3, 0));
            assert_eq!(in_face.group(4), (3, 1));
            assert_eq!(in_face.group(9), (3, 6));
            assert_eq!(
                in_face.group_width(9),
                in_face.width_of(3, 6),
                "the badge is a face of its own past the three"
            );
            assert!(in_face.group_width(9) > in_face.group_width(3));
            // The ring and the ground around it stand within the face it sits on.
            assert!(
                in_face.side / 2.0 + Face::REACH <= 24.0 / 2.0,
                "a face fits the control it stands on"
            );
        }

        phase a_stacked_face_keeps_its_initials_whole;
        // A face in a stack keeps both its initials: the ground of the face drawn over it starts past
        // them, never across them. Two letters at the initials' size are what a face wears
        // (`initials_for_name`), so the measured "CC" is the widest a face can show.
        {
            for (label, stack) in [
                ("bar", FaceStack::BAR),
                ("window", FaceStack::WINDOW),
                ("in a control", FaceStack::in_face(24.0)),
            ] {
                let theme = crate::theme::Theme::dark();
                let mut tr = crate::text_render::TextRenderer::new();
                let style = crate::widget::avatar::initials_style(&theme, stack.side);
                let (advance, _) = tr.measure_style("CC", None, &style);
                let initials_right = stack.side / 2.0 + f64::from(advance) / 2.0;
                // Where the next face's ground begins, from the step the stack stands by.
                let covered_from = stack.step() - Face::REACH;
                assert!(
                    covered_from > initials_right,
                    "{label}: the next face covers from {covered_from:.2} of {:.0} px, initials end at \
                 {initials_right:.2}",
                    stack.side
                );
                // And it is the rule that decides the step for every stack the shell draws, not the
                // overlap each was handed: an overlap alone stands closer than the letters are wide.
                let outer = stack.side + 2.0 * (RING + GAP);
                let plain = outer - stack.overlap;
                let rule = Face::REACH
                    + stack.side / 2.0
                    + stack.side * INITIALS_SPAN / 2.0
                    + INITIALS_CLEARANCE;
                assert!(
                    plain <= rule,
                    "{label}: an overlap of {:.1} stands wider than the letters need",
                    stack.overlap
                );
                assert!(
                    (stack.step() - rule.max(plain)).abs() < 1e-9,
                    "{label}: the step is the rule's, not the overlap's"
                );
            }
        }

        phase stacked_faces_still_overlap_and_fit_a_bar;
        // The overlap still reads as a stack: each face stands nearer than its whole width, and the
        // bar's four faces fit the width a bar gives them.
        {
            let bar = FaceStack::BAR;
            let outer = bar.side + 2.0 * (RING + GAP);
            assert!(bar.step() < outer, "neighbours overlap");
            assert!(
                bar.width(4) < 4.0 * outer,
                "four faces stand narrower than four rings"
            );
        }
    }

    #[test]
    fn a_face_that_follows_you_wears_an_eye_on_its_lower_edge() {
        let theme = crate::theme::Theme::dark();
        let bar = theme.tokens.colors.surfaces.base;
        let colour = Color::from_rgb8(0xd9, 0xa4, 0x1e);
        let plain = Face::new("Grace Hopper", "grace", colour);
        let following = Face {
            follows_you: true,
            ..plain
        };
        let (still, marked) = (
            render(&theme, &[plain], bar),
            render(&theme, &[following], bar),
        );
        // The face stands with its middle 12 px left of the stack's end at 60, 30.
        let (x1, y1) = (60.0 - FaceStack::BAR.width(1) / 2.0 + 12.0, 42.0);
        let radius = 24.0 * FOLLOWS_YOU / 2.0;
        let (cx, cy) = (x1 - radius / 2.0, y1 - radius / 2.0);
        let changed: Vec<(u32, u32)> = (0..80u32)
            .flat_map(|x| (0..60u32).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                crate::offscreen::OffscreenRenderer::sample(&still, 80, x, y)
                    != crate::offscreen::OffscreenRenderer::sample(&marked, 80, x, y)
            })
            .collect();
        assert!(
            changed.len() > 40,
            "the eye shows: {} pixels changed",
            changed.len()
        );
        for (x, y) in changed {
            let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
            assert!(
                dx.hypot(dy) <= radius + GAP + 1.0,
                "only the eye's corner of the face changes, not {x}, {y}"
            );
        }
    }
}

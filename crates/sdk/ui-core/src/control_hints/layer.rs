//! A host's control hints on screen: the [`Coach`] deciding, the card placed beside its anchor, over
//! the windows, drawn through its motion.

use super::card::{self, Contents, Frame, Look};
use super::coach::{Coach, Phase, Timing};
use super::{HintSpec, Learned, Quiet};
use crate::a11y::{AccessNode, Politeness, Role};
use crate::motion::{Beckon, stage};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::EffectiveMotion;
use crate::widget::floating_surface::PopoverSide;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke, Vec2};
use vello::peniko::Fill;

/// How a hint shows right now, as its host places it.
#[derive(Clone, Debug, PartialEq)]
pub struct HintPlace {
    /// What the hint points at: where its gesture is done.
    pub anchor: Rect,
    /// Which side of the anchor the card is on.
    pub side: PopoverSide,
    /// A line the gesture takes (a window's edge, the side the windows hide past), lit in the
    /// person's colour while the hint shows.
    pub edge: Option<Line>,
    /// Its words, in the person's language.
    pub words: String,
    /// The keys it names, as the keyboard spells them; empty for none.
    pub keys: Vec<String>,
    /// Whether the card points at its anchor; one that only sits near where its gesture leads
    /// (search, which no control on screen opens) points at nothing.
    pub points: bool,
    /// The person points by touch: the picture shows a finger on the screen rather than a pointer or a
    /// touchpad.
    pub touch: bool,
    /// The stretch the card may slide along to find free ground, beside the anchor: the edge it
    /// teaches, the bar it is held by. A card that points at nothing may also move away from its
    /// anchor as far as this reaches. The anchor alone where the card stays at its middle.
    pub along: Rect,
    /// Every window and card on screen, whole: the card stands outside them where it fits.
    pub windows: Vec<Rect>,
    /// What the card never covers: the text and controls on screen.
    pub ink: Vec<Rect>,
}

/// What the host's frame says about the person and the screen.
#[derive(Clone, Copy, Debug)]
pub struct HintLook {
    pub colour: Option<(u8, u8, u8)>,
    /// Where learned hints go: the person menu (their avatar).
    pub keep_at: Option<Point>,
    /// Where cards may stand.
    pub area: Rect,
}

#[derive(Clone, Debug)]
struct Placed {
    index: usize,
    since: Instant,
    place: HintPlace,
    card: Rect,
    notch: Option<(PopoverSide, f64)>,
}

/// A host's set of control hints, scheduled and drawn.
pub struct HintLayer<S: 'static> {
    coach: Coach<S>,
    placed: Option<Placed>,
    /// Everything the last draw painted: the card with its shadow, the lit edge, the dot and where it
    /// goes.
    painted: Option<Rect>,
}

impl<S: Copy + PartialEq + 'static> HintLayer<S> {
    #[must_use]
    pub fn new(specs: &'static [HintSpec<S>], learned: Learned) -> Self {
        Self {
            coach: Coach::new(specs, learned),
            placed: None,
            painted: None,
        }
    }

    #[must_use]
    pub fn coach(&self) -> &Coach<S> {
        &self.coach
    }

    pub fn coach_mut(&mut self) -> &mut Coach<S> {
        &mut self.coach
    }

    /// Advance to `now`: `has` and `quiet` as [`Coach::tick`] takes them, `place` how each hint would
    /// show now (`None` where its anchor is not on screen).
    pub fn update(
        &mut self,
        now: Instant,
        holds: &dyn Fn(S) -> bool,
        quiet: Option<Quiet>,
        place: &dyn Fn(&HintSpec<S>) -> Option<HintPlace>,
        motion: EffectiveMotion,
    ) {
        let specs = self.coach.specs();
        let places: Vec<Option<HintPlace>> = specs.iter().map(place).collect();
        self.coach.tick(
            now,
            holds,
            quiet,
            &|index| places.get(index).is_some_and(Option::is_some),
            Timing::of(motion),
        );
        let Some(current) = self.coach.current() else {
            self.placed = None;
            return;
        };
        let fresh = places.get(current.index).cloned().flatten();
        match (&mut self.placed, current.phase) {
            (Some(placed), Phase::Shown(_)) if placed.index == current.index => {
                if let Some(place) = fresh {
                    placed.place = place;
                }
            }
            (Some(placed), _) if placed.index == current.index => {}
            (slot, Phase::Shown(since)) => {
                *slot = fresh.map(|place| Placed {
                    index: current.index,
                    since,
                    place,
                    card: Rect::ZERO,
                    notch: None,
                });
            }
            (slot, _) => *slot = None,
        }
    }

    /// Whether the hint on screen moves at `now`.
    #[must_use]
    pub fn needs_frame(&self, now: Instant, motion: EffectiveMotion) -> bool {
        self.next_wake(now, motion)
            .is_some_and(|at| at <= now + crate::time::Duration::from_millis(17))
    }

    /// When the layer next needs a frame: soon while a hint moves, at the
    /// schedule's next change otherwise.
    #[must_use]
    pub fn next_wake(&self, now: Instant, motion: EffectiveMotion) -> Option<Instant> {
        let schedule = self.coach.next_change(Timing::of(motion));
        let moving = self
            .placed
            .as_ref()
            .and_then(|placed| match self.coach.current()?.phase {
                Phase::Shown(_) => Beckon::next_frame(placed.since, now, motion),
                Phase::Leaving(_) | Phase::Done(_) => Some(now),
            });
        match (moving, schedule) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The card as last drawn, for hit tests and capture checks.
    #[must_use]
    pub fn card_rect(&self) -> Option<Rect> {
        self.placed
            .as_ref()
            .map(|placed| placed.card)
            .filter(|card| !card.is_zero_area())
    }

    /// Everything the last draw painted (the card with its shadow, a lit edge, the dot it folds into
    /// and where that goes): while the hint moves, the only pixels it changes are under this and the
    /// last draw's.
    #[must_use]
    pub fn painted_rect(&self) -> Option<Rect> {
        self.painted
    }

    /// The hint for assistive technology: its words and keys, said politely when it comes.
    #[must_use]
    pub fn access_node(&self) -> Option<AccessNode> {
        let placed = self.placed.as_ref()?;
        if !matches!(self.coach.current()?.phase, Phase::Shown(_)) {
            return None;
        }
        let mut name = placed.place.words.clone();
        if !placed.place.keys.is_empty() {
            name.push_str(": ");
            name.push_str(&placed.place.keys.join(" "));
        }
        Some(
            AccessNode::new(Role::Label)
                .with_name(name)
                .with_semantic_key("control-hint")
                .with_live(Politeness::Polite)
                .with_bounds(placed.card),
        )
    }

    /// Draw the hint on screen at `now`.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
        look: &HintLook,
    ) {
        self.painted = None;
        let Some(current) = self.coach.current() else {
            return;
        };
        let mut painted = Painted::default();
        let motion = theme.motion();
        let specs = self.coach.specs();
        let Some((index, since, spot)) = self
            .placed
            .as_ref()
            .map(|placed| (placed.index, placed.since, placed.place.clone()))
        else {
            return;
        };
        let Some(spec) = specs.get(index) else {
            return;
        };
        let person = look.colour.map_or(theme.tokens.colors.intent.you, |rgb| {
            theme.tokens.colors.presence.of_rgb(rgb)
        });
        let colours = Look::of(person, theme);
        let contents = Contents {
            gesture: spec.gesture,
            touch: spot.touch,
            words: &spot.words,
            keys: &spot.keys,
        };
        if matches!(current.phase, Phase::Shown(_))
            || self
                .placed
                .as_ref()
                .is_some_and(|placed| placed.card.is_zero_area())
        {
            let size = card::size(&contents, text, theme);
            let previous = self
                .placed
                .as_ref()
                .map(|placed| placed.card)
                .filter(|card| !card.is_zero_area());
            let Some((rect, notch)) = place(&spot, size, look.area, previous) else {
                // No free spot beside its anchor: one never drawn yet waits for its next chance,
                // one on screen goes rather than stand over what the person reads.
                self.placed = None;
                if previous.is_some() {
                    self.coach.dismiss(now);
                } else {
                    self.coach.withdraw(now);
                }
                return;
            };
            if let Some(placed) = self.placed.as_mut() {
                placed.card = rect;
                placed.notch = notch;
            }
        }
        let Some(placed) = self.placed.as_mut() else {
            return;
        };
        let age = now.saturating_duration_since(since);
        let phase = Beckon::gesture_phase(age, motion);
        let (rect, notch) = (placed.card, placed.notch);
        match current.phase {
            Phase::Shown(_) => {
                let arrival = Beckon::arrival(age, motion);
                if let Some(edge) = placed.place.edge {
                    lit_edge(scene, transform, edge, &colours, arrival.opacity);
                    painted.edge(edge);
                }
                let pose = arrival_pose(rect, notch, arrival.scale, arrival.approach);
                painted.card(pose.transform_rect_bbox(rect));
                let frame = Frame {
                    rect,
                    notch,
                    look: colours,
                    contents,
                    phase,
                    check: 0.0,
                    opacity: arrival.opacity,
                    content_opacity: 1.0,
                    pulse: arrival.pulse,
                };
                card::paint(scene, transform * pose, text, theme, &frame);
            }
            Phase::Leaving(since) => {
                let opacity = Beckon::leaving(now.saturating_duration_since(since), motion);
                if let Some(edge) = placed.place.edge {
                    lit_edge(scene, transform, edge, &colours, opacity);
                    painted.edge(edge);
                }
                painted.card(rect);
                let frame = Frame {
                    rect,
                    notch,
                    look: colours,
                    contents,
                    phase,
                    check: 0.0,
                    opacity,
                    content_opacity: 1.0,
                    pulse: None,
                };
                card::paint(scene, transform, text, theme, &frame);
            }
            Phase::Done(since) => {
                let Some(done) = Beckon::completion(now.saturating_duration_since(since), motion)
                else {
                    return;
                };
                let disc = card::dot_centre(rect);
                if done.fold < 0.98 {
                    let dot = Rect::from_center_size(disc, (DOT, DOT));
                    let folded = Rect::new(
                        rect.x0 + (dot.x0 - rect.x0) * done.fold,
                        rect.y0 + (dot.y0 - rect.y0) * done.fold,
                        rect.x1 + (dot.x1 - rect.x1) * done.fold,
                        rect.y1 + (dot.y1 - rect.y1) * done.fold,
                    );
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "an opacity is drawn at f32 precision"
                    )]
                    let content_opacity = (1.0 - stage(done.fold, 0.0, 0.45)) as f32;
                    let frame = Frame {
                        rect: folded,
                        notch: notch.filter(|_| done.fold <= 0.0),
                        look: colours,
                        contents,
                        phase,
                        check: done.check,
                        opacity: done.opacity,
                        content_opacity,
                        pulse: None,
                    };
                    let pop = Affine::scale_about(done.pop, rect.center());
                    painted.card(pop.transform_rect_bbox(rect));
                    card::paint(scene, transform * pop, text, theme, &frame);
                }
                if done.fold >= 0.98 || done.drift > 0.0 {
                    let (at, size) = match look.keep_at {
                        Some(keep) => (
                            drift_path(disc, keep, done.drift),
                            DOT + (DOT_KEPT - DOT) * done.drift,
                        ),
                        None => (disc, DOT),
                    };
                    scene.push_layer(
                        Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        done.opacity,
                        transform,
                        &Rect::from_center_size(at, (DOT * 6.0, DOT * 6.0)),
                    );
                    card::dot(scene, transform, at, size, &colours);
                    scene.pop_layer();
                    painted.mark(Rect::from_center_size(at, (DOT * 6.0, DOT * 6.0)));
                }
                if let (Some((spread, strength)), Some(keep)) = (done.landing, look.keep_at) {
                    let at = Rect::from_center_size(keep, (LANDING, LANDING));
                    painted.mark(at.inflate(LANDING * 2.0, LANDING * 2.0));
                    card::pulse(
                        scene,
                        transform,
                        at,
                        LANDING / 2.0,
                        &colours,
                        (spread, strength),
                        true,
                    );
                }
            }
        }
        self.painted = painted.0;
    }
}

#[derive(Default)]
struct Painted(Option<Rect>);

impl Painted {
    const CARD_REACH: f64 = 40.0;
    const EDGE_REACH: f64 = 48.0;

    fn mark(&mut self, rect: Rect) {
        self.0 = Some(self.0.map_or(rect, |painted| painted.union(rect)));
    }

    fn card(&mut self, rect: Rect) {
        self.mark(rect.inflate(Self::CARD_REACH, Self::CARD_REACH));
    }

    fn edge(&mut self, edge: Line) {
        self.mark(Rect::from_points(edge.p0, edge.p1).inflate(Self::EDGE_REACH, Self::EDGE_REACH));
    }
}

const DOT: f64 = 14.0;
const DOT_KEPT: f64 = 7.0;
const LANDING: f64 = 24.0;
const ANCHOR_GAP: f64 = 4.0;
const AREA_MARGIN: f64 = 8.0;

/// How far a card keeps from the text and controls it stands beside: less than the card's own
/// distance from its anchor, so a card may still stand right beside the bar or the window it
/// points at.
const INK_MARGIN: f64 = card::NOTCH_D + ANCHOR_GAP - 1.0;
/// The step a card slides by along its anchor, looking for free ground.
const SLIDE_STEP: f64 = 8.0;

/// Where a card of `size` is placed for `place` inside `area`, and where its notch points: beside
/// its anchor, on its side or the other, slid along the anchor's stretch to the nearest spot that
/// covers none of [`HintPlace::ink`]. A spot outside every window comes first, then one on a
/// window's empty ground. `previous`, where the card stood last frame, stays while it is still such
/// a spot, so the card does not wander as the content under it changes. `None` where no spot is
/// free: the hint does not show there.
fn place(
    place: &HintPlace,
    size: (f64, f64),
    area: Rect,
    previous: Option<Rect>,
) -> Option<(Rect, Option<(PopoverSide, f64)>)> {
    let spare = area.inset(-AREA_MARGIN);
    let covers_ink = |rect: Rect| {
        let near = rect.inflate(INK_MARGIN, INK_MARGIN);
        place.ink.iter().any(|ink| ink.overlaps(near))
    };
    let outside = |rect: Rect| !place.windows.iter().any(|window| window.overlaps(rect));
    let sides = [place.side, place.side.opposite()];
    let free = |rect: &Rect| !covers_ink(*rect);
    let kept = previous
        .filter(|previous| {
            (previous.width() - size.0).abs() < 0.5 && (previous.height() - size.1).abs() < 0.5
        })
        .and_then(|previous| {
            sides
                .into_iter()
                .find(|side| {
                    spots(place, *side, size, spare).any(|spot| spot == previous) && free(&previous)
                })
                .map(|side| (side, previous))
        });
    let (side, rect) = kept
        .or_else(|| {
            sides.into_iter().find_map(|side| {
                spots(place, side, size, spare)
                    .find(|spot| free(spot) && outside(*spot))
                    .map(|spot| (side, spot))
            })
        })
        .or_else(|| {
            sides.into_iter().find_map(|side| {
                spots(place, side, size, spare)
                    .find(free)
                    .map(|spot| (side, spot))
            })
        })?;
    let card = rect.center();
    // The notch is on the card's side facing the anchor, where the card meets it.
    let notch = match side {
        PopoverSide::Below => (
            PopoverSide::Above,
            card.x.clamp(place.anchor.x0, place.anchor.x1),
        ),
        PopoverSide::Above => (
            PopoverSide::Below,
            card.x.clamp(place.anchor.x0, place.anchor.x1),
        ),
        PopoverSide::Right => (
            PopoverSide::Left,
            card.y.clamp(place.anchor.y0, place.anchor.y1),
        ),
        PopoverSide::Left => (
            PopoverSide::Right,
            card.y.clamp(place.anchor.y0, place.anchor.y1),
        ),
    };
    // Only where the card still faces its anchor: slid past it, a notch would point at nothing.
    let faces = match notch.0 {
        PopoverSide::Above | PopoverSide::Below => {
            notch.1 > rect.x0 + card::radius(rect) && notch.1 < rect.x1 - card::radius(rect)
        }
        PopoverSide::Left | PopoverSide::Right => {
            notch.1 > rect.y0 + card::radius(rect) && notch.1 < rect.y1 - card::radius(rect)
        }
    };
    Some((rect, (faces && place.points).then_some(notch)))
}

/// Every spot a card of `size` may take on `side` of `place`'s anchor inside `spare`, nearest the
/// anchor's middle first: just beside the anchor, slid along [`HintPlace::along`] a step at a time
/// either way; a card that points at nothing also moves away from the anchor, a step at a time,
/// as far as that stretch reaches. None where the card does not fit on that side.
fn spots(
    place: &HintPlace,
    side: PopoverSide,
    size: (f64, f64),
    spare: Rect,
) -> impl Iterator<Item = Rect> {
    let (w, h) = size;
    let anchor = place.anchor;
    let gap = card::NOTCH_D + ANCHOR_GAP;
    let across = match side {
        PopoverSide::Below => Some(anchor.y1 + gap).filter(|y| y + h <= spare.y1),
        PopoverSide::Above => Some(anchor.y0 - gap - h).filter(|y| *y >= spare.y0),
        PopoverSide::Right => Some(anchor.x1 + gap).filter(|x| x + w <= spare.x1),
        PopoverSide::Left => Some(anchor.x0 - gap - w).filter(|x| *x >= spare.x0),
    };
    // How far the card may move away from the anchor: as far as the stretch reaches past it on
    // that side, kept inside the area. A card that points at its anchor stays right beside it.
    let away = match side {
        PopoverSide::Below => (place.along.y1 - anchor.y1).min(spare.y1 - h - anchor.y1 - gap),
        PopoverSide::Above => (anchor.y0 - place.along.y0).min(anchor.y0 - gap - h - spare.y0),
        PopoverSide::Right => (place.along.x1 - anchor.x1).min(spare.x1 - w - anchor.x1 - gap),
        PopoverSide::Left => (anchor.x0 - place.along.x0).min(anchor.x0 - gap - w - spare.x0),
    };
    let away = if place.points { 0.0 } else { away.max(0.0) };
    let outward = match side {
        PopoverSide::Below | PopoverSide::Right => 1.0,
        PopoverSide::Above | PopoverSide::Left => -1.0,
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a count of steps across a stretch of the screen"
    )]
    let rows = (away / SLIDE_STEP).floor() as u32;
    let sideways = matches!(side, PopoverSide::Left | PopoverSide::Right);
    // Where the card's middle may go along the anchor: over its stretch, kept inside the area.
    let (half, lo, hi, edge_lo, edge_hi, mid) = if sideways {
        let along = place.along.union(anchor);
        (
            h / 2.0,
            along.y0,
            along.y1,
            spare.y0,
            spare.y1,
            anchor.center().y,
        )
    } else {
        let along = place.along.union(anchor);
        (
            w / 2.0,
            along.x0,
            along.x1,
            spare.x0,
            spare.x1,
            anchor.center().x,
        )
    };
    let fits = (edge_lo + half, (edge_hi - half).max(edge_lo + half));
    let lo = lo.clamp(fits.0, fits.1);
    let hi = hi.clamp(fits.0, fits.1);
    let mid = mid.clamp(lo, hi);
    let steps = ((hi - lo) / SLIDE_STEP).ceil();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a count of steps along a stretch of the screen"
    )]
    let steps = steps.max(0.0) as u32;
    let across = across.into_iter().flat_map(move |across| {
        (0..=rows).map(move |row| across + outward * f64::from(row) * SLIDE_STEP)
    });
    across.flat_map(move |across| {
        (0..=steps.saturating_mul(2)).filter_map(move |i| {
            let offset = f64::from(i.div_ceil(2)) * SLIDE_STEP;
            let centre = if i % 2 == 1 {
                mid + offset
            } else {
                mid - offset
            };
            (lo..=hi).contains(&centre).then(|| {
                if sideways {
                    Rect::new(across, centre - h / 2.0, across + w, centre + h / 2.0)
                } else {
                    Rect::new(centre - w / 2.0, across, centre + w / 2.0, across + h)
                }
            })
        })
    })
}

fn arrival_pose(
    rect: Rect,
    notch: Option<(PopoverSide, f64)>,
    scale: f64,
    approach: f64,
) -> Affine {
    let (origin, toward) = match notch {
        Some((PopoverSide::Above, x)) => (Point::new(x, rect.y0), Vec2::new(0.0, -1.0)),
        Some((PopoverSide::Below, x)) => (Point::new(x, rect.y1), Vec2::new(0.0, 1.0)),
        Some((PopoverSide::Left, y)) => (Point::new(rect.x0, y), Vec2::new(-1.0, 0.0)),
        Some((PopoverSide::Right, y)) => (Point::new(rect.x1, y), Vec2::new(1.0, 0.0)),
        None => (rect.center(), Vec2::new(0.0, -1.0)),
    };
    Affine::translate(toward * approach) * Affine::scale_about(scale, origin)
}

/// The path a learned hint's dot takes to where hints are kept: a swoop
/// that sets off level and curves into it, never leaving the screen.
fn drift_path(from: Point, to: Point, at: f64) -> Point {
    let bend = Point::new(to.x, from.y);
    let t = at.clamp(0.0, 1.0);
    let a = from.lerp(bend, t);
    let b = bend.lerp(to, t);
    a.lerp(b, t)
}

fn lit_edge(scene: &mut Scene, transform: Affine, edge: Line, look: &Look, opacity: f32) {
    if opacity <= 0.0 {
        return;
    }
    let bounds = Rect::from_points(edge.p0, edge.p1).inflate(3.0, 3.0);
    scene.draw_blurred_rounded_rect(
        transform,
        bounds,
        look.glow.multiply_alpha(opacity),
        3.0,
        crate::surface::std_dev_for(14.0),
    );
    scene.stroke(
        &Stroke::new(3.0).with_caps(vello::kurbo::Cap::Round),
        transform,
        look.fill.multiply_alpha(opacity * stage_opacity(opacity)),
        None,
        &edge,
    );
}

fn stage_opacity(opacity: f32) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an opacity is drawn at f32 precision"
    )]
    let staged = stage(f64::from(opacity), 0.3, 1.0) as f32;
    staged
}

#[cfg(test)]
mod place_tests {
    use super::*;

    /// The card's size, as the resize hint's card measures.
    const CARD: (f64, f64) = (186.0, 56.0);
    /// The desktop under the top bar, on a 1400 × 900 page.
    const AREA: Rect = Rect::new(0.0, 32.0, 1400.0, 900.0);

    /// Notes on the left and Chat on the right, side by side and filling the desktop, as a page
    /// shows them: each window whole (bar and body), and where each shows text and controls.
    fn side_by_side() -> (Vec<Rect>, Vec<Rect>) {
        let notes = Rect::new(32.0, 36.0, 696.0, 888.0);
        let chat = Rect::new(704.0, 36.0, 1368.0, 888.0);
        let ink = vec![
            // The two bars.
            Rect::new(32.0, 36.0, 696.0, 80.0),
            Rect::new(704.0, 36.0, 1368.0, 80.0),
            // Notes: its list and its note.
            Rect::new(48.0, 96.0, 260.0, 124.0),
            Rect::new(48.0, 160.0, 260.0, 214.0),
            Rect::new(322.0, 112.0, 592.0, 236.0),
            // Chat: its list, its empty state in the middle of its messages, its composer.
            Rect::new(712.0, 82.0, 940.0, 176.0),
            Rect::new(1136.0, 386.0, 1180.0, 430.0),
            Rect::new(1035.0, 446.0, 1280.0, 506.0),
            Rect::new(972.0, 818.0, 1344.0, 864.0),
        ];
        (vec![notes, chat], ink)
    }

    fn resize_at(edge_x: f64, side: PopoverSide, windows: Vec<Rect>, ink: Vec<Rect>) -> HintPlace {
        let edge = Rect::new(edge_x, 92.0, edge_x, 876.0);
        HintPlace {
            anchor: edge,
            side,
            edge: Some(Line::new((edge_x, 92.0), (edge_x, 876.0))),
            words: "Drag to resize".into(),
            keys: Vec::new(),
            points: true,
            touch: false,
            along: edge,
            windows,
            ink,
        }
    }

    /// The case a person saw: Chat in front, its trailing edge at the screen's, with nothing past
    /// it. The card stands beside the edge, on Chat's empty ground, clear of the empty state's
    /// words it used to cover, and still points at the edge.
    #[test]
    fn a_resize_hint_at_the_screens_edge_covers_no_text_or_control() {
        let (windows, ink) = side_by_side();
        let place_for = resize_at(1368.0, PopoverSide::Right, windows, ink.clone());
        let (card, notch) = place(&place_for, CARD, AREA, None).expect("Chat has empty ground");
        for ink in &ink {
            assert!(!card.overlaps(*ink), "{card:?} covers {ink:?}");
        }
        let gap = card::NOTCH_D + ANCHOR_GAP;
        assert_eq!(
            card.x1,
            1368.0 - gap,
            "beside the edge it teaches: {card:?}"
        );
        let (side, at) = notch.expect("it points at the edge");
        assert_eq!(side, PopoverSide::Right);
        assert!((92.0..=876.0).contains(&at), "{at}");
    }

    /// Wherever its edge, the card never covers text or controls; where it fits outside
    /// every window, it stands there, over no window's content at all.
    #[test]
    fn a_hints_card_never_covers_a_windows_text_or_controls() {
        let (windows, ink) = side_by_side();
        for (edge_x, side) in [
            (696.0, PopoverSide::Right),
            (704.0, PopoverSide::Left),
            (1368.0, PopoverSide::Right),
            (32.0, PopoverSide::Left),
        ] {
            let spot = resize_at(edge_x, side, windows.clone(), ink.clone());
            if let Some((card, _)) = place(&spot, CARD, AREA, None) {
                assert!(spare_holds(AREA, card), "{card:?} on the desktop");
                for ink in &ink {
                    assert!(!card.overlaps(*ink), "{edge_x}: {card:?} covers {ink:?}");
                }
            }
        }
        // One window, narrower than the desktop: free ground past its trailing edge, outside it.
        let window = Rect::new(32.0, 36.0, 900.0, 888.0);
        let ink = vec![
            Rect::new(32.0, 36.0, 900.0, 80.0),
            Rect::new(48.0, 96.0, 880.0, 870.0),
        ];
        let (card, _) = place(
            &resize_at(900.0, PopoverSide::Right, vec![window], ink),
            CARD,
            AREA,
            None,
        )
        .expect("free ground past the edge");
        assert!(
            !card.overlaps(window),
            "outside the window where it fits: {card:?}"
        );
        // The same edge with the free ground on its other side: it flips there.
        let (card, _) = place(
            &resize_at(900.0, PopoverSide::Left, vec![window], vec![window]),
            CARD,
            AREA,
            None,
        )
        .expect("free ground past the edge, on the other side");
        assert!(card.x0 > 900.0, "flipped outside the window: {card:?}");
    }

    /// A window whose every stretch beside the edge shows text or controls leaves the card no
    /// spot: the hint does not stand there, rather than cover them.
    #[test]
    fn a_hint_with_no_free_spot_does_not_stand() {
        let (windows, _) = side_by_side();
        let ink = windows.clone();
        assert_eq!(
            place(
                &resize_at(700.0, PopoverSide::Right, windows, ink),
                CARD,
                AREA,
                None
            ),
            None
        );
    }

    /// A card on screen keeps its spot while that stays free, rather than wander back toward
    /// the edge's middle each frame.
    #[test]
    fn a_card_on_screen_keeps_its_free_spot() {
        let (windows, ink) = side_by_side();
        let spot = resize_at(1368.0, PopoverSide::Right, windows, ink);
        let (first, _) = place(&spot, CARD, AREA, None).expect("a free spot");
        let lower = first + Vec2::new(0.0, 4.0 * SLIDE_STEP);
        let (kept, _) = place(&spot, CARD, AREA, Some(lower)).expect("a free spot");
        assert_eq!(kept, lower);
    }

    /// A hint whose card finds no free spot before it ever shows does not count as shown, and
    /// comes back at the next chance.
    #[test]
    fn a_hint_with_nowhere_to_stand_waits_for_its_next_chance() {
        use super::super::{Cue, Gesture};
        static RESIZE: &[HintSpec<()>] = &[HintSpec {
            id: "resize",
            when: &[Cue::Holds(())],
            only_while: &[],
            learned_by: &[],
            gesture: Gesture::ResizeEdge,
            words: "resize",
            touch_words: None,
        }];
        let (windows, _) = side_by_side();
        let blocked = resize_at(700.0, PopoverSide::Right, windows.clone(), windows.clone());
        let mut hints = HintLayer::new(RESIZE, Learned::default());
        let at = crate::time::now();
        let draw = |hints: &mut HintLayer<()>, at| {
            hints.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::default(),
                at,
                &HintLook {
                    colour: None,
                    keep_at: None,
                    area: AREA,
                },
            );
        };
        hints.update(
            at,
            &|()| true,
            None,
            &|_| Some(blocked.clone()),
            EffectiveMotion::default(),
        );
        draw(&mut hints, at);
        assert!(hints.coach().current().is_none(), "it does not stand");
        assert!(hints.card_rect().is_none());
        assert_eq!(hints.coach().learned().showings("resize"), 0, "nor count");
        let later = at + super::super::coach::RETRY_AFTER;
        let (_, ink) = side_by_side();
        let free = resize_at(1368.0, PopoverSide::Right, windows, ink);
        hints.update(
            later,
            &|()| true,
            None,
            &|_| Some(free.clone()),
            EffectiveMotion::default(),
        );
        draw(&mut hints, later);
        assert!(hints.card_rect().is_some(), "it shows at the next chance");
        assert_eq!(hints.coach().learned().showings("resize"), 1);
    }

    fn spare_holds(area: Rect, card: Rect) -> bool {
        area.inset(-AREA_MARGIN).contains_rect(card)
    }
}

//! A host's control hints on screen: the [`Coach`] deciding, the card
//! placed at its anchor and drawn through its motion.

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
    /// Which side of the anchor the card stands on.
    pub side: PopoverSide,
    /// A line the gesture takes (a window's edge), lit in the person's
    /// colour while the hint shows.
    pub edge: Option<Line>,
    /// Its words, in the person's language.
    pub words: String,
    /// The keys it names, as the keyboard spells them; empty for none.
    pub keys: Vec<String>,
    /// Whether the card points at its anchor; one that only stands near
    /// where its gesture leads (search, which no control on screen opens)
    /// points at nothing.
    pub points: bool,
}

/// What the host's frame says about the person and the screen.
#[derive(Clone, Copy, Debug)]
pub struct HintLook {
    /// The person's own colour.
    pub colour: Option<(u8, u8, u8)>,
    /// Where learned hints go: the person's own menu (their avatar).
    pub keep_at: Option<Point>,
    /// Where cards may stand.
    pub area: Rect,
}

/// The hint on screen as last placed and drawn.
#[derive(Clone, Debug)]
struct Placed {
    index: usize,
    since: Instant,
    place: HintPlace,
    /// The card as last drawn, and the side its anchor is on.
    card: Rect,
    notch: Option<(PopoverSide, f64)>,
}

/// A host's set of control hints, scheduled and drawn.
pub struct HintLayer<S: 'static> {
    coach: Coach<S>,
    placed: Option<Placed>,
}

impl<S: Copy + PartialEq + 'static> HintLayer<S> {
    #[must_use]
    pub fn new(specs: &'static [HintSpec<S>], learned: Learned) -> Self {
        Self {
            coach: Coach::new(specs, learned),
            placed: None,
        }
    }

    #[must_use]
    pub fn coach(&self) -> &Coach<S> {
        &self.coach
    }

    pub fn coach_mut(&mut self) -> &mut Coach<S> {
        &mut self.coach
    }

    /// Advance to `now`: `holds` and `quiet` as [`Coach::tick`] takes them,
    /// `place` how each hint would show now (`None` where its anchor is not
    /// on screen).
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
                // It follows its anchor while it teaches.
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

    /// The hint for assistive technology: its words and keys, said
    /// politely when it comes.
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
        let Some(current) = self.coach.current() else {
            return;
        };
        let motion = theme.motion();
        let specs = self.coach.specs();
        let Some(placed) = self.placed.as_mut() else {
            return;
        };
        let Some(spec) = specs.get(placed.index) else {
            return;
        };
        let person = look.colour.map_or(theme.tokens.colors.intent.you, |rgb| {
            theme.tokens.colors.presence.of_rgb(rgb)
        });
        let colours = Look::of(person, theme);
        let contents = Contents {
            gesture: spec.gesture,
            words: &placed.place.words,
            keys: &placed.place.keys,
        };
        if matches!(current.phase, Phase::Shown(_)) || placed.card.is_zero_area() {
            let size = card::size(&contents, text, theme);
            let (rect, notch) = place(&placed.place, size, look.area);
            placed.card = rect;
            placed.notch = notch;
        }
        let age = now.saturating_duration_since(placed.since);
        let phase = Beckon::gesture_phase(age, motion);
        let (rect, notch) = (placed.card, placed.notch);
        match current.phase {
            Phase::Shown(_) => {
                let arrival = Beckon::arrival(age, motion);
                if let Some(edge) = placed.place.edge {
                    lit_edge(scene, transform, edge, &colours, arrival.opacity);
                }
                let pose = arrival_pose(rect, notch, arrival.scale, arrival.approach);
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
                }
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
                let disc = Point::new(rect.x0 + card::PAD + 30.0, rect.center().y);
                if done.fold < 0.98 {
                    // The card folds into a dot where its check is.
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
                    // The check lands with a small lift of the whole card.
                    let pop = Affine::scale_about(done.pop, rect.center());
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
                }
                if let (Some((spread, strength)), Some(keep)) = (done.landing, look.keep_at) {
                    // Where it lands, the menu it went into lights once.
                    let at = Rect::from_center_size(keep, (LANDING, LANDING));
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
    }
}

/// The dot a done card folds into, and its size where it is kept.
const DOT: f64 = 14.0;
const DOT_KEPT: f64 = 7.0;
/// The light where the dot lands, about the size of the avatar it lands in.
const LANDING: f64 = 24.0;
/// How far the card stands from its anchor, beyond its notch.
const ANCHOR_GAP: f64 = 4.0;
/// How far from the area's edges a card keeps.
const AREA_MARGIN: f64 = 8.0;

/// Where a card of `size` stands for `place` inside `area`: on its side of
/// the anchor (the other side when there is no room), centred on it, kept
/// inside the area; and where its notch points.
fn place(place: &HintPlace, size: (f64, f64), area: Rect) -> (Rect, Option<(PopoverSide, f64)>) {
    let (w, h) = size;
    let anchor = place.anchor;
    let gap = card::NOTCH_D + ANCHOR_GAP;
    let room = area.inset(-AREA_MARGIN);
    let fits = |side: PopoverSide| match side {
        PopoverSide::Below => anchor.y1 + gap + h <= room.y1,
        PopoverSide::Above => anchor.y0 - gap - h >= room.y0,
        PopoverSide::Right => anchor.x1 + gap + w <= room.x1,
        PopoverSide::Left => anchor.x0 - gap - w >= room.x0,
    };
    let side = if fits(place.side) || !fits(place.side.opposite()) {
        place.side
    } else {
        place.side.opposite()
    };
    let centre = anchor.center();
    let clamp_x = |x: f64| x.clamp(room.x0, (room.x1 - w).max(room.x0));
    let clamp_y = |y: f64| y.clamp(room.y0, (room.y1 - h).max(room.y0));
    let (x, y) = match side {
        PopoverSide::Below => (clamp_x(centre.x - w / 2.0), clamp_y(anchor.y1 + gap)),
        PopoverSide::Above => (clamp_x(centre.x - w / 2.0), clamp_y(anchor.y0 - gap - h)),
        PopoverSide::Right => (clamp_x(anchor.x1 + gap), clamp_y(centre.y - h / 2.0)),
        PopoverSide::Left => (clamp_x(anchor.x0 - gap - w), clamp_y(centre.y - h / 2.0)),
    };
    let rect = Rect::new(x, y, x + w, y + h);
    // The notch is on the card's side facing the anchor.
    let notch = match side {
        PopoverSide::Below => (PopoverSide::Above, centre.x),
        PopoverSide::Above => (PopoverSide::Below, centre.x),
        PopoverSide::Right => (PopoverSide::Left, centre.y),
        PopoverSide::Left => (PopoverSide::Right, centre.y),
    };
    // Only where the card still faces its anchor: pushed past it, a notch
    // would point at nothing.
    let faces = match notch.0 {
        PopoverSide::Above | PopoverSide::Below => {
            centre.x > rect.x0 + card::RADIUS && centre.x < rect.x1 - card::RADIUS
        }
        PopoverSide::Left | PopoverSide::Right => {
            centre.y > rect.y0 + card::RADIUS && centre.y < rect.y1 - card::RADIUS
        }
    };
    (rect, (faces && place.points).then_some(notch))
}

/// The card arriving: scaled about its notch's tip (or its centre), and
/// standing `approach` px back from its anchor.
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
    // It comes out of its anchor: from nearer it, toward where it rests.
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

/// The edge the gesture takes, lit in the person's colour along its
/// length, as the desktop lights an edge a press would move.
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

/// Opacity that comes in a little behind the card's, so the edge lights
/// once the card is there to say why.
fn stage_opacity(opacity: f32) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an opacity is drawn at f32 precision"
    )]
    let staged = stage(f64::from(opacity), 0.3, 1.0) as f32;
    staged
}

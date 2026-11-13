//! A host's control hints on screen: the [`Coach`] deciding, the card placed beside its anchor or in
//! the bar's room and drawn through its motion.

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

/// Where a hint is.
#[derive(Clone, Debug, PartialEq)]
pub enum Stand {
    /// Beside the control the gesture is done on: `anchor` is where it is done, `side` the side of it
    /// the card sits on, and `points` whether the card points at the anchor.
    Beside {
        anchor: Rect,
        side: PopoverSide,
        points: bool,
    },
    /// In the top bar for a gesture with no control on screen to point at: the pill is at `band`'s
    /// leading end, just past the bar's clock, a line tall, whole or not at all.
    InBar { band: Rect },
}

/// How a hint shows right now, as its host places it.
#[derive(Clone, Debug, PartialEq)]
pub struct HintPlace {
    /// Where it is.
    pub stand: Stand,
    /// A line the gesture takes (a window's edge, the bar it is held by, the strip's), lit in the
    /// person's colour while the hint shows.
    pub edge: Option<Line>,
    /// Its words, in the person's language.
    pub words: String,
    /// The keys it names, as the keyboard spells them; empty for none.
    pub keys: Vec<String>,
    /// The person points by touch: the picture shows a finger on the screen rather than a pointer or a
    /// touchpad.
    pub touch: bool,
    /// Another surface's content, as one rect each (a window's body, the
    /// cards standing over the desk and in a window's corners), which the
    /// hint's card never sits over. Where there is no side of its anchor
    /// clear of them, the hint is in [`Self::bar`], or has nowhere to
    /// stand and does not show.
    pub clear: Vec<Rect>,
    /// The room the top bar keeps beside its clock, for a hint beside a control whose sides are all
    /// content (a window's bar over its body, a window's edge between two windows, or at the screen's):
    /// the hint is there instead, as one for a gesture with no control on screen does, while
    /// [`Self::edge`] lights where the gesture is done.
    pub bar: Option<Rect>,
}

impl Stand {
    /// Where the gesture is done: on the control the hint sits beside, or nowhere (a gesture with no
    /// control on screen).
    #[must_use]
    pub fn anchor(&self) -> Option<Rect> {
        match self {
            Stand::Beside { anchor, .. } => Some(*anchor),
            Stand::InBar { .. } => None,
        }
    }
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
    in_bar: bool,
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
                    in_bar: false,
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
            let in_bar = matches!(&spot.stand, Stand::InBar { .. });
            let size = card::size(&contents, text, theme, in_bar);
            let beside = place(&spot, size, look.area).map(|(rect, notch)| (rect, notch, in_bar));
            let placement = beside.or_else(|| {
                let band = spot.bar?;
                let in_room = HintPlace {
                    stand: Stand::InBar { band },
                    ..spot.clone()
                };
                let size = card::size(&contents, text, theme, true);
                place(&in_room, size, look.area).map(|(rect, notch)| (rect, notch, true))
            });
            let Some((rect, notch, in_bar)) = placement else {
                self.placed = None;
                self.coach.dismiss(now);
                return;
            };
            if let Some(placed) = self.placed.as_mut() {
                placed.card = rect;
                placed.notch = notch;
                placed.in_bar = in_bar;
            }
        }
        let Some(placed) = self.placed.as_mut() else {
            return;
        };
        let in_bar = placed.in_bar;
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
                    in_bar,
                    phase,
                    check: 0.0,
                    opacity: arrival.opacity,
                    content_opacity: 1.0,
                    pulse: if in_bar { None } else { arrival.pulse },
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
                    in_bar,
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
                let disc = card::dot_centre(rect, in_bar);
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
                        in_bar,
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

/// Where a card of `size` is `place` inside `area`: in the bar, at the leading end of the
/// room it keeps beside its clock; or beside its anchor, on its side (the other side when there is
/// no room), centred on it, kept inside the area. `None` where nothing can be cleared: a hint never
/// sits over another surface's content, nor over the bar controls, so one with nowhere to
/// stand does not show.
fn place(
    place: &HintPlace,
    size: (f64, f64),
    area: Rect,
) -> Option<(Rect, Option<(PopoverSide, f64)>)> {
    let (w, h) = size;
    let room = area.inset(-AREA_MARGIN);
    let clear = |rect: Rect| !place.clear.iter().any(|card| card.overlaps(rect));
    let (anchor, side, points) = match place.stand {
        // A hint with no control on screen to point at is in the bar, right beside the clock:
        // the bar keeps that room clear of everything it draws, and where the room cannot hold the
        // pill whole it does not show rather than stand over the clock or another control.
        Stand::InBar { band } => {
            if w > band.width() || h > band.height() {
                return None;
            }
            let rect = Rect::new(
                band.x0,
                band.center().y - h / 2.0,
                band.x0 + w,
                band.center().y + h / 2.0,
            );
            return clear(rect).then_some((rect, None));
        }
        Stand::Beside {
            anchor,
            side,
            points,
        } => (anchor, side, points),
    };
    let gap = card::NOTCH_D + ANCHOR_GAP;
    let fits = |side: PopoverSide| match side {
        PopoverSide::Below => anchor.y1 + gap + h <= room.y1,
        PopoverSide::Above => anchor.y0 - gap - h >= room.y0,
        PopoverSide::Right => anchor.x1 + gap + w <= room.x1,
        PopoverSide::Left => anchor.x0 - gap - w >= room.x0,
    };
    let centre = anchor.center();
    let clamp_x = |x: f64| x.clamp(room.x0, (room.x1 - w).max(room.x0));
    let clamp_y = |y: f64| y.clamp(room.y0, (room.y1 - h).max(room.y0));
    let on_side = |side: PopoverSide| {
        let (x, y) = match side {
            PopoverSide::Below => (clamp_x(centre.x - w / 2.0), clamp_y(anchor.y1 + gap)),
            PopoverSide::Above => (clamp_x(centre.x - w / 2.0), clamp_y(anchor.y0 - gap - h)),
            PopoverSide::Right => (clamp_x(anchor.x1 + gap), clamp_y(centre.y - h / 2.0)),
            PopoverSide::Left => (clamp_x(anchor.x0 - gap - w), clamp_y(centre.y - h / 2.0)),
        };
        Rect::new(x, y, x + w, y + h)
    };
    let (side, rect) = [side, side.opposite()]
        .into_iter()
        .filter(|side| fits(*side))
        .map(|side| (side, on_side(side)))
        .find(|(_, rect)| clear(*rect))
        .or_else(|| {
            let rect = on_side(side);
            clear(rect).then_some((side, rect))
        })?;
    let notch = match side {
        PopoverSide::Below => (PopoverSide::Above, centre.x),
        PopoverSide::Above => (PopoverSide::Below, centre.x),
        PopoverSide::Right => (PopoverSide::Left, centre.y),
        PopoverSide::Left => (PopoverSide::Right, centre.y),
    };
    let faces = match notch.0 {
        PopoverSide::Above | PopoverSide::Below => {
            centre.x > rect.x0 + card::radius(rect) && centre.x < rect.x1 - card::radius(rect)
        }
        PopoverSide::Left | PopoverSide::Right => {
            centre.y > rect.y0 + card::radius(rect) && centre.y < rect.y1 - card::radius(rect)
        }
    };
    Some((rect, (faces && points).then_some(notch)))
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
    use super::super::{Cue, Gesture};
    use super::*;
    use crate::control_hints::coach::Current;

    fn place_at(side: PopoverSide, clear: Vec<Rect>) -> HintPlace {
        HintPlace {
            stand: Stand::Beside {
                anchor: Rect::new(700.0, 100.0, 700.0, 700.0),
                side,
                points: true,
            },
            edge: None,
            words: "Drag to resize".into(),
            keys: Vec::new(),
            touch: false,
            clear,
            bar: None,
        }
    }

    fn in_bar(band: Rect, clear: Vec<Rect>) -> HintPlace {
        HintPlace {
            stand: Stand::InBar { band },
            edge: None,
            words: "Open anything".into(),
            keys: vec!["Ctrl".into(), "K".into()],
            touch: false,
            clear,
            bar: None,
        }
    }

    /// A hint keeps off the cards standing over the desk: it sits on the other side of its
    /// anchor when that side is free, never over a card's content.
    #[test]
    fn a_hint_stands_beside_its_anchor_clear_of_the_cards_over_the_desk() {
        let area = Rect::new(0.0, 40.0, 1280.0, 800.0);
        let free = place(
            &place_at(PopoverSide::Right, Vec::new()),
            (220.0, 60.0),
            area,
        )
        .expect("room beside its anchor");
        assert_eq!(free.0.x0, 700.0 + card::NOTCH_D + 4.0);
        let card = Rect::new(700.0, 300.0, 1260.0, 740.0);
        let (rect, _) = place(
            &place_at(PopoverSide::Right, vec![card]),
            (220.0, 60.0),
            area,
        )
        .expect("the other side is free");
        assert!(!rect.overlaps(card), "{rect:?} over the card {card:?}");
        assert!(rect.x1 <= card.x0, "beside it, not over it: {rect:?}");
        let left = Rect::new(0.0, 60.0, 600.0, 760.0);
        assert!(
            place(
                &place_at(PopoverSide::Right, vec![card, left]),
                (220.0, 60.0),
                area
            )
            .is_none(),
            "no place clear of both cards"
        );
    }

    /// Every placement keeps out of the content it is given, wherever it is: in the bar at the
    /// leading end of the room beside its clock, or beside its anchor on either side. A placement
    /// that cannot is one that does not show.
    #[test]
    fn no_placement_stands_over_the_content_it_keeps_clear_of() {
        let area = Rect::new(0.0, 32.0, 1280.0, 800.0);
        let band = Rect::new(628.0, 0.0, 1100.0, 32.0);
        let kept = [
            Rect::new(700.0, 72.0, 1268.0, 788.0),
            Rect::new(1120.0, 84.0, 1240.0, 136.0),
            Rect::new(0.0, 600.0, 300.0, 780.0),
        ];
        let (rect, notch) = place(&in_bar(band, kept.to_vec()), (240.0, 28.0), area)
            .expect("the bar keeps room beside its clock");
        assert_eq!(notch, None, "it points at no control");
        assert_eq!(rect.x0, band.x0, "right beside the clock: {rect:?}");
        assert_eq!(rect.center().y, band.center().y, "a line tall: {rect:?}");
        assert!(band.contains_rect(rect), "{rect:?} in {band:?}");
        for kept in kept {
            assert!(!rect.overlaps(kept), "{rect:?} over {kept:?}");
        }
        assert!(
            place(
                &in_bar(Rect::new(628.0, 0.0, 860.0, 32.0), kept.to_vec()),
                (240.0, 28.0),
                area
            )
            .is_none(),
            "no room for the pill whole"
        );
        assert!(
            place(
                &in_bar(Rect::new(628.0, 0.0, 1100.0, 24.0), kept.to_vec()),
                (240.0, 28.0),
                area
            )
            .is_none(),
            "a bar too shallow to hold it"
        );
        assert!(
            place(
                &in_bar(band, vec![Rect::new(600.0, 0.0, 900.0, 32.0)]),
                (240.0, 28.0),
                area
            )
            .is_none()
        );
        for side in [
            PopoverSide::Below,
            PopoverSide::Above,
            PopoverSide::Right,
            PopoverSide::Left,
        ] {
            if let Some((rect, _)) = place(&place_at(side, kept.to_vec()), (220.0, 56.0), area) {
                for kept in kept {
                    assert!(
                        !rect.overlaps(kept),
                        "{side:?}'s hint {rect:?} over {kept:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_hint_with_nowhere_to_stand_leaves_rather_than_covering_a_card() {
        static RESIZE: &[HintSpec<()>] = &[HintSpec {
            id: "resize",
            when: &[Cue::Holds(())],
            only_while: &[],
            learned_by: &[],
            gesture: Gesture::ResizeEdge,
            words: "resize",
            touch_words: None,
        }];
        let area = Rect::new(0.0, 40.0, 700.0, 800.0);
        let card = Rect::new(0.0, 60.0, 700.0, 760.0);
        let placed = |_spec: &HintSpec<()>| Some(place_at(PopoverSide::Right, vec![card]));
        let mut hints = HintLayer::new(RESIZE, Learned::default());
        let at = crate::time::now();
        hints.update(at, &|()| true, None, &placed, EffectiveMotion::default());
        assert!(hints.coach().current().is_some(), "fixture: it begins");
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        hints.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::default(),
            at,
            &HintLook {
                colour: None,
                keep_at: None,
                area,
            },
        );
        assert!(hints.card_rect().is_none(), "nothing stands over the card");
        assert!(hints.painted_rect().is_none());
        assert!(
            matches!(
                hints.coach().current(),
                Some(Current {
                    phase: Phase::Leaving(_),
                    ..
                })
            ),
            "a hint with nowhere to stand goes"
        );
    }

    #[test]
    fn a_hint_whose_anchor_has_no_clear_side_stands_in_the_bar_with_its_edge_lit() {
        static RESIZE: &[HintSpec<()>] = &[HintSpec {
            id: "resize",
            when: &[Cue::Holds(())],
            only_while: &[],
            learned_by: &[],
            gesture: Gesture::ResizeEdge,
            words: "resize",
            touch_words: None,
        }];
        let area = Rect::new(0.0, 40.0, 1440.0, 900.0);
        let band = Rect::new(780.0, 0.0, 1300.0, 32.0);
        let left = Rect::new(32.0, 80.0, 696.0, 888.0);
        let right = Rect::new(704.0, 80.0, 1408.0, 888.0);
        let edge = Line::new((700.0, 96.0), (700.0, 880.0));
        let spot = HintPlace {
            edge: Some(edge),
            bar: Some(band),
            ..place_at(PopoverSide::Right, vec![left, right])
        };
        let placed = |_spec: &HintSpec<()>| Some(spot.clone());
        let mut hints = HintLayer::new(RESIZE, Learned::default());
        let at = crate::time::now();
        hints.update(at, &|()| true, None, &placed, EffectiveMotion::default());
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        hints.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::default(),
            at,
            &HintLook {
                colour: None,
                keep_at: None,
                area,
            },
        );
        let card = hints.card_rect().expect("it stands in the bar");
        assert!(
            band.contains_rect(card),
            "{card:?} in the bar's room {band:?}"
        );
        for content in [left, right] {
            assert!(!card.overlaps(content), "{card:?} over {content:?}");
        }
        let painted = hints.painted_rect().expect("it painted");
        assert!(
            painted.contains(edge.p0) && painted.contains(edge.p1),
            "the edge lights where the gesture is done"
        );
    }
}

//! The shell's control hints on screen: the [`Coach`] deciding, the pill placed beside its anchor, over
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
    /// Which side of the anchor the pill is on.
    pub side: PopoverSide,
    /// A line the gesture takes (a window's edge, the side the windows hide past), lit in the
    /// person's colour while the hint shows.
    pub edge: Option<Line>,
    /// Its words, in the person's language.
    pub words: String,
    /// The keys it names, as the keyboard spells them; empty for none.
    pub keys: Vec<String>,
    /// Whether the pill points at its anchor; one that only sits near where its gesture leads
    /// (search, which no control on screen opens) points at nothing.
    pub points: bool,
    /// The stretch the pill may slide along to find free ground, beside the anchor: the edge it
    /// teaches, the bar it is held by. A pill that points at nothing may also move away from its
    /// anchor as far as this reaches. The anchor alone where the pill stays at its middle.
    pub along: Rect,
    /// Every window and card on screen, whole: the pill stands outside them where it fits.
    pub windows: Vec<Rect>,
    /// Each window's bar band, whole: the ground the pill may stand on beside its gesture where
    /// there is no ground outside a window. The window's content is never such ground.
    pub bars: Vec<Rect>,
    /// What the pill never covers: the text and controls on screen.
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

    /// The pill as last drawn, for hit tests and capture checks.
    #[must_use]
    pub fn card_rect(&self) -> Option<Rect> {
        self.placed
            .as_ref()
            .map(|placed| placed.card)
            .filter(|card| !card.is_zero_area())
    }

    /// Everything the last draw painted (the pill with its shadow, a lit edge, the dot it folds into
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
        let Some((since, spot)) = self
            .placed
            .as_ref()
            .map(|placed| (placed.since, placed.place.clone()))
        else {
            return;
        };
        let person = look.colour.map_or(theme.tokens.colors.intent.you, |rgb| {
            theme.tokens.colors.presence.of_rgb(rgb)
        });
        let colours = Look::of(person, theme);
        let contents = Contents {
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

/// How far a pill keeps from the text and controls it stands beside: less than the pill's own
/// distance from its anchor, so it may still stand right beside the bar or the window it points
/// at.
const INK_MARGIN: f64 = card::NOTCH_D + ANCHOR_GAP - 1.0;
/// The step a pill slides by along its anchor or a bar, looking for free ground.
const SLIDE_STEP: f64 = 8.0;

/// Where a pill of `size` is placed for `place` inside `area`, and where its notch points: beside
/// its anchor or, where no ground is free there, on a window's bar band next to its gesture. The
/// pill stands outside every window where it fits, or else on a bar's empty ground; it never
/// stands over a window's content, nor covers [`HintPlace::ink`] (the text and controls on
/// screen), and it keeps off the anchor it points at while any ground beside it is free.
/// `previous`, where the pill stood last frame, stays while it is still such a spot, so it does
/// not wander as the content under it changes. `None` where no spot is free: the hint does not
/// show there.
fn place(
    place: &HintPlace,
    size: (f64, f64),
    area: Rect,
    previous: Option<Rect>,
) -> Option<(Rect, Option<(PopoverSide, f64)>)> {
    let spare = area;
    let clear = |rect: Rect| {
        let near = rect.inflate(INK_MARGIN, INK_MARGIN);
        spare.contains_rect(rect) && !place.ink.iter().any(|ink| ink.overlaps(near))
    };
    // The ground a pill may stand on: outside every window, or on a window's bar band. A window's
    // content is never ground, so what the person reads there stays uncovered.
    let outside = |rect: Rect| !place.windows.iter().any(|window| window.overlaps(rect));
    let on_bar = |rect: Rect| place.bars.iter().any(|bar| bar.contains_rect(rect));
    let ground = |rect: Rect| clear(rect) && (outside(rect) || on_bar(rect));
    // Beside the anchor, never on it: one that points at a stretch of bar does not hide it.
    let beside = |rect: Rect| ground(rect) && !rect.overlaps(place.anchor);
    let kept = previous
        .filter(|previous| {
            (previous.width() - size.0).abs() < 0.5 && (previous.height() - size.1).abs() < 0.5
        })
        .filter(|previous| beside(*previous));
    let sides = [place.side, place.side.opposite()];
    let rect = kept
        .or_else(|| {
            sides.into_iter().find_map(|side| {
                spots(place, side, size, spare).find(|spot| beside(*spot) && outside(*spot))
            })
        })
        .or_else(|| {
            sides
                .into_iter()
                .find_map(|side| spots(place, side, size, spare).find(|spot| beside(*spot)))
        })
        .or_else(|| {
            // The bar ground nearest where the gesture is, across every window's bar: the last
            // resort where a bar has no free stretch beside the place that takes it.
            bar_spots(place, size, spare)
                .filter(|spot| ground(*spot))
                .min_by(|a, b| reach(*a, place.anchor).total_cmp(&reach(*b, place.anchor)))
        })?;
    Some((rect, notch_for(rect, place.anchor, place.points)))
}

/// How far apart two rects are, corner to corner: `0` where they touch or overlap.
fn reach(a: Rect, b: Rect) -> f64 {
    let dx = (b.x0 - a.x1).max(a.x0 - b.x1).max(0.0);
    let dy = (b.y0 - a.y1).max(a.y0 - b.y1).max(0.0);
    dx.hypot(dy)
}

/// On which side of `rect` its notch stands and where along it, pointing at `anchor`: the side
/// facing the anchor, along the axis the anchor lies further on. `None` where the pill stands on
/// its anchor (nothing to point at) or where the notch would not fit its side.
fn notch_for(rect: Rect, anchor: Rect, points: bool) -> Option<(PopoverSide, f64)> {
    if !points {
        return None;
    }
    let x_gap = if anchor.x1 <= rect.x0 {
        rect.x0 - anchor.x1
    } else if rect.x1 <= anchor.x0 {
        anchor.x0 - rect.x1
    } else {
        0.0
    };
    let y_gap = if anchor.y1 <= rect.y0 {
        rect.y0 - anchor.y1
    } else if rect.y1 <= anchor.y0 {
        anchor.y0 - rect.y1
    } else {
        0.0
    };
    if x_gap <= 0.0 && y_gap <= 0.0 {
        return None;
    }
    let radius = card::radius(rect);
    if y_gap >= x_gap {
        (rect.width() >= 2.0 * radius + card::NOTCH_W).then(|| {
            let side = if anchor.y1 <= rect.y0 {
                PopoverSide::Above
            } else {
                PopoverSide::Below
            };
            let at = anchor.center().x.clamp(rect.x0 + radius, rect.x1 - radius);
            (side, at)
        })
    } else {
        (rect.height() >= 2.0 * radius + card::NOTCH_W).then(|| {
            let side = if anchor.x1 <= rect.x0 {
                PopoverSide::Left
            } else {
                PopoverSide::Right
            };
            let at = anchor.center().y.clamp(rect.y0 + radius, rect.y1 - radius);
            (side, at)
        })
    }
}

/// Every spot a pill of `size` may take on `side` of `place`'s anchor inside `spare`, nearest the
/// anchor's middle first: beside the anchor, slid along [`HintPlace::along`] a step at a time
/// either way, or on the anchor's own band where the anchor is as deep as the pill (a window's
/// bar, beside the stretch that takes it); a pill that points at nothing also moves away from the
/// anchor, a step at a time, as far as that stretch reaches. None where the pill does not fit on
/// that side.
fn spots(
    place: &HintPlace,
    side: PopoverSide,
    size: (f64, f64),
    spare: Rect,
) -> impl Iterator<Item = Rect> {
    let (w, h) = size;
    let anchor = place.anchor;
    let gap = card::NOTCH_D + ANCHOR_GAP;
    let along_side = matches!(side, PopoverSide::Left | PopoverSide::Right);
    // How far the pill may move away from the anchor: as far as the stretch reaches past it on
    // that side, kept inside the area. A pill that points at its anchor stays right beside it.
    let away = match side {
        PopoverSide::Below => (place.along.y1 - anchor.y1).min(spare.y1 - h - anchor.y1 - gap),
        PopoverSide::Above => (anchor.y0 - place.along.y0).min(anchor.y0 - gap - h - spare.y0),
        PopoverSide::Right => (place.along.x1 - anchor.x1).min(spare.x1 - w - anchor.x1 - gap),
        PopoverSide::Left => (anchor.x0 - place.along.x0).min(anchor.x0 - gap - w - spare.x0),
    };
    let outward = match side {
        PopoverSide::Below | PopoverSide::Right => 1.0,
        PopoverSide::Above | PopoverSide::Left => -1.0,
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a count of steps across a stretch of the screen"
    )]
    let rows = ((if place.points { 0.0 } else { away.max(0.0) }) / SLIDE_STEP).floor() as u32;
    // The pill's own band of the anchor, where the anchor is as deep as the pill there: beside
    // the stretch a window's bar takes it by.
    let band = if along_side {
        (anchor.width() >= w).then(|| anchor.center().x - w / 2.0)
    } else {
        (anchor.height() >= h).then(|| anchor.center().y - h / 2.0)
    };
    let across: Vec<Option<f64>> = vec![
        match side {
            PopoverSide::Below => Some(anchor.y1 + gap).filter(|y| y + h <= spare.y1),
            PopoverSide::Above => Some(anchor.y0 - gap - h).filter(|y| *y >= spare.y0),
            PopoverSide::Right => Some(anchor.x1 + gap).filter(|x| x + w <= spare.x1),
            PopoverSide::Left => Some(anchor.x0 - gap - w).filter(|x| *x >= spare.x0),
        },
        band.filter(|at| {
            if along_side {
                *at >= spare.x0 && at + w <= spare.x1
            } else {
                *at >= spare.y0 && at + h <= spare.y1
            }
        }),
    ];
    let (half, lo, hi, edge_lo, edge_hi, mid) = if along_side {
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
    let across = across.into_iter().flatten().flat_map(move |across| {
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
                if along_side {
                    Rect::new(across, centre - h / 2.0, across + w, centre + h / 2.0)
                } else {
                    Rect::new(centre - w / 2.0, across, centre + w / 2.0, across + h)
                }
            })
        })
    })
}

/// Every spot a pill of `size` may take on a window's bar band inside `spare`, nearest where the
/// gesture is first: the ground that belongs to the window's own chrome, never to its content.
fn bar_spots(place: &HintPlace, size: (f64, f64), spare: Rect) -> impl Iterator<Item = Rect> {
    let (w, h) = size;
    let bars: Vec<Rect> = place
        .bars
        .iter()
        .filter(|bar| bar.height() >= h && bar.width() >= w)
        .copied()
        .collect();
    let mid_x = place.anchor.center().x;
    bars.into_iter().flat_map(move |bar| {
        let band = bar.center().y - h / 2.0;
        let lo = bar.x0.clamp(spare.x0, (spare.x1 - w).max(spare.x0));
        let hi = (bar.x1 - w).clamp(lo, (spare.x1 - w).max(lo));
        let mid = (mid_x - w / 2.0).clamp(lo, hi);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a count of steps along a bar"
        )]
        let steps = ((hi - lo) / SLIDE_STEP).ceil().max(0.0) as u32;
        (0..=steps.saturating_mul(2)).filter_map(move |i| {
            let offset = f64::from(i.div_ceil(2)) * SLIDE_STEP;
            let x0 = if i % 2 == 1 {
                mid + offset
            } else {
                mid - offset
            };
            (lo..=hi)
                .contains(&x0)
                .then(|| Rect::new(x0, band, x0 + w, band + h))
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

    /// The pill's size, as the resize hint's measures.
    const PILL: (f64, f64) = (128.0, 30.0);
    /// The desktop under the top bar, on a 1400 × 900 page.
    const AREA: Rect = Rect::new(0.0, 32.0, 1400.0, 900.0);

    /// Notes on the left and Chat on the right, side by side and filling the desktop, as a page
    /// shows them: each window whole (bar and body), its bar, and where each shows text and
    /// controls: the bars' tabs and window controls, and the content under them.
    fn side_by_side() -> (Vec<Rect>, Vec<Rect>, Vec<Rect>) {
        let notes = Rect::new(32.0, 36.0, 696.0, 888.0);
        let chat = Rect::new(704.0, 36.0, 1368.0, 888.0);
        let bars = vec![
            Rect::new(32.0, 36.0, 696.0, 80.0),
            Rect::new(704.0, 36.0, 1368.0, 80.0),
        ];
        let ink = vec![
            // The two bars' tabs and window controls.
            Rect::new(40.0, 48.0, 180.0, 68.0),
            Rect::new(646.0, 48.0, 690.0, 68.0),
            Rect::new(712.0, 48.0, 852.0, 68.0),
            Rect::new(1318.0, 48.0, 1362.0, 68.0),
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
        (vec![notes, chat], bars, ink)
    }

    /// The body of each `window`: what it shows under `bars`, where no pill may stand.
    fn bodies(windows: &[Rect], bars: &[Rect]) -> Vec<Rect> {
        windows
            .iter()
            .filter_map(|window| {
                let bar = bars
                    .iter()
                    .find(|bar| bar.x0 == window.x0 && bar.x1 == window.x1)?;
                Some(Rect::new(
                    window.x0,
                    bar.y1.max(window.y0),
                    window.x1,
                    window.y1,
                ))
            })
            .collect()
    }

    fn resize_at(
        edge_x: f64,
        side: PopoverSide,
        windows: Vec<Rect>,
        bars: Vec<Rect>,
        ink: Vec<Rect>,
    ) -> HintPlace {
        let edge = Rect::new(edge_x, 92.0, edge_x, 876.0);
        HintPlace {
            anchor: edge,
            side,
            edge: Some(Line::new((edge_x, 92.0), (edge_x, 876.0))),
            words: "Drag to resize".into(),
            keys: Vec::new(),
            points: true,
            along: edge,
            windows,
            bars,
            ink,
        }
    }

    fn covers_no_text_or_content(place_for: &HintPlace, card: Rect) {
        for ink in &place_for.ink {
            assert!(!card.overlaps(*ink), "{card:?} covers {ink:?}");
        }
        for body in bodies(&place_for.windows, &place_for.bars) {
            assert!(!card.overlaps(body), "{card:?} stands over {body:?}");
        }
    }

    /// Chat in front, its trailing edge at the screen's, with nothing past it. The pill stands on
    /// Chat's bar, clear of its tab and its window controls and every word Chat shows, and still
    /// points at the edge it teaches.
    #[test]
    fn a_resize_hint_at_the_screens_edge_covers_no_text_or_control() {
        let (windows, bars, ink) = side_by_side();
        let place_for = resize_at(1368.0, PopoverSide::Right, windows, bars, ink);
        let (card, notch) = place(&place_for, PILL, AREA, None).expect("Chat's bar has ground");
        covers_no_text_or_content(&place_for, card);
        let chat_bar = Rect::new(704.0, 36.0, 1368.0, 80.0);
        assert!(chat_bar.contains_rect(card), "on the bar: {card:?}");
        let (side, at) = notch.expect("it points at the edge");
        assert_eq!(side, PopoverSide::Right);
        assert!((card.y0..=card.y1).contains(&at), "{at} on {card:?}");
    }

    /// Wherever its edge, the pill never covers text, controls or a window's body; where it fits
    /// outside every window, it stands there.
    #[test]
    fn a_hints_pill_never_covers_a_windows_text_or_content() {
        let (windows, bars, ink) = side_by_side();
        for (edge_x, side) in [
            (696.0, PopoverSide::Right),
            (704.0, PopoverSide::Left),
            (1368.0, PopoverSide::Right),
            (32.0, PopoverSide::Left),
        ] {
            let spot = resize_at(edge_x, side, windows.clone(), bars.clone(), ink.clone());
            if let Some((card, _)) = place(&spot, PILL, AREA, None) {
                assert!(spare_holds(AREA, card), "{card:?} on the desktop");
                covers_no_text_or_content(&spot, card);
            }
        }
        // One window, narrower than the desktop: free ground past its trailing edge, outside it.
        let window = Rect::new(32.0, 36.0, 900.0, 888.0);
        let bar = Rect::new(32.0, 36.0, 900.0, 80.0);
        let ink = vec![
            Rect::new(40.0, 48.0, 180.0, 68.0),
            Rect::new(850.0, 48.0, 894.0, 68.0),
            Rect::new(48.0, 96.0, 880.0, 870.0),
        ];
        let (card, _) = place(
            &resize_at(
                900.0,
                PopoverSide::Right,
                vec![window],
                vec![bar],
                ink.clone(),
            ),
            PILL,
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
            &resize_at(900.0, PopoverSide::Left, vec![window], vec![bar], ink),
            PILL,
            AREA,
            None,
        )
        .expect("free ground past the edge, on the other side");
        assert!(card.x0 > 900.0, "flipped outside the window: {card:?}");
    }

    /// The move hint points at the stretch of bar that takes the window, and the pill stands on
    /// the bar's ground beside it: never on the stretch itself, never over the tab or the window
    /// controls, never over the window's content.
    #[test]
    fn a_move_hint_stands_on_the_bar_beside_the_stretch_that_takes_it() {
        let (windows, bars, ink) = side_by_side();
        let grab = Rect::new(300.0, 36.0, 340.0, 80.0);
        let spot = HintPlace {
            anchor: grab,
            side: PopoverSide::Below,
            edge: None,
            words: "Drag to move".into(),
            keys: Vec::new(),
            points: true,
            along: Rect::new(32.0, 36.0, 696.0, 80.0),
            windows,
            bars,
            ink,
        };
        let (card, notch) = place(&spot, PILL, AREA, None).expect("the bar has ground");
        covers_no_text_or_content(&spot, card);
        assert!(
            !card.overlaps(grab),
            "beside the stretch, not on it: {card:?}"
        );
        assert!(
            Rect::new(32.0, 36.0, 696.0, 80.0).contains_rect(card),
            "on the bar it teaches: {card:?}"
        );
        let (side, _) = notch.expect("it points at the stretch it teaches");
        assert!(matches!(side, PopoverSide::Left | PopoverSide::Right));
    }

    /// A window whose bar and body hold nothing free leaves the pill no spot: the hint does not
    /// stand there, rather than cover what the person reads.
    #[test]
    fn a_hint_with_no_free_spot_does_not_stand() {
        let (windows, bars, _) = side_by_side();
        let ink = windows.clone();
        assert_eq!(
            place(
                &resize_at(700.0, PopoverSide::Right, windows, bars, ink),
                PILL,
                AREA,
                None
            ),
            None
        );
    }

    /// A pill on screen keeps its spot while that stays free, rather than wander back toward
    /// the edge's middle each frame.
    #[test]
    fn a_pill_on_screen_keeps_its_free_spot() {
        let (windows, bars, ink) = side_by_side();
        let spot = resize_at(1368.0, PopoverSide::Right, windows, bars, ink);
        let (first, _) = place(&spot, PILL, AREA, None).expect("a free spot");
        let beside = first + Vec2::new(-2.0 * SLIDE_STEP, 0.0);
        assert!(
            Rect::new(704.0, 36.0, 1368.0, 80.0).contains_rect(beside),
            "fixture: the shifted spot is still on the bar: {beside:?}"
        );
        let (kept, _) = place(&spot, PILL, AREA, Some(beside)).expect("a free spot");
        assert_eq!(kept, beside);
    }

    /// A hint whose pill finds no free spot before it ever shows does not count as shown, and
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
        let (windows, bars, _) = side_by_side();
        let blocked = resize_at(
            700.0,
            PopoverSide::Right,
            windows.clone(),
            bars.clone(),
            windows.clone(),
        );
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
        let (_, _, ink) = side_by_side();
        let free = resize_at(1368.0, PopoverSide::Right, windows, bars, ink);
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
        area.contains_rect(card)
    }
}

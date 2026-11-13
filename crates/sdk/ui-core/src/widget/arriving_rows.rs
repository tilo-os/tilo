//! Rows that arrive one by one: the same placeholder steps as [`ArrivingView`](super::ArrivingView),
//! kept per row.

use std::collections::HashMap;
use std::hash::Hash;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::arriving::{Arrival, ArrivalClock, Source, Step};
use crate::motion::CrossFade;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::EffectiveMotion;
use crate::widget::{ArrivalFace, Element, Showing, SkeletonShape, TaffyTreeExt, Widget};

#[derive(Clone, Debug)]
struct RowArrival {
    clock: ArrivalClock,
    showing: Showing,
    fade: CrossFade<Showing>,
    replaced: Step,
}

impl RowArrival {
    fn pending(from: &Source, now: Instant) -> Self {
        Self {
            clock: ArrivalClock::new(now, from.clone()),
            showing: Showing::Placeholder,
            fade: CrossFade::new(Showing::Placeholder),
            replaced: Step::Ground,
        }
    }

    fn is_live(&self, now: Instant) -> bool {
        self.showing == Showing::Placeholder || self.fade.is_moving(now)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowFade {
    pub outgoing: Option<(Step, f32)>,
    pub incoming: f32,
}

#[derive(Clone, Debug)]
pub struct ArrivingRows<K> {
    slots: HashMap<K, usize>,
    rows: RowArrivals,
}

impl<K> Default for ArrivingRows<K> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            rows: RowArrivals::default(),
        }
    }
}

impl<K: Clone + Eq + Hash> ArrivingRows<K> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe<T, E>(
        &mut self,
        key: &K,
        arrival: &Arrival<T, E>,
        motion: Option<EffectiveMotion>,
        now: Instant,
    ) -> Showing {
        let slot = self.slots.get(key).copied();
        let (showing, added) = self.rows.observe(slot, Answer::of(arrival), motion, now);
        if let Some(added) = added {
            self.slots.insert(key.clone(), added);
        }
        showing
    }

    #[must_use]
    pub fn face(&self, key: &K, shape: SkeletonShape, now: Instant) -> Option<ArrivalFace<'_>> {
        self.rows.face(*self.slots.get(key)?, shape, now)
    }

    #[must_use]
    pub fn fade(&self, key: &K, now: Instant) -> Option<RowFade> {
        self.rows.fade(*self.slots.get(key)?, now)
    }

    #[must_use]
    pub fn outgoing_face(
        &self,
        key: &K,
        shape: SkeletonShape,
        now: Instant,
    ) -> Option<(ArrivalFace<'_>, f32)> {
        self.rows.outgoing_face(*self.slots.get(key)?, shape, now)
    }

    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.slots.contains_key(key)
    }

    pub fn paint(
        &self,
        key: &K,
        shape: SkeletonShape,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        now: Instant,
        content: &mut dyn FnMut(&mut PaintCtx<'_>),
    ) {
        match self.slots.get(key) {
            Some(&slot) => self.rows.paint(slot, shape, ctx, rect, now, content),
            None => content(ctx),
        }
    }

    /// Keep only the rows `keep` names, and of those only the ones still
    /// pending or fading in: a settled row needs no timing.
    pub fn retain(&mut self, now: Instant, mut keep: impl FnMut(&K) -> bool) {
        let rows = &mut self.rows;
        self.slots
            .retain(|key, slot| rows.keep(*slot, keep(key), now));
    }

    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.rows.is_moving(now)
    }

    #[must_use]
    pub fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.rows.next_wake_deadline(now)
    }

    #[must_use]
    pub fn reveal_due(&self, now: Instant) -> bool {
        self.rows.reveal_due(now)
    }

    pub fn mark_drawn(&mut self, now: Instant) {
        self.rows.drawn_at = Some(now);
    }

    #[must_use]
    pub fn needs_redraw(&self, now: Instant) -> bool {
        self.rows.needs_redraw(now)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[derive(Clone, Copy)]
enum Answer<'a> {
    Pending(&'a Source),
    Landed(Showing),
}

impl<'a> Answer<'a> {
    fn of<T, E>(arrival: &'a Arrival<T, E>) -> Self {
        match arrival {
            Arrival::Pending { from } => Self::Pending(from),
            Arrival::Ready(_) => Self::Landed(Showing::Answer),
            Arrival::Failed(_) => Self::Landed(Showing::Failure),
        }
    }
}

/// Every pending or fading row's timing, by slot. Not generic, so the
/// toolkit holds it once rather than each widget's key type a copy.
#[derive(Clone, Debug, Default)]
struct RowArrivals {
    rows: Vec<Option<RowArrival>>,
    free: Vec<usize>,
    drawn_at: Option<Instant>,
    /// The sources named in the frame painted at the instant it holds: the
    /// rows are one region, so each source is named on its first pending
    /// row painted, not on every row.
    named: std::cell::RefCell<(Option<Instant>, crate::widget::NamedOnce)>,
}

impl RowArrivals {
    fn row(&self, slot: usize) -> Option<&RowArrival> {
        self.rows.get(slot).and_then(Option::as_ref)
    }

    fn live(&self) -> impl Iterator<Item = &RowArrival> {
        self.rows.iter().flatten()
    }

    fn observe(
        &mut self,
        slot: Option<usize>,
        answer: Answer<'_>,
        motion: Option<EffectiveMotion>,
        now: Instant,
    ) -> (Showing, Option<usize>) {
        let landed = match answer {
            Answer::Pending(from) => {
                if let Some(row) = slot.and_then(|slot| self.rows.get_mut(slot)?.as_mut()) {
                    if row.clock.has_arrived() {
                        *row = RowArrival::pending(from, now);
                    }
                    return (Showing::Placeholder, None);
                }
                return (
                    Showing::Placeholder,
                    Some(self.add(RowArrival::pending(from, now))),
                );
            }
            Answer::Landed(landed) => landed,
        };
        // A row never seen pending was there from the start.
        let Some(row) = slot.and_then(|slot| self.rows.get_mut(slot)?.as_mut()) else {
            return (landed, None);
        };
        row.clock.arrive(now);
        if row.showing == Showing::Placeholder {
            if row.clock.shows_at().is_some_and(|at| now < at) {
                return (Showing::Placeholder, None);
            }
            row.replaced = row.clock.replaced_step();
            match motion {
                Some(motion) if row.clock.arrives_with_motion() => {
                    row.fade.arrive(landed, motion, now);
                }
                _ => row.fade = CrossFade::new(landed),
            }
        } else if row.showing != landed {
            row.fade = CrossFade::new(landed);
        }
        row.showing = landed;
        (landed, None)
    }

    fn add(&mut self, row: RowArrival) -> usize {
        if let Some(slot) = self.free.pop() {
            self.rows[slot] = Some(row);
            return slot;
        }
        self.rows.push(Some(row));
        self.rows.len() - 1
    }

    fn keep(&mut self, slot: usize, kept: bool, now: Instant) -> bool {
        let stays = kept && self.row(slot).is_some_and(|row| row.is_live(now));
        if !stays && let Some(row) = self.rows.get_mut(slot) {
            *row = None;
            self.free.push(slot);
        }
        stays
    }

    fn face(&self, slot: usize, shape: SkeletonShape, now: Instant) -> Option<ArrivalFace<'_>> {
        let row = self.row(slot)?;
        (row.showing == Showing::Placeholder).then(|| {
            let step = match row.clock.step(now) {
                // Due, but not shown until the widget looks again.
                Step::Shown => row.clock.replaced_step(),
                step => step,
            };
            self.named_once(ArrivalFace::at_step(shape, step, row.clock.source()), now)
        })
    }

    /// `face`, naming its source if no row painted before it at `now` did:
    /// a placeholder or the face an answer fades out, so the line that
    /// named the source stays as the answer lands.
    fn named_once<'a>(&self, face: ArrivalFace<'a>, now: Instant) -> ArrivalFace<'a> {
        let mut named = self.named.borrow_mut();
        if named.0 != Some(now) {
            *named = (Some(now), crate::widget::NamedOnce::default());
        }
        face.named_once(&mut named.1)
    }

    fn fade(&self, slot: usize, now: Instant) -> Option<RowFade> {
        let row = self.row(slot)?;
        if row.showing == Showing::Placeholder || !row.fade.is_moving(now) {
            return None;
        }
        Some(RowFade {
            outgoing: row
                .fade
                .outgoing(now)
                .map(|(_, opacity)| (row.replaced, opacity)),
            incoming: row.fade.incoming(now),
        })
    }

    fn outgoing_face(
        &self,
        slot: usize,
        shape: SkeletonShape,
        now: Instant,
    ) -> Option<(ArrivalFace<'_>, f32)> {
        let (step, opacity) = self.fade(slot, now)?.outgoing?;
        let row = self.row(slot)?;
        Some((
            self.named_once(ArrivalFace::at_step(shape, step, row.clock.source()), now),
            opacity,
        ))
    }

    fn paint(
        &self,
        slot: usize,
        shape: SkeletonShape,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        now: Instant,
        content: &mut dyn FnMut(&mut PaintCtx<'_>),
    ) {
        if let Some(face) = self.face(slot, shape, now) {
            face.paint(ctx, rect);
            return;
        }
        let Some(fade) = self.fade(slot, now) else {
            content(ctx);
            return;
        };
        if let Some((face, opacity)) = self.outgoing_face(slot, shape, now) {
            with_opacity(ctx, rect, opacity, &mut |ctx| face.paint(ctx, rect));
        }
        with_opacity(ctx, rect, fade.incoming, content);
    }

    fn is_moving(&self, now: Instant) -> bool {
        self.live().any(|row| row.fade.is_moving(now))
    }

    fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.live()
            .filter(|row| row.showing == Showing::Placeholder)
            .filter_map(|row| match row.clock.shows_at() {
                Some(at) => Some(at.max(now)),
                None => row.clock.next_step_at(now),
            })
            .min()
    }

    fn reveal_due(&self, now: Instant) -> bool {
        self.live().any(|row| {
            row.showing == Showing::Placeholder && row.clock.shows_at().is_some_and(|at| now >= at)
        })
    }

    fn needs_redraw(&self, now: Instant) -> bool {
        if self.is_moving(now) {
            return true;
        }
        match self.drawn_at {
            Some(drawn) => self
                .next_wake_deadline(drawn)
                .is_some_and(|at| at > drawn && at <= now),
            None => self.live().next().is_some(),
        }
    }
}

/// Paint `paint` at `opacity`, in a layer only while it is part-way.
fn with_opacity(
    ctx: &mut PaintCtx<'_>,
    clip: Rect,
    opacity: f32,
    paint: &mut dyn FnMut(&mut PaintCtx<'_>),
) {
    if opacity <= 0.0 {
        return;
    }
    let part = opacity < 1.0;
    if part {
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            ctx.transform,
            &clip.inflate(2.0, 2.0),
        );
    }
    paint(ctx);
    if part {
        ctx.scene.pop_layer();
    }
}

pub(crate) struct RowReserve {
    height: f32,
    rect: Rect,
}

impl RowReserve {
    pub(crate) fn element(height: f64) -> Element {
        Box::new(Self {
            height: crate::num::f32_of_f64(height.max(0.0)),
            rect: Rect::ZERO,
        })
    }
}

impl Widget for RowReserve {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: length(self.height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "RowReserve"
    }
}

#[cfg(test)]
#[path = "arriving_rows_tests.rs"]
mod tests;

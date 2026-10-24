//! Rows that arrive one by one: the same placeholder steps as
//! [`ArrivingView`](super::ArrivingView), kept per row.
//!
//! A list, table or tree whose model answers a row as
//! [`Arrival::Pending`] reserves that row's slot and paints its placeholder
//! there: the ground until [`GRACE`](crate::arriving::GRACE), then a
//! skeleton that stays at least [`MIN_SHOWN`](crate::arriving::MIN_SHOWN),
//! naming where the row comes from after
//! [`NAME_SOURCE_AFTER`](crate::arriving::NAME_SOURCE_AFTER). The row's
//! content cross-fades into its slot when it lands, and no other row moves.
//! [`ArrivingRows`] keeps that timing for each pending row; the widget asks
//! it what a row shows and when it next changes, so a pending row costs no
//! frames between its steps.

use std::collections::HashMap;
use std::hash::Hash;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::arriving::{Arrival, ArrivalClock, Step};
use crate::motion::CrossFade;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::EffectiveMotion;
use crate::widget::{ArrivalFace, Element, Showing, SkeletonShape, TaffyTreeExt, Widget};

/// One row's arrival: its timing, and its fade once it landed.
#[derive(Clone, Debug)]
struct RowArrival {
    clock: ArrivalClock,
    showing: Showing,
    fade: CrossFade<Showing>,
    /// The placeholder step its content replaced: what fades out.
    replaced: Step,
}

/// How a landed row fades in: the placeholder step it replaced at its
/// outgoing opacity, and its content's opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowFade {
    pub outgoing: Option<(Step, f32)>,
    pub incoming: f32,
}

/// The arrival timing of a widget's pending rows, by row key.
#[derive(Clone, Debug)]
pub struct ArrivingRows<K> {
    rows: HashMap<K, RowArrival>,
    /// When the widget last drew what these rows show.
    drawn_at: Option<Instant>,
}

impl<K> Default for ArrivingRows<K> {
    fn default() -> Self {
        Self {
            rows: HashMap::new(),
            drawn_at: None,
        }
    }
}

impl<K: Clone + Eq + Hash> ArrivingRows<K> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What row `key` shows at `now`, its model answering `arrival`. A row
    /// first seen pending starts its timing now; a row that lands shows
    /// once its skeleton stood its minimum, fading in under `motion` (none
    /// when the widget has no theme yet: it shows at once).
    pub fn observe<T, E>(
        &mut self,
        key: &K,
        arrival: &Arrival<T, E>,
        motion: Option<EffectiveMotion>,
        now: Instant,
    ) -> Showing {
        let landed = match arrival {
            Arrival::Pending { from } => {
                let row = self.rows.entry(key.clone()).or_insert_with(|| RowArrival {
                    clock: ArrivalClock::new(now, from.clone()),
                    showing: Showing::Placeholder,
                    fade: CrossFade::new(Showing::Placeholder),
                    replaced: Step::Ground,
                });
                // Asked again (a retry, or a row refreshed after it showed):
                // its placeholder starts over.
                if row.clock.has_arrived() {
                    *row = RowArrival {
                        clock: ArrivalClock::new(now, from.clone()),
                        showing: Showing::Placeholder,
                        fade: CrossFade::new(Showing::Placeholder),
                        replaced: Step::Ground,
                    };
                }
                return Showing::Placeholder;
            }
            Arrival::Ready(_) => Showing::Answer,
            Arrival::Failed(_) => Showing::Failure,
        };
        // A row never seen pending was there from the start.
        let Some(row) = self.rows.get_mut(key) else {
            return landed;
        };
        row.clock.arrive(now);
        if row.showing == Showing::Placeholder {
            if row.clock.shows_at().is_some_and(|at| now < at) {
                return Showing::Placeholder;
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
        landed
    }

    /// The placeholder row `key` paints at `now` in `shape`, while it
    /// shows one.
    #[must_use]
    pub fn face(&self, key: &K, shape: SkeletonShape, now: Instant) -> Option<ArrivalFace<'_>> {
        let row = self.rows.get(key)?;
        (row.showing == Showing::Placeholder).then(|| {
            let step = match row.clock.step(now) {
                // Due, but not shown until the widget looks again.
                Step::Shown => row.clock.replaced_step(),
                step => step,
            };
            ArrivalFace::at_step(shape, step, row.clock.source())
        })
    }

    /// How row `key` fades in at `now`, while it does.
    #[must_use]
    pub fn fade(&self, key: &K, now: Instant) -> Option<RowFade> {
        let row = self.rows.get(key)?;
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

    /// The placeholder step a landed row's content replaced, at the
    /// opacity it fades out with at `now`, while it does.
    #[must_use]
    pub fn outgoing_face(
        &self,
        key: &K,
        shape: SkeletonShape,
        now: Instant,
    ) -> Option<(ArrivalFace<'_>, f32)> {
        let (step, opacity) = self.fade(key, now)?.outgoing?;
        let row = self.rows.get(key)?;
        Some((
            ArrivalFace::at_step(shape, step, row.clock.source()),
            opacity,
        ))
    }

    /// Whether row `key` is pending or fading in.
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.rows.contains_key(key)
    }

    /// Paint what row `key` shows over its slot `rect` at `now`, with
    /// `content` drawing the row's own widget: the placeholder, or the
    /// content fading in over the step it replaced, or just the content.
    pub fn paint(
        &self,
        key: &K,
        shape: SkeletonShape,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        now: Instant,
        content: impl FnOnce(&mut PaintCtx<'_>),
    ) {
        if let Some(face) = self.face(key, shape, now) {
            face.paint(ctx, rect);
            return;
        }
        let Some(fade) = self.fade(key, now) else {
            content(ctx);
            return;
        };
        if let Some((face, opacity)) = self.outgoing_face(key, shape, now) {
            with_opacity(ctx, rect, opacity, |ctx| face.paint(ctx, rect));
        }
        with_opacity(ctx, rect, fade.incoming, content);
    }

    /// Keep only the rows `keep` names, and of those only the ones still
    /// pending or fading in: a settled row needs no timing.
    pub fn retain(&mut self, now: Instant, mut keep: impl FnMut(&K) -> bool) {
        self.rows.retain(|key, row| {
            keep(key) && (row.showing == Showing::Placeholder || row.fade.is_moving(now))
        });
    }

    /// Whether a row is fading in at `now`.
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.rows.values().any(|row| row.fade.is_moving(now))
    }

    /// The next instant after `now` at which a placeholder changes: its next
    /// step, or the content that landed showing once its skeleton stood its
    /// minimum.
    #[must_use]
    pub fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.rows
            .values()
            .filter(|row| row.showing == Showing::Placeholder)
            .filter_map(|row| match row.clock.shows_at() {
                Some(at) => Some(at.max(now)),
                None => row.clock.next_step_at(now),
            })
            .min()
    }

    /// Whether a row is waiting to show content that landed: the widget
    /// looks at its model again by `now`.
    #[must_use]
    pub fn reveal_due(&self, now: Instant) -> bool {
        self.rows.values().any(|row| {
            row.showing == Showing::Placeholder && row.clock.shows_at().is_some_and(|at| now >= at)
        })
    }

    /// The widget drew what the rows show at `now`.
    pub fn mark_drawn(&mut self, now: Instant) {
        self.drawn_at = Some(now);
    }

    /// Whether what the rows show changed since the widget last drew them.
    #[must_use]
    pub fn needs_redraw(&self, now: Instant) -> bool {
        if self.is_moving(now) {
            return true;
        }
        match self.drawn_at {
            Some(drawn) => self
                .next_wake_deadline(drawn)
                .is_some_and(|at| at > drawn && at <= now),
            None => !self.rows.is_empty(),
        }
    }

    /// Whether any row is pending or fading in.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Paint `paint` at `opacity`, in a layer only while it is part-way.
fn with_opacity(
    ctx: &mut PaintCtx<'_>,
    clip: Rect,
    opacity: f32,
    paint: impl FnOnce(&mut PaintCtx<'_>),
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

/// A pending row's slot: as tall as the row it stands for, and empty. Its
/// widget paints the placeholder steps over it.
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

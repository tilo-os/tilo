//! The rows of a virtualized collection on their way after a structural
//! change. Owners note only their shown rows: survivors move from where
//! they stood, arrivals fade in, and (for an owner that paints them) the
//! rows the change took away fade out where they stood. Once the change has
//! run its course every row stands where it is laid out.

use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// A row on its way after a change the person made (`motion::ListChange`):
/// moving from where it stood to its new place, or fading in where it
/// arrived. Rows the change did not move have none.
#[derive(Clone, Copy, Debug)]
pub struct RowMotion {
    /// How far below its laid-out place it is drawn (negative: above).
    offset: crate::motion::MotionValue<f64>,
    /// Its opacity.
    fade: crate::motion::MotionValue<f64>,
}

impl RowMotion {
    fn rest() -> Self {
        Self {
            offset: crate::motion::MotionValue::new(0.0).with_tolerance(0.25),
            fade: crate::motion::MotionValue::new(1.0),
        }
    }

    /// How far below its laid-out place it is drawn at `now`.
    #[must_use]
    pub fn offset(&self, now: Instant) -> f64 {
        self.offset.get(now)
    }

    /// Its opacity at `now`.
    #[must_use]
    pub fn opacity(&self, now: Instant) -> f32 {
        crate::motion::unit_opacity(self.fade.get(now))
    }

    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.offset.is_moving(now) || self.fade.is_moving(now)
    }

    fn settles_at(&self) -> Option<Instant> {
        self.offset.settles_at().max(self.fade.settles_at())
    }
}

/// Where the shown rows stood just before a structural change the person
/// made, by key, for the next frame to move them from.
struct PendingRows<K> {
    before: Vec<(K, f64)>,
    /// Rows that stood nowhere shown before fade in where they arrive.
    arrivals_fade: bool,
    source: MotionSource,
}

/// The rows of a virtualized list or table on their way after a change the
/// person just made (`motion::ListChange`): the owner notes where its shown
/// rows stand before the change ([`Self::capture`]), and the next frame
/// moves the survivors from there to where they are laid out now and fades
/// arrivals in ([`Self::start`]). Only rows shown after the change move;
/// nothing outside the viewport animates. A change nobody just asked for (a
/// sync), a shortcut's result and reduced motion land at once.
pub struct RowMotions<K> {
    rows: std::collections::HashMap<K, RowMotion>,
    pending: Option<PendingRows<K>>,
    /// Rows the change took out of the shown ones, fading out where they
    /// stood, for an owner that paints them ([`Self::fading_out_leavers`]).
    leaving: Option<Vec<LeavingRow<K>>>,
}

/// A row a change took away, fading out where it stood.
struct LeavingRow<K> {
    key: K,
    top: f64,
    fade: crate::motion::MotionValue<f64>,
}

impl<K> Default for RowMotions<K> {
    fn default() -> Self {
        Self {
            rows: std::collections::HashMap::new(),
            pending: None,
            leaving: None,
        }
    }
}

impl<K: Clone + Eq + std::hash::Hash> RowMotions<K> {
    /// Rows a change takes out of the shown ones fade out where they stood
    /// ([`Self::leaving`]): an owner that keeps what they showed paints
    /// them, as a tree does the rows a collapse folds away.
    #[must_use]
    pub fn fading_out_leavers(mut self) -> Self {
        self.leaving = Some(Vec::new());
        self
    }

    /// The input a change happening at `now` moves for: the person's
    /// pointer or finger, a few hundred milliseconds ago. `None` when nobody
    /// just made it, when a key or an action did (its result lands), or
    /// under reduced motion (a list is read, not watched).
    #[must_use]
    pub fn caused_by_hand(now: Instant, reduce_motion: bool) -> Option<MotionSource> {
        if reduce_motion {
            return None;
        }
        crate::motion::recent_input(now).filter(|source| !source.lands_at_once())
    }

    /// Before a structural change: note where the shown rows stand
    /// (`before`, each row's top). With no `source` (see
    /// [`Self::caused_by_hand`]) the change lands at once. Two changes
    /// before a frame move the rows from where the first found them.
    pub fn capture(
        &mut self,
        before: impl IntoIterator<Item = (K, f64)>,
        arrivals_fade: bool,
        source: Option<MotionSource>,
    ) {
        let Some(source) = source else {
            self.pending = None;
            return;
        };
        if let Some(pending) = self.pending.as_mut() {
            pending.arrivals_fade |= arrivals_fade;
            return;
        }
        self.pending = Some(PendingRows {
            before: before.into_iter().collect(),
            arrivals_fade,
            source,
        });
    }

    /// Forget a change noted but not started: it lands at once.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Whether a change waits for the next frame to start moving.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Start the motion of a change noted before this frame, for the rows
    /// shown now (`shown`, each row's laid-out top), and forget rows that
    /// have arrived. A row already moving continues from where it is drawn.
    /// Asks the frame pump for frames until the last one rests.
    pub fn start(
        &mut self,
        shown: impl IntoIterator<Item = (K, f64)>,
        motion: EffectiveMotion,
        now: Instant,
    ) {
        use crate::motion::{Direction, ListChange};
        if let Some(change) = self.pending.take() {
            let shift = ListChange::MOVE.timing(Direction::Enter, motion, change.source);
            let fade = ListChange::FADE.timing(Direction::Enter, motion, change.source);
            let shown: Vec<(K, f64)> = shown.into_iter().collect();
            if let Some(leaving) = self.leaving.as_mut() {
                let fade_out = ListChange::FADE.timing(Direction::Exit, motion, change.source);
                let gone = change
                    .before
                    .iter()
                    .filter(|(key, _)| !shown.iter().any(|(now_shown, _)| now_shown == key));
                for (key, top) in gone {
                    let mut fade = crate::motion::MotionValue::new(1.0);
                    fade.set(0.0, fade_out, now);
                    leaving.retain(|row| row.key != *key);
                    leaving.push(LeavingRow {
                        key: key.clone(),
                        top: *top,
                        fade,
                    });
                }
            }
            for (key, top) in shown {
                let was = change
                    .before
                    .iter()
                    .find(|(before, _)| *before == key)
                    .map(|(_, was)| *was);
                match was {
                    Some(was) if (was - top).abs() > 0.5 => {
                        let entry = self.rows.entry(key).or_insert_with(RowMotion::rest);
                        let drawn = entry.offset.get(now);
                        entry.offset.snap(drawn + (was - top));
                        entry.offset.set(0.0, shift, now);
                    }
                    None if change.arrivals_fade => {
                        let entry = self.rows.entry(key).or_insert_with(RowMotion::rest);
                        entry.fade.snap(0.0);
                        entry.fade.set(1.0, fade, now);
                    }
                    _ => {}
                }
            }
            let leaving = self.leaving.iter().flatten();
            if let Some(until) = (self.rows.values().filter_map(RowMotion::settles_at))
                .chain(leaving.filter_map(|row| row.fade.settles_at()))
                .max()
            {
                crate::motion::want_frames_until(until);
            }
        }
        self.settle(now);
    }

    /// Forget the rows that have arrived and the ones that have faded out.
    pub fn settle(&mut self, now: Instant) {
        self.rows.retain(|_, row| row.is_moving(now));
        if let Some(leaving) = self.leaving.as_mut() {
            leaving.retain(|row| row.fade.is_moving(now));
        }
    }

    /// The rows fading out where they stood at `now`: each one's key, its
    /// top before the change and its opacity.
    pub fn leaving(&self, now: Instant) -> impl Iterator<Item = (&K, f64, f32)> {
        self.leaving
            .iter()
            .flatten()
            .filter(move |row| row.fade.is_moving(now))
            .map(move |row| {
                let opacity = crate::motion::unit_opacity(row.fade.get(now));
                (&row.key, row.top, opacity)
            })
    }

    /// Forget every row's motion: they all stand where they are laid out.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.pending = None;
        if let Some(leaving) = self.leaving.as_mut() {
            leaving.clear();
        }
    }

    /// The motion of the row keyed `key`, while it has one.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&RowMotion> {
        self.rows.get(key)
    }

    /// Where the row keyed `key` is drawn at `now`: how far below its
    /// laid-out place, and its opacity. `(0, 1)` for a row at rest.
    #[must_use]
    pub fn pose(&self, key: &K, now: Instant) -> (f64, f32) {
        self.rows
            .get(key)
            .map_or((0.0, 1.0), |row| (row.offset(now), row.opacity(now)))
    }

    /// Whether no row is on its way.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.leaving.iter().flatten().next().is_none()
    }

    /// Whether any row is still moving at `now` (its owner asks for frames).
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.rows.values().any(|row| row.is_moving(now))
            || self
                .leaving
                .iter()
                .flatten()
                .any(|row| row.fade.is_moving(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;

    fn motion(reduced: bool) -> EffectiveMotion {
        EffectiveMotion::new(crate::tokens::Motion::default(), reduced)
    }

    #[test]
    fn rows_move_arrivals_fade_in_and_leavers_fade_out_unless_motion_is_reduced() {
        let now = crate::time::now();
        let mut rows = RowMotions::default().fading_out_leavers();
        rows.capture([(1, 0.0), (2, 20.0)], true, Some(MotionSource::Pointer));
        rows.start([(1, 40.0), (3, 20.0)], motion(false), now);
        let middle = now + Duration::from_millis(40);
        let (offset, opacity) = rows.pose(&1, middle);
        assert!(offset < 0.0 && offset > -40.0, "{offset}");
        assert_eq!(opacity, 1.0);
        let arriving = rows.pose(&3, middle).1;
        assert!(arriving > 0.0 && arriving < 1.0, "{arriving}");
        let leaving = rows.leaving(middle).collect::<Vec<_>>();
        assert_eq!(leaving.len(), 1);
        assert_eq!((*leaving[0].0, leaving[0].1), (2, 20.0));
        assert!(leaving[0].2 > 0.0 && leaving[0].2 < 1.0);
        let done = now + Duration::from_millis(500);
        assert_eq!(rows.pose(&1, done), (0.0, 1.0));
        assert_eq!(rows.leaving(done).count(), 0);
        rows.settle(done);
        assert!(rows.is_empty(), "settled rows are gone");

        // Under reduced motion nobody's hand moves the rows: they land.
        let mut reduced = RowMotions::default().fading_out_leavers();
        let source = RowMotions::<i32>::caused_by_hand(now, true);
        reduced.capture([(1, 0.0)], true, source);
        reduced.start([(2, 0.0)], motion(true), now);
        assert_eq!(reduced.pose(&2, now), (0.0, 1.0));
        assert_eq!(reduced.leaving(now).count(), 0);
        assert!(!reduced.is_moving(now));
    }
}

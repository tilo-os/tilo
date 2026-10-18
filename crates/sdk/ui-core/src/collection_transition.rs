//! Bounded keyed transition snapshots for virtualized collections.
//!
//! Owners snapshot only their visible window plus overscan. Reconciliation
//! then animates surviving keys between geometries, fades arriving keys in,
//! and retains the old payload for departing-row paint ghosts.
//!
//! A transition only moves rows toward where they are now: a surviving or
//! arriving row ends at the geometry its owner passes to
//! [`KeyedTransitionSet::visual`] (a row measured after the snapshot was
//! taken still lands at its measured height), and once the transition has
//! run its course every row stands where it is laid out and the finished
//! tracks are dropped ([`KeyedTransitionSet::settle`]).

use crate::motion::{Direction, ListChange};
use crate::time::{Duration, Instant};
use crate::tokens::{EffectiveMotion, MotionSource};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowGeometry {
    pub top: f64,
    pub height: f64,
}

impl RowGeometry {
    pub fn new(top: f64, height: f64) -> Self {
        Self {
            top,
            height: height.max(0.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyedRowSnapshot<K, P> {
    pub key: K,
    pub geometry: RowGeometry,
    pub payload: P,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionVisual {
    pub geometry: RowGeometry,
    pub opacity: f32,
}

#[derive(Clone, Debug)]
struct Track<K, P> {
    key: K,
    from: RowGeometry,
    to: Option<RowGeometry>,
    payload: P,
    entering: bool,
}

/// One active structural transition over a bounded set of visible keys, on
/// the standard list change (`motion::ListChange`): survivors move on its
/// move, arrivals and departures fade on its fade.
pub struct KeyedTransitionSet<K, P> {
    tracks: Vec<Track<K, P>>,
    started: Instant,
    motion: EffectiveMotion,
    source: MotionSource,
    /// How long the whole change runs: the longer of its move and fades.
    duration: Duration,
}

impl<K, P> Default for KeyedTransitionSet<K, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, P> KeyedTransitionSet<K, P> {
    pub fn new() -> Self {
        Self {
            tracks: Vec::new(),
            started: crate::time::now(),
            motion: EffectiveMotion::default(),
            source: MotionSource::Pointer,
            duration: Duration::ZERO,
        }
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
    }

    #[must_use]
    pub fn is_active(&self, now: Instant) -> bool {
        !self.tracks.is_empty() && now.saturating_duration_since(self.started) < self.duration
    }

    /// Drop every track once the transition has run its course, so nothing
    /// is held at a snapshot's geometry afterwards.
    pub fn settle(&mut self, now: Instant) {
        if !self.tracks.is_empty() && !self.is_active(now) {
            self.tracks.clear();
        }
    }

    /// Compare two bounded snapshots. Reduced motion and a change a
    /// shortcut made land at once, by clearing all tracks.
    pub fn reconcile(
        &mut self,
        old: &[KeyedRowSnapshot<K, P>],
        new: &[KeyedRowSnapshot<K, P>],
        now: Instant,
        motion: EffectiveMotion,
        source: MotionSource,
    ) where
        K: Clone + PartialEq,
        P: Clone,
    {
        self.clear();
        let duration = [
            ListChange::MOVE.settle_time(Direction::Enter, motion, source),
            ListChange::FADE.settle_time(Direction::Enter, motion, source),
            ListChange::FADE.settle_time(Direction::Exit, motion, source),
        ]
        .into_iter()
        .max()
        .unwrap_or(Duration::ZERO);
        if motion.reduced() || duration.is_zero() || old.is_empty() {
            return;
        }
        self.started = now;
        self.motion = motion;
        self.source = source;
        self.duration = duration;
        for row in new {
            if let Some(previous) = old.iter().find(|previous| previous.key == row.key) {
                if previous.geometry != row.geometry {
                    self.tracks.push(Track {
                        key: row.key.clone(),
                        from: previous.geometry,
                        to: Some(row.geometry),
                        payload: row.payload.clone(),
                        entering: false,
                    });
                }
            } else {
                self.tracks.push(Track {
                    key: row.key.clone(),
                    from: row.geometry,
                    to: Some(row.geometry),
                    payload: row.payload.clone(),
                    entering: true,
                });
            }
        }
        for row in old {
            if !new.iter().any(|next| next.key == row.key) {
                self.tracks.push(Track {
                    key: row.key.clone(),
                    from: row.geometry,
                    to: None,
                    payload: row.payload.clone(),
                    entering: false,
                });
            }
        }
    }

    #[must_use]
    pub fn visual(&self, key: &K, settled: RowGeometry, now: Instant) -> TransitionVisual
    where
        K: PartialEq,
    {
        let Some(track) = self
            .tracks
            .iter()
            .find(|track| track.to.is_some() && &track.key == key)
        else {
            return TransitionVisual {
                geometry: settled,
                opacity: 1.0,
            };
        };
        if !self.is_active(now) {
            return TransitionVisual {
                geometry: settled,
                opacity: 1.0,
            };
        }
        // Toward where the row stands now, not where it stood when the
        // snapshot was taken: a row measured since lands at its height.
        let from = if track.entering { settled } else { track.from };
        TransitionVisual {
            geometry: interpolate(
                from,
                settled,
                self.progress(ListChange::MOVE, Direction::Enter, now),
            ),
            opacity: if track.entering {
                crate::motion::unit_opacity(self.progress(ListChange::FADE, Direction::Enter, now))
            } else {
                1.0
            },
        }
    }

    pub fn exiting(&self, now: Instant) -> impl Iterator<Item = (&K, &P, TransitionVisual)> {
        let progress =
            crate::motion::unit_opacity(self.progress(ListChange::FADE, Direction::Exit, now));
        let active = self.is_active(now);
        self.tracks
            .iter()
            .filter(move |track| active && track.to.is_none())
            .map(move |track| {
                (
                    &track.key,
                    &track.payload,
                    TransitionVisual {
                        geometry: track.from,
                        opacity: 1.0 - progress,
                    },
                )
            })
    }

    fn progress(&self, spec: crate::motion::MotionSpec, direction: Direction, now: Instant) -> f64 {
        spec.progress_after(
            direction,
            self.motion,
            self.source,
            now.saturating_duration_since(self.started),
        )
    }
}

fn interpolate(from: RowGeometry, to: RowGeometry, progress: f64) -> RowGeometry {
    RowGeometry {
        top: from.top + (to.top - from.top) * progress,
        height: from.height + (to.height - from.height) * progress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: u64, top: f64) -> KeyedRowSnapshot<u64, &'static str> {
        KeyedRowSnapshot {
            key,
            geometry: RowGeometry::new(top, 20.0),
            payload: "row",
        }
    }

    fn motion(reduced: bool) -> EffectiveMotion {
        EffectiveMotion::new(crate::tokens::Motion::default(), reduced)
    }

    #[test]
    fn finished_transitions_stand_rows_where_they_are_laid_out() {
        let now = crate::time::now();
        let mut set = KeyedTransitionSet::new();
        // A row captured at an estimate before it was measured.
        set.reconcile(
            &[row(1, 0.0), row(2, 20.0)],
            &[row(1, 40.0), row(3, 20.0)],
            now,
            motion(false),
            MotionSource::Pointer,
        );
        let measured = RowGeometry::new(40.0, 32.0);
        // Mid-way the row moves toward its measured geometry, not the
        // snapshot's: top and height are the same share of the way there.
        let middle = set.visual(&1, measured, now + Duration::from_millis(60));
        let moved = middle.geometry.top / 40.0;
        assert!(moved > 0.0 && moved < 1.0, "{moved}");
        assert!(((middle.geometry.height - 20.0) / 12.0 - moved).abs() < 1e-9);
        // Once the transition has run its course it stands where it is laid
        // out, and the finished tracks are dropped.
        let done = now + Duration::from_millis(500);
        assert_eq!(set.visual(&1, measured, done).geometry, measured);
        assert_eq!(set.visual(&3, measured, done).opacity, 1.0);
        assert_eq!(set.exiting(done).count(), 0);
        set.settle(done);
        assert!(!set.is_active(done));
        assert_eq!(set.exiting(now).count(), 0, "settled tracks are gone");
    }

    #[test]
    fn collection_transitions_move_fade_and_honour_reduced_motion() {
        // Case `surviving_key_moves_between_old_and_new_geometry`.
        {
            let now = crate::time::now();
            let mut set = KeyedTransitionSet::new();
            set.reconcile(
                &[row(1, 0.0)],
                &[row(1, 40.0)],
                now,
                motion(false),
                MotionSource::Pointer,
            );
            let visual = set.visual(
                &1,
                RowGeometry::new(40.0, 20.0),
                now + Duration::from_millis(60),
            );
            assert!(visual.geometry.top > 0.0 && visual.geometry.top < 40.0);
            assert_eq!(visual.opacity, 1.0);
        }

        // Case `arrivals_fade_in_and_departures_retain_payload`.
        {
            let now = crate::time::now();
            let mut set = KeyedTransitionSet::new();
            set.reconcile(
                &[row(1, 0.0)],
                &[row(2, 0.0)],
                now,
                motion(false),
                MotionSource::Pointer,
            );
            let middle = now + Duration::from_millis(40);
            let arriving = set.visual(&2, RowGeometry::new(0.0, 20.0), middle).opacity;
            assert!(arriving > 0.0 && arriving < 1.0, "{arriving}");
            let ghosts = set.exiting(middle).collect::<Vec<_>>();
            assert_eq!(ghosts.len(), 1);
            assert_eq!(*ghosts[0].0, 1);
            assert_eq!(*ghosts[0].1, "row");
            assert!(ghosts[0].2.opacity > 0.0 && ghosts[0].2.opacity < 1.0);
        }

        // Case `reduced_motion_has_no_active_or_ghost_state`, and the same
        // for a change a shortcut made.
        for (reduced, source) in [
            (true, MotionSource::Pointer),
            (false, MotionSource::Keyboard),
        ] {
            let now = crate::time::now();
            let mut set = KeyedTransitionSet::new();
            set.reconcile(&[row(1, 0.0)], &[row(2, 0.0)], now, motion(reduced), source);
            assert!(!set.is_active(now));
            assert_eq!(set.exiting(now).count(), 0);
            assert_eq!(
                set.visual(&2, RowGeometry::new(0.0, 20.0), now),
                TransitionVisual {
                    geometry: RowGeometry::new(0.0, 20.0),
                    opacity: 1.0,
                }
            );
        }
    }
}

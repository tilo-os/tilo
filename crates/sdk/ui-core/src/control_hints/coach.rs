//! When a control hint shows, goes, and is learned: a pure schedule over
//! what the host observed, with no drawing and no clock of its own.

use super::{Cue, HintSpec, Learned, Quiet};
use crate::time::{Duration, Instant};

/// How long an unanswered hint stays before it goes on its own.
pub const IGNORED_AFTER: Duration = Duration::from_secs(9);
/// The least time between two hints beginning to show.
pub const MIN_GAP: Duration = Duration::from_secs(60);
/// How long after it went unanswered a hint may come back, once, in the
/// same session. A later session may bring it back, once, at its moment.
pub const RETURN_AFTER: Duration = Duration::from_secs(10 * 60);
/// How long a moment a host saw ([`Cue::Saw`]) stays the moment to show a
/// hint: one that comes after this is no longer about what just happened.
pub const CUE_FRESH: Duration = Duration::from_secs(20);
/// The most times one hint shows, ever.
pub const MAX_SHOWINGS: u8 = 2;

/// Where the hint on screen is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Arriving, then resting at its anchor, since this instant.
    Shown(Instant),
    /// Going away unanswered (ignored, or the moment passed), since this
    /// instant.
    Leaving(Instant),
    /// Its gesture was done: the acknowledgement, since this instant.
    Done(Instant),
}

/// The hint on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Current {
    /// Its place in the registered set.
    pub index: usize,
    pub phase: Phase,
}

/// Per hint, this session.
#[derive(Clone, Copy, Debug, Default)]
struct Track {
    /// When its moment last came ([`Cue::Saw`]).
    cued: Option<Instant>,
    /// When it last went unanswered in this session.
    left: Option<Instant>,
}

/// The schedule of a set of hints over the signals `S` a host observes.
///
/// A host feeds it what the person did ([`Self::observe`]) and, every
/// frame, what holds and whether now is a quiet moment ([`Self::tick`]).
/// It answers which hint to show ([`Self::current`]) and what was learned
/// or shown that the person's own record should keep ([`Self::learned`]).
pub struct Coach<S: 'static> {
    specs: &'static [HintSpec<S>],
    tracks: Vec<Track>,
    learned: Learned,
    current: Option<Current>,
    started: Option<Instant>,
    last_began: Option<Instant>,
    /// The record changed since the host last took it.
    dirty: bool,
}

impl<S: Copy + PartialEq + 'static> Coach<S> {
    #[must_use]
    pub fn new(specs: &'static [HintSpec<S>], learned: Learned) -> Self {
        Self {
            specs,
            tracks: vec![Track::default(); specs.len()],
            learned,
            current: None,
            started: None,
            last_began: None,
            dirty: false,
        }
    }

    /// The registered hints.
    #[must_use]
    pub fn specs(&self) -> &'static [HintSpec<S>] {
        self.specs
    }

    /// The hint on screen, if any.
    #[must_use]
    pub fn current(&self) -> Option<Current> {
        self.current
    }

    /// What the person's record keeps: which hints are learned, how often
    /// each showed, and whether hints show at all.
    #[must_use]
    pub fn learned(&self) -> &Learned {
        &self.learned
    }

    /// The record, when it changed since the last call: the host writes
    /// it to the person's own data.
    pub fn take_changed(&mut self) -> Option<Learned> {
        std::mem::take(&mut self.dirty).then(|| self.learned.clone())
    }

    /// Fold in the record as another device left it: a hint learned
    /// anywhere is learned here, and a hint on screen that the other device
    /// learned goes.
    pub fn merge(&mut self, other: &Learned, now: Instant) {
        let before = self.learned.clone();
        self.learned.merge(other);
        if self.learned != before {
            // What the merge added is the other device's; only what this
            // one adds on top needs writing back.
            self.dirty |= self.learned != *other;
        }
        self.retire_unwanted(now);
    }

    /// Turn hints on or off (the person's one setting).
    pub fn set_enabled(&mut self, enabled: bool, now: Instant) {
        if self.learned.enabled != enabled {
            self.learned.enabled = enabled;
            self.dirty = true;
        }
        self.retire_unwanted(now);
    }

    /// The person did `signal`. Every hint it teaches is learned, shown or
    /// not: one on screen acknowledges it, one not yet shown never will.
    /// A hint whose moment it is ([`Cue::Saw`]) remembers that.
    pub fn observe(&mut self, signal: S, now: Instant) {
        for (index, spec) in self.specs.iter().enumerate() {
            if spec.learned_by.contains(&signal) && !self.learned.knows(spec.id) {
                self.learned.learn(spec.id);
                self.dirty = true;
                if let Some(current) = self.current.as_mut()
                    && current.index == index
                    && matches!(current.phase, Phase::Shown(_))
                {
                    current.phase = Phase::Done(now);
                }
            }
            if spec.when.contains(&Cue::Saw(signal))
                && let Some(track) = self.tracks.get_mut(index)
            {
                track.cued = Some(now);
            }
        }
    }

    /// Advance to `now`. `holds` says whether a fact holds right now;
    /// `quiet` is why no hint may begin now, if anything; `anchored` whether
    /// the host can place a hint at all (its anchor is on screen); the
    /// phases' lengths come from `timing`.
    pub fn tick(
        &mut self,
        now: Instant,
        holds: &dyn Fn(S) -> bool,
        quiet: Option<Quiet>,
        anchored: &dyn Fn(usize) -> bool,
        timing: Timing,
    ) {
        let started = *self.started.get_or_insert(now);
        if let Some(current) = self.current {
            self.advance(current, now, quiet, anchored, timing);
        }
        if self.current.is_some()
            || quiet.is_some()
            || !self.learned.enabled
            || self
                .last_began
                .is_some_and(|began| now.saturating_duration_since(began) < MIN_GAP)
        {
            return;
        }
        let next = (0..self.specs.len())
            .find(|&index| self.wanted(index, now, started, holds) && anchored(index));
        if let Some(index) = next {
            let id = self.specs[index].id;
            self.learned.note_shown(id);
            self.dirty = true;
            self.last_began = Some(now);
            self.current = Some(Current {
                index,
                phase: Phase::Shown(now),
            });
        }
    }

    /// When the schedule next changes on its own, for a host that sleeps
    /// between frames: the hint on screen ending a phase, or a waiting
    /// [`Cue::After`] coming due.
    #[must_use]
    pub fn next_change(&self, timing: Timing) -> Option<Instant> {
        if let Some(current) = self.current {
            return Some(match current.phase {
                Phase::Shown(since) => since + IGNORED_AFTER,
                Phase::Leaving(since) => since + timing.leaving,
                Phase::Done(since) => since + timing.done,
            });
        }
        let started = self.started?;
        let mut due = self.last_began.map(|began| began + MIN_GAP);
        for (index, spec) in self.specs.iter().enumerate() {
            if self.learned.knows(spec.id) {
                continue;
            }
            for cue in spec.when {
                if let Cue::After(after) = cue {
                    let at = (started + *after).max(due.unwrap_or(started));
                    due = Some(due.map_or(at, |d| d.max(at)));
                }
            }
            if let Some(left) = self.tracks.get(index).and_then(|t| t.left) {
                due = Some(due.map_or(left + RETURN_AFTER, |d| d.min(left + RETURN_AFTER)));
            }
        }
        due
    }

    fn advance(
        &mut self,
        current: Current,
        now: Instant,
        quiet: Option<Quiet>,
        anchored: &dyn Fn(usize) -> bool,
        timing: Timing,
    ) {
        let since = |at: Instant| now.saturating_duration_since(at);
        match current.phase {
            Phase::Shown(at) => {
                // Typing goes on beside a hint already showing; a call, a
                // follow or a lost anchor take it away.
                let interrupted = matches!(quiet, Some(Quiet::Call | Quiet::Following))
                    || !anchored(current.index);
                if since(at) >= IGNORED_AFTER || interrupted || !self.learned.enabled {
                    if let Some(track) = self.tracks.get_mut(current.index) {
                        track.left = Some(now);
                    }
                    self.current = Some(Current {
                        index: current.index,
                        phase: Phase::Leaving(now),
                    });
                }
            }
            Phase::Leaving(at) => {
                if since(at) >= timing.leaving {
                    self.current = None;
                }
            }
            Phase::Done(at) => {
                if since(at) >= timing.done {
                    self.current = None;
                }
            }
        }
    }

    fn wanted(
        &self,
        index: usize,
        now: Instant,
        started: Instant,
        holds: &dyn Fn(S) -> bool,
    ) -> bool {
        let (Some(spec), Some(track)) = (self.specs.get(index), self.tracks.get(index)) else {
            return false;
        };
        if self.learned.knows(spec.id) || self.learned.showings(spec.id) >= MAX_SHOWINGS {
            return false;
        }
        if track
            .left
            .is_some_and(|left| now.saturating_duration_since(left) < RETURN_AFTER)
        {
            return false;
        }
        if !spec.only_while.iter().all(|fact| holds(*fact)) {
            return false;
        }
        spec.when.iter().any(|cue| match cue {
            Cue::After(after) => now.saturating_duration_since(started) >= *after,
            Cue::Saw(_) => track
                .cued
                .is_some_and(|at| now.saturating_duration_since(at) <= CUE_FRESH),
            Cue::Holds(fact) => holds(*fact),
        })
    }

    /// A hint on screen that is no longer wanted (learned elsewhere, hints
    /// turned off) goes.
    fn retire_unwanted(&mut self, now: Instant) {
        if let Some(current) = self.current
            && let Phase::Shown(_) = current.phase
            && let Some(spec) = self.specs.get(current.index)
            && (!self.learned.enabled || self.learned.knows(spec.id))
        {
            self.current = Some(Current {
                index: current.index,
                phase: Phase::Leaving(now),
            });
        }
    }
}

/// How long the phases that end on their own take, from the motion
/// ([`crate::motion::Beckon`]) under the current reduced-motion choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub leaving: Duration,
    pub done: Duration,
}

impl Timing {
    #[must_use]
    pub fn of(motion: crate::tokens::EffectiveMotion) -> Self {
        Self {
            leaving: crate::motion::Beckon::leaving_time(motion),
            done: crate::motion::Beckon::COMPLETION,
        }
    }
}

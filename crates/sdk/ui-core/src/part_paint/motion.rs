//! The clock a part's transitions run on. A painter declares how a change
//! moves ([`super::PartPainter::transition`]) and draws a given moment; the
//! toolkit holds the time.

use crate::time::Instant;

use super::{PainterSet, Part, PartState, States, Transition};

/// Where a part is in its motion: the states it is moving between and how
/// far it has come.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    pub state: PartState,
    /// 0 at the change, 1 on arrival; a spring may pass 1 on its way.
    pub progress: f32,
    /// Arrived: the part is drawn as it will stay.
    pub settled: bool,
}

impl Moment {
    /// At rest in `states`.
    #[must_use]
    pub fn settled(states: States) -> Self {
        Self {
            state: PartState::settled(states),
            progress: 1.0,
            settled: true,
        }
    }
}

/// One part's motion between states, run on the toolkit's clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartMotion {
    from: States,
    to: States,
    started: Option<Instant>,
    transition: Transition,
}

impl PartMotion {
    /// At rest in `states`.
    #[must_use]
    pub fn new(states: States) -> Self {
        Self {
            from: states,
            to: states,
            started: None,
            transition: Transition::None,
        }
    }

    /// The states the part is going to.
    #[must_use]
    pub fn target(&self) -> States {
        self.to
    }

    /// Move to `states` at `now`, the way `set` says `part` moves;
    /// `inherit` is the toolkit's own motion for the change, taken where
    /// the set inherits it or there is no set. With `reduce_motion` every
    /// change lands at once. A change while moving starts from where the
    /// part was going.
    pub fn go(
        &mut self,
        states: States,
        (set, part): (Option<&PainterSet>, Part),
        inherit: Transition,
        reduce_motion: bool,
        now: Instant,
    ) {
        if states == self.to {
            return;
        }
        let declared = set.map_or(Transition::Inherit, |set| {
            set.painter().transition(part, self.to, states)
        });
        let transition = match declared {
            Transition::Inherit => inherit,
            declared => declared,
        };
        self.from = self.to;
        self.to = states;
        if reduce_motion || matches!(transition, Transition::None | Transition::Inherit) {
            self.from = states;
            self.started = None;
            self.transition = Transition::None;
        } else {
            self.started = Some(now);
            self.transition = transition;
        }
    }

    /// Where the part is at `now`.
    #[must_use]
    pub fn at(&self, now: Instant) -> Moment {
        let Some(started) = self.started else {
            return Moment::settled(self.to);
        };
        let elapsed = now.saturating_duration_since(started).as_secs_f32();
        match self.transition.at(elapsed) {
            Some((progress, false)) => Moment {
                state: PartState {
                    now: self.to,
                    was: self.from,
                },
                progress,
                settled: false,
            },
            _ => Moment::settled(self.to),
        }
    }

    /// Whether the part is still moving at `now`: its owner asks for
    /// another frame.
    #[must_use]
    pub fn moving(&self, now: Instant) -> bool {
        !self.at(now).settled
    }
}

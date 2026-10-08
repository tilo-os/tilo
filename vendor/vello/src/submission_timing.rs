// Copyright 2026 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Opt-in CPU boundaries of one caller-owned render transaction.
//!
//! These are host wall times, including scheduling, not GPU timestamps. No
//! clocks are sampled outside an explicitly enabled scope. The scope carries
//! no scene data and does not change renderer layout or submission ordering.

use std::{cell::Cell, marker::PhantomData, rc::Rc, time::Instant};

const LIMIT: u32 = 256;

/// Sequential CPU boundaries within Vello's existing aggregate render call.
#[derive(Clone, Copy, Debug)]
pub enum Stage {
    /// Scene-to-recording conversion (does not include damage planning).
    Scene,
    /// GPU command encoding, including timestamp resolution and post-render commands.
    Commands,
    /// The actual command encoder finish call.
    Finish,
    /// The actual queue submit call.
    Submit,
}

/// Bounded samples; counts distinguish absent stages from measured zero duration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmissionTiming {
    /// Scene conversion samples.
    pub scene_count: u32,
    /// Command encoding samples.
    pub command_count: u32,
    /// Encoder finish samples.
    pub finish_count: u32,
    /// Queue submit samples.
    pub submit_count: u32,
    /// Scene conversion wall time.
    pub scene_us: u64,
    /// Command encoding wall time.
    pub commands_us: u64,
    /// Encoder finish wall time.
    pub finish_us: u64,
    /// Queue submit wall time.
    pub submit_us: u64,
    /// Nested scopes, duration overflow or exceeded sample bounds make attribution unusable.
    pub invalid: bool,
}

impl SubmissionTiming {
    /// Every observed scene must have reached all three submission boundaries.
    pub fn complete(self) -> bool {
        !self.invalid
            && self.scene_count > 0
            && self.scene_count == self.command_count
            && self.scene_count == self.finish_count
            && self.scene_count == self.submit_count
    }

    /// Accumulate bounded render attempts or bands in the same caller frame.
    pub fn append(&mut self, other: Self) {
        self.invalid |= other.invalid;
        self.scene_count = self.scene_count.saturating_add(other.scene_count);
        self.command_count = self.command_count.saturating_add(other.command_count);
        self.finish_count = self.finish_count.saturating_add(other.finish_count);
        self.submit_count = self.submit_count.saturating_add(other.submit_count);
        self.invalid |= add_time(&mut self.scene_us, other.scene_us);
        self.invalid |= add_time(&mut self.commands_us, other.commands_us);
        self.invalid |= add_time(&mut self.finish_us, other.finish_us);
        self.invalid |= add_time(&mut self.submit_us, other.submit_us);
        self.invalid |= [
            self.scene_count,
            self.command_count,
            self.finish_count,
            self.submit_count,
        ]
        .into_iter()
        .any(|count| count > LIMIT);
    }

    fn record(&mut self, stage: Stage, elapsed: u128) {
        let Ok(elapsed) = u64::try_from(elapsed) else {
            self.invalid = true;
            return;
        };
        let (count, total) = match stage {
            Stage::Scene => (&mut self.scene_count, &mut self.scene_us),
            Stage::Commands => (&mut self.command_count, &mut self.commands_us),
            Stage::Finish => (&mut self.finish_count, &mut self.finish_us),
            Stage::Submit => (&mut self.submit_count, &mut self.submit_us),
        };
        if *count >= LIMIT {
            self.invalid = true;
        } else {
            *count += 1;
            self.invalid |= add_time(total, elapsed);
        }
    }
}

fn add_time(total: &mut u64, elapsed: u64) -> bool {
    if let Some(sum) = total.checked_add(elapsed) {
        *total = sum;
        false
    } else {
        true
    }
}

thread_local! {
    static CURRENT: Cell<Option<SubmissionTiming>> = const { Cell::new(None) };
}

/// A thread-confined opt-in scope. Dropping it discards an unfinished transaction.
pub struct Scope(PhantomData<Rc<()>>);

impl Scope {
    /// Enable observation only when the caller's existing diagnostic gate is on.
    pub fn begin(enabled: bool) -> Option<Self> {
        if !enabled {
            return None;
        }
        CURRENT.with(|current| {
            if let Some(mut outer) = current.get() {
                outer.invalid = true;
                current.set(Some(outer));
                None
            } else {
                current.set(Some(SubmissionTiming::default()));
                Some(Self(PhantomData))
            }
        })
    }

    /// Finish observation; the caller supplies its existing frame identity.
    pub fn finish(self) -> SubmissionTiming {
        CURRENT.with(|current| current.take().unwrap_or_default())
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(None));
    }
}

pub(crate) struct Boundary {
    stage: Stage,
    started: Option<Instant>,
}

impl Boundary {
    pub(crate) fn start(stage: Stage) -> Self {
        Self {
            stage,
            started: CURRENT.with(|current| current.get().map(|_| Instant::now())),
        }
    }
}

impl Drop for Boundary {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            CURRENT.with(|current| {
                if let Some(mut value) = current.get() {
                    value.record(self.stage, started.elapsed().as_micros());
                    current.set(Some(value));
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_missing_aborted_and_nested_are_not_complete() {
        assert!(Scope::begin(false).is_none());
        assert!(Boundary::start(Stage::Scene).started.is_none());
        let scope = Scope::begin(true).unwrap();
        drop(Boundary::start(Stage::Scene));
        assert!(!scope.finish().complete());
        let outer = Scope::begin(true).unwrap();
        assert!(Scope::begin(true).is_none());
        assert!(outer.finish().invalid);
        drop(Scope::begin(true));
        assert!(Boundary::start(Stage::Submit).started.is_none());
    }

    #[test]
    fn zero_is_observed_and_exhaustion_is_explicit() {
        let mut value = SubmissionTiming::default();
        for stage in [Stage::Scene, Stage::Commands, Stage::Finish, Stage::Submit] {
            value.record(stage, 0);
        }
        assert!(value.complete());
        for _ in 0..LIMIT {
            value.record(Stage::Submit, 0);
        }
        assert!(value.invalid);
        assert!(!value.complete());
        let mut overflow = SubmissionTiming::default();
        overflow.record(Stage::Scene, u128::from(u64::MAX));
        overflow.record(Stage::Scene, 1);
        assert!(overflow.invalid);
        assert!(!overflow.complete());
        let mut oversized = SubmissionTiming::default();
        oversized.record(Stage::Scene, u128::MAX);
        assert!(oversized.invalid);
        assert_eq!(oversized.scene_count, 0);
    }
}

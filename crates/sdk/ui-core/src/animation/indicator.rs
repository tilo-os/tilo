//! The cadence of looping indicators: a spinner, a tab's busy arc, a
//! sweeping bar.
//!
//! An indicator only says "still working"; it does not need the display's
//! full refresh to say it. It moves in steps of [`INDICATOR_STEP`] and
//! repaints (damages its own bounds, presents) only when its step changes,
//! so one busy mark on screen costs a few frames a second instead of every
//! refresh. Under reduced motion it stands still (`EffectiveMotion::loops`).
use crate::time::{Duration, Instant};

/// One step of a looping indicator: 30 a second.
pub const INDICATOR_STEP: Duration = Duration::from_nanos(33_333_333);

/// The step `now` falls in, counted from the indicator's `epoch`.
#[must_use]
pub fn indicator_step(epoch: Instant, now: Instant) -> u64 {
    let elapsed = now.saturating_duration_since(epoch).as_nanos();
    u64::try_from(elapsed / INDICATOR_STEP.as_nanos()).unwrap_or(u64::MAX)
}

/// Time since `epoch` at the start of the step `now` falls in: what an
/// indicator draws, so its pose changes once per step.
#[must_use]
pub fn indicator_elapsed(epoch: Instant, now: Instant) -> Duration {
    let step = u32::try_from(indicator_step(epoch, now)).unwrap_or(u32::MAX);
    INDICATOR_STEP.saturating_mul(step)
}

/// Where in a loop `period` long an indicator stands at `now`, `0..1`,
/// taken at the start of the step `now` falls in.
#[must_use]
pub fn indicator_phase(epoch: Instant, now: Instant, period: Duration) -> f64 {
    let period = period.as_nanos().max(1);
    let at = indicator_elapsed(epoch, now).as_nanos() % period;
    let (at, period) = (
        u64::try_from(at).unwrap_or(u64::MAX),
        u64::try_from(period).unwrap_or(u64::MAX),
    );
    #[allow(
        clippy::cast_precision_loss,
        reason = "a phase within one loop needs no more than f64's 52 bits"
    )]
    let phase = at as f64 / period as f64;
    phase
}

/// When the step after the one `now` falls in begins.
#[must_use]
pub fn next_indicator_step(epoch: Instant, now: Instant) -> Instant {
    let next = indicator_step(epoch, now).saturating_add(1);
    let next = u32::try_from(next).unwrap_or(u32::MAX);
    epoch
        .checked_add(INDICATOR_STEP.saturating_mul(next))
        .unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_indicator_moves_in_steps_of_its_cadence() {
        let epoch = crate::time::now();
        assert_eq!(indicator_step(epoch, epoch), 0);
        let mid = epoch + Duration::from_millis(50);
        assert_eq!(indicator_step(epoch, mid), 1);
        assert_eq!(indicator_elapsed(epoch, mid), INDICATOR_STEP);
        assert_eq!(next_indicator_step(epoch, mid), epoch + INDICATOR_STEP * 2);
        // A frame inside one step shows the same pose as the step's start.
        let early = epoch + INDICATOR_STEP + Duration::from_millis(1);
        assert_eq!(
            indicator_elapsed(epoch, early),
            indicator_elapsed(epoch, mid)
        );
    }
}

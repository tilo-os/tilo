//! Timing one phase of an action, when the `tilo_agent_session::timing`
//! target is at debug.

use std::time::Instant;

/// A clock read for timing one phase of an action, when the
/// `tilo_agent_session::timing` target is at debug: off, it reads no clock.
pub(super) fn timing_start() -> Option<Instant> {
    tracing::enabled!(target: "tilo_agent_session::timing", tracing::Level::DEBUG)
        .then(Instant::now)
}

/// Time the phase `step`, begun at `started` ([`timing_start`]), in
/// microseconds: `TIMING <step> <µs>` says where a slow action waited.
pub(super) fn timing_done(step: &'static str, started: Option<Instant>) {
    if let Some(started) = started {
        tracing::debug!(
            target: "tilo_agent_session::timing",
            "TIMING {step} {}",
            started.elapsed().as_micros()
        );
    }
}

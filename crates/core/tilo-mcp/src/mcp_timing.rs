//! How long a phase of a call took, on the `tilo_mcp::timing` target at
//! debug: a call's phases say where a slow one waited.

/// A clock read for timing one phase of a call, when the `tilo_mcp::timing`
/// target is at debug: off, it reads no clock and costs one disabled check.
pub(crate) fn timing_start() -> Option<std::time::Instant> {
    tracing::enabled!(target: "tilo_mcp::timing", tracing::Level::DEBUG)
        .then(std::time::Instant::now)
}

/// Time the phase `step`, begun at `started` ([`timing_start`]), in
/// microseconds: `TIMING <step> <µs>` says where a slow call waited.
pub(crate) fn timing_done(step: &'static str, started: Option<std::time::Instant>) {
    if let Some(started) = started {
        tracing::debug!(
            target: "tilo_mcp::timing",
            "TIMING {step} {}",
            started.elapsed().as_micros()
        );
    }
}

/// [`timing_done`], saying `what` the phase was for:
/// `TIMING <step> <µs> <what>`.
pub(crate) fn timing_done_saying(step: &'static str, started: std::time::Instant, what: &str) {
    tracing::debug!(
        target: "tilo_mcp::timing",
        "TIMING {step} {} {what}",
        started.elapsed().as_micros()
    );
}

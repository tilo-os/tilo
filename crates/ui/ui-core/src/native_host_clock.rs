//! Direct clock bounds for opt-in native input diagnostics.
use crate::time::Instant;
// Only the native diagnostics name this clock; the portable toolkit a server
// links re-exports just the bracket its sampler returns.
#[cfg(any(feature = "native", test))]
pub(crate) use tilo_presentation::clock::CLOCK_NAME;
pub(crate) use tilo_presentation::clock::HostClockBracket;

pub(crate) fn sample() -> (Instant, Option<HostClockBracket>) {
    tilo_presentation::clock::sample(crate::time::now)
}

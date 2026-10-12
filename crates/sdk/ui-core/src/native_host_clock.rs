//! Direct host-clock bounds for opt-in native input diagnostics.
use crate::time::Instant;
pub(crate) use tilo_presentation::clock::{CLOCK_NAME, HostClockBracket};

pub(crate) fn sample() -> (Instant, Option<HostClockBracket>) {
    tilo_presentation::clock::sample(crate::time::now)
}

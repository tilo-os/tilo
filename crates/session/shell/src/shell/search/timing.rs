//! Provider costs in the existing owner profile and opt-in debug tracing.
use std::time::Instant;

use crate::compositor::metrics::profile;

pub(super) struct Cost<'a> {
    phase: &'static str,
    provider: &'a str,
    start: Option<Instant>,
}

impl<'a> Cost<'a> {
    pub(super) fn new(phase: &'static str, provider: &'a str) -> Self {
        let enabled = profile::active()
            || tracing::enabled!(target: "shell::search_timing", tracing::Level::DEBUG);
        Self {
            phase,
            provider,
            start: enabled.then(Instant::now),
        }
    }
}

impl Drop for Cost<'_> {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            let elapsed = start.elapsed();
            profile::record_phase(self.phase, elapsed, None);
            // Never log the query, labels or resource contents.
            let elapsed_us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
            tracing::debug!(target: "shell::search_timing", phase = self.phase,
                provider = self.provider, elapsed_us, "search provider cost");
        }
    }
}

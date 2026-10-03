//! Opt-in, bounded CPU stage measurements, and what asked for each presented
//! frame (widget and app names only). No document content is retained.
use std::collections::{BTreeMap, VecDeque};
use web_time::Instant;

const LIMIT: usize = 16_384;

#[derive(serde::Serialize)]
pub(crate) struct Sample {
    pub stage: &'static str,
    pub ms: f64,
}

#[derive(Default)]
pub(crate) struct FrameMetrics {
    enabled: bool,
    samples: VecDeque<Sample>,
    dropped: usize,
    /// Presented frames by what asked for each; a frame with several
    /// reasons counts once for every one.
    requesters: BTreeMap<String, u64>,
}

impl FrameMetrics {
    pub fn configure(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.samples.clear();
        self.dropped = 0;
        self.requesters.clear();
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn begin(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    /// Count one presented frame for each of `reasons`.
    pub fn presented(&mut self, reasons: Vec<String>) {
        if !self.enabled {
            return;
        }
        for reason in reasons {
            if self.requesters.len() < LIMIT || self.requesters.contains_key(&reason) {
                *self.requesters.entry(reason).or_default() += 1;
            }
        }
    }

    pub fn take_requesters(&mut self) -> BTreeMap<String, u64> {
        std::mem::take(&mut self.requesters)
    }

    pub fn end(&mut self, stage: &'static str, start: Option<Instant>) {
        if let Some(start) = start {
            if self.samples.len() == LIMIT {
                self.samples.pop_front();
                self.dropped = self.dropped.saturating_add(1);
            }
            self.samples.push_back(Sample {
                stage,
                ms: start.elapsed().as_secs_f64() * 1000.0,
            });
        }
    }

    pub fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.dropped)
    }

    pub fn take(&mut self) -> Vec<Sample> {
        self.samples.drain(..).collect()
    }
}

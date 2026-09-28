//! Opt-in, bounded CPU stage measurements. No document content is retained.
//! Ported from PR 541 without its redraw-source notes.
use std::collections::VecDeque;
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
}

impl FrameMetrics {
    pub fn configure(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.samples.clear();
        self.dropped = 0;
    }

    pub fn begin(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
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

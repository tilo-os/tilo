//! Opt-in, bounded CPU stage measurements. No document content is retained.
use std::collections::{BTreeMap, VecDeque};
use web_time::Instant;
const LIMIT: usize = 16_384;
/// Distinct diagnostic note keys retained per capture (widget paths, not content).
const NOTE_KEYS: usize = 64;
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
    notes: BTreeMap<String, u32>,
}
impl FrameMetrics {
    pub fn configure(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.samples.clear();
        self.dropped = 0;
        self.notes.clear();
    }
    /// Count a dynamic diagnostic key (for example a dirty widget path); bounded.
    pub fn note(&mut self, key: &str) {
        if !self.enabled {
            return;
        }
        if let Some(count) = self.notes.get_mut(key) {
            *count += 1;
        } else if self.notes.len() < NOTE_KEYS {
            self.notes.insert(key.to_owned(), 1);
        }
    }
    pub fn take_notes(&mut self) -> BTreeMap<String, u32> {
        std::mem::take(&mut self.notes)
    }
    pub fn begin(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }
    pub fn end(&mut self, stage: &'static str, start: Option<Instant>) {
        if let Some(start) = start {
            if self.samples.len() == LIMIT {
                self.samples.pop_front();
                self.dropped += 1;
            }
            self.samples.push_back(Sample {
                stage,
                ms: start.elapsed().as_secs_f64() * 1000.0,
            });
        }
    }
    #[cfg(target_arch = "wasm32")]
    pub fn count(&mut self, stage: &'static str) {
        if !self.enabled {
            return;
        }
        if self.samples.len() == LIMIT {
            self.samples.pop_front();
            self.dropped += 1;
        }
        self.samples.push_back(Sample { stage, ms: 0.0 });
    }
    pub fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.dropped)
    }
    pub fn take(&mut self) -> Vec<Sample> {
        self.samples.drain(..).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_capture_has_no_samples_and_enable_resets_the_window() {
        let mut metrics = FrameMetrics::default();
        assert!(metrics.begin().is_none());
        metrics.end("disabled", metrics.begin());
        assert!(metrics.take().is_empty());
        metrics.configure(true);
        metrics.end("draw", metrics.begin());
        assert_eq!(metrics.take().len(), 1);
        assert!(metrics.take().is_empty());
        metrics.end("old-window", metrics.begin());
        metrics.configure(false);
        assert!(metrics.take().is_empty());
    }
    #[test]
    fn capture_keeps_only_the_most_recent_bounded_samples() {
        let mut metrics = FrameMetrics::default();
        metrics.configure(true);
        metrics.end("old", metrics.begin());
        for _ in 0..LIMIT {
            metrics.end("recent", metrics.begin());
        }
        let samples = metrics.take();
        assert_eq!(samples.len(), LIMIT);
        assert_eq!(metrics.take_dropped(), 1);
        assert_eq!(metrics.take_dropped(), 0);
        assert!(samples.iter().all(|sample| sample.stage == "recent"));
    }
    #[test]
    fn notes_count_distinct_keys_and_stay_bounded() {
        let mut metrics = FrameMetrics::default();
        metrics.note("/disabled");
        assert!(metrics.take_notes().is_empty());
        metrics.configure(true);
        for _ in 0..3 {
            metrics.note("/Column/Button");
        }
        for i in 0..(NOTE_KEYS * 2) {
            metrics.note(&format!("/path/{i}"));
        }
        let notes = metrics.take_notes();
        assert_eq!(notes.get("/Column/Button"), Some(&3));
        assert_eq!(notes.len(), NOTE_KEYS);
        assert!(metrics.take_notes().is_empty());
    }
}

//! What a run measured.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Stats {
    pub joined: AtomicU64,
    pub presence_tx: AtomicU64,
    pub presence_rx: AtomicU64,
    pub presence_refused: AtomicU64,
    pub chat_tx: AtomicU64,
    pub committed: AtomicU64,
    pub chat_rx: AtomicU64,
    pub live_open: AtomicU64,
    pub live_ended: AtomicU64,
    pub session_failures: AtomicU64,
    join_ms: Mutex<Vec<f64>>,
    /// The admission part of each join: `/join` and the redemptions.
    admit_ms: Mutex<Vec<f64>>,
    chat_ms: Mutex<Vec<f64>>,
    /// How long each guest refused for a full room waited for the answer.
    full_ms: Mutex<Vec<f64>>,
    failures: Mutex<BTreeMap<String, u64>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The `q` quantile of `values` (nearest rank).
fn quantile(values: &mut [f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let rank = ((values.len() as f64) * q).ceil() as usize;
    values
        .get(rank.saturating_sub(1).min(values.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default()
}

impl Stats {
    pub fn join(&self, admit_ms: f64, ms: f64) {
        self.joined.fetch_add(1, Ordering::Relaxed);
        lock(&self.join_ms).push(ms);
        lock(&self.admit_ms).push(admit_ms);
    }

    pub fn full(&self, ms: f64) {
        lock(&self.full_ms).push(ms);
    }

    pub fn chat(&self, ms: f64) {
        self.chat_rx.fetch_add(1, Ordering::Relaxed);
        lock(&self.chat_ms).push(ms);
    }

    pub fn fail(&self, what: impl Into<String>) {
        let mut what: String = what.into();
        what.truncate(160);
        *lock(&self.failures).entry(what).or_default() += 1;
    }

    pub fn summary(&self) -> serde_json::Value {
        let mut join = lock(&self.join_ms).clone();
        let mut admit = lock(&self.admit_ms).clone();
        // In arrival order: whether joining slows as the room fills.
        let by_tenth = |values: &[f64]| -> Vec<f64> {
            values
                .chunks(values.len().div_ceil(10).max(1))
                .map(|chunk| chunk.iter().sum::<f64>() / chunk.len() as f64)
                .collect()
        };
        let join_by_tenth = by_tenth(&join);
        let admit_by_tenth = by_tenth(&admit);
        let mut chat = lock(&self.chat_ms).clone();
        let mut full = lock(&self.full_ms).clone();
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        serde_json::json!({
            "joined": load(&self.joined),
            "join_ms": {
                "p50": quantile(&mut join, 0.5),
                "p95": quantile(&mut join, 0.95),
                "max": quantile(&mut join, 1.0),
            },
            "join_ms_mean_by_tenth": join_by_tenth,
            "admit_ms": {
                "p50": quantile(&mut admit, 0.5),
                "p95": quantile(&mut admit, 0.95),
            },
            "admit_ms_mean_by_tenth": admit_by_tenth,
            "full": {
                "count": full.len(),
                "p95_ms": quantile(&mut full, 0.95),
                "max_ms": quantile(&mut full, 1.0),
            },
            "presence_tx": load(&self.presence_tx),
            "presence_rx": load(&self.presence_rx),
            "presence_refused": load(&self.presence_refused),
            "chat_tx": load(&self.chat_tx),
            "chat_rx": load(&self.chat_rx),
            "committed": load(&self.committed),
            "chat_ms": {
                "p50": quantile(&mut chat, 0.5),
                "p95": quantile(&mut chat, 0.95),
                "max": quantile(&mut chat, 1.0),
            },
            "live_open": load(&self.live_open),
            "live_ended": load(&self.live_ended),
            "session_failures": load(&self.session_failures),
            "failures": lock(&self.failures).clone(),
        })
    }
}

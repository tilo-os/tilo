//! What a run measured.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_lock::MutexRecover as _;

#[derive(Default)]
pub struct Stats {
    pub joined: AtomicU64,
    pub presence_tx: AtomicU64,
    pub presence_rx: AtomicU64,
    pub presence_refused: AtomicU64,
    pub chat_tx: AtomicU64,
    pub committed: AtomicU64,
    pub chat_rx: AtomicU64,
    /// Messages sent before their receiver joined, which reach it with the
    /// room's history; their age says nothing about delivery.
    pub chat_history: AtomicU64,
    pub live_open: AtomicU64,
    pub live_ended: AtomicU64,
    pub session_failures: AtomicU64,
    join_ms: Mutex<Vec<f64>>,
    /// The admission part of each join: `/join` and the redemptions.
    admit_ms: Mutex<Vec<f64>>,
    /// `/join`'s answer alone (the service pre-admitting), in arrival order.
    service_ms: Mutex<Vec<f64>>,
    /// Delivery of messages sent while their receiver was in the room, with
    /// when each was sent (ms into the run).
    chat_ms: Mutex<Vec<(u64, f64)>>,
    /// How long each guest refused for a full room waited for the answer.
    full_ms: Mutex<Vec<f64>>,
    /// How long opening the chat's first part took the guests that scrolled
    /// back to it, and the frames it came in.
    scrollback_ms: Mutex<Vec<f64>>,
    scrollback_frames: AtomicU64,
    failures: Mutex<BTreeMap<String, u64>>,
}

/// The width of the send-time windows chat delivery is reported in.
const CHAT_WINDOW_MS: u64 = 5000;

/// The `q` quantile of `values` (nearest rank).
pub fn quantile(values: &mut [f64], q: f64) -> f64 {
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
        self.join_ms.lock_recover().push(ms);
        self.admit_ms.lock_recover().push(admit_ms);
    }

    pub fn service(&self, ms: f64) {
        self.service_ms.lock_recover().push(ms);
    }

    pub fn full(&self, ms: f64) {
        self.full_ms.lock_recover().push(ms);
    }

    pub fn chat(&self, sent: u64, ms: f64) {
        self.chat_rx.fetch_add(1, Ordering::Relaxed);
        self.chat_ms.lock_recover().push((sent, ms));
    }

    pub fn scrollback(&self, ms: f64, frames: u32) {
        self.scrollback_ms.lock_recover().push(ms);
        self.scrollback_frames
            .fetch_add(u64::from(frames), Ordering::Relaxed);
    }

    pub fn chat_history(&self) {
        self.chat_history.fetch_add(1, Ordering::Relaxed);
    }

    pub fn fail(&self, what: impl Into<String>) {
        let mut what: String = what.into();
        what.truncate(160);
        *self.failures.lock_recover().entry(what).or_default() += 1;
    }

    pub fn summary(&self) -> serde_json::Value {
        let mut join = self.join_ms.lock_recover().clone();
        let mut admit = self.admit_ms.lock_recover().clone();
        let mut service = self.service_ms.lock_recover().clone();
        // In arrival order: whether joining slows as the room fills.
        let by_tenth = |values: &[f64]| -> Vec<f64> {
            values
                .chunks(values.len().div_ceil(10).max(1))
                .map(|chunk| chunk.iter().sum::<f64>() / chunk.len() as f64)
                .collect()
        };
        let join_by_tenth = by_tenth(&join);
        let admit_by_tenth = by_tenth(&admit);
        let service_by_tenth = by_tenth(&service);
        let delivered = self.chat_ms.lock_recover().clone();
        // By when each message was sent: whether delivery slows while guests
        // are still joining.
        let mut windows: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
        for (sent, ms) in &delivered {
            windows
                .entry(sent / CHAT_WINDOW_MS * CHAT_WINDOW_MS / 1000)
                .or_default()
                .push(*ms);
        }
        let chat_p95_by_sent_s: BTreeMap<u64, f64> = windows
            .into_iter()
            .map(|(at, mut ms)| (at, quantile(&mut ms, 0.95)))
            .collect();
        let mut chat: Vec<f64> = delivered.into_iter().map(|(_, ms)| ms).collect();
        let mut full = self.full_ms.lock_recover().clone();
        let mut scrollback = self.scrollback_ms.lock_recover().clone();
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        serde_json::json!({
            "joined": load(&self.joined),
            "join_ms": {
                "p50": quantile(&mut join, 0.5),
                "p95": quantile(&mut join, 0.95),
                "p99": quantile(&mut join, 0.99),
                "max": quantile(&mut join, 1.0),
            },
            "join_ms_mean_by_tenth": join_by_tenth,
            "admit_ms": {
                "p50": quantile(&mut admit, 0.5),
                "p95": quantile(&mut admit, 0.95),
            },
            "admit_ms_mean_by_tenth": admit_by_tenth,
            "service_ms": {
                "p50": quantile(&mut service, 0.5),
                "p95": quantile(&mut service, 0.95),
            },
            "service_ms_mean_by_tenth": service_by_tenth,
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
            "chat_history": load(&self.chat_history),
            "committed": load(&self.committed),
            "chat_ms": {
                "p50": quantile(&mut chat, 0.5),
                "p95": quantile(&mut chat, 0.95),
                "p99": quantile(&mut chat, 0.99),
                "max": quantile(&mut chat, 1.0),
            },
            "chat_p95_ms_by_sent_s": chat_p95_by_sent_s,
            "scrollback": {
                "count": scrollback.len(),
                "frames": load(&self.scrollback_frames),
                "p50_ms": quantile(&mut scrollback, 0.5),
                "p99_ms": quantile(&mut scrollback, 0.99),
            },
            "live_open": load(&self.live_open),
            "live_ended": load(&self.live_ended),
            "session_failures": load(&self.session_failures),
            "failures": self.failures.lock_recover().clone(),
        })
    }
}

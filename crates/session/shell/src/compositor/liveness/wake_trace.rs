//! Bounded, allocation-free recording on the loop and heartbeat threads.
//! Enabled only with the development liveness socket; never wakes the loop.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

const CAPACITY: usize = 64;
static START: OnceLock<Instant> = OnceLock::new();
static MISSED: AtomicU64 = AtomicU64::new(0);
static TRACE: Mutex<Trace> = Mutex::new(Trace::new());

#[derive(Clone, Copy, Debug)]
struct Entry {
    sequence: u64,
    micros: u64,
    event: &'static str,
    detail: u64,
}

struct Trace {
    sequence: u64,
    next: usize,
    entries: [Entry; CAPACITY],
}

impl Trace {
    const fn new() -> Self {
        Self {
            sequence: 0,
            next: 0,
            entries: [Entry {
                sequence: 0,
                micros: 0,
                event: "",
                detail: 0,
            }; CAPACITY],
        }
    }

    fn record(&mut self, event: &'static str, detail: u64, micros: u64) {
        self.sequence = self.sequence.saturating_add(1);
        if let Some(slot) = self.entries.get_mut(self.next) {
            *slot = Entry {
                sequence: self.sequence,
                micros,
                event,
                detail,
            };
        }
        self.next = self.next.saturating_add(1) % CAPACITY;
    }
}

pub(super) fn enable() {
    START.get_or_init(Instant::now);
}

pub(crate) fn record(event: &'static str, detail: u64) {
    let Some(start) = START.get() else { return };
    // Diagnostics must never make the loop wait for the reporter.
    let Some(mut trace) = TRACE.try_lock() else {
        MISSED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    trace.record(
        event,
        detail,
        u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
    );
}

pub(super) fn render() -> String {
    use std::fmt::Write as _;
    // Copy under the lock, symbolize/format only after releasing it.
    let mut entries = TRACE.lock().entries;
    entries.sort_unstable_by_key(|entry| entry.sequence);
    let mut out = format!(
        "wake trace (missed {} contended records):\n",
        MISSED.load(Ordering::Relaxed)
    );
    for entry in entries.iter().filter(|entry| entry.sequence != 0) {
        let _ = writeln!(
            out,
            "{} +{}us {} {}",
            entry.sequence, entry.micros, entry.event, entry.detail
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollover_keeps_only_the_last_64_events_in_sequence_order() {
        let mut trace = Trace::new();
        for detail in 1..=100 {
            trace.record("ping-requested", detail, detail * 10);
        }
        trace.entries.sort_unstable_by_key(|entry| entry.sequence);
        assert_eq!(trace.entries.len(), 64);
        for (entry, expected) in trace.entries.iter().zip(37..=100) {
            assert_eq!(entry.sequence, expected);
            assert_eq!(entry.detail, expected);
            assert_eq!(entry.micros, expected * 10);
            assert_eq!(entry.event, "ping-requested");
        }
    }

    #[test]
    fn recording_never_waits_for_a_reporter_holding_the_buffer() {
        enable();
        let _reporter = TRACE.lock();
        let before = MISSED.load(Ordering::Relaxed);
        record("dispatch-enter-ms", u64::MAX);
        assert!(MISSED.load(Ordering::Relaxed) > before);
    }
}

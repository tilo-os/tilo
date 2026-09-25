//! Content-free agent latency marks shared by the broker and the Cloud host.
//!
//! Every line carries only a static stage name, an optional opaque invocation
//! id, a wall-clock microsecond stamp and numeric durations/counters; never
//! arguments, text, record ids or keys. Filter with target
//! `qos_cloud_host::agent_timing` (the host's historical target, kept so one
//! filter follows a step across the broker, worker and Store).
use std::time::{Duration, Instant};

/// Stages shorter than this are not logged by [`slow`]; hot paths (every
/// Store open, every lease check) stay quiet unless they are actually slow.
pub const SLOW: Duration = Duration::from_millis(1);

fn unix_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_micros() as u64)
}

/// One point event.
pub fn mark(event: &'static str, invocation: Option<uuid::Uuid>) {
    match invocation {
        Some(invocation) => tracing::info!(
            target: "qos_cloud_host::agent_timing",
            event, %invocation, unix_us = unix_us(), "agent timing"
        ),
        None => tracing::info!(
            target: "qos_cloud_host::agent_timing",
            event, unix_us = unix_us(), "agent timing"
        ),
    }
}

/// One finished stage with its duration and up to a few numeric details
/// (`name=value` pairs of static names and integers only).
pub fn span(
    event: &'static str,
    invocation: Option<uuid::Uuid>,
    elapsed: Duration,
    details: &[(&'static str, u64)],
) {
    let elapsed_us = elapsed.as_micros() as u64;
    let mut detail = String::new();
    for (name, value) in details {
        use std::fmt::Write;
        let _ = write!(
            detail,
            "{}{name}={value}",
            if detail.is_empty() { "" } else { " " }
        );
    }
    match invocation {
        Some(invocation) => tracing::info!(
            target: "qos_cloud_host::agent_timing",
            event, %invocation, unix_us = unix_us(), elapsed_us, detail = %detail, "agent timing"
        ),
        None => tracing::info!(
            target: "qos_cloud_host::agent_timing",
            event, unix_us = unix_us(), elapsed_us, detail = %detail, "agent timing"
        ),
    }
}

/// [`span`] from `started`, only when at least [`SLOW`] elapsed.
pub fn slow(
    event: &'static str,
    invocation: Option<uuid::Uuid>,
    started: Instant,
    details: &[(&'static str, u64)],
) {
    let elapsed = started.elapsed();
    if elapsed >= SLOW {
        span(event, invocation, elapsed, details);
    }
}

static STORE_SPANS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Turn on Store-level stage lines ([`slow_store`]) for this process. Only a
/// long-running host calls this: one-shot CLI commands stay silent, so their
/// combined output remains exactly the JSON result.
pub fn enable_store_spans() {
    STORE_SPANS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether [`enable_store_spans`] was called in this process.
pub fn store_spans_enabled() -> bool {
    STORE_SPANS.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`slow`] for Store-level stages (no invocation), only once
/// [`enable_store_spans`] was called.
pub fn slow_store(event: &'static str, started: Instant, details: &[(&'static str, u64)]) {
    if STORE_SPANS.load(std::sync::atomic::Ordering::Relaxed) {
        slow(event, None, started, details);
    }
}

/// Microseconds since `started`, for detail fields.
pub fn us(started: Instant) -> u64 {
    started.elapsed().as_micros() as u64
}

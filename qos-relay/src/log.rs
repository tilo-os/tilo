//! Tracing subscriber setup.
//!
//! The relay logs structured events at `info` by default. The
//! `RelayConfig.log_level` field is fed straight into
//! `EnvFilter::try_new`; callers may also export `RUST_LOG=...` which
//! takes precedence (gives operators an escape hatch from inside the
//! systemd unit).

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Initialise the global subscriber. Calling more than once is a
/// no-op (the second `init()` returns an error which we swallow);
/// the integration tests need that because they spawn a relay
/// per-test.
pub fn init(default_level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .try_init();
}

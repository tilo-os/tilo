//! Tracing subscriber setup.
//!
//! Logs structured events at `info` by default. `RelayConfig.log_level`
//! feeds `EnvFilter::try_new`; `RUST_LOG=...` takes precedence, giving
//! operators an escape hatch from inside the systemd unit.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Initialises the global subscriber. A second call is a no-op (its
/// error is swallowed) since integration tests spawn a relay per test.
pub fn init(default_level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .try_init();
}

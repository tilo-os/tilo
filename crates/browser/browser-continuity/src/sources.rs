//! Reviewed legacy source capture, available only through explicit grants.

#[cfg(all(unix, feature = "native"))]
mod native;
#[cfg(all(unix, feature = "native"))]
pub use native::*;
#[cfg(not(all(unix, feature = "native")))]
mod unsupported;
#[cfg(not(all(unix, feature = "native")))]
pub use unsupported::*;

/// Maximum size of one captured database or WAL.
pub const MAX_DATABASE_BYTES: u64 = 64 * 1024 * 1024;
/// Difference between Chromium and Unix epochs in microseconds.
pub const CHROMIUM_EPOCH_MICROS: u64 = 11_644_473_600_000_000;

/// Whether this build supports capture from explicitly granted sources.
#[must_use]
pub const fn supported() -> bool {
    cfg!(all(unix, feature = "native"))
}

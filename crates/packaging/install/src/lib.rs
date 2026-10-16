//! Library surface of `tilo-install`.
//!
//! Everything the CLI in `main.rs` does that is pure logic lives
//! here so it is unit-testable without spawning a `.tapp`
//! install. The installed-package registry, the content store and
//! integrity records belong to the `package-store` crate, which the
//! launcher, the shell and the app host share.
//!
//! - [`gc`]        — reclaiming payloads and runtimes nothing needs.
//! - [`shim`]      — the PATH shims a `[cli]` package publishes.
//! - [`runtime_blobs`] — runtime payload bytes already on this device.
//! - [`state`]     — the user's per-room app data: what uninstalling
//!   must ask about rather than assume.

pub mod gc;
pub mod runtime_blobs;
pub mod shim;
pub mod state;

pub use shim::{ShimError, ShimInstall, locate_shim_source, remove_shims, write_shims};

/// The name of the binary as registered with Cargo. Exposed for
/// integration tests that want to invoke it via `CARGO_BIN_EXE_*`.
pub const BINARY_NAME: &str = "tilo-install";

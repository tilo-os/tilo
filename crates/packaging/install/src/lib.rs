//! Library surface of `qos-install`.
//!
//! Everything the CLI in `main.rs` does that is pure logic lives
//! here so it is unit-testable without spawning a real `.qpkg`
//! install. The registry serde types stay in `main.rs` because they
//! are the shape of the CLI's on-disk contract, not something re-used
//! elsewhere.
//!
//! - [`shim`]      — the PATH shims a `[cli]` package publishes.
//! - [`isa`]       — host ISA detection, for target selection.
//! - [`integrity`] — re-verifying an installed payload against the
//!   signature chain, long after the install that checked it.
//! - [`state`]     — the user's per-room app data: what uninstalling
//!   must ask about rather than assume.

pub mod integrity;
pub mod isa;
pub mod shim;
pub mod state;

pub use shim::{
    SIDECAR_SUFFIX, ShimError, ShimInstall, locate_shim_source, remove_shims, write_shims,
};

/// The name of the binary as registered with Cargo. Exposed for
/// integration tests that want to invoke it via `CARGO_BIN_EXE_*`.
pub const BINARY_NAME: &str = "qos-install";

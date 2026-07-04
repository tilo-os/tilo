//! Library surface of `qos-install`.
//!
//! Everything the `install`/`remove` main.rs does that is pure
//! logic (shim writing) lives here so it's unit-testable without
//! spawning a real .qpkg install. The registry serde types stay
//! in `main.rs` because they're the shape of the CLI's on-disk
//! contract, not something re-used elsewhere.

pub mod shim;

pub use shim::{ShimError, ShimInstall, locate_shim_source, remove_shims, write_shims, SIDECAR_SUFFIX};

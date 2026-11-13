//! What `tilo-install` does that needs no running process, so it can be tested without spawning an
//! install: reclaiming ([`gc`]), the PATH shims a package publishes ([`shim`]), runtime payload bytes
//! already on the device ([`runtime_blobs`]) and the app data uninstalling must ask about ([`state`]).

pub mod gc;
pub mod runtime_blobs;
pub mod shim;
pub mod state;

pub use shim::{ShimError, ShimInstall, locate_shim_source, remove_shims, write_shims};

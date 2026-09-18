//! Actual qOS component renderer. Authentication/state transport are installed by the peer layer.
#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
mod clipboard;

#[cfg(any(target_arch = "wasm32", test))]
mod close_recovery;

#[cfg(target_arch = "wasm32")]
mod peer;

#[cfg(not(target_arch = "wasm32"))]
mod fixture;
#[cfg(not(target_arch = "wasm32"))]
pub use fixture::{fixture_seed,fixture_writer_insert};

mod admitted_records;

//! Actual qOS component renderer. Authentication/state transport are installed by the peer layer.
#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
mod clipboard;

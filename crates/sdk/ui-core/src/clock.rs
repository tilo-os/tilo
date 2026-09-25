//! Monotonic and wall clocks for shared widgets and host-independent services.
#[cfg(not(target_arch = "wasm32"))]
pub use std::time::*;
#[cfg(target_arch = "wasm32")]
pub use web_time::*;

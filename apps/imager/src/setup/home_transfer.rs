//! Native whole-home transfer is owned by the platform backend.
pub use imager_backend::home_transfer::*;

#[cfg(all(test, target_os = "linux"))]
mod promotion_tests;

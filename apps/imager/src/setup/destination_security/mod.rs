//! Native destination namespace protection.

pub(super) use imager_backend::destination_security::*;

#[cfg(all(test, windows))]
mod windows_tests;

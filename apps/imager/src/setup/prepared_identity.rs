//! Compatibility facade for backend-owned native setup operations.
pub use imager_backend::prepared_identity::*;

#[cfg(all(test, unix))]
mod presentation_tests;

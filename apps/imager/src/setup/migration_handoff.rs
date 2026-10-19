//! Compatibility facade for backend-owned native setup operations.
pub use imager_backend::setup::migration_handoff::*;

#[cfg(all(test, target_os = "linux", feature = "installation"))]
mod staging_record_tests;

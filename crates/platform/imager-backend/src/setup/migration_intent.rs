//! Opaque, source-bound permission carried from belongings review into staging.
//! Unsupported hosts can name and carry the permission but cannot construct one.
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
pub use native::*;

#[cfg(not(target_os = "linux"))]
mod unsupported;
#[cfg(not(target_os = "linux"))]
pub use unsupported::MigrationIntent;

#[cfg(not(target_os = "linux"))]
pub type Capture = fn(
    &std::path::Path,
    &str,
    super::migration_handoff::Categories,
) -> std::io::Result<super::migration_handoff::Plan>;

//! Native review, custody and revalidation are owned by the platform backend.
pub use imager_backend::migration_review::*;

#[cfg(target_os = "linux")]
mod chooser;
#[cfg(target_os = "linux")]
pub use chooser::{HomeRootSet, MountedRootChooser};

#[cfg(all(test, target_os = "linux"))]
mod portable_roundtrip_tests;

#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod mount_boundary_tests;

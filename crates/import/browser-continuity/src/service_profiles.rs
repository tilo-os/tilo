//! Website-session profiles with platform-selected, fail-closed storage support.

#[cfg(all(target_os = "linux", feature = "native"))]
mod native;
#[cfg(all(target_os = "linux", feature = "native"))]
pub use native::{GrantedRoot, Profile};
#[cfg(not(all(target_os = "linux", feature = "native")))]
mod unsupported;
#[cfg(not(all(target_os = "linux", feature = "native")))]
pub use unsupported::{GrantedRoot, Profile};

/// Whether this build can admit storage roots for website-session profiles.
#[must_use]
pub const fn supported() -> bool {
    cfg!(all(target_os = "linux", feature = "native"))
}

//! Website-session profiles with platform-selected, fail-closed storage support.

#[cfg(all(target_os = "linux", feature = "service-profiles"))]
mod native;
#[cfg(all(target_os = "linux", feature = "service-profiles"))]
pub use native::{GrantedRoot, Profile};
#[cfg(not(all(target_os = "linux", feature = "service-profiles")))]
mod unsupported;
#[cfg(not(all(target_os = "linux", feature = "service-profiles")))]
pub use unsupported::{GrantedRoot, Profile};

/// Whether this build can admit storage roots for website-session profiles.
#[must_use]
pub const fn supported() -> bool {
    cfg!(all(target_os = "linux", feature = "service-profiles"))
}

/// The profile a restart holds while its engine is stopped and started again on it.
impl crate::session_engine::Profile for Profile {
    fn path(&self) -> std::io::Result<&std::path::Path> {
        Self::path(self)
    }

    fn cleanup_after_engine_exit(&mut self) -> std::io::Result<()> {
        Self::cleanup_after_engine_exit(self)
    }
}

//! Prepared-setup protocol validation and native metadata custody.
//!
//! These records and descriptor checks are not Home, original-source or
//! application admission. Source authority remains with the sealed admissions.
mod install_profile;
#[cfg(test)]
mod tests;
pub use install_profile::PublicInstallProfile;
#[cfg(target_os = "linux")]
pub mod capacity;
#[cfg(target_os = "linux")]
pub mod installed_namespace;

fn invalid() -> std::io::Error {
    std::io::Error::other("Prepared setup custody is invalid or changed")
}

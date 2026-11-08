//! Browser profile authority lives in the native backend.
//! Service-session discovery retains its independent, consented helpers.
#[cfg(any(test, feature = "test-support"))]
pub use crate::browser_profiles::discover;

#[cfg(target_os = "linux")]
pub mod service_profiles;

#[cfg(target_os = "linux")]
use super::{
    account_transfer::{Candidate, Origin, Provider as AccountProvider},
    migration::PinnedDirectory,
    nofollow::metadata_nofollow,
};
#[cfg(target_os = "linux")]
use crate::browser_profiles::{components, firefox_sections, invalid, regular, storage, walk};
#[cfg(target_os = "linux")]
use browser_continuity::Provider;
#[cfg(target_os = "linux")]
use std::{io, path::Path, sync::Arc};
#[cfg(target_os = "linux")]
use tilo_files::native::atomic::Privacy;

#[cfg(target_os = "linux")]
const MAX_PROFILE_DIRECTORIES: usize = 128;
#[cfg(target_os = "linux")]
const MAX_INI: u64 = 64 * 1024;

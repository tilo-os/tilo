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
use crate::browser_profiles::{
    chromium_names, components, firefox_index, invalid, regular, storage, walk,
};
#[cfg(target_os = "linux")]
use browser_continuity::Provider;
#[cfg(target_os = "linux")]
use std::{io, path::Path, sync::Arc};

//! Same-computer selections that stay private in the retained Home.
//!
//! The installer records the selected accounts, mail, browser, applications and Home exclusions in a
//! private journal inside the source Home; only an opaque operation and journal digest travel in the
//! reviewed plan. After first boot the journal is re-read under the retained source and the selected
//! mail is sealed and imported with a fresh in-memory key.
//!
//! Writing that journal needs the retained source's ownership and mounts, so only the first installed,
//! Linux-hosted system has the route; a host without it refuses in [`prepare`].

#[cfg(target_os = "linux")]
mod native;
#[cfg(not(target_os = "linux"))]
mod unsupported;

use super::{
    account_transfer::ThunderbirdSelection,
    migration_handoff::{DeferredHandoff, Plan},
    migration_store::NativePath,
};
use crate::account_sources::ReviewedThunderbird;
use crate::browser_selection::SelectedSources;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub binding: DeferredHandoff,
    pub journal: PathBuf,
}

#[derive(Clone, Copy)]
pub struct Prepare<'a> {
    pub source_home: &'a Path,
    pub plan: &'a Plan,
    pub exclusions: &'a [NativePath],
    pub mail: &'a [(ReviewedThunderbird, ThunderbirdSelection)],
    pub browser: &'a SelectedSources,
    pub applications: bool,
    pub accounts: bool,
    /// The TPM-protected package already carries the selected sign-ins, so the journal does not ask
    /// for their credentials again.
    pub accounts_in_package: bool,
    /// The TPM-protected package already carries the selected browser profiles, so the journal must
    /// not import the same bookmarks or history again.
    pub browser_in_package: bool,
}

#[cfg(target_os = "linux")]
pub use native::*;
#[cfg(not(target_os = "linux"))]
pub use unsupported::*;

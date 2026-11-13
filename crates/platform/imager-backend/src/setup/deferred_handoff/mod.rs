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
pub use crate::account_sources::SelectedAccount;
use crate::{account_sources::ReviewedThunderbird, browser_selection::SelectedSources};
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
    /// The exact GOA accounts selected on the account pages; their identities and enabled services
    /// stay in the private source journal, and the public plan carries only its digest.
    pub account_sources: &'a [SelectedAccount],
    /// The TPM-protected package already carries the selected sign-ins, so the journal must not create
    /// a second pending account row for them.
    pub accounts_in_package: bool,
    /// The TPM-protected package already carries the selected browser profiles, so the journal must
    /// not import the same bookmarks or history again.
    pub browser_in_package: bool,
}

#[cfg(target_os = "linux")]
pub use native::*;
#[cfg(not(target_os = "linux"))]
pub use unsupported::*;

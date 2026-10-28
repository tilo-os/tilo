//! Same-computer selections that stay private in the retained Home.
//!
//! The installer records the person's selected accounts, mail, browser,
//! application and Home-exclusion decisions in a private journal inside the
//! source Home. Selected GNOME Online Account identities keep their enabled
//! services there too, so the destination can create exact pending rows
//! without pretending an old token authenticates every service. Only an opaque
//! operation and journal digest travel in the reviewed migration plan. After
//! first boot the retained source is mounted again, the journal is re-read
//! under that source root, and selected mail is sealed and imported with a
//! fresh in-memory key. No credential, content key or private profile name is
//! placed on installation media, and no source path or UID is taken from
//! editable plan metadata.
//!
//! Writing that journal needs the retained source's ownership and mounts, so
//! only the first installed, Linux-hosted system has the route. This module
//! owns the platform decision: the reviewed values and their prepared
//! reference are portable, and a host without the route refuses in
//! [`prepare`] rather than in every caller.

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

/// What the installer gives the handoff after writing the private journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub binding: DeferredHandoff,
    pub journal: PathBuf,
}

/// Reviewed values copied from the UI into the private source journal.
#[derive(Clone, Copy)]
pub struct Prepare<'a> {
    pub source_home: &'a Path,
    pub plan: &'a Plan,
    pub exclusions: &'a [NativePath],
    pub mail: &'a [(ReviewedThunderbird, ThunderbirdSelection)],
    pub browser: &'a SelectedSources,
    pub applications: bool,
    pub accounts: bool,
    /// The exact GOA accounts selected on the account pages. Their identities
    /// and enabled services stay in the private source journal; the public
    /// plan carries only its authenticated digest.
    pub account_sources: &'a [SelectedAccount],
    /// The TPM-protected package already carries the selected sign-ins, so the
    /// journal must not create a second pending account row for them.
    pub accounts_in_package: bool,
    /// The TPM-protected package already carries the selected browser
    /// profiles, so the journal must not import the same bookmarks or history
    /// again.
    pub browser_in_package: bool,
}

#[cfg(target_os = "linux")]
pub use native::*;
#[cfg(not(target_os = "linux"))]
pub use unsupported::*;

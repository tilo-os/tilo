//! What the installed-app registries say, read from disk: the file stamps a memoised list is
//! checked against, and the ids apps were published under before.
//!
//! Reading the catalogue is not launching, and the identity service needs the renamed-app table
//! as much as a window does, so it lives below both.

use std::path::PathBuf;
use std::sync::Mutex;
use tilo_lock::MutexRecover as _;

/// The registry roots an install writes to, in the order a lookup reads them.
pub(crate) fn app_cache_roots() -> Vec<PathBuf> {
    [
        package_store::layout::default_user_root(),
        package_store::layout::system_data_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Which version of a registry file is on disk. An install replaces the registry by one
/// rename, its commit point, so a completed install always shows as another file here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct RegistryStamp {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) modified: Option<std::time::SystemTime>,
    pub(crate) len: u64,
}

/// The stamps of the registries as they are now, one per root; `None` for a root without one.
pub(crate) fn registry_stamps() -> Vec<Option<RegistryStamp>> {
    use std::os::unix::fs::MetadataExt as _;
    app_cache_roots()
        .iter()
        .map(|root| {
            let metadata =
                std::fs::metadata(root.join(package_store::layout::REGISTRY_FILE)).ok()?;
            Some(RegistryStamp {
                device: metadata.dev(),
                inode: metadata.ino(),
                modified: metadata.modified().ok(),
                len: metadata.len(),
            })
        })
        .collect()
}

/// The ids installed apps were published under before, as the catalogue believes them
/// ([`tilo_installed_apps::earlier_ids`]): the one lookup a window, a launch or an open written
/// under an earlier id goes through. Read again only when a registry changed; empty for an
/// unreadable catalogue.
pub(crate) fn installed_earlier_ids() -> tilo_action_policy::EarlierIds {
    static EARLIER: Mutex<Option<(Vec<Option<RegistryStamp>>, tilo_action_policy::EarlierIds)>> =
        Mutex::new(None);
    let stamps = registry_stamps();
    if let Some((at, ids)) = EARLIER.lock_recover().as_ref()
        && *at == stamps
    {
        return ids.clone();
    }
    let ids = package_store::Catalog::from_env()
        .ok()
        .and_then(|catalog| tilo_installed_apps::earlier_ids(&catalog).ok())
        .unwrap_or_default();
    // Every decision this process makes without a table of its own reads these too.
    tilo_action_policy::earlier::register_all(&ids);
    *EARLIER.lock_recover() = Some((stamps, ids.clone()));
    ids
}

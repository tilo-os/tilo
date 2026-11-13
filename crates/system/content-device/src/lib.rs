//! `tilo-content-device`: the device content store.
//!
//! The filesystem daemon keeps this device's content and fetches what a collaborator
//! or another of the user's devices stored. The launcher hands an app a scoped
//! capability to it, and [`device()`] is the one store every caller in the process
//! shares.

use tilo_content::ContentStore;

#[cfg(unix)]
mod device;

#[cfg(unix)]
mod public;

#[cfg(unix)]
pub use device::DeviceBackend;
#[cfg(unix)]
pub use device::SessionBackend;
#[cfg(unix)]
pub use public::PublicCopy;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublicState {
    /// The whole copy, verified against its hash.
    Here {
        bytes: Vec<u8>,
        hash: String,
    },
    /// Kept and on its way, arriving for the first time or again after the device
    /// made room.
    Arriving,
    NotKept,
}

/// A public file on a host with no device to keep one on: there is never one.
#[cfg(not(unix))]
#[derive(Clone)]
pub struct PublicCopy(std::convert::Infallible);

#[cfg(not(unix))]
impl PublicCopy {
    /// # Errors
    ///
    /// Never: there is no copy to keep.
    pub fn keep(&self) -> std::io::Result<()> {
        match self.0 {}
    }

    pub fn release(&self) -> std::io::Result<()> {
        match self.0 {}
    }

    pub fn here(&self) -> std::io::Result<Option<String>> {
        match self.0 {}
    }

    pub fn read(&self) -> std::io::Result<PublicState> {
        match self.0 {}
    }
}

/// The device content store, when the launcher handed this process its capability.
/// Every returned store shares the one connection. `None` on hosts without device
/// transport; no storage fallback is opened.
#[must_use]
pub fn device() -> Option<ContentStore> {
    #[cfg(unix)]
    {
        device::store()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The public file `name` this device may keep a copy of, when the launcher handed
/// this process its capability.
#[must_use]
pub fn public_copy(name: &'static str) -> Option<PublicCopy> {
    #[cfg(unix)]
    {
        Some(PublicCopy::new(device::client()?.clone(), name))
    }
    #[cfg(not(unix))]
    {
        let _ = name;
        None
    }
}

/// The store an app uses unless it is handed another: the device content store
/// ([`device()`]), otherwise the store kept on this machine
/// ([`ContentStore::local_for_app`]).
#[must_use]
pub fn for_app() -> ContentStore {
    device().unwrap_or_else(ContentStore::local_for_app)
}

/// The content a session's apps read, as the session's own process: the daemon at
/// `TILO_FS_BLOB_SOCKET` (`fs serve-media`), serving an address when a workspace this
/// process draws references it ([`set_session_workspaces`]). `None` when no media is
/// served or no workspace names a scope.
#[must_use]
pub fn session(workspace: &str) -> Option<ContentStore> {
    #[cfg(unix)]
    {
        device::session_store(workspace)
    }
    #[cfg(not(unix))]
    {
        let _ = workspace;
        None
    }
}

/// Tell every store a session's process opens from here on which workspaces it draws,
/// the one in front first: a place of any of them reads its pictures through it.
///
/// Off unix no store opens for those workspaces, so the list is taken and dropped.
pub fn set_session_workspaces(workspaces: Vec<String>) {
    #[cfg(unix)]
    {
        device::set_session_workspaces(workspaces);
    }
    #[cfg(not(unix))]
    drop(workspaces);
}

#[cfg(test)]
#[cfg(not(unix))]
mod tests {
    #[test]
    fn device_store_is_unavailable_without_device_transport() {
        assert!(super::device().is_none());
    }
}

//! `tilo-content-device`: the device content store.
//!
//! The filesystem daemon keeps this device's content and fetches what a member
//! or another of the user's devices stored. The launcher hands an app a scoped
//! capability to it, and [`device()`] is the one store every caller in the process
//! shares.

#[cfg(not(target_arch = "wasm32"))]
use tilo_content::ContentBackend;
use tilo_content::ContentStore;

#[cfg(not(target_arch = "wasm32"))]
mod device;

#[cfg(not(target_arch = "wasm32"))]
mod public;

#[cfg(not(target_arch = "wasm32"))]
pub use device::DeviceBackend;
#[cfg(not(target_arch = "wasm32"))]
pub use device::SessionBackend;
#[cfg(not(target_arch = "wasm32"))]
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

/// A public file in a browser, which has no device to keep one on: there is never one.
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub struct PublicCopy(std::convert::Infallible);

#[cfg(target_arch = "wasm32")]
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
    #[cfg(not(target_arch = "wasm32"))]
    {
        device::store()
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// The public file `name` this device may keep a copy of, when the launcher handed
/// this process its capability.
#[must_use]
pub fn public_copy(name: &'static str) -> Option<PublicCopy> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(PublicCopy::new(device::client()?.clone(), name))
    }
    #[cfg(target_arch = "wasm32")]
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

/// The device content store over a connection the session opened on this process's
/// behalf, scoped to one app in one workspace: what a tilolang instance keeps and
/// reads, which the launcher never started with a capability of its own.
///
/// # Errors
///
/// The connection is not the daemon's document-media handoff.
#[cfg(not(target_arch = "wasm32"))]
pub fn for_connection(stream: tilo_socket::Stream) -> std::io::Result<ContentStore> {
    let client = runtime_fs::blobs::BlobClient::from_app_stream(stream)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(ContentStore::from_backend(
        std::sync::Arc::new(DeviceBackend::new(client)) as std::sync::Arc<dyn ContentBackend>,
    ))
}

/// The content a session's apps read, as the session's own process: the daemon at
/// `TILO_FS_BLOB_SOCKET` (`fs serve-media`), serving an address when a workspace this
/// process draws references it ([`set_session_workspaces`]). `None` when no media is
/// served or no workspace names a scope.
#[must_use]
pub fn session(workspace: &str) -> Option<ContentStore> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        device::session_store(workspace)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = workspace;
        None
    }
}

/// Tell every store a session's process opens from here on which workspaces it draws,
/// the one in front first: a place of any of them reads its pictures through it.
///
/// In a browser no store opens for those workspaces, so the list is taken and dropped.
pub fn set_session_workspaces(workspaces: Vec<String>) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        device::set_session_workspaces(workspaces);
    }
    #[cfg(target_arch = "wasm32")]
    drop(workspaces);
}

#[cfg(test)]
#[cfg(target_arch = "wasm32")]
mod tests {
    #[test]
    fn device_store_is_unavailable_without_device_transport() {
        assert!(super::device().is_none());
    }
}

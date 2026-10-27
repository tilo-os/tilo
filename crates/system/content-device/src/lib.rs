//! `tilo-content-device` — the device content store.
//!
//! The filesystem daemon keeps this device's content and fetches what a collaborator or another
//! of the user's devices stored. The launcher hands an app a scoped capability to it;
//! `DeviceBackend` is the [`ContentBackend`](tilo_content::ContentBackend) over that
//! capability, and [`device()`] the store every caller in the process shares.
//!
//! It is its own crate because it is a client of the daemon (`runtime-fs`): `tilo-content`
//! itself stays free of the daemon's protocol crates, so the resource layer and the types it
//! stores can name content without depending on the filesystem.

use tilo_content::ContentStore;

#[cfg(unix)]
mod device;

#[cfg(unix)]
mod public;

#[cfg(unix)]
pub use device::DeviceBackend;
#[cfg(unix)]
pub use public::PublicCopy;

/// What reading a public copy found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublicState {
    /// The whole copy, verified against its hash.
    Here { bytes: Vec<u8>, hash: String },
    /// Kept, and on its way: arriving for the first time, or again after
    /// the device made room.
    Arriving,
    /// Nobody on this device keeps it.
    NotKept,
}

/// A public file, on a host that has no device to keep one on: there is
/// never one.
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

    /// # Errors
    ///
    /// Never.
    pub fn release(&self) -> std::io::Result<()> {
        match self.0 {}
    }

    /// # Errors
    ///
    /// Never.
    pub fn here(&self) -> std::io::Result<Option<String>> {
        match self.0 {}
    }

    /// # Errors
    ///
    /// Never.
    pub fn read(&self) -> std::io::Result<PublicState> {
        match self.0 {}
    }
}

/// The device content store, when the launcher handed this process its capability.
/// Every returned store shares the one connection. Returns `None` on hosts without
/// device transport; no storage fallback is opened.
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

/// The public file `name` this device may keep a copy of, when the launcher handed this
/// process its capability.
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

/// The store an app uses unless it is handed another: the device content store when the
/// launcher handed this process its capability ([`device()`]), otherwise the store kept on
/// this machine ([`ContentStore::local_for_app`]).
#[must_use]
pub fn for_app() -> ContentStore {
    device().unwrap_or_else(ContentStore::local_for_app)
}

#[cfg(test)]
#[cfg(not(unix))]
mod tests {
    #[test]
    fn device_store_is_unavailable_without_device_transport() {
        assert!(super::device().is_none());
    }
}

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
pub use device::DeviceBackend;

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

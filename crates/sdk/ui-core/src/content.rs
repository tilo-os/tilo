//! Content by address: the store a host lends an app (`ViewCx::content`),
//! and in a native process the device store the launcher handed it, so an
//! app or a headless agent reaches content through the toolkit rather than
//! through the store's own crates.

pub use tilo_content::{ContentStore, valid_hash};

/// The device content store the launcher handed this process, or `None`
/// when it was handed none (started outside a Tilo session).
#[cfg(feature = "native")]
#[must_use]
pub fn device() -> Option<ContentStore> {
    tilo_content_device::device()
}

//! Content by address: the store a host lends an app (`ViewCx::content`),
//! and in a native process the device store the launcher handed it, so an
//! app or a headless agent reaches content through the toolkit rather than
//! through the store's own crates.

pub use tilo_content::{ContentStore, valid_hash};
pub use tilo_content_device::{PublicCopy, PublicState};

/// The device's one copy of the public file `name` (a catalogue the
/// filesystem daemon mirrors for every app and workspace), when this host
/// keeps such copies: `None` on one that keeps none (the web).
#[must_use]
pub fn public_copy(name: &'static str) -> Option<PublicCopy> {
    tilo_content_device::public_copy(name)
}

/// The device content store the launcher handed this process, or `None`
/// when it was handed none (started outside a Tilo session).
#[cfg(feature = "native")]
#[must_use]
pub fn device() -> Option<ContentStore> {
    tilo_content_device::device()
}

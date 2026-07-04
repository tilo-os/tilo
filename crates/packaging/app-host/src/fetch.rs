//! Remote fetch — stub.
//!
//! The v0 daemon is offline-only: callers hand it .qpkg bytes and it
//! parks them in the store. A future revision will resolve a remote
//! "qpkg://<hex>" URI to iroh-blobs, download the bundle, and hand the
//! bytes to [`crate::install::install_qpkg`]. Until that lands the
//! entry point below is a placeholder so the module layout is stable
//! for callers that already reference it.

use thiserror::Error;

use pkg::id::PackageId;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("remote fetch is not implemented in this build; use `install` with local .qpkg bytes")]
    NotImplemented,
}

/// Placeholder — returns `NotImplemented`. Real remote fetch is
/// tracked as a follow-up (iroh-blobs resolver + trust-set integration).
pub async fn fetch_remote(_id: PackageId) -> Result<Vec<u8>, FetchError> {
    Err(FetchError::NotImplemented)
}

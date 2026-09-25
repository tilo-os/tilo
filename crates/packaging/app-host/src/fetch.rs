//! Remote fetch — resolve an app-id through the registry protocol,
//! download the tapp over iroh-blobs, verify it fail-closed, and hand
//! the bytes to [`crate::install::install_tapp`].
//!
//! The heavy lifting (signed index, target selection, iroh transport,
//! signature/hash verification) lives in the `app-registry` crate; this
//! module is the daemon-side glue that turns a verified fetch into an
//! installed, registry-tracked package.

use std::path::Path;

use thiserror::Error;

use app_registry::HostProfile;
use app_registry::resolve::{self, FetchOutcome};

use crate::install::{self, InstallError};
use crate::registry::Pinning;
use crate::store::Store;
use tilo_app_package::id::PackageId;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("resolve/fetch {app_id}: {source}")]
    Resolve {
        app_id: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("install: {0}")]
    Install(#[from] InstallError),
}

/// Resolve + download + verify the tapp for `app_id` (optionally
/// constrained by a semver `version` requirement), then install it
/// into `store` with the requested pinning. Returns the installed
/// [`PackageId`].
///
/// `workspace_root`, when given, layers that workspace's
/// `registries.toml` over the global config so a workspace can add or
/// override registries.
///
/// # Errors
///
/// Returns [`FetchError::Resolve`] if resolving or downloading `app_id`
/// fails, or [`FetchError::Install`] if the fetched tapp fails to
/// install (bad signature, unpack failure, or a store write error).
pub async fn fetch_and_install(
    store: &Store,
    workspace_root: Option<&Path>,
    app_id: &str,
    version: Option<&str>,
    pinning: Pinning,
) -> Result<PackageId, FetchError> {
    let outcome = fetch_remote(workspace_root, app_id, version).await?;
    // `install_tapp` re-verifies and does blocking IO; the caller
    // (service task) already runs this off the capnp event loop.
    let id = install::install_tapp(store, &outcome.bytes, pinning)?;
    Ok(id)
}

/// Resolve + download + verify the tapp for `app_id` WITHOUT
/// installing. Bytes are returned only after the blake3 hash and the
/// publisher signature have been checked (fail-closed).
///
/// # Errors
///
/// Returns [`FetchError::Resolve`] if `app_id` cannot be resolved, the
/// download fails, or the fetched bytes fail hash/signature
/// verification.
pub async fn fetch_remote(
    workspace_root: Option<&Path>,
    app_id: &str,
    version: Option<&str>,
) -> Result<FetchOutcome, FetchError> {
    let host = HostProfile::detect();
    resolve::resolve_and_fetch(workspace_root, &host, app_id, version)
        .await
        .map_err(|source| FetchError::Resolve {
            app_id: app_id.to_string(),
            source,
        })
}

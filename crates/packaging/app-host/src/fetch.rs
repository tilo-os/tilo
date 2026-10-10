//! Remote fetch — resolve an app-id through the registry protocol,
//! download the tapp over iroh-blobs, verify it fail-closed, and install
//! it into the package store with [`crate::install::install_tapp`].
//!
//! The heavy lifting (signed index, target selection, iroh transport,
//! signature/hash verification) lives in the `app-registry` crate; this
//! module is the daemon-side glue that turns a verified fetch into an
//! installed, registry-tracked package.

use std::path::Path;

use thiserror::Error;

use app_registry::HostProfile;
use app_registry::resolve::{self, FetchOutcome};

use crate::install::InstallError;

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

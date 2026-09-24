//! Document media through the content-store capability the launcher hands
//! every app.
//!
//! [`install_from_environment`] puts a [`CachedContentStore`] over that
//! capability behind `qdoc::blobstore`, so every codec, widget and app that
//! stores or draws an image reaches the device content store without knowing
//! it exists. Without the capability (a unit test, a screenshot run, a
//! process started outside the launcher) `qdoc` keeps its process-local
//! store.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use qdoc::blobstore::{
    BlobStoreError, BlobStoreLimits, CachedContentStore, ContentStoreClient, Fetch,
};
use runtime_fs::blobs::{BlobClient, BlobError, ReadOutcome};

/// `qdoc`'s view of the capability.
struct Capability(BlobClient);

fn store_error(error: BlobError) -> BlobStoreError {
    match error {
        BlobError::TooLarge { len, limit } => BlobStoreError::ItemTooLarge {
            actual: usize::try_from(len).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
        },
        other => BlobStoreError::Unavailable(other.to_string()),
    }
}

impl ContentStoreClient for Capability {
    fn put(&self, bytes: &[u8], mime: &str) -> Result<String, BlobStoreError> {
        self.0
            .put(bytes, mime)
            .map(|stored| stored.hash)
            .map_err(store_error)
    }

    fn read(&self, hash: &str) -> Fetch {
        match self.0.read_all(hash) {
            Ok(ReadOutcome::Ready { bytes, .. }) => Fetch::Ready(bytes),
            Ok(ReadOutcome::Pending) => Fetch::Pending,
            Ok(ReadOutcome::Unavailable) => Fetch::Unavailable,
            Err(error) => {
                tracing::warn!(%error, "document media read failed");
                Fetch::Unavailable
            }
        }
    }

    fn has(&self, hash: &str) -> bool {
        self.0.has(hash).unwrap_or(false)
    }
}

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Back `qdoc::blobstore` with the launcher's content-store capability, if
/// this process was given one, and import the app's private legacy store in
/// the background. Returns whether a capability was installed. Idempotent:
/// the inherited descriptor is taken once.
pub fn install_from_environment() -> bool {
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return true;
    }
    let Ok(client) = BlobClient::from_environment() else {
        INSTALLED.store(false, Ordering::Release);
        return false;
    };
    install(client);
    let roots = legacy_roots(|key| std::env::var_os(key));
    if !roots.is_empty() {
        // A host that refuses background work retries on a later launch.
        let _ = crate::background::Background::current()
            .spawn("blob-import", move || import_legacy(&roots));
    }
    true
}

/// Back `qdoc::blobstore` with `client`.
pub fn install(client: BlobClient) {
    qdoc::blobstore::set_store(Arc::new(CachedContentStore::new(
        Arc::new(Capability(client)),
        BlobStoreLimits::default(),
    )));
}

/// Where apps kept document media before it moved into the content store:
/// a private store under the app's cache directory, or an explicit
/// `TILO_BLOB_STORE`. Both sit inside the app's own sandbox directory.
fn legacy_roots(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(explicit) = env("TILO_BLOB_STORE").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(explicit));
    }
    let cache = env("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".cache")));
    if let Some(cache) = cache {
        let root = cache.join("tilo").join("blobs");
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

fn import_legacy(roots: &[PathBuf]) {
    for root in roots {
        import_one(root);
    }
}

fn import_one(root: &Path) {
    match qdoc::blobstore::import_legacy_directory(root) {
        Ok(report) if !report.already_done => tracing::info!(
            root = %root.display(),
            imported = report.imported,
            skipped = report.skipped,
            "moved private document media into the content store"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            root = %root.display(), %error,
            "private document media not yet moved; retrying on next launch"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_roots_follow_the_old_store_location() {
        let roots = legacy_roots(|key| match key {
            "XDG_CACHE_HOME" => Some("/run/tilo/app/.cache".into()),
            "HOME" => Some("/run/tilo/app".into()),
            _ => None,
        });
        assert_eq!(
            roots,
            vec![PathBuf::from("/run/tilo/app/.cache/tilo/blobs")]
        );
        let roots = legacy_roots(|key| match key {
            "TILO_BLOB_STORE" => Some("/peer-blobs".into()),
            "HOME" => Some("/home/u".into()),
            _ => None,
        });
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/peer-blobs"),
                PathBuf::from("/home/u/.cache/tilo/blobs")
            ]
        );
    }
}

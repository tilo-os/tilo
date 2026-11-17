//! The content that documents projected as files refer to (an image in a document), kept in the
//! daemon's own [`ContentStore`] beside file content.
//!
//! The mount reads and writes it through [`tilo_content::ContentStore`], whose calls are
//! synchronous: FUSE worker threads make them, and the daemon's runtime while it seeds a mount.
//! Each call is handed to the store's own runtime and waited for here, so neither a FUSE thread
//! nor a daemon worker ever drives the store itself.

use std::io::Read;
use std::sync::Arc;

use iroh_blobs::Hash;
use tilo_content::{ContentBackend, ContentError, ContentReader, Stored};

use crate::store::ContentStore;

/// The daemon's content store as a [`tilo_content`] backend.
pub struct DaemonContent {
    store: Arc<ContentStore>,
    runtime: tokio::runtime::Handle,
}

/// The largest item a document keeps in the store: what the device content store accepts from an
/// app ([`runtime_fs::blobs::MAX_BLOB_BYTES`] is the same bound on the app side).
const MAX_ITEM_BYTES: usize = 256 * 1024 * 1024;

impl DaemonContent {
    /// The content of `store`, on the store's own runtime, or for the in-memory store (which
    /// has none) on `fallback`.
    #[must_use]
    pub fn new(store: Arc<ContentStore>, fallback: tokio::runtime::Handle) -> Self {
        let runtime = store.runtime().unwrap_or(fallback);
        Self { store, runtime }
    }

    /// As a [`tilo_content::ContentStore`], the type the mount keeps.
    #[must_use]
    pub fn into_store(self) -> tilo_content::ContentStore {
        tilo_content::ContentStore::new(self)
    }

    fn wait<T: Send + 'static>(
        &self,
        work: impl FnOnce(Arc<ContentStore>) -> futures_util::future::BoxFuture<'static, T>,
    ) -> Result<T, ContentError> {
        let (sent, answer) = std::sync::mpsc::sync_channel::<T>(1);
        let future = work(Arc::clone(&self.store));
        self.runtime.spawn(async move {
            let _ = sent.send(future.await);
        });
        answer
            .recv()
            .map_err(|_| ContentError::Io("the content store stopped".into()))
    }
}

fn parse(hash: &str) -> Option<Hash> {
    tilo_content::valid_hash(hash)
        .then(|| hash.to_ascii_lowercase().parse().ok())
        .flatten()
}

impl ContentBackend for DaemonContent {
    fn put(&self, reader: &mut dyn Read, _mime: &str) -> Result<Stored, ContentError> {
        let mut bytes = Vec::new();
        let sentinel = u64::try_from(MAX_ITEM_BYTES)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        reader.take(sentinel).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_ITEM_BYTES {
            return Err(ContentError::ItemTooLarge {
                actual: bytes.len(),
                limit: MAX_ITEM_BYTES,
            });
        }
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let hash = self
            .wait(move |store| Box::pin(async move { store.ingest(bytes).await }))?
            .map_err(|error| ContentError::Io(error.to_string()))?;
        Ok(Stored {
            hash: hash.to_hex(),
            len,
        })
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        let Some(hash) = parse(hash) else {
            return Err(ContentError::InvalidHash(hash.to_owned()));
        };
        let bytes = self.wait(move |store| Box::pin(async move { store.local(hash).await }))?;
        Ok(bytes.map(|bytes| Box::new(std::io::Cursor::new(bytes)) as ContentReader))
    }

    fn has(&self, hash: &str) -> bool {
        let Some(hash) = parse(hash) else {
            return false;
        };
        self.wait(move |store| Box::pin(async move { store.has(hash).await }))
            .unwrap_or(false)
    }

    fn max_item_bytes(&self) -> usize {
        MAX_ITEM_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn documents_keep_their_content_in_the_daemon_store() {
        let store = Arc::new(ContentStore::memory());
        let content =
            DaemonContent::new(Arc::clone(&store), tokio::runtime::Handle::current()).into_store();
        let bytes = b"an image a document holds".to_vec();
        let kept = tokio::task::spawn_blocking({
            let content = content.clone();
            let bytes = bytes.clone();
            move || content.unsealed().put(&bytes)
        })
        .await
        .expect("joins")
        .expect("stores");
        assert_eq!(kept, tilo_content::hash_bytes(&bytes));
        // What the mount stored is what the daemon serves to apps and peers.
        let hash: Hash = kept.parse().expect("a blake3 hash");
        assert_eq!(store.local(hash).await.as_deref(), Some(&bytes[..]));
        let read = tokio::task::spawn_blocking(move || content.read(&kept))
            .await
            .expect("joins");
        assert_eq!(read.as_deref(), Some(&bytes[..]));
    }
}

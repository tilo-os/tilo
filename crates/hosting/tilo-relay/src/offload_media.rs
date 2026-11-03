//! The workspaces' document media an offload route binds its inputs from and
//! keeps its outputs in, wherever this device's content store is.
//!
//! An offloadable action's inputs and outputs are document media: content
//! an app put for its workspace, named by the blake3 of its bytes
//! ([`runtime_fs::blobs`]). The route reaches it only through the scope of
//! the invoking app in the invoking workspace, exactly as that app would, so
//! possessing a hash never makes content an input: bytes another workspace, a
//! file or another device put on this device read as if they did not exist.
//!
//! A session that owns its profile's content store holds that media in
//! process ([`DocumentMedia`]), which is how a session reads and keeps what
//! it offers and what it answers.

use std::future::Future;
use std::pin::Pin;

use fs::document_media::DocumentMedia;
use runtime_fs::blobs::{BlobScope, ReadOutcome};

use crate::offload_content::BindError;

/// What a [`WorkspaceMedia`] call answers, later.
pub type MediaFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The document media of this device's workspaces, reached only through one
/// app's scope in one workspace.
pub trait WorkspaceMedia: Send + Sync + 'static {
    /// How many bytes `digest` is, when `scope`'s workspace reaches it and it is
    /// wholly on this device.
    ///
    /// # Errors
    ///
    /// [`BindError::NotLocal`] when `scope`'s workspace does not reach it here,
    /// and [`BindError::Media`] when the media did not answer.
    fn len<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>>;

    /// The whole of `digest` as `scope` reads it, checked against it.
    ///
    /// # Errors
    ///
    /// [`BindError::NotLocal`] when `scope`'s workspace does not reach it here,
    /// [`BindError::Integrity`] when the bytes do not hash to `digest`, and
    /// [`BindError::Media`] when the media did not answer.
    fn read<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>>;

    /// Keep `bytes` as `scope`'s media and return their digest.
    ///
    /// # Errors
    ///
    /// Why the store refused them.
    fn keep<'a>(
        &'a self,
        scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>>;
}

/// `bytes`, the whole of what was read as `digest`, checked against it.
fn checked(digest: [u8; 32], bytes: Vec<u8>, total: u64) -> Result<Vec<u8>, BindError> {
    if u64::try_from(bytes.len()).ok() != Some(total) {
        return Err(BindError::NotLocal);
    }
    if blake3::hash(&bytes).as_bytes() == &digest {
        Ok(bytes)
    } else {
        Err(BindError::Integrity)
    }
}

impl WorkspaceMedia for DocumentMedia {
    fn len<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>> {
        Box::pin(async move {
            self.len_of(scope, digest.into())
                .await
                .ok_or(BindError::NotLocal)
        })
    }

    fn read<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>> {
        Box::pin(async move {
            match self
                .read_range(scope.clone(), digest.into(), 0, u32::MAX)
                .await
            {
                ReadOutcome::Ready { bytes, total_len } => checked(digest, bytes, total_len),
                ReadOutcome::Pending | ReadOutcome::Unavailable => Err(BindError::NotLocal),
            }
        })
    }

    fn keep<'a>(
        &'a self,
        scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        Box::pin(async move {
            self.store(scope, bytes)
                .await
                .map(|hash| *hash.as_bytes())
                .map_err(|error| error.to_string())
        })
    }
}

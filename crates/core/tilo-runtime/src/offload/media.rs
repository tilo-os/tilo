//! The workspaces' document media an offload route keeps a turn's outputs
//! in, wherever this device's content store is.
//!
//! An offloadable action's outputs are document media: content kept for the
//! invoking app in its workspace, named by the blake3 of its bytes
//! ([`runtime_fs::blobs`]). The route reaches it only through that app's
//! scope, exactly as the app would.
//!
//! A session that owns its profile's content store holds that media in
//! process ([`DocumentMedia`]).

use std::future::Future;
use std::pin::Pin;

use runtime_fs::blobs::BlobScope;
use tilo_fs::document_media::DocumentMedia;

/// What a [`WorkspaceMedia`] call answers, later.
pub type MediaFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The document media of this device's workspaces, reached only through one
/// app's scope in one workspace.
pub trait WorkspaceMedia: Send + Sync + 'static {
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

impl WorkspaceMedia for DocumentMedia {
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

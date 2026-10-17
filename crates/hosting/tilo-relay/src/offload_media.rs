//! The rooms' document media an offload route binds its inputs from and
//! keeps its outputs in, wherever this device's content store is.
//!
//! An offloadable action's inputs and outputs are document media: content
//! an app put for its room, named by the blake3 of its bytes
//! ([`runtime_fs::blobs`]). The route reaches it only through the scope of
//! the invoking app in the invoking room, exactly as that app would, so
//! possessing a hash never makes content an input: bytes another room, a
//! file or another device put on this device read as if they did not exist.
//!
//! Two sessions keep that media in two places, and [`RoomMedia`] is both:
//!
//! - A session that owns its profile's content store holds it in process
//!   ([`DocumentMedia`]).
//! - The desktop session's apps keep theirs in the filesystem daemon, a
//!   separate process. The route reaches it as the desktop session already
//!   does to open content an app hands it: over the daemon's document-media
//!   socket, with the session principal, which hands out nothing but one
//!   app's scope in one room ([`SessionMedia`]). No new protocol, and no
//!   authority the session did not already have.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use fs::document_media::DocumentMedia;
use runtime_fs::blobs::{BlobClient, BlobError, BlobScope, ReadOutcome};

use crate::offload_content::BindError;
use tilo_lock::MutexRecover as _;

/// What a [`RoomMedia`] call answers, later.
pub type MediaFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The document media of this device's rooms, reached only through one
/// app's scope in one room.
pub trait RoomMedia: Send + Sync + 'static {
    /// How many bytes `digest` is, when `scope`'s room reaches it and it is
    /// wholly on this device.
    ///
    /// # Errors
    ///
    /// [`BindError::NotLocal`] when `scope`'s room does not reach it here,
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
    /// [`BindError::NotLocal`] when `scope`'s room does not reach it here,
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

impl RoomMedia for DocumentMedia {
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

/// The filesystem daemon's document media, as the desktop session reaches
/// it: over the daemon's socket with the session principal, through the
/// scope of the app that invoked the action in its room. Connects on first
/// use and again after the daemon went away. Each call blocks a thread of
/// the runtime's blocking pool for at most one local round trip per range.
#[derive(Clone)]
pub struct SessionMedia(Arc<SessionConnection>);

struct SessionConnection {
    socket: PathBuf,
    session: Mutex<Option<BlobClient>>,
}

impl SessionMedia {
    /// The daemon's document media at `socket`.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self(Arc::new(SessionConnection {
            socket: socket.into(),
            session: Mutex::new(None),
        }))
    }

    /// Run `call` with `scope`'s capability on a blocking thread.
    async fn scoped<T: Send + 'static>(
        &self,
        scope: &BlobScope,
        call: impl FnOnce(&BlobClient) -> Result<T, BlobError> + Send + 'static,
    ) -> Result<T, BlobError> {
        let connection = Arc::clone(&self.0);
        let scope = scope.clone();
        tokio::task::spawn_blocking(move || connection.scoped(&scope, call))
            .await
            .map_err(|_| BlobError::Disconnected("the document media call panicked".into()))?
    }
}

impl SessionConnection {
    fn scoped<T>(
        &self,
        scope: &BlobScope,
        call: impl FnOnce(&BlobClient) -> Result<T, BlobError>,
    ) -> Result<T, BlobError> {
        let mut session = self.session.lock_recover();
        let client = if let Some(client) = session.as_ref() {
            client.clone()
        } else {
            let client = BlobClient::connect_session(&self.socket)?;
            *session = Some(client.clone());
            client
        };
        drop(session);
        let result = client
            .scope(scope.room(), scope.app())
            .and_then(|scoped| call(&scoped));
        if matches!(result, Err(BlobError::Disconnected(_))) {
            // Reconnect on the next call rather than keep a dead session.
            *self.session.lock_recover() = None;
        }
        result
    }
}

impl RoomMedia for SessionMedia {
    fn len<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>> {
        Box::pin(async move {
            let hash = hex::encode(digest);
            match self.scoped(scope, move |scoped| scoped.size(&hash)).await {
                Ok(Some(len)) => Ok(len),
                Ok(None) => Err(BindError::NotLocal),
                Err(error) => Err(BindError::Media(error.to_string())),
            }
        })
    }

    fn read<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>> {
        Box::pin(async move {
            let hash = hex::encode(digest);
            match self
                .scoped(scope, move |scoped| scoped.read_all(&hash))
                .await
            {
                Ok(ReadOutcome::Ready { bytes, total_len }) => checked(digest, bytes, total_len),
                // Still arriving from a collaborator is not here yet.
                Ok(ReadOutcome::Pending | ReadOutcome::Unavailable) => Err(BindError::NotLocal),
                Err(error) => Err(BindError::Media(error.to_string())),
            }
        })
    }

    fn keep<'a>(
        &'a self,
        scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        Box::pin(async move {
            let kept = self
                .scoped(scope, move |scoped| {
                    scoped.put(&bytes, "application/octet-stream")
                })
                .await
                .map_err(|error| error.to_string())?;
            let mut digest = [0; 32];
            hex::decode_to_slice(&kept.hash, &mut digest)
                .map_err(|_| "the store answered a malformed content hash".to_string())?;
            Ok(digest)
        })
    }
}

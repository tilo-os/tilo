//! A home peer's replica file versions, read by a process beside it.
//!
//! `fs replica` keeps each workspace's [`ContentStore`] open for its whole
//! life, so another process on the device — the compute worker in the relay
//! daemon, which runs a job on the replica's copy of an input instead of
//! fetching it — cannot open the store. File-backed versions it reads from
//! their files ([`ContentStore::read_file_version_unlocked`]); smaller ones
//! are loose blobs inside the store's database, and those it asks the
//! replica for here.
//!
//! The channel is deliberately narrow:
//!
//! - **One store per socket.** Each workspace's replica serves its own store
//!   at [`socket_path`] inside its own cache directory; a request names a
//!   file version and nothing else, so it cannot reach another workspace.
//! - **Read-only and local.** The only request reads a version every byte of
//!   which is already on this device, checked against its manifest; nothing
//!   is fetched, written or listed.
//! - **The owner only.** The socket is owner-only and a peer of another uid
//!   is dropped unanswered.
//! - **Bounded.** Versions above [`MAX_SERVED_BYTES`] are refused (the
//!   reader has those from their files), frames are capped on both sides,
//!   reads run [`MAX_CONCURRENT_READS`] at a time, and a client gives up
//!   after [`CLIENT_TIMEOUT`].
//!
//! The answer is not trusted beyond that: the caller checks the bytes
//! against a hash it got elsewhere.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wire_frame::Framing;

use crate::content::FILE_BACKED_MIN;
use crate::store::ContentStore;

/// The socket's name inside a replica workspace's cache directory.
pub const SOCKET_NAME: &str = "replica-content.sock";

/// The largest version served: everything above it is file-backed and read
/// from its file instead.
pub const MAX_SERVED_BYTES: u64 = FILE_BACKED_MIN;

/// How many versions the replica reads at once; further requests wait.
pub const MAX_CONCURRENT_READS: usize = 8;

/// How long a client waits to connect, send, and be answered.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the replica keeps a connection that sends nothing.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Every message is one frame: a little-endian `u32` length, then the
/// postcard-encoded [`Request`] or [`Response`]. Room for a served version,
/// its length prefix and the response tag.
pub const FRAMING: Framing = Framing::little_endian(1024 * 1024 + 64);
const _: () = assert!(
    MAX_SERVED_BYTES == 1024 * 1024,
    "FRAMING fits a served version"
);

/// Where the replica of the workspace cached at `cache_dir` serves its
/// versions.
#[must_use]
pub fn socket_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(SOCKET_NAME)
}

/// The one request: the whole of file version `content` (its manifest
/// hash), if it is at most `max_bytes` long.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub content: [u8; 32],
    pub max_bytes: u64,
}

/// The answer to a [`Request`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// Every byte of the version, checked against its manifest.
    Found(#[serde(with = "serde_bytes")] Vec<u8>),
    /// The version is unknown, not wholly on this device, corrupt, or
    /// longer than the request or [`MAX_SERVED_BYTES`] allows.
    Absent,
}

/// Serve `store`'s file versions on `listener` for as long as the task
/// runs. The caller binds `listener` owner-only ([`socket_path`]).
pub async fn serve(listener: tokio::net::UnixListener, store: Arc<ContentStore>) {
    let reads = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_READS));
    let owner = rustix::process::geteuid().as_raw();
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // A transient accept failure (descriptor pressure) must not
            // spin the loop.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        if !stream
            .peer_cred()
            .is_ok_and(|credential| credential.uid() == owner)
        {
            continue;
        }
        tokio::spawn(answer(stream, Arc::clone(&store), Arc::clone(&reads)));
    }
}

/// Answer one connection's requests in turn until it closes, idles, or
/// sends something malformed.
async fn answer(
    mut stream: tokio::net::UnixStream,
    store: Arc<ContentStore>,
    reads: Arc<tokio::sync::Semaphore>,
) {
    let requests = FRAMING.with_max_len(256);
    while let Ok(Ok(Some(request))) = tokio::time::timeout(
        IDLE_TIMEOUT,
        requests.read_value_async::<_, Request>(&mut stream),
    )
    .await
    {
        let Ok(permit) = reads.acquire().await else {
            return;
        };
        let response = read_version(&store, &request).await;
        drop(permit);
        if FRAMING
            .write_value_async(&mut stream, &response)
            .await
            .is_err()
        {
            return;
        }
    }
}

/// `request`'s version from `store`, if it is wholly local, within bounds,
/// and matches its manifest. Never fetches.
async fn read_version(store: &ContentStore, request: &Request) -> Response {
    let content = iroh_blobs::Hash::from_bytes(request.content);
    let Some(manifest) = store.manifest(content).await else {
        return Response::Absent;
    };
    let len = manifest.total_len();
    if len > request.max_bytes.min(MAX_SERVED_BYTES) {
        return Response::Absent;
    }
    match store.read_local(content, &manifest, 0..len).await {
        Some(bytes) if manifest.verify(&bytes) => Response::Found(bytes),
        _ => Response::Absent,
    }
}

/// The whole of file version `content` from the replica serving the
/// workspace cached at `cache_dir`, if it holds it wholly and it is at most
/// `max_bytes` (and [`MAX_SERVED_BYTES`]) long. Blocking; call it off an
/// async runtime.
///
/// `Ok(None)` too when no replica is serving there. The bytes are what the
/// replica answered: check them against a hash you trust.
///
/// # Errors
///
/// Returns an error when the replica is there but the exchange fails or
/// times out.
pub fn read(cache_dir: &Path, content: [u8; 32], max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    use std::os::unix::net::UnixStream;
    let mut stream = match UnixStream::connect(socket_path(cache_dir)) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    let request = Request {
        content,
        max_bytes: max_bytes.min(MAX_SERVED_BYTES),
    };
    FRAMING.write_value(&mut stream, &request)?;
    match FRAMING.read_value::<_, Response>(&mut stream)? {
        Some(Response::Found(bytes))
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= request.max_bytes =>
        {
            Ok(Some(bytes))
        }
        Some(Response::Found(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "replica answered more bytes than asked for",
        )),
        Some(Response::Absent) => Ok(None),
        None => Err(io::ErrorKind::UnexpectedEof.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listen(cache_dir: &Path) -> tokio::net::UnixListener {
        let listener = tilo_files::native::private::bind_socket(&socket_path(cache_dir)).unwrap();
        listener.set_nonblocking(true).unwrap();
        tokio::net::UnixListener::from_std(listener).unwrap()
    }

    /// `read`, off the runtime as its callers run it.
    async fn ask(cache_dir: &Path, content: [u8; 32], max_bytes: u64) -> Option<Vec<u8>> {
        let cache_dir = cache_dir.to_owned();
        tokio::task::spawn_blocking(move || read(&cache_dir, content, max_bytes))
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_replica_serves_its_own_small_versions_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let cache_dir = root.path().join("replica");
        let store = Arc::new(ContentStore::open(&cache_dir).await.unwrap());
        let small = b"a small photograph kept as loose blobs".to_vec();
        let small_version = *store
            .write_content(&small)
            .await
            .unwrap()
            .unwrap()
            .as_bytes();
        let large: Vec<u8> = (0..2 * 1024 * 1024_u32)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let large_version = *store
            .write_content(&large)
            .await
            .unwrap()
            .unwrap()
            .as_bytes();
        let server = tokio::spawn(serve(listen(&cache_dir), Arc::clone(&store)));
        let len = u64::try_from(small.len()).unwrap();

        assert_eq!(
            ask(&cache_dir, small_version, u64::MAX).await,
            Some(small.clone())
        );
        assert_eq!(ask(&cache_dir, small_version, len).await, Some(small));
        assert_eq!(
            ask(&cache_dir, small_version, len - 1).await,
            None,
            "over the bound"
        );
        assert_eq!(ask(&cache_dir, [7; 32], u64::MAX).await, None, "unknown");
        // File-backed versions are read from their files, never sent here.
        assert_eq!(ask(&cache_dir, large_version, u64::MAX).await, None);

        // Many at once all get answered, a few reads at a time.
        let asks = (0..64)
            .map(|_| ask(&cache_dir, small_version, u64::MAX))
            .collect::<Vec<_>>();
        for answer in futures_util::future::join_all(asks).await {
            assert!(answer.is_some());
        }
        server.abort();
    }

    #[test]
    fn without_a_replica_serving_nothing_is_held() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(read(root.path(), [1; 32], u64::MAX).unwrap(), None);
        // A socket nobody listens on any more.
        drop(std::os::unix::net::UnixListener::bind(socket_path(root.path())).unwrap());
        assert_eq!(read(root.path(), [1; 32], u64::MAX).unwrap(), None);
    }
}

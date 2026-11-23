//! One process holds a profile's documents, and every other process that opens them follows the
//! holder.
//!
//! A profile keeps its documents in one directory, one document each, as [`FsStore`] lays them out.
//! The process holding them is their only writer, and leases them through [`FsStore`]. The others
//! read the same documents: they notice the holder's saves with [`Follow`], and hand it their own
//! writes through its [`Socket`]. A target without files, locks or local sockets (a browser page)
//! holds what it opens in memory and serves no socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::DOCUMENT_FILE;
use durable_store::{DurableStore, StorageKey};

/// The documents `keys` name in `store`, followed: what a follower compares to notice the holder's
/// saves, and watches to hear of them at once.
pub struct Follow {
    documents: Vec<PathBuf>,
}

impl Follow {
    /// Follow the documents `keys` name in `store`. Each is a directory holding
    /// [`DOCUMENT_FILE`]; a document the holder has not made yet has one too, and the watch arms
    /// when it appears. Keys a store keeps nowhere locally are not followed; a follower then
    /// reads its holder's documents through the store.
    pub fn of(store: &dyn DurableStore, keys: &[StorageKey]) -> Self {
        let documents = keys
            .iter()
            .filter_map(|key| store.local_path(key))
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect();
        Self { documents }
    }

    /// Where the documents stand: what the platform reports of each document file, so a follower
    /// takes a save in once.
    pub fn stamp(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for document in &self.documents {
            tilo_files::native::stamp(&document.join(DOCUMENT_FILE)).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Call `on_change` when the holder saves one of the documents: a follower's watch on its
    /// holder. `None` where nothing can be watched (a target without watches, or a directory the
    /// holder has not made yet); the follower then looks before each read instead.
    pub fn watch(&self, on_change: impl Fn() + Send + Sync + 'static) -> Option<Watch> {
        #[cfg(feature = "watch")]
        {
            let on_change = Arc::new(on_change);
            let mut watches = Vec::new();
            for document in &self.documents {
                let on_change = Arc::clone(&on_change);
                let watch = tilo_files::native::watch_changes(
                    document,
                    |path| path.file_name().is_some_and(|name| name == DOCUMENT_FILE),
                    move || on_change(),
                )
                .ok()?;
                watches.push(watch);
            }
            Some(Watch { _watches: watches })
        }
        #[cfg(not(feature = "watch"))]
        {
            let _ = on_change;
            None
        }
    }
}

/// A follower's watch on the holder's documents; dropping it stops the news.
pub struct Watch {
    #[cfg(feature = "watch")]
    _watches: Vec<tilo_files::native::ChangeWatch>,
}

/// The holder's socket: each request another process sends is answered with what `answer` makes
/// of it, on a thread of its own. However deep the profile lives, its socket opens.
pub struct Socket {
    socket: PathBuf,
    closed: Arc<AtomicBool>,
}

impl Socket {
    /// # Errors
    /// The socket cannot be opened (`Unsupported` on a target without one), so no other process
    /// can hand this one its writes.
    pub fn open(
        socket: impl Into<PathBuf>,
        answer: impl Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        socket::open(socket.into(), Box::new(answer))
    }

    /// Send `request` through the holder's socket and read its answer.
    ///
    /// # Errors
    /// No holder answers: `NotFound` where it never opened a socket, `Unsupported` on a target
    /// without one, else what the socket said.
    pub fn send(socket: &Path, request: &[u8]) -> std::io::Result<Vec<u8>> {
        socket::send(socket, request)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        socket::close(&self.socket);
    }
}

type Answer = Box<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;

#[cfg(unix)]
mod socket {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Answer, Socket};

    const MAX_MESSAGE: u64 = 4 * 1024 * 1024;
    /// The thread that serves the holder's requests, as the thread dump names it.
    const THREAD: &str = "tilo-held-socket";

    pub(super) fn open(socket: PathBuf, answer: Answer) -> std::io::Result<Socket> {
        // The socket of a holder that ended without closing it.
        match std::fs::remove_file(&socket) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let listener = tilo_files::native::socket::bind(&socket)?;
        let closed = Arc::new(AtomicBool::new(false));
        let watching = Arc::clone(&closed);
        tilo_rt::spawn_thread(THREAD, move || {
            for stream in listener.incoming() {
                if watching.load(Ordering::Acquire) {
                    return;
                }
                if let Ok(stream) = stream {
                    serve(&stream, &answer);
                }
            }
        })?;
        Ok(Socket { socket, closed })
    }

    fn serve(stream: &UnixStream, answer: &Answer) {
        let mut line = Vec::new();
        if BufReader::new(stream.take(MAX_MESSAGE))
            .read_until(b'\n', &mut line)
            .is_err()
        {
            return;
        }
        let mut reply = answer(line.strip_suffix(b"\n").unwrap_or(&line));
        reply.push(b'\n');
        let _ = (&*stream).write_all(&reply);
    }

    pub(super) fn send(socket: &Path, request: &[u8]) -> std::io::Result<Vec<u8>> {
        let stream = tilo_files::native::socket::connect(socket)?;
        let mut body = request.to_vec();
        body.push(b'\n');
        (&stream).write_all(&body)?;
        let mut line = Vec::new();
        BufReader::new((&stream).take(MAX_MESSAGE)).read_until(b'\n', &mut line)?;
        if line.pop() != Some(b'\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the holder closed the socket without an answer",
            ));
        }
        Ok(line)
    }

    pub(super) fn close(socket: &Path) {
        drop(tilo_files::native::socket::connect(socket));
        let _ = std::fs::remove_file(socket);
    }
}

#[cfg(not(unix))]
mod socket {
    use std::path::{Path, PathBuf};

    use super::{Answer, Socket};

    pub(super) fn open(_socket: PathBuf, _answer: Answer) -> std::io::Result<Socket> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no socket to another process's documents here",
        ))
    }

    pub(super) fn send(_socket: &Path, _request: &[u8]) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no socket to another process's documents here",
        ))
    }

    pub(super) fn close(_socket: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FsStore;
    use durable_store::WriteOp;

    fn key(name: &str) -> StorageKey {
        StorageKey::app(name)
    }

    #[test]
    fn a_followers_stamp_changes_when_the_holder_saves() {
        let root = tempfile::tempdir().unwrap();
        let store = FsStore::new(root.path());
        let follow = Follow::of(&store, &[key("person"), key("device")]);
        let before = follow.stamp();
        let held = store.lease(&key("person")).unwrap();
        store
            .write(&key("person"), WriteOp::Replace(vec![7; 16]), None)
            .unwrap();
        store.flush(None).unwrap();
        drop(held);
        assert_ne!(follow.stamp(), before);
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_answers_a_request_and_closes_when_dropped_at_any_depth() {
        let root = tempfile::tempdir().unwrap();
        socket_answers_and_closes(&root.path().join("held.sock"));
        // A profile deep enough that the path is past what a Unix socket
        // address can hold still gets one.
        let deep = root.path().join("a".repeat(60)).join("b".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        let socket = deep.join("held.sock");
        assert!(socket.as_os_str().len() > 108, "{}", socket.display());
        socket_answers_and_closes(&socket);
    }

    #[cfg(unix)]
    fn socket_answers_and_closes(socket: &Path) {
        let holder = Socket::open(socket, |request| [b"seen ", request].concat()).unwrap();
        assert_eq!(Socket::send(socket, b"hello").unwrap(), b"seen hello");
        drop(holder);
        assert_eq!(
            Socket::send(socket, b"again").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}

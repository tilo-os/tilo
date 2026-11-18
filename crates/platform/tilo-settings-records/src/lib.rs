//! Where a profile's settings documents live, for `settings-store`, which keeps
//! what they hold and compiles unchanged for every target.
//!
//! A profile keeps its documents in `<root>/records/`, one directory each, as the
//! device's document store lays them out: the process holding the profile is
//! their only writer there. The others read the same documents, notice the
//! holder's saves ([`Records::watch`]) and hand it their writes through its socket
//! ([`Records::open_socket`], [`Records::send`]). A target without files, locks or
//! local sockets (a browser page) reads nothing, holds what it opens and serves
//! no socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const RECORDS_DIR: &str = "records";
/// The document whose lock says which process holds the profile, relative to the
/// documents directory: the person's, which the holder leases first. It stays
/// behind when the holder exits.
#[cfg(not(target_arch = "wasm32"))]
pub const LOCK_FILE: &str = "person/canonical.lock";
const SOCKET_FILE: &str = "settings.sock";

#[must_use]
pub fn profile_root() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join("tilo"))
}

#[derive(Clone, Debug)]
pub struct Records {
    dir: PathBuf,
}

pub struct Watch {
    #[cfg(feature = "watch")]
    _watches: Vec<tilo_files::native::ChangeWatch>,
}

pub struct Socket {
    socket: PathBuf,
    closed: Arc<AtomicBool>,
}

impl Records {
    #[must_use]
    pub fn of(root: &Path) -> Self {
        Self {
            dir: root.join(RECORDS_DIR),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The holder's lock and socket, held while the process holds the profile,
    /// never part of the documents it keeps.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn runtime_files(&self) -> [PathBuf; 2] {
        [self.dir.join(LOCK_FILE), self.dir.join(SOCKET_FILE)]
    }

    /// Call `on_change` when the holder saves one of the `documents` (each a
    /// directory beneath [`Self::dir`] holding `file`): a follower's watch on its
    /// holder. `None` where nothing can be watched, such as a document the holder
    /// has not made yet; the follower then looks before each read.
    pub fn watch(
        &self,
        documents: &'static [&'static str],
        file: &'static str,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Option<Watch> {
        #[cfg(feature = "watch")]
        {
            let on_change = Arc::new(on_change);
            let mut watches = Vec::new();
            for document in documents {
                let on_change = Arc::clone(&on_change);
                let watch = tilo_files::native::watch_changes(
                    &self.dir.join(document),
                    move |path| path.file_name().is_some_and(|name| name == file),
                    move || on_change(),
                )
                .ok()?;
                watches.push(watch);
            }
            Some(Watch { _watches: watches })
        }
        #[cfg(not(feature = "watch"))]
        {
            let _ = (documents, file, on_change);
            None
        }
    }

    /// Open the holder's socket: each request another process sends is answered
    /// with what `answer` makes of it, on a thread of its own. However
    /// deep the profile lives, its socket opens.
    ///
    /// # Errors
    /// The socket cannot be opened (`Unsupported` on a target without one), so no
    /// other process can hand this one its writes.
    pub fn open_socket(
        &self,
        answer: impl Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
    ) -> std::io::Result<Socket> {
        socket::open(&self.dir.join(SOCKET_FILE), Box::new(answer))
    }

    /// Send `request` through the holder's socket and read its answer.
    ///
    /// # Errors
    /// No holder answers: `NotFound` where it never opened a socket,
    /// `Unsupported` on a target without one, else what the socket said.
    pub fn send(&self, request: &[u8]) -> std::io::Result<Vec<u8>> {
        socket::send(&self.dir.join(SOCKET_FILE), request)
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
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Answer, Socket};

    const MAX_MESSAGE: u64 = 4 * 1024 * 1024;

    pub(super) fn open(socket: &Path, answer: Answer) -> std::io::Result<Socket> {
        // The socket of a holder that ended without closing it.
        match std::fs::remove_file(socket) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let listener = tilo_files::native::socket::bind(socket)?;
        let closed = Arc::new(AtomicBool::new(false));
        let watching = Arc::clone(&closed);
        tilo_rt::spawn_thread("tilo-settings-socket", move || {
            for stream in listener.incoming() {
                if watching.load(Ordering::Acquire) {
                    return;
                }
                if let Ok(stream) = stream {
                    serve(&stream, &answer);
                }
            }
        })?;
        Ok(Socket {
            socket: socket.to_path_buf(),
            closed,
        })
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
    use std::path::Path;

    use super::{Answer, Socket};

    pub(super) fn open(_socket: &Path, _answer: Answer) -> std::io::Result<Socket> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no socket to another process's settings here",
        ))
    }

    pub(super) fn send(_socket: &Path, _request: &[u8]) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no socket to another process's settings here",
        ))
    }

    pub(super) fn close(_socket: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_socket_answers_a_request_and_closes_when_dropped() {
        let root = tempfile::tempdir().unwrap();
        socket_answers_and_closes(&Records::of(root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn a_profile_deeper_than_a_socket_path_still_has_a_socket() {
        let root = tempfile::tempdir().unwrap();
        let records = Records::of(&root.path().join("a".repeat(60)).join("b".repeat(60)));
        let [_, socket] = records.runtime_files();
        assert!(socket.as_os_str().len() > 108, "{}", socket.display());
        socket_answers_and_closes(&records);
    }

    #[cfg(unix)]
    fn socket_answers_and_closes(records: &Records) {
        std::fs::create_dir_all(records.dir()).unwrap();
        let socket = records
            .open_socket(|request| [b"seen ", request].concat())
            .unwrap();
        assert_eq!(records.send(b"hello").unwrap(), b"seen hello");
        drop(socket);
        assert_eq!(
            records.send(b"again").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}

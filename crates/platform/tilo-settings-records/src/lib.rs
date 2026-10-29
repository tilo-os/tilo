//! Where a profile's settings documents live, for `settings-store`, which
//! keeps what they hold and compiles unchanged for every target.
//!
//! A profile keeps its documents in `<root>/records/`. One process holds
//! them at a time ([`Records::try_hold`]); the others read the same files,
//! notice the holder's saves ([`Records::stamp`], [`Records::watch`]) and
//! hand it their writes through its door ([`Records::open_door`],
//! [`Records::knock`]). A target without files, locks or local sockets (a
//! browser page) reads nothing, holds what it opens and has no door.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The directory below a profile's root holding its documents.
pub const RECORDS_DIR: &str = "records";
/// The holder's lock in the documents directory, which a browser page has
/// none of. It stays behind when the holder exits.
#[cfg(not(target_arch = "wasm32"))]
pub const LOCK_FILE: &str = "settings.lock";
const DOOR_FILE: &str = "settings.sock";

/// The root of this person's own settings profile: `tilo` below the XDG
/// config home.
#[must_use]
pub fn profile_root() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join("tilo"))
}

/// One profile's documents directory.
#[derive(Clone, Debug)]
pub struct Records {
    dir: PathBuf,
}

/// What tells two saves of a document apart without reading it: its size
/// and modification time, or nothing where it does not exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp(Option<(u64, std::time::SystemTime)>);

/// Whether this process could take the profile.
pub enum Hold {
    /// It holds it for as long as it keeps the [`Holding`].
    Held(Holding),
    /// Another process holds it.
    Busy,
    /// There is nothing to lock (no directory, or a target without locks):
    /// nobody else can hold it, so this process does, in memory.
    Unlockable,
}

/// The lock of the process holding a profile. A browser page has no
/// locks, and holds what it opens ([`Hold::Unlockable`]).
pub struct Holding {
    #[cfg(not(target_arch = "wasm32"))]
    _lock: tilo_files::Lock,
}

/// A follower's watch on the holder's saves; ends when dropped.
pub struct Watch {
    #[cfg(feature = "watch")]
    _watch: tilo_files::native::ChangeWatch,
}

/// The holder's door for other processes' writes; closes when dropped.
pub struct Door {
    socket: PathBuf,
    closed: Arc<AtomicBool>,
}

impl Records {
    /// The documents of the profile whose root is `root`.
    #[must_use]
    pub fn of(root: &Path) -> Self {
        Self {
            dir: root.join(RECORDS_DIR),
        }
    }

    /// The documents' directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The holder's lock and door. They belong to the process holding the
    /// profile while it runs, never to the documents it keeps.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn runtime_files(&self) -> [PathBuf; 2] {
        [self.dir.join(LOCK_FILE), self.dir.join(DOOR_FILE)]
    }

    /// Take the profile if no other process holds it.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn try_hold(&self) -> Hold {
        Hold::Unlockable
    }

    /// Take the profile once the process holding it lets it go.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn wait_to_hold(&self) -> Hold {
        Hold::Unlockable
    }

    /// Take the profile if no other process holds it.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn try_hold(&self) -> Hold {
        let lock = self.dir.join(LOCK_FILE);
        match tilo_files::native::lock::prepare_parent(&lock)
            .ok()
            .and_then(|()| tilo_files::file_at(&lock).ok())
        {
            Some((dir, name)) => {
                match tilo_files::blocking::try_lock(&dir, &name, tilo_files::LockMode::Exclusive) {
                    Ok(Some(lock)) => Hold::Held(Holding { _lock: lock }),
                    Ok(None) => Hold::Busy,
                    Err(_) => Hold::Unlockable,
                }
            }
            None => Hold::Unlockable,
        }
    }

    /// Take the profile once the process holding it lets it go.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn wait_to_hold(&self) -> Hold {
        match tilo_files::file_at(&self.dir.join(LOCK_FILE)).and_then(|(dir, name)| {
            tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive)
        }) {
            Ok(lock) => Hold::Held(Holding { _lock: lock }),
            Err(_) => Hold::Busy,
        }
    }

    /// The bytes of document `name`; `None` where it was never saved.
    ///
    /// # Errors
    /// It exists and cannot be read.
    pub fn read(&self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.dir.join(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// What tells this save of document `name` from the next.
    #[must_use]
    pub fn stamp(&self, name: &str) -> Stamp {
        Stamp(
            std::fs::metadata(self.dir.join(name))
                .ok()
                .and_then(|meta| Some((meta.len(), meta.modified().ok()?))),
        )
    }

    /// Replace document `name` with `bytes`, crash-safely.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn write(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        tilo_files::native::atomic::write(&self.dir.join(name), bytes)
    }

    /// Call `on_change` when one of the documents `names` is saved: a
    /// follower's watch on its holder. `None` where nothing can be watched;
    /// the follower then looks before each read.
    pub fn watch(
        &self,
        names: &'static [&'static str],
        on_change: impl Fn() + Send + 'static,
    ) -> Option<Watch> {
        #[cfg(feature = "watch")]
        {
            tilo_files::native::watch_changes(
                &self.dir,
                move |path| {
                    path.file_name()
                        .is_some_and(|name| names.iter().any(|known| name == *known))
                },
                on_change,
            )
            .ok()
            .map(|watch| Watch { _watch: watch })
        }
        #[cfg(not(feature = "watch"))]
        {
            let _ = (names, on_change);
            None
        }
    }

    /// Open the holder's door: each request another process sends is
    /// answered with what `answer` makes of it, on a thread of the door's
    /// own. `None` where there is no door to open.
    pub fn open_door(
        &self,
        answer: impl Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
    ) -> Option<Door> {
        door::open(&self.dir.join(DOOR_FILE), Box::new(answer))
    }

    /// Send `request` through the holder's door and read its answer.
    ///
    /// # Errors
    /// No holder answers: `NotFound` where it never opened a door,
    /// `Unsupported` on a target without one, else what the socket said.
    pub fn knock(&self, request: &[u8]) -> std::io::Result<Vec<u8>> {
        door::knock(&self.dir.join(DOOR_FILE), request)
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        door::close(&self.socket);
    }
}

type Answer = Box<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;

#[cfg(unix)]
mod door {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Answer, Door};

    /// The largest request or answer that crosses the door: settings are small.
    const MAX_MESSAGE: u64 = 4 * 1024 * 1024;

    pub(super) fn open(socket: &Path, answer: Answer) -> Option<Door> {
        let _ = std::fs::remove_file(socket);
        let listener = match UnixListener::bind(socket) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(socket = %socket.display(), %error, "settings take no writes from other processes");
                return None;
            }
        };
        let closed = Arc::new(AtomicBool::new(false));
        let watching = Arc::clone(&closed);
        tilo_rt::spawn_thread("tilo-settings-door", move || {
            for stream in listener.incoming() {
                if watching.load(Ordering::Acquire) {
                    return;
                }
                if let Ok(stream) = stream {
                    serve(&stream, &answer);
                }
            }
        })
        .ok()?;
        Some(Door {
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

    pub(super) fn knock(socket: &Path, request: &[u8]) -> std::io::Result<Vec<u8>> {
        let stream = UnixStream::connect(socket)?;
        let mut body = request.to_vec();
        body.push(b'\n');
        (&stream).write_all(&body)?;
        let mut line = Vec::new();
        BufReader::new((&stream).take(MAX_MESSAGE)).read_until(b'\n', &mut line)?;
        if line.pop() != Some(b'\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the holder closed the door without an answer",
            ));
        }
        Ok(line)
    }

    /// Wake the door's thread, which finds it closed and ends.
    pub(super) fn close(socket: &Path) {
        drop(UnixStream::connect(socket));
        let _ = std::fs::remove_file(socket);
    }
}

#[cfg(not(unix))]
mod door {
    use std::path::Path;

    use super::{Answer, Door};

    pub(super) fn open(_socket: &Path, _answer: Answer) -> Option<Door> {
        None
    }

    pub(super) fn knock(_socket: &Path, _request: &[u8]) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no door to another process's settings here",
        ))
    }

    pub(super) fn close(_socket: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_process_holds_a_profile_and_reads_back_what_it_wrote() {
        let root = tempfile::tempdir().unwrap();
        let records = Records::of(root.path());
        assert!(records.read("person.automerge").unwrap().is_none());
        let held = records.try_hold();
        assert!(
            matches!(held, Hold::Held(_)),
            "nobody else holds a fresh profile"
        );
        let before = records.stamp("person.automerge");
        records.write("person.automerge", b"bytes").unwrap();
        assert_ne!(records.stamp("person.automerge"), before);
        assert_eq!(
            records.read("person.automerge").unwrap().as_deref(),
            Some(&b"bytes"[..])
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_door_answers_a_knock_and_closes_when_dropped() {
        let root = tempfile::tempdir().unwrap();
        let records = Records::of(root.path());
        std::fs::create_dir_all(records.dir()).unwrap();
        let door = records
            .open_door(|request| [b"seen ", request].concat())
            .unwrap();
        assert_eq!(records.knock(b"hello").unwrap(), b"seen hello");
        drop(door);
        assert_eq!(
            records.knock(b"again").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}

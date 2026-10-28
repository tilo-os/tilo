//! The session's `workspace-id` file, naming the room of the workspace in
//! front, written off the compositor thread.
//!
//! A workspace switch changes the room, and the compositor used to replace
//! the file on its input path: an atomic replacement syncs the file and its
//! directory, which on a busy disk held the frame loop for 1.87 s inside one
//! workspace switch. The file only matters to commands started
//! afterwards (an app run from a terminal, `tilo-app dev`), so a writer
//! thread replaces it with the newest room and the frame loop only hands
//! over a string. It is not synced either: it lives in the runtime
//! directory, which a crash empties, and a sync forces a journal commit that
//! every other write on the filesystem then waits for, the compositor
//! thread's own included.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

/// The newest room the frame loop asked for, published by a writer thread.
/// Dropping it waits for the writer and removes the file it wrote.
pub struct PublishedRoom {
    room: Option<String>,
    /// Each request names a room, or `None` while no workspace is active.
    requests: Option<mpsc::Sender<Option<String>>>,
    writer: Option<JoinHandle<()>>,
}

impl PublishedRoom {
    /// Publish to `path`. `None` when the writer thread cannot start; the
    /// session then has no `workspace-id` file, as where it has no place.
    pub fn at(path: PathBuf) -> Option<Self> {
        let (requests, received) = mpsc::channel();
        let writer = std::thread::Builder::new()
            .name("tilo-workspace-id".into())
            .spawn(move || write_requests(&path, &received));
        match writer {
            Ok(writer) => Some(Self {
                room: None,
                requests: Some(requests),
                writer: Some(writer),
            }),
            Err(error) => {
                tracing::warn!(%error, "session room cannot be published");
                None
            }
        }
    }

    /// Name `room` from now on. Returns at once; nothing touches the disk
    /// on the caller's thread.
    pub fn follow(&mut self, room: Option<String>) {
        if room == self.room {
            return;
        }
        self.room.clone_from(&room);
        if let Some(requests) = &self.requests {
            let _ = requests.send(room);
        }
    }
}

impl Drop for PublishedRoom {
    fn drop(&mut self) {
        // Closing the channel ends the writer, which removes what it wrote.
        drop(self.requests.take());
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// The writer thread: skip to the newest request, replace or remove the
/// file, and remove it when the session ends.
fn write_requests(path: &Path, received: &mpsc::Receiver<Option<String>>) {
    let mut written = false;
    while let Ok(first) = received.recv() {
        let room = received.try_iter().last().unwrap_or(first);
        written = publish(path, room.as_deref());
    }
    if written {
        let _ = std::fs::remove_file(path);
    }
}

/// Replace or remove the file; whether it now names a room.
fn publish(path: &Path, room: Option<&str>) -> bool {
    let result = match room {
        Some(room) => {
            tilo_files::native::atomic::write_regenerable_with_mode(path, room.as_bytes(), 0o600)
                .map_err(|error| error.to_string())
        }
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    };
    if let Err(error) = result {
        // Not retried: the next workspace change tries again.
        tracing::warn!(%error, path = %path.display(), "session room not published");
    }
    room.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Wait for the writer thread to leave `path` reading `expected`
    /// (`None`: absent). A loaded test machine can starve the writer for
    /// seconds, so only the shared hang bound calls it stuck.
    fn eventually(path: &Path, expected: Option<&str>) {
        tilo_test_env::hang::wait_until(format_args!("workspace-id to read {expected:?}"), || {
            std::fs::read_to_string(path).ok().as_deref() == expected
        });
    }

    #[test]
    fn the_room_in_front_is_published_off_the_caller_thread_and_removed_at_the_end() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("workspace-id");
        let mut published = PublishedRoom::at(path.clone()).expect("writer thread");

        published.follow(Some("room-a".into()));
        published.follow(Some("room-b".into()));
        eventually(&path, Some("room-b"));
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the room is the session's alone");

        published.follow(None);
        // No active workspace, no file.
        eventually(&path, None);

        published.follow(Some("room-c".into()));
        drop(published);
        assert!(!path.exists(), "the session's end removes the file");
    }
}

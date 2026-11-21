//! A process that unmounts a FUSE mount when the one that mounted it ends, however it ends.
//!
//! fuser watches a mount it made only with `AutoUnmount`, and refuses that with an
//! owner-only mount ([`fuser::SessionACL::Owner`]): fusermount serves auto-unmount for a
//! shared mount alone (`allow_other` or `allow_root`), which would let other users in.
//! So the mount is watched from outside instead: a process of its own, in a process group
//! of its own, waits on a pipe only the mounting process holds and unmounts once it is
//! closed. A session killed or crashed (a stop of its whole process group, `SIGKILL`, an
//! abort) leaves the kernel's connection aborted but the mount in place, and this removes
//! it, so the next start can mount at that path again.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};

/// Holds the mount at `path` for as long as the process that made it lives.
pub struct Guardian {
    /// The pipe's end: closing it, with this struct's drop or the process's end, tells the
    /// helper to unmount.
    _end: ChildStdin,
    path: PathBuf,
}

impl Guardian {
    /// A guardian for the mount at `at`, made once it is mounted: one made before a mount
    /// that then failed would unmount what another session serves at that path.
    ///
    /// # Errors
    ///
    /// The helper process could not be started.
    pub fn new(at: &Path) -> io::Result<Self> {
        let mut process = Command::new("sh")
            .args([
                "-c",
                // The read ends with the pipe, so with the process that holds its other end,
                // and the helper then unmounts what is still mounted there: lazily, so a
                // folder an app that outlived its session still holds open is detached rather
                // than left behind, and only while the mount is up, so a session that already
                // unmounted its own mount does not take down another one made since here.
                "read -r _; grep -qF \" $1 \" /proc/self/mountinfo \
                 && exec fusermount3 -u -q -z -- \"$1\"",
                "tilo-mount-guardian",
            ])
            .arg(at)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // A stop of the session's whole process group, its SIGKILL included, must not end
            // the guardian with it.
            .process_group(0)
            .spawn()?;
        let Some(end) = process.stdin.take() else {
            return Err(io::Error::other("the mount guardian has no pipe"));
        };
        // Nobody here waits for the guardian, which ends when the mount it cleans up does; the
        // thread reaps it then rather than leave it to the process to collect.
        std::thread::spawn(move || {
            let _ = process.wait();
        });
        Ok(Self {
            _end: end,
            path: at.to_path_buf(),
        })
    }

    /// The mount it watches.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

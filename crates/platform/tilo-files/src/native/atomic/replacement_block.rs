//! Deliberately failing atomic replacement, for exercising save-failure paths.

use std::io;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::thread::JoinHandle;
use std::time::Duration;

/// Make [`crate::native::atomic::write`]-style replacement of the existing file at `path`
/// fail until `release` fires or `timeout` passes, whichever comes first.
///
/// Apps use this to demonstrate and test how they report a failed save. The
/// returned thread restores the original file; join it to wait for that.
///
/// On Unix the file is moved aside and a directory takes its name: removing
/// write permission is not enough when running as root, but no replacement
/// can rename a file over a directory. On Windows the file is held open
/// without sharing.
///
/// # Errors
///
/// Returns the error that prevented blocking; the file is left in place.
pub fn block_replacement(
    path: &Path,
    release: Receiver<()>,
    timeout: Duration,
) -> io::Result<JoinHandle<()>> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)?;
        Ok(std::thread::spawn(move || {
            let _ = release.recv_timeout(timeout);
            drop(lock);
        }))
    }
    #[cfg(unix)]
    {
        let path = path.to_path_buf();
        let backup = path.with_extension("replacement-blocked");
        std::fs::rename(&path, &backup)?;
        if let Err(error) = std::fs::create_dir(&path) {
            let _ = std::fs::rename(&backup, &path);
            return Err(error);
        }
        Ok(std::thread::spawn(move || {
            let _ = release.recv_timeout(timeout);
            let _ = std::fs::remove_dir(&path);
            let _ = std::fs::rename(&backup, &path);
        }))
    }
}

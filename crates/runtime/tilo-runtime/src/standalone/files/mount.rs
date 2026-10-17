//! The workspace mounted at its folder's path, on a Linux desktop where
//! this user can mount FUSE: the same filesystem Tilo OS mounts at
//! `/var/workspace`, reachable by this user only.

use super::{INTERVAL, NamespaceFile};
use std::{
    ffi::OsStr,
    io,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    sync::{Arc, mpsc},
    thread::JoinHandle,
};

/// The helper an unprivileged process mounts FUSE through.
const FUSERMOUNT: &str = "fusermount3";

/// Whether this user can mount FUSE: the device `dev_fuse` exists and
/// [`FUSERMOUNT`] is an executable on `path`.
pub(super) fn available_in(dev_fuse: &Path, path: Option<&OsStr>) -> bool {
    dev_fuse.exists()
        && path.is_some_and(|path| {
            std::env::split_paths(path).any(|dir| {
                std::fs::metadata(dir.join(FUSERMOUNT))
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
        })
}

/// A mounted workspace; unmounted when dropped.
pub(super) struct Mount {
    session: Option<fuser::BackgroundSession>,
    stop: Option<mpsc::Sender<()>>,
    persist: Option<JoinHandle<()>>,
}

impl Mount {
    /// Mount `fs` at `at` and persist its namespace as it changes.
    pub(super) fn start(
        fs: tilo_fs::fs::QuickFS,
        at: &Path,
        namespace: Arc<NamespaceFile>,
    ) -> io::Result<Self> {
        clear_stale_mount(at)?;
        std::fs::create_dir_all(at)?;
        let mut config = fuser::Config::default();
        config.mount_options = vec![fuser::MountOption::FSName("tilo".into())];
        // Owner-only, the default, needs no system FUSE policy; the kernel
        // then refuses every other user.
        config.acl = fuser::SessionACL::Owner;
        let session = fuser::spawn_mount(fs, at, &config)?;
        let (stop, stopped) = mpsc::channel::<()>();
        let persist = std::thread::Builder::new()
            .name("tilo-workspace-persist".into())
            .spawn(move || {
                while stopped.recv_timeout(INTERVAL) == Err(mpsc::RecvTimeoutError::Timeout) {
                    save(&namespace);
                }
                save(&namespace);
            })?;
        Ok(Self {
            session: Some(session),
            stop: Some(stop),
            persist: Some(persist),
        })
    }
}

impl Drop for Mount {
    /// Unmounts first, so no write lands after the last save.
    fn drop(&mut self) {
        if let Some(session) = self.session.take()
            && let Err(error) = tilo_fs::fuse_session::unmount_and_join(session)
        {
            tracing::warn!(%error, "the workspace did not unmount cleanly");
        }
        drop(self.stop.take());
        if let Some(persist) = self.persist.take() {
            let _ = persist.join();
        }
    }
}

fn save(namespace: &NamespaceFile) {
    if let Err(error) = namespace.save_if_changed() {
        tracing::warn!(%error, "the workspace namespace was not saved");
    }
}

/// A mount a crashed runtime left behind answers `ENOTCONN`; unmount it
/// so the folder's path can be mounted again.
fn clear_stale_mount(at: &Path) -> io::Result<()> {
    match std::fs::metadata(at) {
        Err(error) if error.raw_os_error() == Some(libc::ENOTCONN) => {
            let status = std::process::Command::new(FUSERMOUNT)
                .args(["-u", "-z"])
                .arg(at)
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "{FUSERMOUNT} could not remove the stale mount at {}",
                    at.display()
                )))
            }
        }
        _ => Ok(()),
    }
}

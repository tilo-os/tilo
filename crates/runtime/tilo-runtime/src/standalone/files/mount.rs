//! The workspace mounted at its folder's path, on a Linux desktop where
//! this user can mount FUSE: the same filesystem Tilo OS mounts at
//! `/var/workspace`, reachable by this user only.

use super::{INTERVAL, NamespaceFile};
use std::{
    ffi::OsStr,
    io,
    path::Path,
    sync::{Arc, mpsc},
    thread::JoinHandle,
};
use tilo_fs::fuse_session::clear_stale_mount;

/// Whether this user can mount FUSE ([`tilo_fs::fuse_session::mountable`]).
pub(super) fn available_in(dev_fuse: &Path, path: Option<&OsStr>) -> bool {
    tilo_fs::fuse_session::mountable(dev_fuse, path)
}

/// A mounted workspace; unmounted when dropped.
pub(super) struct Mount {
    session: Option<fuser::BackgroundSession>,
    at: std::path::PathBuf,
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
            at: at.to_path_buf(),
            stop: Some(stop),
            persist: Some(persist),
        })
    }
}

impl Drop for Mount {
    /// Unmounts first, so no write lands after the last save.
    fn drop(&mut self) {
        if let Some(session) = self.session.take()
            && let Err(error) = tilo_fs::fuse_session::unmount_and_join(session, &self.at)
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

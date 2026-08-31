//! Mount / unmount lifecycle handle around a [`QosAppFs`].
//!
//! `fuser::spawn_mount2` gives a `BackgroundSession` that runs the
//! FUSE event loop on a spawned thread and unmounts on drop. [`Mount`]
//! wraps that with: a small explicit [`MountOptions`] surface, a
//! domain `MountError` that keeps `fuser`/`std::io::Error` internals
//! from leaking, an explicit `unmount()` for graceful teardown in
//! tests, and the same drop-unmount as a safety net.

use std::path::{Path, PathBuf};

use fuser::{BackgroundSession, Config, MountOption, SessionACL};
use thiserror::Error;

use crate::collection::Collection;
use crate::fs::{BuildError, QosAppFs};
use crate::store::BlobStore;

/// Options for building a [`Mount`]. Defaults are conservative:
/// read-only, no `allow_other`, auto-unmount on drop, no uid/gid remap.
#[derive(Clone, Debug, Default)]
pub struct MountOptions {
    /// Allow other users to access the mount; requires
    /// `user_allow_other` in /etc/fuse.conf. Off by default so the
    /// launcher opts in explicitly.
    pub allow_other: bool,
    /// The filesystem's advertised name in /proc/self/mounts and `mount` output; defaults to "qos-app".
    pub fs_name: Option<String>,
    /// The filesystem's subtype in `mount` output; defaults to "qos-app-fs".
    pub subtype: Option<String>,
    /// Override owner uid stamped on every getattr; `None` = use the mounting process's uid.
    pub uid: Option<u32>,
    /// Override owner gid stamped on every getattr; `None` = use the mounting process's gid.
    pub gid: Option<u32>,
    /// Page-cache capacity in 4 KiB pages; `None` uses the crate default (16 MiB).
    pub cache_pages: Option<usize>,
}

/// A live FUSE mount; drop = unmount + join. Internally a
/// `BackgroundSession` whose event-loop thread unmounts the FUSE
/// device on drop.
pub struct Mount {
    /// The FUSE mount point (kept for `mountpoint()`/diagnostics).
    mountpoint: PathBuf,
    /// The background FUSE session; `None` after [`Mount::unmount`] has been called explicitly.
    session: Option<BackgroundSession>,
}

impl Mount {
    /// Build a filesystem from `collection` + `store` and mount it at
    /// `mountpoint`; returns once the FUSE session is running on its worker thread.
    ///
    /// # Errors
    ///
    /// Returns [`MountError::Build`] if the inode table can't be built
    /// from `collection`, or [`MountError::Fuse`] if the kernel mount
    /// itself fails.
    pub fn spawn<S: BlobStore + Send + Sync + 'static>(
        collection: &Collection,
        store: S,
        mountpoint: &Path,
        options: &MountOptions,
    ) -> Result<Self, MountError> {
        let mountpoint = mountpoint.to_path_buf();
        let mut fs = if let Some(pages) = options.cache_pages {
            QosAppFs::with_cache_pages(collection, store, pages)?
        } else {
            QosAppFs::new(collection, store)?
        };
        if let (Some(uid), Some(gid)) = (options.uid, options.gid) {
            fs = fs.with_owner(uid, gid);
        }

        let mount_options: Vec<MountOption> = vec![
            MountOption::RO,
            MountOption::NoDev,
            MountOption::NoSuid,
            MountOption::FSName(
                options
                    .fs_name
                    .clone()
                    .unwrap_or_else(|| "qos-app".to_string()),
            ),
            MountOption::Subtype(
                options
                    .subtype
                    .clone()
                    .unwrap_or_else(|| "qos-app-fs".to_string()),
            ),
        ];

        // Config is #[non_exhaustive] — fill it via Default +
        // field assignment rather than a struct literal.
        let mut config = Config::default();
        config.mount_options = mount_options;
        if options.allow_other {
            config.acl = SessionACL::All;
        }

        let session = fuser::spawn_mount2(fs, &mountpoint, &config)
            .map_err(|e| MountError::Fuse(mountpoint.clone(), e))?;
        Ok(Self {
            mountpoint,
            session: Some(session),
        })
    }

    /// The mount point path.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// Explicitly unmount + join the FUSE worker. Idempotent —
    /// subsequent calls are no-ops. Drop also triggers this, but a test
    /// asserting on post-unmount state should call it directly.
    ///
    /// # Errors
    ///
    /// Returns [`MountError::Fuse`] if the kernel unmount fails.
    pub fn unmount(mut self) -> Result<(), MountError> {
        match self.session.take() {
            Some(session) => session
                .umount_and_join()
                .map_err(|e| MountError::Fuse(self.mountpoint.clone(), e)),
            None => Ok(()),
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        // Take + drop the session so BackgroundSession's own Drop unmounts.
        // We deliberately do NOT call umount_and_join here — that would
        // block, and Drop must not block on user I/O in the failure path.
        drop(self.session.take());
    }
}

/// Errors from [`Mount::spawn`] + [`Mount::unmount`].
#[derive(Debug, Error)]
pub enum MountError {
    /// Building the [`QosAppFs`] from the collection failed.
    #[error("build filesystem: {0}")]
    Build(#[from] BuildError),
    /// `fuser` reported a mount / session error.
    #[error("fuser at {0:?}: {1}")]
    Fuse(PathBuf, std::io::Error),
}

//! Telling inotify of the changes the kernel did not make.
//!
//! inotify reports what passes through this kernel's VFS. A peer's edit,
//! merged into the namespace over gossip, never does, so a watch on the
//! mount (the folder Files shows, its record catalogue) saw nothing of it
//! and its owner had to read again on a timer. FUSE's reverse notifications
//! drop what the kernel cached, but raise no inotify event for an entry
//! that was added or changed.
//!
//! So after each change to the namespace the daemon drops what the kernel
//! cached of the changed nodes, then sets
//! [`CHANGE_SIGNAL_XATTR`](tilo_files::native::CHANGE_SIGNAL_XATTR) through
//! the mount on each directory whose entries changed and that the kernel
//! has listed: the VFS reports that as `IN_ATTRIB` on the directory to every
//! watch on it. The value is a token only this daemon knows, and setting it
//! changes nothing; anyone else is refused. The root is signalled too
//! whenever the record catalogue's revision moves, which is what a watch on
//! the catalogue waits for.
//!
//! The mount answers the attribute with `1` once a signal has gone through,
//! and only while the worker raises them, so a watcher trusts its watch
//! (`tilo_files::native::watch_sees_every_change`) only when it may: where a
//! signal cannot be raised (a read-only mount, a refused `setxattr`), the
//! watcher keeps reading the folder again on its own.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_lock::MutexRecover as _;

/// What the mount answers for the attribute while the signal is live.
const LIVE: &[u8] = b"1";

/// One mount's change signal, shared by its FUSE callbacks and its worker.
pub struct ChangeSignal {
    /// What only this daemon sets the attribute to.
    token: Vec<u8>,
    live: AtomicBool,
    /// Directories the kernel has listed: only a directory someone listed
    /// can be watched by someone who cares what is in it.
    listed: Mutex<HashSet<u64>>,
}

impl ChangeSignal {
    /// A signal with a fresh token, not yet live.
    ///
    /// # Errors
    ///
    /// When the operating system has no randomness to give.
    pub fn new() -> std::io::Result<Self> {
        use getrandom::rand_core::TryRng as _;
        let mut token = [0_u8; 32];
        getrandom::SysRng
            .try_fill_bytes(&mut token)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self {
            token: hex::encode(token).into_bytes(),
            live: AtomicBool::new(false),
            listed: Mutex::new(HashSet::new()),
        })
    }

    /// The kernel listed directory `ino`.
    pub(crate) fn note_listed(&self, ino: u64) {
        self.listed.lock_recover().insert(ino);
    }

    /// What the attribute reads as: `1` while signals are raised.
    pub(crate) fn answer(&self) -> Option<&'static [u8]> {
        self.live.load(Ordering::Acquire).then_some(LIVE)
    }

    /// Whether `value` is this daemon's own signal.
    pub(crate) fn accepts(&self, value: &[u8]) -> bool {
        value == self.token.as_slice()
    }

    /// Whether the worker raises signals now.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

#[cfg(target_os = "linux")]
pub use worker::ChangeSignalTask;

#[cfg(target_os = "linux")]
mod worker {
    use std::collections::BTreeSet;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use fuser::{INodeNo, Notifier};
    use tilo_files::native::CHANGE_SIGNAL_XATTR;
    use tilo_lock::MutexRecover as _;

    use super::ChangeSignal;
    use crate::namespace::{Namespace, ROOT_INO};

    /// A burst of changes (one peer's sync, a folder copied in) is signalled
    /// once, this long after it began.
    const SETTLE: Duration = Duration::from_millis(150);

    /// `statfs` magic of FUSE: signals go to the mount, never to the
    /// directory under it once it is gone.
    const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

    /// The running worker; dropping it stops the signals.
    pub struct ChangeSignalTask {
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for ChangeSignalTask {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// What the worker remembers between passes.
    struct Seen {
        heads: Vec<automerge::ChangeHash>,
        catalogue: String,
    }

    impl ChangeSignal {
        /// Raise signals for `namespace`, mounted at `mount_point`, whose
        /// session `notifier` belongs to, on `runtime`. A first signal on the
        /// root must go through before the mount says it signals.
        ///
        /// # Errors
        ///
        /// When that first signal cannot be raised: the mount is read-only,
        /// is not this FUSE mount, or the attribute is refused.
        pub fn start(
            self: &Arc<Self>,
            namespace: Arc<Mutex<Namespace>>,
            mount_point: &Path,
            notifier: Notifier,
            runtime: &tokio::runtime::Handle,
        ) -> io::Result<ChangeSignalTask> {
            self.raise(mount_point, Path::new(""))?;
            let (mut changes, mut seen) = {
                let mut ns = namespace.lock_recover();
                ns.collect_projected_changes();
                let seen = Seen {
                    heads: ns.heads(),
                    catalogue: ns.indexed_record_revision().to_owned(),
                };
                (ns.subscribe_changes(), seen)
            };
            self.live.store(true, Ordering::Release);
            let signal = Arc::clone(self);
            let mount_point = mount_point.to_path_buf();
            let task = runtime.spawn(async move {
                while changes.changed().await.is_ok() {
                    tokio::time::sleep(SETTLE).await;
                    changes.borrow_and_update();
                    let pass = Pass {
                        signal: Arc::clone(&signal),
                        namespace: Arc::clone(&namespace),
                        mount_point: mount_point.clone(),
                        notifier: notifier.clone(),
                    };
                    match tokio::task::spawn_blocking(move || pass.run(&seen)).await {
                        Ok(Ok(next)) => seen = next,
                        Ok(Err(error)) => {
                            eprintln!("change signal stopped: {error}");
                            break;
                        }
                        Err(_) => break,
                    }
                }
                signal.live.store(false, Ordering::Release);
                namespace.lock_recover().stop_collecting_projected_changes();
            });
            Ok(ChangeSignalTask { task })
        }

        /// Set the attribute on `relative` below `mount_point`, through the
        /// mount, so watches on it hear `IN_ATTRIB`.
        fn raise(&self, mount_point: &Path, relative: &Path) -> io::Result<()> {
            let stat = rustix::fs::statfs(mount_point)?;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
            )]
            let magic = stat.f_type as u32;
            if magic != FUSE_SUPER_MAGIC {
                return Err(io::Error::other(format!(
                    "{} is no longer a FUSE mount",
                    mount_point.display()
                )));
            }
            rustix::fs::setxattr(
                mount_point.join(relative),
                CHANGE_SIGNAL_XATTR,
                &self.token,
                rustix::fs::XattrFlags::empty(),
            )
            .map_err(io::Error::from)
        }
    }

    /// One pass over the changes since the last.
    struct Pass {
        signal: Arc<ChangeSignal>,
        namespace: Arc<Mutex<Namespace>>,
        mount_point: PathBuf,
        notifier: Notifier,
    }

    impl Pass {
        fn run(self, seen: &Seen) -> io::Result<Seen> {
            let (nodes, directories, next) = {
                let mut ns = self.namespace.lock_recover();
                let changes = ns.kernel_changes_since(&seen.heads);
                let next = Seen {
                    heads: ns.heads(),
                    catalogue: ns.indexed_record_revision().to_owned(),
                };
                let mut directories: BTreeSet<u64> = {
                    let listed = self.signal.listed.lock_recover();
                    changes
                        .directories
                        .into_iter()
                        .filter(|ino| listed.contains(ino))
                        .collect()
                };
                if next.catalogue != seen.catalogue {
                    directories.insert(ROOT_INO);
                }
                let directories: Vec<(u64, PathBuf)> = directories
                    .into_iter()
                    .filter_map(|ino| Some((ino, ns.kernel_path_for(ino)?)))
                    .collect();
                drop(ns);
                (changes.nodes, directories, next)
            };
            // The kernel's cached attributes and pages go first, so whoever
            // the signal wakes reads what is there now.
            for ino in nodes {
                let _ = self.notifier.inval_inode(INodeNo(ino), 0, 0);
            }
            for (ino, path) in directories {
                match self.signal.raise(&self.mount_point, &path) {
                    Ok(()) => {}
                    // Gone, or hidden from the mount's view, since the
                    // namespace was read: nothing there to watch.
                    Err(error)
                        if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR)) =>
                    {
                        self.signal.listed.lock_recover().remove(&ino);
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(next)
        }
    }
}

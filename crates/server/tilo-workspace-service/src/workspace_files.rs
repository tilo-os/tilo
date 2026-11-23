//! The files a host keeps beside its device store, for itself and for each
//! workspace it serves. None holds a workspace's changes: those are the
//! device store's ([`tilo_peer_state::store`]).
//!
//! ```text
//! <state_root>/
//! ├── iroh-secret.bin              # the host's endpoint secret (0600)
//! ├── revocations.log              # root-signed revocations it refuses
//! └── workspaces/<workspace_id>/
//!     └── governed-access.log      # the devices and people it admits
//! ```

mod governed_access;
mod revocation_log;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_barrier;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tilo_server_protocol::WorkspaceId;

/// The workspace's membership: its access record's devices and handshake
/// roots, the one projection a host keeps.
pub use governed_access::GOVERNED_ACCESS_FILE;
pub use revocation_log::{
    REVOCATIONS_FILE, append_revocation, append_revocations, load_revocations, save_revocations,
};
/// The metrics snapshot file is read by clients as well as the daemon.
pub use tilo_server_protocol::metrics::{METRICS_FILE, load_metrics, save_metrics};

/// A host file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// The file does not hold what its format says.
    #[error("json: {0}")]
    Json(String),
}

/// Write `bytes` to `path` whole or not at all.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    tilo_files::native::atomic::write(path, bytes)
}

/// One workspace's files on this host.
#[derive(Clone)]
pub struct WorkspaceFiles {
    root: PathBuf,
    workspace_id: WorkspaceId,
    /// What the governed access log holds, shared by every handle on
    /// `root` this open made: a write appends the change without reading
    /// the log back ([`governed_access`]).
    governed_access: Arc<std::sync::Mutex<Option<governed_access::Held>>>,
}

/// The workspace's members as its host keeps them: the devices it admits, the
/// person roots its handshake admits, and whether governance owns that
/// admission. Both sets share a single atomic file so a crash cannot pair new
/// roots with old devices (or the reverse).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GovernedAccessRecord {
    pub(crate) member_devices: Vec<[u8; 32]>,
    pub(crate) member_user_roots: Vec<[u8; 32]>,
    /// Governance decides who joins: nothing is enrolled by a host's own rule.
    pub(crate) governed: bool,
}

impl WorkspaceFiles {
    /// Open (creating if necessary) the files of `workspace_id` under
    /// `state_root/workspaces/<id>/`.
    ///
    /// # Errors
    ///
    /// The workspace's directory can't be created.
    pub fn open(state_root: &Path, workspace_id: WorkspaceId) -> Result<Self, FilesError> {
        let root = state_root.join("workspaces").join(workspace_id.to_string());
        #[cfg(test)]
        test_barrier::pause(&root, "open");
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            workspace_id,
            governed_access: Arc::default(),
        })
    }

    /// Root directory of this workspace's files.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// The device public keys the workspace's access record admits.
    ///
    /// # Errors
    ///
    /// Returns an error if the record exists but can't be read.
    pub fn load_membership(&self) -> Result<Vec<[u8; 32]>, FilesError> {
        #[cfg(test)]
        test_barrier::pause(&self.root, "membership-read");
        Ok(self
            .load_governed_access()?
            .map_or_else(Vec::new, |access| access.member_devices))
    }

    /// Replace the device half of the workspace's access record, keeping the
    /// handshake roots it admits and whether governance owns it as they are.
    /// A workspace whose host keeps no record yet gets one that governance does
    /// not own: its devices are the ones its host enrolled itself.
    ///
    /// # Errors
    ///
    /// Returns an error if the record can't be read or written.
    pub fn save_membership(&self, devices: &[[u8; 32]]) -> Result<(), FilesError> {
        #[cfg(test)]
        test_barrier::pause(&self.root, "membership-write");
        let mut access = self.load_governed_access()?.unwrap_or_default();
        access.member_devices = devices.to_vec();
        self.save_governed_access(&access).map(drop)
    }
}
/// `<state_root>/iroh-secret.bin` with owner-only access so the relay's
/// iroh node id is stable across restarts. An existing secret is never
/// replaced: only a secret that is absent is minted, and it is published
/// with an exclusive create, so relays starting at the same moment all
/// return the one secret that reached disk.
///
/// # Errors
///
/// Returns an error if an existing secret file cannot be read or is not
/// exactly 32 bytes, or if creating the state root or publishing a freshly
/// minted secret fails.
pub fn load_or_create_iroh_secret(state_root: &Path) -> Result<[u8; 32], FilesError> {
    use rand::Rng;

    let path = state_root.join("iroh-secret.bin");
    match read_iroh_secret(&path) {
        Err(FilesError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
        existing => return existing,
    }
    fs::create_dir_all(state_root)?;
    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    // `false` means another starter published first; its secret is the one
    // every later start will read, so return that rather than ours.
    tilo_files::native::private::create_atomic(&path, &secret)?;
    read_iroh_secret(&path)
}

fn read_iroh_secret(path: &Path) -> Result<[u8; 32], FilesError> {
    let bytes = fs::read(path)?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        FilesError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("iroh secret is {} bytes, want 32", bytes.len()),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iroh_secret_is_minted_once_and_never_replaced() {
        {
            let tmp = tempfile::tempdir().unwrap();
            let a = load_or_create_iroh_secret(tmp.path()).unwrap();
            let b = load_or_create_iroh_secret(tmp.path()).unwrap();
            assert_eq!(a, b);
            assert_ne!(a, [0u8; 32]);
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("iroh-secret.bin");
            std::fs::write(&path, [7u8; 32]).unwrap();
            assert_eq!(load_or_create_iroh_secret(tmp.path()).unwrap(), [7u8; 32]);
            // A malformed secret is an error, not a reason to mint a new identity.
            std::fs::write(&path, b"short").unwrap();
            assert!(load_or_create_iroh_secret(tmp.path()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"short");
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("iroh-secret.bin");
            // A directory in the secret's place fails every read, whatever the
            // caller's privileges (a mode-000 file is still readable by root).
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("marker"), b"kept").unwrap();
            assert!(load_or_create_iroh_secret(tmp.path()).is_err());
            assert!(path.is_dir(), "the unreadable secret must not be replaced");
            assert_eq!(std::fs::read(path.join("marker")).unwrap(), b"kept");
            assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
        }
        {
            for _ in 0..8 {
                let tmp = tempfile::tempdir().unwrap();
                let root = tmp.path().to_path_buf();
                let check = std::sync::Arc::new(std::sync::Barrier::new(4));
                // Spawn every key holder before joining: interleaving spawn/join
                // would deadlock the barrier.
                let mut workers = Vec::with_capacity(4);
                for _ in 0..4 {
                    let root = root.clone();
                    let check = std::sync::Arc::clone(&check);
                    workers.push(std::thread::spawn(move || {
                        check.wait();
                        load_or_create_iroh_secret(&root).unwrap()
                    }));
                }
                let secrets: Vec<_> = workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .collect();
                let persisted = std::fs::read(root.join("iroh-secret.bin")).unwrap();
                assert!(secrets.iter().all(|secret| secret.as_slice() == persisted));
                assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn minted_iroh_secret_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        load_or_create_iroh_secret(tmp.path()).unwrap();
        let mode = std::fs::metadata(tmp.path().join("iroh-secret.bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn governed_access_is_one_authoritative_device_and_root_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceFiles::open(tmp.path(), WorkspaceId::from_u128(7)).unwrap();
        store
            .save_governed_access(&GovernedAccessRecord {
                member_devices: vec![[1; 32]],
                member_user_roots: vec![[2; 32]],
                governed: true,
            })
            .unwrap();

        assert_eq!(store.load_membership().unwrap(), vec![[1; 32]]);
        assert_eq!(
            store.load_governed_access().unwrap().unwrap(),
            GovernedAccessRecord {
                member_devices: vec![[1; 32]],
                member_user_roots: vec![[2; 32]],
                governed: true,
            }
        );
        store.save_membership(&[[3; 32]]).unwrap();
        assert_eq!(
            store.load_governed_access().unwrap().unwrap(),
            GovernedAccessRecord {
                member_devices: vec![[3; 32]],
                member_user_roots: vec![[2; 32]],
                governed: true,
            },
            "device-only membership writers must preserve the governed root half"
        );
        assert!(store.root().join(GOVERNED_ACCESS_FILE).exists());
    }
}

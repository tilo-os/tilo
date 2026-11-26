//! The files a host keeps beside its device store, for itself and for each
//! workspace it serves. None holds a workspace's changes: those are the
//! device store's ([`tilo_peer_state::store`]).
//!
//! ```text
//! <device>/
//! ├── iroh-secret.bin              # the host's endpoint secret (0600)
//! ├── revocations.log              # root-signed revocations it refuses
//! └── service/                    # the device's workspace service
//!     └── workspaces/<workspace_id>/  # what the host keeps for the workspace
//! ```
//!
//! Who a workspace admits is no file here: it is the workspace's signed member
//! list, which the device store keeps.

mod revocation_log;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tilo_server_protocol::WorkspaceId;

pub use revocation_log::{
    REVOCATIONS_FILE, append_revocation, append_revocations, load_revocations, save_revocations,
};

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
        fs::create_dir_all(&root)?;
        Ok(Self { root, workspace_id })
    }

    /// Root directory of this workspace's files.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
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
}

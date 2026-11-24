//! The sealed records this device holds, and the workspace each belongs to, kept on disk so a
//! later session still knows them.
//!
//! Layout: `<root>/<artifact uuid>.workspace` and `<uuid>.record`, which name the workspace the
//! record is in and the record it is.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use collab_types::{ArtifactId, SignedOp};

/// History written to its files but not yet synced to disk. The broker
/// writes history under the lock its channels share, which the compositor's
/// main loop takes too, so the data sync, which can wait on a busy disk for
/// a long time, is left to the caller once it has released that lock. A
/// caller syncs before it hands what it recorded on (fans an op out to apps
/// or peers), so it still delivers an op only once
/// its history is on disk.
#[must_use = "recorded history is durable only once synced"]
#[derive(Debug, Default)]
pub struct Unsynced(Vec<File>);

impl Unsynced {
    /// Leave `file`'s data sync to [`Self::sync`]. A file that cannot be
    /// handed on (no descriptor left to duplicate it) is synced now.
    fn defer(&mut self, file: &File) -> std::io::Result<()> {
        match file.try_clone() {
            Ok(handle) => {
                self.0.push(handle);
                Ok(())
            }
            // No descriptor left to duplicate: the file is synced now.
            Err(_) => tilo_failure::ignored!(
                "the sync below stands in for the deferred one",
                file.sync_data()
            ),
        }
    }

    /// Sync every file written, so what was recorded survives a crash.
    pub fn sync(self) -> std::io::Result<()> {
        let mut first = Ok(());
        for file in self.0 {
            if let Err(error) = file.sync_data()
                && first.is_ok()
            {
                first = Err(error);
            }
        }
        first
    }

    /// [`Self::sync`], logging a failure: the op it recorded is still
    /// delivered live, as one whose append failed is.
    pub fn finish(self) {
        if let Err(error) = self.sync() {
            tracing::warn!(%error, "collab history: sync failed");
        }
    }
}

/// The records this device holds sealed history of, and the workspace each belongs to.
pub struct CollabHistoryStore {
    root: PathBuf,
    /// The `.record` files of the store, read once on first use and kept
    /// with every binding after.
    records: Option<HashMap<ArtifactId, HeldDocument>>,
}

/// The record an artifact's history is, as its `.record` file says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeldDocument {
    workspace: String,
    app_id: String,
    document_id: String,
}

impl HeldDocument {
    fn encode(&self) -> String {
        format!("{}\n{}\n{}", self.workspace, self.app_id, self.document_id)
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.split('\n');
        let (workspace, app_id, document_id) = (lines.next()?, lines.next()?, lines.next()?);
        let held = Self {
            workspace: workspace.to_owned(),
            app_id: app_id.to_owned(),
            document_id: document_id.to_owned(),
        };
        (lines.next().is_none() && !workspace.is_empty() && !app_id.is_empty()).then_some(held)
    }
}

impl CollabHistoryStore {
    /// Open (creating, mode 0700) the store at `root`.
    pub fn open(root: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            records: None,
        })
    }

    /// The session store under the launcher-provided state root.
    #[cfg(not(test))]
    pub fn open_for_session() -> std::io::Result<Self> {
        let state = tilo_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "launcher did not provide XDG_STATE_HOME for collaboration history",
            )
        })?;
        Self::open(state.join("tilo/collaboration/history"))
    }

    /// Whether `op` belongs in durable history: content-key ciphertext only.
    /// Cleartext routes keep their existing in-memory window, and a
    /// single-use MLS application message could never be read back.
    pub fn retains(op: &SignedOp) -> bool {
        op.encryption
            .is_some_and(|metadata| metadata.retention().is_some())
    }

    /// Remember that `artifact`'s history belongs to the workspace `cluster_id`,
    /// beside its log, so a later session still knows which workspace's members
    /// may page through it. The binding's sync is left to the caller.
    pub fn bind_workspace(
        &mut self,
        artifact: ArtifactId,
        cluster_id: &str,
    ) -> std::io::Result<Unsynced> {
        let path = self.workspace_path(artifact);
        if fs::read_to_string(&path).is_ok_and(|bound| bound == cluster_id) {
            return Ok(Unsynced::default());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(cluster_id.as_bytes())?;
        let mut unsynced = Unsynced::default();
        unsynced.defer(&file)?;
        Ok(unsynced)
    }

    /// The workspace `artifact`'s history was bound to, if any session bound it.
    pub fn workspace_of(&self, artifact: ArtifactId) -> Option<String> {
        fs::read_to_string(self.workspace_path(artifact))
            .ok()
            .filter(|workspace| !workspace.is_empty())
    }

    fn workspace_path(&self, artifact: ArtifactId) -> PathBuf {
        self.root.join(format!("{}.workspace", artifact.0))
    }

    fn resource_path(&self, artifact: ArtifactId) -> PathBuf {
        self.root.join(format!("{}.record", artifact.0))
    }

    /// The records this store holds history of, read from disk on first
    /// use. A file that does not parse (torn by a crash) names no record,
    /// and the log says which.
    fn held_documents(&mut self) -> &mut HashMap<ArtifactId, HeldDocument> {
        let root = &self.root;
        self.records.get_or_insert_with(|| {
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(root = %root.display(), %error, "collab history: held records cannot be listed");
                    return HashMap::new();
                }
            };
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let path = entry.path();
                    if path
                        .extension()
                        .is_none_or(|extension| extension != "record")
                    {
                        return None;
                    }
                    let unreadable = |reason: &dyn std::fmt::Display| {
                        tracing::warn!(path = %path.display(), %reason, "collab history: held record names no record");
                    };
                    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
                    let artifact = match uuid::Uuid::parse_str(stem) {
                        Ok(artifact) => artifact,
                        Err(error) => {
                            unreadable(&format_args!("file name is not an artifact id: {error}"));
                            return None;
                        }
                    };
                    let text = match fs::read_to_string(&path) {
                        Ok(text) => text,
                        Err(error) => {
                            unreadable(&error);
                            return None;
                        }
                    };
                    let Some(held) = HeldDocument::decode(&text) else {
                        unreadable(&"contents are not workspace, app and record lines");
                        return None;
                    };
                    Some((ArtifactId(artifact), held))
                })
                .collect()
        })
    }

    /// Remember that `artifact`'s history is the record `document_id` of the
    /// app `app_id` in the workspace `cluster_id` ([`Self::bind_workspace`] included),
    /// so a later session still knows the records it holds there
    /// ([`Self::records_in`]). The binding's sync is left to the caller.
    pub fn bind_document(
        &mut self,
        artifact: ArtifactId,
        cluster_id: &str,
        app_id: &str,
        document_id: &str,
    ) -> std::io::Result<Unsynced> {
        let held = HeldDocument {
            workspace: cluster_id.to_owned(),
            app_id: app_id.to_owned(),
            document_id: document_id.to_owned(),
        };
        if self.held_documents().get(&artifact) == Some(&held) {
            return Ok(Unsynced::default());
        }
        let workspace = self.bind_workspace(artifact, cluster_id)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.resource_path(artifact))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(held.encode().as_bytes())?;
        let mut unsynced = workspace;
        unsynced.defer(&file)?;
        self.held_documents().insert(artifact, held);
        Ok(unsynced)
    }

    /// The records of the app `app_id` in the workspace `cluster_id` this store
    /// holds history of, this session or an earlier one.
    pub fn records_in(&mut self, app_id: &str, cluster_id: &str) -> Vec<String> {
        self.held_documents()
            .values()
            .filter(|held| held.app_id == app_id && held.workspace == cluster_id)
            .map(|held| held.document_id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        AttestationId, ContentRetention, Ed25519PublicKey, EncryptionMetadata, OpKind,
    };

    fn op(artifact: ArtifactId, seq: u64, encryption: Option<EncryptionMetadata>) -> SignedOp {
        let mut op = SignedOp::new_unsigned(
            seq.to_le_bytes().to_vec(),
            artifact,
            Ed25519PublicKey::from_bytes([4; 32]),
            AttestationId::new(),
            OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            seq,
        );
        op.encryption = encryption;
        op
    }

    /// A record bound in one session is known to the next, and only content-key ciphertext
    /// belongs in history.
    #[test]
    fn a_bound_record_is_known_to_a_later_session() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("history");
        let artifact = ArtifactId::new();
        {
            let mut store = CollabHistoryStore::open(root.clone()).unwrap();
            store
                .bind_document(artifact, "kitchen", "so.tilo.notes", "groceries")
                .unwrap()
                .sync()
                .unwrap();
        }
        let mut store = CollabHistoryStore::open(root).unwrap();
        assert_eq!(store.records_in("so.tilo.notes", "kitchen"), ["groceries"]);
        assert!(store.records_in("so.tilo.notes", "garden").is_empty());
        assert_eq!(store.workspace_of(artifact).as_deref(), Some("kitchen"));

        let sealed = EncryptionMetadata::content_key(1, ContentRetention::Delta);
        assert!(CollabHistoryStore::retains(&op(artifact, 1, Some(sealed))));
        assert!(!CollabHistoryStore::retains(&op(artifact, 2, None)));
        assert!(!CollabHistoryStore::retains(&op(
            artifact,
            3,
            Some(EncryptionMetadata::mls_application(1)),
        )));
    }
}

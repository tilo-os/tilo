//! What this device keeps of its sessions for the next one: the workspace in front, the person's
//! workspace list, what could not be written to a workspace yet, the unlock history, the fire times
//! of scheduled shortcuts, the wallpapers, the notification centre, and what finishing an update
//! asked and told. Each is one document of the device's document store ([`app_state::FsStore`])
//! below the session's state directory, under a reserved key no app holds.
//!
//! The store commits on its own thread, so a session that keeps something never waits for the
//! disk; [`flush`] is what a session that ends waits for. A session holds each of its documents as
//! its only writer: another session on the same state directory reads them, and becomes their
//! writer once that session lets them go.

use std::path::Path;
use std::sync::Arc;

use durable_store::{DurableStore as _, Lease, Revision, StorageKey, WriteOp};

/// The workspace in front and the displays' partition ([`crate::device_state`]).
pub(crate) const SHOWN: &str = "so.tilo.session-shown";
/// The person's own workspaces as this device lists them ([`crate::workspaces`]).
pub(crate) const WORKSPACES: &str = "so.tilo.session-workspaces";
/// What this device could not write yet of its workspaces' layouts ([`crate::layout`]).
pub(crate) const PENDING_LAYOUT: &str = "so.tilo.session-pending-layout";
/// What the person's devices answered to an unlock ([`crate::unlock_history`]).
pub(crate) const UNLOCK: &str = "so.tilo.session-unlock";
/// The picture each workspace on this device shows ([`crate::wallpapers`]).
pub(crate) const WALLPAPERS: &str = "so.tilo.session-wallpapers";
/// What the notification centre held when the session ended ([`crate::notifications`]).
pub(crate) const NOTIFICATIONS: &str = "so.tilo.session-notifications";
/// When a restart for an update was last asked for, or done ([`crate::update_finish`]).
pub(crate) const UPDATE_ASKED: &str = "so.tilo.session-update-asked";
/// How updates ended that the person was told about: the deployment fallbacks and the live updates
/// that did not finish ([`crate::update_finish`]).
pub(crate) const UPDATES_TOLD: &str = "so.tilo.session-fallbacks-told";

/// The store of a session whose state directory is `state`: one per directory in this process.
pub(crate) fn store(state: &Path) -> Arc<app_state::FsStore> {
    let root = state.join("tilo").join("documents");
    // What a session keeps is the person's alone: the store's directory is theirs, and so is
    // everything the store writes beneath it.
    if let Err(error) = tilo_files::native::atomic::create_private_directory(&root) {
        tracing::warn!(%error, root = %root.display(), "the device's session state has no directory");
    }
    app_state::shared_store(root)
}

/// Wait until everything this session handed the store of `state` is on disk.
pub(crate) fn flush(state: Option<&Path>) {
    let Some(state) = state else {
        return;
    };
    if let Err(error) = store(state).flush(None) {
        tracing::warn!(%error, "what this session keeps was not all written");
    }
}

/// One document a session keeps, under its reserved key.
pub(crate) struct Kept {
    store: Arc<app_state::FsStore>,
    key: StorageKey,
    /// Held while this session is the document's writer: `None` while another session holds it,
    /// asked for again at each write until that session lets it go.
    lease: std::sync::Mutex<Option<Lease>>,
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kept")
            .field("key", &self.key)
            .field(
                "writer",
                &self.lease.lock().is_ok_and(|lease| lease.is_some()),
            )
            .finish_non_exhaustive()
    }
}

impl Kept {
    /// The document `name` of the session whose state directory is `state`, held as its writer
    /// unless another session holds it.
    pub(crate) fn open(state: &Path, name: &str) -> Self {
        let store = store(state);
        let key = StorageKey::app(name);
        // A session before this one in this process may have let the document go with its last
        // write still being committed, which holds the document until it lands: wait for it.
        let leased = store.lease(&key).or_else(|error| match error {
            durable_store::StoreError::Busy(_) => {
                if let Err(error) = store.flush(Some(&key)) {
                    tracing::warn!(%error, %key, "what the session before wrote was not all written");
                }
                store.lease(&key)
            }
            error => Err(error),
        });
        let lease = match leased {
            Ok(lease) => Some(lease),
            Err(error) => {
                tracing::warn!(%error, %key, "another session keeps this; this one reads it until it lets it go");
                None
            }
        };
        Self {
            store,
            key,
            lease: std::sync::Mutex::new(lease),
        }
    }

    /// What the document holds, `None` when nothing was kept yet.
    ///
    /// # Errors
    /// The store could not read it.
    pub(crate) fn read(&self) -> Result<Option<Vec<u8>>, durable_store::StoreError> {
        Ok(self.store.read(&self.key)?.map(|stored| stored.bytes))
    }

    /// Keep `bytes` as the document, replacing what it held; the write that names it, `None` when
    /// this session is not its writer or the store refused it.
    pub(crate) fn replace(&self, bytes: Vec<u8>) -> Option<Revision> {
        self.write(WriteOp::Replace(bytes))
    }

    /// Add `bytes` to the end of what the document holds.
    pub(crate) fn append(&self, bytes: Vec<u8>) -> Option<Revision> {
        self.write(WriteOp::Append(bytes))
    }

    fn write(&self, op: WriteOp) -> Option<Revision> {
        if !self.writer() {
            return None;
        }
        match self.store.write(&self.key, op, None) {
            Ok(revision) => Some(revision),
            Err(error) => {
                tracing::warn!(%error, key = %self.key, "what this session keeps was not written");
                None
            }
        }
    }

    /// Whether this session is the document's writer, becoming it when the session that held the
    /// document has let it go.
    fn writer(&self) -> bool {
        let Ok(mut lease) = self.lease.lock() else {
            return false;
        };
        if lease.is_none() {
            *lease = match self.store.lease(&self.key) {
                Ok(taken) => Some(taken),
                // The other session still holds it.
                Err(durable_store::StoreError::Busy(_)) => None,
                Err(error) => {
                    tracing::warn!(%error, key = %self.key, "this session cannot become the writer of what it keeps");
                    None
                }
            };
        }
        lease.is_some()
    }
}

/// The file the store of `state` keeps the document `name` in, for a test that looks at it.
#[cfg(test)]
pub(crate) fn local_path(state: &Path, name: &str) -> std::path::PathBuf {
    store(state)
        .local_path(&StorageKey::app(name))
        .expect("a document of the device's store is a file")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_that_opens_while_another_keeps_a_document_writes_it_once_that_one_ends() {
        let state = tempfile::tempdir().expect("a state directory");
        let first = Kept::open(state.path(), SHOWN);
        assert!(
            first.replace(b"first".to_vec()).is_some(),
            "the first session writes"
        );

        let second = Kept::open(state.path(), SHOWN);
        assert_eq!(
            second.read().expect("it reads").as_deref(),
            Some(&b"first"[..]),
            "the second reads what the first kept"
        );
        assert!(
            second.replace(b"second".to_vec()).is_none(),
            "and writes nothing while the first holds it"
        );

        flush(Some(state.path()));
        drop(first);
        flush(Some(state.path()));
        assert!(
            second.replace(b"second".to_vec()).is_some(),
            "once the first lets it go, the second is its writer"
        );
        flush(Some(state.path()));
        drop(second);
        let next = Kept::open(state.path(), SHOWN);
        assert_eq!(
            next.read().expect("it reads").as_deref(),
            Some(&b"second"[..])
        );
    }
}

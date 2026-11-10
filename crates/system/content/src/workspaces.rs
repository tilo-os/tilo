//! A host's content, one store per workspace it serves.
//!
//! A host that serves workspaces keeps the bytes their records reference
//! and do not carry: a file a caller uploaded, what an app made in a record
//! the host runs, the files the workspace's members wrote, what a bot fetched
//! from outside. [`WorkspaceStores`] is the one place a host keeps them: a
//! directory per workspace, each a [`ContentStore`] bounded by the same
//! [`ContentLimits`], so one workspace's store never answers for another's.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use crate::{ContentLimits, ContentStore, DirBackend};

/// Each workspace's content store under one directory, by the workspace's
/// id.
pub struct WorkspaceStores {
    dir: PathBuf,
    limits: ContentLimits,
    stores: Mutex<BTreeMap<String, ContentStore>>,
}

impl WorkspaceStores {
    /// The stores under `dir`, each bounded by `limits`.
    #[must_use]
    pub fn in_dir(dir: impl Into<PathBuf>, limits: ContentLimits) -> Self {
        Self {
            dir: dir.into(),
            limits,
            stores: Mutex::default(),
        }
    }

    /// The content store of `workspace`: one store per workspace, however
    /// often it is asked for, so its limits hold across everyone who keeps
    /// content there.
    ///
    /// # Errors
    ///
    /// `workspace` is not a workspace's id (a UUID).
    pub fn get(&self, workspace: &str) -> Result<ContentStore, String> {
        let workspace = workspace_dir(workspace).ok_or("the workspace is not a uuid")?;
        let store = self
            .stores
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace)
            .or_insert_with_key(|workspace| {
                ContentStore::new(DirBackend::with_limits(
                    self.dir.join(workspace),
                    self.limits,
                ))
            })
            .clone();
        Ok(store)
    }

    /// The limits every workspace's store is bounded by.
    #[must_use]
    pub const fn limits(&self) -> ContentLimits {
        self.limits
    }
}

/// `workspace` as the name of its directory: a UUID, lower-case and
/// hyphenated, whichever way it was written. Anything else names no
/// directory.
fn workspace_dir(workspace: &str) -> Option<String> {
    let digits: String = workspace.chars().filter(|c| *c != '-').collect();
    if digits.len() != 32 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digits = digits.to_ascii_lowercase();
    let part = |from: usize, to: usize| digits.get(from..to).unwrap_or_default();
    Some(format!(
        "{}-{}-{}-{}-{}",
        part(0, 8),
        part(8, 12),
        part(12, 16),
        part(16, 20),
        part(20, 32)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

    #[test]
    fn each_workspace_keeps_its_own_content() {
        let dir = std::env::temp_dir().join(format!("tilo-content-ws-{}", std::process::id()));
        let stores = WorkspaceStores::in_dir(&dir, ContentLimits::default());
        let hash = stores.get(WORKSPACE).unwrap().put(b"the report").unwrap();
        assert_eq!(
            stores
                .get(&WORKSPACE.to_uppercase().replace('-', ""))
                .unwrap()
                .read(&hash),
            Some(b"the report".to_vec()),
            "a workspace is one store however its id is written"
        );
        let other = stores.get("00000000-0000-4000-8000-000000000000").unwrap();
        assert_eq!(
            other.read(&hash),
            None,
            "one workspace's store answers for no other"
        );
        assert!(stores.get("../elsewhere").is_err());
        stores.get(WORKSPACE).unwrap().release(&hash).unwrap();
        assert_eq!(
            stores.get(WORKSPACE).unwrap().read(&hash),
            None,
            "a released item is gone"
        );
        assert!(stores.get("").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_workspace_is_bounded_by_its_limits() {
        let dir =
            std::env::temp_dir().join(format!("tilo-content-ws-limit-{}", std::process::id()));
        let limits = ContentLimits {
            max_total_bytes: 16,
            ..ContentLimits::default()
        };
        let stores = WorkspaceStores::in_dir(&dir, limits);
        let store = stores.get(WORKSPACE).unwrap();
        store.put(b"twelve bytes").unwrap();
        assert!(
            store.put(b"and twelve more").is_err(),
            "past its limit a workspace keeps no more"
        );
        assert!(
            stores
                .get("00000000-0000-4000-8000-000000000000")
                .unwrap()
                .put(b"twelve bytes")
                .is_ok(),
            "another workspace has its own room"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! Deleting a governed workspace, as its owner asks
//! ([`tilo_server_session::workspaces::DELETE_WORKSPACE`]): final, and with
//! everything this host keeps of it.
//!
//! Releasing a workspace ([`Workspaces::release`]) stops serving it and keeps
//! its files, because it lives on elsewhere. Deleting it ends it: everyone in
//! it is told it was deleted ([`Refusal::WorkspaceDeleted`]) while their session is
//! still open, and let in no more; its record, the state and history it
//! retained, and whatever else this host lists for it go from the disk and
//! from memory, so the storage it took no longer counts against its owner;
//! and whoever keeps more of it here (the content its members posted, the
//! app workers acting in it) hears it went ([`Workspaces::deletions`]).
//!
//! Its Linux machine goes first, while the workspace is still there for the
//! machine's host to hold ([`Workspaces::machines`]): a machine that could not
//! be deleted fails the deletion, which is asked again, and the workspace
//! stays as it was.
//!
//! A marker beside where its record was is written first and stays
//! (`<workspace>.deleted`): a host stopped halfway finishes deleting it when it
//! opens again, and the workspace is never made again under its identity.
//!
//! A branch that did not take everything was never anyone's: nobody was
//! answered its identity, so it is discarded rather than deleted
//! ([`Workspaces::discard_unfinished`]). Its marker (`<workspace>.branching`)
//! is written before it is made and goes only once it is its person's: a host
//! that stops before then opens none of it again, and the same branch can be
//! asked for again under the same identity.

use std::path::Path;
use std::sync::Arc;

use tilo_server_protocol::refusal::Refusal;

use super::Workspaces;
use tilo_lock::MutexRecover as _;
#[cfg(feature = "agents")]
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::WorkspaceId;

/// The extension of the marker of a workspace this host deleted.
pub(super) const DELETED: &str = "deleted";
/// The extension of the marker of a branch being made here, until it is its
/// person's.
const BRANCHING: &str = "branching";

impl Workspaces {
    /// Delete `workspace`, with everything this host keeps of it: final.
    /// Whose it is to delete the caller decides (the host's operation checks
    /// its owner).
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, its Linux machine could not be
    /// deleted, or the deletion could not be recorded. Once recorded, what is
    /// left of it is removed when the host opens again.
    pub async fn delete(&self, workspace: &str) -> Result<(), String> {
        let governed = self
            .inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or("this host does not hold that workspace")?;
        // Before the workspace is held: its machine's host enters it to be
        // asked, which takes the workspace's lock.
        self.delete_machine(workspace).await?;
        let governed = governed.lock().await;
        let workspace_id = governed.workspace_id();
        // Recorded first: a host stopped halfway finishes when it opens.
        // Written off the runtime's threads: a sync of a new file is disk
        // latency, which every other request on this runtime would wait for.
        let marker = governed.path.with_extension(DELETED);
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let file = std::fs::File::create(&marker).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            if let Some(dir) = marker.parent() {
                std::fs::File::open(dir)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())??;
        self.inner.workspaces.lock().await.remove(workspace);
        self.inner.readings.forget(workspace);
        // A workspace gone from here admits nobody at the handshake either.
        self.inner.memberships.forget(workspace);
        self.inner.host.access.people_moved();
        self.inner.direct.delete(workspace_id);
        // Everyone in it hears why while their session is open, then nobody
        // is let in. The workspace is already recorded as deleted, so a
        // connection auditing its subscriptions drops this one instead of
        // ending a session that serves other workspaces too — a backend
        // reaches all its customers' workspaces on one session.
        let bound = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned();
        if let Some(bound) = bound {
            bound
                .lock()
                .await
                .refuse_every_live_member(&Refusal::WorkspaceDeleted);
        }
        self.stop_serving(workspace_id).await;
        drop(governed);
        self.forget_deleted(workspace, workspace_id).await;
        tracing::info!(%workspace, workspace_id = %workspace_id, "governed workspace deleted");
        let _ = self
            .inner
            .deletions
            .send((workspace.to_owned(), workspace_id));
        let _ = self.inner.changes.send(workspace.to_owned());
        // Gone from every list that named it.
        self.personal_changed();
        Ok(())
    }

    /// Let go of `workspace` (`workspace_id`) here with everything this host
    /// keeps of it, and mark nothing: unlike a deleted workspace it is not
    /// refused afterwards, and can be held again under the same identity.
    ///
    /// # Errors
    ///
    /// Who may reach it could not be recorded.
    pub(super) async fn discard(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
    ) -> Result<(), String> {
        let governed = self.inner.workspaces.lock().await.get(workspace).cloned();
        if let Some(governed) = governed {
            let governed = governed.lock().await;
            self.inner.workspaces.lock().await.remove(workspace);
            self.inner.readings.forget(workspace);
            self.inner.memberships.forget(workspace);
            // Served no more, and not refused as deleted: it can come again.
            self.inner.direct.release(workspace_id);
            self.stop_serving(workspace_id).await;
            drop(governed);
        }
        self.forget_deleted(workspace, workspace_id).await;
        Ok(())
    }

    /// Whether the workspace whose server workspace is `workspace_id` was
    /// deleted here: whoever asks for it is told so
    /// ([`Refusal::WorkspaceDeleted`]), the same on every queue.
    #[must_use]
    pub fn deleted(&self, workspace_id: WorkspaceId) -> bool {
        self.inner.direct.deleted(workspace_id)
    }

    /// The workspaces this host deleted, as each goes: its id and the
    /// server workspace that carried it. Whoever keeps more of a workspace
    /// than its record (the content its members posted, app workers acting
    /// in it) lets go of it on hearing.
    #[must_use]
    pub fn deletions(&self) -> tokio::sync::broadcast::Receiver<(String, WorkspaceId)> {
        self.inner.deletions.subscribe()
    }

    /// Forget everything this host keeps of `workspace` (`workspace_id`),
    /// deleted: who owns it and what it took, who it listed, and its files. Its
    /// entry went with its governance.
    async fn forget_deleted(&self, workspace: &str, workspace_id: WorkspaceId) {
        if let Some(own) = self.own()
            && let Err(error) = own.forget(workspace)
        {
            tracing::warn!(%workspace, %error, "a deleted workspace stays in the record of whose workspaces are whose");
        }
        self.inner.memberships.forget(workspace);
        self.inner.channel_notices.lock_recover().remove(workspace);
        self.inner
            .queued_admissions
            .lock_recover()
            .remove(workspace);
        self.inner.queued_writes.lock_recover().remove(workspace);
        self.inner
            .unbuildable_windows
            .lock_recover()
            .remove(workspace);
        // Its retained history and its files go off the runtime's threads:
        // removing a workspace walks and unlinks directories, and the store's
        // index is held only while its entry is dropped.
        let (store, dir, workspace_owned) = (
            Arc::clone(&self.inner.store),
            self.inner.dir.clone(),
            workspace.to_owned(),
        );
        let removed = tokio::task::spawn_blocking(move || {
            if let Err(error) = store.forget(workspace_id) {
                tracing::warn!(%workspace_owned, %error, "a deleted workspace's history stays in the store");
            }
            remove_files(&dir, &workspace_owned);
        })
        .await;
        if let Err(error) = removed {
            tracing::warn!(%workspace, %error, "a deleted workspace's files are not removed");
        }
    }
}

/// Whether the host opening the records in `dir` passes over `path`: anything but a record; the
/// marker of a workspace deleted here, which `direct` refuses as deleted from now on, whoever asks;
/// and the record of one deleted while the host stopped halfway, whose remains go.
pub(super) fn passed_over(
    dir: &Path,
    direct: &tilo_peer_state::direct::Serving,
    path: &Path,
) -> bool {
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(DELETED) => {
            if let Some(workspace_id) = stem.and_then(tilo_server_protocol::WorkspaceId::parse) {
                direct.delete(workspace_id);
            }
            true
        }
        Some("workspace") if path.with_extension(DELETED).exists() => {
            if let Some(workspace) = stem {
                remove_files(dir, workspace);
            }
            true
        }
        // A branch a stop cut short was never anyone's: none of it opens, and
        // its marker stays for the next ask under its identity, which discards
        // what else it left ([`Workspaces::discard_unfinished`]).
        Some("workspace") if path.with_extension(BRANCHING).exists() => {
            if let Some(workspace) = stem {
                remove_files(dir, workspace);
            }
            true
        }
        Some("workspace") => false,
        _ => true,
    }
}

/// Remove what the host whose workspaces' records are in `dir` keeps of
/// `workspace` on disk: the files beside its record, its record last, and the
/// state and history it retained. The marker of its deletion stays.
pub(super) fn remove_files(dir: &Path, workspace: &str) {
    let beside = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split_once('.'))
                .is_some_and(|(stem, what)| {
                    stem == workspace && what != DELETED && what != BRANCHING && what != "workspace"
                })
        });
    let mut paths: Vec<_> = beside.collect();
    paths.push(dir.join(workspace).with_extension("workspace"));
    if let (Some(state), Some(workspace_id)) = (
        dir.parent(),
        tilo_server_protocol::WorkspaceId::parse(workspace),
    ) {
        paths.push(state.join("workspaces").join(workspace_id.to_string()));
        paths.push(state.join("files-puts").join(workspace_id.to_string()));
        paths.push(
            state
                .join("store")
                .join("workspaces")
                .join(workspace_id.to_string()),
        );
    }
    for path in paths {
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "a deleted workspace's file stays");
        }
    }
}

impl Workspaces {
    /// The marker of `branch`, a branch's identity (a UUID), beside where its
    /// record is.
    fn branching_marker(&self, branch: &str) -> std::path::PathBuf {
        self.inner.dir.join(branch).with_extension(BRANCHING)
    }

    /// Whether `branch` is a branch this host began and never finished: never
    /// answered to anyone, so it may be discarded and made again.
    pub(super) fn unfinished_branch(&self, branch: &str) -> bool {
        tilo_server_protocol::WorkspaceId::parse(branch).is_some()
            && self.branching_marker(branch).exists()
    }

    #[cfg(feature = "agents")]
    /// Mark `branch` as a branch being made, durably, before anything of it is:
    /// only an identity nothing here holds, so a marker never stands beside a
    /// workspace made otherwise.
    ///
    /// # Errors
    ///
    /// Something here holds `branch` already, or the marker could not be written.
    pub(super) async fn begin_branch(&self, branch: &str) -> Result<(), String> {
        let record = self.inner.dir.join(branch);
        let held = self.inner.workspaces.lock().await.contains_key(branch)
            || ["workspace", DELETED, super::RELEASED]
                .iter()
                .any(|extension| record.with_extension(extension).exists());
        if held || tilo_server_protocol::WorkspaceId::parse(branch).is_none() {
            return Err("this host has that workspace already".to_owned());
        }
        let marker = self.branching_marker(branch);
        tokio::task::spawn_blocking(move || synced(&marker))
            .await
            .map_err(|error| error.to_string())?
    }

    #[cfg(feature = "agents")]
    /// `branch` is its person's: its marker goes, durably, before anyone is
    /// answered it.
    ///
    /// # Errors
    ///
    /// The marker could not be removed: the branch is not finished.
    pub(super) async fn finish_branch(&self, branch: &str) -> Result<(), String> {
        let marker = self.branching_marker(branch);
        tokio::task::spawn_blocking(move || {
            std::fs::remove_file(&marker).map_err(|error| error.to_string())?;
            sync_parent(&marker)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[cfg(feature = "agents")]
    /// Discard `branch`, a branch this host began and never finished
    /// ([`Self::unfinished_branch`]): nobody was answered its identity, so,
    /// unlike a deleted workspace, nothing marks it gone and it can be made
    /// again under the same identity. Its machine goes first, as any
    /// workspace's does; then what this host holds and keeps of it, the
    /// content its records named, and its marker last, so a host stopped
    /// halfway discards the rest on the next ask. Refused for any other
    /// workspace: a workspace that was ever answered is deleted, never this.
    ///
    /// # Errors
    ///
    /// `branch` is not an unfinished branch, or its machine or files stay.
    pub(super) async fn discard_unfinished(&self, branch: &str) -> Result<(), String> {
        let workspace_id = tilo_server_protocol::WorkspaceId::parse(branch)
            .filter(|_| self.unfinished_branch(branch))
            .ok_or("that is no branch being made here")?;
        self.delete_machine(branch).await?;
        self.discard(branch, workspace_id).await?;
        let content = self.inner.branch_content.read_recover().clone();
        if let Some(content) = content {
            let branch = branch.to_owned();
            tokio::task::spawn_blocking(move || content.discard(&branch))
                .await
                .map_err(|error| error.to_string())??;
        }
        let marker = self.branching_marker(branch);
        tokio::task::spawn_blocking(move || {
            std::fs::remove_file(&marker).map_err(|error| error.to_string())?;
            sync_parent(&marker)
        })
        .await
        .map_err(|error| error.to_string())??;
        tracing::info!(%branch, "a branch that did not take everything was discarded");
        Ok(())
    }
}

#[cfg(feature = "agents")]
/// Write the empty file `path` and its directory's entry for it to disk.
fn synced(path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    sync_parent(path)
}

#[cfg(feature = "agents")]
/// Write `path`'s directory to disk: a file made or removed in it stays so.
fn sync_parent(path: &Path) -> Result<(), String> {
    match path.parent() {
        Some(dir) => std::fs::File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

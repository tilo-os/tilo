//! Deleting a governed workspace, as its owner asks
//! ([`tilo_relay_session::workspaces::DELETE_ROOM`]): final, and with
//! everything this host keeps of it.
//!
//! Releasing a workspace ([`Workspaces::release`]) stops serving it and keeps
//! its files, because it lives on elsewhere. Deleting it ends it: everyone in
//! it is told it was deleted ([`WORKSPACE_DELETED`]) while their session is
//! still open, and let in no more; its record, the state and history it
//! retained, and whatever else this host lists for it go from the disk and
//! from memory, so the storage it took no longer counts against its owner;
//! and whoever keeps more of it here (the content its members posted, the
//! app workers acting in it) hears it went ([`Workspaces::deletions`]).
//!
//! A marker beside where its record was is written first and stays
//! (`<workspace>.deleted`): a host stopped halfway finishes deleting it when it
//! opens again, and the workspace is never made again under its identity.

use std::path::Path;

use tilo_peer_state::GovernedAccess;
use tilo_relay_protocol::refusal::WORKSPACE_DELETED;

use super::Workspaces;
use tilo_lock::MutexRecover as _;

/// The extension of the marker of a workspace this host deleted.
pub(super) const DELETED: &str = "deleted";

impl Workspaces {
    /// Delete `workspace`, with everything this host keeps of it: final.
    /// Whose it is to delete the caller decides (the host's operation checks
    /// its owner).
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or the deletion could not be
    /// recorded. Once recorded, what is left of it is removed when the host
    /// opens again.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
    )]
    pub async fn delete(&self, workspace: &str) -> Result<(), String> {
        let governed = self
            .inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or("this host does not govern that room")?;
        let governed = governed.lock().await;
        let workspace_id = governed.workspace_id();
        // Recorded first: a host stopped halfway finishes it when it opens.
        let marker = governed.path.with_extension(DELETED);
        let file = std::fs::File::create(&marker).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        if let Some(dir) = marker.parent() {
            std::fs::File::open(dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| error.to_string())?;
        }
        self.inner.workspaces.lock().await.remove(workspace);
        self.inner.direct.delete(workspace_id);
        // Everyone in it hears why while their session is open, then nobody
        // is let in.
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
                .refuse_every_live_member(WORKSPACE_DELETED);
        }
        self.inner
            .host
            .access
            .replace(
                workspace_id,
                true,
                GovernedAccess {
                    member_devices: Vec::new(),
                    member_user_roots: Vec::new(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        crate::workspace_io::evict(&self.inner.host.workspaces, workspace_id).await;
        drop(governed);
        self.forget_deleted(workspace, workspace_id);
        tracing::info!(%workspace, workspace_id, "governed room deleted");
        let _ = self
            .inner
            .deletions
            .send((workspace.to_owned(), workspace_id));
        let _ = self.inner.changes.send(workspace.to_owned());
        Ok(())
    }

    /// Whether the workspace whose relay workspace is `workspace_id` was
    /// deleted here: whoever asks for it is told so
    /// ([`WORKSPACE_DELETED`]), the same on every lane.
    #[must_use]
    pub fn deleted(&self, workspace_id: u64) -> bool {
        self.inner.direct.deleted(workspace_id)
    }

    /// The workspaces this host deleted, as each goes: its id and the
    /// relay workspace that carried it. Whoever keeps more of a workspace
    /// than its record (the content its members posted, app workers acting
    /// in it) lets go of it on hearing.
    #[must_use]
    pub fn deletions(&self) -> tokio::sync::broadcast::Receiver<(String, u64)> {
        self.inner.deletions.subscribe()
    }

    /// Forget everything this host keeps of `workspace` (`workspace_id`),
    /// deleted: who owns it and what it took, who it listed, and its files. Its
    /// door went with its governance.
    fn forget_deleted(&self, workspace: &str, workspace_id: u64) {
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
        if let Err(error) = self.inner.store.compact(workspace_id, u64::MAX) {
            tracing::warn!(%workspace, %error, "a deleted workspace's history stays in the store");
        }
        remove_files(&self.inner.dir, workspace);
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
            if let Some(workspace_id) =
                stem.and_then(|stem| tilo_relay_protocol::room_workspace_id(stem).ok())
            {
                direct.delete(workspace_id);
            }
            true
        }
        Some("room") if path.with_extension(DELETED).exists() => {
            if let Some(workspace) = stem {
                remove_files(dir, workspace);
            }
            true
        }
        Some("room") => false,
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
                .is_some_and(|(stem, what)| stem == workspace && what != DELETED && what != "room")
        });
    let mut paths: Vec<_> = beside.collect();
    paths.push(dir.join(workspace).with_extension("room"));
    if let (Some(state), Ok(workspace_id)) = (
        dir.parent(),
        tilo_relay_protocol::room_workspace_id(workspace),
    ) {
        paths.push(state.join("workspaces").join(workspace_id.to_string()));
        paths.push(
            state
                .join("store")
                .join("workspaces")
                .join(format!("{workspace_id:016x}")),
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

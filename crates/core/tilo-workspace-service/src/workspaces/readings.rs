//! A hosted workspace as it is read without holding its lock: the replica
//! every change is made in, the workspace's name, the keys of the device it
//! is held on and the invitations its join log and its entry's addresses left
//! it. A workspace's lock is held across what takes long — asking the key
//! service, which settles a newcomer's access first, and a commit's writes
//! — so every read that can answer from the replica alone reads it here.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

use tilo_peer_state::direct::Replica;

use super::collaboration::CollaborationError;
use super::{DeviceKeys, HostedWorkspace, StatePeer, Workspaces};

/// What a hosted workspace is read through without its lock.
#[derive(Clone)]
pub(super) struct Reading {
    pub(super) replica: Replica,
    pub(super) workspace: String,
    /// The keys a sealed document is opened with; a build that opens none
    /// reads only what is in the clear.
    pub(super) device: Option<DeviceKeys>,
    /// The invitations the workspace honours, as its last write left them.
    pub(super) invitations: Invitations,
    /// Whether the workspace is a member copy, read as this device's member.
    pub(super) member: bool,
    /// The workspace's one store, read a command at a time: what a record's reads answer from
    /// while the workspace's lock is held by a write waiting on a key or a link.
    pub(super) store: std::sync::Arc<std::sync::Mutex<tilo_resource_store::Store>>,
    /// Where that store keeps the files of the documents it leaves memory.
    pub(super) root: std::path::PathBuf,
}

/// What a workspace's join log and entry say of the invitations it issued,
/// kept beside the workspace: a page reading an invitation reads a card here,
/// at once, however long the workspace's lock stays taken.
/// A workspace this service holds, as [`Workspaces::copy_now`] reads it.
#[derive(Clone)]
pub struct HeldCopy {
    /// Kept as a member copy, not hosted here.
    pub member: bool,
    /// The name its governance gives it.
    pub name: String,
    /// Its replica.
    pub peer: StatePeer,
}

impl HeldCopy {
    fn of(reading: &Reading) -> Self {
        let peer = reading.replica.read(StatePeer::clone);
        Self {
            member: reading.member,
            name: peer.governance().workspace_name().unwrap_or_default(),
            peer,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Invitations(Arc<std::sync::Mutex<Arc<Issued>>>);

/// The invitations a workspace issued, as [`Invitations`] answers them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Issued {
    /// Every invitation its join log keeps, by id: whether someone took it
    /// back.
    pub(super) offered: BTreeMap<[u8; 32], bool>,
    /// The invitations someone redeemed.
    pub(super) redeemed: BTreeSet<[u8; 32]>,
    /// The invitations sent to an address at its entry.
    pub(super) mailed: BTreeSet<[u8; 32]>,
}

impl Issued {
    /// What the join log says of the invitation `invite_id`
    /// ([`tilo_peer_state::join_log::Offer`]).
    pub(super) fn offer(&self, invite_id: [u8; 32]) -> tilo_peer_state::join_log::Offer {
        tilo_peer_state::join_log::Offer::of(
            self.offered.get(&invite_id).copied(),
            self.redeemed.contains(&invite_id),
        )
    }

    /// Whether the invitation `invite_id` was sent to an address left at the
    /// workspace's entry.
    pub(super) fn mailed(&self, invite_id: [u8; 32]) -> bool {
        self.mailed.contains(&invite_id)
    }
}

impl Invitations {
    /// What the workspace's last write left of its invitations.
    pub(super) fn now(&self) -> Arc<Issued> {
        Arc::clone(&self.0.lock_recover())
    }

    /// Keep `change` made to what the workspace's invitations are: the
    /// workspace writes it under its lock, so a page reading a card always
    /// sees what it last answered for.
    pub(super) fn change(&self, change: impl FnOnce(&mut Issued)) {
        let mut kept = self.0.lock_recover();
        let mut next = Issued::clone(&kept);
        change(&mut next);
        *kept = Arc::new(next);
    }
}

type Listed = std::sync::Weak<tokio::sync::Mutex<HostedWorkspace>>;

/// The [`Reading`] of each workspace hosted here, kept from the moment it is kept. A read of a
/// session's own reads only this: such a read often waits with its whole thread, and a task of
/// that thread queued for a workspace's lock, or for the registry's, would be handed the lock's
/// next turn and never be polled to take it.
pub(super) struct Readings(
    pub(super) std::sync::Mutex<HashMap<WorkspaceId, (Listed, Reading)>>,
    /// Moves whenever a workspace is held here that was not before.
    tokio::sync::watch::Sender<u64>,
);

impl Default for Readings {
    fn default() -> Self {
        Self(
            std::sync::Mutex::default(),
            tokio::sync::watch::Sender::new(0),
        )
    }
}

impl Readings {
    pub(super) fn keep(
        &self,
        workspace: WorkspaceId,
        hosted: &std::sync::Arc<tokio::sync::Mutex<HostedWorkspace>>,
        reading: Reading,
    ) {
        let mut known = self.0.lock_recover();
        known.retain(|_, (of, _)| of.strong_count() > 0);
        let held = known
            .insert(workspace, (std::sync::Arc::downgrade(hosted), reading))
            .is_none();
        drop(known);
        if held {
            self.1.send_modify(|holds| *holds = holds.wrapping_add(1));
        }
    }

    /// What moves whenever a workspace is held here that was not before.
    pub(crate) fn holds(&self) -> tokio::sync::watch::Receiver<u64> {
        self.1.subscribe()
    }

    /// Read `hosted`, kept here as `workspace`, from now on.
    pub(super) fn hold(
        &self,
        workspace: &str,
        hosted: &std::sync::Arc<tokio::sync::Mutex<HostedWorkspace>>,
        reading: Reading,
    ) {
        if let Some(workspace) = WorkspaceId::parse(workspace) {
            self.keep(workspace, hosted, reading);
        }
    }

    /// Read `workspace` no more: it is not held here.
    pub(super) fn forget(&self, workspace: &str) {
        if let Some(workspace) = WorkspaceId::parse(workspace) {
            self.0.lock_recover().remove(&workspace);
        }
    }

    /// What `workspace`, held here, is read through.
    pub(super) fn held(&self, workspace: &str) -> Option<Reading> {
        let (of, reading) = self
            .0
            .lock_recover()
            .get(&WorkspaceId::parse(workspace)?)?
            .clone();
        (of.strong_count() > 0).then_some(reading)
    }

    /// The workspaces held here, by their ids.
    /// Every workspace held now, with what it is read through.
    fn all(&self) -> Vec<(WorkspaceId, Reading)> {
        self.0
            .lock_recover()
            .iter()
            .filter(|(_, (of, _))| of.strong_count() > 0)
            .map(|(workspace, (_, reading))| (*workspace, reading.clone()))
            .collect()
    }

    pub(crate) fn workspaces(&self) -> Vec<String> {
        self.0
            .lock_recover()
            .iter()
            .filter(|(_, (of, _))| of.strong_count() > 0)
            .map(|(workspace, _)| workspace.to_string())
            .collect()
    }
}

impl HostedWorkspace {
    /// What this workspace is read through without its lock.
    pub(super) fn reading(&self) -> Reading {
        Reading {
            replica: self.peer.clone(),
            workspace: self.workspace().to_owned(),
            device: self.device.clone(),
            invitations: self.invitations.clone(),
            member: self.member.is_some(),
            store: self.resources.shared(),
            root: self.resources.root().to_path_buf(),
        }
    }
}

impl Reading {
    /// The store's document `doc`, saved as the store holds it now, read without the workspace's
    /// lock: from memory, or from the file it kept. `None` for a document the store placed none
    /// by.
    pub(super) fn document(&self, doc: &str) -> Option<Vec<u8>> {
        super::resources::kept_document(&self.store, &self.root, doc)
    }

    /// The store's content document of `app`'s record `record`, saved as the store holds it,
    /// read without the workspace's lock. `None` for a record the store holds nothing of.
    pub(super) fn record(
        &self,
        surfaces: &collab_types::app_surface::Surfaces,
        app: &str,
        record: &str,
    ) -> Option<Vec<u8>> {
        super::resources::kept_record(&self.store, surfaces, app, record)
    }

    /// The store, a command at a time, without the workspace's lock.
    pub(super) fn store(&self) -> std::sync::MutexGuard<'_, tilo_resource_store::Store> {
        use tilo_lock::MutexRecover as _;
        self.store.lock_recover()
    }

    /// Every resource of the store that `query` finds, page after page, by id with what the
    /// index holds of it: read one command at a time, without the workspace's lock.
    pub(super) fn found(
        &self,
        mut query: collab_types::store::Query,
    ) -> Vec<(String, collab_types::store::Indexed)> {
        let mut found = Vec::new();
        {
            let store = self.store();
            loop {
                let page = store.query(&query, &|_| true);
                found.extend(page.resources);
                match page.next {
                    Some(next) => query.after = Some(next),
                    None => break,
                }
            }
            drop(store);
        }
        found
    }
}

impl Workspaces {
    /// What `workspace` is read through: its store and its copy, a command or a read at a time.
    /// No lock of the workspace is taken, and none is waited for: a workspace that is held but
    /// has not kept its reading yet is one still opening, and this waits for that reading —
    /// published before the open answers — rather than for the workspace.
    ///
    /// # Errors
    ///
    /// This host holds no such workspace.
    pub(super) async fn reading_now(&self, workspace: &str) -> Result<Reading, CollaborationError> {
        if let Some(reading) = self.inner.readings.held(workspace) {
            return Ok(reading);
        }
        let mut kept = self.inner.readings.holds();
        loop {
            if let Some(reading) = self.inner.readings.held(workspace) {
                return Ok(reading);
            }
            if !self.inner.workspaces.lock().await.contains_key(workspace) {
                return Err(CollaborationError::new(
                    "this server does not hold that workspace",
                ));
            }
            if kept.changed().await.is_err() {
                return Err(CollaborationError::new(
                    "this server does not hold that workspace",
                ));
            }
        }
    }

    /// What the workspace `workspace` hosted here is read through without
    /// its lock; `None` for a workspace not hosted here. Only the first read
    /// of a workspace opened here waits for its lock.
    /// `workspace` as this service holds it now, read without waiting on it: a caller on the
    /// runtime that serves the workspaces waits on nothing that runtime has to run. A wait on the
    /// workspace's lock from there waits on itself when a task of that runtime holds the lock.
    /// `None` for a workspace not held here.
    #[must_use]
    pub fn copy_now(&self, workspace: WorkspaceId) -> Option<HeldCopy> {
        self.inner
            .readings
            .held(&workspace.to_string())
            .map(|reading| HeldCopy::of(&reading))
    }

    /// Every workspace this service holds now, read as [`Self::copy_now`] reads one.
    #[must_use]
    pub fn copies_now(&self) -> Vec<(WorkspaceId, HeldCopy)> {
        self.inner
            .readings
            .all()
            .into_iter()
            .map(|(workspace, reading)| (workspace, HeldCopy::of(&reading)))
            .collect()
    }

    /// What the workspace `workspace` is read through, by its id spelled out: [`Self::reading`]
    /// for the callers that hold the id as the command names it.
    pub(super) async fn reading_of(&self, workspace: &str) -> Option<Reading> {
        self.reading(WorkspaceId::parse(workspace)?).await
    }

    pub(super) async fn reading(&self, workspace: WorkspaceId) -> Option<Reading> {
        self.reading_now(&workspace.to_string()).await.ok()
    }

    /// Keep what `workspace`, held here, is read through without its lock: a
    /// page's first read of it then needs no turn at the workspace's own.
    pub(super) async fn keep_reading(&self, workspace: &Arc<tokio::sync::Mutex<HostedWorkspace>>) {
        let held = workspace.lock().await;
        let (workspace_id, reading) = (held.workspace_id(), held.reading());
        drop(held);
        self.inner.readings.keep(workspace_id, workspace, reading);
    }

    /// `answer` of the governance of the workspace `workspace` hosted here,
    /// as its replica holds it now; `None` for a workspace not hosted here.
    pub async fn governing<T>(
        &self,
        workspace: WorkspaceId,
        answer: impl FnOnce(&Governance<'_>) -> T,
    ) -> Option<T> {
        let reading = self.reading(workspace).await?;
        let peer = reading.replica.lock();
        Some(answer(&Governance {
            peer: &peer,
            #[cfg(feature = "agents")]
            workspace: &reading.workspace,
        }))
    }
}

/// A hosted workspace's governance as its replica holds it now: what every
/// decision of who reads what is made against.
pub struct Governance<'a> {
    pub peer: &'a StatePeer,
    /// The workspace's name, which only the views pages read ask for.
    #[cfg(feature = "agents")]
    pub workspace: &'a str,
}

/// What the broker admits an app's connection and checks its calls by: the workspaces this
/// service holds, each as its copy here describes it, read without waiting on any workspace's
/// lock.
#[cfg(feature = "apps")]
impl tilo_broker_core::services::WorkspaceAuthority for Workspaces {
    /// The revocations the workspace's copy here holds: what an agent's key and a saved
    /// shortcut's signer are checked against here.
    fn workspace_revocations(&self, workspace: &str) -> Option<tilo_user_identity::RevocationList> {
        let peer = self.copy_now(WorkspaceId::parse(workspace)?)?.peer;
        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in peer.revocations() {
            revocations.insert(entry).ok()?;
        }
        Some(revocations)
    }

    /// The registries the workspace's copy here records: what a moderation request's destination
    /// is checked against here.
    fn package_registries(&self, workspace: &str) -> Option<serde_json::Value> {
        let peer = self.copy_now(WorkspaceId::parse(workspace)?)?.peer;
        Some(serde_json::json!({ "registries": peer.governance().package_registries() }))
    }

    /// The work kept done in the workspace's copy here: what counts a shortcut's triggered runs
    /// here.
    fn done_under(&self, workspace: &str, prefix: &str) -> Vec<serde_json::Value> {
        tilo_failure::ignored!(
            "a name that is no workspace id names no workspace held here, nor work done in one",
            WorkspaceId::parse(workspace)
        )
        .and_then(|id| self.copy_now(id))
        .map(|copy| {
            copy.peer
                .workspace()
                .done_under(prefix)
                .into_values()
                .map(|done| done.answer)
                .collect()
        })
        .unwrap_or_default()
    }

    /// Every workspace held here, as one tree: the ones served from here first, then the ones it
    /// takes part in.
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        let tree = tilo_desktop_model::Layout {
            workspaces: self.workspaces_now(),
            ..tilo_desktop_model::Layout::default()
        };
        read(&tree);
        Ok(())
    }

    /// Only the requested workspace is built.
    fn with_workspace(
        &self,
        workspace: &str,
        read: &mut dyn FnMut(Option<&tilo_desktop_model::Workspace>),
    ) -> Result<(), String> {
        let held = WorkspaceId::parse(workspace)
            .and_then(|id| self.workspace_now(id))
            .filter(|held| held.id().to_string() == workspace);
        read(held.as_ref());
        Ok(())
    }
}

impl Workspaces {
    /// The workspaces held here, each as its copy describes it: the ones served from here first,
    /// then the ones it takes part in.
    #[must_use]
    pub fn workspaces_now(&self) -> Vec<tilo_desktop_model::Workspace> {
        let mut copies = self.copies_now();
        copies.sort_by_key(|(_, copy)| copy.member);
        copies
            .into_iter()
            .filter_map(|(workspace, copy)| super::describe(workspace, &copy.name, &copy.peer))
            .collect()
    }

    /// One workspace as [`Self::workspaces_now`] describes it, built alone.
    #[must_use]
    pub fn workspace_now(&self, workspace: WorkspaceId) -> Option<tilo_desktop_model::Workspace> {
        let copy = self.copy_now(workspace)?;
        super::describe(workspace, &copy.name, &copy.peer)
    }
}

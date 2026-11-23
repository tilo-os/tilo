//! Each joined workspace as its governance admits it: the page holds its workspaces
//! in the same joined-workspace owner native devices hold theirs in
//! ([`tilo_peer_state::owner::NativeWorkspaceOwner`]), kept in the page's
//! storage through its [`PeerStore`].
//!
//! The host sends a subscriber the workspace's bootstrap ahead of its replay
//! ([`tilo_server_protocol::Envelope::WorkspaceBootstrap`]): the workspace-join
//! contract's membership answer, the workspace's governing authority, roster and
//! revocations. The owner holds the workspace on that answer, vouched for by the
//! host the page authenticated to
//! ([`tilo_peer_state::owner::NativeWorkspaceOwner::install_vouched`]), and admits
//! every governance message the host relays through it: members'
//! certificates and the workspace document. What the workspace's governance says is
//! read off that replica: whether it shares its members' layout, who its
//! members are, who takes part in its channels, and whether a member's
//! write reaches the page's apps. The replica decides each document as
//! native devices do (its author's certificate and role, its signature and
//! route, its claims against the app surface it names) and keeps none: the
//! page's apps hold their records.
//!
//! A workspace's replica is the only source of what its governance says: a
//! workspace with no governance yet (no bootstrap received) admits no
//! member's write and names no key holders. A page places its
//! person's own workspaces on their host before joining them
//! ([`Outbox::join_placed`](crate::Outbox::join_placed)), so those have governance too.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collab_types::Ed25519PublicKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{
    Change, JournalParts, Journaled, NativeAuthorityStorage, NativeCommittedView,
    NativeWorkspaceOwner, Store,
};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

use crate::membership_check::PeerStore;
use tilo_peer_state::document_frame::DocumentRoute;

/// The owner's record in the page's [`PeerStore`]. A record the store
/// refused leaves the owner fenced for the rest of the session, as a
/// native owner is after an uncertain write: the next session starts from
/// what the store holds.
struct PageWorkspaces {
    store: Arc<dyn PeerStore>,
    healthy: AtomicBool,
    /// The runs of ownership this page keeps beside its record, by name: those the record it last
    /// kept names, and those put since. A store without runs of its own reads them from here.
    runs: std::sync::Mutex<BTreeMap<String, Option<Vec<u8>>>>,
    /// The record kept as an index and per-workspace parts, so a publication
    /// writes what it changed rather than the record. `None` when the store
    /// keeps the record whole ([`PeerStore::keeps_workspaces_parts`]).
    journaled: std::sync::Mutex<Option<Journaled>>,
    /// Whether the store keeps the record of the workspaces it holds at all.
    keeps: bool,
    /// Whether the store keeps the record's parts at all.
    parts: bool,
}

/// The record's parts as the page's store keeps them
/// ([`PeerStore::load_workspaces_part`] and its siblings).
struct StoreParts(Arc<dyn PeerStore>);

impl JournalParts for StoreParts {
    fn read_part(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.load_workspaces_part(name))
    }

    fn write_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.0.save_workspaces_part(name, bytes)
    }

    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.0.append_workspaces_part(name, bytes)
    }
}

impl PageWorkspaces {
    fn new(store: Arc<dyn PeerStore>) -> Self {
        let keeps = store.keeps_workspaces();
        let parts = keeps && store.keeps_workspaces_parts();
        Self {
            store,
            healthy: AtomicBool::new(true),
            runs: std::sync::Mutex::default(),
            journaled: std::sync::Mutex::default(),
            keeps,
            parts,
        }
    }

    fn parts(&self) -> StoreParts {
        StoreParts(Arc::clone(&self.store))
    }

    /// The record this store kept, and the journaled form of it the next
    /// publication writes in.
    fn kept(&self) -> Option<NativeAuthorityStorage> {
        if !self.keeps {
            return None;
        }
        if self.parts {
            match Journaled::open(&self.parts()) {
                Ok(Some((journaled, record))) => {
                    *self.journaled.lock_recover() = Some(journaled);
                    return Some(record);
                }
                Ok(None) => return None,
                Err(error) => {
                    tracing::warn!(%error, "the workspaces this page holds did not read");
                    return None;
                }
            }
        }
        self.store
            .load_workspaces()
            .and_then(|bytes| serde_json::from_slice::<NativeAuthorityStorage>(&bytes).ok())
    }
}

impl Store for PageWorkspaces {
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn uncertain(&self, _workspace: WorkspaceId) -> bool {
        !self.healthy()
    }

    fn shows(&self, _workspace: WorkspaceId) -> bool {
        false
    }

    fn put_ownership(&self, workspace: WorkspaceId, bytes: &[u8]) -> Result<[u8; 32], String> {
        let hash = *blake3::hash(bytes).as_bytes();
        let name = tilo_peer_state::owner::ownership_run_name(workspace, hash);
        if self.store.keeps_workspaces() {
            self.store.save_workspaces_run(&name, Some(bytes))?;
        }
        self.runs.lock_recover().insert(name, Some(bytes.to_vec()));
        Ok(hash)
    }

    fn read_ownership(&self, workspace: WorkspaceId, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        let name = tilo_peer_state::owner::ownership_run_name(workspace, hash);
        let kept = self.runs.lock_recover().get(&name).cloned().flatten();
        if let Some(bytes) = kept {
            return Ok(bytes);
        }
        self.store
            .load_workspaces_run(&name)
            .ok_or_else(|| format!("the run of ownership {name} is not kept"))
    }

    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        // Made only for a store that keeps it: the record holds every record of every workspace,
        // and a device writing a large folder makes it after each write. A store that keeps its
        // parts writes only what this publication changed.
        let written = if !self.keeps {
            Ok(())
        } else if self.parts {
            let parts = self.parts();
            let mut journaled = self.journaled.lock_recover();
            if journaled.is_none() {
                *journaled = Some(Journaled::start(next));
            }
            match journaled.as_mut() {
                Some(journaled) => journaled.publish(&parts, next, &change),
                None => Ok(()),
            }
        } else {
            serde_json::to_vec(next)
                .map_err(|error| error.to_string())
                .and_then(|bytes| self.store.save_workspaces(&bytes))
        };
        if let Err(error) = written {
            self.healthy.store(false, Ordering::Release);
            return Err(error);
        }
        install();
        // A run the record no longer names moved to ownership's head.
        let named: BTreeSet<String> = next.ownership_runs().collect();
        let gone: Vec<String> = {
            let mut runs = self.runs.lock_recover();
            let gone = runs
                .keys()
                .filter(|name| !named.contains(*name))
                .cloned()
                .collect();
            runs.retain(|name, _| named.contains(name));
            gone
        };
        for name in gone.into_iter().filter(|_| self.keeps) {
            if let Err(error) = self.store.save_workspaces_run(&name, None) {
                tracing::warn!(%error, "a run of ownership the record no longer names was kept");
            }
        }
        Ok(())
    }
}

/// The workspaces this page joined, by workspace.
pub(crate) struct Replicas {
    /// This device.
    device: DeviceCert,
    owner: NativeWorkspaceOwner,
    workspaces: BTreeSet<WorkspaceId>,
}

impl Replicas {
    /// The workspaces of `device`, as `store` last kept them. A record that no
    /// longer restores is let go: the host's bootstraps and replays bring
    /// its workspaces back.
    pub(crate) fn new(device: DeviceCert, store: Arc<dyn PeerStore>) -> Self {
        let now = tilo_clock::wall();
        let workspaces = Arc::new(PageWorkspaces::new(store));
        let kept = workspaces.kept();
        let runs = kept
            .iter()
            .flat_map(NativeAuthorityStorage::ownership_runs)
            .map(|name| (name, None))
            .collect();
        *workspaces.runs.lock_recover() = runs;
        let owner = kept
            .and_then(|kept| NativeWorkspaceOwner::restore(workspaces.clone(), kept, now).ok())
            .filter(|owner| {
                owner.storage().held().all(|(workspace, _)| {
                    owner
                        .with_current(workspace, |view| Ok(view.device() == &device))
                        .unwrap_or(false)
                })
            });
        if let Some(owner) = &owner
            && let Some(journaled) = workspaces.journaled.lock_recover().as_mut()
        {
            journaled.resume(owner);
        }
        let owner = owner.unwrap_or_else(|| NativeWorkspaceOwner::empty(workspaces));
        let workspaces = owner
            .storage()
            .held()
            .map(|(workspace, _)| workspace)
            .collect();
        Self {
            device,
            owner,
            workspaces,
        }
    }

    fn with_workspace<T>(
        &self,
        workspace_id: WorkspaceId,
        read: impl FnOnce(&NativeCommittedView) -> Result<T, String>,
    ) -> Option<Result<T, String>> {
        if !self.workspaces.contains(&workspace_id) {
            return None;
        }
        Some(self.owner.with_current(workspace_id, read))
    }

    /// Hold `workspace_id`'s workspace on the bootstrap the host sent (`bytes`,
    /// a postcard membership answer). A replica of the same workspace and
    /// authority stays: a resumed subscription replays only what it has not
    /// received, and the session follows what that replica's governance
    /// already says right after the answer.
    ///
    /// # Errors
    /// The bootstrap is not a membership answer for this device and workspace,
    /// or the workspace it names does not build.
    pub(crate) fn bootstrap(
        &mut self,
        workspace_id: WorkspaceId,
        bytes: &[u8],
        now: SystemTime,
    ) -> Result<(), String> {
        self.owner
            .install_vouched(workspace_id, bytes, &self.device, now)?;
        if self
            .owner
            .storage()
            .held()
            .any(|(workspace, _)| workspace == workspace_id)
        {
            self.workspaces.insert(workspace_id);
        }
        Ok(())
    }

    /// Admit `message`, governance `author` sent to `workspace_id`, as of when
    /// it was signed ([`relayed`]). The report says whether it changed
    /// what the workspace's governance says, and the documents it let the replica decide.
    ///
    /// # Errors
    /// No replica of the workspace is held, or its governance refuses the
    /// message.
    pub(crate) fn admit(
        &self,
        workspace_id: WorkspaceId,
        author: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<tilo_peer_state::AdmissionReport, tilo_peer_state::AdmissionError> {
        if !self.workspaces.contains(&workspace_id) {
            return Err(tilo_peer_state::AdmissionError::rejected(
                neighbour_wire::RefusalReason::MissingDependency,
                "the workspace is not held here yet",
            ));
        }
        self.owner
            .receive(workspace_id, relayed(author), message, now)
    }

    /// Keep `documents`, each a document its author wrote to `workspace_id` ([`relayed`]), in
    /// its replica as the workspace's governance decides
    /// them, in order and in one owner turn: one report per document. What it cannot decide yet
    /// the replica holds, and decides once it can.
    ///
    /// # Errors
    /// No replica of the workspace is held, or committing the replica failed.
    pub(crate) fn keep(
        &self,
        workspace_id: WorkspaceId,
        documents: Vec<([u8; 32], NeighbourMsg)>,
        now: SystemTime,
    ) -> Result<
        Vec<Result<tilo_peer_state::AdmissionReport, tilo_peer_state::AdmissionError>>,
        String,
    > {
        self.workspace(workspace_id)
            .ok_or("the workspace has no governance")?;
        let inputs = documents
            .into_iter()
            .map(|(author, message)| (relayed(author), message))
            .collect();
        self.owner.receive_batch(workspace_id, inputs, now)
    }

    /// Whether `workspace_id`'s governance, as this replica holds it, admits
    /// this device now: what it reads the workspace's records as. A host
    /// replays a workspace's governance in parts, and its newest part — the
    /// change that admitted a device that just entered — can follow what the
    /// device was already sent.
    pub(crate) fn admits_this_device(&self, workspace_id: WorkspaceId) -> bool {
        let device = self.device.device_public_key;
        self.with_workspace(workspace_id, |view| {
            view.peer()
                .permissions(device, tilo_clock::wall())
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .is_some_and(|admitted| admitted.is_ok())
    }

    /// The workspace held as `workspace_id`, once its governance is.
    pub(crate) fn workspace(&self, workspace_id: WorkspaceId) -> Option<WorkspaceId> {
        self.workspaces
            .contains(&workspace_id)
            .then_some(workspace_id)
    }

    /// `workspace_id`'s workspace and the members it lists by person id.
    pub(crate) fn members(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<(String, Option<BTreeSet<String>>)> {
        self.with_workspace(workspace_id, |view| {
            let peer = view.peer();
            let doc = peer.admitted_workspace();
            Ok((
                peer.workspace_id().to_owned(),
                doc.and_then(tilo_peer_state::members::people),
            ))
        })?
        .ok()
    }

    /// What `workspace_id`'s governance calls each of its people, by person (root key, hex): the
    /// name its member list shows, also for someone it removed, whose changes still read as
    /// theirs. Everything that names a person here who is not here now names them by this.
    pub(crate) fn member_names(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<BTreeMap<String, String>> {
        self.with_workspace(workspace_id, |view| {
            let list = view
                .peer()
                .governance()
                .governed_members(tilo_clock::wall())?;
            Ok(list
                .members
                .iter()
                .filter_map(|member| {
                    let person = member.id.person_id()?.to_owned();
                    let name = member.name.trim();
                    (!name.is_empty()).then(|| (person, name.to_owned()))
                })
                .collect())
        })?
        .ok()
    }

    /// Whose each of `workspace_id`'s member devices is, as its governance admitted them: device
    /// (hex) to person (root key, hex), for every device a current member is here from. What
    /// shows a device by its person (who is here, a call's roster) reads it off this.
    pub(crate) fn member_devices(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<BTreeMap<String, String>> {
        self.with_workspace(workspace_id, |view| {
            let list = view
                .peer()
                .governance()
                .governed_members(tilo_clock::wall())?;
            Ok(list
                .members
                .iter()
                .filter(|member| !member.removed)
                .filter_map(|member| {
                    let person = member.id.person_id()?.to_owned();
                    let devices = list.devices.get(&member.id)?;
                    Some(
                        devices
                            .iter()
                            .map(move |device| (hex::encode(device), person.clone())),
                    )
                })
                .flatten()
                .collect())
        })?
        .ok()
    }

    /// What each call of `workspace_id` allows, by channel, as the workspace keeps it.
    pub(crate) fn call_rules(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<BTreeMap<String, collab_types::calls::CallRule>> {
        self.with_workspace(workspace_id, |view| Ok(view.peer().governance().calls()))?
            .ok()
    }

    /// The workspaces' governance, as the channels' directory reads it.
    pub(crate) fn owner(&self) -> NativeWorkspaceOwner {
        self.owner.clone()
    }

    /// Where `workspace_id`'s host listens, read off the signed hint the
    /// workspace's own governance keeps for it: the address a peer dials the
    /// host at, and the relay it is reached through. `None` until the host's
    /// bootstrap or a governance message named it.
    pub(crate) fn host(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<tilo_server_client::ServerAddress> {
        let hint = self
            .with_workspace(workspace_id, |view| {
                let host = view.workspace_host().device_public_key;
                Ok(view
                    .peer()
                    .governance()
                    .keeping_devices()
                    .into_iter()
                    .find(|hint| hint.host.device_public_key == host))
            })?
            .ok()??;
        let payload =
            tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(
                &hint,
            )
            .ok()?;
        let relay = match payload.packet_relay_url.as_str() {
            "iroh:direct" => None,
            url => Some(url.parse().ok()?),
        };
        Some(tilo_server_client::ServerAddress::through(
            payload.endpoint_addr,
            relay,
        ))
    }

    /// The devices that hold the keys of `route`'s workspace, as the
    /// channels' directory names them; `None` while the workspace's
    /// governance does not answer.
    pub(crate) fn key_holders(&self, route: &DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> {
        use tilo_workspace_keys::KeyDirectory as _;
        let workspace = WorkspaceId::parse(&route.cluster_id)?;
        crate::workspace_keys::OwnerDirectory::reader(self.owner())
            .allowed_devices(workspace, tilo_clock::unix_now())
    }

    /// One line per workspace for diagnostics: the host it was joined through.
    pub(crate) fn describe(&self) -> String {
        self.workspaces
            .iter()
            .filter_map(|workspace_id| {
                self.with_workspace(*workspace_id, |view| {
                    Ok(format!(
                        "workspace {}: joined through {}",
                        view.peer().workspace_id(),
                        hex::encode(&view.workspace_host().device_public_key[..4])
                    ))
                })?
                .ok()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A write `author` signed when it made it, which the workspace's host sent
/// this session, relayed as it arrived or from what its store kept: decided
/// as of its own timestamp, and in the clear only before its record sealed.
fn relayed(author: [u8; 32]) -> TransportOrigin {
    TransportOrigin::RetainedServerDelta(author)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::owner::testing;

    fn device() -> (SigningKey, DeviceCert) {
        let root = SigningKey::from_bytes(&[41; 32]);
        let key = SigningKey::from_bytes(&[42; 32]);
        (
            key.clone(),
            testing::certificate(&root, &key, "A's browser"),
        )
    }

    /// A workspace this page holds is read back after the page restarts from
    /// what its store kept: the durability a page's writable state must have.
    #[test]
    fn the_workspace_this_page_holds_survives_a_restart() -> Result<(), Box<dyn std::error::Error>>
    {
        let (key, certificate) = device();
        let store: Arc<dyn PeerStore> = Arc::new(crate::MemoryPeerStore::default());
        let workspace = {
            let replicas = Replicas::new(certificate.clone(), Arc::clone(&store));
            let owner = replicas.owner();
            let workspace = testing::founded(&owner, &certificate, &key)?;
            if !owner.storage().held().any(|(held, _)| held == workspace) {
                return Err("the page does not hold the workspace it founded".into());
            }
            workspace
        };

        // The page starts again with what its store kept between.
        let reopened = Replicas::new(certificate, Arc::clone(&store));
        if reopened.workspace(workspace).is_none() {
            return Err("the workspace is not held after a restart".into());
        }
        let (name, members) = reopened
            .members(workspace)
            .ok_or("its governance does not read")?;
        if members.map(|people| people.len()) != Some(1) {
            return Err(format!("{name} does not list its one person").into());
        }
        let admitted = reopened
            .with_workspace(workspace, |view| {
                Ok(view
                    .peer()
                    .governance()
                    .governed_policy()?
                    .admitted()
                    .count())
            })
            .ok_or("its admissions do not read")??;
        if admitted != 1 {
            return Err("the founder's admission is not held".into());
        }
        Ok(())
    }
}

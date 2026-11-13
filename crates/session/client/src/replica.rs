//! Each joined workspace as its governance admits it: the page holds its workspaces
//! in the same joined-workspace owner native devices hold theirs in
//! ([`tilo_peer_state::owner::NativeWorkspaceOwner`]), kept in the page's
//! storage through its [`PeerStore`].
//!
//! The host sends a subscriber the workspace's bootstrap ahead of its replay
//! ([`tilo_relay_protocol::Envelope::WorkspaceBootstrap`]): the workspace-join
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
//! member's write and names no channel participants. A page places its
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
    Change, NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, Store,
};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

use crate::custody::PeerStore;
use tilo_peer_state::document_frame::DocumentRoute;

/// The owner's record in the page's [`PeerStore`]. A record the store
/// refused leaves the owner fenced for the rest of the session, as a
/// native owner is after an uncertain write: the next session starts from
/// what the store holds.
struct PageWorkspaces {
    store: Arc<dyn PeerStore>,
    healthy: AtomicBool,
    /// The runs of custody this page keeps beside its record, by name: those the record it last
    /// kept names, and those put since. A store without runs of its own reads them from here.
    runs: std::sync::Mutex<BTreeMap<String, Option<Vec<u8>>>>,
}

impl Store for PageWorkspaces {
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn uncertain(&self, _room: WorkspaceId) -> bool {
        !self.healthy()
    }

    fn slot_workspace(&self, _slot: tilo_desktop_model::WorkspaceSlot) -> Option<WorkspaceId> {
        None
    }

    fn put_custody(&self, workspace: WorkspaceId, bytes: &[u8]) -> Result<[u8; 32], String> {
        let hash = *blake3::hash(bytes).as_bytes();
        let name = tilo_peer_state::owner::custody_run_name(workspace, hash);
        self.store.save_rooms_run(&name, Some(bytes))?;
        self.runs.lock_recover().insert(name, Some(bytes.to_vec()));
        Ok(hash)
    }

    fn read_custody(&self, workspace: WorkspaceId, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        let name = tilo_peer_state::owner::custody_run_name(workspace, hash);
        if let Some(Some(bytes)) = self.runs.lock_recover().get(&name) {
            return Ok(bytes.clone());
        }
        self.store
            .load_rooms_run(&name)
            .ok_or_else(|| format!("the run of custody {name} is not kept"))
    }

    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        _change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let bytes = serde_json::to_vec(next).map_err(|error| error.to_string())?;
        if let Err(error) = self.store.save_rooms(&bytes) {
            self.healthy.store(false, Ordering::Release);
            return Err(error);
        }
        install();
        // A run the record no longer names moved to custody's head.
        let named: BTreeSet<String> = next.custody_runs().collect();
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
        for name in gone {
            if let Err(error) = self.store.save_rooms_run(&name, None) {
                tracing::warn!(%error, "a run of custody the record no longer names was kept");
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
        let kept = store
            .load_rooms()
            .and_then(|bytes| serde_json::from_slice::<NativeAuthorityStorage>(&bytes).ok());
        let runs = kept
            .iter()
            .flat_map(NativeAuthorityStorage::custody_runs)
            .map(|name| (name, None))
            .collect();
        let workspaces = Arc::new(PageWorkspaces {
            store,
            healthy: AtomicBool::new(true),
            runs: std::sync::Mutex::new(runs),
        });
        let owner = kept
            .and_then(|kept| NativeWorkspaceOwner::restore(workspaces.clone(), kept, now).ok())
            .filter(|owner| {
                owner.storage().held().all(|(workspace, _)| {
                    owner
                        .with_current(workspace, |view| Ok(view.device() == &device))
                        .unwrap_or(false)
                })
            });
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
    /// a postcard membership answer); returns whether the replica is new.
    /// A replica of the same workspace and authority stays: a resumed
    /// subscription replays only what it has not received.
    ///
    /// # Errors
    /// The bootstrap is not a membership answer for this device and workspace,
    /// or the workspace it names does not build.
    pub(crate) fn bootstrap(
        &mut self,
        workspace_id: WorkspaceId,
        bytes: &[u8],
        now: SystemTime,
    ) -> Result<bool, String> {
        let installed = self
            .owner
            .install_vouched(workspace_id, bytes, &self.device, now)?;
        if self
            .owner
            .storage()
            .held()
            .any(|(workspace, _)| workspace == workspace_id)
        {
            self.workspaces.insert(workspace_id);
        }
        Ok(installed)
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
    ) -> Result<tilo_peer_state::AdmissionReport, String> {
        if !self.workspaces.contains(&workspace_id) {
            return Err("the workspace has no governance".into());
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
    ) -> Result<Vec<Result<tilo_peer_state::AdmissionReport, String>>, String> {
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
        self.workspaces.contains(&workspace_id).then_some(workspace_id)
    }

    /// `workspace_id`'s workspace and the members it lists by person id.
    pub(crate) fn members(&self, workspace_id: WorkspaceId) -> Option<(String, Option<BTreeSet<String>>)> {
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

    /// Whose each of `workspace_id`'s member devices is, as its governance admitted them: device
    /// (hex) to person (root key, hex), for every device a current member is here from. What
    /// shows a device by its person (who is here, a call's roster) reads it off this.
    pub(crate) fn member_devices(&self, workspace_id: WorkspaceId) -> Option<BTreeMap<String, String>> {
        self.with_workspace(workspace_id, |view| {
            let members = view
                .peer()
                .governance()
                .governed_members(tilo_clock::wall())?;
            Ok(members
                .into_iter()
                .filter(|member| !member.removed)
                .flat_map(|member| {
                    let person = hex::encode(member.user_public_key);
                    member
                        .active_device_keys
                        .into_iter()
                        .map(move |device| (hex::encode(device), person.clone()))
                })
                .collect())
        })?
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
    pub(crate) fn host(&self, workspace_id: WorkspaceId) -> Option<tilo_relay_client::Host> {
        let hint = self
            .with_workspace(workspace_id, |view| {
                let host = view.workspace_host().device_public_key;
                Ok(view
                    .peer()
                    .governance()
                    .workspace_hosts()
                    .into_iter()
                    .find(|hint| hint.host.device_public_key == host))
            })?
            .ok()??;
        let payload =
            tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1::from_transport_hint(
                &hint,
            )
            .ok()?;
        let relay = match payload.packet_relay_url.as_str() {
            "iroh:direct" => None,
            url => Some(url.parse().ok()?),
        };
        Some(tilo_relay_client::Host::through(
            payload.endpoint_addr,
            relay,
        ))
    }

    /// The devices that read the group carrying `route`'s keys, as the
    /// channels' directory names them; `None` while the workspace's
    /// governance does not answer for the route.
    pub(crate) fn participants(&self, route: &DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> {
        use tilo_channels::ChannelDirectory as _;
        let workspace = WorkspaceId::parse(&route.cluster_id)?;
        let directory = crate::channels::OwnerDirectory::reader(self.owner());
        let now = tilo_clock::unix_now();
        let named = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        let scope = directory.unit_scope(workspace, &named, now)?;
        let routed = (scope == named.channel(&route.cluster_id)).then_some(&named);
        directory.allowed_devices(workspace, scope, routed, now)
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
    TransportOrigin::RetainedRelayDelta(author)
}

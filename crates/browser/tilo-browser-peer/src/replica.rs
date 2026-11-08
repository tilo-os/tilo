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
//! ([`crate::PeerHandle::join_placed`]), so those have governance too.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collab_types::Ed25519PublicKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;
use tilo_desktop_model::projection::workspace_members;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{
    Change, NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, Store,
};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

use crate::channels::{Channels, RecordPolicy};
use crate::custody::PeerStore;
use tilo_peer_state::document_frame::DocumentRoute;

/// The owner's record in the page's [`PeerStore`]. A record the store
/// refused leaves the owner fenced for the rest of the session, as a
/// native owner is after an uncertain write: the next session starts from
/// what the store holds.
struct PageWorkspaces {
    store: Arc<dyn PeerStore>,
    healthy: AtomicBool,
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
        Ok(())
    }
}

/// The workspaces this page joined, by workspace.
pub(crate) struct Replicas {
    /// This device.
    device: DeviceCert,
    owner: NativeWorkspaceOwner,
    workspaces: BTreeMap<u64, WorkspaceId>,
}

impl Replicas {
    /// The workspaces of `device`, as `store` last kept them. A record that no
    /// longer restores is let go: the host's bootstraps and replays bring
    /// its workspaces back.
    pub(crate) fn new(device: DeviceCert, store: Arc<dyn PeerStore>) -> Self {
        let now = SystemTime::now();
        let kept = store
            .load_rooms()
            .and_then(|bytes| serde_json::from_slice::<NativeAuthorityStorage>(&bytes).ok());
        let workspaces = Arc::new(PageWorkspaces {
            store,
            healthy: AtomicBool::new(true),
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
            .map(|(workspace, held)| (held.0, workspace))
            .collect();
        Self {
            device,
            owner,
            workspaces,
        }
    }

    fn with_workspace<T>(
        &self,
        workspace_id: u64,
        read: impl FnOnce(&NativeCommittedView) -> Result<T, String>,
    ) -> Option<Result<T, String>> {
        let workspace = self.workspaces.get(&workspace_id)?;
        Some(self.owner.with_current(*workspace, read))
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
        workspace_id: u64,
        bytes: &[u8],
        now: SystemTime,
    ) -> Result<bool, String> {
        let installed = self
            .owner
            .install_vouched(workspace_id, bytes, &self.device, now)?;
        if let Some((workspace, _)) = self
            .owner
            .storage()
            .held()
            .find(|(_, workspace)| workspace.0 == workspace_id)
        {
            self.workspaces.insert(workspace_id, workspace);
        }
        Ok(installed)
    }

    /// Admit `message`, governance `author` sent to `workspace_id`;
    /// `retained` while the host replays the workspace. Returns whether it
    /// changed what the workspace's governance says.
    ///
    /// # Errors
    /// No replica of the workspace is held, or its governance refuses the
    /// message.
    pub(crate) fn admit(
        &self,
        workspace_id: u64,
        author: [u8; 32],
        retained: bool,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(workspace) = self.workspaces.get(&workspace_id) else {
            return Err("the workspace has no governance".into());
        };
        let report =
            self.owner
                .receive(*workspace, origin(author, retained), message, None, now)?;
        Ok(report.changed || report.metadata_changed)
    }

    /// Decide `message`, a document `author` wrote to `workspace_id`;
    /// `retained` while the host replays the workspace. `Ok(true)` lets it reach
    /// the page's apps, `Ok(false)` holds it until the workspace's governance
    /// can decide it.
    ///
    /// # Errors
    /// The workspace's governance refuses it.
    pub(crate) fn decide_document(
        &self,
        workspace_id: u64,
        author: [u8; 32],
        retained: bool,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<bool, String> {
        // Held until the workspace's bootstrap arrives.
        self.with_workspace(workspace_id, |view| {
            view.peer()
                .decide_relayed_document(origin(author, retained), message, now)
        })
        .unwrap_or(Ok(false))
    }

    /// `workspace_id`'s workspace and the members it lists by person id.
    pub(crate) fn members(&self, workspace_id: u64) -> Option<(String, Option<BTreeSet<String>>)> {
        self.with_workspace(workspace_id, |view| {
            let peer = view.peer();
            let doc = peer.admitted_workspace();
            Ok((
                peer.workspace_id().to_owned(),
                doc.and_then(workspace_members),
            ))
        })?
        .ok()
    }

    /// What the workspace's governance says now about `route`'s content: the
    /// group whose keys carry it and the devices that group admits. The group
    /// is the key scope the workspace's governance names for the route
    /// ([`tilo_peer_state::PeerGovernance::unit_scope`]), the decision a
    /// native device makes, and the devices are that group's readers, the host
    /// the workspace was joined through and this device. `None` while the
    /// workspace's governance does not answer for the route.
    pub(crate) fn policy(&self, route: &DocumentRoute) -> Option<RecordPolicy> {
        let workspace_id = route.workspace_id().ok()?;
        let unit = Channels::channel(route);
        let named = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        self.with_workspace(workspace_id, |view| {
            let peer = view.peer();
            let now = SystemTime::now();
            let governance = peer.governance();
            let scope = governance.unit_scope(now, &named)?;
            // A unit with readers of its own is carried by its own group,
            // whose record this page decides Read on; the workspace's scope is
            // read by the workspace's participants, and needs no record.
            let readers = governance
                .channel_readers(now, scope, (scope == unit).then_some(&named))?
                .ok_or_else(|| {
                    "the workspace's governance names no readers of the route".to_owned()
                })?;
            let mut allowed: BTreeSet<Ed25519PublicKey> = readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect();
            allowed.insert(Ed25519PublicKey::from_bytes(
                view.workspace_host().device_public_key,
            ));
            allowed.insert(Ed25519PublicKey::from_bytes(self.device.device_public_key));
            Ok(Some(RecordPolicy {
                unit,
                scope,
                allowed,
            }))
        })?
        .ok()
        .flatten()
    }

    /// Where `workspace_id`'s host listens, read off the signed hint the
    /// workspace's own governance keeps for it: the address a peer dials the
    /// host at, and the relay it is reached through. `None` until the host's
    /// bootstrap or a governance message named it.
    #[cfg(feature = "iroh")]
    pub(crate) fn host_endpoint(
        &self,
        workspace_id: u64,
    ) -> Option<(iroh_base::EndpointAddr, Option<iroh_base::RelayUrl>)> {
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
        Some((payload.endpoint_addr, relay))
    }

    /// The devices that may sign and be added to `route`'s group: those
    /// [`Self::policy`] names. `None` while the workspace's governance does
    /// not answer for the route.
    pub(crate) fn participants(&self, route: &DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.policy(route).map(|policy| policy.allowed)
    }

    /// One line per workspace for diagnostics: the host it was joined through.
    pub(crate) fn describe(&self) -> String {
        self.workspaces
            .keys()
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

fn origin(author: [u8; 32], retained: bool) -> TransportOrigin {
    if retained {
        TransportOrigin::RetainedRelayDelta(author)
    } else {
        TransportOrigin::RelayDelta(author)
    }
}

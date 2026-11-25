//! Workspaces installed by the native durable owner, over the direct connection.
//!
//! The link itself is `tilo_peer_state::owner::session`'s: this actor keeps one
//! route per enrolled workspace, holds it, and runs the link a hold reports as
//! its own task. What the link admits into the workspace's owner reaches this
//! device's apps through [`InternalEvent::NativeHeard`], and what the owner
//! publishes leaves through the same link, so a workspace's state has one path
//! between its devices and its host.
use super::{ActorState, Arc, HashMap, InternalEvent, NeighbourMsg};
use crate::WorkspaceId;
use crate::own_service::OwnService;
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::owner::session::CannotDial;
pub use tilo_peer_state::owner::session::{Connect, Input, Scope};
use tilo_peer_state::owner::session::{Holder, Link, Linked, OwnHost, Reach};
use tilo_workspace_join::TransportHint;

struct Route {
    connect: Connect,
    /// What the workspace's hold shares with this actor.
    link: Arc<Link>,
    hold: tokio::task::JoinHandle<()>,
    /// The workspace's host is this device's own: its link stays in process.
    local: bool,
}
/// A workspace's link is not up (yet): the caller may retry.
pub const NOT_CONNECTED: &str = "the workspace is not connected";

impl Drop for Route {
    fn drop(&mut self) {
        self.hold.abort();
    }
}
#[derive(Default)]
pub(super) struct Routes {
    entries: HashMap<super::WorkspaceId, Route>,
}
impl Routes {
    pub(super) fn contains(&self, ws: WorkspaceId) -> bool {
        self.entries.contains_key(&ws)
    }
    pub(super) fn remove(&mut self, ws: WorkspaceId) {
        if self.entries.remove(&ws).is_some() {
            crate::workspace_sync::forget(ws);
        }
    }
    /// Whether a route goes to a host on another machine: routes to this
    /// device's own host need no upkeep.
    pub(super) fn any_remote(&self) -> bool {
        self.entries.values().any(|route| !route.local)
    }
    /// Whether a link for the workspace on topic `ws` is being established:
    /// a dial, or a renewal of this device's membership ahead of one, has
    /// not reported back yet.
    pub(super) fn establishing(&self, ws: WorkspaceId) -> bool {
        self.entries
            .get(&ws)
            .is_some_and(|route| route.link.establishing())
    }
    /// Dial the workspace on topic `ws` again now.
    pub(super) fn retry(&self, ws: WorkspaceId) {
        if let Some(route) = self.entries.get(&ws) {
            route.link.retry();
        }
    }
    /// Every workspace this device hosts itself, as its routes now stand: their
    /// links join in process, and a join that has not happened yet is
    /// work this device's loop keeps, not nothing.
    pub(super) fn local(&self) -> impl Iterator<Item = super::WorkspaceId> + '_ {
        self.entries
            .iter()
            .filter(|(_, route)| route.local)
            .map(|(&ws, _)| ws)
    }
}

/// One workspace's link, held by this actor: each link up, and each dial that
/// failed, is answered to it.
struct NativeHolder {
    ws: WorkspaceId,
    endpoint: iroh::Endpoint,
    own_service: Option<OwnService>,
    tx: tokio::sync::mpsc::Sender<InternalEvent>,
}

impl Holder for NativeHolder {
    fn reach(&self) -> Reach {
        Reach {
            endpoint: self.endpoint.clone(),
            own_host: self
                .own_service
                .as_ref()
                .and_then(OwnService::get)
                .map(|host| host as Arc<dyn OwnHost>),
        }
    }

    /// Dials follow the heartbeat ([`ActorState::retry_native_routes`]).
    fn paces_itself(&self) -> bool {
        false
    }

    fn dial_failed(
        &self,
        via: TransportHint,
        reason: tilo_server_protocol::refusal::Refusal,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let _ = self
                .tx
                .send(InternalEvent::NativeDialFailed {
                    ws: self.ws,
                    via: Box::new(via),
                    reason,
                })
                .await;
        })
    }

    fn cannot_dial(
        &self,
        cause: CannotDial,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let _ = self
                .tx
                .send(InternalEvent::NativeCannotDial { ws: self.ws, cause })
                .await;
        })
    }

    fn run(
        &self,
        linked: Linked,
        scope: Arc<Scope>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let (held, ended) = tokio::sync::oneshot::channel();
            if self
                .tx
                .send(InternalEvent::NativeLinkConnected {
                    ws: self.ws,
                    linked: Box::new(linked),
                    scope,
                    held,
                })
                .await
                .is_ok()
            {
                // Over once the actor lets the link go.
                let _ = ended.await;
            }
        })
    }
}

/// What one link to a workspace's host has sent of this device's ownership of it: each frame
/// leaves once per link, oldest first, and a new link starts with a new one.
#[derive(Default)]
struct Sending {
    sent: std::collections::BTreeSet<[u8; 32]>,
}

impl Sending {
    /// Send each frame of `workspace`'s this device wrote that this link has not sent, through
    /// `deliver`; `took` is told of each the host keeps no answer for. Whether the link took
    /// everything.
    ///
    /// # Errors
    ///
    /// The ownership record cannot be written after a frame went.
    fn drive(
        &mut self,
        owner: &tilo_peer_state::owner::DeviceWorkspaces,
        workspace: WorkspaceId,
        mut took: impl FnMut(&[u8]) -> Result<(), String>,
        mut deliver: impl FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        let frames = owner.frames(workspace);
        let queued: std::collections::BTreeSet<[u8; 32]> = frames
            .iter()
            .map(|bytes| *blake3::hash(bytes).as_bytes())
            .collect();
        self.sent.retain(|id| queued.contains(id));
        for bytes in &frames {
            let Ok(message) = NeighbourMsg::decode(bytes) else {
                continue;
            };
            // What no channel and retention name is not this device's to publish on the link.
            if neighbour_wire::retention(&message, false).is_none() {
                continue;
            }
            let id = *blake3::hash(bytes).as_bytes();
            if !self.sent.insert(id) {
                continue;
            }
            if !deliver(bytes) {
                self.sent.remove(&id);
                return Ok(false);
            }
            // A document write stays in ownership until its host answers it; anything else
            // leaves the record once a link carried it.
            if !matches!(message, NeighbourMsg::AppDocument { .. })
                && let Err(error) = took(bytes)
            {
                return Err(error);
            }
        }
        Ok(true)
    }
}

/// A workspace's link as this actor keeps it: the device's end of the
/// direct connection, the pin it was admitted under, and the task running it.
pub(super) struct NativeLink {
    pub scope: Arc<Scope>,
    pub link: Arc<DirectSync>,
    /// What this link sent of the device's ownership of the workspace.
    sending: Sending,
    /// Held while the workspace's hold runs this link; let go with it.
    held: Option<tokio::sync::oneshot::Sender<()>>,
    pump: tokio::task::JoinHandle<()>,
}

impl Drop for NativeLink {
    fn drop(&mut self) {
        // The workspace's hold dials again once its link is let go.
        drop(self.held.take());
        self.pump.abort();
    }
}

/// How a message this device publishes into a workspace it takes part in over
/// its host's link travels there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Carried {
    /// Over the link itself, as this device's own: a sealed write, which the
    /// workspace's host never admits and keeps for the record's
    /// channel members, the workspace's channel governance, which the host
    /// takes in and sends on to the other members, and an ask for a record's
    /// content, which the host answers or hands to the member keeping it.
    Link,
    /// In the workspace's own publication: a cleartext write, which the
    /// device's engine published already and the link publishes in order.
    Publication,
    /// Nowhere: the link syncs the workspace's state by itself.
    Nowhere,
}

/// How `msg` travels in a workspace this device takes part in over its
/// host's link; `None` for what goes on the workspace's live session instead.
pub(super) fn carried(msg: &NeighbourMsg) -> Option<Carried> {
    match msg {
        NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some() => {
            Some(Carried::Link)
        }
        _ if is_governance(msg) || is_record_content(msg) => Some(Carried::Link),
        NeighbourMsg::AppDocument { .. } => Some(Carried::Publication),
        NeighbourMsg::SyncReq { .. } | NeighbourMsg::SyncResp { .. } => Some(Carried::Nowhere),
        _ => None,
    }
}

/// Whether `msg` is a workspace's channel governance: a frame of a channel's
/// group, a device asking to join one, or the host's word that one has none.
pub(super) fn is_governance(msg: &NeighbourMsg) -> bool {
    msg.governance_workspace().is_some()
}

/// Whether `msg` is about a record's content: a member asking the workspace
/// for the bytes a record names, or a device answering with the pieces it keeps
/// or with word that it keeps none. It is live traffic between the workspace's
/// members, never their state, and travels as such.
pub(super) fn is_record_content(msg: &NeighbourMsg) -> bool {
    matches!(
        msg,
        NeighbourMsg::DocumentContentWants { .. }
            | NeighbourMsg::DocumentContentPieces { .. }
            | NeighbourMsg::DocumentContentLacks { .. }
            | NeighbourMsg::DocumentContentRefused { .. }
            | NeighbourMsg::DocumentContentKept { .. }
            | NeighbourMsg::DocumentContentReleased { .. }
    )
}

/// The message event `heard`, a frame a workspace's link brought, is for this
/// device's apps when it is about a record's content: an ask or an answer
/// reaches them as the same frame sent on the workspace's topic does, from the
/// device that authored it. `None` for everything else, which is the
/// workspace's state the link admitted.
pub(super) fn content_message(
    ws: WorkspaceId,
    heard: &tilo_peer_state::direct::Heard,
) -> Option<super::NetEvent> {
    if !is_record_content(&heard.message) {
        return None;
    }
    let peer = iroh::EndpointId::from_bytes(&heard.author).ok()?;
    Some(super::NetEvent::Msg {
        ws,
        peer,
        membership_epoch: None,
        replayed: heard.retained,
        msg: heard.message.clone(),
    })
}

impl ActorState {
    /// Send `msg` to the workspace's host over the link up now
    /// ([`Carried::Link`]). A sealed write is kept at the link's store, and
    /// the next link the workspace has sends it again; channel governance
    /// with no link up waits for the next one, which sends what this device
    /// remembers of it as it connects.
    pub(super) fn publish_over_link(&self, ws: WorkspaceId, msg: &NeighbourMsg) {
        if let Some(link) = self.native_links.get(&ws)
            && let Err(error) = link.link.send(msg.clone())
        {
            tracing::debug!(ws = %ws, %error, "kept for the workspace's next link");
        }
        if !is_governance(msg) {
            self.tell_unsynced(ws);
        }
    }

    /// Tell the workspace on topic `ws`'s apps how many of this device's writes
    /// to it are not yet synced ([`crate::workspace_sync::unsynced`]): the
    /// committed writes in its owner's outgoing ownership, all of them while no
    /// link of the workspace carries them. A write sent over a link up now is
    /// on its way, and says nothing.
    pub(super) fn tell_unsynced(&self, ws: WorkspaceId) {
        let Some(route) = self.native_routes.entries.get(&ws) else {
            return;
        };
        let carried = self.native_links.contains_key(&ws);
        let unsynced = if carried {
            0
        } else {
            route
                .connect
                .owner()
                .frames(route.connect.workspace())
                .len()
        };
        crate::workspace_sync::unsynced(ws, unsynced);
    }

    /// A frame the workspace on topic `ws`'s link admitted: the workspace's
    /// state is in, and this device's apps take their part of it.
    pub(super) async fn native_heard(
        &self,
        ws: WorkspaceId,
        heard: tilo_peer_state::direct::Heard,
    ) {
        let Some(link) = self.native_links.get(&ws) else {
            return;
        };
        // A record's content is the workspace's members' own traffic, never its
        // state: an ask or an answer reaches this device's apps as one sent on
        // the workspace's topic does, so the content service keeping what its
        // records name hears it.
        if is_record_content(&heard.message) {
            self.deliver_content(ws, &heard).await;
            return;
        }
        self.deliver(ws, Arc::clone(&link.scope), heard).await;
    }

    /// Hand `heard`, a frame of the workspace on topic `ws` about a record's
    /// content, to this device's apps as the message its author sent, as one
    /// arriving on the workspace's topic is: the content service keeps the
    /// ask or answer, and the workspace's state keeps none of it.
    async fn deliver_content(&self, ws: WorkspaceId, heard: &tilo_peer_state::direct::Heard) {
        let Some(message) = content_message(ws, heard) else {
            return;
        };
        self.tally.note(heard.message.kind());
        self.publish_to_subscriber(ws, message).await;
    }

    /// Hand `heard`, a frame of the workspace on topic `ws` that this
    /// device's owner took in under `scope`, to this device's apps.
    async fn deliver(
        &self,
        ws: WorkspaceId,
        scope: Arc<Scope>,
        heard: tilo_peer_state::direct::Heard,
    ) {
        let input = Input::heard(scope, heard);
        self.tally.note(super::heard_kind(&input));
        self.publish_to_subscriber(ws, super::NetEvent::NativeInput(Box::new(input)))
            .await;
    }

    /// The workspace on topic `ws`'s link has its host's retained history in
    /// hand: the workspace holds what its host holds.
    pub(super) async fn native_ready(&self, ws: WorkspaceId) {
        crate::workspace_sync::synced(ws);
        let Some(link) = self.native_links.get(&ws) else {
            return;
        };
        let Ok((payload, _)) =
            WorkspaceDeviceHintV1::authenticate_transport_hint(link.scope.host())
        else {
            return;
        };
        let peer = payload.endpoint_addr.id;
        self.publish_to_subscriber(
            ws,
            super::NetEvent::HostReady {
                ws,
                peer,
                // This host keeps the workspace's records on its link's own session, which
                // replays them all to this device as its history starts: nothing of a record
                // waits for the record to be opened here.
                documents_when_opened: false,
            },
        )
        .await;
    }

    /// The workspace on topic `ws`'s link ended: its host said its owner
    /// `deleted` it, or the next heartbeat dials it again.
    pub(super) async fn native_link_closed(&mut self, ws: WorkspaceId, deleted: bool) {
        if deleted {
            tracing::info!(ws = %ws, "a workspace's owner deleted it; nothing dials it again");
            // Its hold goes first, so letting go of the link dials nothing.
            self.native_routes.remove(ws);
            self.native_links.remove(&ws);
            self.publish_to_subscriber(ws, super::NetEvent::WorkspaceDeleted { ws })
                .await;
            return;
        }
        tracing::info!(
            ws = %ws,
            "a workspace's link ended; the next heartbeat dials it again"
        );
        self.native_links.remove(&ws);
        // The workspace's hold dials again at once: no heartbeat runs for a
        // workspace this device's own host serves.
        self.native_routes.retry(ws);
        // What this device holds for the workspace is not on its way now.
        self.tell_unsynced(ws);
    }

    /// Publish what the workspace `ws`'s owner holds for its other devices over
    /// the link it holds now.
    /// Retirement commits to disk, so it runs off this actor.
    /// Send this device's ownership of the workspace on topic `ws` over its link up now, as far as
    /// the link takes it, then retire what the workspace receipted or refused.
    pub(super) fn drive_native_publication(&mut self, ws: WorkspaceId) {
        if let (Some(link), Some(route), Some(identity)) = (
            self.native_links.get_mut(&ws),
            self.native_routes.entries.get(&ws),
            &self.host_identity,
        ) {
            let (owner, workspace) = (route.connect.owner().clone(), route.connect.workspace());
            let sent = link.scope.current(identity).and_then(|()| {
                link.sending.drive(
                    &owner,
                    workspace,
                    |bytes| owner.sent(workspace, bytes),
                    |bytes| {
                        NeighbourMsg::decode(bytes)
                            .map_err(|error| error.to_string())
                            .and_then(|message| link.link.send(message))
                            .inspect_err(|error| tracing::debug!(%error, "native publication remains in durable ownership"))
                            .is_ok()
                    },
                )
            });
            if let Err(error) = sent {
                tracing::debug!(%error, "native publication remains in durable ownership");
            }
            // A frame this device wrote left its record as the link took it; nothing waits to be
            // retired when the other end answers.
        } else {
            tracing::debug!(
                ws = %ws,
                linked = self.native_links.contains_key(&ws),
                routed = self.native_routes.entries.contains_key(&ws),
                "this device's ownership of a workspace waits: it holds no link to send it on"
            );
        }
        self.tell_unsynced(ws);
    }
    /// The device workspace `ws`'s link reaches answered this device's writes `answers`: a
    /// refusal is the other end's, and says what it could not take; what went is already out of
    /// this device's record.
    pub(super) fn native_answered(
        &self,
        ws: WorkspaceId,
        answers: &[tilo_peer_state::direct::Answered],
    ) {
        for answered in answers {
            if matches!(answered.kept, tilo_peer_state::direct::Kept::Refused { .. }) {
                tracing::warn!(ws = %ws, write = ?answered.write, "a peer refused this device's write");
            }
        }
        if !answers.is_empty() {
            let _ = self
                .internal_tx
                .try_send(InternalEvent::NativeRetired { ws });
        }
    }

    pub(super) fn native_connect(&mut self, connect: Connect) {
        let ws = connect.workspace();
        if !self.native_routes.contains(ws) && self.native_routes.entries.len() >= 64 {
            return;
        }
        // A route that is dialling, or whose link is up, is the one this
        // workspace takes part through: it needs no second dial.
        if self.native_routes.contains(ws)
            && (self.native_routes.establishing(ws) || self.native_links.contains_key(&ws))
        {
            return;
        }
        // A link that was up carried nothing of what this one carries: the
        // route below dials its own.
        self.native_links.remove(&ws);
        // A workspace pinned to this device, on a device that runs no host of its
        // own, has no host to reach: its apps' empty states are their own
        // from the start, as in a workspace that syncs with no host. With a
        // host here, its link joins that host in process.
        if self.own_service.is_none()
            && self
                .host_identity
                .as_ref()
                .is_some_and(|identity| connect.hosted_here(identity))
        {
            self.native_routes.remove(ws);
            crate::workspace_sync::forget(ws);
            return;
        }
        // Apps in the workspace say they are connecting until its first sync.
        crate::workspace_sync::connecting(ws);
        let link = Arc::new(Link::default());
        // The hold's first dial starts on its own task: until it reports
        // back, the workspace has a link being established, not none.
        link.dialling();
        let holder = NativeHolder {
            ws,
            endpoint: self.endpoint.clone(),
            own_service: self.own_service.clone(),
            tx: self.internal_tx.clone(),
        };
        let hold = tokio::spawn({
            let (connect, link) = (connect.clone(), Arc::clone(&link));
            let identity = self.host_identity.clone();
            async move {
                tilo_peer_state::owner::session::hold(connect, identity, &holder, &link).await;
            }
        });
        self.native_routes.entries.insert(
            ws,
            Route {
                connect,
                link,
                hold,
                local: false,
            },
        );
        self.tell_unsynced(ws);
        self.retry_native_routes();
    }
    /// Let go of each workspace's link no longer pinned to its material, and
    /// have every workspace's hold dial again: the heartbeat paces the dials.
    pub(super) fn retry_native_routes(&mut self) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        let unhosted_here: Vec<WorkspaceId> = self
            .native_routes
            .entries
            .iter()
            .filter(|_| self.own_service.is_none())
            .filter(|(_, route)| route.connect.hosted_here(identity))
            .map(|(&ws, _)| ws)
            .collect();
        for ws in unhosted_here {
            // It became pinned to this device, which runs no host, since it
            // was connected (the identity arrived after it): nothing to dial.
            self.native_routes.remove(ws);
            crate::workspace_sync::forget(ws);
        }
        for (&ws, route) in &mut self.native_routes.entries {
            let material = route.connect.material(identity);
            if let Err(error) = &material {
                tracing::debug!(%error, ws = %ws, "native workspace ownership unusable for its host");
            }
            route.local = material.as_ref().is_ok_and(|(hint, cert, ..)| {
                tilo_peer_state::owner::session::hosted_by(hint, &cert.device_public_key)
            });
            let hosts = route
                .connect
                .holders(&identity.device_cert.device_public_key)
                .unwrap_or_default();
            drop_unpinned(&mut self.native_links, ws, &material, &hosts);
            route.link.retry();
        }
    }

    /// A dial of `via`, one of the workspace's holders, failed.
    pub(super) async fn native_dial_failed(
        &mut self,
        ws: WorkspaceId,
        via: &TransportHint,
        reason: &tilo_server_protocol::refusal::Refusal,
    ) {
        self.tally.note("NativeHostDial");
        if !self.native_routes.contains(ws) {
            return;
        }
        // A holder that said the workspace's owner deleted it has nothing for
        // this device any more, however the dial found it out: the workspace
        // leaves as it does when the news comes over a link that was up.
        if *reason == tilo_server_protocol::refusal::Refusal::WorkspaceDeleted {
            self.native_link_closed(ws, true).await;
            return;
        }
        // The host did not answer: until a link with a host of the workspace
        // completes, its apps say it cannot be reached and work on what this
        // device has. The route dials again on the next heartbeat.
        // Where the host was dialled, as its hint names it: a hint signed before the host knew
        // its server names only the sockets it had then.
        let at = WorkspaceDeviceHintV1::authenticate_transport_hint(via)
            .map(|(payload, _)| format!("{:?}", payload.endpoint_addr))
            .unwrap_or_default();
        tracing::debug!(ws = %ws, host = %hex::encode(via.host.device_public_key), %at, %reason, "native host dial failed; retrying");
        crate::workspace_sync::unreachable(ws);
    }

    /// The workspace on topic `ws`'s hold dials none of its holders, for
    /// `cause`: its apps say what the workspace holds cannot arrive, and why.
    /// A workspace placed on this device waits for this device's own service
    /// while that is still opening: only a service that failed to open is a
    /// failure.
    pub(super) fn native_cannot_dial(&self, ws: WorkspaceId, cause: CannotDial) {
        use ui::workspace_sync::WorkspaceUnavailable;
        if !self.native_routes.contains(ws) {
            return;
        }
        let unavailable = match cause {
            CannotDial::NoIdentity => WorkspaceUnavailable::NoIdentity,
            CannotDial::NotRenewed => WorkspaceUnavailable::NotRenewed,
            CannotDial::HoldersUnreadable => WorkspaceUnavailable::HoldersUnreadable,
            CannotDial::NoHolders => WorkspaceUnavailable::NoHolders,
            CannotDial::OwnServiceStopped => {
                if !self.own_service.as_ref().is_none_or(OwnService::failed) {
                    return;
                }
                WorkspaceUnavailable::OwnServiceStopped
            }
        };
        tracing::debug!(ws = %ws, ?cause, "the workspace's holders cannot be dialled");
        crate::workspace_sync::unavailable(ws, unavailable);
    }

    /// The person asked to dial the holders of the workspace on topic `ws`
    /// again now: its apps wait for the dial again, and its hold dials.
    pub(super) fn redial_native_route(&mut self, ws: WorkspaceId) {
        if !self.native_routes.contains(ws) {
            return;
        }
        crate::workspace_sync::retrying(ws);
        self.retry_native_routes();
        // Without an identity the routes are not retried as a whole; this
        // one still hears the retry and says how it stands.
        self.native_routes.retry(ws);
    }

    /// The workspace on topic `ws`'s hold reached a host: its link runs here
    /// until `held` is let go with it.
    pub(super) fn native_link_connected(
        &mut self,
        ws: WorkspaceId,
        linked: Linked,
        scope: Arc<Scope>,
        held: tokio::sync::oneshot::Sender<()>,
    ) {
        self.tally.note("NativeHostDial");
        let Some(route) = self.native_routes.entries.get(&ws) else {
            return;
        };
        let local = route.local;
        // The host is reachable again: a certificate renewal that failed
        // meanwhile tries again.
        tilo_identity::provisioning::RENEWAL_CAUSES.changed();
        // An earlier link of the workspace, with whichever holder, is let go.
        self.native_links.remove(&ws);
        let scope_host = scope.host().host.device_public_key;
        let link = Arc::new(linked.link.clone());
        let pump =
            super::spawn_direct_pump(ws, linked, Arc::clone(&link), self.internal_tx.clone());
        // What this device said while the link was down: its channel governance, which its writer
        // publishes again on the link.
        let backlog: Vec<(u8, Vec<u8>)> = self
            .governance
            .get(&ws)
            .into_iter()
            .flatten()
            .map(|(_, bytes)| (0, bytes.clone()))
            .collect();
        for (_, bytes) in backlog {
            if let Ok(msg) = NeighbourMsg::decode(&bytes)
                && let Err(error) = link.send(msg)
            {
                tracing::warn!(ws = %ws, %error, "held state not sent to the workspace's link");
            }
        }
        self.native_links.insert(
            ws,
            NativeLink {
                scope,
                link,
                sending: Sending::default(),
                held: Some(held),
                pump,
            },
        );
        if local {
            // A workspace this device hosts needs nothing of its own host's
            // live session: it is the same process.
            tracing::debug!(ws = %ws, "joined the workspace's own host in process");
        } else {
            // The other half of "a workspace's link ended": from here on the
            // workspace's governance and records reach this device again.
            let host = hex::encode(scope_host);
            tracing::debug!(ws = %ws, %host, "a workspace's link is up");
        }
        self.drive_native_publication(ws);
    }
}

/// Drop topic `ws`'s link when it is not pinned to its workspace's current
/// `material`, or reaches a host the workspace no longer names among its
/// `hosts`.
fn drop_unpinned(
    links: &mut HashMap<super::WorkspaceId, NativeLink>,
    ws: WorkspaceId,
    material: &Result<(TransportHint, super::DeviceCert, u64), String>,
    hosts: &[TransportHint],
) {
    let unpinned = links.get(&ws).is_some_and(|link| {
        !material
            .as_ref()
            .is_ok_and(|(hint, device, continuity)| link.scope.pinned_to(hint, device, *continuity))
            || !hosts.contains(link.scope.host())
    });
    if unpinned {
        links.remove(&ws);
    }
}
#[cfg(test)]
mod tests {
    use super::{Carried, NeighbourMsg, WorkspaceId, carried};
    use collab_types::{MlsAccessRequestV1, MlsGovernanceAction, MlsGovernanceFrame};

    /// A host asks a linked member for the surface a change names; the member's
    /// session hears it as a member's message, which its content service keeps,
    /// never as workspace state, which the owner has no use for.
    #[test]
    fn a_record_contents_ask_heard_on_a_link_reaches_the_session_as_a_message() {
        let device = ed25519_dalek::SigningKey::from_bytes(&[5; 32])
            .verifying_key()
            .to_bytes();
        let want = NeighbourMsg::DocumentContentWants {
            requester: 9,
            cluster_id: "workspace".into(),
            wants: vec![neighbour_wire::content::Wanted {
                app_id: "so.tilo.chat".into(),
                hash: "ab".repeat(32),
                have: Vec::new(),
            }],
        };
        let heard = |message: NeighbourMsg| tilo_peer_state::direct::Heard {
            author: device,
            message,
            landed: false,
            retained: false,
            report: Err(tilo_peer_state::AdmissionError::malformed(
                "the workspace keeps no state of it",
            )),
            fresh: true,
        };
        let Some(super::super::NetEvent::Msg {
            ws,
            peer,
            replayed,
            msg,
            ..
        }) = super::content_message(WorkspaceId::from_u128(7), &heard(want.clone()))
        else {
            panic!("a record's content heard on a link is a message");
        };
        assert_eq!((ws, replayed), (WorkspaceId::from_u128(7), false));
        assert_eq!(peer.as_bytes(), &device);
        assert_eq!(msg, want);
        assert!(
            super::content_message(
                WorkspaceId::from_u128(7),
                &heard(NeighbourMsg::SyncResp {
                    changes: Vec::new(),
                    workspace: "workspace".into(),
                })
            )
            .is_none(),
            "the workspace's state is what the link admitted, not a message"
        );
    }

    /// A workspace a device takes part in over its host's link has no other
    /// path to its host: channel governance kept off the link left every
    /// device founding the channel's group on its own, with keys no other
    /// member held.
    #[test]
    fn channel_governance_travels_over_the_workspaces_link() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let channel = collab_types::WorkspaceId::new();
        let genesis = NeighbourMsg::MlsGovernance {
            workspace: "workspace".into(),
            frame: Box::new(
                MlsGovernanceFrame::signed(channel, [0; 32], 0, MlsGovernanceAction::Genesis, &key)
                    .unwrap(),
            ),
            delivery: 0,
        };
        let request = || Box::new(MlsAccessRequestV1::signed(channel, vec![1], 1, &key).unwrap());
        let asking = NeighbourMsg::MlsAccessRequest {
            workspace: "workspace".into(),
            request: request(),
            delivery: 0,
        };
        // A member's ask for a record's content reaches its host the same way:
        // the host answers it, or hands it to the member keeping it.
        let want = NeighbourMsg::DocumentContentWants {
            requester: 9,
            cluster_id: "workspace".into(),
            wants: vec![neighbour_wire::content::Wanted {
                app_id: "so.tilo.notes".into(),
                hash: "ab".repeat(32),
                have: Vec::new(),
            }],
        };
        for message in [genesis, asking, want] {
            assert_eq!(carried(&message), Some(Carried::Link), "{}", message.kind());
        }
    }
}

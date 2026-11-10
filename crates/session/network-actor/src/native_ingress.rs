//! Workspaces installed by the native durable owner, over the direct lane.
//!
//! The link itself is `tilo_peer_state::owner::session`'s: this actor keeps one
//! route per enrolled workspace, holds it, and runs the link a hold reports as
//! its own task. What the link admits into the workspace's owner reaches this
//! device's apps through [`InternalEvent::NativeHeard`], and what the owner
//! publishes leaves through the same link, so a workspace's state has one path
//! between its devices and its host.
use super::{ActorState, Arc, HashMap, InternalEvent, NeighbourMsg};
use crate::local_host::LocalHost;
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::join::TransportHint;
pub use tilo_peer_state::owner::session::{Connect, Input, Scope};
use tilo_peer_state::owner::session::{Holder, Link, Linked, OwnHost, Reach};

struct Route {
    connect: Connect,
    /// What the workspace's hold shares with this actor.
    link: Arc<Link>,
    hold: tokio::task::JoinHandle<()>,
    /// The workspace's host is this device's own: its link stays in process.
    local: bool,
}
/// A governed workspace's link is not up (yet): the caller may retry.
pub const NOT_CONNECTED: &str = "the governed workspace is not connected";

impl Drop for Route {
    fn drop(&mut self) {
        self.hold.abort();
    }
}
#[derive(Default)]
pub(super) struct Routes {
    entries: HashMap<u64, Route>,
}
impl Routes {
    pub(super) fn contains(&self, ws: u64) -> bool {
        self.entries.contains_key(&ws)
    }
    pub(super) fn remove(&mut self, ws: u64) {
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
    pub(super) fn establishing(&self, ws: u64) -> bool {
        self.entries
            .get(&ws)
            .is_some_and(|route| route.link.establishing())
    }
    /// Dial the workspace on topic `ws` again now.
    pub(super) fn retry(&self, ws: u64) {
        if let Some(route) = self.entries.get(&ws) {
            route.link.retry();
        }
    }
    /// Every workspace this device hosts itself, as its routes now stand: their
    /// links join in process, and a join that has not happened yet is
    /// work this device's loop keeps, not nothing.
    pub(super) fn local(&self) -> impl Iterator<Item = u64> + '_ {
        self.entries
            .iter()
            .filter(|(_, route)| route.local)
            .map(|(&ws, _)| ws)
    }
}

/// One workspace's link, held by this actor: each link up, and each dial that
/// failed, is answered to it.
struct NativeHolder {
    ws: u64,
    endpoint: iroh::Endpoint,
    local_host: Option<LocalHost>,
    tx: tokio::sync::mpsc::Sender<InternalEvent>,
}

impl Holder for NativeHolder {
    fn reach(&self) -> Reach {
        Reach {
            endpoint: self.endpoint.clone(),
            own_host: self
                .local_host
                .as_ref()
                .and_then(LocalHost::get)
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
        governor: bool,
        reason: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let _ = self
                .tx
                .send(InternalEvent::NativeDialFailed {
                    ws: self.ws,
                    via: Box::new(via),
                    governor,
                    reason,
                })
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

/// A workspace's link as this actor keeps it: the device's end of the
/// direct lane, the pin it was admitted under, and the task running it.
pub(super) struct NativeLink {
    pub scope: Arc<Scope>,
    pub link: Arc<DirectSync>,
    /// Held while the workspace's hold runs this link; let go with it.
    held: Option<tokio::sync::oneshot::Sender<()>>,
    pump: tokio::task::JoinHandle<()>,
}

impl Drop for NativeLink {
    fn drop(&mut self) {
        self.scope.close();
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
    /// workspace's governor never admits and its host keeps for the record's
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
/// host's link; `None` for what goes on the workspace's live plane instead.
pub(super) fn carried(msg: &NeighbourMsg) -> Option<Carried> {
    match msg {
        NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some() => {
            Some(Carried::Link)
        }
        _ if is_channel_governance(msg) || is_record_content(msg) => Some(Carried::Link),
        NeighbourMsg::AppDocument { .. } => Some(Carried::Publication),
        NeighbourMsg::SyncReq { .. } | NeighbourMsg::SyncResp { .. } => Some(Carried::Nowhere),
        _ => None,
    }
}

/// Whether `msg` is a workspace's channel governance: a frame of a channel's
/// group, or a device asking to join one.
pub(super) fn is_channel_governance(msg: &NeighbourMsg) -> bool {
    matches!(
        msg,
        NeighbourMsg::MlsGovernance { .. }
            | NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::MlsRoutedAccessRequest { .. }
    )
}

/// Whether `msg` is about a record's content: a member asking the workspace
/// for the bytes a record names, or a device answering with the pieces it keeps
/// or with word that it keeps none. It is live traffic between the workspace's
/// members, never their state, and travels as such.
pub(super) fn is_record_content(msg: &NeighbourMsg) -> bool {
    matches!(
        msg,
        NeighbourMsg::RecordContentWant { .. }
            | NeighbourMsg::RecordContentPiece { .. }
            | NeighbourMsg::RecordContentLacks { .. }
    )
}

/// The message event `heard`, a frame a workspace's link brought, is for this
/// device's apps when it is about a record's content: an ask or an answer
/// reaches them as the same frame sent on the workspace's topic does, from the
/// device that authored it. `None` for everything else, which is the
/// workspace's state the link admitted.
pub(super) fn content_message(
    ws: u64,
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
    pub(super) fn publish_over_link(&self, ws: u64, msg: &NeighbourMsg) {
        if let Some(link) = self.native_links.get(&ws)
            && let Err(error) = link.link.send(msg.clone())
        {
            tracing::debug!(ws, %error, "kept for the workspace's next link");
        }
        if !is_channel_governance(msg) {
            self.tell_unsynced(ws);
        }
    }

    /// Tell the workspace on topic `ws`'s apps how many of this device's writes
    /// to it are not yet synced ([`crate::workspace_sync::unsynced`]): the
    /// committed writes in its owner's outgoing custody, all of them while no
    /// link of the workspace carries them. A write sent over a link up now is
    /// on its way, and says nothing.
    pub(super) fn tell_unsynced(&self, ws: u64) {
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
                .unadmitted_writes(route.connect.workspace())
                .unwrap_or(0)
        };
        crate::workspace_sync::unsynced(ws, unsynced);
    }

    /// A frame the workspace on topic `ws`'s link admitted: the workspace's
    /// state is in, and this device's apps take their part of it.
    pub(super) async fn native_heard(&self, ws: u64, heard: tilo_peer_state::direct::Heard) {
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
        // The devices that take part through this one hear the rest of the
        // workspace through it alone. Its channels' governance goes to them
        // with the rest of what the channel service hears.
        if let Some(members) = &self.members
            && heard.report.is_ok()
            && !is_channel_governance(&heard.message)
        {
            members.carry(link.scope.workspace(), heard.author, heard.message.clone());
        }
        self.deliver(ws, heard).await;
    }

    /// Hand `heard`, a frame of the workspace on topic `ws` about a record's
    /// content, to this device's apps as the message its author sent, as one
    /// arriving on the workspace's topic is: the content service keeps the
    /// ask or answer, and the workspace's state keeps none of it.
    async fn deliver_content(&self, ws: u64, heard: &tilo_peer_state::direct::Heard) {
        let Some(message) = content_message(ws, heard) else {
            return;
        };
        self.tally.note(heard.message.kind());
        self.publish_to_subscriber(ws, message).await;
    }

    /// Hand `heard`, a frame of the workspace on topic `ws` that this
    /// device's owner took in, to this device's apps.
    async fn deliver(&self, ws: u64, heard: tilo_peer_state::direct::Heard) {
        let Some(link) = self.native_links.get(&ws) else {
            return;
        };
        let input = Input::heard(Arc::clone(&link.scope), heard);
        self.tally.note(super::heard_kind(&input));
        self.publish_to_subscriber(ws, super::NetEvent::NativeInput(Box::new(input)))
            .await;
    }

    /// Relay for `members`, the devices that take part in this device's
    /// workspaces through it: what their links bring reaches
    /// [`Self::member_heard`].
    pub(super) fn serve_members(&mut self, members: tilo_peer_state::direct::Owning) {
        let (tell, mut told) = tokio::sync::mpsc::unbounded_channel();
        members.hear(Arc::new(move |workspace, heard| {
            let _ = tell.send((workspace, heard.clone()));
        }));
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            while let Some((workspace, heard)) = told.recv().await {
                let ws = crate::workspace_topic_id(workspace);
                if tx
                    .send(InternalEvent::MemberHeard { ws, heard })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        self.members = Some(members);
    }

    /// A frame a device that takes part in the workspace on topic `ws`
    /// through this one wrote, which this device's owner took in: it reaches
    /// this device's apps, and the workspace's own host over this device's
    /// link, so the devices that take part there read it too. Its link
    /// already sent it to this device's other members. The channels'
    /// governance takes its own path, through the channel service.
    pub(super) async fn member_heard(&self, ws: u64, heard: tilo_peer_state::direct::Heard) {
        if (heard.report.is_err() && !is_record_content(&heard.message))
            || is_channel_governance(&heard.message)
        {
            return;
        }
        let Some(link) = self.native_links.get(&ws) else {
            tracing::debug!(
                ws,
                "a member's frame reaches no host and no app: this device holds no link of the workspace"
            );
            return;
        };
        // A record's content a member asks for or answers reaches the workspace's
        // host over this device's link as its writes do.
        let content = is_record_content(&heard.message);
        if (content || matches!(heard.message, NeighbourMsg::AppDocument { .. }))
            && let Err(error) = link.link.send(heard.message.clone())
        {
            tracing::debug!(ws, %error, "a member's write waits for the workspace's next link");
        }
        if content {
            self.deliver_content(ws, &heard).await;
        } else {
            self.deliver(ws, heard).await;
        }
    }

    /// Send `msg`, a write this device publishes, to the devices that take
    /// part in its workspace through this one: they read the workspace
    /// through this device alone.
    pub(super) fn carry_own(&self, msg: &NeighbourMsg) {
        let (Some(members), Some(identity)) = (&self.members, &self.host_identity) else {
            return;
        };
        let NeighbourMsg::AppDocument { cluster_id, .. } = msg else {
            return;
        };
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(cluster_id) else {
            return;
        };
        members.carry(workspace, identity.device_public_key(), msg.clone());
    }

    /// The workspace on topic `ws`'s link has its host's retained history in
    /// hand: the workspace holds what its host holds.
    pub(super) async fn native_ready(&self, ws: u64) {
        crate::workspace_sync::synced(ws);
        let Some(link) = self.native_links.get(&ws) else {
            return;
        };
        let Ok((payload, _)) =
            IrohWorkspaceHostHintV1::authenticate_transport_hint(link.scope.host())
        else {
            return;
        };
        let peer = payload.endpoint_addr.id;
        self.publish_to_subscriber(ws, super::NetEvent::HostReady { ws, peer })
            .await;
    }

    /// The workspace on topic `ws`'s link ended.
    pub(super) fn native_link_closed(&mut self, ws: u64) {
        tracing::info!(
            ws,
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
    pub(super) fn drive_native_publication(&self, ws: u64) {
        if let (Some(link), Some(identity)) = (self.native_links.get(&ws), &self.host_identity)
            && let Err(error) = link.scope.publish(&link.link, identity)
        {
            tracing::debug!(%error,"native publication remains in durable custody");
        }
        self.tell_unsynced(ws);
    }
    pub(super) fn native_connect(&mut self, connect: Connect) {
        let ws = crate::workspace_topic_id(connect.workspace());
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
        if self.local_host.is_none()
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
            local_host: self.local_host.clone(),
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
        let unhosted_here: Vec<u64> = self
            .native_routes
            .entries
            .iter()
            .filter(|_| self.local_host.is_none())
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
                tracing::debug!(%error, ws, "native workspace custody unusable for its host");
            }
            route.local = material.as_ref().is_ok_and(|(hint, cert, ..)| {
                tilo_peer_state::owner::session::hosted_by(hint, &cert.device_public_key)
            });
            let hosts = route
                .connect
                .hosts(&identity.device_cert.device_public_key)
                .unwrap_or_default();
            drop_unpinned(&mut self.native_links, ws, &material, &hosts);
            route.link.retry();
        }
    }

    /// A dial of `via`, the workspace's governor or another of its hosts, failed.
    pub(super) fn native_dial_failed(
        &self,
        ws: u64,
        via: &TransportHint,
        governor: bool,
        reason: &str,
    ) {
        self.tally.note(if governor {
            "NativeGovernorDial"
        } else {
            "NativeHostDial"
        });
        if !self.native_routes.contains(ws) {
            return;
        }
        // The host did not answer: until a link with a host of the workspace
        // completes, its apps say it cannot be reached and work on what this
        // device has. The route dials again on the next heartbeat.
        tracing::debug!(ws, host = %hex::encode(via.host.device_public_key), %reason, "native host dial failed; retrying");
        crate::workspace_sync::unreachable(ws);
    }

    /// The workspace on topic `ws`'s hold reached a host: its link runs here
    /// until `held` is let go with it.
    pub(super) fn native_link_connected(
        &mut self,
        ws: u64,
        linked: Linked,
        scope: Arc<Scope>,
        held: tokio::sync::oneshot::Sender<()>,
    ) {
        let governor = scope.reaches_governor();
        self.tally.note(if governor {
            "NativeGovernorDial"
        } else {
            "NativeHostDial"
        });
        let Some(route) = self.native_routes.entries.get(&ws) else {
            return;
        };
        let local = route.local;
        // The host is reachable again: a certificate renewal that failed
        // meanwhile tries again.
        tilo_identity_host::provisioning::RENEWAL_CAUSES.changed();
        // A link that stood in for this one is let go: a host that held the
        // workspace while its governor was away.
        self.native_links.remove(&ws);
        let link = Arc::new(linked.link.clone());
        let pump = super::spawn_direct_pump(
            ws,
            linked,
            Arc::clone(&link),
            scope.clone(),
            self.internal_tx.clone(),
        );
        // What this device said while the link was down: its channel
        // governance (its writer publishes it again), and the held state of
        // its workspace.
        let backlog: Vec<(u8, Vec<u8>)> = self
            .governance
            .get(&ws)
            .into_iter()
            .flatten()
            .map(|(_, bytes)| (0, bytes.clone()))
            .chain(
                self.host_backlog
                    .remove(&ws)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, _, bytes)| (1, bytes)),
            )
            .collect();
        for (_, bytes) in backlog {
            if let Ok(msg) = postcard::from_bytes(&bytes)
                && let Err(error) = link.send(msg)
            {
                tracing::warn!(ws, %error, "held state not sent to the workspace's link");
            }
        }
        self.native_links.insert(
            ws,
            NativeLink {
                scope,
                link,
                held: Some(held),
                pump,
            },
        );
        if local {
            // A workspace this device hosts needs nothing of its own host's
            // live plane: it is the same process.
            tracing::debug!(ws, "joined the workspace's own host in process");
        }
        self.drive_native_publication(ws);
    }
}

/// Drop topic `ws`'s link when it is not pinned to its workspace's current
/// `material`, or reaches a host the workspace no longer names among its
/// `hosts`.
fn drop_unpinned(
    links: &mut HashMap<u64, NativeLink>,
    ws: u64,
    material: &Result<(TransportHint, super::DeviceCert, u64), String>,
    hosts: &[TransportHint],
) {
    let unpinned = links.get(&ws).is_some_and(|link| {
        !material
            .as_ref()
            .is_ok_and(|(hint, device, continuity)| link.scope.pinned_to(hint, device, *continuity))
            || !(link.scope.reaches_governor() || hosts.contains(link.scope.host()))
    });
    if unpinned {
        links.remove(&ws);
    }
}
#[cfg(test)]
mod tests {
    use super::{Carried, NeighbourMsg, carried};
    use collab_types::{ChannelId, MlsAccessRequestV1, MlsGovernanceAction, MlsGovernanceFrame};

    /// A host asks a linked member for the surface a change names; the member's
    /// session hears it as a member's message, which its content service keeps,
    /// never as workspace state, which the owner has no use for.
    #[test]
    fn a_record_contents_ask_heard_on_a_link_reaches_the_session_as_a_message() {
        let device = ed25519_dalek::SigningKey::from_bytes(&[5; 32])
            .verifying_key()
            .to_bytes();
        let want = NeighbourMsg::RecordContentWant {
            requester: 9,
            app_id: "so.tilo.chat".into(),
            cluster_id: "workspace".into(),
            hash: "ab".repeat(32),
        };
        let heard = |message: NeighbourMsg| tilo_peer_state::direct::Heard {
            author: device,
            message,
            landed: false,
            retained: false,
            report: Err("the workspace keeps no state of it".into()),
            fresh: true,
        };
        let Some(super::super::NetEvent::Msg {
            ws,
            peer,
            replayed,
            msg,
            ..
        }) = super::content_message(7, &heard(want.clone()))
        else {
            panic!("a record's content heard on a link is a message");
        };
        assert_eq!((ws, replayed), (7, false));
        assert_eq!(peer.as_bytes(), &device);
        assert_eq!(msg, want);
        assert!(
            super::content_message(
                7,
                &heard(NeighbourMsg::SyncResp {
                    changes: Vec::new(),
                    room: "workspace".into(),
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
        let channel = ChannelId::new();
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
        let routed = NeighbourMsg::MlsRoutedAccessRequest {
            room: "workspace".into(),
            request: request(),
            route: Box::new(collab_types::ChannelRoute::new("app", "record")),
            delivery: 0,
        };
        // A member's ask for a record's content reaches its host the same way:
        // the host answers it, or hands it to the member keeping it.
        let want = NeighbourMsg::RecordContentWant {
            requester: 9,
            app_id: "so.tilo.notes".into(),
            cluster_id: "workspace".into(),
            hash: "ab".repeat(32),
        };
        for message in [genesis, asking, routed, want] {
            assert_eq!(carried(&message), Some(Carried::Link), "{}", message.kind());
        }
    }
}

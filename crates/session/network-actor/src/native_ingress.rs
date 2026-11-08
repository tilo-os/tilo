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

impl ActorState {
    /// Send `msg`, a member's own write, to the workspace's host as its link
    /// carries it: the link keeps it at its store, and the next link the
    /// workspace has sends it again.
    pub(super) fn publish_sealed_native(&mut self, ws: u64, msg: &NeighbourMsg) {
        if let Some(link) = self.native_links.get(&ws)
            && let Err(error) = link.link.send(msg.clone())
        {
            tracing::debug!(ws, %error, "sealed write kept for the workspace's next link");
        }
        self.tell_unsynced(ws);
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
    pub(super) async fn native_heard(&mut self, ws: u64, heard: tilo_peer_state::direct::Heard) {
        let Some(link) = self.native_links.get(&ws) else {
            return;
        };
        let Some(input) = Input::heard(Arc::clone(&link.scope), heard) else {
            return;
        };
        self.tally.note(super::heard_kind(&input));
        self.publish_to_subscriber(ws, super::NetEvent::NativeInput(input))
            .await;
    }

    /// The workspace on topic `ws`'s link has its host's retained history in
    /// hand: the workspace holds what its host holds.
    pub(super) async fn native_ready(&mut self, ws: u64) {
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

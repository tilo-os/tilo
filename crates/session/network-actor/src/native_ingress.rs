//! Closed relay provenance for rooms installed by the native durable owner.
//!
//! The session itself is `tilo_peer_state::owner::session`'s; this actor keeps one
//! route per enrolled room, dials it again when its pin changes and runs
//! the connected session's pump beside every other host session. A room
//! hosted on this device is not dialled: its session goes to this device's
//! own host in process ([`crate::local_host`]), through the same
//! handshake and session code.
use super::{
    ActorState, Arc, DeviceCert, HashMap, HostRuntime, InternalEvent, RelayCoeditSession,
    spawn_host_pump,
};
use crate::local_host::LocalHost;
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_peer_state::join::TransportHint;
pub use tilo_peer_state::owner::session::{Connect, Input, Scope};
use tilo_peer_state::owner::session::{DialError, Holder, Link, OwnHost, Reach};

struct Route {
    connect: Connect,
    /// What the room's hold shares with this actor.
    link: Arc<Link>,
    hold: tokio::task::JoinHandle<()>,
    /// The room's host is this device's own: its session stays in process.
    local: bool,
}
/// A governed room's relay session is not up (yet): the caller may retry.
pub const NOT_CONNECTED: &str = "the governed room is not connected";

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
            crate::room_sync::forget(ws);
        }
    }
    /// Whether a route goes to a host on another machine: routes to this
    /// device's own host keep no relay up and need no upkeep.
    pub(super) fn any_remote(&self) -> bool {
        self.entries.values().any(|route| !route.local)
    }
    /// Whether a session for the room on topic `ws` is being established:
    /// a dial, or a renewal of this device's membership ahead of one, has
    /// not reported back yet.
    pub(super) fn establishing(&self, ws: u64) -> bool {
        self.entries
            .get(&ws)
            .is_some_and(|route| route.link.establishing())
    }
    /// Dial the room on topic `ws` again now.
    pub(super) fn retry(&self, ws: u64) {
        if let Some(route) = self.entries.get(&ws) {
            route.link.retry();
        }
    }
}

/// One room's session, held for this actor: each session up, and each
/// dial that failed, is answered to it.
struct NativeHolder {
    ws: u64,
    endpoint: iroh::Endpoint,
    https: Option<super::HttpsRoute>,
    local_host: Option<LocalHost>,
    tx: tokio::sync::mpsc::Sender<InternalEvent>,
}

impl Holder for NativeHolder {
    fn reach(&self) -> Reach {
        Reach {
            endpoint: self.endpoint.clone(),
            https: self
                .https
                .as_ref()
                .map(|super::HttpsRoute(http)| Arc::clone(http)),
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
        error: DialError,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let _ = self
                .tx
                .send(InternalEvent::NativeDialFailed {
                    ws: self.ws,
                    via: Box::new(via),
                    governor,
                    error,
                })
                .await;
        })
    }

    fn run(
        &self,
        session: RelayCoeditSession,
        scope: Arc<Scope>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let (held, ended) = tokio::sync::oneshot::channel();
            if self
                .tx
                .send(InternalEvent::NativeHostConnected {
                    ws: self.ws,
                    session: Box::new(session),
                    scope,
                    held,
                })
                .await
                .is_ok()
            {
                // Over once the actor lets the session go.
                let _ = ended.await;
            }
        })
    }
}

/// The connected relay session of the joined room on topic `ws`.
pub(super) fn native_session(
    sessions: &HashMap<(u64, iroh::EndpointId), HostRuntime>,
    ws: u64,
) -> Option<Arc<RelayCoeditSession>> {
    sessions.iter().find_map(|((topic, _), runtime)| {
        (*topic == ws && runtime.native_scope.is_some()).then(|| runtime.session.clone())
    })
}

impl ActorState {
    /// Admit authenticated governance through the same durable owner before
    /// replayed app state can stall the compositor's event drain. This actor
    /// awaits one blocking admission at a time; the live scope still
    /// checks current custody, revocation and its exact session pin.
    pub(super) async fn admit_native_governance(
        &self,
        ws: u64,
        event: super::HostEvent,
    ) -> super::HostEvent {
        let input = match &event {
            super::HostEvent::NativeInput(input)
                if matches!(
                    input.message(),
                    neighbour_wire::NeighbourMsg::SyncResp { .. }
                ) =>
            {
                input.clone()
            }
            _ => return event,
        };
        let Some(identity) = self.host_identity.clone() else {
            return event;
        };
        let room = input.room();
        match tokio::task::spawn_blocking(move || {
            let before = input.generation().ok();
            input.admit(&identity)?;
            Ok::<_, String>(before != input.generation().ok())
        })
        .await
        {
            Ok(Ok(changed)) => {
                self.tally.note(if changed {
                    "NativeGovernanceChanged"
                } else {
                    "NativeGovernanceUnchanged"
                });
                if changed {
                    self.drive_native_publication(ws);
                    if let Some(wake) = &self.wake {
                        wake();
                    }
                }
            }
            Ok(Err(error)) => {
                self.tally.note("NativeGovernanceRefused");
                tracing::warn!(%error, %room, "native relay governance admission denied");
            }
            Err(error) => {
                tracing::warn!(%error, %room, "native relay governance admission worker failed");
            }
        }
        event
    }
    /// Every native scope joined for the room on topic `ws`, with its relay
    /// session and this host's identity. Without an identity there are none.
    fn native_scopes(
        &self,
        ws: u64,
    ) -> impl Iterator<Item = (&Scope, &RelayCoeditSession, &super::HostIdentity)> {
        let identity = self.host_identity.as_ref();
        self.host_sessions
            .iter()
            .filter(move |((topic, _), _)| *topic == ws)
            .filter_map(move |(_, runtime)| {
                Some((
                    runtime.native_scope.as_deref()?,
                    &*runtime.session,
                    identity?,
                ))
            })
    }
    /// A refresh only asks the original subscribed host to run its own genuine
    /// checkpoint barrier. It grants no local admission and retires no outbox.
    pub(super) fn request_native_refresh(&self, ws: u64) {
        for (scope, session, identity) in self.native_scopes(ws) {
            if let Err(error) = scope.refresh(session, identity) {
                tracing::debug!(%error, "native checkpoint refresh blocked");
            }
        }
    }
    /// Send `msg`, a member's own sealed write, to the room's host as a
    /// commit, and hold it until a host keeps it ([`super::sealed_outbox`]).
    pub(super) fn publish_sealed_native(&mut self, ws: u64, msg: &super::NeighbourMsg) {
        let (Some((channel, retention)), Ok(bytes)) =
            (super::retention(msg, false), postcard::to_allocvec(msg))
        else {
            return;
        };
        // Held until a host keeps it: a session up now may already be gone
        // without this device knowing, and the next one sends it again.
        let (op, payload, record) = self
            .sealed_outbox
            .entry(ws)
            .or_default()
            .hold(channel, retention, bytes);
        // In the room's log too: a device shut down before a host kept it
        // still holds it when it starts again.
        if let (Some(record), Some(route)) = (record, self.native_routes.entries.get(&ws)) {
            self.sealed_records.hold(ws, route.connect.room(), record);
        }
        if let Some(session) = native_session(&self.host_sessions, ws)
            && let Err(error) = session.commit(op, &payload)
        {
            tracing::debug!(ws, %error, "sealed write held for the room's next session");
        }
        self.tell_unsynced(ws);
    }

    /// A host of the room on topic `ws` keeps this device's sealed write
    /// `op`: it is let go, here and in the room's log.
    pub(super) fn sealed_kept(&mut self, ws: u64, op: [u8; 16]) {
        let Some(outbox) = self.sealed_outbox.get_mut(&ws) else {
            return;
        };
        if !outbox.kept(op) {
            return;
        }
        let empty = outbox.is_empty();
        if empty {
            self.sealed_outbox.remove(&ws);
        }
        if let Some(route) = self.native_routes.entries.get(&ws) {
            self.sealed_records
                .kept(ws, route.connect.room(), op, empty);
        }
        self.tell_unsynced(ws);
    }

    /// A host of the room on topic `ws` did not keep this device's sealed
    /// write `op`: it stays held, not yet synced, until the room's next
    /// session sends it again.
    pub(super) fn sealed_refused(&mut self, ws: u64, op: [u8; 16]) {
        if self
            .sealed_outbox
            .get_mut(&ws)
            .is_some_and(|outbox| outbox.refused(op))
        {
            self.tell_unsynced(ws);
        }
    }

    /// Tell the room on topic `ws`'s apps how many of this device's writes
    /// to it are not yet synced ([`crate::room_sync::unsynced`]):
    /// the committed writes in its owner's outgoing custody and the sealed
    /// writes held for its hosts, all of them while no session of the room
    /// carries them, else the sealed writes its host did not keep. A write
    /// sent on a session up now is on its way, and says nothing.
    pub(super) fn tell_unsynced(&self, ws: u64) {
        let Some(route) = self.native_routes.entries.get(&ws) else {
            return;
        };
        let carried = native_session(&self.host_sessions, ws).is_some();
        let sealed = self
            .sealed_outbox
            .get(&ws)
            .map_or(0, |outbox| outbox.unsynced(carried));
        let committed = if carried {
            0
        } else {
            route
                .connect
                .owner()
                .unadmitted_writes(route.connect.room())
                .unwrap_or(0)
        };
        if crate::room_sync::unsynced(ws, sealed.saturating_add(committed))
            && let Some(wake) = &self.wake
        {
            wake();
        }
    }

    pub(super) fn drive_native_publication(&self, ws: u64) {
        for (scope, session, identity) in self.native_scopes(ws) {
            if let Err(error) = scope.publish(session, identity) {
                tracing::debug!(%error,"native publication remains in durable custody");
            }
        }
        self.tell_unsynced(ws);
    }
    pub(super) fn native_connect(&mut self, connect: Connect, https: Option<super::HttpsRoute>) {
        let ws = crate::room_topic_id(connect.room());
        if !self.native_routes.contains(ws) && self.native_routes.entries.len() >= 64 {
            return;
        }
        self.host_sessions.retain(|(topic, _), _| *topic != ws);
        // A room pinned to this device, on a device that runs no host of its
        // own, has no host to reach: its apps' empty states are their own
        // from the start, as in a workspace that syncs with no host. With a
        // host here, its session joins that host in process.
        if self.local_host.is_none()
            && self
                .host_identity
                .as_ref()
                .is_some_and(|identity| connect.hosted_here(identity))
        {
            self.native_routes.remove(ws);
            crate::room_sync::forget(ws);
            return;
        }
        // Apps in the room say they are connecting until its first sync.
        crate::room_sync::connecting(ws);
        // The sealed writes the room's log held when this device started:
        // its next session sends them.
        match self
            .held_log
            .as_ref()
            .map(|log| log.read(connect.room()))
            .transpose()
        {
            Ok(Some(held)) if !held.is_empty() => {
                self.sealed_outbox.entry(ws).or_default().restore(held);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(ws, %error, "the room's held sealed writes cannot be read");
            }
        }
        let link = Arc::new(Link::default());
        // The hold's first dial starts on its own task: until it reports
        // back, the room has a session being established, not none.
        link.dialling();
        let holder = NativeHolder {
            ws,
            endpoint: self.endpoint.clone(),
            https,
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
    /// Let go of each room's sessions no longer pinned to its material, and
    /// have every room's hold dial again: the heartbeat paces the dials.
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
            crate::room_sync::forget(ws);
        }
        for (&ws, route) in &mut self.native_routes.entries {
            let material = route.connect.material(identity);
            if let Err(error) = &material {
                tracing::debug!(%error, ws, "native room custody unusable for its host");
            }
            route.local = material.as_ref().is_ok_and(|(hint, cert, ..)| {
                tilo_peer_state::owner::session::hosted_by(hint, &cert.device_public_key)
            });
            let hosts = route
                .connect
                .hosts(&identity.device_cert.device_public_key)
                .unwrap_or_default();
            drop_unpinned_sessions(&mut self.host_sessions, ws, &material, &hosts);
            route.link.retry();
        }
    }

    /// A dial of `via`, the room's governor or another of its hosts, failed.
    pub(super) fn native_dial_failed(
        &self,
        ws: u64,
        via: &TransportHint,
        governor: bool,
        error: DialError,
    ) {
        self.tally.note(if governor {
            "NativeGovernorDial"
        } else {
            "NativeHostDial"
        });
        if !self.native_routes.contains(ws) {
            return;
        }
        let Ok((payload, _)) = IrohWorkspaceHostHintV1::authenticate_transport_hint(via) else {
            return;
        };
        dial_refused(&self.subscribers, ws, payload.endpoint_addr.id, error);
    }

    /// The room on topic `ws`'s hold reached a host: its session runs here
    /// until `held` is let go with it.
    pub(super) fn native_host_connected(
        &mut self,
        ws: u64,
        session: RelayCoeditSession,
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
        let Ok((payload, _)) = IrohWorkspaceHostHintV1::authenticate_transport_hint(scope.host())
        else {
            return;
        };
        let host = payload.endpoint_addr.id;
        if governor {
            // The governor is back: the host that stood in for it is let go.
            self.host_sessions.retain(|(topic, _), runtime| {
                let stays = *topic != ws;
                if !stays {
                    runtime.pump.abort();
                }
                stays
            });
        } else if self.host_sessions.keys().any(|(topic, _)| *topic == ws) {
            return;
        }
        let session = Arc::new(session);
        let replaying = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pump = spawn_host_pump(
            ws,
            host,
            session.clone(),
            Arc::clone(&replaying),
            self.internal_tx.clone(),
            Some(scope.clone()),
            !local,
        );
        // What this device said while the session was down: its channel
        // governance (its writer publishes it again), and every sealed
        // write no host has kept yet, sent before or meanwhile.
        for (channel, bytes) in self.governance.get(&ws).into_iter().flatten() {
            if let Err(error) = session.publish_checkpoint(*channel, bytes.clone()) {
                tracing::warn!(ws, %error, "channel governance not sent to the room's relay");
            }
        }
        for (channel, retention, bytes) in self.host_backlog.remove(&ws).unwrap_or_default() {
            let sent = match retention {
                super::Retention::Checkpoint => session.publish_checkpoint(channel, bytes),
                super::Retention::Delta => session.publish_retained_delta(channel, bytes),
            };
            if let Err(error) = sent {
                tracing::warn!(ws, %error, "held state not sent to the room's relay");
            }
        }
        if let Some(outbox) = self.sealed_outbox.get_mut(&ws) {
            for (op, payload) in outbox.commits() {
                if let Err(error) = session.commit(op, &payload) {
                    tracing::debug!(ws, %error, "sealed write held for the room's next session");
                }
            }
            outbox.sent_again();
        }
        // Pointers, cursor chat and reactions ride the room's live plane,
        // which carries only what its members say: join it, as a workspace
        // host's session does.
        if let Err(error) = session.live_join() {
            tracing::warn!(ws, %error, "could not join the room's live plane");
        }
        self.native_media.reconnected(ws, &session);
        self.reopen_records(ws, &session);
        self.host_sessions.insert(
            (ws, host),
            HostRuntime {
                replaying,
                actions: None,
                session,
                pump,
                native_scope: Some(scope),
                native_held: Some(held),
                local,
            },
        );
        self.drive_native_publication(ws);
    }
}

/// Drop topic `ws`'s host sessions not pinned to its room's current
/// `material`, or reaching a host the room no longer names among its
/// `hosts`.
fn drop_unpinned_sessions(
    sessions: &mut HashMap<(u64, iroh::EndpointId), HostRuntime>,
    ws: u64,
    material: &Result<
        (
            TransportHint,
            DeviceCert,
            tilo_user_identity::RevocationList,
            u64,
        ),
        String,
    >,
    hosts: &[TransportHint],
) {
    sessions.retain(|(topic, _), runtime| {
        *topic != ws
            || runtime.native_scope.as_ref().is_some_and(|scope| {
                material
                    .as_ref()
                    .is_ok_and(|(hint, device, _, continuity)| {
                        scope.pinned_to(hint, device, *continuity)
                    })
                    && (scope.reaches_governor() || hosts.contains(scope.host()))
            })
    });
}

/// What a failed dial of the joined room on topic `ws`, whose host is
/// `host`, tells the person and the shell.
fn dial_refused(
    subscribers: &HashMap<u64, tokio::sync::mpsc::Sender<super::NetEvent>>,
    ws: u64,
    host: iroh::EndpointId,
    error: DialError,
) {
    match error {
        // The room's host speaks another peer protocol version: the room
        // cannot sync until one side updates, which the person is told,
        // rather than holding a room that silently stays empty.
        DialError::PeerVersion { theirs } => {
            tracing::warn!(
                ws,
                theirs,
                ours = neighbour_wire::PEER_PROTOCOL_VERSION,
                "the room's host speaks another peer protocol version"
            );
            crate::services::peer_version(host.to_string(), String::new(), theirs);
            crate::room_sync::other_version(ws, theirs);
        }
        // The room removed this device's person: dialing again changes
        // nothing until they join again, which the shell does through the
        // room's door where it has one.
        DialError::NotAdmitted => {
            tracing::warn!(ws, "the room's host admits this device's person no longer");
            if let Some(subscriber) = subscribers.get(&ws) {
                let _ = subscriber.try_send(super::NetEvent::NotAdmitted { ws });
            }
        }
        // Anything else did not reach the room's host: until a sync with
        // it completes, its apps say it cannot be reached and work on what
        // this device has. The route dials again on the next heartbeat.
        error => {
            tracing::debug!(%error, ws, "native host dial failed; retrying");
            crate::room_sync::unreachable(ws);
        }
    }
}

/// What a room host's session carries for the collaboration broker and no
/// owner admits, taken by this actor straight to the broker's drain in the
/// order the host said it. It reaches the broker without the frame loop,
/// which an app opening a record may be holding while it waits for it.
pub enum ChannelTraffic {
    /// A member's sealed write to a record of a room whose records are
    /// sealed channels.
    Sealed(Input),
    /// The room's host on topic `ws` finished replaying what it retains:
    /// every sealed write it replayed is queued ahead of this.
    Replayed(super::WorkspaceId),
}

/// The queue [`ChannelTraffic`] waits in for the broker's drain.
pub struct ChannelQueue {
    traffic: std::sync::Mutex<std::collections::VecDeque<ChannelTraffic>>,
    wake: crate::Wake,
}

impl ChannelQueue {
    /// A queue whose drain `wake` rouses.
    pub fn new(wake: crate::Wake) -> Arc<Self> {
        Arc::new(Self {
            traffic: std::sync::Mutex::default(),
            wake,
        })
    }

    /// Queue `traffic` and wake the drain.
    pub fn push(&self, traffic: ChannelTraffic) {
        if let Ok(mut queued) = self.traffic.lock() {
            queued.push_back(traffic);
        }
        (self.wake)();
    }

    /// Everything queued, oldest first.
    pub fn take(&self) -> std::collections::VecDeque<ChannelTraffic> {
        self.traffic
            .lock()
            .map(|mut queued| std::mem::take(&mut *queued))
            .unwrap_or_default()
    }
}

impl std::fmt::Debug for ChannelQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelQueue").finish_non_exhaustive()
    }
}

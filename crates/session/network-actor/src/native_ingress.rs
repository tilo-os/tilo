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

struct Route {
    connect: Connect,
    /// The host's HTTPS routes, for a host that publishes no iroh address.
    https: Option<super::HttpsRoute>,
    serial: u64,
    pending: Option<tokio::task::JoinHandle<()>>,
    /// Dials in a row the host closed without answering this device's
    /// hello: a refusal, or a host from before versioned handshakes. Past
    /// [`tilo_peer_state::owner::session::UNANSWERED_BEFORE_PROBE`] the next one
    /// probes which.
    unanswered: u32,
    /// The room's host is this device's own: its session stays in process.
    local: bool,
}
/// A governed room's relay session is not up (yet): the caller may retry.
pub const NOT_CONNECTED: &str = "the governed room is not connected";

impl Drop for Route {
    fn drop(&mut self) {
        if let Some(task) = &self.pending {
            task.abort();
        }
    }
}
#[derive(Default)]
pub(super) struct Routes {
    entries: HashMap<u64, Route>,
    serial: u64,
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
            .is_some_and(|route| route.pending.is_some())
    }
    /// Whether the room on topic `ws` is hosted on this device.
    pub(super) fn is_local(&self, ws: u64) -> bool {
        self.entries.get(&ws).is_some_and(|route| route.local)
    }
}

/// Prove this device's membership of `connect`'s room with its renewed
/// certificate ([`tilo_peer_state::owner::enroll::renew_membership`]), over the
/// route its sessions take; the next heartbeat dials with it.
/// A route whose custody no longer matches its room: when the device's
/// certificate was renewed, it proves its membership with the renewal and
/// dials once the room holds it.
fn renew_route(
    route: &mut Route,
    endpoint: &iroh::Endpoint,
    identity: &super::HostIdentity,
    local_host: Option<&LocalHost>,
) {
    let renewed = matches!(route.connect.renewal(identity), Ok(Some(_)));
    if renewed
        && route
            .pending
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    {
        return;
    }
    if let Some(task) = route.pending.take() {
        task.abort();
    }
    if renewed {
        route.pending = renew(
            route.connect.clone(),
            route.https.clone(),
            endpoint.clone(),
            identity.clone(),
            local_host,
        );
    }
}

fn renew(
    connect: Connect,
    https: Option<super::HttpsRoute>,
    endpoint: iroh::Endpoint,
    identity: super::HostIdentity,
    local_host: Option<&LocalHost>,
) -> Option<tokio::task::JoinHandle<()>> {
    let room = connect.room();
    // A room hosted here renews with this device's own host, in process.
    let own = identity.device_cert.device_public_key;
    let hosted_here = connect
        .owner()
        .with_current(room, |view| {
            Ok(tilo_peer_state::owner::session::hosted_by(
                view.transport_hint()?,
                &own,
            ))
        })
        .unwrap_or(false);
    let client = match (https, hosted_here) {
        (_, true) => local_host?.join_client(own)?,
        (Some(super::HttpsRoute(http)), false) => {
            tilo_relay_session::workspace_join::WorkspaceJoinClient::https(http, None)
        }
        (None, false) => tilo_relay_session::workspace_join::WorkspaceJoinClient::new(endpoint),
    };
    Some(tokio::spawn(async move {
        match tilo_peer_state::owner::enroll::renew_membership(
            client,
            identity,
            connect.owner().clone(),
            room,
        )
        .await
        {
            Ok(true) => tracing::info!(%room, "renewed this device's certificate in the room"),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%room, %error, "this device's renewed certificate was not taken");
            }
        }
    }))
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
    /// Whether this device's own host serves the room on topic `ws`, on the
    /// route it has or the material it would join by: its session then stays
    /// in process, and nothing it commits is carried over the network.
    fn host_governs_here(&self, ws: u64) -> bool {
        let Some(identity) = self.host_identity.as_ref() else {
            return false;
        };
        self.native_routes.entries.get(&ws).is_some_and(|route| {
            route
                .connect
                .material(identity)
                .is_ok_and(|(hint, cert, ..)| {
                    tilo_peer_state::owner::session::hosted_by(&hint, &cert.device_public_key)
                })
        })
    }
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
    /// Send `request` to the governor of the governed room on topic `ws`,
    /// opening the room's private lane on first use.
    pub(super) fn governor_action(
        &mut self,
        ws: u64,
        room: uuid::Uuid,
        request: Box<super::governor_lane::Request>,
    ) {
        let Some(runtime) = self
            .host_sessions
            .iter_mut()
            .find_map(|((topic, _), runtime)| {
                (*topic == ws
                    && runtime
                        .native_scope
                        .as_ref()
                        .is_some_and(|scope| scope.reaches_governor()))
                .then_some(runtime)
            })
        else {
            let _ = request.reply.send(Err(NOT_CONNECTED.into()));
            return;
        };
        runtime
            .governor
            .get_or_insert_with(|| super::governor_lane::Lane::open(runtime.session.clone(), room))
            .send(*request);
    }

    pub(super) fn native_connect(&mut self, connect: Connect, https: Option<super::HttpsRoute>) {
        let ws = crate::room_topic_id(connect.room());
        if !self.native_routes.contains(ws) && self.native_routes.entries.len() >= 64 {
            return;
        }
        self.host_sessions.retain(|(topic, _), _| *topic != ws);
        let Some(serial) = self.native_routes.serial.checked_add(1) else {
            return;
        };
        self.native_routes.serial = serial;
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
        self.native_routes.entries.insert(
            ws,
            Route {
                connect,
                https,
                serial: self.native_routes.serial,
                pending: None,
                unanswered: 0,
                local: false,
            },
        );
        self.tell_unsynced(ws);
        self.retry_native_routes();
    }
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
            if material.is_err() {
                self.host_sessions.retain(|(topic, _), _| *topic != ws);
                renew_route(route, &self.endpoint, identity, self.local_host.as_ref());
                continue;
            }
            let hosts = route
                .connect
                .hosts(&identity.device_cert.device_public_key)
                .unwrap_or_default();
            // A session with another host of the room stands in for the
            // governor's until the governor's own is up again: it is dialled
            // here, at the heartbeat, like any session that ended.
            if keeps_pinned_session(&mut self.host_sessions, ws, &material, &hosts)
                || route
                    .pending
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
            {
                continue;
            }
            let Ok((hint, cert, revocations, _)) = material else {
                continue;
            };
            route.local =
                tilo_peer_state::owner::session::hosted_by(&hint, &cert.device_public_key);
            let key = identity.device_signing_key.clone();
            let member = Member {
                cert,
                key,
                revocations,
            };
            if route.local {
                // This device's own host serves the room: its session stays
                // in process.
                route.pending = join_local(
                    self.local_host.as_ref(),
                    ws,
                    route.serial,
                    hint,
                    member,
                    self.internal_tx.clone(),
                );
                continue;
            }
            // Over iroh a host from before versioned handshakes closes the
            // stream on this device's hello without a word, as a refusal
            // does: after a few, ask it the way such a host reads.
            let probe = route.https.is_none()
                && route.unanswered >= tilo_peer_state::owner::session::UNANSWERED_BEFORE_PROBE;
            route.pending = Some(spawn_dial(Dial {
                ws,
                serial: route.serial,
                via: hint.clone(),
                hint,
                member,
                https: route.https.clone(),
                probe,
                endpoint: self.endpoint.clone(),
                tx: self.internal_tx.clone(),
            }));
        }
    }

    /// The session of the room on topic `ws` with the host `scope` reached
    /// ended: with no other session of the room up, dial the next host the
    /// room's governance names, now.
    pub(super) fn native_session_closed(&mut self, ws: u64, scope: &Scope) {
        if self.host_sessions.keys().any(|(topic, _)| *topic == ws) {
            return;
        }
        // A room this device's own host serves is joined again in process,
        // now: no heartbeat runs for it, and until it is, nothing the host
        // commits reaches this device's view of the room.
        if self.native_routes.is_local(ws) || self.host_governs_here(ws) {
            self.retry_native_routes();
            return;
        }
        let after = (!scope.reaches_governor()).then(|| scope.host().clone());
        self.fail_over(ws, after.as_ref());
    }

    /// Dial the host the room on topic `ws` names after `after` (the first
    /// one when `None`) in its governance, the governor left out: the room
    /// stays in step through it while its governor is out of reach. With no
    /// host left, the next heartbeat dials the governor again and starts
    /// over when that fails.
    fn fail_over(&mut self, ws: u64, after: Option<&TransportHint>) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        let Some(route) = self.native_routes.entries.get_mut(&ws) else {
            return;
        };
        if route.local
            || route
                .pending
                .as_ref()
                .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let Ok((hint, cert, revocations, _)) = route.connect.material(identity) else {
            return;
        };
        let Ok(hosts) = route.connect.hosts(&cert.device_public_key) else {
            return;
        };
        let next = after
            .and_then(|after| hosts.iter().position(|host| host == after))
            .map_or(0, |index| index.saturating_add(1));
        let Some(via) = hosts.into_iter().nth(next) else {
            if after.is_some() {
                tracing::info!(
                    ws,
                    "no other host of the room answers; the next heartbeat dials its governor"
                );
            }
            return;
        };
        tracing::info!(
            ws,
            host = %hex::encode(via.host.device_public_key),
            "the room's governor is out of reach; dialling another of its hosts"
        );
        route.pending = Some(spawn_dial(Dial {
            ws,
            serial: route.serial,
            hint,
            via,
            member: Member {
                cert,
                key: identity.device_signing_key.clone(),
                revocations,
            },
            https: None,
            probe: false,
            endpoint: self.endpoint.clone(),
            tx: self.internal_tx.clone(),
        }));
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one dial's outcome: the pins it is checked against, failing over, and the session set up"
    )]
    pub(super) fn native_host_connected(
        &mut self,
        ws: u64,
        serial: u64,
        (hint, via): (TransportHint, TransportHint),
        certificate: DeviceCert,
        result: Box<Result<RelayCoeditSession, tilo_peer_state::owner::session::DialError>>,
    ) {
        self.tally.note(if via == hint {
            "NativeGovernorDial"
        } else {
            "NativeHostDial"
        });
        let Some(route) = self
            .native_routes
            .entries
            .get_mut(&ws)
            .filter(|route| route.serial == serial)
        else {
            return;
        };
        route.pending = None;
        let local = route.local;
        // The host is reachable again: a certificate renewal that failed
        // meanwhile tries again.
        if result.is_ok() {
            tilo_identity_host::provisioning::RENEWAL_CAUSES.changed();
        }
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        // The room's security may have moved on while dialling (another
        // member joined): the session is still this device's as long as its
        // host and certificate are, and its standing checks out now.
        let Some(continuity) =
            route
                .connect
                .material(identity)
                .ok()
                .and_then(|(current, device, _, generation)| {
                    (current == hint && device == certificate).then_some(generation)
                })
        else {
            return;
        };
        let Ok((payload, _)) = IrohWorkspaceHostHintV1::authenticate_transport_hint(&via) else {
            return;
        };
        let governor = via == hint;
        if governor {
            route.unanswered = match &*result {
                Err(tilo_peer_state::owner::session::DialError::NoAnswer(_)) => {
                    route.unanswered.saturating_add(1)
                }
                _ => 0,
            };
        }
        let connect = route.connect.clone();
        let session = match *result {
            Ok(session) => session,
            Err(error) => {
                // A host that did not answer stands for one out of reach: the
                // room goes on through its next host. One that refused this
                // device, or speaks another version, is said.
                let unreachable = matches!(
                    error,
                    tilo_peer_state::owner::session::DialError::NoAnswer(_)
                        | tilo_peer_state::owner::session::DialError::Failed(_)
                );
                dial_refused(&self.subscribers, ws, payload.endpoint_addr.id, error);
                let up = self.host_sessions.keys().any(|(topic, _)| *topic == ws);
                if unreachable && !up {
                    self.fail_over(ws, (!governor).then_some(&via));
                }
                return;
            }
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
        let scope = Arc::new(Scope::via(connect, hint, via, certificate, continuity));
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
                governor: None,
                session,
                pump,
                native_scope: Some(scope),
                local,
            },
        );
        self.drive_native_publication(ws);
    }
}

/// Drop topic `ws`'s host sessions not pinned to its room's current
/// `material`, or reaching a host the room no longer names among its
/// `hosts`, and answer whether a session with its governor is left.
fn keeps_pinned_session(
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
) -> bool {
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
    sessions.iter().any(|((topic, _), runtime)| {
        *topic == ws
            && runtime
                .native_scope
                .as_ref()
                .is_some_and(|scope| scope.reaches_governor())
    })
}

/// One dial of a room's host: `via`, the governor `hint` names or another
/// host of the room, as `member`.
struct Dial {
    ws: u64,
    serial: u64,
    hint: TransportHint,
    via: TransportHint,
    member: Member,
    https: Option<super::HttpsRoute>,
    probe: bool,
    endpoint: iroh::Endpoint,
    tx: tokio::sync::mpsc::Sender<InternalEvent>,
}

/// Dial `dial`'s host and answer the actor what came of it.
fn spawn_dial(dial: Dial) -> tokio::task::JoinHandle<()> {
    let Dial {
        ws,
        serial,
        hint,
        via,
        member,
        https,
        probe,
        endpoint,
        tx,
    } = dial;
    let Member {
        cert,
        key,
        revocations,
    } = member;
    tokio::spawn(async move {
        let expected_device = cert.clone();
        let probing = (probe, endpoint.clone(), expected_device.clone());
        let result = match https {
            Some(super::HttpsRoute(http)) => {
                tilo_peer_state::owner::session::dial_https(http, ws, &via, cert, key, revocations)
                    .await
            }
            None => {
                tilo_peer_state::owner::session::dial(endpoint, ws, &via, cert, key, revocations)
                    .await
            }
        };
        let result = match result {
            Err(tilo_peer_state::owner::session::DialError::NoAnswer(reason)) if probing.0 => {
                match tilo_peer_state::owner::session::probe_version(&probing.1, &via, &probing.2)
                    .await
                {
                    Some(theirs) => {
                        Err(tilo_peer_state::owner::session::DialError::PeerVersion { theirs })
                    }
                    None => Err(tilo_peer_state::owner::session::DialError::NoAnswer(reason)),
                }
            }
            result => result,
        };
        let _ = tx
            .send(InternalEvent::NativeHostConnected {
                ws,
                serial,
                hint: Box::new(hint),
                via: Box::new(via),
                certificate: expected_device,
                result: Box::new(result),
            })
            .await;
    })
}

/// This device as a member of a room: its certificate for the room, the
/// key it signs with, and the revocations it presents.
struct Member {
    cert: DeviceCert,
    key: ed25519_dalek::SigningKey,
    revocations: tilo_user_identity::RevocationList,
}

/// Join the room on topic `ws`, hosted by this device's own host, as
/// `member`: over the host's in-process carrier, through the handshake and
/// session a remote member's dial takes, answering as a dial does. `None`
/// until the host opens: there is nothing to join yet, and its opening asks
/// again ([`super::NetCmd::LocalHostReady`]).
fn join_local(
    local_host: Option<&LocalHost>,
    ws: u64,
    serial: u64,
    hint: TransportHint,
    member: Member,
    tx: tokio::sync::mpsc::Sender<InternalEvent>,
) -> Option<tokio::task::JoinHandle<()>> {
    let host = local_host.and_then(LocalHost::get)?;
    Some(tokio::spawn(async move {
        let Member {
            cert,
            key,
            revocations,
        } = member;
        let own_device = cert.device_public_key;
        let result = match host.connect_local(cert.clone(), key, revocations).await {
            Ok(client) => {
                tilo_peer_state::owner::session::join_local(client, ws, &hint, own_device)
            }
            Err(error) => Err(tilo_peer_state::owner::session::DialError::from(error)),
        };
        let _ = tx
            .send(InternalEvent::NativeHostConnected {
                ws,
                serial,
                via: Box::new(hint.clone()),
                hint: Box::new(hint),
                certificate: cert,
                result: Box::new(result),
            })
            .await;
    }))
}

/// What a failed dial of the joined room on topic `ws`, whose host is
/// `host`, tells the person and the shell.
fn dial_refused(
    subscribers: &HashMap<u64, tokio::sync::mpsc::Sender<super::NetEvent>>,
    ws: u64,
    host: iroh::EndpointId,
    error: tilo_peer_state::owner::session::DialError,
) {
    match error {
        // The room's host speaks another peer protocol version: the room
        // cannot sync until one side updates, which the person is told,
        // rather than holding a room that silently stays empty.
        tilo_peer_state::owner::session::DialError::PeerVersion { theirs } => {
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
        tilo_peer_state::owner::session::DialError::NotAdmitted => {
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

//! Closed relay provenance for rooms installed by the native durable owner.
//!
//! The session itself is `tilo_joined_room::session`'s; this actor keeps one
//! route per enrolled room, dials it again when its pin changes and runs
//! the connected session's pump beside every other host session. A room
//! hosted on this device is not dialled: its session goes to this device's
//! own host in process ([`super::super::local_host`]), through the same
//! handshake and session code.
use super::{
    ActorState, Arc, DeviceCert, HashMap, HostRuntime, InternalEvent, RelayCoeditSession,
    spawn_host_pump,
};
use crate::network::local_host::LocalHost;
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
pub use tilo_joined_room::session::{Connect, Input, Scope};
use tilo_peer_state::join::TransportHint;

struct Route {
    connect: Connect,
    /// The host's HTTPS routes, for a host that publishes no iroh address.
    https: Option<super::HttpsRoute>,
    serial: u64,
    pending: Option<tokio::task::JoinHandle<()>>,
    /// Dials in a row the host closed without answering this device's
    /// hello: a refusal, or a host from before versioned handshakes. Past
    /// [`tilo_joined_room::session::UNANSWERED_BEFORE_PROBE`] the next one
    /// probes which.
    unanswered: u32,
    /// The room's host is this device's own: its session stays in process.
    local: bool,
}
/// A governed room's relay session is not up (yet): the caller may retry.
pub(crate) const NOT_CONNECTED: &str = "the governed room is not connected";

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
            crate::shell::room_sync::forget(ws);
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
/// certificate ([`tilo_joined_room::enroll::renew_membership`]), over the
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
            Ok(tilo_joined_room::session::hosted_by(
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
        match tilo_joined_room::enroll::renew_membership(
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
                if changed {
                    self.drive_native_publication(ws);
                    if let Some(wake) = &self.wake {
                        wake();
                    }
                }
            }
            Ok(Err(error)) => {
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
    /// Send `msg`, a member's own sealed write, on the room's relay
    /// session, or hold it until the session is up.
    pub(super) fn publish_sealed_native(&mut self, ws: u64, msg: &super::NeighbourMsg) {
        let (Some((channel, retention)), Ok(bytes)) =
            (super::retention(msg, false), postcard::to_allocvec(msg))
        else {
            return;
        };
        if let Some(session) = native_session(&self.host_sessions, ws) {
            let sent = match retention {
                super::Retention::Checkpoint => session.publish_checkpoint(channel, bytes),
                super::Retention::Delta => session.publish_retained_delta(channel, bytes),
            };
            if let Err(error) = sent {
                tracing::warn!(ws, %error, "sealed write not sent to the room's relay");
            }
            return;
        }
        self.hold_backlog(ws, channel, retention, bytes);
    }

    pub(super) fn drive_native_publication(&self, ws: u64) {
        for (scope, session, identity) in self.native_scopes(ws) {
            if let Err(error) = scope.publish(session, identity) {
                tracing::debug!(%error,"native publication remains in durable custody");
            }
        }
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
        let ws = crate::shell::network::room_topic_id(connect.room());
        if !self.native_routes.contains(ws) && self.native_routes.entries.len() >= 64 {
            return;
        }
        self.host_sessions.retain(|(topic, _), _| *topic != ws);
        let Some(serial) = self.native_routes.serial.checked_add(1) else {
            return;
        };
        self.native_routes.serial = serial;
        // Apps in the room say they are connecting until its first sync.
        crate::shell::room_sync::connecting(ws);
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
        self.retry_native_routes();
    }
    pub(super) fn retry_native_routes(&mut self) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
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
            route.local = tilo_joined_room::session::hosted_by(&hint, &cert.device_public_key);
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
                && route.unanswered >= tilo_joined_room::session::UNANSWERED_BEFORE_PROBE;
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
        result: Box<Result<RelayCoeditSession, tilo_joined_room::session::DialError>>,
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
                Err(tilo_joined_room::session::DialError::NoAnswer(_)) => {
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
                    tilo_joined_room::session::DialError::NoAnswer(_)
                        | tilo_joined_room::session::DialError::Failed(_)
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
        );
        // What this device said while the session was down: its channel
        // governance (the steward redelivers its own) and its sealed writes.
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
                tracing::warn!(ws, %error, "held sealed write not sent to the room's relay");
            }
        }
        // Pointers, cursor chat and reactions ride the room's live plane,
        // which carries only what its members say: join it, as a workspace
        // host's session does.
        if let Err(error) = session.live_join() {
            tracing::warn!(ws, %error, "could not join the room's live plane");
        }
        self.native_media.reconnected(ws, &session);
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
                tilo_joined_room::session::dial_https(http, ws, &via, cert, key, revocations).await
            }
            None => {
                tilo_joined_room::session::dial(endpoint, ws, &via, cert, key, revocations).await
            }
        };
        let result = match result {
            Err(tilo_joined_room::session::DialError::NoAnswer(reason)) if probing.0 => {
                match tilo_joined_room::session::probe_version(&probing.1, &via, &probing.2).await {
                    Some(theirs) => {
                        Err(tilo_joined_room::session::DialError::PeerVersion { theirs })
                    }
                    None => Err(tilo_joined_room::session::DialError::NoAnswer(reason)),
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
            Ok(client) => tilo_joined_room::session::join_local(client, ws, &hint, own_device),
            Err(error) => Err(tilo_joined_room::session::DialError::from(error)),
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
    error: tilo_joined_room::session::DialError,
) {
    match error {
        // The room's host speaks another peer protocol version: the room
        // cannot sync until one side updates, which the person is told,
        // rather than holding a room that silently stays empty.
        tilo_joined_room::session::DialError::PeerVersion { theirs } => {
            tracing::warn!(
                ws,
                theirs,
                ours = neighbour_wire::PEER_PROTOCOL_VERSION,
                "the room's host speaks another peer protocol version"
            );
            crate::shell::collaborator_versions::note(host.to_string(), String::new(), theirs);
            crate::shell::room_sync::other_version(ws, theirs);
        }
        // The room removed this device's person: dialing again changes
        // nothing until they join again, which the shell does through the
        // room's door where it has one.
        tilo_joined_room::session::DialError::NotAdmitted => {
            tracing::warn!(ws, "the room's host admits this device's person no longer");
            if let Some(subscriber) = subscribers.get(&ws) {
                let _ = subscriber.try_send(super::NetEvent::NotAdmitted { ws });
            }
        }
        error => tracing::debug!(%error, ws, "native host dial failed; retrying"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::actor::HostIdentity;
    #[test]
    fn native_custody_missing_disk_never_uses_retained_certificate() {
        let (directory, owner, room, _, key) =
            crate::shell::native_workspace_owner::test_enrolled_owner();
        let certificate = owner
            .with_current(room, |view| Ok(view.device().clone()))
            .unwrap();
        let mut identity = HostIdentity {
            device_cert: certificate,
            device_signing_key: key,
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        };
        let topic = crate::shell::network::room_topic_id(room);
        assert!(identity.current_native_material(topic).is_ok());
        identity.identity_paths = Some(tilo_user_identity::IdentityPaths::under(
            directory.path().join("missing-custody"),
        ));
        assert!(identity.current_native_material(topic).is_err());
        assert!(
            Connect::new(owner, room)
                .unwrap()
                .material(&identity)
                .is_err()
        );
    }
}

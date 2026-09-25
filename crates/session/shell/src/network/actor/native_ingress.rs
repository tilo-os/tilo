//! Closed relay provenance for rooms installed by the native durable owner.
use super::*;
use crate::shell::{native_workspace_owner::NativeWorkspaceOwner, state::RoomId};
use qos_peer_state::{NeighbourMsg, TransportOrigin, join::TransportHint};
use tilo_relay::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostTrust};

#[derive(Clone)]
pub(crate) struct Connect {
    owner: NativeWorkspaceOwner,
    room: RoomId,
}
impl std::fmt::Debug for Connect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NativeConnect").field(&self.room).finish()
    }
}
impl Connect {
    pub(crate) fn new(owner: NativeWorkspaceOwner, room: RoomId) -> Result<Self, String> {
        owner.with_current(room, |_| Ok(()))?;
        Ok(Self { owner, room })
    }
    fn material(
        &self,
        identity: &HostIdentity,
    ) -> Result<(TransportHint, DeviceCert, RevocationList, u64), String> {
        self.owner.with_current(self.room, |view| {
            let (cert, entries) = identity.current_native_material(view.workspace())?;
            if &cert != view.device() {
                return Err("native relay custody differs from enrollment".into());
            }
            view.peer()
                .permissions(cert.device_public_key, std::time::SystemTime::now())?;
            if !view.peer().certificates().contains(&cert) {
                return Err("selected certificate differs from current admitted roster".into());
            }
            let mut revocations = RevocationList::new();
            for entry in entries.into_iter().chain(view.peer().revocations()) {
                revocations.insert(entry).map_err(|e| e.to_string())?;
            }
            if revocations.is_revoked(&view.transport_hint().host.device_public_key) {
                return Err("native relay host is revoked".into());
            }
            Ok((
                view.transport_hint().clone(),
                cert,
                revocations,
                view.continuity(),
            ))
        })
    }
}
struct Route {
    connect: Connect,
    serial: u64,
    pending: Option<tokio::task::JoinHandle<()>>,
}
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
        self.entries.remove(&ws);
    }
}

pub(super) struct Scope {
    connect: Connect,
    hint: TransportHint,
    device: DeviceCert,
    continuity: u64,
    live: Mutex<bool>,
    cursor: Mutex<super::native_publication::Cursor>,
    refresh_sent: Mutex<Option<std::time::Instant>>,
}
impl Scope {
    pub(super) fn close(&self) {
        if let Ok(mut live) = self.live.lock() {
            *live = false;
        }
    }
}
impl Scope {
    pub(super) fn input(
        self: &Arc<Self>,
        author: [u8; 32],
        retained: bool,
        msg: NeighbourMsg,
    ) -> Input {
        Input {
            scope: self.clone(),
            author,
            retained,
            msg,
        }
    }
}
/// Only the authenticated relay pump can construct this. Generic gossip messages
/// cannot become RelayDelta evidence through the shell event dispatcher.
#[derive(Clone)]
pub(crate) struct Input {
    scope: Arc<Scope>,
    author: [u8; 32],
    retained: bool,
    msg: NeighbourMsg,
}
impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRelayInput")
            .field("room", &self.scope.connect.room)
            .field("retained", &self.retained)
            .finish_non_exhaustive()
    }
}
impl Input {
    pub(crate) fn room(&self) -> RoomId {
        self.scope.connect.room
    }
    pub(crate) fn author(&self) -> [u8; 32] {
        self.author
    }
    pub(crate) fn message(&self) -> &NeighbourMsg {
        &self.msg
    }
    pub(crate) fn admit(
        &self,
        identity: &HostIdentity,
    ) -> Result<qos_peer_state::AdmissionReport, String> {
        let live = self
            .scope
            .live
            .lock()
            .map_err(|_| "native relay continuity poisoned")?;
        if !*live {
            return Err("native relay session closed".into());
        }
        let (hint, device, _, continuity) = self.scope.connect.material(identity)?;
        if hint != self.scope.hint
            || device != self.scope.device
            || continuity != self.scope.continuity
        {
            return Err("native relay pin changed".into());
        }
        let origin = if self.retained {
            TransportOrigin::RetainedRelayDelta(self.author)
        } else {
            TransportOrigin::RelayDelta(self.author)
        };
        self.scope.connect.owner.receive(
            self.room(),
            origin,
            self.msg.clone(),
            std::time::SystemTime::now(),
        )
    }
    pub(crate) fn role(&self) -> Result<collab_types::Role, String> {
        self.scope.connect.owner.with_current(self.room(), |view| {
            view.peer()
                .permissions(self.author, std::time::SystemTime::now())
                .map(|(role, _, _)| role)
        })
    }
}
impl ActorState {
    /// A refresh only asks the original subscribed host to run its own genuine
    /// checkpoint barrier. It grants no local admission and retires no outbox.
    pub(super) fn request_native_refresh(&self, ws: u64) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        for ((topic, _), runtime) in &self.host_sessions {
            if *topic != ws {
                continue;
            }
            let Some(scope) = runtime.native_scope.as_ref() else {
                continue;
            };
            let result = (|| {
                let live = scope
                    .live
                    .lock()
                    .map_err(|_| "native refresh session poisoned")?;
                if !*live {
                    return Err("native refresh session closed".into());
                }
                let (hint, device, _, continuity) = scope.connect.material(identity)?;
                if hint != scope.hint || device != scope.device || continuity != scope.continuity {
                    return Err("native refresh scope changed".into());
                }
                scope
                    .connect
                    .owner
                    .with_current(scope.connect.room, |view| {
                        if view.device() != &scope.device
                            || view.transport_hint() != &scope.hint
                            || view.continuity() != scope.continuity
                        {
                            return Err("native refresh current owner changed".into());
                        }
                        view.peer().permissions(
                            scope.device.device_public_key,
                            std::time::SystemTime::now(),
                        )?;
                        let mut last = scope
                            .refresh_sent
                            .lock()
                            .map_err(|_| "native refresh cadence poisoned")?;
                        let now = std::time::Instant::now();
                        if last.is_some_and(|sent| {
                            now.saturating_duration_since(sent) < Duration::from_secs(30)
                        }) {
                            return Ok(());
                        }
                        runtime
                            .session
                            .request_checkpoint_refresh()
                            .map_err(|error| error.to_string())?;
                        *last = Some(now);
                        Ok(())
                    })
            })();
            if let Err(error) = result {
                tracing::debug!(%error, "native checkpoint refresh blocked");
            }
        }
    }
    pub(super) fn drive_native_publication(&mut self, ws: u64) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        for ((topic, _), runtime) in &self.host_sessions {
            if *topic != ws {
                continue;
            }
            let Some(scope) = runtime.native_scope.as_ref() else {
                continue;
            };
            let result = (|| {
                let live = scope
                    .live
                    .lock()
                    .map_err(|_| "native publication session poisoned")?;
                if !*live {
                    return Err("native publication session closed".into());
                }
                let (hint, device, _, continuity) = scope.connect.material(identity)?;
                if hint != scope.hint || device != scope.device || continuity != scope.continuity {
                    return Err("native publication session changed".into());
                }
                // Reconciliation may persist a genuine authorized successor. Its
                // failure must not erase or prevent other fresh retained frames.
                if let Err(error) = scope.connect.owner.reconcile_outgoing(
                    scope.connect.room,
                    &identity.device_signing_key,
                    std::time::SystemTime::now(),
                ) {
                    tracing::debug!(%error,"native publication reconciliation blocked");
                }
                scope.connect.owner.with_outgoing(
                    scope.connect.room,
                    std::time::SystemTime::now(),
                    |view, outgoing| {
                        if view.device() != &scope.device
                            || view.transport_hint() != &scope.hint
                            || view.continuity() != scope.continuity
                        {
                            return Err("native publication authority changed".into());
                        }
                        let (selected, _) = identity.current_native_material(view.workspace())?;
                        if &selected != view.device() {
                            return Err("native publication custody changed".into());
                        }
                        view.peer().permissions(
                            selected.device_public_key,
                            std::time::SystemTime::now(),
                        )?;
                        let mut cursor = scope
                            .cursor
                            .lock()
                            .map_err(|_| "native publication cursor poisoned")?;
                        cursor.drive(
                            outgoing.entries().map(|entry| {
                                (
                                    entry.id(),
                                    entry.bytes(),
                                    entry.channel(),
                                    entry.retention(),
                                )
                            }),
                            |bytes, channel, retention| {
                                match retention {
                                    qos_peer_state::transport::Retention::Delta => runtime
                                        .session
                                        .publish_retained_delta(channel, bytes.to_vec()),
                                    // Members never republish a governor checkpoint with
                                    // their own relay author, even if a producer errs.
                                    qos_peer_state::transport::Retention::Checkpoint => {
                                        return Err(
                                            "native member checkpoint publication refused".into()
                                        );
                                    }
                                }
                                .map_err(|e| e.to_string())
                            },
                        )
                    },
                )
            })();
            if let Err(error) = result {
                tracing::debug!(%error,"native publication remains in durable custody");
            }
        }
    }
    pub(super) fn native_connect(&mut self, connect: Connect) {
        let ws = crate::shell::network::room_topic_id(connect.room);
        if !self.native_routes.contains(ws) && self.native_routes.entries.len() >= 64 {
            return;
        }
        self.host_sessions.retain(|(topic, _), _| *topic != ws);
        let Some(serial) = self.native_routes.serial.checked_add(1) else {
            return;
        };
        self.native_routes.serial = serial;
        self.native_routes.entries.insert(
            ws,
            Route {
                connect,
                serial: self.native_routes.serial,
                pending: None,
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
            if material.is_err() {
                self.host_sessions.retain(|(topic, _), _| *topic != ws);
                if let Some(task) = route.pending.take() {
                    task.abort();
                }
                continue;
            }
            self.host_sessions.retain(|(topic, _), runtime| {
                *topic != ws
                    || runtime.native_scope.as_ref().is_some_and(|scope| {
                        material
                            .as_ref()
                            .is_ok_and(|(hint, device, _, continuity)| {
                                hint == &scope.hint
                                    && device == &scope.device
                                    && *continuity == scope.continuity
                            })
                    })
            });
            if self.host_sessions.keys().any(|(topic, _)| *topic == ws)
                || route
                    .pending
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
            {
                continue;
            }
            let (hint, cert, revocations, continuity) = material.expect("checked native material");
            let expected_device = cert.clone();
            let endpoint = self.endpoint.clone();
            let key = identity.device_signing_key.clone();
            let tx = self.internal_tx.clone();
            let serial = route.serial;
            route.pending = Some(tokio::spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(10), async {
                    let (payload, trust) =
                        IrohWorkspaceHostHintV1::authenticate_transport_hint(&hint)
                            .map_err(|e| e.to_string())?;
                    if payload.workspace_id != ws {
                        return Err("native relay workspace pin mismatch".into());
                    }
                    let addr = payload.endpoint_addr.clone();
                    let own_device = cert.device_public_key;
                    let client = match trust {
                        WorkspaceHostTrust::CertifiedHost => {
                            RelayClient::connect_v2_to_enrolled_home_peer(
                                endpoint,
                                addr,
                                hint.host.user_public_key,
                                hint.host.device_public_key,
                                cert,
                                key,
                                revocations,
                            )
                            .await
                        }
                        WorkspaceHostTrust::InteractiveOwner(trust) => {
                            RelayClient::connect_v2_to_interactive_workspace_owner(
                                endpoint,
                                addr,
                                trust,
                                cert,
                                key,
                                revocations,
                            )
                            .await
                        }
                    }
                    .map_err(|e| e.to_string())?;
                    if client.relay_peer.cert != hint.host {
                        return Err(
                            "native relay authenticated a different host certificate".into()
                        );
                    }
                    RelayCoeditSession::join(client, ws, own_device).map_err(|e| e.to_string())
                })
                .await
                .map_err(|_| "native relay connection timed out".to_string())
                .and_then(|result| result);
                let _ = tx
                    .send(InternalEvent::NativeHostConnected {
                        ws,
                        serial,
                        hint,
                        certificate: expected_device,
                        continuity,
                        result: Box::new(result),
                    })
                    .await;
            }));
        }
    }
    pub(super) fn native_host_connected(
        &mut self,
        ws: u64,
        serial: u64,
        hint: TransportHint,
        certificate: DeviceCert,
        continuity: u64,
        result: Box<Result<RelayCoeditSession, String>>,
    ) {
        let Some(route) = self
            .native_routes
            .entries
            .get_mut(&ws)
            .filter(|route| route.serial == serial)
        else {
            return;
        };
        route.pending = None;
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        if !route
            .connect
            .material(identity)
            .is_ok_and(|(current, device, _, generation)| {
                current == hint && device == certificate && generation == continuity
            })
        {
            return;
        }
        let Ok(session) = *result else { return };
        let Ok((payload, _)) = IrohWorkspaceHostHintV1::authenticate_transport_hint(&hint) else {
            return;
        };
        let host = payload.endpoint_addr.id;
        let scope = Arc::new(Scope {
            connect: route.connect.clone(),
            hint,
            device: certificate,
            continuity,
            live: Mutex::new(true),
            cursor: Mutex::new(super::native_publication::Cursor::default()),
            refresh_sent: Mutex::new(None),
        });
        let session = Arc::new(session);
        let pump = spawn_host_pump(
            ws,
            host,
            session.clone(),
            self.internal_tx.clone(),
            Some(scope.clone()),
        );
        self.host_sessions.insert(
            (ws, host),
            HostRuntime {
                actions: None,
                session,
                pump,
                native_scope: Some(scope),
            },
        );
        self.drive_native_publication(ws);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            workspace_device_certs: Default::default(),
            workspace_revocations: Default::default(),
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

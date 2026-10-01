//! Closed relay provenance for rooms installed by the native durable owner.
//!
//! The session itself is `tilo_joined_room::session`'s; this actor keeps one
//! route per enrolled room, dials it again when its pin changes and runs
//! the connected session's pump beside every other host session.
use super::{
    ActorState, Arc, DeviceCert, HashMap, HostRuntime, InternalEvent, RelayCoeditSession,
    spawn_host_pump,
};
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
pub use tilo_joined_room::session::{Connect, Input, Scope};
use tilo_peer_state::join::TransportHint;

struct Route {
    connect: Connect,
    serial: u64,
    pending: Option<tokio::task::JoinHandle<()>>,
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
        self.entries.remove(&ws);
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
            let result = scope.refresh(&runtime.session, identity);
            if let Err(error) = result {
                tracing::debug!(%error, "native checkpoint refresh blocked");
            }
        }
    }
    pub(super) fn drive_native_publication(&self, ws: u64) {
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
            let result = scope.publish(&runtime.session, identity);
            if let Err(error) = result {
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
                (*topic == ws && runtime.native_scope.is_some()).then_some(runtime)
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

    pub(super) fn native_connect(&mut self, connect: Connect) {
        let ws = crate::shell::network::room_topic_id(connect.room());
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
            if let Err(error) = &material {
                tracing::debug!(%error, ws, "native room custody unusable for its host");
            }
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
                                scope.pinned_to(hint, device, *continuity)
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
            let Ok((hint, cert, revocations, continuity)) = material else {
                continue;
            };
            let expected_device = cert.clone();
            let endpoint = self.endpoint.clone();
            let key = identity.device_signing_key.clone();
            let tx = self.internal_tx.clone();
            let serial = route.serial;
            route.pending = Some(tokio::spawn(async move {
                let result =
                    tilo_joined_room::session::dial(endpoint, ws, &hint, cert, key, revocations)
                        .await;
                let _ = tx
                    .send(InternalEvent::NativeHostConnected {
                        ws,
                        serial,
                        hint: Box::new(hint),
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
        let session = match *result {
            Ok(session) => session,
            Err(error) => {
                tracing::debug!(%error, ws, "native host dial failed; retrying");
                return;
            }
        };
        let Ok((payload, _)) = IrohWorkspaceHostHintV1::authenticate_transport_hint(&hint) else {
            return;
        };
        let host = payload.endpoint_addr.id;
        let scope = Arc::new(Scope::new(
            route.connect.clone(),
            hint,
            certificate,
            continuity,
        ));
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
            },
        );
        self.drive_native_publication(ws);
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

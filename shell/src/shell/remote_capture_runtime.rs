//! Certified-route orchestration for remote camera, microphone, and scanner sessions.
//!
//! This state machine correlates authenticated personal-fabric envelopes. It
//! never accepts app identity from an inbound decision or media chunk: the
//! verified local registration captured when the request was made remains the
//! authority used by the target-local adapter bridge.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use rand_core::{OsRng, RngCore};

use crate::portal::CaptureBroker;
use crate::shell::device_fabric::{PersonalFabricAction, PersonalFabricActionEnvelope};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::realtime_session::VerifiedAppRegistration;
use crate::shell::remote_capture_adapter::{
    RemoteCaptureAdapter, RemoteCaptureAdapterError, RemoteCaptureEndpoint,
    RemoteCaptureTargetBridge,
};
use crate::shell::remote_capture_device::{
    MAX_REMOTE_CAPTURE_SESSIONS, REMOTE_CAPTURE_PROTOCOL_VERSION, RemoteCaptureChunk,
    RemoteCaptureGrant, RemoteCaptureProfile, RemoteCaptureRegistry, RemoteCaptureRequest,
    RemoteCaptureSignal,
};
use crate::shell::remote_capture_source::{CapturedRemoteChunk, RemoteCaptureSource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteCaptureConsentView {
    pub request_id: [u8; 16],
    pub peer: DeviceId,
    pub app_label: String,
    pub profile: RemoteCaptureProfile,
}

struct OutboundRequest {
    app: VerifiedAppRegistration,
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
}

struct InboundRequest {
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
}

struct TargetSession {
    app: VerifiedAppRegistration,
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
    grant: RemoteCaptureGrant,
}

struct SourceSession {
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
    grant: RemoteCaptureGrant,
    next_sequence: u64,
}

struct ProvisionResult {
    session_id: [u8; 16],
    endpoint: Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError>,
}

pub(crate) struct RemoteCaptureFabricRuntime {
    registry: RemoteCaptureRegistry,
    bridge: RemoteCaptureTargetBridge,
    adapter: Arc<Mutex<Box<dyn RemoteCaptureAdapter>>>,
    source: Box<dyn RemoteCaptureSource>,
    provision_tx: mpsc::SyncSender<ProvisionResult>,
    provision_rx: mpsc::Receiver<ProvisionResult>,
    provisioning: BTreeMap<[u8; 16], TargetSession>,
    completed_provisioning:
        BTreeMap<[u8; 16], Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError>>,
    pending_revoke: BTreeSet<[u8; 16]>,
    pending_bridge_peers: BTreeSet<DeviceId>,
    pending_bridge_revoke_all: bool,
    outgoing: Vec<(DeviceId, PersonalFabricAction)>,
    outbound: BTreeMap<[u8; 16], OutboundRequest>,
    inbound: BTreeMap<[u8; 16], InboundRequest>,
    target_sessions: BTreeMap<[u8; 16], TargetSession>,
    source_sessions: BTreeMap<[u8; 16], SourceSession>,
}

impl RemoteCaptureFabricRuntime {
    pub(crate) fn new(
        broker: Arc<Mutex<CaptureBroker>>,
        adapter: Box<dyn RemoteCaptureAdapter>,
        source: Box<dyn RemoteCaptureSource>,
    ) -> Self {
        let (provision_tx, provision_rx) = mpsc::sync_channel(MAX_REMOTE_CAPTURE_SESSIONS);
        Self {
            registry: RemoteCaptureRegistry::default(),
            bridge: RemoteCaptureTargetBridge::new(broker),
            adapter: Arc::new(Mutex::new(adapter)),
            source,
            provision_tx,
            provision_rx,
            provisioning: BTreeMap::new(),
            completed_provisioning: BTreeMap::new(),
            pending_revoke: BTreeSet::new(),
            pending_bridge_peers: BTreeSet::new(),
            pending_bridge_revoke_all: false,
            outgoing: Vec::new(),
            outbound: BTreeMap::new(),
            inbound: BTreeMap::new(),
            target_sessions: BTreeMap::new(),
            source_sessions: BTreeMap::new(),
        }
    }

    pub(crate) fn request(
        &mut self,
        app: VerifiedAppRegistration,
        source: DeviceId,
        target: DeviceId,
        app_label: String,
        profile: RemoteCaptureProfile,
    ) -> Result<PersonalFabricAction, String> {
        if source == target || self.outbound.len() >= MAX_REMOTE_CAPTURE_SESSIONS {
            return Err("remote capture request capacity is exhausted".into());
        }
        let request_id = random_id(|id| self.outbound.contains_key(id));
        let request = RemoteCaptureRequest {
            version: REMOTE_CAPTURE_PROTOCOL_VERSION,
            request_id,
            app_id: app.app_id().to_string(),
            workspace_id: app.workspace_id().to_string(),
            app_label,
            profile,
        };
        if !request.valid() {
            return Err("invalid remote capture request".into());
        }
        self.outbound.insert(
            request_id,
            OutboundRequest {
                app,
                request: request.clone(),
                source,
                target,
            },
        );
        Ok(PersonalFabricAction::RemoteCaptureSignal {
            signal: RemoteCaptureSignal::Request(request),
        })
    }

    pub(crate) fn handle_action(
        &mut self,
        envelope: &PersonalFabricActionEnvelope,
        now: Instant,
    ) -> bool {
        match &envelope.action {
            PersonalFabricAction::RemoteCaptureSignal { signal } => {
                self.handle_signal(envelope, signal, now);
                true
            }
            PersonalFabricAction::RemoteCaptureChunk { chunk } => {
                self.handle_chunk(envelope, chunk, now);
                true
            }
            _ => false,
        }
    }

    /// Advance worker-owned endpoint provisioning and source capture without
    /// waiting on PipeWire/V4L2 discovery from the compositor frame.
    pub(crate) fn pump(&mut self, now: Instant) {
        for result in self.provision_rx.try_iter() {
            self.completed_provisioning
                .insert(result.session_id, result.endpoint);
        }

        let completed = self
            .completed_provisioning
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for session_id in completed {
            let Some(endpoint) = self.completed_provisioning.remove(&session_id) else {
                continue;
            };
            let Some(session) = self.provisioning.remove(&session_id) else {
                self.pending_revoke.insert(session_id);
                continue;
            };
            match endpoint {
                Ok(endpoint) => {
                    let Ok(mut adapter) = self.adapter.try_lock() else {
                        self.provisioning.insert(session_id, session);
                        self.completed_provisioning.insert(session_id, Ok(endpoint));
                        continue;
                    };
                    if self
                        .bridge
                        .prepare_endpoint(
                            &session.app,
                            &session.request,
                            session.source.clone(),
                            session.target.clone(),
                            session_id,
                            session.grant,
                            endpoint,
                            adapter.as_mut(),
                        )
                        .is_ok()
                    {
                        self.target_sessions.insert(session_id, session);
                    } else {
                        self.registry.revoke(session_id, session.grant);
                        self.outgoing
                            .push((session.source, stop_action(session_id, session.grant)));
                    }
                }
                Err(_) => {
                    self.registry.revoke(session_id, session.grant);
                    self.outgoing
                        .push((session.source, stop_action(session_id, session.grant)));
                }
            }
        }

        if let Ok(mut adapter) = self.adapter.try_lock() {
            for session_id in std::mem::take(&mut self.pending_revoke) {
                adapter.revoke(session_id);
            }
            if self.pending_bridge_revoke_all {
                self.bridge.revoke_all(adapter.as_mut());
                self.pending_bridge_revoke_all = false;
                self.pending_bridge_peers.clear();
            } else {
                for peer in std::mem::take(&mut self.pending_bridge_peers) {
                    self.bridge.revoke_peer(&peer, adapter.as_mut());
                }
            }
        }

        for captured in self.source.drain() {
            self.publish_captured(captured, now);
        }
        for session_id in self.source.drain_failures() {
            if let Some(session) = self.source_sessions.remove(&session_id) {
                self.source.stop(session_id);
                self.registry.revoke(session_id, session.grant);
                self.outgoing
                    .push((session.target, stop_action(session_id, session.grant)));
            }
        }
    }

    pub(crate) fn drain_outgoing(&mut self) -> Vec<(DeviceId, PersonalFabricAction)> {
        std::mem::take(&mut self.outgoing)
    }

    fn publish_captured(&mut self, captured: CapturedRemoteChunk, now: Instant) {
        let Some(session) = self.source_sessions.get_mut(&captured.session_id) else {
            return;
        };
        let sequence = session.next_sequence;
        let Some(next_sequence) = session.next_sequence.checked_add(1) else {
            self.source.stop(captured.session_id);
            self.source_sessions.remove(&captured.session_id);
            return;
        };
        session.next_sequence = next_sequence;
        let chunk = RemoteCaptureChunk {
            session_id: captured.session_id,
            grant: session.grant,
            sequence,
            offset: captured.offset,
            total_bytes: captured.total_bytes,
            page_index: captured.page_index,
            end_of_unit: captured.end_of_unit,
            payload: captured.payload,
        };
        if !self.registry.accept_chunk(
            &session.source,
            &session.target,
            &session.request.app_id,
            &session.request.workspace_id,
            &chunk,
            now,
        ) {
            let grant = session.grant;
            let target = session.target.clone();
            self.source.stop(captured.session_id);
            self.source_sessions.remove(&captured.session_id);
            self.registry.revoke(captured.session_id, grant);
            self.outgoing
                .push((target, stop_action(captured.session_id, grant)));
            return;
        }
        self.outgoing.push((
            session.target.clone(),
            PersonalFabricAction::RemoteCaptureChunk { chunk },
        ));
    }

    fn handle_signal(
        &mut self,
        envelope: &PersonalFabricActionEnvelope,
        signal: &RemoteCaptureSignal,
        now: Instant,
    ) {
        match signal {
            RemoteCaptureSignal::Request(request) => {
                if self.inbound.len() >= MAX_REMOTE_CAPTURE_SESSIONS
                    || self.inbound.contains_key(&request.request_id)
                {
                    return;
                }
                self.inbound.insert(
                    request.request_id,
                    InboundRequest {
                        request: request.clone(),
                        source: envelope.target.clone(),
                        target: envelope.source.clone(),
                    },
                );
            }
            RemoteCaptureSignal::Decision {
                request_id,
                accepted,
                session_id,
                grant,
                profile,
            } => {
                let Some(pending) = self.outbound.remove(request_id) else {
                    return;
                };
                if pending.source != envelope.source || pending.target != envelope.target {
                    self.outbound.insert(*request_id, pending);
                    return;
                }
                if !accepted {
                    return;
                }
                let (Some(session_id), Some(grant), Some(profile)) =
                    (*session_id, *grant, profile.clone())
                else {
                    return;
                };
                if !self.registry.accept_grant(
                    &pending.request,
                    pending.source.clone(),
                    pending.target.clone(),
                    session_id,
                    grant,
                    profile,
                    now,
                ) {
                    return;
                }
                let target_session = TargetSession {
                    app: pending.app,
                    request: pending.request,
                    source: pending.source,
                    target: pending.target,
                    grant,
                };
                self.provisioning.insert(session_id, target_session);
                let adapter = Arc::clone(&self.adapter);
                let tx = self.provision_tx.clone();
                let profile = self
                    .provisioning
                    .get(&session_id)
                    .expect("inserted")
                    .request
                    .profile
                    .clone();
                if std::thread::Builder::new()
                    .name("tilo-remote-capture-provision".into())
                    .spawn(move || {
                        let endpoint = adapter
                            .lock()
                            .map_err(|_| {
                                RemoteCaptureAdapterError::BackendUnavailable(
                                    "remote capture adapter lock is unavailable".into(),
                                )
                            })
                            .and_then(|mut adapter| adapter.provision(session_id, &profile));
                        // The queue is bounded by the maximum provisioning map.
                        if tx
                            .try_send(ProvisionResult {
                                session_id,
                                endpoint,
                            })
                            .is_err()
                            && let Ok(mut adapter) = adapter.lock()
                        {
                            adapter.revoke(session_id);
                        }
                    })
                    .is_err()
                {
                    self.provisioning.remove(&session_id);
                    self.registry.revoke(session_id, grant);
                }
            }
            RemoteCaptureSignal::Stop { session_id, grant } => {
                if let Some(session) = self.target_sessions.get(session_id)
                    && session.source == envelope.source
                    && session.target == envelope.target
                    && session.grant == *grant
                {
                    let session = self.target_sessions.remove(session_id).expect("present");
                    self.registry.revoke(*session_id, *grant);
                    if let Ok(mut adapter) = self.adapter.try_lock() {
                        let _ = self.bridge.revoke(
                            &session.app,
                            &session.source,
                            &session.target,
                            *session_id,
                            *grant,
                            adapter.as_mut(),
                        );
                    } else {
                        self.pending_revoke.insert(*session_id);
                    }
                } else if let Some(session) = self.source_sessions.get(session_id)
                    && session.target == envelope.source
                    && session.source == envelope.target
                    && session.grant == *grant
                {
                    self.source_sessions.remove(session_id);
                    self.source.stop(*session_id);
                    self.registry.revoke(*session_id, *grant);
                }
            }
        }
    }

    fn handle_chunk(
        &mut self,
        envelope: &PersonalFabricActionEnvelope,
        chunk: &RemoteCaptureChunk,
        now: Instant,
    ) {
        let Some(session) = self.target_sessions.get(&chunk.session_id) else {
            return;
        };
        if session.source != envelope.source
            || session.target != envelope.target
            || !self.bridge.accepts_chunk(
                &session.app,
                &session.source,
                &session.target,
                chunk.session_id,
                chunk.grant,
            )
            || !self.registry.accept_chunk(
                &session.source,
                &session.target,
                session.app.app_id(),
                session.app.workspace_id(),
                chunk,
                now,
            )
        {
            return;
        }
        match session.request.profile.kind() {
            crate::shell::remote_capture_device::RemoteCaptureKind::DocumentScanner => {
                let _ = self.bridge.append_scanner_chunk(
                    &session.app,
                    &session.source,
                    &session.target,
                    chunk,
                );
            }
            crate::shell::remote_capture_device::RemoteCaptureKind::Camera
            | crate::shell::remote_capture_device::RemoteCaptureKind::Microphone => {
                if let Ok(mut adapter) = self.adapter.try_lock() {
                    let _ = self.bridge.append_media_chunk(
                        &session.app,
                        &session.source,
                        &session.target,
                        chunk,
                        adapter.as_mut(),
                    );
                }
            }
        }
    }

    pub(crate) fn consents(&self) -> Vec<RemoteCaptureConsentView> {
        self.inbound
            .values()
            .map(|pending| RemoteCaptureConsentView {
                request_id: pending.request.request_id,
                peer: pending.target.clone(),
                app_label: pending.request.app_label.clone(),
                profile: pending.request.profile.clone(),
            })
            .collect()
    }

    pub(crate) fn decide(
        &mut self,
        request_id: [u8; 16],
        accepted: bool,
        now: Instant,
    ) -> Result<(DeviceId, PersonalFabricAction), String> {
        let pending = self
            .inbound
            .remove(&request_id)
            .ok_or_else(|| "unknown remote capture request".to_string())?;
        if !accepted {
            return Ok((
                pending.target,
                PersonalFabricAction::RemoteCaptureSignal {
                    signal: RemoteCaptureSignal::Decision {
                        request_id,
                        accepted: false,
                        session_id: None,
                        grant: None,
                        profile: None,
                    },
                },
            ));
        }
        let (session_id, grant) = self.registry.grant(
            &pending.request,
            pending.source.clone(),
            pending.target.clone(),
            now,
        )?;
        if self
            .source
            .start(session_id, &pending.request.profile)
            .is_err()
        {
            self.registry.revoke(session_id, grant);
            return Ok((
                pending.target,
                PersonalFabricAction::RemoteCaptureSignal {
                    signal: RemoteCaptureSignal::Decision {
                        request_id,
                        accepted: false,
                        session_id: None,
                        grant: None,
                        profile: None,
                    },
                },
            ));
        }
        self.source_sessions.insert(
            session_id,
            SourceSession {
                request: pending.request.clone(),
                source: pending.source,
                target: pending.target.clone(),
                grant,
                next_sequence: 1,
            },
        );
        Ok((
            pending.target,
            PersonalFabricAction::RemoteCaptureSignal {
                signal: RemoteCaptureSignal::Decision {
                    request_id,
                    accepted: true,
                    session_id: Some(session_id),
                    grant: Some(grant),
                    profile: Some(pending.request.profile),
                },
            },
        ))
    }

    pub(crate) fn approve_target(
        &mut self,
        session_id: [u8; 16],
        now: Instant,
    ) -> Result<(), String> {
        let session = self
            .target_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown remote capture session".to_string())?;
        let approval = self
            .bridge
            .approve(
                &session.app,
                &session.source,
                &session.target,
                session_id,
                session.grant,
                now,
            )
            .map_err(|error| error.to_string())?;
        self.bridge
            .activate(
                &session.app,
                &session.source,
                &session.target,
                session_id,
                session.grant,
                approval,
                now,
            )
            .map_err(|error| error.to_string())
    }

    pub(crate) fn send_chunk(
        &mut self,
        chunk: RemoteCaptureChunk,
        now: Instant,
    ) -> Result<(DeviceId, PersonalFabricAction), String> {
        let session = self
            .source_sessions
            .get(&chunk.session_id)
            .ok_or_else(|| "unknown remote capture source session".to_string())?;
        if !self.registry.accept_chunk(
            &session.source,
            &session.target,
            &session.request.app_id,
            &session.request.workspace_id,
            &chunk,
            now,
        ) {
            return Err("remote capture chunk failed its exact session binding".into());
        }
        Ok((
            session.target.clone(),
            PersonalFabricAction::RemoteCaptureChunk { chunk },
        ))
    }

    pub(crate) fn teardown_peer(&mut self, peer: &DeviceId) {
        self.outbound
            .retain(|_, pending| &pending.source != peer && &pending.target != peer);
        self.inbound
            .retain(|_, pending| &pending.source != peer && &pending.target != peer);
        let provisioning = self
            .provisioning
            .iter()
            .filter_map(|(id, session)| {
                (&session.source == peer || &session.target == peer).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in provisioning {
            self.provisioning.remove(&id);
            self.pending_revoke.insert(id);
        }
        self.target_sessions
            .retain(|_, session| &session.source != peer && &session.target != peer);
        let source_ids = self
            .source_sessions
            .iter()
            .filter_map(|(id, session)| {
                (&session.source == peer || &session.target == peer).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in source_ids {
            self.source_sessions.remove(&id);
            self.source.stop(id);
        }
        self.registry.revoke_peer(peer);
        if let Ok(mut adapter) = self.adapter.try_lock() {
            self.bridge.revoke_peer(peer, adapter.as_mut());
        } else {
            self.pending_bridge_peers.insert(peer.clone());
        }
    }

    pub(crate) fn teardown_all(&mut self) {
        self.outbound.clear();
        self.inbound.clear();
        self.target_sessions.clear();
        self.pending_revoke
            .extend(self.provisioning.keys().copied());
        self.pending_revoke
            .extend(self.completed_provisioning.keys().copied());
        self.provisioning.clear();
        self.completed_provisioning.clear();
        for session_id in std::mem::take(&mut self.source_sessions).into_keys() {
            self.source.stop(session_id);
        }
        self.source.stop_all();
        self.registry.revoke_all();
        if let Ok(mut adapter) = self.adapter.try_lock() {
            self.bridge.revoke_all(adapter.as_mut());
        } else {
            self.pending_bridge_revoke_all = true;
        }
    }
}

fn stop_action(session_id: [u8; 16], grant: RemoteCaptureGrant) -> PersonalFabricAction {
    PersonalFabricAction::RemoteCaptureSignal {
        signal: RemoteCaptureSignal::Stop { session_id, grant },
    }
}

fn random_id(mut exists: impl FnMut(&[u8; 16]) -> bool) -> [u8; 16] {
    loop {
        let mut id = [0; 16];
        OsRng.fill_bytes(&mut id);
        if id != [0; 16] && !exists(&id) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::remote_capture_adapter::{RemoteCaptureAdapterError, RemoteCaptureEndpoint};
    use crate::shell::remote_capture_device::{RemoteCaptureKind, RemoteVideoFormat};
    use crate::shell::remote_capture_source::RemoteCaptureSource;

    #[derive(Default)]
    struct FakeAdapter {
        ingested: usize,
        revoked: usize,
        fail_provision: bool,
    }

    impl RemoteCaptureAdapter for FakeAdapter {
        fn availability(&self) -> crate::shell::remote_capture_adapter::RemoteCaptureAvailability {
            crate::shell::remote_capture_adapter::RemoteCaptureAvailability {
                camera: true,
                microphone: true,
                scanner: true,
            }
        }

        fn provision(
            &mut self,
            _session_id: [u8; 16],
            _profile: &RemoteCaptureProfile,
        ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError> {
            if self.fail_provision {
                return Err(RemoteCaptureAdapterError::BackendUnavailable(
                    "test failure".into(),
                ));
            }
            Ok(RemoteCaptureEndpoint::PipeWireNode(7))
        }

        fn ingest(
            &mut self,
            _session_id: [u8; 16],
            _profile: &RemoteCaptureProfile,
            _chunk: &RemoteCaptureChunk,
        ) -> Result<(), RemoteCaptureAdapterError> {
            self.ingested += 1;
            Ok(())
        }

        fn revoke(&mut self, _session_id: [u8; 16]) {
            self.revoked += 1;
        }
    }

    #[derive(Default)]
    struct FakeSource {
        fail_start: bool,
    }

    impl RemoteCaptureSource for FakeSource {
        fn availability(&self) -> crate::shell::remote_capture_adapter::RemoteCaptureAvailability {
            crate::shell::remote_capture_adapter::RemoteCaptureAvailability {
                camera: true,
                microphone: true,
                scanner: true,
            }
        }

        fn start(
            &mut self,
            _session_id: [u8; 16],
            _profile: &RemoteCaptureProfile,
        ) -> Result<(), String> {
            if self.fail_start {
                Err("test failure".into())
            } else {
                Ok(())
            }
        }

        fn drain(&mut self) -> Vec<CapturedRemoteChunk> {
            Vec::new()
        }

        fn drain_failures(&mut self) -> Vec<[u8; 16]> {
            Vec::new()
        }

        fn stop(&mut self, _session_id: [u8; 16]) {}

        fn stop_all(&mut self) {}
    }

    fn app() -> VerifiedAppRegistration {
        VerifiedAppRegistration::new_verified("dev.tilo.Camera", "workspace-a").unwrap()
    }

    fn runtime() -> RemoteCaptureFabricRuntime {
        runtime_with(FakeAdapter::default(), FakeSource::default())
    }

    fn runtime_with(adapter: FakeAdapter, source: FakeSource) -> RemoteCaptureFabricRuntime {
        RemoteCaptureFabricRuntime::new(
            Arc::new(Mutex::new(CaptureBroker::new())),
            Box::new(adapter),
            Box::new(source),
        )
    }

    fn envelope(
        source: &DeviceId,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> PersonalFabricActionEnvelope {
        PersonalFabricActionEnvelope {
            request_id: [9; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 1,
            action,
        }
    }

    fn profile() -> RemoteCaptureProfile {
        RemoteCaptureProfile::Camera {
            format: RemoteVideoFormat::Nv12,
            width: 1280,
            height: 720,
            frames_per_second: 30,
        }
    }

    #[test]
    fn exact_route_consent_and_local_app_approval_gate_chunks() {
        let phone = DeviceId::new("phone").unwrap();
        let laptop = DeviceId::new("laptop").unwrap();
        let now = Instant::now();
        let mut target_runtime = runtime();
        let request_action = target_runtime
            .request(
                app(),
                phone.clone(),
                laptop.clone(),
                "Camera".into(),
                profile(),
            )
            .unwrap();
        let request_id = match &request_action {
            PersonalFabricAction::RemoteCaptureSignal {
                signal: RemoteCaptureSignal::Request(request),
            } => request.request_id,
            _ => unreachable!(),
        };

        let mut source_runtime = runtime();
        assert!(source_runtime.handle_action(&envelope(&laptop, &phone, request_action), now));
        assert_eq!(source_runtime.consents().len(), 1);
        let (decision_target, decision) = source_runtime.decide(request_id, true, now).unwrap();
        assert_eq!(decision_target, laptop);
        target_runtime.handle_action(&envelope(&phone, &laptop, decision), now);

        let (&session_id, source_session) = source_runtime.source_sessions.iter().next().unwrap();
        let chunk = RemoteCaptureChunk {
            session_id,
            grant: source_session.grant,
            sequence: 1,
            offset: 0,
            total_bytes: 4,
            page_index: None,
            end_of_unit: true,
            payload: vec![1, 2, 3, 4],
        };
        let (_, action) = source_runtime.send_chunk(chunk.clone(), now).unwrap();
        for _ in 0..100 {
            target_runtime.pump(now);
            if target_runtime.target_sessions.contains_key(&session_id) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(target_runtime.target_sessions.contains_key(&session_id));
        target_runtime.handle_action(&envelope(&phone, &laptop, action.clone()), now);
        assert_eq!(
            target_runtime.registry.phase(session_id),
            Some(crate::shell::remote_capture_device::RemoteCapturePhase::Granted)
        );
        target_runtime.approve_target(session_id, now).unwrap();
        target_runtime.handle_action(
            &envelope(&DeviceId::new("spoof").unwrap(), &laptop, action.clone()),
            now,
        );
        assert_eq!(
            target_runtime.registry.phase(session_id),
            Some(crate::shell::remote_capture_device::RemoteCapturePhase::Granted)
        );
        target_runtime.handle_action(&envelope(&phone, &laptop, action), now);
        assert_eq!(
            target_runtime.registry.phase(session_id),
            Some(crate::shell::remote_capture_device::RemoteCapturePhase::Streaming)
        );
    }

    #[test]
    fn peer_and_policy_teardown_remove_pending_and_live_authority() {
        let phone = DeviceId::new("phone").unwrap();
        let laptop = DeviceId::new("laptop").unwrap();
        let mut runtime = runtime();
        let action = runtime
            .request(app(), phone.clone(), laptop, "Camera".into(), profile())
            .unwrap();
        assert!(matches!(
            action,
            PersonalFabricAction::RemoteCaptureSignal { .. }
        ));
        runtime.teardown_peer(&phone);
        assert!(runtime.outbound.is_empty());
        runtime.teardown_all();
        assert!(runtime.consents().is_empty());
        assert_eq!(RemoteCaptureKind::Camera, profile().kind());
    }

    #[test]
    fn source_backend_failure_returns_a_denial_without_authority() {
        let phone = DeviceId::new("phone").unwrap();
        let laptop = DeviceId::new("laptop").unwrap();
        let now = Instant::now();
        let mut target = runtime();
        let request = target
            .request(
                app(),
                phone.clone(),
                laptop.clone(),
                "Camera".into(),
                profile(),
            )
            .unwrap();
        let request_id = match &request {
            PersonalFabricAction::RemoteCaptureSignal {
                signal: RemoteCaptureSignal::Request(request),
            } => request.request_id,
            _ => unreachable!(),
        };
        let mut source = runtime_with(FakeAdapter::default(), FakeSource { fail_start: true });
        source.handle_action(&envelope(&laptop, &phone, request), now);
        let (_, decision) = source.decide(request_id, true, now).unwrap();
        assert!(matches!(
            decision,
            PersonalFabricAction::RemoteCaptureSignal {
                signal: RemoteCaptureSignal::Decision {
                    accepted: false,
                    session_id: None,
                    grant: None,
                    ..
                }
            }
        ));
        assert!(source.source_sessions.is_empty());
    }

    #[test]
    fn target_adapter_failure_revokes_grant_and_stops_exact_source() {
        let phone = DeviceId::new("phone").unwrap();
        let laptop = DeviceId::new("laptop").unwrap();
        let now = Instant::now();
        let mut target = runtime_with(
            FakeAdapter {
                fail_provision: true,
                ..FakeAdapter::default()
            },
            FakeSource::default(),
        );
        let request = target
            .request(
                app(),
                phone.clone(),
                laptop.clone(),
                "Camera".into(),
                profile(),
            )
            .unwrap();
        let request_id = match &request {
            PersonalFabricAction::RemoteCaptureSignal {
                signal: RemoteCaptureSignal::Request(request),
            } => request.request_id,
            _ => unreachable!(),
        };
        let mut source = runtime();
        source.handle_action(&envelope(&laptop, &phone, request), now);
        let (_, decision) = source.decide(request_id, true, now).unwrap();
        target.handle_action(&envelope(&phone, &laptop, decision), now);
        for _ in 0..100 {
            target.pump(now);
            if !target.outgoing.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(target.target_sessions.is_empty());
        assert!(target.provisioning.is_empty());
        assert!(matches!(
            target.drain_outgoing().as_slice(),
            [(
                peer,
                PersonalFabricAction::RemoteCaptureSignal {
                    signal: RemoteCaptureSignal::Stop { .. }
                }
            )] if peer == &phone
        ));
    }
}

//! Certified-route orchestration for remote camera, microphone, and scanner sessions.
//!
//! This state machine correlates authenticated personal-fabric envelopes. It
//! never accepts app identity from an inbound decision or media chunk: the
//! verified local registration captured when the request was made remains the
//! authority used by the target-local adapter bridge.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rand_core::{OsRng, RngCore};

use crate::portal::CaptureBroker;
use crate::shell::device_fabric::{PersonalFabricAction, PersonalFabricActionEnvelope};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::realtime_session::VerifiedAppRegistration;
use crate::shell::remote_capture_adapter::{RemoteCaptureAdapter, RemoteCaptureTargetBridge};
use crate::shell::remote_capture_device::{
    MAX_REMOTE_CAPTURE_SESSIONS, REMOTE_CAPTURE_PROTOCOL_VERSION, RemoteCaptureChunk,
    RemoteCaptureGrant, RemoteCaptureProfile, RemoteCaptureRegistry, RemoteCaptureRequest,
    RemoteCaptureSignal,
};

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
}

pub(crate) struct RemoteCaptureFabricRuntime {
    registry: RemoteCaptureRegistry,
    bridge: RemoteCaptureTargetBridge,
    adapter: Box<dyn RemoteCaptureAdapter>,
    outbound: BTreeMap<[u8; 16], OutboundRequest>,
    inbound: BTreeMap<[u8; 16], InboundRequest>,
    target_sessions: BTreeMap<[u8; 16], TargetSession>,
    source_sessions: BTreeMap<[u8; 16], SourceSession>,
}

impl RemoteCaptureFabricRuntime {
    pub(crate) fn new(
        broker: Arc<Mutex<CaptureBroker>>,
        adapter: Box<dyn RemoteCaptureAdapter>,
    ) -> Self {
        Self {
            registry: RemoteCaptureRegistry::default(),
            bridge: RemoteCaptureTargetBridge::new(broker),
            adapter,
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
                if self
                    .bridge
                    .prepare(
                        &pending.app,
                        &pending.request,
                        pending.source.clone(),
                        pending.target.clone(),
                        session_id,
                        grant,
                        self.adapter.as_mut(),
                    )
                    .is_err()
                {
                    self.registry.revoke(session_id, grant);
                    return;
                }
                self.target_sessions.insert(
                    session_id,
                    TargetSession {
                        app: pending.app,
                        request: pending.request,
                        source: pending.source,
                        target: pending.target,
                        grant,
                    },
                );
            }
            RemoteCaptureSignal::Stop { session_id, grant } => {
                if let Some(session) = self.target_sessions.get(session_id)
                    && session.source == envelope.source
                    && session.target == envelope.target
                    && session.grant == *grant
                {
                    let session = self.target_sessions.remove(session_id).expect("present");
                    self.registry.revoke(*session_id, *grant);
                    let _ = self.bridge.revoke(
                        &session.app,
                        &session.source,
                        &session.target,
                        *session_id,
                        *grant,
                        self.adapter.as_mut(),
                    );
                } else if let Some(session) = self.source_sessions.get(session_id)
                    && session.target == envelope.source
                    && session.source == envelope.target
                    && session.grant == *grant
                {
                    self.source_sessions.remove(session_id);
                    self.registry.revoke(*session_id, *grant);
                    self.adapter.revoke(*session_id);
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
                let _ = self.bridge.append_media_chunk(
                    &session.app,
                    &session.source,
                    &session.target,
                    chunk,
                    self.adapter.as_mut(),
                );
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
        self.source_sessions.insert(
            session_id,
            SourceSession {
                request: pending.request.clone(),
                source: pending.source,
                target: pending.target.clone(),
                grant,
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
            self.adapter.revoke(id);
        }
        self.registry.revoke_peer(peer);
        self.bridge.revoke_peer(peer, self.adapter.as_mut());
    }

    pub(crate) fn teardown_all(&mut self) {
        self.outbound.clear();
        self.inbound.clear();
        self.target_sessions.clear();
        for session_id in std::mem::take(&mut self.source_sessions).into_keys() {
            self.adapter.revoke(session_id);
        }
        self.registry.revoke_all();
        self.bridge.revoke_all(self.adapter.as_mut());
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

    #[derive(Default)]
    struct FakeAdapter {
        ingested: usize,
        revoked: usize,
    }

    impl RemoteCaptureAdapter for FakeAdapter {
        fn provision(
            &mut self,
            _profile: &RemoteCaptureProfile,
        ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError> {
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

    fn app() -> VerifiedAppRegistration {
        VerifiedAppRegistration::new_verified("dev.tilo.Camera", "workspace-a").unwrap()
    }

    fn runtime() -> RemoteCaptureFabricRuntime {
        RemoteCaptureFabricRuntime::new(
            Arc::new(Mutex::new(CaptureBroker::new())),
            Box::new(FakeAdapter::default()),
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
}

//! Certified-route orchestration for remote camera, microphone, and scanner sessions.
//!
//! This state machine correlates authenticated personal-fabric envelopes. It
//! never accepts app identity from an inbound decision or media chunk: the
//! verified local registration captured when the request was made remains the
//! authority used by the target-local adapter bridge.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::Instant;

use crate::portal::CaptureBroker;
use crate::shell::device_fabric::{PersonalFabricAction, PersonalFabricActionEnvelope};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::realtime_session::VerifiedAppRegistration;
use crate::shell::remote_capture::adapter::{
    ProductionRemoteCaptureAdapter, RemoteCaptureAdapter, RemoteCaptureAdapterError,
    RemoteCaptureAvailability, RemoteCaptureEndpoint, RemoteCaptureTargetBridge,
    RemoteSessionBinding,
};
use crate::shell::remote_capture::device::{
    MAX_REMOTE_CAPTURE_SESSIONS, REMOTE_CAPTURE_PROTOCOL_VERSION, RemoteCaptureChunk,
    RemoteCaptureGrant, RemoteCaptureKind, RemoteCapturePhase, RemoteCaptureProfile,
    RemoteCaptureRegistry, RemoteCaptureRequest, RemoteCaptureSignal, grant_expiry,
};
use crate::shell::remote_capture::source::{
    CapturedRemoteChunk, ProductionRemoteCaptureSource, RemoteCaptureSource,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteCaptureConsentView {
    pub request_id: [u8; 16],
    pub peer: DeviceId,
    pub app_label: String,
    pub profile: RemoteCaptureProfile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteCaptureSessionPhase {
    Requesting,
    Provisioning,
    AppApprovalRequired,
    WaitingForReceiver,
    Streaming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteCaptureDegradedReason {
    SourceUnavailable,
    ReceiverUnavailable,
    TransferInterrupted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteCaptureDegradedView {
    pub peer: DeviceId,
    pub kind: RemoteCaptureKind,
    pub reason: RemoteCaptureDegradedReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteCaptureSessionView {
    pub session_id: Option<[u8; 16]>,
    pub peer: DeviceId,
    pub kind: RemoteCaptureKind,
    pub phase: RemoteCaptureSessionPhase,
    pub receiving: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RemoteCaptureView {
    pub local_sources: RemoteCaptureAvailability,
    pub local_receivers: RemoteCaptureAvailability,
    pub consents: Vec<RemoteCaptureConsentView>,
    pub sessions: Vec<RemoteCaptureSessionView>,
    pub degraded: Vec<RemoteCaptureDegradedView>,
}

pub(crate) type SharedRemoteCaptureRuntime = Arc<Mutex<RemoteCaptureFabricRuntime>>;

static ACTIVE_RUNTIME: OnceLock<Mutex<Option<Weak<Mutex<RemoteCaptureFabricRuntime>>>>> =
    OnceLock::new();

pub(crate) fn install_active_runtime(runtime: &SharedRemoteCaptureRuntime) {
    if let Ok(mut active) = ACTIVE_RUNTIME.get_or_init(|| Mutex::new(None)).lock() {
        *active = Some(Arc::downgrade(runtime));
    }
}

fn active_runtime() -> Result<SharedRemoteCaptureRuntime, String> {
    ACTIVE_RUNTIME
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| crate::shell::i18n::t("remote-capture-error-unavailable"))?
        .as_ref()
        .and_then(Weak::upgrade)
        .ok_or_else(|| crate::shell::i18n::t("remote-capture-error-unavailable"))
}

pub(crate) fn active_view() -> RemoteCaptureView {
    active_runtime()
        .and_then(|runtime| {
            runtime
                .try_lock()
                .map(|runtime| runtime.view())
                .map_err(|_| crate::shell::i18n::t("remote-capture-error-busy"))
        })
        .unwrap_or_default()
}

pub(crate) fn request_remote_capture(
    network: &crate::shell::network::ShellNetwork,
    app: VerifiedAppRegistration,
    peer: DeviceId,
    kind: RemoteCaptureKind,
) -> Result<(), String> {
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    let label = app.app_id().to_string();
    let runtime = active_runtime()?;
    let action = runtime
        .try_lock()
        .map_err(|_| crate::shell::i18n::t("remote-capture-error-busy"))?
        .request_standard(app, peer.clone(), local, label, kind)?;
    let request_id = match &action {
        PersonalFabricAction::RemoteCaptureSignal {
            signal: RemoteCaptureSignal::Request(request),
        } => request.request_id,
        _ => return Err("remote capture request produced an invalid action".into()),
    };
    if let Err(error) = network.send_personal_fabric_action(&peer, action) {
        if let Ok(mut runtime) = runtime.try_lock()
            && let Some(pending) = runtime.outbound.remove(&request_id)
        {
            runtime.note_degraded(
                peer,
                pending.request.profile.kind(),
                RemoteCaptureDegradedReason::TransferInterrupted,
            );
        }
        return Err(error);
    }
    Ok(())
}

pub(crate) fn decide_remote_capture(
    network: &crate::shell::network::ShellNetwork,
    request_id: [u8; 16],
    approved: bool,
) -> Result<(), String> {
    let runtime = active_runtime()?;
    let (peer, action) = runtime
        .try_lock()
        .map_err(|_| crate::shell::i18n::t("remote-capture-error-busy"))?
        .decide(request_id, approved, Instant::now())?;
    let session_id = match &action {
        PersonalFabricAction::RemoteCaptureSignal {
            signal:
                RemoteCaptureSignal::Decision {
                    accepted: true,
                    session_id,
                    ..
                },
        } => *session_id,
        _ => None,
    };
    if let Err(error) = network.send_personal_fabric_action(&peer, action) {
        if let Some(session_id) = session_id
            && let Ok(mut runtime) = runtime.try_lock()
        {
            let kind = runtime
                .source_sessions
                .get(&session_id)
                .map(|session| session.request.profile.kind());
            let _ = runtime.stop_session(session_id);
            if let Some(kind) = kind {
                runtime.note_degraded(peer, kind, RemoteCaptureDegradedReason::TransferInterrupted);
            }
        }
        return Err(error);
    }
    Ok(())
}

pub(crate) fn approve_remote_capture_target(session_id: [u8; 16]) -> Result<(), String> {
    active_runtime()?
        .try_lock()
        .map_err(|_| crate::shell::i18n::t("remote-capture-error-busy"))?
        .approve_target(session_id, Instant::now())
}

pub(crate) fn stop_remote_capture(
    network: &crate::shell::network::ShellNetwork,
    session_id: [u8; 16],
) -> Result<(), String> {
    let runtime = active_runtime()?;
    let (peer, action) = runtime
        .try_lock()
        .map_err(|_| crate::shell::i18n::t("remote-capture-error-busy"))?
        .stop_session(session_id)?;
    network.send_personal_fabric_action(&peer, action)?;
    Ok(())
}

struct OutboundRequest {
    app: VerifiedAppRegistration,
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
    expires_at: Instant,
}

struct InboundRequest {
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
    expires_at: Instant,
}

struct TargetSession {
    app: VerifiedAppRegistration,
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
    grant: RemoteCaptureGrant,
    expires_at: Instant,
}

struct SourceSession {
    request: RemoteCaptureRequest,
    source: DeviceId,
    target: DeviceId,
    grant: RemoteCaptureGrant,
    next_sequence: u64,
    expires_at: Instant,
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
    pending_bridge_apps: BTreeSet<(String, String)>,
    pending_bridge_revoke_all: bool,
    pending_target_revoke: BTreeMap<[u8; 16], TargetSession>,
    outgoing: Vec<(DeviceId, PersonalFabricAction)>,
    outbound: BTreeMap<[u8; 16], OutboundRequest>,
    inbound: BTreeMap<[u8; 16], InboundRequest>,
    target_sessions: BTreeMap<[u8; 16], TargetSession>,
    source_sessions: BTreeMap<[u8; 16], SourceSession>,
    degraded: VecDeque<RemoteCaptureDegradedView>,
}

impl RemoteCaptureFabricRuntime {
    /// This machine's capture, its programs and devices probed off the
    /// compositor thread ([`CaptureHost::watch`]).
    pub(crate) fn production(broker: Arc<Mutex<CaptureBroker>>) -> Self {
        let host = crate::shell::remote_capture::host::CaptureHost::watch();
        Self::new(
            broker,
            Box::new(ProductionRemoteCaptureAdapter::new(host.clone())),
            Box::new(ProductionRemoteCaptureSource::new(host)),
        )
    }

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
            pending_bridge_apps: BTreeSet::new(),
            pending_bridge_revoke_all: false,
            pending_target_revoke: BTreeMap::new(),
            outgoing: Vec::new(),
            outbound: BTreeMap::new(),
            inbound: BTreeMap::new(),
            target_sessions: BTreeMap::new(),
            source_sessions: BTreeMap::new(),
            degraded: VecDeque::new(),
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
            return Err(crate::shell::i18n::t("remote-capture-error-capacity"));
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
                expires_at: grant_expiry(Instant::now()),
            },
        );
        Ok(PersonalFabricAction::RemoteCaptureSignal {
            signal: RemoteCaptureSignal::Request(request),
        })
    }

    pub(crate) fn request_standard(
        &mut self,
        app: VerifiedAppRegistration,
        remote_source: DeviceId,
        local_target: DeviceId,
        app_label: String,
        kind: RemoteCaptureKind,
    ) -> Result<PersonalFabricAction, String> {
        let available = self
            .adapter
            .try_lock()
            .map_err(|_| crate::shell::i18n::t("remote-capture-error-receiver-busy"))?
            .availability();
        if !availability_supports(available, kind) {
            return Err(crate::shell::i18n::t("remote-capture-error-cannot-receive"));
        }
        self.request(
            app,
            remote_source,
            local_target,
            app_label,
            standard_profile(kind),
        )
    }

    pub(crate) fn view(&self) -> RemoteCaptureView {
        let local_receivers = self.adapter.try_lock().map_or_else(
            |_| RemoteCaptureAvailability::default(),
            |adapter| adapter.availability(),
        );
        let mut sessions = self
            .outbound
            .values()
            .map(|pending| RemoteCaptureSessionView {
                session_id: None,
                peer: pending.source.clone(),
                kind: pending.request.profile.kind(),
                phase: RemoteCaptureSessionPhase::Requesting,
                receiving: true,
            })
            .chain(
                self.provisioning
                    .iter()
                    .map(|(session_id, session)| RemoteCaptureSessionView {
                        session_id: Some(*session_id),
                        peer: session.source.clone(),
                        kind: session.request.profile.kind(),
                        phase: RemoteCaptureSessionPhase::Provisioning,
                        receiving: true,
                    }),
            )
            .chain(self.target_sessions.iter().map(|(session_id, session)| {
                RemoteCaptureSessionView {
                    session_id: Some(*session_id),
                    peer: session.source.clone(),
                    kind: session.request.profile.kind(),
                    phase: match self.registry.phase(*session_id) {
                        Some(RemoteCapturePhase::Streaming) => RemoteCaptureSessionPhase::Streaming,
                        _ => RemoteCaptureSessionPhase::AppApprovalRequired,
                    },
                    receiving: true,
                }
            }))
            .chain(self.source_sessions.iter().map(|(session_id, session)| {
                RemoteCaptureSessionView {
                    session_id: Some(*session_id),
                    peer: session.target.clone(),
                    kind: session.request.profile.kind(),
                    phase: match self.registry.phase(*session_id) {
                        Some(RemoteCapturePhase::Streaming) => RemoteCaptureSessionPhase::Streaming,
                        _ => RemoteCaptureSessionPhase::WaitingForReceiver,
                    },
                    receiving: false,
                }
            }))
            .collect::<Vec<_>>();
        sessions.truncate(MAX_REMOTE_CAPTURE_SESSIONS * 2);
        RemoteCaptureView {
            local_sources: self.source.availability(),
            local_receivers,
            consents: self.consents(),
            sessions,
            degraded: self.degraded.iter().cloned().collect(),
        }
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
    #[expect(
        clippy::too_many_lines,
        reason = "the capture pump keeps provisioning, capture and failure handling in one ordered tick"
    )]
    pub(crate) fn pump(&mut self, now: Instant) {
        self.expire_idle(now);
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
            if let Ok(endpoint) = endpoint {
                let Ok(mut adapter) = self.adapter.try_lock() else {
                    self.provisioning.insert(session_id, session);
                    self.completed_provisioning.insert(session_id, Ok(endpoint));
                    continue;
                };
                let prepared = self
                    .bridge
                    .prepare_endpoint(
                        &session.app,
                        &session.request,
                        RemoteSessionBinding {
                            source: session.source.clone(),
                            target: session.target.clone(),
                            session_id,
                            grant: session.grant,
                        },
                        endpoint,
                        adapter.as_mut(),
                    )
                    .is_ok();
                drop(adapter);
                if prepared {
                    self.target_sessions.insert(session_id, session);
                } else {
                    self.registry.revoke(session_id, session.grant);
                    self.note_degraded(
                        session.source.clone(),
                        session.request.profile.kind(),
                        RemoteCaptureDegradedReason::ReceiverUnavailable,
                    );
                    self.outgoing
                        .push((session.source, stop_action(session_id, session.grant)));
                }
            } else {
                self.registry.revoke(session_id, session.grant);
                self.note_degraded(
                    session.source.clone(),
                    session.request.profile.kind(),
                    RemoteCaptureDegradedReason::ReceiverUnavailable,
                );
                self.outgoing
                    .push((session.source, stop_action(session_id, session.grant)));
            }
        }

        if let Ok(mut adapter) = self.adapter.try_lock() {
            for (session_id, session) in std::mem::take(&mut self.pending_target_revoke) {
                let _ = self.bridge.revoke(
                    &session.app,
                    &session.source,
                    &session.target,
                    session_id,
                    session.grant,
                    adapter.as_mut(),
                );
                self.pending_revoke.remove(&session_id);
            }
            for session_id in std::mem::take(&mut self.pending_revoke) {
                adapter.revoke(session_id);
            }
            if self.pending_bridge_revoke_all {
                self.bridge.revoke_all(adapter.as_mut());
                self.pending_bridge_revoke_all = false;
                self.pending_bridge_peers.clear();
                self.pending_bridge_apps.clear();
            } else {
                for peer in std::mem::take(&mut self.pending_bridge_peers) {
                    self.bridge.revoke_peer(&peer, adapter.as_mut());
                }
                for (app_id, workspace_id) in std::mem::take(&mut self.pending_bridge_apps) {
                    self.bridge
                        .revoke_app(&app_id, &workspace_id, adapter.as_mut());
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
                self.note_degraded(
                    session.target.clone(),
                    session.request.profile.kind(),
                    RemoteCaptureDegradedReason::TransferInterrupted,
                );
                self.outgoing
                    .push((session.target, stop_action(session_id, session.grant)));
            }
        }
    }

    pub(crate) fn drain_outgoing(&mut self) -> Vec<(DeviceId, PersonalFabricAction)> {
        std::mem::take(&mut self.outgoing)
    }

    pub(crate) fn outgoing_failed(&mut self, peer: &DeviceId, action: &PersonalFabricAction) {
        let PersonalFabricAction::RemoteCaptureChunk { chunk } = action else {
            return;
        };
        if !self
            .source_sessions
            .get(&chunk.session_id)
            .is_some_and(|session| &session.target == peer && session.grant == chunk.grant)
        {
            return;
        }
        let Some(session) = self.source_sessions.remove(&chunk.session_id) else {
            return;
        };
        self.source.stop(chunk.session_id);
        self.registry.revoke(chunk.session_id, session.grant);
        self.note_degraded(
            peer.clone(),
            session.request.profile.kind(),
            RemoteCaptureDegradedReason::TransferInterrupted,
        );
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
            let kind = session.request.profile.kind();
            self.source.stop(captured.session_id);
            self.source_sessions.remove(&captured.session_id);
            self.registry.revoke(captured.session_id, grant);
            self.note_degraded(
                target.clone(),
                kind,
                RemoteCaptureDegradedReason::TransferInterrupted,
            );
            self.outgoing
                .push((target, stop_action(captured.session_id, grant)));
            return;
        }
        self.outgoing.push((
            session.target.clone(),
            PersonalFabricAction::RemoteCaptureChunk { chunk },
        ));
        session.expires_at = grant_expiry(now);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one handler validates each remote-capture signal beside its state change"
    )]
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
                        expires_at: grant_expiry(now),
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
                    expires_at: grant_expiry(now),
                };
                let profile = target_session.request.profile.clone();
                self.provisioning.insert(session_id, target_session);
                let adapter = Arc::clone(&self.adapter);
                let tx = self.provision_tx.clone();
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
                if self.target_sessions.get(session_id).is_some_and(|session| {
                    session.source == envelope.source
                        && session.target == envelope.target
                        && session.grant == *grant
                }) && let Some(session) = self.target_sessions.remove(session_id)
                {
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
                        self.pending_target_revoke.insert(*session_id, session);
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
            crate::shell::remote_capture::device::RemoteCaptureKind::DocumentScanner => {}
            crate::shell::remote_capture::device::RemoteCaptureKind::Camera
            | crate::shell::remote_capture::device::RemoteCaptureKind::Microphone => {
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
        if let Some(session) = self.target_sessions.get_mut(&chunk.session_id) {
            session.expires_at = grant_expiry(now);
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
            .ok_or_else(|| crate::shell::i18n::t("remote-capture-error-unknown-request"))?;
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
            self.note_degraded(
                pending.target.clone(),
                pending.request.profile.kind(),
                RemoteCaptureDegradedReason::SourceUnavailable,
            );
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
                expires_at: grant_expiry(now),
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
            .ok_or_else(|| crate::shell::i18n::t("remote-capture-error-unknown-session"))?;
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

    pub(crate) fn stop_session(
        &mut self,
        session_id: [u8; 16],
    ) -> Result<(DeviceId, PersonalFabricAction), String> {
        if let Some(session) = self.target_sessions.remove(&session_id) {
            self.registry.revoke(session_id, session.grant);
            if let Ok(mut adapter) = self.adapter.try_lock() {
                let _ = self.bridge.revoke(
                    &session.app,
                    &session.source,
                    &session.target,
                    session_id,
                    session.grant,
                    adapter.as_mut(),
                );
            } else {
                self.pending_revoke.insert(session_id);
                let stop = (
                    session.source.clone(),
                    stop_action(session_id, session.grant),
                );
                self.pending_target_revoke.insert(session_id, session);
                return Ok(stop);
            }
            return Ok((session.source, stop_action(session_id, session.grant)));
        }
        if let Some(session) = self.source_sessions.remove(&session_id) {
            self.source.stop(session_id);
            self.registry.revoke(session_id, session.grant);
            return Ok((session.target, stop_action(session_id, session.grant)));
        }
        Err(crate::shell::i18n::t(
            "remote-capture-error-unknown-session",
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
        self.pending_target_revoke
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

    /// Revoke receiver-side authority when its locally verified app loses
    /// focus. Source sessions belong to a remote requester and are therefore
    /// intentionally unaffected.
    pub(crate) fn teardown_target_app(&mut self, app_id: &str, workspace_id: &str) {
        self.outbound.retain(|_, pending| {
            pending.app.app_id() != app_id || pending.app.workspace_id() != workspace_id
        });

        let provisioning = self
            .provisioning
            .iter()
            .filter_map(|(id, session)| {
                (session.app.app_id() == app_id && session.app.workspace_id() == workspace_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for session_id in provisioning {
            if let Some(session) = self.provisioning.remove(&session_id) {
                self.completed_provisioning.remove(&session_id);
                self.pending_revoke.insert(session_id);
                self.outgoing
                    .push((session.source, stop_action(session_id, session.grant)));
            }
        }

        let targets = self
            .target_sessions
            .iter()
            .filter_map(|(id, session)| {
                (session.app.app_id() == app_id && session.app.workspace_id() == workspace_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for session_id in targets {
            if let Some(session) = self.target_sessions.remove(&session_id) {
                self.outgoing
                    .push((session.source, stop_action(session_id, session.grant)));
            }
        }

        self.pending_target_revoke.retain(|_, session| {
            session.app.app_id() != app_id || session.app.workspace_id() != workspace_id
        });
        self.registry.revoke_app(app_id, workspace_id);
        if let Ok(mut adapter) = self.adapter.try_lock() {
            self.bridge
                .revoke_app(app_id, workspace_id, adapter.as_mut());
        } else {
            self.pending_bridge_apps
                .insert((app_id.to_string(), workspace_id.to_string()));
        }
    }

    pub(crate) fn teardown_all(&mut self) {
        self.outbound.clear();
        self.inbound.clear();
        self.target_sessions.clear();
        self.pending_target_revoke.clear();
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

    fn expire_idle(&mut self, now: Instant) {
        self.outbound.retain(|_, pending| pending.expires_at > now);
        self.inbound.retain(|_, pending| pending.expires_at > now);

        let provisioning = self
            .provisioning
            .iter()
            .filter_map(|(id, session)| (session.expires_at <= now).then_some(*id))
            .collect::<Vec<_>>();
        for session_id in provisioning {
            if let Some(session) = self.provisioning.remove(&session_id) {
                self.registry.revoke(session_id, session.grant);
                self.pending_revoke.insert(session_id);
                self.note_degraded(
                    session.source,
                    session.request.profile.kind(),
                    RemoteCaptureDegradedReason::TransferInterrupted,
                );
            }
        }

        let targets = self
            .target_sessions
            .iter()
            .filter_map(|(id, session)| (session.expires_at <= now).then_some(*id))
            .collect::<Vec<_>>();
        for session_id in targets {
            if let Some(session) = self.target_sessions.remove(&session_id) {
                self.registry.revoke(session_id, session.grant);
                if let Ok(mut adapter) = self.adapter.try_lock() {
                    let _ = self.bridge.revoke(
                        &session.app,
                        &session.source,
                        &session.target,
                        session_id,
                        session.grant,
                        adapter.as_mut(),
                    );
                } else {
                    self.pending_revoke.insert(session_id);
                    self.pending_target_revoke.insert(session_id, session);
                }
            }
        }

        let sources = self
            .source_sessions
            .iter()
            .filter_map(|(id, session)| (session.expires_at <= now).then_some(*id))
            .collect::<Vec<_>>();
        for session_id in sources {
            if let Some(session) = self.source_sessions.remove(&session_id) {
                self.source.stop(session_id);
                self.registry.revoke(session_id, session.grant);
            }
        }
    }

    fn note_degraded(
        &mut self,
        peer: DeviceId,
        kind: RemoteCaptureKind,
        reason: RemoteCaptureDegradedReason,
    ) {
        while self.degraded.len() >= MAX_REMOTE_CAPTURE_SESSIONS {
            self.degraded.pop_front();
        }
        self.degraded
            .push_back(RemoteCaptureDegradedView { peer, kind, reason });
    }
}

fn stop_action(session_id: [u8; 16], grant: RemoteCaptureGrant) -> PersonalFabricAction {
    PersonalFabricAction::RemoteCaptureSignal {
        signal: RemoteCaptureSignal::Stop { session_id, grant },
    }
}

fn standard_profile(kind: RemoteCaptureKind) -> RemoteCaptureProfile {
    match kind {
        RemoteCaptureKind::Camera => RemoteCaptureProfile::Camera {
            format: crate::shell::remote_capture::device::RemoteVideoFormat::Nv12,
            width: 160,
            height: 120,
            frames_per_second: 15,
        },
        RemoteCaptureKind::Microphone => RemoteCaptureProfile::Microphone {
            format: crate::shell::remote_capture::device::RemoteAudioFormat::PcmS16Le,
            sample_rate: 48_000,
            channels: 2,
            frame_millis: 20,
        },
        RemoteCaptureKind::DocumentScanner => RemoteCaptureProfile::DocumentScanner {
            format: crate::shell::remote_capture::device::RemoteDocumentFormat::Png,
            max_pages: 1,
            max_document_bytes: 16_777_216,
        },
    }
}

fn availability_supports(availability: RemoteCaptureAvailability, kind: RemoteCaptureKind) -> bool {
    match kind {
        RemoteCaptureKind::Camera => availability.camera,
        RemoteCaptureKind::Microphone => availability.microphone,
        RemoteCaptureKind::DocumentScanner => availability.scanner,
    }
}

fn random_id(mut exists: impl FnMut(&[u8; 16]) -> bool) -> [u8; 16] {
    loop {
        let id = crate::random::bytes::<16>();
        if id != [0; 16] && !exists(&id) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::remote_capture::adapter::{RemoteCaptureAdapterError, RemoteCaptureEndpoint};
    use crate::shell::remote_capture::device::{
        REMOTE_CAPTURE_GRANT_TTL, RemoteCaptureKind, RemoteVideoFormat,
    };
    use crate::shell::remote_capture::source::RemoteCaptureSource;

    #[derive(Default)]
    struct FakeAdapter {
        ingested: usize,
        revoked: usize,
        fail_provision: bool,
    }

    impl RemoteCaptureAdapter for FakeAdapter {
        fn availability(&self) -> crate::shell::remote_capture::adapter::RemoteCaptureAvailability {
            crate::shell::remote_capture::adapter::RemoteCaptureAvailability {
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
            self.ingested = self.ingested.saturating_add(1);
            Ok(())
        }

        fn revoke(&mut self, _session_id: [u8; 16]) {
            self.revoked = self.revoked.saturating_add(1);
        }
    }

    struct FakeSource {
        fail_start: bool,
        chunks: Arc<Mutex<Vec<CapturedRemoteChunk>>>,
    }

    impl Default for FakeSource {
        fn default() -> Self {
            Self {
                fail_start: false,
                chunks: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl RemoteCaptureSource for FakeSource {
        fn availability(&self) -> crate::shell::remote_capture::adapter::RemoteCaptureAvailability {
            crate::shell::remote_capture::adapter::RemoteCaptureAvailability {
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
            std::mem::take(&mut *self.chunks.lock().unwrap())
        }

        fn drain_failures(&mut self) -> Vec<[u8; 16]> {
            Vec::new()
        }

        fn stop(&mut self, _session_id: [u8; 16]) {}

        fn stop_all(&mut self) {}
    }

    fn app() -> VerifiedAppRegistration {
        VerifiedAppRegistration::new_verified("so.tilo.Camera", "workspace-a").unwrap()
    }

    fn other_app() -> VerifiedAppRegistration {
        VerifiedAppRegistration::new_verified("so.tilo.Calls", "workspace-a").unwrap()
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn capture_runtime_gates_chunks_on_consent_approval_and_bounded_profiles() {
        {
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

            let captured = Arc::new(Mutex::new(Vec::new()));
            let mut source_runtime = runtime_with(
                FakeAdapter::default(),
                FakeSource {
                    fail_start: false,
                    chunks: Arc::clone(&captured),
                },
            );
            assert!(source_runtime.handle_action(&envelope(&laptop, &phone, request_action), now));
            assert_eq!(source_runtime.consents().len(), 1);
            let (decision_target, decision) = source_runtime.decide(request_id, true, now).unwrap();
            assert_eq!(decision_target, laptop);
            target_runtime.handle_action(&envelope(&phone, &laptop, decision), now);

            let (&session_id, source_grant) = source_runtime
                .source_sessions
                .iter()
                .next()
                .map(|(session_id, session)| (session_id, session.grant))
                .unwrap();
            captured.lock().unwrap().push(CapturedRemoteChunk {
                session_id,
                offset: 0,
                total_bytes: 4,
                page_index: None,
                end_of_unit: true,
                payload: vec![1, 2, 3, 4],
            });
            source_runtime.pump(now);
            let (action_target, action) = source_runtime.drain_outgoing().pop().unwrap();
            assert_eq!(action_target, laptop);
            let PersonalFabricAction::RemoteCaptureChunk { chunk } = &action else {
                panic!("source pump did not publish its captured chunk");
            };
            assert_eq!(chunk.grant, source_grant);
            assert_eq!(chunk.sequence, 1);
            let deadline = tilo_test_env::hang_deadline();
            while std::time::Instant::now() < deadline {
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
                Some(crate::shell::remote_capture::device::RemoteCapturePhase::Granted)
            );
            target_runtime.approve_target(session_id, now).unwrap();
            target_runtime.handle_action(
                &envelope(&DeviceId::new("spoof").unwrap(), &laptop, action.clone()),
                now,
            );
            assert_eq!(
                target_runtime.registry.phase(session_id),
                Some(crate::shell::remote_capture::device::RemoteCapturePhase::Granted)
            );
            target_runtime.handle_action(&envelope(&phone, &laptop, action), now);
            assert_eq!(
                target_runtime.registry.phase(session_id),
                Some(crate::shell::remote_capture::device::RemoteCapturePhase::Streaming)
            );
        }

        {
            let phone = DeviceId::new("phone").unwrap();
            let laptop = DeviceId::new("laptop").unwrap();
            let mut runtime = runtime();
            runtime
                .request(app(), phone, laptop, "Camera".into(), profile())
                .unwrap();
            assert_eq!(runtime.outbound.len(), 1);

            runtime.pump(Instant::now() + REMOTE_CAPTURE_GRANT_TTL);

            assert!(runtime.outbound.is_empty());
            assert!(runtime.source_sessions.is_empty());
            assert!(runtime.target_sessions.is_empty());
        }

        {
            for kind in [
                RemoteCaptureKind::Camera,
                RemoteCaptureKind::Microphone,
                RemoteCaptureKind::DocumentScanner,
            ] {
                assert!(standard_profile(kind).valid());
            }
            let phone = DeviceId::new("phone").unwrap();
            let laptop = DeviceId::new("laptop").unwrap();
            let mut runtime = runtime();
            runtime
                .request_standard(
                    app(),
                    phone.clone(),
                    laptop,
                    "Camera".into(),
                    RemoteCaptureKind::Camera,
                )
                .unwrap();
            let view = runtime.view();
            assert_eq!(view.sessions.len(), 1);
            assert_eq!(view.sessions[0].peer, phone);
            assert_eq!(view.sessions[0].session_id, None);
            assert_eq!(
                view.sessions[0].phase,
                RemoteCaptureSessionPhase::Requesting
            );
            assert!(view.local_sources.camera);
            assert!(view.local_receivers.camera);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn capture_runtime_teardown_revokes_exactly_the_affected_authority() {
        {
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

        {
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
            let mut source = runtime();
            source.handle_action(&envelope(&laptop, &phone, request), now);
            let (_, decision) = source.decide(request_id, true, now).unwrap();
            target.handle_action(&envelope(&phone, &laptop, decision), now);

            let deadline = tilo_test_env::hang_deadline();
            while std::time::Instant::now() < deadline {
                target.pump(now);
                if !target.target_sessions.is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let (&session_id, grant) = target
                .target_sessions
                .iter()
                .next()
                .map(|(id, session)| (id, session.grant))
                .expect("target endpoint was provisioned");
            target
                .request(
                    other_app(),
                    phone.clone(),
                    laptop,
                    "Calls".into(),
                    profile(),
                )
                .unwrap();

            target.teardown_target_app("so.tilo.Camera", "workspace-a");

            assert!(target.target_sessions.is_empty());
            assert_eq!(target.registry.phase(session_id), None);
            assert_eq!(target.outbound.len(), 1);
            assert_eq!(
                target.outbound.values().next().unwrap().app.app_id(),
                "so.tilo.Calls"
            );
            assert!(target.approve_target(session_id, now).is_err());
            assert!(matches!(
                target.drain_outgoing().as_slice(),
                [(
                    peer,
                    PersonalFabricAction::RemoteCaptureSignal {
                        signal: RemoteCaptureSignal::Stop {
                            session_id: stopped,
                            grant: stopped_grant
                        }
                    }
                )] if peer == &phone && stopped == &session_id && stopped_grant == &grant
            ));
        }

        {
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
            let mut source = runtime_with(
                FakeAdapter::default(),
                FakeSource {
                    fail_start: true,
                    ..FakeSource::default()
                },
            );
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
            assert_eq!(
                source.view().degraded[0].reason,
                RemoteCaptureDegradedReason::SourceUnavailable
            );
        }

        {
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
            let deadline = tilo_test_env::hang_deadline();
            while std::time::Instant::now() < deadline {
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
            assert_eq!(
                target.view().degraded[0].reason,
                RemoteCaptureDegradedReason::ReceiverUnavailable
            );
        }
    }
}

//! Target-local adapters for certified-device camera, microphone, and scanner input.
//!
//! Fabric messages stop at [`RemoteCaptureProfile`]. PipeWire node identifiers,
//! V4L2 file descriptors, scanner files, and the fresh app-consent capability in
//! this module are process-local and deliberately have no serialization traits.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rand_core::{OsRng, RngCore};
use thiserror::Error;

use crate::portal::{
    CaptureAuthorizationId, CaptureBroker, CaptureKind, CaptureLeaseId, CaptureSessionBinding,
    LocalCaptureHandle,
};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::realtime_session::VerifiedAppRegistration;
use crate::shell::remote_capture_device::{
    MAX_REMOTE_CAPTURE_SESSIONS, RemoteCaptureChunk, RemoteCaptureGrant, RemoteCaptureKind,
    RemoteCaptureProfile, RemoteCaptureRequest,
};

/// A provisioned, target-local media endpoint. These values must never enter a
/// fabric action, collaborative document, log, or app-visible resource.
pub(crate) enum RemoteCaptureEndpoint {
    PipeWireNode(u32),
    V4l2Device(OwnedFd),
    DocumentScanner,
}

/// Injection seam implemented by the target's PipeWire/V4L2 backend.
pub(crate) trait RemoteCaptureAdapter {
    fn provision(
        &mut self,
        profile: &RemoteCaptureProfile,
    ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError>;

    fn revoke(&mut self, _session_id: [u8; 16]) {}
}

/// Fresh, single-use proof that the target user approved one exact local app
/// for one exact remote input session. It is target-process local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RemoteCaptureApproval([u8; 32]);

struct ScannerSpool {
    file: File,
    bytes: u64,
    max_bytes: u64,
    last_page: Option<u32>,
}

enum PreparedEndpoint {
    Media,
    Scanner(ScannerSpool),
}

struct LocalSession {
    source: DeviceId,
    target: DeviceId,
    app_id: String,
    workspace_id: String,
    profile: RemoteCaptureProfile,
    grant: RemoteCaptureGrant,
    binding: CaptureSessionBinding,
    source_id: Option<String>,
    endpoint: PreparedEndpoint,
    approval: Option<(RemoteCaptureApproval, Option<CaptureAuthorizationId>)>,
    lease: Option<CaptureLeaseId>,
}

/// Owns only target-local state. The caller remains responsible for validating
/// every received chunk in `RemoteCaptureRegistry` before appending it here.
pub(crate) struct RemoteCaptureTargetBridge {
    broker: Arc<Mutex<CaptureBroker>>,
    sessions: BTreeMap<[u8; 16], LocalSession>,
}

impl RemoteCaptureTargetBridge {
    pub(crate) fn new(broker: Arc<Mutex<CaptureBroker>>) -> Self {
        Self {
            broker,
            sessions: BTreeMap::new(),
        }
    }

    /// Provision a target-local endpoint without granting any app access.
    pub(crate) fn prepare(
        &mut self,
        app: &VerifiedAppRegistration,
        request: &RemoteCaptureRequest,
        source: DeviceId,
        target: DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
        adapter: &mut dyn RemoteCaptureAdapter,
    ) -> Result<(), RemoteCaptureAdapterError> {
        if !request.valid()
            || request.app_id != app.app_id()
            || request.workspace_id != app.workspace_id()
            || session_id == [0; 16]
            || !grant.valid()
            || source == target
            || self.sessions.contains_key(&session_id)
            || self.sessions.len() >= MAX_REMOTE_CAPTURE_SESSIONS
        {
            return Err(RemoteCaptureAdapterError::BindingMismatch);
        }

        let endpoint = adapter.provision(&request.profile)?;
        if !endpoint_matches(&endpoint, request.profile.kind()) {
            adapter.revoke(session_id);
            return Err(RemoteCaptureAdapterError::WrongEndpoint);
        }
        let binding =
            CaptureSessionBinding::from_verified(app, hex::encode(session_id), target.as_str())
                .map_err(|_| RemoteCaptureAdapterError::BindingMismatch)?;
        let (source_id, endpoint) = match endpoint {
            RemoteCaptureEndpoint::PipeWireNode(node) => {
                let id = local_source_id(session_id);
                self.register_source(&id, request, LocalCaptureHandle::PipeWireNode(node))?;
                (Some(id), PreparedEndpoint::Media)
            }
            RemoteCaptureEndpoint::V4l2Device(fd) => {
                let id = local_source_id(session_id);
                self.register_source(&id, request, LocalCaptureHandle::FileDescriptor(fd))?;
                (Some(id), PreparedEndpoint::Media)
            }
            RemoteCaptureEndpoint::DocumentScanner => {
                let RemoteCaptureProfile::DocumentScanner {
                    max_document_bytes, ..
                } = request.profile
                else {
                    unreachable!("endpoint kind was checked above")
                };
                let file = tempfile::tempfile().map_err(RemoteCaptureAdapterError::Spool)?;
                (
                    None,
                    PreparedEndpoint::Scanner(ScannerSpool {
                        file,
                        bytes: 0,
                        max_bytes: max_document_bytes,
                        last_page: None,
                    }),
                )
            }
        };
        self.sessions.insert(
            session_id,
            LocalSession {
                source,
                target,
                app_id: app.app_id().to_string(),
                workspace_id: app.workspace_id().to_string(),
                profile: request.profile.clone(),
                grant,
                binding,
                source_id,
                endpoint,
                approval: None,
                lease: None,
            },
        );
        Ok(())
    }

    /// Mint fresh app consent for a prepared endpoint. A second approval
    /// replaces and revokes the previous unconsumed proof.
    pub(crate) fn approve(
        &mut self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
        now: Instant,
    ) -> Result<RemoteCaptureApproval, RemoteCaptureAdapterError> {
        let broker = Arc::clone(&self.broker);
        let session = self.exact_session_mut(app, source, target, session_id, grant)?;
        if session.lease.is_some() {
            return Err(RemoteCaptureAdapterError::AlreadyActive);
        }
        let capture_authorization = if let Some(source_id) = session.source_id.as_deref() {
            let kind = capture_kind(session.profile.kind())?;
            Some(
                broker
                    .lock()
                    .map_err(|_| RemoteCaptureAdapterError::BrokerUnavailable)?
                    .authorize(&session.binding, source_id, kind, now)
                    .map_err(|error| RemoteCaptureAdapterError::Broker(error.to_string()))?,
            )
        } else {
            None
        };
        if let Some((_, old)) = session
            .approval
            .replace((mint_approval(), capture_authorization))
        {
            if let Some(old) = old {
                let _ = broker
                    .lock()
                    .map(|mut broker| broker.revoke_authorizations(&[old]));
            }
        }
        Ok(session.approval.expect("approval was inserted").0)
    }

    /// Consume the exact approval and make the endpoint available to the app.
    pub(crate) fn activate(
        &mut self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
        approval: RemoteCaptureApproval,
        now: Instant,
    ) -> Result<(), RemoteCaptureAdapterError> {
        let broker = Arc::clone(&self.broker);
        let session = self.exact_session_mut(app, source, target, session_id, grant)?;
        let Some((expected, authorization)) = session.approval.take() else {
            return Err(RemoteCaptureAdapterError::ApprovalRequired);
        };
        if expected != approval {
            session.approval = Some((expected, authorization));
            return Err(RemoteCaptureAdapterError::ApprovalRequired);
        }
        if let Some(authorization) = authorization {
            let source_id = session
                .source_id
                .as_deref()
                .ok_or(RemoteCaptureAdapterError::WrongEndpoint)?;
            let kind = capture_kind(session.profile.kind())?;
            let lease = broker
                .lock()
                .map_err(|_| RemoteCaptureAdapterError::BrokerUnavailable)?
                .acquire(authorization, &session.binding, source_id, kind, now)
                .map_err(|error| RemoteCaptureAdapterError::Broker(error.to_string()))?;
            session.lease = Some(lease);
        }
        Ok(())
    }

    /// Append one registry-validated scanner chunk to an unnamed, bounded file.
    pub(crate) fn append_scanner_chunk(
        &mut self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        chunk: &RemoteCaptureChunk,
    ) -> Result<(), RemoteCaptureAdapterError> {
        let session = self.exact_session_mut(app, source, target, chunk.session_id, chunk.grant)?;
        if session.approval.is_some() || !matches!(session.endpoint, PreparedEndpoint::Scanner(_)) {
            return Err(RemoteCaptureAdapterError::ApprovalRequired);
        }
        let PreparedEndpoint::Scanner(spool) = &mut session.endpoint else {
            return Err(RemoteCaptureAdapterError::WrongEndpoint);
        };
        if !chunk.valid_for(RemoteCaptureKind::DocumentScanner)
            || chunk.offset != spool.bytes
            || chunk.total_bytes > spool.max_bytes
            || chunk.page_index < spool.last_page
        {
            return Err(RemoteCaptureAdapterError::InvalidChunk);
        }
        spool
            .file
            .seek(SeekFrom::Start(spool.bytes))
            .and_then(|_| spool.file.write_all(&chunk.payload))
            .map_err(RemoteCaptureAdapterError::Spool)?;
        spool.bytes = spool
            .bytes
            .checked_add(chunk.payload.len() as u64)
            .ok_or(RemoteCaptureAdapterError::InvalidChunk)?;
        spool.last_page = chunk.page_index;
        Ok(())
    }

    /// Atomically revoke broker authority and drop the unnamed scanner spool.
    pub(crate) fn revoke(
        &mut self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
        adapter: &mut dyn RemoteCaptureAdapter,
    ) -> Result<(), RemoteCaptureAdapterError> {
        self.exact_session(app, source, target, session_id, grant)?;
        let session = self
            .sessions
            .remove(&session_id)
            .ok_or(RemoteCaptureAdapterError::BindingMismatch)?;
        self.revoke_local(&session);
        adapter.revoke(session_id);
        Ok(())
    }

    pub(crate) fn revoke_peer(
        &mut self,
        peer: &DeviceId,
        adapter: &mut dyn RemoteCaptureAdapter,
    ) -> usize {
        self.revoke_matching(adapter, |session| {
            &session.source == peer || &session.target == peer
        })
    }

    pub(crate) fn revoke_app(
        &mut self,
        app_id: &str,
        workspace_id: &str,
        adapter: &mut dyn RemoteCaptureAdapter,
    ) -> usize {
        self.revoke_matching(adapter, |session| {
            session.app_id == app_id && session.workspace_id == workspace_id
        })
    }

    pub(crate) fn revoke_all(&mut self, adapter: &mut dyn RemoteCaptureAdapter) -> usize {
        self.revoke_matching(adapter, |_| true)
    }

    fn register_source(
        &self,
        source_id: &str,
        request: &RemoteCaptureRequest,
        handle: LocalCaptureHandle,
    ) -> Result<(), RemoteCaptureAdapterError> {
        self.broker
            .lock()
            .map_err(|_| RemoteCaptureAdapterError::BrokerUnavailable)?
            .register_source(
                source_id,
                format!("{} on certified device", request.app_label),
                capture_kind(request.profile.kind())?,
                handle,
            )
            .map_err(|error| RemoteCaptureAdapterError::Broker(error.to_string()))
    }

    fn exact_session(
        &self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
    ) -> Result<&LocalSession, RemoteCaptureAdapterError> {
        self.sessions
            .get(&session_id)
            .filter(|session| exact(session, app, source, target, grant))
            .ok_or(RemoteCaptureAdapterError::BindingMismatch)
    }

    fn exact_session_mut(
        &mut self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
    ) -> Result<&mut LocalSession, RemoteCaptureAdapterError> {
        self.sessions
            .get_mut(&session_id)
            .filter(|session| exact(session, app, source, target, grant))
            .ok_or(RemoteCaptureAdapterError::BindingMismatch)
    }

    fn revoke_local(&self, session: &LocalSession) {
        if let Ok(mut broker) = self.broker.lock() {
            broker.session_ended(&session.binding);
            if let Some(source_id) = session.source_id.as_deref() {
                broker.remove_source(source_id);
            }
        }
    }

    fn revoke_matching(
        &mut self,
        adapter: &mut dyn RemoteCaptureAdapter,
        predicate: impl Fn(&LocalSession) -> bool,
    ) -> usize {
        let ids = self
            .sessions
            .iter()
            .filter_map(|(id, session)| predicate(session).then_some(*id))
            .collect::<Vec<_>>();
        for id in &ids {
            if let Some(session) = self.sessions.remove(id) {
                self.revoke_local(&session);
                adapter.revoke(*id);
            }
        }
        ids.len()
    }
}

fn exact(
    session: &LocalSession,
    app: &VerifiedAppRegistration,
    source: &DeviceId,
    target: &DeviceId,
    grant: RemoteCaptureGrant,
) -> bool {
    session.app_id == app.app_id()
        && session.workspace_id == app.workspace_id()
        && &session.source == source
        && &session.target == target
        && session.grant == grant
}

fn endpoint_matches(endpoint: &RemoteCaptureEndpoint, kind: RemoteCaptureKind) -> bool {
    matches!(
        (endpoint, kind),
        (
            RemoteCaptureEndpoint::PipeWireNode(_),
            RemoteCaptureKind::Camera
        ) | (
            RemoteCaptureEndpoint::PipeWireNode(_),
            RemoteCaptureKind::Microphone
        ) | (
            RemoteCaptureEndpoint::V4l2Device(_),
            RemoteCaptureKind::Camera
        ) | (
            RemoteCaptureEndpoint::DocumentScanner,
            RemoteCaptureKind::DocumentScanner
        )
    )
}

fn capture_kind(kind: RemoteCaptureKind) -> Result<CaptureKind, RemoteCaptureAdapterError> {
    match kind {
        RemoteCaptureKind::Camera => Ok(CaptureKind::Camera),
        RemoteCaptureKind::Microphone => Ok(CaptureKind::Microphone),
        RemoteCaptureKind::DocumentScanner => Err(RemoteCaptureAdapterError::WrongEndpoint),
    }
}

fn local_source_id(session_id: [u8; 16]) -> String {
    format!("remote-capture-{}", hex::encode(session_id))
}

fn mint_approval() -> RemoteCaptureApproval {
    let mut value = [0; 32];
    while value == [0; 32] {
        OsRng.fill_bytes(&mut value);
    }
    RemoteCaptureApproval(value)
}

#[derive(Debug, Error)]
pub(crate) enum RemoteCaptureAdapterError {
    #[error("remote capture identity does not match the approved session")]
    BindingMismatch,
    #[error("the provisioned adapter cannot satisfy this capture profile")]
    WrongEndpoint,
    #[error("fresh target-local app approval is required")]
    ApprovalRequired,
    #[error("remote capture session is already active")]
    AlreadyActive,
    #[error("target-local capture broker is unavailable")]
    BrokerUnavailable,
    #[error("target-local capture broker rejected the operation: {0}")]
    Broker(String),
    #[error("invalid or out-of-order scanner chunk")]
    InvalidChunk,
    #[error("private scanner spool failed: {0}")]
    Spool(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::remote_capture_device::{
        REMOTE_CAPTURE_PROTOCOL_VERSION, RemoteDocumentFormat, RemoteVideoFormat,
    };

    #[derive(Default)]
    struct FakeAdapter {
        scanner: bool,
        revoked: Vec<[u8; 16]>,
    }

    impl RemoteCaptureAdapter for FakeAdapter {
        fn provision(
            &mut self,
            _profile: &RemoteCaptureProfile,
        ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError> {
            Ok(if self.scanner {
                RemoteCaptureEndpoint::DocumentScanner
            } else {
                RemoteCaptureEndpoint::PipeWireNode(42)
            })
        }

        fn revoke(&mut self, session_id: [u8; 16]) {
            self.revoked.push(session_id);
        }
    }

    fn app(id: &str, workspace: &str) -> VerifiedAppRegistration {
        VerifiedAppRegistration::new_verified(id, workspace).unwrap()
    }

    fn request(profile: RemoteCaptureProfile) -> RemoteCaptureRequest {
        RemoteCaptureRequest {
            version: REMOTE_CAPTURE_PROTOCOL_VERSION,
            request_id: [1; 16],
            app_id: "dev.tilo.Camera".into(),
            workspace_id: "workspace-a".into(),
            app_label: "Camera".into(),
            profile,
        }
    }

    fn grant_for(request: &RemoteCaptureRequest) -> ([u8; 16], RemoteCaptureGrant) {
        let mut registry = crate::shell::remote_capture_device::RemoteCaptureRegistry::default();
        registry
            .grant(
                request,
                DeviceId::new("phone").unwrap(),
                DeviceId::new("laptop").unwrap(),
                Instant::now(),
            )
            .unwrap()
    }

    #[test]
    fn media_endpoint_requires_fresh_exact_app_approval_and_revokes_atomically() {
        let broker = Arc::new(Mutex::new(CaptureBroker::new()));
        let mut bridge = RemoteCaptureTargetBridge::new(Arc::clone(&broker));
        let mut adapter = FakeAdapter::default();
        let profile = RemoteCaptureProfile::Camera {
            format: RemoteVideoFormat::Nv12,
            width: 1280,
            height: 720,
            frames_per_second: 30,
        };
        let request = request(profile);
        let (session, grant) = grant_for(&request);
        let owner = app("dev.tilo.Camera", "workspace-a");
        let source = DeviceId::new("phone").unwrap();
        let target = DeviceId::new("laptop").unwrap();
        bridge
            .prepare(
                &owner,
                &request,
                source.clone(),
                target.clone(),
                session,
                grant,
                &mut adapter,
            )
            .unwrap();
        assert_eq!(
            broker.lock().unwrap().enumerate(CaptureKind::Camera).len(),
            1
        );
        assert!(matches!(
            bridge.activate(
                &owner,
                &source,
                &target,
                session,
                grant,
                mint_approval(),
                Instant::now()
            ),
            Err(RemoteCaptureAdapterError::ApprovalRequired)
        ));
        let approval = bridge
            .approve(&owner, &source, &target, session, grant, Instant::now())
            .unwrap();
        bridge
            .activate(
                &owner,
                &source,
                &target,
                session,
                grant,
                approval,
                Instant::now(),
            )
            .unwrap();
        assert!(matches!(
            bridge.revoke(
                &app("dev.tilo.Other", "workspace-a"),
                &source,
                &target,
                session,
                grant,
                &mut adapter
            ),
            Err(RemoteCaptureAdapterError::BindingMismatch)
        ));
        bridge
            .revoke(&owner, &source, &target, session, grant, &mut adapter)
            .unwrap();
        assert!(
            broker
                .lock()
                .unwrap()
                .enumerate(CaptureKind::Camera)
                .is_empty()
        );
        assert_eq!(adapter.revoked, vec![session]);
    }

    #[test]
    fn scanner_uses_unnamed_bounded_sequential_spool_and_peer_teardown() {
        let broker = Arc::new(Mutex::new(CaptureBroker::new()));
        let mut bridge = RemoteCaptureTargetBridge::new(broker);
        let mut adapter = FakeAdapter {
            scanner: true,
            ..FakeAdapter::default()
        };
        let request = request(RemoteCaptureProfile::DocumentScanner {
            format: RemoteDocumentFormat::Pdf,
            max_pages: 2,
            max_document_bytes: 8,
        });
        let (session, grant) = grant_for(&request);
        let owner = app("dev.tilo.Camera", "workspace-a");
        let source = DeviceId::new("phone").unwrap();
        let target = DeviceId::new("laptop").unwrap();
        bridge
            .prepare(
                &owner,
                &request,
                source.clone(),
                target.clone(),
                session,
                grant,
                &mut adapter,
            )
            .unwrap();
        let approval = bridge
            .approve(&owner, &source, &target, session, grant, Instant::now())
            .unwrap();
        bridge
            .activate(
                &owner,
                &source,
                &target,
                session,
                grant,
                approval,
                Instant::now(),
            )
            .unwrap();
        let first = RemoteCaptureChunk {
            session_id: session,
            grant,
            sequence: 1,
            offset: 0,
            total_bytes: 8,
            page_index: Some(0),
            end_of_unit: false,
            payload: vec![1, 2, 3, 4],
        };
        bridge
            .append_scanner_chunk(&owner, &source, &target, &first)
            .unwrap();
        let mut sparse = first.clone();
        sparse.sequence = 2;
        sparse.offset = 6;
        sparse.payload = vec![5, 6];
        assert!(matches!(
            bridge.append_scanner_chunk(&owner, &source, &target, &sparse),
            Err(RemoteCaptureAdapterError::InvalidChunk)
        ));
        assert_eq!(bridge.revoke_peer(&source, &mut adapter), 1);
        assert_eq!(adapter.revoked, vec![session]);
    }

    #[test]
    fn app_and_policy_teardown_are_exact_and_bounded() {
        let broker = Arc::new(Mutex::new(CaptureBroker::new()));
        let mut bridge = RemoteCaptureTargetBridge::new(broker);
        let mut adapter = FakeAdapter::default();
        let request = request(RemoteCaptureProfile::Camera {
            format: RemoteVideoFormat::Mjpeg,
            width: 640,
            height: 480,
            frames_per_second: 15,
        });
        let owner = app("dev.tilo.Camera", "workspace-a");
        for index in 1..=MAX_REMOTE_CAPTURE_SESSIONS {
            let (session, grant) = grant_for(&request);
            bridge
                .prepare(
                    &owner,
                    &request,
                    DeviceId::new(format!("phone-{index}")).unwrap(),
                    DeviceId::new("laptop").unwrap(),
                    session,
                    grant,
                    &mut adapter,
                )
                .unwrap();
        }
        let (extra, extra_grant) = grant_for(&request);
        assert!(matches!(
            bridge.prepare(
                &owner,
                &request,
                DeviceId::new("extra").unwrap(),
                DeviceId::new("laptop").unwrap(),
                extra,
                extra_grant,
                &mut adapter
            ),
            Err(RemoteCaptureAdapterError::BindingMismatch)
        ));
        assert_eq!(
            bridge.revoke_app("dev.tilo.Other", "workspace-a", &mut adapter),
            0
        );
        assert_eq!(
            bridge.revoke_app("dev.tilo.Camera", "workspace-a", &mut adapter),
            MAX_REMOTE_CAPTURE_SESSIONS
        );
        assert_eq!(bridge.revoke_all(&mut adapter), 0);
    }
}

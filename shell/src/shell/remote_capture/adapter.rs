//! Target-local adapters for certified-device camera, microphone, and scanner input.
//!
//! Fabric messages stop at [`RemoteCaptureProfile`]. PipeWire node identifiers,
//! V4L2 file descriptors, scanner files, and the fresh app-consent capability in
//! this module are process-local and deliberately have no serialization traits.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::process::{Child, ChildStdin, Command, Stdio};
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
use crate::shell::remote_capture::device::{
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
pub(crate) trait RemoteCaptureAdapter: Send {
    fn availability(&self) -> RemoteCaptureAvailability;

    fn provision(
        &mut self,
        session_id: [u8; 16],
        profile: &RemoteCaptureProfile,
    ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError>;

    fn revoke(&mut self, _session_id: [u8; 16]) {}

    fn ingest(
        &mut self,
        _session_id: [u8; 16],
        _profile: &RemoteCaptureProfile,
        _chunk: &RemoteCaptureChunk,
    ) -> Result<(), RemoteCaptureAdapterError> {
        Err(RemoteCaptureAdapterError::WrongEndpoint)
    }
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
    complete: bool,
    delivered: bool,
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
    active: bool,
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

    /// Install an endpoint produced by the bounded provisioning worker. This
    /// half is deliberately non-blocking and safe to call from a compositor
    /// tick; discovery and subprocess startup happen before it.
    pub(crate) fn prepare_endpoint(
        &mut self,
        app: &VerifiedAppRegistration,
        request: &RemoteCaptureRequest,
        source: DeviceId,
        target: DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
        endpoint: RemoteCaptureEndpoint,
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
            adapter.revoke(session_id);
            return Err(RemoteCaptureAdapterError::BindingMismatch);
        }
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
                let file = unnamed_scanner_spool().map_err(RemoteCaptureAdapterError::Spool)?;
                (
                    None,
                    PreparedEndpoint::Scanner(ScannerSpool {
                        file,
                        bytes: 0,
                        max_bytes: max_document_bytes,
                        last_page: None,
                        complete: false,
                        delivered: false,
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
                active: false,
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
        session.active = true;
        Ok(())
    }

    pub(crate) fn accepts_chunk(
        &self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
    ) -> bool {
        self.exact_session(app, source, target, session_id, grant)
            .is_ok_and(|session| session.active)
    }

    pub(crate) fn append_media_chunk(
        &mut self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        chunk: &RemoteCaptureChunk,
        adapter: &mut dyn RemoteCaptureAdapter,
    ) -> Result<(), RemoteCaptureAdapterError> {
        let session = self.exact_session_mut(app, source, target, chunk.session_id, chunk.grant)?;
        if !session.active || !matches!(session.endpoint, PreparedEndpoint::Media) {
            return Err(RemoteCaptureAdapterError::ApprovalRequired);
        }
        if !chunk.valid_for(session.profile.kind()) {
            return Err(RemoteCaptureAdapterError::InvalidChunk);
        }
        adapter.ingest(chunk.session_id, &session.profile, chunk)
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
        if !session.active || !matches!(session.endpoint, PreparedEndpoint::Scanner(_)) {
            return Err(RemoteCaptureAdapterError::ApprovalRequired);
        }
        let PreparedEndpoint::Scanner(spool) = &mut session.endpoint else {
            return Err(RemoteCaptureAdapterError::WrongEndpoint);
        };
        if !chunk.valid_for(RemoteCaptureKind::DocumentScanner)
            || chunk.offset != spool.bytes
            || chunk.total_bytes > spool.max_bytes
            || chunk.page_index < spool.last_page
            || (chunk.end_of_unit && chunk.offset + chunk.payload.len() as u64 != chunk.total_bytes)
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
        if chunk.end_of_unit {
            spool.complete = true;
        }
        Ok(())
    }

    /// Return a single target-local descriptor for a completed scan. The
    /// unnamed file never acquires a path and exact app/session binding is
    /// rechecked at delivery time.
    pub(crate) fn take_completed_scanner(
        &mut self,
        app: &VerifiedAppRegistration,
        source: &DeviceId,
        target: &DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
    ) -> Result<File, RemoteCaptureAdapterError> {
        let session = self.exact_session_mut(app, source, target, session_id, grant)?;
        let PreparedEndpoint::Scanner(spool) = &mut session.endpoint else {
            return Err(RemoteCaptureAdapterError::WrongEndpoint);
        };
        if !session.active || !spool.complete || spool.delivered {
            return Err(RemoteCaptureAdapterError::ApprovalRequired);
        }
        let mut file = spool
            .file
            .try_clone()
            .map_err(RemoteCaptureAdapterError::Spool)?;
        file.seek(SeekFrom::Start(0))
            .map_err(RemoteCaptureAdapterError::Spool)?;
        spool.delivered = true;
        Ok(file)
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

fn unnamed_scanner_spool() -> std::io::Result<File> {
    let directory = c"/tmp";
    // SAFETY: `directory` is a static NUL-terminated path and a successful
    // O_TMPFILE open returns a new descriptor owned exclusively by this call.
    let fd = unsafe {
        libc::open(
            directory.as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: ownership of the fresh descriptor transfers to `File` once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RemoteCaptureAvailability {
    pub camera: bool,
    pub microphone: bool,
    pub scanner: bool,
}

enum ProductionEndpoint {
    Camera(File),
    Microphone {
        loopback: Child,
        writer: Child,
        stdin: ChildStdin,
    },
}

/// Production target-side injection. It provisions only kernel/PipeWire
/// endpoints that actually exist and keeps every raw device/node identity
/// inside the shell process.
pub(crate) struct ProductionRemoteCaptureAdapter {
    availability: RemoteCaptureAvailability,
    sessions: BTreeMap<[u8; 16], ProductionEndpoint>,
}

impl ProductionRemoteCaptureAdapter {
    pub(crate) fn detect() -> Self {
        Self {
            availability: RemoteCaptureAvailability {
                camera: remote_camera_loopback().is_some() && trusted_command("v4l2-ctl").is_some(),
                microphone: trusted_command("pw-loopback").is_some()
                    && trusted_command("pw-cat").is_some()
                    && trusted_command("pw-dump").is_some(),
                // Reception needs only the compositor-private anonymous spool.
                // `scanimage` is a source-side capability.
                scanner: true,
            },
            sessions: BTreeMap::new(),
        }
    }

    fn provision_camera(
        &mut self,
        session_id: [u8; 16],
        profile: &RemoteCaptureProfile,
    ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError> {
        use crate::shell::remote_capture::device::RemoteVideoFormat;
        let RemoteCaptureProfile::Camera {
            format,
            width,
            height,
            ..
        } = profile
        else {
            return Err(RemoteCaptureAdapterError::WrongEndpoint);
        };
        if *format != RemoteVideoFormat::Nv12 || !self.availability.camera {
            return Err(RemoteCaptureAdapterError::BackendUnavailable(
                "a Tilo v4l2loopback camera accepting NV12 is unavailable".into(),
            ));
        }
        let path = remote_camera_loopback().ok_or_else(|| {
            RemoteCaptureAdapterError::BackendUnavailable(
                "the remote-camera loopback disappeared".into(),
            )
        })?;
        let status = Command::new(trusted_command("v4l2-ctl").ok_or_else(|| {
            RemoteCaptureAdapterError::BackendUnavailable("v4l2-ctl is unavailable".into())
        })?)
        .args([
            "--device",
            path.to_str().ok_or_else(|| {
                RemoteCaptureAdapterError::BackendUnavailable(
                    "remote-camera device path is invalid".into(),
                )
            })?,
            "--set-fmt-video",
            &format!("width={width},height={height},pixelformat=NV12"),
        ])
        .status()
        .map_err(RemoteCaptureAdapterError::Io)?;
        if !status.success() {
            return Err(RemoteCaptureAdapterError::BackendUnavailable(
                "the remote-camera loopback rejected the negotiated profile".into(),
            ));
        }
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(RemoteCaptureAdapterError::Io)?;
        let reader = std::fs::OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(RemoteCaptureAdapterError::Io)?;
        self.sessions
            .insert(session_id, ProductionEndpoint::Camera(writer));
        Ok(RemoteCaptureEndpoint::V4l2Device(OwnedFd::from(reader)))
    }

    fn provision_microphone(
        &mut self,
        session_id: [u8; 16],
        profile: &RemoteCaptureProfile,
    ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError> {
        use crate::shell::remote_capture::device::RemoteAudioFormat;
        let RemoteCaptureProfile::Microphone {
            format,
            sample_rate,
            channels,
            ..
        } = profile
        else {
            return Err(RemoteCaptureAdapterError::WrongEndpoint);
        };
        if *format != RemoteAudioFormat::PcmS16Le || !self.availability.microphone {
            return Err(RemoteCaptureAdapterError::BackendUnavailable(
                "PipeWire remote microphone injection is unavailable".into(),
            ));
        }
        let suffix = hex::encode(session_id);
        let sink_name = format!("tilo.remote-mic-sink.{suffix}");
        let source_name = format!("tilo.remote-mic-source.{suffix}");
        let mut loopback = Command::new(trusted_command("pw-loopback").ok_or_else(|| {
            RemoteCaptureAdapterError::BackendUnavailable("pw-loopback is unavailable".into())
        })?)
            .arg(format!(
                "--capture-props={{ node.name=\"{sink_name}\" media.class=\"Audio/Sink\" }}"
            ))
            .arg(format!(
                "--playback-props={{ node.name=\"{source_name}\" node.description=\"Tilo remote microphone\" media.class=\"Audio/Source\" }}"
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(RemoteCaptureAdapterError::Io)?;
        let node_id = wait_for_pipewire_node(&source_name).map_err(|error| {
            let _ = loopback.kill();
            error
        })?;
        let mut writer = Command::new(trusted_command("pw-cat").ok_or_else(|| {
            RemoteCaptureAdapterError::BackendUnavailable("pw-cat is unavailable".into())
        })?)
        .arg("--playback")
        .arg("--target")
        .arg(&sink_name)
        .arg("--format")
        .arg("s16")
        .arg("--rate")
        .arg(sample_rate.to_string())
        .arg("--channels")
        .arg(channels.to_string())
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            let _ = loopback.kill();
            RemoteCaptureAdapterError::Io(error)
        })?;
        let stdin = writer.stdin.take().ok_or_else(|| {
            let _ = writer.kill();
            let _ = loopback.kill();
            RemoteCaptureAdapterError::BackendUnavailable(
                "pw-cat did not expose its bounded audio input".into(),
            )
        })?;
        self.sessions.insert(
            session_id,
            ProductionEndpoint::Microphone {
                loopback,
                writer,
                stdin,
            },
        );
        Ok(RemoteCaptureEndpoint::PipeWireNode(node_id))
    }
}

impl RemoteCaptureAdapter for ProductionRemoteCaptureAdapter {
    fn availability(&self) -> RemoteCaptureAvailability {
        self.availability
    }

    fn provision(
        &mut self,
        session_id: [u8; 16],
        profile: &RemoteCaptureProfile,
    ) -> Result<RemoteCaptureEndpoint, RemoteCaptureAdapterError> {
        if session_id == [0; 16] || self.sessions.contains_key(&session_id) {
            return Err(RemoteCaptureAdapterError::BindingMismatch);
        }
        match profile.kind() {
            RemoteCaptureKind::Camera => self.provision_camera(session_id, profile),
            RemoteCaptureKind::Microphone => self.provision_microphone(session_id, profile),
            RemoteCaptureKind::DocumentScanner => Ok(RemoteCaptureEndpoint::DocumentScanner),
        }
    }

    fn ingest(
        &mut self,
        session_id: [u8; 16],
        profile: &RemoteCaptureProfile,
        chunk: &RemoteCaptureChunk,
    ) -> Result<(), RemoteCaptureAdapterError> {
        if !chunk.valid_for(profile.kind()) {
            return Err(RemoteCaptureAdapterError::InvalidChunk);
        }
        match self.sessions.get_mut(&session_id) {
            Some(ProductionEndpoint::Camera(writer))
                if profile.kind() == RemoteCaptureKind::Camera =>
            {
                writer
                    .write_all(&chunk.payload)
                    .map_err(RemoteCaptureAdapterError::Io)
            }
            Some(ProductionEndpoint::Microphone { stdin, .. })
                if profile.kind() == RemoteCaptureKind::Microphone =>
            {
                stdin
                    .write_all(&chunk.payload)
                    .map_err(RemoteCaptureAdapterError::Io)
            }
            _ => Err(RemoteCaptureAdapterError::WrongEndpoint),
        }
    }

    fn revoke(&mut self, session_id: [u8; 16]) {
        if let Some(ProductionEndpoint::Microphone {
            mut loopback,
            mut writer,
            ..
        }) = self.sessions.remove(&session_id)
        {
            let _ = writer.kill();
            let _ = loopback.kill();
        }
    }
}

impl Drop for ProductionRemoteCaptureAdapter {
    fn drop(&mut self) {
        let sessions = self.sessions.keys().copied().collect::<Vec<_>>();
        for session in sessions {
            self.revoke(session);
        }
    }
}

fn trusted_command(name: &str) -> Option<std::path::PathBuf> {
    ["/usr/bin", "/usr/libexec/tilo"]
        .into_iter()
        .map(std::path::Path::new)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn remote_camera_loopback() -> Option<std::path::PathBuf> {
    std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let label = std::fs::read_to_string(entry.path().join("name")).ok()?;
            label
                .trim()
                .starts_with("Tilo Remote Camera")
                .then(|| std::path::Path::new("/dev").join(entry.file_name()))
        })
}

fn wait_for_pipewire_node(name: &str) -> Result<u32, RemoteCaptureAdapterError> {
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let output = Command::new(trusted_command("pw-dump").ok_or_else(|| {
            RemoteCaptureAdapterError::BackendUnavailable("pw-dump is unavailable".into())
        })?)
        .output()
        .map_err(RemoteCaptureAdapterError::Io)?;
        if output.status.success()
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            && let Some(node) = value.as_array().and_then(|items| {
                items.iter().find_map(|item| {
                    (item
                        .pointer("/info/props/node.name")
                        .and_then(|v| v.as_str())
                        == Some(name))
                    .then(|| item.get("id").and_then(serde_json::Value::as_u64))
                    .flatten()
                    .and_then(|id| u32::try_from(id).ok())
                })
            })
        {
            return Ok(node);
        }
        if Instant::now() >= deadline {
            return Err(RemoteCaptureAdapterError::BackendUnavailable(
                "PipeWire did not publish the remote microphone node".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
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
    #[error("remote capture backend is unavailable: {0}")]
    BackendUnavailable(String),
    #[error("remote capture backend I/O failed: {0}")]
    Io(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::remote_capture::device::{
        REMOTE_CAPTURE_PROTOCOL_VERSION, RemoteDocumentFormat, RemoteVideoFormat,
    };

    #[derive(Default)]
    struct FakeAdapter {
        scanner: bool,
        revoked: Vec<[u8; 16]>,
    }

    impl RemoteCaptureAdapter for FakeAdapter {
        fn availability(&self) -> RemoteCaptureAvailability {
            RemoteCaptureAvailability {
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
        let mut registry = crate::shell::remote_capture::device::RemoteCaptureRegistry::default();
        registry
            .grant(
                request,
                DeviceId::new("phone").unwrap(),
                DeviceId::new("laptop").unwrap(),
                Instant::now(),
            )
            .unwrap()
    }

    fn prepare(
        bridge: &mut RemoteCaptureTargetBridge,
        adapter: &mut FakeAdapter,
        owner: &VerifiedAppRegistration,
        request: &RemoteCaptureRequest,
        source: DeviceId,
        target: DeviceId,
        session: [u8; 16],
        grant: RemoteCaptureGrant,
    ) -> Result<(), RemoteCaptureAdapterError> {
        let endpoint = adapter.provision(session, &request.profile)?;
        bridge.prepare_endpoint(
            owner, request, source, target, session, grant, endpoint, adapter,
        )
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
        prepare(
            &mut bridge,
            &mut adapter,
            &owner,
            &request,
            source.clone(),
            target.clone(),
            session,
            grant,
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
            bridge.activate(
                &owner,
                &source,
                &target,
                session,
                grant,
                approval,
                Instant::now(),
            ),
            Err(RemoteCaptureAdapterError::ApprovalRequired)
                | Err(RemoteCaptureAdapterError::AlreadyActive)
        ));
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
        prepare(
            &mut bridge,
            &mut adapter,
            &owner,
            &request,
            source.clone(),
            target.clone(),
            session,
            grant,
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
        let second = RemoteCaptureChunk {
            sequence: 2,
            offset: 4,
            end_of_unit: true,
            payload: vec![5, 6, 7, 8],
            ..first
        };
        bridge
            .append_scanner_chunk(&owner, &source, &target, &second)
            .unwrap();
        let mut completed = bridge
            .take_completed_scanner(&owner, &source, &target, session, grant)
            .unwrap();
        assert_eq!(completed.metadata().unwrap().len(), 8);
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut completed, &mut contents).unwrap();
        assert_eq!(contents, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(matches!(
            bridge.take_completed_scanner(&owner, &source, &target, session, grant),
            Err(RemoteCaptureAdapterError::ApprovalRequired)
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
            prepare(
                &mut bridge,
                &mut adapter,
                &owner,
                &request,
                DeviceId::new(format!("phone-{index}")).unwrap(),
                DeviceId::new("laptop").unwrap(),
                session,
                grant,
            )
            .unwrap();
        }
        let (extra, extra_grant) = grant_for(&request);
        assert!(matches!(
            prepare(
                &mut bridge,
                &mut adapter,
                &owner,
                &request,
                DeviceId::new("extra").unwrap(),
                DeviceId::new("laptop").unwrap(),
                extra,
                extra_grant,
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

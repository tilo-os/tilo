//! Genuine fixed installed daemon enrollment on the existing storage lane.
//! The public codec describes a role; this owner requires kernel QPE and the
//! independently held installed person anchor before producing a verifier FD.
use crate::setup::installed_retained_actor::{
    InstalledRetainedLaunchInput, with_proof_deadline_at,
};
use runtime_fs::{
    external_history::{
        EnrolledVerifier, EnrollmentChallenge, EnrollmentCommand, EnrollmentContext,
        daemon_control::{DaemonDelegation, EnrollmentRequest},
    },
    storage::{DEFAULT_STORAGE_SOCKET, STORAGE_FRAMING, StorageRequest, StorageResponse},
};
use runtime_portal::{
    fd_transport,
    process_binding::{self, ExecutingPeer},
    retained_role::{NativeHistoryVerifierRecord, VerifiedDaemonPeer},
};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn nonce() -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}
fn frame() -> wire_frame::Framing {
    wire_frame::Framing::little_endian(256 * 1024)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AdmissionPurpose {
    SelectedReplace,
    HomeHistoryRead,
    SelectedEditorHistoryRead,
}
pub struct PendingDaemonEnrollment {
    input: Arc<InstalledRetainedLaunchInput>,
    purpose: AdmissionPurpose,
    peer: VerifiedDaemonPeer,
    stream: UnixStream,
    context: EnrollmentContext,
    challenge: EnrollmentChallenge,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}
pub struct CertifiedNativeHistoryDaemon {
    input: Arc<InstalledRetainedLaunchInput>,
    purpose: AdmissionPurpose,
    peer: VerifiedDaemonPeer,
    stream: Mutex<UnixStream>,
    context: EnrollmentContext,
    challenge: EnrollmentChallenge,
    sealed: File,
    cancel: Arc<AtomicBool>,
    failed: AtomicBool,
}
impl PendingDaemonEnrollment {
    pub fn begin(
        input: Arc<InstalledRetainedLaunchInput>,
        request: &EnrollmentRequest,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    ) -> io::Result<Self> {
        Self::begin_purpose(
            input,
            request,
            cancel,
            deadline,
            AdmissionPurpose::SelectedReplace,
        )
    }
    pub fn begin_read(
        input: Arc<InstalledRetainedLaunchInput>,
        request: &EnrollmentRequest,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    ) -> io::Result<Self> {
        Self::begin_purpose(
            input,
            request,
            cancel,
            deadline,
            AdmissionPurpose::HomeHistoryRead,
        )
    }
    pub fn begin_editor_read(
        input: Arc<InstalledRetainedLaunchInput>,
        request: &EnrollmentRequest,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    ) -> io::Result<Self> {
        Self::begin_purpose(
            input,
            request,
            cancel,
            deadline,
            AdmissionPurpose::SelectedEditorHistoryRead,
        )
    }
    fn begin_purpose(
        input: Arc<InstalledRetainedLaunchInput>,
        request: &EnrollmentRequest,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
        purpose: AdmissionPurpose,
    ) -> io::Result<Self> {
        input.revalidate(&cancel)?;
        match purpose {
            AdmissionPurpose::SelectedReplace => input.check_selected_parent_write(&cancel)?,
            AdmissionPurpose::HomeHistoryRead => input.check_home_history_read(&cancel)?,
            AdmissionPurpose::SelectedEditorHistoryRead => {
                input.check_selected_editor_history_read(&cancel)?
            }
        }
        if !request.valid()
            || request.person_binding
                != serde_json::to_vec(input.binding()).map_err(io::Error::other)?
            || (matches!(
                purpose,
                AdmissionPurpose::SelectedReplace | AdmissionPurpose::SelectedEditorHistoryRead
            ) && input.binding().resource_record
                != format!("selected-open-v2:edit-in-parent:{}", request.request_id))
        {
            return Err(refused());
        }
        let mut stream = connect(&cancel, deadline)?;
        let held = stream.try_clone()?;
        with_proof_deadline_at(&held, &cancel, deadline, || {
            let storage_request = match purpose {
                AdmissionPurpose::SelectedReplace => StorageRequest::BindExternalSource,
                AdmissionPurpose::HomeHistoryRead | AdmissionPurpose::SelectedEditorHistoryRead => {
                    StorageRequest::BindExternalReadSource
                }
            };
            STORAGE_FRAMING
                .write_value(&mut stream, &storage_request)
                .map_err(io::Error::other)?;
            let response: StorageResponse = STORAGE_FRAMING
                .read_value(&mut stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let matching = match purpose {
                AdmissionPurpose::SelectedReplace => {
                    matches!(response, StorageResponse::ExternalSourceBinding)
                }
                AdmissionPurpose::HomeHistoryRead | AdmissionPurpose::SelectedEditorHistoryRead => {
                    matches!(
                        response,
                        StorageResponse::ExternalReadSourceBinding { schema: 1 }
                    )
                }
            };
            if !matching {
                return Err(refused());
            }
            process_binding::complete(&mut stream)?;
            process_binding::arm(&stream)?;
            let expected = nonce();
            stream.write_all(&process_binding::MAGIC)?;
            stream.write_all(&expected)?;
            let peer = Arc::new(process_binding::receive(&stream, &expected, 0)?);
            let peer = VerifiedDaemonPeer::admit(peer)?;
            stream.write_all(&[1])?;
            let bytes = frame()
                .read(&mut stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let challenge: EnrollmentChallenge =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if !challenge.valid() {
                return Err(refused());
            }
            let (certificate, revocations) = input.current_history_material(&cancel)?;
            if challenge.certificate
                != serde_json::to_vec(&certificate).map_err(io::Error::other)?
                || revocations.is_revoked(&certificate.device_public_key)
            {
                return Err(refused());
            }
            let verifier = NativeHistoryVerifierRecord {
                schema: 1,
                source_owner: input.native_owner_sha256(&cancel)?,
                source_operation: input.binding().operation_id.clone(),
                person: certificate.user_public_key,
                personal_room: input.binding().room.clone(),
                device: certificate.device_public_key,
            };
            let context = EnrollmentContext {
                schema: 1,
                nonce: nonce(),
                verifier,
                person_binding: request.person_binding.clone(),
            };
            context.encode()?;
            peer.check_current()?;
            input.revalidate(&cancel)?;
            Ok(Self {
                input: input.clone(),
                purpose,
                peer,
                stream: stream.try_clone()?,
                context,
                challenge,
                cancel: cancel.clone(),
                deadline,
            })
        })
    }
    pub fn delegation(&self, request: &EnrollmentRequest) -> io::Result<DaemonDelegation> {
        self.input.revalidate(&self.cancel)?;
        self.peer.check_current()?;
        let delegation = DaemonDelegation {
            schema: 1,
            request_id: request.request_id.clone(),
            context: self.context.clone(),
            daemon_pid: self.peer.peer().pid(),
            daemon_start: self.peer.start_time()?,
            program_identity: self.peer.program_identity()?,
        };
        if !delegation.matches(request) {
            return Err(refused());
        }
        Ok(delegation)
    }
    pub fn peer(&self) -> &ExecutingPeer {
        self.peer.peer()
    }
    pub fn finish(self, proof: File) -> io::Result<Arc<CertifiedNativeHistoryDaemon>> {
        let Self {
            input,
            purpose,
            peer,
            mut stream,
            context,
            challenge,
            cancel,
            deadline,
        } = self;
        let held = stream.try_clone()?;
        with_proof_deadline_at(&held, &cancel, deadline, || {
            peer.check_current()?;
            input.revalidate(&cancel)?;
            let context_file = super::seal(&context.encode()?)?;
            send_file(&mut stream, 61, &context_file)?;
            send_file(&mut stream, 62, &proof)?;
            let bytes = frame()
                .read(&mut stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let enrolled = EnrolledVerifier::decode(&bytes)?;
            let (certificate, revocations) = input.current_history_material(&cancel)?;
            if revocations.is_revoked(&certificate.device_public_key) {
                return Err(refused());
            }
            enrolled.verify(&context, &challenge, &certificate.device_public_key)?;
            peer.check_current()?;
            input.revalidate(&cancel)?;
            let sealed = super::seal(&context.verifier.encode()?)?;
            let value = Arc::new(CertifiedNativeHistoryDaemon {
                input: input.clone(),
                purpose,
                peer,
                stream: Mutex::new(stream),
                context,
                challenge,
                sealed,
                cancel: cancel.clone(),
                failed: AtomicBool::new(false),
            });
            value.check()?;
            Ok(value)
        })
    }
}
impl CertifiedNativeHistoryDaemon {
    pub fn is_home_history_read(&self) -> bool {
        self.purpose == AdmissionPurpose::HomeHistoryRead
    }
    pub fn is_selected_editor_history_read(&self) -> bool {
        self.purpose == AdmissionPurpose::SelectedEditorHistoryRead
    }
    pub fn input(&self) -> &Arc<InstalledRetainedLaunchInput> {
        &self.input
    }
    pub fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        Arc::ptr_eq(self.input(), input)
    }
    pub fn check(&self) -> io::Result<()> {
        let result = (|| {
            if self.failed.load(Ordering::Acquire) {
                return Err(refused());
            }
            self.peer.check_current()?;
            self.input.revalidate(&self.cancel)?;
            match self.purpose {
                AdmissionPurpose::HomeHistoryRead => {
                    self.input.check_home_history_read(&self.cancel)?
                }
                AdmissionPurpose::SelectedEditorHistoryRead => self
                    .input
                    .check_selected_editor_history_read(&self.cancel)?,
                AdmissionPurpose::SelectedReplace => {}
            }
            let (certificate, revocations) = self.input.current_history_material(&self.cancel)?;
            if self.context.verifier.device != certificate.device_public_key
                || self.context.verifier.person != certificate.user_public_key
                || self.context.verifier.source_owner
                    != self.input.native_owner_sha256(&self.cancel)?
                || self.context.verifier.source_operation != self.input.binding().operation_id
                || self.context.verifier.personal_room != self.input.binding().room
                || revocations.is_revoked(&certificate.device_public_key)
            {
                return Err(refused());
            }
            self.peer.check_current()
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    pub fn refresh(&self) -> io::Result<()> {
        self.check()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(refused)?;
        let mut stream = self.stream.lock().map_err(|_| refused())?;
        let held = stream.try_clone()?;
        let result = with_proof_deadline_at(&held, &self.cancel, deadline, || {
            let requested = nonce();
            frame()
                .write(
                    &mut *stream,
                    &serde_json::to_vec(&EnrollmentCommand::Refresh { nonce: requested })
                        .map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            let bytes = frame()
                .read(&mut *stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let enrolled = EnrolledVerifier::decode(&bytes)?;
            let (certificate, _) = self.input.current_history_material(&self.cancel)?;
            enrolled.verify_refresh(
                &self.context,
                &self.challenge,
                requested,
                &certificate.device_public_key,
            )?;
            self.check()
        });
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    pub(super) fn verify_replacement(
        &self,
        observation: &runtime_fs::directory_backend::PendingReplacementObservation,
        prepared: &runtime_fs::external_history::PreparedReceipt,
        committed: &runtime_fs::external_history::CommittedReceipt,
    ) -> io::Result<()> {
        if self.purpose != AdmissionPurpose::SelectedReplace {
            return Err(refused());
        }
        self.check()?;
        observation.validate().map_err(io::Error::other)?;
        prepared.verify(&self.context.verifier)?;
        committed.verify(prepared, &self.context.verifier)?;
        if prepared.operation_id != uuid::Uuid::from_bytes(observation.operation).to_string()
            || prepared.source_owner != observation.source_owner
            || prepared.intent_digest != observation.intent_digest
            || prepared.archive_binding_digest != observation.archive_plan_digest
            || committed.source_identity != observation.source
            || committed.victim_identity != observation.expected_victim
            || committed.destination_identity != observation.source
        {
            return Err(refused());
        }
        self.check()
    }
    pub(super) fn replacement_command(
        &self,
        request: &str,
        handoff: Option<&crate::setup::retained_source_worker::PendingHandoff>,
    ) -> io::Result<runtime_fs::external_history::replacement_control::ReplacementReply> {
        if self.purpose != AdmissionPurpose::SelectedReplace {
            return Err(refused());
        }
        self.check()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(refused)?;
        let mut stream = self.stream.lock().map_err(|_| refused())?;
        let held = stream.try_clone()?;
        let command = if handoff.is_some() {
            EnrollmentCommand::StartReplacement {
                request_id: request.to_owned(),
            }
        } else {
            EnrollmentCommand::PollReplacement {
                request_id: request.to_owned(),
            }
        };
        let result = with_proof_deadline_at(&held, &self.cancel, deadline, || {
            frame()
                .write(
                    &mut *stream,
                    &serde_json::to_vec(&command).map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            if let Some(handoff) = handoff {
                let (record, provider, program) = handoff.descriptors()?;
                send_file(&mut stream, 63, record)?;
                send_descriptor(&mut stream, 64, provider.as_fd())?;
                send_file(&mut stream, 65, program)?;
            }
            let bytes = wire_frame::Framing::little_endian(
                runtime_fs::external_history::replacement_control::ReplacementReply::LIMIT,
            )
            .read(&mut *stream)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
            let reply =
                runtime_fs::external_history::replacement_control::ReplacementReply::decode(
                    &bytes, request,
                )?;
            self.check()?;
            Ok(reply)
        });
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    /// Fixed retirement command bound to real same-pin signed ACK owners.
    pub(super) fn retire_replacement(
        &self,
        older: &super::AcknowledgedSelectedReplacement,
        successor: &super::AcknowledgedSelectedReplacement,
    ) -> io::Result<()> {
        use runtime_fs::external_history::replacement_control::ReplacementReply;
        if self.purpose != AdmissionPurpose::SelectedReplace {
            return Err(refused());
        }
        self.check()?;
        older.verify_retirement_receipt(self)?;
        successor.verify_retirement_receipt(self)?;
        let (operation, digest) = older.retirement_binding();
        let request = uuid::Uuid::from_bytes(older.observation().operation).to_string();
        let next = uuid::Uuid::from_bytes(successor.observation().operation).to_string();
        let command = EnrollmentCommand::RetireReplacement {
            request_id: request.clone(),
            successor_request_id: next,
            operation_id: operation.to_owned(),
            transaction_digest: digest,
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(refused)?;
        let mut stream = self.stream.lock().map_err(|_| refused())?;
        let held = stream.try_clone()?;
        let result = with_proof_deadline_at(&held, &self.cancel, deadline, || {
            frame()
                .write(
                    &mut *stream,
                    &serde_json::to_vec(&command).map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            let bytes = wire_frame::Framing::little_endian(ReplacementReply::LIMIT)
                .read(&mut *stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let reply = ReplacementReply::decode(&bytes, &request)?;
            let retired = match reply {
                ReplacementReply::Retired { .. } => {
                    ReplacementReply::decode_retired(&bytes, &request, operation, digest)?;
                    true
                }
                ReplacementReply::Refused {
                    operation_id: Some(id),
                    ..
                } if id == operation => false,
                ReplacementReply::Pending { operation_id, .. } if operation_id == operation => {
                    false
                }
                _ => return Err(refused()),
            };
            self.check()?;
            Ok(retired)
        });
        // A well-framed matching refusal retains the task and may be retried.
        // Lost/malformed framing poisons this exact lane and retains all owners.
        match result {
            Ok(true) => Ok(()),
            Ok(false) => Err(io::ErrorKind::WouldBlock.into()),
            Err(error) => {
                self.failed.store(true, Ordering::Release);
                Err(error)
            }
        }
    }
    pub fn history_read_command(
        &self,
        request: &str,
        query: Option<&str>,
        handoff: Option<&crate::setup::retained_source_worker::HistoryReadHandoff>,
    ) -> io::Result<runtime_fs::external_history::read_control::HistoryReadReply> {
        use runtime_fs::external_history::read_control::{HistoryReadReply, valid_query};
        if !matches!(
            self.purpose,
            AdmissionPurpose::HomeHistoryRead | AdmissionPurpose::SelectedEditorHistoryRead
        ) || uuid::Uuid::parse_str(request).is_err()
            || (query.is_some() == handoff.is_some())
            || query.is_some_and(|query| !valid_query(query))
        {
            return Err(refused());
        }
        self.check()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(refused)?;
        let mut stream = self.stream.lock().map_err(|_| refused())?;
        let held = stream.try_clone()?;
        let command = match query {
            Some(query) => EnrollmentCommand::QueryHistory {
                request_id: request.to_owned(),
                query: query.to_owned(),
            },
            None => EnrollmentCommand::AdoptHistoryRead {
                request_id: request.to_owned(),
            },
        };
        let result = with_proof_deadline_at(&held, &self.cancel, deadline, || {
            frame()
                .write(
                    &mut *stream,
                    &serde_json::to_vec(&command).map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            if let Some(handoff) = handoff {
                let (record, provider, program) = handoff.descriptors()?;
                send_file(&mut stream, 68, record)?;
                send_descriptor(&mut stream, 69, provider.as_fd())?;
                send_file(&mut stream, 70, program)?;
            }
            let bytes = wire_frame::Framing::little_endian(HistoryReadReply::LIMIT)
                .read(&mut *stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let reply = HistoryReadReply::decode(&bytes, request)?;
            if let Some(handoff) = handoff {
                handoff.check()?;
            }
            self.check()?;
            Ok(reply)
        });
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    /// Releases only this Reader lane's already admitted subject. Current Read
    /// permission is deliberately not required for relinquishing owned custody;
    /// the real daemon peer and exact held control connection remain mandatory.
    pub fn retire_history_read(&self, request: &str) -> io::Result<()> {
        use runtime_fs::external_history::read_control::HistoryReadReply;
        if !matches!(
            self.purpose,
            AdmissionPurpose::HomeHistoryRead | AdmissionPurpose::SelectedEditorHistoryRead
        ) || uuid::Uuid::parse_str(request).is_err()
        {
            return Err(refused());
        }
        self.peer.check_current()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(refused)?;
        let mut stream = self.stream.lock().map_err(|_| refused())?;
        let held = stream.try_clone()?;
        // Cancellation of the reader prohibits queries, not relinquishing its
        // actual admitted cap. Cleanup still has a finite owned IO deadline.
        let cleanup_cancel = AtomicBool::new(false);
        with_proof_deadline_at(&held, &cleanup_cancel, deadline, || {
            frame()
                .write(
                    &mut *stream,
                    &serde_json::to_vec(&EnrollmentCommand::RetireHistoryRead {
                        request_id: request.to_owned(),
                    })
                    .map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            let bytes = wire_frame::Framing::little_endian(HistoryReadReply::LIMIT)
                .read(&mut *stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            if !matches!(
                HistoryReadReply::decode(&bytes, request)?,
                HistoryReadReply::Retired { .. }
            ) {
                return Err(refused());
            }
            self.peer.check_current()
        })
    }
    pub fn source_owner(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        let owner = self.context.verifier.source_owner;
        self.check()?;
        Ok(owner)
    }
    pub fn context_digest(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        let digest = self.context.digest()?;
        self.check()?;
        Ok(digest)
    }
    pub fn verifier_file(&self) -> io::Result<File> {
        self.check()?;
        if self.purpose != AdmissionPurpose::SelectedReplace {
            return Err(refused());
        }
        let file = self.sealed.try_clone()?;
        self.check()?;
        Ok(file)
    }
}
impl Drop for CertifiedNativeHistoryDaemon {
    fn drop(&mut self) {
        self.failed.store(true, Ordering::Release);
        let stream = self.stream.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}
fn send_file(stream: &mut UnixStream, tag: u8, file: &File) -> io::Result<()> {
    send_descriptor(stream, tag, file.as_fd())
}
fn send_descriptor(
    stream: &mut UnixStream,
    tag: u8,
    descriptor: impl std::os::fd::AsFd,
) -> io::Result<()> {
    fd_transport::send_with_fd(stream.as_fd(), &[tag], descriptor)?;
    let mut ack = [0];
    stream.read_exact(&mut ack)?;
    if ack != [tag] {
        return Err(refused());
    }
    Ok(())
}
fn connect(cancel: &AtomicBool, deadline: Instant) -> io::Result<UnixStream> {
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if DEFAULT_STORAGE_SOCKET.len() >= address.sun_path.len() {
        return Err(refused());
    }
    for (to, from) in address
        .sun_path
        .iter_mut()
        .zip(DEFAULT_STORAGE_SOCKET.bytes())
    {
        *to = from as libc::c_char;
    }
    let result = unsafe {
        libc::connect(
            owned.as_raw_fd(),
            (&raw const address).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EAGAIN)) {
            return Err(error);
        }
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut event = libc::pollfd {
                fd: owned.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let result = unsafe { libc::poll(&raw mut event, 1, 10) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result > 0 {
                let mut error = 0i32;
                let mut len = std::mem::size_of::<i32>() as libc::socklen_t;
                if unsafe {
                    libc::getsockopt(
                        owned.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        (&raw mut error).cast(),
                        &raw mut len,
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                break;
            }
        }
    }
    let stream = UnixStream::from(owned);
    stream.peer_addr()?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

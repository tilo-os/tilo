//! Certified external-source enrollment on the existing daemon storage lane.
//! Enrollment describes a verifier. Only a later actual pending capability,
//! durable canonical transaction and observed native completion can save.
pub(crate) mod coordinator;
mod lane_controller;
mod owned_io;
mod owned_lane;
mod pending_source;
mod read_handle;
mod revocations;
pub use coordinator::ExternalHistoryRuntime;
// Cap'n Proto capabilities are confined to the daemon's local executor.
type LocalFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
use runtime_fs::external_history::replacement_control;
use std::os::fd::AsFd;

use crate::{
    PeerCredentials, record_publisher::RecordPublisher, workspace_authority::WorkspaceAuthority,
};
use ed25519_dalek::Signer;
use runtime_collab::retained_person::{
    MAX_PROOF_BYTES, RetainedLaunchBinding, SignedRetainedPersonProof, VerifiedRetainedPersonEpoch,
};
use runtime_fs::external_history::{EnrolledVerifier, EnrollmentChallenge, EnrollmentContext};
use runtime_portal::{
    fd_transport, process_binding,
    retained_role::{FS_DAEMON_PATH, VerifiedHelperPeer},
};
use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_user_identity::{RevocationList, verify_device_cert_at};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn nonce() -> [u8; 32] {
    let mut bytes = [0; 32];
    let halves = [
        uuid::Uuid::new_v4().into_bytes(),
        uuid::Uuid::new_v4().into_bytes(),
    ];
    for (destination, source) in bytes.iter_mut().zip(halves.into_iter().flatten()) {
        *destination = source;
    }
    bytes
}
fn now_ns() -> io::Result<u64> {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
    )
    .map_err(io::Error::other)
}
/// Every first QPE byte and partial frame is under one absolute deadline.
/// Shutdown reaches the exact socket even when a peer stops mid-frame.
fn bounded<T>(
    stream: &UnixStream,
    cancel: &AtomicBool,
    deadline: Instant,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let held = stream.try_clone()?;
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let watcher = scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                    let _ = held.shutdown(std::net::Shutdown::Both);
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let result = if cancel.load(Ordering::Acquire) {
            Err(refused())
        } else {
            call()
        };
        done.store(true, Ordering::Release);
        if watcher.join().is_err() {
            return Err(refused());
        }
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(refused());
        }
        result
    })
}
fn frame() -> wire_frame::Framing {
    wire_frame::Framing::little_endian(2 * 1024 * 1024).reject_empty()
}
fn receive_file(stream: &mut UnixStream, tag: u8) -> io::Result<File> {
    let (payload, descriptor) = fd_transport::recv_with_fd(stream.as_fd())?;
    if payload != [tag] {
        return Err(refused());
    }
    let descriptor = descriptor.ok_or_else(refused)?;
    stream.write_all(&[tag])?;
    Ok(File::from(descriptor))
}
fn sealed_context(mut file: File) -> io::Result<EnrollmentContext> {
    let metadata = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let required = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if metadata.uid() != 0
        || !metadata.is_file()
        || metadata.nlink() != 0
        || metadata.len() == 0
        || metadata.len() > 32 * 1024
        || seals < 0
        || seals & required != required
    {
        return Err(refused());
    }
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(32 * 1024 + 1).read_to_end(&mut bytes)?;
    EnrollmentContext::decode(&bytes)
}
fn enrollment_selection(binding: &RetainedLaunchBinding, context: &EnrollmentContext) -> bool {
    // Resource/request substitutions must also fail against the exact held
    // canonical binding bytes, not merely remain syntactically valid UUIDs.
    if serde_json::to_vec(binding).ok().as_deref() != Some(context.person_binding.as_slice()) {
        return false;
    }
    if binding.resource_record == "retained-home" {
        return binding.operation_id == context.verifier.source_operation;
    }
    let Some(request) = binding
        .resource_record
        .strip_prefix("selected-open-v2:edit-in-parent:")
    else {
        return false;
    };
    // The binding operation is the protected installed source operation;
    // the selected resource suffix is the independently fresh action request.
    // The root's typed EnrollmentRequest checks that suffix before sealing this
    // exact binding, and certified Shell proof refresh replays the held resource.
    binding.operation_id == context.verifier.source_operation
        && uuid::Uuid::parse_str(request).is_ok_and(|id| !id.is_nil() && id.to_string() == request)
}
fn own_program() -> io::Result<File> {
    for path in ["/", "/usr", "/usr/bin"] {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(refused());
        }
    }
    let program = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(FS_DAEMON_PATH)?;
    let metadata = program.metadata()?;
    let actual = File::open("/proc/self/exe")?.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || metadata.len() == 0
        || (actual.dev(), actual.ino()) != (metadata.dev(), metadata.ino())
    {
        return Err(refused());
    }
    Ok(program)
}
/// Concrete daemon context, built from the same live certified credentials,
/// revocations, signed workspace authority and canonical publisher as the mount.
/// No record/key/UID received on a socket can construct this factory.
pub struct ExternalHistoryFactory {
    credentials: PeerCredentials,
    revocations: Arc<RwLock<RevocationList>>,
    authority: Arc<WorkspaceAuthority>,
    publisher: Arc<RecordPublisher>,
    program: File,
    durable_revocations: revocations::DurableRevocations,
    runtime: Option<ExternalHistoryRuntime>,
    lanes: std::sync::Mutex<Option<lane_controller::Controller>>,
}
impl ExternalHistoryFactory {
    /// Wrong-workspace mounts intentionally have no external personal factory.
    /// No device-ticket workspace is relabelled as the person's workspace.
    /// # Errors
    /// Refuses missing installed daemon identity or invalid live certification/authority.
    pub fn for_mount(
        credentials: PeerCredentials,
        revocations: Arc<RwLock<RevocationList>>,
        authority: Arc<WorkspaceAuthority>,
        publisher: Arc<RecordPublisher>,
        identity_paths: &tilo_user_identity::IdentityPaths,
        runtime: Option<ExternalHistoryRuntime>,
    ) -> io::Result<Option<Arc<Self>>> {
        let room = collab_types::person_room(&credentials.user_public_key());
        if authority.workspace() != room || publisher.workspace_id() != room {
            return Ok(None);
        }
        let durable_revocations =
            revocations::DurableRevocations::open(identity_paths, credentials.user_public_key())?;
        let value = Arc::new(Self {
            credentials,
            revocations,
            authority,
            publisher,
            program: own_program()?,
            durable_revocations,
            runtime,
            lanes: std::sync::Mutex::new(None),
        });
        value.check_current()?;
        Ok(Some(value))
    }
    fn check_current(&self) -> io::Result<()> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused());
        }
        let actual = own_program()?.metadata()?;
        let held = self.program.metadata()?;
        if (actual.dev(), actual.ino()) != (held.dev(), held.ino()) {
            return Err(refused());
        }
        let cert = self.credentials.cert();
        verify_device_cert_at(cert, now_ns()?).map_err(io::Error::other)?;
        if cert.capabilities.foreign
            || self.credentials.signing_key().verifying_key().to_bytes() != cert.device_public_key
            || self
                .revocations
                .read()
                .map_err(|_| refused())?
                .is_revoked_at(&cert.device_public_key, now_ns()?)
            || self.authority.governor() != cert.user_public_key
            || self.authority.workspace() != collab_types::person_room(&cert.user_public_key)
            || self.publisher.workspace_id() != self.authority.workspace()
            || self.publisher.actor().person.as_bytes() != &cert.user_public_key
            || self.publisher.actor().device.as_bytes() != &cert.device_public_key
        {
            return Err(refused());
        }
        self.authority.reload().map_err(io::Error::other)?;
        if !self
            .authority
            .handle()
            .permits(&cert.user_public_key, "/", crate::acl::Access::Write)
        {
            return Err(refused());
        }
        Ok(())
    }
    /// Switch an explicitly requested existing storage connection into private
    /// enrollment. Tags61/62 are sealed context and genuinely delegated fresh
    /// Shell proof endpoint. The returned owned admission must remain attached
    /// to the later pending/history coordinator; enrollment alone never saves.
    fn enroll_with_purpose(
        self: &Arc<Self>,
        mut stream: UnixStream,
        cancel: &AtomicBool,
        purpose: AdmissionPurpose,
    ) -> io::Result<(ExternalSourceAdmission, UnixStream)> {
        let guard = stream.try_clone()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(refused)?;
        let admission = bounded(&guard, cancel, deadline, || {
            self.check_current()?;
            process_binding::arm(&stream)?;
            let challenge = nonce();
            stream.write_all(&process_binding::MAGIC)?;
            stream.write_all(&challenge)?;
            let helper = Arc::new(process_binding::receive(&stream, &challenge, 0)?);
            let helper = VerifiedHelperPeer::admit(helper)?;
            stream.write_all(&[1])?;
            process_binding::complete(&mut stream)?;
            let daemon_challenge = EnrollmentChallenge {
                schema: 1,
                nonce: nonce(),
                certificate: serde_json::to_vec(self.credentials.cert())
                    .map_err(io::Error::other)?,
                revocations: serde_json::to_vec(
                    &self
                        .revocations
                        .read()
                        .map_err(|_| refused())?
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                )
                .map_err(io::Error::other)?,
            };
            if !daemon_challenge.valid() {
                return Err(refused());
            }
            frame()
                .write(
                    &mut stream,
                    &serde_json::to_vec(&daemon_challenge).map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            let context = sealed_context(receive_file(&mut stream, 61)?)?;
            let proof_stream =
                UnixStream::from(std::os::fd::OwnedFd::from(receive_file(&mut stream, 62)?));
            let binding: RetainedLaunchBinding =
                serde_json::from_slice(&context.person_binding).map_err(io::Error::other)?;
            binding.validate()?;
            if serde_json::to_vec(&binding).map_err(io::Error::other)? != context.person_binding
                || binding.root_pid != helper.peer().pid()
                || !enrollment_selection(&binding, &context)
                || binding.room != context.verifier.personal_room
                || context.verifier.person != self.credentials.user_public_key()
                || context.verifier.device != self.credentials.device_public_key()
                || context.verifier.personal_room != self.authority.workspace().to_string()
            {
                return Err(refused());
            }
            let purpose = if purpose == AdmissionPurpose::HomeHistoryRead {
                // Existing storage read switch carries no caller-selectable
                // purpose. Derive the private refinement ONLY from the held
                // helper-sealed context and exact fresh certified Shell binding.
                AdmissionPurpose::classify_reader(&binding, &context).ok_or_else(refused)?
            } else {
                purpose
            };
            let proof = DelegatedProof::connect(proof_stream, binding, cancel, deadline)?;
            let mut admission = ExternalSourceAdmission {
                factory: Arc::clone(self),
                helper,
                context,
                proof,
                revoked: false,
                daemon_nonce: daemon_challenge.nonce,
                purpose,
            };
            admission.refresh(cancel, deadline)?;
            let mut reply = EnrolledVerifier {
                schema: 1,
                daemon_nonce: daemon_challenge.nonce,
                requester_nonce: admission.context.nonce,
                context_digest: admission.context.digest()?,
                verifier: admission.context.verifier.clone(),
                signature: Vec::new(),
            };
            reply.signature = self
                .credentials
                .signing_key()
                .sign(&reply.signing_bytes()?)
                .to_bytes()
                .to_vec();
            admission.refresh(cancel, deadline)?;
            frame()
                .write(
                    &mut stream,
                    &serde_json::to_vec(&reply).map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            Ok(admission)
        })?;
        Ok((admission, stream))
    }
    /// Actual accepted storage callsite keeps the source enrollment and its
    /// fresh certified proof alive. It exposes no mutation command; the pending
    /// replacement/history coordinator is a separate typed capability step.
    pub(crate) fn serve_enrollment(self: Arc<Self>, stream: UnixStream) -> io::Result<()> {
        self.serve_lane(stream, AdmissionPurpose::SelectedReplace)
    }
    pub(crate) fn serve_read_enrollment(self: Arc<Self>, stream: UnixStream) -> io::Result<()> {
        self.serve_lane(stream, AdmissionPurpose::HomeHistoryRead)
    }
    fn serve_lane(
        self: Arc<Self>,
        stream: UnixStream,
        purpose: AdmissionPurpose,
    ) -> io::Result<()> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (admission, stream) = self.enroll_with_purpose(stream, &cancel, purpose)?;
        let controller = {
            let mut slot = self.lanes.lock().map_err(|_| refused())?;
            if slot.is_none() {
                *slot = Some(lane_controller::Controller::start()?);
            }
            slot.as_ref().ok_or_else(refused)?.clone()
        };
        controller.serve(admission, stream, cancel)
    }
}
struct DelegatedProof {
    stream: Option<UnixStream>,
    shell: process_binding::ExecutingPeer,
    binding: RetainedLaunchBinding,
    epoch: Option<VerifiedRetainedPersonEpoch>,
}
impl DelegatedProof {
    fn connect(
        mut stream: UnixStream,
        binding: RetainedLaunchBinding,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> io::Result<Self> {
        let held = stream.try_clone()?;
        bounded(&held, cancel, deadline, || {
            process_binding::arm(&stream)?;
            let challenge = nonce();
            stream.write_all(&process_binding::MAGIC)?;
            stream.write_all(&challenge)?;
            let shell = process_binding::receive(&stream, &challenge, binding.target_uid)?;
            if !shell.current() || shell.pid() != binding.target_pid || shell.uid() == 0 {
                return Err(refused());
            }
            stream.write_all(&[1])?;
            // Shell independently admits the delegated actual FS daemon task.
            process_binding::complete(&mut stream)?;
            Ok(Self {
                stream: Some(stream),
                shell,
                binding,
                epoch: None,
            })
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdmissionPurpose {
    SelectedReplace,
    HomeHistoryRead,
    SelectedEditorHistoryRead,
}
impl AdmissionPurpose {
    fn history_reader(self) -> bool {
        matches!(
            self,
            Self::HomeHistoryRead | Self::SelectedEditorHistoryRead
        )
    }
    // This only classifies an already-authenticated enrollment. It neither
    // proves Author nor grants Read: fresh root/Shell selection and genuine
    // source-resident ReadSubject are independently mandatory.
    fn classify_reader(
        binding: &RetainedLaunchBinding,
        context: &EnrollmentContext,
    ) -> Option<Self> {
        if !enrollment_selection(binding, context) {
            return None;
        }
        if binding.app_id == "so.tilo.file-explorer" && binding.resource_record == "retained-home" {
            Some(Self::HomeHistoryRead)
        } else if binding
            .resource_record
            .starts_with("selected-open-v2:edit-in-parent:")
        {
            Some(Self::SelectedEditorHistoryRead)
        } else {
            None
        }
    }
    fn matches_selection(
        self,
        binding: &RetainedLaunchBinding,
        context: &EnrollmentContext,
    ) -> bool {
        match self {
            Self::SelectedReplace => enrollment_selection(binding, context),
            Self::HomeHistoryRead | Self::SelectedEditorHistoryRead => {
                Self::classify_reader(binding, context) == Some(self)
            }
        }
    }
}
/// Private opaque live admission. No serialized context/observation constructor.
pub(crate) struct ExternalSourceAdmission {
    factory: Arc<ExternalHistoryFactory>,
    helper: VerifiedHelperPeer,
    context: EnrollmentContext,
    proof: DelegatedProof,
    revoked: bool,
    daemon_nonce: [u8; 32],
    purpose: AdmissionPurpose,
}
impl ExternalSourceAdmission {
    fn verify_refreshed(
        &mut self,
        proof: &SignedRetainedPersonProof,
        challenge: [u8; 32],
    ) -> io::Result<()> {
        let revocations = self
            .factory
            .revocations
            .read()
            .map_err(|_| refused())?
            .clone();
        let entries = proof.authenticated_revocations_for_retained_helper(
            &self.proof.binding,
            &challenge,
            &self.proof.shell,
            self.factory.credentials.cert(),
            &revocations,
            &self.helper,
        )?;
        {
            let mut current = self.factory.revocations.write().map_err(|_| refused())?;
            self.factory
                .durable_revocations
                .merge_and_publish(&mut current, entries)?;
        }
        let revocations = self
            .factory
            .revocations
            .read()
            .map_err(|_| refused())?
            .clone();
        match &self.proof.epoch {
            Some(epoch) => epoch.revalidate_for_retained_helper(
                proof,
                &challenge,
                &self.proof.shell,
                self.factory.credentials.cert(),
                &revocations,
                &self.helper,
            )?,
            None => {
                self.proof.epoch = Some(proof.verify_for_retained_helper(
                    &self.proof.binding,
                    &challenge,
                    &self.proof.shell,
                    self.factory.credentials.cert(),
                    &revocations,
                    &self.helper,
                )?);
            }
        }
        self.factory.check_current()?;
        self.helper.check_current()?;
        Ok(())
    }
    fn revoke_proof(&mut self) {
        self.revoked = true;
        if let Some(epoch) = &self.proof.epoch {
            epoch.revoke();
        }
        if let Some(stream) = &self.proof.stream {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
    fn check_proof_role(&self) -> io::Result<()> {
        if self.revoked
            || !self.proof.shell.current()
            || !self
                .purpose
                .matches_selection(&self.proof.binding, &self.context)
        {
            return Err(refused());
        }
        self.factory.check_current()?;
        self.helper.check_current()
    }
    fn refresh(&mut self, cancel: &AtomicBool, deadline: Instant) -> io::Result<()> {
        self.check_proof_role()?;
        let mut stream = self.proof.stream.take().ok_or_else(refused)?;
        let result = (|| {
            let held = stream.try_clone()?;
            bounded(&held, cancel, deadline, || {
                let challenge = nonce();
                stream.write_all(&challenge)?;
                let bytes = wire_frame::Framing::little_endian(MAX_PROOF_BYTES)
                    .reject_empty()
                    .read(&mut stream)
                    .map_err(io::Error::other)?
                    .ok_or_else(refused)?;
                Ok((SignedRetainedPersonProof::decode(&bytes)?, challenge))
            })
        })();
        self.proof.stream = Some(stream);
        let result = result.and_then(|(proof, challenge)| self.verify_refreshed(&proof, challenge));
        if result.is_err() {
            self.revoke_proof();
        }
        result
    }
    fn refresh_shared<'a>(
        admission: &'a std::rc::Rc<std::cell::RefCell<Self>>,
        cancel: &'a AtomicBool,
        deadline: Instant,
    ) -> LocalFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let stream = {
                let mut current = admission.try_borrow_mut().map_err(|_| refused())?;
                current.check_proof_role()?;
                current.proof.stream.take().ok_or_else(refused)?
            };
            // A missing stream serializes refreshes fail-closed. Cancellation must
            // not leave a live admission after losing custody of its proof IO.
            let mut turn = ProofRefreshTurn {
                admission,
                completed: false,
            };
            let returned = owned_io::exchange(stream, cancel, deadline, |stream| {
                let challenge = nonce();
                stream.write_all(&challenge)?;
                let bytes = wire_frame::Framing::little_endian(MAX_PROOF_BYTES)
                    .reject_empty()
                    .read(stream)
                    .map_err(io::Error::other)?
                    .ok_or_else(refused)?;
                Ok((SignedRetainedPersonProof::decode(&bytes)?, challenge))
            })
            .await;
            let result = match returned {
                Ok((stream, result)) => {
                    let mut current = admission.try_borrow_mut().map_err(|_| refused())?;
                    current.proof.stream = Some(stream);
                    // Recheck live state, never a snapshot from before the await.
                    current.check_proof_role()?;
                    result
                        .and_then(|(proof, challenge)| current.verify_refreshed(&proof, challenge))
                }
                Err(error) => Err(error),
            };
            if result.is_ok() {
                turn.completed = true;
            }
            result
        })
    }
}

struct ProofRefreshTurn<'a> {
    admission: &'a std::rc::Rc<std::cell::RefCell<ExternalSourceAdmission>>,
    completed: bool,
}
impl Drop for ProofRefreshTurn<'_> {
    fn drop(&mut self) {
        if !self.completed
            && let Ok(mut admission) = self.admission.try_borrow_mut()
        {
            admission.revoke_proof();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reader_refinement_preserves_files_scope_and_refuses_readonly_or_generic_editor() {
        let source = "11111111-1111-4111-8111-111111111111";
        let selected = "33333333-3333-4333-8333-333333333333";
        let room = "22222222-2222-4222-8222-222222222222";
        let mut binding = RetainedLaunchBinding {
            operation_id: source.into(),
            app_id: "so.tilo.file-explorer".into(),
            package_sha256: "11".repeat(32),
            room: room.into(),
            resource_record: "retained-home".into(),
            target_uid: 1000,
            target_pid: 10,
            root_pid: 11,
            context_sha256: "22".repeat(32),
        };
        let mut context = EnrollmentContext {
            schema: 1,
            nonce: [1; 32],
            verifier: runtime_fs::external_history::VerifierClaims {
                schema: 1,
                source_owner: [2; 32],
                source_operation: source.into(),
                person: [3; 32],
                personal_room: room.into(),
                device: [4; 32],
            },
            person_binding: serde_json::to_vec(&binding).unwrap(),
        };
        // Classification is vocabulary only; no actual admission/proof/source
        // Read capability is constructed by this regression.
        assert!(matches!(
            AdmissionPurpose::classify_reader(&binding, &context),
            Some(AdmissionPurpose::HomeHistoryRead)
        ));
        binding.app_id = "so.tilo.word-processor".into();
        assert!(AdmissionPurpose::classify_reader(&binding, &context).is_none());
        // Canonical producer serializes the SOURCE operation here, while the
        // resource carries the separate selected REQUEST UUID.
        binding.resource_record = format!("selected-open-v2:edit-in-parent:{selected}");
        binding.validate().unwrap();
        let serialized = serde_json::to_vec(&binding).unwrap();
        context.person_binding = serialized.clone();
        binding = serde_json::from_slice(&serialized).unwrap();
        assert_ne!(binding.operation_id, selected);
        assert!(enrollment_selection(&binding, &context));
        assert!(AdmissionPurpose::SelectedReplace.matches_selection(&binding, &context));
        context.verifier.source_operation = selected.into();
        assert!(!enrollment_selection(&binding, &context));
        assert!(!AdmissionPurpose::SelectedReplace.matches_selection(&binding, &context));
        context.verifier.source_operation = source.into();
        assert!(matches!(
            AdmissionPurpose::classify_reader(&binding, &context),
            Some(AdmissionPurpose::SelectedEditorHistoryRead)
        ));
        assert!(!AdmissionPurpose::HomeHistoryRead.matches_selection(&binding, &context));
        assert!(AdmissionPurpose::SelectedEditorHistoryRead.matches_selection(&binding, &context));
        let actual_selected_binding = binding.clone();
        binding.resource_record = format!("selected-open-v2:edit-in-parent:{source}");
        // Still a canonical nonnil UUID, but not the held selected request.
        assert!(!enrollment_selection(&binding, &context));
        assert!(AdmissionPurpose::classify_reader(&binding, &context).is_none());
        binding = actual_selected_binding;
        binding.resource_record = format!("selected-open-v2:read-only:{selected}");
        assert!(AdmissionPurpose::classify_reader(&binding, &context).is_none());
        binding.resource_record = format!("selected-open-v2:edit-in-parent:{selected}");
        binding.operation_id = selected.into();
        assert!(!enrollment_selection(&binding, &context));
        assert!(AdmissionPurpose::classify_reader(&binding, &context).is_none());
        binding.operation_id = source.into();
        binding.resource_record =
            "selected-open-v2:edit-in-parent:00000000-0000-0000-0000-000000000000".into();
        assert!(!enrollment_selection(&binding, &context));
        binding.resource_record = "selected-open-v2:edit-in-parent:../record".into();
        assert!(AdmissionPurpose::classify_reader(&binding, &context).is_none());
        assert!(!AdmissionPurpose::SelectedReplace.history_reader());
    }
    #[test]
    fn stalled_first_proof_io_observes_one_absolute_deadline() {
        let (mut stream, _silent_peer) = UnixStream::pair().unwrap();
        let guard = stream.try_clone().unwrap();
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let result = bounded(
            &guard,
            &cancel,
            started.checked_add(Duration::from_millis(80)).unwrap(),
            || {
                let mut bytes = [0; 32];
                stream.read_exact(&mut bytes)
            },
        );
        // The peer never answers: returning at all is the deadline's doing.
        assert!(result.is_err());
    }
    #[test]
    fn cancellation_shuts_exact_stalled_proof_socket_and_joins_watcher() {
        let (mut stream, _silent_peer) = UnixStream::pair().unwrap();
        let guard = stream.try_clone().unwrap();
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(40));
                cancel.store(true, Ordering::Release);
            });
            // The deadline is an hour away: returning at all is the
            // cancellation's doing.
            let result = bounded(
                &guard,
                &cancel,
                started.checked_add(Duration::from_secs(3600)).unwrap(),
                || {
                    let mut bytes = [0; 32];
                    stream.read_exact(&mut bytes)
                },
            );
            assert!(result.is_err());
        });
    }
    #[test]
    fn mutable_regular_context_is_not_root_sealed_admission() {
        let file = tempfile::tempfile().unwrap();
        assert!(sealed_context(file).is_err());
    }
}

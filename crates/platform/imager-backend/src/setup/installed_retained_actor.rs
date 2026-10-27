//! Installed root admission on the existing mutually authenticated native stream.
//! Caller labels select a request, never credentials, source paths or authority.
//! No public dispatcher, process spawn or positive launch status lives here.
use super::{
    home_capture::{RetainedSourceAuthority, RetainedSourceProjection},
    prepared_setup::{AuthenticatedInstalledRetained, IdentityKind, InstalledPersonAnchor},
};
use runtime_collab::retained_person::{
    MAX_PROOF_BYTES, RetainedLaunchBinding, SignedRetainedPersonProof, VerifiedRetainedPersonEpoch,
};
use runtime_portal::process_binding::{self, ExecutingPeer};
use std::os::fd::AsFd;
use std::{
    fs::File,
    io::{self, Write},
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tilo_user_identity::{DeviceCert, RevocationList};

mod original_copy;
pub use original_copy::ReviewedInstalledOriginalCopy;

pub use crate::retained_source::launch_request::LaunchRequest;
#[cfg(test)]
use crate::retained_source::launch_request::MAX_REQUEST;
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Installed retained launch refused",
    )
}
fn root() -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(refused());
    }
    Ok(())
}
fn launch_binding(
    request: &LaunchRequest,
    peer: &ExecutingPeer,
) -> io::Result<RetainedLaunchBinding> {
    let value = RetainedLaunchBinding {
        operation_id: request.operation_id().to_owned(),
        app_id: request.app_id().to_owned(),
        package_sha256: request.package_sha256().to_owned(),
        room: request.room().to_owned(),
        resource_record: request.resource_record().to_owned(),
        target_uid: peer.uid(),
        target_pid: peer.pid(),
        root_pid: std::process::id(),
        context_sha256: request.context_sha256().to_owned(),
    };
    value.validate()?;
    Ok(value)
}

/// Implemented ONLY by the paired root-native installed custody adapter.
/// There is no CLI constructor/default and no wire-selected certificate store.
/// Every current() must reopen/revalidate its canonical installed certificate
/// and CURRENT revocations; errors cannot fall back to an old snapshot.
pub(in crate::setup) trait RootInstalledPersonAnchor: Send + Sync {
    fn current(&self) -> io::Result<(DeviceCert, RevocationList)>;
    /// Only entries extracted after fresh exact proof/device/root-signature
    /// verification reach this seam. The real adapter must validate signatures
    /// again and preserve every previously authenticated entry monotonically.
    fn refresh_verified(&self, entries: &[tilo_user_identity::RevocationEntry]) -> io::Result<()>;
}

struct ProofClient {
    stream: UnixStream,
    peer: Arc<ExecutingPeer>,
    binding: RetainedLaunchBinding,
    epoch: Option<VerifiedRetainedPersonEpoch>,
}
fn challenge() -> [u8; 32] {
    let mut value = [0; 32];
    value[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    value[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    value
}
const PROOF_TOTAL_LIMIT: Duration = Duration::from_secs(10);

pub use crate::retained_source::deadline::{
    ProofInterrupt as InstalledProofInterrupt, with_proof_deadline_at,
};
fn with_proof_deadline<T>(
    stream: &UnixStream,
    cancel: &AtomicBool,
    run: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    with_proof_deadline_at(stream, cancel, Instant::now() + PROOF_TOTAL_LIMIT, run)
}

impl ProofClient {
    fn connect(
        mut stream: UnixStream,
        request: &LaunchRequest,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        root()?;
        let held = stream.try_clone()?;
        with_proof_deadline(&held, cancel, || {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            stream.set_write_timeout(Some(Duration::from_secs(10)))?;
            process_binding::arm(&stream)?;
            let nonce = challenge();
            stream.write_all(&process_binding::MAGIC)?;
            stream.write_all(&nonce)?;
            let peer = process_binding::receive_nonroot_for_authentication(&stream, &nonce)?;
            if !peer.current() || peer.uid() == 0 {
                return Err(refused());
            }
            let binding = launch_binding(request, &peer)?;
            stream.write_all(&[1])?;
            // The Shell pins this exact root task before it signs any fresh reply.
            process_binding::complete(&mut stream)?;
            // complete() clears its handshake timeouts: restore bounded proof I/O.
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            stream.set_write_timeout(Some(Duration::from_secs(10)))?;
            if !peer.current() {
                return Err(refused());
            }
            Ok(Self {
                stream,
                peer: Arc::new(peer),
                binding,
                epoch: None,
            })
        })
    }
    fn prove(
        &mut self,
        anchor: &dyn RootInstalledPersonAnchor,
        cancel: &AtomicBool,
    ) -> io::Result<DeviceCert> {
        let result = (|| {
            root()?;
            let held = self.stream.try_clone()?;
            with_proof_deadline(&held, cancel, || self.prove_current(anchor))
        })();
        if result.is_err() {
            if let Some(epoch) = &self.epoch {
                epoch.revoke();
            }
        }
        result
    }
    fn prove_current(&mut self, anchor: &dyn RootInstalledPersonAnchor) -> io::Result<DeviceCert> {
        if !self.peer.current() {
            return Err(refused());
        }
        let (certificate, revocations) = anchor.current()?;
        let nonce = challenge();
        self.stream.write_all(&nonce)?;
        let bytes = wire_frame::Framing::little_endian(MAX_PROOF_BYTES)
            .reject_empty()
            .read(&mut self.stream)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        let proof = SignedRetainedPersonProof::decode(&bytes)?;
        // Extract only after authenticating this exact fresh response against
        // the installed certificate and already protected root revocations.
        let entries = proof.authenticated_revocations(
            &self.binding,
            &nonce,
            &self.peer,
            &certificate,
            &revocations,
        )?;
        anchor.refresh_verified(&entries)?;
        let (current, current_revocations) = anchor.current()?;
        // Refresh is monotonic and reload is mandatory. Newly learned device
        // revocation or replaced certificate must refuse BEFORE epoch admission.
        match &self.epoch {
            Some(epoch) => {
                epoch.revalidate(&proof, &nonce, &self.peer, &current, &current_revocations)?;
            }
            None => {
                self.epoch = Some(proof.verify(
                    &self.binding,
                    &nonce,
                    &self.peer,
                    &current,
                    &current_revocations,
                )?);
            }
        }
        Ok(current)
    }
}

fn check_editor_origin(
    origin: &super::retained_open_selection::SelectedOriginCustody,
    cancel: &AtomicBool,
) -> io::Result<()> {
    super::migration::check_cancel(cancel)?;
    // New-editor cancellation cannot become Files actor revocation. Actual
    // Files peer/certificate/source checks still latch their own failures.
    origin.check(&AtomicBool::new(false))?;
    super::migration::check_cancel(cancel)
}

/// Private constructor; request JSON, a serialized declaration, FD or signer
/// labels cannot mint this admission. It is not Serialize/Deserialize/Clone.
pub struct ReviewedInstalledRetainedLaunch {
    installed: Arc<AuthenticatedInstalledRetained>,
    source: Arc<RetainedSourceAuthority>,
    proof: Mutex<ProofClient>,
    proof_interrupt: Arc<InstalledProofInterrupt>,
    anchor: Arc<dyn RootInstalledPersonAnchor>,
    observed: Option<tilo_identity_host::enrolled_transport::RootObservedCurrentDevice>,
    binding: RetainedLaunchBinding,
    revoked: AtomicBool,
}
impl ReviewedInstalledRetainedLaunch {
    pub fn retirement_pending() -> bool {
        RetainedSourceAuthority::unknown_retirement()
    }

    /// The caller supplies only the EXISTING connected Shell/root stream and
    /// closed scope. Root custody is opened internally from authenticated A;
    /// callers cannot inject an anchor. Nothing is launched or advertised.
    pub(in crate::setup) fn review(
        stream: UnixStream,
        request: LaunchRequest,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        Self::review_current(stream, request, None, cancel)
    }
    /// Fresh editor certificate/QPE admission derives physical source custody
    /// only from a genuine still-live Files selection, never a mounted path.
    pub(in crate::setup) fn review_editor(
        stream: UnixStream,
        request: LaunchRequest,
        origin: &super::retained_open_selection::SelectedOriginCustody,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        if !request
            .resource_record()
            .strip_prefix("selected-open-v2:edit-in-parent:")
            .is_some_and(|value| {
                uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
            })
        {
            return Err(refused());
        }
        Self::review_current(stream, request, Some(origin), cancel)
    }
    fn review_current(
        stream: UnixStream,
        request: LaunchRequest,
        origin: Option<&super::retained_open_selection::SelectedOriginCustody>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        root()?;
        super::migration::check_cancel(cancel)?;
        let installed = AuthenticatedInstalledRetained::open_installed(request.operation_id())?;
        installed.revalidate()?;
        let manifest = installed.manifest();
        let plan = manifest
            .retained_home
            .as_ref()
            .ok_or_else(refused)?
            .as_plan();
        // Original credentials remain authenticated source facts. The target
        // identity is independently verified from the actual executing Shell
        // and installed certified personal session, not inferred from this UID.
        if plan.home_uid() == 0 {
            return Err(refused());
        }
        if matches!(manifest.identity_kind, IdentityKind::CurrentDevice { .. }) {
            return Err(refused());
        }
        let person = manifest.identity_public_key;
        if let Some(origin) = origin {
            check_editor_origin(origin, cancel)?;
            if origin.input.reviewed.installed.manifest() != manifest
                || origin.input.binding().operation_id != request.operation_id()
                || origin.input.person_public_key() != person
            {
                return Err(refused());
            }
        }
        // B's initial retained group route is exclusively the canonical personal
        // room. Reject shared-workspace context BEFORE opening any RW source.
        if request.room() != runtime_collab::retained_person::personal_room(&person) {
            return Err(refused());
        }
        let mut proof = ProofClient::connect(stream, &request, cancel)?;
        let held = proof.stream.try_clone()?;
        let directory = with_proof_deadline(&held, cancel, || {
            let (bytes, descriptor) =
                runtime_portal::fd_transport::recv_with_fd(proof.stream.as_fd())?;
            if bytes != b"TILORHD1" || !proof.peer.current() {
                return Err(refused());
            }
            descriptor.map(File::from).ok_or_else(refused)
        })?;
        let (device_public_key, observed) = match manifest.identity_kind {
            IdentityKind::EnrolledDevice { device_public_key } => {
                drop(directory);
                (device_public_key, None)
            }
            IdentityKind::SingleDeviceBackup => {
                let current =
                    tilo_identity_host::enrolled_transport::RootObservedCurrentDevice::review(
                        directory,
                        proof.peer.uid(),
                    )?;
                super::prepared_setup::publish_retained_primary_owner(
                    &installed,
                    &current,
                    &proof.peer,
                )?;
                (current.device_public_key(), Some(current))
            }
            IdentityKind::CurrentDevice { .. } => return Err(refused()),
        };
        let anchor: Arc<dyn RootInstalledPersonAnchor> =
            InstalledPersonAnchor::open(person, device_public_key)?;
        let certificate = proof.prove(anchor.as_ref(), cancel)?;
        if certificate.user_public_key != person
            || certificate.device_public_key != device_public_key
        {
            return Err(refused());
        }
        installed.revalidate()?;
        // Pin the exact new proof socket before allocating any source lease;
        // descriptor failure cannot abandon freshly mounted physical custody.
        let proof_interrupt = Arc::new(InstalledProofInterrupt::observe(&proof.stream)?);
        let source = if let Some(origin) = origin {
            check_editor_origin(origin, cancel)?;
            origin
                .input
                .projection
                .derive_installed(Arc::clone(&installed), &person, cancel)?
        } else {
            RetainedSourceAuthority::review_installed(Arc::clone(&installed), &person, cancel)?
        };
        if source.identity_public_key() != &person {
            source.revoke();
            source.retire()?;
            return Err(refused());
        }
        let binding = proof.binding.clone();
        let value = Arc::new(Self {
            installed,
            source,
            proof: Mutex::new(proof),
            proof_interrupt,
            anchor,
            observed,
            binding,
            revoked: AtomicBool::new(false),
        });
        let result = value.revalidate(cancel).and_then(|()| {
            if let Some(origin) = origin {
                check_editor_origin(origin, cancel)?;
                if origin.input.reviewed.installed.manifest() != value.installed.manifest()
                    || origin.input.binding().operation_id != value.binding.operation_id
                    || origin.input.person_public_key()
                        != value.installed.manifest().identity_public_key
                {
                    return Err(refused());
                }
            }
            Ok(())
        });
        if let Err(error) = result {
            value.retire()?;
            return Err(error);
        }
        Ok(value)
    }
    pub fn binding(&self) -> &RetainedLaunchBinding {
        &self.binding
    }
    /// Share only the kernel peer authenticated by this actor's fresh certified
    /// context. Routing a consuming child never rewrites the original Shell PID.
    pub fn target_peer(&self, cancel: &AtomicBool) -> io::Result<Arc<ExecutingPeer>> {
        self.revalidate(cancel)?;
        let peer = {
            let proof = self.proof.lock().map_err(|_| refused())?;
            if !proof.peer.current()
                || proof.peer.uid() != self.binding.target_uid
                || proof.peer.pid() != self.binding.target_pid
            {
                return Err(refused());
            }
            Arc::clone(&proof.peer)
        };
        self.revalidate(cancel)?;
        if !peer.current() {
            return Err(refused());
        }
        Ok(peer)
    }
    fn target_credentials(&self, cancel: &AtomicBool) -> io::Result<(u32, u32)> {
        // Target native credentials belong to the authenticated live Shell peer,
        // never the original filesystem owner or a target-side NSS lookup.
        self.revalidate(cancel)?;
        let credentials = {
            let proof = self.proof.lock().map_err(|_| refused())?;
            if !proof.peer.current()
                || proof.peer.uid() != self.binding.target_uid
                || proof.peer.pid() != self.binding.target_pid
            {
                return Err(refused());
            }
            (proof.peer.uid(), proof.peer.gid())
        };
        self.revalidate(cancel)?;
        Ok(credentials)
    }
    pub fn revoke(&self) {
        self.revoked
            .store(true, std::sync::atomic::Ordering::Release);
        self.source.revoke();
    }
    pub fn revalidate(&self, cancel: &AtomicBool) -> io::Result<()> {
        let result = self.revalidate_current(cancel);
        if result.is_err() {
            self.revoke();
        }
        result
    }
    fn revalidate_current(&self, cancel: &AtomicBool) -> io::Result<()> {
        root()?;
        super::migration::check_cancel(cancel)?;
        if self.revoked.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused());
        }
        self.installed.revalidate()?;
        self.source.revalidate(cancel)?;
        let certificate = self
            .proof
            .lock()
            .map_err(|_| refused())?
            .prove(self.anchor.as_ref(), cancel)?;
        let manifest = self.installed.manifest();
        let device_public_key = match (&manifest.identity_kind, &self.observed) {
            (IdentityKind::EnrolledDevice { device_public_key }, None) => *device_public_key,
            (IdentityKind::SingleDeviceBackup, Some(current)) => {
                current.validate()?;
                if current.public_key() != manifest.identity_public_key {
                    return Err(refused());
                }
                current.device_public_key()
            }
            _ => return Err(refused()),
        };
        if certificate.user_public_key != manifest.identity_public_key
            || certificate.device_public_key != device_public_key
        {
            return Err(refused());
        }
        self.installed.revalidate()?;
        self.source.revalidate(cancel)?;
        if self.revoked.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused());
        }
        Ok(())
    }
    /// The canonical namespace owner MUST retain this inseparable input until
    /// actual namespace death. No cloned directory alone is a valid handoff.
    pub fn launch_input(
        self: &Arc<Self>,
        cancel: &AtomicBool,
    ) -> io::Result<InstalledRetainedLaunchInput> {
        self.revalidate(cancel)?;
        let projection = self.source.projection(cancel)?;
        self.revalidate(cancel)?;
        Ok(InstalledRetainedLaunchInput {
            reviewed: Arc::clone(self),
            projection,
        })
    }
    /// Call only AFTER supervised namespace death and input/launcher custody
    /// have actually been dropped. Outstanding custody refuses retirement.
    pub fn retire(self: Arc<Self>) -> io::Result<()> {
        let value = Arc::try_unwrap(self).map_err(|_| refused())?;
        let Self {
            installed,
            source,
            proof,
            proof_interrupt,
            anchor,
            observed,
            binding,
            revoked,
        } = value;
        drop((
            installed,
            proof,
            proof_interrupt,
            anchor,
            observed,
            binding,
            revoked,
        ));
        source.retire()
    }
}

/// Native launcher adapter only. No wire constructor and no positive status;
/// retain in the actual NamespaceOwner through observation of namespace death.
pub struct InstalledRetainedLaunchInput {
    reviewed: Arc<ReviewedInstalledRetainedLaunch>,
    projection: RetainedSourceProjection,
}
impl InstalledRetainedLaunchInput {
    /// Canonical enrolled material from the held installed anchor; a daemon
    /// certificate or public verifier record can never select these keys.
    pub fn current_history_material(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<(DeviceCert, RevocationList)> {
        self.revalidate(cancel)?;
        let (certificate, revocations) = self.reviewed.anchor.current()?;
        if certificate.user_public_key != self.person_public_key()
            || revocations.is_revoked(&certificate.device_public_key)
        {
            return Err(refused());
        }
        self.revalidate(cancel)?;
        Ok((certificate, revocations))
    }
    pub fn native_owner_sha256(&self, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
        self.revalidate(cancel)?;
        let owner = self.projection.native_owner_sha256(cancel)?;
        self.revalidate(cancel)?;
        Ok(owner)
    }
    pub fn proof_interrupt(&self) -> Arc<InstalledProofInterrupt> {
        Arc::clone(&self.reviewed.proof_interrupt)
    }
    pub fn removal_namespace(&self, cancel: &AtomicBool) -> io::Result<File> {
        self.revalidate(cancel)?;
        let directory = self.reviewed.source.removal_namespace(cancel)?;
        self.revalidate(cancel)?;
        Ok(directory)
    }
    pub fn directory(&self) -> &File {
        self.projection.directory()
    }
    pub fn person_public_key(&self) -> [u8; 32] {
        self.reviewed.installed.manifest().identity_public_key
    }
    pub fn original(&self) -> &super::home_capture::OriginalCallerFacts {
        self.projection.original()
    }
    pub fn binding(&self) -> &RetainedLaunchBinding {
        self.reviewed.binding()
    }
    pub fn target_credentials(&self, cancel: &AtomicBool) -> io::Result<(u32, u32)> {
        self.reviewed.target_credentials(cancel)
    }
    pub fn revalidate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.reviewed.revalidate(cancel)?;
        self.projection.revalidate(cancel)
    }
    pub fn original_home_alias(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<&launcher::retained_view::OriginalHomeAlias> {
        self.revalidate(cancel)?;
        self.projection.original_home_alias(cancel)
    }
    /// A current ordinary Files launch may request independently enrolled
    /// history Read. Original source consent is separate from the source kernel
    /// Read checked when a readable native subject is actually opened.
    pub fn check_home_history_read(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.revalidate(cancel)?;
        let binding = self.binding();
        let plan = self
            .reviewed
            .installed
            .manifest()
            .retained_home
            .as_ref()
            .ok_or_else(refused)?
            .as_plan();
        if binding.app_id != "so.tilo.file-explorer"
            || binding.resource_record != "retained-home"
            || binding.room
                != runtime_collab::retained_person::personal_room(&self.person_public_key())
            || !plan.categories.home
            || !matches!(
                plan.home_policy,
                super::migration_handoff::HomePolicy::AccessReadOnly
                    | super::migration_handoff::HomePolicy::AccessReadWrite
            )
        {
            return Err(refused());
        }
        self.revalidate(cancel)
    }
    /// Zero-effect enrollment context for a selected editor Reader. Native Read
    /// rights and actual target Author are checked by the later opaque Reader
    /// refinement and source-held readable subject, never this context string.
    pub fn check_selected_editor_history_read(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.check_selected_parent_write(cancel)?;
        let binding = self.binding();
        let id = binding
            .resource_record
            .strip_prefix("selected-open-v2:edit-in-parent:")
            .ok_or_else(refused)?;
        let parsed = uuid::Uuid::parse_str(id).map_err(io::Error::other)?;
        if parsed.is_nil()
            || parsed.to_string() != id
            || binding.room
                != runtime_collab::retained_person::personal_room(&self.person_public_key())
        {
            return Err(refused());
        }
        self.revalidate(cancel)
    }
    /// Only the independently authenticated installed source record determines
    /// write consent; reported transfer/resize DTOs and Home mode cannot grant it.
    pub fn check_selected_parent_write(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.revalidate(cancel)?;
        let manifest = self.reviewed.installed.manifest();
        let plan = manifest
            .retained_home
            .as_ref()
            .ok_or_else(refused)?
            .as_plan();
        if !plan.categories.home
            || plan.home_policy != super::migration_handoff::HomePolicy::AccessReadWrite
        {
            return Err(refused());
        }
        self.revalidate(cancel)
    }
    pub fn revoke(&self) {
        self.reviewed.revoke();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    #[test]
    fn unclassified_observation_refuses_root_sender_and_unprivileged_receiver() {
        let (mut host, mut sender) = UnixStream::pair().unwrap();
        let nonce = challenge();
        if rustix::process::getuid().as_raw() == 0 && rustix::process::geteuid().as_raw() == 0 {
            let worker = std::thread::spawn(move || process_binding::complete(&mut sender));
            host.set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            host.set_write_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            process_binding::arm(&host).unwrap();
            host.write_all(&process_binding::MAGIC).unwrap();
            host.write_all(&nonce).unwrap();
            let error = process_binding::receive_nonroot_for_authentication(&host, &nonce)
                .err()
                .expect("root sender cannot become a target person");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            drop(host);
            assert!(
                worker.join().unwrap().is_err(),
                "no authentication ACK was issued"
            );
        } else {
            let error = process_binding::receive_nonroot_for_authentication(&host, &nonce)
                .err()
                .expect("ordinary caller cannot bypass expected-UID policy");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            sender.set_nonblocking(true).unwrap();
            let mut byte = [0];
            use std::io::Read;
            assert_eq!(
                sender.read(&mut byte).unwrap_err().kind(),
                io::ErrorKind::WouldBlock,
                "refusal precedes exchange or authority admission"
            );
        }
    }
    #[test]
    fn partial_proof_progress_never_renews_absolute_deadline() {
        use std::io::Read;
        let (mut receiver, mut sender) = UnixStream::pair().unwrap();
        let held = receiver.try_clone().unwrap();
        let progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sent = progress.clone();
        let (ready, observed) = std::sync::mpsc::sync_channel(1);
        let (mut unrelated, mut peer) = UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            sender
                .set_write_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            sender.write_all(&64_u32.to_le_bytes()).unwrap();
            sender.write_all(&[7, 7]).unwrap();
            sent.store(2, Ordering::Release);
            ready.send(()).unwrap();
            for _ in 2..64 {
                if sender.write_all(&[7]).is_err() {
                    break;
                }
                sent.fetch_add(1, Ordering::Release);
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        tilo_test_env::recv("the partial payload to be sent", &observed);
        let start = Instant::now();
        let error = with_proof_deadline_at(
            &held,
            &AtomicBool::new(false),
            start + Duration::from_millis(200),
            || {
                wire_frame::Framing::little_endian(MAX_PROOF_BYTES)
                    .reject_empty()
                    .read(&mut receiver)
                    .map_err(io::Error::other)
            },
        )
        .unwrap_err();
        writer.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        // The payload trickles in over more than a second, so a deadline
        // renewed by progress would have read it whole.
        assert!(
            progress.load(Ordering::Acquire) >= 2,
            "actual partial payload arrived"
        );
        // Shutdown is confined to the held proof socket, not another transport.
        peer.write_all(&[9]).unwrap();
        let mut byte = [0];
        unrelated.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [9]);
    }

    #[test]
    fn cancelled_proof_fence_refuses_before_first_io() {
        use std::io::Read;
        let (receiver, mut sender) = UnixStream::pair().unwrap();
        sender
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut invoked = false;
        let error = with_proof_deadline(&receiver, &AtomicBool::new(true), || {
            invoked = true;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!invoked);
        assert_eq!(sender.read(&mut [0]).unwrap(), 0, "exact socket shut down");
    }

    #[cfg(feature = "native-capture-qualification")]
    #[test]
    fn stalled_initial_qpe_is_interrupted_by_actual_cancellation() {
        use std::io::Read;
        assert_eq!(
            rustix::process::getuid().as_raw(),
            0,
            "explicit native root qualifier"
        );
        assert_eq!(rustix::process::geteuid().as_raw(), 0);
        let (host, mut peer) = UnixStream::pair().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let peer_cancel = cancel.clone();
        let stalled = std::thread::spawn(move || {
            peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut magic = [0; 4];
            peer.read_exact(&mut magic).unwrap();
            assert_eq!(magic, process_binding::MAGIC);
            let mut nonce = [0; 32];
            peer.read_exact(&mut nonce).unwrap();
            assert_ne!(nonce, [0; 32]);
            // No credentials, certificate, ACK or positive authority is forged.
            peer_cancel.store(true, Ordering::Release);
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        });
        let request = LaunchRequest::decode(&serde_json::to_vec(&request()).unwrap()).unwrap();
        let error = ProofClient::connect(host, &request, &cancel).err().unwrap();
        stalled.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    fn request() -> serde_json::Value {
        serde_json::json!({"schema":1,"operation_id":"11111111-1111-4111-8111-111111111111",
            "app_id":"so.tilo.files","package_sha256":"a".repeat(64),
            "room":"22222222-2222-4222-8222-222222222222","resource_record":"home",
            "context_sha256":"b".repeat(64)})
    }
    #[test]
    fn closed_launch_request_refuses_credentials_paths_commands_and_invalid_scope() {
        LaunchRequest::decode(&serde_json::to_vec(&request()).unwrap()).unwrap();
        for name in [
            "argv",
            "source",
            "groups",
            "target_uid",
            "target_gid",
            "target_pid",
            "root_pid",
            "active_person",
        ] {
            let mut value = request();
            value[name] = serde_json::json!("untrusted");
            assert!(LaunchRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for (name, invalid) in [
            ("schema", serde_json::json!(2)),
            ("operation_id", serde_json::json!("../request")),
            ("room", serde_json::json!("not-a-room")),
            ("package_sha256", serde_json::json!("old-or-unobserved")),
            ("context_sha256", serde_json::json!("a".repeat(65))),
            ("app_id", serde_json::json!("")),
            ("resource_record", serde_json::json!("changed\nrecord")),
        ] {
            let mut value = request();
            value[name] = invalid;
            assert!(LaunchRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        assert!(LaunchRequest::decode(&vec![b' '; MAX_REQUEST + 1]).is_err());
        let duplicate = serde_json::to_string(&request()).unwrap();
        let duplicate = format!("{},\"schema\":1}}", duplicate.trim_end_matches('}'));
        assert!(LaunchRequest::decode(duplicate.as_bytes()).is_err());
    }
}

#[cfg(all(test, feature = "native-capture-qualification"))]
mod different_uid_native {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    const CHILD: &str = "TILO_RETAINED_EXECUTING_PEER_TEST";
    const SELECTOR: &str = "setup::installed_retained_actor::different_uid_native::root_observes_actual_nonroot_task_without_inferring_original_credentials";

    #[test]
    fn root_observes_actual_nonroot_task_without_inferring_original_credentials() {
        if std::env::var_os(CHILD).is_some() {
            assert_eq!(rustix::process::getuid().as_raw(), 1004);
            assert_eq!(unsafe { libc::getgid() }, 1005);
            let mut stream = unsafe { UnixStream::from_raw_fd(42) };
            process_binding::complete(&mut stream).expect("actual dropped task binding");
            return;
        }
        assert_eq!(
            rustix::process::getuid().as_raw(),
            0,
            "explicit native root qualifier"
        );
        assert_eq!(rustix::process::geteuid().as_raw(), 0);
        let executable =
            File::open("/proc/self/exe").expect("exact running executable, including memfd");
        let (mut root_stream, child_stream) = UnixStream::pair().expect("owned private stream");
        // Keep originals above both fixed destinations so dup2 ordering cannot
        // overwrite either source descriptor. No ambient executable path enters
        // the credential-dropped child.
        fn duplicate(file: &impl AsRawFd) -> File {
            let raw = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
            assert!(raw >= 64, "owned descriptor duplication");
            unsafe { File::from_raw_fd(raw) }
        }
        let executable = duplicate(&executable);
        let socket = duplicate(&child_stream);
        let executable_fd = executable.as_raw_fd();
        let socket_fd = socket.as_raw_fd();
        let mut command = std::process::Command::new("/proc/self/fd/41");
        command
            .args(["--exact", SELECTOR, "--nocapture"])
            .env(CHILD, "1");
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(executable_fd, 41) < 0
                    || libc::dup2(socket_fd, 42) < 0
                    || libc::setgroups(0, std::ptr::null()) < 0
                    || libc::setresgid(1005, 1005, 1005) < 0
                    || libc::setresuid(1004, 1004, 1004) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = OwnedChild(command.spawn().expect("actual different-UID child"));
        drop((executable, socket, child_stream));
        root_stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        root_stream
            .set_write_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        process_binding::arm(&root_stream).unwrap();
        let nonce = challenge();
        root_stream.write_all(&process_binding::MAGIC).unwrap();
        root_stream.write_all(&nonce).unwrap();
        let peer = match process_binding::receive_nonroot_for_authentication(&root_stream, &nonce) {
            Ok(peer) => peer,
            Err(error) => {
                drop(root_stream);
                panic!("actual QPE1 task authentication failed: {error}");
            }
        };
        assert_eq!(peer.pid(), child.0.id());
        assert_eq!((peer.uid(), peer.gid()), (1004, 1005));
        assert_ne!(peer.uid(), 1000, "original Home UID is not target identity");
        assert!(peer.current());
        root_stream.write_all(&[1]).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("observe owned child") {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned child exit deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(status.success());
        assert!(!peer.current(), "kernel task lifetime cannot be replayed");
        // This is kernel observation only. No InstalledRetainedLaunchInput,
        // person proof, file grant, projection or successful edit is minted.
    }
}

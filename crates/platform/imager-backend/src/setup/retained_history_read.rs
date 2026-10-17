//! Independent current Files Read enrollment. Neither editor proof custody nor
//! historical record labels can construct this source/daemon owner.
use super::{
    installed_retained_actor::InstalledRetainedLaunchInput,
    retained_selected_edit::CertifiedNativeHistoryDaemon,
};
use runtime_fs::external_history::daemon_control::EnrollmentRequest;
use std::os::fd::AsFd;
use std::{io, sync::Arc};
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
/// Root-local Read enrollment from an independently certified ordinary Files
/// launch. A Read source must still open an actual readable native subject;
/// this object does not convert metadata, names or record IDs into file rights.
pub struct RootFilesHistoryReadEnrollment {
    input: Arc<InstalledRetainedLaunchInput>,
    daemon: Arc<CertifiedNativeHistoryDaemon>,
    read_request_id: String,
}
impl RootFilesHistoryReadEnrollment {
    pub(in crate::setup) fn admit(
        input: Arc<InstalledRetainedLaunchInput>,
        daemon: Arc<CertifiedNativeHistoryDaemon>,
        request: &EnrollmentRequest,
    ) -> io::Result<Arc<Self>> {
        daemon.check()?;
        if !daemon.is_home_history_read()
            || !daemon.uses_input(&input)
            || !request.valid()
            || request.person_binding
                != serde_json::to_vec(input.binding()).map_err(io::Error::other)?
        {
            return Err(refused());
        }
        let result = Arc::new(Self {
            input,
            daemon,
            read_request_id: request.request_id.clone(),
        });
        result.check()?;
        Ok(result)
    }
    /// A fresh routing ID on the same independently certified Reader lane.
    /// It neither selects a record nor admits the native subject.
    pub(in crate::setup) fn for_subject(self: &Arc<Self>) -> io::Result<Arc<Self>> {
        self.check()?;
        let result = Arc::new(Self {
            input: self.input.clone(),
            daemon: self.daemon.clone(),
            read_request_id: uuid::Uuid::new_v4().to_string(),
        });
        result.check()?;
        Ok(result)
    }
    pub(in crate::setup) fn check(&self) -> io::Result<()> {
        self.daemon.check()?;
        if !self.daemon.is_home_history_read()
            || !self.daemon.uses_input(&self.input)
            || self.input.binding().app_id != "so.tilo.file-explorer"
            || self.input.binding().resource_record != "retained-home"
        {
            return Err(refused());
        }
        self.daemon.check()
    }
    pub(in crate::setup) fn input(&self) -> &Arc<InstalledRetainedLaunchInput> {
        &self.input
    }
    pub(in crate::setup) fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        Arc::ptr_eq(&self.input, input)
    }
    pub(in crate::setup) fn read_request_id(&self) -> &str {
        &self.read_request_id
    }
    // Discovery is deliberately not an authority constructor. The actual
    // daemon privately matches a unique still-held acknowledged pin instead.
    pub(in crate::setup) fn completed_request_id(&self) -> Option<&str> {
        None
    }
    pub(in crate::setup) fn enrollment_context_digest(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        let digest = self.daemon.context_digest()?;
        self.check()?;
        Ok(digest)
    }
    pub(in crate::setup) fn source_owner(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        let owner = self.daemon.source_owner()?;
        self.check()?;
        Ok(owner)
    }
    pub(in crate::setup) fn daemon(&self) -> &Arc<CertifiedNativeHistoryDaemon> {
        &self.daemon
    }
}

/// The existing bootstrap transaction owns this exchange, before the ordinary
/// consuming launcher is delegated. The new proof descriptor comes from Shell's
/// fresh Read responder and is never duplicated from the editor's issuer lane.
pub fn enroll_files_read(
    stream: &mut std::os::unix::net::UnixStream,
    input: Arc<InstalledRetainedLaunchInput>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    deadline: std::time::Instant,
) -> io::Result<Option<Arc<RootFilesHistoryReadEnrollment>>> {
    use super::retained_selected_edit::PendingDaemonEnrollment;
    use runtime_fs::external_history::daemon_control::{
        EnrollmentControl, EnrollmentReply, MAX_CONTROL_BYTES,
    };
    use runtime_portal::fd_transport;
    use std::{
        fs::File,
        io::{Read, Write},
    };
    input.check_home_history_read(&cancel)?;
    let held = stream.try_clone()?;
    super::installed_retained_actor::with_proof_deadline_at(&held, &cancel, deadline, || {
        let framing = wire_frame::Framing::little_endian(MAX_CONTROL_BYTES).reject_empty();
        let bytes = framing
            .read(stream)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        let EnrollmentControl::EnrollHistoryReadDaemon(request) =
            EnrollmentControl::decode(&bytes)?
        else {
            return Err(refused());
        };
        if request.person_binding
            != serde_json::to_vec(input.binding()).map_err(io::Error::other)?
        {
            return Err(refused());
        }
        let pending = match PendingDaemonEnrollment::begin_read(
            input.clone(),
            &request,
            cancel.clone(),
            deadline,
        ) {
            Ok(pending) => pending,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::Unsupported
                ) =>
            {
                input.check_home_history_read(&cancel)?;
                let reply = EnrollmentReply::Refused {
                    schema: 1,
                    request_id: request.request_id.clone(),
                };
                framing
                    .write(stream, &reply.encode(&request)?)
                    .map_err(io::Error::other)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let reply = EnrollmentReply::Delegated {
            delegation: Box::new(pending.delegation(&request)?),
        };
        framing
            .write(stream, &reply.encode(&request)?)
            .map_err(io::Error::other)?;
        fd_transport::send_with_fd(stream.as_fd(), &[61], pending.peer().pidfd())?;
        let mut ack = [0];
        stream.read_exact(&mut ack)?;
        if ack != [61] {
            return Err(refused());
        }
        let (tag, fd) = fd_transport::recv_with_fd(stream.as_fd())?;
        if tag != [62] {
            return Err(refused());
        }
        let proof = File::from(fd.ok_or_else(refused)?);
        stream.write_all(&[62])?;
        let daemon = pending.finish(proof)?;
        RootFilesHistoryReadEnrollment::admit(input.clone(), daemon, &request).map(Some)
    })
}

/// The projection owns this reader alongside the original source supervisor.
/// Each cached entry owns a genuine Read subject/handoff, never an inode grant.
pub struct RootHistoryReadSession {
    enrollment: Arc<super::retained_selected_editor_history_read::RootHistoryReadEnrollment>,
    publisher: Arc<super::retained_source_worker::HistoryReadPublisher>,
    handles: std::sync::Mutex<Vec<ReadHandle>>,
    selected: Option<
        std::sync::Weak<super::retained_source_relay::selected_parent::SourceSelectedParent>,
    >,
    retired: Arc<std::sync::atomic::AtomicBool>,
}
struct ReadHandle {
    enrollment: Arc<super::retained_selected_editor_history_read::RootHistoryReadEnrollment>,
    handoff: super::retained_source_worker::HistoryReadHandoff,
    observed: runtime_fs::directory_backend::HistoryReadObservation,
    adopted: bool,
    identity: Arc<std::sync::atomic::AtomicBool>,
    retirement: u8,
    completed_request_id: Option<String>,
}
impl ReadHandle {
    // Cleanup owns the exact registered realm; it neither queries nor restores Read.
    fn retire(&mut self) -> io::Result<()> {
        if self.retirement == 0 {
            self.enrollment
                .daemon()
                .retire_history_read(self.enrollment.read_request_id())?;
            self.retirement = 1;
        }
        if self.retirement == 1 {
            self.handoff.retire_rpc()?;
            self.retirement = 2;
        }
        if self.retirement == 2 {
            self.handoff
                .subject()
                .retire_controller_cache()
                .map_err(io::Error::other)?;
            self.retirement = 3;
            self.identity
                .store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }
}
/// A cleanup witness holds no Reader proof, input or publisher alive.
pub struct RootHistoryReadCleanup {
    session: std::sync::Weak<RootHistoryReadSession>,
    retired: Arc<std::sync::atomic::AtomicBool>,
}
impl RootHistoryReadCleanup {
    pub(in crate::setup) fn session(&self) -> Option<Arc<RootHistoryReadSession>> {
        self.session.upgrade()
    }
    pub(in crate::setup) fn retire_preceding(
        &self,
        proof: &super::retained_selected_edit::AcknowledgedSelectedReplacement,
        parents: &[runtime_fs::EntryName],
        leaf: &runtime_fs::EntryName,
        parent: [u64; 2],
    ) -> io::Result<()> {
        if let Some(session) = self.session.upgrade() {
            return session.retire_preceding(proof, parents, leaf, parent);
        }
        if self.retired.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err(refused())
        }
    }
}
impl RootHistoryReadSession {
    pub(in crate::setup) fn admit(
        enrollment: Arc<super::retained_selected_editor_history_read::RootHistoryReadEnrollment>,
        publisher: Arc<super::retained_source_worker::HistoryReadPublisher>,
    ) -> io::Result<Arc<Self>> {
        enrollment.check()?;
        if enrollment.selected_editor().is_some() {
            return Err(refused());
        }
        Ok(Arc::new(Self {
            enrollment,
            publisher,
            handles: std::sync::Mutex::new(Vec::new()),
            selected: None,
            retired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }))
    }
    pub(in crate::setup) fn admit_selected(
        enrollment: Arc<super::retained_selected_editor_history_read::RootHistoryReadEnrollment>,
        publisher: Arc<super::retained_source_worker::HistoryReadPublisher>,
        selected: &Arc<super::retained_source_relay::selected_parent::SourceSelectedParent>,
    ) -> io::Result<Arc<Self>> {
        enrollment.check()?;
        if !enrollment.uses_input(selected.input())
            || !enrollment
                .selected_editor()
                .is_some_and(|editor| selected.uses_history_editor(editor))
        {
            return Err(refused());
        }
        selected.check().map_err(io::Error::other)?;
        let result = Arc::new(Self {
            enrollment,
            publisher,
            handles: std::sync::Mutex::new(Vec::new()),
            selected: Some(Arc::downgrade(selected)),
            retired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        result.enrollment.check()?;
        Ok(result)
    }
    fn check_subject(
        &self,
        observed: &runtime_fs::directory_backend::HistoryReadObservation,
    ) -> io::Result<()> {
        if self.retired.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused());
        }
        if let Some(selected) = &self.selected {
            selected
                .upgrade()
                .ok_or_else(refused)?
                .check_history_subject(observed)?;
        }
        self.enrollment.check()
    }
    pub(in crate::setup) fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        self.enrollment.uses_input(input)
    }
    pub(in crate::setup) fn query(
        &self,
        subject: Arc<dyn runtime_fs::directory_backend::WorkspaceHistoryReadSubjectBackend>,
        expected: [u64; 2],
        query: &[u8],
    ) -> io::Result<Vec<u8>> {
        use runtime_fs::external_history::read_control::{HistoryReadReply, valid_query};
        self.enrollment.check()?;
        let query = std::str::from_utf8(query).map_err(io::Error::other)?;
        if !valid_query(query) {
            return Err(refused());
        }
        subject.check().map_err(io::Error::other)?;
        let observed = subject.observe().map_err(io::Error::other)?;
        observed.validate().map_err(io::Error::other)?;
        self.check_subject(&observed)?;
        if observed.identity != expected
            || observed.source_owner != self.enrollment.source_owner()?
        {
            return Err(refused());
        }
        // Serializes adoption with queries and retirement. No source-call lock
        // is held here: each subject callback rechecks its own real custody.
        let mut handles = self.handles.lock().map_err(|_| refused())?;
        self.check_subject(&observed)?;
        // A new Save replaces the named inode. Retire only the old admitted
        // subject for this exact locus, with a real daemon ACK, before adoption.
        let mut old = 0;
        while old < handles.len() {
            let entry = &handles[old];
            if entry.observed != observed
                && entry.observed.source_owner == observed.source_owner
                && entry.observed.root == observed.root
                && entry.observed.parent == observed.parent
                && entry.observed.parents == observed.parents
                && entry.observed.name == observed.name
            {
                handles[old].retire()?;
                handles.remove(old);
            } else {
                old += 1;
            }
        }
        let index = if let Some(index) = handles.iter().position(|entry| entry.observed == observed)
        {
            if !handles[index].adopted || handles[index].retirement != 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            index
        } else {
            if handles.len() >= 64 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let enrollment = self.enrollment.for_subject()?;
            let handoff = self
                .publisher
                .connect_history_read_handoff(enrollment.clone(), subject)?;
            // Ownership precedes the first possible daemon admission. Unknown
            // delivery must retain this same unread-channel/subject custody.
            handles.push(ReadHandle {
                enrollment,
                handoff,
                observed,
                adopted: false,
                identity: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                retirement: 0,
                completed_request_id: None,
            });
            let index = handles.len() - 1;
            let entry = &handles[index];
            let reply = entry.enrollment.daemon().history_read_command(
                entry.enrollment.read_request_id(),
                None,
                Some(&entry.handoff),
            )?;
            match reply {
                HistoryReadReply::Adopted {
                    completed_request_id,
                    ..
                } => {
                    handles[index].completed_request_id = Some(completed_request_id);
                    handles[index].adopted = true;
                }
                HistoryReadReply::Refused { .. } => {
                    // Refusal is not source-driver retirement. Release this
                    // actual pre-registered subject only after its own cleanup
                    // command receives observed retirement of that request.
                    handles[index].retire()?;
                    handles.pop();
                    return Err(refused());
                }
                _ => return Err(refused()),
            }
            handles[index].handoff.check()?;
            index
        };
        let entry = &handles[index];
        entry.handoff.check()?;
        let reply = entry.enrollment.daemon().history_read_command(
            entry.enrollment.read_request_id(),
            Some(query),
            None,
        )?;
        entry.handoff.check()?;
        self.check_subject(&entry.observed)?;
        self.enrollment.check()?;
        match reply {
            HistoryReadReply::Read { bytes, .. } => Ok(bytes),
            _ => Err(refused()),
        }
    }
    /// Mint only from the actual same-pin acknowledged Save and an independently
    /// opened current Read subject. The daemon must have matched that exact ACK.
    pub(in crate::setup) fn adopt_acknowledged(
        self: &Arc<Self>,
        subject: Arc<dyn runtime_fs::directory_backend::WorkspaceHistoryReadSubjectBackend>,
        proof: &super::retained_selected_edit::AcknowledgedSelectedReplacement,
        edited: &super::retained_source_relay::selected_parent::SourceSelectedParent,
    ) -> io::Result<Arc<RootHistoryReadContinuity>> {
        proof.check_custody(edited).map_err(io::Error::other)?;
        subject.check().map_err(io::Error::other)?;
        let observed = subject.observe().map_err(io::Error::other)?;
        if observed.identity != proof.observation().destination
            || observed.source_owner != proof.observation().source_owner
        {
            return Err(refused());
        }
        let bytes = self.query(subject, observed.identity, b"user.tilo.record-id")?;
        let record: [u8; 16] = bytes.try_into().map_err(|_| refused())?;
        let completed = uuid::Uuid::from_bytes(proof.observation().operation).to_string();
        let token = {
            let handles = self.handles.lock().map_err(|_| refused())?;
            let entry = handles
                .iter()
                .find(|entry| entry.observed == observed)
                .ok_or_else(refused)?;
            if !entry.adopted || entry.completed_request_id.as_deref() != Some(completed.as_str()) {
                return Err(refused());
            }
            entry.handoff.check()?;
            Arc::new(RootHistoryReadContinuity {
                session: Arc::downgrade(self),
                identity: entry.identity.clone(),
                record,
            })
        };
        proof.check_custody(edited).map_err(io::Error::other)?;
        token.check()?;
        Ok(token)
    }

    pub(in crate::setup) fn cleanup(self: &Arc<Self>) -> RootHistoryReadCleanup {
        RootHistoryReadCleanup {
            session: Arc::downgrade(self),
            retired: self.retired.clone(),
        }
    }
    fn retire_identity(&self, identity: &Arc<std::sync::atomic::AtomicBool>) -> io::Result<()> {
        let mut handles = self.handles.lock().map_err(|_| refused())?;
        if let Some(index) = handles
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.identity, identity))
        {
            handles[index].retire()?;
            handles.remove(index);
            return Ok(());
        }
        if identity.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err(refused())
        }
    }
    fn retire_preceding(
        &self,
        proof: &super::retained_selected_edit::AcknowledgedSelectedReplacement,
        parents: &[runtime_fs::EntryName],
        leaf: &runtime_fs::EntryName,
        parent: [u64; 2],
    ) -> io::Result<()> {
        let observation = proof.observation();
        let completed = uuid::Uuid::from_bytes(observation.operation).to_string();
        let mut handles = self.handles.lock().map_err(|_| refused())?;
        let mut index = 0;
        while index < handles.len() {
            let entry = &handles[index];
            // Include post-registration failures which never yielded a token.
            if entry.observed.source_owner == observation.source_owner
                && entry.observed.identity == observation.destination
                && entry.observed.parent == parent
                && entry.observed.name == leaf.as_os_str().as_encoded_bytes()
                && entry.observed.parents.iter().map(Vec::as_slice).eq(parents
                    .iter()
                    .map(|name| name.as_os_str().as_encoded_bytes()))
            {
                if entry
                    .completed_request_id
                    .as_deref()
                    .is_some_and(|id| id != completed)
                {
                    return Err(refused());
                }
                handles[index].retire()?;
                handles.remove(index);
            } else {
                index += 1;
            }
        }
        Ok(())
    }
    pub(in crate::setup) fn retire(&self) -> io::Result<()> {
        let mut handles = self.handles.lock().map_err(|_| refused())?;
        while let Some(entry) = handles.last_mut() {
            entry.retire()?;
            handles.pop();
        }
        self.retired
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// Constructor-private continuity of an actual adopted independently current
/// Read owner. An observed identity or successful Save cannot construct it.
/// Removal/replacement of its exact registered handle invalidates this token;
/// holding it cannot revive the Files proof or renew the original source lease.
pub struct RootHistoryReadContinuity {
    session: std::sync::Weak<RootHistoryReadSession>,
    identity: Arc<std::sync::atomic::AtomicBool>,
    record: [u8; 16],
}
impl RootHistoryReadContinuity {
    pub(in crate::setup) fn retire(&self) -> io::Result<()> {
        if let Some(session) = self.session.upgrade() {
            return session.retire_identity(&self.identity);
        }
        if self.identity.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err(refused())
        }
    }
    pub(in crate::setup) fn check_acknowledged(
        &self,
        proof: &super::retained_selected_edit::AcknowledgedSelectedReplacement,
        selected: &super::retained_source_relay::selected_parent::SourceSelectedParent,
    ) -> io::Result<()> {
        proof.check_custody(selected).map_err(io::Error::other)?;
        self.check()?;
        {
            let session = self.session.upgrade().ok_or_else(refused)?;
            let handles = session.handles.lock().map_err(|_| refused())?;
            let entry = handles
                .iter()
                .find(|entry| Arc::ptr_eq(&entry.identity, &self.identity))
                .ok_or_else(refused)?;
            let op = uuid::Uuid::from_bytes(proof.observation().operation).to_string();
            if entry.completed_request_id.as_deref() != Some(op.as_str())
                || entry.observed.source_owner != proof.observation().source_owner
                || entry.observed.identity != proof.observation().destination
            {
                return Err(refused());
            }
            entry.handoff.check()?;
        }
        proof.check_custody(selected).map_err(io::Error::other)?;
        self.check()
    }
    pub(in crate::setup) fn check(&self) -> io::Result<()> {
        use runtime_fs::external_history::read_control::HistoryReadReply;
        let session = self.session.upgrade().ok_or_else(refused)?;
        session.enrollment.check()?;
        let handles = session.handles.lock().map_err(|_| refused())?;
        let entry = handles
            .iter()
            .find(|entry| Arc::ptr_eq(&entry.identity, &self.identity))
            .ok_or_else(refused)?;
        if !entry.adopted || entry.completed_request_id.is_none() || entry.retirement != 0 {
            return Err(refused());
        }
        entry.handoff.check()?;
        session.check_subject(&entry.observed)?;
        let reply = entry.enrollment.daemon().history_read_command(
            entry.enrollment.read_request_id(),
            Some("user.tilo.record-id"),
            None,
        )?;
        match reply {
            HistoryReadReply::Read { bytes, .. } if bytes.as_slice() == self.record => {}
            _ => return Err(refused()),
        }
        entry.handoff.check()?;
        session.check_subject(&entry.observed)?;
        session.enrollment.check()
    }
}

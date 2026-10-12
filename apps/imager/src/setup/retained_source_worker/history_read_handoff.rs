//! Fresh unread source Read stream, pinned to an actual admitted Reader.
use super::super::retained_selected_editor_history_read::RootHistoryReadEnrollment;
use super::{RetainedSourceWorker, refused};
use launcher::owned_service::{
    RetainedIoAuthority, RetainedWorkerConnector, RetainedWorkerWitness,
};
use runtime_fs::directory_backend::{HistoryReadObservation, WorkspaceHistoryReadSubjectBackend};
use runtime_portal::retained_role::NativeHistoryReadHandoff;
use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    net::Shutdown,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    sync::Arc,
    time::Duration,
};
/// Descriptor custody is inseparable from current original Read, actual source
/// program/peer and fresh admitted Reader proof. None of these labels is a grant.
pub(in crate::setup) struct HistoryReadHandoff {
    publisher: Arc<HistoryReadPublisher>,
    stream: UnixStream,
    sealed: File,
    program: File,
    witness: RetainedWorkerWitness,
    enrollment: Arc<RootHistoryReadEnrollment>,
    subject: Arc<dyn WorkspaceHistoryReadSubjectBackend>,
    observation: HistoryReadObservation,
    record: NativeHistoryReadHandoff,
}
impl HistoryReadHandoff {
    pub(in crate::setup) fn check(&self) -> io::Result<()> {
        self.publisher.check()?;
        self.enrollment.check()?;
        self.witness.check()?;
        self.subject.check().map_err(io::Error::other)?;
        let observed = self.subject.observe().map_err(io::Error::other)?;
        observed.validate().map_err(io::Error::other)?;
        if observed != self.observation
            || self.record.source_owner != self.enrollment.source_owner()?
            || self.record.source_owner != observed.source_owner
            || self.record.enrollment_context_digest
                != self.enrollment.enrollment_context_digest()?
            || self.record.read_request_id != self.enrollment.read_request_id()
            || self.record.completed_request_id.as_deref() != self.enrollment.completed_request_id()
        {
            return Err(refused());
        }
        self.witness.check()?;
        self.publisher.check()?;
        self.enrollment.check()
    }
    /// Borrow ONLY for enrolled daemon roles68/69/70. Fresh context/proof61/62
    /// have already established the independently admitted Read enrollment. QPE remains unread here.
    pub(in crate::setup) fn descriptors(&self) -> io::Result<(&File, &UnixStream, &File)> {
        self.check()?;
        let metadata = self.program.metadata()?;
        if [metadata.dev(), metadata.ino()] != self.record.worker_program {
            return Err(refused());
        }
        Ok((&self.sealed, &self.stream, &self.program))
    }
    /// Retire only this actual source Read RPC realm after real daemon Retired.
    /// Named leaf may have advanced; held scope and source witness remain live.
    pub(in crate::setup) fn retire_rpc(&self) -> io::Result<()> {
        self.publisher.check_retirement_custody()?;
        self.witness.check_retirement_custody()?;
        self.subject
            .retire_daemon_handoff(self.record.ticket)
            .map_err(io::Error::other)?;
        self.witness.check_retirement_custody()?;
        self.publisher.check_retirement_custody()
    }
    pub(in crate::setup) fn subject(&self) -> &Arc<dyn WorkspaceHistoryReadSubjectBackend> {
        &self.subject
    }
}
impl Drop for HistoryReadHandoff {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
fn seal(bytes: &[u8]) -> io::Result<File> {
    if unsafe { libc::getuid() } != 0
        || unsafe { libc::geteuid() } != 0
        || bytes.is_empty()
        || bytes.len() > 4096
    {
        return Err(refused());
    }
    let descriptor = unsafe {
        libc::memfd_create(
            c"tilo-native-read-handoff".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    file.write_all(bytes)?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 0
        || metadata.mode() & 0o7777 != 0o400
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } != seals
    {
        return Err(refused());
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

/// Privately minted from the exact owned source worker before it moves into
/// projection custody. Kernel endpoint, actual supervisor lifetime and current
/// installed source authority are inseparable; no public label constructor.
pub(in crate::setup) struct HistoryReadPublisher {
    connector: RetainedWorkerConnector,
    input: Arc<super::super::installed_retained_actor::InstalledRetainedLaunchInput>,
    authority: Arc<dyn RetainedIoAuthority>,
    live: Arc<std::sync::atomic::AtomicBool>,
}
impl HistoryReadPublisher {
    fn check_retirement_custody(&self) -> io::Result<()> {
        // Exact worker witness is checked by the held handoff before/after RPC.
        // Authority remains mandatory in check(), descriptors() and every arm.
        if unsafe { libc::getuid() } != 0
            || unsafe { libc::geteuid() } != 0
            || !self.live.load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(refused());
        }
        Ok(())
    }
    fn check(&self) -> io::Result<launcher::owned_service::ServiceCustody> {
        if !self.live.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused());
        }
        self.authority.check()?;
        let actual = self.connector.custody()?;
        self.authority.check()?;
        if !self.live.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused());
        }
        Ok(actual)
    }
}
impl RetainedSourceWorker {
    /// Mint before moving the actual supervisor into its projection owner. The
    /// owner must still be retained until real source stop; this derivative is
    /// not an alternative cleanup owner and cannot renew native session life.
    pub(in crate::setup) fn history_read_publisher(&self) -> io::Result<Arc<HistoryReadPublisher>> {
        let before = self.check_current_custody()?;
        let result = Arc::new(HistoryReadPublisher {
            connector: self
                .endpoint
                .as_ref()
                .ok_or_else(refused)?
                .retain_worker_connector()?,
            input: self.read_input.as_ref().ok_or_else(refused)?.clone(),
            authority: self.authority.as_ref().ok_or_else(refused)?.clone(),
            live: self.source_live.clone(),
        });
        if result.check()? != before || self.check_current_custody()? != before {
            return Err(refused());
        }
        Ok(result)
    }
    pub(in crate::setup) fn connect_history_read_handoff(
        &self,
        enrollment: Arc<RootHistoryReadEnrollment>,
        subject: Arc<dyn WorkspaceHistoryReadSubjectBackend>,
    ) -> io::Result<HistoryReadHandoff> {
        let before = self.check_current_custody()?;
        let result = self
            .history_read_publisher()?
            .connect_history_read_handoff(enrollment, subject)?;
        if self.check_current_custody()? != before {
            return Err(refused());
        }
        Ok(result)
    }
}
impl HistoryReadPublisher {
    /// Create only from current typed Reader enrollment and a genuine
    /// source-held Read subject. Never borrow editor replacement authority.
    pub(in crate::setup) fn connect_history_read_handoff(
        self: &Arc<Self>,
        enrollment: Arc<RootHistoryReadEnrollment>,
        subject: Arc<dyn WorkspaceHistoryReadSubjectBackend>,
    ) -> io::Result<HistoryReadHandoff> {
        let before = self.check()?;
        let authority = &self.authority;
        if !enrollment.uses_input(&self.input) {
            return Err(refused());
        }
        enrollment.check()?;
        subject.check().map_err(io::Error::other)?;
        let observed = subject.observe().map_err(io::Error::other)?;
        observed.validate().map_err(io::Error::other)?;
        if observed.source_owner != authority.native_owner_sha256()?
            || observed.source_owner != enrollment.source_owner()?
        {
            return Err(refused());
        }
        let mut challenge = [0; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let ticket = subject
            .arm_daemon_handoff(challenge)
            .map_err(io::Error::other)?;
        let (mut stream, witness) = self.connector.connect_unread()?;
        let program = witness.held_program()?;
        let record = NativeHistoryReadHandoff {
            schema: 1,
            read_request_id: enrollment.read_request_id().to_owned(),
            completed_request_id: enrollment.completed_request_id().map(str::to_owned),
            enrollment_context_digest: enrollment.enrollment_context_digest()?,
            source_owner: observed.source_owner,
            worker_pid: witness.pid(),
            worker_start: witness.start_time(),
            worker_uid: witness.uid(),
            worker_gid: witness.gid(),
            worker_program: witness.program_identity(),
            ticket,
            challenge,
        };
        let sealed = seal(&record.encode()?)?;
        if self.check()? != before {
            return Err(refused());
        }
        witness.check()?;
        enrollment.check()?;
        if subject.observe().map_err(io::Error::other)? != observed {
            return Err(refused());
        }
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut prelude = [0; 52];
        prelude[..4].copy_from_slice(b"RHR1");
        prelude[4..20].copy_from_slice(&ticket);
        prelude[20..].copy_from_slice(&challenge);
        if stream.write(&prelude)? != prelude.len() {
            return Err(io::Error::other("partial Read prelude; no replay"));
        }
        stream.set_write_timeout(None)?;
        let handoff = HistoryReadHandoff {
            publisher: self.clone(),
            stream,
            sealed,
            program,
            witness,
            enrollment,
            subject,
            observation: observed,
            record,
        };
        handoff.check()?;
        if self.check()? != before {
            return Err(refused());
        }
        Ok(handoff)
    }
}

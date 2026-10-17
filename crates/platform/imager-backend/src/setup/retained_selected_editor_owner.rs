//! One typed editor owner on the original authenticated root bootstrap.
//! Staging is custody only; namespace readiness requires the canonical issuer
//! and its retained kernel witness. No public callback can mint either state.
use super::{
    installed_retained_actor::{LaunchRequest, ReviewedInstalledRetainedLaunch},
    retained_open_dispatch::SelectedOriginCustody,
    retained_selected_edit::{RootFilesEditOrigin, RootSelectedEditAdmission, RootSelectedEditor},
    retained_selected_edit_session::RetainedSelectedEditSession,
};
use launcher::{
    ordinary_child::ManagedConsumingBootstrap,
    owned_service::ServiceCancellation,
    retained_view::{ConsumerChallenge, ConsumerCommand, RootConsumerChannel},
};
use runtime_collab::open_selected::{TargetNamespaceReceipt, v2};
use sha2::{Digest, Sha256};
use std::os::fd::AsFd;
use std::{
    fs::File,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const DOMAIN: &[u8] = b"tilo-ordinary-retained-home-context-v1\0";
const PREPARE_BUDGET: Duration = Duration::from_secs(10);
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
#[derive(Clone)]
pub(super) enum EditorStatus {
    Pending,
    NamespaceReady(TargetNamespaceReceipt),
    Refused,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StageStatus {
    Preparing,
    Prepared,
    Refused,
}
struct State {
    stage: StageStatus,
    status: EditorStatus,
    admission: Option<Arc<RootSelectedEditAdmission>>,
    prepared: Option<PreparedConsumer>,
    daemon: Option<Arc<super::retained_selected_edit::CertifiedNativeHistoryDaemon>>,
    reader:
        Option<Arc<super::retained_selected_editor_history_read::PendingSelectedEditorHistoryRead>>,
}
#[derive(Clone)]
struct PreparedConsumer {
    input: Arc<super::installed_retained_actor::InstalledRetainedLaunchInput>,
    actor: Arc<ReviewedInstalledRetainedLaunch>,
    managed: Arc<Mutex<ManagedConsumingBootstrap>>,
    challenge: ConsumerChallenge,
    shell: Arc<runtime_portal::process_binding::ExecutingPeer>,
}
pub(super) struct SelectedEditorOwner {
    request: v2::RootRequest,
    state: Arc<Mutex<State>>,
    cancel: Arc<AtomicBool>,
    cancellation: ServiceCancellation,
    parent_cancel: Arc<AtomicBool>,
    proof: UnixStream,
    endpoint: UnixStream,
    release: Option<mpsc::Sender<()>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl SelectedEditorOwner {
    /// Construct and register before waiting for preparation. Fresh proof
    /// refusal never drops a worker which may already hold native custody.
    pub(super) fn stage(
        custody: SelectedOriginCustody,
        request: v2::RootRequest,
        launch: v2::LaunchSelected,
        context: Vec<u8>,
        proof: UnixStream,
        endpoint: UnixStream,
        parent_cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let declaration = review_declaration(&request, &launch, &context)?;
        custody.check(&parent_cancel)?;
        if custody.components != request.components
            || custody.input.binding().operation_id != launch.binding.operation_id
        {
            return Err(refused());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancellation = ServiceCancellation::default();
        let state = Arc::new(Mutex::new(State {
            stage: StageStatus::Preparing,
            status: EditorStatus::Pending,
            admission: None,
            prepared: None,
            daemon: None,
            reader: None,
        }));
        let stopped = Arc::new(AtomicBool::new(false));
        let proof_observation = proof.try_clone()?;
        let endpoint_observation = endpoint.try_clone()?;
        let (release, released) = mpsc::channel();
        let work = Worker {
            request: request.clone(),
            launch,
            context,
            declaration,
            custody,
            proof,
            endpoint,
            parent_cancel: Arc::clone(&parent_cancel),
            cancel: Arc::clone(&cancel),
            cancellation: cancellation.clone(),
            state: Arc::clone(&state),
            stopped: Arc::clone(&stopped),
            released,
        };
        let worker = std::thread::Builder::new()
            .name("retained-selected-editor-owner".into())
            .spawn(move || work.run())?;
        Ok(Self {
            request,
            state,
            cancel,
            cancellation,
            parent_cancel,
            proof: proof_observation,
            endpoint: endpoint_observation,
            release: Some(release),
            stopped,
            worker: Some(worker),
        })
    }
    pub(super) fn request(&self) -> &v2::RootRequest {
        &self.request
    }
    pub(super) fn stage_status(&self) -> StageStatus {
        self.state
            .lock()
            .map_or(StageStatus::Refused, |state| state.stage)
    }
    /// The dispatcher has already written Staged on the existing bootstrap.
    /// Delegate only this retained child and the exact freshly certified command.
    pub(super) fn delegate_shell(
        &mut self,
        stream: &mut UnixStream,
        deadline: Instant,
    ) -> io::Result<launcher::ordinary_child::DelegationOutcome> {
        let prepared = (|| {
            if self.cancel.load(Ordering::Acquire) || self.parent_cancel.load(Ordering::Acquire) {
                return Err(refused());
            }
            let state = self.state.lock().map_err(|_| refused())?;
            if state.stage != StageStatus::Prepared {
                return Err(refused());
            }
            let prepared = state.prepared.as_ref().ok_or_else(refused)?.clone();
            drop(state);
            prepared.actor.revalidate(&self.cancel)?;
            if !prepared.shell.current() {
                return Err(refused());
            }
            Ok::<_, io::Error>(prepared)
        })();
        let prepared = if let Ok(prepared) = prepared {
            prepared
        } else {
            launcher::ordinary_child::refuse_consuming_bootstrap(stream, deadline)?;
            return Ok(launcher::ordinary_child::DelegationOutcome::Refused);
        };
        let mut managed = if let Ok(managed) = prepared.managed.lock() {
            managed
        } else {
            launcher::ordinary_child::refuse_consuming_bootstrap(stream, deadline)?;
            return Ok(launcher::ordinary_child::DelegationOutcome::Refused);
        };
        managed.delegate_shell_command(
            stream,
            &prepared.challenge.command,
            &prepared.shell,
            deadline,
        )
    }
    /// Enroll the fixed certified daemon before releasing this exact editor.
    /// The separately delegated proof belongs to the target lifetime, not Files.
    pub(super) fn enroll_history(
        &mut self,
        stream: &mut UnixStream,
        deadline: Instant,
    ) -> io::Result<bool> {
        use super::retained_selected_edit::PendingDaemonEnrollment;
        use runtime_fs::external_history::daemon_control::{
            EnrollmentControl, EnrollmentReply, MAX_CONTROL_BYTES,
        };
        use runtime_portal::fd_transport;
        let prepared = {
            let state = self.state.lock().map_err(|_| refused())?;
            if state.stage != StageStatus::Prepared || state.daemon.is_some() {
                return Err(refused());
            }
            state.prepared.as_ref().ok_or_else(refused)?.clone()
        };
        let held = stream.try_clone()?;
        super::installed_retained_actor::with_proof_deadline_at(
            &held,
            &self.cancel,
            deadline,
            || {
                prepared.actor.revalidate(&self.cancel)?;
                if self.parent_cancel.load(Ordering::Acquire) || !prepared.shell.current() {
                    return Err(refused());
                }
                let framing = wire_frame::Framing::little_endian(MAX_CONTROL_BYTES).reject_empty();
                let bytes = framing
                    .read(stream)
                    .map_err(io::Error::other)?
                    .ok_or_else(refused)?;
                let EnrollmentControl::EnrollHistoryDaemon(request) =
                    EnrollmentControl::decode(&bytes)?
                else {
                    return Err(refused());
                };
                if request.request_id != self.request.request_id {
                    return Err(refused());
                }
                let pending = match PendingDaemonEnrollment::begin(
                    prepared.input.clone(),
                    &request,
                    self.cancel.clone(),
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
                        prepared.actor.revalidate(&self.cancel)?;
                        if self.parent_cancel.load(Ordering::Acquire) || Instant::now() >= deadline
                        {
                            return Err(refused());
                        }
                        let reply = EnrollmentReply::Refused {
                            schema: 1,
                            request_id: request.request_id.clone(),
                        };
                        framing
                            .write(stream, &reply.encode(&request)?)
                            .map_err(io::Error::other)?;
                        return Ok(false);
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
                prepared.actor.revalidate(&self.cancel)?;
                let mut state = self.state.lock().map_err(|_| refused())?;
                if state.stage != StageStatus::Prepared
                    || state.daemon.is_some()
                    || self.cancel.load(Ordering::Acquire)
                    || self.parent_cancel.load(Ordering::Acquire)
                {
                    return Err(refused());
                }
                state.daemon = Some(daemon);
                Ok(true)
            },
        )
    }
    /// Separate fresh Reader custody on the same exclusive Stage transaction.
    pub(super) fn enroll_history_read(
        &mut self,
        stream: &mut UnixStream,
        deadline: Instant,
    ) -> io::Result<bool> {
        let prepared = {
            let state = self.state.lock().map_err(|_| refused())?;
            if state.stage != StageStatus::Prepared
                || state.daemon.is_none()
                || state.reader.is_some()
            {
                return Err(refused());
            }
            state.prepared.as_ref().ok_or_else(refused)?.clone()
        };
        prepared.actor.revalidate(&self.cancel)?;
        if self.parent_cancel.load(Ordering::Acquire) || !prepared.shell.current() {
            return Err(refused());
        }
        let Some(reader) =
            super::retained_selected_editor_history_read::enroll_selected_editor_read(
                stream,
                prepared.input.clone(),
                self.cancel.clone(),
                deadline,
            )?
        else {
            return Ok(false);
        };
        prepared.actor.revalidate(&self.cancel)?;
        let mut state = self.state.lock().map_err(|_| refused())?;
        if state.stage != StageStatus::Prepared
            || state.reader.is_some()
            || self.cancel.load(Ordering::Acquire)
            || self.parent_cancel.load(Ordering::Acquire)
        {
            return Err(refused());
        }
        state.reader = Some(reader);
        Ok(true)
    }
    pub(super) fn release_after_staged(&mut self) -> io::Result<()> {
        if self.stage_status() != StageStatus::Prepared
            || self.parent_cancel.load(Ordering::Acquire)
        {
            return Err(refused());
        }
        self.release
            .take()
            .ok_or_else(refused)?
            .send(())
            .map_err(|_| refused())
    }
    /// Use the actual retained proof socket with the mux's whole-request fence
    /// before calling poll: fresh certificate checks cannot renew its budget.
    pub(super) fn proof_transport(&self) -> &UnixStream {
        &self.proof
    }
    pub(super) fn poll(&self) -> io::Result<EditorStatus> {
        if self.cancel.load(Ordering::Acquire) || self.parent_cancel.load(Ordering::Acquire) {
            return Ok(EditorStatus::Refused);
        }
        let (status, admission) = {
            let state = self.state.lock().map_err(|_| refused())?;
            (state.status.clone(), state.admission.clone())
        };
        if let EditorStatus::NamespaceReady(_) = &status {
            let admission = admission.ok_or_else(refused)?;
            if !admission.consumer_admitted()?
                || self.observed_stopped()
                || self.cancel.load(Ordering::Acquire)
            {
                self.cancel();
                return Ok(EditorStatus::Refused);
            }
        }
        Ok(status)
    }
    /// Only the adopted kernel namespace qualifies independence from Files.
    /// A staged or ready-looking transport status is insufficient.
    pub(super) fn consumer_admitted(&self) -> io::Result<bool> {
        let admission = self.state.lock().map_err(|_| refused())?.admission.clone();
        match admission {
            Some(admission) => admission.consumer_admitted(),
            None => Ok(false),
        }
    }
    /// This is minted only after session retirement positively observed the
    /// actual consumer/target/relay/source stop. A thread exit is insufficient.
    pub(super) fn observed_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub(super) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        self.cancellation.cancel();
        let _ = self.proof.shutdown(std::net::Shutdown::Both);
        let _ = self.endpoint.shutdown(std::net::Shutdown::Both);
    }
    pub(super) fn retire_before_exit(&mut self) {
        self.cancel();
        self.release.take();
        while !self.observed_stopped() {
            std::thread::park_timeout(Duration::from_millis(50));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for SelectedEditorOwner {
    fn drop(&mut self) {
        self.retire_before_exit();
    }
}

struct Worker {
    request: v2::RootRequest,
    launch: v2::LaunchSelected,
    context: Vec<u8>,
    declaration: LaunchRequest,
    custody: SelectedOriginCustody,
    proof: UnixStream,
    endpoint: UnixStream,
    parent_cancel: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    cancellation: ServiceCancellation,
    state: Arc<Mutex<State>>,
    stopped: Arc<AtomicBool>,
    released: mpsc::Receiver<()>,
}
impl Worker {
    fn run(self) {
        let mut reviewed = None;
        let mut session = None;
        let mut managed = None;
        // Fresh proof uses the SAME original helper and a new independently
        // reviewed target binding. Its cancellation fence precedes first QPE.
        let preparation = (|| {
            let fence = WorkerFence::start(
                &self.proof,
                &self.endpoint,
                Arc::clone(&self.cancel),
                Arc::clone(&self.parent_cancel),
                self.cancellation.clone(),
                Some(PREPARE_BUDGET),
            )?;
            let actor = ReviewedInstalledRetainedLaunch::review_editor(
                self.proof.try_clone()?,
                self.declaration.clone(),
                &self.custody,
                &self.cancel,
            )?;
            reviewed = Some(Arc::clone(&actor));
            if actor.binding() != &self.launch.binding {
                return Err(refused());
            }
            actor.revalidate(&self.cancel)?;
            let origin = RootFilesEditOrigin::admit(self.custody, &self.cancel)?;
            let input = Arc::new(actor.launch_input(&self.cancel)?);
            let challenge = challenge(
                &self.request,
                &self.launch,
                &self.context,
                input.person_public_key(),
            )?;
            let shell = actor.target_peer(&self.cancel)?;
            let consuming = Arc::new(Mutex::new(ManagedConsumingBootstrap::spawn(
                // Inherit this editor's revocation while canonical remote R
                // independently checks kernel/program/namespace custody and
                // remains available for cleanup after cancellation.
                self.cancellation.child(),
            )?));
            managed = Some(Arc::clone(&consuming));
            fence.check()?;
            {
                let mut state = self.state.lock().map_err(|_| refused())?;
                state.prepared = Some(PreparedConsumer {
                    input: Arc::clone(&input),
                    actor: Arc::clone(&actor),
                    managed: Arc::clone(&consuming),
                    challenge,
                    shell,
                });
                state.stage = StageStatus::Prepared;
            }
            drop(fence);
            // Staged transport must be sent successfully before the actual
            // target launcher exists; no QPE or view export runs before release.
            let deadline = Instant::now()
                .checked_add(PREPARE_BUDGET)
                .ok_or(io::ErrorKind::InvalidInput)?;
            loop {
                if self.cancel.load(Ordering::Acquire) || self.parent_cancel.load(Ordering::Acquire)
                {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                match self.released.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err(refused()),
                }
            }
            let fence = WorkerFence::start(
                &self.proof,
                &self.endpoint,
                Arc::clone(&self.cancel),
                Arc::clone(&self.parent_cancel),
                self.cancellation.clone(),
                None,
            )?;
            actor.revalidate(&self.cancel)?;
            let challenge = self
                .state
                .lock()
                .map_err(|_| refused())?
                .prepared
                .as_ref()
                .ok_or_else(refused)?
                .challenge
                .clone();
            let mut channel =
                RootConsumerChannel::authenticate(self.endpoint.try_clone()?, &self.cancellation)?;
            let binding = channel.request(&challenge, &self.cancellation)?;
            let launcher = channel.retain_peer();
            let daemon = self
                .state
                .lock()
                .map_err(|_| refused())?
                .daemon
                .clone()
                .ok_or_else(refused)?;
            daemon.check()?;
            let editor = RootSelectedEditor::admit(
                input,
                Arc::clone(&launcher),
                binding.clone(),
                self.request.clone(),
                Arc::clone(&self.cancel),
            )?;
            let reader = self
                .state
                .lock()
                .map_err(|_| refused())?
                .reader
                .clone()
                .ok_or_else(refused)?
                .admit_editor(editor.clone())?;
            session = Some(RetainedSelectedEditSession::start(
                origin,
                editor,
                Arc::clone(&self.cancel),
                Arc::clone(&consuming),
                Arc::clone(&daemon),
                reader,
            )?);
            let owned = session.as_mut().ok_or_else(refused)?;
            // Store actual typed admission before possible delivery. Cleanup
            // retains the session even if view transport loses its witness.
            self.state.lock().map_err(|_| refused())?.admission =
                Some(Arc::clone(owned.admission()?));
            owned.issue_selected_edit(channel, &self.cancellation)?;
            let admission = owned.admission()?;
            if !admission.consumer_admitted()? {
                return Err(refused());
            }
            let receipt = TargetNamespaceReceipt {
                schema: 1,
                request_id: self.request.request_id.clone(),
                app_id: binding.app_id.clone(),
                package_sha256: binding.package_sha256,
                launcher_pid: launcher.pid(),
                launcher_start: launcher.start_time()?,
            };
            if !receipt.valid() {
                return Err(refused());
            }
            self.state.lock().map_err(|_| refused())?.status =
                EditorStatus::NamespaceReady(receipt);
            let mut refresh_at = Instant::now();
            loop {
                if owned.consumer_observed_stopped()? {
                    break;
                }
                if self.cancel.load(Ordering::Acquire) || self.parent_cancel.load(Ordering::Acquire)
                {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if Instant::now() >= refresh_at {
                    let refresh_fence = WorkerFence::start(
                        &self.proof,
                        &self.endpoint,
                        Arc::clone(&self.cancel),
                        Arc::clone(&self.parent_cancel),
                        self.cancellation.clone(),
                        Some(PREPARE_BUDGET),
                    )?;
                    daemon.refresh()?;
                    owned.check()?;
                    refresh_fence.check()?;
                    drop(refresh_fence);
                    refresh_at = Instant::now()
                        .checked_add(Duration::from_secs(2))
                        .ok_or(io::ErrorKind::InvalidInput)?;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(fence);
            Ok::<(), io::Error>(())
        })();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.stage = StageStatus::Refused;
            state.status = EditorStatus::Refused;
            state.admission.take();
            state.prepared.take();
            state.daemon.take();
        }
        self.cancel.store(true, Ordering::Release);
        self.cancellation.cancel();
        if let Some(actor) = &reviewed {
            actor.revoke();
        }
        if let Some(owned) = session.as_mut() {
            // Possible delivery without a witness cannot prove absence. Retire
            // keeps returning refusal and this exact worker remains the owner.
            loop {
                let result = owned.retire();
                if owned.cleanup_observed_stopped() {
                    break;
                }
                let _ = result;
                std::thread::park_timeout(Duration::from_millis(50));
            }
        }
        drop(session);
        // Session retirement first observes init/outer child and removes the
        // remote exposure, then retires target/relay/source. Only now may the
        // isolated bootstrap exit. Preparation failures retain it here too.
        if let Some(managed) = managed.take() {
            managed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_before_exit();
        }
        if let Some(actor) = reviewed.take() {
            while Arc::strong_count(&actor) != 1 {
                actor.revoke();
                std::thread::park_timeout(Duration::from_millis(50));
            }
            if actor.retire().is_err() {
                // Canonical retirement did not prove source stop. Never mint
                // stopped from a worker return or let the root process exit.
                loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            }
        }
        let _ = preparation;
        // Constructor failure may precede reviewed actor publication. Keep
        // this worker/controller live while exact native source cleanup is
        // terminally uncertain; no absent actor can manufacture stopped.
        while ReviewedInstalledRetainedLaunch::retirement_pending() {
            std::thread::park_timeout(Duration::from_millis(50));
        }
        self.stopped.store(true, Ordering::Release);
    }
}

fn review_declaration(
    request: &v2::RootRequest,
    launch: &v2::LaunchSelected,
    context: &[u8],
) -> io::Result<LaunchRequest> {
    if !request.valid()
        || !v2::RootCommand::StageSelectedConsumer(launch.clone()).valid()
        || request.request_id != launch.request_id
        || request.app_id != launch.binding.app_id
        || request.access != launch.access
        || launch.binding.resource_record != request.resource_record()
    {
        return Err(refused());
    }
    let _ = challenge(request, launch, context, [0; 32])?;
    LaunchRequest::decode(&serde_json::to_vec(&serde_json::json!({
        "schema":1,"operation_id":launch.binding.operation_id,"app_id":launch.binding.app_id,
        "package_sha256":launch.binding.package_sha256,"room":launch.binding.room,
        "resource_record":launch.binding.resource_record,"context_sha256":launch.binding.context_sha256,
    })).map_err(io::Error::other)?)
}
type Context = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<String>,
    Vec<(String, String)>,
);
fn challenge(
    request: &v2::RootRequest,
    launch: &v2::LaunchSelected,
    bytes: &[u8],
    person: [u8; 32],
) -> io::Result<ConsumerChallenge> {
    if bytes.is_empty()
        || bytes.len() > 64 * 1024
        || !bytes.starts_with(DOMAIN)
        || hex::encode(Sha256::digest(bytes)) != launch.binding.context_sha256
    {
        return Err(refused());
    }
    let (operation, app, publisher, package, room, resource, program, args, env): Context =
        serde_json::from_slice(&bytes[DOMAIN.len()..]).map_err(io::Error::other)?;
    if operation != launch.binding.operation_id
        || app != launch.binding.app_id
        || app != request.app_id
        || publisher != launch.publisher_hex
        || package != launch.binding.package_sha256
        || room != launch.binding.room
        || resource != request.resource_record()
        || resource != launch.binding.resource_record
    {
        return Err(refused());
    }
    Ok(ConsumerChallenge {
        operation_id: operation,
        shell_context_sha256: launch.binding.context_sha256.clone(),
        person,
        resource_record: resource,
        publisher_hex: publisher,
        command: ConsumerCommand { program, args, env },
    })
}

/// Exact socket fence: finite preparation before first QPE; lifetime cancellation
/// thereafter. It owns socket clones only, never source/namespace authority.
struct WorkerFence {
    done: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    parent: Arc<AtomicBool>,
    deadline: Option<Instant>,
}
impl WorkerFence {
    fn start(
        proof: &UnixStream,
        endpoint: &UnixStream,
        cancel: Arc<AtomicBool>,
        parent: Arc<AtomicBool>,
        control: ServiceCancellation,
        budget: Option<Duration>,
    ) -> io::Result<Self> {
        let proof = proof.try_clone()?;
        let endpoint = endpoint.try_clone()?;
        let deadline = budget
            .map(|budget| {
                Instant::now()
                    .checked_add(budget)
                    .ok_or(io::ErrorKind::InvalidInput)
            })
            .transpose()?;
        let done = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&done);
        let own = Arc::clone(&cancel);
        let root = Arc::clone(&parent);
        let worker = std::thread::Builder::new()
            .name("retained-editor-proof-fence".into())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    if own.load(Ordering::Acquire)
                        || root.load(Ordering::Acquire)
                        || control.is_cancelled()
                        || deadline.is_some_and(|d| Instant::now() >= d)
                    {
                        own.store(true, Ordering::Release);
                        control.cancel();
                        let _ = proof.shutdown(std::net::Shutdown::Both);
                        let _ = endpoint.shutdown(std::net::Shutdown::Both);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })?;
        Ok(Self {
            done,
            worker: Some(worker),
            cancel,
            parent,
            deadline,
        })
    }
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) || self.parent.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}
impl Drop for WorkerFence {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Check the installed signed worker/runtime cohort used by the concrete linked
/// RetainedSelectedEditSession factory. This creates no source/app capability.
pub(super) fn check_installed_edit_backend() -> io::Result<()> {
    let _service = super::home_capture::TrustedCaptureService::native()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    fn declaration() -> (v2::RootRequest, v2::LaunchSelected, Vec<u8>) {
        let request = v2::RootRequest {
            schema: 2,
            request_id: "11111111-1111-4111-8111-111111111111".into(),
            components: vec![b"notes.txt".to_vec()],
            app_id: "so.tilo.text-editor".into(),
            access: v2::SelectedAccess::EditInParent,
        };
        let mut context = DOMAIN.to_vec();
        context.extend(
            serde_json::to_vec(&(
                "22222222-2222-4222-8222-222222222222",
                &request.app_id,
                "a".repeat(64),
                "b".repeat(64),
                "33333333-3333-4333-8333-333333333333",
                request.resource_record(),
                "/usr/bin/tilo-launcher",
                vec!["--open", "/run/tilo/retained-selected/notes.txt"],
                Vec::<(String, String)>::new(),
            ))
            .unwrap(),
        );
        let launch = v2::LaunchSelected {
            schema: 2,
            request_id: request.request_id.clone(),
            access: request.access,
            publisher_hex: "a".repeat(64),
            binding: runtime_collab::retained_person::RetainedLaunchBinding {
                operation_id: "22222222-2222-4222-8222-222222222222".into(),
                app_id: request.app_id.clone(),
                package_sha256: "b".repeat(64),
                room: "33333333-3333-4333-8333-333333333333".into(),
                resource_record: request.resource_record(),
                target_uid: 1000,
                target_pid: 42,
                root_pid: 1,
                context_sha256: hex::encode(Sha256::digest(&context)),
            },
        };
        (request, launch, context)
    }
    #[test]
    fn stage_declaration_rejects_context_publisher_access_and_request_substitution() {
        let (request, launch, context) = declaration();
        // Parsing a closed declaration is inert; this test mints no proof,
        // native source, Prepared state, namespace receipt or write grant.
        assert!(review_declaration(&request, &launch, &context).is_ok());
        let mut altered = launch.clone();
        altered.publisher_hex = "d".repeat(64);
        assert!(review_declaration(&request, &altered, &context).is_err());
        altered = launch.clone();
        altered.request_id = "44444444-4444-4444-8444-444444444444".into();
        assert!(review_declaration(&request, &altered, &context).is_err());
        altered = launch.clone();
        altered.access = v2::SelectedAccess::ReadOnly;
        assert!(review_declaration(&request, &altered, &context).is_err());
        let mut bytes = context;
        bytes.push(b' ');
        assert!(review_declaration(&request, &launch, &bytes).is_err());
        assert!(review_declaration(&request, &launch, &vec![0; 64 * 1024 + 1]).is_err());
    }
    #[test]
    fn fresh_editor_proof_fence_stops_real_stalled_initial_qpe() {
        let (mut proof, mut peer) = UnixStream::pair().unwrap();
        let (endpoint, mut launcher) = UnixStream::pair().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let control = ServiceCancellation::default();
        let fence = WorkerFence::start(
            &proof,
            &endpoint,
            Arc::clone(&cancel),
            Arc::new(AtomicBool::new(false)),
            control.clone(),
            Some(Duration::from_millis(80)),
        )
        .unwrap();
        peer.write_all(&runtime_portal::process_binding::MAGIC[..3])
            .unwrap();
        let started = Instant::now();
        assert!(runtime_portal::process_binding::complete(&mut proof).is_err());
        assert!(fence.check().is_err());
        assert!(cancel.load(Ordering::Acquire));
        assert!(control.is_cancelled());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(launcher.read(&mut [0]).unwrap(), 0);
    }
    #[test]
    fn root_shutdown_interrupts_owned_editor_proof_without_claiming_namespace_stop() {
        let (mut proof, mut peer) = UnixStream::pair().unwrap();
        let (endpoint, mut launcher) = UnixStream::pair().unwrap();
        let parent = Arc::new(AtomicBool::new(false));
        let own = Arc::new(AtomicBool::new(false));
        let _fence = WorkerFence::start(
            &proof,
            &endpoint,
            Arc::clone(&own),
            Arc::clone(&parent),
            ServiceCancellation::default(),
            None,
        )
        .unwrap();
        let signal = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            parent.store(true, Ordering::Release);
        });
        assert!(runtime_portal::process_binding::complete(&mut proof).is_err());
        signal.join().unwrap();
        assert!(own.load(Ordering::Acquire));
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        assert_eq!(launcher.read(&mut [0]).unwrap(), 0);
    }
}

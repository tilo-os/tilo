//! Fixed installed helper composition of the certified Shell, original source,
//! target filesystem and ordinary consuming namespace. No GUI or sample state.
pub mod original_copy;
use crate::retained_source::consumer::{
    endpoint::{ConsumerEndpoint, EndpointRole},
    namespace,
};

use super::{
    home_capture::RequestDirectoryPin,
    installed_retained_actor::{LaunchRequest, ReviewedInstalledRetainedLaunch},
    migration::PinnedDirectory,
    retained_consumer_lease::{PendingRetainedConsumer, RetainedConsumerLease},
    retained_consumer_selection::ReviewedConsumerSelection,
    retained_open_dispatch::RetainedOpenDispatcher,
    retained_projection_session::RetainedProjectionSession,
};
use launcher::{
    ordinary_child::ManagedConsumingBootstrap,
    owned_service::ServiceCancellation,
    retained_view::{ConsumerChallenge, ConsumerCommand, RootConsumerChannel},
};
use runtime_portal::{
    process_binding,
    request_rendezvous::{self, DirectoryPin, Rendezvous},
};
use sha2::{Digest, Sha256};
use std::os::fd::AsFd;
use std::{
    ffi::OsString,
    io::{self, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const CONTEXT_DOMAIN: &[u8] = b"tilo-ordinary-retained-home-context-v1\0";
const BOOTSTRAP_BUDGET: Duration = Duration::from_secs(120);
const _: () =
    assert!(EndpointRole::Retirement.tag() == runtime_collab::retained_retirement::ENDPOINT_TAG);
/// Fixed internal owned-child mode. This entry carries no source paths or
/// serialized grants; the child authenticates its actual parent and Shell.
pub(super) fn run_consuming_bootstrap(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    crate::retained_source::consumer::run(arguments)
}

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: u32,
    channel: Rendezvous,
    launch: serde_json::Value,
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
    bytes: &[u8],
    binding: &runtime_collab::retained_person::RetainedLaunchBinding,
    person: [u8; 32],
) -> io::Result<ConsumerChallenge> {
    if bytes.is_empty() || bytes.len() > 64 * 1024 || !bytes.starts_with(CONTEXT_DOMAIN) {
        return Err(refused());
    }
    if hex::encode(Sha256::digest(bytes)) != binding.context_sha256 {
        return Err(refused());
    }
    let (operation, app, publisher, package, room, resource, program, args, env): Context =
        serde_json::from_slice(&bytes[CONTEXT_DOMAIN.len()..]).map_err(io::Error::other)?;
    if operation != binding.operation_id
        || app != binding.app_id
        || package != binding.package_sha256
        || room != binding.room
        || resource != "retained-home"
        || resource != binding.resource_record
    {
        return Err(refused());
    }
    Ok(ConsumerChallenge {
        operation_id: operation,
        shell_context_sha256: binding.context_sha256.clone(),
        person,
        resource_record: resource,
        publisher_hex: publisher,
        command: ConsumerCommand { program, args, env },
    })
}

/// Invoked only by the fixed installed helper mode with one existing private
/// request path. Source paths, native credentials, certificates and programs
/// are never arguments to this entry point.
pub(super) fn run(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    let mut terminal = None;
    let result = run_current(arguments, &mut terminal);
    // A source constructor can fail before a reviewed actor exists. Its
    // terminal cleanup latch retains actual quarantined native mount custody;
    // an empty actor slot cannot prove the fixed helper may exit.
    while ReviewedInstalledRetainedLaunch::retirement_pending() {
        std::thread::park_timeout(Duration::from_millis(50));
    }
    if let Some(mut observation) = terminal {
        runtime_collab::retained_retirement::write(&mut observation.writer, &observation.subject)?;
        observation.writer.shutdown(std::net::Shutdown::Write)?;
    }
    result
}
struct CleanupObservation {
    writer: UnixStream,
    subject: runtime_collab::retained_retirement::Subject,
    _gate: super::retained_consumer_lease::RetirementGate,
}
fn run_current(
    arguments: impl Iterator<Item = OsString>,
    terminal: &mut Option<CleanupObservation>,
) -> io::Result<()> {
    let mut arguments = arguments;
    let path = arguments.next().ok_or_else(refused)?;
    if arguments.next().is_some() {
        return Err(refused());
    }
    RootConsumerChannel::verify_current_helper()?;
    let path = Path::new(&path);
    if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new("request.json")) {
        return Err(refused());
    }
    let directory = path.parent().ok_or_else(refused)?;
    let pinned = PinnedDirectory::open_absolute(directory)?;
    let pin = RequestDirectoryPin(pinned);
    let caller = pin.file().metadata()?.uid();
    if caller == 0 {
        return Err(refused());
    }
    let bytes = request_rendezvous::read_request_bytes(&pin, caller, 16 * 1024)?;
    let request: Request = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if request.schema != 1 {
        return Err(refused());
    }
    let launch =
        LaunchRequest::decode(&serde_json::to_vec(&request.launch).map_err(io::Error::other)?)?;
    let mut bootstrap = request.channel.connect(directory, &pin, caller)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let cancellation = ServiceCancellation::default();
    let fence = BootstrapFence::start(&bootstrap, Arc::clone(&cancel), cancellation.clone())?;
    bootstrap.set_read_timeout(Some(Duration::from_secs(10)))?;
    bootstrap.set_write_timeout(Some(Duration::from_secs(10)))?;
    process_binding::arm(&bootstrap)?;
    let mut nonce = [0; 32];
    nonce[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    nonce[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bootstrap.write_all(&process_binding::MAGIC)?;
    bootstrap.write_all(&nonce)?;
    let shell = process_binding::receive(&bootstrap, &nonce, caller)?;
    if shell.pid() != request.channel.caller_pid() || !shell.current() {
        return Err(refused());
    }
    bootstrap.write_all(&[1])?;
    process_binding::complete(&mut bootstrap)?;
    bootstrap.set_read_timeout(Some(Duration::from_secs(10)))?;
    bootstrap.set_write_timeout(Some(Duration::from_secs(10)))?;
    RootConsumerChannel::verify_current_helper()?;
    pin.validate()?;
    let proof = receive_endpoint(&mut bootstrap, EndpointRole::PersonProof)?;
    let mut reviewed = Some(ReviewedInstalledRetainedLaunch::review(
        proof, launch, &cancel,
    )?);
    let result = run_reviewed(
        &mut bootstrap,
        &request.channel,
        &shell,
        &mut reviewed,
        &cancel,
        &cancellation,
        &fence,
        terminal,
    );
    // The controller normally consumes this exact slot once Files and every
    // unadopted worker have retired, before editor-only supervision. Early
    // constructor failures still retain their actual actor here through stop.
    if let Some(actor) = reviewed.take() {
        if result.is_err() {
            cancel.store(true, Ordering::Release);
            actor.revoke();
        }
        while Arc::strong_count(&actor) != 1 {
            cancel.store(true, Ordering::Release);
            actor.revoke();
            std::thread::park_timeout(Duration::from_millis(50));
        }
        if actor.retire().is_err() {
            // Consuming retirement can retain uncertain mounted custody.
            // No remaining DTO/Arc can retry it or prove process exit safe.
            cancel.store(true, Ordering::Release);
            loop {
                std::thread::park_timeout(Duration::from_millis(50));
            }
        }
    }
    drop(fence);
    result
}

fn receive_endpoint(stream: &mut UnixStream, role: EndpointRole) -> io::Result<UnixStream> {
    let channel = std::fs::File::from(stream.as_fd().try_clone_to_owned()?);
    let endpoint = ConsumerEndpoint::receive(&channel, role)?;
    Ok(UnixStream::from(std::os::fd::OwnedFd::from(
        endpoint.into_file(),
    )))
}

fn run_reviewed(
    bootstrap: &mut UnixStream,
    rendezvous: &Rendezvous,
    shell: &process_binding::ExecutingPeer,
    reviewed_slot: &mut Option<Arc<ReviewedInstalledRetainedLaunch>>,
    cancel: &Arc<AtomicBool>,
    cancellation: &ServiceCancellation,
    fence: &BootstrapFence,
    terminal: &mut Option<CleanupObservation>,
) -> io::Result<()> {
    let reviewed = reviewed_slot.as_ref().ok_or_else(refused)?;
    let binding = reviewed.binding();
    if !shell.current()
        || binding.target_pid != shell.pid()
        || binding.target_uid != shell.uid()
        || rendezvous.caller_pid() != shell.pid()
    {
        return Err(refused());
    }
    let endpoint = receive_endpoint(bootstrap, EndpointRole::Consumer)?;
    let cleanup = receive_endpoint(bootstrap, EndpointRole::Retirement)?;
    let cleanup_subject = runtime_collab::retained_retirement::Subject {
        source_operation: binding.operation_id.clone(),
        context_sha256: binding.context_sha256.clone(),
        root_pid: std::process::id(),
    };
    cleanup_subject.validate()?;
    let context = wire_frame::Framing::little_endian(64 * 1024)
        .reject_empty()
        .read(bootstrap)
        .map_err(io::Error::other)?
        .ok_or_else(refused)?;
    let input = Arc::new(reviewed.launch_input(cancel)?);
    let challenge = challenge(&context, input.binding(), input.person_public_key())?;
    // Independent ordinary Files Read enrollment precedes target release; the
    // actual issuer and source input belong to this Files lifetime, not an editor.
    let history_reader = super::retained_history_read::enroll_files_read(
        bootstrap,
        input.clone(),
        cancel.clone(),
        fence.initial_deadline()?,
    )?;
    // Keep the authenticated original source/UDisks actor in this namespace.
    // Only this separately owned fixed child isolates a private consuming
    // namespace and spawns the target-credential ordinary launcher there.
    let managed = Arc::new(Mutex::new(ManagedConsumingBootstrap::spawn(
        cancellation.clone(),
    )?));
    {
        let mut owner = managed.lock().map_err(|_| refused())?;
        match owner.delegate_shell_command(
            bootstrap,
            &challenge.command,
            shell,
            fence.initial_deadline()?,
        )? {
            launcher::ordinary_child::DelegationOutcome::Delegated => {}
            launcher::ordinary_child::DelegationOutcome::Refused => return Err(refused()),
        }
    }
    let mut channel = RootConsumerChannel::authenticate(endpoint, cancellation)?;
    let consumer = channel.request(&challenge, cancellation)?;
    let selection = ReviewedConsumerSelection::review(
        Arc::clone(&input),
        channel.retain_peer(),
        consumer,
        context,
        cancel,
    )?;
    let projection = RetainedProjectionSession::start_home_for_consumer_with_history(
        Arc::clone(&input),
        &selection,
        Arc::clone(cancel),
        history_reader,
    )?;
    let mut pending =
        PendingRetainedConsumer::new(Arc::clone(&input), projection, cancel, managed)?;
    let export = pending.export_view(&selection, cancel, channel, cancellation);
    let lease = match pending.observe_consumer(selection) {
        Ok(lease) => lease,
        Err(mut pending) => {
            // Failed receipt-only/pre-view transport retires normally. If the
            // canonical sender may have delivered a view but its namespace
            // witness was lost, the fixed helper remains the live owner.
            pending.retain_failed_handoff(cancel);
            return Err(export.err().unwrap_or_else(refused));
        }
    };
    let lease = Arc::new(Mutex::new(lease));
    drop(input);
    let result = (|| {
        export?;
        lease.lock().map_err(|_| refused())?.check(cancel)?;
        fence.observed_consumer()?;
        let mut opens = RetainedOpenDispatcher::new(
            bootstrap.try_clone()?,
            Arc::clone(cancel),
            cancellation.clone(),
            lease
                .lock()
                .map_err(|_| refused())?
                .original_proof_interrupt()?,
        )?;
        let mut gate = None;
        let outcome = RetainedConsumerLease::supervise_shared_with_open_requests(
            &lease,
            cancel,
            Duration::from_secs(5),
            &mut opens,
            reviewed_slot,
            &mut gate,
        );
        if let Some(gate) = gate {
            *terminal = Some(CleanupObservation {
                writer: cleanup,
                subject: cleanup_subject,
                _gate: gate,
            });
        }
        outcome
    })();
    if result.is_err() {
        // A forgotten Arc cannot outlive helper process exit. Keep this actual
        // owner here until the held init pidfd and both workers prove stopped.
        // Poisoning cannot prove stop or permit process exit. Retain the exact
        // owner even after an unrelated lock holder panicked.
        match lease.lock() {
            Ok(mut owner) => owner.retire_before_exit(cancel),
            Err(poisoned) => poisoned.into_inner().retire_before_exit(cancel),
        }
    }
    result
}
/// The original private bootstrap is bounded before its first QPE operation.
/// After real namespace admission it observes disconnect/cancellation for the
/// whole app lifetime; fresh certified proof is refreshed by the joint owner.
struct BootstrapFence {
    phase: Arc<Mutex<FencePhase>>,
    finished: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
struct FencePhase {
    deadline: Option<Instant>,
}
impl BootstrapFence {
    fn start(
        stream: &UnixStream,
        cancel: Arc<AtomicBool>,
        cancellation: ServiceCancellation,
    ) -> io::Result<Self> {
        Self::start_with_budget(stream, cancel, cancellation, BOOTSTRAP_BUDGET)
    }
    fn start_with_budget(
        stream: &UnixStream,
        cancel: Arc<AtomicBool>,
        cancellation: ServiceCancellation,
        budget: Duration,
    ) -> io::Result<Self> {
        let stream = stream.try_clone()?;
        let deadline = Instant::now()
            .checked_add(budget)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let phase = Arc::new(Mutex::new(FencePhase {
            deadline: Some(deadline),
        }));
        let finished = Arc::new(AtomicBool::new(false));
        let worker_phase = Arc::clone(&phase);
        let worker_finished = Arc::clone(&finished);
        let worker = std::thread::Builder::new()
            .name("retained-root-bootstrap-fence".into())
            .spawn(move || {
                while !worker_finished.load(Ordering::Acquire) {
                    let state = worker_phase.lock();
                    let refusal = match state {
                        Ok(phase) => {
                            // Once admitted, only explicit global cancellation
                            // belongs to this fence. The mux owns clean EOF at
                            // complete request boundaries and rejects truncation.
                            cancel.load(Ordering::Acquire)
                                || phase
                                    .deadline
                                    .is_some_and(|deadline| Instant::now() >= deadline)
                        }
                        Err(_) => true,
                    };
                    if refusal {
                        cancel.store(true, Ordering::Release);
                        cancellation.cancel();
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })?;
        Ok(Self {
            phase,
            finished,
            worker: Some(worker),
        })
    }
    fn initial_deadline(&self) -> io::Result<Instant> {
        let deadline = self
            .phase
            .lock()
            .map_err(|_| refused())?
            .deadline
            .ok_or_else(refused)?;
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(deadline)
    }
    fn observed_consumer(&self) -> io::Result<()> {
        let mut phase = self.phase.lock().map_err(|_| refused())?;
        phase.deadline = None;
        Ok(())
    }
}
impl Drop for BootstrapFence {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn run_original_copy(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    original_copy::run(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    fn context() -> (
        Vec<u8>,
        runtime_collab::retained_person::RetainedLaunchBinding,
    ) {
        let operation = "11111111-1111-4111-8111-111111111111";
        let room = "22222222-2222-4222-8222-222222222222";
        let mut bytes = CONTEXT_DOMAIN.to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                operation,
                "so.tilo.files",
                "a".repeat(64),
                "b".repeat(64),
                room,
                "retained-home",
                "/usr/libexec/tilo/tilo-launcher",
                vec!["run"],
                Vec::<(String, String)>::new(),
            ))
            .unwrap(),
        );
        let binding = runtime_collab::retained_person::RetainedLaunchBinding {
            operation_id: operation.into(),
            app_id: "so.tilo.files".into(),
            package_sha256: "b".repeat(64),
            room: room.into(),
            resource_record: "retained-home".into(),
            target_uid: 1000,
            target_pid: 12,
            root_pid: 13,
            context_sha256: hex::encode(Sha256::digest(&bytes)),
        };
        (bytes, binding)
    }
    #[test]
    fn canonical_shell_context_cannot_change_selected_app_scope_or_digest() {
        let (bytes, binding) = context();
        let decoded = challenge(&bytes, &binding, [1; 32]).unwrap();
        assert_eq!(decoded.command.program, "/usr/libexec/tilo/tilo-launcher");
        assert_eq!(decoded.command.args, ["run"]);
        for field in ["operation", "app", "package", "room", "resource", "digest"] {
            let mut changed = binding.clone();
            match field {
                "operation" => changed.operation_id = "33333333-3333-4333-8333-333333333333".into(),
                "app" => changed.app_id = "so.tilo.terminal".into(),
                "package" => changed.package_sha256 = "c".repeat(64),
                "room" => changed.room = "44444444-4444-4444-8444-444444444444".into(),
                "resource" => changed.resource_record = "another-source".into(),
                _ => changed.context_sha256 = "d".repeat(64),
            }
            assert!(
                challenge(&bytes, &changed, [1; 32]).is_err(),
                "changed {field}"
            );
        }
        let mut altered = bytes;
        altered.push(b' ');
        assert!(challenge(&altered, &binding, [1; 32]).is_err());
        assert!(challenge(&vec![b'a'; 64 * 1024 + 1], &binding, [1; 32]).is_err());
    }
    /// Start a fence and find its watching thread.
    fn fence_thread(start: impl FnOnce() -> BootstrapFence) -> (BootstrapFence, String) {
        tilo_test_env::idle::spawned_thread("retained-root-b", start)
    }

    /// Wait until the fence's thread has woken to look `turns` more times.
    fn fence_turns(tid: &str, turns: u64) {
        let from = tilo_test_env::idle::context_switches(tid);
        tilo_test_env::wait_until("the fence to look at its transport again", || {
            tilo_test_env::idle::context_switches(tid) >= from + turns
        });
    }

    #[test]
    fn bootstrap_absolute_deadline_interrupts_real_blocked_socket() {
        let (mut receiver, mut peer) = UnixStream::pair().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let control = ServiceCancellation::default();
        let fence = BootstrapFence::start_with_budget(
            &receiver,
            cancel.clone(),
            control.clone(),
            Duration::from_millis(80),
        )
        .unwrap();
        // The peer never writes: only the fence's deadline ends the read.
        receiver
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut byte = [0];
        assert_eq!(receiver.read(&mut byte).unwrap(), 0);
        assert!(cancel.load(Ordering::Acquire));
        assert!(control.is_cancelled());
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
        drop(fence);
    }
    #[test]
    fn admitted_bootstrap_data_and_clean_eof_belong_to_mux() {
        let (mut receiver, mut peer) = UnixStream::pair().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let control = ServiceCancellation::default();
        let (fence, tid) = fence_thread(|| {
            BootstrapFence::start_with_budget(
                &receiver,
                Arc::clone(&cancel),
                control.clone(),
                tilo_test_env::hang_bound(),
            )
            .unwrap()
        });
        fence.observed_consumer().unwrap();
        peer.write_all(b"request").unwrap();
        // Let the fence look at the admitted transport a few times with
        // unread data still queued. POLLIN must not be interpreted as
        // disconnection.
        fence_turns(&tid, 3);
        assert!(!cancel.load(Ordering::Acquire));
        assert!(!control.is_cancelled());
        let mut bytes = [0u8; 7];
        receiver.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"request");
        drop(peer);
        fence_turns(&tid, 3);
        assert_eq!(receiver.read(&mut [0]).unwrap(), 0);
        assert!(!cancel.load(Ordering::Acquire));
        assert!(!control.is_cancelled());
        drop(fence);
    }
    #[test]
    fn explicit_global_cancel_revokes_admitted_bootstrap_owner() {
        let (mut receiver, _peer) = UnixStream::pair().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let control = ServiceCancellation::default();
        let fence = BootstrapFence::start_with_budget(
            &receiver,
            cancel.clone(),
            control.clone(),
            tilo_test_env::hang_bound(),
        )
        .unwrap();
        fence.observed_consumer().unwrap();
        cancel.store(true, Ordering::Release);
        tilo_test_env::wait_until("the fence to revoke the cancelled owner", || {
            control.is_cancelled()
        });
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(receiver.read(&mut [0]).unwrap(), 0);
        drop(fence);
    }
}

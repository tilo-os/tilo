//! The fixed installed root consumer, reached through the existing private
//! capture request/rendezvous. No inherited descriptor is assumed under pkexec.
mod installed_copy;
use super::{PreparedRetainedHome, Registration, refused};
pub(crate) use installed_copy::OriginalCopyBootstrapCustody;
pub(super) use installed_copy::establish_original_copy;
use runtime_portal::{
    fd_transport, process_binding,
    request_rendezvous::{self, DirectoryPin},
};
use sha2::Digest;
use std::os::fd::AsFd;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const HELPER: &str = "/usr/libexec/tilo-prepared-setup";
const SHELL: &str = "/usr/bin/shell";
const PKEXEC: &str = "/usr/bin/pkexec";
const OPTION: &str = "--retained-home-consumer";
const REQUEST_BOUND: usize = 16 * 1024;
const BOOTSTRAP_TIME: Duration = Duration::from_secs(120);

/// Caller-supplied operation comes from the installed retained source record;
/// no fresh migration operation is generated for a launch. Root independently
/// checks that operation against its installed source and consent custody.
pub(super) fn start(
    prepared: Arc<PreparedRetainedHome>,
    installed_operation: &str,
    cancel: Arc<AtomicBool>,
) -> io::Result<Registration> {
    let operation = uuid::Uuid::parse_str(installed_operation).map_err(io::Error::other)?;
    if operation.is_nil() || operation.to_string() != installed_operation {
        return Err(refused());
    }
    if cancel.load(Ordering::Acquire) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    prepared.check(&prepared.command)?;
    let context = prepared.context_bytes(installed_operation)?;
    let shell = ProtectedProgram::open(Path::new(SHELL))?;
    shell.check_current_image()?;
    let helper = ProtectedProgram::open(Path::new(HELPER))?;
    let pkexec = ProtectedProgram::open(Path::new(PKEXEC))?;
    let scratch = private_scratch_in(&super::super::runtime_dir())?;
    let pin = PrivateDirectory::open(scratch.path())?;
    let (channel, listener) = request_rendezvous::Rendezvous::create(scratch.path(), pin.clone())?;
    let request = serde_json::to_vec(&serde_json::json!({
        "schema": 1, "channel": channel,
        "launch": { "schema": 1, "operation_id": installed_operation,
            "app_id": prepared.package.id, "package_sha256": prepared.package.package_id_hex,
            "room": prepared.room.to_string(), "resource_record": super::RESOURCE,
            "context_sha256": hex::encode(sha2::Sha256::digest(&context)) }
    }))
    .map_err(io::Error::other)?;
    let published =
        request_rendezvous::publish_request_bytes(pin.as_ref(), &request, REQUEST_BOUND)?;
    helper.check()?;
    pkexec.check()?;
    let observation = Arc::new(super::retirement::Observation::reserve(
        installed_operation,
        &hex::encode(sha2::Sha256::digest(&context)),
    )?);
    let mut child = Command::new(PKEXEC)
        .arg(HELPER)
        .arg(OPTION)
        .arg(scratch.path().join("request.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now()
        .checked_add(BOOTSTRAP_TIME)
        .unwrap_or_else(Instant::now);
    let selection = BootstrapSelection {
        prepared,
        installed_operation: installed_operation.to_owned(),
        context,
        shell,
        helper,
        pkexec,
        cancel,
        observation: observation.clone(),
    };
    let result = establish(selection, &mut child, &listener, deadline);
    drop(published);
    drop(listener);
    match result {
        Ok((registration, proof, bootstrap, cleanup)) => {
            supervise_observed(
                child,
                scratch,
                Some((
                    proof,
                    bootstrap,
                    registration
                        .dispatcher
                        .as_ref()
                        .ok_or_else(refused)?
                        .clone(),
                    cleanup,
                )),
                observation,
            )?;
            Ok(registration)
        }
        Err(error) => {
            // Root owns the confined source/target and their retirement. Closing
            // the private channels cancels its admission; never claim a failed
            // user-UID kill retired a privileged process. Custody waits for exit.
            supervise_observed(child, scratch, None, observation)?;
            Err(error)
        }
    }
}

struct BootstrapSelection {
    prepared: Arc<PreparedRetainedHome>,
    installed_operation: String,
    context: Vec<u8>,
    shell: ProtectedProgram,
    helper: ProtectedProgram,
    pkexec: ProtectedProgram,
    cancel: Arc<AtomicBool>,
    observation: Arc<super::retirement::Observation>,
}
struct CleanupObservation {
    reader: UnixStream,
    subject: runtime_collab::retained_retirement::Subject,
}
#[allow(
    clippy::too_many_lines,
    reason = "one ordered privileged bootstrap: each step checks the custody the step before established"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the dispatcher is moved into the registration the bootstrap ends in"
)]
fn establish(
    selection: BootstrapSelection,
    child: &mut Child,
    listener: &request_rendezvous::Listener,
    deadline: Instant,
) -> io::Result<(
    Registration,
    Vec<ProofCustody>,
    UnixStream,
    CleanupObservation,
)> {
    let BootstrapSelection {
        prepared,
        installed_operation,
        context,
        shell,
        helper,
        pkexec,
        cancel,
        observation,
    } = selection;
    listener.socket().set_nonblocking(true)?;
    let mut stream = loop {
        alive(child, &cancel, deadline)?;
        listener.validate()?;
        match listener.socket().accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let fence = IoFence::new(stream.try_clone()?, cancel.clone(), deadline)?;
    // Root first pins this actual Shell, then this Shell pins the exact
    // owned fixed-helper child. A different UID0 process is refused.
    process_binding::complete(&mut stream)?;
    alive(child, &cancel, deadline)?;
    process_binding::arm(&stream)?;
    let mut challenge = [0; 32];
    challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    stream.write_all(&process_binding::MAGIC)?;
    stream.write_all(&challenge)?;
    let root = process_binding::receive(&stream, &challenge, 0)?;
    if root.pid() != child.id() || !root.current() {
        return Err(refused());
    }
    helper.check()?;
    pkexec.check()?;
    stream.write_all(&[1])?;
    let binding = prepared.binding(&installed_operation, root.pid())?;
    let (proof_stream, proof_root) = UnixStream::pair()?;
    let proof_shutdown = proof_stream.try_clone()?;
    let proof_prepared = prepared.clone();
    let proof_binding = binding.clone();
    let proof = std::thread::Builder::new()
        .name("tilo-retained-person-proof".into())
        .spawn(move || proof_prepared.serve_proof(proof_stream, &proof_binding))?;
    let proof = ProofCustody {
        shutdown: proof_shutdown,
        worker: Some(proof),
        tracking: None,
    };
    handoff(&mut stream, 71, &proof_root, &root, &cancel, deadline)?;
    let (launcher_endpoint, root_endpoint) = UnixStream::pair()?;
    handoff(&mut stream, 72, &root_endpoint, &root, &cancel, deadline)?;
    let subject = observation.bind(&root)?;
    let (cleanup_reader, cleanup_writer) = UnixStream::pair()?;
    cleanup_reader.shutdown(std::net::Shutdown::Write)?;
    cleanup_writer.shutdown(std::net::Shutdown::Read)?;
    handoff(
        &mut stream,
        runtime_collab::retained_retirement::ENDPOINT_TAG,
        &cleanup_writer,
        &root,
        &cancel,
        deadline,
    )?;
    drop(cleanup_writer);
    let cleanup = CleanupObservation {
        reader: cleanup_reader,
        subject,
    };
    // ACKs above acknowledge descriptor transport only. Root still has to
    // admit the person, signed selection and actual launcher before view.
    wire_frame::Framing::little_endian(64 * 1024)
        .reject_empty()
        .write(&mut stream, &context)
        .map_err(io::Error::other)?;
    alive(child, &cancel, deadline)?;
    if !root.current()
        || proof
            .worker
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    {
        return Err(refused());
    }
    let root = Arc::new(AdmittedRootHelper {
        start: root.start_time()?,
        peer: root,
        shell,
        helper,
        pkexec,
        cancel: cancel.clone(),
    });
    prepared.check_history_read()?;
    let request = runtime_fs::external_history::daemon_control::EnrollmentRequest {
        schema: 1,
        request_id: uuid::Uuid::new_v4().to_string(),
        person_binding: serde_json::to_vec(&binding).map_err(io::Error::other)?,
    };
    let Some((recipient, context)) =
        receive_history_read_daemon_delegation(&mut stream, root.clone(), &request)?
    else {
        return Err(refused());
    };
    prepared.check_history_read_context(&binding, &context)?;
    let (daemon_stream, daemon_endpoint) = UnixStream::pair()?;
    let read_prepared = prepared.clone();
    let read_binding = binding.clone();
    let read_session = prepared.session.clone();
    let read_proof = start_selected_proof(daemon_stream, move |proof_stream| {
        read_session.serve_retained_daemon_proof_checked(
            proof_stream,
            &read_binding.clone(),
            &recipient,
            move || read_prepared.check_history_read_context(&read_binding, &context),
        )
    })?;
    // Descriptor transport alone is not enrollment. Root must finish the
    // canonical Read daemon handshake before ordinary Files delegation.
    handoff(
        &mut stream,
        62,
        &daemon_endpoint,
        root.executing(),
        &cancel,
        deadline,
    )?;
    alive(child, &cancel, deadline)?;
    let proofs = vec![proof, read_proof];
    let ordinary = launcher::ordinary_child::DelegatedConsumingBootstrap::receive_until(
        &mut stream,
        root.executing(),
        deadline,
    )?
    .receive_ordinary_until(
        &launcher::retained_view::ConsumerCommand {
            program: prepared
                .command
                .program
                .to_str()
                .ok_or_else(refused)?
                .to_owned(),
            args: prepared.command.args.clone(),
            env: prepared.command.env.clone(),
        },
        deadline,
    )?;
    alive(child, &cancel, deadline)?;
    drop(fence);
    let dispatcher = super::open_selected::Dispatcher::new(
        stream.try_clone()?,
        root.clone(),
        prepared.clone(),
        installed_operation,
    );
    dispatcher.attach_retirement(observation)?;
    Ok((
        Registration::new(prepared, binding, launcher_endpoint, root, dispatcher)?
            .with_prepared_child(ordinary),
        proofs,
        stream,
        cleanup,
    ))
}

fn alive(child: &mut Child, cancel: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    if child.try_wait()?.is_some() {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    Ok(())
}
fn handoff(
    stream: &mut UnixStream,
    tag: u8,
    endpoint: &UnixStream,
    root: &process_binding::ExecutingPeer,
    cancel: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    if !root.current() || cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(refused());
    }
    fd_transport::send_with_fd(stream.as_fd(), &[tag], endpoint.as_fd())?;
    let mut ack = [0];
    stream.read_exact(&mut ack)?;
    if ack != [tag]
        || !root.current()
        || cancel.load(Ordering::Acquire)
        || Instant::now() >= deadline
    {
        return Err(refused());
    }
    Ok(())
}

pub(super) struct ProofCustody {
    shutdown: UnixStream,
    worker: Option<std::thread::JoinHandle<io::Result<()>>>,
    tracking: Option<super::retirement::ProofTicket>,
}
impl ProofCustody {
    pub(super) fn tracked(mut self, ticket: super::retirement::ProofTicket) -> Self {
        self.tracking = Some(ticket);
        self
    }
    fn join_cleanup(&mut self) -> io::Result<String> {
        let _ = self.shutdown.shutdown(std::net::Shutdown::Both);
        let worker = self.worker.take().ok_or(io::ErrorKind::NotFound)?;
        let outcome = worker
            .join()
            .map_err(|_| io::Error::other("proof responder panicked"))?;
        match outcome {
            Ok(()) => Ok("completed".into()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(format!("cleanup:{:?}", error.kind()))
            }
            Err(error) => Err(error),
        }
    }
}
impl Drop for ProofCustody {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let outcome = self.join_cleanup();
            if let Some(ticket) = self.tracking.take() {
                ticket.record(&outcome);
            }
        }
    }
}
fn supervise_observed(
    child: Child,
    scratch: tempfile::TempDir,
    channels: Option<(
        Vec<ProofCustody>,
        UnixStream,
        Arc<super::open_selected::Dispatcher>,
        CleanupObservation,
    )>,
    observation: Arc<super::retirement::Observation>,
) -> io::Result<()> {
    let custody = Arc::new(std::sync::Mutex::new(Some((child, scratch, channels))));
    let worker_custody = custody.clone();
    let worker = std::thread::Builder::new()
        .name("tilo-retained-root-consumer".into())
        .spawn(move || {
            let owned = if let Ok(mut custody) = worker_custody.lock() {
                custody.take()
            } else {
                std::mem::forget(worker_custody);
                return;
            };
            let Some((mut child, scratch, channels)) = owned else {
                return;
            };
            loop {
                match child.wait() {
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        // An unknown wait is not a terminal receipt; preserve this
                        // exact request ancestry rather than deleting a live role.
                        std::mem::forget((child, scratch, channels));
                        return;
                    }
                }
            }
            observation.reaped();
            let Some((mut proofs, bootstrap, dispatcher, mut cleanup)) = channels else {
                std::mem::forget(scratch);
                return;
            };
            let outcome = (|| {
                cleanup
                    .reader
                    .set_read_timeout(Some(Duration::from_secs(3)))?;
                runtime_collab::retained_retirement::read(&mut cleanup.reader, &cleanup.subject)?;
                let outcomes = proofs
                    .iter_mut()
                    .map(ProofCustody::join_cleanup)
                    .collect::<io::Result<Vec<_>>>()?;
                let deadline = Instant::now()
                    .checked_add(Duration::from_secs(10))
                    .unwrap_or_else(Instant::now);
                loop {
                    match observation.retired(outcomes.clone()) {
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        result => break result,
                    }
                }
            })();
            if outcome.is_err() {
                observation.unknown();
                std::mem::forget((scratch, proofs, bootstrap, dispatcher, cleanup));
                return;
            }
            drop((proofs, bootstrap, dispatcher, cleanup));
            drop(scratch);
        });
    if let Err(error) = worker {
        // A failed supervising-thread spawn must not drop a privileged child
        // and remove its request directory while it may still be executing.
        std::mem::forget(custody);
        return Err(error);
    }
    Ok(())
}

fn supervise(
    child: Child,
    scratch: tempfile::TempDir,
    channels: Option<(
        Vec<ProofCustody>,
        UnixStream,
        Arc<super::open_selected::Dispatcher>,
    )>,
) -> io::Result<()> {
    let custody = Arc::new(std::sync::Mutex::new(Some((child, scratch, channels))));
    let worker_custody = custody.clone();
    let worker = std::thread::Builder::new()
        .name("tilo-retained-root-consumer".into())
        .spawn(move || {
            let owned = if let Ok(mut custody) = worker_custody.lock() {
                custody.take()
            } else {
                std::mem::forget(worker_custody);
                return;
            };
            let Some((mut child, scratch, channels)) = owned else {
                return;
            };
            loop {
                match child.wait() {
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        // An unknown wait is not a terminal receipt; preserve this
                        // exact request ancestry rather than deleting a live role.
                        std::mem::forget((child, scratch, channels));
                        return;
                    }
                }
            }
            drop(channels);
            drop(scratch);
        });
    if let Err(error) = worker {
        // A failed supervising-thread spawn must not drop a privileged child
        // and remove its request directory while it may still be executing.
        std::mem::forget(custody);
        return Err(error);
    }
    Ok(())
}

/// Cancellation/absolute deadline interrupts the real socket, including QPE's
/// blocking reads, rather than waiting for the next application-level frame.
pub(super) struct IoFence {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl IoFence {
    pub(super) fn new(
        stream: UnixStream,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let finished = stop.clone();
        let worker = std::thread::Builder::new()
            .name("tilo-retained-bootstrap-fence".into())
            .spawn(move || {
                while !finished.load(Ordering::Acquire) {
                    if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for IoFence {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct DirectoryEdge {
    file: File,
    name: Option<std::ffi::OsString>,
    identity: std::fs::Metadata,
}
/// A fresh directory only its owner may enter, for the launch request and
/// its socket. A temporary directory otherwise takes the process umask
/// (commonly 0755), which the request rendezvous rightly refuses as public.
fn private_scratch_in(parent: &Path) -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("tilo-retained-launch-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
}

struct PrivateDirectory {
    root: DirectoryEdge,
    edges: Vec<DirectoryEdge>,
}
impl PrivateDirectory {
    fn open(path: &Path) -> io::Result<Arc<Self>> {
        if !path.is_absolute() {
            return Err(refused());
        }
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        let root = DirectoryEdge {
            identity: root.metadata()?,
            file: root,
            name: None,
        };
        let mut edges = Vec::<DirectoryEdge>::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let parent = edges.last().unwrap_or(&root);
                    let named = PathBuf::from(format!("/proc/self/fd/{}", parent.file.as_raw_fd()))
                        .join(name);
                    let file = OpenOptions::new()
                        .read(true)
                        .custom_flags(
                            libc::O_DIRECTORY
                                | libc::O_NOFOLLOW
                                | libc::O_CLOEXEC
                                | libc::O_NONBLOCK,
                        )
                        .open(named)?;
                    edges.push(DirectoryEdge {
                        identity: file.metadata()?,
                        file,
                        name: Some(name.into()),
                    });
                }
                _ => return Err(refused()),
            }
        }
        let pin = Arc::new(Self { root, edges });
        pin.validate()?;
        Ok(pin)
    }
}
impl PrivateDirectory {
    fn ancestry(&self) -> impl Iterator<Item = &DirectoryEdge> {
        std::iter::once(&self.root).chain(&self.edges)
    }
}
impl DirectoryPin for PrivateDirectory {
    fn file(&self) -> &File {
        self.edges.last().map_or(&self.root.file, |edge| &edge.file)
    }
    fn readable_file(&self) -> io::Result<File> {
        self.file().try_clone()
    }
    fn validate(&self) -> io::Result<()> {
        let mut parent = &self.root;
        for child in &self.edges {
            let name = child.name.as_ref().ok_or_else(refused)?;
            let actual = std::fs::symlink_metadata(
                PathBuf::from(format!("/proc/self/fd/{}", parent.file.as_raw_fd())).join(name),
            )?;
            let held = child.file.metadata()?;
            if !actual.is_dir()
                || (actual.dev(), actual.ino()) != (child.identity.dev(), child.identity.ino())
                || (held.dev(), held.ino()) != (child.identity.dev(), child.identity.ino())
            {
                return Err(refused());
            }
            parent = child;
        }
        Ok(())
    }
    fn validate_destination(&self) -> io::Result<()> {
        self.validate()?;
        let caller = rustix::process::geteuid().as_raw();
        for edge in self.ancestry() {
            let facts = edge.file.metadata()?;
            let sticky_root = facts.uid() == 0 && facts.mode() & libc::S_ISVTX != 0;
            if ![0, caller].contains(&facts.uid()) || (facts.mode() & 0o022 != 0 && !sticky_root) {
                return Err(refused());
            }
        }
        let facts = self.file().metadata()?;
        if facts.uid() != caller || facts.mode() & 0o077 != 0 {
            return Err(refused());
        }
        Ok(())
    }
}

struct ProtectedProgram {
    parent: Arc<PrivateDirectory>,
    name: std::ffi::OsString,
    file: File,
}
impl ProtectedProgram {
    fn open(path: &Path) -> io::Result<Self> {
        let parent = PrivateDirectory::open(path.parent().ok_or_else(refused)?)?;
        for edge in parent.ancestry() {
            let facts = edge.file.metadata()?;
            if facts.uid() != 0 || facts.mode() & 0o022 != 0 {
                return Err(refused());
            }
        }
        let name = path.file_name().ok_or_else(refused)?.to_owned();
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(
                PathBuf::from(format!("/proc/self/fd/{}", parent.file().as_raw_fd())).join(&name),
            )?;
        let facts = file.metadata()?;
        let mut header = [0; 4];
        if !facts.is_file()
            || facts.uid() != 0
            || facts.mode() & 0o022 != 0
            || facts.mode() & 0o111 == 0
        {
            return Err(refused());
        }
        file.read_exact(&mut header)?;
        if header != *b"\x7fELF" {
            return Err(refused());
        }
        let program = Self { parent, name, file };
        program.check()?;
        Ok(program)
    }
    fn check_current_image(&self) -> io::Result<()> {
        self.check()?;
        let current = std::fs::metadata("/proc/self/exe")?;
        let pinned = self.file.metadata()?;
        if (current.dev(), current.ino()) != (pinned.dev(), pinned.ino()) {
            return Err(refused());
        }
        Ok(())
    }
    fn check(&self) -> io::Result<()> {
        self.parent.validate()?;
        for edge in self.parent.ancestry() {
            let facts = edge.file.metadata()?;
            if facts.uid() != 0 || facts.mode() & 0o022 != 0 {
                return Err(refused());
            }
        }
        let actual = std::fs::symlink_metadata(
            PathBuf::from(format!("/proc/self/fd/{}", self.parent.file().as_raw_fd()))
                .join(&self.name),
        )?;
        let held = self.file.metadata()?;
        if !actual.is_file()
            || actual.uid() != 0
            || actual.mode() & 0o022 != 0
            || actual.mode() & 0o111 == 0
            || (actual.dev(), actual.ino(), actual.len()) != (held.dev(), held.ino(), held.len())
        {
            return Err(refused());
        }
        Ok(())
    }
}

/// Only fixed protected execution plus actual owned-child QPE can construct
/// this role custody. Endpoint descriptors and serialized labels cannot.
pub(super) struct AdmittedRootHelper {
    peer: process_binding::ExecutingPeer,
    start: u64,
    shell: ProtectedProgram,
    helper: ProtectedProgram,
    pkexec: ProtectedProgram,
    pub(super) cancel: Arc<AtomicBool>,
}
impl AdmittedRootHelper {
    pub(super) fn executing(&self) -> &process_binding::ExecutingPeer {
        &self.peer
    }
    pub(super) fn release(
        &self,
        gate: &UnixStream,
        launcher_pid: u32,
    ) -> io::Result<Arc<process_binding::ExecutingPeer>> {
        self.check()?;
        let mut gate = gate.try_clone()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        gate.set_read_timeout(Some(Duration::from_secs(10)))?;
        gate.set_write_timeout(Some(Duration::from_secs(10)))?;
        let _fence = IoFence::new(gate.try_clone()?, self.cancel.clone(), deadline)?;
        process_binding::complete(&mut gate)?;
        process_binding::arm(&gate)?;
        let mut challenge = [0; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        gate.write_all(&process_binding::MAGIC)?;
        gate.write_all(&challenge)?;
        let launcher =
            process_binding::receive(&gate, &challenge, rustix::process::geteuid().as_raw())?;
        if launcher.pid() != launcher_pid || !launcher.current() {
            return Err(refused());
        }
        gate.write_all(&[1])?;
        self.check()?;
        let facts = self.helper.file.metadata()?;
        let expectation = serde_json::to_vec(&serde_json::json!({
            "pid": self.peer.pid(), "start": self.start,
            "helper_dev": facts.dev(), "helper_ino": facts.ino()
        }))
        .map_err(io::Error::other)?;
        wire_frame::Framing::big_endian(4096)
            .reject_empty()
            .write(&mut gate, &expectation)
            .map_err(io::Error::other)?;
        fd_transport::send_with_fd(gate.as_fd(), &[0], self.peer.pidfd())?;
        let mut ack = [0];
        gate.read_exact(&mut ack)?;
        if ack != [1] || !launcher.current() {
            return Err(refused());
        }
        self.check()?;
        Ok(Arc::new(launcher))
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) || !self.peer.belongs_to(self.peer.pid(), self.start)
        {
            return Err(refused());
        }
        self.shell.check_current_image()?;
        self.helper.check()?;
        self.pkexec.check()
    }
}

pub(super) fn start_selected_proof(
    stream: UnixStream,
    serve: impl FnOnce(UnixStream) -> io::Result<()> + Send + 'static,
) -> io::Result<ProofCustody> {
    let shutdown = stream.try_clone()?;
    let worker = std::thread::Builder::new()
        .name("tilo-retained-selected-proof".into())
        .spawn(move || serve(stream))?;
    Ok(ProofCustody {
        shutdown,
        worker: Some(worker),
        tracking: None,
    })
}
impl AdmittedRootHelper {
    pub(super) fn pid(&self) -> u32 {
        self.peer.pid()
    }
}

/// Private recipient minted only on this Shell's existing owned-helper lane.
/// The helper independently admitted the protected daemon; direct fresh QPE
/// must additionally match the exact transferred kernel pidfd and start.
pub(crate) struct DelegatedDaemonRecipient {
    root: Arc<AdmittedRootHelper>,
    pidfd: std::os::fd::OwnedFd,
    pid: u32,
    start: u64,
    program: ProtectedProgram,
}
impl crate::shell::collab_broker::RetainedProofRecipient for DelegatedDaemonRecipient {
    fn check(
        &self,
        peer: &process_binding::ExecutingPeer,
        binding: &runtime_collab::retained_person::RetainedLaunchBinding,
    ) -> io::Result<()> {
        self.root.check()?;
        self.program.check()?;
        if binding.root_pid != self.root.pid()
            || peer.uid() != 0
            || peer.pid() != self.pid
            || peer.start_time()? != self.start
            || !peer.current()
        {
            return Err(refused());
        }
        peer.matches_transferred_pidfd(std::os::fd::AsFd::as_fd(&self.pidfd))?;
        self.root.check()
    }
}
/// Invoked while the exact existing Dispatcher transaction owns the stream.
/// Refusal remains per-editor and cannot be confused with descriptor admission.
pub(super) fn receive_daemon_delegation(
    stream: &mut UnixStream,
    root: Arc<AdmittedRootHelper>,
    request: &runtime_fs::external_history::daemon_control::EnrollmentRequest,
) -> io::Result<
    Option<(
        Arc<DelegatedDaemonRecipient>,
        runtime_fs::external_history::EnrollmentContext,
    )>,
> {
    receive_daemon_delegation_control(
        stream,
        root,
        request,
        &runtime_fs::external_history::daemon_control::EnrollmentControl::EnrollHistoryDaemon(
            request.clone(),
        ),
    )
}
fn receive_history_read_daemon_delegation(
    stream: &mut UnixStream,
    root: Arc<AdmittedRootHelper>,
    request: &runtime_fs::external_history::daemon_control::EnrollmentRequest,
) -> io::Result<
    Option<(
        Arc<DelegatedDaemonRecipient>,
        runtime_fs::external_history::EnrollmentContext,
    )>,
> {
    receive_daemon_delegation_control(
        stream,
        root,
        request,
        &runtime_fs::external_history::daemon_control::EnrollmentControl::EnrollHistoryReadDaemon(
            request.clone(),
        ),
    )
}
/// Separate fresh Reader enrollment for the actual selected editor. Existing
/// Files Read and editor replacement delegation remain distinct control kinds.
pub(super) fn receive_editor_read_delegation(
    stream: &mut UnixStream,
    root: Arc<AdmittedRootHelper>,
    request: &runtime_fs::external_history::daemon_control::EnrollmentRequest,
) -> io::Result<
    Option<(
        Arc<DelegatedDaemonRecipient>,
        runtime_fs::external_history::EnrollmentContext,
    )>,
> {
    use runtime_fs::external_history::daemon_control::EnrollmentControl;
    receive_daemon_delegation_control(
        stream,
        root,
        request,
        &EnrollmentControl::EnrollSelectedEditorHistoryReadDaemon(request.clone()),
    )
}
fn receive_daemon_delegation_control(
    stream: &mut UnixStream,
    root: Arc<AdmittedRootHelper>,
    request: &runtime_fs::external_history::daemon_control::EnrollmentRequest,
    control: &runtime_fs::external_history::daemon_control::EnrollmentControl,
) -> io::Result<
    Option<(
        Arc<DelegatedDaemonRecipient>,
        runtime_fs::external_history::EnrollmentContext,
    )>,
> {
    use runtime_fs::external_history::daemon_control::{EnrollmentReply, MAX_CONTROL_BYTES};
    root.check()?;
    let framing = wire_frame::Framing::little_endian(MAX_CONTROL_BYTES).reject_empty();
    framing
        .write(stream, &control.encode()?)
        .map_err(io::Error::other)?;
    let bytes = framing
        .read(stream)
        .map_err(io::Error::other)?
        .ok_or_else(refused)?;
    let delegation = match EnrollmentReply::decode(&bytes, request)? {
        EnrollmentReply::Refused { .. } => return Ok(None),
        EnrollmentReply::Delegated { delegation } => delegation,
    };
    root.check()?;
    let program = ProtectedProgram::open(Path::new("/usr/bin/fs"))?;
    let facts = program.file.metadata()?;
    if [facts.dev(), facts.ino()] != delegation.program_identity
        || delegation.daemon_pid == root.pid()
    {
        return Err(refused());
    }
    let (tag, fd) = fd_transport::recv_with_fd(stream.as_fd())?;
    if tag != [61] {
        return Err(refused());
    }
    let pidfd = fd.ok_or_else(refused)?;
    root.check()?;
    stream.write_all(&[61])?;
    Ok(Some((
        Arc::new(DelegatedDaemonRecipient {
            root,
            pidfd,
            pid: delegation.daemon_pid,
            start: delegation.daemon_start,
            program,
        }),
        delegation.context,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_request_uses_original_atomic_publication_and_refuses_replacement() {
        let scratch = private_scratch_in(&std::env::temp_dir()).expect("private request parent");
        let pin = PrivateDirectory::open(scratch.path()).expect("held real ancestry");
        let (channel, listener) =
            request_rendezvous::Rendezvous::create(scratch.path(), pin.clone())
                .expect("actual existing request channel");
        let wire = serde_json::to_value(&channel).expect("existing wire");
        assert_eq!(wire["caller_pid"], std::process::id());
        assert_eq!(wire["path"]["platform"], "Unix");
        request_rendezvous::publish_request_bytes(pin.as_ref(), b"original", 8)
            .expect("canonical owner-only publication");
        assert!(request_rendezvous::publish_request_bytes(pin.as_ref(), b"changed", 8).is_err());
        assert_eq!(
            request_rendezvous::read_request_bytes(
                pin.as_ref(),
                rustix::process::geteuid().as_raw(),
                8
            )
            .expect("bounded held read"),
            b"original"
        );
        assert!(
            request_rendezvous::read_request_bytes(
                pin.as_ref(),
                rustix::process::geteuid().as_raw(),
                7
            )
            .is_err()
        );
        std::fs::set_permissions(
            scratch.path().join("request.json"),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("change actual request privacy");
        assert!(
            request_rendezvous::read_request_bytes(
                pin.as_ref(),
                rustix::process::geteuid().as_raw(),
                8
            )
            .is_err()
        );
        let relocated = scratch.path().with_extension("relocated");
        std::fs::rename(scratch.path(), &relocated).expect("replace original name");
        std::fs::create_dir(scratch.path()).expect("new directory at original path");
        assert!(listener.validate().is_err());
        assert!(
            channel
                .connect(
                    scratch.path(),
                    pin.as_ref(),
                    rustix::process::geteuid().as_raw()
                )
                .is_err()
        );
        std::fs::remove_dir_all(relocated).expect("remove relocated request");
    }

    #[test]
    fn cancellation_and_absolute_deadline_interrupt_actual_blocking_socket() {
        for cancelled in [false, true] {
            let (stream, mut reader) = UnixStream::pair().expect("real private socket");
            reader
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("test bound");
            let cancel = Arc::new(AtomicBool::new(cancelled));
            let fence = IoFence::new(
                stream,
                cancel,
                Instant::now()
                    .checked_add(Duration::from_millis(30))
                    .unwrap_or_else(Instant::now),
            )
            .expect("actual cancellation worker");
            let mut byte = [0];
            assert_eq!(
                reader
                    .read(&mut byte)
                    .expect("interrupted by socket shutdown"),
                0
            );
            drop(fence);
        }
    }

    #[test]
    fn different_protected_installed_image_cannot_claim_executing_shell_role() {
        let program = ProtectedProgram::open(Path::new("/usr/bin/true"))
            .expect("real protected installed ELF");
        assert!(program.check_current_image().is_err());
        let scratch = tempfile::tempdir().expect("untrusted executable directory");
        // Writable by anyone, so untrusted whoever runs the test, root too.
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o777))
            .expect("untrusted mode");
        let forged = scratch.path().join("shell");
        std::fs::write(&forged, b"\x7fELFfake").expect("wrong executable");
        std::fs::set_permissions(&forged, std::fs::Permissions::from_mode(0o755))
            .expect("executable mode");
        assert!(ProtectedProgram::open(&forged).is_err());
    }
}

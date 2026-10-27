//! A short-lived target-owner importer, endorsed on the existing owner channel.
use super::super::CONTEXT_DOMAIN;
use super::{
    AdmittedRootHelper, Arc, AsRawFd, AtomicBool, BOOTSTRAP_TIME, Child, Command, Digest, Duration,
    HELPER, Instant, IoFence, MetadataExt, PKEXEC, Path, PreparedRetainedHome, PrivateDirectory,
    ProofCustody, ProtectedProgram, REQUEST_BOUND, Read, SHELL, Stdio, UnixStream, Write, alive,
    fd_transport, handoff, io, process_binding, refused, request_rendezvous, supervise,
};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;

pub(super) struct CopyImporterCustody {
    child: Option<Child>,
    root: Arc<AdmittedRootHelper>,
    peer: Arc<process_binding::ExecutingPeer>,
}
impl CopyImporterCustody {
    /// Kernel exit is retirement evidence, never a migration completion receipt.
    pub(super) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.as_mut().ok_or_else(refused)?.try_wait()?;
        // An observed exit is authoritative retirement. While the child is
        // live, retain and recheck the exact QPE-attested execution custody.
        if status.is_none() && !self.peer.current() {
            return Err(refused());
        }
        Ok(status)
    }
}
impl Drop for CopyImporterCustody {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            retain_importer_until_exit(child, self.root.clone());
        }
    }
}
fn retain_importer_until_exit(child: Child, root: Arc<AdmittedRootHelper>) {
    let custody = Arc::new(std::sync::Mutex::new(Some((child, root))));
    let worker_custody = custody.clone();
    if std::thread::Builder::new()
        .name("tilo-installed-copy-importer".into())
        .spawn(move || {
            let owned = if let Ok(mut held) = worker_custody.lock() {
                held.take()
            } else {
                std::mem::forget(worker_custody);
                return;
            };
            let Some((mut child, root)) = owned else {
                return;
            };
            loop {
                match child.wait() {
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        std::mem::forget((child, root));
                        return;
                    }
                }
            }
        })
        .is_err()
    {
        // The actual child may still be mutating private target custody. A
        // supervisor allocation failure cannot become a fabricated stopped ACK.
        std::mem::forget(custody);
    }
}
impl AdmittedRootHelper {
    /// Order on the already QPE-authenticated Shell owner channel:
    /// big-endian4096 recipient observation, pidfd83+ACK1, endpoint84+ACK84.
    /// The root admits this precise child before sending any archive.
    pub(super) fn spawn_installed_copy_importer(
        self: &Arc<Self>,
        owner_channel: &mut UnixStream,
        deadline: Instant,
    ) -> io::Result<CopyImporterCustody> {
        self.check()?;
        let (shell_gate, child_gate) = UnixStream::pair()?;
        let (root_endpoint, child_root) = UnixStream::pair()?;
        let gate_fd = child_gate.as_raw_fd();
        let root_fd = child_root.as_raw_fd();
        if gate_fd == root_fd || gate_fd < 3 || root_fd < 3 || gate_fd > 4095 || root_fd > 4095 {
            return Err(refused());
        }
        let mut command = Command::new(HELPER);
        command
            .arg("--installed-original-copy-receiver")
            .arg(gate_fd.to_string())
            .arg(root_fd.to_string())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: after fork this closure performs only fcntl on the two live,
        // owned socket descriptors. The parent copies remain close-on-exec.
        unsafe {
            command.pre_exec(move || {
                for fd in [gate_fd, root_fd] {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        self.helper.check()?;
        let mut child = command.spawn()?;
        drop(child_gate);
        drop(child_root);
        let result = (|| {
            alive(&mut child, &self.cancel, deadline)?;
            let peer = self.release(&shell_gate, child.id())?;
            let expected = self.helper.file.metadata()?;
            let actual = std::fs::metadata(format!("/proc/{}/exe", peer.pid()))?;
            if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) || !peer.current() {
                return Err(refused());
            }
            let observation = launcher::retained_view::installed_copy::CopyRecipientObservation {
                pid: peer.pid(),
                start: peer.start_time()?,
                helper_dev: expected.dev(),
                helper_ino: expected.ino(),
            };
            self.check()?;
            owner_channel.set_read_timeout(Some(Duration::from_secs(10)))?;
            owner_channel.set_write_timeout(Some(Duration::from_secs(10)))?;
            let _fence = IoFence::new(owner_channel.try_clone()?, self.cancel.clone(), deadline)?;
            wire_frame::Framing::big_endian(4096)
                .reject_empty()
                .write(
                    owner_channel,
                    &serde_json::to_vec(&observation).map_err(io::Error::other)?,
                )
                .map_err(io::Error::other)?;
            fd_transport::send_with_fd(owner_channel.as_fd(), &[83], peer.pidfd())?;
            let mut ack = [0];
            owner_channel.read_exact(&mut ack)?;
            if ack != [1] || !peer.current() {
                return Err(refused());
            }
            handoff(
                owner_channel,
                84,
                &root_endpoint,
                self.executing(),
                &self.cancel,
                deadline,
            )?;
            self.check()?;
            alive(&mut child, &self.cancel, deadline)?;
            Ok(peer)
        })();
        // This gate is handshake-only. The importer retains the genuine root
        // pidfd and authenticates its root endpoint separately before archives.
        let _ = shell_gate.shutdown(std::net::Shutdown::Both);
        drop(root_endpoint);
        match result {
            Ok(peer) => Ok(CopyImporterCustody {
                child: Some(child),
                root: self.clone(),
                peer,
            }),
            Err(error) => {
                retain_importer_until_exit(child, self.clone());
                Err(error)
            }
        }
    }
}

/// Actual signed Imager selection and native personal session supplied by the
/// existing Shell preparation. Nothing starts on ordinary Files launch.
#[allow(
    clippy::too_many_lines,
    reason = "one ordered privileged bootstrap: each step checks the custody the step before established, and a failure supervises the child it started"
)]
pub(crate) fn establish_original_copy(
    prepared: &Arc<PreparedRetainedHome>,
    installed_operation: &str,
    cancel: &Arc<AtomicBool>,
) -> io::Result<OriginalCopyBootstrapCustody> {
    let operation = uuid::Uuid::parse_str(installed_operation).map_err(io::Error::other)?;
    if operation.is_nil()
        || operation.to_string() != installed_operation
        || prepared.package.id != "so.tilo.imager"
    {
        return Err(refused());
    }
    prepared.check(&prepared.command)?;
    let context = copy_context(prepared, installed_operation)?;
    let shell = ProtectedProgram::open(Path::new(SHELL))?;
    shell.check_current_image()?;
    let helper = ProtectedProgram::open(Path::new(HELPER))?;
    let pkexec = ProtectedProgram::open(Path::new(PKEXEC))?;
    let scratch = tempfile::Builder::new()
        .prefix("tilo-original-copy-launch-")
        .tempdir_in(super::super::super::runtime_dir())?;
    let pin = PrivateDirectory::open(scratch.path())?;
    let (channel, listener) = request_rendezvous::Rendezvous::create(scratch.path(), pin.clone())?;
    let resource = format!("original-copy:{installed_operation}");
    let request = serde_json::to_vec(&serde_json::json!({
        "schema":1,"channel":channel,"launch":{"schema":1,"operation_id":installed_operation,
        "app_id":prepared.package.id,"package_sha256":prepared.package.package_id_hex,
        "room":prepared.room.to_string(),"resource_record":resource,"context_sha256":hex::encode(sha2::Sha256::digest(&context))}
    })).map_err(io::Error::other)?;
    let publication =
        request_rendezvous::publish_request_bytes(pin.as_ref(), &request, REQUEST_BOUND)?;
    helper.check()?;
    pkexec.check()?;
    let mut child = Command::new(PKEXEC)
        .arg(HELPER)
        .arg("--installed-original-copy-controller")
        .arg(scratch.path().join("request.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now()
        .checked_add(BOOTSTRAP_TIME)
        .unwrap_or_else(Instant::now);
    let result = (|| {
        listener.socket().set_nonblocking(true)?;
        let mut bootstrap = loop {
            alive(&mut child, cancel, deadline)?;
            listener.validate()?;
            match listener.socket().accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        };
        bootstrap.set_read_timeout(Some(Duration::from_secs(10)))?;
        bootstrap.set_write_timeout(Some(Duration::from_secs(10)))?;
        let _fence = IoFence::new(bootstrap.try_clone()?, cancel.clone(), deadline)?;
        process_binding::complete(&mut bootstrap)?;
        alive(&mut child, cancel, deadline)?;
        process_binding::arm(&bootstrap)?;
        let nonce = nonce();
        bootstrap.write_all(&process_binding::MAGIC)?;
        bootstrap.write_all(&nonce)?;
        let peer = process_binding::receive(&bootstrap, &nonce, 0)?;
        if peer.pid() != child.id() || !peer.current() {
            return Err(refused());
        }
        helper.check()?;
        pkexec.check()?;
        bootstrap.write_all(&[1])?;
        let mut binding = prepared.binding(installed_operation, peer.pid())?;
        binding.resource_record.clone_from(&resource);
        binding.context_sha256 = hex::encode(sha2::Sha256::digest(&context));
        binding.validate()?;
        let (proof_stream, proof_root) = UnixStream::pair()?;
        let proof_shutdown = proof_stream.try_clone()?;
        let proof_prepared = prepared.clone();
        let proof_binding = binding;
        let proof_session = prepared.session.clone();
        let proof_worker = std::thread::Builder::new()
            .name("tilo-original-copy-person-proof".into())
            .spawn(move || {
                proof_session.serve_retained_launch_proof_checked(
                    proof_stream,
                    &proof_binding,
                    move || proof_prepared.check(&proof_prepared.command),
                )
            })?;
        let proof = ProofCustody {
            shutdown: proof_shutdown,
            worker: Some(proof_worker),
            tracking: None,
        };
        handoff(&mut bootstrap, 71, &proof_root, &peer, cancel, deadline)?;
        let (mut owner_channel, root_owner) = UnixStream::pair()?;
        handoff(&mut bootstrap, 72, &root_owner, &peer, cancel, deadline)?;
        wire_frame::Framing::little_endian(64 * 1024)
            .reject_empty()
            .write(&mut bootstrap, &context)
            .map_err(io::Error::other)?;
        let root = Arc::new(AdmittedRootHelper {
            start: peer.start_time()?,
            peer,
            shell,
            helper,
            pkexec,
            cancel: cancel.clone(),
        });
        authenticate_owner_channel(&mut owner_channel, &root, deadline)?;
        let importer = root.spawn_installed_copy_importer(&mut owner_channel, deadline)?;
        root.check()?;
        prepared.check(&prepared.command)?;
        alive(&mut child, cancel, deadline)?;
        Ok((root, importer, proof, owner_channel, bootstrap))
    })();
    drop(publication);
    drop(listener);
    match result {
        Ok((root, importer, proof, owner_channel, bootstrap)) => Ok(OriginalCopyBootstrapCustody {
            held: Some((
                child,
                scratch,
                root,
                importer,
                proof,
                owner_channel,
                bootstrap,
            )),
        }),
        Err(error) => {
            supervise(child, scratch, None)?;
            Err(error)
        }
    }
}
fn nonce() -> [u8; 32] {
    let mut nonce = [0; 32];
    nonce[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    nonce[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    nonce
}
fn copy_context(prepared: &PreparedRetainedHome, operation: &str) -> io::Result<Vec<u8>> {
    prepared.check(&prepared.command)?;
    let mut bytes = CONTEXT_DOMAIN.to_vec();
    bytes.extend(
        serde_json::to_vec(&(
            operation,
            &prepared.package.id,
            hex::encode(prepared.publisher),
            &prepared.package.package_id_hex,
            prepared.room.to_string(),
            format!("original-copy:{operation}"),
            &prepared.command.program,
            &prepared.command.args,
            &prepared.command.env,
        ))
        .map_err(io::Error::other)?,
    );
    if bytes.len() > 64 * 1024 {
        return Err(refused());
    }
    Ok(bytes)
}
fn authenticate_owner_channel(
    stream: &mut UnixStream,
    root: &Arc<AdmittedRootHelper>,
    deadline: Instant,
) -> io::Result<()> {
    root.check()?;
    let _fence = IoFence::new(stream.try_clone()?, root.cancel.clone(), deadline)?;
    process_binding::complete(stream)?;
    process_binding::arm(stream)?;
    let nonce = nonce();
    stream.write_all(&process_binding::MAGIC)?;
    stream.write_all(&nonce)?;
    let peer = process_binding::receive(stream, &nonce, 0)?;
    if peer.pid() != root.peer.pid() || peer.start_time()? != root.start {
        return Err(refused());
    }
    peer.matches_transferred_pidfd(root.peer.pidfd())?;
    stream.write_all(&[1])?;
    root.check()
}
type CopyBootstrap = (
    Child,
    tempfile::TempDir,
    Arc<AdmittedRootHelper>,
    CopyImporterCustody,
    ProofCustody,
    UnixStream,
    UnixStream,
);
pub(crate) struct OriginalCopyBootstrapCustody {
    held: Option<CopyBootstrap>,
}
impl OriginalCopyBootstrapCustody {
    pub(crate) fn send_owner_command(
        &mut self,
        command: &launcher::retained_view::installed_copy::CopyOwnerCommand,
    ) -> io::Result<()> {
        let (_, _, root, _, _, owner, _) = self.held.as_mut().ok_or_else(refused)?;
        root.check()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let _fence = IoFence::new(owner.try_clone()?, root.cancel.clone(), deadline)?;
        wire_frame::Framing::big_endian(16 * 1024)
            .reject_empty()
            .write(
                owner,
                &serde_json::to_vec(command).map_err(io::Error::other)?,
            )
            .map_err(io::Error::other)?;
        if matches!(
            command,
            launcher::retained_view::installed_copy::CopyOwnerCommand::AcknowledgeComplete { .. }
                | launcher::retained_view::installed_copy::CopyOwnerCommand::AcknowledgeCleanup { .. }
        ) {
            // Root may now retire. The authenticated Complete was already read
            // while it was live; this write acknowledges receipt, not new work.
            Ok(())
        } else {
            root.check()
        }
    }
    /// Idle polling consumes no frame bytes. Any incomplete frame is fatal;
    /// dropping custody shuts down the same channel and retains both children.
    pub(crate) fn receive_owner_event(
        &mut self,
    ) -> io::Result<Option<launcher::retained_view::installed_copy::CopyOwnerEvent>> {
        let (_, _, root, _, _, owner, _) = self.held.as_mut().ok_or_else(refused)?;
        root.check()?;
        let mut poll = libc::pollfd {
            fd: owner.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points at one initialized descriptor for this call.
        let count = unsafe { libc::poll(&raw mut poll, 1, 100) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        root.check()?;
        if count == 0 {
            return Ok(None);
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let _fence = IoFence::new(owner.try_clone()?, root.cancel.clone(), deadline)?;
        let bytes = wire_frame::Framing::big_endian(16 * 1024)
            .reject_empty()
            .read(owner)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        let event = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        root.check()?;
        Ok(Some(event))
    }
    /// Called only after the genuine matching `CleanupFinished` owner receipt.
    /// Both owned children and the fresh certified responder must actually end.
    pub(crate) fn try_observe_cleanup(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let (root_child, _, _, importer, proof, _, _) = self.held.as_mut().ok_or_else(refused)?;
        let Some(importer_status) = importer.try_wait()? else {
            return Ok(None);
        };
        if !importer_status.success() {
            return Err(io::Error::other("Copy importer retirement failed"));
        }
        let Some(root_status) = root_child.try_wait()? else {
            return Ok(None);
        };
        if !root_status.success() {
            return Err(io::Error::other("Copy source retirement failed"));
        }
        proof.join_cleanup()?;
        Ok(Some(root_status))
    }
    /// Returns actual process exits only, never an adoption success receipt.
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let (root_child, _, _, importer, _, _, _) = self.held.as_mut().ok_or_else(refused)?;
        if importer.try_wait()?.is_none() {
            return Ok(None);
        }
        root_child.try_wait()
    }
}
impl Drop for OriginalCopyBootstrapCustody {
    fn drop(&mut self) {
        let Some(held) = self.held.take() else {
            return;
        };
        let custody = Arc::new(std::sync::Mutex::new(Some(held)));
        let worker_custody = custody.clone();
        if std::thread::Builder::new()
            .name("tilo-original-copy-bootstrap".into())
            .spawn(move || {
                let held = if let Ok(mut held) = worker_custody.lock() {
                    held.take()
                } else {
                    std::mem::forget(worker_custody);
                    return;
                };
                let Some((mut child, scratch, root, importer, proof, owner, bootstrap)) = held
                else {
                    return;
                };
                let _ = owner.shutdown(std::net::Shutdown::Both);
                let _ = bootstrap.shutdown(std::net::Shutdown::Both);
                drop(proof);
                loop {
                    match child.wait() {
                        Ok(_) => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => {
                            std::mem::forget((child, scratch, root, importer, owner, bootstrap));
                            return;
                        }
                    }
                }
                drop(importer);
                drop(scratch);
            })
            .is_err()
        {
            std::mem::forget(custody);
        }
    }
}

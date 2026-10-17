//! Installed Copy on the existing private Shell/root request and person gate.
//! Source/output work and importer retirement remain owned until observed stop.
use super::*;
use crate::setup::{
    home_capture::{InstalledCopyTransaction, InstalledVerifiedArtifacts},
    installed_copy::{CopyCommand, CopyEvent},
    installed_retained_actor::ReviewedInstalledOriginalCopy,
};
use launcher::retained_view::installed_copy::{
    CopyOwnerCommand, CopyOwnerEvent, InstalledCopyIssue,
};
use std::{os::fd::AsRawFd, sync::mpsc};

pub(super) fn run(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    // Before bootstrap watcher threads or any source mount; affects only this
    // fixed root helper, never the source host/Shell propagation state.
    let namespace = namespace::ManagedConsumerNamespace::isolate()?;
    let result = run_current(arguments, &namespace);
    while ReviewedInstalledOriginalCopy::retirement_pending()
        || crate::setup::home_capture::installed_copy_retirement_pending()
        || InstalledCopyIssue::retirement_pending()
    {
        std::thread::park_timeout(Duration::from_millis(50));
    }
    result
}
fn copy_context(
    bytes: &[u8],
    binding: &runtime_collab::retained_person::RetainedLaunchBinding,
) -> io::Result<()> {
    if bytes.is_empty()
        || bytes.len() > 64 * 1024
        || !bytes.starts_with(CONTEXT_DOMAIN)
        || hex::encode(Sha256::digest(bytes)) != binding.context_sha256
    {
        return Err(refused());
    }
    let (operation, app, publisher, package, room, resource, program, args, env): Context =
        serde_json::from_slice(&bytes[CONTEXT_DOMAIN.len()..]).map_err(io::Error::other)?;
    if operation != binding.operation_id
        || app != "so.tilo.imager"
        || app != binding.app_id
        || package != binding.package_sha256
        || room != binding.room
        || resource != format!("original-copy:{operation}")
        || resource != binding.resource_record
        || publisher.len() != 64
        || hex::decode(&publisher).map_err(io::Error::other)?.len() != 32
        || program.is_empty()
        || args.len() > 256
        || env.len() > 256
    {
        return Err(refused());
    }
    // The command remains a certificate-context binding only. This mode never
    // executes a program, environment or argument selected by this tuple.
    Ok(())
}
fn run_current(
    arguments: impl Iterator<Item = OsString>,
    namespace: &namespace::ManagedConsumerNamespace,
) -> io::Result<()> {
    let mut arguments = arguments;
    let path = arguments.next().ok_or_else(refused)?;
    if arguments.next().is_some() {
        return Err(refused());
    }
    let path = Path::new(&path);
    if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new("request.json")) {
        return Err(refused());
    }
    let directory = path.parent().ok_or_else(refused)?;
    let pin = RequestDirectoryPin(PinnedDirectory::open_absolute(directory)?);
    let caller = pin.file().metadata()?.uid();
    if caller == 0 {
        return Err(refused());
    }
    let bytes = request_rendezvous::read_request_bytes(&pin, caller, 16 * 1024)?;
    let request: Request = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if request.schema != 1 {
        return Err(refused());
    }
    let launch = LaunchRequest::decode(&serde_json::to_vec(&request.launch)?)?;
    let mut bootstrap = request.channel.connect(directory, &pin, caller)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let cancellation = ServiceCancellation::default();
    let fence = BootstrapFence::start(&bootstrap, cancel.clone(), cancellation.clone())?;
    bootstrap.set_read_timeout(Some(Duration::from_secs(10)))?;
    bootstrap.set_write_timeout(Some(Duration::from_secs(10)))?;
    process_binding::arm(&bootstrap)?;
    let mut nonce = [0; 32];
    tilo_seal::fill_random(&mut nonce).map_err(io::Error::other)?;
    bootstrap.write_all(&process_binding::MAGIC)?;
    bootstrap.write_all(&nonce)?;
    let shell = process_binding::receive(&bootstrap, &nonce, caller)?;
    if shell.pid() != request.channel.caller_pid() || !shell.current() {
        return Err(refused());
    }
    bootstrap.write_all(&[1])?;
    process_binding::complete(&mut bootstrap)?;
    pin.validate()?;
    namespace.check()?;
    let proof = receive_endpoint(&mut bootstrap, EndpointRole::PersonProof)?;
    let actor = ReviewedInstalledOriginalCopy::review(proof, launch, &cancel)?;
    let result = run_reviewed(
        &mut bootstrap,
        &actor,
        &shell,
        &cancel,
        &cancellation,
        &fence,
    );
    if result.is_err() {
        actor.revoke();
    }
    while Arc::strong_count(&actor) != 1 {
        actor.revoke();
        std::thread::park_timeout(Duration::from_millis(50));
    }
    // The transaction, signed runs, output issue and real importer must leave
    // their slots before consuming actual source/person custody.
    if actor.retire().is_err() {
        loop {
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }
    drop(fence);
    namespace.check()?;
    result
}
fn run_reviewed(
    bootstrap: &mut UnixStream,
    actor: &Arc<ReviewedInstalledOriginalCopy>,
    shell: &process_binding::ExecutingPeer,
    cancel: &Arc<AtomicBool>,
    cancellation: &ServiceCancellation,
    fence: &BootstrapFence,
) -> io::Result<()> {
    let binding = actor.launch_binding()?;
    let certified = actor.target_peer(cancel)?;
    if certified.pid() != shell.pid()
        || certified.start_time()? != shell.start_time()?
        || certified.uid() != shell.uid()
        || binding.target_pid != shell.pid()
        || binding.target_uid != shell.uid()
    {
        return Err(refused());
    }
    let endpoint = receive_endpoint(bootstrap, EndpointRole::Consumer)?;
    let context = wire_frame::Framing::little_endian(64 * 1024)
        .reject_empty()
        .read(bootstrap)
        .map_err(io::Error::other)?
        .ok_or_else(refused)?;
    copy_context(&context, &binding)?;
    let mut owner = RootConsumerChannel::authenticate(endpoint, cancellation)?;
    let recipient = owner.receive_installed_copy_recipient(&certified, cancellation)?;
    let endpoint = owner.receive_installed_copy_endpoint(cancellation)?;
    let mut receiver = RootConsumerChannel::authenticate(endpoint, cancellation)?;
    receiver.verify_installed_copy_recipient(&recipient)?;
    let importer = receiver.retain_peer();
    // This is an actual already authenticated short-lived nonroot helper;
    // issue construction later additionally matches its observed Shell child.
    fence.observed_consumer()?;
    let transport = ServiceCancellation::default();
    owner.send_installed_copy_status(
        &CopyOwnerEvent::Copying {
            operation: binding.operation_id.clone(),
        },
        &transport,
    )?;
    let tx = InstalledCopyTransaction::prepare_or_recover(actor.clone())?;
    tx.attempt()
        .mark_receiver_started(importer.clone(), cancel)?;
    let (progress_tx, progress_rx) = mpsc::channel();
    let worker_tx = tx.clone();
    let mut worker = Some(
        std::thread::Builder::new()
            .name("installed-copy-export-verifier".into())
            .spawn(move || {
                worker_tx
                    .export_and_verify(progress_tx)
                    .verified_export()?
                    .seal()
                    .map(Arc::new)
            })?,
    );
    let mut control_error = None;
    while !worker.as_ref().is_some_and(JoinHandle::is_finished) {
        while let Ok(event) = progress_rx.try_recv() {
            if owner
                .send_installed_copy_status(&event, &transport)
                .is_err()
            {
                tx.cancel();
                control_error = Some(refused());
            }
        }
        match owner.receive_installed_copy_command(Duration::from_millis(50), &transport) {
            Ok(Some(CopyOwnerCommand::Cancel {})) => {
                tx.cancel();
                cancel.store(true, Ordering::Release);
            }
            Ok(Some(
                CopyOwnerCommand::Retry {}
                | CopyOwnerCommand::AcknowledgeComplete { .. }
                | CopyOwnerCommand::ResumeCleanup { .. }
                | CopyOwnerCommand::AcknowledgeCleanup { .. },
            )) => {
                tx.cancel();
                control_error = Some(refused());
            }
            Ok(None) => {}
            Err(error) => {
                tx.cancel();
                control_error = Some(error);
            }
        }
        if cancel.load(Ordering::Acquire) || cancellation.is_cancelled() {
            tx.cancel();
        }
    }
    let result = worker
        .take()
        .ok_or_else(refused)?
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("Installed copy worker panicked")));
    let artifacts = match result {
        Ok(artifacts) if control_error.is_none() && !cancel.load(Ordering::Acquire) => artifacts,
        result => {
            drop(result);
            // Shut down exactly this importer transport; its actual gate stop
            // watcher cancels and joins any owned adoption worker before exit.
            let _ = receiver.stop_installed_copy_transport();
            drop(receiver);
            wait_importer(&importer)?;
            tx.attempt().record_receiver_cancelled(cancel)?;
            let error = control_error.unwrap_or_else(|| {
                io::Error::other("Installed copy export/verifier did not finish")
            });
            let _ = owner.send_installed_copy_status(
                &CopyOwnerEvent::Failed {
                    operation: binding.operation_id,
                    message: error.to_string(),
                },
                &transport,
            );
            return Err(error);
        }
    };
    let mut issue = match artifacts.prepare_issue(receiver, recipient, cancellation.clone()) {
        Ok(issue) => issue,
        Err(error) => {
            wait_importer(&importer)?;
            tx.attempt().record_receiver_cancelled(cancel)?;
            return Err(error);
        }
    };
    let received = drive_receive(
        &mut issue, &mut owner, &artifacts, importer, cancel, &transport,
    );
    if received.is_err() {
        let _ = issue.cancel_receiver();
    }
    while !issue.retire()? {
        std::thread::park_timeout(Duration::from_millis(50));
    }
    match received {
        Ok(received) => {
            let (_, _, home, applications) = received.evidence();
            // Actual full adoption verification came from the pinned importer,
            // and its kernel task has now stopped before releasing source IO.
            let completion = Arc::new(
                crate::setup::home_capture::InstalledCopyCompletion::observed(received, cancel)?,
            );
            if tx
                .attempt()
                .record_receiver_complete(&completion, cancel)
                .is_err()
            {
                // Publication may have reached durable storage before its error.
                // Keep the real completion; never infer permission to re-export.
                park_completed_copy(&completion);
            }
            let home = home.map(hex::encode);
            let applications = applications.map(hex::encode);
            let sent = owner.send_installed_copy_status(
                &CopyOwnerEvent::Complete {
                    operation: binding.operation_id.clone(),
                    home_cipher_sha256: home.clone(),
                    applications_cipher_sha256: applications.clone(),
                },
                &transport,
            );
            if sent.is_err() {
                park_completed_copy(&completion);
            }
            // Keep this authenticated root task alive until Shell has consumed
            // the exact successful receipt. Process death never substitutes for
            // receipt delivery, and a buffered frame cannot race root teardown.
            loop {
                match owner.receive_installed_copy_command(Duration::from_millis(50), &transport) {
                    Ok(Some(CopyOwnerCommand::AcknowledgeComplete {
                        operation,
                        home_cipher_sha256,
                        applications_cipher_sha256,
                    })) if operation == binding.operation_id
                        && home_cipher_sha256 == home
                        && applications_cipher_sha256 == applications =>
                    {
                        // Complete remains successful even if exact material retirement
                        // fails. Keep the actual root task and owned permit alive;
                        // process exit would lose uncertain native custody.
                        return finish_cleanup(
                            &completion,
                            &tx,
                            cancel,
                            &mut owner,
                            &transport,
                            &binding.operation_id,
                        );
                    }
                    Ok(Some(_)) | Err(_) => park_completed_copy(&completion),
                    Ok(None) => (),
                }
            }
        }
        Err(error) => {
            tx.attempt().record_receiver_cancelled(cancel)?;
            let _ = owner.send_installed_copy_status(
                &CopyOwnerEvent::Failed {
                    operation: binding.operation_id,
                    message: error.to_string(),
                },
                &transport,
            );
            Err(error)
        }
    }
}
/// Retry only bounded transient IO failures against the same real completion
/// and its private observed-effect permit. Refusal and cancellation stay errors;
/// the caller keeps root custody and exposes cleanup separately from Complete.
fn finish_cleanup(
    completion: &Arc<crate::setup::home_capture::InstalledCopyCompletion>,
    transaction: &Arc<crate::setup::home_capture::InstalledCopyTransaction>,
    cancel: &AtomicBool,
    owner: &mut RootConsumerChannel,
    transport: &ServiceCancellation,
    operation: &str,
) -> io::Result<()> {
    loop {
        match retire_completed_copy(completion, transaction, cancel) {
            Ok(metadata_quarantined) => {
                if owner
                    .send_installed_copy_status(
                        &CopyOwnerEvent::CleanupFinished {
                            operation: operation.into(),
                            metadata_quarantined,
                        },
                        transport,
                    )
                    .is_err()
                {
                    park_completed_copy(completion);
                }
                loop {
                    match owner.receive_installed_copy_command(Duration::from_millis(50), transport)
                    {
                        Ok(Some(CopyOwnerCommand::AcknowledgeCleanup {
                            operation: actual,
                            metadata_quarantined: observed,
                        })) if actual == operation && observed == metadata_quarantined => {
                            return Ok(());
                        }
                        Ok(None) => (),
                        _ => park_completed_copy(completion),
                    }
                }
            }
            Err(error) => {
                let resumable = !matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied
                        | io::ErrorKind::InvalidData
                        | io::ErrorKind::InvalidInput
                        | io::ErrorKind::NotFound
                        | io::ErrorKind::UnexpectedEof
                );
                if owner
                    .send_installed_copy_status(
                        &CopyOwnerEvent::CleanupPending {
                            operation: operation.into(),
                            message: "Temporary files are still held.".into(),
                            resumable,
                        },
                        transport,
                    )
                    .is_err()
                    || !resumable
                {
                    park_completed_copy(completion);
                }
                loop {
                    match owner.receive_installed_copy_command(Duration::from_millis(50), transport)
                    {
                        Ok(Some(CopyOwnerCommand::ResumeCleanup { operation: actual }))
                            if actual == operation =>
                        {
                            break;
                        }
                        Ok(None) => (),
                        _ => park_completed_copy(completion),
                    }
                }
            }
        }
    }
}
fn retire_completed_copy(
    completion: &Arc<crate::setup::home_capture::InstalledCopyCompletion>,
    transaction: &InstalledCopyTransaction,
    cancel: &AtomicBool,
) -> io::Result<bool> {
    for turn in 0..3 {
        match completion.retire_material(transaction.attempt(), cancel) {
            Err(error)
                if turn < 2
                    && matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) =>
            {
                std::thread::park_timeout(Duration::from_millis(50));
            }
            result => return result,
        }
    }
    unreachable!("bounded cleanup loop always returns on its final attempt")
}
/// Durable Complete cannot become Failed or authorize a fresh export. Retain
/// the same opaque completion and its actual source/output/namespace custody in
/// this executing root task when ACK or exact retirement remains uncertain.
fn park_completed_copy(completion: &Arc<crate::setup::home_capture::InstalledCopyCompletion>) -> ! {
    let _held = completion.clone();
    loop {
        std::thread::park_timeout(Duration::from_secs(1));
    }
}
fn wait_importer(peer: &process_binding::ExecutingPeer) -> io::Result<()> {
    loop {
        let mut fd = libc::pollfd {
            fd: peer.pidfd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&raw mut fd, 1, 50) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(refused());
        }
        if fd.revents & libc::POLLIN != 0 {
            return Ok(());
        }
    }
}
/// Constructor-private observation from the actual pinned importer channel.
/// Arrays, expectations, process death or a public checkpoint cannot construct it.
pub struct ActualCopyReceiveComplete {
    artifacts: Arc<InstalledVerifiedArtifacts>,
    importer: Arc<process_binding::ExecutingPeer>,
    home: Option<[u8; 32]>,
    applications: Option<[u8; 32]>,
}
impl ActualCopyReceiveComplete {
    pub fn evidence(
        &self,
    ) -> (
        &Arc<InstalledVerifiedArtifacts>,
        &Arc<process_binding::ExecutingPeer>,
        Option<[u8; 32]>,
        Option<[u8; 32]>,
    ) {
        (
            &self.artifacts,
            &self.importer,
            self.home,
            self.applications,
        )
    }
}
fn drive_receive(
    issue: &mut InstalledCopyIssue,
    owner: &mut RootConsumerChannel,
    artifacts: &Arc<InstalledVerifiedArtifacts>,
    importer: Arc<process_binding::ExecutingPeer>,
    cancel: &AtomicBool,
    transport: &ServiceCancellation,
) -> io::Result<ActualCopyReceiveComplete> {
    let tx = artifacts.transaction();
    let operation = tx.operation().to_owned();
    let (expected_home, expected_apps) = artifacts.expectations();
    if serde_json::to_vec(issue.declaration()?)? != serde_json::to_vec(&artifacts.declaration()?)? {
        return Err(refused());
    }
    issue.deliver()?;
    let mut prepared = false;
    let mut home_complete = expected_home.is_none();
    let mut apps_complete = expected_apps.is_none();
    let mut finish = false;
    let mut apps_requested = false;
    loop {
        crate::setup::migration::check_cancel(cancel)?;
        if let Some(command) =
            owner.receive_installed_copy_command(Duration::from_millis(1), transport)?
        {
            match command {
                CopyOwnerCommand::Cancel {} => {
                    tx.cancel();
                    return Err(io::ErrorKind::Interrupted.into());
                }
                CopyOwnerCommand::Retry {}
                | CopyOwnerCommand::AcknowledgeComplete { .. }
                | CopyOwnerCommand::ResumeCleanup { .. }
                | CopyOwnerCommand::AcknowledgeCleanup { .. } => {
                    return Err(refused());
                }
            }
        }
        let Some(event) = issue.receive_timeout::<CopyEvent>(Duration::from_millis(50))? else {
            continue;
        };
        match event {
            CopyEvent::Reviewed {
                operation: op,
                home,
                applications,
            } if op == operation
                && home == expected_home.is_some()
                && applications == expected_apps.is_some()
                && !prepared =>
            {
                prepared = true;
                issue.send(&CopyCommand::Prepare {})?;
            }
            CopyEvent::ReadyToAdopt { operation: op, .. }
                if op == operation && prepared && !home_complete =>
            {
                owner.send_installed_copy_status(
                    &CopyOwnerEvent::Adopting {
                        operation: operation.clone(),
                    },
                    transport,
                )?;
                issue.send(&CopyCommand::Adopt {})?;
            }
            CopyEvent::Adopting { operation: op } if op == operation => {}
            CopyEvent::AdoptionFinished {
                operation: op,
                complete: true,
                cancelled: false,
                failures: 0,
                verified: true,
                ..
            } if op == operation => {
                home_complete = true;
            }
            CopyEvent::ApplicationsRestored { operation: op, .. } if op == operation => {
                apps_complete = true;
            }
            CopyEvent::Complete {
                operation: op,
                home_sha256,
                applications_sha256,
            } if op == operation && finish => {
                if home_sha256 != expected_home.as_ref().map(|h| h.ciphertext_sha256)
                    || applications_sha256 != expected_apps.as_ref().map(|a| a.ciphertext_sha256)
                {
                    return Err(refused());
                }
                return Ok(ActualCopyReceiveComplete {
                    artifacts: artifacts.clone(),
                    importer,
                    home: home_sha256,
                    applications: applications_sha256,
                });
            }
            CopyEvent::Failed {
                operation: op,
                message,
            } if op == operation => return Err(io::Error::other(message)),
            _ => return Err(refused()),
        }
        if prepared && home_complete && !apps_complete && !apps_requested {
            apps_requested = true;
            owner.send_installed_copy_status(
                &CopyOwnerEvent::RestoringApplications {
                    operation: operation.clone(),
                },
                transport,
            )?;
            issue.send(&CopyCommand::RestoreApplications {})?;
        }
        if prepared && home_complete && apps_complete && !finish {
            finish = true;
            issue.send(&CopyCommand::Finish {})?;
        }
    }
}

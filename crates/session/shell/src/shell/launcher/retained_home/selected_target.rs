//! An admitted selected object continued through the ordinary signed launch queue.
use super::super::{InstalledApp, LaunchCommand, action_authority};
use super::{
    Registration,
    bootstrap::{self, ProofCustody},
    open_selected::{Dispatcher, Objects},
    refused,
};
use crate::shell::{
    native_workspace_owner::NativeWorkspaceOwner,
    state::{RoomId, WorkspaceTreeHandle},
};
use runtime_collab::{
    open_selected::{
        self, ClaimReply, ClaimSelected, LaunchSelected, ObservedOpenReply, PollSelected,
        RootCommand, RootReceipt, RootRequest, TargetNamespaceReceipt, TargetReply,
        v2::{self, SelectedAccess},
    },
    retained_person::RetainedLaunchBinding,
};
use runtime_portal::{fd_transport, process_binding::ExecutingPeer};
use sha2::{Digest, Sha256};
use std::os::fd::AsFd;
use std::{
    io::{self, Read},
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_action_broker::services::{OpenSelectedContext, OpenSelectedFuture};
const START_BUDGET: Duration = Duration::from_secs(120);
type Check = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
/// A claim is custody on this actual admitted bootstrap, not a UUID credential.
pub(crate) struct SelectedContinuation {
    source: Arc<Dispatcher>,
    context: OpenSelectedContext,
    request: RootRequest,
    receipt: RootReceipt,
    access: Option<SelectedAccess>,
    destination: String,
    succeeded: AtomicBool,
    prepared: Mutex<Option<LifetimeReply>>,
}
/// Where a prepared target's lifetime is answered, or why it is not.
type LifetimeReply = tokio::sync::oneshot::Sender<Result<Arc<TargetLifetime>, String>>;
/// Where a continuation waits for its target's lifetime.
type LifetimeAnswer = tokio::sync::oneshot::Receiver<Result<Arc<TargetLifetime>, String>>;
impl SelectedContinuation {
    fn editable(&self) -> bool {
        self.access.is_some_and(SelectedAccess::editable)
    }
    pub(super) fn gate_opcode(&self) -> u8 {
        if self.editable() { 4 } else { 3 }
    }
    fn check_edit_author(&self, owner: &NativeWorkspaceOwner) -> io::Result<()> {
        if self.editable() {
            let certificate = self
                .context
                .observed_launch()
                .certificate()
                .map_err(io::Error::other)?;
            if super::super::native_app_open_access(
                owner,
                self.room(),
                &certificate,
                &self.request.app_id,
            )
            .map_err(io::Error::other)?
                != super::super::NativeAppOpenAccess::Author
            {
                return Err(refused());
            }
        }
        Ok(())
    }
    pub(crate) fn room(&self) -> RoomId {
        self.source.prepared().room
    }
    pub(crate) fn destination(&self) -> &str {
        &self.destination
    }
    pub(crate) fn prepare_workspace(
        &self,
        tree: &WorkspaceTreeHandle,
        package: &str,
    ) -> Result<super::super::LaunchGrants, String> {
        self.context.authorize()?;
        let owner = tree
            .read()
            .installed_native_owner()
            .ok_or("native selected target owner unavailable")?;
        self.check_edit_author(&owner).map_err(|e| e.to_string())?;
        let workspace = super::super::native_launch_workspace(tree, self.room())?;
        if package != self.request.app_id
            || !self
                .context
                .observed_launch()
                .matches(package, None, &workspace)
        {
            return Err("original selected target launch scope differs".into());
        }
        let path = std::path::Path::new(&self.destination);
        let mime = mime_guess::from_path(path)
            .first_raw()
            .and_then(|mime| desktop_policy::ResourceKind::mime(mime).ok())
            .is_some_and(|kind| super::super::revalidate_resource_claim(package, &kind));
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| desktop_policy::ResourceKind::extension(extension).ok())
            .is_some_and(|kind| super::super::revalidate_resource_claim(package, &kind));
        if !mime && !extension {
            return Err("selected target no longer declares this file type".into());
        }
        super::super::prepare_observed_native_launch(
            tree,
            self.room(),
            package,
            self.context.observed_launch(),
        )
    }
    fn checker(&self) -> Check {
        let context = self.context.clone();
        Arc::new(move || context.authorize())
    }
    pub(crate) fn report_error(&self, error: String) {
        if let Ok(mut pending) = self.prepared.lock()
            && let Some(reply) = pending.take()
        {
            let _ = reply.send(Err(error));
        }
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lease is moved into the spawned target, and the prepared target is kept until its lifetime is answered"
    )]
    pub(crate) fn dispatch(
        self: &Arc<Self>,
        app: InstalledApp,
        mut command: LaunchCommand,
        lease: Option<action_authority::Lease>,
        owner: Option<NativeWorkspaceOwner>,
    ) -> io::Result<()> {
        self.context.authorize().map_err(io::Error::other)?;
        super::super::super::launch_readiness::attach(&mut command);
        super::super::diagnostics::freeze_retained(
            &mut command.env,
            *super::super::lock(&super::super::LaunchSession::current().diagnostics),
        )?;
        let target = Arc::new(PreparedTarget::new(
            &app,
            command.clone(),
            self.clone(),
            owner.ok_or_else(refused)?,
        )?);
        let (registration, lifetime) = target.register()?;
        let lease = lease
            .ok_or_else(refused)?
            .retain_selected_target(lifetime.clone());
        registration
            .spawn(app, command, lease)
            .map_err(io::Error::other)?;
        let reply = self
            .prepared
            .lock()
            .map_err(|_| refused())?
            .take()
            .ok_or_else(refused)?;
        reply.send(Ok(lifetime)).map_err(|_| refused())
    }
    fn claim(
        source: Arc<Dispatcher>,
        context: OpenSelectedContext,
        request: RootRequest,
        receipt: RootReceipt,
        access: Option<SelectedAccess>,
    ) -> io::Result<(Arc<Self>, LifetimeAnswer)> {
        if receipt.request_id != request.request_id
            || receipt.source_operation != source.operation()
        {
            return Err(refused());
        }
        let checker = {
            let context = context.clone();
            Arc::new(move || context.authorize()) as Check
        };
        let claim = ClaimSelected {
            schema: 1,
            request_id: request.request_id.clone(),
            source_operation: receipt.source_operation.clone(),
            app_id: request.app_id.clone(),
        };
        let reply = source.transact(checker.as_ref(), |stream| {
            write_command(stream, &RootCommand::ClaimSelected(claim), access)?;
            let bytes = read_frame(stream, open_selected::MAX_WIRE_BYTES)?;
            let reply = match access {
                Some(access) => v2::decode_claim(&bytes, access)?,
                None => open_selected::decode_claim_reply(&bytes)?,
            };
            if reply.request_id() != request.request_id {
                return Err(refused());
            }
            Ok(reply)
        })?;
        if !matches!(reply, ClaimReply::Claimed { .. }) {
            return Err(io::Error::other("selected object launch claim refused"));
        }
        let destination = open_selected::selected_destination(&request.components)?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        Ok((
            Arc::new(Self {
                source,
                context,
                request,
                receipt,
                access,
                destination,
                succeeded: AtomicBool::new(false),
                prepared: Mutex::new(Some(sender)),
            }),
            receiver,
        ))
    }
    fn poll(&self, target: &PreparedTarget) -> io::Result<Option<TargetNamespaceReceipt>> {
        let reply = self.source.transact(self.checker().as_ref(), |stream| {
            write_command(
                stream,
                &RootCommand::PollSelected(PollSelected {
                    schema: 1,
                    request_id: self.request.request_id.clone(),
                }),
                self.access,
            )?;
            let bytes = read_frame(stream, open_selected::MAX_WIRE_BYTES)?;
            let reply = match self.access {
                Some(access) => v2::decode_target(&bytes, access)?,
                None => open_selected::decode_target_reply(&bytes)?,
            };
            if reply.request_id() != self.request.request_id {
                return Err(refused());
            }
            Ok(reply)
        })?;
        match reply {
            TargetReply::Pending { .. } => Ok(None),
            TargetReply::NamespaceReady { receipt } => match target.check_receipt(&receipt) {
                Ok(()) => Ok(Some(receipt)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(error) => Err(error),
            },
            TargetReply::Refused { .. } => Err(io::Error::other(
                "selected target namespace admission refused",
            )),
            TargetReply::CancelAccepted { .. } => Err(refused()),
        }
    }
}
/// Native proof and package ownership stay with the actual ordinary supervisor.
pub(crate) struct TargetLifetime {
    prepared: Arc<PreparedTarget>,
    _proof: ProofCustody,
    _daemon_proof: Option<ProofCustody>,
    _reader_proof: Option<ProofCustody>,
}
impl TargetLifetime {
    fn commit(&self, receipt: &TargetNamespaceReceipt) -> io::Result<()> {
        self.prepared.check_receipt(receipt)?;
        self.prepared.check()?;
        self.prepared.committed.store(true, Ordering::Release);
        self.prepared.check()?;
        self.prepared
            .selection
            .succeeded
            .store(true, Ordering::Release);
        Ok(())
    }
}
pub(super) struct PreparedTarget {
    package: package_store::InstalledPackage,
    publisher: [u8; 32],
    command: LaunchCommand,
    selection: Arc<SelectedContinuation>,
    owner: NativeWorkspaceOwner,
    committed: AtomicBool,
    launcher: Mutex<Option<Arc<ExecutingPeer>>>,
}
impl PreparedTarget {
    fn new(
        app: &InstalledApp,
        command: LaunchCommand,
        selection: Arc<SelectedContinuation>,
        owner: NativeWorkspaceOwner,
    ) -> io::Result<Self> {
        let package = package_store::find_installed(&app.package_id)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        if !app.has_gui
            || app.package_id != selection.request.app_id
            || app.manifest_path != package.manifest_path
            || app.payload_root != package.payload_root
        {
            return Err(refused());
        }
        let publisher = super::verified_gui_package(&package)?;
        selection.check_edit_author(&owner)?;
        let value = Self {
            package,
            publisher,
            command,
            selection,
            owner,
            committed: AtomicBool::new(false),
            launcher: Mutex::new(None),
        };
        value.check()?;
        Ok(value)
    }
    pub(super) fn opcode(&self) -> u8 {
        self.selection.gate_opcode()
    }
    pub(super) fn package(&self) -> &str {
        &self.package.id
    }
    pub(super) fn room(&self) -> RoomId {
        self.selection.room()
    }
    pub(super) fn person(&self) -> String {
        self.selection
            .source
            .prepared()
            .session
            .person()
            .principal()
            .to_owned()
    }
    pub(super) fn check_command(&self, command: &LaunchCommand) -> io::Result<()> {
        if command != &self.command {
            return Err(refused());
        }
        self.check()
    }
    fn check(&self) -> io::Result<()> {
        if !self.committed.load(Ordering::Acquire) {
            self.selection
                .context
                .authorize()
                .map_err(io::Error::other)?;
        }
        let session = || &self.selection.source.prepared().session;
        session().check().map_err(io::Error::other)?;
        let certificate = session()
            .retained_launch_certificate(self.room())
            .map_err(io::Error::other)?;
        if !self.committed.load(Ordering::Acquire)
            && self
                .selection
                .context
                .observed_launch()
                .certificate()
                .map_err(io::Error::other)?
                != certificate
        {
            return Err(refused());
        }
        if self.selection.editable()
            && super::super::native_app_open_access(
                &self.owner,
                self.room(),
                &certificate,
                &self.package.id,
            )
            .map_err(io::Error::other)?
                != super::super::NativeAppOpenAccess::Author
        {
            return Err(refused());
        }
        if self
            .owner
            .with_current(self.room(), |view| Ok(view.device().clone()))
            .map_err(io::Error::other)?
            != certificate
        {
            return Err(refused());
        }
        if super::super::native_app_open_access(
            &self.owner,
            self.room(),
            &certificate,
            &self.package.id,
        )
        .map_err(io::Error::other)?
            == super::super::NativeAppOpenAccess::Delegate
        {
            return Err(refused());
        }
        let installed = package_store::find_installed(&self.package.id)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        if installed.package_id_hex != self.package.package_id_hex
            || installed.manifest_path != self.package.manifest_path
            || installed.payload_root != self.package.payload_root
            || super::verified_gui_package(&installed)? != self.publisher
        {
            return Err(refused());
        }
        super::validate_command(&self.command, &installed, self.room())?;
        if self.command.program
            != super::super::selected_execution_program()
                .map_err(|e| io::Error::other(e.user_message()))?
            || self.command.managed_agent_id.is_some()
            || self
                .command
                .args
                .iter()
                .any(|arg| arg == "--retained-files")
        {
            return Err(refused());
        }
        let mut open = self
            .command
            .args
            .iter()
            .enumerate()
            .filter(|(_, arg)| arg.as_str() == "--open");
        let Some((index, _)) = open.next() else {
            return Err(refused());
        };
        if open.next().is_some()
            || self.command.args.get(index.saturating_add(1)) != Some(&self.selection.destination)
        {
            return Err(refused());
        }
        session().check().map_err(io::Error::other)
    }
    fn context(&self) -> io::Result<Vec<u8>> {
        self.check()?;
        let mut bytes = super::CONTEXT_DOMAIN.to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                self.selection.source.operation(),
                &self.package.id,
                hex::encode(self.publisher),
                &self.package.package_id_hex,
                self.room().to_string(),
                self.resource(),
                &self.command.program,
                &self.command.args,
                &self.command.env,
            ))
            .map_err(io::Error::other)?,
        );
        if bytes.len() > 64 * 1024 {
            return Err(refused());
        }
        Ok(bytes)
    }
    fn resource(&self) -> String {
        match self.selection.access {
            Some(access) => format!(
                "selected-open-v2:{}:{}",
                access.as_str(),
                self.selection.request.request_id
            ),
            None => format!("selected-open:{}", self.selection.request.request_id),
        }
    }
    pub(super) fn binding(&self) -> io::Result<RetainedLaunchBinding> {
        let value = RetainedLaunchBinding {
            operation_id: self.selection.source.operation().into(),
            app_id: self.package.id.clone(),
            package_sha256: self.package.package_id_hex.clone(),
            room: self.room().to_string(),
            resource_record: self.resource(),
            target_uid: rustix::process::geteuid().as_raw(),
            target_pid: std::process::id(),
            root_pid: self.selection.source.root().pid(),
            context_sha256: hex::encode(Sha256::digest(self.context()?)),
        };
        value.validate()?;
        Ok(value)
    }
    fn editable_stage(&self, binding: &RetainedLaunchBinding) -> Option<v2::LaunchSelected> {
        self.selection.editable().then(|| v2::LaunchSelected {
            schema: 2,
            request_id: self.selection.request.request_id.clone(),
            binding: binding.clone(),
            publisher_hex: hex::encode(self.publisher),
            access: SelectedAccess::EditInParent,
        })
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered root transaction: the proof, the endpoint and the staging are handed over and acknowledged in turn on one stream"
    )]
    fn register(self: &Arc<Self>) -> io::Result<(Registration, Arc<TargetLifetime>)> {
        let binding = self.binding()?;
        let (proof_stream, root_proof) = UnixStream::pair()?;
        let prepared = self.clone();
        let proof_binding = binding.clone();
        let session = self.selection.source.prepared().session.clone();
        let proof = bootstrap::start_selected_proof(proof_stream, move |stream| {
            session.serve_retained_launch_proof_checked(stream, &proof_binding, move || {
                prepared.check()
            })
        })?;
        let proof = self.selection.source.track_proof(proof)?;
        let mut daemon_proof = None;
        let mut reader_proof = None;
        let (endpoint, root_endpoint) = UnixStream::pair()?;
        let stage = self.editable_stage(&binding);
        let launch = LaunchSelected {
            schema: 1,
            request_id: self.selection.request.request_id.clone(),
            binding: binding.clone(),
            publisher_hex: hex::encode(self.publisher),
        };
        let staged = self.selection.source.transact_operation(
            self.selection.checker().as_ref(),
            |stream, deadline| {
                write_command(
                    stream,
                    &RootCommand::LaunchSelected(launch),
                    self.selection.access,
                )?;
                wire_frame::Framing::little_endian(64 * 1024)
                    .reject_empty()
                    .write(stream, &self.context()?)
                    .map_err(io::Error::other)?;
                for (tag, fd) in [(84, root_proof.as_fd()), (85, root_endpoint.as_fd())] {
                    self.check()?;
                    fd_transport::send_with_fd(stream.as_fd(), &[tag], fd)?;
                    let mut ack = [0];
                    stream.read_exact(&mut ack)?;
                    if ack != [tag] {
                        return Err(refused());
                    }
                }
                if let Some(stage) = &stage {
                    let bytes = read_frame(stream, open_selected::MAX_WIRE_BYTES)?;
                    // This exact response confirms bounded root staging only.
                    // Actual namespace admission and adopted window are still
                    // required by observe_target before semantic completion.
                    match v2::decode_stage(&bytes, stage)? {
                        v2::StageResult::Staged { .. } => {}
                        v2::StageResult::Refused { .. } => {
                            return Ok(Err(io::Error::other(
                                "selected editor staging unavailable",
                            )));
                        }
                    }
                }
                if stage.is_some() {
                    let request = runtime_fs::external_history::daemon_control::EnrollmentRequest {
                        schema: 1,
                        request_id: self.selection.request.request_id.clone(),
                        person_binding: serde_json::to_vec(&binding).map_err(io::Error::other)?,
                    };
                    let Some((recipient, context)) = bootstrap::receive_daemon_delegation(
                        stream,
                        self.selection.source.root().clone(),
                        &request,
                    )?
                    else {
                        return Ok(Err(io::Error::other(
                            "selected editor history enrollment unavailable",
                        )));
                    };
                    self.check_daemon_context(&context)?;
                    let (daemon_stream, daemon_endpoint) = UnixStream::pair()?;
                    let prepared = self.clone();
                    let daemon_binding = binding.clone();
                    let daemon_session = self.selection.source.prepared().session.clone();
                    let custody =
                        bootstrap::start_selected_proof(daemon_stream, move |proof_stream| {
                            daemon_session.serve_retained_daemon_proof_checked(
                                proof_stream,
                                &daemon_binding,
                                &recipient,
                                move || prepared.check_daemon_context(&context),
                            )
                        })?;
                    let custody = self.selection.source.track_proof(custody)?;
                    // Transport ACK is not daemon admission or a source grant.
                    // Root completes enrollment before ordinary delegation.
                    fd_transport::send_with_fd(stream.as_fd(), &[62], daemon_endpoint.as_fd())?;
                    let mut ack = [0];
                    stream.read_exact(&mut ack)?;
                    if ack != [62] {
                        return Err(refused());
                    }
                    daemon_proof = Some(custody);
                    self.check()?;
                    let Some((reader, reader_context)) = bootstrap::receive_editor_read_delegation(
                        stream,
                        self.selection.source.root().clone(),
                        &request,
                    )?
                    else {
                        return Ok(Err(io::Error::other(
                            "selected editor Read enrollment unavailable",
                        )));
                    };
                    self.check_daemon_context(&reader_context)?;
                    let (reader_stream, reader_endpoint) = UnixStream::pair()?;
                    let prepared_reader = self.clone();
                    let reader_binding = binding.clone();
                    let reader_session = self.selection.source.prepared().session.clone();
                    let custody =
                        bootstrap::start_selected_proof(reader_stream, move |proof_stream| {
                            reader_session.serve_retained_daemon_proof_checked(
                                proof_stream,
                                &reader_binding,
                                &reader,
                                move || prepared_reader.check_daemon_context(&reader_context),
                            )
                        })?;
                    let custody = self.selection.source.track_proof(custody)?;
                    // The fresh Read stream is not a clone of Files or of the
                    // edit issuer. The daemon independently admits its purpose.
                    fd_transport::send_with_fd(stream.as_fd(), &[62], reader_endpoint.as_fd())?;
                    let mut ack = [0];
                    stream.read_exact(&mut ack)?;
                    if ack != [62] {
                        return Err(refused());
                    }
                    reader_proof = Some(custody);
                    self.check()?;
                }
                let delegated =
                    match launcher::ordinary_child::DelegatedConsumingBootstrap::receive_selected(
                        stream,
                        self.selection.source.root().executing(),
                        deadline,
                    )? {
                        Ok(Some(delegated)) => delegated,
                        Ok(None) => {
                            return Ok(Err(io::Error::other(
                                "selected editor delegation unavailable",
                            )));
                        }
                        Err(error) => return Ok(Err(error)),
                    };
                Ok(delegated.receive_ordinary_until(
                    &launcher::retained_view::ConsumerCommand {
                        program: self
                            .command
                            .program
                            .to_str()
                            .ok_or_else(refused)?
                            .to_owned(),
                        args: self.command.args.clone(),
                        env: self.command.env.clone(),
                    },
                    deadline,
                ))
            },
        )??;
        self.check()?;
        let registration = Registration::selected(
            self.clone(),
            binding,
            endpoint,
            self.selection.source.root().clone(),
        )?
        .with_prepared_child(staged);
        let lifetime = Arc::new(TargetLifetime {
            prepared: self.clone(),
            _proof: proof,
            _daemon_proof: daemon_proof,
            _reader_proof: reader_proof,
        });
        Ok((registration, lifetime))
    }
    fn check_daemon_context(
        &self,
        context: &runtime_fs::external_history::EnrollmentContext,
    ) -> io::Result<()> {
        self.check()?;
        context.encode()?;
        let binding = self.binding()?;
        let certificate = self
            .selection
            .source
            .prepared()
            .session
            .retained_launch_certificate(self.room())
            .map_err(io::Error::other)?;
        if context.person_binding != serde_json::to_vec(&binding).map_err(io::Error::other)?
            || context.verifier.source_operation != binding.operation_id
            || context.verifier.personal_room != binding.room
            || context.verifier.person != certificate.user_public_key
            || context.verifier.device != certificate.device_public_key
        {
            return Err(refused());
        }
        self.check()
    }
    pub(super) fn bind_launcher(&self, launcher: Arc<ExecutingPeer>) -> io::Result<()> {
        self.check()?;
        let mut held = self.launcher.lock().map_err(|_| refused())?;
        if held.is_some() || !launcher.current() {
            return Err(refused());
        }
        *held = Some(launcher);
        drop(held);
        Ok(())
    }
    fn check_receipt(&self, receipt: &TargetNamespaceReceipt) -> io::Result<()> {
        self.check()?;
        let held = self.launcher.lock().map_err(|_| refused())?;
        let launcher = held.as_ref().ok_or(io::ErrorKind::WouldBlock)?;
        if !receipt.valid()
            || receipt.request_id != self.selection.request.request_id
            || receipt.app_id != self.package.id
            || receipt.package_sha256 != self.package.package_id_hex
            || !launcher.belongs_to(receipt.launcher_pid, receipt.launcher_start)
        {
            return Err(refused());
        }
        drop(held);
        Ok(())
    }
}
fn write_command(
    stream: &mut UnixStream,
    command: &RootCommand,
    access: Option<SelectedAccess>,
) -> io::Result<()> {
    let bytes = match access {
        None => {
            if !command.valid() {
                return Err(refused());
            }
            open_selected::encode(command)?
        }
        Some(access) => {
            let command = match command {
                RootCommand::OpenSelected(v) => v2::RootCommand::OpenSelected(v2::RootRequest {
                    schema: 2,
                    request_id: v.request_id.clone(),
                    components: v.components.clone(),
                    app_id: v.app_id.clone(),
                    access,
                }),
                RootCommand::ClaimSelected(v) => {
                    v2::RootCommand::ClaimSelected(v2::ClaimSelected {
                        schema: 2,
                        request_id: v.request_id.clone(),
                        source_operation: v.source_operation.clone(),
                        app_id: v.app_id.clone(),
                        access,
                    })
                }
                RootCommand::LaunchSelected(v) => {
                    let launch = v2::LaunchSelected {
                        schema: 2,
                        request_id: v.request_id.clone(),
                        binding: v.binding.clone(),
                        publisher_hex: v.publisher_hex.clone(),
                        access,
                    };
                    if access == SelectedAccess::EditInParent {
                        v2::RootCommand::StageSelectedConsumer(launch)
                    } else {
                        v2::RootCommand::LaunchSelected(launch)
                    }
                }
                RootCommand::PollSelected(v) => v2::RootCommand::PollSelected(v2::PollSelected {
                    schema: 2,
                    request_id: v.request_id.clone(),
                    access,
                }),
                RootCommand::CancelSelected(v) => {
                    v2::RootCommand::CancelSelected(v2::PollSelected {
                        schema: 2,
                        request_id: v.request_id.clone(),
                        access,
                    })
                }
            };
            if !command.valid() {
                return Err(refused());
            }
            open_selected::encode(&command)?
        }
    };
    wire_frame::Framing::little_endian(open_selected::MAX_WIRE_BYTES)
        .reject_empty()
        .write(stream, &bytes)
        .map_err(io::Error::other)
}
fn read_frame(stream: &mut UnixStream, max: usize) -> io::Result<Vec<u8>> {
    wire_frame::Framing::little_endian(max)
        .reject_empty()
        .read(stream)
        .map_err(io::Error::other)?
        .ok_or_else(refused)
}
/// Captures the actual launch session before the async broker leaves its thread.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the dispatcher found is moved into the continuation it claims"
)]
pub(crate) fn begin(
    tree: WorkspaceTreeHandle,
    context: OpenSelectedContext,
    request: open_selected::Request,
) -> OpenSelectedFuture {
    let session = super::super::LaunchSession::current();
    Box::pin(async move {
        let setup = tokio::task::spawn_blocking(move || {
            session.adopt();
            if !request.valid() {
                return Err("invalid selected-open request".to_string());
            }
            context.authorize()?;
            let selected = context.duplicate_selected(request.selected_descriptor)?;
            let parent = context.duplicate_selected(request.parent_descriptor)?;
            let proof = UnixStream::from(std::os::fd::OwnedFd::from(
                context.duplicate_selected(request.proof_descriptor)?,
            ));
            let source = super::open_selected::find(context.workspace(), context.executing())
                .map_err(|e| e.to_string())?;
            let check = {
                let context = context.clone();
                Arc::new(move || context.authorize()) as Check
            };
            let receipt = source
                .admit(
                    &request.request,
                    Objects {
                        selected,
                        parent,
                        proof,
                    },
                    context.executing(),
                    check.as_ref(),
                )
                .map_err(|e| e.to_string())?;
            let (continuation, prepared) =
                SelectedContinuation::claim(source, context, request.request, receipt, None)
                    .map_err(|e| e.to_string())?;
            let app = super::super::installed_apps()
                .map_err(|e| e.user_message())?
                .into_iter()
                .find(|app| app.package_id == continuation.request.app_id)
                .ok_or("selected target application is no longer installed")?;
            let readiness = crate::shell::launch_readiness::observe(|| {
                super::super::queue_selected_open(app, tree, continuation.clone())
                    .map_err(|e| e.user_message())
            })?;
            Ok::<_, String>((continuation, prepared, readiness))
        })
        .await
        .map_err(|e| e.to_string())??;
        observe_target(setup.0, setup.1, setup.2).await
    })
}
#[allow(
    clippy::significant_drop_tightening,
    reason = "the dispatcher found is moved into the continuation it claims"
)]
pub(crate) fn begin_v2(
    tree: WorkspaceTreeHandle,
    context: OpenSelectedContext,
    request: v2::Request,
) -> tilo_action_broker::services::OpenSelectedV2Future {
    let session = super::super::LaunchSession::current();
    Box::pin(async move {
        let setup = tokio::task::spawn_blocking(move || {
            session.adopt();
            if !request.valid() {
                return Err("invalid selected-open request".to_string());
            }
            std::str::from_utf8(
                request
                    .request
                    .components
                    .last()
                    .ok_or("selected leaf missing")?,
            )
            .map_err(|_| "selected filename cannot be represented by the current launcher argv")?;
            context.authorize()?;
            let selected = context.duplicate_selected(request.selected_descriptor)?;
            let parent = context.duplicate_selected(request.parent_descriptor)?;
            let proof = UnixStream::from(std::os::fd::OwnedFd::from(
                context.duplicate_selected(request.proof_descriptor)?,
            ));
            let source = super::open_selected::find(context.workspace(), context.executing())
                .map_err(|e| e.to_string())?;
            let check = {
                let context = context.clone();
                Arc::new(move || context.authorize()) as Check
            };
            let receipt = source
                .admit_v2(
                    &request.request,
                    Objects {
                        selected,
                        parent,
                        proof,
                    },
                    context.executing(),
                    check.as_ref(),
                )
                .map_err(|e| e.to_string())?;
            let (continuation, prepared) = SelectedContinuation::claim(
                source,
                context,
                request.request.navigation(),
                receipt,
                Some(request.request.access),
            )
            .map_err(|e| e.to_string())?;
            let app = super::super::installed_apps()
                .map_err(|e| e.user_message())?
                .into_iter()
                .find(|app| app.package_id == continuation.request.app_id)
                .ok_or("selected target application is no longer installed")?;
            let readiness = crate::shell::launch_readiness::observe(|| {
                super::super::queue_selected_open(app, tree, continuation.clone())
                    .map_err(|e| e.user_message())
            })?;
            Ok::<_, String>((continuation, prepared, readiness))
        })
        .await
        .map_err(|e| e.to_string())??;
        let access = setup.0.access.ok_or("selected access domain lost")?;
        let reply = observe_target(setup.0, setup.1, setup.2).await?;
        Ok(v2::ObservedReply {
            schema: 2,
            access,
            reply,
        })
    })
}
/// Ends the namespace polling of a target observation when the
/// observation ends, whichever way.
struct StopPolling(Arc<AtomicBool>);
impl Drop for StopPolling {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
async fn observe_target(
    continuation: Arc<SelectedContinuation>,
    prepared: LifetimeAnswer,
    readiness: tilo_action_broker::services::LaunchReadiness,
) -> Result<ObservedOpenReply, String> {
    let deadline = Instant::now()
        .checked_add(START_BUDGET)
        .unwrap_or_else(Instant::now);
    let lifetime = tokio::time::timeout(START_BUDGET, prepared)
        .await
        .map_err(|_| "selected target preparation timed out")?
        .map_err(|_| "selected target preparation retired")??;
    let poll_continuation = continuation.clone();
    let poll_target = lifetime.prepared.clone();
    let polling = Arc::new(AtomicBool::new(true));
    let _poll_guard = StopPolling(polling.clone());
    let namespace = tokio::task::spawn_blocking(move || {
        loop {
            if !polling.load(Ordering::Acquire) {
                return Err("selected target observation cancelled".to_string());
            }
            if Instant::now() >= deadline {
                return Err("selected target namespace admission timed out".to_string());
            }
            if let Some(receipt) = poll_continuation
                .poll(&poll_target)
                .map_err(|e| e.to_string())?
            {
                return Ok(receipt);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    let namespace = async { namespace.await.map_err(|e| e.to_string())? };
    let ready = async {
        tokio::time::timeout(remaining, readiness.wait())
            .await
            .map_err(|_| "selected target window startup timed out".to_string())?
    };
    let (receipt, ()) = tokio::try_join!(namespace, ready)?;
    lifetime.commit(&receipt).map_err(|e| e.to_string())?;
    drop(lifetime);
    Ok(ObservedOpenReply {
        schema: 1,
        request_id: continuation.request.request_id.clone(),
        source_operation: continuation.receipt.source_operation.clone(),
        target: receipt,
    })
}

impl Drop for SelectedContinuation {
    fn drop(&mut self) {
        if self.succeeded.load(Ordering::Acquire) {
            return;
        }
        let source = self.source.clone();
        let request_id = self.request.request_id.clone();
        let access = self.access;
        // Failure cleanup requests revocation; it never claims the namespace
        // stopped. If this worker cannot start, root still owns the finite
        // claim and fresh target proof refusal fences any issued view.
        let _ = std::thread::Builder::new()
            .name("tilo-selected-open-cancel".into())
            .spawn(move || {
                let prepared = source.prepared().clone();
                let check =
                    Arc::new(move || prepared.check(&prepared.command).map_err(|e| e.to_string()))
                        as Check;
                let _ = source.transact(check.as_ref(), |stream| {
                    write_command(
                        stream,
                        &RootCommand::CancelSelected(PollSelected {
                            schema: 1,
                            request_id: request_id.clone(),
                        }),
                        access,
                    )?;
                    let bytes = read_frame(stream, open_selected::MAX_WIRE_BYTES)?;
                    let reply = match access {
                        Some(access) => v2::decode_target(&bytes, access)?,
                        None => open_selected::decode_target_reply(&bytes)?,
                    };
                    if reply.request_id() != request_id {
                        return Err(refused());
                    }
                    match reply {
                        TargetReply::CancelAccepted { .. } | TargetReply::Refused { .. } => Ok(()),
                        _ => Err(refused()),
                    }
                });
            });
    }
}

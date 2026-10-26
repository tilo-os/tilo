//! Private preparation for ordinary, signed GUI launches using retained Home.
//! A root-owned actor must supply the namespace endpoint after its independent
//! installed-helper admission. Nothing here creates source filesystem rights.
mod bootstrap;
pub(crate) mod retirement;
pub(crate) use bootstrap::OriginalCopyBootstrapCustody;
pub(crate) mod discovery;
pub(crate) mod open_selected;
pub(crate) mod selected_target;

use super::{InstalledApp, LaunchCommand};
use crate::shell::action_authority;
use crate::shell::{
    collab_broker::NativePersonalSession, native_workspace_owner::NativeWorkspaceOwner,
    state::RoomId,
};
use runtime_collab::retained_person::RetainedLaunchBinding;
use sha2::{Digest, Sha256};
use std::os::fd::AsFd;
use std::{io, os::unix::net::UnixStream, sync::Arc, time::Duration};

const RESOURCE: &str = "retained-home";
const CONTEXT_DOMAIN: &[u8] = b"tilo-ordinary-retained-home-context-v1\0";
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "ordinary retained Home launch refused",
    )
}

/// Private preflight routing for ordinary Files Home. The old projection is
/// only an operation hint; root independently admits the installed source.
pub(super) struct FilesHomeLaunch {
    operation: String,
    owner: NativeWorkspaceOwner,
    session: Arc<NativePersonalSession>,
    room: RoomId,
}
impl FilesHomeLaunch {
    pub(super) fn select(
        app: &InstalledApp,
        room: RoomId,
        owner: Option<&NativeWorkspaceOwner>,
    ) -> io::Result<Option<Self>> {
        if app.package_id != super::FILES_PACKAGE_ID {
            return Ok(None);
        }
        let operation = super::lock(&super::RETAINED_FILES)
            .get(&room)
            .and_then(Option::as_ref)
            .map(|(operation, _)| operation.clone());
        let Some(operation) = operation else {
            return Ok(None);
        };
        let parsed = uuid::Uuid::parse_str(&operation).map_err(io::Error::other)?;
        if parsed.is_nil() || parsed.to_string() != operation {
            return Err(refused());
        }
        let owner = owner.ok_or_else(refused)?.clone();
        let certificate = owner
            .with_current(room, |view| Ok(view.device().clone()))
            .map_err(io::Error::other)?;
        let principal = hex::encode(certificate.user_public_key);
        let session = crate::shell::collab_broker::NativePersonalSource::admit_for_socket(
            &launcher::services::collab_socket(),
            &principal,
        )
        .map_err(io::Error::other)?;
        if session
            .retained_launch_certificate(room)
            .map_err(io::Error::other)?
            != certificate
        {
            return Err(refused());
        }
        Ok(Some(Self {
            operation,
            owner,
            session,
            room,
        }))
    }

    pub(super) fn attach_projection(
        &self,
        command: &mut LaunchCommand,
    ) -> io::Result<tempfile::TempDir> {
        let directory = tempfile::Builder::new()
            .prefix("tilo-retained-files-launch-")
            .tempdir_in(super::runtime_dir())?;
        let path = directory.path().join("locations.json");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema": 1, "locations": [{"id":self.operation,"path":"/run/tilo/retained-home"}]
        }))
        .map_err(io::Error::other)?;
        tilo_files::native::atomic::write_private(&path, &bytes)?;
        command
            .args
            .extend(["--retained-files".into(), path.display().to_string()]);
        Ok(directory)
    }

    pub(super) fn dispatch(
        self,
        app: InstalledApp,
        mut command: LaunchCommand,
        lease: Option<action_authority::Lease>,
        projection: Option<tempfile::TempDir>,
    ) -> io::Result<()> {
        // Startup metadata is part of the exact root-replayed context. The
        // retained queue must never attach it after preparation freezes argv.
        super::super::launch_readiness::attach(&mut command);
        self.session.check().map_err(io::Error::other)?;
        let current = super::lock(&super::RETAINED_FILES)
            .get(&self.room)
            .and_then(Option::as_ref)
            .map(|(operation, _)| operation.clone());
        if current.as_deref() != Some(self.operation.as_str()) {
            return Err(refused());
        }
        let lease = lease
            .ok_or_else(refused)?
            .retain_home_projection(projection.ok_or_else(refused)?);
        super::diagnostics::freeze_retained(
            &mut command.env,
            *super::lock(&super::LaunchSession::current().diagnostics),
        )?;
        let prepared = Arc::new(PreparedRetainedHome::prepare(
            &app,
            command.clone(),
            self.owner,
            self.session,
            self.room,
        )?);
        let registration = prepared.bootstrap(
            &self.operation,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )?;
        registration
            .spawn(app, command, lease)
            .map_err(io::Error::other)
    }
}

/// Not serializable or constructible from a launch-request label. Native
/// session, signed installation and accepted room remain live custody.
pub(crate) struct PreparedRetainedHome {
    package: package_store::InstalledPackage,
    publisher: [u8; 32],
    owner: NativeWorkspaceOwner,
    session: Arc<NativePersonalSession>,
    room: RoomId,
    command: LaunchCommand,
}
impl PreparedRetainedHome {
    pub(crate) fn bootstrap(
        self: Arc<Self>,
        installed_source_operation: &str,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<Registration> {
        bootstrap::start(self, installed_source_operation, cancel)
    }

    /// Explicit installed original-copy continuation for the genuine signed
    /// Imager selection. Ordinary Files bootstrap never enters this path.
    pub(crate) fn bootstrap_original_copy(
        self: &Arc<Self>,
        installed_source_operation: &str,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<OriginalCopyBootstrapCustody> {
        bootstrap::establish_original_copy(self, installed_source_operation, cancel)
    }

    pub(crate) fn prepare(
        app: &InstalledApp,
        command: LaunchCommand,
        owner: NativeWorkspaceOwner,
        session: Arc<NativePersonalSession>,
        room: RoomId,
    ) -> io::Result<Self> {
        let package = package_store::find_installed(&app.package_id)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        if !app.has_gui
            || package.manifest_path != app.manifest_path
            || package.payload_root != app.payload_root
        {
            return Err(refused());
        }
        let publisher = verified_gui_package(&package)?;
        let prepared = Self {
            package,
            publisher,
            owner,
            session,
            room,
            command,
        };
        prepared.check(&prepared.command)?;
        Ok(prepared)
    }

    fn check(&self, command: &LaunchCommand) -> io::Result<()> {
        if command != &self.command
            || command.program
                != super::selected_execution_program()
                    .map_err(|error| io::Error::other(error.user_message()))?
            || command.managed_agent_id.is_some()
        {
            return Err(refused());
        }
        let installed = package_store::find_installed(&self.package.id)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        // Eviction bookkeeping is irrelevant; installed package authority is
        // the signed identity, paths and publisher, not last-used timestamps.
        if installed.package_id_hex != self.package.package_id_hex
            || installed.manifest_path != self.package.manifest_path
            || installed.payload_root != self.package.payload_root
            || verified_gui_package(&installed)? != self.publisher
        {
            return Err(refused());
        }
        validate_command(command, &installed, self.room)?;
        let certificate = self
            .session
            .retained_launch_certificate(self.room)
            .map_err(io::Error::other)?;
        require_author(&self.owner, self.room, &certificate, &installed.id)?;
        self.session.check().map_err(io::Error::other)
    }

    /// Fresh Files Read semantics are checked independently of ordinary Author
    /// launch admission. An Author label alone cannot authorize history reads.
    fn check_history_read(&self) -> io::Result<()> {
        self.check(&self.command)?;
        if self.package.id != super::FILES_PACKAGE_ID {
            return Err(refused());
        }
        let certificate = self
            .session
            .retained_launch_certificate(self.room)
            .map_err(io::Error::other)?;
        require_history_read(&self.owner, self.room, &certificate, &self.package.id)?;
        self.session.check().map_err(io::Error::other)
    }

    fn check_history_read_context(
        &self,
        binding: &RetainedLaunchBinding,
        context: &runtime_fs::external_history::EnrollmentContext,
    ) -> io::Result<()> {
        self.check_history_read()?;
        context.encode()?;
        if self.binding(&binding.operation_id, binding.root_pid)? != *binding {
            return Err(refused());
        }
        let certificate = self
            .session
            .retained_launch_certificate(self.room)
            .map_err(io::Error::other)?;
        if context.person_binding != serde_json::to_vec(binding).map_err(io::Error::other)?
            || context.verifier.source_operation != binding.operation_id
            || context.verifier.personal_room != binding.room
            || context.verifier.person != certificate.user_public_key
            || context.verifier.device != certificate.device_public_key
        {
            return Err(refused());
        }
        self.check_history_read()
    }

    /// The root actor must replay these exact bytes with the already frozen
    /// ordinary launch context; this digest is not independent app authority.
    pub(crate) fn context_bytes(&self, operation: &str) -> io::Result<Vec<u8>> {
        self.check(&self.command)?;
        let mut bytes = CONTEXT_DOMAIN.to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                operation,
                &self.package.id,
                hex::encode(self.publisher),
                &self.package.package_id_hex,
                self.room.to_string(),
                RESOURCE,
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

    pub(crate) fn binding(
        &self,
        operation: &str,
        authenticated_root_pid: u32,
    ) -> io::Result<RetainedLaunchBinding> {
        let value = RetainedLaunchBinding {
            operation_id: operation.to_owned(),
            app_id: self.package.id.clone(),
            package_sha256: self.package.package_id_hex.clone(),
            room: self.room.to_string(),
            resource_record: RESOURCE.into(),
            target_uid: rustix::process::geteuid().as_raw(),
            target_pid: std::process::id(),
            root_pid: authenticated_root_pid,
            context_sha256: hex::encode(Sha256::digest(self.context_bytes(operation)?)),
        };
        value.validate()?;
        Ok(value)
    }

    /// Called on its own worker, independently of the registration socket.
    /// The existing QPE handshake verifies the exact root task and refreshes
    /// certificate/revocations at every request. No caller-supplied proof bytes.
    pub(crate) fn serve_proof(
        self: Arc<Self>,
        stream: UnixStream,
        binding: &RetainedLaunchBinding,
    ) -> io::Result<()> {
        if self.binding(&binding.operation_id, binding.root_pid)? != *binding {
            return Err(refused());
        }
        self.session
            .clone()
            .serve_retained_launch_proof_checked(stream, binding, move || self.check(&self.command))
    }
}

fn verified_gui_package(package: &package_store::InstalledPackage) -> io::Result<[u8; 32]> {
    let report = package_store::integrity::verify(package, &package.payload_root)
        .map_err(io::Error::other)?;
    if !report.is_intact() {
        return Err(refused());
    }
    let record = package_store::integrity::read_record(
        &package_store::integrity::record_dir(package),
        &package.id,
        &package.package_id_hex,
    )
    .map_err(io::Error::other)?;
    let manifest = launcher::manifest::ManifestView::from_toml_str(
        std::str::from_utf8(&record.manifest_bytes).map_err(io::Error::other)?,
    )
    .map_err(io::Error::other)?;
    if manifest.package_id != package.id
        || manifest.kind != launcher::manifest::KIND_APP
        || manifest.entrypoint.gui_bin().is_none()
    {
        return Err(refused());
    }
    Ok(record.root.publisher_identity)
}

fn require_author(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    certificate: &tilo_user_identity::DeviceCert,
    package: &str,
) -> io::Result<()> {
    if super::native_app_open_access(owner, room, certificate, package).map_err(io::Error::other)?
        != super::NativeAppOpenAccess::Author
    {
        return Err(refused());
    }
    Ok(())
}

fn require_history_read(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    certificate: &tilo_user_identity::DeviceCert,
    package: &str,
) -> io::Result<()> {
    owner
        .with_current(room, |view| {
            if !view.peer().certificates().contains(certificate) {
                return Err("history Read certificate is no longer admitted".into());
            }
            let now = std::time::SystemTime::now();
            view.peer()
                .check_document_read(certificate.device_public_key, package, "", now)?;
            let member = view
                .peer()
                .permissions(certificate.device_public_key, now)?;
            let node = member.policy.node();
            let application = tilo_action_policy::Authority::person(
                member
                    .policy
                    .party_of(tilo_action_policy::Subject::App(package.to_owned())),
            );
            if !member.allows(&node, tilo_action_policy::ActionKind::Read, now)
                || application
                    .decide(
                        &node.app(package),
                        tilo_action_policy::ActionKind::Read,
                        &tilo_action_policy::Context::at_time(now),
                    )
                    .answer
                    != tilo_action_policy::Answer::Allow
            {
                return Err("history Read capability is unavailable".into());
            }
            Ok(())
        })
        .map_err(io::Error::other)
}

fn validate_command(
    command: &LaunchCommand,
    package: &package_store::InstalledPackage,
    room: RoomId,
) -> io::Result<()> {
    let Some(identity) = &command.service_identity else {
        return Err(refused());
    };
    if identity.app_id != package.id
        || identity.workspace_id != room.to_string()
        || command.args.first().map(String::as_str) != Some("run")
        || command
            .args
            .iter()
            .any(|arg| ["--service", "--cli", "--headless-console"].contains(&arg.as_str()))
    {
        return Err(refused());
    }
    for (flag, expected) in [
        (
            "--manifest",
            package.manifest_path.to_string_lossy().into_owned(),
        ),
        (
            "--payload-root",
            package.payload_root.to_string_lossy().into_owned(),
        ),
        ("--app-id", package.id.clone()),
        ("--workspace-id", room.to_string()),
        ("--expected-package-hash", package.package_id_hex.clone()),
    ] {
        let mut positions = command
            .args
            .iter()
            .enumerate()
            .filter(|(_, value)| value.as_str() == flag);
        let Some((index, _)) = positions.next() else {
            return Err(refused());
        };
        if positions.next().is_some()
            || command.args.get(index.saturating_add(1)) != Some(&expected)
        {
            return Err(refused());
        }
    }
    Ok(())
}

/// The stream is only transport. The independently authenticated root actor
/// and signed launcher still have to admit one another before mounting bytes.
enum PreparedRegistration {
    Home(Arc<PreparedRetainedHome>),
    Selected(Arc<selected_target::PreparedTarget>),
}
impl PreparedRegistration {
    fn package(&self) -> &str {
        match self {
            Self::Home(home) => &home.package.id,
            Self::Selected(target) => target.package(),
        }
    }
    fn room(&self) -> RoomId {
        match self {
            Self::Home(home) => home.room,
            Self::Selected(target) => target.room(),
        }
    }
    fn person(&self) -> String {
        match self {
            Self::Home(home) => home.session.person().principal().to_owned(),
            Self::Selected(target) => target.person(),
        }
    }
    fn opcode(&self) -> u8 {
        match self {
            Self::Home(_) => 2,
            Self::Selected(selected) => selected.opcode(),
        }
    }
    fn check(&self, command: &LaunchCommand) -> io::Result<()> {
        match self {
            Self::Home(home) => home.check(command),
            Self::Selected(target) => target.check_command(command),
        }
    }
    fn binding(&self, operation: &str, root: u32) -> io::Result<RetainedLaunchBinding> {
        match self {
            Self::Home(home) => home.binding(operation, root),
            Self::Selected(target) => {
                let binding = target.binding()?;
                if binding.operation_id != operation || binding.root_pid != root {
                    return Err(refused());
                }
                Ok(binding)
            }
        }
    }
}
pub(crate) struct Registration {
    child: Option<launcher::ordinary_child::PreparedOrdinaryChild>,
    prepared: PreparedRegistration,
    endpoint: UnixStream,
    binding: RetainedLaunchBinding,
    root: Arc<bootstrap::AdmittedRootHelper>,
    dispatcher: Option<Arc<open_selected::Dispatcher>>,
}
impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetainedHomeRegistration")
    }
}
impl Registration {
    fn new(
        prepared: Arc<PreparedRetainedHome>,
        binding: RetainedLaunchBinding,
        endpoint: UnixStream,
        root: Arc<bootstrap::AdmittedRootHelper>,
        dispatcher: Arc<open_selected::Dispatcher>,
    ) -> io::Result<Self> {
        if prepared.binding(&binding.operation_id, binding.root_pid)? != binding {
            return Err(refused());
        }
        Ok(Self {
            child: None,
            prepared: PreparedRegistration::Home(prepared),
            endpoint,
            binding,
            root,
            dispatcher: Some(dispatcher),
        })
    }

    fn selected(
        prepared: Arc<selected_target::PreparedTarget>,
        binding: RetainedLaunchBinding,
        endpoint: UnixStream,
        root: Arc<bootstrap::AdmittedRootHelper>,
    ) -> io::Result<Self> {
        if prepared.binding()? != binding {
            return Err(refused());
        }
        Ok(Self {
            child: None,
            prepared: PreparedRegistration::Selected(prepared),
            binding,
            endpoint,
            root,
            dispatcher: None,
        })
    }
    /// Existing authenticated root bootstrap alone supplies actual child custody.
    pub(super) fn with_prepared_child(
        mut self,
        child: launcher::ordinary_child::PreparedOrdinaryChild,
    ) -> Self {
        self.child = Some(child);
        self
    }
    pub(super) fn take_prepared_child(
        &mut self,
    ) -> io::Result<launcher::ordinary_child::PreparedOrdinaryChild> {
        self.child.take().ok_or_else(refused)
    }
    pub(super) fn operation_id(&self) -> &str {
        &self.binding.operation_id
    }
    /// Enter the same pending-launch queue and ordinary start supervisor.
    /// No alternative app policy, manifest or service-only launch is built.
    pub(crate) fn spawn(
        self,
        app: InstalledApp,
        command: LaunchCommand,
        authority: action_authority::Lease,
    ) -> Result<(), String> {
        if app.package_id != self.prepared.package() {
            return Err(refused().to_string());
        }
        self.prepared
            .check(&command)
            .map_err(|error| error.to_string())?;
        let session = super::LaunchSession::current();
        super::admit_launch(
            &session.gate,
            super::HeldLaunch {
                authority: Some(authority),
                retained: Some(self),
                package_id: app.package_id,
                display_name: app.display_name,
                command,
            },
            |held| super::room_hold::admit(&session, held, super::start_held),
            |event| session.push(event),
        )
    }

    pub(super) fn release(
        self,
        gate: &UnixStream,
        pid: u32,
        command: &LaunchCommand,
        lease: Option<&mut action_authority::Lease>,
        child: &launcher::ordinary_child::PreparedOrdinaryChild,
    ) -> io::Result<()> {
        self.prepared.check(command)?;
        if self
            .prepared
            .binding(&self.binding.operation_id, self.binding.root_pid)?
            != self.binding
        {
            return Err(refused());
        }
        let launch = super::lock(&super::LAUNCHED_PROCESSES);
        let recorded = launch.get(&pid).ok_or_else(refused)?;
        if !recorded.confined
            || recorded.package_id != self.prepared.package()
            || recorded.workspace_id.as_deref() != Some(self.prepared.room().to_string().as_str())
            || super::proc_stat(pid).map(|(_, start)| start) != Some(recorded.start_time)
        {
            return Err(refused());
        }
        drop(launch);
        lease.ok_or_else(refused)?.retained_release(
            pid,
            self.prepared.room(),
            self.prepared.package(),
            &self.prepared.person(),
        )?;
        self.prepared.check(command)?;
        gate.set_write_timeout(Some(Duration::from_secs(10)))?;
        runtime_portal::fd_transport::send_with_fd(
            gate.as_fd(),
            &[self.prepared.opcode()],
            self.endpoint.as_fd(),
        )?;
        let launcher = self.root.release(gate, pid)?;
        child.check_peer(&launcher)?;
        match &self.prepared {
            PreparedRegistration::Home(_) => {
                self.dispatcher.as_ref().ok_or_else(refused)?.bind(launcher)
            }
            PreparedRegistration::Selected(target) => target.bind_launcher(launcher),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{
        collab_broker::NativePersonalSource, collab_broker_capnp::LocalSessionAuthority,
        native_workspace_owner::test_enrolled_owner,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use std::{sync::atomic::Ordering, time::SystemTime};
    use tilo_action_policy::{Answer, Grant, KindSet, Subject};
    use tilo_peer_state::TransportOrigin;

    fn signed_package(root: &std::path::Path) -> package_store::InstalledPackage {
        let key = SigningKey::from_bytes(&[151; 32]);
        let manifest = format!(
            "[package]\nid = \"so.tilo.retained-fixture\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
             [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\
             [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             [sandbox.net]\nmode = \"off\"\n[entrypoint]\nbin = \"bin/app\"\n",
            hex::encode(key.verifying_key().to_bytes())
        );
        let manifest = manifest.as_bytes();
        let payload_bytes = b"synthetic signed app payload";
        // A fixture encoded in the documented stable package wire format.
        // Admission below uses the production decoder, signature verifier and
        // installed payload integrity walk; no fixture grant is manufactured.
        let mut collection = b"TLOCOL\0\0".to_vec();
        collection.extend(1_u32.to_le_bytes());
        collection.extend(7_u32.to_le_bytes());
        collection.extend(b"bin/app");
        collection.extend(blake3::hash(payload_bytes).as_bytes());
        let mut signed = b"TLOPKG\0\0".to_vec();
        signed.extend(tilo_app_package::TAPP_FORMAT_VERSION.to_le_bytes());
        signed.extend(blake3::hash(manifest).as_bytes());
        signed.extend(blake3::hash(&collection).as_bytes());
        signed.push(1);
        signed.extend(key.verifying_key().as_bytes());
        signed.extend(key.sign(&signed).to_bytes());
        let hash = blake3::hash(&signed).to_hex().to_string();
        let payload = root.join("pkgs").join(&hash);
        let metadata = root.join("pkgs").join(format!("{hash}.pkg"));
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::create_dir_all(&metadata).unwrap();
        std::fs::write(payload.join("bin/app"), payload_bytes).unwrap();
        std::fs::write(metadata.join("manifest.toml"), manifest).unwrap();
        std::fs::write(metadata.join("signed-root"), signed).unwrap();
        std::fs::write(metadata.join("collection.qcl"), collection).unwrap();
        package_store::InstalledPackage {
            id: "so.tilo.retained-fixture".into(),
            version: "1.0.0".into(),
            package_id_hex: hash,
            payload_root: payload,
            manifest_path: metadata.join("manifest.toml"),
            publisher_key_hex: hex::encode(key.verifying_key().to_bytes()),
            pinned: false,
            installed_at: 0,
            last_used: 0,
            attested_by: String::new(),
            cli_commands: Vec::new(),
            runtime: String::new(),
            source: String::new(),
            held: false,
            system: false,
            payload_bytes: payload_bytes.len() as u64,
        }
    }

    #[test]
    fn ordinary_retained_selection_replays_real_signed_package_and_payload_integrity() {
        let directory = tempfile::tempdir().unwrap();
        let package = signed_package(directory.path());
        assert_eq!(
            verified_gui_package(&package).unwrap(),
            SigningKey::from_bytes(&[151; 32])
                .verifying_key()
                .to_bytes()
        );
        std::fs::write(package.payload_root.join("bin/app"), b"replaced app").unwrap();
        assert!(verified_gui_package(&package).is_err());
        std::fs::write(
            package.payload_root.join("bin/app"),
            b"synthetic signed app payload",
        )
        .unwrap();
        verified_gui_package(&package).unwrap();
        let original = std::fs::read(&package.manifest_path).unwrap();
        std::fs::write(
            &package.manifest_path,
            b"[package]\nid = \"so.tilo.changed\"\n",
        )
        .unwrap();
        assert!(verified_gui_package(&package).is_err());
        std::fs::write(&package.manifest_path, original).unwrap();
        let path = package.manifest_path.parent().unwrap().join("signed-root");
        let mut changed = std::fs::read(&path).unwrap();
        *changed.last_mut().unwrap() ^= 1;
        std::fs::write(path, changed).unwrap();
        assert!(verified_gui_package(&package).is_err());
    }

    fn governance(owner: &NativeWorkspaceOwner, room: RoomId, cap: Grant) {
        let key = SigningKey::from_bytes(&[61; 32]);
        let now = SystemTime::now();
        let messages = owner
            .with_current(room, |view| {
                let before = view.peer().clone();
                let mut candidate = before.clone();
                candidate
                    .governance_mut()
                    .put_governed_grants(&[cap], &key, now)?;
                candidate.publication_mut().durable_mutation_messages(
                    &before,
                    &key,
                    now,
                    view.generation().checked_add(1).unwrap(),
                )
            })
            .unwrap();
        for message in messages {
            owner
                .receive(
                    room,
                    TransportOrigin::RetainedRelayDelta(key.verifying_key().to_bytes()),
                    message,
                    now,
                )
                .unwrap();
        }
    }

    #[test]
    fn retained_launch_requires_actual_current_room_author_not_viewer_or_label() {
        let (_directory, owner, room, _now, _key) = test_enrolled_owner();
        let (certificate, person) = owner
            .with_current(room, |view| {
                let member = view
                    .peer()
                    .permissions(view.device().device_public_key, SystemTime::now())?;
                Ok((view.device().clone(), member.person))
            })
            .unwrap();
        assert!(require_author(&owner, room, &certificate, "so.tilo.chat").is_err());
        governance(
            &owner,
            room,
            Grant::new(
                "retained-author",
                Subject::app("so.tilo.chat"),
                tilo_action_policy::TreePath::room(room.to_string()),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                Subject::person(person.clone()),
            ),
        );
        require_author(&owner, room, &certificate, "so.tilo.chat").unwrap();
        let mut changed = certificate.clone();
        changed.device_label.push_str(" changed");
        assert!(require_author(&owner, room, &changed, "so.tilo.chat").is_err());
        assert!(
            require_author(
                &owner,
                RoomId::parse("33333333-3333-4333-8333-333333333333").unwrap(),
                &certificate,
                "so.tilo.chat"
            )
            .is_err()
        );
        governance(
            &owner,
            room,
            Grant::new(
                "retained-revoked-author",
                Subject::person(person.clone()),
                tilo_action_policy::TreePath::room(room.to_string()),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person(person),
            ),
        );
        assert!(require_author(&owner, room, &certificate, "so.tilo.chat").is_err());
    }

    fn revoke_governance(owner: &NativeWorkspaceOwner, room: RoomId, ids: &[String]) {
        let key = SigningKey::from_bytes(&[61; 32]);
        let now = SystemTime::now();
        let messages = owner
            .with_current(room, |view| {
                let before = view.peer().clone();
                let mut candidate = before.clone();
                assert!(
                    candidate.governance_mut().remove_governed_grants(
                        &ids.iter()
                            .map(|id| tilo_action_policy::GrantId::new(id.clone()))
                            .collect::<Vec<_>>(),
                        &key,
                        now
                    )?
                );
                candidate.publication_mut().durable_mutation_messages(
                    &before,
                    &key,
                    now,
                    view.generation().checked_add(1).unwrap(),
                )
            })
            .unwrap();
        for message in messages {
            owner
                .receive(
                    room,
                    TransportOrigin::RetainedRelayDelta(key.verifying_key().to_bytes()),
                    message,
                    now,
                )
                .unwrap();
        }
    }

    #[test]
    fn history_read_requires_actual_current_read_even_for_an_author() {
        let (_directory, owner, room, _now, _key) = test_enrolled_owner();
        let (certificate, person) = owner
            .with_current(room, |view| {
                let member = view
                    .peer()
                    .permissions(view.device().device_public_key, SystemTime::now())?;
                Ok((view.device().clone(), member.person))
            })
            .unwrap();
        governance(
            &owner,
            room,
            Grant::new(
                "history-author",
                Subject::app("so.tilo.chat"),
                tilo_action_policy::TreePath::room(room.to_string()),
                KindSet::UNDOABLE,
                Answer::Allow,
                Subject::person(person.clone()),
            ),
        );
        governance(
            &owner,
            room,
            Grant::new(
                "history-reader",
                Subject::app("so.tilo.chat"),
                tilo_action_policy::TreePath::room(room.to_string()),
                KindSet::READ,
                Answer::Allow,
                Subject::person(person.clone()),
            ),
        );
        require_author(&owner, room, &certificate, "so.tilo.chat").unwrap();
        require_history_read(&owner, room, &certificate, "so.tilo.chat").unwrap();
        let mut changed = certificate.clone();
        changed.device_label.push_str(" changed");
        assert!(require_history_read(&owner, room, &changed, "so.tilo.chat").is_err());
        governance(
            &owner,
            room,
            Grant::new(
                "history-read-ceiling",
                Subject::person(person.clone()),
                tilo_action_policy::TreePath::room(room.to_string()),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person(person),
            ),
        );
        require_history_read(&owner, room, &certificate, "so.tilo.chat").unwrap();
        assert!(require_author(&owner, room, &certificate, "so.tilo.chat").is_err());
        revoke_governance(
            &owner,
            room,
            &["history-reader".into(), "history-author".into()],
        );
        assert!(require_history_read(&owner, room, &certificate, "so.tilo.chat").is_err());
    }

    #[test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "test fixtures transitively own long-lived permits and lock guards by design"
    )]
    fn retained_certificate_rechecks_genuine_native_session_and_never_revives_old_epoch() {
        let fixture = tilo_action_broker::collab_broker_capnp::tests::certified_person_fixture();
        let lifecycle = LocalSessionAuthority::new();
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let _source = NativePersonalSource::install_for_test(
            socket.clone(),
            fixture.broker.clone(),
            lifecycle.lifecycle(),
            fixture.host.clone(),
        )
        .unwrap();
        assert!(
            NativePersonalSource::admit_for_socket(&socket, fixture.context.principal()).is_err()
        );
        lifecycle.lifecycle().publish_native_state(false, false);
        let session =
            NativePersonalSource::admit_for_socket(&socket, fixture.context.principal()).unwrap();
        let mut key = [0; 32];
        hex::decode_to_slice(fixture.context.principal(), &mut key).unwrap();
        let room = RoomId::parse(&collab_types::person_room(&key).to_string()).unwrap();
        session.retained_launch_certificate(room).unwrap();
        assert!(
            session
                .retained_launch_certificate(
                    RoomId::parse("33333333-3333-4333-8333-333333333333").unwrap()
                )
                .is_err()
        );
        lifecycle.lifecycle().set_locked(true);
        assert!(session.retained_launch_certificate(room).is_err());
        lifecycle.lifecycle().set_locked(false);
        assert!(session.retained_launch_certificate(room).is_err());
        let fresh =
            NativePersonalSource::admit_for_socket(&socket, fixture.context.principal()).unwrap();
        fresh.retained_launch_certificate(room).unwrap();
        fixture.current.store(false, Ordering::Release);
        assert!(fresh.retained_launch_certificate(room).is_err());
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "test fixtures transitively own long-lived permits and lock guards by design"
    )]
    fn files_home_caller_replays_native_source_and_ignores_ambient_location_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", directory.path());
        let fixture = tilo_action_broker::collab_broker_capnp::tests::certified_person_fixture();
        let lifecycle = LocalSessionAuthority::new();
        lifecycle.lifecycle().publish_native_state(false, false);
        let socket = launcher::services::collab_socket();
        let source = NativePersonalSource::install_for_test(
            socket.clone(),
            fixture.broker.clone(),
            lifecycle.lifecycle(),
            fixture.host.clone(),
        )
        .unwrap();
        let session =
            NativePersonalSource::admit_for_socket(&socket, fixture.context.principal()).unwrap();
        let mut key = [0; 32];
        hex::decode_to_slice(fixture.context.principal(), &mut key).unwrap();
        let room = RoomId::parse(&collab_types::person_room(&key).to_string()).unwrap();
        let (_owner_directory, owner, other_room, _, _) = test_enrolled_owner();
        let operation = uuid::Uuid::new_v4().to_string();
        let app = InstalledApp {
            package_id: super::super::FILES_PACKAGE_ID.into(),
            display_name: "Files".into(),
            localized_names: std::collections::BTreeMap::new(),
            keywords: vec![],
            category: None,
            version: "1.0.0".into(),
            manifest_path: "/apps/files/manifest.toml".into(),
            payload_root: "/apps/files".into(),
            has_gui: true,
            opens_extensions: vec![],
            opens_mime_types: vec![],
            opens_uri_schemes: vec![],
            opens_secondary: false,
            offers: vec![],
        };
        super::super::lock(&super::super::RETAINED_FILES).insert(
            other_room,
            Some((operation.clone(), "/ambient/forged/source".into())),
        );
        assert!(FilesHomeLaunch::select(&app, other_room, None).is_err());
        assert!(FilesHomeLaunch::select(&app, other_room, Some(&owner)).is_err());
        super::super::lock(&super::super::RETAINED_FILES).remove(&other_room);
        let selection = FilesHomeLaunch {
            operation: operation.clone(),
            owner,
            session,
            room,
        };
        let mut command = LaunchCommand {
            program: "/usr/libexec/tilo/tilo-launcher".into(),
            args: vec![],
            env: vec![],
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
            x11_workspace: None,
        };
        let projection = selection.attach_projection(&mut command).unwrap();
        let path = projection.path().join("locations.json");
        let bytes = std::fs::read(&path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["locations"][0]["path"], "/run/tilo/retained-home");
        assert_eq!(json["locations"][0]["id"], operation);
        assert!(!String::from_utf8(bytes).unwrap().contains("ambient"));
        // Revoking the real routing hint must refuse before entering bootstrap;
        // no normal/unscoped launch is substituted after selection.
        assert_eq!(
            selection
                .dispatch(app, command, None, Some(projection))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(source);
    }
}

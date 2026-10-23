//! Declared source-capture helper protocol. Requests are caller-private claims;
//! source scope and protected durable capture custody are independently checked.
use super::{
    home_capture::{Broker, Grant, Rendezvous, RpcAction, RpcReply, Status},
    migration::PinnedDirectory,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub const ARG: &str = "--tilo-home-capture-helper";
const MAX_REQUEST: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Capture,
    Validate,
    Cancel,
    Release,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    pub action: Action,
    pub grant: Grant,
    #[serde(default)]
    pub channel: Option<Rendezvous>,
}

/// Closed registration request for Ext4/XFS or supported same-volume Home.
/// No Btrfs Grant, snapshot claim, caller groups or completed archive appear.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalRequest {
    schema: u32,
    action: OriginalAction,
    pub home: super::migration_store::NativePath,
    pub plan: super::migration_handoff::Plan,
    channel: Rendezvous,
    pub preparation: Option<super::migration_review::OriginalPreparation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inventory_file: Option<CallerInventory>,
    #[serde(skip)]
    inventory_bytes: Option<Vec<u8>>,
}
impl OriginalRequest {
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub fn channel_for_registration(&self) -> &Rendezvous {
        &self.channel
    }
    pub fn new(
        home: &Path,
        plan: super::migration_handoff::Plan,
        channel: Rendezvous,
    ) -> io::Result<Self> {
        let (plan, inventory_bytes) = plan.original_inventory_selection()?;
        if !plan.categories.home || plan.home_policy != super::migration_handoff::HomePolicy::Copy {
            return Err(failure(
                "Original registration requires selected full Home copying",
            ));
        }
        Ok(Self {
            schema: 2,
            action: OriginalAction::RegisterOriginal,
            home: super::migration_store::NativePath::encode(home),
            plan,
            channel,
            preparation: super::migration_review::OriginalPreparation::review(home)?,
            inventory_file: None,
            inventory_bytes,
        })
    }
    pub fn new_applications(
        home: &Path,
        plan: super::migration_handoff::Plan,
        channel: Rendezvous,
    ) -> io::Result<Self> {
        let (plan, inventory_bytes) = plan.original_inventory_selection()?;
        if plan.categories.home
            || !plan.categories.flatpak
            || plan.home_policy != super::migration_handoff::HomePolicy::Copy
            || inventory_bytes.is_none()
        {
            return Err(failure(
                "Original applications registration requires exact selected data consent",
            ));
        }
        Ok(Self {
            schema: 2,
            action: OriginalAction::RegisterApplications,
            home: super::migration_store::NativePath::encode(home),
            plan,
            channel,
            preparation: super::migration_review::OriginalPreparation::review(home)?,
            inventory_file: None,
            inventory_bytes,
        })
    }
    fn validate_material(&self) -> io::Result<()> {
        self.plan.validate()?;
        let selected = match self.action {
            OriginalAction::RegisterOriginal => self.plan.categories.home,
            OriginalAction::RegisterApplications => {
                !self.plan.categories.home
                    && self.plan.categories.flatpak
                    && self.plan.flatpak_inventory.is_some()
            }
        };
        if !selected || self.plan.home_policy != super::migration_handoff::HomePolicy::Copy {
            return Err(failure(
                "Original registration action differs from selected material",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallerInventory {
    device: u64,
    inode: u64,
    len: u64,
}
fn original_inventory_name(operation: &str) -> String {
    format!("{operation}.flatpak-inventory.json")
}
impl OriginalRequest {
    /// Called inside the mutually authenticated original-owner admission only.
    pub fn admitted_inventory(&self, directory: &Path, caller: u32) -> io::Result<Option<File>> {
        use super::migration_handoff::{INVENTORY_BOUND, OriginalInventoryBinding};
        let binding = OriginalInventoryBinding::parse(&self.plan)?;
        match (binding, self.inventory_file.as_ref()) {
            (None, None) => Ok(None),
            (Some(binding), Some(expected)) => {
                let parent = PinnedDirectory::open_absolute(directory)?;
                let facts = parent.file.metadata()?;
                if facts.uid() != caller || facts.mode() & 0o077 != 0 {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                let name = original_inventory_name(&self.plan.operation_id);
                let mut options = OpenOptions::new();
                options.read(true).follow(FollowSymlinks::No).nonblock(true);
                let mut file = cap_primitives::fs::open(&parent.file, Path::new(&name), &options)?;
                let before = file.metadata()?;
                if !before.is_file()
                    || before.uid() != caller
                    || before.nlink() != 1
                    || before.mode() & 0o077 != 0
                    || (before.dev(), before.ino(), before.len())
                        != (expected.device, expected.inode, expected.len)
                    || before.len() != binding.len
                    || before.len() > INVENTORY_BOUND
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(INVENTORY_BOUND + 1)
                    .read_to_end(&mut bytes)?;
                binding.validate_bytes(&bytes)?;
                let after = file.metadata()?;
                if (after.dev(), after.ino(), after.len())
                    != (before.dev(), before.ino(), before.len())
                    || before.mtime() != after.mtime()
                    || before.mtime_nsec() != after.mtime_nsec()
                    || before.ctime() != after.ctime()
                    || before.ctime_nsec() != after.ctime_nsec()
                {
                    return Err(failure("Original inventory changed while reading"));
                }
                let named = super::nofollow::metadata_nofollow(&parent.file, Path::new(&name))?;
                let observed = cap_primitives::fs::Metadata::from_file(&file)?;
                if !super::nofollow::same_directory_identity(&named, &observed) {
                    return Err(failure("Original inventory name changed"));
                }
                parent.validate()?;
                Ok(Some(file))
            }
            _ => Err(failure(
                "Original protected inventory handoff missing or invented",
            )),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum OriginalAction {
    RegisterOriginal,
    RegisterApplications,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum DispatchRequest {
    Capture(Request),
    Original(OriginalRequest),
    Retained(RetainedRequest),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedRequest {
    schema: u32,
    action: RetainedAction,
    pub home: super::migration_store::NativePath,
    pub plan: super::migration_handoff::Plan,
    pub channel: Rendezvous,
    pub setup_binding: String,
    pub identity_public_key: [u8; 32],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RetainedAction {
    RegisterRetainedCaller,
}
impl RetainedRequest {
    pub fn new(
        home: &Path,
        plan: super::migration_handoff::Plan,
        channel: Rendezvous,
        setup_binding: String,
        identity_public_key: [u8; 32],
    ) -> io::Result<Self> {
        let value = Self {
            schema: 1,
            action: RetainedAction::RegisterRetainedCaller,
            home: super::migration_store::NativePath::encode(home),
            plan,
            channel,
            setup_binding,
            identity_public_key,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> io::Result<()> {
        self.plan.bytes()?;
        let path = self.home.decode().map_err(io::Error::other)?;
        if self.schema != 1
            || !path.is_absolute()
            || !self.plan.categories.home
            || self.plan.home_policy != super::migration_handoff::HomePolicy::AccessReadWrite
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.identity_public_key == [0; 32]
        {
            return Err(failure("Invalid explicitly retained caller registration"));
        }
        Ok(())
    }
}
pub fn publish_retained_request(directory: &Path, request: &RetainedRequest) -> io::Result<File> {
    request.validate()?;
    let bytes = serde_json::to_vec(request)?;
    if bytes.len() > MAX_ORIGINAL_REQUEST {
        return Err(failure("Retained request exceeds bound"));
    }
    publish_request_bytes(directory, &bytes, MAX_ORIGINAL_REQUEST)
}
const MAX_ORIGINAL_REQUEST: usize = 96 * 1024;

fn failure(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_request_bytes(path: &Path, caller: u32, maximum: usize) -> io::Result<Vec<u8>> {
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| failure("missing capture request parent"))?,
    )?;
    let facts = parent.file.metadata()?;
    if facts.uid() != caller || facts.mode() & 0o077 != 0 {
        return Err(failure(
            "capture request parent is not private to the authenticated caller",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = cap_primitives::fs::open(
        &parent.file,
        Path::new(
            path.file_name()
                .ok_or_else(|| failure("missing capture request filename"))?,
        ),
        &options,
    )?;
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != caller
        || before.mode() & 0o077 != 0
        || before.nlink() != 1
        || before.len() > maximum as u64
    {
        return Err(failure(
            "capture request is not the bounded caller-private file",
        ));
    }
    let mut bytes = Vec::new();
    let mut file = file;
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let stamp = |metadata: &std::fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    if bytes.len() > maximum || stamp(&before) != stamp(&after) {
        return Err(failure("capture request changed while reading"));
    }
    let named = super::nofollow::metadata_nofollow(
        &parent.file,
        Path::new(
            path.file_name()
                .ok_or_else(|| failure("missing request name"))?,
        ),
    )?;
    let observed = cap_primitives::fs::Metadata::from_file(&file)?;
    if !super::nofollow::same_directory_identity(&named, &observed) || named.is_symlink() {
        return Err(failure("capture request name changed"));
    }
    parent.validate()?;
    Ok(bytes)
}

#[cfg(test)]
pub fn read_request(path: &Path, caller: u32) -> io::Result<Request> {
    let bytes = read_request_bytes(path, caller, MAX_REQUEST)?;
    let request: Request = serde_json::from_slice(&bytes)?;
    if request.schema != 1 {
        return Err(failure("unsupported capture request"));
    }
    Ok(request)
}

fn emit(status: &Status) -> io::Result<()> {
    let mut output = io::stdout().lock();
    let bytes = serde_json::to_vec(status)?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn commands(cancel: Arc<AtomicBool>) -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        let mut frame = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match input.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => {
                    if frame == b"status" {
                        // A full pending status slot is harmless; input remains
                        // bounded and cancellation cannot block behind it.
                        let _ = sender.try_send(());
                        frame.clear();
                        continue;
                    }
                    break;
                }
                Ok(1) if frame.len() < 16 => frame.push(byte[0]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
        // EOF, cancellation/release, malformed or oversized commands revoke
        // this session. No command can retain an unmonitored capture.
        cancel.store(true, Ordering::Release);
    });
    receiver
}

fn execute(request: Request, caller: u32, request_directory: &Path) -> io::Result<()> {
    if matches!(request.action, Action::Cancel | Action::Release) {
        return emit(&Broker::release_existing(&request.grant, caller)?);
    }
    // Admit the exact signed worker contract before creating snapshot custody.
    super::home_capture::require_provider()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let input = commands(Arc::clone(&cancel));
    let mut broker = if matches!(request.action, Action::Capture) {
        Broker::capture(request.grant, caller, &cancel)?
    } else {
        Broker::reopen(request.grant, caller, &cancel)?
    };
    let mut rpc = request
        .channel
        .as_ref()
        .map(|channel| {
            super::home_capture::connect_rpc(
                channel,
                request_directory,
                caller,
                &broker.status().operation,
                cancel.clone(),
            )
        })
        .transpose()?;
    emit(&broker.status())?;
    loop {
        if let Some(session) = rpc.as_mut() {
            match session.incoming.try_recv() {
                Ok(Ok(mut incoming)) => {
                    session.begin(&mut incoming)?;
                    let reply = match incoming.call.action {
                        RpcAction::Run { command, .. } => {
                            if let Ok(response) = broker.run_command_reporting(
                                &std::env::current_exe()?,
                                command,
                                incoming.output,
                                &cancel,
                                &mut |progress| session.progress(progress),
                            ) {
                                RpcReply::Response { response }
                            } else {
                                broker.finish_failed_command()?;
                                RpcReply::Failed
                            }
                        }
                        RpcAction::RegisterOriginal
                        | RpcAction::RegisterOriginalMounted { .. }
                        | RpcAction::RegisterRetainedCaller => {
                            return Err(failure(
                                "Original registration requires its explicit helper request",
                            ));
                        }
                        RpcAction::Validate => {
                            broker.validate(&cancel)?;
                            RpcReply::Validated
                        }
                        RpcAction::Cancel => {
                            cancel.store(true, Ordering::Release);
                            broker.release()?;
                            session.reply(&RpcReply::Released)?;
                            emit(&broker.status())?;
                            return Ok(());
                        }
                    };
                    session.reply(&reply)?;
                }
                Ok(Err(error)) => {
                    cancel.store(true, Ordering::Release);
                    broker.release()?;
                    emit(&broker.status())?;
                    return Err(error);
                }
                Err(mpsc::TryRecvError::Disconnected) => cancel.store(true, Ordering::Release),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        match input.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => emit(&broker.status())?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => cancel.store(true, Ordering::Release),
        }
        if let Err(error) = broker.validate(&cancel) {
            broker.release()?;
            emit(&broker.status())?;
            return if cancel.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(error)
            };
        }
    }
}

fn execute_retained(request: RetainedRequest, caller: u32, directory: &Path) -> io::Result<()> {
    request.validate()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let input = commands(cancel.clone());
    #[cfg(debug_assertions)]
    if std::env::var_os("TILO_NATIVE_CAPTURE_DIAGNOSTICS").is_some() {
        eprintln!("native retained request admitted");
    }
    let mut session = super::home_capture::RootSession::connect_retained(
        &request,
        directory,
        caller,
        cancel.clone(),
    )?;
    let mut status = Status {
        operation: request.plan.operation_id,
        state: super::home_capture::State::Intended,
        expires_unix: None,
    };
    emit(&status)?;
    loop {
        if cancel.load(Ordering::Acquire) {
            status.state = super::home_capture::State::Released;
            return emit(&status);
        }
        match session.incoming.try_recv() {
            Ok(Ok(mut incoming)) => {
                session.begin(&mut incoming)?;
                if !matches!(&incoming.call.action, RpcAction::RegisterRetainedCaller)
                    || incoming.output.is_some()
                    || incoming.mounted.is_some()
                {
                    session.reply(&RpcReply::Failed)?;
                    return Err(failure(
                        "Retained registration refuses capture/export/mounted inputs",
                    ));
                }
                let declaration = incoming.retained_declaration.take().ok_or_else(|| {
                    failure("Retained registration has no actual source descriptor proof")
                })?;
                session.reply(&RpcReply::RetainedCallerRegistered { declaration })?;
                status.state = super::home_capture::State::Released;
                return emit(&status);
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(failure("Retained original peer disconnected"));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match input.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => emit(&status)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => cancel.store(true, Ordering::Release),
        }
    }
}
fn execute_original(request: OriginalRequest, caller: u32, directory: &Path) -> io::Result<()> {
    if request.schema != 2 {
        return Err(failure("Unsupported original source registration"));
    }
    request.validate_material()?;
    request.plan.bytes()?; // existing reboot Plan bound, no dropped inventory
    let cancel = Arc::new(AtomicBool::new(false));
    let input = commands(cancel.clone());
    let (mut session, (witness, original_caller)) =
        super::home_capture::RootSession::connect_original(
            &request.channel,
            directory,
            caller,
            &request,
            cancel.clone(),
        )?;
    // Intended is not capture Ready. This path creates no snapshot/worker.
    let mut status = Status {
        operation: request.plan.operation_id,
        state: super::home_capture::State::Intended,
        expires_unix: None,
    };
    emit(&status)?;
    loop {
        if cancel.load(Ordering::Acquire) {
            status.state = super::home_capture::State::Released;
            return emit(&status);
        }
        match session.incoming.try_recv() {
            Ok(Ok(mut incoming)) => {
                session.begin(&mut incoming)?;
                if !matches!(
                    incoming.call.action,
                    RpcAction::RegisterOriginal | RpcAction::RegisterOriginalMounted { .. }
                ) || incoming.output.is_some()
                {
                    session.reply(&RpcReply::Failed)?;
                    return Err(failure(
                        "Original registration refuses capture/export commands",
                    ));
                }
                let reply = match incoming.call.action {
                    RpcAction::RegisterOriginal if incoming.mounted.is_none() => {
                        RpcReply::OriginalRegistered {
                            witness,
                            original_caller,
                        }
                    }
                    RpcAction::RegisterOriginalMounted { generation, .. } => {
                        let (returned, mounted) = incoming
                            .mounted
                            .ok_or_else(|| failure("Protected mounted registration missing"))?;
                        if returned != generation {
                            return Err(failure("Mounted registration generation changed"));
                        }
                        RpcReply::MountedOriginalRegistered {
                            generation,
                            witness: mounted,
                            original_caller,
                        }
                    }
                    _ => return Err(failure("Original registration inputs conflict")),
                };
                session.reply(&reply)?;
                status.state = super::home_capture::State::Released;
                return emit(&status);
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(failure("Original registration peer disconnected"));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match input.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => emit(&status)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => cancel.store(true, Ordering::Release),
        }
    }
}

/// Recognize only this helper; framework registration happens before any UI.
pub fn maybe_run(args: &[String]) -> Result<Option<i32>, String> {
    if args.get(1).map(String::as_str) != Some(ARG) {
        return Ok(None);
    }
    let (Some(path), None) = (args.get(2), args.get(3)) else {
        return Err("usage: --tilo-home-capture-helper <private-request.json>".into());
    };
    #[cfg(debug_assertions)]
    if std::env::var_os("TILO_NATIVE_CAPTURE_DIAGNOSTICS").is_some() {
        eprintln!("native capture helper entered");
    }
    // SAFETY: reads process credentials; never changes ownership.
    if rustix::process::geteuid().as_raw() != 0 {
        return Err("source capture helper requires authorized elevation".into());
    }
    let caller = super::staging::prepared_setup::caller_uid().map_err(|error| error.to_string())?;
    let bytes = read_request_bytes(Path::new(path), caller, MAX_ORIGINAL_REQUEST)
        .map_err(|error| error.to_string())?;
    let request: DispatchRequest =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let directory = Path::new(path)
        .parent()
        .ok_or("capture request parent missing")?;
    match request {
        DispatchRequest::Capture(request) => {
            if bytes.len() > MAX_REQUEST || request.schema != 1 {
                return Err("unsupported or oversized capture request".into());
            }
            execute(request, caller, directory)
        }
        DispatchRequest::Original(request) => execute_original(request, caller, directory),
        DispatchRequest::Retained(request) => execute_retained(request, caller, directory),
    }
    .map_err(|error| {
        #[cfg(debug_assertions)]
        if std::env::var_os("TILO_NATIVE_CAPTURE_DIAGNOSTICS").is_some() {
            eprintln!("native capture helper execution failed: {error}");
        }
        error.to_string()
    })?;
    Ok(Some(0))
}

/// Scoped request publication for the existing canonical elevation launcher.
/// No source access or privilege is granted by writing this caller-private file.
pub fn publish_request(directory: &Path, request: &Request) -> io::Result<File> {
    publish_request_bytes(directory, &serde_json::to_vec(request)?, MAX_REQUEST)
}
pub fn publish_original_request(
    directory: &Path,
    request: &mut OriginalRequest,
) -> io::Result<File> {
    request.plan.bytes()?;
    if let Some(bytes) = &request.inventory_bytes {
        let pin = PinnedDirectory::open_absolute(directory)?;
        pin.validate_destination()?;
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            super::migration_durability::readable_directory(&pin.file)?,
        )?;
        use tilo_files::native::atomic::{Privacy, PublishMode};
        storage.validate_privacy(Privacy::OwnerOnly)?;
        let name = original_inventory_name(&request.plan.operation_id);
        storage
            .publish(
                std::ffi::OsStr::new(&name),
                bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(io::Error::other)?;
        let file = storage.open_read(std::ffi::OsStr::new(&name), Privacy::OwnerOnly)?;
        let facts = file.metadata()?;
        request.inventory_file = Some(CallerInventory {
            device: facts.dev(),
            inode: facts.ino(),
            len: facts.len(),
        });
        pin.validate_destination()?;
    }
    publish_request_bytes(
        directory,
        &serde_json::to_vec(request)?,
        MAX_ORIGINAL_REQUEST,
    )
}
pub fn publish_request_bytes(directory: &Path, bytes: &[u8], maximum: usize) -> io::Result<File> {
    let pin = super::home_capture::RequestDirectoryPin(PinnedDirectory::open_absolute(directory)?);
    runtime_portal::request_rendezvous::publish_request_bytes(&pin, bytes, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn public_request_publication_uses_readable_pinned_directory_and_reopens_privately() {
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let uid = rustix::process::getuid().as_raw();
        let grant: Grant = serde_json::from_value(serde_json::json!({
            "schema":1,"operation":uuid::Uuid::new_v4().to_string(),"caller":uid,
            "home":super::super::migration_store::NativePath::encode(directory.path()),
            "inode":std::fs::metadata(directory.path()).unwrap().ino(),"owner":uid,
            "filesystem":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "subvolume":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "custody":null,"preparation_namespace":null
        }))
        .unwrap();
        let request = Request {
            schema: 1,
            action: Action::Capture,
            grant,
            channel: None,
        };
        let published = publish_request(directory.path(), &request).unwrap();
        assert_eq!(published.metadata().unwrap().mode() & 0o777, 0o600);
        let reread = read_request(&directory.path().join("request.json"), uid).unwrap();
        assert_eq!(reread.grant, request.grant);
        assert!(publish_request(directory.path(), &request).is_err());
        assert_eq!(
            read_request(&directory.path().join("request.json"), uid)
                .unwrap()
                .grant,
            request.grant
        );
    }
}

#[cfg(test)]
mod retained_request_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    #[test]
    fn retained_request_never_accepts_caller_credentials_or_copy_policy() {
        let temp = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let (channel, _listener) = Rendezvous::create(temp.path()).unwrap();
        let value = serde_json::json!({
            "schema":1,"operation_id":uuid::Uuid::new_v4().to_string(),
            "categories":{"home":true,"flatpak":false,"accounts":false},
            "home_policy":"access-read-write",
            "home":{"volume":{"disk_guid":"11111111-1111-4111-8111-111111111111",
                "disk_bytes":68719476736u64,"sector_bytes":512,
                "partition_guid":"22222222-2222-4222-8222-222222222222",
                "first_lba":2048,"last_lba":1048575,
                "filesystem_uuid":"33333333-3333-4333-8333-333333333333","filesystem":"ext4"},
                "relative":super::super::migration_store::NativePath::encode(Path::new("home/person")),
                "inode":1234,"original_uid":1000,"subvolume":null},"flatpak_inventory":null
        });
        let mut plan: super::super::migration_handoff::Plan =
            serde_json::from_value(value).unwrap();
        let request = RetainedRequest::new(
            temp.path(),
            plan.clone(),
            channel.clone(),
            "owned-setup".into(),
            [1; 32],
        )
        .unwrap();
        let request_bytes = serde_json::to_vec(&request).unwrap();
        assert!(matches!(
            serde_json::from_slice::<DispatchRequest>(&request_bytes).unwrap(),
            DispatchRequest::Retained(_)
        ));
        for key in ["groups", "original", "primary_gid", "supplementary_gids"] {
            let mut changed = serde_json::to_value(&request).unwrap();
            changed[key] = serde_json::json!([1002, 1003]);
            assert!(
                serde_json::from_value::<DispatchRequest>(changed).is_err(),
                "{key} cannot select root credentials"
            );
        }
        plan.home_policy = super::super::migration_handoff::HomePolicy::Copy;
        assert!(
            RetainedRequest::new(
                temp.path(),
                plan.clone(),
                channel.clone(),
                "owned-setup".into(),
                [1; 32]
            )
            .is_err()
        );
        plan.home_policy = super::super::migration_handoff::HomePolicy::AccessReadWrite;
        assert!(
            OriginalRequest::new(temp.path(), plan, channel).is_err(),
            "retained purpose cannot enter readonly copy registration"
        );
        let held = publish_retained_request(temp.path(), &request).unwrap();
        assert_eq!(held.metadata().unwrap().len(), request_bytes.len() as u64);
        let parsed: DispatchRequest = serde_json::from_slice(
            &read_request_bytes(
                &temp.path().join("request.json"),
                rustix::process::getuid().as_raw(),
                MAX_ORIGINAL_REQUEST,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(parsed, DispatchRequest::Retained(_)));
    }
}

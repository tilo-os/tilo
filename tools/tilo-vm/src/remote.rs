//! How tilo-vm reaches a device: the action frames of the device's own daemon endpoint, signed
//! with this machine's device key, as every other device of the person asks it. Each step is one
//! action on the device's `devices/<device>/vm`, admitted by the grants of the person's own
//! workspace; bytes go into the device's content store before the step that names them.

use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, ContentPutAnswer, ContentPutStep, InvokeArgument,
    InvokeRequest, MAX_CONTENT_FRAME_BYTES, MAX_HAVE_HASHES, Pending, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionFailure, InvocationId, InvocationUpdate};
use tilo_device_protocol::{
    Call, RpcError, Status, StatusResult, TreeFile, TreeManifest, TreeSymlink,
};
use tilo_peer_state::actions::{ActionService, NoActions, REPLY_DEADLINE, RemoteDevice};

use crate::config::DeviceRoute;
use crate::error::CliError;

mod keepawake;

/// The provider a device answers its steps under.
pub const PROVIDER: &str = "so.tilo.devices";

/// The resource a device's steps act on.
pub fn resource(device: &str) -> String {
    format!("devices/{device}/vm")
}

/// The identity this machine asks as: its device key, certified by the person, and the person's
/// own workspace, whose grants decide what the device lets it do.
#[derive(Clone)]
pub struct Machine {
    pub key: SigningKey,
    pub user: [u8; 32],
    pub workspace: String,
    /// Where `tilo` keeps this identity: `None` for the person's own login, else the environment
    /// that scopes tilo-vm's own person ([`Machine::own`]).
    pub scope: Option<PathBuf>,
}

impl std::fmt::Debug for Machine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Machine")
            .field("device", &hex::encode(self.key.verifying_key().to_bytes()))
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// The `tilo` command tilo-vm asks: the one built beside this tilo-vm when there is one (a
/// checkout's build, or CI's tilo-vm with the command it built), else the one on `PATH`.
pub(crate) fn tilo_program() -> PathBuf {
    beside_or_on_path("tilo")
}

/// The program `name` built beside this tilo-vm, else the one on `PATH`.
fn beside_or_on_path(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(name)))
        .filter(|beside| beside.is_file())
        .unwrap_or_else(|| name.into())
}

impl Machine {
    /// The person's own login on this machine (`tilo login`), or the person whose root it holds;
    /// `None` when it has neither.
    pub fn here() -> Result<Option<Self>, CliError> {
        let keys = tilo_identity::provisioning::collaboration_identity_paths();
        let place =
            tilo_device_identity::login::Place::here(keys).map_err(|error| login_error(&error))?;
        Self::at(&place, None)
    }

    /// The throwaway person tilo-vm makes for itself under `dir` when this machine has none (CI,
    /// a fresh builder), and the environment that scopes `tilo` to it.
    pub fn own(dir: &Path) -> Result<Option<Self>, CliError> {
        let place = tilo_device_identity::login::Place::at(
            dir.join("state/tilo/device"),
            tilo_device_identity::IdentityPaths::under(dir.join("identity")),
        );
        Self::at(&place, Some(dir.to_path_buf()))
    }

    /// The environment `tilo` runs in for this identity.
    pub fn env(&self) -> Vec<(String, String)> {
        self.scope.as_deref().map(own_env).unwrap_or_default()
    }

    fn at(
        place: &tilo_device_identity::login::Place,
        scope: Option<PathBuf>,
    ) -> Result<Option<Self>, CliError> {
        // A computer a ticket logged in acts in its person's own workspace, as the certificate the
        // login filed names them.
        if let Some(device) = tilo_device_identity::login::served_device_key(place.keys())
            && let Some(cert) = tilo_device_identity::login::ticket_cert(place.keys(), &device)
        {
            let key = tilo_device_identity::persist::load_device_key(place.keys())
                .map_err(|error| login_error(&anyhow::anyhow!("{error}")))?;
            return Ok(Some(Self {
                key,
                user: cert.user_public_key,
                workspace: tilo_server_protocol::servers::person_workspace_id(
                    &cert.user_public_key,
                ),
                scope,
            }));
        }
        let Ok(user) = tilo_device_identity::persist::read_public_key(&place.keys().user_pubkey)
        else {
            return Ok(None);
        };
        // tilo-vm's own person is served by a daemon of its own, which runs only while asked.
        let _daemon = scope.as_deref().map(OwnDaemon::start).transpose()?;
        let workspace = founded(scope.as_deref().map(own_env).unwrap_or_default())?;
        let key = tilo_device_identity::persist::load_device_key(place.keys())
            .map_err(|error| login_error(&anyhow::anyhow!("{error}")))?;
        Ok(Some(Self {
            key,
            user,
            workspace,
            scope,
        }))
    }
}

/// The workspace the daemon `tilo` reaches in `env` founded for its person, as it answers
/// `tilo device key` with no login.
fn founded(env: Vec<(String, String)>) -> Result<String, CliError> {
    let output = std::process::Command::new(tilo_program())
        .args(["device", "key"])
        .envs(env)
        .output()
        .map_err(CliError::io("run tilo device key"))?;
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .ok()
        .and_then(|answer| answer.get("workspace")?.as_str().map(str::to_owned))
        .ok_or_else(|| CliError::CommandFailed {
            command: "tilo device key".into(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
}

/// The environment that keeps `tilo` to tilo-vm's own person under `dir`: its identity, its
/// state and the profile of its daemon ([`OwnDaemon`]), whose native endpoint every `tilo` run in
/// it asks, so none installs or starts this account's own service.
pub fn own_env(dir: &Path) -> Vec<(String, String)> {
    [
        ("TILO_IDENTITY_ROOT", "identity"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("TILO_RUNTIME_PROFILE", OWN_PROFILE),
        ("HOME", OWN_HOME),
    ]
    .into_iter()
    .map(|(name, sub)| (name.to_owned(), dir.join(sub).display().to_string()))
    .collect()
}

/// Where under tilo-vm's own person's directory its daemon keeps its profile and sockets.
const OWN_PROFILE: &str = "profile";
/// The home its daemon keeps the person's workspace folders in, apart from this account's.
const OWN_HOME: &str = "home";
/// How long tilo-vm's own daemon has to answer at its native endpoint.
const OWN_DAEMON_START: Duration = Duration::from_secs(60);
/// How long it has to stop once told to.
const OWN_DAEMON_STOP: Duration = Duration::from_secs(10);

/// The daemon serving tilo-vm's own person: `tilo-runtime` over a private profile under the
/// person's directory, a child of tilo-vm while tilo-vm needs it (founding the person, a VM
/// joining it), stopped when dropped. It is never this account's service, so an account with no
/// user manager (CI's, reached through `sudo`) runs it too, and the account's own Tilo daemon,
/// when it has one, is left alone.
pub struct OwnDaemon {
    /// `None` when a daemon answered already (another tilo-vm's).
    child: Option<std::process::Child>,
}

impl OwnDaemon {
    /// The daemon of the person under `dir`, started when none answers, once it answers.
    pub fn start(dir: &Path) -> Result<Self, CliError> {
        let profile = dir.join(OWN_PROFILE);
        let socket = profile.join("native.sock");
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return Ok(Self { child: None });
        }
        for private in [&profile, &dir.join(OWN_HOME)] {
            make_private_dir(private)?;
        }
        let log_path = dir.join("daemon.log");
        let log = std::fs::File::create(&log_path)
            .map_err(CliError::io(format!("create {}", log_path.display())))?;
        let program = beside_or_on_path("tilo-runtime");
        let child = std::process::Command::new(&program)
            .arg("--profile")
            .arg(&profile)
            .envs(own_env(dir))
            .stdin(std::process::Stdio::null())
            .stdout(
                log.try_clone()
                    .map_err(CliError::io(format!("open {}", log_path.display())))?,
            )
            .stderr(log)
            .spawn()
            .map_err(CliError::io(format!("run {}", program.display())))?;
        let mut daemon = Self { child: Some(child) };
        let deadline = std::time::Instant::now() + OWN_DAEMON_START;
        loop {
            if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                return Ok(daemon);
            }
            let exited = daemon
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten());
            if exited.is_some() || std::time::Instant::now() >= deadline {
                return Err(CliError::CommandFailed {
                    command: format!("{} --profile {}", program.display(), profile.display()),
                    status: exited.map_or_else(
                        || format!("no answer within {}s", OWN_DAEMON_START.as_secs()),
                        |status| status.to_string(),
                    ),
                    stderr: last_lines(&log_path, 20),
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for OwnDaemon {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // Told to stop, the daemon lets go of the device's lock and its endpoints itself.
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let deadline = std::time::Instant::now() + OWN_DAEMON_STOP;
        while std::time::Instant::now() < deadline {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Make `dir` and its parents, and keep it to this account alone.
fn make_private_dir(dir: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)))
        .map_err(CliError::io(format!("make {}", dir.display())))
}

/// The last `count` lines of the file at `path`, for an error that says why a program stopped.
fn last_lines(path: &Path, count: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

fn login_error(error: &anyhow::Error) -> CliError {
    CliError::NotLoggedIn {
        detail: error.to_string(),
    }
}

/// One device, asked over its daemon's action frames.
pub struct Remote {
    vm: String,
    device: Arc<RemoteDevice>,
    caller: Caller,
    resource: String,
    /// The endpoint the connection goes out on: it lives as long as the device is asked.
    endpoint: iroh::Endpoint,
    /// What the device answered `status` with when it was reached: its version, architecture,
    /// tools, accounts and boot.
    pub status: StatusResult,
}

impl Remote {
    /// Reach `route`'s device as `machine` and ask its `status`, which is also how an unreachable
    /// or refusing device shows.
    pub fn connect(vm: &str, machine: &Machine, route: &DeviceRoute) -> Result<Self, CliError> {
        let device: [u8; 32] = hex::decode(&route.device)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| CliError::Config {
                path: vm.into(),
                detail: format!("{} is not a device key", route.device),
            })?;
        let mut addr =
            iroh::EndpointAddr::new(iroh::EndpointId::from_bytes(&device).map_err(|error| {
                CliError::Config {
                    path: vm.into(),
                    detail: error.to_string(),
                }
            })?);
        for address in &route.addresses {
            if let Ok(socket) = address.parse() {
                addr = addr.with_ip_addr(socket);
            }
        }
        let relay = route.relay.as_deref().and_then(|url| url.parse().ok());
        let runtime = tilo_rt::app().handle();
        let _entered = runtime.enter();
        let server = tilo_server_client::ServerAddress::through(addr, relay);
        let endpoint = runtime
            .block_on(server.bind(Some(machine.key.to_bytes())))
            .map_err(|error| CliError::Unreachable {
                vm: vm.into(),
                detail: error.to_string(),
                debug: vec![],
            })?;
        let service = ActionService::new(machine.key.clone(), Arc::new(NoActions));
        let device = Arc::new(service.remote(&endpoint, &server.addr(), &route.workspace));
        let mut remote = Self {
            vm: vm.into(),
            device,
            caller: Caller::authenticated(machine.user, machine.key.verifying_key().to_bytes()),
            resource: resource(&route.device),
            endpoint,
            status: unanswered(),
        };
        remote.status = remote.call(Status { account: None })?;
        Ok(remote)
    }

    /// Ask `status` again, after the device changed.
    pub fn refresh(&mut self) -> Result<&StatusResult, CliError> {
        self.status = self.call(Status { account: None })?;
        Ok(&self.status)
    }

    /// Run one step and wait for what it completes with; its progress goes to stderr as it comes.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "callers build the step in the call, as they would an action's arguments"
    )]
    pub fn call<C: Call>(&mut self, params: C) -> Result<C::Output, CliError> {
        let serde_json::Value::Object(argument) = serde_json::to_value(&params)
            .map_err(|error| CliError::InvalidInput(error.to_string()))?
        else {
            return Err(CliError::InvalidInput(format!(
                "{} takes an object",
                C::ACTION
            )));
        };
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.into(),
                address: ActionAddress::new(&self.resource, C::ACTION),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        match settle(&self.device, &self.caller, request)
            .map_err(|error| self.refused(C::ACTION, error))?
        {
            InvocationUpdate::Succeeded { output, .. } => {
                serde_json::from_value(output.value.unwrap_or_default()).map_err(|error| {
                    CliError::CommandFailed {
                        command: C::ACTION.into(),
                        status: "unreadable answer".into(),
                        stderr: error.to_string(),
                    }
                })
            }
            InvocationUpdate::Failed { error, .. } => Err(CliError::Remote(failure(error))),
            InvocationUpdate::Cancelled { message, .. } => Err(CliError::CommandFailed {
                command: C::ACTION.into(),
                status: "cancelled".into(),
                stderr: message.unwrap_or_default(),
            }),
            _ => Err(self.refused(C::ACTION, ActionCallError::Unanswered)),
        }
    }

    /// Why a step never ran, as the person reads it.
    fn refused(&self, action: &str, error: ActionCallError) -> CliError {
        match error {
            ActionCallError::UnknownAction => CliError::DeviceTooOld {
                vm: self.vm.clone(),
                action: action.into(),
            },
            ActionCallError::NotPermitted => CliError::NotGranted {
                vm: self.vm.clone(),
                resource: self.resource.clone(),
            },
            other => CliError::Unreachable {
                vm: self.vm.clone(),
                detail: other.to_string(),
                debug: vec![],
            },
        }
    }

    fn put_step(&self, step: ContentPutStep) -> Result<ContentPutAnswer, CliError> {
        answered(self.device.content_put(step)).map_err(|error| self.refused("content", error))
    }

    /// Which of `hashes` the device already holds.
    fn have(&self, hashes: &[String]) -> Result<Vec<bool>, CliError> {
        let mut present = Vec::with_capacity(hashes.len());
        for batch in hashes.chunks(MAX_HAVE_HASHES) {
            match self.put_step(ContentPutStep::Have {
                hashes: batch.to_vec(),
            })? {
                ContentPutAnswer::Have { present: held } if held.len() == batch.len() => {
                    present.extend(held);
                }
                _ => return Err(unexpected("have")),
            }
        }
        Ok(present)
    }

    /// Put `bytes` into the device's content store unless it holds them already, and name them by
    /// their blake3 hex.
    pub fn put(&mut self, bytes: &[u8]) -> Result<String, CliError> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        if self.have(std::slice::from_ref(&hash))?.first() != Some(&true) {
            self.send(len(bytes.len()), bytes, &hash)?;
        }
        Ok(hash)
    }

    /// [`Self::put`] for what `reader` yields, read whole: a layer tar is one blob on the device.
    pub fn put_reader(&mut self, mut reader: impl Read) -> Result<String, CliError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(CliError::io("read the layer"))?;
        self.put(&bytes)
    }

    /// Send `size` bytes from `reader`, which hash to `hash`, in chunks the device takes.
    fn send(&self, size: u64, mut reader: impl Read, hash: &str) -> Result<(), CliError> {
        let ContentPutAnswer::Begun { upload } = self.put_step(ContentPutStep::Begin { size })?
        else {
            return Err(unexpected("begin"));
        };
        // One append is one signed frame between devices, so a chunk is what one frame carries.
        let mut chunk = vec![0; MAX_CONTENT_FRAME_BYTES];
        let mut offset = 0_u64;
        loop {
            let read = reader
                .read(&mut chunk)
                .map_err(CliError::io("read what is sent"))?;
            if read == 0 {
                break;
            }
            let piece = chunk.get(..read).unwrap_or_default();
            self.put_step(ContentPutStep::append(upload, offset, piece))?;
            offset = offset.saturating_add(len(read));
        }
        match self.put_step(ContentPutStep::Finish { upload })? {
            ContentPutAnswer::Finished { content } if content.ends_with(hash) => Ok(()),
            _ => Err(unexpected("finish")),
        }
    }

    /// Put every file of `files` under `root` the device does not hold yet, then the manifest
    /// that names them all by hash. Files are read as they are hashed and sent, never held whole
    /// together, so an image layout of many gigabytes goes as well as a worktree. Returns the
    /// manifest's hash and how many bytes went.
    pub fn put_tree(&mut self, root: &Path, files: &[PathBuf]) -> Result<(String, u64), CliError> {
        let mut manifest = TreeManifest::default();
        let mut sizes = Vec::new();
        for relative in files {
            let path = root.join(relative);
            let meta = std::fs::symlink_metadata(&path)
                .map_err(CliError::io(format!("read {}", path.display())))?;
            let shown = relative.to_string_lossy().replace('\\', "/");
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path)
                    .map_err(CliError::io(format!("read {}", path.display())))?;
                manifest.symlinks.push(TreeSymlink {
                    path: shown,
                    target: target.to_string_lossy().into_owned(),
                });
            } else if meta.is_file() {
                manifest.entries.push(TreeFile {
                    path: shown,
                    mode: mode(&meta),
                    hash: hash_file(&path)?,
                });
                sizes.push((path, meta.len()));
            }
        }
        let hashes: Vec<String> = manifest.entries.iter().map(|e| e.hash.clone()).collect();
        let held = self.have(&hashes)?;
        let mut sent = 0_u64;
        let mut seen = std::collections::BTreeSet::new();
        for (((path, size), hash), held) in sizes.iter().zip(&hashes).zip(held) {
            if !held && seen.insert(hash) {
                let file = std::fs::File::open(path)
                    .map_err(CliError::io(format!("read {}", path.display())))?;
                self.send(*size, file, hash)?;
                sent = sent.saturating_add(*size);
            }
        }
        let listed =
            serde_json::to_vec(&manifest).map_err(|e| CliError::InvalidInput(e.to_string()))?;
        Ok((self.put(&listed)?, sent))
    }

    /// Lay `root`'s tracked and untracked files out on the device as worktree `name`: the files
    /// and their manifest ([`Self::put_tree`]), then `tree-sync`, which also deletes what the
    /// manifest no longer names.
    pub fn sync_tree(
        &mut self,
        name: &str,
        root: &Path,
        files: &[PathBuf],
    ) -> Result<tilo_device_protocol::TreeSyncResult, CliError> {
        let (manifest, _) = self.put_tree(root, files)?;
        self.call(tilo_device_protocol::TreeSync {
            worktree: name.into(),
            manifest,
        })
    }

    /// Run `work` while the device is held awake: the `awake` step renews its sleep lease every
    /// minute, so a long build on this machine does not find the device asleep after.
    pub(crate) fn keep_awake_while<T>(
        &self,
        work: impl FnOnce() -> T,
    ) -> Result<(T, Result<(), CliError>), CliError> {
        self.keep_awake_every(Duration::from_secs(60), work)
    }

    fn keep_awake_every<T>(
        &self,
        interval: Duration,
        work: impl FnOnce() -> T,
    ) -> Result<(T, Result<(), CliError>), CliError> {
        let renewing = Self {
            vm: self.vm.clone(),
            device: Arc::clone(&self.device),
            caller: self.caller.clone(),
            endpoint: self.endpoint.clone(),
            resource: self.resource.clone(),
            status: self.status.clone(),
        };
        let renewing = std::sync::Mutex::new(renewing);
        keepawake::during(
            interval,
            || {
                let lease = renewing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .call(tilo_device_protocol::Awake::default())?;
                if lease.held {
                    Ok(())
                } else {
                    Err(CliError::io("renew the device sleep lease")(
                        std::io::Error::other(
                            lease
                                .problem
                                .unwrap_or_else(|| "the sleep lock is not held".into()),
                        ),
                    ))
                }
            },
            work,
        )
    }
}

/// The invocation's terminal update. Steps run as long as they need (a build takes many
/// minutes), so the invocation is followed until it ends, printing each progress line the device
/// reports.
fn settle(
    device: &RemoteDevice,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<InvocationUpdate, ActionCallError> {
    let id = request.invocation;
    let receipt = answered(device.invoke(caller, request))?;
    match receipt.status {
        ReceiptStatus::Accepted => {}
        ReceiptStatus::Completed { output } => {
            return Ok(InvocationUpdate::Succeeded {
                invocation: id,
                output,
            });
        }
        ReceiptStatus::Failed { error } => {
            return Ok(InvocationUpdate::Failed {
                invocation: id,
                error,
            });
        }
        ReceiptStatus::Cancelled { message } => {
            return Ok(InvocationUpdate::Cancelled {
                invocation: id,
                message,
            });
        }
        ReceiptStatus::Rejected { error } => return Err(error),
    }
    let mut printed = 0;
    loop {
        match device.next_change(caller, &id) {
            Some(change) => drop(change.wait_timeout(REPLY_DEADLINE)),
            None => std::thread::sleep(Duration::from_millis(500)),
        }
        let state = device
            .observe(caller, &id)
            .ok_or(ActionCallError::UnknownInvocation)?;
        match state.update {
            InvocationUpdate::Progress {
                completed: Some(completed),
                message: Some(message),
                ..
            } if completed > printed => {
                let mut stderr = std::io::stderr().lock();
                let _ = stderr.write_all(unprinted(&message, completed, printed));
                let _ = stderr.flush();
                printed = completed;
            }
            update if state.is_terminal() => return Ok(update),
            _ => {}
        }
    }
}

/// The bytes of a progress `message` (the last of the `completed` bytes the step wrote) past the
/// `printed` ones already shown; all of it once the output ran further ahead than the message
/// reaches back.
fn unprinted(message: &str, completed: u64, printed: u64) -> &[u8] {
    let start = completed.saturating_sub(len(message.len()));
    let skip = usize::try_from(printed.saturating_sub(start)).unwrap_or(usize::MAX);
    message.as_bytes().get(skip..).unwrap_or_default()
}

/// A frame's answer, or [`ActionCallError::Unanswered`] once the reply deadline passes.
fn answered<T>(pending: Pending<Result<T, ActionCallError>>) -> Result<T, ActionCallError> {
    match pending.wait_timeout(REPLY_DEADLINE) {
        Ok(Some(answer)) => answer,
        Ok(None) | Err(_) => Err(ActionCallError::Unanswered),
    }
}

#[cfg(unix)]
fn mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o7777
}

/// Files from a filesystem without unix modes land as plain readable files.
#[cfg(not(unix))]
fn mode(_: &std::fs::Metadata) -> u32 {
    0o644
}

fn len(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// A file's blake3 hex, read in pieces.
fn hash_file(path: &Path) -> Result<String, CliError> {
    let mut hasher = blake3::Hasher::new();
    let file =
        std::fs::File::open(path).map_err(CliError::io(format!("read {}", path.display())))?;
    hasher
        .update_reader(file)
        .map_err(CliError::io(format!("read {}", path.display())))?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// The `status` of a device not asked yet.
fn unanswered() -> StatusResult {
    StatusResult {
        daemon_version: String::new(),
        arch: String::new(),
        base: tilo_device_protocol::BaseInfo::default(),
        tools: tilo_device_protocol::VmTools::default(),
        boot: tilo_device_protocol::image::BootFacts::default(),
        accounts: Vec::new(),
        seat0: None,
        storage: tilo_device_protocol::Storage::default(),
        issues: Vec::new(),
        image_change: None,
        accounts_image: None,
    }
}

fn unexpected(step: &str) -> CliError {
    CliError::CommandFailed {
        command: format!("content {step}"),
        status: "unexpected answer".into(),
        stderr: String::new(),
    }
}

/// A step's failure, with the code and hint the device gave it.
fn failure(error: ActionFailure) -> RpcError {
    let ActionFailure {
        details, message, ..
    } = error;
    details
        .and_then(|details| serde_json::from_value(details).ok())
        .unwrap_or_else(|| RpcError {
            code: tilo_device_protocol::code::JOB,
            message,
            hint: "the device's message above names the cause".into(),
            see: None,
        })
}

#[cfg(test)]
pub(crate) mod fixture;

#[cfg(test)]
mod tests;

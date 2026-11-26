//! The supervisor: authenticated control and individually leased shell sessions.
//!
//! It runs on the host in the user's session, outside every app sandbox: the only thing
//! here that holds `unshare` and `mount`, and it hands none of that to the guest (see the
//! trust boundary in [`crate::lib`](crate)). It resolves the record and its images, makes
//! the guest's network namespace and filter, starts the broker, then spawns `bwrap` and
//! restarts it if asked.
//!
//! An app cannot build a namespace, so `apps/terminal` asks for a guest instead:
//! `tilo-ush serve` listens on a socket `tilo-launcher` passes every app, and one line of
//! JSON naming a [`Target`] gets back a pty master over `SCM_RIGHTS` — a terminal, never a
//! namespace or a capability.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::FromRawFd;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use crate::home::seed_guest_home;
use tilo_clock::unix_now;
use tilo_egress::{Egress, EgressOptions, Netns, spawn_egress};
use tilo_environment::{Environment, Target, ToolBuilder, is_record_key};

use crate::audit::AuditLog;
use crate::broker::{Broker, BrokerState, EnvironmentHandle};
use crate::guest::{GuestPlan, GuestUser};
use crate::images::Images;
use crate::policy::Policy;
use crate::records::Records;
use crate::store::Store;

mod hosted_terminal;
mod session_end;
use hosted_terminal::serve_hosted;
use session_end::{EndReport, EndWatch, GuestEnd, watch_setup};

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub environment: Option<String>,
    pub command: Vec<String>,
    pub term: String,
    pub host_root: PathBuf,
    pub verbose: bool,
    pub marks_key: Option<String>,
    pub editor: Option<Arc<runtime_portal::editor::Socket>>,
    pub terminal: Option<Arc<OwnedFd>>,
    pub mcp_server: Option<PathBuf>,
    /// On a host that serves other people's workspaces: the workspace's
    /// own VM runs the session's programs, not a guest on this kernel
    /// ([`crate::hosted_vm`]). Set from the host's environment.
    pub hosted_vm: Option<crate::hosted_vm::HostedTarget>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            environment: None,
            command: Vec::new(),
            term: std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
            host_root: PathBuf::from("/"),
            verbose: false,
            marks_key: std::env::var(crate::command_marks::KEY_ENV)
                .ok()
                .filter(|key| crate::command_marks::valid_key(key)),
            editor: None,
            terminal: None,
            mcp_server: None,
            hosted_vm: crate::hosted_vm::HostedTarget::from_env(),
        }
    }
}

const SANDBOX_SETUP_FAILED: &str = "\
ush: the shell's sandbox could not be set up, so no shell started;
ush: the setup error is above.";

/// The first line of every reason a session's sandbox was not built.
const NOT_STARTED: &str = "ush: the shell's sandbox could not be set up, so no shell started";

/// How this machine wires a guest's egress. The resolvers and `pasta` come
/// from the machine; three variables exist for the integration tests, which
/// fake the outside world on loopback:
///
/// - `TILO_USH_RESOLVER` — resolvers (`ip:port`, comma-separated) instead of
///   the machine's `resolv.conf`;
/// - `TILO_USH_EGRESS_MAP_HOST_LOOPBACK` — an address the guest reaches the
///   host's loopback at;
/// - `TILO_USH_EGRESS_REFUSE_NETFILTER=1` — act as if the kernel refused the
///   packet filter, to prove the session then does not start.
pub fn egress_options(host_root: &Path) -> EgressOptions {
    let mut options = EgressOptions::from_host(host_root);
    if let Some(list) = std::env::var_os("TILO_USH_RESOLVER") {
        options.upstream = list
            .to_string_lossy()
            .split(',')
            .filter_map(|a| a.trim().parse().ok())
            .collect();
    }
    if let Some(addr) = std::env::var_os("TILO_USH_EGRESS_MAP_HOST_LOOPBACK") {
        options.map_host_loopback = addr.to_string_lossy().parse().ok();
    }
    options.refuse_netfilter =
        std::env::var_os("TILO_USH_EGRESS_REFUSE_NETFILTER").is_some_and(|v| v == "1");
    options
}

/// Where the runtime's own binary is, as the supervisor sees it. `current_exe` rather
/// than a constant: the guest must run the same build that built its confinement, which
/// in a dev tree is under `target/`.
///
/// # Errors
///
/// When `TILO_USH_BIN` is unset and [`std::env::current_exe`] cannot resolve the running
/// binary.
pub fn runtime_binary() -> std::io::Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("TILO_USH_BIN") {
        return Ok(PathBuf::from(explicit));
    }
    std::env::current_exe()
}

pub struct Session {
    pub store: Store,
    pub opts: SessionOptions,
    pub session_id: String,
    pub images: Arc<dyn Images>,
    pub builder: Arc<dyn ToolBuilder>,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub key: String,
    pub environment: Environment,
    pub images: Vec<(String, PathBuf)>,
}

impl Session {
    pub fn new(
        store: Store,
        opts: SessionOptions,
        images: Arc<dyn Images>,
        builder: Arc<dyn ToolBuilder>,
    ) -> Self {
        Self {
            store,
            opts,
            session_id: uuid::Uuid::new_v4().simple().to_string(),
            images,
            builder,
        }
    }

    pub(crate) fn records(&self) -> Records {
        Records::of(&self.store)
    }

    pub(crate) fn environment_id(&self, key: &str) -> String {
        format!("{}/{key}", self.store.workspace_id())
    }

    pub(crate) fn workspace_env(&self, key: &str) -> Vec<(String, String)> {
        vec![
            (
                crate::workspace::WORKSPACE_ENV.to_owned(),
                self.store.workspace_id().to_string(),
            ),
            (
                crate::workspace::ENVIRONMENT_ENV.to_owned(),
                self.environment_id(key),
            ),
        ]
    }

    /// The environment this session names, as the record stands now, with
    /// every image resolved to its tree.
    ///
    /// # Errors
    ///
    /// Returns an error when the record cannot be resolved (see
    /// [`Records::resolve`]) or an image is not available.
    pub fn resolve(&self) -> anyhow::Result<Resolved> {
        let (key, environment) = self.resolve_record()?;
        let arch = tilo_environment::host_arch();
        let runtime = environment
            .runtime_image(arch)
            .map_err(anyhow::Error::msg)?;
        let mut images = Vec::new();
        let mut environment = environment;
        let tools = environment.tools.clone();
        for tool in tools.iter().rev() {
            let label = format!("tool ({})", tool.packages.join(", "));
            let id = match self.tool_image(tool, &runtime) {
                Ok(id) => id,
                Err(missing) => {
                    // No usable image for this architecture yet: build it here,
                    // for the same packages on this node's image of the
                    // runtime, and record it so the next session is a hit.
                    eprintln!("ush: {missing}; building it for {arch}…");
                    let id = self
                        .builder
                        .build(&runtime, &tool.packages, &mut std::io::stderr())
                        .map_err(|e| anyhow::anyhow!("{missing}, and {e}"))?;
                    environment = self.records().apply(
                        &key,
                        Environment::ADD_TOOL.with((
                            tool.packages.clone(),
                            arch.to_owned(),
                            id.to_string(),
                        )),
                    )?;
                    id
                }
            };
            let tree = self.images.tree(&id).map_err(anyhow::Error::msg)?;
            images.push((format!("{label} {id}"), tree));
        }
        if !self.images.is_local(&runtime) {
            // Stderr is the session's terminal: the person sees why there is
            // no prompt yet, instead of a pane that only echoes their keys.
            eprintln!("ush: preparing this environment's runtime for its first use…");
        }
        let tree = self.images.tree(&runtime).map_err(anyhow::Error::msg)?;
        images.push((
            format!("runtime {} {runtime}", environment.runtime.release),
            tree,
        ));
        Ok(Resolved {
            key,
            environment,
            images,
        })
    }

    fn tool_image(
        &self,
        tool: &tilo_environment::ToolRef,
        runtime: &tilo_environment::PackageId,
    ) -> Result<tilo_environment::PackageId, String> {
        let arch = tilo_environment::host_arch();
        let id = tool
            .image(arch)?
            .ok_or_else(|| format!("{} has no image for {arch}", tool.packages.join(", ")))?;
        self.images.check_tool(&id, runtime)?;
        self.images.tree(&id)?;
        Ok(id)
    }

    /// Build the plan for this session: everything the guest will be is decided
    /// here, from the record and the store — never from anything the guest said.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime binary's own path cannot be resolved
    /// (see [`runtime_binary`]).
    pub fn plan(
        &self,
        resolved: &Resolved,
        broker_socket: PathBuf,
        seccomp_blob: PathBuf,
    ) -> std::io::Result<GuestPlan> {
        let policy = Policy::new(resolved.environment.grants.clone());
        let now = Some(unix_now());
        let session_dir = self.store.session_dir(&self.session_id);
        let user = GuestUser::current();
        let (home, guest_home, home_by_grants) =
            crate::home::guest_home(resolved, &self.store, &session_dir, &user);
        let images: Vec<PathBuf> = resolved.images.iter().map(|(_, p)| p.clone()).collect();
        // Resolved, as a grant names its directory: `bwrap` mounts through
        // the real path, and a grant of the home must still find the store
        // under it.
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let runtime_bin = runtime_binary()?;
        let helper_runtime = crate::helper_runtime::HelperRuntime::resolve(&runtime_bin)?;
        Ok(GuestPlan {
            shell: crate::shell_host::guest_shell_path(),
            images,
            host_root: self.opts.host_root.clone(),
            session_dir,
            var_dir: self.store.environment_var(&resolved.key),
            home,
            guest_home,
            home_by_grants,
            user,
            environment: resolved.environment.name.clone(),
            masked: vec![canonical(self.store.root())],
            trusted_dirs: policy.trusted_dirs(now),
            readable_dirs: policy.readable_dirs(now),
            grant_root: self.store.grant_root().map(Path::to_path_buf),
            runtime_bin,
            helper_runtime,
            linux_vm_commands: crate::linux_vm_commands(),
            broker_socket,
            seccomp_blob,
            command: self.opts.command.clone(),
            term: self.opts.term.clone(),
            env: self.workspace_env(&resolved.key),
            resolver: None,
            terminal: None,
            control: None,
            mcp_server: self.opts.mcp_server.clone(),
        })
    }

    /// The guest's network: a namespace of its own and egress for the
    /// record's `NetHost` grants. Fails closed — a policy that cannot be
    /// enforced is an error, never an open network.
    pub fn network(&self, resolved: &Resolved) -> anyhow::Result<(Netns, Egress)> {
        let netns = Netns::create()
            .map_err(|e| anyhow::anyhow!("{NOT_STARTED}:\nush: its network namespace: {e}"))?;
        let policy = Policy::new(resolved.environment.grants.clone()).egress(Some(unix_now()));
        if self.opts.verbose {
            eprintln!("ush: network: {}", describe_policy(&policy));
        }
        let egress = spawn_egress(&netns, policy, &egress_options(&self.opts.host_root))
            .map_err(|e| anyhow::anyhow!("{NOT_STARTED}:\nush: {e}"))?;
        Ok((netns, egress))
    }

    pub fn run(&mut self) -> anyhow::Result<i32> {
        if self.opts.command.is_empty() {
            return self.run_tilo_side(None);
        }
        self.store.ensure()?;
        let resolved = self.resolve()?;
        let socket = crate::mcp_server::open_for(
            &self.store,
            &self.environment_id(&resolved.key),
            &resolved.environment,
        );
        self.opts.mcp_server = socket.as_ref().map(|socket| socket.path().to_path_buf());
        let session_dir = self.store.session_dir(&self.session_id);
        std::fs::create_dir_all(&session_dir)?;
        let outcome = self.run_guest(&resolved, &session_dir);
        drop(socket);
        crate::guest::remove_dir_all_forced(&session_dir);
        outcome
    }

    pub(crate) fn broker_state(
        &self,
        resolved: &Resolved,
        egress: Option<Arc<tilo_egress::Egress>>,
    ) -> BrokerState {
        BrokerState {
            session_id: self.session_id.clone(),
            environment: EnvironmentHandle {
                var_dir: self.store.environment_var(&resolved.key),
                records: self.records(),
                key: resolved.key.clone(),
                built_from: resolved.environment.clone(),
            },
            audit: AuditLog::at(self.store.audit_dir()),
            restart_requested: false,
            host_root: self.opts.host_root.clone(),
            images: resolved.images.clone(),
            builder: Arc::clone(&self.builder),
            landlock: "installed by the guest at startup".into(),
            egress,
            person: crate::host::signed_in_person(),
            device: crate::host::this_device(),
        }
    }

    fn run_guest(&self, resolved: &Resolved, session_dir: &Path) -> anyhow::Result<i32> {
        // Before anything else: a guest whose network cannot be confined as
        // its record says does not start.
        let (netns, egress) = self.network(resolved)?;
        let egress = Arc::new(egress);
        let seccomp_blob = crate::seccomp::write_guest_filter(session_dir)?;
        let broker_socket = session_dir.join("broker.sock");
        let broker = Broker::start(
            broker_socket.clone(),
            self.broker_state(resolved, Some(Arc::clone(&egress))),
        )?;

        let mut plan = self.plan(resolved, broker_socket, seccomp_blob)?;
        plan.resolver = Some(egress.resolv_conf().to_owned());
        plan.prepare()?;
        if !plan.home_by_grants {
            seed_guest_home(&plan.guest_home)?;
        }

        if self.opts.verbose {
            let argv = crate::guest::build_argv(&plan, None);
            eprintln!(
                "ush: {}",
                argv.iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }

        // The guest's first process is this binary again, so it can install Landlock
        // inside the namespace — using the paths the guest itself will use.
        let user_command = std::mem::take(&mut plan.command);
        let mut command = plan.helper_runtime.as_ref().map_or_else(
            || vec![format!("{}/tilo-ush", crate::GUEST_BIN_DIR)],
            crate::helper_runtime::HelperRuntime::entry,
        );
        command.push("guest-init".into());
        if self.opts.terminal.is_some() {
            command.extend([
                crate::setup::TERMINAL_FD_ARG.to_owned(),
                crate::setup::TERMINAL_FD.to_string(),
            ]);
        }
        command.push("--".into());
        command.extend(user_command);
        plan.command = command;
        plan.terminal.clone_from(&self.opts.terminal);

        let mut child = crate::guest::spawn(&plan, Some(&netns))?;
        let status = child.wait()?;
        let code = status.code().unwrap_or(1);
        if code != 0 && !broker.guest_connected() {
            if self.opts.terminal.is_some() {
                anyhow::bail!("{NOT_STARTED}");
            }
            eprintln!("{SANDBOX_SETUP_FAILED}");
        }
        drop(broker);
        drop(egress);
        drop(netns);
        Ok(code)
    }
}

fn describe_policy(policy: &tilo_egress::EgressPolicy) -> String {
    match policy {
        tilo_egress::EgressPolicy::Any => "unrestricted (a * grant)".into(),
        tilo_egress::EgressPolicy::Hosts(hosts) if hosts.is_empty() => {
            "none (no host is granted)".into()
        }
        p @ tilo_egress::EgressPolicy::Hosts(_) => format!("only {}", p.describe().join(", ")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ServeRequest {
    OpenSession {
        #[serde(default)]
        target: Target,
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
        #[serde(default)]
        term: Option<String>,
        /// A program to run in the guest instead of the interactive shell.
        /// It runs with exactly the guest's confinement, so asking for one
        /// grants nothing a user typing it at the prompt would not have.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command: Vec<String>,
        /// Who asked the app for this session. Agents never reach
        /// [`Target::Host`].
        #[serde(default)]
        requested_by: crate::host::RequestedBy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editor_protocol: Option<u8>,
        /// Explicit opt-in: before a session's lease closes, tell the app on
        /// it how the session ended (`EndReport`): its shell exited, or its
        /// guest could not be set up. Existing clients get a lease that only
        /// closes.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        end_report: bool,
        /// The Terminal session record the window shows, by its route: in a workspace a
        /// server hosts, the session whose shell runs there.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<String>,
        /// The session where a hosted workspace's shell runs to come back to, by the id an
        /// earlier open answered ([`ServeResponse::Session::host_session`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_session: Option<String>,
    },
    ListEnvironments,
    /// How long the person allows Terminal's Host target, `None` once they
    /// took it back: the session hands their answer whenever it changes. Only
    /// a caller outside every app sandbox — the session — may hand it.
    AllowHost {
        expiry: Option<tilo_action_policy::Lifetime>,
    },
    CreateEnvironment {
        name: String,
    },
    EnvironmentAction {
        record: String,
        action: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    /// Branch the environments of the request's workspace into the workspace
    /// `into`, a branch of it the person made ([`crate::branch`]). Only the
    /// session may ask: an app names no workspace but its own.
    BranchWorkspace {
        into: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentEntry {
    pub key: String,
    pub name: String,
    pub tools: Vec<String>,
    pub grants: Vec<(String, bool)>,
}

impl EnvironmentEntry {
    fn of(key: &str, env: &Environment) -> Self {
        let policy = Policy::new(env.grants.clone());
        let now = Some(unix_now());
        let writable = policy.trusted_dirs(now);
        let mut grants: Vec<(String, bool)> = writable
            .iter()
            .map(|dir| (dir.display().to_string(), true))
            .collect();
        grants.extend(
            policy
                .readable_dirs(now)
                .iter()
                .map(|dir| (dir.display().to_string(), false)),
        );
        Self {
            key: key.to_owned(),
            name: env.name.clone(),
            tools: env
                .tools
                .iter()
                .flat_map(|tool| tool.packages.iter().cloned())
                .collect(),
            grants,
        }
    }
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum ServeResponse {
    Session {
        session: String,
        environment: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        commands: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editor_protocol: Option<u8>,
        /// The session's shell runs where a server runs the workspace's: the host's session
        /// it is, to come back to after the connection is lost.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_session: Option<String>,
    },
    Denied {
        reason: String,
    },
    /// The device's own shell needs the person's allow first. Everything
    /// else about the request was acceptable, so asking makes sense.
    HostNotAllowed {
        reason: String,
    },
    /// The environment cannot be built here: no `env-runtime` image is
    /// installed to make the default environment from. The connection stays
    /// open for another request, so the app can offer the device's own shell
    /// instead.
    NoRuntime {
        reason: String,
    },
    Environments {
        environments: Vec<EnvironmentEntry>,
    },
    Allowed,
    Branched {
        #[serde(flatten)]
        branched: crate::branch::Branched,
    },
    /// The workspace's host is asked for the session's shell, which may wait for the person
    /// to allow it on their devices: the session or the refusal follows on the connection.
    Waiting,
}

/// Decide one request: the environment record it names (`None` for the
/// default). Pure, so the check can be tested without a pty.
pub fn decide_serve(req: &ServeRequest) -> Result<Option<String>, String> {
    let ServeRequest::OpenSession { target, .. } = req else {
        return Err("not a session request".into());
    };
    match target {
        Target::Host => Err(
            "not available as an environment: the host target (your own login shell) \
             is decided by the host check"
                .into(),
        ),
        Target::Environment { record: None, .. } => Ok(None),
        Target::Environment {
            record: Some(record),
            ..
        } if is_record_key(record) => Ok(Some(record.clone())),
        Target::Environment {
            record: Some(record),
            ..
        } => Err(format!("{record:?} does not name an environment record")),
    }
}

/// Listen for apps and hand out guests.
///
/// # Errors
///
/// Returns an error if the store cannot be prepared, the socket path's
/// parent cannot be created, the socket cannot be bound, or its permissions
/// cannot be set to owner-only.
pub fn serve(socket: &Path, store: &Store, host_root: &Path) -> anyhow::Result<()> {
    store.ensure()?;
    // Invalid activation addressed to this process is fatal: unlinking and
    // rebinding would race queued clients and could replace an endpoint
    // systemd already exposed.
    // SAFETY: called from the `tilo-ush serve` main thread before it spawns
    // any connection thread; nothing else has started a thread.
    let activated = unsafe { tilo_files::native::private::activated_socket(socket)? };
    let listener = if let Some(listener) = activated {
        listener
    } else {
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        tilo_files::native::private::bind_socket(socket)?
    };
    tracing::info!(socket = %socket.display(), "ush: serving guests");
    crate::runtime_import::start(store);

    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let store = store.clone();
        let host_root = host_root.to_path_buf();
        std::thread::spawn(move || {
            if let Err(e) = serve_one(&stream, &store, &host_root) {
                tracing::warn!(error = %e, "ush: serve failed");
            }
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Served {
    /// Answered for good: a session ran, or the request was not one to retry.
    Done,
    Refused,
    Malformed,
}

pub const SERVICE_PROTOCOL: u16 = 2;
const MAX_ACTIVE_SESSIONS: usize = 8;
const MAX_SERVICE_LINE: u64 = 4096;

struct SessionTransport<'a> {
    stream: &'a UnixStream,
    active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// One app's control connection: it opens with a hello naming
/// [`SERVICE_PROTOCOL`], then asks as often as it likes, each session on a
/// lease of its own.
fn serve_one(stream: &UnixStream, store: &Store, host_root: &Path) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Hello {
        op: String,
        protocol: u16,
    }
    let mut writer = stream.try_clone()?;
    let mut raw = stream.try_clone()?;
    let (caller, launched, carried) = crate::host::read_handoff(&mut raw)?;
    let mut reader = BufReader::new(std::io::Cursor::new(carried).chain(raw));
    let transport = SessionTransport {
        stream,
        active: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let mut negotiated = false;
    loop {
        let mut line = String::new();
        if reader
            .by_ref()
            .take(MAX_SERVICE_LINE + 1)
            .read_line(&mut line)?
            == 0
        {
            return Ok(());
        }
        anyhow::ensure!(
            u64::try_from(line.len())? <= MAX_SERVICE_LINE,
            "ush request exceeds its bound"
        );
        if !negotiated {
            let hello = serde_json::from_str::<Hello>(&line)
                .ok()
                .filter(|hello| hello.op == "hello" && hello.protocol == SERVICE_PROTOCOL);
            if hello.is_none() {
                reply(
                    &mut writer,
                    &ServeResponse::Denied {
                        reason: format!(
                            "a connection opens with a hello for ush control protocol {SERVICE_PROTOCOL}"
                        ),
                    },
                    None,
                )?;
                return Ok(());
            }
            negotiated = true;
            writer.write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")?;
            writer.flush()?;
            continue;
        }
        let served = serve_request(
            &transport,
            &mut writer,
            store,
            host_root,
            (&caller, launched.as_deref()),
            &line,
        )?;
        if served == Served::Malformed {
            return Ok(());
        }
    }
}

fn session_limit_reached(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
) -> anyhow::Result<bool> {
    if transport.active.load(std::sync::atomic::Ordering::Acquire) < MAX_ACTIVE_SESSIONS {
        return Ok(false);
    }
    reply(
        writer,
        &ServeResponse::Denied {
            reason: "this window has reached its active session limit".into(),
        },
        None,
    )?;
    Ok(true)
}

fn parse_service_request(
    writer: &mut UnixStream,
    line: &str,
) -> anyhow::Result<Option<ServeRequest>> {
    match serde_json::from_str(line) {
        Ok(request) => Ok(Some(request)),
        Err(error) => {
            reply(
                writer,
                &ServeResponse::Denied {
                    reason: format!("malformed request: {error}"),
                },
                None,
            )?;
            Ok(None)
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct RequestScope {
    #[serde(default)]
    workspace: Option<String>,
}

fn serve_request(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
    store: &Store,
    host_root: &Path,
    (caller, launched): (&crate::host::Caller, Option<&str>),
    line: &str,
) -> anyhow::Result<Served> {
    let Some(req) = parse_service_request(writer, line)? else {
        return Ok(Served::Malformed);
    };
    let asked = serde_json::from_str::<RequestScope>(line)
        .unwrap_or_default()
        .workspace;
    let store = &match crate::workspace::scope_of(caller, launched, asked.as_deref()) {
        Ok(workspace) => store.for_workspace(&workspace),
        Err(reason) => {
            AuditLog::at(store.audit_dir()).append(
                &crate::audit::AuditEntry::new("serve", "-", "workspace", false).because(&reason),
            );
            reply(writer, &ServeResponse::Denied { reason }, None)?;
            return Ok(Served::Refused);
        }
    };
    if matches!(req, ServeRequest::OpenSession { .. }) && session_limit_reached(transport, writer)?
    {
        return Ok(Served::Refused);
    }
    match &req {
        ServeRequest::ListEnvironments
        | ServeRequest::CreateEnvironment { .. }
        | ServeRequest::EnvironmentAction { .. } => {
            let response = match directory(&req, store) {
                Ok(environments) => ServeResponse::Environments { environments },
                Err(reason) => ServeResponse::Denied { reason },
            };
            reply(writer, &response, None)?;
            return Ok(Served::Done);
        }
        ServeRequest::OpenSession {
            target: Target::Host,
            ..
        } => return serve_host(transport, writer, store, caller, &req),
        ServeRequest::AllowHost { expiry } => {
            reply(writer, &allow_host(caller, *expiry), None)?;
            return Ok(Served::Done);
        }
        ServeRequest::BranchWorkspace { into } => {
            reply(writer, &branch_workspace(caller, store, into), None)?;
            return Ok(Served::Done);
        }
        ServeRequest::OpenSession { .. } => {}
    }
    let environment = match decide_serve(&req) {
        Ok(e) => e,
        Err(reason) => {
            AuditLog::at(store.audit_dir()).append(
                &crate::audit::AuditEntry::new("serve", "-", "open-session", false)
                    .because(&reason),
            );
            reply(writer, &ServeResponse::Denied { reason }, None)?;
            return Ok(Served::Refused);
        }
    };
    serve_guest_session(
        transport,
        writer,
        store,
        host_root,
        req,
        environment.as_deref(),
    )
}

fn serve_guest_session(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
    store: &Store,
    host_root: &Path,
    req: ServeRequest,
    environment: Option<&str>,
) -> anyhow::Result<Served> {
    let record = environment
        .unwrap_or(tilo_environment::DEFAULT_RECORD)
        .to_owned();

    // A workspace a server hosts runs its default session's shell where the server runs it,
    // never on this device: whether this device could build an environment is no matter.
    if environment.is_none()
        && let Some(served) = serve_hosted(transport, writer, store, &req)?
    {
        return Ok(served);
    }

    // The record must resolve before a terminal is handed out: a guest that
    // cannot be built would print why on its pty and exit, leaving the app a
    // dead pane and no chance to offer anything else.
    if refused_unbuildable(writer, store, environment, &record)? {
        return Ok(Served::Refused);
    }

    open_environment(
        transport,
        writer,
        store,
        host_root,
        environment,
        &record,
        req,
    )
}

fn open_environment(
    transport: &SessionTransport<'_>,
    writer: &UnixStream,
    store: &Store,
    host_root: &Path,
    environment: Option<&str>,
    record: &str,
    req: ServeRequest,
) -> anyhow::Result<Served> {
    let record = record.to_owned();
    let ServeRequest::OpenSession {
        cols,
        rows,
        term,
        command,
        commands,
        editor_protocol,
        end_report,
        ..
    } = req
    else {
        return Ok(Served::Done);
    };
    let commands =
        commands.filter(|key| command.is_empty() && crate::command_marks::valid_key(key));
    let (master, slave) = open_pty(cols, rows)?;
    let editor = if editor_protocol == Some(runtime_portal::editor::VERSION) && command.is_empty() {
        Some(runtime_portal::editor::Socket::pair()?)
    } else {
        None
    };

    let id = uuid::Uuid::new_v4().simple().to_string();
    AuditLog::at(store.audit_dir()).append(&crate::audit::AuditEntry::new(
        &id,
        &record,
        "open-session",
        true,
    ));
    let launch = Launch {
        store,
        environment,
        term: term.as_deref().unwrap_or("xterm-256color"),
        host_root,
        command: &command,
        commands: commands.as_deref(),
        editor: editor.as_ref().map(|(_, supervisor)| supervisor),
    };

    // The guest gets the slave as its stdio; the app gets the master. A *child
    // process*, not a thread: attaching a pty means `dup2` onto fds 0/1/2 and
    // `setsid`, both process-wide — on a thread that would steal the
    // supervisor's own stdio and controlling terminal (and under a test harness, the harness's).
    let (child, diagnostics) = spawn_attached(&launch, &slave)?;
    // The child already holds its own clones for stdio, so the original can
    // close as soon as the spawn is done.
    drop(slave);
    let session_id = id.clone();
    hand_off_session(
        transport,
        writer,
        store,
        &ServeResponse::Session {
            session: id,
            environment: record,
            commands: commands.is_some(),
            editor_protocol: editor.as_ref().map(|_| runtime_portal::editor::VERSION),
            host_session: None,
        },
        master,
        child,
        editor.map(|(app, _)| app),
        || {
            Some(EndWatch {
                ended: watch_setup(diagnostics, session_id),
                report: end_report,
            })
        },
    )?;
    Ok(Served::Done)
}

struct PendingSessionChild(Option<std::process::Child>);

impl PendingSessionChild {
    fn transfer(&mut self) -> anyhow::Result<std::process::Child> {
        self.0
            .take()
            .ok_or_else(|| anyhow::anyhow!("session child was already transferred"))
    }
}

impl Drop for PendingSessionChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn hand_off_session(
    transport: &SessionTransport<'_>,
    writer: &UnixStream,
    store: &Store,
    response: &ServeResponse,
    master: std::fs::File,
    child: std::process::Child,
    editor: Option<runtime_portal::editor::Socket>,
    on_started: impl FnOnce() -> Option<EndWatch>,
) -> anyhow::Result<()> {
    use std::os::fd::AsFd;
    #[derive(serde::Serialize)]
    struct Leased<'a> {
        #[serde(flatten)]
        response: &'a ServeResponse,
        protocol: u16,
    }
    let mut child = PendingSessionChild(Some(child));
    let close_event = "close-session";
    let ServeResponse::Session {
        session,
        environment,
        ..
    } = response
    else {
        anyhow::bail!("session handoff requires an admitted session");
    };
    let (id, record, audit_dir) = (session.clone(), environment.clone(), store.audit_dir());
    // Terminal gave up on the open before the answer came: nothing holds this
    // session's terminal, so the pending child is stopped (on drop) and the
    // close audited rather than left running unseen.
    let undelivered = |error: anyhow::Error| {
        AuditLog::at(store.audit_dir()).append(
            &crate::audit::AuditEntry::new(&id, &record, close_event, true)
                .because("terminal gone before the session was handed over"),
        );
        error
    };
    let (supervisor_lease, terminal_lease) = UnixStream::pair()?;
    let mut bytes = serde_json::to_vec(&Leased {
        response,
        protocol: SERVICE_PROTOCOL,
    })?;
    bytes.push(b'\n');
    let mut rights = vec![master.as_fd(), terminal_lease.as_fd()];
    if let Some(app) = &editor {
        rights.push(app.as_fd());
    }
    runtime_portal::fd_transport::send_with_fds(writer, &bytes, &rights)
        .map_err(|error| undelivered(error.into()))?;
    drop(editor);
    drop(master);
    drop(terminal_lease);
    let watch = on_started();
    let active = transport.active.clone();
    let child = child.transfer()?;
    active.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    std::thread::spawn(move || {
        let termination = finish_session(&supervisor_lease, child, watch);
        AuditLog::at(audit_dir).append(
            &crate::audit::AuditEntry::new(&id, &record, close_event, true).because(termination),
        );
        active.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    });
    Ok(())
}

/// Refuse an open whose environment cannot be built, saying why, logging
/// and auditing it. Returns whether it was refused.
fn refused_unbuildable(
    writer: &mut UnixStream,
    store: &Store,
    environment: Option<&str>,
    record: &str,
) -> anyhow::Result<bool> {
    let Some((refusal, reason)) =
        unbuildable(store, environment, &*crate::images::on_this_device())
    else {
        return Ok(false);
    };
    tracing::warn!(record = %record, %reason, "ush: the environment cannot be built");
    AuditLog::at(store.audit_dir()).append(
        &crate::audit::AuditEntry::new("serve", record, "open-session", false).because(&reason),
    );
    reply(writer, &refusal, None)?;
    Ok(true)
}

/// Why the environment `record` (`None`: the default) cannot be built on
/// this device: the answer for the app and the reason in words. `None` when
/// it resolves. Nothing is written: the session makes the default
/// environment on its first use, so this answer — which Terminal waits for
/// with a short deadline — never includes a durable record write.
pub fn unbuildable(
    store: &Store,
    record: Option<&str>,
    images: &dyn Images,
) -> Option<(ServeResponse, String)> {
    match Records::of(store).resolvable(record, images) {
        Ok(()) => None,
        Err(crate::records::RecordError::NoRuntime(why)) => {
            let reason = format!("no environment can be built on this device: {why}");
            Some((
                ServeResponse::NoRuntime {
                    reason: reason.clone(),
                },
                reason,
            ))
        }
        Err(error) => {
            let reason = error.to_string();
            Some((
                ServeResponse::Denied {
                    reason: reason.clone(),
                },
                reason,
            ))
        }
    }
}

/// Take the person's answer for Terminal's Host target. Only a caller outside every app sandbox —
/// the session, which keeps the answer — may hand it.
fn allow_host(
    caller: &crate::host::Caller,
    expiry: Option<tilo_action_policy::Lifetime>,
) -> ServeResponse {
    if *caller != crate::host::Caller::Session {
        return ServeResponse::Denied {
            reason: "only the session hands this answer".into(),
        };
    }
    crate::host::hand_allow(expiry);
    ServeResponse::Allowed
}

fn branch_workspace(caller: &crate::host::Caller, store: &Store, into: &str) -> ServeResponse {
    if *caller != crate::host::Caller::Session {
        return ServeResponse::Denied {
            reason: "only the session branches a workspace".into(),
        };
    }
    let into = match crate::workspace::WorkspaceId::parse(Some(into)) {
        Ok(into) if !into.is_primary() => into,
        Ok(_) => {
            return ServeResponse::Denied {
                reason: "a branch is a workspace of its own".into(),
            };
        }
        Err(reason) => return ServeResponse::Denied { reason },
    };
    let branched = crate::branch::branch_environments(store, &store.for_workspace(&into));
    AuditLog::at(store.audit_dir()).append(
        &crate::audit::AuditEntry::new("serve", "-", "branch-workspace", branched.is_ok())
            .because(format!("{} into {into}", store.workspace_id().as_str())),
    );
    match branched {
        Ok(branched) => ServeResponse::Branched { branched },
        Err(reason) => ServeResponse::Denied { reason },
    }
}

fn directory(req: &ServeRequest, store: &Store) -> Result<Vec<EnvironmentEntry>, String> {
    let records = Records::of(store);
    match req {
        ServeRequest::CreateEnvironment { name } => {
            let name = name.trim();
            if name.is_empty() {
                return Err("an environment needs a name".into());
            }
            let env = if let Ok(Some(default)) = records.load(tilo_environment::DEFAULT_RECORD) {
                Environment {
                    name: name.to_owned(),
                    runtime: default.runtime,
                    ..Environment::default()
                }
            } else {
                let (release, image) = crate::images::on_this_device().current_runtime()?;
                Environment::new(name, &release, tilo_environment::host_arch(), &image)
            };
            let id: String = uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(12)
                .collect();
            let key = format!("env-{id}");
            records.create(&key, &env).map_err(|e| e.to_string())?;
        }
        ServeRequest::EnvironmentAction {
            record,
            action,
            args,
        } => {
            let invoke = match action.as_str() {
                "rename" => Environment::RENAME.with(
                    args.get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
                "grant-dir" => Environment::GRANT_DIR.with((
                    args.get("path")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    args.get("write")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                )),
                other => {
                    return Err(format!(
                        "`{other}` is not an environment action an app may ask for here"
                    ));
                }
            };
            records.apply(record, invoke).map_err(|e| e.to_string())?;
        }
        ServeRequest::ListEnvironments
        | ServeRequest::OpenSession { .. }
        | ServeRequest::AllowHost { .. }
        | ServeRequest::BranchWorkspace { .. } => {}
    }
    let mut out = Vec::new();
    for key in records.keys().map_err(|e| e.to_string())? {
        if let Ok(Some(env)) = records.load(&key) {
            out.push(EnvironmentEntry::of(&key, &env));
        }
    }
    Ok(out)
}

fn serve_host(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
    store: &Store,
    caller: &crate::host::Caller,
    req: &ServeRequest,
) -> anyhow::Result<Served> {
    let ServeRequest::OpenSession {
        cols,
        rows,
        term,
        command,
        requested_by,
        commands,
        ..
    } = req
    else {
        return Ok(Served::Done);
    };
    let ownership = crate::host::SessionOwnership::discover();
    let allowed = crate::host::host_allowed(tilo_clock::unix_now_ms());
    if let Err(denied) = crate::host::decide_host(caller, *requested_by, &ownership, allowed) {
        let reason = denied.reason();
        AuditLog::at(store.audit_dir()).append(
            &crate::audit::AuditEntry::new("serve", "host", "open-session", false).because(&reason),
        );
        let response = if denied == crate::host::HostDenied::NotAllowed {
            ServeResponse::HostNotAllowed { reason }
        } else {
            ServeResponse::Denied { reason }
        };
        reply(writer, &response, None)?;
        return Ok(Served::Refused);
    }
    crate::host::spend_one_time();
    let term = term.as_deref().unwrap_or("xterm-256color");
    let (cols, rows) = (*cols, *rows);
    let Some(account) = crate::host::login_account() else {
        reply(
            writer,
            &ServeResponse::Denied {
                reason: "this account has no passwd entry to take a login shell from".into(),
            },
            None,
        )?;
        return Ok(Served::Done);
    };
    let (master, slave) = open_pty(cols, rows)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    AuditLog::at(store.audit_dir()).append(&crate::audit::AuditEntry::new(
        &id,
        "host",
        "open-session",
        true,
    ));
    let interactive_bash = command.is_empty() && crate::command_marks::is_bash(&account.shell);
    let key = commands
        .as_deref()
        .filter(|key| interactive_bash && crate::command_marks::valid_key(key));
    let rc = interactive_bash
        .then(|| {
            let rc = store.session_dir(&id).join("shell.bash");
            std::fs::create_dir_all(store.session_dir(&id)).ok()?;
            std::fs::write(&rc, crate::shell_start::bash_rc(key.is_some())).ok()?;
            Some(rc)
        })
        .flatten();
    let commands = key.filter(|_| rc.is_some());
    let child = spawn_on_pty(
        crate::host::host_command(&account, term, command, rc.as_deref(), commands),
        &slave,
    )?;
    drop(slave);
    mark_terminal_session(transport.stream, child.id(), &id);
    hand_off_session(
        transport,
        writer,
        store,
        &ServeResponse::Session {
            session: id,
            environment: "host".into(),
            commands: commands.is_some(),
            editor_protocol: None,
            host_session: None,
        },
        master,
        child,
        None,
        || None,
    )?;
    Ok(Served::Done)
}

/// Record which Terminal launch asked for the Host session led by `leader`
/// (the process on the other end of `stream`), so the desktop can serve
/// what is typed in it with that Terminal's workspace scope. Without the
/// record the session still opens; the desktop then serves it nothing.
fn mark_terminal_session(stream: &UnixStream, leader: u32, session: &str) {
    let Some(requester) = peer_pid(stream) else {
        tracing::warn!("ush: the terminal asking for a host session could not be identified");
        return;
    };
    let Some(dir) = crate::terminal_sessions::dir(std::env::var_os("XDG_RUNTIME_DIR")) else {
        return;
    };
    if let Err(error) = crate::terminal_sessions::mark(&dir, leader, requester, session) {
        tracing::warn!(%error, "ush: could not record the host session's terminal");
    }
}

#[cfg(not(target_os = "linux"))]
fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    (rc == 0).then_some(())?;
    u32::try_from(cred.pid).ok().filter(|pid| *pid != 0)
}

/// Wait for whichever side of a served session ends first and tear the other down; returns
/// the line the audit log records.
///
/// The private lease is the lifetime capability in both directions. When Terminal closes
/// its end, this tears down exactly the child it created: dropping the pty master is not
/// enough, because a PID-namespace init may survive a terminal hangup. When the child exits
/// first it is reaped at once and the connection closed, which is how Terminal tells a
/// finished session from the hand-off window in which its pty master also reports
/// `EIO`.
fn finish_session(
    stream: &UnixStream,
    mut child: std::process::Child,
    watch: Option<EndWatch>,
) -> String {
    match wait_for_guest_or_terminal(stream, &child) {
        LifetimeEnd::GuestExited => {
            let status = child.wait();
            // How it ended — a server bridge's status, a local guest's setup
            // report: a terminal that asked reads it before the lease closes,
            // and tells a shell that exited from a connection that was lost
            // or a guest that never started.
            let mut failed = None;
            if let Some(watch) = watch {
                let end = watch
                    .ended
                    .join()
                    .unwrap_or_else(|_| GuestEnd::SetupFailed("the session's watch failed".into()));
                if watch.report
                    && let Ok(mut line) = serde_json::to_vec(&EndReport::from(&end))
                {
                    line.push(b'\n');
                    let _ = (&*stream).write_all(&line);
                }
                if let GuestEnd::SetupFailed(reason) = end {
                    failed = Some(reason);
                }
            }
            let _ = stream.shutdown(std::net::Shutdown::Both);
            match (status, failed) {
                (_, Some(reason)) => format!("not set up: {reason}"),
                (Ok(status), None) => format!("exit {}", status.code().unwrap_or(-1)),
                (Err(error), None) => format!("wait failed: {error}"),
            }
        }
        LifetimeEnd::TerminalClosed => match child.try_wait() {
            Ok(Some(status)) => format!("exit {}", status.code().unwrap_or(-1)),
            Ok(None) => match child.kill().and_then(|()| child.wait()) {
                Ok(status) => format!("terminal closed; exit {}", status.code().unwrap_or(-1)),
                Err(error) => format!("terminal closed; teardown failed: {error}"),
            },
            Err(error) => format!("status failed: {error}"),
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LifetimeEnd {
    GuestExited,
    TerminalClosed,
}

/// Block until either the guest process exits or Terminal's end of the
/// session connection becomes readable (closed, or an unexpected byte).
///
/// Waits on a pidfd so neither side is polled. Without pidfd support the
/// guest's exit cannot be observed separately, so only the terminal side is
/// waited on, as before.
fn wait_for_guest_or_terminal(stream: &UnixStream, child: &std::process::Child) -> LifetimeEnd {
    let Ok(pid) = libc::pid_t::try_from(child.id()) else {
        return wait_for_terminal(stream);
    };
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let Ok(raw) = std::os::fd::RawFd::try_from(raw) else {
        return wait_for_terminal(stream);
    };
    if raw < 0 {
        return wait_for_terminal(stream);
    }
    let pidfd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    let mut fds = [
        libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready >= 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return wait_for_terminal(stream);
        }
    }
    let [terminal, guest] = fds;
    if guest.revents != 0 && terminal.revents == 0 {
        LifetimeEnd::GuestExited
    } else {
        LifetimeEnd::TerminalClosed
    }
}

fn wait_for_terminal(stream: &UnixStream) -> LifetimeEnd {
    let mut reader = stream;
    let _ = reader.read(&mut [0u8; 1]);
    LifetimeEnd::TerminalClosed
}

struct Launch<'a> {
    store: &'a Store,
    environment: Option<&'a str>,
    term: &'a str,
    host_root: &'a Path,
    command: &'a [String],
    commands: Option<&'a str>,
    editor: Option<&'a runtime_portal::editor::Socket>,
}

fn spawn_attached(
    launch: &Launch<'_>,
    slave: &std::fs::File,
) -> std::io::Result<(std::process::Child, OwnedFd)> {
    let exe = runtime_binary()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run");
    cmd.arg(crate::setup::TERMINAL_FD_ARG)
        .arg(crate::setup::TERMINAL_FD.to_string());
    if let Some(record) = launch.environment {
        cmd.arg("--environment").arg(record);
    }
    cmd.arg("--host-root").arg(launch.host_root);
    // Held until the spawn: the editor endpoint, moved off the descriptor
    // the terminal takes in the child.
    let moved_editor = match launch.editor {
        Some(editor) if editor.as_fd().as_raw_fd() == crate::setup::TERMINAL_FD => {
            Some(editor.as_fd().try_clone_to_owned()?)
        }
        _ => None,
    };
    if let Some(editor) = launch.editor {
        use std::os::unix::process::CommandExt;
        let fd = moved_editor
            .as_ref()
            .map_or_else(|| editor.as_fd().as_raw_fd(), AsRawFd::as_raw_fd);
        cmd.arg("--editor-fd").arg(fd.to_string());
        // SAFETY: only this owned endpoint is inherited by the trusted runtime.
        unsafe {
            cmd.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    if !launch.command.is_empty() {
        cmd.arg("--").args(launch.command);
    }
    cmd.env(crate::store::STORE_ENV, launch.store.root());
    cmd.env(crate::store::RECORDS_ENV, launch.store.records_root());
    if let Some(root) = launch.store.grant_root() {
        cmd.env(crate::store::GRANT_ROOT_ENV, root);
    }
    match launch.store.workspace() {
        Some(workspace) => cmd.env(crate::store::WORKSPACE_ENV, workspace),
        None => cmd.env_remove(crate::store::WORKSPACE_ENV),
    };
    match launch.store.files() {
        Some(files) => cmd.env(crate::store::FILES_ENV, files),
        None => cmd.env_remove(crate::store::FILES_ENV),
    };
    cmd.env(
        crate::store::WORKSPACE_ID_ENV,
        launch.store.workspace_id().as_str(),
    );
    cmd.env("TERM", launch.term);
    match launch.commands {
        Some(key) => cmd.env(crate::command_marks::KEY_ENV, key),
        None => cmd.env_remove(crate::command_marks::KEY_ENV),
    };
    let (read, write) = diagnostics_pipe()?;
    let child = spawn_on_pty_with(cmd, slave, Some(write))?;
    drop(moved_editor);
    Ok((child, read))
}

fn spawn_on_pty(
    cmd: std::process::Command,
    slave: &std::fs::File,
) -> std::io::Result<std::process::Child> {
    spawn_on_pty_with(cmd, slave, None)
}

fn diagnostics_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded, so both are new descriptors nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn spawn_on_pty_with(
    mut cmd: std::process::Command,
    slave: &std::fs::File,
    diagnostics: Option<OwnedFd>,
) -> std::io::Result<std::process::Child> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    cmd.stdin(Stdio::from(slave.try_clone()?));
    cmd.stdout(Stdio::from(slave.try_clone()?));
    let terminal_too = diagnostics.is_some();
    match diagnostics {
        Some(diagnostics) => cmd.stderr(Stdio::from(diagnostics)),
        None => cmd.stderr(Stdio::from(slave.try_clone()?)),
    };

    let slave_fd = slave.as_raw_fd();
    // SAFETY: `pre_exec` runs between fork and exec in the child. Every call
    // here is async-signal-safe and none allocates. `setsid` is what makes the
    // child a session leader so `TIOCSCTTY` can hand it the pty; without a
    // controlling terminal the shell has no job control.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if terminal_too {
                // Standard input is the pty already; dup2 leaves the copy
                // open across exec.
                if libc::dup2(libc::STDIN_FILENO, crate::setup::TERMINAL_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// A pty whose two ends are close-on-exec: neither end travels into the shell, and holding the
/// master is the only thing that keeps the terminal open.
///
/// `openpty` (glibc) leaves both descriptors inheritable, so without this the shell a session
/// starts inherits the *master* as well as its own terminal, and it keeps the terminal's last
/// reader alive itself: when the session ends and the supervisor drops its end, the hangup never
/// arrives and the shell runs on as an orphan, reparented and holding a terminal nothing shows
/// (the Terminal's shell is the one that matters — it is the program a session starts, not one
/// the supervisor could only kill). The fork for `spawn_on_pty` happens between this call and the
/// exec, so the flags reach the child too.
fn open_pty(cols: u16, rows: u16) -> std::io::Result<(std::fs::File, std::fs::File)> {
    use std::os::fd::{AsRawFd as _, FromRawFd};
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw const size,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openpty` succeeded, so both are owned, open descriptors. `fcntl` only reads and rewrites
    // their descriptor flags; both calls are async-signal-safe and neither allocates.
    let pty = unsafe {
        let pty = (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        );
        for end in [&pty.0, &pty.1] {
            let fd = end.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        pty
    };
    Ok(pty)
}

fn reply(
    stream: &mut UnixStream,
    response: &ServeResponse,
    fd: Option<BorrowedFd<'_>>,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    match fd {
        None => {
            stream.write_all(&bytes)?;
            stream.flush()?;
        }
        Some(fd) => send_with_fd(&*stream, &bytes, fd)?,
    }
    Ok(())
}

pub use runtime_portal::fd_transport::{recv_with_fd, send_with_fd};

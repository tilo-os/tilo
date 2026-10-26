//! The supervisor: authenticated control and individually leased shell sessions.
//!
//! Runs on the host, in the user's session, outside every app sandbox — the
//! only thing here that holds `unshare` and `mount`, and it hands none of
//! that to the guest; see the trust boundary in [`crate::lib`](crate).
//!
//! ```text
//! ush
//!  ├─ resolve the environment record (the default is made on first use)
//!  ├─ resolve its images to read-only trees                (crate::images)
//!  ├─ a network namespace of its own, its DNS stub, its
//!  │  nftables and pasta, from the NetHost grants          (tilo_egress)
//!  ├─ compile the guest's seccomp filter                   (crate::seccomp)
//!  ├─ start the broker on a socket in the session dir      (crate::broker)
//!  ├─ spawn bwrap ─┬─ (inside the network namespace) unshare, stack, pivot
//!  │               ├─ install the seccomp filter        ← the boundary
//!  │               └─ exec tilo-ush guest-init ─┬─ install Landlock
//!  │                                           └─ exec the shell
//!  ├─ wait
//!  └─ restart if asked, from the record as it stands then
//! ```
//!
//! # `serve` — how an app reaches a guest
//!
//! A Tilo app cannot build a namespace (its seccomp filter denies the
//! syscalls and its sandbox has no path to this binary), so `apps/terminal`
//! asks for a guest instead of starting one: `tilo-ush serve` listens on a
//! socket `tilo-launcher` passes every app as a connected descriptor, and one
//! line of JSON naming a [`Target`] gets back a pty master over `SCM_RIGHTS` —
//! a terminal, never a namespace or a capability.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::FromRawFd;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_clock::unix_now;
use tilo_egress::{Egress, EgressOptions, Netns, spawn_egress};
use tilo_environment::{Environment, HomePolicy, Target, ToolBuilder, is_record_key};

use crate::audit::AuditLog;
use crate::broker::{Broker, BrokerState, EnvironmentHandle};
use crate::guest::{GuestPlan, GuestUser};
use crate::images::Images;
use crate::policy::Policy;
use crate::records::Records;
use crate::store::Store;

/// What the user asked for on the command line.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// The environment record to build from; `None` is the default
    /// environment.
    pub environment: Option<String>,
    /// `ush -c "…"`. Empty means an interactive shell.
    pub command: Vec<String>,
    pub term: String,
    /// The machine's root. `/` outside tests.
    pub host_root: PathBuf,
    pub verbose: bool,
    /// The key the shell marks its command runs with for Terminal
    /// ([`crate::runs`]); `None` marks nothing.
    pub runs: Option<String>,
    /// Connected participant-private editing endpoint from this served session.
    pub editor: Option<Arc<runtime_portal::editor::Socket>>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            environment: None,
            command: Vec::new(),
            term: std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
            host_root: PathBuf::from("/"),
            verbose: false,
            runs: std::env::var(crate::runs::KEY_ENV)
                .ok()
                .filter(|key| crate::runs::valid_key(key)),
            editor: None,
        }
    }
}

/// Printed beneath the actual setup error, including loader failures.
/// No mount-specific advice is given for unrelated startup failures.
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

/// Where the runtime's own binary is, as the supervisor sees it.
/// `current_exe` rather than a constant: the guest must run the same build
/// that built its confinement, which in a dev tree is under `target/`.
///
/// # Errors
///
/// Returns an error if `TILO_USH_BIN` is unset and
/// [`std::env::current_exe`] cannot resolve the running binary's path.
pub fn runtime_binary() -> std::io::Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("TILO_USH_BIN") {
        return Ok(PathBuf::from(explicit));
    }
    std::env::current_exe()
}

/// The shell a guest hosts. The host's `$SHELL` is a hint: it is used when
/// the images have it, else `/usr/bin/bash`, else `/usr/bin/sh`, so a session
/// never fails to start over a preference.
pub fn guest_shell(images: &[PathBuf]) -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(s) = std::env::var_os("SHELL") {
        let s = PathBuf::from(s);
        // `/bin` is a link into `/usr/bin` in the guest; look where it points.
        let in_usr = s
            .strip_prefix("/bin")
            .map_or_else(|_| s.clone(), |rest| Path::new("/usr/bin").join(rest));
        candidates.push(in_usr);
    }
    candidates.push(PathBuf::from("/usr/bin/bash"));
    candidates.push(PathBuf::from("/usr/bin/sh"));
    for c in &candidates {
        let Ok(relative) = c.strip_prefix("/") else {
            continue;
        };
        if images.iter().any(|image| image.join(relative).exists()) {
            return c.clone();
        }
    }
    PathBuf::from("/bin/sh")
}

/// One session, built and torn down.
pub struct Session {
    pub store: Store,
    pub opts: SessionOptions,
    pub session_id: String,
    pub images: Arc<dyn Images>,
    pub builder: Arc<dyn ToolBuilder>,
}

/// An environment resolved for one guest: its record and its image trees.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub key: String,
    pub environment: Environment,
    /// `(label, root)`, topmost first: tools newest first, the runtime last.
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

    fn records(&self) -> Records {
        Records::at(self.store.records_root())
    }

    /// The environment this session names, as the record stands now, with
    /// every image resolved to its tree.
    ///
    /// # Errors
    ///
    /// Returns an error when the record cannot be resolved (see
    /// [`Records::resolve`]) or an image is not available.
    pub fn resolve(&self) -> anyhow::Result<Resolved> {
        let (key, environment) = self
            .records()
            .resolve(self.opts.environment.as_deref(), &*self.images)?;
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

    /// This node's image of `tool`, checked to be built for `runtime` and
    /// available here; the reason when there is none to use.
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
        let guest_home = match resolved.environment.home {
            HomePolicy::Persistent => self.store.environment_home(&resolved.key),
            HomePolicy::Ephemeral => session_dir.join("home"),
        };
        let images: Vec<PathBuf> = resolved.images.iter().map(|(_, p)| p.clone()).collect();
        // Resolved, as a grant names its directory: `bwrap` mounts through
        // the real path, and a grant of the home must still find the store
        // under it.
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let runtime_bin = runtime_binary()?;
        let helper_runtime = crate::helper_runtime::HelperRuntime::resolve(&runtime_bin)?;
        let shell = guest_shell(&images);
        let runs = self
            .opts
            .runs
            .clone()
            .filter(|_| self.opts.command.is_empty() && crate::runs::is_bash(&shell));
        Ok(GuestPlan {
            shell,
            runs,
            shell_rc: false,
            images,
            host_root: self.opts.host_root.clone(),
            session_dir,
            var_dir: self.store.environment_var(&resolved.key),
            guest_home,
            user: GuestUser::current(),
            environment: resolved.environment.name.clone(),
            masked: vec![
                canonical(self.store.root()),
                canonical(self.store.records_root()),
            ],
            trusted_dirs: policy.trusted_dirs(now),
            readable_dirs: policy.readable_dirs(now),
            grant_root: self.store.grant_root().map(Path::to_path_buf),
            runtime_bin,
            helper_runtime,
            legacy_commands: crate::legacy_commands(),
            broker_socket,
            seccomp_blob,
            command: self.opts.command.clone(),
            term: self.opts.term.clone(),
            env: Vec::new(),
            resolver: None,
            editor: None,
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

    /// May a restart request rebuild this session, or is it a one-shot? Only an
    /// interactive session restarts: `ush -c "…"` has a command with side
    /// effects, and rebuilding would re-run it.
    pub fn restarts_in_place(&self) -> bool {
        self.opts.command.is_empty()
    }

    /// Run the session to completion, restarting in place as many times as the
    /// guest asks. Returns the guest's exit code.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be prepared, or if building or
    /// running a guest fails (see `Session::run_once`).
    pub fn run(&mut self) -> anyhow::Result<i32> {
        self.store.ensure()?;
        let code;
        loop {
            let (exit, again) = self.run_once()?;
            if !again || !self.restarts_in_place() {
                code = exit;
                break;
            }
            // A restart is a new namespace: a new session directory and broker
            // socket, and the record read again — that is how a tool added or a
            // directory granted reaches the shell.
            self.session_id = uuid::Uuid::new_v4().simple().to_string();
        }
        Ok(code)
    }

    fn run_once(&self) -> anyhow::Result<(i32, bool)> {
        let resolved = self.resolve()?;
        let session_dir = self.store.session_dir(&self.session_id);
        std::fs::create_dir_all(&session_dir)?;
        let outcome = self.run_guest(&resolved, &session_dir);
        // Holds the generated `/etc`, the broker socket, an ephemeral home.
        crate::guest::remove_dir_all_forced(&session_dir);
        outcome
    }

    fn run_guest(&self, resolved: &Resolved, session_dir: &Path) -> anyhow::Result<(i32, bool)> {
        // Before anything else: a guest whose network cannot be confined as
        // its record says does not start.
        let (netns, egress) = self.network(resolved)?;
        let egress = Arc::new(egress);
        let seccomp_blob = crate::seccomp::write_guest_filter(session_dir)?;
        let broker_socket = session_dir.join("broker.sock");
        let broker = Broker::start(
            broker_socket.clone(),
            BrokerState {
                session_id: self.session_id.clone(),
                environment: EnvironmentHandle {
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
                egress: Some(Arc::clone(&egress)),
            },
        )?;

        let mut plan = self.plan(resolved, broker_socket, seccomp_blob)?;
        plan.resolver = Some(egress.resolv_conf().to_owned());
        plan.prepare()?;
        seed_guest_home(&plan.guest_home)?;
        let editor = if self.opts.editor.is_some() && self.opts.command.is_empty() {
            crate::editor::Prepared::prepare(&plan).unwrap_or(None)
        } else {
            None
        };
        plan.editor = editor.as_ref().map(|prepared| prepared.guest.clone());
        if self.opts.command.is_empty() {
            plan.write_shell_rc()?;
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
        command.extend(["guest-init".into(), "--".into()]);
        if user_command.is_empty() {
            // The session's login shell, started as the Host's is.
            command.extend(
                plan.shell_argv()
                    .into_iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            );
        } else {
            command.extend(user_command);
        }
        plan.command = command;

        let mut child = crate::guest::spawn(&plan, Some(&netns))?;
        plan.editor = None; // the parent must not retain the inherited producer FD
        let _editor_worker = match (editor, self.opts.editor.as_ref()) {
            (Some(editor), Some(app)) => {
                if let Ok(worker) = editor.start(Arc::clone(app), child.id()) {
                    Some(worker)
                } else {
                    app.retire();
                    None
                }
            }
            (_, Some(app)) => {
                app.retire();
                None
            }
            _ => None,
        };
        let status = child.wait()?;
        let code = status.code().unwrap_or(1);
        if code != 0 && !broker.guest_connected() {
            eprintln!("{SANDBOX_SETUP_FAILED}");
        }
        let restart = broker.restart_requested();
        drop(broker);
        // The stub and pasta stop before the namespace's holder goes.
        drop(egress);
        drop(netns);
        Ok((code, restart))
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

/// Give a fresh guest home the dotfiles that make a shell usable. Never
/// seeded from the user's own home: an environment's home is its own.
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "the `${VAR}` below is shell syntax in the .bashrc content, not a Rust format placeholder"
)]
fn seed_guest_home(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let bashrc = home.join(".bashrc");
    if bashrc.exists() {
        return Ok(());
    }
    std::fs::write(
        &bashrc,
        "# ush guest shell\n\
         [ -f /etc/bashrc ] && . /etc/bashrc\n\
         PS1='[\\[\\e[36m\\]${TILO_USH_ENVIRONMENT:-ush}\\[\\e[0m\\] \\W]\\$ '\n\
         alias ls='ls --color=auto'\n",
    )
}

// serve — handing a guest to an app

/// What an app asks for over the service socket.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ServeRequest {
    /// Open a session on `target` and hand back its pty master.
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
        /// For an environment on a relay: the session to re-attach to (hex),
        /// as an earlier answer named it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay_session: Option<String>,
        /// The key the session's shell marks its command runs with
        /// ([`crate::runs`]), chosen by the app for this session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runs: Option<String>,
        /// Explicit opt-in; existing clients continue to receive exactly one FD.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editor_protocol: Option<u8>,
        /// Explicit opt-in: before a relay session's lease closes, tell the
        /// app on it how the session ended (`EndReport`): its shell exited,
        /// the connection to the relay was lost, or another terminal took
        /// it over. Existing clients get a lease that only closes.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        end_report: bool,
    },
    /// The environments there are, for Terminal's picker and its
    /// environments panel.
    ListEnvironments,
    /// The person's relays an environment can run on.
    ListRelays,
    /// The person allowed Terminal's Host target, for `expiry` (the shell's
    /// permission prompt answers with this). Only a caller outside every app
    /// sandbox — the shell — may record it.
    AllowHost { expiry: collab_types::caps::Expiry },
    /// Make a new environment on the default environment's runtime, with
    /// nothing added and nothing granted. Answers with every environment.
    CreateEnvironment { name: String },
    /// Apply one of the record's actions (`rename`) to an environment.
    /// Answers with every environment.
    EnvironmentAction {
        record: String,
        action: String,
        #[serde(default)]
        args: serde_json::Value,
    },
}

/// One environment as an app sees it: what it is called, what it has, and
/// which directories it was granted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentEntry {
    pub key: String,
    pub name: String,
    pub tools: Vec<String>,
    /// Granted directories, `(path, writable)`.
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
    /// The pty master follows as an `SCM_RIGHTS` descriptor on this message.
    Session {
        session: String,
        /// The environment record the session was built from.
        environment: String,
        /// For an environment on a relay: the session's id there, which
        /// re-attaches to it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay_session: Option<String>,
        /// The session's shell is asked to mark its runs with the key the
        /// request named. A shell other than `bash` writes no marks.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        runs: bool,
        /// V1 bundles PTY first, private editor second, under this session's lifetime.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editor_protocol: Option<u8>,
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
    /// Every environment, by key.
    Environments {
        environments: Vec<EnvironmentEntry>,
    },
    /// The allow was recorded.
    Allowed,
    /// The person's relays.
    Relays {
        relays: Vec<crate::relay::RelayPeer>,
    },
}

/// Decide one request: the environment record it names (`None` for the
/// default). Pure, so the gate can be tested without a pty.
///
/// # Errors
///
/// Returns the reason a target cannot be served: the host target (not served
/// here), or a record key an app may not use.
pub fn decide_serve(req: &ServeRequest) -> Result<Option<String>, String> {
    let ServeRequest::OpenSession { target, .. } = req else {
        return Err("not a session request".into());
    };
    match target {
        Target::Host => Err(
            "not available as an environment: the host target (your own login shell) \
             is decided by the host gate"
                .into(),
        ),
        Target::Environment {
            location: tilo_environment::Location::Relay { .. },
            ..
        } => Err("an environment on a relay is carried there, not built here".into()),
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
    import_default_runtime_in_background(store);

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

/// Import the default environment's runtime into the user's store on a thread of its own,
/// once, as soon as the supervisor runs. The first session of the default environment
/// imports it too when it is not there yet: a system store image has no copy in the user's
/// store until its first use, and the copy reads and checks thousands of files. Starting it
/// when the supervisor starts (which an app opening Terminal does) overlaps that copy with
/// the app's own start, and the store's lock lets the session wait for the copy under way
/// instead of making a second one. The journal records how long it took.
fn import_default_runtime_in_background(store: &Store) {
    let records = Records::at(store.records_root());
    let spawned = std::thread::Builder::new()
        .name("ush-runtime-import".into())
        .spawn(move || {
            let images = crate::images::on_this_device();
            let runtime = match records.load(tilo_environment::DEFAULT_RECORD) {
                Ok(Some(environment)) => environment
                    .runtime_image(tilo_environment::host_arch())
                    .ok(),
                Ok(None) => images.current_runtime().ok().map(|(_, id)| id),
                Err(_) => None,
            };
            let Some(runtime) = runtime else { return };
            if images.is_local(&runtime) {
                return;
            }
            let started = std::time::Instant::now();
            tracing::info!(%runtime, "ush: importing the default runtime for its first use");
            match images.tree(&runtime) {
                Ok(_) => tracing::info!(
                    %runtime,
                    elapsed_ms = started.elapsed().as_millis(),
                    "ush: imported the default runtime for its first use"
                ),
                Err(error) => tracing::warn!(
                    %runtime,
                    %error,
                    elapsed_ms = started.elapsed().as_millis(),
                    "ush: importing the default runtime failed; its first session tries again"
                ),
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "ush: could not start the runtime import");
    }
}

/// Requests one connection may make. A refused open spends nothing — the
/// connection is the app's one way to ask — so the app may ask again on it
/// (the device's own shell after an environment could not be built), but not
/// without end.
const MAX_REQUESTS_PER_CONNECTION: usize = 8;

/// Whether a request ended the connection's use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Served {
    /// Answered for good: a session ran, or the request was not one to retry.
    Done,
    /// An open was refused; the app may ask again on this connection.
    Refused,
    /// A malformed message cannot retain persistent control.
    Malformed,
}

/// Persistent control speaks version two; legacy clients keep their single lease.
pub const SERVICE_PROTOCOL: u16 = 2;
const MAX_ACTIVE_SESSIONS: usize = 8;
const MAX_SERVICE_LINE: u64 = 4096;

struct SessionTransport<'a> {
    stream: &'a UnixStream,
    persistent: bool,
    active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn serve_one(stream: &UnixStream, store: &Store, host_root: &Path) -> anyhow::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut raw = stream.try_clone()?;
    let (caller, carried) = crate::host::read_caller(&mut raw)?;
    let mut reader = BufReader::new(std::io::Cursor::new(carried).chain(raw));
    let mut transport = SessionTransport {
        stream,
        persistent: false,
        active: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let mut legacy_requests = 0usize;
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
        if serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .is_some_and(|value| {
                value.get("op").and_then(serde_json::Value::as_str) == Some("hello")
            })
        {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Hello {
                op: String,
                protocol: u16,
            }
            let hello: Hello = serde_json::from_str(&line)?;
            anyhow::ensure!(
                hello.op == "hello" && hello.protocol == SERVICE_PROTOCOL,
                "ush control protocol is unsupported"
            );
            transport.persistent = true;
            writer.write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")?;
            writer.flush()?;
            continue;
        }
        if !transport.persistent {
            if legacy_requests >= MAX_REQUESTS_PER_CONNECTION {
                return Ok(());
            }
            legacy_requests = legacy_requests.saturating_add(1);
        }
        let served = serve_request(&transport, &mut writer, store, host_root, &caller, &line)?;
        if served == Served::Malformed || (!transport.persistent && served == Served::Done) {
            return Ok(());
        }
    }
}

fn session_limit_reached(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
) -> anyhow::Result<bool> {
    if !transport.persistent
        || transport.active.load(std::sync::atomic::Ordering::Acquire) < MAX_ACTIVE_SESSIONS
    {
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

fn serve_request(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
    store: &Store,
    host_root: &Path,
    caller: &crate::host::Caller,
    line: &str,
) -> anyhow::Result<Served> {
    let Some(req) = parse_service_request(writer, line)? else {
        return Ok(Served::Malformed);
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
        ServeRequest::OpenSession { target, .. } if is_relay(target) => {
            return serve_relay(transport, writer, store, &req);
        }
        ServeRequest::ListRelays => {
            reply(writer, &list_relays(), None)?;
            return Ok(Served::Done);
        }
        ServeRequest::AllowHost { expiry } => {
            reply(writer, &allow_host(caller, store, *expiry), None)?;
            return Ok(Served::Done);
        }
        ServeRequest::OpenSession { .. } => {}
    }
    let environment = match decide_serve(&req) {
        Ok(e) => e,
        Err(reason) => {
            AuditLog::at(store.audit_dir()).append(
                &crate::audit::AuditRecord::new("serve", "-", "open-session", false)
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

/// Build the guest of an environment on this device on a fresh pty and hand
/// its master to the app; the connection is the session's lifetime.
fn open_environment(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
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
        runs,
        editor_protocol,
        ..
    } = req
    else {
        return Ok(Served::Done);
    };
    let runs = runs.filter(|key| command.is_empty() && crate::runs::valid_key(key));
    let (master, slave) = open_pty(cols, rows)?;
    let editor = if editor_protocol == Some(runtime_portal::editor::VERSION) && command.is_empty() {
        Some(runtime_portal::editor::Socket::pair()?)
    } else {
        None
    };

    let id = uuid::Uuid::new_v4().simple().to_string();
    AuditLog::at(store.audit_dir()).append(&crate::audit::AuditRecord::new(
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
        runs: runs.as_deref(),
        editor: editor.as_ref().map(|(_, supervisor)| supervisor),
    };

    // The guest gets the slave as its stdio; the app gets the master. A *child
    // process*, not a thread: attaching a pty means `dup2` onto fds 0/1/2 and
    // `setsid`, both process-wide — on a thread that would steal the
    // supervisor's own stdio and controlling terminal (and under a test harness, the harness's).
    let label = command.first().map_or_else(
        || "Shell".to_owned(),
        |program| runtime_portal::jobs::program_label(program.as_ref()),
    );
    let child = spawn_attached(&launch, &slave, session_job(transport, &label))?;
    // The child already holds its own clones for stdio, so the original can
    // close as soon as the spawn is done.
    drop(slave);
    hand_off_session(
        transport,
        writer,
        store,
        &ServeResponse::Session {
            session: id,
            environment: record,
            relay_session: None,
            runs: runs.is_some(),
            editor_protocol: editor.as_ref().map(|_| runtime_portal::editor::VERSION),
        },
        master,
        child,
        editor.map(|(app, _)| app),
        || None,
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
    writer: &mut UnixStream,
    store: &Store,
    response: &ServeResponse,
    master: std::fs::File,
    child: std::process::Child,
    editor: Option<runtime_portal::editor::Socket>,
    on_started: impl FnOnce() -> Option<RelayWatch>,
) -> anyhow::Result<()> {
    use std::os::fd::AsFd;
    #[derive(serde::Serialize)]
    struct Leased<'a> {
        #[serde(flatten)]
        response: &'a ServeResponse,
        protocol: u16,
    }
    let mut child = PendingSessionChild(Some(child));
    let close_event = if matches!(
        response,
        ServeResponse::Session {
            relay_session: Some(_),
            ..
        }
    ) {
        "close-relay-session"
    } else {
        "close-session"
    };
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
            &crate::audit::AuditRecord::new(&id, &record, close_event, true)
                .because("terminal gone before the session was handed over"),
        );
        error
    };
    if !transport.persistent {
        // The legacy lease is the request connection itself: PTY first, then
        // the editor capability when one was negotiated.
        if let Some(app) = &editor {
            let mut bytes = serde_json::to_vec(response)?;
            bytes.push(b'\n');
            runtime_portal::fd_transport::send_with_fds(
                &*writer,
                &bytes,
                &[master.as_fd(), app.as_fd()],
            )
            .map_err(|error| undelivered(error.into()))?;
        } else {
            reply(writer, response, Some(master.as_fd())).map_err(undelivered)?;
        }
        drop(editor);
        drop(master);
        // The legacy lease carries no report: it only closes.
        let watch = on_started().map(|watch| RelayWatch {
            report: false,
            ..watch
        });
        let termination = finish_session(transport.stream, child.transfer()?, watch);
        AuditLog::at(audit_dir).append(
            &crate::audit::AuditRecord::new(&id, &record, close_event, true).because(termination),
        );
        return Ok(());
    }
    let (supervisor_lease, terminal_lease) = UnixStream::pair()?;
    let mut bytes = serde_json::to_vec(&Leased {
        response,
        protocol: SERVICE_PROTOCOL,
    })?;
    bytes.push(b'\n');
    // Roles, in order: PTY master, private session lease, then the editor
    // capability when `editor_protocol` was negotiated.
    let mut rights = vec![master.as_fd(), terminal_lease.as_fd()];
    if let Some(app) = &editor {
        rights.push(app.as_fd());
    }
    runtime_portal::fd_transport::send_with_fds(&*writer, &bytes, &rights)
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
            &crate::audit::AuditRecord::new(&id, &record, close_event, true).because(termination),
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
        &crate::audit::AuditRecord::new("serve", record, "open-session", false).because(&reason),
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
    match Records::at(store.records_root()).resolvable(record, images) {
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

/// Record the person's allow for Terminal's Host target. Only a caller
/// outside every app sandbox — the shell, which put the prompt up — may.
fn allow_host(
    caller: &crate::host::Caller,
    store: &Store,
    expiry: collab_types::caps::Expiry,
) -> ServeResponse {
    if *caller != crate::host::Caller::Session {
        return ServeResponse::Denied {
            reason: "only the session's own shell records this allow".into(),
        };
    }
    match crate::host::HostPolicy::record_allow(store, crate::host::TERMINAL_APP_ID, expiry) {
        Ok(()) => ServeResponse::Allowed,
        Err(e) => ServeResponse::Denied {
            reason: format!("the allow could not be recorded: {e}"),
        },
    }
}

/// Answer a request about the environments themselves.
fn directory(req: &ServeRequest, store: &Store) -> Result<Vec<EnvironmentEntry>, String> {
    let records = Records::at(store.records_root());
    match req {
        ServeRequest::CreateEnvironment { name } => {
            let name = name.trim();
            if name.is_empty() {
                return Err("an environment needs a name".into());
            }
            // A new environment starts on the default environment's runtime
            // release, or on the newest one this device holds.
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
        | ServeRequest::ListRelays
        | ServeRequest::AllowHost { .. } => {}
    }
    let mut out = Vec::new();
    for key in records.keys().map_err(|e| e.to_string())? {
        if let Ok(Some(env)) = records.load(&key) {
            out.push(EnvironmentEntry::of(&key, &env));
        }
    }
    Ok(out)
}

/// Run the device's own login shell on a fresh pty and hand its master to
/// the app — once [`crate::host::decide_host`] allows `caller` this request.
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
        runs,
        ..
    } = req
    else {
        return Ok(Served::Done);
    };
    let policy = crate::host::HostPolicy::in_force(store);
    let ownership = crate::host::SessionOwnership::discover();
    if let Err(denied) =
        crate::host::decide_host(caller, *requested_by, &ownership, &policy, Some(unix_now()))
    {
        let reason = denied.reason();
        AuditLog::at(store.audit_dir()).append(
            &crate::audit::AuditRecord::new("serve", "host", "open-session", false)
                .because(&reason),
        );
        let response = if denied == crate::host::HostDenied::NotAllowed {
            ServeResponse::HostNotAllowed { reason }
        } else {
            ServeResponse::Denied { reason }
        };
        reply(writer, &response, None)?;
        return Ok(Served::Refused);
    }
    crate::host::HostPolicy::spend_one_time(crate::host::TERMINAL_APP_ID);
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
    AuditLog::at(store.audit_dir()).append(&crate::audit::AuditRecord::new(
        &id,
        "host",
        "open-session",
        true,
    ));
    // The interactive shell starts as a guest's does (crate::shell_start).
    let interactive_bash = command.is_empty() && crate::runs::is_bash(&account.shell);
    let key = runs
        .as_deref()
        .filter(|key| interactive_bash && crate::runs::valid_key(key));
    let rc = interactive_bash
        .then(|| {
            let rc = store.session_dir(&id).join("shell.bash");
            std::fs::create_dir_all(store.session_dir(&id)).ok()?;
            std::fs::write(&rc, crate::shell_start::bash_rc("", key.is_some())).ok()?;
            Some(rc)
        })
        .flatten();
    let runs = key.filter(|_| rc.is_some());
    let label = command.first().map_or_else(
        || runtime_portal::jobs::program_label(account.shell.as_os_str()),
        |program| runtime_portal::jobs::program_label(program.as_ref()),
    );
    let child = spawn_on_pty(
        crate::host::host_command(&account, term, command, rc.as_deref(), runs),
        &slave,
        session_job(transport, &label),
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
            relay_session: None,
            runs: runs.is_some(),
            editor_protocol: None,
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

/// The pid the kernel names for the other end of `stream`.
#[cfg(not(target_os = "linux"))]
fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
}

/// The pid the kernel names for the other end of `stream`.
#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: `cred` and `len` are live, correctly sized out-parameters.
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

/// The relay session a terminal asks for: re-attaching to `relay_session`,
/// or opening one on the environment `record` (keyed `key`); why not.
fn relay_session_to(
    store: &Store,
    record: Option<&str>,
    relay_session: Option<&str>,
    key: &str,
) -> Result<crate::relay::RelaySession, String> {
    match relay_session {
        Some(session) if crate::relay::is_session_id(session) => {
            Ok(crate::relay::RelaySession::Attach {
                session: session.to_owned(),
            })
        }
        Some(_) => Err("not a relay session id".into()),
        None => {
            // The relay builds the guest from the record as this device
            // holds it; the default comes into being on first use, as here.
            Records::at(store.records_root())
                .resolve(record, &*crate::images::on_this_device())
                .map_err(|e| e.to_string())?;
            Ok(crate::relay::RelaySession::Open {
                record: key.to_owned(),
            })
        }
    }
}

/// Carry the terminal to an environment session on one of the person's
/// relays: `tilo-relay-environment` runs on the pty handed back, so the app
/// gets a pty from this broker whatever the target. The answer names the
/// relay session, which re-attaches to it later.
fn serve_relay(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
    store: &Store,
    req: &ServeRequest,
) -> anyhow::Result<Served> {
    let ServeRequest::OpenSession {
        target:
            Target::Environment {
                record,
                location: tilo_environment::Location::Relay { relay },
            },
        cols,
        rows,
        term,
        relay_session,
        end_report,
        ..
    } = req
    else {
        return Ok(Served::Done);
    };
    let denied = |writer: &mut UnixStream, reason: String| {
        reply(writer, &ServeResponse::Denied { reason }, None)?;
        Ok(Served::Refused)
    };
    let identity = crate::relay::identity_paths();
    let Some(peer) = crate::relay::relays(&identity)
        .into_iter()
        .find(|peer| peer.relay == *relay)
    else {
        return denied(writer, format!("{relay} is not one of your relays"));
    };
    let key = record
        .as_deref()
        .unwrap_or(tilo_environment::DEFAULT_RECORD)
        .to_owned();
    let session = match relay_session_to(store, record.as_deref(), relay_session.as_deref(), &key) {
        Ok(session) => session,
        Err(reason) => return denied(writer, reason),
    };
    let Some(binary) = crate::relay::bridge_binary() else {
        return denied(
            writer,
            "tilo-relay-environment is not installed, so no relay session can open".into(),
        );
    };
    let Some(identity_root) = crate::relay::identity_root(&identity) else {
        return denied(writer, "this device has no identity".into());
    };
    let (master, slave) = open_pty(*cols, *rows)?;
    let launch = RelayLaunch {
        binary: &binary,
        peer: &peer,
        session: &session,
        records_root: store.records_root(),
        identity_root: &identity_root,
        term: term.as_deref().unwrap_or("xterm-256color"),
    };
    let job = session_job(transport, "Shell");
    let (child, status, session_id) = match start_relay_bridge(&launch, &slave, job)? {
        Ok(started) => started,
        Err(reason) => return denied(writer, reason),
    };
    drop(slave);
    let id = uuid::Uuid::new_v4().simple().to_string();
    AuditLog::at(store.audit_dir()).append(&crate::audit::AuditRecord::new(
        &id,
        &key,
        "open-relay-session",
        true,
    ));
    hand_off_session(
        transport,
        writer,
        store,
        &ServeResponse::Session {
            session: id,
            environment: key,
            relay_session: Some(session_id),
            runs: false,
            editor_protocol: None,
        },
        master,
        child,
        None,
        || {
            Some(RelayWatch {
                bridge: watch_relay_bridge(status, store.records_root().to_path_buf()),
                report: *end_report,
            })
        },
    )?;
    Ok(Served::Done)
}

fn is_relay(target: &Target) -> bool {
    matches!(
        target,
        Target::Environment {
            location: tilo_environment::Location::Relay { .. },
            ..
        }
    )
}

/// The relays a new environment can be opened on, as last heard. When an
/// answer is older than a minute the relays are asked again in the
/// background; Terminal reads the list again and sees the answers.
fn list_relays() -> ServeResponse {
    let identity = crate::relay::identity_paths();
    let relays = crate::relay::relays(&identity);
    if crate::relay::needs_asking(&relays, tilo_clock::unix_now_ms()) {
        crate::relay::ask_relays_again(&identity);
    }
    ServeResponse::Relays {
        relays: relays
            .into_iter()
            .filter(|peer| peer.runs_environments)
            .collect(),
    }
}

/// What a relay session's bridge is started with.
struct RelayLaunch<'a> {
    binary: &'a Path,
    peer: &'a crate::relay::RelayPeer,
    session: &'a crate::relay::RelaySession,
    records_root: &'a Path,
    identity_root: &'a Path,
    term: &'a str,
}

type StartedBridge = (std::process::Child, crate::relay::StatusReader, String);

/// Start `tilo-relay-environment` on `slave` and wait for it to name the
/// session; the inner error is why it did not open.
fn start_relay_bridge(
    launch: &RelayLaunch<'_>,
    slave: &std::fs::File,
    job: Option<runtime_portal::jobs::Slot>,
) -> std::io::Result<Result<StartedBridge, String>> {
    let (status_read, status_write) = crate::relay::status_pipe()?;
    let mut cmd = crate::relay::bridge_command(
        launch.binary,
        launch.peer,
        launch.session,
        launch.records_root,
        launch.identity_root,
        launch.term,
    );
    let write_fd = status_write.as_raw_fd();
    // SAFETY: `pre_exec` runs between fork and exec; it only calls dup2.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(move || crate::relay::install_status_fd(write_fd));
    }
    let mut child = spawn_on_pty(cmd, slave, job)?;
    drop(status_write);
    let mut status = crate::relay::StatusReader::new(status_read);
    match status.next(Some(crate::relay::OPEN_PATIENCE)) {
        Some(crate::relay::BridgeStatus::Session { session, .. }) => {
            Ok(Ok((child, status, session)))
        }
        other => {
            let _ = child.kill();
            let _ = child.wait();
            let reason = match other {
                Some(crate::relay::BridgeStatus::Ended {
                    reason: Some(reason),
                    ..
                }) => reason,
                _ => format!("the session on {} did not open", launch.peer.label),
            };
            // The relay had no worker for it: what the picker shows is out
            // of date, so ask the relays again.
            if reason.contains("NoWorker") {
                crate::relay::ask_relays_again(&crate::relay::identity_paths());
            }
            Ok(Err(reason))
        }
    }
}

/// How a relay session's bridge ended, as it said on its status channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayEnd {
    Exited,
    /// The connection to the relay was lost; the session may still run
    /// there, and the tab re-attaches to it.
    Lost(String),
    /// Another terminal took the session over; it runs on there.
    Superseded,
}

impl RelayEnd {
    /// The end a bridge's last status line names; a bridge that stopped
    /// without saying lost its connection, as far as the tab can tell.
    fn of(status: Option<crate::relay::BridgeStatus>) -> Self {
        match status {
            Some(crate::relay::BridgeStatus::Ended { code: Some(_), .. }) => Self::Exited,
            Some(crate::relay::BridgeStatus::Ended {
                superseded: true, ..
            }) => Self::Superseded,
            Some(crate::relay::BridgeStatus::Ended {
                reason: Some(reason),
                ..
            }) => Self::Lost(reason),
            _ => Self::Lost("the connection to the relay ended".into()),
        }
    }
}

/// What an app that asked (`end_report`) reads on its lease just before it
/// closes: one JSON line.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "ended", rename_all = "kebab-case")]
enum EndReport<'a> {
    Exited,
    Lost { reason: &'a str },
    Superseded,
}

impl<'a> From<&'a RelayEnd> for EndReport<'a> {
    fn from(end: &'a RelayEnd) -> Self {
        match end {
            RelayEnd::Exited => Self::Exited,
            RelayEnd::Lost(reason) => Self::Lost { reason },
            RelayEnd::Superseded => Self::Superseded,
        }
    }
}

/// A relay session's bridge, watched: how it ended, and whether the app
/// asked to be told.
pub(crate) struct RelayWatch {
    bridge: std::thread::JoinHandle<RelayEnd>,
    report: bool,
}

/// Read a relay session's status channel to its end: the tools the relay
/// builds for it become the record's, as a `pkg install` on this device
/// would, and its last line says how it ended.
fn watch_relay_bridge(
    mut status: crate::relay::StatusReader,
    records_root: PathBuf,
) -> std::thread::JoinHandle<RelayEnd> {
    std::thread::spawn(move || {
        let records = Records::at(records_root);
        let mut ended = None;
        while let Some(line) = status.next(None) {
            match line {
                crate::relay::BridgeStatus::ToolBuilt {
                    record,
                    packages,
                    images,
                } => {
                    for (arch, image) in images {
                        let _ = records.apply(
                            &record,
                            Environment::ADD_TOOL.with((packages.clone(), arch, image)),
                        );
                    }
                }
                line @ crate::relay::BridgeStatus::Ended { .. } => ended = Some(line),
                crate::relay::BridgeStatus::Session { .. } => {}
            }
        }
        RelayEnd::of(ended)
    })
}

/// Wait for whichever side of a served session ends first and tear the
/// other down; returns the line the audit log records.
///
/// The private lease (or legacy request connection) is the lifetime capability, in
/// both directions. When Terminal closes (or crashes) its end, this tears
/// down exactly the child it created: dropping the pty master is not enough
/// because a PID-namespace init may survive a terminal hangup. When the
/// child exits first, it is reaped at once and the connection closed, which
/// is how Terminal tells a finished session from the hand-off window in which
/// its PTY master also reports `EIO`.
fn finish_session(
    stream: &UnixStream,
    mut child: std::process::Child,
    relay: Option<RelayWatch>,
) -> String {
    match wait_for_guest_or_terminal(stream, &child) {
        LifetimeEnd::GuestExited => {
            let status = child.wait();
            // A relay session's bridge said how it ended: a terminal that
            // asked reads it before the lease closes, and tells a shell that
            // exited from a connection that was lost.
            if let Some(relay) = relay {
                let end = relay
                    .bridge
                    .join()
                    .unwrap_or_else(|_| RelayEnd::Lost("the relay bridge failed".into()));
                if relay.report
                    && let Ok(mut line) = serde_json::to_vec(&EndReport::from(&end))
                {
                    line.push(b'\n');
                    let _ = (&*stream).write_all(&line);
                }
            }
            let _ = stream.shutdown(std::net::Shutdown::Both);
            match status {
                Ok(status) => format!("exit {}", status.code().unwrap_or(-1)),
                Err(error) => format!("wait failed: {error}"),
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

/// Which side ended a hosted session first.
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
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor
    // or -1; it reads no memory from this process.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let Ok(raw) = std::os::fd::RawFd::try_from(raw) else {
        return wait_for_terminal(stream);
    };
    if raw < 0 {
        return wait_for_terminal(stream);
    }
    // SAFETY: pidfd_open returned a new descriptor that nothing else owns.
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
        // SAFETY: `fds` is a valid array of two initialised pollfd entries.
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

/// What a served session runs: `tilo-ush run` on the same store, for one
/// environment.
struct Launch<'a> {
    store: &'a Store,
    environment: Option<&'a str>,
    term: &'a str,
    host_root: &'a Path,
    command: &'a [String],
    /// The key the session's shell marks its runs with.
    runs: Option<&'a str>,
    editor: Option<&'a runtime_portal::editor::Socket>,
}

/// Start `tilo-ush run` with `slave` as its stdio, in its own session, with the
/// pty as its controlling terminal.
fn spawn_attached(
    launch: &Launch<'_>,
    slave: &std::fs::File,
    job: Option<runtime_portal::jobs::Slot>,
) -> std::io::Result<std::process::Child> {
    let exe = runtime_binary()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run");
    if let Some(record) = launch.environment {
        cmd.arg("--environment").arg(record);
    }
    cmd.arg("--host-root").arg(launch.host_root);
    if let Some(editor) = launch.editor {
        use std::os::unix::process::CommandExt;
        let fd = editor.as_fd().as_raw_fd();
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
    cmd.env("TERM", launch.term);
    match launch.runs {
        Some(key) => cmd.env(crate::runs::KEY_ENV, key),
        None => cmd.env_remove(crate::runs::KEY_ENV),
    };
    spawn_on_pty(cmd, slave, job)
}

/// Every session is an independent job of the app that asked for it (the
/// Terminal, for the person or an agent's tool run): it starts in a cgroup of
/// its own below the app's (`runtime_portal::jobs`), so a session that runs
/// out of memory, a runaway build in one tab, ends alone and the Terminal and
/// its other tabs keep running. The app is the one whose launcher opened this
/// connection. `None` (the session runs here, as before) outside a session's
/// delegated app tree.
fn session_job(
    transport: &SessionTransport<'_>,
    label: &str,
) -> Option<runtime_portal::jobs::Slot> {
    runtime_portal::jobs::slot_for_peer(transport.stream, label)
}

/// Start `cmd` with `slave` as its stdio, in its own session, with the pty as
/// its controlling terminal; in `job` when it has one.
fn spawn_on_pty(
    mut cmd: std::process::Command,
    slave: &std::fs::File,
    job: Option<runtime_portal::jobs::Slot>,
) -> std::io::Result<std::process::Child> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    if let Some(job) = job {
        job.place(&mut cmd);
    }

    cmd.stdin(Stdio::from(slave.try_clone()?));
    cmd.stdout(Stdio::from(slave.try_clone()?));
    cmd.stderr(Stdio::from(slave.try_clone()?));

    let slave_fd = slave.as_raw_fd();
    // SAFETY: `pre_exec` runs between fork and exec in the child. Both calls
    // are async-signal-safe and neither allocates. `setsid` is what makes the
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
            Ok(())
        });
    }
    cmd.spawn()
}

/// Open a pty pair sized for the caller's grid.
fn open_pty(cols: u16, rows: u16) -> std::io::Result<(std::fs::File, std::fs::File)> {
    use std::os::fd::FromRawFd;
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: both fds are out-parameters `openpty` fills in; `size` is a
    // properly initialised `winsize`.
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
    // SAFETY: `openpty` succeeded, so both are owned, open descriptors.
    Ok(unsafe {
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    })
}

/// Write a reply, optionally carrying one descriptor.
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

/// `sendmsg` with a single-descriptor `SCM_RIGHTS` control message.
pub use runtime_portal::fd_transport::{recv_with_fd, send_with_fd};

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_environment::{DEFAULT_RECORD, PackageId};

    /// A Host session's marked `bash` is the person's interactive login
    /// shell on a real pty: it reads their `~/.bash_profile`, and Tab
    /// completes a unique path prefix.
    #[test]
    fn a_marked_host_shell_is_a_login_shell_and_tab_completes() {
        use std::io::{Read as _, Write as _};
        let bash = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|dir| dir.join("bash"))
            .find(|path| path.is_file())
            .expect("bash, the shell whose completion is under test");
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".bash_profile"),
            "export FROM_PROFILE=login\n",
        )
        .unwrap();
        std::fs::create_dir(home.path().join("fixture-unique-directory")).unwrap();
        let session = tempfile::tempdir().unwrap();
        let rc = session.path().join("shell.bash");
        std::fs::write(&rc, crate::shell_start::bash_rc("", true)).unwrap();
        let account = crate::host::LoginAccount {
            user: "you".into(),
            home: home.path().to_path_buf(),
            shell: bash,
        };
        let mut cmd = crate::host::host_command(
            &account,
            "xterm-256color",
            &[],
            Some(&rc),
            Some("k3y0fth1ss3ss10n"),
        );
        cmd.env("HISTFILE", home.path().join("history"));
        cmd.env("INPUTRC", "/dev/null");
        let (mut master, slave) = open_pty(80, 24).unwrap();
        let mut child = spawn_on_pty(cmd, &slave, None).unwrap();
        drop(slave);
        let mut reader = master.try_clone().unwrap();
        let (seen_tx, seen) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 || seen_tx.send(buffer[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        master
            .write_all(b"echo \"[$FROM_PROFILE]\" fixture-uni\t\n")
            .unwrap();
        let mut output = Vec::new();
        let expected = "[login] fixture-unique-directory/";
        tilo_test_env::wait_for("the completed line's output", || {
            while let Ok(chunk) = seen.try_recv() {
                output.extend(chunk);
            }
            String::from_utf8_lossy(&output)
                .contains(expected)
                .then_some(())
        });
        master.write_all(b"exit\n").unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn guest_exit_and_failed_handoff_reap_the_exact_child() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::at(root.path().join("store"));
        store.ensure().unwrap();
        for fails in [false, true] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let transport = SessionTransport {
                stream: &server.try_clone().unwrap(),
                persistent: true,
                active: active.clone(),
            };
            let (master, slave) = open_pty(80, 24).unwrap();
            let child = spawn_on_pty(
                std::process::Command::new(if fails { "cat" } else { "true" }),
                &slave,
                None,
            )
            .unwrap();
            let pid = child.id();
            drop(slave);
            if fails {
                client.shutdown(std::net::Shutdown::Both).unwrap();
            }
            let handed = hand_off_session(
                &transport,
                &mut server,
                &store,
                &ServeResponse::Session {
                    session: "exact".into(),
                    environment: "default".into(),
                    relay_session: None,
                    runs: false,
                    editor_protocol: None,
                },
                master,
                child,
                None,
                || None,
            );
            if fails {
                assert!(handed.is_err());
            } else {
                handed.unwrap();
                let (_, mut rights) = runtime_portal::fd_transport::recv_with_fds(&client).unwrap();
                assert_eq!(rights.len(), 2);
                let mut lease = UnixStream::from(rights.pop().unwrap());
                lease
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                assert_eq!(
                    lease.read(&mut [0u8; 1]).unwrap(),
                    0,
                    "guest exit must close only its session lease"
                );
                client.write_all(b"control").unwrap();
                let mut bytes = [0u8; 7];
                server.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"control");
            }
            tilo_test_env::wait_until("the session to be reaped", || {
                active.load(std::sync::atomic::Ordering::Acquire) == 0
            });
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "the exact child must be reaped"
            );
        }
    }

    #[test]
    fn persistent_session_leases_cancel_and_reap_without_closing_control() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::at(root.path().join("store"));
        store.ensure().unwrap();
        let (client, mut server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let transport = SessionTransport {
            stream: &server.try_clone().unwrap(),
            persistent: true,
            active: active.clone(),
        };
        for index in 0..13 {
            let environment = if index % 2 == 0 { "default" } else { "host" };
            let (master, slave) = open_pty(80, 24).unwrap();
            let child = spawn_on_pty(std::process::Command::new("cat"), &slave, None).unwrap();
            let pid = child.id();
            drop(slave);
            let id = uuid::Uuid::new_v4().simple().to_string();
            hand_off_session(
                &transport,
                &mut server,
                &store,
                &ServeResponse::Session {
                    session: id.clone(),
                    environment: environment.into(),
                    relay_session: None,
                    runs: false,
                    editor_protocol: None,
                },
                master,
                child,
                None,
                || None,
            )
            .unwrap();
            let (bytes, mut rights) = runtime_portal::fd_transport::recv_with_fds(&client).unwrap();
            let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(response["session"], id);
            assert_eq!(response["environment"], environment);
            assert_eq!(response["protocol"], SERVICE_PROTOCOL);
            assert_eq!(rights.len(), 2);
            // Terminal holds the lease as a `SessionLifetime`, whose drop
            // shuts the connection down even while a reader's duplicate of
            // it is still open.
            let lease = runtime_portal::pending_connection::SessionLifetime::new(UnixStream::from(
                rights.pop().unwrap(),
            ));
            let reader_clone = lease.clone_for_reader().unwrap();
            let terminal = std::fs::File::from(rights.pop().unwrap());
            // SAFETY: terminal is an owned live PTY descriptor. isatty reads no user memory.
            assert_eq!(unsafe { libc::isatty(terminal.as_raw_fd()) }, 1);
            drop(lease);
            tilo_test_env::wait_until("the session to be reaped", || {
                active.load(std::sync::atomic::Ordering::Acquire) == 0
            });
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "session child must be reaped"
            );
            drop(reader_clone);
            drop(terminal);
            (&client).write_all(b"live").unwrap();
            let mut bytes = [0u8; 4];
            server.read_exact(&mut bytes).unwrap();
            assert_eq!(
                &bytes, b"live",
                "session cancellation must not consume control"
            );
        }
    }

    #[test]
    fn persistent_control_negotiates_before_open_and_preserves_the_authenticated_caller() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::at(root.path().join("store"));
        store.ensure().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        client
            .set_write_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let worker = std::thread::spawn(move || serve_one(&server, &store, Path::new("/")));
        client
            .write_all(&crate::host::encode_handoff("so.tilo.unrelated").unwrap())
            .unwrap();
        client
            .write_all(b"{\"op\":\"hello\",\"protocol\":2}\n")
            .unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        let accepted = serde_json::from_str::<serde_json::Value>(&hello).unwrap();
        let mut denied = Vec::new();
        if accepted["result"] == "ready" {
            for index in 0..13 {
                let requested_by = if index % 2 == 0 { "person" } else { "agent" };
                let request = serde_json::json!({
                    "op": "open-session", "target": {"kind": "host"},
                    "requested_by": requested_by
                });
                writeln!(client, "{request}").unwrap();
                let mut response = String::new();
                reader.read_line(&mut response).unwrap();
                denied.push(serde_json::from_str::<serde_json::Value>(&response).unwrap());
            }
            client.write_all(b"not-json\n").unwrap();
            let mut malformed = String::new();
            reader.read_line(&mut malformed).unwrap();
            let refusal: serde_json::Value = serde_json::from_str(&malformed).unwrap();
            assert_eq!(refusal["result"], "denied");
            assert_eq!(reader.read_line(&mut String::new()).unwrap(), 0);
        }
        drop(reader);
        drop(client);
        worker.join().unwrap().unwrap();
        assert_eq!(accepted["result"], "ready", "{accepted}");
        assert_eq!(accepted["protocol"], 2);
        assert_eq!(denied.len(), 13);
        for refusal in denied {
            assert_eq!(refusal["result"], "denied", "{refusal}");
        }
    }

    #[test]
    fn the_session_ends_when_the_guest_or_the_terminal_does() {
        // phase: a_guest_that_exits_first_ends_the_session_without_the_terminal
        {
            let (supervisor_end, _terminal_end) = UnixStream::pair().expect("socket pair");
            let mut child = std::process::Command::new("true")
                .spawn()
                .expect("spawn guest");
            assert_eq!(
                wait_for_guest_or_terminal(&supervisor_end, &child),
                LifetimeEnd::GuestExited,
                "an exited guest must end the session while Terminal still holds its end"
            );
            assert!(child.wait().expect("reap guest").success());
        }
        // phase: a_closed_terminal_ends_the_session_of_a_running_guest
        {
            let (supervisor_end, terminal_end) = UnixStream::pair().expect("socket pair");
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn guest");
            drop(terminal_end);
            assert_eq!(
                wait_for_guest_or_terminal(&supervisor_end, &child),
                LifetimeEnd::TerminalClosed
            );
            assert!(child.try_wait().expect("guest status").is_none());
            child.kill().expect("stop guest");
            let _ = child.wait();
        }
    }

    // A relay session's lease tells a terminal that asked whether its shell
    // exited or its connection was lost: only an exit closes its window.
    #[test]
    fn a_relay_sessions_lease_says_whether_its_shell_exited_or_the_connection_was_lost() {
        use crate::relay::BridgeStatus;
        let ended = |code: Option<i32>, reason: Option<&str>, superseded: bool| {
            Some(BridgeStatus::Ended {
                code,
                reason: reason.map(str::to_owned),
                superseded,
            })
        };
        assert_eq!(RelayEnd::of(ended(Some(0), None, false)), RelayEnd::Exited);
        assert_eq!(RelayEnd::of(ended(Some(3), None, false)), RelayEnd::Exited);
        assert_eq!(
            RelayEnd::of(ended(None, Some("relay unreachable"), false)),
            RelayEnd::Lost("relay unreachable".into())
        );
        assert_eq!(
            RelayEnd::of(ended(None, Some("attached on another terminal"), true)),
            RelayEnd::Superseded
        );
        assert!(
            matches!(RelayEnd::of(None), RelayEnd::Lost(_)),
            "a bridge that stopped without a word did not see the shell exit"
        );

        for (end, line, report) in [
            (RelayEnd::Exited, Some(r#"{"ended":"exited"}"#), true),
            (
                RelayEnd::Lost("relay unreachable".into()),
                Some(r#"{"ended":"lost","reason":"relay unreachable"}"#),
                true,
            ),
            (
                RelayEnd::Superseded,
                Some(r#"{"ended":"superseded"}"#),
                true,
            ),
            (RelayEnd::Exited, None, false),
        ] {
            let (supervisor, terminal) = UnixStream::pair().expect("socket pair");
            let child = std::process::Command::new("true")
                .spawn()
                .expect("spawn the bridge");
            let watch = RelayWatch {
                bridge: std::thread::spawn(move || end),
                report,
            };
            let termination = finish_session(&supervisor, child, Some(watch));
            assert!(termination.starts_with("exit"), "{termination}");
            let mut read = String::new();
            (&terminal)
                .read_to_string(&mut read)
                .expect("the lease closes after the report");
            match line {
                Some(line) => assert_eq!(read.trim_end(), line),
                None => assert!(
                    read.is_empty(),
                    "a terminal that did not ask is told nothing"
                ),
            }
        }
    }

    #[test]
    fn cancelling_a_terminal_connection_kills_and_reaps_its_guest_with_clones_open() {
        let (supervisor, terminal) = UnixStream::pair().expect("socket pair");
        let _reader_clone = terminal.try_clone().expect("reader lifetime clone");
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn guest");
        let pid = libc::pid_t::try_from(child.id()).expect("guest pid");
        terminal
            .shutdown(std::net::Shutdown::Both)
            .expect("cancel pending or accepted session");
        let termination = finish_session(&supervisor, child, None);
        assert!(
            termination.starts_with("terminal closed; exit"),
            "{termination}"
        );
        // SAFETY: waitpid writes only to this local status; WNOHANG cannot block.
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) },
            -1,
            "production teardown must reap the child, not leave a zombie"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn failed_descriptor_handoff_kills_and_reaps_the_unclaimed_child() {
        let (mut supervisor, terminal) = UnixStream::pair().expect("socket pair");
        terminal
            .shutdown(std::net::Shutdown::Both)
            .expect("cancel before reply");
        let (master, _slave) = open_pty(80, 24).expect("PTY");
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn guest or bridge");
        let pid = libc::pid_t::try_from(child.id()).expect("child pid");
        let handed = reply(
            &mut supervisor,
            &ServeResponse::Session {
                session: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                environment: "default".into(),
                relay_session: None,
                runs: false,
                editor_protocol: None,
            },
            Some(master.as_fd()),
        );
        assert!(
            handed.is_err(),
            "the cancelled terminal cannot accept a descriptor"
        );
        // The pending guard owns the child until a handoff is accepted.
        drop(PendingSessionChild(Some(child)));
        // SAFETY: waitpid writes only to this local status; WNOHANG cannot block.
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) },
            -1,
            "the production failed-handoff path must reap the child"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    /// Terminal parses this reply strictly (`UshSessionReply` in
    /// `apps/terminal/src/main_body.rs`, unknown fields refused), and its test
    /// `a_session_that_marks_its_runs_opens_and_keeps_the_key` sends these
    /// bytes. A field added here must be added there too, or every open of a
    /// session that sends it fails as a malformed reply.
    #[test]
    fn a_session_reply_keeps_its_wire_form() {
        let reply = ServeResponse::Session {
            session: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            environment: "default".into(),
            relay_session: None,
            runs: true,
            editor_protocol: None,
        };
        assert_eq!(
            serde_json::to_string(&reply).unwrap(),
            "{\"result\":\"session\",\"session\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"environment\":\"default\",\"runs\":true}"
        );
    }

    fn open(target: Target) -> ServeRequest {
        ServeRequest::OpenSession {
            target,
            cols: 80,
            rows: 24,
            term: None,
            command: Vec::new(),
            requested_by: crate::host::RequestedBy::Person,
            relay_session: None,
            runs: None,
            editor_protocol: None,
            end_report: false,
        }
    }

    #[test]
    fn serve_builds_environments_and_refuses_the_host_target() {
        assert_eq!(decide_serve(&open(Target::default_environment())), Ok(None));
        assert_eq!(
            decide_serve(&open(Target::Environment {
                record: Some("work".into()),
                location: tilo_environment::Location::ThisDevice,
            })),
            Ok(Some("work".into()))
        );
        let err = decide_serve(&open(Target::Host)).unwrap_err();
        assert!(err.starts_with("not available"), "{err}");

        // The host target says when an agent asked for it; leaving it out is
        // the person.
        let host: ServeRequest = serde_json::from_str(
            r#"{"op":"open-session","target":{"kind":"host"},"requested_by":"agent"}"#,
        )
        .unwrap();
        assert!(matches!(
            host,
            ServeRequest::OpenSession {
                target: Target::Host,
                requested_by: crate::host::RequestedBy::Agent,
                ..
            }
        ));
        assert!(matches!(
            open(Target::Host),
            ServeRequest::OpenSession {
                requested_by: crate::host::RequestedBy::Person,
                ..
            }
        ));
        let refused: ServeResponse =
            serde_json::from_str(r#"{"result":"host-not-allowed","reason":"ask"}"#).unwrap();
        assert_eq!(
            refused,
            ServeResponse::HostNotAllowed {
                reason: "ask".into()
            }
        );
        let err = decide_serve(&open(Target::Environment {
            record: Some("../../etc".into()),
            location: tilo_environment::Location::ThisDevice,
        }))
        .unwrap_err();
        assert!(err.contains("does not name"), "{err}");
    }

    #[test]
    fn a_session_request_names_its_target_and_may_name_a_program() {
        let shell: ServeRequest =
            serde_json::from_str(r#"{"op":"open-session","target":{"kind":"environment"}}"#)
                .unwrap();
        assert!(matches!(
            shell,
            ServeRequest::OpenSession { ref target, ref command, .. }
                if *target == Target::default_environment() && command.is_empty()
        ));
        let program: ServeRequest = serde_json::from_str(
            r#"{"op":"open-session","target":{"kind":"environment","record":"work"},"command":["htop","-d","5"]}"#,
        )
        .unwrap();
        assert!(matches!(
            program,
            ServeRequest::OpenSession { ref command, .. } if command == &["htop", "-d", "5"]
        ));
        assert_eq!(decide_serve(&program).unwrap(), Some("work".into()));
    }

    struct FixtureImages(PathBuf);
    impl Images for FixtureImages {
        fn tree(&self, id: &PackageId) -> Result<PathBuf, String> {
            let tree = self.0.join(id.to_hex());
            tree.is_dir()
                .then_some(tree)
                .ok_or_else(|| format!("{id} is not here"))
        }
        fn check_tool(&self, tool: &PackageId, _runtime: &PackageId) -> Result<(), String> {
            if *tool == PackageId::from_bytes([9; 32]) {
                Err("tool was built for another runtime".into())
            } else {
                Ok(())
            }
        }
        fn current_runtime(&self) -> Result<(String, PackageId), String> {
            Ok(("2026.10".into(), PackageId::from_bytes([1; 32])))
        }
    }

    fn session_in(dir: &Path) -> Session {
        let images = dir.join("images");
        for byte in [1_u8, 2] {
            let tree = images.join(PackageId::from_bytes([byte; 32]).to_hex());
            std::fs::create_dir_all(tree.join("usr/bin")).unwrap();
            std::fs::create_dir_all(tree.join("etc")).unwrap();
        }
        std::fs::write(
            images
                .join(PackageId::from_bytes([1; 32]).to_hex())
                .join("usr/bin/bash"),
            b"",
        )
        .unwrap();
        let store = Store::at(dir.join("ush"));
        store.ensure().unwrap();
        Session::new(
            store,
            SessionOptions::default(),
            Arc::new(FixtureImages(images)),
            Arc::new(tilo_environment::NoToolBuilder),
        )
    }

    #[test]
    fn the_environment_directory_lists_creates_and_renames_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("ush"));
        store.ensure().unwrap();
        let records = Records::at(store.records_root());
        let mut default = Environment::new(
            tilo_environment::DEFAULT_NAME,
            "2026.10",
            tilo_environment::host_arch(),
            &PackageId::from_bytes([1; 32]),
        );
        default.tools.clear();
        records.create(DEFAULT_RECORD, &default).unwrap();

        let listed = directory(&ServeRequest::ListEnvironments, &store).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key, DEFAULT_RECORD);

        let created = directory(
            &ServeRequest::CreateEnvironment {
                name: " Writing ".into(),
            },
            &store,
        )
        .unwrap();
        let writing = created
            .iter()
            .find(|entry| entry.key != DEFAULT_RECORD)
            .expect("the new environment is listed");
        assert_eq!(writing.name, "Writing");
        assert!(writing.tools.is_empty() && writing.grants.is_empty());
        assert_eq!(
            records.load(&writing.key).unwrap().unwrap().runtime,
            default.runtime,
            "a new environment starts on the default environment's runtime"
        );

        let renamed = directory(
            &ServeRequest::EnvironmentAction {
                record: writing.key.clone(),
                action: "rename".into(),
                args: serde_json::json!({"name": "Prose"}),
            },
            &store,
        )
        .unwrap();
        assert!(renamed.iter().any(|entry| entry.name == "Prose"));

        let granted_dir = dir.path().join("src");
        std::fs::create_dir_all(&granted_dir).unwrap();
        let granted = directory(
            &ServeRequest::EnvironmentAction {
                record: writing.key.clone(),
                action: "grant-dir".into(),
                args: serde_json::json!({"path": granted_dir.display().to_string(), "write": true}),
            },
            &store,
        )
        .unwrap();
        let entry = granted.iter().find(|e| e.key == writing.key).unwrap();
        assert_eq!(
            entry.grants,
            vec![(granted_dir.display().to_string(), true)]
        );
        assert!(
            directory(
                &ServeRequest::EnvironmentAction {
                    record: writing.key.clone(),
                    action: "grant-dir".into(),
                    args: serde_json::json!({"path": "/usr", "write": true}),
                },
                &store,
            )
            .is_err(),
            "the record's own rules still refuse what the guest builds itself"
        );
        assert!(
            directory(
                &ServeRequest::EnvironmentAction {
                    record: writing.key.clone(),
                    action: "set-runtime".into(),
                    args: serde_json::Value::Null,
                },
                &store,
            )
            .is_err(),
            "only the actions an app may ask for here are applied"
        );
        assert!(
            directory(
                &ServeRequest::CreateEnvironment { name: "  ".into() },
                &store
            )
            .is_err()
        );
    }

    #[test]
    fn the_plan_comes_from_the_record_its_images_and_its_grants() {
        let dir = tempfile::tempdir().unwrap();
        let session = session_in(dir.path());
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("SHELL", "/bin/bash");

        // First use creates the default environment on the current runtime.
        let resolved = session.resolve().unwrap();
        assert_eq!(resolved.key, DEFAULT_RECORD);
        assert_eq!(resolved.images.len(), 1);
        assert!(resolved.images[0].0.starts_with("runtime "));

        let records = session.records();
        let granted = dir.path().join("src");
        std::fs::create_dir_all(&granted).unwrap();
        records
            .apply(
                DEFAULT_RECORD,
                Environment::ADD_TOOL.with((
                    vec!["pandoc".into()],
                    tilo_environment::host_arch().to_owned(),
                    PackageId::from_bytes([2; 32]).to_string(),
                )),
            )
            .unwrap();
        records
            .apply(
                DEFAULT_RECORD,
                Environment::GRANT_DIR.with((granted.display().to_string(), true)),
            )
            .unwrap();

        let resolved = session.resolve().unwrap();
        assert!(
            resolved.images[0].0.contains("pandoc"),
            "the tool stacks on top"
        );
        assert!(resolved.images[1].0.starts_with("runtime "));
        let plan = session
            .plan(
                &resolved,
                PathBuf::from("/tmp/b.sock"),
                PathBuf::from("/tmp/f.bpf"),
            )
            .unwrap();
        assert_eq!(plan.trusted_dirs, vec![granted]);
        assert!(plan.readable_dirs.is_empty());
        assert_eq!(
            plan.guest_home,
            session.store.environment_home(DEFAULT_RECORD)
        );
        assert_eq!(plan.var_dir, session.store.environment_var(DEFAULT_RECORD));
        assert_eq!(plan.shell, PathBuf::from("/usr/bin/bash"));
        assert_eq!(plan.environment, tilo_environment::DEFAULT_NAME);
        assert!(
            plan.masked
                .contains(&session.store.root().canonicalize().unwrap())
        );
    }

    fn add_tool(session: &Session, package: &str, arch: &str, byte: u8) {
        session
            .records()
            .apply(
                DEFAULT_RECORD,
                Environment::ADD_TOOL.with((
                    vec![package.into()],
                    arch.into(),
                    PackageId::from_bytes([byte; 32]).to_string(),
                )),
            )
            .unwrap();
    }

    struct BuildsTwo;
    impl ToolBuilder for BuildsTwo {
        fn build(
            &self,
            runtime: &PackageId,
            _packages: &[String],
            _progress: &mut dyn Write,
        ) -> Result<PackageId, tilo_environment::ToolBuildError> {
            if *runtime == PackageId::from_bytes([1; 32]) {
                Ok(PackageId::from_bytes([2; 32]))
            } else {
                Err(tilo_environment::ToolBuildError::Failed(
                    "wrong runtime".into(),
                ))
            }
        }
    }

    #[test]
    fn a_tool_without_an_image_here_is_built_here_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session_in(dir.path());
        session.builder = Arc::new(BuildsTwo);
        session.resolve().unwrap();
        let other = if tilo_environment::host_arch() == "x86_64" {
            "aarch64"
        } else {
            "x86_64"
        };
        add_tool(&session, "pandoc", other, 5);

        let resolved = session.resolve().unwrap();
        assert!(
            resolved.images[0].0.contains("pandoc"),
            "{:?}",
            resolved.images
        );
        let tool = &resolved.environment.tools[0];
        assert_eq!(
            tool.image(tilo_environment::host_arch()).unwrap(),
            Some(PackageId::from_bytes([2; 32])),
            "the image built here is recorded next to the other architecture's"
        );
        assert_eq!(
            tool.image(other).unwrap(),
            Some(PackageId::from_bytes([5; 32]))
        );
    }

    #[test]
    fn a_tool_that_cannot_be_used_or_built_stops_the_session_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let session = session_in(dir.path());
        session.resolve().unwrap();
        let arch = tilo_environment::host_arch();
        add_tool(&session, "gone", arch, 7);
        let err = session.resolve().unwrap_err().to_string();
        assert!(
            err.contains("is not here") && err.contains("not available"),
            "{err}"
        );

        session
            .records()
            .apply(
                DEFAULT_RECORD,
                Environment::REMOVE_TOOL.with(vec!["gone".into()]),
            )
            .unwrap();
        add_tool(&session, "elsewhere", arch, 9);
        let err = session.resolve().unwrap_err().to_string();
        assert!(err.contains("another runtime"), "{err}");

        let mut named = session_in(dir.path());
        named.opts.environment = Some("work".into());
        let err = named.resolve().unwrap_err().to_string();
        assert!(err.contains("no environment"), "{err}");
    }

    #[test]
    fn a_one_shot_command_never_restarts_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let interactive = session_in(dir.path());
        assert!(interactive.restarts_in_place());
        let mut one_shot = session_in(dir.path());
        one_shot.opts.command = vec!["/bin/sh".into(), "-c".into(), "restart".into()];
        assert!(!one_shot.restarts_in_place());
    }

    #[test]
    fn the_guest_home_is_seeded_once() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        seed_guest_home(&home).unwrap();
        let rc = home.join(".bashrc");
        assert!(rc.is_file());
        std::fs::write(&rc, b"# edited by the user\n").unwrap();
        seed_guest_home(&home).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rc).unwrap(),
            "# edited by the user\n"
        );
    }

    #[test]
    fn the_guest_shell_falls_back_rather_than_failing_the_session() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::write(dir.path().join("usr/bin/bash"), b"#!elf").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("SHELL", "/usr/bin/definitely-not-here");
        assert_eq!(
            guest_shell(&[dir.path().to_path_buf()]),
            PathBuf::from("/usr/bin/bash")
        );
    }

    #[test]
    fn a_pty_opens_at_the_requested_size_and_survives_the_socket() {
        // phase: a_pty_pair_opens_at_the_requested_size
        {
            let (master, _slave) = open_pty(120, 40).unwrap();
            let mut size: libc::winsize = unsafe { std::mem::zeroed() };
            // SAFETY: `master` is a live pty master descriptor.
            let rc = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
            assert_eq!(rc, 0);
            assert_eq!((size.ws_col, size.ws_row), (120, 40));
        }
        // phase: a_descriptor_survives_the_socket
        // The descriptor really crosses the socket — this is the whole mechanism
        // by which a sandboxed app gets a terminal without getting a namespace.
        {
            let (a, b) = UnixStream::pair().unwrap();
            let (master, mut slave) = open_pty(80, 24).unwrap();

            send_with_fd(&a, b"{\"result\":\"session\"}\n", &master).unwrap();
            let (payload, fd) = recv_with_fd(&b).unwrap();
            assert!(String::from_utf8_lossy(&payload).contains("session"));
            let fd = fd.expect("no descriptor arrived");

            // Write on the slave, read it back through the received master. If the
            // descriptor were not the same terminal this would block or fail.
            slave.write_all(b"hello\n").unwrap();
            slave.flush().unwrap();
            let mut buf = [0u8; 32];
            let n = std::fs::File::from(fd).read(&mut buf).unwrap();
            assert!(n > 0, "nothing came back through the received descriptor");
            assert!(String::from_utf8_lossy(&buf[..n]).contains("hello"));
        }
    }
}

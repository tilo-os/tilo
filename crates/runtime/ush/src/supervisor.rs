//! The supervisor: one session, from `ush` to the shell exiting.
//!
//! Runs on the host, in the user's session, outside every app sandbox — the
//! only thing here that holds `unshare` and `mount`, and it hands none of
//! that to the guest; see the trust boundary in [`crate::lib`](crate).
//!
//! ```text
//! ush
//!  ├─ resolve the environment record (the default is made on first use)
//!  ├─ resolve its images to read-only trees                (crate::images)
//!  ├─ compile the guest's seccomp filter                   (crate::seccomp)
//!  ├─ start the broker on a socket in the session dir      (crate::broker)
//!  ├─ spawn bwrap ─┬─ unshare, stack the images, pivot
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
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_clock::unix_now;
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
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            environment: None,
            command: Vec::new(),
            term: std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
            host_root: PathBuf::from("/"),
            verbose: false,
        }
    }
}

/// Printed under `bwrap`'s own one-line reason, which names a mount option
/// rather than a cause. Short lines: it lands in a terminal of any width.
const SANDBOX_SETUP_FAILED: &str = "\
ush: the shell's sandbox could not be set up, so no shell started;
ush: bwrap's reason is above. An overlay mount refused with \"Invalid
ush: argument\" usually means the images sit on a filesystem overlayfs
ush: cannot stack on (another overlay): `dmesg` shows why.";

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
        let policy = Policy::new(resolved.environment.grants.0.clone());
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
        Ok(GuestPlan {
            shell: guest_shell(&images),
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
            runtime_bin: runtime_binary()?,
            broker_socket,
            seccomp_blob,
            command: self.opts.command.clone(),
            term: self.opts.term.clone(),
            env: Vec::new(),
        })
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
            },
        )?;

        let mut plan = self.plan(resolved, broker_socket, seccomp_blob)?;
        plan.prepare()?;
        seed_guest_home(&plan.guest_home)?;

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
        let shell = plan.shell.clone();
        let user_command = std::mem::take(&mut plan.command);
        let mut command = vec![
            format!("{}/tilo-ush", crate::GUEST_BIN_DIR),
            "guest-init".into(),
            "--".into(),
        ];
        if user_command.is_empty() {
            command.push(shell.to_string_lossy().into_owned());
            command.push("-i".into());
        } else {
            command.extend(user_command);
        }
        plan.command = command;

        let mut child = crate::guest::spawn(&plan)?;
        let status = child.wait()?;
        let code = status.code().unwrap_or(1);
        if code != 0 && !broker.guest_connected() {
            eprintln!("{SANDBOX_SETUP_FAILED}");
        }
        let restart = broker.restart_requested();
        drop(broker);
        Ok((code, restart))
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
    },
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
    },
    Denied {
        reason: String,
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
    let ServeRequest::OpenSession { target, .. } = req;
    match target {
        Target::Host => Err(
            "not available: this supervisor builds environments only; the host target \
             (your own login shell) is not served here"
                .into(),
        ),
        Target::Environment { record: None } => Ok(None),
        Target::Environment {
            record: Some(record),
        } if is_record_key(record) => Ok(Some(record.clone())),
        Target::Environment {
            record: Some(record),
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
    let listener = if let Some(listener) = systemd_listener(socket)? {
        listener
    } else {
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        }
        listener
    };
    tracing::info!(socket = %socket.display(), "ush: serving guests");

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

/// Consume systemd's one pre-bound service socket when activation metadata is
/// addressed to this process. Invalid matching metadata is fatal: falling back
/// to unlinking and rebinding would race queued clients and could replace an
/// endpoint systemd already exposed.
#[allow(
    clippy::disallowed_methods,
    reason = "systemd socket activation: LISTEN_* must not leak to guest children; \
              cleared right after fd 3 is taken, and `serve` calls this before it \
              spawns any connection thread, from a `tilo-ush serve` main thread \
              that has started no other threads"
)]
fn systemd_listener(expected: &Path) -> anyhow::Result<Option<UnixListener>> {
    const SYSTEMD_LISTEN_FD: libc::c_int = 3;

    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id());
    if !pid_matches {
        return Ok(None);
    }
    anyhow::ensure!(
        std::env::var("LISTEN_FDS").as_deref() == Ok("1"),
        "systemd must pass exactly one ush listener"
    );
    // SAFETY: fcntl only validates the scalar descriptor.
    anyhow::ensure!(
        unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_GETFD) } != -1,
        "systemd ush listener fd is closed"
    );
    // SAFETY: validated activation metadata transfers ownership of fd 3 once.
    let listener = unsafe { UnixListener::from_raw_fd(SYSTEMD_LISTEN_FD) };
    anyhow::ensure!(
        listener.local_addr()?.as_pathname() == Some(expected),
        "systemd ush listener has an unexpected path"
    );
    // Guest processes must never inherit the host supervisor endpoint.
    anyhow::ensure!(
        unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) } != -1,
        "cannot mark systemd ush listener close-on-exec"
    );
    // SAFETY: serve is entered before any worker thread is started.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    Ok(Some(listener))
}

fn serve_one(stream: &UnixStream, store: &Store, host_root: &Path) -> anyhow::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let req: ServeRequest = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            return reply(
                &mut writer,
                &ServeResponse::Denied {
                    reason: format!("malformed request: {e}"),
                },
                None,
            );
        }
    };
    let environment = match decide_serve(&req) {
        Ok(e) => e,
        Err(reason) => {
            AuditLog::at(store.audit_dir()).append(
                &crate::audit::AuditRecord::new("serve", "-", "open-session", false)
                    .because(&reason),
            );
            return reply(&mut writer, &ServeResponse::Denied { reason }, None);
        }
    };

    let ServeRequest::OpenSession {
        cols,
        rows,
        term,
        command,
        ..
    } = req;
    let (master, slave) = open_pty(cols, rows)?;

    let id = uuid::Uuid::new_v4().simple().to_string();
    let record = environment
        .clone()
        .unwrap_or_else(|| tilo_environment::DEFAULT_RECORD.to_owned());
    AuditLog::at(store.audit_dir()).append(&crate::audit::AuditRecord::new(
        &id,
        &record,
        "open-session",
        true,
    ));
    let launch = Launch {
        store,
        environment: environment.as_deref(),
        term: term.as_deref().unwrap_or("xterm-256color"),
        host_root,
        command: &command,
    };

    // The guest gets the slave as its stdio; the app gets the master. A *child
    // process*, not a thread: attaching a pty means `dup2` onto fds 0/1/2 and
    // `setsid`, both process-wide — on a thread that would steal the
    // supervisor's own stdio and controlling terminal (and under a test harness, the harness's).
    let child = spawn_attached(&launch, &slave)?;
    // The child already holds its own clones for stdio, so the original can
    // close as soon as the spawn is done.
    drop(slave);
    reply(
        &mut writer,
        &ServeResponse::Session {
            session: id.clone(),
            environment: record.clone(),
        },
        Some(master.as_raw_fd()),
    )?;
    // Hold the master open until the app has it: closing before the descriptor
    // is received would hand over a dead terminal.
    drop(master);
    // The request connection is the lifetime capability for this guest, in
    // both directions. When Terminal closes (or crashes) its end, this worker
    // tears down exactly the child it created: dropping the pty master is not
    // enough because the PID-namespace init may survive a terminal hangup.
    // When the guest exits first, this worker reaps it at once and closes the
    // connection, which is how Terminal tells a finished session from the
    // hand-off window in which its PTY master also reports `EIO`.
    let mut child = child;
    let termination = match wait_for_guest_or_terminal(stream, &child) {
        LifetimeEnd::GuestExited => {
            let status = child.wait();
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
    };
    AuditLog::at(store.audit_dir()).append(
        &crate::audit::AuditRecord::new(&id, &record, "close-session", true).because(termination),
    );
    Ok(())
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
}

/// Start `tilo-ush run` with `slave` as its stdio, in its own session, with the
/// pty as its controlling terminal.
fn spawn_attached(
    launch: &Launch<'_>,
    slave: &std::fs::File,
) -> std::io::Result<std::process::Child> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let exe = runtime_binary()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run");
    if let Some(record) = launch.environment {
        cmd.arg("--environment").arg(record);
    }
    cmd.arg("--host-root").arg(launch.host_root);
    if !launch.command.is_empty() {
        cmd.arg("--").args(launch.command);
    }
    cmd.env(crate::store::STORE_ENV, launch.store.root());
    cmd.env(crate::store::RECORDS_ENV, launch.store.records_root());
    cmd.env("TERM", launch.term);
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
    fd: Option<RawFd>,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    match fd {
        None => {
            stream.write_all(&bytes)?;
            stream.flush()?;
        }
        Some(fd) => send_with_fd(stream.as_raw_fd(), &bytes, fd)?,
    }
    Ok(())
}

/// `sendmsg` with a single-descriptor `SCM_RIGHTS` control message.
pub use runtime_portal::fd_transport::{recv_with_fd, send_with_fd};

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_environment::{DEFAULT_RECORD, PackageId};

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

    fn open(target: Target) -> ServeRequest {
        ServeRequest::OpenSession {
            target,
            cols: 80,
            rows: 24,
            term: None,
            command: Vec::new(),
        }
    }

    #[test]
    fn serve_builds_environments_and_refuses_the_host_target() {
        assert_eq!(decide_serve(&open(Target::default_environment())), Ok(None));
        assert_eq!(
            decide_serve(&open(Target::Environment {
                record: Some("work".into())
            })),
            Ok(Some("work".into()))
        );
        let err = decide_serve(&open(Target::Host)).unwrap_err();
        assert!(err.starts_with("not available"), "{err}");
        let err = decide_serve(&open(Target::Environment {
            record: Some("../../etc".into()),
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

            send_with_fd(
                a.as_raw_fd(),
                b"{\"result\":\"session\"}\n",
                master.as_raw_fd(),
            )
            .unwrap();
            let (payload, fd) = recv_with_fd(b.as_raw_fd()).unwrap();
            assert!(String::from_utf8_lossy(&payload).contains("session"));
            let fd = fd.expect("no descriptor arrived");

            // Write on the slave, read it back through the received master. If the
            // descriptor were not the same terminal this would block or fail.
            slave.write_all(b"hello\n").unwrap();
            slave.flush().unwrap();
            let mut buf = [0u8; 32];
            // SAFETY: `fd` is the descriptor just received and still open.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            assert!(n > 0, "nothing came back through the received descriptor");
            // `n > 0` was just asserted above, so the cast below cannot lose the sign.
            #[allow(clippy::cast_sign_loss, reason = "n > 0 was just asserted above")]
            let n = n as usize;
            assert!(String::from_utf8_lossy(&buf[..n]).contains("hello"));
            // SAFETY: we own `fd` now.
            unsafe { libc::close(fd) };
        }
    }
}

//! The supervisor: one session, from `ush` to the shell exiting.
//!
//! Runs on the host, in the user's session, outside every app sandbox — the
//! only thing here that holds `unshare` and `mount`, and it hands none of
//! that to the guest; see the trust boundary in [`crate::lib`](crate).
//!
//! ```text
//! ush
//!  ├─ ensure the store and the layer                       (crate::layer)
//!  ├─ compile the guest's seccomp filter                   (crate::profile)
//!  ├─ start the broker on a socket in the session dir      (crate::broker)
//!  ├─ exec bwrap ─┬─ unshare, mount the overlays, pivot
//!  │              ├─ install the seccomp filter        ← the boundary
//!  │              └─ exec tilo-ush guest-init ─┬─ install Landlock
//!  │                                          └─ exec the shell
//!  ├─ wait
//!  └─ undo one-time installs, burn if asked, restart if asked
//! ```
//!
//! # `serve` — how an app reaches a guest
//!
//! A Tilo app cannot build a namespace (its seccomp filter denies the
//! syscalls and its sandbox has no path to this binary), so `apps/terminal`
//! asks for a guest instead of starting one: `tilo-ush serve` listens on a
//! socket `tilo-launcher` passes every app as a connected descriptor, and one
//! line of JSON gets back a pty master over `SCM_RIGHTS` — a terminal, never
//! a namespace, a capability, or a `dsh` session
//! ([`crate::profile::Profile::reachable_from_app_launch`] gates every request).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::FromRawFd;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use tilo_clock::unix_now;

use crate::audit::AuditLog;
use crate::broker::{Broker, BrokerState};
use crate::guest::GuestPlan;
use crate::layer::Layer;
use crate::policy::Policy;
use crate::profile::Profile;
use crate::store::Store;

/// What the user asked for on the command line.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub profile: Profile,
    /// `--ephemeral`: a layer of this session's own, destroyed on the way out.
    pub ephemeral: bool,
    /// `ush -c "…"`. Empty means an interactive shell.
    pub command: Vec<String>,
    pub term: String,
    /// The host root to shadow. `/` outside tests.
    pub base: PathBuf,
    pub verbose: bool,
    /// The terminal's shell executes in the caller's home. The `ush` CLI
    /// leaves this off so a package runtime still sees only the store's copy.
    pub user_home: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            profile: Profile::Ush,
            ephemeral: false,
            command: Vec::new(),
            term: std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
            base: PathBuf::from("/"),
            verbose: false,
            user_home: false,
        }
    }
}

/// Printed under `bwrap`'s own one-line reason, which names a mount option
/// rather than a cause. Short lines: it lands in a terminal of any width.
const SANDBOX_SETUP_FAILED: &str = "\
ush: the shell's sandbox could not be set up, so no shell started;
ush: bwrap's reason is above. An overlay mount refused with \"Invalid
ush: argument\" usually means /usr is already a stacked overlay (a
ush: system extension merged over the image): the kernel stacks no
ush: deeper, and `dmesg` shows \"maximum fs stacking depth exceeded\".";

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

/// The shell a guest hosts. The host's `$SHELL` is a hint, not guaranteed to
/// exist in the guest — a shell that isn't falls back to `/usr/bin/bash`
/// then `/bin/sh`, so a session never fails to start over a preference.
pub fn guest_shell(base_visible: &Path) -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(s) = std::env::var_os("SHELL") {
        candidates.push(PathBuf::from(s));
    }
    candidates.push(PathBuf::from("/usr/bin/bash"));
    candidates.push(PathBuf::from("/bin/sh"));
    for c in &candidates {
        let on_host = base_visible.join(c.strip_prefix("/").unwrap_or(c));
        if on_host.exists() {
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
}

/// Where `~` points, and which directory under it must stay hidden.
///
/// A terminal session (`user_home`) binds the caller's real home so a typed
/// `echo ok > ~/probe` creates that file where the user can see it. The store
/// lives under that home and holds `policy.json`, so it is covered. The `ush`
/// CLI leaves `user_home` off and keeps the isolated copy.
fn session_directories(
    user_home: bool,
    home_mountpoint: &Path,
    store_home: PathBuf,
    store_root: &Path,
) -> (PathBuf, Option<PathBuf>) {
    if !user_home {
        return (store_home, None);
    }
    let masked_store = store_root
        .starts_with(home_mountpoint)
        .then(|| store_root.to_path_buf());
    (home_mountpoint.to_path_buf(), masked_store)
}

impl Session {
    pub fn new(store: Store, opts: SessionOptions) -> Self {
        Self {
            store,
            opts,
            session_id: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    fn layer(&self) -> Layer {
        if self.opts.ephemeral {
            self.store.ephemeral_layer(&self.session_id)
        } else {
            self.store.persistent_layer()
        }
    }

    fn lower_layers(&self) -> Vec<PathBuf> {
        if self.opts.ephemeral {
            let persistent = self.store.persistent_layer();
            if persistent.exists() {
                return vec![persistent.path().to_path_buf()];
            }
        }
        Vec::new()
    }

    /// Build the plan for this session: everything the guest will be is decided
    /// here, from the store and the policy — never from anything the guest said.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime binary's own path cannot be resolved
    /// (see [`runtime_binary`]).
    pub fn plan(
        &self,
        broker_socket: PathBuf,
        seccomp_blob: PathBuf,
    ) -> std::io::Result<GuestPlan> {
        let policy = Policy::load(&self.store.policy_path());
        let now = Some(unix_now());
        // Resolved, not as `$HOME` spells it: on an ostree system `/home` is a
        // symlink to `var/home`, and `bwrap` refuses to mount through a symlink.
        // `/var/home/you` is real, and the base's link still resolves to it too.
        let home_mountpoint = xdg_paths::home()
            .map_or_else(|| PathBuf::from("/root"), |h| h.canonicalize().unwrap_or(h));
        let store_root = self
            .store
            .root()
            .canonicalize()
            .unwrap_or_else(|_| self.store.root().to_path_buf());
        let (guest_home, masked_store) = session_directories(
            self.opts.user_home,
            &home_mountpoint,
            self.store.guest_home(),
            &store_root,
        );

        Ok(GuestPlan {
            profile: self.opts.profile,
            base: self.opts.base.clone(),
            upper: self.layer().path().to_path_buf(),
            lower_layers: self.lower_layers(),
            workdir_root: self.store.session_dir(&self.session_id),
            guest_home,
            home_mountpoint,
            masked_store,
            trusted_dirs: policy.trusted_dirs(now),
            readable_dirs: policy.readable_dirs(now),
            runtime_bin: runtime_binary()?,
            broker_socket,
            seccomp_blob,
            command: self.opts.command.clone(),
            shell: guest_shell(&self.opts.base),
            term: self.opts.term.clone(),
            env: Vec::new(),
        })
    }

    /// May a restart request rebuild this session, or is it a one-shot? Only an
    /// interactive session restarts: `ush -c "…"` has a command with side
    /// effects, and rebuilding would re-run it — `pkg burn` asking for a restart
    /// would burn, rebuild, burn, forever.
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
            // A restart is a new namespace: a new session directory and broker socket.
            // Reusing the old ids would leave a stale overlay workdir the next mount refuses.
            self.session_id = uuid::Uuid::new_v4().simple().to_string();
        }
        Ok(code)
    }

    fn run_once(&self) -> anyhow::Result<(i32, bool)> {
        let layer = self.layer();
        // Before anything is written: a store on tmpfs mounts every bind and then
        // dies at the overlay with only `Invalid argument`, far from the cause.
        crate::layer::upper_filesystem_supported(layer.path())?;
        layer.create(&self.opts.base)?;
        // Seeding writes a guest `.bashrc`. That belongs in the isolated home,
        // never in the directory the user actually lives in.
        if !self.opts.user_home {
            seed_guest_home(&self.store.guest_home())?;
        }

        let session_dir = self.store.session_dir(&self.session_id);
        std::fs::create_dir_all(&session_dir)?;
        let seccomp_blob = crate::profile::write_guest_filter(&session_dir, self.opts.profile)?;
        let broker_socket = session_dir.join("broker.sock");

        let broker = Broker::start(
            broker_socket.clone(),
            BrokerState {
                session_id: self.session_id.clone(),
                profile: self.opts.profile,
                policy_path: self.store.policy_path(),
                policy: Policy::load(&self.store.policy_path()),
                audit: AuditLog::at(self.store.audit_dir()),
                one_time: Vec::new(),
                restart_requested: false,
                burn_requested: false,
                base: self.opts.base.clone(),
                layer: layer.path().to_path_buf(),
                layer_kind: layer.kind().as_str().to_string(),
                landlock: "installed by the guest at startup".into(),
            },
        )?;

        let plan = self.plan(broker_socket, seccomp_blob)?;
        plan.prepare()?;

        if self.opts.verbose {
            let argv = crate::guest::build_argv(&plan, None, None);
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
        let mut plan = plan;
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
        let burn = broker.burn_requested();
        let one_time = broker.one_time_packages();
        drop(broker);

        // The overlay is unmounted now, so the layer is an ordinary directory
        // again and both of these are safe.
        if !one_time.is_empty() && !burn {
            self.undo_one_time(&one_time);
        }
        if burn {
            layer.burn()?;
        }
        crate::guest::remove_dir_all_forced(&session_dir);
        if self.opts.ephemeral && !restart {
            layer.burn()?;
        }
        Ok((code, restart))
    }

    /// Remove the packages a `--one-time` install put in the layer, by starting
    /// a throwaway non-interactive guest against the same layer — removing an
    /// rpm means running rpm, which needs the base to run its scriptlets.
    fn undo_one_time(&self, packages: &[String]) {
        let releasever = crate::pkg::releasever();
        let argv = crate::pkg::remove_argv(packages, &releasever);
        let mut opts = self.opts.clone();
        opts.command = argv;
        opts.ephemeral = false;
        opts.verbose = false;
        let cleanup = Session {
            store: self.store.clone(),
            opts,
            session_id: format!("{}-cleanup", self.session_id),
        };
        // A failure here leaves the package in the layer (visible in `pkg list`,
        // removable by hand) — not a reason to fail a session already finished.
        if let Err(e) = cleanup.run_once() {
            tracing::warn!(error = %e, "could not undo the session's one-time installs");
        }
        let manifest = self
            .layer()
            .path()
            .join(crate::pkg::MANIFEST_PATH.trim_start_matches('/'));
        let mut m = crate::pkg::Manifest::load(&manifest);
        for p in packages {
            m.forget(p);
        }
        let _ = m.save(&manifest);
    }
}

/// Give a fresh guest home the dotfiles that make a shell usable.
/// Minimal home contents for the guest. The home is write-isolated by default,
/// so it is not seeded from the user's home directory.
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
         PS1='[\\[\\e[36m\\]${TILO_USH_PROFILE:-ush}\\[\\e[0m\\] \\W]\\$ '\n\
         alias ls='ls --color=auto'\n",
    )
}

// serve — handing a guest to an app

/// What an app asks for over the service socket.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ServeRequest {
    /// Open a guest and hand back its pty master.
    OpenSession {
        #[serde(default)]
        profile: String,
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
        profile: String,
    },
    Denied {
        reason: String,
    },
}

/// Decide one request. Pure, so the gate on `dsh` can be tested without a pty.
///
/// # Errors
///
/// Returns an error if the requested profile is not reachable from an app
/// launch (currently only `dsh`, the unsandboxed developer profile).
pub fn decide_serve(req: &ServeRequest) -> Result<Profile, String> {
    match req {
        ServeRequest::OpenSession { profile, .. } => {
            let requested = Profile::parse(profile);
            if !requested.reachable_from_app_launch() {
                return Err(format!(
                    "`{}` is the developer profile and is not a sandbox; it is \
                     reachable only by running it yourself, never from an app \
                     launch",
                    requested.as_str()
                ));
            }
            Ok(requested)
        }
    }
}

/// Listen for apps and hand out guests.
///
/// # Errors
///
/// Returns an error if the store cannot be prepared, the socket path's
/// parent cannot be created, the socket cannot be bound, or its permissions
/// cannot be set to owner-only.
pub fn serve(socket: &Path, store: &Store, base: &Path) -> anyhow::Result<()> {
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
        let base = base.to_path_buf();
        std::thread::spawn(move || {
            if let Err(e) = serve_one(&stream, &store, base) {
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

fn serve_one(stream: &UnixStream, store: &Store, base: PathBuf) -> anyhow::Result<()> {
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
    let profile = match decide_serve(&req) {
        Ok(p) => p,
        Err(reason) => {
            AuditLog::at(store.audit_dir()).append(
                &crate::audit::AuditRecord::new("serve", "ush", "open-session", false)
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

    let mut opts = SessionOptions {
        profile,
        base,
        term: term.unwrap_or_else(|| "xterm-256color".into()),
        // This socket is the terminal's shell. Commands the user types,
        // including `echo ok > ~/probe`, have to land in their home. The
        // `ush` CLI does not set this, so package sessions stay isolated.
        user_home: true,
        ..SessionOptions::default()
    };
    opts.command = command;

    let session = Session::new(store.clone(), opts);
    let id = session.session_id.clone();
    AuditLog::at(store.audit_dir()).append(&crate::audit::AuditRecord::new(
        &id,
        profile.as_str(),
        "open-session",
        true,
    ));

    // The guest gets the slave as its stdio; the app gets the master. A *child
    // process*, not a thread: attaching a pty means `dup2` onto fds 0/1/2 and
    // `setsid`, both process-wide — on a thread that would steal the
    // supervisor's own stdio and controlling terminal (and under a test harness, the harness's).
    let child = spawn_attached(&session, &slave)?;
    // Matches the timing spawn_attached had when it owned `slave`: the
    // child already holds its own clones for stdio, so the original can
    // close as soon as the spawn is done.
    drop(slave);
    reply(
        &mut writer,
        &ServeResponse::Session {
            session: id.clone(),
            profile: profile.as_str().to_string(),
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
        &crate::audit::AuditRecord::new(&id, "ush", "close-session", true).because(termination),
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

/// Start `tilo-ush run` with `slave` as its stdio, in its own session, with the
/// pty as its controlling terminal.
fn spawn_attached(
    session: &Session,
    slave: &std::fs::File,
) -> std::io::Result<std::process::Child> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let exe = runtime_binary()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run");
    if session.opts.user_home {
        cmd.arg("--user-home");
    }
    cmd.env(crate::store::STORE_ENV, session.store.root());
    cmd.env("TERM", &session.opts.term);
    cmd.env("TILO_USH_BASE", &session.opts.base);
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

    use crate::layer::LayerKind;

    /// The gate on the deliberate exception, at the place an app would hit it.
    #[test]
    fn serve_refuses_dsh_and_defaults_every_other_profile_to_ush() {
        let open = |profile: &str| ServeRequest::OpenSession {
            profile: profile.into(),
            cols: 80,
            rows: 24,
            term: None,
            command: Vec::new(),
        };
        let err = decide_serve(&open("dsh")).unwrap_err();
        assert!(err.contains("not a sandbox"), "{err}");
        for name in ["ush", "", "nonsense"] {
            assert_eq!(decide_serve(&open(name)).unwrap(), Profile::Ush, "{name:?}");
        }
    }

    #[test]
    fn a_session_request_may_name_a_program_and_defaults_to_the_shell() {
        let shell: ServeRequest =
            serde_json::from_str(r#"{"op":"open-session","profile":"ush"}"#).unwrap();
        assert!(
            matches!(shell, ServeRequest::OpenSession { ref command, .. } if command.is_empty())
        );
        let program: ServeRequest = serde_json::from_str(
            r#"{"op":"open-session","profile":"ush","command":["htop","-d","5"]}"#,
        )
        .unwrap();
        assert!(matches!(
            program,
            ServeRequest::OpenSession { ref command, .. } if command == &["htop", "-d", "5"]
        ));
        assert_eq!(decide_serve(&program).unwrap(), Profile::Ush);
    }

    #[test]
    fn session_options_choose_layers_and_restart_behaviour() {
        // phase: an_ephemeral_session_stacks_the_persistent_layer_and_a_normal_one_does_not
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::at(dir.path().join("ush"));
            store.ensure().unwrap();
            // A persistent layer has to exist to be stacked.
            std::fs::create_dir_all(store.persistent_dir()).unwrap();
            std::fs::write(store.persistent_dir().join(".ush-layer"), b"persistent\n").unwrap();

            let normal = Session::new(store.clone(), SessionOptions::default());
            assert!(normal.lower_layers().is_empty());
            assert_eq!(normal.layer().kind(), LayerKind::Persistent);

            let ephemeral = Session::new(
                store.clone(),
                SessionOptions {
                    ephemeral: true,
                    ..SessionOptions::default()
                },
            );
            assert_eq!(
                ephemeral.lower_layers(),
                vec![store.persistent_dir()],
                "an ephemeral session must still see what you installed for good"
            );
            assert_eq!(ephemeral.layer().kind(), LayerKind::Ephemeral);
        }
        // phase: a_one_shot_command_never_restarts_in_place
        // `pkg burn` asks for a restart so the guest comes back on a fresh layer;
        // under `ush -c` that would re-run the command that asked, looping forever.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::at(dir.path().join("ush"));

            let interactive = Session::new(store.clone(), SessionOptions::default());
            assert!(interactive.restarts_in_place());

            let one_shot = Session::new(
                store,
                SessionOptions {
                    command: vec!["/bin/sh".into(), "-c".into(), "pkg burn".into()],
                    ..SessionOptions::default()
                },
            );
            assert!(
                !one_shot.restarts_in_place(),
                "restarting a session that was given a command re-runs the command"
            );
        }
    }

    #[test]
    fn the_plan_grants_exactly_what_the_policy_file_names() {
        // phase: the_plan_takes_its_grants_from_the_policy_file
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::at(dir.path().join("ush"));
            store.ensure().unwrap();
            let trusted = dir.path().join("src");
            std::fs::create_dir_all(&trusted).unwrap();

            let mut policy = Policy::default();
            policy.trust_dir(&trusted).unwrap();
            policy.save(&store.policy_path()).unwrap();

            let session = Session::new(store, SessionOptions::default());
            let plan = session
                .plan(PathBuf::from("/tmp/b.sock"), PathBuf::from("/tmp/f.bpf"))
                .unwrap();
            assert_eq!(plan.trusted_dirs, vec![trusted]);
        }
        // phase: a_session_with_no_policy_grants_nothing
        // A guest with an empty policy sees no host directory. The plan is where
        // that is decided, so it is where it is asserted.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::at(dir.path().join("ush"));
            store.ensure().unwrap();
            let session = Session::new(store, SessionOptions::default());
            let plan = session
                .plan(PathBuf::from("/tmp/b.sock"), PathBuf::from("/tmp/f.bpf"))
                .unwrap();
            assert!(plan.trusted_dirs.is_empty());
            assert!(plan.readable_dirs.is_empty());
        }
    }

    #[test]
    fn the_guest_home_and_shell_are_prepared_without_the_real_ones() {
        // phase: the_guest_home_is_seeded_once_and_not_from_the_real_one
        {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            seed_guest_home(&home).unwrap();
            let rc = home.join(".bashrc");
            assert!(rc.is_file());
            std::fs::write(&rc, b"# edited by the user\n").unwrap();
            seed_guest_home(&home).unwrap();
            assert_eq!(
                std::fs::read_to_string(&rc).unwrap(),
                "# edited by the user\n",
                "re-seeding must not overwrite what the user put in their guest home"
            );
        }
        // phase: the_guest_shell_falls_back_rather_than_failing_the_session
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
            std::fs::write(dir.path().join("usr/bin/bash"), b"#!elf").unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("SHELL", "/usr/bin/definitely-not-here");
            assert_eq!(guest_shell(dir.path()), PathBuf::from("/usr/bin/bash"));
        }
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

    #[test]
    fn a_terminal_session_uses_the_caller_home_and_covers_the_store() {
        let home = Path::new("/var/home/you");
        let store = Path::new("/var/home/you/.local/share/ush");
        let (guest, mask) = session_directories(true, home, store.join("home"), store);
        assert_eq!(guest, home);
        assert_eq!(mask.as_deref(), Some(store));

        let (guest, mask) = session_directories(false, home, PathBuf::from("/store/home"), store);
        assert_eq!(guest, PathBuf::from("/store/home"));
        assert!(mask.is_none());

        // A store kept outside the home is already unreachable; covering a
        // path the guest cannot see would only hide a directory they own.
        let outside = Path::new("/var/lib/ush");
        let (guest, mask) = session_directories(true, home, outside.join("home"), outside);
        assert_eq!(guest, home);
        assert!(mask.is_none());
    }
}

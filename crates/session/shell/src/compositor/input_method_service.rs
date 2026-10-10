//! Session-owned trusted input methods: the on-screen keyboard and the
//! composing input method.
//!
//! Settings stores intent; the compositor owns execution because it also owns
//! the private Wayland sockets carrying `input-method-v2`. Each trusted input
//! method gets its own private display, and each display admits only the
//! process group the compositor itself started — the on-screen keyboard through
//! the ordinary package sandbox, the composer as the image's own executable,
//! which the socket checks by path as well. [`super::input_method::arbitrate`]
//! decides which of them runs, from the active input source and — through
//! [`super::input_method::osk_service_wanted`] — the on-screen keyboard preference and
//! the input hardware in use. Stopping one terminates its whole process group,
//! so an old input method cannot linger after it lost the seat.

use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::LoopHandle;
use smithay::wayland::socket::ListeningSocketSource;

use settings_store::hardware::InputMethodEngine;

use super::input_environment::InputEnvironment;
use super::input_method::{InputMethodOwner, OskInput, OskPreference, TrustedInputMethod};
use super::state::{ClientState, State};
use crate::shell::state::RoomId;

const PACKAGE_ID: &str = super::state::OSK_APP_ID;
const PACKAGE_KIND: &str = "service";
const RETRY_BACKOFF: Duration = Duration::from_secs(5);
const INSTALL_REFRESH: Duration = Duration::from_secs(30);

/// The on-screen keyboard preference in `settings`.
pub fn osk_preference(settings: &settings_store::live::Snapshot) -> OskPreference {
    OskPreference::from_setting(
        settings
            .document
            .get("accessibility")
            .and_then(|section| section.get("on_screen_keyboard")),
    )
}

/// What one service should be running: the owner it serves and, for the
/// composer, the layout its engine types on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    owner: InputMethodOwner,
    layout: String,
}

/// The process to spawn for a request.
struct Launch {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// `Some`: start from an empty environment and inherit only these.
    inherit: Option<Vec<&'static str>>,
    sandboxed: bool,
}

pub struct InputMethodService {
    kind: TrustedInputMethod,
    socket: PathBuf,
    accepting: Arc<AtomicBool>,
    process_group: Arc<AtomicI32>,
    /// Whether the current launch's process is alive. A fresh flag per
    /// launch, so a stopping predecessor can never mark its successor dead.
    running: Arc<AtomicBool>,
    /// A revoked predecessor must finish bounded group termination before
    /// another launch can acquire this socket's authority.
    stopping: Option<Arc<AtomicBool>>,
    installed: Option<crate::shell::launcher::InstalledApp>,
    desired: Option<Request>,
    launched: Option<Request>,
    stop: Option<StopSignal>,
    last_attempt: Option<Instant>,
}

/// Stops the current launch when dropped. Its supervisor sleeps on this ring
/// and the child's exit instead of waking every 150 ms to look at both.
struct StopSignal(Arc<bounded_command::WakeFd>);

impl Drop for StopSignal {
    fn drop(&mut self) {
        self.0.ring();
    }
}

impl InputMethodService {
    pub fn new(kind: TrustedInputMethod, socket: PathBuf) -> Self {
        Self {
            kind,
            socket,
            accepting: Arc::new(AtomicBool::new(false)),
            process_group: Arc::new(AtomicI32::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            stopping: None,
            installed: None,
            desired: None,
            launched: None,
            stop: None,
            last_attempt: None,
        }
    }

    pub fn kind(&self) -> TrustedInputMethod {
        self.kind
    }

    /// The running service's process group, if it has one.
    pub fn process_group(&self) -> Option<libc::pid_t> {
        let group = self.process_group.load(Ordering::Acquire);
        (group > 0).then_some(group)
    }

    /// Shared with the socket callback. A service that does not own the seat
    /// rejects new connections even though the listening source stays
    /// registered.
    pub fn socket_authorization(&self) -> InputMethodSocketAuthorization {
        InputMethodSocketAuthorization {
            accepting: Arc::clone(&self.accepting),
            process_group: Arc::clone(&self.process_group),
            executable: (self.kind == TrustedInputMethod::Composer)
                .then(|| PathBuf::from(super::input_method::COMPOSER_EXECUTABLE)),
        }
    }

    /// Follow the seat's owner. This service runs only while the owner is of
    /// its kind; a changed engine or layout restarts the composer with the
    /// new configuration.
    pub fn set_owner(&mut self, owner: InputMethodOwner, primary_layout: &str) {
        let desired = (owner.kind() == Some(self.kind)).then(|| Request {
            owner,
            layout: primary_layout.to_string(),
        });
        if desired == self.desired {
            return;
        }
        self.desired = desired;
        self.stop_current();
        self.last_attempt = None;
        self.ensure_running();
    }

    /// Retry a crashed/missing service with a bounded backoff. Called from the
    /// compositor tick; it never waits for the child.
    pub fn tick(&mut self) {
        if self.desired.is_some() && !self.running.load(Ordering::Acquire) {
            self.ensure_running();
        }
    }

    /// Follow only the canonical installed keyboard row. Unrelated package
    /// changes cannot restart the service or alter its input authority.
    fn set_installed(&mut self, installed: Option<crate::shell::launcher::InstalledApp>) -> bool {
        if self.kind != TrustedInputMethod::OnScreenKeyboard || self.installed == installed {
            return false;
        }
        self.installed = installed;
        self.stop_current();
        self.last_attempt = None;
        true
    }

    fn stop_current(&mut self) {
        self.accepting.store(false, Ordering::Release);
        self.process_group.store(0, Ordering::Release);
        self.launched = None;
        if let Some(stop) = self.stop.take() {
            self.stopping = Some(Arc::clone(&self.running));
            drop(stop);
        }
        self.running = Arc::new(AtomicBool::new(false));
    }

    fn ensure_running(&mut self) {
        if self
            .stopping
            .as_ref()
            .is_some_and(|running| running.load(Ordering::Acquire))
        {
            return;
        }
        self.stopping = None;
        let Some(request) = self.desired.clone() else {
            return;
        };
        if self.running.load(Ordering::Acquire)
            || self
                .last_attempt
                .is_some_and(|at| at.elapsed() < RETRY_BACKOFF)
        {
            return;
        }
        self.last_attempt = Some(Instant::now());

        let launch = match self.launch_for(&request) {
            Ok(launch) => launch,
            Err(error) => {
                tracing::warn!(target: "tilo::input_method", kind = ?self.kind, %error, "input method unavailable");
                return;
            }
        };
        let wake = match bounded_command::WakeFd::new() {
            Ok(wake) => Arc::new(wake),
            Err(error) => {
                tracing::warn!(target: "tilo::input_method", kind = ?self.kind, %error, "input method stop signal unavailable");
                return;
            }
        };
        self.stop = Some(StopSignal(Arc::clone(&wake)));
        self.launched = Some(request);
        // Reserve this launch before its supervisor thread can run, so a
        // compositor tick cannot create a duplicate while spawn is pending.
        let running = Arc::new(AtomicBool::new(true));
        self.running = Arc::clone(&running);
        self.accepting.store(true, Ordering::Release);
        let process_group = Arc::clone(&self.process_group);
        let kind = self.kind;
        let supervisor = std::thread::Builder::new()
            .name("tilo-input-method".to_string())
            .spawn(move || supervise(kind, &launch, &wake, &running, &process_group));
        if let Err(error) = supervisor {
            self.running.store(false, Ordering::Release);
            self.accepting.store(false, Ordering::Release);
            self.stop = None;
            self.launched = None;
            tracing::warn!(target: "tilo::input_method", %error, "could not create input method supervisor thread");
        }
    }

    fn launch_for(&self, request: &Request) -> Result<Launch, String> {
        match request.owner {
            InputMethodOwner::Nobody => Err("no input method requested".into()),
            InputMethodOwner::OnScreenKeyboard => {
                let app = self
                    .installed
                    .as_ref()
                    .ok_or_else(|| format!("{PACKAGE_ID} is not installed"))?;
                let command = service_launch_command(app, &self.socket)?;
                Ok(Launch {
                    program: command.program,
                    args: command.args,
                    env: command.env,
                    inherit: None,
                    sandboxed: true,
                })
            }
            InputMethodOwner::Composer(engine) => {
                let runtime = crate::shell::launcher::runtime_dir();
                let config_home = runtime.join("tilo-session").join("input-method");
                let launch = super::input_method::composer_launch(
                    engine,
                    &request.layout,
                    &self.socket,
                    &config_home,
                    &super::xwayland_composer::composer_authority_path(&runtime),
                );
                for (path, body) in &launch.files {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
                    }
                    std::fs::write(path, body)
                        .map_err(|e| format!("writing {}: {e}", path.display()))?;
                }
                Ok(Launch {
                    program: launch.program,
                    args: launch.args,
                    env: launch.env,
                    inherit: Some(launch.inherit),
                    sandboxed: false,
                })
            }
        }
    }
}

fn supervise(
    kind: TrustedInputMethod,
    launch: &Launch,
    stop: &bounded_command::WakeFd,
    running: &AtomicBool,
    process_group: &AtomicI32,
) {
    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        // Every process of the service shares one group. The supervisor
        // can therefore revoke the service as one unit.
        .process_group(0);
    if let Some(inherit) = &launch.inherit {
        cmd.env_clear();
        for name in inherit {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
    }
    if launch.sandboxed {
        crate::shell::launcher::apply_launch_env(&mut cmd, &launch.env);
    } else {
        cmd.envs(launch.env.iter().map(|(k, v)| (k, v)));
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(target: "tilo::input_method", ?kind, %error, "could not start input method");
            running.store(false, Ordering::Release);
            return;
        }
    };
    running.store(true, Ordering::Release);
    let pgid = child.id() as libc::pid_t;
    // A stop that raced the spawn already cleared the group; never re-arm
    // the socket for a process that is about to be terminated.
    let armed = process_group
        .compare_exchange(0, pgid, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    // Sleep until the child exits or the stop signal rings (or is dropped).
    let woke = if armed {
        bounded_command::wait_child_or(&mut child, Some(stop.fd()), None)
    } else {
        Ok(bounded_command::Wakeup::Woken)
    };
    match woke {
        Ok(bounded_command::Wakeup::Exited(status)) => {
            if !status.success() {
                tracing::warn!(target: "tilo::input_method", ?kind, ?status, "input method exited");
            }
        }
        // Only the stop signal rings this descriptor, and there is no deadline.
        Ok(bounded_command::Wakeup::Woken | bounded_command::Wakeup::Deadline) => {
            bounded_command::terminate_tree(&mut child, Duration::from_secs(1));
        }
        Err(error) => {
            tracing::warn!(target: "tilo::input_method", ?kind, %error, "could not inspect input method");
        }
    }
    // Even if the launcher exited first, no descendant may retain the old
    // package/runtime or the private display. Reap before publishing completion.
    bounded_command::kill_tree(&mut child);
    let _ = child.wait();
    let _ = process_group.compare_exchange(pgid, 0, Ordering::AcqRel, Ordering::Acquire);
    running.store(false, Ordering::Release);
}

impl Drop for InputMethodService {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        self.stop_current();
    }
}

/// Read-only credential shared with the listening-socket callback. A random,
/// unexported path limits discovery; `SO_PEERCRED` plus the compositor-created
/// process group (and, for the composer, the executable) is the actual
/// authorization check.
#[derive(Clone)]
pub struct InputMethodSocketAuthorization {
    accepting: Arc<AtomicBool>,
    process_group: Arc<AtomicI32>,
    executable: Option<PathBuf>,
}

impl InputMethodSocketAuthorization {
    pub fn allows(&self, stream: &UnixStream) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let expected = self.process_group.load(Ordering::Acquire);
        if expected <= 0 {
            return false;
        }
        let Some(credentials) = peer_credentials(stream) else {
            return false;
        };
        // SAFETY: `credentials.pid` came from the kernel's SO_PEERCRED
        // response and geteuid has no failure mode.
        let peer_group = unsafe { libc::getpgid(credentials.pid) };
        let expected_uid = unsafe { libc::geteuid() };
        credential_decision(
            expected,
            expected_uid,
            credentials.uid,
            peer_group,
            is_descendant_of(credentials.pid, expected),
        ) && self
            .executable
            .as_deref()
            .is_none_or(|executable| runs_executable(credentials.pid, executable))
    }
}

/// Whether `pid` is running `executable`, compared through the kernel's own
/// record of the mapped binary rather than anything the process reports.
fn runs_executable(pid: libc::pid_t, executable: &Path) -> bool {
    let Ok(running) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let expected = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    running == expected
}

fn peer_credentials(stream: &UnixStream) -> Option<libc::ucred> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    // `size_of::<ucred>()` is a small compile-time-fixed struct size; it can never actually
    // exceed `u32::MAX`, but a checked conversion still beats trusting that by construction.
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: both pointers refer to initialized storage with the advertised
    // length; `stream` remains alive throughout the call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            std::ptr::from_mut(&mut length),
        )
    };
    (result == 0 && length as usize >= std::mem::size_of::<libc::ucred>()).then_some(credentials)
}

fn credential_decision(
    expected_root_and_group: libc::pid_t,
    expected_uid: libc::uid_t,
    peer_uid: libc::uid_t,
    peer_group: libc::pid_t,
    peer_is_descendant: bool,
) -> bool {
    expected_root_and_group > 0
        && peer_uid == expected_uid
        && peer_group == expected_root_and_group
        && peer_is_descendant
}

fn is_descendant_of(mut pid: libc::pid_t, ancestor: libc::pid_t) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        if pid <= 1 {
            return false;
        }
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            return false;
        };
        let Some(parent) = status.lines().find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
        }) else {
            return false;
        };
        if parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

/// The installed on-screen keyboard package, if it is a graphical service in
/// its signed manifest.
fn installed_osk() -> Result<crate::shell::launcher::InstalledApp, String> {
    let app = crate::shell::launcher::installed_apps()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|app| app.package_id == PACKAGE_ID)
        .ok_or_else(|| format!("{PACKAGE_ID} is not installed"))?;
    let manifest = launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|e| format!("reading {}: {e}", app.manifest_path.display()))?;
    if manifest.kind != PACKAGE_KIND || manifest.entrypoint.gui_bin().is_none() {
        return Err(format!(
            "{PACKAGE_ID} is not a graphical service in its signed manifest"
        ));
    }
    Ok(app)
}

fn service_launch_command(
    app: &crate::shell::launcher::InstalledApp,
    socket: &Path,
) -> Result<crate::shell::launcher::LaunchCommand, String> {
    let launcher = crate::shell::launcher::resolve_launcher_bin().map_err(|e| e.to_string())?;
    Ok(crate::shell::launcher::build_launch_command(
        app,
        &launcher,
        socket,
        RoomId::nil(),
        None,
        None,
        crate::compositor::render_node::resolved(),
        None,
    ))
}

/// Both trusted input methods of a session, their private displays, and the
/// arbitration between them.
pub struct InputMethodServices {
    services: Vec<InputMethodService>,
    sockets: Vec<PathBuf>,
    /// The saved settings the owner follows, re-read by [`Self::reload`].
    primary_layout: String,
    engine: Option<InputMethodEngine>,
    osk_preference: OskPreference,
    /// Whether the on-screen keyboard package can run at all. Without it an
    /// automatic keyboard must never take the seat, or nothing would answer
    /// the focused field.
    osk_installed: bool,
    /// The input hardware facts, followed by [`Self::follow_input`].
    osk_input: OskInput,
    installed_epoch: u64,
    installed_checked: Instant,
}

impl InputMethodServices {
    /// Bind one private display per trusted input method and register its
    /// acceptor. A display that cannot be bound leaves that input method
    /// unavailable; the rest of the session is unaffected.
    pub fn start(handle: &LoopHandle<'static, State>) -> Self {
        let mut services = Vec::new();
        let mut sockets = Vec::new();
        for (kind, prefix) in [
            (TrustedInputMethod::OnScreenKeyboard, "tilo-osk"),
            (TrustedInputMethod::Composer, "tilo-im"),
        ] {
            let (socket, path) = match bind_private_display(prefix) {
                Ok(bound) => bound,
                Err(error) => {
                    tracing::warn!(target: "tilo::input_method", ?kind, %error, "private input-method display unavailable");
                    continue;
                }
            };
            sockets.push(path.clone());
            let service = InputMethodService::new(kind, path.clone());
            let authorization = service.socket_authorization();
            let accepted = handle.insert_source(socket, move |client_stream, (), state| {
                if !authorization.allows(&client_stream) {
                    tracing::warn!(target: "tilo::input_method", ?kind, "rejected connection to a private input-method display");
                    return;
                }
                match state
                    .display_handle
                    .insert_client(client_stream, ClientState::input_method_arc(kind))
                {
                    Ok(client) => state.register_trusted_input_method_client(&client, kind),
                    Err(error) => {
                        tracing::warn!(target: "tilo::input_method", ?kind, %error, "could not accept input-method connection");
                    }
                }
            });
            if let Err(error) = accepted {
                tracing::warn!(target: "tilo::input_method", ?kind, %error, "private input-method socket source rejected");
                continue;
            }
            tracing::info!(target: "tilo::input_method", ?kind, socket = %path.display(), "private input-method display ready");
            services.push(service);
        }
        Self {
            services,
            sockets,
            primary_layout: "us".to_string(),
            engine: None,
            osk_preference: OskPreference::default(),
            osk_installed: false,
            osk_input: OskInput::default(),
            installed_epoch: crate::shell::launcher::installed_apps_epoch(),
            installed_checked: Instant::now(),
        }
    }

    /// Re-read the preferences that decide the owner and apply the decision.
    pub fn reload(&mut self, state: &mut State, settings: &settings_store::live::Snapshot) {
        self.reload_for_scope(
            state,
            settings,
            &settings_store::keyboard_sources::active_scope(),
        );
    }

    pub fn reload_for_scope(
        &mut self,
        state: &mut State,
        settings: &settings_store::live::Snapshot,
        scope: &str,
    ) {
        let input =
            settings_store::hardware::RichInputConfig::from_section(&settings.section("input"));
        let input = settings_store::keyboard_sources::effective(&input, scope);
        self.primary_layout = input
            .sources()
            .first()
            .map_or_else(|| "us".to_string(), |source| source.id.clone());
        self.engine = input.active_input_method();
        self.osk_preference = osk_preference(settings);
        self.refresh_installed(state);
        self.apply(state);
    }

    /// Follow the input hardware in use. Only a change in whether the
    /// on-screen keyboard should run re-arbitrates the seat; typing and
    /// touching merely hide and show it (see [`Self::osk_shown`]).
    pub fn follow_input(&mut self, state: &mut State, input: &InputEnvironment) {
        let facts = OskInput::from_environment(input);
        if facts == self.osk_input {
            return;
        }
        let before = self.osk_service_wanted();
        self.osk_input = facts;
        if self.osk_service_wanted() != before {
            self.apply(state);
        }
    }

    fn osk_service_wanted(&self) -> bool {
        self.osk_installed
            && super::input_method::osk_service_wanted(self.osk_preference, self.osk_input)
    }

    /// Whether a running on-screen keyboard may be shown for the focused
    /// field: the preference and the input used most recently allow it.
    pub fn osk_shown(&self) -> bool {
        self.osk_service_wanted()
            && super::input_method::osk_wanted(self.osk_preference, self.osk_input)
    }

    /// Apply the ownership decision: the seat's global gate first
    /// (disconnecting the previous owner), then the services, so the new
    /// owner is never refused by a stale gate.
    fn apply(&mut self, state: &mut State) {
        let owner = super::input_method::arbitrate(
            self.engine,
            self.engine
                .is_some_and(super::input_method::engine_installed)
                && self.has(TrustedInputMethod::Composer),
            self.osk_service_wanted(),
        );
        if state.input_method_owner_kind() != owner.kind() {
            tracing::info!(target: "tilo::input_method", ?owner, "input method owner changed");
        }
        state.set_input_method_owner(owner.kind());
        for service in &mut self.services {
            service.set_owner(owner, &self.primary_layout);
        }
    }

    fn refresh_installed(&mut self, state: &mut State) {
        let read_epoch = crate::shell::launcher::installed_apps_epoch();
        let installed = match installed_osk() {
            Ok(app) => Some(app),
            Err(error) => {
                tracing::debug!(target: "tilo::input_method", %error, "on-screen keyboard unavailable");
                None
            }
        };
        self.osk_installed = installed.is_some() && self.has(TrustedInputMethod::OnScreenKeyboard);
        for service in &mut self.services {
            if service.set_installed(installed.clone())
                && state.input_method_owner_kind() == Some(TrustedInputMethod::OnScreenKeyboard)
            {
                // Revoke established clients as well as the accept gate. The
                // next client will activate the existing focused text input;
                // neither focus nor the visibility preference is changed.
                state.set_input_method_owner(None);
            }
        }
        // If a registry change raced this read, its epoch remains pending for
        // the next tick rather than being hidden by the fallback interval.
        self.installed_epoch = read_epoch;
        self.installed_checked = Instant::now();
    }

    pub fn tick(&mut self, state: &mut State) {
        if self.installed_epoch != crate::shell::launcher::installed_apps_epoch()
            || self.installed_checked.elapsed() >= INSTALL_REFRESH
        {
            self.refresh_installed(state);
            self.apply(state);
        }
        for service in &mut self.services {
            service.tick();
        }
        // X11 displays connect the composer and discount its connection by
        // this group.
        super::xwayland_composer::set_composer_process_group(
            self.services
                .iter()
                .find(|service| service.kind() == TrustedInputMethod::Composer)
                .and_then(InputMethodService::process_group),
        );
    }

    /// The private display paths, for unlinking at teardown.
    pub fn sockets(&self) -> &[PathBuf] {
        &self.sockets
    }

    fn has(&self, kind: TrustedInputMethod) -> bool {
        self.services.iter().any(|service| service.kind() == kind)
    }
}

/// Bind a private display used only by a compositor-launched input method.
/// The random suffix keeps it out of well-known socket-name guesses; unlike
/// the privileged socket it is never exported in the session environment.
fn bind_private_display(prefix: &str) -> Result<(ListeningSocketSource, PathBuf), String> {
    let name = format!(
        "{prefix}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let socket = ListeningSocketSource::with_name(&name).map_err(|e| e.to_string())?;
    let path = crate::shell::launcher::runtime_dir().join(socket.socket_name());
    Ok((socket, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    struct InstalledServiceFixture {
        methods: InputMethodServices,
        state: State,
        directory: tempfile::TempDir,
        display: smithay::reexports::wayland_server::Display<State>,
        _event_loop: smithay::reexports::calloop::EventLoop<'static, State>,
        _env: tilo_test_env::ScopedEnv,
    }

    impl InstalledServiceFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", directory.path());
            env.set("XDG_CONFIG_HOME", directory.path());
            env.set("TILO_APPS_DATA_DIR", directory.path());
            let config = directory.path().join("tilo");
            std::fs::create_dir_all(&config).unwrap();
            std::fs::write(
                config.join("settings.json"),
                r#"{"accessibility":{"on_screen_keyboard":"always"}}"#,
            )
            .unwrap();
            let payload = directory.path().join("payload");
            std::fs::create_dir_all(&payload).unwrap();
            std::fs::write(
                directory.path().join("registry.toml"),
                format!(
                    "[[entry]]\nid = '{PACKAGE_ID}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                    payload.display().to_string(),
                    payload.join("manifest.toml").display().to_string()
                ),
            )
            .unwrap();
            let launcher = directory.path().join("launcher");
            std::fs::write(
                &launcher,
                "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 30; done\n",
            )
            .unwrap();
            bounded_command::make_executable(&launcher).unwrap();
            env.set("TILO_LAUNCHER_BIN", &launcher);
            let event_loop = smithay::reexports::calloop::EventLoop::try_new().unwrap();
            let display = smithay::reexports::wayland_server::Display::new().unwrap();
            let state = State::new(event_loop.handle(), display.handle());
            let methods = InputMethodServices::start(&event_loop.handle());
            let fixture = Self {
                methods,
                state,
                directory,
                display,
                _event_loop: event_loop,
                _env: env,
            };
            fixture.install("1.0.0");
            fixture
        }

        fn install(&self, version: &str) {
            std::fs::write(
                self.directory.path().join("payload/manifest.toml"),
                crate::shell::test_support::canonical_manifest(
                    &format!("id = '{PACKAGE_ID}'\nversion = '{version}'\nkind = 'service'"),
                    "[entrypoint]\nbin = 'bin/app'\ntype = 'gui'\n",
                ),
            )
            .unwrap();
            crate::shell::launcher::invalidate_app_cache();
        }

        fn osk(&self) -> &InputMethodService {
            self.methods
                .services
                .iter()
                .find(|service| service.kind() == TrustedInputMethod::OnScreenKeyboard)
                .unwrap()
        }
    }

    impl Drop for InstalledServiceFixture {
        fn drop(&mut self) {
            let running: Vec<_> = self
                .methods
                .services
                .iter()
                .flat_map(|service| {
                    std::iter::once(Arc::clone(&service.running)).chain(service.stopping.clone())
                })
                .collect();
            self.methods.services.clear();
            wait_for(|| running.iter().all(|flag| !flag.load(Ordering::Acquire)));
            crate::shell::launcher::invalidate_app_cache();
        }
    }

    /// The fixture's settings file as the compositor's live snapshot would
    /// hold it.
    fn saved_settings() -> settings_store::live::Snapshot {
        settings_store::live::Snapshot {
            generation: 0,
            document: settings_store::load(),
        }
    }

    fn wait_for(mut ready: impl FnMut() -> bool) {
        let started = Instant::now();
        while !ready() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "bounded service transition did not finish"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn installed_keyboard_update_revokes_old_authority_before_replacement() {
        let mut fixture = InstalledServiceFixture::new();
        fixture
            .methods
            .reload(&mut fixture.state, &saved_settings());
        wait_for(|| fixture.osk().process_group().is_some());
        let old_group = fixture.osk().process_group().unwrap();
        let old_running = Arc::clone(&fixture.osk().running);
        let (server, mut client) = UnixStream::pair().unwrap();
        let old_client = fixture
            .state
            .display_handle
            .insert_client(
                server,
                ClientState::input_method_arc(TrustedInputMethod::OnScreenKeyboard),
            )
            .unwrap();
        fixture.state.register_trusted_input_method_client(
            &old_client,
            TrustedInputMethod::OnScreenKeyboard,
        );
        fixture.install("2.0.0");
        fixture.methods.tick(&mut fixture.state);
        let revoked = !fixture.osk().accepting.load(Ordering::Acquire)
            && fixture.osk().process_group().is_none();
        // Like the compositor loop, dispatch the standard Wayland display
        // after revocation so the backend reaps its queued client kill.
        // wl_display.sync: display object 1, opcode 0, twelve-byte message,
        // with new callback object 2. No application content is transmitted.
        client
            .write_all(
                &[
                    1_u32.to_ne_bytes(),
                    (12_u32 << 16).to_ne_bytes(),
                    2_u32.to_ne_bytes(),
                ]
                .concat(),
            )
            .unwrap();
        fixture
            .display
            .dispatch_clients(&mut fixture.state)
            .unwrap();
        let disconnected = fixture
            .state
            .display_handle
            .backend_handle()
            .get_client_data(old_client.id())
            .is_err();
        // Verify the normal tick, not a Settings toggle or another service door,
        // eventually starts the new installed generation after bounded cleanup.
        let started = Instant::now();
        while fixture.osk().process_group().is_none() && started.elapsed() < Duration::from_secs(5)
        {
            fixture.methods.tick(&mut fixture.state);
            std::thread::sleep(Duration::from_millis(10));
        }
        let new_group = fixture.osk().process_group();
        let old_finished = !old_running.load(Ordering::Acquire);
        let owner = fixture.state.input_method_owner_kind();
        let wanted = fixture.methods.osk_shown();
        // Capture assertions after owned cleanup so an old-baseline failure
        // cannot leak a supervisor or a TERM-resistant fixture process.
        drop(fixture);
        assert!(
            revoked,
            "updated package must revoke the old socket before relaunch"
        );
        assert!(
            disconnected,
            "already-connected old service cannot retain the seat"
        );
        assert!(
            old_finished,
            "replacement waits for old process-group termination"
        );
        assert!(new_group.is_some_and(|group| group != old_group));
        assert_eq!(owner, Some(TrustedInputMethod::OnScreenKeyboard));
        assert!(
            wanted,
            "package update preserves the saved visibility preference"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn unrelated_package_update_preserves_the_current_keyboard_launch() {
        let mut fixture = InstalledServiceFixture::new();
        fixture
            .methods
            .reload(&mut fixture.state, &saved_settings());
        wait_for(|| fixture.osk().process_group().is_some());
        let group = fixture.osk().process_group();
        let running = Arc::clone(&fixture.osk().running);
        let other = fixture.directory.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("manifest.toml"),
            crate::shell::test_support::canonical_manifest(
                "id = 'dev.tilo.other'\nversion = '1.0.0'\nkind = 'app'",
                "[entrypoint]\nbin = 'bin/app'\ntype = 'gui'\n",
            ),
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.directory.path().join("registry.toml"))
            .unwrap()
            .write_all(
                format!(
                    "[[entry]]\nid = 'dev.tilo.other'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                    other.display().to_string(),
                    other.join("manifest.toml").display().to_string()
                )
                .as_bytes(),
            )
            .unwrap();
        crate::shell::launcher::invalidate_app_cache();
        fixture.methods.tick(&mut fixture.state);
        let unchanged = fixture.osk().process_group() == group
            && Arc::ptr_eq(&fixture.osk().running, &running)
            && fixture.osk().accepting.load(Ordering::Acquire);
        drop(fixture);
        assert!(
            unchanged,
            "another installed application must not replace or revoke the keyboard"
        );
    }

    /// Context switches of this process's one thread named `name`.
    fn thread_context_switches(name: &str) -> u64 {
        let threads: Vec<_> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .map(|task| task.unwrap().path())
            .filter(|task| {
                std::fs::read_to_string(task.join("comm")).is_ok_and(|comm| comm.trim() == name)
            })
            .collect();
        assert_eq!(threads.len(), 1, "exactly one {name} thread must be live");
        std::fs::read_to_string(threads[0].join("status"))
            .unwrap()
            .lines()
            .filter(|line| line.contains("ctxt_switches:"))
            .map(|line| {
                line.split_whitespace()
                    .last()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            })
            .sum()
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn running_keyboard_supervisor_sleeps_until_stopped() {
        let mut fixture = InstalledServiceFixture::new();
        fixture
            .methods
            .reload(&mut fixture.state, &saved_settings());
        wait_for(|| fixture.osk().process_group().is_some());
        let running = Arc::clone(&fixture.osk().running);
        // Let the supervisor reach its wait; the kernel truncates names to 15 bytes.
        std::thread::sleep(Duration::from_millis(100));
        let before = thread_context_switches("tilo-input-meth");
        std::thread::sleep(Duration::from_millis(400));
        let idle = thread_context_switches("tilo-input-meth").saturating_sub(before);
        let alive = running.load(Ordering::Acquire);
        // Dropping stops the TERM-ignoring fixture through the stop signal
        // alone, and waits (bounded) for the supervisor to finish.
        drop(fixture);
        assert!(alive, "the fixture keyboard must still be running");
        // The 150 ms stop poll this replaces switched about three times here.
        assert_eq!(idle, 0, "an idle input method supervisor must not wake");
        assert!(!running.load(Ordering::Acquire));
    }

    #[test]
    fn losing_the_seat_closes_the_accept_gate() {
        let mut service = InputMethodService::new(
            TrustedInputMethod::OnScreenKeyboard,
            PathBuf::from("/not/opened"),
        );
        service.desired = Some(Request {
            owner: InputMethodOwner::OnScreenKeyboard,
            layout: "us".into(),
        });
        service.accepting.store(true, Ordering::Release);
        service.process_group.store(42, Ordering::Release);
        service.set_owner(
            InputMethodOwner::Composer(settings_store::hardware::InputMethodEngine::Rime),
            "us",
        );
        assert!(!service.accepting.load(Ordering::Acquire));
        assert_eq!(service.process_group.load(Ordering::Acquire), 0);
        assert!(service.desired.is_none());
    }

    #[test]
    fn a_service_only_wants_owners_of_its_kind() {
        let mut composer =
            InputMethodService::new(TrustedInputMethod::Composer, PathBuf::from("/not/opened"));
        composer.set_owner(InputMethodOwner::OnScreenKeyboard, "us");
        assert!(composer.desired.is_none());
        assert!(!composer.accepting.load(Ordering::Acquire));
    }

    #[test]
    fn socket_authorization_rejects_closed_gate_and_wrong_group() {
        let service = InputMethodService::new(
            TrustedInputMethod::OnScreenKeyboard,
            PathBuf::from("/not/opened"),
        );
        let authorization = service.socket_authorization();
        let (client, _server) = UnixStream::pair().expect("socket pair");

        authorization.accepting.store(true, Ordering::Release);
        assert!(!authorization.allows(&client), "no launched group yet");

        authorization
            .process_group
            .store(i32::MAX, Ordering::Release);
        assert!(!authorization.allows(&client), "wrong process group");

        authorization.accepting.store(false, Ordering::Release);
        assert!(!authorization.allows(&client));
    }

    #[test]
    fn the_composer_display_admits_only_the_composer_executable() {
        let service =
            InputMethodService::new(TrustedInputMethod::Composer, PathBuf::from("/not/opened"));
        let authorization = service.socket_authorization();
        let (client, _server) = UnixStream::pair().expect("socket pair");
        // Everything else about this peer (uid, and its own process group)
        // is as trusted as it gets; it is still not the composer binary.
        // SAFETY: getpgrp has no failure mode.
        let own_group = unsafe { libc::getpgrp() };
        authorization.accepting.store(true, Ordering::Release);
        authorization
            .process_group
            .store(own_group, Ordering::Release);
        assert!(!authorization.allows(&client));

        // SAFETY: getpid has no failure mode.
        let pid = unsafe { libc::getpid() };
        let own = std::env::current_exe().expect("test executable");
        assert!(runs_executable(pid, &own));
        assert!(!runs_executable(
            pid,
            Path::new(super::super::input_method::COMPOSER_EXECUTABLE)
        ));
    }

    #[test]
    fn wrong_group_or_same_uid_sibling_is_rejected() {
        // SAFETY: geteuid has no failure mode.
        let uid = unsafe { libc::geteuid() };
        assert!(credential_decision(4100, uid, uid, 4100, true));
        assert!(!credential_decision(4100, uid, uid, 4101, true));
        assert!(!credential_decision(4100, uid, uid, 4100, false));
        assert!(!credential_decision(
            4100,
            uid,
            uid.saturating_add(1),
            4100,
            true
        ));
    }

    #[test]
    fn child_process_group_is_dedicated() {
        // SAFETY: getpgrp has no failure mode.
        let parent_group = unsafe { libc::getpgrp() };
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "read _"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().expect("spawn process-group probe");
        let pid = child.id() as libc::pid_t;
        // SAFETY: `pid` names the live child above.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        assert_ne!(pid, parent_group);
        drop(child.stdin.take());
        let _ = child.wait();
    }
}

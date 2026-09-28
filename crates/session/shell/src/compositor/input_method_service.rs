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
//! [`super::input_method::osk_wanted`] — the on-screen keyboard preference and
//! the input hardware in use. Stopping one terminates its whole process group,
//! so an old input method cannot linger after it lost the seat.

use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{Sender, channel};
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

pub fn osk_preference() -> OskPreference {
    OskPreference::from_setting(
        settings_store::load_section("accessibility").get("on_screen_keyboard"),
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
    desired: Option<Request>,
    launched: Option<Request>,
    stop: Option<Sender<()>>,
    last_attempt: Option<Instant>,
}

impl InputMethodService {
    pub fn new(kind: TrustedInputMethod, socket: PathBuf) -> Self {
        Self {
            kind,
            socket,
            accepting: Arc::new(AtomicBool::new(false)),
            process_group: Arc::new(AtomicI32::new(0)),
            running: Arc::new(AtomicBool::new(false)),
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
        self.accepting
            .store(self.desired.is_some(), Ordering::Release);
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

    fn stop_current(&mut self) {
        self.process_group.store(0, Ordering::Release);
        self.launched = None;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.running = Arc::new(AtomicBool::new(false));
    }

    fn ensure_running(&mut self) {
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
        let (tx, rx) = channel();
        self.stop = Some(tx);
        self.launched = Some(request);
        let running = Arc::new(AtomicBool::new(false));
        self.running = Arc::clone(&running);
        let process_group = Arc::clone(&self.process_group);
        let kind = self.kind;
        let supervisor = std::thread::Builder::new()
            .name("tilo-input-method".to_string())
            .spawn(move || supervise(kind, &launch, &rx, &running, &process_group));
        if let Err(error) = supervisor {
            self.stop = None;
            self.launched = None;
            tracing::warn!(target: "tilo::input_method", %error, "could not create input method supervisor thread");
        }
    }

    fn launch_for(&self, request: &Request) -> Result<Launch, String> {
        match request.owner {
            InputMethodOwner::Nobody => Err("no input method requested".into()),
            InputMethodOwner::OnScreenKeyboard => {
                let command = service_launch_command(&self.socket)?;
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
    stop: &std::sync::mpsc::Receiver<()>,
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
    loop {
        if !armed || stop.recv_timeout(Duration::from_millis(150)).is_ok() {
            bounded_command::terminate_tree(&mut child, Duration::from_secs(1));
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    tracing::warn!(target: "tilo::input_method", ?kind, ?status, "input method exited");
                }
                break;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(target: "tilo::input_method", ?kind, %error, "could not inspect input method");
                break;
            }
        }
    }
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
    let app = crate::shell::launcher::read_installed_apps()
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

fn service_launch_command(socket: &Path) -> Result<crate::shell::launcher::LaunchCommand, String> {
    let app = installed_osk()?;
    let launcher = crate::shell::launcher::resolve_launcher_bin().map_err(|e| e.to_string())?;
    Ok(crate::shell::launcher::build_launch_command(
        &app,
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
        }
    }

    /// Re-read the preferences that decide the owner and apply the decision.
    pub fn reload(&mut self, state: &mut State) {
        let input = settings_store::hardware::RichInputConfig::from_section(
            &settings_store::load_section("input"),
        );
        self.primary_layout = input
            .sources()
            .first()
            .map_or_else(|| "us".to_string(), |source| source.id.clone());
        self.engine = input.active_input_method();
        self.osk_preference = osk_preference();
        self.osk_installed = self.has(TrustedInputMethod::OnScreenKeyboard)
            && match installed_osk() {
                Ok(_) => true,
                Err(error) => {
                    tracing::debug!(target: "tilo::input_method", %error, "on-screen keyboard unavailable");
                    false
                }
            };
        self.apply(state);
    }

    /// Follow the input hardware in use. Only a change in whether the
    /// on-screen keyboard is wanted re-arbitrates the seat.
    pub fn follow_input(&mut self, state: &mut State, input: &InputEnvironment) {
        let facts = OskInput::from_environment(input);
        if facts == self.osk_input {
            return;
        }
        let before = self.osk_wanted();
        self.osk_input = facts;
        if self.osk_wanted() != before {
            self.apply(state);
        }
    }

    fn osk_wanted(&self) -> bool {
        self.osk_installed && super::input_method::osk_wanted(self.osk_preference, self.osk_input)
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
            self.osk_wanted(),
        );
        if state.input_method_owner_kind() != owner.kind() {
            tracing::info!(target: "tilo::input_method", ?owner, "input method owner changed");
        }
        state.set_input_method_owner(owner.kind());
        for service in &mut self.services {
            service.set_owner(owner, &self.primary_layout);
        }
    }

    pub fn tick(&mut self) {
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

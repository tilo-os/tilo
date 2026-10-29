//! A session's shell on the Tilo side, on a Linux device.
//!
//! A new Terminal session runs ush's interpreter here, in the process that
//! serves the session's terminal, outside every guest, with
//! [`ush_tilo::TiloHost`] as its host. This module gives that host what it
//! needs from this device:
//!
//! - [`RecordEnvironment`]: the environment record's grants, and its file
//!   actions through the session's broker, the same door a guest's `cp`
//!   knocks on ([`crate::file_actions::brokered`]);
//! - [`DevicePlace`]: the clock, account names, and Ctrl-C;
//! - [`SessionGuest`]: the session's Linux guest, for the programs that are
//!   none of the shell's own (tier 3). It starts the first time one is run
//!   and is kept until the session ends, or until something in it asks for a
//!   `restart`; each program runs in it on the shell's own streams
//!   ([`crate::guest_exec`]).
//!
//! The granted files are read with [`ush_tilo::NativeFiles`]: they are on
//! this device's disk, at the paths their grants name.

use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use collab_types::caps::CapOverride;
use ush_shell::{Input, Output, Program, Run, Std, Stdio};
use ush_tilo::{Acted, Environment, Place, Programs};

use std::os::fd::AsFd as _;
use std::path::PathBuf;

use tilo_clock::unix_now;
use tilo_egress::{Egress, Netns};
use tilo_environment::Environment as Record;

use crate::broker::{Broker, BrokerState, Response, RunGroup};
use crate::file_actions::FileAction;
use crate::guest_exec::{Answer, Request};
use crate::policy::Policy;
use crate::records::Records;
use crate::supervisor::{Resolved, Session};

/// The environment record, through the session's broker.
pub struct RecordEnvironment {
    state: Arc<Mutex<BrokerState>>,
    /// The record's document: its grants are read again only once it
    /// changed.
    document: Option<std::path::PathBuf>,
    grants: Mutex<Option<(Stamp, Vec<CapOverride>)>>,
}

/// What a document looked like when it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: i64,
    modified_ns: i64,
    ino: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.mtime(),
        modified_ns: meta.mtime_nsec(),
        ino: meta.ino(),
    })
}

impl RecordEnvironment {
    pub fn new(state: Arc<Mutex<BrokerState>>) -> Self {
        let document = state.lock().ok().and_then(|state| {
            state
                .environment
                .records
                .record_dir(&state.environment.key)
                .ok()
                .map(|dir| dir.join(app_state::DOCUMENT_FILE))
        });
        Self {
            state,
            document,
            grants: Mutex::new(None),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BrokerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Environment for RecordEnvironment {
    fn grants(&self) -> Vec<CapOverride> {
        let now = self.document.as_deref().and_then(stamp);
        let mut cached = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        if let (Some(now), Some((then, grants))) = (now, cached.as_ref())
            && now == *then
        {
            return grants.clone();
        }
        let grants = self.state().environment.current().grants;
        *cached = now.map(|now| (now, grants.clone()));
        grants
    }

    fn act(&self, action: FileAction, run: Option<&Run>) -> Result<Acted, String> {
        let run = run.map(|run| RunGroup {
            id: run.id.clone(),
            label: run.label.clone(),
        });
        let answer = crate::file_actions::brokered(&self.state(), &action, run.as_ref());
        match answer {
            Response::FileAction { id, operations } => Ok(Acted {
                entry: Some(id),
                operations,
            }),
            Response::Denied { reason } => Err(reason),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }
}

/// Set by the `SIGINT` handler: the person pressed Ctrl-C.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// The write end of [`DevicePlace`]'s wake pipe, for the handler; -1 when
/// there is none.
static WAKE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let fd = WAKE.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1_u8;
        // SAFETY: write(2) is async-signal-safe; one byte from a local.
        unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
    }
}

/// This device as the place the shell runs: its clock, its accounts, and
/// Ctrl-C, which also wakes whatever waits on [`Self::interrupts`].
pub struct DevicePlace {
    wake_read: OwnedFd,
    _wake_write: OwnedFd,
}

impl DevicePlace {
    /// Take Ctrl-C as a request to stop what is running, not as the end of
    /// the shell, and take back the terminal from programs without stopping.
    ///
    /// # Errors
    ///
    /// The wake pipe could not be made.
    pub fn install() -> std::io::Result<Self> {
        let mut fds = [0; 2];
        // SAFETY: pipe2 writes two descriptors into the array we own.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the kernel just returned these two descriptors.
        let (wake_read, wake_write) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        WAKE.store(wake_write.as_raw_fd(), Ordering::SeqCst);
        // SAFETY: the handler only stores to an atomic and writes one byte,
        // both async-signal-safe; the sigaction struct is fully initialised.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_interrupt as *const () as usize;
            // No SA_RESTART: a read of the terminal returns at once.
            action.sa_flags = 0;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(libc::SIGINT, &raw const action, std::ptr::null_mut());
            libc::signal(libc::SIGQUIT, libc::SIG_IGN);
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
            // Taking the terminal back from a program asks from the
            // background.
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        }
        Ok(Self {
            wake_read,
            _wake_write: wake_write,
        })
    }

    /// Readable once Ctrl-C was pressed, until the interrupt is cleared.
    pub fn interrupts(&self) -> BorrowedFd<'_> {
        self.wake_read.as_fd()
    }
}

impl Drop for DevicePlace {
    fn drop(&mut self) {
        WAKE.store(-1, Ordering::SeqCst);
    }
}

impl Place for DevicePlace {
    fn interrupted(&self) -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }

    fn clear_interrupt(&self) {
        INTERRUPTED.store(false, Ordering::SeqCst);
        let mut drain = [0_u8; 64];
        // SAFETY: a non-blocking read into a local buffer of its length.
        while unsafe {
            libc::read(
                self.wake_read.as_raw_fd(),
                drain.as_mut_ptr().cast(),
                drain.len(),
            )
        } > 0
        {}
    }

    fn now(&self) -> i64 {
        i64::try_from(tilo_clock::unix_now()).unwrap_or(0)
    }

    fn utc_offset(&self, at: i64) -> i32 {
        let time: libc::time_t = at;
        // SAFETY: localtime_r writes only into the zeroed struct we own.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&raw const time, &raw mut tm).is_null() {
                return 0;
            }
            i32::try_from(tm.tm_gmtoff).unwrap_or(0)
        }
    }

    fn sleep(&self, millis: u64) {
        let mut fds = [crate::broker::pollfd(
            self.wake_read.as_raw_fd(),
            libc::POLLIN,
        )];
        // Woken by Ctrl-C, else the whole time.
        let _ = crate::broker::poll_fds(&mut fds, Some(std::time::Duration::from_millis(millis)));
    }

    fn user_name(&self, uid: u32) -> Option<String> {
        account_name("/etc/passwd", uid)
    }

    fn group_name(&self, gid: u32) -> Option<String> {
        account_name("/etc/group", gid)
    }

    fn pid(&self) -> u32 {
        std::process::id()
    }

    fn hostname(&self) -> String {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map_or_else(|_| "localhost".into(), |h| h.trim().to_string())
    }
}

/// The name an id has in `passwd` or `group`.
fn account_name(file: &str, id: u32) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _ = fields.next();
        (fields.next()?.parse::<u32>().ok()? == id).then(|| name.to_string())
    })
}

/// The session's own standard streams: what a program gets where the
/// shell's stream is the session's (the terminal).
pub struct SessionStreams {
    pub stdin: OwnedFd,
    pub stdout: OwnedFd,
    pub stderr: OwnedFd,
}

impl SessionStreams {
    fn of(&self, stream: Std) -> std::io::Result<OwnedFd> {
        match stream {
            Std::In => self.stdin.try_clone(),
            Std::Out => self.stdout.try_clone(),
            Std::Err => self.stderr.try_clone(),
        }
    }

    /// The shell's streams over these: standard input and output, and
    /// standard error.
    ///
    /// # Errors
    ///
    /// A descriptor could not be duplicated.
    pub fn stdio(&self) -> std::io::Result<Stdio> {
        Ok(Stdio {
            stdin: Input::host(std::fs::File::from(self.stdin.try_clone()?)),
            stdout: Output::host(Std::Out, std::fs::File::from(self.stdout.try_clone()?)),
            stderr: Output::host(Std::Err, std::fs::File::from(self.stderr.try_clone()?)),
        })
    }
}

/// A guest that serves programs: its control socket, and what keeps it
/// running. Dropping it stops the guest.
pub struct RunningGuest {
    control: UnixStream,
    /// What else lives as long as the guest (its process, its network),
    /// dropped after the control socket closes, in order.
    keep: Vec<Box<dyn Send + Sync>>,
}

impl RunningGuest {
    pub fn new(control: UnixStream, keep: Vec<Box<dyn Send + Sync>>) -> Self {
        Self { control, keep }
    }
}

impl Drop for RunningGuest {
    fn drop(&mut self) {
        // Closing the control socket ends the server; then the guest's
        // process, then its network.
        let _ = self.control.shutdown(std::net::Shutdown::Both);
        for kept in self.keep.drain(..) {
            drop(kept);
        }
    }
}

/// A child process that is killed and reaped when dropped.
pub struct Reaped(pub std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

type Launch = dyn Fn() -> anyhow::Result<RunningGuest> + Send + Sync;

type Launched = std::sync::mpsc::Sender<anyhow::Result<RunningGuest>>;

/// The session's Linux guest, started for the first program and kept for
/// the session.
///
/// It is started on a thread of its own that lasts as long as this does:
/// `bwrap` ends its guest when the thread that started it ends
/// (`--die-with-parent`), and the program that asks first may be running on
/// a pipeline's thread.
pub struct SessionGuest {
    launch: Mutex<std::sync::mpsc::Sender<Launched>>,
    /// Whether something in the guest asked for it to be built again from
    /// the record (`restart`), taking the request.
    restart_asked: Box<dyn Fn() -> bool + Send + Sync>,
    guest: Mutex<Option<Arc<RunningGuest>>>,
    starts: AtomicUsize,
    streams: SessionStreams,
    place: Arc<DevicePlace>,
}

impl SessionGuest {
    pub fn new(
        launch: Box<Launch>,
        restart_asked: Box<dyn Fn() -> bool + Send + Sync>,
        streams: SessionStreams,
        place: Arc<DevicePlace>,
    ) -> Self {
        let (asks, asked) = std::sync::mpsc::channel::<Launched>();
        let _ = std::thread::Builder::new()
            .name("ush-guest".into())
            .spawn(move || {
                while let Ok(reply) = asked.recv() {
                    let _ = reply.send(launch());
                }
            });
        Self {
            launch: Mutex::new(asks),
            restart_asked,
            guest: Mutex::new(None),
            starts: AtomicUsize::new(0),
            streams,
            place,
        }
    }

    /// How many times the guest was started.
    pub fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }

    /// The running guest, started now if there is none.
    fn guest(&self) -> anyhow::Result<Arc<RunningGuest>> {
        let mut guest = self.guest.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = guest.as_ref() {
            return Ok(Arc::clone(running));
        }
        let (reply, launched) = std::sync::mpsc::channel();
        self.launch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(reply)
            .map_err(|_| anyhow::anyhow!("the session's guest launcher stopped"))?;
        let running = Arc::new(
            launched
                .recv()
                .map_err(|_| anyhow::anyhow!("the session's guest launcher stopped"))??,
        );
        self.starts.fetch_add(1, Ordering::SeqCst);
        *guest = Some(Arc::clone(&running));
        drop(guest);
        Ok(running)
    }

    /// Stop the guest: the next program starts it again from the record.
    fn forget(&self) {
        self.guest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    /// The descriptor a program gets for one of the shell's streams, and,
    /// when it is a pipe, the end the shell keeps.
    fn stream(&self, host: Option<Std>) -> std::io::Result<(OwnedFd, Option<OwnedFd>)> {
        if let Some(stream) = host {
            return Ok((self.streams.of(stream)?, None));
        }
        let (reader, writer) = std::io::pipe()?;
        Ok((reader.into(), Some(writer.into())))
    }

    fn take_terminal(&self) {
        let fd = self.streams.stdin.as_raw_fd();
        // SAFETY: plain calls on our own descriptor; SIGTTOU is ignored
        // (DevicePlace::install), so asking from the background is allowed.
        unsafe {
            if libc::isatty(fd) == 1 {
                libc::tcsetpgrp(fd, libc::getpgrp());
            }
        }
    }
}

fn copy_out(from: OwnedFd, to: &Output) {
    use std::io::Read as _;
    let mut from = std::fs::File::from(from);
    let mut buf = [0_u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(buf.get(..n).unwrap_or(&[])).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

fn feed(from: &Input, to: OwnedFd) {
    use std::io::Write as _;
    let mut to = std::fs::File::from(to);
    let mut buf = [0_u8; 16 * 1024];
    while let Ok(n) = from.read(&mut buf) {
        if n == 0 || to.write_all(buf.get(..n).unwrap_or(&[])).is_err() {
            break;
        }
    }
}

impl Programs for SessionGuest {
    fn run(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: Option<&dyn Fn() -> bool>,
    ) -> Program {
        let Some(name) = argv.first() else {
            return Program::Exited(0);
        };
        let failed = |reason: &str| {
            let _ = io.stderr.write_str(&format!("ush: {name}: {reason}\n"));
            Program::Exited(126)
        };
        let guest = match self.guest() {
            Ok(guest) => guest,
            Err(error) => {
                return failed(&format!(
                    "this environment's Linux guest could not be started: {error:#}"
                ));
            }
        };
        let streams = (|| {
            Ok::<_, std::io::Error>((
                self.stream(io.stdin.host_stream())?,
                self.stream(io.stdout.host_stream())?,
                self.stream(io.stderr.host_stream())?,
            ))
        })();
        let Ok(((stdin, feed_end), (stdout, out_end), (stderr, err_end))) = streams else {
            return failed("its streams could not be made");
        };
        // A pipe's ends: for standard input the program reads and the shell
        // writes; for the outputs the other way round.
        let (stdout, out_end) = match out_end {
            Some(writer) => (writer, Some(stdout)),
            None => (stdout, None),
        };
        let (stderr, err_end) = match err_end {
            Some(writer) => (writer, Some(stderr)),
            None => (stderr, None),
        };
        let held_terminal = io.stdin.host_stream() == Some(Std::In);
        let request = Request {
            argv: argv.to_vec(),
            env: env.to_vec(),
            cwd: cwd.to_path_buf(),
        };
        let answer = std::thread::scope(|scope| {
            if let Some(to) = feed_end {
                let from = io.stdin.clone();
                scope.spawn(move || feed(&from, to));
            }
            if let Some(from) = out_end {
                let to = io.stdout.clone();
                scope.spawn(move || copy_out(from, &to));
            }
            if let Some(from) = err_end {
                let to = io.stderr.clone();
                scope.spawn(move || copy_out(from, &to));
            }
            crate::guest_exec::run(
                &guest.control,
                &request,
                [stdin, stdout, stderr],
                Some(self.place.interrupts()),
                stop,
            )
        });
        if held_terminal {
            self.take_terminal();
        }
        if (self.restart_asked)() {
            self.forget();
        }
        match answer {
            Ok(Answer::Exited { status }) => Program::Exited(status),
            Ok(Answer::NotFound) => Program::NotFound,
            Ok(Answer::Failed { reason }) => failed(&reason),
            Err(error) => {
                // The guest went away: the next program starts another.
                self.forget();
                failed(&format!("this environment's Linux guest stopped: {error}"))
            }
        }
    }
}

// The session that runs it: the record, the broker, the shell, and the guest
// when a program needs one.

impl Session {
    /// Run the session's shell on the Tilo side ([`crate::tilo_side`]): the
    /// interpreter here, outside every guest, reading what the record grants
    /// and writing through its actions, with a guest started only for the
    /// first program that needs one. `script` runs to its end; without one
    /// the shell is interactive on the session's terminal.
    ///
    /// # Errors
    ///
    /// The store or the record could not be read, or the session's broker
    /// could not be started.
    pub fn run_tilo_side(&mut self, script: Option<&str>) -> anyhow::Result<i32> {
        self.store.ensure()?;
        let (key, environment) = Records::at(self.store.records_root())
            .resolve(self.opts.environment.as_deref(), &*self.images)?;
        let session_dir = self.store.session_dir(&self.session_id);
        std::fs::create_dir_all(&session_dir)?;
        let outcome = self.tilo_side_in(key, environment, &session_dir, script);
        crate::guest::remove_dir_all_forced(&session_dir);
        outcome
    }

    fn tilo_side_in(
        &self,
        key: String,
        environment: Record,
        session_dir: &Path,
        script: Option<&str>,
    ) -> anyhow::Result<i32> {
        use crate::tilo_side::{
            DevicePlace, Reaped, RecordEnvironment, RunningGuest, SessionGuest, SessionStreams,
        };
        let resolved = Resolved {
            key,
            environment,
            // Resolved when the guest starts, if it ever does.
            images: Vec::new(),
        };
        let broker = Broker::start(
            session_dir.join("broker.sock"),
            self.broker_state(&resolved, None),
        )?;
        let state = Arc::clone(broker.state());
        let place = Arc::new(DevicePlace::install()?);
        let dup = |fd: BorrowedFd<'_>| fd.try_clone_to_owned();
        let streams = || -> std::io::Result<SessionStreams> {
            Ok(SessionStreams {
                stdin: dup(std::io::stdin().as_fd())?,
                stdout: dup(std::io::stdout().as_fd())?,
                // Served, standard error is the supervisor's log; the
                // shell's is the terminal.
                stderr: match &self.opts.terminal {
                    Some(terminal) => dup(terminal.as_fd())?,
                    None => dup(std::io::stderr().as_fd())?,
                },
            })
        };
        let launcher = Session {
            store: self.store.clone(),
            opts: self.opts.clone(),
            session_id: self.session_id.clone(),
            images: Arc::clone(&self.images),
            builder: Arc::clone(&self.builder),
        };
        let launch_state = Arc::clone(&state);
        let socket = broker.socket().to_path_buf();
        let launch = Box::new(move || -> anyhow::Result<RunningGuest> {
            let resolved = launcher.resolve()?;
            let (control, served) = UnixStream::pair()?;
            let socket = socket.clone();
            let (child, netns, egress) =
                launcher.spawn_exec_guest(&resolved, socket, OwnedFd::from(served))?;
            if let Ok(mut state) = launch_state.lock() {
                state.images.clone_from(&resolved.images);
                state.environment.built_from = resolved.environment;
                state.egress = Some(Arc::clone(&egress));
            }
            Ok(RunningGuest::new(
                control,
                vec![Box::new(Reaped(child)), Box::new(egress), Box::new(netns)],
            ))
        });
        let restart_state = Arc::clone(&state);
        let restart_asked = Box::new(move || {
            restart_state
                .lock()
                .is_ok_and(|mut state| std::mem::take(&mut state.restart_requested))
        });
        let programs = Arc::new(SessionGuest::new(
            launch,
            restart_asked,
            streams()?,
            Arc::clone(&place),
        ));
        let session_streams = streams()?;
        let io = session_streams.stdio()?;
        let host: Arc<dyn ush_shell::Host> = Arc::new(ush_tilo::TiloHost::new(
            Arc::new(RecordEnvironment::new(Arc::clone(&state))),
            Arc::new(ush_tilo::NativeFiles),
            programs,
            place,
            io.stderr.clone(),
        ));
        let user = crate::guest::GuestUser::current();
        let now = Some(unix_now());
        let policy = Policy::new(resolved.environment.grants.clone());
        let cwd = policy
            .trusted_dirs(now)
            .into_iter()
            .chain(policy.readable_dirs(now))
            .next()
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut shell = ush_shell::Shell::new(host, self.shell_env(&resolved, &user), cwd, io);
        let status = if let Some(script) = script {
            shell.run_to_exit(script)
        } else {
            let rc = self.store.environment_home(&resolved.key).join(".ushrc");
            if let Ok(text) = std::fs::read_to_string(rc) {
                shell.set_interactive(true);
                let _ = shell.run_script(&text);
            }
            shell.interactive(&ush_shell::Session {
                runs_key: self.opts.runs.clone(),
            })
        };
        drop(shell);
        drop(broker);
        Ok(status)
    }

    /// The Tilo-side shell's variables: what a guest's processes start
    /// with, since every program it runs runs there.
    fn shell_env(
        &self,
        resolved: &Resolved,
        user: &crate::guest::GuestUser,
    ) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = [
            ("PATH", crate::guest::GUEST_PATH.to_owned()),
            ("HOME", user.home().to_string_lossy().into_owned()),
            ("TERM", self.opts.term.clone()),
            ("USER", user.name.clone()),
            ("LOGNAME", user.name.clone()),
            (
                "SHELL",
                crate::shell_host::guest_shell_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("TILO_USH_ENVIRONMENT", resolved.environment.name.clone()),
            ("TILO_USH_BROKER", crate::GUEST_BROKER_SOCK.to_owned()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        for key in crate::guest::INHERITED_ENV {
            if let Some(value) = std::env::var_os(key).filter(|v| !v.is_empty()) {
                env.push(((*key).to_owned(), value.to_string_lossy().into_owned()));
            }
        }
        env
    }

    /// Start the session's guest for the Tilo-side shell's programs: its
    /// first process serves them one command at a time on `control`
    /// ([`crate::guest_exec`]). Answers the guest's process, its network
    /// namespace and its egress, which live as long as it does.
    ///
    /// # Errors
    ///
    /// The guest's network, filter or root could not be set up, or `bwrap`
    /// could not be started.
    pub fn spawn_exec_guest(
        &self,
        resolved: &Resolved,
        broker_socket: PathBuf,
        control: OwnedFd,
    ) -> anyhow::Result<(std::process::Child, Netns, Arc<Egress>)> {
        let session_dir = self.store.session_dir(&self.session_id);
        let (netns, egress) = self.network(resolved)?;
        let egress = Arc::new(egress);
        let seccomp_blob = crate::seccomp::write_guest_filter(&session_dir)?;
        let mut plan = self.plan(resolved, broker_socket, seccomp_blob)?;
        plan.runs = None;
        plan.resolver = Some(egress.resolv_conf().to_owned());
        plan.prepare()?;
        crate::supervisor::seed_guest_home(&plan.guest_home)?;
        let entry = plan.helper_runtime.as_ref().map_or_else(
            || vec![format!("{}/tilo-ush", crate::GUEST_BIN_DIR)],
            crate::helper_runtime::HelperRuntime::entry,
        );
        let mut command = entry.clone();
        command.extend(["guest-init".to_owned(), "--".to_owned()]);
        command.extend(entry);
        command.extend([
            "exec-serve".to_owned(),
            crate::guest_exec::CONTROL_FD_ARG.to_owned(),
            crate::guest_exec::CONTROL_FD.to_string(),
        ]);
        plan.command = command;
        plan.control = Some(Arc::new(control));
        let child = crate::guest::spawn(&plan, Some(&netns))?;
        Ok((child, netns, egress))
    }
}

#[cfg(test)]
mod tests;

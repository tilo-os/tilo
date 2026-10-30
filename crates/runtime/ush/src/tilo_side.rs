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
//! - [`DevicePlace`]: the clock, account names, the session's pty for the
//!   line editor (keys one at a time, its width), Ctrl-C and Ctrl+Z;
//! - [`SessionGuest`]: the session's Linux guest, for the programs that are
//!   none of the shell's own (tier 3). It starts the first time one is run
//!   and is kept until the session ends, or until something in it asks for a
//!   `restart`; each program runs in it on the shell's own streams
//!   ([`crate::guest_exec`]), as part of the shell's job, which stops,
//!   continues and ends its process group there. Tab completes the names of
//!   the programs in the images the guest is built from.
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
use crate::guest_exec::{Answer, Command, InJob, Request, Signal as ExecSignal};
use crate::policy::Policy;
use crate::supervisor::{Resolved, Session};
use ush_shell::jobs::{Job, Signal};

/// The environment record, through the session's broker.
pub struct RecordEnvironment {
    state: Arc<Mutex<BrokerState>>,
    /// The record's document: its grants are read again only once it
    /// changed.
    document: Option<std::path::PathBuf>,
    grants: Mutex<Option<(Stamp, Vec<CapOverride>)>>,
    /// Where a write too large to travel inside its action is kept.
    content: Option<crate::content::EnvironmentContent>,
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
        let (document, content) = state.lock().ok().map_or((None, None), |state| {
            (
                state
                    .environment
                    .records
                    .record_dir(&state.environment.key)
                    .ok()
                    .map(|dir| dir.join(app_state::DOCUMENT_FILE)),
                Some(crate::file_actions::content_in(&state.environment)),
            )
        });
        Self {
            state,
            document,
            grants: Mutex::new(None),
            content,
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

    fn keep(&self) -> std::io::Result<Box<dyn ush_tilo::Keeping>> {
        self.content
            .as_ref()
            .ok_or_else(|| std::io::Error::other("the environment's content store is not here"))?
            .keep()
    }
}

/// Set by the `SIGINT` handler: the person pressed Ctrl-C.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// The write end of [`DevicePlace`]'s wake pipe, for the handler; -1 when
/// there is none.
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// Set by the `SIGTSTP` handler: the person pressed Ctrl+Z while the
/// shell had the terminal (one of its own commands ran, or a program that
/// does not read the terminal).
static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// The write end of [`DevicePlace`]'s suspend pipe, for the handler.
static SUSPEND_WAKE: AtomicI32 = AtomicI32::new(-1);

fn wake(fd: &AtomicI32) {
    let fd = fd.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1_u8;
        // SAFETY: write(2) is async-signal-safe; one byte from a local.
        unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
    }
}

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
    wake(&WAKE);
}

extern "C" fn on_suspend(_: libc::c_int) {
    SUSPENDED.store(true, Ordering::SeqCst);
    wake(&SUSPEND_WAKE);
}

/// A non-blocking pipe: its read end and its write end.
fn wake_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two descriptors into the array we own.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the kernel just returned these two descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Read everything there is in a non-blocking pipe.
fn drain(fd: &OwnedFd) {
    let mut buf = [0_u8; 64];
    // SAFETY: a non-blocking read into a local buffer of its length.
    while unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

/// This device as the place the shell runs: its clock, its accounts, the
/// session's terminal for the line editor, Ctrl-C, which also wakes
/// whatever waits on [`Self::interrupts`], and Ctrl+Z, which wakes what
/// waits on [`Self::suspends`].
pub struct DevicePlace {
    wake_read: OwnedFd,
    _wake_write: OwnedFd,
    suspend_read: OwnedFd,
    _suspend_write: OwnedFd,
    /// The session's terminal, when it has one.
    terminal: Option<OwnedFd>,
    /// The terminal's settings as commands left them, while the line editor
    /// has it taking keys one at a time: what it gets back afterwards.
    cooked: Mutex<Option<libc::termios>>,
    /// Terminal's native editor capability, served by the line editor.
    field: Option<Arc<SessionField>>,
}

impl DevicePlace {
    /// Take Ctrl-C as a request to stop what is running, not as the end of
    /// the shell, and take back the terminal from programs without stopping.
    ///
    /// # Errors
    ///
    /// The wake pipe could not be made.
    pub fn install() -> std::io::Result<Self> {
        let (wake_read, wake_write) = wake_pipe()?;
        let (suspend_read, suspend_write) = wake_pipe()?;
        WAKE.store(wake_write.as_raw_fd(), Ordering::SeqCst);
        SUSPEND_WAKE.store(suspend_write.as_raw_fd(), Ordering::SeqCst);
        // SAFETY: the handler only stores to an atomic and writes one byte,
        // both async-signal-safe; the sigaction struct is fully initialised.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_interrupt as *const () as usize;
            // No SA_RESTART: a read of the terminal returns at once.
            action.sa_flags = 0;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(libc::SIGINT, &raw const action, std::ptr::null_mut());
            // Ctrl+Z stops the job in the foreground, never the shell.
            action.sa_sigaction = on_suspend as *const () as usize;
            libc::sigaction(libc::SIGTSTP, &raw const action, std::ptr::null_mut());
            libc::signal(libc::SIGQUIT, libc::SIG_IGN);
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
            // Taking the terminal back from a program asks from the
            // background.
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::signal(libc::SIGTTIN, libc::SIG_IGN);
        }
        Ok(Self {
            wake_read,
            _wake_write: wake_write,
            suspend_read,
            _suspend_write: suspend_write,
            terminal: None,
            cooked: Mutex::new(None),
            field: None,
        })
    }

    /// The session's terminal is `terminal`: the line editor takes keys
    /// from it one at a time.
    #[must_use]
    pub fn on_terminal(mut self, terminal: OwnedFd) -> Self {
        self.terminal = Some(terminal);
        self
    }

    /// Terminal edits the line at the prompt through `field`.
    #[must_use]
    pub fn with_field(mut self, field: Arc<SessionField>) -> Self {
        self.field = Some(field);
        self
    }

    /// Readable once Ctrl-C was pressed, until the interrupt is cleared.
    pub fn interrupts(&self) -> BorrowedFd<'_> {
        self.wake_read.as_fd()
    }

    /// Readable once Ctrl+Z was pressed, until it is taken
    /// ([`Place::take_suspend`]).
    pub fn suspends(&self) -> BorrowedFd<'_> {
        self.suspend_read.as_fd()
    }

    /// Give the session's terminal back to the shell's own process group:
    /// the prompt is the shell's, whatever program had the terminal last.
    pub fn take_terminal(&self) {
        if let Some(fd) = &self.terminal {
            take_terminal(fd.as_raw_fd());
        }
    }
}

/// Make the shell's own process group the foreground of the terminal `fd`,
/// when it is one. SIGTTOU is ignored ([`DevicePlace::install`]), so asking
/// from the background is allowed.
fn take_terminal(fd: libc::c_int) {
    // SAFETY: plain calls on a descriptor we hold.
    unsafe {
        if libc::isatty(fd) == 1 {
            libc::tcsetpgrp(fd, libc::getpgrp());
        }
    }
}

impl Drop for DevicePlace {
    fn drop(&mut self) {
        WAKE.store(-1, Ordering::SeqCst);
        SUSPEND_WAKE.store(-1, Ordering::SeqCst);
    }
}

impl Place for DevicePlace {
    fn interrupted(&self) -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }

    fn clear_interrupt(&self) {
        INTERRUPTED.store(false, Ordering::SeqCst);
        drain(&self.wake_read);
    }

    fn take_suspend(&self) -> bool {
        // Drained whatever the answer, so nothing waiting on the pipe wakes
        // again for a Ctrl+Z another thread took.
        drain(&self.suspend_read);
        SUSPENDED.swap(false, Ordering::SeqCst)
    }

    fn edit_mode(&self, on: bool) -> bool {
        let Some(fd) = self.terminal.as_ref().map(std::os::fd::AsRawFd::as_raw_fd) else {
            return false;
        };
        let mut cooked = self.cooked.lock().unwrap_or_else(PoisonError::into_inner);
        if !on {
            if let Some(settings) = cooked.take() {
                // SAFETY: tcsetattr reads the termios struct we own, for a
                // descriptor we hold.
                unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw const settings) };
            }
            return true;
        }
        take_terminal(fd);
        // SAFETY: tcgetattr and tcsetattr only read and write the termios
        // struct we own, on a descriptor we hold.
        let was = unsafe {
            let mut now: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &raw mut now) != 0 {
                return false;
            }
            let mut raw = now;
            // Keys one at a time, unechoed; Ctrl-C, Ctrl-Z and Ctrl-S are
            // keys too, and Enter arrives as itself.
            raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
            raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::INLCR | libc::IGNCR);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSADRAIN, &raw const raw) != 0 {
                return false;
            }
            now
        };
        // Switched on twice, it goes back to what it was first.
        cooked.get_or_insert(was);
        true
    }

    fn field(&self) -> Option<Arc<dyn ush_shell::field::Field>> {
        self.field
            .clone()
            .map(|field| field as Arc<dyn ush_shell::field::Field>)
    }

    fn columns(&self) -> usize {
        let Some(fd) = &self.terminal else {
            return 80;
        };
        // SAFETY: TIOCGWINSZ writes only into the winsize struct we own.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &raw mut size) == 0 && size.ws_col > 0
            {
                return usize::from(size.ws_col);
            }
        }
        80
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

    /// [`Self::stdio`], its input read by the line editor through `field`
    /// when Terminal edits the line too ([`SessionField::keys`]).
    ///
    /// # Errors
    ///
    /// A descriptor could not be duplicated.
    pub fn stdio_editing(&self, field: Option<&Arc<SessionField>>) -> std::io::Result<Stdio> {
        let mut io = self.stdio()?;
        if let Some(field) = field {
            let terminal = std::fs::File::from(self.stdin.try_clone()?);
            io.stdin = Input::host(field.keys(terminal));
        }
        Ok(io)
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
    /// The roots of the images the guest is built from, for the names of
    /// its programs.
    listing: Box<dyn Fn() -> Vec<PathBuf> + Send + Sync>,
    roots: Mutex<Option<Vec<PathBuf>>>,
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
            listing: Box::new(Vec::new),
            roots: Mutex::new(None),
        }
    }

    /// The guest is built from the images whose roots `listing` answers:
    /// Tab completes the names of the programs on their `PATH`.
    #[must_use]
    pub fn listing(mut self, listing: Box<dyn Fn() -> Vec<PathBuf> + Send + Sync>) -> Self {
        self.listing = listing;
        self
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
        self.roots
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

/// A program's three streams as the guest gets them, and the shell's ends
/// of those that are pipes.
struct ProgramStreams {
    given: [OwnedFd; 3],
    /// What the shell writes the program's input into.
    feed: Option<OwnedFd>,
    /// What the shell reads the program's output and errors from.
    out: Option<OwnedFd>,
    err: Option<OwnedFd>,
}

impl SessionGuest {
    fn program_streams(&self, io: &Stdio) -> std::io::Result<ProgramStreams> {
        let (stdin, feed) = self.stream(io.stdin.host_stream())?;
        let (stdout, out) = self.stream(io.stdout.host_stream())?;
        let (stderr, err) = self.stream(io.stderr.host_stream())?;
        // A pipe's ends: for standard input the program reads and the shell
        // writes; for the outputs the other way round.
        let (stdout, out) = match out {
            Some(writer) => (writer, Some(stdout)),
            None => (stdout, None),
        };
        let (stderr, err) = match err {
            Some(writer) => (writer, Some(stderr)),
            None => (stderr, None),
        };
        Ok(ProgramStreams {
            given: [stdin, stdout, stderr],
            feed,
            out,
            err,
        })
    }

    /// What became of a program the guest answered for.
    fn ended(&self, name: &str, io: &Stdio, answer: std::io::Result<Answer>) -> Program {
        if (self.restart_asked)() {
            self.forget();
        }
        match answer {
            Ok(Answer::Exited { status }) => Program::Exited(status),
            Ok(Answer::NotFound) => Program::NotFound,
            Ok(Answer::Failed { reason }) => failed(name, io, &reason),
            Ok(other) => failed(name, io, &format!("the guest answered {other:?}")),
            Err(error) => {
                // The guest went away: the next program starts another.
                self.forget();
                failed(
                    name,
                    io,
                    &format!("this environment's Linux guest stopped: {error}"),
                )
            }
        }
    }

    /// The roots of the images the guest is built from, read once.
    fn image_roots(&self) -> Vec<PathBuf> {
        let mut roots = self.roots.lock().unwrap_or_else(PoisonError::into_inner);
        roots.get_or_insert_with(|| (self.listing)()).clone()
    }
}

/// Hand `request`, a program of `job`'s, to the guest and learn its
/// process group. The job's group is held until then, so the job's next
/// program joins it. `Err` is what the guest answered instead.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the job's group is held until the guest named it"
)]
fn start_in_job(
    guest: &RunningGuest,
    mut request: Request,
    given: [OwnedFd; 3],
    job: &Job,
) -> Result<Command, std::io::Result<Answer>> {
    let mut group = job.group();
    request.job = Some(InJob {
        group: group.and_then(|g| i32::try_from(g).ok()),
        foreground: job.foreground(),
    });
    let mut command = Command::start(&guest.control, &request, given).map_err(Err)?;
    loop {
        match command.next(&[], None).map_err(Err)?.answer {
            Some(Answer::Started { group: started }) => {
                *group = Some(i64::from(started));
                return Ok(command);
            }
            Some(answer) if answer.is_final() => return Err(Ok(answer)),
            _ => {}
        }
    }
}

/// Say why `name` could not run.
fn failed(name: &str, io: &Stdio, reason: &str) -> Program {
    let _ = io.stderr.write_str(&format!("ush: {name}: {reason}\n"));
    Program::Exited(126)
}

/// Feed and drain a program's pipes on `scope`'s threads.
fn pump<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    io: &'scope Stdio,
    feed_end: Option<OwnedFd>,
    out_end: Option<OwnedFd>,
    err_end: Option<OwnedFd>,
) {
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
}

/// The programs on `path` in the image roots `roots`: a directory of the
/// guest's `PATH` is that directory in each of the images it is built from.
pub fn programs_in(roots: &[PathBuf], path: &str) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut names = std::collections::BTreeSet::new();
    for dir in path.split(':').filter(|d| d.starts_with('/')) {
        let inside = dir.trim_start_matches('/');
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root.join(inside)) else {
                continue;
            };
            for entry in entries.flatten() {
                // A link is the image's own, pointing into the image: taken
                // as the program it names.
                let runs = entry.file_type().is_ok_and(|ft| {
                    ft.is_symlink()
                        || (ft.is_file()
                            && entry
                                .metadata()
                                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
                });
                if runs {
                    names.insert(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
    }
    names.into_iter().collect()
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
        let guest = match self.guest() {
            Ok(guest) => guest,
            Err(error) => {
                return failed(
                    name,
                    io,
                    &format!("this environment's Linux guest could not be started: {error:#}"),
                );
            }
        };
        let Ok(streams) = self.program_streams(io) else {
            return failed(name, io, "its streams could not be made");
        };
        let held_terminal = io.stdin.host_stream() == Some(Std::In);
        let request = Request {
            argv: argv.to_vec(),
            env: env.to_vec(),
            cwd: cwd.to_path_buf(),
            job: None,
        };
        let answer = std::thread::scope(|scope| {
            pump(scope, io, streams.feed, streams.out, streams.err);
            crate::guest_exec::run(
                &guest.control,
                &request,
                streams.given,
                Some(self.place.interrupts()),
                stop,
            )
        });
        if held_terminal {
            self.place.take_terminal();
        }
        self.ended(name, io, answer)
    }

    fn run_in_job(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        job: &Arc<Job>,
    ) -> Program {
        let Some(name) = argv.first() else {
            return Program::Exited(0);
        };
        let guest = match self.guest() {
            Ok(guest) => guest,
            Err(error) => {
                return failed(
                    name,
                    io,
                    &format!("this environment's Linux guest could not be started: {error:#}"),
                );
            }
        };
        let Ok(streams) = self.program_streams(io) else {
            return failed(name, io, "its streams could not be made");
        };
        let request = Request {
            argv: argv.to_vec(),
            env: env.to_vec(),
            cwd: cwd.to_path_buf(),
            job: None,
        };
        let mut command = match start_in_job(&guest, request, streams.given, job) {
            Ok(command) => command,
            Err(answer) => return self.ended(name, io, answer),
        };
        let Ok(signaller) = command.signaller() else {
            command.signal(ExecSignal::Kill);
            return failed(name, io, "it could not be reached");
        };
        let continued = Arc::clone(job);
        let attached = job.attach(move |signal| {
            signaller.send(match signal {
                Signal::Stop => ExecSignal::Stop,
                Signal::Continue => ExecSignal::Continue {
                    foreground: continued.foreground(),
                },
                Signal::Terminate => ExecSignal::Terminate,
                Signal::Interrupt => ExecSignal::Interrupt,
                Signal::Kill => ExecSignal::Kill,
            });
        });
        let answer = std::thread::scope(|scope| {
            pump(scope, io, streams.feed, streams.out, streams.err);
            let mut passed_on = false;
            loop {
                // Ctrl-C and Ctrl+Z that reached the shell are the job's
                // while it is in the foreground: a program that does not
                // read the terminal does not get them itself.
                let foreground = job.foreground();
                let mut wakes = Vec::new();
                if foreground {
                    wakes.push(self.place.suspends());
                    if !passed_on {
                        wakes.push(self.place.interrupts());
                    }
                }
                let woke = command.next(&wakes, None)?;
                if woke.ready.first().copied().unwrap_or(false) && self.place.take_suspend() {
                    job.stop();
                }
                if woke.ready.get(1).copied().unwrap_or(false) {
                    passed_on = true;
                    command.signal(ExecSignal::Interrupt);
                }
                match woke.answer {
                    Some(Answer::Stopped) => {
                        // Ctrl+Z reached it: the job stops, and the prompt
                        // has the terminal again.
                        job.stopped();
                        self.place.take_terminal();
                    }
                    Some(answer) if answer.is_final() => break Ok(answer),
                    _ => {}
                }
            }
        });
        drop(attached);
        // The job's last program ended: the shell's own commands that follow
        // in the line have the terminal again.
        if job.foreground() && job.programs_running() == 0 {
            self.place.take_terminal();
        }
        self.ended(name, io, answer)
    }

    fn names(&self, path: &str) -> Vec<String> {
        programs_in(&self.image_roots(), path)
    }
}

// The session that runs it: the record, the broker, the shell, and the guest
// when a program needs one.

/// Where the session's shell starts: `~` when the grants reach it, else the
/// first directory they grant.
fn starting_dir(policy: &Policy, home: &Path, now: Option<u64>) -> PathBuf {
    std::iter::once(home.to_path_buf())
        .filter(|home| policy.permits(home, collab_types::caps::Access::Read, now))
        .chain(policy.trusted_dirs(now))
        .chain(policy.readable_dirs(now))
        .next()
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The text of `path`, read as every file is: through the grants.
fn read_through(host: &dyn ush_shell::Host, path: &Path) -> std::io::Result<String> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut host.open_read(path)?, &mut text)?;
    Ok(text)
}

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
        let (key, environment) = self.resolve_record()?;
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
            Reaped, RecordEnvironment, RunningGuest, SessionGuest, SessionStreams,
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
        let dup = |fd: BorrowedFd<'_>| fd.try_clone_to_owned();
        let (place, field) = self.session_place()?;
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
        let listing = self.image_listing(&state);
        let programs = Arc::new(
            SessionGuest::new(launch, restart_asked, streams()?, Arc::clone(&place))
                .listing(listing),
        );
        let session_streams = streams()?;
        let io = session_streams.stdio_editing(field.as_ref())?;
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
        let home = crate::home::home_of(&resolved.environment, &user);
        let cwd = starting_dir(&policy, &home, now);
        let mut shell = ush_shell::Shell::new(
            Arc::clone(&host),
            self.shell_env(&resolved, &user, &home),
            cwd,
            io,
        );
        let status = if let Some(script) = script {
            shell.run_to_exit(script)
        } else {
            let rc = read_through(&*host, &home.join(".ushrc"));
            if let Ok(text) = rc {
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

    /// This device as the session's place: on the session's terminal, with
    /// Terminal's native editor capability, when it asked for one, served
    /// by the line editor.
    fn session_place(&self) -> anyhow::Result<(Arc<DevicePlace>, Option<Arc<SessionField>>)> {
        let field = self
            .opts
            .editor
            .as_ref()
            .map(|socket| SessionField::serve(Arc::clone(socket)))
            .transpose()?;
        let mut place =
            DevicePlace::install()?.on_terminal(std::io::stdin().as_fd().try_clone_to_owned()?);
        if let Some(field) = &field {
            place = place.with_field(Arc::clone(field));
        }
        Ok((Arc::new(place), field))
    }

    /// What names the images the session's guest is built from, for Tab:
    /// the guest's own once it started, else those of the record this
    /// device already holds. Tab never builds or fetches.
    fn image_listing(
        &self,
        state: &Arc<Mutex<BrokerState>>,
    ) -> Box<dyn Fn() -> Vec<PathBuf> + Send + Sync> {
        let lister = Session {
            store: self.store.clone(),
            opts: self.opts.clone(),
            session_id: self.session_id.clone(),
            images: Arc::clone(&self.images),
            builder: Arc::clone(&self.builder),
        };
        let state = Arc::clone(state);
        Box::new(move || {
            let started: Vec<PathBuf> = state.lock().map_or_else(
                |_| Vec::new(),
                |state| state.images.iter().map(|(_, root)| root.clone()).collect(),
            );
            if started.is_empty() {
                lister.local_image_roots()
            } else {
                started
            }
        })
    }

    /// The roots of the record's images that are on this device already,
    /// topmost first; one that would have to be built or fetched is left
    /// out.
    fn local_image_roots(&self) -> Vec<PathBuf> {
        let Ok((_, environment)) = self.resolve_record() else {
            return Vec::new();
        };
        let arch = tilo_environment::host_arch();
        let tools = environment
            .tools
            .iter()
            .rev()
            .filter_map(|tool| tool.image(arch).ok().flatten());
        tools
            .chain(environment.runtime_image(arch).ok())
            .filter(|id| self.images.is_local(id))
            .filter_map(|id| self.images.tree(&id).ok())
            .collect()
    }

    /// The Tilo-side shell's variables: what a guest's processes start
    /// with, since every program it runs runs there.
    fn shell_env(
        &self,
        resolved: &Resolved,
        user: &crate::guest::GuestUser,
        home: &Path,
    ) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = [
            ("PATH", crate::guest::GUEST_PATH.to_owned()),
            ("HOME", home.to_string_lossy().into_owned()),
            ("TERM", self.opts.term.clone()),
            ("USER", user.name.clone()),
            ("LOGNAME", user.name.clone()),
            (
                "XDG_RUNTIME_DIR",
                user.runtime_dir().to_string_lossy().into_owned(),
            ),
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
        plan.resolver = Some(egress.resolv_conf().to_owned());
        plan.prepare()?;
        if !plan.home_by_grants {
            crate::supervisor::seed_guest_home(&plan.guest_home)?;
        }
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

mod field;

pub use field::{Keys, SessionField};

#[cfg(test)]
mod tests;

//! A hosted workspace's shell on a terminal, carried to a person's device as a byte stream.
//!
//! [`EnvironmentTerminals::open`] starts a Terminal session's shell interactively
//! ([`WorkspaceShells::interactive`]) on a pseudo-terminal this host holds the master of, and
//! answers a [`TerminalStream`]: what is typed goes in ([`ToTerminal::In`]), the window's
//! size follows ([`ToTerminal::Resize`], set on the master, which the VM's terminal
//! follows), and what the shell and its programs draw comes out ([`FromTerminal::Out`])
//! until the shell exits ([`FromTerminal::Exited`]).
//!
//! The stream's transport is the caller's: [`serve`] carries a stream over any byte stream in
//! [`wire`]'s frames, and a caller with its own frames uses [`TerminalStream::tell`] and
//! [`TerminalStream::hear`].
//!
//! A session outlives its stream: dropped, the shell is kept for [`LEFT_AFTER`] with what it
//! printed meanwhile, for [`EnvironmentTerminals::attach`] to pick up by the session's id (32
//! hex characters). A second stream on a session supersedes the first
//! ([`FromTerminal::Superseded`]). A session nobody came back to hangs up: the shell and the
//! program on its terminal end, as when a device's terminal window closes.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use linux_vm_protocol::TerminalSize;

pub use crate::environment_programs::LEFT_AFTER;
use crate::workspace_shell::{Interactive, Payer, WorkspaceShells};

/// What a stream sends a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToTerminal {
    /// Bytes typed, as the terminal emulator sends them.
    In(Vec<u8>),
    /// The window's size changed.
    Resize { cols: u16, rows: u16 },
}

/// What a session sends its stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromTerminal {
    /// What the shell and its programs drew.
    Out(Vec<u8>),
    /// The shell exited with this status; the session is over.
    Exited(i32),
    /// Another stream attached to the session; this one gets nothing more.
    Superseded,
}

/// The most of what a session printed that is kept for a stream not reading it: past it, a
/// detached session forgets the oldest, and an attached one waits for its stream.
const KEPT_OUTPUT: usize = 1 << 20;

/// The most one [`FromTerminal::Out`] frame carries.
const OUT_FRAME: usize = 64 * 1024;

/// How often the terminal is looked at while it says nothing.
const TICK: Duration = Duration::from_millis(100);

/// The kept sessions of a host's workspace shells, by id.
pub struct EnvironmentTerminals {
    shells: Arc<WorkspaceShells>,
    left_after: Duration,
    kept: Arc<Table>,
}

type Table = Mutex<HashMap<String, Arc<Kept>>>;

struct Kept {
    id: String,
    workspace: String,
    session: String,
    /// Where typed bytes and sizes go; taken when the session hangs up.
    master: Mutex<Option<std::fs::File>>,
    shell: Mutex<Option<Interactive>>,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// The stream that hears the session; 0 when none does.
    attached: u64,
    streams: u64,
    pending: VecDeque<FromTerminal>,
    pending_bytes: usize,
    /// No stream came back in time, so the terminal was hung up.
    hung_up: bool,
    /// [`FromTerminal::Exited`] was heard.
    over: bool,
    /// The shell ended and [`FromTerminal::Exited`] is queued.
    ended: bool,
}

/// One stream's hold on a session.
pub struct TerminalStream {
    kept: Arc<Kept>,
    stream: u64,
    table: Weak<Table>,
    left_after: Duration,
}

impl EnvironmentTerminals {
    #[must_use]
    pub fn new(shells: Arc<WorkspaceShells>) -> Self {
        Self {
            shells,
            left_after: LEFT_AFTER,
            kept: Arc::default(),
        }
    }

    /// The same sessions, kept for `left_after` once their stream drops.
    #[must_use]
    pub const fn keeping_for(mut self, left_after: Duration) -> Self {
        self.left_after = left_after;
        self
    }

    #[must_use]
    pub fn shells(&self) -> &Arc<WorkspaceShells> {
        &self.shells
    }

    /// A stream on `workspace`'s Terminal session `session` running interactively, its window
    /// `size`: the session's shell started on a new terminal, paid for by `payer`, its runs
    /// marked with `runs_key`; or, when that session is already kept on a terminal, a new
    /// stream on it, superseding the one it had.
    ///
    /// # Errors
    ///
    /// No terminal could be made, or the session's shell could not start.
    pub fn open(
        &self,
        workspace: &str,
        session: &str,
        payer: Option<&Payer>,
        size: TerminalSize,
        runs_key: Option<String>,
    ) -> io::Result<TerminalStream> {
        let running = lock(&self.kept)
            .values()
            .find(|kept| kept.workspace == workspace && kept.session == session && kept.live())
            .cloned();
        if let Some(running) = running {
            running.resize(size);
            return Ok(self.stream_on(running));
        }
        let (master, slave) = pty(size)?;
        let shell = self
            .shells
            .interactive(workspace, session, payer, slave, runs_key)?;
        let reading = master.try_clone()?;
        let mut id = [0_u8; 16];
        getrandom::fill(&mut id).map_err(io::Error::other)?;
        let running = Arc::new(Kept {
            id: hex::encode(id),
            workspace: workspace.to_owned(),
            session: session.to_owned(),
            master: Mutex::new(Some(std::fs::File::from(master))),
            shell: Mutex::new(Some(shell)),
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        lock(&self.kept).insert(running.id.clone(), Arc::clone(&running));
        let pumped = Arc::clone(&running);
        let table = Arc::downgrade(&self.kept);
        std::thread::Builder::new()
            .name("workspace-terminal".into())
            .spawn(move || pump(&pumped, std::fs::File::from(reading), &table))?;
        Ok(self.stream_on(running))
    }

    /// A new stream on the kept session `id`, superseding the one it had; `None` when no
    /// session by that id is kept (it ended, or nobody came back to it in time).
    #[must_use]
    pub fn attach(&self, id: &str) -> Option<TerminalStream> {
        let running = lock(&self.kept).get(id).cloned()?;
        running.live().then(|| self.stream_on(running))
    }

    /// The ids of the sessions kept now, attached or not.
    #[must_use]
    pub fn kept(&self) -> Vec<String> {
        lock(&self.kept).keys().cloned().collect()
    }

    fn stream_on(&self, kept: Arc<Kept>) -> TerminalStream {
        let stream = {
            let mut state = lock(&kept.state);
            state.streams = state.streams.saturating_add(1);
            state.attached = state.streams;
            state.streams
        };
        kept.changed.notify_all();
        TerminalStream {
            kept,
            stream,
            table: Arc::downgrade(&self.kept),
            left_after: self.left_after,
        }
    }
}

impl TerminalStream {
    /// The session's id, for [`EnvironmentTerminals::attach`]: 32 hex characters.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.kept.id
    }

    /// Send `frame` to the session; `false` once this stream is superseded or the session
    /// is over.
    pub fn tell(&self, frame: &ToTerminal) -> bool {
        if !self.current() {
            return false;
        }
        match frame {
            ToTerminal::In(bytes) => self.kept.type_in(bytes),
            ToTerminal::Resize { cols, rows } => self.kept.resize(TerminalSize {
                cols: *cols,
                rows: *rows,
            }),
        }
    }

    /// What the session says next, waited for at most `wait`.
    #[must_use]
    pub fn hear(&self, wait: Duration) -> Option<FromTerminal> {
        let until = Instant::now().checked_add(wait);
        let mut state = lock(&self.kept.state);
        loop {
            if state.attached != self.stream {
                return Some(FromTerminal::Superseded);
            }
            if let Some(frame) = state.pending.pop_front() {
                if let FromTerminal::Out(bytes) = &frame {
                    state.pending_bytes = state.pending_bytes.saturating_sub(bytes.len());
                }
                let over = matches!(frame, FromTerminal::Exited(_));
                if over {
                    state.over = true;
                }
                drop(state);
                self.kept.changed.notify_all();
                if over {
                    forget(&self.table, &self.kept);
                }
                return Some(frame);
            }
            let left = until.map_or(TICK, |until| until.saturating_duration_since(Instant::now()));
            if left.is_zero() {
                return None;
            }
            state = self
                .kept
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn current(&self) -> bool {
        let state = lock(&self.kept.state);
        state.attached == self.stream && !state.over && !state.hung_up
    }
}

impl Drop for TerminalStream {
    fn drop(&mut self) {
        let mut state = lock(&self.kept.state);
        if state.attached != self.stream {
            return;
        }
        state.attached = 0;
        let over = state.over;
        drop(state);
        self.kept.changed.notify_all();
        if over {
            return;
        }
        let stream = self.stream;
        let kept = Arc::clone(&self.kept);
        let table = self.table.clone();
        let left_after = self.left_after;
        let _ = std::thread::Builder::new()
            .name("workspace-terminal-kept".into())
            .spawn(move || {
                std::thread::sleep(left_after);
                let mut state = lock(&kept.state);
                if state.attached != 0 || state.streams != stream {
                    return;
                }
                state.hung_up = true;
                let ended = state.ended;
                drop(state);
                kept.changed.notify_all();
                // Closing the master hangs the terminal up; the pump forgets the session
                // once its shell ended.
                lock(&kept.master).take();
                if ended {
                    forget(&table, &kept);
                }
            });
    }
}

impl Kept {
    fn live(&self) -> bool {
        let state = lock(&self.state);
        !state.hung_up && !state.over
    }

    fn type_in(&self, bytes: &[u8]) -> bool {
        lock(&self.master).as_mut().is_some_and(|file| {
            if interrupt_key(file).is_some_and(|key| bytes.contains(&key)) {
                self.interrupt();
            }
            file.write_all(bytes).is_ok()
        })
    }

    fn interrupt(&self) {
        if let Some(shell) = lock(&self.shell).as_ref() {
            shell.interrupt();
        }
    }

    fn resize(&self, size: TerminalSize) -> bool {
        let size = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        lock(&self.master).as_ref().is_some_and(|file| {
            // SAFETY: TIOCSWINSZ only reads the winsize struct, which lives across the call.
            unsafe { libc::ioctl(file.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) == 0 }
        })
    }

    /// Hand `frame` to the stream, or keep it for the next one.
    fn say(&self, frame: FromTerminal) {
        let mut state = lock(&self.state);
        if let FromTerminal::Out(bytes) = &frame {
            while state.attached != 0
                && !state.hung_up
                && state.pending_bytes.saturating_add(bytes.len()) > KEPT_OUTPUT
            {
                state = self
                    .changed
                    .wait_timeout(state, TICK)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            while state.pending_bytes.saturating_add(bytes.len()) > KEPT_OUTPUT {
                match state.pending.pop_front() {
                    Some(FromTerminal::Out(old)) => {
                        state.pending_bytes = state.pending_bytes.saturating_sub(old.len());
                    }
                    Some(_) | None => break,
                }
            }
            state.pending_bytes = state.pending_bytes.saturating_add(bytes.len());
            if let Some(FromTerminal::Out(last)) = state.pending.back_mut()
                && last.len().saturating_add(bytes.len()) <= OUT_FRAME
            {
                last.extend_from_slice(bytes);
                drop(state);
                self.changed.notify_all();
                return;
            }
        }
        state.pending.push_back(frame);
        drop(state);
        self.changed.notify_all();
    }

    fn hung_up(&self) -> bool {
        lock(&self.state).hung_up
    }
}

/// The key that interrupts on `master`'s terminal while it turns keys into signals (a
/// command the shell runs itself, not the line editor or a program on a terminal in the VM,
/// which take keys as they come).
fn interrupt_key(master: &std::fs::File) -> Option<u8> {
    // SAFETY: tcgetattr writes only into the termios struct owned here.
    unsafe {
        let mut modes: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(master.as_raw_fd(), &raw mut modes) != 0 {
            return None;
        }
        (modes.c_lflag & libc::ISIG != 0).then_some(modes.c_cc[libc::VINTR])
    }
}

/// Carry what the terminal draws to the session's streams until the shell is done with it,
/// then its exit status.
fn pump(kept: &Arc<Kept>, master: std::fs::File, table: &Weak<Table>) {
    let mut buffer = vec![0_u8; OUT_FRAME];
    let tick = libc::c_int::try_from(TICK.as_millis()).unwrap_or(100);
    while !kept.hung_up() {
        let mut poll = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll reads and writes only the one pollfd owned here.
        let ready = unsafe { libc::poll(&raw mut poll, 1, tick) };
        if ready <= 0 {
            continue;
        }
        if poll.revents & libc::POLLIN == 0 {
            // POLLHUP: every slave closed, the shell is done with the terminal.
            break;
        }
        match (&master).read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => kept.say(FromTerminal::Out(
                buffer.get(..read).unwrap_or_default().to_vec(),
            )),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // A master reads EIO once the last slave is closed.
            Err(_) => break,
        }
    }
    // Hung up, the shell sees it only once no master is left open.
    drop(master);
    let shell = lock(&kept.shell).take();
    let status = shell.map_or(1, Interactive::wait);
    // The shell is gone: take the master so nothing types into a closed session.
    lock(&kept.master).take();
    kept.say(FromTerminal::Exited(status));
    let mut state = lock(&kept.state);
    state.ended = true;
    let hung_up = state.hung_up;
    drop(state);
    if hung_up {
        forget(table, kept);
    }
}

fn forget(table: &Weak<Table>, kept: &Arc<Kept>) {
    if let Some(table) = table.upgrade() {
        let mut table = lock(&table);
        if table
            .get(&kept.id)
            .is_some_and(|known| Arc::ptr_eq(known, kept))
        {
            table.remove(&kept.id);
        }
    }
}

/// A pseudo-terminal of `size`, its master and slave, neither inherited by programs.
fn pty(size: TerminalSize) -> io::Result<(OwnedFd, OwnedFd)> {
    let (mut master, mut slave) = (-1, -1);
    let size = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes the two descriptors and reads only `size`.
    let opened = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &raw const size,
        )
    };
    if opened != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty succeeded; both descriptors are owned only here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [&master, &slave] {
        // SAFETY: F_SETFD only changes the descriptor's flags.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((master, slave))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a stream ended when [`serve`] carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The shell exited with this status.
    Exited(i32),
    /// Another stream took the session.
    Superseded,
    /// The far end went away; the session is kept for [`LEFT_AFTER`].
    Left,
}

/// Carry `stream` over a byte stream: [`wire`] frames of [`ToTerminal`] read from `from`,
/// [`FromTerminal`] written to `to`, until the shell exits, another stream takes the session,
/// or the far end goes away.
///
/// # Errors
///
/// Never: a stream that fails to write has [`Ended::Left`].
pub fn serve(
    stream: TerminalStream,
    from: impl Read + Send + 'static,
    mut to: impl Write,
) -> io::Result<Ended> {
    let stream = Arc::new(stream);
    let left = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reading = Arc::clone(&stream);
    let gone = Arc::clone(&left);
    std::thread::Builder::new()
        .name("workspace-terminal-in".into())
        .spawn(move || {
            let mut from = from;
            while let Ok(Some(frame)) = wire::read_to(&mut from) {
                if !reading.tell(&frame) {
                    break;
                }
            }
            gone.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(reading);
        })?;
    loop {
        if left.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(Ended::Left);
        }
        let Some(frame) = stream.hear(TICK) else {
            continue;
        };
        if wire::write_from(&mut to, &frame)
            .and_then(|()| to.flush())
            .is_err()
        {
            return Ok(Ended::Left);
        }
        match frame {
            FromTerminal::Exited(status) => return Ok(Ended::Exited(status)),
            FromTerminal::Superseded => return Ok(Ended::Superseded),
            FromTerminal::Out(_) => {}
        }
    }
}

/// The frames of a terminal stream: a tag byte, the payload's length as a big-endian `u32`,
/// and the payload.
///
/// ```text
/// 1 In          the bytes typed
/// 2 Resize      cols: u16, rows: u16
/// 3 Out         the bytes drawn
/// 4 Exited      status: i32
/// 5 Superseded  (empty)
/// ```
pub mod wire {
    use std::io::{self, Read, Write};

    use super::{FromTerminal, ToTerminal};

    /// The most one frame's payload holds.
    pub const MAX_FRAME: usize = 1 << 20;

    const IN: u8 = 1;
    const RESIZE: u8 = 2;
    const OUT: u8 = 3;
    const EXITED: u8 = 4;
    const SUPERSEDED: u8 = 5;

    /// # Errors
    ///
    /// The write failed, or the payload is over [`MAX_FRAME`].
    pub fn write_to(to: &mut impl Write, frame: &ToTerminal) -> io::Result<()> {
        match frame {
            ToTerminal::In(bytes) => write(to, IN, bytes),
            ToTerminal::Resize { cols, rows } => {
                let mut size = cols.to_be_bytes().to_vec();
                size.extend_from_slice(&rows.to_be_bytes());
                write(to, RESIZE, &size)
            }
        }
    }

    /// The next frame; `None` when the stream ended between frames.
    ///
    /// # Errors
    ///
    /// The read failed, or the frame is not one a stream sends a session.
    pub fn read_to(from: &mut impl Read) -> io::Result<Option<ToTerminal>> {
        let Some((tag, payload)) = read(from)? else {
            return Ok(None);
        };
        match (tag, payload.as_slice()) {
            (IN, _) => Ok(Some(ToTerminal::In(payload))),
            (RESIZE, &[c0, c1, r0, r1]) => Ok(Some(ToTerminal::Resize {
                cols: u16::from_be_bytes([c0, c1]),
                rows: u16::from_be_bytes([r0, r1]),
            })),
            _ => Err(invalid(tag)),
        }
    }

    /// # Errors
    ///
    /// The write failed, or the payload is over [`MAX_FRAME`].
    pub fn write_from(to: &mut impl Write, frame: &FromTerminal) -> io::Result<()> {
        match frame {
            FromTerminal::Out(bytes) => write(to, OUT, bytes),
            FromTerminal::Exited(status) => write(to, EXITED, &status.to_be_bytes()),
            FromTerminal::Superseded => write(to, SUPERSEDED, &[]),
        }
    }

    /// The next frame; `None` when the stream ended between frames.
    ///
    /// # Errors
    ///
    /// The read failed, or the frame is not one a session sends a stream.
    pub fn read_from(from: &mut impl Read) -> io::Result<Option<FromTerminal>> {
        let Some((tag, payload)) = read(from)? else {
            return Ok(None);
        };
        match (tag, payload.as_slice()) {
            (OUT, _) => Ok(Some(FromTerminal::Out(payload))),
            (EXITED, &[a, b, c, d]) => Ok(Some(FromTerminal::Exited(i32::from_be_bytes([
                a, b, c, d,
            ])))),
            (SUPERSEDED, []) => Ok(Some(FromTerminal::Superseded)),
            _ => Err(invalid(tag)),
        }
    }

    fn invalid(tag: u8) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not a terminal stream frame: tag {tag}"),
        )
    }

    fn write(to: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
        let len = u32::try_from(payload.len())
            .ok()
            .filter(|_| payload.len() <= MAX_FRAME)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
        let mut frame = Vec::with_capacity(payload.len().saturating_add(5));
        frame.push(tag);
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(payload);
        to.write_all(&frame)
    }

    fn read(from: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
        let mut head = [0_u8; 5];
        match from.read_exact(&mut head) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let [tag, l0, l1, l2, l3] = head;
        let len = usize::try_from(u32::from_be_bytes([l0, l1, l2, l3]))
            .ok()
            .filter(|len| *len <= MAX_FRAME)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
        let mut payload = vec![0_u8; len];
        from.read_exact(&mut payload)?;
        Ok(Some((tag, payload)))
    }
}

#[cfg(test)]
#[path = "environment_terminals_tests.rs"]
mod tests;

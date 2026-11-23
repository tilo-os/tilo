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
//!
//! A host hands a stream to one device ([`EnvironmentTerminals::hand_out`]): the device dials
//! the host's [`ALPN`], says which stream it takes ([`wire::Hello`]), and is answered
//! ([`wire::Welcome`]) before the frames; another device presenting the same id is refused
//! ([`EnvironmentTerminals::serve_handed`]). A stream nobody took within [`HANDED_FOR`] is
//! dropped, which leaves its session kept as any dropped stream does.
//!
//! Every command line the session's shell runs is told of as it ends ([`Ran`]), from the
//! shell's own tracking of its runs ([`ush_shell::Runs`]), with what it printed meanwhile: to
//! what the session was opened with ([`TerminalAsk::ran`]), whether a stream is attached or
//! not. A host that runs the shell for another hands those runs out as a stream of their own
//! ([`EnvironmentTerminals::hand_runs`], [`wire::read_ran`]).

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use linux_vm_protocol::TerminalSize;
use tilo_actions::remote::environment_commands::{Person, VmSecret};

/// How long a session is kept when its stream went away and nobody took it again.
pub const LEFT_AFTER: Duration = Duration::from_secs(90);

/// The protocol a device dials a host's terminal streams on.
pub use runtime_portal::workspace_terminal::ALPN;

/// How long a stream handed out waits for its device to take it.
pub const HANDED_FOR: Duration = Duration::from_secs(60);

/// The most of what a run printed that is kept with it ([`Ran::output`]).
pub const RAN_OUTPUT: usize = 512 * 1024;

use crate::workspace_shell::{Interactive, Payer, WorkspaceShells};

/// A command line a session's shell ran: what it was, where, when, how it ended, and what it
/// printed on the terminal while it ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ran {
    pub command: String,
    pub cwd: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub exit: i32,
    /// What it printed, at most [`RAN_OUTPUT`] of it.
    #[serde(skip)]
    pub output: Vec<u8>,
    /// It printed more than was kept.
    pub truncated: bool,
}

/// What a session's runs go to, each as it ends.
pub type RanTo = Arc<dyn Fn(Ran) + Send + Sync>;

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
    handed: Handed<Hold>,
}

/// What a host hands a device: a stream on a session, or the runs of one.
enum Hold {
    Stream(TerminalStream),
    Runs(RanQueue),
}

/// What a host handed out for one device each to take, by a one-use id (32 hex characters):
/// taken by that device alone, once, and dropped when nobody took it within its time.
pub struct Handed<T> {
    waiting: Arc<Waiting<T>>,
    waits: Duration,
}

/// What waits for its device, by its id: the device, and what it takes.
type Waiting<T> = Mutex<HashMap<String, ([u8; 32], T)>>;

impl<T: Send + 'static> Handed<T> {
    #[must_use]
    pub fn new(waits: Duration) -> Self {
        Self {
            waiting: Arc::default(),
            waits,
        }
    }

    /// Hand `value` to `device`: the id it takes it by.
    ///
    /// # Errors
    ///
    /// No id could be drawn, or nothing could drop it once its time passed.
    pub fn hand(&self, device: [u8; 32], value: T) -> io::Result<String> {
        let mut id = [0_u8; 16];
        getrandom::fill(&mut id).map_err(io::Error::other)?;
        let id = hex::encode(id);
        lock(&self.waiting).insert(id.clone(), (device, value));
        let waiting = Arc::downgrade(&self.waiting);
        let (forgotten, waits) = (id.clone(), self.waits);
        std::thread::Builder::new()
            .name("terminal-handed".into())
            .spawn(move || {
                std::thread::sleep(waits);
                if let Some(waiting) = waiting.upgrade() {
                    let dropped = lock(&waiting).remove(&forgotten);
                    drop(dropped);
                }
            })?;
        Ok(id)
    }

    /// What `id` hands `device`; `None` when it hands it nothing: no such id, its time
    /// passed, it was taken, or it is another device's, which keeps it.
    #[must_use]
    pub fn take(&self, id: &str, device: &[u8; 32]) -> Option<T> {
        let mut waiting = lock(&self.waiting);
        if waiting.get(id).is_some_and(|(owner, _)| owner == device) {
            return waiting.remove(id).map(|(_, value)| value);
        }
        None
    }
}

type Table = Mutex<HashMap<String, Arc<Kept>>>;

struct Kept {
    id: String,
    workspace: String,
    session: String,
    /// What the shell's runs are told to, and what reads the master while one runs.
    runs: Option<Arc<SessionRuns>>,
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

/// A session's shell asked for on a terminal ([`EnvironmentTerminals::open_for`]).
pub struct TerminalAsk<'a> {
    pub workspace: &'a str,
    pub session: &'a str,
    /// Who pays for the VM its programs run in, when not the host's own account.
    pub payer: Option<&'a Payer>,
    /// Who the shell runs as: the person or agent who opened it, whom what it makes records.
    pub person: Option<&'a Person>,
    /// The window's size.
    pub cols: u16,
    pub rows: u16,
    /// What each command line the session runs goes to as it ends, attached or not; kept
    /// only by the open that starts the session's shell.
    pub ran: Option<RanTo>,
    /// The kept session to come back to instead, by its id.
    pub attach: Option<&'a str>,
    /// What every program the session's shell starts in the VM is given, over any variable of
    /// the same name: read once, as the session starts, and never the shell's own variables.
    pub secrets: &'a [VmSecret],
}

/// A stream handed to a device: the one-use id it takes it by, and its session's id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandedOut {
    pub stream: String,
    pub session: String,
    /// This open started the session's shell, rather than coming back to it: its
    /// [`TerminalAsk::ran`] hears the session's runs.
    pub started: bool,
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
            handed: Handed::new(HANDED_FOR),
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

    /// A stream on `asked`'s Terminal session running interactively in its window: the
    /// session's shell started on a new terminal, paid for by its payer, as its person, its runs
    /// marked with `runs_key`, each told to its `ran` as it ends, every program it starts in the
    /// VM given its secrets; or, when that session is already kept on a terminal, a new stream
    /// on it, superseding the one it had (and `ran` dropped). Its `attach` is not read here.
    ///
    /// # Errors
    ///
    /// No terminal could be made, or the session's shell could not start.
    pub fn open(
        &self,
        asked: &TerminalAsk<'_>,
        runs_key: Option<String>,
    ) -> io::Result<TerminalStream> {
        let (workspace, session) = (asked.workspace, asked.session);
        let size = TerminalSize {
            cols: asked.cols,
            rows: asked.rows,
        };
        let running = lock(&self.kept)
            .values()
            .find(|kept| kept.workspace == workspace && kept.session == session && kept.live())
            .cloned();
        if let Some(running) = running {
            running.resize(size);
            return Ok(self.stream_on(running));
        }
        let (master, slave) = pty(size)?;
        let runs = match asked.ran.clone() {
            Some(to) => Some(Arc::new(SessionRuns {
                master: std::fs::File::from(master.try_clone()?),
                to: Mutex::new(Some(to)),
                running: Mutex::new(None),
                kept: OnceLock::new(),
            })),
            None => None,
        };
        let heard = runs.clone().map(|runs| runs as Arc<dyn ush_shell::Runs>);
        let shell = self.shells.interactive(
            asked.workspace,
            asked.session,
            asked.payer,
            asked.person,
            slave,
            ush_shell::Session {
                runs_key,
                runs: heard,
            },
            asked.secrets.to_vec(),
        )?;
        let reading = master.try_clone()?;
        let mut id = [0_u8; 16];
        getrandom::fill(&mut id).map_err(io::Error::other)?;
        let running = Arc::new(Kept {
            id: hex::encode(id),
            workspace: workspace.to_owned(),
            session: session.to_owned(),
            runs: runs.clone(),
            master: Mutex::new(Some(std::fs::File::from(master))),
            shell: Mutex::new(Some(shell)),
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        if let Some(runs) = &runs {
            let _ = runs.kept.set(Arc::downgrade(&running));
        }
        lock(&self.kept).insert(running.id.clone(), Arc::clone(&running));
        let pumped = Arc::clone(&running);
        let table = Arc::downgrade(&self.kept);
        std::thread::Builder::new()
            .name("workspace-terminal".into())
            .spawn(move || pump(&pumped, std::fs::File::from(reading), &table))?;
        Ok(self.stream_on(running))
    }

    /// A new stream on the kept session `id` of `workspace`'s Terminal session `session`,
    /// superseding the one it had; `None` when no session by that id is kept for it (it
    /// ended, nobody came back to it in time, or it is another session's).
    #[must_use]
    pub fn attach(&self, workspace: &str, session: &str, id: &str) -> Option<TerminalStream> {
        let running = lock(&self.kept)
            .get(id)
            .filter(|kept| kept.workspace == workspace && kept.session == session)
            .cloned()?;
        running.live().then(|| self.stream_on(running))
    }

    /// Open `asked` on a terminal, or come back to the session it names, and hand its stream
    /// to `device` ([`Self::hand_out`]).
    ///
    /// # Errors
    ///
    /// The session to come back to is gone, or the shell could not start: in words.
    pub fn open_for(&self, device: [u8; 32], asked: &TerminalAsk<'_>) -> Result<HandedOut, String> {
        let stream = match asked.attach {
            Some(id) => self
                .attach(asked.workspace, asked.session, id)
                .ok_or_else(|| "the terminal session ended".to_owned())?,
            None => self
                .open(asked, None)
                .map_err(|error| format!("the session's shell did not start: {error}"))?,
        };
        let session = stream.id().to_owned();
        // The first stream on a session is the one its shell started with.
        let started = asked.attach.is_none() && stream.stream == 1;
        let stream = self
            .hand_out(device, stream)
            .map_err(|error| error.to_string())?;
        Ok(HandedOut {
            stream,
            session,
            started,
        })
    }

    /// Hand `stream` to `device`, to take on a stream of its own ([`Self::serve_handed`]): the
    /// one-use id it presents.
    ///
    /// # Errors
    ///
    /// No id could be drawn.
    pub fn hand_out(&self, device: [u8; 32], stream: TerminalStream) -> io::Result<String> {
        self.handed.hand(device, Hold::Stream(stream))
    }

    /// Hand the runs `runs` keeps of session `session` to `device`, to follow on a stream of
    /// its own ([`Self::serve_handed`]): the one-use id it presents.
    ///
    /// # Errors
    ///
    /// No id could be drawn.
    pub fn hand_runs(&self, device: [u8; 32], runs: RanQueue) -> io::Result<String> {
        self.handed.hand(device, Hold::Runs(runs))
    }

    /// Carry, for the device `device` that dialled, the stream it names in its
    /// [`wire::Hello`] on `from`, answering on `to` first ([`wire::Welcome`]); a stream not
    /// handed to it is refused there. Runs handed out ([`Self::hand_runs`]) are carried as
    /// [`wire::write_ran`] frames until their session ends.
    ///
    /// # Errors
    ///
    /// The hello could not be read or answered, or it named no stream handed to `device`.
    pub fn serve_handed(
        &self,
        device: [u8; 32],
        mut from: impl Read + Send + 'static,
        mut to: impl Write,
    ) -> io::Result<Ended> {
        let Some(hello) = wire::read_hello(&mut from)? else {
            return Ok(Ended::Left);
        };
        let stream = match self.handed.take(&hello.stream, &device) {
            None => return Err(wire::refuse(&mut to, NOT_HANDED)),
            Some(Hold::Runs(runs)) => return serve_runs(&runs, to),
            Some(Hold::Stream(stream)) => stream,
        };
        wire::write_welcome(
            &mut to,
            &wire::Welcome::Opened {
                session: stream.id().to_owned(),
            },
        )?;
        to.flush()?;
        serve(stream, from, to)
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

/// A host's terminals going away hang up every session they keep: nobody could attach to
/// one again, so its shell and the program on its terminal end now.
impl Drop for EnvironmentTerminals {
    fn drop(&mut self) {
        for kept in lock(&self.kept).drain().map(|(_, kept)| kept) {
            kept.hang_up(None);
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
            let left = until.map_or(TICK, |until| {
                until.saturating_duration_since(Instant::now())
            });
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
                // The pump forgets the session once its shell ended.
                if kept.hang_up(Some(stream)) == Some(true) {
                    forget(&table, &kept);
                }
            });
    }
}

impl Kept {
    /// Close the terminal's master, which hangs it up, unless a stream attached since
    /// `left_by` (the last stream) left. Answers whether it hung up, and whether the shell had
    /// already ended.
    fn hang_up(&self, left_by: Option<u64>) -> Option<bool> {
        let mut state = lock(&self.state);
        if left_by.is_some_and(|stream| state.attached != 0 || state.streams != stream) {
            return None;
        }
        state.hung_up = true;
        let ended = state.ended;
        drop(state);
        self.changed.notify_all();
        lock(&self.master).take();
        Some(ended)
    }

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

    /// Read what the terminal drew from `master` and say it: `None` when a run's end took it
    /// first ([`SessionRuns`]), else how much was read (0 at its end). What a run's command
    /// drew is kept with the run, read under the same hold a run's end reads the rest under.
    fn read_out(&self, master: &std::fs::File, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        let mut running = self.runs.as_ref().map(|runs| lock(&runs.running));
        if running.is_some() && !readable(master) {
            return Ok(None);
        }
        let read = (&*master).read(buffer)?;
        let bytes = buffer.get(..read).unwrap_or_default();
        if !bytes.is_empty() {
            if let Some(Some(run)) = running.as_deref_mut() {
                run.keep(bytes);
            }
            self.say(FromTerminal::Out(bytes.to_vec()));
        }
        drop(running);
        Ok(Some(read))
    }
}

/// Whether `master` has something to read now.
fn readable(master: &std::fs::File) -> bool {
    let mut poll = libc::pollfd {
        fd: master.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll reads and writes only the one pollfd owned here.
    let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
    ready > 0 && poll.revents & libc::POLLIN != 0
}

/// What hears a session's shell run its command lines ([`ush_shell::Runs`]), keeping what
/// each drew on the terminal and telling the run to [`TerminalAsk::ran`] as it ends.
///
/// The shell writes a run's output to the terminal before it says the run ended, so at the end
/// all of it is drawn: what the pump did not read yet is read here, under the hold the pump
/// reads under, in the order it was drawn. At a start, what was drawn before (the prompt, the
/// line as typed) is read off the same way, so it is not the run's.
struct SessionRuns {
    master: std::fs::File,
    /// Let go once the shell exited.
    to: Mutex<Option<RanTo>>,
    running: Mutex<Option<Running>>,
    kept: OnceLock<Weak<Kept>>,
}

/// A run going on: what [`Ran`] it is so far.
struct Running {
    number: u64,
    ran: Ran,
}

impl Running {
    fn keep(&mut self, bytes: &[u8]) {
        let room = RAN_OUTPUT.saturating_sub(self.ran.output.len());
        if bytes.len() > room {
            self.ran.truncated = true;
        }
        self.ran
            .output
            .extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or_default());
    }
}

impl std::fmt::Debug for SessionRuns {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SessionRuns")
    }
}

impl SessionRuns {
    /// Read and say what the terminal drew that the pump did not read yet, keeping it with
    /// `running` when a run goes on.
    fn drain(&self, running: &mut Option<Running>) {
        let Some(kept) = self.kept.get().and_then(Weak::upgrade) else {
            return;
        };
        let mut buffer = vec![0_u8; OUT_FRAME];
        while readable(&self.master) {
            let read = match (&self.master).read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            let bytes = buffer.get(..read).unwrap_or_default();
            if let Some(run) = running.as_mut() {
                run.keep(bytes);
            }
            kept.say(FromTerminal::Out(bytes.to_vec()));
        }
    }
}

impl ush_shell::Runs for SessionRuns {
    fn started(&self, number: u64, command: &str, cwd: &std::path::Path) {
        let mut running = lock(&self.running);
        let mut none = None;
        self.drain(&mut none);
        *running = Some(Running {
            number,
            ran: Ran {
                command: command.to_owned(),
                cwd: cwd.to_string_lossy().into_owned(),
                started_ms: now_ms(),
                ..Ran::default()
            },
        });
    }

    fn ended(&self, number: u64, status: i32) {
        let mut running = lock(&self.running);
        self.drain(&mut running);
        let Some(Running { mut ran, .. }) = running.take().filter(|run| run.number == number)
        else {
            return;
        };
        drop(running);
        ran.exit = status;
        ran.ended_ms = now_ms();
        let to = lock(&self.to).clone();
        if let Some(to) = to {
            to(ran);
        }
    }
}

fn now_ms() -> i64 {
    tilo_clock::unix_now_ms_i64()
}

/// The runs of one session, kept for the one stream that follows them
/// ([`EnvironmentTerminals::hand_runs`]): what [`Self::to`] is told, in order, until the
/// session ends, when every [`RanTo`] it handed out is dropped.
#[derive(Clone)]
pub struct RanQueue {
    session: Arc<OnceLock<String>>,
    queue: Arc<(Mutex<Queued>, Condvar)>,
}

#[derive(Default)]
struct Queued {
    ran: VecDeque<Ran>,
    /// Every [`RanTo`] it handed out is gone: nothing more is told.
    over: bool,
}

/// What a [`RanTo`] of a [`RanQueue`] holds: dropped with it, the queue is over.
struct Telling(Arc<(Mutex<Queued>, Condvar)>);

impl Drop for Telling {
    fn drop(&mut self) {
        lock(&self.0.0).over = true;
        self.0.1.notify_all();
    }
}

/// The most runs a queue keeps that nobody follows; past it, the oldest go.
const QUEUED_RUNS: usize = 1024;

impl Default for RanQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl RanQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: Arc::default(),
            queue: Arc::default(),
        }
    }

    /// What the session's runs go to, for [`TerminalAsk::ran`].
    #[must_use]
    pub fn to(&self) -> RanTo {
        let telling = Telling(Arc::clone(&self.queue));
        Arc::new(move |ran: Ran| {
            let (queue, changed) = &*telling.0;
            let mut queued = lock(queue);
            if queued.ran.len() >= QUEUED_RUNS {
                queued.ran.pop_front();
            }
            queued.ran.push_back(ran);
            drop(queued);
            changed.notify_all();
        })
    }

    /// Name the session the runs are of, for the stream that follows them.
    pub fn of(&self, session: &str) {
        let _ = self.session.set(session.to_owned());
    }

    /// The next run; `None` once the session ended and every run was taken.
    fn next(&self) -> Option<Ran> {
        let (queue, changed) = &*self.queue;
        let mut queued = lock(queue);
        loop {
            if let Some(ran) = queued.ran.pop_front() {
                return Some(ran);
            }
            if queued.over {
                return None;
            }
            queued = changed.wait(queued).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Carry `runs` to `to`, as [`wire::write_ran`] frames after a welcome, until their session
/// ended or the far end went away.
fn serve_runs(runs: &RanQueue, mut to: impl Write) -> io::Result<Ended> {
    let session = runs.session.get().cloned().unwrap_or_default();
    wire::write_welcome(&mut to, &wire::Welcome::Opened { session })?;
    to.flush()?;
    while let Some(ran) = runs.next() {
        if wire::write_ran(&mut to, &ran)
            .and_then(|()| to.flush())
            .is_err()
        {
            return Ok(Ended::Left);
        }
    }
    Ok(Ended::Left)
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
        match kept.read_out(&master, &mut buffer) {
            Ok(Some(0)) => break,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // A master reads EIO once the last slave is closed.
            Err(_) => break,
        }
    }
    // Hung up, the shell sees it only once no master is left open.
    drop(master);
    let shell = lock(&kept.shell).take();
    let status = shell.map_or(1, Interactive::wait);
    // The shell is gone: take the master so nothing types into a closed session, and let go
    // of what its runs went to, which hears no more.
    lock(&kept.master).take();
    if let Some(runs) = &kept.runs {
        lock(&runs.to).take();
    }
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

/// What a device presenting a stream not handed to it is told.
pub const NOT_HANDED: &str = "no such terminal stream for this device";

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
/// 6 Hello       JSON: the stream a device takes, first on the stream
/// 7 Welcome     JSON: the answer to it, before every other frame
/// 8 Ran         a run's JSON, a newline, then what it printed
/// ```
pub mod wire {
    use std::io::{self, Read, Write};

    use serde::{Deserialize, Serialize};

    use super::{FromTerminal, Ran, ToTerminal};

    /// What a device says first on a terminal stream: the stream it takes.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Hello {
        /// The one-use id the host handed it.
        pub stream: String,
    }

    /// The host's answer to a [`Hello`].
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "answer", rename_all = "snake_case")]
    pub enum Welcome {
        /// The stream is the device's: the frames of the session `session` follow.
        Opened { session: String },
        /// It is not; nothing follows.
        Refused { reason: String },
    }

    /// The most one frame's payload holds.
    pub const MAX_FRAME: usize = 1 << 20;

    const IN: u8 = 1;
    const RESIZE: u8 = 2;
    const OUT: u8 = 3;
    const EXITED: u8 = 4;
    const SUPERSEDED: u8 = 5;
    const HELLO: u8 = 6;
    const WELCOME: u8 = 7;
    const RAN: u8 = 8;

    /// # Errors
    ///
    /// The write failed, or the run is over [`MAX_FRAME`].
    pub fn write_ran(to: &mut impl Write, ran: &Ran) -> io::Result<()> {
        let mut payload = serde_json::to_vec(ran).map_err(io::Error::other)?;
        payload.push(b'\n');
        payload.extend_from_slice(&ran.output);
        write(to, RAN, &payload)
    }

    /// The next run on a stream of runs; `None` when it ended between frames.
    ///
    /// # Errors
    ///
    /// The read failed, or the frame is no run.
    pub fn read_ran(from: &mut impl Read) -> io::Result<Option<Ran>> {
        let Some((tag, payload)) = read(from)? else {
            return Ok(None);
        };
        if tag != RAN {
            return Err(invalid(tag));
        }
        let at = payload
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| invalid(tag))?;
        let (head, output) = payload.split_at(at);
        let mut ran: Ran = serde_json::from_slice(head)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        ran.output = output.get(1..).unwrap_or_default().to_vec();
        Ok(Some(ran))
    }

    /// # Errors
    ///
    /// The write failed.
    pub fn write_hello(to: &mut impl Write, hello: &Hello) -> io::Result<()> {
        write(
            to,
            HELLO,
            &serde_json::to_vec(hello).map_err(io::Error::other)?,
        )
    }

    /// The hello a stream starts with; `None` when it ended before one.
    ///
    /// # Errors
    ///
    /// The read failed, or the first frame is no hello.
    pub fn read_hello(from: &mut impl Read) -> io::Result<Option<Hello>> {
        read_json(from, HELLO)
    }

    /// # Errors
    ///
    /// The write failed.
    pub fn write_welcome(to: &mut impl Write, welcome: &Welcome) -> io::Result<()> {
        write(
            to,
            WELCOME,
            &serde_json::to_vec(welcome).map_err(io::Error::other)?,
        )
    }

    /// The answer to a hello; `None` when the stream ended before one.
    ///
    /// # Errors
    ///
    /// The read failed, or the frame is no answer to a hello.
    pub fn read_welcome(from: &mut impl Read) -> io::Result<Option<Welcome>> {
        read_json(from, WELCOME)
    }

    /// Tell `to` it is refused for `reason`: the error the refusal is.
    pub fn refuse(to: &mut impl Write, reason: &str) -> io::Error {
        let _ = write_welcome(
            to,
            &Welcome::Refused {
                reason: reason.to_owned(),
            },
        )
        .and_then(|()| to.flush());
        io::Error::new(io::ErrorKind::PermissionDenied, reason.to_owned())
    }

    fn read_json<T: serde::de::DeserializeOwned>(
        from: &mut impl Read,
        tag: u8,
    ) -> io::Result<Option<T>> {
        let Some((read, payload)) = read(from)? else {
            return Ok(None);
        };
        if read != tag {
            return Err(invalid(read));
        }
        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

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
            (EXITED, &[a, b, c, d]) => {
                Ok(Some(FromTerminal::Exited(i32::from_be_bytes([a, b, c, d]))))
            }
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

/// A terminal stream carried over an iroh connection: a host's route on [`ALPN`], and the dial
/// of another host's.
#[cfg(feature = "quic")]
pub mod quic {
    use std::io::{self, Read, Write};
    use std::sync::Arc;
    use std::time::Duration;

    use iroh::endpoint::{Connection, RecvStream, SendStream};
    use iroh::protocol::{AcceptError, ProtocolHandler};
    use iroh::{Endpoint, EndpointAddr};
    use tokio::runtime::Handle;

    /// How long a stream that ended waits for the far end to close the connection, so what
    /// was written last reaches it.
    const CLOSE_PATIENCE: Duration = Duration::from_secs(10);

    /// What a stream reads from the far end, read on a thread of its own.
    pub struct Receiving {
        stream: RecvStream,
        runtime: Handle,
    }

    impl Read for Receiving {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.runtime.block_on(self.stream.read(buffer)) {
                Ok(read) => Ok(read.unwrap_or(0)),
                Err(error) => Err(io::Error::new(io::ErrorKind::ConnectionReset, error)),
            }
        }
    }

    /// What a stream writes to the far end, written from a thread of its own.
    pub struct Sending {
        stream: SendStream,
        runtime: Handle,
    }

    impl Write for Sending {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.runtime
                .block_on(self.stream.write(bytes))
                .map_err(|error| io::Error::new(io::ErrorKind::BrokenPipe, error))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sending {
        /// Say nothing more follows.
        pub fn finish(&mut self) {
            let _ = self.stream.finish();
        }
    }

    type Serve = dyn Fn([u8; 32], Receiving, &mut Sending) + Send + Sync;

    /// A host's terminal streams on [`ALPN`](super::ALPN): each connection's first stream is
    /// handed, with the endpoint that dialled, to what serves it, on a thread of its own.
    #[derive(Clone)]
    pub struct Route(Arc<Serve>);

    impl Route {
        pub fn new(
            serve: impl Fn([u8; 32], Receiving, &mut Sending) + Send + Sync + 'static,
        ) -> Self {
            Self(Arc::new(serve))
        }
    }

    impl std::fmt::Debug for Route {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("terminal streams")
        }
    }

    impl ProtocolHandler for Route {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            let peer = *connection.remote_id().as_bytes();
            let (send, recv) = connection
                .accept_bi()
                .await
                .map_err(AcceptError::from_err)?;
            let runtime = Handle::current();
            let serve = Arc::clone(&self.0);
            let served = tokio::task::spawn_blocking(move || {
                let mut sending = Sending {
                    stream: send,
                    runtime: runtime.clone(),
                };
                serve(
                    peer,
                    Receiving {
                        stream: recv,
                        runtime,
                    },
                    &mut sending,
                );
                sending.finish();
            })
            .await;
            let _ = tokio::time::timeout(CLOSE_PATIENCE, connection.closed()).await;
            served.map_err(AcceptError::from_err)
        }
    }

    /// Dial the terminal streams of the host at `address` from `endpoint`, on `runtime`, from a
    /// thread that is none of its own: the connection, which the stream lasts as long as, and
    /// the stream.
    ///
    /// # Errors
    ///
    /// The host could not be reached.
    pub fn dial(
        endpoint: &Endpoint,
        address: EndpointAddr,
        runtime: &Handle,
    ) -> io::Result<(Connection, Sending, Receiving)> {
        runtime.block_on(async {
            let connection = endpoint
                .connect(address, super::ALPN)
                .await
                .map_err(|error| io::Error::new(io::ErrorKind::ConnectionRefused, error))?;
            let (send, recv) = connection
                .open_bi()
                .await
                .map_err(|error| io::Error::new(io::ErrorKind::ConnectionRefused, error))?;
            Ok((
                connection,
                Sending {
                    stream: send,
                    runtime: runtime.clone(),
                },
                Receiving {
                    stream: recv,
                    runtime: runtime.clone(),
                },
            ))
        })
    }
}

#[cfg(test)]
#[path = "environment_terminals_tests.rs"]
mod tests;

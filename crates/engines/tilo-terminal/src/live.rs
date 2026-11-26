//! A live session: the pty, the grid it prints to, and what its shell marked.
//!
//! A [`Session`] is a process on a pty. What a person sees of it — the emulator's grid, the runs
//! its shell marked, the title it set, the moment it ended — is this device's, and it is kept
//! here so the window that draws the grid and the app's own calls (a command an agent asks for,
//! the screen read back) see the same one. One thread owns the pty and the emulator (neither is
//! shared: the emulator holds the grid's rows and the parser's pointers), and publishes what
//! changed where any thread reads it. Every live session is registered by its id
//! ([`register`]), which is what the app's record names it by and what the window looks up when
//! it builds a `ui.Terminal`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tilo_content::ContentStore;

use crate::TermModes;
use crate::commands::{Commands, FinishedCommand, Marks};
use crate::emulator::{DEFAULT_HISTORY, Emulator};
use crate::graphics::TerminalImagePlacement;
use crate::provider::{OpenError, OpenOptions, Requester};
use crate::screen::{Line, Screen};
use crate::session::{PtyReadDisposition, Session, SessionEnd, classify_pty_read};
use crate::target::SessionTarget;

/// Something that happened to a session: what its shell marked, titled or asked, and how it
/// ended. A reader asks for what came after the count it last saw, so several readers — the
/// window drawing the grid and an app's own call — each see every event once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The program set the window's title (OSC 0/2).
    Title(String),
    /// A run the shell marked with OSC 133 ended.
    Finished(FinishedCommand),
    /// The shell exited.
    Exit(i32),
    /// The session ended without its shell exiting: a lost connection, or a refusal.
    Lost(String),
    /// The bell rang.
    Bell,
    /// The program put text on the clipboard (OSC 52); the bool says it asked for the primary
    /// selection.
    Clipboard(String, bool),
}

/// How many events a session keeps for its readers before the oldest go.
const MAX_EVENTS: usize = 1024;
/// The newest output `readable` answers, in bytes.
const MAX_READ_SCREEN: usize = 64 * 1024;

/// What wakes a window when a session's grid changed.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// What the grid shows, as the threads that do not own the emulator read it: plain rows, so it
/// crosses threads.
#[derive(Clone, Default)]
struct Shown {
    main: Vec<Line>,
    alt: Vec<Line>,
    alternate: bool,
    cursor: (u64, u64),
    links: BTreeMap<u16, String>,
    images: Vec<TerminalImagePlacement>,
}

/// What any thread may read of a session.
struct Shared {
    shown: Mutex<Shown>,
    modes: Mutex<TermModes>,
    revision: AtomicU64,
    events: Mutex<Vec<Event>>,
    dropped: AtomicU64,
    ended: Mutex<Option<SessionEnd>>,
    columns: AtomicU64,
    rows: AtomicU64,
    /// The size the grid thread has yet to apply, when the window resized.
    resized: Mutex<Option<(usize, usize)>>,
    /// The grid thread should drop what it shows and its scrollback before its next read.
    clear: AtomicBool,
    closed: AtomicBool,
}

impl Shared {
    fn rows(&self) -> Vec<Line> {
        let shown = self
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if shown.alternate {
            shown.alt.clone()
        } else {
            shown.main.clone()
        }
    }

    fn publish(&self, shown: Shown, modes: TermModes, revision: u64) {
        *self
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = shown;
        *self
            .modes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = modes;
        self.revision.store(revision, Ordering::Release);
    }

    fn push(&self, event: Event) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if events.len() >= MAX_EVENTS {
            let excess = events.len() + 1 - MAX_EVENTS;
            events.drain(..excess);
            self.dropped.fetch_add(excess as u64, Ordering::AcqRel);
        }
        events.push(event);
        drop(events);
        self.revision.fetch_add(1, Ordering::AcqRel);
    }
}

/// One running session, as this device keeps it.
pub struct Live {
    session: Arc<Session>,
    shared: Arc<Shared>,
    content: ContentStore,
    allow_clipboard: AtomicBool,
    /// What wakes the window drawing this session, when one is: installed by the toolkit's
    /// handle once a view shows the grid.
    wake: Mutex<Option<Wake>>,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Live")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl Live {
    /// Open a session on `target` and start reading it, with `columns` by `rows` cells of grid.
    ///
    /// # Errors
    ///
    /// Why the session could not open; see [`OpenError`].
    pub fn open(
        target: &SessionTarget,
        requested_by: Requester,
        options: &OpenOptions<'_>,
        content: ContentStore,
        columns: u16,
        rows: u16,
        wake: Option<Wake>,
    ) -> Result<Arc<Self>, OpenError> {
        let session = Session::with_options(target, requested_by, options)?;
        Self::of(session, content, columns, rows, wake)
    }

    /// Keep `session` live: start its reader and register it.
    ///
    /// # Errors
    ///
    /// Why the session could not start.
    pub fn of(
        session: Session,
        content: ContentStore,
        columns: u16,
        rows: u16,
        wake: Option<Wake>,
    ) -> Result<Arc<Self>, OpenError> {
        let columns = usize::from(columns.max(1));
        let rows = usize::from(rows.max(1));
        let live = Arc::new(Self {
            session: Arc::new(session),
            shared: Arc::new(Shared {
                shown: Mutex::new(Shown::default()),
                modes: Mutex::new(TermModes::default()),
                revision: AtomicU64::new(1),
                events: Mutex::new(Vec::new()),
                dropped: AtomicU64::new(0),
                ended: Mutex::new(None),
                columns: AtomicU64::new(columns as u64),
                rows: AtomicU64::new(rows as u64),
                resized: Mutex::new(None),
                clear: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
            content,
            allow_clipboard: AtomicBool::new(false),
            wake: Mutex::new(wake),
        });
        register(&live);
        live.start_reader(columns, rows);
        Ok(live)
    }

    /// A live session with no process, for a view or a test that draws a grid of its own.
    #[must_use]
    pub fn without_process(columns: u16, rows: u16) -> Arc<Self> {
        let session = Session::without_process();
        let columns = usize::from(columns.max(1));
        let rows = usize::from(rows.max(1));
        let live = Arc::new(Self {
            session: Arc::new(session),
            shared: Arc::new(Shared {
                shown: Mutex::new(Shown::default()),
                modes: Mutex::new(TermModes::default()),
                revision: AtomicU64::new(1),
                events: Mutex::new(Vec::new()),
                dropped: AtomicU64::new(0),
                ended: Mutex::new(None),
                columns: AtomicU64::new(columns as u64),
                rows: AtomicU64::new(rows as u64),
                resized: Mutex::new(None),
                clear: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
            content: ContentStore::memory(),
            allow_clipboard: AtomicBool::new(false),
            wake: Mutex::new(None),
        });
        register(&live);
        live
    }

    /// A live session whose program runs at a far end this process does not own, as an SSH
    /// channel's shell: `far` carries its bytes and its window's size both ways, and the grid is
    /// this process's.
    ///
    /// # Errors
    ///
    /// Why the session could not be kept.
    pub fn carrying(
        far: crate::session::FarSide,
        content: ContentStore,
        columns: u16,
        rows: u16,
        wake: Option<Wake>,
    ) -> Result<Arc<Self>, OpenError> {
        Self::of(Session::over(far), content, columns, rows, wake)
    }

    /// The session's own id, which the app's record and the window name it by.
    #[must_use]
    pub fn id(&self) -> &str {
        self.session.id()
    }

    /// The pty session.
    #[must_use]
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Whether the program may put text on the clipboard (OSC 52). Off unless the person allowed
    /// it.
    pub fn set_allow_clipboard(&self, allow: bool) {
        self.allow_clipboard.store(allow, Ordering::Relaxed);
    }

    #[must_use]
    pub fn screen(&self) -> Screen {
        let shown = self
            .shared
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let active = if shown.alternate {
            shown.alt
        } else {
            shown.main
        };
        Screen {
            main: active.into_iter().map(std::rc::Rc::new).collect(),
            alt: Vec::new(),
            alternate: shown.alternate,
            cursor: shown.cursor,
            links: shown.links,
            images: shown.images,
        }
    }

    /// The newest rows, for a caller that only reads what is on screen.
    #[must_use]
    pub fn rows(&self) -> Vec<Line> {
        self.shared.rows()
    }

    #[must_use]
    pub fn modes(&self) -> TermModes {
        *self
            .shared
            .modes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn ended(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
            || self
                .shared
                .ended
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
    }

    /// How the session ended, once it did.
    #[must_use]
    pub fn end(&self) -> Option<SessionEnd> {
        self.shared
            .ended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Every event after `since`, and the count to ask from next.
    #[must_use]
    pub fn events(&self, since: u64) -> (Vec<Event>, u64) {
        let first = self.shared.dropped.load(Ordering::Acquire);
        let from = since.saturating_sub(first) as usize;
        let events = {
            let held = self
                .shared
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            held.get(from.min(held.len())..)
                .map_or_else(Vec::new, <[Event]>::to_vec)
        };
        let next = first + events.len() as u64;
        (events, next)
    }

    /// The link cells of the grid name, for a reader of the screen.
    #[must_use]
    pub fn links(&self) -> BTreeMap<u16, String> {
        self.shared
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .links
            .clone()
    }

    /// The images placed on the grid.
    #[must_use]
    pub fn placements(&self) -> Vec<TerminalImagePlacement> {
        self.shared
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .images
            .clone()
    }

    /// The bytes of an image a placement names.
    #[must_use]
    pub fn image(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.read(hash)
    }

    /// What the session shows, as text: its newest output, cut at a bound rather than in the
    /// middle of a character.
    #[must_use]
    pub fn readable(&self) -> (String, bool) {
        let rows = self.shared.rows();
        let mut reversed = String::new();
        let mut rows = rows.iter().rev().peekable();
        while let Some(row) = rows.next() {
            for character in row.text.trim_end().chars().rev() {
                if reversed.len() + character.len_utf8() > MAX_READ_SCREEN {
                    return (reversed.chars().rev().collect(), true);
                }
                reversed.push(character);
            }
            if rows.peek().is_some() {
                if reversed.len() == MAX_READ_SCREEN {
                    return (reversed.chars().rev().collect(), true);
                }
                reversed.push('\n');
            }
        }
        (reversed.chars().rev().collect(), false)
    }

    /// The words a selection covers, each row's trailing blanks cut.
    #[must_use]
    pub fn selection_text(&self, anchor: crate::GridPos, cursor: crate::GridPos) -> Option<String> {
        let selection = crate::Selection { anchor, cursor };
        let columns = self.shared.columns.load(Ordering::Acquire) as usize;
        crate::selection::text(&self.screen(), &selection, columns)
    }

    /// Write `bytes` to the pty.
    ///
    /// # Errors
    ///
    /// Why the write failed; the session is ended then.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        match self.session.write(bytes) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.shared
                    .push(Event::Lost(format!("terminal input failed: {error}")));
                Err(error)
            }
        }
    }

    /// Run one command line, as a person pressing Enter would.
    ///
    /// # Errors
    ///
    /// Why the write failed.
    pub fn run(&self, command: &str) -> std::io::Result<()> {
        self.session.run(command)
    }

    /// Interrupt the foreground program.
    ///
    /// # Errors
    ///
    /// Why the interrupt failed.
    pub fn interrupt(&self) -> std::io::Result<()> {
        self.session.interrupt()
    }

    /// The grid is `columns` by `rows` cells.
    ///
    /// # Errors
    ///
    /// Why the pty refused the size.
    pub fn resize(&self, columns: u16, rows: u16) -> Result<(), String> {
        let (columns, rows) = (columns.max(1), rows.max(1));
        self.shared
            .columns
            .store(u64::from(columns), Ordering::Release);
        self.shared.rows.store(u64::from(rows), Ordering::Release);
        *self
            .shared
            .resized
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((usize::from(columns), usize::from(rows)));
        self.wake();
        self.session.resize(columns, rows)
    }

    /// Clear the screen and scrollback, as `clear` does.
    pub fn clear(&self) {
        self.shared.clear.store(true, Ordering::Release);
        let mut shown = self
            .shared
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shown.main.clear();
        shown.alt.clear();
        drop(shown);
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
        self.wake();
    }

    /// End the session: close its pty, and say that it ended.
    pub fn close(&self) {
        if !self.shared.closed.swap(true, Ordering::AcqRel) {
            self.session.close();
            self.wake();
        }
    }

    /// Tell a program that asked for it that the window's focus came or went.
    pub fn focus(&self, focused: bool) {
        if self.modes().focus_report {
            let _ = self
                .session
                .write(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// Hand the grid's events to every reader and wake whoever draws it.
    pub fn push(&self, event: Event) {
        self.shared.push(event);
        self.wake();
    }

    /// Wake the window that shows this grid, when one is drawing it.
    pub fn set_wake(&self, wake: Wake) {
        *self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
    }

    fn wake(&self) {
        let wake = self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Read the pty for the session's life on a thread of its own, feeding the grid and its
    /// marks.
    fn start_reader(self: &Arc<Self>, columns: usize, rows: usize) {
        let Some(mut reader) = self.session.take_reader() else {
            self.finish(SessionEnd::Exited);
            return;
        };
        let live = Arc::clone(self);
        let name = format!("terminal-{}", self.session.id());
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            let mut emulator = Emulator::with_history(columns, rows, DEFAULT_HISTORY);
            let mut marks = Marks::new(
                live.session
                    .commands_key()
                    .map_or_else(crate::commands::new_key, str::to_owned),
            );
            let mut commands = Commands::default();
            let mut revision = live.revision();
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let resized = live
                    .shared
                    .resized
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some((columns, rows)) = resized {
                    emulator.resize(columns, rows);
                    revision += 1;
                    live.publish(&emulator, revision);
                }
                if live.shared.clear.swap(false, Ordering::AcqRel) {
                    emulator = Emulator::with_history(columns, rows, DEFAULT_HISTORY);
                    commands = Commands::default();
                    revision += 1;
                    live.publish(&emulator, revision);
                }
                let read = reader.read(&mut buffer);
                match classify_pty_read(&read, || live.session.reader_lease().open()) {
                    PtyReadDisposition::Data(read) => {
                        revision = live.feed(
                            &mut emulator,
                            &mut marks,
                            &mut commands,
                            buffer.get(..read).unwrap_or_default(),
                            revision,
                        );
                    }
                    PtyReadDisposition::Retry => {}
                    PtyReadDisposition::Closed => break,
                }
            }
            let end = live.session.reader_lease().end();
            live.finish(end);
        });
        if spawned.is_err() {
            self.finish(SessionEnd::Failed(
                "the reader thread did not start".to_owned(),
            ));
        }
    }

    fn publish(&self, emulator: &Emulator, revision: u64) {
        let screen = emulator.screen();
        let shown = Shown {
            main: screen.main.iter().map(|line| (**line).clone()).collect(),
            alt: screen.alt.iter().map(|line| (**line).clone()).collect(),
            alternate: screen.alternate,
            cursor: screen.cursor,
            links: screen.links.clone(),
            images: screen.images.clone(),
        };
        self.shared.publish(shown, emulator.modes(), revision);
        self.wake();
    }

    /// Feed what the pty printed: the program's queries are answered, the grid is updated, and
    /// what the shell marked becomes finished runs. Runs on the reader thread only.
    fn feed(
        &self,
        emulator: &mut Emulator,
        marks: &mut Marks,
        commands: &mut Commands,
        bytes: &[u8],
        revision: u64,
    ) -> u64 {
        let mut finished = Vec::new();
        for piece in marks.push(bytes) {
            if let Some(run) = commands.follow(&piece, now_ms()) {
                finished.push(run);
            }
        }
        let output = emulator.feed(
            bytes,
            &self.content,
            self.allow_clipboard.load(Ordering::Relaxed),
        );
        let revision = revision + 1;
        self.publish(emulator, revision);
        if !output.responses.is_empty() {
            let _ = self.session.write(&output.responses);
        }
        if let Some(title) = output.title {
            self.shared.push(Event::Title(title));
        }
        for _ in 0..output.bells {
            self.shared.push(Event::Bell);
        }
        for (text, primary) in output.clipboard {
            self.shared.push(Event::Clipboard(text, primary));
        }
        for run in finished {
            self.shared.push(Event::Finished(run));
        }
        self.wake();
        revision
    }

    /// The session's shell ended: say how, once.
    fn finish(&self, end: SessionEnd) {
        {
            let mut ended = self
                .shared
                .ended
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if ended.is_some() {
                return;
            }
            *ended = Some(end.clone());
        }
        self.shared.closed.store(true, Ordering::Release);
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
        match end {
            SessionEnd::Exited => self.shared.push(Event::Exit(0)),
            SessionEnd::Lost(reason) | SessionEnd::Failed(reason) => {
                self.shared.push(Event::Lost(reason));
            }
            SessionEnd::Superseded => self
                .shared
                .push(Event::Lost("another window took this session".to_owned())),
        }
        self.wake();
    }
}

/// The wall clock in milliseconds, as a run's times are kept.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn registry() -> &'static Mutex<BTreeMap<String, Arc<Live>>> {
    static LIVE: OnceLock<Mutex<BTreeMap<String, Arc<Live>>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A counter that rises whenever a session is registered or released: a window rebuilds the set
/// of handles it hands its views when it changed.
#[must_use]
pub fn generation() -> u64 {
    static GENERATION: AtomicU64 = AtomicU64::new(0);
    GENERATION.load(Ordering::Acquire)
}

fn bumped_generation() {
    static GENERATION: AtomicU64 = AtomicU64::new(0);
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn held_registry() -> MutexGuard<'static, BTreeMap<String, Arc<Live>>> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Register `live` under its id, so the window and the app's calls find the one session.
pub fn register(live: &Arc<Live>) {
    held_registry().insert(live.id().to_owned(), Arc::clone(live));
    // The window's `ui.Terminal` reads its sessions from the toolkit, which this crate is not
    // part of: hand it the grid here, once, so the widget draws what a shell prints.
    crate::toolkit::hand_to_toolkit();
    bumped_generation();
}

/// The live session `id` names, when this device holds it.
#[must_use]
pub fn get(id: &str) -> Option<Arc<Live>> {
    held_registry().get(id).cloned()
}

/// Every live session this device holds.
#[must_use]
pub fn attached() -> Vec<(String, Arc<Live>)> {
    held_registry()
        .iter()
        .map(|(id, live)| (id.clone(), Arc::clone(live)))
        .collect()
}

/// Stop holding `id`: a session the app forgot is no longer reachable.
#[must_use]
pub fn release(id: &str) -> Option<Arc<Live>> {
    let released = held_registry().remove(id);
    if released.is_some() {
        bumped_generation();
    }
    released
}

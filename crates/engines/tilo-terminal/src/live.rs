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
use crate::commands::{Commands, FinishedCommand, HistoryAsk, Mark, Marks, Piece};
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
    /// The shell asked for the lines its Up recalls, as `key` names its marks.
    History { key: String, ask: HistoryAsk },
    /// The session's host holds the command at the prompt until someone acts: why, as the
    /// host's refusal says it, and whether an agent asked for it. `None` once it let it go.
    Held(Option<HeldRun>),
}

/// A command the session's host holds, as its refusal says why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldRun {
    /// What happened and what to do, in plain words.
    pub message: String,
    /// The page where a payment method is added, when that would let it through.
    pub add_card: Option<String>,
    /// An agent asked for it: the shell does not wait, and the prompt is the person's.
    pub agent: bool,
}

impl HeldRun {
    fn of(refusal: &serde_json::Value, agent: bool) -> Self {
        let add_card = refusal
            .get("next")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|step| {
                step.get("kind").and_then(serde_json::Value::as_str) == Some("add_payment_method")
            })
            .and_then(|step| step.get("url").and_then(serde_json::Value::as_str))
            .filter(|url| url.starts_with("https://") && !url.chars().any(char::is_whitespace))
            .map(str::to_owned);
        Self {
            message: refusal
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            add_card,
            agent,
        }
    }
}

/// A command line asked of the session, typed at its prompt once the shell shows one.
struct Asked {
    command: String,
    agent: bool,
}

/// What the session knows of its shell's marks, as the threads that type into it read it.
#[derive(Default)]
struct Prompt {
    /// The key the shell's marks carry, once the reader made it.
    key: String,
    /// The shell marked anything at all: it is one that reads Terminal's marks.
    marked: bool,
    /// The shell takes a whole command line in a mark of its own.
    reads_input: bool,
    /// The shell waits at its prompt: nothing runs in front of it.
    ready: bool,
    /// The command lines waiting for the prompt, oldest first.
    waiting: std::collections::VecDeque<Asked>,
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
    /// The shell's prompt and the command lines waiting for it.
    prompt: Mutex<Prompt>,
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
    /// The editor running in the session that offers its text to the platform, when the
    /// supervisor negotiated one: an agent reads and replaces its selection through it rather
    /// than typing at the shell.
    editor: Option<tilo_process::editor::Client>,
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
        let editor = session
            .take_editor()
            .and_then(|socket| tilo_process::editor::Client::new(socket, Arc::new(|| {})).ok());
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
                prompt: Mutex::new(Prompt::default()),
            }),
            content,
            allow_clipboard: AtomicBool::new(false),
            wake: Mutex::new(wake),
            editor,
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
                prompt: Mutex::new(Prompt::default()),
            }),
            content: ContentStore::memory(),
            allow_clipboard: AtomicBool::new(false),
            wake: Mutex::new(None),
            editor: None,
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

    /// The editor running in this session that offers its text, when one does and its lease
    /// is current: the lease to replace its selection with, and whether it can undo or redo.
    #[must_use]
    pub fn editor(&self) -> Option<tilo_process::editor::Snapshot> {
        self.editor.as_ref()?.snapshot()
    }

    /// Replace the selection of the editor running in this session with `text`, under the
    /// `lease` its snapshot gave, and wait for the editor to say it applied it.
    ///
    /// # Errors
    ///
    /// No editor offers its text here, its lease is no longer current, or it did not
    /// acknowledge the edit: the text is never typed at the shell instead.
    pub fn replace_editor_selection(&self, lease: &str, text: String) -> Result<(), String> {
        use tilo_process::editor::{Failure, Request, Status, TEXT_BYTES};
        if text.len() > TEXT_BYTES || lease.len() > 512 {
            return Err("The text is longer than the editor takes at once".into());
        }
        let editor = self
            .editor
            .as_ref()
            .filter(|editor| editor.snapshot().is_some())
            .ok_or("No editor in this session offers its text")?;
        let lease =
            serde_json::from_str(lease).map_err(|_| "The editor's lease is not one it gave")?;
        let (answered, answer) = std::sync::mpsc::sync_channel(1);
        editor.submit(
            editor.epoch(),
            Request::Replace {
                lease,
                transaction: 1,
                text,
            },
            Status::Applied,
            Box::new(EditorAnswer(answered)),
        );
        match answer.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(Failure::Unknown)) | Err(_) => {
                Err("The editor did not acknowledge this edit".into())
            }
            Ok(Err(_)) => Err("The editor's lease is no longer current".into()),
        }
    }

    /// Whether the program may put text on the clipboard (OSC 52). Off unless the person allowed
    /// it.
    pub fn set_allow_clipboard(&self, allow: bool) {
        self.allow_clipboard.store(allow, Ordering::Relaxed);
    }

    /// Whether the program may put text on the clipboard now.
    #[must_use]
    pub fn allows_clipboard(&self) -> bool {
        self.allow_clipboard.load(Ordering::Relaxed)
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
        self.run_as(command, false)
    }

    /// Run one command line at the shell's prompt, `agent` saying an agent asked: a shell that
    /// marks its commands takes it once it shows its prompt, in place of what was half typed
    /// there, and one that reads Terminal's marks knows who asked. A shell that marks nothing
    /// is typed into now.
    ///
    /// # Errors
    ///
    /// Why the write failed.
    pub fn run_as(&self, command: &str, agent: bool) -> std::io::Result<()> {
        let mut prompt = self
            .shared
            .prompt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !prompt.marked {
            drop(prompt);
            return self.write(&typed_line(command));
        }
        prompt.waiting.push_back(Asked {
            command: command.to_owned(),
            agent,
        });
        self.type_waiting(prompt)
    }

    /// Type the oldest waiting command line when the shell is at its prompt: it takes the prompt
    /// itself, so the line is written before another thread's mark of a new prompt may type the
    /// one queued behind it.
    fn type_waiting(&self, mut prompt: std::sync::MutexGuard<'_, Prompt>) -> std::io::Result<()> {
        if !prompt.ready {
            return Ok(());
        }
        let Some(asked) = prompt.waiting.pop_front() else {
            return Ok(());
        };
        let line = if prompt.reads_input {
            use base64::Engine as _;
            format!(
                "\x1b]133;R;k={};cmd={}{}\x07",
                prompt.key,
                base64::engine::general_purpose::STANDARD.encode(&asked.command),
                if asked.agent { ";agent=1" } else { "" }
            )
            .into_bytes()
        } else {
            typed_line(&asked.command)
        };
        prompt.ready = false;
        self.write(&line)
    }

    /// Answer the shell's ask for the lines its Up recalls: `lines` are the history's that
    /// match, newest first; each is answered once, past the `skip` it already showed, at most
    /// `limit` of them and as many as fit one answer.
    ///
    /// # Errors
    ///
    /// Why the write failed.
    pub fn answer_history(
        &self,
        key: &str,
        ask: &HistoryAsk,
        lines: &[String],
    ) -> std::io::Result<()> {
        use base64::Engine as _;
        const ANSWER_BYTES: usize = 3072;
        let b64 = base64::engine::general_purpose::STANDARD;
        let limit = ask.limit.clamp(1, 256);
        let mut seen = std::collections::HashSet::new();
        let mut distinct = lines
            .iter()
            .filter(|line| seen.insert(line.as_str()))
            .skip(ask.skip);
        let mut entries = Vec::new();
        let mut bytes = 0;
        let mut end = true;
        for line in distinct.by_ref() {
            let encoded = b64.encode(line);
            if !entries.is_empty()
                && (entries.len() >= limit || bytes + encoded.len() + 1 > ANSWER_BYTES)
            {
                end = false;
                break;
            }
            bytes += encoded.len() + 1;
            entries.push(encoded);
        }
        let answer = format!(
            "\x1b]133;H;k={key};s={};q={};e={}{}\x07",
            ask.skip,
            b64.encode(&ask.matching),
            entries.join(","),
            if end { ";end=1" } else { "" }
        );
        self.write(answer.as_bytes())
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
        let mut told = Vec::new();
        {
            let mut prompt = self
                .shared
                .prompt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for piece in marks.push(bytes) {
                if let Piece::Mark(mark) = &piece {
                    prompt.marked = true;
                    match mark {
                        Mark::Prompt => prompt.ready = true,
                        Mark::Started { .. } => prompt.ready = false,
                        Mark::History(ask) => told.push(Event::History {
                            key: marks.key().to_owned(),
                            ask: ask.clone(),
                        }),
                        Mark::Held { agent, refusal } => {
                            told.push(Event::Held(Some(HeldRun::of(refusal, *agent))));
                        }
                        Mark::Released => told.push(Event::Held(None)),
                        _ => {}
                    }
                }
                if let Some(run) = commands.follow(&piece, now_ms()) {
                    finished.push(run);
                }
            }
            marks.key().clone_into(&mut prompt.key);
            prompt.reads_input = marks.reads_input();
            if let Err(error) = self.type_waiting(prompt) {
                tracing::warn!(%error, "a command line asked of the terminal was not typed");
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
        for event in told {
            self.shared.push(event);
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

/// What the editor answered an edit with, for the caller waiting on it.
struct EditorAnswer(std::sync::mpsc::SyncSender<Result<(), tilo_process::editor::Failure>>);

impl tilo_process::editor::Completion for EditorAnswer {
    fn send(
        &self,
        send: &mut dyn FnMut() -> std::io::Result<()>,
    ) -> Result<(), tilo_process::editor::Failure> {
        send().map_err(|_| tilo_process::editor::Failure::Unknown)
    }

    fn finish(self: Box<Self>, result: Result<(), tilo_process::editor::Failure>) {
        let _ = self.0.send(result);
    }
}

/// The wall clock in milliseconds, as a run's times are kept.
/// A command line typed at a prompt that reads no marks: Ctrl-U first, so what was half typed
/// does not become part of it, then the line and Enter.
fn typed_line(command: &str) -> Vec<u8> {
    let mut line = vec![0x15];
    line.extend_from_slice(command.as_bytes());
    line.push(b'\r');
    line
}

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

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_session_with_no_process_keeps_its_grid_and_reports_what_it_was_told;

        phase events_are_read_once_per_reader_and_the_grid_is_published_whole;
        {
            let live = Live::without_process(20, 5);
            assert_eq!(live.revision(), 1);
            let (events, cursor) = live.events(0);
            assert!(events.is_empty());
            live.push(Event::Title("build".to_owned()));
            live.push(Event::Bell);
            let (events, next) = live.events(cursor);
            assert_eq!(events, vec![Event::Title("build".to_owned()), Event::Bell]);
            let (again, _) = live.events(next);
            assert!(again.is_empty(), "a reader sees each event once");
            let (all, _) = live.events(0);
            assert_eq!(all.len(), 2, "a second reader sees them from the start");
        }

        phase clearing_empties_the_grid_and_ending_says_so_once;
        {
            let live = Live::without_process(20, 5);
            live.clear();
            assert_eq!(live.rows().len(), 0);
            live.close();
            assert!(live.ended());
            let end = live.end();
            assert_eq!(end, None, "closing alone reports no shell exit");
        }
    }

    /// A far end built from channels: the bytes it was sent, the output it feeds, and the size
    /// and close it heard.
    #[derive(Clone, Default)]
    struct Far {
        input: Arc<Mutex<Vec<u8>>>,
        output: Arc<Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>>,
        sizes: Arc<Mutex<Vec<(u16, u16)>>>,
        closed: Arc<std::sync::atomic::AtomicBool>,
    }

    struct Output(std::sync::mpsc::Receiver<Vec<u8>>);

    impl std::io::Read for Output {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Ok(bytes) = self.0.recv() else {
                return Ok(0);
            };
            let taken = bytes.len().min(buffer.len());
            buffer[..taken].copy_from_slice(&bytes[..taken]);
            Ok(taken)
        }
    }

    struct Input(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Input {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn far() -> (crate::session::FarSide, Far) {
        let (output, received) = std::sync::mpsc::channel();
        let far = Far::default();
        *far.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(output);
        let sizes = Arc::clone(&far.sizes);
        let closed = Arc::clone(&far.closed);
        (
            crate::session::FarSide {
                writer: Box::new(Input(Arc::clone(&far.input))),
                reader: Box::new(Output(received)),
                resize: Box::new(move |columns, rows| {
                    sizes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((columns, rows));
                }),
                close: Box::new(move || closed.store(true, Ordering::Release)),
                shell_name: "ssh".to_owned(),
                ended: Arc::default(),
                relay_session: None,
                commands: None,
            },
            far,
        )
    }

    tilo_test_env::scenario! {
        fn a_session_over_a_far_side_prints_types_and_resizes_both_ways;

        phase what_the_far_side_prints_reaches_the_grid;
        {
            let (far_side, far) = far();
            let live = Live::carrying(
                far_side,
                tilo_content::ContentStore::memory(),
                80,
                24,
                None,
            )
            .unwrap();
            let sender = far
                .output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("the far side writes once");
            sender.send(b"tilo-remote ready\r\n".to_vec()).unwrap();
            tilo_test_env::wait_until("the banner in the grid", || {
                live.rows()
                    .iter()
                    .any(|line| line.text.contains("tilo-remote ready"))
            });

            live.write(b"ls\r").unwrap();
            assert_eq!(
                far.input
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_slice(),
                b"ls\r",
                "the grid's keys reach the far side"
            );

            live.resize(100, 30).unwrap();
            assert_eq!(
                *far.sizes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                vec![(100, 30)],
                "the window's size reaches the far side"
            );

            live.session().report_end(crate::session::SessionEnd::Lost(
                "the connection dropped".to_owned(),
            ));
            live.close();
            assert!(far.closed.load(Ordering::Acquire), "closing lets the far side go");
        }

        phase a_lost_connection_is_reported_as_lost_not_as_an_exit;
        {
            let (far_side, far) = far();
            let live = Live::carrying(
                far_side,
                tilo_content::ContentStore::memory(),
                80,
                24,
                None,
            )
            .unwrap();
            live.session().report_end(crate::session::SessionEnd::Lost(
                "the machine hung up".to_owned(),
            ));
            drop(
                far.output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take(),
            );
            tilo_test_env::wait_until("the session to end", || live.ended());
            let (events, _) = live.events(0);
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    Event::Lost(reason) if reason == "the machine hung up"
                )),
                "{events:?}"
            );
        }
    }
}

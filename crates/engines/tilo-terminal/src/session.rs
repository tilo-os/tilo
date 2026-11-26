//! One terminal session: the pty its shell runs on, the grid the shell paints, and the verbs a
//! view or an agent calls on it.
//!
//! The session owns the process. A view takes the pty's reader once, feeds what the shell printed
//! into its own [`crate::Emulator`] and paints that grid; an agent calls [`Session::run`] or
//! [`Session::interrupt`]. What the process printed never becomes a record; only the runs its
//! shell marked with OSC 133 do, and those are read by [`crate::commands`] from the same stream.

use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use portable_pty::{CommandBuilder, MasterPty, PtySize};

use crate::provider::{NullMaster, open_with, route, spawn_pty};
use crate::provider::{OpenError, OpenOptions, Requester};
use crate::target::SessionTarget;

/// How a session ended, once its terminal closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEnd {
    /// The shell exited: its window closes.
    Exited,
    /// The connection to a remote shell was lost: the tab reconnects.
    Lost(String),
    /// Another terminal took the session over.
    Superseded,
    /// The environment could not be set up.
    Failed(String),
}

impl SessionEnd {
    /// What the supervisor reported, when it reported anything.
    #[must_use]
    pub fn from_report(line: &[u8]) -> Self {
        #[derive(serde::Deserialize)]
        #[serde(tag = "ended", rename_all = "kebab-case")]
        enum Report {
            Exited,
            Lost {
                #[serde(default)]
                reason: String,
            },
            Superseded,
            Failed {
                #[serde(default)]
                reason: String,
            },
        }
        match serde_json::from_slice::<Report>(line) {
            Ok(Report::Lost { reason }) => Self::Lost(reason),
            Ok(Report::Superseded) => Self::Superseded,
            Ok(Report::Failed { reason }) => Self::Failed(reason),
            Ok(Report::Exited) | Err(_) => Self::Exited,
        }
    }
}

/// The lease the supervisor closes when the session ends: it tells a hosted start-up `EIO` from
/// an exit, and says how the session ended.
pub struct ReaderLease {
    lease: Option<runtime_portal::pending_connection::ReaderLifetime>,
    report: std::cell::RefCell<Option<Vec<u8>>>,
    /// How the far side said the session ended, when it said so before its stream closed.
    reported: Option<SessionEnd>,
}

impl ReaderLease {
    #[must_use]
    pub fn new(lease: Option<runtime_portal::pending_connection::ReaderLifetime>) -> Self {
        Self {
            lease,
            report: std::cell::RefCell::new(None),
            reported: None,
        }
    }

    /// A lease whose session already told how it ends.
    #[must_use]
    pub fn telling(
        lease: Option<runtime_portal::pending_connection::ReaderLifetime>,
        reported: Option<SessionEnd>,
    ) -> Self {
        Self {
            lease,
            report: std::cell::RefCell::new(None),
            reported,
        }
    }

    /// Whether the supervisor still holds the session open; a report of
    /// how it ended is kept, never taken for an open session.
    pub fn open(&self) -> bool {
        if let Some(lease) = &self.lease {
            use runtime_portal::pending_connection::LeaseState;
            return match lease.state() {
                LeaseState::Open => true,
                LeaseState::Closed => false,
                LeaseState::Reported(line) => {
                    self.report.replace(Some(line));
                    false
                }
            };
        }
        false
    }

    /// How the session ended, once its terminal closed: what the far side said, else what the
    /// supervisor reported, else (a local shell) the shell exited.
    #[must_use]
    pub fn end(&self) -> SessionEnd {
        if let Some(reported) = &self.reported {
            return reported.clone();
        }
        if self.report.borrow().is_none() {
            let _ = self.open();
        }
        self.report
            .borrow()
            .as_deref()
            .map_or(SessionEnd::Exited, SessionEnd::from_report)
    }
}

/// What one read from the pty means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtyReadDisposition {
    /// `n` bytes arrived.
    Data(usize),
    /// Nothing yet: read again.
    Retry,
    /// The session's terminal is closed.
    Closed,
}

/// A PTY master with no slave open: `EIO` on Linux, end of file on macOS. A hosted session sees it
/// in its hand-off window, so only a closed `session_open` makes it final. `EINTR` is never
/// closure.
#[must_use]
pub fn classify_pty_read(
    result: &std::io::Result<usize>,
    session_open: impl FnOnce() -> bool,
) -> PtyReadDisposition {
    match result {
        Ok(n) if *n > 0 => return PtyReadDisposition::Data(*n),
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            return PtyReadDisposition::Retry;
        }
        _ => {}
    }
    if tilo_process::editor::read_finds_no_slave(result) && session_open() {
        PtyReadDisposition::Retry
    } else {
        PtyReadDisposition::Closed
    }
}

/// The pty a supervisor opened and the facts of the session behind it.
pub(crate) struct Backend {
    pub(crate) writer: Box<dyn Write + Send>,
    pub(crate) reader: Box<dyn Read + Send>,
    pub(crate) master: Box<dyn MasterPty + Send>,
    pub(crate) shell_name: String,
    pub(crate) relay_session: Option<String>,
    pub(crate) supervisor_session: Option<String>,
    pub(crate) commands: Option<String>,
    pub(crate) editor: Option<tilo_process::editor::Socket>,
    pub(crate) lease: Option<runtime_portal::pending_connection::SessionLifetime>,
}

/// The far end of a session this process does not run: the bytes its input goes to, the bytes
/// its output comes from, and how its window's size and its end reach it. What a connection
/// carries a session over — an SSH channel, a supervisor's hand-off — is this, and the engine
/// that runs it keeps the connection.
pub struct FarSide {
    /// Input goes here: the bytes typed at the grid.
    pub writer: Box<dyn Write + Send>,
    /// Output comes from here, until it ends.
    pub reader: Box<dyn Read + Send>,
    /// The window was resized: told to the far side, in cells.
    pub resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    /// The session is being closed: the far side is let go.
    pub close: Box<dyn Fn() + Send + Sync>,
    /// The program at the far prompt, when its name is known.
    pub shell_name: String,
}

/// One open terminal session.
pub struct Session {
    pub(crate) id: String,
    pub(crate) writer: Mutex<Box<dyn Write + Send>>,
    pub(crate) reader: Mutex<Option<Box<dyn Read + Send>>>,
    pub(crate) master: Mutex<Box<dyn MasterPty + Send>>,
    pub(crate) shell_name: String,
    /// How the far side said the session ended, before its stream closed: a connection lost,
    /// never a shell that exited.
    reported_end: Mutex<Option<SessionEnd>>,
    /// What closing the session tells the far side.
    on_close: Option<Box<dyn Fn() + Send + Sync>>,
    /// The session on another device this one relays, when it was carried.
    pub(crate) relayed: Option<String>,
    /// The supervisor's own id for this session, when its host opened it.
    pub(crate) supervisor: Option<String>,
    /// The key this session's shell marks its runs with, when it marks them.
    pub(crate) commands: Option<String>,
    pub(crate) editor: Mutex<Option<tilo_process::editor::Socket>>,
    pub(crate) lease: Mutex<Option<runtime_portal::pending_connection::SessionLifetime>>,
    closed: AtomicBool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("shell_name", &self.shell_name)
            .field("relay_session", &self.relayed)
            .field("supervisor_session", &self.supervisor)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Open a session on `target`: the supervisor when the device has one, a guest this device
    /// builds itself when it can, the device's own login shell when that is the target.
    ///
    /// # Errors
    ///
    /// Why the session could not open; see [`OpenError`].
    pub fn open(target: &SessionTarget, requested_by: Requester) -> Result<Self, OpenError> {
        Self::with_options(target, requested_by, &OpenOptions::default())
    }

    /// [`Session::open`], with what the caller knows about the session it asks for: the
    /// launcher's program, a relayed session, the workspace and record the run belongs to, the
    /// supervisor connection the launcher handed this process, and the flag a supervisor raises
    /// when the workspace's host is asked for the session.
    ///
    /// # Errors
    ///
    /// Why the session could not open; see [`OpenError`].
    pub fn with_options(
        target: &SessionTarget,
        requested_by: Requester,
        options: &OpenOptions<'_>,
    ) -> Result<Self, OpenError> {
        let program = options
            .program
            .map_or_else(crate::provider::launch_command_from_env, <[String]>::to_vec);
        let chosen = route(target, options.has_service());
        let resolved = OpenOptions {
            program: Some(&program),
            ..*options
        };
        open_with(&chosen, target, requested_by, &resolved)
    }

    /// A session on a pty this process opened itself.
    ///
    /// # Errors
    ///
    /// Why the pty could not be opened.
    pub fn with_command(cmd: CommandBuilder) -> Result<Self, Box<dyn std::error::Error>> {
        let (writer, reader, master, shell_name) = spawn_pty(cmd)?;
        Ok(Self::from_parts(writer, reader, master, shell_name))
    }

    /// A session with no process behind it, for a view that has not opened one yet.
    #[must_use]
    pub fn without_process() -> Self {
        Self::from_parts(
            Box::new(std::io::sink()),
            Box::new(std::io::empty()),
            Box::new(NullMaster),
            String::new(),
        )
    }

    /// A session whose program runs at a far end this process does not own, as an SSH channel's
    /// shell or a supervisor's hand-off: `far` carries its bytes and its size both ways.
    #[must_use]
    pub fn over(far: FarSide) -> Self {
        let FarSide {
            writer,
            reader,
            resize,
            close,
            shell_name,
        } = far;
        let mut session = Self::from_parts(
            writer,
            reader,
            Box::new(RemoteMaster { resize }),
            shell_name,
        );
        session.on_close = Some(close);
        session
    }

    /// The far side says how the session ended: a connection lost, or the program gone. Read by
    /// the reader when its stream closes, so a lost connection is never reported as an exit.
    pub fn report_end(&self, end: SessionEnd) {
        *self
            .reported_end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(end);
    }

    /// A session over a pty someone else opened and handed over, as a supervisor does.
    pub(crate) fn from_backend(backend: Backend) -> Self {
        let Backend {
            writer,
            reader,
            master,
            shell_name,
            relay_session,
            supervisor_session,
            commands,
            editor,
            lease,
        } = backend;
        Self {
            id: crate::commands::new_key(),
            writer: Mutex::new(writer),
            reader: Mutex::new(Some(reader)),
            master: Mutex::new(master),
            shell_name,
            relayed: relay_session,
            supervisor: supervisor_session,
            commands,
            editor: Mutex::new(editor),
            lease: Mutex::new(lease),
            reported_end: Mutex::new(None),
            on_close: None,
            closed: AtomicBool::new(false),
        }
    }

    fn from_parts(
        writer: Box<dyn Write + Send>,
        reader: Box<dyn Read + Send>,
        master: Box<dyn MasterPty + Send>,
        shell_name: String,
    ) -> Self {
        Self {
            id: crate::commands::new_key(),
            writer: Mutex::new(writer),
            reader: Mutex::new(Some(reader)),
            master: Mutex::new(master),
            shell_name,
            relayed: None,
            supervisor: None,
            commands: None,
            editor: Mutex::new(None),
            lease: Mutex::new(None),
            reported_end: Mutex::new(None),
            on_close: None,
            closed: AtomicBool::new(false),
        }
    }

    /// The session's own name, which its app and its runs refer to it by.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name of the program at the session's prompt.
    #[must_use]
    pub fn shell_name(&self) -> &str {
        &self.shell_name
    }

    /// The session on the workspace's host this one relays, when it was carried.
    #[must_use]
    pub fn relay_session(&self) -> Option<&str> {
        self.relayed.as_deref()
    }

    /// The supervisor's id for this session, when its host opened it.
    #[must_use]
    pub fn supervisor_session(&self) -> Option<&str> {
        self.supervisor.as_deref()
    }

    /// The key the session's shell marks its runs with, when it marks them.
    #[must_use]
    pub fn commands_key(&self) -> Option<&str> {
        self.commands.as_deref()
    }

    /// Take the pty's writer, once, for a view that keeps one.
    #[must_use]
    pub fn take_writer(&self) -> Option<Box<dyn Write + Send>> {
        self.writer
            .lock()
            .ok()
            .map(|mut w| std::mem::replace(&mut *w, Box::new(std::io::sink())))
    }

    /// Take the pty's master, once, for a view that resizes it.
    #[must_use]
    pub fn take_master(&self) -> Option<Box<dyn MasterPty + Send>> {
        self.master
            .lock()
            .ok()
            .map(|mut m| std::mem::replace(&mut *m, Box::new(NullMaster)))
    }

    /// Take the pty's reader, once: whoever takes it pumps what the shell prints.
    #[must_use]
    pub fn take_reader(&self) -> Option<Box<dyn Read + Send>> {
        self.reader.lock().ok().and_then(|mut reader| reader.take())
    }

    /// Take the platform editor capability, when the supervisor negotiated one.
    #[must_use]
    pub fn take_editor(&self) -> Option<tilo_process::editor::Socket> {
        self.editor.lock().ok().and_then(|mut editor| editor.take())
    }

    /// The session's lease, for a view that keeps it beside its reader.
    #[must_use]
    pub fn take_lifetime(&self) -> Option<runtime_portal::pending_connection::SessionLifetime> {
        self.lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// The lease the supervisor closes when the session ends.
    #[must_use]
    pub fn reader_lease(&self) -> ReaderLease {
        let reported = self
            .reported_end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        ReaderLease::telling(
            self.lease
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(|socket| socket.clone_for_reader().ok()),
            reported,
        )
    }

    /// Write `bytes` to the shell: a key, a paste, a whole command line.
    ///
    /// # Errors
    ///
    /// The shell is gone.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        writer.write_all(bytes)?;
        writer.flush()
    }

    /// Send `command` to the shell as one line, as typing it and pressing Enter does.
    ///
    /// # Errors
    ///
    /// The shell is gone.
    pub fn run(&self, command: &str) -> std::io::Result<()> {
        let mut line = command.as_bytes().to_vec();
        line.push(b'\r');
        self.write(&line)
    }

    /// Interrupt the foreground program, as Ctrl-C does.
    ///
    /// # Errors
    ///
    /// The shell is gone.
    pub fn interrupt(&self) -> std::io::Result<()> {
        self.write(&[0x03])
    }

    /// Tell the shell its window is `cols` × `rows`.
    ///
    /// # Errors
    ///
    /// The pty refused the new size.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let master = self
            .master
            .lock()
            .map_err(|_| "the terminal's pty is gone".to_string())?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| error.to_string())
    }

    /// Close the session's terminal; its process ends with it.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flush()
            .ok();
        if let Some(close) = &self.on_close {
            close();
        }
    }
}

/// The master of a session that runs at a far end: a resize is the far side's to hear, and the
/// pty this process might open is none.
struct RemoteMaster {
    resize: Box<dyn Fn(u16, u16) + Send + Sync>,
}

impl MasterPty for RemoteMaster {
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize::default())
    }

    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        (self.resize)(size.cols, size.rows);
        Ok(())
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(std::io::sink()))
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<i32> {
        None
    }

    #[cfg(unix)]
    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session writes what its view sends, marks an interrupt, and closes once.
    #[test]
    fn a_session_writes_its_input_and_closes_once() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let path = dir.path().join("feed");
        let writer = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(&path)
            .expect("a scratch file");
        let reader = std::fs::File::open(&path).expect("a reader");
        let session = Session::from_parts(
            Box::new(writer),
            Box::new(reader),
            Box::new(crate::provider::NullMaster),
            "sh".into(),
        );
        session
            .run("echo hello")
            .expect("the line reaches the shell");
        session.interrupt().expect("ctrl-c reaches the shell");
        session.close();
        session.close();

        let mut written = String::new();
        std::io::Read::read_to_string(
            &mut std::fs::File::open(&path).expect("the feed"),
            &mut written,
        )
        .expect("the feed reads back");
        assert_eq!(written, "echo hello\r\u{3}");
        assert_eq!(
            session.commands_key(),
            None,
            "a shell that marks nothing says so"
        );
        assert_eq!(session.reader_lease().end(), SessionEnd::Exited);
    }

    /// The supervisor's report says how a session ended: only an exit closes its window.
    #[test]
    fn a_sessions_end_is_read_from_its_report() {
        assert_eq!(
            SessionEnd::from_report(br#"{"ended":"exited"}"#),
            SessionEnd::Exited
        );
        assert_eq!(
            SessionEnd::from_report(br#"{"ended":"lost","reason":"connection reset"}"#),
            SessionEnd::Lost("connection reset".into())
        );
        assert_eq!(
            SessionEnd::from_report(br#"{"ended":"superseded"}"#),
            SessionEnd::Superseded
        );
        assert_eq!(
            SessionEnd::from_report(br#"{"ended":"failed","reason":"no image"}"#),
            SessionEnd::Failed("no image".into())
        );
        assert_eq!(SessionEnd::from_report(b"not json"), SessionEnd::Exited);
    }

    /// A pty read that finds no slave while the session is open is a hand-off, not an exit.
    #[test]
    fn a_pty_read_that_finds_no_slave_is_final_only_when_the_session_closed() {
        let closed: std::io::Result<usize> = Err(std::io::Error::from(std::io::ErrorKind::Other));
        assert_eq!(
            classify_pty_read(&Ok(4), || true),
            PtyReadDisposition::Data(4)
        );
        assert_eq!(
            classify_pty_read(
                &Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                || true
            ),
            PtyReadDisposition::Retry
        );
        assert_eq!(
            classify_pty_read(&Ok(0), || true),
            PtyReadDisposition::Closed,
            "end of file closes the session"
        );
        let _ = closed;
    }
}

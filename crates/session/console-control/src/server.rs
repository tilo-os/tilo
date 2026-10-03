//! Endpoints and the request queue.
//!
//! Listener threads never touch app state. Each request line is queued with
//! a reply channel, the app is woken (on the live medium: a winit
//! `UserEvent` through the event-loop proxy), and the app's UI thread drains
//! the queue with [`ConsoleServer::take_pending`], answers each request, and
//! the listener writes the answer back. The app therefore handles a console
//! request exactly where it handles a click.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{
    DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, PermissionsExt as _,
};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::wire;

/// The virtio-serial port name a hypervisor attaches for the console
/// channel. The device node exists only when a VM was started with a
/// `virtserialport` of exactly this name; physical machines have no
/// virtio-serial bus at all.
pub const VIRTIO_PORT: &str = "/dev/virtio-ports/org.tilo.console.0";

/// How long a listener waits for the UI thread to answer one request.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Concurrent Unix-socket connections served at once. The console has one
/// human and at most a few tools; a peer that opens more waits its turn.
const MAX_CONNECTIONS: usize = 8;

/// Wakes the owning app after a request was queued.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// One place a console app listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A Unix socket in the app user's private runtime directory.
    Socket(PathBuf),
    /// A virtio-serial port character device (one stream, reopened as the
    /// host end connects and disconnects).
    Port(PathBuf),
}

impl Endpoint {
    /// The endpoints this app should serve on this machine: the private
    /// runtime socket when `XDG_RUNTIME_DIR` is an absolute path, and the
    /// virtio port when the hypervisor attached one.
    pub fn discover(app: &str) -> Vec<Self> {
        let mut endpoints = Vec::new();
        if let Some(runtime) = xdg_paths::xdg::runtime_dir() {
            endpoints.push(Self::Socket(socket_path(&runtime, app)));
        }
        if std::fs::metadata(VIRTIO_PORT).is_ok_and(|meta| meta.file_type().is_char_device()) {
            endpoints.push(Self::Port(PathBuf::from(VIRTIO_PORT)));
        }
        endpoints
    }
}

/// `<runtime>/tilo-console/<app>.sock` — the path `tilo-control --socket`
/// takes to reach a console app from its own machine.
pub fn socket_path(runtime: &Path, app: &str) -> PathBuf {
    runtime.join("tilo-console").join(format!("{app}.sock"))
}

/// One queued request. Answer it with [`Pending::respond`]; dropping it
/// unanswered makes the listener report a timeout-style error.
pub struct Pending {
    line: String,
    reply: mpsc::Sender<String>,
}

impl Pending {
    pub fn line(&self) -> &str {
        &self.line
    }

    pub fn respond(self, reply: String) {
        let _ = self.reply.send(reply);
    }
}

/// The request queue shared by every endpoint of one app.
#[derive(Clone, Default)]
pub struct ConsoleServer {
    queue: Arc<Mutex<VecDeque<Pending>>>,
}

impl ConsoleServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `line` as if it had arrived on an endpoint and return where the
    /// answer will appear. Used by the listeners and by in-process drivers
    /// (headless tests) that have no socket.
    pub fn submit(&self, line: impl Into<String>) -> mpsc::Receiver<String> {
        let (reply, answer) = mpsc::channel();
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(Pending {
                line: line.into(),
                reply,
            });
        }
        answer
    }

    /// Everything queued since the last call, oldest first.
    pub fn take_pending(&self) -> Vec<Pending> {
        self.queue
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default()
    }

    /// Serve every endpoint, returning the ones that could not start with
    /// their reason. One failing endpoint does not stop the others.
    pub fn serve_all(&self, endpoints: &[Endpoint], waker: &Waker) -> Vec<(Endpoint, io::Error)> {
        endpoints
            .iter()
            .filter_map(|endpoint| {
                self.serve(endpoint, Arc::clone(waker))
                    .err()
                    .map(|error| (endpoint.clone(), error))
            })
            .collect()
    }

    /// Start serving one endpoint on background threads.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket directory is not private to this
    /// user, a non-socket occupies the socket path, or binding fails.
    pub fn serve(&self, endpoint: &Endpoint, waker: Waker) -> io::Result<()> {
        match endpoint {
            Endpoint::Socket(path) => self.serve_socket(path, waker),
            Endpoint::Port(path) => {
                let server = self.clone();
                let path = path.clone();
                std::thread::Builder::new()
                    .name("console-port".into())
                    .spawn(move || server.serve_port(&path, &waker))
                    .map(|_| ())
            }
        }
    }

    fn serve_socket(&self, path: &Path, waker: Waker) -> io::Result<()> {
        let listener = bind_private(path)?;
        let server = self.clone();
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("console-socket".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        active.fetch_sub(1, Ordering::SeqCst);
                        let mut stream = stream;
                        let _ = writeln!(
                            stream,
                            "{}",
                            wire::error("busy", "too many console connections; retry shortly")
                        );
                        continue;
                    }
                    let server = server.clone();
                    let waker = Arc::clone(&waker);
                    let finished = Arc::clone(&active);
                    let spawned = std::thread::Builder::new()
                        .name("console-connection".into())
                        .spawn(move || {
                            if let Ok(reader) = stream.try_clone() {
                                let _ = server.serve_stream(reader, stream, &waker);
                            }
                            finished.fetch_sub(1, Ordering::SeqCst);
                        });
                    if spawned.is_err() {
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })
            .map(|_| ())
    }

    /// Serve a virtio port for the life of the process. The host end comes
    /// and goes: a read of zero bytes means nobody is connected right now,
    /// so the thread waits and reads again instead of treating it as the
    /// end of the channel.
    fn serve_port(&self, path: &Path, waker: &Waker) {
        loop {
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path);
            match opened.and_then(|file| file.try_clone().map(|reader| (reader, file))) {
                Ok((reader, writer)) => {
                    let _ = self.serve_lines(reader, writer, waker, true);
                }
                Err(_) => std::thread::sleep(Duration::from_secs(1)),
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Answer every request line on one connection until it closes.
    ///
    /// # Errors
    ///
    /// Returns the I/O error that ended the connection, if any.
    pub fn serve_stream(
        &self,
        reader: impl Read,
        writer: impl Write,
        waker: &Waker,
    ) -> io::Result<()> {
        self.serve_lines(reader, writer, waker, false)
    }

    /// `persistent` is the virtio port: its end of file only means the host
    /// end is not connected at the moment, and an oversized line is refused
    /// without giving up the one channel there is.
    fn serve_lines(
        &self,
        reader: impl Read,
        mut writer: impl Write,
        waker: &Waker,
        persistent: bool,
    ) -> io::Result<()> {
        let mut reader = BufReader::new(reader);
        loop {
            match read_request(&mut reader)? {
                Line::Request(line) => {
                    let reply = self.exchange(line, waker);
                    writeln!(writer, "{reply}")?;
                    writer.flush()?;
                }
                Line::TooLong => {
                    writeln!(
                        writer,
                        "{}",
                        wire::error("parse_error", "request line too long")
                    )?;
                    writer.flush()?;
                    if !persistent {
                        return Ok(());
                    }
                    reader.skip_until(b'\n')?;
                }
                Line::Eof if persistent => std::thread::sleep(Duration::from_millis(250)),
                Line::Eof => return Ok(()),
            }
        }
    }

    fn exchange(&self, line: String, waker: &Waker) -> String {
        let seq = wire::request_seq(&line);
        let answer = self.submit(line);
        waker();
        let reply = answer.recv_timeout(REPLY_TIMEOUT).unwrap_or_else(|_| {
            wire::error(
                "timeout",
                "the application did not answer; it may be busy or shutting down",
            )
        });
        wire::with_seq(reply, seq)
    }
}

enum Line {
    Request(String),
    TooLong,
    Eof,
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Line> {
    let mut buffer = Vec::new();
    let limit = u64::try_from(wire::MAX_LINE_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
    loop {
        buffer.clear();
        let read = reader.by_ref().take(limit).read_until(b'\n', &mut buffer)?;
        if read == 0 {
            return Ok(Line::Eof);
        }
        if buffer.last() != Some(&b'\n') && buffer.len() > wire::MAX_LINE_BYTES {
            return Ok(Line::TooLong);
        }
        let text = String::from_utf8_lossy(&buffer);
        let text = text.trim();
        if !text.is_empty() {
            return Ok(Line::Request(text.to_owned()));
        }
    }
}

/// Bind `path` so only this user can reach it: its directory is created
/// 0700, or must already be a directory this user owns that nobody else can
/// enter; a stale socket of ours is replaced; anything else is refused.
fn bind_private(path: &Path) -> io::Result<UnixListener> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "socket path has no directory")
    })?;
    let me = rustix::process::geteuid().as_raw();
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.file_type().is_dir() || meta.uid() != me || meta.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{} must be a directory owned by this user and closed to everyone else",
                        dir.display()
                    ),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        Err(error) => return Err(error),
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() && meta.uid() == me => {
            std::fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not this user's socket", path.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn noop() -> Waker {
        Arc::new(|| {})
    }

    /// Answer queued requests on this thread, as the UI thread would.
    fn answer_until(server: &ConsoleServer, count: usize) -> Vec<String> {
        let mut seen = Vec::new();
        let deadline = tilo_test_env::hang_deadline();
        while seen.len() < count && std::time::Instant::now() < deadline {
            for pending in server.take_pending() {
                seen.push(pending.line().to_owned());
                let reply = wire::answer(&serde_json::json!({ "echo": pending.line() }));
                pending.respond(reply);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        seen
    }

    #[test]
    fn console_server_answers_only_through_its_owner_and_never_hangs() {
        // Phase `loopback_submission_is_answered_by_the_owner`.
        {
            let server = ConsoleServer::new();
            let answer = server.submit(r#"{"op":"state_dump"}"#);
            let pending = server.take_pending();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].line(), r#"{"op":"state_dump"}"#);
            for p in pending {
                p.respond("ok".into());
            }
            assert_eq!(answer.recv().unwrap(), "ok");
            assert!(server.take_pending().is_empty());
        }
        // Phase `a_private_socket_round_trips_lines_through_the_owner`.
        {
            let runtime = tempfile::tempdir().unwrap();
            std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let path = socket_path(runtime.path(), "installer");
            let server = ConsoleServer::new();
            server
                .serve(&Endpoint::Socket(path.clone()), noop())
                .unwrap();

            let dir_mode = std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777;
            let sock_mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
            assert_eq!((dir_mode, sock_mode), (0o700, 0o600));

            let client = std::thread::spawn(move || {
                let mut stream = UnixStream::connect(&path).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut replies = Vec::new();
                for request in [r#"{"op":"state_dump"}"#, r#"{"op":"actions_enumerate"}"#] {
                    writeln!(stream, "{request}").unwrap();
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    replies.push(line);
                }
                replies
            });
            let seen = answer_until(&server, 2);
            let replies = client.join().unwrap();
            assert_eq!(seen.len(), 2);
            assert!(replies[0].contains("state_dump"), "{replies:?}");
            assert!(replies[1].contains("actions_enumerate"), "{replies:?}");
        }
        // Phase `a_shared_or_foreign_socket_directory_is_refused`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let dir = runtime.path().join("tilo-console");
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let error = ConsoleServer::new()
                .serve(&Endpoint::Socket(dir.join("installer.sock")), noop())
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(dir.join("installer.sock"), b"not a socket").unwrap();
            let error = ConsoleServer::new()
                .serve(&Endpoint::Socket(dir.join("installer.sock")), noop())
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        }
        // Phase `a_stream_answers_each_line_and_stops_at_oversized_input`.
        {
            let server = ConsoleServer::new();
            let (mut client, host) = UnixStream::pair().unwrap();
            let serving = {
                let server = server.clone();
                std::thread::spawn(move || {
                    let reader = host.try_clone().unwrap();
                    server.serve_stream(reader, host, &noop())
                })
            };
            writeln!(client, "{{\"op\":\"state_dump\"}}\n").unwrap();
            let seen = answer_until(&server, 1);
            assert_eq!(seen, [r#"{"op":"state_dump"}"#]);
            let mut reader = BufReader::new(client.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("echo"));

            client
                .write_all(&vec![b'x'; wire::MAX_LINE_BYTES + 10])
                .unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("too long"), "{line}");
            serving.join().unwrap().unwrap();
        }
        // Phase `an_unanswered_request_times_out_rather_than_hanging_forever`.
        {
            // Dropping the pending entry closes its reply channel immediately,
            // which the listener reports instead of waiting out the timeout.
            let server = ConsoleServer::new();
            let answer = server.submit("{}");
            drop(server.take_pending());
            assert!(answer.recv().is_err());
        }
    }
}

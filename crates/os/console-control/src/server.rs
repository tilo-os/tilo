//! Endpoints, and the queue between them and the app.
//!
//! Each endpoint reads one line at a time: a frame of the action wire
//! ([`remote::RemoteActionFrame`]), answered through [`tilo_action_line::answer_line`] with this
//! server as the [`ActionProvider`], or `{"op":"state_dump"}`, answered `{"state": ..}`: the
//! framing a session's control socket uses. Listener threads never touch app state. What a
//! line asks is queued as one [`Asked`], the app is woken (on the live medium: a winit
//! `UserEvent` through the event-loop proxy), and the app's UI thread drains the queue with
//! [`ConsoleServer::take_pending`] and answers each with [`Asked::answer`]. The app therefore
//! handles a console request exactly where it handles a click.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{
    DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, PermissionsExt as _,
};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::api::{self, ActionAddress};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest,
    MAX_REMOTE_ACTION_FRAME_BYTES, MAX_REMOTE_WAIT, Pending, ReceiptStatus, pending,
};
use tilo_actions::{ActionFailure, ActionOutput, EncodedAction, InvocationId, ResourceSpec};

/// The virtio-serial port name a hypervisor attaches for the console
/// channel. The device node exists only when a VM was started with a
/// `virtserialport` of exactly this name; physical machines have no
/// virtio-serial bus at all.
pub const VIRTIO_PORT: &str = "/dev/virtio-ports/org.tilo.console.0";

/// How long a listener waits for the UI thread to answer one request.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest request line accepted: one frame. A peer that streams more is refused rather than
/// buffered without bound.
pub const MAX_LINE_BYTES: usize = MAX_REMOTE_ACTION_FRAME_BYTES;

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
        if let Some(runtime) = tilo_paths::xdg::runtime_dir() {
            endpoints.push(Self::Socket(socket_path(&runtime, app)));
        }
        if std::fs::metadata(VIRTIO_PORT).is_ok_and(|meta| meta.file_type().is_char_device()) {
            endpoints.push(Self::Port(PathBuf::from(VIRTIO_PORT)));
        }
        endpoints
    }
}

/// `<runtime>/tilo-console/<app>.sock`: where a program on the app's own machine reaches it.
pub fn socket_path(runtime: &Path, app: &str) -> PathBuf {
    runtime.join("tilo-console").join(format!("{app}.sock"))
}

/// One request waiting for the app's UI thread. Deliberately not `Debug`: invocation arguments
/// can carry secrets (passphrases).
pub enum Asked {
    /// A page of the actions the app publishes.
    Catalogue {
        query: Box<CatalogueQuery>,
        answer: Completer<Result<CataloguePage, ActionCallError>>,
    },
    /// One invocation, answered once it ran.
    Invoke {
        request: Box<InvokeRequest>,
        answer: Completer<Result<InvocationReceipt, ActionCallError>>,
    },
    /// What the app shows, as data.
    State { answer: Completer<Value> },
}

/// What a console app answers its console with.
pub trait ConsoleApp {
    /// The provider callers address its actions at.
    fn provider(&self) -> &'static str;
    /// The resources the app publishes now, with their actions.
    fn published(&self) -> Vec<ResourceSpec>;
    /// Run one admitted action: what it answers, or why it failed.
    ///
    /// # Errors
    ///
    /// The app's own failure, said as the receipt says it.
    fn run(
        &mut self,
        address: &ActionAddress,
        encoded: EncodedAction,
    ) -> Result<Value, Box<ActionFailure>>;
    /// What the app shows, as data: what `state_dump` answers.
    fn state(&self) -> Value;
}

impl Asked {
    /// Answer this request from `app`, on the app's own thread.
    pub fn answer(self, app: &mut impl ConsoleApp) {
        match self {
            Self::Catalogue { query, answer } => answer.complete(Ok(CataloguePage::published(
                app.provider(),
                &app.published(),
                &query,
            ))),
            Self::Invoke { request, answer } => {
                let invocation = request.invocation;
                let status = match admit(app, &request) {
                    Err(error) => ReceiptStatus::Rejected { error },
                    Ok(encoded) => match app.run(&request.target.address, encoded) {
                        Ok(value) => ReceiptStatus::Completed {
                            output: ActionOutput {
                                value: Some(value),
                                resources: Vec::new(),
                            },
                        },
                        Err(error) => ReceiptStatus::Failed { error: *error },
                    },
                };
                answer.complete(Ok(InvocationReceipt { invocation, status }));
            }
            Self::State { answer } => answer.complete(app.state()),
        }
    }
}

/// `request` admitted against what `app` publishes now ([`api::admit`]).
fn admit(app: &impl ConsoleApp, request: &InvokeRequest) -> Result<EncodedAction, ActionCallError> {
    admit_to(app.provider(), &app.published(), request)
}

/// `request` admitted against `published`, the resources `provider` publishes now
/// ([`api::admit`]): what an app that answers [`Asked::Invoke`] itself, later than at once,
/// checks before it runs the action.
///
/// # Errors
///
/// Why the request does not admit: another provider's, an action nobody publishes, arguments
/// that are not a JSON object or do not fit.
pub fn admit_to(
    provider: &str,
    published: &[ResourceSpec],
    request: &InvokeRequest,
) -> Result<EncodedAction, ActionCallError> {
    if request.target.provider != provider {
        return Err(ActionCallError::UnknownAction);
    }
    // No store keeps asked records here: an approve a frame carries is no one's.
    tilo_actions::remote::asked::admit_frame(None, request)?;
    let arguments = match &request.argument {
        InvokeArgument::None => Map::new(),
        InvokeArgument::Json(arguments) => arguments.clone(),
        InvokeArgument::Text(_) => {
            return Err(ActionCallError::InvalidRequest {
                reason: "a console action takes its arguments as a JSON object".into(),
            });
        }
    };
    api::admit(
        published,
        &request.target.address,
        &arguments,
        request.confirmation.as_ref(),
    )
    .map_err(|error| ActionCallError::from_api(&error))
}

/// The request queue shared by every endpoint of one app, and the provider its frames are
/// answered by.
#[derive(Clone)]
pub struct ConsoleServer {
    app: &'static str,
    queue: Arc<Mutex<VecDeque<Asked>>>,
    waker: Arc<OnceLock<Waker>>,
}

impl ConsoleServer {
    /// The console of the app `app` names: its provider, and its socket's name.
    #[must_use]
    pub fn new(app: &'static str) -> Self {
        Self {
            app,
            queue: Arc::default(),
            waker: Arc::default(),
        }
    }

    /// Everything queued since the last call, oldest first.
    #[must_use]
    pub fn take_pending(&self) -> Vec<Asked> {
        self.queue
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default()
    }

    /// Queue `asked` and wake the app.
    fn ask(&self, asked: Asked) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(asked);
        }
        if let Some(wake) = self.waker.get() {
            wake();
        }
    }

    /// The line that answers request `line`, once the app answered it.
    #[must_use]
    pub fn answer(&self, line: &str) -> String {
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                return tilo_action_line::line_refusal("parse_error", &error.to_string());
            }
        };
        match request.get("op").and_then(Value::as_str) {
            None if request.get("version").is_some() => tilo_action_line::answer_line(
                self,
                &Caller::local_app(self.app),
                line.as_bytes(),
                REPLY_TIMEOUT.saturating_sub(MAX_REMOTE_WAIT),
            ),
            Some("state_dump") => {
                let (answer, state) = pending();
                self.ask(Asked::State { answer });
                let Ok(Some(state)) = state.wait_timeout(REPLY_TIMEOUT) else {
                    return tilo_action_line::line_refusal(
                        "timeout",
                        "the application did not answer; it may be busy or shutting down",
                    );
                };
                // A long-lived client on the virtio channel names its request, so an answer to
                // one it gave up on is recognised and skipped.
                let mut reply = Map::new();
                reply.insert("state".into(), state);
                if let Some(seq) = request
                    .get("seq")
                    .filter(|seq| seq.is_string() || seq.is_number())
                {
                    reply.insert("seq".into(), seq.clone());
                }
                Value::Object(reply).to_string()
            }
            _ => tilo_action_line::line_refusal(
                "not_implemented",
                "a console app answers action frames and state_dump only: no synthetic input, \
                 no snapshots",
            ),
        }
    }

    /// [`Self::answer`] with `app` answering on this thread: how a test drives an app through
    /// its console, as an endpoint does, without a socket.
    pub fn exchange(&self, app: &mut impl ConsoleApp, line: &str) -> String {
        std::thread::scope(|scope| {
            let asking = scope.spawn(|| self.answer(line));
            loop {
                let finished = asking.is_finished();
                for asked in self.take_pending() {
                    asked.answer(app);
                }
                if finished {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            asking.join().unwrap_or_else(|_| {
                tilo_action_line::line_refusal("unavailable", "the console stopped")
            })
        })
    }

    /// Serve every endpoint [`Endpoint::discover`] finds for this app, waking the app with
    /// `waker` for each request, logging which listen and which could not start. One failing
    /// endpoint does not stop the others.
    pub fn serve_discovered(&self, waker: Waker) {
        self.set_waker(waker);
        let app = self.app;
        for endpoint in Endpoint::discover(app) {
            match self.serve(&endpoint) {
                Ok(()) => tracing::info!(app, ?endpoint, "console listening"),
                Err(error) => {
                    tracing::warn!(app, ?endpoint, %error, "console endpoint unavailable");
                }
            }
        }
    }

    /// Wake the app with `waker` for each request from now on; the first waker set stays.
    pub fn set_waker(&self, waker: Waker) {
        let _ = self.waker.set(waker);
    }

    /// Start serving one endpoint on background threads.
    pub fn serve(&self, endpoint: &Endpoint) -> io::Result<()> {
        match endpoint {
            Endpoint::Socket(path) => self.serve_socket(path),
            Endpoint::Port(path) => {
                let server = self.clone();
                let path = path.clone();
                std::thread::Builder::new()
                    .name("console-port".into())
                    .spawn(move || server.serve_port(&path))
                    .map(|_| ())
            }
        }
    }

    fn serve_socket(&self, path: &Path) -> io::Result<()> {
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
                            tilo_action_line::line_refusal(
                                "busy",
                                "too many console connections; retry shortly"
                            )
                        );
                        continue;
                    }
                    let server = server.clone();
                    let finished = Arc::clone(&active);
                    let spawned = std::thread::Builder::new()
                        .name("console-connection".into())
                        .spawn(move || {
                            if let Ok(reader) = stream.try_clone() {
                                let _ = server.serve_stream(reader, stream);
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
    fn serve_port(&self, path: &Path) {
        loop {
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path);
            match opened.and_then(|file| file.try_clone().map(|reader| (reader, file))) {
                Ok((reader, writer)) => {
                    let _ = self.serve_lines(reader, writer, true);
                }
                Err(_) => std::thread::sleep(Duration::from_secs(1)),
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Answer every request line on one connection until it closes.
    pub fn serve_stream(&self, reader: impl Read, writer: impl Write) -> io::Result<()> {
        self.serve_lines(reader, writer, false)
    }

    /// `persistent` is the virtio port: its end of file only means the host
    /// end is not connected at the moment, and an oversized line is refused
    /// without giving up the one channel there is.
    fn serve_lines(
        &self,
        reader: impl Read,
        mut writer: impl Write,
        persistent: bool,
    ) -> io::Result<()> {
        let mut reader = BufReader::new(reader);
        loop {
            match read_request(&mut reader)? {
                Line::Request(line) => {
                    let reply = self.answer(&line);
                    writeln!(writer, "{reply}")?;
                    writer.flush()?;
                }
                Line::TooLong => {
                    writeln!(
                        writer,
                        "{}",
                        tilo_action_line::line_refusal("parse_error", "request line too long")
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
}

/// The app's actions as a provider: each request waits in the queue for the app's UI thread,
/// which answers it once it ran. Nothing outlives its answer, so there is nothing to observe or
/// cancel afterwards.
impl ActionProvider for ConsoleServer {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let (answer, page) = pending();
        self.ask(Asked::Catalogue {
            query: Box::new(query),
            answer,
        });
        page
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (answer, receipt) = pending();
        self.ask(Asked::Invoke {
            request: Box::new(request),
            answer,
        });
        receipt
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

enum Line {
    Request(String),
    TooLong,
    Eof,
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Line> {
    let mut buffer = Vec::new();
    let limit = u64::try_from(MAX_LINE_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
    loop {
        buffer.clear();
        let read = reader.by_ref().take(limit).read_until(b'\n', &mut buffer)?;
        if read == 0 {
            return Ok(Line::Eof);
        }
        if buffer.last() != Some(&b'\n') && buffer.len() > MAX_LINE_BYTES {
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
    let listener = tilo_files::native::socket::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::net::UnixStream;
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage, RemoteActionTarget};
    use tilo_actions::{ActionSpec, OperationKind};

    const OP: &str = "test.console";

    /// An app with one action, `next`, that counts how often it ran.
    struct Counter(u32);

    impl ConsoleApp for Counter {
        fn provider(&self) -> &'static str {
            "counter"
        }

        fn published(&self) -> Vec<ResourceSpec> {
            vec![
                ResourceSpec::new("counter", "test.counter", "Counter").with_action(
                    ActionSpec::operation("next", "Next", OperationKind::Effect, OP, &"next")
                        .unwrap(),
                ),
            ]
        }

        fn run(
            &mut self,
            address: &ActionAddress,
            encoded: EncodedAction,
        ) -> Result<Value, Box<ActionFailure>> {
            if (address.action.as_str(), encoded.type_id.as_str()) != ("next", OP) {
                return Err(Box::new(ActionFailure {
                    code: "unexpected".into(),
                    message: "not the counter's action".into(),
                    details: None,
                    retryable: false,
                }));
            }
            self.0 = self.0.saturating_add(1);
            Ok(json!({ "count": self.0 }))
        }

        fn state(&self) -> Value {
            json!({ "app": "counter", "count": self.0 })
        }
    }

    fn next_frame() -> String {
        tilo_action_line::frame_line(RemoteActionMessage::Invoke(InvokeRequest::plain(
            RemoteActionTarget {
                provider: "counter".into(),
                address: ActionAddress::new("counter", "next"),
            },
            Map::new(),
        )))
    }

    fn receipt(line: &str) -> ReceiptStatus {
        match RemoteActionFrame::decode(line.trim().as_bytes()).map(|frame| frame.message) {
            Ok(RemoteActionMessage::Receipt(receipt)) => receipt.status,
            other => panic!("not a receipt: {other:?} from {line}"),
        }
    }

    /// Answer queued requests on this thread, as the UI thread would, until `count` were.
    fn answer_until(server: &ConsoleServer, app: &mut Counter, count: usize) {
        let mut seen = 0;
        let deadline = tilo_test_env::hang_deadline();
        while seen < count && std::time::Instant::now() < deadline {
            for asked in server.take_pending() {
                asked.answer(app);
                seen = seen.saturating_add(1);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn console_server_answers_frames_through_its_owner_and_never_hangs() {
        {
            let server = ConsoleServer::new("counter");
            let mut app = Counter(0);
            let state: Value =
                serde_json::from_str(&server.exchange(&mut app, r#"{"op":"state_dump","seq":7}"#))
                    .unwrap();
            assert_eq!(
                (state["state"]["app"].clone(), state["seq"].clone()),
                (json!("counter"), json!(7))
            );
            let ran = receipt(&server.exchange(&mut app, &next_frame()));
            assert!(matches!(ran, ReceiptStatus::Completed { .. }), "{ran:?}");
            assert_eq!(app.0, 1);
            let refused: Value =
                serde_json::from_str(&server.exchange(&mut app, r#"{"op":"type","text":"x"}"#))
                    .unwrap();
            assert_eq!(refused["err"]["code"], "not_implemented");
            let other =
                tilo_action_line::frame_line(RemoteActionMessage::Invoke(InvokeRequest::plain(
                    RemoteActionTarget {
                        provider: "installer".into(),
                        address: ActionAddress::new("counter", "next"),
                    },
                    Map::new(),
                )));
            assert!(matches!(
                receipt(&server.exchange(&mut app, &other)),
                ReceiptStatus::Rejected {
                    error: ActionCallError::UnknownAction
                }
            ));
            assert_eq!(app.0, 1);
        }
        {
            let runtime = tempfile::tempdir().unwrap();
            std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let path = socket_path(runtime.path(), "counter");
            let server = ConsoleServer::new("counter");
            server.serve(&Endpoint::Socket(path.clone())).unwrap();

            let dir_mode = std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777;
            let sock_mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
            assert_eq!((dir_mode, sock_mode), (0o700, 0o600));

            let client = std::thread::spawn(move || {
                let mut stream = UnixStream::connect(&path).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut replies = Vec::new();
                for request in [r#"{"op":"state_dump"}"#.to_owned(), next_frame()] {
                    writeln!(stream, "{request}").unwrap();
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    replies.push(line);
                }
                replies
            });
            let mut app = Counter(0);
            answer_until(&server, &mut app, 2);
            let replies = client.join().unwrap();
            assert!(replies[0].contains("\"state\""), "{replies:?}");
            assert!(matches!(
                receipt(&replies[1]),
                ReceiptStatus::Completed { .. }
            ));
        }
    }

    #[test]
    fn a_console_endpoint_refuses_unsafe_paths_long_lines_and_dropped_requests() {
        {
            let runtime = tempfile::tempdir().unwrap();
            let dir = runtime.path().join("tilo-console");
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let error = ConsoleServer::new("installer")
                .serve(&Endpoint::Socket(dir.join("installer.sock")))
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(dir.join("installer.sock"), b"not a socket").unwrap();
            let error = ConsoleServer::new("installer")
                .serve(&Endpoint::Socket(dir.join("installer.sock")))
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        }
        {
            let server = ConsoleServer::new("counter");
            let (mut client, host) = UnixStream::pair().unwrap();
            let serving = {
                let server = server.clone();
                std::thread::spawn(move || {
                    let reader = host.try_clone().unwrap();
                    server.serve_stream(reader, host)
                })
            };
            writeln!(client, "{{\"op\":\"state_dump\"}}\n").unwrap();
            let mut app = Counter(0);
            answer_until(&server, &mut app, 1);
            let mut reader = BufReader::new(client.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("counter"));

            client.write_all(&vec![b'x'; MAX_LINE_BYTES + 10]).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("too long"), "{line}");
            serving.join().unwrap().unwrap();
        }
        {
            // A request the app drops unanswered is refused at once, never waited out.
            let server = ConsoleServer::new("counter");
            let asking = {
                let server = server.clone();
                std::thread::spawn(move || server.answer(r#"{"op":"state_dump"}"#))
            };
            let deadline = tilo_test_env::hang_deadline();
            let mut dropped = 0;
            while dropped == 0 && std::time::Instant::now() < deadline {
                dropped = server.take_pending().len();
                std::thread::sleep(Duration::from_millis(5));
            }
            let refused: Value = serde_json::from_str(&asking.join().unwrap()).unwrap();
            assert_eq!(refused["err"]["code"], "timeout");
        }
    }
}

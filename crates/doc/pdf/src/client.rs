//! Client side of the sandboxed helper: spawning, request/response,
//! crash detection and transparent restart.
//!
//! Every open document keeps its own read-only descriptor, so after a helper
//! dies the next request simply starts a new helper and re-opens the
//! documents it needs. The request that was in flight when the helper died
//! fails with [`Error::Crashed`]; a document that keeps crashing the engine
//! is refused rather than restarted forever.

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::edits::Edits;
use crate::error::Error;
use crate::model::{AnnotationInfo, DocumentInfo, FormField, Link, OutlineEntry, RenderOptions};
use crate::proto::{Channel, OpenKind, PROTOCOL_VERSION, ProbeReport, Request, Response};
use crate::text::RawChar;
use crate::{Bitmap, shm};

/// How long a helper may take to start and confine itself.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one ordinary request may run before the helper is presumed hung.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Exports copy and rewrite whole documents.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);
/// A document that has taken the helper down this many times is refused.
const MAX_CRASHES_PER_DOCUMENT: u32 = 3;

struct Connection {
    child: Child,
    channel: Channel,
    generation: u64,
}

impl Connection {
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct DocEntry {
    fd: OwnedFd,
    kind: OpenKind,
    password: Option<String>,
    opened_in: Option<u64>,
    crashes: u32,
}

#[derive(Default)]
struct State {
    connection: Option<Connection>,
    docs: HashMap<u64, DocEntry>,
    generation: u64,
    helpers_started: u64,
}

/// The sandboxed backend.
pub struct Sandboxed {
    helper: PathBuf,
    state: Mutex<State>,
}

enum Payload {
    None,
    Fd(OwnedFd),
}

impl Sandboxed {
    /// A backend that runs `helper`. The first helper starts eagerly so a
    /// missing library or an unavailable sandbox is reported here.
    ///
    /// # Errors
    ///
    /// When the helper cannot start or confine itself.
    pub fn start(helper: &Path) -> Result<Self, Error> {
        let this = Self {
            helper: helper.to_path_buf(),
            state: Mutex::new(State::default()),
        };
        {
            let mut state = this.lock();
            let connection = this.spawn(&mut state)?;
            state.connection = Some(connection);
        }
        Ok(this)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn spawn(&self, state: &mut State) -> Result<Connection, Error> {
        let (ours, theirs) = UnixStream::pair().map_err(Error::Io)?;
        let mut command = Command::new(&self.helper);
        command
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .env_clear();
        for key in ["TILO_PDFIUM_LIB", "HOME", "LANG"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let child = command
            .spawn()
            .map_err(|e| Error::Unavailable(format!("starting {}: {e}", self.helper.display())))?;
        ours.set_read_timeout(Some(START_TIMEOUT))
            .map_err(Error::Io)?;
        let mut connection = Connection {
            child,
            channel: Channel::new(ours),
            generation: state.generation.saturating_add(1),
        };
        match connection.channel.recv::<Response>() {
            Ok(Response::Ready { version }) if version == PROTOCOL_VERSION => {}
            Ok(Response::Failed { message, .. }) => {
                connection.kill();
                return Err(Error::Unavailable(format!(
                    "the PDF helper could not start: {message}"
                )));
            }
            Ok(_) => {
                connection.kill();
                return Err(Error::Unavailable(
                    "the PDF helper speaks another protocol".into(),
                ));
            }
            Err(e) => {
                connection.kill();
                return Err(Error::Unavailable(format!(
                    "the PDF helper did not start: {e}"
                )));
            }
        }
        state.generation = connection.generation;
        state.helpers_started = state.helpers_started.saturating_add(1);
        Ok(connection)
    }

    /// Register a document; it is opened in the helper lazily.
    ///
    /// # Errors
    ///
    /// Opening failures reported by the engine.
    pub fn open(
        &self,
        id: u64,
        fd: OwnedFd,
        kind: OpenKind,
        password: Option<String>,
    ) -> Result<DocumentInfo, Error> {
        let mut state = self.lock();
        state.docs.insert(
            id,
            DocEntry {
                fd,
                kind,
                password,
                opened_in: None,
                crashes: 0,
            },
        );
        let result = self.ensure_open(&mut state, id);
        if result.is_err() {
            state.docs.remove(&id);
        }
        drop(state);
        result
            .and_then(|info| info.ok_or_else(|| Error::Engine("document was already open".into())))
    }

    /// Make sure a live helper has document `id` open. Returns the info when
    /// this call opened it.
    fn ensure_open(&self, state: &mut State, id: u64) -> Result<Option<DocumentInfo>, Error> {
        if state.connection.is_none() {
            let connection = self.spawn(state)?;
            state.connection = Some(connection);
        }
        let generation = state.generation;
        let entry = state
            .docs
            .get(&id)
            .ok_or_else(|| Error::Invalid("the document is closed".into()))?;
        if entry.opened_in == Some(generation) {
            return Ok(None);
        }
        if entry.crashes >= MAX_CRASHES_PER_DOCUMENT {
            return Err(Error::Crashed(
                "this document repeatedly crashed the PDF engine".into(),
            ));
        }
        let request = Request::Open {
            doc: id,
            kind: entry.kind,
            password: entry.password.clone(),
        };
        let fd = entry.fd.try_clone().map_err(Error::Io)?;
        let response = Self::exchange(state, &[id], &request, Some(&fd), REQUEST_TIMEOUT)?;
        match response {
            (Response::Opened(info), _) => {
                if let Some(entry) = state.docs.get_mut(&id) {
                    entry.opened_in = Some(generation);
                }
                Ok(Some(info))
            }
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// Send one request (after its optional descriptor) and read the reply.
    /// Any transport failure is treated as a helper crash.
    fn exchange(
        state: &mut State,
        docs: &[u64],
        request: &Request,
        fd: Option<&OwnedFd>,
        timeout: Duration,
    ) -> Result<(Response, Payload), Error> {
        let Some(connection) = state.connection.as_mut() else {
            return Err(Error::Crashed("the PDF helper is not running".into()));
        };
        let outcome = (|| -> std::io::Result<(Response, Payload)> {
            connection
                .channel
                .stream()
                .set_read_timeout(Some(timeout))?;
            connection.channel.send(request)?;
            if let Some(fd) = fd {
                connection.channel.send_fd(fd.as_fd())?;
            }
            let response: Response = connection.channel.recv()?;
            let payload = match &response {
                Response::Rendered { .. } | Response::Exported { .. } => {
                    Payload::Fd(connection.channel.recv_fd()?)
                }
                _ => Payload::None,
            };
            Ok((response, payload))
        })();
        match outcome {
            Ok((Response::Failed { kind, message }, _)) => Err(Error::from_failure(kind, message)),
            Ok(ok) => Ok(ok),
            Err(e) => {
                let timed_out = matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                );
                if let Some(connection) = state.connection.take() {
                    connection.kill();
                }
                for id in docs {
                    if let Some(entry) = state.docs.get_mut(id) {
                        entry.crashes = entry.crashes.saturating_add(1);
                    }
                }
                Err(if timed_out {
                    Error::Timeout
                } else {
                    Error::Crashed(format!("the PDF helper stopped: {e}"))
                })
            }
        }
    }

    fn call(
        &self,
        docs: &[u64],
        request: &Request,
        timeout: Duration,
    ) -> Result<(Response, Payload), Error> {
        let mut state = self.lock();
        for id in docs {
            self.ensure_open(&mut state, *id)?;
        }
        Self::exchange(&mut state, docs, request, None, timeout)
    }

    pub fn close(&self, id: u64) {
        let mut state = self.lock();
        let was_open = state
            .docs
            .remove(&id)
            .and_then(|e| e.opened_in)
            .is_some_and(|g| g == state.generation);
        if was_open && state.connection.is_some() {
            let _ = Self::exchange(
                &mut state,
                &[],
                &Request::Close { doc: id },
                None,
                REQUEST_TIMEOUT,
            );
        }
        drop(state);
    }

    pub fn render(&self, id: u64, page: usize, options: &RenderOptions) -> Result<Bitmap, Error> {
        let request = Request::Render {
            doc: id,
            page: u32::try_from(page).map_err(|_| Error::Invalid("page out of range".into()))?,
            options: *options,
        };
        match self.call(&[id], &request, REQUEST_TIMEOUT)? {
            (Response::Rendered { width, height }, Payload::Fd(fd)) => {
                let len = u64::from(width)
                    .saturating_mul(u64::from(height))
                    .saturating_mul(4);
                let map = shm::map_sealed(&fd, len).map_err(Error::Io)?;
                let len =
                    usize::try_from(len).map_err(|_| Error::Limit("render too large".into()))?;
                Ok(Bitmap::from_shared(
                    width,
                    height,
                    shm::SharedBytes::new(map, len),
                ))
            }
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub fn text(&self, id: u64, page: usize) -> Result<Vec<RawChar>, Error> {
        let page = u32::try_from(page).map_err(|_| Error::Invalid("page out of range".into()))?;
        match self.call(&[id], &Request::Text { doc: id, page }, REQUEST_TIMEOUT)? {
            (Response::Text(chars), _) => Ok(chars),
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub fn links(&self, id: u64, page: usize) -> Result<Vec<Link>, Error> {
        let page = u32::try_from(page).map_err(|_| Error::Invalid("page out of range".into()))?;
        match self.call(&[id], &Request::Links { doc: id, page }, REQUEST_TIMEOUT)? {
            (Response::Links(links), _) => Ok(links),
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub fn annotations(&self, id: u64, page: usize) -> Result<Vec<AnnotationInfo>, Error> {
        let page = u32::try_from(page).map_err(|_| Error::Invalid("page out of range".into()))?;
        match self.call(
            &[id],
            &Request::Annotations { doc: id, page },
            REQUEST_TIMEOUT,
        )? {
            (Response::Annotations(a), _) => Ok(a),
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub fn form_fields(&self, id: u64) -> Result<Vec<FormField>, Error> {
        match self.call(&[id], &Request::FormFields { doc: id }, REQUEST_TIMEOUT)? {
            (Response::FormFields(f), _) => Ok(f),
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub fn outline(&self, id: u64) -> Result<Vec<OutlineEntry>, Error> {
        match self.call(&[id], &Request::Outline { doc: id }, REQUEST_TIMEOUT)? {
            (Response::Outline(o), _) => Ok(o),
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub fn export(&self, id: u64, edits: &Edits, imports: &[u64]) -> Result<Vec<u8>, Error> {
        let mut docs = vec![id];
        docs.extend_from_slice(imports);
        let request = Request::Export {
            doc: id,
            edits: edits.clone(),
            imports: imports.to_vec(),
        };
        match self.call(&docs, &request, EXPORT_TIMEOUT)? {
            (Response::Exported { len }, Payload::Fd(fd)) => {
                let map = shm::map_sealed(&fd, len).map_err(Error::Io)?;
                let len =
                    usize::try_from(len).map_err(|_| Error::Limit("export too large".into()))?;
                Ok(map.get(..len).map(<[u8]>::to_vec).unwrap_or_default())
            }
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub fn probe(&self, path: &str) -> Result<ProbeReport, Error> {
        match self.call(
            &[],
            &Request::Probe {
                path: path.to_string(),
            },
            REQUEST_TIMEOUT,
        )? {
            (Response::Probe(report), _) => Ok(report),
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// Process id of the live helper, if one is running.
    pub fn helper_pid(&self) -> Option<u32> {
        self.lock().connection.as_ref().map(|c| c.child.id())
    }

    /// How many helpers this backend has started.
    pub fn helpers_started(&self) -> u64 {
        self.lock().helpers_started
    }
}

impl Drop for Sandboxed {
    fn drop(&mut self) {
        let connection = self.lock().connection.take();
        if let Some(connection) = connection {
            connection.kill();
        }
    }
}

fn unexpected(response: &Response) -> Error {
    Error::Engine(format!(
        "unexpected reply from the PDF helper: {response:?}"
    ))
}

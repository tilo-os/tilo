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
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::edits::Edits;
use crate::error::Error;
use crate::model::{AnnotationInfo, DocumentInfo, FormField, Link, OutlineEntry, RenderOptions};
use crate::proto::{
    Channel, FailureKind, OpenKind, PROTOCOL_VERSION, ProbeReport, Request, Response,
};
use crate::text::RawChar;
use tilo_sandbox::shm;

use crate::Bitmap;
use tilo_lock::MutexRecover as _;

/// How long a helper may take to start and confine itself.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one ordinary request may run before the helper is presumed hung.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Exports copy and rewrite whole documents.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);
/// A document that has taken the helper down this many times is refused.
const MAX_CRASHES_PER_DOCUMENT: u32 = 3;
/// Idle pixel buffers kept for reuse, by count and by bytes.
const MAX_IDLE_BUFFERS: usize = 6;
const MAX_IDLE_BUFFER_BYTES: u64 = 160 * 1024 * 1024;
/// Buffer capacities are rounded up to this, so nearby sizes share buffers.
const BUFFER_GRANULE: u64 = 1024 * 1024;

/// A pixel buffer: a memory file the helper renders into, mapped read-only here.
struct PixelBuffer {
    id: u64,
    len: u64,
    fd: OwnedFd,
    map: memmap2::Mmap,
}

/// Idle pixel buffers, plus the ids of retired buffers the helper should
/// unmap at its next request.
#[derive(Default)]
struct Pool {
    idle: Vec<Arc<PixelBuffer>>,
    retired: Vec<u64>,
    next_id: u64,
}

impl Pool {
    fn take(&mut self, need: u64) -> Option<Arc<PixelBuffer>> {
        let best = self
            .idle
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.len >= need && b.len <= need.saturating_mul(2).saturating_add(BUFFER_GRANULE)
            })
            .min_by_key(|(_, b)| b.len)
            .map(|(i, _)| i)?;
        Some(self.idle.swap_remove(best))
    }

    fn give_back(&mut self, buffer: Arc<PixelBuffer>) {
        let idle_bytes: u64 = self.idle.iter().map(|b| b.len).sum();
        if self.idle.len() >= MAX_IDLE_BUFFERS
            || idle_bytes.saturating_add(buffer.len) > MAX_IDLE_BUFFER_BYTES
        {
            self.retired.push(buffer.id);
        } else {
            self.idle.push(buffer);
        }
    }
}

/// A bitmap's claim on a pixel buffer; the buffer returns to the pool when
/// the last clone of the bitmap is dropped.
struct Lease {
    buffer: Option<Arc<PixelBuffer>>,
    len: usize,
    pool: Weak<Mutex<Pool>>,
}

impl AsRef<[u8]> for Lease {
    fn as_ref(&self) -> &[u8] {
        self.buffer
            .as_ref()
            .and_then(|b| b.map.get(..self.len))
            .unwrap_or_default()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let (Some(buffer), Some(pool)) = (self.buffer.take(), self.pool.upgrade()) {
            pool.lock_recover().give_back(buffer);
        }
    }
}

struct Connection {
    child: Child,
    channel: Channel,
    generation: u64,
}

impl Connection {
    /// Stop the helper and describe how it ended.
    fn kill(mut self) -> String {
        let status = if let Ok(Some(status)) = self.child.try_wait() {
            Some(status)
        } else {
            let _ = self.child.kill();
            self.child.wait().ok()
        };
        status.map_or_else(|| "unknown exit".to_string(), |s| s.to_string())
    }

    /// Whether the helper has already exited (it may die between requests,
    /// for example after a request it answered brought it down on exit).
    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

struct DocEntry {
    fd: OwnedFd,
    len: Option<u64>,
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
    /// Pixel buffer id → the helper generation it is attached to.
    attached: HashMap<u64, u64>,
}

/// The sandboxed backend.
pub struct Sandboxed {
    helper: PathBuf,
    state: Mutex<State>,
    pool: Arc<Mutex<Pool>>,
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
            pool: Arc::new(Mutex::new(Pool::default())),
        };
        {
            let mut state = this.lock();
            let connection = this.spawn(&mut state)?;
            state.connection = Some(connection);
        }
        Ok(this)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    fn spawn(&self, state: &mut State) -> Result<Connection, Error> {
        let (ours, theirs) = UnixStream::pair().map_err(Error::Io)?;
        let mut command = helper_command(&self.helper);
        command.stdin(Stdio::from(OwnedFd::from(theirs)));
        let child = spawn_from_spawner_thread(command)
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
            Ok(Response::Failed {
                kind: FailureKind::Sandbox,
                message,
            }) => {
                let _ = connection.kill();
                return Err(Error::Unconfined(message));
            }
            Ok(Response::Failed { message, .. }) => {
                let _ = connection.kill();
                return Err(Error::Unavailable(format!(
                    "the PDF helper could not start: {message}"
                )));
            }
            Ok(_) => {
                let _ = connection.kill();
                return Err(Error::Unavailable(
                    "the PDF helper speaks another protocol".into(),
                ));
            }
            Err(e) => {
                let _ = connection.kill();
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
        len: Option<u64>,
        kind: OpenKind,
        password: Option<String>,
    ) -> Result<DocumentInfo, Error> {
        let mut state = self.lock();
        state.docs.insert(
            id,
            DocEntry {
                fd,
                len,
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
        self.ensure_running(state)?;
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
            len: entry.len,
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

    /// Replace a helper that has exited since the last request with a fresh
    /// one, so a request that never reached the dead helper does not fail.
    fn ensure_running(&self, state: &mut State) -> Result<(), Error> {
        if state.connection.as_mut().is_some_and(Connection::exited)
            && let Some(dead) = state.connection.take()
        {
            let how = dead.kill();
            eprintln!("tilo-pdf: the PDF helper exited between requests ({how}); restarting");
        }
        if state.connection.is_none() {
            let connection = self.spawn(state)?;
            state.connection = Some(connection);
        }
        Ok(())
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
                Response::Exported { .. } => Payload::Fd(connection.channel.recv_fd()?),
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
                let how = state
                    .connection
                    .take()
                    .map_or_else(String::new, Connection::kill);
                for id in docs {
                    if let Some(entry) = state.docs.get_mut(id) {
                        entry.crashes = entry.crashes.saturating_add(1);
                    }
                }
                Err(if timed_out {
                    Error::Timeout
                } else {
                    Error::Crashed(format!("the PDF helper stopped ({how}): {e}"))
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
        self.ensure_running(&mut state)?;
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

    fn lock_pool(&self) -> std::sync::MutexGuard<'_, Pool> {
        self.pool.lock_recover()
    }

    /// A pixel buffer of at least `need` bytes, reused when one is idle.
    fn pixel_buffer(&self, need: u64) -> Result<Arc<PixelBuffer>, Error> {
        let mut pool = self.lock_pool();
        if let Some(buffer) = pool.take(need) {
            return Ok(buffer);
        }
        pool.next_id = pool.next_id.saturating_add(1);
        let id = pool.next_id;
        drop(pool);
        let len = need.div_ceil(BUFFER_GRANULE).saturating_mul(BUFFER_GRANULE);
        let file = shm::create("tilo-pdf-pixels", len).map_err(Error::Io)?;
        // SAFETY: the helper writes this memory while it renders a request
        // this client made and waits for; bytes handed out are never
        // interpreted beyond copying them as pixels (see the protocol docs).
        let map = shm::map_shared(&file).map_err(Error::Io)?;
        Ok(Arc::new(PixelBuffer {
            id,
            len,
            fd: file.into(),
            map,
        }))
    }

    /// Render into a pooled pixel buffer. `need` is the output size in
    /// bytes, computed by the caller from the page geometry.
    pub fn render(
        &self,
        id: u64,
        page: usize,
        options: &RenderOptions,
        need: u64,
    ) -> Result<Bitmap, Error> {
        let page = u32::try_from(page).map_err(|_| Error::Invalid("page out of range".into()))?;
        let buffer = self.pixel_buffer(need.max(4))?;
        let mut lease = Lease {
            buffer: Some(Arc::clone(&buffer)),
            len: 0,
            pool: Arc::downgrade(&self.pool),
        };
        let retired = std::mem::take(&mut self.lock_pool().retired);
        let mut state = self.lock();
        self.ensure_open(&mut state, id)?;
        let generation = state.generation;
        for old in retired {
            if state.attached.remove(&old) == Some(generation) {
                Self::exchange(
                    &mut state,
                    &[],
                    &Request::ReleaseBuffer { buffer: old },
                    None,
                    REQUEST_TIMEOUT,
                )?;
            }
        }
        let attach = state.attached.get(&buffer.id) != Some(&generation);
        let request = Request::Render {
            doc: id,
            page,
            options: *options,
            buffer: buffer.id,
            buffer_len: buffer.len,
            attach,
        };
        let fd = attach.then_some(&buffer.fd);
        let response = Self::exchange(&mut state, &[id], &request, fd, REQUEST_TIMEOUT);
        if attach && response.is_ok() {
            state.attached.insert(buffer.id, generation);
        }
        drop(state);
        match response? {
            (Response::Rendered { width, height }, _) => {
                let len = u64::from(width)
                    .saturating_mul(u64::from(height))
                    .saturating_mul(4);
                if len > buffer.len {
                    return Err(Error::Engine("the render overran its buffer".into()));
                }
                let len =
                    usize::try_from(len).map_err(|_| Error::Limit("render too large".into()))?;
                Ok(Bitmap::from_parts(width, height, {
                    lease.len = len;
                    Arc::new(lease)
                }))
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

    /// Send the development-only abort request.
    pub fn abort_helper(&self) -> Error {
        match self.call(&[], &Request::Abort, REQUEST_TIMEOUT) {
            Ok((other, _)) => unexpected(&other),
            Err(e) => e,
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
            let _ = connection.kill();
        }
    }
}

/// Start a helper from one thread that lives as long as the process.
///
/// The helper asks the kernel to kill it when its parent goes away
/// (`PR_SET_PDEATHSIG`), and "parent" there means the *thread* that forked
/// it. Spawning from whichever thread happened to need a helper would kill
/// the helper as soon as that thread finished.
/// The time zone the helper runs in: a POSIX rule, so libc never opens a
/// zone file for it. pdfium converts times only with `gmtime`, yet glibc's
/// first conversion of any kind loads the local zone — `/etc/localtime`,
/// which the sandbox rightly refuses. Nothing the helper returns is in local
/// time (document dates are passed through as written), so UTC is what it
/// needs, and it makes exports identical whichever machine wrote them.
const HELPER_TZ: &str = "UTC0";

/// The helper's command line and environment: nothing inherited but what it
/// needs to find pdfium and fonts, in UTC.
fn helper_command(helper: &Path) -> Command {
    let mut command = Command::new(helper);
    command
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .env_clear()
        .env("TZ", HELPER_TZ);
    for key in ["TILO_PDFIUM_LIB", "HOME", "LANG"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

fn spawn_from_spawner_thread(command: Command) -> std::io::Result<Child> {
    type Job = (Command, std::sync::mpsc::Sender<std::io::Result<Child>>);
    static SPAWNER: std::sync::OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> =
        std::sync::OnceLock::new();
    let sender = SPAWNER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let started = std::thread::Builder::new()
            .name("tilo-pdf-spawner".into())
            .spawn(move || {
                for (mut command, reply) in rx {
                    let _ = reply.send(command.spawn());
                }
            });
        if let Err(e) = started {
            eprintln!("tilo-pdf: cannot start the helper spawner thread: {e}");
        }
        Mutex::new(tx)
    });
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    sender
        .lock_recover()
        .send((command, reply_tx))
        .map_err(|_| std::io::Error::other("the helper spawner thread is gone"))?;
    reply_rx
        .recv()
        .map_err(|_| std::io::Error::other("the helper spawner thread is gone"))?
}

fn unexpected(response: &Response) -> Error {
    Error::Engine(format!(
        "unexpected reply from the PDF helper: {response:?}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// glibc reads `/etc/localtime` on its first time conversion unless `TZ`
    /// names a rule it can parse without a file; the sandbox denies that read.
    #[test]
    fn the_helper_runs_in_utc_so_it_never_reads_a_zone_file() {
        let command = helper_command(Path::new("tilo-pdf-helper"));
        let tz = command
            .get_envs()
            .find(|(key, _)| *key == "TZ")
            .and_then(|(_, value)| value);
        // A POSIX rule, not a zone name ("UTC", "Etc/UTC"), which glibc
        // would look up under /usr/share/zoneinfo.
        assert_eq!(tz, Some(std::ffi::OsStr::new("UTC0")));
    }
}

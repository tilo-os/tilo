//! The line wire's client: one connection to a provider that answers frames a line each, as an
//! [`ActionProvider`] of its own, so whatever walks a provider walks the one at the other end.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;

use super::frame_line;
use crate::outcome::InvocationId;
use crate::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer,
    ContentGetAnswer, ContentGets, ContentPiece, ContentPut, ContentPutAnswer, ContentPutStep,
    ContentPuts, InvocationReceipt, InvocationState, InvokeRequest, MAX_CONTENT_FRAME_BYTES,
    MAX_HAVE_HASHES, MAX_PUT_MANY_BYTES, MAX_PUT_MANY_FILES, MAX_REMOTE_ACTION_FRAME_BYTES,
    MAX_REMOTE_WAIT, Pending, RemoteActionFrame, RemoteActionMessage, UpdateMark, pending,
};

/// How long past the longest remote wait an answer is waited for before the ask is given up.
const SLACK: Duration = Duration::from_secs(30);

type Answered<T> = Result<T, ActionCallError>;
type Asked = Completer<Answered<RemoteActionMessage>>;

/// One connection to a provider over the line wire. Every request names what its answer
/// answers (a correlation, or the invocation it is about), so requests go out at once from any
/// thread and one reader hands each answer to whoever waits for it. Who the caller is the other
/// end decides from the connection, never from the [`Caller`] a method is handed.
///
/// When the connection ends, every waiting request is answered with why, and so is every later
/// one.
#[derive(Clone)]
pub struct LineClient(Arc<Shared>);

struct Shared {
    writer: Mutex<Box<dyn Write + Send>>,
    waiting: Mutex<Waiting>,
    next: AtomicU64,
    /// The connection, shut when the last clone of the client goes: the reader then ends, and
    /// the other end hears the client left.
    #[cfg(unix)]
    stream: Option<std::os::unix::net::UnixStream>,
}

#[cfg(unix)]
impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(stream) = &self.stream {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[derive(Default)]
struct Waiting {
    /// Why the connection ended, once it did.
    ended: Option<ActionCallError>,
    by_correlation: HashMap<u64, Asked>,
    receipts: HashMap<InvocationId, Vec<Asked>>,
    cancels: HashMap<InvocationId, Vec<Asked>>,
    /// How far each invocation was seen to get, for a wait that asks past it.
    seen: HashMap<InvocationId, UpdateMark>,
}

/// What a request's answer is found by.
#[derive(Clone, Copy)]
enum Key {
    Correlation(u64),
    Receipt(InvocationId),
    Cancel(InvocationId),
}

impl LineClient {
    /// A client over `read` and `write`, the two halves of one connection.
    ///
    /// # Errors
    /// No thread could be started to read the connection.
    pub fn over(
        read: impl Read + Send + 'static,
        write: impl Write + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::reading(
            read,
            Shared {
                writer: Mutex::new(Box::new(write)),
                waiting: Mutex::default(),
                next: AtomicU64::new(1),
                #[cfg(unix)]
                stream: None,
            },
        )
    }

    /// A client over the unix socket connection `stream`, which it shuts when its last clone
    /// goes.
    ///
    /// # Errors
    /// The connection cannot be read.
    #[cfg(unix)]
    pub fn of_stream(stream: std::os::unix::net::UnixStream) -> std::io::Result<Self> {
        let (read, write) = (stream.try_clone()?, stream.try_clone()?);
        Self::reading(
            read,
            Shared {
                writer: Mutex::new(Box::new(write)),
                waiting: Mutex::default(),
                next: AtomicU64::new(1),
                stream: Some(stream),
            },
        )
    }

    fn reading(read: impl Read + Send + 'static, shared: Shared) -> std::io::Result<Self> {
        let shared = Arc::new(shared);
        let reader = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("tilo-line-client".into())
            .spawn(move || read_answers(BufReader::new(read), &reader))?;
        Ok(Self(shared))
    }

    /// A client of the provider listening at the unix socket `path`.
    ///
    /// # Errors
    /// Nothing listens there, or the connection cannot be read.
    #[cfg(unix)]
    pub fn connect(path: &std::path::Path) -> std::io::Result<Self> {
        Self::of_stream(std::os::unix::net::UnixStream::connect(path)?)
    }

    fn correlation(&self) -> u64 {
        self.0.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Send `message`, its answer found by `key`.
    fn ask(
        &self,
        key: Key,
        message: RemoteActionMessage,
    ) -> Pending<Answered<RemoteActionMessage>> {
        let (asked, answer) = pending();
        {
            let mut waiting = self.0.waiting.lock_recover();
            if let Some(ended) = &waiting.ended {
                asked.complete(Err(ended.clone()));
                return answer;
            }
            match key {
                Key::Correlation(correlation) => {
                    waiting.by_correlation.insert(correlation, asked);
                }
                Key::Receipt(invocation) => {
                    waiting.receipts.entry(invocation).or_default().push(asked);
                }
                Key::Cancel(invocation) => {
                    waiting.cancels.entry(invocation).or_default().push(asked);
                }
            }
        }
        let line = frame_line(message);
        let sent = {
            let mut writer = self.0.writer.lock_recover();
            writeln!(writer, "{line}").and_then(|()| writer.flush())
        };
        if let Err(error) = sent {
            end(
                &self.0,
                &ActionCallError::Unavailable {
                    reason: format!("the connection closed: {error}"),
                },
            );
        }
        answer
    }

    /// [`Self::ask`], waited for.
    fn asked(&self, key: Key, message: RemoteActionMessage) -> Answered<RemoteActionMessage> {
        self.ask(key, message)
            .wait_timeout(MAX_REMOTE_WAIT.saturating_add(SLACK))
            .ok()
            .flatten()
            .unwrap_or(Err(ActionCallError::Unanswered))
    }

    /// One step of an upload, waited for.
    fn put_step(&self, step: ContentPutStep) -> Answered<ContentPutAnswer> {
        let correlation = self.correlation();
        match self.asked(
            Key::Correlation(correlation),
            RemoteActionMessage::ContentPutRequest { correlation, step },
        )? {
            RemoteActionMessage::ContentPutResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        }
    }

    /// One piece of `content` from `offset`, at most `length` bytes, waited for.
    fn get_piece(&self, content: &str, offset: u64, length: u64) -> Answered<ContentGetAnswer> {
        let correlation = self.correlation();
        match self.asked(
            Key::Correlation(correlation),
            RemoteActionMessage::ContentGetRequest {
                correlation,
                content: content.to_owned(),
                offset,
                length,
            },
        )? {
            RemoteActionMessage::ContentGetResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        }
    }

    fn saw(&self, state: Option<&InvocationState>) {
        if let Some(state) = state {
            self.0
                .waiting
                .lock_recover()
                .seen
                .insert(state.invocation(), UpdateMark::of(&state.update));
        }
    }
}

/// What the other end answered where it should have answered something else.
fn unexpected(answer: &RemoteActionMessage) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: format!("the other end answered with {answer:?}"),
    }
}

/// Answer every waiting request with `why`, and every later one.
fn end(shared: &Shared, why: &ActionCallError) {
    let mut waiting = shared.waiting.lock_recover();
    waiting.ended.get_or_insert_with(|| why.clone());
    let mut all: Vec<Asked> = waiting
        .by_correlation
        .drain()
        .map(|(_, asked)| asked)
        .collect();
    all.extend(waiting.receipts.drain().flat_map(|(_, asked)| asked));
    all.extend(waiting.cancels.drain().flat_map(|(_, asked)| asked));
    drop(waiting);
    for asked in all {
        asked.complete(Err(why.clone()));
    }
}

/// Read answers off the connection and hand each to whoever waits for it, until it ends.
fn read_answers(mut read: impl BufRead, shared: &std::sync::Weak<Shared>) {
    let limit = u64::try_from(MAX_REMOTE_ACTION_FRAME_BYTES.saturating_add(2)).unwrap_or(u64::MAX);
    let why = loop {
        let mut line = Vec::new();
        match std::io::Read::take(&mut read, limit).read_until(b'\n', &mut line) {
            Ok(0) => {
                break ActionCallError::Unavailable {
                    reason: "the connection closed".into(),
                };
            }
            Ok(_) => {}
            Err(error) => {
                break ActionCallError::Unavailable {
                    reason: format!("the connection failed: {error}"),
                };
            }
        }
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let line = line.strip_suffix(b"\n").unwrap_or(&line);
        if let Ok(frame) = RemoteActionFrame::decode(line) {
            answered(&shared, frame.message);
        } else {
            // A line that is no frame is the other end refusing the connection.
            let refusal: Option<serde_json::Value> = serde_json::from_slice(line).ok();
            let message = refusal
                .as_ref()
                .and_then(|refusal| refusal.pointer("/err/message"))
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| String::from_utf8_lossy(line).into_owned(), str::to_owned);
            let code = refusal
                .as_ref()
                .and_then(|refusal| refusal.pointer("/err/code"))
                .and_then(serde_json::Value::as_str);
            let why = if code == Some("not_permitted") {
                ActionCallError::Refused { reason: message }
            } else {
                ActionCallError::Unavailable { reason: message }
            };
            end(&shared, &why);
        }
    };
    if let Some(shared) = shared.upgrade() {
        end(&shared, &why);
    }
}

/// Hand `message` to whoever waits for it; an answer nobody waits for is dropped.
fn answered(shared: &Shared, message: RemoteActionMessage) {
    let mut waiting = shared.waiting.lock_recover();
    let asked = match &message {
        RemoteActionMessage::CatalogueResponse { correlation, .. }
        | RemoteActionMessage::ContentPutResponse { correlation, .. }
        | RemoteActionMessage::ContentGetResponse { correlation, .. }
        | RemoteActionMessage::Update {
            correlation: Some(correlation),
            ..
        } => waiting.by_correlation.remove(correlation),
        RemoteActionMessage::Receipt(receipt) => {
            take_first(&mut waiting.receipts, receipt.invocation)
        }
        RemoteActionMessage::CancelResponse { invocation, .. } => {
            take_first(&mut waiting.cancels, *invocation)
        }
        _ => None,
    };
    drop(waiting);
    if let Some(asked) = asked {
        asked.complete(Ok(message));
    }
}

fn take_first(
    all: &mut HashMap<InvocationId, Vec<Asked>>,
    invocation: InvocationId,
) -> Option<Asked> {
    let asked = all.get_mut(&invocation)?;
    let first = (!asked.is_empty()).then(|| asked.remove(0));
    if asked.is_empty() {
        all.remove(&invocation);
    }
    first
}

impl ActionProvider for LineClient {
    fn query(&self, _caller: &Caller, query: CatalogueQuery) -> Pending<Answered<CataloguePage>> {
        let correlation = self.correlation();
        self.ask(
            Key::Correlation(correlation),
            RemoteActionMessage::CatalogueRequest { correlation, query },
        )
        .map(|answer| match answer? {
            RemoteActionMessage::CatalogueResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        })
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Answered<InvocationReceipt>> {
        let invocation = request.invocation;
        self.ask(
            Key::Receipt(invocation),
            RemoteActionMessage::Invoke(request),
        )
        .map(|answer| match answer? {
            RemoteActionMessage::Receipt(receipt) => Ok(receipt),
            other => Err(unexpected(&other)),
        })
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let correlation = self.correlation();
        let state = match self
            .asked(
                Key::Correlation(correlation),
                RemoteActionMessage::Observe {
                    correlation,
                    invocation: *id,
                    wait_ms: None,
                    seen: None,
                },
            )
            .ok()?
        {
            RemoteActionMessage::Update { state, .. } => state,
            _ => None,
        };
        self.saw(state.as_ref());
        state
    }

    /// Resolves once the invocation moves past where it was last seen here, or the longest
    /// remote wait passes; `None` for one seen to end.
    fn next_change(&self, _caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let seen = self
            .0
            .waiting
            .lock_recover()
            .seen
            .get(id)
            .copied()
            .unwrap_or(UpdateMark::Accepted);
        if seen == UpdateMark::Finished {
            return None;
        }
        let correlation = self.correlation();
        let wait_ms = u32::try_from(MAX_REMOTE_WAIT.as_millis()).unwrap_or(u32::MAX);
        let this = self.clone();
        Some(
            self.ask(
                Key::Correlation(correlation),
                RemoteActionMessage::Observe {
                    correlation,
                    invocation: *id,
                    wait_ms: Some(wait_ms),
                    seen: Some(seen),
                },
            )
            .map(move |answer| {
                if let Ok(RemoteActionMessage::Update { state, .. }) = answer {
                    this.saw(state.as_ref());
                }
            }),
        )
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Answered<()>> {
        self.ask(
            Key::Cancel(*id),
            RemoteActionMessage::Cancel { invocation: *id },
        )
        .map(|answer| match answer? {
            RemoteActionMessage::CancelResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        })
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::new(self.clone()))
    }

    fn content_gets(&self) -> Option<Arc<dyn ContentGets>> {
        Some(Arc::new(self.clone()))
    }
}

/// What the other end answered for an upload step where it should have answered something else.
fn unexpected_step(answer: &ContentPutAnswer) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: format!("the other end answered an upload step with {answer:?}"),
    }
}

impl LineClient {
    /// Open an upload of `size` bytes, which the other end keeps by the id it answers
    /// ([`LinePut::upload`]).
    ///
    /// # Errors
    /// The other end refused it, or stopped answering.
    pub fn begin_put(&self, size: u64) -> Answered<LinePut> {
        match self.put_step(ContentPutStep::Begin { size })? {
            ContentPutAnswer::Begun { upload } => Ok(LinePut {
                client: self.clone(),
                upload,
                size,
                written: 0,
            }),
            other => Err(unexpected_step(&other)),
        }
    }

    /// Take up the upload `upload` of `size` bytes, begun on a connection that was lost: it goes
    /// on from the bytes the other end has of it ([`ContentPut::written`]).
    ///
    /// # Errors
    /// The other end keeps no such upload for this caller, or stopped answering.
    pub fn resume_put(&self, upload: u64, size: u64) -> Answered<LinePut> {
        match self.put_step(ContentPutStep::Progress { upload })? {
            ContentPutAnswer::Appended { written } => Ok(LinePut {
                client: self.clone(),
                upload,
                size,
                written,
            }),
            other => Err(unexpected_step(&other)),
        }
    }
}

impl ContentPuts for LineClient {
    fn begin(&self, _caller: &Caller, size: u64) -> Answered<Box<dyn ContentPut>> {
        Ok(Box::new(self.begin_put(size)?))
    }

    fn put_many(&self, caller: &Caller, files: &[Vec<u8>]) -> Answered<Vec<String>> {
        let mut contents = Vec::with_capacity(files.len());
        let mut group: Vec<String> = Vec::new();
        let mut bytes = 0_usize;
        let send = |group: &mut Vec<String>, contents: &mut Vec<String>| {
            let files = std::mem::take(group);
            let count = files.len();
            match self.put_step(ContentPutStep::Many { files })? {
                ContentPutAnswer::Many { contents: answered } if answered.len() == count => {
                    contents.extend(answered);
                    Ok(())
                }
                other => Err(unexpected_step(&other)),
            }
        };
        for file in files {
            // A file too large for one step goes up as an upload of its own.
            if file.len() > MAX_PUT_MANY_BYTES {
                if !group.is_empty() {
                    send(&mut group, &mut contents)?;
                    bytes = 0;
                }
                let mut put = self.begin(caller, u64::try_from(file.len()).unwrap_or(u64::MAX))?;
                put.append(0, file)?;
                contents.push(put.finish()?);
                continue;
            }
            if !group.is_empty()
                && (bytes.saturating_add(file.len()) > MAX_PUT_MANY_BYTES
                    || group.len() >= MAX_PUT_MANY_FILES)
            {
                send(&mut group, &mut contents)?;
                bytes = 0;
            }
            bytes = bytes.saturating_add(file.len());
            group.push(hex::encode(file));
        }
        if !group.is_empty() {
            send(&mut group, &mut contents)?;
        }
        Ok(contents)
    }

    fn have(&self, _caller: &Caller, hashes: &[String]) -> Answered<Vec<bool>> {
        let mut present = Vec::with_capacity(hashes.len());
        for chunk in hashes.chunks(MAX_HAVE_HASHES) {
            match self.put_step(ContentPutStep::Have {
                hashes: chunk.to_vec(),
            })? {
                ContentPutAnswer::Have { present: answered } if answered.len() == chunk.len() => {
                    present.extend(answered);
                }
                other => return Err(unexpected_step(&other)),
            }
        }
        Ok(present)
    }
}

/// One upload the other end keeps open.
pub struct LinePut {
    client: LineClient,
    upload: u64,
    size: u64,
    written: u64,
}

impl LinePut {
    /// The id the other end keeps the upload by.
    #[must_use]
    pub const fn upload(&self) -> u64 {
        self.upload
    }
}

impl ContentPut for LinePut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    /// `chunk` at `offset`, a frame's worth of bytes at a time.
    fn append(&mut self, offset: u64, chunk: &[u8]) -> Answered<u64> {
        let mut at = offset;
        for piece in chunk.chunks(MAX_CONTENT_FRAME_BYTES) {
            match self
                .client
                .put_step(ContentPutStep::append(self.upload, at, piece))?
            {
                ContentPutAnswer::Appended { written } => {
                    self.written = written;
                    at = written;
                }
                other => return Err(unexpected_step(&other)),
            }
        }
        Ok(self.written)
    }

    fn finish(self: Box<Self>) -> Answered<String> {
        match self.client.put_step(ContentPutStep::Finish {
            upload: self.upload,
        })? {
            ContentPutAnswer::Finished { content } => Ok(content),
            other => Err(unexpected_step(&other)),
        }
    }
}

impl ContentGets for LineClient {
    /// `length` bytes of `content` from `offset`, asked a frame's worth at a time; a `length` of
    /// zero asks for one piece.
    fn get(
        &self,
        _caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Answered<ContentPiece> {
        let piece_bytes = u64::try_from(MAX_CONTENT_FRAME_BYTES).unwrap_or(u64::MAX);
        let mut bytes = Vec::new();
        let mut at = offset;
        let mut size;
        loop {
            let left = length.saturating_sub(at.saturating_sub(offset));
            let wanted = if length == 0 {
                piece_bytes
            } else {
                left.min(piece_bytes)
            };
            let answer = self.get_piece(content, at, wanted)?;
            let piece = answer.bytes().ok_or_else(|| ActionCallError::Unavailable {
                reason: format!("the other end answered {content} with bytes that are not hex"),
            })?;
            if answer.offset != at || (piece.is_empty() && at < answer.size) {
                return Err(ActionCallError::Unavailable {
                    reason: format!(
                        "the other end answered {content} at {} with {} bytes, asked at {at}",
                        answer.offset,
                        piece.len()
                    ),
                });
            }
            size = answer.size;
            at = at.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
            bytes.extend_from_slice(&piece);
            if length == 0 || at >= answer.size || at.saturating_sub(offset) >= length {
                break;
            }
        }
        Ok(ContentPiece {
            size,
            offset,
            bytes,
        })
    }
}

//! A whole connection of the line wire, answered as one caller: what a local socket that keeps
//! state between lines serves.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;

use super::{answer_message, frame_line, line_refusal};
use crate::outcome::InvocationId;
use crate::remote::refuse;
use crate::remote::{
    ActionCallError, ActionProvider, Caller, ContentGetAnswer, HostedContentPuts,
    MAX_REMOTE_ACTION_FRAME_BYTES, Pending, RemoteActionFrame, RemoteActionMessage,
    STORE_COMMANDS_UNAVAILABLE,
};

/// How many requests that wait one connection keeps answering at once; one past them is answered
/// in turn, so a caller that floods waits slows only itself.
const WAITING_AT_ONCE: usize = 64;

/// Answer every line `input` reads as `tree` answers the frame it holds, `first` before them,
/// writing each answer to `output` until `input` ends or `output` closes.
///
/// `tree` names, for each frame, the provider it is asked of and the caller it is asked as: a
/// connection that serves every workspace its device holds answers each frame in the workspace
/// it names ([`RemoteActionFrame::workspace`]). A frame `tree` refuses is answered with that
/// refusal, and the connection goes on.
///
/// Uploads stay open between lines in `uploads` ([`HostedContentPuts`]), each answering only the
/// caller that began it: kept beyond the connection, one a lost connection cut is taken up on the
/// next. Content is read through the provider's [`ActionProvider::content_gets`]. A request that waits
/// ([`RemoteActionMessage::waits`]) is answered on a thread of its own, so the lines after it are
/// answered meanwhile; each answer names what it answers, so the order answers go out in is
/// theirs. A line that is no frame is refused, and the connection goes on.
///
/// A connection that ends cancels the calls it still waits on, through
/// [`ActionProvider::cancel`]: a call that outlives its caller is told it stopped.
pub fn serve_lines<H: ActionProvider + Clone + Send + Sync + 'static>(
    tree: impl FnMut(&RemoteActionFrame) -> Result<(H, Caller), ActionCallError>,
    uploads: &HostedContentPuts,
    first: Option<Vec<u8>>,
    input: impl BufRead,
    output: impl Write + Send + 'static,
    slack: Duration,
) {
    serve_lines_with(tree, uploads, first, input, output, slack, None);
}

/// What a host's own lane did with a frame.
pub enum Take {
    /// The frame is not the lane's: the provider answers it as any other.
    NotCall,
    /// The lane took it and answers nothing.
    Silent,
    /// The lane's answer, as the line to write.
    Answer(String),
}

/// A host's own frames on a connection it serves, beside the action wire: a call lane on an
/// app's connection ([`crate::remote::call`]). The host answers them itself and may write
/// pushes of its own to the same connection; a frame it does not take goes to the provider.
pub trait CallLanes: Send + Sync {
    /// Take `message` as the lane's, or say it is the provider's.
    fn take(&self, message: &RemoteActionMessage) -> Take;
    /// How many bytes of binary body follow `message` on the connection, when the lane reads
    /// one (an app's sound output write carries its PCM so). None for every line-only frame.
    fn body_len(&self, _message: &RemoteActionMessage) -> Option<usize> {
        None
    }
    /// Take a frame whose binary `body` was read after it.
    fn take_body(&self, message: &RemoteActionMessage, _body: &[u8]) -> Take {
        self.take(message)
    }
}

/// [`serve_lines`], with `lanes` answering the frames it owns before the provider sees them.
pub fn serve_lines_with<H: ActionProvider + Clone + Send + Sync + 'static>(
    tree: impl FnMut(&RemoteActionFrame) -> Result<(H, Caller), ActionCallError>,
    uploads: &HostedContentPuts,
    first: Option<Vec<u8>>,
    input: impl BufRead,
    output: impl Write + Send + 'static,
    slack: Duration,
    lanes: Option<&dyn CallLanes>,
) {
    let _ended = serve_lines_handing(tree, uploads, first, input, output, slack, lanes, None);
}

/// What tells a connection's serving that what answers it is ending ([`serve_lines_handing`]).
pub trait Handover: Sync {
    /// Whether what answers the connection is ending: no call is taken any more.
    fn handing(&self) -> bool;
    /// Whether the time to finish the calls in flight ran out: those still running are
    /// cancelled.
    fn overdue(&self) -> bool;
}

/// How a connection's serving ended ([`serve_lines_handing`]).
pub enum Served<R> {
    /// The connection ended, or its output closed.
    Ended,
    /// What answered it is ending: every call taken here was answered, and the connection is
    /// handed on, to be served by what answers next.
    Handed {
        /// The line read and not answered, which asks a call of what answers next.
        held: Option<Vec<u8>>,
        /// The input, at the line after it.
        input: R,
        /// Where answers went.
        output: Box<dyn Write + Send>,
    },
}

/// [`serve_lines_with`], handing the connection on once `handover` says what answers it ends.
///
/// From then on no new call is taken: a frame that follows a call this connection made (its
/// observing, its cancelling, a step of a transfer) is still answered here, and the first frame
/// that asks something new is held. Once every call the connection made was answered (or
/// [`Handover::overdue`] says their time ran out, and they are cancelled), the connection is
/// handed back with that held line, so its caller sees no gap. An input that reads nothing for
/// now says so with [`std::io::ErrorKind::TimedOut`], and is read again after `handover` is
/// asked.
#[expect(
    clippy::too_many_lines,
    reason = "one loop, kept whole so the order it answers in is the connection's order"
)]
pub fn serve_lines_handing<
    H: ActionProvider + Clone + Send + Sync + 'static,
    R: BufRead,
    W: Write + Send + 'static,
>(
    mut tree: impl FnMut(&RemoteActionFrame) -> Result<(H, Caller), ActionCallError>,
    uploads: &HostedContentPuts,
    first: Option<Vec<u8>>,
    mut input: R,
    output: W,
    slack: Duration,
    lanes: Option<&dyn CallLanes>,
    handover: Option<&dyn Handover>,
) -> Served<R> {
    let connection = Arc::new(Connection {
        output: Mutex::new(Some(output)),
        waiting: AtomicUsize::new(0),
        in_flight: Mutex::default(),
    });
    let mut next = first;
    let mut partial = Vec::new();
    let stopped = loop {
        let line = match next.take() {
            Some(line) => line,
            None => match read_line(&mut input, &mut partial) {
                Ok(Some(line)) => line,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => match handover {
                    Some(handover)
                        if handover.handing()
                            && partial.is_empty()
                            && connection.answered(handover) =>
                    {
                        break Stopped::Handed(None);
                    }
                    Some(_) => continue,
                    None => break Stopped::Ended,
                },
                Ok(None) | Err(_) => break Stopped::Ended,
            },
        };
        let frame = match RemoteActionFrame::decode(&line) {
            Ok(frame) => frame,
            Err(error) => {
                if !write_line(
                    &connection.output,
                    &line_refusal("parse_error", &error.to_string()),
                ) {
                    break Stopped::Ended;
                }
                continue;
            }
        };
        if let Some(handover) = handover
            && handover.handing()
            && !connection.follows(&frame.message)
        {
            // A new call is for what answers next: it waits until every call taken here is.
            while !connection.answered(handover) {
                std::thread::sleep(ANSWERED_EVERY);
            }
            break Stopped::Handed(Some(line));
        }
        if let Some(lanes) = lanes {
            let taken = match lanes.body_len(&frame.message) {
                Some(length) => {
                    let mut body = vec![0_u8; length];
                    match std::io::Read::read_exact(&mut input, &mut body) {
                        Ok(()) => lanes.take_body(&frame.message, &body),
                        Err(_) => break Stopped::Ended,
                    }
                }
                None => lanes.take(&frame.message),
            };
            match taken {
                Take::NotCall => {}
                Take::Silent => continue,
                Take::Answer(answer) => {
                    if !write_line(&connection.output, &answer) {
                        break Stopped::Ended;
                    }
                    continue;
                }
            }
        }
        let (provider, caller) = match tree(&frame) {
            Ok(served) => served,
            Err(refusal) => {
                let answer = refuse(&frame.message, refusal).map_or_else(
                    || line_refusal("parse_error", "a frame sent here asks something"),
                    frame_line,
                );
                if !write_line(&connection.output, &answer) {
                    break Stopped::Ended;
                }
                continue;
            }
        };
        let (provider, caller) = (&provider, &caller);
        let invocation = invocation_of(&frame.message);
        if let Some(invocation) = invocation {
            connection
                .in_flight
                .lock_recover()
                .insert(invocation, (provider.clone(), caller.clone()));
        }
        let ended = invocation.or_else(|| observed_by(&frame.message));
        match answer_frame(
            provider,
            caller,
            uploads,
            frame.message,
            slack,
            &connection,
            ended,
        ) {
            Answered::Spawned => {}
            Answered::Ended => break Stopped::Ended,
            Answered::Line(answer) => {
                if !write_line(&connection.output, &answer) {
                    break Stopped::Ended;
                }
            }
        }
    };
    finish(&connection, stopped, input)
}

/// End serving `connection`: what it still waits on is cancelled, as its caller's call, and
/// one `stopped` to be handed on goes on with what answers next, from `input`.
fn finish<H: ActionProvider, R, W: Write + Send + 'static>(
    connection: &Connection<H, W>,
    stopped: Stopped,
    input: R,
) -> Served<R> {
    let running: Vec<(InvocationId, H, Caller)> = connection
        .in_flight
        .lock_recover()
        .drain()
        .map(|(invocation, (provider, caller))| (invocation, provider, caller))
        .collect();
    for (invocation, provider, caller) in running {
        let _cancelling = provider.cancel(&caller, &invocation);
    }
    let Stopped::Handed(held) = stopped else {
        return Served::Ended;
    };
    // A waiting answer still being made writes nowhere once the connection is handed on: what
    // answers next writes to it alone.
    let Some(output) = connection.output.lock_recover().take() else {
        return Served::Ended;
    };
    Served::Handed {
        held,
        input,
        output: Box::new(output),
    }
}

/// Why a connection's serving stopped reading it.
enum Stopped {
    /// It ended, or its output closed.
    Ended,
    /// What answers it ends: the connection goes on with what answers next, from the line held,
    /// if one was.
    Handed(Option<Vec<u8>>),
}

/// How often a connection handing over looks whether its calls were answered.
const ANSWERED_EVERY: Duration = Duration::from_millis(10);

/// What one connection keeps while it serves: where answers go, how many frames wait at once, and
/// the calls it still waits on, each cancelled when the connection ends.
struct Connection<H, W> {
    /// `None` once the connection is handed on.
    output: Mutex<Option<W>>,
    waiting: AtomicUsize,
    in_flight: Mutex<HashMap<InvocationId, (H, Caller)>>,
}

impl<H, W> Connection<H, W> {
    /// Whether every call taken here was answered: none waits for its answer, and none runs on,
    /// unless `handover` says their time ran out.
    fn answered(&self, handover: &dyn Handover) -> bool {
        self.waiting.load(Ordering::Acquire) == 0
            && (self.in_flight.lock_recover().is_empty() || handover.overdue())
    }

    /// Whether `message` follows a call this connection made, rather than asking a new one.
    fn follows(&self, message: &RemoteActionMessage) -> bool {
        match message {
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => {
                self.in_flight.lock_recover().contains_key(invocation)
            }
            RemoteActionMessage::ContentPutRequest { .. }
            | RemoteActionMessage::ContentGetRequest { .. } => true,
            _ => false,
        }
    }
}

/// What became of the answer to one frame.
enum Answered {
    /// A thread of its own answers it, so the lines after it are answered meanwhile.
    Spawned,
    /// The line to write now.
    Line(String),
    /// It could not be answered: the connection ends.
    Ended,
}

/// Answer the frame `message` as `provider` and `caller`, keeping `invocation` as the
/// connection's call until an answer ends it.
fn answer_frame<H: ActionProvider + Clone + Send + Sync + 'static, W: Write + Send + 'static>(
    provider: &H,
    caller: &Caller,
    uploads: &HostedContentPuts,
    message: RemoteActionMessage,
    slack: Duration,
    connection: &Arc<Connection<H, W>>,
    invocation: Option<InvocationId>,
) -> Answered {
    let line = match message {
        RemoteActionMessage::ContentPutRequest { correlation, step } => {
            let result = uploads.step(&|target| provider.content_puts_for(target), caller, step);
            frame_line(RemoteActionMessage::ContentPutResponse {
                correlation,
                result,
            })
        }
        RemoteActionMessage::ContentGetRequest {
            correlation,
            content,
            offset,
            length,
        } => {
            let result = provider
                .content_gets()
                .ok_or_else(no_content_store)
                .and_then(|store| store.get(caller, &content, offset, length))
                .and_then(ContentGetAnswer::new);
            frame_line(RemoteActionMessage::ContentGetResponse {
                correlation,
                result,
            })
        }
        // Taken here, in the order the connection sent them, so a change sent in parts is taken
        // whole; answered once the store answers, while the lines after it are answered.
        RemoteActionMessage::StoreRequest {
            correlation,
            command,
            kind,
        } => {
            let answer = match provider.store_commands() {
                Some(store) => store.command(caller, command, kind),
                None => Pending::ready(Err(ActionCallError::Unavailable {
                    reason: STORE_COMMANDS_UNAVAILABLE.into(),
                })),
            };
            connection.waiting.fetch_add(1, Ordering::AcqRel);
            let connection = Arc::clone(connection);
            answer.then(move |result| {
                let result = result.unwrap_or(Err(ActionCallError::Unanswered));
                write_line(&connection.output, &store_line(correlation, result));
                connection.waiting.fetch_sub(1, Ordering::AcqRel);
            });
            return Answered::Spawned;
        }
        message
            if message.waits() && connection.waiting.load(Ordering::Relaxed) < WAITING_AT_ONCE =>
        {
            connection.waiting.fetch_add(1, Ordering::AcqRel);
            let (provider, caller) = (provider.clone(), caller.clone());
            let connection = Arc::clone(connection);
            let spawned = std::thread::Builder::new()
                .name("tilo-line-wait".into())
                .spawn(move || {
                    let answer = answer_message(&provider, &caller, message, slack);
                    if let Some(invocation) = invocation
                        && answers_the_end(&answer)
                    {
                        connection.in_flight.lock_recover().remove(&invocation);
                    }
                    write_line(&connection.output, &answer);
                    connection.waiting.fetch_sub(1, Ordering::AcqRel);
                });
            return if spawned.is_err() {
                Answered::Ended
            } else {
                Answered::Spawned
            };
        }
        message => answer_message(provider, caller, message, slack),
    };
    if let Some(invocation) = invocation
        && answers_the_end(&line)
    {
        connection.in_flight.lock_recover().remove(&invocation);
    }
    Answered::Line(line)
}

/// The invocation a message waits on, when it is one a connection keeps waiting on.
fn invocation_of(message: &RemoteActionMessage) -> Option<InvocationId> {
    match message {
        RemoteActionMessage::Invoke(request) => Some(request.invocation),
        _ => None,
    }
}

/// The invocation a message observes: an answer that says it ended ends the connection's wait
/// on it, when the connection made it.
fn observed_by(message: &RemoteActionMessage) -> Option<InvocationId> {
    match message {
        RemoteActionMessage::Observe { invocation, .. } => Some(*invocation),
        _ => None,
    }
}

/// Whether the answer to an invocation ends it: one that runs on is kept as the connection's call
/// until it ends or the connection does, and an answer this cannot read is its end.
fn answers_the_end(answer: &str) -> bool {
    let Ok(frame) = RemoteActionFrame::decode(answer.as_bytes()) else {
        return true;
    };
    match frame.message {
        RemoteActionMessage::Receipt(receipt) => receipt.status.is_terminal(),
        RemoteActionMessage::Update { state, .. } => state.is_none_or(|state| state.is_terminal()),
        _ => true,
    }
}

/// The answer to the store command `correlation`: the store's, or why it does not fit a frame,
/// so an answer too large never reads as the connection's end.
fn store_line(correlation: u64, result: Result<serde_json::Value, ActionCallError>) -> String {
    let answer = |result| RemoteActionMessage::StoreResponse {
        correlation,
        result,
    };
    match RemoteActionFrame::new(answer(result)).encode() {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => frame_line(answer(Err(ActionCallError::Unavailable {
            reason: format!("the store's answer does not fit the connection: {error}"),
        }))),
    }
}

/// What a connection is told whose provider keeps no content.
fn no_content_store() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "no content store is open here".into(),
    }
}

/// Write `line` and its end; whether the other side still reads.
fn write_line(output: &Mutex<Option<impl Write>>, line: &str) -> bool {
    output.lock_recover().as_mut().is_some_and(|output| {
        writeln!(output, "{line}")
            .and_then(|()| output.flush())
            .is_ok()
    })
}

/// The next line of `input`, without its end; `None` once it ended. A line longer than any
/// frame is read past and answered as the empty line, which is no frame. What was read of a line
/// when reading failed stays in `partial`, read on from there next time.
fn read_line(input: &mut impl BufRead, partial: &mut Vec<u8>) -> std::io::Result<Option<Vec<u8>>> {
    let limit = u64::try_from(MAX_REMOTE_ACTION_FRAME_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
    if partial.len() <= MAX_REMOTE_ACTION_FRAME_BYTES {
        let left = limit.saturating_sub(u64::try_from(partial.len()).unwrap_or(u64::MAX));
        let read = std::io::Read::take(&mut *input, left).read_until(b'\n', partial)?;
        if read == 0 && partial.is_empty() {
            return Ok(None);
        }
        let mut line = std::mem::take(partial);
        if line.last() == Some(&b'\n') {
            line.pop();
            return Ok(Some(line));
        }
        if line.len() <= MAX_REMOTE_ACTION_FRAME_BYTES {
            // The input ended within its last line.
            return Ok(Some(line));
        }
        // Too long: what is left of it is read past, and the line is kept as too long.
        *partial = line;
    }
    loop {
        let mut rest = Vec::new();
        let read = std::io::Read::take(&mut *input, limit).read_until(b'\n', &mut rest)?;
        if read == 0 || rest.last() == Some(&b'\n') {
            partial.clear();
            return Ok(Some(Vec::new()));
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::api::ActionAddress;
    use crate::outcome::{ActionOutput, InvocationId};
    use crate::remote::{
        CataloguePage, CatalogueQuery, Completer, ContentGets, ContentPiece, ContentPut,
        ContentPuts, InvocationReceipt, InvocationState, InvokeRequest, LineClient, Pending,
        ReceiptStatus, RemoteActionTarget, TestProvider, pending,
    };
    use std::collections::HashMap;
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    /// A provider that keeps content in memory, its actions [`TestProvider`]'s.
    #[derive(Clone)]
    struct Kept {
        actions: Arc<TestProvider>,
        store: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    struct Upload {
        store: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        size: u64,
        bytes: Vec<u8>,
    }

    impl ContentPut for Upload {
        fn size(&self) -> u64 {
            self.size
        }
        fn written(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
            if offset != self.written() {
                return Err(ActionCallError::InvalidRequest {
                    reason: "a piece out of order".into(),
                });
            }
            self.bytes.extend_from_slice(chunk);
            Ok(self.written())
        }
        fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
            if self.written() != self.size {
                return Err(ActionCallError::InvalidRequest {
                    reason: "an upload finished short".into(),
                });
            }
            let content = format!("tilo.content:{}", blake3::hash(&self.bytes).to_hex());
            self.store
                .lock_recover()
                .insert(content.clone(), self.bytes);
            Ok(content)
        }
    }

    impl ContentPuts for Kept {
        fn begin(&self, _: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
            Ok(Box::new(Upload {
                store: Arc::clone(&self.store),
                size,
                bytes: Vec::new(),
            }))
        }
        fn have(&self, _: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
            let store = self.store.lock_recover();
            Ok(hashes
                .iter()
                .map(|hash| store.contains_key(&format!("tilo.content:{hash}")))
                .collect())
        }
    }

    impl ContentGets for Kept {
        fn get(
            &self,
            _: &Caller,
            content: &str,
            offset: u64,
            length: u64,
        ) -> Result<ContentPiece, ActionCallError> {
            let bytes = self
                .store
                .lock_recover()
                .get(content)
                .cloned()
                .ok_or_else(|| ActionCallError::InvalidRequest {
                    reason: "no such content".into(),
                })?;
            let length = if length == 0 {
                300_000
            } else {
                length.min(300_000)
            };
            let start = usize::try_from(offset).unwrap().min(bytes.len());
            let end = start
                .saturating_add(usize::try_from(length).unwrap())
                .min(bytes.len());
            Ok(ContentPiece {
                size: bytes.len() as u64,
                offset,
                bytes: bytes[start..end].to_vec(),
            })
        }
    }

    impl ActionProvider for Kept {
        fn query(
            &self,
            caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            self.actions.query(caller, query)
        }
        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.actions.invoke(caller, request)
        }
        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.actions.observe(caller, id)
        }
        fn cancel(
            &self,
            caller: &Caller,
            id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            self.actions.cancel(caller, id)
        }
        fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
            Some(Arc::new(self.clone()))
        }
        fn content_gets(&self) -> Option<Arc<dyn ContentGets>> {
            Some(Arc::new(self.clone()))
        }
    }

    /// A client of `provider`, served as `caller` on the other end of a socket.
    fn connected<H: ActionProvider + Clone + Send + Sync + 'static>(
        provider: H,
        caller: Caller,
    ) -> LineClient {
        let (near, far) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let output = far.try_clone().unwrap();
            serve_lines(
                |_| Ok((provider.clone(), caller.clone())),
                &HostedContentPuts::default(),
                None,
                BufReader::new(far),
                output,
                Duration::ZERO,
            );
        });
        LineClient::of_stream(near).unwrap()
    }

    fn target() -> RemoteActionTarget {
        RemoteActionTarget {
            provider: "notes".into(),
            address: ActionAddress::new("notes", "create"),
        }
    }

    /// Asked from any thread at once, each answer reaches the ask it answers, decided as the
    /// caller the serving end names, whoever the client says it is.
    #[test]
    fn answers_reach_their_asks_as_the_serving_ends_caller() {
        let actions = Arc::new(
            TestProvider::new()
                .with_catalogue(|_, query| {
                    Ok(CataloguePage {
                        revision: "r1".into(),
                        ..CataloguePage::from_rows("r1".into(), Vec::new(), &query)
                    })
                })
                .completing(|request| ActionOutput {
                    value: Some(serde_json::json!({ "asked": request.invocation.0.to_string() })),
                    resources: Vec::new(),
                }),
        );
        let served = Caller::local_app("notes").in_workspace("w");
        let client = connected(
            Kept {
                actions: Arc::clone(&actions),
                store: Arc::default(),
            },
            served.clone(),
        );
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let client = client.clone();
                std::thread::spawn(move || {
                    let request = InvokeRequest::plain(target(), serde_json::Map::new());
                    let invocation = request.invocation;
                    let receipt = client
                        .invoke(&Caller::local_person("someone else"), request)
                        .wait_timeout(Duration::from_secs(10))
                        .ok()
                        .flatten()
                        .expect("answered")
                        .unwrap();
                    assert_eq!(receipt.invocation, invocation);
                    let ReceiptStatus::Completed { output } = receipt.status else {
                        panic!("not completed: {receipt:?}");
                    };
                    assert_eq!(
                        output.value.unwrap()["asked"],
                        invocation.0.to_string(),
                        "each receipt answers its own invocation"
                    );
                })
            })
            .collect();
        let page = client
            .query(
                &Caller::local_person("someone else"),
                CatalogueQuery::default(),
            )
            .wait_timeout(Duration::from_secs(10))
            .ok()
            .flatten()
            .expect("answered")
            .unwrap();
        assert_eq!(page.revision, "r1");
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(actions.asked().len(), 8);
        assert!(
            actions.callers().iter().all(|caller| *caller == served),
            "every call is the serving end's caller: {:?}",
            actions.callers()
        );
    }

    /// A file larger than a frame goes up in pieces on one open upload, small files go up
    /// together, and each comes back whole, read a piece at a time.
    #[test]
    fn content_goes_up_in_frames_and_comes_back_whole() {
        let kept = Kept {
            actions: Arc::new(TestProvider::new()),
            store: Arc::default(),
        };
        let client = connected(kept, Caller::local_person("ada"));
        let caller = Caller::local_person("ada");
        let large: Vec<u8> = (0..1_000_000_u32).map(|at| (at % 251) as u8).collect();
        let small = vec![b"one".to_vec(), b"two".to_vec(), large.clone()];
        let puts = client.content_puts().unwrap();
        let contents = puts.put_many(&caller, &small).unwrap();
        assert_eq!(contents.len(), 3);
        let present = puts
            .have(&caller, &[contents[0].clone(), "0".repeat(64)])
            .unwrap();
        assert_eq!(
            present,
            [true, false],
            "a put file is there, another is not"
        );
        let gets = client.content_gets().unwrap();
        let whole = gets
            .get(&caller, &contents[2], 0, large.len() as u64)
            .unwrap();
        assert_eq!(whole.size, large.len() as u64);
        assert!(whole.bytes == large, "the large file comes back whole");
        let first = gets.get(&caller, &contents[2], 10, 0).unwrap();
        assert_eq!(first.bytes.len(), 300_000, "zero asks for one piece");
        assert_eq!(first.bytes[..], large[10..300_010]);
        assert_eq!(gets.get(&caller, &contents[1], 0, 0).unwrap().bytes, b"two");
        let refused = gets.get(&caller, "tilo.content:nothing", 0, 0).unwrap_err();
        assert!(
            matches!(refused, ActionCallError::InvalidRequest { .. }),
            "{refused:?}"
        );
    }

    /// A connection the other end closes answers what waits on it, and every later ask, with
    /// why; a refusal line says it in the other end's words.
    #[test]
    fn a_closed_connection_answers_every_ask() {
        let (near, far) = UnixStream::pair().unwrap();
        let client = LineClient::of_stream(near).unwrap();
        let mut far_writer = far.try_clone().unwrap();
        let mut reader = BufReader::new(far);
        let asked = client.query(&Caller::local_person("ada"), CatalogueQuery::default());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(RemoteActionFrame::decode(line.trim_end().as_bytes()).is_ok());
        writeln!(far_writer, "{}", line_refusal("not_permitted", "not yours")).unwrap();
        let refused = asked
            .wait_timeout(Duration::from_secs(10))
            .ok()
            .flatten()
            .expect("answered")
            .unwrap_err();
        assert_eq!(
            refused,
            ActionCallError::Refused {
                reason: "not yours".into()
            }
        );
        let later = client
            .cancel(&Caller::local_person("ada"), &InvocationId::new())
            .wait_timeout(Duration::from_secs(10))
            .ok()
            .flatten()
            .expect("answered")
            .unwrap_err();
        assert_eq!(later, refused);
        // A client that goes closes its connection, so the other end hears it left.
        drop(client);
        let mut rest = String::new();
        assert_eq!(reader.read_line(&mut rest).unwrap(), 0, "{rest}");
    }

    /// What answers a call kept until it hears it stop.
    type Held = Completer<Result<InvocationReceipt, ActionCallError>>;

    /// A provider whose one call runs until it hears it stop, recording the stop.
    #[derive(Clone, Default)]
    struct Waiting {
        asked: Arc<Mutex<Vec<InvocationId>>>,
        cancels: Arc<Mutex<Vec<InvocationId>>>,
        held: Arc<Mutex<Vec<Held>>>,
    }

    impl ActionProvider for Waiting {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::default()))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.asked.lock_recover().push(request.invocation);
            let (complete, answer) = pending();
            self.held.lock_recover().push(complete);
            answer
        }

        fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(&self, _: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
            self.cancels.lock_recover().push(*id);
            Pending::ready(Ok(()))
        }
    }

    /// A connection that ends cancels the call it still waited on: one that outlives the
    /// connection that began it is told it stopped, rather than answered nobody.
    #[test]
    fn a_connection_that_ends_cancels_the_call_it_waited_on() {
        let provider = Waiting::default();
        let caller = Caller::local_app("notes").in_workspace("w");
        let (near, far) = UnixStream::pair().unwrap();
        let output = far.try_clone().unwrap();
        let serving = provider.clone();
        std::thread::spawn(move || {
            serve_lines(
                move |_| Ok((serving.clone(), caller.clone())),
                &HostedContentPuts::default(),
                None,
                BufReader::new(far),
                output,
                Duration::ZERO,
            );
        });
        let client = LineClient::of_stream(near).unwrap();
        let mut request = InvokeRequest::plain(target(), serde_json::Map::new());
        request.wait_ms = Some(0);
        let invocation = request.invocation;
        let pending = client.invoke(&Caller::local_person("ada"), request);
        tilo_test_env::wait_until("the provider to hold the call", || {
            provider.asked.lock_recover().contains(&invocation)
        });
        drop(client);
        tilo_test_env::wait_until("the call to be cancelled", || {
            provider.cancels.lock_recover().contains(&invocation)
        });
        drop(pending);
    }
}

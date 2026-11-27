//! The line wire's client: one connection to a provider that answers frames a line each, as an
//! [`ActionProvider`] of its own, so whatever walks a provider walks the one at the other end.
//! A call goes out in the workspace its caller names ([`Caller::workspace`]), which the other end
//! answers it in where it serves more than one.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;

use super::frame_line_in;
use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::api::{ApiBytes, ApiHead, ApiRequest, MAX_API_BODY};
use tilo_actions::remote::audio_output::{
    self, Request as AudioOutputRequest, Snapshot as AudioSnapshot,
};
use tilo_actions::remote::call::{BridgedCall, CallAudio, CallReport, CallStatus};
use tilo_actions::remote::native_media::{
    self, Request as CallMediaRequest, Snapshot as CallSnapshot,
};
use tilo_actions::remote::shell::{ShellBytes, ShellOpen, ShellOpened, lost_report};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ChangeFeed, ChangePage,
    Completer, ContentGetAnswer, ContentGets, ContentPiece, ContentPut, ContentPutAnswer,
    ContentPutStep, ContentPuts, InvocationReceipt, InvocationState, InvokeRequest,
    MAX_CONTENT_FRAME_BYTES, MAX_HAVE_HASHES, MAX_PUT_MANY_BYTES, MAX_PUT_MANY_FILES,
    MAX_REMOTE_ACTION_FRAME_BYTES, MAX_REMOTE_WAIT, Pending, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget, ResourceReadRequest, ResourceReads, UpdateMark,
    WatchScope, pending,
};

/// How long past the longest remote wait an answer is waited for before the ask is given up.
const SLACK: Duration = Duration::from_secs(30);

type Answered<T> = Result<T, ActionCallError>;
type Asked = Completer<Answered<RemoteActionMessage>>;

/// One connection to a provider over the line wire. Every request names what its answer
/// answers (a correlation, or the invocation it is about), so requests go out at once from any
/// thread and one reader hands each answer to whoever waits for it. Who the caller is the other
/// end decides from the connection, never from the [`Caller`] a method is handed: of the caller
/// only the workspace it names goes with the request.
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
    #[cfg(any(unix, windows))]
    stream: Option<tilo_socket::Stream>,
}

#[cfg(any(unix, windows))]
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
    /// The frames the workspace's call said that the app has not taken yet, the newest
    /// [`MAX_SAID`] of them: live audio is never queued past that.
    said: VecDeque<CallAudio>,
    /// The device's latest word on the outside call, until the app takes it.
    asked: Option<CallStatus>,
    /// Answers to the app's own call commands, by correlation, until it takes them: what an app
    /// that sends its commands from its frame loop reads instead of waiting for each.
    media: VecDeque<(u64, Result<CallSnapshot, ActionCallError>)>,
    /// What the workspace's live session heard from the other devices, the newest
    /// [`MAX_HEARD`] of them, until the app's views take them.
    presence: VecDeque<String>,
    /// Where each open shell's output goes, by its id.
    shells: HashMap<u64, std::sync::mpsc::Sender<ShellHeard>>,
    /// The output of a shell the device just opened, until its opener takes it: kept from the
    /// answer on, so nothing the shell printed before is lost.
    opened: HashMap<u64, std::sync::mpsc::Receiver<ShellHeard>>,
    /// Where the body of each API answer still coming goes, by its request's correlation.
    api: HashMap<u64, std::sync::mpsc::Sender<ApiHeard>>,
}

/// What came of an API answer's body after its head: a piece, and last its end. A receiver that
/// finds its sender gone without the end lost the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiHeard {
    Piece(Vec<u8>),
    End,
}

/// How long an API answer's head is waited for: the API itself answers a call it waited on for
/// two minutes as unanswered, which this leaves room to hear.
const API_ANSWER_WAIT: Duration = Duration::from_secs(150);

/// What one of the app's shells said: output, and last how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellHeard {
    Output(Vec<u8>),
    /// The device's report of how it ended, empty when the shell exited.
    Ended(String),
}

/// How many of the workspace's frames wait for an app that has not taken them: a second of a
/// call's voice. Older ones are dropped, never played late.
const MAX_SAID: usize = 50;

/// How many answers to an app's own call commands wait for it to take them. An app holds one
/// watch and one picture collection outstanding, so older answers are a bug of its own.
const MAX_MEDIA_ANSWERS: usize = 32;

/// How many words heard from the workspace's other devices wait for the app's views to take
/// them: presence is visual and short-lived, and a view that falls this far behind reads the
/// current layout anyway.
const MAX_HEARD: usize = 64;

/// How long a presence word's answer is waited for: a word is sent from the app's frame loop,
/// and a device that does not answer it must not hold the frame.
const PRESENCE_ANSWERED_WITHIN: Duration = Duration::from_secs(5);

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
                #[cfg(any(unix, windows))]
                stream: None,
            },
        )
    }

    /// A client over the local socket connection `stream` (a Unix socket, or a named pipe on
    /// Windows), which it shuts when its last clone goes.
    ///
    /// # Errors
    /// The connection cannot be read.
    #[cfg(any(unix, windows))]
    pub fn of_stream(stream: tilo_socket::Stream) -> std::io::Result<Self> {
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

    /// A client of the provider listening at the local socket `path`.
    ///
    /// # Errors
    /// Nothing listens there, or the connection cannot be read.
    #[cfg(any(unix, windows))]
    pub fn connect(path: &std::path::Path) -> std::io::Result<Self> {
        Self::of_stream(tilo_socket::connect(path)?)
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
        self.ask_in(key, message, None)
    }

    /// Send `message` asked in `workspace` (none: the other end's own), its answer found by
    /// `key`.
    fn ask_in(
        &self,
        key: Key,
        message: RemoteActionMessage,
        workspace: Option<&str>,
    ) -> Pending<Answered<RemoteActionMessage>> {
        self.ask_with_body_in(key, message, workspace, &[])
    }

    /// [`Self::ask_in`], writing `body` after the frame as one write of the connection: what a
    /// sound output write carries its PCM as.
    fn ask_with_body_in(
        &self,
        key: Key,
        message: RemoteActionMessage,
        workspace: Option<&str>,
        body: &[u8],
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
        let line = frame_line_in(message, workspace);
        let sent = {
            let mut writer = self.0.writer.lock_recover();
            writeln!(writer, "{line}")
                .and_then(|()| writer.write_all(body))
                .and_then(|()| writer.flush())
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
        self.asked_in(key, message, None)
    }

    /// [`Self::ask_in`], waited for.
    fn asked_in(
        &self,
        key: Key,
        message: RemoteActionMessage,
        workspace: Option<&str>,
    ) -> Answered<RemoteActionMessage> {
        self.ask_in(key, message, workspace)
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

    /// `command` (`collab_types::store::wire::Command`, as JSON) to the records of the app this
    /// connection is, in `workspace`, waited for: the store's `Reply`, as JSON. A record it founds
    /// is of `kind`. The other end knows the app by the connection; a workspace other than the
    /// one it acts in is refused.
    ///
    /// # Errors
    /// The store refused the command, or the connection did not answer.
    pub fn store(
        &self,
        workspace: &str,
        command: serde_json::Value,
        kind: Option<&str>,
    ) -> Answered<serde_json::Value> {
        // Room for the frame around it: a command too large to send is refused here, never sent
        // as a line the other end reads as no frame.
        let size = serde_json::to_vec(&command).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_REMOTE_ACTION_FRAME_BYTES.saturating_sub(1024) {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("a store command of {size} bytes does not fit the connection"),
            });
        }
        let correlation = self.correlation();
        match self.asked_in(
            Key::Correlation(correlation),
            RemoteActionMessage::StoreRequest {
                correlation,
                command,
                kind: kind.map(str::to_owned),
            },
            Some(workspace),
        )? {
            RemoteActionMessage::StoreResponse { result, .. } => result,
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

/// A record read goes to the other end as it was asked, which decides it as whoever connected:
/// what a client of a device's dispatch reads records through.
impl ResourceReads for LineClient {
    fn answer_read(
        self: Arc<Self>,
        _caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer {
        let ours = self.correlation();
        let asked = self
            .ask(
                Key::Correlation(ours),
                RemoteActionMessage::ResourceRead {
                    correlation: ours,
                    on_behalf_of: None,
                    read,
                },
            )
            .map(move |answer| RemoteActionMessage::ResourceReading {
                correlation,
                result: match answer {
                    Ok(RemoteActionMessage::ResourceReading { result, .. }) => result,
                    Ok(other) => Err(unexpected(&other)),
                    Err(error) => Err(error),
                },
            });
        Box::pin(async move {
            asked.await.unwrap_or(RemoteActionMessage::ResourceReading {
                correlation,
                result: Err(ActionCallError::Unanswered),
            })
        })
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
    // A shell carried on a connection that ended was lost, not exited.
    let lost = match why {
        ActionCallError::Unavailable { reason } | ActionCallError::Refused { reason } => {
            lost_report(reason)
        }
        other => lost_report(&other.to_string()),
    };
    for (_, shell) in waiting.shells.drain() {
        let _ = shell.send(ShellHeard::Ended(lost.clone()));
    }
    // An answer whose body was still coming ends without its end: its reader hears it was lost.
    waiting.api.clear();
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
        RemoteActionMessage::CallSaid(frame) => {
            if waiting.said.len() >= MAX_SAID {
                waiting.said.pop_front();
            }
            waiting.said.push_back(frame.clone());
            None
        }
        RemoteActionMessage::CallStatus(status) => {
            waiting.asked = Some(status.clone());
            None
        }
        RemoteActionMessage::CallMediaResult {
            correlation,
            result,
        } => {
            if let Some(asked) = waiting.by_correlation.remove(correlation) {
                Some(asked)
            } else {
                if waiting.media.len() >= MAX_MEDIA_ANSWERS {
                    waiting.media.pop_front();
                }
                waiting.media.push_back((*correlation, result.clone()));
                None
            }
        }
        RemoteActionMessage::PresenceHeard { heard } => {
            if waiting.presence.len() >= MAX_HEARD {
                waiting.presence.pop_front();
            }
            waiting.presence.push_back(heard.clone());
            None
        }
        RemoteActionMessage::ShellOutput(ShellBytes { shell, bytes }) => {
            if let Some(heard) = waiting.shells.get(shell) {
                let _ = heard.send(ShellHeard::Output(bytes.clone()));
            }
            None
        }
        RemoteActionMessage::ShellEnded(ended) => {
            if let Some(heard) = waiting.shells.remove(&ended.shell) {
                let _ = heard.send(ShellHeard::Ended(ended.report.clone()));
            }
            None
        }
        RemoteActionMessage::ApiBody(_)
        | RemoteActionMessage::ApiEnd { .. }
        | RemoteActionMessage::ApiResponse { .. } => api_answered(&mut waiting, &message),
        RemoteActionMessage::ShellOpenResponse {
            correlation,
            result,
        } => {
            if let Ok(opened) = result {
                let (heard, hearing) = std::sync::mpsc::channel();
                waiting.shells.insert(opened.shell, heard);
                waiting.opened.insert(opened.shell, hearing);
            }
            waiting.by_correlation.remove(correlation)
        }
        RemoteActionMessage::CatalogueResponse { correlation, .. }
        | RemoteActionMessage::CallCarryResponse { correlation, .. }
        | RemoteActionMessage::CallReportResponse { correlation, .. }
        | RemoteActionMessage::AudioOutputResponse { correlation, .. }
        | RemoteActionMessage::ResourceReading { correlation, .. }
        | RemoteActionMessage::ContentPutResponse { correlation, .. }
        | RemoteActionMessage::ContentGetResponse { correlation, .. }
        | RemoteActionMessage::StoreResponse { correlation, .. }
        | RemoteActionMessage::PresenceSaidResult { correlation, .. }
        | RemoteActionMessage::ChangesResponse { correlation, .. }
        | RemoteActionMessage::ChangedResponse { correlation, .. }
        | RemoteActionMessage::ShownActionsResponse { correlation, .. }
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

/// Hand an API answer's head to whoever asked, or a piece of its body to whoever reads it.
fn api_answered(waiting: &mut Waiting, message: &RemoteActionMessage) -> Option<Asked> {
    match message {
        RemoteActionMessage::ApiBody(ApiBytes { correlation, bytes }) => {
            if let Some(heard) = waiting.api.get(correlation) {
                let _ = heard.send(ApiHeard::Piece(bytes.clone()));
            }
            None
        }
        RemoteActionMessage::ApiEnd { correlation } => {
            if let Some(heard) = waiting.api.remove(correlation) {
                let _ = heard.send(ApiHeard::End);
            }
            None
        }
        RemoteActionMessage::ApiResponse {
            correlation,
            result,
        } => {
            if result.is_err() {
                waiting.api.remove(correlation);
            }
            waiting.by_correlation.remove(correlation)
        }
        _ => None,
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
    fn query(&self, caller: &Caller, query: CatalogueQuery) -> Pending<Answered<CataloguePage>> {
        let correlation = self.correlation();
        self.ask_in(
            Key::Correlation(correlation),
            RemoteActionMessage::CatalogueRequest { correlation, query },
            caller.workspace(),
        )
        .map(|answer| match answer? {
            RemoteActionMessage::CatalogueResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Answered<InvocationReceipt>> {
        let invocation = request.invocation;
        self.ask_in(
            Key::Receipt(invocation),
            RemoteActionMessage::Invoke(request),
            caller.workspace(),
        )
        .map(|answer| match answer? {
            RemoteActionMessage::Receipt(receipt) => Ok(receipt),
            other => Err(unexpected(&other)),
        })
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let correlation = self.correlation();
        let state = match self
            .asked_in(
                Key::Correlation(correlation),
                RemoteActionMessage::Observe {
                    correlation,
                    invocation: *id,
                    wait_ms: None,
                    seen: None,
                },
                caller.workspace(),
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
    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
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
            self.ask_in(
                Key::Correlation(correlation),
                RemoteActionMessage::Observe {
                    correlation,
                    invocation: *id,
                    wait_ms: Some(wait_ms),
                    seen: Some(seen),
                },
                caller.workspace(),
            )
            .map(move |answer| {
                if let Ok(RemoteActionMessage::Update { state, .. }) = answer {
                    this.saw(state.as_ref());
                }
            }),
        )
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Answered<()>> {
        self.ask_in(
            Key::Cancel(*id),
            RemoteActionMessage::Cancel { invocation: *id },
            caller.workspace(),
        )
        .map(|answer| match answer? {
            RemoteActionMessage::CancelResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        })
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::new(self.clone()))
    }

    /// This connection's puts for the action `target` names, so a host that walks a provider at
    /// the other end of it passes the action on.
    fn content_puts_for(
        &self,
        target: Option<&RemoteActionTarget>,
    ) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::new(TargetedPuts {
            client: self.clone(),
            target: target.cloned(),
        }))
    }

    fn content_gets(&self) -> Option<Arc<dyn ContentGets>> {
        Some(Arc::new(self.clone()))
    }

    fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        Some(Arc::new(self.clone()))
    }

    /// The other end's own feed of the changes to the records of the caller's workspace.
    fn change_feed(&self) -> Option<Arc<dyn ChangeFeed>> {
        Some(Arc::new(self.clone()))
    }

    /// Resolves once the other end says what `scope` follows may have changed.
    fn changed(&self, caller: &Caller, scope: WatchScope) -> Option<Pending<Answered<()>>> {
        let correlation = self.correlation();
        Some(
            self.ask_in(
                Key::Correlation(correlation),
                RemoteActionMessage::ChangedRequest { correlation, scope },
                caller.workspace(),
            )
            .map(|answer| match answer? {
                RemoteActionMessage::ChangedResponse { result, .. } => result,
                other => Err(unexpected(&other)),
            }),
        )
    }

    /// What the other end says of the actions a window shows, asked as this connection: the
    /// device decides each with the ordinary answer in the workspace that keeps its record, and
    /// answers again past `ask.after` when the grants change.
    fn shown_actions(
        &self,
        caller: &Caller,
        ask: tilo_actions::shown::ShownAsk,
    ) -> Pending<Answered<Vec<tilo_actions::shown::ActionDecision>>> {
        let correlation = self.correlation();
        self.ask_in(
            Key::Correlation(correlation),
            RemoteActionMessage::ShownActionsRequest {
                correlation,
                actions: ask.actions,
                after: ask.after,
                wait_ms: ask.wait_ms,
            },
            caller.workspace(),
        )
        .map(|answer| match answer? {
            RemoteActionMessage::ShownActionsResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        })
    }
}

impl ChangeFeed for LineClient {
    fn since(&self, caller: &Caller, after: Option<&str>) -> Pending<Answered<ChangePage>> {
        let correlation = self.correlation();
        self.ask_in(
            Key::Correlation(correlation),
            RemoteActionMessage::ChangesRequest {
                correlation,
                after: after.map(str::to_owned),
            },
            caller.workspace(),
        )
        .map(|answer| match answer? {
            RemoteActionMessage::ChangesResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        })
    }
}

/// One connection's content puts for the action `target` names
/// ([`ActionProvider::content_puts_for`]).
struct TargetedPuts {
    client: LineClient,
    target: Option<RemoteActionTarget>,
}

impl ContentPuts for TargetedPuts {
    fn begin(&self, _caller: &Caller, size: u64) -> Answered<Box<dyn ContentPut>> {
        Ok(Box::new(
            self.client.begin_put_for(size, self.target.clone())?,
        ))
    }

    fn put_many(&self, _caller: &Caller, files: &[Vec<u8>]) -> Answered<Vec<String>> {
        self.client.put_many_for(files, self.target.as_ref())
    }

    fn have(&self, caller: &Caller, hashes: &[String]) -> Answered<Vec<bool>> {
        self.client.have(caller, hashes)
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
        self.begin_put_for(size, None)
    }

    /// Open an upload of `size` bytes for the action `target` names: the store the provider that
    /// answers that action keeps, which is the device's own one for an action of the caller's own
    /// device.
    ///
    /// # Errors
    /// The other end refused it, or stopped answering.
    pub fn begin_put_for(
        &self,
        size: u64,
        target: Option<RemoteActionTarget>,
    ) -> Answered<LinePut> {
        match self.put_step(ContentPutStep::Begin { size, target })? {
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

/// The call lane an app's connection carries ([`tilo_actions::remote::call`]): the app says which outside
/// call it carries into which workspace call, who is in it, and what its people say; the device
/// hands back what the workspace's call says and where it wants the app. A connection that is no
/// app's, or one whose app the workspace grants no call, is refused with the reason.
impl LineClient {
    /// Carry `pair` from now on, or let go of the call carried (`None`).
    ///
    /// # Errors
    /// The device refused it (the app may not carry that call, or none at all), or stopped
    /// answering.
    pub fn carry_call(&self, pair: Option<BridgedCall>) -> Answered<()> {
        let correlation = self.correlation();
        match self.asked(
            Key::Correlation(correlation),
            RemoteActionMessage::CallCarryRequest { correlation, pair },
        )? {
            RemoteActionMessage::CallCarryResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        }
    }

    /// Say who is in the outside calls and where the app stands in them.
    ///
    /// # Errors
    /// The report is not one an app may make, the connection carries no call lane, or the other
    /// end stopped answering.
    pub fn report_call(&self, report: CallReport) -> Answered<()> {
        let correlation = self.correlation();
        match self.asked(
            Key::Correlation(correlation),
            RemoteActionMessage::CallReport {
                correlation,
                report,
            },
        )? {
            RemoteActionMessage::CallReportResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        }
    }

    /// Hand the workspace's call one frame an outside person said. Nothing answers it: a frame
    /// the call does not take is dropped there, as live audio is.
    ///
    /// # Errors
    /// The connection closed.
    pub fn hear_call(&self, frame: CallAudio) -> Answered<()> {
        let line = frame_line_in(RemoteActionMessage::CallHeard(frame), None);
        let mut writer = self.0.writer.lock_recover();
        writeln!(writer, "{line}")
            .and_then(|()| writer.flush())
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the connection closed: {error}"),
            })
    }

    /// The frames the workspace's call said since the last time, oldest first.
    #[must_use]
    pub fn said_call(&self) -> Vec<CallAudio> {
        self.0.waiting.lock_recover().said.drain(..).collect()
    }

    /// Where the device last asked the app to be in the outside call, and the line to show its
    /// people there, once: `None` when it asked nothing new since.
    #[must_use]
    pub fn asked_call(&self) -> Option<CallStatus> {
        self.0.waiting.lock_recover().asked.take()
    }
}

/// An app's sound output on its device ([`tilo_actions::remote::audio_output`]): the commands its media
/// stack sends, and the output's state as the device answers. Every command waits at most
/// [`audio_output::DEADLINE`]; a write past a full device queue is answered with the sequence
/// still accepted, which the caller retries.
impl LineClient {
    /// One command of an app's sound output, waited for.
    ///
    /// # Errors
    /// The device refused it, the connection ended, or no answer came in time.
    pub fn audio_output(&self, command: AudioOutputRequest) -> Answered<AudioSnapshot> {
        let correlation = self.correlation();
        self.audio_output_answer(
            Key::Correlation(correlation),
            RemoteActionMessage::AudioOutputRequest {
                correlation,
                command,
            },
            &[],
        )
    }

    /// One write of PCM to the open output `generation`. The samples travel as the frame's
    /// binary body, little-endian, 20 ms at most.
    ///
    /// # Errors
    /// The samples are past one write's bound, the device refused them, the connection ended, or
    /// no answer came in time.
    pub fn audio_output_write(
        &self,
        generation: u64,
        sequence: u64,
        samples: &[i16],
    ) -> Answered<AudioSnapshot> {
        if samples.is_empty()
            || samples.len() > audio_output::MAX_SAMPLES
            || !samples
                .len()
                .is_multiple_of(usize::from(audio_output::CHANNELS))
        {
            return Err(ActionCallError::Refused {
                reason: "invalid audio output write".into(),
            });
        }
        let body = audio_output::body_of(samples);
        let correlation = self.correlation();
        self.audio_output_answer(
            Key::Correlation(correlation),
            RemoteActionMessage::AudioOutputRequest {
                correlation,
                command: AudioOutputRequest::Write {
                    generation,
                    sequence,
                    bytes: u32::try_from(body.len()).map_err(|_| ActionCallError::Refused {
                        reason: "invalid audio output write".into(),
                    })?,
                },
            },
            &body,
        )
    }

    /// One command of an app's sound output, sent without waiting for its answer: what pause,
    /// flush, volume and close are, which the app never holds its decoder for.
    ///
    /// # Errors
    /// The connection ended.
    pub fn audio_output_send(&self, command: AudioOutputRequest) -> Answered<()> {
        let line = frame_line_in(
            RemoteActionMessage::AudioOutputRequest {
                correlation: 0,
                command,
            },
            None,
        );
        let mut writer = self.0.writer.lock_recover();
        writeln!(writer, "{line}")
            .and_then(|()| writer.flush())
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the connection closed: {error}"),
            })
    }

    /// The device's answer to `message`, whose binary `body` follows it, waited for.
    fn audio_output_answer(
        &self,
        key: Key,
        message: RemoteActionMessage,
        body: &[u8],
    ) -> Answered<AudioSnapshot> {
        let answered = self
            .ask_with_body_in(key, message, None, body)
            .wait_timeout(audio_output::DEADLINE)
            .ok()
            .flatten()
            .unwrap_or(Err(ActionCallError::Unanswered))?;
        match answered {
            RemoteActionMessage::AudioOutputResponse { result, .. } => result,
            other => Err(unexpected(&other)),
        }
        .and_then(valid_output)
    }
}

/// The call an app takes part in itself ([`tilo_actions::remote::native_media`]): joining one of its
/// workspace's calls, muting, the camera and screen it sends, the pictures its view draws, and
/// the status it shows. The device's call service binds every command to the connection's app,
/// its workspace and its grants; a connection that carries no call lane, or one whose app the
/// workspace grants no call, is refused with the reason.
impl LineClient {
    /// One command of the app's own call, waited for as long as the device may hold it.
    ///
    /// # Errors
    /// The device refused it, the connection ended, or the answer did not come in time.
    pub fn call_media(&self, request: CallMediaRequest) -> Answered<CallSnapshot> {
        let correlation = self.correlation();
        let held = native_media::lost(&request);
        match self
            .ask(
                Key::Correlation(correlation),
                RemoteActionMessage::CallMediaRequest {
                    correlation,
                    request,
                },
            )
            .wait_timeout(held)
            .ok()
            .flatten()
            .unwrap_or(Err(ActionCallError::Unanswered))?
        {
            RemoteActionMessage::CallMediaResult { result, .. } => result,
            other => Err(unexpected(&other)),
        }
    }

    /// Send one command of the app's own call without waiting for its answer: the correlation
    /// this answers is what [`Self::call_media_answers`] hands its answer back by.
    ///
    /// # Errors
    /// The connection ended.
    pub fn call_media_ask(&self, request: CallMediaRequest) -> Answered<u64> {
        let correlation = self.correlation();
        let line = frame_line_in(
            RemoteActionMessage::CallMediaRequest {
                correlation,
                request,
            },
            None,
        );
        let sent = {
            let mut writer = self.0.writer.lock_recover();
            writeln!(writer, "{line}").and_then(|()| writer.flush())
        };
        sent.map_err(|error| ActionCallError::Unavailable {
            reason: format!("the connection closed: {error}"),
        })?;
        Ok(correlation)
    }

    /// The answers to the commands sent with [`Self::call_media_ask`] that came since the last
    /// time, oldest first.
    #[must_use]
    pub fn call_media_answers(&self) -> Vec<(u64, Answered<CallSnapshot>)> {
        self.0.waiting.lock_recover().media.drain(..).collect()
    }
}

/// The app's own presence in its workspace ([`crate::remote::RemoteActionMessage::PresenceSaid`]):
/// what its views say of their person — pointers, carets, selections, who is composing, a
/// workspace status, the steps of an action as they run — said by the device in the workspace's
/// live session for the connection's app, workspace and member, so the person's other devices
/// hear it exactly as the shell's own words go.
impl LineClient {
    /// Say one word of the app's person: `said`, a `collab_types::presence::Said` as JSON.
    ///
    /// # Errors
    /// The device refused the word (and said nothing), the connection ended, or the device did
    /// not answer in time.
    pub fn say_presence(&self, said: &serde_json::Value) -> Answered<()> {
        let correlation = self.correlation();
        let answer = self.ask(
            Key::Correlation(correlation),
            RemoteActionMessage::PresenceSaid {
                correlation,
                said: said.to_string(),
            },
        );
        match answer
            .wait_timeout(PRESENCE_ANSWERED_WITHIN)
            .ok()
            .flatten()
            .unwrap_or(Err(ActionCallError::Unanswered))?
        {
            RemoteActionMessage::PresenceSaidResult { result, .. } => result,
            other => Err(unexpected(&other)),
        }
    }

    /// What the device's live session heard in the app's workspace since the last look, each a
    /// `collab_types::presence::Heard` as JSON, oldest first.
    pub fn presence_heard(&self) -> Vec<String> {
        self.0.waiting.lock_recover().presence.drain(..).collect()
    }
}

/// A sound output snapshot that is one of the bounds' own.
fn valid_output(snapshot: AudioSnapshot) -> Answered<AudioSnapshot> {
    if snapshot.valid() {
        Ok(snapshot)
    } else {
        Err(ActionCallError::Refused {
            reason: "invalid audio output state".into(),
        })
    }
}

/// The shell lane an app's own connection carries ([`tilo_actions::remote::shell`]): the device opens a
/// shell its shell runtime runs, keeps its terminal, and carries its bytes here both ways.
impl LineClient {
    /// Open a shell as `open` asks: its id and what the runtime said of it, and where its
    /// output arrives, last with how it ended.
    ///
    /// # Errors
    /// The device refused it (the app may not open shells, the runtime said no), or stopped
    /// answering.
    pub fn open_shell(
        &self,
        open: ShellOpen,
    ) -> Answered<(ShellOpened, std::sync::mpsc::Receiver<ShellHeard>)> {
        let correlation = self.correlation();
        match self.asked(
            Key::Correlation(correlation),
            RemoteActionMessage::ShellOpenRequest { correlation, open },
        )? {
            RemoteActionMessage::ShellOpenResponse { result, .. } => {
                let opened = result?;
                let hearing = self
                    .0
                    .waiting
                    .lock_recover()
                    .opened
                    .remove(&opened.shell)
                    .ok_or(ActionCallError::Unanswered)?;
                Ok((opened, hearing))
            }
            other => Err(unexpected(&other)),
        }
    }

    /// Type `bytes` into the shell `shell`.
    ///
    /// # Errors
    /// The connection closed.
    pub fn type_into_shell(&self, shell: u64, bytes: &[u8]) -> Answered<()> {
        for piece in bytes.chunks(tilo_actions::remote::shell::MAX_SHELL_BYTES) {
            self.say(RemoteActionMessage::ShellInput(ShellBytes {
                shell,
                bytes: piece.to_vec(),
            }))?;
        }
        Ok(())
    }

    /// The shell `shell`'s window is now `cols` by `rows` cells.
    ///
    /// # Errors
    /// The connection closed.
    pub fn resize_shell(&self, shell: u64, cols: u16, rows: u16) -> Answered<()> {
        self.say(RemoteActionMessage::ShellResize { shell, cols, rows })
    }

    /// Let the shell `shell` go: the device ends its session.
    ///
    /// # Errors
    /// The connection closed.
    pub fn close_shell(&self, shell: u64) -> Answered<()> {
        self.0.waiting.lock_recover().shells.remove(&shell);
        self.say(RemoteActionMessage::ShellClose { shell })
    }

    /// Have the other end answer `method` on `target` (a `/v1` path with its query) with
    /// `headers` and `body`, as its API answers whoever this connection is: the answer's status
    /// and headers, and where its body arrives, piece by piece and last its end.
    ///
    /// # Errors
    /// The body is over one request's bound, the other end carries no API, the connection
    /// ended, or no answer came in time.
    pub fn api_request(
        &self,
        method: &str,
        target: &str,
        headers: Vec<(String, String)>,
        body: &[u8],
    ) -> Answered<(ApiHead, std::sync::mpsc::Receiver<ApiHeard>)> {
        let size = u32::try_from(body.len())
            .ok()
            .filter(|size| *size as usize <= MAX_API_BODY)
            .ok_or_else(|| ActionCallError::InvalidRequest {
                reason: format!(
                    "a request carries at most {MAX_API_BODY} bytes; put a larger file in pieces"
                ),
            })?;
        let correlation = self.correlation();
        let (heard, hearing) = std::sync::mpsc::channel();
        self.0.waiting.lock_recover().api.insert(correlation, heard);
        let request = ApiRequest {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
            body: size,
        };
        let answer = self
            .ask_with_body_in(
                Key::Correlation(correlation),
                RemoteActionMessage::ApiRequest {
                    correlation,
                    request,
                },
                None,
                body,
            )
            .wait_timeout(API_ANSWER_WAIT)
            .ok()
            .flatten()
            .unwrap_or(Err(ActionCallError::Unanswered));
        match answer {
            Ok(RemoteActionMessage::ApiResponse { result, .. }) => Ok((result?, hearing)),
            Ok(other) => {
                self.0.waiting.lock_recover().api.remove(&correlation);
                Err(unexpected(&other))
            }
            Err(error) => {
                self.0.waiting.lock_recover().api.remove(&correlation);
                Err(error)
            }
        }
    }

    /// Send `message`, which nothing answers.
    fn say(&self, message: RemoteActionMessage) -> Answered<()> {
        let line = frame_line_in(message, None);
        let mut writer = self.0.writer.lock_recover();
        writeln!(writer, "{line}")
            .and_then(|()| writer.flush())
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the connection closed: {error}"),
            })
    }
}

impl ContentPuts for LineClient {
    fn begin(&self, _caller: &Caller, size: u64) -> Answered<Box<dyn ContentPut>> {
        Ok(Box::new(self.begin_put(size)?))
    }

    fn put_many(&self, _caller: &Caller, files: &[Vec<u8>]) -> Answered<Vec<String>> {
        self.put_many_for(files, None)
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

impl LineClient {
    /// [`ContentPuts::put_many`] for the action `target` names, as [`Self::begin_put_for`] does.
    ///
    /// # Errors
    /// What [`Self::begin_put`] or the other end's store refused.
    pub fn put_many_for(
        &self,
        files: &[Vec<u8>],
        target: Option<&RemoteActionTarget>,
    ) -> Answered<Vec<String>> {
        let mut contents = Vec::with_capacity(files.len());
        let mut group: Vec<String> = Vec::new();
        let mut bytes = 0_usize;
        let send = |group: &mut Vec<String>, contents: &mut Vec<String>| {
            let files = std::mem::take(group);
            let count = files.len();
            match self.put_step(ContentPutStep::Many {
                files,
                target: target.cloned(),
            })? {
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
                let mut put = self.begin_put_for(
                    u64::try_from(file.len()).unwrap_or(u64::MAX),
                    target.cloned(),
                )?;
                put.append(0, file)?;
                contents.push(Box::new(put).finish()?);
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

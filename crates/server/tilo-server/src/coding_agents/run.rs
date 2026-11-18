//! One run of a coding agent in a workspace's machine, followed by its host.
//!
//! The harness runs in the machine as a served run ([`ush::served_runs`]).
//! This is its host's half: it answers what the run asks at its endpoints,
//! its model by the person's own ([`super::model_endpoint`]) and Tilo's MCP
//! server by the sub-agent's own caller, reads what the harness prints into
//! the sub-agent's conversation as it prints it, and carries a message for
//! it to the harness's input. Nothing here bounds how long a run lasts: it
//! is followed until it is over.
//!
//! The person's key is never here either: the model is asked through
//! [`PersonModel`], which reads the key where the provider is called. What
//! this file sends into the machine is the model's answers and the MCP
//! server's, and what it puts into the conversation is what the harness said.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tilo_lock::MutexRecover as _;
use ush::served_runs::wire::{Asked, FromRun, MAX_BODY_CHUNK, ToRun, VERSION};
use ush::served_runs::{Heard, Over, ServedRun};

use super::harness::{Harness, MODEL, ModelProtocol, Says, TILO};
use super::model_endpoint::{self, Answer, Wanted};
use crate::workspaces::person_model::{Interaction, PersonModel};

/// How often a run that says nothing is looked at.
const LOOK: Duration = Duration::from_millis(200);
/// How long one answer of the person's model may take.
const MODEL_TIME: Duration = Duration::from_secs(10 * 60);
/// How long a harness waits for the head of a streamed answer before the
/// host opens the stream and keeps it alive: an answer or a refusal that
/// comes sooner goes out whole, under its own status.
const HEAD_WAIT: Duration = Duration::from_secs(2);
/// How often an open stream is kept alive while its answer is made.
const PING_EVERY: Duration = Duration::from_secs(10);
/// The most of what a run printed beside its protocol that is kept for the
/// words of a failure.
const BESIDE_KEPT: usize = 4 * 1024;
/// The longest line of a harness's output that is read; a longer one is no
/// line of its protocol.
const MAX_LINE: usize = 8 * 1024 * 1024;

/// The connection to a served run, as its host holds it.
pub trait Machine: Send + Sync {
    /// Send `frame` to the run; `false` once it is over or let go of.
    fn tell(&self, frame: &ToRun) -> bool;
    /// What the run says next, waited for at most `wait`.
    fn hear(&self, wait: Duration) -> Option<Heard>;
    /// End the run now, without asking its program.
    fn cut(&self);
}

impl Machine for ServedRun {
    fn tell(&self, frame: &ToRun) -> bool {
        Self::tell(self, frame)
    }

    fn hear(&self, wait: Duration) -> Option<Heard> {
        Self::hear(self, wait)
    }

    fn cut(&self) {
        Self::cut(self);
    }
}

/// Where a coding agent's words go: the sub-agent's own conversation and
/// record, written as the sub-agent.
pub trait Conversation: Send + Sync {
    /// The harness's own id for this conversation, to take it up later.
    fn session(&self, id: &str);
    /// It said `words`: a message of its own.
    fn says(&self, words: &str);
    /// What it is doing now, in a few words.
    fn doing(&self, what: &str);
    /// It finished what it was asked, or could not and says why; it stays
    /// ready for the next message.
    fn finished(&self, ok: bool, words: &str);
    /// The run is over.
    fn over(&self, ended: &Ended);
}

/// How a run came to its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The harness ended by itself, having done what it was asked.
    Finished,
    /// It was stopped.
    Stopped,
    /// It could not start, or ended saying it failed.
    Failed,
    /// Its machine or its connection went away under it.
    Lost,
}

impl Ending {
    /// The word a record keeps for it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Lost => "lost",
        }
    }
}

/// A run that is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub how: Ending,
    /// The harness's exit status, when it ran.
    pub exit: Option<i32>,
    /// One plain line of why, empty for a run that finished.
    pub words: String,
    /// What its model calls cost, in millionths of the currency.
    pub spent_micros: u64,
}

/// One JSON-RPC message to Tilo's MCP server and its answer, none for a
/// message that asks for none.
pub type Tilo = dyn Fn(Value) -> Option<Value> + Send + Sync;

/// What answers a run's endpoints, on the host.
#[derive(Clone)]
pub struct Serves {
    /// The person's own model for a kind of work, asked where the work is
    /// and counted on the day of whoever the run spends as.
    pub model: Arc<dyn Fn(Interaction) -> PersonModel + Send + Sync>,
    /// Tilo's MCP server, as the sub-agent's own caller.
    pub tilo: Arc<Tilo>,
    /// The language refusals are worded in for the person.
    pub language: Option<String>,
}

/// What the threads of one run share.
struct Shared {
    machine: Arc<dyn Machine>,
    harness: Arc<dyn Harness>,
    serves: Serves,
    conversation: Arc<dyn Conversation>,
    /// Its person, or the agent that started it, asked it to stop.
    stopping: AtomicBool,
    /// It is over: nothing is answered any more.
    over: AtomicBool,
    spent_micros: AtomicU64,
    ended: Mutex<Option<Ended>>,
}

/// One run of a coding agent, held by its host.
pub struct CodingRun {
    shared: Arc<Shared>,
}

impl CodingRun {
    /// Follow `machine`, a served run of `harness`, to its end: answer its
    /// endpoints with `serves` and write what it says to `conversation`.
    ///
    /// # Errors
    ///
    /// The thread that follows it could not be started.
    pub fn follow(
        machine: Arc<dyn Machine>,
        harness: Arc<dyn Harness>,
        serves: Serves,
        conversation: Arc<dyn Conversation>,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            machine,
            harness,
            serves,
            conversation,
            stopping: AtomicBool::new(false),
            over: AtomicBool::new(false),
            spent_micros: AtomicU64::new(0),
            ended: Mutex::new(None),
        });
        let following = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("coding-run".into())
            .spawn(move || follow(&following))?;
        Ok(Self { shared })
    }

    /// Give the harness `text` as its next message: a note from the agent
    /// that started it, or its person's own words. It takes it at its next
    /// step. `false` once the run is over.
    pub fn message(&self, text: &str) -> bool {
        self.shared
            .machine
            .tell(&ToRun::In(self.shared.harness.message(text)))
    }

    /// Say that nothing more will be said to the harness: its input ends,
    /// and a harness that reads to the end of its input finishes what it is
    /// doing and ends by itself. `false` once the run is over.
    pub fn close(&self) -> bool {
        self.shared.machine.tell(&ToRun::CloseIn)
    }

    /// Ask the harness to end; it is ended when it has not within its grace.
    pub fn stop(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.machine.tell(&ToRun::Stop);
    }

    /// How the run ended, once it is over.
    #[must_use]
    pub fn ended(&self) -> Option<Ended> {
        self.shared.ended.lock_recover().clone()
    }

    /// What its model calls cost so far, in millionths of the currency.
    #[must_use]
    pub fn spent_micros(&self) -> u64 {
        self.shared.spent_micros.load(Ordering::SeqCst)
    }
}

impl Drop for CodingRun {
    fn drop(&mut self) {
        // A run nobody holds has no host: it ends with its handle.
        if !self.shared.over.load(Ordering::SeqCst) {
            self.shared.machine.cut();
        }
    }
}

/// What the following thread keeps between two frames.
struct Following {
    reads: Box<dyn super::harness::Reads>,
    /// What the harness printed since its last whole line.
    line: Vec<u8>,
    /// The requests whose bodies are still arriving.
    asking: HashMap<u64, (Asked, Vec<u8>)>,
    /// The requests being answered, each with what lets go of it.
    answering: HashMap<u64, Arc<AtomicBool>>,
    /// The launcher said it is up: the machine can serve a run.
    ready: bool,
    exit: Option<i32>,
    /// What the launcher said went wrong, in its words.
    failed: Option<String>,
    /// The end of what was printed beside the protocol.
    beside: String,
}

fn follow(shared: &Arc<Shared>) {
    let mut following = Following {
        reads: shared.harness.reads(),
        line: Vec::new(),
        asking: HashMap::new(),
        answering: HashMap::new(),
        ready: false,
        exit: None,
        failed: None,
        beside: String::new(),
    };
    let over = loop {
        match shared.machine.hear(LOOK) {
            None => {}
            Some(Heard::Frame(frame)) => heard(shared, &mut following, frame),
            Some(Heard::Over(over)) => break over,
        }
    };
    shared.over.store(true, Ordering::SeqCst);
    for gone in following.answering.values() {
        gone.store(true, Ordering::SeqCst);
    }
    let ended = ending(shared, &following, &over);
    shared.conversation.over(&ended);
    *shared.ended.lock_recover() = Some(ended);
}

/// Keep the end of `text` for the words of a failure.
fn keep(beside: &mut String, text: &str) {
    beside.push_str(text);
    if beside.len() > BESIDE_KEPT {
        let mut from = beside.len().saturating_sub(BESIDE_KEPT);
        while !beside.is_char_boundary(from) {
            from = from.saturating_add(1);
        }
        beside.drain(..from);
    }
}

fn heard(shared: &Arc<Shared>, following: &mut Following, frame: FromRun) {
    match frame {
        FromRun::Ready { version, .. } => {
            if version == VERSION {
                following.ready = true;
            } else {
                following.failed = Some(format!(
                    "the machine's agent serves runs in version {version} of its frames, and \
                     this host reads version {VERSION}"
                ));
                shared.machine.cut();
            }
        }
        FromRun::Started { .. } => {}
        FromRun::Out(bytes) => printed(shared, following, &bytes),
        FromRun::Err(bytes) => keep(&mut following.beside, &String::from_utf8_lossy(&bytes)),
        FromRun::Asked(asked) => {
            following.asking.insert(asked.id, (asked, Vec::new()));
        }
        FromRun::AskedBody { id, bytes, last } => {
            let Some((_, body)) = following.asking.get_mut(&id) else {
                return;
            };
            body.extend(bytes);
            if body.len() > model_endpoint::MAX_REQUEST_BYTES {
                following.asking.remove(&id);
                let too_long = model_endpoint::failure(
                    413,
                    "request_too_large",
                    "the request is larger than this endpoint reads",
                );
                whole(shared.machine.as_ref(), id, &too_long);
            } else if last && let Some((asked, body)) = following.asking.remove(&id) {
                let gone = Arc::new(AtomicBool::new(false));
                following.answering.insert(id, Arc::clone(&gone));
                let shared = Arc::clone(shared);
                let started = std::thread::Builder::new()
                    .name("coding-run-answer".into())
                    .spawn(move || answer(&shared, &asked, &body, &gone));
                if started.is_err() {
                    following.answering.remove(&id);
                }
            }
        }
        FromRun::AskedGone { id } => {
            following.asking.remove(&id);
            if let Some(gone) = following.answering.remove(&id) {
                gone.store(true, Ordering::SeqCst);
            }
        }
        FromRun::Turned { endpoint } => {
            tracing::warn!(%endpoint, "a caller that is not of the run was turned away at its endpoint");
        }
        FromRun::Failed { message } => following.failed = Some(message),
        FromRun::Exited(status) => following.exit = Some(status.shell_code()),
    }
}

/// Read what the harness printed, each whole line as it ends.
fn printed(shared: &Shared, following: &mut Following, bytes: &[u8]) {
    following.line.extend_from_slice(bytes);
    while let Some(end) = following.line.iter().position(|byte| *byte == b'\n') {
        let rest = following.line.split_off(end.saturating_add(1));
        let line = std::mem::replace(&mut following.line, rest);
        let line = String::from_utf8_lossy(&line);
        let said = following.reads.line(&line);
        if said.is_empty() && !line.trim().is_empty() {
            keep(&mut following.beside, &line);
        }
        for said in said {
            match said {
                Says::Session(id) => shared.conversation.session(&id),
                Says::Words(words) => shared.conversation.says(&words),
                Says::Doing(what) => shared.conversation.doing(&what),
                Says::Finished { ok, words } => shared.conversation.finished(ok, &words),
            }
        }
    }
    if following.line.len() > MAX_LINE {
        following.line.clear();
    }
}

/// The last line that says something of `text`.
fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

/// How the run ended, from what it said until it was over.
fn ending(shared: &Shared, following: &Following, over: &Over) -> Ended {
    let said_beside = || {
        let beside = last_line(&following.beside);
        if beside.is_empty() {
            last_line(&over.words).to_owned()
        } else {
            beside.to_owned()
        }
    };
    let (how, words) = if let Some(refusal) = &over.refusal {
        (Ending::Failed, refusal.message.clone())
    } else if !following.ready {
        // No launcher answered: the machine did not start one, or its image
        // is from before its agent served runs.
        let why = last_line(&over.words);
        (
            Ending::Failed,
            if why.is_empty() {
                "This workspace's machine cannot serve a coding agent yet: its image is older \
                 than this host."
                    .to_owned()
            } else {
                format!("This workspace's machine could not serve a coding agent: {why}")
            },
        )
    } else if let Some(failed) = &following.failed {
        (Ending::Failed, failed.clone())
    } else if shared.stopping.load(Ordering::SeqCst) {
        (Ending::Stopped, String::new())
    } else {
        match following.exit {
            Some(0) => (Ending::Finished, String::new()),
            Some(_) => (Ending::Failed, said_beside()),
            None => (Ending::Lost, said_beside()),
        }
    };
    Ended {
        how,
        exit: following.exit,
        words,
        spent_micros: shared.spent_micros.load(Ordering::SeqCst),
    }
}

/// Send `body` as more of the answer to request `id`, in pieces a frame
/// carries; `last` ends the answer.
fn more(machine: &dyn Machine, id: u64, body: &[u8], last: bool) -> bool {
    let mut pieces = body.chunks(MAX_BODY_CHUNK).peekable();
    if pieces.peek().is_none() {
        return !last
            || machine.tell(&ToRun::AnswerBody {
                id,
                bytes: Vec::new(),
                last,
            });
    }
    while let Some(piece) = pieces.next() {
        let told = machine.tell(&ToRun::AnswerBody {
            id,
            bytes: piece.to_vec(),
            last: last && pieces.peek().is_none(),
        });
        if !told {
            return false;
        }
    }
    true
}

/// Send the head of the answer to request `id`.
fn head(machine: &dyn Machine, id: u64, status: u16, content_type: &str) -> bool {
    machine.tell(&ToRun::Answer {
        id,
        status,
        headers: vec![("content-type".to_owned(), content_type.to_owned())],
    })
}

/// Send a whole answer to request `id`.
fn whole(machine: &dyn Machine, id: u64, answer: &Answer) {
    let _ = head(machine, id, answer.status, answer.content_type)
        && more(machine, id, &answer.body, true);
}

/// Answer one request of the run, on a thread of its own.
fn answer(shared: &Shared, asked: &Asked, body: &[u8], gone: &AtomicBool) {
    let machine = shared.machine.as_ref();
    match asked.endpoint.as_str() {
        MODEL => match shared.harness.model_protocol() {
            ModelProtocol::AnthropicMessages => answer_model(shared, asked, body, gone),
        },
        TILO => whole(machine, asked.id, &answer_tilo(shared, asked, body)),
        _ => whole(
            machine,
            asked.id,
            &Answer {
                status: 404,
                content_type: "text/plain; charset=utf-8",
                body: b"this run has no such endpoint".to_vec(),
            },
        ),
    }
}

/// Tilo's MCP server over HTTP as a harness speaks it: each posted message
/// is answered with its answer as JSON, a message that asks for none is
/// accepted, and there is no stream the server opens by itself.
fn answer_tilo(shared: &Shared, asked: &Asked, body: &[u8]) -> Answer {
    let plain = |status: u16, words: &str| Answer {
        status,
        content_type: "text/plain; charset=utf-8",
        body: words.as_bytes().to_vec(),
    };
    if asked.method != "POST" {
        return plain(405, "Tilo's MCP server answers posted messages");
    }
    let Ok(request) = serde_json::from_slice::<Value>(body) else {
        return plain(400, "an MCP message is JSON");
    };
    let answered = match request {
        Value::Array(messages) => {
            let answers: Vec<Value> = messages
                .into_iter()
                .filter_map(|message| (shared.serves.tilo)(message))
                .collect();
            (!answers.is_empty()).then_some(Value::Array(answers))
        }
        message => (shared.serves.tilo)(message),
    };
    match answered {
        Some(answer) => Answer {
            status: 200,
            content_type: "application/json",
            body: answer.to_string().into_bytes(),
        },
        None => Answer {
            status: 202,
            content_type: "application/json",
            body: Vec::new(),
        },
    }
}

/// A streamed answer's connection: opened once, by whoever comes first, the
/// answer or the wait for it.
struct Stream {
    opened: bool,
    /// The answer is on its way out: nothing keeps the stream alive any more.
    answered: bool,
}

/// Ask the person's own model what the harness asked, and answer in the
/// provider's shape. An answer or a refusal that comes at once goes out
/// under its own status. A harness that reads a stream and whose answer
/// takes longer has its stream opened and kept alive meanwhile, and a
/// refusal then arrives as the stream's own error.
fn answer_model(shared: &Shared, asked: &Asked, body: &[u8], gone: &AtomicBool) {
    let machine = shared.machine.as_ref();
    let id = asked.id;
    let wanted = match model_endpoint::wanted(&asked.method, &asked.path, body) {
        Ok(Wanted::Message(wanted)) => wanted,
        Ok(Wanted::Count(tokens)) => return whole(machine, id, &model_endpoint::counted(tokens)),
        Err(refused) => return whole(machine, id, &refused),
    };
    let model = (shared.serves.model)(shared.harness.kind_of(&wanted.model));
    let stopped = || gone.load(Ordering::SeqCst) || shared.over.load(Ordering::SeqCst);
    let stream = Mutex::new(Stream {
        opened: false,
        answered: false,
    });
    let answered = std::thread::scope(|scope| {
        // Dropped when the answer is there, which ends the keeping alive at once.
        let (answering, answered) = mpsc::channel::<()>();
        if wanted.stream {
            let (stream, stopped) = (&stream, &stopped);
            scope.spawn(move || keep_alive(machine, id, stream, &answered, stopped));
        }
        let deadline = Instant::now()
            .checked_add(MODEL_TIME)
            .unwrap_or_else(Instant::now);
        let answer = model.answer_blocking(&wanted.request, deadline, &stopped, &mut |_| {});
        stream.lock_recover().answered = true;
        drop(answering);
        answer
    });
    if stopped() {
        return;
    }
    let opened = stream.lock_recover().opened;
    match answered {
        Ok(answer) => {
            shared
                .spent_micros
                .fetch_add(answer.response.cost_micros, Ordering::SeqCst);
            let said = model_endpoint::message(
                &answer.response,
                &wanted.model,
                &format!("msg_{}", uuid::Uuid::new_v4().simple()),
            );
            if wanted.stream {
                let (status, content_type) = model_endpoint::stream_begins();
                let _ = (opened || head(machine, id, status, content_type))
                    && more(machine, id, &model_endpoint::events(&said), true);
            } else {
                whole(machine, id, &model_endpoint::whole(&said));
            }
        }
        Err(refusal) => {
            let (status, kind, words) =
                model_endpoint::refused(&refusal, shared.serves.language.as_deref());
            if opened {
                more(
                    machine,
                    id,
                    &model_endpoint::failure_event(kind, &words),
                    true,
                );
            } else {
                whole(machine, id, &model_endpoint::failure(status, kind, &words));
            }
        }
    }
}

/// While a streamed answer is made, open its stream once the harness has
/// waited [`HEAD_WAIT`] and keep it alive every [`PING_EVERY`], until the
/// answer is there (`answered` ends) or nobody waits for it any more.
fn keep_alive(
    machine: &dyn Machine,
    id: u64,
    stream: &Mutex<Stream>,
    answered: &mpsc::Receiver<()>,
    stopped: &(dyn Fn() -> bool + Sync),
) {
    let mut next = Instant::now().checked_add(HEAD_WAIT);
    while matches!(
        answered.recv_timeout(LOOK),
        Err(mpsc::RecvTimeoutError::Timeout)
    ) {
        if stopped() {
            return;
        }
        if next.is_some_and(|next| Instant::now() < next) {
            continue;
        }
        let mut stream = stream.lock_recover();
        if stream.answered {
            return;
        }
        if !stream.opened {
            let (status, content_type) = model_endpoint::stream_begins();
            stream.opened = head(machine, id, status, content_type);
        }
        let alive = stream.opened && more(machine, id, model_endpoint::PING, false);
        drop(stream);
        if !alive {
            return;
        }
        next = Instant::now().checked_add(PING_EVERY);
    }
}

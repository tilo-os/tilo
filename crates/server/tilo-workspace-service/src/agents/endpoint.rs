//! This computer's one model endpoint: a program the engine runs here (a coding agent's harness)
//! asks its model in its provider's own words, and the workspace's model answers.
//!
//! It is one loopback listener for the whole daemon, opened the first time a program is admitted
//! and kept for as long as the daemon runs. Each admission is a fresh random word, so its address
//! is `http://127.0.0.1:<port>/<secret>/`: a program of this computer that does not hold a live
//! secret is answered nothing, and what holds one is asked as the turn it was admitted for. Under
//! it, `model/` answers in the dialect the program speaks, by the model the workspace chose for
//! the turn's kind of work ([`PlanTurns`], which asks it through `person_model`):
//!
//! - `model/v1/messages` (and its `count_tokens`): Anthropic's Messages ([`messages`]);
//! - `model/v1/chat/completions`: `OpenAI`'s chat completions ([`chat`]).
//!
//! `mcp` answers Tilo's MCP server as the admitted turn, which goes on with the newest of what its
//! person wrote ([`Serves::tools`]). The model's key stays where the model is asked: nothing here
//! holds it.
//!
//! It is plain HTTP/1.1, one request to a connection: what a program asks of a local endpoint,
//! answered whole once its answer is there.

pub mod chat;
pub mod messages;

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tilo_agent_session::model::PlanTurns;
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;

use crate::agents::WorkspaceActionProvider;
use crate::workspaces::person_model::protocol::{ModelError, ModelRequest, ModelResponse};

/// How long one answer of the model may take.
const MODEL_TIME: Duration = Duration::from_secs(10 * 60);
/// The longest head of a request that is read.
const MAX_HEAD: usize = 64 * 1024;
/// The most of a request's body that is read: a whole conversation with its pictures.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
/// What an answer may take when the program names no limit.
const DEFAULT_OUTPUT_TOKENS: u32 = 4096;
/// What a program sent beside words that this endpoint does not pass on.
const NOT_PASSED: &str = "[something was attached here that this model endpoint does not pass on]";

/// The agent tools of the turn an admission answers now.
pub type Tools = Arc<Mutex<Arc<McpServer<WorkspaceActionProvider>>>>;

/// What answers an admitted program.
pub struct Serves {
    /// The workspace's model for the turn's kind of work, as its turn asks it.
    pub model: Box<dyn PlanTurns>,
    /// What its model calls are named in the usage log by.
    pub key: String,
    /// Tilo's MCP server, as the turn.
    pub tools: Tools,
    /// Where the MCP server's answers are made.
    pub runtime: tokio::runtime::Handle,
}

/// Where an admitted program reaches this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// The base address its model answers at, in every dialect.
    pub model: String,
    /// The address of Tilo's MCP server for its turn.
    pub tilo: String,
}

/// A request a program made of its model, in whichever dialect it spoke.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    pub request: ModelRequest,
    /// The model the program named: said back to it, never followed.
    pub model: String,
    /// Whether it reads its answer as a stream of events.
    pub stream: bool,
}

/// An answer in the provider's shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

const JSON: &str = "application/json";
const EVENTS: &str = "text/event-stream";

/// One admitted program's turn.
struct Admitted {
    model: Mutex<Box<dyn PlanTurns>>,
    key: String,
    tools: Tools,
    runtime: tokio::runtime::Handle,
    spent: Arc<AtomicU64>,
}

type Table = Arc<Mutex<HashMap<String, Arc<Admitted>>>>;

/// The daemon's one listener and who it answers.
struct Device {
    address: SocketAddr,
    admitted: Table,
}

static DEVICE: OnceLock<Result<Device, String>> = OnceLock::new();

fn device() -> Result<&'static Device, String> {
    DEVICE
        .get_or_init(|| {
            let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
            let address = listener.local_addr().map_err(|e| e.to_string())?;
            let admitted = Table::default();
            let table = Arc::clone(&admitted);
            std::thread::Builder::new()
                .name("model-endpoint".into())
                .spawn(move || {
                    for connection in listener.incoming().flatten() {
                        let table = Arc::clone(&table);
                        let _ = std::thread::Builder::new()
                            .name("model-endpoint-ask".into())
                            .spawn(move || answer(&table, &connection));
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(Device { address, admitted })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// A program's admission at this computer's endpoint, answered for as long as it is held.
pub struct Admission {
    address: SocketAddr,
    secret: String,
}

impl Admission {
    /// Admit a program to ask as `serves`' turn, counting what the model's answers cost in
    /// `spent`.
    ///
    /// # Errors
    /// This computer's endpoint could not take a loopback address.
    pub fn admit(serves: Serves, spent: Arc<AtomicU64>) -> Result<Self, String> {
        let device = device()?;
        let secret = uuid::Uuid::new_v4().simple().to_string();
        device.admitted.lock_recover().insert(
            secret.clone(),
            Arc::new(Admitted {
                model: Mutex::new(serves.model),
                key: serves.key,
                tools: serves.tools,
                runtime: serves.runtime,
                spent,
            }),
        );
        Ok(Self {
            address: device.address,
            secret,
        })
    }

    /// Where its program reaches this computer.
    #[must_use]
    pub fn endpoints(&self) -> Endpoints {
        let base = format!("http://{}/{}", self.address, self.secret);
        Endpoints {
            model: format!("{base}/model"),
            tilo: format!("{base}/mcp"),
        }
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if let Ok(device) = device() {
            device.admitted.lock_recover().remove(&self.secret);
        }
    }
}

/// One request: its method, its path and its body.
struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read(connection: &TcpStream) -> Option<Request> {
    let mut reading = BufReader::new(connection.try_clone().ok()?);
    let mut first = String::new();
    reading.read_line(&mut first).ok()?;
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next()?.to_owned(), parts.next()?.to_owned());
    let (mut length, mut head) = (0_usize, first.len());
    loop {
        let mut line = String::new();
        let read = reading.read_line(&mut line).ok()?;
        head = head.saturating_add(read);
        if read == 0 || head > MAX_HEAD {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
    }
    if length > MAX_REQUEST_BYTES {
        return None;
    }
    let mut body = vec![0; length];
    reading.read_exact(&mut body).ok()?;
    Some(Request { method, path, body })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        429 => "Too Many Requests",
        _ => "Error",
    }
}

fn write(mut connection: &TcpStream, answer: &Answer) {
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        answer.status,
        reason(answer.status),
        answer.content_type,
        answer.body.len()
    );
    let _ = connection
        .write_all(head.as_bytes())
        .and_then(|()| connection.write_all(&answer.body))
        .and_then(|()| connection.flush());
}

fn plain(status: u16, words: &str) -> Answer {
    Answer {
        status,
        content_type: "text/plain; charset=utf-8",
        body: words.as_bytes().to_vec(),
    }
}

fn answer(table: &Table, connection: &TcpStream) {
    let Some(request) = read(connection) else {
        return write(connection, &plain(400, "the request was not read"));
    };
    let (secret, rest) = request
        .path
        .strip_prefix('/')
        .and_then(|path| path.split_once('/'))
        .unwrap_or_default();
    let Some(admitted) = table.lock_recover().get(secret).cloned() else {
        return write(connection, &plain(404, "no program is admitted here"));
    };
    let answered = match rest.strip_prefix("model") {
        Some(asked) => model(&admitted, &request, asked),
        None if rest.split('?').next() == Some("mcp") => tilo(&admitted, &request),
        None => plain(404, "there is no such endpoint"),
    };
    write(connection, &answered);
}

/// The dialects a program may ask its model in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    Messages,
    Chat,
}

impl Dialect {
    fn asked(self, body: &[u8]) -> Result<Asked, String> {
        match self {
            Self::Messages => messages::asked(body),
            Self::Chat => chat::asked(body),
        }
    }

    fn invalid(self, why: &str) -> Answer {
        match self {
            Self::Messages => messages::failure(400, "invalid_request_error", why),
            Self::Chat => chat::failure(400, "invalid_request_error", why),
        }
    }

    fn answer(self, response: &ModelResponse, asked: &Asked) -> Answer {
        match self {
            Self::Messages => messages::answer(response, asked),
            Self::Chat => chat::answer(response, asked),
        }
    }

    fn refused(self, error: &ModelError) -> Answer {
        match self {
            Self::Messages => messages::refused(error),
            Self::Chat => chat::refused(error),
        }
    }
}

/// The workspace's model, asked what the program asked, answered in its dialect.
fn model(admitted: &Admitted, request: &Request, path: &str) -> Answer {
    let route = path
        .split('?')
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    let dialect = match (request.method.as_str(), route) {
        ("POST", "/v1/messages") => Dialect::Messages,
        ("POST", "/v1/messages/count_tokens") => return messages::counted(&request.body),
        ("POST", "/v1/chat/completions") => Dialect::Chat,
        _ => {
            return plain(
                404,
                "this endpoint answers messages and chat completions, and nothing else",
            );
        }
    };
    let asked = match dialect.asked(&request.body) {
        Ok(asked) => asked,
        Err(why) => return dialect.invalid(&why),
    };
    let deadline = tilo_clock::now()
        .checked_add(MODEL_TIME)
        .unwrap_or_else(tilo_clock::now);
    let answered =
        admitted
            .model
            .lock_recover()
            .turn(&asked.request, &admitted.key, deadline, false);
    match answered {
        Ok(response) => {
            admitted
                .spent
                .fetch_add(response.cost_micros, Ordering::SeqCst);
            dialect.answer(&response, &asked)
        }
        Err(error) => dialect.refused(&error),
    }
}

/// Tilo's MCP server over HTTP as a program speaks it: each posted message is answered with its
/// answer as JSON, a message that asks for none is accepted, and there is no stream the server
/// opens by itself.
fn tilo(admitted: &Admitted, request: &Request) -> Answer {
    if request.method != "POST" {
        return plain(405, "Tilo's MCP server answers posted messages");
    }
    let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
        return plain(400, "an MCP message is JSON");
    };
    let tools = Arc::clone(&admitted.tools.lock_recover());
    let answered = admitted.runtime.block_on(async {
        match message {
            Value::Array(messages) => {
                let mut answers = Vec::new();
                for message in messages {
                    answers.extend(tools.handle(message).await);
                }
                (!answers.is_empty()).then_some(Value::Array(answers))
            }
            message => tools.handle(message).await,
        }
    });
    match answered {
        Some(answer) => Answer {
            status: 200,
            content_type: JSON,
            body: answer.to_string().into_bytes(),
        },
        None => Answer {
            status: 202,
            content_type: JSON,
            body: Vec::new(),
        },
    }
}

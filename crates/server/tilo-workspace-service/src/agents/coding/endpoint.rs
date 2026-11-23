//! A run's own endpoint: its model and Tilo's MCP server, answered on this computer's loopback.
//!
//! Each run gets an address of its own, `http://127.0.0.1:<port>/<secret>/`, the secret a fresh
//! random word: a program of this computer that does not hold it is answered nothing. Under it,
//! `model/` answers the harness's model in its provider's own protocol
//! ([`super::model_endpoint`]) by the workspace's model for the sub-agent's kind of work, and
//! `mcp` answers Tilo's MCP server as the sub-agent's turn, which goes on with the newest of
//! what its person wrote ([`Serves::tools`]). The model's key stays where the model is asked:
//! nothing here holds it.
//!
//! It is plain HTTP/1.1, one request to a connection: what a harness asks of a local endpoint,
//! answered whole once its answer is there.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tilo_agent_session::model::PlanTurns;
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;

use super::harness::Endpoints;
use super::model_endpoint::{self, Answer, Wanted};
use crate::agents::WorkspaceActionProvider;

/// How long one answer of the model may take.
const MODEL_TIME: Duration = Duration::from_secs(10 * 60);
/// The longest head of a request that is read.
const MAX_HEAD: usize = 64 * 1024;

/// The agent tools of the turn a run answers now.
pub(super) type Tools = Arc<Mutex<Arc<McpServer<WorkspaceActionProvider>>>>;

/// What answers a run's endpoints.
pub(super) struct Serves {
    /// The workspace's model for the sub-agent's kind of work, as its turn asks it.
    pub model: Box<dyn PlanTurns>,
    /// What its model calls are named in the usage log by.
    pub key: String,
    /// Tilo's MCP server, as the sub-agent's turn.
    pub tools: Tools,
    /// Where the MCP server's answers are made.
    pub runtime: tokio::runtime::Handle,
}

/// What the connections of one endpoint share.
struct Shared {
    secret: String,
    model: Mutex<Box<dyn PlanTurns>>,
    key: String,
    tools: Tools,
    runtime: tokio::runtime::Handle,
    spent: Arc<AtomicU64>,
}

/// A run's endpoint, answering for as long as it is held.
pub(super) struct Endpoint {
    address: SocketAddr,
    secret: String,
    closed: Arc<AtomicBool>,
}

impl Endpoint {
    /// Answer at a fresh loopback address, counting what the model's answers cost in `spent`.
    ///
    /// # Errors
    /// No loopback address could be taken.
    pub fn open(serves: Serves, spent: Arc<AtomicU64>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let secret = uuid::Uuid::new_v4().simple().to_string();
        let closed = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            secret: secret.clone(),
            model: Mutex::new(serves.model),
            key: serves.key,
            tools: serves.tools,
            runtime: serves.runtime,
            spent,
        });
        let stop = Arc::clone(&closed);
        std::thread::Builder::new()
            .name("coding-endpoint".into())
            .spawn(move || {
                for connection in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(connection) = connection else {
                        continue;
                    };
                    let shared = Arc::clone(&shared);
                    let _ = std::thread::Builder::new()
                        .name("coding-endpoint-ask".into())
                        .spawn(move || answer(&shared, &connection));
                }
            })?;
        Ok(Self {
            address,
            secret,
            closed,
        })
    }

    /// Where its harness reaches it.
    #[must_use]
    pub fn endpoints(&self) -> Endpoints {
        let base = format!("http://{}/{}", self.address, self.secret);
        Endpoints {
            model: format!("{base}/model"),
            tilo: format!("{base}/mcp"),
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        // Wakes the listener, which then sees it is closed.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
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
    if length > model_endpoint::MAX_REQUEST_BYTES {
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

fn answer(shared: &Shared, connection: &TcpStream) {
    let Some(request) = read(connection) else {
        return write(
            connection,
            &model_endpoint::failure(400, "invalid_request_error", "the request was not read"),
        );
    };
    let answered = match request
        .path
        .strip_prefix('/')
        .and_then(|path| path.strip_prefix(shared.secret.as_str()))
    {
        Some(rest) => match rest.strip_prefix("/model") {
            Some(asked) => model(shared, &request, asked),
            None if rest.split('?').next() == Some("/mcp") => tilo(shared, &request),
            None => plain(404, "this run has no such endpoint"),
        },
        None => plain(404, "this run has no such endpoint"),
    };
    write(connection, &answered);
}

/// The workspace's model, asked what the harness asked, answered in the provider's shape.
fn model(shared: &Shared, request: &Request, path: &str) -> Answer {
    let wanted = match model_endpoint::wanted(&request.method, path, &request.body) {
        Ok(Wanted::Message(wanted)) => wanted,
        Ok(Wanted::Count(tokens)) => return model_endpoint::counted(tokens),
        Err(refused) => return refused,
    };
    let deadline = tilo_clock::now()
        .checked_add(MODEL_TIME)
        .unwrap_or_else(tilo_clock::now);
    let answered = shared
        .model
        .lock_recover()
        .turn(&wanted.request, &shared.key, deadline, false);
    match answered {
        Ok(response) => {
            shared
                .spent
                .fetch_add(response.cost_micros, Ordering::SeqCst);
            let said = model_endpoint::message(
                &response,
                &wanted.model,
                &format!("msg_{}", uuid::Uuid::new_v4().simple()),
            );
            if wanted.stream {
                let (status, content_type) = model_endpoint::stream_begins();
                Answer {
                    status,
                    content_type,
                    body: model_endpoint::events(&said),
                }
            } else {
                model_endpoint::whole(&said)
            }
        }
        Err(error) => {
            let (status, kind, words) = model_endpoint::refused(&error);
            model_endpoint::failure(status, kind, &words)
        }
    }
}

/// Tilo's MCP server over HTTP as a harness speaks it: each posted message is answered with its
/// answer as JSON, a message that asks for none is accepted, and there is no stream the server
/// opens by itself.
fn tilo(shared: &Shared, request: &Request) -> Answer {
    if request.method != "POST" {
        return plain(405, "Tilo's MCP server answers posted messages");
    }
    let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
        return plain(400, "an MCP message is JSON");
    };
    let tools = Arc::clone(&shared.tools.lock_recover());
    let answered = shared.runtime.block_on(async {
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

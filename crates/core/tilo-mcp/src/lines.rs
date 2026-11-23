//! Newline-delimited JSON-RPC over a byte stream: how an MCP connection's
//! lines are read, answered and written, on stdio or a socket.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;

use crate::McpServer;

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Line(Vec<u8>),
    TooLong,
    Eof,
}

/// Read one newline-terminated request line, capped at `MAX_REQUEST_BYTES`.
///
/// An oversized line is consumed through its newline and reported as
/// [`Frame::TooLong`]; end of input yields any partial line, then [`Frame::Eof`].
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Frame> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if too_long {
                Ok(Frame::TooLong)
            } else if line.is_empty() {
                Ok(Frame::Eof)
            } else {
                Ok(Frame::Line(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        // `position` indexes into `available`, so `position + 1` is at most its length.
        let consumed = newline.map_or(available.len(), |position| position.saturating_add(1));
        // Everything before the first newline, or the whole buffer when there is none.
        let content = available
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        if !too_long {
            if line.len().saturating_add(content.len()) > MAX_REQUEST_BYTES {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(content);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if too_long {
                return Ok(Frame::TooLong);
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Frame::Line(line));
        }
    }
}

/// Write `reply` as one JSON line and flush it.
pub fn write_reply(writer: &mut impl Write, reply: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, reply).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// What a connection's reader thread hands its serving loop.
pub enum InputEvent {
    Frame(Frame),
    Error(String),
}

/// Read `reader`'s frames on a thread of their own, so a connection's
/// serving loop writes replies while the next request has yet to arrive.
pub fn spawn_reader(
    reader: impl Read + Send + 'static,
) -> io::Result<tokio::sync::mpsc::Receiver<InputEvent>> {
    // Bounds aggregate buffered input as well as each individual request.
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    std::thread::Builder::new()
        .name("tilo-mcp-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let event = match read_frame(&mut reader) {
                    Ok(frame) => InputEvent::Frame(frame),
                    Err(error) => InputEvent::Error(error.to_string()),
                };
                let terminal =
                    matches!(event, InputEvent::Frame(Frame::Eof) | InputEvent::Error(_));
                if sender.blocking_send(event).is_err() || terminal {
                    return;
                }
            }
        })
        .map_err(|error| io::Error::other(format!("failed to start MCP reader: {error}")))?;
    Ok(receiver)
}

fn protocol_error(code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":null,"error":{"code":code,"message":message}})
}

/// A line's reply, or `None` for a notification.
type Reply = io::Result<Option<Value>>;

/// How a line is answered, relative to the lines after it.
pub enum Answer<'a> {
    /// Before the next line is read, by the connection's own thread:
    /// `initialize`, whose run id and page detail every later call of the
    /// session reads, and a daemon's own native requests.
    InOrder(std::pin::Pin<Box<dyn std::future::Future<Output = Reply> + 'a>>),
    /// On the runtime, beside the session's other requests: a `tilo_status`
    /// (reading or cancelling) answers while the `tilo_do` it is about waits.
    /// The session bounds how many of these work at once.
    Overlapping(std::pin::Pin<Box<dyn std::future::Future<Output = Reply> + Send>>),
}

/// `request` answered by `server`, as an MCP request on a line-framed
/// connection is.
pub fn mcp_answer<H>(server: &std::sync::Arc<McpServer<H>>, request: Value) -> Answer<'static>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    let server = std::sync::Arc::clone(server);
    let initialize = request.get("method").and_then(Value::as_str) == Some("initialize");
    let reply = Box::pin(async move { Ok::<_, io::Error>(server.handle(request).await) });
    if initialize {
        Answer::InOrder(reply)
    } else {
        Answer::Overlapping(reply)
    }
}

/// Whether a connection's caller acts no further, and why: set by [`Ending::answer`] once a
/// request found it so, which ends the connection after that answer ([`Ending::check`]).
#[derive(Clone, Default)]
pub struct Ending(std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl Ending {
    /// The answer to `request` once `server`'s caller acts no further ([`McpServer::ended`]):
    /// why, as the call's own refusal, answered before the next line is read. `None` while the
    /// caller acts, and the request is answered as any other.
    pub fn answer<H>(
        &self,
        server: &std::sync::Arc<McpServer<H>>,
        request: &Value,
    ) -> Option<Answer<'static>>
    where
        H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
    {
        let why = server.ended()?;
        let id = request.get("id").cloned();
        let reply = id.map(|id| {
            if request.get("method").and_then(Value::as_str) == Some("tools/call") {
                json!({"jsonrpc": "2.0", "id": id, "result": {
                    "content": [{"type": "text", "text": why}], "isError": true}})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": why}})
            }
        });
        *self.0.lock_recover() = Some(why);
        Some(Answer::InOrder(Box::pin(async move { Ok(reply) })))
    }

    /// What ends the connection once a request was answered that its caller acts no further:
    /// the `check` [`serve_lines`] runs, which runs after that answer was written.
    ///
    /// # Errors
    /// The caller acts no further: why.
    pub fn check(&self) -> io::Result<()> {
        match &*self.0.lock_recover() {
            Some(why) => Err(io::Error::new(io::ErrorKind::PermissionDenied, why.clone())),
            None => Ok(()),
        }
    }
}

/// How often the stdio host checks its app children.
const CHECK_EVERY: Duration = Duration::from_millis(25);

/// Answer one newline-framed connection until its input ends.
///
/// `answer` answers each request; an overlapping one runs on `runtime`
/// beside the others in flight, and its reply is written, whole and on its
/// own line, as soon as it is ready. JSON-RPC ids pair replies with
/// requests, so they may come out of order. Only this loop writes, so lines
/// never interleave. At end of input the requests in flight are answered
/// before it returns. `check`, when given, runs every [`CHECK_EVERY`] and
/// ends the loop with its error.
pub fn serve_lines<'a>(
    runtime: &tilo_rt::AppRuntime,
    mut input: tokio::sync::mpsc::Receiver<InputEvent>,
    writer: &mut impl Write,
    mut answer: impl FnMut(Value) -> Answer<'a>,
    mut check: Option<&mut dyn FnMut() -> io::Result<()>>,
) -> io::Result<()> {
    let handle = runtime.handle().clone();
    runtime
        .block_on(async move {
            // Dropped with the connection, which aborts what still runs.
            let mut running = tokio::task::JoinSet::new();
            let mut open = true;
            let mut checks = tokio::time::interval(CHECK_EVERY);
            checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            while open || !running.is_empty() {
                tokio::select! {
                    event = input.recv(), if open => {
                        let request = match event {
                            Some(InputEvent::Frame(Frame::Line(bytes))) => {
                                serde_json::from_slice::<Value>(&bytes)
                            }
                            Some(InputEvent::Frame(Frame::Eof)) => {
                                open = false;
                                continue;
                            }
                            Some(InputEvent::Frame(Frame::TooLong)) => {
                                let reply = protocol_error(-32700, "request line exceeds 1 MiB");
                                write_reply(writer, &reply)?;
                                continue;
                            }
                            Some(InputEvent::Error(message)) => {
                                return Err(io::Error::new(io::ErrorKind::InvalidData, message));
                            }
                            None => {
                                return Err(io::Error::new(
                                    io::ErrorKind::BrokenPipe,
                                    "MCP reader ended without EOF",
                                ));
                            }
                        };
                        match request.map(&mut answer) {
                            Ok(Answer::InOrder(reply)) => {
                                if let Some(reply) = reply.await? {
                                    write_reply(writer, &reply)?;
                                }
                            }
                            Ok(Answer::Overlapping(reply)) => {
                                running.spawn_on(reply, &handle);
                            }
                            Err(_) => write_reply(writer, &protocol_error(-32700, "Parse error"))?,
                        }
                    }
                    Some(done) = running.join_next() => {
                        if let Some(reply) = done.map_err(io::Error::other)?? {
                            write_reply(writer, &reply)?;
                        }
                    }
                    _ = checks.tick(), if check.is_some() => {
                        if let Some(check) = check.as_mut() {
                            check()?;
                        }
                    }
                }
            }
            Ok(())
        })
        .map_err(io::Error::other)?
}

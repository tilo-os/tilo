//! A client for the console channel of a scenario VM: the installer and the
//! greeter answer action frames, one per line, and `state_dump`, on a
//! virtio-serial port whose host end `os/dev/vm.sh` exposes as a Unix socket.
//! An answer is matched to its request (an invocation's id, `state_dump`'s
//! `seq`), so a reply to a request this client gave up on is skipped.

use std::io::{BufRead, BufReader};
#[cfg(windows)]
use std::net::TcpStream;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

use serde_json::{Map, Value, json};

/// The action wire's version these frames are written at (`RemoteActionFrame` v2).
const FRAME_VERSION: u16 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleError {
    Unreachable(String),
    Refused { code: String, message: String },
    Protocol(String),
}

impl std::fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(why) => write!(f, "console unreachable: {why}"),
            Self::Refused { code, message } => write!(f, "{code}: {message}"),
            Self::Protocol(why) => write!(f, "console protocol error: {why}"),
        }
    }
}

/// The confirmation an invocation carries, as its action's contract asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// The caller's own yes.
    Confirmed,
    /// The phrase the contract names, typed exactly.
    Phrase(String),
}

/// The frame that invokes `resource`'s `action` of `provider` with `args`, and the invocation
/// id its receipt names.
#[must_use]
pub fn invoke_frame(
    provider: &str,
    resource: &str,
    action: &str,
    args: Map<String, Value>,
    confirmation: Option<&Confirmation>,
) -> (Value, String) {
    let invocation = uuid::Uuid::new_v4().to_string();
    let argument = if args.is_empty() {
        json!("None")
    } else {
        Value::Object(Map::from_iter([("Json".to_owned(), Value::Object(args))]))
    };
    let mut request = Map::new();
    request.insert("invocation".into(), json!(invocation));
    request.insert(
        "target".into(),
        json!({ "provider": provider, "address": { "resource": resource, "action": action } }),
    );
    request.insert("argument".into(), argument);
    match confirmation {
        Some(Confirmation::Confirmed) => {
            request.insert("confirmation".into(), json!("Confirmed"));
        }
        Some(Confirmation::Phrase(phrase)) => {
            request.insert("confirmation".into(), json!({ "Phrase": phrase }));
        }
        None => {}
    }
    (
        json!({ "version": FRAME_VERSION, "message": { "Invoke": request } }),
        invocation,
    )
}

/// The frame that asks for the actions offered that `needle` names.
#[must_use]
pub fn catalogue_frame(needle: &str) -> Value {
    json!({
        "version": FRAME_VERSION,
        "message": { "CatalogueRequest": { "correlation": 1, "query": { "needle": needle } } },
    })
}

/// What the receipt frame `reply` says the invocation answered: its value once it completed,
/// or why it was refused or failed.
///
/// # Errors
///
/// [`ConsoleError::Refused`] with the refusal's or the failure's code, and
/// [`ConsoleError::Protocol`] for anything that is not a settled receipt.
pub fn receipt_value(reply: &Value) -> Result<Value, ConsoleError> {
    let status = reply
        .pointer("/message/Receipt/status")
        .ok_or_else(|| refusal_or_protocol(reply))?;
    if let Some(completed) = status.get("Completed") {
        return Ok(completed
            .pointer("/output/value")
            .cloned()
            .unwrap_or(Value::Null));
    }
    if let Some(error) = status.pointer("/Failed/error") {
        let text = |key: &str| error.get(key).and_then(Value::as_str).unwrap_or_default();
        return Err(ConsoleError::Refused {
            code: text("code").to_owned(),
            message: text("message").to_owned(),
        });
    }
    if let Some(error) = status.pointer("/Rejected/error") {
        // A refusal travels as its variant: a bare name, or one field object.
        let (code, message) = match error {
            Value::String(code) => (code.clone(), String::new()),
            Value::Object(variant) => variant
                .iter()
                .next()
                .map(|(code, fields)| {
                    let reason = fields.get("reason").and_then(Value::as_str);
                    (code.clone(), reason.unwrap_or_default().to_owned())
                })
                .unwrap_or_default(),
            _ => (String::new(), error.to_string()),
        };
        return Err(ConsoleError::Refused { code, message });
    }
    Err(ConsoleError::Protocol(format!(
        "the action has not settled: {status}"
    )))
}

pub trait Console {
    /// `state_dump`: what the app believes is on screen.
    ///
    /// # Errors
    ///
    /// [`ConsoleError`] when the app cannot be asked or does not answer.
    fn state(&mut self) -> Result<Value, ConsoleError>;

    /// Invoke `resource`'s `action` of `provider` with `args` and the confirmation its contract
    /// asks: what it answered.
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Refused`] with the API's code when the app refused or the action failed.
    fn invoke(
        &mut self,
        provider: &str,
        resource: &str,
        action: &str,
        args: Map<String, Value>,
        confirmation: Option<&Confirmation>,
    ) -> Result<Value, ConsoleError>;
}

fn refusal_or_protocol(reply: &Value) -> ConsoleError {
    match reply.get("err") {
        Some(err) => ConsoleError::Refused {
            code: err
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        None => ConsoleError::Protocol(format!("unexpected reply shape: {reply}")),
    }
}

pub struct SocketConsole {
    path: PathBuf,
    next: u64,
    pub timeout: Duration,
}

impl SocketConsole {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            next: 0,
            timeout: Duration::from_secs(15),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(windows)]
    fn request(
        &self,
        request: &Value,
        answers: impl Fn(&Value) -> bool,
    ) -> Result<Value, ConsoleError> {
        let endpoint = self.path.to_string_lossy();
        let address = endpoint.strip_prefix("tcp://").ok_or_else(|| {
            ConsoleError::Protocol(format!("invalid TCP console endpoint: {endpoint}"))
        })?;
        let stream = TcpStream::connect(address)
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        self.exchange(stream, request, answers)
    }

    #[cfg(unix)]
    fn request(
        &self,
        request: &Value,
        answers: impl Fn(&Value) -> bool,
    ) -> Result<Value, ConsoleError> {
        let stream = tilo_files::native::socket::connect(&self.path)
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        self.exchange(stream, request, answers)
    }

    /// Send `request` and read until the reply `answers` accepts, skipping what answers an
    /// earlier request.
    fn exchange<S: std::io::Read + std::io::Write>(
        &self,
        mut stream: S,
        request: &Value,
        answers: impl Fn(&Value) -> bool,
    ) -> Result<Value, ConsoleError> {
        let unreachable = |error: std::io::Error| ConsoleError::Unreachable(error.to_string());
        writeln!(stream, "{request}").map_err(unreachable)?;
        stream.flush().map_err(unreachable)?;
        let now = Instant::now();
        let deadline = now.checked_add(self.timeout).unwrap_or(now);
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return Err(ConsoleError::Unreachable("connection closed".into())),
                Ok(_) => {}
                Err(error) => return Err(unreachable(error)),
            }
            let Ok(reply) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if answers(&reply) {
                return Ok(reply);
            }
            if Instant::now() >= deadline {
                return Err(ConsoleError::Unreachable(
                    "no answer to this request".into(),
                ));
            }
        }
    }
}

impl Console for SocketConsole {
    fn state(&mut self) -> Result<Value, ConsoleError> {
        self.next = self.next.wrapping_add(1);
        let seq = format!("tilo-vm-{}-{}", std::process::id(), self.next);
        let reply = self.request(&json!({ "op": "state_dump", "seq": seq }), |reply| {
            reply.get("seq").and_then(Value::as_str) == Some(seq.as_str())
                || (reply.get("err").is_some() && reply.get("seq").is_none())
        })?;
        reply
            .get("state")
            .cloned()
            .ok_or_else(|| refusal_or_protocol(&reply))
    }

    fn invoke(
        &mut self,
        provider: &str,
        resource: &str,
        action: &str,
        args: Map<String, Value>,
        confirmation: Option<&Confirmation>,
    ) -> Result<Value, ConsoleError> {
        let (frame, invocation) = invoke_frame(provider, resource, action, args, confirmation);
        let reply = self.request(&frame, |reply| {
            reply
                .pointer("/message/Receipt/invocation")
                .and_then(Value::as_str)
                == Some(invocation.as_str())
                || reply.get("err").is_some()
        })?;
        receipt_value(&reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    #[cfg(windows)]
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    /// A console that answers one request line with `answer` of it, after a stale line.
    #[cfg(unix)]
    fn serve_once(
        path: &Path,
        answer: impl FnOnce(&Value) -> Vec<Value> + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let mut writer = stream;
            for reply in answer(&request) {
                writeln!(writer, "{reply}").unwrap();
            }
        })
    }

    #[test]
    fn the_console_matches_replies_to_requests_and_reports_reachability() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("console.sock");
            let server = serve_once(&path, |request| {
                vec![
                    json!({"state": {"app": "stale"}, "seq": "old"}),
                    json!({"state": {"app": "installer"}, "seq": request["seq"]}),
                ]
            });
            let mut console = SocketConsole::new(&path);
            assert_eq!(console.state().unwrap()["app"], "installer");
            server.join().unwrap();

            let path = dir.path().join("invoke.sock");
            let server = serve_once(&path, |request| {
                let invoke = &request["message"]["Invoke"];
                assert_eq!(invoke["target"]["address"]["action"], "begin");
                assert_eq!(invoke["confirmation"]["Phrase"], "/dev/vdb");
                let receipt = |invocation: &Value, status: Value| json!({"version": FRAME_VERSION, "message": {"Receipt": {"invocation": invocation, "status": status}}});
                vec![
                    receipt(
                        &json!("00000000-0000-4000-8000-000000000000"),
                        json!("Accepted"),
                    ),
                    receipt(
                        &invoke["invocation"],
                        json!({"Completed": {"output": {"value": {"step": "install"}}}}),
                    ),
                ]
            });
            let mut console = SocketConsole::new(&path);
            let answered = console
                .invoke(
                    "installer",
                    "installer.review",
                    "begin",
                    Map::new(),
                    Some(&Confirmation::Phrase("/dev/vdb".into())),
                )
                .unwrap();
            assert_eq!(answered["step"], "install");
            server.join().unwrap();
        }

        #[cfg(windows)]
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let mut writer = stream;
                writeln!(writer, "{}", json!({"state":{"app":"stale"},"seq":"old"})).unwrap();
                writeln!(
                    writer,
                    "{}",
                    json!({"state":{"app":"installer"},"seq":request["seq"]})
                )
                .unwrap();
            });
            let path = PathBuf::from(format!("tcp://{address}"));
            let mut console = SocketConsole::new(&path);
            assert_eq!(console.state().unwrap()["app"], "installer");
            server.join().unwrap();
        }

        {
            #[cfg(not(windows))]
            let mut console = SocketConsole::new(Path::new("/nonexistent/console.sock"));
            #[cfg(windows)]
            let mut console = {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let address = listener.local_addr().unwrap();
                drop(listener);
                SocketConsole::new(&PathBuf::from(format!("tcp://{address}")))
            };
            assert!(matches!(console.state(), Err(ConsoleError::Unreachable(_))));
        }
    }

    #[test]
    fn a_refused_or_failed_receipt_says_why() {
        for (status, code, message) in [
            (
                json!({"Rejected": {"error": {"Refused": {"reason": "other_step: …"}}}}),
                "Refused",
                "other_step: …",
            ),
            (
                json!({"Rejected": {"error": "UnknownAction"}}),
                "UnknownAction",
                "",
            ),
            (
                json!({"Failed": {"error": {"code": "action_failed", "message": "no"}}}),
                "action_failed",
                "no",
            ),
        ] {
            let reply = json!({"version": FRAME_VERSION, "message": {"Receipt": {"invocation": "x", "status": status}}});
            assert_eq!(
                receipt_value(&reply),
                Err(ConsoleError::Refused {
                    code: code.into(),
                    message: message.into()
                })
            );
        }
    }
}

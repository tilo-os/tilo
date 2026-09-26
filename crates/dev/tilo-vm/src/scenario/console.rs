//! A client for the console channel of a scenario VM.
//!
//! The installer and the greeter answer the `tilo-control` action protocol
//! on a virtio-serial port; `os/dev/vm.sh` exposes its host end as a Unix
//! socket (`$TILO_RUN/console.sock`). This is the same protocol
//! `tilo-control --socket <that path> actions list` speaks, with one
//! addition a long-lived client needs: every request carries a `seq`, and
//! replies to anything else — a request written before the guest was
//! listening, answered after this client gave up on it — are skipped.

use std::io::{BufRead, BufReader};
#[cfg(windows)]
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

use serde_json::{Map, Value, json};

/// Why a console request did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleError {
    /// Nothing answered: the socket is absent, the guest is still booting,
    /// or the app is not running.
    Unreachable(String),
    /// The app refused the request, with the door's code.
    Refused { code: String, message: String },
    /// The reply was not the protocol.
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

/// One request/response exchange. The socket client implements it; the
/// scenario driver's tests implement it with a scripted installer.
pub trait Console {
    /// Send `request` (an object with an `op`) and return the reply object.
    ///
    /// # Errors
    ///
    /// [`ConsoleError`] when nothing answered or the reply was malformed.
    fn request(&mut self, request: Value) -> Result<Value, ConsoleError>;

    /// `state_dump`: what the app believes is on screen.
    ///
    /// # Errors
    ///
    /// [`ConsoleError`] as for [`Console::request`].
    fn state(&mut self) -> Result<Value, ConsoleError> {
        let reply = self.request(json!({ "op": "state_dump" }))?;
        reply
            .get("state")
            .cloned()
            .ok_or_else(|| refusal_or_protocol(&reply))
    }

    /// `actions_invoke` for `<action>@<resource>` on `provider`.
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Refused`] with the door's code when the app refused.
    fn invoke(
        &mut self,
        provider: &str,
        resource: &str,
        action: &str,
        args: Map<String, Value>,
    ) -> Result<Value, ConsoleError> {
        let reply = self.request(json!({
            "op": "actions_invoke",
            "provider": provider,
            "id": format!("{action}@{resource}"),
            "args": args,
        }))?;
        reply
            .get("answer")
            .cloned()
            .ok_or_else(|| refusal_or_protocol(&reply))
    }
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

/// The console socket of a running scenario VM.
pub struct SocketConsole {
    path: PathBuf,
    next: u64,
    /// How long one request may wait for its answer.
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
}

impl Console for SocketConsole {
    #[cfg(windows)]
    fn request(&mut self, request: Value) -> Result<Value, ConsoleError> {
        let endpoint = self.path.to_string_lossy();
        let address = endpoint.strip_prefix("tcp://").ok_or_else(|| {
            ConsoleError::Protocol(format!("invalid TCP console endpoint: {endpoint}"))
        })?;
        let stream = TcpStream::connect(address)
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        self.exchange(stream, request)
    }

    #[cfg(unix)]
    fn request(&mut self, request: Value) -> Result<Value, ConsoleError> {
        let stream = UnixStream::connect(&self.path)
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| ConsoleError::Unreachable(error.to_string()))?;
        self.exchange(stream, request)
    }
}

impl SocketConsole {
    fn exchange<S: std::io::Read + std::io::Write>(
        &mut self,
        mut stream: S,
        mut request: Value,
    ) -> Result<Value, ConsoleError> {
        self.next = self.next.wrapping_add(1);
        let seq = format!("tilo-vm-{}-{}", std::process::id(), self.next);
        if let Value::Object(object) = &mut request {
            object.insert("seq".into(), json!(seq));
        }
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
            if reply.get("seq").and_then(Value::as_str) == Some(seq.as_str()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    #[cfg(windows)]
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    /// Stale replies are skipped and the matching one returned; the TCP console matches the request
    /// sequence; a missing socket is unreachable and a refusal keeps its code.
    #[test]
    fn the_console_matches_replies_to_requests_and_reports_reachability() {
        // Phase: stale_replies_are_skipped_and_the_matching_one_returned.
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("console.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let mut writer = stream;
                // An answer left over from an earlier, abandoned request first.
                writeln!(
                    writer,
                    "{}",
                    json!({"state": {"app": "stale"}, "seq": "old"})
                )
                .unwrap();
                writeln!(
                    writer,
                    "{}",
                    json!({"state": {"app": "installer"}, "seq": request["seq"]})
                )
                .unwrap();
            });
            let mut console = SocketConsole::new(&path);
            let state = console.state().unwrap();
            assert_eq!(state["app"], "installer");
            server.join().unwrap();
        }

        // Phase: tcp_console_matches_the_request_sequence.
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

        // Phase: a_missing_socket_is_unreachable_and_a_refusal_keeps_its_code.
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
            let refusal = refusal_or_protocol(
                &json!({"err": {"code": "action_unavailable", "message": "other_step: …"}}),
            );
            assert_eq!(
                refusal,
                ConsoleError::Refused {
                    code: "action_unavailable".into(),
                    message: "other_step: …".into()
                }
            );
        }
    }
}

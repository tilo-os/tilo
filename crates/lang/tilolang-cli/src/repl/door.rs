//! The session's agent door as the REPL speaks to it: newline-delimited MCP JSON-RPC on the
//! door's socket (`docs/runtime-mcp.md`, "Inside a Tilo session"), and, for a nested session,
//! its control socket, where the person's remote answer admits the REPL.
//!
//! The REPL is an agent like any other: it reaches only what the person grants it, every call
//! passes the broker's gate as that agent, and its changes are its run's in history and undo.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};

/// The MCP version the REPL asks for.
const PROTOCOL: &str = "2025-06-18";

/// The client name the person is asked to admit, and the agent the REPL is admitted as.
pub const CLIENT: &str = "tilolang repl";
pub const AGENT: &str = "tilolang-repl";

/// What the person's remote answer admits the REPL to a nested session's workspace as: the
/// delegation a person's device gives its agents (reading and undoable changes, reaching outside
/// asking first). The answers the person gives it (Settings → Agents → Review access) are theirs:
/// the REPL never writes them, so a `deny` or `ask` there decides its invocations.
const PRESET: &str = "operator";

/// How long a door call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the person has to admit the REPL.
const ADMIT_TIMEOUT: Duration = Duration::from_secs(300);

/// One JSON line at a time over a Unix socket.
struct Lines {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Lines {
    fn connect(path: &Path) -> Result<Self, String> {
        let stream =
            UnixStream::connect(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
        })
    }

    fn send(&mut self, message: &Json) -> Result<(), String> {
        let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .map_err(|error| format!("the door went away: {error}"))
    }

    fn receive(&mut self, timeout: Duration) -> Result<Json, String> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout))
            .map_err(|error| error.to_string())?;
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err("the door closed the connection".to_owned()),
            Ok(_) => serde_json::from_str(&line).map_err(|error| format!("not JSON: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Err("the door did not answer in time".to_owned())
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

/// A nested session's control socket: what the person at it answers remotely.
pub struct Control {
    path: PathBuf,
}

impl Control {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// One request, its answer.
    ///
    /// # Errors
    /// The socket is gone, or the session refused the request.
    pub fn request(&self, payload: &Json) -> Result<Json, String> {
        let mut lines = Lines::connect(&self.path)?;
        lines.send(payload)?;
        let reply = lines.receive(CALL_TIMEOUT)?;
        if let Some(error) = reply.get("err") {
            return Err(format!(
                "{}: {}",
                error["code"].as_str().unwrap_or("refused"),
                error["message"].as_str().unwrap_or_default()
            ));
        }
        Ok(reply
            .get("answer")
            .or_else(|| reply.get("ok"))
            .cloned()
            .unwrap_or(reply))
    }

    /// The agent admission waiting for the person, by the id its decision rows carry.
    fn waiting_prompt(&self, deadline: Instant) -> Result<String, String> {
        loop {
            let listed = self.request(&json!({
                "op": "actions_enumerate", "query": "permission-prompts", "limit": 2000
            }))?;
            let found = listed
                .get("actions")
                .and_then(Json::as_array)
                .and_then(|rows| {
                    rows.iter().find_map(|row| {
                        (row["provider"] == "permission-prompts")
                            .then(|| row["id"].as_str()?.strip_prefix("deny@").map(str::to_owned))
                            .flatten()
                    })
                });
            if let Some(prompt) = found {
                return Ok(prompt);
            }
            if Instant::now() >= deadline {
                return Err("the session never asked to admit the REPL".to_owned());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// What a tool answered: its text, and whether it is an error.
#[derive(Debug, Clone)]
pub struct Answer {
    pub text: String,
    pub failed: bool,
}

/// The REPL's connection to the door, admitted.
pub struct Door {
    lines: Lines,
    next: u64,
    /// The tools the door serves the REPL.
    pub tools: Vec<String>,
}

impl Door {
    /// Connect to the door at `socket` and be admitted: by the person in the session's own
    /// prompt, or, for a nested session (`control`), by the person's remote answer, which admits
    /// it as [`PRESET`].
    ///
    /// # Errors
    /// The door is not there, the person did not admit the REPL, or the door refused it.
    pub fn open(
        socket: &Path,
        control: Option<&Control>,
        say: &dyn Fn(&str),
    ) -> Result<Self, String> {
        let mut lines = Lines::connect(socket)
            .map_err(|error| format!("the session's agent door isn't available: {error}"))?;
        lines.send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": CLIENT, "version": env!("CARGO_PKG_VERSION")}
            }
        }))?;
        let now = Instant::now();
        let deadline = now.checked_add(ADMIT_TIMEOUT).unwrap_or(now);
        match control {
            Some(control) => {
                let prompt = control.waiting_prompt(deadline)?;
                control.request(&json!({
                    "op": "actions_invoke", "provider": "permission-prompts",
                    "id": format!("allow-once@{prompt}"),
                    "args": {"confirm": true, "agent": AGENT, "preset": PRESET},
                    "wait_ms": 5000
                }))?;
            }
            None => say("waiting for you to admit “tilolang repl” in the desktop…"),
        }
        let initialized = lines.receive(ADMIT_TIMEOUT)?;
        if let Some(error) = initialized.get("error") {
            return Err(format!(
                "the door did not admit the REPL: {}",
                error["message"].as_str().unwrap_or("refused")
            ));
        }
        lines.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        let mut door = Self {
            lines,
            next: 1,
            tools: Vec::new(),
        };
        let listed = door.request("tools/list", &json!({}))?;
        door.tools = listed
            .get("tools")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        Ok(door)
    }

    fn request(&mut self, method: &str, params: &Json) -> Result<Json, String> {
        self.next = self.next.wrapping_add(1);
        let id = self.next;
        self.lines
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let reply = self.lines.receive(CALL_TIMEOUT)?;
            if reply.get("id") != Some(&json!(id)) {
                // A notification, or the answer to a request given up on.
                continue;
            }
            if let Some(error) = reply.get("error") {
                return Err(error["message"].as_str().unwrap_or("refused").to_owned());
            }
            return Ok(reply.get("result").cloned().unwrap_or(Json::Null));
        }
    }

    /// Call the tool `name` with `arguments`.
    ///
    /// # Errors
    /// The door refused the call as a protocol error, or went away.
    pub fn call(&mut self, name: &str, arguments: &Json) -> Result<Answer, String> {
        let result = self.request("tools/call", &json!({"name": name, "arguments": arguments}))?;
        let text = result
            .get("content")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<String>();
        Ok(Answer {
            text,
            failed: result
                .get("isError")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        })
    }

    /// Whether the door serves the tool `name`.
    #[must_use]
    pub fn serves(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool == name)
    }
}

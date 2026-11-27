//! The daemon's one connection to this device's shell runtime's supervisor: every shell an app's
//! process opens ([`crate::app_shells`]) and every question about the workspace's environments is
//! asked on it, one at a time, as the supervisor answers a connection.
//!
//! An app's process never reaches the supervisor: it holds no socket of the runtime's, and asks
//! the daemon on its own line. The daemon connects once, opens with the hello the runtime's
//! control protocol names, and asks the workspace's question in the workspace the call was
//! admitted in. A connection the runtime dropped (it restarted) is made again on the next ask.

use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

/// How long the supervisor has to answer one request: an open checks first that the environment
/// can be built, which on a device's first open reads the system store's images.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// The most one answer may carry.
const MAX_ANSWER: usize = 4 * 1024 * 1024;

/// The control protocol the supervisor is spoken to in.
const PROTOCOL: u16 = crate::supervisor::SERVICE_PROTOCOL;

/// The supervisor's answer to one request: its line, and the descriptors that came with it (a
/// session's terminal and lease).
pub struct Answer {
    pub line: Vec<u8>,
    pub descriptors: Vec<OwnedFd>,
}

/// The daemon's connection to the shell runtime listening at its socket.
pub struct ControlLine {
    socket: PathBuf,
    held: Mutex<Option<UnixStream>>,
}

impl ControlLine {
    /// The shell runtime listening at `socket`.
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            held: Mutex::new(None),
        }
    }

    /// Ask `request` in `workspace` and wait for the answer. A runtime that says it waits (a
    /// workspace a server hosts, asked to allow the shell on another device) is waited for until
    /// it answers.
    ///
    /// # Errors
    /// The runtime is not running, or did not answer in time.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the connection is held from the request's write to its answer: two asks never \
                  share one line"
    )]
    pub fn ask(&self, workspace: &str, request: &Value) -> io::Result<Answer> {
        let mut request = request.clone();
        if let Some(fields) = request.as_object_mut() {
            fields.insert("workspace".into(), Value::String(workspace.to_owned()));
        }
        let mut line = serde_json::to_vec(&request).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        // A connection the runtime let go of (it restarted) fails as the request is written: it
        // is made again once, and the request asked on the new one.
        for again in [false, true] {
            let stream = match held.as_mut() {
                Some(stream) => stream,
                None => held.insert(self.connect()?),
            };
            let written = stream.write_all(&line).and_then(|()| stream.flush());
            let answered = written.and_then(|()| answer(stream));
            match answered {
                Ok(answer) => return Ok(answer),
                Err(error) => {
                    *held = None;
                    if again || error.kind() != io::ErrorKind::BrokenPipe {
                        return Err(error);
                    }
                }
            }
        }
        Err(io::Error::other("the shell runtime did not answer"))
    }

    /// Ask `request` in `workspace` for an answer that is a line alone: the environments.
    ///
    /// # Errors
    /// As [`Self::ask`], and an answer that is no JSON.
    pub fn ask_value(&self, workspace: &str, request: &Value) -> io::Result<Value> {
        let answer = self.ask(workspace, request)?;
        serde_json::from_slice(&answer.line).map_err(io::Error::other)
    }

    fn connect(&self) -> io::Result<UnixStream> {
        let mut stream = tilo_files::native::socket::connect(&self.socket).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Tilo's shell runtime is not running here: {error}"),
            )
        })?;
        stream.set_read_timeout(Some(ANSWER_WITHIN))?;
        stream.write_all(format!("{{\"op\":\"hello\",\"protocol\":{PROTOCOL}}}\n").as_bytes())?;
        let ready = answer(&stream)?;
        let said: Value = serde_json::from_slice(&ready.line).map_err(io::Error::other)?;
        if !ready.descriptors.is_empty()
            || said.get("result").and_then(Value::as_str) != Some("ready")
            || said.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL))
        {
            return Err(io::Error::other(
                "Tilo's shell runtime speaks another protocol than this daemon",
            ));
        }
        Ok(stream)
    }
}

/// Read one answer: its line, past a line that says the runtime waits, and the descriptors that
/// came with it.
fn answer(stream: &UnixStream) -> io::Result<Answer> {
    let mut read = Vec::new();
    let mut descriptors = Vec::new();
    loop {
        if let Some(end) = read.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = read.drain(..=end).collect();
            let line = line.strip_suffix(b"\n").unwrap_or(&line).to_vec();
            if waits(&line) {
                // Whoever allows it takes as long as they take.
                stream.set_read_timeout(None)?;
                continue;
            }
            stream.set_read_timeout(Some(ANSWER_WITHIN))?;
            return Ok(Answer { line, descriptors });
        }
        let (bytes, mut came) = runtime_portal::fd_transport::recv_with_fds(stream)?;
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Tilo's shell runtime closed its connection",
            ));
        }
        descriptors.append(&mut came);
        read.extend_from_slice(&bytes);
        if read.len() > MAX_ANSWER {
            return Err(io::Error::other(
                "the shell runtime's answer is over its bound",
            ));
        }
    }
}

/// Whether `line` says the runtime asked the workspace's host and waits for it.
fn waits(line: &[u8]) -> bool {
    serde_json::from_slice::<Value>(line)
        .is_ok_and(|said| said.get("result").and_then(Value::as_str) == Some("waiting"))
}

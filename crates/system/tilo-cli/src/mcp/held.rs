//! The `tilo mcp` connections to this computer's one daemon (`tilo-runtime`), which holds
//! the device's session with the workspace.
//!
//! The host keeps one session per device, so two processes entering as the same device cut each
//! other off, over and over: each agent's calls then fail as "not connected to the host" or go
//! unanswered. Every `tilo mcp` therefore reaches the daemon over a socket in the device's
//! directory ([`reach`], starting `tilo-runtime` when none listens) and relays its agent's
//! stdio to it ([`relay`]). The daemon serves each connection as its own agent run over the one
//! session.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use tilo_api::daemon::{DaemonPlace, naming};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

/// How long a connection waits for a daemon to listen.
const REACHING: Duration = Duration::from_secs(10);
/// How long a connection waits for the user service it asked to start a daemon that listens,
/// before it starts one itself.
const SERVICE_STARTING: Duration = Duration::from_secs(5);
/// How often a connection looks for a daemon that has not started listening.
const POLL: Duration = Duration::from_millis(50);

/// Start this computer's daemon unless one listens already: what `tilo login` and `tilo mount`
/// leave running.
///
/// # Errors
/// It did not start, or did not listen in time.
pub(crate) fn ensure(place: &DaemonPlace) -> Result<(), String> {
    reach(place).map(drop)
}

/// A connection to this computer's daemon, starting it when none listens.
pub(crate) fn reach(place: &DaemonPlace) -> Result<UnixStream, String> {
    reach_starting(place, |via_service| start_daemon(place, via_service))
}

/// How a daemon was started.
enum Started {
    /// As a process of this one's, waited for and started again if it ended without listening.
    Process(std::process::Child),
    /// As the user service, which keeps it from then on: asked at this instant.
    Service(Instant),
}

/// [`reach`], with `start` starting a daemon (through the user service where it may); what it
/// started is waited for, and started again if it ended without listening (one that was leaving
/// held the lock). A service that started no daemon listening within half the wait is not asked
/// again: this command starts one itself.
fn reach_starting(
    place: &DaemonPlace,
    start: impl Fn(bool) -> std::io::Result<Started>,
) -> Result<UnixStream, String> {
    let deadline = Instant::now()
        .checked_add(REACHING)
        .unwrap_or_else(Instant::now);
    let mut started: Option<Started> = None;
    let mut via_service = true;
    loop {
        match tilo_files::native::socket::connect(place.socket()) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                // Where it listens and what it logged: for whoever keeps this
                // computer's logs, not for the agent's sentence.
                tracing::debug!(
                    socket = %place.socket().display(),
                    log = %place.log().display(),
                    %error,
                    "Tilo's background sync did not answer"
                );
                return Err(
                    "Tilo's background sync isn't answering on this computer. Run `tilo status` \
                     to see how it stands, then try again."
                        .to_owned(),
                );
            }
            Err(_) => {}
        }
        let ended = match started.as_mut() {
            None => true,
            Some(Started::Process(child)) => child.try_wait().ok().flatten().is_some(),
            Some(Started::Service(asked)) => {
                let given_up = asked.elapsed() >= SERVICE_STARTING;
                via_service &= !given_up;
                given_up
            }
        };
        if ended {
            started = Some(start(via_service).map_err(|error| {
                tracing::debug!(%error, "Tilo's background sync did not start");
                "Tilo's background sync didn't start on this computer. Run `tilo status` to see \
                 how it stands, then try again."
                    .to_owned()
            })?);
        }
        std::thread::sleep(POLL);
    }
}

/// Start the daemon (`tilo-runtime`): as the user service where it is installed, so the
/// one daemon of the device lives where the person's login keeps it, never inside the agent or
/// command that happened to need it first (and ends with it). Elsewhere on its own: in a process
/// group of its own, so the agent that ends this connection does not end it with the others',
/// and adding what it says to its log.
fn start_daemon(place: &DaemonPlace, via_service: bool) -> std::io::Result<Started> {
    if via_service && crate::service::start_installed() {
        return Ok(Started::Service(Instant::now()));
    }
    place.start().map(Started::Process)
}

/// Serve this connection's stdio alone, every call answered with `problem`: with no login to
/// reach a daemon for, or while none could be reached.
pub(crate) fn alone(problem: &str) -> Result<ExitCode> {
    use std::io::{BufRead as _, Write as _};
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = super::answer_alone(&line, problem) {
            writeln!(stdout, "{answer}")?;
            stdout.flush()?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Relay this agent's stdio to the daemon over `stream`. A daemon that ends
/// first is reached again (a new one started): the agent's opening is said
/// to it again, and a request it left unanswered is answered as one to make
/// again, so the agent keeps its tools.
pub(crate) fn relay(stream: UnixStream, place: &DaemonPlace) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(relaying(stream, place))
}

async fn relaying(first: UnixStream, place: &DaemonPlace) -> Result<ExitCode> {
    let (lines_in, mut input) = tokio::sync::mpsc::channel::<String>(64);
    // What the agent's harness calls this session: this process is the agent's own child, so it
    // is the one that knows, and each daemon reached says it in the initialize it passes on.
    let session_name = tilo_mcp::session_api::session_name();
    tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = stdin.next_line().await {
            if lines_in.send(line).await.is_err() {
                return;
            }
        }
    });
    let mut stdout = tokio::io::stdout();
    let mut opening = Opening::default();
    let mut next = Some(first);
    let mut input_open = true;
    // Requests a daemon ended before answering: a read is asked again of the
    // daemon reached in its place, once; anything else is the agent's to
    // repeat, since it may have run.
    let mut carried: Vec<String> = Vec::new();
    let mut retried: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        let stream = if let Some(stream) = next.take() {
            stream
        } else {
            let place = place.clone();
            let reached = tokio::task::spawn_blocking(move || reach(&place)).await?;
            match reached {
                Ok(stream) => stream,
                Err(why) => anyhow::bail!("{why}"),
            }
        };
        stream.set_nonblocking(true)?;
        let (read, mut write) = tokio::net::UnixStream::from_std(stream)?.into_split();
        let mut answers = BufReader::new(read).lines();
        // Said again to a daemon reached anew; its answer to the
        // initialize the agent already had is not the agent's to hear.
        let mut swallow = None;
        if let Some(said) = opening.again() {
            for line in &said.lines {
                write.write_all(format!("{line}\n").as_bytes()).await?;
            }
            swallow = said.answered;
        }
        let mut unanswered: HashMap<String, Unanswered> = HashMap::new();
        for line in std::mem::take(&mut carried) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            retried.insert(id.to_string());
            write.write_all(format!("{line}\n").as_bytes()).await?;
            unanswered.insert(id.to_string(), Unanswered { id, line });
        }
        let ended = loop {
            tokio::select! {
                line = input.recv(), if input_open => if let Some(line) = line {
                    let line = naming(line, session_name.as_deref());
                    if write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                        break true;
                    }
                    if let Some(id) = opening.note(&line) {
                        unanswered.insert(id.to_string(), Unanswered { id, line });
                    }
                } else {
                    input_open = false;
                    let _ = write.shutdown().await;
                },
                answer = answers.next_line() => match answer {
                    Ok(Some(line)) => {
                        let id = serde_json::from_str::<Value>(&line)
                            .ok()
                            .and_then(|answer| answer.get("id").cloned())
                            .map(|id| id.to_string());
                        if id.is_some() && id == swallow {
                            swallow = None;
                            continue;
                        }
                        if let Some(id) = &id {
                            unanswered.remove(id);
                        }
                        stdout.write_all(format!("{line}\n").as_bytes()).await?;
                        stdout.flush().await?;
                    }
                    _ => break input_open,
                },
            }
        };
        if !ended {
            // The agent's input closed and the daemon answered everything.
            return Ok(ExitCode::SUCCESS);
        }
        for (key, request) in unanswered {
            if a_read(&request.line) && retried.insert(key) {
                carried.push(request.line);
                continue;
            }
            let again = json!({"jsonrpc": "2.0", "id": request.id, "error": {"code": -32000,
                "message": "Tilo lost its connection to the workspace before it answered. Run `tilo status` to see how it stands, then try again."}});
            stdout.write_all(format!("{again}\n").as_bytes()).await?;
        }
        stdout.flush().await?;
    }
}

/// A request the daemon ended before answering.
struct Unanswered {
    id: Value,
    line: String,
}

/// Whether a request only reads: asking it again runs nothing twice.
fn a_read(line: &str) -> bool {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && matches!(
            message.pointer("/params/name").and_then(Value::as_str),
            Some("tilo_ls" | "tilo_read" | "tilo_find" | "tilo_status")
        )
}

/// What an agent said to open its connection, said again to a daemon
/// reached anew.
#[derive(Default)]
struct Opening {
    initialize: Option<(String, Value)>,
    initialized: Option<String>,
}

/// The opening to say again, and the id of the answer to drop.
struct SaidAgain {
    lines: Vec<String>,
    answered: Option<String>,
}

impl Opening {
    /// Note `line` the agent sends: its opening is kept, and a request's id
    /// is answered (an answer awaited).
    fn note(&mut self, line: &str) -> Option<Value> {
        let message: Value = serde_json::from_str(line).ok()?;
        let method = message.get("method").and_then(Value::as_str)?;
        let id = message.get("id").cloned();
        match method {
            "initialize" => {
                self.initialize = id.clone().map(|id| (line.to_owned(), id));
            }
            "notifications/initialized" => self.initialized = Some(line.to_owned()),
            _ => {}
        }
        id
    }

    fn again(&self) -> Option<SaidAgain> {
        let (initialize, id) = self.initialize.as_ref()?;
        Some(SaidAgain {
            lines: std::iter::once(initialize.clone())
                .chain(self.initialized.clone())
                .collect(),
            answered: Some(id.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reads_are_asked_again_by_themselves() {
        assert!(a_read(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_read","arguments":{}}}"#
        ));
        assert!(a_read(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_status"}}"#
        ));
        assert!(!a_read(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tilo_do","arguments":{}}}"#
        ));
        assert!(!a_read(
            r#"{"jsonrpc":"2.0","id":5,"method":"initialize","params":{}}"#
        ));
    }
}

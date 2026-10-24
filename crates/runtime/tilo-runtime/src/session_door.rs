//! Where an MCP client inside a Tilo login reaches the desktop it runs in.
//!
//! A Tilo session's compositor serves its action broker to agents at
//! `$XDG_RUNTIME_DIR/tilo-session/agent.sock` (the session's *agent door*):
//! the person admits each connection as an agent member of a workspace, and
//! every call then passes the broker's ordinary gate as that agent. A
//! `--connect` client inside the login relays to that door. It never starts
//! a standalone runtime there: a second, empty authority beside the desktop
//! would answer every question about the desktop wrongly, and silently.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The agent door's path under `runtime_dir`; `None` when there is none to
/// look in, which leaves no private place for it.
#[must_use]
pub fn agent_socket_path(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    Some(tilo_paths::login::session_dir(runtime_dir)?.join(tilo_paths::login::AGENT_SOCKET_FILE))
}

/// Where a `--connect` client goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// The desktop's agent door at this path.
    Session(PathBuf),
    /// A standalone runtime profile, started when none runs.
    Standalone,
}

/// Choose a `--connect` client's route. `standalone` is the client's own
/// explicit choice of a profile (`--standalone`, `--profile` or
/// `TILO_RUNTIME_PROFILE`); `door` is [`agent_socket_path`]'s answer and
/// `exists` whether something is there.
pub fn route(
    standalone: bool,
    tilo_login: bool,
    door: Option<PathBuf>,
    exists: impl FnOnce(&Path) -> bool,
) -> io::Result<Route> {
    if standalone || !tilo_login {
        return Ok(Route::Standalone);
    }
    match door {
        Some(path) if exists(&path) => Ok(Route::Session(path)),
        Some(path) => Err(missing_door(&format!(
            "nothing listens at {}; the desktop serves it once first login is complete",
            path.display()
        ))),
        None => Err(missing_door(
            "this process has no XDG_RUNTIME_DIR and the account has no /run/user directory to \
             look in",
        )),
    }
}

fn missing_door(reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "the desktop's agent door isn't available: {reason}. This is a Tilo login, where a \
             standalone runtime would be a second, empty authority beside the desktop, so none \
             was started. Check that the Tilo desktop is running and its first login is \
             complete; pass --standalone to run a separate runtime anyway"
        ),
    )
}

/// The error a client reads when the door is there but does not answer.
#[must_use]
pub fn unreachable_door(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "the desktop's agent door isn't available: {} does not answer ({error}). The Tilo \
             desktop may have ended or be starting; pass --standalone to run a separate runtime \
             instead",
            path.display()
        ),
    )
}

/// The most the door reads from a client before it is admitted: its
/// `initialize` and any discovery probe before it.
const MAX_HELLO: usize = 64 * 1024;
/// The most of a client's own name the person is shown.
const MAX_CLIENT_NAME: usize = 64;

/// What a connecting client said about itself in its `initialize`. It is
/// shown to the person who decides; it is never authority: the client
/// chooses neither the principal it acts as nor the workspace it acts in.
#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    /// The whole `initialize` request, answered once the person admits it.
    pub request: serde_json::Value,
    /// `clientInfo.name`, trimmed, bounded and stripped of control
    /// characters; `MCP client` when it gave none.
    pub client_name: String,
    /// `clientInfo.version`, as bounded.
    pub client_version: Option<String>,
}

/// What the person decided about one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Consent {
    /// Serve it as agent `agent`, confined to `room`.
    Admit { room: String, agent: String },
    /// Refuse it, telling the client why.
    Refuse(String),
}

/// Why the door would not ask the person at all.
#[derive(Debug)]
pub enum HelloError {
    /// The client sent something before, or instead of, `initialize`: it is
    /// answered with this JSON-RPC id, then the connection closes.
    NotInitialize(serde_json::Value),
    /// The client asked whether the server speaks a newer protocol before
    /// it initializes (MCP's `server/discover`). The door does not, so this
    /// request with this id is answered "method not found", and the client
    /// goes on to `initialize` on the same connection, as the protocol has
    /// it fall back. Nothing is asked of the person and nothing runs.
    Discover(serde_json::Value),
    /// The line was unreadable, over `MAX_HELLO`, or the client left.
    Io(io::Error),
}

fn bounded_text(value: Option<&serde_json::Value>) -> Option<String> {
    let text: String = value
        .and_then(serde_json::Value::as_str)?
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_CLIENT_NAME)
        .collect();
    (!text.is_empty()).then_some(text)
}

/// The request a client may send before `initialize` to learn whether the
/// server speaks a newer protocol revision.
const DISCOVER: &str = "server/discover";

/// Parse a line a connection sends before it is admitted. Only an
/// `initialize` request opens the door; a `server/discover` probe is
/// answered as unsupported; anything else — a tool call, a notification,
/// junk — is refused before the person is asked, and nothing it asked for
/// runs.
///
/// # Errors
/// Returns [`HelloError::Discover`] for a discovery probe and
/// [`HelloError::NotInitialize`] for anything else but `initialize`.
pub fn parse_hello(line: &[u8]) -> Result<Hello, HelloError> {
    let request: serde_json::Value = serde_json::from_slice(line)
        .map_err(|_| HelloError::NotInitialize(serde_json::Value::Null))?;
    let id = request
        .get("id")
        .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
        .cloned();
    let Some(id) = id else {
        return Err(HelloError::NotInitialize(serde_json::Value::Null));
    };
    if request.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(HelloError::NotInitialize(id));
    }
    match request.get("method").and_then(serde_json::Value::as_str) {
        Some("initialize") => {}
        Some(DISCOVER) => return Err(HelloError::Discover(id)),
        _ => return Err(HelloError::NotInitialize(id)),
    }
    let info = request.pointer("/params/clientInfo");
    Ok(Hello {
        client_name: bounded_text(info.and_then(|info| info.get("name")))
            .unwrap_or_else(|| "MCP client".to_owned()),
        client_version: bounded_text(info.and_then(|info| info.get("version"))),
        request,
    })
}

/// Read exactly one line from `reader`, a byte at a time, so nothing the
/// client sent after it is consumed before the connection is admitted.
/// `budget` is what is left of [`MAX_HELLO`] for everything the client sends
/// before its `initialize`, so probes cannot keep an unadmitted connection
/// reading without end.
fn read_hello_line(reader: &mut impl io::Read, budget: &mut usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the client left before it said who it is",
                ));
            }
            Ok(_) => {
                let [byte] = byte;
                if byte == b'\n' {
                    return Ok(line);
                }
                let Some(left) = budget.checked_sub(1) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the initialize request is too long",
                    ));
                };
                *budget = left;
                line.push(byte);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// The JSON-RPC "method not found" answer to a `server/discover` probe.
fn not_found_reply(id: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32601, "message": format!("Method not found: {DISCOVER}")}
    })
}

/// A JSON-RPC error reply the door sends before it closes.
#[must_use]
pub fn error_reply(id: &serde_json::Value, message: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":message}})
}

#[cfg(unix)]
fn write_line(stream: &mut std::os::unix::net::UnixStream, value: &serde_json::Value) {
    use std::io::Write;
    let _ = stream.write_all(format!("{value}\n").as_bytes());
    let _ = stream.flush();
}

/// Whether the client on `stream` is still connected: a waiting prompt for
/// a client that left is withdrawn rather than answered for nobody.
#[cfg(unix)]
#[must_use]
pub fn client_connected(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut byte = 0_u8;
    // SAFETY: a one-byte peek into a live local, on a descriptor `stream`
    // owns for the whole call.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match read {
        0 => false,
        n if n > 0 => true,
        _ => {
            let error = io::Error::last_os_error();
            matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            )
        }
    }
}

/// Serve one agent door connection, whose peer the host has already
/// checked: read its `initialize`, `ask` the person (with a probe that says
/// whether the client is still there), and only once they admit it answer
/// that `initialize` and every later request as the admitted agent, over
/// `host`'s ordinary gate. Before then the door reads nothing more, so no
/// request a client queued behind its `initialize` can run unadmitted. A
/// refusal answers the `initialize` with a JSON-RPC error and closes.
///
/// # Errors
/// Returns transport errors; a refusal is `Ok`.
/// Close after a refusal without a reset: a client that already queued more
/// lines would otherwise have the error discarded with its unread input.
/// Half-close, then drain a bounded amount briefly.
#[cfg(unix)]
fn finish_refusal(stream: &mut std::os::unix::net::UnixStream) {
    use std::io::Read as _;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let mut sink = [0u8; 4096];
    let mut drained = 0usize;
    while drained < 64 * 1024 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained = drained.saturating_add(n),
        }
    }
}

/// The first line a client sends the door, read a byte at a time so nothing
/// after it is consumed, within the bound and time an `initialize` gets.
/// What it says decides how the connection is served: [`is_control_request`].
///
/// # Errors
/// The client left, sent too much, or said nothing in time.
#[cfg(unix)]
pub fn read_first_line(stream: &mut std::os::unix::net::UnixStream) -> io::Result<Vec<u8>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut budget = MAX_HELLO;
    read_hello_line(stream, &mut budget)
}

/// Whether `line` is a request of the desktop's control protocol — one JSON
/// object naming its `op` — rather than MCP's JSON-RPC. `tilo action` and
/// `tilo open` speak it from inside a terminal app.
#[must_use]
pub fn is_control_request(line: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(line).is_ok_and(|request| {
        request.get("op").is_some_and(serde_json::Value::is_string)
            && request.get("jsonrpc").is_none()
    })
}

/// Serve one accepted session-door connection.
#[cfg(unix)]
pub fn serve<H>(
    stream: std::os::unix::net::UnixStream,
    host: H,
    ask: impl FnOnce(&Hello, &dyn Fn() -> bool) -> Consent,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionHost + Send + Sync + 'static,
{
    serve_after(stream, None, host, ask)
}

/// [`serve`] a connection whose first line, `first`, was already read
/// ([`read_first_line`]).
#[cfg(unix)]
pub fn serve_after<H>(
    mut stream: std::os::unix::net::UnixStream,
    mut first: Option<Vec<u8>>,
    host: H,
    ask: impl FnOnce(&Hello, &dyn Fn() -> bool) -> Consent,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionHost + Send + Sync + 'static,
{
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut budget = MAX_HELLO.saturating_sub(first.as_ref().map_or(0, Vec::len));
    let hello = loop {
        let line = match first.take() {
            Some(line) => Ok(line),
            None => read_hello_line(&mut stream, &mut budget),
        };
        match line
            .map_err(HelloError::Io)
            .and_then(|line| parse_hello(&line))
        {
            Ok(hello) => break hello,
            Err(HelloError::Discover(id)) => {
                // A client that does not read its answers is not waited on.
                use std::io::Write as _;
                stream.write_all(format!("{}\n", not_found_reply(&id)).as_bytes())?;
            }
            Err(HelloError::NotInitialize(id)) => {
                write_line(
                    &mut stream,
                    &error_reply(
                        &id,
                        "the agent door opens with initialize; nothing else is answered before the \
                     person admits this client",
                    ),
                );
                finish_refusal(&mut stream);
                return Ok(());
            }
            Err(HelloError::Io(error)) => return Err(error),
        }
    };
    stream.set_read_timeout(None)?;
    let id = hello.request.get("id").cloned().unwrap_or_default();
    let probe = {
        let stream = stream.try_clone()?;
        move || client_connected(&stream)
    };
    let (room, agent) = match ask(&hello, &probe) {
        Consent::Admit { room, agent } => (room, agent),
        Consent::Refuse(message) => {
            write_line(&mut stream, &error_reply(&id, &message));
            finish_refusal(&mut stream);
            return Ok(());
        }
    };
    let server = std::sync::Arc::new(crate::mcp::McpServer::for_agent(host, room, &agent));
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    if let Some(reply) = runtime
        .block_on(server.handle(hello.request))
        .map_err(io::Error::other)?
    {
        crate::mcp_process::write_reply(&mut stream, &reply)?;
    }
    let input = crate::mcp_process::spawn_reader(stream.try_clone()?)?;
    let ended = crate::mcp_process::serve_lines(
        runtime,
        input,
        &mut stream,
        |request| crate::mcp_process::mcp_answer(&server, request),
        None,
    );
    let _ = stream.shutdown(std::net::Shutdown::Both);
    ended
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_whose_door_is_missing_says_why_and_how_to_go_on() {
        let nowhere = route(false, true, None, |_| true).unwrap_err();
        assert!(
            nowhere
                .to_string()
                .starts_with("the desktop's agent door isn't available:"),
            "{nowhere}"
        );
        let down = route(
            false,
            true,
            Some(PathBuf::from("/run/user/1000/tilo-session/agent.sock")),
            |_| false,
        )
        .unwrap_err();
        let text = down.to_string();
        assert!(
            text.contains("agent.sock") && text.contains("first login"),
            "{text}"
        );
        assert!(text.contains("--standalone"), "{text}");
    }

    #[test]
    fn control_requests_are_told_from_mcp_by_their_op() {
        assert!(is_control_request(
            br#"{"args":{},"id":"x","op":"actions_invoke","provider":"p"}"#
        ));
        assert!(!is_control_request(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#
        ));
        assert!(!is_control_request(br#"{"jsonrpc":"2.0","op":"x"}"#));
        assert!(!is_control_request(br#"{"op":7}"#));
        assert!(!is_control_request(b"op"));
    }

    #[test]
    fn the_door_lives_in_the_private_session_directory() {
        assert_eq!(
            agent_socket_path(Some("/run/user/1000".into())),
            Some(PathBuf::from("/run/user/1000/tilo-session/agent.sock"))
        );
        assert_eq!(agent_socket_path(Some("run/user".into())), None);
        assert_eq!(agent_socket_path(None), None);
    }

    #[test]
    fn a_tilo_login_relays_to_its_door_and_never_falls_back_to_a_standalone_runtime() {
        let door = PathBuf::from("/run/user/1000/tilo-session/agent.sock");
        assert_eq!(
            route(false, true, Some(door.clone()), |_| true).unwrap(),
            Route::Session(door.clone())
        );
        let missing = route(false, true, Some(door.clone()), |_| false).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(missing.to_string().contains("--standalone"), "{missing}");
        assert!(route(false, true, None, |_| true).is_err());
        assert_eq!(
            route(true, true, Some(door.clone()), |_| true).unwrap(),
            Route::Standalone
        );
        assert_eq!(
            route(false, false, Some(door), |_| true).unwrap(),
            Route::Standalone
        );
    }

    #[test]
    fn only_an_initialize_request_opens_the_door() {
        let refused = |line: &str| match parse_hello(line.as_bytes()) {
            Err(HelloError::NotInitialize(id)) => id,
            other => panic!("{line}: {other:?}"),
        };
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}"#),
            serde_json::json!(4)
        );
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            serde_json::Value::Null
        );
        assert_eq!(refused("not json"), serde_json::Value::Null);
        assert!(matches!(
            parse_hello(br#"{"jsonrpc":"2.0","id":"probe","method":"server/discover","params":{}}"#),
            Err(HelloError::Discover(id)) if id == "probe"
        ));
        let hello = parse_hello(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"  Claude\u0007 Code ","version":"2.1"}}}"#,
        )
        .unwrap();
        assert_eq!(hello.client_name, "Claude Code");
        assert_eq!(hello.client_version.as_deref(), Some("2.1"));
        let anonymous =
            parse_hello(br#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{}}"#)
                .unwrap();
        assert_eq!(anonymous.client_name, "MCP client");
    }

    /// Every caller the door's host was asked to answer for.
    #[derive(Clone, Default)]
    struct Recording(std::sync::Arc<std::sync::Mutex<Vec<tilo_actions::remote::Caller>>>);

    impl tilo_actions::remote::ActionHost for Recording {
        fn query(
            &self,
            caller: &tilo_actions::remote::Caller,
            query: tilo_actions::remote::CatalogueQuery,
        ) -> tilo_actions::remote::Pending<
            Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
        > {
            self.0.lock().unwrap().push(caller.clone());
            tilo_actions::remote::Pending::ready(Ok(
                tilo_actions::remote::CataloguePage::from_rows("1".into(), Vec::new(), &query),
            ))
        }
        fn invoke(
            &self,
            caller: &tilo_actions::remote::Caller,
            _request: tilo_actions::remote::InvokeRequest,
        ) -> tilo_actions::remote::Pending<
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
        > {
            self.0.lock().unwrap().push(caller.clone());
            tilo_actions::remote::Pending::ready(Err(
                tilo_actions::remote::ActionHostError::NotPermitted,
            ))
        }
        fn observe(
            &self,
            _caller: &tilo_actions::remote::Caller,
            _id: &tilo_actions::InvocationId,
        ) -> Option<tilo_actions::remote::InvocationState> {
            None
        }
        fn cancel(
            &self,
            _caller: &tilo_actions::remote::Caller,
            _id: &tilo_actions::InvocationId,
        ) -> Result<(), tilo_actions::remote::ActionHostError> {
            Ok(())
        }
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Claude Code","version":"2"}}}"#;
    const LIST_APPS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_ls","arguments":{}}}"#;

    /// Run the door on one end of a socket pair, deciding with `consent`,
    /// with `client` written to the other end first.
    fn door(
        client: &str,
        consent: Consent,
    ) -> (
        std::os::unix::net::UnixStream,
        std::thread::JoinHandle<io::Result<()>>,
        Recording,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::io::Write;
        let (mut near, far) = std::os::unix::net::UnixStream::pair().unwrap();
        near.write_all(client.as_bytes()).unwrap();
        let host = Recording::default();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let served = {
            let host = host.clone();
            let asked = std::sync::Arc::clone(&asked);
            std::thread::spawn(move || {
                serve(far, host, |hello, _connected| {
                    assert_eq!(hello.client_name, "Claude Code");
                    asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    consent
                })
            })
        };
        (near, served, host, asked)
    }

    fn lines(stream: std::os::unix::net::UnixStream) -> Vec<serde_json::Value> {
        use std::io::BufRead;
        std::io::BufReader::new(stream)
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect()
    }

    #[test]
    fn a_refused_client_reads_a_json_rpc_error_and_nothing_it_queued_runs() {
        let (near, served, host, asked) = door(
            &format!("{INITIALIZE}\n{LIST_APPS}\n"),
            Consent::Refuse("the person said no".into()),
        );
        served.join().unwrap().unwrap();
        let replies = lines(near);
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[0]["error"]["message"], "the person said no");
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            host.0.lock().unwrap().is_empty(),
            "a refused client reached the host"
        );
    }

    #[test]
    fn a_tool_call_before_initialize_is_refused_without_asking_anyone() {
        let (near, served, host, asked) = door(
            &format!("{LIST_APPS}\n{INITIALIZE}\n"),
            Consent::Admit {
                room: "room".into(),
                agent: "claude-code".into(),
            },
        );
        served.join().unwrap().unwrap();
        let replies = lines(near);
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0]["id"], 2);
        assert!(replies[0]["error"].is_object());
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(host.0.lock().unwrap().is_empty());
    }

    /// Claude Code (and any client of MCP's 2026-07-28 revision) probes with
    /// `server/discover` before `initialize`, and falls back to `initialize`
    /// on the same connection when told the method is not found. Refusing
    /// and closing on the probe left such a client unable to connect at all.
    #[test]
    fn a_discovery_probe_is_answered_unsupported_and_the_client_goes_on_to_initialize() {
        const DISCOVER_PROBE: &str = r#"{"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#;
        let (near, served, host, asked) = door(
            &format!("{DISCOVER_PROBE}\n{INITIALIZE}\n{LIST_APPS}\n"),
            Consent::Admit {
                room: "room".into(),
                agent: "claude-code".into(),
            },
        );
        near.shutdown(std::net::Shutdown::Write).unwrap();
        let replies = lines(near);
        served.join().unwrap().unwrap();
        assert_eq!(replies.len(), 3, "{replies:?}");
        assert_eq!(replies[0]["id"], "server-discover-probe-1");
        assert_eq!(replies[0]["error"]["code"], -32601, "{}", replies[0]);
        assert!(
            replies[1]["result"]["serverInfo"].is_object(),
            "{}",
            replies[1]
        );
        assert_eq!(replies[2]["id"], 2);
        assert_eq!(replies[2]["result"]["isError"], false, "{}", replies[2]);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!host.0.lock().unwrap().is_empty());
    }

    #[test]
    fn probes_alone_never_admit_and_cannot_keep_the_door_reading() {
        const PROBE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#;
        let flood = format!("{PROBE}\n").repeat(MAX_HELLO / PROBE.len() + 1);
        let (near, served, host, asked) = door(
            &flood,
            Consent::Admit {
                room: "room".into(),
                agent: "claude-code".into(),
            },
        );
        // Read the door's answers as a client would, so only the budget
        // ends the connection. The door closes with probes still unread,
        // so the end may read as a reset.
        let reader = std::thread::spawn(move || {
            use std::io::BufRead;
            std::io::BufReader::new(near)
                .lines()
                .map_while(Result::ok)
                .count()
        });
        let ended = served.join().unwrap();
        assert_eq!(
            ended.unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "past the hello budget the door stops reading"
        );
        assert!(reader.join().unwrap() > 0, "each probe was answered");
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(host.0.lock().unwrap().is_empty());
    }

    #[test]
    fn an_admitted_client_is_served_as_its_agent_in_its_room() {
        let (near, served, host, asked) = door(
            &format!("{INITIALIZE}\n{LIST_APPS}\n"),
            Consent::Admit {
                room: "room".into(),
                agent: "claude-code".into(),
            },
        );
        near.shutdown(std::net::Shutdown::Write).unwrap();
        let replies = lines(near);
        served.join().unwrap().unwrap();
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert!(
            replies[0]["result"]["serverInfo"].is_object(),
            "{}",
            replies[0]
        );
        assert_eq!(replies[1]["id"], 2);
        assert_eq!(replies[1]["result"]["isError"], false, "{}", replies[1]);
        let callers = host.0.lock().unwrap();
        assert!(!callers.is_empty());
        for caller in callers.iter() {
            assert_eq!(
                caller.principal(),
                &tilo_actions::remote::CallerPrincipal::Agent("claude-code".into())
            );
            assert_eq!(caller.room(), Some("room"));
            assert!(caller.execution_id().is_some());
        }
    }

    #[test]
    fn the_person_is_told_when_a_waiting_client_leaves() {
        use std::io::Write;
        let (mut near, far) = std::os::unix::net::UnixStream::pair().unwrap();
        near.write_all(format!("{INITIALIZE}\n").as_bytes())
            .unwrap();
        let (asking, asked) = std::sync::mpsc::channel();
        let served = std::thread::spawn(move || {
            serve(far, Recording::default(), move |_hello, connected| {
                assert!(connected(), "the client is still there");
                asking.send(()).unwrap();
                tilo_test_env::wait_until("the waiting client to leave", || !connected());
                Consent::Refuse("gone".into())
            })
        });
        tilo_test_env::recv("the person to be asked", &asked);
        drop(near);
        served.join().unwrap().unwrap();
    }
}

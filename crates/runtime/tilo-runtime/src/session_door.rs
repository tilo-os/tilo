//! Where an MCP client inside a Tilo login reaches the desktop it runs in.
//!
//! A Tilo session's compositor serves its action broker to agents at
//! `$XDG_RUNTIME_DIR/tilo-session/agent.sock` (the session's *agent door*):
//! the person admits each connection as an agent member of a workspace, and
//! every call then passes the broker's ordinary gate as that agent. A
//! `--connect` client inside the login relays to that door. It never starts
//! a standalone runtime there: a second, empty authority beside the desktop
//! would answer every question about the desktop wrongly, and silently.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// The desktop name a Tilo login announces in `XDG_CURRENT_DESKTOP`.
pub const TILO_DESKTOP: &str = "tilo";
/// The session's private directory under `XDG_RUNTIME_DIR`, shared with the
/// compositor's other control sockets.
pub const SESSION_DIR: &str = "tilo-session";
/// The agent door's file name in [`SESSION_DIR`].
pub const AGENT_SOCKET_FILE: &str = "agent.sock";

/// Whether an `XDG_CURRENT_DESKTOP` value names a Tilo login. The value is a
/// colon list (a Tilo session exports `tilo:GNOME`), so Tilo is one entry of
/// it, never necessarily the whole value.
#[must_use]
pub fn names_tilo(current_desktop: Option<&OsStr>) -> bool {
    current_desktop
        .and_then(OsStr::to_str)
        .is_some_and(|value| {
            value
                .split(':')
                .any(|part| part.eq_ignore_ascii_case(TILO_DESKTOP))
        })
}

/// Whether this process runs inside a Tilo login.
#[must_use]
pub fn is_tilo_login() -> bool {
    names_tilo(std::env::var_os("XDG_CURRENT_DESKTOP").as_deref())
}

/// The agent door's path for `runtime_dir` (`XDG_RUNTIME_DIR`); `None` when
/// that is unset or relative, which leaves no private place for it.
#[must_use]
pub fn agent_socket_path(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    let runtime_dir = PathBuf::from(runtime_dir?);
    runtime_dir
        .is_absolute()
        .then(|| runtime_dir.join(SESSION_DIR).join(AGENT_SOCKET_FILE))
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
///
/// # Errors
/// Inside a Tilo login with no door, says so instead of starting a second
/// authority beside the desktop.
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
        Some(path) => Err(missing_door(&path.display().to_string())),
        None => Err(missing_door("$XDG_RUNTIME_DIR/tilo-session/agent.sock")),
    }
}

fn missing_door(path: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "this Tilo session serves no agent door at {path}; the desktop serves it once first \
             login is complete. A standalone runtime here would be a second, empty authority \
             beside the desktop, so none was started: pass --standalone to run one anyway"
        ),
    )
}

/// The error a client reads when the door is there but does not answer.
#[must_use]
pub fn unreachable_door(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "the Tilo session's agent door at {} does not answer ({error}); pass --standalone \
             to run a separate runtime instead",
            path.display()
        ),
    )
}

/// The longest `initialize` line the door reads before it is admitted.
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
    /// The line was unreadable, over [`MAX_HELLO`], or the client left.
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

/// Parse a connection's first line. Only an `initialize` request opens the
/// door; anything else — a tool call, a notification, junk — is refused
/// before the person is asked, and nothing it asked for runs.
///
/// # Errors
/// Returns [`HelloError::NotInitialize`] for anything but `initialize`.
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
    if request.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || request.get("method").and_then(serde_json::Value::as_str) != Some("initialize")
    {
        return Err(HelloError::NotInitialize(id));
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
fn read_hello_line(reader: &mut impl io::Read) -> io::Result<Vec<u8>> {
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
                if line.len() >= MAX_HELLO {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the initialize request is too long",
                    ));
                }
                line.push(byte);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
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
            Ok(n) => drained += n,
        }
    }
}

#[cfg(unix)]
pub fn serve<H>(
    mut stream: std::os::unix::net::UnixStream,
    host: H,
    ask: impl FnOnce(&Hello, &dyn Fn() -> bool) -> Consent,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionHost + Send + Sync + 'static,
{
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    let hello = match read_hello_line(&mut stream)
        .map_err(HelloError::Io)
        .and_then(|line| parse_hello(&line))
    {
        Ok(hello) => hello,
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
    fn a_tilo_login_is_one_entry_of_the_colon_list() {
        for value in ["tilo", "tilo:GNOME", "GNOME:tilo", "Tilo"] {
            assert!(names_tilo(Some(OsStr::new(value))), "{value}");
        }
        for value in ["", "GNOME", "tilox:GNOME", "KDE"] {
            assert!(!names_tilo(Some(OsStr::new(value))), "{value}");
        }
        assert!(!names_tilo(None));
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
        // The client's own explicit choice, and every other desktop, keep the
        // standalone runtime.
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
    const LIST_APPS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_list_apps","arguments":{}}}"#;

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
        let served = std::thread::spawn(move || {
            serve(far, Recording::default(), |_hello, connected| {
                assert!(connected(), "the client is still there");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while connected() {
                    assert!(std::time::Instant::now() < deadline, "never saw it leave");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Consent::Refuse("gone".into())
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(near);
        served.join().unwrap().unwrap();
    }
}

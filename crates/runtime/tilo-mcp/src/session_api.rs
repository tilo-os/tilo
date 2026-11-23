//! Where an MCP client inside a Tilo login reaches the desktop it runs in.
//!
//! A Tilo session's compositor serves its action broker to agents at
//! `$XDG_RUNTIME_DIR/tilo-session/agent.sock` (the session's *MCP server*):
//! the person admits each connection as an agent member of a workspace, and
//! every call then passes the broker's ordinary check as that agent. A
//! `--connect` client inside the login relays to that server. It never starts
//! a standalone runtime there: a second, empty authority beside the desktop
//! would answer every question about the desktop wrongly, and silently.

use std::ffi::OsString;
use std::io::{self, BufRead as _};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// The MCP server's path under `runtime_dir`; `None` when there is none to
/// look in, which leaves no private place for it.
#[must_use]
pub fn agent_socket_path(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    Some(tilo_paths::login::session_dir(runtime_dir)?.join(tilo_paths::login::AGENT_SOCKET_FILE))
}

/// The desktop server this process belongs to, when it runs on a Tilo desktop: the one its
/// confined environment names, else its login's own. `Ok(None)` off a Tilo desktop; `Err` on one
/// whose server is not there (yet), saying why.
///
/// # Errors
/// This is a Tilo desktop, and nothing listens where its server goes.
pub fn desktop_server() -> io::Result<Option<PathBuf>> {
    match route(
        false,
        environment_server(std::env::var_os(tilo_paths::login::AGENT_SOCKET_ENV)),
        tilo_paths::login::is_tilo_login(),
        agent_socket_path(tilo_paths::login::login_runtime_dir().map(Into::into)),
        Path::exists,
    )? {
        Route::Session(server) => Ok(Some(server)),
        Route::Standalone => Ok(None),
    }
}

/// What the agent's harness calls this session ([`SESSION_NAME`]), when it calls it anything: the
/// name a person or harness set for it in `T_SESSION_NAME`, as a session is named. `None` when
/// none is set: the agent is then shown by its product's name.
#[must_use]
pub fn session_name() -> Option<String> {
    session_name_in(&|name| std::env::var(name).ok())
}

/// [`session_name`], from the environment `var` reads.
fn session_name_in(var: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let name = var("T_SESSION_NAME")
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())?;
    Some(name.chars().take(MAX_SESSION_NAME).collect())
}

/// Bridge this process's stdio to the desktop's MCP server at `server`, whose `initialize` also
/// says the session the client is (`session_name`, when it knows one): the desktop shows the agent
/// by that name rather than by its product's name.
///
/// # Errors
/// Nothing answers at `server`, or the lines could not be carried.
#[cfg(unix)]
pub fn bridge(server: &Path, session_name: Option<String>) -> io::Result<()> {
    let stream = tilo_files::native::socket::connect(server)
        .map_err(|error| unreachable_server(server, &error))?;
    bridge_stdio(
        stream,
        Asking {
            workspace: None,
            key: agent_key()?,
            session_name,
        },
    )
}

/// Whether a desktop's MCP server answers at `server`: a desktop that ended leaves its socket
/// file behind, which nothing accepts on.
#[must_use]
pub fn answers(server: &Path) -> bool {
    tilo_files::native::socket::connect(server).is_ok()
}

/// The variable naming the file that holds the agent key this client presents: the key a person
/// started their session with for an agent (`tilo-session --agent-key FILE`), which the session's
/// server admits without asking them, as the agent the key's grant names.
pub const AGENT_KEY_ENV: &str = "TILO_AGENT_KEY";

/// The agent key in the file [`AGENT_KEY_ENV`] names, when it names one.
///
/// # Errors
/// The variable names a file that cannot be read, or holds no key.
pub fn agent_key() -> io::Result<Option<String>> {
    let Some(path) = std::env::var_os(AGENT_KEY_ENV).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "{AGENT_KEY_ENV} names {}, which could not be read: {error}",
                path.display()
            ),
        )
    })?;
    key_text(&text).map(Some).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{AGENT_KEY_ENV} names {}, which holds no agent key",
                path.display()
            ),
        )
    })
}

/// The most characters an agent key is.
const MAX_KEY: usize = 128;

/// `text` as an agent key: one word of printable ASCII, at most [`MAX_KEY`] long.
fn key_text(text: &str) -> Option<String> {
    let key = text.trim();
    (!key.is_empty() && key.len() <= MAX_KEY && key.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| key.to_owned())
}

/// Where a `--connect` client goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// The desktop's MCP server at this path.
    Session(PathBuf),
    /// A standalone runtime profile, started when none runs.
    Standalone,
}

/// The server of the confined environment this process runs in, when its
/// environment names one ([`tilo_paths::login::AGENT_SOCKET_ENV`]): an absolute
/// path, as `ush` sets it.
#[must_use]
pub fn environment_server(named: Option<OsString>) -> Option<PathBuf> {
    named.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// Choose a `--connect` client's route. `standalone` is the client's own
/// explicit choice of a profile (`--standalone`, `--profile` or
/// `TILO_RUNTIME_PROFILE`); `environment` is [`environment_server`]'s answer,
/// the server of the confined environment the client runs in, which it uses in
/// place of the session's own; `server` is [`agent_socket_path`]'s answer and
/// `exists` whether something is there.
pub fn route(
    standalone: bool,
    environment: Option<PathBuf>,
    tilo_login: bool,
    server: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> io::Result<Route> {
    if standalone {
        return Ok(Route::Standalone);
    }
    if let Some(path) = environment {
        return if exists(&path) {
            Ok(Route::Session(path))
        } else {
            Err(missing_server(&format!(
                "this environment's server, {}, is not there; the desktop opens it as the \
                 environment starts",
                path.display()
            )))
        };
    }
    if !tilo_login {
        return Ok(Route::Standalone);
    }
    match server {
        Some(path) if exists(&path) => Ok(Route::Session(path)),
        Some(path) => Err(missing_server(&format!(
            "nothing listens at {}; the desktop serves it once first login is complete",
            path.display()
        ))),
        None => Err(missing_server(
            "this process has no XDG_RUNTIME_DIR and the account has no /run/user directory to \
             look in",
        )),
    }
}

fn missing_server(reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "the desktop's MCP server isn't available: {reason}. This is a Tilo login, where a \
             standalone runtime would be a second, empty authority beside the desktop, so none \
             was started. Check that the Tilo desktop is running and its first login is \
             complete; pass --standalone to run a separate runtime anyway"
        ),
    )
}

/// The error a client reads when the server is there but does not answer.
#[must_use]
pub fn unreachable_server(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "the desktop's MCP server isn't available: {} does not answer ({error}). The Tilo \
             desktop may have ended or be starting; pass --standalone to run a separate runtime \
             instead",
            path.display()
        ),
    )
}

/// The most the server reads from a client before it is admitted: its
/// `initialize` and any discovery probe before it.
const MAX_HELLO: usize = 64 * 1024;
/// The most of a client's own name the person is shown.
const MAX_CLIENT_NAME: usize = 64;
/// The most of a session's own name the person is shown.
const MAX_SESSION_NAME: usize = 64;

/// What a connecting client said about itself in its `initialize`. It is
/// shown to the person who decides; it is never authority: the client
/// chooses neither the actor it acts as nor the workspace it acts in.
#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    /// The whole `initialize` request, answered once the person admits it.
    pub request: serde_json::Value,
    /// `clientInfo.name`, trimmed, bounded and stripped of control
    /// characters; `MCP client` when it gave none.
    pub client_name: String,
    /// `clientInfo.version`, as bounded.
    pub client_version: Option<String>,
    /// The workspace the client asks to work in ([`WORKSPACE`]), by id: a request the person
    /// decides, admitted only for a workspace they may let an agent into. `None` asks for the
    /// workspace in front of them.
    pub workspace: Option<String>,
    /// The agent key the client presents ([`KEY`]): the one the person started the session with
    /// for an agent, which admits it as that key's agent without asking them; one the session does
    /// not hold, or one taken back, is refused. Kept out of [`Self::request`].
    pub key: Option<String>,
    /// What the client calls its session ([`SESSION_NAME`]), as its own harness names it ("Office
    /// Hours"): what the person's shell shows the agent by, instead of the product's name. Never
    /// authority: it names nothing the client may act as.
    pub session_name: Option<String>,
}

/// Where a client's `initialize` names the workspace it asks to work in, by id:
/// `params.capabilities.experimental["so.tilo"].workspace`. `--connect --workspace ID` writes it.
pub const WORKSPACE: &str = "/params/capabilities/experimental/so.tilo/workspace";

/// Where a client's `initialize` presents an agent key: `params.capabilities.experimental
/// ["so.tilo"].key`. `--connect` and `tilo mcp` write the key [`AGENT_KEY_ENV`] names.
pub const KEY: &str = "/params/capabilities/experimental/so.tilo/key";

/// Where a client's `initialize` names the session it is: `params.capabilities.experimental
/// ["so.tilo"]["session-name"]`, as its own harness names it. `tilo mcp` writes it.
pub const SESSION_NAME: &str = "/params/capabilities/experimental/so.tilo/session-name";

/// The provider the session's own resources (its shell, layout, windows and notices) are
/// published under on its servers: what the catalogue lists them under and an invocation names.
pub const SESSION_PROVIDER: &str = "so.tilo.session";

/// What the person decided about one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Consent {
    /// Serve it as agent `agent`, confined to `workspace`, acting for `person`:
    /// the person who admitted it, whose own records it reads and whose
    /// authority bounds what it does (the lesser of theirs and the agent's).
    /// `name` is what the person's shell calls it: what everything it writes
    /// is shown by.
    Admit {
        workspace: String,
        agent: String,
        person: String,
        name: String,
    },
    /// Refuse it, telling the client why.
    Refuse(String),
}

/// Why the server would not ask the person at all.
#[derive(Debug)]
pub enum HelloError {
    /// The client sent something before, or instead of, `initialize`: it is
    /// answered with this JSON-RPC id, then the connection closes.
    NotInitialize(serde_json::Value),
    /// The client asked whether the server speaks a newer protocol before
    /// it initializes (MCP's `server/discover`). The server does not, so this
    /// request with this id is answered "method not found", and the client
    /// goes on to `initialize` on the same connection, as the protocol has
    /// it fall back. Nothing is asked of the person and nothing runs.
    Discover(serde_json::Value),
    /// The line was unreadable, over `MAX_HELLO`, or the client left.
    Io(io::Error),
}

fn bounded_text(value: Option<&serde_json::Value>) -> Option<String> {
    tilo_actions::context::one_line(value?.as_str()?, MAX_CLIENT_NAME)
}

/// The request a client may send before `initialize` to learn whether the
/// server speaks a newer protocol revision.
const DISCOVER: &str = "server/discover";

/// Parse a line a connection sends before it is admitted. Only an
/// `initialize` request opens the server; a `server/discover` probe is
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
    // The key is the client's secret: it is read here and kept nowhere the request goes.
    let mut request = request;
    let key = request
        .pointer_mut("/params/capabilities/experimental/so.tilo")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|ours| ours.remove("key"))
        .and_then(|key| key.as_str().and_then(key_text));
    let info = request.pointer("/params/clientInfo");
    Ok(Hello {
        key,
        client_name: bounded_text(info.and_then(|info| info.get("name")))
            .unwrap_or_else(|| "MCP client".to_owned()),
        client_version: bounded_text(info.and_then(|info| info.get("version"))),
        session_name: bounded_text(request.pointer(SESSION_NAME)),
        workspace: bounded_text(request.pointer(WORKSPACE)),
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

/// A JSON-RPC error reply the server sends before it closes.
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

/// Serve one MCP server connection, whose peer the session has already
/// checked: read its `initialize`, `ask` the person (with a probe that says
/// whether the client is still there), and only once they admit it answer
/// that `initialize` and every later request as the admitted agent, over the
/// provider's ordinary check. Before then the server reads nothing more, so no
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

/// The first line a client sends the server, read a byte at a time so nothing
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
/// object naming its `op`, or an action frame naming its `version` — rather
/// than MCP's JSON-RPC. `tilo do` and `tilo open` send frames from inside a
/// terminal app.
#[must_use]
pub fn is_control_request(line: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(line).is_ok_and(|request| {
        (request.get("op").is_some_and(serde_json::Value::is_string)
            || request
                .get("version")
                .is_some_and(serde_json::Value::is_u64))
            && request.get("jsonrpc").is_none()
    })
}

/// Serve one accepted session-server connection.
#[cfg(unix)]
pub fn serve<H>(
    stream: std::os::unix::net::UnixStream,
    provider: H,
    ask: impl FnOnce(&Hello, &dyn Fn() -> bool) -> Consent,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    serve_after(stream, None, || Ok(provider), None, ask)
}

/// [`serve`] a connection whose first line, `first`, was already read
/// ([`read_first_line`]), through the provider `provider` makes once the person
/// admitted it.
#[cfg(unix)]
pub fn serve_after<H>(
    stream: std::os::unix::net::UnixStream,
    first: Option<Vec<u8>>,
    provider: impl FnOnce() -> Result<H, String>,
    tasks: Option<std::sync::Arc<dyn crate::TaskHosts>>,
    ask: impl FnOnce(&Hello, &dyn Fn() -> bool) -> Consent,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    serve_with(
        stream,
        first,
        provider,
        Beside {
            tasks,
            devices: None,
        },
        ask,
    )
}

/// What a server's connection is served with beside its host: where its task's
/// programs run, and the person's devices as the host reaches them.
#[derive(Default)]
pub struct Beside {
    #[cfg(unix)]
    pub tasks: Option<std::sync::Arc<dyn crate::TaskHosts>>,
    pub devices: Option<std::sync::Arc<dyn crate::devices::DeviceDirectory>>,
}

/// [`serve_after`], with what `beside` holds.
///
/// # Errors
/// Returns transport errors; a refusal is `Ok`.
#[cfg(unix)]
pub fn serve_with<H>(
    mut stream: std::os::unix::net::UnixStream,
    first: Option<Vec<u8>>,
    provider: impl FnOnce() -> Result<H, String>,
    beside: Beside,
    ask: impl FnOnce(&Hello, &dyn Fn() -> bool) -> Consent,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    let Beside { tasks, devices } = beside;
    let Some(hello) = opening_hello(&mut stream, first)? else {
        return Ok(());
    };
    let id = hello.request.get("id").cloned().unwrap_or_default();
    let probe = {
        let stream = stream.try_clone()?;
        move || client_connected(&stream)
    };
    // The agent of the person's the connection acts as.
    let (workspace, agent, person, name) = match ask(&hello, &probe) {
        Consent::Admit {
            workspace,
            agent,
            person,
            name,
        } => (workspace, agent, person, name),
        Consent::Refuse(message) => {
            write_line(&mut stream, &error_reply(&id, &message));
            finish_refusal(&mut stream);
            return Ok(());
        }
    };
    // What the admitted client acts through, made once it is admitted: the
    // run the person's admission started, on a desktop.
    let provider = match provider() {
        Ok(provider) => provider,
        Err(message) => {
            write_line(&mut stream, &error_reply(&id, &message));
            finish_refusal(&mut stream);
            return Ok(());
        }
    };
    // The connection is one task: its own group, whose programs a tilolang host of the task's own
    // takes (docs/designs/app-language/agents.md), each as the agent. The group goes by the
    // session the client named, when it did: that is the name history shows and undo groups by;
    // else by the name the person's shell gave the connection.
    let group = format!("server-{}", uuid::Uuid::new_v4().simple());
    let shown = hello.session_name.clone().unwrap_or_else(|| name.clone());
    let server = crate::McpServer::for_agent(provider, workspace, &agent, &person)
        .agent_named(&shown)
        .with_agent_session(group);
    let server = server.with_task_hosts(tasks).with_devices(devices);
    serve_server(stream, hello.request, server)
}

/// Serve one accepted connection as the server `admit` builds once the client said who it is
/// (its `initialize`, `hello`): the host decides who the connection is, asking whom it asks
/// while `connected` says the client is still there, and builds the server it acts through, or
/// refuses it with the reason the client is told. Where [`serve_with`] admits a client as the
/// agent of a person on this device, a host that builds its callers itself (a workspace's
/// server, for a program in the workspace's machine) admits it through this.
///
/// # Errors
/// Returns transport errors; a refusal is `Ok`.
#[cfg(unix)]
pub fn serve_as<H>(
    mut stream: std::os::unix::net::UnixStream,
    first: Option<Vec<u8>>,
    admit: impl FnOnce(&Hello, &dyn Fn() -> bool) -> Result<crate::McpServer<H>, String>,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    let Some(hello) = opening_hello(&mut stream, first)? else {
        return Ok(());
    };
    let probe = {
        let stream = stream.try_clone()?;
        move || client_connected(&stream)
    };
    match admit(&hello, &probe) {
        Ok(server) => serve_server(stream, hello.request, server),
        Err(message) => {
            let id = hello.request.get("id").cloned().unwrap_or_default();
            write_line(&mut stream, &error_reply(&id, &message));
            finish_refusal(&mut stream);
            Ok(())
        }
    }
}

/// The client's `initialize` on `stream`, after `first` when that was read already; `None`
/// when the client sent something else first, which was answered and the connection ended.
#[cfg(unix)]
fn opening_hello(
    stream: &mut std::os::unix::net::UnixStream,
    mut first: Option<Vec<u8>>,
) -> io::Result<Option<Hello>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut budget = MAX_HELLO.saturating_sub(first.as_ref().map_or(0, Vec::len));
    let hello = loop {
        let line = match first.take() {
            Some(line) => Ok(line),
            None => read_hello_line(stream, &mut budget),
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
                    stream,
                    &error_reply(
                        &id,
                        "the MCP server opens with initialize; nothing else is answered before the \
                     person admits this client",
                    ),
                );
                finish_refusal(stream);
                return Ok(None);
            }
            Err(HelloError::Io(error)) => return Err(error),
        }
    };
    stream.set_read_timeout(None)?;
    Ok(Some(hello))
}

/// Answer `initialize`, the client's first request, as `server`, then every request after it,
/// until the client leaves.
#[cfg(unix)]
fn serve_server<H>(
    mut stream: std::os::unix::net::UnixStream,
    initialize: serde_json::Value,
    server: crate::McpServer<H>,
) -> io::Result<()>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    let server = std::sync::Arc::new(server);
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    if let Some(reply) = runtime
        .block_on(server.handle(initialize))
        .map_err(io::Error::other)?
    {
        crate::lines::write_reply(&mut stream, &reply)?;
    }
    let input = crate::lines::spawn_reader(stream.try_clone()?)?;
    let ended = crate::lines::serve_lines(
        runtime,
        input,
        &mut stream,
        |request| crate::lines::mcp_answer(&server, request),
        None,
    );
    let _ = stream.shutdown(std::net::Shutdown::Both);
    ended
}

/// The agent id a client is proposed as, by its name ([`Hello::client_name`]), folded into the
/// canonical form agent ids are written in (`Claude Code` → `claude-code`).
#[must_use]
pub fn proposed_agent_id(client: &str) -> String {
    let mut id = String::new();
    for character in client.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            id.push(character);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
        if id.len() >= 64 {
            break;
        }
    }
    let id = id.trim_matches('-');
    if id.is_empty() {
        "agent".to_owned()
    } else {
        id.to_owned()
    }
}

/// What the relay writes into a client's `initialize`, under
/// `params.capabilities.experimental["so.tilo"]`: the workspace it asks for
/// ([`WORKSPACE`]), the agent key it presents ([`KEY`]) and the session it is
/// ([`SESSION_NAME`]).
#[derive(Clone, Debug, Default)]
pub struct Asking {
    pub workspace: Option<String>,
    pub key: Option<String>,
    pub session_name: Option<String>,
}

impl Asking {
    fn is_empty(&self) -> bool {
        self.workspace.is_none() && self.key.is_none() && self.session_name.is_none()
    }

    /// `line` with what this asks written in, when it is the client's `initialize`; `None` for
    /// any other line, which goes to the server as it is.
    #[must_use]
    pub fn written_into(&self, line: &[u8]) -> Option<Vec<u8>> {
        let mut request: serde_json::Value = serde_json::from_slice(line).ok()?;
        if request.get("method").and_then(serde_json::Value::as_str) != Some("initialize") {
            return None;
        }
        let params = request
            .as_object_mut()?
            .entry("params")
            .or_insert_with(|| serde_json::json!({}));
        let mut at = params;
        for key in ["capabilities", "experimental", "so.tilo"] {
            at = at
                .as_object_mut()?
                .entry(key)
                .or_insert_with(|| serde_json::json!({}));
        }
        let ours = at.as_object_mut()?;
        if let Some(workspace) = &self.workspace {
            ours.insert("workspace".into(), serde_json::json!(workspace));
        }
        if let Some(key) = &self.key {
            ours.insert("key".into(), serde_json::json!(key));
        }
        if let Some(name) = &self.session_name {
            ours.insert("session-name".into(), serde_json::json!(name));
        }
        serde_json::to_vec(&request).ok()
    }
}

/// Relay this process's stdin to `stream` and `stream` to its stdout; the
/// client's `initialize` says what `asking` asks, when it asks anything.
#[cfg(unix)]
pub fn bridge_stdio(mut stream: UnixStream, asking: Asking) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    std::thread::Builder::new()
        .name("tilo-mcp-proxy-stdin".into())
        .spawn(move || {
            let result = if asking.is_empty() {
                relay(&mut io::stdin().lock(), &mut writer)
            } else {
                relay_asking(
                    &mut io::BufReader::new(io::stdin().lock()),
                    &mut writer,
                    &asking,
                )
            };
            let _ = writer.shutdown(std::net::Shutdown::Write);
            result
        })?;
    relay(&mut stream, &mut io::stdout().lock())?;
    Ok(())
}

/// [`relay`], with the client's `initialize` saying what `asking` asks: the
/// lines before it, and it, are read whole; everything after is copied as it
/// comes.
pub fn relay_asking(
    from: &mut impl io::BufRead,
    to: &mut impl io::Write,
    asking: &Asking,
) -> io::Result<u64> {
    /// The most the client sends before its `initialize` is through, as the
    /// server reads it.
    const MOST: u64 = 64 * 1024;
    let mut total = 0_u64;
    loop {
        let mut line = Vec::new();
        let read =
            io::Read::take(&mut *from, MOST.saturating_sub(total)).read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(total);
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        let body = line.strip_suffix(b"\n").unwrap_or(&line);
        let asked = asking.written_into(body);
        match &asked {
            Some(rewritten) => {
                to.write_all(rewritten)?;
                to.write_all(b"\n")?;
            }
            None => to.write_all(&line)?,
        }
        to.flush()?;
        if asked.is_some() || total >= MOST {
            break;
        }
    }
    Ok(total.saturating_add(relay(from, to)?))
}

/// Copy `from` to `to`, flushing after every read. A client waits for each
/// reply before it sends the next request, so a reply must not sit in a
/// buffer until more arrives: `io::copy` into a buffered stdout fills its
/// buffer and writes only when it is full or the stream ends.
pub fn relay(from: &mut impl io::Read, to: &mut impl io::Write) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0_u64;
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        to.write_all(buffer.get(..read).unwrap_or_default())?;
        to.flush()?;
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop that ended leaves its socket file behind: only a server that accepts answers.
    #[test]
    fn a_socket_an_ended_desktop_left_does_not_answer() {
        let dir = std::env::temp_dir().join(format!("tilo-mcp-answers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.sock");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(answers(&path));
        drop(listener);
        assert!(path.exists());
        assert!(!answers(&path));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_client_is_proposed_as_a_canonical_agent_id() {
        assert_eq!(proposed_agent_id("Claude Code"), "claude-code");
        assert_eq!(proposed_agent_id("  Claude   Code 2.1 "), "claude-code-2-1");
        assert_eq!(proposed_agent_id("!!!"), "agent");
        assert_eq!(proposed_agent_id("MCP client"), "mcp-client");
    }

    /// The session's name is what its harness calls it: `T_SESSION_NAME` when one is set (the way
    /// a session is named). None set names nothing: the agent is then shown by its product.
    #[test]
    fn a_sessions_name_comes_from_its_harness() {
        let named = |vars: &'static [(&'static str, &'static str)]| {
            session_name_in(&move |name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            })
        };
        assert_eq!(
            named(&[("T_SESSION_NAME", "  Office Hours ")]).as_deref(),
            Some("Office Hours"),
            "the name a harness set wins, trimmed"
        );
        assert_eq!(
            named(&[("T_SESSION_NAME", "  ")]),
            None,
            "a blank name names nothing"
        );
        assert_eq!(named(&[]), None);
    }

    tilo_test_env::scenario! {
        fn a_client_is_routed_to_its_logins_server_and_opens_it_with_initialize;

        phase a_login_whose_server_is_missing_says_why_and_how_to_go_on;
        {
            let nowhere = route(false, None, true, None, |_| true).unwrap_err();
            assert!(
                nowhere
                    .to_string()
                    .starts_with("the desktop's MCP server isn't available:"),
                "{nowhere}"
            );
            let down = route(
                false,
                None,
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

        phase control_requests_are_told_from_mcp_by_their_op;
        {
            assert!(is_control_request(br#"{"op":"state_dump"}"#));
            assert!(is_control_request(
                br#"{"version":3,"message":{"CatalogueRequest":{"correlation":1}}}"#
            ));
            assert!(!is_control_request(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#
            ));
            assert!(!is_control_request(br#"{"jsonrpc":"2.0","op":"x"}"#));
            assert!(!is_control_request(br#"{"op":7}"#));
            assert!(!is_control_request(b"op"));
        }

        phase the_server_lives_in_the_private_session_directory;
        {
            assert_eq!(
                agent_socket_path(Some("/run/user/1000".into())),
                Some(PathBuf::from("/run/user/1000/tilo-session/agent.sock"))
            );
            assert_eq!(agent_socket_path(Some("run/user".into())), None);
            assert_eq!(agent_socket_path(None), None);
        }

        phase a_tilo_login_relays_to_its_server_and_never_falls_back_to_a_standalone_runtime;
        {
            let server = PathBuf::from("/run/user/1000/tilo-session/agent.sock");
            assert_eq!(
                route(false, None, true, Some(server.clone()), |_| true).unwrap(),
                Route::Session(server.clone())
            );
            let missing = route(false, None, true, Some(server.clone()), |_| false).unwrap_err();
            assert_eq!(missing.kind(), io::ErrorKind::NotFound);
            assert!(missing.to_string().contains("--standalone"), "{missing}");
            assert!(route(false, None, true, None, |_| true).is_err());
            assert_eq!(
                route(true, None, true, Some(server.clone()), |_| true).unwrap(),
                Route::Standalone
            );
            assert_eq!(
                route(false, None, false, Some(server), |_| true).unwrap(),
                Route::Standalone
            );
        }

        phase a_client_in_an_environment_uses_its_environments_server_in_any_login;
        {
            let own = PathBuf::from("/run/tilo/agent.sock");
            let session = PathBuf::from("/run/user/1000/tilo-session/agent.sock");
            assert_eq!(
                environment_server(Some(own.clone().into())),
                Some(own.clone()),
                "the environment names its server"
            );
            assert_eq!(
                environment_server(Some("run/tilo/agent.sock".into())),
                None,
                "only an absolute path names one"
            );
            assert_eq!(environment_server(None), None);
            for login in [true, false] {
                assert_eq!(
                    route(
                        false,
                        Some(own.clone()),
                        login,
                        Some(session.clone()),
                        |_| true
                    )
                    .unwrap(),
                    Route::Session(own.clone()),
                    "an environment's own server, not the session's"
                );
            }
            let gone = route(false, Some(own.clone()), true, Some(session), |path| {
                path != own.as_path()
            })
            .unwrap_err();
            assert_eq!(gone.kind(), io::ErrorKind::NotFound);
            assert!(gone.to_string().contains("/run/tilo/agent.sock"), "{gone}");
            assert_eq!(
                route(true, Some(own), true, None, |_| true).unwrap(),
                Route::Standalone,
                "a client's own choice of a profile still wins"
            );
        }

        phase a_client_names_the_workspace_it_asks_for;
        {
            let hello = parse_hello(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{"experimental":{"so.tilo":{"workspace":"3f2c"}}},"clientInfo":{"name":"Claude Code"}}}"#,
            )
            .unwrap();
            assert_eq!(hello.workspace.as_deref(), Some("3f2c"));
            let anywhere = parse_hello(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"Claude Code"}}}"#,
            )
            .unwrap();
            assert_eq!(anywhere.workspace, None, "the workspace in front");
        }

        phase only_an_initialize_request_opens_the_server;
        {
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
    }

    /// Every caller the server's host was asked to answer for.
    #[derive(Clone, Default)]
    struct Recording(std::sync::Arc<std::sync::Mutex<Vec<tilo_actions::remote::Caller>>>);

    impl tilo_actions::remote::ActionProvider for Recording {
        fn query(
            &self,
            caller: &tilo_actions::remote::Caller,
            query: tilo_actions::remote::CatalogueQuery,
        ) -> tilo_actions::remote::Pending<
            Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
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
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
        > {
            self.0.lock().unwrap().push(caller.clone());
            tilo_actions::remote::Pending::ready(Err(
                tilo_actions::remote::ActionCallError::NotPermitted,
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
        ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>
        {
            tilo_actions::remote::Pending::ready(Ok(()))
        }
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Claude Code","version":"2"}}}"#;
    const LIST_APPS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_ls","arguments":{}}}"#;

    /// Run the server on one end of a socket pair, deciding with `consent`,
    /// with `client` written to the other end first.
    fn server(
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
        let provider = Recording::default();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let served = {
            let provider = provider.clone();
            let asked = std::sync::Arc::clone(&asked);
            std::thread::spawn(move || {
                serve(far, provider, |hello, _connected| {
                    assert_eq!(hello.client_name, "Claude Code");
                    asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    consent
                })
            })
        };
        (near, served, provider, asked)
    }

    fn lines(stream: std::os::unix::net::UnixStream) -> Vec<serde_json::Value> {
        use std::io::BufRead;
        std::io::BufReader::new(stream)
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect()
    }

    tilo_test_env::scenario! {
        fn the_server_serves_only_an_initialized_client_the_person_admitted;

        phase a_refused_client_reads_a_json_rpc_error_and_nothing_it_queued_runs;
        {
            let (near, served, provider, asked) = server(
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
                provider.0.lock().unwrap().is_empty(),
                "a refused client reached the host"
            );
        }

        phase a_tool_call_before_initialize_is_refused_without_asking_anyone;
        {
            let (near, served, provider, asked) = server(
                &format!("{LIST_APPS}\n{INITIALIZE}\n"),
                Consent::Admit {
                    workspace: "workspace".into(),
                    agent: "claude-code".into(),
                    person: "ada".into(),
                    name: "Claude Code".into(),
                },
            );
            served.join().unwrap().unwrap();
            let replies = lines(near);
            assert_eq!(replies.len(), 1, "{replies:?}");
            assert_eq!(replies[0]["id"], 2);
            assert!(replies[0]["error"].is_object());
            assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(provider.0.lock().unwrap().is_empty());
        }

        phase a_discovery_probe_is_answered_unsupported_and_the_client_goes_on_to_initialize;
        // Claude Code (and any client of MCP's 2026-07-28 revision) probes with
        // `server/discover` before `initialize`, and falls back to `initialize`
        // on the same connection when told the method is not found. Refusing
        // and closing on the probe left such a client unable to connect at all.
        {
            const DISCOVER_PROBE: &str = r#"{"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#;
            let (near, served, provider, asked) = server(
                &format!("{DISCOVER_PROBE}\n{INITIALIZE}\n{LIST_APPS}\n"),
                Consent::Admit {
                    workspace: "workspace".into(),
                    agent: "claude-code".into(),
                    person: "ada".into(),
                    name: "Claude Code".into(),
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
            assert!(!provider.0.lock().unwrap().is_empty());
        }

        phase probes_alone_never_admit_and_cannot_keep_the_server_reading;
        {
            // Few enough probes to fit in a socket unread, so writing them never
            // waits on a server that is answering them, and a client that answers
            // none of it is what the budget is for: the server reads them out,
            // answers each, and stops rather than going on to wait for the
            // fallback that never comes.
            let probes = 8;
            let flood = format!("{PROBE}\n").repeat(probes);
            let (near, served, provider, asked) = server(
                &flood,
                Consent::Admit {
                    workspace: "workspace".into(),
                    agent: "claude-code".into(),
                    person: "ada".into(),
                    name: "Claude Code".into(),
                },
            );
            let reader = std::thread::spawn(move || {
                use std::io::BufRead;
                let replies: Vec<serde_json::Value> =
                    std::io::BufReader::new(near.try_clone().unwrap())
                        .lines()
                        .map_while(Result::ok)
                        .take(probes)
                        .map_while(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
                        .collect();
                let _ = near.shutdown(std::net::Shutdown::Both);
                replies
            });
            let replies = reader.join().unwrap();
            // The server goes on to wait for the `initialize` a client falls back
            // to, which this one never sends.
            assert!(served.join().unwrap().is_err());
            assert!(!replies.is_empty(), "each probe was answered");
            for reply in &replies {
                assert_eq!(reply["error"]["code"], -32601);
            }
            assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(provider.0.lock().unwrap().is_empty());
        }

        phase an_admitted_client_is_served_as_its_agent_in_its_workspace;
        {
            let (near, served, provider, asked) = server(
                &format!("{INITIALIZE}\n{LIST_APPS}\n"),
                Consent::Admit {
                    workspace: "workspace".into(),
                    agent: "claude-code".into(),
                    person: "ada".into(),
                    name: "Claude Code".into(),
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
            let callers = provider.0.lock().unwrap();
            assert!(!callers.is_empty());
            for caller in callers.iter() {
                assert_eq!(
                    caller.acting().member_id(),
                    Some(&tilo_action_policy::MemberId::agent("claude-code"))
                );
                assert_eq!(caller.workspace(), Some("workspace"));
                assert!(caller.execution_id().is_some());
            }
        }

        phase the_person_is_told_when_a_waiting_client_leaves;
        {
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

    const PROBE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#;
}
